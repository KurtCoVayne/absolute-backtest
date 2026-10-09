# NOTES: momentum_liquid_monthly in abt

## Deliverables
- `final.dsl`: the final strategy (identical to `attempts/4.dsl`).
- `run.sh`: the full backtest, `bin/abt run --strategy momentum_liquid_monthly --synthetic --days 1000 --symbols A,...,O --compounding on env lib final.dsl`.
- `attempts/1.dsl` to `attempts/4.dsl`: every version. Attempt 3 holds the logic; attempt 4 only renames `obs` to `min_obs` and adds comments (same output).
- `logs/`: saved outputs of the final runs and sensitivity checks (the outputs of attempts 1 to 3 were printed to the terminal only).
- Budget: 14 `bin/abt` invocations (usage, 4 checks, 9 runs). Log analysis used python and awk on the saved output, not `bin/abt`.

## Final strategy
- `env equities_1d_v2`, `uses catalog`, `mode target`, `resolution @1d`.
- Rebalance dates: `mstart(T) :- bar(T), month_start(T).` (first bar of each month).
- Momentum: the catalog's `logret` (total return, dividends included) summed over `window(T, 12mo)` minus the sum over `window(T, 1mo)`, which is the log total return over [T-12mo, T-1mo). It is gated by `lag(T, look, _)` (the data reaches back a full look-back) and needs at least 240 bars in the look-back window.
- Candidates: `universe`, `member(A, T, index)` with `index = "SPX"`, catalog `liquid(A, T, min_adv)` with `min_adv = 1_000_000 USD` (20-day average of close x volume), and a momentum value.
- Selection: `top(hold, cand(...), by (M desc, A asc))` with `hold = 10`, on rebalance dates.
- Decisions: `target_weight(A, 1 / hold)` for selected names; `target_weight(A, 0)` for held names that are not selected.
- Checker: 0 errors, 0 warnings, `6 params, 0 literals`. Only the structural 0 and 1 remain in rules.

## Attempts
| # | Change | Check | Run |
|---|---|---|---|
| 1 | index as literal `"SP500"`, 200-bar floor, no full-history gate | clean | 0 decisions, no warning |
| 2 | index as a Label parameter (default `SP500`), 200-bar floor, no gate; run with `--param index=SPX` | clean | 395 decisions; first rebalance 2022-11-01 on about 10 months of history |
| 3 | add `lag(T, look, _)` gate; `min_obs` 240; default `SPX` | clean | 362 decisions; first rebalance 2023-02-01 (default fixed-base accounting) |
| 4 | readability only (rename, comments) | clean | identical to the final run |

## What was hard
1. **Index label.** The spec says "index" without a name, and the label vocabulary lives in the data-bundle doc, which is not in `docs/`. `"SP500"` gave zero decisions, and neither the checker nor the run said anything about it. `"SPX"` gave members (found by trial with `--param`). Labels look unvalidated, so a wrong label looks like an empty index (or `SP500` exists with no members; I could not tell which).
2. **Equal weight under the default accounting.** With `--compounding off` (the default), `target_weight` is a fraction of the starting capital, so positions stop scaling with equity. I used `--compounding on`, so each name is 10% of current equity at every rebalance.
3. **Exits in target mode.** A held name that drops out of the top ten needs an explicit `target_weight(A, 0)`. The docs do not say whether a held name without a target is left alone or closed. I assumed it is left alone, so I emitted explicit zero targets (not tested).
4. **Skip window.** There is no half-open or left-open window. I used the difference of two closed `window` sums, which gives [T-12mo, T-1mo).
5. **Full history.** `min K` alone lets a partial look-back through (attempt 2 ranked on about 10 months). The `lag(T, look, _)` gate fixes it. The docs describe the behaviour of `lag` but not this use as an idiom.
6. **Degrees of freedom.** Every non-structural literal in a strategy rule is warned (W5), so all numbers became parameters, including `min K` (a Count parameter is accepted).
7. **Readability of output.** Decisions print Equity ids (E1..E15), not the symbols A..O, and no per-name values (momentum, ADV) are printed. I did not try `abt explain` to inspect the ranking.

