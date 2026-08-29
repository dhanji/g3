//! Action Envelope tool - writes and verifies the action envelope.
//!
//! The `write_envelope` tool is the agent's explicit final step before
//! completing a plan. It:
//! 1. Parses the provided YAML facts into an ActionEnvelope
//! 2. Writes it to the session's `envelope.yaml`
//! 3. Runs `verify_envelope()` which compiles the rulespec and executes
//!    datalog verification in shadow form (results written to files, not
//!    injected into context)
//! 4. If all predicates pass, stamps the envelope with a verification token
//!    (`verified: "g3v1:<base64>"`) that proves deterministic checks passed.
//!
//! This creates a clear happens-before edge: envelope creation + verification
//! must complete before `plan_verify()` (triggered on plan completion) checks
//! that the envelope exists.

use anyhow::{anyhow, Result};
use base64::Engine;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use tracing::debug;

use crate::paths::get_session_logs_dir;
use crate::ui_writer::UiWriter;
use crate::ToolCall;

use super::executor::ToolContext;
use super::invariants::{
    get_envelope_path, read_envelope, read_rulespec,
    write_envelope, ActionEnvelope, Rulespec,
};
use super::datalog::{
    compile_rulespec, execute_rules, extract_facts, format_datalog_program,
    format_datalog_results,
};

// ============================================================================
// Verification Key Management
// ============================================================================

const VERIFICATION_KEY_FILENAME: &str = "verification.key";
const VERIFICATION_KEY_LEN: usize = 32;
const TOKEN_PREFIX: &str = "g3v1:";

/// Get the path to the global verification key: `~/.g3/verification.key`
fn get_verification_key_path() -> Result<PathBuf> {
    let home = std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .map_err(|_| anyhow!("Cannot determine home directory"))?;
    Ok(PathBuf::from(home).join(".g3").join(VERIFICATION_KEY_FILENAME))
}

/// Read or create the verification key at `~/.g3/verification.key`.
///
/// - If the key file exists, reads and returns it.
/// - If it doesn't exist, generates 32 random bytes, writes them with
///   mode 600 (Unix), and returns the key.
/// - The key is raw bytes, never logged or shown to the LLM.
pub fn get_or_create_verification_key() -> Result<Vec<u8>> {
    let path = get_verification_key_path()?;

    // If key exists, read and return it
    if path.exists() {
        let key = std::fs::read(&path)?;
        if key.len() == VERIFICATION_KEY_LEN {
            return Ok(key);
        }
        // Key file is wrong size — regenerate
        debug!("Verification key has wrong size ({}), regenerating", key.len());
    }

    // Ensure ~/.g3/ directory exists
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    // Generate 32 random bytes
    use rand::Rng;
    let mut rng = rand::thread_rng();
    let mut key = vec![0u8; VERIFICATION_KEY_LEN];
    rng.fill(&mut key[..]);

    // Write key file
    std::fs::write(&path, &key)?;

    // Set permissions to 600 (owner read/write only) on Unix
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let perms = std::fs::Permissions::from_mode(0o600);
        std::fs::set_permissions(&path, perms)?;
    }

    debug!("Generated new verification key at {}", path.display());
    Ok(key)
}

/// Read the verification key. Returns None if it doesn't exist.
pub fn read_verification_key() -> Result<Option<Vec<u8>>> {
    let path = get_verification_key_path()?;
    if !path.exists() {
        return Ok(None);
    }
    let key = std::fs::read(&path)?;
    if key.len() != VERIFICATION_KEY_LEN {
        return Ok(None);
    }
    Ok(Some(key))
}

// ============================================================================
// Token Computation
// ============================================================================

/// Compute a canonical YAML representation of the envelope facts.
///
/// This produces a deterministic string by sorting keys, ensuring
/// the same facts always produce the same canonical form.
fn canonical_facts_yaml(envelope: &ActionEnvelope) -> String {
    // Use serde_yaml which produces deterministic output for the same structure.
    // We serialize only the facts (not the verified field) to get a stable input.
    let mut sorted_facts: Vec<(&String, &serde_yaml::Value)> =
        envelope.facts.iter().collect();
    sorted_facts.sort_by_key(|(k, _)| *k);

    let mut mapping = serde_yaml::Mapping::new();
    for (k, v) in sorted_facts {
        mapping.insert(
            serde_yaml::Value::String(k.clone()),
            v.clone(),
        );
    }
    serde_yaml::to_string(&mapping).unwrap_or_default()
}

