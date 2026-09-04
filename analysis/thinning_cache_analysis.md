# Cache-aware thinning — measurement (plan item T1)

Baseline measurement for making `thin_context()` cache-aware. Produced **before**
any behaviour change, so the fix can be judged against it.

Reproduce:

```bash
cargo test -p g3-core --test thinning_cost_model_test    # 14 property tests
python3 analysis/acd/sweep_thinning.py                   # empirical sweep
```

Raw output: [`analysis/acd/thinning_results.md`](acd/thinning_results.md).

---

## ⚠️ Correction to numbers I reported earlier

While building this harness I found that **two exploratory figures I quoted in
conversation were wrong**, in opposite directions. Both came from a throwaway
script that did not model what `thin_context()` actually rewrites.

| what | claimed | actual | why it was wrong |
|---|---|---|---|
| floor=20%, cache-aligned | **17.0%** | **6.4%** | The script thinned *every* large message in the first third, including assistant tool-call inputs for tools that are never thinned. That inflated thinnable mass by ~31%. |
| floor=50%, cache-aligned | 3.1% | 1.1% | Same cause. |

`thin_context()` rewrites exactly two things:

1. **Tool results** over 500 chars (`create_tool_result_modification`)
2. **`write_file` / `str_replace` argument payloads** (`thin_write_file_args`,
   `thin_str_replace_args`)

Everything else — `shell`, `read_file`, `rg`, `code_search` tool-call inputs,
assistant prose — is left alone. Token mass in the corpus:

| category | share | thinnable? |
|---|---|---|
| tool results | 51.2% | yes |
| `write_file`/`str_replace` args | 18.2% | yes |
| other tool-call inputs | 30.6% | **no** |

My first sweep omitted category 2 (under-reporting by 18%); the exploratory
script included category 3 (over-reporting by 31%). The harness now models
exactly categories 1 and 2, and the tests guard that.

**The direction of every conclusion is unchanged. The magnitude is roughly a
third of what I first said.**

---

## 1. The mechanism

`thin_context()` rewrites messages in `[0, len/3)` — the first third. The
rolling Anthropic cache breakpoint slides to the *end* of history every 10 tool
calls. So thinning writes almost always land *below* the breakpoint, inside the
cached prefix, invalidating it and forcing a 1.25× re-write where a 0.1× read
would otherwise have happened.

| floor | thin events | events hitting the cached prefix | share |
|---|---|---|---|
| 10% | 135 | 126 | 93.3% |
| 20% | 105 | 104 | **99.0%** |
| 30% | 62 | 61 | 98.4% |
| 40% | 40 | 40 | **100.0%** |
| 50% | 19 | 19 | **100.0%** |

At the current floor of 50%, **every single thinning event corrupts the cache.**

## 2. Cost: thinning today is actively harmful

94 real sessions with ≥20 tool calls. Effective input cost vs never thinning
(cache read 0.1×, write 1.25×, fresh 1.0×):

