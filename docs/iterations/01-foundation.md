# Iteration 1 · Foundation: verify, trace, the hot store

Oct 18, 2026. The first of the iterations that implement, verify and deploy `docs/spec/`. Each iteration runs five phases in order (verify, fill gaps, test, performance, deploy) and ends with the report at the bottom of this page. The scope of this one is M-0 to M-2 as `05-migration.md` now states them: the workspace and its three programs, `abt-wire`, `abt-agg`, the new `abt-codec`, staging S0–S6, the hot store, appends, consolidation, recovery and the replay stream over it. Its exit criteria: G-ST1 and G-ST2 measured, G1 on bundles imported through `bundle_v2`, G5, G3 in-process against socket, and the store tests of ST-13 green.

## Where the tree starts

The migration M-0 to M-10 of the Oct 12 specification landed before this iteration (`docs/spec/ERRATA.md`, E-1 to E-82). The spec then gained `07-storage.md` (Oct 15) and its amendments to the other documents; none of it is implemented. `docs/spec/TRACE.md` holds the row of every clause with its status at the start of this iteration: every ST row is missing, one row contradicts the documents (A-D2: one binary instead of three programs, E-81), and two deviations are noted in partial rows (E-77 entity-parallel strata, E-82 polled broker reports).

## The machine

Every number in this page was measured on the build container: a 4-core Intel Xeon 2.1 GHz virtual machine with AVX-512, 15 GB of memory, Linux 6.18, Rust 1.97. It is not the reference machine (Apple M4, 10 cores, 16 GB). Its hard `memlock` limit is 8 MB, so the store cannot be locked here: residency, G-ST1 and the figures of ST-2.4 are measured and labelled, and the gates that need the M4 stay open for the owner.

Vendor data comes from the company's S3 buckets through the AWS connector: the NDLake history in full (`curated/bars_by_date`, 132 year files, 4.20 GB, watermark 2026-10-15) and its registry; for Databento, which the bucket holds as 117,677 per-day files that the connector can presign only 25 at a time, the six study roots (ES, ZN, ZT, CL, GC, 6J) for 2024 and 2025-03-03/04 with every definition. The full Databento history needs one consolidated object per root in the bucket, or the sizing run on the owner's machine. No vendor file enters the repository.

## Plan

