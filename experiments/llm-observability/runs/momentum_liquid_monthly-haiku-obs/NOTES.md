# NOTES: momentum_liquid_monthly in abt

## Files and invocations

- `final.dsl` is the final strategy. Its code is identical to `attempts/1.dsl`; only the header comment differs. It is saved as `attempts/3.dsl`.
- `run.sh` is the full backtest per the TASK.md convention, run from the workspace. Its output is in `run_output.txt`, and it reproduces attempt 1's headline numbers exactly.
- `attempts/1.dsl` is the first full version, with the index label "SPX" chosen by hand. `attempts/2.dsl` is a deliberate probe: the same file with label "NDX", to see what the diagnostics say when nothing is selected. It is not a candidate.
- `ledger1/` holds the parquet ledger (universe, mstart, cand, sel, held, position, bar) from the attempt-1 default run. `fills_default.txt` is the same run with `--fills`.
- `bin/abt` was invoked 18 times in total (usage, check, show, runs, queries).

## Spec clause to rule

| Clause | Construct |
| --- | --- |
| each month start | `mstart(T) :- bar(T), month_start(T).` |
| ten index members | `member(A, T, "SPX")` inside `cand` |
| liquid, at least $1M average daily volume | catalog `liquid(A, T, min_adv)` (mean of close times volume over 20d, at least 10 bars) |
| 12-1 momentum on total returns | `tri`, a total-return index recursed over `prev` from catalog `ret` (dividends included); `mom = tri(T - skip) / tri(T - lookback) - 1` |
| strongest ten, ranked | `sel(A, T) :- mstart(T), top(n_hold, cand(A, T, M), by (M desc, A asc)).` |
| equally weighted | `target_weight(A, 1 / n_hold)` |
| everything else flat at the rebalance | `target_weight(A, 0)` for every `held` name not in `sel` |

## What was hard

1. **The index label.** The spec says "index members" and `member` takes a closed `Label` vocabulary that the data bundle defines (`docs/data-bundle.md`, which is not in the workspace). Nothing I could run listed the labels. I chose "SPX". The evidence that it is present is the non-empty candidate set and the ledger count below. `abt check` accepts any string label: the NDX probe also checks with 0 errors and 0 warnings. Only a run exposes a wrong label.
2. **Total-return momentum.** The catalog library has `ret` (one-period total return) and `close_adj` (split-adjusted only), but no momentum or total-return index. The features library's `momentum` is written against `equities_1d`, which the docs say is an E error with the catalog environment. I did not test that. I built `tri` by copying the `cumfactor` pattern from `lib/catalog.dsl`: a recursion through `prev` that passed WF-4 on the first try.
3. **Top-N over entities.** The spec's example `mom_candidate(+A, ...)` has `+A`, so it cannot be the range of a `top` (`+` arguments must be bound). I declared `cand(-A, @T, -M)` and used `by (M desc, A asc)`. The explicit `A asc` is a tie-break I added, because the identity rule for `-A` outputs is not clear to me. The checker raised no warning.
4. **Equal weight versus the accounting default.** `target_weight` is a fraction of the starting capital under the default fixed-base accounting, not a fraction of equity. This is the largest gap between the spec and the headline numbers (see below).
5. **Literals.** I moved `12mo`, `1mo` and `1_000_000 USD` into `param` so the rules have 0 in-rule literals (W5). `--param 'min_adv=2_000_000_000 USD'` parsed the unit literal correctly.

## What the docs lacked or got wrong

