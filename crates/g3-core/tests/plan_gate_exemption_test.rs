//! The plan approval gate must never swallow a PLAN tool's own result.
//!
//! The gate runs after every tool in plan mode and, when the tree has new
//! changes and the plan is unapproved, REPLACES the tool's result with
//! "IMPLEMENTATION BLOCKED … create a plan with plan_write". Applied to
//! plan_write / plan_read / plan_approve that is a catch-22: the model is told
//! to write a plan in answer to writing a plan, and the plan it just wrote is
//! hidden from it (PR #75, Jochen). Plan tools touch only g3's session state,
//! never the working tree, so they are exempt — on BOTH execution paths: the
//! sequential one (`execute_tool_in_dir`) and the concurrent batch
//! (`execute_batch_concurrently`, where plan_read is concurrency-safe).
//!
//! Every other tool stays gated: the negative cases put a gated tool in the
//! same turn and assert it is still BLOCKED.

use g3_core::tools::plan::{write_plan, Check, Checks, Plan, PlanItem, PlanState};
use g3_core::ui_writer::NullUiWriter;
use g3_core::Agent;
use g3_providers::mock::{MockChunk, MockProvider, MockResponse};
use g3_providers::{ProviderRegistry, Usage};
use tempfile::TempDir;

const PROVIDER: &str = "anthropic.default";
const BLOCKED: &str = "IMPLEMENTATION BLOCKED";

fn usage() -> Usage {
    Usage { prompt_tokens: 100, completion_tokens: 50, total_tokens: 150, cache_creation_tokens: 0, cache_read_tokens: 0 }
}

/// One response carrying several native tool calls on the final chunk — the
/// shape that makes g3 run them as a (possibly concurrent) batch.
fn batch(tag: &str, calls: Vec<(&str, serde_json::Value)>) -> MockResponse {
    let mut fin = MockChunk::finished("tool_use");
    fin.tool_calls = Some(
        calls
            .into_iter()
            .enumerate()
            .map(|(i, (t, a))| g3_providers::ToolCall { id: format!("toolu_{tag}_{i}_{t}"), tool: t.into(), args: a })
            .collect(),
    );
    MockResponse::custom(vec![fin], usage())
}

fn git(dir: &std::path::Path, args: &[&str]) {
    std::process::Command::new("git").args(args).current_dir(dir).output().expect("git");
}

/// A clean repo, plan mode on, an UNAPPROVED plan, and then a stray file
/// changed after the baseline — so the gate WOULD block any tool it inspects.
async fn gated_agent(session_id: &str, responses: Vec<MockResponse>) -> (Agent<NullUiWriter>, TempDir) {
    let repo = TempDir::new().unwrap();
    let p = repo.path();
    git(p, &["init"]);
    git(p, &["config", "user.email", "t@t"]);
    git(p, &["config", "user.name", "t"]);
    std::fs::write(p.join("README.md"), "# t").unwrap();
    git(p, &["add", "."]);
    git(p, &["commit", "-m", "init"]);

    let mut prov = MockProvider::new().with_name(PROVIDER).with_model("claude-opus-5").with_native_tool_calling(true);
    for r in responses {
        prov = prov.with_response(r);
    }
    prov = prov.with_default_response(MockResponse::text("done"));
    let mut registry = ProviderRegistry::new();
    registry.register(prov);
    registry.set_default(PROVIDER).unwrap();
    let mut agent = Agent::new_for_test(g3_config::Config::default(), NullUiWriter, registry).await.unwrap();

    agent.set_session_id(session_id.to_string());
    agent.set_working_dir(p.to_string_lossy().to_string());
    agent.set_plan_mode(true, Some(&p.to_string_lossy()));

    let mut plan = Plan::new("gate-exemption");
    plan.items.push(PlanItem {
        id: "I1".into(),
        description: "marker-item-description".into(),
        state: PlanState::Todo,
        touches: vec!["src/x.rs".into()],
        checks: Checks {
            happy: Check::new("h", "t"),
            negative: vec![Check::new("n", "t")],
            boundary: vec![Check::new("b", "t")],
        },
        evidence: vec![],
        notes: None,
    });
    write_plan(session_id, &plan).unwrap();

    // Changed AFTER the baseline: every gated tool from here on is BLOCKED.
    std::fs::write(p.join("stray.txt"), "x").unwrap();
    (agent, repo)
}

