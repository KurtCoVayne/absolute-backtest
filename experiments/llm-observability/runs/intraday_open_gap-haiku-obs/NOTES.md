# NOTES: intraday_open_gap in abt (minute bars)

## Outcome

- `final.dsl` checks clean: 0 errors, 0 warnings. The checker reports 2 params, 0 literals, 5 rules.
- `run.sh` (full backtest on the seeded synthetic market, 40 sessions of 390 minute bars, 5 symbols) makes 64 decisions (32 buys, 32 sells) and 64 fills. Buys fill at the 09:32 bar, sells at the 16:00 bar.
- After the default cost model the book loses 7,782.71 USD (end equity 992,246.64). With `--frictionless` the same trades lose 154.00 USD, so the gross edge is about zero and costs decide the sign.
- Attempts: one, `attempts/1.dsl`. It checked clean and ran on the first try, so no later version was needed. `final.dsl` is a byte-identical copy.
- Invocations: 18 `bin/abt` calls in total, including the usage call. Two were refused (see the ledger notes). `commands.log` in the workspace root lists the same 18 calls.

## The strategy

```
first_bar(A, T)        :- universe_m(A, T), day_start(T).
gap_up(A, T)           :- first_bar(A, T), prev(T, T0), close_m(A, T0, P0), close_m(A, T, P1),
                          R = P1 / P0 - 1, R > gap.
second_after_gap(A, T) :- universe_m(A, T), prev(T, T0), gap_up(A, T0).
decide(T, buy(A, qty)) :- gap_up(A, T).
decide(T, sell(A, Q, moc)) :- second_after_gap(A, T), position(A, T, Q), Q > 0 shares.
```

- Parameters: `gap : Scalar = 0.01` and `qty : Quantity<Shares> = 100 shares`.
- A bar is labelled by its close, so the first minute is the 09:31 bar and the last is 16:00. `prev(T, T0)` on the 09:31 bar is the previous session's last minute, so the previous close needs no daily relation. I looked at the daily close in `lib/bars.dsl` (a resample with `min 300`) and did not use it.
- The buy is a market order decided on the 09:31 bar, so it fills at the 09:32 close.
- The sell is a market-on-close order decided on the 09:32 bar, so it fills at the 16:00 close. The reason is in the next section.

## Spec choices the spec does not settle

1. Quantity. The spec says "buy" but not how much. I chose 100 shares as a parameter.
2. Symbols. The spec does not mention them. The synthetic market has five (AAA, BBB, CCC, DDD, SPY), and each is tested on its own gap.
3. "More than one percent" is strict, `R > gap`. Near the boundary, six of the seven first-minute returns between 0.96% and 1.09% fire. The one that does not is AAA on 2024-01-19 (0.9605%).
4. Previous day's close is the close of the previous session's last minute. The spec does not say whether it means the daily close. Here they are the same bar.
5. "At the second minute" is a time, not a price. The fill is the 09:32 close plus the default slippage and impact. With `--frictionless` the fill equals the close exactly.
6. No position guard on buys. If a sell did not fill, the next gap day adds 100 shares, and that day's 09:32 sell takes the whole position. The spec is silent on this case.

## The sell at the day's last minute

This was the hardest part. Decision time and fill time differ here.

- No causal test says "this is the last bar of the session". There is no day-end or next-bar builtin, and WF-6 forbids a rule at 16:00 from seeing the next session.
- Per the execution contract (section 6), a market sell decided at 16:00 would fill at the next session's first bar. I did not test this.
- `moc` fills at the close of the last bar of the session containing T, so a decision at any bar of the day works. I put the decision on the 09:32 bar, after the buy fill, so the position is known and the sell can be sized from `position`.
- Consequence: the decision list shows `sell(DDD, 100, moc)` at 09:32, while the fill is at 16:00. Reading the decision list alone would suggest a 09:32 exit.

## What was hard

1. The sell timing above. The `moc` order type is described only in the order-types paragraph of section 6. The README syntax examples show `target_weight(A, W, moc)` and nothing for delta mode.
2. Literal order. `prev(T, T0)` cannot be inverted. Starting from `gap_up(A, T0)` and then writing `prev(T, T0)` would leave T unbound, so I wrote `second_after_gap` keyed on `universe_m` so that T is bound first. This was design work against WF-1, not a checker error.
3. Getting prices for an independent check. `--ledger` refuses relations with `+` arguments, and `close_m` has one. I got the prices from `--dump` in a separate run, because the two flags cannot be combined.

