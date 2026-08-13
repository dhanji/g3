//! Session management utilities for the Agent.
//!
//! This module handles session ID generation, context window persistence,
//! and session logging. It extracts the pure utility functions and I/O
//! operations from the Agent, keeping the Agent as a thin orchestrator.

use crate::context_window::ContextWindow;
use crate::paths::{ensure_session_dir, get_context_summary_file, get_g3_dir, get_session_file};
use g3_providers::MessageRole;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};
use tracing::{debug, error};

/// Format token count in compact form (e.g., 1K, 2M, 100b, 200K)
/// Clamps to 4 chars right-aligned.
pub fn format_token_count(tokens: u32) -> String {
    let mut raw = if tokens >= 1_000_000_000 {
        format!("{}b", tokens / 1_000_000_000)
    } else if tokens >= 1_000_000 {
        format!("{}M", tokens / 1_000_000)
    } else if tokens >= 1_000 {
        format!("{}K", tokens / 1_000)
    } else {
        "0K".to_string()
    };

    if raw.len() > 4 {
        raw.truncate(4);
    }

    format!("{:>4}", raw)
}

/// Pick a single Unicode indicator for token magnitude (maps to color bands).
pub fn token_indicator(tokens: u32) -> &'static str {
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

/// Generate a session ID based on description and optional agent name.
///
/// For agent mode, uses agent name as prefix.
/// For regular mode, uses first 5 words of description.
/// Appends a hash for uniqueness.
pub fn generate_session_id(description: &str, agent_name: Option<&str>) -> String {
    // For agent mode, use agent name as prefix for clarity
    // For regular mode, use first 5 words of description
    let prefix = if let Some(name) = agent_name {
        name.to_string()
    } else {
        description
            .chars()
            .filter(|c| c.is_alphanumeric() || c.is_whitespace() || *c == '-' || *c == '_')
            .collect::<String>()
            .split_whitespace()
            .take(5)
            .collect::<Vec<_>>()
            .join("_")
            .to_lowercase()
    };

    // Create a hash for uniqueness (description + agent name + timestamp)
    let mut hasher = DefaultHasher::new();
    description.hash(&mut hasher);
    if let Some(name) = agent_name {
        name.hash(&mut hasher);
    }
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    timestamp.hash(&mut hasher);
    let hash = hasher.finish();

    // Format: prefix_hash
    format!("{}_{:x}", prefix, hash)
}

/// Save the context window to a session file.
///
/// If session_id is provided, saves to `.g3/sessions/<session_id>/session.json`.
/// Otherwise, saves to `.g3/sessions/anonymous_<timestamp>/session.json`.
pub fn save_context_window(
    session_id: Option<&str>,
    context_window: &ContextWindow,
    status: &str,
) {
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    // Determine filename based on session ID
    let filename = if let Some(id) = session_id {
        // Ensure session directory exists
        if let Err(e) = ensure_session_dir(id) {
            error!("Failed to create session directory: {}", e);
            return;
        }
        get_session_file(id)
    } else {
        // Create anonymous session for sessions without ID
        let anonymous_id = format!("anonymous_{}", timestamp);
        if let Err(e) = ensure_session_dir(&anonymous_id) {
            error!("Failed to create anonymous session directory: {}", e);
            return;
        }
        get_session_file(&anonymous_id)
    };

    let context_data = serde_json::json!({
        "session_id": session_id,
        "timestamp": timestamp,
        "status": status,
        "context_window": {
            "used_tokens": context_window.used_tokens,
            "total_tokens": context_window.total_tokens,
            "percentage_used": context_window.percentage_used(),
            "conversation_history": context_window.conversation_history
        }
    });

    match serde_json::to_string_pretty(&context_data) {
        Ok(json_content) => {
            if let Err(e) = write_atomic(&filename, &json_content) {
                error!("Failed to save context window to {:?}: {}", &filename, e);
            }
        }
        Err(e) => {
            error!("Failed to serialize context window: {}", e);
        }
    }
}

/// Write a file atomically: serialize into a sibling temp file, then rename.
///
/// `fs::write` truncates the target and streams into it, so a concurrent reader
/// can observe a half-written file and fail to parse it. That was survivable
/// when each turn wrote a brand-new session dir, but `--resume` now continues
/// IN PLACE, so session.json is rewritten under readers (butler.app polls it
/// while a turn streams). rename(2) within a directory is atomic on macOS and
/// Linux, so a reader sees either the complete old file or the complete new one.
///
/// A failed write also leaves the previous file intact rather than truncated,
/// which matters because a partial session.json reads as a lost conversation.
fn write_atomic(path: &std::path::Path, contents: &str) -> std::io::Result<()> {
    // Temp file must be a sibling: rename is only atomic within a filesystem.
    let tmp = path.with_extension("json.tmp");
    {
        let mut f = std::fs::File::create(&tmp)?;
        use std::io::Write;
        f.write_all(contents.as_bytes())?;
        // fsync before rename so a crash can't leave a renamed-but-empty file.
        f.sync_all()?;
    }
    match std::fs::rename(&tmp, path) {
        Ok(()) => Ok(()),
        Err(e) => {
            // Don't leave litter behind if the rename failed.
            let _ = std::fs::remove_file(&tmp);
            Err(e)
        }
    }
}

