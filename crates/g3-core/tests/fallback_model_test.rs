//! Overload fallback model tests (`--fallback-model`)
//!
//! These drive the REAL retry path (`Agent::execute_task` ->
//! stream_completion_with_tools -> stream_with_retry) with mock providers that
//! inject provider-level errors, because the behaviour under test is entirely
//! about what happens between attempts. Asserting on the registry alone would
//! not prove the agent ever consults it.
//!
//! The contract being protected:
//!   1. A "model overloaded" error switches the turn to the fallback model.
//!   2. The switch lasts EXACTLY one turn — the next turn starts on the default.
//!   3. With no fallback registered, retry behaviour is unchanged.
//!   4. A broken fallback reverts to the default rather than failing the turn.

use g3_core::ui_writer::NullUiWriter;
use g3_core::Agent;
use g3_providers::mock::{MockProvider, MockResponse};
use g3_providers::ProviderRegistry;

const DEFAULT_NAME: &str = "anthropic.default";
const FALLBACK_NAME: &str = "anthropic.default#fallback";
const DEFAULT_MODEL: &str = "claude-opus-5";
const FALLBACK_MODEL: &str = "claude-opus-4-8";

/// Config with retries dialled down so an exhausted-retry test does not sit
/// through exponential backoff.
fn fast_retry_config() -> g3_config::Config {
    let mut config = g3_config::Config::default();
    config.agent.max_retry_attempts = 2;
    config
}

/// Build an agent whose registry holds `default` and, optionally, `fallback`.
async fn agent_with(
    default: MockProvider,
    fallback: Option<MockProvider>,
) -> Agent<NullUiWriter> {
    let mut registry = ProviderRegistry::new();
    registry.register(default);
    if let Some(fallback) = fallback {
        registry.register_fallback(fallback);
    }
    registry.set_default(DEFAULT_NAME).unwrap();

    Agent::new_for_test(fast_retry_config(), NullUiWriter, registry)
        .await
        .expect("failed to build agent")
}

fn default_provider(errors: usize) -> MockProvider {
    let provider = MockProvider::new()
        .with_name(DEFAULT_NAME)
        .with_model(DEFAULT_MODEL)
        .with_native_tool_calling(true)
        .with_default_response(MockResponse::text("answered by the default model"));
    if errors > 0 {
        provider.with_error("Overloaded", errors)
    } else {
        provider
    }
}

fn fallback_provider() -> MockProvider {
    MockProvider::new()
        .with_name(FALLBACK_NAME)
        .with_model(FALLBACK_MODEL)
        .with_native_tool_calling(true)
        .with_default_response(MockResponse::text("answered by the fallback model"))
}

// =============================================================================
// HAPPY PATH: overload engages the fallback, and it serves the turn
// =============================================================================

#[tokio::test]
async fn test_overload_switches_to_fallback_and_completes_turn() {
    let agent = &mut agent_with(default_provider(1), Some(fallback_provider())).await;

    let result = agent.execute_task("hello", None, false).await;
    assert!(result.is_ok(), "turn should complete: {:?}", result.err());

    // The fallback is what actually served the turn.
    assert!(
        agent.provider_registry().is_fallback_active(),
        "fallback must remain active for the rest of the overloaded turn"
    );
    assert_eq!(
        agent.provider_registry().get(None).unwrap().model(),
        FALLBACK_MODEL
    );

    let history = &agent.get_context_window().conversation_history;
    let last = history.last().unwrap();
    assert!(
        last.content.contains("fallback model"),
        "the response should have come from the fallback, got: {}",
        last.content
    );
}

/// The fallback must actually RECEIVE the request — not merely be selected.
#[tokio::test]
async fn test_fallback_provider_receives_the_request() {
    let mut registry = ProviderRegistry::new();
    registry.register(default_provider(1));
    let fallback = fallback_provider();
    registry.register_fallback(fallback);
    registry.set_default(DEFAULT_NAME).unwrap();

    let mut agent = Agent::new_for_test(fast_retry_config(), NullUiWriter, registry)
        .await
        .unwrap();
    agent.execute_task("hello", None, false).await.unwrap();

    let fb = agent.provider_registry().get(Some(FALLBACK_NAME)).unwrap();
    assert_eq!(fb.model(), FALLBACK_MODEL);
    // The turn's answer text is the fallback's, proving it served the request.
    let last = agent
        .get_context_window()
        .conversation_history
        .last()
        .unwrap()
        .content
        .clone();
    assert!(last.contains("fallback model"), "got: {last}");
}

