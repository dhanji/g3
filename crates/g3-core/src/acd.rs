//! Aggressive Context Dehydration (ACD) module.
//!
//! This module provides functionality for dehydrating conversation history
//! into persistent fragments that can be rehydrated on demand. This allows
//! for much longer effective sessions by saving context to disk and replacing
//! it with compact stubs.
//!
//! ## Design
//!
//! When ACD is enabled (`--acd` flag), after every compaction/summary:
//! 1. All messages before the summary are saved to a fragment file
//! 2. Those messages are replaced with a compact stub in the context
//! 3. The stub contains metadata to help decide if rehydration is worthwhile
//! 4. Fragments form a linked list via `preceding_fragment_id`
//!
//! ## Fragment Storage
//!
//! Fragments are stored in `.g3/sessions/<session_id>/fragments/`
//! as JSON files named `fragment_<id>.json`.

use anyhow::{Context, Result};
use g3_providers::Message;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use tracing::{debug, warn};

use crate::paths::get_fragments_dir;
use crate::ToolCall;

/// A dehydrated context fragment containing saved conversation history.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Fragment {
    /// Unique identifier for this fragment
    pub fragment_id: String,
    /// When this fragment was created
    pub created_at: String,
    /// The dehydrated messages
    pub messages: Vec<Message>,
    /// Total number of messages
    pub message_count: usize,
    /// Number of user messages
    pub user_message_count: usize,
    /// Number of assistant messages
    pub assistant_message_count: usize,
    /// Summary of tool calls by tool name
    pub tool_call_summary: HashMap<String, usize>,
    /// Estimated token count for this fragment
    pub estimated_tokens: u32,
    /// Brief topic hints extracted from the conversation
    pub topics: Vec<String>,
    /// ID of the preceding fragment in the chain (None for first fragment)
    pub preceding_fragment_id: Option<String>,
    /// The first user message (task) in full for forensics
    pub first_user_message: Option<String>,
}

impl Fragment {
    /// Create a new fragment from a slice of messages.
    ///
    /// # Arguments
    /// * `messages` - The messages to dehydrate
    /// * `preceding_fragment_id` - ID of the previous fragment in the chain
    pub fn new(messages: Vec<Message>, preceding_fragment_id: Option<String>) -> Self {
        let fragment_id = generate_fragment_id();
        let created_at = chrono::Utc::now().to_rfc3339();

        // Count messages by role
        let mut user_count = 0;
        let mut assistant_count = 0;
        for msg in &messages {
            match msg.role {
                g3_providers::MessageRole::User => user_count += 1,
                g3_providers::MessageRole::Assistant => assistant_count += 1,
                g3_providers::MessageRole::System => {}
            }
        }

        // Extract tool call summary
        let tool_call_summary = extract_tool_call_summary(&messages);

        // Estimate tokens
        let estimated_tokens = estimate_fragment_tokens(&messages);

        // Extract topics
        let topics = extract_topics(&messages);

        // Extract first user message for forensics
        let first_user_message = messages
            .iter()
            .find(|m| matches!(m.role, g3_providers::MessageRole::User))
            .filter(|m| !m.content.starts_with("Tool result"))
            .map(|m| m.content.clone());

        Self {
            fragment_id,
            created_at,
            message_count: messages.len(),
            user_message_count: user_count,
            assistant_message_count: assistant_count,
            tool_call_summary,
            estimated_tokens,
            topics,
            preceding_fragment_id,
            first_user_message,
            messages,
        }
    }

    /// Generate the stub message content for this fragment.
    pub fn generate_stub(&self) -> String {
        let mut stub = String::new();
        stub.push_str("---\n");
        // Include the first user message for context
        if let Some(ref task) = self.first_user_message {
            stub.push_str(&format!("{}\n\n", task));
        }

        // Tool call summary
        let tool_part = if !self.tool_call_summary.is_empty() {
            let total_calls: usize = self.tool_call_summary.values().sum();
            let tool_details: Vec<String> = self
                .tool_call_summary
                .iter()
                .map(|(tool, count)| format!("{} x{}", tool, count))
                .collect();
            format!("{} tool calls ({})", total_calls, tool_details.join(", "))
        } else {
            "no tool calls".to_string()
        };

        stub.push_str(&format!(
            "⚡ DEHYDRATED CONTEXT: {}, {} total msgs. To restore, call: rehydrate(fragment_id: \"{}\")\n",
            tool_part, self.message_count, self.fragment_id
        ));

        stub.push_str("---");

        stub
    }

