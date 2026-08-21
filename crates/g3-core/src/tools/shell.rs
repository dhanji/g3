//! Shell command execution tools.

use anyhow::Result;
use std::fs;
use tracing::debug;

use crate::paths::{generate_short_id, get_tools_output_dir};
use crate::ui_writer::UiWriter;
use crate::utils::resolve_paths_in_shell_command;
use crate::utils::shell_escape_command;
use crate::ToolCall;

use super::executor::ToolContext;

/// Threshold for truncating output (8KB)
const OUTPUT_TRUNCATE_THRESHOLD: usize = 8 * 1024;

/// Number of characters to show in truncated output head
const TRUNCATED_HEAD_SIZE: usize = 500;

/// Truncate output if it exceeds the threshold, saving full content to a file.
/// 
/// If the output is larger than OUTPUT_TRUNCATE_THRESHOLD:
/// 1. Saves the full output to `.g3/sessions/<session_id>/tools/<tool>_<id>_<stream>.txt`
/// 2. Returns the first TRUNCATED_HEAD_SIZE chars with a message pointing to the file
///
/// If session_id is None, returns the original output unchanged.
fn truncate_large_output(
    output: &str,
    session_id: Option<&str>,
    tool_name: &str,
    stream_name: &str, // "stdout" or "stderr"
) -> String {
    // If output is small enough or no session, return as-is
    if output.len() <= OUTPUT_TRUNCATE_THRESHOLD || session_id.is_none() {
        return output.to_string();
    }

    let session_id = session_id.unwrap();
    let output_id = generate_short_id();
    let tools_dir = get_tools_output_dir(session_id);
    
    // Create tools directory if needed
    if let Err(e) = fs::create_dir_all(&tools_dir) {
        debug!("Failed to create tools output dir: {}", e);
        return output.to_string();
    }

    let filename = format!("{}_{}.txt", tool_name, output_id);
    let file_path = tools_dir.join(&filename);

    // Save full output to file
    if let Err(e) = fs::write(&file_path, output) {
        debug!("Failed to save large output to file: {}", e);
        return output.to_string();
    }

    // Truncate to first TRUNCATED_HEAD_SIZE chars (UTF-8 safe)
    let head: String = output.chars().take(TRUNCATED_HEAD_SIZE).collect();
    let total_chars = output.chars().count();
    
    format!(
        "{}\n\n[[ {} TRUNCATED ({} total chars) ]]\nFull output saved to: {}\nUse read_file to see more.",
        head,
        stream_name.to_uppercase(),
        total_chars,
        file_path.display()
    )
}

/// Execute the `shell` tool.
pub async fn execute_shell<W: UiWriter>(tool_call: &ToolCall, ctx: &ToolContext<'_, W>) -> Result<String> {
    debug!("Processing shell tool call");
    
    let command = match tool_call.args.get("command").and_then(|v| v.as_str()) {
        Some(cmd) => cmd,
        None => {
            debug!("No command parameter found in args: {:?}", tool_call.args);
            return Ok("❌ Missing command argument".to_string());
        }
    };
    
    debug!("Command string: {}", command);
    // First resolve any file paths with Unicode space fallback (macOS screenshot names)
    let resolved_command = resolve_paths_in_shell_command(command);
    debug!("Resolved command: {}", resolved_command);
    let escaped_command = shell_escape_command(&resolved_command);

    let executor = g3_execution::CodeExecutor::new();

    struct ToolOutputReceiver<'a, W: UiWriter> {
        ui_writer: &'a W,
    }

    impl<'a, W: UiWriter> g3_execution::OutputReceiver for ToolOutputReceiver<'a, W> {
        fn on_output_line(&self, line: &str) {
            self.ui_writer.update_tool_output_line(line);
        }
        fn on_heartbeat(&self, elapsed_secs: u64) {
            self.ui_writer.notify_tool_heartbeat(elapsed_secs);
        }
    }

    let receiver = ToolOutputReceiver {
        ui_writer: ctx.ui_writer,
    };

    debug!(
        "ABOUT TO CALL execute_bash_streaming_in_dir: escaped_command='{}', working_dir={:?}",
        escaped_command, ctx.working_dir
    );

    match executor
        .execute_bash_streaming_in_dir(&escaped_command, &receiver, ctx.working_dir)
        .await
    {
        Ok(result) => {
            if result.success {
                if result.stdout.is_empty() {
                    Ok("⚡️ ran successfully".to_string())
                } else {
                    let stdout = result.stdout.trim();
                    let truncated = truncate_large_output(
                        stdout,
                        ctx.session_id,
                        "shell_stdout",
                        "stdout",
                    );
                    Ok(truncated)
                }
            } else {
                // Build error message with available information
                let stderr = result.stderr.trim();
                let stdout = result.stdout.trim();
                
                if !stderr.is_empty() {
                    let truncated = truncate_large_output(
                        stderr,
                        ctx.session_id,
                        "shell_stderr",
                        "stderr",
                    );
                    Ok(format!("❌ {}", truncated))
                } else if !stdout.is_empty() {
                    // Sometimes error info is in stdout
                    let truncated = truncate_large_output(
                        stdout,
                        ctx.session_id,
                        "shell_stdout",
                        "stdout",
                    );
                    Ok(format!("❌ Exit code {}: {}", result.exit_code, truncated))
                } else {
                    Ok(format!("❌ Command failed with exit code {}", result.exit_code))
                }
            }
        }
        Err(e) => Ok(format!("❌ Execution error: {}", e)),
    }
}

