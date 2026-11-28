//! Tests for session continuation functionality
//!
//! Note: These tests use serial execution because they modify the current directory

use g3_core::session_continuation::{
    SessionContinuation, clear_continuation, ensure_session_dir,
    get_latest_continuation_path, get_session_dir, has_valid_continuation,
    load_continuation, save_continuation,
};
use std::fs;
use std::sync::Mutex;
use tempfile::TempDir;

// Global mutex to ensure tests run serially (they modify current directory)
static TEST_MUTEX: Mutex<()> = Mutex::new(());

/// Helper to set up a test environment with a temporary directory
/// Returns the temp dir (must be kept alive) and the original directory
fn setup_test_env() -> (TempDir, std::path::PathBuf) {
    let original_dir = std::env::current_dir().expect("Failed to get current dir");
    let temp_dir = TempDir::new().expect("Failed to create temp dir");
    std::env::set_current_dir(temp_dir.path()).expect("Failed to change to temp dir");
    (temp_dir, original_dir)
}

/// Restore the original directory
fn teardown_test_env(original_dir: std::path::PathBuf) {
    let _ = std::env::set_current_dir(original_dir);
}

#[test]
fn test_session_continuation_creation() {
    // This test doesn't need file system access
    let continuation = SessionContinuation::new(false, None, 
        "test_session_123".to_string(),
        None,
        Some("Task completed successfully".to_string()),
        "/path/to/session.json".to_string(),
        45.0,
        Some("- [x] Task 1\n- [ ] Task 2".to_string()),
        "/home/user/project".to_string(),
    );

    assert_eq!(continuation.session_id, "test_session_123");
    assert_eq!(
        continuation.summary,
        Some("Task completed successfully".to_string())
    );
    assert_eq!(continuation.context_percentage, 45.0);
}

#[test]
fn test_save_and_load_continuation() {
    let _lock = TEST_MUTEX.lock().unwrap();
    let (temp_dir, original_dir) = setup_test_env();

    let original = SessionContinuation::new(false, None, 
        "save_load_test".to_string(),
        None,
        Some("Test summary content".to_string()),
        "/.g3/sessions/save_load_test/session.json".to_string(),
        35.5,
        Some("- [ ] Pending task".to_string()),
        temp_dir.path().to_string_lossy().to_string(),
    );

    // Save the continuation
    let saved_path = save_continuation(&original).expect("Failed to save continuation");
    assert!(saved_path.exists());

    // Verify the symlink was created
    let session_dir = get_session_dir();
    assert!(session_dir.is_symlink(), "session should be a symlink");

    // Load it back
    let loaded = load_continuation()
        .expect("Failed to load continuation")
        .expect("No continuation found");

    assert_eq!(loaded.session_id, original.session_id);
    assert_eq!(loaded.summary, original.summary);
    assert_eq!(loaded.session_log_path, original.session_log_path);
    assert!((loaded.context_percentage - original.context_percentage).abs() < 0.01);
    assert_eq!(loaded.todo_snapshot, original.todo_snapshot);
    assert_eq!(loaded.working_directory, original.working_directory);

    teardown_test_env(original_dir);
}

#[test]
fn test_find_incomplete_agent_session() {
    use g3_core::session_continuation::find_incomplete_agent_session;
    
    let _lock = TEST_MUTEX.lock().unwrap();
    let (_temp_dir, original_dir) = setup_test_env();

    // Get the actual current directory (after set_current_dir in setup)
    let current_working_dir = std::env::current_dir()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_default();

    // Create an agent mode session with incomplete TODOs
    let agent_session = SessionContinuation::new(
        true,  // is_agent_mode
        Some("fowler".to_string()),  // agent_name
        "fowler_session_1".to_string(),
        None,
        Some("Working on task".to_string()),
        "/path/to/session.json".to_string(),
        50.0,
        Some("- [x] Done\n- [ ] Not done yet".to_string()),  // incomplete TODO
        current_working_dir,  // Use actual current dir
    );
    save_continuation(&agent_session).expect("Failed to save agent session");

    // Should find the incomplete session for "fowler"
    let result = find_incomplete_agent_session("fowler").expect("Failed to search");
    assert!(result.is_some(), "Should find incomplete fowler session");
    let found = result.unwrap();
    assert_eq!(found.session_id, "fowler_session_1");
    assert_eq!(found.agent_name, Some("fowler".to_string()));

    // Should NOT find session for different agent
    let result = find_incomplete_agent_session("pike").expect("Failed to search");
    assert!(result.is_none(), "Should not find session for pike");

    teardown_test_env(original_dir);
}

