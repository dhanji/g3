pub mod background_process;
pub mod code_search;
pub mod error_handling;
pub mod feedback_extraction;
pub mod paths;
pub mod project;
pub mod retry;
pub mod session_continuation;
pub mod streaming_parser;
pub mod task_result;
pub mod ui_writer;
pub mod utils;
pub mod webdriver_session;

pub use task_result::TaskResult;
pub use retry::{RetryConfig, RetryResult, execute_with_retry, retry_operation};
pub use feedback_extraction::{ExtractedFeedback, FeedbackSource, FeedbackExtractionConfig, extract_coach_feedback};
pub use session_continuation::{SessionContinuation, load_continuation, save_continuation, clear_continuation, has_valid_continuation, get_session_dir, load_context_from_session_log};

// Export agent prompt generation for CLI use
pub use prompts::get_agent_system_prompt;

#[cfg(test)]
mod task_result_comprehensive_tests;
use crate::ui_writer::UiWriter;

#[cfg(test)]
mod tilde_expansion_tests;

#[cfg(test)]
mod error_handling_test;
mod prompts;

use anyhow::Result;
use g3_computer_control::WebDriverController;
use g3_config::Config;
use g3_execution::CodeExecutor;
use g3_providers::{CacheControl, CompletionRequest, Message, MessageRole, ProviderRegistry, Tool};
use prompts::{get_system_prompt_for_native, SYSTEM_PROMPT_FOR_NON_NATIVE_TOOL_USE};
#[allow(unused_imports)]
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::io::Write;
use std::time::{Duration, Instant};
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, warn};

// Re-export path utilities for backward compatibility
pub use paths::{
    G3_WORKSPACE_PATH_ENV, ensure_session_dir, get_context_summary_file, get_g3_dir, get_logs_dir,
    get_session_file, get_session_logs_dir, get_thinned_dir, logs_dir,
};
use paths::{get_todo_path, get_session_todo_path};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCall {
    pub tool: String,
    pub args: serde_json::Value, // Should be a JSON object with tool-specific arguments
}


// Re-export WebDriverSession from its own module
pub use webdriver_session::WebDriverSession;

/// Options for fast-start discovery execution
#[derive(Debug, Clone)]
pub struct DiscoveryOptions<'a> {
    pub messages: &'a [Message],
    pub fast_start_path: Option<&'a str>,
}

#[derive(Debug, Clone)]
pub enum StreamState {
    Generating,
    ToolDetected(ToolCall),
    Executing,
    Resuming,
}


// Re-export StreamingToolParser from its own module
pub use streaming_parser::StreamingToolParser;


#[derive(Debug, Clone)]
pub struct ContextWindow {
    pub used_tokens: u32,
    pub total_tokens: u32,
    pub cumulative_tokens: u32, // Track cumulative tokens across all interactions
    pub conversation_history: Vec<Message>,
    pub last_thinning_percentage: u32, // Track the last percentage at which we thinned
}

impl ContextWindow {
    pub fn new(total_tokens: u32) -> Self {
        Self {
            used_tokens: 0,
            total_tokens,
            cumulative_tokens: 0,
            conversation_history: Vec::new(),
            last_thinning_percentage: 0,
        }
    }

    pub fn add_message(&mut self, message: Message) {
        self.add_message_with_tokens(message, None);
    }

    /// Add a message with optional token count from the provider
    pub fn add_message_with_tokens(&mut self, message: Message, tokens: Option<u32>) {
        // Skip messages with empty content to avoid API errors
        if message.content.trim().is_empty() {
            warn!("Skipping empty message to avoid API error");
            return;
        }

        // Use provided token count if available, otherwise estimate
        let token_count = tokens.unwrap_or_else(|| Self::estimate_tokens(&message.content));
        self.used_tokens += token_count;
        self.cumulative_tokens += token_count;
        self.conversation_history.push(message);

        debug!(
            "Added message with {} tokens (used: {}/{}, cumulative: {})",
            token_count, self.used_tokens, self.total_tokens, self.cumulative_tokens
        );
    }

    /// Update token usage from provider response
    /// NOTE: This only updates cumulative_tokens (total API usage tracking).
    /// It does NOT update used_tokens because:
    /// 1. prompt_tokens represents the ENTIRE context sent to API (already tracked via add_message)
    /// 2. completion_tokens will be tracked when the assistant message is added via add_message
    /// Adding total_tokens here would cause double/triple counting and break the 80% threshold check.
    pub fn update_usage_from_response(&mut self, usage: &g3_providers::Usage) {
        // Only update cumulative tokens for API usage tracking
        // Do NOT update used_tokens - that's tracked via add_message to avoid double counting
        self.cumulative_tokens += usage.total_tokens;

        debug!(
            "Updated cumulative tokens: {} (used: {}/{}, cumulative: {})",
            usage.total_tokens, self.used_tokens, self.total_tokens, self.cumulative_tokens
        );
    }

    /// More accurate token estimation
    fn estimate_tokens(text: &str) -> u32 {
        // Better heuristic:
        // - Average English text: ~4 characters per token
        // - Code/JSON: ~3 characters per token (more symbols)
        // - Add 10% buffer for safety
        let base_estimate = if text.contains("{") || text.contains("```") || text.contains("fn ") {
            (text.len() as f32 / 3.0).ceil() as u32 // Code/JSON
        } else {
            (text.len() as f32 / 4.0).ceil() as u32 // Regular text
        };
        (base_estimate as f32 * 1.1).ceil() as u32 // Add 10% buffer
    }

    pub fn update_usage(&mut self, usage: &g3_providers::Usage) {
        // Deprecated: Use update_usage_from_response instead
        self.update_usage_from_response(usage);
    }

    /// Update cumulative token usage (for streaming) when no provider usage data is available
    /// NOTE: This only updates cumulative_tokens, not used_tokens.
    /// The assistant message will be added via add_message which tracks used_tokens.
    pub fn add_streaming_tokens(&mut self, new_tokens: u32) {
        // Only update cumulative tokens - used_tokens is tracked via add_message
        self.cumulative_tokens += new_tokens;
        debug!(
            "Updated cumulative streaming tokens: {} (used: {}/{}, cumulative: {})",
            new_tokens, self.used_tokens, self.total_tokens, self.cumulative_tokens
        );
    }

    pub fn percentage_used(&self) -> f32 {
        if self.total_tokens == 0 {
            0.0
        } else {
            (self.used_tokens as f32 / self.total_tokens as f32) * 100.0
        }
    }

    /// Clear the conversation history while preserving system messages
    /// Used by /clear command to start fresh
    pub fn clear_conversation(&mut self) {
        // Keep only system messages (system prompt, README, etc.)
        let system_messages: Vec<Message> = self.conversation_history
            .iter()
            .filter(|m| matches!(m.role, MessageRole::System))
            .cloned()
            .collect();
        
        self.conversation_history = system_messages;
        self.used_tokens = self.conversation_history.iter()
            .map(|m| Self::estimate_tokens(&m.content))
            .sum();
        self.last_thinning_percentage = 0;
    }

    pub fn remaining_tokens(&self) -> u32 {
        self.total_tokens.saturating_sub(self.used_tokens)
    }

    /// Check if we should trigger summarization (at 80% capacity)
    pub fn should_summarize(&self) -> bool {
        // Trigger at 80% OR if we're getting close to absolute limits
        // This prevents issues with models that have large contexts but still hit limits
        let percentage_trigger = self.percentage_used() >= 80.0;

        // Also trigger if we're approaching common token limits
        // Most models start having issues around 150k tokens
        let absolute_trigger = self.used_tokens > 150_000;

        percentage_trigger || absolute_trigger
    }

    /// Create a summary request prompt for the current conversation
    pub fn create_summary_prompt(&self) -> String {
        "Please provide a comprehensive summary of our conversation so far. Include:

1. **Main Topic/Goal**: What is the primary task or objective being worked on?
2. **Key Decisions**: What important decisions have been made?
3. **Actions Taken**: What specific actions, commands, or code changes have been completed?
4. **Current State**: What is the current status of the work?
5. **Important Context**: Any critical information, file paths, configurations, or constraints that should be remembered?
6. **Pending Items**: What remains to be done or what was the user's last request?

Format this as a detailed but concise summary that can be used to resume the conversation from scratch while maintaining full context.".to_string()
    }

    /// Reset the context window with a summary
    /// Preserves the original system prompt as the first message
    pub fn reset_with_summary(
        &mut self,
        summary: String,
        latest_user_message: Option<String>,
    ) -> usize {
        // Calculate chars saved (old history minus new summary)
        let old_chars: usize = self
            .conversation_history
            .iter()
            .map(|m| m.content.len())
            .sum();

        // Preserve the original system prompt (first message) and optionally the README (second message)
        let original_system_prompt = self.conversation_history.first().cloned();
        let readme_message = self.conversation_history.get(1).and_then(|msg| {
            if matches!(msg.role, MessageRole::System) && 
               (msg.content.contains("Project README") || msg.content.contains("Agent Configuration")) {
                Some(msg.clone())
            } else {
                None
            }
        });

        // Clear the conversation history
        self.conversation_history.clear();
        self.used_tokens = 0;

        // Re-add the original system prompt first (critical invariant)
        if let Some(system_prompt) = original_system_prompt {
            self.add_message(system_prompt);
        }

        // Re-add the README message if it existed
        if let Some(readme) = readme_message {
            self.add_message(readme);
        }

        // Add the summary as a system message
        let summary_message = Message::new(
            MessageRole::System,
            format!("Previous conversation summary:\n\n{}", summary),
        );
        self.add_message(summary_message);

        // Add the latest user message if provided
        if let Some(user_msg) = latest_user_message {
            self.add_message(Message::new(MessageRole::User, user_msg));
        }

        let new_chars: usize = self
            .conversation_history
            .iter()
            .map(|m| m.content.len())
            .sum();
        old_chars.saturating_sub(new_chars)
    }

    /// Check if we should trigger context thinning
    /// Triggers at 50%, 60%, 70%, and 80% thresholds
    pub fn should_thin(&self) -> bool {
        let current_percentage = self.percentage_used() as u32;

        // Check if we've crossed a new 10% threshold starting at 50%
        if current_percentage >= 50 {
            let current_threshold = (current_percentage / 10) * 10; // Round down to nearest 10%
            if current_threshold > self.last_thinning_percentage && current_threshold <= 80 {
                return true;
            }
        }

        false
    }