/// The result text for each tool_use id, in history order.
fn results(agent: &Agent<NullUiWriter>) -> Vec<(String, String)> {
    agent
        .get_context_window()
        .conversation_history
        .iter()
        .filter_map(|m| m.tool_result_id.clone().map(|id| (id, m.content.clone())))
        .collect()
}

fn result_for<'a>(rs: &'a [(String, String)], needle: &str) -> &'a str {
    &rs.iter().find(|(id, _)| id.contains(needle)).unwrap_or_else(|| panic!("no result for {needle}: {rs:?}")).1
}

#[tokio::test]
async fn sequential_plan_read_is_not_replaced_by_the_gate_but_shell_still_is() {
    let (mut agent, _repo) = gated_agent(
        "gate-exempt-seq",
        vec![
            MockResponse::native_tool_call("plan_read", serde_json::json!({})),
            MockResponse::native_tool_call("shell", serde_json::json!({"command": "true"})),
        ],
    )
    .await;
    agent.execute_task("go", None, false).await.expect("turn ok");
    let rs = results(&agent);
    assert_eq!(rs.len(), 2, "{rs:?}");

    let plan = &rs[0].1;
    assert!(!plan.contains(BLOCKED), "plan_read was swallowed by the gate: {plan}");
    assert!(plan.contains("marker-item-description"), "plan_read lost its plan: {plan}");

    let shell = &rs[1].1;
    assert!(shell.contains(BLOCKED), "the gate must still block non-plan tools: {shell}");
}

#[tokio::test]
async fn concurrent_batch_exempts_plan_read_but_still_gates_its_sibling() {
    let (mut agent, _repo) = gated_agent(
        "gate-exempt-conc",
        vec![batch("c", vec![("plan_read", serde_json::json!({})), ("read_file", serde_json::json!({"file_path": "README.md"}))])],
    )
    .await;
    agent.execute_task("go", None, false).await.expect("turn ok");
    let rs = results(&agent);
    assert_eq!(rs.len(), 2, "{rs:?}");

    let plan = result_for(&rs, "plan_read");
    assert!(!plan.contains(BLOCKED), "concurrent plan_read was swallowed by the gate: {plan}");
    assert!(plan.contains("marker-item-description"), "{plan}");

    let read = result_for(&rs, "read_file");
    assert!(read.contains(BLOCKED), "concurrent sibling must still be gated: {read}");
}

#[tokio::test]
async fn plan_read_with_no_plan_says_so_instead_of_returning_nothing() {
    // An empty tool result reads to the model as "the call did nothing"; it
    // then retries plan_read or invents a plan. Say what's true.
    let repo = TempDir::new().unwrap();
    let mut prov = MockProvider::new().with_name(PROVIDER).with_native_tool_calling(true);
    prov = prov.with_response(MockResponse::native_tool_call("plan_read", serde_json::json!({})));
    prov = prov.with_default_response(MockResponse::text("done"));
    let mut registry = ProviderRegistry::new();
    registry.register(prov);
    registry.set_default(PROVIDER).unwrap();
    let mut agent = Agent::new_for_test(g3_config::Config::default(), NullUiWriter, registry).await.unwrap();
    agent.set_session_id("plan-read-no-plan-xyz".into());
    agent.set_working_dir(repo.path().to_string_lossy().to_string());
    agent.execute_task("go", None, false).await.expect("turn ok");
    let rs = results(&agent);
    assert!(rs[0].1.contains("No plan exists yet"), "{rs:?}");
}
