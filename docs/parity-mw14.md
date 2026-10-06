# MW14 in abt

MW14 is the company's weekly long-only S&P 1500 momentum book (stageanalyst
`strategy/ivmom/book.py`, canon re-frozen 2026-08-14), the production book.
abt reproduces it from the same NDLake daily bars, with every rule in
`corpus/company/mw14.dsl`, and matches the canonical weekly returns exactly.

## Result

The reference is the book's canonical series `_canon_wkret_mw14_full.npy`:
1,852 weekly returns from the decision week ending 1991-01-04, scored by the
book's own `run_mw1_core.stats` (52 weeks a year, risk-free 0, population
deviation, CAGR and drawdown compounded on the fixed-base returns).

| | Python canon | abt |
| --- | ---: | ---: |
| weeks | 1,852 | 1,852 |
| CAGR | 20.8995% | 20.8995% |
| max drawdown | −26.1782% | −26.1782% |
| Sharpe | 1.0199 | 1.0199 |
| volatility | 20.74% | 20.74% |
| P&L on the fixed $1M | $7,534,877 | $7,534,876.54 |
| weekly correlation | | 1.000000 |
| largest weekly difference | | 7.9e-15 |
| fixed-capital (additive) drawdown | −29.27% ($292,686) | −29.2686% ($292,686) |
| 1990s / 2000s / 2010s / 2020–26 CAGR | 25.54 / 16.04 / 12.30 / 36.88% | 25.54 / 16.04 / 12.30 / 36.88% |

Every one of the 1,852 weeks equals the canon to floating-point rounding
(`scripts/parity/mw14_compare.py`); so every target, every banded trade and
every cost agreed. abt's own report over the canon window (`--end
2026-07-02`, so that the book's last week, which books its cost with no
return, has the bar after it) prints the same figures: `cagr 0.208995
max_drawdown 0.261782 sharpe 1.0196 (population 1.0199)`, and on the
additive side `total_pnl 7534876.54`, drawdown 0.292686.

## How the book maps onto abt

**Data** (`scripts/ingest/ndlake_weekly.py`, env `corpus/env/equities_1w.dsl`).
The weekly panel of `data.py` is rebuilt from the daily bars: one bar per
market week keyed at the week's last trading day (a name that last traded on
Thursday is keyed at Friday, as the book keys rows by week), the name's last
raw close (the $3 floor), its weekly mean daily dollar volume, its split
factor, S&P 500/400/600 membership at its own week end (inclusive spells),
and the running total-return index the book accrues (the product of the
daily (close + dividend) x split / previous close) as the price the executor
fills and marks on. The gate's $SPXTR index is built in float32, as the
book's vendor export forces; VIX and its 4-week mean are the book's own
weekly inputs. Names that stop trading get a `delisted` event the week after.

**Rules** (`corpus/company/mw14.dsl`):

| Book | abt |
| --- | --- |
| `adj_close = close x cumprod(split)` | `cumf` by temporal recursion over the name's own rows (`asof` reaches its previous row) |
| Clenow: 18-row OLS of log adj_close on time, `(exp(52 b) - 1) R²` | `ols_beta` and `corr` over `rows(T, 18, min 18)` against a row counter |
| `in_universe`: member, raw close >= $3, 4-week dollar volume >= $2M | `eligible` (`rows(T, 4, min 4)`) |
| R1 ordinal rank of the score, ties by asset id, no score no rank | `rank(scored(...), by (S desc, A asc), as K)` (ids zero-padded asset ids) |
| R2 rank <= 7, gate, dollar-volume decile >= 5 | `enter`; the decile `ceil(rank / n x 10) >= 5` as `rank / n x 10 > 4` with `rank ... ties average` |
| R3 rank <= 28 or adj_close above its 40-row mean | `stay` (`rows(T, 40, min 40)`) |
| latch `stay & cummax(enter)` per run of stays | `lp`: 1 or 0 at every row, `stay and (enter or lp at the previous row)` |
| R5 gate or VIX at or below its 4-week mean, else scale 0 | `unpanicked` |
| R7 dial 0.70 when > 25% of latched names are > 50% above their 40-row mean | `crowded`, `scale` |
| R4 `min(1/N x scale, 0.15)`, kept above 1e-6 | `w` (in the book's order of operations) |
| R6 band 2.5% (1e-12 tolerance) on the drifted fixed-base weight; exits always | the four decide rules, `target_weight(A, W, moc)` |
| fills at the week's close, 10 bps a side on traded weight, fixed $1M | `moc`, `--commission-bps 10`, `--compounding off --capital 1000000` |

**Run**:

```
abt run --strategy mw14 --bundle B --untested --price-relation trclose --actions in-prices \
  --compounding off --capital 1000000 --lot fractional --frictionless --commission-bps 10 \
  --margin-rate 0 --on-leverage allow --delist-proceeds last-price \
  --start 1991-01-01 --end 2026-07-02 --periods-per-year 52 --window-sums exact \
  corpus/env corpus/company/mw14.dsl
```

`--window-sums exact` computes the rolling `mean`, `corr` and `ols_beta` with
the two-pass formulas, so the weeks equal the canon bit for bit. With the
default running sums the result is the same here (all 1,852 weeks), but a
last-bit difference could in principle move a name across a band edge.

`--untested` because the bundle's action-reconciliation test, calibrated on
daily bars, flags 8,560 weekly moves of small caps beyond its thresholds; the
data is the book's as it uses it, and the other five tests pass. The run
takes 145 s at a 3.1 GB peak on an Apple M4 (4,240 names, 4.2M asset-weeks,
12.8 bars/s; it took 16–18 minutes and 8–10 GB before the performance work
recorded in `docs/assessment.md`).

## Known differences, none of which occurred

The translation keeps three behaviours the book does not share; none moved a
week of 1991–2026, but they are the places to look on other data:

- A held name with no bar in a week (a halt) is kept and marked at its last
  price; the book drops it at no cost and may re-buy it.
- A held weight drifts as quantity x price / base here and as w (1 + r) in
  the book: equal up to rounding, which could flip a trade at the band's edge.
- The book's last decision week books its cost with no return. Here that
  needs a bar after it: the market series run past the equities, so ending
  the run at 2026-07-02 includes it (a run ending on the equities' last week
  settles its closing orders at the end of the run instead).

## Engine pieces the book needed

Fixed-base accounting (now the default), `moc` orders (trade at the signal
close), `rows` windows and `rank` in the DSL, completeness of recursive
relations, `--actions in-prices`, `--delist-proceeds last-price`,
`--commission-bps`, `--start`/`--end` (features warm up on 1990, trading
starts in 1991) and the metric conventions. See `docs/semantic-model.md`.

## Reproduce

```
python scripts/ingest/ndlake_weekly.py --lake ~/data/ref/mw14/ndlake --artifacts ~/data/ref/mw14/artifacts --out IN
abt bundle build --from IN --env equities_1w --version mw14 --out B corpus/env corpus/company/mw14.dsl
abt run ... --returns R.parquet        # the command above
python scripts/parity/mw14_compare.py R.parquet ~/data/ref/mw14/artifacts/_canon_wkret_mw14_full.npy
```

Inputs and their checksums: `docs/parity-inputs.md`.