/// Compute a canonical YAML representation of the rulespec.
fn canonical_rulespec_yaml(rulespec: &Rulespec) -> String {
    serde_yaml::to_string(rulespec).unwrap_or_default()
}

/// Mint a verification token using a keyed SipHash MAC.
///
/// The token is computed as:
///   SipHash-2-4(key[0..16], canonical_facts || "\x00" || canonical_rulespec)
///
/// Then encoded as: `g3v1:<base64(8-byte hash)>`
///
/// This is a keyed PRF (pseudo-random function) — not a plain hash.
/// The key is never exposed to the LLM, making the token unguessable.
pub fn mint_token(key: &[u8], envelope: &ActionEnvelope, rulespec: &Rulespec) -> String {
    let facts_yaml = canonical_facts_yaml(envelope);
    let rulespec_yaml = canonical_rulespec_yaml(rulespec);

    let hash = compute_keyed_hash(key, &facts_yaml, &rulespec_yaml);
    let encoded = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(hash.to_le_bytes());

    format!("{}{}", TOKEN_PREFIX, encoded)
}

/// Compute a keyed SipHash over the canonical content.
///
/// Uses the first 16 bytes of the key as SipHash key (k0, k1).
/// The message is: facts_yaml + NUL separator + rulespec_yaml.
fn compute_keyed_hash(key: &[u8], facts_yaml: &str, rulespec_yaml: &str) -> u64 {
    // Extract k0 and k1 from the key (first 16 bytes)
    let k0 = if key.len() >= 8 {
        u64::from_le_bytes(key[0..8].try_into().unwrap())
    } else {
        0
    };
    let k1 = if key.len() >= 16 {
        u64::from_le_bytes(key[8..16].try_into().unwrap())
    } else {
        0
    };

    #[allow(deprecated)] // SipHasher is deprecated but we need keyed hashing
    let mut hasher = std::hash::SipHasher::new_with_keys(k0, k1);
    facts_yaml.hash(&mut hasher);
    0u8.hash(&mut hasher); // NUL separator
    rulespec_yaml.hash(&mut hasher);
    hasher.finish()
}

// ============================================================================
// Token Verification (cross-process)
// ============================================================================

/// Verify the token in an envelope against the verification key and rulespec.
///
/// This is the cross-process verification entry point. It:
/// 1. Reads `~/.g3/verification.key`
/// 2. Reads the envelope from the session
/// 3. Reads the rulespec from the working directory
/// 4. Recomputes the token and compares
///
/// Returns:
/// - `Ok(true)` if the token matches
/// - `Ok(false)` if the token doesn't match or is missing
/// - `Err(...)` if required files are missing
pub fn verify_token(session_id: &str, working_dir: &Path) -> Result<bool> {
    // Read verification key
    let key = match read_verification_key()? {
        Some(k) => k,
        None => return Err(anyhow!("Verification key not found at ~/.g3/verification.key")),
    };

    // Read envelope
    let envelope = match read_envelope(session_id)? {
        Some(e) => e,
        None => return Err(anyhow!("Envelope not found for session {}", session_id)),
    };

    // Check that envelope has a verified field
    let stored_token = match &envelope.verified {
        Some(t) => t.clone(),
        None => return Ok(false),
    };

    // Read rulespec
    let rulespec = match read_rulespec(working_dir)? {
        Some(rs) => rs,
        None => return Err(anyhow!("Rulespec not found at {}/analysis/rulespec.yaml", working_dir.display())),
    };

    // Recompute token (using envelope without the verified field for computation)
    let mut clean_envelope = envelope.clone();
    clean_envelope.verified = None;
    let expected_token = mint_token(&key, &clean_envelope, &rulespec);

    Ok(stored_token == expected_token)
}

// ============================================================================
// Envelope Verification
// ============================================================================

/// Result of the envelope verification pipeline.
pub struct VerifyResult {
    /// Pipeline stages completed: (flat_icon, description)
    pub stages: Vec<(String, String)>,
    /// Number of predicates that passed (None if no rulespec)
    pub passed: Option<usize>,
    /// Total number of predicates (None if no rulespec)
    pub total: Option<usize>,
    /// Number of predicates that failed
    pub failed: usize,
    /// Short summary for LLM context
    pub llm_summary: String,
}

// ============================================================================
// Tool Implementation
// ============================================================================

