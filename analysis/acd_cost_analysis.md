# ACD (`--acd`) — cost and fidelity analysis

**Question asked:** Does `--acd` degrade performance? Will it save a lot in token
cost as hoped? How could we improve or rebuild it to lower costs by pruning tool
results that are no longer salient once extraction is done?

**Short answers:**

1. **Yes, it degrades fidelity** — in five concrete, test-confirmed ways. Two are
   serious enough that the feature misinforms the model in its default
   configuration and corrupts its own fragment chain on `--resume`. **These are now
   fixed** ([§6 Stage 1](#stage-1--fix-the-defects--shipped-no-cost-change-pure-correctness)).
2. **It saves real money, but roughly half of what you'd hope, and not where you'd
   expect.** Aggregate **26.4%** of effective input cost across 94 real sessions.
   Its structural ceiling is ~38% of spend, because it only ever fires at
   user-turn boundaries.
3. **Yes, salience pruning helps — but only if it is cache-aligned.** The obvious
   implementation ("evict a tool result as soon as it goes stale") is **worse than
   doing nothing** in 61 of 94 sessions. Combined with ACD, a cache-aligned design
   reaches **35.4%** aggregate savings.

**Status:** the correctness fixes are shipped and tested. The pruning design is
specified and simulation-validated but deliberately **not** implemented — it needs
an accuracy A/B first, for reasons in [§7.2](#72-the-risk-this-analysis-does-not-quantify).

Everything below is measured, not estimated. Method and reproduction in
[§7](#7-method-and-how-to-reproduce).

---

## 1. Why the cost model has to be cache-aware

The cost of a message is not its size. It is its size **multiplied by the number of
subsequent provider requests whose prefix still contains it**, discounted by
whether it sat inside a cached prefix.

A single agent turn with N tool calls issues N+1 provider requests, and each one
resends the entire conversation so far. That is quadratic in the tool count.

Measured across 94 real sessions:

| Prefix amplification (baseline) | |
|---|---|
| median | **37.3×** |
| max | 139.4× |
| min | 11.1× |

A session whose final context is 55k tokens can bill 3.9M input tokens. Any
analysis that reasons about "context size" rather than "billed prefix-tokens" is
measuring the wrong quantity.

Pricing used throughout (Anthropic-style multipliers on base input price):

| | multiplier |
|---|---|
| fresh input | 1.0× |
| cache read | 0.1× |
| cache write | **1.25×** |

The 1.25× cache-write penalty is the single most important term in this document.
It is why the intuitive pruning design loses money.

---

## 2. Where the money actually goes

Billed input tokens by category, baseline policy, aggregated over 94 sessions:

| category | share of billed input |
|---|---|
| tool results | **39.0%** |
| assistant messages + tool_call inputs | **35.0%** |
| system prompt | 23.9% |
| assistant prose | 1.4% |
| user messages | 0.7% |

Tool results and tool-call inputs together are **74%** of spend. That is the target.
Assistant prose and user messages are noise — optimising them is pointless.

---

## 3. What turn-aligned ACD can even reach

`Agent::dehydrate_context()` (`crates/g3-core/src/lib.rs:1975`) is called only from
`finalize_streaming_turn()`. It fires **once per user turn**, never inside the tool
loop.

Partitioning every billing event by whether ACD could possibly have affected it:

| region | share of billed tokens | reachable by ACD? |
|---|---|---|
| spend incurred after a later turn began | **38.2%** | yes |
| spend inside the message's own turn | **38.0%** | no — ACD fires only at turn boundaries |
| system prompt | 23.9% | no — never dehydrated |

**ACD's theoretical ceiling is 38.2% of spend.** It achieves 26.4%, so it is
actually fairly efficient *within its reach* — the problem is that its reach is
structurally limited to under 40% of the bill.

The 38.0% burned inside the current turn's tool loop is reachable **only** by
per-tool-call pruning. The 23.9% system prompt is reachable by neither, and sets
an Amdahl floor on the whole exercise.

---

## 4. Measured savings

Effective input cost, base-price-equivalent tokens, 94 sessions:

| policy | median | mean | aggregate | worst | best |
|---|---|---|---|---|---|
| turn-aligned ACD (current `--acd`) | 17.4% | 18.0% | **26.4%** | **−4.9%** | 48.9% |
| eager per-tool-call prune | −3.9% | −2.1% | **−3.7%** | −29.1% | 41.4% |
| cache-aligned prune | 17.6% | 18.5% | 20.8% | 0.2% | 41.6% |
| **ACD + cache-aligned prune** | **26.7%** | **27.1%** | **35.4%** | 1.5% | **52.2%** |

Top sessions by absolute cost:

| session | reqs | baseline | ACD% | eager% | aligned% | ACD+aligned% |
|---|---|---|---|---|---|---|
| create_a_plan_create_a_d48a545c | 159 | 4,954,266 | 40.5% | −11.6% | 23.8% | **44.2%** |
| id_like_to_add_model_c5d20acf | 166 | 3,574,196 | 47.1% | 8.8% | 33.4% | **51.7%** |
| create_a_plan_look_at_d6e41bce | 255 | 3,553,946 | 39.7% | −18.1% | 15.6% | **47.4%** |
| create_a_plan_in_git_ae95ff8e | 188 | 3,486,363 | 45.6% | −12.1% | 18.7% | **51.9%** |
| create_a_plan_during_write_env | 171 | 3,429,366 | 42.3% | −20.2% | 12.8% | **50.4%** |
| research_the_agent_skills_spec | 179 | 3,148,382 | 13.3% | −10.6% | 18.3% | **31.0%** |
| goal_add_an_interactive_plan | 154 | 3,137,921 | 45.5% | −15.6% | 18.2% | **52.2%** |
| create_a_plan_add_an_b84af651 | 149 | 2,414,811 | 11.1% | −20.5% | 8.9% | **19.6%** |

### 4.1 Where ACD saves nothing — or costs money

This is the case not to gloss over. ACD acts at turn boundaries, so a session with
one long turn gets **nothing**, and pays a small penalty for the stub it inserts:

| session shape | n | median ACD savings |
|---|---|---|
| single-turn | 5 | **−0.4%** |
| multi-turn | 89 | 18.6% |

**24 of 94 sessions saw zero or negative savings from ACD.** Worst observed: −4.9%.

The penalty mechanism: dehydrating truncates the prefix, which invalidates every
cached block beyond the system prompt. The next request pays a 1.25× cache write
to rebuild what it had been reading at 0.1×. When a turn boundary arrives with
little accumulated history to discard, that re-write costs more than the
dehydration saved.

This matters because a long autonomous run is *exactly* the single-turn shape —
the case where context pressure is worst is the case ACD helps least.

### 4.2 The counterintuitive result: eager pruning loses money

"Evict a tool result as soon as it is no longer salient" sounds strictly good. It
is not. Measured:

| policy | evictions | tokens re-written at 1.25× |
|---|---|---|
| baseline | 0 | 37,401,926 |
| eager prune | 2,747 | **45,589,429** |
| cache-aligned prune | 2,578 | **25,693,470** |

Eager pruning evicts *slightly more* and moves *strictly fewer* raw tokens, yet
costs **more than doing nothing in 61 of 94 sessions** (median −3.9%).

The reason: every eviction rewrites a byte inside the cached prefix, invalidating
the cache from that offset. The forced re-write at 1.25× exceeds what the eviction
saved at 0.1×. Cache-aligned pruning evicts the *same set* of results — eviction is
idempotent — but defers application to the moment the rolling cache breakpoint
slides, when the prefix was going to be re-written anyway. **Same content removed,
92/94 sessions cheaper.**

Pinned as a regression test:
`crates/g3-core/tests/acd_cost_model_test.rs::eager_pruning_can_cost_more_than_doing_nothing`.

### 4.3 Two hypotheses I had wrong

Stated plainly, because both were in my initial read of the feature:

1. **"Pruning will dominate ACD, since most spend is intra-turn."** False. ACD
   deletes prior turns *wholesale* — every tool call, result and assistant message
   — whereas pruning only shrinks result *bodies* and must leave the
   `tool_use`/`tool_result` skeleton intact. ACD 26.4% vs pruning 20.8%.
2. **"Pruning aggressively is strictly better than pruning conservatively."**
   False, and inverted: aggressive pruning is net-negative. Timing dominates volume.

They are complementary, not competing: ACD owns prior turns, pruning owns the
current one. Together, 35.4%.

---

## 5. Fidelity defects

Confirmed by executable characterization tests in
`crates/g3-core/tests/acd_fidelity_characterization_test.rs` (10/10 passing). These
pin down *actual* behaviour, not desired behaviour.

| # | Defect | Consequence |
|---|---|---|
| **1** | `extract_tool_call_summary()` (`acd.rs:196`) scans `msg.content` for inline JSON, never reads `msg.tool_calls` | With any native-tool-calling provider — **the default** — every stub claims **"no tool calls"**. That is exactly the metadata the model uses to decide whether rehydrating is worthwhile. The stub actively misinforms it. |
| **2** | `Message.kind` is `#[serde(skip)]` (`g3-providers/src/lib.rs:141`) | On `--resume`, stubs reload as `Regular`. `rposition(is_dehydrated_stub)` returns `None`, `dehydrate_start` falls back to 0, and **already-dehydrated content is re-dehydrated into nested stubs.** The chain silently degrades. |
| 3 | The stub replacing a span is plain prose | The live context loses every structured tool interaction from the dehydrated span. (The on-disk fragment does retain them, so rehydration can recover.) |
| 4 | `estimate_fragment_tokens()` uses flat `len/4`; `ContextWindow` uses `len/3` for JSON/code | `execute_rehydrate()`'s capacity check undercounts by >20% on JSON payloads and will green-light a rehydration that overflows the window. |
| 5 | `dehydrate_start = last_stub_index + 2` assumes a Summary always follows the stub, but it is appended only when non-empty | On an empty final response the index overshoots and **nothing is dehydrated** — a silent no-op, not a crash. The context grows unchecked; the feature's entire purpose fails quietly. |

Defects 1 and 2 are the serious ones. Note they interact: because the stub reports
"no tool calls" (1), a user cannot easily tell that resume is re-dehydrating
garbage (2).

A sixth, milder issue: `execute_rehydrate()` truncates each restored message at
2000 chars, so rehydration cannot reconstruct an exact prefix. It is a summary
view, not a restore. That is a defensible design choice but is not documented as
such.

---

## 6. Recommended design

Ordered by **cost-saved per unit of risk**, so partial rollout is possible. Each
stage is independently shippable.

### Stage 1 — Fix the defects ✅ SHIPPED (no cost change, pure correctness)

Risk: minimal. Savings: 0% directly, but makes stub metadata trustworthy, which is
a precondition for the model making sensible rehydrate decisions.

| fix | location |
|---|---|
| `extract_tool_call_summary()` now reads `msg.tool_calls` as well as inline JSON, without double-counting when a provider emits both | `crates/g3-core/src/acd.rs:208` |
| `Message.kind` persisted via `#[serde(default)]` (not `skip`); legacy sessions still load as `Regular` | `crates/g3-providers/src/lib.rs:156` |
| `dehydrate_start` clamped to `history.len()` | `crates/g3-core/src/lib.rs:2009` |
| `estimate_fragment_tokens()` now matches `ContextWindow` (JSON/code `len/3`, prose `len/4`, +20 per tool call) | `crates/g3-core/src/acd.rs:288` |

Verification: 16/16 fidelity tests, 9/9 pre-existing ACD integration tests,
487/487 `g3-core` lib tests, 34/34 `g3-providers` tests.

Note this stage changes **no cost behaviour at all**. It makes the feature honest,
which is the precondition for trusting anything it reports.

### Stage 2 — Cache-aligned tool-result pruning (the main win) — DESIGNED, NOT BUILT

Deliberately not implemented as part of this analysis. See
[§7.2](#72-the-risk-this-analysis-does-not-quantify): the accuracy risk is
unmeasured, and cost-per-session is a metric that *improves when the agent gets
dumber*. Shipping a live context-mutation path on cost evidence alone would be
backwards. The design below is simulation-validated and ready to build
deliberately, behind the metrics in §7.2.

**"Extraction then eviction."** Once a tool result has been consumed by the
model — it has read the file, run the test, seen the output — its full body is
dead weight that gets re-billed on every subsequent request. Replace the body with
a compact **receipt** while keeping the `tool_use`/`tool_result` pair
structurally intact.

Receipt contents: tool name, args digest, outcome (ok/error + size), and the
on-disk path of the spilled body so it stays recoverable.

**Salience policy:**

| rule | value | rationale |
|---|---|---|
| never-prune-last-K | 6 tool results | the model is probably still reasoning about these |
| size floor | 150 tokens | below this the receipt costs as much as the body |
| receipt size | ~40 tokens | |
| pinned tools | `plan_read`, `plan_write`, `plan_approve`, `todo_read`, `todo_write`, `remember`, `write_envelope` | load-bearing state, not observations |
| images | never evicted | image blocks must stay nested in `tool_result.content` |

**The co-scheduling contract — the load-bearing rule:**

> Eviction is **evaluated** after every tool call, but **applied** only when the
> rolling cache breakpoint slides (every 10 tool calls).

Never prune mid-prefix. The measurements in §4.2 are the entire justification: same
evictions, 92/94 sessions cheaper. A prune that is not co-scheduled with the cache
breakpoint is a cost *regression*, and must be treated as a bug.

**Invariant that must never break:** eviction must never orphan a `tool_use`.
Anthropic requires every `tool_use` block to have a matching `tool_result` in the
next message. Pruning replaces result *bodies*, never removes messages, and never
touches `tool_result_id`.

### Stage 3 — Make ACD's turn boundary cache-aware

ACD currently dehydrates unconditionally at every turn boundary, which is why it
goes negative on short turns (§4.1). Gate it: only dehydrate when the estimated
saving exceeds the cache-rebuild cost it will incur. On a turn with little
accumulated history, skip it.

Expected effect: eliminates the −4.9% tail; the 24/94 zero-or-negative sessions
become ≥0.

### Stage 4 — Lossless rehydration (optional)

Drop the 2000-char truncation and restore fragments exactly, or document the
current behaviour as a deliberate summary view. Today it is neither.

### What NOT to do

- Do **not** prune eagerly. Measured net-negative.
- Do **not** attack assistant prose or user messages. 2.1% of spend combined.
- Do **not** expect to beat ~40% without addressing the system prompt (23.9%),
  which no eviction policy can touch.

---

## 7. Method, and how to reproduce

```bash
# Property tests for the cost model (11 tests)
cargo test -p g3-core --test acd_cost_model_test

# Defect characterization (10 tests)
cargo test -p g3-core --test acd_fidelity_characterization_test

# Empirical sweep over real sessions
python3 analysis/acd/sweep.py --top 15 > analysis/acd/results.md
```

The Rust harness proves the model's *properties* on synthetic transcripts; the
Python sweep applies the same model to real `.g3/sessions/*/session.json`. Both
mirror `ContextWindow::estimate_message_tokens` exactly (code/JSON `len/3×1.1`,
prose `len/4×1.1`, +20 per tool call), guarded by
`token_estimation_matches_context_window_heuristic`.

Corpus: 94 sessions with ≥20 tool calls, 0 unparseable.

### 7.1 What was NOT measured — read before quoting these numbers

These are the honest limits of the analysis:

- **No real API `prompt_tokens`.** Everything uses g3's own char-based heuristic.
  Workspace memory records that this heuristic drifted ~48% over 809 messages in a
  live session. **Absolute token counts here could be off by tens of percent.**
  *Relative* policy comparisons are far more trustworthy, since all policies are
  measured with the same ruler.
- **No cache TTL expiry.** Ephemeral cache entries expire (5 min / 1 hr). A slow
  session loses cache hits this model grants for free, which would make all
  cache-dependent conclusions *conservative*.
- **No output-token cost.** Input only. Output is unaffected by context policy.
- **Cache breakpoint placement is idealised** — modelled as sliding cleanly every 10
  tool calls. Real code also contends with Anthropic's 4-breakpoint limit.
- **The pruning policy has never run.** Stages 2–4 are designs; their savings are
  simulated, not observed. Stage 1 numbers are the only ones describing code that
  exists.
- **Corpus bias.** These are *my* sessions on *this* repo: heavy `rg`/`read_file`,
  large text results. A workload with small tool results would see less benefit.

### 7.2 The risk this analysis does not quantify

**Pruning may degrade agent accuracy, and cost-per-session is a metric that
improves when the agent gets dumber.** An agent that forgets what it read will
re-read it — cheap per request, but more turns, worse answers, and possibly *higher*
total cost.

None of the numbers here measure answer quality. Before enabling pruning by
default, it needs:

- a **re-read rate** metric: how often the same file/command is fetched twice after
  its first result was evicted (a spike means K is too small);
- **task-completion comparison** on a fixed benchmark with pruning on vs off;
- **rehydrate frequency** as a proxy for the model noticing it lost something.

If the re-read rate rises materially, the eviction saved nothing and cost accuracy.

### 7.3 Interaction with existing mechanisms — avoid double-pruning

g3 already has three overlapping context mechanisms. A fourth must not fight them:

| mechanism | trigger | action |
|---|---|---|
| `thin_context()` (thinnify) | every 10% of context growth | replaces large tool results in the **first third** with file refs |
| `thin_context_all()` (skinnify) | compaction fallback | same, **entire** context |
| compaction | 80% of window | LLM-summarises everything |
| **proposed pruning** | every 10 tool calls | receipts for stale results, cache-aligned |

Thinnify already does something very close to the proposed eviction. The honest
framing of Stage 2 is **"make thinnify cache-aware and run it on a tool-call
cadence instead of a percentage-of-context cadence"** — it should reuse
`resolve_thinned_dir()` / `create_tool_result_modification()`, not duplicate them.
A message already thinned must be recognised as already-evicted and skipped, or
the two mechanisms will each spill the other's file references to disk.

### 7.4 The 1M-context case

With a 1M-token window, compaction (80% threshold) may never fire in a normal
session. ACD and pruning become the *only* pressure-relief valves — and since cost
scales with prefix size regardless of whether the window is full, **the larger the
context window, the more the quadratic amplification hurts, and the more per-tool-call
pruning is worth.** Workspace memory already records a related hazard: a hardcoded
absolute token guard once made 1M-context sessions compact at ~15% of their window.
Any threshold added here must be proportional, never absolute.
