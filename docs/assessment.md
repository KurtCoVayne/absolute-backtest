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


## Data layer budgets (D-9.1), measured at M-2, Oct 15, 2026

The data layer of `docs/spec` (`crates/abt-data`) measured against the
budgets of D-9.1 at the end of migration step M-2. These numbers are not the
reference machine's: they come from a 4-core Intel Xeon at 2.8 GHz with
15 GB in a cloud container, in a release build. G10 asserts the budgets on
the owner's M4, after M-8.

The measurements come from `crates/abt-data/tests/budgets.rs`, an ignored
test, run with
`cargo test -p abt-data --release --test budgets -- --ignored --nocapture`.
The data:

- 2,000 entities × 2,520 days of one numeric column, synthetic.
- Ingested as ten yearly versions into monthly partitions.
- Every query read from disk. M-2 has no resident layer: mmap, the CSR
  index and the resident LRU come at M-8.

| Operation | D-9.1 bound | Measured | |
| --- | --- | ---: | --- |
| Ingest | ≥ 200,000 rows/s | 2.70M rows/s | within, 13× |
| Footprint | ≤ 16 B per value resident | 3.05 B per value **on disk** | resident not measurable yet |
| `point` | ≤ 50 µs from disk | 847 µs median | 17× over |
| `latest` | ≤ 50 µs from disk | 810 µs median | 16× over |
| `series` of 252 rows | < 100 µs resident | 6.25 ms median from disk, 12 partitions | not comparable: from disk |
| `section` | < 5 ms at 10,000 entities from a resident partition | 1.76 ms median at 2,000 entities from disk | |
| Rolling 252-row mean, 10,000 × 7,560 | < 10 s on 1 thread, < 2 s on 8 | about 94 s on 1 thread, 80 s on 8 (`running`); 130 s and 89 s (`two_pass`) | 9–45× over |

The rolling rows are scaled linearly from 2,000 × 2,520 (5.04M rows). That
size took 6.3 s on one thread and 5.4 s on eight with `running`, and 8.5 s
and 5.9 s with `two_pass`. "Eight threads" is eight threads on four cores.
The barrier latency is a live budget and waits for M-6.

**Where the time goes.** At M-2 `latest` read every partition from the start
of the data up to `t_max`, and took 27 ms. It now reads back from `t_max` and
stops at the first bucket that holds the entity's rows, when the identity
fixes the fact's key. That leaves `point` and `latest` dominated by one
partition's open: the manifest's JSON, the Parquet footer, the offset index
and one row group's decode. A resident layer and mmap (M-8) remove all of
that.

The aggregate pass on 5.04M rows with `running` on one thread spends:

| Stage | Share | Notes |
| --- | ---: | --- |
| Writing the answer as a cache object | 42% (2.7 s) | through the general `put` path: a bucket string formatted per row, a lexicographic sort of rows already in order, deduplication, the log and zstd Parquet |
| The windows themselves | 23% (1.5 s) | 3.5 s with `two_pass`; the only stage that is parallel, 3.4× on four cores |
| The output's sort into (t, entity) | 13% (0.8 s) | a comparison sort where a counting sort over the labels would do |
| Reading and grouping the rows | 16% (1.1 s) | |
| Building the answer batch | 5% (0.3 s) | |

The pass is therefore bounded by its serial stages, not by the arithmetic.
M-8's list:

- a put path for rows already sorted;
- bucket keys computed once per bucket;
- a counting sort of the output;
- columnar grouping instead of a map of row vectors.

**At M-8 (Oct 17): the resident layer.** Reads go through the resident
cache of decoded row groups, with manifests read once per version
(ERRATA E-74). Same data, same machine:

| Operation | D-9.1 bound | From disk | Resident | |
| --- | --- | ---: | ---: | --- |
| `point` | ≤ 50 µs | 721 µs | 40 µs | within |
| `latest` | ≤ 50 µs | 689 µs | 41 µs | within |
| `series` of 252 rows | < 100 µs resident | 6.1 ms | 158 µs | 1.6× over |
| `section` | < 5 ms at 10,000 names | 1.6 ms at 2,000 | 303 µs at 2,000 | within, scaled |
| Resident footprint | ≤ 16 B per value | | 8 B per value | within |

M-2's list for the aggregate pass is partly done: dense slots, a columnar
answer in (t, group) order without a sort, bucket names once per span, and
no sort of rows already sorted. The rolling mean of 5.04M rows on eight
threads now takes 4.7 s (`running`), from 5.6 s, and 5.3 s (`two_pass`),
from 6.1 s. Of that, 1.3 s is the answer's log segment, written at this
VM's 190 MB/s for fresh pages, and about 1 s is the Parquet encoding.

**At M-8 (Oct 17): the kernel's evaluator.** `momentum_12_1` over 100
synthetic names and 2,520 days, in release on the same machine. The table
gives the kernel's run time (`--timing`'s `run_s`) and the window bars it
solves:

| Kernel | Set evaluator | Tuple evaluator | Window bars solved |
| --- | ---: | ---: | ---: |
| M-8a | 43.2 s | 43.7 s | 62.7M |
| `dividend_at`'s rows served from the window cache (ERRATA E-75) | 19.1 s | | 0.98M |
| the cache read in order into the aggregate's columns, the literal analysed once | 15.0 s | 15.1 s | 0.98M |

The cache was the gain. `dividend_at` tests `Ex = T` inside a year's
window, which made every call's rows its own: each call solved its whole
window again.

The set evaluator itself gains little here. The time is in two places:

- **The window's per-bar solves.** They are nested in the aggregation and
  still one environment at a time (E-75).
- **The stream's insertion into the stored tables.** Each tuple was
  checked against its bar's whole block for a revision. That was 12% of the
  kernel's time at 200 names, and it grew with the square of the names.
  M-8b searches only the rows received at earlier instants: at 300 names
  the run takes 42.1 s, from 52.2 s.

The whole corpus was also timed: the 24 programs and the two books, each on
its seeded market, the faster of three runs (`tests/eval_set.rs`). It
takes 11.4 s with the tuple evaluator and 11.2 s with the set evaluator.
Those runs are short, 0.26 s to 1.2 s each, and mostly data setup. The
longest, `r8l`, takes 1.23 s against 1.05 s.

**Memory at M-8.** The same run peaks at 878 MB at 100 names and 2.6 GB at
300. Valgrind's massif, on a smaller run, puts the live heap at about a
third of that:

- the stored tuples, about 80 B each;
- the stream's batches in flight, bounded by its flow-control window. Each
  batch row carries every relation's columns, mostly null;
- the data layer's resident cache, about 45 MB here.

The rest is glibc's per-thread arenas. The data service allocates on its
threads what the kernel frees on its own:

| Allocator | Run time | Peak |
| --- | ---: | ---: |
| glibc, default arenas | 14.8 s | 878 MB |
| glibc, `MALLOC_ARENA_MAX=1` | 15.7 s | 649 MB |
| glibc, `MALLOC_ARENA_MAX=2` | 15.2 s | 781 MB |
| jemalloc (`LD_PRELOAD`) | 14.9 s | 763 MB |

The owner's machine runs macOS, whose allocator is not glibc's, so nothing
is changed for Linux alone. A Linux run can set `MALLOC_ARENA_MAX`.

M-8b then stores a key's tuples row-major in one buffer, with no allocation
per tuple (ERRATA E-76). Measured back to back with the M-8c binary (at 100
names the faster of two runs):

| Names | M-8c | M-8b |
| ---: | ---: | ---: |
| 100 | 14.5 s, 957 MB | 13.8 s, 791 MB |
| 300 | 54.2 s, 2.77 GB | 44.0 s, 2.21 GB |

The rows still hold 16 B values with the key's time in each. 3.1's typed
columns would take a close to 12 B.

**At M-8: the targets of A-7.3.** `scripts/bench.sh none OUT` runs the
synthetic ladder and the data layer's budgets here; with the lake and the
books' stores it runs the rest on the owner's machine:

| Target | Value | Measured here | |
| --- | --- | --- | --- |
| Sustained ingest | 200,000 rows/s | 2.93M rows/s | within |
| Cross-section of one bar | < 1 ms | 308 µs at 2,000 names, resident | within |
| Point lookup | O(log n) | 39 µs, resident | |
| Storage per stored value | ≤ 16 B resident, ≤ 4 B on disk | 10.0 B resident, 3.05 B on disk | within |
| MW14 full replay | ≤ 60 s, ≤ 1.5 GB | the owner's data | |
| R8L full replay | ≤ 3 s, ≤ 0.6 GB | the owner's data | |
| Barrier to decision, live | < 50 ms | a live feed | |