/// Execute the `write_envelope` tool.
///
/// Accepts YAML facts, writes the action envelope, runs verification,
/// and displays a compact pipeline summary via the UI writer.
pub async fn execute_write_envelope<W: UiWriter>(
    tool_call: &ToolCall,
    ctx: &mut ToolContext<'_, W>,
) -> Result<String> {
    debug!("Processing write_envelope tool call");

    let session_id = match ctx.session_id {
        Some(id) => id,
        None => return Ok("Error: No active session - envelopes are session-scoped.".to_string()),
    };

    // Get the facts YAML from args
    let facts_yaml = match tool_call.args.get("facts").and_then(|v| v.as_str()) {
        Some(f) => f,
        None => return Ok("Error: Missing 'facts' argument. Provide the envelope facts as YAML.".to_string()),
    };

    // Parse the YAML into an ActionEnvelope
    let envelope: ActionEnvelope = match serde_yaml::from_str(facts_yaml) {
        Ok(e) => e,
        Err(e) => return Ok(format!("Error: Invalid envelope YAML: {}", e)),
    };

    // Validate that facts is non-empty
    if envelope.facts.is_empty() {
        return Ok(
            "Error: Envelope has empty facts. The YAML must contain a non-empty `facts` top-level key. Example:\n\n\
             ```yaml\n\
             facts:\n\
             \x20 my_feature:\n\
             \x20   capabilities: [feature_a, feature_b]\n\
             \x20   file: \"src/my_feature.rs\"\n\
             ```".to_string()
        );
    }

    // Write the envelope to disk (without verified token initially)
    if let Err(e) = write_envelope(session_id, &envelope) {
        return Ok(format!("Error: Failed to write envelope: {}", e));
    }

    let fact_groups = envelope.facts.len();

    // Run verification pipeline
    let effective_wd = ctx.working_dir
        .map(Path::new)
        .unwrap_or_else(|| Path::new("."));
    let vr = verify_envelope(session_id, effective_wd);

    // Display compact pipeline via UI writer
    let stage_refs: Vec<(&str, &str)> = vr.stages.iter()
        .map(|(icon, desc)| (icon.as_str(), desc.as_str()))
        .collect();
    ctx.ui_writer.print_envelope_compact(fact_groups, &stage_refs, vr.passed, vr.total, vr.failed);

    Ok(vr.llm_summary)
}

// ============================================================================
// Envelope Verification Pipeline
// ============================================================================

