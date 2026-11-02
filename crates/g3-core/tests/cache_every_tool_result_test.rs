//! Cache breakpoint on EVERY tool result (2026-11).
//!
//! CHARACTERIZATION: the tool loop used to place one rolling cache_control
//! breakpoint every 10th tool call. Requests in between carried up to 9 tool
//! rounds of plain uncached input on every call — measured over 400 butler
//! sessions as 66M uncached input tokens against 966M cache reads.
//!
//! What this protects:
//! - Every request after a tool result has its NEWEST message cached
//!   (so the only uncached tokens are the genuinely new ones).
//! - The rolling budget (3 when caching is on: the system block takes the 4th)
//!   is never exceeded, however long the session runs.
//! - Caching off -> no markers at all.

use g3_core::ui_writer::NullUiWriter;
use g3_core::Agent;
use g3_providers::mock::{MockProvider, MockResponse};
use g3_providers::{MessageRole, ProviderRegistry};

const TOOL_ROUNDS: usize = 25;

fn provider(cache: bool) -> MockProvider {
    let mut p = MockProvider::new()
        .with_name("anthropic.default")
        .with_model("claude-opus-5")
        .with_native_tool_calling(true)
        .with_cache_control_support(cache);
    for i in 0..TOOL_ROUNDS {
        p = p.with_response(MockResponse::native_tool_call(
            "shell",
            serde_json::json!({ "command": format!("echo round-{i}") }),
        ));
    }
    p.with_response(MockResponse::text("all done"))
}

async fn agent(p: MockProvider, cache_config: Option<&str>) -> Agent<NullUiWriter> {
    let mut registry = ProviderRegistry::new();
    registry.register(p);
    registry.set_default("anthropic.default").unwrap();
    let mut config = g3_config::Config::default();
    config.providers.default_provider = "anthropic.default".to_string();
    config.providers.anthropic.insert(
        "default".to_string(),
        g3_config::AnthropicConfig {
            api_key: "test".to_string(),
            model: "claude-opus-5".to_string(),
            max_tokens: None,
            temperature: None,
            cache_config: cache_config.map(String::from),
            enable_1m_context: None,
            thinking_budget_tokens: None,
            effort: None,
        },
    );
    Agent::new_for_test(config, NullUiWriter, registry)
        .await
        .expect("agent construction")
}

#[tokio::test]
async fn every_request_after_a_tool_result_caches_the_newest_message() {
    let p = provider(true);
    let handle = p.clone();
    let mut a = agent(p, Some("1hour")).await;
    a.execute_task("run the rounds", None, false).await.expect("turn ok");

    let requests = handle.get_requests();
    assert!(requests.len() > TOOL_ROUNDS, "expected {} requests, got {}", TOOL_ROUNDS + 1, requests.len());

    // Request 0 is the user turn; every later one follows a tool result.
    for (i, req) in requests.iter().enumerate().skip(1) {
        let last = req.messages.last().expect("non-empty");
        assert!(
            matches!(last.role, MessageRole::User) && last.tool_result_id.is_some(),
            "request {i}: last message is not a tool result"
        );
        assert!(
            last.cache_control.is_some(),
            "request {i}: newest tool result is NOT cached — uncached tail grows every call"
        );
        let n = req.messages.iter().filter(|m| m.cache_control.is_some()).count();
        assert!(n <= 3, "request {i}: {n} rolling breakpoints, budget is 3 (+1 system = API cap 4)");
    }
}

#[tokio::test]
async fn budget_holds_and_keeps_the_most_recent_breakpoints() {
    let p = provider(true);
    let handle = p.clone();
    let mut a = agent(p, Some("1hour")).await;
    a.execute_task("run the rounds", None, false).await.expect("turn ok");

    let last_req = handle.get_requests().pop().unwrap();
    let marked: Vec<usize> = last_req
        .messages
        .iter()
        .enumerate()
        .filter(|(_, m)| m.cache_control.is_some())
        .map(|(i, _)| i)
        .collect();
    assert_eq!(marked.len(), 3, "budget should be full after {TOOL_ROUNDS} rounds: {marked:?}");
    // Oldest-first eviction: the survivors are the 3 newest tool results,
    // i.e. at positions len-1, len-3, len-5 (tool_use / tool_result pairs).
    let len = last_req.messages.len();
    assert_eq!(marked, vec![len - 5, len - 3, len - 1], "evicted the wrong breakpoints");
}

#[tokio::test]
async fn caching_off_places_no_markers() {
    let p = provider(false);
    let handle = p.clone();
    let mut a = agent(p, None).await;
    a.execute_task("run the rounds", None, false).await.expect("turn ok");
    for (i, req) in handle.get_requests().iter().enumerate() {
        let n = req.messages.iter().filter(|m| m.cache_control.is_some()).count();
        assert_eq!(n, 0, "request {i} has {n} markers with caching disabled");
    }
}