| floor | timing | aggregate | median | worst | negative sessions |
|---|---|---|---|---|---|
| 10% | immediate (today's design) | **−9.31%** | −5.29% | −57.5% | 59/94 |
| 10% | **cache-aligned** | **+7.30%** | +5.22% | 0.0% | **0** |
| 20% | immediate | −8.30% | −4.78% | −57.5% | 51/94 |
| 20% | **cache-aligned** | **+6.41%** | +5.66% | 0.0% | **0** |
| 30% | immediate | −6.99% | −7.31% | −57.5% | 33/94 |
| 30% | **cache-aligned** | +4.98% | +4.75% | 0.0% | **0** |
| 40% | immediate | −7.18% | −7.81% | −36.3% | 26/94 |
| 40% | **cache-aligned** | +2.71% | +3.70% | 0.0% | **0** |
| 50% (today) | immediate | **−4.74%** | −6.49% | −32.6% | 13/94 |
| 50% | **cache-aligned** | +1.06% | +1.81% | 0.0% | **0** |

Two results matter more than the headline percentages:

1. **Thinning as currently implemented costs money.** At the shipped floor of
   50% it is −4.74% aggregate — worse than not thinning at all — and negative in
   13 of 94 sessions, worst case −32.6%. It is a context-relief mechanism being
   paid for in cache misses.
2. **Alignment is what makes an aggressive floor safe.** Immediate thinning gets
   *worse* as the floor drops (−4.74% → −9.31%), because more thins mean more
   cache invalidations. Cache-aligned thinning gets *better* (+1.06% → +7.30%)
   and is negative in **zero sessions at every floor tested**. The worst
   observed case is exactly 0.0% — a no-op.

**Lowering the floor without fixing alignment first would make things
substantially worse.** That ordering is not optional.

## 3. Reach — the honest denominator

| floor | sessions that thin at all |
|---|---|
| 50% (today) | 12/94 (13%) |
| 40% | 25/94 (27%) |
| 30% | 38/94 (40%) |
| 20% | 62/94 (66%) |
| 10% | 78/94 (83%) |

At the current floor this change is a **strict no-op for 87% of sessions**.
Median peak context usage is 37.2%; exactly one session in the corpus ever
reached 80%. The aggregate numbers above are diluted accordingly — the
per-session effect on sessions that *do* thin is larger, but so is the variance.

## 4. Safety: what does an aggressive floor actually evict?

The property that makes a low floor defensible is that **`[0, len/3)` is a
moving window**. As history grows, the first third grows with it, so the newest
two-thirds is always protected.

| floor | messages thinned | min age | p10 age | age<6 | age<10 |
|---|---|---|---|---|---|
| 50% | 111 | 70 | 83 | 0.0% | 0.0% |
| 30% | 292 | 9 | 28 | 0.0% | 0.3% |
| 20% | 455 | 9 | 23 | 0.0% | 1.5% |
| 10% | 477 | 8 | 16 | 0.0% | 5.2% |

*age* = tool calls elapsed between a result arriving and being thinned.

Even at floor=10, **nothing younger than 8 tool calls is ever evicted** — outside
the `NEVER_PRUNE_LAST_K = 6` window from the ACD design. No evicted content is
under 6 tool calls old at any floor.

## 5. Cost of deferral: peak context

Deferral means relief arrives up to 10 tool calls later. Bounded, and measured
negligible:

| floor | peak (immediate) | peak (aligned) | change |
|---|---|---|---|
| 10% | 6,492,184 | 6,478,998 | **−0.20%** |
| 20% | 6,513,162 | 6,539,059 | +0.40% |
| 30% | 6,688,921 | 6,752,841 | +0.96% |
| 50% | 7,107,415 | 7,122,173 | +0.21% |

Under 1% in every case, and *negative* at floor=10. The reason is item 6.

## 6. An assumption of mine that was wrong

I predicted deferral would change *when* content is thinned but not *what* —
"same content, better timing". **It changes both, in our favour.**

Because the target window is `[0, len/3)` and history keeps growing while a thin
request is latched, the first third is **larger** by the time a deferred thin
fires. Deferral therefore sweeps up strictly *more* content in *fewer*
cache-invalidating passes. Measured in the harness: 26 messages thinned deferred
vs 22 immediate, across fewer thin events.

Pinned as
`boundary_two_thresholds_crossed_before_a_breakpoint_coalesce_into_one_thin`.

## 7. What this model does NOT capture

- **No accuracy term.** Evicting context always looks free here. Savings rise
  monotonically as the floor drops — that is a signal the model is *incomplete*,
  not a recommendation to set the floor to zero. At floor=10 we would be
  spilling most of what the agent read.
- **No real API `prompt_tokens`.** Uses g3's char heuristic, which workspace
  memory records drifting ~48% over a long session. Relative comparisons between
  policies are far more trustworthy than absolute magnitudes.
- **No cache TTL expiry.** Ephemeral entries expire (5 min / 1 hr); a slow
  session loses hits this model grants free. Makes cache-dependent conclusions
  *conservative*.
- **Idealised breakpoint placement** — modelled as a clean slide every 10 tool
  calls, ignoring Anthropic's 4-breakpoint ceiling.
- **Corpus bias.** These are sessions on this repo: heavy `rg`/`read_file`, large
  text results.

## 8. Implication for the remaining plan items

- **T2/T3 (alignment) is the prerequisite, not the optimisation.** Thinning is
  currently net-negative; alignment is what turns it positive.
- **T6 (configurable floor) must default to 50.** Lowering the default is a
  separate, evidence-gated decision — §7 shows the model cannot see the cost of
  being wrong about it.
- **T5 (idempotent thinning) becomes load-bearing at low floors.** At floor=20,
  455 messages are thinned vs 111 today; string-prefix detection of
  "already thinned" is far likelier to misfire at that volume.

## 9. Scout: a floor=5 configuration that this cost model does NOT veto (2026-09-04)

`ContextWindow::thinning_floor_percent` (default 50, matching
`DEFAULT_THINNING_FLOOR_PERCENT`) is now configurable via
`agent.thinning_floor_percent` / `--thinning-floor`. The `scout` research
agent (`crates/g3-core/src/tools/research.rs::run_scout_agent`) is spawned
with `--thinning-floor=5` — thinning becomes eligible at 5/10/15/.../80
instead of 50/60/70/80, so large webdriver page-source dumps get discarded
almost immediately after the tool call that produced them.

**This is deliberately scoped to scout only, not a default-floor change**,
for two independent reasons:

1. **§8's T6 conclusion stands for the default.** Lowering the shared default
   floor is still gated on T2/T3 (cache-aligned deferral) landing first — the
   cost model above shows an aggressive floor is net-negative without it.
2. **Scout doesn't pay the cost this model measures.** Every `run_scout_agent`
   invocation is `--new-session` — a fresh, single-purpose, single-invocation
   background process (`research.rs`) that runs to completion and exits.
   There is no multi-turn session for a rolling Anthropic cache breakpoint to
   *have* built up in the first place, so the cache-invalidation penalty this
   whole document is about (§1-§2) has nothing to invalidate. The trade this
   document warns against — thinning corrupting a cache that would otherwise
   have been read cheaply — requires a cache to exist across turns; scout's
   only "turn" is the one it's given.

**Alignment (the other half of the ask) needed no new code.** `should_thin()`
is consulted at exactly two call sites, and both already sit at tool-call
boundaries, never mid-stream:

- `lib.rs`'s tool-execution loop, immediately **before** dispatching each
  completed tool call (i.e. after the model has decided to call a tool, before
  that tool — e.g. `webdriver_get_page_source` — actually runs and its huge
  result lands in history).
- `ensure_context_capacity()`, consulted pre-stream, before the next request
  is even sent.

Neither is reachable while a tool result (e.g. a multi-hundred-KB HTML page
source) is being written into `conversation_history` — thinning only ever
sees a **prior** tool result once the *next* tool call is about to start. So
"discard all HTML after nav, aligned to new tool call start" was already
true of the mechanism; the missing piece was purely that the floor itself
was a hardcoded constant, not that timing needed fixing.

**What this does NOT fix**: thinning still only rewrites tool results over
500 chars and `write_file`/`str_replace` args (§1) — a giant single
`webdriver_get_page_source` result comfortably clears that bar, but if scout
ever needs *other* large payloads (e.g. a giant tool-call *input*) discarded,
that's outside `thin_context()`'s scope entirely and would need a separate
change. Also unresolved: floor=5 thins far more aggressively within scout's
own single session — per §4's moving-window property this still can't evict
anything younger than ~1-2 tool calls at such a low floor, so a scout run
that needs to *reference* HTML it fetched more than a couple of tool calls
ago will find it already replaced with a file pointer. That's the intended
trade (headroom over recall) but worth remembering if scout starts behaving
oddly on multi-step research.
