#!/usr/bin/env python3
"""Cache-aware cost sweep over real g3 session transcripts.

This is the empirical counterpart to `crates/g3-core/tests/acd_cost_model_test.rs`.
The Rust harness proves the model's *properties* on synthetic transcripts; this
script applies the same model to real `.g3/sessions/*/session.json` files to get
the actual numbers.

Both implementations must stay in agreement. The token heuristic below mirrors
`ContextWindow::estimate_message_tokens` exactly:
    code/JSON -> len/3 * 1.1     prose -> len/4 * 1.1     tool_call -> +20

Usage:
    python3 analysis/acd/sweep.py [--sessions-dir .g3/sessions] [--min-tools 20]
"""

import argparse
import bisect
import glob
import json
import math
import os
import statistics
import sys

# ---------------------------------------------------------------------------
# Pricing (Anthropic-style, as multipliers on the base input price)
# ---------------------------------------------------------------------------
RATE_FRESH = 1.0
RATE_CACHE_READ = 0.1
RATE_CACHE_WRITE = 1.25

CACHE_BREAKPOINT_EVERY = 10   # g3 slides its rolling breakpoint every 10 tool calls
STUB_TOKENS = 120.0           # measured from Fragment::generate_stub()

# Eviction policy knobs (mirrors the Rust harness)
NEVER_PRUNE_LAST_K = 6
SIZE_FLOOR_TOKENS = 150.0
RECEIPT_TOKENS = 40.0
PINNED_TOOLS = {
    "plan_read", "plan_write", "plan_approve",
    "todo_read", "todo_write",
    "remember", "write_envelope",
}


# ---------------------------------------------------------------------------
# Token estimation
# ---------------------------------------------------------------------------
def estimate_tokens(text):
    if not text:
        return 0.0
    if "{" in text or "```" in text or "fn " in text:
        base = len(text) / 3.0
    else:
        base = len(text) / 4.0
    return math.ceil(base * 1.1)


def message_tokens(m):
    total = estimate_tokens(m.get("content") or "")
    for tc in (m.get("tool_calls") or []):
        s = json.dumps(tc.get("input", {}))
        total += math.ceil(len(s) / 3.0 * 1.1) + 20
    return float(total)


def classify(m):
    content = m.get("content") or ""
    if m.get("tool_result_id") or content.startswith("Tool result"):
        return "tool_result"
    role = m.get("role")
    if role == "system":
        return "system"
    if role == "assistant":
        # A tool call may be structured (`tool_calls`) or, in older sessions,
        # inline JSON in the message body. Both are tool-call spend and must be
        # attributed as such, or the breakdown badly understates it.
        if (m.get("tool_calls") or []) or '"tool"' in content:
            return "assistant_tc"
        return "assistant_text"
    return "user"


# ---------------------------------------------------------------------------
# Simulator
# ---------------------------------------------------------------------------
def evict_stale(ctx):
    """Evict stale tool-result bodies. Returns (n_evicted, first_mutated_index)."""
    positions = [i for i, e in enumerate(ctx) if e["kind"] == "tool_result"]
    if len(positions) <= NEVER_PRUNE_LAST_K:
        return 0, None
    stale = positions[:len(positions) - NEVER_PRUNE_LAST_K]
    evicted = 0
    first = None
    for idx in stale:
        e = ctx[idx]
        if e["tokens"] <= SIZE_FLOOR_TOKENS or e.get("tool") in PINNED_TOOLS:
            continue
        e["tokens"] = RECEIPT_TOKENS
        e["kind"] = "tool_result_receipt"
        evicted += 1
        if first is None:
            first = idx
    return evicted, first


