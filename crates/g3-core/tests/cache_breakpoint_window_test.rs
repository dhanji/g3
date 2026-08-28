//! Regression test for the prompt-cache breakpoint sliding window.
//!
//! Anthropic caches the request prefix only up to the last `cache_control`
//! breakpoint, and allows at most 4 breakpoints per request. The agent places a
//! breakpoint on every 10th tool result. Previously the placement was gated by
//! `count_cache_controls_in_history() < 4`, so once 4 breakpoints accumulated
//! (the first user message plus tool results #10/#20/#30) the guard stayed false
//! forever and the last breakpoint froze near the start of a long session. The
//! growing tail past that frozen breakpoint was then re-sent uncached every turn.
//!
//! The fix slides the 4-breakpoint window forward: when the cap is reached the
//! oldest movable breakpoint is recycled so a fresh one can be placed near the
//! tail. This test drives >40 tool results through the real placement path and
//! asserts the last breakpoint advances while the total stays within the cap.

use g3_core::ui_writer::NullUiWriter;
use g3_core::Agent;
use g3_providers::mock::{MockProvider, MockResponse};
use g3_providers::ProviderRegistry;

/// Build a config whose default Anthropic provider has caching enabled, so
/// `get_provider_cache_control()` returns a real cache config.
fn config_with_anthropic_cache() -> g3_config::Config {
    let mut config = g3_config::Config::default();
    config.providers.anthropic.insert(
        "default".to_string(),
        g3_config::AnthropicConfig {
            api_key: "test-key".to_string(),
            model: "claude-test".to_string(),
            max_tokens: Some(4096),
            temperature: None,
            cache_config: Some("ephemeral".to_string()),
            enable_1m_context: None,
            thinking_budget_tokens: None,
        },
    );
    config
}

async fn create_agent() -> Agent<NullUiWriter> {
    // Name the mock provider "anthropic" so it resolves to the anthropic config
    // (config_name defaults to "default") and caching is considered supported.
    let provider = MockProvider::new()
        .with_name("anthropic")
        .with_cache_control_support(true)
        .with_default_response(MockResponse::text("done"));

    let mut registry = ProviderRegistry::new();
    registry.register(provider);

    Agent::new_for_test(config_with_anthropic_cache(), NullUiWriter, registry)
        .await
        .expect("Failed to create agent")
}

/// Count cache_control breakpoints in the conversation history.
fn count_cache_controls(agent: &Agent<NullUiWriter>) -> usize {
    agent
        .get_context_window()
        .conversation_history
        .iter()
        .filter(|m| m.cache_control.is_some())
        .count()
}

/// Index of the last (newest) cache_control breakpoint in the history.
fn last_cache_control_index(agent: &Agent<NullUiWriter>) -> Option<usize> {
    agent
        .get_context_window()
        .conversation_history
        .iter()
        .enumerate()
        .filter(|(_, m)| m.cache_control.is_some())
        .map(|(idx, _)| idx)
        .next_back()
}

#[tokio::test]
async fn breakpoint_window_slides_forward_past_the_cap() {
    let mut agent = create_agent().await;

    // Seed the conversation and place the anchor breakpoint on the first user
    // message (this is the breakpoint that should stay put).
    agent
        .execute_task("kick off a long session", None, false)
        .await
        .expect("initial task should succeed");

    let breakpoints_after_seed = count_cache_controls(&agent);
    assert!(
        breakpoints_after_seed >= 1,
        "expected an anchor breakpoint on the first user message, got {breakpoints_after_seed}",
    );

    // Drive 30 tool results. Breakpoints land on #10/#20/#30, which together with
    // the anchor reaches the 4-breakpoint cap. Record where the last breakpoint
    // sits at the cap — under the old append-only behavior it freezes here.
    let mut frozen_last_index = None;
    for n in 1..=30 {
        let cadence_hit = n % 10 == 0;
        agent
            .push_tool_result_for_test(&format!("tool result {n}"), cadence_hit)
            .expect("tool result placement should succeed");
        if n == 30 {
            frozen_last_index = last_cache_control_index(&agent);
        }
    }

    assert_eq!(
        count_cache_controls(&agent),
        4,
        "should be at the 4-breakpoint cap after 30 tool results",
    );
    let frozen_last_index = frozen_last_index.expect("a breakpoint should exist at the cap");

    // Keep going well past the cap (41 total). With the sliding window, the last
    // breakpoint must advance toward the tail instead of staying frozen.
    for n in 31..=41 {
        let cadence_hit = n % 10 == 0; // #40
        agent
            .push_tool_result_for_test(&format!("tool result {n}"), cadence_hit)
            .expect("tool result placement should succeed");
    }

    let total = count_cache_controls(&agent);
    assert!(
        total <= 4,
        "total breakpoints must never exceed Anthropic's cap of 4, got {total}",
    );

    let advanced_last_index =
        last_cache_control_index(&agent).expect("a breakpoint should still exist");
    assert!(
        advanced_last_index > frozen_last_index,
        "last breakpoint should advance past the frozen position {frozen_last_index}, \
         but it is at {advanced_last_index}",
    );

    // The advanced breakpoint should sit near the growing tail, not back at the
    // early frozen spot — confirming the tail is now inside the cached prefix.
    let history_len = agent.get_context_window().conversation_history.len();
    assert!(
        history_len - advanced_last_index <= 2,
        "advanced breakpoint should be near the tail (len {history_len}, idx {advanced_last_index})",
    );
}
