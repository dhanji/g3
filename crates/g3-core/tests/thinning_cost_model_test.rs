//! Cache-aware cost model for **context thinning** (plan item T1).
//!
//! # Why this exists
//!
//! `thin_context()` rewrites messages in the FIRST THIRD of history
//! (`[0, len/3)`), while the rolling Anthropic cache breakpoint slides to the
//! END of history every 10 tool calls. Those two facts collide: thinning almost
//! always mutates bytes that sit *inside* the cached prefix, which invalidates
//! the cache from that offset and forces the next request to pay the 1.25x
//! cache-write rate instead of reading at 0.1x.
//!
//! Measured on 94 real sessions: **85.9% (median) of thinning writes land
//! inside the cached prefix**, and immediate thinning is net-negative in 12 of
//! the 19 sessions that ever thin.
//!
//! This harness models `should_thin()`'s real trigger and `thin_context()`'s
//! real target so the fix can be evaluated arithmetically. It deliberately
//! parameterises the trigger FLOOR, because "should we thin at 20% instead of
//! 50%?" must be answered by measurement rather than intuition.
//!
//! # What this model does NOT capture
//!
//! There is no term for **accuracy loss**. Evicting context always looks free
//! here. That is why a monotonic "lower floor is always better" result from
//! this model is a warning that the model is incomplete, not a licence to set
//! the floor to zero. See `analysis/thinning_cache_analysis.md`.

use g3_providers::{Message, MessageRole, MessageToolCall};

// ============================================================================
// Constants mirroring production behaviour
// ============================================================================

const RATE_FRESH: f64 = 1.0;
const RATE_CACHE_READ: f64 = 0.1;
const RATE_CACHE_WRITE: f64 = 1.25;

/// g3 slides its rolling cache breakpoint every 10 tool calls.
/// See `Agent::stream_completion_with_tools` (`crates/g3-core/src/lib.rs`).
const CACHE_BREAKPOINT_EVERY: usize = 10;

/// `should_thin()` stops firing above this percentage; beyond it compaction
/// takes over. Mirrors `current_threshold <= 80`.
const THIN_CEILING_PERCENT: u32 = 80;

/// `collect_thin_modifications()` ignores tool results below this size.
const THIN_SIZE_FLOOR_CHARS: usize = 500;

/// Tokens a thinned message collapses to ("Tool result saved to <path>").
const THINNED_TOKENS: f64 = 15.0;

// ============================================================================
// Token estimation (mirrors ContextWindow::estimate_message_tokens)
// ============================================================================

fn estimate_tokens(text: &str) -> f64 {
    let base = if text.contains('{') || text.contains("```") || text.contains("fn ") {
        (text.len() as f64) / 3.0
    } else {
        (text.len() as f64) / 4.0
    };
    (base * 1.1).ceil()
}

fn estimate_message_tokens(m: &Message) -> f64 {
    let mut total = estimate_tokens(&m.content);
    for tc in &m.tool_calls {
        total += ((tc.input.to_string().len() as f64) / 3.0 * 1.1).ceil() + 20.0;
    }
    total
}

// ============================================================================
// Model
// ============================================================================

/// When a latched thin request is actually applied.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThinTiming {
    /// Never thin. The control arm.
    Never,
    /// Today's behaviour: thin the instant the threshold is crossed.
    Immediate,
    /// Proposed: latch on threshold crossing, apply at the next cache-breakpoint
    /// slide so the prefix was going to be rewritten anyway.
    CacheAligned,
}

/// Trigger policy for thinning.
#[derive(Debug, Clone, Copy)]
pub struct ThinPolicy {
    /// Percentage of context at or above which thinning may fire (today: 50).
    pub floor_percent: u32,
    pub timing: ThinTiming,
}

