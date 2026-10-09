# MW14 in abt: notes

## Deliverables and status

- `final.dsl` (identical to `attempts/1.dsl`): checks clean (`0 error(s), 0 warning(s)`; `20 params, 0 literals, 35 rules`).
- `run.sh`: the full backtest per the run conventions. Output saved in `run_final.txt`; exit 0.
- `attempts/`: 1 = the strategy; 2 = audit diagnostics, failed its check (an unbound `+A`); 3 = attempt 2 fixed; 4 = attempt 3 plus membership and market-series diagnostics. Diagnostic relations (`dg_*`) do not feed `decide`.
- `ledger3/`, `ledger4/`: ledgers from attempts 3 and 4. `audit/`: pandas scripts that recompute the chain from those ledgers. `run_sens_crowd01.txt`: sensitivity run (see below).
- `bin/abt` invocations: 22 in total (usage 1; checks 5; runs 7, including 2 that failed because of my own mistakes: a `--ledger` on a `+A` relation and a run without `env lib`; `show` 1; `query` 8). The sensitivity run is one of the 7 runs.

## Headline results (final run, `run.sh`)

- `decisions: 1057   fills: 1059   dropped: 0` (1057 decisions = 137 band rebalances of held names + 468 new buys + 452 sells; the 2 extra fills are delisting closes).
- `metrics per bar (52 a year, 467 periods): cagr 0.144655   max_drawdown 0.103911   sharpe 1.4425 (population 1.4441)   volatility 0.0969`
- `additive: annual_return 0.139882   max_drawdown 0.107994   total_pnl 1256250.58   profit_factor 1.891 per period, 1.770 per trade   trades 454   win_rate 0.507`
- `costs: commissions 93262.65 ... turnover 93438714.62`; `exposure: max gross 1238857.81   max leverage 1.118`; `end_equity: 3364667.4857`.
- These numbers carry a data artifact at splits (below). About $0.29M of the $1.26M additive P&L comes from it.

## The data problem: trclose is not continuous across splits (most important)

- Every one of the 52 split events in the data is a 2-for-1 (`F = 2.0`), all on universe rows. At each split week the raw `close` halves, as it should. The split-adjusted close (`close` times the cumulative split factor) is continuous: median weekly ratio 1.0042. The total-return index `trclose` doubles: median ratio 2.0084. On non-split weeks `trclose` and the split-adjusted close move together (|difference| <= 0.5%). So `trclose` appears to apply the split factor a second time.
- The spec says the total-return index already carries splits (`--actions in-prices`), and the executor does not adjust share counts. So any holding through a split gains its full value at that week. In 2016–2024 four held names crossed a split: E041 (2021-12-24), E023 (2022-06-17), E029 (2024-05-10), E043 (2024-11-29). Their total artifact is $290,768, about 23% of the additive total P&L.
- The jump also reaches the trading band: the held weight doubles, so each of those four weeks has a band sell (`decide#1`, target below current weight in all four).
- I did not change the price, because the spec says to trade and mark at `trclose`. The headline numbers therefore include the artifact. A corrected backtest needs either a continuous total-return series or `--price-relation close --actions apply`, with the band also using the raw close. I did not run that.

## What was hard

- No library fits `equities_1w`: `features` and `metrics` target `equities_1d`, `catalog` targets `equities_1d_v2`, and `bars` and `features_m` target `equities_1m`. Everything is in the strategy file (157 lines, 20 params).
- Own-row windows: `T1 in rows(T, N, min N)` with `universe(A, T1)` as the first atom. The rank literal groups by the bound time, so I bind `T` through `wk(T)` before each `rank(...)`.
- Previous own row for the latch: `prev(T, T1), universe(A, T0) asof T1, latched(A, T0)`.
- Count versus Scalar. `count` returns Count, and I needed Scalar row numbers, so the row number is `sum(1.0)` over a 100-year window minus 1. Count/Count division (`NC / NL`, `S / N`) was accepted.
- No product aggregate. The cumulative split factor is `exp(sum(log F))` over a 100-year window; it matches the direct product to 1e-16 in the audit.
- `--ledger` refuses relations with `+` arguments (`padj`, `m40`, `score`, ...). Per-stock series needed `-A` wrappers.

## What the docs lacked or got wrong