/// Verify the action envelope against the compiled rulespec using datalog.
///
/// Returns a `VerifyResult` with pipeline stages and verification counts.
/// Stages are displayed as compact lines with flat icons.
pub fn verify_envelope(session_id: &str, working_dir: &Path) -> VerifyResult {
    let mut stages: Vec<(String, String)> = Vec::new();

    // Stage 1: envelope written
    stages.push(("✎".into(), "envelope written".into()));
    let envelope_path = get_envelope_path(session_id);
    let path_display = envelope_path.display();

    // Read rulespec from analysis/rulespec.yaml
    let rulespec = match read_rulespec(working_dir) {
        Ok(Some(rs)) => rs,
        Ok(None) => {
            eprintln!("  -- no analysis/rulespec.yaml found, skipping verification");
            return VerifyResult {
                stages,
                passed: None, total: None, failed: 0,
                llm_summary: format!("Envelope written to: {}\nNo rulespec — skipping verification.", path_display),
            };
        }
        Err(e) => {
            eprintln!("  !! failed to read rulespec: {}", e);
            return VerifyResult {
                stages,
                passed: None, total: None, failed: 0,
                llm_summary: format!("Envelope written to: {}\nFailed to read rulespec: {}", path_display, e),
            };
        }
    };

    // Stage 2: compile rulespec
    let compiled = match compile_rulespec(&rulespec, "envelope-verify", 0) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("  !! failed to compile rulespec: {}", e);
            return VerifyResult {
                stages,
                passed: None, total: None, failed: 0,
                llm_summary: format!("Envelope written to: {}\nFailed to compile rulespec: {}", path_display, e),
            };
        }
    };

    if compiled.is_empty() {
        eprintln!("  -- rulespec has no predicates, skipping verification");
        return VerifyResult {
            stages,
            passed: None, total: None, failed: 0,
            llm_summary: format!("Envelope written to: {}\nRulespec has no predicates.", path_display),
        };
    }

    let pred_count = compiled.predicates.len();
    stages.push(("\u{2699}".into(), format!("rulespec compiled ({} predicates)", pred_count)));

    // Load envelope back from disk (to verify what was actually written)
    let envelope = match read_envelope(session_id) {
        Ok(Some(e)) => e,
        Ok(None) => {
            eprintln!("  !! no envelope found after write");
            return VerifyResult {
                stages,
                passed: None, total: None, failed: 0,
                llm_summary: format!("Envelope written to: {}\nCould not be re-read for verification.", path_display),
            };
        }
        Err(e) => {
            eprintln!("  !! failed to load envelope: {}", e);
            return VerifyResult {
                stages,
                passed: None, total: None, failed: 0,
                llm_summary: format!("Envelope written to: {}\nFailed to re-read: {}", path_display, e),
            };
        }
    };

    // Extract facts and execute datalog rules
    let facts = extract_facts(&envelope, &compiled);
    let result = execute_rules(&compiled, &facts);

    // Write artifacts to session dir (shadow mode)
    let session_dir = get_session_logs_dir(session_id);
    let dl_path = session_dir.join("rulespec.compiled.dl");
    let datalog_program = format_datalog_program(&compiled, &facts);
    if let Err(e) = std::fs::write(&dl_path, &datalog_program) {
        eprintln!("  !! failed to write compiled rules: {}", e);
    }
    let eval_output = format_datalog_results(&result);
    let eval_path = session_dir.join("datalog_evaluation.txt");
    if let Err(e) = std::fs::write(&eval_path, &eval_output) {
        eprintln!("  !! failed to write evaluation report: {}", e);
    }

    // Stage 3: verification result
    let total = result.passed_count + result.failed_count;
    let passed = result.passed_count;
    let failed = result.failed_count;

    if failed == 0 {
        stages.push(("\u{2713}".into(), format!("verified {}/{}", passed, total)));
    } else {
        stages.push(("\u{2717}".into(), format!("verified {}/{}, {} failed", passed, total, failed)));
    }

    // Stage 4: stamp if all passed
    if failed == 0 && passed > 0 {
        match stamp_envelope(session_id, &envelope, &rulespec) {
            Ok(_) => {
                stages.push(("\u{2235}".into(), "token stamped".into()));
                eprintln!("  -- envelope stamped with verification token");
            }
            Err(e) => {
                eprintln!("  !! failed to stamp envelope: {}", e);
            }
        }
    }

    // LLM summary (token value intentionally omitted)
    //
    // The path IS included. Without it, callers had no reliable way to find
    // their own envelope file -- observed 2026-08-30: a duty guessed
    // `ls -t .g3/sessions/ | head -1` ("most recently modified session dir")
    // to locate it for the subsequent `email_sender.py send --envelope`
    // call. That guess is wrong under any concurrency: a DIFFERENT session
    // touching its own dir more recently makes this resolve to somebody
    // else's envelope, silently. The path is deterministic and already
    // known here -- there is no reason to make the caller reconstruct it.
    let llm_summary = if failed == 0 {
        format!("Envelope written to: {}\nVerification: {}/{} passed.", path_display, passed, total)
    } else {
        format!("Envelope written to: {}\nVerification: {}/{} passed, {} failed.", path_display, passed, total, failed)
    };

    VerifyResult {
        stages,
        passed: Some(passed as usize),
        total: Some(total as usize),
        failed: failed as usize,
        llm_summary,
    }
}

