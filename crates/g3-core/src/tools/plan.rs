//! Plan Mode - Structured task planning with cognitive forcing.
//!
//! This module implements Plan Mode, which replaces the TODO system with a
//! checklist-style plan that forces reasoning about:
//! - Happy path
//! - Negative case  
//! - Boundary condition
//!
//! A task is done ONLY when all plan items are satisfied with evidence.

use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::fmt;
use std::path::PathBuf;
use std::path::Path;
use tracing::debug;

use crate::paths::{ensure_session_dir, get_session_logs_dir};
use crate::ui_writer::UiWriter;
use crate::ToolCall;

use super::executor::ToolContext;

use super::invariants::{format_envelope_markdown, get_envelope_path, read_envelope};

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

/// A check with description and target.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Check {
    /// Description of what this check verifies
    pub desc: String,
    /// Target module/function/file this check applies to
    pub target: String,
}

impl Check {
    pub fn new(desc: impl Into<String>, target: impl Into<String>) -> Self {
        Self {
            desc: desc.into(),
            target: target.into(),
        }
    }

    /// Validate that the check has required fields.
    pub fn validate(&self) -> Result<()> {
        if self.desc.trim().is_empty() {
            return Err(anyhow!("Check description cannot be empty"));
        }
        if self.target.trim().is_empty() {
            return Err(anyhow!("Check target cannot be empty"));
        }
        Ok(())
    }
}

/// The three required checks for each plan item.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Checks {
    /// Happy path check - normal successful operation
    pub happy: Check,
    /// Negative case checks - error handling, invalid input (at least 1 required)
    pub negative: Vec<Check>,
    /// Boundary condition checks - edge cases, limits (at least 1 required)
    pub boundary: Vec<Check>,
}

impl Checks {
    /// Validate all checks (1 happy, 1+ negative, 1+ boundary).
    pub fn validate(&self) -> Result<()> {
        self.happy.validate().map_err(|e| anyhow!("happy check: {}", e))?;
        
        if self.negative.is_empty() {
            return Err(anyhow!("at least one negative check is required"));
        }
        for (i, check) in self.negative.iter().enumerate() {
            check.validate().map_err(|e| anyhow!("negative check [{}]: {}", i, e))?;
        }
        
        if self.boundary.is_empty() {
            return Err(anyhow!("at least one boundary check is required"));
        }
        for (i, check) in self.boundary.iter().enumerate() {
            check.validate().map_err(|e| anyhow!("boundary check [{}]: {}", i, e))?;
        }
        Ok(())
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
    pub state: PlanState,
    /// Paths/modules this affects
    pub touches: Vec<String>,
    /// The three required checks
    pub checks: Checks,
    /// Evidence when done (file:line, test names, snippets)
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub evidence: Vec<String>,
    /// Short explanation including implementation nuances
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
}

impl PlanItem {
    /// Create a new plan item with required fields.
    pub fn new(
        id: impl Into<String>,
        description: impl Into<String>,
        touches: Vec<String>,
        checks: Checks,
    ) -> Self {
        Self {
            id: id.into(),
            description: description.into(),
            state: PlanState::Todo,
            touches,
            checks,
            evidence: Vec::new(),
            notes: None,
        }
    }

    /// Validate the plan item structure.
    pub fn validate(&self) -> Result<()> {
        if self.id.trim().is_empty() {
            return Err(anyhow!("Item id cannot be empty"));
        }
        if self.description.trim().is_empty() {
            return Err(anyhow!("Item description cannot be empty"));
        }
        if self.touches.is_empty() {
            return Err(anyhow!("Item must specify at least one path/module in 'touches'"));
        }
        self.checks.validate().map_err(|e| anyhow!("Item '{}': {}", self.id, e))?;

        // If done, must have evidence and notes
        if self.state == PlanState::Done {
            if self.evidence.is_empty() {
                return Err(anyhow!(
                    "Item '{}' is marked done but has no evidence",
                    self.id
                ));
            }
            if self.notes.as_ref().map(|n| n.trim().is_empty()).unwrap_or(true) {
                return Err(anyhow!(
                    "Item '{}' is marked done but has no notes",
                    self.id
                ));
            }
        }

        Ok(())
    }

    /// Check if this item is terminal (done or blocked).
    pub fn is_terminal(&self) -> bool {
        matches!(self.state, PlanState::Done | PlanState::Blocked)
    }
}

/// A complete plan with metadata and items.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Plan {
    /// Unique identifier for this plan
    pub plan_id: String,
    /// Current revision number (increments on each write)
    pub revision: u32,
    /// The revision that was approved (None if not yet approved)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approved_revision: Option<u32>,
    /// The plan items
    pub items: Vec<PlanItem>,
}

impl Plan {
    /// Create a new plan with the given ID.
    pub fn new(plan_id: impl Into<String>) -> Self {
        Self {
            plan_id: plan_id.into(),
            revision: 1,
            approved_revision: None,
            items: Vec::new(),
        }
    }

    /// Check if the plan has been approved.
    pub fn is_approved(&self) -> bool {
        self.approved_revision.is_some()
    }

    /// Approve the current revision.
    pub fn approve(&mut self) {
        self.approved_revision = Some(self.revision);
    }

    /// Check if all items are terminal (done or blocked).
    pub fn is_complete(&self) -> bool {
        !self.items.is_empty() && self.items.iter().all(|item| item.is_terminal())
    }

    /// Validate the entire plan structure.
    pub fn validate(&self) -> Result<()> {
        if self.plan_id.trim().is_empty() {
            return Err(anyhow!("Plan ID cannot be empty"));
        }

        if self.items.is_empty() {
            return Err(anyhow!("Plan must have at least one item"));
        }

        if self.items.len() > 7 {
            // Warn but don't fail - this is a guideline
            debug!("Plan has {} items (recommended max is 7)", self.items.len());
        }

        // Check for duplicate IDs
        let mut seen_ids = std::collections::HashSet::new();
        for item in &self.items {
            if !seen_ids.insert(&item.id) {
                return Err(anyhow!("Duplicate item ID: {}", item.id));
            }
            item.validate()?;
        }

        Ok(())
    }

