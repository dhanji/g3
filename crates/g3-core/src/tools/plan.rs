//! Plan — a short, persisted checklist for multi-step work.
//!
//! Slimmed 2026-11-26. This used to be "Plan Mode": a three-checks schema
//! (happy/negative/boundary), `touches`, filesystem-verified `evidence`, a
//! `plan_read`/`plan_approve` pair and a git-status approval gate. Measured
//! on 2,783 butler sessions, that cost ~6 extra round trips and ~4k output
//! tokens per planned session; the gate fired twice ever, and the evidence
//! verifier's errors were false positives (unexpanded `~`, the wrong cwd).
//!
//! What survives is what has a consumer:
//! - `plan.g3.md` on disk, rendered by butler.app's plan strip and the CLI,
//!   and readable after a context compaction;
//! - an unfinished plan cannot silently lose an item (mark it `blocked`).
//!
//! Approval is a conversation (a confirm card), not a tool: the model writes
//! the plan once the user has said yes, so every written plan is stamped
//! approved. Old plan files with `touches`/`checks`/`evidence` still load —
//! serde ignores the extra fields, and the next write drops them.

use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::fmt;
use std::path::PathBuf;
use tracing::debug;

use crate::paths::{ensure_session_dir, get_session_logs_dir};
use crate::ui_writer::UiWriter;
use crate::ToolCall;

use super::executor::ToolContext;

// ============================================================================
// Plan Schema
// ============================================================================

/// State of a plan item.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum PlanState {
    #[default]
    Todo,
    Doing,
    Done,
    Blocked,
}

impl fmt::Display for PlanState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PlanState::Todo => write!(f, "todo"),
            PlanState::Doing => write!(f, "doing"),
            PlanState::Done => write!(f, "done"),
            PlanState::Blocked => write!(f, "blocked"),
        }
    }
}

impl std::str::FromStr for PlanState {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "todo" => Ok(PlanState::Todo),
            "doing" => Ok(PlanState::Doing),
            "done" => Ok(PlanState::Done),
            "blocked" => Ok(PlanState::Blocked),
            _ => Err(anyhow!("Invalid plan state: {}", s)),
        }
    }
}

/// A single item in the plan.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlanItem {
    /// Stable identifier (e.g., "I1", "I2")
    pub id: String,
    /// What will be done
    pub description: String,
    /// Current state
    #[serde(default)]
    pub state: PlanState,
    /// Optional one-liner: why it's blocked, what it turned out to need
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
}

impl PlanItem {
    pub fn new(id: impl Into<String>, description: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            description: description.into(),
            state: PlanState::Todo,
            notes: None,
        }
    }

    pub fn validate(&self) -> Result<()> {
        if self.id.trim().is_empty() {
            return Err(anyhow!("Item id cannot be empty"));
        }
        if self.description.trim().is_empty() {
            return Err(anyhow!("Item '{}' has an empty description", self.id));
        }
        Ok(())
    }

    /// Done or blocked.
    pub fn is_terminal(&self) -> bool {
        matches!(self.state, PlanState::Done | PlanState::Blocked)
    }
}

fn first_revision() -> u32 {
    1
}

/// A complete plan with metadata and items.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Plan {
    pub plan_id: String,
    /// Owned by g3: 1 for a fresh plan, +1 per update. Whatever the model
    /// sends is ignored (it used to be required, and omitting it — as the
    /// prompt's own example did — was a rejected write).
    #[serde(default = "first_revision")]
    pub revision: u32,
    /// Kept for butler.app's plan strip, which shows approved plans only.
    /// Every write stamps it (see module docs).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approved_revision: Option<u32>,
    pub items: Vec<PlanItem>,
}

impl Plan {
    pub fn new(plan_id: impl Into<String>) -> Self {
        Self {
            plan_id: plan_id.into(),
            revision: 1,
            approved_revision: None,
            items: Vec::new(),
        }
    }

    pub fn is_approved(&self) -> bool {
        self.approved_revision.is_some()
    }

    pub fn approve(&mut self) {
        self.approved_revision = Some(self.revision);
    }

    /// All items done or blocked (and there is at least one).
    pub fn is_complete(&self) -> bool {
        !self.items.is_empty() && self.items.iter().all(|item| item.is_terminal())
    }

    pub fn validate(&self) -> Result<()> {
        if self.plan_id.trim().is_empty() {
            return Err(anyhow!("Plan ID cannot be empty"));
        }
        if self.items.is_empty() {
            return Err(anyhow!("Plan must have at least one item"));
        }
        let mut seen = HashSet::new();
        for item in &self.items {
            if !seen.insert(&item.id) {
                return Err(anyhow!("Duplicate item ID: {}", item.id));
            }
            item.validate()?;
        }
        Ok(())
    }

