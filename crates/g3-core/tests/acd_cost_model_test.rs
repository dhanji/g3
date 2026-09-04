//! Cache-aware cost model for context-management policies (ACD, pruning).
//!
//! # Why this exists
//!
//! Reasoning about "will `--acd` save me money?" by eyeballing the code is
//! hopeless, because the cost of a *message* is not its size — it is its size
//! multiplied by the number of subsequent provider requests whose prefix still
//! contains it, discounted by whether it sat inside a cached prefix.
//!
//! A single agent turn with N tool calls issues N+1 provider requests, and each
//! one resends the ENTIRE conversation so far. That is quadratic. A 55k-token
//! final prefix with 124 tool calls bills ~3.9M input tokens — a ~70x
//! amplification. Any policy that does not attack the *intra-turn* tool loop is
//! attacking the wrong term.
//!
//! This harness replays a transcript, reconstructs each provider request's
//! prefix, and prices it, so policy comparisons are arithmetic instead of
//! opinion.
//!
//! # Pricing model
//!
//! Anthropic-style prompt caching, expressed as multipliers on the base input
//! price:
//!   * fresh (uncached) input   -> 1.0x
//!   * cache read (prefix hit)  -> 0.1x
//!   * cache write              -> 1.25x
//!
//! We deliberately price cache WRITES too, because the central finding is that
//! a policy which mutates the prefix (as ACD does at every turn boundary, and
//! as naive eager pruning does at every tool call) destroys the cached prefix
//! and forces a re-write. A "savings" that ignores that penalty is a lie.

use g3_providers::{Message, MessageRole, MessageToolCall};

// ============================================================================
// Pricing constants
// ============================================================================

/// Multiplier for fresh (uncached) input tokens.
const RATE_FRESH: f64 = 1.0;
/// Multiplier for tokens read from a cached prefix.
const RATE_CACHE_READ: f64 = 0.1;
/// Multiplier for tokens written into the cache.
const RATE_CACHE_WRITE: f64 = 1.25;

/// g3 slides its rolling cache breakpoint every 10 tool calls.
/// See `Agent::stream_completion_with_tools` in `crates/g3-core/src/lib.rs`.
const CACHE_BREAKPOINT_EVERY: usize = 10;

/// Token cost of a dehydrated-context stub. Measured from
/// `Fragment::generate_stub()` output: first user message + one summary line.
const STUB_TOKENS: f64 = 120.0;

// ============================================================================
// Token estimation (mirrors ContextWindow::estimate_message_tokens)
// ============================================================================

/// Estimate tokens for a string using g3's own heuristic.
///
/// Mirrors `ContextWindow::estimate_tokens`: code/JSON is denser (~3 chars per
/// token) than prose (~4), plus a 10% safety buffer.
fn estimate_tokens(text: &str) -> f64 {
    let base = if text.contains('{') || text.contains("```") || text.contains("fn ") {
        (text.len() as f64) / 3.0
    } else {
        (text.len() as f64) / 4.0
    };
    (base * 1.1).ceil()
}

/// Estimate tokens for a whole message including structured tool_calls.
///
/// Mirrors `ContextWindow::estimate_message_tokens`, including the ~20 token
/// per-tool-call overhead for the name and id.
fn estimate_message_tokens(m: &Message) -> f64 {
    let mut total = estimate_tokens(&m.content);
    for tc in &m.tool_calls {
        let input_str = tc.input.to_string();
        total += ((input_str.len() as f64) / 3.0 * 1.1).ceil() + 20.0;
    }
    total
}

// ============================================================================
// Transcript model
// ============================================================================

/// What role a message plays in the cost breakdown.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    System,
    /// A plain user turn (starts a new turn).
    User,
    /// Assistant prose with no tool call.
    AssistantText,
    /// Assistant message carrying structured tool_calls.
    AssistantToolCall,
    /// A `Tool result: ...` message.
    ToolResult,
    /// A tool result whose body has been evicted to a receipt.
    ToolResultReceipt,
    /// A dehydrated-context stub.
    Stub,
}

