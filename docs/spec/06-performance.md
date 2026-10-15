# 06 · Performance engineering: algorithms and data structures per layer

Oct 12, 2026 · what each layer is built from, why, and how the separated system reaches the targets of A-7.3. Tags and cross-references as in `00-architecture.md`. Complexity bounds use `n` rows, `E` entities, `w` window length, `k` rows returned, `B` bars, `R` rules, `P` threads.

---

## 1. The principle

Separation is paid for exactly once, at the boundary, and the boundary is designed so that the payment is zero on the hot path: tables cross it as Arrow buffers without copying (in-process) or as one sequential IPC write (sockets); control crosses it as zero-copy FlatBuffers; nothing crosses it per tuple or per rule. Everything else is the ordinary discipline of fast code: columnar layouts, dense integer ids, sorted-merge joins, incremental window states, no allocation per instance, work off the critical path, and a fixed order wherever a reduction touches floating point. Each layer is then optimised for its own access pattern, which is the gain the split buys: the store is a scan engine, the kernel is a column engine over a sliding window, the query layer is an embarrassingly parallel reader of immutable tables.

---

## 2. The data layer

### 2.1 Structures

| Component | Structure | Operation and bound | Why this one |
| --- | --- | --- | --- |
| Log (D-3.1) | append-only Arrow IPC stream file per (relation, stream); group commit of ≤ 4,096 rows or 10 ms live; one `write` and one `fdatasync` per group (fsync policy: every group live, every 64 MiB in historical ingest); footer index (seq → byte offset) every 4,096 rows | append O(1) amortised; seek to `seq` O(log n) then sequential | the log is the stream: replay is a sequential read at disk bandwidth with no merge |
| Hot tail (D-3.1) | per relation a ring of immutable Arrow batches held by `Arc`; index `t → (batch, start, len)` as a `BTreeMap<i64, SmallVec>` (arrivals are nearly time-ordered, late rows are rare inserts); index `(entity, t) → (batch, row)` as a Swiss table (`hashbrown`, `FxHash` on the packed 128-bit key) | lookup O(log n) / O(1); append amortised O(1); readers never block the writer | an `Arc` per batch and an atomic high-water `seq` give snapshot reads without locks: a reader clones the batch list at its `seq` and filters; batches are dropped only after compaction and when no reader holds them |
| Partitions (D-3.1) | Parquet, row groups of 65,536 rows sorted (`t`, `entity`, `r`), `zstd` 3, byte-stream-split for `f64`, dictionary + delta for ids and times; per row group zone maps (min/max per column); a **CSR entity offset index** per file (sorted entity ids + prefix offsets + `u32` row offsets, 4 B per row) in a sidecar `.idx`, memory-mapped | cross-section at `t`: binary search on the `t` column's zone maps then one contiguous slice, O(log n + k); series per entity: binary search in the sorted ids, O(log E), then a gather of `k` offsets | one physical order serves the three consumers (stream, cross-section, aggregate pass); per-entity reads are gathers, which `arrow::compute::take` does with SIMD |
| Resident cache (D-3.2) | decoded Arrow arrays per row group in an LRU (`clock` second-chance over a slab) under the resident budget; Parquet pages decoded lazily on first touch at 1–2 GB/s per core | hit O(1); miss = one row group decode | Parquet is not zero-copy; the hot history (the last 5 years @1m) stays decoded, the rest is paid on touch |
| Compaction (D-3.3) | take the tail's closed buckets, `sort_unstable_by_key` on the packed (`t`, `entity`, `r`) key (pdqsort: O(n) on presorted runs, O(n log n) worst), write the bucket's partition, build its CSR index, advance `S` | O(n log n) worst, O(n) typical | the tail is already nearly sorted; a radix sort gains nothing measurable here |
| Late rows (D-3.3) | delta partitions per bucket, merged on read by a 2-way merge on (`t`, `entity`, `r`), folded at the next full compaction | read O(k + d), `d` delta rows | a late row never rewrites a sealed file |
| Registry (D-2.4) | ids in `hashbrown` maps keyed by `u64`; vendor keys interned to `u32` labels; `(vendor, key) → sorted Vec<(from, to, id)>` with binary search on `t`; the label dictionary as an append-only `Vec<String>` plus a hash map | resolve (vendor, key, t) in O(log m) | identity attribution is on the ingest path for every row |
| Stream merge (D-5.3) | k-way merge with a `BinaryHeap` over per-relation cursors keyed by (`r`, relation id, identity); the calendar labels are one more cursor; batches assembled by Arrow builders and flushed at every barrier | O(n log k), `k` relations (≤ 20) | replay of a recorded stream is a sequential read with no merge; the merge runs only when a spec combines versions |
| Live path (D-5.4) | feed decoder thread → bounded MPSC ring (`crossbeam` `ArrayQueue`) → writer thread (group commit, `seq` assignment, log append, `Arc` publish of the tail batch) → per-subscriber bounded channels; barrier timer thread with a `BinaryHeap` of upcoming labels per calendar and the grace | per event O(1); barrier emission O(log calendars) | one writer, no locks on the path; the write-ahead order is the canonical order |
| Snapshot reads (D-3.3) | partitions carry a `seq` column; the tail filters by `seq ≤ N` on the batch's `seq` array with `arrow::compute::filter` | O(k) | exactness without MVCC bookkeeping |
| Predicates (D-4.1) | zone-map pruning per row group, then vectorised comparison kernels producing a boolean mask, then `filter` | O(n_touched) with SIMD | the predicate algebra has no joins by design, so every form is a scan with a mask |
| Content-addressed cache (D-3.5) | key = BLAKE3 of the canonical request; index as a relation (`cache.index`) loaded into a hash map at start; LRU by last-touch `seq`; pinned set as a bitmap over cache ids | hit O(1) | bar views and pushdown aggregates are computed once per version and shared by every run on it |
| Resolver (D-7.4) | offers sorted by span start; per need a DP over sorted endpoints (weighted interval cover) | O(m log m) per need | a handful of offers per need; exactness over cleverness |

