# Cache-aware thinning sweep

corpus: 94 sessions with >=20 tool calls (skipped/corrupt: 0)

## Reach: how many sessions does thinning even touch?

| floor | sessions that thin | share |
|---|---|---|
| 10% | 85/94 | 90% |
| 20% | 69/94 | 73% |
| 30% | 45/94 | 48% |
| 40% | 31/94 | 33% |
| 50% | 16/94 | 17% |

## Effective cost vs never thinning

| floor | timing | aggregate | median | worst | negative sessions |
|---|---|---|---|---|---|
| 10% | immediate (today) | -9.31% | -5.29% | -57.5% | 59 |
| 10% | **cache-aligned** | 7.30% | 5.22% | 0.0% | 0 |
| 20% | immediate (today) | -8.30% | -4.78% | -57.5% | 51 |
| 20% | **cache-aligned** | 6.41% | 5.66% | 0.0% | 0 |
| 30% | immediate (today) | -6.99% | -7.31% | -57.5% | 33 |
| 30% | **cache-aligned** | 4.98% | 4.75% | 0.0% | 0 |
| 40% | immediate (today) | -7.18% | -7.81% | -36.3% | 26 |
| 40% | **cache-aligned** | 2.71% | 3.70% | 0.0% | 0 |
| 50% | immediate (today) | -4.74% | -6.49% | -32.6% | 13 |
| 50% | **cache-aligned** | 1.06% | 1.81% | 0.0% | 0 |

## Mechanism: do thinning writes land inside the cached prefix?

| floor | thin events | events hitting cached prefix | share |
|---|---|---|---|
| 10% | 151 | 142 | 94.0% |
| 20% | 120 | 119 | 99.2% |
| 30% | 75 | 74 | 98.7% |
| 40% | 47 | 47 | 100.0% |
| 50% | 23 | 23 | 100.0% |

## Safety proxy: how recent is the content we evict?

| floor | messages thinned | min age | p10 age | age<6 | age<10 |
|---|---|---|---|---|---|
| 10% | 599 | 8 | 16 | 0.0% | 4.2% |
| 20% | 562 | 9 | 24 | 0.0% | 1.2% |
| 30% | 395 | 9 | 30 | 0.0% | 0.3% |
| 40% | 321 | 16 | 45 | 0.0% | 0.0% |
| 50% | 191 | 63 | 84 | 0.0% | 0.0% |

'age' = tool calls elapsed between a result arriving and being thinned.
The 'first third' is a MOVING window, so the newest two-thirds is always
protected -- which is why min age stays high even at an aggressive floor.

## Cost of deferral: does peak context grow?

| floor | peak (immediate) | peak (aligned) | change |
|---|---|---|---|
| 10% | 6,384,529 | 6,344,209 | -0.63% |
| 20% | 6,404,716 | 6,402,643 | -0.03% |
| 30% | 6,584,465 | 6,619,553 | +0.53% |
| 40% | 6,831,034 | 6,854,413 | +0.34% |
| 50% | 7,031,999 | 7,008,747 | -0.33% |

Best floor by aggregate saving (aligned): **10% -> 7.30%**

> Savings rise monotonically as the floor drops. That is a signal the
> model lacks an accuracy term, NOT a recommendation to set the floor to
> zero. See analysis/thinning_cache_analysis.md before changing the default.