/// One entry in the simulated live context.
#[derive(Debug, Clone)]
struct Entry {
    tokens: f64,
    kind: Kind,
    /// Name of the tool, for pin rules and supersession detection.
    tool: Option<String>,
}

/// Classify a message the way the provider layer will see it.
fn classify(m: &Message) -> Kind {
    if m.tool_result_id.is_some() || m.content.starts_with("Tool result") {
        return Kind::ToolResult;
    }
    match m.role {
        MessageRole::System => Kind::System,
        MessageRole::Assistant => {
            if m.tool_calls.is_empty() {
                Kind::AssistantText
            } else {
                Kind::AssistantToolCall
            }
        }
        MessageRole::User => Kind::User,
    }
}

// ============================================================================
// Policies
// ============================================================================

/// Context-management policy under test.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Policy {
    /// No context management. Every request resends everything.
    Baseline,
    /// Current `--acd`: collapse prior turns into a stub at each USER TURN
    /// boundary only. Never touches the intra-turn tool loop.
    TurnAlignedAcd,
    /// Evaluate + apply eviction after EVERY tool call. Maximally aggressive,
    /// and maximally cache-hostile: each prune invalidates the cached prefix.
    EagerPrune,
    /// Evaluate after every tool call but APPLY only when the prune boundary
    /// coincides with the rolling cache-breakpoint slide. This is the design
    /// this analysis recommends.
    CacheAlignedPrune,
    /// Both mechanisms together. They attack disjoint regions of the bill —
    /// ACD reclaims prior turns, pruning reclaims the current turn's tool loop
    /// — so their savings compose.
    AcdPlusCacheAlignedPrune,
}

/// Tools whose results are load-bearing state, never evictable.
const PINNED_TOOLS: &[&str] = &[
    "plan_read",
    "plan_write",
    "plan_approve",
    "todo_read",
    "todo_write",
    "remember",
    "write_envelope",
];

/// Never evict the most recent K tool results — the model is probably still
/// reasoning about them.
const NEVER_PRUNE_LAST_K: usize = 6;
/// Do not bother evicting anything smaller than this; the receipt would cost
/// nearly as much as the body.
const SIZE_FLOOR_TOKENS: f64 = 150.0;
/// Cost of a receipt that replaces an evicted body.
const RECEIPT_TOKENS: f64 = 40.0;

fn is_pinned(tool: &Option<String>) -> bool {
    tool.as_deref()
        .map(|t| PINNED_TOOLS.contains(&t))
        .unwrap_or(false)
}

// ============================================================================
// The simulator
// ============================================================================

/// Result of replaying a transcript under one policy.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct CostReport {
    /// Number of provider requests issued.
    pub requests: usize,
    /// Sum of every request's prefix, ignoring caching. The "raw" bill.
    pub raw_input_tokens: f64,
    /// Effective cost in base-price-equivalent tokens, after cache discounts
    /// and cache-write penalties.
    pub effective_cost: f64,
    /// Tokens billed at the cache-read rate.
    pub cache_read_tokens: f64,
    /// Tokens billed at the cache-write rate.
    pub cache_write_tokens: f64,
    /// Tokens billed fresh.
    pub fresh_tokens: f64,
    /// Size of the final live prefix.
    pub final_prefix_tokens: f64,
    /// How many tool results were evicted to receipts.
    pub evictions: usize,
}

impl CostReport {
    /// Prefix amplification: how many times over the final prefix was billed.
    pub fn amplification(&self) -> f64 {
        if self.final_prefix_tokens <= 0.0 {
            0.0
        } else {
            self.raw_input_tokens / self.final_prefix_tokens
        }
    }

    /// Percent saved relative to a baseline report.
    pub fn savings_vs(&self, baseline: &CostReport) -> f64 {
        if baseline.effective_cost <= 0.0 {
            return 0.0;
        }
        100.0 - (self.effective_cost * 100.0 / baseline.effective_cost)
    }
}

