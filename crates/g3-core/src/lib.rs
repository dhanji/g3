pub mod error_handling;
pub mod project;
pub mod task_result;
pub mod ui_writer;
pub mod code_search;
pub use task_result::TaskResult;

#[cfg(test)]
mod task_result_comprehensive_tests;
use crate::ui_writer::UiWriter;

// Make fixed_filter_json public so it can be accessed from g3-cli
pub mod fixed_filter_json;
#[cfg(test)]
mod fixed_filter_tests;

#[cfg(test)]
mod tilde_expansion_tests;

#[cfg(test)]
mod error_handling_test;
use anyhow::Result;
use g3_config::Config;
use g3_execution::CodeExecutor;
use g3_computer_control::WebDriverController;
use g3_providers::{CompletionRequest, Message, MessageRole, ProviderRegistry, Tool};
#[allow(unused_imports)]
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::time::{Duration, Instant};
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info, warn};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCall {
    pub tool: String,
    pub args: serde_json::Value, // Should be a JSON object with tool-specific arguments
}

#[derive(Debug, Clone)]
pub enum StreamState {
    Generating,
    ToolDetected(ToolCall),
    Executing,
    Resuming,
}

/// Modern streaming tool parser that properly handles native tool calls and SSE chunks
#[derive(Debug)]
pub struct StreamingToolParser {
    /// Buffer for accumulating text content
    text_buffer: String,
    /// Buffer for accumulating native tool calls
    native_tool_calls: Vec<g3_providers::ToolCall>,
    /// Whether we've received a message_stop event
    message_stopped: bool,
    /// Whether we're currently in a JSON tool call (for fallback parsing)
    in_json_tool_call: bool,
    /// Start position of JSON tool call (for fallback parsing)
    json_tool_start: Option<usize>,
}

impl Default for StreamingToolParser {
    fn default() -> Self {
        Self::new()
    }
}

impl StreamingToolParser {
    pub fn new() -> Self {
        Self {
            text_buffer: String::new(),
            native_tool_calls: Vec::new(),
            message_stopped: false,
            in_json_tool_call: false,
            json_tool_start: None,
        }
    }

    /// Process a streaming chunk and return completed tool calls if any
    pub fn process_chunk(&mut self, chunk: &g3_providers::CompletionChunk) -> Vec<ToolCall> {
        let mut completed_tools = Vec::new();

        // Add text content to buffer
        if !chunk.content.is_empty() {
            self.text_buffer.push_str(&chunk.content);
        }

        // Handle native tool calls
        if let Some(ref tool_calls) = chunk.tool_calls {
            debug!("Received native tool calls: {:?}", tool_calls);

            // Accumulate native tool calls
            for tool_call in tool_calls {
                self.native_tool_calls.push(tool_call.clone());
            }
        }

        // Check if message is finished/stopped
        if chunk.finished {
            self.message_stopped = true;
            debug!("Message finished, processing accumulated tool calls");
        }

        // If we have native tool calls and the message is stopped, return them
        if self.message_stopped && !self.native_tool_calls.is_empty() {
            debug!(
                "Converting {} native tool calls",
                self.native_tool_calls.len()
            );

            for native_tool in &self.native_tool_calls {
                let converted_tool = ToolCall {
                    tool: native_tool.tool.clone(),
                    args: native_tool.args.clone(),
                };
                completed_tools.push(converted_tool);
            }

            // Clear native tool calls after processing
            self.native_tool_calls.clear();
        }

        // Fallback: Try to parse JSON tool calls from text if no native tool calls
        if completed_tools.is_empty() && !chunk.content.is_empty() {
            if let Some(json_tool) = self.try_parse_json_tool_call(&chunk.content) {
                completed_tools.push(json_tool);
            }
        }

        completed_tools
    }