def simulate(hist, policy):
    """Replay a transcript, pricing every provider request.

    policy in {baseline, acd, eager_prune, cache_aligned_prune, acd+aligned}
    """
    do_acd = policy in ("acd", "acd+aligned")
    prune_mode = None
    if policy == "eager_prune":
        prune_mode = "eager"
    elif policy in ("cache_aligned_prune", "acd+aligned"):
        prune_mode = "aligned"

    ctx = []
    cached_upto = 0
    cache_dirty = False
    total_tools = 0
    pending_tool = None

    raw = 0.0
    cost = 0.0
    reads = writes = fresh_total = 0.0
    requests = 0
    evictions = 0

    for m in hist:
        kind = classify(m)

        # Turn boundary: the only place ACD ever acts.
        if kind == "user" and do_acd and ctx:
            system = [e for e in ctx if e["kind"] == "system"]
            summaries = [e for e in ctx if e["kind"] == "assistant_text"]
            n_system = len(system)
            ctx = system + [{"tokens": STUB_TOKENS, "kind": "stub", "tool": None}]
            if summaries:
                ctx.append(summaries[-1])
            if cached_upto > n_system:
                cache_dirty = True
            cached_upto = min(cached_upto, n_system)

        if kind == "assistant_tc":
            tcs = m.get("tool_calls") or []
            pending_tool = tcs[0].get("name") if tcs else None

        ctx.append({
            "tokens": message_tokens(m),
            "kind": kind,
            "tool": pending_tool if kind == "tool_result" else None,
        })

        if kind != "tool_result":
            continue
        total_tools += 1

        at_boundary = (total_tools % CACHE_BREAKPOINT_EVERY == 0)
        apply = (prune_mode == "eager") or (prune_mode == "aligned" and at_boundary)

        if apply:
            n, first = evict_stale(ctx)
            evictions += n
            if first is not None and first < cached_upto:
                cache_dirty = True
                cached_upto = first

        if at_boundary:
            cached_upto = len(ctx)
            cache_dirty = True

        cu = min(cached_upto, len(ctx))
        cached = sum(e["tokens"] for e in ctx[:cu])
        fresh = sum(e["tokens"] for e in ctx[cu:])

        if cache_dirty:
            writes += cached
            cost += cached * RATE_CACHE_WRITE
            cache_dirty = False
        else:
            reads += cached
            cost += cached * RATE_CACHE_READ
        fresh_total += fresh
        cost += fresh * RATE_FRESH

        raw += cached + fresh
        requests += 1

    final_prefix = sum(e["tokens"] for e in ctx)
    return {
        "requests": requests,
        "raw": raw,
        "cost": cost,
        "reads": reads,
        "writes": writes,
        "fresh": fresh_total,
        "final_prefix": final_prefix,
        "evictions": evictions,
        "amplification": (raw / final_prefix) if final_prefix else 0.0,
    }


# ---------------------------------------------------------------------------
# Spend attribution: where do the billed tokens actually go?
# ---------------------------------------------------------------------------
def attribute_spend(hist):
    """Billed tokens per category under the baseline policy."""
    req_idx = [i for i, m in enumerate(hist) if classify(m) == "tool_result"]
    if not req_idx:
        return {}
    out = {}
    for i, m in enumerate(hist):
        n = len(req_idx) - bisect.bisect_left(req_idx, i)
        k = classify(m)
        out[k] = out.get(k, 0.0) + message_tokens(m) * n
    return out


def intra_vs_inter_turn(hist):
    """Split billed tokens into what turn-aligned ACD can reach vs what it cannot.

    ACD collapses only messages belonging to *previous* user turns, and only at
    the moment a new user turn begins. So for each message we ask: at the time
    it was billed, had a later user turn already started? If not, that spend was
    structurally unreachable by ACD no matter how aggressive it is.

    This is the honest version of the "ceiling" number: it is computed per
    *billing event*, not per message, because a message billed 100 times while
    still inside its own turn contributes 100 units of unreachable spend.
    """
    req_idx = [i for i, m in enumerate(hist) if classify(m) == "tool_result"]
    if not req_idx:
        return 0.0, 0.0, 0.0
    turn_starts = [i for i, m in enumerate(hist) if classify(m) == "user"]

    reachable = unreachable_system = unreachable_current = 0.0
    for i, m in enumerate(hist):
        k = classify(m)
        tok = message_tokens(m)
        # Requests that carry this message in their prefix.
        first_req = bisect.bisect_left(req_idx, i)
        if k == "system":
            unreachable_system += tok * (len(req_idx) - first_req)
            continue
        # The next user turn after this message; ACD can only collapse it for
        # requests issued at or after that point.
        nxt = bisect.bisect_right(turn_starts, i)
        boundary = turn_starts[nxt] if nxt < len(turn_starts) else None
        for r in req_idx[first_req:]:
            if boundary is not None and r > boundary:
                reachable += tok
            else:
                unreachable_current += tok
    return reachable, unreachable_system, unreachable_current