1. **Verify.** Merge the spec branch; rerun every gate (the in-process suite, the suite over sockets, the deployment test, `scripts/bench.sh` parts 2 and 3); write `TRACE.md` and this page; commit them before any code.
2. **Fill gaps, in migration order.**
   - M-0: the three programs `abt-data`, `abt-kernel`, `abt-query` (each layer's commands move into its crate; `abt` dispatches to them); `abt-wire` gains `Tick`, `Decimal128(18, s)`, `numerics exact`, `ticks`, and the `map` request.
   - M-1: `abt-agg` gains the `exact` mode over tick inputs.
   - M-2: the `abt-codec` crate (tick, block, predict, summary, rank, wavelet, adjust) with its property tests and Criterion benches; then the hot store in `abt-data` (segments, directory and block tables, manifests and versions, the calendar ladder, presence and rank, the catalog structures, event-interval summaries, the revision table, the query forms of ST-6.3), staging S0–S6 with `abt data stage --measure`, appends and the seal journal, consolidation, recovery, residency and memory profiles, the `map` request; each source switched to the hot store in its own commit with the golden runs unchanged.
3. **Test.** Property tests for every proposition touched, each shown to fail once (`scripts/mutations/`, `scripts/mutate.sh`); the rows of ST-13; crash injection with `kill -9` during appends, seals, consolidation and staging; G3 and G5 over the hot store.
4. **Performance.** The codec benches, the encoding study's benches and scripts on the fetched samples, `scripts/bench.sh` with the NDLake mirror, the sizing run; p50/p99, throughput, peak RSS and locked memory against A-7.3, ST-2.4 and ST-9.
5. **Deploy.** The three programs over Unix sockets in the `workstation` profile with a deployment document, the store prefaulted; `kill -9` of the data process and restart-to-serving at most 30 s; a pinned binding reproduced byte for byte before and after; `replay-sim` and `live-sim`.

Spec gaps met on the way are amended in the documents with a new decision row (A-D47 onward) and listed in the report.

## Report

Oct 18, 2026, on the machine above, branch `claude/practical-archimedes-nb8pqs` from f4a0d3f (the merge of the spec branch) on; the pull request targets `spec/three-layer-split`. Every commit passed `cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings` and `cargo test --workspace --no-fail-fast` before it was pushed, and CI (`.github/workflows/ci.yml`) is green on each.

### Phase 1 · Verify

The spec branch merged in f4a0d3f; `README.md` keeps the M-9c text with its `docs/spec/` row extended by 07's status. The gates were rerun before any new code (release binary of c06733f for the bench, debug for the suites):

| Gate | Command | Result |
| --- | --- | --- |
| the in-process suite | `cargo test --workspace --no-fail-fast` | 117 test binaries: 739 passed, 0 failed, 2 ignored |
| the suite over sockets (E-81) | `ABT_TRANSPORT=socket cargo test --workspace` | 744 passed, 1 failed, 2 ignored: `abt-tests/tests/time_literals.rs::no_second_literals_outside_the_time_modules`, a lint over the tree's sources, tripped by a scratch file present in the tree at the time; it passes in every later run of the suite (phase 5 reruns the socket suite over the final tree) |
| the three-process deployment | `cargo test -p abt --test deployment` | passed (in the suite) |
| `scripts/bench.sh none OUT`, parts 2 and 3 | `SYNTH_RUNGS="100 300" BUDGETS=1` | momentum_12_1 on 100 synthetic names: run 12.1 s, 208 bars/s, 801 MB peak RSS; on 300 names: 36.3 s, 69 bars/s, 2,339 MB; log-log slope against names 1.00 in run time, 0.98 in memory. The budgets part wrote an empty log (rerun in phase 4). |

The two ignored tests are `crates/abt-data/tests/budgets.rs:63` (`measure_the_budgets`, run by `scripts/bench.sh` part 3 in release) and `crates/abt-tests/tests/eval_set.rs:58` (`time_the_two_evaluators`, a timing comparison, not a check). `docs/spec/TRACE.md` (c06733f) traced 177 rows: 102 done, 12 partial, 52 missing, 1 contradicts.

### Phase 2 · Fill gaps

Eight gated commits in migration order, each with its errata entry (`docs/spec/ERRATA.md`) and, where the spec left a choice, its decision row (`00-architecture.md` 3):

| Commit | Scope | Record |
| --- | --- | --- |
| 0b4e1d4 | M-0: the three programs `abt-data`, `abt-kernel`, `abt-query` (each layer's commands in its crate; `abt` dispatches); the deployment test and `scripts/bench.sh` run the three binaries | E-83; closes E-81 |
| fcfe23f | M-0: protocol 1.1: `Tick`, `Numerics::Exact`, `ticks` on `QueryRequest`, `Decimal128(18, s)`, the `map` request | E-84 |
| b5e8e33 | M-1: `abt-agg`'s `exact` mode over tick inputs (`i128`, 256-bit for `Σx²`, one correctly rounded division), tested against the rational reference | E-85, A-D47 |
| b777d38, 1f9adaf | M-2: the `abt-codec` crate: ticks and quanta, lane-interleaved blocks at every width with patched exceptions, prediction, block and cumulative summaries, rank directories, wavelet matrices, adjusted views, the 32-byte header; property tests and Criterion benches | E-86 |
| b8fa2b3 | M-2: the hot store's segments, directory, manifests and versions, presence, staging S1 to S5 (classify, build, verify by decoding, atomic publish), the query forms over blocks, every market adapter's bar relations staged hot | E-87, A-D48 to A-D50 |
| 26b8e3d | M-2: ticks on the wire (`Decimal128` answers) and the exact aggregate pass over the hot store | E-88 |
| 6d8fdcc | M-2: the calendar ladder of a hot bar relation, each level from the nearest finer one, verified against the base rows on every ingest | E-89 |
| fcd0714 | M-2: appends and tails, the seal journal with generations, append files, consolidation, recovery, residency and the `--profile` flag, the `map` request, the `stats` rows | E-90, A-D51 |

The TV golden (`abt-tests/tests/tv_golden.rs`) and G6 stayed unchanged through the switch of every source to the hot store. Not built in this iteration, and said so in the trace: the sparse tables and wavelets behind the store's query forms (the codec has them; the store's forms read blocks and cumulatives), the catalog structures and standing event aggregates of ST-5.4 to 5.6, the event-interval summaries of ST-5.10, the S0 landing log and the S6 cold copy, the kernel's own exact windows (iteration 2 with M-3), the Norgate and Databento adapters' switch to the staging pipeline's S0 (they stage through S1 to S5 like every market adapter).

### Phase 3 · Test

- **Falsification.** `scripts/mutations/` holds twelve patches, one per proposition the iteration touched; `bash scripts/mutate.sh` applies each, runs the test its header names, requires the failure and reverts. Every patch makes its test fail (4b758b2; the record is in TRACE's note column). The first ST-3.2 patch did not: the encoder verifies every reconstruction against the source bits after the grid test, so a value rounded onto the grid still became a raw exception and the block stayed lossless; the patch now drops both guards, as its header says. The ST-3.3 row named a test that did not exist; `sums_are_exact_across_quanta` now holds it.
- **New tests.** `abt-codec/tests/roundtrip.rs::sums_are_exact_across_quanta` (Proposition 3.3 over three quanta, whole blocks and row ranges, a sum past `i64`); `abt-data/tests/determinism.rs` (D-2.7 and D-4.2: every query form at one sequence, with and without a receipt filter, through the version and through a binding, reread between appends that seal, after a consolidation and after a revision builds a new version: the same bytes); `abt-data/tests/appends.rs` (seven tests: appends against a Parquet twin, readers on three threads at recorded sequences, restart with a torn journal and junk past the last block, consolidation twice with a revision on the folded base, the automatic consolidation, the `map` request decoded with `abt-codec`, the `stats` rows); `abt-data/tests/recovery.rs` (a child process aborted at each of five fault points and killed at four random moments; every answer at every durable sequence equals a never-killed twin's); `abt-data/tests/hot.rs` and the ladder test; the exact pass tests of `abt-data/tests/agg.rs`.
- **ST-13.** Codec identity at every width, the ladder, appends, revisions, recovery and the exact aggregates of the codec are done; ranges, consolidation (the memory bound is by construction, not measured), adjusted ranges (the codec half) and ticks on the wire (the data layer's half) are partial; the real-data round trip and the sizing run are phase 4's; calendars (DST weeks, Sunday sessions, early closes, holidays, session-table changes) wait for the session table (iteration 3); residency's lock is refused on this VM. The rows and their evidence are in `TRACE.md`.
- **G3 and G5** run over the hot store: the suite's synthetic markets stage their bars hot (`abt::ingest_dataset` through `adapter::hot_bars`, A-D49), so `determinism.rs` (in process against a socket, four kernels on one service, one against eight data threads) and the `fold*.rs` tests read blocks; under appends, `determinism.rs` and `appends.rs` in `abt-data`.
- **Counts** (`cargo test --workspace --no-fail-fast -j 3` at 4b758b2, 10 min wall): 132 test binaries, 803 passed, 0 failed, 2 ignored (the two above). Known answers (Q-4.6) and the corpus (G2) are in the suite and pass.
- **TRACE** at 4b758b2: 119 done, 26 partial, 22 missing, 0 contradicts (the E-81 row closed by M-0; E-77 and E-82 stay as noted deviations in partial rows).

### Phase 4 · Performance

Every figure below is from this VM (the machine above), release binaries of 375332a, one measurement at a time, nothing else running.

**The vendor data.** NDLake in full: `curated/bars_by_date` (132 year files, 1896 to 2026), `archive`, the registry, the index membership and the classification, 4.0 GB under `data/vendor/ndlake/` (git-ignored; `data/README.md` says what is where). Databento: the six study roots on 2025-03-03 and 2025-03-04, ES and CL over March 2024 (20 trading days each), the 28 `GLBX.MDP3` definitions and `manifest.csv`, 21 MB under `data/vendor/dbn/`; every file's SHA-256 matches the manifest. Nothing was downloaded twice and nothing is deleted by the tooling: the stores built from these files live under `data/store/`.

**The sizing run (ST-7.4, G-ST2's harness).** The full NDLake history does not fit this VM's memory through the Norgate adapter (it holds every bar of the lake in Arrow before staging, and S2 reads the current version whole), so the run covers the S&P 500's members since 2014-01-01, the span `scripts/bench.sh` uses:

```
abt-data ingest --store data/store/ndlake-2014 --adapter norgate --from data/vendor/ndlake \
  --start 2014-01-01 --exceptions scripts/reviewed/norgate-spx-2014.parquet --attributes reviewed=complete --measure
abt-data stats --store data/store/ndlake-2014 --measure
```

| Measured | Value |
| --- | --- |
| Universe | 756 assets, 2,080,091 bars from 2014-01-02 to 2026-10-14; 146 splits, 24,477 dividends, 153 delistings, 769 membership intervals, 383 reviewed exceptions |
| Ingest, whole process (the adapter's read, S1 to S5 of five bar relations, the Parquet relations, the log) | 100.2 s wall, 87.2 s user, peak RSS 7,071 MB |
| Staging per bar relation, S1 to S5 (`open`) | 96, 255, 1,495, 1,252, 782 ms (normalise, classify, build, verify, publish) |
| S4 per bar relation | 5,194 blocks and 4,160,182 values decoded against their source, 5,194 summaries recomputed, 581,225 random ranges checked, 756 instruments; the ladder's 575,177 bars (four levels) each against its base rows |
| Base value column (`open`, `high`, `low`, `close`; 2,597 blocks) | 9.27 to 9.30 MB: 4.46 B per value |
| Base value column (`volume`) | 8.47 MB: 4.07 B per value |
| Receipt residual, per relation | 0.57 MB: 0.28 B per row (the residual is constant under the receipt rule; the bytes are the lane layout's floor per block) |
| Presence (labels, bitmaps, sequence runs), per relation | 0.34 MB: 0.16 B per row |
| Manifest; directory (one 16-byte locator per block, in memory) | 0.13 MB; 0.28 MB (17,290 locators) |
| Ladder, per relation (week, month, quarter, year: value, count, sequence, residual, presence) | 5.3 + 5.7 + 6.0 + 5.9 MB = 22.9 MB: 11.0 B per base row |
| Hot bytes per bar relation (base, ladder, presence, manifest, directory) | 33.4 to 33.9 MB: 16.0 to 16.3 B per row |
| Hot segments on disk, the five relations (the ladder's count, sequence and residual segments are shared by hash) | 111 MB; `hot/` 115 MB |
| The store on disk | 762 MB: the write-ahead log 608 MB, Parquet relations (`member` 1.6M rows, `listed`, `ticker`, `delisted`, `dividend`, `split`, `classification`, the registry) 37 MB, manifests 1.6 MB |

Against ST-9's table (148M Norgate rows in 1.3 GB, 1.76 B per value for five columns): the base columns measure 4.46 B per value here, two and a half times the study's `for_bpv` of 1.6 (ST-2.2, blocks of 184 rows at the value's own exponent). Three causes, each measured:

1. **NDLake's prices are `f32` adjusted values whose exact decimals run to eight places** (the study's exponent distribution for 1995: 14 % of close values at exponent 0, 37 % at 2, 29 % at 3, 2 % at 4, 0.7 % at 5, and a tail to 8; the 2014 store's version exponent for `close` is 6). A block of 1,024 rows takes the finest exponent any of its values needs (Proposition 3.2 is per block), so one value at 10⁻⁶ puts four years of a stock's prices at 28 bits each. The codec's width rule (ST-4.4) patches wide *offsets* as exceptions but does not trade the *exponent* against raw exceptions; doing so (a cost-based exponent, the off-grid values as raw-bit exceptions at 74 bits) would halve the base columns but make `exact` unavailable on such blocks (A-D49's rule: a raw exception refuses `exact` for the column). This is an owner decision for iteration 2 (open question 5).
2. **Each instrument's last block is padded to 1,024 rows by the lane layout** (ST-4.1 lets only the last block be short): 756 of the 2,597 base blocks are partial, and every ladder level holds one padded block per instrument and column, so the four coarse levels cost 11 B per base row, more than the base itself, and the yearly level 321 B per bar. The ladder is 0.35 GB in ST-9's table for the full history; with one padded block per instrument, level and column, its cost scales with instruments (about 3.5 KB each), not rows. A short-block layout for coarse levels, or levels built on demand below a row count, is iteration 2's (open question 5).
3. **The log is kept whole after publication**: 608 MB for 2.1M bars of five columns and the Parquet relations, four times the hot store. ST-9's "append logs and live tails 0.5 GB" counts the live path only; the historical ingest's log can be truncated at `replay_from` once a version is published (`abt data gc`, iteration 2).

A projection to the full history would multiply 4.5 to 4.9 B per value by about 148M rows and five columns (3.3 to 3.6 GB for the base columns alone) and the ladder by the number of instruments ever listed; it is a projection, not a measurement, and the run on the owner's machine replaces it. The Databento half is not measured: the bucket's per-day objects (open question 1).

**The real-data round trip (ST-13).** Every block of the five bar relations decoded bit for bit against its source in S4 (4,160,182 values per relation, 20.8M in all), every summary recomputed, 581,225 random ranges per relation checked, and 575,177 ladder bars per relation recomputed from their base rows: `verified` in the staging reports above. The full history and the Databento futures are open with the sizing run.

**The codec's benches (`cargo bench -p abt-codec`, 375332a; Criterion medians, 100 samples each).** `decode_and_sum` runs over 32,768 blocks (33.5M values) at six widths; the figures are per value.

| Width | Packed decode + sum | Raw `u32` scan | One value (`extract`) |
| ---: | ---: | ---: | ---: |
| 5 | 0.405 ns | 0.438 ns | 24.7 ns |
| 8 | 0.279 ns | 0.400 ns | 27.8 ns |
| 11 | 0.461 ns | 0.419 ns | 35.5 ns |
| 14 | 0.466 ns | 0.519 ns | 37.8 ns |
| 20 | 0.506 ns | 0.399 ns | 38.5 ns |
| 27 | 0.502 ns | 0.519 ns | 48.0 ns |

ST-2.4's study measured 0.18 to 0.38 ns per value for the packed decode and 0.63 to 0.85 ns for an `i64` scan on a 2-core 2.8 GHz VM; here the packed decode runs at the speed of a raw `u32` scan, 0.28 to 0.51 ns per value, with the two inside each other's noise at every width (the run-to-run spread of the raw scan alone is 12.3 to 16.7 ms at width 5). The target rows of ST-2.4 are the M4's.

Range operations at width 11, rows chosen at random over the length named (the packed form against a raw `u32` scan of the same rows):

| Operation | Rows | Packed | Raw scan |
| --- | ---: | ---: | ---: |
| window sum from cumulatives, edges decoded | 5 to 255 | 4.03 µs | 0.14 µs |
| | 250 to 2,250 | 23.5 µs | 0.53 µs |
| | 2,000 to 22,000 | 16.2 µs | 3.79 µs |
| series sum | 1 to 32 | 1.09 µs | 0.009 µs |
| | 32 to 1,024 | 15.9 µs | 0.27 µs |
| | 1,024 to 65,536 | 14.4 µs | 11.2 µs |
| range max (sparse table, edges decoded) | 1 to 32 | 0.60 µs | 0.03 µs |
| | 32 to 1,024 | 1.39 µs | 0.37 µs |
| | 1,024 to 65,536 | 2.11 µs | 10.5 µs |
| OHLCV bar | 1 to 32 | 4.21 µs | 0.19 µs |
| | 32 to 1,024 | 21.2 µs | 1.47 µs |
| | 1,024 to 65,536 | 23.1 µs | 72.1 µs |
| median (wavelet matrix against copy and select) | 20 to 300 | 0.82 µs | 0.65 µs |
| | 300 to 3,000 | 0.45 µs | 3.53 µs |
| | 3,000 to 30,000 | 0.36 µs | 33.8 µs |
| count below (wavelet against a scan) | 20 to 300 | 0.23 µs | 0.29 µs |
| | 300 to 3,000 | 0.20 µs | 1.38 µs |
| | 3,000 to 30,000 | 0.21 µs | 13.1 µs |

The structures do what ST-6 says above a few thousand rows: a range max over up to 65,536 rows in 2.1 µs against a 10.5 µs scan, a median over 30,000 rows in 0.36 µs against 33.8 µs, an OHLCV bar in 23 µs against 72 µs. Below that the packed forms cost more than a scan of the raw rows, and the window sum from cumulatives costs 16 to 24 µs where ST-2.4's study measured 0.6 to 4 µs for the same operation in `lanes-bench extra`: the crate decodes whole edge blocks where the study's bench decodes the shorter edge's rows only. The study's benches run on this VM below for the comparison on one machine; closing the gap (row-range decoding at the edges, ST-5.1's shorter-edge rule inside the crate's `summary` path) is an iteration 2 item, not a regression against `docs/assessment.md`, whose rows are the kernel's.

**The encoding study on this VM (`scripts/study/encoding`, the samples under `data/study/` as links into the vendor files, `python3 -I`; `pcodec` from PyPI).** The Python studies reproduce ST-2's sizes (sizes do not depend on the machine; the pcodec speeds do):

| Column | Rows | Lanes, FOR, blocks per instrument and year (B per value) | With the block GCD | Parquet as stored | pcodec | Exponent p50 / max |
| --- | ---: | ---: | ---: | ---: | ---: | --- |
| NDLake 1995 close | 3.06M (13,989 assets) | 1.626 | 0.906 (GCD p50 125) | 1.99 | 0.80 (`f32`), 0.65 (ints) | 3 / 8 |
| NDLake 2024 close | 5.43M (20,905 assets) | 1.714 | 1.601 (GCD p50 1) | 3.27 | 1.77 (`f32`), 1.24 (ints) | 3 / 6 |
| NDLake 2024 volume | | 2.689 | | 2.84 | 2.37 | |
| ES close (2025-03-03/04) | | 1.648 | 1.142 (GCD 25) | 2.36 | 0.90 | 2 / 2 |
| ZN close | | 2.541 | 0.816 (GCD 15,625) | 1.00 | 0.36 | 6 / 7 |
| ZT close | | 3.103 | 0.766 (GCD 390,625) | 1.24 | 0.40 | 8 / 8 |
| CL close | | 0.832 | 0.829 | 1.60 | 0.58 | 2 / 4 |
| GC close | | 0.946 | 0.946 | 2.09 | 0.78 | 1 / 1 |
| 6J close | | 1.190 | 0.924 (GCD 3) | 2.02 | 0.71 | 7 / 7 |

Every NDLake value is an `f32` and round-trips under the `f32` rule (`roundtrip_exact_share` 1.0); the Databento ticks round-trip exactly through the definitions' tick sizes for 55 to 100 % of rows per root (the rest are contracts without a definition in the sample: 4 to 76 % of CL's rows are spreads). pcodec decodes at 2.3 to 5.4 ns per value here, ten times the lane decode. The prediction study: OHLC from the close takes NDLake 2024 from 1.543 to 1.387 B per value, 1995 from 0.828 to 0.667; ES from 0.903 to 0.695, CL from 0.631 to 0.448, ZN from 0.586 to 0.379, GC from 0.803 to 0.604; the ORATS sections are skipped (no sample).

The study's 1.6 to 1.7 B per value for NDLake closes against the store's 4.46 is the block length: the study's blocks are one instrument's year (184 rows on average) at the exponent its own values need (p50 3), the store's are 1,024 rows (about four years of one stock) at the finest exponent among them, and NDLake's `f32` adjusted prices put a value at 10⁻⁵ or 10⁻⁶ into most of them. Cause 1 above, and open question 5.

**The study's Rust benches on this VM** (`scripts/study/encoding/{lanes,raw,real,codec}-bench`, `cargo run --release`, single-threaded, best of 9; 33.5M values per test where the study says so):

| Bench | Measured here |
| --- | --- |
| `lanes-bench`: packed decode + exact sum, widths 5, 8, 11, 14, 20, 27 | 0.191, 0.249, 0.254, 0.349, 0.410, 0.404 ns per value; raw `u32` scan 0.29 to 0.41; raw `i64` scan 0.81 to 0.92; `f64` canonical-order sum 1.18 to 1.26 |
| `lanes-bench extra`: encode (min, GCD, width, pack) | 8.8 (w8), 9.1 (w11), 11.5 (w20) ns per value |
| `lanes-bench extra`: random window sum, cumulative block sums with the shorter edge decoded, width 11 | 663 ns (5 to 255 rows), 717 ns (250 to 2,250), 669 ns (2,000 to 22,000); scanning raw `u32` 132, 579, 3,327 ns |
| `lanes-bench range`: point extract; series decode + sum, batch sorted, range max, OHLCV bar over 1 to 32 rows / 32 to 1,024 rows | 20.1 ns (raw load 15.8); 203 / 542 ns series (raw 34 / 352), 75 / 253 ns batch, 233 / 640 ns range max (raw 68 / 550), 855 / 2,264 ns OHLCV bar (raw 268 / 2,422) |
| `raw-bench`: NDLake 2024 closes, 4.19M values, resident uncompressed | exact `i64` scan 0.376 ns per value; `f64` scan in canonical order 0.760; rolling 252-row mean 1.58 ns per output by sliding state, 2.36 by prefix sums; exact rolling variance 7.13; random window mean 14.5 ns by prefix sums against 178.5 ns scanning |
| `real-bench`: the real columns packed with the block GCD, fused decode + exact sum | NDLake 1995 close 0.91 B per value (median width 6), 1.72 ns per value; NDLake 2024 close 1.60 B (width 11), 1.41 ns; 2024 volume 2.69 B (width 20), 1.78 ns; ES close 1.14 B (width 8), 1.43 ns; CL close 0.83 B (width 5), 1.30 ns; the exact `f32` rebuild gives the source bits on every column; the ORATS rows stop the run (no sample: exit 101 after the rows above) |
| `codec-bench` (synthetic first pass, 16.4M values) | ALP+FOR block to exact `i64` sum 1.69 B per value at 1.29 ns; ALP+delta 1.36 B at 1.45 ns; `f64` canonical sum 1.07 ns; exact `i64` tick sum 0.58 ns; exact `i128` sum of squares 0.72 ns |

Two comparisons on one machine, the study's bench against the crate: the decode + sum kernel runs at 0.25 ns per value in `lanes-bench` and 0.46 in `abt-codec` at width 11 (the study's kernel is a const-generic width, the crate's a dispatch over generated kernels; best of 9 against a Criterion median), and the random window sum from cumulatives takes 0.7 µs in the study's bench and 4 to 24 µs in the crate, whose `summary` path decodes whole edge blocks. Both are iteration 2's (the crate's row-range decode exists in `block::unpack_rows` and is not yet what `summary` uses); neither changes an answer.

{{PHASE4_BENCH_SH}}

### Phase 5 · Deploy

(Pending: filled in below once run.)

### Amendments

Each with the alternative rejected (the rows in `00-architecture.md` 3 say why):

| Row | Decision | Rejected |
| --- | --- | --- |
| A-D47 | `exact` over what has no integer form is `two_pass`; `std`, `corr`, `ols_r` take exact centred sums to `f64` before the root | a refusal of `exact` on float columns (every program with a derived series would lose the mode) |
| A-D48 | the segment file (`ABTSEG\0\1`, runs, run table, BLAKE3 trailer, named by hash) and the directory rule | a header array with offsets at the front (a segment could not be appended to, nor its hash computed in one pass) |
| A-D49 | which relations are hot: `storage = hot` on the bar relations of every market adapter; the rest in Parquet | every relation hot (the registry and the kernel's outputs carry labels, bytes and nulls the block format does not hold) |
| A-D50 | the receipt time as the integer residual `r − t`; rows numbered per instrument in label order with sequence runs | a receipt column of its own format (a second code path for one column that packs to nothing under a receipt rule) |
| A-D51 | appends beside immutable blocks: tails, append files, journal generations, consolidation by copy and re-encode of the partial block's rows; a row at a held label is a revision | re-encoding a tail in place under readers (an epoch scheme for a case the revision path already covers) |
| A-D52 | one deployment document for the rehearsal and the live run: a `live-sim` run takes it, its kill switch, deadlines and recording stream apply, the broker fields are logged and not acted on | a document of its own for `live-sim`, or refusing the broker fields (the rehearsal would not prove the document the live run uses) |

Errata E-83 to E-90 record how each of M-0 to M-2 was built against the documents' wording; E-91 (phase 5) records this iteration's phases 3 to 5.

### Open questions for the owner

1. **Databento's full history.** The bucket holds 117,677 per-day files and the connector presigns 25 at a time; the sizing run's Databento half needs either one consolidated object per root in the bucket or a run on the owner's machine with `aws s3 sync`. The subset here (the six study roots on 2025-03-03/04 and ES and CL over March 2024, every definition) is checked against `manifest.csv`; the four `IFUS.IMPACT` definitions in the bucket no longer match the manifest (newer objects) and were not used.
2. **G-ST1, residency and G10 on the M4.** This VM cannot lock the store (8 MB `memlock`), is not the reference machine, and the targets of ST-2.4 and A-7.3 are the M4's; the figures here are labelled and the rows stay open for the owner's run.
3. **G1 on the v2 books.** `~/data/ref` and the books' bundles are the owner's; the stand-in is `tv_golden.rs`.
4. **The ORATS samples** (ST-11) were not pulled: the encoding study's ORATS sections are skipped and said so.
5. **The exponent a block takes, and the ladder's padded blocks** (phase 4): NDLake's prices are `f32` adjusted values with decimals to 10⁻⁸, so a 1,024-row block takes an exponent of 5 or 6 and 28 bits per value where the study's 184-row blocks took 13; the four daily ladder levels cost one padded block per instrument, level and column. The choices for iteration 2, the owner's since they touch ST-3 and ST-4: (a) a cost-based exponent per block with the off-grid values as raw-bit exceptions, which halves the base columns and refuses `exact` on such blocks under A-D49's rule, or a per-block `exact` refusal instead of per column; (b) shorter blocks, or the study's instrument-year blocks, for daily data; (c) a compact block for a level with fewer than 1,024 rows per instrument, or coarse levels built on demand. The measured store stands as the baseline either way.

### Iteration 2 (plan)

Semantics (M-3 to M-5 of `05-migration.md` as amended): rerun the gates and the trace; the kernel's `exact` windows over `Decimal128` columns (the kernel half of ST-13's ticks on the wire); the store's query forms over sparse tables and wavelets (range min/max, median, quantile, count: ST-6.3 over ST-5.7); the catalog structures and standing event aggregates (ST-5.4 to 5.6) and the event-interval summaries (ST-5.10); the S0 landing log and the S6 cold copy; the contract, state-table and query-service rows marked partial; G1 on the v2 books (owner), G2, G3 complete, G7, G9; trace at or above 90 %.
