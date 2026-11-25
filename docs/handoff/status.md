# Status ledger

Nov 24, 2026. What was achieved, what was not, and the evidence for each. Every figure names its source. Where a source is a document in this tree, the document holds the full record.

## 1. Timeline

| When (2026) | What |
| --- | --- |
| Oct 5 to 7 | The old single-crate engine reproduces both company books exactly: MW14 (1,852 weekly returns) and R8L (21,892 legs). Performance work takes MW14 from 963 s to 126 s on an Apple M4 (PR #69, `docs/assessment.md`) |
| Oct 9 to 12 | The second formulation of the language and its corpus (PRs #70, #71). The three-layer specification is written (issue #72, `docs/spec/`) |
| Oct 15 to 17 | Migration steps M-0 to M-10 land on `spec/three-layer-split` in three days. M-8, the performance step, takes one morning (Oct 17, 06:54 to 11:05); its last commit records what it leaves unbuilt (ERRATA E-77) |
| Oct 18 to 19 | `docs/spec/07-storage.md` is merged and iteration 1 builds the hot store (PR #74, `docs/iterations/01-foundation.md`). The books run on the three programs for the first time; the full MW14 is killed at 443 s with the front past 8.5 GB (PR #75, ERRATA E-93) |
| Oct 19 to 20 | The pipeline's first campaign runs on the engine. The defects found there are fixed as ERRATA E-104 to E-124 (PRs #76, #77, #78). The pipeline machine is built from that tree |
| Nov 4 to 5 | MW14 is profiled on the old engine against the Python book on one machine (issue #79; data on branch `perf/mw14-profile`) |
| Nov 5 to 6 | M-8 work on the three-layer engine: parallel evaluation, the retention horizon, the first step of the stream projection, three batch paths in the evaluator. The full MW14 finishes: 377 s, then 367 s (PR #80) |
| Nov 6, 06:04 UTC | The benchmark machine is stopped. No engine work since |
| Nov 9 to 14 | The pipeline's chat layer goes live, still on the Oct 20 engine build |

## 2. Achieved

1. **The split exists and runs.** Three programs and a front over one protocol, on sockets or in process. Eight crates, 99,700 lines, 859 tests (two ignored). The last green CI run, at commit `3e0d002`, passed 857. Every commit after it fails CI (section 5).
2. **The correctness instruments.** Determinism across transports, threads and kernels (gate G3). A pandas reference differential (G6). About 50 mutation patches, each failing its test. Two evaluators held equal on every corpus program and both books (`crates/abt-tests/tests/eval_set.rs`), and a trimmed history held equal to the whole (`crates/abt-tests/tests/retention.rs`).
3. **The hot store.** Integer ticks in content-addressed segments, appends by sequence number, consolidation, the calendar ladder. Ingest at 3.10M rows a second against a target of 200,000 (TRACE, "A-7.3 sustained ingest"). Restart in 92 ms on the store of the lake since 2014.
4. **The honesty layer.** Plans with hashes, lineages and counted trials, hold-outs enforced by truncating the stream, claims, the deflated Sharpe ratio, the pipeline's house presets.
5. **The books.** Exact on the old engine. On Nov 6 the full MW14 finished on the three-layer engine for the first time, because the retention horizon (ERRATA E-126) bounded the kernel's memory. The horizon is set by `abt run` only; a run made by a plan does not get it (section 5).
6. **Real use.** The engine has run every engine backtest the pipeline has answered since Oct 19.

Gates G2, G3, G4, G5, G6, G7 and G9 are done. G1, G8 and G10 are partial.

## 3. Performance

### MW14 full replay

1991 to 2026, 4,240 names. The first four rows are on `m8g.xlarge` machines (Graviton4, 4 vCPU, 16 GB): the same instance type, not the same instance. Issue #79's two rows were measured on Nov 4 to 5, before `m8-bench2` was launched.

| | Wall time | Memory | Source |
| --- | --- | --- | --- |
| Python book, cold to answer | 25.24 s (panel build 15.29 s, book 9.93 s); 74.9 CPU-seconds | 9.4 GB peak | issue #79, §1 |
| Old engine, `713ed1a`, `abt run` | 366.33 s (423.85 s cold to answer) | 3.5 GB | issue #79, §1 |
| Three-layer engine, first full run | 377 s | front 3.7 GB, data service 3.8 GB | `docs/assessment.md`, "At M-8 (Nov 6)" |
| Three-layer engine, PR #80 head | 367 s | the same | `docs/assessment.md`, same paragraph |
| Old engine on an Apple M4 | 145 s exact sums, 126 s running sums | 3.1 GB | `docs/parity-mw14.md`; `docs/assessment.md` |
| Target A-7.3 (stated for the M4) | 60 s | 1.5 GB | `docs/spec/00-architecture.md` §7.3 |
| Design projection (stated for the M4) | 6 to 10 s | under 1 GB | `docs/spec/06-performance.md` §5.1 |

Three things to keep straight when reading it:

- The M4 is about 2.4 times faster per core than the Graviton box (issue #79, measured on the old engine). So 60 s on the Graviton box is a harder gate than A-7.3's 60 s, and the design's 6 to 10 s is about 14 to 24 s there if that ratio carries over. The spec does not say how many threads its estimate assumes.
- The engine runs on one thread. The Python book uses about three cores. In CPU seconds the gap is 5 times, not 15.
- Before Nov 6 the three-layer engine could not finish this run at all. The retention horizon made it finish; nothing has made it fast.
- The 367 s is one run, on a tree where one test of the suite fails (section 5). Its result was byte for byte the 377 s run's.

### Where the 367 s goes

The run's own clock (`abt run --timing` with `ABT_TIMING_RELS=1`), from `docs/assessment.md`:

| Part | Seconds |
| --- | --- |
| Deriving relations | 200 |
| of which the four per-instrument windows: `clenow` 38, `dvol` 30, `eligible` 17, `ma` 16 | 101 (the source rounds to 100) |
| of which `dollar_volume@base` 14, `prev_row` 12, `dividend_at` 12, `traded@weekly` 11 | 49 |
| Waiting on the stream of daily rows | about 75 |
| The executor's fills and marks | 54 |
| Evicting the memo | 17 |
| Inserting streamed tuples | 10 |
| Not itemised | about 11 |

Grouped by cause. The grouping is this document's reading of the clock, not a measurement, and `docs/assessment.md` reads the same run differently (below).

- **Daily rows the kernel streams and buckets itself: up to about 140 s (38%).** The wait on the stream (75), the insertion (10), and the three derived relations that only bucket daily rows into weeks: `dvol` (30), `dollar_volume@base` (14), `traded@weekly` (11). The 55 s of bucketing is the firm part.
- **The evaluator and the executor: about 216 s (59%).** The other derivations (145: the windows `clenow`, `eligible` and `ma` 71, `prev_row` and `dividend_at` 24, and 50 the clock does not itemise), fills and marks (54), memo eviction (17).

The profile of the same run (`perf`, frame-pointer build): `malloc` and `free` 14%, `Value::cmp` 6.6%, the narrowing closure of `call` 4.2%, `atom_pattern` 4.2%, `rows_columns` 4.2%, `Memo::get` 3.8%, `Value::clone` 2.6%. No data-layer symbol is above 1%.

**The two readings.** From that profile `docs/assessment.md` (lines 336 to 344) concludes that the run is "evaluator-bound", that the gap to Python is "not the stream", and that pushing `dvol` and `traded@weekly` down "is a memory lever before it is a time lever". The profile and the clock are different instruments. The profile samples the CPU of the front process: it cannot see the 75 s the front waits for the next batch, nor the data service's own work. The clock is wall time. What neither settles is how much of the wait goes away when fewer rows stream. So 140 s is an upper bound on what the roadmap's package 2 can remove, and package 1 measures the data service's CPU time to replace it with a figure.

The run did not print rows per relation. By the counts of `docs/iterations/01-foundation.md`, open question 7, `equity.listed` (15.9M) and `equity.member` (13.0M) are 28.9M of the rows the kernel stored; `equity.ticker` (15.9M) has not streamed for this book since ERRATA E-127. The rest are daily bars.

Issue #79 measured the old engine's work on the same book: 55,466,803 derived-relation calls at 6.6 µs each, which is 87 µs per asset-week against the Python book's 2.4 µs. 62% of those calls were the split-adjustment chain, which Python does with two column expressions. Its ranked fixes were: stop allocating per derivation, evaluate a set at a time, store columns, give each relation its own memo horizon, remove the scans, and parallelise last.

### What the Nov 5 to 6 changes bought

| Change | Effect | Source |
| --- | --- | --- |
| Retention horizon (E-126) | The run finishes: front 3.7 GB instead of killed past 8.5 GB. 59.2M stored rows dropped. `abt run` only | `docs/assessment.md` |
| Parallel evaluation (E-125), 4 threads | 95.8 s before the change to 93.8 s on a synthetic 1,000 names, CPU 122.9 s. 16.3 s of the 93.8 s is inside the steps it cuts | `docs/assessment.md`, Nov 5 |
| Three mechanical changes (`9a255d9`): a sort skipped, slot maps cached, a vector reused | 114.2 to 113.1 user-seconds on 1,000 synthetic names, "within noise". It is also the commit that broke a test (section 5) | PR #80's body |
| Three batch paths in the set evaluator (`09f0c0b`, `5dd1a64`, `479782a`) | 377 s to 367 s on the book; 27.8 to 23.1 CPU-seconds on 1,000 synthetic names | `docs/assessment.md` |
| Stream projection, first step (E-127) | MW14 no longer receives open, high, low, tickers or classification | ERRATA E-127 |

### The other targets

| Target | Value | Measured | State |
| --- | --- | --- | --- |
| R8L full replay | 3 s, 0.6 GB | Old engine: 3.0 s, 0.6 GB. Three-layer engine: a seven-root, one-year subset only (2.8 to 5.3 s, 0 decisions) | Never run in full |
| Sustained ingest | 200,000 rows/s | 3.10M rows/s | Met |
| Cross-section of one bar | under 1 ms | 384 µs at 2,000 names | Met, scaled |
| Point and latest lookup | 5 µs resident | 35 and 36 µs | 7 times over |
| Series of 252 rows | under 100 µs | 125 µs | 1.25 times over |
| Rolling 252-row mean, 10,000 names by 30 years | 10 s on one thread | 62 to 104 s, scaled from 5.04M rows | 6 to 10 times over |
| Storage per value, Norgate and Databento | 2 B | 4.46 B in base columns; 11 B with the ladder | 2 to 5 times over |
| Restart to serving | 30 s | 92 ms consolidated; 34 to 70 s on MW14's tail-heavy store | Over on tail-heavy stores |
| Barrier to decision, live | under 50 ms | not measured | Needs a live feed |
| Instruments live | 10,000 at one minute | not measured | Needs a live feed |

All from `docs/spec/TRACE.md` (the rows named "A-7.3" and "D-9.1"), measured on a 4-core Xeon VM on Oct 18. None of these was measured on the reference machine.

One figure exists only in the pipeline's record: intraday futures run at about 1.2 ms a minute bar per root, so a full-history intraday plan takes 10 to 15 hours (`/Users/alejandro/absolute-pipeline/docs/iterations/01-mvp.md:162`).

## 4. Parity

| Path | Result | Source |
| --- | --- | --- |
| Old engine, v1 bundle | Exact: 1,852 of 1,852 weeks, largest difference 7.9e-15 | `docs/parity-mw14.md` |
| Three-layer engine, the same bundle ingested with `bundle_v2` | **No full-size run is recorded.** `docs/parity-mw14.md` says the kernel is at the canon "through the v1 bundle", but TRACE's "ST-13 parity" row is missing and iteration 1 could not finish the full book. The recipe may not bind at all (below) | `docs/parity-mw14.md`, last section; TRACE line 204 |
| Three-layer engine, the lake through the Norgate adapter | **Not equal.** Weekly correlation 0.9777; CAGR 18.5% against 20.9%; Sharpe 0.959 against 1.020; maximum drawdown −25.9% against −26.2%; 25% of weeks equal to 1e-4 | `docs/assessment.md`, Nov 6; PR #80 |
| R8L on the three-layer engine | Not run on the full history | TRACE, "A-7.3 R8L full replay" |

The Norgate path is the one that matters, because it is the path with no hand-built bundle in it. `docs/parity-mw14.md` attributes the difference to "the adapter's weekly views against the bundle's". That attribution rests on the unrecorded run of the row above, so the kernel and the adapter have not been separated as causes. The candidates the old ingest script handled explicitly are: the week's key (a name that last traded on Thursday is keyed at Friday), the total-return index used as the fill price, membership at the name's own week end, and the float32 `$SPXTR` series. PR #80 also records that a holdings comparison was not done because the store exposes no `equity.ticker` relation.

The recipe for the second row is `docs/parity-mw14.md`, "Reproduce", second block: ingest the old engine's weekly bundle and run `corpus-v2/company/mw14.dsl` on it. That program has demanded `bars @1d … complete` and `require reviewed` since Oct 12, the bundle is weekly and unreviewed, and the resolver refuses a stored calendar coarser than the one asked for (`crates/abt-data/src/resolver/plan.rs:79`). Whether it binds has not been tried.

The run has 1,904 weekly bars and the canon 1,852 weeks. The 52 more are presumably the bars of 1990, before `--start`; that was not checked. The comparison script aligns by index from the run's first bar, so the run must start at the canon's first decision week (`--start 1991-01-04`).

## 5. Pull request #80

"M-8: entity-parallel evaluation, the retention horizon and the first step of the stream projection". 22 commits past `cc3edc8`, 18 files, +2018 −135, all in `abt-kernel`, `abt-tests` and the docs.

**CI fails at the head, for two reasons.**

| | Failure A (current) | Failure B (hidden behind A) |
| --- | --- | --- |
| Step | Clippy | Tests |
| What | `clippy::unnecessary_get_then_check` on `self.memo_stuck.get(&memo_key).is_none()` | `quantile_conventions_and_the_aggregate_halt` panics with ``Internal("variable `P1` unbound in rule cut::cut#1")`` |
| Where | `crates/abt-kernel/src/kernel/eval.rs:610` in this working tree (line 605 at the head) | `crates/abt-tests/tests/kernel.rs:675` |
| Since | `eb3345c` | `9a255d9` ("atom slot maps cached") |
| Fix | `!self.memo_stuck.contains_key(&memo_key)` | See below |

The CI history of the branch, by commit:

| Commit | CI | What the commit is |
| --- | --- | --- |
| `3e0d002` | Green, 857 passed | The first push of the PR |
| `9a255d9` | Test B fails | The three mechanical changes |
| `09f0c0b`, `5dd1a64`, `479782a` | Test B fails | The three batch paths |
| `eb3345c` | Lint A fails; the tests are not reached | `call_first` |
| `2040b40` (head) | The same | Documents only |

**The suspected cause of B**, from reading the code and not from running it. Commit `9a255d9` caches each atom's slot map (the position in the rule's environment that each of the atom's terms binds) under the key `(cr.idx, atom as *const Atom as usize)` (`eval.rs:1090`), on the assumption that a literal's address is stable for the run. But a window's conjunction is reordered into a temporary on each evaluation: `let order = conj_order(conj);` at `eval.rs:1842`, and again at `eval.rs:2879`, where `conj_order` returns a new vector of cloned literals. When the temporary is freed its address can be reused by the next one, so `close(_, T, P1)` can be handed the cached slot map of `close(_, T, P)` under the same rule index. A second reader of the code reached the same conclusion on Nov 24.

Two consequences for the fix. Address reuse depends on the allocator, so the test must be reproduced on Linux; a pass on macOS clears nothing. And the cheapest fix is to remove the cache, since its commit measured no gain (section 3). Keeping it needs a key that does not depend on an address, and `atom_slots` is not handed the literal's position.

**What is not known at the head.** `cargo test` stops at the first failing binary. From `9a255d9` to `479782a` it stopped at `kernel`, so the 42 of `abt-tests`' 77 binaries that come after it in alphabetical order never ran, nor did `abt-wire`'s tests. Among them are `retention`, `tv`, `tv_golden`, `verify`, `options`, `outputs`, `pushdown` and `state`. At `eb3345c` and the head no test ran at all, because the lint step comes first. So the three batch paths and `call_first` have never been through the whole suite in CI; `docs/assessment.md` says `eval_set` was run for each batch path on the benchmark machine. The PR body's "857 passed" describes the first push, six code commits before the head.

**What the PR does not reach: runs made by a plan.** The retention horizon is switched on by `abt run`'s command line (`crates/abt-kernel/src/cli.rs:586`, where `--history window` is the default). In the configuration itself the switch is off by default (`crates/abt-kernel/src/kernel/mod.rs:1120`) and is not serialised. The kernel service builds a run's configuration without it (`crates/abt-kernel/src/kernel/service.rs:122`), and `KernelRequest` has no field for it (`crates/abt-wire/schema/abt.fbs`). A run made by `abt plan run` therefore keeps the whole history, as it did before the PR. ERRATA E-126 describes the switch as `abt run`'s and does not say this. It matters because the pipeline's backtests are plan runs (`/Users/alejandro/absolute-pipeline/agents-harness/agents_harness/local.py:493`). Found by reading the code on Nov 24; not run.

**What the PR itself lists as not fixed.**

- No column-block evaluator; `--threads 1` stays the default.
- `asof_memo` (`crates/abt-kernel/src/kernel/mod.rs:2209`) is keyed without the entity, so one entity's answer can be served to another. This is not in ERRATA or TRACE.
- `--history all` on the full MW14 thrashes a 16 GB machine and does not finish.
- A primitive read inside a pushed-down aggregate still streams.
- MW14 through the Norgate adapter is not at the canon.

**Three things from issue #79 that are still in the code:** the rows-window early exit drops its group's buffer; `bar_field`, `has_price` and `bar_volume` (`mod.rs:3537`, `mod.rs:3607`, `mod.rs:3642`) scan whole bar blocks; the memo has one global horizon, not one per relation.

## 6. Specification coverage

`docs/spec/TRACE.md` has 208 status rows: 157 done, 40 partial, 11 missing. The 51 open rows, grouped by why they are open. The number is the row's line in TRACE.

### Waiting on the owner's machine, data or a live feed (21)

| Line | Row | Status | What is open |
| --- | --- | --- | --- |
| 36 | A-D24 vendors | partial | Fixtures only; a real session and the lake are the owner's |
| 37 | A-D25 options in scope | partial | The owner's options book was not supplied |
| 43 | A-D31 scale and latency targets | partial | Measured on a VM, not the M4 |
| 110 | G1 parity | partial | The inputs are the owner's; `tv_golden.rs` stands in |
| 120 | G8 options | partial | 25 hand cases pass; the owner's book not supplied |
| 122 | G10 performance | partial | VM figures only |
| 123 | G-ST1 codec on the M4 | missing | The M4 is the owner's |
| 124 | G-ST2 sizing run | partial | The full lake does not fit the VM through the adapter |
| 203 | ST-13 performance | missing | Encoding benches on the reference machine |
| 204 | ST-13 parity | missing | G1 through the new store |
| 210 | ST-13 residency | partial | Page faults through a G10 run are not read back |
| 211 | ST-13 G-ST1 | missing | The codec on the M4 |
| 212 | ST-13 G-ST2 | partial | The Databento subset only |
| 218 | A-7.3 instruments live | missing | Needs a live feed |
| 219 | A-7.3 history resident | partial | The full history does not fit the VM |
| 220 | A-7.3 barrier to decision | missing | Needs a live feed |
| 228 | A-7.3 peak by profile | partial | Not measured at size |
| 230 | A-7.3 full-minute futures bundle | missing | Not measured |
| 235 | D-9.1 barrier latency live | missing | Needs a live feed |
| 238 | ST-2.4 decode and sum | partial | The reference machine's figures |
| 240 | ST-9 store size | partial | Measured on 2014 to 2026 only |

### Performance work not built (10)

| Line | Row | Status | What is open |
| --- | --- | --- | --- |
| 96 | X-3.1 bit identity under entity parallelism | missing | Stale: the row still says "not built"; line 113 records it built one level down |
| 113 | F-5.3 / M-8 entity-parallel | partial | The cut is per rule application; the column-block evaluator and the per-stratum schedule are not built |
| 114 | 06-performance 3.1 retention | partial | Rows are trimmed, not re-laid as chunks |
| 115 | 06-performance 3.3 stream projection | partial | Primitives inside pushed aggregates still stream; columns are not projected |
| 224 | A-7.3 MW14 full replay | missing | The row still carries iteration 1's figures, not Nov 6's |
| 225 | A-7.3 R8L full replay | partial | A subset only |
| 231 | D-9.1 point, latest | partial | 7 times over |
| 233 | D-9.1 series | partial | 1.25 times over |
| 236 | D-9.1 rolling mean | partial | The pass decodes blocks instead of using the cumulative summaries |
| 239 | ST-2.4 ranges | partial | Whole edge blocks are decoded |

### Hot-store structures not built, or built differently (18)

| Line | Row | Status | What is open |
| --- | --- | --- | --- |
| 47 | A-D35 integer ticks | partial | The note is stale (it says the adapters still write Parquet) |
| 48 | A-D36 lane-interleaved blocks | partial | OHLC prediction from the close is not applied |
| 49 | A-D37 everything precomputed and hot | partial | Wavelet matrices, cross-moments, catalog structures and event-interval summaries are missing. The ladder is built, though this row says otherwise |
| 50 | A-D38 seven-stage staging | partial | No adapter lands objects (S0); the cold export (S6) is missing |
| 51 | A-D39 appends in place | partial | Built beside the published blocks (A-D51), not in place |
| 55 | A-D43 adjusted series as views | partial | The codec's arithmetic only; the data layer's views are not built |
| 57 | A-D45 hot means locked | partial | The shedding order is not built |
| 58 | A-D46 futures calendars by trade date | partial | Trade dates are UTC dates until a session table exists |
| 100 | ST-5.2 exact ladder | partial | No reader consumes the levels; futures sessions await the session table |
| 101 | ST-6.1 range closure | partial | Sparse tables and wavelets are not behind the store's query forms |
| 197 | ST-13 real-data round trip | partial | The Databento subset and the lake since 2014 only |
| 198 | ST-13 exact aggregates | partial | Not run over the store's blocks |
| 200 | ST-13 ranges | partial | Order statistics and bars over the store |
| 206 | ST-13 consolidation | partial | The memory bound is by construction, not measured |
| 207 | ST-13 adjusted ranges | partial | The codec half only |
| 208 | ST-13 calendars | missing | DST weeks, Sunday sessions, early closes, holidays |
| 227 | A-7.3 storage per value | partial | 2 to 5 times over the target |
| 229 | A-7.3 restart to serving | partial | 34 to 70 s on a tail-heavy store |

### Exact numerics in the kernel (2)

| Line | Row | Status | What is open |
| --- | --- | --- | --- |
| 93 | P-1.1 one implementation, one answer | partial | The kernel evaluates `numerics exact` as `two_pass` |
| 209 | ST-13 ticks on the wire | partial | The kernel reads floats; its exact windows are not built |

`docs/iterations/01-foundation.md` planned an iteration 2 for the last two groups ("trace at or above 90%"). It was never started: the pipeline work came first, then the performance work.

## 7. Open errata

Deviations that `docs/spec/ERRATA.md` records as open or not built. The roadmap package that closes each is in the last column.

| Erratum | What is open | Package |
| --- | --- | --- |
| E-125 | No column-block evaluator; threads are spawned per cut step, no persistent pool | 3, 4 |
| E-126 | A program with an unbounded lookback trims nothing; `--history all` keeps everything. Not in the erratum: a run made by a plan is not trimmed at all (section 5) | 3; change (a) under package 7 |
| E-127 | A primitive inside a pushed aggregate still streams; `dvol` has no data-layer form; `traded@weekly` is a `count` the pass refuses; columns are not projected | 2 |
| E-89 | No reader consumes the ladder | 2 |
| E-99 | An aggregate request has no instrument filter, so the data layer computes every instrument's answer | 2 |
| E-120 | No hard bound on what one binding costs the data service. Also a suspect race in an `appends.rs` test, for which a fix sits in the amendments tree | None builds it. Listed as change (c) under package 7 |
| E-93 | The packed tail is not built; a tail-heavy store takes 34 to 70 s to open | None ("Not in this roadmap") |
| E-90 | The shedding order is not built; consolidation runs under the write lock | later |
| E-87 | S0 and S6 are not built; a revision copies the whole instrument | later |
| E-85, E-88, E-96 | The kernel's `numerics exact` is `two_pass` | later |
| E-107 | Under `--plan written`, a builtin written before its instrument's literal steps over every instrument's sessions | later |
| E-110 | A plan that reads a trial's outputs after 256 later runs reruns the trial | later |
| E-105 | Whether rolls over adapter-ingested contracts are fully closed by E-116 is not stated | 6 |
| E-82 | No broker events in the stream; no broker adapter | later |

## 8. The pipeline's blockers, by owning layer

From the pipeline's two iteration records. Paths are under `/Users/alejandro/absolute-pipeline/docs/iterations/`. "Fixed" means fixed in the engine and deployed; a workaround in the pipeline does not count.

### Data layer

| # | Symptom | Source | State |
| --- | --- | --- | --- |
| 1 | A query on a hot relation opens the whole store: 15 GB, up to 21 GB | `02-chat.md:47`, `02-chat.md:235` | Open (E-120). Worked around with caches |
| 2 | A minute demand streams every bar since 2010, whatever the plan's window | `02-chat.md:205` | Open. In no package: a stream request has no start time. Change (b) under package 7 |
| 3 | Daily bars over the minute base stream every minute | `02-chat.md:205` | Open. Package 2a |
| 4 | The futures data service idles at 90% CPU and 16 GB | `02-chat.md:115` | Open, not diagnosed |
| 5 | Opening a store replays its log: about 126M rows and 5 GB on the futures store | `01-mvp.md:182` | Fix uncommitted in the amendments tree |
| 6 | Point values 100 times too large on 17 roots; the store refuses to restate a fact | `02-chat.md:101` | A rebuilt store exists, not verified, not served (`02-chat.md:211`) |
| 7 | A run's records (4,254 relations) cannot be copied between stores | `02-chat.md:211` | Open |
| 8 | The full equity store is disabled: one binding took the service to 45 GB in three minutes | `01-mvp.md:125` | Open (E-120) |
| 9 | Contract specs: no tick size in the minute store; a few old contracts have no point value | `02-chat.md:110` | Open |
| 10 | Missing data: a daily VIX series, ETFs, industry classification; equity bars end 2026-10-14 | `02-chat.md:111`, `02-chat.md:209` | Open, blocked by 8 |

### Kernel

| # | Symptom | Source | State |
| --- | --- | --- | --- |
| 11 | Memory per run: runs killed at the 14 GB slot cap | `02-chat.md:205`, `01-mvp.md:120` | Open. PR #80's retention horizon covers `abt run`, not the `abt plan run` the pipeline issues (section 5). Change (a) under package 7 |
| 12 | The 3,000 s tool limit: a single-root daily run takes 5 to 8 minutes | `02-chat.md:113` | Open. Speed: packages 2 and 3 |
| 13 | Intraday speed: 1.2 ms a minute bar per root | `01-mvp.md:162` | Open. Package 6 |
| 14 | Bracket and contingent orders are rejected by the checker | `02-chat.md:202` | Open. Language |
| 15 | An MOC exit is booked a session late | `02-chat.md:207` | Open. Simulator defect |
| 16 | `true_range@session` is empty on the Databento store | `02-chat.md:114` | Open |
| 17 | `contract()` reads nothing; the agreed `adjustment` relation is not built | `01-mvp.md:161` | Open |
| 18 | Non-USD futures (NIY, CNH, RP, RY) are accounted as dollars | `02-chat.md:109` | Open. The engine is USD only (A-D32) |
| 19 | The equity position-limit invariant halted runs | `01-mvp.md:159` | Fixed as E-119 in PR #76 |
| 20 | N-hour bars had no form | `01-mvp.md:160` | Fixed as E-124 in PR #78, UTC-aligned |
| 21 | A `ledger R into` returns nothing; a `session(X, [5min, …))` form panics the kernel; the honesty property was not rerun on the final engine | `01-mvp.md:91` and the pipeline's skills | Open |

### Query layer

| # | Symptom | Source | State |
| --- | --- | --- | --- |
| 22 | A development plan needs about 2.5 GB per grid cell on 750 names | `02-chat.md:112` | Open |

### Fixed in iteration 1

| # | Symptom | Source | State |
| --- | --- | --- | --- |
| 23 | A quadratic loader, services holding 40 GB, a 1 MiB reply limit, negative Sharpe ratios passing a claim, grids silently running the default, held-out trades dropped, roll commission never charged, rows lost on rebuild, ingest memory following the store | `01-mvp.md:182`; ERRATA E-104 to E-123 | Fixed in PRs #76 and #77 |

Blockers 2, 3, 11, 12 and 13 are the ones that kill runs. Package 2a reaches 3. Packages 2 and 3 reach 12, and package 6, which is the same two applied to minute data, reaches 13. Blockers 2 and 11 need two changes that are in no speed package; the roadmap lists them under package 7. Blockers 1 and 8 are the data service's memory (E-120), and 5 is the store's open (package 0, step 6).

## 9. Loose ends

### This tree (`/Users/alejandro/absolute-backtest-m8`)

- **16 uncommitted files**, last touched Nov 6. They change `pub type Tuple` from `Vec<Value>` to `smallvec::SmallVec<[Value; 4]>` (`crates/abt-kernel/src/kernel/mod.rs:39`) across the kernel and add the `smallvec` dependency. The experiment was in progress when the session was stopped. It was never built or measured, and it does not touch `crates/abt-tests`, whose 61 `ds.add("…", vec![…])` calls would no longer compile against it.
- The three batch paths of Nov 6 (commits `09f0c0b`, `5dd1a64`, `479782a`) have no erratum. `docs/assessment.md` credits them to E-125, whose text does not describe them.
- The handoff documents (`CLAUDE.md`, `docs/handoff/`) are untracked. `docs/handoff/smallvec-nov6.patch` is a copy of the 16 files' diff, taken Nov 24.

### The amendments tree (`/Users/alejandro/absolute-backtest-amend`)

- **An unpushed fix, uncommitted since Oct 20.** `crates/abt-data/src/store.rs` and `crates/abt-data/src/store_hot.rs` (`Store::each_log_batch`, `replay_kept`): a replay no longer holds the whole log after a consolidation. Its own erratum leaves one thing open: the replay still reads the log from its start point at every open. With it come a new test (`crates/abt-data/tests/replay.rs`, untracked), a mutation patch (`scripts/mutations/e-125-replay.patch`, untracked), and a fix to the flaky test `readers_at_a_sequence_keep_their_view_under_appends` in `crates/abt-data/tests/appends.rs`.
- **Its errata are numbered E-125 and E-126**, which collide with PR #80's. They need new numbers (E-128, E-129) when brought over. PR #80 changes no file in `abt-data`, so the code applies cleanly.
- **A stash**: "E-120 bound wip (scan ranges; the other instance, stopped 15:20 UTC)", 8 files, +246 −26. Unfinished work on the per-binding bound. The four engine trees are worktrees of one repository and share the stash list, so this stash is `stash@{0}` in every tree, and a stash made anywhere else would take that place.
- The local branch is four commits ahead of `origin/amend/pipeline-mvp`, all four already on `spec/three-layer-split`.
- Whether the pipeline machine's build includes the uncommitted fix cannot be told from here: the machine's copy has no usable git metadata.

### The owner's checkout (`/Users/alejandro/absolute-backtest`)

Listed for completeness; nothing here is to be changed. An uncommitted edit to `tests/fold.rs`, an untracked first-formulation strategy (`corpus/strategies/polarity_retest.dsl`), and an untracked experiment (`experiments/unicorn-edge/`) whose seven runs never finished. None of it runs on the three-layer engine.

### GitHub

- The default branch is still the old engine, so the repository's front page shows code nobody builds.
- `main` is a stale copy of the integration branch.
- Issue #73 (spin-off distributions a session early) is open and untouched.

### AWS

- `m8-bench2` is stopped, has no instance profile, and its SSH key's private half was in a scratch directory that no longer exists. Its key pair (`m8-bench2`) and security group (`sg-0ad78c062ca2bff80`) remain.
- The pipeline machine's disk is 98% full (6.2 GB free on Nov 24). It still serves the futures store with the wrong point values (`store-futures-current` points at `store-futures-live-50`).

## 10. Contradictions in the documents

To be corrected with package 0, in the same commit as the code they describe.

| Where | Says | Is |
| --- | --- | --- |
| `README.md:1319` | "M-8's entity-parallel strata were not built" | Built one level down (E-125) |
| `README.md:88` | Decisions "A-D1 to A-D46" | A-D1 to A-D56 |
| `docs/spec/TRACE.md:3` | "(A-D1 to A-D46)" | The table has rows to A-D56 |
| `docs/spec/00-architecture.md:7` | "A-D1 to A-D50" | Its own table goes to A-D56 |
| TRACE line 96 (X-3.1) | missing, "not built" | Line 113 records it partial |
| TRACE line 224 (MW14 full replay) | missing, with iteration 1's killed run | Finished on Nov 6: 367 s, 3.7 GB |
| TRACE line 114 | "front 3.6 GB" | `docs/assessment.md` and PR #80 say 3.7 GB |
| TRACE line 47 (A-D35) | "the vendor adapters … still write Parquet" | They stage hot (A-D49, E-91) |
| TRACE line 49 (A-D37) | The ladder is missing | Built (E-89); what is missing is a reader |
| `docs/parity-mw14.md` | Inputs under `~/data/ref/mw14/` | That directory does not exist on this Mac (`README.md` of this handoff has the paths) |
| `docs/parity-mw14.md`, "Reproduce", second block | The weekly bundle is ingested and `corpus-v2/company/mw14.dsl` run on it | The program demands daily bars from a reviewed store; whether the recipe binds has not been tried (section 4) |
| ERRATA E-126 | The retention horizon is the default | For `abt run`. A plan's runs are not trimmed (section 5) |
| `docs/assessment.md`, lines 340 to 344 | The gap to Python is "not the stream" | Its profile sees only the front's CPU, not the 75 s wait in the same run's clock (section 3) |
| The pipeline's `01-mvp.md:182` | "E-125" is the log replay | In this tree E-125 is parallel evaluation |
| `docs/spec/02-kernel.md`, `01-data.md` | `exact` is the default numerics for new programs | The kernel evaluates it as `two_pass` (E-85) |
| `docs/spec/07-storage.md` ST-8.8 | A tail rebuild takes under 10 s for 50M rows | 34 to 70 s measured for 46M (E-93) |

## 11. The pipeline today

Read on Nov 24 from the pipeline machine (read-only). 252 posts have been submitted since Oct 19.

| How the post ended | Posts |
| --- | --- |
| Ended at the analyst stage (a post with no testable claim ends here) | 80 |
| Engine backtest by the pipeline's own quant model: finished | 5 |
| Engine backtest by the pipeline's own quant model: failed | 113 |
| Engine backtest driven by hand by an Opus agent: finished | 25 |
| Engine backtest driven by hand by an Opus agent: failed | 20 |
| Answered from Python, outside the engine | 9 |

Of the pipeline's 78 futures posts, 2 have a verdict and 28 were killed for memory or the 3,000 s limit (`/Users/alejandro/absolute-pipeline/docs/iterations/02-chat.md:205`). The call path and what the engine must do before a redeploy are in `/Users/alejandro/absolute-pipeline/docs/iterations/03-engine-handoff.md`.
