//! Observed token usage must reach `session.json` — and survive a resume.
//!
//! WHY THIS EXISTS
//! ---------------
//! g3 read `cache_read_input_tokens` off every Anthropic response and threw it
//! away. It reached no log, no UI and no session file, which made "is prompt
//! caching working?" unfalsifiable from outside the process: a cached run and
//! an uncached run produced byte-identical artifacts. Downstream, butler's cost
//! dashboard had no choice but to MODEL cache savings from a policy and
//! reconstruct token counts from character counts.
//!
//! So the thing under test is not a number, it is a CONTRACT: the block exists,
//! it is named `token_usage`, it carries the four token classes plus a
//! per-model split, and it accumulates across turns instead of being
//! overwritten by the latest one.
//!
//! WHAT THIS DELIBERATELY DOES NOT TEST
//! ------------------------------------
//! Whether Anthropic's numbers are correct — that is the provider's word and
//! the only thing we have. The wire-parse half (`message_delta` carrying the
//! final `output_tokens`) is unit-tested in `g3-providers/src/anthropic.rs`,
//! next to the structs it deserializes.

use g3_core::context_window::ContextWindow;
use g3_core::{session, CacheStats, ModelTokenUsage};
use g3_providers::{Message, MessageRole, Usage};
use serde_json::Value;

fn usage(prompt: u32, completion: u32, cache_create: u32, cache_read: u32) -> Usage {
    Usage {
        prompt_tokens: prompt,
        completion_tokens: completion,
        total_tokens: prompt + completion,
        cache_creation_tokens: cache_create,
        cache_read_tokens: cache_read,
    }
}

/// `save_context_window` writes into `$G3_WORKSPACE_PATH/.g3/sessions/<id>/`.
///
/// ⚠️ The workspace is selected by a PROCESS-GLOBAL env var, and cargo runs
/// tests in threads of one process. Without this mutex the cases silently
/// clobber each other's `G3_WORKSPACE_PATH` and fail with "no session.json" at
/// a path that belongs to a different case — which reads as a broken feature
/// rather than a broken harness. (Observed on the first run of this file.)
static WORKSPACE_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn with_workspace<T>(f: impl FnOnce(&std::path::Path) -> T) -> T {
    // A panicking case poisons the lock; the env var is reset on entry
    // regardless, so recovering is safe and keeps one failure from cascading
    // into "everything after it also failed".
    let _guard = WORKSPACE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let tmp = tempfile::TempDir::new().unwrap();
    std::env::set_var("G3_WORKSPACE_PATH", tmp.path());
    let out = f(tmp.path());
    std::env::remove_var("G3_WORKSPACE_PATH");
    out
}

fn read_session(root: &std::path::Path, id: &str) -> Value {
    let p = root.join(".g3").join("sessions").join(id).join("session.json");
    let text = std::fs::read_to_string(&p)
        .unwrap_or_else(|e| panic!("no session.json at {}: {}", p.display(), e));
    serde_json::from_str(&text).expect("session.json is not valid JSON")
}

fn context_with_one_message() -> ContextWindow {
    let mut cw = ContextWindow::new(200_000);
    cw.add_message(Message::new(MessageRole::User, "hello".to_string()));
    cw
}

// ── happy path ───────────────────────────────────────────────────────────────

#[test]
fn observed_usage_is_persisted_with_all_four_token_classes() {
    with_workspace(|root| {
        let mut stats = CacheStats::default();
        stats.record("claude-opus-5", &usage(1_000, 500, 20_000, 300_000));
        stats.record("claude-opus-5", &usage(1_200, 700, 0, 320_000));

        session::save_context_window(
            Some("s_happy"),
            &context_with_one_message(),
            &stats,
            "completed",
        );

        let data = read_session(root, "s_happy");
        let tu = data
            .get("token_usage")
            .expect("session.json must carry a token_usage block");

        assert_eq!(tu["api_calls"], 2);
        assert_eq!(tu["cache_hit_calls"], 2);
        assert_eq!(tu["input_tokens"], 2_200);
        // Output is the most expensive token class; a cost figure without it
        // is not a cost figure.
        assert_eq!(tu["output_tokens"], 1_200);
        assert_eq!(tu["cache_creation_tokens"], 20_000);
        assert_eq!(tu["cache_read_tokens"], 620_000);

        // The one number that answers "is caching on and working":
        // cache_read / (cache_read + input) = 620000 / 622200.
        let rate = tu["cache_hit_rate"].as_f64().expect("cache_hit_rate must be a number");
        assert!(
            (rate - 620_000.0 / 622_200.0).abs() < 1e-9,
            "cache_hit_rate must exclude cache_creation from the denominator, got {rate}"
        );

        // The existing contract must not have moved.
        assert!(data["context_window"]["conversation_history"].is_array());
        assert_eq!(data["status"], "completed");
    });
}

