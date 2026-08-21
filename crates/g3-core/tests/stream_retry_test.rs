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

// ── Why the retry is NOT at turn level ──────────────────────────────────────

/// CHARACTERIZATION — this test documents a hazard, not a desired behaviour.
///
/// The obvious fix for dying turns was to wrap `agent.execute_task(...)` in
/// `g3_cli::task_execution::execute_task_with_retry`, which already exists and
/// is already wired into the terminal REPL. `agent_mode.rs` (the path
/// butler.app spawns) instead does a bare `?`, so it looked like a one-line
/// omission.
///
/// It is not. `execute_single_task` adds the user message to the context window
/// unconditionally, at the top of every call. So re-invoking it for the same
/// user input appends that input a SECOND time — and on a turn that had already
/// run tool calls, it would also re-run them against the accumulated history.
/// That converts a lost turn into a corrupted transcript, which is strictly
/// worse than the problem being solved.
///
/// Note the duplication does NOT depend on tool count: it happens even when
/// zero tools ran, because the failed attempt's user message is still in
/// history when the retry adds its own. An earlier draft of this fix proposed
/// gating turn-level retry on "no tools executed yet"; this test is what
/// disproved that.
///
/// If someone later wires turn-level retry in anyway, this test fails and
/// points at the reason.
#[tokio::test]
async fn calling_execute_task_twice_duplicates_the_user_message() {
    let provider = base_provider().with_default_response(MockResponse::text("ok"));
    let mut agent = agent_with(provider).await;

    // Exactly what a turn-level retry loop does: same input, second call.
    agent.execute_task("the one user message", None, false).await.unwrap();
    agent.execute_task("the one user message", None, false).await.unwrap();

    assert_eq!(
        user_message_count(&agent),
        2,
        "documents the hazard: re-invoking a turn re-adds the user message, \
         which is why the retry lives inside the streaming loop instead",
    );
}

// ── The silent break after a tool ran (2026-08-21) ──────────────────────────
//
// THE DEFECT. The retry added above lives in the `else` arm of a branch that
// asks "did a tool execute in THIS iteration?":
//
//     if iter.tool_executed {
//         warn!("Stream error after tool execution, attempting to continue");
//         break;                        // <-- no retry, no error, no signal
//     } else {
//         match classify_stream_failure(...) { ... }
//     }
//
// So the protection covers a mid-stream failure only when NO tool ran. When a
// tool HAS run — i.e. every iteration of a working agentic turn — the same
// transient 503 instead breaks out of the stream loop, falls through to
// finalization, and the turn ends. Exit code 0, session stamped "completed",
// nothing in the transcript to say anything went wrong.
//
// That is precisely the corpse signature Dhanji reports as "butler froze":
// the transcript ends on a tool result, the model never speaks again, and
// there is no error anywhere to explain it.
//
// Measured in the local corpus: 0 `context_status` retry markers across 14
// event streams that contained 3 dead turns. The retry was never firing,
// because the deaths were all taking this door.

/// A response that runs a tool, then dies mid-stream on the NEXT chunk.
///
/// The tool call is what routes the failure into the `tool_executed` branch —
/// the whole point. A `dies_midway()` response cannot reach it, which is why
/// the existing tests all passed while this path stayed broken.
fn tool_then_dies(error: &str) -> MockResponse {
    MockResponse {
        chunks: vec![
            MockChunk::tool_streaming("shell"),
            MockChunk::tool_call("shell", serde_json::json!({"command": "echo hi"})),
            MockChunk::stream_error(error),
        ],
        usage: zero_usage(),
    }
}

