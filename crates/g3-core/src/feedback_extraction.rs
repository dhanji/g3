//! Coach feedback extraction module
//!
//! This module provides robust extraction of coach feedback from various sources:
//! - Session log files (JSON format)
//! - Native tool calling JSON format
//! - Conversation history
//! - TaskResult response fallback
//!
//! Used by both autonomous mode (g3-cli) and planning mode (g3-planner).

use crate::{Agent, TaskResult};
use crate::ui_writer::UiWriter;
use serde_json::Value;
use tracing::{debug, warn};

/// Result of feedback extraction with source information
#[derive(Debug, Clone)]
pub struct ExtractedFeedback {
    /// The extracted feedback text
    pub content: String,
    /// The source where feedback was found
    pub source: FeedbackSource,
}

/// Source of the extracted feedback
#[derive(Debug, Clone, PartialEq)]
pub enum FeedbackSource {
    /// From session log file (verified final_output tool call)
    SessionLog,
    /// From native tool call JSON in response
    NativeToolCall,
    /// From conversation history in agent
    ConversationHistory,
    /// From TaskResult response (fallback)
    TaskResultResponse,
    /// Default fallback message
    DefaultFallback,
}

impl ExtractedFeedback {
    /// Create a new extracted feedback
    pub fn new(content: String, source: FeedbackSource) -> Self {
        Self { content, source }
    }

    /// Check if the feedback indicates approval
    pub fn is_approved(&self) -> bool {
        self.content.contains("IMPLEMENTATION_APPROVED")
    }

    /// Check if the feedback is a fallback/default
    pub fn is_fallback(&self) -> bool {
        self.source == FeedbackSource::DefaultFallback
    }
}

/// Configuration for feedback extraction
#[derive(Debug, Clone)]
pub struct FeedbackExtractionConfig {
    /// Whether to print debug information
    pub verbose: bool,
    /// Default feedback message if extraction fails
    pub default_feedback: String,
}

impl Default for FeedbackExtractionConfig {
    fn default() -> Self {
        Self {
            verbose: false,
            default_feedback: "The implementation needs review. Please ensure all requirements are met and the code compiles without errors.".to_string(),
        }
    }
}

/// Extract coach feedback using multiple fallback methods
///
/// Tries extraction in this order:
/// 1. Session log file (most reliable for final_output tool calls)
/// 2. Native tool call JSON in the response
/// 3. Conversation history from the agent
/// 4. TaskResult response parsing
/// 5. Default fallback message
///
/// # Arguments
/// * `coach_result` - The task result from coach execution
/// * `agent` - The coach agent (for session ID and conversation history)
/// * `config` - Extraction configuration
///
/// # Returns
/// Extracted feedback with source information, never fails
pub fn extract_coach_feedback<W>(
    coach_result: &TaskResult,
    agent: &Agent<W>,
    config: &FeedbackExtractionConfig,
) -> ExtractedFeedback
where
    W: UiWriter + Send + Sync + 'static,
{
    // Try session log with verified final_output pattern first.
    if let Some(session_id) = agent.get_session_id() {
        if let Some(feedback) = try_extract_from_session_log(&session_id, config) {
            debug!("Extracted coach feedback from session log (final_output): {} chars", feedback.len());
            return ExtractedFeedback::new(feedback, FeedbackSource::SessionLog);
        }
    }

    // Fallback: Try native tool call JSON parsing (backwards compatibility)
    if let Some(feedback) = try_extract_from_native_tool_call(&coach_result.response) {
        debug!("Extracted coach feedback from native tool call: {} chars", feedback.len());
        return ExtractedFeedback::new(feedback, FeedbackSource::NativeToolCall);
    }

    // Fallback: Try conversation history with final_output pattern (backwards compatibility)
    if let Some(session_id) = agent.get_session_id() {
        if let Some(feedback) = try_extract_from_conversation_history(&session_id, config) {
            debug!("Extracted coach feedback from conversation history: {} chars", feedback.len());
            return ExtractedFeedback::new(feedback, FeedbackSource::ConversationHistory);
        }
    }

    // Fallback: Try TaskResult parsing (extracts last text block)
    let extracted = coach_result.extract_final_output();
    if !extracted.is_empty() && !is_meta_coach_text(&extracted) {
        debug!("Extracted coach feedback from task result: {} chars", extracted.len());
        return ExtractedFeedback::new(extracted, FeedbackSource::TaskResultResponse);
    }

    // Last resort: use the last assistant message, but skip obvious meta-review chatter.
    if let Some(session_id) = agent.get_session_id() {
        if let Some(feedback) = try_extract_last_assistant_message(&session_id, config) {
            debug!(
                "Extracted coach feedback from last assistant message: {} chars",
                feedback.len()
            );
            return ExtractedFeedback::new(feedback, FeedbackSource::ConversationHistory);
        }
    }

    // Fallback to default
    warn!("Could not extract coach feedback, using default");
    ExtractedFeedback::new(config.default_feedback.clone(), FeedbackSource::DefaultFallback)
}

