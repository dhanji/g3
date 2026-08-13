//! Session continuation support for long-running interactive sessions.
//!
//! This module provides functionality to save and restore session state,
//! allowing users to resume work across multiple g3 invocations.
//!
//! The session continuation uses a symlink-based approach:
//! - `.g3/session` is a symlink pointing to the current session directory
//! - `latest.json` is stored inside each session directory (`.g3/sessions/<session_id>/latest.json`)
//! - Following the symlink gives access to the current session's continuation data

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use tracing::{debug, error, warn};

/// Version of the session continuation format
const CONTINUATION_VERSION: &str = "1.0";

/// Name of the continuation file within each session directory
const CONTINUATION_FILENAME: &str = "latest.json";

/// Session continuation artifact containing all information needed to resume a session
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionContinuation {
    /// Version of the continuation format
    pub version: String,
    /// Whether this session was running in agent mode
    pub is_agent_mode: bool,
    /// Name of the agent (e.g., "fowler", "pike") if in agent mode
    pub agent_name: Option<String>,
    /// Timestamp when the continuation was saved
    pub created_at: String,
    /// Original session ID
    pub session_id: String,
    /// Human-readable description (first user message, truncated)
    #[serde(default)]
    pub description: Option<String>,
    /// Session summary (last assistant response)
    pub summary: Option<String>,
    /// Path to the full session log (g3_session_*.json)
    pub session_log_path: String,
    /// Context window usage percentage when saved
    pub context_percentage: f32,
    /// Snapshot of the TODO list content
    pub todo_snapshot: Option<String>,
    /// Working directory where the session was running
    pub working_directory: String,
}

impl SessionContinuation {
    /// Create a new session continuation artifact
    pub fn new(
        is_agent_mode: bool,
        agent_name: Option<String>,
        session_id: String,
        description: Option<String>,
        summary: Option<String>,
        session_log_path: String,
        context_percentage: f32,
        todo_snapshot: Option<String>,
        working_directory: String,
    ) -> Self {
        Self {
            version: CONTINUATION_VERSION.to_string(),
            is_agent_mode,
            agent_name,
            created_at: chrono::Utc::now().to_rfc3339(),
            session_id,
            description,
            summary,
            session_log_path,
            context_percentage,
            todo_snapshot,
            working_directory,
        }
    }

    /// Check if the context can be fully restored (< 80% used)
    pub fn can_restore_full_context(&self) -> bool {
        self.context_percentage < 80.0
    }

    /// Check if this session has incomplete TODO items
    pub fn has_incomplete_todos(&self) -> bool {
        match &self.todo_snapshot {
            Some(todo) => todo.contains("- [ ]"),
            None => false,
        }
    }
}

/// Get the path to the .g3 directory
fn get_g3_dir() -> PathBuf {
    crate::get_g3_dir()
}

/// Get the path to the .g3/session symlink
pub fn get_session_dir() -> PathBuf {
    get_g3_dir().join("session")
}

/// Get the path to the .g3/sessions directory (where all sessions are stored)
fn get_sessions_dir() -> PathBuf {
    get_g3_dir().join("sessions")
}

/// Get the path to a specific session's directory
fn get_session_path(session_id: &str) -> PathBuf {
    get_sessions_dir().join(session_id)
}

/// Recover the agent name from a session directory name.
///
/// `session::generate_session_id()` builds ids as `<prefix>_<hex hash>`, where the
/// prefix IS the agent name in agent mode, and the first 5 words of the prompt
/// otherwise. So the agent is recoverable from the id alone.
///
/// This matters because `latest.json` is only written on graceful exit. Resuming a
/// session that is still running — or that crashed — falls back to `session.json`,
/// which has never contained an `agent_name` field at all. That made
/// `agent_name` unconditionally `None` on the fallback path, so
/// `g3 --agent butler --resume <id>` rejected its own sessions with
/// "belongs to agent '(none)'".
///
/// Returns `None` unless the name matches `<lowercase>_<hex>`: a prompt-derived id
/// such as `process_new_emails_9f3a` has a multi-word prefix and is correctly read
/// as having no agent.
fn agent_name_from_session_id(dir_name: &str) -> Option<String> {
    let (prefix, hash) = dir_name.rsplit_once('_')?;
    if prefix.is_empty() || hash.is_empty() {
        return None;
    }
    if !prefix.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit()) {
        return None;
    }
    if !hash.chars().all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()) {
        return None;
    }
    Some(prefix.to_string())
}

