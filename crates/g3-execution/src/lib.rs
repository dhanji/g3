use anyhow::Result;
use regex::Regex;
use std::io::Write;
use std::process::Command;
use tempfile::NamedTempFile;
use tracing::{debug, error};

/// Expand tilde (~) in a path to the user's home directory
fn expand_tilde(path: &str) -> String {
    if path.starts_with("~") {
        if let Some(home) = std::env::var_os("HOME") {
            let home_str = home.to_string_lossy();
            return path.replacen("~", &home_str, 1);
        }
    }
    path.to_string()
}

pub struct CodeExecutor {
    /// How often `on_heartbeat` fires during a silent command.
    ///
    /// A field rather than a bare constant purely so tests can drive it at
    /// millisecond speed. Nothing in production sets it — `new()` uses
    /// HEARTBEAT_INTERVAL, and a test asserting the real 30s cadence would
    /// take a minute to prove one beat.
    heartbeat_interval: std::time::Duration,
}

/// How often a running command reports that it is still running.
///
/// Matches Anthropic's SSE keep-alive cadence (measured 30.0s) on purpose: a
/// consumer watching the events file for liveness then sees the SAME rhythm
/// whether g3 is thinking or running a tool, and needs one threshold rather
/// than two. butler.app's `G3_MAX_IDLE_SEC` is sized against this and nothing
/// else — notably NOT against how long a tool may run, which is what coupled
/// it to the per-tool timeout before this existed.
const HEARTBEAT_INTERVAL: std::time::Duration = std::time::Duration::from_secs(30);

#[derive(Debug, Clone)]
pub struct ExecutionResult {
    pub stdout: String,
    pub stderr: String,
    pub exit_code: i32,
    pub success: bool,
}

impl CodeExecutor {
    pub fn new() -> Self {
        Self {
            heartbeat_interval: HEARTBEAT_INTERVAL,
        }
    }

    /// A executor whose heartbeat fires at `interval`. Tests only — see the
    /// field's comment for why this exists rather than a global.
    pub fn with_heartbeat_interval(interval: std::time::Duration) -> Self {
        Self {
            heartbeat_interval: interval,
        }
    }

    /// Extract code blocks from LLM response and execute them
    pub async fn execute_from_response(&self, response: &str) -> Result<String> {
        self.execute_from_response_with_options(response, true)
            .await
    }

    /// Extract code blocks from LLM response and execute them with UI options
    pub async fn execute_from_response_with_options(
        &self,
        response: &str,
        show_code: bool,
    ) -> Result<String> {
        debug!(
            "CodeExecutor received response ({} chars): {}",
            response.len(),
            response
        );
        let code_blocks = self.extract_code_blocks(response)?;

        if code_blocks.is_empty() {
            if show_code {
                return Ok(format!(
                    "⚠️  No executable code blocks found in response.\n\n{}",
                    response
                ));
            } else {
                return Ok("⚠️  No executable code found.".to_string());
            }
        }

        let mut results = Vec::new();

        // Only show the original LLM response if show_code is true
        if show_code {
            results.push(response.to_string());
            results.push("\n🚀 Executing code...\n".to_string());
        }

        for (language, code) in code_blocks {
            debug!("Executing {} code", language);

            if show_code {
                results.push(format!("📋 Running {} code:", language));
            }

            match self.execute_code(&language, &code).await {
                Ok(result) => {
                    if result.success {
                        if show_code {
                            results.push("✅ Success".to_string());
                        }
                        // Always show stdout if there is any, regardless of show_code
                        if !result.stdout.is_empty() {
                            results.push(result.stdout.trim().to_string());
                        }
                    } else {
                        results.push("❌ Failed".to_string());
                        if !result.stderr.is_empty() {
                            results.push(format!("Error: {}", result.stderr.trim()));
                        }
                    }
                }
                Err(e) => {
                    error!("Failed to execute {} code: {}", language, e);
                    results.push(format!("❌ Execution failed: {}", e));
                }
            }
        }

        // If no results were added (e.g., successful execution with no output),
        // return a simple success message when show_code is false
        if results.is_empty() && !show_code {
            Ok("✅ Done".to_string())
        } else {
            Ok(results.join("\n"))
        }
    }