# ---------------------------------------------------------------------------
# Main sweep
# ---------------------------------------------------------------------------
def load_sessions(sessions_dir, min_tools):
    """Yield (name, history). Corrupt files are skipped with a warning."""
    ok, skipped = [], 0
    for f in sorted(glob.glob(os.path.join(sessions_dir, "*", "session.json"))):
        name = os.path.basename(os.path.dirname(f))
        try:
            with open(f) as fh:
                d = json.load(fh)
        except (json.JSONDecodeError, OSError, UnicodeDecodeError) as e:
            print(f"  ! skipping {name}: {type(e).__name__}: {e}", file=sys.stderr)
            skipped += 1
            continue
        if not isinstance(d, dict):
            skipped += 1
            continue
        hist = (d.get("context_window") or {}).get("conversation_history") or []
        if not isinstance(hist, list):
            skipped += 1
            continue
        n_tools = sum(1 for m in hist if isinstance(m, dict) and classify(m) == "tool_result")
        if n_tools < min_tools:
            continue
        ok.append((name, [m for m in hist if isinstance(m, dict)]))
    return ok, skipped


def pct(part, whole):
    return (part * 100.0 / whole) if whole else 0.0


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--sessions-dir", default=".g3/sessions")
    ap.add_argument("--min-tools", type=int, default=20)
    ap.add_argument("--top", type=int, default=15)
    args = ap.parse_args()

    sessions, skipped = load_sessions(args.sessions_dir, args.min_tools)
    if not sessions:
        print("No sessions matched. Nothing to report.")
        return 0

    rows = []
    agg_spend = {}
    reach_tot = sys_tot = cur_tot = 0.0

    for name, hist in sessions:
        base = simulate(hist, "baseline")
        acd = simulate(hist, "acd")
        eager = simulate(hist, "eager_prune")
        aligned = simulate(hist, "cache_aligned_prune")
        combo = simulate(hist, "acd+aligned")
        rows.append((name, base, acd, eager, aligned, combo))

        for k, v in attribute_spend(hist).items():
            agg_spend[k] = agg_spend.get(k, 0.0) + v
        r, s, c = intra_vs_inter_turn(hist)
        reach_tot += r
        sys_tot += s
        cur_tot += c

    rows.sort(key=lambda r: -r[1]["cost"])

    print(f"# ACD cache-aware cost sweep")
    print(f"\nsessions analysed: {len(rows)}  (skipped/corrupt: {skipped}, "
          f"min tool calls: {args.min_tools})")

    # --- amplification ---
    amps = [r[1]["amplification"] for r in rows]
    print(f"\n## Prefix amplification (baseline)")
    print(f"median {statistics.median(amps):.1f}x   max {max(amps):.1f}x   "
          f"min {min(amps):.1f}x")
    print("Every message is billed once per subsequent provider request. This "
          "is the term that dominates cost.")

    # --- per-session table ---
    print(f"\n## Effective cost, base-price-equivalent tokens (top {args.top})\n")
    print(f"| {'session':<38} | reqs | baseline | ACD% | eager% | aligned% | ACD+aligned% |")
    print("|---|---|---|---|---|---|---|")
    for name, b, a, e, al, c in rows[:args.top]:
        print(f"| {name[:38]:<38} | {b['requests']:>4} | {b['cost']:>10,.0f} | "
              f"{100 - pct(a['cost'], b['cost']):>5.1f}% | "
              f"{100 - pct(e['cost'], b['cost']):>6.1f}% | "
              f"{100 - pct(al['cost'], b['cost']):>7.1f}% | "
              f"{100 - pct(c['cost'], b['cost']):>7.1f}% |")

    def savings(i):
        return [100 - pct(r[i]["cost"], r[1]["cost"]) for r in rows]

    print(f"\n## Savings summary\n")
    print(f"| policy | median | mean | aggregate | worst | best |")
    print(f"|---|---|---|---|---|---|")
    tb = sum(r[1]["cost"] for r in rows)
    for i, label in [(2, "turn-aligned ACD"), (3, "eager prune"),
                     (4, "cache-aligned prune"), (5, "ACD + cache-aligned prune")]:
        s = savings(i)
        t = sum(r[i]["cost"] for r in rows)
        print(f"| {label} | {statistics.median(s):.1f}% | {statistics.mean(s):.1f}% | "
              f"{100 - pct(t, tb):.1f}% | {min(s):.1f}% | {max(s):.1f}% |")

    n_eager_worse = sum(1 for r in rows if r[3]["cost"] > r[1]["cost"])
    print(f"\nEager (every-tool-call) pruning is WORSE than doing nothing in "
          f"{n_eager_worse}/{len(rows)} sessions — it keeps invalidating the cached prefix.")

    # --- where the money goes ---
    print(f"\n## Where billed input tokens actually go (baseline)\n")
    tot = sum(agg_spend.values())
    print("| category | share |")
    print("|---|---|")
    labels = {
        "assistant_tc": "assistant messages + tool_call inputs",
        "system": "system prompt",
        "tool_result": "tool results",
        "assistant_text": "assistant prose",
        "user": "user messages",
    }
    for k, v in sorted(agg_spend.items(), key=lambda x: -x[1]):
        print(f"| {labels.get(k, k)} | {pct(v, tot):.1f}% |")

    # --- the structural finding ---
    print(f"\n## What turn-aligned ACD can even reach\n")
    grand = reach_tot + sys_tot + cur_tot
    print("| region | share of billed tokens | reachable by ACD? |")
    print("|---|---|---|")
    print(f"| spend incurred after a later turn began | {pct(reach_tot, grand):.1f}% | yes |")
    print(f"| system prompt | {pct(sys_tot, grand):.1f}% | no (never dehydrated) |")
    print(f"| spend inside the message's own turn | {pct(cur_tot, grand):.1f}% | no (ACD fires only at turn boundaries) |")
    print(f"\nTurn-aligned ACD can touch at most {pct(reach_tot, grand):.1f}% of billed spend. "
          f"The {pct(cur_tot, grand):.1f}% burned inside the current turn's tool loop is "
          f"reachable ONLY by per-tool-call pruning, and the {pct(sys_tot, grand):.1f}% "
          f"system prompt is reachable by neither.")

    # --- cache write penalty ---
    print(f"\n## Cache-write penalty (why eager pruning loses)\n")
    ew = sum(r[3]["writes"] for r in rows)
    aw = sum(r[4]["writes"] for r in rows)
    bw = sum(r[1]["writes"] for r in rows)
    ee = sum(r[3]["evictions"] for r in rows)
    ae = sum(r[4]["evictions"] for r in rows)
    print("| policy | evictions | tokens re-written at 1.25x |")
    print("|---|---|---|")
    print(f"| baseline | 0 | {bw:,.0f} |")
    print(f"| eager prune | {ee:,} | {ew:,.0f} |")
    print(f"| cache-aligned prune | {ae:,} | {aw:,.0f} |")

    n_worse = sum(1 for r in rows if r[3]["cost"] > r[4]["cost"])
    print(f"\nCache-aligned pruning is cheaper than eager pruning in "
          f"{n_worse}/{len(rows)} sessions.")

    n_acd_negative = sum(1 for r in rows if r[2]["cost"] >= r[1]["cost"])
    print(f"Sessions where turn-aligned ACD saved nothing or cost more: "
          f"{n_acd_negative}/{len(rows)}.")

    # --- does combining beat either alone? ---
    n_combo_best = sum(1 for r in rows
                       if r[5]["cost"] <= min(r[2]["cost"], r[4]["cost"]))
    print(f"Sessions where ACD + cache-aligned pruning is the cheapest policy: "
          f"{n_combo_best}/{len(rows)}.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
