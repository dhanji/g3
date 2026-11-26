//! Slim plan: a checklist, not a ceremony (2026-11-26).
//!
//! Measured on 2,783 butler sessions before this change: the three-checks
//! schema, the filesystem "evidence" verifier and the plan_approve gate cost
//! ~6 extra round trips and ~4k output tokens per planned session, and the
//! verifier's errors were false positives (unexpanded `~`, wrong cwd). What
//! survives is the part with a consumer: an on-disk checklist that
//! butler.app's plan strip renders, and the rule that an unfinished plan
//! cannot silently lose an item.

use g3_core::tool_definitions::{create_tool_definitions, ToolConfig};
use g3_core::tools::plan::{get_plan_path, read_plan};
use g3_core::ui_writer::NullUiWriter;
use g3_core::{Agent, ToolCall};
use serial_test::serial;
use tempfile::TempDir;

async fn agent_in(temp_dir: &TempDir, session: &str) -> (Agent<NullUiWriter>, String) {
    std::env::set_current_dir(temp_dir.path()).unwrap();
    let mut agent = Agent::new(g3_config::Config::default(), NullUiWriter)
        .await
        .unwrap();
    agent.init_session_id_for_test(session);
    let sid = agent.get_session_id().unwrap().to_string();
    (agent, sid)
}

fn call(tool: &str, args: serde_json::Value) -> ToolCall {
    ToolCall { tool: tool.to_string(), args, id: String::new() }
}

fn write(plan: &str) -> ToolCall {
    call("plan_write", serde_json::json!({ "plan": plan }))
}

const SLIM: &str = "plan_id: slim\nitems:\n  - {id: I1, description: first, state: doing}\n  - {id: I2, description: second, state: todo}\n  - {id: I3, description: third, state: todo}\n";

#[tokio::test]
#[serial]
async fn slim_plan_without_revision_or_checks_is_accepted_and_stamped_approved() {
    let tmp = TempDir::new().unwrap();
    let (mut agent, sid) = agent_in(&tmp, "slim-accept").await;

    let out = agent.execute_tool(&write(SLIM)).await.unwrap();
    assert!(out.starts_with("✅"), "slim plan must be accepted: {out}");

    let plan = read_plan(&sid).unwrap().expect("plan.g3.md written");
    assert_eq!(plan.items.len(), 3);
    assert_eq!(plan.revision, 1);
    // Written only after Dhanji's tap, so a written plan IS approved — the
    // butler.app strip shows approved plans only.
    assert_eq!(plan.approved_revision, Some(1));
    let md = std::fs::read_to_string(get_plan_path(&sid)).unwrap();
    assert!(md.contains("approved_revision: 1"), "strip reads this field: {md}");
}

#[tokio::test]
#[serial]
async fn done_needs_no_evidence_and_completion_runs_no_verifier() {
    let tmp = TempDir::new().unwrap();
    let (mut agent, _) = agent_in(&tmp, "slim-done").await;
    agent.execute_tool(&write(SLIM)).await.unwrap();

    let done = "plan_id: slim\nitems:\n  - {id: I1, description: first, state: done}\n  - {id: I2, description: second, state: done, notes: tests green}\n  - {id: I3, description: third, state: blocked, notes: needs Dhanji}\n";
    let out = agent.execute_tool(&write(done)).await.unwrap();
    assert!(out.starts_with("✅"), "done without evidence is fine: {out}");
    assert!(!out.contains("VERIFICATION"), "no evidence verifier: {out}");
    assert!(!out.contains("nvelope"), "plans are not coupled to envelopes: {out}");
}

#[tokio::test]
#[serial]
async fn unfinished_plan_cannot_silently_drop_an_item() {
    let tmp = TempDir::new().unwrap();
    let (mut agent, sid) = agent_in(&tmp, "slim-drop").await;
    agent.execute_tool(&write(SLIM)).await.unwrap();

    let dropped = "plan_id: slim\nitems:\n  - {id: I1, description: first, state: done}\n  - {id: I2, description: second, state: todo}\n";
    let out = agent.execute_tool(&write(dropped)).await.unwrap();
    assert!(out.contains("I3") && out.starts_with("❌"), "removal refused: {out}");
    assert_eq!(read_plan(&sid).unwrap().unwrap().items.len(), 3, "file untouched");

    // Marking it blocked instead is the sanctioned path, and bumps revision.
    let blocked = "plan_id: slim\nitems:\n  - {id: I1, description: first, state: done}\n  - {id: I2, description: second, state: todo}\n  - {id: I3, description: third, state: blocked}\n";
    assert!(agent.execute_tool(&write(blocked)).await.unwrap().starts_with("✅"));
    assert_eq!(read_plan(&sid).unwrap().unwrap().revision, 2);
}