/// Execute the `background_process` tool.
pub async fn execute_background_process<W: UiWriter>(
    tool_call: &ToolCall,
    ctx: &ToolContext<'_, W>,
) -> Result<String> {
    debug!("Processing background_process tool call");
    
    let name = match tool_call.args.get("name").and_then(|v| v.as_str()) {
        Some(n) => n,
        None => return Ok("❌ Missing 'name' argument".to_string()),
    };

    let command = match tool_call.args.get("command").and_then(|v| v.as_str()) {
        Some(c) => c,
        None => return Ok("❌ Missing 'command' argument".to_string()),
    };

    // Use provided working_dir, or fall back to context working_dir, or current dir
    let work_dir = tool_call
        .args
        .get("working_dir")
        .and_then(|v| v.as_str())
        .map(|s| std::path::PathBuf::from(shellexpand::tilde(s).as_ref()))
        .or_else(|| ctx.working_dir.map(std::path::PathBuf::from))
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_default());

    match ctx.background_process_manager.start(name, command, &work_dir) {
        Ok(info) => Ok(format!(
            "✅ Background process '{}' started\n\n\
            **PID:** {}\n\
            **Log file:** {}\n\
            **Working dir:** {}\n\n\
            To interact with this process, use the shell tool:\n\
            - View logs: `tail -100 {}`\n\
            - Follow logs: `tail -f {}` (blocks until Ctrl+C)\n\
            - Check status: `ps -p {}`\n\
            - Stop process: `kill {}`",
            info.name,
            info.pid,
            info.log_file.display(),
            info.working_dir.display(),
            info.log_file.display(),
            info.log_file.display(),
            info.pid,
            info.pid
        )),
        Err(e) => Ok(format!("❌ Failed to start background process: {}", e)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_truncate_small_output() {
        let output = "small output";
        let result = truncate_large_output(output, Some("test-session"), "shell", "stdout");
        assert_eq!(result, output);
    }

    #[test]
    fn test_truncate_no_session() {
        let output = "x".repeat(10000);
        let result = truncate_large_output(&output, None, "shell", "stdout");
        assert_eq!(result, output);
    }

    #[test]
    fn test_truncate_large_output_format() {
        let large_output = "x".repeat(10000);
        
        assert!(large_output.len() > OUTPUT_TRUNCATE_THRESHOLD);
        
        // Test UTF-8 safe truncation
        let head: String = large_output.chars().take(TRUNCATED_HEAD_SIZE).collect();
        assert_eq!(head.len(), TRUNCATED_HEAD_SIZE);
    }

    #[test]
    fn test_truncate_utf8_safe() {
        // Test with multi-byte characters
        let emoji_output = "🎉".repeat(5000); // Each emoji is 4 bytes
        let head: String = emoji_output.chars().take(TRUNCATED_HEAD_SIZE).collect();
        
        // Should have exactly TRUNCATED_HEAD_SIZE characters (emojis)
        assert_eq!(head.chars().count(), TRUNCATED_HEAD_SIZE);
    }

    #[test]
    fn test_truncate_saves_to_file() {
        use tempfile::TempDir;
        use std::env;
        
        // Create a temp directory and set it as the workspace
        let temp_dir = TempDir::new().unwrap();
        let old_dir = env::current_dir().unwrap();
        env::set_current_dir(temp_dir.path()).unwrap();
        
        let large_output = "y".repeat(10000);
        let result = truncate_large_output(&large_output, Some("test-sess"), "shell_stdout", "stdout");
        
        // Should be truncated
        assert!(result.contains("[[ STDOUT TRUNCATED"));
        assert!(result.contains("Use read_file to see more."));
        assert!(result.starts_with(&"y".repeat(500)));
        
        env::set_current_dir(old_dir).unwrap();
    }
}
