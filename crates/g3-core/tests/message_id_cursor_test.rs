//! Tests for persisted `Message.id` and its use as a resume cursor.
//!
//! WHY THIS EXISTS
//! ---------------
//! `Message.id` used to be `#[serde(skip)]`: identity existed in memory and was
//! discarded at the persistence boundary, so every reload produced `""`.
//!
//! Consumers that needed to express "resume after this message" were therefore
//! forced to use an ARRAY INDEX. That is not stable. Context management rewrites
//! `conversation_history` underneath such an index:
//!
//!   * thinning     — mutates message CONTENT in place (indices survive)
//!   * compaction   — CLEARS and rebuilds, usually much shorter (indices do not)
//!
//! A stored index of 101 against a history compacted down to 14 entries silently
//! means something else, or nothing. In butler.app that manifested as a turn's
//! own output leaking into the "what you already have" baseline and then being
//! replayed on top of itself — a duplicate that only appeared if compaction
//! happened to fire mid-turn.
//!
//! These tests pin the properties a cursor actually needs.

use g3_core::ContextWindow;
use g3_providers::{Message, MessageRole};

fn ctx_with(n: usize) -> ContextWindow {
    let mut c = ContextWindow::new(100_000);
    c.add_message(Message::new(MessageRole::System, "sys".to_string()));
    for i in 0..n {
        let role = if i % 2 == 0 { MessageRole::User } else { MessageRole::Assistant };
        c.add_message(Message::new(role, format!("message number {}", i)));
    }
    c
}

// ── Persistence ─────────────────────────────────────────────────────────────

#[test]
fn id_survives_a_serde_round_trip() {
    let c = ctx_with(3);
    let ids: Vec<String> = c.conversation_history.iter().map(|m| m.id.clone()).collect();
    assert!(ids.iter().all(|i| !i.is_empty()), "fresh messages must have ids");

    let json = serde_json::to_string(&c).unwrap();
    let back: ContextWindow = serde_json::from_str(&json).unwrap();
    let ids_back: Vec<String> = back.conversation_history.iter().map(|m| m.id.clone()).collect();

    assert_eq!(ids, ids_back, "ids must be identical after save→load");
}

#[test]
fn ids_are_unique_across_a_realistic_history() {
    // A tool loop appends many messages inside the same clock second, so
    // uniqueness must not depend on the timestamp component.
    let c = ctx_with(500);
    let mut ids: Vec<String> = c.conversation_history.iter().map(|m| m.id.clone()).collect();
    let total = ids.len();
    ids.sort();
    ids.dedup();
    assert_eq!(ids.len(), total, "duplicate ids would make a cursor ambiguous");
}

// ── Legacy hydration ────────────────────────────────────────────────────────

#[test]
fn legacy_history_without_ids_loads_and_hydrates() {
    // Exactly the shape written before ids were persisted: no `id` key at all.
    let json = r#"{
        "used_tokens": 0,
        "total_tokens": 100000,
        "cumulative_tokens": 0,
        "conversation_history": [
            {"role": "system", "content": "sys"},
            {"role": "user", "content": "hello"},
            {"role": "assistant", "content": "hi"}
        ],
        "last_thinning_percentage": 0
    }"#;
    let mut c: ContextWindow = serde_json::from_str(json).expect("legacy session must still load");
    assert!(c.conversation_history.iter().all(|m| m.id.is_empty()));

    let assigned = c.hydrate_message_ids();
    assert_eq!(assigned, 3, "every id-less message should be backfilled");
    assert!(c.conversation_history.iter().all(|m| !m.id.is_empty()));
}

#[test]
fn hydration_is_idempotent_and_preserves_existing_ids() {
    // Called on every load, so it must never churn ids that already exist —
    // a cursor handed out before a reload has to keep resolving afterwards.
    let mut c = ctx_with(4);
    let before: Vec<String> = c.conversation_history.iter().map(|m| m.id.clone()).collect();

    assert_eq!(c.hydrate_message_ids(), 0, "nothing to assign");
    assert_eq!(c.hydrate_message_ids(), 0, "still nothing on a second call");

    let after: Vec<String> = c.conversation_history.iter().map(|m| m.id.clone()).collect();
    assert_eq!(before, after, "existing ids must not be regenerated");
}

#[test]
fn hydration_fills_only_the_gaps_in_a_partially_migrated_history() {
    // A session resumed mid-migration: some messages have ids, some don't.
    let mut c = ctx_with(3);
    let kept = c.conversation_history[2].id.clone();
    c.conversation_history[1].id = String::new();
    c.conversation_history[3].id = String::new();

    assert_eq!(c.hydrate_message_ids(), 2);
    assert_eq!(c.conversation_history[2].id, kept, "untouched id must be stable");
    assert!(c.conversation_history.iter().all(|m| !m.id.is_empty()));
}

// ── The cursor primitive ────────────────────────────────────────────────────