    /// Get a summary of the plan status.
    pub fn status_summary(&self) -> String {
        let total = self.items.len();
        let done = self.items.iter().filter(|i| i.state == PlanState::Done).count();
        let doing = self.items.iter().filter(|i| i.state == PlanState::Doing).count();
        let blocked = self.items.iter().filter(|i| i.state == PlanState::Blocked).count();
        let todo = self.items.iter().filter(|i| i.state == PlanState::Todo).count();

        let approved_str = if let Some(rev) = self.approved_revision {
            format!(" (approved at rev {})", rev)
        } else {
            " (NOT APPROVED)".to_string()
        };

        format!(
            "Plan '{}' rev {}{}: {}/{} done, {} doing, {} blocked, {} todo",
            self.plan_id, self.revision, approved_str, done, total, doing, blocked, todo
        )
    }
}

// ============================================================================
// Evidence Verification Types
// ============================================================================

/// Type of evidence that can be verified.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EvidenceType {
    /// Code location: file path with optional line range (e.g., "src/foo.rs:42-118")
    CodeLocation {
        file_path: String,
        start_line: Option<u32>,
        end_line: Option<u32>,
    },
    /// Test reference: file path with test function name (e.g., "tests/foo.rs::test_bar")
    TestReference {
        file_path: String,
        test_name: String,
    },
    /// Unknown format - will be skipped with a warning
    Unknown(String),
}

/// Result of verifying a single piece of evidence.
#[derive(Debug, Clone)]
pub enum VerificationStatus {
    /// Evidence verified successfully
    Verified,
    /// Warning - evidence may be invalid but not blocking
    Warning(String),
    /// Error - evidence is definitely invalid
    Error(String),
    /// Skipped - couldn't verify (e.g., unknown format)
    Skipped(String),
}

impl VerificationStatus {
    pub fn is_ok(&self) -> bool {
        matches!(self, VerificationStatus::Verified | VerificationStatus::Skipped(_))
    }

    pub fn is_warning_or_error(&self) -> bool {
        matches!(self, VerificationStatus::Warning(_) | VerificationStatus::Error(_))
    }
}

/// Result of verifying a single evidence string.
#[derive(Debug, Clone)]
pub struct EvidenceVerification {
    /// The original evidence string
    pub evidence: String,
    /// Parsed evidence type
    pub evidence_type: EvidenceType,
    /// Verification result
    pub status: VerificationStatus,
}

/// Result of verifying all evidence for a plan item.
#[derive(Debug, Clone)]
pub struct ItemVerification {
    /// Item ID
    pub item_id: String,
    /// Item description (for display)
    pub description: String,
    /// Results for each piece of evidence
    pub evidence_results: Vec<EvidenceVerification>,
    /// Whether the item had no evidence (warning condition)
    pub missing_evidence: bool,
}

/// Result of verifying an entire plan.
#[derive(Debug, Clone)]
pub struct PlanVerification {
    /// Plan ID
    pub plan_id: String,
    /// Results for each verified item
    pub item_results: Vec<ItemVerification>,
    /// Count of items that were skipped (blocked state)
    pub skipped_count: usize,
}

impl PlanVerification {
    /// Check if all verifications passed (no errors or warnings).
    pub fn all_passed(&self) -> bool {
        self.item_results.iter().all(|item| {
            !item.missing_evidence
                && item.evidence_results.iter().all(|e| e.status.is_ok())
        })
    }

    /// Count total warnings and errors.
    pub fn count_issues(&self) -> (usize, usize) {
        let mut warnings = 0;
        let mut errors = 0;
        for item in &self.item_results {
            if item.missing_evidence {
                warnings += 1;
            }
            for ev in &item.evidence_results {
                match &ev.status {
                    VerificationStatus::Warning(_) => warnings += 1,
                    VerificationStatus::Error(_) => errors += 1,
                    _ => {}
                }
            }
        }
        (warnings, errors)
    }
}

/// Parse an evidence string into an EvidenceType.
pub fn parse_evidence(evidence: &str) -> EvidenceType {
    let evidence = evidence.trim();
    
    // Check for test reference format: "path/to/file.rs::test_name"
    if let Some(idx) = evidence.find("::") {
        let file_path = evidence[..idx].to_string();
        let test_name = evidence[idx + 2..].to_string();
        return EvidenceType::TestReference { file_path, test_name };
    }
    
    // Check for code location format: "path/to/file.rs:42" or "path/to/file.rs:42-118"
    if let Some(idx) = evidence.rfind(':') {
        let file_path = evidence[..idx].to_string();
        let line_part = &evidence[idx + 1..];
        
        // Try to parse line range (e.g., "42-118" or just "42")
        if let Some((start, end)) = parse_line_range(line_part) {
            return EvidenceType::CodeLocation {
                file_path,
                start_line: Some(start),
                end_line: end,
            };
        }
    }
    
    // Check if it looks like a file path (contains . or /)
    if evidence.contains('.') || evidence.contains('/') {
        return EvidenceType::CodeLocation {
            file_path: evidence.to_string(),
            start_line: None,
            end_line: None,
        };
    }
    
    // Unknown format
    EvidenceType::Unknown(evidence.to_string())
}

/// Parse a line range string like "42" or "42-118".
/// Returns (start_line, optional_end_line).
fn parse_line_range(s: &str) -> Option<(u32, Option<u32>)> {
    if let Some(dash_idx) = s.find('-') {
        let start_str = &s[..dash_idx];
        let end_str = &s[dash_idx + 1..];
        let start = start_str.parse::<u32>().ok()?;
        let end = end_str.parse::<u32>().ok()?;
        Some((start, Some(end)))
    } else {
        let line = s.parse::<u32>().ok()?;
        Some((line, None))
    }
}

