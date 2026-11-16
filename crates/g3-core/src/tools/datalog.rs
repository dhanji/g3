//! Datalog-based invariant verification using datafrog.
//!
//! This module compiles rulespecs into datalog relations and executes them
//! against facts extracted from action envelopes. It provides a more rigorous
//! verification mechanism than the simple predicate evaluation in invariants.rs.
//!
//! ## Architecture
//!
//! 1. **Compilation Phase** (on-the-fly at plan_verify):
//!    - Parse rulespec claims and predicates
//!    - Generate datafrog relations and rules
//!    - Rulespec is read from `analysis/rulespec.yaml`
//!
//! 2. **Execution Phase** (on plan_verify):
//!    - Extract facts from action envelope using selectors
//!    - Inject facts into datafrog relations
//!    - Run datalog to fixed point
//!    - Collect and format results
//!
//! ## Datalog Mapping
//!
//! Claims become base relations:
//! ```text
//! claim_value(claim_name: String, value: String)
//! ```
//!
//! Predicates become rules that derive pass/fail:
//! ```text
//! predicate_pass(pred_id) :- claim_value(claim, expected_value)
//! ```

use anyhow::{anyhow, Result};
use datafrog::{Iteration, Relation};
use serde::{Deserialize, Serialize};
use serde_yaml::Value as YamlValue;
use std::collections::{HashMap, HashSet};

use super::invariants::{
    ActionEnvelope, InvariantSource, PredicateRule, Rulespec, Selector,
};
#[cfg(test)]
use super::invariants::{Claim, Predicate};


// ============================================================================
// Compiled Datalog Representation
// ============================================================================

/// A compiled predicate ready for datalog execution.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompiledPredicate {
    /// Unique ID for this predicate (index in original rulespec)
    pub id: usize,
    /// Name of the claim this predicate references
    pub claim_name: String,
    /// The selector path from the claim
    pub selector: String,
    /// The rule type
    pub rule: PredicateRule,
    /// Expected value (serialized as string for datalog)
    pub expected_value: Option<String>,
    /// Source of this invariant
    pub source: InvariantSource,
    /// Optional notes
    pub notes: Option<String>,
    /// Optional when condition (compiled)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub when: Option<CompiledWhenCondition>,
}

/// A compiled when condition for datalog execution.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompiledWhenCondition {
    pub claim_name: String,
    pub selector: String,
    pub rule: PredicateRule,
    pub expected_value: Option<String>,
}

/// Compiled rulespec ready for datalog execution.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompiledRulespec {
    /// Original plan_id this was compiled for
    pub plan_id: String,
    /// Revision of the plan when compiled
    pub compiled_at_revision: u32,
    /// Compiled predicates
    pub predicates: Vec<CompiledPredicate>,
    /// Claim name -> selector mapping
    pub claims: HashMap<String, String>,
}

impl CompiledRulespec {
    /// Check if the compiled rulespec is empty.
    pub fn is_empty(&self) -> bool {
        self.predicates.is_empty()
    }
}

// ============================================================================
// Compilation: Rulespec -> Datalog
// ============================================================================

/// Compile a rulespec into a datalog-ready representation.
///
/// This validates the rulespec and converts it into a form that can be
/// efficiently executed by datafrog.
pub fn compile_rulespec(
    rulespec: &Rulespec,
    plan_id: &str,
    revision: u32,
) -> Result<CompiledRulespec> {
    // Build claim lookup
    let mut claims: HashMap<String, String> = HashMap::new();
    for claim in &rulespec.claims {
        // Validate selector syntax
        Selector::parse(&claim.selector).map_err(|e| {
            anyhow!(
                "Invalid selector '{}' in claim '{}': {}",
                claim.selector,
                claim.name,
                e
            )
        })?;
        claims.insert(claim.name.clone(), claim.selector.clone());
    }

    // Compile predicates
    let mut compiled_predicates = Vec::new();
    for (idx, predicate) in rulespec.predicates.iter().enumerate() {
        // Verify claim exists
        let selector = claims.get(&predicate.claim).ok_or_else(|| {
            anyhow!(
                "Predicate {} references unknown claim '{}'",
                idx,
                predicate.claim
            )
        })?;

        // Convert value to string representation for datalog
        let expected_value = predicate.value.as_ref().map(yaml_value_to_string);

        compiled_predicates.push(CompiledPredicate {
            id: idx,
            claim_name: predicate.claim.clone(),
            selector: selector.clone(),
            rule: predicate.rule.clone(),
            expected_value,
            source: predicate.source,
            notes: predicate.notes.clone(),
            when: predicate.when.as_ref().map(|w| {
                let when_selector = claims.get(&w.claim).cloned().unwrap_or_default();
                CompiledWhenCondition {
                    claim_name: w.claim.clone(),
                    selector: when_selector,
                    rule: w.rule.clone(),
                    expected_value: w.value.as_ref().map(yaml_value_to_string),
                }
            }),
        });
    }

    Ok(CompiledRulespec {
        plan_id: plan_id.to_string(),
        compiled_at_revision: revision,
        predicates: compiled_predicates,
        claims,
    })
}

/// Convert a YAML value to a string for datalog comparison.
fn yaml_value_to_string(value: &YamlValue) -> String {
    match value {
        YamlValue::Null => "null".to_string(),
        YamlValue::Bool(b) => b.to_string(),
        YamlValue::Number(n) => n.to_string(),
        YamlValue::String(s) => s.clone(),
        YamlValue::Sequence(seq) => {
            // For sequences, we'll handle them specially in execution
            format!(
                "[{}]",
                seq.iter()
                    .map(yaml_value_to_string)
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        }
        YamlValue::Mapping(_) => "{object}".to_string(),
        YamlValue::Tagged(t) => format!("!{}", t.tag),
    }
}

// ============================================================================
// Fact Extraction: Envelope -> Datalog Facts
// ============================================================================

/// A fact extracted from the envelope for datalog processing.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Fact {
    /// The claim name this fact belongs to
    pub claim_name: String,
    /// The extracted value as a string
    pub value: String,
}

/// Extract facts from an action envelope using the compiled rulespec's selectors.
///
/// Returns a set of facts that can be injected into datafrog relations.
pub fn extract_facts(envelope: &ActionEnvelope, compiled: &CompiledRulespec) -> HashSet<Fact> {
    let mut facts = HashSet::new();
    let envelope_value = envelope.to_yaml_value();
    // Build a "facts"-wrapped version so selectors with a "facts." prefix also work.
    let mut wrapped = serde_yaml::Mapping::new();
    wrapped.insert(YamlValue::String("facts".into()), envelope_value.clone());
    let wrapped_value = YamlValue::Mapping(wrapped);

    for (claim_name, selector_str) in &compiled.claims {
        let selector = match Selector::parse(selector_str) {
            Ok(s) => s,
            Err(_) => continue, // Skip invalid selectors (shouldn't happen after compilation)
        };

        let values = selector.select(&envelope_value);

        // If the selector didn't match anything on the unwrapped value,
        // try against the "facts"-wrapped version. This handles rulespec
        // selectors written as "facts.feature.done" when the envelope
        // stores facts without the "facts" wrapper key.
        let values = if values.is_empty() {
            selector.select(&wrapped_value)
        } else {
            values
        };

        for value in values {
            // Extract individual values from the selected result
            extract_values_recursive(claim_name, &value, &mut facts);
        }
    }

    facts
}

/// Recursively extract values from a YAML value into facts.
fn extract_values_recursive(claim_name: &str, value: &YamlValue, facts: &mut HashSet<Fact>) {
    match value {
        YamlValue::Sequence(seq) => {
            // For arrays, add each element as a separate fact
            for item in seq {
                extract_values_recursive(claim_name, item, facts);
            }
            // Also add the array length as a special fact
            facts.insert(Fact {
                claim_name: format!("{}.__length", claim_name),
                value: seq.len().to_string(),
            });
        }
        YamlValue::Mapping(map) => {
            // For objects, add a marker that it exists
            facts.insert(Fact {
                claim_name: claim_name.to_string(),
                value: "{object}".to_string(),
            });
            // And recurse into each field
            for (k, v) in map {
                let key_str = yaml_value_to_string(k);
                let nested_claim = format!("{}.{}", claim_name, key_str);
                extract_values_recursive(&nested_claim, v, facts);
            }
        }
        YamlValue::Null => {
            // Null values are intentionally NOT inserted as facts.
            // This ensures `exists` returns false and `not_exists` returns true
            // for null envelope values (e.g., `breaking_changes: null`).
        }
        _ => {
            // Scalar values
            facts.insert(Fact {
                claim_name: claim_name.to_string(),
                value: yaml_value_to_string(value),
            });
        }
    }
}

// ============================================================================
// Datalog Execution
// ============================================================================

/// Result of evaluating a single predicate via datalog.
#[derive(Debug, Clone)]
pub struct DatalogPredicateResult {
    /// Predicate ID
    pub id: usize,
    /// Claim name
    pub claim_name: String,
    /// Rule type
    pub rule: PredicateRule,
    /// Expected value (if any)
    pub expected_value: Option<String>,
    /// Whether the predicate passed
    pub passed: bool,
    /// Human-readable reason
    pub reason: String,
    /// Source of the invariant
    pub source: InvariantSource,
    /// Notes from the predicate
    pub notes: Option<String>,
}

/// Result of executing all datalog rules.
#[derive(Debug, Clone)]
pub struct DatalogExecutionResult {
    /// Results for each predicate
    pub predicate_results: Vec<DatalogPredicateResult>,
    /// Number of facts extracted from envelope
    pub fact_count: usize,
    /// Number of predicates that passed
    pub passed_count: usize,
    /// Number of predicates that failed
    pub failed_count: usize,
}