#[test]
fn usage_is_attributed_to_the_model_that_served_the_call() {
    // A session CAN span models: --fallback-model switches on overload, and
    // butler.app's picker can move a resumed chat from Sonnet to Opus. Opus is
    // ~2.5x Sonnet per token, so totals with no model attached cannot be
    // priced — a consumer would have to guess one model for the whole session,
    // which is the modelled number this change exists to delete.
    with_workspace(|root| {
        let mut stats = CacheStats::default();
        stats.record("claude-opus-5", &usage(1_000, 100, 0, 50_000));
        stats.record("claude-sonnet-5", &usage(2_000, 200, 0, 10_000));

        session::save_context_window(
            Some("s_models"),
            &context_with_one_message(),
            &stats,
            "completed",
        );

        let tu = read_session(root, "s_models")["token_usage"].clone();
        assert_eq!(tu["by_model"]["claude-opus-5"]["input_tokens"], 1_000);
        assert_eq!(tu["by_model"]["claude-opus-5"]["cache_read_tokens"], 50_000);
        assert_eq!(tu["by_model"]["claude-sonnet-5"]["output_tokens"], 200);
        assert_eq!(tu["by_model"]["claude-sonnet-5"]["api_calls"], 1);
        // Per-model must SUM to the totals, or one of the two is wrong and a
        // reader cannot tell which.
        assert_eq!(tu["input_tokens"], 3_000);
        assert_eq!(tu["cache_read_tokens"], 60_000);
    });
}

#[test]
fn persisted_block_round_trips_back_into_cache_stats() {
    // This is the RESUME contract. `g3 --resume` continues in place and
    // rewrites session.json, so without reading the prior totals back, turn 40
    // of a chat would persist only turn 40's tokens and the previous 39 turns'
    // spend would silently vanish from the record.
    with_workspace(|root| {
        let mut stats = CacheStats::default();
        stats.record("claude-opus-5", &usage(1_000, 500, 20_000, 300_000));
        stats.record("claude-sonnet-5", &usage(50, 60, 0, 0));

        session::save_context_window(
            Some("s_round"),
            &context_with_one_message(),
            &stats,
            "running",
        );

        let path = root
            .join(".g3")
            .join("sessions")
            .join("s_round")
            .join("session.json");
        let restored = session::read_token_usage(&path).expect("must read its own output back");

        assert_eq!(restored.total_calls, stats.total_calls);
        assert_eq!(restored.total_input_tokens, stats.total_input_tokens);
        assert_eq!(restored.total_output_tokens, stats.total_output_tokens);
        assert_eq!(
            restored.total_cache_creation_tokens,
            stats.total_cache_creation_tokens
        );
        assert_eq!(restored.total_cache_read_tokens, stats.total_cache_read_tokens);
        assert_eq!(restored.cache_hit_calls, stats.cache_hit_calls);
        assert_eq!(
            restored.by_model.get("claude-opus-5"),
            Some(&ModelTokenUsage {
                calls: 1,
                input_tokens: 1_000,
                output_tokens: 500,
                cache_creation_tokens: 20_000,
                cache_read_tokens: 300_000,
            })
        );

        // And a further turn accumulating ON TOP of the restored state must
        // add, not replace — this is what makes the persisted number a session
        // total rather than a last-turn total.
        let mut resumed = restored;
        resumed.record("claude-opus-5", &usage(10, 20, 0, 400_000));
        assert_eq!(resumed.total_calls, 3);
        assert_eq!(resumed.total_cache_read_tokens, 700_000);
    });
}

// ── negative: malformed input must degrade to None, never to partial totals ──

