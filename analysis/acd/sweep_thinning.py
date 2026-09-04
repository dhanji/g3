#!/usr/bin/env python3
"""Cache-aware cost sweep for context THINNING over real g3 sessions.

Empirical counterpart to `crates/g3-core/tests/thinning_cost_model_test.rs`.
The Rust harness proves the model's properties on synthetic transcripts; this
applies the same model to real `.g3/sessions/*/session.json`.

Models:
  * `should_thin()`  -- fires on 10% threshold steps between a FLOOR and the
                        80% ceiling, at most once per threshold.
  * `thin_context()` -- rewrites tool results >500 chars in `[0, len/3)`.
  * the rolling Anthropic cache breakpoint, which slides to the end of history
    every 10 tool calls.

The floor is parameterised because "should we thin at 20% instead of 50%?" is a
measurement question.

WHAT THIS DOES NOT MODEL: accuracy loss. Evicting context always looks free
here. A monotonic "lower floor is better" result means the model is incomplete,
not that the floor should be zero.

Usage:
    python3 analysis/acd/sweep_thinning.py
    python3 analysis/acd/sweep_thinning.py --floors 10,20,30,50
"""

import argparse
import glob
import json
import math
import os
import statistics
import sys

RATE_FRESH = 1.0
RATE_CACHE_READ = 0.1
RATE_CACHE_WRITE = 1.25

CACHE_BREAKPOINT_EVERY = 10
THIN_CEILING_PERCENT = 80
THIN_SIZE_FLOOR_CHARS = 500
THINNED_TOKENS = 15.0


# ---------------------------------------------------------------------------
# Token estimation -- mirrors ContextWindow::estimate_message_tokens
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
        total += math.ceil(len(json.dumps(tc.get("input", {}))) / 3.0 * 1.1) + 20
    return float(total)


def is_tool_result(m):
    return bool(m.get("tool_result_id")) or (m.get("content") or "").startswith("Tool result")


# Tools whose *arguments* thin_context() rewrites via thin_write_file_args /
# thin_str_replace_args. Their large `content`/`diff` payloads are spilled to
# disk exactly like tool results are.
THINNABLE_ARG_TOOLS = ("write_file", "str_replace")


def is_thinnable_tool_call(m):
    """Assistant message whose tool-call ARGS thin_context() would rewrite.

    Omitting these under-reports thinnable mass by ~18%; treating *every* large
    assistant message as thinnable over-reports it by ~31%. Both errors were
    made during exploration -- model exactly what the code does.
    """
    for tc in (m.get("tool_calls") or []):
        if tc.get("name") in THINNABLE_ARG_TOOLS:
            return True
    content = m.get("content") or ""
    if '"tool"' in content:
        return any('"%s"' % t in content for t in THINNABLE_ARG_TOOLS)
    return False


def thinnable_chars(m):
    """Chars thin_context() could reclaim from this message."""
    if is_tool_result(m):
        return len(m.get("content") or "")
    total = 0
    for tc in (m.get("tool_calls") or []):
        if tc.get("name") in THINNABLE_ARG_TOOLS:
            total += len(json.dumps(tc.get("input", {})))
    if not (m.get("tool_calls") or []) and is_thinnable_tool_call(m):
        total += len(m.get("content") or "")
    return total


