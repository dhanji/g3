//! Regression test for the bug where `--resume` dropped `tool_calls` and
//! `tool_result_id` from persisted assistant/user messages.
//!
//! Historical bug: `Agent::restore_from_continuation` in `crates/g3-core/src/lib.rs`
//! reconstructed each `Message` by hand from `role`/`content` only, discarding
//! `tool_calls` and `tool_result_id`. Every resume corrupted the transcript
//! (assistant messages lost their tool_use bindings; tool result user messages
//! lost their `tool_result_id`), which compounded across chained resumes.
//!
//! butler.app spawns g3 with `--resume` on every send, so it re-corrupted its
//! own history on every turn, eventually training the model to emit "Let me
//! see:" text without ever calling a tool.
//!
//! The fix: use `serde_json::from_value::<Message>` to deserialize each entry,
//! which preserves all Serialize/Deserialize fields on `Message`.
//!
//! This test verifies the roundtrip works and that legacy entries without the
//! new fields still load with safe defaults.
//!
//! NOTE: We test the deserialization behavior directly against the same shape
//! the restore path uses, rather than constructing a full Agent (heavy).

use g3_providers::{Message, MessageRole, MessageToolCall};

/// Emulate the fixed restore-loop's per-message parse. Kept in one place so
/// the assertions read like the production code path.
fn restore_message_from_json(msg: &serde_json::Value) -> Option<Message> {
    let role_str = msg.get("role").and_then(|r| r.as_str()).unwrap_or("user");
    if role_str == "system" {
        return None;
    }
    match serde_json::from_value::<Message>(msg.clone()) {
        Ok(m) => Some(m),
        Err(_) => {
            let content = msg.get("content").and_then(|c| c.as_str()).unwrap_or("");
            let role = match role_str {
                "assistant" => MessageRole::Assistant,
                _ => MessageRole::User,
            };
            Some(Message {
                role,
                id: String::new(),
                images: Vec::new(),
                content: content.to_string(),
                kind: g3_providers::MessageKind::Regular,
                cache_control: None,
                tool_calls: Vec::new(),
                tool_result_id: None,
            })
        }
    }
}

#[test]
fn tool_calls_survive_resume_roundtrip() {
    // An assistant message with a native tool_use — what a real session.json
    // contains today after the save path (which serializes Message directly).
    let raw = serde_json::json!({
        "role": "assistant",
        "content": "Let me check the file:",
        "tool_calls": [
            {
                "id": "toolu_01ABCDEF",
                "name": "shell",
                "input": { "command": "ls /tmp" }
            }
        ]
    });

    let m = restore_message_from_json(&raw).expect("assistant message should restore");
    assert!(
        matches!(m.role, MessageRole::Assistant),
        "role should be Assistant"
    );
    assert_eq!(
        m.tool_calls.len(),
        1,
        "tool_calls MUST survive resume — this was the bug"
    );
    assert_eq!(m.tool_calls[0].id, "toolu_01ABCDEF");
    assert_eq!(m.tool_calls[0].name, "shell");
    assert!(m.tool_result_id.is_none());
}

#[test]
fn tool_result_id_survives_resume_roundtrip() {
    // Boundary: a user message that is actually a tool_result — it has a
    // `tool_result_id` pointing at the preceding assistant tool_use.
    let raw = serde_json::json!({
        "role": "user",
        "content": "hello streaming world\n",
        "tool_result_id": "toolu_01ABCDEF"
    });

    let m = restore_message_from_json(&raw).expect("tool result user message should restore");
    assert!(matches!(m.role, MessageRole::User));
    assert_eq!(
        m.tool_result_id.as_deref(),
        Some("toolu_01ABCDEF"),
        "tool_result_id MUST survive resume"
    );
}

#[test]
fn legacy_message_without_tool_calls_loads_with_defaults() {
    // Negative: an old session.json entry with just role+content (which is
    // what the fallback hand-parse used to produce) still loads cleanly with
    // empty tool_calls, no crash.
    let raw = serde_json::json!({
        "role": "assistant",
        "content": "just plain text, no tool use here"
    });

    let m = restore_message_from_json(&raw).expect("legacy message should restore");
    assert!(matches!(m.role, MessageRole::Assistant));
    assert_eq!(m.content, "just plain text, no tool use here");
    assert!(m.tool_calls.is_empty(), "legacy message → empty tool_calls");
    assert!(m.tool_result_id.is_none());
}

#[test]
fn malformed_message_falls_back_safely() {
    // Negative: an entry missing required fields (or with wrong shapes) should
    // fall through to the safe defaults, not panic.
    let raw = serde_json::json!({
        "role": "assistant"
        // no content, no tool_calls
    });

    let m = restore_message_from_json(&raw).expect("malformed message should still restore");
    assert!(matches!(m.role, MessageRole::Assistant));
    // Deserialization may succeed (content defaults to "" if serde permits) or
    // fall back to the manual path. Either way: no panic, empty tool_calls.
    assert!(m.tool_calls.is_empty());
}

#[test]
fn system_messages_are_skipped() {
    // System messages should be skipped (they're preserved on the agent).
    let raw = serde_json::json!({
        "role": "system",
        "content": "You are butler..."
    });

    assert!(
        restore_message_from_json(&raw).is_none(),
        "system messages must be skipped"
    );
}

#[test]
fn full_conversation_roundtrip_preserves_tool_use_pair() {
    // Boundary: the smallest realistic corrupted-history shape — an assistant
    // tool_use followed by a user tool_result. This is exactly what butler.app
    // was losing on every resume, producing "text-only assistant" + "orphan
    // tool_result" pairs that poisoned subsequent turns.
    let history = serde_json::json!([
        { "role": "user", "content": "run: echo hi" },
        {
            "role": "assistant",
            "content": "🍌➡️🌍",
            "tool_calls": [
                { "id": "toolu_XYZ", "name": "shell", "input": { "command": "echo hi" } }
            ]
        },
        {
            "role": "user",
            "content": "hi",
            "tool_result_id": "toolu_XYZ"
        }
    ]);

    let messages: Vec<Message> = history
        .as_array()
        .unwrap()
        .iter()
        .filter_map(restore_message_from_json)
        .collect();

    assert_eq!(messages.len(), 3);
    // Assistant tool_calls preserved
    assert_eq!(messages[1].tool_calls.len(), 1);
    assert_eq!(messages[1].tool_calls[0].id, "toolu_XYZ");
    // Tool result id preserved
    assert_eq!(messages[2].tool_result_id.as_deref(), Some("toolu_XYZ"));
}

#[test]
fn message_tool_call_struct_roundtrips_via_serde() {
    // Sanity: MessageToolCall itself round-trips through serde_json cleanly.
    let tc = MessageToolCall {
        id: "toolu_123".to_string(),
        name: "read_file".to_string(),
        input: serde_json::json!({ "file_path": "/tmp/x", "start": 0, "end": 100 }),
    };
    let s = serde_json::to_string(&tc).expect("serialize");
    let back: MessageToolCall = serde_json::from_str(&s).expect("deserialize");
    assert_eq!(back.id, "toolu_123");
    assert_eq!(back.name, "read_file");
    assert_eq!(back.input["file_path"], "/tmp/x");
}
