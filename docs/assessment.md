# Assessment: distance from a product

Oct 5, 2026 · after the first runs on real data (branch `handoff/local-test`).
The data and its decisions are in [`handoff-data.md`](handoff-data.md).

## Update, Oct 6, 2026: two production books reproduced

Two of the company's production strategies now run in abt with the same
results as their reference implementations (`docs/parity-mw14.md`,
`docs/parity-r8l.md`):

- **MW14**, the weekly S&P 1500 momentum book: its 1,852 weekly returns over
  1991–2026 equal the Python canon to 8e-15 (CAGR 20.90%, max drawdown
  −26.18%, Sharpe 1.02, P&L $7.53M on a fixed $1M). The run took 16–18
  minutes at an 8–10 GB peak; after the performance work below it takes
  145 s at 3.1 GB.
- **MORNIGHT-R8L**, the intraday futures opening-range book: its 21,892 legs
  are the book's, and with the book's entry convention every leg and every
  one of 6,850 days equals it (+18.95%/yr, Sharpe 1.28, drawdown 19.67%,
  $5.15M). The run took 14 s at 1.5 GB; now 3.9 s at 0.6 GB.

What that required of the engine, all opt-in or with the old default kept
except accounting: fixed-base accounting as the default, order types with a
time in force (market, MOO, MOC, MOO-MOC, limit, stop), futures contracts
(multiplier, per-contract commission, prices across zero, no borrow), row
windows and a rank reduction in the DSL, delisting at the last price,
dividends reinvested, actions carried by a total-return price, a run window,
both metric conventions on a reporting calendar, and two fixes: completeness
of recursive relations (a greatest fixpoint), and as-of reads of daily bars
from a minute rule (which could see the day's own bar intraday).

CSV is gone (gap A4, first half): every file abt reads or writes outside a
bundle's log is Parquet, and the symbol history is indexed. On the SPX
bundle the build fell from 7.6 s / 3.6 GB to 4.9 s / 2.0 GB, about 1.0 KB
per price row. The second half of A4 (streaming the rows instead of holding
them) and A1 (incremental windows) remain: MW14 spends its 18 minutes in
windowed aggregation, and a full-minute futures bundle (about 47M bars) does
not fit this machine, which is why the R8L bundle keeps the minute prints the
book reads rather than every minute.

## Performance work, Oct 6, 2026

Profiled with macOS `sample` on release builds (line tables on). Each change
kept MW14's 1,852 weeks equal to the canon (to 8e-15) and R8L's legs and
days equal to the book. Every step is timed on the same machine (Apple M4,
16 GB):

| Step | What it fixed (profile share before) | MW14 1991–95 | MW14 full | R8L full |
| --- | --- | ---: | ---: | ---: |
| before | | 56.8 s, 7.1 GB | 963 s, 9.6 GB | 13.7 s, 1.5 GB |
| 1. copies and layout | the executor cloned its bar history every step (35% of R8L); `RelInfo` cloned per call; `Value` had grown to 48 bytes (now 16); SipHash on the memo; two copies of the data (the CLI's dataset now moves into the kernel); freeing at exit | 41.5 s, 3.8 GB | | 7.9 s, 0.8 GB |
| 2. indexed stores | an instrument-bound read of a stored relation scanned the bar's whole block (48% of MW14): blocks are sorted by entity lazily and binary-searched | 20.0 s, 4.0 GB | 174 s, 7.9 GB | |
| 4. bounded memo | every derived result of every bar was kept: the memo is bucketed by time and the past beyond the program's lookback dropped (the latest of each as-of-read relation kept) | 21.8 s, 2.3 GB | 201 s, 3.3 GB | 8.4 s, 0.6 GB |
| 3. incremental rows windows | each call re-walked its window and materialised an environment per row: groups keep their last N rows, solve only new bars, and hand the aggregate its columns in the general path's order | 17.0 s | 146 s, 3.4 GB | 4.1 s |
| refinements | stored atoms bind from the block; an as-of read answered by the latest bar skips the walk; memo hits indexed by entity | 14.8 s, 2.3 GB | **145 s, 3.1 GB** | **3.9 s, 0.6 GB** |
| sorted windows (exact) | each call re-sorted its window into the general path's order (8% of MW14, 25% of R8L): a group keeps its rows sorted as they arrive and leave | 14.3 s | 140 s, 3.1 GB | 3.0 s, 0.6 GB |
| running sums (default) | rolling `mean`/`sum`/`std`/`cov`/`corr`/`ols_beta` in O(1) a bar from compensated, shifted sliding sums rebuilt every N rows; equal to rounding, not to the bit (`--window-sums exact` restores the two-pass formulas) | 14.3 s | **126 s, 3.1 GB** | **3.0 s, 0.6 GB** |

MW14 is 7.6x faster in a third of the memory; R8L 4.6x faster.

**Running sums and exactness.** The aggregate formulas themselves were never
the cost (0.2% of MW14): the gain of running sums is not building a
window's columns. With them MW14 still equals the canon on all 1,852 weeks;
R8L loses one leg of 21,892 (ZT, 2018-03-09: a last-bit difference in the
20-session ATR flips a threshold test), Sharpe 1.2782 against the book's
1.2777. The parity runs therefore use `--window-sums exact`. Studies
also evaluate grid points in parallel (`--threads`, default the cores up to
4): four `size_proxy` points on the SPX bundle take 14.8 s on two threads
against 24.1 s on one.

**What the profile shows now.** MW14's remaining time is the evaluator's
per-call overhead (an environment and a result vector per derivation, the
memo key, deallocation), spread over the recursive running products and
latches (`cumf`, `row`, `lp`, about a fifth) and the rolling features of
~4,240 names a week. The next levers, in order:

- **Columnar primitive stores** (plan step 5): typed column blocks instead of
  a `Vec<Value>` per tuple would cut the data's ~1.6 GB to ~0.4 GB and make a
  full-minute futures bundle fit.
- **Set-at-a-time evaluation of the cross-section**: evaluating a derived
  relation for all instruments of a bar in one pass over column blocks,
  instead of one demand-driven call per instrument, would remove most of the
  per-call overhead; it is a new evaluator, kept equal to the current one by
  the parity runs.
- **Parallel instruments within a bar**: once evaluation is set-at-a-time the
  per-instrument work is independent and can be split over threads.


**Correctness carries over to real data.** The path from the company's
Norgate lake to a tested bundle works:

- 756 point-in-time S&P 500 names, 2014–2026, pass all six bundle tests.
- The kernel and the independent pandas reference agree on every one of
  17,632 fills of `momentum_12_1`.
- The four canonical premiums land inside their published ranges, and the
  known 2020 low-volatility failure shows up.

It took two changes to the bundle tests and a reviewed exception list, all
recorded.

**Scale stops at the Russell 1000 on a 16 GB machine.** Run time and memory
are linear in the number of names. The limits are per-bar windowed
aggregation (time) and ~1.8 KB per price row in the CSV ingest (memory).
Full US daily needs the engine work in list A first.

**As a product it is a research CLI.** It has no service, users, data
feed, broker, scheduler or observability (list B). The data also has gaps
that no engine work fixes (list C).

## Measurements

Machine: Apple M4, 10 cores, 16 GB RAM, macOS 26.5.2. Rust 1.99, release
profile with thin LTO, one codegen unit and line tables. Commit `d2f08a9`
plus the bench fixes. Real data: point-in-time S&P 500 subsets (`--limit N`
by asset id), 3,207 daily bars from 2014-01-02 to 2026-10-02. Script:
`scripts/bench.sh ~/data/ndlake OUT`.

Ingest:

| Names | Price rows | Lake → CSV s | CSV → bundle s | Bundle build peak RSS | Bundle test s |
| ---: | ---: | ---: | ---: | ---: | ---: |
| 100 | 286,494 | 1.6 | 0.8 | 0.54 GB | 0.3 |
| 250 | 720,919 | 1.2 | 2.1 | 1.33 GB | 0.8 |
| 500 | 1,428,748 | 1.4 | 4.6 | 2.61 GB | 1.5 |
| 756 (S&P 500 PIT) | 2,075,269 | 2.1 | 7.2 | 3.68 GB | 2.1 |
| 1,800 (Russell 1000 PIT) | 4,353,924 | 2.3 | 19.9 | 7.65 GB | 4.5 |

Runs (batch kernel unless noted; `n` is a tenth of the names, at most 50,
or 20 for the long-only strategy):

| Names | Strategy | Kernel | Load s | Run s | Bars/s | Peak RSS MB |
| ---: | --- | --- | ---: | ---: | ---: | ---: |
| 100 | momentum_12_1 | batch | 0.23 | 13.5 | 237.9 | 1,049 |
| 100 | low_volatility | batch | 0.23 | 12.9 | 248.5 | 1,037 |
| 100 | total_return_momentum | batch | 0.23 | 17.6 | 182.4 | 1,248 |
| 250 | momentum_12_1 | batch | 0.50 | 36.4 | 88.2 | 2,485 |
| 250 | low_volatility | batch | 0.48 | 34.3 | 93.5 | 2,453 |
| 250 | total_return_momentum | batch | 0.49 | 47.5 | 67.5 | 3,301 |
| 500 | momentum_12_1 | batch | 0.92 | 75.1 | 42.7 | 4,912 |
| 500 | low_volatility | batch | 0.96 | 70.6 | 45.4 | 4,844 |
| 500 | total_return_momentum | batch | 0.97 | 97.4 | 32.9 | 6,449 |
| 500 | low_volatility | fold | 1.17 | 72.4 | 44.3 | 5,667 |
| 756 | momentum_12_1 | batch | 1.36 | 108.9 | 29.5 | 6,556 |
| 756 | low_volatility | batch | 1.57 | 101.7 | 31.5 | 7,013 |
| 756 | total_return_momentum | batch | 1.60 | 142.2 | 22.6 | 7,063 |
| 1,800 | momentum_12_1 (bundle untested†) | batch | 2.85 | 247.0 | 13.0 | 7,436 |

† The Russell 1000 bundle fails action reconciliation only on names
outside the reviewed S&P 500 exception list. It is a performance
measurement, not a vouched result.

Log-log slopes against names (100 to 756): run time 1.03–1.04, peak RSS
0.89–0.95. **Both are linear.** The fold kernel matches the batch kernel's
speed (72.4 against 70.6 s) at 17% more memory.

Where the time goes: windowed aggregation. `momentum_12_1` at 756 names makes
1.89M window calls that solve 408M window bars, about 216 per call against a
252-bar lookback. The per-bar row cache (`window_rows` in
`src/kernel/eval.rs`) saves re-solving a bar's rows, but each aggregate is
rebuilt over its whole window at every bar. `size_proxy` (a 60-day ADV) solves
1.6M window bars and runs in 6.9 s at 756 names; the 252-day strategies take
about 100 s. The O(universe) per-symbol scans in `bar_price`, `bar_volume` and
`call` are real but not dominant at these sizes: if they were, run time would
grow as N² and the slope would be near 2.

Where the memory goes:

- The history is held twice: the `Dataset` and the kernel stores. `size_proxy`,
  which has almost no window cache, peaks at 3.5 GB for 16.4M tuples, about
  216 B per tuple across both copies (~110 B each, as the handoff estimated).
- Runs with 252-day windows peak at 3.2–3.4 KB per price row: the window
  caches roughly double the footprint.
- The CSV adapter peaks at 1.8 KB per price row while building, which makes
  ingest the memory limit at the Russell 1000.

Not run, because RAM says stop:

| Universe since 2014 | Assets | Price rows | Projected build RSS | Projected run |
| --- | ---: | ---: | ---: | ---: |
| Russell 3000 PIT | 5,841 | 12.2M | ≈ 21 GB | ≈ 12 min, ≈ 20 GB |
| All lake equities (US, CA and AU) | 35,845 | 47.0M | ≈ 85 GB | ≈ 45 min, ≈ 80 GB |

## A. Engine gaps the measurements expose

| # | Gap | Size | Where | Evidence |
| --- | --- | :-: | --- | --- |
| A1 | **Incremental (sliding) windowed aggregation.** Keep running sums and counts, or a monotone deque for min/max, per group, instead of rebuilding each aggregate over its whole window every bar. The expected gain is about the window length (~100–250×) on the strategies that dominate run time. | L | `src/kernel/eval.rs` (`Literal::Agg`, `window_rows`), `src/kernel/fold.rs` (checkpointed windows) | ~216 bar-solves per window call; run time tracks lookback length (6.9 s for a 60-day window against ~100 s for 252-day) |
| A2 | **Columnar, compact stores.** Tuples are `Vec<Value>` on the heap at ~220 B each; one column of `f64` per relation and attribute, keyed by (time, symbol), would be ~16 B. | L | `src/kernel/mod.rs` (`Store`, `Dataset`), `src/bundle.rs` (`read_facts`) | ~216 B per tuple across two copies (3.5 GB for 16.4M tuples, `size_proxy`); 7 GB peak per run at 756 names |
| A3 | **One in-memory copy.** Drop or stream the `Dataset` once the kernel's stores are built; read bundle partitions straight into the stores. | M | `src/bin/abt.rs` (`load_dataset` and the runner), `src/kernel/mod.rs` (`Kernel::new`) | Code read: `load_dataset` keeps the `Dataset` alive for the whole run, next to the stores built from it |
| A4 | **Streaming, indexed ingest.** *Half done (Oct 6): inputs are Parquet read by column, the symbol history is indexed; the build is 1.0 KB per price row.* The adapters still hold every tuple before writing; stream the rows and write partitions as they fill. | M | `src/ingest.rs` (`read_csv`, `SymbolHistory::id_at`), `src/bundle.rs` (`write_bundle`) | 1.8 KB per price row at build: the Russell 1000 needs 7.65 GB; Russell 3000 projects to ~21 GB |
| A5 | **Symbol-indexed lookups.** `bar_price`, `bar_volume` and `call` on stored relations scan every tuple at a key, O(universe) each, so O(N²) per bar. Not dominant up to 1,800 names; it becomes so at full US (~15,000 names a bar). | M | `src/kernel/mod.rs` (`bar_price`, `bar_volume`), `src/kernel/eval.rs` (`call`) | Code read; slope 1.04 says not yet dominant |
| A6 | **O(history) executor bookkeeping.** The dividend scan walks `by_time.range(..=t)`, the whole dividend history, every bar; `fill` clones the bar list every bar. | S | `src/kernel/executor.rs` (`actions`, `fill`) | Code read |
| A7 | **Bounded retention and checkpoint size in the fold.** Windows and stores grow with history; checkpoints serialise them as JSON. | M | `src/kernel/fold.rs` | The fold ran at 17% more memory than batch; checkpoint size not measured this session |
| A8 | **Parallelism.** Everything is single-threaded. Grid points, walk-forward folds and independent strategies are embarrassingly parallel. | M | `src/study/mod.rs` (`run_study`) | 1 of 10 cores busy |
| A9 | **The synthetic generator breaks above a handful of names.** Drift is `0.0002 × (i − n/2)` and volatility `0.01 + 0.004 × i`, so at 100+ names prices collapse to zero within months. Bound both independently of `n` (this shifts seeded tests). | S | `src/data.rs` (`synthetic_daily`, `synthetic_daily_v2`) | `abt synth` at 100 × 2,500 writes zero closes; the synthetic bench was replaced by the real-data ladder |
| A10 | **Bundle test detail shows the first five problems only.** Reviewing real data needed `scripts/ingest/reconcile_report.py` to see all 403. Write the full list beside the manifest. | S | `src/bundle.rs` (`run_tests_with`) | 403 problems, 5 shown |
| A11 | **Spin-offs and same-day split plus dividend.** The catalog has no spin-off relation, so a vendor's cash-equivalent encoding is the only option. A dividend on a split's ex-date is paid on post-split shares, so the converter has to rescale it. | M | `corpus/env/equities_1d_v2.dsl`, `src/kernel/executor.rs` (`actions`) | HLT 2017, HON 2026, GOOGL 2014 and 15 more |

## B. Platform gaps, regardless of speed

| # | Gap | Size | Where |
| --- | --- | :-: | --- |
| B1 | No reference implementation of the minute execution contract or the feature library; `reference/` covers the daily contract only. | M | `reference/engine.py` |
| B2 | Spread and borrow are proxies (borrow by ADV bucket; no locate data); no observed rates. | M | `src/kernel/executor.rs` |
| B3 | No live data feed or broker adapter. | L | — |
| B4 | No service or UI: the CLI and files are the whole interface. | L | `src/bin/abt.rs` |
| B5 | No users, permissions or audit beyond the trial log. | M | `src/study/log.rs` |
| B6 | No scheduler: the lake's daily update does not trigger a bundle rebuild, test or rerun. | M | — |
| B7 | Trial logs are JSON files in a directory; there is no database and no concurrent writers. | M | `src/study/log.rs`, `src/study/lineage.rs` |
| B8 | No observability: no metrics or tracing, only the `--timing` line added this session. | S | `src/bin/abt.rs` |
| B9 | Not expressible: intraday execution styles, limit and stop orders, optimiser weights, non-equity instruments (the company's Databento data is futures), fundamentals (in the lake but not in the environment). | L | `corpus/env/`, `src/kernel/executor.rs` |

## C. Data gaps, regardless of the engine

| # | Gap | Effect | Size of the fix |
| --- | --- | --- | :-: |
| C1 | No delisting reasons in Norgate | Inferred from the last prints (12 bankruptcy, 1 other, 138 acquisition in the S&P set). AABA is misclassified as a total loss. | S with a vendor field, M otherwise |
| C2 | No dividend announcement or pay dates | announce = pay = ex. `dividend_capture` cannot fire. Cash is credited weeks early. | M (another vendor) |
| C3 | No ticker history (final ticker applied retroactively) | Ticker literals resolve only current names (W6). | S (Norgate's symbol history, if exported) |
| C4 | Classification is one 2026 snapshot | No point-in-time sectors before 2026-09-21. | M |
| C5 | No US equity minute bars (the Databento bucket is futures) | No `equities_1m` bundle and no resampled-versus-Norgate reconciliation. | L (a Databento equities subscription) |
| C6 | Real data errors kept | TFCF/TFCFA 2019-03-19 (the Fox separation as a split). SNDK 2025-02-13 and MRNA 2026-08-19 are unverified. | S each |
