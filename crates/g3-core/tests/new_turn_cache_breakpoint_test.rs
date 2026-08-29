//! Regression test for the 2026-08-29 "Found 9" cache_control overflow bug.
//!
//! CHARACTERIZATION: every NEW USER TURN (a chat send, a "continue", a plain
//! reply — anything that goes through `execute_single_task`) stamped a fresh
//! `cache_control` marker on the user message whenever the provider supports
//! caching, with NO check against the rolling budget and no slide-forward.
//! The tool-execution loop already had this discipline
//! (`clear_rolling_cache_breakpoints()` / `max_rolling_cache_breakpoints()`),
//! but the new-turn path did not, so a chat sent repeatedly (a few
//! clarifying replies, a couple of "continue"s) accumulated one rolling
//! breakpoint per send. Once the system-block reservation (1 slot) plus the
//! accumulated rolling breakpoints exceeded Anthropic's hard cap of 4,
//! every subsequent request 400'd:
//!
//!   "A maximum of 4 blocks with cache_control may be provided. Found 9."
//!
//! Confirmed live 2026-08-29 against session butler_ed85ce50451a4380, which
//! sent 8 user turns and was left permanently poisoned (the excess markers
//! are baked into session.json and replay on every resume).
//!
//! What this test protects:
//! - Repeated new-turn sends never accumulate more than
//!   `max_rolling_cache_breakpoints()` rolling breakpoints in history.
//! - The count strictly does not grow past the budget no matter how many
//!   turns are sent (boundary: many turns, not just enough to trip the old bug).
//!
//! What this test intentionally does NOT assert:
//! - The exact tool-loop breakpoint-sliding cadence (covered by
//!   `cache_breakpoint_budget_tests` in g3-core/src/lib.rs).
//! - Anthropic's actual HTTP behavior (out of scope for a unit-level agent test).

use g3_core::ui_writer::NullUiWriter;
use g3_core::Agent;
use g3_providers::mock::{MockChunk, MockProvider, MockResponse};
use g3_providers::{ProviderRegistry, Usage};

fn text_response(text: &str) -> MockResponse {
    MockResponse::custom(
        vec![MockChunk::content(text), MockChunk::finished("end_turn")],
        Usage {
            prompt_tokens: 100,
            completion_tokens: 20,
            total_tokens: 120,
            cache_creation_tokens: 0,
            cache_read_tokens: 0,
        },
    )
}

async fn agent_with_cache_enabled(num_responses: usize) -> Agent<NullUiWriter> {
    let mut provider = MockProvider::new()
        .with_name("anthropic.default")
        .with_cache_control_support(true);
    for i in 0..num_responses {
        provider = provider.with_response(text_response(&format!("reply {i}")));
    }

    let mut registry = ProviderRegistry::new();
    registry.register(provider);

    let mut config = g3_config::Config::default();
    config.providers.default_provider = "anthropic.default".to_string();
    config.providers.anthropic.insert(
        "default".to_string(),
        g3_config::AnthropicConfig {
            api_key: "test".to_string(),
            model: "claude-opus-5".to_string(),
            max_tokens: None,
            temperature: None,
            cache_config: Some("1hour".to_string()),
            enable_1m_context: None,
            thinking_budget_tokens: None,
        },
    );

    Agent::new_for_test(config, NullUiWriter, registry)
        .await
        .expect("agent construction")
}

/// Happy path: a handful of sends stays within budget.
#[tokio::test]
async fn a_few_new_turns_never_exceed_the_rolling_budget() {
    let mut agent = agent_with_cache_enabled(4).await;
    for i in 0..4 {
        agent
            .execute_task(&format!("turn {i}"), None, false)
            .await
            .expect("turn should succeed");
    }
    let count = agent.count_cache_controls_in_history_for_test();
    assert!(
        count <= agent.max_rolling_cache_breakpoints_for_test(),
        "after 4 turns, rolling breakpoints ({count}) must not exceed the budget ({})",
        agent.max_rolling_cache_breakpoints_for_test()
    );
}

/// This is the exact scenario that broke: 9 new-turn sends on one chat
/// (the real session had 8; round up to be sure the boundary is crossed
/// with margin). Before the fix, this accumulated 9 rolling breakpoints —
/// one per send — with no clearing, which combined with the system block's
/// reserved slot to exceed Anthropic's hard cap of 4.
#[tokio::test]
async fn nine_new_turns_still_respect_the_rolling_budget() {
    let mut agent = agent_with_cache_enabled(9).await;
    for i in 0..9 {
        agent
            .execute_task(&format!("turn {i}"), None, false)
            .await
            .expect("turn should succeed");
    }
    let count = agent.count_cache_controls_in_history_for_test();
    let budget = agent.max_rolling_cache_breakpoints_for_test();
    assert!(
        count <= budget,
        "after 9 turns (the exact count that broke session \
         butler_ed85ce50451a4380), rolling breakpoints ({count}) must not \
         exceed the budget ({budget}) — this is the 'Found 9' bug"
    );
}

/// Boundary: many more turns than the bug needed. If sliding-forward regresses
/// back to unconditional accumulation, this fails immediately and loudly
/// rather than only failing right at the historical trip point.
#[tokio::test]
async fn many_new_turns_never_accumulate_unboundedly() {
    let mut agent = agent_with_cache_enabled(25).await;
    for i in 0..25 {
        agent
            .execute_task(&format!("turn {i}"), None, false)
            .await
            .expect("turn should succeed");
        let count = agent.count_cache_controls_in_history_for_test();
        let budget = agent.max_rolling_cache_breakpoints_for_test();
        assert!(
            count <= budget,
            "turn {i}: rolling breakpoints ({count}) exceeded budget ({budget})"
        );
    }
}

/// Negative: with caching disabled entirely (no cache_config), no
/// cache_control markers should ever appear, regardless of turn count.
#[tokio::test]
async fn caching_disabled_never_adds_markers() {
    let mut provider = MockProvider::new().with_name("anthropic.default");
    for i in 0..5 {
        provider = provider.with_response(text_response(&format!("reply {i}")));
    }
    let mut registry = ProviderRegistry::new();
    registry.register(provider);

    let config = g3_config::Config::default(); // no cache_config set anywhere
    let mut agent = Agent::new_for_test(config, NullUiWriter, registry)
        .await
        .expect("agent construction");

    for i in 0..5 {
        agent
            .execute_task(&format!("turn {i}"), None, false)
            .await
            .expect("turn should succeed");
    }
    assert_eq!(
        agent.count_cache_controls_in_history_for_test(),
        0,
        "caching is off; no message should ever carry a cache_control marker"
    );
}