1. **Missing referenced documents.** Many comments and sections point to `docs/data-bundle.md` (sections 2, 3, 5, 6, 7) and `corpus/`. Neither is in the workspace. The label vocabulary, catalog conventions, delisting haircuts and cost defaults all live there.
2. **Two descriptions of `universe`.** `docs/semantic-model.md` section 6 says "market data (membership as of T)". `env/equities_1d_v2.dsl` says "Listed and tradable on T". That leaves "index members" ambiguous between `universe` and `member`.
3. **Accounting is only in the semantic model.** The README and `docs/observability.md` do not say that `target_weight` is a fraction of starting capital by default. Only `docs/semantic-model.md` section 7 does. The run prints `accounting: fixed base 1000000.00`, and that is the only in-run signal. By the end of the default run the book was 58.6% invested (cash 901,815.60 of equity 2,177,955.33).
4. **`--symbol` on a decision.** `query --rel decide --at 2023-02-01 --symbol E15 --explain` printed `decide at 2023-02-01 (@1d): 0 tuple(s) for E15` on the same output where `rule ...decide#1 fired ... with 10 solution(s); first solution: A = E15 ... W = 0.1`. The entity sits inside the decision constructor, so `--symbol` filters nothing. The docs do not cover decision constructors.
5. **The path summary contradicts itself.** The never-fired line in the NDX probe reads: "`cand` is the first empty relation on this path; what it reads is not empty: universe+ (14800 tuples), member+ (14800 tuples), liquid+ (0 tuples), mom+ (0 tuples)". The reads listed as "not empty" include two empty ones. The rule-level line was the one that named the cause: "literal 2 `member(A, T, "NDX")` has no solution".
6. **A misleading EMPTY.** The 2B-screen run printed "empty relations (demanded, never derived; a rule reading one positively never fires): div_at". That is true only for `div_or_zero#1`. The second rule of `div_or_zero` covers the no-dividend case, so the warning alarms on something harmless.
7. **Entity names.** `--symbols A,...,O` produces entities named `E1` to `E15` in every output (decisions, positions, `--symbol`). Neither TASK.md nor the docs say so.
8. **Delisting defaults.** The usage shows `--haircut` defaults with bankruptcy at 1, meaning a total write-off. No doc at the strategy level says a delisting can zero a held position (see the E15 item under unsure).
9. Minor: `--quiet` did not suppress the report on the ledger run. `abt show` prints `lookback : Duration = 1y` for the `12mo` I wrote; this is correct but needs a note when comparing.

## Constructs guessed at or unverified

- Guessed: the label literal `"SPX"` in a body atom. It is accepted, and the run confirms it matches the synthetic members.
- Accepted but not probed: `cand(-A, ...)` as a `top` range with `by (M desc, A asc)`; no W4 or W7 warning.
- Accepted: `lag(T, skip, T1)` with `Duration` parameters; `W = 1 / n_hold` (Count to Scalar); `not sel(A, T)` on a `top`-derived relation (`show` lists `sel` as complete).
- Not used or tested: `rank`, `resample`, `asof`, `rows`, order types, `--dividends reinvest`, `--study`. None were needed.
- No construct I tried was missing from the language. The gaps were in data, accounting and documentation.

## Spec rules I am not sure I implemented faithfully

1. **Momentum window.** I used `[T - 12mo, T - 1mo]`, which is 11 months of return, following the features library convention (`Lookback 12mo, Skip 1mo`). If the spec means 12 months of return that ends one month back, `lookback` should be `13mo`. I did not run that sensitivity.
2. **Index membership.** Every universe name carries "SPX" at every month start, so the literal never removes a name. Evidence: the coverage line `member 14800 tuples in the data` equals `universe 14800`; `liquid` is called 681 times, and the ledger counts 681 universe pairs at month starts. A different index would produce zero candidates (the NDX probe).
3. **Liquidity window and scale.** The spec gives no averaging window, so I used the catalog default: 20 calendar days, at least 10 bars, mean of close times volume. The $1M threshold never binds. `query --rel adv ... E15,20d,10` returned `adv(E15, 2023-02-01, 20d, 10, 1767027170.1846664)`. The 15 of the 681 pairs that fail the screen are, I infer, the 15 names on the first bar, where the window holds one bar. I did not query them individually. With a $2B screen, `liquid` had 24 tuples out of 681 calls.
4. **Equal weight.** Under the default fixed-base accounting, 10 names at 0.1 is 10% of starting capital, so the book drifts into cash. With `--compounding on`, weights are fractions of equity and the book ends fully invested (final cash -862.20). I kept the TASK convention for `run.sh` for comparability and flag this as the decision you should make.
5. **First rebalance skipped.** The 2023-01-02 rebalance produces no decision. Its 12-month lookback would fall on 2022-01-02, one day before the first bar on 2022-01-03. Decisions start 2023-02-01. I think this is correct, since the rule has no data before that date.
6. **Short candidate list.** If fewer than `n_hold` names qualify, the book holds fewer and the rest is cash. This is not binding here: from February 2023 on, 14 or 15 candidates qualified each month.
7. **Delisting.** The spec is silent. E15 was delisted for bankruptcy. Its last universe bar is 2025-01-24, with 10 shares and a last close of 7,471.92 (about $74.7k). The forced sale on 2025-01-27 was `fill 2025-01-27 E15 -10 @ 0.0000 (forced: margin call or delisting)`, which matches `delisted(E15, 2025-01-27, bankruptcy)`. That is roughly 7.5% of starting capital, and the strategy had no exit rule before it. I did not add one, because the spec does not ask for one.
8. **Total-return chain.** `tri` multiplies one-period returns, so a missing bar for a listed name would break its chain permanently. The coverage shows `ret 14129 calls 14129 with tuples`, so no demanded `ret` call failed. I did not check every bar.
9. **Execution timing.** A decision at a month's first bar uses that bar's close and fills at the next bar's close, per the execution contract. I read "at each month start" that way.
10. **Two no-op re-targets.** Two buys (E12 on 2025-03-03, E10 on 2025-04-01) left the share count unchanged, because whole-share rounding of a fixed-base target absorbed the change. These are the reason for 362 decisions and 361 fills.