/// Replay `messages` under `policy` and price every provider request.
///
/// A provider request is modelled as occurring immediately after each tool
/// result is appended — that is exactly what g3's streaming tool loop does:
/// execute tool, append result, send the whole history again.
pub fn simulate(messages: &[Message], policy: Policy) -> CostReport {
    let mut ctx: Vec<Entry> = Vec::new();
    let mut report = CostReport::default();

    let does_acd = matches!(
        policy,
        Policy::TurnAlignedAcd | Policy::AcdPlusCacheAlignedPrune
    );

    // Index into `ctx` up to which the prefix is currently cached.
    let mut cached_upto: usize = 0;
    // Set when the prefix was mutated below `cached_upto`, meaning the next
    // request must re-write the cache rather than read it.
    let mut cache_dirty = false;

    let mut total_tool_calls: usize = 0;
    // Track the last tool name seen on an assistant message, so the following
    // tool result can be attributed to it.
    let mut pending_tool: Option<String> = None;

    for m in messages {
        let kind = classify(m);

        // --- Turn boundary: this is the ONLY place ACD ever acts. ---
        //
        // `Agent::dehydrate_context()` bails out when there is nothing new to
        // dehydrate. Modelling that faithfully matters: without it the very
        // first user message "dehydrates" a context holding only the system
        // prompt, which ADDS a stub and makes ACD look like it costs money in
        // single-turn sessions. The real code does not do that.
        let has_dehydratable = ctx.iter().any(|e| e.kind != Kind::System && e.kind != Kind::Stub);
        if kind == Kind::User && does_acd && has_dehydratable {
            let system: Vec<Entry> = ctx.iter().filter(|e| e.kind == Kind::System).cloned().collect();
            let last_summary: Option<Entry> = ctx
                .iter()
                .rev()
                .find(|e| e.kind == Kind::AssistantText)
                .cloned();

            let n_system = system.len();
            ctx = system;
            ctx.push(Entry {
                tokens: STUB_TOKENS,
                kind: Kind::Stub,
                tool: None,
            });
            if let Some(s) = last_summary {
                ctx.push(s);
            }
            // Dehydration truncates the prefix. Anything cached beyond the
            // system block is gone, and the next request pays a cache WRITE.
            if cached_upto > n_system {
                cache_dirty = true;
            }
            cached_upto = cached_upto.min(n_system);
        }

        if kind == Kind::AssistantToolCall {
            pending_tool = m.tool_calls.first().map(|tc| tc.name.clone());
        }

        let tool_for_entry = if kind == Kind::ToolResult {
            pending_tool.clone()
        } else {
            None
        };

        ctx.push(Entry {
            tokens: estimate_message_tokens(m),
            kind,
            tool: tool_for_entry,
        });

        // A tool result closes an iteration => a provider request follows.
        if kind != Kind::ToolResult {
            continue;
        }
        total_tool_calls += 1;

        // --- Eviction: evaluated per tool call. ---
        let at_cache_boundary = total_tool_calls % CACHE_BREAKPOINT_EVERY == 0;
        let should_apply = match policy {
            Policy::EagerPrune => true,
            Policy::CacheAlignedPrune | Policy::AcdPlusCacheAlignedPrune => at_cache_boundary,
            _ => false,
        };

        if should_apply {
            let (evicted, first_changed) = evict_stale_tool_results(&mut ctx);
            report.evictions += evicted;
            if let Some(idx) = first_changed {
                // Mutating the prefix at `idx` invalidates every cached token
                // at or after it.
                if idx < cached_upto {
                    cache_dirty = true;
                    cached_upto = idx;
                }
            }
        }

        // --- Slide the rolling cache breakpoint. ---
        if at_cache_boundary {
            cached_upto = ctx.len();
            // Establishing a new breakpoint is itself a cache write, but under
            // CacheAlignedPrune the prune happened *just before* the slide, so
            // the write we pay for is the one we were going to pay anyway.
            cache_dirty = true;
        }

        // --- Price this request. ---
        let cached: f64 = ctx[..cached_upto.min(ctx.len())].iter().map(|e| e.tokens).sum();
        let fresh: f64 = ctx[cached_upto.min(ctx.len())..].iter().map(|e| e.tokens).sum();

        if cache_dirty {
            report.cache_write_tokens += cached;
            report.effective_cost += cached * RATE_CACHE_WRITE;
            cache_dirty = false;
        } else {
            report.cache_read_tokens += cached;
            report.effective_cost += cached * RATE_CACHE_READ;
        }
        report.fresh_tokens += fresh;
        report.effective_cost += fresh * RATE_FRESH;

        report.raw_input_tokens += cached + fresh;
        report.requests += 1;
    }

    report.final_prefix_tokens = ctx.iter().map(|e| e.tokens).sum();
    report
}