// =============================================================================
// THE CORE REQUIREMENT: one turn only
// =============================================================================

/// Turn 1 overloads and uses the fallback; turn 2 must be back on the default.
#[tokio::test]
async fn test_fallback_reverts_on_the_very_next_turn() {
    // Only the FIRST call fails, so if turn 2 ran on the fallback we would still
    // get a successful result — the assertion must therefore be about WHICH
    // model answered, not merely that the turn succeeded.
    let mut agent = agent_with(default_provider(1), Some(fallback_provider())).await;

    agent.execute_task("first", None, false).await.unwrap();
    assert!(
        agent.provider_registry().is_fallback_active(),
        "turn 1 should have engaged the fallback"
    );

    agent.execute_task("second", None, false).await.unwrap();
    assert!(
        !agent.provider_registry().is_fallback_active(),
        "turn 2 MUST start on the default model — the fallback is a one-turn measure"
    );
    assert_eq!(
        agent.provider_registry().get(None).unwrap().model(),
        DEFAULT_MODEL
    );

    let last = agent
        .get_context_window()
        .conversation_history
        .last()
        .unwrap()
        .content
        .clone();
    assert!(
        last.contains("default model"),
        "turn 2 should be answered by the default model, got: {last}"
    );
}

/// Two overloaded turns in a row each engage the fallback independently — the
/// reset must not be a one-shot that disables the feature after first use.
#[tokio::test]
async fn test_fallback_can_engage_again_on_a_later_turn() {
    // Errors on call 1 (turn 1) and call 2 (turn 2's first attempt).
    let mut agent = agent_with(default_provider(2), Some(fallback_provider())).await;

    agent.execute_task("first", None, false).await.unwrap();
    assert!(agent.provider_registry().is_fallback_active());

    agent.execute_task("second", None, false).await.unwrap();
    assert!(
        agent.provider_registry().is_fallback_active(),
        "a second overloaded turn must be able to engage the fallback again"
    );
    let last = agent
        .get_context_window()
        .conversation_history
        .last()
        .unwrap()
        .content
        .clone();
    assert!(last.contains("fallback model"), "got: {last}");
}

// =============================================================================
// NEGATIVE: a broken fallback must not make things worse
// =============================================================================

/// A typo'd fallback model (404 => non-recoverable) must revert to the default
/// and let the turn finish. Otherwise one bad flag value turns every transient
/// overload into a hard failure — worse than not having the flag.
#[tokio::test]
async fn test_broken_fallback_reverts_to_default_and_turn_succeeds() {
    let broken_fallback = MockProvider::new()
        .with_name(FALLBACK_NAME)
        .with_model("claude-does-not-exist")
        .with_native_tool_calling(true)
        .always_failing("404 not_found_error: model not found");

    // Default fails once (overload), then works.
    let mut agent = agent_with(default_provider(1), Some(broken_fallback)).await;

    let result = agent.execute_task("hello", None, false).await;
    assert!(
        result.is_ok(),
        "a broken fallback must not fail the turn: {:?}",
        result.err()
    );
    assert!(
        !agent.provider_registry().is_fallback_active(),
        "must have reverted off the broken fallback"
    );

    let last = agent
        .get_context_window()
        .conversation_history
        .last()
        .unwrap()
        .content
        .clone();
    assert!(
        last.contains("default model"),
        "the default model should have served the turn after reverting, got: {last}"
    );
}

