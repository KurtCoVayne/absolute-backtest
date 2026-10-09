# NOTES: intraday_open_gap in abt

## Deliverables

- `final.dsl`: the strategy (a copy of `attempts/4.dsl`). `bin/abt check env lib final.dsl` reports 0 errors and 0 warnings; degrees of freedom 2 params, 0 literals, 3 rules.
- `run.sh`: `bin/abt run --strategy intraday_open_gap --synthetic --days 40 env lib final.dsl`, run from the workspace.
- `attempts/1.dsl` to `attempts/4.dsl`: 1 is the first version (same logic, parameter named `gap`, no comments); 2 is the commented version and gives identical numbers; 3 is a `moo_moc` variant, rejected because the run dropped all 32 entries; 4 is the final text with a corrected comment.
- `logs/`: the run outputs quoted below.

## The strategy as written

- `gap_open(A, T, G)`: at a session's first bar (`day_start(T)`), G = close(T) / close(prev(T)) - 1. For a session's first bar, `prev(T)` is the previous session's last minute, so G is measured against the previous close.
- `decide(T, buy(A, qty))`: when G > `min_gap` (0.01). Decided at minute 1, so the market-order fill is at minute 2's close.
- `decide(T, sell(A, Q, moc))`: at the bar after an entry decision (`prev(T, T1)` with `decided(T1, buy(A, _))`), sell the shares held (`position(A, T, Q)`, Q > 0). The `moc` order fills at the close of the session's last minute.

## What was hard

1. Selling at the session's last minute without looking ahead. There is no day-end builtin (only `day_start`), and a decision cannot refer to a later bar. The only causal form I found is a `moc` order decided one bar after the entry. That forced two choices: gate the exit on the entry's `decided` record, and size it from `position`, not from the entry quantity, so that an entry which did not fill cannot produce an oversize sell.
2. Timing. Decisions at T fill at the next bar's close, and `position(A, T)` already includes fills of decisions made at `prev(T)`. Both facts are in the semantic model (sections 4 and 6). I had to combine them to see that the minute-2 exit sees the minute-1 entry.
3. The construct that reads most like the brief, `moo_moc` ("in at the next open, flat at that session's close"), passes `check` but cannot run on `equities_1m`. Attempt 3 dropped all 32 entries with "moo: X did not open in the session after T" (and "moo_moc: the data ended before it executed" for the last session). The minute environment has no open series, so an opening fill has no price.
4. Debugging numbers. `abt explain` reports only "fired" with the bindings, or the first failing literal (for example "literal 2 `G > min_gap` has no solution"). It never prints computed values such as G, so I could not inspect the gap directly.

## What the docs lacked or got wrong

- Order types on delta constructors: the docs show only `target_weight(A, W, moc)`. Nothing says that `buy` and `sell` take the same trailing term. `sell(A, Q, moc)` was accepted and honoured (fills at 16:00).
- `moo` and `moo_moc`: the docs say they fill at the next bar's open but do not say which relation supplies that open. `check` does not warn when the environment has no open. The run only reports a dropped count and one line per drop.
- `position` signature: semantic model section 6 gives `position(+A, @T, -Q)`, but `lib/features.dsl` (`held`) and `lib/features_m.dsl` (`held_m`) call `position(A, T, Q)` with A unbound. `check` reports 0 errors. Either the signature, the libraries or the checker is wrong.
- Missing reference: the README and the semantic model cite `docs/data-bundle.md` (degrees of freedom, cost model, metrics, bundle rules). It is not in `docs/`. The cost model and the ADV definition came only from the `abt run` usage text.
- ADV at minute resolution: the usage text says impact is measured against ADV over `--adv-window N bars behind ADV` (default 20). The semantic model says "average daily volume". At minute resolution the default is a 20-minute average. The default run's impact cost (7,478.20 over 64 fills of 100 shares, mean fill price about 65.3, about 1.8% of price per fill) implies an ADV of roughly 3,000 shares if the documented formula is read as I read it. That is about one minute's volume, not a day's. This is my inference from the numbers, not something the docs state, but it makes impact the dominant cost in this run.
- Metrics: the default metrics are per bar (98,280 periods a year), so the Sharpe annualises minute returns. For a strategy that trades once a day, `--report-by day` (252 a year, 40 periods) is the meaningful view. The docs do not recommend it.
- No documented way to name "the last bar of the session" other than `moc`.

