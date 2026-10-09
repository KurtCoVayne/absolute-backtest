# MW14 in abt: notes

## Files
- `final.dsl`: the strategy `mw14` (identical to `attempts/2.dsl`). `abt check` on it: 0 errors, 0 warnings, "20 params, 0 literals, 41 rules".
- `run.sh`: the full backtest under the run conventions.
- `attempts/1.dsl`: first draft, 8 errors (7 WF-2 mode errors, 1 WF-3 type error) and 2 W4 warnings.
- `attempts/2.dsl`: the draft with entity relations declared `-A` and the crowding count rewritten as a guarded division. Checks clean.
- `commands.log` also sits in the workspace; I did not create or read it.
- The data directory was not read. The strategy was written from docs/, env/ and lib/ and checked with abt.

## Headline result (`run.sh`)
- Decisions 1057: 468 new-name buys (decide#1), 137 band rebalances (decide#2), 452 sells (decide#3). 229 of 468 weeks had a decision. First decision 2016-01-15: five names at 0.15 (N = 5, capped).
- Fills 1059, two more than decisions (presumably the two delisting closes; the run reports 2 delistings). Metrics line: trades 454, win rate 0.507.
- Metrics (467 weekly periods from 2016-01-01): cagr 0.144655, max_drawdown 0.103911, sharpe 1.4425 (population 1.4441), volatility 0.0969.
- end_equity 3,364,667.49 (total_return 2.3647) on the fixed 1,000,000 base. Commissions 93,262.65 on turnover 93,438,714.62 (10 bps per side). Fees, slippage and impact are zero. Max gross 1,238,857.81, max leverage 1.118.
- Observed target weights: 1/N for N = 7 to 17, and the 0.15 cap. On 2016-01-29 all five names were sold, when panic switched on.

## Verification
- `--window-sums exact` gives identical decisions and metrics to the default.
- `--verify-causality` passes at 5 sampled bars.
- Crowding branch: the spec's 25% test never fires on this data, so the dial is inactive in the reported run. Forcing the test with `--param crowd_frac=0` (a diagnostic, not the strategy) produces 0.70/N weights in 28 week-weight pairs and changes cagr to 0.133077. So the branch works.
- `abt explain`: panic fires on 2016-01-29 and not on 2016-01-22 (gate holds). `latched` fires for E036 on 2016-01-22, its buy week being 2016-01-15, and not for E002, whose first buy is 2016-02-26.
- Not checked against the raw data, and no hand reference implementation was written.

## How the spec maps to the strategy
- The time domain is `week(T) :- universe(_, T)`. Market series use `series(T, "SPXTR", V)` with string labels.
- Every window is over the stock's own rows: `rows(T, N, min N)` with `universe(A, T1)` (or the series atom) as the first conjunct.
- Split adjustment: `cumlog` = sum over own rows of log(split factor), with 0 for rows without a split. `adjpx` = close x exp(cumlog).
- Score: y = log(adjpx / 1 USD/share), x = own-row count `rowno`. b = `ols_beta(Y, X)`, R = `corr(Y, X)`, score = (exp(ann*b) - 1) * R * R. The origin of x and the constant offset in y leave b and R unchanged.
- Ranks: `rank(..., by (S desc, A asc), as K)` for scores; `by (D asc), ties average` for liquidity. T is bound before each rank so the group is one week.
- Latch, without recursion: the language has no previous-own-row builtin. I used the equivalent form latched(A,T) iff may_stay(A,T) and some own row E <= T has enter(A,E) and exits(A,E) = exits(A,T), where exits(A,T) counts own rows up to T that may not stay. Entering implies may_stay, so the two exit counts agree exactly when no non-stay row falls between E and T. Induction on the spec's recursion gives the same set.
- Crowding: cpop counts latched names with a 40-row mean; cnum counts those with adjpx > 1.5 x mean. Crowded iff cpop > 0 and cnum/cpop > 0.25.
- Scale: 0 if panic, else 0.70 if crowded, else 1.
- Targets: tw = least(scale/N, 0.15) for latched and eligible names, kept if above 1e-6, with N the count of latched and eligible names.
- Band: a name not held is bought iff target >= 0.025 - 1e-12. A held name with a target is re-traded iff |target - current| >= 0.025 - 1e-12, otherwise no decision is emitted. A held name with no target is sold to 0. Current weight = position x trclose / 1,000,000.
- All constants are parameters (20 of them). Only 0 and 1 appear in rules, which are exempt from W5.

## What was hard
1. The latch on own rows. prev and lag step calendar bars, and WF-4 recursion must go through them, so "latched at its own previous row" cannot be written directly. I used the exit-count reformulation above.
2. Windowed aggregates below `min K` yield no tuple, even for count and sum. A count of rows that may be zero therefore has no value. I summed a 0/1 indicator over all own rows so the window always has at least one observation. `min 0` is untested.
3. Modes. Any relation enumerated over all stocks, such as `count(A) over (elig(A, T))`, must be declared `-A`. With `+A` the checker raised WF-2 errors, and a `+A` head made `rank` degenerate (W4). This took a full round.
4. Count and Scalar. Count cannot multiply a Scalar (`crowd_frac * P` was rejected). Division is the bridge: `F = C / P` with a `P > 0` guard, since x/0 halts the run, and `X = C / 1` for row numbers.
5. No product aggregate. Cumulative split factors are exp(sum of log factors). `log(P)` is rejected for a price, so `log(P / 1 USD/share)` is used; the offset does not affect b or R.
6. No calendar primitive for weekly bars. `week(T) :- universe(_, T)` supplies the domain.
7. `abt explain` runs the full backtest first. Without `--on-leverage allow` it halted at 2016-02-26 on leverage and reported that instead of the rule.

## Docs gaps and errors
- The `+`/`-` rule is stated, but nothing says an enumerating aggregate needs `-` on the relation. The fix surfaced only in the checker output.
- Section 2 says Count converts to Scalar only through division. The working forms (`C / P`, `C / 1`) are not shown.
- Section 4 says windowed groups below `min K` yield no tuple. It does not say what this means for zero counts, and `min 0` is not discussed.
- No own-row previous-element builtin is documented. The WF-4 recursion examples go through calendar prev/lag only.
- `rows` appears in the temporal-builtins table and in one README sentence, with no example. The rule that the group's rows are those where the conjunction's first atom holds is the key detail, and it is only in the table text.
- `--window-sums` and `--verify-causality` appear only in the usage text, not in the README.
- No outright errors found. The run covers 468 weekly bars, 2016-01-01 to 2024-12-13.

## Constructs that did not exist or were guessed
- Not in the language as documented: a previous-own-row builtin, a product aggregate, `first`/`last` outside resample. Not tested: `min 0`.
- Accepted but not shown in the docs: `C / 1` as a Count-to-Scalar conversion. String labels in atoms (`member(A, T, "SP1500")`) worked as the docs describe.
- `rows(T, 1000, min 1)` is used as "all history". Own history is at most 520 rows, so this is exact here.

## Spec rules I am not sure I implemented faithfully
1. Split adjustment: the product covers the stock's own rows up to and including the current row. Split factors in weeks the stock is not in the universe are ignored. The formula is taken literally (raw close x cumulative product); the data's factor convention is unverified.
2. The windows of 4, 18 and 40 rows include the current row. "Last 52 weeks" is the last 52 SPXTR observations, including this week.
3. Liquidity n counts all eligible names, including those without a score. Ties take average ranks.
4. Crowding population: latched names in the universe with a 40-row mean, including latched names not eligible this week (the spec says "in the universe"). The 25% test is strict, and an empty population is not crowded.
5. Panic overrides the crowding dial (scale 0).
6. Band: no decision is emitted when the band is not hit (rather than a target equal to the current weight). "Held" means position > 0 shares. The dust filter applies to targets only.
7. Sells: a held name that is not a target is sold, even if it is not in the universe this week. The executor settles it.
8. Current weight uses the position before this week's fills (moc fills at this week's close) and trclose at this week.
