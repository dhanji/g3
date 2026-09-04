//! Research tool: spawns a scout agent to perform web-based research.
//!
//! The research tool is **asynchronous** - it spawns the scout agent in the background
//! and returns immediately with a research_id. The agent can continue with other work
//! while research is in progress. Results are automatically injected into the conversation
//! when ready, or the agent can check status with the `research_status` tool.

use anyhow::Result;
use std::process::Stdio;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;
use tracing::{debug, error};

use crate::ui_writer::UiWriter;
use crate::ToolCall;
use g3_config::WebDriverBrowser;

use super::executor::ToolContext;

/// Delimiter markers for scout report extraction
const REPORT_START_MARKER: &str = "---SCOUT_REPORT_START---";
const REPORT_END_MARKER: &str = "---SCOUT_REPORT_END---";

/// Error patterns that indicate context window exhaustion
const CONTEXT_ERROR_PATTERNS: &[&str] = &[
    "context length", "context_length_exceeded", "maximum context", "token limit",
    "too many tokens", "exceeds the model", "context window", "max_tokens",
];

/// Execute the research tool - spawns scout agent in background and returns immediately.
///
/// This is the **async** version of research. It:
/// 1. Registers a new research task with the PendingResearchManager
/// 2. Spawns the scout agent in a background tokio task
/// 3. Returns immediately with a placeholder message containing the research_id
/// 4. The background task updates the manager when research completes
/// 5. Results are injected into the conversation at the next natural break point
pub async fn execute_research<W: UiWriter>(
    tool_call: &ToolCall,
    ctx: &mut ToolContext<'_, W>,
) -> Result<String> {
    let query = tool_call
        .args
        .get("query")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("Missing required 'query' parameter"))?;

    // Register the research task and get an ID
    let research_id = ctx.pending_research_manager.register(query);
    
    // Clone values needed for the background task
    let query_owned = query.to_string();
    let research_id_clone = research_id.clone();
    let manager = ctx.pending_research_manager.clone();
    let browser = ctx.config.webdriver.browser.clone();
    
    // Find the g3 executable path
    let g3_path = std::env::current_exe()
        .unwrap_or_else(|_| std::path::PathBuf::from("g3"));

    // Spawn the scout agent in a background task
    tokio::spawn(async move {
        let result = run_scout_agent(&g3_path, &query_owned, browser).await;
        
        match result {
            Ok(report) => {
                debug!("Research {} completed successfully", research_id_clone);
                manager.complete(&research_id_clone, report);
            }
            Err(e) => {
                error!("Research {} failed: {}", research_id_clone, e);
                manager.fail(&research_id_clone, e.to_string());
            }
        }
    });

    // Return immediately with placeholder
    let placeholder = format!(
        "🔍 **Research initiated** (id: `{}`)

\
**Query:** {}

\
Research is running in the background. You can:
- Continue with other work - results will be automatically provided when ready
- Check status with `research_status` tool
- If you need the results before continuing, say so and yield the turn to the user

\
_Estimated time: 30-120 seconds depending on query complexity_",
        research_id,
        query
    );
    
    Ok(placeholder)
}

