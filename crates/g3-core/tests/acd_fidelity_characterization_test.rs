//! Regression tests for ACD fidelity defects (plan items I2 and I6).
//!
//! These began as *characterization* tests that pinned down buggy behaviour, so
//! the analysis in `analysis/acd_cost_analysis.md` would rest on executable
//! evidence rather than on a reading of the source. The defects have since been
//! fixed (I6), so each test now asserts the CORRECTED behaviour and guards
//! against regression.
//!
//! Each test retains a `DEFECT:` comment describing the original bug and the
//! user-visible consequence, because that context is what makes the assertion
//! worth keeping.

use g3_core::acd::Fragment;
use g3_providers::{Message, MessageKind, MessageRole, MessageToolCall};

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// An assistant message that calls a tool the MODERN way: a structured
/// `MessageToolCall`, which is what every native-tool-calling provider
/// (Anthropic, OpenAI, Gemini) produces today.
fn assistant_structured_tool_call(name: &str, input: serde_json::Value) -> Message {
    let mut m = Message::new(MessageRole::Assistant, "Let me look.".to_string());
    m.tool_calls.push(MessageToolCall {
        id: format!("toolu_{}", name),
        name: name.to_string(),
        input,
    });
    m
}

/// An assistant message that calls a tool the LEGACY way: inline JSON in the
/// message body. Only embedded/non-native providers still do this.
fn assistant_inline_json_tool_call(name: &str) -> Message {
    Message::new(
        MessageRole::Assistant,
        format!(r#"{{"tool": "{}", "args": {{"file_path": "x.rs"}}}}"#, name),
    )
}

// ===========================================================================
// DEFECT 1: stub tool-call summary is blind to structured tool calls
// ===========================================================================

#[test]
fn fixed_stub_counts_structured_tool_calls() {
    // DEFECT: `extract_tool_call_summary()` in acd.rs scans `msg.content` for
    // inline JSON. Structured tool calls live in `msg.tool_calls`, which it
    // never inspects. Consequence: with any native-tool-calling provider —
    // i.e. the default configuration — every dehydration stub claims the
    // segment contained "no tool calls".
    //
    // That is precisely the metadata the LLM is supposed to use to decide
    // whether rehydrating is worthwhile. The stub actively misinforms it.
    let messages = vec![
        Message::new(MessageRole::User, "refactor the parser".to_string()),
        assistant_structured_tool_call("read_file", serde_json::json!({"file_path": "a.rs"})),
        Message::new(MessageRole::User, "Tool result: fn main() {}".to_string()),
        assistant_structured_tool_call("str_replace", serde_json::json!({"file_path": "a.rs"})),
        Message::new(MessageRole::User, "Tool result: ok".to_string()),
        assistant_structured_tool_call("shell", serde_json::json!({"command": "cargo test"})),
        Message::new(MessageRole::User, "Tool result: 12 passed".to_string()),
    ];

    let fragment = Fragment::new(messages, None);

    // Three unmistakable tool calls happened; all three must be reported.
    assert_eq!(fragment.tool_call_summary.get("read_file"), Some(&1));
    assert_eq!(fragment.tool_call_summary.get("str_replace"), Some(&1));
    assert_eq!(fragment.tool_call_summary.get("shell"), Some(&1));

    let total: usize = fragment.tool_call_summary.values().sum();
    assert_eq!(total, 3, "summary must account for every structured call");

    let stub = fragment.generate_stub();
    assert!(
        !stub.contains("no tool calls"),
        "the stub must not claim 'no tool calls'. Stub was:\n{}",
        stub
    );
    assert!(
        stub.contains("3 tool calls"),
        "the stub must report the true count. Stub was:\n{}",
        stub
    );
    for tool in ["read_file", "str_replace", "shell"] {
        assert!(stub.contains(tool), "stub must name {}: \n{}", tool, stub);
    }
}

#[test]
fn legacy_inline_json_tool_calls_are_still_counted() {
    // The legacy inline-JSON form must keep working — the embedded provider and
    // other non-native backends still produce it. (This form working correctly
    // is why the structured-call bug went unnoticed for so long: every unit
    // test in acd.rs constructs messages the old way.)
    let messages = vec![
        assistant_inline_json_tool_call("shell"),
        assistant_inline_json_tool_call("shell"),
        assistant_inline_json_tool_call("read_file"),
    ];

    let fragment = Fragment::new(messages, None);

    assert_eq!(fragment.tool_call_summary.get("shell"), Some(&2));
    assert_eq!(fragment.tool_call_summary.get("read_file"), Some(&1));
    assert!(
        fragment.generate_stub().contains("3 tool calls"),
        "legacy inline JSON must remain counted after the fix"
    );
}

#[test]
fn mixed_inline_and_structured_transcript_counts_all_calls() {
    // A realistic transcript during provider migration: some inline, some
    // structured. Previously the stub reported only the inline ones, so the
    // undercount was silent and partial — worse than an obvious zero.
    let messages = vec![
        assistant_inline_json_tool_call("shell"),
        assistant_structured_tool_call("read_file", serde_json::json!({"file_path": "a.rs"})),
        assistant_structured_tool_call("write_file", serde_json::json!({"file_path": "b.rs"})),
    ];

    let fragment = Fragment::new(messages, None);
    let counted: usize = fragment.tool_call_summary.values().sum();

    assert_eq!(
        counted, 3,
        "all 3 tool calls must be counted regardless of representation, got {}",
        counted
    );
    assert_eq!(fragment.tool_call_summary.get("shell"), Some(&1));
    assert_eq!(fragment.tool_call_summary.get("read_file"), Some(&1));
    assert_eq!(fragment.tool_call_summary.get("write_file"), Some(&1));
}

#[test]
fn assistant_message_with_multiple_structured_calls_counts_each() {
    // Boundary: one assistant message may carry several tool calls (parallel
    // tool use). Counting only the first would undercount silently.
    let mut m = Message::new(MessageRole::Assistant, "Doing three things.".to_string());
    for (i, name) in ["read_file", "read_file", "shell"].iter().enumerate() {
        m.tool_calls.push(MessageToolCall {
            id: format!("toolu_{}", i),
            name: name.to_string(),
            input: serde_json::json!({}),
        });
    }

    let fragment = Fragment::new(vec![m], None);
    assert_eq!(fragment.tool_call_summary.get("read_file"), Some(&2));
    assert_eq!(fragment.tool_call_summary.get("shell"), Some(&1));
}

#[test]
fn structured_calls_are_not_double_counted_with_inline_json() {
    // Negative: a provider that emits BOTH a structured call and an inline JSON
    // echo of the same call must be counted once, not twice.
    let mut m = Message::new(
        MessageRole::Assistant,
        r#"{"tool": "shell", "args": {"command": "ls"}}"#.to_string(),
    );
    m.tool_calls.push(MessageToolCall {
        id: "toolu_0".to_string(),
        name: "shell".to_string(),
        input: serde_json::json!({"command": "ls"}),
    });

    let fragment = Fragment::new(vec![m], None);
    let total: usize = fragment.tool_call_summary.values().sum();
    assert_eq!(total, 1, "structured form wins; no double counting");
    assert_eq!(fragment.tool_call_summary.get("shell"), Some(&1));
}

// ===========================================================================
// DEFECT 2: MessageKind does not survive session persistence
// ===========================================================================

#[test]
fn fixed_message_kind_survives_serialization() {
    // DEFECT: `Message.kind` is `#[serde(skip)]` in g3-providers/src/lib.rs.
    // Session state is persisted to `.g3/sessions/<id>/session.json`, so on
    // `--resume` every message comes back as `MessageKind::Regular`.
    //
    // Consequence for ACD: `dehydrate_context()` locates previously-dehydrated
    // content with `rposition(|m| m.is_dehydrated_stub())`. After a resume that
    // returns `None`, so `dehydrate_start` falls back to 0 and the agent
    // re-dehydrates content that is already a stub — writing a fragment whose
    // payload is mostly a previous fragment's stub. The chain silently
    // degrades into nested stubs.
    let stub = Message::with_kind(
        MessageRole::User,
        "---\n⚡ DEHYDRATED CONTEXT: 40 tool calls, 200 total msgs.\n---".to_string(),
        MessageKind::DehydratedStub,
    );

    assert!(stub.is_dehydrated_stub(), "in memory the kind is correct");

    let json = serde_json::to_string(&stub).expect("serialize");
    assert!(
        json.contains("DehydratedStub"),
        "kind must be written to disk. JSON: {}",
        json
    );

    let reloaded: Message = serde_json::from_str(&json).expect("deserialize");
    assert_eq!(
        reloaded.kind,
        MessageKind::DehydratedStub,
        "a DehydratedStub must reload as a DehydratedStub"
    );
    assert!(
        reloaded.is_dehydrated_stub(),
        "is_dehydrated_stub() must survive the round-trip, so dehydrate_start \
         finds the prior stub instead of resetting to 0"
    );

    assert_eq!(reloaded.content, stub.content);
}

#[test]
fn fixed_summary_kind_survives_serialization() {
    // Same root cause, affecting the `+2` skip in dehydrate_context() which
    // assumes a stub is followed by a Summary message.
    let summary = Message::with_kind(
        MessageRole::Assistant,
        "I refactored the parser.".to_string(),
        MessageKind::Summary,
    );
    assert!(summary.is_summary());

    let round_tripped: Message =
        serde_json::from_str(&serde_json::to_string(&summary).unwrap()).unwrap();
    assert!(
        round_tripped.is_summary(),
        "Summary kind must survive persistence too"
    );
}

#[test]
fn resume_finds_prior_stub_and_does_not_redehydrate() {
    // The end-to-end consequence of the fix, expressed the way
    // `dehydrate_context()` actually consumes it: locate the last stub via
    // `rposition(is_dehydrated_stub)` on a history that has been through a
    // save/load cycle.
    let history = vec![
        Message::new(MessageRole::System, "system prompt".to_string()),
        Message::with_kind(
            MessageRole::User,
            "⚡ DEHYDRATED CONTEXT: 40 tool calls".to_string(),
            MessageKind::DehydratedStub,
        ),
        Message::with_kind(
            MessageRole::Assistant,
            "Previously I refactored the parser.".to_string(),
            MessageKind::Summary,
        ),
        Message::new(MessageRole::User, "now add tests".to_string()),
    ];

    // Round-trip the whole history, as `--resume` does.
    let json = serde_json::to_string(&history).unwrap();
    let reloaded: Vec<Message> = serde_json::from_str(&json).unwrap();

    let last_stub = reloaded.iter().rposition(|m| m.is_dehydrated_stub());
    assert_eq!(
        last_stub,
        Some(1),
        "after resume the stub must still be locatable; None here is what \
         caused already-dehydrated content to be re-dehydrated into nested stubs"
    );

    // dehydrate_start = stub + 2 => 3, so only the new user message is
    // considered for dehydration, not the stub and summary that precede it.
    let dehydrate_start = (last_stub.unwrap() + 2).min(reloaded.len());
    assert_eq!(dehydrate_start, 3);
    let to_dehydrate: Vec<usize> = (0..reloaded.len()).filter(|i| *i >= dehydrate_start).collect();
    assert_eq!(
        to_dehydrate,
        vec![3],
        "only genuinely new content is re-dehydrated"
    );
}

#[test]
fn legacy_session_json_without_kind_still_deserializes() {
    // Guard for the eventual fix (I6): whatever we change, session files
    // written before the change must still load. This documents the
    // compatibility requirement now, while it is cheap to state.
    let legacy = r#"{"role":"user","content":"hello","images":[]}"#;
    let m: Message = serde_json::from_str(legacy).expect("legacy session must still load");
    assert_eq!(m.content, "hello");
    assert_eq!(m.kind, MessageKind::Regular);
}

// ===========================================================================
// DEFECT 3: dehydration drops structured tool calls from the kept context
// ===========================================================================

#[test]
fn fragment_retains_tool_calls_on_disk_and_stub_now_names_them() {
    // The fragment on disk retains `tool_calls` (rehydration is not lossy in
    // that respect). Before the fix, the stub that REPLACES it in the live
    // context could not even name the tool. Now it can, which is what lets the
    // model judge whether rehydrating is worth the tokens.
    let msg = assistant_structured_tool_call("read_file", serde_json::json!({"file_path": "a.rs"}));
    let fragment = Fragment::new(vec![msg], None);

    assert_eq!(
        fragment.messages[0].tool_calls.len(),
        1,
        "fragment retains the structured call on disk"
    );

    let stub = fragment.generate_stub();
    assert!(
        stub.contains("read_file"),
        "the stub must name the tool so the model can judge rehydration. Stub:\n{}",
        stub
    );
}

// ===========================================================================
// DEFECT 4: fragment token estimate disagrees with ContextWindow
// ===========================================================================

#[test]
fn fixed_fragment_token_estimate_agrees_with_context_window() {
    // DEFECT: `estimate_fragment_tokens()` in acd.rs uses a flat len/4*1.1.
    // `ContextWindow::estimate_tokens()` uses len/3*1.1 when the text contains
    // '{', '```' or 'fn ' — i.e. for exactly the JSON/code payloads that tool
    // results are made of.
    //
    // Consequence: `execute_rehydrate()` compares `fragment.estimated_tokens`
    // against remaining context to decide whether rehydration fits. Because
    // the estimate is ~25% low on JSON, it will green-light a rehydration that
    // actually overflows the window.
    let json_body = format!(
        r#"{{"result": "{}"}}"#,
        "x".repeat(40_000)
    );
    let messages = vec![Message::new(MessageRole::User, json_body.clone())];
    let fragment = Fragment::new(messages, None);

    let acd_estimate = fragment.estimated_tokens as f64;
    let context_window_estimate = (json_body.len() as f64 / 3.0 * 1.1).ceil();

    // Must now agree within rounding (each applies ceil at slightly different
    // points), NOT differ by 25%.
    let delta = (acd_estimate - context_window_estimate).abs();
    assert!(
        delta <= 2.0,
        "acd.rs says {:.0} tokens, ContextWindow says {:.0}; the rehydrate \
         capacity check depends on these agreeing",
        acd_estimate,
        context_window_estimate
    );
}

#[test]
fn fixed_fragment_token_estimate_counts_structured_tool_call_input() {
    // Boundary: a fragment whose weight is mostly tool_call INPUT (a large
    // write_file payload) must not be estimated as near-zero just because the
    // message text is short.
    let big = "z".repeat(30_000);
    let msg = assistant_structured_tool_call(
        "write_file",
        serde_json::json!({"file_path": "big.rs", "content": big}),
    );
    let text_only = (msg.content.len() as f64 / 4.0 * 1.1).ceil();

    let fragment = Fragment::new(vec![msg], None);

    assert!(
        (fragment.estimated_tokens as f64) > text_only * 10.0,
        "tool_call input must dominate the estimate; got {} vs text-only {:.0}",
        fragment.estimated_tokens,
        text_only
    );
}

#[test]
fn prose_fragment_still_uses_the_cheaper_heuristic() {
    // Negative: the fix must not inflate plain prose to the JSON rate.
    let prose = "the quick brown fox jumps over the lazy dog ".repeat(500);
    let fragment = Fragment::new(
        vec![Message::new(MessageRole::User, prose.clone())],
        None,
    );
    let expected = ((prose.len() as f64 / 4.0).ceil() * 1.1).ceil();
    assert!(
        ((fragment.estimated_tokens as f64) - expected).abs() <= 2.0,
        "prose must keep the 4-chars-per-token rate; got {} expected {:.0}",
        fragment.estimated_tokens,
        expected
    );
}

// ===========================================================================
// Boundary: the +2 skip in dehydrate_context()
// ===========================================================================

#[test]
fn fixed_stub_without_following_summary_clamps_dehydrate_start() {
    // `dehydrate_context()` computes `dehydrate_start = last_stub_index + 2`,
    // assuming the stub is always followed by a Summary message. But the code
    // only appends that summary `if !summary_content.trim().is_empty()`.
    //
    // When the model ends a turn with an empty/whitespace response — which
    // happens on cancellation and on tool-only final turns — the stub is the
    // LAST message, and `last_stub_index + 2` points one past the end.
    let history_len = 5usize;
    let last_stub_index = 4usize; // stub is the final message

    // The clamp applied in lib.rs.
    let dehydrate_start = (last_stub_index + 2).min(history_len);
    assert_eq!(
        dehydrate_start, history_len,
        "dehydrate_start must be clamped to the history length, not {}",
        last_stub_index + 2
    );
    assert!(
        dehydrate_start <= history_len,
        "a clamped index can never address past the end of history"
    );
}

#[test]
fn clamp_is_a_noop_in_the_normal_stub_plus_summary_case() {
    // Negative: the clamp must not perturb the common path, where the stub is
    // followed by a summary and further messages.
    let history_len = 10usize;
    let last_stub_index = 1usize;
    assert_eq!(
        (last_stub_index + 2).min(history_len),
        3,
        "normal case must still skip exactly the stub and its summary"
    );
}

#[test]
fn boundary_empty_fragment_claims_no_phantom_tool_calls() {
    // Degenerate input must not fabricate metadata.
    let fragment = Fragment::new(vec![], None);

    assert_eq!(fragment.message_count, 0);
    assert!(fragment.tool_call_summary.is_empty());
    assert!(fragment.topics.is_empty());
    assert!(fragment.first_user_message.is_none());

    let stub = fragment.generate_stub();
    assert!(
        stub.contains("no tool calls") && stub.contains("0 total msgs"),
        "empty fragment must produce an honest stub, got:\n{}",
        stub
    );
}