### 2.2 The aggregate pass (D-6.4)

The pass is the one algorithm the data layer must get right, since it replaces the kernel's window re-solves.

1. **Dense slots.** The pass assigns each entity a dense `u32` slot in sorted-id order, so per-entity state is a `Vec<WindowState>` indexed by slot, contiguous and cache-resident (4,240 names × a few hundred bytes).
2. **Time-major scan with per-thread entity ranges.** Rows arrive in (`t`, `entity`) order as decoded column chunks. With `P` threads, thread `p` owns the slot range `[pE/P, (p+1)E/P)`. Each `t`-block is contiguous and sorted by entity, so thread `p` finds its subrange by two binary searches per block (O(log E)) and touches only its rows: no partitioning copy, no per-thread rescan of the data. Total work O(n + B·P·log E), bandwidth-bound.
3. **Window states** from `abt-agg` (P-1.2): a two-stacks deque for `min`/`max` (O(1) amortised), Neumaier running sums for `sum`/`count`/`mean`, the shifted compensated sliding sums rebuilt exactly every `w` rows for the moments in `running` mode and the two-pass formulas over a ring of the window's rows in `two_pass` mode, and for `median`/`quantile` a Fenwick tree over the entity's coordinate-compressed values (the pass is over a sealed version, so the value domain is known before the pass: O(log w) insert, delete and k-th; the live incremental form uses the order-statistics tree of 3.2).
4. **Preallocated output.** The input scan counts rows; the output arrays (`entity`, `t`, `value`, `count`) are allocated once with that length and each thread writes its own rows in place, so the output is already in (`t`, `entity`) order with no merge.
5. **Bucket aggregates** are the same pass with the window state reset at each bucket boundary of the target calendar and one output row per (entity, bucket).

**Proposition 2.1 [N] (the pass is deterministic under parallelism).** The output of the pass does not depend on `P`. *Proof.* Each entity's state is updated by exactly one thread in `t` order, which is the canonical reduction order for that entity (A-4.7); outputs are written to positions fixed by the input order. ∎

---

## 3. The kernel

### 3.1 Structures