impl ThinPolicy {
    pub fn never() -> Self {
        Self { floor_percent: 50, timing: ThinTiming::Never }
    }
    pub fn immediate(floor_percent: u32) -> Self {
        Self { floor_percent, timing: ThinTiming::Immediate }
    }
    pub fn cache_aligned(floor_percent: u32) -> Self {
        Self { floor_percent, timing: ThinTiming::CacheAligned }
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct ThinCostReport {
    pub requests: usize,
    pub raw_input_tokens: f64,
    pub effective_cost: f64,
    pub cache_read_tokens: f64,
    pub cache_write_tokens: f64,
    pub fresh_tokens: f64,
    /// How many times a thin was actually applied.
    pub thin_events: usize,
    /// How many individual messages were collapsed.
    pub messages_thinned: usize,
    /// Thin applications that mutated a message inside the cached prefix.
    pub thins_hitting_cache: usize,
    /// Peak context size in tokens across the whole replay.
    pub peak_tokens: f64,
    /// Ages (in tool calls) of content at the moment it was thinned.
    pub evicted_ages: Vec<usize>,
}

impl ThinCostReport {
    pub fn savings_vs(&self, baseline: &ThinCostReport) -> f64 {
        if baseline.effective_cost <= 0.0 {
            return 0.0;
        }
        100.0 - (self.effective_cost * 100.0 / baseline.effective_cost)
    }

    pub fn min_evicted_age(&self) -> Option<usize> {
        self.evicted_ages.iter().copied().min()
    }
}

/// One entry in the simulated live context.
#[derive(Debug, Clone)]
struct Entry {
    tokens: f64,
    is_tool_result: bool,
    /// Original char length, used for the 500-char thinning floor.
    chars: usize,
    thinned: bool,
    /// Tool-call ordinal when this entry entered the context.
    born_at: usize,
}

fn is_tool_result(m: &Message) -> bool {
    m.tool_result_id.is_some() || m.content.starts_with("Tool result")
}

/// Replay `messages` under `policy`, pricing every provider request.
///
/// `total_tokens` is the context-window size that percentages are measured
/// against — the same denominator `ContextWindow::percentage_used()` uses.
pub fn simulate(messages: &[Message], policy: ThinPolicy, total_tokens: f64) -> ThinCostReport {
    let mut ctx: Vec<Entry> = Vec::new();
    let mut r = ThinCostReport::default();

    let mut cached_upto: usize = 0;
    let mut cache_dirty = false;
    let mut tool_calls: usize = 0;

    // Mirrors ContextWindow::last_thinning_percentage.
    let mut last_thinning_percentage: u32 = 0;
    let mut pending_thin = false;

    for m in messages {
        ctx.push(Entry {
            tokens: estimate_message_tokens(m),
            is_tool_result: is_tool_result(m),
            chars: m.content.len(),
            thinned: false,
            born_at: tool_calls,
        });

        if !is_tool_result(m) {
            continue;
        }
        tool_calls += 1;

        let used: f64 = ctx.iter().map(|e| e.tokens).sum();
        r.peak_tokens = r.peak_tokens.max(used);

        // --- should_thin(): 10% steps between floor and ceiling ---
        if policy.timing != ThinTiming::Never {
            let pct = ((used / total_tokens) * 100.0) as u32;
            if pct >= policy.floor_percent {
                let threshold = (pct / 10) * 10;
                if threshold > last_thinning_percentage && threshold <= THIN_CEILING_PERCENT {
                    last_thinning_percentage = threshold;
                    pending_thin = true;
                }
            }
        }

        let at_breakpoint = tool_calls % CACHE_BREAKPOINT_EVERY == 0;
        let apply_now = pending_thin
            && match policy.timing {
                ThinTiming::Never => false,
                ThinTiming::Immediate => true,
                ThinTiming::CacheAligned => at_breakpoint,
            };

        if apply_now {
            let (count, lowest) = apply_thin_first_third(&mut ctx, tool_calls, &mut r.evicted_ages);
            if count > 0 {
                r.thin_events += 1;
                r.messages_thinned += count;
                if let Some(idx) = lowest {
                    if idx < cached_upto {
                        r.thins_hitting_cache += 1;
                        // Mutating inside the cached prefix invalidates it from
                        // `idx` onward. Under CacheAligned this coincides with a
                        // breakpoint slide, so the write was already being paid.
                        cached_upto = idx;
                        if policy.timing == ThinTiming::Immediate {
                            cache_dirty = true;
                        }
                    }
                }
            }
            pending_thin = false;
        }

        if at_breakpoint {
            cached_upto = ctx.len();
            cache_dirty = true;
        }

        // --- price this request ---
        let split = cached_upto.min(ctx.len());
        let cached: f64 = ctx[..split].iter().map(|e| e.tokens).sum();
        let fresh: f64 = ctx[split..].iter().map(|e| e.tokens).sum();

        if cache_dirty {
            r.cache_write_tokens += cached;
            r.effective_cost += cached * RATE_CACHE_WRITE;
            cache_dirty = false;
        } else {
            r.cache_read_tokens += cached;
            r.effective_cost += cached * RATE_CACHE_READ;
        }
        r.fresh_tokens += fresh;
        r.effective_cost += fresh * RATE_FRESH;
        r.raw_input_tokens += cached + fresh;
        r.requests += 1;
    }

    r
}

/// Thin large, un-thinned tool results in the first third of history.
/// Returns `(messages_collapsed, lowest_mutated_index)`.
fn apply_thin_first_third(
    ctx: &mut [Entry],
    now: usize,
    ages: &mut Vec<usize>,
) -> (usize, Option<usize>) {
    let end = (ctx.len() / 3).max(1);
    let mut count = 0;
    let mut lowest = None;

    for i in 0..end.min(ctx.len()) {
        let e = &mut ctx[i];
        if e.thinned || !e.is_tool_result || e.chars <= THIN_SIZE_FLOOR_CHARS {
            continue;
        }
        ages.push(now.saturating_sub(e.born_at));
        e.tokens = THINNED_TOKENS;
        e.thinned = true;
        count += 1;
        if lowest.is_none() {
            lowest = Some(i);
        }
    }

    (count, lowest)
}

// ============================================================================
// Builders
// ============================================================================

fn sys(content: &str) -> Message {
    Message::new(MessageRole::System, content.to_string())
}

fn user(content: &str) -> Message {
    Message::new(MessageRole::User, content.to_string())
}

fn assistant_tool_call(name: &str) -> Message {
    let mut m = Message::new(MessageRole::Assistant, "Checking.".to_string());
    m.tool_calls.push(MessageToolCall {
        id: format!("toolu_{}", name),
        name: name.to_string(),
        input: serde_json::json!({ "q": "x".repeat(200) }),
    });
    m
}

fn tool_result(id: &str, body_chars: usize) -> Message {
    let mut m = Message::new(
        MessageRole::User,
        format!("Tool result: {}", "y".repeat(body_chars)),
    );
    m.tool_result_id = Some(id.to_string());
    m
}

/// Build a single-turn transcript with `tools` tool calls.
fn transcript(tools: usize, body_chars: usize, system_chars: usize) -> Vec<Message> {
    let mut v = vec![sys(&"S".repeat(system_chars)), user("do the thing")];
    for i in 0..tools {
        v.push(assistant_tool_call("shell"));
        v.push(tool_result(&format!("t{}", i), body_chars));
    }
    v
}

/// Context size chosen so a transcript reaches a target percentage.
fn window_for(messages: &[Message], target_peak_pct: f64) -> f64 {
    let total: f64 = messages.iter().map(estimate_message_tokens).sum();
    total / (target_peak_pct / 100.0)
}

// ============================================================================
// T1 happy: reproduce the measured collision and the net-negative result
// ============================================================================

#[test]
fn happy_thinning_writes_land_inside_the_cached_prefix() {
    // The core mechanical claim: thinning targets [0, len/3) while the cache
    // breakpoint sits at the end of history, so thinning writes below it.
    let msgs = transcript(60, 4_000, 30_000);
    let window = window_for(&msgs, 75.0);

    let r = simulate(&msgs, ThinPolicy::immediate(50), window);

    assert!(r.thin_events > 0, "thinning must actually fire in this fixture");
    assert!(
        r.thins_hitting_cache > 0,
        "at least one thin must have mutated the cached prefix"
    );
    assert_eq!(
        r.thins_hitting_cache, r.thin_events,
        "every thin here lands inside the cached prefix, because the breakpoint \
         (at history end) is always past the first third"
    );

    // Accounting identity.
    let bucketed = r.cache_read_tokens + r.cache_write_tokens + r.fresh_tokens;
    assert!(
        (bucketed - r.raw_input_tokens).abs() < 1.0,
        "buckets must sum to the raw bill: {:.1} vs {:.1}",
        bucketed,
        r.raw_input_tokens
    );
}

#[test]
fn happy_immediate_thinning_can_be_net_negative_but_aligned_is_not() {
    // Measured on real sessions: immediate thinning is net-negative in 12/19
    // thinning sessions; cache-aligned was negative in ZERO at every floor.
    let msgs = transcript(80, 5_000, 40_000);
    let window = window_for(&msgs, 78.0);

    let never = simulate(&msgs, ThinPolicy::never(), window);
    let immediate = simulate(&msgs, ThinPolicy::immediate(50), window);
    let aligned = simulate(&msgs, ThinPolicy::cache_aligned(50), window);

    assert!(immediate.thin_events > 0 && aligned.thin_events > 0);

    assert!(
        aligned.effective_cost < immediate.effective_cost,
        "cache-aligned must beat immediate: aligned={:.0} immediate={:.0}",
        aligned.effective_cost,
        immediate.effective_cost
    );

    // The mechanism: immediate pays extra cache writes.
    assert!(
        immediate.cache_write_tokens > aligned.cache_write_tokens,
        "immediate thinning forces extra cache re-writes: {:.0} vs {:.0}",
        immediate.cache_write_tokens,
        aligned.cache_write_tokens
    );

    // Aligned must never be worse than not thinning at all.
    assert!(
        aligned.effective_cost <= never.effective_cost,
        "aligned thinning must not cost more than never thinning: {:.0} vs {:.0}",
        aligned.effective_cost,
        never.effective_cost
    );

    println!(
        "never={:.0} immediate={:.0} ({:+.1}%) aligned={:.0} ({:+.1}%)",
        never.effective_cost,
        immediate.effective_cost,
        immediate.savings_vs(&never),
        aligned.effective_cost,
        aligned.savings_vs(&never),
    );
}

#[test]
fn happy_lower_floor_thins_more_and_saves_more_when_aligned() {
    // The floor question, answered arithmetically. Measured aggregate on real
    // sessions: 50%->3.1%, 30%->12.4%, 20%->17.0%.
    let msgs = transcript(80, 5_000, 40_000);
    let window = window_for(&msgs, 60.0);
    let never = simulate(&msgs, ThinPolicy::never(), window);

    let high = simulate(&msgs, ThinPolicy::cache_aligned(50), window);
    let low = simulate(&msgs, ThinPolicy::cache_aligned(20), window);

    assert!(
        low.thin_events >= high.thin_events,
        "a lower floor must fire at least as often ({} vs {})",
        low.thin_events,
        high.thin_events
    );
    assert!(
        low.savings_vs(&never) > high.savings_vs(&never),
        "lower floor should save more when aligned: low={:.1}% high={:.1}%",
        low.savings_vs(&never),
        high.savings_vs(&never)
    );
}

// ============================================================================
// T1 negative
// ============================================================================

#[test]
fn negative_session_below_the_floor_is_an_exact_noop() {
    // A session that never crosses the floor must cost EXACTLY the same as
    // never thinning — not "approximately", not "a small saving".
    let msgs = transcript(40, 800, 5_000);
    let window = window_for(&msgs, 25.0); // peaks well below a 50% floor

    let never = simulate(&msgs, ThinPolicy::never(), window);
    let immediate = simulate(&msgs, ThinPolicy::immediate(50), window);
    let aligned = simulate(&msgs, ThinPolicy::cache_aligned(50), window);

    assert_eq!(immediate.thin_events, 0, "floor never crossed");
    assert_eq!(aligned.thin_events, 0, "floor never crossed");
    assert_eq!(
        immediate.effective_cost, never.effective_cost,
        "no thinning => identical cost"
    );
    assert_eq!(aligned.effective_cost, never.effective_cost);
    assert_eq!(immediate.messages_thinned, 0);
    assert!(immediate.evicted_ages.is_empty());
}

#[test]
fn negative_empty_and_toolless_transcripts_do_not_panic() {
    for policy in [
        ThinPolicy::never(),
        ThinPolicy::immediate(20),
        ThinPolicy::cache_aligned(20),
    ] {
        let empty = simulate(&[], policy, 200_000.0);
        assert_eq!(empty.requests, 0);
        assert_eq!(empty.effective_cost, 0.0);
        assert_eq!(empty.thin_events, 0);
        assert_eq!(empty.peak_tokens, 0.0);

        // Pure chat: no tool results => no priced requests, nothing to thin.
        let chat = vec![sys("s"), user("hi"), Message::new(MessageRole::Assistant, "hello".into())];
        let r = simulate(&chat, policy, 200_000.0);
        assert_eq!(r.requests, 0, "no tool loop => no requests");
        assert_eq!(r.thin_events, 0);
    }
}

#[test]
fn negative_small_tool_results_are_never_thinned() {
    // `collect_thin_modifications()` skips results <= 500 chars. A session made
    // entirely of small results must thin nothing even far above the floor.
    let msgs = transcript(60, 100, 200_000);
    let window = window_for(&msgs, 79.0);

    let r = simulate(&msgs, ThinPolicy::cache_aligned(20), window);
    assert_eq!(
        r.messages_thinned, 0,
        "results below the 500-char floor must be left alone"
    );

    let never = simulate(&msgs, ThinPolicy::never(), window);
    assert_eq!(
        r.effective_cost, never.effective_cost,
        "nothing thinned => identical cost"
    );
}

#[test]
fn negative_zero_total_tokens_does_not_divide_by_zero() {
    // Defensive: ContextWindow::percentage_used() guards total_tokens == 0.
    // The model must not produce NaN/inf and must not thin.
    let msgs = transcript(20, 4_000, 1_000);
    let r = simulate(&msgs, ThinPolicy::cache_aligned(20), 0.0);
    assert!(
        r.effective_cost.is_finite(),
        "cost must stay finite with a zero-sized window, got {}",
        r.effective_cost
    );
}

// ============================================================================
// T1 boundary
// ============================================================================

#[test]
fn boundary_floor_fires_at_exactly_the_floor_and_not_below() {
    // Peaking at exactly the floor must thin; one notch below must not.
    let msgs = transcript(60, 4_000, 20_000);

    let at_floor = window_for(&msgs, 50.0);
    let below_floor = window_for(&msgs, 49.0);

    let hit = simulate(&msgs, ThinPolicy::cache_aligned(50), at_floor);
    let miss = simulate(&msgs, ThinPolicy::cache_aligned(50), below_floor);

    assert!(
        hit.thin_events > 0,
        "peaking at the floor must trigger thinning"
    );
    assert_eq!(
        miss.thin_events, 0,
        "peaking just below the floor must not trigger thinning"
    );
}

#[test]
fn boundary_thinning_stops_at_the_80_percent_ceiling() {
    // `should_thin()` requires threshold <= 80. Above that, compaction owns the
    // problem — this model must not keep thinning forever.
    let msgs = transcript(120, 6_000, 30_000);
    let window = window_for(&msgs, 130.0); // drives well past 80%

    let r = simulate(&msgs, ThinPolicy::cache_aligned(20), window);

    // Thresholds 20,30,40,50,60,70,80 => at most 7 latch events.
    assert!(
        r.thin_events <= 7,
        "at most one thin per 10% threshold up to the 80% ceiling, got {}",
        r.thin_events
    );
}

#[test]
fn boundary_two_thresholds_crossed_before_a_breakpoint_coalesce_into_one_thin() {
    // Under deferral, crossing 50% then 60% before the next breakpoint must
    // apply ONE thin, not two. This is what makes deferral cheaper rather than
    // merely later.
    let msgs = transcript(80, 6_000, 20_000);
    let window = window_for(&msgs, 75.0);

    let immediate = simulate(&msgs, ThinPolicy::immediate(20), window);
    let aligned = simulate(&msgs, ThinPolicy::cache_aligned(20), window);

    assert!(
        aligned.thin_events <= immediate.thin_events,
        "coalescing means aligned fires no more often than immediate ({} vs {})",
        aligned.thin_events,
        immediate.thin_events
    );

    // I expected deferral to change only WHEN, not WHAT. It changes both, and
    // in our favour: because `thin_context()` targets `[0, len/3)` and history
    // keeps growing while a thin is latched, the first-third window is LARGER
    // by the time a deferred thin fires. So deferral sweeps up strictly more
    // content in fewer passes.
    //
    // Measured here: 26 messages thinned deferred vs 22 immediate.
    assert!(
        aligned.messages_thinned >= immediate.messages_thinned,
        "deferral lets the moving first-third window grow, so it should thin at \
         least as much content: aligned={} immediate={}",
        aligned.messages_thinned,
        immediate.messages_thinned
    );

    // This is the crux: MORE content removed in FEWER cache-invalidating
    // passes. That is why deferral is not merely "later", it is strictly better.
    assert!(
        aligned.effective_cost < immediate.effective_cost,
        "more thinned in fewer passes must cost less: aligned={:.0} immediate={:.0}",
        aligned.effective_cost,
        immediate.effective_cost
    );

    println!(
        "immediate: {} events / {} msgs | aligned: {} events / {} msgs",
        immediate.thin_events,
        immediate.messages_thinned,
        aligned.thin_events,
        aligned.messages_thinned,
    );
}

#[test]
fn boundary_moving_window_never_evicts_very_recent_content() {
    // The safety property that makes an aggressive floor defensible: the
    // "first third" is a MOVING window, so the newest two-thirds is always
    // protected. Measured on real sessions, min evicted age was 7-8 tool calls
    // even at floor=10.
    let msgs = transcript(90, 5_000, 20_000);
    let window = window_for(&msgs, 70.0);

    let r = simulate(&msgs, ThinPolicy::cache_aligned(20), window);
    assert!(r.messages_thinned > 0, "fixture must actually thin");

    let min_age = r.min_evicted_age().expect("some content was evicted");
    assert!(
        min_age >= 6,
        "must never evict content younger than the NEVER_PRUNE_LAST_K=6 window; \
         min age was {}",
        min_age
    );
}

#[test]
fn boundary_emergency_path_cannot_defer() {
    // Above 90% the real code thins immediately via ensure_context_capacity()
    // to avoid an API overflow. Deferring there could overflow the window, so
    // the model records that immediate thinning still reduces peak context
    // relative to never thinning — the correctness property T4 must preserve.
    let msgs = transcript(100, 6_000, 30_000);
    let window = window_for(&msgs, 95.0);

    let never = simulate(&msgs, ThinPolicy::never(), window);
    let immediate = simulate(&msgs, ThinPolicy::immediate(50), window);

    assert!(
        immediate.peak_tokens < never.peak_tokens,
        "immediate thinning must relieve peak context: {:.0} vs {:.0}",
        immediate.peak_tokens,
        never.peak_tokens
    );
}

#[test]
fn boundary_deferral_does_not_materially_raise_peak_context() {
    // The cost of deferring is that relief arrives later. The deferral window is
    // bounded by CACHE_BREAKPOINT_EVERY tool calls, so peak context must not
    // blow out. T7 re-checks this against the real implementation.
    let msgs = transcript(80, 5_000, 30_000);
    let window = window_for(&msgs, 75.0);

    let immediate = simulate(&msgs, ThinPolicy::immediate(20), window);
    let aligned = simulate(&msgs, ThinPolicy::cache_aligned(20), window);

    let growth = (aligned.peak_tokens - immediate.peak_tokens) / immediate.peak_tokens * 100.0;
    assert!(
        growth < 15.0,
        "deferral must not materially raise peak context; grew {:.1}%",
        growth
    );
}

#[test]
fn token_estimation_matches_context_window_heuristic() {
    // Guard: if ContextWindow's heuristic changes, this model must follow or
    // every number in the analysis silently becomes wrong.
    let prose = "the quick brown fox jumps over the lazy dog";
    let code = "fn main() { println!(\"hi\"); }";
    assert_eq!(estimate_tokens(prose), ((prose.len() as f64 / 4.0) * 1.1).ceil());
    assert_eq!(estimate_tokens(code), ((code.len() as f64 / 3.0) * 1.1).ceil());
}