/// Get the path to the latest.json continuation file
/// This follows the symlink to get the actual path
pub fn get_latest_continuation_path() -> PathBuf {
    get_session_dir().join(CONTINUATION_FILENAME)
}

/// Ensure the .g3 directory exists (but not the session symlink)
pub fn ensure_session_dir() -> Result<PathBuf> {
    let g3_dir = get_g3_dir();
    if !g3_dir.exists() {
        std::fs::create_dir_all(&g3_dir)?;
        debug!("Created .g3 directory: {:?}", g3_dir);
    }
    Ok(get_session_dir())
}

/// Update the .g3/session symlink to point to the given session directory
fn update_session_symlink(session_id: &str) -> Result<()> {
    let symlink_path = get_session_dir();
    let target_path = get_session_path(session_id);
    
    // Remove existing symlink or directory if it exists
    if symlink_path.exists() || symlink_path.is_symlink() {
        if symlink_path.is_symlink() {
            std::fs::remove_file(&symlink_path)
                .context("Failed to remove existing session symlink")?;
        } else if symlink_path.is_dir() {
            // Migration: if it's an old-style directory, remove it
            std::fs::remove_dir_all(&symlink_path)
                .context("Failed to remove old session directory")?;
            debug!("Migrated old .g3/session directory to symlink");
        }
    }
    
    // Create the symlink
    #[cfg(unix)]
    std::os::unix::fs::symlink(&target_path, &symlink_path)
        .context("Failed to create session symlink")?;
    
    #[cfg(windows)]
    std::os::windows::fs::symlink_dir(&target_path, &symlink_path)
        .context("Failed to create session symlink")?;
    
    debug!("Updated session symlink: {:?} -> {:?}", symlink_path, target_path);
    Ok(())
}

/// Save a session continuation artifact
/// This saves latest.json in the session's directory and updates the symlink
pub fn save_continuation(continuation: &SessionContinuation) -> Result<PathBuf> {
    let session_id = &continuation.session_id;
    let session_path = get_session_path(session_id);
    
    // Ensure the session directory exists
    if !session_path.exists() {
        std::fs::create_dir_all(&session_path)
            .context("Failed to create session directory")?;
    }
    
    // Save latest.json in the session directory
    let latest_path = session_path.join(CONTINUATION_FILENAME);
    let json = serde_json::to_string_pretty(continuation)?;
    std::fs::write(&latest_path, &json)?;
    
    // Update the symlink to point to this session
    update_session_symlink(session_id)?;
    
    debug!("Saved session continuation to {:?}", latest_path);
    Ok(latest_path)
}

/// Load the latest session continuation artifact if it exists
pub fn load_continuation() -> Result<Option<SessionContinuation>> {
    let symlink_path = get_session_dir();
    
    // Check if the symlink exists and is valid
    if !symlink_path.is_symlink() && !symlink_path.exists() {
        debug!("No session symlink found at {:?}", symlink_path);
        return Ok(None);
    }
    
    // If it's a symlink, check if the target exists
    if symlink_path.is_symlink() {
        let target = std::fs::read_link(&symlink_path)?;
        if !target.exists() && !symlink_path.exists() {
            debug!("Session symlink target does not exist: {:?}", target);
            return Ok(None);
        }
    }
    
    let latest_path = symlink_path.join(CONTINUATION_FILENAME);
    
    if !latest_path.exists() {
        debug!("No continuation file found at {:?}", latest_path);
        return Ok(None);
    }
    
    let json = std::fs::read_to_string(&latest_path)?;
    let continuation: SessionContinuation = serde_json::from_str(&json)?;
    
    // Validate version
    if continuation.version != CONTINUATION_VERSION {
        warn!(
            "Continuation version mismatch: expected {}, got {}",
            CONTINUATION_VERSION, continuation.version
        );
    }
    
    debug!("Loaded session continuation from {:?}", latest_path);
    Ok(Some(continuation))
}