/// Run the scout agent and return the research report.
/// This is the blocking part that runs in a background task.
async fn run_scout_agent(
    g3_path: &std::path::Path,
    query: &str,
    browser: WebDriverBrowser,
) -> Result<String> {
    // Build the command with appropriate webdriver flags
    let mut cmd = Command::new(g3_path);
    cmd
        .arg("--agent")
        .arg("scout")
        .arg("--new-session")  // Always start fresh for research
        // Scout's webdriver research generates huge disposable HTML dumps
        // (page sources, search results) that are only useful for the one
        // tool call that produced them. An aggressive 5% thinning floor
        // (vs the default 50%) discards that content right after each tool
        // call instead of letting it accumulate toward compaction, giving
        // the research job far more headroom to actually finish. Scout is a
        // one-shot background process with no multi-turn prompt-cache reuse
        // to protect, so the cache-invalidation cost that makes a low floor
        // risky for long-lived sessions (see analysis/thinning_cache_analysis.md)
        // doesn't apply here.
        .arg("--thinning-floor=5")
        .arg("--quiet");  // Suppress log file creation

    // Propagate the webdriver browser choice
    match browser {
        WebDriverBrowser::ChromeHeadless => { cmd.arg("--chrome-headless"); }
        WebDriverBrowser::Safari => { cmd.arg("--webdriver"); }
    }

    let mut child = cmd.arg(query)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| anyhow::anyhow!("Failed to spawn scout agent: {}", e))?;

    // Capture stdout to find the report content
    let stdout = child.stdout.take()
        .ok_or_else(|| anyhow::anyhow!("Failed to capture scout agent stdout"))?;
    
    // Also capture stderr for error messages
    let stderr = child.stderr.take()
        .ok_or_else(|| anyhow::anyhow!("Failed to capture scout agent stderr"))?;
    
    let mut reader = BufReader::new(stdout).lines();
    let mut all_output = Vec::new();
    
    // Spawn a task to collect stderr
    let stderr_handle = tokio::spawn(async move {
        let mut stderr_reader = BufReader::new(stderr).lines();
        let mut stderr_output = Vec::new();
        while let Some(line) = stderr_reader.next_line().await.ok().flatten() {
            stderr_output.push(line);
        }
        stderr_output
    });

    // Collect stdout lines (no progress display in background)
    while let Some(line) = reader.next_line().await? {
        all_output.push(line);
    }
    
    // Collect stderr output
    let stderr_output = stderr_handle.await.unwrap_or_default();

    // Wait for the process to complete
    let status = child.wait().await
        .map_err(|e| anyhow::anyhow!("Failed to wait for scout agent: {}", e))?;

    if !status.success() {
        // Build detailed error message
        let exit_code = status.code().map(|c| c.to_string()).unwrap_or_else(|| "unknown".to_string());
        let full_output = all_output.join("\n");
        let stderr_text = stderr_output.join("\n");
        
        // Check for context window exhaustion
        let combined_output = format!("{} {}", full_output, stderr_text).to_lowercase();
        let is_context_error = CONTEXT_ERROR_PATTERNS.iter()
            .any(|pattern| combined_output.contains(pattern));
        
        if is_context_error {
            return Err(anyhow::anyhow!(
                "Context Window Exhausted\n\n\
                The research query required more context than the model supports.\n\n\
                **Suggestions:**\n\
                - Try a more specific, narrower query\n\
                - Break the research into smaller sub-questions\n\
                - Use a model with a larger context window\n\n\
                Exit code: {}",
                exit_code
            ));
        }
        
        // Generic error with details
        return Err(anyhow::anyhow!(
            "Scout Agent Failed\n\n\
            Exit code: {}\n\n\
            {}{}",
            exit_code,
            if !stderr_text.is_empty() { format!("**Error output:**\n{}\n\n", stderr_text.chars().take(1000).collect::<String>()) } else { String::new() },
            if !all_output.is_empty() { format!("**Last output lines:**\n{}", all_output.iter().rev().take(10).rev().cloned().collect::<Vec<_>>().join("\n")) } else { String::new() }
        ));
    }

    // Join all output and extract the report between markers
    let full_output = all_output.join("\n");
    
    extract_report(&full_output)
}