/// Verify a code location evidence (file exists, line numbers valid).
/// 
/// # Arguments
/// * `file_path` - Path to the file (relative to working_dir or absolute)
/// * `start_line` - Optional start line number (1-indexed)
/// * `end_line` - Optional end line number (1-indexed)
/// * `working_dir` - Working directory for resolving relative paths
pub fn verify_code_location(
    file_path: &str,
    start_line: Option<u32>,
    end_line: Option<u32>,
    working_dir: Option<&str>,
) -> VerificationStatus {
    use std::path::Path;
    
    // Resolve the file path
    let resolved_path = if Path::new(file_path).is_absolute() {
        PathBuf::from(file_path)
    } else {
        match working_dir {
            Some(dir) => PathBuf::from(dir).join(file_path),
            None => PathBuf::from(file_path),
        }
    };
    
    // Check if file exists
    if !resolved_path.exists() {
        return VerificationStatus::Error(format!("File not found: {}", file_path));
    }
    
    // If no line numbers specified, file existence is enough
    if start_line.is_none() {
        return VerificationStatus::Verified;
    }
    
    // Read file and count lines
    let content = match std::fs::read_to_string(&resolved_path) {
        Ok(c) => c,
        Err(e) => return VerificationStatus::Error(format!("Cannot read file {}: {}", file_path, e)),
    };
    
    let line_count = content.lines().count() as u32;
    let check_line = end_line.unwrap_or_else(|| start_line.unwrap());
    
    if check_line > line_count {
        return VerificationStatus::Warning(format!(
            "Line {} exceeds file length ({} lines) in {}",
            check_line, line_count, file_path
        ));
    }
    
    VerificationStatus::Verified
}

/// Verify a test reference evidence (test function exists in file).
/// 
/// This checks that the test file exists and contains a function with the given name.
/// It does NOT run the test - just verifies it exists.
/// 
/// # Arguments
/// * `file_path` - Path to the test file (relative to working_dir or absolute)
/// * `test_name` - Name of the test function to find
/// * `working_dir` - Working directory for resolving relative paths
pub fn verify_test_reference(
    file_path: &str,
    test_name: &str,
    working_dir: Option<&str>,
) -> VerificationStatus {
    use std::path::Path;
    
    // Resolve the file path
    let resolved_path = if Path::new(file_path).is_absolute() {
        PathBuf::from(file_path)
    } else {
        match working_dir {
            Some(dir) => PathBuf::from(dir).join(file_path),
            None => PathBuf::from(file_path),
        }
    };
    
    // Check if file exists
    if !resolved_path.exists() {
        return VerificationStatus::Error(format!("Test file not found: {}", file_path));
    }
    
    // Read file content
    let content = match std::fs::read_to_string(&resolved_path) {
        Ok(c) => c,
        Err(e) => return VerificationStatus::Error(format!("Cannot read test file {}: {}", file_path, e)),
    };
    
    // Look for the test function
    // For Rust: look for `fn test_name` or `fn test_name(`
    // TODO: Add support for other languages (Python: def test_name, JS: test('name', etc.)
    let rust_pattern = format!("fn {}", test_name);
    let rust_pattern_with_paren = format!("fn {}(", test_name);
    
    if content.contains(&rust_pattern) || content.contains(&rust_pattern_with_paren) {
        return VerificationStatus::Verified;
    }
    
    // Check for #[test] attribute near the function name as a fallback
    // This handles cases where the function might be named differently
    if content.contains(test_name) && content.contains("#[test]") {
        return VerificationStatus::Verified;
    }
    
    VerificationStatus::Warning(format!(
        "Test function '{}' not found in {}",
        test_name, file_path
    ))
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
    
    // Extract YAML from markdown code block
    let yaml_content = extract_yaml_from_markdown(&content)?;
    
    let plan: Plan = serde_yaml::from_str(&yaml_content)?;
    Ok(Some(plan))
}

/// Write a plan to the session's plan.g3.md file.
pub fn write_plan(session_id: &str, plan: &Plan) -> Result<()> {
    // Validate before writing
    plan.validate()?;

    let _ = ensure_session_dir(session_id)?;
    let path = get_plan_path(session_id);

    // Format as markdown with YAML code block
    let content = format_plan_as_markdown(plan);
    
    std::fs::write(&path, content)?;
    Ok(())
}

/// Extract YAML content from a markdown file with ```yaml code block.
fn extract_yaml_from_markdown(content: &str) -> Result<String> {
    let start_marker = "```yaml";

    if let Some(start_idx) = content.find(start_marker) {
        let yaml_start = start_idx + start_marker.len();
        // Find closing ``` that appears at the start of a line.
        // A simple .find("```") would match backticks embedded inside YAML
        // string values (e.g., descriptions containing code fences), truncating
        // the YAML and causing parse errors.
        let remainder = &content[yaml_start..];
        for (i, line) in remainder.split('\n').enumerate() {
            if i > 0 && line.starts_with("```") {
                let offset: usize = remainder.split('\n').take(i).map(|l| l.len() + 1).sum();
                let yaml = remainder[..offset].trim();
                return Ok(yaml.to_string());
            }
        }
    }

    // If no code block, try parsing the whole content as YAML
    Ok(content.to_string())
}

/// Format a plan as markdown with embedded YAML.
fn format_plan_as_markdown(plan: &Plan) -> String {
    let yaml = serde_yaml::to_string(plan).unwrap_or_else(|_| "# Error serializing plan".to_string());
    
    let mut md = String::new();
    md.push_str(&format!("# Plan: {}\n\n", plan.plan_id));
    md.push_str(&format!("**Status**: {}\n\n", plan.status_summary()));
    md.push_str("## Plan Data\n\n");
    md.push_str("```yaml\n");
    md.push_str(&yaml);
    md.push_str("```\n");
    
    md
}

// ============================================================================
// Plan Verification
// ============================================================================

/// Verify a single piece of evidence.
fn verify_single_evidence(evidence: &str, working_dir: Option<&str>) -> EvidenceVerification {
    let evidence_type = parse_evidence(evidence);
    
    let status = match &evidence_type {
        EvidenceType::CodeLocation { file_path, start_line, end_line } => {
            verify_code_location(file_path, *start_line, *end_line, working_dir)
        }
        EvidenceType::TestReference { file_path, test_name } => {
            verify_test_reference(file_path, test_name, working_dir)
        }
        EvidenceType::Unknown(s) => {
            VerificationStatus::Skipped(format!("Unknown evidence format: {}", s))
        }
    };
    
    EvidenceVerification {
        evidence: evidence.to_string(),
        evidence_type,
        status,
    }
}