    /// Extract code blocks from markdown-formatted text
    fn extract_code_blocks(&self, text: &str) -> Result<Vec<(String, String)>> {
        let mut blocks = Vec::new();

        debug!("Extracting code blocks from text: {}", text);

        // Pattern 1: Standard markdown format ```language\ncode```
        let markdown_re = Regex::new(r"(?s)```(\w+)?\n(.*?)```")?;
        for cap in markdown_re.captures_iter(text) {
            let language = cap
                .get(1)
                .map(|m| m.as_str().to_lowercase())
                .unwrap_or_else(|| "bash".to_string()); // Default to bash
            let code = cap.get(2).map(|m| m.as_str()).unwrap_or("").trim();

            debug!(
                "Found markdown code block - language: '{}', code: '{}'",
                language, code
            );

            if !code.is_empty() {
                blocks.push((language, code.to_string()));
            }
        }

        // Pattern 2: Bracket format [Language]code[/Language]
        let bracket_re = Regex::new(r"(?s)\[(\w+)\]\s*(.*?)\s*\[/(\w+)\]")?;
        for cap in bracket_re.captures_iter(text) {
            let open_lang = cap.get(1).map(|m| m.as_str()).unwrap_or("");
            let close_lang = cap.get(3).map(|m| m.as_str()).unwrap_or("");

            // Only match if opening and closing tags are the same (case insensitive)
            if open_lang.to_lowercase() == close_lang.to_lowercase() {
                let language = open_lang.to_lowercase();
                let code = cap.get(2).map(|m| m.as_str()).unwrap_or("").trim();

                debug!(
                    "Found bracket code block - language: '{}', code: '{}'",
                    language, code
                );

                if !code.is_empty() {
                    blocks.push((language, code.to_string()));
                }
            }
        }

        debug!("Total code blocks found: {}", blocks.len());
        Ok(blocks)
    }

    /// Execute code in the specified language
    pub async fn execute_code(&self, language: &str, code: &str) -> Result<ExecutionResult> {
        match language.to_lowercase().as_str() {
            "python" | "py" => self.execute_python(code).await,
            "bash" | "shell" | "sh" => self.execute_bash(code).await,
            "javascript" | "js" => self.execute_javascript(code).await,
            _ => {
                // Try to execute as bash by default
                debug!("Unknown language '{}', trying as bash", language);
                self.execute_bash(code).await
            }
        }
    }

    /// Execute Python code
    async fn execute_python(&self, code: &str) -> Result<ExecutionResult> {
        let mut temp_file = NamedTempFile::new()?;
        temp_file.write_all(code.as_bytes())?;
        let temp_path = temp_file.path();

        let output = Command::new("python3").arg(temp_path).output()?;

        Ok(ExecutionResult {
            stdout: String::from_utf8_lossy(&output.stdout).to_string(),
            stderr: String::from_utf8_lossy(&output.stderr).to_string(),
            exit_code: output.status.code().unwrap_or(-1),
            success: output.status.success(),
        })
    }

    /// Execute Bash code
    async fn execute_bash(&self, code: &str) -> Result<ExecutionResult> {
        // Check if this is a detached/daemon command that should run independently
        let is_detached = code.trim_start().starts_with("setsid ")
            || code.trim_start().starts_with("nohup ")
            || code.contains(" disown")
            || (code.contains(" &") && (code.contains("nohup") || code.contains("setsid")));

        if is_detached {
            // For detached commands, just spawn and return immediately
            use std::process::Stdio;
            Command::new("bash")
                .arg("-c")
                .arg(code)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()?;

            return Ok(ExecutionResult {
                stdout: "✅ Command launched in background (detached process)".to_string(),
                stderr: String::new(),
                exit_code: 0,
                success: true,
            });
        }

        let output = Command::new("bash").arg("-c").arg(code).output()?;

        Ok(ExecutionResult {
            stdout: String::from_utf8_lossy(&output.stdout).to_string(),
            stderr: String::from_utf8_lossy(&output.stderr).to_string(),
            exit_code: output.status.code().unwrap_or(-1),
            success: output.status.success(),
        })
    }