/// Try to extract the last assistant message from session log (PRIMARY method)
/// This is the preferred extraction method - looks for the last substantial
/// assistant message content, regardless of whether it used final_output tool.
fn try_extract_last_assistant_message(
    session_id: &str,
    config: &FeedbackExtractionConfig,
) -> Option<String> {
    let _ = config; // config no longer used but kept for API compatibility
    
    // Use .g3/sessions/<session_id>/session.json path
    let log_file_path = crate::get_session_file(session_id);
    if !log_file_path.exists() {
        debug!("Session log file not found: {:?}", log_file_path);
        return None;
    }

    let log_content = std::fs::read_to_string(&log_file_path).ok()?;
    let log_json: Value = serde_json::from_str(&log_content).ok()?;

    // Try to get conversation history from context_window
    let messages = log_json
        .get("context_window")?
        .get("conversation_history")?
        .as_array()?;

    extract_last_meaningful_assistant_message(messages)
}

fn extract_last_meaningful_assistant_message(messages: &[Value]) -> Option<String> {
    for msg in messages.iter().rev() {
        let role = msg.get("role").and_then(|v| v.as_str())?;

        if role.eq_ignore_ascii_case("assistant") {
            if let Some(text) = extract_assistant_text(msg) {
                if !is_meta_coach_text(&text) {
                    return Some(text);
                }
            }
        }
    }

    None
}

fn extract_assistant_text(msg: &Value) -> Option<String> {
    let content = msg.get("content")?;

    if let Some(content_str) = content.as_str() {
        let trimmed = content_str.trim();
        if !trimmed.is_empty() && trimmed.len() > 10 {
            return Some(trimmed.to_string());
        }
    }

    if let Some(content_array) = content.as_array() {
        for block in content_array {
            if block.get("type").and_then(|v| v.as_str()) == Some("text") {
                if let Some(text) = block.get("text").and_then(|v| v.as_str()) {
                    let trimmed = text.trim();
                    if !trimmed.is_empty() && trimmed.len() > 10 {
                        return Some(trimmed.to_string());
                    }
                }
            }
        }
    }

    None
}

fn is_meta_coach_text(text: &str) -> bool {
    let normalized = text.trim().to_ascii_lowercase();
    let prefixes = [
        "let me ",
        "now let me ",
        "i'll ",
        "i will ",
        "first, let me ",
    ];
    let meta_markers = [
        "run the acceptance commands",
        "check the remaining",
        "review the requirements more carefully",
        "read the spec more carefully",
        "check several important details",
        "check the current working directory",
        "find the actual workspace directory",
        "deeper review of the implementation quality",
        "more carefully",
    ];

    prefixes.iter().any(|p| normalized.starts_with(p))
        && meta_markers.iter().any(|m| normalized.contains(m))
}

/// Try to extract feedback from session log file
fn try_extract_from_session_log(
    session_id: &str,
    config: &FeedbackExtractionConfig,
) -> Option<String> {
    let _ = config; // config no longer used but kept for API compatibility
    
    // Use .g3/sessions/<session_id>/session.json path
    let log_file_path = crate::get_session_file(session_id);
    if !log_file_path.exists() {
        debug!("Session log file not found: {:?}", log_file_path);
        return None;
    }

    let log_content = std::fs::read_to_string(&log_file_path).ok()?;
    let log_json: Value = serde_json::from_str(&log_content).ok()?;

    // Try to get conversation history from context_window
    let messages = log_json
        .get("context_window")?
        .get("conversation_history")?
        .as_array()?;

    // Search backwards for final_output tool result
    extract_final_output_from_messages(messages)
}