impl DatalogExecutionResult {
    /// Check if all predicates passed.
    pub fn all_passed(&self) -> bool {
        self.failed_count == 0
    }
}

/// Execute compiled datalog rules against extracted facts.
///
/// This uses datafrog to evaluate the predicates. The execution model:
/// 1. Create relations for claim values
/// 2. For each predicate, check if the required facts exist
/// 3. Collect pass/fail results
pub fn execute_rules(
    compiled: &CompiledRulespec,
    facts: &HashSet<Fact>,
) -> DatalogExecutionResult {
    let mut predicate_results = Vec::new();
    let mut passed_count = 0;
    let mut failed_count = 0;

    // Build a lookup for quick fact checking
    let fact_lookup: HashMap<&str, HashSet<&str>> = {
        let mut lookup: HashMap<&str, HashSet<&str>> = HashMap::new();
        for fact in facts {
            lookup
                .entry(fact.claim_name.as_str())
                .or_default()
                .insert(fact.value.as_str());
        }
        lookup
    };

    // Use datafrog for the core evaluation
    // We model this as: for each predicate, check if the required relation holds
    let mut iteration = Iteration::new();

    // Create a relation of all (claim_name, value) pairs
    let claim_values: Relation<(String, String)> = facts
        .iter()
        .map(|f| (f.claim_name.clone(), f.value.clone()))
        .collect();

    // Variable to hold claim values during iteration
    let claim_var = iteration.variable::<(String, String)>("claim_values");
    claim_var.extend(claim_values.iter().cloned());

    // Run to fixed point (trivial in this case since we have no recursive rules)
    while iteration.changed() {
        // No recursive rules, so this completes immediately
    }

    // Now evaluate each predicate
    for pred in &compiled.predicates {
        // Check when condition — if present and not met, skip (vacuous pass)
        let result = if let Some(when) = &pred.when {
            // Build a synthetic predicate to evaluate the when condition
            // using the same logic as regular predicate evaluation.
            let when_pred = CompiledPredicate {
                id: usize::MAX, // sentinel — not a real predicate
                claim_name: when.claim_name.clone(),
                selector: when.selector.clone(),
                rule: when.rule.clone(),
                expected_value: when.expected_value.clone(),
                source: pred.source,
                notes: None,
                when: None,
            };
            let when_met = evaluate_predicate_datalog(&when_pred, &fact_lookup).passed;
            if !when_met {
                DatalogPredicateResult {
                    id: pred.id,
                    claim_name: pred.claim_name.clone(),
                    rule: pred.rule.clone(),
                    expected_value: pred.expected_value.clone(),
                    passed: true,
                    reason: "Skipped (when condition not met)".to_string(),
                    source: pred.source,
                    notes: pred.notes.clone(),
                }
            } else {
                evaluate_predicate_datalog(pred, &fact_lookup)
            }
        } else {
            evaluate_predicate_datalog(pred, &fact_lookup)
        };
        
        if result.passed {
            passed_count += 1;
        } else {
            failed_count += 1;
        }
        
        predicate_results.push(result);
    }

    DatalogExecutionResult {
        predicate_results,
        fact_count: facts.len(),
        passed_count,
        failed_count,
    }
}

// ── Predicate evaluation helpers ────────────────────────────────────────
// Each returns (passed: bool, reason: String) for a specific rule type.

/// Exists / NotExists: check whether the claim has any values.
/// `expect_present = true` → Exists; `false` → NotExists.
fn eval_existence(claim_values: Option<&HashSet<&str>>, expect_present: bool) -> (bool, String) {
    let has_values = claim_values.map_or(false, |v| !v.is_empty());
    if expect_present == has_values {
        if has_values {
            (true, "Value exists".into())
        } else {
            (true, "Value does not exist as expected".into())
        }
    } else if expect_present {
        (false, "Value does not exist".into())
    } else {
        (false, "Value exists but should not".into())
    }
}

/// The bare, lowercased addresses in a recipient-shaped value:
/// `a@b.com`, `Name <a@b.com>`, and comma/semicolon-joined lists of either.
/// Parts without an `@` are not addresses and are dropped.
fn addresses_in(value: &str) -> Vec<String> {
    value
        .split(|c| c == ',' || c == ';')
        .filter_map(|part| {
            let part = part.trim();
            let bare = match (part.rfind('<'), part.rfind('>')) {
                (Some(l), Some(r)) if l < r => &part[l + 1..r],
                _ => part,
            };
            let bare = bare.trim().trim_matches(|c| c == '"' || c == '\'').to_lowercase();
            if bare.contains('@') { Some(bare) } else { None }
        })
        .collect()
}

/// Does blocklist `entry` catch any address inside `value`?
///
/// An entry WITH an `@` is a whole address (case-folded, exact). An entry
/// WITHOUT one is a DOMAIN, matching that domain and its subdomains.
/// ⚠️ NOT a substring test: `r@wideplay.com` must not catch
/// `sender@wideplay.com`, nor `rethrick.com` catch `notrethrick.com`.
/// Mirrors butler.app/email_send_api.py `_blocked_addresses`.
fn address_blocked(value: &str, entry: &str) -> bool {
    let e = entry.trim().to_lowercase();
    if e.is_empty() {
        return false;
    }
    addresses_in(value).iter().any(|addr| {
        if e.contains('@') {
            *addr == e
        } else {
            let domain = addr.rsplit('@').next().unwrap_or("");
            domain == e || domain.ends_with(&format!(".{}", e))
        }
    })
}

/// Contains / NotContains: check whether a specific value is among the claim's facts.
/// `expect_present = true` → Contains; `false` → NotContains.
///
/// NotContains is ADDRESS-AWARE on top of exact membership (a strict superset,
/// so it can only refuse more): a value holding email addresses is also
/// refused when `address_blocked` catches it. Exact membership alone let
/// `R@x.com`, `Name <a@b>`, comma-joined lists and every address at a blocked
/// domain through a recipient blocklist (red-team, 2026-11-16). Contains is
/// deliberately left exact — loosening a POSITIVE check would pass more.
fn eval_membership(
    claim_values: Option<&HashSet<&str>>,
    pred: &CompiledPredicate,
    expect_present: bool,
) -> (bool, String) {
    let expected = pred.expected_value.as_deref().unwrap_or("");
    match claim_values {
        Some(values) => {
            let found = values.contains(expected);
            if expect_present {
                if found {
                    (true, format!("Contains '{}'", expected))
                } else {
                    (false, format!("Does not contain '{}'", expected))
                }
            } else if found {
                (false, format!("Contains '{}' but should not", expected))
            } else if let Some(hit) = values.iter().find(|v| address_blocked(v, expected)) {
                (false, format!("'{}' is blocked by '{}'", hit, expected))
            } else {
                (true, format!("Does not contain '{}'", expected))
            }
        }
        None if expect_present => (false, format!("Claim '{}' has no values", pred.claim_name)),
        None => (true, format!("Claim '{}' has no values (not_contains passes vacuously)", pred.claim_name)),
    }
}

/// Equals: exactly one value must match the expected string.
fn eval_equals(claim_values: Option<&HashSet<&str>>, pred: &CompiledPredicate) -> (bool, String) {
    let expected = pred.expected_value.as_deref().unwrap_or("");
    let Some(values) = claim_values else {
        return (false, format!("Claim '{}' has no values", pred.claim_name));
    };
    if values.len() == 1 && values.contains(expected) {
        (true, format!("Equals '{}'", expected))
    } else if values.len() > 1 {
        (false, format!("Multiple values found, expected single value '{}'", expected))
    } else {
        let actual = values.iter().next().copied().unwrap_or("<none>");
        (false, format!("Expected '{}', got '{}'", expected, actual))
    }
}

/// MinLength / MaxLength: compare the `__length` fact against a threshold.
fn eval_length(
    fact_lookup: &HashMap<&str, HashSet<&str>>,
    pred: &CompiledPredicate,
    cmp: impl Fn(usize, usize) -> bool,
    pass_op: &str,
    fail_op: &str,
    label: &str,
) -> (bool, String) {
    let expected: usize = pred.expected_value.as_deref()
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    let length_key = format!("{}.__length", pred.claim_name);
    let length: usize = fact_lookup
        .get(length_key.as_str())
        .and_then(|v| v.iter().next())
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    if cmp(length, expected) {
        (true, format!("Length {} {} {}", length, pass_op, expected))
    } else {
        (false, format!("Length {} {} {} ({})", length, fail_op, expected, label))
    }
}

/// GreaterThan / LessThan: parse the first claim value as f64 and compare.
fn eval_numeric_cmp(
    claim_values: Option<&HashSet<&str>>,
    pred: &CompiledPredicate,
    cmp: impl Fn(f64, f64) -> bool,
    op: &str,
) -> (bool, String) {
    let expected: f64 = pred.expected_value.as_deref()
        .and_then(|s| s.parse().ok())
        .unwrap_or(0.0);
    let Some(values) = claim_values else {
        return (false, format!("Claim '{}' has no values", pred.claim_name));
    };
    match values.iter().next().and_then(|s| s.parse::<f64>().ok()) {
        Some(actual) if cmp(actual, expected) => (true, format!("{} {} {}", actual, op, expected)),
        Some(actual) => (false, format!("{} is not {} {}", actual, op, expected)),
        None => (false, "Value is not a number".into()),
    }
}

/// Matches: check if any claim value matches a regex pattern.
fn eval_matches(claim_values: Option<&HashSet<&str>>, pred: &CompiledPredicate) -> (bool, String) {
    let pattern = pred.expected_value.as_deref().unwrap_or("");
    let regex = match regex::Regex::new(pattern) {
        Ok(r) => r,
        Err(e) => return (false, format!("Invalid regex: {}", e)),
    };
    let Some(values) = claim_values else {
        return (false, format!("Claim '{}' has no values", pred.claim_name));
    };
    if values.iter().any(|v| regex.is_match(v)) {
        (true, format!("Matches pattern '{}'", pattern))
    } else {
        (false, format!("No value matches pattern '{}'", pattern))
    }
}

