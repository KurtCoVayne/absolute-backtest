# MORNIGHT-R8L in abt

MORNIGHT-R8L is the intraday futures opening-range book of `d20-research`
(PAPER-BOOK PB-5 and its 2026-09-02 amendment, ledger row 150): R8 without the
late leg, over the HOME-26 roots, 2000 to 2026-08-14, on TradeStation
1-minute back-adjusted continuous contracts. abt reproduces it from the same
minute bars, with every rule in `corpus/company/r8l.dsl`, and matches the
book exactly.

## Result

The reference is the book's fixed-base scorecard (`results/r42`,
`scripts/r39_fixed.py`): daily P&L over the $1M base on the ES session
calendar (6,850 days, days without a leg at zero), annual return = mean x 252,
Sharpe = mean / population deviation x sqrt(252), drawdown of the cumulative
sum, at f = 0.6494% of the base per ATR per leg (the largest f with a 20%
drawdown).

| | Python book (r42) | abt, replication bundle | abt, real prints |
| --- | ---: | ---: | ---: |
| legs | 21,892 | 21,892 | 21,892 |
| annual return | 18.9460% | 18.9460% | 19.0281% |
| Sharpe (daily) | 1.277734 | 1.277734 | 1.283499 |
| max drawdown (of $1M) | 19.6680% | 19.6680% | 19.4741% |
| total P&L | $5,150,002.36 | $5,150,002.36 | $5,172,308.38 |
| profit factor (daily) | 1.311314 | 1.311314 | 1.312947 |
| profit factor (per leg) | 1.18 | 1.184 | 1.185 |
| winning legs | 52.8% | 52.8% | 52.8% |
| days equal to the book's (1e-9 of the base) | | 6,850 of 6,850 | 6,626 of 6,850 |

Leg by leg (`scripts/parity/r8l_compare.py` against
`results/r40/leg_flags.parquet`), abt makes the book's 21,892 legs, market
and session for market and session, each in the book's direction. With the
replication bundle every leg's P&L equals the book's to 1e-6 dollars.

The two bundles differ in one convention only. Where the session's minute 30
did not trade (231 of the legs), the book fills the entry at the close before
minute 30, a price that was not printed at the entry time (its own audit,
F-015, lists it). The default bundle enters at the next traded minute's open,
the first price a market order could have had; the replication bundle
(`--entry-fallback signal-close`) places the book's price at minute 30 so
the run reproduces the book. Those 231 legs account for the whole difference
in the last column (+$22,306 over 26 years).

## How the book maps onto abt

**Data** (`scripts/ingest/tradestation_sessions.py`, env
`corpus/env/futures_sessions.dsl`). The session grid is rebuilt exactly as
`build_panel_ts` builds it: each root's primary-session windows
(`sessions.py`, the ET windows moved to Chicago by a constant hour), minute
index = minutes since the window opens, grid width = the most common session
length, minutes beyond it dropped, sessions through 2026-08-14. From the grid
the bundle keeps the session bar (first open, high, low, last close, `@1d`)
and the real minute prints the book reads: the open of minute 0, the close of
the last traded minute before minute 20 and before minute 30, the entry print
(the first traded minute from minute 30) and the exit print (the session's
last traded minute), each labelled at its minute's close. A full minute
bundle (about 47M bars) does not fit the kernel's memory on this machine; the
prints are real bars of the grid, not computed values, and every feature is
computed by the strategy. Checked against the golden legs, the minute-0 open,
the two closes, the ATR, the gap and the exit price equal the book's on all
21,892 legs.

The protocol's trading calendar is data: `clock` holds the decision time (30
minutes after the open) of every session the book may trade, an ES session day
in the root's usable years (`DEV_USABLE_FROM`) from 2000-01-01 to 2026-08-14;
`calendar` holds the ES session days the scorecard reports on. The security
table carries each root's point value (6J scaled by 0.01, TradeStation's 6J
prices being 100x the exchange's) and its commission per contract per side.

**Rules** (`corpus/company/r8l.dsl`):

| Book | abt |
| --- | --- |
| `atr_sma(o, h, l, c, 20)` rolled one session | `atr` @1d: mean true range over `rows(T, 20, min 20)` of the root's sessions; read at the decision as of the previous session |
| `m = (c30 - o0) / atr`, `g = (adj_open - prev_c) / atr` | `sig`: today's prints `asof T` after the session's open, the previous session's ATR and close |
| `m_med = |m|.rolling(250, min_periods=100).median().shift(1)` | `m_med` @1d over `rows(T, 250, min 100)` anchored on `session` (a session without m still takes a place), read only when keyed at the previous session (`shift(1)` is the previous row, never an older value) |
| main: `|m| >= 2 m_med`, gap aligned, `latefrac <= 1/3`, `dir = sign(m)` | `leg` (two rules, one per sign) |
| sidecar: `|m| < 2 m_med`, `g <= -0.75`, short | `leg` (third rule) |
| `net = dir (x - e) / atr - comm_r`, `D = f x $1M` | `target_quantity(A, dir x D / (ATR x point value), moo_moc)`: in at the entry print's open, flat at the session's last close, commission per contract per side from the security table |

**Run** (fixed $1M, no compounding, fractional contracts, commission only):

```
abt run --strategy r8l --bundle B --price-relation close_m --compounding off \
  --capital 1000000 --lot fractional --frictionless --margin-rate 0 \
  --on-leverage allow --on-margin-call allow --report-by day --report-calendar calendar \
  --periods-per-year 252 --window-sums exact corpus/env corpus/company/r8l.dsl
```

`--window-sums exact` is needed for the exact match: with the default running
sums the 20-session ATR differs from the book's in the last bits, which flips
one leg's threshold test (ZT, 2018-03-09; 21,891 of 21,892 legs, Sharpe
1.2782 against 1.2777).

`--on-margin-call allow` because a futures book's notional is many times its
equity and the book has no margin model. The run takes 3.9 s at a 0.6 GB
peak (Apple M4, 183,175 decision bars, 47,000 bars/s; 14 s and 1.5 GB before
the performance work in `docs/assessment.md`).

## Engine pieces the book needed

- **Order types**: `moo_moc`, an intraday position (in at the next open, flat
  at that session's close); a market-on-close order fills at the step of its
  closing bar.
- **Contracts**: the multiplier and per-contract commission in the security
  table; back-adjusted prices may cross zero; futures are sold short without a
  borrow fee or a locate.
- **Fixed-base accounting** (`--compounding off`, the default) and the two
  metric conventions, with `--report-by day --report-calendar REL`.
- **Rows windows anchored on the first atom** (a rolling window with a minimum
  of periods), and **as-of reads of daily bars from a minute rule** that see a
  day only once it has closed (before this, a minute rule could read the day's
  own bar intraday).

## Reproduce

```
python scripts/ingest/tradestation_sessions.py --cache ~/data/ref/r8l/cache \
  --config ~/data/ref/r8l/config --commissions ~/data/ref/r8l/commissions_per_side.json \
  --out IN [--entry-fallback signal-close]
abt bundle build --from IN --env futures_sessions --version r8l --out B corpus/env corpus/company/r8l.dsl
abt bundle test B corpus/env corpus/company/r8l.dsl
abt run ... --returns R.parquet --dump DUMP      # the command above
python scripts/parity/r8l_compare.py DUMP R.parquet ~/data/ref/r8l/results/leg_flags.parquet
```

Inputs and their checksums: `docs/parity-inputs.md`.
