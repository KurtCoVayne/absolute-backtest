# NOTES: breakout_with_stop (haiku-obs)

## Result

- `final.dsl` (identical to `attempts/3.dsl`) checks clean: 0 errors, 0 warnings, "6 params, 0 literals, 10 rules".
- `run.sh` is the full backtest (12 names, 1000 synthetic bars, default configuration).
- Final run: 28 decisions (14 entries at `target_weight(A, 0.1)`, 14 exits at `target_weight(A, 0)`), 28 fills, 0 dropped.
- Metrics line: `cagr 0.017384  max_drawdown 0.032638  sharpe 0.5380 (population 0.5383)  volatility 0.0330`.
  Also: total_return 0.0707, end_equity 1,070,712.16, total_pnl 70,487.86, trades 14, win_rate 0.500.
  Costs: commissions 108.05, fees 40.82, slippage 12,293.70, impact 6,266.16.
  Exposure: max gross 379,181 (about 3.8 full-size positions), max leverage 0.355. The ten-name cap never bound.

## Attempts

- `attempts/1.dsl`: first version. `check` failed in `held_before` (head time `T` unbound before `prev`). Fixed by anchoring with `bar(T)`.
- `attempts/2.dsl`: checked clean. Its first run gave 30 decisions, and the first entry (2022-11-11) came from a 52-week window that held only about 200 bars, so it was not a full 52-week high. Added the full-history guard.
- `attempts/3.dsl`: checked clean. This is the final strategy. Its header comment still says "attempt 3". `attempts/2.dsl` carries a stale "attempt 1" header comment (cosmetic).
- The cap test and the second seed used `attempts/3.dsl` with `--param` and `--seed` overrides. No new strategy versions were written.

## The rules as implemented

- `breakout(A, T, RV)` (open loop): `universe(A, T)`, `lag(T, lookback, _)` (full 52 weeks of data), `highest(A, T, lookback, min_obs, H)` from `features` (max close over the 364 days before T, excluding T), `close > H`, `volume > vol_mult * vol_avg`, with `RV = V / AV`.
- `vol_avg(A, T, AV)`: mean volume over the 20 sessions before T, `rows(prev(T), avg_days, min avg_days)`.
- `cand`: a breakout in a name that is not `held`.
- `slots(T, S)`: `max_names - count(held at T)`.
- `pick`: `top(S, cand, by (RV desc, A asc))` per bar.
- `held_before`, `trail_hi`: the trailing high is the running max of closes from the first held bar (the fill bar) onward. It is carried forward with `prev`, which is the recursion WF-4 permits.
- Decisions: `pick` gives `target_weight(A, 1 / max_names)`. A held name with `close < (1 - max_drop) * trail_hi` gives `target_weight(A, 0)`.
- Parameters: lookback 52w, min_obs 200, avg_days 20, vol_mult 2.0, max_drop 0.08, max_names 10. There are no literals other than 0 and 1.

## What was hard

- Binding order. `prev(T, ·)` and `lag` need T already bound, so a head time must be anchored by a positive atom (`bar(T)`) before any `prev`. The checker's message names the problem, but the observability doc has no example of it.
- "20-day" means calendar time in this language (`20d` is 20 calendar days, about 14 sessions). To get 20 sessions I used `rows` with `prev(T)`.
- A `min K` count does not guard against a window that starts before the first bar. See "Docs" below.
- Sizing and leverage. Ten slots at 10% of the fixed base deploy the whole base at full occupancy. In a run with two slots at 50% each, the default leverage policy halted the run (see below).
- Ledger limits. `close` and `volume` cannot be ledgered because they take a `+` argument, and `--ledger` cannot be combined with `--dump`. I checked the numbers from `--dump` output instead.
- I did not use the library's `flat` (`not position(A, T, _)`), because I did not verify whether `position` keeps zero-share tuples. I used `not held(A, T, _)` (`Q > 0`) instead.

## Docs: gaps and errors

1. A window's minimum count does not mean a full window. With `min 200` on `highest` (a 364-day window), the first entry came when the window held about 200 bars, roughly ten months of data. `highest` was then a shorter-than-52-week high. I needed `lag(T, N, _)` to require a full year. The semantic model says "every window declares a minimum observation count", and `min K` does guard against gaps inside the data. It does not guard against a window that starts before the first bar.
2. The semantic model describes `rows(T, N, min K)` as "the N latest bars at or before T" in one table row, with no example. The README mentions it in one paragraph. I used `T0 = prev(T)` to exclude today. The checker accepted it, and the independent check (below) matched it.
3. `--symbol` does not match an entity inside a decision constructor. `query --rel decide --at 2023-01-25 --symbol K --explain` printed "decide at 2023-01-25 (@1d): 0 tuple(s) for K", although without `--symbol` the same query printed `decide(2023-01-25, target_weight(K, 0.1))` and rule `decide#1` fired with `A = K`.
4. `--ledger` restrictions. The doc says a ledger covers only relations without `+` arguments, which I missed at first. The message `--ledger runs on the batch kernel and cannot be combined with --dump, --study or --verify-causality` is not in the doc.
5. Leverage wording. The usage text says that under `--compounding off`, "leverage is judged against" the fixed capital. The halt message instead said "against equity 990394.88 (limit 1x gross)". The semantic model also says transaction costs are never leverage, but in the halted run cash went to -45,073 after a fill. I did not isolate whether costs, sizing or the equity-based check caused it.

## Constructs

