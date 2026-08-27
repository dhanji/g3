//! Tests for `--no-auto-memory` actually disabling auto-memory.
//!
//! THE BUG THIS EXISTS TO PREVENT
//! ------------------------------
//! The auto-memory setting has TWO effects, and for a long time the flag only
//! governed one of them:
//!
//!   1. `send_auto_memory_reminder()` — an extra end-of-turn LLM round trip that
//!      says "MEMORY CHECKPOINT: ... call `remember` now". Gated by the flag. ✅
//!   2. A standing directive in the system prompt ("Memory is auto-loaded at
//!      startup. Call `remember` at end of turn when you discover code locations
//!      worth noting."), present on EVERY turn. NOT gated by the flag. ❌
//!
//! butler.app passed `--no-auto-memory` on every invocation and still watched its
//! 12k tier-1 memory budget fill to 99%, of which 65% was engineering notes about
//! g3 and butler internals — because effect (2) kept asking for exactly that,
//! turn after turn, while the flag reported that auto-memory was off.
//!
//! The fix hangs prompt rewriting off `set_auto_memory()` so the prompt cannot
//! disagree with the flag. These tests pin BOTH halves, because a fix to one is
//! indistinguishable from a fix to both if you only measure the reminder.

use g3_core::prompts::{
    apply_auto_memory, get_agent_system_prompt, get_system_prompt_for_native,
    get_system_prompt_for_non_native, remove_auto_memory_markers,
    strip_auto_memory_directive,
};

/// The exact sentence that caused the bloat. If this string changes in
/// `prompts/system/native.md`, this test SHOULD fail loudly rather than silently
/// stop measuring anything — a guard whose needle no longer exists is a guard
/// that always passes.
const DIRECTIVE: &str = "Call `remember` at end of turn";

#[test]
fn test_directive_actually_exists_in_the_shipped_prompt() {
    // Sanity-probe the needle before trusting any assertion about its absence.
    // Without this, every "stripped" assertion below would pass vacuously if the
    // prompt file were reworded.
    assert!(
        get_system_prompt_for_native().contains(DIRECTIVE),
        "the auto-memory directive is missing from the default prompt; the other \
         tests in this file are now measuring nothing. Update DIRECTIVE."
    );
}

// ---------------------------------------------------------------------------
// Happy path
// ---------------------------------------------------------------------------

#[test]
fn test_native_prompt_drops_directive_when_disabled() {
    let prompt = apply_auto_memory(&get_system_prompt_for_native(), false);
    assert!(!prompt.contains(DIRECTIVE));
    assert!(
        !prompt.contains("# Workspace Memory"),
        "the section heading is part of the directive and must go with it"
    );
}

#[test]
fn test_agent_prompt_drops_directive_when_disabled() {
    // butler's actual path: a custom agent prompt with the directive appended.
    let prompt = apply_auto_memory(&get_agent_system_prompt("You are butler.", true), false);
    assert!(!prompt.contains(DIRECTIVE));
    assert!(
        prompt.contains("You are butler."),
        "the agent's own identity must survive the strip"
    );
}

// ---------------------------------------------------------------------------
// Negative: the flag must not disable auto-memory for everyone
// ---------------------------------------------------------------------------

#[test]
fn test_directive_retained_when_enabled() {
    // The mutation that a happy-path-only suite would miss: stripping
    // unconditionally satisfies every "is it gone" assertion while breaking the
    // default behaviour of every other g3 agent.
    let prompt = apply_auto_memory(&get_system_prompt_for_native(), true);
    assert!(
        prompt.contains(DIRECTIVE),
        "auto-memory ON must keep instructing the model to remember"
    );
}

#[test]
fn test_enabled_and_disabled_differ() {
    // Guards against both branches collapsing to the same thing — the signal must
    // VARY with the input, or the test proves only that a function runs.
    let on = apply_auto_memory(&get_system_prompt_for_native(), true);
    let off = apply_auto_memory(&get_system_prompt_for_native(), false);
    assert_ne!(
        on, off,
        "the auto-memory setting must actually change the prompt"
    );
    assert!(
        off.len() < on.len(),
        "disabling should REMOVE text, not add it (on={}, off={})",
        on.len(),
        off.len()
    );
}

// ---------------------------------------------------------------------------
// Boundary
// ---------------------------------------------------------------------------

#[test]
fn test_markers_never_reach_the_model_either_way() {
    for enabled in [true, false] {
        let prompt = apply_auto_memory(&get_system_prompt_for_native(), enabled);
        assert!(
            !prompt.contains("AUTO-MEMORY"),
            "bookkeeping marker leaked into the prompt with auto_memory={}",
            enabled
        );
    }
}

#[test]
fn test_non_native_prompt_is_gated_too() {
    // Embedded models take a different assembly path (JSON tool format spliced
    // into the middle). A fix that only covers the native path leaves them
    // bloating memory exactly as before.
    let prompt = apply_auto_memory(&get_system_prompt_for_non_native(), false);
    assert!(!prompt.contains(DIRECTIVE));
    assert!(
        prompt.contains("Tool Call Format"),
        "the non-native tool instructions must survive the strip"
    );
}

#[test]
fn test_strip_is_idempotent() {
    // set_auto_memory() may be called more than once (agent mode sets it, a later
    // config pass could set it again). A second strip must not eat more text.
    let once = strip_auto_memory_directive(&get_system_prompt_for_native());
    let twice = strip_auto_memory_directive(&once);
    assert_eq!(once, twice, "stripping twice must equal stripping once");
}

#[test]
fn test_strip_leaves_unmarked_prompt_untouched() {
    // A hand-edited or older prompt file has no markers. Returning it unchanged is
    // the correct failure; a partial strip would corrupt the prompt silently.
    let plain = "# Prompt\n\nNo markers anywhere.";
    assert_eq!(strip_auto_memory_directive(plain), plain);
    assert_eq!(remove_auto_memory_markers(plain), plain);
}

#[test]
fn test_strip_preserves_surrounding_content() {
    // The directive sits at the END of native.md today, so a naive
    // truncate-at-marker would pass every test above. Assert with content on BOTH
    // sides that only the marked span is removed.
    let doc = format!(
        "BEFORE\n<!-- BEGIN AUTO-MEMORY -->\n{}\n<!-- END AUTO-MEMORY -->\nAFTER",
        DIRECTIVE
    );
    let stripped = strip_auto_memory_directive(&doc);
    assert!(stripped.contains("BEFORE"), "content before the span was lost");
    assert!(
        stripped.contains("AFTER"),
        "content after the span was lost — a truncating implementation would \
         pass while silently dropping the rest of the prompt: {:?}",
        stripped
    );
    assert!(!stripped.contains(DIRECTIVE));
}
