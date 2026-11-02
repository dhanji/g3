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

/// Threshold for truncating output (32KB). Below this the model sees everything.
///
/// Was 8KB with a 500-char head and NO tail (until 2026-11): shell is ~78% of
/// all tool calls, and the line that matters — the compiler error, the test
/// summary, the traceback — is almost always at the END, which is exactly the
/// part that was being thrown away. Every truncation then cost a follow-up
/// read_file round trip (~84k prompt tokens each) to see the tail.
const OUTPUT_TRUNCATE_THRESHOLD: usize = 32 * 1024;

/// Characters kept from the start of truncated output.
const TRUNCATED_HEAD_SIZE: usize = 6_000;

/// Characters kept from the end of truncated output. Larger than the head
/// because failures, summaries and exit banners live at the end.
const TRUNCATED_TAIL_SIZE: usize = 10_000;

/// Truncate output if it exceeds the threshold, saving full content to a file.
///
/// If the output is larger than OUTPUT_TRUNCATE_THRESHOLD bytes:
/// 1. Saves the full output to `.g3/sessions/<session_id>/tools/<tool>_<id>.txt`
/// 2. Returns the first TRUNCATED_HEAD_SIZE chars, an elision marker naming the
///    omitted line range and the file, then the last TRUNCATED_TAIL_SIZE chars.
///
/// Cuts are UTF-8 safe and snapped to line boundaries where one is nearby.
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

    let (head, tail) = head_and_tail(output, TRUNCATED_HEAD_SIZE, TRUNCATED_TAIL_SIZE);
    let total_chars = output.chars().count();
    let total_lines = output.lines().count();
    let head_lines = head.lines().count();
    let tail_lines = tail.lines().count();
    let omitted_lines = total_lines.saturating_sub(head_lines + tail_lines);

    format!(
        "{}\n\n[[ {} TRUNCATED: {} total chars / {} lines; {} middle lines omitted ]]\n\
         Full output saved to: {}\n\
         (read_file with start/end, or rg on that path, to see the middle)\n\n{}",
        head.trim_end(),
        stream_name.to_uppercase(),
        total_chars,
        total_lines,
        omitted_lines,
        file_path.display(),
        tail.trim_start_matches('\n'),
    )
}

