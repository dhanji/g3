//! Context window management for conversation history and token tracking.
//!
//! This module handles:
//! - Token counting and usage tracking
//! - Conversation history management
//! - Context thinning (reducing context size by saving large tool results to disk)
//! - Summarization triggers

use g3_providers::{Message, MessageRole, Usage};
use serde::{Deserialize, Serialize};
use tracing::{debug, warn};

use crate::paths::get_thinned_dir;
use crate::ToolCall;

// ============================================================================
// Types
// ============================================================================

/// Result of a context thinning operation.
/// Contains semantic data for the UI layer to format.
#[derive(Debug, Clone)]
pub struct ThinResult {
    pub scope: ThinScope,
    pub before_percentage: u32,
    pub after_percentage: u32,
    /// Number of tool result messages that were thinned
    pub leaned_count: usize,
    /// Number of tool calls in assistant messages that were thinned
    pub tool_call_leaned_count: usize,
    pub chars_saved: usize,
    pub had_changes: bool,
}

/// Scope for context thinning operations
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThinScope {
    /// Process only the first third of messages (incremental thinning)
    FirstThird,
    /// Process all messages (aggressive thinning, aka "skinnify")
    All,
}

impl ThinScope {
    fn file_prefix(&self) -> &'static str {
        match self {
            ThinScope::FirstThird => "leaned",
            ThinScope::All => "skinny",
        }
    }

    fn error_action(&self) -> &'static str {
        match self {
            ThinScope::FirstThird => "thinning",
            ThinScope::All => "skinnifying",
        }
    }
}

/// Represents a modification to be applied to a message during thinning
#[derive(Debug)]
enum ThinModification {
    ReplaceContent {
        index: usize,
        new_content: String,
        chars_saved: usize,
    },
}

// ============================================================================
// ContextWindow
// ============================================================================

/// Default fraction of the context window at which compaction triggers.
pub const DEFAULT_COMPACTION_THRESHOLD_PERCENT: f32 = 80.0;

/// Clamp bounds for a configured compaction threshold. Below 10% compaction
/// would fire constantly (the summary itself costs tokens); above 95% we risk
/// blowing the real API limit before the summary request can be made.
const MIN_COMPACTION_THRESHOLD_PERCENT: f32 = 10.0;
const MAX_COMPACTION_THRESHOLD_PERCENT: f32 = 95.0;