/// Evict stale tool-result bodies to receipts.
///
/// Returns `(number_evicted, index_of_first_mutation)`. The second value is
/// what tells the caller how much cached prefix was destroyed.
fn evict_stale_tool_results(ctx: &mut [Entry]) -> (usize, Option<usize>) {
    let tool_result_positions: Vec<usize> = ctx
        .iter()
        .enumerate()
        .filter(|(_, e)| e.kind == Kind::ToolResult)
        .map(|(i, _)| i)
        .collect();

    if tool_result_positions.len() <= NEVER_PRUNE_LAST_K {
        return (0, None);
    }

    let stale = &tool_result_positions[..tool_result_positions.len() - NEVER_PRUNE_LAST_K];
    let mut evicted = 0;
    let mut first_changed = None;

    for &idx in stale {
        let e = &mut ctx[idx];
        if e.tokens <= SIZE_FLOOR_TOKENS || is_pinned(&e.tool) {
            continue;
        }
        e.tokens = RECEIPT_TOKENS;
        e.kind = Kind::ToolResultReceipt;
        evicted += 1;
        if first_changed.is_none() {
            first_changed = Some(idx);
        }
    }

    (evicted, first_changed)
}

// ============================================================================
// Transcript builders for tests
// ============================================================================

fn sys(content: &str) -> Message {
    Message::new(MessageRole::System, content.to_string())
}

fn user(content: &str) -> Message {
    Message::new(MessageRole::User, content.to_string())
}

fn assistant_with_tool(name: &str, input_size: usize) -> Message {
    let mut m = Message::new(MessageRole::Assistant, "Let me check.".to_string());
    m.tool_calls.push(MessageToolCall {
        id: format!("t_{}", name),
        name: name.to_string(),
        input: serde_json::json!({ "payload": "x".repeat(input_size) }),
    });
    m
}

fn tool_result(id: &str, body_size: usize) -> Message {
    let mut m = Message::new(
        MessageRole::User,
        format!("Tool result: {}", "y".repeat(body_size)),
    );
    m.tool_result_id = Some(id.to_string());
    m
}

/// Build a transcript: one system prompt, then `turns` user turns, each with
/// `tools_per_turn` tool calls.
fn build_transcript(turns: usize, tools_per_turn: usize, body_size: usize) -> Vec<Message> {
    let mut msgs = vec![sys(&"S".repeat(30_000))];
    for t in 0..turns {
        msgs.push(user(&format!("task number {}", t)));
        for i in 0..tools_per_turn {
            msgs.push(assistant_with_tool("shell", 200));
            msgs.push(tool_result(&format!("t_{}_{}", t, i), body_size));
        }
        msgs.push(Message::new(
            MessageRole::Assistant,
            format!("Done with task {}.", t),
        ));
    }
    msgs
}

// ============================================================================
// I1 happy: amplification and cache split are reproduced
// ============================================================================