/// Split `s` into a head of at most `head_chars` and a tail of at most
/// `tail_chars` characters, UTF-8 safe. Each cut snaps to the nearest newline
/// inside the kept region (if one exists within the last/first 20%), so lines
/// are not sliced mid-way. Caller guarantees the two do not overlap.
fn head_and_tail(s: &str, head_chars: usize, tail_chars: usize) -> (&str, &str) {
    // Byte index of the head cut.
    let mut head_end = s.char_indices().nth(head_chars).map(|(i, _)| i).unwrap_or(s.len());
    if let Some(nl) = s[..head_end].rfind('\n') {
        if nl >= head_end * 4 / 5 {
            head_end = nl;
        }
    }
    // Byte index of the tail cut.
    let total = s.chars().count();
    let mut tail_start = if total > tail_chars {
        s.char_indices().nth(total - tail_chars).map(|(i, _)| i).unwrap_or(s.len())
    } else {
        0
    };
    let tail_len = s.len() - tail_start;
    if let Some(off) = s[tail_start..].find('\n') {
        if off <= tail_len / 5 {
            tail_start += off + 1;
        }
    }
    if tail_start < head_end {
        tail_start = head_end;
    }
    (&s[..head_end], &s[tail_start..])
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
        concurrent: bool,
    }

    impl<'a, W: UiWriter> g3_execution::OutputReceiver for ToolOutputReceiver<'a, W> {
        fn on_output_line(&self, line: &str) {
            if !self.concurrent {
                self.ui_writer.update_tool_output_line(line);
            }
        }
        fn on_heartbeat(&self, elapsed_secs: u64) {
            self.ui_writer.notify_tool_heartbeat(elapsed_secs);
        }
    }

    let receiver = ToolOutputReceiver {
        ui_writer: ctx.ui_writer,
        concurrent: ctx.concurrent,
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
        let output = "x".repeat(OUTPUT_TRUNCATE_THRESHOLD * 2);
        let result = truncate_large_output(&output, None, "shell", "stdout");
        assert_eq!(result, output);
    }

    #[test]
    fn test_truncate_large_output_format() {
        let large_output = "x".repeat(OUTPUT_TRUNCATE_THRESHOLD + 1);
        
        assert!(large_output.len() > OUTPUT_TRUNCATE_THRESHOLD);
        
        // Test UTF-8 safe truncation
        let head: String = large_output.chars().take(TRUNCATED_HEAD_SIZE).collect();
        assert_eq!(head.len(), TRUNCATED_HEAD_SIZE);
    }

    #[test]
    fn test_truncate_utf8_safe() {
        // Test with multi-byte characters
        let emoji_output = "🎉".repeat(TRUNCATED_HEAD_SIZE * 2); // Each emoji is 4 bytes
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
        
        let large_output = "y".repeat(OUTPUT_TRUNCATE_THRESHOLD + 1);
        let result = truncate_large_output(&large_output, Some("test-sess"), "shell_stdout", "stdout");
        
        // Should be truncated
        assert!(result.contains("[[ STDOUT TRUNCATED"));
        assert!(result.contains("Full output saved to:"));
        assert!(result.starts_with(&"y".repeat(500)));
        
        env::set_current_dir(old_dir).unwrap();
    }

    /// The reason this change exists: the END of a long build/test log is
    /// where the failure is, and it used to be discarded entirely.
    #[test]
    fn test_truncate_keeps_tail_with_the_error() {
        use tempfile::TempDir;
        let temp_dir = TempDir::new().unwrap();
        let old_dir = std::env::current_dir().unwrap();
        std::env::set_current_dir(temp_dir.path()).unwrap();

        let mut log = String::new();
        for i in 0..5000 {
            log.push_str(&format!("   Compiling crate_{} v0.1.0\n", i));
        }
        log.push_str("error[E0308]: mismatched types at src/lib.rs:42\n");
        log.push_str("error: could not compile `g3-core` due to 1 previous error");
        assert!(log.len() > OUTPUT_TRUNCATE_THRESHOLD);

        let result = truncate_large_output(&log, Some("tail-sess"), "shell_stdout", "stdout");
        std::env::set_current_dir(old_dir).unwrap();

        assert!(result.contains("[[ STDOUT TRUNCATED"), "not truncated: {}", &result[..200]);
        assert!(result.contains("error[E0308]: mismatched types"), "tail error lost");
        assert!(result.ends_with("due to 1 previous error"), "tail not at end");
        assert!(result.starts_with("   Compiling crate_0 v0.1.0"), "head lost");
        assert!(result.contains("middle lines omitted"));
        // Bounded: head + tail + marker, nowhere near the original.
        assert!(result.len() < TRUNCATED_HEAD_SIZE + TRUNCATED_TAIL_SIZE + 1_000,
            "result too large: {}", result.len());
    }

    #[test]
    fn test_threshold_is_inclusive_boundary() {
        let exact = "z".repeat(OUTPUT_TRUNCATE_THRESHOLD);
        assert_eq!(truncate_large_output(&exact, Some("b-sess"), "shell", "stdout"), exact);
    }

    #[test]
    fn test_head_and_tail_utf8_and_line_snapping() {
        let s = "🎉".repeat(40_000);
        let (h, t) = head_and_tail(&s, TRUNCATED_HEAD_SIZE, TRUNCATED_TAIL_SIZE);
        assert_eq!(h.chars().count(), TRUNCATED_HEAD_SIZE);
        assert_eq!(t.chars().count(), TRUNCATED_TAIL_SIZE);

        let lines: String = (0..10_000).map(|i| format!("line {:05}\n", i)).collect();
        let (h, t) = head_and_tail(&lines, 1000, 1000);
        assert!(h.ends_with(|c: char| c.is_ascii_digit()), "head cut mid-line: {:?}", &h[h.len()-12..]);
        assert!(t.starts_with("line "), "tail cut mid-line: {:?}", &t[..12]);
    }

    #[test]
    fn test_head_and_tail_never_overlap() {
        let s = "abc\n".repeat(10);
        let (h, t) = head_and_tail(&s, 30, 30);
        assert!(h.len() + t.len() <= s.len());
    }
}