## What the docs lacked or got wrong

1. ADV at @1m. Section 6 says impact moves the price by a multiple of the square root of the quantity over "average daily volume". The usage text says `--adv-window N bars behind ADV (default 20)`. At @1m the executor uses the mean of the last 20 one-minute volumes. I checked this on all 64 fills: reported impact equals 0.1 x sqrt(qty / mean volume of the 20 bars ending at the fill bar) x qty x price, with zero error. Mean minute volume here is about 3,100 shares, so a 100-share order costs about 1.8% of price per fill. With daily volume (about 390 times larger, roughly 1.2 million shares) the impact would be about 20 times smaller. Impact is 98% of total costs and is why no round trip wins after costs. This needs an author decision, not a fix from me.
2. `--symbol` on a relation without an entity argument. `query --rel decide --at 2024-02-21T09:32:00 --symbol AAA --explain` printed `decide at 2024-02-21T09:32:00 (@1m): 0 tuple(s) for AAA`, directly above `rule intraday_open_gap::decide#2 fired ... first solution: A = AAA, Q = 100`. Without `--symbol` the tuple is `decide(2024-02-21T09:32:00, sell(AAA, 100, moc))`. The docs do not say that `--symbol` filters `decide` down to nothing.
3. `--ledger` restrictions. `--ledger` cannot be combined with `--dump`, `--study` or `--verify-causality`. That appears only in the error text, not in observability.md. observability.md does say that relations with `+` arguments cannot be ledgered, and I still listed `close_m` and got the refusal.
4. `position` signature. Section 6 gives `position(+A: Equity, @T, -Q)`. `abt show` prints `position(-A: Equity, @T, -Q)`. This is harmless here because A is always bound.
5. Missing references. The README cites `docs/data-bundle.md` (section 6), observability.md cites `docs/language-v2.md` (section 8), and the semantic model cites `corpus/`. None of them are in the workspace, and I did not look outside it.
6. Minute timestamps. The docs' `--at` examples are dates. `--at 2024-02-21T09:31:00` worked.

## Constructs I guessed at, and what does not exist

Guessed at, all accepted by the checker and behaving as documented:
- `sell(A, Q, moc)` in delta mode. The docs show order types only in target mode. The run confirmed that the order fills at the 16:00 close.
- `param gap : Scalar = 0.01` with no range, and `R = P1 / P0 - 1` with a bare `1`.
- `Q > 0 shares`, copied from `held_m` in `lib/features_m.dsl`.
- `day_start(T)` used as a filter on `universe_m(A, T)` to find the first minute.

Does not exist, as far as the docs and checker show:
- a day-end or next-bar builtin;
- an open price at @1m. `equities_1m` has only `close_m`, `volume_m` and `universe_m`, so `moo` and `moo_moc` have no open to fill at;
- a daily close at @1m without a resample. I avoided `lib/bars.dsl` and its `min K` entirely.

## Rules I am not sure I implemented faithfully

- Quantity: 100 shares is invented.
- Sell decision time: 09:32 rather than 16:00. The fill time matches the spec. I found no causal way to decide at 16:00 here.
- Trading all five symbols rather than one.
- No position guard on buys.
- Previous-close definition: the prior bar, which has the same value as the daily close here.

## What each inspection command told me