/// Load a session continuation by session ID (full or partial prefix match).
/// 
/// This function searches for sessions matching the given ID:
/// - First looks for `latest.json` (saved continuation artifact)
/// - Falls back to constructing a continuation from `session.json` if available
/// 
/// This function searches for sessions matching the given ID:
/// - If an exact match is found, it returns that session
/// - If a unique prefix match is found, it returns that session
/// - If multiple sessions match the prefix, it returns an error listing them
/// - If no sessions match, it returns an error
/// 
/// The session must be in the current working directory.
pub fn load_continuation_by_id(session_id: &str) -> Result<SessionContinuation> {
    let sessions_dir = get_sessions_dir();
    
    if !sessions_dir.exists() {
        anyhow::bail!("No sessions directory found. No sessions have been created yet.");
    }
    
    let current_dir = std::env::current_dir()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_default();
    
    let mut matches: Vec<SessionContinuation> = Vec::new();
    
    // Scan all session directories for matches
    for entry in std::fs::read_dir(&sessions_dir)? {
        let entry = entry?;
        let path = entry.path();
        
        if !path.is_dir() {
            continue;
        }
        
        let dir_name = path.file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("");
        
        // Check if this session ID matches (exact or prefix)
        if !dir_name.starts_with(session_id) {
            continue;
        }
        
        // Check for latest.json in this session directory
        let latest_path = path.join(CONTINUATION_FILENAME);
        let session_json_path = path.join("session.json");
        
        // Try to load from latest.json first, then fall back to session.json
        let continuation: SessionContinuation = if latest_path.exists() {
            let json = std::fs::read_to_string(&latest_path)?;
            serde_json::from_str(&json)?
        } else if session_json_path.exists() {
            // Construct a continuation from session.json
            let json = std::fs::read_to_string(&session_json_path)?;
            let session_data: serde_json::Value = serde_json::from_str(&json)?;
            
            // Extract working directory from session data
            let working_dir = session_data
                .get("working_directory")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string();
            
            // Extract context percentage
            let context_pct = session_data
                .get("context_window")
                .and_then(|cw| cw.get("percentage_used"))
                .and_then(|v| v.as_f64())
                .unwrap_or(0.0) as f32;
            
            SessionContinuation {
                version: CONTINUATION_VERSION.to_string(),
                // session.json carries no agent fields, so recover them from the
                // directory name (see agent_name_from_session_id).
                is_agent_mode: session_data
                    .get("is_agent_mode")
                    .and_then(|v| v.as_bool())
                    .unwrap_or_else(|| agent_name_from_session_id(dir_name).is_some()),
                agent_name: session_data
                    .get("agent_name")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string())
                    .or_else(|| agent_name_from_session_id(dir_name)),
                created_at: session_data.get("timestamp").and_then(|v| v.as_str()).unwrap_or_default().to_string(),
                session_id: dir_name.to_string(),
                description: None,
                summary: None,
                session_log_path: session_json_path.to_string_lossy().to_string(),
                context_percentage: context_pct,
                todo_snapshot: None,
                working_directory: working_dir,
            }
        } else {
            continue;
        };
        
        // Only include sessions from the current working directory
        // If working_directory is empty (constructed from session.json without this field),
        // we allow it since the user is explicitly requesting by ID
        if continuation.working_directory.is_empty() 
            || continuation.working_directory == current_dir {
            matches.push(continuation);
        }
    }
    
    match matches.len() {
        0 => anyhow::bail!("No session found matching '{}' in current directory", session_id),
        1 => Ok(matches.remove(0)),
        _ => {
            let ids: Vec<_> = matches.iter().map(|s| s.session_id.as_str()).collect();
            anyhow::bail!("Multiple sessions match '{}': {}", session_id, ids.join(", "));
        }
    }
}