- Used and accepted: `mode target` with `target_weight(A, W)` (exit at W = 0), `rows(T0, N, min K)` with `T0 = prev(T)`, `lag(T, N, _)` as an existence test for history, `greatest(H0, P)`, `count(A1) over (held(A1, T, _))`, `max_names - C` (Count minus Count) passed to `top`, and `1 / max_names` as the weight.
- Not present or not found: no "full window" builtin, no trading-session duration, no check for windows that reach before the data.
- Not used: `decided`, `nav`, `rank`, `first`, `last`.

## Spec rules I am not sure I implemented faithfully

1. "20-day average": I used the 20 sessions before T and excluded today. Including today, or using 20 calendar days, would change the signals. I did not test either.
2. "52-week high": 364 calendar days, closes only (as the spec says), strictly above every prior close in the window, with a full year of history and at least 200 closes in the window.
3. "Highest close since entry": the trail starts at the fill bar (the first held bar), so the signal bar's close is excluded. It includes today's close.
4. Stop: exit when the close is below 0.92 times the trailing high. Exits and entries fill at the next bar's close (the executor's default market order).
5. "Equal size": 10% of the starting capital per name (fixed base), not 10% of current equity. The positions are equal at entry and drift afterwards.
6. "At most ten names": a name sold at T still uses its slot at T, and the slot frees at T+1. Held names plus entries never exceed the cap, as the independent check confirmed for two slots.
7. Slot competition: candidates are ranked by volume ratio (descending), then ticker. The spec says nothing about this, so it is my choice. The data never exercised it (see below).
8. One position per name: re-entry after an exit is allowed, with no cooldown.

## What each inspection command told me

1. `bin/abt check` (attempt 1): "error [B] ... in rule breakout_with_stop::held_before#1: head variable `T` is unbound", and "error [F] ... temporal key `T0` of `held` is not derived from the head time `T`". Changed: added `bar(T)` before `prev`.
2. `bin/abt show` (attempt 2 and final): "15 relations reachable from decide ... depth 5". `breakout` is "open loop", which is what I wanted: the signal does not read the book, while `pick`, `trail_hi` and the exits do. `trail_hi` is "recursive (reads its own past)". `held_before` is complete, so `not held_before` is legal. No change.
3. `bin/abt run` (attempt 2): "highest 12000 calls 9600 with tuples" and "2022-11-11 target_weight(I, 0.1)". The first entry came from a partial window. Changed: added `lag(T, lookback, _)`. After the change the run gave "highest 8880 calls 8880 with tuples", the first entry was on 2023-01-25, and decisions fell from 30 to 28. The removed pair was the 2022 entry and its exit. The rest of the decisions were unchanged.
4. `bin/abt query --explain` on `breakout` for K on 2023-01-25: "first solution: A = K, AV = 975546.35, H = 870.99, P = 871.32, V = 2362265". I reproduced these numbers independently from the dumped data, so no change.
5. `bin/abt query --explain` on `decide` for K on 2023-02-02: "decide#2 fired ... first solution: A = K, H = 950.95, P = 865.26". That is a drop of 9.0 percent, which confirms the exit. No change.
6. `bin/abt query --explain` on `decide` for K on 2023-01-25, with and without `--symbol` (see Docs, item 3). This was an observability gap, not a strategy change.
7. `--ledger` attempts: "--ledger: `close` takes the inputs A; a ledger is written for relations without `+` arguments", and the `--dump` conflict. Changed: I verified with `--dump` and Python instead of ledgers.
8. `--param max_names=2` with the default leverage policy: "run halted: risk policy halted the run at 2024-12-26: target_weight(E, 0.5) ... leverage: after the fill cash would be -45073.34 and gross exposure 1035468.22 against equity 990394.88 (limit 1x gross)". Changed nothing in the final strategy. The ten-name run peaked at 0.355 leverage, so the policy did not bind there. I used `--on-leverage allow` only for the cap test.
9. `bin/abt query --explain` on `pick` for L on 2025-03-26 with `max_names=2`: "pick#1 did not fire at 2025-03-26 with A = L: literal 3 `top(S, cand(A, T, RV), ...)` has no solution". This confirms the cap blocks a candidate when no slot is free.

## Independent check (outside abt)

I recomputed the rule in Python from the dumped close and volume series (`verify/dump3`, `verify/dump_cap2`, `verify/dump_seed2`), with the position state and the next-bar fill rule. Results:

- Final run: 17 breakouts (matching the coverage line), the same 28 decisions, and fills on the next bar.
- Two slots, leverage allowed (`verify/dump_cap2`): the same 26 decisions. One candidate was blocked (L on 2025-03-26, no free slot).
- Seed 2, two slots (`verify/dump_seed2`): the same 18 decisions. No bar had more candidates than free slots, so the tie-break ranking was not exercised in either seed.

## Headline results

- Final run (`run.sh`): 28 decisions, 28 fills, 0 dropped. Metrics as above.
- Entries: K 2023-01-25, F 2023-02-09, J 2023-04-21, E 2023-07-13, K 2023-09-06, K 2023-10-06, L 2024-05-31, J 2024-07-30, L 2024-10-31, G 2024-12-23, E 2024-12-26, and three more.
- Coverage: `breakout` 17 with tuples, `pick` and `cand` 14 each (one candidate per signal bar), `trail_hi` 182 tuples, no EMPTY relations.
- Budget: 22 `bin/abt` invocations (usage, checks, show, runs, queries, and three ledger runs that failed validation).