    /// Fallback method to parse JSON tool calls from text content
    fn try_parse_json_tool_call(&mut self, _content: &str) -> Option<ToolCall> {
        // Look for JSON tool call patterns
        let patterns = [
            r#"{"tool":"#,
            r#"{ "tool":"#,
            r#"{"tool" :"#,
            r#"{ "tool" :"#,
        ];

        // If we're not currently in a JSON tool call, look for the start
        if !self.in_json_tool_call {
            for pattern in &patterns {
                if let Some(pos) = self.text_buffer.rfind(pattern) {
                    debug!(
                        "Found JSON tool call pattern '{}' at position {}",
                        pattern, pos
                    );
                    self.in_json_tool_call = true;
                    self.json_tool_start = Some(pos);
                    break;
                }
            }
        }

        // If we're in a JSON tool call, try to find the end and parse it
        if self.in_json_tool_call {
            if let Some(start_pos) = self.json_tool_start {
                let json_text = &self.text_buffer[start_pos..];

                // Try to find a complete JSON object
                let mut brace_count = 0;
                let mut in_string = false;
                let mut escape_next = false;

                for (i, ch) in json_text.char_indices() {
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
                                // Found complete JSON object
                                let json_str = &json_text[..=i];
                                debug!("Attempting to parse JSON tool call: {}", json_str);

                                // First try to parse as a ToolCall
                                if let Ok(tool_call) = serde_json::from_str::<ToolCall>(json_str) {
                                    // Validate that this is actually a proper tool call
                                    // The args should be a JSON object with reasonable keys
                                    if let Some(args_obj) = tool_call.args.as_object() {
                                        // Check if any key looks like it contains agent message content
                                        // This would indicate a malformed tool call where the message
                                        // got mixed into the args
                                        let has_message_like_key = args_obj.keys().any(|key| {
                                            key.len() > 100
                                                || key.contains('\n')
                                                || key.contains("I'll")
                                                || key.contains("Let me")
                                                || key.contains("Here's")
                                                || key.contains("I can")
                                                || key.contains("I need")
                                                || key.contains("First")
                                                || key.contains("Now")
                                                || key.contains("The ")
                                        });

                                        if has_message_like_key {
                                            debug!("Detected malformed tool call with message-like keys, skipping");
                                            // This looks like a malformed tool call, skip it
                                            self.in_json_tool_call = false;
                                            self.json_tool_start = None;
                                            break;
                                        }

                                        // Also check if the values look reasonable
                                        // Tool arguments should typically be file paths, commands, or content
                                        // Not entire agent messages

                                        debug!(
                                            "Successfully parsed valid JSON tool call: {:?}",
                                            tool_call
                                        );
                                        // Reset JSON parsing state
                                        self.in_json_tool_call = false;
                                        self.json_tool_start = None;
                                        return Some(tool_call);
                                    }
                                    // If args is not an object, skip this as invalid
                                    debug!("Tool call args is not an object, skipping");
                                } else {
                                    debug!("Failed to parse JSON tool call: {}", json_str);
                                    // Reset and continue looking
                                    self.in_json_tool_call = false;
                                    self.json_tool_start = None;
                                }
                                break;
                            }
                        }
                        _ => {}
                    }
                }
            }
        }

        None
    }

    /// Get the accumulated text content (excluding tool calls)
    pub fn get_text_content(&self) -> &str {
        &self.text_buffer
    }

    /// Get content before a specific position (for display purposes)
    pub fn get_content_before_position(&self, pos: usize) -> String {
        if pos <= self.text_buffer.len() {
            self.text_buffer[..pos].to_string()
        } else {
            self.text_buffer.clone()
        }
    }

    /// Check if the message has been stopped/finished
    pub fn is_message_stopped(&self) -> bool {
        self.message_stopped
    }

    /// Reset the parser state for a new message
    pub fn reset(&mut self) {
        self.text_buffer.clear();
        self.native_tool_calls.clear();
        self.message_stopped = false;
        self.in_json_tool_call = false;
        self.json_tool_start = None;
    }

    /// Get the current text buffer length (for position tracking)
    pub fn text_buffer_len(&self) -> usize {
        self.text_buffer.len()
    }
}

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
    pub fn update_usage_from_response(&mut self, usage: &g3_providers::Usage) {
        // Add the tokens from this response to our running total
        // The usage.total_tokens represents tokens used in this single API call
        self.used_tokens += usage.total_tokens;
        self.cumulative_tokens += usage.total_tokens;

        debug!(
            "Added {} tokens from provider response (used: {}/{}, cumulative: {})",
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

    /// Update cumulative token usage (for streaming)
    pub fn add_streaming_tokens(&mut self, new_tokens: u32) {
        self.used_tokens += new_tokens;
        self.cumulative_tokens += new_tokens;
        debug!(
            "Added {} streaming tokens (used: {}/{}, cumulative: {})",
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
    pub fn reset_with_summary(&mut self, summary: String, latest_user_message: Option<String>) -> usize {
        // Calculate chars saved (old history minus new summary)
        let old_chars: usize = self.conversation_history.iter()
            .map(|m| m.content.len())
            .sum();
        
        // Clear the conversation history
        self.conversation_history.clear();
        self.used_tokens = 0;

        // Add the summary as a system message
        let summary_message = Message {
            role: MessageRole::System,
            content: format!("Previous conversation summary:\n\n{}", summary),
        };
        self.add_message(summary_message);

        // Add the latest user message if provided
        if let Some(user_msg) = latest_user_message {
            self.add_message(Message {
                role: MessageRole::User,
                content: user_msg,
            });
        }
        
        let new_chars: usize = self.conversation_history.iter()
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
    pub fn thin_context(&mut self) -> (String, usize) {
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
        
        // Create ~/tmp directory if it doesn't exist
        let tmp_dir = shellexpand::tilde("~/tmp").to_string();
        if let Err(e) = std::fs::create_dir_all(&tmp_dir) {
            warn!("Failed to create ~/tmp directory: {}", e);
            return ("⚠️  Context thinning failed: could not create ~/tmp directory".to_string(), 0);
        }
        
        // Scan the first third of messages
        for i in 0..first_third_end {
            if let Some(message) = self.conversation_history.get_mut(i) {
                // Process User messages that look like tool results
                if matches!(message.role, MessageRole::User) && message.content.starts_with("Tool result:") {
                    let content_len = message.content.len();
                    
                    // Only thin if the content is greater than 500 chars
                    if content_len > 500 {
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
                        
                        debug!("Thinned tool result {} ({} chars) to {}", i, original_len, file_path);
                    }
                }
                
                // Process Assistant messages that contain tool calls with large arguments
                if matches!(message.role, MessageRole::Assistant) {
                    // Try to parse the message content as JSON to find tool calls
                    let content = &message.content;
                    
                    // Look for JSON tool call patterns
                    if let Some(tool_call_start) = content.find(r#"{"tool":"#)
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
                                        let content_info = args_obj.get("content")
                                            .and_then(|v| v.as_str())
                                            .map(|s| (s.to_string(), s.len()));
                                        
                                        if let Some((content_str, content_len)) = content_info {
                                            // Only thin if content is greater than 500 chars
                                            if content_len > 500 {
                                                let timestamp = std::time::SystemTime::now()
                                                    .duration_since(std::time::UNIX_EPOCH)
                                                    .unwrap_or_default()
                                                    .as_secs();
                                                let filename = format!("leaned_write_file_content_{}_{}.txt", timestamp, i);
                                                let file_path = format!("{}/{}", tmp_dir, filename);
                                                
                                                if std::fs::write(&file_path, &content_str).is_ok() {
                                                    args_obj.insert(
                                                        "content".to_string(),
                                                        serde_json::Value::String(format!("<content saved to {}>", file_path))
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
                                        let diff_info = args_obj.get("diff")
                                            .and_then(|v| v.as_str())
                                            .map(|s| (s.to_string(), s.len()));
                                        
                                        if let Some((diff_str, diff_len)) = diff_info {
                                            // Only thin if diff is greater than 500 chars
                                            if diff_len > 500 {
                                                let timestamp = std::time::SystemTime::now()
                                                    .duration_since(std::time::UNIX_EPOCH)
                                                    .unwrap_or_default()
                                                    .as_secs();
                                                let filename = format!("leaned_str_replace_diff_{}_{}.txt", timestamp, i);
                                                let file_path = format!("{}/{}", tmp_dir, filename);
                                                
                                                if std::fs::write(&file_path, &diff_str).is_ok() {
                                                    args_obj.insert(
                                                        "diff".to_string(),
                                                        serde_json::Value::String(format!("<diff saved to {}>", file_path))
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
                                        message.content = format!("{}{}{}", prefix, new_json, suffix);
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
                (format!("🥒 Context thinned at {}%: {} tool results, ~{} chars saved", 
                        current_threshold, leaned_count, chars_saved), chars_saved)
            }
        } else if tool_call_leaned_count > 0 {
            (format!("🥒 Context thinned at {}%: {} tool calls, ~{} chars saved", 
                    current_threshold, tool_call_leaned_count, chars_saved), chars_saved)
        } else {
            (format!("ℹ Context thinning triggered at {}% but no large tool results or tool calls found in first third", 
                    current_threshold), 0)
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
    thinning_events: Vec<usize>, // chars saved per thinning event
    pending_90_summarization: bool, // flag to trigger summarization at 90%
    auto_compact: bool, // whether to auto-compact at 90% before tool calls
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
    webdriver_session: std::sync::Arc<tokio::sync::RwLock<Option<std::sync::Arc<tokio::sync::Mutex<g3_computer_control::SafariDriver>>>>>,
    safaridriver_process: std::sync::Arc<tokio::sync::RwLock<Option<tokio::process::Child>>>,
    macax_controller: std::sync::Arc<tokio::sync::RwLock<Option<g3_computer_control::MacAxController>>>,
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
        Self::new_with_mode_and_readme(config, ui_writer, false, readme_content, false).await
    }

    pub async fn new_autonomous_with_readme(
        config: Config,
        ui_writer: W,
        readme_content: Option<String>,
    ) -> Result<Self> {
        Self::new_with_mode_and_readme(config, ui_writer, true, readme_content, false).await
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
        Self::new_with_mode_and_readme(config, ui_writer, false, readme_content, quiet).await
    }

    pub async fn new_autonomous_with_readme_and_quiet(
        config: Config,
        ui_writer: W,
        readme_content: Option<String>,
        quiet: bool,
    ) -> Result<Self> {
        Self::new_with_mode_and_readme(config, ui_writer, true, readme_content, quiet).await
    }

    async fn new_with_mode(
        config: Config,
        ui_writer: W,
        is_autonomous: bool,
        quiet: bool,
    ) -> Result<Self> {
        Self::new_with_mode_and_readme(config, ui_writer, is_autonomous, None, quiet).await
    }

    async fn new_with_mode_and_readme(
        config: Config,
        ui_writer: W,
        is_autonomous: bool,
        readme_content: Option<String>,
        quiet: bool,
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

        // Only register providers that are configured AND selected as the default provider
        // This prevents unnecessary initialization of heavy providers like embedded models

        // Register embedded provider if configured AND it's the default provider
        if let Some(embedded_config) = &config.providers.embedded {
            if providers_to_register.contains(&"embedded".to_string()) {
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

        // Register OpenAI provider if configured AND it's the default provider
        if let Some(openai_config) = &config.providers.openai {
            if providers_to_register.contains(&"openai".to_string()) {
                let openai_provider = g3_providers::OpenAIProvider::new(
                    openai_config.api_key.clone(),
                    Some(openai_config.model.clone()),
                    openai_config.base_url.clone(),
                    openai_config.max_tokens,
                    openai_config.temperature,
                )?;
                providers.register(openai_provider);
            }
        }

        // Register Anthropic provider if configured AND it's the default provider
        if let Some(anthropic_config) = &config.providers.anthropic {
            if providers_to_register.contains(&"anthropic".to_string()) {
                let anthropic_provider = g3_providers::AnthropicProvider::new(
                    anthropic_config.api_key.clone(),
                    Some(anthropic_config.model.clone()),
                    anthropic_config.max_tokens,
                    anthropic_config.temperature,
                )?;
                providers.register(anthropic_provider);
            }
        }

        // Register Databricks provider if configured AND it's the default provider
        if let Some(databricks_config) = &config.providers.databricks {
            if providers_to_register.contains(&"databricks".to_string()) {

                let databricks_provider = if let Some(token) = &databricks_config.token {
                    // Use token-based authentication
                    g3_providers::DatabricksProvider::from_token(
                        databricks_config.host.clone(),
                        token.clone(),
                        databricks_config.model.clone(),
                        databricks_config.max_tokens,
                        databricks_config.temperature,
                    )?
                } else {
                    // Use OAuth authentication
                    g3_providers::DatabricksProvider::from_oauth(
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

        // Set default provider
        debug!(
            "Setting default provider to: {}",
            config.providers.default_provider
        );
        providers.set_default(&config.providers.default_provider)?;
        debug!("Default provider set successfully");

        // Determine context window size based on active provider
        let context_length = Self::determine_context_length(&config, &providers)?;
        let mut context_window = ContextWindow::new(context_length);

        // If README content is provided, add it as the first system message
        if let Some(readme) = readme_content {
            let readme_message = Message {
                role: MessageRole::System,
                content: readme,
            };
            context_window.add_message(readme_message);
        }

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
            todo_content: std::sync::Arc::new(tokio::sync::RwLock::new(String::new())),
            is_autonomous,
            quiet,
            computer_controller,
            webdriver_session: std::sync::Arc::new(tokio::sync::RwLock::new(None)),
            safaridriver_process: std::sync::Arc::new(tokio::sync::RwLock::new(None)),
            macax_controller: {
                std::sync::Arc::new(tokio::sync::RwLock::new(
                    if macax_enabled { Some(g3_computer_control::MacAxController::new()?) }
                    else { None }
                ))
            },
        })
    }

    fn determine_context_length(config: &Config, providers: &ProviderRegistry) -> Result<u32> {
        // Get the active provider to determine context length
        let provider = providers.get(None)?;
        let provider_name = provider.name();
        let model_name = provider.model();

        // Use provider-specific context length if available, otherwise fall back to agent config
        let context_length = match provider_name {
            "embedded" => {
                // For embedded models, use the configured context_length or model-specific defaults
                if let Some(embedded_config) = &config.providers.embedded {
                    embedded_config.context_length.unwrap_or_else(|| {
                        // Model-specific defaults for embedded models
                        match embedded_config.model_type.to_lowercase().as_str() {
                            "codellama" => 16384, // CodeLlama supports 16k context
                            "llama" => 4096,      // Base Llama models
                            "mistral" => 8192,    // Mistral models
                            "qwen" => 32768,      // Qwen2.5 supports 32k context
                            _ => 4096,            // Conservative default
                        }
                    })
                } else {
                    config.agent.max_context_length as u32
                }
            }
            "openai" => {
                192000
            }
            "anthropic" => {
                // Claude models have large context windows
                200000 // Default for Claude models
            }
            "databricks" => {
                // Databricks models have varying context windows depending on the model
                if model_name.contains("claude") {
                    200000 // Claude models on Databricks have large context windows
                } else if model_name.contains("llama") || model_name.contains("dbrx") {
                    32768 // DBRX supports 32k context
                } else {
                    16384 // Conservative default for other Databricks models
                }
            }
            _ => config.agent.max_context_length as u32,
        };

        info!(
            "Using context length: {} tokens for provider: {} (model: {})",
            context_length, provider_name, model_name
        );

        Ok(context_length)
    }

    pub fn get_provider_info(&self) -> Result<(String, String)> {
        let provider = self.providers.get(None)?;
        Ok((provider.name().to_string(), provider.model().to_string()))
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
        self.execute_task_with_options(description, language, false, false, false)
            .await
    }

    pub async fn execute_task_with_options(
        &mut self,
        description: &str,
        language: Option<&str>,
        _auto_execute: bool,
        show_prompt: bool,
        show_code: bool,
    ) -> Result<TaskResult> {
        self.execute_task_with_timing(
            description,
            language,
            _auto_execute,
            show_prompt,
            show_code,
            false,
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
    ) -> Result<TaskResult> {
        // Execute the task directly without splitting
        self.execute_single_task(
            description,
            show_prompt,
            show_code,
            show_timing,
            cancellation_token,
        )
        .await
    }

    async fn execute_single_task(
        &mut self,
        description: &str,
        show_prompt: bool,
        _show_code: bool,
        show_timing: bool,
        cancellation_token: CancellationToken,
    ) -> Result<TaskResult> {
        // Reset the JSON tool call filter state at the start of each new task
        // This prevents the filter from staying in suppression mode between user interactions
        fixed_filter_json::reset_fixed_json_tool_state();

        // Generate session ID based on the initial prompt if this is a new session
        if self.session_id.is_none() {
            self.session_id = Some(self.generate_session_id(description));
        }

        // Only add system message if this is the first interaction (empty conversation history)
        if self.context_window.conversation_history.is_empty() {
            let provider = self.providers.get(None)?;
            let system_prompt = if provider.has_native_tool_calling() {
                // For native tool calling providers, use a more explicit system prompt
                "You are G3, an AI programming agent of the same skill level as a seasoned engineer at a major technology company. You analyze given tasks and write code to achieve goals.

You have access to tools. When you need to accomplish a task, you MUST use the appropriate tool. Do not just describe what you would do - actually use the tools.

IMPORTANT: You must call tools to achieve goals. When you receive a request:
1. Analyze and identify what needs to be done
2. Call the appropriate tool with the required parameters
3. Continue or complete the task based on the result
4. If you repeatedly try something and it fails, try a different approach
5. Call the final_output tool with a detailed summary when done.

For shell commands: Use the shell tool with the exact command needed. Avoid commands that produce a large amount of output, and consider piping those outputs to files. Example: If asked to list files, immediately call the shell tool with command parameter \"ls\".
If you create temporary files for verification, place these in a subdir named 'tmp'. Do NOT pollute the current dir.

For working with code, prioritize use of code_search tool over read_file, first.

Additional examples for the 'code_search' tool:
  - Find functions: {\"tool\": \"code_search\", \"args\": {\"searches\": [{\"name\": \"find_functions\", \"query\": \"(function_item name: (identifier) @name)\", \"language\": \"rust\", \"paths\": [\"src/\"]}]}}
  - Find async functions: {\"tool\": \"code_search\", \"args\": {\"searches\": [{\"name\": \"find_async\", \"query\": \"(function_item (function_modifiers) name: (identifier) @name)\", \"language\": \"rust\"}]}}
  - Find structs: {\"tool\": \"code_search\", \"args\": {\"searches\": [{\"name\": \"structs\", \"query\": \"(struct_item name: (type_identifier) @name)\", \"language\": \"rust\"}]}}
  - Multiple searches: {\"tool\": \"code_search\", \"args\": {\"searches\": [{\"name\": \"funcs\", \"query\": \"(function_item name: (identifier) @name)\", \"language\": \"rust\"}, {\"name\": \"structs\", \"query\": \"(struct_item name: (type_identifier) @name)\", \"language\": \"rust\"}]}}
  - With context lines: {\"tool\": \"code_search\", \"args\": {\"searches\": [{\"name\": \"funcs\", \"query\": \"(function_item name: (identifier) @name)\", \"language\": \"rust\", \"context_lines\": 3}]}}
       - \"context\": 3 (show surrounding lines),
       - \"json_style\": \"stream\" (for large results)


IMPORTANT: If the user asks you to just respond with text (like \"just say hello\" or \"tell me about X\"), do NOT use tools. Simply respond with the requested text directly. Only use tools when you need to execute commands or complete tasks that require action.

When taking screenshots of specific windows (like \"my Safari window\" or \"my terminal\"), ALWAYS use list_windows first to identify the correct window ID, then use take_screenshot with the window_id parameter.

Do not explain what you're going to do - just do it by calling the tools.

# Task Management

Use todo_read and todo_write for all but the simplest of tasks.

Workflow:
- Start: read → write checklist
- During: read → update progress
- End: verify all complete

Warning: todo_write overwrites entirely; always todo_read first (skipping is an error)

Keep items short, specific, action-oriented. Not using the todo tools for complex tasks is an error.

Template:
- [ ] Implement feature X
  - [ ] Update API
  - [ ] Write tests
  - [ ] Run tests
  - [ ] Run lint
- [ ] Blocked: waiting on credentials


# Response Guidelines

- Use Markdown formatting for all responses except tool calls.
- Whenever taking actions, use the pronoun 'I'
".to_string()
            } else {
                // For non-native providers (embedded models), use JSON format instructions
                "You are G3, a general-purpose AI agent. Your goal is to analyze and solve problems by writing code.

You have access to tools. When you need to accomplish a task, you MUST use the appropriate tool. Do not just describe what you would do - actually use the tools.

# Tool Call Format

When you need to execute a tool, write ONLY the JSON tool call on a new line:

{\"tool\": \"tool_name\", \"args\": {\"param\": \"value\"}

The tool will execute immediately and you'll receive the result (success or error) to continue with.

# Available Tools

Short description for providers without native calling specs:

- **shell**: Execute shell commands
  - Format: {\"tool\": \"shell\", \"args\": {\"command\": \"your_command_here\"}
  - Example: {\"tool\": \"shell\", \"args\": {\"command\": \"ls ~/Downloads\"}

- **read_file**: Read the contents of a file (supports partial reads via start/end)
  - Format: {\"tool\": \"read_file\", \"args\": {\"file_path\": \"path/to/file\", \"start\": 0, \"end\": 100}
  - Example: {\"tool\": \"read_file\", \"args\": {\"file_path\": \"src/main.rs\"}
  - Example (partial): {\"tool\": \"read_file\", \"args\": {\"file_path\": \"large.log\", \"start\": 0, \"end\": 1000}

- **write_file**: Write content to a file (creates or overwrites)
  - Format: {\"tool\": \"write_file\", \"args\": {\"file_path\": \"path/to/file\", \"content\": \"file content\"}
  - Example: {\"tool\": \"write_file\", \"args\": {\"file_path\": \"src/lib.rs\", \"content\": \"pub fn hello() {}\"}

- **str_replace**: Replace text in a file using a diff
  - Format: {\"tool\": \"str_replace\", \"args\": {\"file_path\": \"path/to/file\", \"diff\": \"--- old\\n-old text\\n+++ new\\n+new text\"}
  - Example: {\"tool\": \"str_replace\", \"args\": {\"file_path\": \"src/main.rs\", \"diff\": \"--- old\\n-old_code();\\n+++ new\\n+new_code();\"}

- **final_output**: Signal task completion with a detailed summary of work done in markdown format
  - Format: {\"tool\": \"final_output\", \"args\": {\"summary\": \"what_was_accomplished\"}

- **todo_read**: Read the entire TODO list content
  - Format: {\"tool\": \"todo_read\", \"args\": {}}
  - Example: {\"tool\": \"todo_read\", \"args\": {}}

- **todo_write**: Write or overwrite the entire TODO list (WARNING: overwrites completely, always read first)
  - Format: {\"tool\": \"todo_write\", \"args\": {\"content\": \"- [ ] Task 1\\n- [ ] Task 2\"}}
  - Example: {\"tool\": \"todo_write\", \"args\": {\"content\": \"- [ ] Implement feature\\n  - [ ] Write tests\\n  - [ ] Run tests\"}}

- **code_search**: Syntax-aware code search using tree-sitter. Supports Rust, Python, JavaScript, TypeScript.
  - Format: {\"tool\": \"code_search\", \"args\": {\"searches\": [{\"name\": \"label\", \"query\": \"tree-sitter query\", \"language\": \"rust|python|javascript|typescript\", \"paths\": [\"src/\"], \"context_lines\": 0}]}}
  - Find functions: {\"tool\": \"code_search\", \"args\": {\"searches\": [{\"name\": \"find_functions\", \"query\": \"(function_item name: (identifier) @name)\", \"language\": \"rust\", \"paths\": [\"src/\"]}]}}
  - Find async functions: {\"tool\": \"code_search\", \"args\": {\"searches\": [{\"name\": \"find_async\", \"query\": \"(function_item (function_modifiers) name: (identifier) @name)\", \"language\": \"rust\"}]}}
  - Find structs: {\"tool\": \"code_search\", \"args\": {\"searches\": [{\"name\": \"structs\", \"query\": \"(struct_item name: (type_identifier) @name)\", \"language\": \"rust\"}]}}
  - Multiple searches: {\"tool\": \"code_search\", \"args\": {\"searches\": [{\"name\": \"funcs\", \"query\": \"(function_item name: (identifier) @name)\", \"language\": \"rust\"}, {\"name\": \"structs\", \"query\": \"(struct_item name: (type_identifier) @name)\", \"language\": \"rust\"}]}}
  - With context lines: {\"tool\": \"code_search\", \"args\": {\"searches\": [{\"name\": \"funcs\", \"query\": \"(function_item name: (identifier) @name)\", \"language\": \"rust\", \"context_lines\": 3}]}}
       - \"context\": 3 (show surrounding lines),
       - \"json_style\": \"stream\" (for large results)

# Instructions

1. Analyze the request and break down into smaller tasks if appropriate
2. Execute ONE tool at a time. An exception exists for when you're writing files. See below.
3. STOP when the original request was satisfied
4. Call the final_output tool when done

For reading files, prioritize use of code_search tool use with multiple search requests per call instead of read_file, if it makes sense.

Exception to using ONE tool at a time:
If all you’re doing is WRITING files, and you don’t need to do anything else between each step.
You can issue MULTIPLE write_file tool calls in a request, however you may ONLY make a SINGLE write_file call for any file in that request.
For example you may call:
[START OF REQUEST]
write_file(\"helper.rs\", \"...\")
write_file(\"file2.txt\", \"...\")
[DONE]

But NOT:
[START OF REQUEST]
write_file(\"helper.rs\", \"...\")
write_file(\"file2.txt\", \"...\")
write_file(\"helper.rs\", \"...\")
[DONE]

# Task Management

Use todo_read and todo_write for tasks with 3+ steps, multiple files/components, or uncertain scope.

Workflow:
- Start: read → write checklist
- During: read → update progress
- End: verify all complete

Warning: todo_write overwrites entirely; always todo_read first (skipping is an error)

Keep items short, specific, action-oriented. Not using the todo tools for complex tasks is an error.

Template:
- [ ] Implement feature X
  - [ ] Update API
  - [ ] Write tests

# Response Guidelines

- Use Markdown formatting for all responses except tool calls.
- Whenever taking actions, use the pronoun 'I'

".to_string()
            };

            if show_prompt {
                self.ui_writer.print_system_prompt(&system_prompt);
            }

            // Add system message to context window
            let system_message = Message {
                role: MessageRole::System,
                content: system_prompt,
            };
            self.context_window.add_message(system_message);
        }

        // Add user message to context window
        let user_message = Message {
            role: MessageRole::User,
            content: format!("Task: {}", description),
        };
        self.context_window.add_message(user_message);

        // Use the complete conversation history for the request
        let messages = self.context_window.conversation_history.clone();

        // Check if provider supports native tool calling and add tools if so
        let provider = self.providers.get(None)?;
        let tools = if provider.has_native_tool_calling() {
            Some(Self::create_tool_definitions(self.config.webdriver.enabled, self.config.macax.enabled, self.config.computer_control.enabled))
        } else {
            None
        };

        // Get max_tokens from provider configuration
        let max_tokens = match provider.name() {
            "databricks" => {
                // Use the model's maximum limit for Databricks to allow large file generation
                Some(32000)
            }
            _ => {
                // Default for other providers
                Some(16000)
            }
        };

        let request = CompletionRequest {
            messages,
            max_tokens,
            temperature: Some(0.1),
            stream: true, // Enable streaming
            tools,
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
            let assistant_message = Message {
                role: MessageRole::Assistant,
                content: response_content.clone(),
            };
            self.context_window.add_message(assistant_message);
        } else {
            debug!("Assistant response was empty (likely only tool execution), skipping message addition");
        }

        // Save context window at the end of successful interaction
        self.save_context_window("completed");

        // Check if we need to do 90% auto-compaction
        if self.pending_90_summarization {
            self.ui_writer.print_context_status(
                "\n⚡ Context window reached 90% - auto-compacting...\n"
            );
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

        // Create logs directory if it doesn't exist
        let logs_dir = std::path::Path::new("logs");
        if !logs_dir.exists() {
            if let Err(e) = std::fs::create_dir_all(logs_dir) {
                error!("Failed to create logs directory: {}", e);
                return;
            }
        }

        // Use session-based filename if we have a session ID, otherwise fall back to timestamp
        let filename = if let Some(ref session_id) = self.session_id {
            format!("logs/g3_session_{}.json", session_id)
        } else {
            format!("logs/g3_context_{}.json", timestamp)
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
                if let Err(e) = std::fs::write(&filename, json_content) {
                    error!("Failed to save context window to {}: {}", filename, e);
                }
            }
            Err(e) => {
                error!("Failed to serialize context window: {}", e);
            }
        }
    }

    pub fn get_context_window(&self) -> &ContextWindow {
        &self.context_window
    }

    /// Manually trigger context summarization regardless of context window size
    /// Returns Ok(true) if summarization was successful, Ok(false) if it failed
    pub async fn force_summarize(&mut self) -> Result<bool> {
        info!("Manual summarization triggered");
        
        self.ui_writer.print_context_status(&format!(
            "\n🗜️ Manual summarization requested (current usage: {}%)...",
            self.context_window.percentage_used() as u32
        ));

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
            Message {
                role: MessageRole::System,
                content: "You are a helpful assistant that creates concise summaries."
                    .to_string(),
            },
            Message {
                role: MessageRole::User,
                content: format!(
                    "Based on this conversation history, {}\n\nConversation:\n{}",
                    summary_prompt, conversation_text
                ),
            },
        ];

        let provider = self.providers.get(None)?;

        // Dynamically calculate max_tokens for summary based on what's left
        let summary_max_tokens = match provider.name() {
            "databricks" | "anthropic" => {
                let model_limit = 200_000u32;
                let current_usage = self.context_window.used_tokens;
                let available = model_limit
                    .saturating_sub(current_usage)
                    .saturating_sub(5000);
                Some(available.min(10_000))
            }
            "embedded" => {
                let model_limit = self.context_window.total_tokens;
                let current_usage = self.context_window.used_tokens;
                let available = model_limit
                    .saturating_sub(current_usage)
                    .saturating_sub(1000);
                Some(available.min(3000))
            }
            _ => {
                let available = self.context_window.remaining_tokens().saturating_sub(2000);
                Some(available.min(5000))
            }
        };

        debug!(
            "Requesting summary with max_tokens: {:?} (current usage: {} tokens)",
            summary_max_tokens, self.context_window.used_tokens
        );

        let summary_request = CompletionRequest {
            messages: summary_messages,
            max_tokens: summary_max_tokens,
            temperature: Some(0.3),
            stream: false,
            tools: None,
        };

        // Get the summary
        match provider.complete(summary_request).await {
            Ok(summary_response) => {
                self.ui_writer.print_context_status(
                    "✅ Context compacted successfully.\n",
                );

                // Get the latest user message to preserve it
                let latest_user_msg = self
                    .context_window
                    .conversation_history
                    .iter()
                    .rev()
                    .find(|m| matches!(m.role, MessageRole::User))
                    .map(|m| m.content.clone());

                // Reset context with summary
                let chars_saved = self.context_window
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
        info!("Manual context thinning triggered");
        let (message, chars_saved) = self.context_window.thin_context();
        self.thinning_events.push(chars_saved);
        message
    }

    /// Reload README.md and AGENTS.md and replace the first system message
    /// Returns Ok(true) if README was found and reloaded, Ok(false) if no README was present initially
    pub fn reload_readme(&mut self) -> Result<bool> {
        info!("Manual README reload triggered");
        
        // Check if the first message in conversation history is a system message with README content
        let has_readme = self
            .context_window
            .conversation_history
            .first()
            .map(|m| matches!(m.role, MessageRole::System) && 
                    (m.content.contains("Project README") || m.content.contains("Agent Configuration")))
            .unwrap_or(false);
        
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
            // Replace the first message with the new content
            if let Some(first_msg) = self.context_window.conversation_history.first_mut() {
                first_msg.content = combined_content;
                info!("README content reloaded successfully");
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
        stats.push_str(&format!("   • Used Tokens:       {:>10} / {}\n", 
            self.context_window.used_tokens, 
            self.context_window.total_tokens));
        stats.push_str(&format!("   • Usage Percentage:  {:>10.1}%\n", 
            self.context_window.percentage_used()));
        stats.push_str(&format!("   • Remaining Tokens:  {:>10}\n", 
            self.context_window.remaining_tokens()));
        stats.push_str(&format!("   • Cumulative Tokens: {:>10}\n", 
            self.context_window.cumulative_tokens));
        stats.push_str(&format!("   • Last Thinning:     {:>10}%\n", 
            self.context_window.last_thinning_percentage));
        stats.push('\n');
        
        // Context optimization metrics
        stats.push_str("🗜️  Context Optimization:\n");
        stats.push_str(&format!("   • Thinning Events:   {:>10}\n", 
            self.thinning_events.len()));
        if !self.thinning_events.is_empty() {
            let total_thinned: usize = self.thinning_events.iter().sum();
            let avg_thinned = total_thinned / self.thinning_events.len();
            stats.push_str(&format!("   • Total Chars Saved: {:>10}\n", total_thinned));
            stats.push_str(&format!("   • Avg Chars/Event:   {:>10}\n", avg_thinned));
        }
        
        stats.push_str(&format!("   • Summarizations:    {:>10}\n", 
            self.summarization_events.len()));
        if !self.summarization_events.is_empty() {
            let total_summarized: usize = self.summarization_events.iter().sum();
            let avg_summarized = total_summarized / self.summarization_events.len();
            stats.push_str(&format!("   • Total Chars Saved: {:>10}\n", total_summarized));
            stats.push_str(&format!("   • Avg Chars/Event:   {:>10}\n", avg_summarized));
        }
        stats.push('\n');
        
        // Performance metrics
        stats.push_str("⚡ Performance:\n");
        if !self.first_token_times.is_empty() {
            let avg_ttft = self.first_token_times.iter().sum::<Duration>() / self.first_token_times.len() as u32;
            let mut sorted_times = self.first_token_times.clone();
            sorted_times.sort();
            let median_ttft = sorted_times[sorted_times.len() / 2];
            stats.push_str(&format!("   • Avg Time to First Token:    {:>6.3}s\n", avg_ttft.as_secs_f64()));
            stats.push_str(&format!("   • Median Time to First Token: {:>6.3}s\n", median_ttft.as_secs_f64()));
        }
        stats.push('\n');
        
        // Conversation history
        stats.push_str("💬 Conversation History:\n");
        stats.push_str(&format!("   • Total Messages:    {:>10}\n", 
            self.context_window.conversation_history.len()));
        
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
        stats.push_str(&format!("   • Assistant Messages:{:>10}\n", assistant_count));
        stats.push('\n');
        
        // Tool call metrics
        stats.push_str("🔧 Tool Call Metrics:\n");
        stats.push_str(&format!("   • Total Tool Calls:  {:>10}\n", 
            self.tool_call_metrics.len()));
        
        let successful_calls = self.tool_call_metrics.iter()
            .filter(|(_, _, success)| *success)
            .count();
        let failed_calls = self.tool_call_metrics.len() - successful_calls;
        
        stats.push_str(&format!("   • Successful:        {:>10}\n", successful_calls));
        stats.push_str(&format!("   • Failed:            {:>10}\n", failed_calls));
        
        if !self.tool_call_metrics.is_empty() {
            let total_duration: Duration = self.tool_call_metrics.iter()
                .map(|(_, duration, _)| *duration)
                .sum();
            let avg_duration = total_duration / self.tool_call_metrics.len() as u32;
            
            stats.push_str(&format!("   • Total Duration:    {:>10.2}s\n", 
                total_duration.as_secs_f64()));
            stats.push_str(&format!("   • Average Duration:  {:>10.2}s\n", 
                avg_duration.as_secs_f64()));
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

    async fn stream_completion(
        &mut self,
        request: CompletionRequest,
        show_timing: bool,
    ) -> Result<TaskResult> {
        self.stream_completion_with_tools(request, show_timing)
            .await
    }

    /// Create tool definitions for native tool calling providers
    fn create_tool_definitions(enable_webdriver: bool, enable_macax: bool, enable_computer_control: bool) -> Vec<Tool> {
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
                description: "Read the entire TODO list content. Use this to view current tasks, notes, and any other information stored in the TODO list.".to_string(),
                input_schema: json!({
                    "type": "object",
                    "properties": {},
                    "required": []
                }),
            },
            Tool {
                name: "todo_write".to_string(),
                description: "Write or overwrite the entire TODO list content. This tool replaces the complete TODO list with the provided string. Use this to update tasks, add new items, or reorganize the TODO list. WARNING: This operation completely replaces the TODO list content. Make sure to include all content you want to keep, not just the changes.".to_string(),
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
        ];
        
        // Add code_search tool
        tools.push(Tool {
            name: "code_search".to_string(),
            description: "Batch syntax-aware code searches using embedded tree-sitter. Supports up to 20 searches in parallel for Rust, Python, JavaScript, TypeScript, Go, Java, C, and C++. Uses tree-sitter query syntax (S-expressions).".to_string(),
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
                                "language": { "type": "string", "enum": ["rust", "python", "javascript", "typescript", "go", "java", "c", "cpp"], "description": "Programming language to search." },
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
                    description: "Get the HTML source of the current page".to_string(),
                    input_schema: json!({
                        "type": "object",
                        "properties": {},
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
        let max_attempts = if self.is_autonomous { 6 } else { 3 };

        loop {
            attempt += 1;
            let provider = self.providers.get(None)?;

            match provider.stream(request.clone()).await {
                Ok(stream) => {
                    if attempt > 1 {
                        info!("Stream started successfully after {} attempts", attempt);
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

        // Check if we need to summarize before starting
        if self.context_window.should_summarize() {
            // Notify user about summarization
            self.ui_writer.print_context_status(&format!(
                "\n🗜️ Context window reaching capacity ({}%). Creating summary...",
                self.context_window.percentage_used() as u32
            ));

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
                Message {
                    role: MessageRole::System,
                    content: "You are a helpful assistant that creates concise summaries."
                        .to_string(),
                },
                Message {
                    role: MessageRole::User,
                    content: format!(
                        "Based on this conversation history, {}\n\nConversation:\n{}",
                        summary_prompt, conversation_text
                    ),
                },
            ];

            let provider = self.providers.get(None)?;

            // Dynamically calculate max_tokens for summary based on what's left
            // We need to ensure: used_tokens + max_tokens <= total_context_limit
            let summary_max_tokens = match provider.name() {
                "databricks" | "anthropic" => {
                    // Claude models have 200k context
                    // Calculate how much room we have left
                    let model_limit = 200_000u32;
                    let current_usage = self.context_window.used_tokens;
                    // Leave some buffer (5k tokens) for safety
                    let available = model_limit
                        .saturating_sub(current_usage)
                        .saturating_sub(5000);
                    // Cap at a reasonable summary size (10k tokens max)
                    Some(available.min(10_000))
                }
                "embedded" => {
                    // For smaller context models, be more conservative
                    let model_limit = self.context_window.total_tokens;
                    let current_usage = self.context_window.used_tokens;
                    // Leave 1k buffer
                    let available = model_limit
                        .saturating_sub(current_usage)
                        .saturating_sub(1000);
                    // Cap at 3k for embedded models
                    Some(available.min(3000))
                }
                _ => {
                    // Default: conservative approach
                    let available = self.context_window.remaining_tokens().saturating_sub(2000);
                    Some(available.min(5000))
                }
            };

            debug!(
                "Requesting summary with max_tokens: {:?} (current usage: {} tokens)",
                summary_max_tokens, self.context_window.used_tokens
            );

            let summary_request = CompletionRequest {
                messages: summary_messages,
                max_tokens: summary_max_tokens,
                temperature: Some(0.3), // Lower temperature for factual summary
                stream: false,
                tools: None,
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
                    let chars_saved = self.context_window
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

            let provider = self.providers.get(None)?;
            debug!("Got provider: {}", provider.name());

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
                provider.name().to_string(),
                provider.model().to_string(),
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
                provider.name(),
                provider.model(),
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

                        // Handle completed tool calls
                        if let Some(tool_call) = completed_tools.into_iter().next() {
                            debug!("Processing completed tool call: {:?}", tool_call);
                            
                            // Check if we should auto-compact at 90% BEFORE executing the tool
                            // We need to do this before any borrows of self
                            if self.auto_compact && self.context_window.percentage_used() >= 90.0 {
                                // Set flag to trigger summarization after this turn completes
                                // We can't do it now due to borrow checker constraints
                                self.pending_90_summarization = true;
                            }
                            
                            // Check if we should thin the context BEFORE executing the tool
                            if self.context_window.should_thin() {
                                let (thin_summary, chars_saved) = self.context_window.thin_context();
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
                                fixed_filter_json::fixed_filter_json_tool_calls(&clean_content);
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
                            self.ui_writer.println(""); // New line before tool execution

                            // Skip printing tool call details for final_output
                            if tool_call.tool != "final_output" {
                                // Tool call header
                                self.ui_writer.print_tool_header(&tool_call.tool);
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

                            let exec_start = Instant::now();
                            // Add 8-minute timeout for tool execution
                            let tool_result = match tokio::time::timeout(
                                Duration::from_secs(8 * 60), // 8 minutes
                                self.execute_tool(&tool_call),
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
                                // For final_output, display the summary without truncation
                                for line in tool_result.lines() {
                                    self.ui_writer.update_tool_output_line(line);
                                }
                                self.ui_writer.println("");
                            } else {
                                let output_lines: Vec<&str> = tool_result.lines().collect();

                                // Check if UI wants full output (machine mode) or truncated (human mode)
                                let wants_full = self.ui_writer.wants_full_output();
                                
                                // Helper function to safely truncate strings at character boundaries
                                let truncate_line = |line: &str, max_width: usize, truncate: bool| -> String {
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
                                
                                // For todo tools, show all lines without truncation
                                let is_todo_tool = tool_call.tool == "todo_read" || tool_call.tool == "todo_write";
                                let max_lines_to_show = if is_todo_tool || wants_full { output_len } else { MAX_LINES };

                                for (idx, line) in output_lines.iter().enumerate() {
                                    if !is_todo_tool && !wants_full && idx >= max_lines_to_show {
                                        break;
                                    }
                                    // Clip line to max width
                                    let clipped_line = truncate_line(line, MAX_LINE_WIDTH, !wants_full);
                                    self.ui_writer.update_tool_output_line(&clipped_line);
                                }

                                if !is_todo_tool && !wants_full && output_len > MAX_LINES {
                                    self.ui_writer.print_tool_output_summary(output_len);
                                }
                            }

                            // Check if this was a final_output tool call
                            if tool_call.tool == "final_output" {
                                // The summary was displayed above when we printed the tool result
                                // Add it to full_response so it's included in the TaskResult
                                full_response.push_str(&tool_result);
                                self.ui_writer.println("");
                                let _ttft =
                                    first_token_time.unwrap_or_else(|| stream_start.elapsed());

                                // Add timing if needed
                                let final_response = if show_timing {
                                    format!(
                                        "{}\n\n🕝 {} | 💭 {}",
                                        full_response,
                                        Self::format_duration(stream_start.elapsed()),
                                        Self::format_duration(_ttft)
                                    )
                                } else {
                                    full_response
                                };

                                return Ok(TaskResult::new(
                                    final_response,
                                    self.context_window.clone(),
                                ));
                            }

                            // Closure marker with timing
                            if tool_call.tool != "final_output" {
                                self.ui_writer
                                    .print_tool_timing(&Self::format_duration(exec_duration));
                                self.ui_writer.print_agent_prompt();
                            }

                            // Add the tool call and result to the context window using RAW unfiltered content
                            // This ensures the log file contains the true raw content including JSON tool calls
                            let tool_message = if !raw_content_for_log.trim().is_empty() {
                                Message {
                                    role: MessageRole::Assistant,
                                    content: format!(
                                        "{}\n\n{{\"tool\": \"{}\", \"args\": {}}}",
                                        raw_content_for_log.trim(),
                                        tool_call.tool,
                                        tool_call.args
                                    ),
                                }
                            } else {
                                // No text content before tool call, just include the tool call
                                Message {
                                    role: MessageRole::Assistant,
                                    content: format!(
                                        "{{\"tool\": \"{}\", \"args\": {}}}",
                                        tool_call.tool, tool_call.args
                                    ),
                                }
                            };
                            let result_message = Message {
                                role: MessageRole::User,
                                content: format!("Tool result: {}", tool_result),
                            };

                            self.context_window.add_message(tool_message);
                            self.context_window.add_message(result_message);

                            // Update the request with the new context for next iteration
                            request.messages = self.context_window.conversation_history.clone();

                            // Ensure tools are included for native providers in subsequent iterations
                            if provider.has_native_tool_calling() {
                                request.tools = Some(Self::create_tool_definitions(self.config.webdriver.enabled, self.config.macax.enabled, self.config.computer_control.enabled));
                            }

                            // DO NOT add final_display_content to full_response here!
                            // The content was already displayed during streaming and added to current_response.
                            // Adding it again would cause duplication when the agent message is printed.
                            // The only time we should add to full_response is:
                            // 1. For final_output tool (handled separately)
                            // 2. At the end when no tools were executed (handled in the "no tool executed" branch)
                            
                            tool_executed = true;

                            // Reset the JSON tool call filter state after each tool execution
                            // This ensures the filter doesn't stay in suppression mode for subsequent streaming content
                            fixed_filter_json::reset_fixed_json_tool_state();

                            // Reset parser for next iteration - this clears the text buffer
                            parser.reset();
                            
                            // Clear current_response for next iteration to prevent buffered text
                            // from being incorrectly displayed after tool execution
                            current_response.clear();
                            // Reset response_started flag for next iteration
                            response_started = false;
                            break; // Break out of current stream to start a new one
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
                                    fixed_filter_json::fixed_filter_json_tool_calls(&clean_content);

                                if !filtered_content.is_empty() {
                                    if !response_started {
                                        self.ui_writer.print_agent_prompt();
                                        response_started = true;
                                    }

                                    self.ui_writer.print_agent_response(&filtered_content);
                                    self.ui_writer.flush();
                                    current_response.push_str(&filtered_content);
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
                                        fixed_filter_json::fixed_filter_json_tool_calls(
                                            &clean_text,
                                        );

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
                                        provider.name(),
                                        provider.model()
                                    );
                                    error!("Chunks received: {}", chunks_received);
                                    error!("Parser state:");
                                    error!("  - Text buffer length: {}", parser.text_buffer_len());
                                    error!(
                                        "  - Text buffer content: {:?}",
                                        parser.get_text_content()
                                    );
                                    error!("  - Native tool calls: {:?}", parser.native_tool_calls);
                                    error!("  - Message stopped: {}", parser.is_message_stopped());
                                    error!("  - In JSON tool call: {}", parser.in_json_tool_call);
                                    error!("  - JSON tool start: {:?}", parser.json_tool_start);
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

                                // Set full_response to current_response (don't append)
                                // current_response already contains everything that was displayed
                                // Don't set full_response here - it would duplicate the output
                                // The text was already displayed during streaming
                                // Return empty string to avoid duplication
                                full_response = String::new();

                                self.ui_writer.println("");
                                let _ttft =
                                    first_token_time.unwrap_or_else(|| stream_start.elapsed());

                                // Add timing if needed
                                let final_response = if show_timing {
                                    format!(
                                        "{}\n\n⏱️ {} | 💭 {}",
                                        full_response,
                                        Self::format_duration(stream_start.elapsed()),
                                        Self::format_duration(_ttft)
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
                        let error_details = format!("Streaming error at chunk {}: {}", chunks_received + 1, error_msg);
                        
                        error!("Error type: {}", std::any::type_name_of_val(&e));
                        error!("Parser state at error: text_buffer_len={}, native_tool_calls={}, message_stopped={}",
                            parser.text_buffer_len(), parser.native_tool_calls.len(), parser.is_message_stopped());

                        // Store the error for potential logging later
                        _last_error = Some(error_details.clone());
                        
                        // Check if this is a recoverable connection error
                        let is_connection_error = error_msg.contains("unexpected EOF") 
                            || error_msg.contains("connection") 
                            || error_msg.contains("chunk size line")
                            || error_msg.contains("body error");
                        
                        if is_connection_error {
                            warn!("Connection error at chunk {}, treating as end of stream", chunks_received + 1);
                            // If we have any content or tool calls, treat this as a graceful end
                            if chunks_received > 0 && (!parser.get_text_content().is_empty() || parser.native_tool_calls.len() > 0) {
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

                if !has_response {
                    warn!(
                        "Loop exited without any response after {} iterations",
                        iteration_count
                    );
                } else {
                    // Only set full_response if it's empty (first iteration without tools)
                    // This prevents duplication when the agent responds without calling final_output
                    if full_response.is_empty() && !current_response.is_empty() {
                        full_response = current_response.clone();
                        debug!(
                            "Set full_response from current_response: {} chars",
                            full_response.len()
                        );
                    }
                    self.ui_writer.println("");
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
                        let assistant_message = Message {
                            role: MessageRole::Assistant,
                            content: raw_clean,
                        };
                        self.context_window.add_message(assistant_message);
                    }
                }

                // Add timing if needed
                let final_response = if show_timing {
                    format!(
                        "{}\n\n⏱️ {} | 💭 {}",
                        full_response,
                        Self::format_duration(stream_start.elapsed()),
                        Self::format_duration(_ttft)
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
            format!(
                "{}\n\n⏱️ {} | 💭 {}",
                full_response,
                Self::format_duration(stream_start.elapsed()),
                Self::format_duration(_ttft)
            )
        } else {
            full_response
        };

        Ok(TaskResult::new(final_response, self.context_window.clone()))
    }

    async fn execute_tool(&self, tool_call: &ToolCall) -> Result<String> {
        debug!("=== EXECUTING TOOL ===");
        debug!("Tool name: {}", tool_call.tool);
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

                        match executor
                            .execute_bash_streaming(&escaped_command, &receiver)
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
                        Ok(summary_str.to_string())
                    } else {
                        Ok("✅ Turn completed".to_string())
                    }
                } else {
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

                    match controller.take_screenshot(path, region, Some(window_id)).await {
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
                    let path = tool_call.args.get("path")
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
                let content = self.todo_content.read().await;
                if content.is_empty() {
                    Ok("📝 TODO list is empty".to_string())
                } else {
                    Ok(format!("📝 TODO list:\n{}", content.as_str()))
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
                            return Ok(format!("❌ TODO list too large: {} chars (max: {})", char_count, max_chars));
                        }

                        let mut todo = self.todo_content.write().await;
                        *todo = content_str.to_string();
                        Ok(format!("✅ TODO list updated ({} chars)", char_count))
                    } else {
                        Ok("❌ Invalid content argument".to_string())
                    }
                } else {
                    Ok("❌ Missing content argument".to_string())
                }
            }
            "webdriver_start" => {
                debug!("Processing webdriver_start tool call");
                
                if !self.config.webdriver.enabled {
                    return Ok("❌ WebDriver is not enabled. Use --webdriver flag to enable.".to_string());
                }
                
                // Check if session already exists
                let session_guard = self.webdriver_session.read().await;
                if session_guard.is_some() {
                    drop(session_guard);
                    return Ok("✅ WebDriver session already active".to_string());
                }
                drop(session_guard);
                
                // Note: Safari Remote Automation must be enabled before using WebDriver.
                // Run this once: safaridriver --enable
                // Or enable manually: Safari → Develop → Allow Remote Automation
                
                // Start safaridriver process
                let port = self.config.webdriver.safari_port;
                info!("Starting safaridriver on port {}", port);
                
                let safaridriver_result = tokio::process::Command::new("safaridriver")
                    .arg("--port")
                    .arg(port.to_string())
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null())
                    .spawn();
                
                let mut safaridriver_process = match safaridriver_result {
                    Ok(process) => process,
                    Err(e) => {
                        return Ok(format!("❌ Failed to start safaridriver: {}\n\nMake sure safaridriver is installed.", e));
                    }
                };
                
                // Wait for safaridriver to start up
                info!("Waiting for safaridriver to start...");
                tokio::time::sleep(tokio::time::Duration::from_millis(1000)).await;
                
                // Connect to SafariDriver
                match g3_computer_control::SafariDriver::with_port(port).await {
                    Ok(driver) => {
                        let session = std::sync::Arc::new(tokio::sync::Mutex::new(driver));
                        *self.webdriver_session.write().await = Some(session);
                        
                        // Store the process handle
                        *self.safaridriver_process.write().await = Some(safaridriver_process);
                        
                        info!("WebDriver session started successfully");
                        Ok("✅ WebDriver session started successfully! Safari should open automatically.".to_string())
                    }
                    Err(e) => {
                        // Kill the safaridriver process if connection failed
                        let _ = safaridriver_process.kill().await;
                        
                        Ok(format!("❌ Failed to connect to SafariDriver: {}\n\nThis might be because:\n  - Safari Remote Automation is not enabled (run: safaridriver --enable)\n  - Port {} is already in use\n  - Safari failed to start\n  - Network connectivity issue\n\nTo enable Remote Automation:\n  1. Run: safaridriver --enable (requires password, one-time setup)\n  2. Or manually: Safari → Develop → Allow Remote Automation", e, port))
                    }
                }
            }
            "webdriver_navigate" => {
                debug!("Processing webdriver_navigate tool call");
                
                if !self.config.webdriver.enabled {
                    return Ok("❌ WebDriver is not enabled. Use --webdriver flag to enable.".to_string());
                }
                
                let session_guard = self.webdriver_session.read().await;
                let session = match session_guard.as_ref() {
                    Some(s) => s.clone(),
                    None => return Ok("❌ No active WebDriver session. Call webdriver_start first.".to_string()),
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
                    return Ok("❌ WebDriver is not enabled. Use --webdriver flag to enable.".to_string());
                }
                
                let session_guard = self.webdriver_session.read().await;
                let session = match session_guard.as_ref() {
                    Some(s) => s.clone(),
                    None => return Ok("❌ No active WebDriver session. Call webdriver_start first.".to_string()),
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
                    return Ok("❌ WebDriver is not enabled. Use --webdriver flag to enable.".to_string());
                }
                
                let session_guard = self.webdriver_session.read().await;
                let session = match session_guard.as_ref() {
                    Some(s) => s.clone(),
                    None => return Ok("❌ No active WebDriver session. Call webdriver_start first.".to_string()),
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
                    return Ok("❌ WebDriver is not enabled. Use --webdriver flag to enable.".to_string());
                }
                
                let session_guard = self.webdriver_session.read().await;
                let session = match session_guard.as_ref() {
                    Some(s) => s.clone(),
                    None => return Ok("❌ No active WebDriver session. Call webdriver_start first.".to_string()),
                };
                
                let selector = match tool_call.args.get("selector").and_then(|v| v.as_str()) {
                    Some(s) => s,
                    None => return Ok("❌ Missing selector argument".to_string()),
                };
                
                let mut driver = session.lock().await;
                match driver.find_element(selector).await {
                    Ok(elem) => {
                        match elem.text().await {
                            Ok(text) => Ok(format!("Element text: {}", text)),
                            Err(e) => Ok(format!("❌ Failed to get element text: {}", e)),
                        }
                    }
                    Err(e) => Ok(format!("❌ Failed to find element '{}': {}", selector, e)),
                }
            }
            "webdriver_find_elements" => {
                debug!("Processing webdriver_find_elements tool call");
                
                if !self.config.webdriver.enabled {
                    return Ok("❌ WebDriver is not enabled. Use --webdriver flag to enable.".to_string());
                }
                
                let session_guard = self.webdriver_session.read().await;
                let session = match session_guard.as_ref() {
                    Some(s) => s.clone(),
                    None => return Ok("❌ No active WebDriver session. Call webdriver_start first.".to_string()),
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
                        Ok(format!("Found {} elements:\n{}", results.len(), results.join("\n")))
                    }
                    Err(e) => Ok(format!("❌ Failed to find elements '{}': {}", selector, e)),
                }
            }
            "webdriver_click" => {
                debug!("Processing webdriver_click tool call");
                
                if !self.config.webdriver.enabled {
                    return Ok("❌ WebDriver is not enabled. Use --webdriver flag to enable.".to_string());
                }
                
                let session_guard = self.webdriver_session.read().await;
                let session = match session_guard.as_ref() {
                    Some(s) => s.clone(),
                    None => return Ok("❌ No active WebDriver session. Call webdriver_start first.".to_string()),
                };
                
                let selector = match tool_call.args.get("selector").and_then(|v| v.as_str()) {
                    Some(s) => s,
                    None => return Ok("❌ Missing selector argument".to_string()),
                };
                
                let mut driver = session.lock().await;
                match driver.find_element(selector).await {
                    Ok(mut elem) => {
                        match elem.click().await {
                            Ok(_) => Ok(format!("✅ Clicked element '{}'", selector)),
                            Err(e) => Ok(format!("❌ Failed to click element: {}", e)),
                        }
                    }
                    Err(e) => Ok(format!("❌ Failed to find element '{}': {}", selector, e)),
                }
            }
            "webdriver_send_keys" => {
                debug!("Processing webdriver_send_keys tool call");
                
                if !self.config.webdriver.enabled {
                    return Ok("❌ WebDriver is not enabled. Use --webdriver flag to enable.".to_string());
                }
                
                let session_guard = self.webdriver_session.read().await;
                let session = match session_guard.as_ref() {
                    Some(s) => s.clone(),
                    None => return Ok("❌ No active WebDriver session. Call webdriver_start first.".to_string()),
                };
                
                let selector = match tool_call.args.get("selector").and_then(|v| v.as_str()) {
                    Some(s) => s,
                    None => return Ok("❌ Missing selector argument".to_string()),
                };
                
                let text = match tool_call.args.get("text").and_then(|v| v.as_str()) {
                    Some(t) => t,
                    None => return Ok("❌ Missing text argument".to_string()),
                };
                
                let clear_first = tool_call.args.get("clear_first")
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
                    return Ok("❌ WebDriver is not enabled. Use --webdriver flag to enable.".to_string());
                }
                
                let session_guard = self.webdriver_session.read().await;
                let session = match session_guard.as_ref() {
                    Some(s) => s.clone(),
                    None => return Ok("❌ No active WebDriver session. Call webdriver_start first.".to_string()),
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
                    return Ok("❌ WebDriver is not enabled. Use --webdriver flag to enable.".to_string());
                }
                
                let session_guard = self.webdriver_session.read().await;
                let session = match session_guard.as_ref() {
                    Some(s) => s.clone(),
                    None => return Ok("❌ No active WebDriver session. Call webdriver_start first.".to_string()),
                };
                
                let driver = session.lock().await;
                match driver.page_source().await {
                    Ok(source) => {
                        // Truncate if too long
                        if source.len() > 10000 {
                            Ok(format!("Page source ({} chars, truncated to 10000):\n{}...", source.len(), &source[..10000]))
                        } else {
                            Ok(format!("Page source ({} chars):\n{}", source.len(), source))
                        }
                    }
                    Err(e) => Ok(format!("❌ Failed to get page source: {}", e)),
                }
            }
            "webdriver_screenshot" => {
                debug!("Processing webdriver_screenshot tool call");
                
                if !self.config.webdriver.enabled {
                    return Ok("❌ WebDriver is not enabled. Use --webdriver flag to enable.".to_string());
                }
                
                let session_guard = self.webdriver_session.read().await;
                let session = match session_guard.as_ref() {
                    Some(s) => s.clone(),
                    None => return Ok("❌ No active WebDriver session. Call webdriver_start first.".to_string()),
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
                    return Ok("❌ WebDriver is not enabled. Use --webdriver flag to enable.".to_string());
                }
                
                let session_guard = self.webdriver_session.read().await;
                let session = match session_guard.as_ref() {
                    Some(s) => s.clone(),
                    None => return Ok("❌ No active WebDriver session. Call webdriver_start first.".to_string()),
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
                    return Ok("❌ WebDriver is not enabled. Use --webdriver flag to enable.".to_string());
                }
                
                let session_guard = self.webdriver_session.read().await;
                let session = match session_guard.as_ref() {
                    Some(s) => s.clone(),
                    None => return Ok("❌ No active WebDriver session. Call webdriver_start first.".to_string()),
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
                    return Ok("❌ WebDriver is not enabled. Use --webdriver flag to enable.".to_string());
                }
                
                let session_guard = self.webdriver_session.read().await;
                let session = match session_guard.as_ref() {
                    Some(s) => s.clone(),
                    None => return Ok("❌ No active WebDriver session. Call webdriver_start first.".to_string()),
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
                    return Ok("❌ WebDriver is not enabled. Use --webdriver flag to enable.".to_string());
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
                                info!("WebDriver session closed successfully");
                                
                                // Kill the safaridriver process
                                if let Some(mut process) = self.safaridriver_process.write().await.take() {
                                    if let Err(e) = process.kill().await {
                                        warn!("Failed to kill safaridriver process: {}", e);
                                    } else {
                                        info!("Safaridriver process terminated");
                                    }
                                }
                                
                                Ok("✅ WebDriver session closed and safaridriver stopped".to_string())
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
                    return Ok("❌ macOS Accessibility is not enabled. Use --macax flag to enable.".to_string());
                }
                
                let controller_guard = self.macax_controller.read().await;
                let controller = match controller_guard.as_ref() {
                    Some(c) => c,
                    None => return Ok("❌ macOS Accessibility controller not initialized.".to_string()),
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
                    return Ok("❌ macOS Accessibility is not enabled. Use --macax flag to enable.".to_string());
                }
                
                let controller_guard = self.macax_controller.read().await;
                let controller = match controller_guard.as_ref() {
                    Some(c) => c,
                    None => return Ok("❌ macOS Accessibility controller not initialized.".to_string()),
                };
                
                match controller.get_frontmost_app() {
                    Ok(app) => Ok(format!("Frontmost application: {}", app.name)),
                    Err(e) => Ok(format!("❌ Failed to get frontmost app: {}", e)),
                }
            }
            "macax_activate_app" => {
                debug!("Processing macax_activate_app tool call");
                
                if !self.config.macax.enabled {
                    return Ok("❌ macOS Accessibility is not enabled. Use --macax flag to enable.".to_string());
                }
                
                let app_name = match tool_call.args.get("app_name").and_then(|v| v.as_str()) {
                    Some(n) => n,
                    None => return Ok("❌ Missing app_name argument".to_string()),
                };
                
                let controller_guard = self.macax_controller.read().await;
                let controller = match controller_guard.as_ref() {
                    Some(c) => c,
                    None => return Ok("❌ macOS Accessibility controller not initialized.".to_string()),
                };
                
                match controller.activate_app(app_name) {
                    Ok(_) => Ok(format!("✅ Activated application: {}", app_name)),
                    Err(e) => Ok(format!("❌ Failed to activate app: {}", e)),
                }
            }
            "macax_press_key" => {
                debug!("Processing macax_press_key tool call");
                
                if !self.config.macax.enabled {
                    return Ok("❌ macOS Accessibility is not enabled. Use --macax flag to enable.".to_string());
                }
                
                let app_name = match tool_call.args.get("app_name").and_then(|v| v.as_str()) {
                    Some(n) => n,
                    None => return Ok("❌ Missing app_name argument".to_string()),
                };
                
                let key = match tool_call.args.get("key").and_then(|v| v.as_str()) {
                    Some(k) => k,
                    None => return Ok("❌ Missing key argument".to_string()),
                };
                
                let modifiers_vec: Vec<&str> = tool_call.args.get("modifiers")
                    .and_then(|v| v.as_array())
                    .map(|arr| arr.iter()
                        .filter_map(|v| v.as_str())
                        .collect())
                    .unwrap_or_default();
                
                let controller_guard = self.macax_controller.read().await;
                let controller = match controller_guard.as_ref() {
                    Some(c) => c,
                    None => return Ok("❌ macOS Accessibility controller not initialized.".to_string()),
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
                    return Ok("❌ macOS Accessibility is not enabled. Use --macax flag to enable.".to_string());
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
                    None => return Ok("❌ macOS Accessibility controller not initialized.".to_string()),
                };
                
                match controller.type_text(app_name, text) {
                    Ok(_) => Ok(format!("✅ Typed text into {}", app_name)),
                    Err(e) => Ok(format!("❌ Failed to type text: {}", e)),
                }
            }
            "vision_find_text" => {
                debug!("Processing vision_find_text tool call");
                
                if let Some(controller) = &self.computer_controller {
                    let app_name = tool_call.args.get("app_name")
                        .and_then(|v| v.as_str())
                        .ok_or_else(|| anyhow::anyhow!("Missing app_name parameter"))?;
                    
                    let text = tool_call.args.get("text")
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
                    let app_name = tool_call.args.get("app_name")
                        .and_then(|v| v.as_str())
                        .ok_or_else(|| anyhow::anyhow!("Missing app_name parameter"))?;
                    
                    let text = tool_call.args.get("text")
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
                                return Ok(format!("❌ Invalid bounding box dimensions: width={}, height={}", location.width, location.height));
                            }
                            
                            debug!("[vision_click_text] Location from find_text_in_app: x={}, y={}, width={}, height={}, text='{}'",
                                location.x, location.y, location.width, location.height, location.text);
                            
                            // Calculate center using the SCALED dimensions
                            // X: Use right edge instead of center (Vision OCR bounding box seems offset)
                            // This gives us: left edge + full width = right edge
                            // Y: top edge - half of scaled height (subtract because Y increases upward)
                            let click_x = location.x + location.width;  // Right edge
                            let half_height = location.height / 2;
                            let click_y = location.y - half_height;
                            
                            debug!("[vision_click_text] Click position calculation: x={} + {} = {} (right edge), y={} - {} = {}",
                                location.x, location.width, click_x, location.y, half_height, click_y);
                            debug!("[vision_click_text] This means: left_edge={}, center={}, right_edge={}",
                                location.x, click_x, location.x + location.width);
                            
                            match controller.click_at(click_x, click_y, Some(app_name)) {
                                Ok(_) => Ok(format!("✅ Clicked on '{}' in {} at ({}, {})", text, app_name, click_x, click_y)),
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
                    return Ok("❌ extract_text_with_boxes requires --macax flag to be enabled".to_string());
                }
                
                if let Some(controller) = &self.computer_controller {
                    let path = tool_call.args.get("path")
                        .and_then(|v| v.as_str())
                        .ok_or_else(|| anyhow::anyhow!("Missing path parameter"))?;
                    
                    // Optional: take screenshot of app first
                    let final_path = if let Some(app_name) = tool_call.args.get("app_name").and_then(|v| v.as_str()) {
                        let temp_path = format!("/tmp/g3_extract_boxes_{}.png", uuid::Uuid::new_v4());
                        match controller.take_screenshot(&temp_path, None, Some(app_name)).await {
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
                                Ok(json) => Ok(format!("✅ Extracted {} text elements:\n{}", locations.len(), json)),
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
                    let app_name = tool_call.args.get("app_name")
                        .and_then(|v| v.as_str())
                        .ok_or_else(|| anyhow::anyhow!("Missing app_name parameter"))?;
                    
                    let text = tool_call.args.get("text")
                        .and_then(|v| v.as_str())
                        .ok_or_else(|| anyhow::anyhow!("Missing text parameter"))?;
                    
                    let direction = tool_call.args.get("direction")
                        .and_then(|v| v.as_str())
                        .unwrap_or("right");
                    
                    let distance = tool_call.args.get("distance")
                        .and_then(|v| v.as_i64())
                        .unwrap_or(50) as i32;
                    
                    match controller.find_text_in_app(app_name, text).await {
                        Ok(Some(location)) => {
                            // Calculate click position based on direction
                            // location.x is LEFT edge, location.y is TOP edge (in NSScreen space)
                            let (click_x, click_y) = match direction {
                                "right" => (location.x + location.width + distance, location.y - (location.height / 2)),
                                "below" => (location.x + (location.width / 2), location.y - location.height - distance),
                                "left" => (location.x - distance, location.y - (location.height / 2)),
                                "above" => (location.x + (location.width / 2), location.y + distance),
                                _ => (location.x + location.width + distance, location.y - (location.height / 2)),
                            };
                            debug!("[vision_click_near_text] Clicking {} of text at ({}, {})", direction, click_x, click_y);
                            
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
                let request: crate::code_search::CodeSearchRequest = match serde_json::from_value(tool_call.args.clone()) {
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
                            Err(e) => {
                                Ok(format!("❌ Failed to serialize response: {}", e))
                            }
                        }
                    }
                    Err(e) => {
                        Ok(format!("❌ Code search failed: {}", e))
                    }
                }
            }
            _ => {
                warn!("Unknown tool: {}", tool_call.tool);
                Ok(format!("❓ Unknown tool: {}", tool_call.tool))
            }
        }
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
}

// Note: JSON tool call filtering is now handled by fixed_filter_json::fixed_filter_json_tool_calls

// Apply unified diff to an input string with optional [start, end) bounds
pub fn apply_unified_diff_to_string(
    file_content: &str,
    diff: &str,
    start_char: Option<usize>,
    end_char: Option<usize>,
) -> Result<String> {
    // Parse full unified diff into hunks and apply sequentially.
    let hunks = parse_unified_diff_hunks(diff);
    if hunks.is_empty() {
        anyhow::bail!(
            "Invalid diff format. Expected unified diff with @@ hunks or +/- with context lines"
        );
    }

    // Normalize line endings to avoid CRLF/CR mismatches
    let content_norm = file_content.replace("\r\n", "\n").replace('\r', "\n");

    // Determine and validate the search range
    let search_start = start_char.unwrap_or(0);
    let search_end = end_char.unwrap_or(content_norm.len());

    if search_start > content_norm.len() {
        anyhow::bail!(
            "start position {} exceeds file length {}",
            search_start,
            content_norm.len()
        );
    }
    if search_end > content_norm.len() {
        anyhow::bail!(
            "end position {} exceeds file length {}",
            search_end,
            content_norm.len()
        );
    }
    if search_start > search_end {
        anyhow::bail!(
            "start position {} is greater than end position {}",
            search_start,
            search_end
        );
    }

    // Extract the region we're going to modify, ensuring we're at char boundaries
    // Find the nearest valid char boundaries
    let start_boundary = if search_start == 0 {
        0
    } else {
        content_norm
            .char_indices()
            .find(|(i, _)| *i >= search_start)
            .map(|(i, _)| i)
            .unwrap_or(search_start)
    };
    let end_boundary = content_norm
        .char_indices()
        .find(|(i, _)| *i >= search_end)
        .map(|(i, _)| i)
        .unwrap_or(content_norm.len());

    let mut region_content = content_norm[start_boundary..end_boundary].to_string();

    // Apply hunks in order
    for (idx, (old_block, new_block)) in hunks.iter().enumerate() {
        debug!(
            "Applying hunk {}: old_len={}, new_len={}",
            idx + 1,
            old_block.len(),
            new_block.len()
        );

        if let Some(pos) = region_content.find(old_block) {
            let endpos = pos + old_block.len();
            region_content.replace_range(pos..endpos, new_block);
        } else {
            // Not found; provide helpful diagnostics with a short preview
            let preview_len = old_block.len().min(200);
            let mut old_preview = old_block[..preview_len].to_string();
            if old_block.len() > preview_len {
                old_preview.push_str("...");
            }

            let range_note = if start_char.is_some() || end_char.is_some() {
                format!(
                    " (within character range {}:{})",
                    start_boundary, end_boundary
                )
            } else {
                String::new()
            };

            anyhow::bail!(
                "Pattern not found in file{}\nHunk {} failed. Searched for:\n{}",
                range_note,
                idx + 1,
                old_preview
            );
        }
    }

    // Reconstruct the full content with the modified region
    let mut result = String::with_capacity(content_norm.len() + region_content.len());
    result.push_str(&content_norm[..start_boundary]);
    result.push_str(&region_content);
    result.push_str(&content_norm[end_boundary..]);
    Ok(result)
}

// Parse a unified diff into a list of hunks as (old_block, new_block)
// Each hunk contains the exact text to search for and the replacement text including context lines.
fn parse_unified_diff_hunks(diff: &str) -> Vec<(String, String)> {
    let mut hunks: Vec<(String, String)> = Vec::new();

    let mut old_lines: Vec<String> = Vec::new();
    let mut new_lines: Vec<String> = Vec::new();
    let mut in_hunk = false;

    for raw_line in diff.lines() {
        let line = raw_line;

        // Skip common diff headers
        if line.starts_with("diff ")
            || line.starts_with("index ")
            || line.starts_with("new file mode")
            || line.starts_with("deleted file mode")
        {
            continue;
        }

        if line.starts_with("--- ") || line.starts_with("+++ ") {
            // File header lines — ignore
            continue;
        }

        if line.starts_with("@@") {
            // Starting a new hunk — flush previous if present
            if in_hunk && (!old_lines.is_empty() || !new_lines.is_empty()) {
                hunks.push((old_lines.join("\n"), new_lines.join("\n")));
                old_lines.clear();
                new_lines.clear();
            }
            in_hunk = true;
            continue;
        }

        if !in_hunk {
            // Some minimal diffs may omit @@; start collecting once we see diff markers
            if line.starts_with(' ')
                || (line.starts_with('-') && !line.starts_with("---"))
                || (line.starts_with('+') && !line.starts_with("+++"))
            {
                in_hunk = true;
            } else {
                continue;
            }
        }

        if let Some(content) = line.strip_prefix(' ') {
            old_lines.push(content.to_string());
            new_lines.push(content.to_string());
        } else if line.starts_with('+') && !line.starts_with("+++") {
            new_lines.push(line[1..].to_string());
        } else if line.starts_with('-') && !line.starts_with("---") {
            old_lines.push(line[1..].to_string());
        } else if line.starts_with('\\') {
            // Example: "\\ No newline at end of file" — ignore
            continue;
        } else {
            // Unknown line type — ignore
        }
    }

    if in_hunk && (!old_lines.is_empty() || !new_lines.is_empty()) {
        hunks.push((old_lines.join("\n"), new_lines.join("\n")));
    }

    hunks
}

// Helper function to properly escape shell commands
fn shell_escape_command(command: &str) -> String {
    // Simple approach: if the command contains file paths with spaces,
    // we need to be more intelligent about escaping

    // For now, let's use a basic approach that handles common cases
    // This is a simplified version - a full implementation would use proper shell parsing

    let parts: Vec<&str> = command.split_whitespace().collect();
    if parts.is_empty() {
        return command.to_string();
    }

    let cmd = parts[0];
    let _args = &parts[1..];

    // Commands that typically take file paths as arguments
    let file_commands = [
        "cat", "ls", "cp", "mv", "rm", "chmod", "chown", "file", "head", "tail", "wc", "grep",
    ];

    if file_commands.contains(&cmd) {
        // For file commands, we need to be smarter about escaping
        // Let's use a different approach: use the original command but wrap it in quotes if needed

        // Check if the command already has proper quoting
        if command.contains('"') || command.contains('\'') {
            // Already has some quoting, use as-is
            return command.to_string();
        }

        // Look for file paths that need escaping (contain spaces but aren't quoted)
        let mut escaped_command = String::new();
        let mut in_quotes = false;
        let mut current_word = String::new();
        let mut words = Vec::new();

        for ch in command.chars() {
            match ch {
                ' ' if !in_quotes => {
                    if !current_word.is_empty() {
                        words.push(current_word.clone());
                        current_word.clear();
                    }
                }
                '"' => {
                    in_quotes = !in_quotes;
                    current_word.push(ch);
                }
                _ => {
                    current_word.push(ch);
                }
            }
        }

        if !current_word.is_empty() {
            words.push(current_word);
        }

        // Reconstruct the command with proper escaping
        for (i, word) in words.iter().enumerate() {
            if i > 0 {
                escaped_command.push(' ');
            }

            // If this word looks like a file path (contains / or ~) and has spaces, quote it
            if word.contains('/') || word.starts_with('~') {
                if word.contains(' ') && !word.starts_with('"') && !word.starts_with('\'') {
                    escaped_command.push_str(&format!("\"{}\"", word));
                } else {
                    escaped_command.push_str(word);
                }
            } else {
                escaped_command.push_str(word);
            }
        }

        escaped_command
    } else {
        // For non-file commands, use the original command
        command.to_string()
    }
}

// Helper function to fix mixed quotes in JSON strings
#[allow(dead_code)]
fn fix_nested_quotes_in_shell_command(json_str: &str) -> String {
    let mut _result = String::new();
    let _chars = json_str.chars().peekable();
    // Example: {"tool": "shell", "args": {"command": "python -c 'import os; print("hello")'"}

    // Look for the pattern: "command": "
    if let Some(command_start) = json_str.find(r#""command": ""#) {
        let command_value_start = command_start + r#""command": ""#.len();

        // Find the end of the command string by looking for the pattern "}
        // We need to be careful about nested quotes
        if let Some(end_marker) = json_str[command_value_start..].find(r#"" }"#) {
            let command_end = command_value_start + end_marker;

            let before = &json_str[..command_value_start];
            let command_content = &json_str[command_value_start..command_end];
            let after = &json_str[command_end..];

            // Fix the command content by properly escaping quotes
            let mut fixed_command = String::new();
            let mut chars = command_content.chars().peekable();

            while let Some(ch) = chars.next() {
                match ch {
                    '"' => {
                        // Check if this quote is already escaped
                        if fixed_command.ends_with('\\') {
                            fixed_command.push(ch); // Already escaped, keep as-is
                        } else {
                            fixed_command.push_str(r#"\""#); // Escape the quote
                        }
                    }
                    '\\' => {
                        // Check what follows the backslash
                        if let Some(&_next_ch) = chars.peek() {
                            if _next_ch == '"' {
                                // This is an escaped quote, keep the backslash
                                fixed_command.push(ch);
                            } else {
                                // Regular backslash, escape it
                                fixed_command.push_str(r#"\\"#);
                            }
                        } else {
                            // Backslash at end, escape it
                            fixed_command.push_str(r#"\\"#);
                        }
                    }
                    _ => fixed_command.push(ch),
                }
            }

            return format!("{}{}{}", before, fixed_command, after);
        }
    }

    // Fallback: if we can't parse the structure, try some basic replacements
    json_str.to_string()
}

// Helper function to fix mixed quotes in JSON (single quotes where double quotes should be)
#[allow(dead_code)]
fn fix_mixed_quotes_in_json(json_str: &str) -> String {
    let mut result = String::new();
    let mut chars = json_str.chars().peekable();
    let mut in_string = false;
    let mut string_delimiter = '"';

    while let Some(ch) = chars.next() {
        match ch {
            '"' if !in_string => {
                // Start of a double-quoted string
                in_string = true;
                string_delimiter = '"';
                result.push(ch);
            }
            '\'' if !in_string => {
                // Start of a single-quoted string - convert to double quotes
                in_string = true;
                string_delimiter = '\'';
                result.push('"'); // Convert single quote to double quote
            }
            c if in_string && c == string_delimiter => {
                // End of current string
                if string_delimiter == '\'' {
                    result.push('"'); // Convert single quote to double quote
                } else {
                    result.push(c);
                }
                in_string = false;
            }
            '"' if in_string && string_delimiter == '\'' => {
                // Double quote inside single-quoted string - escape it
                result.push_str(r#"\""#);
            }
            '\\' if in_string => {
                // Escape sequence - preserve it
                result.push(ch);
                if let Some(&_next_ch) = chars.peek() {
                    result.push(chars.next().unwrap());
                }
            }
            _ => {
                result.push(ch);
            }
        }
    }

    result
}

#[cfg(test)]
mod tests {
    use super::parse_unified_diff_hunks;

    #[test]
    fn parses_minimal_unified_diff_without_hunk_header() {
        let diff = "--- old\n-old text\n+++ new\n+new text\n";
        let hunks = parse_unified_diff_hunks(diff);
        assert_eq!(hunks.len(), 1);
        assert_eq!(hunks[0].0, "old text");
        assert_eq!(hunks[0].1, "new text");
    }

    #[test]
    fn parses_diff_with_context_and_hunk_headers() {
        let diff = "@@ -1,3 +1,3 @@\n common\n-old\n+new\n common2\n";
        let hunks = parse_unified_diff_hunks(diff);
        assert_eq!(hunks.len(), 1);
        assert_eq!(hunks[0].0, "common\nold\ncommon2");
        assert_eq!(hunks[0].1, "common\nnew\ncommon2");
    }
}

#[cfg(test)]
mod integration_tests {
    use super::apply_unified_diff_to_string;

    #[test]
    fn apply_multi_hunk_unified_diff_to_string() {
        let original = "line 1\nkeep\nold A\nkeep 2\nold B\nkeep 3\n";
        let diff =
            "@@ -1,6 +1,6 @@\n line 1\n keep\n-old A\n+new A\n keep 2\n-old B\n+new B\n keep 3\n";
        let result = apply_unified_diff_to_string(original, diff, None, None).unwrap();
        let expected = "line 1\nkeep\nnew A\nkeep 2\nnew B\nkeep 3\n";
        assert_eq!(result, expected);
    }

    #[test]
    fn apply_diff_within_range_only() {
        let original = "A\nold\nB\nold\nC\n";
        // Only the first 'old' should be replaced due to range
        let diff = "@@ -1,3 +1,3 @@\n A\n-old\n+NEW\n B\n";
        let start = 0usize; // Start of file
        let end = original.find("B\n").unwrap() + 2; // up to end of line 'B\n'
        let result = apply_unified_diff_to_string(original, diff, Some(start), Some(end)).unwrap();
        let expected = "A\nNEW\nB\nold\nC\n";
        assert_eq!(result, expected);
    }
}

// Implement Drop to clean up safaridriver process
impl<W: UiWriter> Drop for Agent<W> {
    fn drop(&mut self) {
        // Try to kill safaridriver process if it's still running
        // We need to use try_lock since we can't await in Drop
        if let Ok(mut process_guard) = self.safaridriver_process.try_write() {
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