    /// Execute JavaScript code (requires Node.js)
    async fn execute_javascript(&self, code: &str) -> Result<ExecutionResult> {
        let mut temp_file = NamedTempFile::new()?;
        temp_file.write_all(code.as_bytes())?;
        let temp_path = temp_file.path();

        let output = Command::new("node").arg(temp_path).output()?;

        Ok(ExecutionResult {
            stdout: String::from_utf8_lossy(&output.stdout).to_string(),
            stderr: String::from_utf8_lossy(&output.stderr).to_string(),
            exit_code: output.status.code().unwrap_or(-1),
            success: output.status.success(),
        })
    }
}

impl Default for CodeExecutor {
    fn default() -> Self {
        Self::new()
    }
}

/// Trait for receiving streaming output from command execution
pub trait OutputReceiver: Send + Sync {
    /// Called when a new line of output is available
    fn on_output_line(&self, line: &str);

    /// Called periodically while a command runs but produces NO output.
    ///
    /// # Why this exists
    ///
    /// butler.app decides whether to kill a wedged g3 by watching its NDJSON
    /// events file grow. Anthropic's SSE keep-alive (`upstream_ping`) covers a
    /// long THINK, but it stops dead during tool execution — there is no
    /// request in flight while a subprocess runs. Measured: 434 pings between
    /// tool calls vs 50 during one, with 43% of wall time inside tools.
    ///
    /// So a silent `cargo test` looked byte-identical to a hung process, and
    /// the only way to avoid killing it was to set the idle budget above the
    /// longest tool that could ever run — coupling an unrelated constant to
    /// g3's 8-minute (20 for `research`) tool cap.
    ///
    /// ⚠️ THE BEAT IS ONLY HONEST BECAUSE OF WHERE IT IS CALLED FROM: the same
    /// `select!` loop that reads the child's stdout. It therefore cannot fire
    /// unless that task is being polled, which means a wedged runtime, a
    /// SIGSTOPed process or a starved future all correctly produce silence. A
    /// detached timer task would beat happily through all three and would be
    /// worse than nothing — evidence the server manufactures about itself.
    ///
    /// It DOES keep beating while the child subprocess itself hangs, and that
    /// is correct rather than a gap: `sleep 600` is indistinguishable from a
    /// slow build from the outside, and g3's own per-tool timeout owns that
    /// case. This answers exactly one question — "is g3 still a live,
    /// scheduling process?" — which is what `pid_alive()` cannot answer.
    ///
    /// Default is a no-op so non-streaming receivers need no changes.
    fn on_heartbeat(&self, _elapsed_secs: u64) {}
}

impl CodeExecutor {
    /// Execute bash command with streaming output
    pub async fn execute_bash_streaming<R: OutputReceiver>(
        &self,
        code: &str,
        receiver: &R,
    ) -> Result<ExecutionResult> {
        self.execute_bash_streaming_in_dir(code, receiver, None)
            .await
    }