1. The semantic model's table (section 6) gives `position` as `(+A: Equity, ...)`. `abt show` and the checker report `position(-A: Equity, @T: Timestamp, -Q: ...)`, and `held(A, T, Q) :- position(A, T, Q)` checks with A unbound.
2. WF-4 says recursion must step back "through `prev` or `lag`". The checker accepted the latch recursion whose step is an as-of join (`universe(A, T0) asof T1`). I expected a rejection. The text should say whether an as-of key counts as a step.
3. `README.md` cites `docs/data-bundle.md` (section 6, degrees of freedom), and `observability.md` cites `docs/language-v2.md` (section 8). Neither file is in the workspace.
4. `observability.md` says a relation with `+` arguments is queried with `--inputs`, but the `--ledger` usage text does not mention the restriction. I found it by trial (one wasted invocation). The doc also does not say that per-stock time series therefore need a `-A` wrapper relation, which I had to write.
5. `query --symbol E036 --rel decide --at 2016-01-15 --explain` printed `decide at 2016-01-15 (@1d): 0 tuple(s) for E036`, while the same output said `rule mw14::decide#2 fired ... first solution: A = E036, T = 2016-01-15, W = 0.15`, and the run listed `2016-01-15 target_weight(E036, 0.15, moc) (rule mw14::decide#2)`. `--symbol` does not look inside decision constructors. Nothing in the docs says so.
6. The run prints `warning (transaction-cost neglect): commissions and fees are zero`, but the costs line in the same run reads `commissions 93262.65`. The warning appears to be generic under `--frictionless`. Commissions are about 10 bp of traded notional: 93.26M in total, versus turnover 93.44M, the gap being the two delisting closes, which carry no cost under `last-price`. The second part is my inference.
7. `--explain` names the first failing literal (`literal 5 (NC / NL) > crowd_frac has no solution`) but not the values of its variables. The crowding rule looked like a bug until a ledger showed the largest fraction is 20%.
8. The TASK's run conventions say "fractional shares" but do not list `--lot fractional`. The usage says the default is whole shares. I added the flag.
9. The rank grouping rule (what counts as "the group") appears only in the semantic model's section 4 table. The README has no rank example.

## Constructs I guessed at (all accepted by the checker)

- `sum(1.0)` as a Scalar counter; `exp(sum(log(F)))` as a cumulative product.
- The previous own row through `universe(A, T0) asof T1` (see WF-4 above).
- Parameters as window durations and row counts: `window(T, hist, min 1)` with `hist : Duration`, `rows(T, n_score, min n_score)` with `Count` parameters.
- `1e-12` as a Scalar literal; `1 USD/share` for the log reference; `"SP1500"`, `"SPXTR"`, `"VIX_MA4"` as Label literals; a parameter as a rule-head value (`dial(T, crowd_scale)`).

## Spec rules I am not sure I implemented exactly

1. "Cumulative product of the split factors over the stock's own weeks so far" includes the current week's factor. That is the only reading that makes the split-adjusted close continuous, since the raw close halves at each split.
2. The latch's "own previous row" is an as-of join on `universe`. In this data every stock's consecutive universe rows are exactly 7 days apart, so it equals `prev(T)` here. The gap case is untested.
3. Row number is 0-based. The score does not depend on the offset.
4. "Last 52 weeks" is 52 SPXTR values including the current week. The series is complete (520 weeks, 3 series).
5. Liquidity ranks all eligible names (ties averaged). Score ranks only eligible names with a score. Both are read literally.
6. "Held" means position > 0 (no executor position was negative).
7. The crowding dial never fires at the book's 25% threshold. The most any week reached is 2 of 10 latched names (20%) at 2018-10-05. The 0.70 branch is therefore not exercised by the book itself. A sensitivity run with `--param crowd_frac=0.1` fires `crowded` in 4 non-panic weeks and changes decisions from 1057 to 1076. That run is sensitivity only, not the book.
8. Decisions at T see `position(A, T)`, which reflects the previous week's fills (the semantic model's execution contract).
9. The current weight in the band uses `trclose` as the spec says, so it inherits the split artifact.

## What each inspection command told me (quoted lines that changed my work)