The synthetic ladder runs `momentum_12_1` with n a tenth of the names:
14.8 s and 0.80 GB at 100 names, 46.5 s and 2.35 GB at 300. The log-log
slopes against names are 1.04 for run time and 0.98 for memory: both
linear.

**Correctness carries over to real data.** The path from the company's
Norgate lake to a tested bundle works:

- 756 point-in-time S&P 500 names, 2014–2026, pass all six bundle tests.
- The kernel and the independent pandas reference agree on every one of
  17,632 fills of `momentum_12_1`.
- The four canonical premiums land inside their published ranges, and the
  known 2020 low-volatility failure shows up.

It took two changes to the bundle tests and a reviewed exception list, all
recorded.

**At M-8 (Nov 5): entity-parallel evaluation between barriers (F-5.3,
ERRATA E-125).** `--threads N` cuts every entity-local rule's bindings across
`N` worker kernels after its first literal (`kernel/parallel.rs`), bit for
bit the one-thread run: `tests/eval_set.rs` holds the tuple evaluator, the
set evaluator and the set evaluator on two and four workers equal on every
corpus program and both books, decisions and the ledger of every derived
relation at every bar. Measured on an AWS `m8g.xlarge` (Graviton4, 4 vCPU,
16 GB, Rust 1.99.1, release), `mw14` on the synthetic daily market
(`--synthetic --days 2520`, 504 weekly bars), the base being this tree
before the change:

| Names | base | 1 thread | 2 threads | 4 threads | inside the cut steps, 4 threads |
| ---: | ---: | ---: | ---: | ---: | ---: |
| 300 | 22.9 s | 23.4 s | 23.1 s | 23.9 s (CPU 32.6 s) | 6.5 s of 23.9 |
| 1,000 | 95.8 s | 96.3 s | 94.0 s | 93.8 s (CPU 122.9 s) | 16.3 s of 93.8 |

The cut works and costs nothing at one thread (+0.6%), and it does not
move the wall clock: the rule bodies it cuts are 17% of the run at 1,000
names (`parallel_secs`), the workers spend their CPU (123 s for 94 s of
wall) and Amdahl keeps the rest. `perf` on the 300-name run at one thread
puts the time in the data path and the allocator, not the rule bodies: the
synthetic market's ingest and staging (`ingest_keyed_with`,
`staging::build`, `ladder::build`) 7%, `malloc`/`free` 15%, `memcmp` 4%,
the exact decimal arithmetic (`abt_agg::exact::Wide::divrem`) 3%, the
stream and the hot store (`Stream::next_batch`, `Cumulative::plus`,
`HotVersion::read_at`) 5%, the evaluator's own instance work
(`atom_pattern`, `step`, `term_value`, `push_env`, `rows_columns`,
`Value::cmp`) under 10% in all. The next levers are therefore the stream
and store path the books' runs found (open question 7 of
`docs/iterations/01-foundation.md`) and the per-instance allocations, and
the spawn per cut step (a persistent worker pool) once the cut is worth
more than it costs. The corpus's five-name markets gain nothing (3.1 s
total at one thread, 5.1 s at four before the per-worker row floor, since
no rule reaches `parallel_min_rows`); `--threads` stays `1` by default.

**At M-8 (Nov 6): the full MW14 finishes on this architecture.** The retention
horizon (E-126) and the stream projection's first step (E-127) on the same
`m8g.xlarge` (4 vCPU, 16 GB), the lake staged through the Norgate adapter in
five-year chunks with the book's own `SPXTR@1d` and `VIX@1w` as series bundles
(`docs/parity-mw14.md` now records the recipe): the data service holds the store
at 3.8 GB; the front, `abt run --strategy mw14 --data SOCK --history window
--start 1991-01-04 --end 2026-07-02`, runs the 1,904 weekly bars over 4,240
names in **377 s** at **3.7 GB** resident (367 s with the three increments of 3.2 below, the result byte for byte the same), having dropped 59.2M stored rows
behind the horizon (iteration 1's run of the same book was killed by the
container at 443 s with the front past 8.5 GB and still growing). `--history
all` on the same box thrashes and does not finish: the trim is what makes the
run fit. The run is not at parity through this data path: against the canon,
weekly correlation 0.9777, CAGR 18.5% against 20.9%, Sharpe 0.959 against
1.020, maximum drawdown −25.9% against −26.2%, 25% of the weeks equal to 1e-4
-- the old engine reproduces the canon exactly from the v1 bundle
(`docs/parity-mw14.md`); no full-size run of the three-layer kernel on that
bundle is recorded, so the kernel and the adapter's weekly views of the daily
lake are not yet separated as causes of the gap (the owner's open question 7;
package 5 of `docs/handoff/roadmap.md`). The ledgers of the two
observed relations are written at the barriers under trimming (E-126); on a
served synthetic market of 1,000 names both histories give the same events,
book, fills, decisions and ledgers.

The run's own clock (`--timing` with `ABT_TIMING_RELS`, the seconds per
derived relation and per phase of the fold): derivations 200 s of the 367 --
`clenow` 38, `dvol` 30, `eligible` 17, `ma` 16 (the four per-instrument
windows, 100 s), `dollar_volume@base` 14 (the daily relation `dvol` buckets,
derived whole at 8,934 daily bars), `prev_row` 12, `dividend_at` 12,
`traded@weekly` 11 -- the executor's fills and marks 54 s, the memo's
eviction 17 s, the tuples' insertion 10 s, and some 75 s waiting on the
stream of daily rows. Three increments of 3.2 are in (E-131): a positive
atom over a block in entity order joined by one merge pass, a rows window's
conjunction solved once for the whole block at the bar, the same for bucket
and span windows; each held equal to the demand-driven evaluator by
`tests/eval_set.rs`; together 27.8 to 23.1 CPU-seconds on a served store of
1,000 synthetic names and 377 to 367 s on the book. What they do not reach
is the per-row cost itself -- a `Value` per cell, an environment per
instance, a hash lookup per derived atom -- which is the column-block
evaluator's to remove.

`perf` on this run (frame-pointer build, 299 Hz) is evaluator-bound: `Value::cmp`
6.6%, `malloc`/`free` 14%, the result-narrowing closure of `call` 4.2%,
`atom_pattern` 4.2%, `rows_columns` 4.2%, `Memo::get` 3.8%, `Value::clone`
2.6%, the domain-range collection 2.0%, the block's `push_env` 1.9%; no data
layer symbol reaches 1%. The 15× between this and the Python book's 25 s is
therefore the tuple-at-a-time evaluator and its allocations, not the stream
-- the column-block evaluator and the window states of 3.2 are the next
step, and the aggregate pushdown of `dvol` and `traded@weekly` (E-127) is a
memory lever before it is a time lever.