/// Stamp an envelope with a verification token and re-write it to disk.
///
/// This is called only when all rulespec predicates pass. It:
/// 1. Gets or creates the verification key
/// 2. Computes the token over the canonical facts + rulespec
/// 3. Sets the `verified` field on the envelope
/// 4. Re-writes the envelope to disk
fn stamp_envelope(
    session_id: &str,
    envelope: &ActionEnvelope,
    rulespec: &Rulespec,
) -> Result<()> {
    let key = get_or_create_verification_key()?;

    // Compute token over the envelope without any previous verified field
    let mut clean_envelope = envelope.clone();
    clean_envelope.verified = None;
    let token = mint_token(&key, &clean_envelope, rulespec);

    clean_envelope.verified = Some(token);
    write_envelope(session_id, &clean_envelope)
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use serde_yaml::Value as YamlValue;

    // ── verify_envelope: the summary must name the envelope's own path ─────
    //
    // 2026-08-30: a duty guessed the path of its OWN just-written envelope
    // via `ls -t .g3/sessions/ | head -1`, because verify_envelope's summary
    // never said where the file was. That guess resolves to the wrong
    // session under any concurrency. These pin the fix at the seam most
    // likely to regress: whatever function assembles `llm_summary`.

    #[test]
    fn test_verify_envelope_summary_contains_session_path_on_pass() {
        use tempfile::TempDir;
        use crate::paths::ensure_session_dir;
        let tmp = TempDir::new().unwrap();
        // A REAL rulespec.yaml on disk, so this exercises the full
        // compile+evaluate+stamp path -- the branch the original bug
        // actually hit (verification passed, THEN the caller couldn't find
        // the file). The "no rulespec" test below covers the early-return
        // branch separately; conflating them would leave the pass path
        // unpinned.
        let analysis_dir = tmp.path().join("analysis");
        std::fs::create_dir_all(&analysis_dir).unwrap();
        std::fs::write(
            analysis_dir.join("rulespec.yaml"),
            "claims:\n  - name: caps\n    selector: feature.capabilities\n\
             predicates:\n  - claim: caps\n    rule: exists\n    source: task_prompt\n",
        ).unwrap();

        let session_id = "test-session-path-happy";
        ensure_session_dir(session_id).unwrap();
        let mut envelope = ActionEnvelope::new();
        envelope.add_fact(
            "feature",
            serde_yaml::from_str("capabilities: [a, b]").unwrap(),
        );
        write_envelope(session_id, &envelope).unwrap();

        let vr = verify_envelope(session_id, tmp.path());
        assert_eq!(vr.failed, 0, "fixture rulespec should pass cleanly");
        let expected_path = get_envelope_path(session_id);
        assert!(
            vr.llm_summary.contains(&expected_path.display().to_string()),
            "llm_summary must contain the envelope's own path so a caller \
             never has to guess it: got {:?}",
            vr.llm_summary
        );
    }

    #[test]
    fn test_verify_envelope_summary_path_survives_no_rulespec() {
        // Boundary: the "no rulespec found" branch is a DIFFERENT early
        // return than the full-verification path -- must carry the path too,
        // or a workspace without analysis/rulespec.yaml regresses silently.
        use tempfile::TempDir;
        use crate::paths::ensure_session_dir;
        let tmp = TempDir::new().unwrap(); // empty dir, no analysis/
        let session_id = "test-session-path-no-rulespec";
        ensure_session_dir(session_id).unwrap();
        let mut envelope = ActionEnvelope::new();
        envelope.add_fact("feature", YamlValue::String("x".into()));
        write_envelope(session_id, &envelope).unwrap();

        let vr = verify_envelope(session_id, tmp.path());
        assert!(vr.llm_summary.contains("No rulespec"));
        let expected_path = get_envelope_path(session_id);
        assert!(vr.llm_summary.contains(&expected_path.display().to_string()));
    }

    fn make_test_key() -> Vec<u8> {
        vec![1u8; VERIFICATION_KEY_LEN]
    }

    fn make_different_key() -> Vec<u8> {
        vec![2u8; VERIFICATION_KEY_LEN]
    }

    fn make_test_envelope() -> ActionEnvelope {
        let mut envelope = ActionEnvelope::new();
        envelope.add_fact(
            "feature",
            serde_yaml::from_str("capabilities: [a, b]\nfile: src/foo.rs").unwrap(),
        );
        envelope
    }

    fn make_test_rulespec() -> Rulespec {
        serde_yaml::from_str(
            r#"
            claims:
              - name: caps
                selector: feature.capabilities
            predicates:
              - claim: caps
                rule: exists
                source: task_prompt
            "#,
        )
        .unwrap()
    }

    #[test]
    fn test_mint_token_deterministic() {
        let key = make_test_key();
        let envelope = make_test_envelope();
        let rulespec = make_test_rulespec();

        let token1 = mint_token(&key, &envelope, &rulespec);
        let token2 = mint_token(&key, &envelope, &rulespec);

        assert_eq!(token1, token2, "Same inputs must produce same token");
        assert!(token1.starts_with(TOKEN_PREFIX), "Token must start with g3v1:");
    }

    #[test]
    fn test_mint_token_different_key() {
        let envelope = make_test_envelope();
        let rulespec = make_test_rulespec();

        let token1 = mint_token(&make_test_key(), &envelope, &rulespec);
        let token2 = mint_token(&make_different_key(), &envelope, &rulespec);

        assert_ne!(token1, token2, "Different keys must produce different tokens");
    }

    #[test]
    fn test_mint_token_different_facts() {
        let key = make_test_key();
        let rulespec = make_test_rulespec();

        let envelope1 = make_test_envelope();
        let mut envelope2 = make_test_envelope();
        envelope2.add_fact("extra", YamlValue::String("tampered".to_string()));

        let token1 = mint_token(&key, &envelope1, &rulespec);
        let token2 = mint_token(&key, &envelope2, &rulespec);

        assert_ne!(token1, token2, "Different facts must produce different tokens");
    }

    #[test]
    fn test_mint_token_different_rulespec() {
        let key = make_test_key();
        let envelope = make_test_envelope();

        let rulespec1 = make_test_rulespec();
        let rulespec2: Rulespec = serde_yaml::from_str(
            r#"
            claims:
              - name: caps
                selector: feature.capabilities
            predicates:
              - claim: caps
                rule: min_length
                value: 5
                source: task_prompt
            "#,
        )
        .unwrap();

        let token1 = mint_token(&key, &envelope, &rulespec1);
        let token2 = mint_token(&key, &envelope, &rulespec2);

        assert_ne!(token1, token2, "Different rulespec must produce different tokens");
    }

    #[test]
    fn test_mint_token_format() {
        let key = make_test_key();
        let envelope = make_test_envelope();
        let rulespec = make_test_rulespec();

        let token = mint_token(&key, &envelope, &rulespec);

        assert!(token.starts_with("g3v1:"));
        let b64_part = &token[5..];
        // base64 URL-safe no-pad should decode to 8 bytes (u64)
        let decoded = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(b64_part)
            .expect("Token should be valid base64");
        assert_eq!(decoded.len(), 8, "SipHash produces 8 bytes");
    }

    #[test]
    fn test_fabricated_token_fails() {
        let key = make_test_key();
        let envelope = make_test_envelope();
        let rulespec = make_test_rulespec();

        let real_token = mint_token(&key, &envelope, &rulespec);
        let fake_token = "g3v1:AAAAAAAAAA".to_string();

        assert_ne!(real_token, fake_token, "Fabricated token should not match");
    }

    #[test]
    fn test_envelope_verified_field_serialization() {
        let mut envelope = make_test_envelope();
        envelope.verified = Some("g3v1:test123".to_string());

        let yaml = serde_yaml::to_string(&envelope).unwrap();
        assert!(yaml.contains("verified:"), "YAML should contain verified field");
        assert!(yaml.contains("g3v1:test123"), "YAML should contain token value");
    }

    #[test]
    fn test_envelope_without_verified_field_backward_compat() {
        // Simulate an old envelope YAML without verified field
        let yaml = r#"
            facts:
              feature:
                capabilities: [a, b]
        "#;

        let envelope: ActionEnvelope = serde_yaml::from_str(yaml).unwrap();
        assert!(envelope.verified.is_none(), "Old envelopes should parse with verified=None");
        assert!(!envelope.facts.is_empty());
    }

    #[test]
    fn test_envelope_verified_not_in_to_yaml_value() {
        let mut envelope = make_test_envelope();
        envelope.verified = Some("g3v1:test123".to_string());

        let yaml_value = envelope.to_yaml_value();
        let yaml_str = serde_yaml::to_string(&yaml_value).unwrap();

        assert!(!yaml_str.contains("verified"),
            "to_yaml_value() must not include verified field");
        assert!(!yaml_str.contains("g3v1"),
            "to_yaml_value() must not include token");
    }

    #[test]
    fn test_envelope_verified_none_not_serialized() {
        let envelope = make_test_envelope();
        assert!(envelope.verified.is_none());

        let yaml = serde_yaml::to_string(&envelope).unwrap();
        assert!(!yaml.contains("verified"),
            "None verified should not appear in YAML");
    }

    #[test]
    fn test_canonical_facts_yaml_deterministic() {
        let envelope = make_test_envelope();

        let yaml1 = canonical_facts_yaml(&envelope);
        let yaml2 = canonical_facts_yaml(&envelope);

        assert_eq!(yaml1, yaml2, "Canonical YAML must be deterministic");
    }

    #[test]
    fn test_canonical_facts_yaml_sorted_keys() {
        let mut envelope = ActionEnvelope::new();
        envelope.add_fact("zebra", YamlValue::String("z".to_string()));
        envelope.add_fact("alpha", YamlValue::String("a".to_string()));
        envelope.add_fact("middle", YamlValue::String("m".to_string()));

        let yaml = canonical_facts_yaml(&envelope);

        let alpha_pos = yaml.find("alpha").unwrap();
        let middle_pos = yaml.find("middle").unwrap();
        let zebra_pos = yaml.find("zebra").unwrap();

        assert!(alpha_pos < middle_pos, "Keys should be sorted");
        assert!(middle_pos < zebra_pos, "Keys should be sorted");
    }
}
