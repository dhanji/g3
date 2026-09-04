# ACD cache-aware cost sweep

sessions analysed: 94  (skipped/corrupt: 0, min tool calls: 20)

## Prefix amplification (baseline)
median 37.8x   max 139.4x   min 11.1x
Every message is billed once per subsequent provider request. This is the term that dominates cost.

## Effective cost, base-price-equivalent tokens (top 15)

| session                                | reqs | baseline | ACD% | eager% | aligned% | ACD+aligned% |
|---|---|---|---|---|---|---|
| create_a_plan_create_a_d48a545c10ca815 |  159 |  4,954,266 |  40.5% |  -11.6% |    23.8% |    44.2% |
| id_like_to_add_model_c5d20acf4f63c77a  |  166 |  3,574,196 |  47.1% |    8.8% |    33.4% |    51.7% |
| create_a_plan_look_at_d6e41bce90d7b646 |  255 |  3,553,946 |  39.7% |  -18.1% |    15.6% |    47.4% |
| create_a_plan_in_git_ae95ff8ee303f30   |  188 |  3,486,363 |  45.6% |  -12.1% |    18.7% |    51.9% |
| create_a_plan_during_write_envelope_43 |  171 |  3,429,366 |  42.3% |  -20.2% |    12.8% |    50.4% |
| research_the_agent_skills_specificatio |  179 |  3,148,382 |  13.3% |  -10.6% |    18.3% |    31.0% |
| goal_add_an_interactive_plan_b1a7637ff |  154 |  3,137,921 |  45.5% |  -15.6% |    18.2% |    52.2% |
| analyze_the_g3_session_logs_89da49af72 |  170 |  3,044,487 |  27.2% |  -16.8% |    12.1% |    31.5% |
| create_a_plan_look_at_2910f16a3e828dfa |  172 |  2,882,634 |  26.3% |  -26.0% |    11.5% |    33.2% |
| create_a_plan_make_tools_e19de43dfecdc |  154 |  2,512,237 |  21.4% |  -29.1% |     9.7% |    29.0% |
| create_a_plan_add_an_b84af651683dba47  |  149 |  2,414,811 |  11.1% |  -20.5% |     8.9% |    19.6% |
| create_a_plan_the_research_e44671759f0 |  178 |  2,397,692 |  27.9% |  -16.8% |    15.6% |    37.0% |
| if_in_plan_mode_and_2ca71f887534d11e   |  154 |  2,348,016 |  43.5% |  -11.2% |    19.7% |    51.3% |
| create_a_plan_investigate_this_b6a44f1 |  165 |  2,285,745 |  21.7% |   -2.4% |    23.6% |    37.4% |
| read_the_draft_proposal_in_fb55f0a7bef |  131 |  2,254,661 |  33.4% |  -20.6% |    13.3% |    38.8% |

## Savings summary

| policy | median | mean | aggregate | worst | best |
|---|---|---|---|---|---|
| turn-aligned ACD | 17.6% | 18.0% | 26.5% | -4.9% | 48.9% |
| eager prune | -3.9% | -2.1% | -3.6% | -29.1% | 41.4% |
| cache-aligned prune | 17.6% | 18.5% | 20.8% | 0.2% | 41.6% |
| ACD + cache-aligned prune | 26.7% | 27.2% | 35.4% | 1.5% | 52.2% |

Eager (every-tool-call) pruning is WORSE than doing nothing in 61/94 sessions — it keeps invalidating the cached prefix.

## Where billed input tokens actually go (baseline)

| category | share |
|---|---|
| tool results | 39.0% |
| assistant messages + tool_call inputs | 35.0% |
| system prompt | 23.9% |
| assistant prose | 1.4% |
| user messages | 0.7% |

## What turn-aligned ACD can even reach

| region | share of billed tokens | reachable by ACD? |
|---|---|---|
| spend incurred after a later turn began | 38.2% | yes |
| system prompt | 23.9% | no (never dehydrated) |
| spend inside the message's own turn | 38.0% | no (ACD fires only at turn boundaries) |

Turn-aligned ACD can touch at most 38.2% of billed spend. The 38.0% burned inside the current turn's tool loop is reachable ONLY by per-tool-call pruning, and the 23.9% system prompt is reachable by neither.

## Cache-write penalty (why eager pruning loses)

| policy | evictions | tokens re-written at 1.25x |
|---|---|---|
| baseline | 0 | 37,401,926 |
| eager prune | 2,747 | 45,589,429 |
| cache-aligned prune | 2,578 | 25,693,470 |

Cache-aligned pruning is cheaper than eager pruning in 92/94 sessions.
Sessions where turn-aligned ACD saved nothing or cost more: 24/94.
Sessions where ACD + cache-aligned pruning is the cheapest policy: 66/94.

## Fidelity defects (confirmed by executable characterization tests)

See `crates/g3-core/tests/acd_fidelity_characterization_test.rs`. Each test pins
down actual current behaviour, not desired behaviour.

| # | Defect | Consequence | Test |
|---|---|---|---|
| 1 | `extract_tool_call_summary()` scans `msg.content` for inline JSON and never reads `msg.tool_calls` | With any native-tool-calling provider (the default), **every stub claims "no tool calls"** — the exact metadata the model needs to judge whether to rehydrate | `defect_stub_reports_no_tool_calls_for_structured_tool_calls` |
| 1b | Mixed inline/structured transcripts are partially counted | Silent undercount (3 calls reported as 1) is worse than an obvious zero | `defect_mixed_transcript_undercounts_by_exactly_the_structured_share` |
| 2 | `Message.kind` is `#[serde(skip)]` | On `--resume`, stubs reload as `Regular`; `rposition(is_dehydrated_stub)` returns `None`; `dehydrate_start` resets to 0 and **already-dehydrated content is re-dehydrated into nested stubs** | `defect_message_kind_is_lost_across_serialization` |
| 3 | The stub replacing a span is plain prose | The live context loses every structured tool interaction from the dehydrated span (the on-disk fragment does retain them) | `defect_fragment_preserves_tool_calls_but_stub_cannot_express_them` |
| 4 | `estimate_fragment_tokens()` uses flat `len/4`, `ContextWindow` uses `len/3` for JSON/code | `execute_rehydrate()`'s capacity check undercounts by >20% on JSON payloads and will green-light a rehydration that overflows the window | `defect_fragment_token_estimate_undercounts_json_heavy_content` |
| 5 | `dehydrate_start = last_stub_index + 2` assumes a Summary always follows the stub, but it is only appended when non-empty | On an empty final response the index overshoots, nothing is dehydrated, and the context grows unchecked — silent failure of the feature's purpose | `boundary_stub_without_following_summary_makes_plus_two_overshoot` |

Defects 1 and 2 are the serious ones: together they mean that in the default
configuration the stub is misinformative, and on resume the mechanism corrupts
its own chain.