#[test]
fn index_of_message_id_finds_the_position() {
    let c = ctx_with(5);
    let target = c.conversation_history[3].id.clone();
    assert_eq!(c.index_of_message_id(&target), Some(3));
}

#[test]
fn index_of_unknown_or_empty_id_is_none_not_a_panic() {
    let c = ctx_with(2);
    assert_eq!(c.index_of_message_id("nope-zzzzzz"), None);
    // Empty must never match an un-hydrated message and silently resolve to 0 —
    // that would resume from the top of the conversation.
    assert_eq!(c.index_of_message_id(""), None);
}

#[test]
fn empty_id_does_not_match_even_when_history_has_empty_ids() {
    let mut c = ctx_with(3);
    c.conversation_history[1].id = String::new();
    assert_eq!(
        c.index_of_message_id(""),
        None,
        "an empty cursor must not resolve to the first un-hydrated message"
    );
}

#[test]
fn last_message_id_reports_the_tail() {
    let c = ctx_with(4);
    let expected = c.conversation_history.last().unwrap().id.clone();
    assert_eq!(c.last_message_id(), Some(expected));
}

#[test]
fn last_message_id_is_none_on_empty_history() {
    let c = ContextWindow::new(1000);
    assert_eq!(c.last_message_id(), None);
}

// ── THE POINT: stability under context management ────────────────────────────

#[test]
fn thinning_preserves_ids_so_a_cursor_still_resolves() {
    // Thinning rewrites message CONTENT in place. Ids — and therefore cursors —
    // must be unaffected, even though byte offsets and lengths all change.
    let mut c = ContextWindow::new(100_000);
    c.add_message(Message::new(MessageRole::System, "sys".to_string()));
    for i in 0..30 {
        let role = if i % 2 == 0 { MessageRole::User } else { MessageRole::Assistant };
        c.add_message(Message::new(role, format!("Tool result: {}", "x".repeat(4000))));
        let _ = role;
    }
    let cursor = c.conversation_history[5].id.clone();
    let before = c.index_of_message_id(&cursor);

    c.thin_context(None);

    assert_eq!(
        c.index_of_message_id(&cursor),
        before,
        "thinning must not disturb ids"
    );
}

#[test]
fn compaction_drops_ids_it_deletes_and_says_so_rather_than_lying() {
    // THE CASE THE COUNT-BASED CURSOR GOT WRONG.
    //
    // Summarizing compaction genuinely DELETES messages. A cursor pointing at
    // one of them must resolve to None — an honest "resync, I can't place you" —
    // whereas a stored INDEX would still be a valid-looking number pointing at
    // unrelated content, or past the end.
    let mut c = ctx_with(40);
    let doomed = c.conversation_history[10].id.clone();
    let len_before = c.conversation_history.len();
    assert_eq!(c.index_of_message_id(&doomed), Some(10));

    c.reset_with_summary("a summary of everything".to_string(), Some("latest".to_string()));

    assert!(
        c.conversation_history.len() < len_before,
        "compaction should shrink history (was {}, now {})",
        len_before,
        c.conversation_history.len()
    );
    assert_eq!(
        c.index_of_message_id(&doomed),
        None,
        "a summarized-away id must report as unknown, not as a stale index"
    );

    // And an index taken before compaction is now meaningless — which is exactly
    // why the cursor is an id. Demonstrate the hazard concretely.
    assert!(
        10 >= c.conversation_history.len() || c.conversation_history[10].id != doomed,
        "the old index no longer identifies the same message"
    );
}

#[test]
fn compaction_preserves_the_id_of_messages_it_keeps() {
    // The preserved system prompt / project context / last assistant message are
    // cloned, so their identity must survive. That is what lets a cursor pointing
    // at a SURVIVING message keep working across a compaction.
    let mut c = ctx_with(20);
    let sys_id = c.conversation_history[0].id.clone();
    let last_assistant_id = c
        .conversation_history
        .iter()
        .rev()
        .find(|m| matches!(m.role, MessageRole::Assistant))
        .unwrap()
        .id
        .clone();

    c.reset_with_summary("summary".to_string(), None);

    assert_eq!(
        c.index_of_message_id(&sys_id),
        Some(0),
        "system prompt keeps its id across compaction"
    );
    assert!(
        c.index_of_message_id(&last_assistant_id).is_some(),
        "the preserved last assistant message keeps its id"
    );
}

#[test]
fn ids_remain_unique_after_compaction_adds_new_messages() {
    // Compaction mints new messages (the summary, and a re-created latest user
    // message). Those must not collide with the ids it preserved.
    let mut c = ctx_with(20);
    c.reset_with_summary("summary".to_string(), Some("latest user".to_string()));

    let mut ids: Vec<String> = c.conversation_history.iter().map(|m| m.id.clone()).collect();
    assert!(ids.iter().all(|i| !i.is_empty()), "no message should lack an id");
    let total = ids.len();
    ids.sort();
    ids.dedup();
    assert_eq!(ids.len(), total, "compaction introduced a duplicate id");
}