    pub fn status_summary(&self) -> String {
        let count = |s: PlanState| self.items.iter().filter(|i| i.state == s).count();
        format!(
            "Plan '{}' rev {}: {}/{} done, {} doing, {} blocked, {} todo",
            self.plan_id,
            self.revision,
            count(PlanState::Done),
            self.items.len(),
            count(PlanState::Doing),
            count(PlanState::Blocked),
            count(PlanState::Todo),
        )
    }
}

// ============================================================================
// Plan Storage
// ============================================================================

/// Get the path to the plan.g3.md file for a session.
pub fn get_plan_path(session_id: &str) -> PathBuf {
    get_session_logs_dir(session_id).join("plan.g3.md")
}

/// Read a plan from the session's plan.g3.md file.
pub fn read_plan(session_id: &str) -> Result<Option<Plan>> {
    let path = get_plan_path(session_id);
    if !path.exists() {
        return Ok(None);
    }
    let content = std::fs::read_to_string(&path)?;
    let yaml_content = extract_yaml_from_markdown(&content)?;
    let plan: Plan = serde_yaml::from_str(&yaml_content)?;
    Ok(Some(plan))
}

/// Write a plan to the session's plan.g3.md file.
pub fn write_plan(session_id: &str, plan: &Plan) -> Result<()> {
    plan.validate()?;
    let _ = ensure_session_dir(session_id)?;
    std::fs::write(get_plan_path(session_id), format_plan_as_markdown(plan))?;
    Ok(())
}

/// Extract YAML content from a markdown file with ```yaml code block.
fn extract_yaml_from_markdown(content: &str) -> Result<String> {
    let start_marker = "```yaml";

    if let Some(start_idx) = content.find(start_marker) {
        let yaml_start = start_idx + start_marker.len();
        // Find the closing ``` at the START of a line. A plain .find("```")
        // matched backticks inside YAML string values and truncated the plan.
        let remainder = &content[yaml_start..];
        for (i, line) in remainder.split('\n').enumerate() {
            if i > 0 && line.starts_with("```") {
                let offset: usize = remainder.split('\n').take(i).map(|l| l.len() + 1).sum();
                return Ok(remainder[..offset].trim().to_string());
            }
        }
    }

    // If no code block, try parsing the whole content as YAML
    Ok(content.to_string())
}

/// Format a plan as markdown with embedded YAML.
fn format_plan_as_markdown(plan: &Plan) -> String {
    let yaml = serde_yaml::to_string(plan).unwrap_or_else(|_| "# Error serializing plan".to_string());
    format!(
        "# Plan: {}\n\n**Status**: {}\n\n## Plan Data\n\n```yaml\n{}```\n",
        plan.plan_id,
        plan.status_summary(),
        yaml
    )
}

// ============================================================================
// Tool Implementation
// ============================================================================

