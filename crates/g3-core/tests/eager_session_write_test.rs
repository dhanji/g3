//! Tests for eager `session.json` persistence and the resume-side trim that
//! makes it safe.
//!
//! # The bug this fixes
//!
//! `session.json` used to be written ONLY at the end of a turn (the
//! `save_context_window("completed"/"error"/"cancelled")` calls). A "turn" in g3
//! is the whole agentic loop — user message, N tool calls, final reply — which
//! routinely runs for minutes. Kill it anywhere in there (wall-clock kill,
//! crash, machine sleep, or restarting the server that hosts it) and the
//! session directory was left holding `context_summary.txt` and nothing else:
//! no transcript to read, nothing for `--resume` to load. The conversation was
//! simply gone.
//!
//! Measured on one real butler workspace: 10 of 219 session dirs had no
//! `session.json`; the largest had lost a 210-message conversation.
//!
//! The fix writes the transcript once per LLM iteration. These tests pin the two
//! properties that make that safe:
//!
//! 1. A mid-turn snapshot is READABLE and marked `running`, so a reader can tell
//!    "in progress" from "finished" (and, after a kill, "died mid-turn").
//! 2. A snapshot ending in an unanswered tool call is TRIMMED on restore.
//!    Anthropic requires every `tool_use` to be answered by a `tool_result` in
//!    the next message; `anthropic.rs` strips orphans defensively but the other
//!    providers (databricks, gemini, openai) do not, so restoring that shape
//!    would produce an invalid request.

use g3_core::session::trim_unanswered_tool_calls;
use g3_providers::{Message, MessageRole, MessageToolCall};

fn assistant_with_calls(content: &str, ids: &[&str]) -> Message {
    let mut m = Message::new(MessageRole::Assistant, content.to_string());
    m.tool_calls = ids
        .iter()
        .map(|id| MessageToolCall {
            id: id.to_string(),
            name: "shell".to_string(),
            input: serde_json::json!({"command": "ls"}),
        })
        .collect();
    m
}

fn tool_result(id: &str) -> Message {
    let mut m = Message::new(MessageRole::User, format!("Tool result: ok ({id})"));
    m.tool_result_id = Some(id.to_string());
    m
}

fn user(content: &str) -> Message {
    Message::new(MessageRole::User, content.to_string())
}

fn assistant(content: &str) -> Message {
    Message::new(MessageRole::Assistant, content.to_string())
}

// ── Happy path: the orphan is trimmed ───────────────────────────────────────

#[test]
fn trims_trailing_assistant_whose_tool_call_was_never_answered() {
    // The exact shape a mid-dispatch kill leaves behind: the assistant asked for
    // a tool and the process died before the result was appended.
    let mut hist = vec![
        user("hi"),
        assistant_with_calls("let me look", &["toolu_1"]),
        tool_result("toolu_1"),
        assistant_with_calls("and now this", &["toolu_2"]), // ← never answered
    ];

    let removed = trim_unanswered_tool_calls(&mut hist);

    assert_eq!(removed, 1, "only the orphaned assistant message should go");
    assert_eq!(hist.len(), 3);
    // The completed exchange before it must survive untouched.
    assert!(matches!(hist[1].role, MessageRole::Assistant));
    assert_eq!(hist[1].tool_calls[0].id, "toolu_1");
    assert_eq!(hist[2].tool_result_id.as_deref(), Some("toolu_1"));
}

#[test]
fn trims_partial_results_along_with_their_orphaned_tool_use() {
    // A multi-tool iteration killed halfway: toolu_1 got answered, toolu_2 did
    // not. The message is still invalid, so it goes — and the now-orphaned
    // tool_result for toolu_1 must go with it, since a tool_result with no
    // tool_use is just as invalid as the reverse.
    let mut hist = vec![
        user("do two things"),
        assistant_with_calls("firing both", &["toolu_1", "toolu_2"]),
        tool_result("toolu_1"),
    ];

    let removed = trim_unanswered_tool_calls(&mut hist);

    assert_eq!(removed, 2);
    assert_eq!(hist.len(), 1);
    assert!(matches!(hist[0].role, MessageRole::User));
}

#[test]
fn trims_repeatedly_when_removing_a_tail_exposes_another_orphan() {
    // Removing the tail can uncover a second orphan underneath, so the trim
    // must loop rather than run once.
    let mut hist = vec![
        user("go"),
        assistant_with_calls("first", &["toolu_1"]), // ← also unanswered
        assistant_with_calls("second", &["toolu_2"]), // ← unanswered
    ];

    let removed = trim_unanswered_tool_calls(&mut hist);

    assert_eq!(removed, 2, "both orphans must be removed, not just the last");
    assert_eq!(hist.len(), 1);
}

// ── Negative: well-formed history must NOT be touched ───────────────────────

#[test]
fn leaves_a_complete_tool_use_result_pair_alone() {
    let mut hist = vec![
        user("hi"),
        assistant_with_calls("looking", &["toolu_1"]),
        tool_result("toolu_1"),
        assistant("here is the answer"),
    ];
    let before = hist.len();

    let removed = trim_unanswered_tool_calls(&mut hist);

    assert_eq!(removed, 0, "a well-formed transcript must survive verbatim");
    assert_eq!(hist.len(), before);
}

#[test]
fn leaves_history_with_no_tool_calls_alone() {
    let mut hist = vec![user("hi"), assistant("hello"), user("bye")];

    let removed = trim_unanswered_tool_calls(&mut hist);

    assert_eq!(removed, 0);
    assert_eq!(hist.len(), 3);
}

