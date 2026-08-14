//! Workspace memory tool: remember.
//!
//! These tools provide a persistent "working memory" for the project,
//! storing feature locations, patterns, and entry points discovered
//! during g3 sessions.

use anyhow::Result;
use chrono::Utc;
use std::path::PathBuf;

use crate::ui_writer::UiWriter;
use crate::ToolCall;

use super::executor::ToolContext;

/// The default location of the memory file, relative to the workspace root.
pub const DEFAULT_MEMORY_RELATIVE_PATH: &str = "analysis/memory.md";

/// Resolve the path to the memory file.
///
/// This is the SINGLE source of truth for where workspace memory lives. Both the
/// read path (g3-cli loading memory into the system prompt at startup) and the
/// write path (the `remember` tool) MUST call this. If they ever disagree, memory
/// silently forks: reads come from one file, writes land in another, and neither
/// side reports an error.
///
/// `override_path`, when set (from `--memory <path>`), wins outright. A leading
/// `~` is expanded; a relative path is resolved against `workspace_dir` so that
/// read and write agree regardless of the process's current directory.
///
/// With no override the result is `<workspace_dir>/analysis/memory.md`, which is
/// the historical behaviour and must remain byte-identical for other projects.
pub fn resolve_memory_path(workspace_dir: Option<&str>, override_path: Option<&str>) -> PathBuf {
    let base = workspace_dir
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));

    match override_path {
        Some(raw) if !raw.trim().is_empty() => {
            let expanded = PathBuf::from(shellexpand::tilde(raw.trim()).to_string());
            if expanded.is_absolute() {
                expanded
            } else {
                base.join(expanded)
            }
        }
        _ => base.join("analysis").join("memory.md"),
    }
}

/// Format the file size in a human-readable way.
fn format_size(chars: usize) -> String {
    if chars < 1000 {
        format!("{} chars", chars)
    } else {
        format!("{:.1}k chars", chars as f64 / 1000.0)
    }
}

/// Execute the remember tool.
/// Merges new notes with existing memory and saves to file.
pub async fn execute_remember<W: UiWriter>(
    tool_call: &ToolCall,
    ctx: &mut ToolContext<'_, W>,
) -> Result<String> {
    let notes = tool_call
        .args
        .get("notes")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("Missing required 'notes' parameter"))?;

    let memory_path = resolve_memory_path(ctx.working_dir, ctx.memory_path);

    // Ensure the containing directory exists (analysis/ by default, but the
    // --memory override may point anywhere).
    if let Some(parent) = memory_path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    // Read existing memory or create new
    let existing = if memory_path.exists() {
        std::fs::read_to_string(&memory_path)?
    } else {
        String::new()
    };

    // Merge notes with existing memory
    let updated = merge_memory(&existing, notes);

    // Add/update header with timestamp and size
    let timestamp = Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string();
    let size = format_size(updated.len());
    let final_content = update_header(&updated, &timestamp, &size);

    // Write back
    std::fs::write(&memory_path, &final_content)?;

    Ok(format!("Memory updated. Size: {}", format_size(final_content.len())))
}

/// Merge new notes into existing memory.
/// Appends new notes to the appropriate sections or creates new sections.
fn merge_memory(existing: &str, new_notes: &str) -> String {
    if existing.is_empty() {
        // Start fresh with just the notes
        return new_notes.trim().to_string();
    }

    // Simple merge strategy: append new notes to the end
    // The LLM is responsible for providing well-formatted notes
    // and avoiding duplicates (as instructed in the prompt)
    let existing_trimmed = existing.trim();
    let new_trimmed = new_notes.trim();

    // Remove the header line if present (we'll re-add it)
    let existing_body = remove_header(existing_trimmed);

    format!("{}\n\n{}", existing_body.trim(), new_trimmed)
}

/// Remove the header line (# Workspace Memory and > Updated: ...) from content.
fn remove_header(content: &str) -> String {
    let mut lines: Vec<&str> = content.lines().collect();

    // Remove "# Workspace Memory" if first line
    if !lines.is_empty() && lines[0].starts_with("# Workspace Memory") {
        lines.remove(0);
    }

    // Remove "> Updated: ..." line if present at start
    if !lines.is_empty() && lines[0].starts_with("> Updated:") {
        lines.remove(0);
    }

    // Remove leading empty lines
    while !lines.is_empty() && lines[0].trim().is_empty() {
        lines.remove(0);
    }

    lines.join("\n")
}

/// Update or add the header with timestamp and size.
fn update_header(content: &str, timestamp: &str, size: &str) -> String {
    let body = remove_header(content);
    format!(
        "# Workspace Memory\n> Updated: {} | Size: {}\n\n{}",
        timestamp,
        size,
        body.trim()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_format_size() {
        assert_eq!(format_size(500), "500 chars");
        assert_eq!(format_size(999), "999 chars");
        assert_eq!(format_size(1000), "1.0k chars");
        assert_eq!(format_size(2500), "2.5k chars");
        assert_eq!(format_size(10000), "10.0k chars");
    }

    #[test]
    fn test_merge_memory_empty() {
        let result = merge_memory("", "### New Feature\n- `file.rs` [0..100] - `func()`");
        assert_eq!(result, "### New Feature\n- `file.rs` [0..100] - `func()`");
    }

    #[test]
    fn test_merge_memory_append() {
        let existing = "# Workspace Memory\n> Updated: 2025-01-10 | Size: 1k\n\n### Feature A\n- `a.rs` [0..50]";
        let new_notes = "### Feature B\n- `b.rs` [0..100]";
        let result = merge_memory(existing, new_notes);

        assert!(result.contains("### Feature A"));
        assert!(result.contains("### Feature B"));
        assert!(!result.contains("# Workspace Memory")); // Header removed for re-adding
    }

    #[test]
    fn test_remove_header() {
        let content = "# Workspace Memory\n> Updated: 2025-01-10 | Size: 1k\n\n### Feature\n- details";
        let result = remove_header(content);
        assert!(!result.contains("# Workspace Memory"));
        assert!(!result.contains("> Updated:"));
        assert!(result.contains("### Feature"));
    }

    #[test]
    fn test_update_header() {
        let content = "### Feature\n- details";
        let result = update_header(content, "2025-01-10T12:00:00Z", "500 chars");

        assert!(result.starts_with("# Workspace Memory"));
        assert!(result.contains("> Updated: 2025-01-10T12:00:00Z | Size: 500 chars"));
        assert!(result.contains("### Feature"));
    }
}