#[test]
fn happy_replay_reproduces_prefix_amplification_and_cache_split() {
    let msgs = build_transcript(1, 20, 4_000);
    let r = simulate(&msgs, Policy::Baseline);

    assert_eq!(r.requests, 20, "one provider request per tool call");

    // The whole point: the bill is a large multiple of the final prefix.
    assert!(
        r.amplification() > 5.0,
        "20 tool calls must bill several times the final prefix, got {:.1}x \
         (raw={:.0}, final={:.0})",
        r.amplification(),
        r.raw_input_tokens,
        r.final_prefix_tokens
    );

    // Caching must actually engage: breakpoints slide every 10 tool calls.
    assert!(
        r.cache_read_tokens > 0.0,
        "rolling cache breakpoints should produce cache reads"
    );
    assert!(
        r.cache_write_tokens > 0.0,
        "sliding a breakpoint is a cache write"
    );

    // Effective cost must be strictly below the raw bill — that is the whole
    // value of caching — but not absurdly so.
    assert!(
        r.effective_cost < r.raw_input_tokens,
        "cache discount must reduce effective cost below raw: {:.0} vs {:.0}",
        r.effective_cost,
        r.raw_input_tokens
    );
    assert!(
        r.effective_cost > r.raw_input_tokens * 0.05,
        "cache discount cannot exceed the 0.1x read rate floor"
    );

    // Accounting identity: every billed token is in exactly one bucket.
    let bucketed = r.cache_read_tokens + r.cache_write_tokens + r.fresh_tokens;
    assert!(
        (bucketed - r.raw_input_tokens).abs() < 1.0,
        "token buckets must sum to the raw bill: {:.1} vs {:.1}",
        bucketed,
        r.raw_input_tokens
    );
}

// ============================================================================
// I1 negative: degenerate transcripts
// ============================================================================

#[test]
fn negative_empty_transcript_costs_nothing_and_does_not_panic() {
    for policy in [
        Policy::Baseline,
        Policy::TurnAlignedAcd,
        Policy::EagerPrune,
        Policy::CacheAlignedPrune,
    ] {
        let r = simulate(&[], policy);
        assert_eq!(r.requests, 0, "{:?}: no messages => no requests", policy);
        assert_eq!(r.raw_input_tokens, 0.0);
        assert_eq!(r.effective_cost, 0.0);
        assert_eq!(r.final_prefix_tokens, 0.0);
        assert_eq!(r.amplification(), 0.0, "amplification must not divide by zero");
        assert_eq!(r.evictions, 0);
    }
}

#[test]
fn negative_transcript_with_no_tool_results_issues_no_priced_requests() {
    // Pure chat: system + user + assistant prose. No tool loop at all.
    let msgs = vec![
        sys("you are helpful"),
        user("hello"),
        Message::new(MessageRole::Assistant, "hi there".to_string()),
    ];

    for policy in [Policy::Baseline, Policy::TurnAlignedAcd, Policy::CacheAlignedPrune] {
        let r = simulate(&msgs, policy);
        assert_eq!(
            r.requests, 0,
            "{:?}: the model prices tool-loop iterations; pure chat has none",
            policy
        );
        assert_eq!(r.evictions, 0);
        assert!(
            r.final_prefix_tokens > 0.0,
            "the context still exists even though nothing was billed"
        );
    }
}

#[test]
fn negative_pinned_tool_results_are_never_evicted() {
    // A long run of plan_write results, all large. None may be evicted.
    let mut msgs = vec![sys("s")];
    msgs.push(user("go"));
    for i in 0..20 {
        msgs.push(assistant_with_tool("plan_write", 100));
        msgs.push(tool_result(&format!("p{}", i), 4_000));
    }

    let r = simulate(&msgs, Policy::CacheAlignedPrune);
    assert_eq!(
        r.evictions, 0,
        "plan_write results are load-bearing state and must never be evicted"
    );

    // And therefore pruning cannot beat the baseline here.
    let base = simulate(&msgs, Policy::Baseline);
    assert!(
        (r.effective_cost - base.effective_cost).abs() < 1.0,
        "with everything pinned, pruning must be a no-op: {:.0} vs {:.0}",
        r.effective_cost,
        base.effective_cost
    );
}

