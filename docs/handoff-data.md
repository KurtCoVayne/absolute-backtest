# Handoff: the real data, as found

Oct 5, 2026 · first local test of `abt` on real data (branch `handoff/local-test`).

This records what the company's data actually is, answered from the data
rather than assumed, and every decision taken to turn it into an `abt`
bundle. The engine-side contract is [`data-bundle.md`](data-bundle.md); where
the data and that contract disagree, the decision and its reason are here.

## 1. Where the data lives

| Store | What | Size | Freshness |
| --- | --- | --- | --- |
| `s3://ndlake-094828274452` | Norgate data lake (NDLake), Parquet | 4.2 GB, 142 objects | watermark 2026-10-02 |
| `~/ndlake` (local) | The same lake plus `bars_by_symbol` and the pipeline code (`pipeline/ndlake/*.py`) | 6.2 GB | watermark 2026-08-07 (stale) |
| `s3://dbn-futures-094828274452` | Databento **futures** minute bars (CME Globex, ICE US) | 6.9 GB, 117,709 files | to 2026-06 |

The bundle was built from the S3 lake (fresher), mirrored to `~/data/ndlake`
for 2010–2026. `~/ndlake/pipeline` documents the lake's conventions; the ones
that matter here are cited below.

## 2. Norgate (NDLake)

### Tables

| Table | Rows | Key columns | Notes |
| --- | ---: | --- | --- |
| `curated/bars_by_date/year=YYYY` | 5.43M in 2024 alone (4.19M equity rows, 20,905 equity assets) | `asset_id, ticker, date, open, high, low, close, volume, turnover, div_cash, split_ratio, source_symbol, series_type` | 1896–2026. `series_type` ∈ equity, index, future, cont_future(_ccb), economic, cash_commodity, forex, etf. Prices float64 from float32 sources (e.g. 81.44000244). No duplicate `(asset_id, date)`. |
| `registry/security_master` | 173,146 | `asset_id, ticker, name, security_type, exchange, status, listing_date, delisting_date, valid_from, valid_to, is_current` | SCD-2, half-open `[valid_from, valid_to)`. Versions record attribute changes, not a ticker history. `delisting_date` = the last trading day. |
| `registry/corporate_actions` | 1,029,683 | `asset_id, action_type (DIV/SPLIT), ex_date, div_cash, split_ratio` | 991,037 DIV, 38,646 SPLIT. Ex-date only: **no announcement or pay date**. |
| `indexes/index_membership` | 191,143 | `index_name, asset_id, member_from, member_to` | 52 indexes (S&P 500/400/600/1500, Russell 1000/2000/3000 and others, Nasdaq 100, DJIA, TSX, ASX). Half-open `[member_from, member_to)`, `member_to` NULL while current (`pipeline/ndlake/membership.py`). |
| `classification/classification_full` | 112,604 | `asset_id, scheme, level, code, snapshot_date` | GICS levels 1–4, TRBC levels 1–5. **One snapshot, 2026-09-21: not point in time.** |
| `registry/fundamentals` | 243,721 | `asset_id, field, value, as_of_date` | `mktcap`, `sharesoutstanding` and others. Not used (the engine has no fundamentals relation). |
| `registry/trading_calendar` | 27,294 | `date, market, is_session` | Not used (the bundle derives its days from the bars). |

### The blocking question: are the prices adjusted?

**No. They are as traded (unadjusted)**, which is what the engine needs:

- AAPL closes at 499.23 on 2020-08-28 and 129.04 on 2020-08-31, its 4:1 ex-date.
- NVDA closes at 1208.88 on 2024-06-07 and 121.79 on 2024-06-10, its 10:1 ex-date.
- The lake's Phase-0 gate refuses to ingest back-adjusted series as the system of record (`~/ndlake/pipeline/README.md`).

Splits come from `corporate_actions` with `split_ratio` = new shares per old
(4.0 for AAPL; reverse splits below 1). It is used as the `factor` unchanged:
no inversion is needed. Dividends are in unadjusted, per-share terms
(AAPL paid 0.82 before its split and 0.205 after).

### Other answers

- **Stable identifier:** `asset_id` survives ticker changes and reuse. Delisted
  names keep their own `asset_id`, and Norgate decorates their source symbol
  with the delisting month (`AGN-201503`).
- **Symbol history:** **none usable.** `bars.ticker` is the asset's *final*
  ticker applied to its whole history (Denbury shows `DNRCQ` from 2010). Since
  2014, 163 equity assets show more than one ticker. In the S&P 500 subset, 5
  ticker collisions between assets were renamed to the Norgate source symbol.
  The `ticker` relation therefore resolves only *current* tickers correctly:
  a strategy with a ticker literal (W6) sees the final ticker.
- **Delistings:** a delisted status and date exist, but **no reason**. See §4.
- **Index membership:** interval rows, point in time, delisted members included.
- **Classification:** a single snapshot.

## 3. Databento