    /// Execute bash command with streaming output in a specific directory
    pub async fn execute_bash_streaming_in_dir<R: OutputReceiver>(
        &self,
        code: &str,
        receiver: &R,
        working_dir: Option<&str>,
    ) -> Result<ExecutionResult> {
        use std::process::Stdio;
        use tokio::io::{AsyncBufReadExt, BufReader};
        use tokio::process::Command as TokioCommand;

        // CRITICAL DEBUG: Print to stderr so it's always visible
        debug!("========== execute_bash_streaming_in_dir START ==========");
        debug!("Code to execute: {}", code);
        debug!("Working directory parameter: {:?}", working_dir);
        debug!(
            "FULL DIAGNOSTIC: code='{}', working_dir={:?}",
            code, working_dir
        );

        if let Some(dir) = working_dir {
            debug!(
                "Working dir exists check: {}",
                std::path::Path::new(dir).exists()
            );
            debug!(
                "Working dir is_dir check: {}",
                std::path::Path::new(dir).is_dir()
            );
        }
        debug!(
            "Current process working directory: {:?}",
            std::env::current_dir()
        );

        // Check if this is a detached/daemon command that should run independently
        // Look for patterns like: setsid, nohup with &, or explicit backgrounding with disown
        let is_detached = code.trim_start().starts_with("setsid ")
            || code.trim_start().starts_with("nohup ")
            || code.contains(" disown")
            || (code.contains(" &") && (code.contains("nohup") || code.contains("setsid")));

        if is_detached {
            // For detached commands, just spawn and return immediately
            let mut cmd = TokioCommand::new("bash");
            cmd.arg("-c").arg(code);

            // Set working directory if provided
            if let Some(dir) = working_dir {
                let expanded_dir = expand_tilde(dir);
                cmd.current_dir(&expanded_dir);
            }

            cmd.spawn()?;

            // Don't wait for the process - it's meant to run independently
            return Ok(ExecutionResult {
                stdout: "✅ Command launched in background (detached process)".to_string(),
                stderr: String::new(),
                exit_code: 0,
                success: true,
            });
        }

        let mut cmd = TokioCommand::new("bash");
        cmd.arg("-c")
            .arg(code)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        // Set working directory if provided
        if let Some(dir) = working_dir {
            debug!("Setting current_dir on command to: {}", dir);
            let expanded_dir = expand_tilde(dir);
            debug!("Expanded working dir: {}", expanded_dir);
            debug!(
                "Expanded dir exists: {}",
                std::path::Path::new(&expanded_dir).exists()
            );
            debug!(
                "Expanded dir is_dir: {}",
                std::path::Path::new(&expanded_dir).is_dir()
            );
            cmd.current_dir(&expanded_dir);
        }

        debug!("About to spawn command...");
        let spawn_result = cmd.spawn();
        debug!("Spawn result: {:?}", spawn_result.is_ok());
        let mut child = match spawn_result {
            Ok(c) => c,
            Err(e) => {
                debug!("SPAWN ERROR: {:?}", e);
                return Err(e.into());
            }
        };
        debug!("Command spawned successfully");

        let stdout = child.stdout.take().unwrap();
        let stderr = child.stderr.take().unwrap();

        let stdout_reader = BufReader::new(stdout);
        let stderr_reader = BufReader::new(stderr);

        let mut stdout_lines = stdout_reader.lines();
        let mut stderr_lines = stderr_reader.lines();

        let mut stdout_output = Vec::new();
        let mut stderr_output = Vec::new();

        // Heartbeat while the command runs. See OutputReceiver::on_heartbeat for
        // why this lives INSIDE the same select! that reads stdout, and why a
        // detached timer task would be actively harmful.
        //
        // `interval` fires immediately on first tick, so that one is consumed up
        // front: a beat at t=0 says nothing (we already know the tool started)
        // and would put a record in the events file for every trivial command.
        let exec_start = std::time::Instant::now();
        let mut beat = tokio::time::interval(self.heartbeat_interval);
        beat.tick().await; // discard the immediate first tick

        // Read output lines as they come
        loop {
            tokio::select! {
                _ = beat.tick() => {
                    // Only reached when neither reader had a line ready, i.e.
                    // exactly the silent stretch this exists to cover.
                    receiver.on_heartbeat(exec_start.elapsed().as_secs());
                }
                line = stdout_lines.next_line() => {
                    match line {
                        Ok(Some(line)) => {
                            receiver.on_output_line(&line);
                            stdout_output.push(line);
                        }
                        Ok(None) => break, // EOF
                        Err(e) => {
                            error!("Error reading stdout: {}", e);
                            break;
                        }
                    }
                }
                line = stderr_lines.next_line() => {
                    match line {
                        Ok(Some(line)) => {
                            receiver.on_output_line(&line.to_string());
                            stderr_output.push(line);
                        }
                        Ok(None) => {}, // stderr EOF, continue
                        Err(e) => {
                            error!("Error reading stderr: {}", e);
                        }
                    }
                }
                else => break
            }
        }

        let status = child.wait().await?;

        let result = ExecutionResult {
            stdout: stdout_output.join("\n"),
            stderr: stderr_output.join("\n"),
            exit_code: status.code().unwrap_or(-1),
            success: status.success(),
        };

        debug!("========== execute_bash_streaming_in_dir END ==========");
        debug!("Exit code: {}", result.exit_code);
        debug!("Success: {}", result.success);
        debug!("Stdout length: {}", result.stdout.len());
        debug!("Stderr length: {}", result.stderr.len());
        if !result.stderr.is_empty() {
            debug!("Stderr content: {}", result.stderr);
        }

        Ok(result)
    }
}

