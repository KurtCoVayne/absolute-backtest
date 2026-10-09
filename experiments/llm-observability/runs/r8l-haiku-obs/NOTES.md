# R8L in abt: notes

## Result

- `final.dsl` (identical to `attempts/2.dsl`) checks clean: `r8l: degrees of freedom: 4 params, 0 literals, 6 rules (+ 6 library literals)` and `0 error(s), 0 warning(s)`.
- Full run (`run.sh`): 243 decisions (main long 113: decide#1 112, decide#2 1; main short 105: decide#3 103, decide#4 2; sidecar short 25: decide#5), 486 fills, 0 dropped.
- Metrics line: `metrics per day (252 a year, 500 periods): cagr 0.310425   max_drawdown 0.008858   sharpe 6.3684 (population 6.3748)   volatility 0.0426`
- Additive, fixed base: total_pnl 538,488.71 (final cash 1,538,488.71), annual_return 0.271398, trades 243, win_rate 0.798, profit_factor 9.474 per period / 6.806 per trade.
- Costs: commissions 71,400.33 (exactly the sum of |contracts| times the security table's commission per contract), fees, slippage and impact 0.
- Compounded (the metrics' convention): end_equity 1,710,316.10, total_return 0.7103. Not the fixed-base return; see the docs section.
- Exposure: max gross 1,379,851 (max leverage 1.016).
- Coverage: decide 223 of 7,546 decision bars with tuples; ctx 2,274 of 3,000 calls; atr 2,874; mmed 2,274. No EMPTY relation, no "never demanded" line.

## Files

- `attempts/1.dsl`: first version, latefrac written `L * late_den <= 1`. Checks clean, runs with identical results.
- `attempts/2.dsl`: latefrac written `L <= 1 / late_den` (the spec's form, 1/3 as a float). Checks clean, identical results. Final.
- `final.dsl`, `run.sh`: final strategy and full-backtest command.
- `verify.py`: independent recomputation of the rules from the dumped primitives, compared with the strategy's decisions and fills. Run as `python3 verify.py dump2`.
- `dump1/`, `dump2/`: `--dump` output of attempts 1 and 2 (primitives, decisions, fills, nav). Stdout of each run is in `*.stdout.txt`.

## How the strategy is built

- Library `r8l_daily` (@1d, same file as the strategy): `pclose` (the root's previous close), `tr` (true range), `atr` (mean of the last 20 true ranges, all 20 required), `atr_p` (previous session's ATR), `o0_d` and `c30_d` (minute prints resampled to the day), `m_d`, `am_d`, `mmed` (median of |m| over the last 250 sessions, min 100 values).
- Strategy `r8l` (@1m, `moo_moc`, delta mode): `ctx` reads, at the decision time, the previous session's daily values by as-of plus an equality check on the key (`session(A, P) asof T`, then `atr(A, K1, ATR) asof T, K1 = P`, and so on), and today's prints by as-of plus a guard that the print is after the session open (`open0_m(A, T1, O0) asof T, S < T1`). Five decide rules: main long with |g| < 1; main long with g of at least one ATR and the same sign as m; the two short analogues; and the sidecar short. Size `risk / (ATR * multiplier)`.
- Spec constants are params: `risk` (6493.912071090746 USD), `k_main` (2.0), `gap_side` (-0.75), `late_den` (3.0). 

## What was hard

1. No per-root previous session. `prev` is over the global time domain of the @1d primitives. The root's previous session is written as `C = sum(P) over (T1 in rows(T, 2, min 1), session(A, T1), close_d(A, T1, P), T1 < T)`: the window holds today and the previous session, the filter keeps the previous one, and `sum` over that single row picks its value. I found this by reading the semantic model, not from the one-page syntax.
2. Daily features must live in a @1d unit: WF-10 forbids @1d atoms in a @1m rule. The strategy reads them as of the decision time, and `asof` means "latest key at or before T". If the previous session lacks a value, `asof` returns an older session without any error. Every daily read therefore needs an equality check on the key (`K1 = P`) to keep the spec's "missing, not carried over". The checker does not flag the missing check; it only changes the answer silently.
3. Minute prints need the same care: without `S < T1`, a session that did not trade minute 0 would take the previous session's print.
4. Division: `x / 0` halts the run when demanded. The sign test (`M > 0` or `M < 0`) must come before the latefrac division, and `ATR > 0` before any division by ATR.
5. The gap condition `|g| < 1 or (g and m have the same sign)` has no OR in bodies. It became two rules per direction, partitioning the spec's cases exactly: |g| < 1, and |g| >= 1 with the sign of m.
6. latefrac <= 1/3 cannot be written as a finite decimal. `L <= 1 / late_den` with `late_den = 3.0` gives the same float as 1/3.
7. Literals inside a strategy rule are W5 warnings, so the spec constants became params. The window lengths (20, 250, 100) and the `rows` counts are literals in the library, which are counted but not warned.
8. `--ledger` cannot write relations with `+` arguments. I tried it on the ledger of all intermediate relations; the run stopped at `close_d` after writing only `session.parquet`. I used `--dump` instead and recomputed the rules in Python from the dumped primitives.

## Docs: what was missing or wrong

- `docs/observability.md` refers to `docs/language-v2.md` section 8, which is not in the workspace. `docs/semantic-model.md` refers to `data-bundle.md` and `corpus/`, also absent.
- The one-page syntax in `docs/README.md` omits `rows(...)`, `greatest`, `abs` and `median`. `rows` appears in `semantic-model.md` section 4 and in one line of the README's executor section; `rank` appears only in that last section; the function list (`abs`, `least`, `greatest`, `log`, `exp`) is in `semantic-model.md` section 2. I needed `rows` to express "the last 20 sessions", so this mattered.
- `semantic-model.md` section 6 says "a non-positive price in the data is a load error". The futures data has 486 non-positive closes and the run loaded it without error. The data is declared as futures with `asset_class future`, which presumably exempts it, but no doc says so.
- `--frictionless` is described as "every cost and liquidity model off". The security table's per-contract commission was still charged (71,400.33, which equals the sum of |contracts| times the per-contract commission exactly). The same run printed `warning (transaction-cost neglect): commissions and fees are zero`, which is wrong for this data.
- TASK.md says "fractional contracts" but gives no flag. The default is whole shares, so `--lot fractional` is required; otherwise contracts would be truncated without any warning. I used it.
- The metrics mix conventions in one block. `total_return 0.7103` and `end_equity 1710316.0996` come from compounding the fixed-base daily returns (the semantic model's accounting section says so), while `total_pnl 538488.71` and the final cash `1538488.71` are the fixed-base figures. The run output does not say which is which, and a reader would take 71% as the return of a book with no compounding.
- The order types section shows `target_weight(A, W, moc)`. The delta form `buy(A, Q, moo_moc)` worked, and its fills are the open of the entry print and the close of the session (checked for all 243 decisions).
- `query --symbol` on `decide` prints `decide at ... (@1m): 0 tuple(s) for ZN` while the rule lines below say `fired ... with 1 solution(s)`. `decide` has no entity column (A sits inside the decision value), so the symbol filter removes every tuple. The rule lines are correct; the header is misleading.
- Nothing distinguishes a spec constant from a tunable parameter. The W5 advice "lift it into a param" makes spec constants sweepable, which the study would then treat as degrees of freedom.

## Constructs that worked but I guessed at

- A library and a strategy in one file. The checker counted "6 libraries" (the five in `lib/` plus mine), so it was picked up. The docs do not say this is allowed.
- `buy(A, Q, moo_moc)`: a trailing order type on a delta constructor.
- `greatest(x, y)` with two arguments, `abs`, `median(X) over (...)`, `sum(...)` as a one-row pick.
- `rows(T, N, min K)` inside `sum`, `median` and `mean`.
- Negative and decimal literals in params (`-0.75`, `2.0`), and `0 USD/share`.
- `session(A, P) asof T` as the root's previous session. Confirmed by `query --explain`: P = 2023-06-20 on a decision at 2023-06-21T09:00.

## Spec rules I am unsure I implemented faithfully

1. The m_med window at the start of history (the biggest interpretive choice). I read "the root's last 250 sessions (at least 100 sessions with a value)" as the last min(250, available) sessions, with at least 100 values. Under that reading 83 of the 243 decisions use a window shorter than 250 sessions (all within each root's first 250 sessions). The strict reading (full 250 sessions) would keep 160 decisions. From this run's fills, those 160 trades have P&L 339,048.69 (win rate 0.775), and the other 83 have 199,440.03 (win rate 0.843). This is the subset of the same trades, not a separate backtest; it is exact because each leg is flat by its session close and no feature depends on another decision.
2. "An ATR <= 0 means no trade": implemented as the guard `ATR > 0` on the decision, and m is missing when the previous ATR is not positive. Not exercised: all 2,880 ATR values in this data are positive.
3. Zero values: m = 0 satisfies |m| < 2 m_med, so it goes to the sidecar, as the inequality says. latefrac is computed only after the sign test, so a zero denominator never reaches the division. I did not count exact zeros.
4. The multiplier is read as of the previous session. The data's multiplier equals the security table's value for every root, so this changes nothing here.
5. "The root's own previous session" is implemented per root, but all six roots share one set of 500 session dates, so the global `prev` would have given the same answer. The root-specific construction is not tested against a divergent calendar.
6. Sizing uses the previous session's ATR, the same ATR as in m. The spec's sizing line says only "ATR".
7. The spec constants are params with spec values (risk, 2.0, -0.75, 3.0). A study that sweeps params would move them away from the spec.

Checked by the independent recomputation (`verify.py` over `dump2`): 243 of 243 decisions match on root, time, side and size (relative tolerance 1e-6); the long, short and sidecar counts (113, 105, 25) match the rule counts, but branch identity was not compared decision by decision. Entry fills match the entry print's open (243 of 243), exit fills match the session close (243 of 243). Commissions match exactly, and the cash identity from the fills (quantity times price times the security table's multiplier, less commissions) equals the final cash exactly.

## What each inspection command told me

- `bin/abt check`: clean on the first attempt. Turning spec constants into params removed every W5 warning, so the strategy shows 0 in-rule literals. No change was needed.
- `bin/abt show`: `20 relations reachable from decide (9 primitive, 0 executor, 0 kernel, 10 derived, 1 output), 15 rules, depth 9`, and `ctx` reads `clock+, session+, atr+, close_d+, mmed+, multiplier+, open0_m+, close30_m+, close20_m+`. Confirmed every daily feature is connected and the decision is open loop (it does not read the book). No change.
- Coverage report (attempt 1): `decide 7546 calls 223 with tuples 243 tuples`, `ctx 3000 calls 2274 with tuples`, `atr 2994 calls 2874 with tuples`, `mmed 2994 calls 2274 with tuples`, and no EMPTY line. The first decision for each root falls on session index 122 to 141, so the 100-value requirement delays trading. This led to the window question in unsure item 1.
- `query --rel ctx --at 2023-06-21T09:00:00 --symbol ES --explain`: the first solution has `P = 2023-06-20`, `K1 = K2 = K3 = K4 = 2023-06-20`, `T1 = 2023-06-21T08:31:00`, `T2 = 2023-06-21T09:00:00`, `T3 = 2023-06-21T08:50:00`. This showed the as-of reads bind the previous session and never today's, and the decision reproduces by hand: M = -0.5428, 2 x m_med = 0.5381, |g| = 0.505, latefrac = 0.187, size 6493.91 / (72.54 x 50) = 1.7904.
- `run ... --ledger ...` (attempt 1): `--ledger: close_d takes the inputs A; a ledger is written for relations without + arguments`. The observability doc says this; I missed it. Changed the plan: I used `--dump`.
- `run ... --dump` (attempts 1 and 2): gave the primitives, decisions and fills as Parquet. Used for `verify.py`, which matched all 243 decisions. This also showed that the executor multiplies by the security table's multiplier (the cash identity matches exactly).
- `query --rel decide --at T --symbol X --explain` on four bars, one per branch: decide#1 on ZN 2023-07-10 07:50 (M = 1.117, 2 x m_med = 0.621, |g| = 0.177, latefrac = 0.254, Q = 6.203); decide#2 on GC 2023-12-08 07:50 (G = 1.489 with M = 1.379, latefrac = 0.332, Q = 1.448); decide#4 on 6J 2024-04-17 07:50 (M = -0.775, G = -1.584, latefrac = 0.160, Q = 395.66); decide#5 on ZN 2023-07-04 07:50 (|M| = 0.282 < 0.657, G = -1.150 <= -0.75, Q = 6.191). Each reason matched the spec.
- `query --rel decide --at 2023-06-21T07:50:00 --symbol 6J --explain` (a near miss found by `verify.py`, which listed 329 bars where magnitude and gap pass but latefrac fails): `rule r8l::decide#3 did not fire ...: literal 11 `L <= (1 / late_den)` has no solution`. This is the intended reason (latefrac 0.3391 > 1/3).
- No inspection output showed a bug in the strategy. The first version was already correct on every check I ran. Attempt 2 changes latefrac to the spec's form only.
- My own Python check first reported 0 decisions; the cause was my bug (an index compared against a date-keyed dictionary). Not an abt issue.

## Run conventions (`run.sh`)

`bin/abt run --strategy r8l --data data/sessions --capital 1000000 --compounding off --lot fractional --frictionless --margin-rate 0 --on-leverage allow --on-margin-call allow --price-relation close_m --report-by day --report-calendar calendar --periods-per-year 252 env lib final.dsl`

The run prints `warning (data-snooping): this run is an untracked trial that nothing counts`; I did not log it as a study trial.

## Budget

15 `bin/abt` invocations (usage, 2 checks, 4 runs including the failed ledger and two dumps, 1 `show`, 6 `query`, and the final `run.sh`). The toolchain wrapper appends each invocation to `commands.log` in the workspace; that log has the same 15 entries.

The header comment of `attempts/1.dsl`, `attempts/2.dsl` and `final.dsl` still says "attempt 1" (carried over from the first file). It is a comment only; I did not edit a saved attempt after checking it.
