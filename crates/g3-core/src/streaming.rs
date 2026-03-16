//! Streaming completion logic for the Agent.
//!
//! State management and helpers for streaming LLM responses: tool call
//! detection, deduplication, and auto-continue logic.

use crate::context_window::ContextWindow;
use crate::streaming_parser::StreamingToolParser;
use crate::ToolCall;
use g3_providers::{CompletionRequest, MessageRole};
use serde_json::{Map, Value};
use std::time::{Duration, Instant};
use tracing::{debug, error};

pub const MAX_ITERATIONS: usize = 400;

/// State tracked across streaming iterations
pub struct StreamingState {
    pub full_response: String,
    pub first_token_time: Option<Duration>,
    pub stream_start: Instant,
    pub iteration_count: usize,
    pub response_started: bool,
    pub any_tool_executed: bool,
    pub auto_summary_attempts: usize,
    pub assistant_message_added: bool,
    pub turn_accumulated_usage: Option<g3_providers::Usage>,
}

impl StreamingState {
    pub fn new() -> Self {
        Self {
            full_response: String::new(),
            first_token_time: None,
            stream_start: Instant::now(),
            iteration_count: 0,
            response_started: false,
            any_tool_executed: false,
            auto_summary_attempts: 0,
            assistant_message_added: false,
            turn_accumulated_usage: None,
        }
    }

    pub fn record_first_token(&mut self) {
        if self.first_token_time.is_none() {
            self.first_token_time = Some(self.stream_start.elapsed());
        }
    }

    pub fn get_ttft(&self) -> Duration {
        self.first_token_time
            .unwrap_or_else(|| self.stream_start.elapsed())
    }
}

pub enum ToolOutputFormat {
    SelfHandled,
    Compact(String),
    Regular,
}

/// Determine how to format a tool result for display.
pub fn format_tool_result_summary(
    tool_name: &str,
    tool_result: &str,
    tool_success: bool,
) -> ToolOutputFormat {
    let is_self_handled = is_self_handled_tool(tool_name);
    let is_compact_tool = matches!(
        tool_name,
        "read_file"
            | "write_file"
            | "str_replace"
            | "remember"
            | "screenshot"
            | "coverage"
            | "rehydrate"
            | "code_search"
            | "plan_approve"
            | "load_toolset"
    );

    if is_self_handled {
        ToolOutputFormat::SelfHandled
    } else if is_compact_tool {
        if !tool_success {
            ToolOutputFormat::Compact(truncate_for_display(tool_result, 60))
        } else {
            let summary = format_compact_tool_summary(tool_name, tool_result);
            ToolOutputFormat::Compact(summary)
        }
    } else {
        ToolOutputFormat::Regular
    }
}

pub fn is_compact_tool(tool_name: &str) -> bool {
    matches!(
        tool_name,
        "read_file"
            | "write_file"
            | "str_replace"
            | "remember"
            | "screenshot"
            | "coverage"
            | "rehydrate"
            | "code_search"
            | "plan_approve"
            | "load_toolset"
    )
}

pub fn is_self_handled_tool(tool_name: &str) -> bool {
    matches!(
        tool_name,
        "todo_read" | "todo_write" | "plan_read" | "plan_write" | "write_envelope"
    )
}

/// Format a compact summary for a successful compact tool
fn format_compact_tool_summary(tool_name: &str, tool_result: &str) -> String {
    let output_lines: Vec<&str> = tool_result.lines().collect();
    let output_len = output_lines.len();

    match tool_name {
        "read_file" => format_read_file_summary(output_len, tool_result.len()),
        "write_file" => format_write_file_result(tool_result),
        "str_replace" => {
            let (ins, del) = parse_diff_stats(tool_result);
            format_str_replace_summary(ins, del)
        }
        "remember" => format_remember_summary(tool_result),
        "screenshot" => format_screenshot_summary(tool_result),
        "coverage" => format_coverage_summary(tool_result),
        "rehydrate" => format_rehydrate_summary(tool_result),
        "code_search" => format_code_search_summary(tool_result),
        "plan_approve" => format_plan_approve_summary(tool_result),
        "load_toolset" => format_load_toolset_summary(tool_result),
        _ => "✅ completed".to_string(),
    }
}

