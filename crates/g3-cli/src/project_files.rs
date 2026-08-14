//! Project file reading utilities.
//!
//! Reads AGENTS.md and workspace memory files from the workspace.

use std::path::Path;
use std::path::PathBuf;
use tracing::error;

use crate::template::process_template;
use g3_core::{discover_skills, generate_skills_prompt, Skill};
use g3_config::SkillsConfig;

/// Read AGENTS.md configuration from the workspace directory.
/// Returns formatted content with emoji prefix, or None if not found.
pub fn read_agents_config(workspace_dir: &Path) -> Option<String> {
    // Try AGENTS.md first, then agents.md
    let paths = [
        (workspace_dir.join("AGENTS.md"), "AGENTS.md"),
        (workspace_dir.join("agents.md"), "agents.md"),
    ];

    for (path, name) in &paths {
        if path.exists() {
            match std::fs::read_to_string(path) {
                Ok(content) => {
                    return Some(format!("🤖 Agent Configuration (from {}):{}\n{}", name, "\n", content));
                }
                Err(e) => {
                    error!("Failed to read {}: {}", name, e);
                }
            }
        }
    }
    None
}

/// Read workspace memory, honouring the `--memory <path>` override.
///
/// The location is resolved by `g3_core::tools::memory::resolve_memory_path` —
/// the same function the `remember` tool uses to WRITE. Do not join a memory
/// path by hand here: a read/write split forks memory silently (reads from one
/// file, writes to another, no error on either side).
///
/// Returns formatted content with size info, or None if the file is absent or
/// unreadable (e.g. evicted from iCloud), which degrades to "no memory".
pub fn read_workspace_memory(workspace_dir: &Path, memory_override: Option<&str>) -> Option<String> {
    let memory_path = g3_core::tools::memory::resolve_memory_path(
        workspace_dir.to_str(),
        memory_override,
    );

    if !memory_path.exists() {
        return None;
    }

    match std::fs::read_to_string(&memory_path) {
        Ok(content) => {
            let size = format_size(content.len());
            // Report the path actually used, so an unexpected override is visible.
            let shown = memory_path.display();
            Some(format!(
                "=== Workspace Memory (read from {}, {}) ===\n{}\n=== End Workspace Memory ===",
                shown,
                size,
                content
            ))
        }
        Err(_) => None,
    }
}

/// Read include prompt content from a specified file path.
/// Returns formatted content with emoji prefix, or None if path is None or file doesn't exist.
pub fn read_include_prompt(path: Option<&std::path::Path>) -> Option<String> {
    let path = path?;
    
    if !path.exists() {
        tracing::error!("Include prompt file not found: {}", path.display());
        return None;
    }

    match std::fs::read_to_string(path) {
        Ok(content) => {
            let processed = process_template(&content);
            Some(format!("📎 Included Prompt (from {}):\n{}", path.display(), processed))
        }
        Err(e) => {
            tracing::error!("Failed to read include prompt file {}: {}", path.display(), e);
            None
        }
    }
}

/// Combine AGENTS.md and memory content into a single string for project context.
///
/// Returns None if all inputs are None, otherwise joins non-None parts with double newlines.
/// Prepends the current working directory to help the LLM avoid path hallucinations.
/// 
/// Order: Working Directory → AGENTS.md → Language prompts → Include prompt → Memory
pub fn combine_project_content(
    agents_content: Option<String>,
    memory_content: Option<String>,
    language_content: Option<String>,
    include_prompt: Option<String>,
    skills_content: Option<String>,
    workspace_dir: &Path,
) -> Option<String> {
    // Always include working directory to prevent LLM from hallucinating paths
    let cwd_info = format!("📂 Working Directory: {}", workspace_dir.display());
    
    // Order: cwd → agents → language → include_prompt → skills → memory
    // Include prompt comes BEFORE memory so memory is always last (most recent context)
    let parts: Vec<String> = [
        Some(cwd_info), agents_content, language_content, include_prompt, skills_content, memory_content
    ]
        .into_iter()
        .flatten()
        .collect();

    if parts.is_empty() {
        None
    } else {
        Some(parts.join("\n\n"))
    }
}

/// Format a byte size for display.
fn format_size(len: usize) -> String {
    if len < 1000 {
        format!("{} chars", len)
    } else {
        format!("{:.1}k chars", len as f64 / 1000.0)
    }
}