// ============================================================================
// I1 boundary: the central structural finding
// ============================================================================

#[test]
fn boundary_single_turn_means_turn_aligned_acd_saves_nothing() {
    // THE headline result. One user turn, 40 tool calls — a completely normal
    // g3 session. ACD only acts at user-turn boundaries, so with a single turn
    // it never fires and cannot save a single token.
    let msgs = build_transcript(1, 40, 3_000);

    let base = simulate(&msgs, Policy::Baseline);
    let acd = simulate(&msgs, Policy::TurnAlignedAcd);

    assert_eq!(
        acd.effective_cost, base.effective_cost,
        "turn-aligned ACD cannot save anything inside a single turn"
    );
    assert!(
        acd.savings_vs(&base).abs() < 0.001,
        "expected exactly 0% savings, got {:.3}%",
        acd.savings_vs(&base)
    );

    // Not "roughly nothing" — *exactly* nothing. There is no second user turn,
    // so `dehydrate_context()` never fires, and the transcript the provider
    // sees is byte-for-byte the baseline transcript.
    assert_eq!(
        acd.raw_input_tokens, base.raw_input_tokens,
        "ACD must not alter the prefix at all when there is only one turn"
    );
    assert_eq!(acd.evictions, 0, "ACD evicts nothing; it only stubs turns");

    // Per-tool-call pruning, by contrast, works precisely where ACD cannot.
    let pruned = simulate(&msgs, Policy::CacheAlignedPrune);
    assert!(
        pruned.evictions > 0,
        "per-tool-call pruning must act inside a single turn"
    );
    assert!(
        pruned.savings_vs(&base) > 5.0,
        "per-tool-call pruning should save materially where ACD saves nothing, got {:.1}%",
        pruned.savings_vs(&base)
    );
}

#[test]
fn boundary_system_prompt_share_is_the_savings_floor() {
    // A transcript dominated by an enormous, unprunable system prompt.
    // No policy can save more than the non-system share. This is the Amdahl
    // bound on the whole exercise.
    let mut msgs = vec![sys(&"S".repeat(400_000))];
    msgs.push(user("go"));
    for i in 0..20 {
        msgs.push(assistant_with_tool("shell", 200));
        // Bodies must clear SIZE_FLOOR_TOKENS or the test would pass
        // vacuously by evicting nothing.
        msgs.push(tool_result(&format!("t{}", i), 4_000));
    }

    let base = simulate(&msgs, Policy::Baseline);
    let pruned = simulate(&msgs, Policy::CacheAlignedPrune);

    assert!(
        pruned.evictions > 0,
        "guard against a vacuous pass: eviction must actually happen"
    );

    let savings = pruned.savings_vs(&base);
    assert!(
        savings < 25.0,
        "when the system prompt dominates, savings must be small; got {:.1}%",
        savings
    );
    // Note: savings can be slightly NEGATIVE here. Evicting a body invalidates
    // the cached prefix, and when the prefix is a 400k-token system prompt the
    // forced 1.25x re-write costs more than the evicted body ever saved. This
    // is the cache-write penalty in its purest form, and it is why the eviction
    // policy must be co-scheduled with the cache breakpoint rather than run
    // whenever it "could" free bytes.
    assert!(
        savings > -5.0,
        "the cache-write penalty must stay bounded, got {:.1}%",
        savings
    );
}

#[test]
fn boundary_fewer_results_than_never_prune_k_is_a_noop() {
    // NEVER_PRUNE_LAST_K = 6, so 5 tool results must be untouched.
    let msgs = build_transcript(1, 5, 8_000);
    let r = simulate(&msgs, Policy::EagerPrune);
    assert_eq!(
        r.evictions, 0,
        "with only 5 results and K={}, nothing is stale",
        NEVER_PRUNE_LAST_K
    );

    let base = simulate(&msgs, Policy::Baseline);
    assert_eq!(
        r.effective_cost, base.effective_cost,
        "a no-op prune must cost exactly the baseline"
    );
}