/// Parse diff stats from str_replace result
fn parse_diff_stats(result: &str) -> (i32, i32) {
    // Format: "✅ +N insertions | -M deletions"
    let mut insertions = 0i32;
    let mut deletions = 0i32;

    // Look for "+N insertions" pattern
    if let Some(pos) = result.find("+") {
        let after_plus = &result[pos + 1..];
        insertions = after_plus
            .split_whitespace()
            .next()
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);
    }
    // Look for "-M deletions" pattern
    if let Some(pos) = result.find("-") {
        let after_minus = &result[pos + 1..];
        deletions = after_minus
            .split_whitespace()
            .next()
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);
    }
    (insertions, deletions)
}

impl Default for StreamingState {
    fn default() -> Self {
        Self::new()
    }
}

/// State tracked within a single streaming iteration
pub struct IterationState {
    pub parser: StreamingToolParser,
    pub current_response: String,
    pub tool_executed: bool,
    pub chunks_received: usize,
    pub raw_chunks: Vec<String>,
    pub accumulated_usage: Option<g3_providers::Usage>,
    pub stream_stop_reason: Option<String>,
    /// Last tool call executed in this iteration (for cross-chunk dedup within same response)
    pub last_executed_tool: Option<ToolCall>,
}

impl IterationState {
    pub fn new() -> Self {
        Self {
            parser: StreamingToolParser::new(),
            current_response: String::new(),
            tool_executed: false,
            chunks_received: 0,
            raw_chunks: Vec::new(),
            accumulated_usage: None,
            stream_stop_reason: None,
            last_executed_tool: None,
        }
    }

    /// Store a raw chunk for debugging (limited to first 20 + last few)
    pub fn record_chunk(&mut self, chunk: &g3_providers::CompletionChunk) {
        if self.chunks_received < 20 || chunk.finished {
            self.raw_chunks.push(format!(
                "Chunk #{}: content={:?}, finished={}, tool_calls={:?}",
                self.chunks_received + 1,
                chunk.content,
                chunk.finished,
                chunk.tool_calls
            ));
        } else if self.raw_chunks.len() == 20 {
            self.raw_chunks
                .push("... (chunks 21+ omitted for brevity) ...".to_string());
        }
        self.chunks_received += 1;
    }
}

impl Default for IterationState {
    fn default() -> Self {
        Self::new()
    }
}

/// Clean LLM-specific tokens from content
pub fn clean_llm_tokens(content: &str) -> String {
    content
        .replace("<|im_end|>", "")
        .replace("</s>", "")
        .replace("[/INST]", "")
        .replace("<</SYS>>", "")
}

/// Format a duration for display
pub fn format_duration(duration: Duration) -> String {
    let total_ms = duration.as_millis();

    if total_ms < 1000 {
        format!("{}ms", total_ms)
    } else if total_ms < 60_000 {
        format!("{:.1}s", duration.as_secs_f64())
    } else {
        let minutes = total_ms / 60_000;
        let remaining_seconds = (total_ms % 60_000) as f64 / 1000.0;
        format!("{}m {:.1}s", minutes, remaining_seconds)
    }
}

/// Format the timing footer with optional token usage info
pub fn format_timing_footer(
    elapsed: Duration,
    ttft: Duration,
    turn_tokens: Option<u32>,
    context_percentage: f32,
) -> String {
    let timing = format!(
        "⏱️ {} | 💭 {}",
        format_duration(elapsed),
        format_duration(ttft)
    );

    // Add token usage info if available (dimmed)
    if let Some(tokens) = turn_tokens {
        format!("{}  {} ◉ | {:.0}%", timing, tokens, context_percentage)
    } else {
        format!("{}  {:.0}%", timing, context_percentage)
    }
}