    /// Get the file path for this fragment.
    pub fn file_path(&self, session_id: &str) -> PathBuf {
        get_fragments_dir(session_id).join(format!("fragment_{}.json", self.fragment_id))
    }

    /// Save this fragment to disk.
    pub fn save(&self, session_id: &str) -> Result<PathBuf> {
        let fragments_dir = get_fragments_dir(session_id);
        std::fs::create_dir_all(&fragments_dir)
            .context("Failed to create fragments directory")?;

        let file_path = self.file_path(session_id);
        let json = serde_json::to_string_pretty(self)
            .context("Failed to serialize fragment")?;
        std::fs::write(&file_path, json)
            .context("Failed to write fragment file")?;

        debug!("Saved fragment {} to {:?}", self.fragment_id, file_path);
        Ok(file_path)
    }

    /// Load a fragment from disk.
    pub fn load(session_id: &str, fragment_id: &str) -> Result<Self> {
        let file_path = get_fragments_dir(session_id)
            .join(format!("fragment_{}.json", fragment_id));

        if !file_path.exists() {
            anyhow::bail!("Fragment not found: {}", fragment_id);
        }

        let json = std::fs::read_to_string(&file_path)
            .context("Failed to read fragment file")?;
        let fragment: Fragment = serde_json::from_str(&json)
            .context("Failed to deserialize fragment")?;

        debug!("Loaded fragment {} from {:?}", fragment_id, file_path);
        Ok(fragment)
    }
}

/// Generate a unique fragment ID.
fn generate_fragment_id() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};

    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();

    // Use first 12 hex chars of timestamp hash for brevity
    format!("{:x}", timestamp).chars().take(12).collect()
}

/// Extract a summary of tool calls from messages.
///
/// Tool calls reach us in two forms and BOTH must be counted:
///
/// 1. **Structured** — `Message.tool_calls`, produced by every native
///    tool-calling provider (Anthropic, OpenAI, Gemini). This is the default.
/// 2. **Inline JSON** — embedded in `Message.content`, produced by the embedded
///    provider and other non-native backends.
///
/// This function used to inspect only form (2). Since the default configuration
/// produces form (1), every stub reported "no tool calls" — which is precisely
/// the metadata the LLM uses to judge whether rehydrating a fragment is
/// worthwhile. The stub was actively misinforming it.
fn extract_tool_call_summary(messages: &[Message]) -> HashMap<String, usize> {
    let mut summary = HashMap::new();

    for msg in messages {
        if !matches!(msg.role, g3_providers::MessageRole::Assistant) {
            continue;
        }

        // Form 1: structured tool calls. A single assistant message may carry
        // several, so count each rather than just the first.
        let mut counted_structured = false;
        for tc in &msg.tool_calls {
            *summary.entry(tc.name.clone()).or_insert(0) += 1;
            counted_structured = true;
        }

        // Form 2: inline JSON. Only consult this when there were no structured
        // calls, otherwise a provider that emits both representations for the
        // same call would be double-counted.
        if !counted_structured {
            if let Some(tool_name) = extract_tool_name_from_content(&msg.content) {
                *summary.entry(tool_name).or_insert(0) += 1;
            }
        }
    }

    summary
}

/// Extract tool name from assistant message content.
fn extract_tool_name_from_content(content: &str) -> Option<String> {
    // Look for JSON tool call pattern
    if let Some(start) = content.find(r#""tool""#).or_else(|| content.find(r#""tool" "#)) {
        let after_tool = &content[start..];
        // Find the tool name value
        if let Some(colon_pos) = after_tool.find(':') {
            let after_colon = &after_tool[colon_pos + 1..];
            let trimmed = after_colon.trim_start();
            if trimmed.starts_with('"') {
                let name_start = 1;
                if let Some(name_end) = trimmed[name_start..].find('"') {
                    return Some(trimmed[name_start..name_start + name_end].to_string());
                }
            }
        }
    }

    // Also try parsing as JSON
    if let Ok(tool_call) = serde_json::from_str::<ToolCall>(content) {
        return Some(tool_call.tool);
    }

    // Try to find embedded JSON
    if let Some(start) = content.find('{') {
        if let Some(end) = find_json_end(&content[start..]) {
            let json_str = &content[start..start + end + 1];
            if let Ok(tool_call) = serde_json::from_str::<ToolCall>(json_str) {
                return Some(tool_call.tool);
            }
        }
    }

    None
}

/// Find the end of a JSON object (matching braces).
fn find_json_end(json_str: &str) -> Option<usize> {
    crate::utils::find_json_object_end(json_str)
}