/// Extract the first H1 heading from project context content for display.
/// Looks for H1 headings in AGENTS.md or memory content.
pub fn extract_project_heading(project_context: &str) -> Option<String> {
    // Look for H1 heading in the content
    // Skip prefix lines (emoji markers)
    for line in project_context.lines() {
        let trimmed = line.trim();
        
        // Skip emoji prefix lines
        if trimmed.starts_with("📂") || trimmed.starts_with("🤖") || trimmed.starts_with("🔧") || trimmed.starts_with("📎") || trimmed.starts_with("===") {
            continue;
        }
        
        if let Some(stripped) = trimmed.strip_prefix("# ") {
            let title = stripped.trim();
            if !title.is_empty() {
                return Some(title.to_string());
            }
        }
    }

    // Fallback: first non-empty, non-metadata line
    find_fallback_title(project_context)
}

/// Find a fallback title from the first few lines of content.
fn find_fallback_title(content: &str) -> Option<String> {
    for line in content.lines().take(5) {
        let trimmed = line.trim();
        if !trimmed.is_empty()
            && !trimmed.starts_with("📚")
            && !trimmed.starts_with("📂")
            && !trimmed.starts_with("🤖")
            && !trimmed.starts_with("🔧")
            && !trimmed.starts_with('#')
            && !trimmed.starts_with("==")
            && !trimmed.starts_with("--")
        {
            return Some(truncate_for_display(trimmed, 100));
        }
    }
    None
}

/// Truncate a string for display, adding ellipsis if needed.
fn truncate_for_display(s: &str, max_len: usize) -> String {
    if s.chars().count() <= max_len {
        s.to_string()
    } else {
        // Truncate at character boundary, not byte boundary
        let truncated: String = s.chars().take(max_len.saturating_sub(3)).collect();
        format!("{}...", truncated)
    }
}