/// Log detailed error information when stream produces no content
pub fn log_stream_error(
    iteration_count: usize,
    provider_name: &str,
    provider_model: &str,
    chunks_received: usize,
    parser: &StreamingToolParser,
    request: &CompletionRequest,
    context_window: &ContextWindow,
    session_id: Option<&str>,
    raw_chunks: &[String],
) {
    error!("=== STREAM ERROR: No content or tool calls received ===");
    error!("Iteration: {}/{}", iteration_count, MAX_ITERATIONS);
    error!("Provider: {} (model: {})", provider_name, provider_model);
    error!("Chunks received: {}", chunks_received);

    error!("Parser state:");
    error!("  - Text buffer length: {}", parser.text_buffer_len());
    error!("  - Text buffer content: {:?}", parser.get_text_content());
    error!(
        "  - Has incomplete tool call: {}",
        parser.has_incomplete_tool_call()
    );
    error!("  - Message stopped: {}", parser.is_message_stopped());
    error!("  - In JSON tool call: {}", parser.is_in_json_tool_call());
    error!(
        "  - JSON tool start: {:?}",
        parser.json_tool_start_position()
    );

    error!("Request details:");
    error!("  - Messages count: {}", request.messages.len());
    error!("  - Has tools: {}", request.tools.is_some());
    error!("  - Max tokens: {:?}", request.max_tokens);
    error!("  - Temperature: {:?}", request.temperature);
    error!("  - Stream: {}", request.stream);

    error!("Raw chunks received ({} total):", chunks_received);
    for (i, chunk_str) in raw_chunks.iter().take(25).enumerate() {
        error!("  [{}] {}", i, chunk_str);
    }

    match serde_json::to_string_pretty(request) {
        Ok(json) => {
            error!("(turn on DEBUG logging for the raw JSON request)");
            debug!("Full request JSON:\n{}", json);
        }
        Err(e) => error!("Failed to serialize request: {}", e),
    }

    if let Some(last_user_msg) = request
        .messages
        .iter()
        .rev()
        .find(|m| matches!(m.role, MessageRole::User))
    {
        let truncated = if last_user_msg.content.chars().count() > 500 {
            let chars: String = last_user_msg.content.chars().take(500).collect();
            format!("{}... (truncated)", chars)
        } else {
            last_user_msg.content.clone()
        };
        error!("Last user message: {}", truncated);
    }

    error!("Context window state:");
    error!(
        "  - Used tokens: {}/{}",
        context_window.used_tokens, context_window.total_tokens
    );
    error!(
        "  - Percentage used: {:.1}%",
        context_window.percentage_used()
    );
    error!(
        "  - Conversation history length: {}",
        context_window.conversation_history.len()
    );

    error!("Session ID: {:?}", session_id);
    error!("=== END STREAM ERROR ===");
}

/// Truncate a string value for display, respecting UTF-8 boundaries.
/// Takes only the first line to avoid multi-line output in compact display.
pub fn truncate_for_display(s: &str, max_len: usize) -> String {
    let s = s.lines().next().unwrap_or(s);
    if s.chars().count() <= max_len {
        s.to_string()
    } else {
        let truncated: String = s.char_indices().take(max_len).map(|(_, c)| c).collect();
        format!("{}...", truncated)
    }
}

/// Truncate a line for tool output display
pub fn truncate_line(line: &str, max_width: usize, should_truncate: bool) -> String {
    if !should_truncate || line.chars().count() <= max_width {
        line.to_string()
    } else {
        let truncated: String = line.chars().take(max_width.saturating_sub(3)).collect();
        format!("{truncated}...")
    }
}

/// Check if two tool calls are duplicates (same tool and args)
pub fn are_tool_calls_duplicate(tc1: &ToolCall, tc2: &ToolCall) -> bool {
    tc1.tool == tc2.tool && tc1.args == tc2.args
}

/// Format a tool argument value for display (truncated for readability).
/// Special handling for shell commands (show first line only if multiline).
pub fn format_tool_arg_value(tool_name: &str, key: &str, value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::String(s) => {
            if tool_name == "shell" && key == "command" {
                // For shell commands, show first line only if multiline
                match s.lines().next() {
                    Some(first_line) if s.lines().count() > 1 => format!("{}...", first_line),
                    Some(first_line) => first_line.to_string(),
                    None => s.clone(),
                }
            } else if s.chars().count() > 100 {
                truncate_for_display(s, 100)
            } else {
                s.clone()
            }
        }
        _ => value.to_string(),
    }
}

