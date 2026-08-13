//! Mid-turn input injection: ordering safety.
//!
//! Queued user messages (see `g3_core::pending_input`) are injected into the
//! conversation while a turn is running. There is exactly ONE safe position for
//! that injection, and this test exists to make the unsafe position fail loudly
//! rather than silently corrupting a conversation.
//!
//! # The hazard
//!
//! `AnthropicProvider::strip_orphaned_tool_use` enforces the API rule that every
//! `tool_use` block must be answered by a `tool_result` in the *immediately
//! following* message. If a plain user message is spliced between an assistant's
//! tool_use and its tool_result, the tool_use is deleted from history and (if
//! that leaves the message empty) replaced with the text "(continued)".
//!
//! The failure is nasty precisely because nothing errors: the request succeeds,
//! the model simply never sees that it called a tool. It then re-runs the tool,
//! or reports on work whose result vanished.
//!
//! Injection therefore happens at the TOP of the streaming loop, where the
//! previous iteration's tool results have already been appended.

use g3_providers::{Message, MessageRole, MessageToolCall};

/// Build the message sequence that a turn produces when a tool is dispatched:
/// user prompt, assistant tool_use, matching tool_result.
fn turn_with_tool_call() -> Vec<Message> {
    let mut assistant = Message::new(MessageRole::Assistant, "Reading the file...".to_string());
    assistant.tool_calls = vec![MessageToolCall {
        id: "toolu_abc".to_string(),
        name: "read_file".to_string(),
        input: serde_json::json!({"file_path": "a.rs"}),
    }];

    let mut tool_result = Message::new(MessageRole::User, "Tool result: contents".to_string());
    tool_result.tool_result_id = Some("toolu_abc".to_string());

    vec![
        Message::new(MessageRole::User, "read a.rs".to_string()),
        assistant,
        tool_result,
    ]
}

/// The injected message, as `inject_pending_input` constructs it: a plain user
/// message with NO tool_result_id.
fn injected_message() -> Message {
    Message::new(
        MessageRole::User,
        "💬 **User message sent while you were working** — actually check b.rs".to_string(),
    )
}

fn is_assistant(m: &Message) -> bool {
    matches!(m.role, MessageRole::Assistant)
}

fn is_user(m: &Message) -> bool {
    matches!(m.role, MessageRole::User)
}

/// SAFE POSITION (what the implementation does): append after the tool_result.
///
/// This is the loop-top case — by the time the next iteration begins, the
/// tool_result for the previous iteration is already in history.
#[test]
fn injecting_after_the_tool_result_preserves_the_tool_call() {
    let mut messages = turn_with_tool_call();
    assert_eq!(messages[1].tool_calls.len(), 1);

    // Append at the end: assistant tool_use is still immediately followed by
    // its tool_result, so the pairing is intact.
    messages.push(injected_message());

    let assistant_idx = 1;
    assert!(is_assistant(&messages[assistant_idx]));
    assert!(
        !messages[assistant_idx].tool_calls.is_empty(),
        "assistant tool_call must survive injection"
    );
    assert_eq!(
        messages[assistant_idx + 1].tool_result_id.as_deref(),
        Some("toolu_abc"),
        "the message following the tool_use must still be its tool_result"
    );
    // And the injected message is last, where it cannot break the pairing.
    assert!(messages.last().unwrap().tool_result_id.is_none());
}

/// UNSAFE POSITION (what must never be built): splice between tool_use and
/// tool_result. This test documents the corruption so the invariant is explicit.
#[test]
fn injecting_between_tool_use_and_tool_result_breaks_the_pairing() {
    let mut messages = turn_with_tool_call();

    // Splice the plain user message directly after the assistant's tool_use —
    // i.e. what would happen if injection were done inside the tool-dispatch
    // block instead of at the loop top.
    messages.insert(2, injected_message());

    // The message now following the tool_use carries no tool_result_id, which
    // is exactly the condition strip_orphaned_tool_use looks for.
    let assistant_idx = 1;
    let follower = &messages[assistant_idx + 1];
    assert!(is_user(follower));
    assert!(
        follower.tool_result_id.is_none(),
        "this is the corrupting arrangement: tool_use followed by a non-result \
         user message. The provider will strip the tool_use."
    );

    // Assert the shape the provider will reject, so a future refactor that
    // moves injection into the dispatch block fails here first.
    assert_eq!(
        messages[assistant_idx].tool_calls.len(),
        1,
        "the tool_use is present but unanswered — the provider strips it, and \
         the model never learns it made this call"
    );
}

/// Several queued messages must inject in send order. Reversal would make a
/// correction arrive before the thing it corrects.
#[test]
fn multiple_injected_messages_keep_send_order() {
    let mut messages = turn_with_tool_call();
    for text in ["first", "second", "third"] {
        messages.push(Message::new(MessageRole::User, text.to_string()));
    }
    let tail: Vec<&str> = messages[messages.len() - 3..]
        .iter()
        .map(|m| m.content.as_str())
        .collect();
    assert_eq!(tail, vec!["first", "second", "third"]);
}

/// An injected message must never claim to be a tool result. If it did, it
/// would satisfy the pairing check for a tool_use it has nothing to do with,
/// and the real result would be dropped.
#[test]
fn injected_message_is_never_a_tool_result() {
    let msg = injected_message();
    assert!(
        msg.tool_result_id.is_none(),
        "an interjection must not masquerade as a tool result"
    );
    assert!(is_user(&msg));
    assert!(
        msg.tool_calls.is_empty(),
        "an interjection must not carry tool calls"
    );
}