/// Check if rustup component llvm-tools-preview is installed
pub fn is_llvm_tools_installed() -> Result<bool> {
    let output = Command::new("rustup")
        .args(&["component", "list", "--installed"])
        .output()?;

    let installed = String::from_utf8_lossy(&output.stdout)
        .lines()
        .any(|line| line.trim() == "llvm-tools-preview" || line.starts_with("llvm-tools"));

    Ok(installed)
}

/// Check if cargo-llvm-cov is installed
pub fn is_cargo_llvm_cov_installed() -> Result<bool> {
    let output = Command::new("cargo").args(&["--list"]).output()?;

    let installed = String::from_utf8_lossy(&output.stdout)
        .lines()
        .any(|line| line.trim().starts_with("llvm-cov"));

    Ok(installed)
}

/// Install llvm-tools-preview via rustup
pub fn install_llvm_tools() -> Result<()> {
    debug!("Installing llvm-tools-preview...");
    let output = Command::new("rustup")
        .args(&["component", "add", "llvm-tools-preview"])
        .output()?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("Failed to install llvm-tools-preview: {}", stderr);
    }

    debug!("✅ llvm-tools-preview installed successfully");
    Ok(())
}

/// Install cargo-llvm-cov via cargo install
pub fn install_cargo_llvm_cov() -> Result<()> {
    debug!("Installing cargo-llvm-cov... (this may take a few minutes)");
    let output = Command::new("cargo")
        .args(&["install", "cargo-llvm-cov"])
        .output()?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("Failed to install cargo-llvm-cov: {}", stderr);
    }

    debug!("✅ cargo-llvm-cov installed successfully");
    Ok(())
}

/// Ensure both llvm-tools-preview and cargo-llvm-cov are installed
/// Returns Ok(true) if tools were already installed, Ok(false) if they were installed by this function
pub fn ensure_coverage_tools_installed() -> Result<bool> {
    let mut already_installed = true;

    // Check and install llvm-tools-preview
    if !is_llvm_tools_installed()? {
        debug!("llvm-tools-preview not found, installing...");
        install_llvm_tools()?;
        already_installed = false;
    } else {
        debug!("✅ llvm-tools-preview is already installed");
    }

    // Check and install cargo-llvm-cov
    if !is_cargo_llvm_cov_installed()? {
        debug!("cargo-llvm-cov not found, installing...");
        install_cargo_llvm_cov()?;
        already_installed = false;
    } else {
        debug!("✅ cargo-llvm-cov is already installed");
    }

    Ok(already_installed)
}