#[test]
fn test_find_incomplete_agent_session_ignores_complete_todos() {
    use g3_core::session_continuation::find_incomplete_agent_session;
    
    let _lock = TEST_MUTEX.lock().unwrap();
    let (_temp_dir, original_dir) = setup_test_env();

    let current_working_dir = std::env::current_dir()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_default();

    // Create an agent mode session with ALL TODOs complete
    let complete_session = SessionContinuation::new(
        true,
        Some("fowler".to_string()),
        "fowler_complete".to_string(),
        None,
        Some("All done".to_string()),
        "/path/to/session.json".to_string(),
        50.0,
        Some("- [x] Task 1\n- [x] Task 2".to_string()),  // all complete
        current_working_dir,
    );
    save_continuation(&complete_session).expect("Failed to save");

    // Should NOT find session since all TODOs are complete
    let result = find_incomplete_agent_session("fowler").expect("Failed to search");
    assert!(result.is_none(), "Should not find session with complete TODOs");

    teardown_test_env(original_dir);
}

#[test]
fn test_find_incomplete_agent_session_ignores_non_agent_mode() {
    use g3_core::session_continuation::find_incomplete_agent_session;
    
    let _lock = TEST_MUTEX.lock().unwrap();
    let (_temp_dir, original_dir) = setup_test_env();

    let current_working_dir = std::env::current_dir()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_default();

    // Create a NON-agent mode session with incomplete TODOs
    let non_agent_session = SessionContinuation::new(
        false,  // NOT agent mode
        None,
        "regular_session".to_string(),
        None,
        None,
        "/path/to/session.json".to_string(),
        50.0,
        Some("- [ ] Incomplete task".to_string()),
        current_working_dir,
    );
    save_continuation(&non_agent_session).expect("Failed to save");

    // Should NOT find session since it's not agent mode
    let result = find_incomplete_agent_session("fowler").expect("Failed to search");
    assert!(result.is_none(), "Should not find non-agent-mode session");

    teardown_test_env(original_dir);
}

#[test]
fn test_load_continuation_when_none_exists() {
    let _lock = TEST_MUTEX.lock().unwrap();
    let (_temp_dir, original_dir) = setup_test_env();

    // No continuation should exist in a fresh temp directory
    let result = load_continuation().expect("load_continuation should not error");
    assert!(result.is_none());

    teardown_test_env(original_dir);
}

#[test]
fn test_clear_continuation() {
    let _lock = TEST_MUTEX.lock().unwrap();
    let (_temp_dir, original_dir) = setup_test_env();

    // Create and save a continuation
    let continuation = SessionContinuation::new(false, None, 
        "clear_test".to_string(),
        None,
        Some("Will be cleared".to_string()),
        "/path/to/session.json".to_string(),
        50.0,
        None,
        ".".to_string(),
    );
    save_continuation(&continuation).expect("Failed to save");

    // Verify the symlink exists
    let session_dir = get_session_dir();
    assert!(session_dir.is_symlink(), "session should be a symlink after save");

    // Clear it
    clear_continuation().expect("Failed to clear");

    // Verify the symlink is gone
    assert!(!session_dir.exists() && !session_dir.is_symlink(), "symlink should be removed");

    // Loading should return None
    let result = load_continuation().expect("load should not error");
    assert!(result.is_none());

    teardown_test_env(original_dir);
}

#[test]
fn test_ensure_session_dir_creates_g3_directory() {
    let _lock = TEST_MUTEX.lock().unwrap();
    let (temp_dir, original_dir) = setup_test_env();

    let g3_dir = temp_dir.path().join(".g3");
    assert!(!g3_dir.exists());

    ensure_session_dir().expect("Failed to ensure session dir");

    // The .g3 directory should exist, but not the session symlink
    assert!(g3_dir.exists(), ".g3 directory should be created");
    assert!(g3_dir.is_dir(), ".g3 should be a directory");
    
    // The session symlink should NOT exist until save_continuation is called
    let session_dir = get_session_dir();
    assert!(!session_dir.exists() && !session_dir.is_symlink(), 
            "session symlink should not exist until save_continuation is called");

    teardown_test_env(original_dir);
}

#[test]
fn test_has_valid_continuation_with_missing_session_log() {
    let _lock = TEST_MUTEX.lock().unwrap();
    let (_temp_dir, original_dir) = setup_test_env();

    // Create a continuation pointing to a non-existent session log
    let continuation = SessionContinuation::new(false, None, 
        "invalid_test".to_string(),
        None,
        Some("Summary".to_string()),
        "/nonexistent/path/session.json".to_string(),
        30.0,
        None,
        ".".to_string(),
    );
    save_continuation(&continuation).expect("Failed to save");

    // Should be invalid because session log doesn't exist
    assert!(!has_valid_continuation());

    teardown_test_env(original_dir);
}

#[test]
fn test_has_valid_continuation_with_existing_session_log() {
    let _lock = TEST_MUTEX.lock().unwrap();
    let (temp_dir, original_dir) = setup_test_env();

    // Create a fake session log file
    let session_dir = temp_dir.path().join(".g3").join("sessions").join("valid_test");
    fs::create_dir_all(&session_dir).expect("Failed to create session dir");
    let session_log_path = session_dir.join("session.json");
    fs::write(&session_log_path, "{}").expect("Failed to write session log");

    // Create a continuation pointing to the existing session log
    let continuation = SessionContinuation::new(false, None, 
        "valid_test".to_string(),
        None,
        Some("Summary".to_string()),
        session_log_path.to_string_lossy().to_string(),
        30.0,
        None,
        temp_dir.path().to_string_lossy().to_string(),
    );
    save_continuation(&continuation).expect("Failed to save");

    // Should be valid because session log exists
    assert!(has_valid_continuation());

    teardown_test_env(original_dir);
}