/// Verify a completed plan. Called when all items are done/blocked.
/// 
/// Returns a PlanVerification with results for each done item's evidence.
/// Blocked items are skipped (counted but not verified).
pub fn plan_verify(plan: &Plan, working_dir: Option<&str>) -> PlanVerification {
    let mut item_results = Vec::new();
    let mut skipped_count = 0;
    
    for item in &plan.items {
        match item.state {
            PlanState::Blocked => {
                skipped_count += 1;
                continue;
            }
            PlanState::Done => {
                // Verify this item's evidence
                let missing_evidence = item.evidence.is_empty();
                let evidence_results: Vec<EvidenceVerification> = item
                    .evidence
                    .iter()
                    .map(|e| verify_single_evidence(e, working_dir))
                    .collect();
                
                item_results.push(ItemVerification {
                    item_id: item.id.clone(),
                    description: item.description.clone(),
                    evidence_results,
                    missing_evidence,
                });
            }
            // Skip todo/doing items - they shouldn't be in a complete plan
            _ => {}
        }
    }
    
    PlanVerification {
        plan_id: plan.plan_id.clone(),
        item_results,
        skipped_count,
    }
}


/// Format verification results as a string for display.
/// Uses loud formatting for warnings and errors.
/// If session_id is provided, also checks that envelope.yaml exists at the expected path.
pub fn format_verification_results(verification: &PlanVerification, session_id: Option<&str>, _working_dir: Option<&Path>) -> String {
    let mut output = String::new();
    let (warnings, errors) = verification.count_issues();
    
    output.push_str("\n");
    output.push_str(&"═".repeat(60));
    output.push_str("\n");
    output.push_str("📋 PLAN VERIFICATION RESULTS\n");
    output.push_str(&"═".repeat(60));
    output.push_str("\n\n");
    
    for item in &verification.item_results {
        output.push_str(&format!("[{}] {}\n", item.item_id, item.description));
        
        if item.missing_evidence {
            output.push_str("  ⚠️  WARNING: No evidence provided for done item!\n");
        }
        
        for ev in &item.evidence_results {
            let status_str = match &ev.status {
                VerificationStatus::Verified => "✅".to_string(),
                VerificationStatus::Warning(msg) => format!("⚠️  WARNING: {}", msg),
                VerificationStatus::Error(msg) => format!("❌ ERROR: {}", msg),
                VerificationStatus::Skipped(msg) => format!("⏭️  SKIPPED: {}", msg),
            };
            output.push_str(&format!("  {} {}\n", status_str, ev.evidence));
        }
        output.push_str("\n");
    }
    
    if verification.skipped_count > 0 {
        output.push_str(&format!("ℹ️  {} blocked item(s) skipped\n\n", verification.skipped_count));
    }
    
    // Summary line
    if errors > 0 || warnings > 0 {
        output.push_str(&format!("⚠️  VERIFICATION COMPLETE: {} error(s), {} warning(s)\n", errors, warnings));
    } else {
        output.push_str("✅ VERIFICATION COMPLETE: All evidence validated\n");
    }
    
    // Print envelope location and run datalog verification if session_id provided
    if let Some(sid) = session_id {
        output.push_str("\n");
        output.push_str("📜 ARTIFACTS\n");

        let envelope_path = get_envelope_path(sid);
        let envelope_status = if envelope_path.exists() { "✅" } else { "⚠️  (not found)" };
        output.push_str(&format!("  {} Envelope: {}\n", envelope_status, envelope_path.display()));

        output.push_str("\n");

    }
    
    output.push_str(&"═".repeat(60));
    output.push_str("\n");
    
    output
}

// ============================================================================
// Tool Implementations
// ============================================================================

/// Execute the `plan_read` tool.
pub async fn execute_plan_read<W: UiWriter>(
    _tool_call: &ToolCall,
    ctx: &mut ToolContext<'_, W>,
) -> Result<String> {
    debug!("Processing plan_read tool call");

    let session_id = match ctx.session_id {
        Some(id) => id,
        None => return Ok("❌ No active session - plans are session-scoped.".to_string()),
    };

    let plan_path = get_plan_path(session_id);
    let plan_path_str = plan_path.to_string_lossy().to_string();

    match read_plan(session_id)? {
        Some(plan) => {
            let yaml = serde_yaml::to_string(&plan)?;
            ctx.ui_writer.print_plan_compact(Some(&yaml), Some(&plan_path_str), false);
            
            // Build output with plan
            let mut output = format!(
                "📋 {}\n\n```yaml\n{}```",
                plan.status_summary(),
                yaml
            );
            
            // Append envelope if present
            match read_envelope(session_id) {
                Ok(Some(envelope)) => output.push_str(&format_envelope_markdown(&envelope)),
                _ => output.push_str("\n_No envelope generated._\n"),
            }
            
            Ok(output)
        }
        None => {
            ctx.ui_writer.print_plan_compact(None, None, false);
            Ok("No plan exists yet. Use plan_write to create one.".to_string())
        }
    }
}