// ============================================================================
// The cache-alignment result that motivated the design change
// ============================================================================

#[test]
fn cache_aligned_pruning_beats_eager_pruning() {
    // Eager pruning evicts more, yet costs MORE, because every eviction
    // invalidates the cached prefix and forces a 1.25x re-write. This is the
    // reason the design applies evictions only on cache-breakpoint boundaries.
    let msgs = build_transcript(1, 60, 3_000);

    let base = simulate(&msgs, Policy::Baseline);
    let eager = simulate(&msgs, Policy::EagerPrune);
    let aligned = simulate(&msgs, Policy::CacheAlignedPrune);

    assert!(
        eager.evictions >= aligned.evictions,
        "eager prunes at least as often ({} vs {})",
        eager.evictions,
        aligned.evictions
    );

    assert!(
        aligned.effective_cost < eager.effective_cost,
        "cache-aligned pruning must be cheaper than eager pruning despite \
         evicting no more: aligned={:.0} eager={:.0}",
        aligned.effective_cost,
        eager.effective_cost
    );

    assert!(
        eager.cache_write_tokens > aligned.cache_write_tokens,
        "eager pruning's penalty is cache re-writes: eager={:.0} aligned={:.0}",
        eager.cache_write_tokens,
        aligned.cache_write_tokens
    );

    println!(
        "baseline={:.0}  eager={:.0} ({:.1}%)  aligned={:.0} ({:.1}%)",
        base.effective_cost,
        eager.effective_cost,
        eager.savings_vs(&base),
        aligned.effective_cost,
        aligned.savings_vs(&base),
    );
}

#[test]
fn multi_turn_acd_beats_pruning_but_combining_beats_both() {
    // A correction to an intuition that turned out to be WRONG.
    //
    // I expected per-tool-call pruning to dominate ACD, on the reasoning that
    // most spend is intra-turn. It does not. ACD deletes prior turns
    // *wholesale* — every tool call, every result, every assistant message —
    // whereas pruning only shrinks tool-result BODIES and must leave the
    // tool_use/tool_result skeleton intact. When a session has real turn
    // boundaries, ACD reclaims strictly more.
    //
    // The two are complementary, not competing: ACD owns prior turns, pruning
    // owns the current one. Measured on 94 real sessions: ACD 26.4%,
    // pruning 20.8%, both together 35.4%.
    let msgs = build_transcript(6, 15, 3_000);

    let base = simulate(&msgs, Policy::Baseline);
    let acd = simulate(&msgs, Policy::TurnAlignedAcd);
    let aligned = simulate(&msgs, Policy::CacheAlignedPrune);
    let combo = simulate(&msgs, Policy::AcdPlusCacheAlignedPrune);

    assert!(
        acd.savings_vs(&base) > 0.0,
        "ACD should help when there are many turn boundaries, got {:.1}%",
        acd.savings_vs(&base)
    );
    assert!(
        acd.savings_vs(&base) > aligned.savings_vs(&base),
        "with many turn boundaries ACD ({:.1}%) out-saves pruning ({:.1}%), \
         because it deletes whole turns rather than shrinking bodies",
        acd.savings_vs(&base),
        aligned.savings_vs(&base)
    );

    // The actionable result: do BOTH. Neither alone is the answer.
    assert!(
        combo.savings_vs(&base) > acd.savings_vs(&base),
        "combining must beat ACD alone: combo={:.1}% acd={:.1}%",
        combo.savings_vs(&base),
        acd.savings_vs(&base)
    );
    assert!(
        combo.savings_vs(&base) > aligned.savings_vs(&base),
        "combining must beat pruning alone: combo={:.1}% pruning={:.1}%",
        combo.savings_vs(&base),
        aligned.savings_vs(&base)
    );

    println!(
        "base={:.0}  acd={:.1}%  pruning={:.1}%  combined={:.1}%",
        base.effective_cost,
        acd.savings_vs(&base),
        aligned.savings_vs(&base),
        combo.savings_vs(&base),
    );
}