#[test]
fn malformed_or_absent_token_usage_reads_as_none() {
    // Degrading to `None` is the whole point: `None` means "fall back to the
    // estimate", whereas partial totals would be REPORTED AS MEASURED and be
    // wrong — worse than the estimate they replaced.
    let tmp = tempfile::TempDir::new().unwrap();

    let cases: Vec<(&str, &str)> = vec![
        // A session written by a g3 older than this change.
        ("legacy.json", r#"{"status":"completed","context_window":{}}"#),
        // Block present but not an object.
        ("wrong_type.json", r#"{"token_usage": 42}"#),
        ("null_block.json", r#"{"token_usage": null}"#),
        ("array_block.json", r#"{"token_usage": [1,2,3]}"#),
        // Object, but missing the field that identifies it as a usage block.
        ("no_calls.json", r#"{"token_usage": {"input_tokens": 10}}"#),
        // api_calls of the wrong type (a hand-edited or generated file).
        ("calls_string.json", r#"{"token_usage": {"api_calls": "7"}}"#),
        ("calls_negative.json", r#"{"token_usage": {"api_calls": -3}}"#),
        // Truncated file — a session being written right now.
        ("truncated.json", r#"{"token_usage": {"api_calls": 2, "inp"#),
    ];

    for (name, body) in cases {
        let p = tmp.path().join(name);
        std::fs::write(&p, body).unwrap();
        assert!(
            session::read_token_usage(&p).is_none(),
            "{name}: malformed/absent token_usage must read as None, not partial totals"
        );
    }

    // A file that does not exist at all.
    assert!(session::read_token_usage(&tmp.path().join("nope.json")).is_none());
}

#[test]
fn a_model_entry_that_is_not_an_object_is_skipped_not_fatal() {
    // One junk model key must not cost us the whole block: the totals are
    // still usable and are what the cost figure is built from.
    let v: Value = serde_json::from_str(
        r#"{"api_calls": 2, "input_tokens": 100, "cache_read_tokens": 900,
             "by_model": {"claude-opus-5": {"api_calls": 2, "input_tokens": 100},
                          "junk": "not-an-object"}}"#,
    )
    .unwrap();
    let stats = session::token_usage_from_json(&v).expect("totals are intact; must parse");
    assert_eq!(stats.total_calls, 2);
    assert_eq!(stats.total_input_tokens, 100);
    assert_eq!(stats.by_model.len(), 1, "the junk model key must be dropped");
    assert!(stats.by_model.contains_key("claude-opus-5"));
}

// ── boundary ────────────────────────────────────────────────────────────────

#[test]
fn a_session_that_observed_nothing_still_writes_a_zeroed_block() {
    // ABSENT and ZERO must mean different things. Absent = written by an older
    // g3, so estimate. Zero = this g3 records usage and this session used
    // none. Omitting empty blocks would collapse those two and put honest
    // sessions back on the estimator forever.
    with_workspace(|root| {
        session::save_context_window(
            Some("s_zero"),
            &context_with_one_message(),
            &CacheStats::default(),
            "running",
        );
        let tu = read_session(root, "s_zero")["token_usage"].clone();
        assert!(tu.is_object(), "the block must be present even when empty");
        assert_eq!(tu["api_calls"], 0);
        assert_eq!(tu["cache_read_tokens"], 0);
        // No division by zero, and no fake confidence: 0.0, with api_calls == 0
        // right there to say why.
        assert_eq!(tu["cache_hit_rate"].as_f64(), Some(0.0));
        assert!(
            tu["by_model"].as_object().map(|m| m.is_empty()).unwrap_or(false),
            "by_model must be an empty object, not null"
        );
    });
}

#[test]
fn an_empty_model_name_updates_totals_but_creates_no_by_model_key() {
    // get_provider_info() can fail (no provider registered), yielding "". A
    // zero-length key in the persisted JSON is indistinguishable from a real
    // model to a reader, which would then price it with a default — inventing
    // exactly the kind of guess this change removes.
    let mut stats = CacheStats::default();
    stats.record("", &usage(100, 10, 0, 5_000));
    assert_eq!(stats.total_calls, 1, "the call still happened and was still billed");
    assert_eq!(stats.total_cache_read_tokens, 5_000);
    assert!(stats.by_model.is_empty(), "no empty-string model key");
}

#[test]
fn cache_hit_rate_is_zero_when_nothing_was_observed() {
    assert_eq!(CacheStats::default().cache_hit_rate(), 0.0);

    // All-miss: a real 0% that is NOT a division artifact.
    let mut miss = CacheStats::default();
    miss.record("claude-opus-5", &usage(1_000, 100, 0, 0));
    assert_eq!(miss.cache_hit_rate(), 0.0);
    assert_eq!(miss.cache_hit_calls, 0);

    // All-hit: input_tokens 0 is the shape a fully-cached call has.
    let mut hit = CacheStats::default();
    hit.record("claude-opus-5", &usage(0, 100, 0, 500_000));
    assert_eq!(hit.cache_hit_rate(), 1.0);
    assert_eq!(hit.cache_hit_calls, 1);
}

#[test]
fn saving_without_a_session_id_still_records_usage() {
    // The anonymous path (`anonymous_<timestamp>`) must not be a hole in the
    // telemetry — those runs cost money too.
    with_workspace(|root| {
        let mut stats = CacheStats::default();
        stats.record("claude-opus-5", &usage(10, 20, 0, 30));
        session::save_context_window(None, &context_with_one_message(), &stats, "completed");

        let sessions = root.join(".g3").join("sessions");
        let dir = std::fs::read_dir(&sessions)
            .unwrap()
            .filter_map(|e| e.ok())
            .find(|e| e.file_name().to_string_lossy().starts_with("anonymous_"))
            .expect("an anonymous session dir must have been created");
        let text = std::fs::read_to_string(dir.path().join("session.json")).unwrap();
        let data: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(data["token_usage"]["cache_read_tokens"], 30);
    });
}
