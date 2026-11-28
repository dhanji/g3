//! Regression: resuming a session whose context is >= 80% must still restore
//! the FULL transcript and continue in place.
//!
//! ## The bug (2026-11-28, butler.app chat "API Key Rotation")
//!
//! `restore_from_continuation` only full-restored when
//! `context_percentage < 80`. Above that it fell back to "summary + TODO" — but
//! butler.app sessions never carry a summary, so the fallback restored NOTHING
//! and minted a fresh session dir. A turn that ended at 81.06% was therefore
//! unreachable: every `--resume <id> confirm` forked a blank chat that asked
//! "confirm what?", and because compaction only runs inside a turn, the
//! session could never shrink back under 80%. Permanently stuck.
//!
//! Loading a large transcript is safe: `ensure_context_capacity` thins and
//! compacts before the first request of the turn is streamed.

use g3_config::Config;
use g3_core::session_continuation::SessionContinuation;
use g3_core::ui_writer::NullUiWriter;
use g3_core::Agent;
use serde_json::json;
use serial_test::serial;
use tempfile::TempDir;

const SID: &str = "butler_highctx";

fn write_session(dir: &TempDir) -> String {
    let sdir = dir.path().join(".g3/sessions").join(SID);
    std::fs::create_dir_all(&sdir).unwrap();
    let path = sdir.join("session.json");
    let body = json!({
        "session_id": SID,
        "status": "completed",
        "context_window": {
            "conversation_history": [
                {"role": "user", "content": "do the five commits", "id": "a"},
                {"role": "assistant", "content": "Commit 5 is in. Confirm commit 6?", "id": "b"}
            ],
            "percentage_used": 81.06,
            "total_tokens": 990000,
            "used_tokens": 802450
        }
    });
    std::fs::write(&path, body.to_string()).unwrap();
    path.to_string_lossy().into_owned()
}

fn continuation(log: String, pct: f32, summary: Option<&str>) -> SessionContinuation {
    SessionContinuation {
        version: "1.0".to_string(),
        is_agent_mode: true,
        agent_name: Some("butler".to_string()),
        created_at: "2026-11-28T07:28:11+00:00".to_string(),
        session_id: SID.to_string(),
        description: Some("api key rotation".to_string()),
        summary: summary.map(str::to_string),
        session_log_path: log,
        context_percentage: pct,
        todo_snapshot: None,
        working_directory: "/tmp".to_string(),
    }
}

async fn agent(dir: &TempDir) -> Agent<NullUiWriter> {
    std::env::set_current_dir(dir.path()).unwrap();
    let mut a = Agent::new_with_project_context_and_quiet(Config::default(), NullUiWriter, None, true)
        .await
        .unwrap();
    a.set_agent_mode("butler");
    a
}

fn history_mentions(a: &Agent<NullUiWriter>, needle: &str) -> bool {
    a.get_context_window()
        .conversation_history
        .iter()
        .any(|m| m.content.contains(needle))
}

#[tokio::test]
#[serial]
async fn resume_at_81_percent_restores_full_history_in_place() {
    let dir = TempDir::new().unwrap();
    let log = write_session(&dir);
    let mut a = agent(&dir).await;

    let full = a.restore_from_continuation(&continuation(log, 81.06, None)).unwrap();

    assert!(full, "a readable transcript must take the full-restore path at any %");
    assert_eq!(a.get_session_id(), Some(SID), "must continue IN PLACE, not fork a new dir");
    assert!(history_mentions(&a, "Confirm commit 6?"), "the transcript must be in memory");
}

#[tokio::test]
#[serial]
async fn resume_at_100_percent_still_restores_full_history() {
    let dir = TempDir::new().unwrap();
    let log = write_session(&dir);
    let mut a = agent(&dir).await;

    let full = a.restore_from_continuation(&continuation(log, 100.0, Some("s"))).unwrap();

    assert!(full);
    assert_eq!(a.get_session_id(), Some(SID));
    assert!(history_mentions(&a, "do the five commits"));
}

#[tokio::test]
#[serial]
async fn missing_log_with_a_summary_falls_back_without_adopting_the_id() {
    // The summary path survives for a missing/unreadable log, and must still
    // NOT continue in place (it would clobber whatever is at that id).
    let dir = TempDir::new().unwrap();
    let mut a = agent(&dir).await;
    let missing = dir.path().join("nope/session.json").to_string_lossy().into_owned();

    let full = a
        .restore_from_continuation(&continuation(missing, 10.0, Some("the gist")))
        .unwrap();

    assert!(!full);
    assert_ne!(a.get_session_id(), Some(SID));
    assert!(history_mentions(&a, "the gist"));
}

#[tokio::test]
#[serial]
async fn nothing_restorable_is_an_error_not_a_silent_blank_session() {
    // No transcript, no summary, no TODO: proceeding would answer the user's
    // message in an empty chat — exactly the "confirm what?" failure. Refuse.
    let dir = TempDir::new().unwrap();
    let mut a = agent(&dir).await;
    let missing = dir.path().join("nope/session.json").to_string_lossy().into_owned();

    let r = a.restore_from_continuation(&continuation(missing, 81.0, None));

    assert!(r.is_err(), "an empty restore must be an error, got {:?}", r.ok());
}