| Component | Structure | Operation and bound | Why this one |
| --- | --- | --- | --- |
| Entity slots | per run, an interning of entity ids to dense `u32` slots on first appearance (`hashbrown` map plus a `Vec<u64>` back-map) | O(1) | every per-entity structure in the kernel (positions, marks, window states, last values, bitmaps) becomes a `Vec` indexed by slot |
| Stored tables E[R] for primitives (K-7.1) | per relation a `VecDeque<Arc<Chunk>>` over the retention horizon, a `Chunk` = one bar: sorted slot ids (`Vec<u32>`) and one `Vec<f64>` (or label `Vec<u32>`) per value column; a `Vec<(t, chunk_idx)>` for bar lookup; a per-slot `last: Vec<(t, chunk, row)>` for `latest` | point (slot, `t`): O(log E) binary search in the bar's chunk; cross-section: the chunk; `latest`: O(1); insert: amortised O(1) into the open bar's builder | the stream delivers whole bars; `Arc` chunks make the checkpoint and the query snapshot pointer copies (copy-on-write per bar) |
| Derived blocks (the memo) | per derived relation, per bar, one `Block` in the same chunk layout, indexed by (relation id, bar index) in a `Vec<Vec<Option<Arc<Block>>>>`; parametric relations keyed by (relation id, params hash, bar) in a small hash map | memo hit O(1) array index | the present memo hashes a `Vec<Value>` per call; set-at-a-time evaluation produces one block per rule application, so the key is a pair of integers |
| Universe and closed relations | per bar a bitmap over slots (`Vec<u64>`, one bit per entity) for `universe` and for every closed relation with no value columns | `not R(A, T)`: O(1) bit test; set algebra O(E/64) | negation and range restriction become bit operations |
| Joins between blocks | sorted-merge join on slot ids (both sides sorted by construction) | O(n + m) | no hash tables on the evaluator's path; the output is sorted, so the next join is also a merge |
| Tests and assignments | vectorised over the block's columns: a column-at-a-time interpreter over the expression tree with fused loops per rule (autovectorised by LLVM; `std::simd` where it measurably helps) | O(n) per literal | one loop per literal over contiguous `f64` columns, no environment per instance |
| Cross-sectional reductions (`within (T)`) | `count`/`sum`/`mean`: sequential over sorted slots (canonical order, A-4.7); `top[k]`: a bounded `BinaryHeap`, O(n log k); `rank`: `sort_unstable` of (key, slot) with the declared tie order, O(n log n) | | sequential where floating point is summed, so the result equals the data layer's and the single-thread evaluation |
| Rolling aggregates over derived relations | `abt-agg` window states in a `Vec<WindowState>` per (rule, literal) indexed by slot, advanced once per bar | O(1) amortised per entity per bar; O(log w) for order statistics | the present per-bar row cache re-solves rows; the state is the rows' summary |
| Order statistics live | a weight-balanced order-statistics tree in `abt-agg` (rank and select in O(log w)) | O(log w) | the live form cannot coordinate-compress ahead of time |
| Temporal recursion (K table) | per keyed recursive relation, the previous bar's block | O(1) | V-8.2 |
| `asof` on derived relations | per (relation, slot) the last bar with a tuple, a `Vec<i32>` of bar indices | O(1) | the present memoised walk back is replaced by an index maintained on insert |
| Window groups that cannot be reduced to a state (read the base time, several windows) | the present exact per-bar row cache, retained | O(w) per call | correctness over speed on the rare form |
| Provenance Δ (V-8.2) | per derived block, a CSR of input references: `Vec<u32>` offsets per row and `Vec<(block id, row)>` entries | O(body length) per row, no allocation per tuple | `why` is a walk over integer arrays |
| Failure table F | per (rule, bar), when `trace instances` is declared, a `Vec<(slot, literal index)>` | O(instances attempted) | |
| Per-barrier scratch | a bump arena (`bumpalo`) reset at the end of every barrier; persistent structures keep their capacity | O(1) per allocation, no free | no allocator traffic on the barrier path |
| Executor book | positions, marks, average cost, working orders, open targets as `Vec`s over slots; receivables as a `BinaryHeap` by pay date; corporate actions read from the bar's chunk of `split`/`dividend`/`delisted` (O(actions at `t`), which closes A6 of `docs/assessment.md`); ADV and realised volatility as `abt-agg` states per slot | O(positions + orders) per barrier | the present executor clones its bar list and scans dividend history each bar |
| Margin models (K-5.6) | `reg_t`: per underlying, the few legs are paired by exact enumeration (≤ 8 legs, ≤ 40 pairings); `portfolio`: the 11-point grid as a vectorised repricing over the underlying's positions; `span`: a dot product per contract over 16 scenarios | O(positions) per barrier | small by construction |
| Event log and book output | Arrow builders appended per event; flushed to `put` every N barriers by a writer thread | O(1) per event; I/O off the barrier path | |
| Checkpoint | the `Arc` chunk lists plus window states serialised on a background thread from a point-in-time clone (pointer copies) | O(state) off the barrier path | copy-on-write per bar makes the snapshot free |
| Query service | a read view = the `Arc` lists cloned at the last barrier, served on its own thread | no lock on the evaluator | V-8.8 |

