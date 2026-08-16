//! Mid-stream retry tests — the fix for butler turns dying ~7% of the time
//! (26% on turns with 60+ tool calls).
//!
//! # What was broken
//!
//! `stream_with_retry` retries the act of OPENING a stream. Once bytes are
//! flowing, two branches inside the chunk-processing loop returned `Err`
//! unconditionally:
//!
//!   1. A mid-stream provider error with no tool executed (`lib.rs` ~3217).
//!   2. A stream that finished having produced no text and no tool calls
//!      (`lib.rs` ~3118) — "No response received from the model".
//!
//! Both are transient. Both were fatal. The window is entered once per
//! tool-loop iteration, so turn survival was `(1-p)^N` in the tool count,
//! which is exactly the curve measured across 144 real butler conversations.
//!
//! # What these tests pin
//!
//! - A recoverable mid-stream failure is retried and the turn still completes.
//! - A NON-recoverable mid-stream failure still fails fast (no retry spin).
//! - The budget is finite, and is per-TURN rather than per-iteration.
//! - Retrying does NOT duplicate the user message or any tool result — the
//!   property that makes retrying at this depth safe at all, and the reason
//!   the turn-level retry in `task_execution.rs` was NOT used here.
//!
//! Every test drives the real `Agent::execute_task` path with mock providers.
//! Asserting on `classify_stream_failure` alone would prove the policy is
//! coherent without proving the agent ever consults it.

use g3_core::streaming::{
    classify_stream_failure, StreamFailureAction, MAX_STREAM_RETRIES_PER_TURN,
};
use g3_core::ui_writer::NullUiWriter;
use g3_core::Agent;
use g3_providers::mock::{MockChunk, MockProvider, MockResponse};
use g3_providers::{MessageRole, ProviderRegistry, Usage};

const PROVIDER: &str = "anthropic.default";

/// Retries dialled down so exhausted-budget tests don't sit through backoff.
fn fast_config() -> g3_config::Config {
    let mut config = g3_config::Config::default();
    config.agent.max_retry_attempts = 2;
    config
}

async fn agent_with(provider: MockProvider) -> Agent<NullUiWriter> {
    let mut registry = ProviderRegistry::new();
    registry.register(provider);
    registry.set_default(PROVIDER).unwrap();
    Agent::new_for_test(fast_config(), NullUiWriter, registry)
        .await
        .expect("failed to build agent")
}

fn base_provider() -> MockProvider {
    MockProvider::new()
        .with_name(PROVIDER)
        .with_model("claude-opus-5")
        .with_native_tool_calling(true)
}

fn zero_usage() -> Usage {
    Usage {
        prompt_tokens: 0,
        completion_tokens: 0,
        total_tokens: 0,
        cache_creation_tokens: 0,
        cache_read_tokens: 0,
    }
}

/// A stream that finishes having emitted nothing at all — no text, no tool call.
fn empty_stream() -> MockResponse {
    MockResponse {
        chunks: vec![MockChunk::finished("end_turn")],
        usage: zero_usage(),
    }
}

/// A response that dies partway through, after emitting some text.
fn dies_midway(error: &str) -> MockResponse {
    MockResponse {
        chunks: vec![
            MockChunk::content("I'm starting to answ"),
            MockChunk::stream_error(error),
        ],
        usage: zero_usage(),
    }
}

/// How many user-role messages are in history. The duplication canary: the
/// user's message must appear exactly once no matter how many retries ran.
fn user_message_count(agent: &Agent<NullUiWriter>) -> usize {
    agent
        .get_context_window()
        .conversation_history
        .iter()
        .filter(|m| matches!(m.role, MessageRole::User) && m.tool_result_id.is_none())
        .count()
}

/// All assistant text in history, concatenated.
fn assistant_text(agent: &Agent<NullUiWriter>) -> String {
    agent
        .get_context_window()
        .conversation_history
        .iter()
        .filter(|m| matches!(m.role, MessageRole::Assistant))
        .map(|m| m.content.as_str())
        .collect()
}

// ── The policy function, directly ───────────────────────────────────────────

#[test]
fn a_recoverable_error_with_budget_left_is_retried() {
    let err = anyhow::anyhow!("503 server error");
    assert_eq!(
        classify_stream_failure(&err, 0, 3),
        StreamFailureAction::RetryIteration,
    );
}

#[test]
fn a_non_recoverable_error_is_never_retried_even_with_full_budget() {
    // A malformed request will be malformed on every attempt. Retrying it just
    // multiplies the latency of a failure that was always going to happen.
    let err = anyhow::anyhow!("invalid request: unsupported parameter");
    assert_eq!(
        classify_stream_failure(&err, 0, 3),
        StreamFailureAction::Fail,
    );
}

#[test]
fn an_exhausted_budget_fails_even_a_recoverable_error() {
    let err = anyhow::anyhow!("503 server error");
    assert_eq!(
        classify_stream_failure(&err, 3, 3),
        StreamFailureAction::Fail,
    );
}

#[test]
fn a_context_length_error_is_not_retried_despite_classifying_recoverable() {
    // classify_error() calls this Recoverable, but it is deterministic in the
    // request: the same oversized payload fails identically every time. Burning
    // the budget here would only delay the compaction path that can fix it.
    let err = anyhow::anyhow!("400 bad request: prompt is too long");
    assert_eq!(
        classify_stream_failure(&err, 0, 3),
        StreamFailureAction::Fail,
    );
}