/// Execute the `plan_write` tool.
pub async fn execute_plan_write<W: UiWriter>(
    tool_call: &ToolCall,
    ctx: &mut ToolContext<'_, W>,
) -> Result<String> {
    debug!("Processing plan_write tool call");

    let session_id = match ctx.session_id {
        Some(id) => id,
        None => return Ok("❌ No active session - plans are session-scoped.".to_string()),
    };

    // Get the plan content from args
    let plan_yaml = match tool_call.args.get("plan").and_then(|v| v.as_str()) {
        Some(p) => p,
        None => return Ok("❌ Missing 'plan' argument. Provide the plan as YAML.".to_string()),
    };

    // Parse the YAML
    let mut plan: Plan = match serde_yaml::from_str(plan_yaml) {
        Ok(p) => p,
        Err(e) => return Ok(format!("❌ Invalid plan YAML: {}", e)),
    };

    // Load existing plan to check if this is a new plan or an update
    let existing_plan = read_plan(session_id)?;

    if let Some(existing) = existing_plan {
        if existing.is_complete() {
            // Existing plan is fully complete (all items done/blocked).
            // Treat the incoming plan as a fresh plan — don't carry over
            // approved_revision or enforce item preservation.
            debug!(
                "Existing plan '{}' is complete — allowing fresh plan '{}'",
                existing.plan_id, plan.plan_id
            );
        } else {
            // Preserve approved_revision from existing plan
            plan.approved_revision = existing.approved_revision;
            // Increment revision
            plan.revision = existing.revision + 1;

            // If plan was approved, ensure checks are not removed
            if existing.is_approved() {
                // Verify all existing item IDs still exist
                for existing_item in &existing.items {
                    if !plan.items.iter().any(|i| i.id == existing_item.id) {
                        return Ok(format!(
                            "❌ Cannot remove item '{}' from approved plan. Items can only be marked blocked, not removed.",
                            existing_item.id
                        ));
                    }
                }
            }
        }
    }

    // Validate the plan
    if let Err(e) = plan.validate() {
        return Ok(format!("❌ Plan validation failed: {}", e));
    }

    // Auto-approve in non-interactive (autonomous) mode
    if ctx.is_autonomous && !plan.is_approved() {
        plan.approve();
        debug!("Auto-approved plan in autonomous mode at revision {}", plan.revision);
    }

    // Write the plan
    if let Err(e) = write_plan(session_id, &plan) {
        return Ok(format!("❌ Failed to write plan: {}", e));
    }

    // Display the plan in compact format
    let plan_path = get_plan_path(session_id);
    let plan_path_str = plan_path.to_string_lossy().to_string();
    let yaml = serde_yaml::to_string(&plan)?;
    ctx.ui_writer.print_plan_compact(Some(&yaml), Some(&plan_path_str), true);

    // Read and format envelope if it exists
    let envelope_section = match read_envelope(session_id) {
        Ok(Some(envelope)) => format_envelope_markdown(&envelope),
        Ok(None) => "\n_No envelope generated._\n".to_string(),
        Err(_) => "\n_No envelope generated._\n".to_string(),
    };

    // Check if plan is now complete and trigger verification
    if plan.is_complete() && plan.is_approved() {
        let verification = plan_verify(&plan, ctx.working_dir);
        let verification_output = format_verification_results(&verification, ctx.session_id, ctx.working_dir.map(std::path::Path::new));
        return Ok(format!(
            "✅ Plan updated: {}\n{}\n{}",
            plan.status_summary(),
            verification_output,
            envelope_section
        ));
    }

    Ok(format!(
        "✅ Plan updated: {}\n{}",
        plan.status_summary(),
        envelope_section
    ))
}

/// Execute the `plan_approve` tool.
pub async fn execute_plan_approve<W: UiWriter>(
    _tool_call: &ToolCall,
    ctx: &mut ToolContext<'_, W>,
) -> Result<String> {
    debug!("Processing plan_approve tool call");

    let session_id = match ctx.session_id {
        Some(id) => id,
        None => return Ok("❌ No active session - plans are session-scoped.".to_string()),
    };

    // Load existing plan
    let mut plan = match read_plan(session_id)? {
        Some(p) => p,
        None => return Ok("❌ No plan exists to approve. Use plan_write first.".to_string()),
    };

    if plan.is_approved() {
        return Ok(format!(
            "ℹ️ Plan already approved at revision {}. Current revision: {}",
            plan.approved_revision.unwrap(),
            plan.revision
        ));
    }

    // Approve the plan
    plan.approve();

    // Write back
    if let Err(e) = write_plan(session_id, &plan) {
        return Ok(format!("❌ Failed to save approved plan: {}", e));
    }

    Ok(format!(
        "✅ Plan approved at revision {}. You may now begin implementation.",
        plan.revision
    ))
}

// ============================================================================
// Plan Approval Gate
// ============================================================================

/// Result of checking the plan approval gate after a tool execution.
#[derive(Debug)]
pub enum ApprovalGateResult {
    /// No plan exists, or plan is approved - allow the operation
    Allowed,
    /// Plan exists but not approved, and new files were changed - blocked (warn only, never revert)
    Blocked {
        /// Message to inject into the conversation
        message: String,
    },
    /// Not a git repository - skip the check
    NotGitRepo,
}

/// Get the set of dirty file paths from `git status --porcelain`.
///
/// Returns an empty set if not a git repo or if the command fails.
/// Each entry is the file path as reported by git (relative to repo root).
pub fn get_dirty_files(working_dir: Option<&str>) -> HashSet<String> {
    let dir = working_dir.unwrap_or(".");
    let output = std::process::Command::new("git")
        .args(["status", "--porcelain"])
        .current_dir(dir)
        .output();

    let output = match output {
        Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout).to_string(),
        _ => return HashSet::new(),
    };

    output
        .lines()
        .filter(|line| line.len() >= 3)
        .map(|line| line[3..].trim().to_string())
        .collect()
}