/// Execute the research_status tool - check status of pending research tasks.
pub async fn execute_research_status<W: UiWriter>(
    tool_call: &ToolCall,
    ctx: &mut ToolContext<'_, W>,
) -> Result<String> {
    let research_id = tool_call
        .args
        .get("research_id")
        .and_then(|v| v.as_str());

    if let Some(id) = research_id {
        // Check specific research task
        match ctx.pending_research_manager.get(&id.to_string()) {
            Some(task) => {
                let status_emoji = match task.status {
                    crate::pending_research::ResearchStatus::Pending => "🔄",
                    crate::pending_research::ResearchStatus::Complete => "✅",
                    crate::pending_research::ResearchStatus::Failed => "❌",
                };
                
                let mut output = format!(
                    "{} **Research Status** (id: `{}`)\n\n\
                    **Query:** {}\n\
                    **Status:** {}\n\
                    **Elapsed:** {}\n",
                    status_emoji,
                    task.id,
                    task.query,
                    task.status,
                    task.elapsed_display()
                );
                
                if task.injected {
                    output.push_str("\n_Results have already been injected into the conversation._\n");
                } else if task.status != crate::pending_research::ResearchStatus::Pending {
                    output.push_str("\n_Results will be injected at the next opportunity._\n");
                }
                
                Ok(output)
            }
            None => Ok(format!("❓ No research task found with id: `{}`", id)),
        }
    } else {
        // List all pending research tasks
        let tasks = ctx.pending_research_manager.list_pending();
        
        if tasks.is_empty() {
            return Ok("📋 No pending research tasks.".to_string());
        }
        
        let mut output = format!("📋 **Pending Research Tasks** ({} total)\n\n", tasks.len());
        
        for task in tasks {
            let status_emoji = match task.status {
                crate::pending_research::ResearchStatus::Pending => "🔄",
                crate::pending_research::ResearchStatus::Complete => "✅",
                crate::pending_research::ResearchStatus::Failed => "❌",
            };
            
            output.push_str(&format!(
                "{} `{}` - {} ({})\n   Query: {}\n\n",
                status_emoji,
                task.id,
                task.status,
                task.elapsed_display(),
                truncate_query(&task.query, 60)
            ));
        }
        
        Ok(output)
    }
}

/// Truncate a query for display
fn truncate_query(query: &str, max_len: usize) -> String {
    if query.chars().count() <= max_len {
        query.to_string()
    } else {
        let truncated: String = query.chars().take(max_len - 3).collect();
        format!("{}...", truncated)
    }
}

/// Extract the research report from scout output.
/// 
/// Looks for content between SCOUT_REPORT_START and SCOUT_REPORT_END markers.
/// Preserves ANSI escape codes in the extracted content for terminal formatting.
fn extract_report(output: &str) -> Result<String> {
    // Strip ANSI codes only for finding markers, but preserve them in the output
    let clean_output = strip_ansi_codes(output);
    
    // Find the start marker
    let start_pos = clean_output.find(REPORT_START_MARKER)
        .ok_or_else(|| anyhow::anyhow!(
            "Scout agent did not output a properly formatted report. Expected {} marker.",
            REPORT_START_MARKER
        ))?;
    
    // Find the end marker
    let end_pos = clean_output.find(REPORT_END_MARKER)
        .ok_or_else(|| anyhow::anyhow!(
            "Scout agent report is incomplete. Expected {} marker.",
            REPORT_END_MARKER
        ))?;
    
    if end_pos <= start_pos {
        return Err(anyhow::anyhow!("Invalid report format: end marker before start marker"));
    }
    
    // Now find the same markers in the original output to preserve ANSI codes
    // We need to find the marker positions accounting for ANSI codes
    let original_start = find_marker_position(output, REPORT_START_MARKER)
        .ok_or_else(|| anyhow::anyhow!("Could not find start marker in original output"))?;
    let original_end = find_marker_position(output, REPORT_END_MARKER)
        .ok_or_else(|| anyhow::anyhow!("Could not find end marker in original output"))?;
    
    // Extract content between markers from original (with ANSI codes)
    let report_start = original_start + REPORT_START_MARKER.len();
    let report_content = output[report_start..original_end].trim();
    
    if report_content.is_empty() {
        return Ok("Scout agent returned an empty report.".to_string());
    }
    
    Ok(report_content.to_string())
}