## What the docs lacked or got wrong
- `docs/README.md` and `docs/semantic-model.md` cite `data-bundle.md` (labels, cost model, degrees of freedom, metrics). It is not provided.
- Label literals and label parameters are not validated: a wrong index yields zero decisions silently.
- `lib/features.dsl` (`momentum`, `adv`, `mom_candidate`) is written against `equities_1d` and uses price returns with no dividends, so it does not fit this spec. I used `catalog` instead. I did not test the E rule, but the environment mismatch is plain.
- The run reports `dropped: 0`, yet three target decisions produced no order (see Results). The summary has no count of unexecuted targets.
- The fill annotation "forced: margin call or delisting" is ambiguous when no margin is configured. The delisting appears only in the actions line.
- The default fixed-base accounting is documented but is a trap for any "equally weighted" spec.

## Constructs I guessed at
- `-A` on `cand` and `selected` (the top's output). The checker accepted it; I did not test `+A`.
- `logsum(+A, @T, ...)` with `universe(A, T)` in the body, copied from the catalog's `adv` pattern.
- `lag(T, look, _)` as a full-history gate.
- A Label-typed parameter, `param index : Label = "SPX"`, accepted, and overridable with `--param`.
- Not used: order types (`moc`, `moo`), margin, and cost-model changes.

## Spec points I am unsure I implemented faithfully
1. **Index.** I assumed S&P 500 (`SPX`), the only label that produced members. Other index labels may exist.
2. **ADV.** I used the catalog's `liquid` (20-day average, at least 10 observations, at least $1m). The spec gives no window.
3. **Momentum window.** The window closes on both ends, so the return runs from the close before T-12mo to the close before T-1mo (one-bar boundary convention). The 240-bar floor is my choice. The first rebalance is 2023-02-01, because 2023-01-02 has no bar 12 months earlier (data starts 2022-01-03).
4. **Fewer than ten names.** The spec is silent. In this run exactly ten names entered at all 33 rebalances, so the case did not arise. With fewer, my weights of 1/10 would leave cash.
5. **Timing.** Decisions are taken at the month-start close, and the kernel's default `market` fills them at the next bar's close. The `moc` order type would fill on the decision day's close.
6. **Costs and delistings.** Kernel defaults (commission, slippage at 0.1 x realised volatility, impact, zero proceeds for bankruptcy-type delistings). The spec is silent on all of them.
7. **Sample.** The 1000-bar window starts 2022-01-03, and the book is flat until 2023-02-01. This dilutes CAGR over the full run.
8. **Total returns.** Signals use the catalog's total return. The book credits dividends as cash (the default).

## Results (final `run.sh`)
- Decisions 362: 330 targets of 0.1 and 32 exits (target 0). Fills 360. Dropped 0.
- Fills reconcile as 362 - 3 unfilled targets (E6 on 2024-03-01, E12 on 2024-04-01, E10 on 2025-09-01) + 1 forced zero-price sale of E15 on 2025-01-27 (delisting) = 360. I could not find why the three targets had no order. `--participation 0` gives the same fills and metrics (only a warning line is added), so the participation cap is not the cause.
- Metrics line: end_equity 2,171,886.44; total_return 117.19%; cagr 21.61%; max_drawdown 14.18%; sharpe 1.095 (population 1.096); volatility 19.62%; total_pnl 1,171,886; trades 30; win_rate 0.300; commissions 1,747; fees 283; slippage 88,228; impact 79,309; turnover 21.38M; max gross 2.52M; max leverage 1.003 (no halt).
- `--verify-causality`: passed at 5 sampled bars (`logs/run3_compounding_on.txt`).
- Sensitivities:
  - `--compounding off`: end 2,194,173; cagr 21.9%; sharpe 1.098; max drawdown 14.6%; fills 361.
  - `--delist-proceeds last-price`: end 2,347,795; total return 134.8%; cagr 24.0%; sharpe 1.216. The zero-proceeds exit of E15 costs about $176k of ending equity under the default.
  - `--participation 0`: identical to the final run except for one extra warning line that says there is no participation cap.