#[tokio::test]
#[serial]
async fn completed_plan_makes_way_for_a_fresh_one() {
    let tmp = TempDir::new().unwrap();
    let (mut agent, sid) = agent_in(&tmp, "slim-fresh").await;
    agent
        .execute_tool(&write("plan_id: a\nitems:\n  - {id: I1, description: x, state: done}\n"))
        .await
        .unwrap();
    let out = agent
        .execute_tool(&write("plan_id: b\nitems:\n  - {id: J1, description: y, state: todo}\n"))
        .await
        .unwrap();
    assert!(out.starts_with("✅"), "fresh plan after complete: {out}");
    let plan = read_plan(&sid).unwrap().unwrap();
    assert_eq!((plan.plan_id.as_str(), plan.revision), ("b", 1));
}

#[tokio::test]
#[serial]
async fn malformed_plans_are_rejected() {
    let tmp = TempDir::new().unwrap();
    let (mut agent, sid) = agent_in(&tmp, "slim-bad").await;
    for bad in [
        "plan_id: e\nitems: []\n",
        "plan_id: d\nitems:\n  - {id: I1, description: a, state: todo}\n  - {id: I1, description: b, state: todo}\n",
        "plan_id: s\nitems:\n  - {id: I1, description: a, state: finished}\n",
    ] {
        let out = agent.execute_tool(&write(bad)).await.unwrap();
        assert!(out.starts_with("❌"), "must reject {bad:?}: {out}");
    }
    assert!(read_plan(&sid).unwrap().is_none(), "nothing written");
}

#[tokio::test]
#[serial]
async fn legacy_plan_file_with_checks_and_evidence_still_loads() {
    let tmp = TempDir::new().unwrap();
    let (mut agent, sid) = agent_in(&tmp, "slim-legacy").await;
    let legacy = "# Plan: old\n\n## Plan Data\n\n```yaml\nplan_id: old\nrevision: 4\napproved_revision: 2\nitems:\n- id: I1\n  description: thing\n  state: doing\n  touches: [src/a.rs]\n  checks:\n    happy: {desc: w, target: t}\n    negative: [{desc: n, target: t}]\n    boundary: [{desc: b, target: t}]\n  evidence: [src/a.rs:1]\n```\n";
    let path = get_plan_path(&sid);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, legacy).unwrap();

    let plan = read_plan(&sid).unwrap().expect("legacy parses");
    assert_eq!((plan.revision, plan.items.len()), (4, 1));

    // A resumed session updates it in the slim shape.
    let out = agent
        .execute_tool(&write("plan_id: old\nitems:\n  - {id: I1, description: thing, state: done}\n"))
        .await
        .unwrap();
    assert!(out.starts_with("✅"), "{out}");
    assert_eq!(read_plan(&sid).unwrap().unwrap().revision, 5);
}

#[tokio::test]
#[serial]
async fn plan_read_and_plan_approve_are_gone() {
    let names: Vec<String> = create_tool_definitions(ToolConfig::default())
        .into_iter()
        .map(|t| t.name)
        .collect();
    assert!(names.iter().any(|n| n == "plan_write"));
    assert!(names.iter().any(|n| n == "write_envelope"), "email gate stays");
    for gone in ["plan_read", "plan_approve"] {
        assert!(!names.iter().any(|n| n == gone), "{gone} still advertised");
    }

    // A stale call replayed from an old transcript degrades, never panics.
    let tmp = TempDir::new().unwrap();
    let (mut agent, _) = agent_in(&tmp, "slim-gone").await;
    for gone in ["plan_read", "plan_approve"] {
        let out = agent.execute_tool(&call(gone, serde_json::json!({}))).await.unwrap();
        assert!(out.contains("Unknown tool"), "{gone}: {out}");
    }
}

#[test]
fn plan_write_schema_no_longer_demands_evidence() {
    let defs = create_tool_definitions(ToolConfig::default());
    let pw = defs.iter().find(|t| t.name == "plan_write").unwrap();
    let text = format!("{} {}", pw.description, pw.input_schema);
    for word in ["evidence", "checks", "touches", "revision"] {
        assert!(!text.contains(word), "plan_write still mentions {word}: {text}");
    }
    let env = defs.iter().find(|t| t.name == "write_envelope").unwrap();
    assert!(!env.description.contains("plan"), "envelope unhooked: {}", env.description);
}
