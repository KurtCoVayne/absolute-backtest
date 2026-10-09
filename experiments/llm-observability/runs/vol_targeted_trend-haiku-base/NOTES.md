# NOTES: vol_targeted_trend in abt (haiku-base)

## Files
- `final.dsl`: the final strategy, byte-identical to `attempts/5.dsl`. `abt check env lib final.dsl`: 0 errors, 0 warnings, "4 params, 0 literals, 6 rules".
- `run.sh`: the convention command (`--synthetic --days 1000 --symbols SPY`, default accounting).
- `attempts/1.dsl` to `attempts/5.dsl`: every version, each saved before its check or run.
- `runs/`: stdout and `--dump` output (Parquet) of each run cited here. `runs/dump_final` is the final default run.

## The strategy as written
- `mode target`, `resolution @1d`, `env equities_1d`, `uses features` (for `logret`).
- Trend: `ma(A,T,M)` is the mean of the last `ma_bars` = 200 closes, today's included: `rows(T, ma_bars, min ma_bars)`. Long when `close > ma` (strict).
- Vol: `ann_vol` is the sample std (divisor n-1) of the last `vol_bars` = 20 daily log returns, times `sqrt(trading_days)` = sqrt(252).
- Weight: when long, `target_weight(A, least(1.0, target_vol / ann_vol))`, i.e. min(1, 0.10 / vol). Otherwise `target_weight(A, 0)`.
- Params: ma_bars 200, vol_bars 20, target_vol 0.10, trading_days 252.
- Verified: a pandas recomputation from `runs/dump_final/data/close.parquet` reproduces all 801 kernel decisions (max abs difference 2.3e-15).

## Attempts
1. Derived output relations declared `+A`. WF-2 error.
2. Rules began at `close`, which takes `+A`, so A was unbound. WF-2 error. Fixed by starting every rule from `universe(A, T)`.
3. Clean check and full run. A cap test (`--param target_vol=0.2`) then halted on leverage, because the cap did not hold (see Hard 4).
4. `least(1.0, ...)` in place of `least(1, ...)`. The cap holds. Clean check.
5. Final: comments rewritten, logic unchanged. The default output is identical to attempt 3's.
abt invocations: 15 before run.sh (1 usage, 5 checks, 9 runs), plus 1 for run.sh.

