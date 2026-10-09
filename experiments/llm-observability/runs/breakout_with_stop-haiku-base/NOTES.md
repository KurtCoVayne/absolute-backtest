# breakout_with_stop in abt: notes

## Deliverables
- `final.dsl`: the final strategy (identical to `attempts/3.dsl`). Checks clean: 0 errors, 0 warnings; "6 params, 0 literals, 8 rules".
- `run.sh`: the full backtest command from the TASK.md convention.
- `attempts/1.dsl`, `attempts/2.dsl`, `attempts/3.dsl`: every version. Attempt 3 only rewrote the header comments.
- Verification artifacts, not part of the strategy: `verify/sim_check.py` (a Python re-implementation of the rules, run on the bars and decisions of our own `--dump`), `dump-attempt2/`, `dump-attempt2-mn2/`, and the logs.

## The strategy
- Mode `target`. Each entry is `target_weight(A, 1 / max_names)`, which is 0.1 of the fixed base. Each exit is `target_weight(A, 0)`.
- Entry at T: the name is not held; a bar exists at or before T - 52w (`lag`), so a full 52-week history is on record; the close is above the highest close of the prior 52 weeks (`highest` from `features`, at least 240 closes); the volume is above 2 x the mean volume of the 20 bars before T (`rows(prev(T), 20, min 20)`). Candidates are ranked by volume ratio (desc, then ticker), and `top(open_slots, ...)` takes at most the free slots.
- Exit at T: `peak` is the highest close since the first held bar, including T. Sell when `1 - close/peak > 0.08`.
- Timing: a decision at T fills at the close of T+1 (the default market order).

## Headline results (final run)
Command: `./run.sh`, which is `bin/abt run --strategy breakout_with_stop --synthetic --days 1000 --symbols A,B,C,D,E,F,G,H,I,J,K,L env lib final.dsl`.
- Window 2022-01-03 to 2025-10-31 (1000 bars, 12 symbols). Decisions: 28 (14 buys, 14 sells). Fills: 28, dropped 0, fill ratio 1.0. The first buy is decided on 2023-01-25. At most 3 names are held at once.
- Metrics line: `cagr 0.017384  max_drawdown 0.032638  sharpe 0.5380 (population 0.5383)  volatility 0.0330` (252 per year, 999 periods). Additive: total_pnl 70,487.86, total_return 0.0707, end_equity 1,070,712.16, 14 trades, win_rate 0.500, profit factor 1.255 per period (2.250 per trade).
- Costs: commissions 108.05, fees 40.82, slippage 12,293.70, impact 6,266.16. Max gross 379,181 (max leverage 0.355).

## Verification
- `verify/sim_check.py` recomputes the rules from the dumped bars with the same timing. It reproduces the 28 decisions exactly (dates, names, amounts). With `--param max_names=2 --on-leverage allow` it reproduces 26 of 26, including the one day with more candidates than free slots.
- Disclosure: the check read our own dump (the bars and decisions of our run, in the workspace) and used a Python re-implementation. It was not used to design the strategy. I did not use `abt explain`.

## What was hard
1. Windows and units. `20d` is 20 calendar days (about 14 weekday bars on this data), so a "20-day average" in trading days needed `rows`, which the README does not mention. I anchored it at `prev(T)` to leave today out.
2. "Since entry" has no primitive. The entry is the first held bar (held now, not held at `prev(T)`), and the peak is carried forward with `prev`, which is a WF-4 recursion.
3. Minimum counts. With `min 200` alone, attempt 1 signalled on 2022-11-11, about 40 weeks into the history. A `lag(T, lookback, _)` guard requires a full 52-week history (attempts 2 and 3).
4. Sizing under the leverage policy, the largest problem. With two names at 50% of base under the default leverage policy, the run halted on 2024-12-26: "after the fill cash would be -45073.34 and gross exposure 1035468.22 against equity 990394.88 (limit 1x gross)". A fully invested book halts on a drawdown when the next fill comes, and the strategy cannot see or avoid this. The default ten-name run never holds more than 3 names, so it never reaches that state. Under the default accounting, "equal size, at most ten names" is only safe while the book is not fully invested.
5. The slot tie-break. The spec does not say which candidate wins when there are more candidates than free slots. I chose the volume ratio. The cap never binds on the default data.
6. Verification without `explain`. The dump plus the re-implementation were the only checks.

## What the docs lacked or got wrong
- The README does not mention `rows`, `greatest`, or any scalar function. `rows` appears only in the builtin table of semantic-model section 4, and `greatest` only in section 2.
- The accounting text disagrees with itself. Section 7 says that under compounding off "cash may go negative to carry a loss", but section 6 says a fill that makes cash negative halts by default. The halt compares gross exposure with equity, while the CLI usage says leverage is judged against the fixed capital when compounding is off.
- The README and the semantic model cite `docs/data-bundle.md` and a `corpus/` directory, and neither is in the workspace. I did not look for them elsewhere.
- WF-6 lists the causal builtins (prev, lag, window, prior_window, as-of) but not `rows`. The checker accepts `rows` anchored at a `prev`-bound time, and I treated it as causal.
- WF-7 and section 3 define identity as "entity-typed inputs plus its temporal key". My `cand(-A, @T, -R)` returns A as an output, which that rule does not cover. I put A in the `by` key to be safe.
- `features.flat` takes `+A`, so it must be called after A is bound (WF-2). I avoided it with `not held(A,T,_)` after `universe(A,T)`, and I did not test the error.
- The checker was clean on its first try and raised no diagnostics at all, so it gave no help with the mistakes that mattered (windows, sizing).

## Constructs: guessed or used
- `greatest(H0, P)`: the name came from section 2, and the checker accepted it.
- `rows(T0, N, min K)` with T0 from `prev(T, T0)`: accepted, and the results match the re-implementation.
- `N = max_names - C` with `C = count(...)`: Count minus Count, accepted. `1 / max_names` (a Count) is accepted as a Scalar.
- `top(N, cand(A,T,R), by (R desc, A asc))` with N from an aggregate: worked.
- Parameter ranges such as `26w..104w`, `120..260` and `0.02..0.3`: accepted.
- Not used: `rank`, `resample`, `asof`, order types, `--compounding`.

## Spec rules I am unsure about
1. 20-day average: the 20 bars before T, excluding today. Including today, or using a 20-calendar-day window, gives 26 decisions instead of 28. The L round trip (2024-05-31 buy, 2024-06-06 sell) disappears.
2. Since entry: the peak starts at the first held bar's close. Starting it at the signal bar's close gives the same 28 entries, but L's exit moves from 2024-06-06 to 2024-06-03 and J's from 2024-08-06 to 2024-07-31.
3. 52-week high: strictly above the highest close of the prior 364 days, with a full history and at least 240 closes. Ties are not signals.
4. Slot competition: ranked by volume ratio, then ticker. The cap never binds on the default data.
5. Sizing: 10% of the fixed starting capital per name, not 10% of current equity. This is the most literal reading of "equal size" under the default accounting.
6. Execution: the signal uses the close of T and fills at the close of T+1 (default). A `moc` order would fill at T's close instead.
7. Re-entry after a stop-out is allowed when a new breakout appears.
8. Stop: strictly more than 8% below the peak, on closes.

## Budget
- 12 `bin/abt` invocations: usage 1, `check` 4 (attempts 1, 2, and the final file twice), `run` 7 (attempt 1, attempt 2, the dump run, two names halted, two names with `--on-leverage allow`, and the two final runs). The Python checks on the dumps did not use it. The wrapper's `commands.log` in the workspace lists the same 12 calls.