/// Write a human-readable context window summary to file.
///
/// Format: message_id, role, token_count, indicator, first_120_chars
pub fn write_context_window_summary(session_id: &str, context_window: &ContextWindow) {
    // Ensure session directory exists
    if let Err(e) = ensure_session_dir(session_id) {
        error!("Failed to create session directory: {}", e);
        return;
    }

    let filename = get_context_summary_file(session_id);
    let symlink_path = get_g3_dir().join("sessions").join("current_context_window");

    // Build the summary content
    let mut summary_lines = Vec::new();

    for message in &context_window.conversation_history {
        // Estimate tokens for this message
        let message_tokens = ContextWindow::estimate_tokens(&message.content);

        // Format token count and get indicator
        let token_str = format_token_count(message_tokens);
        let indicator = token_indicator(message_tokens);

        // Get role as string
        let role = match message.role {
            MessageRole::System => "sys",
            MessageRole::User => "usr",
            MessageRole::Assistant => "ass",
        };

        // Get first 120 characters of content, replace newlines
        let content_preview: String = message
            .content
            .chars()
            .take(120)
            .collect::<String>()
            .replace('\n', " ")
            .replace('\r', " ");

        let line = format!(
            "{}, {}, {} {}, {}\n",
            message.id, role, token_str, indicator, content_preview
        );
        summary_lines.push(line);
    }

    // Add total estimate
    let total_token_str = format_token_count(context_window.used_tokens);
    let capacity_str = format_token_count(context_window.total_tokens);
    let percentage = context_window.percentage_used();

    summary_lines.push(format!(
        "\n--- TOTAL: {} / {} ({:.1}%) ---\n",
        total_token_str, capacity_str, percentage
    ));

    // Write to file
    let summary_content = summary_lines.join("");
    if let Err(e) = std::fs::write(&filename, &summary_content) {
        error!(
            "Failed to write context window summary to {:?}: {}",
            &filename, e
        );
        return;
    }

    // Update symlink
    let _ = std::fs::remove_file(&symlink_path);

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
        context_window.conversation_history.len()
    );
}

/// Log an error to the session JSON file.
///
/// Appends an error entry to the conversation history in the session log.
pub fn log_error_to_session(
    session_id: &str,
    error: &anyhow::Error,
    role: &str,
    forensic_context: Option<String>,
) {
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    // Use the new .g3/sessions/<session_id>/session.json path
    let filename = get_session_file(session_id);

    // Read existing session log
    let mut session_data: serde_json::Value = if filename.exists() {
        match std::fs::read_to_string(&filename) {
            Ok(content) => serde_json::from_str(&content).unwrap_or_else(|_| serde_json::json!({})),
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

/// Restore conversation history from a session log file.
///
/// Returns the messages to add to the context window, or None if restoration failed.
pub fn restore_from_session_log(session_log_path: &PathBuf) -> Option<Vec<(MessageRole, String)>> {
    if !session_log_path.exists() {
        return None;
    }

    let json = std::fs::read_to_string(session_log_path).ok()?;
    let session_data: serde_json::Value = serde_json::from_str(&json).ok()?;

    let context_window = session_data.get("context_window")?;
    let history = context_window.get("conversation_history")?;
    let messages = history.as_array()?;

    let mut result = Vec::new();
    for msg in messages {
        let role_str = msg.get("role").and_then(|r| r.as_str()).unwrap_or("user");
        let content = msg.get("content").and_then(|c| c.as_str()).unwrap_or("");

        // Skip system messages (they're preserved separately)
        if role_str == "system" {
            continue;
        }

        let role = match role_str {
            "assistant" => MessageRole::Assistant,
            _ => MessageRole::User,
        };

        result.push((role, content.to_string()));
    }

    Some(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_format_token_count_small() {
        assert_eq!(format_token_count(0), "  0K");
        assert_eq!(format_token_count(500), "  0K");
        assert_eq!(format_token_count(999), "  0K");
    }

    #[test]
    fn test_format_token_count_thousands() {
        assert_eq!(format_token_count(1000), "  1K");
        assert_eq!(format_token_count(5000), "  5K");
        assert_eq!(format_token_count(10000), " 10K");
        assert_eq!(format_token_count(999999), "999K");
    }

    #[test]
    fn test_format_token_count_millions() {
        assert_eq!(format_token_count(1_000_000), "  1M");
        assert_eq!(format_token_count(5_000_000), "  5M");
    }

    #[test]
    fn test_token_indicator() {
        assert_eq!(token_indicator(500), "🟢");
        assert_eq!(token_indicator(1000), "🟢");
        assert_eq!(token_indicator(1001), "🟡");
        assert_eq!(token_indicator(5000), "🟡");
        assert_eq!(token_indicator(5001), "🟠");
        assert_eq!(token_indicator(10000), "🟠");
        assert_eq!(token_indicator(10001), "🔴");
        assert_eq!(token_indicator(20000), "🔴");
        assert_eq!(token_indicator(20001), "🟣");
    }

    #[test]
    fn test_generate_session_id_regular_mode() {
        let id = generate_session_id("implement a function to calculate fibonacci", None);
        assert!(id.starts_with("implement_a_function_to_calculate_"));
        assert!(id.contains('_')); // Has hash suffix
    }

    #[test]
    fn test_generate_session_id_agent_mode() {
        let id = generate_session_id("some task", Some("fowler"));
        assert!(id.starts_with("fowler_"));
    }

    #[test]
    fn test_generate_session_id_uniqueness() {
        // Same description should produce different IDs due to timestamp
        let id1 = generate_session_id("test", None);
        std::thread::sleep(std::time::Duration::from_millis(1));
        let id2 = generate_session_id("test", None);
        assert_ne!(id1, id2);
    }
}
