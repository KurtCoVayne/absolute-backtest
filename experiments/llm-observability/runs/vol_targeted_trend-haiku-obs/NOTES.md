# NOTES: vol_targeted_trend (haiku-obs)

Files: `final.dsl` (= `attempts/3.dsl`), `run.sh` (full backtest, TASK run convention).
Attempts: `attempts/1.dsl` (did not check: binding order), `attempts/2.dsl` (checked clean, but the cap was broken; the stress test found it), `attempts/3.dsl` (cap fixed; checks clean; final).
Helper: `runs/verify_decisions.py` (independent recomputation from dumped closes); `runs/` holds ledger, dump and log outputs.

## The strategy (final.dsl)
- Trend: `ma` = mean of the last 200 closes (`rows(T, ma_rows, min ma_rows)`, trading bars). `trend_up` when close > ma; `trend_dn` when close <= ma.
- Vol: `ann_vol` = sample std (divisor n-1) of the last 60 daily log returns, times `sqrt(ann)`, ann = 252.
- Weight: `vol_weight` = target_vol / ann_vol, target_vol = 0.10. Cap at one as two disjoint decide rules: `W <= 1` gives W; `W > 1` gives 1.
- Flat: `target_weight(A, 0)` on `trend_dn`. Target mode, daily @1d. Signal at close T, filled at close T+1 (executor contract).
- Params: ma_rows 200 (50..300), vol_rows 60 (20..120), target_vol 0.10 (0.05..0.20), ann 252 (no range). Literals: none (0 and 1 are exempt). Check: `degrees of freedom: 4 params, 0 literals, 9 rules`, 0 errors, 0 warnings.

## 1. What was hard
1. Binding order. Rules that read `close(+A, ...)` must bind A first from `universe(A, T)`. My first draft started with `close` and was refused.
2. `least` did not cap (see 2 and the stress run). This is the main bug. The check, the coverage report and `query --explain` at the base target all passed; only a stress run (`target_vol=0.20`) exposed it. At base, every raw weight is below 1 (min realized vol is 13%, so 0.10/vol is at most 0.77), so the cap never binds and no inspection of the base run could show it.
3. Accounting: "weight capped at one" is only "one of equity" under `--compounding on`. Under the default fixed base the weight is a fraction of starting capital (see 4).
4. Dumping and recomputing was the only way to check values of `+A` relations (`ma`, `ann_vol`), because `--ledger` refuses them.

## 2. What the docs lacked or got wrong
1. `least`/`greatest` are only typed in semantic-model.md section 2 ("abs, least, greatest: preserve the dimension; all arguments must agree"). The name suggests min, and I assumed min. It is not: `least(1, x)` returned x = 1.2133782513438456 in the stress run. Either the function does not do what its name says or the docs omit its definition; I cannot tell which from the docs.
2. `rows(T, N, min K)` (N trading bars) is in the builtin table of semantic-model.md section 4 and in the README's "executor features" paragraph, not on the README syntax page. It worked as documented (verified below).
3. Accounting is in semantic-model.md section 6 ("With compounding off (the default) the book is a fixed base of the starting capital: target_weight is a fraction of it, leverage is gross exposure against it"). The run output shows it only as `accounting: fixed base 1000000.00`. The leverage check compares gross with current equity, so under the fixed base a weight of 1 becomes more than 1x equity after losses. The program cannot say "fraction of equity"; that depends on the run flag `--compounding on`.
4. The coverage report is relation-level only. `decide` had 801 tuples and nothing was flagged, but rule `decide#2` (the cap branch) never fired at the base target. Per-rule counts are only in the decisions list (first 20 printed) or the dump's `rule` column.
5. `--ledger` refuses relations with `+` arguments. The usage line does not say so; the error does. observability.md says such relations are queried at one bar with `--inputs`, which gives one bar, not a series. For the series I used `--dump` and recomputed.

## 3. Constructs guessed at, or that did not exist
- Existed and checked: `rows`, `sqrt`, `log`, `std` (sample), `prev`, `least` (accepted by the checker, but it did not behave as min), `Count`-typed params inside `rows` and `min K`, literals 0 and 1 (W5-exempt). `greatest` was not used.
- Guessed: that `least` is min by its name. Wrong.
- Not used: `not trend_up(A, T)` for the flat rule. `close` is not complete (see `show`), so WF-5 would reject it; `trend_dn` is written as its own positive rule.
- Not checked: out-of-range `--param` values (I used 0.20, the top of the range).

## 4. Spec rules I am unsure I implemented faithfully
1. "200-day moving average": I used 200 trading bars, not `200d` (200 calendar days, about 140 bars). The synthetic bars are every weekday (1000 bars = all weekdays 2022-01-03 to 2025-10-31), so 200 bars is about 9.5 months.
2. Realized vol: the window is not specified. I used 60 daily log returns with sample std. The ex-post vol of the position while long is 10.25% (capital basis, 433 bars), not exactly 10%.
3. Annualization: sqrt(252), the kernel's default periods-per-year, so that the metrics agree. The data has about 261 bars a year (1000 bars over 3.8 years), so true annual vol is about 2% higher than the 252 figure.
4. "Single index tracker": the rules range over `universe(A, T)` and do not name SPY (a `"SPY"` literal would warn W6 as a snapshot). With `--symbols SPY` this is one instrument. With more symbols each would be sized on its own and the weights would sum past 1.
5. Accounting (default fixed base, the TASK convention): the position is about 10% of starting capital. With `--compounding on` the same decisions give about 10% of equity and never exceed 1x (max leverage 0.767). I kept the default in `run.sh` because the TASK fixes the run convention; the compounding variant is reported below.
6. Daily rebalancing with no no-trade band: "daily decisions" implies it, and I did not add a band. This drives the cost result (turnover 24.7M, about 25x starting capital).

