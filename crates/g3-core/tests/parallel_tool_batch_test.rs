//! Concurrent execution of a same-response tool batch (2026-11).
//!
//! CHARACTERIZATION: when the model emitted N independent tool calls in one
//! response, g3 ran them one after another (and the Anthropic provider even
//! delivered them one at a time, mid-stream). Measured over 400 butler
//! sessions: 0.96 tool calls per model round trip.
//!
//! What this protects:
//! - A batch of concurrency-safe calls runs in parallel (wall clock ~ the
//!   slowest call, not the sum).
//! - Results are recorded in the model's ORIGINAL order, each answering its
//!   own tool_use id — the Anthropic API rejects anything else.
//! - A batch containing any barrier tool (write_file here) runs strictly
//!   sequentially, so a write is never reordered against a read.
//! - One failing call does not drop its siblings' results.

use g3_core::ui_writer::NullUiWriter;
use g3_core::Agent;
use g3_providers::mock::{MockChunk, MockProvider, MockResponse};
use g3_providers::{MessageRole, ProviderRegistry, Usage};
use std::time::Instant;

const PROVIDER: &str = "anthropic.default";

fn usage() -> Usage {
    Usage { prompt_tokens: 100, completion_tokens: 50, total_tokens: 150, cache_creation_tokens: 0, cache_read_tokens: 0 }
}

/// One response carrying several native tool calls, the way Anthropic
/// delivers a batch: all calls on the final chunk.
fn batch(calls: Vec<(&str, serde_json::Value)>) -> MockResponse {
    let mut fin = MockChunk::finished("tool_use");
    fin.tool_calls = Some(
        calls
            .into_iter()
            .enumerate()
            .map(|(i, (t, a))| g3_providers::ToolCall { id: format!("toolu_batch_{i}_{}", t), tool: t.into(), args: a })
            .collect(),
    );
    MockResponse::custom(vec![fin], usage())
}

async fn run(responses: Vec<MockResponse>) -> (Agent<NullUiWriter>, std::time::Duration) {
    let mut p = MockProvider::new().with_name(PROVIDER).with_model("claude-opus-5").with_native_tool_calling(true);
    for r in responses {
        p = p.with_response(r);
    }
    p = p.with_response(MockResponse::text("done"));
    let mut registry = ProviderRegistry::new();
    registry.register(p);
    registry.set_default(PROVIDER).unwrap();
    let mut agent = Agent::new_for_test(g3_config::Config::default(), NullUiWriter, registry).await.unwrap();
    let start = Instant::now();
    agent.execute_task("go", None, false).await.expect("turn ok");
    (agent, start.elapsed())
}

/// (tool_use id, tool name) for every assistant tool call, and the result
/// text for every tool_result, in history order.
fn transcript(agent: &Agent<NullUiWriter>) -> (Vec<String>, Vec<(String, String)>) {
    let h = &agent.get_context_window().conversation_history;
    let uses = h
        .iter()
        .filter(|m| matches!(m.role, MessageRole::Assistant))
        .flat_map(|m| m.tool_calls.iter().map(|tc| tc.id.clone()))
        .collect();
    let results = h
        .iter()
        .filter_map(|m| m.tool_result_id.clone().map(|id| (id, m.content.clone())))
        .collect();
    (uses, results)
}

#[tokio::test]
async fn independent_shell_calls_run_concurrently_and_stay_in_order() {
    let calls = (0..3)
        .map(|i| ("shell", serde_json::json!({ "command": format!("sleep 1; echo result-{i}") })))
        .collect();
    let (agent, took) = run(vec![batch(calls)]).await;

    // Sequential would be >= 3s. Generous ceiling for a loaded CI box.
    assert!(took.as_secs_f64() < 2.5, "3 x sleep 1 took {:.2}s — batch ran sequentially", took.as_secs_f64());

    let (uses, results) = transcript(&agent);
    assert_eq!(uses.len(), 3);
    assert_eq!(results.len(), 3);
    for (i, (id, content)) in results.iter().enumerate() {
        assert_eq!(id, &uses[i], "result {i} answers the wrong tool_use");
        assert!(content.contains(&format!("result-{i}")), "result {i} out of order: {content}");
    }
}

#[tokio::test]
async fn batch_with_a_write_runs_sequentially_in_order() {
    let dir = tempfile::tempdir().unwrap();
    let f = dir.path().join("x.txt");
    let fs = f.to_str().unwrap();
    // read-before-write, write, read-after-write: only sequential execution
    // in order gives "missing" then "hello".
    let calls = vec![
        ("shell", serde_json::json!({ "command": format!("sleep 0.5; cat {fs} 2>/dev/null || echo missing") })),
        ("write_file", serde_json::json!({ "file_path": fs, "content": "hello" })),
        ("shell", serde_json::json!({ "command": format!("cat {fs}") })),
    ];
    let (agent, _) = run(vec![batch(calls)]).await;
    let (_, results) = transcript(&agent);
    assert_eq!(results.len(), 3);
    assert!(results[0].1.contains("missing"), "read ran after the write: {}", results[0].1);
    assert!(results[2].1.contains("hello"), "read-after-write didn't see the write: {}", results[2].1);
}

#[tokio::test]
async fn a_failing_call_does_not_drop_its_siblings() {
    let calls = vec![
        ("shell", serde_json::json!({ "command": "echo first" })),
        ("read_file", serde_json::json!({ "file_path": "/definitely/not/here.txt" })),
        ("shell", serde_json::json!({ "command": "echo third" })),
    ];
    let (agent, _) = run(vec![batch(calls)]).await;
    let (uses, results) = transcript(&agent);
    assert_eq!(results.len(), 3, "every tool_use must get a tool_result");
    assert_eq!(results.iter().map(|r| &r.0).collect::<Vec<_>>(), uses.iter().collect::<Vec<_>>());
    assert!(results[0].1.contains("first"));
    assert!(results[2].1.contains("third"));
}

#[tokio::test]
async fn thinking_blocks_are_attached_to_the_tool_message_for_replay() {
    let mut chunks = vec![MockChunk::thinking("pick codeword 482917", "sig-abc")];
    chunks.push(MockChunk::tool_call("shell", serde_json::json!({ "command": "echo hi" })));
    chunks.push(MockChunk::finished("tool_use"));
    let (agent, _) = run(vec![MockResponse::custom(chunks, usage())]).await;
    let h = &agent.get_context_window().conversation_history;
    let tool_msg = h
        .iter()
        .find(|m| matches!(m.role, MessageRole::Assistant) && !m.tool_calls.is_empty())
        .expect("assistant tool message");
    assert_eq!(tool_msg.thinking_blocks.len(), 1, "thinking block lost before replay");
    assert_eq!(tool_msg.thinking_blocks[0]["signature"], "sig-abc");
}

/// Live-found 2026-11-02: two IDENTICAL calls in one response (distinct ids)
/// were collapsed by "DUP IN CHUNK", leaving the second tool_use unanswered.
#[tokio::test]
async fn identical_calls_with_distinct_ids_both_execute() {
    let cmd = serde_json::json!({ "command": "echo same" });
    let (agent, _) = run(vec![batch(vec![("shell", cmd.clone()), ("shell", cmd)])]).await;
    let (uses, results) = transcript(&agent);
    assert_eq!(uses.len(), 2, "second identical call was dropped as a duplicate");
    assert_eq!(results.len(), 2, "every tool_use needs its tool_result");
}