/// Estimate token count for messages.
///
/// Must agree with `ContextWindow::estimate_message_tokens`, because
/// `execute_rehydrate()` compares the value stored here against the remaining
/// context to decide whether a fragment will fit. This used to be a flat
/// `len/4 * 1.1` while `ContextWindow` uses `len/3 * 1.1` for anything
/// containing `{`, ` ``` ` or `fn ` — i.e. for exactly the JSON and code that
/// tool results are made of. The result was a >20% undercount on JSON-heavy
/// fragments, so the capacity check would green-light a rehydration that
/// overflowed the window.
fn estimate_fragment_tokens(messages: &[Message]) -> u32 {
    messages
        .iter()
        .map(|m| {
            let text = &m.content;
            // Same discrimination as ContextWindow::estimate_tokens.
            let base = if text.contains('{') || text.contains("```") || text.contains("fn ") {
                (text.len() as f32 / 3.0).ceil()
            } else {
                (text.len() as f32 / 4.0).ceil()
            };
            let mut tokens = (base * 1.1).ceil() as u32;

            // Structured tool calls are serialized onto the wire too, and are
            // always JSON. Mirror ContextWindow's per-call overhead.
            for tc in &m.tool_calls {
                let input_len = tc.input.to_string().len();
                tokens += ((input_len as f32 / 3.0).ceil() * 1.1).ceil() as u32 + 20;
            }
            tokens
        })
        .sum()
}

/// Extract topic hints from messages using heuristics.
fn extract_topics(messages: &[Message]) -> Vec<String> {
    let mut topics = Vec::new();
    let mut seen = std::collections::HashSet::new();

    for msg in messages {
        match msg.role {
            g3_providers::MessageRole::User => {
                // Extract first meaningful part of user messages
                if !msg.content.starts_with("Tool result") {
                    let topic = extract_topic_from_text(&msg.content);
                    if !topic.is_empty() && seen.insert(topic.clone()) {
                        topics.push(topic);
                    }
                }
            }
            g3_providers::MessageRole::Assistant => {
                // Look for file paths in tool calls
                if let Some(path) = extract_file_path(&msg.content) {
                    if seen.insert(path.clone()) {
                        topics.push(format!("edited {}", path));
                    }
                }
            }
            _ => {}
        }

        // Limit topics to keep stub concise
        if topics.len() >= 5 {
            break;
        }
    }

    topics
}

/// Extract a brief topic from text.
fn extract_topic_from_text(text: &str) -> String {
    // Take first line, truncate to ~50 chars
    let first_line = text.lines().next().unwrap_or("");
    let cleaned = first_line.trim();

    if cleaned.chars().count() <= 50 {
        cleaned.to_string()
    } else {
        // Find a good break point (UTF-8 safe)
        let truncated: String = cleaned.chars().take(50).collect();
        if let Some(last_space) = truncated.rfind(' ') {
            // last_space is a byte index into truncated, which is safe since truncated is a new String
            format!("{}...", &truncated[..last_space])
        } else {
            format!("{}...", truncated)
        }
    }
}

/// Extract file path from tool call content.
fn extract_file_path(content: &str) -> Option<String> {
    // Look for file_path in JSON
    if let Some(start) = content.find(r#""file_path""#) {
        let after = &content[start..];
        if let Some(colon) = after.find(':') {
            let after_colon = &after[colon + 1..];
            let trimmed = after_colon.trim_start();
            if trimmed.starts_with('"') {
                if let Some(end) = trimmed[1..].find('"') {
                    let path = &trimmed[1..1 + end];
                    // Return just the filename for brevity
                    return Some(
                        std::path::Path::new(path)
                            .file_name()
                            .and_then(|n| n.to_str())
                            .unwrap_or(path)
                            .to_string(),
                    );
                }
            }
        }
    }
    None
}

/// List all fragments for a session, ordered by creation time.
pub fn list_fragments(session_id: &str) -> Result<Vec<Fragment>> {
    let fragments_dir = get_fragments_dir(session_id);

    if !fragments_dir.exists() {
        return Ok(Vec::new());
    }

    let mut fragments = Vec::new();

    for entry in std::fs::read_dir(&fragments_dir)? {
        let entry = entry?;
        let path = entry.path();

        if path.extension().map_or(false, |e| e == "json") {
            match std::fs::read_to_string(&path) {
                Ok(json) => match serde_json::from_str::<Fragment>(&json) {
                    Ok(fragment) => fragments.push(fragment),
                    Err(e) => warn!("Failed to parse fragment {:?}: {}", path, e),
                },
                Err(e) => warn!("Failed to read fragment {:?}: {}", path, e),
            }
        }
    }

    // Sort by creation time
    fragments.sort_by(|a, b| a.created_at.cmp(&b.created_at));

    Ok(fragments)
}