#[test]
fn eager_pruning_can_cost_more_than_doing_nothing() {
    // The single most counterintuitive measured result, pinned as a test so the
    // implementation cannot quietly regress into it.
    //
    // "Prune a tool result as soon as it goes stale" sounds strictly good: the
    // context gets smaller, so the bill should shrink. It does not. Every
    // eviction rewrites a byte inside the cached prefix, which invalidates the
    // cache from that offset and forces the next request to pay 1.25x to
    // re-establish it. The re-write costs more than the eviction saved.
    //
    // On real transcripts, eager pruning was WORSE than doing nothing in
    // 61 of 94 sessions (median -3.9%).
    let msgs = build_transcript(1, 80, 5_000);

    let base = simulate(&msgs, Policy::Baseline);
    let eager = simulate(&msgs, Policy::EagerPrune);
    let aligned = simulate(&msgs, Policy::CacheAlignedPrune);

    assert!(
        eager.evictions > 0 && aligned.evictions > 0,
        "both policies must actually evict for this comparison to mean anything"
    );

    // Both policies evict the SAME set of results — eviction is idempotent, a
    // body can only be replaced by a receipt once. They differ purely in WHEN.
    // Eager evicts the moment a result goes stale; cache-aligned waits for the
    // breakpoint slide. So any cost difference is attributable entirely to
    // cache behaviour, not to how much content was removed.
    assert_eq!(
        eager.evictions, aligned.evictions,
        "same evictions, different timing — that is the whole experiment"
    );
    assert!(
        eager.raw_input_tokens < aligned.raw_input_tokens,
        "eager evicts EARLIER so it moves strictly fewer raw tokens \
         ({:.0} vs {:.0}) — and still ends up costing more",
        eager.raw_input_tokens,
        aligned.raw_input_tokens
    );

    // Fewer raw tokens, yet more cache re-writes...
    assert!(
        eager.cache_write_tokens > aligned.cache_write_tokens,
        "eager pays more cache re-writes: {:.0} vs {:.0}",
        eager.cache_write_tokens,
        aligned.cache_write_tokens
    );

    // ...and correspondingly fewer cheap cache reads.
    assert!(
        eager.cache_read_tokens < aligned.cache_read_tokens,
        "eager destroys the cache-read opportunity: {:.0} vs {:.0}",
        eager.cache_read_tokens,
        aligned.cache_read_tokens
    );

    assert!(
        aligned.savings_vs(&base) > eager.savings_vs(&base),
        "cache-aligned must beat eager: aligned={:.1}% eager={:.1}%",
        aligned.savings_vs(&base),
        eager.savings_vs(&base)
    );

    println!(
        "raw eager={:.0} aligned={:.0} | savings eager={:.1}% aligned={:.1}%",
        eager.raw_input_tokens,
        aligned.raw_input_tokens,
        eager.savings_vs(&base),
        aligned.savings_vs(&base),
    );
}

#[test]
fn token_estimation_matches_context_window_heuristic() {
    // Guard: if ContextWindow's heuristic changes, this model must follow, or
    // every number in the analysis silently becomes wrong.
    let prose = "the quick brown fox jumps over the lazy dog";
    let code = "fn main() { println!(\"hi\"); }";

    assert_eq!(
        estimate_tokens(prose),
        ((prose.len() as f64 / 4.0) * 1.1).ceil(),
        "prose uses the 4-chars-per-token path"
    );
    assert_eq!(
        estimate_tokens(code),
        ((code.len() as f64 / 3.0) * 1.1).ceil(),
        "code/JSON uses the denser 3-chars-per-token path"
    );

    let m = assistant_with_tool("shell", 300);
    let expected_overhead = 20.0;
    assert!(
        estimate_message_tokens(&m) > estimate_tokens(&m.content) + expected_overhead,
        "tool_call input must be counted, not just the message text"
    );
}