## What each inspection command told me

| Command | What it said | What I did |
| --- | --- | --- |
| `bin/abt check` (attempt 1) | `momentum_liquid_monthly: degrees of freedom: 4 params, 0 literals, 8 rules (+ 3 library literals)`; `0 error(s), 0 warning(s)` | Nothing. The same clean result came with the wrong NDX label, so `check` cannot see labels. |
| `bin/abt show` | `cand ... reads: universe+, member+, liquid+, mom+`; `sel ... reads: mstart+, cand~`; `decide ... reads: sel+, mstart+, held+, sel-` | Confirmed the wiring. `sel` is complete through the reduction, so `not sel` is legal. No change. |
| `run` coverage (attempt 1) | `decisions: 362   fills: 361`; `cand 46 calls 33 with tuples`; `liquid 681 calls 666 with tuples`; `universe 14800`, `member 14800` | Explained the 13 empty months (the lookback predates the data). The equal counts prompted the membership check, and the 666 prompted the ADV check. |
| `--ledger` plus python | Recomputed top-10 by `M desc` matched `sel` in all 33 months (0 mismatches). Ranking on 2023-02-01 ran E15 at +1.65 down to E14 at -0.66. Universe pairs at month starts = 681 = liquid calls | Confirmed the ranking direction and that the membership literal is inert here. No change. |
| `query --rel decide ... --symbol E15 --explain` | `rule ...decide#1 fired ... with 10 solution(s)`; `rule ...decide#2 did not fire ... literal 2 held(A, T, _) has no solution` | Confirmed the sell rule and the buy rule. Ignored the `0 tuple(s)` line, which is the `--symbol` quirk. |
| `query --rel mom ... --symbol E15 --explain` | `first solution: ... I0 = 2.4014077374924025, I1 = 6.363024103706697, M = 1.6497058389389103, T0 = 2022-02-01, T1 = 2022-12-30` | Confirmed the lookback bars and M = I1 / I0 - 1. No change. |
| `query --rel adv ... E15,20d,10 --explain` | `adv(E15, 2023-02-01, 20d, 10, 1767027170.1846664)` | Showed the $1M screen is roughly 1,700 times below the data's dollar volume. I ran the $2B sensitivity. |
| `run --param 'min_adv=2_000_000_000 USD'` | `liquid 681 calls 24 with tuples 24 tuples`; `decisions: 29` | Showed the literal is wired. Kept $1M, the spec's number. |
| `run --compounding on` | `final cash -862.20`, `CAGR 0.213378`, `Sharpe 1.0847` | Changed my view of the accounting: the default ends 41% in cash. I kept the convention for `run.sh` and flagged it. |
| NDX probe run | `decisions: 0`; `never fired: decide derived no tuple over 1000 bars`; `liquid 0 calls, never demanded`; `cand#1 ... literal 2 member(A, T, "NDX") has no solution` | Showed a wrong label is diagnosable from the rule line, which is the decisive one. The path summary contradicts itself (item 5 above). |
| `run --fills` plus python | 361 = 362 - 2 no-change re-targets + 1 forced delisting sale (`fill 2025-01-27 E15 -10 @ 0.0000`) | Found the largest single loss in the book, and led to `delisted(E15, 2025-01-27, bankruptcy)` and `close(E15, 2025-01-24, 7471.92)`. Flagged, not changed. |

## Headline results (`run.sh`, default TASK convention)

- Decisions: 362, which is 330 buys at 0.1 and 32 targets at 0. They fall at 33 rebalances from 2023-02-01 to 2025-10-01 (46 month starts, 13 with no momentum history yet).
- Fills: 361, from the reconciliation above.
- Metrics line: `metrics per bar (252 a year, 999 periods): cagr 0.216952   max_drawdown 0.146298   sharpe 1.0893 (population 1.0898)   volatility 0.1983`. End equity 2,177,955.33; total return 117.80%.
- Costs: commissions 1,112.84, fees 170.85, slippage 51,426.76, impact 33,720.96, turnover 12,388,863.95.
- End book: 10 positions, final cash 901,815.60.
- Sensitivities, same strategy. `--compounding on`: 362 decisions, 360 fills, CAGR 0.213378, Sharpe 1.0847, max drawdown 0.142516, end equity 2,152,704.06, final cash -862.20. `min_adv` of $2B: 29 decisions, end equity 931,086.67, total return -6.9%.