    /// Perform context thinning: scan first third of conversation and replace large tool results
    /// Returns a summary message about what was thinned
    /// If session_id is provided, thinned content is saved to .g3/session/<session_id>/thinned/
    pub fn thin_context(&mut self, session_id: Option<&str>) -> (String, usize) {
        let current_percentage = self.percentage_used() as u32;
        let current_threshold = (current_percentage / 10) * 10;

        // Update the last thinning percentage
        self.last_thinning_percentage = current_threshold;

        // Calculate the first third of the conversation
        let total_messages = self.conversation_history.len();
        let first_third_end = (total_messages / 3).max(1);

        let mut leaned_count = 0;
        let mut tool_call_leaned_count = 0;
        let mut chars_saved = 0;

        // Determine output directory: use session dir if available, otherwise ~/tmp
        let tmp_dir = if let Some(sid) = session_id {
            let thinned_dir = get_thinned_dir(sid);
            if let Err(e) = std::fs::create_dir_all(&thinned_dir) {
                warn!("Failed to create thinned directory: {}", e);
                return (
                    "⚠️  Context thinning failed: could not create thinned directory".to_string(),
                    0,
                );
            }
            thinned_dir.to_string_lossy().to_string()
        } else {
            let fallback_dir = shellexpand::tilde("~/tmp").to_string();
            if let Err(e) = std::fs::create_dir_all(&fallback_dir) {
                warn!("Failed to create ~/tmp directory: {}", e);
                return (
                    "⚠️  Context thinning failed: could not create ~/tmp directory".to_string(),
                    0,
                );
            }
            fallback_dir
        };

        // Scan the first third of messages
        for i in 0..first_third_end {
            // Check if the previous message was a TODO tool call (before getting mutable reference)
            let is_todo_result = if i > 0 {
                if let Some(prev_message) = self.conversation_history.get(i - 1) {
                    if matches!(prev_message.role, MessageRole::Assistant) {
                        prev_message.content.contains(r#""tool":"todo_read""#)
                            || prev_message.content.contains(r#""tool":"todo_write""#)
                            || prev_message.content.contains(r#""tool": "todo_read""#)
                            || prev_message.content.contains(r#""tool": "todo_write""#)
                    } else {
                        false
                    }
                } else {
                    false
                }
            } else {
                false
            };

            if let Some(message) = self.conversation_history.get_mut(i) {
                // Process User messages that look like tool results
                if matches!(message.role, MessageRole::User)
                    && message.content.starts_with("Tool result:")
                {
                    let content_len = message.content.len();

                    // Only thin if the content is greater than 500 chars and not a TODO tool result
                    if !is_todo_result && content_len > 500 {
                        // Generate a unique filename based on timestamp and index
                        let timestamp = std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .unwrap_or_default()
                            .as_secs();
                        let filename = format!("leaned_tool_result_{}_{}.txt", timestamp, i);
                        let file_path = format!("{}/{}", tmp_dir, filename);

                        // Write the content to file
                        if let Err(e) = std::fs::write(&file_path, &message.content) {
                            warn!("Failed to write thinned content to {}: {}", file_path, e);
                            continue;
                        }

                        // Replace the message content with a note
                        let original_len = message.content.len();
                        message.content = format!("Tool result saved to {}", file_path);

                        leaned_count += 1;
                        chars_saved += original_len - message.content.len();

                        debug!(
                            "Thinned tool result {} ({} chars) to {}",
                            i, original_len, file_path
                        );
                    }
                }

                // Process Assistant messages that contain tool calls with large arguments
                if matches!(message.role, MessageRole::Assistant) {
                    // Try to parse the message content as JSON to find tool calls
                    let content = &message.content;

                    // Look for JSON tool call patterns
                    if let Some(tool_call_start) = content
                        .find(r#"{"tool":"#)
                        .or_else(|| content.find(r#"{ "tool":"#))
                        .or_else(|| content.find(r#"{"tool" :"#))
                        .or_else(|| content.find(r#"{ "tool" :"#))
                    {
                        // Try to extract and parse the JSON tool call
                        let json_portion = &content[tool_call_start..];

                        // Find the end of the JSON object
                        if let Some(json_end) = Self::find_json_end(json_portion) {
                            let json_str = &json_portion[..=json_end];

                            // Try to parse as ToolCall
                            if let Ok(mut tool_call) = serde_json::from_str::<ToolCall>(json_str) {
                                let mut modified = false;

                                // Handle write_file tool calls
                                if tool_call.tool == "write_file" {
                                    if let Some(args_obj) = tool_call.args.as_object_mut() {
                                        // Extract content to avoid borrow issues
                                        let content_info = args_obj
                                            .get("content")
                                            .and_then(|v| v.as_str())
                                            .map(|s| (s.to_string(), s.len()));

                                        if let Some((content_str, content_len)) = content_info {
                                            // Only thin if content is greater than 500 chars
                                            if content_len > 500 {
                                                let timestamp = std::time::SystemTime::now()
                                                    .duration_since(std::time::UNIX_EPOCH)
                                                    .unwrap_or_default()
                                                    .as_secs();
                                                let filename = format!(
                                                    "leaned_write_file_content_{}_{}.txt",
                                                    timestamp, i
                                                );
                                                let file_path = format!("{}/{}", tmp_dir, filename);

                                                if std::fs::write(&file_path, &content_str).is_ok()
                                                {
                                                    args_obj.insert(
                                                        "content".to_string(),
                                                        serde_json::Value::String(format!(
                                                            "<content saved to {}>",
                                                            file_path
                                                        )),
                                                    );
                                                    modified = true;
                                                    chars_saved += content_len;
                                                    tool_call_leaned_count += 1;
                                                    debug!("Thinned write_file content {} ({} chars) to {}", i, content_len, file_path);
                                                }
                                            }
                                        }
                                    }
                                }

                                // Handle str_replace tool calls
                                if tool_call.tool == "str_replace" {
                                    if let Some(args_obj) = tool_call.args.as_object_mut() {
                                        // Extract diff to avoid borrow issues
                                        let diff_info = args_obj
                                            .get("diff")
                                            .and_then(|v| v.as_str())
                                            .map(|s| (s.to_string(), s.len()));

                                        if let Some((diff_str, diff_len)) = diff_info {
                                            // Only thin if diff is greater than 500 chars
                                            if diff_len > 500 {
                                                let timestamp = std::time::SystemTime::now()
                                                    .duration_since(std::time::UNIX_EPOCH)
                                                    .unwrap_or_default()
                                                    .as_secs();
                                                let filename = format!(
                                                    "leaned_str_replace_diff_{}_{}.txt",
                                                    timestamp, i
                                                );
                                                let file_path = format!("{}/{}", tmp_dir, filename);

                                                if std::fs::write(&file_path, &diff_str).is_ok() {
                                                    args_obj.insert(
                                                        "diff".to_string(),
                                                        serde_json::Value::String(format!(
                                                            "<diff saved to {}>",
                                                            file_path
                                                        )),
                                                    );
                                                    modified = true;
                                                    chars_saved += diff_len;
                                                    tool_call_leaned_count += 1;
                                                    debug!("Thinned str_replace diff {} ({} chars) to {}", i, diff_len, file_path);
                                                }
                                            }
                                        }
                                    }
                                }

                                // If we modified the tool call, reconstruct the message
                                if modified {
                                    let prefix = &content[..tool_call_start];
                                    let suffix = &content[tool_call_start + json_str.len()..];

                                    // Serialize the modified tool call
                                    if let Ok(new_json) = serde_json::to_string(&tool_call) {
                                        message.content =
                                            format!("{}{}{}", prefix, new_json, suffix);
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }

        // Recalculate token usage after thinning
        self.recalculate_tokens();

        if leaned_count > 0 {
            if tool_call_leaned_count > 0 {
                (format!("🥒 Context thinned at {}%: {} tool results + {} tool calls, ~{} chars saved",
                        current_threshold, leaned_count, tool_call_leaned_count, chars_saved), chars_saved)
            } else {
                (
                    format!(
                        "🥒 Context thinned at {}%: {} tool results, ~{} chars saved",
                        current_threshold, leaned_count, chars_saved
                    ),
                    chars_saved,
                )
            }
        } else if tool_call_leaned_count > 0 {
            (
                format!(
                    "🥒 Context thinned at {}%: {} tool calls, ~{} chars saved",
                    current_threshold, tool_call_leaned_count, chars_saved
                ),
                chars_saved,
            )
        } else {
            (format!("ℹ Context thinning triggered at {}% but no large tool results or tool calls found in first third",
                    current_threshold), 0)
        }
    }

    /// Perform context thinning on the ENTIRE conversation history (not just first third)
    /// This is the "skinnify" variant that processes all messages
    /// Returns a summary message about what was thinned
    /// If session_id is provided, thinned content is saved to .g3/session/<session_id>/thinned/
    pub fn thin_context_all(&mut self, session_id: Option<&str>) -> (String, usize) {
        let current_percentage = self.percentage_used() as u32;

        // Calculate the total messages - process ALL of them
        let total_messages = self.conversation_history.len();

        let mut leaned_count = 0;
        let mut tool_call_leaned_count = 0;
        let mut chars_saved = 0;

        // Determine output directory: use session dir if available, otherwise ~/tmp
        let tmp_dir = if let Some(sid) = session_id {
            let thinned_dir = get_thinned_dir(sid);
            if let Err(e) = std::fs::create_dir_all(&thinned_dir) {
                warn!("Failed to create thinned directory: {}", e);
                return (
                    "⚠️  Context skinnifying failed: could not create thinned directory".to_string(),
                    0,
                );
            }
            thinned_dir.to_string_lossy().to_string()
        } else {
            let fallback_dir = shellexpand::tilde("~/tmp").to_string();
            if let Err(e) = std::fs::create_dir_all(&fallback_dir) {
                warn!("Failed to create ~/tmp directory: {}", e);
                return (
                    "⚠️  Context skinnifying failed: could not create ~/tmp directory".to_string(),
                    0,
                );
            }
            fallback_dir
        };

        // Scan ALL messages (not just first third)
        for i in 0..total_messages {
            // Check if the previous message was a TODO tool call (before getting mutable reference)
            let is_todo_result = if i > 0 {
                if let Some(prev_message) = self.conversation_history.get(i - 1) {
                    if matches!(prev_message.role, MessageRole::Assistant) {
                        prev_message.content.contains(r#""tool":"todo_read""#)
                            || prev_message.content.contains(r#""tool":"todo_write""#)
                            || prev_message.content.contains(r#""tool": "todo_read""#)
                            || prev_message.content.contains(r#""tool": "todo_write""#)
                    } else {
                        false
                    }
                } else {
                    false
                }
            } else {
                false
            };

            if let Some(message) = self.conversation_history.get_mut(i) {
                // Process User messages that look like tool results
                if matches!(message.role, MessageRole::User)
                    && message.content.starts_with("Tool result:")
                {
                    let content_len = message.content.len();

                    // Only thin if the content is greater than 500 chars and not a TODO tool result
                    if !is_todo_result && content_len > 500 {
                        // Generate a unique filename based on timestamp and index
                        let timestamp = std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .unwrap_or_default()
                            .as_secs();
                        let filename = format!("skinny_tool_result_{}_{}.txt", timestamp, i);
                        let file_path = format!("{}/{}", tmp_dir, filename);

                        // Write the content to file
                        if let Err(e) = std::fs::write(&file_path, &message.content) {
                            warn!("Failed to write skinnified content to {}: {}", file_path, e);
                            continue;
                        }

                        // Replace the message content with a note
                        let original_len = message.content.len();
                        message.content = format!("Tool result saved to {}", file_path);

                        leaned_count += 1;
                        chars_saved += original_len - message.content.len();

                        debug!(
                            "Skinnified tool result {} ({} chars) to {}",
                            i, original_len, file_path
                        );
                    }
                }

                // Process Assistant messages that contain tool calls with large arguments
                if matches!(message.role, MessageRole::Assistant) {
                    // Try to parse the message content as JSON to find tool calls
                    let content = &message.content;

                    // Look for JSON tool call patterns
                    if let Some(tool_call_start) = content
                        .find(r#"{"tool":"#)
                        .or_else(|| content.find(r#"{ "tool":"#))
                        .or_else(|| content.find(r#"{"tool" :"#))
                        .or_else(|| content.find(r#"{ "tool" :"#))
                    {
                        // Try to extract and parse the JSON tool call
                        let json_portion = &content[tool_call_start..];

                        // Find the end of the JSON object
                        if let Some(json_end) = Self::find_json_end(json_portion) {
                            let json_str = &json_portion[..=json_end];

                            // Try to parse as ToolCall
                            if let Ok(mut tool_call) = serde_json::from_str::<ToolCall>(json_str) {
                                let mut modified = false;

                                // Handle write_file tool calls
                                if tool_call.tool == "write_file" {
                                    if let Some(args_obj) = tool_call.args.as_object_mut() {
                                        let content_info = args_obj
                                            .get("content")
                                            .and_then(|v| v.as_str())
                                            .map(|s| (s.to_string(), s.len()));

                                        if let Some((content_str, content_len)) = content_info {
                                            if content_len > 500 {
                                                let timestamp = std::time::SystemTime::now()
                                                    .duration_since(std::time::UNIX_EPOCH)
                                                    .unwrap_or_default()
                                                    .as_secs();
                                                let filename = format!(
                                                    "skinny_write_file_content_{}_{}.txt",
                                                    timestamp, i
                                                );
                                                let file_path = format!("{}/{}", tmp_dir, filename);

                                                if std::fs::write(&file_path, &content_str).is_ok() {
                                                    args_obj.insert(
                                                        "content".to_string(),
                                                        serde_json::Value::String(format!(
                                                            "<content saved to {}>",
                                                            file_path
                                                        )),
                                                    );
                                                    modified = true;
                                                    chars_saved += content_len;
                                                    tool_call_leaned_count += 1;
                                                    debug!("Skinnified write_file content {} ({} chars) to {}", i, content_len, file_path);
                                                }
                                            }
                                        }
                                    }
                                }

                                // Handle str_replace tool calls
                                if tool_call.tool == "str_replace" {
                                    if let Some(args_obj) = tool_call.args.as_object_mut() {
                                        let diff_info = args_obj
                                            .get("diff")
                                            .and_then(|v| v.as_str())
                                            .map(|s| (s.to_string(), s.len()));

                                        if let Some((diff_str, diff_len)) = diff_info {
                                            if diff_len > 500 {
                                                let timestamp = std::time::SystemTime::now()
                                                    .duration_since(std::time::UNIX_EPOCH)
                                                    .unwrap_or_default()
                                                    .as_secs();
                                                let filename = format!(
                                                    "skinny_str_replace_diff_{}_{}.txt",
                                                    timestamp, i
                                                );
                                                let file_path = format!("{}/{}", tmp_dir, filename);

                                                if std::fs::write(&file_path, &diff_str).is_ok() {
                                                    args_obj.insert(
                                                        "diff".to_string(),
                                                        serde_json::Value::String(format!(
                                                            "<diff saved to {}>",
                                                            file_path
                                                        )),
                                                    );
                                                    modified = true;
                                                    chars_saved += diff_len;
                                                    tool_call_leaned_count += 1;
                                                    debug!("Skinnified str_replace diff {} ({} chars) to {}", i, diff_len, file_path);
                                                }
                                            }
                                        }
                                    }
                                }

                                // If we modified the tool call, reconstruct the message
                                if modified {
                                    let prefix = &content[..tool_call_start];
                                    let suffix = &content[tool_call_start + json_str.len()..];

                                    // Serialize the modified tool call
                                    if let Ok(new_json) = serde_json::to_string(&tool_call) {
                                        message.content =
                                            format!("{}{}{}", prefix, new_json, suffix);
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }

        // Recalculate token usage after thinning
        self.recalculate_tokens();

        if leaned_count > 0 {
            if tool_call_leaned_count > 0 {
                (format!("🦴 Context skinnified at {}%: {} tool results + {} tool calls across entire history, ~{} chars saved",
                        current_percentage, leaned_count, tool_call_leaned_count, chars_saved), chars_saved)
            } else {
                (
                    format!(
                        "🦴 Context skinnified at {}%: {} tool results across entire history, ~{} chars saved",
                        current_percentage, leaned_count, chars_saved
                    ),
                    chars_saved,
                )
            }
        } else if tool_call_leaned_count > 0 {
            (
                format!(
                    "🦴 Context skinnified at {}%: {} tool calls across entire history, ~{} chars saved",
                    current_percentage, tool_call_leaned_count, chars_saved
                ),
                chars_saved,
            )
        } else {
            (format!("ℹ Context skinnifying triggered at {}% but no large tool results or tool calls found in entire history",
                    current_percentage), 0)
        }
    }

    /// Recalculate token usage based on current conversation history
    fn recalculate_tokens(&mut self) {
        let mut total = 0;
        for message in &self.conversation_history {
            total += Self::estimate_tokens(&message.content);
        }
        self.used_tokens = total;

        debug!("Recalculated tokens after thinning: {} tokens", total);
    }

    /// Helper function to find the end of a JSON object
    fn find_json_end(json_str: &str) -> Option<usize> {
        let mut brace_count = 0;
        let mut in_string = false;
        let mut escape_next = false;

        for (i, ch) in json_str.char_indices() {
            if escape_next {
                escape_next = false;
                continue;
            }

            match ch {
                '\\' => escape_next = true,
                '"' if !escape_next => in_string = !in_string,
                '{' if !in_string => brace_count += 1,
                '}' if !in_string => {
                    brace_count -= 1;
                    if brace_count == 0 {
                        return Some(i);
                    }
                }
                _ => {}
            }
        }

        None
    }
}

pub struct Agent<W: UiWriter> {
    providers: ProviderRegistry,
    context_window: ContextWindow,
    thinning_events: Vec<usize>,      // chars saved per thinning event
    pending_90_summarization: bool,   // flag to trigger summarization at 90%
    auto_compact: bool,               // whether to auto-compact at 90% before tool calls
    summarization_events: Vec<usize>, // chars saved per summarization event
    first_token_times: Vec<Duration>, // time to first token for each completion
    config: Config,
    session_id: Option<String>,
    tool_call_metrics: Vec<(String, Duration, bool)>, // (tool_name, duration, success)
    ui_writer: W,
    is_autonomous: bool,
    quiet: bool,
    computer_controller: Option<Box<dyn g3_computer_control::ComputerController>>,
    todo_content: std::sync::Arc<tokio::sync::RwLock<String>>,
    webdriver_session: std::sync::Arc<
        tokio::sync::RwLock<
            Option<std::sync::Arc<tokio::sync::Mutex<WebDriverSession>>>,
        >,
    >,
    webdriver_process: std::sync::Arc<tokio::sync::RwLock<Option<tokio::process::Child>>>,
    macax_controller:
        std::sync::Arc<tokio::sync::RwLock<Option<g3_computer_control::MacAxController>>>,
    tool_call_count: usize,
    requirements_sha: Option<String>,
    /// Working directory for tool execution (set by --codebase-fast-start)
    working_dir: Option<String>,
    background_process_manager: std::sync::Arc<background_process::BackgroundProcessManager>,
    /// Pending images to attach to the next user message
    pending_images: Vec<g3_providers::ImageContent>,
}

impl<W: UiWriter> Agent<W> {
    pub async fn new(config: Config, ui_writer: W) -> Result<Self> {
        Self::new_with_mode(config, ui_writer, false, false).await
    }

    pub async fn new_with_readme(
        config: Config,
        ui_writer: W,
        readme_content: Option<String>,
    ) -> Result<Self> {
        Self::new_with_mode_and_readme(config, ui_writer, false, readme_content, false, None).await
    }

    pub async fn new_autonomous_with_readme(
        config: Config,
        ui_writer: W,
        readme_content: Option<String>,
    ) -> Result<Self> {
        Self::new_with_mode_and_readme(config, ui_writer, true, readme_content, false, None).await
    }

    pub async fn new_autonomous(config: Config, ui_writer: W) -> Result<Self> {
        Self::new_with_mode(config, ui_writer, true, false).await
    }

    pub async fn new_with_quiet(config: Config, ui_writer: W, quiet: bool) -> Result<Self> {
        Self::new_with_mode(config, ui_writer, false, quiet).await
    }

    pub async fn new_with_readme_and_quiet(
        config: Config,
        ui_writer: W,
        readme_content: Option<String>,
        quiet: bool,
    ) -> Result<Self> {
        Self::new_with_mode_and_readme(config, ui_writer, false, readme_content, quiet, None).await
    }

    pub async fn new_autonomous_with_readme_and_quiet(
        config: Config,
        ui_writer: W,
        readme_content: Option<String>,
        quiet: bool,
    ) -> Result<Self> {
        Self::new_with_mode_and_readme(config, ui_writer, true, readme_content, quiet, None).await
    }

    /// Create a new agent with a custom system prompt (for agent mode)
    /// The custom_system_prompt replaces the default G3 system prompt entirely
    pub async fn new_with_custom_prompt(
        config: Config,
        ui_writer: W,
        custom_system_prompt: String,
        readme_content: Option<String>,
    ) -> Result<Self> {
        Self::new_with_mode_and_readme(config, ui_writer, false, readme_content, false, Some(custom_system_prompt)).await
    }

    async fn new_with_mode(
        config: Config,
        ui_writer: W,
        is_autonomous: bool,
        quiet: bool,
    ) -> Result<Self> {
        Self::new_with_mode_and_readme(config, ui_writer, is_autonomous, None, quiet, None).await
    }

    async fn new_with_mode_and_readme(
        config: Config,
        ui_writer: W,
        is_autonomous: bool,
        readme_content: Option<String>,
        quiet: bool,
        custom_system_prompt: Option<String>,
    ) -> Result<Self> {
        let mut providers = ProviderRegistry::new();

        // In autonomous mode, we need to register both coach and player providers
        // Otherwise, only register the default provider
        let providers_to_register: Vec<String> = if is_autonomous {
            let mut providers = vec![config.providers.default_provider.clone()];
            if let Some(coach) = &config.providers.coach {
                if !providers.contains(coach) {
                    providers.push(coach.clone());
                }
            }
            if let Some(player) = &config.providers.player {
                if !providers.contains(player) {
                    providers.push(player.clone());
                }
            }
            providers
        } else {
            vec![config.providers.default_provider.clone()]
        };

        // Only register providers that are configured AND selected
        // This prevents unnecessary initialization of heavy providers like embedded models

        // Helper to check if a provider ref should be registered
        let should_register = |provider_type: &str, config_name: &str| -> bool {
            let full_ref = format!("{}.{}", provider_type, config_name);
            providers_to_register.iter().any(|p| p == &full_ref || p.starts_with(&format!("{}.", provider_type)))
        };

        // Register embedded providers from HashMap
        for (name, embedded_config) in &config.providers.embedded {
            if should_register("embedded", name) {
                let embedded_provider = g3_providers::EmbeddedProvider::new(
                    embedded_config.model_path.clone(),
                    embedded_config.model_type.clone(),
                    embedded_config.context_length,
                    embedded_config.max_tokens,
                    embedded_config.temperature,
                    embedded_config.gpu_layers,
                    embedded_config.threads,
                )?;
                providers.register(embedded_provider);
            }
        }

        // Register OpenAI providers from HashMap
        for (name, openai_config) in &config.providers.openai {
            if should_register("openai", name) {
                let openai_provider = g3_providers::OpenAIProvider::new_with_name(
                    format!("openai.{}", name),
                    openai_config.api_key.clone(),
                    Some(openai_config.model.clone()),
                    openai_config.base_url.clone(),
                    openai_config.max_tokens,
                    openai_config.temperature,
                )?;
                providers.register(openai_provider);
            }
        }

        // Register OpenAI-compatible providers (e.g., OpenRouter, Groq, etc.)
        for (name, openai_config) in &config.providers.openai_compatible {
            if should_register(name, "default") {
                let openai_provider = g3_providers::OpenAIProvider::new_with_name(
                    name.clone(),
                    openai_config.api_key.clone(),
                    Some(openai_config.model.clone()),
                    openai_config.base_url.clone(),
                    openai_config.max_tokens,
                    openai_config.temperature,
                )?;
                providers.register(openai_provider);
            }
        }

        // Register Anthropic providers from HashMap
        for (name, anthropic_config) in &config.providers.anthropic {
            if should_register("anthropic", name) {
                let anthropic_provider = g3_providers::AnthropicProvider::new_with_name(
                    format!("anthropic.{}", name),
                    anthropic_config.api_key.clone(),
                    Some(anthropic_config.model.clone()),
                    anthropic_config.max_tokens,
                    anthropic_config.temperature,
                    anthropic_config.cache_config.clone(),
                    anthropic_config.enable_1m_context,
                    anthropic_config.thinking_budget_tokens,
                )?;
                providers.register(anthropic_provider);
            }
        }

        // Register Databricks providers from HashMap
        for (name, databricks_config) in &config.providers.databricks {
            if should_register("databricks", name) {
                let databricks_provider = if let Some(token) = &databricks_config.token {
                    // Use token-based authentication
                    g3_providers::DatabricksProvider::from_token_with_name(
                        format!("databricks.{}", name),
                        databricks_config.host.clone(),
                        token.clone(),
                        databricks_config.model.clone(),
                        databricks_config.max_tokens,
                        databricks_config.temperature,
                    )?
                } else {
                    // Use OAuth authentication
                    g3_providers::DatabricksProvider::from_oauth_with_name(
                        format!("databricks.{}", name),
                        databricks_config.host.clone(),
                        databricks_config.model.clone(),
                        databricks_config.max_tokens,
                        databricks_config.temperature,
                    )
                    .await?
                };

                providers.register(databricks_provider);
            }
        }

        // Register Azure providers from HashMap
        for (name, azure_config) in &config.providers.azure {
            if should_register("azure", name) {
                let azure_provider = g3_providers::AzureProvider::new_with_name(
                    format!("azure.{}", name),
                    azure_config.endpoint.clone(),
                    azure_config.api_key.clone(),
                    azure_config.deployment.clone(),
                    Some(azure_config.api_version.clone()),
                    azure_config.max_tokens,
                    azure_config.temperature,
                )?;
                providers.register(azure_provider);
            }
        }

        // Set default provider
        debug!(
            "Setting default provider to: {}",
            config.providers.default_provider
        );
        providers.set_default(&config.providers.default_provider)?;
        debug!("Default provider set successfully");

        // Determine context window size based on active provider
        let mut context_warnings = Vec::new();
        let context_length =
            Self::get_configured_context_length(&config, &providers, &mut context_warnings)?;
        let mut context_window = ContextWindow::new(context_length);

        // Surface any context warnings to the user via UI
        for warning in context_warnings {
            ui_writer.print_context_status(&format!("⚠️ {}", warning));
        }

        // Add system prompt as the FIRST message (before README)
        // This ensures the agent always has proper tool usage instructions
        let provider = providers.get(None)?;
        let provider_has_native_tool_calling = provider.has_native_tool_calling();
        let _ = provider; // Drop provider reference to avoid borrowing issues

        let system_prompt = if let Some(custom_prompt) = custom_system_prompt {
            // Use custom system prompt (for agent mode)
            custom_prompt
        } else {
            // Use default system prompt based on provider capabilities
            if provider_has_native_tool_calling {
                // For native tool calling providers, use a more explicit system prompt
                get_system_prompt_for_native(config.agent.allow_multiple_tool_calls)
            } else {
                // For non-native providers (embedded models), use JSON format instructions
                SYSTEM_PROMPT_FOR_NON_NATIVE_TOOL_USE.to_string()
            }
        };

        let system_message = Message::new(MessageRole::System, system_prompt);
        context_window.add_message(system_message);

        // If README content is provided, add it as a second system message (after the main system prompt)
        if let Some(readme) = readme_content {
            let readme_message = Message::new(MessageRole::System, readme);
            context_window.add_message(readme_message);
        }

        // NOTE: TODO lists are now session-scoped and stored in .g3/sessions/<session_id>/todo.g3.md
        // We don't load any TODO at initialization since we don't have a session_id yet.
        // The agent will use todo_read to load the TODO once a session is established.

        // Initialize computer controller if enabled
        let computer_controller = if config.computer_control.enabled {
            match g3_computer_control::create_controller() {
                Ok(controller) => Some(controller),
                Err(e) => {
                    warn!("Failed to initialize computer control: {}", e);
                    None
                }
            }
        } else {
            None
        };

        // Capture macax_enabled before moving config
        let macax_enabled = config.macax.enabled;

        Ok(Self {
            providers,
            context_window,
            auto_compact: config.agent.auto_compact,
            pending_90_summarization: false,
            thinning_events: Vec::new(),
            summarization_events: Vec::new(),
            first_token_times: Vec::new(),
            config,
            session_id: None,
            tool_call_metrics: Vec::new(),
            ui_writer,
            // TODO content starts empty - session-scoped TODOs are loaded via todo_read
            todo_content: std::sync::Arc::new(tokio::sync::RwLock::new(String::new())),
            is_autonomous,
            quiet,
            computer_controller,
            webdriver_session: std::sync::Arc::new(tokio::sync::RwLock::new(None)),
            webdriver_process: std::sync::Arc::new(tokio::sync::RwLock::new(None)),
            macax_controller: {
                std::sync::Arc::new(tokio::sync::RwLock::new(if macax_enabled {
                    Some(g3_computer_control::MacAxController::new()?)
                } else {
                    None
                }))
            },
            tool_call_count: 0,
            requirements_sha: None,
            working_dir: None,
            background_process_manager: std::sync::Arc::new(
                background_process::BackgroundProcessManager::new(
                    paths::get_logs_dir().join("background_processes")
                )),
            pending_images: Vec::new(),
        })
    }

    /// Validate that the system prompt is the first message in the conversation history.
    /// This is a critical invariant that must be maintained for proper agent operation.
    ///
    /// # Panics
    /// Panics if:
    /// - The conversation history is empty
    /// - The first message is not a System message
    /// - The first message doesn't contain the system prompt markers
    fn validate_system_prompt_is_first(&self) {
        if self.context_window.conversation_history.is_empty() {
            panic!(
                "FATAL: Conversation history is empty. System prompt must be the first message."
            );
        }

        let first_message = &self.context_window.conversation_history[0];

        if !matches!(first_message.role, MessageRole::System) {
            panic!(
                "FATAL: First message is not a System message. Found: {:?}",
                first_message.role
            );
        }

        // Check for system prompt markers that are present in both standard and agent mode
        // Agent mode replaces the identity line but keeps all other instructions
        let has_tool_instructions = first_message.content.contains("IMPORTANT: You must call tools to achieve goals");
        if !has_tool_instructions {
            panic!("FATAL: First system message does not contain the system prompt. This likely means the README was added before the system prompt.");
        }
    }

    /// Convert cache config string to CacheControl enum
    fn parse_cache_control(cache_config: &str) -> Option<CacheControl> {
        match cache_config {
            "ephemeral" => Some(CacheControl::ephemeral()),
            "5minute" => Some(CacheControl::five_minute()),
            "1hour" => Some(CacheControl::one_hour()),
            _ => {
                warn!(
                    "Invalid cache_config value: '{}'. Valid values are: ephemeral, 5minute, 1hour",
                    cache_config
                );
                None
            }
        }
    }

    /// Count how many cache_control annotations exist in the conversation history
    fn count_cache_controls_in_history(&self) -> usize {
        self.context_window
            .conversation_history
            .iter()
            .filter(|msg| msg.cache_control.is_some())
            .count()
    }

    /// Get the configured max_tokens for a provider from top-level config
    fn provider_max_tokens(config: &Config, provider_name: &str) -> Option<u32> {
        // Parse provider reference (format: "provider_type.config_name")
        let parts: Vec<&str> = provider_name.split('.').collect();
        let (provider_type, config_name) = if parts.len() == 2 {
            (parts[0], parts[1])
        } else {
            // Fallback for simple provider names - assume "default" config
            (provider_name, "default")
        };
        
        match provider_type {
            "anthropic" => config.providers.anthropic.get(config_name)?.max_tokens,
            "openai" => config.providers.openai.get(config_name)?.max_tokens,
            "databricks" => config.providers.databricks.get(config_name)?.max_tokens,
            "embedded" => config.providers.embedded.get(config_name)?.max_tokens,
            _ => None,
        }
    }

    /// Get the configured temperature for a provider from top-level config
    fn provider_temperature(config: &Config, provider_name: &str) -> Option<f32> {
        // Parse provider reference (format: "provider_type.config_name")
        let parts: Vec<&str> = provider_name.split('.').collect();
        let (provider_type, config_name) = if parts.len() == 2 {
            (parts[0], parts[1])
        } else {
            // Fallback for simple provider names - assume "default" config
            (provider_name, "default")
        };
        
        match provider_type {
            "anthropic" => config.providers.anthropic.get(config_name)?.temperature,
            "openai" => config.providers.openai.get(config_name)?.temperature,
            "databricks" => config.providers.databricks.get(config_name)?.temperature,
            "embedded" => config.providers.embedded.get(config_name)?.temperature,
            _ => None,
        }
    }

    /// Resolve the max_tokens to use for a given provider, applying fallbacks
    fn resolve_max_tokens(&self, provider_name: &str) -> u32 {
        let base = match provider_name {
            "databricks" => Self::provider_max_tokens(&self.config, "databricks")
                .or(Some(self.config.agent.fallback_default_max_tokens as u32))
                .unwrap_or(32000),
            other => Self::provider_max_tokens(&self.config, other)
                .or(Some(self.config.agent.fallback_default_max_tokens as u32))
                .unwrap_or(16000),
        };
        
        // For Anthropic with thinking enabled, ensure max_tokens is sufficient
        // Anthropic requires: max_tokens > thinking.budget_tokens
        if provider_name == "anthropic" {
            if let Some(budget) = self.get_thinking_budget_tokens(provider_name) {
                let minimum_for_thinking = budget + 1024;
                return base.max(minimum_for_thinking);
            }
        }
        
        base
    }

    /// Get the thinking budget tokens for Anthropic provider, if configured
    fn get_thinking_budget_tokens(&self, provider_name: &str) -> Option<u32> {
        // Parse provider reference (format: "provider_type.config_name")
        let parts: Vec<&str> = provider_name.split('.').collect();
        let (provider_type, config_name) = if parts.len() == 2 {
            (parts[0], parts[1])
        } else {
            // Fallback for simple provider names - assume "default" config
            (provider_name, "default")
        };
        
        // Only Anthropic has thinking_budget_tokens
        if provider_type != "anthropic" {
            return None;
        }
        
        self.config.providers.anthropic
            .get(config_name)
            .and_then(|c| c.thinking_budget_tokens)
    }

    /// Pre-flight check to validate and adjust max_tokens for the thinking.budget_tokens constraint.
    /// Returns the adjusted max_tokens that satisfies: max_tokens > thinking.budget_tokens
    /// Also returns whether we need to apply fallback actions (thinnify/skinnify).
    ///
    /// Returns: (adjusted_max_tokens, needs_context_reduction)
    fn preflight_validate_max_tokens(
        &self,
        provider_name: &str,
        proposed_max_tokens: u32,
    ) -> (u32, bool) {
        // Parse provider type from provider_name (format: "provider_type.config_name")
        let provider_type = provider_name.split('.').next().unwrap_or(provider_name);
        
        // Only applies to Anthropic provider
        if provider_type != "anthropic" {
            return (proposed_max_tokens, false);
        }

        let budget_tokens = match self.get_thinking_budget_tokens(provider_name) {
            Some(budget) => budget,
            None => return (proposed_max_tokens, false), // No thinking enabled
        };

        // Anthropic requires: max_tokens > budget_tokens
        // We add a minimum output buffer of 1024 tokens for actual response content
        let minimum_required = budget_tokens + 1024;

        if proposed_max_tokens >= minimum_required {
            // We have enough headroom
            (proposed_max_tokens, false)
        } else {
            // max_tokens is too low - need to either adjust or reduce context
            warn!(
                "max_tokens ({}) is below required minimum ({}) for thinking.budget_tokens ({}). Context reduction needed.",
                proposed_max_tokens, minimum_required, budget_tokens
            );
            // Return the minimum required, but flag that we need context reduction
            (minimum_required, true)
        }
    }

    /// Calculate max_tokens for a summary request, ensuring it satisfies the thinking constraint.
    /// Applies fallback sequence: thinnify -> skinnify -> hard-coded minimum
    /// Returns (max_tokens, whether_fallback_was_used)
    fn calculate_summary_max_tokens(
        &mut self,
        provider_name: &str,
    ) -> (u32, bool) {
        let model_limit = self.context_window.total_tokens;
        let current_usage = self.context_window.used_tokens;
        
        // Get the configured max_tokens for this provider
        let configured_max_tokens = self.resolve_max_tokens(provider_name);
        
        // Calculate available tokens with buffer
        let buffer = (model_limit / 40).clamp(1000, 10000); // 2.5% buffer
        let available = model_limit
            .saturating_sub(current_usage)
            .saturating_sub(buffer);
        // Use the smaller of available tokens or configured max_tokens,
        // but ensure we don't go below thinking budget floor for Anthropic
        let proposed_max_tokens = available.min(configured_max_tokens);
        let proposed_max_tokens = if provider_name == "anthropic" {
            if let Some(budget) = self.get_thinking_budget_tokens(provider_name) {
                proposed_max_tokens.max(budget + 1024)
            } else {
                proposed_max_tokens
            }
        } else {
            proposed_max_tokens
        };

        // Validate against thinking budget constraint
        let (adjusted, needs_reduction) = self.preflight_validate_max_tokens(provider_name, proposed_max_tokens);
        
        if !needs_reduction {
            return (adjusted, false);
        }

        // We need more headroom - the context is too full
        // Return the adjusted value but flag that fallbacks are needed
        (adjusted, true)
    }

    /// Apply the fallback sequence to free up context space for thinking budget.
    /// Sequence: thinnify (first third) → skinnify (all) → hard-coded minimum
    /// Returns the validated max_tokens that satisfies thinking.budget_tokens constraint.
    fn apply_max_tokens_fallback_sequence(
        &mut self,
        provider_name: &str,
        initial_max_tokens: u32,
        hard_coded_minimum: u32,
    ) -> u32 {
        let (mut max_tokens, needs_reduction) = self.preflight_validate_max_tokens(provider_name, initial_max_tokens);
        
        if !needs_reduction {
            return max_tokens;
        }

        self.ui_writer.print_context_status(
            "⚠️ Context window too full for thinking budget. Applying fallback sequence...\n",
        );

        // Step 1: Try thinnify (first third of context)
        self.ui_writer.print_context_status("🥒 Step 1: Trying thinnify...\n");
        let (thin_msg, thin_saved) = self.context_window.thin_context(self.session_id.as_deref());
        self.thinning_events.push(thin_saved);
        self.ui_writer.print_context_thinning(&thin_msg);

        // Recalculate max_tokens after thinnify
        let recalc_max = self.resolve_max_tokens(provider_name);
        let (new_max, still_needs_reduction) = self.preflight_validate_max_tokens(provider_name, recalc_max);
        max_tokens = new_max;

        if !still_needs_reduction {
            self.ui_writer.print_context_status(
                "✅ Thinnify resolved capacity issue. Continuing...\n",
            );
            return max_tokens;
        }

        // Step 2: Try skinnify (entire context)
        self.ui_writer.print_context_status("🦴 Step 2: Trying skinnify...\n");
        let (skinny_msg, skinny_saved) = self.context_window.thin_context_all(self.session_id.as_deref());
        self.thinning_events.push(skinny_saved);
        self.ui_writer.print_context_thinning(&skinny_msg);

        // Recalculate max_tokens after skinnify
        let recalc_max = self.resolve_max_tokens(provider_name);
        let (final_max, final_needs_reduction) = self.preflight_validate_max_tokens(provider_name, recalc_max);
        max_tokens = final_max;

        if !final_needs_reduction {
            self.ui_writer.print_context_status(
                "✅ Skinnify resolved capacity issue. Continuing...\n",
            );
            return max_tokens;
        }

        // Step 3: Nothing worked, use hard-coded minimum as last resort
        self.ui_writer.print_context_status(&format!(
            "⚠️ Step 3: Context reduction insufficient. Using hard-coded max_tokens={} as last resort...\n",
            hard_coded_minimum
        ));
        
        hard_coded_minimum
    }

    /// Apply the fallback sequence for summary requests to free up context space.
    /// Uses calculate_summary_max_tokens for recalculation (based on available space).
    /// Returns the validated max_tokens for summary requests.
    fn apply_summary_fallback_sequence(
        &mut self,
        provider_name: &str,
    ) -> u32 {
        let (mut summary_max_tokens, needs_reduction) = self.calculate_summary_max_tokens(provider_name);
        
        if !needs_reduction {
            return summary_max_tokens;
        }

        self.ui_writer.print_context_status(
            "⚠️ Context window too full for thinking budget. Applying fallback sequence...\n",
        );

        // Step 1: Try thinnify (first third of context)
        self.ui_writer.print_context_status("🥒 Step 1: Trying thinnify...\n");
        let (thin_msg, thin_saved) = self.context_window.thin_context(self.session_id.as_deref());
        self.thinning_events.push(thin_saved);
        self.ui_writer.print_context_thinning(&thin_msg);

        // Recalculate max_tokens after thinnify
        let (new_max, still_needs_reduction) = self.calculate_summary_max_tokens(provider_name);
        summary_max_tokens = new_max;

        if !still_needs_reduction {
            self.ui_writer.print_context_status(
                "✅ Thinnify resolved capacity issue. Continuing...\n",
            );
            return summary_max_tokens;
        }

        // Step 2: Try skinnify (entire context)
        self.ui_writer.print_context_status("🦴 Step 2: Trying skinnify...\n");
        let (skinny_msg, skinny_saved) = self.context_window.thin_context_all(self.session_id.as_deref());
        self.thinning_events.push(skinny_saved);
        self.ui_writer.print_context_thinning(&skinny_msg);

        // Recalculate max_tokens after skinnify
        let (final_max, final_needs_reduction) = self.calculate_summary_max_tokens(provider_name);
        summary_max_tokens = final_max;

        if !final_needs_reduction {
            self.ui_writer.print_context_status(
                "✅ Skinnify resolved capacity issue. Continuing...\n",
            );
            return summary_max_tokens;
        }

        // Step 3: Nothing worked, use hard-coded minimum
        self.ui_writer.print_context_status(
            "⚠️ Step 3: Context reduction insufficient. Using hard-coded max_tokens=5000 as last resort...\n",
        );
        5000
    }

    /// Resolve the temperature to use for a given provider, applying fallbacks
    fn resolve_temperature(&self, provider_name: &str) -> f32 {
        match provider_name {
            "databricks" => Self::provider_temperature(&self.config, "databricks")
                .unwrap_or(0.1),
            other => Self::provider_temperature(&self.config, other)
                .unwrap_or(0.1),
        }
    }

    /// Print provider diagnostics through the UiWriter for visibility
    pub fn print_provider_banner(&self, role_label: &str) {
        if let Ok((provider_name, model)) = self.get_provider_info() {
            let max_tokens = self.resolve_max_tokens(&provider_name);
            let context_len = self.context_window.total_tokens;

            let mut details = vec![
                format!("provider={}", provider_name),
                format!("model={}", model),
                format!("max_tokens={}", max_tokens),
                format!("context_window_length={}", context_len),
            ];

            if let Ok(provider) = self.providers.get(None) {
                details.push(format!(
                    "native_tools={}",
                    if provider.has_native_tool_calling() {
                        "yes"
                    } else {
                        "no"
                    }
                ));
                if provider.supports_cache_control() {
                    details.push("cache_control=yes".to_string());
                }
            }

            self.ui_writer
                .print_context_status(&format!("{}: {}", role_label, details.join(", ")));
        }
    }

    fn get_configured_context_length(
        config: &Config,
        providers: &ProviderRegistry,
        warnings: &mut Vec<String>,
    ) -> Result<u32> {
        // First, check if there's a global max_context_length override in agent config
        if let Some(max_context_length) = config.agent.max_context_length {
            debug!(
                "Using configured agent.max_context_length: {}",
                max_context_length
            );
            return Ok(max_context_length);
        }

        // Get the active provider to determine context length
        let provider = providers.get(None)?;
        let provider_name = provider.name();
        let model_name = provider.model();

        // Parse provider name to get type and config name
        let parts: Vec<&str> = provider_name.split('.').collect();
        let (provider_type, config_name) = if parts.len() == 2 {
            (parts[0], parts[1])
        } else {
            // Fallback for simple provider names
            (provider_name, "default")
        };

        // Use provider-specific context length if available
        let context_length = match provider_type {
            "embedded" | "embedded." => {
                // For embedded models, use the configured context_length or model-specific defaults
                if let Some(embedded_config) = config.providers.embedded.get(config_name) {
                    embedded_config.context_length.unwrap_or_else(|| {
                        // Model-specific defaults for embedded models
                        match &embedded_config.model_type.to_lowercase()[..] {
                            "codellama" => 16384, // CodeLlama supports 16k context
                            "llama" => 4096,      // Base Llama models
                            "mistral" => 8192,    // Mistral models
                            "qwen" => 32768,      // Qwen2.5 supports 32k context
                            _ => 4096,            // Conservative default
                        }
                    })
                } else {
                    config.agent.fallback_default_max_tokens as u32
                }
            }
            "openai" => {
                // OpenAI models have varying context windows
                if let Some(max_tokens) = Self::provider_max_tokens(config, provider_name) {
                    warnings.push(format!(
                        "Context length falling back to max_tokens ({}) for provider={}",
                        max_tokens, provider_name
                    ));
                    max_tokens
                } else {
                    400000
                }
            }
            "anthropic" => {
                // Claude models have large context windows
                if let Some(max_tokens) = Self::provider_max_tokens(config, provider_name) {
                    warnings.push(format!(
                        "Context length falling back to max_tokens ({}) for provider={}",
                        max_tokens, provider_name
                    ));
                    max_tokens
                } else {
                    200000
                }
            }
            "databricks" => {
                // Databricks models have varying context windows depending on the model
                if let Some(max_tokens) = Self::provider_max_tokens(config, provider_name) {
                    warnings.push(format!(
                        "Context length falling back to max_tokens ({}) for provider={}",
                        max_tokens, provider_name
                    ));
                    max_tokens
                } else if model_name.contains("claude") {
                    200000 // Claude models on Databricks have large context windows
                } else if model_name.contains("llama") || model_name.contains("dbrx") {
                    32768 // DBRX supports 32k context
                } else {
                    16384 // Conservative default for other Databricks models
                }
            }
            "azure" => {
                // Azure AI Claude models have large context windows (200k)
                200000
            }
            _ => config.agent.fallback_default_max_tokens as u32,
        };

        debug!(
            "Using context length: {} tokens for provider: {} (model: {})",
            context_length, provider_name, model_name
        );

        Ok(context_length)
    }

    pub fn get_provider_info(&self) -> Result<(String, String)> {
        let provider = self.providers.get(None)?;
        Ok((provider.name().to_string(), provider.model().to_string()))
    }

    /// Get the default LLM provider
    pub fn get_provider(&self) -> Result<&dyn g3_providers::LLMProvider> {
        self.providers.get(None)
    }

    /// Get the current session ID for this agent
    pub fn get_session_id(&self) -> Option<&str> {
        self.session_id.as_deref()
    }

    pub async fn execute_task(
        &mut self,
        description: &str,
        language: Option<&str>,
        _auto_execute: bool,
    ) -> Result<TaskResult> {
        self.execute_task_with_options(description, language, false, false, false, None)
            .await
    }

    pub async fn execute_task_with_options(
        &mut self,
        description: &str,
        language: Option<&str>,
        _auto_execute: bool,
        show_prompt: bool,
        show_code: bool,
        discovery_options: Option<DiscoveryOptions<'_>>,
    ) -> Result<TaskResult> {
        self.execute_task_with_timing(
            description,
            language,
            _auto_execute,
            show_prompt,
            show_code,
            false,
            discovery_options,
        )
        .await
    }

    pub async fn execute_task_with_timing(
        &mut self,
        description: &str,
        language: Option<&str>,
        _auto_execute: bool,
        show_prompt: bool,
        show_code: bool,
        show_timing: bool,
        discovery_options: Option<DiscoveryOptions<'_>>,
    ) -> Result<TaskResult> {
        // Create a cancellation token that never cancels for backward compatibility
        let cancellation_token = CancellationToken::new();
        self.execute_task_with_timing_cancellable(
            description,
            language,
            _auto_execute,
            show_prompt,
            show_code,
            show_timing,
            cancellation_token,
            discovery_options,
        )
        .await
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn execute_task_with_timing_cancellable(
        &mut self,
        description: &str,
        _language: Option<&str>,
        _auto_execute: bool,
        show_prompt: bool,
        show_code: bool,
        show_timing: bool,
        cancellation_token: CancellationToken,
        discovery_options: Option<DiscoveryOptions<'_>>,
    ) -> Result<TaskResult> {
        // Execute the task directly without splitting
        self.execute_single_task(
            description,
            show_prompt,
            show_code,
            show_timing,
            cancellation_token,
            discovery_options,
        )
        .await
    }

    async fn execute_single_task(
        &mut self,
        description: &str,
        _show_prompt: bool,
        _show_code: bool,
        show_timing: bool,
        cancellation_token: CancellationToken,
        discovery_options: Option<DiscoveryOptions<'_>>,
    ) -> Result<TaskResult> {
        // Reset the JSON tool call filter state at the start of each new task
        // This prevents the filter from staying in suppression mode between user interactions
        self.ui_writer.reset_json_filter();

        // Validate that the system prompt is the first message (critical invariant)
        self.validate_system_prompt_is_first();

        // Generate session ID based on the initial prompt if this is a new session
        if self.session_id.is_none() {
            self.session_id = Some(self.generate_session_id(description));
        }

        // Add user message to context window
        let mut user_message = {
            // Check if we should use cache control (every 10 tool calls)
            // But only if we haven't already added 4 cache_control annotations
            let provider = self.providers.get(None)?;
            let provider_name = provider.name();
            let provider_type = provider_name.split('.').next().unwrap_or("");
            let config_name = provider_name.split('.').nth(1).unwrap_or("default");
            if let Some(cache_config) = match provider_type {
                "anthropic" => {
                    self.config
                        .providers
                        .anthropic
                        .get(config_name)
                        .and_then(|c| c.cache_config.as_ref())
                        .and_then(|config| Self::parse_cache_control(config))
                }
                _ => None,
            } {
                Message::with_cache_control_validated(
                    MessageRole::User,
                    format!("Task: {}", description),
                    cache_config,
                    provider,
                )
            } else {
                Message::new(MessageRole::User, format!("Task: {}", description))
            }
        };
        
        // Attach any pending images to this user message
        if !self.pending_images.is_empty() {
            user_message.images = std::mem::take(&mut self.pending_images);
        }
        
        self.context_window.add_message(user_message);

        // Execute fast-discovery tool calls if provided (immediately after user message)
        if let Some(ref options) = discovery_options {
            self.ui_writer
                .println("▶️  Playing back discovery commands...");
            // Store the working directory for subsequent tool calls in the streaming loop
            if let Some(path) = options.fast_start_path {
                self.working_dir = Some(path.to_string());
            }
            let provider = self.providers.get(None)?;
            let supports_cache = provider.supports_cache_control();
            let message_count = options.messages.len();

            for (idx, discovery_msg) in options.messages.iter().enumerate() {
                if let Ok(tool_call) = serde_json::from_str::<ToolCall>(&discovery_msg.content) {
                    self.add_message_to_context(discovery_msg.clone());
                    let result = self
                        .execute_tool_call_in_dir(&tool_call, options.fast_start_path)
                        .await
                        .unwrap_or_else(|e| format!("Error: {}", e));

                    // Add cache_control to the last user message if provider supports it (anthropic)
                    let is_last = idx == message_count - 1;
                    let result_message = if supports_cache
                        && is_last
                        && self.count_cache_controls_in_history() < 4
                    {
                        Message::with_cache_control(
                            MessageRole::User,
                            format!("Tool result: {}", result),
                            CacheControl::ephemeral(),
                        )
                    } else {
                        Message::new(MessageRole::User, format!("Tool result: {}", result))
                    };
                    self.add_message_to_context(result_message);
                }
            }
        }

        // Use the complete conversation history for the request
        let messages = self.context_window.conversation_history.clone();

        // Check if provider supports native tool calling and add tools if so
        let provider = self.providers.get(None)?;
        let provider_name = provider.name().to_string();
        let _has_native_tool_calling = provider.has_native_tool_calling();
        let _supports_cache_control = provider.supports_cache_control();
        let tools = if provider.has_native_tool_calling() {
            Some(Self::create_tool_definitions(
                self.config.webdriver.enabled,
                self.config.macax.enabled,
                self.config.computer_control.enabled,
            ))
        } else {
            None
        };
        let _ = provider; // Drop the provider reference to avoid borrowing issues

        // Get max_tokens from provider configuration with preflight validation
        // This ensures max_tokens > thinking.budget_tokens for Anthropic with extended thinking
        let initial_max_tokens = self.resolve_max_tokens(&provider_name);
        let max_tokens = Some(self.apply_max_tokens_fallback_sequence(
            &provider_name,
            initial_max_tokens,
            16000, // Hard-coded minimum for main API calls (higher than summary's 5000)
        ));

        let request = CompletionRequest {
            messages,
            max_tokens,
            temperature: Some(self.resolve_temperature(&provider_name)),
            stream: true, // Enable streaming
            tools,
            disable_thinking: false,
        };

        // Time the LLM call with cancellation support and streaming
        let llm_start = Instant::now();
        let result = tokio::select! {
            result = self.stream_completion(request, show_timing) => result,
            _ = cancellation_token.cancelled() => {
                // Save context window on cancellation
                self.save_context_window("cancelled");
                Err(anyhow::anyhow!("Operation cancelled by user"))
            }
        };

        let task_result = match result {
            Ok(result) => result,
            Err(e) => {
                // Save context window on error
                self.save_context_window("error");
                return Err(e);
            }
        };

        let response_content = task_result.response.clone();
        let _llm_duration = llm_start.elapsed();

        // Create a mock usage for now (we'll need to track this during streaming)
        let mock_usage = g3_providers::Usage {
            prompt_tokens: 100,                                   // Estimate
            completion_tokens: response_content.len() as u32 / 4, // Rough estimate
            total_tokens: 100 + (response_content.len() as u32 / 4),
        };

        // Update context window with estimated token usage
        self.context_window.update_usage(&mock_usage);

        // Add assistant response to context window only if not empty
        // This prevents the "Skipping empty message" warning when only tools were executed
        if !response_content.trim().is_empty() {
            let assistant_message = Message::new(MessageRole::Assistant, response_content.clone());
            self.context_window.add_message(assistant_message);
        } else {
            debug!("Assistant response was empty (likely only tool execution), skipping message addition");
        }

        // Save context window at the end of successful interaction
        self.save_context_window("completed");

        // Check if we need to do 90% auto-compaction
        if self.pending_90_summarization {
            self.ui_writer
                .print_context_status("\n⚡ Context window reached 90% - auto-compacting...\n");
            if let Err(e) = self.force_summarize().await {
                warn!("Failed to auto-compact at 90%: {}", e);
            } else {
                self.ui_writer.println("");
            }
            self.pending_90_summarization = false;
        }

        // Return the task result which already includes timing if needed
        Ok(task_result)
    }

    /// Generate a session ID based on the initial prompt
    fn generate_session_id(&self, description: &str) -> String {
        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash, Hasher};

        // Clean and truncate the description for a readable filename
        let clean_description = description
            .chars()
            .filter(|c| c.is_alphanumeric() || c.is_whitespace() || *c == '-' || *c == '_')
            .collect::<String>()
            .split_whitespace()
            .take(5) // Take first 5 words
            .collect::<Vec<_>>()
            .join("_")
            .to_lowercase();

        // Create a hash for uniqueness
        let mut hasher = DefaultHasher::new();
        description.hash(&mut hasher);
        let hash = hasher.finish();

        // Format: clean_description_hash
        format!("{}_{:x}", clean_description, hash)
    }

    /// Save the entire context window to a per-session file
    fn save_context_window(&self, status: &str) {
        // Skip logging if quiet mode is enabled
        if self.quiet {
            return;
        }

        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        // Use new .g3/session/<session_id>/ structure if we have a session ID
        let filename = if let Some(ref session_id) = self.session_id {
            // Ensure session directory exists
            if let Err(e) = ensure_session_dir(session_id) {
                error!("Failed to create session directory: {}", e);
                return;
            }
            get_session_file(session_id)
        } else {
            // Fallback to old logs/ directory for sessions without ID
            let logs_dir = get_logs_dir();
            if let Err(e) = std::fs::create_dir_all(&logs_dir) {
                error!("Failed to create logs directory: {}", e);
                return;
            }
            logs_dir.join(format!("g3_context_{}.json", timestamp))
        };

        let context_data = serde_json::json!({
            "session_id": self.session_id,
            "timestamp": timestamp,
            "status": status,
            "context_window": {
                "used_tokens": self.context_window.used_tokens,
                "total_tokens": self.context_window.total_tokens,
                "percentage_used": self.context_window.percentage_used(),
                "conversation_history": self.context_window.conversation_history
            }
        });

        match serde_json::to_string_pretty(&context_data) {
            Ok(json_content) => {
                if let Err(e) = std::fs::write(&filename, &json_content) {
                    error!("Failed to save context window to {:?}: {}", &filename, e);
                }
            }
            Err(e) => {
                error!("Failed to serialize context window: {}", e);
            }
        }
    }

    /// Format token count in compact form (e.g., 1K, 2M, 100b, 200K) and clamp to 4 chars right-aligned
    fn format_token_count(tokens: u32) -> String {
        let mut raw = if tokens >= 1_000_000_000 {
            format!("{}b", tokens / 1_000_000_000)
        } else if tokens >= 1_000_000 {
            format!("{}M", tokens / 1_000_000)
        } else if tokens >= 1_000 {
            format!("{}K", tokens / 1_000)
        } else {
            format!("0K")
        };

        if raw.len() > 4 {
            raw.truncate(4);
        }

        format!("{:>4}", raw)
    }

    /// Pick a single Unicode indicator for token magnitude (maps to previous color bands)
    fn token_indicator(tokens: u32) -> &'static str {
        if tokens <= 1_000 {
            "🟢"
        } else if tokens <= 5_000 {
            "🟡"
        } else if tokens <= 10_000 {
            "🟠"
        } else if tokens <= 20_000 {
            "🔴"
        } else {
            "🟣"
        }
    }

    /// Write context window summary to file
    /// Format: date&time, token_count, message_id, role, first_100_chars
    fn write_context_window_summary(&self) {
        // Skip if quiet mode is enabled
        if self.quiet {
            return;
        }

        // Skip if no session ID
        let session_id = match &self.session_id {
            Some(id) => id,
            None => return,
        };

        // Ensure session directory exists
        if let Err(e) = ensure_session_dir(session_id) {
            error!("Failed to create session directory: {}", e);
            return;
        }

        // Use new .g3/session/<session_id>/ structure
        let filename = get_context_summary_file(session_id);
        let symlink_path = get_g3_dir().join("sessions").join("current_context_window");

        // Build the summary content
        let mut summary_lines = Vec::new();

        for message in &self.context_window.conversation_history {
            let _timestamp = chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string();

            // Estimate tokens for this message
            let message_tokens = ContextWindow::estimate_tokens(&message.content);

            // Format token count
            let token_str = Self::format_token_count(message_tokens);

            // Get token indicator
            let indicator = Self::token_indicator(message_tokens);

            // Get role as string
            let role = match message.role {
                MessageRole::System => "sys",
                MessageRole::User => "usr",
                MessageRole::Assistant => "ass",
            };

            // Get first 100 characters of content
            let content_preview: String = message.content.chars().take(120).collect();

            // Replace newlines with spaces for single-line format
            let content_preview = content_preview.replace('\n', " ").replace('\r', " ");

            // Format: message_id, role, token_count, indicator, first_100_chars
            let line = format!(
                "{}, {}, {} {}, {}\n",
                message.id, role, token_str, indicator, content_preview
            );
            summary_lines.push(line);
        }

        // Add total estimate after the last line of conversation history
        let total_tokens = self.context_window.used_tokens;
        let total_capacity = self.context_window.total_tokens;
        let percentage = self.context_window.percentage_used();
        let total_token_str = Self::format_token_count(total_tokens);
        let capacity_str = Self::format_token_count(total_capacity);
        
        summary_lines.push(format!(
            "\n--- TOTAL: {} / {} ({:.1}%) ---\n",
            total_token_str, capacity_str, percentage
        ));

        // Write to file
        let summary_content = summary_lines.join("");
        if let Err(e) = std::fs::write(&filename, summary_content) {
            error!(
                "Failed to write context window summary to {:?}: {}",
                &filename, e
            );
            return;
        }

        // Update symlink
        // Remove old symlink if it exists
        let _ = std::fs::remove_file(&symlink_path);

        // Create new symlink
        #[cfg(unix)]
        {
            use std::os::unix::fs::symlink;
            let target = format!("context_window_{}.txt", session_id);
            if let Err(e) = symlink(&target, &symlink_path) {
                error!("Failed to create symlink {:?}: {}", &symlink_path, e);
            }
        }

        #[cfg(windows)]
        {
            use std::os::windows::fs::symlink_file;
            let target = format!("context_window_{}.txt", session_id);
            if let Err(e) = symlink_file(&target, &symlink_path) {
                error!("Failed to create symlink {:?}: {}", &symlink_path, e);
            }
        }

        debug!(
            "Context window summary written to {:?} ({} messages)",
            filename,
            self.context_window.conversation_history.len()
        );
    }

    pub fn get_context_window(&self) -> &ContextWindow {
        &self.context_window
    }

    /// Add a message directly to the context window.
    /// Used for injecting discovery messages before the first LLM turn.
    pub fn add_message_to_context(&mut self, message: Message) {
        self.context_window.add_message(message);
    }

    /// Execute a tool call and return the result.
    /// This is a public wrapper around execute_tool for use by external callers
    /// like the planner's fast-discovery feature.
    pub async fn execute_tool_call(&mut self, tool_call: &ToolCall) -> Result<String> {
        self.execute_tool(tool_call).await
    }

    /// Execute a tool call with an optional working directory (for discovery commands)
    pub async fn execute_tool_call_in_dir(
        &mut self,
        tool_call: &ToolCall,
        working_dir: Option<&str>,
    ) -> Result<String> {
        self.execute_tool_in_dir(tool_call, working_dir).await
    }

    /// Log an error message to the session JSON file as the last message
    /// This is used in autonomous mode to record context length exceeded errors
    pub fn log_error_to_session(
        &self,
        error: &anyhow::Error,
        role: &str,
        forensic_context: Option<String>,
    ) {
        // Skip if quiet mode is enabled
        if self.quiet {
            return;
        }

        // Only log if we have a session ID
        let session_id = match &self.session_id {
            Some(id) => id,
            None => {
                error!("Cannot log error to session: no session ID");
                return;
            }
        };

        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        let logs_dir = get_logs_dir();
        let filename = logs_dir.join(format!("g3_session_{}.json", session_id));

        // Read existing session log
        let mut session_data: serde_json::Value = if std::path::Path::new(&filename).exists() {
            match std::fs::read_to_string(&filename) {
                Ok(content) => {
                    serde_json::from_str(&content).unwrap_or_else(|_| serde_json::json!({}))
                }
                Err(_) => serde_json::json!({}),
            }
        } else {
            serde_json::json!({})
        };

        // Build error message with forensic context
        let error_message = if let Some(context) = forensic_context {
            format!("ERROR: {}\n\nForensic Context:\n{}", error, context)
        } else {
            format!("ERROR: {}", error)
        };

        // Create error message entry
        let error_entry = serde_json::json!({
            "role": role,
            "content": error_message,
            "timestamp": timestamp,
            "error_type": "context_length_exceeded"
        });

        // Append to conversation history
        if let Some(history) = session_data
            .get_mut("context_window")
            .and_then(|cw| cw.get_mut("conversation_history"))
        {
            if let Some(history_array) = history.as_array_mut() {
                history_array.push(error_entry);
            }
        }

        // Write back to file
        if let Ok(json_content) = serde_json::to_string_pretty(&session_data) {
            let _ = std::fs::write(&filename, json_content);
        }
    }

    /// Manually trigger context summarization regardless of context window size
    /// Returns Ok(true) if summarization was successful, Ok(false) if it failed
    pub async fn force_summarize(&mut self) -> Result<bool> {
        debug!("Manual summarization triggered");

        self.ui_writer.print_context_status(&format!(
            "\n🗜️ Manual summarization requested (current usage: {}%)...",
            self.context_window.percentage_used() as u32
        ));

        let provider = self.providers.get(None)?;
        let provider_name = provider.name().to_string();
        let _ = provider; // Release borrow early

        // Apply fallback sequence: thinnify -> skinnify -> hard-coded 5000
        let mut summary_max_tokens = self.apply_summary_fallback_sequence(&provider_name);

        // Apply provider-specific caps
        // For Anthropic with thinking enabled, we need max_tokens > thinking.budget_tokens
        // So we set a higher cap when thinking is configured
        let anthropic_cap = match self.get_thinking_budget_tokens(&provider_name) {
            Some(budget) => (budget + 2000).max(10_000), // At least budget + 2000 for response
            None => 10_000,
        };
        summary_max_tokens = match provider_name.as_str() {
            "anthropic" => summary_max_tokens.min(anthropic_cap),
            "databricks" => summary_max_tokens.min(10_000),
            "embedded" => summary_max_tokens.min(3000),
            _ => summary_max_tokens.min(5000),
        };

        debug!(
            "Requesting summary with max_tokens: {} (current usage: {} tokens)",
            summary_max_tokens, self.context_window.used_tokens
        );

        // Create summary request with FULL history
        let summary_prompt = self.context_window.create_summary_prompt();

        // Get the full conversation history
        let conversation_text = self
            .context_window
            .conversation_history
            .iter()
            .map(|m| format!("{:?}: {}", m.role, m.content))
            .collect::<Vec<_>>()
            .join("\n\n");

        let summary_messages = vec![
            Message::new(
                MessageRole::System,
                "You are a helpful assistant that creates concise summaries.".to_string(),
            ),
            Message::new(
                MessageRole::User,
                format!(
                    "Based on this conversation history, {}\n\nConversation:\n{}",
                    summary_prompt, conversation_text
                ),
            ),
        ];

        let provider = self.providers.get(None)?;

        // Determine if we need to disable thinking mode for this request
        // Anthropic requires: max_tokens > thinking.budget_tokens + 1024
        let disable_thinking = self.get_thinking_budget_tokens(provider.name()).map_or(false, |budget| {
            let minimum_for_thinking = budget + 1024;
            let should_disable = summary_max_tokens <= minimum_for_thinking;
            if should_disable {
                tracing::warn!("Disabling thinking mode for summary: max_tokens ({}) <= minimum_for_thinking ({})", summary_max_tokens, minimum_for_thinking);
            }
            should_disable
        });

        tracing::debug!("Creating summary request: max_tokens={}, disable_thinking={}", summary_max_tokens, disable_thinking);

        let summary_request = CompletionRequest {
            messages: summary_messages,
            max_tokens: Some(summary_max_tokens),
            temperature: Some(self.resolve_temperature(provider.name())),
            stream: false,
            tools: None,
            disable_thinking,
        };

        // Get the summary
        match provider.complete(summary_request).await {
            Ok(summary_response) => {
                self.ui_writer
                    .print_context_status("✅ Context compacted successfully.\n");

                // Get the latest user message to preserve it
                let latest_user_msg = self
                    .context_window
                    .conversation_history
                    .iter()
                    .rev()
                    .find(|m| matches!(m.role, MessageRole::User))
                    .map(|m| m.content.clone());

                // Reset context with summary
                let chars_saved = self
                    .context_window
                    .reset_with_summary(summary_response.content, latest_user_msg);
                self.summarization_events.push(chars_saved);

                Ok(true)
            }
            Err(e) => {
                error!("Failed to create summary: {}", e);
                self.ui_writer.print_context_status(
                    "⚠️ Unable to create summary. Please try again or start a new session.\n",
                );
                Ok(false)
            }
        }
    }

    /// Manually trigger context thinning regardless of thresholds
    pub fn force_thin(&mut self) -> String {
        debug!("Manual context thinning triggered");
        let (message, chars_saved) = self.context_window.thin_context(self.session_id.as_deref());
        self.thinning_events.push(chars_saved);
        message
    }

    /// Manually trigger context thinning for the ENTIRE context window
    /// Unlike force_thin which only processes the first third, this processes all messages
    pub fn force_thin_all(&mut self) -> String {
        debug!("Manual full context skinnifying triggered");
        let (message, chars_saved) = self.context_window.thin_context_all(self.session_id.as_deref());
        self.thinning_events.push(chars_saved);
        message
    }

    /// Reload README.md and AGENTS.md and replace the first system message
    /// Returns Ok(true) if README was found and reloaded, Ok(false) if no README was present initially
    pub fn reload_readme(&mut self) -> Result<bool> {
        debug!("Manual README reload triggered");

        // Check if the second message in conversation history is a system message with README content
        // (The first message should always be the system prompt)
        let has_readme = self
            .context_window
            .conversation_history
            .get(1) // Check the SECOND message (index 1)
            .map(|m| {
                matches!(m.role, MessageRole::System)
                    && (m.content.contains("Project README")
                        || m.content.contains("Agent Configuration"))
            })
            .unwrap_or(false);

        // Validate that the system prompt is still first
        self.validate_system_prompt_is_first();

        if !has_readme {
            return Ok(false);
        }

        // Try to load README.md and AGENTS.md
        let mut combined_content = String::new();
        let mut found_any = false;

        if let Ok(agents_content) = std::fs::read_to_string("AGENTS.md") {
            combined_content.push_str("# Agent Configuration\n\n");
            combined_content.push_str(&agents_content);
            combined_content.push_str("\n\n");
            found_any = true;
        }

        if let Ok(readme_content) = std::fs::read_to_string("README.md") {
            combined_content.push_str("# Project README\n\n");
            combined_content.push_str(&readme_content);
            found_any = true;
        }

        if found_any {
            // Replace the second message (README) with the new content
            if let Some(first_msg) = self.context_window.conversation_history.get_mut(1) {
                first_msg.content = combined_content;
                debug!("README content reloaded successfully");
                Ok(true)
            } else {
                Ok(false)
            }
        } else {
            Ok(false)
        }
    }

    /// Get detailed context statistics
    pub fn get_stats(&self) -> String {
        let mut stats = String::new();
        use std::time::Duration;

        stats.push_str("\n📊 Context Window Statistics\n");
        stats.push_str(&"=".repeat(60));
        stats.push_str("\n\n");

        // Context window usage
        stats.push_str("🗂️  Context Window:\n");
        stats.push_str(&format!(
            "   • Used Tokens:       {:>10} / {}\n",
            self.context_window.used_tokens, self.context_window.total_tokens
        ));
        stats.push_str(&format!(
            "   • Usage Percentage:  {:>10.1}%\n",
            self.context_window.percentage_used()
        ));
        stats.push_str(&format!(
            "   • Remaining Tokens:  {:>10}\n",
            self.context_window.remaining_tokens()
        ));
        stats.push_str(&format!(
            "   • Cumulative Tokens: {:>10}\n",
            self.context_window.cumulative_tokens
        ));
        stats.push_str(&format!(
            "   • Last Thinning:     {:>10}%\n",
            self.context_window.last_thinning_percentage
        ));
        stats.push('\n');

        // Context optimization metrics
        stats.push_str("🗜️  Context Optimization:\n");
        stats.push_str(&format!(
            "   • Thinning Events:   {:>10}\n",
            self.thinning_events.len()
        ));
        if !self.thinning_events.is_empty() {
            let total_thinned: usize = self.thinning_events.iter().sum();
            let avg_thinned = total_thinned / self.thinning_events.len();
            stats.push_str(&format!("   • Total Chars Saved: {:>10}\n", total_thinned));
            stats.push_str(&format!("   • Avg Chars/Event:   {:>10}\n", avg_thinned));
        }

        stats.push_str(&format!(
            "   • Summarizations:    {:>10}\n",
            self.summarization_events.len()
        ));
        if !self.summarization_events.is_empty() {
            let total_summarized: usize = self.summarization_events.iter().sum();
            let avg_summarized = total_summarized / self.summarization_events.len();
            stats.push_str(&format!(
                "   • Total Chars Saved: {:>10}\n",
                total_summarized
            ));
            stats.push_str(&format!("   • Avg Chars/Event:   {:>10}\n", avg_summarized));
        }
        stats.push('\n');

        // Performance metrics
        stats.push_str("⚡ Performance:\n");
        if !self.first_token_times.is_empty() {
            let avg_ttft = self.first_token_times.iter().sum::<Duration>()
                / self.first_token_times.len() as u32;
            let mut sorted_times = self.first_token_times.clone();
            sorted_times.sort();
            let median_ttft = sorted_times[sorted_times.len() / 2];
            stats.push_str(&format!(
                "   • Avg Time to First Token:    {:>6.3}s\n",
                avg_ttft.as_secs_f64()
            ));
            stats.push_str(&format!(
                "   • Median Time to First Token: {:>6.3}s\n",
                median_ttft.as_secs_f64()
            ));
        }
        stats.push('\n');

        // Conversation history
        stats.push_str("💬 Conversation History:\n");
        stats.push_str(&format!(
            "   • Total Messages:    {:>10}\n",
            self.context_window.conversation_history.len()
        ));

        // Count messages by role
        let mut system_count = 0;
        let mut user_count = 0;
        let mut assistant_count = 0;

        for msg in &self.context_window.conversation_history {
            match msg.role {
                MessageRole::System => system_count += 1,
                MessageRole::User => user_count += 1,
                MessageRole::Assistant => assistant_count += 1,
            }
        }

        stats.push_str(&format!("   • System Messages:   {:>10}\n", system_count));
        stats.push_str(&format!("   • User Messages:     {:>10}\n", user_count));
        stats.push_str(&format!(
            "   • Assistant Messages:{:>10}\n",
            assistant_count
        ));
        stats.push('\n');

        // Tool call metrics
        stats.push_str("🔧 Tool Call Metrics:\n");
        stats.push_str(&format!(
            "   • Total Tool Calls:  {:>10}\n",
            self.tool_call_metrics.len()
        ));

        let successful_calls = self
            .tool_call_metrics
            .iter()
            .filter(|(_, _, success)| *success)
            .count();
        let failed_calls = self.tool_call_metrics.len() - successful_calls;

        stats.push_str(&format!(
            "   • Successful:        {:>10}\n",
            successful_calls
        ));
        stats.push_str(&format!("   • Failed:            {:>10}\n", failed_calls));

        if !self.tool_call_metrics.is_empty() {
            let total_duration: Duration = self
                .tool_call_metrics
                .iter()
                .map(|(_, duration, _)| *duration)
                .sum();
            let avg_duration = total_duration / self.tool_call_metrics.len() as u32;

            stats.push_str(&format!(
                "   • Total Duration:    {:>10.2}s\n",
                total_duration.as_secs_f64()
            ));
            stats.push_str(&format!(
                "   • Average Duration:  {:>10.2}s\n",
                avg_duration.as_secs_f64()
            ));
        }
        stats.push('\n');

        // Provider info
        stats.push_str("🔌 Provider:\n");
        if let Ok((provider, model)) = self.get_provider_info() {
            stats.push_str(&format!("   • Provider:          {}\n", provider));
            stats.push_str(&format!("   • Model:             {}\n", model));
        }

        stats.push_str(&"=".repeat(60));
        stats.push('\n');

        stats
    }

    pub fn get_tool_call_metrics(&self) -> &Vec<(String, Duration, bool)> {
        &self.tool_call_metrics
    }

    pub fn get_config(&self) -> &Config {
        &self.config
    }

    pub fn set_requirements_sha(&mut self, sha: String) {
        self.requirements_sha = Some(sha);
    }

    /// Save a session continuation artifact
    /// Called when final_output is invoked to enable session resumption
    pub fn save_session_continuation(&self, final_output_summary: Option<String>) {
        use crate::session_continuation::{save_continuation, SessionContinuation};
        
        let session_id = match &self.session_id {
            Some(id) => id.clone(),
            None => {
                debug!("No session ID, skipping continuation save");
                return;
            }
        };
        
        // Get the session log path (now in .g3/sessions/<session_id>/session.json)
        let session_log_path = get_session_file(&session_id);
        
        // Get current TODO content
        let todo_snapshot = std::fs::read_to_string(get_todo_path()).ok();
        
        // Get working directory
        let working_directory = std::env::current_dir()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_else(|_| ".".to_string());
        
        let continuation = SessionContinuation::new(
            session_id,
            final_output_summary,
            session_log_path.to_string_lossy().to_string(),
            self.context_window.percentage_used(),
            todo_snapshot,
            working_directory,
        );
        
        if let Err(e) = save_continuation(&continuation) {
            error!("Failed to save session continuation: {}", e);
        } else {
            debug!("Saved session continuation artifact");
        }
    }
    
    /// Clear session state and continuation artifacts (for /clear command)
    pub fn clear_session(&mut self) {
        use crate::session_continuation::clear_continuation;
        
        // Clear the context window (keep system prompt)
        self.context_window.clear_conversation();
        
        // Clear continuation artifacts
        if let Err(e) = clear_continuation() {
            error!("Failed to clear continuation artifacts: {}", e);
        }
        
        debug!("Session cleared");
    }

    /// Restore session from a continuation artifact
    /// Returns true if full context was restored, false if only summary was used
    pub fn restore_from_continuation(
        &mut self,
        continuation: &crate::session_continuation::SessionContinuation,
    ) -> Result<bool> {
        use std::path::PathBuf;
        
        let session_log_path = PathBuf::from(&continuation.session_log_path);
        
        // If context < 80%, try to restore full context
        if continuation.can_restore_full_context() && session_log_path.exists() {
            // Load the session log
            let json = std::fs::read_to_string(&session_log_path)?;
            let session_data: serde_json::Value = serde_json::from_str(&json)?;
            
            // Extract conversation history
            if let Some(context_window) = session_data.get("context_window") {
                if let Some(history) = context_window.get("conversation_history") {
                    if let Some(messages) = history.as_array() {
                        // Clear current conversation (keep system messages)
                        self.context_window.clear_conversation();
                        
                        // Restore messages from session log (skip system messages as they're preserved)
                        for msg in messages {
                            let role_str = msg.get("role").and_then(|r| r.as_str()).unwrap_or("user");
                            let content = msg.get("content").and_then(|c| c.as_str()).unwrap_or("");
                            
                            let role = match role_str {
                                "system" => continue, // Skip system messages, already preserved
                                "assistant" => MessageRole::Assistant,
                                _ => MessageRole::User,
                            };
                            
                            self.context_window.add_message(Message {
                                role,
                                id: String::new(),
                                images: Vec::new(),
                                content: content.to_string(),
                                cache_control: None,
                            });
                        }
                        
                        debug!("Restored full context from session log");
                        return Ok(true);
                    }
                }
            }
        }
        
        // Fall back to using final_output summary + TODO
        let mut context_msg = String::new();
        if let Some(ref summary) = continuation.final_output_summary {
            context_msg.push_str(&format!("Previous session summary:\n{}\n\n", summary));
        }
        if let Some(ref todo) = continuation.todo_snapshot {
            context_msg.push_str(&format!("Current TODO state:\n{}\n", todo));
        }
        
        if !context_msg.is_empty() {
            self.context_window.add_message(Message {
                role: MessageRole::User,
                id: String::new(),
                images: Vec::new(),
                content: format!("[Session Resumed]\n\n{}", context_msg),
                cache_control: None,
            });
        }
        
        debug!("Restored session from summary");
        Ok(false)
    }

    async fn stream_completion(
        &mut self,
        request: CompletionRequest,
        show_timing: bool,
    ) -> Result<TaskResult> {
        self.stream_completion_with_tools(request, show_timing)
            .await
    }

    /// Create tool definitions for native tool calling providers
    fn create_tool_definitions(
        enable_webdriver: bool,
        enable_macax: bool,
        enable_computer_control: bool,
    ) -> Vec<Tool> {
        let mut tools = vec![
            Tool {
                name: "shell".to_string(),
                description: "Execute shell commands".to_string(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "command": {
                            "type": "string",
                            "description": "The shell command to execute"
                        }
                    },
                    "required": ["command"]
                }),
            },
            Tool {
                name: "background_process".to_string(),
                description: "Launch a long-running process in the background (e.g., game servers, dev servers). The process runs independently and logs are captured to a file. Use the regular 'shell' tool to read logs (cat/tail), check status (ps), or stop the process (kill). Returns the PID and log file path.".to_string(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "name": {
                            "type": "string",
                            "description": "A unique name for this process (e.g., 'game_server', 'my_app'). Used to identify the process and its log file."
                        },
                        "command": {
                            "type": "string",
                            "description": "The shell command to execute in the background"
                        },
                        "working_dir": {
                            "type": "string",
                            "description": "Optional working directory. Defaults to current directory if not specified."
                        }
                    },
                    "required": ["name", "command"]
                }),
            },
            Tool {
                name: "read_file".to_string(),
                description: "Read the contents of a file. For image files (png, jpg, jpeg, gif, bmp, tiff, webp), automatically extracts text using OCR. For text files, optionally read a specific character range.".to_string(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "file_path": {
                            "type": "string",
                            "description": "The path to the file to read"
                        },
                        "start": {
                            "type": "integer",
                            "description": "Starting character position (0-indexed, inclusive). If omitted, reads from beginning."
                        },
                        "end": {
                            "type": "integer",
                            "description": "Ending character position (0-indexed, EXCLUSIVE). If omitted, reads to end of file."
                        }
                    },
                    "required": ["file_path"]
                }),
            },
            Tool {
                name: "read_image".to_string(),
                description: "Read one or more image files and send them to the LLM for visual analysis. Supports PNG, JPEG, GIF, and WebP formats. Use this when you need to visually inspect images (e.g., find sprites, analyze UI, read diagrams). The images will be included in your next response for analysis.".to_string(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "file_paths": {
                            "type": "array",
                            "items": { "type": "string" },
                            "description": "Array of paths to image files to read"
                        }
                    },
                    "required": ["file_paths"]
                }),
            },
            Tool {
                name: "write_file".to_string(),
                description: "Write content to a file (creates or overwrites). You MUST provide all arguments".to_string(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "file_path": {
                            "type": "string",
                            "description": "The path to the file to write"
                        },
                        "content": {
                            "type": "string",
                            "description": "The content to write to the file"
                        }
                    },
                    "required": ["file_path", "content"]
                }),
            },
            Tool {
                name: "str_replace".to_string(),
                description: "Apply a unified diff to a file. Supports multiple hunks and context lines. Optionally constrain the search to a [start, end) character range (0-indexed; end is EXCLUSIVE). Useful to disambiguate matches or limit scope in large files.".to_string(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "file_path": {
                            "type": "string",
                            "description": "The path to the file to edit"
                        },
                        "diff": {
                            "type": "string",
                            "description": "A unified diff showing what to replace. Supports @@ hunk headers, context lines, and multiple hunks (---/+++ headers optional for minimal diffs)."
                        },
                        "start": {
                            "type": "integer",
                            "description": "Starting character position in the file (0-indexed, inclusive). If omitted, searches from beginning."
                        },
                        "end": {
                            "type": "integer",
                            "description": "Ending character position in the file (0-indexed, EXCLUSIVE - character at this position is NOT included). If omitted, searches to end of file."
                        }
                    },
                    "required": ["file_path", "diff"]
                }),
            },
            Tool {
                name: "final_output".to_string(),
                description: "Signal task completion with a detailed summary".to_string(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "summary": {
                            "type": "string",
                            "description": "A detailed summary in markdown of what was accomplished"
                        }
                    },
                    "required": ["summary"]
                }),
            },
            Tool {
                name: "take_screenshot".to_string(),
                description: "Capture a screenshot of a specific application window. You MUST specify the window_id parameter with the application name (e.g., 'Safari', 'Terminal', 'Google Chrome'). The tool will automatically use the native screencapture command with the application's window ID for a clean capture. Use list_windows first to identify available windows.".to_string(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "path": {
                            "type": "string",
                            "description": "Filename for the screenshot (e.g., 'safari.png'). If a relative path is provided, the screenshot will be saved to ~/tmp or $TMPDIR. Use an absolute path to save elsewhere."
                        },
                        "window_id": {
                            "type": "string",
                            "description": "REQUIRED: Application name to capture (e.g., 'Safari', 'Terminal', 'Google Chrome'). The tool will capture the frontmost window of that application using its native window ID."
                        },
                        "region": {
                            "type": "object",
                            "properties": {
                                "x": {"type": "integer"},
                                "y": {"type": "integer"},
                                "width": {"type": "integer"},
                                "height": {"type": "integer"}
                            }
                        }
                    },
                    "required": ["path", "window_id"]
                }),
            },
            Tool {
                name: "extract_text".to_string(),
                description: "Extract text from an image file using OCR. For extracting text from a specific window, use vision_find_text instead which automatically handles window capture.".to_string(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "path": {
                            "type": "string",
                            "description": "Path to image file (optional if region is provided)"
                        },
                    }
                }),
            },
            Tool {
                name: "todo_read".to_string(),
                description: "Read your current TODO list from todo.g3.md file in the session directory. Shows what tasks are planned and their status. Call this at the start of multi-step tasks to check for existing plans, and during execution to review progress before updating. TODO lists are scoped to the current session.".to_string(),
                input_schema: json!({
                    "type": "object",
                    "properties": {},
                    "required": []
                }),
            },
            Tool {
                name: "todo_write".to_string(),
                description: "Create or update your TODO list in todo.g3.md file with a complete task plan. Use markdown checkboxes: - [ ] for pending, - [x] for complete. This tool replaces the entire file content, so always call todo_read first to preserve existing content. Essential for multi-step tasks. TODO lists are scoped to the current session.".to_string(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "content": {
                            "type": "string",
                            "description": "The TODO list content to save. Use markdown checkbox format: - [ ] for incomplete tasks, - [x] for completed tasks. Support nested tasks with indentation."
                        }
                    },
                    "required": ["content"]
                }),
            },
            Tool {
                name: "code_coverage".to_string(),
                description: "Generate a code coverage report for the entire workspace using cargo llvm-cov. This runs all tests with coverage instrumentation and returns a summary of coverage statistics. Requires llvm-tools-preview and cargo-llvm-cov to be installed (they will be auto-installed if missing).".to_string(),
                input_schema: json!({
                    "type": "object",
                    "properties": {},
                    "required": []
                }),
            },
        ];

        // Add code_search tool
        tools.push(Tool {
            name: "code_search".to_string(),
            description: "Syntax-aware code search that understands code structure, not just text. Finds actual functions, classes, methods, and other code constructs - ignores matches in comments and strings. Much more accurate than grep for code searches. Supports batch searches (up to 20 parallel) with structured results and context lines. Languages: Rust, Python, JavaScript, TypeScript, Go, Java, C, C++, Kotlin. Uses tree-sitter query syntax.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "searches": {
                        "type": "array",
                        "maxItems": 20,
                        "items": {
                            "type": "object",
                            "properties": {
                                "name": { "type": "string", "description": "Label for this search." },
                                "query": { "type": "string", "description": "tree-sitter query in S-expression format (e.g., \"(function_item name: (identifier) @name)\")"},
                                "language": { "type": "string", "enum": ["rust", "python", "javascript", "typescript", "go", "java", "c", "cpp", "kotlin"], "description": "Programming language to search." },
                                "paths": { "type": "array", "items": { "type": "string" }, "description": "Paths/dirs to search. Defaults to current dir if empty." },
                                "context_lines": { "type": "integer", "minimum": 0, "maximum": 20, "default": 0, "description": "Lines of context to include around each match." }
                            },
                            "required": ["name", "query", "language"]
                        }
                    },
                    "max_concurrency": { "type": "integer", "minimum": 1, "default": 4 },
                    "max_matches_per_search": { "type": "integer", "minimum": 1, "default": 500 }
                },
                "required": ["searches"]
            }),
        });

        // Add WebDriver tools if enabled
        if enable_webdriver {
            tools.extend(vec![
                Tool {
                    name: "webdriver_start".to_string(),
                    description: "Start a Safari WebDriver session for browser automation. Must be called before any other webdriver tools. Requires Safari's 'Allow Remote Automation' to be enabled in Develop menu.".to_string(),
                    input_schema: json!({
                        "type": "object",
                        "properties": {},
                        "required": []
                    }),
                },
                Tool {
                    name: "webdriver_navigate".to_string(),
                    description: "Navigate to a URL in the browser".to_string(),
                    input_schema: json!({
                        "type": "object",
                        "properties": {
                            "url": {
                                "type": "string",
                                "description": "The URL to navigate to (must include protocol, e.g., https://)"
                            }
                        },
                        "required": ["url"]
                    }),
                },
                Tool {
                    name: "webdriver_get_url".to_string(),
                    description: "Get the current URL of the browser".to_string(),
                    input_schema: json!({
                        "type": "object",
                        "properties": {},
                        "required": []
                    }),
                },
                Tool {
                    name: "webdriver_get_title".to_string(),
                    description: "Get the title of the current page".to_string(),
                    input_schema: json!({
                        "type": "object",
                        "properties": {},
                        "required": []
                    }),
                },
                Tool {
                    name: "webdriver_find_element".to_string(),
                    description: "Find an element on the page by CSS selector and return its text content".to_string(),
                    input_schema: json!({
                        "type": "object",
                        "properties": {
                            "selector": {
                                "type": "string",
                                "description": "CSS selector to find the element (e.g., 'h1', '.class-name', '#id')"
                            }
                        },
                        "required": ["selector"]
                    }),
                },
                Tool {
                    name: "webdriver_find_elements".to_string(),
                    description: "Find all elements matching a CSS selector and return their text content".to_string(),
                    input_schema: json!({
                        "type": "object",
                        "properties": {
                            "selector": {
                                "type": "string",
                                "description": "CSS selector to find elements"
                            }
                        },
                        "required": ["selector"]
                    }),
                },
                Tool {
                    name: "webdriver_click".to_string(),
                    description: "Click an element on the page".to_string(),
                    input_schema: json!({
                        "type": "object",
                        "properties": {
                            "selector": {
                                "type": "string",
                                "description": "CSS selector for the element to click"
                            }
                        },
                        "required": ["selector"]
                    }),
                },
                Tool {
                    name: "webdriver_send_keys".to_string(),
                    description: "Type text into an input element".to_string(),
                    input_schema: json!({
                        "type": "object",
                        "properties": {
                            "selector": {
                                "type": "string",
                                "description": "CSS selector for the input element"
                            },
                            "text": {
                                "type": "string",
                                "description": "Text to type into the element"
                            },
                            "clear_first": {
                                "type": "boolean",
                                "description": "Whether to clear the element before typing (default: true)"
                            }
                        },
                        "required": ["selector", "text"]
                    }),
                },
                Tool {
                    name: "webdriver_execute_script".to_string(),
                    description: "Execute JavaScript code in the browser and return the result".to_string(),
                    input_schema: json!({
                        "type": "object",
                        "properties": {
                            "script": {
                                "type": "string",
                                "description": "JavaScript code to execute (use 'return' to return a value)"
                            }
                        },
                        "required": ["script"]
                    }),
                },
                Tool {
                    name: "webdriver_get_page_source".to_string(),
                    description: "Get the rendered HTML source of the current page. Returns the current DOM state after JavaScript execution.".to_string(),
                    input_schema: json!({
                        "type": "object",
                        "properties": {
                            "max_length": {
                                "type": "integer",
                                "description": "Maximum length of HTML to return (default: 10000, use 0 for no truncation)"
                            },
                            "save_to_file": {
                                "type": "string",
                                "description": "Optional file path to save the HTML instead of returning it inline"
                            }
                        },
                        "required": []
                    }),
                },
                Tool {
                    name: "webdriver_screenshot".to_string(),
                    description: "Take a screenshot of the browser window".to_string(),
                    input_schema: json!({
                        "type": "object",
                        "properties": {
                            "path": {
                                "type": "string",
                                "description": "Path where to save the screenshot (e.g., '/tmp/screenshot.png')"
                            }
                        },
                        "required": ["path"]
                    }),
                },
                Tool {
                    name: "webdriver_back".to_string(),
                    description: "Navigate back in browser history".to_string(),
                    input_schema: json!({
                        "type": "object",
                        "properties": {},
                        "required": []
                    }),
                },
                Tool {
                    name: "webdriver_forward".to_string(),
                    description: "Navigate forward in browser history".to_string(),
                    input_schema: json!({
                        "type": "object",
                        "properties": {},
                        "required": []
                    }),
                },
                Tool {
                    name: "webdriver_refresh".to_string(),
                    description: "Refresh the current page".to_string(),
                    input_schema: json!({
                        "type": "object",
                        "properties": {},
                        "required": []
                    }),
                },
                Tool {
                    name: "webdriver_quit".to_string(),
                    description: "Close the browser and end the WebDriver session".to_string(),
                    input_schema: json!({
                        "type": "object",
                        "properties": {},
                        "required": []
                    }),
                },
            ]);
        }

        // Add macOS Accessibility tools if enabled
        if enable_macax {
            tools.extend(vec![
                Tool {
                    name: "macax_list_apps".to_string(),
                    description: "List all running applications that can be controlled via macOS Accessibility API".to_string(),
                    input_schema: json!({
                        "type": "object",
                        "properties": {},
                        "required": []
                    }),
                },
                Tool {
                    name: "macax_get_frontmost_app".to_string(),
                    description: "Get the name of the currently active (frontmost) application".to_string(),
                    input_schema: json!({
                        "type": "object",
                        "properties": {},
                        "required": []
                    }),
                },
                Tool {
                    name: "macax_activate_app".to_string(),
                    description: "Bring an application to the front (activate it)".to_string(),
                    input_schema: json!({
                        "type": "object",
                        "properties": {
                            "app_name": {
                                "type": "string",
                                "description": "Name of the application to activate (e.g., 'Safari', 'TextEdit')"
                            }
                        },
                        "required": ["app_name"]
                    }),
                },
                Tool {
                    name: "macax_press_key".to_string(),
                    description: "Press a keyboard key or shortcut in an application (e.g., Cmd+S to save)".to_string(),
                    input_schema: json!({
                        "type": "object",
                        "properties": {
                            "app_name": {
                                "type": "string",
                                "description": "Name of the application"
                            },
                            "key": {
                                "type": "string",
                                "description": "Key to press (e.g., 's', 'return', 'tab')"
                            },
                            "modifiers": {
                                "type": "array",
                                "items": {
                                    "type": "string"
                                },
                                "description": "Modifier keys (e.g., ['command', 'shift'])"
                            }
                        },
                        "required": ["app_name", "key"]
                    }),
                },
            ]);

            // Add type_text tool for typing arbitrary text
            tools.push(Tool {
                name: "macax_type_text".to_string(),
                description: "Type arbitrary text into the currently focused element in an application (supports unicode, emojis, etc.)".to_string(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "app_name": {
                            "type": "string",
                            "description": "Name of the application"
                        },
                        "text": {
                            "type": "string",
                            "description": "Text to type (can include unicode, emojis, special characters)"
                        }
                    },
                    "required": ["app_name", "text"]
                }),
            });
        }

        // Add extract_text_with_boxes tool (requires macax flag)
        if enable_macax {
            tools.push(Tool {
                name: "extract_text_with_boxes".to_string(),
                description: "Extract all text from an image file with bounding box coordinates for each text element. Returns JSON array with text, position (x, y), size (width, height), and confidence for each detected text. Uses Apple Vision Framework for precise sub-pixel accuracy.".to_string(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "path": {
                            "type": "string",
                            "description": "Path to image file to extract text from"
                        },
                        "app_name": {
                            "type": "string",
                            "description": "Optional: Name of application to screenshot first (e.g., 'Safari', 'Things3'). If provided, takes screenshot of app before extracting text."
                        }
                    },
                    "required": ["path"]
                }),
            });
        }

        // Add vision-guided tools (requires computer control)
        if enable_computer_control {
            // Add vision-guided tools
            tools.push(Tool {
                name: "vision_find_text".to_string(),
                description: "Find text in a specific application window and return its location with bounding box coordinates (x, y, width, height) and confidence score. Useful for locating UI elements. Uses Apple Vision Framework for precise sub-pixel accuracy.".to_string(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "app_name": {
                            "type": "string",
                            "description": "Name of the application to search in (e.g., 'Things3', 'Safari', 'TextEdit')"
                        },
                        "text": {
                            "type": "string",
                            "description": "The text to search for on screen"
                        }
                    },
                    "required": ["app_name", "text"]
                }),
            });

            tools.push(Tool {
                name: "vision_click_text".to_string(),
                description: "Find text in a specific application window and click on it (useful for clicking buttons, links, menu items)".to_string(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "app_name": {
                            "type": "string",
                            "description": "Name of the application (e.g., 'Things3', 'Safari', 'TextEdit')"
                        },
                        "text": {
                            "type": "string",
                            "description": "The text to click on (e.g., 'Submit', 'OK', 'Cancel', '+')"
                        }
                    },
                    "required": ["app_name", "text"]
                }),
            });

            tools.push(Tool {
                name: "vision_click_near_text".to_string(),
                description: "Find text in a specific application window and click near it (useful for clicking text fields next to labels)".to_string(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "app_name": {
                            "type": "string",
                            "description": "Name of the application (e.g., 'Things3', 'Safari', 'TextEdit')"
                        },
                        "text": {
                            "type": "string",
                            "description": "The label text to find (e.g., 'Name:', 'Email:', 'Task:')"
                        },
                        "direction": {
                            "type": "string",
                            "enum": ["right", "below", "left", "above"],
                            "description": "Direction to click relative to the text (default: right)"
                        },
                        "distance": {
                            "type": "integer",
                            "description": "Distance in pixels from the text (default: 50)"
                        }
                    },
                    "required": ["app_name", "text"]
                }),
            });
        }

        tools
    }

    /// Helper method to stream with retry logic
    async fn stream_with_retry(
        &self,
        request: &CompletionRequest,
        error_context: &error_handling::ErrorContext,
    ) -> Result<g3_providers::CompletionStream> {
        use crate::error_handling::{calculate_retry_delay, classify_error, ErrorType};

        let mut attempt = 0;
        let max_attempts = if self.is_autonomous {
            self.config.agent.autonomous_max_retry_attempts
        } else {
            self.config.agent.max_retry_attempts
        };

        loop {
            attempt += 1;
            let provider = self.providers.get(None)?;

            match provider.stream(request.clone()).await {
                Ok(stream) => {
                    if attempt > 1 {
                        debug!("Stream started successfully after {} attempts", attempt);
                    }
                    debug!("Stream started successfully");
                    debug!(
                        "Request had {} messages, tools={}, max_tokens={:?}",
                        request.messages.len(),
                        request.tools.is_some(),
                        request.max_tokens
                    );
                    return Ok(stream);
                }
                Err(e) if attempt < max_attempts => {
                    if matches!(classify_error(&e), ErrorType::Recoverable(_)) {
                        let delay = calculate_retry_delay(attempt, self.is_autonomous);
                        warn!(
                            "Recoverable error on attempt {}/{}: {}. Retrying in {:?}...",
                            attempt, max_attempts, e, delay
                        );
                        tokio::time::sleep(delay).await;
                    } else {
                        error_context.clone().log_error(&e);
                        return Err(e);
                    }
                }
                Err(e) => {
                    error_context.clone().log_error(&e);
                    return Err(e);
                }
            }
        }
    }

    async fn stream_completion_with_tools(
        &mut self,
        mut request: CompletionRequest,
        show_timing: bool,
    ) -> Result<TaskResult> {
        use crate::error_handling::ErrorContext;
        use tokio_stream::StreamExt;

        debug!("Starting stream_completion_with_tools");

        let mut full_response = String::new();
        let mut first_token_time: Option<Duration> = None;
        let stream_start = Instant::now();
        let mut iteration_count = 0;
        const MAX_ITERATIONS: usize = 400; // Prevent infinite loops
        let mut response_started = false;
        let mut any_tool_executed = false; // Track if ANY tool was executed across all iterations
        let mut auto_summary_attempts = 0; // Track auto-summary prompt attempts
        const MAX_AUTO_SUMMARY_ATTEMPTS: usize = 5; // Limit auto-summary retries (increased from 2 for better recovery)
        let final_output_called = false; // Track if final_output was called
        // Note: Session-level duplicate tracking was removed - we only prevent sequential duplicates (DUP IN CHUNK, DUP IN MSG)
        let mut turn_accumulated_usage: Option<g3_providers::Usage> = None; // Track token usage for timing footer

        // Check if we need to summarize before starting
        if self.context_window.should_summarize() {
            // First try thinning if we are at capacity, don't call the LLM for a summary (might fail)
            if self.context_window.percentage_used() > 90.0 && self.context_window.should_thin() {
                self.ui_writer.print_context_status(&format!(
                    "\n🥒 Context window at {}%. Trying thinning first...",
                    self.context_window.percentage_used() as u32
                ));

                let (thin_summary, chars_saved) = self.context_window.thin_context(self.session_id.as_deref());
                self.thinning_events.push(chars_saved);
                self.ui_writer.print_context_thinning(&thin_summary);

                // Check if thinning was sufficient
                if !self.context_window.should_summarize() {
                    self.ui_writer.print_context_status(
                        "✅ Thinning resolved capacity issue. Continuing...\n",
                    );
                    // Continue with the original request without summarization
                } else {
                    self.ui_writer.print_context_status(
                        "⚠️ Thinning insufficient. Proceeding with summarization...\n",
                    );
                }
            }

            // Only proceed with summarization if still needed after thinning
            if self.context_window.should_summarize() {
                // Notify user about summarization
                self.ui_writer.print_context_status(&format!(
                    "\n🗜️ Context window reaching capacity ({}%). Creating summary...",
                    self.context_window.percentage_used() as u32
                ));

                let provider = self.providers.get(None)?;
                let provider_name = provider.name().to_string();
                let _ = provider; // Release borrow early

                // Apply fallback sequence: thinnify -> skinnify -> hard-coded 5000
                let mut summary_max_tokens = self.apply_summary_fallback_sequence(&provider_name);

                // Apply provider-specific caps
                // For Anthropic with thinking enabled, we need max_tokens > thinking.budget_tokens
                // So we set a higher cap when thinking is configured
                let anthropic_cap = match self.get_thinking_budget_tokens(&provider_name) {
                    Some(budget) => (budget + 2000).max(10_000), // At least budget + 2000 for response
                    None => 10_000,
                };
                summary_max_tokens = match provider_name.split('.').next().unwrap_or(provider_name.as_str()) {
                    "anthropic" => summary_max_tokens.min(anthropic_cap),
                    "databricks" => summary_max_tokens.min(10_000),
                    "azure" => summary_max_tokens.min(10_000),
                    "embedded" => summary_max_tokens.min(3000),
                    _ => summary_max_tokens.min(5000),
                };
                // Ensure max_tokens is at least 1 to avoid API errors
                summary_max_tokens = summary_max_tokens.max(1);

                debug!(
                    "Requesting summary with max_tokens: {} (current usage: {} tokens)",
                    summary_max_tokens, self.context_window.used_tokens
                );

                // Create summary request with FULL history
                let summary_prompt = self.context_window.create_summary_prompt();

                // Get the full conversation history
                let conversation_text = self
                    .context_window
                    .conversation_history
                    .iter()
                    .map(|m| format!("{:?}: {}", m.role, m.content))
                    .collect::<Vec<_>>()
                    .join("\n\n");

                let summary_messages = vec![
                    Message::new(
                        MessageRole::System,
                        "You are a helpful assistant that creates concise summaries.".to_string(),
                    ),
                    Message::new(
                        MessageRole::User,
                        format!(
                            "Based on this conversation history, {}\n\nConversation:\n{}",
                            summary_prompt, conversation_text
                        ),
                    ),
                ];

                let provider = self.providers.get(None)?;

                // Determine if we need to disable thinking mode for this request
                // Anthropic requires: max_tokens > thinking.budget_tokens + 1024
                let disable_thinking = self.get_thinking_budget_tokens(provider.name()).map_or(false, |budget| {
                    let minimum_for_thinking = budget + 1024;
                    let should_disable = summary_max_tokens <= minimum_for_thinking;
                    if should_disable {
                        tracing::warn!("Disabling thinking mode for summary: max_tokens ({}) <= minimum_for_thinking ({})", summary_max_tokens, minimum_for_thinking);
                    }
                    should_disable
                });

                tracing::debug!("Creating auto-summary request: max_tokens={}, disable_thinking={}", summary_max_tokens, disable_thinking);

                let summary_request = CompletionRequest {
                    messages: summary_messages,
                    max_tokens: Some(summary_max_tokens),
                    temperature: Some(self.resolve_temperature(provider.name())),
                    stream: false,
                    tools: None,
                    disable_thinking,
                };

                // Get the summary
                match provider.complete(summary_request).await {
                    Ok(summary_response) => {
                        self.ui_writer.print_context_status(
                            "✅ Context compacted successfully. Continuing...\n",
                        );

                        // Extract the latest user message from the request
                        let latest_user_msg = request
                            .messages
                            .iter()
                            .rev()
                            .find(|m| matches!(m.role, MessageRole::User))
                            .map(|m| m.content.clone());

                        // Reset context with summary
                        let chars_saved = self
                            .context_window
                            .reset_with_summary(summary_response.content, latest_user_msg);
                        self.summarization_events.push(chars_saved);

                        // Update the request with new context
                        request.messages = self.context_window.conversation_history.clone();
                    }
                    Err(e) => {
                        error!("Failed to create summary: {}", e);
                        self.ui_writer.print_context_status("⚠️ Unable to create summary. Consider starting a new session if you continue to see errors.\n");
                        // Don't continue with the original request if summarization failed
                        // as we're likely at token limit
                        return Err(anyhow::anyhow!("Context window at capacity and summarization failed. Please start a new session."));
                    }
                }
            }
        }

        loop {
            iteration_count += 1;
            debug!("Starting iteration {}", iteration_count);
            if iteration_count > MAX_ITERATIONS {
                warn!("Maximum iterations reached, stopping stream");
                break;
            }

            // Add a small delay between iterations to prevent "model busy" errors
            if iteration_count > 1 {
                tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;
            }

            // Get provider info for logging, then drop it to avoid borrow issues
            let (provider_name, provider_model) = {
                let provider = self.providers.get(None)?;
                (provider.name().to_string(), provider.model().to_string())
            };
            debug!("Got provider: {}", provider_name);

            // Create error context for detailed logging
            let last_prompt = request
                .messages
                .iter()
                .rev()
                .find(|m| matches!(m.role, MessageRole::User))
                .map(|m| m.content.clone())
                .unwrap_or_else(|| "No user message found".to_string());

            let error_context = ErrorContext::new(
                "stream_completion".to_string(),
                provider_name.clone(),
                provider_model.clone(),
                last_prompt,
                self.session_id.clone(),
                self.context_window.used_tokens,
                self.quiet,
            )
            .with_request(
                serde_json::to_string(&request)
                    .unwrap_or_else(|_| "Failed to serialize request".to_string()),
            );

            // Log initial request details
            debug!("Starting stream with provider={}, model={}, messages={}, tools={}, max_tokens={:?}",
                provider_name,
                provider_model,
                request.messages.len(),
                request.tools.is_some(),
                request.max_tokens
            );

            // Try to get stream with retry logic
            let mut stream = match self.stream_with_retry(&request, &error_context).await {
                Ok(s) => s,
                Err(e) => {
                    error!("Failed to start stream: {}", e);
                    // Additional retry for "busy" errors on subsequent iterations
                    if iteration_count > 1 && e.to_string().contains("busy") {
                        warn!(
                            "Model busy on iteration {}, attempting one more retry in 500ms",
                            iteration_count
                        );
                        tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;

                        match self.stream_with_retry(&request, &error_context).await {
                            Ok(s) => s,
                            Err(e2) => {
                                error!("Failed to start stream after retry: {}", e2);
                                error_context.clone().log_error(&e2);
                                return Err(e2);
                            }
                        }
                    } else {
                        return Err(e);
                    }
                }
            };

            // Write context window summary every time we send messages to LLM
            self.write_context_window_summary();

            let mut parser = StreamingToolParser::new();
            let mut current_response = String::new();
            let mut tool_executed = false;
            let mut chunks_received = 0;
            let mut raw_chunks: Vec<String> = Vec::new(); // Store raw chunks for debugging
            let mut _last_error: Option<String> = None;
            let mut accumulated_usage: Option<g3_providers::Usage> = None;

            while let Some(chunk_result) = stream.next().await {
                match chunk_result {
                    Ok(chunk) => {
                        // Notify UI about SSE received (including pings)
                        self.ui_writer.notify_sse_received();

                        // Capture usage data if available
                        if let Some(ref usage) = chunk.usage {
                            accumulated_usage = Some(usage.clone());
                            turn_accumulated_usage = Some(usage.clone());
                            debug!(
                                "Received usage data - prompt: {}, completion: {}, total: {}",
                                usage.prompt_tokens, usage.completion_tokens, usage.total_tokens
                            );
                        }

                        // Store raw chunk for debugging (limit to first 20 and last 5)
                        if chunks_received < 20 || chunk.finished {
                            raw_chunks.push(format!(
                                "Chunk #{}: content={:?}, finished={}, tool_calls={:?}",
                                chunks_received + 1,
                                chunk.content,
                                chunk.finished,
                                chunk.tool_calls
                            ));
                        } else if raw_chunks.len() == 20 {
                            raw_chunks.push("... (chunks 21+ omitted for brevity) ...".to_string());
                        }

                        // Record time to first token
                        if first_token_time.is_none() && !chunk.content.is_empty() {
                            first_token_time = Some(stream_start.elapsed());
                            // Record in agent metrics
                            if let Some(ttft) = first_token_time {
                                self.first_token_times.push(ttft);
                            }
                        }

                        chunks_received += 1;
                        if chunks_received == 1 {
                            debug!(
                                "First chunk received: content_len={}, finished={}",
                                chunk.content.len(),
                                chunk.finished
                            );
                        }

                        // Process chunk with the new parser
                        let completed_tools = parser.process_chunk(&chunk);

                        // Handle completed tool calls - process all if multiple calls enabled
                        let tools_to_process: Vec<ToolCall> =
                            if self.config.agent.allow_multiple_tool_calls {
                                completed_tools
                            } else {
                                // Original behavior - only take the first tool
                                completed_tools.into_iter().take(1).collect()
                            };

                        // Helper function to check if two tool calls are duplicates
                        let are_duplicates = |tc1: &ToolCall, tc2: &ToolCall| -> bool {
                            tc1.tool == tc2.tool && tc1.args == tc2.args
                        };

                        // De-duplicate tool calls and track duplicates
                        let mut last_tool_in_chunk: Option<ToolCall> = None;
                        let mut deduplicated_tools: Vec<(ToolCall, Option<String>)> = Vec::new();

                        for tool_call in tools_to_process {
                            let mut duplicate_type = None;

                            // Check for IMMEDIATELY SEQUENTIAL duplicate in current chunk
                            // Only the immediately previous tool call counts as a duplicate
                            if let Some(ref last_tool) = last_tool_in_chunk {
                                if are_duplicates(last_tool, &tool_call) {
                                duplicate_type = Some("DUP IN CHUNK".to_string());
                                }
                            } else {
                                // Check for IMMEDIATELY SEQUENTIAL duplicate against previous message
                                // Only mark as duplicate if the LAST tool call in the previous message
                                // matches AND there's no significant text after it
                                let mut found_in_prev = false;
                                for msg in self.context_window.conversation_history.iter().rev() {
                                    if matches!(msg.role, MessageRole::Assistant) {
                                        // Find the LAST tool call in the message
                                        let content = &msg.content;
                                        
                                        // Look for the last occurrence of a tool call pattern
                                        if let Some(last_tool_start) = content.rfind(r#"{"tool""#)
                                            .or_else(|| content.rfind(r#"{ "tool""#))
                                        {
                                            // Find the end of this JSON object
                                            if let Some(end_offset) = StreamingToolParser::find_complete_json_object_end(&content[last_tool_start..]) {
                                                let end_idx = last_tool_start + end_offset + 1;
                                                let tool_json = &content[last_tool_start..end_idx];
                                                
                                                // Check if there's any non-whitespace text after this tool call
                                                let text_after = content[end_idx..].trim();
                                                let has_text_after = !text_after.is_empty();
                                                
                                                // Only consider it a duplicate if:
                                                // 1. The tool call matches
                                                // 2. There's no text after it (it was the last thing in the message)
                                                if !has_text_after {
                                                    if let Ok(prev_tool) = serde_json::from_str::<ToolCall>(tool_json) {
                                                        if are_duplicates(&prev_tool, &tool_call) {
                                                            found_in_prev = true;
                                                        }
                                                    }
                                                }
                                            }
                                        }
                                        // Only check the most recent assistant message
                                        break;
                                    }
                                }

                                if found_in_prev {
                                    duplicate_type = Some("DUP IN MSG".to_string());
                                }
                            }

                            // Track the last tool call for sequential duplicate detection
                            last_tool_in_chunk = Some(tool_call.clone());

                            deduplicated_tools.push((tool_call, duplicate_type));
                        }

                        // Process each tool call
                        for (tool_call, duplicate_type) in deduplicated_tools {
                            debug!("Processing completed tool call: {:?}", tool_call);
                            
                            // If it's a duplicate, log it and skip - don't set tool_executed!
                            // Setting tool_executed for duplicates would trigger auto-continue
                            // even when no actual tool execution occurred.
                            if let Some(dup_type) = &duplicate_type {
                                // Log the duplicate with red prefix
                                let prefixed_tool_name =
                                    format!("🟥 {} {}", tool_call.tool, dup_type);
                                let warning_msg = format!(
                                    "⚠️ Duplicate tool call detected ({}): Skipping execution of {} with args {}",
                                    dup_type,
                                    tool_call.tool,
                                    serde_json::to_string(&tool_call.args).unwrap_or_else(|_| "<unserializable>".to_string())
                                );

                                // Log to tool log with red prefix
                                let mut modified_tool_call = tool_call.clone();
                                modified_tool_call.tool = prefixed_tool_name;
                                debug!("{}", warning_msg);

                                // Reset the parser to clear any partial/polluted state.
                                // This prevents "example" tool calls in markdown or LLM stuttering
                                // from polluting subsequent parsing.
                                parser.reset();

                                continue; // Skip execution of duplicate
                            }

                            // Mark that we're executing a tool (only for non-duplicates)
                            tool_executed = true;

                            // Check if we should auto-compact at 90% BEFORE executing the tool
                            // We need to do this before any borrows of self
                            if self.auto_compact && self.context_window.percentage_used() >= 90.0 {
                                // Set flag to trigger summarization after this turn completes
                                // We can't do it now due to borrow checker constraints
                                self.pending_90_summarization = true;
                            }

                            // Check if we should thin the context BEFORE executing the tool
                            if self.context_window.should_thin() {
                                let (thin_summary, chars_saved) =
                                    self.context_window.thin_context(self.session_id.as_deref());
                                self.thinning_events.push(chars_saved);
                                // Print the thinning summary to the user
                                self.ui_writer.print_context_thinning(&thin_summary);
                            }

                            // Track what we've already displayed before getting new text
                            // This prevents re-displaying old content after tool execution
                            let already_displayed_chars = current_response.chars().count();

                            // Get the text content accumulated so far
                            let text_content = parser.get_text_content();

                            // Clean the content
                            let clean_content = text_content
                                .replace("<|im_end|>", "")
                                .replace("</s>", "")
                                .replace("[/INST]", "")
                                .replace("<</SYS>>", "");

                            // Store the raw content BEFORE filtering for the context window log
                            let raw_content_for_log = clean_content.clone();

                            // Filter out JSON tool calls from the display
                            let filtered_content =
                                self.ui_writer.filter_json_tool_calls(&clean_content);
                            let final_display_content = filtered_content.trim();

                            // Display any new content before tool execution
                            // We need to skip what was already shown (tracked in current_response)
                            // but also account for the fact that parser.text_buffer accumulates
                            // across iterations and is never cleared until reset()
                            let new_content =
                                if current_response.len() <= final_display_content.len() {
                                    // Only show content that hasn't been displayed yet
                                    final_display_content
                                        .chars()
                                        .skip(already_displayed_chars)
                                        .collect::<String>()
                                } else {
                                    // Nothing new to display
                                    String::new()
                                };

                            // Don't display text before final_output - it will be in the summary
                            if !new_content.trim().is_empty() && tool_call.tool != "final_output" {
                                #[allow(unused_assignments)]
                                if !response_started {
                                    self.ui_writer.print_agent_prompt();
                                    response_started = true;
                                }
                                self.ui_writer.print_agent_response(&new_content);
                                self.ui_writer.flush();
                                // Update current_response to track what we've displayed
                                current_response.push_str(&new_content);
                            }

                            // Execute the tool with formatted output

                            // Skip printing tool call details for final_output
                            if tool_call.tool != "final_output" {
                                // Tool call header
                                self.ui_writer.print_tool_header(&tool_call.tool, Some(&tool_call.args));
                                if let Some(args_obj) = tool_call.args.as_object() {
                                    for (key, value) in args_obj {
                                        let value_str = match value {
                                            serde_json::Value::String(s) => {
                                                if tool_call.tool == "shell" && key == "command" {
                                                    if let Some(first_line) = s.lines().next() {
                                                        if s.lines().count() > 1 {
                                                            format!("{}...", first_line)
                                                        } else {
                                                            first_line.to_string()
                                                        }
                                                    } else {
                                                        s.clone()
                                                    }
                                                } else if s.len() > 100 {
                                                    // Use char_indices to respect UTF-8 boundaries
                                                    let truncated = s
                                                        .char_indices()
                                                        .take(100)
                                                        .map(|(_, c)| c)
                                                        .collect::<String>();
                                                    format!("{}...", truncated)
                                                } else {
                                                    s.clone()
                                                }
                                            }
                                            _ => value.to_string(),
                                        };
                                        self.ui_writer.print_tool_arg(key, &value_str);
                                    }
                                }
                                self.ui_writer.print_tool_output_header();
                            }

                            // Clone working_dir to avoid borrow checker issues
                            let working_dir = self.working_dir.clone();
                            let exec_start = Instant::now();
                            // Add 8-minute timeout for tool execution
                            let tool_result = match tokio::time::timeout(
                                Duration::from_secs(8 * 60), // 8 minutes
                                // Use working_dir if set (from --codebase-fast-start)
                                self.execute_tool_in_dir(&tool_call, working_dir.as_deref()),
                            )
                            .await
                            {
                                Ok(result) => result?,
                                Err(_) => {
                                    warn!("Tool call {} timed out after 8 minutes", tool_call.tool);
                                    "❌ Tool execution timed out after 8 minutes".to_string()
                                }
                            };
                            let exec_duration = exec_start.elapsed();

                            // Track tool call metrics
                            let tool_success = !tool_result.contains("❌");
                            self.tool_call_metrics.push((
                                tool_call.tool.clone(),
                                exec_duration,
                                tool_success,
                            ));

                            // Display tool execution result with proper indentation
                            if tool_call.tool == "final_output" {
                                // For final_output, use the dedicated method that renders markdown
                                // with a spinner animation
                                self.ui_writer.print_final_output(&tool_result);
                            } else {
                                let output_lines: Vec<&str> = tool_result.lines().collect();

                                // Check if UI wants full output (machine mode) or truncated (human mode)
                                let wants_full = self.ui_writer.wants_full_output();

                                // Helper function to safely truncate strings at character boundaries
                                let truncate_line =
                                    |line: &str, max_width: usize, truncate: bool| -> String {
                                        if !truncate {
                                            // Machine mode - return full line
                                            line.to_string()
                                        } else if line.chars().count() <= max_width {
                                            // Human mode - line fits within limit
                                            line.to_string()
                                        } else {
                                            // Human mode - truncate long line
                                            let truncated: String = line
                                                .chars()
                                                .take(max_width.saturating_sub(3))
                                                .collect();
                                            format!("{}...", truncated)
                                        }
                                    };

                                const MAX_LINES: usize = 5;
                                const MAX_LINE_WIDTH: usize = 80;
                                let output_len = output_lines.len();

                                // Skip printing for todo tools - they already print their content
                                let is_todo_tool =
                                    tool_call.tool == "todo_read" || tool_call.tool == "todo_write";

                                if !is_todo_tool {
                                    let max_lines_to_show = if wants_full { output_len } else { MAX_LINES };

                                    for (idx, line) in output_lines.iter().enumerate() {
                                        if !wants_full && idx >= max_lines_to_show {
                                            break;
                                        }
                                        let clipped_line = truncate_line(line, MAX_LINE_WIDTH, !wants_full);
                                        self.ui_writer.update_tool_output_line(&clipped_line);
                                    }

                                    if !wants_full && output_len > MAX_LINES {
                                        self.ui_writer.print_tool_output_summary(output_len);
                                    }
                                }
                            }

                            // Add the tool call and result to the context window using RAW unfiltered content
                            // This ensures the log file contains the true raw content including JSON tool calls
                            let tool_message = if !raw_content_for_log.trim().is_empty() {
                                Message::new(
                                    MessageRole::Assistant,
                                    format!(
                                        "{}\n\n{{\"tool\": \"{}\", \"args\": {}}}",
                                        raw_content_for_log.trim(),
                                        tool_call.tool,
                                        tool_call.args
                                    ),
                                )
                            } else {
                                // No text content before tool call, just include the tool call
                                Message::new(
                                    MessageRole::Assistant,
                                    format!(
                                        "{{\"tool\": \"{}\", \"args\": {}}}",
                                        tool_call.tool, tool_call.args
                                    ),
                                )
                            };
                            let mut result_message = {
                                // Check if we should use cache control (every 10 tool calls)
                                // But only if we haven't already added 4 cache_control annotations
                                if self.tool_call_count > 0
                                    && self.tool_call_count % 10 == 0
                                    && self.count_cache_controls_in_history() < 4
                                {
                                    let provider = self.providers.get(None)?;
                                    let provider_name = provider.name();
                                    let provider_type = provider_name.split('.').next().unwrap_or("");
                                    let config_name = provider_name.split('.').nth(1).unwrap_or("default");
                                    if let Some(cache_config) = match provider_type {
                                        "anthropic" => {
                                            self.config
                                                .providers
                                                .anthropic
                                                .get(config_name)
                                                .and_then(|c| c.cache_config.as_ref())
                                                .and_then(|config| Self::parse_cache_control(config))
                                        }
                                        _ => None,
                                    } {
                                        Message::with_cache_control_validated(
                                            MessageRole::User,
                                            format!("Tool result: {}", tool_result),
                                            cache_config,
                                            provider,
                                        )
                                    } else {
                                        Message::new(
                                            MessageRole::User,
                                            format!("Tool result: {}", tool_result),
                                        )
                                    }
                                } else {
                                    Message::new(
                                        MessageRole::User,
                                        format!("Tool result: {}", tool_result),
                                    )
                                }
                            };

                            // Attach any pending images to the result message
                            // (images loaded via read_image tool)
                            if !self.pending_images.is_empty() {
                                result_message.images = std::mem::take(&mut self.pending_images);
                            }

                            // Track tokens before adding messages
                            let tokens_before = self.context_window.used_tokens;

                            self.context_window.add_message(tool_message);
                            self.context_window.add_message(result_message);

                            // Check if this was a final_output tool call
                            if tool_call.tool == "final_output" {
                                // Save context window BEFORE returning so the session log includes final_output
                                self.save_context_window("completed");
                                
                                // The summary was already displayed via print_final_output
                                // Don't add it to full_response to avoid duplicate printing
                                // full_response is intentionally left empty/unchanged
                                let _ttft =
                                    first_token_time.unwrap_or_else(|| stream_start.elapsed());

                                // Add timing if needed
                                let final_response = if show_timing {
                                    format!(
                                        "🕝 {} | 💭 {}",
                                        Self::format_duration(stream_start.elapsed()),
                                        Self::format_duration(_ttft)
                                    )
                                } else {
                                    // Return empty string since content was already displayed
                                    String::new()
                                };

                                return Ok(TaskResult::new(
                                    final_response,
                                    self.context_window.clone(),
                                ));
                            }

                            // Closure marker with timing
                            if tool_call.tool != "final_output" {
                                let tokens_delta = self.context_window.used_tokens.saturating_sub(tokens_before);
                                self.ui_writer
                                    .print_tool_timing(&Self::format_duration(exec_duration),
                                        tokens_delta,
                                        self.context_window.percentage_used());
                                self.ui_writer.print_agent_prompt();
                            }

                            // Update the request with the new context for next iteration
                            request.messages = self.context_window.conversation_history.clone();

                            // Ensure tools are included for native providers in subsequent iterations
                            let provider_for_tools = self.providers.get(None)?;
                            if provider_for_tools.has_native_tool_calling() {
                                request.tools = Some(Self::create_tool_definitions(
                                    self.config.webdriver.enabled,
                                    self.config.macax.enabled,
                                    self.config.computer_control.enabled,
                                ));
                            }

                            // DO NOT add final_display_content to full_response here!
                            // The content was already displayed during streaming and added to current_response.
                            // Adding it again would cause duplication when the agent message is printed.
                            // The only time we should add to full_response is:
                            // 1. For final_output tool (handled separately)
                            // 2. At the end when no tools were executed (handled in the "no tool executed" branch)

                            tool_executed = true;
                            any_tool_executed = true; // Track across all iterations

                            // Reset auto-continue attempts after successful tool execution
                            // This gives the LLM fresh attempts since it's making progress
                            auto_summary_attempts = 0;


                            // Reset the JSON tool call filter state after each tool execution
                            // This ensures the filter doesn't stay in suppression mode for subsequent streaming content
                            self.ui_writer.reset_json_filter();

                            // Only reset parser if there are no more unexecuted tool calls in the buffer
                            // This handles the case where the LLM emits multiple tool calls in one response
                            if parser.has_unexecuted_tool_call() {
                                debug!("Parser still has unexecuted tool calls, not resetting buffer");
                                // Mark current tool as consumed so we don't re-detect it
                                parser.mark_tool_calls_consumed();
                            } else {
                                // Reset parser for next iteration - this clears the text buffer
                                parser.reset();
                            }

                            // Clear current_response for next iteration to prevent buffered text
                            // from being incorrectly displayed after tool execution
                            current_response.clear();
                            // Reset response_started flag for next iteration
                            response_started = false;

                            // For single tool mode, break immediately
                            if !self.config.agent.allow_multiple_tool_calls {
                                break; // Break out of current stream to start a new one
                            }
                        } // End of for loop processing each tool call

                        // If we processed any tools in multiple mode, break out to start new stream
                        // BUT only if there are no more unexecuted tool calls in the buffer
                        if tool_executed && self.config.agent.allow_multiple_tool_calls {
                            if parser.has_unexecuted_tool_call() {
                                debug!("Tool executed but parser still has unexecuted tool calls, continuing to process");
                                // Don't break - continue processing to pick up remaining tool calls
                            } else {
                                break;
                            }
                        }

                        // If no tool calls were completed, continue streaming normally
                        if !tool_executed {
                            let clean_content = chunk
                                .content
                                .replace("<|im_end|>", "")
                                .replace("</s>", "")
                                .replace("[/INST]", "")
                                .replace("<</SYS>>", "");

                            if !clean_content.is_empty() {
                                let filtered_content =
                                    self.ui_writer.filter_json_tool_calls(&clean_content);

                                if !filtered_content.is_empty() {
                                    if !response_started {
                                        self.ui_writer.print_agent_prompt();
                                        response_started = true;
                                    }

                                    self.ui_writer.print_agent_response(&filtered_content);
                                    self.ui_writer.flush();
                                    current_response.push_str(&filtered_content);

                                    // Mark parser buffer as consumed up to current position
                                    // This prevents tool-call-like patterns in displayed text
                                    // from triggering false positives in has_unexecuted_tool_call()
                                    parser.mark_tool_calls_consumed();
                                }
                            }
                        }

                        if chunk.finished {
                            debug!("Stream finished: tool_executed={}, current_response_len={}, full_response_len={}, chunks_received={}",
                                tool_executed, current_response.len(), full_response.len(), chunks_received);

                            // Stream finished - check if we should continue or return
                            if !tool_executed {
                                // No tools were executed in this iteration
                                // Check if we got any meaningful response at all
                                // We need to check the parser's text buffer as well, since the LLM
                                // might have responded with text but no final_output tool call
                                let text_content = parser.get_text_content();
                                let has_text_response = !text_content.trim().is_empty()
                                    || !current_response.trim().is_empty();

                                // Don't re-add text from parser buffer if we already displayed it
                                // The parser buffer contains ALL accumulated text, but current_response
                                // already has what was displayed during streaming
                                if current_response.is_empty() && !text_content.trim().is_empty() {
                                    // Only use parser text if we truly have no response
                                    // This should be rare - only if streaming failed to display anything
                                    debug!("Warning: Using parser buffer text as fallback - this may duplicate output");
                                    // Extract only the undisplayed portion from parser buffer
                                    // Parser buffer accumulates across iterations, so we need to be careful
                                    let clean_text = text_content
                                        .replace("<|im_end|>", "")
                                        .replace("</s>", "")
                                        .replace("[/INST]", "")
                                        .replace("<</SYS>>", "");

                                    let filtered_text =
                                        self.ui_writer.filter_json_tool_calls(&clean_text);

                                    // Only use this if we truly have nothing else
                                    if !filtered_text.trim().is_empty() && full_response.is_empty()
                                    {
                                        debug!(
                                            "Using filtered parser text as last resort: {} chars",
                                            filtered_text.len()
                                        );
                                        // Note: This assignment is currently unused but kept for potential future use
                                        let _ = filtered_text;
                                    }
                                }

                                if !has_text_response && full_response.is_empty() {
                                    // Log detailed error information before failing
                                    error!(
                                        "=== STREAM ERROR: No content or tool calls received ==="
                                    );
                                    error!("Iteration: {}/{}", iteration_count, MAX_ITERATIONS);
                                    error!(
                                        "Provider: {} (model: {})",
                                        provider_name, provider_model
                                    );
                                    error!("Chunks received: {}", chunks_received);
                                    error!("Parser state:");
                                    error!("  - Text buffer length: {}", parser.text_buffer_len());
                                    error!(
                                        "  - Text buffer content: {:?}",
                                        parser.get_text_content()
                                    );
                                    error!("  - Has incomplete tool call: {}", parser.has_incomplete_tool_call());
                                    error!("  - Message stopped: {}", parser.is_message_stopped());
                                    error!("  - In JSON tool call: {}", parser.is_in_json_tool_call());
                                    error!("  - JSON tool start: {:?}", parser.json_tool_start_position());
                                    error!("Request details:");
                                    error!("  - Messages count: {}", request.messages.len());
                                    error!("  - Has tools: {}", request.tools.is_some());
                                    error!("  - Max tokens: {:?}", request.max_tokens);
                                    error!("  - Temperature: {:?}", request.temperature);
                                    error!("  - Stream: {}", request.stream);

                                    // Log raw chunks received
                                    error!("Raw chunks received ({} total):", chunks_received);
                                    for (i, chunk_str) in raw_chunks.iter().take(25).enumerate() {
                                        error!("  [{}] {}", i, chunk_str);
                                    }

                                    // Log the full request JSON
                                    match serde_json::to_string_pretty(&request) {
                                        Ok(json) => {
                                            error!(
                                                "(turn on DEBUG logging for the raw JSON request)"
                                            );
                                            debug!("Full request JSON:\n{}", json);
                                        }
                                        Err(e) => {
                                            error!("Failed to serialize request: {}", e);
                                        }
                                    }

                                    // Log last user message for context
                                    if let Some(last_user_msg) = request
                                        .messages
                                        .iter()
                                        .rev()
                                        .find(|m| matches!(m.role, MessageRole::User))
                                    {
                                        error!(
                                            "Last user message: {}",
                                            if last_user_msg.content.len() > 500 {
                                                format!(
                                                    "{}... (truncated)",
                                                    &last_user_msg.content[..500]
                                                )
                                            } else {
                                                last_user_msg.content.clone()
                                            }
                                        );
                                    }

                                    // Log context window state
                                    error!("Context window state:");
                                    error!(
                                        "  - Used tokens: {}/{}",
                                        self.context_window.used_tokens,
                                        self.context_window.total_tokens
                                    );
                                    error!(
                                        "  - Percentage used: {:.1}%",
                                        self.context_window.percentage_used()
                                    );
                                    error!(
                                        "  - Conversation history length: {}",
                                        self.context_window.conversation_history.len()
                                    );

                                    // Log session info
                                    error!("Session ID: {:?}", self.session_id);
                                    error!("=== END STREAM ERROR ===");

                                    // No response received - this is an error condition
                                    warn!("Stream finished without any content or tool calls");
                                    warn!("Chunks received: {}", chunks_received);
                                    return Err(anyhow::anyhow!(
                                        "No response received from the model. The model may be experiencing issues or the request may have been malformed."
                                    ));
                                }

                                // If tools were executed in previous iterations but final_output wasn't called,
                                // break to let the outer loop's auto-continue logic handle it
                                if any_tool_executed && !final_output_called {
                                    debug!("Tools were executed but final_output not called - breaking to auto-continue");
                                    // NOTE: We intentionally do NOT set full_response here.
                                    // The content was already displayed during streaming.
                                    // Setting full_response would cause duplication when the
                                    // function eventually returns.
                                    // Context window is updated separately via add_message().
                                    break;
                                }

                                // Set full_response to current_response (don't append)
                                // current_response already contains everything that was displayed
                                // Don't set full_response here - it would duplicate the output
                                // The text was already displayed during streaming
                                // Return empty string to avoid duplication
                                full_response = String::new();

                                // Save context window BEFORE returning
                                self.save_context_window("completed");
                                let _ttft =
                                    first_token_time.unwrap_or_else(|| stream_start.elapsed());

                                // Add timing if needed
                                let final_response = if show_timing {
                                    let turn_tokens = turn_accumulated_usage.as_ref().map(|u| u.total_tokens);
                                    let timing_footer = Self::format_timing_footer(
                                        stream_start.elapsed(),
                                        _ttft,
                                        turn_tokens,
                                        self.context_window.percentage_used(),
                                    );
                                    format!(
                                        "{}\n\n{}",
                                        full_response,
                                        timing_footer
                                    )
                                } else {
                                    full_response
                                };

                                return Ok(TaskResult::new(
                                    final_response,
                                    self.context_window.clone(),
                                ));
                            }
                            break; // Tool was executed, break to continue outer loop
                        }
                    }
                    Err(e) => {
                        // Capture detailed streaming error information
                        let error_msg = e.to_string();
                        let error_details = format!(
                            "Streaming error at chunk {}: {}",
                            chunks_received + 1,
                            error_msg
                        );

                        error!("Error type: {}", std::any::type_name_of_val(&e));
                        error!("Parser state at error: text_buffer_len={}, has_incomplete={}, message_stopped={}",
                            parser.text_buffer_len(), parser.has_incomplete_tool_call(), parser.is_message_stopped());

                        // Store the error for potential logging later
                        _last_error = Some(error_details.clone());

                        // Check if this is a recoverable connection error
                        let is_connection_error = error_msg.contains("unexpected EOF")
                            || error_msg.contains("connection")
                            || error_msg.contains("chunk size line")
                            || error_msg.contains("body error");

                        if is_connection_error {
                            warn!(
                                "Connection error at chunk {}, treating as end of stream",
                                chunks_received + 1
                            );
                            // If we have any content or tool calls, treat this as a graceful end
                            if chunks_received > 0
                                && (!parser.get_text_content().is_empty()
                                    || parser.has_unexecuted_tool_call())
                            {
                                warn!("Stream terminated unexpectedly but we have content, continuing");
                                break; // Break to process what we have
                            }
                        }

                        if tool_executed {
                            error!("{}", error_details);
                            warn!("Stream error after tool execution, attempting to continue");
                            break; // Break to outer loop to start new stream
                        } else {
                            // Log raw chunks before failing
                            error!("Fatal streaming error. Raw chunks received before error:");
                            for chunk_str in raw_chunks.iter().take(10) {
                                error!("  {}", chunk_str);
                            }
                            return Err(e);
                        }
                    }
                }
            }

            // Update context window with actual usage if available
            if let Some(usage) = accumulated_usage {
                debug!("Updating context window with actual usage from stream");
                self.context_window.update_usage_from_response(&usage);
            } else {
                // Fall back to estimation if no usage data was provided
                debug!("No usage data from stream, using estimation");
                let estimated_tokens = ContextWindow::estimate_tokens(&current_response);
                self.context_window.add_streaming_tokens(estimated_tokens);
            }

            // If we get here and no tool was executed, we're done
            if !tool_executed {
                // IMPORTANT: Do NOT add parser text_content here!
                // The text has already been displayed during streaming via current_response.
                // The parser buffer accumulates ALL text and would cause duplication.
                debug!("Stream completed without tool execution. Response already displayed during streaming.");
                debug!(
                    "Current response length: {}, Full response length: {}",
                    current_response.len(),
                    full_response.len()
                );

                let has_response = !current_response.is_empty() || !full_response.is_empty();

                // Check if the response is essentially empty (just whitespace or timing lines)
                // This detects cases where the LLM outputs nothing substantive
                let response_text = if !current_response.is_empty() {
                    &current_response
                } else {
                    &full_response
                };
                let is_empty_response = response_text.trim().is_empty() 
                    || response_text.lines().all(|line| line.trim().is_empty() || line.trim().starts_with("⏱️"));

                // Check if there's an incomplete tool call in the buffer
                let has_incomplete_tool_call = parser.has_incomplete_tool_call();

                // Check if there's a complete but unexecuted tool call in the buffer
                let has_unexecuted_tool_call = parser.has_unexecuted_tool_call();

                // Log when we detect unexecuted or incomplete tool calls for debugging
                if has_incomplete_tool_call {
                    debug!("Detected incomplete tool call in buffer (buffer_len={}, consumed_up_to={})",
                        parser.text_buffer_len(), parser.text_buffer_len());
                }
                if has_unexecuted_tool_call {
                    debug!("Detected unexecuted tool call in buffer - this may indicate a parsing issue");
                    warn!("Unexecuted tool call detected in buffer after stream ended");
                }

                // Auto-continue if tools were executed but final_output was never called
                // OR if the LLM emitted an incomplete tool call (truncated JSON)
                // OR if the LLM emitted a complete tool call that wasn't executed
                // This ensures we don't return control when the LLM clearly intended to call a tool
                // Note: We removed the redundant condition (any_tool_executed && is_empty_response)
                // because it's already covered by (any_tool_executed && !final_output_called)
                let should_auto_continue = (any_tool_executed && !final_output_called) 
                    || has_incomplete_tool_call 
                    || has_unexecuted_tool_call;
                if should_auto_continue {
                    if auto_summary_attempts < MAX_AUTO_SUMMARY_ATTEMPTS {
                        auto_summary_attempts += 1;
                        if has_incomplete_tool_call {
                            warn!(
                                "LLM emitted incomplete tool call ({} iterations, auto-continue attempt {}/{})",
                                iteration_count, auto_summary_attempts, MAX_AUTO_SUMMARY_ATTEMPTS
                            );
                            self.ui_writer.print_context_status(
                                "\n🔄 Model emitted incomplete tool call. Auto-continuing...\n"
                            );
                        } else if has_unexecuted_tool_call {
                            warn!(
                                "LLM emitted unexecuted tool call ({} iterations, auto-continue attempt {}/{})",
                                iteration_count, auto_summary_attempts, MAX_AUTO_SUMMARY_ATTEMPTS
                            );
                            self.ui_writer.print_context_status(
                                "\n🔄 Model emitted tool call that wasn't executed. Auto-continuing...\n"
                            );
                        } else if is_empty_response {
                            warn!(
                                "LLM emitted empty/trivial response ({} iterations, auto-continue attempt {}/{})",
                                iteration_count, auto_summary_attempts, MAX_AUTO_SUMMARY_ATTEMPTS
                            );
                            self.ui_writer.print_context_status(
                                "\n🔄 Model emitted empty response. Auto-continuing...\n"
                            );
                        } else {
                            warn!(
                                "LLM stopped without calling final_output after executing tools ({} iterations, auto-continue attempt {}/{})",
                                iteration_count, auto_summary_attempts, MAX_AUTO_SUMMARY_ATTEMPTS
                            );
                            self.ui_writer.print_context_status(
                                "\n🔄 Model stopped without calling final_output. Auto-continuing...\n"
                            );
                        }
                        
                        // Add any text response to context before prompting for continuation
                        if has_response {
                            let response_text = if !current_response.is_empty() {
                                current_response.clone()
                            } else {
                                full_response.clone()
                            };
                            if !response_text.trim().is_empty() {
                                let assistant_msg = Message::new(
                                    MessageRole::Assistant,
                                    response_text.trim().to_string(),
                                );
                                self.context_window.add_message(assistant_msg);
                            }
                        }
                        
                        // Add a follow-up message asking for continuation
                        let continue_prompt = if has_incomplete_tool_call {
                            Message::new(
                                MessageRole::User,
                                "Your previous response was cut off mid-tool-call. Please complete the tool call and continue.".to_string(),
                            )
                        } else {
                            Message::new(
                                MessageRole::User,
                                "Please continue until you are done. You **MUST** call `final_output` with a summary when done.".to_string(),
                            )
                        };
                        self.context_window.add_message(continue_prompt);
                        request.messages = self.context_window.conversation_history.clone();
                        
                        // Continue the loop
                        continue;
                    } else {
                        // Max attempts reached, give up gracefully
                        warn!(
                            "Max auto-continue attempts ({}) reached after {} iterations. Conditions: any_tool_executed={}, final_output_called={}, has_incomplete={}, has_unexecuted={}, is_empty_response={}",
                            MAX_AUTO_SUMMARY_ATTEMPTS,
                            iteration_count,
                            any_tool_executed,
                            final_output_called,
                            has_incomplete_tool_call,
                            has_unexecuted_tool_call,
                            is_empty_response
                        );
                        self.ui_writer.print_agent_response(
                            &format!("\n⚠️ The model stopped without calling final_output after {} auto-continue attempts.\n", MAX_AUTO_SUMMARY_ATTEMPTS)
                        );
                    }
                } else if has_response {
                    // Only set full_response if it's empty (first iteration without tools)
                    // This prevents duplication when the agent responds without calling final_output
                    // NOTE: We intentionally do NOT set full_response here anymore.
                    // The content was already displayed during streaming via print_agent_response().
                    // Setting full_response would cause the CLI to print it again.
                    // We only need full_response for the context window (handled separately).
                    debug!(
                        "Response already streamed, not setting full_response. current_response: {} chars",
                        current_response.len()
                    );
                }

                let _ttft = first_token_time.unwrap_or_else(|| stream_start.elapsed());

                // Add the RAW unfiltered response to context window before returning
                // This ensures the log contains the true raw content including any JSON
                if !full_response.trim().is_empty() {
                    // Get the raw text from the parser (before filtering)
                    let raw_text = parser.get_text_content();
                    let raw_clean = raw_text
                        .replace("<|im_end|>", "")
                        .replace("</s>", "")
                        .replace("[/INST]", "")
                        .replace("<</SYS>>", "");

                    if !raw_clean.trim().is_empty() {
                        let assistant_message = Message::new(MessageRole::Assistant, raw_clean);
                        self.context_window.add_message(assistant_message);
                    }
                }

                // Save context window BEFORE returning
                self.save_context_window("completed");
                
                // Add timing if needed
                let final_response = if show_timing {
                    let turn_tokens = turn_accumulated_usage.as_ref().map(|u| u.total_tokens);
                    let timing_footer = Self::format_timing_footer(
                        stream_start.elapsed(),
                        _ttft,
                        turn_tokens,
                        self.context_window.percentage_used(),
                    );
                    format!(
                        "{}\n\n{}",
                        full_response,
                        timing_footer
                    )
                } else {
                    full_response
                };

                return Ok(TaskResult::new(final_response, self.context_window.clone()));
            }

            // Continue the loop to start a new stream with updated context
        }

        // If we exit the loop due to max iterations
        let _ttft = first_token_time.unwrap_or_else(|| stream_start.elapsed());

        // Add timing if needed
        let final_response = if show_timing {
            let turn_tokens = turn_accumulated_usage.as_ref().map(|u| u.total_tokens);
            let timing_footer = Self::format_timing_footer(
                stream_start.elapsed(),
                _ttft,
                turn_tokens,
                self.context_window.percentage_used(),
            );
            format!(
                "{}\n\n{}",
                full_response,
                timing_footer
            )
        } else {
            full_response
        };

        Ok(TaskResult::new(final_response, self.context_window.clone()))
    }

    pub async fn execute_tool(&mut self, tool_call: &ToolCall) -> Result<String> {
        // Increment tool call count
        self.tool_call_count += 1;
        self.execute_tool_in_dir(tool_call, None).await
    }

    /// Execute a tool with an optional working directory (for discovery commands)
    pub async fn execute_tool_in_dir(
        &mut self,
        tool_call: &ToolCall,
        working_dir: Option<&str>,
    ) -> Result<String> {
        // Only increment tool call count if not already incremented by execute_tool
        if working_dir.is_some() {
            self.tool_call_count += 1;
        }

        let result = self.execute_tool_inner_in_dir(tool_call, working_dir).await;
        let log_str = match &result {
            Ok(s) => s.clone(),
            Err(e) => format!("ERROR: {}", e),
        };
        debug!("Tool {} completed: {}", tool_call.tool, &log_str.chars().take(100).collect::<String>());
        result
    }

    async fn execute_tool_inner_in_dir(
        &mut self,
        tool_call: &ToolCall,
        working_dir: Option<&str>,
    ) -> Result<String> {
        debug!("=== EXECUTING TOOL ===");
        debug!("Tool name: {}", tool_call.tool);
        debug!(
            "Working directory passed to execute_tool_inner_in_dir: {:?}",
            working_dir
        );
        debug!("Tool args (raw): {:?}", tool_call.args);
        debug!(
            "Tool args (JSON): {}",
            serde_json::to_string(&tool_call.args)
                .unwrap_or_else(|_| "failed to serialize".to_string())
        );
        debug!("======================");

        match tool_call.tool.as_str() {
            "shell" => {
                debug!("Processing shell tool call");
                if let Some(command) = tool_call.args.get("command") {
                    debug!("Found command parameter: {:?}", command);
                    if let Some(command_str) = command.as_str() {
                        debug!("Command string: {}", command_str);
                        // Use shell escaping to handle filenames with spaces and special characters
                        let escaped_command = shell_escape_command(command_str);

                        let executor = CodeExecutor::new();

                        // Create a receiver for streaming output
                        struct ToolOutputReceiver<'a, W: UiWriter> {
                            ui_writer: &'a W,
                        }

                        impl<'a, W: UiWriter> g3_execution::OutputReceiver for ToolOutputReceiver<'a, W> {
                            fn on_output_line(&self, line: &str) {
                                self.ui_writer.update_tool_output_line(line);
                            }
                        }

                        let receiver = ToolOutputReceiver {
                            ui_writer: &self.ui_writer,
                        };

                        debug!("ABOUT TO CALL execute_bash_streaming_in_dir: escaped_command='{}', working_dir={:?}", escaped_command, working_dir);

                        match executor
                            .execute_bash_streaming_in_dir(&escaped_command, &receiver, working_dir)
                            .await
                        {
                            Ok(result) => {
                                if result.success {
                                    Ok(if result.stdout.is_empty() {
                                        "✅ Command executed successfully".to_string()
                                    } else {
                                        result.stdout.trim().to_string()
                                    })
                                } else {
                                    Ok(format!("❌ Command failed: {}", result.stderr.trim()))
                                }
                            }
                            Err(e) => Ok(format!("❌ Execution error: {}", e)),
                        }
                    } else {
                        debug!("Command parameter is not a string: {:?}", command);
                        Ok("❌ Invalid command argument".to_string())
                    }
                } else {
                    debug!("No command parameter found in args: {:?}", tool_call.args);
                    debug!(
                        "Available keys: {:?}",
                        tool_call
                            .args
                            .as_object()
                            .map(|obj| obj.keys().collect::<Vec<_>>())
                    );
                    Ok("❌ Missing command argument".to_string())
                }
            }
            "background_process" => {
                debug!("Processing background_process tool call");
                let name = tool_call.args.get("name")
                    .and_then(|v| v.as_str());
                let name = match name {
                    Some(n) => n,
                    None => return Ok("❌ Missing 'name' argument".to_string()),
                };

                let command = tool_call.args.get("command")
                    .and_then(|v| v.as_str());
                let command = match command {
                    Some(c) => c,
                    None => return Ok("❌ Missing 'command' argument".to_string()),
                };

                // Use provided working_dir, or fall back to agent's working_dir, or current dir
                let work_dir = tool_call.args.get("working_dir")
                    .and_then(|v| v.as_str())
                    .map(|s| std::path::PathBuf::from(shellexpand::tilde(s).as_ref()))
                    .or_else(|| working_dir.map(std::path::PathBuf::from))
                    .unwrap_or_else(|| std::env::current_dir().unwrap_or_default());

                match self.background_process_manager.start(name, command, &work_dir) {
                    Ok(info) => {
                        Ok(format!(
                            "✅ Background process '{}' started\n\n\
                            **PID:** {}\n\
                            **Log file:** {}\n\
                            **Working dir:** {}\n\n\
                            To interact with this process, use the shell tool:\n\
                            - View logs: `tail -100 {}`\n\
                            - Follow logs: `tail -f {}` (blocks until Ctrl+C)\n\
                            - Check status: `ps -p {}`\n\
                            - Stop process: `kill {}`",
                            info.name, info.pid, 
                            info.log_file.display(), info.working_dir.display(),
                            info.log_file.display(), info.log_file.display(),
                            info.pid, info.pid
                        ))
                    }
                    Err(e) => Ok(format!("❌ Failed to start background process: {}", e)),
                }
            }
            "read_file" => {
                debug!("Processing read_file tool call");
                if let Some(file_path) = tool_call.args.get("file_path") {
                    if let Some(path_str) = file_path.as_str() {
                        // Expand tilde (~) to home directory
                        let expanded_path = shellexpand::tilde(path_str);
                        let path_str = expanded_path.as_ref();

                        // Check if this is an image file
                        let is_image = path_str.to_lowercase().ends_with(".png")
                            || path_str.to_lowercase().ends_with(".jpg")
                            || path_str.to_lowercase().ends_with(".jpeg")
                            || path_str.to_lowercase().ends_with(".gif")
                            || path_str.to_lowercase().ends_with(".bmp")
                            || path_str.to_lowercase().ends_with(".tiff")
                            || path_str.to_lowercase().ends_with(".tif")
                            || path_str.to_lowercase().ends_with(".webp");

                        // If it's an image file, use OCR via extract_text
                        if is_image {
                            if let Some(controller) = &self.computer_controller {
                                match controller.extract_text_from_image(path_str).await {
                                    Ok(text) => {
                                        return Ok(format!(
                                            "📄 Image file (OCR extracted):\n{}",
                                            text
                                        ));
                                    }
                                    Err(e) => {
                                        return Ok(format!(
                                            "❌ Failed to extract text from image '{}': {}",
                                            path_str, e
                                        ))
                                    }
                                }
                            } else {
                                return Ok("❌ Computer control not enabled. Cannot perform OCR on image files. Set computer_control.enabled = true in config.".to_string());
                            }
                        }

                        // Extract optional start and end positions
                        let start_char = tool_call
                            .args
                            .get("start")
                            .and_then(|v| v.as_u64())
                            .map(|n| n as usize);
                        let end_char = tool_call
                            .args
                            .get("end")
                            .and_then(|v| v.as_u64())
                            .map(|n| n as usize);

                        debug!(
                            "Reading file: {}, start={:?}, end={:?}",
                            path_str, start_char, end_char
                        );

                        match std::fs::read_to_string(path_str) {
                            Ok(content) => {
                                // Validate and apply range if specified
                                let start = start_char.unwrap_or(0);
                                let end = end_char.unwrap_or(content.len());

                                // Validation
                                if start > content.len() {
                                    return Ok(format!(
                                        "❌ Start position {} exceeds file length {}",
                                        start,
                                        content.len()
                                    ));
                                }
                                if end > content.len() {
                                    return Ok(format!(
                                        "❌ End position {} exceeds file length {}",
                                        end,
                                        content.len()
                                    ));
                                }
                                if start > end {
                                    return Ok(format!(
                                        "❌ Start position {} is greater than end position {}",
                                        start, end
                                    ));
                                }

                                // Extract the requested portion, ensuring we're at char boundaries
                                // Find the nearest valid char boundaries
                                let start_boundary = if start == 0 {
                                    0
                                } else {
                                    content
                                        .char_indices()
                                        .find(|(i, _)| *i >= start)
                                        .map(|(i, _)| i)
                                        .unwrap_or(start)
                                };
                                let end_boundary = content
                                    .char_indices()
                                    .find(|(i, _)| *i >= end)
                                    .map(|(i, _)| i)
                                    .unwrap_or(content.len());

                                let partial_content = &content[start_boundary..end_boundary];
                                let line_count = partial_content.lines().count();
                                let total_lines = content.lines().count();

                                // Format output with range info if partial
                                if start_char.is_some() || end_char.is_some() {
                                    Ok(format!(
                                        "📄 File content (chars {}-{}, {} lines of {} total):\n{}",
                                        start_boundary,
                                        end_boundary,
                                        line_count,
                                        total_lines,
                                        partial_content
                                    ))
                                } else {
                                    Ok(format!(
                                        "📄 File content ({} lines):\n{}",
                                        line_count, content
                                    ))
                                }
                            }
                            Err(e) => Ok(format!("❌ Failed to read file '{}': {}", path_str, e)),
                        }
                    } else {
                        Ok("❌ Invalid file_path argument".to_string())
                    }
                } else {
                    Ok("❌ Missing file_path argument".to_string())
                }
            }
            "read_image" => {
                debug!("Processing read_image tool call");
                
                // Get paths from file_paths array
                let mut paths: Vec<String> = Vec::new();
                
                if let Some(file_paths) = tool_call.args.get("file_paths") {
                    if let Some(arr) = file_paths.as_array() {
                        for p in arr {
                            if let Some(s) = p.as_str() {
                                paths.push(s.to_string());
                            }
                        }
                    }
                }
                
                if paths.is_empty() {
                    return Ok("❌ Missing or empty file_paths argument".to_string());
                }
                
                let mut results: Vec<String> = Vec::new();
                let mut success_count = 0;
                
                // Print └─ before images to break out of tool output box
                // Print └─ and newline before images to break out of tool output box
                println!("└─\n");
                
                for path_str in &paths {
                    // Expand tilde (~) to home directory
                    let expanded_path = shellexpand::tilde(path_str);
                    let path = std::path::Path::new(expanded_path.as_ref());
                    
                    // Check file exists
                    if !path.exists() {
                        results.push(format!("❌ Image file not found: {}", path_str));
                        continue;
                    }
                    
                    // Read the file first, then detect format from magic bytes
                    match std::fs::read(path) {
                        Ok(bytes) => {
                            // Detect media type from magic bytes (file signature)
                            let media_type = match g3_providers::ImageContent::media_type_from_bytes(&bytes) {
                                Some(mt) => mt,
                                None => {
                                    // Fall back to extension-based detection
                                    let ext = path.extension()
                                        .and_then(|e| e.to_str())
                                        .unwrap_or("");
                                    match g3_providers::ImageContent::media_type_from_extension(ext) {
                                        Some(mt) => mt,
                                        None => {
                                            results.push(format!(
                                                "❌ {}: Unsupported or unrecognized image format",
                                                path_str
                                            ));
                                            continue;
                                        }
                                    }
                                }
                            };
                            
                            let file_size = bytes.len();
                            
                            // Try to get image dimensions
                            let dimensions = Self::get_image_dimensions(&bytes, media_type);
                            
                            // Build info string
                            let dim_str = dimensions
                                .map(|(w, h)| format!("{}x{}", w, h))
                                .unwrap_or_else(|| "unknown".to_string());
                            
                            let size_str = if file_size >= 1024 * 1024 {
                                format!("{:.1} MB", file_size as f64 / (1024.0 * 1024.0))
                            } else if file_size >= 1024 {
                                format!("{:.1} KB", file_size as f64 / 1024.0)
                            } else {
                                format!("{} bytes", file_size)
                            };
                            
                            // Output imgcat inline image to terminal (height constrained)
                            // followed by info line
                            Self::print_imgcat(&bytes, path_str, &dim_str, media_type, &size_str, 5);
                            
                            // Store the image to be attached to the next user message
                            use base64::Engine;
                            let encoded = base64::engine::general_purpose::STANDARD.encode(&bytes);
                            let image = g3_providers::ImageContent::new(media_type, encoded);
                            self.pending_images.push(image);
                            
                            success_count += 1;
                        }
                        Err(e) => {
                            results.push(format!("❌ Failed to read '{}': {}", path_str, e));
                        }
                    }
                }
                
                // Print ┌─ to resume tool output box
                print!("┌─\n");
                
                let summary = if success_count == paths.len() {
                    format!("{} image(s) read.", success_count)
                } else {
                    format!("{}/{} image(s) read.", success_count, paths.len())
                };
                
                // Only include error results if there are any
                if results.is_empty() {
                    Ok(summary)
                } else {
                    Ok(format!("{}\n{}", results.join("\n"), summary))
                }
            }
            "write_file" => {
                debug!("Processing write_file tool call");
                debug!("Raw tool_call.args: {:?}", tool_call.args);
                debug!(
                    "Args as JSON: {}",
                    serde_json::to_string(&tool_call.args)
                        .unwrap_or_else(|_| "failed to serialize".to_string())
                );
                debug!(
                    "Args type: {:?}",
                    std::any::type_name_of_val(&tool_call.args)
                );
                debug!("Args is_object: {}", tool_call.args.is_object());
                debug!("Args is_array: {}", tool_call.args.is_array());
                debug!("Args is_null: {}", tool_call.args.is_null());

                // Try multiple argument formats that different providers might use
                let (path_str, content_str) = if let Some(args_obj) = tool_call.args.as_object() {
                    debug!(
                        "Args object keys: {:?}",
                        args_obj.keys().collect::<Vec<_>>()
                    );

                    // Format 1: Standard format with file_path and content
                    if let (Some(path_val), Some(content_val)) =
                        (args_obj.get("file_path"), args_obj.get("content"))
                    {
                        debug!("Found file_path and content keys");
                        if let (Some(path), Some(content)) =
                            (path_val.as_str(), content_val.as_str())
                        {
                            debug!(
                                "Successfully extracted file_path='{}', content_len={}",
                                path,
                                content.len()
                            );
                            (Some(path), Some(content))
                        } else {
                            debug!("file_path or content values are not strings: path_val={:?}, content_val={:?}", path_val, content_val);
                            (None, None)
                        }
                    }
                    // Format 2: Anthropic-style with path and content
                    else if let (Some(path_val), Some(content_val)) =
                        (args_obj.get("path"), args_obj.get("content"))
                    {
                        debug!("Found path and content keys (Anthropic style)");
                        if let (Some(path), Some(content)) =
                            (path_val.as_str(), content_val.as_str())
                        {
                            debug!(
                                "Successfully extracted path='{}', content_len={}",
                                path,
                                content.len()
                            );
                            (Some(path), Some(content))
                        } else {
                            debug!("path or content values are not strings: path_val={:?}, content_val={:?}", path_val, content_val);
                            (None, None)
                        }
                    }
                    // Format 3: Alternative naming with filename and text
                    else if let (Some(path_val), Some(content_val)) =
                        (args_obj.get("filename"), args_obj.get("text"))
                    {
                        debug!("Found filename and text keys");
                        if let (Some(path), Some(content)) =
                            (path_val.as_str(), content_val.as_str())
                        {
                            debug!(
                                "Successfully extracted filename='{}', text_len={}",
                                path,
                                content.len()
                            );
                            (Some(path), Some(content))
                        } else {
                            debug!("filename or text values are not strings: path_val={:?}, content_val={:?}", path_val, content_val);
                            (None, None)
                        }
                    }
                    // Format 4: Alternative naming with file and data
                    else if let (Some(path_val), Some(content_val)) =
                        (args_obj.get("file"), args_obj.get("data"))
                    {
                        debug!("Found file and data keys");
                        if let (Some(path), Some(content)) =
                            (path_val.as_str(), content_val.as_str())
                        {
                            debug!(
                                "Successfully extracted file='{}', data_len={}",
                                path,
                                content.len()
                            );
                            (Some(path), Some(content))
                        } else {
                            debug!("file or data values are not strings: path_val={:?}, content_val={:?}", path_val, content_val);
                            (None, None)
                        }
                    } else {
                        debug!(
                            "No matching key patterns found. Available argument keys: {:?}",
                            args_obj.keys().collect::<Vec<_>>()
                        );
                        (None, None)
                    }
                } else {
                    debug!("Args is not an object, checking if it's an array");
                    // Format 5: Args might be an array [path, content]
                    if let Some(args_array) = tool_call.args.as_array() {
                        debug!("Args is an array with {} elements", args_array.len());
                        if args_array.len() >= 2 {
                            if let (Some(path), Some(content)) =
                                (args_array[0].as_str(), args_array[1].as_str())
                            {
                                debug!(
                                    "Successfully extracted from array: path='{}', content_len={}",
                                    path,
                                    content.len()
                                );
                                (Some(path), Some(content))
                            } else {
                                debug!(
                                    "Array elements are not strings: [0]={:?}, [1]={:?}",
                                    args_array[0], args_array[1]
                                );
                                (None, None)
                            }
                        } else {
                            debug!("Array has insufficient elements: {}", args_array.len());
                            (None, None)
                        }
                    } else {
                        debug!("Args is neither object nor array");
                        (None, None)
                    }
                };

                debug!(
                    "Final extracted values: path_str={:?}, content_str_len={:?}",
                    path_str,
                    content_str.map(|c| c.len())
                );

                if let (Some(path), Some(content)) = (path_str, content_str) {
                    // Expand tilde (~) to home directory
                    let expanded_path = shellexpand::tilde(path);
                    let path = expanded_path.as_ref();

                    debug!("Writing to file: {}", path);

                    // Create parent directories if they don't exist
                    if let Some(parent) = std::path::Path::new(path).parent() {
                        if let Err(e) = std::fs::create_dir_all(parent) {
                            return Ok(format!(
                                "❌ Failed to create parent directories for '{}': {}",
                                path, e
                            ));
                        }
                    }

                    match std::fs::write(path, content) {
                        Ok(()) => {
                            let line_count = content.lines().count();
                            let char_count = content.len();
                            Ok(format!(
                                "✅ Successfully wrote {} lines ({} characters)",
                                line_count, char_count
                            ))
                        }
                        Err(e) => Ok(format!("❌ Failed to write to file '{}': {}", path, e)),
                    }
                } else {
                    // Provide more detailed error information
                    let available_keys = if let Some(obj) = tool_call.args.as_object() {
                        obj.keys().collect::<Vec<_>>()
                    } else {
                        vec![]
                    };

                    Ok(format!(
                        "❌ Missing file_path or content argument. Available keys: {:?}. Expected formats: {{\"file_path\": \"...\", \"content\": \"...\"}}, {{\"path\": \"...\", \"content\": \"...\"}}, {{\"filename\": \"...\", \"text\": \"...\"}}, or {{\"file\": \"...\", \"data\": \"...\"}}",
                        available_keys
                    ))
                }
            }
            "str_replace" => {
                debug!("Processing str_replace tool call");

                // Extract arguments
                let args_obj = match tool_call.args.as_object() {
                    Some(obj) => obj,
                    None => return Ok("❌ Invalid arguments: expected object".to_string()),
                };

                let file_path = match args_obj.get("file_path").and_then(|v| v.as_str()) {
                    Some(path) => {
                        // Expand tilde (~) to home directory
                        let expanded_path = shellexpand::tilde(path);
                        expanded_path.into_owned()
                    }
                    None => return Ok("❌ Missing or invalid file_path argument".to_string()),
                };

                let diff = match args_obj.get("diff").and_then(|v| v.as_str()) {
                    Some(d) => d,
                    None => return Ok("❌ Missing or invalid diff argument".to_string()),
                };

                // Optional start and end character positions (0-indexed, end is EXCLUSIVE)
                let start_char = args_obj
                    .get("start")
                    .and_then(|v| v.as_u64())
                    .map(|n| n as usize);
                let end_char = args_obj
                    .get("end")
                    .and_then(|v| v.as_u64())
                    .map(|n| n as usize);

                debug!(
                    "str_replace: path={}, start={:?}, end={:?}",
                    file_path, start_char, end_char
                );

                // Read the existing file
                let file_content = match std::fs::read_to_string(&file_path) {
                    Ok(content) => content,
                    Err(e) => return Ok(format!("❌ Failed to read file '{}': {}", file_path, e)),
                };

                // Apply unified diff to content
                let result =
                    match apply_unified_diff_to_string(&file_content, diff, start_char, end_char) {
                        Ok(r) => r,
                        Err(e) => return Ok(format!("❌ {}", e)),
                    };

                // Write the result back to the file
                match std::fs::write(&file_path, &result) {
                    Ok(()) => Ok("✅ applied unified diff".to_string()),
                    Err(e) => Ok(format!("❌ Failed to write to file '{}': {}", file_path, e)),
                }
            }
            "final_output" => {
                if let Some(summary) = tool_call.args.get("summary") {
                    if let Some(summary_str) = summary.as_str() {
                        // Save session continuation artifact
                        self.save_session_continuation(Some(summary_str.to_string()));
                        Ok(summary_str.to_string())
                    } else {
                        self.save_session_continuation(None);
                        Ok("✅ Turn completed".to_string())
                    }
                } else {
                    self.save_session_continuation(None);
                    Ok("✅ Turn completed".to_string())
                }
            }
            "take_screenshot" => {
                if let Some(controller) = &self.computer_controller {
                    let path = tool_call
                        .args
                        .get("path")
                        .and_then(|v| v.as_str())
                        .ok_or_else(|| anyhow::anyhow!("Missing path argument"))?;

                    // Extract window_id (app name) - REQUIRED
                    let window_id = tool_call.args.get("window_id").and_then(|v| v.as_str())
                        .ok_or_else(|| anyhow::anyhow!("Missing window_id argument. You must specify which window to capture (e.g., 'Safari', 'Terminal', 'Google Chrome')."))?;

                    // Extract region if provided
                    let region = tool_call
                        .args
                        .get("region")
                        .and_then(|v| v.as_object())
                        .map(|region_obj| g3_computer_control::types::Rect {
                            x: region_obj.get("x").and_then(|v| v.as_i64()).unwrap_or(0) as i32,
                            y: region_obj.get("y").and_then(|v| v.as_i64()).unwrap_or(0) as i32,
                            width: region_obj
                                .get("width")
                                .and_then(|v| v.as_i64())
                                .unwrap_or(0) as i32,
                            height: region_obj
                                .get("height")
                                .and_then(|v| v.as_i64())
                                .unwrap_or(0) as i32,
                        });

                    match controller
                        .take_screenshot(path, region, Some(window_id))
                        .await
                    {
                        Ok(_) => {
                            // Get the actual path where the screenshot was saved
                            let actual_path = if path.starts_with('/') {
                                path.to_string()
                            } else {
                                let temp_dir = std::env::var("TMPDIR")
                                    .or_else(|_| {
                                        std::env::var("HOME").map(|h| format!("{}/tmp", h))
                                    })
                                    .unwrap_or_else(|_| "/tmp".to_string());
                                format!("{}/{}", temp_dir.trim_end_matches('/'), path)
                            };

                            Ok(format!(
                                "✅ Screenshot of {} saved to: {}",
                                window_id, actual_path
                            ))
                        }
                        Err(e) => Ok(format!("❌ Failed to take screenshot: {}", e)),
                    }
                } else {
                    Ok("❌ Computer control not enabled. Set computer_control.enabled = true in config.".to_string())
                }
            }
            "extract_text" => {
                if let Some(controller) = &self.computer_controller {
                    let path = tool_call
                        .args
                        .get("path")
                        .and_then(|v| v.as_str())
                        .ok_or_else(|| anyhow::anyhow!("Missing path argument"))?;

                    // Extract text from image file only
                    match controller.extract_text_from_image(path).await {
                        Ok(text) => Ok(format!("✅ Extracted text:\n{}", text)),
                        Err(e) => Ok(format!("❌ Failed to extract text: {}", e)),
                    }
                } else {
                    Ok("❌ Computer control not enabled. Set computer_control.enabled = true in config.".to_string())
                }
            }
            "todo_read" => {
                debug!("Processing todo_read tool call");
                // Read from session-specific todo.g3.md if we have a session, else fall back to workspace
                let todo_path = if let Some(ref session_id) = self.session_id {
                    let _ = ensure_session_dir(session_id); // Ensure dir exists
                    get_session_todo_path(session_id)
                } else {
                    get_todo_path()
                };

                if !todo_path.exists() {
                    // Also update in-memory content to stay in sync
                    let mut todo = self.todo_content.write().await;
                    *todo = String::new();
                    Ok("📝 TODO list is empty (no todo.g3.md file found)".to_string())
                } else {
                    match std::fs::read_to_string(&todo_path) {
                        Ok(content) => {
                            // Update in-memory content to stay in sync
                            let mut todo = self.todo_content.write().await;
                            *todo = content.clone();

                            // Check for staleness if enabled and we have a requirements SHA
                            if self.config.agent.check_todo_staleness {
                                if let Some(req_sha) = &self.requirements_sha {
                                    // Parse the first line for the SHA header
                                    if let Some(first_line) = content.lines().next() {
                                        if first_line.starts_with(
                                            "{{Based on the requirements file with SHA256:",
                                        ) {
                                            let parts: Vec<&str> =
                                                first_line.split("SHA256:").collect();
                                            if parts.len() > 1 {
                                                let todo_sha =
                                                    parts[1].trim().trim_end_matches("}}").trim();
                                                if todo_sha != req_sha {
                                                    let warning = format!(
                                                        "⚠️ TODO list is stale! It was generated from a different requirements file.\nExpected SHA: {}\nFound SHA:    {}",
                                                        req_sha, todo_sha
                                                    );
                                                    self.ui_writer.print_context_status(&warning);

                                                    // Beep 6 times
                                                    print!("\x07\x07\x07\x07\x07\x07");
                                                    let _ = std::io::stdout().flush();

                                                    let options = [
                                                        "Ignore and Continue",
                                                        "Mark as Stale",
                                                        "Quit Application",
                                                    ];
                                                    let choice = self.ui_writer.prompt_user_choice("Requirements have changed! What would you like to do?", &options);

                                                    match choice {
                                                        0 => {
                                                            // Ignore and Continue
                                                            self.ui_writer.print_context_status(
                                                                "⚠️ Ignoring staleness warning.",
                                                            );
                                                        }
                                                        1 => {
                                                            // Mark as Stale
                                                            // We return a message to the agent so it knows to regenerate/fix it.
                                                            return Ok("⚠️ TODO list is stale (requirements changed). Please regenerate the TODO list to match the new requirements.".to_string());
                                                        }
                                                        2 => {
                                                            // Quit Application
                                                            self.ui_writer.print_context_status("❌ Quitting application as requested.");
                                                            std::process::exit(0);
                                                        }
                                                        _ => unreachable!(),
                                                    }
                                                }
                                            }
                                        } else {
                                            // Header missing, but we have a SHA. Warn the user?
                                            // For now, maybe just proceed... assuming it's an old TODO.
                                        }
                                    }
                                }
                            }

                            if content.trim().is_empty() {
                                Ok("📝 TODO list is empty".to_string())
                            } else {
                                for line in content.lines() {
                                    self.ui_writer.print_tool_output_line(line);
                                }
                                Ok(format!("📝 TODO list:\n{}", content))
                            }
                        }
                        Err(e) => Ok(format!("❌ Failed to read TODO.md: {}", e)),
                    }
                }
            }
            "todo_write" => {
                debug!("Processing todo_write tool call");
                if let Some(content) = tool_call.args.get("content") {
                    if let Some(content_str) = content.as_str() {
                        let char_count = content_str.chars().count();
                        let max_chars = std::env::var("G3_TODO_MAX_CHARS")
                            .ok()
                            .and_then(|s| s.parse().ok())
                            .unwrap_or(50_000);

                        if max_chars > 0 && char_count > max_chars {
                            return Ok(format!(
                                "❌ TODO list too large: {} chars (max: {})",
                                char_count, max_chars
                            ));
                        }

                        // Check if all todos are completed (all checkboxes are checked)
                        let has_incomplete = content_str.lines().any(|line| {
                            let trimmed = line.trim();
                            trimmed.starts_with("- [ ]")
                        });

                        // If all todos are complete, delete the file instead of writing
                        // EXCEPT in planner mode (G3_TODO_PATH is set) - preserve for rename to completed_todo_*.md
                        let in_planner_mode = std::env::var("G3_TODO_PATH").is_ok();
                        let todo_path = if let Some(ref session_id) = self.session_id {
                            let _ = ensure_session_dir(session_id); // Ensure dir exists
                            get_session_todo_path(session_id)
                        } else {
                            get_todo_path()
                        };
                        
                        if !in_planner_mode && !has_incomplete && (content_str.contains("- [x]") || content_str.contains("- [X]")) {
                            if todo_path.exists() {
                                match std::fs::remove_file(&todo_path) {
                                    Ok(_) => {
                                        let mut todo = self.todo_content.write().await;
                                        *todo = String::new();
                                        // Show the final completed TODOs before deletion
                                        let mut result = String::from("✅ All TODOs completed! Removed todo.g3.md\n\nFinal status:\n");
                                        for line in content_str.lines() {
                                            self.ui_writer.print_tool_output_line(line);
                                            result.push_str(line);
                                            result.push('\n');
                                        }
                                        return Ok(result);
                                    }
                                    Err(e) => return Ok(format!("❌ Failed to remove todo.g3.md: {}", e)),
                                }
                            }
                        }

                        match std::fs::write(&todo_path, content_str) {
                            Ok(_) => {
                                // Also update in-memory content to stay in sync
                                let mut todo = self.todo_content.write().await;
                                *todo = content_str.to_string();
                                // Print the TODO content to the console (inside the tool frame)
                                for line in content_str.lines() {
                                    self.ui_writer.print_tool_output_line(line);
                                }
                                Ok(format!("✅ TODO list updated ({} chars) and saved to todo.g3.md:\n{}", char_count, content_str))
                            }
                            Err(e) => Ok(format!("❌ Failed to write todo.g3.md: {}", e)),
                        }
                    } else {
                        Ok("❌ Invalid content argument".to_string())
                    }
                } else {
                    Ok("❌ Missing content argument".to_string())
                }
            }
            "code_coverage" => {
                debug!("Processing code_coverage tool call");
                self.ui_writer
                    .print_context_status("🔍 Generating code coverage report...");

                // Ensure coverage tools are installed
                match g3_execution::ensure_coverage_tools_installed() {
                    Ok(already_installed) => {
                        if !already_installed {
                            self.ui_writer
                                .print_context_status("✅ Coverage tools installed successfully");
                        }
                    }
                    Err(e) => {
                        return Ok(format!("❌ Failed to install coverage tools: {}", e));
                    }
                }

                // Run cargo llvm-cov --workspace
                let output = std::process::Command::new("cargo")
                    .args(&["llvm-cov", "--workspace"])
                    .current_dir(std::env::current_dir()?)
                    .output()?;

                if output.status.success() {
                    let stdout = String::from_utf8_lossy(&output.stdout);
                    let stderr = String::from_utf8_lossy(&output.stderr);

                    // Combine output
                    let mut result =
                        String::from("✅ Code coverage report generated successfully\n\n");
                    result.push_str("## Coverage Summary\n");
                    result.push_str(&stdout);
                    if !stderr.is_empty() {
                        result.push_str("\n## Warnings\n");
                        result.push_str(&stderr);
                    }
                    Ok(result)
                } else {
                    let stderr = String::from_utf8_lossy(&output.stderr);
                    Ok(format!(
                        "❌ Failed to generate coverage report:\n{}",
                        stderr
                    ))
                }
            }
            "webdriver_start" => {
                debug!("Processing webdriver_start tool call");

                if !self.config.webdriver.enabled {
                    return Ok(
                        "❌ WebDriver is not enabled. Use --webdriver flag to enable.".to_string(),
                    );
                }

                // Check if session already exists
                let session_guard = self.webdriver_session.read().await;
                if session_guard.is_some() {
                    drop(session_guard);
                    return Ok("✅ WebDriver session already active".to_string());
                }
                drop(session_guard);

                // Determine which browser to use based on config
                use g3_config::WebDriverBrowser;
                match &self.config.webdriver.browser {
                    WebDriverBrowser::Safari => {
                        // Note: Safari Remote Automation must be enabled before using WebDriver.
                        // Run this once: safaridriver --enable
                        // Or enable manually: Safari → Develop → Allow Remote Automation

                        let port = self.config.webdriver.safari_port;

                        let driver_result = tokio::process::Command::new("safaridriver")
                            .arg("--port")
                            .arg(port.to_string())
                            .stdout(std::process::Stdio::null())
                            .stderr(std::process::Stdio::null())
                            .spawn();

                        let mut webdriver_process = match driver_result {
                            Ok(process) => process,
                            Err(e) => {
                                return Ok(format!("❌ Failed to start safaridriver: {}\n\nMake sure safaridriver is installed.", e));
                            }
                        };

                        // Wait for safaridriver to start up
                        tokio::time::sleep(tokio::time::Duration::from_millis(1000)).await;

                        // Connect to SafariDriver
                        match g3_computer_control::SafariDriver::with_port(port).await {
                            Ok(driver) => {
                                let session = std::sync::Arc::new(tokio::sync::Mutex::new(WebDriverSession::Safari(driver)));
                                *self.webdriver_session.write().await = Some(session);
                                *self.webdriver_process.write().await = Some(webdriver_process);

                                Ok("✅ WebDriver session started successfully! Safari should open automatically.".to_string())
                            }
                            Err(e) => {
                                let _ = webdriver_process.kill().await;
                                Ok(format!("❌ Failed to connect to SafariDriver: {}\n\nThis might be because:\n  - Safari Remote Automation is not enabled (run: safaridriver --enable)\n  - Port {} is already in use\n  - Safari failed to start\n  - Network connectivity issue\n\nTo enable Remote Automation:\n  1. Run: safaridriver --enable (requires password, one-time setup)\n  2. Or manually: Safari → Develop → Allow Remote Automation", e, port))
                            }
                        }
                    }
                    WebDriverBrowser::ChromeHeadless => {
                        let port = self.config.webdriver.chrome_port;

                        // Start chromedriver process
                        let driver_result = tokio::process::Command::new("chromedriver")
                            .arg(format!("--port={}", port))
                            .stdout(std::process::Stdio::null())
                            .stderr(std::process::Stdio::null())
                            .spawn();

                        let mut webdriver_process = match driver_result {
                            Ok(process) => process,
                            Err(e) => {
                                return Ok(format!("❌ Failed to start chromedriver: {}\n\nMake sure chromedriver is installed and in your PATH.\n\nInstall with:\n  - macOS: brew install chromedriver\n  - Linux: apt install chromium-chromedriver\n  - Or download from: https://chromedriver.chromium.org/downloads", e));
                            }
                        };

                        // Wait for chromedriver to be ready with retry loop
                        let max_retries = 10;
                        let mut last_error = None;
                        
                        for attempt in 0..max_retries {
                            // Wait before each attempt (200ms between retries, total max ~2s)
                            tokio::time::sleep(tokio::time::Duration::from_millis(200)).await;
                            
                            // Try to connect to ChromeDriver in headless mode (with optional custom binary)
                            let driver_result = match &self.config.webdriver.chrome_binary {
                                Some(binary) => g3_computer_control::ChromeDriver::with_port_headless_and_binary(port, Some(binary)).await,
                                None => g3_computer_control::ChromeDriver::with_port_headless(port).await,
                            };
                            
                            match driver_result {
                                Ok(driver) => {
                                    let session = std::sync::Arc::new(tokio::sync::Mutex::new(WebDriverSession::Chrome(driver)));
                                    *self.webdriver_session.write().await = Some(session);
                                    *self.webdriver_process.write().await = Some(webdriver_process);

                                    return Ok("✅ WebDriver session started successfully! Chrome is running in headless mode (no visible window).".to_string());
                                }
                                Err(e) => {
                                    last_error = Some(e);
                                    if attempt < max_retries - 1 {
                                        // Continue retrying
                                        continue;
                                    }
                                }
                            }
                        }

                        // All retries failed
                        let _ = webdriver_process.kill().await;
                        let error_msg = last_error.map(|e| e.to_string()).unwrap_or_else(|| "Unknown error".to_string());
                        Ok(format!("❌ Failed to connect to ChromeDriver after {} attempts: {}\n\nThis might be because:\n  - Chrome is not installed\n  - ChromeDriver version doesn't match Chrome version\n  - Port {} is already in use\n\nMake sure Chrome and ChromeDriver are installed and compatible.", max_retries, error_msg, port))
                    }
                }
            }
            "webdriver_navigate" => {
                debug!("Processing webdriver_navigate tool call");

                if !self.config.webdriver.enabled {
                    return Ok(
                        "❌ WebDriver is not enabled. Use --webdriver flag to enable.".to_string(),
                    );
                }

                let session_guard = self.webdriver_session.read().await;
                let session = match session_guard.as_ref() {
                    Some(s) => s.clone(),
                    None => {
                        return Ok(
                            "❌ No active WebDriver session. Call webdriver_start first."
                                .to_string(),
                        )
                    }
                };
                drop(session_guard);
                let url = match tool_call.args.get("url").and_then(|v| v.as_str()) {
                    Some(u) => u,
                    None => return Ok("❌ Missing url argument".to_string()),
                };

                let mut driver = session.lock().await;
                match driver.navigate(url).await {
                    Ok(_) => Ok(format!("✅ Navigated to {}", url)),
                    Err(e) => Ok(format!("❌ Failed to navigate: {}", e)),
                }
            }
            "webdriver_get_url" => {
                debug!("Processing webdriver_get_url tool call");

                if !self.config.webdriver.enabled {
                    return Ok(
                        "❌ WebDriver is not enabled. Use --webdriver flag to enable.".to_string(),
                    );
                }

                let session_guard = self.webdriver_session.read().await;
                let session = match session_guard.as_ref() {
                    Some(s) => s.clone(),
                    None => {
                        return Ok(
                            "❌ No active WebDriver session. Call webdriver_start first."
                                .to_string(),
                        )
                    }
                };

                let driver = session.lock().await;
                match driver.current_url().await {
                    Ok(url) => Ok(format!("Current URL: {}", url)),
                    Err(e) => Ok(format!("❌ Failed to get URL: {}", e)),
                }
            }
            "webdriver_get_title" => {
                debug!("Processing webdriver_get_title tool call");

                if !self.config.webdriver.enabled {
                    return Ok(
                        "❌ WebDriver is not enabled. Use --webdriver flag to enable.".to_string(),
                    );
                }

                let session_guard = self.webdriver_session.read().await;
                let session = match session_guard.as_ref() {
                    Some(s) => s.clone(),
                    None => {
                        return Ok(
                            "❌ No active WebDriver session. Call webdriver_start first."
                                .to_string(),
                        )
                    }
                };

                let driver = session.lock().await;
                match driver.title().await {
                    Ok(title) => Ok(format!("Page title: {}", title)),
                    Err(e) => Ok(format!("❌ Failed to get title: {}", e)),
                }
            }
            "webdriver_find_element" => {
                debug!("Processing webdriver_find_element tool call");

                if !self.config.webdriver.enabled {
                    return Ok(
                        "❌ WebDriver is not enabled. Use --webdriver flag to enable.".to_string(),
                    );
                }

                let session_guard = self.webdriver_session.read().await;
                let session = match session_guard.as_ref() {
                    Some(s) => s.clone(),
                    None => {
                        return Ok(
                            "❌ No active WebDriver session. Call webdriver_start first."
                                .to_string(),
                        )
                    }
                };

                let selector = match tool_call.args.get("selector").and_then(|v| v.as_str()) {
                    Some(s) => s,
                    None => return Ok("❌ Missing selector argument".to_string()),
                };

                let mut driver = session.lock().await;
                match driver.find_element(selector).await {
                    Ok(elem) => match elem.text().await {
                        Ok(text) => Ok(format!("Element text: {}", text)),
                        Err(e) => Ok(format!("❌ Failed to get element text: {}", e)),
                    },
                    Err(e) => Ok(format!("❌ Failed to find element '{}': {}", selector, e)),
                }
            }
            "webdriver_find_elements" => {
                debug!("Processing webdriver_find_elements tool call");

                if !self.config.webdriver.enabled {
                    return Ok(
                        "❌ WebDriver is not enabled. Use --webdriver flag to enable.".to_string(),
                    );
                }

                let session_guard = self.webdriver_session.read().await;
                let session = match session_guard.as_ref() {
                    Some(s) => s.clone(),
                    None => {
                        return Ok(
                            "❌ No active WebDriver session. Call webdriver_start first."
                                .to_string(),
                        )
                    }
                };

                let selector = match tool_call.args.get("selector").and_then(|v| v.as_str()) {
                    Some(s) => s,
                    None => return Ok("❌ Missing selector argument".to_string()),
                };

                let mut driver = session.lock().await;
                match driver.find_elements(selector).await {
                    Ok(elements) => {
                        let mut results = Vec::new();
                        for (i, elem) in elements.iter().enumerate() {
                            match elem.text().await {
                                Ok(text) => results.push(format!("[{}]: {}", i, text)),
                                Err(_) => results.push(format!("[{}]: <error getting text>", i)),
                            }
                        }
                        Ok(format!(
                            "Found {} elements:\n{}",
                            results.len(),
                            results.join("\n")
                        ))
                    }
                    Err(e) => Ok(format!("❌ Failed to find elements '{}': {}", selector, e)),
                }
            }
            "webdriver_click" => {
                debug!("Processing webdriver_click tool call");

                if !self.config.webdriver.enabled {
                    return Ok(
                        "❌ WebDriver is not enabled. Use --webdriver flag to enable.".to_string(),
                    );
                }

                let session_guard = self.webdriver_session.read().await;
                let session = match session_guard.as_ref() {
                    Some(s) => s.clone(),
                    None => {
                        return Ok(
                            "❌ No active WebDriver session. Call webdriver_start first."
                                .to_string(),
                        )
                    }
                };

                let selector = match tool_call.args.get("selector").and_then(|v| v.as_str()) {
                    Some(s) => s,
                    None => return Ok("❌ Missing selector argument".to_string()),
                };

                let mut driver = session.lock().await;
                match driver.find_element(selector).await {
                    Ok(mut elem) => match elem.click().await {
                        Ok(_) => Ok(format!("✅ Clicked element '{}'", selector)),
                        Err(e) => Ok(format!("❌ Failed to click element: {}", e)),
                    },
                    Err(e) => Ok(format!("❌ Failed to find element '{}': {}", selector, e)),
                }
            }
            "webdriver_send_keys" => {
                debug!("Processing webdriver_send_keys tool call");

                if !self.config.webdriver.enabled {
                    return Ok(
                        "❌ WebDriver is not enabled. Use --webdriver flag to enable.".to_string(),
                    );
                }

                let session_guard = self.webdriver_session.read().await;
                let session = match session_guard.as_ref() {
                    Some(s) => s.clone(),
                    None => {
                        return Ok(
                            "❌ No active WebDriver session. Call webdriver_start first."
                                .to_string(),
                        )
                    }
                };

                let selector = match tool_call.args.get("selector").and_then(|v| v.as_str()) {
                    Some(s) => s,
                    None => return Ok("❌ Missing selector argument".to_string()),
                };

                let text = match tool_call.args.get("text").and_then(|v| v.as_str()) {
                    Some(t) => t,
                    None => return Ok("❌ Missing text argument".to_string()),
                };

                let clear_first = tool_call
                    .args
                    .get("clear_first")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(true);

                let mut driver = session.lock().await;
                match driver.find_element(selector).await {
                    Ok(mut elem) => {
                        if clear_first {
                            if let Err(e) = elem.clear().await {
                                return Ok(format!("❌ Failed to clear element: {}", e));
                            }
                        }
                        match elem.send_keys(text).await {
                            Ok(_) => Ok(format!("✅ Sent keys to element '{}'", selector)),
                            Err(e) => Ok(format!("❌ Failed to send keys: {}", e)),
                        }
                    }
                    Err(e) => Ok(format!("❌ Failed to find element '{}': {}", selector, e)),
                }
            }
            "webdriver_execute_script" => {
                debug!("Processing webdriver_execute_script tool call");

                if !self.config.webdriver.enabled {
                    return Ok(
                        "❌ WebDriver is not enabled. Use --webdriver flag to enable.".to_string(),
                    );
                }

                let session_guard = self.webdriver_session.read().await;
                let session = match session_guard.as_ref() {
                    Some(s) => s.clone(),
                    None => {
                        return Ok(
                            "❌ No active WebDriver session. Call webdriver_start first."
                                .to_string(),
                        )
                    }
                };

                let script = match tool_call.args.get("script").and_then(|v| v.as_str()) {
                    Some(s) => s,
                    None => return Ok("❌ Missing script argument".to_string()),
                };

                let mut driver = session.lock().await;
                match driver.execute_script(script, vec![]).await {
                    Ok(result) => Ok(format!("Script result: {:?}", result)),
                    Err(e) => Ok(format!("❌ Failed to execute script: {}", e)),
                }
            }
            "webdriver_get_page_source" => {
                debug!("Processing webdriver_get_page_source tool call");

                if !self.config.webdriver.enabled {
                    return Ok(
                        "❌ WebDriver is not enabled. Use --webdriver flag to enable.".to_string(),
                    );
                }

                // Extract optional parameters
                let max_length = tool_call
                    .args
                    .get("max_length")
                    .and_then(|v| v.as_u64())
                    .map(|n| n as usize)
                    .unwrap_or(10000);

                let save_to_file = tool_call
                    .args
                    .get("save_to_file")
                    .and_then(|v| v.as_str());

                let session_guard = self.webdriver_session.read().await;
                let session = match session_guard.as_ref() {
                    Some(s) => s.clone(),
                    None => {
                        return Ok(
                            "❌ No active WebDriver session. Call webdriver_start first."
                                .to_string(),
                        )
                    }
                };

                let driver = session.lock().await;
                match driver.page_source().await {
                    Ok(source) => {
                        // If save_to_file is specified, write to file
                        if let Some(file_path) = save_to_file {
                            let expanded_path = shellexpand::tilde(file_path);
                            let path_str = expanded_path.as_ref();

                            // Create parent directories if needed
                            if let Some(parent) = std::path::Path::new(path_str).parent() {
                                if let Err(e) = std::fs::create_dir_all(parent) {
                                    return Ok(format!("❌ Failed to create directories: {}", e));
                                }
                            }

                            match std::fs::write(path_str, &source) {
                                Ok(_) => Ok(format!(
                                    "✅ Page source ({} chars) saved to: {}",
                                    source.len(),
                                    path_str
                                )),
                                Err(e) => Ok(format!("❌ Failed to write file: {}", e)),
                            }
                        } else if max_length > 0 && source.len() > max_length {
                            // Truncate if max_length is set and source exceeds it
                            Ok(format!(
                                "Page source ({} chars, truncated to {}):\n{}...",
                                source.len(),
                                max_length,
                                &source[..max_length]
                            ))
                        } else {
                            // Return full source
                            Ok(format!("Page source ({} chars):\n{}", source.len(), source))
                        }
                    }
                    Err(e) => Ok(format!("❌ Failed to get page source: {}", e)),
                }
            }
            "webdriver_screenshot" => {
                debug!("Processing webdriver_screenshot tool call");

                if !self.config.webdriver.enabled {
                    return Ok(
                        "❌ WebDriver is not enabled. Use --webdriver flag to enable.".to_string(),
                    );
                }

                let session_guard = self.webdriver_session.read().await;
                let session = match session_guard.as_ref() {
                    Some(s) => s.clone(),
                    None => {
                        return Ok(
                            "❌ No active WebDriver session. Call webdriver_start first."
                                .to_string(),
                        )
                    }
                };

                let path = match tool_call.args.get("path").and_then(|v| v.as_str()) {
                    Some(p) => p,
                    None => return Ok("❌ Missing path argument".to_string()),
                };

                let mut driver = session.lock().await;
                match driver.screenshot(path).await {
                    Ok(_) => Ok(format!("✅ Screenshot saved to {}", path)),
                    Err(e) => Ok(format!("❌ Failed to take screenshot: {}", e)),
                }
            }
            "webdriver_back" => {
                debug!("Processing webdriver_back tool call");

                if !self.config.webdriver.enabled {
                    return Ok(
                        "❌ WebDriver is not enabled. Use --webdriver flag to enable.".to_string(),
                    );
                }

                let session_guard = self.webdriver_session.read().await;
                let session = match session_guard.as_ref() {
                    Some(s) => s.clone(),
                    None => {
                        return Ok(
                            "❌ No active WebDriver session. Call webdriver_start first."
                                .to_string(),
                        )
                    }
                };

                let mut driver = session.lock().await;
                match driver.back().await {
                    Ok(_) => Ok("✅ Navigated back".to_string()),
                    Err(e) => Ok(format!("❌ Failed to navigate back: {}", e)),
                }
            }
            "webdriver_forward" => {
                debug!("Processing webdriver_forward tool call");

                if !self.config.webdriver.enabled {
                    return Ok(
                        "❌ WebDriver is not enabled. Use --webdriver flag to enable.".to_string(),
                    );
                }

                let session_guard = self.webdriver_session.read().await;
                let session = match session_guard.as_ref() {
                    Some(s) => s.clone(),
                    None => {
                        return Ok(
                            "❌ No active WebDriver session. Call webdriver_start first."
                                .to_string(),
                        )
                    }
                };

                let mut driver = session.lock().await;
                match driver.forward().await {
                    Ok(_) => Ok("✅ Navigated forward".to_string()),
                    Err(e) => Ok(format!("❌ Failed to navigate forward: {}", e)),
                }
            }
            "webdriver_refresh" => {
                debug!("Processing webdriver_refresh tool call");

                if !self.config.webdriver.enabled {
                    return Ok(
                        "❌ WebDriver is not enabled. Use --webdriver flag to enable.".to_string(),
                    );
                }

                let session_guard = self.webdriver_session.read().await;
                let session = match session_guard.as_ref() {
                    Some(s) => s.clone(),
                    None => {
                        return Ok(
                            "❌ No active WebDriver session. Call webdriver_start first."
                                .to_string(),
                        )
                    }
                };

                let mut driver = session.lock().await;
                match driver.refresh().await {
                    Ok(_) => Ok("✅ Page refreshed".to_string()),
                    Err(e) => Ok(format!("❌ Failed to refresh page: {}", e)),
                }
            }
            "webdriver_quit" => {
                debug!("Processing webdriver_quit tool call");

                if !self.config.webdriver.enabled {
                    return Ok(
                        "❌ WebDriver is not enabled. Use --webdriver flag to enable.".to_string(),
                    );
                }

                // Take the session
                let session = match self.webdriver_session.write().await.take() {
                    Some(s) => s.clone(),
                    None => return Ok("❌ No active WebDriver session.".to_string()),
                };

                // Quit the WebDriver session
                match std::sync::Arc::try_unwrap(session) {
                    Ok(mutex) => {
                        let driver = mutex.into_inner();
                        match driver.quit().await {
                            Ok(_) => {
                                debug!("WebDriver session closed successfully");

                                // Kill the safaridriver process
                                if let Some(mut process) =
                                    self.webdriver_process.write().await.take()
                                {
                                    if let Err(e) = process.kill().await {
                                        warn!("Failed to kill safaridriver process: {}", e);
                                    } else {
                                        debug!("Safaridriver process terminated");
                                    }
                                }

                                Ok("✅ WebDriver session closed and safaridriver stopped"
                                    .to_string())
                            }
                            Err(e) => Ok(format!("❌ Failed to quit WebDriver: {}", e)),
                        }
                    }
                    Err(_) => Ok("❌ Cannot quit: WebDriver session is still in use".to_string()),
                }
            }
            "macax_list_apps" => {
                debug!("Processing macax_list_apps tool call");

                if !self.config.macax.enabled {
                    return Ok(
                        "❌ macOS Accessibility is not enabled. Use --macax flag to enable."
                            .to_string(),
                    );
                }

                let controller_guard = self.macax_controller.read().await;
                let controller = match controller_guard.as_ref() {
                    Some(c) => c,
                    None => {
                        return Ok("❌ macOS Accessibility controller not initialized.".to_string())
                    }
                };

                match controller.list_applications() {
                    Ok(apps) => {
                        let app_list: Vec<String> = apps.iter().map(|a| a.name.clone()).collect();
                        Ok(format!("Running applications:\n{}", app_list.join("\n")))
                    }
                    Err(e) => Ok(format!("❌ Failed to list applications: {}", e)),
                }
            }
            "macax_get_frontmost_app" => {
                debug!("Processing macax_get_frontmost_app tool call");

                if !self.config.macax.enabled {
                    return Ok(
                        "❌ macOS Accessibility is not enabled. Use --macax flag to enable."
                            .to_string(),
                    );
                }

                let controller_guard = self.macax_controller.read().await;
                let controller = match controller_guard.as_ref() {
                    Some(c) => c,
                    None => {
                        return Ok("❌ macOS Accessibility controller not initialized.".to_string())
                    }
                };

                match controller.get_frontmost_app() {
                    Ok(app) => Ok(format!("Frontmost application: {}", app.name)),
                    Err(e) => Ok(format!("❌ Failed to get frontmost app: {}", e)),
                }
            }
            "macax_activate_app" => {
                debug!("Processing macax_activate_app tool call");

                if !self.config.macax.enabled {
                    return Ok(
                        "❌ macOS Accessibility is not enabled. Use --macax flag to enable."
                            .to_string(),
                    );
                }

                let app_name = match tool_call.args.get("app_name").and_then(|v| v.as_str()) {
                    Some(n) => n,
                    None => return Ok("❌ Missing app_name argument".to_string()),
                };

                let controller_guard = self.macax_controller.read().await;
                let controller = match controller_guard.as_ref() {
                    Some(c) => c,
                    None => {
                        return Ok("❌ macOS Accessibility controller not initialized.".to_string())
                    }
                };

                match controller.activate_app(app_name) {
                    Ok(_) => Ok(format!("✅ Activated application: {}", app_name)),
                    Err(e) => Ok(format!("❌ Failed to activate app: {}", e)),
                }
            }
            "macax_press_key" => {
                debug!("Processing macax_press_key tool call");

                if !self.config.macax.enabled {
                    return Ok(
                        "❌ macOS Accessibility is not enabled. Use --macax flag to enable."
                            .to_string(),
                    );
                }

                let app_name = match tool_call.args.get("app_name").and_then(|v| v.as_str()) {
                    Some(n) => n,
                    None => return Ok("❌ Missing app_name argument".to_string()),
                };

                let key = match tool_call.args.get("key").and_then(|v| v.as_str()) {
                    Some(k) => k,
                    None => return Ok("❌ Missing key argument".to_string()),
                };

                let modifiers_vec: Vec<&str> = tool_call
                    .args
                    .get("modifiers")
                    .and_then(|v| v.as_array())
                    .map(|arr| arr.iter().filter_map(|v| v.as_str()).collect())
                    .unwrap_or_default();

                let controller_guard = self.macax_controller.read().await;
                let controller = match controller_guard.as_ref() {
                    Some(c) => c,
                    None => {
                        return Ok("❌ macOS Accessibility controller not initialized.".to_string())
                    }
                };

                match controller.press_key(app_name, key, modifiers_vec.clone()) {
                    Ok(_) => {
                        let modifier_str = if modifiers_vec.is_empty() {
                            String::new()
                        } else {
                            format!(" with modifiers: {}", modifiers_vec.join("+"))
                        };
                        Ok(format!("✅ Pressed key: {}{}", key, modifier_str))
                    }
                    Err(e) => Ok(format!("❌ Failed to press key: {}", e)),
                }
            }
            "macax_type_text" => {
                debug!("Processing macax_type_text tool call");

                if !self.config.macax.enabled {
                    return Ok(
                        "❌ macOS Accessibility is not enabled. Use --macax flag to enable."
                            .to_string(),
                    );
                }

                let app_name = match tool_call.args.get("app_name").and_then(|v| v.as_str()) {
                    Some(n) => n,
                    None => return Ok("❌ Missing app_name argument".to_string()),
                };

                let text = match tool_call.args.get("text").and_then(|v| v.as_str()) {
                    Some(t) => t,
                    None => return Ok("❌ Missing text argument".to_string()),
                };

                let controller_guard = self.macax_controller.read().await;
                let controller = match controller_guard.as_ref() {
                    Some(c) => c,
                    None => {
                        return Ok("❌ macOS Accessibility controller not initialized.".to_string())
                    }
                };

                match controller.type_text(app_name, text) {
                    Ok(_) => Ok(format!("✅ Typed text into {}", app_name)),
                    Err(e) => Ok(format!("❌ Failed to type text: {}", e)),
                }
            }
            "vision_find_text" => {
                debug!("Processing vision_find_text tool call");

                if let Some(controller) = &self.computer_controller {
                    let app_name = tool_call
                        .args
                        .get("app_name")
                        .and_then(|v| v.as_str())
                        .ok_or_else(|| anyhow::anyhow!("Missing app_name parameter"))?;

                    let text = tool_call
                        .args
                        .get("text")
                        .and_then(|v| v.as_str())
                        .ok_or_else(|| anyhow::anyhow!("Missing text parameter"))?;

                    match controller.find_text_in_app(app_name, text).await {
                        Ok(Some(location)) => {
                            Ok(format!(
                                "✅ Found '{}' in {} at position ({}, {}) with size {}x{} (confidence: {:.0}%)",
                                location.text, app_name, location.x, location.y, location.width, location.height,
                                location.confidence * 100.0
                            ))
                        }
                        Ok(None) => Ok(format!("❌ Could not find '{}' in {}", text, app_name)),
                        Err(e) => Ok(format!("❌ Error finding text: {}", e)),
                    }
                } else {
                    Ok("❌ Computer control not enabled. Set computer_control.enabled = true in config.".to_string())
                }
            }
            "vision_click_text" => {
                debug!("Processing vision_click_text tool call");

                if let Some(controller) = &self.computer_controller {
                    let app_name = tool_call
                        .args
                        .get("app_name")
                        .and_then(|v| v.as_str())
                        .ok_or_else(|| anyhow::anyhow!("Missing app_name parameter"))?;

                    let text = tool_call
                        .args
                        .get("text")
                        .and_then(|v| v.as_str())
                        .ok_or_else(|| anyhow::anyhow!("Missing text parameter"))?;

                    match controller.find_text_in_app(app_name, text).await {
                        Ok(Some(location)) => {
                            // Click on center of text
                            // IMPORTANT: location coordinates are in NSScreen space (Y=0 at BOTTOM, increases UPWARD)
                            // location.x is the LEFT edge of the bounding box
                            // location.y is the TOP edge of the bounding box (highest Y value in NSScreen space)
                            // location.width and location.height are already scaled to screen space
                            // To get center: we need to add half the SCALED width and subtract half the SCALED height

                            if location.width == 0 || location.height == 0 {
                                return Ok(format!(
                                    "❌ Invalid bounding box dimensions: width={}, height={}",
                                    location.width, location.height
                                ));
                            }

                            debug!("[vision_click_text] Location from find_text_in_app: x={}, y={}, width={}, height={}, text='{}'",
                                location.x, location.y, location.width, location.height, location.text);

                            // Calculate center using the SCALED dimensions
                            // X: Use right edge instead of center (Vision OCR bounding box seems offset)
                            // This gives us: left edge + full width = right edge
                            // Y: top edge - half of scaled height (subtract because Y increases upward)
                            let click_x = location.x + location.width; // Right edge
                            let half_height = location.height / 2;
                            let click_y = location.y - half_height;

                            debug!("[vision_click_text] Click position calculation: x={} + {} = {} (right edge), y={} - {} = {}",
                                location.x, location.width, click_x, location.y, half_height, click_y);
                            debug!("[vision_click_text] This means: left_edge={}, center={}, right_edge={}",
                                location.x, click_x, location.x + location.width);

                            match controller.click_at(click_x, click_y, Some(app_name)) {
                                Ok(_) => Ok(format!(
                                    "✅ Clicked on '{}' in {} at ({}, {})",
                                    text, app_name, click_x, click_y
                                )),
                                Err(e) => Ok(format!("❌ Failed to click: {}", e)),
                            }
                        }
                        Ok(None) => Ok(format!("❌ Could not find '{}' in {}", text, app_name)),
                        Err(e) => Ok(format!("❌ Error finding text: {}", e)),
                    }
                } else {
                    Ok("❌ Computer control not enabled. Set computer_control.enabled = true in config.".to_string())
                }
            }
            "extract_text_with_boxes" => {
                debug!("Processing extract_text_with_boxes tool call");

                if !self.config.macax.enabled {
                    return Ok(
                        "❌ extract_text_with_boxes requires --macax flag to be enabled"
                            .to_string(),
                    );
                }

                if let Some(controller) = &self.computer_controller {
                    let path = tool_call
                        .args
                        .get("path")
                        .and_then(|v| v.as_str())
                        .ok_or_else(|| anyhow::anyhow!("Missing path parameter"))?;

                    // Optional: take screenshot of app first
                    let final_path = if let Some(app_name) =
                        tool_call.args.get("app_name").and_then(|v| v.as_str())
                    {
                        let temp_path =
                            format!("/tmp/g3_extract_boxes_{}.png", uuid::Uuid::new_v4());
                        match controller
                            .take_screenshot(&temp_path, None, Some(app_name))
                            .await
                        {
                            Ok(_) => temp_path,
                            Err(e) => return Ok(format!("❌ Failed to take screenshot: {}", e)),
                        }
                    } else {
                        path.to_string()
                    };

                    // Extract text with locations
                    match controller.extract_text_with_locations(&final_path).await {
                        Ok(locations) => {
                            // Clean up temp file if we created one
                            if final_path != path {
                                let _ = std::fs::remove_file(&final_path);
                            }

                            // Return as JSON
                            match serde_json::to_string_pretty(&locations) {
                                Ok(json) => Ok(format!(
                                    "✅ Extracted {} text elements:\n{}",
                                    locations.len(),
                                    json
                                )),
                                Err(e) => Ok(format!("❌ Failed to serialize results: {}", e)),
                            }
                        }
                        Err(e) => Ok(format!("❌ Failed to extract text: {}", e)),
                    }
                } else {
                    Ok("❌ Computer control not enabled. Set computer_control.enabled = true in config.".to_string())
                }
            }
            "vision_click_near_text" => {
                debug!("Processing vision_click_near_text tool call");

                if let Some(controller) = &self.computer_controller {
                    let app_name = tool_call
                        .args
                        .get("app_name")
                        .and_then(|v| v.as_str())
                        .ok_or_else(|| anyhow::anyhow!("Missing app_name parameter"))?;

                    let text = tool_call
                        .args
                        .get("text")
                        .and_then(|v| v.as_str())
                        .ok_or_else(|| anyhow::anyhow!("Missing text parameter"))?;

                    let direction = tool_call
                        .args
                        .get("direction")
                        .and_then(|v| v.as_str())
                        .unwrap_or("right");

                    let distance = tool_call
                        .args
                        .get("distance")
                        .and_then(|v| v.as_i64())
                        .unwrap_or(50) as i32;

                    match controller.find_text_in_app(app_name, text).await {
                        Ok(Some(location)) => {
                            // Calculate click position based on direction
                            // location.x is LEFT edge, location.y is TOP edge (in NSScreen space)
                            let (click_x, click_y) = match direction {
                                "right" => (
                                    location.x + location.width + distance,
                                    location.y - (location.height / 2),
                                ),
                                "below" => (
                                    location.x + (location.width / 2),
                                    location.y - location.height - distance,
                                ),
                                "left" => {
                                    (location.x - distance, location.y - (location.height / 2))
                                }
                                "above" => {
                                    (location.x + (location.width / 2), location.y + distance)
                                }
                                _ => (
                                    location.x + location.width + distance,
                                    location.y - (location.height / 2),
                                ),
                            };
                            debug!(
                                "[vision_click_near_text] Clicking {} of text at ({}, {})",
                                direction, click_x, click_y
                            );

                            match controller.click_at(click_x, click_y, Some(app_name)) {
                                Ok(_) => Ok(format!(
                                    "✅ Clicked {} of '{}' in {} at ({}, {})",
                                    direction, text, app_name, click_x, click_y
                                )),
                                Err(e) => Ok(format!("❌ Failed to click: {}", e)),
                            }
                        }
                        Ok(None) => Ok(format!("❌ Could not find '{}' in {}", text, app_name)),
                        Err(e) => Ok(format!("❌ Error finding text: {}", e)),
                    }
                } else {
                    Ok("❌ Computer control not enabled. Set computer_control.enabled = true in config.".to_string())
                }
            }
            "code_search" => {
                debug!("Processing code_search tool call");

                // Parse the request
                let request: crate::code_search::CodeSearchRequest =
                    match serde_json::from_value(tool_call.args.clone()) {
                        Ok(req) => req,
                        Err(e) => {
                            return Ok(format!("❌ Invalid code_search arguments: {}", e));
                        }
                    };

                // Execute the code search
                match crate::code_search::execute_code_search(request).await {
                    Ok(response) => {
                        // Serialize the response to JSON
                        match serde_json::to_string_pretty(&response) {
                            Ok(json_output) => {
                                Ok(format!("✅ Code search completed\n{}", json_output))
                            }
                            Err(e) => Ok(format!("❌ Failed to serialize response: {}", e)),
                        }
                    }
                    Err(e) => Ok(format!("❌ Code search failed: {}", e)),
                }
            }
            _ => {
                warn!("Unknown tool: {}", tool_call.tool);
                Ok(format!("❓ Unknown tool: {}", tool_call.tool))
            }
        }
    }


    /// Get image dimensions from raw bytes
    fn get_image_dimensions(bytes: &[u8], media_type: &str) -> Option<(u32, u32)> {
        match media_type {
            "image/png" => {
                // PNG: width at bytes 16-19, height at bytes 20-23 (big-endian)
                if bytes.len() >= 24 {
                    let width = u32::from_be_bytes([bytes[16], bytes[17], bytes[18], bytes[19]]);
                    let height = u32::from_be_bytes([bytes[20], bytes[21], bytes[22], bytes[23]]);
                    Some((width, height))
                } else {
                    None
                }
            }
            "image/jpeg" => {
                // JPEG: Need to find SOF0/SOF2 marker (FF C0 or FF C2)
                let mut i = 2; // Skip FF D8
                while i + 8 < bytes.len() {
                    if bytes[i] == 0xFF {
                        let marker = bytes[i + 1];
                        // SOF0, SOF1, SOF2 markers contain dimensions
                        if marker == 0xC0 || marker == 0xC1 || marker == 0xC2 {
                            let height = u16::from_be_bytes([bytes[i + 5], bytes[i + 6]]) as u32;
                            let width = u16::from_be_bytes([bytes[i + 7], bytes[i + 8]]) as u32;
                            return Some((width, height));
                        }
                        // Skip to next marker
                        if marker == 0xD8 || marker == 0xD9 || marker == 0x01 || (0xD0..=0xD7).contains(&marker) {
                            i += 2;
                        } else {
                            let len = u16::from_be_bytes([bytes[i + 2], bytes[i + 3]]) as usize;
                            i += 2 + len;
                        }
                    } else {
                        i += 1;
                    }
                }
                None
            }
            "image/gif" => {
                // GIF: width at bytes 6-7, height at bytes 8-9 (little-endian)
                if bytes.len() >= 10 {
                    let width = u16::from_le_bytes([bytes[6], bytes[7]]) as u32;
                    let height = u16::from_le_bytes([bytes[8], bytes[9]]) as u32;
                    Some((width, height))
                } else {
                    None
                }
            }
            "image/webp" => {
                // WebP VP8: dimensions at specific offsets (simplified)
                // This is a basic implementation - WebP format is complex
                if bytes.len() >= 30 && &bytes[12..16] == b"VP8 " {
                    // Lossy WebP
                    let width = (u16::from_le_bytes([bytes[26], bytes[27]]) & 0x3FFF) as u32;
                    let height = (u16::from_le_bytes([bytes[28], bytes[29]]) & 0x3FFF) as u32;
                    Some((width, height))
                } else {
                    None
                }
            }
            _ => None,
        }
    }

    /// Print image using iTerm2 imgcat protocol with info line
    fn print_imgcat(bytes: &[u8], name: &str, dimensions: &str, media_type: &str, size: &str, max_height: u32) {
        use base64::Engine;
        let encoded = base64::engine::general_purpose::STANDARD.encode(bytes);
        // Extract just the filename from the path
        let filename = std::path::Path::new(name)
            .file_name()
            .and_then(|f| f.to_str())
            .unwrap_or(name);
        // iTerm2 inline image protocol (single space prefix)
        print!(" \x1b]1337;File=inline=1;height={};name={}:{}\x07\n", max_height, name, encoded);
        // Print dimmed info line with filename only (no │ prefix)
        println!(" \x1b[2m{} | {} | {} | {}\x1b[0m", filename, dimensions, media_type, size);
        // Blank line before next image (no │ prefix)
        println!();
    }

    fn format_duration(duration: Duration) -> String {
        let total_ms = duration.as_millis();

        if total_ms < 1000 {
            format!("{}ms", total_ms)
        } else if total_ms < 60_000 {
            let seconds = duration.as_secs_f64();
            format!("{:.1}s", seconds)
        } else {
            let minutes = total_ms / 60_000;
            let remaining_seconds = (total_ms % 60_000) as f64 / 1000.0;
            format!("{}m {:.1}s", minutes, remaining_seconds)
        }
    }

    /// Format the timing footer with optional token usage info
    fn format_timing_footer(
        elapsed: Duration,
        ttft: Duration,
        turn_tokens: Option<u32>,
        context_percentage: f32,
    ) -> String {
        let timing = format!("⏱️ {} | 💭 {}", Self::format_duration(elapsed), Self::format_duration(ttft));
        
        // Add token usage info if available (dimmed)
        if let Some(tokens) = turn_tokens {
            format!("{}  \x1b[2m{} ◉ | {:.0}%\x1b[0m", timing, tokens, context_percentage)
        } else {
            format!("{}  \x1b[2m{:.0}%\x1b[0m", timing, context_percentage)
        }
    }
}


// Re-export utility functions
pub use utils::apply_unified_diff_to_string;
use utils::shell_escape_command;

// Implement Drop to clean up safaridriver process
impl<W: UiWriter> Drop for Agent<W> {
    fn drop(&mut self) {
        // Validate system prompt invariant on drop (agent exit)
        // This catches any bugs where the conversation history was corrupted during execution
        if !self.context_window.conversation_history.is_empty() {
            if let Err(e) = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                self.validate_system_prompt_is_first();
            })) {
                eprintln!(
                    "\n⚠️  FATAL ERROR ON EXIT: System prompt validation failed: {:?}",
                    e
                );
            }
        }

        // Try to kill safaridriver process if it's still running
        // We need to use try_lock since we can't await in Drop
        if let Ok(mut process_guard) = self.webdriver_process.try_write() {
            if let Some(process) = process_guard.take() {
                // Use blocking kill since we can't await in Drop
                // This is a best-effort cleanup
                let _ = std::process::Command::new("kill")
                    .arg("-9")
                    .arg(process.id().unwrap_or(0).to_string())
                    .output();

                debug!("Attempted to clean up safaridriver process on Agent drop");
            }
        }
    }
}