# ---------------------------------------------------------------------------
# Simulator
# ---------------------------------------------------------------------------
def apply_thin_first_third(ctx, now, ages):
    """Thin large un-thinned tool results in [0, len/3). Returns (n, lowest_idx)."""
    end = max(len(ctx) // 3, 1)
    n = 0
    lowest = None
    for i in range(min(end, len(ctx))):
        e = ctx[i]
        if e["thinned"] or not e["thinnable"] or e["chars"] <= THIN_SIZE_FLOOR_CHARS:
            continue
        ages.append(now - e["born"])
        # Tool results collapse entirely; tool-call args collapse only the
        # payload, so the surrounding message text survives.
        e["tokens"] = THINNED_TOKENS if e["tr"] else max(
            THINNED_TOKENS, e["tokens"] - e["thinnable_tokens"]
        )
        e["thinned"] = True
        n += 1
        if lowest is None:
            lowest = i
    return n, lowest


def simulate(hist, total_tokens, floor, timing):
    """timing in {never, immediate, aligned}."""
    ctx = []
    cached_upto = 0
    dirty = False
    tools = 0
    last_thin_pct = 0
    pending = False

    cost = raw = reads = writes = fresh_tot = 0.0
    requests = thin_events = msgs_thinned = hit_cache = 0
    peak = 0.0
    ages = []

    for m in hist:
        tr = is_tool_result(m)
        th_chars = thinnable_chars(m)
        ctx.append({
            "tokens": message_tokens(m),
            "tr": tr,
            "thinnable": tr or is_thinnable_tool_call(m),
            "chars": th_chars,
            "thinnable_tokens": math.ceil(th_chars / 3.0 * 1.1),
            "thinned": False,
            "born": tools,
        })
        if not is_tool_result(m):
            continue
        tools += 1

        used = sum(e["tokens"] for e in ctx)
        peak = max(peak, used)

        if timing != "never" and total_tokens > 0:
            pct = int(used * 100 / total_tokens)
            if pct >= floor:
                thr = (pct // 10) * 10
                if thr > last_thin_pct and thr <= THIN_CEILING_PERCENT:
                    last_thin_pct = thr
                    pending = True

        at_bp = (tools % CACHE_BREAKPOINT_EVERY == 0)
        apply = pending and (
            timing == "immediate" or (timing == "aligned" and at_bp)
        )

        if apply:
            n, lowest = apply_thin_first_third(ctx, tools, ages)
            if n:
                thin_events += 1
                msgs_thinned += n
                if lowest is not None and lowest < cached_upto:
                    hit_cache += 1
                    cached_upto = lowest
                    if timing == "immediate":
                        dirty = True
            pending = False

        if at_bp:
            cached_upto = len(ctx)
            dirty = True

        split = min(cached_upto, len(ctx))
        cached = sum(e["tokens"] for e in ctx[:split])
        fresh = sum(e["tokens"] for e in ctx[split:])

        if dirty:
            writes += cached
            cost += cached * RATE_CACHE_WRITE
            dirty = False
        else:
            reads += cached
            cost += cached * RATE_CACHE_READ
        fresh_tot += fresh
        cost += fresh * RATE_FRESH
        raw += cached + fresh
        requests += 1

    return {
        "cost": cost, "raw": raw, "requests": requests,
        "reads": reads, "writes": writes, "fresh": fresh_tot,
        "thin_events": thin_events, "msgs_thinned": msgs_thinned,
        "hit_cache": hit_cache, "peak": peak, "ages": ages,
        "tool_result_tokens": sum(
            e["tokens"] for e in ctx if e["tr"] and not e["thinned"]
        ),
    }


# ---------------------------------------------------------------------------
# Loading
# ---------------------------------------------------------------------------
def load_sessions(sessions_dir, min_tools):
    ok, skipped = [], 0
    for f in sorted(glob.glob(os.path.join(sessions_dir, "*", "session.json"))):
        name = os.path.basename(os.path.dirname(f))
        try:
            with open(f) as fh:
                d = json.load(fh)
        except (json.JSONDecodeError, OSError, UnicodeDecodeError) as e:
            print(f"  ! skipping {name}: {type(e).__name__}", file=sys.stderr)
            skipped += 1
            continue
        if not isinstance(d, dict):
            skipped += 1
            continue
        cw = d.get("context_window")
        if not isinstance(cw, dict):
            skipped += 1
            continue
        hist = cw.get("conversation_history")
        if not isinstance(hist, list):
            skipped += 1
            continue
        hist = [m for m in hist if isinstance(m, dict)]
        if sum(1 for m in hist if is_tool_result(m)) < min_tools:
            continue
        ok.append((name, hist, float(cw.get("total_tokens") or 200000)))
    return ok, skipped


def pct(part, whole):
    return (part * 100.0 / whole) if whole else 0.0


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--sessions-dir", default=".g3/sessions")
    ap.add_argument("--min-tools", type=int, default=20)
    ap.add_argument("--floors", default="10,20,30,40,50")
    ap.add_argument("--top", type=int, default=12)
    args = ap.parse_args()

    floors = [int(x) for x in args.floors.split(",")]
    sessions, skipped = load_sessions(args.sessions_dir, args.min_tools)
    if not sessions:
        print("No sessions matched. Nothing to report.")
        return 0

    print("# Cache-aware thinning sweep\n")
    print(f"corpus: {len(sessions)} sessions with >={args.min_tools} tool calls "
          f"(skipped/corrupt: {skipped})")

    # How many sessions ever thin, at each floor?
    print("\n## Reach: how many sessions does thinning even touch?\n")
    print("| floor | sessions that thin | share |")
    print("|---|---|---|")
    for fl in floors:
        n = sum(1 for _, h, tt in sessions
                if simulate(h, tt, fl, "aligned")["thin_events"] > 0)
        print(f"| {fl}% | {n}/{len(sessions)} | {pct(n, len(sessions)):.0f}% |")

    # Main comparison table.
    print("\n## Effective cost vs never thinning\n")
    print("| floor | timing | aggregate | median | worst | negative sessions |")
    print("|---|---|---|---|---|---|")
    best = None
    for fl in floors:
        for timing in ("immediate", "aligned"):
            base, alt = [], []
            for _, h, tt in sessions:
                base.append(simulate(h, tt, fl, "never")["cost"])
                alt.append(simulate(h, tt, fl, timing)["cost"])
            pairs = [(b, a) for b, a in zip(base, alt) if abs(b - a) > 1e-9]
            sav = [100 - pct(a, b) for b, a in pairs] or [0.0]
            agg = 100 - pct(sum(alt), sum(base))
            neg = sum(1 for s in sav if s < 0)
            label = "immediate (today)" if timing == "immediate" else "**cache-aligned**"
            print(f"| {fl}% | {label} | {agg:.2f}% | {statistics.median(sav):.2f}% "
                  f"| {min(sav):.1f}% | {neg} |")
            if timing == "aligned" and (best is None or agg > best[1]):
                best = (fl, agg)

    # Cache collision rate -- the mechanism.
    print("\n## Mechanism: do thinning writes land inside the cached prefix?\n")
    print("| floor | thin events | events hitting cached prefix | share |")
    print("|---|---|---|---|")
    for fl in floors:
        ev = hc = 0
        for _, h, tt in sessions:
            r = simulate(h, tt, fl, "immediate")
            ev += r["thin_events"]
            hc += r["hit_cache"]
        print(f"| {fl}% | {ev} | {hc} | {pct(hc, ev):.1f}% |")

    # Safety proxy.
    print("\n## Safety proxy: how recent is the content we evict?\n")
    print("| floor | messages thinned | min age | p10 age | age<6 | age<10 |")
    print("|---|---|---|---|---|---|")
    for fl in floors:
        ages = []
        n = 0
        for _, h, tt in sessions:
            r = simulate(h, tt, fl, "aligned")
            ages += r["ages"]
            n += r["msgs_thinned"]
        if not ages:
            print(f"| {fl}% | 0 | - | - | - | - |")
            continue
        a = sorted(ages)
        p10 = a[len(a) // 10]
        u6 = pct(sum(1 for x in a if x < 6), len(a))
        u10 = pct(sum(1 for x in a if x < 10), len(a))
        print(f"| {fl}% | {n} | {min(a)} | {p10} | {u6:.1f}% | {u10:.1f}% |")
    print("\n'age' = tool calls elapsed between a result arriving and being thinned.")
    print("The 'first third' is a MOVING window, so the newest two-thirds is always")
    print("protected -- which is why min age stays high even at an aggressive floor.")

    # Peak-context effect of deferral.
    print("\n## Cost of deferral: does peak context grow?\n")
    print("| floor | peak (immediate) | peak (aligned) | change |")
    print("|---|---|---|---|")
    for fl in floors:
        pi = sum(simulate(h, tt, fl, "immediate")["peak"] for _, h, tt in sessions)
        pa = sum(simulate(h, tt, fl, "aligned")["peak"] for _, h, tt in sessions)
        print(f"| {fl}% | {pi:,.0f} | {pa:,.0f} | {pct(pa - pi, pi):+.2f}% |")

    if best:
        print(f"\nBest floor by aggregate saving (aligned): **{best[0]}% -> {best[1]:.2f}%**")
        print("\n> Savings rise monotonically as the floor drops. That is a signal the")
        print("> model lacks an accuracy term, NOT a recommendation to set the floor to")
        print("> zero. See analysis/thinning_cache_analysis.md before changing the default.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