## Constructs guessed or not verified

- `sell(A, Q, moc)`: a trailing order type on a delta constructor, guessed from the `target_weight(A, W, moc)` example. Accepted and honoured.
- `buy(A, qty, moo_moc)`: accepted by `check`, unusable on `equities_1m` (see above).
- `param min_gap : Scalar = 0.01 in 0.0..0.2`: a Scalar range, accepted.
- `day_start(T)` used as a filter on a @1m rule: accepted.
- `prev(T, T0)` at a session's first bar returning the previous session's last bar: relied on the definition of the time domain; the docs give no example.
- `Q > 0 shares`: taken from `lib/features.dsl`; accepted.
- Not used or not tested: `limit`, `stop`, `asof`, `resample`, the `bars` library, `--price-relation`.

## Spec points I am unsure I implemented faithfully

1. Size. The brief gives no quantity. I used 100 shares per entry (`param qty`). Impact grows with the square root of the quantity, so the results depend on this choice.
2. Entry price. "Buy at the second minute" is implemented as the market fill at minute 2's close, the executor's default. Minute 2's open is not in the data, so the open reading is not available.
3. Exit price. "Sell at the day's last minute" is the `moc` fill at the 16:00 bar's close.
4. Threshold. "More than one percent" is G > 0.01, with the first minute's close compared against the previous session's last close. The inequality is strict.
5. Instruments. The brief names none. The strategy applies to every symbol in `universe_m` (AAA, BBB, CCC, DDD, SPY).
6. Exit sizing and leftovers. The exit sells the shares actually held, so a partially filled entry exits only what filled. There is no flat check at entry: if an exit ever failed and shares were left over, the next entry would add to them. That did not happen here; every session ended flat.
7. First session. 2024-01-02 has no previous close, so it has no trades.
8. Costs and seed. The cost model is the kernel default. I did not pass `--seed`. Two runs of the same strategy gave identical numbers.

## Headline results (final.dsl, run.sh)

- Data: 15,600 minute bars from 2024-01-02 09:31 to 2024-02-26 16:00 (40 sessions), 5 symbols.
- Decisions: 64 (32 buys at minute 1, 32 `moc` sells decided at minute 2). Dropped: 0.
- Fills: 64 (32 buys at 09:32, 32 sells at 16:00). Fill ratio 1.000.
- Entries: 32 out of at most 195 symbol-sessions with a prior close (39 sessions x 5 symbols), about 16%.
- Sessions with at least one entry: 23 of 40.
- Round trips: 32, all losing (win rate 0.000), average about -243 per trade. Total P&L -7,782.71. End equity 992,246.64 from 1,000,000.
- Costs: commissions 64.00, fees 5.71, slippage 80.80, impact 7,478.20. Turnover 418,183.66.
- Metrics line (per bar, default): cagr -0.047857, max_drawdown 0.007769, sharpe -14.8657 (population -14.8662), volatility 0.0033.
- Metrics with `--report-by day`: cagr -0.047859, max_drawdown 0.007754, sharpe -14.127, volatility 0.0034.
- Sensitivity, not the run convention (`--frictionless`): total P&L -154.00, win rate 0.469, end equity 999,845.91. Gross of costs the signal is about flat; impact accounts for most of the default loss.
- `--verify-causality`: passes at 5 sampled bars.

## Invocations

17 `bin/abt` calls: 1 usage, 5 checks, 8 runs (including `run.sh` twice), 3 explain.