/// Get the most recent fragment ID for a session (the tail of the linked list).
pub fn get_latest_fragment_id(session_id: &str) -> Result<Option<String>> {
    let fragments = list_fragments(session_id)?;
    Ok(fragments.last().map(|f| f.fragment_id.clone()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use g3_providers::MessageRole;

    fn make_message(role: MessageRole, content: &str) -> Message {
        Message::new(role, content.to_string())
    }

    #[test]
    fn test_fragment_creation() {
        let messages = vec![
            make_message(MessageRole::User, "Hello, can you help me?"),
            make_message(MessageRole::Assistant, "Of course! What do you need?"),
            make_message(MessageRole::User, "Write a function"),
            make_message(
                MessageRole::Assistant,
                r#"{"tool": "write_file", "args": {"file_path": "test.rs", "content": "fn main() {}"}}"#,
            ),
        ];

        let fragment = Fragment::new(messages, None);

        assert_eq!(fragment.message_count, 4);
        assert_eq!(fragment.user_message_count, 2);
        assert_eq!(fragment.assistant_message_count, 2);
        assert!(fragment.fragment_id.len() > 0);
        assert!(fragment.preceding_fragment_id.is_none());
    }

    #[test]
    fn test_fragment_with_preceding() {
        let messages = vec![make_message(MessageRole::User, "Test")];
        let fragment = Fragment::new(messages, Some("abc123".to_string()));

        assert_eq!(fragment.preceding_fragment_id, Some("abc123".to_string()));
    }

    #[test]
    fn test_tool_call_extraction() {
        let messages = vec![
            make_message(
                MessageRole::Assistant,
                r#"{"tool": "shell", "args": {"command": "ls"}}"#,
            ),
            make_message(
                MessageRole::Assistant,
                r#"{"tool": "read_file", "args": {"file_path": "test.rs"}}"#,
            ),
            make_message(
                MessageRole::Assistant,
                r#"{"tool": "shell", "args": {"command": "pwd"}}"#,
            ),
        ];

        let summary = extract_tool_call_summary(&messages);

        assert_eq!(summary.get("shell"), Some(&2));
        assert_eq!(summary.get("read_file"), Some(&1));
    }

    #[test]
    fn test_stub_generation() {
        let messages = vec![
            make_message(MessageRole::User, "implement auth module"),
            make_message(
                MessageRole::Assistant,
                r#"{"tool": "write_file", "args": {"file_path": "auth.rs", "content": "// auth"}}"#,
            ),
        ];

        let fragment = Fragment::new(messages, None);
        let stub = fragment.generate_stub();

        assert!(stub.contains("DEHYDRATED CONTEXT"));
        assert!(stub.contains(&fragment.fragment_id));
        assert!(stub.contains("2 total msgs"));
        assert!(stub.contains("1 tool calls"));
        assert!(stub.contains("rehydrate"));
    }

    #[test]
    fn test_topic_extraction() {
        let messages = vec![
            make_message(MessageRole::User, "Please fix the login bug"),
            make_message(MessageRole::User, "Tool result: success"),
            make_message(MessageRole::User, "Now add tests for it"),
        ];

        let topics = extract_topics(&messages);

        assert!(topics.contains(&"Please fix the login bug".to_string()));
        assert!(topics.contains(&"Now add tests for it".to_string()));
        // Tool results should be skipped
        assert!(!topics.iter().any(|t| t.contains("Tool result")));
    }

    #[test]
    fn test_topic_truncation() {
        let long_text = "This is a very long message that should be truncated because it exceeds the maximum length we want for topic hints";
        let topic = extract_topic_from_text(long_text);

        assert!(topic.len() <= 55); // 50 + "..."
        assert!(topic.ends_with("..."));
    }

    #[test]
    fn test_file_path_extraction() {
        let content = r#"{"tool": "write_file", "args": {"file_path": "src/auth/login.rs", "content": "..."}}"#;
        let path = extract_file_path(content);

        assert_eq!(path, Some("login.rs".to_string()));
    }

    #[test]
    fn test_fragment_save_load_roundtrip() {
        let temp_dir = std::env::temp_dir();
        let test_session_id = format!("test_acd_{}", std::process::id());

        // Create a fragment
        let messages = vec![
            make_message(MessageRole::User, "Test message"),
            make_message(MessageRole::Assistant, "Test response"),
        ];
        let fragment = Fragment::new(messages.clone(), None);
        let original_id = fragment.fragment_id.clone();

        // Temporarily override the g3 dir for testing
        let fragments_dir = temp_dir.join(".g3").join("sessions").join(&test_session_id).join("fragments");
        std::fs::create_dir_all(&fragments_dir).unwrap();

        // Save directly to temp location
        let file_path = fragments_dir.join(format!("fragment_{}.json", original_id));
        let json = serde_json::to_string_pretty(&fragment).unwrap();
        std::fs::write(&file_path, &json).unwrap();

        // Load it back
        let loaded_json = std::fs::read_to_string(&file_path).unwrap();
        let loaded: Fragment = serde_json::from_str(&loaded_json).unwrap();

        assert_eq!(loaded.fragment_id, original_id);
        assert_eq!(loaded.message_count, 2);
        assert_eq!(loaded.messages.len(), 2);
        assert_eq!(loaded.messages[0].content, "Test message");

        // Cleanup
        let _ = std::fs::remove_dir_all(temp_dir.join(".g3").join("sessions").join(&test_session_id));
    }

    #[test]
    fn test_empty_fragment() {
        let fragment = Fragment::new(vec![], None);

        assert_eq!(fragment.message_count, 0);
        assert_eq!(fragment.user_message_count, 0);
        assert_eq!(fragment.assistant_message_count, 0);
        assert!(fragment.tool_call_summary.is_empty());
        assert!(fragment.topics.is_empty());
    }

    #[test]
    fn test_fragment_id_uniqueness() {
        let id1 = generate_fragment_id();
        std::thread::sleep(std::time::Duration::from_millis(1));
        let id2 = generate_fragment_id();

        assert_ne!(id1, id2);
    }

    #[test]
    fn test_linked_list_chain() {
        let frag1 = Fragment::new(
            vec![make_message(MessageRole::User, "First")],
            None,
        );
        let frag2 = Fragment::new(
            vec![make_message(MessageRole::User, "Second")],
            Some(frag1.fragment_id.clone()),
        );
        let frag3 = Fragment::new(
            vec![make_message(MessageRole::User, "Third")],
            Some(frag2.fragment_id.clone()),
        );

        // Verify chain
        assert!(frag1.preceding_fragment_id.is_none());
        assert_eq!(frag2.preceding_fragment_id, Some(frag1.fragment_id.clone()));
        assert_eq!(frag3.preceding_fragment_id, Some(frag2.fragment_id.clone()));
    }

    #[test]
    fn test_stub_with_no_tools() {
        let messages = vec![
            make_message(MessageRole::User, "Just chatting"),
            make_message(MessageRole::Assistant, "Sure, let's chat!"),
        ];

        let fragment = Fragment::new(messages, None);
        let stub = fragment.generate_stub();

        // Should have "no tool calls" in the compact format
        assert!(stub.contains("no tool calls"));
    }

    #[test]
    fn test_stub_with_multiple_tools() {
        let messages = vec![
            make_message(
                MessageRole::Assistant,
                r#"{"tool": "shell", "args": {"command": "ls"}}"#,
            ),
            make_message(
                MessageRole::Assistant,
                r#"{"tool": "read_file", "args": {"file_path": "a.rs"}}"#,
            ),
            make_message(
                MessageRole::Assistant,
                r#"{"tool": "write_file", "args": {"file_path": "b.rs", "content": "x"}}"#,
            ),
        ];

        let fragment = Fragment::new(messages, None);
        let stub = fragment.generate_stub();

        assert!(stub.contains("3 tool calls"));
        assert!(stub.contains("shell"));
        assert!(stub.contains("read_file"));
        assert!(stub.contains("write_file"));
    }

    #[test]
    fn test_token_estimation() {
        let messages = vec![
            make_message(MessageRole::User, "Hello"), // 5 chars
            make_message(MessageRole::Assistant, "World"), // 5 chars
        ];

        let tokens = estimate_fragment_tokens(&messages);

        // 10 chars / 4 * 1.1 ≈ 3 tokens
        assert!(tokens > 0);
        assert!(tokens < 10);
    }

    #[test]
    fn test_extract_tool_name_embedded_json() {
        let content = "Let me check that file for you.

{\"tool\": \"read_file\", \"args\": {\"file_path\": \"test.rs\"}}";
        let tool_name = extract_tool_name_from_content(content);

        assert_eq!(tool_name, Some("read_file".to_string()));
    }

    #[test]
    fn test_extract_tool_name_no_tool() {
        let content = "This is just regular text without any tool calls.";
        let tool_name = extract_tool_name_from_content(content);

        assert!(tool_name.is_none());
    }
}