- `bin/abt` (usage): listed `--fills`, `--dump`, `--ledger`, `--frictionless` and `--report-by`. No change.
- `check`: "0 error(s), 0 warning(s)" on the first attempt. No change.
- `show`: `reads: gap_up+, second_after_gap+, position+`, and `decide#2: decide(T, sell(A, Q, moc)) :- ...`. The graph had no stray reads, so no change.
- `run` (attempt 1): coverage `gap_up 15600 calls 23 with tuples 32 tuples`, `second_after_gap 15600 calls 23 with tuples 32 tuples`, `decide 15600 calls 46 with tuples 64 tuples`, and no EMPTY lines. The decision list shows `sell(DDD, 100, moc)` at 09:32, which led me to check the fill times.
- `run --fills` (with the ledger): `fill 2024-01-03T09:32:00 DDD +100 @ 81.6185` and `fill 2024-01-03T16:00:00 DDD -100 @ 78.9052`. Timing confirmed. The `gap_up` ledger has 32 rows, matching the decisions.
- `run --ledger close_m`: "--ledger: `close_m` takes the inputs A; a ledger is written for relations without `+` arguments". This changed my approach: prices came from `--dump`.
- `run --ledger ... --dump`: "--ledger runs on the batch kernel and cannot be combined with --dump, --study or --verify-causality". This changed my approach: separate runs.
- `run --dump`, with `work/check_gap.py` (my recomputation from raw closes): the 32 buy decisions equal, exactly, the 32 (symbol, day) pairs whose first-minute close is more than 1% above the prior close. Fills fall only at 09:32 (buys) and 16:00 (sells). Close-to-close round trips average -0.074%, with 15 of 32 positive. Fill-based returns average -3.62%, with none positive.
- `run --dump`, with `work/check_impact.py`: the impact formula above, exact on all 64 fills.
- `run --frictionless --dump`: `win_rate 0.469` and `total_pnl -154.00`. My check found fill prices equal the bar closes (maximum difference 0.0). This confirmed the fill mechanics and that the rule has no meaningful gross edge here.
- `query --explain --rel gap_up --at 2024-02-21T09:31:00 --symbol AAA`: "rule intraday_open_gap::gap_up#1 fired ... P0 = 51.43, P1 = 51.96, R = 0.010305269298075181, T0 = 2024-02-20T16:00:00". This confirmed the reason.
- `query --explain --rel gap_up --at 2024-01-19T09:31:00 --symbol AAA`: "did not fire ... literal 6 `R > gap` has no solution". This is the same symbol with R = 0.96%, so the threshold is the reason.
- `query --explain --rel gap_up --at 2024-01-02T09:31:00 --symbol AAA`: "literal 2 `prev(T, T0)` has no solution". The first session has no prior close, so nothing trades on 2024-01-02, as intended.
- `query --explain --rel decide --at 2024-02-21T09:32:00 --symbol AAA`: "decide#1 did not fire ... literal 1 `gap_up(A, T)` has no solution" and "decide#2 fired ... Q = 100". The "0 tuple(s) for AAA" line contradicted the rule firing. I traced that to the `--symbol` issue above, and changed my next query to drop `--symbol`.

## Headline results (final.dsl, via run.sh)

- Window: 2024-01-02 09:31 to 2024-02-26 16:00, 40 sessions, 15,600 bars, 5 symbols. The first session has no prior close, so it trades nothing.
- Gap days: 32 symbol-days on 23 sessions, checked against the raw closes.
- Decisions: 64 (32 `buy` market decided at 09:31, 32 `sell` moc decided at 09:32). Fills: 64, at 09:32 and 16:00. Fill ratio 1.000, 0 dropped. Participation average 3.59%, maximum 6.50%.
- Metrics line: `metrics per bar (98280 a year, 15599 periods): cagr -0.047857   max_drawdown 0.007769   sharpe -14.8657 (population -14.8662)   volatility 0.0033`
- Additive: `total_pnl -7782.71   profit_factor 0.672 per period, 0.000 per trade   trades 32   win_rate 0.000`
- Costs: `commissions 64.00   fees 5.71   slippage 80.80   impact 7478.20   turnover 418183.66`
- Daily view (`--report-by day --periods-per-year 252`, diagnostic only): `sharpe -14.1270 (40 periods)`, with the same total PnL.
- Frictionless (diagnostic only): `total_pnl -154.00`, `win_rate 0.469`, end equity 999,845.91.

The Sharpe figures are large and negative because no round trip wins after costs, and every active day is negative (profit factor 0 per day), over a short sample.

## Files

- `attempts/1.dsl`: the only attempt.
- `final.dsl`: identical copy of attempt 1.
- `run.sh`: the full backtest command.
- `work/final-run.txt`: output of `run.sh`.
- `work/check_gap.py` and `work/check_impact.py`: my independent checks. Run as `python3 -I work/check_gap.py work/dump-1` and `python3 -I work/check_impact.py work/dump-1`.
- `work/ledger-1/`, `work/dump-1/`, `work/dump-frictionless/`, `work/run1-*.txt` and `work/gaps_run1.csv`: raw outputs of the diagnostic runs.
