# R8L in abt: notes

## Deliverables and attempts

- `final.dsl`: the strategy (identical to `attempts/3.dsl`). `abt check env lib final.dsl`: 0 errors, 0 warnings, 8 params, 0 in-rule literals, 14 rules.
- `run.sh`: the full backtest under the TASK.md run conventions. Output of the final run is in `runs/final_run.txt`.
- `attempts/1.dsl`: first full version. Failed WF-2 (`dayv`, `lfr` and `prevf` took `A` as an input that the decide rules did not bind).
- `attempts/2.dsl`: same logic with `A` as an output of those three relations. Checks clean; first run.
- `attempts/3.dsl`: restructured so that the sidecar does not depend on the close before minute 20 (the sidecar has no latefrac condition). Results identical to attempt 2 (same decisions and fills). This is the final.
- `attempts/4.dsl` to `attempts/6.dsl` are diagnostic probes, not strategy revisions, kept because the rules say to save every file that was run:
  - 4: decides when the daily close-before-30 equals today's minute print. Result: 0 decisions, so a minute rule's as-of read does not see the daily bar of the day it is in.
  - 5: the same without the equality, but with no exit order. It halted on "ruin" because one-contract positions accumulated (not a logic error in the probe; it is the reason for 6).
  - 6: corrected control with `moo_moc`. Result: 2,994 decisions, so the daily as-of read does find earlier bars. Together with 4 this settles the causality question.
- `runs/`: run outputs and dumps. `analysis/show.py`: a helper that prints dumped parquet. I did not open `data/sessions` or the data copy inside the dumps; I read only run outputs (decisions, fills, nav, config bar labels).

Invocations of `bin/abt`: 17 (usage 1, explain 1, checks 7, runs 8), within the budget.

## How the spec maps onto abt

- The strategy decides at `@1m` (clock). Daily features are `@1d` relations declared inside the strategy (`rel ... @1d`).
- Per root previous session: `session(A, T0) asof prev(T, P)`. `prev` is global over the time domain (all roots), so a root's own previous session needs the as-of join.
- ATR: `rows(T, atr_n, min atr_n)` over `session(A, T1)`, with `trd` (true range) required on every row. This gives "needs all 20".
- m_med: `median(abs(M)) over (T1 in rows(T, mwin, min mmin), session(A, T1), md(A, T1, M))`. A session with no m takes a row but no value; min 100.
- m (`md`): daily close-before-30 and open-of-minute-0 via `resample(... to @1d, min 1, last/first)`, scaled by the previous session's ATR; absent when that ATR is not positive.
- "As of previous session" without carry-forward: `sfeat` bundles the session's features; `prevf` reads it with `asof T` and requires the returned key to equal the previous session's key (`S1 = S0`). A missing previous feature therefore yields no trade, not an older value.
- Intraday prints at T: `dayv` and `lfr` use `asof T` and require the print's label to be after today's open S (`S < T1`), so yesterday's print cannot stand in for a missing one.
- Latefrac: two rules, split on the sign of the denominator (no `!=` in the language). Undefined denominator means no main entry.
- Entries: `decide(T, buy(A, Q, moo_moc))` (main long, m > 0), `decide(T, short(A, Q, moo_moc))` (main short, m < 0), and the same head for the sidecar. Gap alignment is written as `G > -gap_band` for longs and `G < gap_band` for shorts (derived from |g| < 1 or same sign). Sizing `Q = risk / (ATR * MU)` (Notional / Price = Quantity).
- Exit: `moo_moc` flat at the session close; no sell rules.

## What was hard

1. Keyed derived relations need their keys bound before the call (`+A`). The checker's message named the exact atom, so it was quick to fix.
2. Per-root previous session: nothing in the docs gives a per-entity previous bar. The `session asof prev` idiom had to be found by reasoning from the semantics.
3. Carry-forward: an `asof` read returns the latest key, so a missing previous feature silently becomes an older one. Needed an explicit key-equality check on every daily bundle read from a minute rule.
4. No `!=`, no disjunction in bodies, no `sign`. Split rules and derived inequalities instead.
5. The entry-price question (below) could not be settled from the docs.
6. `abt explain` on a rule that fires prints only "fired ... with N solution(s)", with no bindings, so arithmetic could not be checked that way.

## Docs: gaps and things that were wrong or misleading

- `--frictionless` is described as "every cost and liquidity model off". In fact the per-share schedule and fees are zeroed (config: `commission_per_share 0.0`), but the per-contract commission from the security table is still charged (ES fills carry 2.297 per contract, NQ and 6J 2.297, CL and GC 2.41, ZN 1.72). The TASK's run convention depends on that distinction and the usage text does not state it.
- Run output warns "commissions and fees are zero" in the same run that reports commissions of 71,400.33. The warning is misleading.
- `--price-relation` is "the primitive the executor fills at". The docs do not say how a `moo` order gets its open price. With `--price-relation open_m` every moo order is dropped ("moo: ES did not open in the session after ..."). With `close_m` entry fills differ from the close print (see below).
- WF-6's list of causal window forms does not name `rows`, but the checker accepts `T1 in rows(T, N, min K)` as causal. The table in section 4 names it, so this is a doc gap, not a checker bug.
- The `asof` semantics (a daily bar counts only once its day has ended) are documented and correct (probe 4 vs 6). Good.
- `prev` is global over the time domain. Multi-entity "previous bar" needs `asof prev`; this idiom is not shown anywhere.
- Every run prints "warning (data-snooping): untracked trial" unless `--study` is named. Noise for single runs.
- `clock`'s description, "30 minutes after the open ... together with that session's open time", is ambiguous. I did not check whether the clock has open-time rows; no trades can come from them anyway because no prints exist yet.