/// Try to extract feedback from native tool call JSON in response
fn try_extract_from_native_tool_call(response: &str) -> Option<String> {
    // Look for various patterns of final_output tool calls
    
    // Pattern 1: JSON tool call with "tool": "final_output"
    if let Some(feedback) = try_extract_json_tool_call(response) {
        return Some(feedback);
    }

    // Pattern 2: Anthropic-style native tool use block
    if let Some(feedback) = try_extract_anthropic_tool_use(response) {
        return Some(feedback);
    }

    // Pattern 3: OpenAI-style function call
    if let Some(feedback) = try_extract_openai_function_call(response) {
        return Some(feedback);
    }

    None
}

/// Extract JSON tool call pattern
fn try_extract_json_tool_call(response: &str) -> Option<String> {
    // Look for {"tool": "final_output", "args": {"summary": "..."}}
    let mut search_pos = 0;
    while let Some(pos) = response[search_pos..].find("\"tool\"") {
        let actual_pos = search_pos + pos;
        
        // Find the start of the JSON object
        let json_start = response[..actual_pos].rfind('{')?;
        
        // Try to find matching closing brace
        if let Some(json_str) = extract_balanced_json(&response[json_start..]) {
            if let Ok(json) = serde_json::from_str::<Value>(&json_str) {
                if json.get("tool").and_then(|v| v.as_str()) == Some("final_output") {
                    if let Some(args) = json.get("args") {
                        if let Some(summary) = args.get("summary").and_then(|v| v.as_str()) {
                            return Some(summary.to_string());
                        }
                    }
                }
            }
        }
        
        search_pos = actual_pos + 1;
    }
    
    None
}

/// Extract Anthropic-style tool use block
fn try_extract_anthropic_tool_use(response: &str) -> Option<String> {
    // Look for content_block with type "tool_use" and name "final_output"
    if !response.contains("tool_use") || !response.contains("final_output") {
        return None;
    }

    // Try to parse as JSON array of content blocks
    if let Some(start) = response.find('[') {
        if let Some(json_str) = extract_balanced_json(&response[start..]) {
            if let Ok(blocks) = serde_json::from_str::<Vec<Value>>(&json_str) {
                for block in blocks {
                    if block.get("type").and_then(|v| v.as_str()) == Some("tool_use")
                        && block.get("name").and_then(|v| v.as_str()) == Some("final_output")
                    {
                        if let Some(input) = block.get("input") {
                            if let Some(summary) = input.get("summary").and_then(|v| v.as_str()) {
                                return Some(summary.to_string());
                            }
                        }
                    }
                }
            }
        }
    }

    None
}