#[tokio::test]
async fn a_transient_failure_after_a_tool_ran_is_retried_not_silently_dropped() {
    // THE REGRESSION TEST for the silent break.
    //
    // Before the fix the first response's 503 broke out of the loop and the
    // turn finalized as a success with no answer — so `result.is_ok()` was
    // TRUE and only the missing text revealed the loss. Assert on the TEXT,
    // not on ok-ness: a test keying on is_ok() passes against the bug.
    let provider = base_provider()
        .with_response(tool_then_dies("503 server error"))
        .with_response(MockResponse::text("here is the answer after the tool"));
    let mut agent = agent_with(provider).await;

    let result = agent.execute_task("run a tool then answer", None, false).await;

    assert!(
        result.is_ok(),
        "a transient drop after a tool call must not fail the turn: {:?}",
        result.err(),
    );
    let text = assistant_text(&agent);
    assert!(
        text.contains("here is the answer after the tool"),
        "the turn ended without the model's answer — the mid-stream failure \
         after a tool call was swallowed by the silent break. history: {text:?}",
    );
}

#[tokio::test]
async fn a_non_recoverable_failure_after_a_tool_ran_still_fails() {
    // Negative: the fix must not become "retry everything, forever". A
    // malformed-request error after a tool call has to surface, not spin.
    let provider = base_provider()
        .with_response(tool_then_dies("invalid request: unsupported parameter"))
        .with_response(MockResponse::text("should never be reached"));
    let mut agent = agent_with(provider).await;

    let result = agent.execute_task("run a tool then answer", None, false).await;

    assert!(
        result.is_err(),
        "a non-recoverable error after a tool call must still fail the turn",
    );
}

#[tokio::test]
async fn repeated_failures_after_tool_calls_give_up_rather_than_looping() {
    // Boundary: the per-turn budget still bounds this path. Without a shared
    // budget, a provider failing after every tool call would retry forever —
    // an infinite loop is a worse failure than a dead turn, because nothing
    // times out and the chat lock is held the whole time.
    let mut provider = base_provider();
    for _ in 0..(MAX_STREAM_RETRIES_PER_TURN + 3) {
        provider = provider.with_response(tool_then_dies("503 server error"));
    }
    let mut agent = agent_with(provider).await;

    let result = agent.execute_task("run a tool then answer", None, false).await;

    assert!(
        result.is_err(),
        "a persistently failing provider must exhaust the budget and stop",
    );
    assert_eq!(
        user_message_count(&agent),
        1,
        "retries must never duplicate the user message, even on this path",
    );
}

/// BOUNDARY — the cost of bounding the after-a-tool path.
///
/// Before 2026-08-21 that path retried without touching the budget, so it could
/// not exhaust one. Giving it the SHARED per-turn pot bounds an infinite spin,
/// but it also means a long turn with a flaky provider can now die where it
/// previously ground on. That tradeoff was accepted on measurement, and this
/// test pins the measurement so it cannot rot silently.
///
/// Corpus (.g3/butler.app/events, 2026-08-21): 748 tool-call iterations, ZERO
/// mid-stream drops — so the per-iteration drop rate is below 1/748 ≈ 0.0013.
/// With a shared pot of 3, a turn dies only on the 4th drop; at p = 0.0013 even
/// a 233-iteration turn (the longest observed) exhausts it with probability
/// well under 1%.
///
/// If MAX_STREAM_RETRIES_PER_TURN is ever LOWERED, that arithmetic changes and
/// this is the test that should make someone redo it.
#[test]
fn the_shared_budget_still_clears_the_longest_observed_turn() {
    let longest_observed_turn_iterations = 233u32;
    let measured_drop_rate = 1.0 / 748.0;

    // Expected drops across the longest turn we have ever seen.
    let expected_drops = longest_observed_turn_iterations as f64 * measured_drop_rate;

    assert!(
        (MAX_STREAM_RETRIES_PER_TURN as f64) > expected_drops * 3.0,
        "budget of {} leaves too little headroom over the {:.2} drops expected \
         in a {}-iteration turn at the measured rate of {:.4}/iteration",
        MAX_STREAM_RETRIES_PER_TURN,
        expected_drops,
        longest_observed_turn_iterations,
        measured_drop_rate,
    );
}