The bucket holds **futures only**: `GLBX.MDP3/ohlcv-1m/<ROOT>.FUT/<date>.parquet`
(about 30 roots: ES, NQ, CL, GC, ZN, 6E and others) and
`IFUS.IMPACT/ohlcv-1m/...`, plus `definition/<ROOT>.parquet`.

- **Schema:** `ohlcv-1m`. Columns `ts_event (timestamp ns UTC), instrument_id (u32), symbol (ESU6), open, high, low, close (float64), volume (u64)`.
- **Prices** are already decoded floats, not fixed-point int64 at 1e-9.
- **`ts_recv`** is absent, as expected for OHLCV.
- **Symbology:** each row carries `instrument_id` and the raw `symbol`. `definition` maps `instrument_id → raw_symbol, root, activation, expiration, min_price_increment, unit_of_measure_qty (multiplier), currency`.

**There are no US equity minute bars**, so the `equities_1m` bundle of the
plan (one company as one `Equity` across both bundles, joined by raw symbol)
cannot be built from this bucket. The engine also has no futures model: no
contract multiplier, roll or margin by contract. Both stay recorded as gaps
(`docs/assessment.md`).

## 4. Decisions taken to build `equities_1d_v2@2026.10` (S&P 500, point in time, 2014–2026)

Script: `scripts/ingest/norgate_lake_to_inputs.py` (formerly `norgate_parquet_to_csv.py`; DuckDB, streamed to Parquet since 2026-10-06, CSV before
with `COPY`). The report is `ingest-report.json` in the output directory.