/// Format tool output lines for display, respecting machine vs human mode.
pub fn format_tool_output_summary(
    output: &str,
    max_lines: usize,
    max_width: usize,
    wants_full: bool,
) -> Vec<String> {
    let lines: Vec<&str> = output.lines().collect();
    let limit = if wants_full { lines.len() } else { max_lines };

    lines
        .iter()
        .take(limit)
        .map(|line| truncate_line(line, max_width, !wants_full))
        .collect()
}

/// Format a read_file result summary (e.g., "✅ read 42 lines | 1.2k chars").
pub fn format_read_file_summary(line_count: usize, char_count: usize) -> String {
    let char_display = if char_count >= 1000 {
        format!("{:.1}k", char_count as f64 / 1000.0)
    } else {
        format!("{}", char_count)
    };
    format!("{} lines ({} chars)", line_count, char_display)
}

/// Format a write_file result summary.
pub fn format_write_file_summary(line_count: usize, char_count: usize) -> String {
    let char_display = if char_count >= 1000 {
        format!("{:.1}k", char_count as f64 / 1000.0)
    } else {
        format!("{}", char_count)
    };
    format!("wrote {} lines | {} chars", line_count, char_display)
}

/// Format a write_file result for compact display.
/// Parses the tool result which is in format: "wrote N lines | M chars"
/// Returns a compact summary like "wrote N lines | M chars"
pub fn format_write_file_result(tool_result: &str) -> String {
    // Parse "wrote N lines | M chars" or "wrote N lines | M.Mk chars"
    if let Some(rest) = tool_result.strip_prefix("wrote ") {
        // rest is "N lines | M chars" or "N lines | M.Mk chars"
        if let Some((lines_part, chars_part)) = rest.split_once(" | ") {
            let lines = lines_part.trim_end_matches(" lines");
            let chars = chars_part.trim_end_matches(" chars");
            return format!("wrote {} lines | {} chars", lines, chars);
        }
    }
    // Fallback: return the original result if parsing fails
    tool_result.to_string()
}

/// Format a str_replace result summary.
pub fn format_str_replace_summary(insertions: i32, deletions: i32) -> String {
    if insertions > 0 && deletions > 0 {
        format!("+{} | -{}", insertions, deletions)
    } else if insertions > 0 {
        format!("+{}", insertions)
    } else {
        format!("-{}", deletions)
    }
}

/// Format a remember tool result summary.
pub fn format_remember_summary(result: &str) -> String {
    // Result format: "Memory updated. Size: 1.2k" or similar
    if let Some(size_pos) = result.find("Size: ") {
        let size_str = &result[size_pos + 6..];
        let size = size_str.split_whitespace().next().unwrap_or("?");
        format!("📝 memory updated ({})", size)
    } else {
        "📝 memory updated".to_string()
    }
}

/// Format a take_screenshot result summary.
pub fn format_screenshot_summary(result: &str) -> String {
    // Result format: "✅ Screenshot of X saved to: /path/to/file.png"
    if let Some(path_pos) = result.find("saved to: ") {
        let path = &result[path_pos + 10..].trim();
        format!("📸 {}", path)
    } else if result.contains("❌") {
        "❌ failed".to_string()
    } else {
        "📸 saved".to_string()
    }
}

/// Format a code_coverage result summary.
pub fn format_coverage_summary(result: &str) -> String {
    // Try to extract coverage percentage from result
    if result.contains("❌") {
        "❌ failed".to_string()
    } else {
        "📊 report generated".to_string()
    }
}

/// Format a rehydrate result summary.
pub fn format_rehydrate_summary(result: &str) -> String {
    // Result format: "✅ Rehydrated fragment 'abc123' (47 messages, ~18500 tokens)"
    if let Some(start) = result.find("fragment '") {
        let after = &result[start + 10..];
        if let Some(end) = after.find("'") {
            let fragment_id = &after[..end];
            return format!("🔄 restored '{}'", fragment_id);
        }
    }
    if result.contains("❌") {
        "❌ failed".to_string()
    } else {
        "🔄 restored".to_string()
    }
}

