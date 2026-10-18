# Iteration 1 · Foundation: verify, trace, the hot store

Oct 18, 2026. The first of the iterations that implement, verify and deploy `docs/spec/`. Each iteration runs five phases in order (verify, fill gaps, test, performance, deploy) and ends with the report at the bottom of this page. The scope of this one is M-0 to M-2 as `05-migration.md` now states them: the workspace and its three programs, `abt-wire`, `abt-agg`, the new `abt-codec`, staging S0–S6, the hot store, appends, consolidation, recovery and the replay stream over it. Its exit criteria: G-ST1 and G-ST2 measured, G1 on bundles imported through `bundle_v2`, G5, G3 in-process against socket, and the store tests of ST-13 green.

## Where the tree starts

The migration M-0 to M-10 of the Oct 12 specification landed before this iteration (`docs/spec/ERRATA.md`, E-1 to E-82). The spec then gained `07-storage.md` (Oct 15) and its amendments to the other documents; none of it is implemented. `docs/spec/TRACE.md` holds the row of every clause with its status at the start of this iteration: every ST row is missing, and three rows contradict the documents (E-77 entity-parallel strata, E-81 one binary instead of three programs, E-82 polled broker reports).

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

(Written at the end of the iteration.)