/// Clear the session continuation symlink (for /clear command)
/// This only removes the symlink, not the actual session data
pub fn clear_continuation() -> Result<()> {
    let symlink_path = get_session_dir();
    
    if symlink_path.is_symlink() {
        std::fs::remove_file(&symlink_path)?;
        debug!("Removed session symlink: {:?}", symlink_path);
    } else if symlink_path.is_dir() {
        // Handle old-style directory (migration case)
        for entry in std::fs::read_dir(&symlink_path)? {
            let entry = entry?;
            let path = entry.path();
            if path.is_file() {
                std::fs::remove_file(&path)?;
                debug!("Removed session file: {:?}", path);
            }
        }
        std::fs::remove_dir(&symlink_path)?;
        debug!("Removed old session directory: {:?}", symlink_path);
    }
    
    debug!("Cleared session continuation");
    Ok(())
}

/// Check if a continuation exists and is valid
pub fn has_valid_continuation() -> bool {
    match load_continuation() {
        Ok(Some(continuation)) => {
            // Check if the session log still exists
            let session_log_path = PathBuf::from(&continuation.session_log_path);
            if !session_log_path.exists() {
                warn!("Session log no longer exists: {:?}", session_log_path);
                return false;
            }
            
            // Check if we're in the same working directory
            let current_dir = std::env::current_dir()
                .map(|p| p.to_string_lossy().to_string())
                .unwrap_or_default();
            
            if current_dir != continuation.working_directory {
                debug!(
                    "Working directory changed: {} -> {}",
                    continuation.working_directory, current_dir
                );
                // Still valid, but user should be aware
            }
            
            true
        }
        Ok(None) => false,
        Err(e) => {
            error!("Error checking continuation: {}", e);
            false
        }
    }
}

/// Load the full context window from a session log file
pub fn load_context_from_session_log(session_log_path: &Path) -> Result<Option<serde_json::Value>> {
    if !session_log_path.exists() {
        return Ok(None);
    }
    
    let json = std::fs::read_to_string(session_log_path)?;
    let session_data: serde_json::Value = serde_json::from_str(&json)?;
    
    Ok(Some(session_data))
}

/// Find an incomplete agent session for the given agent name.
/// Returns the most recent session that:
/// 1. Was running in agent mode with the matching agent name
/// 2. Has incomplete TODO items (contains "- [ ]")
/// 3. Is in the same working directory
pub fn find_incomplete_agent_session(agent_name: &str) -> Result<Option<SessionContinuation>> {
    let sessions_dir = get_sessions_dir();
    
    if !sessions_dir.exists() {
        debug!("Sessions directory does not exist: {:?}", sessions_dir);
        return Ok(None);
    }
    
    let current_dir = std::env::current_dir()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_default();
    
    let mut candidates: Vec<SessionContinuation> = Vec::new();
    
    // Scan all session directories
    for entry in std::fs::read_dir(&sessions_dir)? {
        let entry = entry?;
        let path = entry.path();
        
        if !path.is_dir() {
            continue;
        }
        
        // Check for latest.json in this session directory
        let latest_path = path.join(CONTINUATION_FILENAME);
        if !latest_path.exists() {
            continue;
        }
        
        // Try to load the continuation
        let json = match std::fs::read_to_string(&latest_path) {
            Ok(j) => j,
            Err(_) => continue,
        };
        
        let continuation: SessionContinuation = match serde_json::from_str(&json) {
            Ok(c) => c,
            Err(_) => continue, // Skip sessions with old format
        };
        
        // Check if this is an agent mode session with matching name
        if !continuation.is_agent_mode {
            continue;
        }
        
        if continuation.agent_name.as_deref() != Some(agent_name) {
            continue;
        }
        
        // Check if in same working directory
        if continuation.working_directory != current_dir {
            continue;
        }
        
        // Check if has incomplete TODOs (either in snapshot or in the actual file)
        let has_incomplete = if continuation.has_incomplete_todos() {
            true
        } else if continuation.todo_snapshot.is_none() {
            // Fallback: check the actual todo.g3.md file in the session directory
            // This handles sessions created before todo_snapshot was properly saved
            let todo_file_path = path.join("todo.g3.md");
            if todo_file_path.exists() {
                std::fs::read_to_string(&todo_file_path)
                    .map(|content| content.contains("- [ ]"))
                    .unwrap_or(false)
            } else {
                false
            }
        } else {
            false
        };
        
        if has_incomplete {
            candidates.push(continuation);
        }
    }
    
    // Sort by created_at descending and return the most recent
    candidates.sort_by(|a, b| b.created_at.cmp(&a.created_at));
    Ok(candidates.into_iter().next())
}