### 3.2 Set-at-a-time evaluation (M-8)

A rule compiled to a block plan:

```
for each stratum in order:
  for each rule of the stratum, in plan order (tests first, generators by estimated bindings):
    bindings := the block of the first generator at this bar (or the universe bitmap)
    for each literal in Π order:
      generator  → bindings := merge_join(bindings, block_of(literal, bar))
      test       → bindings := filter(bindings, vectorised predicate)
      assignment → bindings := bindings ⊕ column(vectorised expression)
      aggregation over rows/window → column from the (rule, literal) window states, advanced for the slots present
      aggregation within (T) → one scalar, sequential over sorted slots
      negation   → bindings := andnot(bindings, bitmap_of(relation, bar))
    emit the head's block (sorted by slot by construction); record Δ as CSR
```

Entity-local strata (every rule's literals bind at most one entity and use no `within`) run in parallel over fixed slot ranges with `rayon`'s fixed-size chunking, each range writing its own slice of the output block; strata with a cross-sectional reduction run sequentially. The evaluator is kept equal to the demand-driven evaluator of today by translation validation on every corpus program and both books (M-8's gate).

**Proposition 3.1 [N] (bit identity under entity parallelism).** The blocks produced do not depend on the number of threads. *Proof.* Entity-local rules compute each row from that entity's inputs alone, in a fixed literal order; the output positions are fixed by slot order; reductions over the cross-section are sequential; `abt-agg` states are advanced by one thread per entity in bar order. ∎

### 3.3 Stream projection

The analyser computes the set of relations the program reads *outside* pushdown aggregates. The `StreamSpec` carries only those relations and only the columns read; pushed-down aggregates arrive as relations of their own. For MW14 the kernel therefore streams no daily bar tuples at all: the weekly views, the catalog relations and the series are its whole input (≈ 2.4M rows instead of 9.1M). For R8L the decision calendar is one label per session (`session + 30min`) plus the session closes, every session feature is a pushdown (`true_range@session`, `open0`, `close_before[M]`), and the kernel streams 178k decision barriers and the pushdown rows rather than 47M minute tuples; the executor's fills at specific minute prints are `point` queries (two per leg, 44k in all). This is the single largest effect of the split on the two books.

### 3.4 Receipt filtering is the stream order

Because the stream delivers tuples in receipt order and emits a barrier after every tuple with `r ≤ at(ℓ)` (D-5.2), Σ_t at barrier `ℓ` contains exactly the admissible tuples by construction; the evaluator applies no filter at all. The explicit `r ≤ at(ℓ)` is sent only on the rare `latest` query beyond the horizon and on live aggregate extensions.

---

## 4. The query layer

| Component | Algorithm | Bound | Note |
| --- | --- | --- | --- |
| Return metrics | one pass over the NAV curve: Welford moments, running peak for drawdown, autocorrelations to lag 10 directly | O(B) | `B` ≤ 10⁴ for daily, ≤ 10⁶ for minute: direct is enough, no FFT |
| Newey–West | Bartlett weights over `L` lags | O(B·L) | |
| Block bootstrap | stationary bootstrap, 2,000 reps; reps parallel with per-rep seeds derived as `hash(seed, rep)` so the result is independent of `P` | O(reps·B) | |
| PBO (CSCV) | C(16, 8) = 12,870 splits × grid points; the in-sample ranks per split by `sort_unstable` | O(splits · G log G) | |
| Regression engine (`ic`, `half_life`, `fama_macbeth`, `adf`, `kpss`, `engle_granger`, `regression`) | Householder QR with column pivoting in `f64`; HAC covariance from the residuals | O(n·p²) | tolerances per NIST difficulty class: lower 1e-12 relative, average 1e-9, higher 1e-6 (Filip is the higher class and is accepted at that tolerance, as the certified digits are beyond `f64` normal equations) |
| Johansen | the generalised eigenproblem reduced by Cholesky to a symmetric one; Jacobi eigen-decomposition (`p` ≤ 10) | O(p³) | |
| Hill, quantiles | `select_nth_unstable` | O(n) | |
| Spearman IC per bar | rank by `sort_unstable` per bar | O(B · E log E) | parallel over bars |
| Co-activation | 2×2 counts from the kernel's C and F tables; permutation within bars for the exact test | O(instances) | |
| Effective rules | Jacobi over the `R × R` activation correlation matrix | O(R³), `R` ≤ 100 | |
| Plan executor | topological order over the DAG; a pool of kernel endpoints; steps dispatched as endpoints free up; every table read through the data layer as Arrow batches (zero-copy in-process) | | a grid of `G` points on `K` kernels runs in ⌈G/K⌉ waves |
| Tables | immutable Arrow batches; group-bys and joins the metrics need are sorted-merge over `run_id`/`t`, already sorted by the kernel's writes | O(n) | the query layer never builds a hash join |

Every statistic is a pure function of immutable tables, so a plan's steps that read different runs run on different threads with no coordination, and a report over 10,000 trials is a scan.

---

## 5. Reaching the targets

### 5.1 Where the time goes after the split

| Book | Today | After | How |
| --- | --- | --- | --- |
| MW14 (1991–2026, 4,240 names/week) | 126 s, 3.1 GB: ~400M window-bar solves of rolling features, per-call environments, two copies of the data | stream 2.4M rows ≈ 1 s; weekly views and `dvol` from the cache (first run 2 s, then 0); `clenow`'s two regressions as `abt-agg` `ols_slope`/`ols_r` states over `rows[18]` of a derived relation: 7.9M O(1) state advances ≈ 1 s; `ma`, `eligible`, `rank`, `latch`, `target` as block operations ≈ 2 s; executor ≈ 0.5 s; writes off-path | ≈ 6–10 s, < 1 GB resident (the horizon holds 52 weekly bars of 4,240 names plus the series) against the 60 s target |
| R8L (2000–2026, 26 roots, 47M minute bars) | 3.0 s, 0.6 GB on a bundle reduced to the prints the book reads; the full-minute bundle does not fit | the full bundle resident in the data layer (47M × 5 columns × 16 B = 3.8 GB decoded, 1 GB on disk); session features as one aggregate pass per version ≈ 1 s on 8 threads, cached; the kernel sees 178k barriers and 44k point queries ≈ 1 s | ≈ 1–2 s against the 3 s target, and the full bundle fits (A-7.3) |

### 5.2 Live latency (A-7.3: < 50 ms barrier to decision)

Barrier emission ≤ 1 ms after the clock (the write-ahead path is sequential); the batch holding the barrier is already in the kernel's channel; `fill` and `open_bar` are O(positions); `decide_at` on a MW14-class program over 10,000 names is block operations over resident columns (≈ 5–15 ms); limits and invariants O(orders + positions); the decision event is appended to a buffer. The measured phases go into the run report every barrier.

### 5.3 Memory (A-7.3)

Data layer: ≤ 16 B per resident value by decoded-Arrow accounting (`f64` 8 B, `t` and `entity` shared across the row's columns, dictionaries for labels), ≤ 4 B on disk with `zstd` and byte-stream-split. Kernel: the horizon bound of K-4.6, refused at start if over budget. Query layer: one run's tables at a time per thread.

### 5.4 Ingest (A-7.3: 200,000 rows/s)

Group commit amortises the syscalls; the registry's attribution is two hash lookups and one binary search per row; the tail's indices are updated per batch, not per row; compaction runs on its own thread. The historical path (vendor files) bypasses the live ring and writes batches straight to the log.

### 5.5 The measurement loop

Every claim above is a number the system records: D-9.1's budgets as tests, per-phase barrier timings in `kernel.<run>.report`, estimated versus observed cost per literal in `kernel.<run>.plans`, cache hit rates in `abt data stats`, and `scripts/bench.sh` on the reference machine at every migration step. A regression is a row that changed, not a profile session to be scheduled.

---

## 6. What is established, new and decided in this document

- [E] Group commit and write-ahead logging; CSR indices; zone maps; pdqsort; two-stacks sliding windows (Daniel Lemire's and the DABA family); Welford and Chan–Golub–LeVeque; Fenwick trees; order-statistics trees; sorted-merge joins; Householder QR with column pivoting; Jacobi eigen-decomposition; the stationary bootstrap.
- [N] The pass is deterministic under parallelism (2.1); bit identity under entity parallelism (3.1); stream projection as a consequence of the analyser's pushdown set (3.3); receipt filtering as the stream order (3.4).
- [D] Time-major partitions with a CSR entity index rather than a second entity-major copy; dense slots over hash maps everywhere in the kernel; one block per rule application as the memo rather than a hash per call; sequential cross-sectional reductions rather than tree reductions, for bit identity; direct autocorrelations rather than FFT; QR rather than normal equations.