/// Discover skills from configured paths and generate the skills prompt.
///
/// Returns the skills prompt section if any skills are found, None otherwise.
/// Skills are discovered from:
/// 1. Global: ~/.g3/skills/
/// 2. Extra paths from config
/// 3. Workspace: .g3/skills/ (highest priority)
pub fn discover_and_format_skills(
    workspace_dir: &Path,
    skills_config: &SkillsConfig,
) -> (Vec<Skill>, Option<String>) {
    if !skills_config.enabled {
        return (Vec::new(), None);
    }

    // Convert extra_paths from config to PathBuf
    let extra_paths: Vec<PathBuf> = skills_config
        .extra_paths
        .iter()
        .map(|p| PathBuf::from(p))
        .collect();

    let skills = discover_skills(Some(workspace_dir), &extra_paths);
    
    if skills.is_empty() {
        return (Vec::new(), None);
    }

    let prompt = generate_skills_prompt(&skills);
    (skills, Some(prompt))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_project_heading() {
        let content = "# My Project\n\nSome description";
        assert_eq!(extract_project_heading(content), Some("My Project".to_string()));
    }

    #[test]
    fn test_extract_project_heading_with_agents_prefix() {
        let content = "🤖 Agent Configuration (from AGENTS.md):\n# Cool App\n\nDescription";
        assert_eq!(extract_project_heading(content), Some("Cool App".to_string()));
    }

    #[test]
    fn test_format_size() {
        assert_eq!(format_size(500), "500 chars");
        assert_eq!(format_size(1500), "1.5k chars");
    }

    #[test]
    fn test_truncate_for_display() {
        assert_eq!(truncate_for_display("short", 100), "short");
        let long = "a".repeat(150);
        let truncated = truncate_for_display(&long, 100);
        assert!(truncated.ends_with("..."));
        assert_eq!(truncated.len(), 100);
    }

    #[test]
    fn test_truncate_for_display_utf8() {
        // Multi-byte characters should not cause panics
        let emoji_text = "Hello 👋 World 🌍 Test ✨ More text here and more";
        let truncated = truncate_for_display(emoji_text, 15);
        assert!(truncated.ends_with("..."));
        assert!(truncated.chars().count() <= 15);
    }

    #[test]
    fn test_combine_project_content_all_some() {
        let workspace = std::path::PathBuf::from("/test/workspace");
        let result = combine_project_content(
            Some("agents".to_string()),
            Some("memory".to_string()),
            Some("language".to_string()),
            None, // include_prompt
            None, // skills_content
            &workspace,
        );
        assert!(result.is_some());
        let content = result.unwrap();
        assert!(content.contains("📂 Working Directory: /test/workspace"));
        assert!(content.contains("agents"));
        assert!(content.contains("memory"));
        assert!(content.contains("language"));
    }

    #[test]
    fn test_combine_project_content_partial() {
        let workspace = std::path::PathBuf::from("/test/workspace");
        let result = combine_project_content(None, Some("memory".to_string()), None, None, None, &workspace);
        assert!(result.is_some());
        let content = result.unwrap();
        assert!(content.contains("📂 Working Directory: /test/workspace"));
        assert!(content.contains("memory"));
    }

    #[test]
    fn test_combine_project_content_all_none() {
        let workspace = std::path::PathBuf::from("/test/workspace");
        let result = combine_project_content(None, None, None, None, None, &workspace);
        // Now always returns Some because we always include the working directory
        assert!(result.is_some());
        assert!(result.unwrap().contains("📂 Working Directory: /test/workspace"));
    }

    #[test]
    fn test_combine_project_content_with_include_prompt() {
        let workspace = std::path::PathBuf::from("/test/workspace");
        let result = combine_project_content(
            Some("agents".to_string()),
            Some("memory".to_string()),
            Some("language".to_string()),
            Some("include_prompt".to_string()),
            None, // skills_content
            &workspace,
        );
        assert!(result.is_some());
        let content = result.unwrap();
        assert!(content.contains("include_prompt"));
    }

    #[test]
    fn test_combine_project_content_order() {
        // Verify correct ordering: agents < language < include_prompt < memory
        let workspace = std::path::PathBuf::from("/test/workspace");
        let result = combine_project_content(
            Some("AGENTS_CONTENT".to_string()),
            Some("MEMORY_CONTENT".to_string()),
            Some("LANGUAGE_CONTENT".to_string()),
            Some("INCLUDE_PROMPT_CONTENT".to_string()),
            None, // skills_content
            &workspace,
        );
        let content = result.unwrap();
        
        // Find positions of each section
        let agents_pos = content.find("AGENTS_CONTENT").expect("agents not found");
        let language_pos = content.find("LANGUAGE_CONTENT").expect("language not found");
        let include_pos = content.find("INCLUDE_PROMPT_CONTENT").expect("include_prompt not found");
        let memory_pos = content.find("MEMORY_CONTENT").expect("memory not found");
        
        // Verify order: agents < language < include_prompt < memory
        assert!(agents_pos < language_pos, "agents should come before language");
        assert!(language_pos < include_pos, "language should come before include_prompt");
        assert!(include_pos < memory_pos, "include_prompt should come before memory");
    }

    #[test]
    fn test_combine_project_content_order_memory_last() {
        // Verify memory is always last even when include_prompt is None
        let workspace = std::path::PathBuf::from("/test/workspace");
        let result = combine_project_content(
            Some("AGENTS".to_string()),
            Some("MEMORY".to_string()),
            Some("LANGUAGE".to_string()),
            None, // no include_prompt
            None, // skills_content
            &workspace,
        );
        let content = result.unwrap();
        
        // Memory should still be last
        let language_pos = content.find("LANGUAGE").expect("language not found");
        let memory_pos = content.find("MEMORY").expect("memory not found");
        assert!(language_pos < memory_pos, "memory should come after language");
    }

    #[test]
    fn test_read_include_prompt_none_path() {
        // None path should return None
        let result = read_include_prompt(None);
        assert!(result.is_none());
    }

    #[test]
    fn test_read_include_prompt_nonexistent_file() {
        // Non-existent file should return None
        let path = std::path::Path::new("/nonexistent/path/to/file.md");
        let result = read_include_prompt(Some(path));
        assert!(result.is_none());
    }

    #[test]
    fn test_read_include_prompt_valid_file() {
        // Create a temp file and read it
        let temp_dir = std::env::temp_dir();
        let temp_file = temp_dir.join("test_include_prompt.md");
        std::fs::write(&temp_file, "Test prompt content").unwrap();
        
        let result = read_include_prompt(Some(&temp_file));
        assert!(result.is_some());
        let content = result.unwrap();
        assert!(content.contains("📎 Included Prompt"));
        assert!(content.contains("Test prompt content"));
        
        // Cleanup
        let _ = std::fs::remove_file(&temp_file);
    }

    #[test]
    fn test_read_include_prompt_with_template_variables() {
        // Create a temp file with template variables
        let temp_dir = std::env::temp_dir();
        let temp_file = temp_dir.join("test_include_prompt_template.md");
        std::fs::write(&temp_file, "Today is {{today}} and {{unknown}} stays").unwrap();
        
        let result = read_include_prompt(Some(&temp_file));
        assert!(result.is_some());
        let content = result.unwrap();
        
        // {{today}} should be replaced with a date, {{unknown}} should remain
        assert!(!content.contains("{{today}}"));
        assert!(content.contains("{{unknown}}"));
        
        // Cleanup
        let _ = std::fs::remove_file(&temp_file);
    }
}