/// Format a code_search result summary.
pub fn format_code_search_summary(result: &str) -> String {
    // Result format: "✅ Code search completed\n{json}"
    // JSON contains: {"searches": [...], "total_matches": N, "total_files_searched": M}
    if result.contains("❌") {
        "❌ failed".to_string()
    } else if let Some(json_start) = result.find('{') {
        // Try to parse the JSON to extract summary stats
        if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&result[json_start..]) {
            let matches = parsed
                .get("total_matches")
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            let files = parsed
                .get("total_files_searched")
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            return format!("🔍 {} matches in {} files", matches, files);
        }
        "🔍 search complete".to_string()
    } else {
        "🔍 search complete".to_string()
    }
}

/// Format a plan_approve result summary.
pub fn format_plan_approve_summary(result: &str) -> String {
    // Result formats:
    // "✅ Plan approved at revision N. You may now begin implementation."
    // "ℹ️ Plan already approved at revision N. Current revision: M"
    // "❌ No plan exists to approve..."
    if result.contains("❌") {
        "❌ failed".to_string()
    } else if result.contains("already approved") {
        "ℹ️ already approved".to_string()
    } else if let Some(pos) = result.find("revision ") {
        let after = &result[pos + 9..];
        let rev: String = after.chars().take_while(|c| c.is_ascii_digit()).collect();
        format!("✅ approved rev {}", rev)
    } else {
        "✅ approved".to_string()
    }
}

/// Format a load_toolset result summary.
pub fn format_load_toolset_summary(result: &str) -> String {
    // Result formats:
    // "✅ Loaded toolset 'webdriver' with 8 tools:\n\n..."
    // "✅ Toolset 'webdriver' is already loaded. You can use its tools."
    // "❌ Unknown toolset 'foo'..."
    if result.contains("❌") {
        "❌ failed".to_string()
    } else if result.contains("already loaded") {
        "ℹ️ already loaded".to_string()
    } else if let Some(start) = result.find("toolset '") {
        let after = &result[start + 9..];
        if let Some(end) = after.find('\'') {
            let name = &after[..end];
            return format!("🧩 loaded '{}'", name);
        }
        "🧩 loaded".to_string()
    } else {
        "🧩 loaded".to_string()
    }
}

// =============================================================================
// Tool Call Deduplication
// =============================================================================

/// Result of deduplicating a batch of tool calls.
/// Each tool call is paired with an optional duplicate marker.
pub type DeduplicatedTools = Vec<(ToolCall, Option<String>)>;

/// Deduplicate tool calls, detecting sequential duplicates within a chunk
/// and duplicates against the previous message.
///
/// Returns each tool call paired with `Some("DUP IN CHUNK")` or `Some("DUP IN MSG")`
/// if it's a duplicate, or `None` if it should be executed.
pub fn deduplicate_tool_calls<F>(
    tool_calls: Vec<ToolCall>,
    check_previous_message: F,
) -> DeduplicatedTools
where
    F: Fn(&ToolCall) -> Option<String>,
{
    let mut last_tool_in_chunk: Option<ToolCall> = None;
    let mut result = Vec::with_capacity(tool_calls.len());

    for tool_call in tool_calls {
        let duplicate_type = if let Some(ref last) = last_tool_in_chunk {
            // Check for sequential duplicate within this chunk
            if are_tool_calls_duplicate(last, &tool_call) {
                Some("DUP IN CHUNK".to_string())
            } else {
                None
            }
        } else {
            // First tool in chunk - check against previous message
            check_previous_message(&tool_call)
        };

        last_tool_in_chunk = Some(tool_call.clone());
        result.push((tool_call, duplicate_type));
    }

    result
}