## 5. What each inspection command told me (quoted lines that changed what I did)
- `bin/abt` (usage): listed `--param`, `--dump`, `--compounding`, `--on-leverage`, `--frictionless`. Used `--dump` for the independent check.
- `check attempts/1.dsl`: `error [M] vol_targeted_trend at 24:27 in rule vol_targeted_trend::trend_up#1: `+A` of `close` is an input and must be bound before the call, but `A` is unbound here (WF-2 modes)`. Changed: every rule now starts from `universe(A, T)`.
- `check attempts/2.dsl`: `vol_targeted_trend: degrees of freedom: 4 params, 0 literals, 7 rules`, `0 error(s), 0 warning(s)`. Clean, and silent on the cap.
- `run --ledger ma,ann_vol,trend_up,trend_dn,decide`: `--ledger: ma takes the inputs A; a ledger is written for relations without + arguments`. Changed: ledger only `trend_up,trend_dn,decide`; `ma` and `ann_vol` checked by dump.
- `run` (attempt 2) coverage: `decide 1000 calls 801 with tuples 801 tuples`, `ann_vol 435 calls 435 with tuples`, `trend_dn 1000 calls 366 with tuples`, no EMPTY line. Confirmed `ann_vol` is demanded only on the 435 up bars and that the 200-bar warm-up gives 801 decisions.
- `show` (attempt 2): `decide ... reads: trend_up+, ann_vol+, trend_dn+`; `ann_vol ... reads: universe+, logret~`; `close(+A: Equity, @T: Timestamp, -P: Price<USD>)  [@1d, not complete: a missing tuple is unknown]`. Confirmed the wiring; the completeness line is why the flat rule is positive, not `not trend_up`.
- `run --param target_vol=0.20` (attempt 2): `run halted: risk policy halted the run at 2022-12-28: target_weight(SPY, 1.2133782513438456) (rule vol_targeted_trend::decide#1): leverage: ... gross exposure 1212163.12 against equity 977472.18 (limit 1x gross)`. Changed: the cap is now explicit (attempt 3).
- `run --param target_vol=0.20 --on-leverage allow` (attempt 3): `exposure: max gross 999999.84   max leverage 2.455`, `funding: ... margin interest 19137.87`. Showed the cap holds per decision (`decide#2` at W = 1 on 386 bars) but 1x starting capital reaches 2.46x equity. I set `allow` only to finish the run; decisions are open-loop (show), so they do not depend on the executor policy. I did not run the default `halt` on attempt 3; the halt on attempt 2 used the same gross-versus-equity check.
- `query --rel decide --at 2023-02-24 --explain`: `rule vol_targeted_trend::decide#1 fired at 2023-02-24 with 1 solution(s); first solution: A = SPY, T = 2023-02-24, W = 0.5801545637428875`; `rule vol_targeted_trend::decide#2 did not fire at 2023-02-24: literal 3 `W > 1` has no solution`. Confirmed the cap branch is dormant at base.
- `query --rel decide --at 2022-10-07 --explain`: `rule ...decide#1 did not fire ...: literal 1 `trend_up(A, T)` has no solution`; `rule ...decide#3 fired at 2022-10-07 with 1 solution(s)`. Confirmed the flat branch.
- `query --rel trend_up --at 2023-02-24 --explain`: `first solution: A = SPY, M = 39.67415, P = 40.19, T = 2023-02-24`. The close is above the average.
- `query --rel vol_weight --at 2023-02-24 --inputs SPY --explain`: `first solution: A = SPY, S = 0.17236785892856982, T = 2023-02-24, W = 0.5801545637428875`. Check: 0.10 / 0.17237 = 0.58015.
- `run --compounding on` and `run --frictionless`: no program change. They attribute the result to costs (section 6).

## 6. Independent check and headline results
Independent check (not an abt command): from the dumped closes, pandas computes the 200-bar mean, the 60-bar sample std of log returns times sqrt(252), and W = min(1, 0.10/vol) on up bars and 0 on down bars.
- Final run: all 801 decisions match, max |difference| 3.3e-16. Uncapped W times vol = 0.1000000000 on all 435 up bars.
- Stress (target 0.20, attempt 3): 386 capped at 1, 49 uncapped, max |difference| 4.4e-16.

Headline results (`run.sh`, final.dsl):
- `strategy vol_targeted_trend over 1000 bars (2022-01-03 to 2025-10-31), 1 symbols`
- decisions: 801 (435 on decide#1, 366 on decide#3, 0 on decide#2)
- fills 451; dropped 1 (2025-10-31, no next bar); trades 18
- `metrics per bar (252 a year, 999 periods): cagr -0.060012   max_drawdown 0.234903   sharpe -0.8356 (population -0.8361)   volatility 0.0710`
- end equity 782,435.30; total return -21.76%
- costs: commissions 2,926; fees 332; slippage 27,431; impact 239,289; turnover 24,711,242
- exposure: max gross 767,117; max leverage 0.795
- Sensitivity, same decisions: `--compounding on` gives total return -20.30%, Sharpe -0.7740, max leverage 0.767. `--frictionless` gives total return +2.60%, Sharpe 0.1296, max drawdown 11.3%.
- The default costs (about 270k, almost all impact from daily resizing) turn a positive gross result into a loss.

## Process
- `bin/abt` invocations: 19 in total (usage 1, check 4, run 9 incl. one failed `--ledger` and the `run.sh` run, show 1, query 4). The budget was about 30.
- Outside the workspace I ran only `which duckdb parquet-tools` (a PATH lookup, no result used) and Python imports of the installed pyarrow and pandas. I did not read `bin/abt`'s source or any other file outside the workspace.