/// AnyOf / NoneOf: parse the expected value as a bracketed set and check membership.
/// `expect_in_set = true` → AnyOf (pass if value in set); `false` → NoneOf.
fn eval_set_membership(
    claim_values: Option<&HashSet<&str>>,
    pred: &CompiledPredicate,
    expect_in_set: bool,
) -> (bool, String) {
    let set: Vec<&str> = pred.expected_value.as_deref()
        .map(|v| v.trim_matches(|c| c == '[' || c == ']').split(", ").collect())
        .unwrap_or_default();
    let Some(values) = claim_values else {
        return if expect_in_set {
            (false, format!("Claim '{}' has no values", pred.claim_name))
        } else {
            (true, format!("Claim '{}' has no values (none_of passes vacuously)", pred.claim_name))
        };
    };
    let found = values.iter().any(|v| set.contains(v));
    if expect_in_set {
        if found { (true, "Value is in allowed set".into()) }
        else { (false, "Value is not in allowed set".into()) }
    } else if found {
        (false, "Value is in forbidden set".into())
    } else {
        (true, "Value is not in forbidden set".into())
    }
}

/// Evaluate a single predicate against the fact lookup table.
fn evaluate_predicate_datalog(
    pred: &CompiledPredicate,
    fact_lookup: &HashMap<&str, HashSet<&str>>,
) -> DatalogPredicateResult {
    let claim_values = fact_lookup.get(pred.claim_name.as_str());
    
    let (passed, reason) = match pred.rule {
        PredicateRule::Exists => eval_existence(claim_values, true),
        PredicateRule::NotExists => eval_existence(claim_values, false),
        PredicateRule::Contains => eval_membership(claim_values, pred, true),
        PredicateRule::NotContains => eval_membership(claim_values, pred, false),
        PredicateRule::Equals => eval_equals(claim_values, pred),
        PredicateRule::MinLength => eval_length(fact_lookup, pred, |len, exp| len >= exp, ">=", "<", "minimum"),
        PredicateRule::MaxLength => eval_length(fact_lookup, pred, |len, exp| len <= exp, "<=", ">", "maximum"),
        PredicateRule::GreaterThan => eval_numeric_cmp(claim_values, pred, |a, e| a > e, ">"),
        PredicateRule::LessThan => eval_numeric_cmp(claim_values, pred, |a, e| a < e, "<"),
        PredicateRule::Matches => eval_matches(claim_values, pred),
        PredicateRule::AnyOf => eval_set_membership(claim_values, pred, true),
        PredicateRule::NoneOf => eval_set_membership(claim_values, pred, false),
    };

    DatalogPredicateResult {
        id: pred.id,
        claim_name: pred.claim_name.clone(),
        rule: pred.rule.clone(),
        expected_value: pred.expected_value.clone(),
        passed,
        reason,
        source: pred.source,
        notes: pred.notes.clone(),
    }
}

// ============================================================================
// Datalog Program Generation
// ============================================================================

/// Escape a string value for use in a datalog literal.
///
/// Replaces backslashes, double quotes, and newlines with escape sequences.
fn escape_datalog_string(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
        .replace('\r', "\\r")
        .replace('\t', "\\t")
}

/// Generate the Soufflé rule body for a single predicate.
///
/// Returns the clause after `predicate_pass(id) :- `.
fn rule_body_for_predicate(rule: &PredicateRule, claim: &str, expected: &str) -> String {
    match rule {
        PredicateRule::Exists =>
            format!("claim_value(\"{}\", _)", claim),
        PredicateRule::NotExists =>
            format!("!claim_value(\"{}\", _)", claim),
        PredicateRule::Equals | PredicateRule::Contains =>
            format!("claim_value(\"{}\", \"{}\")", claim, expected),
        PredicateRule::NotContains =>
            format!("!claim_value(\"{}\", \"{}\")", claim, expected),
        PredicateRule::GreaterThan =>
            format!("claim_value(\"{}\", V), to_number(V, N), N > {}", claim, expected),
        PredicateRule::LessThan =>
            format!("claim_value(\"{}\", V), to_number(V, N), N < {}", claim, expected),
        PredicateRule::MinLength =>
            format!("claim_length(\"{}\", N), N >= {}", claim, expected),
        PredicateRule::MaxLength =>
            format!("claim_length(\"{}\", N), N <= {}", claim, expected),
        PredicateRule::Matches =>
            format!("claim_value(\"{}\", V), match(\"{}\", V)", claim, expected),
        PredicateRule::AnyOf =>
            format!("claim_value(\"{}\", V), any_of(\"{}\", V)", claim, expected),
        PredicateRule::NoneOf =>
            format!("!claim_value(\"{}\", V), none_of(\"{}\", V)", claim, expected),
    }
}

/// Format a compiled rulespec and extracted facts as a datalog program.
///
/// Produces a textual `.dl` file with:
/// - Relation declarations (`.decl`)
/// - Fact assertions from the envelope
/// - Rules derived from rulespec predicates
/// - An output directive for query results
///
/// This is a Soufflé-style datalog dialect, which is the most widely
/// used textual datalog format.
pub fn format_datalog_program(
    compiled: &CompiledRulespec,
    facts: &HashSet<Fact>,
) -> String {
    let mut out = String::new();

    // ── Header ──────────────────────────────────────────────────────
    out.push_str("// Auto-generated datalog program\n");
    out.push_str(&format!("// Plan: {}\n", compiled.plan_id));
    out.push_str(&format!("// Compiled at revision: {}\n", compiled.compiled_at_revision));
    out.push_str("\n");

    // ── Relation declarations ───────────────────────────────────────
    out.push_str("// --- Relation declarations ---\n");
    out.push_str(".decl claim_value(claim: symbol, value: symbol)\n");
    out.push_str(".decl claim_length(claim: symbol, length: number)\n");
    out.push_str(".decl predicate_pass(id: number)\n");
    out.push_str(".decl predicate_fail(id: number)\n");
    out.push_str("\n");
    out.push_str(".output predicate_pass\n");
    out.push_str(".output predicate_fail\n");
    out.push_str("\n");

    // ── Facts ───────────────────────────────────────────────────────
    out.push_str("// --- Facts (from envelope) ---\n");
    // Sort for deterministic output
    let mut sorted_facts: Vec<&Fact> = facts.iter().collect();
    sorted_facts.sort_by(|a, b| (&a.claim_name, &a.value).cmp(&(&b.claim_name, &b.value)));

    for fact in &sorted_facts {
        if fact.claim_name.ends_with(".__length") {
            // Length facts go into the claim_length relation
            let base_claim = fact.claim_name.trim_end_matches(".__length");
            if let Ok(n) = fact.value.parse::<i64>() {
                out.push_str(&format!(
                    "claim_length(\"{}\", {}).\n",
                    escape_datalog_string(base_claim),
                    n,
                ));
            }
        } else {
            out.push_str(&format!(
                "claim_value(\"{}\", \"{}\").\n",
                escape_datalog_string(&fact.claim_name),
                escape_datalog_string(&fact.value),
            ));
        }
    }
    out.push_str("\n");

    // ── Rules (from predicates) ─────────────────────────────────────
    out.push_str("// --- Rules (from rulespec predicates) ---\n");
    for pred in &compiled.predicates {
        let id = pred.id;
        let claim = escape_datalog_string(&pred.claim_name);
        let expected = pred
            .expected_value
            .as_deref()
            .map(|v| escape_datalog_string(v))
            .unwrap_or_default();

        // Emit a comment describing the predicate
        out.push_str(&format!(
            "// pred[{}]: {} {} {}{}\n",
            id,
            pred.rule,
            pred.claim_name,
            pred.expected_value.as_deref().map(|v| format!("'{}'", v)).unwrap_or_default(),
            pred.notes.as_deref().map(|n| format!("  -- {}", n)).unwrap_or_default(),
        ));

        let body = rule_body_for_predicate(&pred.rule, &claim, &expected);
        out.push_str(&format!("predicate_pass({}) :- {}.\n", id, body));

        // Derive failure as the negation of pass
        out.push_str(&format!(
            "predicate_fail({}) :- !predicate_pass({}).\n",
            id, id,
        ));
        out.push_str("\n");
    }

    out
}
// ============================================================================
// Formatting
// ============================================================================