#[test]
fn test_continuation_serialization_format() {
    let _lock = TEST_MUTEX.lock().unwrap();
    let (_temp_dir, original_dir) = setup_test_env();

    let continuation = SessionContinuation::new(false, None, 
        "format_test".to_string(),
        None,
        Some("Test summary".to_string()),
        "/path/to/session.json".to_string(),
        42.5,
        Some("- [x] Done\n- [ ] Todo".to_string()),
        "/workspace".to_string(),
    );
    save_continuation(&continuation).expect("Failed to save");

    // Read the raw JSON and verify structure
    let json_content =
        fs::read_to_string(get_latest_continuation_path()).expect("Failed to read file");
    let parsed: serde_json::Value =
        serde_json::from_str(&json_content).expect("Failed to parse JSON");

    assert_eq!(parsed["version"], "1.0");
    assert_eq!(parsed["session_id"], "format_test");
    assert_eq!(parsed["summary"], "Test summary");
    assert_eq!(parsed["session_log_path"], "/path/to/session.json");
    assert!((parsed["context_percentage"].as_f64().unwrap() - 42.5).abs() < 0.01);
    assert_eq!(parsed["todo_snapshot"], "- [x] Done\n- [ ] Todo");
    assert_eq!(parsed["working_directory"], "/workspace");
    assert!(parsed["created_at"].as_str().is_some()); // Should have a timestamp

    teardown_test_env(original_dir);
}

#[test]
fn test_multiple_saves_update_symlink() {
    let _lock = TEST_MUTEX.lock().unwrap();
    let (temp_dir, original_dir) = setup_test_env();

    // Save first continuation
    let first = SessionContinuation::new(false, None, 
        "first_session".to_string(),
        None,
        Some("First summary".to_string()),
        "/path/first.json".to_string(),
        20.0,
        None,
        ".".to_string(),
    );
    save_continuation(&first).expect("Failed to save first");

    // Verify symlink points to first session
    let session_dir = get_session_dir();
    let first_target = fs::read_link(&session_dir).expect("Failed to read symlink");
    assert!(first_target.to_string_lossy().contains("first_session"));

    // Save second continuation (should update symlink)
    let second = SessionContinuation::new(false, None, 
        "second_session".to_string(),
        None,
        Some("Second summary".to_string()),
        "/path/second.json".to_string(),
        60.0,
        None,
        ".".to_string(),
    );
    save_continuation(&second).expect("Failed to save second");

    // Verify symlink now points to second session
    let second_target = fs::read_link(&session_dir).expect("Failed to read symlink");
    assert!(second_target.to_string_lossy().contains("second_session"));

    // Load should return the second one
    let loaded = load_continuation()
        .expect("Failed to load")
        .expect("No continuation");
    assert_eq!(loaded.session_id, "second_session");
    assert_eq!(
        loaded.summary,
        Some("Second summary".to_string())
    );

    // Both session directories should exist with their own latest.json
    let sessions_dir = temp_dir.path().join(".g3").join("sessions");
    assert!(sessions_dir.join("first_session").join("latest.json").exists());
    assert!(sessions_dir.join("second_session").join("latest.json").exists());

    teardown_test_env(original_dir);
}

#[test]
fn test_symlink_migration_from_old_directory() {
    let _lock = TEST_MUTEX.lock().unwrap();
    let (temp_dir, original_dir) = setup_test_env();

    // Create an old-style .g3/session directory with latest.json
    let old_session_dir = temp_dir.path().join(".g3").join("session");
    fs::create_dir_all(&old_session_dir).expect("Failed to create old session dir");
    let old_latest = old_session_dir.join("latest.json");
    fs::write(&old_latest, r#"{"version":"1.0","session_id":"old"}"#)
        .expect("Failed to write old latest.json");

    // Save a new continuation - this should migrate the old directory to a symlink
    let continuation = SessionContinuation::new(false, None, 
        "new_session".to_string(),
        None,
        Some("New summary".to_string()),
        "/path/to/session.json".to_string(),
        50.0,
        None,
        ".".to_string(),
    );
    save_continuation(&continuation).expect("Failed to save");

    // The session path should now be a symlink, not a directory
    let session_dir = get_session_dir();
    assert!(session_dir.is_symlink(), "session should be a symlink after migration");

    // Load should return the new session
    let loaded = load_continuation()
        .expect("Failed to load")
        .expect("No continuation");
    assert_eq!(loaded.session_id, "new_session");

    teardown_test_env(original_dir);
}