/// List all available sessions in the current working directory.
/// Returns sessions sorted by creation time (most recent first).
pub fn list_sessions_for_directory() -> Result<Vec<SessionContinuation>> {
    let sessions_dir = get_sessions_dir();
    
    if !sessions_dir.exists() {
        debug!("Sessions directory does not exist: {:?}", sessions_dir);
        return Ok(Vec::new());
    }
    
    let current_dir = std::env::current_dir()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_default();
    
    let mut sessions: Vec<SessionContinuation> = Vec::new();
    
    // Scan all session directories
    for entry in std::fs::read_dir(&sessions_dir)? {
        let entry = entry?;
        let path = entry.path();
        
        if !path.is_dir() {
            continue;
        }
        
        // Check for latest.json in this session directory
        let latest_path = path.join(CONTINUATION_FILENAME);
        if !latest_path.exists() {
            continue;
        }
        
        // Try to load the continuation
        let json = match std::fs::read_to_string(&latest_path) {
            Ok(j) => j,
            Err(_) => continue,
        };
        
        let continuation: SessionContinuation = match serde_json::from_str(&json) {
            Ok(c) => c,
            Err(_) => continue, // Skip sessions with old format
        };
        
        // Only include sessions from the current working directory
        if continuation.working_directory == current_dir {
            sessions.push(continuation);
        }
    }
    
    // Sort by created_at descending (most recent first)
    sessions.sort_by(|a, b| b.created_at.cmp(&a.created_at));
    
    Ok(sessions)
}