/// Build a deterministic fingerprint for a tool call using tool name and normalized args.
/// Object keys are sorted recursively so equivalent JSON payloads hash identically.
pub fn tool_fingerprint(tool_name: &str, args: &Value) -> String {
    fn normalize(value: &Value) -> Value {
        match value {
            Value::Object(obj) => {
                let mut keys: Vec<&String> = obj.keys().collect();
                keys.sort();
                let mut normalized = Map::new();
                for key in keys {
                    if let Some(val) = obj.get(key) {
                        normalized.insert(key.clone(), normalize(val));
                    }
                }
                Value::Object(normalized)
            }
            Value::Array(arr) => Value::Array(arr.iter().map(normalize).collect()),
            _ => value.clone(),
        }
    }

    let normalized_args = normalize(args);
    let normalized_json =
        serde_json::to_string(&normalized_args).unwrap_or_else(|_| String::from("{}"));
    format!("{tool_name}|{normalized_json}")
}

/// Tools that are expected to be called repeatedly with identical args as a polling pattern.
/// These should not be blocked by turn-scoped duplicate suppression.
pub fn is_turn_dedup_exempt_tool(tool_name: &str) -> bool {
    matches!(tool_name, "research_status")
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DedupTextResult {
    pub content: String,
    pub dropped_chars: usize,
    pub dropped_events: u32,
}

/// Conservative dedup for streaming text:
/// - drops exact consecutive duplicates already present at the tail of `previous`,
/// - collapses exact in-chunk repetition (e.g. "abcabc" -> "abc"),
/// - does not attempt overlap merge for partial matches.
pub fn dedup_consecutive_text(previous: &str, incoming: &str) -> DedupTextResult {
    if incoming.is_empty() {
        return DedupTextResult::default();
    }

    let mut dropped_chars = 0usize;
    let mut dropped_events = 0u32;

    let (collapsed, internal_dropped_chars) = collapse_exact_repetition(incoming);
    if internal_dropped_chars > 0 {
        dropped_chars += internal_dropped_chars;
        dropped_events += 1;
    }

    if !collapsed.is_empty() && previous.ends_with(&collapsed) {
        dropped_chars += collapsed.len();
        dropped_events += 1;
        return DedupTextResult {
            content: String::new(),
            dropped_chars,
            dropped_events,
        };
    }

    DedupTextResult {
        content: collapsed,
        dropped_chars,
        dropped_events,
    }
}

fn collapse_exact_repetition(incoming: &str) -> (String, usize) {
    if incoming.is_empty() {
        return (String::new(), 0);
    }

    let len = incoming.len();
    let mut unit_boundaries: Vec<usize> = incoming.char_indices().map(|(i, _)| i).collect();
    unit_boundaries.push(len);

    for &unit_len in unit_boundaries.iter().skip(1) {
        if unit_len == len || len % unit_len != 0 {
            continue;
        }

        let unit = &incoming[..unit_len];
        let repeats = len / unit_len;
        if repeats > 1 && unit.repeat(repeats) == incoming {
            return (unit.to_string(), len - unit_len);
        }
    }

    (incoming.to_string(), 0)
}

// =============================================================================
// Auto-Continue Decision Logic
// =============================================================================

/// Reasons why the streaming loop should auto-continue.
#[derive(Debug, Clone, PartialEq)]
pub enum AutoContinueReason {
    /// Tools were executed and we're in autonomous mode
    ToolsExecuted,
    /// LLM emitted an incomplete (truncated) tool call
    IncompleteToolCall,
    /// LLM emitted a tool call that wasn't executed
    UnexecutedToolCall,
    /// Response was truncated due to max_tokens limit
    MaxTokensTruncation,
}

/// Determine if the streaming loop should auto-continue.
/// Returns `Some(reason)` if it should continue, `None` otherwise.
pub fn should_auto_continue(
    is_autonomous: bool,
    any_tool_executed: bool,
    has_incomplete_tool_call: bool,
    has_unexecuted_tool_call: bool,
    was_truncated: bool,
) -> Option<AutoContinueReason> {
    if !is_autonomous {
        return None;
    }

    if any_tool_executed {
        Some(AutoContinueReason::ToolsExecuted)
    } else if has_incomplete_tool_call {
        Some(AutoContinueReason::IncompleteToolCall)
    } else if has_unexecuted_tool_call {
        Some(AutoContinueReason::UnexecutedToolCall)
    } else if was_truncated {
        Some(AutoContinueReason::MaxTokensTruncation)
    } else {
        None
    }
}

/// Determine if a response is essentially empty (whitespace or timing only)
pub fn is_empty_response(response: &str) -> bool {
    response.trim().is_empty()
        || response
            .lines()
            .all(|line| line.trim().is_empty() || line.trim().starts_with("⏱️"))
}

/// Check if an error is a recoverable connection error
pub fn is_connection_error(error_msg: &str) -> bool {
    error_msg.contains("unexpected EOF")
        || error_msg.contains("connection")
        || error_msg.contains("chunk size line")
        || error_msg.contains("body error")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_clean_llm_tokens() {
        assert_eq!(clean_llm_tokens("hello<|im_end|>"), "hello");
        assert_eq!(clean_llm_tokens("test</s>more"), "testmore");
        assert_eq!(clean_llm_tokens("[/INST]response"), "response");
    }

    #[test]
    fn test_format_duration() {
        assert_eq!(format_duration(Duration::from_millis(500)), "500ms");
        assert_eq!(format_duration(Duration::from_millis(1500)), "1.5s");
        assert_eq!(format_duration(Duration::from_secs(90)), "1m 30.0s");
    }

    #[test]
    fn test_truncate_for_display() {
        assert_eq!(truncate_for_display("short", 10), "short");
        assert_eq!(truncate_for_display("this is long", 5), "this ...");
        // Multi-line input should only use first line
        assert_eq!(
            truncate_for_display("first line\nsecond line", 20),
            "first line"
        );
        assert_eq!(
            truncate_for_display("❌ Error\nDetails here", 10),
            "❌ Error"
        );
    }

    #[test]
    fn test_is_empty_response() {
        assert!(is_empty_response(""));
        assert!(is_empty_response("   \n  "));
        assert!(is_empty_response("⏱️ 1.5s"));
        assert!(!is_empty_response("actual content"));
    }

    #[test]
    fn test_is_connection_error() {
        assert!(is_connection_error("unexpected EOF during read"));
        assert!(is_connection_error("connection reset"));
        assert!(!is_connection_error("invalid JSON"));
    }

    #[test]
    fn test_format_tool_arg_value_shell_command() {
        let val = serde_json::json!("echo hello\necho world");
        assert_eq!(
            format_tool_arg_value("shell", "command", &val),
            "echo hello..."
        );

        let single_line = serde_json::json!("ls -la");
        assert_eq!(
            format_tool_arg_value("shell", "command", &single_line),
            "ls -la"
        );
    }

    #[test]
    fn test_format_tool_arg_value_truncation() {
        let long_str = "a".repeat(150);
        let val = serde_json::json!(long_str);
        let result = format_tool_arg_value("read_file", "path", &val);
        assert!(result.len() < 110); // 100 chars + "..."
        assert!(result.ends_with("..."));
    }

    #[test]
    fn test_format_read_file_summary() {
        assert_eq!(format_read_file_summary(42, 500), "42 lines (500 chars)");
        assert_eq!(
            format_read_file_summary(100, 1500),
            "100 lines (1.5k chars)"
        );
    }

    #[test]
    fn test_format_tool_output_summary() {
        let output = "line1\nline2\nline3\nline4\nline5\nline6";
        let lines = format_tool_output_summary(output, 3, 80, false);
        assert_eq!(lines.len(), 3);
        assert_eq!(lines[0], "line1");
    }

    #[test]
    fn test_deduplicate_tool_calls_no_duplicates() {
        let tools = vec![
            ToolCall {
                tool: "shell".to_string(),
                args: serde_json::json!({"command": "ls"}),
                id: String::new(),
            },
            ToolCall {
                tool: "read_file".to_string(),
                args: serde_json::json!({"path": "foo.rs"}),
                id: String::new(),
            },
        ];

        let result = deduplicate_tool_calls(tools, |_| None);

        assert_eq!(result.len(), 2);
        assert!(result[0].1.is_none());
        assert!(result[1].1.is_none());
    }

    #[test]
    fn test_deduplicate_tool_calls_sequential_duplicate() {
        let tools = vec![
            ToolCall {
                tool: "shell".to_string(),
                args: serde_json::json!({"command": "ls"}),
                id: String::new(),
            },
            ToolCall {
                tool: "shell".to_string(),
                args: serde_json::json!({"command": "ls"}),
                id: String::new(),
            },
        ];

        let result = deduplicate_tool_calls(tools, |_| None);

        assert_eq!(result.len(), 2);
        assert!(result[0].1.is_none(), "First should not be duplicate");
        assert_eq!(result[1].1, Some("DUP IN CHUNK".to_string()));
    }

    #[test]
    fn test_deduplicate_tool_calls_previous_message_duplicate() {
        let tools = vec![ToolCall {
            tool: "shell".to_string(),
            args: serde_json::json!({"command": "ls"}),
            id: String::new(),
        }];

        // Simulate finding a duplicate in previous message
        let result = deduplicate_tool_calls(tools, |_| Some("DUP IN MSG".to_string()));

        assert_eq!(result.len(), 1);
        assert_eq!(result[0].1, Some("DUP IN MSG".to_string()));
    }

    #[test]
    fn test_tool_fingerprint_normalizes_object_key_order() {
        let left = serde_json::json!({
            "z": 1,
            "a": {"b": 2, "a": 1},
            "arr": [{"y": 2, "x": 1}]
        });
        let right = serde_json::json!({
            "arr": [{"x": 1, "y": 2}],
            "a": {"a": 1, "b": 2},
            "z": 1
        });

        assert_eq!(
            tool_fingerprint("shell", &left),
            tool_fingerprint("shell", &right)
        );
    }

    #[test]
    fn test_turn_dedup_exempt_tool() {
        assert!(is_turn_dedup_exempt_tool("research_status"));
        assert!(!is_turn_dedup_exempt_tool("shell"));
    }

    #[test]
    fn test_dedup_consecutive_text_drops_tail_duplicate() {
        let result = dedup_consecutive_text("hello world", "world");
        assert_eq!(result.content, "");
        assert_eq!(result.dropped_events, 1);
        assert_eq!(result.dropped_chars, "world".len());
    }

    #[test]
    fn test_dedup_consecutive_text_collapses_internal_repeat() {
        let result = dedup_consecutive_text("", "Yes. Yes. Yes. ");
        assert_eq!(result.content, "Yes. ");
        assert_eq!(result.dropped_events, 1);
        assert_eq!(
            result.dropped_chars,
            "Yes. Yes. Yes. ".len() - "Yes. ".len()
        );
    }

    #[test]
    fn test_dedup_consecutive_text_keeps_non_duplicate_content() {
        let result = dedup_consecutive_text("abc", "bcx");
        assert_eq!(
            result,
            DedupTextResult {
                content: "bcx".to_string(),
                dropped_chars: 0,
                dropped_events: 0,
            }
        );
    }

    #[test]
    fn test_should_auto_continue_not_autonomous() {
        // Never auto-continue in interactive mode
        assert_eq!(should_auto_continue(false, true, false, false, false), None);
        assert_eq!(should_auto_continue(false, false, true, false, false), None);
    }

    #[test]
    fn test_should_auto_continue_autonomous() {
        use AutoContinueReason::*;

        // Tools executed
        assert_eq!(
            should_auto_continue(true, true, false, false, false),
            Some(ToolsExecuted)
        );

        // Incomplete tool call
        assert_eq!(
            should_auto_continue(true, false, true, false, false),
            Some(IncompleteToolCall)
        );

        // Unexecuted tool call
        assert_eq!(
            should_auto_continue(true, false, false, true, false),
            Some(UnexecutedToolCall)
        );

        // Max tokens truncation
        assert_eq!(
            should_auto_continue(true, false, false, false, true),
            Some(MaxTokensTruncation)
        );

        // Nothing special - no auto-continue
        assert_eq!(should_auto_continue(true, false, false, false, false), None);
    }
}