- `bin/abt` (usage): `--margin-rate X (default 0.05)` made `--margin-rate 0` necessary; `--lot` defaults to whole shares; `--actions in-prices` and `--start` semantics confirmed.
- `check attempts/1.dsl`: `mw14: degrees of freedom: 20 params, 0 literals, 35 rules` and `0 error(s), 0 warning(s)`. The first attempt checked clean, so the work moved to running it.
- Run 1 coverage: `crowded   374 calls   0 with tuples   0 tuples   EMPTY: demanded, never derived`. This changed the work: I audited the crowding rule with ledgers (below). The rule is right and the data never reaches 25%.
- Run 1 `--ledger` on `padj`: `--ledger: padj takes the inputs A; a ledger is written for relations without + arguments`. This changed the work: diagnostic `-A` wrappers in attempts 2 to 4.
- `check attempts/2.dsl`: `error [M] ... rule mw14::dg_rowno#1: +A of rowno is an input and must be bound before the call`. Fixed in attempt 3 by enumerating `universe(A, T)` first.
- Ledger audit (attempts 3 and 4, 2016–2024): 195 universe rows have a price more than 1.5 times their 40-row mean (maximum 2.03), but the weekly count fraction peaks at 0.2 over the full history (NL = 10, NC = 2 at 2018-10-05); `crowded` never fires. Also the split audit above: `median trclose jump ratio at split (tr / tr_prev): 2.0084   median padj jump ratio: 1.0042`. This changed the work: the artifact was quantified and reported. Price left unchanged.
- Pandas replication of the chain from ledgers (all matches): adjusted close, 40-row means and 4-row ADV agree to 1e-15; scores to 1e-10. Eligible names (18,288 rows), score ranks (17,755), liquidity ranks, `liquid` (11,197), gate (296 weeks), panic (123), may-stay (18,410), enter (1,338), latched (5,376), NL and NC per week (520 weeks), dial (520 weeks), target weights (4,405), and decision counts 137 + 468 + 452 = 1,057. Nothing in the strategy changed as a result.
- `show --strategy mw14`: `35 relations reachable from decide (7 primitive, 1 executor, 0 kernel, 26 derived, 1 output), 35 rules`, with no dead-rule warning. `latched ... recursive (reads its own past), open loop`. It also shows `position(-A: ...)`, which contradicts the semantic model (point 1 above).
- `query --rel decide --at 2016-01-15 --symbol E036 --explain`: `rule mw14::decide#2 fired at 2016-01-15 with 5 solution(s); first solution: A = E036, T = 2016-01-15, W = 0.15`. This confirms a new name is bought at min(1/5, 0.15). The zero-tuple `--symbol` result is point 5 above.
- `query --rel decide --at 2016-01-29 --explain` (no symbol): five `target_weight(E0xx, 0, moc)` tuples, `decide#3 fired ... first solution: A = E036`. Sells all held names because no target survives.
- `query --rel calm --at 2016-01-29 --explain`: `calm#1 did not fire ... literal 1 gate(T) has no solution`; `calm#2 did not fire ... literal 1 vixcalm(T) has no solution`. With `query --rel panic --at 2016-01-29`: `panic#1 fired`. The gate is off and VIX is not calm, so the dial goes to 0.
- `query --rel crowded --at 2018-10-05 --explain`: `rule mw14::crowded#1 did not fire at 2018-10-05: literal 5 (NC / NL) > crowd_frac has no solution`. That is 2 of 10 = 20%, below 25%.
- `query --rel latched --at 2016-01-29 --symbol E036 --explain`: `latched#1 did not fire ... literal 1 enter(A, T) has no solution`; `latched#2 fired ... first solution: A = E036, T = 2016-01-29, T0 = 2016-01-22, T1 = 2016-01-22`. The latch carries from the previous own row during a panic week.
- `query --rel enter --at 2016-01-15 --symbol E036 --explain`: `enter#1 fired ... first solution: A = E036, RS = 7`.
- `query --rel liquid --at 2016-01-15 --symbol E036 --explain`: `first solution: A = E036, LR = 34, NE = 38`. So 34/38 x 10 = 8.9 > 4, as the rule requires.
- Sensitivity run (`--param crowd_frac=0.1`): `crowded   374 calls   4 with tuples   4 tuples`, `decisions: 1076`. The 0.70 branch reaches the trades.
