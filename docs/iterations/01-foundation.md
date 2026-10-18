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

(Pending the driver: filled in below once measured.)

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

### Iteration 2 (plan)

Semantics (M-3 to M-5 of `05-migration.md` as amended): rerun the gates and the trace; the kernel's `exact` windows over `Decimal128` columns (the kernel half of ST-13's ticks on the wire); the store's query forms over sparse tables and wavelets (range min/max, median, quantile, count: ST-6.3 over ST-5.7); the catalog structures and standing event aggregates (ST-5.4 to 5.6) and the event-interval summaries (ST-5.10); the S0 landing log and the S6 cold copy; the contract, state-table and query-service rows marked partial; G1 on the v2 books (owner), G2, G3 complete, G7, G9; trace at or above 90 %.