fn default_compaction_threshold_percent() -> f32 {
    DEFAULT_COMPACTION_THRESHOLD_PERCENT
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContextWindow {
    pub used_tokens: u32,
    pub total_tokens: u32,
    /// Track cumulative tokens across all interactions
    pub cumulative_tokens: u32,
    pub conversation_history: Vec<Message>,
    /// Track the last percentage at which we thinned
    pub last_thinning_percentage: u32,
    /// Fraction of `total_tokens` at which compaction triggers.
    ///
    /// This is deliberately *proportional*. An earlier version also had an
    /// absolute `used_tokens > 150_000` guard, which silently capped every
    /// large window: with the Anthropic 1M-context beta enabled, sessions
    /// compacted at ~15% of the available window instead of 80%.
    #[serde(default = "default_compaction_threshold_percent")]
    pub compaction_threshold_percent: f32,
}

impl ContextWindow {
    pub fn new(total_tokens: u32) -> Self {
        Self::with_threshold(total_tokens, DEFAULT_COMPACTION_THRESHOLD_PERCENT)
    }

    /// Create a context window with an explicit compaction threshold (percent
    /// of the buffered total). Values outside 10..=95 are clamped; non-finite
    /// values fall back to the default.
    pub fn with_threshold(total_tokens: u32, compaction_threshold_percent: f32) -> Self {
        // Apply a 1% safety buffer to absorb token estimation drift.
        // Our heuristic (chars/3 * 1.1 for code, chars/4 * 1.1 for text) slightly
        // undercounts over long sessions with hundreds of tool calls. Without this
        // buffer, accumulated drift of ~89 tokens caused API 400 errors:
        //   "prompt is too long: 200089 tokens > 200000 maximum"
        let buffered_tokens = (total_tokens as f64 * 0.99) as u32;
        let threshold = if compaction_threshold_percent.is_finite() {
            compaction_threshold_percent.clamp(
                MIN_COMPACTION_THRESHOLD_PERCENT,
                MAX_COMPACTION_THRESHOLD_PERCENT,
            )
        } else {
            warn!(
                "Non-finite compaction threshold {}; falling back to {}%",
                compaction_threshold_percent, DEFAULT_COMPACTION_THRESHOLD_PERCENT
            );
            DEFAULT_COMPACTION_THRESHOLD_PERCENT
        };
        Self {
            used_tokens: 0,
            total_tokens: buffered_tokens,
            cumulative_tokens: 0,
            conversation_history: Vec::new(),
            last_thinning_percentage: 0,
            compaction_threshold_percent: threshold,
        }
    }

    // ========================================================================
    // Message Management
    // ========================================================================

    pub fn add_message(&mut self, message: Message) {
        self.add_message_with_tokens(message, None);
    }

    /// Add a message with optional token count from the provider
    pub fn add_message_with_tokens(&mut self, message: Message, tokens: Option<u32>) {
        // Skip truly empty messages, but keep messages that have structured tool calls or tool results
        if message.content.trim().is_empty() && message.tool_calls.is_empty() && message.tool_result_id.is_none() {
            warn!("Skipping empty message to avoid API error");
            return;
        }

        let token_count = tokens.unwrap_or_else(|| Self::estimate_message_tokens(&message));
        self.used_tokens += token_count;
        self.cumulative_tokens += token_count;
        self.conversation_history.push(message);

        debug!(
            "Added message with {} tokens (used: {}/{}, cumulative: {})",
            token_count, self.used_tokens, self.total_tokens, self.cumulative_tokens
        );
    }

    /// Clear the conversation history while preserving system messages.
    /// Used by /clear command to start fresh.
    pub fn clear_conversation(&mut self) {
        let system_messages: Vec<Message> = self
            .conversation_history
            .iter()
            .filter(|m| matches!(m.role, MessageRole::System))
            .cloned()
            .collect();

        self.conversation_history = system_messages;
        self.used_tokens = self
            .conversation_history
            .iter()
            .map(|m| Self::estimate_message_tokens(m))
            .sum();
        self.last_thinning_percentage = 0;
    }

    // ========================================================================
    // Token Tracking
    // ========================================================================

    /// Update token usage from provider response.
    ///
    /// NOTE: This only updates cumulative_tokens (total API usage tracking).
    /// Calibrates `used_tokens` from the provider's actual token count when
    /// available.  Our heuristic estimation (chars/3 or chars/4) drifts
    /// over long sessions because it doesn't account for tool definitions
    /// (~4000 tokens) sent alongside the conversation history.
    ///
    /// `prompt_tokens` is the ground-truth count of every token the API
    /// received (system prompt + conversation history + tool definitions).
    /// By snapping `used_tokens` to this value after each API call, we
    /// eliminate accumulated drift and ensure `should_compact()` triggers
    /// at the right time.
    ///
    /// When `prompt_tokens` is 0 (some providers don't report it), we leave
    /// `used_tokens` unchanged and fall back to the heuristic estimate.
    pub fn update_usage_from_response(&mut self, usage: &Usage) {
        self.cumulative_tokens += usage.total_tokens;

        // Calibrate used_tokens from the provider's actual prompt token count.
        // prompt_tokens = all tokens sent to the API (system + history + tools).
        // This is the ground truth — use it to correct heuristic drift.
        if usage.prompt_tokens > 0 {
            let old = self.used_tokens;
            self.used_tokens = usage.prompt_tokens;
            debug!(
                "Calibrated used_tokens from API: {} -> {} (drift was {} tokens)",
                old, self.used_tokens, (self.used_tokens as i64 - old as i64).abs()
            );
        }

        debug!(
            "Post-calibration: used={}/{}, cumulative={}",
            self.used_tokens, self.total_tokens, self.cumulative_tokens
        );
    }

    /// Deprecated: Use update_usage_from_response instead
    pub fn update_usage(&mut self, usage: &Usage) {
        self.update_usage_from_response(usage);
    }

    /// Update cumulative token usage (for streaming) when no provider usage data is available.
    /// NOTE: This only updates cumulative_tokens, not used_tokens.
    pub fn add_streaming_tokens(&mut self, new_tokens: u32) {
        self.cumulative_tokens += new_tokens;
        debug!(
            "Updated cumulative streaming tokens: {} (used: {}/{}, cumulative: {})",
            new_tokens, self.used_tokens, self.total_tokens, self.cumulative_tokens
        );
    }

    /// Recalculate token usage based on current conversation history.
    pub fn recalculate_tokens(&mut self) {
        self.used_tokens = self
            .conversation_history
            .iter()
            .map(|m| Self::estimate_message_tokens(m))
            .sum();
        debug!("Recalculated tokens after thinning: {} tokens", self.used_tokens);
    }

    /// More accurate token estimation.
    pub fn estimate_tokens(text: &str) -> u32 {
        // Heuristic:
        // - Average English text: ~4 characters per token
        // - Code/JSON: ~3 characters per token (more symbols)
        // - Add 10% buffer for safety
        let base_estimate = if text.contains('{') || text.contains("```") || text.contains("fn ") {
            (text.len() as f32 / 3.0).ceil() as u32
        } else {
            (text.len() as f32 / 4.0).ceil() as u32
        };
        (base_estimate as f32 * 1.1).ceil() as u32
    }

    /// Estimate tokens for a full message, including structured tool_calls.
    ///
    /// When the message is sent to the API, tool_calls are serialized as
    /// structured blocks (e.g. Anthropic `tool_use`) whose input JSON counts
    /// toward the prompt token budget.  `estimate_tokens()` only looks at
    /// `message.content`, so tool_call inputs were previously invisible to
    /// the token tracker — causing used_tokens to massively undercount and
    /// compaction to never trigger.
    pub fn estimate_message_tokens(message: &Message) -> u32 {
        let mut total = Self::estimate_tokens(&message.content);
        for tc in &message.tool_calls {
            // Serialize the input Value to a string for size estimation.
            // Tool call inputs are always JSON/structured, so use the
            // code/JSON heuristic (chars/3 * 1.1).
            let input_str = tc.input.to_string();
            let base = (input_str.len() as f32 / 3.0).ceil() as u32;
            let tc_tokens = (base as f32 * 1.1).ceil() as u32;
            // Also count the tool name + id overhead (~20 tokens)
            total += tc_tokens + 20;
        }
        total
    }

    // ========================================================================
    // Capacity Queries
    // ========================================================================

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

    /// Tokens at which compaction triggers: a fraction of the buffered window.
    pub fn compaction_threshold_tokens(&self) -> u32 {
        ((self.total_tokens as f64) * (self.compaction_threshold_percent as f64) / 100.0) as u32
    }

    /// Check if we should trigger compaction.
    ///
    /// Purely proportional to the window size. Do NOT reintroduce an absolute
    /// token guard here: a hardcoded `used_tokens > 150_000` used to make
    /// 1M-context sessions compact at ~15% of their window.
    pub fn should_compact(&self) -> bool {
        if self.total_tokens == 0 {
            return false;
        }
        self.percentage_used() >= self.compaction_threshold_percent
    }

    /// Check if we should trigger context thinning.
    /// Triggers at 50%, 60%, 70%, and 80% thresholds.
    pub fn should_thin(&self) -> bool {
        let current_percentage = self.percentage_used() as u32;
        if current_percentage < 50 {
            return false;
        }

        let current_threshold = (current_percentage / 10) * 10;
        current_threshold > self.last_thinning_percentage && current_threshold <= 80
    }

    // ========================================================================
    // Compaction / Summary
    // ========================================================================

    /// Create a summary request prompt for the current conversation.
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

    /// Reset the context window with a summary.
    /// Preserves the original system prompt as the first message.
    pub fn reset_with_summary(
        &mut self,
        summary: String,
        latest_user_message: Option<String>,
    ) -> usize {
        self.reset_with_summary_and_stub(summary, latest_user_message, None)
    }

    /// Reset context window with a summary and optional ACD stub.
    /// Preserves the original system prompt as the first message.
    /// If stub is provided, it's added as a system message before the summary.
    pub fn reset_with_summary_and_stub(
        &mut self,
        summary: String,
        latest_user_message: Option<String>,
        stub: Option<String>,
    ) -> usize {
        let old_chars: usize = self
            .conversation_history
            .iter()
            .map(|m| m.content.len())
            .sum();

        // Extract preserved messages before clearing
        let preserved = self.extract_preserved_messages();

        // Clear and rebuild
        self.conversation_history.clear();
        self.used_tokens = 0;

        // Re-add preserved messages
        if let Some(system_prompt) = preserved.system_prompt {
            self.add_message(system_prompt);
        }
        if let Some(project_context) = preserved.project_context {
            self.add_message(project_context);
        }

        // Add ACD stub if provided (before summary so LLM knows about dehydrated context)
        if let Some(stub_content) = stub {
            self.add_message(Message::new(MessageRole::System, stub_content));
        }

        // Add the summary as a USER message (not System) to maintain proper alternation.
        // This allows: [Summary as User] -> [Last Assistant] -> [Latest User]
        // which is valid User/Assistant alternation.
        self.add_message(Message::new(
            MessageRole::User,
            format!("Previous conversation summary:\n\n{}", summary),
        ));

        // Add the last assistant message if present (preserves continuity)
        if let Some(assistant_msg) = preserved.last_assistant_message {
            self.add_message(assistant_msg);
        }

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

    /// Extract messages that should be preserved across compaction.
    fn extract_preserved_messages(&self) -> PreservedMessages {
        let system_prompt = self.conversation_history.first().cloned();

        // Look for project context (AGENTS.md, memory, etc.) in the second message
        let project_context = self.conversation_history.get(1).and_then(|msg| {
            if matches!(msg.role, MessageRole::System)
                && msg.content.contains("Agent Configuration")
            {
                Some(msg.clone())
            } else {
                None
            }
        });

        // Find the last assistant message in the conversation.
        // IMPORTANT: Strip tool_calls from the preserved message. After compaction,
        // the tool_result messages are summarized away, so keeping tool_calls would
        // create orphaned tool_use blocks that violate the Anthropic API constraint:
        // "Each tool_use block must have a corresponding tool_result block in the next message."
        let last_assistant_message = self
            .conversation_history
            .iter()
            .rev()
            .find(|m| matches!(m.role, MessageRole::Assistant))
            .map(|m| {
                let mut msg = m.clone();
                if !msg.tool_calls.is_empty() {
                    debug!("Stripping {} tool_calls from preserved assistant message during compaction", msg.tool_calls.len());
                    msg.tool_calls.clear();
                }
                msg
            });

        PreservedMessages {
            system_prompt,
            project_context,
            last_assistant_message,
        }
    }

    // ========================================================================
    // Context Thinning
    // ========================================================================

    /// Thin context (first third only).
    pub fn thin_context(&mut self, session_id: Option<&str>) -> ThinResult {
        self.thin_context_with_scope(session_id, ThinScope::FirstThird)
    }

    /// Thin entire context (all messages).
    pub fn thin_context_all(&mut self, session_id: Option<&str>) -> ThinResult {
        self.thin_context_with_scope(session_id, ThinScope::All)
    }

    /// Perform context thinning: scan messages and replace large tool results with file references.
    ///
    /// # Arguments
    /// * `session_id` - If provided, thinned content is saved to .g3/session/<session_id>/thinned/
    /// * `scope` - Controls which messages to process (first third or all)
    pub fn thin_context_with_scope(
        &mut self,
        session_id: Option<&str>,
        scope: ThinScope,
    ) -> ThinResult {
        let current_percentage = self.percentage_used() as u32;

        // Only update last_thinning_percentage for incremental thinning
        if scope == ThinScope::FirstThird {
            let current_threshold = (current_percentage / 10) * 10;
            self.last_thinning_percentage = current_threshold;
        }

        // Resolve output directory
        let tmp_dir = match Self::resolve_thinned_dir(session_id, scope) {
            Ok(dir) => dir,
            Err(_) => return ThinResult::no_changes(scope, current_percentage),
        };

        // Calculate message range based on scope
        let end_index = match scope {
            ThinScope::FirstThird => (self.conversation_history.len() / 3).max(1),
            ThinScope::All => self.conversation_history.len(),
        };

        // Collect and apply modifications
        let modifications =
            self.collect_thin_modifications(end_index, &tmp_dir, scope.file_prefix());
        let (leaned_count, tool_call_leaned_count, chars_saved) =
            self.apply_thin_modifications(&modifications);

        // Recalculate token usage after thinning
        self.recalculate_tokens();

        ThinResult {
            scope,
            before_percentage: current_percentage,
            after_percentage: self.percentage_used() as u32,
            leaned_count,
            tool_call_leaned_count,
            chars_saved,
            had_changes: leaned_count > 0 || tool_call_leaned_count > 0,
        }
    }

    /// Resolve the directory for storing thinned content.
    fn resolve_thinned_dir(session_id: Option<&str>, scope: ThinScope) -> Result<String, String> {
        let dir = if let Some(sid) = session_id {
            get_thinned_dir(sid).to_string_lossy().to_string()
        } else {
            shellexpand::tilde("~/tmp").to_string()
        };

        if let Err(e) = std::fs::create_dir_all(&dir) {
            warn!("Failed to create thinned directory: {}", e);
            return Err(format!(
                "⚠️  Context {} failed: could not create directory",
                scope.error_action()
            ));
        }

        Ok(dir)
    }

    /// Collect all modifications needed for thinning without mutating.
    fn collect_thin_modifications(
        &self,
        end_index: usize,
        tmp_dir: &str,
        file_prefix: &str,
    ) -> Vec<ThinModification> {
        let mut modifications = Vec::new();

        for i in 0..end_index {
            let Some(message) = self.conversation_history.get(i) else {
                continue;
            };

            // Process User messages that look like tool results
            if matches!(message.role, MessageRole::User)
                && message.content.starts_with("Tool result:")
                && !self.is_todo_tool_result(i)
                && message.content.len() > 500
            {
                if let Some(m) =
                    Self::create_tool_result_modification(&message.content, i, tmp_dir, file_prefix)
                {
                    modifications.push(m);
                }
            }

            // Process Assistant messages that contain tool calls with large arguments
            if matches!(message.role, MessageRole::Assistant) {
                if let Some(m) =
                    Self::create_tool_call_modification(&message.content, i, tmp_dir, file_prefix)
                {
                    modifications.push(m);
                }
            }
        }

        modifications
    }

    /// Apply collected modifications and return counts.
    fn apply_thin_modifications(
        &mut self,
        modifications: &[ThinModification],
    ) -> (usize, usize, usize) {
        let mut leaned_count = 0;
        let mut tool_call_leaned_count = 0;
        let mut chars_saved = 0;

        for modification in modifications {
            let ThinModification::ReplaceContent {
                index,
                new_content,
                chars_saved: saved,
            } = modification;

            if let Some(msg) = self.conversation_history.get_mut(*index) {
                if msg.content.starts_with("Tool result:") {
                    leaned_count += 1;
                } else {
                    tool_call_leaned_count += 1;
                }
                msg.content = new_content.clone();
                chars_saved += saved;
            }
        }

        (leaned_count, tool_call_leaned_count, chars_saved)
    }

    /// Check if message at index i is a result of a TODO tool call.
    fn is_todo_tool_result(&self, i: usize) -> bool {
        if i == 0 {
            return false;
        }

        self.conversation_history
            .get(i - 1)
            .map(|prev| {
                matches!(prev.role, MessageRole::Assistant)
                    && (prev.content.contains(r#""tool":"todo_read""#)
                        || prev.content.contains(r#""tool":"todo_write""#)
                        || prev.content.contains(r#""tool": "todo_read""#)
                        || prev.content.contains(r#""tool": "todo_write""#))
            })
            .unwrap_or(false)
    }

    /// Create a modification for thinning a tool result message.
    fn create_tool_result_modification(
        content: &str,
        index: usize,
        tmp_dir: &str,
        file_prefix: &str,
    ) -> Option<ThinModification> {
        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let filename = format!("{}_tool_result_{}_{}.txt", file_prefix, timestamp, index);
        let file_path = format!("{}/{}", tmp_dir, filename);

        if let Err(e) = std::fs::write(&file_path, content) {
            warn!("Failed to write thinned content to {}: {}", file_path, e);
            return None;
        }

        let original_len = content.len();
        let new_content = format!("Tool result saved to {}", file_path);
        let chars_saved = original_len - new_content.len();

        debug!(
            "Thinned tool result {} ({} chars) to {}",
            index, original_len, file_path
        );

        Some(ThinModification::ReplaceContent {
            index,
            new_content,
            chars_saved,
        })
    }

    /// Create a modification for thinning tool calls in an assistant message.
    fn create_tool_call_modification(
        content: &str,
        index: usize,
        tmp_dir: &str,
        file_prefix: &str,
    ) -> Option<ThinModification> {
        // Look for JSON tool call patterns
        let tool_call_start = content
            .find(r#"{"tool":"#)
            .or_else(|| content.find(r#"{ "tool":"#))
            .or_else(|| content.find(r#"{"tool" :"#))
            .or_else(|| content.find(r#"{ "tool" :"#))?;

        let json_portion = &content[tool_call_start..];
        let json_end = Self::find_json_end(json_portion)?;
        let json_str = &json_portion[..=json_end];

        let mut tool_call: ToolCall = serde_json::from_str(json_str).ok()?;
        let mut modified = false;
        let mut chars_saved = 0;

        // Handle write_file tool calls
        if tool_call.tool == "write_file" {
            if let Some((saved, new_args)) =
                Self::thin_write_file_args(&tool_call.args, index, tmp_dir, file_prefix)
            {
                tool_call.args = new_args;
                modified = true;
                chars_saved += saved;
            }
        }

        // Handle str_replace tool calls
        if tool_call.tool == "str_replace" {
            if let Some((saved, new_args)) =
                Self::thin_str_replace_args(&tool_call.args, index, tmp_dir, file_prefix)
            {
                tool_call.args = new_args;
                modified = true;
                chars_saved += saved;
            }
        }

        if !modified {
            return None;
        }

        // Reconstruct the message
        let prefix = &content[..tool_call_start];
        let suffix = &content[tool_call_start + json_str.len()..];
        let new_json = serde_json::to_string(&tool_call).ok()?;
        let new_content = format!("{}{}{}", prefix, new_json, suffix);

        Some(ThinModification::ReplaceContent {
            index,
            new_content,
            chars_saved,
        })
    }

    /// Thin write_file args by saving content to file.
    /// Returns (chars_saved, new_args) if thinned.
    fn thin_write_file_args(
        args: &serde_json::Value,
        index: usize,
        tmp_dir: &str,
        file_prefix: &str,
    ) -> Option<(usize, serde_json::Value)> {
        let args_obj = args.as_object()?;
        let content_str = args_obj.get("content")?.as_str()?;

        if content_str.len() <= 500 {
            return None;
        }

        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let filename = format!(
            "{}_write_file_content_{}_{}.txt",
            file_prefix, timestamp, index
        );
        let file_path = format!("{}/{}", tmp_dir, filename);

        std::fs::write(&file_path, content_str).ok()?;

        let content_len = content_str.len();
        let mut new_args = args_obj.clone();
        new_args.insert(
            "content".to_string(),
            serde_json::Value::String(format!("<content saved to {}>", file_path)),
        );

        debug!(
            "Thinned write_file content {} ({} chars) to {}",
            index, content_len, file_path
        );

        Some((content_len, serde_json::Value::Object(new_args)))
    }

    /// Thin str_replace args by saving diff to file.
    /// Returns (chars_saved, new_args) if thinned.
    fn thin_str_replace_args(
        args: &serde_json::Value,
        index: usize,
        tmp_dir: &str,
        file_prefix: &str,
    ) -> Option<(usize, serde_json::Value)> {
        let args_obj = args.as_object()?;
        let diff_str = args_obj.get("diff")?.as_str()?;

        if diff_str.len() <= 500 {
            return None;
        }

        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let filename = format!(
            "{}_str_replace_diff_{}_{}.txt",
            file_prefix, timestamp, index
        );
        let file_path = format!("{}/{}", tmp_dir, filename);

        std::fs::write(&file_path, diff_str).ok()?;

        let diff_len = diff_str.len();
        let mut new_args = args_obj.clone();
        new_args.insert(
            "diff".to_string(),
            serde_json::Value::String(format!("<diff saved to {}>", file_path)),
        );

        debug!(
            "Thinned str_replace diff {} ({} chars) to {}",
            index, diff_len, file_path
        );

        Some((diff_len, serde_json::Value::Object(new_args)))
    }

    // ========================================================================
    // JSON Utilities
    // ========================================================================

    /// Find the end position of a JSON object.
    pub fn find_json_end(json_str: &str) -> Option<usize> {
        crate::utils::find_json_object_end(json_str)
    }
}

// ============================================================================
// Helper Types
// ============================================================================

/// Messages preserved across compaction.
struct PreservedMessages {
    system_prompt: Option<Message>,
    project_context: Option<Message>,
    last_assistant_message: Option<Message>,
}

impl ThinResult {
    /// Create a ThinResult indicating no changes were made.
    fn no_changes(scope: ThinScope, percentage: u32) -> Self {
        Self {
            scope,
            before_percentage: percentage,
            after_percentage: percentage,
            leaned_count: 0,
            tool_call_leaned_count: 0,
            chars_saved: 0,
            had_changes: false,
        }
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use g3_providers::MessageToolCall;

    #[test]
    fn test_new_context_window() {
        let cw = ContextWindow::new(100_000);
        assert_eq!(cw.used_tokens, 0);
        assert_eq!(cw.total_tokens, 99_000); // 1% buffer: 100_000 * 0.99
        assert_eq!(cw.cumulative_tokens, 0);
        assert!(cw.conversation_history.is_empty());
    }

    #[test]
    fn test_1pct_buffer_200k() {
        // The exact scenario from the screenshot: 200k Anthropic context window
        let cw = ContextWindow::new(200_000);
        assert_eq!(cw.total_tokens, 198_000, "200k * 0.99 = 198k");
    }

    #[test]
    fn test_1pct_buffer_zero() {
        // Edge case: zero tokens should not underflow
        let cw = ContextWindow::new(0);
        assert_eq!(cw.total_tokens, 0);
    }

    #[test]
    fn test_1pct_buffer_small() {
        // Small context window: 100 * 0.99 = 99
        let cw = ContextWindow::new(100);
        assert_eq!(cw.total_tokens, 99);
    }

    #[test]
    fn test_1pct_buffer_percentage_uses_buffered_total() {
        // percentage_used() should report against the buffered limit
        let mut cw = ContextWindow::new(200_000);
        assert_eq!(cw.total_tokens, 198_000);

        // Set used_tokens to 198_000 (100% of buffered, 99% of raw)
        cw.used_tokens = 198_000;
        let pct = cw.percentage_used();
        assert!(
            (pct - 100.0).abs() < 0.01,
            "Should be ~100% of buffered limit, got {:.2}%",
            pct,
        );
        // This means compaction triggers well before the raw API limit
        assert!(cw.should_compact());
    }

    #[test]
    fn test_percentage_used() {
        let mut cw = ContextWindow::new(100);
        // total_tokens is 99 after 1% buffer
        cw.used_tokens = 50;
        let expected = (50.0 / 99.0) * 100.0;
        assert!((cw.percentage_used() - expected).abs() < 0.01);
    }

    #[test]
    fn test_remaining_tokens() {
        let mut cw = ContextWindow::new(100);
        // total_tokens is 99 after 1% buffer
        cw.used_tokens = 30;
        assert_eq!(cw.remaining_tokens(), 69); // 99 - 30
    }

    #[test]
    fn test_should_compact_at_80_percent() {
        let mut cw = ContextWindow::new(100);
        cw.used_tokens = 79;
        assert!(!cw.should_compact());
        cw.used_tokens = 80;
        assert!(cw.should_compact());
    }

    /// Regression: an absolute `used_tokens > 150_000` guard used to force
    /// compaction at ~15% of a 1M-context window. The threshold must be
    /// proportional to the window, not a hardcoded token count.
    #[test]
    fn test_1m_window_does_not_compact_at_150k() {
        let mut cw = ContextWindow::new(1_000_000);
        assert_eq!(cw.total_tokens, 990_000);

        cw.used_tokens = 150_001;
        assert!(
            !cw.should_compact(),
            "1M window at {:.1}% must not compact (old absolute 150k guard)",
            cw.percentage_used(),
        );

        // 200k used is still only ~20% of the window
        cw.used_tokens = 200_000;
        assert!(!cw.should_compact(), "1M window at ~20% must not compact");
    }

    #[test]
    fn test_1m_window_compacts_at_80_percent() {
        let mut cw = ContextWindow::new(1_000_000);
        let threshold = cw.compaction_threshold_tokens(); // 80% of 990k = 792k
        assert_eq!(threshold, 792_000);

        cw.used_tokens = threshold - 1_000;
        assert!(!cw.should_compact());

        cw.used_tokens = threshold;
        assert!(cw.should_compact());
    }

    /// No regression for the default Anthropic window: 200k still compacts at
    /// 80% of the buffered total (158.4k).
    #[test]
    fn test_200k_window_still_compacts_at_80_percent() {
        let mut cw = ContextWindow::new(200_000);
        assert_eq!(cw.compaction_threshold_tokens(), 158_400);

        cw.used_tokens = 150_000;
        assert!(!cw.should_compact(), "150k is below 80% of 198k");

        cw.used_tokens = 158_400;
        assert!(cw.should_compact());
    }

    #[test]
    fn test_configured_threshold_is_honoured() {
        let mut cw = ContextWindow::with_threshold(1_000_000, 60.0);
        assert_eq!(cw.compaction_threshold_tokens(), 594_000); // 60% of 990k

        cw.used_tokens = 500_000;
        assert!(!cw.should_compact());

        cw.used_tokens = 594_000;
        assert!(cw.should_compact());
    }

    #[test]
    fn test_threshold_is_clamped() {
        // Absurdly low: compaction would fire on the system prompt alone
        let cw = ContextWindow::with_threshold(200_000, 0.0);
        assert_eq!(cw.compaction_threshold_percent, 10.0);

        // Absurdly high: no room left to make the summary request
        let cw = ContextWindow::with_threshold(200_000, 500.0);
        assert_eq!(cw.compaction_threshold_percent, 95.0);

        // Negative
        let cw = ContextWindow::with_threshold(200_000, -20.0);
        assert_eq!(cw.compaction_threshold_percent, 10.0);

        // Non-finite falls back to the default
        let cw = ContextWindow::with_threshold(200_000, f32::NAN);
        assert_eq!(cw.compaction_threshold_percent, 80.0);
    }

    #[test]
    fn test_zero_window_never_compacts() {
        // Degenerate window: must not divide by zero or spuriously compact
        let mut cw = ContextWindow::new(0);
        assert_eq!(cw.total_tokens, 0);
        assert_eq!(cw.percentage_used(), 0.0);
        assert!(!cw.should_compact());

        cw.used_tokens = 999_999;
        assert!(!cw.should_compact(), "zero-capacity window must not compact");
        assert_eq!(cw.compaction_threshold_tokens(), 0);
    }

    /// A resumed session whose serialized form predates the threshold field
    /// must deserialize to the default rather than 0% (which would compact
    /// immediately).
    #[test]
    fn test_deserialize_without_threshold_field_uses_default() {
        let json = r#"{
            "used_tokens": 40000,
            "total_tokens": 990000,
            "cumulative_tokens": 40000,
            "conversation_history": [],
            "last_thinning_percentage": 0
        }"#;
        let cw: ContextWindow = serde_json::from_str(json).expect("legacy shape must deserialize");
        assert_eq!(cw.compaction_threshold_percent, 80.0);
        assert!(!cw.should_compact(), "40k of 990k must not compact");
    }

    #[test]
    fn test_should_thin_thresholds() {
        let mut cw = ContextWindow::new(100);

        // Below 50% - should not thin
        cw.used_tokens = 49;
        assert!(!cw.should_thin());

        // At 50% - should thin (first time)
        cw.used_tokens = 50;
        assert!(cw.should_thin());

        // After thinning at 50%, shouldn't thin again until 60%
        cw.last_thinning_percentage = 50;
        cw.used_tokens = 55;
        assert!(!cw.should_thin());

        // At 60% - should thin again
        cw.used_tokens = 60;
        assert!(cw.should_thin());
    }

    #[test]
    fn test_estimate_tokens_regular_text() {
        let text = "Hello world, this is a test.";
        let tokens = ContextWindow::estimate_tokens(text);
        assert!(tokens > 0 && tokens < 20);
    }

    #[test]
    fn test_estimate_tokens_code() {
        let code = "fn main() { println!(\"hello\"); }";
        let tokens = ContextWindow::estimate_tokens(code);
        assert!(tokens > 0);
    }

    #[test]
    fn test_find_json_end() {
        assert_eq!(ContextWindow::find_json_end("{}"), Some(1));
        assert_eq!(ContextWindow::find_json_end(r#"{"a": 1}"#), Some(7));
        assert_eq!(ContextWindow::find_json_end(r#"{"a": {"b": 2}}"#), Some(14));
        assert_eq!(ContextWindow::find_json_end("{incomplete"), None);
    }

    #[test]
    fn test_thin_scope_properties() {
        assert_eq!(ThinScope::FirstThird.file_prefix(), "leaned");
        assert_eq!(ThinScope::All.file_prefix(), "skinny");
        assert_eq!(ThinScope::FirstThird.error_action(), "thinning");
        assert_eq!(ThinScope::All.error_action(), "skinnifying");
    }

    // ====================================================================
    // Compaction: tool_call stripping tests
    // ====================================================================

    /// Helper to create a Message with tool_calls
    fn assistant_msg_with_tool_calls(content: &str, tool_call_ids: &[&str]) -> Message {
        let mut msg = Message::new(MessageRole::Assistant, content.to_string());
        msg.tool_calls = tool_call_ids
            .iter()
            .map(|id| MessageToolCall {
                id: id.to_string(),
                name: "read_file".to_string(),
                input: serde_json::json!({"file_path": "/tmp/test.rs"}),
            })
            .collect();
        msg
    }

    #[test]
    fn test_compaction_strips_tool_calls_from_last_assistant() {
        // Reproduce the exact bug: assistant message with tool_calls gets preserved
        // across compaction, creating orphaned tool_use blocks.
        let mut cw = ContextWindow::new(100_000);

        // Build a conversation: system, user, assistant(with tool_call), user(tool_result), user(new input)
        cw.add_message(Message::new(MessageRole::System, "You are a helpful assistant.".to_string()));
        cw.add_message(Message::new(MessageRole::User, "Read the file please.".to_string()));
        cw.add_message(assistant_msg_with_tool_calls(
            "Let me read that file for you.",
            &["toolu_01QRFL8vGKDjZZkfHR586Srb"],
        ));
        let mut tool_result = Message::new(MessageRole::User, "Tool result: file contents here...".to_string());
        tool_result.tool_result_id = Some("toolu_01QRFL8vGKDjZZkfHR586Srb".to_string());
        cw.add_message(tool_result);

        // Now compact
        cw.reset_with_summary(
            "Summary: user asked to read a file, assistant read it.".to_string(),
            Some("Now do something else.".to_string()),
        );

        // Find the preserved assistant message
        let assistant_msgs: Vec<&Message> = cw
            .conversation_history
            .iter()
            .filter(|m| matches!(m.role, MessageRole::Assistant))
            .collect();

        assert_eq!(assistant_msgs.len(), 1, "Should have exactly one assistant message");
        let preserved = assistant_msgs[0];

        // The key assertion: tool_calls must be stripped
        assert!(
            preserved.tool_calls.is_empty(),
            "tool_calls should be stripped from preserved assistant message, but found: {:?}",
            preserved.tool_calls
        );

        // Text content should be preserved
        assert!(preserved.content.contains("Let me read that file"));
    }

    #[test]
    fn test_compaction_drops_assistant_with_only_tool_calls_no_text() {
        // Edge case: assistant message has tool_calls but empty content.
        // After stripping tool_calls, the message is empty and should be dropped.
        let mut cw = ContextWindow::new(100_000);

        cw.add_message(Message::new(MessageRole::System, "You are a helpful assistant.".to_string()));
        cw.add_message(Message::new(MessageRole::User, "Do something.".to_string()));

        // Assistant message with tool_calls but empty text content
        let mut assistant = Message::new(MessageRole::Assistant, "".to_string());
        assistant.tool_calls = vec![MessageToolCall {
            id: "toolu_abc123".to_string(),
            name: "shell".to_string(),
            input: serde_json::json!({"command": "ls"}),
        }];
        // Force-add it (bypassing the empty check since it has tool_calls)
        cw.conversation_history.push(assistant);

        let mut tool_result = Message::new(MessageRole::User, "Tool result: file1 file2".to_string());
        tool_result.tool_result_id = Some("toolu_abc123".to_string());
        cw.add_message(tool_result);

        // Compact
        cw.reset_with_summary(
            "Summary: ran ls command.".to_string(),
            Some("What next?".to_string()),
        );

        // The empty assistant message (after tool_call stripping) should be dropped
        let assistant_msgs: Vec<&Message> = cw
            .conversation_history
            .iter()
            .filter(|m| matches!(m.role, MessageRole::Assistant))
            .collect();

        assert_eq!(
            assistant_msgs.len(), 0,
            "Empty assistant message (after tool_call stripping) should be dropped"
        );
    }

    #[test]
    fn test_compaction_preserves_normal_assistant_message() {
        // Normal case: assistant message without tool_calls should be preserved as-is.
        let mut cw = ContextWindow::new(100_000);

        cw.add_message(Message::new(MessageRole::System, "You are a helpful assistant.".to_string()));
        cw.add_message(Message::new(MessageRole::User, "Hello!".to_string()));
        cw.add_message(Message::new(
            MessageRole::Assistant,
            "Hello! How can I help you today?".to_string(),
        ));

        cw.reset_with_summary(
            "Summary: greeting exchange.".to_string(),
            Some("Tell me a joke.".to_string()),
        );

        let assistant_msgs: Vec<&Message> = cw
            .conversation_history
            .iter()
            .filter(|m| matches!(m.role, MessageRole::Assistant))
            .collect();

        assert_eq!(assistant_msgs.len(), 1);
        assert!(assistant_msgs[0].tool_calls.is_empty());
        assert!(assistant_msgs[0].content.contains("Hello! How can I help you today?"));
    }

    // ====================================================================
    // Tool-call token tracking tests
    // ====================================================================

    #[test]
    fn test_estimate_message_tokens_content_only() {
        // Message without tool_calls should behave like estimate_tokens
        let msg = Message::new(MessageRole::Assistant, "Hello world".to_string());
        let msg_tokens = ContextWindow::estimate_message_tokens(&msg);
        let text_tokens = ContextWindow::estimate_tokens("Hello world");
        assert_eq!(msg_tokens, text_tokens);
    }

    #[test]
    fn test_estimate_message_tokens_with_tool_calls() {
        // Message with tool_calls should count both content and tool input
        let mut msg = Message::new(MessageRole::Assistant, "Let me read that.".to_string());
        msg.tool_calls.push(MessageToolCall {
            id: "toolu_abc".to_string(),
            name: "shell".to_string(),
            input: serde_json::json!({"command": "echo hello world this is a moderately long command string for testing purposes"}),
        });

        let msg_tokens = ContextWindow::estimate_message_tokens(&msg);
        let text_only_tokens = ContextWindow::estimate_tokens("Let me read that.");

        // Must be strictly greater than text-only estimate
        assert!(
            msg_tokens > text_only_tokens,
            "estimate_message_tokens ({}) should be > text-only estimate ({})",
            msg_tokens, text_only_tokens
        );

        // The tool input is ~90 chars of JSON → ~30 tokens + 20 overhead = ~50 extra
        assert!(
            msg_tokens >= text_only_tokens + 20,
            "tool_call should add at least 20 tokens overhead, got delta={}",
            msg_tokens - text_only_tokens
        );
    }

    #[test]
    fn test_estimate_message_tokens_empty_content_with_tool_calls() {
        // Message with empty content but tool_calls should still count tool input
        let mut msg = Message::new(MessageRole::Assistant, "".to_string());
        msg.tool_calls.push(MessageToolCall {
            id: "toolu_xyz".to_string(),
            name: "write_envelope".to_string(),
            input: serde_json::json!({"facts": "a]".repeat(1000)}),
        });

        let tokens = ContextWindow::estimate_message_tokens(&msg);
        assert!(tokens > 100, "Large tool input should produce significant token count, got {}", tokens);
    }

    #[test]
    fn test_estimate_message_tokens_large_tool_input() {
        // Simulate the write_envelope case: 3751 chars of YAML in tool input
        let large_yaml = "a: b\n".repeat(750); // ~3750 chars
        let mut msg = Message::new(MessageRole::Assistant, "Writing envelope.".to_string());
        msg.tool_calls.push(MessageToolCall {
            id: "toolu_env".to_string(),
            name: "write_envelope".to_string(),
            input: serde_json::json!({"facts": large_yaml}),
        });

        let tokens = ContextWindow::estimate_message_tokens(&msg);
        // 3750 chars of JSON / 3 * 1.1 ≈ 1375 tokens + 20 overhead + content tokens
        assert!(tokens > 1000, "Large tool input should produce >1000 tokens, got {}", tokens);
    }

    #[test]
    fn test_add_message_counts_tool_call_tokens() {
        let mut cw = ContextWindow::new(200_000);

        // Add a message with tool_calls
        let mut msg = Message::new(MessageRole::Assistant, "Running command.".to_string());
        msg.tool_calls.push(MessageToolCall {
            id: "toolu_1".to_string(),
            name: "shell".to_string(),
            input: serde_json::json!({"command": "x]".repeat(500)}),
        });

        cw.add_message(msg);

        // used_tokens should reflect the tool_call input, not just the content
        let content_only = ContextWindow::estimate_tokens("Running command.");
        assert!(
            cw.used_tokens > content_only,
            "used_tokens ({}) should be > content-only estimate ({})",
            cw.used_tokens, content_only
        );
    }

    #[test]
    fn test_should_compact_triggers_with_tool_call_tokens() {
        // Reproduce the core bug: tool_calls push real usage past 80% but
        // the old code would have tracked only content tokens (staying low).
        let mut cw = ContextWindow::new(1000);

        // Add a message with small content but large tool input
        // Content: ~5 tokens. Tool input: ~1000 chars → ~367 tokens + 20 = ~387
        // Total: ~392 tokens → 39% of 1000. Not enough alone.
        // Add several to push past 80%.
        for i in 0..3 {
            let mut msg = Message::new(MessageRole::Assistant, "ok".to_string());
            msg.tool_calls.push(MessageToolCall {
                id: format!("toolu_{}", i),
                name: "shell".to_string(),
                input: serde_json::json!({"command": "x".repeat(800)}),
            });
            cw.add_message(msg);
            // Also add a tool result
            let mut result = Message::new(MessageRole::User, "Tool result: done".to_string());
            result.tool_result_id = Some(format!("toolu_{}", i));
            cw.add_message(result);
        }

        // With tool_call tracking, should_compact should trigger
        assert!(
            cw.should_compact(),
            "should_compact should trigger when tool_calls push past 80%, percentage={}%",
            cw.percentage_used()
        );
    }

    #[test]
    fn test_recalculate_tokens_includes_tool_calls() {
        let mut cw = ContextWindow::new(200_000);

        let mut msg = Message::new(MessageRole::Assistant, "hi".to_string());
        msg.tool_calls.push(MessageToolCall {
            id: "toolu_r".to_string(),
            name: "shell".to_string(),
            input: serde_json::json!({"command": "x".repeat(600)}),
        });
        cw.add_message(msg);

        let tokens_after_add = cw.used_tokens;
        cw.recalculate_tokens();

        assert_eq!(cw.used_tokens, tokens_after_add,
            "recalculate_tokens should produce same result as add_message for tool_call messages");
    }
}