#[test]
fn accepts_results_that_are_not_in_the_immediately_next_message() {
    // g3 appends one (tool_use, tool_result) pair per tool, so for a multi-tool
    // iteration the answer to the FIRST call is not adjacent to the message that
    // made it. Requiring adjacency here would trim valid history — data loss
    // dressed up as a safety check.
    let mut hist = vec![
        user("do two things"),
        assistant_with_calls("both at once", &["toolu_1", "toolu_2"]),
        tool_result("toolu_1"),
        tool_result("toolu_2"),
    ];

    let removed = trim_unanswered_tool_calls(&mut hist);

    assert_eq!(removed, 0, "answers may be spread across later messages");
    assert_eq!(hist.len(), 4);
}

#[test]
fn ignores_tool_calls_on_a_non_assistant_message() {
    // Defensive: only an assistant message can legitimately carry tool_use, so a
    // stray user message with tool_calls must not trigger truncation of real
    // history.
    let mut hist = vec![user("hi"), assistant("hello")];
    let mut odd = user("weird");
    odd.tool_calls = vec![MessageToolCall {
        id: "toolu_x".to_string(),
        name: "shell".to_string(),
        input: serde_json::json!({}),
    }];
    hist.push(odd);

    let removed = trim_unanswered_tool_calls(&mut hist);

    assert_eq!(removed, 0);
    assert_eq!(hist.len(), 3);
}

// ── Boundary conditions ─────────────────────────────────────────────────────

#[test]
fn empty_history_is_a_no_op() {
    let mut hist: Vec<Message> = Vec::new();
    assert_eq!(trim_unanswered_tool_calls(&mut hist), 0);
    assert!(hist.is_empty());
}

#[test]
fn history_that_is_entirely_one_orphan_trims_to_empty() {
    // Killed during the very first iteration. Must not panic, and must leave an
    // empty (not invalid) history.
    let mut hist = vec![assistant_with_calls("starting", &["toolu_1"])];

    let removed = trim_unanswered_tool_calls(&mut hist);

    assert_eq!(removed, 1);
    assert!(hist.is_empty());
}

#[test]
fn an_assistant_with_an_empty_tool_calls_vec_is_not_an_orphan() {
    // `tool_calls: []` serializes away entirely (skip_serializing_if), so this
    // is what every plain text reply looks like on the way back in.
    let mut hist = vec![user("hi"), assistant("no tools needed")];
    assert!(hist[1].tool_calls.is_empty());

    assert_eq!(trim_unanswered_tool_calls(&mut hist), 0);
    assert_eq!(hist.len(), 2);
}

// ── The mid-turn snapshot itself ────────────────────────────────────────────
//
// These assert the FILE contract butler.app depends on: a turn in flight leaves
// a parseable session.json whose status says `running`, and the trimmed shape
// survives the JSON roundtrip that resume actually performs.

#[test]
fn mid_turn_snapshot_shape_is_parseable_and_marked_running() {
    // Mirror what session::save_context_window writes, then read it back the way
    // restore_from_continuation does.
    let snapshot = serde_json::json!({
        "session_id": "butler_dead1",
        "timestamp": 1_760_000_000u64,
        "status": "running",
        "context_window": {
            "used_tokens": 1234,
            "total_tokens": 200_000,
            "percentage_used": 0.6,
            "conversation_history": [
                {"role": "user", "content": "hi"},
                {"role": "assistant", "content": "thinking",
                 "tool_calls": [{"id": "toolu_1", "name": "shell",
                                 "input": {"command": "ls"}}]},
            ]
        }
    });

    let text = serde_json::to_string_pretty(&snapshot).expect("serialize");
    let parsed: serde_json::Value = serde_json::from_str(&text).expect("mid-turn file must parse");

    // The discriminator butler.app needs: not "completed", so a reader that
    // finds this with no live process knows the turn died rather than finished.
    assert_eq!(parsed["status"], "running");

    let hist = parsed["context_window"]["conversation_history"]
        .as_array()
        .expect("history present");
    assert_eq!(hist.len(), 2, "a mid-turn transcript is readable, not empty");

    // Deserialize as the restore path does, then trim: the unanswered call at
    // the tail must be dropped, leaving a resumable conversation.
    let mut restored: Vec<Message> = hist
        .iter()
        .map(|m| serde_json::from_value::<Message>(m.clone()).expect("Message deserializes"))
        .collect();
    assert_eq!(restored[1].tool_calls.len(), 1, "tool_calls survive the roundtrip");

    let removed = trim_unanswered_tool_calls(&mut restored);
    assert_eq!(removed, 1);
    assert_eq!(restored.len(), 1);
    assert_eq!(restored[0].content, "hi");
}

#[test]
fn a_completed_snapshot_needs_no_trimming() {
    // The ordinary end-of-turn file: every call answered, status terminal.
    let mut hist = vec![
        user("hi"),
        assistant_with_calls("looking", &["toolu_1"]),
        tool_result("toolu_1"),
        assistant("done"),
    ];
    let json = serde_json::to_string(&hist).expect("serialize");
    let mut roundtripped: Vec<Message> = serde_json::from_str(&json).expect("deserialize");

    assert_eq!(trim_unanswered_tool_calls(&mut roundtripped), 0);
    assert_eq!(roundtripped.len(), hist.len());
    // And trimming is idempotent on an already-clean history.
    assert_eq!(trim_unanswered_tool_calls(&mut hist), 0);
}