/// Find the position of a marker in text that may contain ANSI codes.
/// Searches by stripping ANSI codes character by character to find the true position.
fn find_marker_position(text: &str, marker: &str) -> Option<usize> {
    // Simple approach: search for the marker directly first
    // The markers themselves shouldn't contain ANSI codes
    if let Some(pos) = text.find(marker) {
        return Some(pos);
    }
    
    // If not found directly, the marker might be split by ANSI codes
    // This is unlikely for our use case, but handle it gracefully
    None
}

/// Strip ANSI escape codes from a string.
/// 
/// Handles common ANSI sequences like:
/// - CSI sequences: \x1b[...m (colors, styles)
/// - OSC sequences: \x1b]...\x07 (terminal titles, etc.)
pub fn strip_ansi_codes(s: &str) -> String {
    let mut result = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            // Start of escape sequence
            match chars.peek() {
                Some('[') => {
                    // CSI sequence: \x1b[...X where X is a letter
                    chars.next(); // consume '['
                    while let Some(&next) = chars.peek() {
                        chars.next();
                        if next.is_ascii_alphabetic() {
                            break;
                        }
                    }
                }
                Some(']') => {
                    // OSC sequence: \x1b]...\x07
                    chars.next(); // consume ']'
                    while let Some(&next) = chars.peek() {
                        chars.next();
                        if next == '\x07' {
                            break;
                        }
                    }
                }
                _ => {
                    // Unknown escape, skip just the ESC
                }
            }
        } else {
            result.push(c);
        }
    }
    
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_strip_ansi_codes() {
        // Simple color code
        assert_eq!(strip_ansi_codes("\x1b[31mred\x1b[0m"), "red");
        
        // RGB color code (like the bug we saw)
        assert_eq!(
            strip_ansi_codes("\x1b[38;2;216;177;114mtmp/file.md\x1b[0m"),
            "tmp/file.md"
        );
        
        // Multiple codes
        assert_eq!(
            strip_ansi_codes("\x1b[1m\x1b[32mbold green\x1b[0m normal"),
            "bold green normal"
        );
        
        // No codes
        assert_eq!(strip_ansi_codes("plain text"), "plain text");
        
        // Empty string
        assert_eq!(strip_ansi_codes(""), "");
    }

    #[test]
    fn test_extract_report_success() {
        let output = r#"Some preamble text
---SCOUT_REPORT_START---
# Research Brief

This is the report content.
---SCOUT_REPORT_END---
Some trailing text"#;
        
        let result = extract_report(output).unwrap();
        assert!(result.contains("Research Brief"));
        assert!(result.contains("This is the report content."));
        assert!(!result.contains("preamble"));
        assert!(!result.contains("trailing"));
    }

    #[test]
    fn test_extract_report_with_ansi_codes() {
        let output = "\x1b[32m---SCOUT_REPORT_START---\x1b[0m\n# Report\n\x1b[31m---SCOUT_REPORT_END---\x1b[0m";
        
        let result = extract_report(output).unwrap();
        assert!(result.contains("# Report"));
    }

    #[test]
    fn test_extract_report_missing_start() {
        let output = "No markers here";
        let result = extract_report(output);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("SCOUT_REPORT_START"));
    }

    #[test]
    fn test_extract_report_missing_end() {
        let output = "---SCOUT_REPORT_START---\nContent but no end";
        let result = extract_report(output);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("SCOUT_REPORT_END"));
    }

    #[test]
    fn test_extract_report_empty_content() {
        let output = "---SCOUT_REPORT_START---\n---SCOUT_REPORT_END---";
        let result = extract_report(output).unwrap();
        assert!(result.contains("empty report"));
    }

    #[test]
    fn test_truncate_query() {
        assert_eq!(truncate_query("short query", 50), "short query");
        
        let long_query = "This is a very long research query that should be truncated for display purposes";
        let result = truncate_query(long_query, 40);
        assert!(result.len() <= 40);
        assert!(result.ends_with("..."));
    }
}