/// If BOTH models are down, the turn fails as it would have without the feature
/// — the fallback adds a chance, it does not add a guarantee.
#[tokio::test]
async fn test_both_models_failing_still_errors() {
    let dead_fallback = MockProvider::new()
        .with_name(FALLBACK_NAME)
        .with_model(FALLBACK_MODEL)
        .with_native_tool_calling(true)
        .always_failing("Overloaded");

    let dead_default = MockProvider::new()
        .with_name(DEFAULT_NAME)
        .with_model(DEFAULT_MODEL)
        .with_native_tool_calling(true)
        .always_failing("Overloaded");

    let mut agent = agent_with(dead_default, Some(dead_fallback)).await;
    let result = agent.execute_task("hello", None, false).await;

    assert!(
        result.is_err(),
        "with every model overloaded the turn must still fail rather than hang"
    );
}

// =============================================================================
// BOUNDARY: feature off, and non-overload errors
// =============================================================================

/// With no fallback registered the retry path must behave exactly as before:
/// an overload is retried against the same model and nothing is "switched".
#[tokio::test]
async fn test_no_fallback_registered_retries_normally() {
    // Fails once, then succeeds — the plain retry should absorb it.
    let mut agent = agent_with(default_provider(1), None).await;

    let result = agent.execute_task("hello", None, false).await;
    assert!(result.is_ok(), "plain retry should succeed: {:?}", result.err());
    assert!(!agent.provider_registry().has_fallback());
    assert!(!agent.provider_registry().is_fallback_active());

    let last = agent
        .get_context_window()
        .conversation_history
        .last()
        .unwrap()
        .content
        .clone();
    assert!(last.contains("default model"), "got: {last}");
}

/// Feature off and the model stays down: the turn fails after the normal retry
/// budget, proving the fallback code did not silently grant extra attempts to
/// everyone.
#[tokio::test]
async fn test_no_fallback_exhausts_retries_and_fails() {
    let dead = MockProvider::new()
        .with_name(DEFAULT_NAME)
        .with_model(DEFAULT_MODEL)
        .with_native_tool_calling(true)
        .always_failing("Overloaded");

    let mut agent = agent_with(dead, None).await;
    let result = agent.execute_task("hello", None, false).await;

    assert!(result.is_err(), "should fail once retries are exhausted");
}

/// A non-overload recoverable error (rate limit) must NOT engage the fallback.
/// A 429 means "you are sending too much", which follows the API key to the
/// fallback — switching models would not help and would burn the switch.
#[tokio::test]
async fn test_rate_limit_does_not_engage_fallback() {
    let rate_limited = MockProvider::new()
        .with_name(DEFAULT_NAME)
        .with_model(DEFAULT_MODEL)
        .with_native_tool_calling(true)
        .with_default_response(MockResponse::text("answered by the default model"))
        .with_error("429 rate limit exceeded", 1);

    let mut agent = agent_with(rate_limited, Some(fallback_provider())).await;
    let result = agent.execute_task("hello", None, false).await;

    assert!(result.is_ok(), "rate limit should be retried: {:?}", result.err());
    assert!(
        !agent.provider_registry().is_fallback_active(),
        "a rate limit is not an overload — the fallback must stay parked"
    );
    let last = agent
        .get_context_window()
        .conversation_history
        .last()
        .unwrap()
        .content
        .clone();
    assert!(last.contains("default model"), "got: {last}");
}

/// A non-recoverable error on the DEFAULT model (fallback never engaged) must
/// still fail immediately — the revert logic is guarded on the fallback being
/// active, and must not accidentally grant a retry to plain bad requests.
#[tokio::test]
async fn test_non_recoverable_on_default_fails_without_engaging_fallback() {
    let bad_request = MockProvider::new()
        .with_name(DEFAULT_NAME)
        .with_model(DEFAULT_MODEL)
        .with_native_tool_calling(true)
        .always_failing("invalid_request_error: something is malformed");

    let mut agent = agent_with(bad_request, Some(fallback_provider())).await;
    let result = agent.execute_task("hello", None, false).await;

    assert!(result.is_err(), "a malformed request must fail fast");
    assert!(
        !agent.provider_registry().is_fallback_active(),
        "a non-recoverable error is not an overload; the fallback must not engage"
    );
}