/// Format datalog execution results for display.
///
/// This is used for shadow/dry-run output - printed to console but not
/// injected into the context window.
pub fn format_datalog_results(result: &DatalogExecutionResult) -> String {
    let mut output = String::new();

    output.push_str("\n");
    output.push_str(&"─".repeat(60));
    output.push_str("\n");
    output.push_str("🔬 DATALOG INVARIANT VERIFICATION (shadow mode)\n");
    output.push_str(&"─".repeat(60));
    output.push_str("\n\n");

    output.push_str(&format!("Facts extracted: {}\n\n", result.fact_count));

    for pr in &result.predicate_results {
        let status = if pr.passed { "✅" } else { "❌" };
        let value_str = pr
            .expected_value
            .as_ref()
            .map(|v| format!(" '{}'", v))
            .unwrap_or_default();
        
        output.push_str(&format!(
            "{} [{}] {} {}{}\n",
            status, pr.source, pr.rule, pr.claim_name, value_str
        ));
        output.push_str(&format!("   {}\n", pr.reason));
        
        if let Some(notes) = &pr.notes {
            output.push_str(&format!("   📝 {}\n", notes));
        }
        output.push('\n');
    }

    output.push_str(&"─".repeat(60));
    output.push_str("\n");
    
    if result.all_passed() {
        output.push_str(&format!(
            "✅ All {} invariant(s) satisfied (datalog)\n",
            result.passed_count
        ));
    } else {
        output.push_str(&format!(
            "⚠️  {}/{} invariant(s) satisfied, {} failed (datalog)\n",
            result.passed_count,
            result.passed_count + result.failed_count,
            result.failed_count
        ));
    }
    
    output.push_str(&"─".repeat(60));
    output.push_str("\n");

    output
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::invariants::WhenCondition;

    fn make_test_rulespec() -> Rulespec {
        let mut rulespec = Rulespec::new();
        rulespec.claims.push(Claim::new("caps", "csv_importer.capabilities"));
        rulespec.claims.push(Claim::new("file", "csv_importer.file"));
        rulespec.predicates.push(
            Predicate::new("caps", PredicateRule::Contains, InvariantSource::TaskPrompt)
                .with_value(YamlValue::String("handle_tsv".to_string()))
                .with_notes("User requested TSV support"),
        );
        rulespec.predicates.push(
            Predicate::new("file", PredicateRule::Exists, InvariantSource::Memory),
        );
        rulespec
    }

    fn make_test_envelope() -> ActionEnvelope {
        let mut envelope = ActionEnvelope::new();
        envelope.add_fact(
            "csv_importer",
            serde_yaml::from_str(
                r#"
                capabilities:
                  - handle_headers
                  - handle_tsv
                  - handle_quoted_fields
                file: src/import/csv.rs
            "#,
            )
            .unwrap(),
        );
        envelope
    }

    // ========================================================================
    // Compilation Tests
    // ========================================================================

    #[test]
    fn test_compile_rulespec_success() {
        let rulespec = make_test_rulespec();
        let compiled = compile_rulespec(&rulespec, "test-plan", 1).unwrap();

        assert_eq!(compiled.plan_id, "test-plan");
        assert_eq!(compiled.compiled_at_revision, 1);
        assert_eq!(compiled.predicates.len(), 2);
        assert_eq!(compiled.claims.len(), 2);
    }

    #[test]
    fn test_compile_rulespec_empty() {
        let rulespec = Rulespec::new();
        let compiled = compile_rulespec(&rulespec, "test", 1).unwrap();

        assert!(compiled.is_empty());
        assert!(compiled.predicates.is_empty());
        assert!(compiled.claims.is_empty());
    }

    #[test]
    fn test_compile_rulespec_invalid_selector() {
        let mut rulespec = Rulespec::new();
        rulespec.claims.push(Claim {
            name: "bad".to_string(),
            selector: "".to_string(), // Empty selector is invalid
        });

        let result = compile_rulespec(&rulespec, "test", 1);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("Invalid selector"));
    }

    #[test]
    fn test_compile_rulespec_unknown_claim() {
        let mut rulespec = Rulespec::new();
        rulespec.claims.push(Claim::new("known", "foo.bar"));
        rulespec.predicates.push(Predicate::new(
            "unknown", // References non-existent claim
            PredicateRule::Exists,
            InvariantSource::TaskPrompt,
        ));

        let result = compile_rulespec(&rulespec, "test", 1);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("unknown claim"));
    }

    #[test]
    fn test_compile_exists_predicate_no_value() {
        let mut rulespec = Rulespec::new();
        rulespec.claims.push(Claim::new("test", "foo.bar"));
        rulespec.predicates.push(Predicate::new(
            "test",
            PredicateRule::Exists,
            InvariantSource::TaskPrompt,
        ));

        let compiled = compile_rulespec(&rulespec, "test", 1).unwrap();
        assert!(compiled.predicates[0].expected_value.is_none());
    }

    // ========================================================================
    // Fact Extraction Tests
    // ========================================================================

    #[test]
    fn test_extract_facts_basic() {
        let envelope = make_test_envelope();
        let rulespec = make_test_rulespec();
        let compiled = compile_rulespec(&rulespec, "test", 1).unwrap();

        let facts = extract_facts(&envelope, &compiled);

        // Should have facts for capabilities array elements
        assert!(facts.contains(&Fact {
            claim_name: "caps".to_string(),
            value: "handle_tsv".to_string(),
        }));
        assert!(facts.contains(&Fact {
            claim_name: "caps".to_string(),
            value: "handle_headers".to_string(),
        }));
    }

    #[test]
    fn test_extract_facts_missing_path() {
        let envelope = ActionEnvelope::new(); // Empty envelope
        let rulespec = make_test_rulespec();
        let compiled = compile_rulespec(&rulespec, "test", 1).unwrap();

        let facts = extract_facts(&envelope, &compiled);

        // Should return empty set, not error
        assert!(facts.is_empty());
    }

    #[test]
    fn test_extract_facts_array_length() {
        let envelope = make_test_envelope();
        let rulespec = make_test_rulespec();
        let compiled = compile_rulespec(&rulespec, "test", 1).unwrap();

        let facts = extract_facts(&envelope, &compiled);

        // Should have length fact
        assert!(facts.contains(&Fact {
            claim_name: "caps.__length".to_string(),
            value: "3".to_string(),
        }));
    }

    #[test]
    fn test_extract_facts_deeply_nested() {
        let mut envelope = ActionEnvelope::new();
        envelope.add_fact(
            "a",
            serde_yaml::from_str(
                r#"
                b:
                  c:
                    d:
                      e:
                        f: deep_value
            "#,
            )
            .unwrap(),
        );

        let mut rulespec = Rulespec::new();
        rulespec.claims.push(Claim::new("deep", "a.b.c.d.e.f"));
        let compiled = compile_rulespec(&rulespec, "test", 1).unwrap();

        let facts = extract_facts(&envelope, &compiled);

        assert!(facts.contains(&Fact {
            claim_name: "deep".to_string(),
            value: "deep_value".to_string(),
        }));
    }

    #[test]
    fn test_extract_facts_null_value() {
        let mut envelope = ActionEnvelope::new();
        envelope.add_fact("nullable", YamlValue::Null);

        let mut rulespec = Rulespec::new();
        rulespec.claims.push(Claim::new("test", "nullable"));
        let compiled = compile_rulespec(&rulespec, "test", 1).unwrap();

        let facts = extract_facts(&envelope, &compiled);

        // Null values should NOT produce facts — this ensures not_exists passes for null
        assert!(facts.is_empty(), "Null values should not produce facts, got: {:?}", facts);
    }

    #[test]
    fn test_extract_facts_with_facts_prefix_selector() {
        // Simulate a rulespec that uses "facts." prefix in selectors
        // (common when rulespec authors think of the envelope YAML structure
        // which has a top-level "facts:" key)
        let mut envelope = ActionEnvelope::new();
        envelope.add_fact(
            "csv_importer",
            serde_yaml::from_str("capabilities: [handle_csv, handle_tsv]\nfile: src/csv.rs").unwrap(),
        );

        let mut rulespec = Rulespec::new();
        // Selector uses "facts." prefix — should still work via fallback
        rulespec.claims.push(Claim::new("caps", "facts.csv_importer.capabilities"));
        rulespec.claims.push(Claim::new("file", "facts.csv_importer.file"));
        let compiled = compile_rulespec(&rulespec, "test", 1).unwrap();

        let facts = extract_facts(&envelope, &compiled);

        assert!(!facts.is_empty(), "Should extract facts even with 'facts.' prefix selector");
        assert!(facts.contains(&Fact {
            claim_name: "caps".to_string(),
            value: "handle_csv".to_string(),
        }));
        assert!(facts.contains(&Fact {
            claim_name: "caps".to_string(),
            value: "handle_tsv".to_string(),
        }));
        assert!(facts.contains(&Fact {
            claim_name: "file".to_string(),
            value: "src/csv.rs".to_string(),
        }));
    }

    #[test]
    fn test_extract_facts_roundtrip_from_yaml() {
        // Simulate the real write_envelope → read_envelope → extract_facts flow
        let yaml = "facts:\n  feature:\n    done: true\n    capabilities: [handle_csv, handle_tsv]\n    file: src/lib.rs";
        let envelope: ActionEnvelope = serde_yaml::from_str(yaml).unwrap();
        assert!(!envelope.facts.is_empty(), "Envelope should have facts after parsing");

        let mut rulespec = Rulespec::new();
        rulespec.claims.push(Claim::new("feature_done", "feature.done"));
        rulespec.claims.push(Claim::new("caps", "feature.capabilities"));
        let compiled = compile_rulespec(&rulespec, "test", 1).unwrap();

        let facts = extract_facts(&envelope, &compiled);
        assert!(!facts.is_empty(), "Should extract facts from round-tripped envelope");
        assert!(facts.contains(&Fact {
            claim_name: "feature_done".to_string(),
            value: "true".to_string(),
        }));
        assert!(facts.contains(&Fact {
            claim_name: "caps".to_string(),
            value: "handle_csv".to_string(),
        }));
    }

    // ========================================================================
    // Execution Tests
    // ========================================================================

    #[test]
    fn test_execute_rules_all_pass() {
        let envelope = make_test_envelope();
        let rulespec = make_test_rulespec();
        let compiled = compile_rulespec(&rulespec, "test", 1).unwrap();
        let facts = extract_facts(&envelope, &compiled);

        let result = execute_rules(&compiled, &facts);

        assert!(result.all_passed());
        assert_eq!(result.passed_count, 2);
        assert_eq!(result.failed_count, 0);
    }

    #[test]
    fn test_execute_rules_contains_fail() {
        let envelope = make_test_envelope();
        
        let mut rulespec = Rulespec::new();
        rulespec.claims.push(Claim::new("caps", "csv_importer.capabilities"));
        rulespec.predicates.push(
            Predicate::new("caps", PredicateRule::Contains, InvariantSource::TaskPrompt)
                .with_value(YamlValue::String("handle_xlsx".to_string())), // Not in envelope
        );

        let compiled = compile_rulespec(&rulespec, "test", 1).unwrap();
        let facts = extract_facts(&envelope, &compiled);
        let result = execute_rules(&compiled, &facts);

        assert!(!result.all_passed());
        assert_eq!(result.failed_count, 1);
        assert!(result.predicate_results[0].reason.contains("Does not contain"));
    }

    #[test]
    fn test_execute_rules_exists_pass() {
        let envelope = make_test_envelope();
        
        let mut rulespec = Rulespec::new();
        rulespec.claims.push(Claim::new("file", "csv_importer.file"));
        rulespec.predicates.push(Predicate::new(
            "file",
            PredicateRule::Exists,
            InvariantSource::TaskPrompt,
        ));

        let compiled = compile_rulespec(&rulespec, "test", 1).unwrap();
        let facts = extract_facts(&envelope, &compiled);
        let result = execute_rules(&compiled, &facts);

        assert!(result.all_passed());
    }

    #[test]
    fn test_execute_rules_not_exists_pass() {
        let envelope = make_test_envelope();
        
        let mut rulespec = Rulespec::new();
        rulespec.claims.push(Claim::new("missing", "nonexistent.path"));
        rulespec.predicates.push(Predicate::new(
            "missing",
            PredicateRule::NotExists,
            InvariantSource::TaskPrompt,
        ));

        let compiled = compile_rulespec(&rulespec, "test", 1).unwrap();
        let facts = extract_facts(&envelope, &compiled);
        let result = execute_rules(&compiled, &facts);

        assert!(result.all_passed());
    }

    #[test]
    fn test_execute_rules_equals_pass() {
        let envelope = make_test_envelope();
        
        let mut rulespec = Rulespec::new();
        rulespec.claims.push(Claim::new("file", "csv_importer.file"));
        rulespec.predicates.push(
            Predicate::new("file", PredicateRule::Equals, InvariantSource::TaskPrompt)
                .with_value(YamlValue::String("src/import/csv.rs".to_string())),
        );

        let compiled = compile_rulespec(&rulespec, "test", 1).unwrap();
        let facts = extract_facts(&envelope, &compiled);
        let result = execute_rules(&compiled, &facts);

        assert!(result.all_passed());
    }

    #[test]
    fn test_execute_rules_min_length_pass() {
        let envelope = make_test_envelope();
        
        let mut rulespec = Rulespec::new();
        rulespec.claims.push(Claim::new("caps", "csv_importer.capabilities"));
        rulespec.predicates.push(
            Predicate::new("caps", PredicateRule::MinLength, InvariantSource::TaskPrompt)
                .with_value(YamlValue::Number(2.into())),
        );

        let compiled = compile_rulespec(&rulespec, "test", 1).unwrap();
        let facts = extract_facts(&envelope, &compiled);
        let result = execute_rules(&compiled, &facts);

        assert!(result.all_passed());
        assert!(result.predicate_results[0].reason.contains("3 >= 2"));
    }

    #[test]
    fn test_execute_rules_max_length_fail() {
        let envelope = make_test_envelope();
        
        let mut rulespec = Rulespec::new();
        rulespec.claims.push(Claim::new("caps", "csv_importer.capabilities"));
        rulespec.predicates.push(
            Predicate::new("caps", PredicateRule::MaxLength, InvariantSource::TaskPrompt)
                .with_value(YamlValue::Number(2.into())), // Array has 3 elements
        );

        let compiled = compile_rulespec(&rulespec, "test", 1).unwrap();
        let facts = extract_facts(&envelope, &compiled);
        let result = execute_rules(&compiled, &facts);

        assert!(!result.all_passed());
        assert!(result.predicate_results[0].reason.contains("3 > 2"));
    }

    #[test]
    fn test_execute_rules_matches_pass() {
        let envelope = make_test_envelope();
        
        let mut rulespec = Rulespec::new();
        rulespec.claims.push(Claim::new("file", "csv_importer.file"));
        rulespec.predicates.push(
            Predicate::new("file", PredicateRule::Matches, InvariantSource::TaskPrompt)
                .with_value(YamlValue::String(r"src/.*\.rs".to_string())),
        );

        let compiled = compile_rulespec(&rulespec, "test", 1).unwrap();
        let facts = extract_facts(&envelope, &compiled);
        let result = execute_rules(&compiled, &facts);

        assert!(result.all_passed());
    }

    #[test]
    fn test_execute_rules_no_facts() {
        let envelope = ActionEnvelope::new();
        let rulespec = make_test_rulespec();
        let compiled = compile_rulespec(&rulespec, "test", 1).unwrap();
        let facts = extract_facts(&envelope, &compiled);

        let result = execute_rules(&compiled, &facts);

        // Both predicates should fail (contains and exists)
        assert!(!result.all_passed());
        assert_eq!(result.failed_count, 2);
    }

    #[test]
    fn test_execute_rules_greater_than() {
        let mut envelope = ActionEnvelope::new();
        envelope.add_fact("count", YamlValue::Number(42.into()));

        let mut rulespec = Rulespec::new();
        rulespec.claims.push(Claim::new("count", "count"));
        rulespec.predicates.push(
            Predicate::new("count", PredicateRule::GreaterThan, InvariantSource::TaskPrompt)
                .with_value(YamlValue::Number(10.into())),
        );

        let compiled = compile_rulespec(&rulespec, "test", 1).unwrap();
        let facts = extract_facts(&envelope, &compiled);
        let result = execute_rules(&compiled, &facts);

        assert!(result.all_passed());
    }

    #[test]
    fn test_execute_rules_less_than() {
        let mut envelope = ActionEnvelope::new();
        envelope.add_fact("count", YamlValue::Number(5.into()));

        let mut rulespec = Rulespec::new();
        rulespec.claims.push(Claim::new("count", "count"));
        rulespec.predicates.push(
            Predicate::new("count", PredicateRule::LessThan, InvariantSource::TaskPrompt)
                .with_value(YamlValue::Number(10.into())),
        );

        let compiled = compile_rulespec(&rulespec, "test", 1).unwrap();
        let facts = extract_facts(&envelope, &compiled);
        let result = execute_rules(&compiled, &facts);

        assert!(result.all_passed());
    }

    // ========================================================================
    // Storage Tests
    // ========================================================================

    #[test]
    fn test_compiled_rulespec_serialization() {
        let rulespec = make_test_rulespec();
        let compiled = compile_rulespec(&rulespec, "test", 1).unwrap();

        let json = serde_json::to_string(&compiled).unwrap();
        let deserialized: CompiledRulespec = serde_json::from_str(&json).unwrap();

        assert_eq!(deserialized.plan_id, compiled.plan_id);
        assert_eq!(deserialized.predicates.len(), compiled.predicates.len());
    }

    // ========================================================================
    // Formatting Tests
    // ========================================================================

    #[test]
    fn test_format_datalog_results() {
        let envelope = make_test_envelope();
        let rulespec = make_test_rulespec();
        let compiled = compile_rulespec(&rulespec, "test", 1).unwrap();
        let facts = extract_facts(&envelope, &compiled);
        let result = execute_rules(&compiled, &facts);

        let output = format_datalog_results(&result);

        assert!(output.contains("DATALOG INVARIANT VERIFICATION"));
        assert!(output.contains("shadow mode"));
        assert!(output.contains("✅"));
        assert!(output.contains("Facts extracted:"));
    }

    #[test]
    fn test_execute_rules_full_pipeline_with_facts_prefix() {
        // End-to-end: YAML envelope → extract_facts (with facts. prefix) → execute_rules
        let yaml = "facts:\n  my_feature:\n    capabilities: [fast_search, caching]\n    file: src/search.rs\n    breaking: false";
        let envelope: ActionEnvelope = serde_yaml::from_str(yaml).unwrap();

        let mut rulespec = Rulespec::new();
        // Use facts. prefix selectors (the common mistake)
        rulespec.claims.push(Claim::new("caps", "facts.my_feature.capabilities"));
        rulespec.claims.push(Claim::new("file", "facts.my_feature.file"));
        rulespec.claims.push(Claim::new("breaking", "facts.my_feature.breaking"));
        rulespec.predicates.push(
            Predicate::new("caps", PredicateRule::Contains, InvariantSource::TaskPrompt)
                .with_value(YamlValue::String("fast_search".to_string())),
        );
        rulespec.predicates.push(
            Predicate::new("file", PredicateRule::Exists, InvariantSource::TaskPrompt),
        );
        rulespec.predicates.push(
            Predicate::new("breaking", PredicateRule::Equals, InvariantSource::TaskPrompt)
                .with_value(YamlValue::String("false".to_string())),
        );

        let compiled = compile_rulespec(&rulespec, "test", 1).unwrap();
        let facts = extract_facts(&envelope, &compiled);
        assert!(facts.len() > 0, "Should extract facts with facts. prefix selectors");

        let result = execute_rules(&compiled, &facts);
        assert_eq!(result.fact_count, facts.len());
        assert!(result.all_passed(), "All predicates should pass: {:?}",
            result.predicate_results.iter()
                .filter(|r| !r.passed)
                .map(|r| format!("{}: {}", r.claim_name, r.reason))
                .collect::<Vec<_>>());
        assert_eq!(result.passed_count, 3);
        assert_eq!(result.failed_count, 0);
    }

    #[test]
    fn test_execute_rules_full_pipeline_without_facts_prefix() {
        // End-to-end: YAML envelope → extract_facts (without facts. prefix) → execute_rules
        let yaml = "facts:\n  my_feature:\n    capabilities: [fast_search, caching]\n    file: src/search.rs";
        let envelope: ActionEnvelope = serde_yaml::from_str(yaml).unwrap();

        let mut rulespec = Rulespec::new();
        // Use direct selectors (no facts. prefix)
        rulespec.claims.push(Claim::new("caps", "my_feature.capabilities"));
        rulespec.claims.push(Claim::new("file", "my_feature.file"));
        rulespec.predicates.push(
            Predicate::new("caps", PredicateRule::Contains, InvariantSource::TaskPrompt)
                .with_value(YamlValue::String("caching".to_string())),
        );
        rulespec.predicates.push(
            Predicate::new("file", PredicateRule::Exists, InvariantSource::TaskPrompt),
        );

        let compiled = compile_rulespec(&rulespec, "test", 1).unwrap();
        let facts = extract_facts(&envelope, &compiled);
        assert!(facts.len() > 0, "Should extract facts without facts. prefix");

        let result = execute_rules(&compiled, &facts);
        assert!(result.all_passed());
        assert_eq!(result.passed_count, 2);
    }

    // ========================================================================
    // Datalog Program Generation Tests
    // ========================================================================

    #[test]
    fn test_format_datalog_program_butler_example() {
        // Mirrors the butler rulespec: email_reviewed equals true
        let mut rulespec = Rulespec::new();
        rulespec.claims.push(Claim::new("email_reviewed", "facts.reviewed"));
        rulespec.predicates.push(
            Predicate::new("email_reviewed", PredicateRule::Equals, InvariantSource::TaskPrompt)
                .with_value(YamlValue::Bool(true))
                .with_notes("Outgoing emails must be manually reviewed before sending"),
        );

        let compiled = compile_rulespec(&rulespec, "outbound-email", 0).unwrap();

        let mut envelope = ActionEnvelope::new();
        envelope.add_fact("facts", serde_yaml::from_str("reviewed: true").unwrap());
        let facts = extract_facts(&envelope, &compiled);

        let dl = format_datalog_program(&compiled, &facts);

        // Header
        assert!(dl.contains("// Auto-generated datalog program"));
        assert!(dl.contains("// Plan: outbound-email"));

        // Relation declarations
        assert!(dl.contains(".decl claim_value(claim: symbol, value: symbol)"));
        assert!(dl.contains(".decl claim_length(claim: symbol, length: number)"));
        assert!(dl.contains(".decl predicate_pass(id: number)"));
        assert!(dl.contains(".decl predicate_fail(id: number)"));
        assert!(dl.contains(".output predicate_pass"));
        assert!(dl.contains(".output predicate_fail"));

        // Facts
        assert!(dl.contains(r#"claim_value("email_reviewed", "true")."#));

        // Rule for equals
        assert!(dl.contains(r#"predicate_pass(0) :- claim_value("email_reviewed", "true")."#));
        assert!(dl.contains("predicate_fail(0) :- !predicate_pass(0)."));

        // Comment with notes
        assert!(dl.contains("Outgoing emails must be manually reviewed"));
    }

    #[test]
    fn test_format_datalog_program_empty_rulespec() {
        let rulespec = Rulespec::new();
        let compiled = compile_rulespec(&rulespec, "empty", 0).unwrap();
        let facts = std::collections::HashSet::new();

        let dl = format_datalog_program(&compiled, &facts);

        // Should still have valid structure
        assert!(dl.contains(".decl claim_value"));
        assert!(dl.contains(".decl predicate_pass"));
        assert!(dl.contains("// --- Facts (from envelope) ---"));
        assert!(dl.contains("// --- Rules (from rulespec predicates) ---"));

        // No fact assertions (lines ending with period) or rules beyond declarations
        assert!(!dl.contains(r#"claim_value(""#));
        assert!(!dl.contains("predicate_pass(0)"));
    }

    #[test]
    fn test_format_datalog_program_empty_facts() {
        let mut rulespec = Rulespec::new();
        rulespec.claims.push(Claim::new("test", "foo.bar"));
        rulespec.predicates.push(
            Predicate::new("test", PredicateRule::Exists, InvariantSource::TaskPrompt),
        );

        let compiled = compile_rulespec(&rulespec, "test", 1).unwrap();
        let facts = std::collections::HashSet::new();

        let dl = format_datalog_program(&compiled, &facts);

        // Has declarations and rules but no fact assertions
        assert!(dl.contains(".decl claim_value"));
        assert!(dl.contains("predicate_pass(0) :- claim_value"));
        assert!(dl.contains("predicate_fail(0) :- !predicate_pass(0)"));
        // No claim_value facts
        // The rules section will reference claim_value("test", _) but the facts section should not
        let facts_section = dl.split("// --- Rules").next().unwrap();
        assert!(!facts_section.contains(r#"claim_value("test""#));
    }

    #[test]
    fn test_format_datalog_program_special_characters() {
        let mut rulespec = Rulespec::new();
        rulespec.claims.push(Claim::new("msg", "message"));
        rulespec.predicates.push(
            Predicate::new("msg", PredicateRule::Equals, InvariantSource::TaskPrompt)
                .with_value(YamlValue::String("hello \"world\"".to_string())),
        );

        let compiled = compile_rulespec(&rulespec, "test", 0).unwrap();

        let mut envelope = ActionEnvelope::new();
        envelope.add_fact("message", YamlValue::String("hello \"world\"".to_string()));
        let facts = extract_facts(&envelope, &compiled);

        let dl = format_datalog_program(&compiled, &facts);

        // Quotes should be escaped
        assert!(dl.contains(r#"\"world\""#));
    }

    #[test]
    fn test_format_datalog_program_all_rule_types() {
        let mut rulespec = Rulespec::new();

        // Create claims for each rule type
        rulespec.claims.push(Claim::new("c_exists", "a"));
        rulespec.claims.push(Claim::new("c_not_exists", "b"));
        rulespec.claims.push(Claim::new("c_equals", "c"));
        rulespec.claims.push(Claim::new("c_contains", "d"));
        rulespec.claims.push(Claim::new("c_gt", "e"));
        rulespec.claims.push(Claim::new("c_lt", "f"));
        rulespec.claims.push(Claim::new("c_min", "g"));
        rulespec.claims.push(Claim::new("c_max", "h"));
        rulespec.claims.push(Claim::new("c_matches", "i"));

        // Add one predicate per rule type
        rulespec.predicates.push(
            Predicate::new("c_exists", PredicateRule::Exists, InvariantSource::TaskPrompt),
        );
        rulespec.predicates.push(
            Predicate::new("c_not_exists", PredicateRule::NotExists, InvariantSource::TaskPrompt),
        );
        rulespec.predicates.push(
            Predicate::new("c_equals", PredicateRule::Equals, InvariantSource::TaskPrompt)
                .with_value(YamlValue::String("val".to_string())),
        );
        rulespec.predicates.push(
            Predicate::new("c_contains", PredicateRule::Contains, InvariantSource::TaskPrompt)
                .with_value(YamlValue::String("item".to_string())),
        );
        rulespec.predicates.push(
            Predicate::new("c_gt", PredicateRule::GreaterThan, InvariantSource::TaskPrompt)
                .with_value(YamlValue::Number(10.into())),
        );
        rulespec.predicates.push(
            Predicate::new("c_lt", PredicateRule::LessThan, InvariantSource::TaskPrompt)
                .with_value(YamlValue::Number(100.into())),
        );
        rulespec.predicates.push(
            Predicate::new("c_min", PredicateRule::MinLength, InvariantSource::TaskPrompt)
                .with_value(YamlValue::Number(2.into())),
        );
        rulespec.predicates.push(
            Predicate::new("c_max", PredicateRule::MaxLength, InvariantSource::TaskPrompt)
                .with_value(YamlValue::Number(5.into())),
        );
        rulespec.predicates.push(
            Predicate::new("c_matches", PredicateRule::Matches, InvariantSource::TaskPrompt)
                .with_value(YamlValue::String("^foo.*".to_string())),
        );

        let compiled = compile_rulespec(&rulespec, "all-rules", 1).unwrap();
        let facts = std::collections::HashSet::new();

        let dl = format_datalog_program(&compiled, &facts);

        // Each rule type produces a distinct pattern
        assert!(dl.contains(r#"predicate_pass(0) :- claim_value("c_exists", _)."#));
        assert!(dl.contains(r#"predicate_pass(1) :- !claim_value("c_not_exists", _)."#));
        assert!(dl.contains(r#"predicate_pass(2) :- claim_value("c_equals", "val")."#));
        assert!(dl.contains(r#"predicate_pass(3) :- claim_value("c_contains", "item")."#));
        assert!(dl.contains(r#"predicate_pass(4) :- claim_value("c_gt", V), to_number(V, N), N > 10."#));
        assert!(dl.contains(r#"predicate_pass(5) :- claim_value("c_lt", V), to_number(V, N), N < 100."#));
        assert!(dl.contains(r#"predicate_pass(6) :- claim_length("c_min", N), N >= 2."#));
        assert!(dl.contains(r#"predicate_pass(7) :- claim_length("c_max", N), N <= 5."#));
        assert!(dl.contains(r#"predicate_pass(8) :- claim_value("c_matches", V), match("^foo.*", V)."#));

        // Each has a corresponding fail rule
        for i in 0..9 {
            assert!(dl.contains(&format!("predicate_fail({}) :- !predicate_pass({}).", i, i)));
        }
    }

    #[test]
    fn test_format_datalog_program_length_facts() {
        let mut rulespec = Rulespec::new();
        rulespec.claims.push(Claim::new("caps", "csv_importer.capabilities"));
        rulespec.predicates.push(
            Predicate::new("caps", PredicateRule::MinLength, InvariantSource::TaskPrompt)
                .with_value(YamlValue::Number(2.into())),
        );

        let compiled = compile_rulespec(&rulespec, "test", 0).unwrap();

        let envelope = make_test_envelope();
        let facts = extract_facts(&envelope, &compiled);

        let dl = format_datalog_program(&compiled, &facts);

        // Length facts should use claim_length relation
        assert!(dl.contains(r#"claim_length("caps", 3)."#));
        // Individual values should use claim_value
        assert!(dl.contains(r#"claim_value("caps", "handle_tsv")."#));
        assert!(dl.contains(r#"claim_value("caps", "handle_headers")."#));
    }

    #[test]
    fn test_format_datalog_program_deterministic_output() {
        let envelope = make_test_envelope();
        let rulespec = make_test_rulespec();
        let compiled = compile_rulespec(&rulespec, "test", 1).unwrap();
        let facts = extract_facts(&envelope, &compiled);

        let dl1 = format_datalog_program(&compiled, &facts);
        let dl2 = format_datalog_program(&compiled, &facts);

        // Output should be identical across calls (sorted facts)
        assert_eq!(dl1, dl2);
    }

    #[test]
    fn test_escape_datalog_string() {
        assert_eq!(escape_datalog_string("hello"), "hello");
        assert_eq!(escape_datalog_string("say \"hi\""), "say \\\"hi\\\"");
        assert_eq!(escape_datalog_string("line1\nline2"), "line1\\nline2");
        assert_eq!(escape_datalog_string("tab\there"), "tab\\there");
        assert_eq!(escape_datalog_string("back\\slash"), "back\\\\slash");
    }

    // ========================================================================
    // New Rules: NotContains, AnyOf, NoneOf in Datalog
    // ========================================================================

    #[test]
    fn test_execute_rules_not_contains_pass() {
        let mut rulespec = Rulespec::new();
        rulespec.claims.push(Claim::new("caps", "feature.capabilities"));
        rulespec.predicates.push(
            Predicate::new("caps", PredicateRule::NotContains, InvariantSource::TaskPrompt)
                .with_value(YamlValue::String("deprecated".to_string())),
        );

        let compiled = compile_rulespec(&rulespec, "test", 1).unwrap();

        let mut envelope = ActionEnvelope::new();
        envelope.add_fact("feature", serde_yaml::from_str("capabilities: [handle_csv, handle_tsv]").unwrap());
        let facts = extract_facts(&envelope, &compiled);

        let result = execute_rules(&compiled, &facts);
        assert!(result.all_passed(), "not_contains should pass when element absent");
    }

    #[test]
    fn test_execute_rules_not_contains_fail() {
        let mut rulespec = Rulespec::new();
        rulespec.claims.push(Claim::new("caps", "feature.capabilities"));
        rulespec.predicates.push(
            Predicate::new("caps", PredicateRule::NotContains, InvariantSource::TaskPrompt)
                .with_value(YamlValue::String("handle_csv".to_string())),
        );

        let compiled = compile_rulespec(&rulespec, "test", 1).unwrap();

        let mut envelope = ActionEnvelope::new();
        envelope.add_fact("feature", serde_yaml::from_str("capabilities: [handle_csv, handle_tsv]").unwrap());
        let facts = extract_facts(&envelope, &compiled);

        let result = execute_rules(&compiled, &facts);
        assert_eq!(result.failed_count, 1, "not_contains should fail when element present");
    }

    // ------------------------------------------------------------------------
    // Address-aware not_contains (red-team finding, 2026-11-16): exact set
    // membership let "R@wideplay.com", "Jess <jess@x>", comma-joined lists and
    // every address at a blocked DOMAIN walk straight past a blocklist.
    // ------------------------------------------------------------------------

    /// Run ONE `email.to not_contains <entry>` predicate over `to`; true = refused.
    fn to_refused(to: &str, entry: &str) -> bool {
        let mut rulespec = Rulespec::new();
        rulespec.claims.push(Claim::new("email_to", "email.to"));
        rulespec.predicates.push(
            Predicate::new("email_to", PredicateRule::NotContains, InvariantSource::Memory)
                .with_value(YamlValue::String(entry.to_string())),
        );
        let compiled = compile_rulespec(&rulespec, "test", 1).unwrap();
        let mut envelope = ActionEnvelope::new();
        let mut email = serde_yaml::Mapping::new();
        email.insert(YamlValue::String("to".into()), YamlValue::String(to.into()));
        envelope.add_fact("email", YamlValue::Mapping(email));
        let facts = extract_facts(&envelope, &compiled);
        execute_rules(&compiled, &facts).failed_count == 1
    }

    #[test]
    fn test_not_contains_address_exact_still_refused() {
        assert!(to_refused("jess@example.com", "jess@example.com"));
    }

    #[test]
    fn test_not_contains_address_is_case_insensitive() {
        assert!(to_refused("R@WidePlay.com", "r@wideplay.com"));
    }

    #[test]
    fn test_not_contains_address_inside_display_name() {
        assert!(to_refused("Jess McInnes <jess@example.com>", "jess@example.com"));
    }

    #[test]
    fn test_not_contains_address_inside_comma_list() {
        assert!(to_refused("ari@example.org, jess@example.com", "jess@example.com"));
        assert!(to_refused("ari@example.org;jess@example.com", "jess@example.com"));
    }

    #[test]
    fn test_not_contains_domain_entry_blocks_every_address_there() {
        assert!(to_refused("dhanji@rethrick.com", "rethrick.com"));
        assert!(to_refused("x@mail.rethrick.com", "rethrick.com"));
        assert!(to_refused("a@b.com, Someone <s@RETHRICK.com>", "rethrick.com"));
    }

    #[test]
    fn test_not_contains_address_is_not_a_substring_test() {
        // `sender@wideplay.com` CONTAINS "r@wideplay.com"; a substring test
        // would refuse a legitimate stranger forever.
        assert!(!to_refused("sender@wideplay.com", "r@wideplay.com"));
        // A lookalike domain is not a subdomain.
        assert!(!to_refused("x@notrethrick.com", "rethrick.com"));
        assert!(!to_refused("ari@example.org", "jess@example.com"));
    }

    #[test]
    fn test_not_contains_non_address_claims_keep_exact_semantics() {
        // No '@' in the value -> the address leg never fires.
        assert!(!to_refused("handle_csv_v2", "handle_csv"));
        assert!(!to_refused("Handle_CSV", "handle_csv"));
        assert!(to_refused("handle_csv", "handle_csv"));
    }

    #[test]
    fn test_execute_rules_any_of_pass() {
        let mut rulespec = Rulespec::new();
        rulespec.claims.push(Claim::new("format", "feature.output_format"));
        rulespec.predicates.push(
            Predicate::new("format", PredicateRule::AnyOf, InvariantSource::TaskPrompt)
                .with_value(YamlValue::Sequence(vec![
                    YamlValue::String("json".to_string()),
                    YamlValue::String("yaml".to_string()),
                ])),
        );

        let compiled = compile_rulespec(&rulespec, "test", 1).unwrap();

        let mut envelope = ActionEnvelope::new();
        envelope.add_fact("feature", serde_yaml::from_str("output_format: json").unwrap());
        let facts = extract_facts(&envelope, &compiled);

        let result = execute_rules(&compiled, &facts);
        assert!(result.all_passed(), "any_of should pass when value is in set");
    }

    #[test]
    fn test_execute_rules_any_of_fail() {
        let mut rulespec = Rulespec::new();
        rulespec.claims.push(Claim::new("format", "feature.output_format"));
        rulespec.predicates.push(
            Predicate::new("format", PredicateRule::AnyOf, InvariantSource::TaskPrompt)
                .with_value(YamlValue::Sequence(vec![
                    YamlValue::String("json".to_string()),
                    YamlValue::String("yaml".to_string()),
                ])),
        );

        let compiled = compile_rulespec(&rulespec, "test", 1).unwrap();

        let mut envelope = ActionEnvelope::new();
        envelope.add_fact("feature", serde_yaml::from_str("output_format: xml").unwrap());
        let facts = extract_facts(&envelope, &compiled);

        let result = execute_rules(&compiled, &facts);
        assert_eq!(result.failed_count, 1, "any_of should fail when value not in set");
    }

    #[test]
    fn test_execute_rules_none_of_pass() {
        let mut rulespec = Rulespec::new();
        rulespec.claims.push(Claim::new("format", "feature.output_format"));
        rulespec.predicates.push(
            Predicate::new("format", PredicateRule::NoneOf, InvariantSource::TaskPrompt)
                .with_value(YamlValue::Sequence(vec![
                    YamlValue::String("xml".to_string()),
                    YamlValue::String("csv".to_string()),
                ])),
        );

        let compiled = compile_rulespec(&rulespec, "test", 1).unwrap();

        let mut envelope = ActionEnvelope::new();
        envelope.add_fact("feature", serde_yaml::from_str("output_format: json").unwrap());
        let facts = extract_facts(&envelope, &compiled);

        let result = execute_rules(&compiled, &facts);
        assert!(result.all_passed(), "none_of should pass when value not in forbidden set");
    }

    #[test]
    fn test_execute_rules_none_of_fail() {
        let mut rulespec = Rulespec::new();
        rulespec.claims.push(Claim::new("format", "feature.output_format"));
        rulespec.predicates.push(
            Predicate::new("format", PredicateRule::NoneOf, InvariantSource::TaskPrompt)
                .with_value(YamlValue::Sequence(vec![
                    YamlValue::String("xml".to_string()),
                    YamlValue::String("csv".to_string()),
                ])),
        );

        let compiled = compile_rulespec(&rulespec, "test", 1).unwrap();

        let mut envelope = ActionEnvelope::new();
        envelope.add_fact("feature", serde_yaml::from_str("output_format: xml").unwrap());
        let facts = extract_facts(&envelope, &compiled);

        let result = execute_rules(&compiled, &facts);
        assert_eq!(result.failed_count, 1, "none_of should fail when value in forbidden set");
    }

    // ========================================================================
    // When Conditions in Datalog
    // ========================================================================

    #[test]
    fn test_execute_rules_when_condition_met() {
        let mut rulespec = Rulespec::new();
        rulespec.claims.push(Claim::new("is_breaking", "api.breaking"));
        rulespec.claims.push(Claim::new("caps", "feature.capabilities"));
        rulespec.predicates.push(
            Predicate::new("caps", PredicateRule::MinLength, InvariantSource::TaskPrompt)
                .with_value(YamlValue::Number(2.into()))
                .with_when(WhenCondition::new("is_breaking", PredicateRule::Equals)
                    .with_value(YamlValue::Bool(true))),
        );

        let compiled = compile_rulespec(&rulespec, "test", 1).unwrap();

        let mut envelope = ActionEnvelope::new();
        envelope.add_fact("api", serde_yaml::from_str("breaking: true").unwrap());
        envelope.add_fact("feature", serde_yaml::from_str("capabilities: [a, b, c]").unwrap());
        let facts = extract_facts(&envelope, &compiled);

        let result = execute_rules(&compiled, &facts);
        assert!(result.all_passed(), "When met + predicate passes should pass");
    }

    #[test]
    fn test_execute_rules_when_condition_not_met_vacuous_pass() {
        let mut rulespec = Rulespec::new();
        rulespec.claims.push(Claim::new("is_breaking", "api.breaking"));
        rulespec.claims.push(Claim::new("caps", "feature.capabilities"));
        rulespec.predicates.push(
            Predicate::new("caps", PredicateRule::MinLength, InvariantSource::TaskPrompt)
                .with_value(YamlValue::Number(100.into())) // would fail if evaluated
                .with_when(WhenCondition::new("is_breaking", PredicateRule::Equals)
                    .with_value(YamlValue::Bool(true))),
        );

        let compiled = compile_rulespec(&rulespec, "test", 1).unwrap();

        let mut envelope = ActionEnvelope::new();
        envelope.add_fact("api", serde_yaml::from_str("breaking: false").unwrap());
        envelope.add_fact("feature", serde_yaml::from_str("capabilities: [a]").unwrap());
        let facts = extract_facts(&envelope, &compiled);

        let result = execute_rules(&compiled, &facts);
        assert!(result.all_passed(), "When not met should be vacuous pass");
        assert!(result.predicate_results[0].reason.contains("Skipped"));
    }

    #[test]
    fn test_execute_rules_when_exists_on_null() {
        let mut rulespec = Rulespec::new();
        rulespec.claims.push(Claim::new("has_tests", "testing.tests"));
        rulespec.claims.push(Claim::new("coverage", "testing.coverage"));
        rulespec.predicates.push(
            Predicate::new("coverage", PredicateRule::GreaterThan, InvariantSource::TaskPrompt)
                .with_value(YamlValue::Number(80.into()))
                .with_when(WhenCondition::new("has_tests", PredicateRule::Exists)),
        );

        let compiled = compile_rulespec(&rulespec, "test", 1).unwrap();

        // tests is null → when(exists) not met → vacuous pass
        let mut envelope = ActionEnvelope::new();
        envelope.add_fact("testing", serde_yaml::from_str("tests: null\ncoverage: 50").unwrap());
        let facts = extract_facts(&envelope, &compiled);

        let result = execute_rules(&compiled, &facts);
        assert!(result.all_passed(), "When exists on null should skip (vacuous pass)");
    }

    #[test]
    fn test_execute_rules_when_matches_condition_met() {
        // This is the butler rulespec pattern: when subject matches ^Re:
        let mut rulespec = Rulespec::new();
        rulespec.claims.push(Claim::new("subject", "subject"));
        rulespec.claims.push(Claim::new("reply_to", "reply_to_message_id"));
        rulespec.predicates.push(
            Predicate::new("reply_to", PredicateRule::Exists, InvariantSource::TaskPrompt)
                .with_when(WhenCondition::new("subject", PredicateRule::Matches)
                    .with_value(YamlValue::String("^Re: ".to_string()))),
        );

        let compiled = compile_rulespec(&rulespec, "test", 1).unwrap();

        // Reply email WITH reply_to_message_id → should pass
        let mut envelope = ActionEnvelope::new();
        envelope.add_fact("subject", YamlValue::String("Re: Hello".to_string()));
        envelope.add_fact("reply_to_message_id", YamlValue::String("<abc@example.com>".to_string()));
        let facts = extract_facts(&envelope, &compiled);

        let result = execute_rules(&compiled, &facts);
        assert!(result.all_passed(), "When matches met + exists should pass");
        // Crucially: should NOT say "Skipped"
        assert!(!result.predicate_results[0].reason.contains("Skipped"),
            "Should evaluate predicate, not skip it");
    }

    #[test]
    fn test_execute_rules_when_matches_condition_met_but_predicate_fails() {
        // Reply email WITHOUT reply_to_message_id → when met, predicate fails
        let mut rulespec = Rulespec::new();
        rulespec.claims.push(Claim::new("subject", "subject"));
        rulespec.claims.push(Claim::new("reply_to", "reply_to_message_id"));
        rulespec.predicates.push(
            Predicate::new("reply_to", PredicateRule::Exists, InvariantSource::TaskPrompt)
                .with_when(WhenCondition::new("subject", PredicateRule::Matches)
                    .with_value(YamlValue::String("^Re: ".to_string()))),
        );

        let compiled = compile_rulespec(&rulespec, "test", 1).unwrap();

        // Reply email WITHOUT reply_to → when condition met, predicate should FAIL
        let mut envelope = ActionEnvelope::new();
        envelope.add_fact("subject", YamlValue::String("Re: Hello".to_string()));
        // No reply_to_message_id fact
        let facts = extract_facts(&envelope, &compiled);

        let result = execute_rules(&compiled, &facts);
        assert_eq!(result.failed_count, 1,
            "When matches met but exists fails → should fail, not vacuous pass");
    }

    #[test]
    fn test_execute_rules_when_matches_condition_not_met() {
        // Non-reply email → when condition not met → vacuous pass (skip)
        let mut rulespec = Rulespec::new();
        rulespec.claims.push(Claim::new("subject", "subject"));
        rulespec.claims.push(Claim::new("reply_to", "reply_to_message_id"));
        rulespec.predicates.push(
            Predicate::new("reply_to", PredicateRule::Exists, InvariantSource::TaskPrompt)
                .with_when(WhenCondition::new("subject", PredicateRule::Matches)
                    .with_value(YamlValue::String("^Re: ".to_string()))),
        );

        let compiled = compile_rulespec(&rulespec, "test", 1).unwrap();

        // Non-reply email → when not met → skip
        let mut envelope = ActionEnvelope::new();
        envelope.add_fact("subject", YamlValue::String("Hello World".to_string()));
        // No reply_to_message_id
        let facts = extract_facts(&envelope, &compiled);

        let result = execute_rules(&compiled, &facts);
        assert!(result.all_passed(), "Non-reply should get vacuous pass");
        assert!(result.predicate_results[0].reason.contains("Skipped"),
            "Should be skipped for non-reply");
    }

    // ========================================================================
    // format_datalog_program for new rules
    // ========================================================================

    #[test]
    fn test_format_datalog_program_new_rules() {
        let mut rulespec = Rulespec::new();
        rulespec.claims.push(Claim::new("caps", "feature.capabilities"));
        rulespec.claims.push(Claim::new("format", "feature.output_format"));

        rulespec.predicates.push(
            Predicate::new("caps", PredicateRule::NotContains, InvariantSource::TaskPrompt)
                .with_value(YamlValue::String("deprecated".to_string())),
        );
        rulespec.predicates.push(
            Predicate::new("format", PredicateRule::AnyOf, InvariantSource::TaskPrompt)
                .with_value(YamlValue::Sequence(vec![
                    YamlValue::String("json".to_string()),
                    YamlValue::String("yaml".to_string()),
                ])),
        );
        rulespec.predicates.push(
            Predicate::new("format", PredicateRule::NoneOf, InvariantSource::TaskPrompt)
                .with_value(YamlValue::Sequence(vec![
                    YamlValue::String("xml".to_string()),
                ])),
        );

        let compiled = compile_rulespec(&rulespec, "test-new-rules", 1).unwrap();

        let mut envelope = ActionEnvelope::new();
        envelope.add_fact("feature", serde_yaml::from_str("capabilities: [a, b]\noutput_format: json").unwrap());
        let facts = extract_facts(&envelope, &compiled);

        let dl = format_datalog_program(&compiled, &facts);

        // Verify header
        assert!(dl.contains("// Plan: test-new-rules"));

        // Verify not_contains rule
        assert!(dl.contains("not_contains"), "Should contain not_contains rule comment");
        assert!(dl.contains("predicate_pass(0)"));

        // Verify any_of rule
        assert!(dl.contains("any_of"), "Should contain any_of rule");
        assert!(dl.contains("predicate_pass(1)"));

        // Verify none_of rule
        assert!(dl.contains("none_of"), "Should contain none_of rule");
        assert!(dl.contains("predicate_pass(2)"));

        // Verify failure derivation for all
        assert!(dl.contains("predicate_fail(0) :- !predicate_pass(0)."));
        assert!(dl.contains("predicate_fail(1) :- !predicate_pass(1)."));
        assert!(dl.contains("predicate_fail(2) :- !predicate_pass(2)."));
    }
}