| # | Decision | Why |
| --- | --- | --- |
| 1 | The universe is every asset that was ever an `S&P 500` member with an interval overlapping [2014-01-01, 2026-10-02]: 756 assets. | Point in time, never the current list. |
| 2 | Membership is `[member_from, member_to)` converted to inclusive (`to` = the bar before `member_to`), clipped to the asset's bars, and split around days without a bar (5 intervals). | The adapter's `to` is inclusive. The membership test wants every member in the universe on that day. |
| 3 | The delisting date is the first trading day after the last print. 151 assets; in every case Norgate's `delisting_date` equals the last bar. | The executor force-closes at the last trade. `universe` stops at the delisting bar. |
| 4 | **The delisting reason is inferred:** a ticker ending in Q, or a last close under $1 → `bankruptcy` (12). A last close under 50% of the 60-bar max → `other` (1: AABA). Anything else → `acquisition` (138). | Norgate has no reasons. Under the default haircuts (bankruptcy 1, other 1, acquisition 0) an unmapped reason would be a total loss, wrong for the many acquisitions. **AABA (Altaba, a voluntary liquidation) is misclassified as `other`.** Override per run with `--haircut other=0`. |
| 5 | Dividends: announce = pay = ex-date. | Norgate has neither date. Announce = ex is conservative. Pay = ex credits the cash a few weeks early (slightly optimistic on cash). **Consequence: `dividend_capture` (buy after the announcement, before the ex-date) can never fire on this data.** That is correct, since no earlier announcement time is known. |
| 6 | A dividend on the same ex-date as a split is divided by the split factor (9 cases). | Norgate quotes it per pre-split share. The executor (`actions()` in `src/kernel/executor.rs`) applies the split first and pays the dividend on the post-split position. HLT 2017-01-04 (a 1:3 reverse split plus the Park and HGV spin-offs) and HON 2026-06-29 reconcile only this way. |
| 7 | Spin-offs stay as Norgate books them: a cash "dividend" worth the spun-off shares on the ex-date (GOOGL 2014-04-03 $567, KDP 2018 $103.75, DD 2025 $47.50, CTVA 2026 $66 and others; 18 in total). | It is total-return correct: the holder is credited the value at the ex-date. The catalog has no spin-off relation. Strategies that read `dividend` amounts see these as huge dividends. |
| 8 | Classification: GICS level 1 from the 2026-09-21 snapshot, emitted only from that date. | Using it earlier would be look-ahead. Sector-neutral strategies have no history to work with. |
| 9 | Prices are rounded to 6 decimals; a missing or non-positive O/H/L would be filled from the close (0 in this subset). | Removes the float32 noise while keeping sub-cent prints positive (FRCB's low of 0.000001 in December 2023). |
| 10 | Tickers: the final ticker per asset. A collision renames the earlier-ending asset to its source symbol (5 cases). | The identity test rejects overlapping intervals. |

## 5. Bundle tests: what failed and what was decided

| Test | First result | Data or test? | Decision |
| --- | --- | --- | --- |
| membership | 29 problems: a member on a day the name has no bar | Data (halts and missing rows); the test is right | Converter decision 2 |
| action reconciliation | 403 problems | Mostly the **test**: see below | Code change and reviewed exceptions |

Action reconciliation, in detail:

1. **It ignored dividends** (22 problems). A spin-off booked as a distribution,
   and a reverse split plus distribution on one day, are explained by the
   dividend. The doc requires "every price gap at an ex-date is explained by
   an action", and a dividend is an action. **Changed in `src/bundle.rs`:** the
   ratio is now (close + that day's dividends) × split factor / previous
   close (tests in `tests/bundle.rs`).
2. **The band [0.6, 1.67] is too strict for real US equities** (380 problems):
   - **336** are distressed or post-failure trading: FRCB, SIVBQ and SBNY after
     their 2023 failures, plus the Q-tickers.
   - **10** are the 2020-03-09 oil crash and COVID-19 moves.
   - **9** are GME, PBI and FOSL in January 2021.
   - **31** are single-name events: WWE 2014, GMCR's buyout, PG&E's bankruptcy
     filing, EPAM 2022, GL's short report, DXCM, CPRI, FL, CNC, FISV, FMC and
     others.

   Loosening the band would also hide a missing split. Instead, a bundle may
   carry **reviewed exceptions** (`exceptions.parquet`: `test,security,date,reason`; a text file until 2026-10-06).
   The test accepts a listed problem and counts it in its detail. **Added in
   `src/bundle.rs`, `src/ingest.rs` and `abt bundle build`.** The reviewed file
   is `scripts/ingest/reviewed/norgate-spx-2014.parquet` (383 rows; edit with `scripts/ingest/exceptions_edit.py`), drafted by
   `scripts/ingest/reconcile_report.py --propose` and reviewed by category.
3. **Real data errors kept, not corrected:**
   - TFCF and TFCFA on 2019-03-19: Norgate books the Fox Corp separation as a
     0.736817 split with no price move, so a holder loses about 26% that day.
   - CHKAQ's 1:200 reverse split on 2020-04-15 came with a real −38% move.
4. **Unverified:** SNDK on 2025-02-13 (−53%, near the SanDisk separation;
   when-issued pricing?) and MRNA on 2026-08-19 (+177%). Both are accepted
   provisionally and marked `UNVERIFIED` in the file.

Final result: `equities_1d_v2@2026.10` passes all 6 tests (382 reviewed
exceptions accepted). One of the 383 rows matched no problem in the engine's
test; the Python diagnostic and the Rust test disagree on one bar, which is
not yet identified (the rows have no duplicates).

## 6. First runs on `equities_1d_v2@2026.10`

Machine: Apple M4, 10 cores, 16 GB RAM, macOS; release build (thin LTO,
one codegen unit). Data: 756 assets, 3,207 bars (2014-01-02 to 2026-10-02),
16,436,755 tuples. Default cost model.

| Strategy | `--param` | run s | bars/s | peak RSS MB | decisions | total return | max drawdown |
| --- | --- | ---: | ---: | ---: | ---: | ---: | ---: |
| total_return_momentum (long only) | n=20 | 145.2 | 22.1 | 7,383 | 69,164 | +453.2% | 59.5% |
| momentum_12_1 (long-short) | n=50 | 109.2 | 29.4 | 5,847 | 18,518 | −9.2% | 38.0% |
| low_volatility (long-short) | n=50 | 100.3 | 32.0 | 7,296 | 20,839 | −52.4% | 57.9% |
| short_term_reversal (long-short) | n=50 | 99.6 | 32.2 | 7,268 | 26,414 | −52.7% | 55.4% |
| size_proxy (long-short) | n=50 | 6.9 | 467.4 | 3,549 | 16,989 | −36.9% | 40.1% |
| dividend_capture | | 11.4 | 281.5 | 3,846 | 0 | 0 | 0 (cannot fire: announce = ex, §4 item 5) |

Loading the bundle takes 1.3 to 1.6 s each time. Run time is dominated by
windowed aggregations: about 400M window-bar solves per run for the
strategies with a rolling feature. `size_proxy`'s 60-day ADV solves 1.6M.

- **Reference diff** (`momentum_12_1`, n=50, dump of 350 MB):
  `kernel and reference agree`. The pandas replay takes 110 s.
- **Stylized facts** (`ABT_BUNDLE_DIR=… ABT_STYLIZED_N=50 cargo test --release --test stylized`):
  all four inside their ranges, 327 s.

  | Strategy | CAGR | Volatility | Sharpe (se) | Max drawdown | Turnover |
  | --- | ---: | ---: | ---: | ---: | ---: |
  | momentum_12_1 | −0.75% | 10.7% | −0.02 (0.33) | 38.0% | 5.57 |
  | low_volatility | −5.67% | 10.7% | −0.49 (0.32) | 57.9% | 8.01 |
  | short_term_reversal | −5.72% | 8.1% | −0.69 (0.31) | 55.4% | 16.54 |
  | size_proxy | −3.56% | 5.2% | −0.67 (0.34) | 40.1% | 3.05 |

  The low-volatility failure year (2020) shows a negative Sharpe as
  required. The 2009 momentum check does not apply: the sample starts in
  2014. All four are dollar-neutral, not beta-neutral, inside the S&P 500,
  net of costs and a borrow proxy, over a period dominated by high-beta
  large caps. Negative premia are plausible there, and none is significant.