/// Check if file changes occurred without an approved plan.
///
/// This function should be called after each tool execution when in plan mode.
/// It checks `git status --porcelain` for changes (excluding any files that were
/// already dirty at baseline), and if a plan exists but isn't approved, returns a
/// blocking message. **Never reverts or deletes files.**
pub fn check_plan_approval_gate(
    session_id: &str,
    working_dir: Option<&str>,
    baseline_dirty: &HashSet<String>,
) -> ApprovalGateResult {
    let dir = working_dir.unwrap_or(".");

    // Check if this is a git repository
    let git_check = std::process::Command::new("git")
        .args(["rev-parse", "--git-dir"])
        .current_dir(dir)
        .output();

    if git_check.is_err() || !git_check.unwrap().status.success() {
        return ApprovalGateResult::NotGitRepo;
    }

    // Get current dirty files, excluding baseline
    let current_dirty = get_dirty_files(working_dir);
    let new_dirty: Vec<&String> = current_dirty
        .iter()
        .filter(|f| !baseline_dirty.contains(*f))
        .collect();

    // Check if a plan exists and whether it's approved
    let plan = match read_plan(session_id) {
        Ok(Some(plan)) => plan,
        Ok(None) => {
            if new_dirty.is_empty() {
                return ApprovalGateResult::Allowed;
            }

            let files_list = new_dirty
                .iter()
                .map(|f| format!("  - {}", f))
                .collect::<Vec<_>>()
                .join("\n");

            return ApprovalGateResult::Blocked {
                message: format!(
                    "⚠️ IMPLEMENTATION BLOCKED\n\n\
                     File changes detected without a plan:\n\
                     {}\n\n\
                     Before implementing, you must:\n\
                     1. Create a plan with `plan_write`\n\
                     2. Get the plan approved by the user\n\n\
                     Do not attempt to implement until the plan is approved.",
                    files_list
                ),
            };
        }
        Err(_) => return ApprovalGateResult::Allowed, // Can't read plan, allow (error case)
    };

    if plan.is_approved() {
        return ApprovalGateResult::Allowed;
    }

    if new_dirty.is_empty() {
        return ApprovalGateResult::Allowed;
    }

    let files_list = new_dirty
        .iter()
        .map(|f| format!("  - {}", f))
        .collect::<Vec<_>>()
        .join("\n");

    let message = format!(
        "⚠️ IMPLEMENTATION BLOCKED\n\n\
         File changes detected without an approved plan:\n\
         {}\n\n\
         Before implementing, you must:\n\
         1. Create a plan with `plan_write`\n\
         2. Request the user's explicit approval or edits to plan\n\n\
         Do not attempt to implement until the plan is approved.",
        files_list
    );

    ApprovalGateResult::Blocked {
        message,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_test_check() -> Check {
        Check::new("Test description", "test::target")
    }

    fn make_test_checks() -> Checks {
        Checks {
            happy: make_test_check(),
            negative: vec![make_test_check()],
            boundary: vec![make_test_check()],
        }
    }

    fn make_test_item(id: &str) -> PlanItem {
        PlanItem::new(
            id,
            "Test item description",
            vec!["src/test.rs".to_string()],
            make_test_checks(),
        )
    }

    #[test]
    fn test_plan_state_display() {
        assert_eq!(PlanState::Todo.to_string(), "todo");
        assert_eq!(PlanState::Doing.to_string(), "doing");
        assert_eq!(PlanState::Done.to_string(), "done");
        assert_eq!(PlanState::Blocked.to_string(), "blocked");
    }

    #[test]
    fn test_plan_state_from_str() {
        assert_eq!("todo".parse::<PlanState>().unwrap(), PlanState::Todo);
        assert_eq!("DOING".parse::<PlanState>().unwrap(), PlanState::Doing);
        assert_eq!("Done".parse::<PlanState>().unwrap(), PlanState::Done);
        assert!("invalid".parse::<PlanState>().is_err());
    }

    #[test]
    fn test_check_validation() {
        let valid = Check::new("desc", "target");
        assert!(valid.validate().is_ok());

        let empty_desc = Check::new("", "target");
        assert!(empty_desc.validate().is_err());

        let empty_target = Check::new("desc", "");
        assert!(empty_target.validate().is_err());
    }

    #[test]
    fn test_checks_validation_multiple_negative_and_boundary() {
        // Multiple negative and boundary checks should validate
        let checks = Checks {
            happy: make_test_check(),
            negative: vec![
                Check::new("Invalid input", "parse::input"),
                Check::new("Missing file", "io::read"),
                Check::new("Network error", "net::connect"),
            ],
            boundary: vec![
                Check::new("Empty input", "parse::input"),
                Check::new("Max size input", "parse::input"),
            ],
        };
        assert!(checks.validate().is_ok());
    }

    #[test]
    fn test_checks_validation_empty_negative_fails() {
        let checks = Checks {
            happy: make_test_check(),
            negative: vec![],  // Empty - should fail
            boundary: vec![make_test_check()],
        };
        let err = checks.validate().unwrap_err();
        assert!(err.to_string().contains("at least one negative check"));
    }

    #[test]
    fn test_checks_validation_empty_boundary_fails() {
        let checks = Checks {
            happy: make_test_check(),
            negative: vec![make_test_check()],
            boundary: vec![],  // Empty - should fail
        };
        let err = checks.validate().unwrap_err();
        assert!(err.to_string().contains("at least one boundary check"));
    }

    #[test]
    fn test_checks_validation_single_of_each_passes() {
        // Minimum case: exactly 1 of each
        let checks = make_test_checks();
        assert!(checks.validate().is_ok());
        assert_eq!(checks.negative.len(), 1);
        assert_eq!(checks.boundary.len(), 1);
    }

    #[test]
    fn test_plan_item_validation() {
        let item = make_test_item("I1");
        assert!(item.validate().is_ok());

        // Done item without evidence should fail
        let mut done_item = make_test_item("I2");
        done_item.state = PlanState::Done;
        assert!(done_item.validate().is_err());

        // Done item with evidence but no notes should fail
        done_item.evidence = vec!["src/test.rs:42".to_string()];
        assert!(done_item.validate().is_err());

        // Done item with evidence and notes should pass
        done_item.notes = Some("Implementation notes".to_string());
        assert!(done_item.validate().is_ok());
    }

    #[test]
    fn test_plan_validation() {
        let mut plan = Plan::new("test-plan");
        
        // Empty plan should fail
        assert!(plan.validate().is_err());

        // Plan with item should pass
        plan.items.push(make_test_item("I1"));
        assert!(plan.validate().is_ok());

        // Duplicate IDs should fail
        plan.items.push(make_test_item("I1"));
        assert!(plan.validate().is_err());
    }

    #[test]
    fn test_plan_is_complete() {
        let mut plan = Plan::new("test");
        plan.items.push(make_test_item("I1"));
        plan.items.push(make_test_item("I2"));

        assert!(!plan.is_complete());

        plan.items[0].state = PlanState::Done;
        plan.items[0].evidence = vec!["test".to_string()];
        plan.items[0].notes = Some("notes".to_string());
        assert!(!plan.is_complete());

        plan.items[1].state = PlanState::Blocked;
        assert!(plan.is_complete());
    }

    #[test]
    fn test_plan_approval() {
        let mut plan = Plan::new("test");
        plan.items.push(make_test_item("I1"));

        assert!(!plan.is_approved());
        assert_eq!(plan.approved_revision, None);

        plan.approve();
        assert!(plan.is_approved());
        assert_eq!(plan.approved_revision, Some(1));
    }

    #[test]
    fn test_yaml_extraction() {
        let md = r#"# Plan: test

**Status**: ...

## Plan Data

```yaml
plan_id: test
revision: 1
items: []
```
"#;

        let yaml = extract_yaml_from_markdown(md).unwrap();
        assert!(yaml.contains("plan_id: test"));
    }

    #[test]
    fn test_yaml_extraction_with_backticks_in_values() {
        // This is the exact bug: YAML values containing ``` caused
        // extract_yaml_from_markdown to truncate at the embedded backticks
        // instead of finding the real closing fence.
        let md = "# Plan: test\n\n## Plan Data\n\n\
```yaml\n\
plan_id: test\n\
revision: 1\n\
items:\n\
  - id: I1\n\
    description: 'Fix the ```yaml parsing issue with ```'\n\
    state: todo\n\
    touches:\n\
      - src/plan.rs\n\
    checks:\n\
      happy:\n\
        desc: Works\n\
        target: plan\n\
      negative:\n\
        - desc: Fails gracefully\n\
          target: plan\n\
      boundary:\n\
        - desc: Edge case\n\
          target: plan\n\
```\n";

        let yaml = extract_yaml_from_markdown(md).unwrap();
        // Must contain the full YAML, not truncated at the embedded backticks
        assert!(yaml.contains("plan_id: test"), "should contain plan_id");
        assert!(yaml.contains("description:"), "should contain description field");
        assert!(yaml.contains("state: todo"), "should contain state field");
        assert!(yaml.contains("checks:"), "should contain checks");
    }

    #[test]
    fn test_yaml_extraction_no_code_block_fallback() {
        let raw_yaml = "plan_id: test\nrevision: 1\nitems: []\n";
        let yaml = extract_yaml_from_markdown(raw_yaml).unwrap();
        assert_eq!(yaml, raw_yaml);
    }

    #[test]
    fn test_yaml_extraction_closing_fence_no_trailing_newline() {
        let md = "```yaml\nplan_id: test\nrevision: 1\nitems: []\n```";
        let yaml = extract_yaml_from_markdown(md).unwrap();
        assert!(yaml.contains("plan_id: test"));
    }

    #[test]
    fn test_plan_serialization_roundtrip() {
        let mut plan = Plan::new("test-plan");
        plan.items.push(make_test_item("I1"));
        plan.approve();

        let yaml = serde_yaml::to_string(&plan).unwrap();
        let parsed: Plan = serde_yaml::from_str(&yaml).unwrap();

        assert_eq!(parsed.plan_id, plan.plan_id);
        assert_eq!(parsed.revision, plan.revision);
        assert_eq!(parsed.approved_revision, plan.approved_revision);
        assert_eq!(parsed.items.len(), plan.items.len());
    }

    // ========================================================================
    // Evidence Parsing Tests
    // ========================================================================

    #[test]
    fn test_parse_evidence_code_location_with_line_range() {
        let evidence = "src/foo.rs:42-118";
        match parse_evidence(evidence) {
            EvidenceType::CodeLocation { file_path, start_line, end_line } => {
                assert_eq!(file_path, "src/foo.rs");
                assert_eq!(start_line, Some(42));
                assert_eq!(end_line, Some(118));
            }
            _ => panic!("Expected CodeLocation"),
        }
    }

    #[test]
    fn test_parse_evidence_code_location_single_line() {
        let evidence = "src/bar.rs:99";
        match parse_evidence(evidence) {
            EvidenceType::CodeLocation { file_path, start_line, end_line } => {
                assert_eq!(file_path, "src/bar.rs");
                assert_eq!(start_line, Some(99));
                assert_eq!(end_line, None);
            }
            _ => panic!("Expected CodeLocation"),
        }
    }

    #[test]
    fn test_parse_evidence_code_location_file_only() {
        let evidence = "src/lib.rs";
        match parse_evidence(evidence) {
            EvidenceType::CodeLocation { file_path, start_line, end_line } => {
                assert_eq!(file_path, "src/lib.rs");
                assert_eq!(start_line, None);
                assert_eq!(end_line, None);
            }
            _ => panic!("Expected CodeLocation"),
        }
    }

    #[test]
    fn test_parse_evidence_test_reference() {
        let evidence = "tests/integration.rs::test_happy_path";
        match parse_evidence(evidence) {
            EvidenceType::TestReference { file_path, test_name } => {
                assert_eq!(file_path, "tests/integration.rs");
                assert_eq!(test_name, "test_happy_path");
            }
            _ => panic!("Expected TestReference"),
        }
    }

    #[test]
    fn test_parse_evidence_unknown_format() {
        let evidence = "some random text";
        match parse_evidence(evidence) {
            EvidenceType::Unknown(s) => {
                assert_eq!(s, "some random text");
            }
            _ => panic!("Expected Unknown"),
        }
    }

    #[test]
    fn test_parse_evidence_whitespace_trimmed() {
        let evidence = "  src/foo.rs:42  ";
        match parse_evidence(evidence) {
            EvidenceType::CodeLocation { file_path, start_line, .. } => {
                assert_eq!(file_path, "src/foo.rs");
                assert_eq!(start_line, Some(42));
            }
            _ => panic!("Expected CodeLocation"),
        }
    }

    // ========================================================================
    // Verification Tests
    // ========================================================================

    #[test]
    fn test_verify_code_location_file_exists() {
        // Use Cargo.toml which should always exist in the workspace
        let status = verify_code_location("Cargo.toml", None, None, None);
        assert!(matches!(status, VerificationStatus::Verified));
    }

    #[test]
    fn test_verify_code_location_file_not_found() {
        let status = verify_code_location("nonexistent_file_xyz.rs", None, None, None);
        match status {
            VerificationStatus::Error(msg) => {
                assert!(msg.contains("not found"));
            }
            _ => panic!("Expected Error"),
        }
    }

    #[test]
    fn test_verify_code_location_line_out_of_range() {
        // Cargo.toml exists but probably doesn't have 999999 lines
        let status = verify_code_location("Cargo.toml", Some(1), Some(999999), None);
        match status {
            VerificationStatus::Warning(msg) => {
                assert!(msg.contains("exceeds file length"));
            }
            _ => panic!("Expected Warning, got {:?}", status),
        }
    }

    #[test]
    fn test_verify_test_reference_file_not_found() {
        let status = verify_test_reference("nonexistent_test.rs", "test_foo", None);
        match status {
            VerificationStatus::Error(msg) => {
                assert!(msg.contains("not found"));
            }
            _ => panic!("Expected Error"),
        }
    }

    #[test]
    fn test_verification_status_helpers() {
        assert!(VerificationStatus::Verified.is_ok());
        assert!(VerificationStatus::Skipped("reason".to_string()).is_ok());
        assert!(!VerificationStatus::Warning("warn".to_string()).is_ok());
        assert!(!VerificationStatus::Error("err".to_string()).is_ok());

        assert!(!VerificationStatus::Verified.is_warning_or_error());
        assert!(VerificationStatus::Warning("warn".to_string()).is_warning_or_error());
        assert!(VerificationStatus::Error("err".to_string()).is_warning_or_error());
    }

    #[test]
    fn test_approval_gate_no_plan_no_changes() {
        // With a non-existent session and no uncommitted changes, it should allow.
        // We use a temp dir that's a fresh git repo with no changes.
        let temp_dir = tempfile::tempdir().unwrap();
        std::process::Command::new("git")
            .args(["init"])
            .current_dir(temp_dir.path())
            .output()
            .unwrap();
        let result = check_plan_approval_gate("nonexistent-session-xyz", Some(temp_dir.path().to_str().unwrap()), &HashSet::new());
        assert!(matches!(result, ApprovalGateResult::Allowed));
    }

    #[test]
    fn test_approval_gate_no_plan_with_changes() {
        // With a non-existent session but uncommitted changes, it should block.
        let temp_dir = tempfile::tempdir().unwrap();
        std::process::Command::new("git")
            .args(["init"])
            .current_dir(temp_dir.path())
            .output()
            .unwrap();
        // Create an untracked file to simulate changes
        std::fs::write(temp_dir.path().join("new_file.txt"), "test content").unwrap();
        
        let result = check_plan_approval_gate("nonexistent-session-xyz", Some(temp_dir.path().to_str().unwrap()), &HashSet::new());
        assert!(matches!(result, ApprovalGateResult::Blocked { .. }));
        
        // Verify the blocking message mentions creating a plan
        if let ApprovalGateResult::Blocked { message } = result {
            assert!(message.contains("plan_write"));
        }
    }

    #[test]
    fn test_approval_gate_not_git_repo() {
        // /tmp is typically not a git repo
        let result = check_plan_approval_gate("any-session", Some("/tmp"), &HashSet::new());
        assert!(matches!(result, ApprovalGateResult::NotGitRepo));
    }

    #[test]
    fn test_approval_gate_warns_without_reverting() {
        // Dirty files should appear in the warning message but NOT be deleted/reverted.
        let temp_dir = tempfile::tempdir().unwrap();
        std::process::Command::new("git")
            .args(["init"])
            .current_dir(temp_dir.path())
            .output()
            .unwrap();
        // Create an untracked file
        let file_path = temp_dir.path().join("should_survive.txt");
        std::fs::write(&file_path, "precious content").unwrap();

        let result = check_plan_approval_gate(
            "nonexistent-session-xyz",
            Some(temp_dir.path().to_str().unwrap()),
            &HashSet::new(),
        );
        assert!(matches!(result, ApprovalGateResult::Blocked { .. }));

        // The file must still exist on disk — gate must NOT delete it
        assert!(file_path.exists(), "Gate must not delete untracked files");
        assert_eq!(
            std::fs::read_to_string(&file_path).unwrap(),
            "precious content"
        );
    }

    #[test]
    fn test_approval_gate_excludes_baseline() {
        // Files in the baseline set should be excluded from the gate check.
        let temp_dir = tempfile::tempdir().unwrap();
        std::process::Command::new("git")
            .args(["init"])
            .current_dir(temp_dir.path())
            .output()
            .unwrap();
        // Create a file that will be in the baseline
        std::fs::write(temp_dir.path().join("pre_existing.txt"), "old content").unwrap();

        // Baseline includes this file
        let baseline: HashSet<String> = ["pre_existing.txt".to_string()].into_iter().collect();

        let result = check_plan_approval_gate(
            "nonexistent-session-xyz",
            Some(temp_dir.path().to_str().unwrap()),
            &baseline,
        );
        // Only baseline files are dirty → should be Allowed
        assert!(matches!(result, ApprovalGateResult::Allowed));
    }

    #[test]
    fn test_approval_gate_blocks_new_files_with_baseline() {
        // Baseline files are excluded, but new files should still trigger blocking.
        let temp_dir = tempfile::tempdir().unwrap();
        std::process::Command::new("git")
            .args(["init"])
            .current_dir(temp_dir.path())
            .output()
            .unwrap();
        // Pre-existing file (in baseline)
        std::fs::write(temp_dir.path().join("pre_existing.txt"), "old").unwrap();
        // New file (not in baseline)
        std::fs::write(temp_dir.path().join("new_file.txt"), "new").unwrap();

        let baseline: HashSet<String> = ["pre_existing.txt".to_string()].into_iter().collect();

        let result = check_plan_approval_gate(
            "nonexistent-session-xyz",
            Some(temp_dir.path().to_str().unwrap()),
            &baseline,
        );
        assert!(matches!(result, ApprovalGateResult::Blocked { .. }));

        if let ApprovalGateResult::Blocked { message } = result {
            // Should mention the new file but NOT the baseline file
            assert!(message.contains("new_file.txt"), "Should mention new file");
            assert!(!message.contains("pre_existing.txt"), "Should NOT mention baseline file");
        }

        // Both files must still exist
        assert!(temp_dir.path().join("pre_existing.txt").exists());
        assert!(temp_dir.path().join("new_file.txt").exists());
    }

    #[test]
    fn test_get_dirty_files_returns_file_paths() {
        let temp_dir = tempfile::tempdir().unwrap();
        std::process::Command::new("git")
            .args(["init"])
            .current_dir(temp_dir.path())
            .output()
            .unwrap();
        std::fs::write(temp_dir.path().join("a.txt"), "a").unwrap();
        std::fs::write(temp_dir.path().join("b.txt"), "b").unwrap();

        let dirty = get_dirty_files(Some(temp_dir.path().to_str().unwrap()));
        assert!(dirty.contains("a.txt"));
        assert!(dirty.contains("b.txt"));
        assert!(!dirty.contains("c.txt"));
    }

    #[test]
    fn test_get_dirty_files_non_git_repo() {
        // Non-git directory should return empty set without error
        let temp_dir = tempfile::tempdir().unwrap();
        let dirty = get_dirty_files(Some(temp_dir.path().to_str().unwrap()));
        assert!(dirty.is_empty());
    }
}