## Constructs guessed, or workarounds for things that do not exist

Accepted by the checker and used, but not shown in the docs: `rel name(...) @1d` inside a `@1m` strategy; `buy(A, Q, moo_moc)` (trailing order type on a delta constructor); `greatest(a, b)` (assumed binary max); `median(abs(M))` (expression argument to an aggregate); `rows(T, N, min K)` with parameters for N and K; `min 1` in resample; `0 USD/share` as a zero price literal; `_` in an output position of an as-of atom.

Not available, so worked around: `!=`, `or`, `sign()`, unary minus on a parameter (avoided; negative literal `-0.75` works), `first`/`last` outside resample (not needed).

## Rules I am not sure I implemented faithfully

1. Entry price. The spec says the leg enters at the open of the entry print. Under the mandated `--price-relation close_m`, entry fills happen at the entry print (open+31) but are not at its close: the NAV identity below shows that the entry fill differs from the same-bar mark (close_m) on 203 of 204 single-fill entries (only one equal), by a typical minute move (ES about 0.5 points, NQ about 2.2). The most likely explanation is that moo fills at the open print, as documented ("fills at the open of the instrument's next bar"). I did not confirm the exact open values, because that would mean reading the data, which the rules excluded. If the executor instead used the close, results would differ; I did not test that.
2. Implied multipliers. The NAV identity reproduces the executor's P&L exactly with per-root multipliers ES 50, NQ 20, GC 100, CL 1000, ZN 1000, 6J 1250 (integer on all 204 round trips that did not share a bar with another fill). I used the `multiplier` relation as of the previous session; the spec does not give its time key. The exact multiplier match suggests it is constant.
3. m_med with short history. "The root's last 250 sessions" is implemented as the last up to 250 rows with at least 100 values. The spec does not say whether 250 rows must exist.
4. ATR "needs all 20": implemented as the latest 20 sessions of the root all carrying a true range.
5. Undefined cases I chose: latefrac with a zero denominator means no main entry; m = 0 means no main direction; ATR <= 0 means no m for that session.
6. Gap alignment written as G > -1 (longs) and G < 1 (shorts), derived by hand from |g| < 1 or same sign. Checked by reasoning, not by test.
7. `lf_max` is the decimal 0.3333333333333333, not exactly 1/3 (irrelevant except at an exact tie).
8. The daily close `close_d` is taken as the session close used for the previous-close gap and the true range.

## Headline results (final run)

Command: `./run.sh` (= `bin/abt run --strategy r8l --data data/sessions --capital 1000000 --compounding off --lot fractional --price-relation close_m --frictionless --margin-rate 0 --on-leverage allow --on-margin-call allow --report-by day --report-calendar calendar --periods-per-year 252 env lib final.dsl`).

- Decisions: 243 (buy main 113, short main 105, short sidecar 25), at most one per root-session, 0 dropped. By root: ES 44, NQ 47, GC 41, 6J 39, CL 36, ZN 36. First decision: ES 2023-06-21, NQ 06-22, ZN 07-04, GC 07-05, CL 07-13, 6J 07-18.
- Fills: 486 (243 entries at open+31 = 07:51, 08:31 or 09:01; 243 exits at each root's session close: 15:00 ES and NQ, 14:00 ZN and 6J, 13:30 CL, 12:30 GC). Fill quantities are fractional, as required.
- Costs: commissions 71,400.33 (per contract, from the security table). No slippage, fees or impact.
- Fixed-base P&L (additive): total_pnl 538,488.71 (53.8% of $1,000,000), annual return 27.1%, max drawdown 0.89%, win rate 79.8%, profit factor 9.47 per period. Final cash 1,538,488.71, flat at the end. Max gross exposure 1,379,851 (leverage 1.016).
- Metrics line: `metrics per day (252 a year, 500 periods): cagr 0.310425   max_drawdown 0.008858   sharpe 6.3684 (population 6.3748)   volatility 0.0426`.
- The header's end_equity 1,710,316 and total_return 0.7103 come from compounding the fixed-base daily returns, which the TASK excludes. I report the additive figure as the book's result.
- `--verify-causality` on attempt 3: causality verified at 5 sampled bars.

Caveat: the performance is high (Sharpe 6.4, 80% win rate, drawdown under 1%). I found no lookahead (probes 4 and 6 and the executor's causality check), but the synthetic data may embed a strong effect, and the entry-price question above is the largest open item.