(Nov 24: that reading rests on the profile, which samples the front's CPU.
It cannot see the 75 s of the same run's clock that the front spends
waiting on the stream, nor the data service's own work. The clock groups
up to 140 s on the data path -- the wait, the insertion, and the three
derivations that only bucket daily rows into weeks -- and 216 s in the
evaluator and the executor (`docs/handoff/status.md` section 3). Which
reading is right is measured, not argued: the benchmark of
`docs/handoff/roadmap.md` package 1 records the data service's CPU time
beside the front's wait.)

**Package 1 (Nov 25): the benchmark is a script, and it reproduces Nov 6.**
`scripts/bench-mw14.sh LAKE SPXTR VIX CANON N` stages its own store (keyed by
a hash of the lake, the series and the code that writes a store), runs N
cold repetitions, each on a fresh copy of that store with its own data
service, compares each with the canon and prints one JSON line. On
`m8-bench2` (`m8g.xlarge`, Rust 1.99.1), staging takes 20 minutes and a
data service is serving 39 s after it starts (the store's open; not in the
run's seconds below).

| | Runs | Run s | Front CPU s | Front peak | Service CPU s | Decisions | Weekly correlation |
| --- | ---: | --- | ---: | ---: | ---: | ---: | ---: |
| The Nov 6 commit's binary (`2040b40`) on this store | 2 | 372.0, 372.3 | 279.4 | 3.51 GB | 139.7 | 6,455 | 0.977724 |
| The merged head (`1b3bf73`) | 5 | 379.4 mean (373.5 to 383.3, s.d. 3.3) | 288.3 | 3.51 GB | 137.3 | 6,481 | 0.977595 |
| The head with `Tuple` as a `SmallVec` | 4 | 302.3 mean (301.2 to 303.7) | 210.9 | 3.51 GB | 137.6 | 6,481 | 0.977595 |
| The head with mimalloc (a trial, not in the tree) | 2 | 313.1, 318.2 | 222.1 | 4.42, 3.80 GB | 136.3 | 6,481 | 0.977595 |
| `SmallVec` and mimalloc (a trial) | 2 | 270.5, 272.4 | 179.0 | 4.43, 3.80 GB | 132.8 | 6,481 | 0.977595 |

The Nov 6 binary gives the Nov 6 record again: 372 s against 367 s (one
run then), the same correlation of 0.9777. The head is 7 s slower (2%)
and decides 26 times more. Between the two are package 0's fixes: E-130
(an atom's slot map computed at each use, where the cache served one atom
another's map) and E-132 (the as-of memo keyed by the whole pattern,
which is what changes the decisions); the 7 s are not divided between
them. Package 1's gate reads "five repetitions reproduce 367 s within 3%":
the Nov 6 code does (+1.4%), and the head's mean is 3.4% over it because
the head is no longer that code. Its five repetitions write the same 1,851 weekly returns to the last bit (one
SHA-256 over them, `compare.abt_sha256` in the script's line). Against the
canon: 464 weeks equal to 1e-4, 226 to 1e-9, 196 to 8e-15; CAGR 18.54%
against 20.91%, Sharpe 0.962 against 1.020, maximum drawdown −25.90%
against −26.18%.

*Where the 379 s go* (the mean of the five; the run's clock now covers the
whole run, `phases` under `ABT_TIMING_RELS=1`):

| Before the stream | s | In the stream loop | s |
| --- | ---: | --- | ---: |
| `bind`: the resolver, then the binding's reads of the store | 55.3 | `decide`: derivations the decisions demand | 149.7 |
| `check`: the preconditions | 37.4 | `fill`: derivations the executor demands (prices, marks) | 56.7 |
| `push`: the pushed aggregate (`close@weekly`) | 7.5 | `evict`: the memo behind the horizon | 17.6 |
| `open`: the subscription | 0.0 | `decode`: batches into tuples | 17.1 |
| | | `insert`: tuples into the stores | 9.2 |
| | | `trim`: stored rows behind the horizon | 8.9 |
| | | `warm`: inputs derived before a parallel cut | 3.4 |
| | | `wait`: blocked on the next batch | 0.9 |
| | | in no phase | 15.6 |
| **Total** | **100.2** | **Total** (`stream`) | **279.1** |

Writing the outputs takes 0.2 s. This settles the question the Nov 24
note left open, and neither reading was right. The "75 s waiting on the
stream" of Nov 6 is not a wait on the stream: the front waits 0.9 s for
batches in the whole run, so the data service is never what the fold is
waiting for. It is 100 s *before the first tuple*: `bind_at` asks the data
layer for the distinct times of every bound base relation and the distinct
labels of every label column, each a pass over a relation of 13 to 16
million rows, and the preconditions read `close`, `listed` and `member`
whole into a row object apiece. Neither needs the rows: this is the
cheapest quarter of the run to remove. The rest of the data path is the
35 s that move rows (decode, insert, trim) and the 57 s of the three
derivations that only bucket daily rows (`dvol` 31.5, `dollar_volume@base`
14.0, `traded@weekly` 11.3): 192 s in all, against 187 s of evaluation
proper.

The stream delivers 61,508,345 tuples: `close`, `volume` and `listed`
15,691,165 each, `member` 12,868,490, `delisted` 1,404,787, `dividend`
145,460, `value` 11,624, `split` 4,489; 59,173,737 are dropped behind the
horizon. No rule of the book names `listed`. Seconds per derived relation
(194.7 of the 209.3 in derivations): `clenow` 40.1, `dvol` 31.5,
`eligible` 17.5, `ma` 17.2, `dollar_volume@base` 14.0, `prev_row` 13.4,
`dividend_at` 12.2, `traded@weekly` 11.3, `total_return` 6.9, `tr_index`
4.4, `cum_split` 4.1, `split_factor` 3.5, `dividend_or_zero` 3.4,
`row_number` 2.9, `adj_close` 2.9, `universe` 2.8, `stay` 2.4, `extension`
2.2, `member@weekly` 1.9.

*The profile* (`[profile.profiling]`, frame pointers, 299 Hz, one
repetition under `perf` through the script's `WRAP`; it ran in 379.9 s,
the release build's time). **The allocator is a third of the front's
CPU**: `malloc` with what it calls 22.7%, `free` 9.1%, `realloc` 3.4%
(`_int_malloc` alone 16.6%, with `unlink_chunk` and `malloc_consolidate`:
glibc's slow paths). Nov 6 read 14%. After it: `solve_kleene_block` 2.8%
of its own, `Block::push_env` 2.8%, a closure of `rows_columns` 2.7%,
`Fold::step` 2.5%, `Value::clone` 2.2%, `Value::cmp` 1.8%, the collection
of a domain's range in the as-of walk 1.8%. By callers: `Kernel::call`
71.6%, the decisions 51.1%, the executor's prices 19.4% (all of it
derivation behind `bar_price`), `rows_columns` 14.1%,
`prefetch_bucket_window` 8.5%, `evict_memo` 6.0%, `Decoder::events` 5.9%,
`trim_stores` 4.2%, the preconditions 3.7%.

*The `SmallVec` experiment of Nov 6 is kept.* The handoff expected to drop
it. Measured with the release binary against the same store, two runs of
the patch on the head and two of the gated tree: 302.3 s against 379.4 s,
77 s less (20%), the same returns to the last bit. The
stream loop falls from 279 to 201 s: `decide` 150 to 118, `fill` 57 to
44, `evict` 17.6 to 5.8, `decode` 17.1 to 9.6, `insert` 9.2 to 6.7, `trim`
8.9 to 4.5; the front's CPU from 288 to 210 s. `bind` and `check` do not
move (55 and 37 s): they build no tuples. A tuple of up
to its inline length no longer allocates, which is where a third of the
CPU was going.

*The allocator, a trial.* With mimalloc as the `abt` binary's global
allocator (a `#[global_allocator]` static in `crates/abt/src/main.rs`, the
crate built from its C source; the change is in no branch of the
repository):
315.6 s alone, 271.5 s on top of the `SmallVec`, the same returns to the
last bit in all four runs. It is 17% alone and 10% after the `SmallVec`
has taken the tuples' share, and it costs memory: the front's peak goes
from 3.51 GB to 3.80 GB in one repetition and 4.43 GB in the other of
each pair. The note under the allocator table above (nothing is changed
for Linux alone) stands until the owner decides: it is a new dependency, and
of the two gates of package 3 (60 s and 1.5 GB) the memory one is the
nearer. The share it removes also shrinks as packages 2 and 3 stop
building a tuple per row.

*The synthetic cell* (1,000 names, 2,520 days, `--history window`, no
store; `docs/handoff/benchmark.md`): 96.9 and 96.5 s at the head, 3.86 GB
peak, 25.2M tuples (Nov 5: 95.8 s); 88.1 and 87.9 s and 3.58 GB with the
`SmallVec`; 87.9 and 87.3 s and 3.85 GB with mimalloc. The cell
understates the book: 9% there for either change, 20% and 17% on the real
run.

**Package 2, first part (Nov 25): 100 s before the first tuple become 14 s,
and the front's peak falls from 3.5 GB to 1.0 GB.** The record above put 100
s of the run before the stream opens: `bind` 55, the preconditions 37, the
pushed aggregate 7.5. Each was a pass over relations of 13 to 16 million
rows to answer a question about their columns. Now (E-138, E-139):
`bind`'s distinct times and labels are read from a hot version's label
index and from one Parquet column, with row groups skipped by their
statistics; the preconditions read named columns through a visitor instead
of a row object per row (the 3.5 GB peak was those objects), a comparison
no value can pass (`price <= 0`) is answered from the blocks' and row
groups' least values without a row read, and the listing is checked against
the membership in one pass; a hot read with appended rows is ordered by two
counting passes instead of a sort; the decoder resolves a batch's
relations once and converts its cells by type. Every kernel call on the
data layer is clocked under `ABT_TIMING_RELS` (`data <call> <relation>
<detail> rows= secs=` lines, and a total per stage).

The machine ran this evening about 10% slower than the night before, with
nothing else on it: the tree of the record above took 331.6 s at 20:13 UTC
against 302.3 s at night, with the same fingerprint. The comparison is
within the evening:

| | Runs | Run s | `bind` | `check` | `push` | Front peak | Service CPU s |
| --- | ---: | --- | ---: | ---: | ---: | ---: | ---: |
| The record's tree (`910b298`) | 1 | 331.6 | 56.6 | 39.7 | 7.9 | 3.51 GB | 142.0 |
| With this part | 2 | 235.7, 237.1 | 3.5, 3.6 | 6.0, 5.5 | 4.7, 4.4 | 1.01 GB | 60.0 |

The same weekly returns to the last bit (`c086a539…`). Of `bind`'s 3.5 s
the slowest calls are the membership's distinct list (0.8 s), the listing's
distinct times (0.7 s) and the dividends' (0.6 s); of `check`'s 5.5 to 6.0
s, `close` read whole as columns (2.4 s), the membership (1.1 s) and the
listing (1.0 s). The data service's CPU falls from 142 to 60 s. What remains
of the run is the stream loop (221 s: `decide` 138, `fill` 49), which the
column-block evaluator and the delivered answers of package 2's second part
take next.

**Package 3, the row path's quick wins (Nov 25): the stream loop from 221 s
to 171 s, the same returns.** Units S01 to S03 of the evaluator's plan,
which change today's evaluator and none of its answers. S01: the as-of walk
probes the first bar and steps back one label at a time instead of
collecting the relation's whole domain at every call (`prev_row` 11.9 s to
3.7 s); a merge join binds in place instead of copying a relation's
tuples; a parameter is resolved at build; a rows window's anchor is solved
once and its prefetched rows are taken, not cloned twice; an aggregate's
function is parsed once. S02: the memo keeps a chained relation's last
evicted result whole (`Memo::latest_block`) and spills only the entities
that left it (`evict` 6.9 s to 0.8 s); a store's kept rows are merged, not
rebuilt and sorted at every barrier (`trim` 5.4 s to 1.9 s). S03's door:
the listing leaves the stream when no rule, observation or after-run
command reads it (E-141: 15.69M tuples fewer); the fold's late test is one
comparison with the last decided label; a run of rows arriving in
identity order is inserted without hashing (`insert` 6.9 s to 3.7 s); the
phases line gains `build`, `spec`, `inv`, `observe`, `post` and
`loop_other`, and the relation table `stuck_bars` and `small_bars` a
relation.

On top of package 2's first part, the same evening: 185.0 and 185.0 s
against 235.7 and 237.1 s without these units and 331.6 s for package 1's
tree; the front's CPU 175 s, its peak 0.97 GB; 45.8M tuples; the same
weekly returns to the last bit. What is left: `decide` 118 s and `fill` 38
s, the derivations themselves (`clenow` 34.5, `dvol` 28.6, `ma` 15.6,
`eligible` 13.0, `dividend_at` 12.4, `traded@weekly` 10.9,
`dollar_volume@base` 8.0). These are the column-block evaluator's and the
delivered answers' (package 2's second part).

**Package 5, parity through the adapter (Nov 25): the weekly correlation from
0.9776 to 0.99974, 1,825 of 1,851 weeks equal to 1e-4.** The first
measurements against a finished run (`docs/handoff/` does not hold them;
the counts below are the measuring agent's, from the store and a pandas
replica of the book that equals the canon in all 1,852 weeks) found twelve
differences and ranked them; the largest was not on the desk list. Fixed,
each with a test that failed first (E-144 to E-149):

| | What was wrong | Weeks it moved beyond 1e-4 (the replica, switched alone) |
| --- | --- | ---: |
| E-144 | A library rule read a daily bar at an earlier weekly label as a point read of that day: a name with no bar on a label day lost its price chain for good (10.4% of rows unpriced) | 1,069 |
| E-146 | Every name's rows began at the run's first bar under `--history window`: the windows filled during 1991; the run now starts in 1990 and the book trades from 1991-01-04, its gate from a staged weekly index from 1988 | 162 |
| E-145 | `dividend_at` counted a dividend only when its ex-date was the week's label (17% of them) | 386 |
| E-149 | The adapter kept an asset only in the five-year spans where it was a member (4.4M bars missing), ended membership a day early, rounded the lake's values | 192 and 113 |
| E-147 | The weekly VIX was keyed at the Monday plus four days: stale in holiday weeks | 22 |
| E-148 | The target weight's arithmetic | (last bits) |

On `m8-bench2`, the store staged with the new adapter options (4,319
instruments a span, 20.1M closes against 15.7M), two runs:

| | Weekly correlation | Weeks equal to 1e-4 | to 8e-15 | CAGR | Sharpe | Max drawdown |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| Before (`1b3bf73`) | 0.977595 | 464 | 196 | 18.54% | 0.962 | −25.90% |
| E-144 alone | 0.987384 | 763 | 205 | 20.08% | 0.977 | −26.39% |
| All of package 5 so far | **0.999737** | **1,825** | 479 | 21.03% | 1.024 | −26.19% |
| The canon | 1 | 1,851 | 1,851 | 20.91% | 1.020 | −26.18% |

The 52 weeks of 1990 trade nothing (the sum of their returns is 0), and
every run label but the last is its weekly series' key (the last week,
2026-06-24, is the data's partial week, E-147). The run costs 336.9 and
335.5 s against 185 s for the same code at the old data: 28% more rows, 52
more weeks, a book that now holds its names (88,874 decisions against
6,481), and 75 s in the two relations of the membership test (`sp1500`
46.6 s, `sp1500_day` 28.7 s, a daily relation at every bar). Open: the 26
weeks beyond 1e-4 and the 1,346 between 1e-4 and 8e-15 (the weekly total
return is one period on the week's closes where the book compounds each
day's dividend; the split-day dividend, D7, which the adapter, the library
and the book compute three ways; E-134), and a cheaper membership test.

**Package 5, second part (Nov 25): the membership test from 76.5 s to about 3
s, and the residual explained.** E-160: `eligible` reads `sp1500@base` at
the name's own last bar of the week, the key of its latest daily close as
of the label (`close@base(A, L, _) asof T`), instead of a daily relation
derived at every bar; the same answers, the same fingerprint
(`6e72c9bb…`), the run 337.2 s to 251.6 s on the parity store. The 26
weeks beyond 1e-4 and the 1,346 between 1e-4 and 8e-15 were then measured
against the replica with one switch per mechanism, added in turn: five
switches reproduce the run in all 1,851 weeks to 8e-15.

| Mechanism | Weeks beyond 1e-4 | Between 1e-4 and 8e-15 |
| --- | ---: | ---: |
| The weekly total return is one period on the week's closes, `(P·F + D)/P0`; the book compounds each day, `(c + D)·s/c₋₁` | 24 | 1,235 |
| The split-day dividend (D7): the store's `D/F` added to `P·F`, where the book takes `(P + D)·F` | 0 | 3 |
| Two dividends of one name in one week: the weekly read keeps the latest (349 asset-weeks) | 0 | 5 |
| A delisting is applied at the bar of the week that holds its session; the book drops the name at no cost the week after | 0 | 16 |
| The name is bought again after that exit (it still has a row that week) and can never be sold | 2 | 87 |

Measured and changing no week: E-134's restarts (25), halts, the
membership's edge cases. The 105 names stuck after a re-buy (95) or
delisted with no delisting event (10) make 83,830 expired exit orders,
94% of the run's 88,874 decisions, at no cost to the returns but not to
the clock. What each needs: the library's total return as the book's
product over the week's dividends, `(P·F/P0)·Π(1 + D_i·F_i/Pex_i) − 1`,
one close read a dividend (a product aggregate, and the next fix); a
window over an event relation on a coarser calendar that returns every
tuple of the bucket; the executor applying a delisting at the first
decision bar whose week has no print of the name, and expiring orders on
a delisted name.

**Package 3, the column-block evaluator's first operators (Nov 26): the
parity run from 249 s to 180 s, the same returns.** Units S05, S06, S07,
S09 and S10 of the evaluator's plan (E-150, E-151). `--evaluator block`:
on a memo miss `Kernel::call` asks the relation's compiled plan
(`kernel/colplan.rs`) to derive the whole cross-section at the bar as
columns (`kernel/col.rs`, `ColBlock`, a `BlockMemo` inside the memo), and
falls back to the rules where the plan declines (a partial operation, a
read behind the horizon, a block that is not one row an entity). The
operators: a scan, a join, a binding and a test over columns with one
arithmetic shared with the row path (`kernel/cexpr.rs`); a pushed
aggregate's answer; a rows window fed per entity from columns
(`kernel/colops.rs`, over S05's per-entity window state, which is on by
default for the row path too). Planned on MW14: `adj_close`,
`close@weekly`, `dollar_volume@base`, `scored`, `stay`, `latched`,
`extension`, `in_book`, `ma`, `clenow`. `eligible` is not: since E-160 its
body holds an as-of read no operator plans yet.

On the benchmark of package 1's store (no parity changes), before and
after: `clenow` 34.8 s to 6.0, `ma` 15.7 to 2.2, `eligible` 13.1 to 2.2,
the run 280 s to 206 s (a tree without package 2's first part). On the
parity benchmark, on top of package 5's second part: 249 s to 180.2 s,
`clenow` 9.1 s, `ma` 3.2, `eligible` 13.2 (row path); the fingerprint
`6e72c9bb…` unchanged; the stored counters equal to the set evaluator's.
What the parity run still derives row by row: `dvol` 39.7 and
`traded@weekly` 15.7 (delivered by package 2's second part),
`dividend_at` 16.9, `eligible` 13.2, `total_return` 5.7, `prev_row` 4.5,
`tr_index` 4.4, `cum_split` 4.3, `split_factor` 3.8, `row_number` 3.7,
`dividend_or_zero` 3.6, and `fill` 59.7 s (the executor's reads of
`tr_index@weekly`): S11 to S14.

**Package 2, second part (Nov 26): weekly answers computed by the data
service and delivered in the stream; the parity run from 180 s to 134 s,
the same returns.** E-155 to E-159, E-175. Protocol 1.3 (W1) carries the
fields: a factor relation, an instrument filter, an answer asked without
its table, and the answer's version, row count and count of inexact rows.
The service's pass (D1) computes a count as rows, a mean of a product of
two relations (`dvol`'s close times volume) and an instrument-filtered
answer, bit for bit the kernel's arithmetic, and counts the rows received
after their barrier. Where that count is 0 and the literal's bucket is the
decision calendar's at the rule's own label, the kernel names the answer's
cache object in the stream and reads it with no local fallback (K1,
`Kernel::delivered_answers`), `traded@weekly` shares `close@weekly`'s
answer (K3), and the column-block evaluator reads delivered answers
through the same path (`Kernel::delivered_block`, `ColOp::PushedJoin`).
A streamed cache object is held against pruning while its stream is open.
`dvol` 39.7 s, `traded@weekly` 15.7 s and `close@weekly` leave the clock
(each derived through its plan at every bar); `push` grows from 6.2 s to
15.8 s, the service's two new passes.

On `m8-bench2`, the parity store, `--evaluator block`: 134.5 and 133.1 s
against 180.2 s, the fingerprint `6e72c9bb…` unchanged, the front's peak
1.13 GB. Close and volume still stream: `eligible`'s as-of read of
`close@base` (E-160) and the executor's `bar_volume` read them (K2, E-158,
drops a relation only delivered answers read; none in MW14). What the
run spends now (a profile of this tree): `decide` 42.5 s and `fill` 44.2 s,
a fifth of the CPU in `case_value` and a seventh in `window_columns`
(`dividend_at`, `split_factor`, `dividend_or_zero`: S13, S14), `push` 15.8
s, `check` 8.3 s, the data service 92 s of CPU beside the front's 110 s.

**Package 3, the column-block evaluator's remaining operators (Nov 26): the
parity run from 134 s to 81 s, the same returns.** E-152 to E-154, E-170,
E-171. An as-of read of a stored relation with the entity bound
(`ColOp::AsofStored`, through the walk the row path uses,
`Kernel::asof_stored_into`) and a read at a time a row binds
(`ColOp::GatherAt`): `eligible` and `total_return` planned again. The
previous row (`ColOp::AsofDerived`, `AntiJoin`, `Prev`, over the row
path's `asof_walk` and its memo): `prev_row`, `universe`, `first_row`.
Temporally recursive relations planned, their read of an earlier bar a
`GatherAt` (`row_number`, `cum_split`, `tr_index`, the latch). The case
(`ColOp::Case`: `split_factor`, `dividend_or_zero`) and a calendar window
over an as-of read of a stored relation (`ColOp::WindowAgg`:
`dividend_at`). Each held equal to the row path by `eval_set.rs` (the
corpus, both books, MW14 over 120 names with holes, resumed and fed late
tuples, whole and trimmed, 1 and 4 workers, the stored counters on one),
with a mutation each.

On `m8-bench2`, the parity store, `--evaluator block`, on top of package
2's second part: 81.4 s against 134 s, the fingerprint `6e72c9bb…`
unchanged; `decide` 42.5 s to 22.8, `fill` 44.2 to 15.6; the front's CPU
59.8 s; `dividend_at` 10.3 s, `clenow` 8.0, `eligible` 3.9, `ma` 2.8, the
rest under 1.3 s each. Before the first tuple: `bind` 3.6 s, `check` 7.8,
`push` 14.1 (the data service's three passes). The units' own runs, on
package 3's first operators without the delivered answers: 180.2 s,
167.1 (the as-of read), 159.2 (the previous row), 149.4 (the chains),
141.5 (the case).

**Package 5, third part (Nov 26): 1,851 of 1,851 weeks to 8e-15, and the
calendar window read for the whole frame: 78.7 s.** E-161 to E-166. The
five mechanisms the second part measured are in the engine (every action
of a bar's bucket on a coarser calendar, E-162; the dividend compounded
at its ex-date, `prod` and `dividend_reinvested`, E-163; a delisting's
week, E-161; the split-day dividend as the adapter's decision, E-164):
the weekly returns equal the canon in all 1,851 weeks to 8e-15
(fingerprint `be605991…`, 4,862 decisions). Merged onto the remaining
operators the run costs 90.8 s, not 81.4: `dividend_at` now reads every
key of the bucket, which E-171's operator does not, so the rule went back
to the row path (15.4 s). `Kernel::run_window_frame` (E-165) reads each
bar's bucket once for every entity and scatters its tuples to the frame's
rows: `dividend_at` 3.1 s, the run 78.7 s, the same fingerprint, the
front's peak 1.14 GB. Its clock: `bind` 3.7 s, `check` 8.1, `push` 14.6,
the stream 52.1 (`decide` 23.5, `fill` 10.6, `insert` 5.4, `decode` 3.3,
`trim` 2.6, the invariants 2.4, `wait` 1.8, `warm` 1.2); `clenow` 8.2 s,
`eligible` 3.8, `dividend_reinvested` 3.2, `dividend_at` 3.1, `ma` 3.0,
the rest under 1.4 s each. The kernel's thread is the critical path (67 s
of CPU at 90.8 s) and its profile is flat: no symbol above 7%, the
allocator 15% together. The stream still carries 53M rows (`close` and
`volume` 20.1M each, `member` 12.9M), which the data service's
subscription thread spends 34 s of CPU producing.

The socket pass of the gate failed on the remaining operators' tree:
`eval_set` could not start a data service (`Cannot allocate memory`) at
4.3 GB resident. Over a socket a dropped client never ended its
connection (the reader's thread held the socket's other descriptor), so a
test binary kept every service it started and the stores they map: 58,308
mappings at the base tree, the limit of 65,530 with the new tests. E-166
shuts the socket down with its last writer.

**R8L read at its clock's bars (Nov 26): 393.8 s and 5.79 GB to 32.9 s and
0.60 GB.** E-167. The market is synthetic, since the book's own history
is on the pipeline machine alone: the book's 26 roots, 3,000 days of
minute bars from 2000 (180,000 barriers, 23.6M tuples), `--evaluator
block`, and `risk=65 USD`, a hundredth of the book's risk a leg, because
the synthetic market ruins the book at its own risk within two years and
a ruined book stops deciding. Every run below makes the same 4,618
decisions and 9,236 fills.

| Tree | Run | Kernel peak | Where the time is |
| --- | --- | --- | --- |
| `3be3ba4` (PR #80 merged) | 393.8 s | 5.79 GB | an observed relation off the base calendar switched the history's trim off |
| observation at the barriers, the trim kept | 244.3 s | 2.61 GB | `observe` 204.1 s: the three observed relations queried at each of the 180,000 minutes |
| observation at the clock's bars (E-167) | 32.9 s | 0.60 GB | the stream 23.6 s: `observe` 10.4, `decide` 4.8, `insert` 2.8, the invariants 2.0, `decode` 1.2 |

The last row ran beside a staging job on the same machine, so it is not a
clean clock; the first two ran alone. `--timing` now prints two latency
lines. Over all 180,000 barriers: mean 0.106 ms, median 0.017, p99 3.32,
max 29.0. Over the 3,000 barriers a decision is made at: mean 1.66 ms,
median 1.68, p99 2.20, max 29.0, against A-7.3's 50 ms from a barrier to
its decision. That is the kernel's share in a replay. A live feed's
transport is not in it, and the live rehearsal feed (`abt data serve
--record synthetic`) is equities only, so no live R8L run can be made from
the command line.

What is left on R8L, in the order of its cost:

- **The session view counts the universe at every minute.**
  `universe@session` is generated as a count over the session's bucket of
  the base `universe`, so the base relation is evaluated at each of the
  180,000 minutes: `universe@session` 3.5 s, `universe` 2.0, `instruments`
  2.1 of the 23.6.
- **The session's windows are walked in the kernel.**
  `close_before[30min]@base` 2.3 s, `close_before[30min]@session` 2.0,
  `close_before[20min]@base` 1.3. They are not pushed to the data service
  as MW14's weekly aggregates are.
- **Two block plans are declined at every bar** (`atr`, `m_median`).
- **Every base minute is still a barrier.** The design of
  `docs/spec/06-performance.md` section 3.3 (the kernel woken at the
  178k decision bars, the session's features pushed down, a fill as a
  point query) is not built. On the book's real history, minute bars of 26
  roots from 2010, the run has not been made on this tree.
- **A live run keeps its whole history** (`trim_stores` is off for a live
  run, `cli.rs`). Whether a live run may trim is a decision of the owner's.

**The work before the stream, together (E-168).** MW14's preconditions
(8.1 s) and its pushed aggregates (14.6 s) ran one after the other, and
the data service answered one aggregate at a time, since a pass held the
store's write lock from its first read to its stored answer. A pass is
now two halves: `agg::prepare` computes under the read lock, `agg::finish`
stores under the write lock and takes an answer another pass stored
first. The kernel asks its distinct pushed requests on four threads and
checks the preconditions on a thread of their own while it builds and
pushes. MW14 on the parity store: 78.7 s to 68.1 s, the same fingerprint
(`be605991…`), 1,851 of 1,851 weeks to 8e-15. The clock before the stream
is 16.6 s where it was 26.4 (`check` 13.2 s and `push` 11.1 s now
overlap; each is slower than alone, the machine having four cores); the
stream is the 51.3 s it was (`decide` 22.8, `fill` 10.4, `insert` 5.3,
`decode` 3.4, `trim` 2.4, the invariants 2.4). The front peaks at
1.11 GB; the data service at 7.61 GB with 89 s of CPU, which is now the
run's memory. `--threads 4` on the same store takes 72.7 s (`decide`
28.4): under the block evaluator the windowed rules are not cut across
workers, so the workers add their overhead and nothing else.

**Real history only (Nov 26).** The owner's rule from this day: a
benchmark or a validation run reads real market history, never a
synthetic market and never a lake cloned wider. The records above that
were made on a synthetic market (R8L's of this morning, the 1,000-name
cell of Nov 5) are kept as what was measured then and are not the
project's numbers. What follows was run on the vendors' data, brought up
to date the same day.

*The data.* Norgate: the lake in `s3://ndlake-094828274452`, the newest
there is, bars to 2026-10-26 (`_meta/manifest.json`, watermark
2026-10-27) where the Mac's lake ends on 2026-06-24. Its index
membership file was last written on Sep 19: the lists are those of that
day carried forward. A newer lake needs the Norgate updater on the
owner's Windows machine and its upload. Databento: the 81 futures roots
the pipeline holds, pulled again from 2026-08-01 through the pipeline's
cost gate (every request priced at $0.00 before it was made), to the
session of 2026-11-25; the history before it is the pipeline machine's
(2010-06-07 for CME, 2018-12-23 for ICE). TradeStation, the R8L parity
record's source from 2000, ends on 2026-08-24 and is extended only by an
export of the owner's.

*MW14 on the lake of Oct 27.* The book's two series are carried to the
lake's last day by `scripts/mw14-series.py`, each from the source the
book's own file was made from: `$SPXTR` from the lake, equal to the
book's file on each of the 9,717 days they share; the weekly VIX from
CBOE's history (the week's last close and its 4-week mean), equal to the
book's file in each of its 1,906 whole weeks. The lake's own `$VIX` is
not used: its weekly last close differs from CBOE's in 11 of those weeks.
The run: 65.0 s on those series (71.8 s before the store's fold and the
batched pass below, the same returns to the byte), 1,869 weeks from
1991-01-04 to 2026-10-23, 17 past the canon's last. Against the canon
over the 1,852 weeks they share:
correlation 0.9983, CAGR 21.16% where the canon has 20.90%, Sharpe 1.030
against 1.020, the same drawdown (26.19% and 26.18%), 417 weeks equal to
8e-15 and 1,469 to 1e-4, the largest difference 3.3% (the week of
1994-07-29). The same tree on the June lake equals the canon in every
week, so the difference is the lake's, and it was measured file against
file over the days both hold (to 2026-06-24, the three lists' names):

- The memberships are the same, 6,004 intervals.
- 77 names that were S&P 500 members on 1990-01-02 have no bar in the
  June lake and their whole history in the lake of Oct 27: most were
  delisted between 1994 and 2006 (Grumman, Gerber, Syntex, American
  Cyanamid, the old McKesson, UAL), and CNA and NL still trade. They are
  63,541 member-days of 18.7 million. The canon's book never saw them;
  this one ranks them.
- Of 20.1 million shared bars: the close differs in 10 (two names), a
  dividend in 10 (six names; four moved from June 24 to June 23), the
  volume in 1,194 (985 names).

So the canon is not the reference for this lake. The reference would be
the Python book run on the same lake, which is the owner's to make
(`strategy/ivmom/data.py` builds the panel the canon was frozen from).
The lake also carries `curated/bars_by_date/archive/`, 2.9 million bars
from 1896 to 1989 that repeat some rows of the yearly partitions (505
days of `$SPXTR`, the same bar twice); no (asset, day) is repeated inside
a partition and the archive ends on 1989-12-29, so nothing a stage reads
(from 1990) is repeated.

*The log compressed (E-169).* A log segment is written with the Arrow
IPC format's zstd body compression. On the real 6J the log goes from
1,350 MB to 374 MB and the store from 1,865 MB to 890 MB, with the same
versions, an ingest of 73 s where it took 80 and the same 5.0 GB peak.

*The hot store after a restart (E-137, E-172, E-173).* The two stores
staged this day were the first real ones opened again after their
appends, and three faults of the append path showed, none of which the
fixtures reach. A seal record named its instrument by an ordinal that a
later instrument moves (E-137): a service restarted on the futures store
refused its own journal. The presences of earlier generations were never
removed (E-172): 3.8 GB of 4.5 in the futures store's hot directory at
15 roots, and the staging stopped on its disk guard at 23. And an
instrument that receives no more rows keeps its tail, so every open
replays the log from where that instrument stopped (E-173): the equity
store took 21.5 s and 2.5 GB to serve. A staging now ends by folding the
tails (`abt data consolidate RELATION --tails`). The equity service on
the pipeline machine: 55 s and 6.2 GB to serve before the three, 0.2 to
0.5 s and 1.03 GB after. MW14 on the benchmark machine, the same store
before and after its fold: the service ready in 32.6 s, then 9.0 s; its
CPU over the run 92.0 s, then 42.4 s; the run 71.9 s, then 69.0 s.

*The data service's memory in a run (E-174).* After the fold the service
still peaked at 8.2 GB in a MW14 run whose kernel holds 1.15 GB. It was
the aggregate pass: it read a relation whole and held its rows several
times while it reduced them, and a run asks four passes at once. The
pass over a hot relation now reads a batch of instruments at a time: the
peak is 4.74 GB, the service's CPU 37.5 s, the run 65.0 s, the returns
the same bytes. What it does not reach: one instrument with millions of
rows (a futures root's minute bars), and the answers that are not passes.
A run's checks ask for whole relations (`equity.close`, 30 million rows,
6.6 s), and a stream of the whole equity store held the service at 6 to
10.6 GB on the pipeline machine with the batched pass in. That is the
roadmap's package 7 (c).

*R8L's store, and R8L on it.* The 26 roots from Databento's minute bars
(`scripts/r8l-store.sh`): 96,308,743 bars a relation from 2010-06-07 to
the session of 2026-11-25 (KC and SB from 2018-12-23), 19 GB, opened in
0.9 s. **The book does not run on it, and no number of this run is the
book's.** The store's sessions are the pipeline's, the electronic day
ended at the 17:45 New York break. The book's are each root's primary
session (`d20-research`, `d20research/data/sessions.py`: 08:30 to 15:00
Chicago for the equity indices, 07:20 to 14:00 for the treasuries and
the yen, 08:30 to 13:20 for the grains since April 2012), which is what
the parity bundle was built from (`docs/parity-r8l.md`). `r8l.dsl`
decides at `session + 30min`: on this store that is half an hour after
the evening open, held to the next afternoon's close. The run halted on
2014-06-24, its $1M base lost, after 470 s, with the kernel at 15.2 GB
and the service at 11.3 GB. A halted run prints no clock, so the 15.2 GB
is not attributed; the synthetic record above (32.9 s, 0.60 GB) did not
predict it. What R8L needs before it is measured on real history: the
Databento adapter told each root's session window (bars outside it are
not the book's), a store staged with them, and room for it (the pipeline
machine has 9 GB free beside this store). The same run cut at
`--end 2014-06-01` was killed at 22.3 GB: a run reads its stream to the
store's last row whatever its window, and nothing after the window is
trimmed.

*The pipeline's agents on this engine.* The pipeline's own runner
(`agents-harness/direct/run_direct.py`, unmodified, its side-run mode:
nothing posted to the team) with the real quant agent, on the two stores
above served by this tree's binaries beside production's, four posts:

| Post | Result | Time | Spend |
| --- | --- | --- | --- |
| NQ, an IBS rule | done, a full backtest: the check, the plan (sanity, base, grid, reveal), the report, the submission | 1,269 s | $0.41 |
| ES, Double Seven | failed its translation check: the program states $7.50 a round turn and the fills pay $4.594 | 672 s | $0.28 |
| S&P 500 mean reversion (twice) | the first time the check passed and two trades reconciled to the cent, then the study died with a test service I had capped at 8 GB; the second time both check runs were killed at `abt-slot`'s 14 GB | 730 s, 633 s | $0.28 each |

Three things they met, none of them new with this tree:

- *Every agent's first plan is refused.* The skill's example writes two
  steps on one line with nothing between them
  (`abt-plans-and-claims/SKILL.md:94`), the parser reads one step that
  observes `size report main`, and each agent spent a call finding out.
  The deployed engine parses the same. The refusal now says where a step
  ends; the skill's line is the pipeline's to change.
- *The commission.* The catalog's rate is charged before the program's
  (E-71), and this store states each root's (R8L reads `per_contract from
  catalog`), so a `futures_house` program that writes 3.75 a side pays
  2.297. E-96 left the precedence to the owner. One store cannot serve
  both until it is settled.
- *A check run holds the store's whole history.* The second equity
  failure, repeated by hand: the agent's program over its six-month
  window, one ticker, took 121.6 s and 11.6 GB in the kernel (23.5 GB
  with every name), of which the decisions were 1.9 s. The stream carried
  256.8 million tuples from 1990 to the store's last day. A plan's kernel
  trims nothing (the roadmap's package 7 (a)), a stream has no lower
  bound (7 (b)), and before a run's window opens nothing is trimmed even
  by `abt run`. The deployed engine trims nowhere: 23 of the 134 jobs in
  production's own job directory met the same kill. The skill teaches a
  parameter for a one-instrument check (`only`), which scopes nothing.
  The same program with `need instruments where ticker = "AAPL"` in its
  data block (E-99 scopes the stream by it) took 9.2 s and 0.09 GB: the
  same 5 decisions, 5 fills and $9,568.33, over 132 bars where the
  unscoped run counts 135 (three days on which only other instruments
  have a bar), so its per-period metrics differ in the third digit.

*MW14 on the real Russell 3000 (Nov 26).* The lake's own Russell
memberships, the book's rules unchanged (`docs/handoff/benchmark.md`, "A
wider universe"): 12,373 assets from 1990 to 2026-10-26, about 3,000 at
a time. At `3c10fc5` with `--evaluator block` on the benchmark machine
the run takes **164.8 s**, inside the owner's bound of 3 minutes for a
universe of that size: 5,115 decisions, the run's process at 2.57 GB,
the data service at 8.54 GB and ready after 18.0 s. The stream is
135.2 s (decide 63.6, fill 23.0, insert 20.0, decode 7.3, trim 6.8), the
checks 25.0 s and the pushed aggregates 17.6 s. The store is 3.7 GB,
staged in 19 spans of two years in 46 minutes. There is no canon for
it.

*R8L on each root's primary session (Nov 26; E-176, E-177).* The
Databento adapter takes a root's session windows (E-176) and
`scripts/r8l-store.sh` stages the 26 roots with the book's: 33.2
million minute bars to the session of 2026-11-26, 3.1 GB where the
whole-day store is 19 GB (120.2 of the 252.8 million bars read lie
outside the windows; `gc` after each root evicts the versions its
ingest superseded, which were 10 GB of the 19), in 25 minutes on disk.
`r8l.dsl` runs on it to the end: 462.5 s, 16,353 legs, the kernel at
8.1 GB and the service at 5.5 GB, a barrier 0.14 ms on average and a
decision barrier 5.2 ms (p99 13.3 ms, max 21.4 ms). These and the
numbers below are of Nov 27, on the store staged from the pulls as
repaired (two paragraphs down).

The first run on that store was wrong, and found the engine fault of
E-177. Its legs were 87.6% of the book's with 5,001 the book does not
make, most of them short sidecar legs, at 1.45 to 1.5 times the book's
P&L a matched leg: a risk unit an eighth to a quarter too small. ES's
20-session mean true range on 2024-04-02 is 43.66 points from the
store's bars and was 33.86 in the run; it is 43.66 again in a run of ES
alone or of ES and NQ, 33.86 beside ZN and 36.73 beside GC, and exact
with `--no-pushdown`. The data layer's pass buckets every instrument's
rows by one list of labels, and the kernel's list for the sessions
calendar is every root's session closes together, so ES's `high@session`
and `low@session` were those of its last hour, after the treasuries'
15:00 close. The kernel now reads the pass's answer only where it is the
instrument's whole session and evaluates the others itself. That costs
R8L about 70 s of 450 and 4 ms a decision barrier, until the pass is
given a list of labels per instrument.

With it R8L on Databento's history stands beside the Python book on
TradeStation's (`scripts/parity/r8l_legs.py`), over 2011-05-25 to
2026-08-14: 15,555 legs in the book and 15,493 in abt, 14,749 the same
market and day (94.8% of the book's), 99.92% of those in the same
direction, leg P&L correlation 0.973. On ES's 3,910 session days at the
book's fixed base the book returns 22.35% a year at a Sharpe of 1.365
with a 19.2% drawdown, and abt 21.31% at 1.326 with 17.7%; the daily
series correlate 0.964. SB and KC (79% and 80% of the book's legs), HE
(84%), LE and PL (87%) are the roots where the two vendors' sessions
differ most.
The run's own report had no periods at first: `sessions of "ES"` looked
for a symbol named `ES`, and a vendor store's series is `ES.v.0`
(E-178, which names it by its ticker as an instruments table does). On
ES's 4,234 sessions from 2010-06-07 it gives an additive annual return
of 20.88%, a Sharpe of 1.302 and a drawdown of 22.90%. Its total P&L,
$3,507,482, is under the sum of the fills' cash flows ($3,537,607): a
report on a calendar counts that calendar's days, and $30,124 falls on
14 days with no ES session (Good Fridays on which the treasuries trade,
two days of Oct 2012, four of 2014 the vendor has no ES bar of, the two
national days of mourning, a Sunday on which ETH trades).

That difference was $34,478 on 14 days in the first run with a report,
and two of the 14 were Sep 1 and Oct 1, 2026, ordinary sessions. The
pull of Nov 26 had cut them: the pipeline's month-chunked pull writes a
session day's file from the first chunk that holds any of it and skips
the day in the next, so a session that opens before midnight UTC on a
month's last day keeps its first two hours. 116 day files in 59 of the
pull's 81 roots were cut that way, and the store had no primary session
on those two days for 16 of R8L's 26 roots. `scripts/r8l-repair.py`
asked the vendor for the days again through the pulls' cost gate and
replaced the files; `scripts/r8l-views.py` now takes the file with more
rows where two pulls hold a day; R8L on the store staged again from
them, in memory and to the session of Nov 25, took 441.7 s and made
16,348 legs at a Sharpe of 1.301 on 4,233 sessions, 21.22% a year at
1.321 beside the book's 22.35% at 1.365 (before: 439.0 s, 16,347 legs,
1.310 on 4,231 sessions, 21.33% at 1.329 beside 22.07% at 1.347). The other weekdays on which some roots had no session were
asked for too: Feb 28, 2020 was cut in the history and the vendor holds
more of it, whole for the equity indices, the yen and the livestock
and without the hours from 14:00 to 20:00 UTC for the treasuries, the
metals, the energies and the grains; fourteen days of 2012 and 2014
are the vendor's own gaps.

On Nov 27 the pulls were held against the vendor two more ways, and
both found more. A day file's first and last bar must belong to the
day it is named for: KC's and SB's do not from 2026-06-22. The pulls'
writer drops the vendor's duplicate bars and then groups the frame by a
day series numbered from zero, which pandas aligns by row label, so
every row after a duplicate takes the day of a later row: each file
holds the end of the day before and lacks its own, and a chunk's last
rows are written nowhere (KC lacked 6,195 of its 303,233 bars from Aug
3 to Nov 25, the last hours of four month ends). Both roots are asked
for again from June 2026, a row's day taken by position. The other
check is the vendor's own record count, a year of a root at a time and
by halves down to the day where it differs from the files: for the 24
CME roots every year agrees except on 17 days, among them the two
national days of mourning (2018-12-05 and 2025-01-09), of which no
pull held a file, and the dataset's first evening. 100 root-days are
written whole, with eight more that a check of the files' footers had
found (2020-06-30 cut at 14:10 UTC for six roots). The vendor held the session of Nov
26 by the morning of Nov 27, and it is in: Thanksgiving, on which 16
of the 26 roots trade. The store staged from all of it is on the
pipeline machine's disk, where the room was made by the engine's own
`gc` on the whole-day store (20.9 GB to 10.0 GB in 6.5 minutes), and
the numbers of this section are R8L on it. The pipeline's own pulls
carry the first and the third fault and are left as they are; its days
are written whole beside them (`docs/handoff/benchmark.md`, "What the
pulls lacked").

*The store's `gc` on real data.* On a copy of the folded MW14 store,
`abt data gc --disk-budget 0` evicted 58 superseded versions in 9.2 s
(76 version manifests to 18), and MW14 on the result gave the same
returns in 65.2 s.

*The agents again, and a plan's threads (Nov 26).* Two more posts
through the pipeline's runner, one at a time, on the committed binaries
($0.47): neither reached a full backtest. The equity post met the
whole-history check run again (package 7 (a) and (b)). The NQ post's
development plan, a base run and a grid, was killed at the slot's
14 GB, where the day's earlier NQ post had finished with another
program. Run by hand, that plan peaks at 15.3 GB on either build: `abt
plan run --kernels 1` bounds the kernels and not the work, since the
executor runs `threads` steps at a time on them (4 by default,
`abt-query/src/plan/exec.rs`), and each step's run holds the store's
history. With `--threads 1` the same plan peaks at 4.1 GB and takes
2:44 where it took about 1:45. The pipeline's tool passes `--kernels 1`
alone (`agents-harness/agents_harness/local.py`), so a grid on a minute
store needs `--threads 1` beside it there, or package 7 (a). A third
post on the build with E-177, with that flag added to its `plan run`
by a wrapper beside the unmodified runner ($0.27), did not reach its
plan: the agent wrote `commission per_contract 3.75`, the store states
the book's 2.297 for NQ, and its translation check refused the
difference (E-71 and E-96's open call, as on the ES post before). The
stores staged here carry R8L's commissions, so an agent's house program
cannot be checked on them until the precedence is settled or a store
is staged without them.

A fourth, on the build with E-178 and an NQ store staged the pipeline's
way, with no commission stated ($0.22): its check plan ran, 19
decisions over four months, with no memory kill and no commission
refused. The agent then stopped at its translation check: it read the
fills through the pipeline's `query_run`, `ledger fill`, and found each
`moc` fill one session after its signal. The run's events have the fill
at the decision's own close (2026-06-03T21:00 at 31,093.00, the
session's close); `ledger fill` prints the program's `fill` relation,
whose row is at the bar the program first sees the fill, the next one.
The deployed engine keys that relation the same way (`sim/mod.rs`), so
this is not a change of this branch: the skill or the tool has to say
which time that column is, or read the run's events.

A fifth run, both posts again, on the same build and the NQ store
staged from the repaired pulls ($0.61). The NQ post ran to a full
backtest and was submitted: 986 s, 16 trials (a run, a grid of 12 and
3 claims), the check, the development plan, the reveal and the verdict
written, nothing killed. Its agent met the same ledger and went on. It
is the first full backtest through the pipeline's runner on the final
build, with `--threads 1` added to its `plan run`. The equity post's
translation run was killed for memory again, one step at a time or
four: a check on the equity store holds the store's whole history in
the kernel (package 7 (a) and (b)), which no option of the plan
changes. The agent's notes name one more thing of the pipeline's: its
`plan_check` refused the forward plan the skill prescribes (`holdout
after H` without a window, then with one).

A sixth run on Nov 27, the NQ post alone, on the NQ store staged on the
pipeline machine's disk from the pulls as repaired, to the session of
Nov 26 ($0.60): a full backtest again, submitted, in 1,204 s and 34
tool calls, nothing killed. Its agent read the fill ledger as it is (a
`moc` fill shown at the next bar, at the close of the decision's own
session) and reconciled two trades by hand against the store's bars
before going on. Its notes name `plan_check` and the forward plan
again. The runner's recorded spend stands at $76.68.

### The panel

(This first record was made before the rule above: three of its four
stores were synthetic. `scripts/panel.sh` now runs two groups, each on
real history: R8L on its 26 roots from Databento and the corpus's daily
strategies on the S&P 500 lake.)

The owner's instruction of Nov 26: a change made for one book is measured
on every program. `scripts/panel.sh NAME TREE` runs each program but MW14
once, cold, on a fresh copy of its store with a data service of its own,
over the store's whole history, and writes one line a run: seconds, peak
memory, decisions, fills and the SHA-256 of the returns. The stores:
`r8l` on the synthetic minute futures market above; `opening_gap` and
`resampled_momentum` on a synthetic minute equity market (20 names, 250
days); the daily strategies written for a small market on a synthetic
daily market from 1990; the six strategies that need the list `SPX` on
the S&P 500 lake staged from 1990, every member.

The base tree (`3be3ba4`), the cells that finished:

| Program | Store | Run | Peak | Returns |
| --- | --- | --- | --- | --- |
| `r8l` | futures, minute | 393.8 s | 5.65 GB | |
| `opening_gap` | equity, minute | 1.3 s | | `806d34321af46911` |
| `monthly_rebalance` | equity, daily | 1.8 s | | `e76d74745ff99a24` |
| `pairs_trading` | equity, daily | 4.3 s | 0.07 GB | `de43af1eb64fb0f3` |
| `dividend_capture` | S&P 500 lake | 72.9 s | 1.50 GB | |

Most of the small daily strategies halt on the 100-name synthetic market
(a risk policy refuses the book's first orders), and `resampled_momentum`
halts on a margin call: a halted run measures nothing. The daily market is
staged again with the five names those strategies were written for. The
current tree's column and the rest of the base column are not measured
yet.

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