/// Format a session's created_at timestamp for display
pub fn format_session_time(created_at: &str) -> String {
    match chrono::DateTime::parse_from_rfc3339(created_at) {
        Ok(dt) => {
            let local: chrono::DateTime<chrono::Local> = dt.into();
            let now = chrono::Local::now();
            let duration = now.signed_duration_since(local);
            
            // Show relative time for recent sessions, absolute for older ones
            if duration.num_minutes() < 1 {
                "just now".to_string()
            } else if duration.num_minutes() < 60 {
                format!("{} min ago", duration.num_minutes())
            } else if duration.num_hours() < 24 {
                let hours = duration.num_hours();
                if hours == 1 {
                    "1 hour ago".to_string()
                } else {
                    format!("{} hours ago", hours)
                }
            } else if duration.num_days() < 7 {
                let days = duration.num_days();
                if days == 1 {
                    "yesterday".to_string()
                } else {
                    format!("{} days ago", days)
                }
            } else {
                // For older sessions, show the date
                local.format("%b %d, %Y").to_string()
            }
        }
        Err(_) => created_at.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── agent recovery from the session id ──────────────────────────────────
    //
    // Regression: latest.json is written only on graceful exit, so resuming a
    // running or crashed session falls back to session.json — which has never
    // held an agent_name. That made `g3 --agent butler --resume <id>` fail with
    // "Session '<id>' belongs to agent '(none)', not 'butler'" on its own
    // sessions. 4 real conversations were unresumable this way.

    #[test]
    fn agent_mode_ids_yield_their_agent() {
        assert_eq!(
            agent_name_from_session_id("butler_fd63622402933845").as_deref(),
            Some("butler")
        );
        assert_eq!(
            agent_name_from_session_id("scout_1b2c").as_deref(),
            Some("scout")
        );
    }

    #[test]
    fn prompt_derived_ids_have_no_agent() {
        // A bare `g3 "<prompt>"` names the dir after the first 5 words, so the
        // prefix is multi-word and must NOT be read as an agent. Getting this
        // wrong would let butler.app resume duty runs as if they were chats.
        for id in [
            "process_new_emails_9f3a",
            "produce_a_morning_briefing_for_1a2b",
            "create_a_plan_what_is_67ab",
        ] {
            assert_eq!(agent_name_from_session_id(id), None, "id: {}", id);
        }
    }

    #[test]
    fn malformed_ids_are_rejected_rather_than_guessed() {
        // No hash, empty halves, non-hex tail, uppercase — in every case we do
        // not know the agent, and saying so beats inventing one.
        for id in [
            "butler",
            "butler_",
            "_abc123",
            "butler_xyz",
            "Butler_ab12",
            "",
            "_",
        ] {
            assert_eq!(agent_name_from_session_id(id), None, "id: {}", id);
        }
    }

    #[test]
    fn single_char_boundaries() {
        assert_eq!(agent_name_from_session_id("a_f").as_deref(), Some("a"));
        // 16 hex chars is what the real generator emits; longer is still hex.
        assert_eq!(
            agent_name_from_session_id("butler_0123456789abcdef").as_deref(),
            Some("butler")
        );
    }

    #[test]
    fn recovery_agrees_with_the_real_generator() {
        // The whole premise: generate_session_id() uses the agent name AS the
        // prefix. Assert that against the generator itself, so a change there
        // breaks this test rather than silently breaking resume.
        for agent in ["butler", "scout", "g3"] {
            let id = crate::session::generate_session_id("some prompt text", Some(agent));
            assert_eq!(
                agent_name_from_session_id(&id).as_deref(),
                Some(agent),
                "generated id: {}",
                id
            );
        }
        // ...and that a non-agent invocation is not mistaken for one.
        let id = crate::session::generate_session_id("process new emails now please", None);
        assert_eq!(agent_name_from_session_id(&id), None, "generated id: {}", id);
    }

    #[test]
    fn test_session_continuation_creation() {
        let continuation = SessionContinuation::new(
            false,
            None,
            "test_session_123".to_string(),
            Some("Task completed successfully".to_string()),
            None,
            "/path/to/session.json".to_string(),
            45.0,
            Some("- [x] Task 1\n- [ ] Task 2".to_string()),
            "/home/user/project".to_string(),
        );
        
        assert_eq!(continuation.version, CONTINUATION_VERSION);
        assert_eq!(continuation.session_id, "test_session_123");
        assert!(continuation.can_restore_full_context());
    }

    #[test]
    fn test_can_restore_full_context() {
        let mut continuation = SessionContinuation::new(
            false,
            None,
            "test".to_string(),
            None,
            None,
            "path".to_string(),
            50.0,
            None,
            ".".to_string(),
        );
        
        assert!(continuation.can_restore_full_context()); // 50% < 80%
        
        continuation.context_percentage = 80.0;
        assert!(!continuation.can_restore_full_context()); // 80% >= 80%
        
        continuation.context_percentage = 95.0;
        assert!(!continuation.can_restore_full_context()); // 95% >= 80%
    }

    #[test]
    fn test_has_incomplete_todos() {
        let mut continuation = SessionContinuation::new(
            true,
            Some("fowler".to_string()),
            "test".to_string(),
            None,
            None,
            "path".to_string(),
            50.0,
            Some("- [x] Done\n- [ ] Not done".to_string()),
            ".".to_string(),
        );
        
        assert!(continuation.has_incomplete_todos());
        
        continuation.todo_snapshot = Some("- [x] All done".to_string());
        assert!(!continuation.has_incomplete_todos());
        
        continuation.todo_snapshot = None;
        assert!(!continuation.has_incomplete_todos());
    }
}