#[test]
fn the_budget_boundary_is_exclusive_so_the_last_retry_is_actually_granted() {
    // Off-by-one guard: with max=3, retries_used=2 must still retry, or the
    // effective budget is 2 while the constant claims 3.
    let err = anyhow::anyhow!("503 server error");
    assert_eq!(
        classify_stream_failure(&err, 2, 3),
        StreamFailureAction::RetryIteration,
    );
}

// ── Through the real agent ──────────────────────────────────────────────────

#[tokio::test]
async fn a_mid_stream_failure_is_retried_and_the_turn_completes() {
    // First attempt dies mid-stream; second succeeds. Before the fix this was
    // a dead turn and a human typing "continue".
    let provider = base_provider()
        .with_response(dies_midway("503 server error"))
        .with_response(MockResponse::text("here is the real answer"));
    let mut agent = agent_with(provider).await;

    let result = agent.execute_task("do the thing", None, false).await;

    assert!(
        result.is_ok(),
        "a transient mid-stream drop must not kill the turn: {:?}",
        result.err(),
    );
    let text = assistant_text(&agent);
    assert!(
        text.contains("here is the real answer"),
        "the retry's response should be what lands in history, got: {text:?}",
    );
}

#[tokio::test]
async fn retrying_does_not_duplicate_the_user_message() {
    // THE reason the retry lives inside the streaming loop instead of wrapping
    // the whole turn: execute_single_task unconditionally re-adds the user
    // message, so a turn-level retry would duplicate it (and re-run every tool
    // call already executed). This is the canary for that regression.
    let provider = base_provider()
        .with_response(dies_midway("503 server error"))
        .with_response(dies_midway("502 server error"))
        .with_response(MockResponse::text("finally"));
    let mut agent = agent_with(provider).await;

    agent
        .execute_task("only say this once", None, false)
        .await
        .expect("turn should survive two transient drops");

    assert_eq!(
        user_message_count(&agent),
        1,
        "the user's message must appear exactly once regardless of retries",
    );
}

#[tokio::test]
async fn a_non_recoverable_mid_stream_error_still_fails_the_turn() {
    // The guard must not have been widened into "retry everything". A provider
    // rejecting the request shape should surface immediately.
    let provider = base_provider()
        .with_response(dies_midway("invalid request: unsupported parameter"))
        .with_response(MockResponse::text("should never be reached"));
    let mut agent = agent_with(provider).await;

    let result = agent.execute_task("do the thing", None, false).await;

    assert!(
        result.is_err(),
        "a non-recoverable mid-stream error must still fail the turn",
    );
}

#[tokio::test]
async fn a_persistently_failing_stream_gives_up_rather_than_looping_forever() {
    // Budget is load-bearing: without it, a provider stuck on 503 would spin
    // until MAX_ITERATIONS (400) or the app's 45-minute wall clock.
    let provider = base_provider().with_default_response(dies_midway("503 server error"));
    // Keep a handle: the mock is cheap to clone and shares its counters, so we
    // can ask how many attempts were actually made.
    let probe = provider.clone();
    let mut agent = agent_with(provider).await;

    let result = agent.execute_task("do the thing", None, false).await;

    assert!(
        result.is_err(),
        "an endlessly failing stream must terminate the turn, not hang",
    );
    // Attempts = the initial try plus the budget. This is the assertion that
    // makes the budget real: a per-ITERATION budget, or no budget at all, would
    // let this climb toward MAX_ITERATIONS instead.
    let attempts = probe.call_count();
    assert_eq!(
        attempts,
        (MAX_STREAM_RETRIES_PER_TURN + 1) as usize,
        "expected 1 initial attempt + {MAX_STREAM_RETRIES_PER_TURN} retries, got {attempts}",
    );
}

#[tokio::test]
async fn an_empty_stream_is_retried_rather_than_killing_the_turn() {
    // The "No response received from the model" path. All 28 recorded butler
    // stream_completion errors arrived here (raw_response: null).
    let provider = base_provider()
        .with_response(empty_stream())
        .with_response(MockResponse::text("recovered after an empty stream"));
    let mut agent = agent_with(provider).await;

    let result = agent.execute_task("do the thing", None, false).await;

    assert!(
        result.is_ok(),
        "an empty stream is the most transient failure there is: {:?}",
        result.err(),
    );
    let text = assistant_text(&agent);
    assert!(
        text.contains("recovered after an empty stream"),
        "got: {text:?}",
    );
}

#[tokio::test]
async fn repeated_empty_streams_still_terminate_with_an_error() {
    let provider = base_provider().with_default_response(empty_stream());
    let mut agent = agent_with(provider).await;

    let result = agent.execute_task("do the thing", None, false).await;

    assert!(
        result.is_err(),
        "endless empty streams must terminate, not hang",
    );
    let msg = result.err().unwrap().to_string();
    assert!(
        msg.contains("No response received"),
        "the original diagnosis should survive the retry wrapper, got: {msg}",
    );
}

#[test]
fn the_per_turn_budget_is_small_enough_to_bound_a_long_turn() {
    // A turn routinely runs 60-180 iterations. If this budget were ever made
    // per-iteration, the worst case would be 180 * N retries — not a budget.
    // Pinning the constant makes that change visible in review.
    assert_eq!(MAX_STREAM_RETRIES_PER_TURN, 3);
}