## What was hard
1. Modes. Every rule must bind its `+` inputs first. `close` and `volume` take `+A`, so each rule starts from `universe(A, T)`. Two of my five versions failed on this.
2. "Flat otherwise" cannot be `not above_ma(A, T)`: `close` is not declared complete, so WF-5 refuses `not`. I wrote the complement as a comparison (`P <= M`). The only difference is that no flat decision is emitted before the first 200 closes, when the book is flat anyway.
3. "200-day" is ambiguous here. Windows take calendar durations (`window(T, 200d, min K)`; the README's `sma(A, T, 50d, 30, S)`), and 200 calendar days hold about 138 bars. I used `rows(T, 200, min 200)` for 200 trading bars.
4. The cap silently failed. `W = least(1, target_vol / V)` passed the checker, but with target 0.20 the kernel emitted target_weight 1.320397 on 2022-12-28. That is exactly 0.20 / 0.151470, and the run halted on the leverage policy. 305 of the 435 long days exceed 1 under that target. `least(1.0, ...)` capped exactly: 305 decisions equal 1.0 and none exceed it. My guess, not confirmed: the bare integer 1 is typed Count and compared wrongly with a Scalar. The "all arguments must agree" check passes because the dimensions match.
5. Default accounting sizes weights against starting capital, not equity. In the convention run, cash went negative on 4 bars. The first was 2024-07-04: a buy of 957 shares at 52.59 with 39.3k of cash left cash at -11.1k, and margin interest was 20.75. Max gross/equity was 1.039, and no halt fired. With `--compounding on`, max leverage is 0.896 (about the max weight, 0.896), and cash never goes negative (minimum 94,108).

## Docs: gaps and errors
- The README syntax page has no list of builtin functions. `sqrt`, `least`, `abs`, `log` and `exp` appear only in the semantic model (section 2, typing rules). `rows` appears in the semantic model's builtin table and in one README line on executor features.
- The typing of a bare integer in a function argument is undocumented. "Count or Scalar from context" gives no rule for function arguments. The checker accepted a Count-typed 1 in `least` with a Scalar, and the result was silently wrong, with no diagnostic.
- Leverage wording conflicts. The usage text says `--on-leverage halt` applies when a fill "would borrow or put gross exposure above equity", but the convention run borrowed on a fill (2024-07-04) with no halt. The semantic model says fixed-base leverage is measured against the base. The nav `leverage` column is gross/equity (cap test: 998,841 / 399,698 = 2.499), so the reported figure and the policy use different bases, and the docs do not say which applies where.
- The TASK convention names no accounting mode, and the default (fixed base) is the one under which a "weight capped at one" does not cap equity. The docs give no guidance for sizing strategies.
- There is no target-mode example (the README example is delta mode). Re-issue of unfilled targets, fills at the next close, and the last decision dropped with "no next bar" had to be inferred from the execution contract.
- Literal exemption: W5 exempts 1 "in any unit", and `1.0` also counted as 0 literals. That matches the rule, but the docs do not say it applies to decimal forms.

## Constructs guessed or unverified
- `least`: guessed from the typing rules. Works with a decimal literal (`1.0`), not with a bare `1`, in this kernel.
- `sqrt`: guessed from the typing rules. Works: the annualization matches sqrt(252) exactly.
- `rows(T, N, min K)` with Count params for N and K: worked.
- `param NAME : T = v in lo..hi`: worked. `--param NAME=VALUE` overrides: worked.
- Not used: ticker literals (the universe comes from `--symbols SPY`), `not` over derived relations, and `if`/`else` (not tested; I did not look for it).

## Spec faithfulness: where I am unsure
1. "200-day" is 200 trading bars, not 200 calendar days.
2. The 20-bar vol window, and the choice of the index's own trailing vol as the sizing estimate, are my choices. The spec gives neither.
3. Weight semantics and accounting. run.sh uses the convention (fixed base). The spec's weight reads as a fraction of equity, which needs `--compounding on`; its numbers are below. I did not change the strategy for this, because the executor sets the base.
4. Price is raw `close`, not total return. The synthetic run has no dividends or splits, so this has no effect here.
5. The average includes today's close, and "above" is strict.
6. Daily re-sizing (the spec's daily decisions) changes the weight on every bar: 451 fills and 30.4M of turnover on a 1M book. Impact (267k) and slippage (33k) dominate. Frictionless, the same decisions make +21k. I did not add a rebalancing band, since the spec does not ask for one.
7. The rule has no symbol filter. The run's `--symbols SPY` defines the universe. With more symbols, each would be sized on its own, with no cap across them.
8. Realized vol. While long, the position targets 10% by construction (median asset vol 17.3% times median weight 0.58, about 10%). The book's realized vol is 7.3% because it is flat 46% of bars: sqrt(0.543) x 10% = 7.4%.

## Headline results (run.sh, convention)
- Decisions: 801, from 2022-10-07 to 2025-10-31. 435 long, with weights from 0.40 to 0.90 (median 0.58), and 366 flat. First long decision 2022-12-28.
- Fills: 451 (238 buys, 213 sells), fill ratio 1.000. Dropped: 1 (2025-10-31, target 0.4953, "no next bar"). The position is still open at the end: SPY 9,868 shares.
- Metrics line: `cagr -0.071359   max_drawdown 0.270626   sharpe -0.9781 (population -0.9786)   volatility 0.0729`
- Additive: total return -25.43%, end equity 745,658.95, total_pnl -282,892.97, trades 18 closed (19 entries), win rate 0.111, profit factor 0.787 per period and 0.213 per trade.
- Costs: impact 267,191; slippage 33,009; commissions 3,459; fees 411; turnover 30,362,340; margin interest 20.75.
- Exposure: max gross 895,177; max leverage 1.039.

Sensitivities, same strategy with flags changed:

| run | fills | total return | CAGR | Sharpe | max DD | total pnl | max leverage |
| --- | --- | --- | --- | --- | --- | --- | --- |
| convention (fixed base) | 451 | -25.43% | -7.14% | -0.978 | 27.06% | -282,893 | 1.039 |
| `--compounding on` | 450 | -23.73% | -6.61% | -0.906 | 25.41% | -237,344 | 0.896 |
| `--frictionless` (fixed base) | 451 | +1.17% | +0.29% | 0.077 | 11.38% | +21,205 | n/a |

Cap test (`--param target_vol=0.2`, fixed base, final.dsl): 305 of 801 decisions at exactly 1.0 and none above. 447 fills, total return -44.56%, max gross 999,999.84 (1x base), max leverage 2.499 after equity fell to 399,698. No halt, because fills are judged against the base.