/// Execute the `plan_write` tool: create, update or replace the plan.
pub async fn execute_plan_write<W: UiWriter>(
    tool_call: &ToolCall,
    ctx: &mut ToolContext<'_, W>,
) -> Result<String> {
    debug!("Processing plan_write tool call");

    let session_id = match ctx.session_id {
        Some(id) => id,
        None => return Ok("❌ No active session - plans are session-scoped.".to_string()),
    };

    let plan_yaml = match tool_call.args.get("plan").and_then(|v| v.as_str()) {
        Some(p) => p,
        None => return Ok("❌ Missing 'plan' argument. Provide the plan as YAML.".to_string()),
    };

    let mut plan: Plan = match serde_yaml::from_str(plan_yaml) {
        Ok(p) => p,
        Err(e) => return Ok(format!("❌ Invalid plan YAML: {}", e)),
    };

    plan.revision = 1;
    if let Some(existing) = read_plan(session_id)? {
        // A finished plan makes way for a fresh one; an unfinished one is
        // being updated, and may not silently lose scope.
        if !existing.is_complete() {
            let dropped: Vec<&str> = existing
                .items
                .iter()
                .filter(|old| !plan.items.iter().any(|i| i.id == old.id))
                .map(|old| old.id.as_str())
                .collect();
            if !dropped.is_empty() {
                return Ok(format!(
                    "❌ Cannot drop item(s) {} from an unfinished plan. Mark them `blocked` (with a note saying why) instead.",
                    dropped.join(", ")
                ));
            }
            plan.revision = existing.revision + 1;
        }
    }

    if let Err(e) = plan.validate() {
        return Ok(format!("❌ Plan validation failed: {}", e));
    }

    plan.approve();
    if let Err(e) = write_plan(session_id, &plan) {
        return Ok(format!("❌ Failed to write plan: {}", e));
    }

    let plan_path = get_plan_path(session_id);
    let yaml = serde_yaml::to_string(&plan)?;
    ctx.ui_writer
        .print_plan_compact(Some(&yaml), Some(&plan_path.to_string_lossy()), true);

    Ok(format!("✅ Plan updated: {}", plan.status_summary()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plan_with(states: &[PlanState]) -> Plan {
        let mut plan = Plan::new("test");
        for (i, s) in states.iter().enumerate() {
            let mut item = PlanItem::new(format!("I{}", i + 1), "do a thing");
            item.state = *s;
            plan.items.push(item);
        }
        plan
    }

    #[test]
    fn test_plan_state_display_and_parse() {
        assert_eq!(PlanState::Blocked.to_string(), "blocked");
        assert_eq!("DOING".parse::<PlanState>().unwrap(), PlanState::Doing);
        assert!("finished".parse::<PlanState>().is_err());
    }

    #[test]
    fn test_validation() {
        assert!(Plan::new("empty").validate().is_err(), "no items");
        assert!(plan_with(&[PlanState::Done]).validate().is_ok(), "done needs no evidence");

        let mut dup = plan_with(&[PlanState::Todo, PlanState::Todo]);
        dup.items[1].id = "I1".into();
        assert!(dup.validate().is_err(), "duplicate id");

        let mut blank = plan_with(&[PlanState::Todo]);
        blank.items[0].description = "  ".into();
        assert!(blank.validate().is_err(), "blank description");
    }

    #[test]
    fn test_is_complete() {
        assert!(!Plan::new("none").is_complete(), "an empty plan is not complete");
        assert!(!plan_with(&[PlanState::Done, PlanState::Doing]).is_complete());
        assert!(plan_with(&[PlanState::Done, PlanState::Blocked]).is_complete());
    }

    #[test]
    fn test_minimal_yaml_defaults() {
        let plan: Plan = serde_yaml::from_str("plan_id: p\nitems:\n  - {id: I1, description: x}\n").unwrap();
        assert_eq!(plan.revision, 1);
        assert_eq!(plan.items[0].state, PlanState::Todo);
        assert!(plan.approved_revision.is_none());
    }

    #[test]
    fn test_legacy_fields_ignored_and_dropped_on_write() {
        let legacy = "plan_id: p\nrevision: 3\nitems:\n  - id: I1\n    description: x\n    state: done\n    touches: [a]\n    checks: {happy: {desc: w, target: t}, negative: [], boundary: []}\n    evidence: [a.rs:1]\n    notes: ok\n";
        let plan: Plan = serde_yaml::from_str(legacy).unwrap();
        assert_eq!(plan.items[0].notes.as_deref(), Some("ok"));
        let out = serde_yaml::to_string(&plan).unwrap();
        for gone in ["touches", "checks", "evidence"] {
            assert!(!out.contains(gone), "{gone} survived a rewrite: {out}");
        }
    }

    #[test]
    fn test_status_summary() {
        let s = plan_with(&[PlanState::Done, PlanState::Doing, PlanState::Blocked, PlanState::Todo]).status_summary();
        assert_eq!(s, "Plan 'test' rev 1: 1/4 done, 1 doing, 1 blocked, 1 todo");
    }

    #[test]
    fn test_yaml_extraction() {
        let md = "# Plan: test\n\n## Plan Data\n\n```yaml\nplan_id: test\nrevision: 1\nitems: []\n```\n";
        assert!(extract_yaml_from_markdown(md).unwrap().contains("plan_id: test"));
    }

    #[test]
    fn test_yaml_extraction_with_backticks_in_values() {
        // A ``` inside a YAML value must not be taken for the closing fence.
        let md = "## Plan Data\n\n```yaml\nplan_id: test\nitems:\n  - id: I1\n    description: 'Fix the ```yaml parsing issue with ```'\n    state: todo\n```\n";
        let yaml = extract_yaml_from_markdown(md).unwrap();
        assert!(yaml.contains("state: todo"), "truncated at embedded backticks: {yaml}");
    }

    #[test]
    fn test_yaml_extraction_no_code_block_fallback() {
        let raw = "plan_id: test\nrevision: 1\nitems: []\n";
        assert_eq!(extract_yaml_from_markdown(raw).unwrap(), raw);
    }

    #[test]
    fn test_yaml_extraction_closing_fence_no_trailing_newline() {
        let md = "```yaml\nplan_id: test\nrevision: 1\nitems: []\n```";
        assert!(extract_yaml_from_markdown(md).unwrap().contains("plan_id: test"));
    }

    #[test]
    fn test_markdown_roundtrip() {
        let mut plan = plan_with(&[PlanState::Doing]);
        plan.approve();
        let md = format_plan_as_markdown(&plan);
        let parsed: Plan = serde_yaml::from_str(&extract_yaml_from_markdown(&md).unwrap()).unwrap();
        assert_eq!(parsed.approved_revision, Some(1));
        assert_eq!(parsed.items[0].state, PlanState::Doing);
    }
}