/// Extract OpenAI-style function call
fn try_extract_openai_function_call(response: &str) -> Option<String> {
    // Look for function_call or tool_calls with final_output
    if !response.contains("final_output") {
        return None;
    }

    // Try to find function call JSON
    if let Some(pos) = response.find("\"function_call\"") {
        if let Some(json_start) = response[pos..].find('{') {
            let start = pos + json_start;
            if let Some(json_str) = extract_balanced_json(&response[start..]) {
                if let Ok(json) = serde_json::from_str::<Value>(&json_str) {
                    if json.get("name").and_then(|v| v.as_str()) == Some("final_output") {
                        if let Some(args_str) = json.get("arguments").and_then(|v| v.as_str()) {
                            if let Ok(args) = serde_json::from_str::<Value>(args_str) {
                                if let Some(summary) = args.get("summary").and_then(|v| v.as_str()) {
                                    return Some(summary.to_string());
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    None
}

/// Try to extract from conversation history in session log
fn try_extract_from_conversation_history(
    session_id: &str,
    config: &FeedbackExtractionConfig,
) -> Option<String> {
    let _ = config; // config no longer used but kept for API compatibility
    
    // Use .g3/sessions/<session_id>/session.json path
    let log_file_path = crate::get_session_file(session_id);
    if !log_file_path.exists() {
        return None;
    }

    let log_content = std::fs::read_to_string(&log_file_path).ok()?;
    let log_json: Value = serde_json::from_str(&log_content).ok()?;

    // Check for tool_calls array in the log
    if let Some(tool_calls) = log_json.get("tool_calls").and_then(|v| v.as_array()) {
        // Look backwards for final_output
        for call in tool_calls.iter().rev() {
            if call.get("tool").and_then(|v| v.as_str()) == Some("final_output") {
                if let Some(args) = call.get("args") {
                    if let Some(summary) = args.get("summary").and_then(|v| v.as_str()) {
                        return Some(summary.to_string());
                    }
                }
            }
        }
    }

    None
}

/// Extract final_output from message array
fn extract_final_output_from_messages(messages: &[Value]) -> Option<String> {
    // Go backwards through conversation to find the last final_output tool result
    for i in (0..messages.len()).rev() {
        let msg = &messages[i];
        let role = msg.get("role").and_then(|v| v.as_str())?;
        
        // Check for User message with "Tool result:"
        if role.eq_ignore_ascii_case("user") {
            if let Some(content) = msg.get("content").and_then(|v| v.as_str()) {
                if content.starts_with("Tool result:") {
                    // Verify preceding message was a final_output tool call
                    if i > 0 && is_final_output_tool_call(&messages[i - 1]) {
                        let feedback = content
                            .strip_prefix("Tool result: ")
                            .or_else(|| content.strip_prefix("Tool result:"))
                            .unwrap_or(content)
                            .to_string();
                        return Some(feedback);
                    }
                }
            }
        }
        
        // Also check for native tool results in assistant messages
        if role.eq_ignore_ascii_case("assistant") {
            if let Some(content) = msg.get("content") {
                // Could be string or array (for native tool calling)
                if let Some(content_str) = content.as_str() {
                    if let Some(feedback) = try_extract_from_native_tool_call(content_str) {
                        return Some(feedback);
                    }
                } else if let Some(content_array) = content.as_array() {
                    for block in content_array {
                        if block.get("type").and_then(|v| v.as_str()) == Some("tool_use")
                            && block.get("name").and_then(|v| v.as_str()) == Some("final_output")
                        {
                            if let Some(input) = block.get("input") {
                                if let Some(summary) = input.get("summary").and_then(|v| v.as_str()) {
                                    return Some(summary.to_string());
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    
    None
}

/// Check if a message is a final_output tool call
fn is_final_output_tool_call(msg: &Value) -> bool {
    let role = match msg.get("role").and_then(|v| v.as_str()) {
        Some(r) => r,
        None => return false,
    };
    
    if !role.eq_ignore_ascii_case("assistant") {
        return false;
    }
    
    if let Some(content) = msg.get("content") {
        // Check string content
        if let Some(content_str) = content.as_str() {
            if content_str.contains("\"tool\": \"final_output\"") 
               || content_str.contains("\"tool\":\"final_output\"") {
                return true;
            }
        }
        
        // Check array content (native tool calling)
        if let Some(content_array) = content.as_array() {
            for block in content_array {
                if block.get("type").and_then(|v| v.as_str()) == Some("tool_use")
                    && block.get("name").and_then(|v| v.as_str()) == Some("final_output")
                {
                    return true;
                }
            }
        }
    }
    
    // Check tool_calls field (OpenAI format)
    if let Some(tool_calls) = msg.get("tool_calls").and_then(|v| v.as_array()) {
        for call in tool_calls {
            if let Some(function) = call.get("function") {
                if function.get("name").and_then(|v| v.as_str()) == Some("final_output") {
                    return true;
                }
            }
        }
    }
    
    false
}

/// Extract a balanced JSON object/array from a string
fn extract_balanced_json(s: &str) -> Option<String> {
    let chars: Vec<char> = s.chars().collect();
    if chars.is_empty() {
        return None;
    }
    
    let opener = chars[0];
    let closer = match opener {
        '{' => '}',
        '[' => ']',
        _ => return None,
    };
    
    let mut depth = 0;
    let mut in_string = false;
    let mut escape_next = false;
    
    for (i, &c) in chars.iter().enumerate() {
        if escape_next {
            escape_next = false;
            continue;
        }
        
        if c == '\\' && in_string {
            escape_next = true;
            continue;
        }
        
        if c == '"' {
            in_string = !in_string;
            continue;
        }
        
        if in_string {
            continue;
        }
        
        if c == opener {
            depth += 1;
        } else if c == closer {
            depth -= 1;
            if depth == 0 {
                return Some(chars[..=i].iter().collect());
            }
        }
    }
    
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_balanced_json_object() {
        let input = r#"{"tool": "test", "args": {"key": "value"}} extra"#;
        let result = extract_balanced_json(input);
        assert_eq!(result, Some(r#"{"tool": "test", "args": {"key": "value"}}"#.to_string()));
    }

    #[test]
    fn test_extract_balanced_json_array() {
        let input = r#"[{"type": "test"}, {"type": "test2"}] extra"#;
        let result = extract_balanced_json(input);
        assert_eq!(result, Some(r#"[{"type": "test"}, {"type": "test2"}]"#.to_string()));
    }

    #[test]
    fn test_extract_balanced_json_with_strings() {
        let input = r#"{"message": "hello {world}", "count": 1}"#;
        let result = extract_balanced_json(input);
        assert_eq!(result, Some(input.to_string()));
    }

    #[test]
    fn test_try_extract_json_tool_call() {
        let response = r#"Some text {"tool": "final_output", "args": {"summary": "Test feedback"}} more text"#;
        let result = try_extract_json_tool_call(response);
        assert_eq!(result, Some("Test feedback".to_string()));
    }

    #[test]
    fn test_try_extract_json_tool_call_not_final_output() {
        let response = r#"{"tool": "shell", "args": {"command": "ls"}}"#;
        let result = try_extract_json_tool_call(response);
        assert_eq!(result, None);
    }

    #[test]
    fn test_is_final_output_tool_call_string() {
        let msg = serde_json::json!({
            "role": "assistant",
            "content": r#"{"tool": "final_output", "args": {"summary": "done"}}"#
        });
        assert!(is_final_output_tool_call(&msg));
    }

    #[test]
    fn test_is_final_output_tool_call_native() {
        let msg = serde_json::json!({
            "role": "assistant",
            "content": [{
                "type": "tool_use",
                "name": "final_output",
                "input": {"summary": "done"}
            }]
        });
        assert!(is_final_output_tool_call(&msg));
    }

    #[test]
    fn test_is_final_output_tool_call_openai() {
        let msg = serde_json::json!({
            "role": "assistant",
            "content": "",
            "tool_calls": [{
                "function": {
                    "name": "final_output",
                    "arguments": r#"{"summary": "done"}"#
                }
            }]
        });
        assert!(is_final_output_tool_call(&msg));
    }

    #[test]
    fn test_extracted_feedback_is_approved() {
        let feedback = ExtractedFeedback::new(
            "IMPLEMENTATION_APPROVED - great work!".to_string(),
            FeedbackSource::SessionLog,
        );
        assert!(feedback.is_approved());

        let feedback = ExtractedFeedback::new(
            "Please fix the following issues".to_string(),
            FeedbackSource::SessionLog,
        );
        assert!(!feedback.is_approved());
    }

    #[test]
    fn test_extracted_feedback_is_fallback() {
        let feedback = ExtractedFeedback::new(
            "Default message".to_string(),
            FeedbackSource::DefaultFallback,
        );
        assert!(feedback.is_fallback());

        let feedback = ExtractedFeedback::new(
            "Real feedback".to_string(),
            FeedbackSource::SessionLog,
        );
        assert!(!feedback.is_fallback());
    }

    #[test]
    fn test_feedback_extraction_config_default() {
        let config = FeedbackExtractionConfig::default();
        assert!(!config.verbose);
        assert!(config.default_feedback.contains("review"));
    }

    #[test]
    fn test_is_meta_coach_text_detects_review_chatter() {
        assert!(is_meta_coach_text(
            "Now let me check the attach_to_order method more carefully — specifically whether attaching to an order moves the state to linked."
        ));
        assert!(!is_meta_coach_text(
            "Fix reopened history persistence and make attach_to_order persist linked state."
        ));
    }

    #[test]
    fn test_extract_last_meaningful_assistant_message_skips_meta_review_chatter() {
        let messages = vec![
            serde_json::json!({
                "role": "assistant",
                "content": "Let me run the acceptance commands from the correct directory."
            }),
            serde_json::json!({
                "role": "assistant",
                "content": "Now let me check the attach_to_order method more carefully — specifically whether attaching to an order moves the state to linked."
            }),
            serde_json::json!({
                "role": "assistant",
                "content": "The implementation is missing reopened history preservation and attach_to_order does not persist linked state."
            }),
        ];

        let extracted = extract_last_meaningful_assistant_message(&messages);
        assert_eq!(
            extracted.as_deref(),
            Some(
                "The implementation is missing reopened history preservation and attach_to_order does not persist linked state."
            )
        );
    }

    #[test]
    fn test_extract_last_meaningful_assistant_message_returns_none_for_only_meta_chatter() {
        let messages = vec![
            serde_json::json!({
                "role": "assistant",
                "content": "Let me check the current working directory and run the tests."
            }),
            serde_json::json!({
                "role": "assistant",
                "content": "Now let me check the remaining acceptance commands individually and review the requirements more carefully."
            }),
        ];

        assert_eq!(extract_last_meaningful_assistant_message(&messages), None);
    }
}
