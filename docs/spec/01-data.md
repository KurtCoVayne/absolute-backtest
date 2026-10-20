# 01 · The data layer (`abt-data`)

Oct 12, 2026 · build sheet for the first of the three programs. Tags and cross-references as in `00-architecture.md`.

---

## 1. Scope

The data layer is a store, a query engine, a stream and a resolver. It holds typed relations as facts with two timestamps, answers structured queries and aggregate requests in bounded time, emits event streams in a canonical order with explicit clock barriers, records live feeds so that they replay, and plans the cheapest way to satisfy a demand from the local store, the vendors and the feeds. It also stores, as relations, everything the other two layers produce.

It has no semantics (A-D6). It does not know what a close is, what a universe is, whether a tuple was knowable at a bar, or whether a bundle is point in time. Every such question is a filter or a precondition in another layer. The three things it guarantees about data are *schema conformance* (every row has the declared columns and types and satisfies the declared column constraints), *identity* (an instrument id names one instrument forever) and *order* (a stream replays in the same order with the same sequence numbers).

---

## 2. The data model

### 2.1 Scalar types

The wire types of `abt-wire` (P-1.1), as the data layer stores them:

| Type | Representation | Arrow type |
| --- | --- | --- |
| `Entity` | instrument id, `u64`, assigned by the identity registry (2.4), never reused | `UInt64` |
| `Time` | nanoseconds since the Unix epoch, UTC, `i64` (A-D7) | `Timestamp(Nanosecond, "UTC")` |
| `Num` | `f64`; a dimension is schema metadata, not a runtime tag | `Float64` |
| `Tick` | an exact decimal: `i64` units of `10^(−s)`, `s ≤ 9` per batch (added by `07-storage.md`, ST-10.4) | `Decimal128(18, s)` |
| `Count` | `i64` | `Int64` |
| `Label` | `u32` into a dictionary owned by the registry (2.5) | `Dictionary(UInt32, Utf8)` |
| `Bool` | | `Boolean` |
| `Bytes` | opaque, for other layers' values (a decision term, a checkpoint) | `Binary` |

[D] `f64` for prices and quantities, with vendor fixed-point (Databento's `1e-9` `i64`) converted at ingest. The kernel computes in `f64`; storing fixed-point would add a conversion on every read and a second numeric type to the dimension algebra.

**Amended by `07-storage.md` (A-D35).** For Norgate and Databento, prices and quantities are *stored* as tick columns: exact integers with a per-block quantum found from the data (ST-3), every vendor value reproduced bit for bit. `Num` stays the type on the wire and in the kernel; it is a view computed on read (one correctly rounded division, or a verified multiply). The measurement that reversed this decision: Databento prices are on each contract's tick but are not `f32`, `f64` sums are not associative, and packed ticks decode faster than `f64` scans (ST-2).

### 2.2 Relations and schemas

**Definition 2.1 (schema).** A relation schema is (name, class, columns, calendar, constraints, attributes) where a column is (name, type, role), roles are `entity`, `event_time`, `receipt_time`, `value`, `label`; exactly one column has role `event_time` (`t`) and at most one `receipt_time` (`r`); the *identity* of the relation is its `entity` columns and `t`; `calendar` names the base calendar the relation is keyed on (a label the kernel interprets; the data layer uses it only to choose partition granularity, 3.1); `constraints` are declared per column from the closed set {`not_null`, `> c`, `>= c`, `< c`, `<= c`, `in {labels}`}; `attributes` are free (key, value) pairs set by an adapter or a reviewer and read by other layers' preconditions (for example `reviewed = complete`, `receipt = recorded`).

The **schema registry** holds every schema by (class, name). A class catalog file (`corpus-v2/catalog/*.dsl`) is a rendering of the registry for the kernel; the registry is the source and `abt data catalog --class equity` prints that file (V-3.2).

**Definition 2.2 (revision).** Two rows of one relation with equal identity and `t` and different `r` are revisions of one fact. The store keeps every revision. A query returns, unless it asks for all revisions, the revision with the greatest `r` among those admitted by its receipt filter (4.2). A sealed version (2.6) may carry the attribute `revisions = none` after its adapter collapsed them.

[D] Keeping revisions, rather than rejecting a second row for one identity as the present loader does: a correction delivered after the fact is a market fact with a receipt time, and the historical-revision bias (B-4) is only measurable if the store keeps what was revised. The present duplicate rejection becomes a kernel precondition on versions that declare `revisions = none`.

### 2.3 Classes

A class is a label grouping relations that share an entity sort and a unit: `equity`, `future`, `option`, `series`, `pipeline` (the provenance the pipeline's gateway writes, E-94), and the namespaces of the other layers (`kernel`, `query`). The registry carries, per class, the entity sort name (`Equity`, `Future`, `Option`, `Name`) and the unit (`shares`, `contracts`, `contracts`, none). The relations of the market classes:

| Class | Relations at the base calendar | Reference relations | Supplied by |
| --- | --- | --- | --- |
| `equity` | `open`, `high`, `low`, `close`, `volume` (`entity`, `t`, `r`, value) | `listed`, `ticker`, `split`, `dividend`, `delisted`, `member`, `classification`, `margin` | Norgate adapter |
| `future` | `open`, `high`, `low`, `close`, `volume`, `open_interest`; `settlement(entity, t, r, price)` | `future_spec`, `continuous`, `session`, `member`, `margin` | Databento adapter |
| `option` | `quote(entity, t, r, bid, ask, bid_size, ask_size, last, volume, open_interest)`; `greeks(entity, t, r, iv, delta, gamma, theta, vega, rho)` | `option_spec`, `chain_index`, `session`, `margin` | ORATS adapter |
| `series` | `value(name: Label, t, r, v)` | — | Norgate (index series), ORATS (underlying quotes) |
| `pipeline` | `document(doc_id: Label, source: Label, author: Label, p, r, size: Count)`; `hypothesis(id: Label, doc_ids: Label (a JSON list), H, agent: Label, r)`; `submission(id: Label, hypothesis_id: Label, program_hash: Label, plan_hash: Label, lineage: Label, H, r)`; `outcome(submission_id: Label, t, reveal: Count, r, forward_from, forward_to, status: Label)`; `audit(job: Label, t, kind: Label, subject: Label, bytes: Count, duration_ns: Count, outcome: Label, r)` | — | the pipeline's gateway, through `put` (PL-7.2; E-94) |

Columns beyond the identity are the adapter's declaration; a strategy reads them through the schema the kernel is given.

### 2.4 Identity registry

**Definition 2.3 (instrument).** An instrument is an id with a class and a listing span. The registry holds:

| Relation | Columns | Meaning |
| --- | --- | --- |
| `instrument` | `id`, `class`, `from`, `to`, `exchange`, `currency` | the instrument's existence |
| `vendor_id` | `vendor: Label`, `key: Label`, `id`, `from`, `to` | a vendor's key for the instrument over a span (Norgate `asset_id`, Databento `instrument_id` and `raw_symbol`, ORATS ticker and OCC symbol, FIX `SecurityID` with `SecurityIDSource`) |
| `ticker` | `id`, `t`, `r`, `symbol: Label` | the symbol carried at `t` (equities: a history when the vendor supplies one, the final ticker over the whole span otherwise, with `attributes.ticker_history = final`) |
| `future_spec` | `id`, `root: Label`, `expiry: Time`, `first_trade`, `last_trade`, `point_value`, `tick`, `settlement: Label` | a contract |
| `continuous` | `root_id`, `t`, `r`, `contract_id`, `adjustment: Num` | which contract the back-adjusted series of `root_id` holds at `t` and the additive adjustment applied, computed on the root's decimal grid (its prices' and tick sizes') so the adjusted prices stay on it (A-D53) |
| `option_spec` | `id`, `underlying: Entity`, `right: Label`, `strike`, `expiry`, `style: Label`, `settlement: Label`, `multiplier`, `occ: Label` | a contract; `style ∈ {american, european}`, `settlement ∈ {physical, cash}`, `right ∈ {call, put}` |
| `chain_index` | `underlying`, `t`, `r`, `id` | the contracts of `underlying`'s chain known at `t` (what a chain query enumerates, 4.4) |
| `session` | `calendar: Label`, `label: Time`, `open: Time`, `close: Time`, `instrument: Entity?` | a session of a calendar (per instrument when the calendar is per instrument, V-3.5) |
| `calendar_label` | `calendar: Label`, `label: Time` | the labels of a declared calendar, what barriers are emitted on (5.2) |

**Assignment.** An id is assigned at first sight of a (vendor, key) pair the registry cannot resolve, by a rule per adapter (7.7): equities by Norgate `asset_id`, futures contracts by Databento `instrument_id` with the root from `definition`, options by OCC symbol. Two vendors' keys that denote one instrument are joined by the adapter's mapping rule (a Databento equity `raw_symbol` to a Norgate `asset_id` by symbol at date); a key no rule resolves rejects the row at ingest with the key and the row, never a silent new id (A-D24, the registry maps Norgate, Databento, ORATS and FIX only).

**Proposition 2.4 [N] (identity).** An id denotes one instrument for the life of the store; a row is attributed to an id only through a `vendor_id` interval containing its `t`. *Proof.* Ids are assigned once and never reused (an id is a counter the registry persists); attribution looks up (vendor, key, t) in `vendor_id`, which the identity check (8.1) keeps non-overlapping per (vendor, key). ∎

### 2.5 Labels

Labels are interned per registry in one dictionary with stable `u32` codes; a code is never reused and the dictionary is append-only, so a dictionary snapshot at any sequence number decodes every label written before it.

### 2.6 Versions

**Definition 2.5 (version).** A version is an immutable dataset of one relation: either a *sealed* set of partitions with a hash over their bytes, or a *log prefix* (stream id, seq ≤ N) of a recorded stream. Both are immutable: a sealed version's partitions are never rewritten, and a log's prefix is fixed once written. A version carries the schema it conforms to, the source it came from, the span [min t, max t], row count, column statistics and attributes.

**Definition 2.6 (current).** For each (relation, source) the registry names a current version; ingest of new data creates a new version (sealed data) or extends the log (a feed). A binding (7.6) names versions, never "current".

**Proposition 2.7 [N].** A binding read at two times returns the same rows in the same order. *Proof.* Versions are immutable by Definition 2.5; a query over a version is a deterministic function of the version's bytes and the query (4.6); the registry never rebinds a version id to other bytes. ∎

---

## 3. The storage engine

**Amended by `07-storage.md` (A-D36 to A-D39).** For Norgate and Databento the resident form is ST-4: instrument-contiguous, lane-interleaved blocks of tick values, with the precomputed structures of ST-5 (cumulative summaries, the calendar ladder, presence and rank, the catalog structures, wavelet matrices), all hot. Partitions of 3.1 and 3.2 are replaced for them by ST-7's content-addressed segments, compaction and delta partitions (3.3) by sealing tails and copy-on-write revisions (ST-8), and the per-row `seq` column by per-instrument visible counts (ST-8.2). The log, the manifest, versions, snapshot reads at a sequence number (Definition 3.2 and Proposition 3.3 hold, ST Prop. 8.1), retention and pinning (3.4), the cache (3.5) and tiers (3.7) stand. The time-major Parquet layout below remains the form for relations of other sources and for `put`s by other layers. *Amended (A-D49, iteration 1):* a version is hot when its attribute says `storage = hot`, which the adapters set for the bar relations they stage (`open`, `high`, `low`, `close`, `volume`); every other relation stays in Parquet, as does a version whose rows the blocks cannot hold (its attribute then says `storage = parquet`). A relation a source holds hot stays hot: a later write without the `storage` attribute follows the current version, and one asking for Parquet is refused (A-D51).

### 3.1 Layout

Per relation and source:

- **The log.** Every event the layer accepts for the relation, in acceptance order, as a sequence of Arrow IPC record batches on disk (`log/<relation>/<stream>.arrows`) with a sequence number per row and a footer index of (seq, byte offset) every 4,096 rows. The log is the source of truth and the canonical stream (5.1). It is append-only.
- **The hot tail.** The rows of the log not yet compacted, resident as Arrow batches, with two in-memory indices: `t → row ranges` (a sorted map) and `(entity, t) → row` (a hash map); size bounded by the retention rule (3.4).
- **Partitions.** Immutable Parquet files (`part/<relation>/<version>/<bucket>.parquet`) built by compaction from sealed prefixes of the log, rows sorted by (`t`, `entity`, `r`), one file per bucket of the partition calendar: month for base calendars at @1d or coarser, day for @1m, hour for @1s. Each file carries a footer with per-row-group zone maps (min/max of `t`, `entity`, `r` and every numeric column) and an **entity offset index**: for each entity in the file, the sorted row offsets of its rows.
- **The manifest.** Per version: the partition list with spans, row counts, the schema, the attributes, the hash (`manifest/<relation>/<version>.json`).

[D] Time-major primary order (`t`, `entity`). The two access patterns are the cross-section at a bar (contiguous) and the stream in order (sequential), both time-major; per-entity series are served by the entity offset index (a gather of `k` rows, 4.3) and rolling aggregates by a single time-major pass with per-entity state (6.4), so no entity-major copy is needed. The present kernel `Store` is this layout with a `Vec<Value>` per row.

### 3.2 Encoding

Parquet with `zstd` level 3; `entity` dictionary-encoded then delta; `t` and `r` delta-encoded; `f64` byte-stream-split; labels dictionary. Target ≤ 4 B per value on disk and ≤ 16 B resident (A-7.3). Resident partitions are memory-mapped and decoded by row group on first touch; the tail is plain Arrow.

### 3.3 Sealing, compaction and reads at a sequence number

**Definition 3.1 (sealed prefix).** The log's sealed prefix is the greatest seq `S` such that every row with seq ≤ `S` has been compacted into partitions. Rows with seq > `S` are the hot tail.

**Compaction** runs when the tail exceeds its budget or a bucket of the partition calendar is older than the tail retention: it takes the tail rows whose `t` falls in buckets that are *closed* (every calendar barrier for the bucket has been emitted, 5.2, or the source is historical), sorts them into (`t`, `entity`, `r`), writes the bucket's partition and advances `S`. A late row for an already-compacted bucket (its `t` in a bucket whose partition exists) is written to a *delta partition* for that bucket (`<bucket>.<n>.parquet`), merged on read by `r`; delta partitions are folded into the base at the next full compaction of the version. A compaction never changes a sealed version: it produces the next version of the relation (2.6).

**Definition 3.2 (snapshot read).** A query with `as_of_seq = N` sees exactly the rows with seq ≤ `N`. Partitions hold only sealed rows (seq ≤ `S`); for `N ≥ S` the read is partitions plus the tail filtered by seq ≤ `N`; for `N < S` the read is partitions filtered by the per-row `seq` column, which partitions keep for this purpose.

**Proposition 3.3 [N] (snapshot reads are exact and bounded).** A snapshot read at `N` returns the rows with seq ≤ `N` and no other, in O(log n + k) for a point or entity range and O(k) for a cross-section, `k` the rows returned. *Proof.* Sealed rows are exactly those with seq ≤ `S` (Definition 3.1); the tail filter is exact; the `seq` column makes the partition filter exact. Bounds: zone maps and the entity offset index locate the row group in O(log n); the cross-section is a contiguous range at `t`. ∎

### 3.4 Retention, budgets, eviction, pinning

The tail retains at least two closed buckets per relation and at most `tail_budget` bytes (configuration, default 2 GiB). Resident partitions are evicted LRU under `resident_budget` (default: half of physical memory); on-disk versions are evicted LRU under `disk_budget` (default 200 GiB) except *pinned* versions. A binding pins its versions while any plan that references it exists (Q-2.6); `abt data pin` pins by hand; `abt data gc` evicts.

### 3.5 Content-addressed cache

**Definition 3.4 (cache object).** A cache object is an immutable dataset keyed by the BLAKE3 hash of the canonical form of the request that produced it (an aggregate request, a bar view, a kernel checkpoint, a ledger, a derived relation of a run). It is stored as a version of a relation in the `cache` namespace with the request as its attribute. A hit is a version lookup by key; a miss is computed by the requester (the data layer for aggregates, 6; the kernel for everything else) and `put` under the key.

The cache is what makes "remain hot" cheap: a bar view at `weeks(1)` of the equity close is one object per (relation version, calendar, numerics), computed once per version and shared by every program (V-3.6).

### 3.6 Writes by other layers

The kernel and the query layer write relations through `put` (P-3.1) under their namespaces (`kernel.<run id>.<relation>`, `query.<plan hash>.<relation>`). A `put` is an ingest with the schema the writer supplies; it goes through the log, the tail and compaction like any adapter's rows, and the data layer is the single writer of every file (A-D20). Concurrent `put`s from several kernels are serialised by the log; each receives its sequence range.

### 3.7 Tiers

A store tier is an adapter with `get(version) → partitions`, `put(version, partitions)`, `has(version)`. The local tier is the only one implemented; an S3 tier (the NDLake bucket) is the owner's open item (A-8) and plugs in as a second tier consulted before vendors in the resolver's cost model (7.3).

---

## 4. The query interface

Queries are typed requests (P-3.2), not text; `abt data query` accepts a text form for the command line and prints the typed request it built. Every query takes a *view*: a version or a binding, and an `as_of_seq`; and a *receipt filter* `r ≤ A` (default: no filter). Every query returns an Arrow table in the relation's schema (plus `seq` on request), or a stream of batches.

### 4.1 Forms

| Form | Arguments | Answer | Cost |
| --- | --- | --- | --- |
| `point` | relation, identity, `t` | the row (latest revision admitted) or none | O(log n) |
| `latest` | relation, identity, `t_max` | the row with the greatest `t ≤ t_max` admitted by the receipt filter (the as-of read, S-4, served without semantics) | O(log n) |
| `series` | relation, identity, [`t0`, `t1`] | the entity's rows in `t` order | O(log n + k) |
| `section` | relation, `t` | every entity's row at `t` | O(k) |
| `scan` | relation, [`t0`, `t1`], predicate | rows in (`t`, `entity`) order satisfying the predicate | O(log n + k) with zone-map pruning |
| `exists`, `count` | as `scan` | | as `scan`, pruned |
| `distinct` | relation, column, [`t0`, `t1`] | the distinct values (the entities present, the labels used) | O(k) |
| `chain` | underlying, `t`, filters (4.4) | the option ids of the chain at `t` admitted by the filters, with their specs | O(log n + k) |
| `aggregate` | section 6 | | section 6 |
| `catalog` | a predicate over the registry's relations (2.4, 7.1) | rows of the registry | O(k) |
| `subscribe` | stream, `from_seq`, filters | section 5 | |
| `explain` | any of the above | the estimated rows and bytes and the partitions touched | O(partitions) |

**Definition 4.1 (predicate).** A predicate is a conjunction of disjunctions of atoms, an atom being `column op constant` with `op ∈ {=, ≠, <, ≤, >, ≥}`, `column in {constants}`, or `column is null`. No joins, no arithmetic: a join is the kernel's, and an arithmetic condition is a rule.

### 4.2 The receipt filter

`r ≤ A` restricts every form to rows received by `A`, and `latest` and `point` to the latest revision among them. It is the whole of the data layer's contribution to point-in-time correctness: the kernel passes the current barrier's instant as `A` (K-4.3), the query layer passes an embargo's instants (Q-2.3), and the store evaluates a comparison. A relation without a receipt column has `r = t` by Definition 2.1's default, so the filter degenerates to `t ≤ A`, the v1 convention.

### 4.3 Series and the entity offset index

`series` reads the entity offset index of each partition in [`t0`, `t1`] and gathers the rows; the tail is read through its `(entity, t)` index. Gathering `k` rows from memory-mapped column chunks costs O(k) random reads; at 10,000 instruments and 252 rows the whole cross-section's series cost 2.5M reads, which is why rolling aggregates are not served by `series` but by the pass of 6.4.

### 4.4 Chains

A chain query takes an underlying, `t` and filters from the closed set: `expiry in [t + d0, t + d1]`, `moneyness in [m0, m1]` relative to a spot the caller supplies (the data layer does not know which price is spot), `right`, `style`, `strike in [k0, k1]`, `open_interest ≥ n` (from the latest `quote` row admitted). It answers from `chain_index` and `option_spec`; when `chain_index` has no row for (underlying, t) in the view, the answer is empty and `explain` says so, which is what the resolver's dynamic demand (7.6) reads to decide a fetch.

### 4.5 Text form

`abt data query 'close(A, T, P) at 2020-03-06 for A in (AAPL, MSFT) as_of seq 1203444 r <= 2020-03-06T21:00Z' --view <binding>` is the command-line rendering: a relation with named columns, `at`/`over`/`for`/`where` clauses, and the view. Symbols resolve through `ticker` at the view's last `t` unless `at date` is given. The command prints the typed request first, then the table.

### 4.6 Determinism of answers

**Proposition 4.2 [N].** Every query form is a deterministic function of (view, `as_of_seq`, receipt filter, arguments). *Proof.* The view is immutable (2.7); `as_of_seq` fixes the tail's contents (3.3); each form's result order is defined (4.1); predicates are pure. ∎ This is what A-D11 requires and what G3 tests across thread counts.

---

## 5. Streams

### 5.1 Events

```
Event = Tuple   { stream, seq, relation, entity, t, r, row }
      | Barrier { stream, seq, calendar, label, at }      # the close of bucket `label` of `calendar`, emitted at instant `at`
      | Mark    { stream, seq, kind, at }                  # session_open | session_close | feed_up | feed_down | gap(from, to)
```

A stream is a totally ordered sequence of events with `seq` monotone and gap-free from the subscriber's `from_seq`. A stream is defined by a *stream spec*: the relations it carries (by version or binding), the calendars it emits barriers on, the receipt filter, and the clock source.

### 5.2 The clock source and barriers

**Definition 5.1 (clock source).** In `replay` the clock source is the set of `calendar_label` rows of the calendars the spec names, within the view; a barrier for label `ℓ` of calendar `C` is emitted at instant `at = ℓ` after every tuple with `r ≤ ℓ` and before any tuple with `r > ℓ`. In `live` the clock source is the exchange calendar's labels for the session in progress and the wall clock: the barrier for `ℓ` is emitted when the wall clock passes `ℓ + grace(C)`, `grace` a per-calendar tolerance from configuration (default 0 for sessions, 250 ms for sub-daily bars), and every tuple received before that emission with `r ≤ ℓ` precedes it.

**Proposition 5.2 [N] (barriers partition the stream).** For every calendar `C` the stream names, the tuples with `r` in (`ℓ_{i−1}`, `ℓ_i`] lie between the barriers for `ℓ_{i−1}` and `ℓ_i`. *Proof.* Replay: by construction of the merge order (5.3). Live: a tuple received after the barrier for `ℓ_i` was emitted has `seq` greater than the barrier's; its `r` is its receipt instant, which is after `ℓ_i + grace ≥ ℓ_i`. ∎ Live, a tuple with event time in a closed bucket arriving after that bucket's barrier is *late* and is simply a tuple with `r` past the barrier; the kernel's late rule applies (K-4.4). The data layer never reorders it.

### 5.3 Canonical order

**Definition 5.3 (order).** Events are ordered by (`r` for tuples and `at` for barriers and marks, then kind with barriers after tuples of equal instant, then relation id, then identity, then revision order). `seq` numbers the events in this order. In replay the order is computed by a k-way merge of the relations' logs with the calendar labels; in live it is the acceptance order, which the layer makes consistent with the definition by stamping `r` at acceptance and never accepting a tuple with `r` below the last emitted barrier's `at` as belonging before it.

**Proposition 5.4 [N] (replayability).** Subscribing to a recorded stream from `from_seq` yields the same events with the same `seq` as any earlier subscription from the same `from_seq`. *Proof.* The log is append-only and the merge is deterministic over immutable inputs; a live stream is recorded in its acceptance order, which is its canonical order by Definition 5.3. ∎ This is Proposition A-4.4's data-side half.

### 5.4 Recording live streams

A live stream is written to its log before any subscriber sees an event (write-ahead); a subscriber that falls behind reads from the log, not from memory, so the layer never drops and never buffers unboundedly. The lag of each subscriber (events and seconds) is reported in a `Mark{kind: lag}` on the control channel every second and is what the kernel's falling-behind rule reads (K-6.3).

### 5.5 Replay of a live session

A `replay-sim` subscription to a stream recorded live replays the log from `from_seq` at full speed with the same barriers at the same positions; a `--paced` option replays at wall-clock pace for rehearsal. Both yield the same events (5.4).

### 5.6 Backpressure and subscriptions

A subscription is a bounded channel (configuration, default 65,536 events) fed from the log; the producer blocks on a full channel. A subscriber may `ack(seq)` to let the layer release tail memory; a subscription without acks for `tail_budget` bytes falls back to reading partitions.

---

## 6. Aggregates

### 6.1 The request

```
Aggregate { input: relation column (by view), function, window, group, receipt_filter, numerics, output: relation name }
window = rows(N, min K) | prior_rows(N, min K) | window(δ, min K) | prior_window(δ, min K) | bucket(calendar)
group  = per entity (default) | per label column | whole
function ∈ abt-agg's set (P-1.2): sum, count, mean, var, std, min, max, first, last, median, quantile(q), cov, corr, ols_slope, ols_r, ols_beta
```

The answer is a relation keyed like the input (entity, `t`) with the aggregate as its value, or keyed by the bucket label for `bucket`. It is a cache object (3.5) keyed by the request's canonical form, which includes the input version, so a new version of the input is a new object.

### 6.2 Windows over stored relations

`rows(N)` counts the entity's own rows in the relation; `window(δ)` is a calendar duration over `t` (S-4); `prior_*` exclude `t`; `bucket(C)` groups the rows of the input whose `t` falls in a bucket of calendar `C` and keys the answer by the bucket label (the `resample` of S-4; the bar views of V-3.3). `min K` yields no row when fewer than `K` inputs are present; `rows(N)` without `min` requires all `N` (V-7.2).

### 6.3 Numerics

The request's `numerics ∈ {exact, two_pass, running}` selects the `abt-agg` form (P-1.2); the cache key includes it. Results are bit-identical to the kernel's evaluation of the same aggregate over the same rows (6.5). `exact` (added by `07-storage.md`, ST-3, Proposition 3.3) applies to tick columns: sums and moments are exact integers in the instrument's finest exponent, accumulated in `i128`, so they are independent of order and vectorise; the `f64` result is one correctly rounded division. It is the default for new programs; the parity books (MW14, R8L) pin `two_pass`. In `exact` mode tick columns cross the protocol as `Tick` (`Decimal128`), so both layers reduce the same integers. Aggregates over adjusted series (split, total return, continuous futures) are evaluated piecewise between events (ST-5.10). A column with no integer form (a float column, a derived series) is reduced in `exact` as in `two_pass`, and a function that is not a rational of the sums (`std`, `corr`, `ols_r`) takes its exact centred sums to `f64`, each correctly rounded, and finishes in `f64` (A-D47).

### 6.4 The algorithm

A rolling or bucket aggregate over a relation is computed in one time-major pass over the view's partitions and tail (3.1): rows arrive in (`t`, `entity`) order; per entity the pass keeps the window state of `abt-agg` (a two-stacks deque for distributive functions, the Welford or shifted running sums for moments, an order-statistics structure for median and quantile), emits the aggregate at each `t` where the window condition holds, and evicts rows leaving the window. The pass is O(rows · c(w)) with `c(w)` the per-row cost of the function's class (F-8), parallel over disjoint entity ranges when `group = per entity` (A-D11 is preserved: each entity's sequence is computed in `t` order by one thread, and the output order is fixed by (`t`, `entity`)).

### 6.5 Equality with the kernel

**Proposition 6.1 [N] (pushdown equality).** For a rule `R(A, T, X) :- universe(A, T), X = agg(V) over (T1 in W, S(A, T1, V))` with `S` a stored relation and `W` a window form of 6.1, the kernel's evaluation of `X` at every (A, T) under receipt filter `A ≤ barrier(T)` equals the data layer's `Aggregate` answer at (A, T) under the same filter and numerics, bit for bit. *Proof.* Both reduce the same multiset of rows (the rows of `S` for `A` in `W`, admitted by the filter) in the same order (`t` ascending, which for one entity is the canonical order of A-4.7) with the same `abt-agg` function in the same mode. ∎ Where the kernel's rule reads the window's base time or has several windows, no pushdown is made (K-4.5).

---

## 7. The resolver

### 7.1 The source catalog

| Relation | Columns | Meaning |
| --- | --- | --- |
| `source` | `name`, `kind ∈ {store, tier, vendor, feed}`, `adapter`, `freshness ∈ {historical, delayed(d), live}` | a source |
| `offers` | `source`, `class`, `relation`, `calendar`, `from`, `to`, `scope ∈ {all, list(L), chain, by_request}`, `has_receipt: Bool`, `supplies: {iv, greeks, margin, oi}`, `attributes` | what the source can provide |
| `price` | `source`, `unit ∈ {request, gb, symbol_day, month}`, `money: Num`, `currency: Label` | the cost model (A-8: placeholders until supplied) |
| `quota` | `source`, `window: Duration`, `requests: Count`, `bytes: Count` | rate limits |
| `cached` | `source`, `relation`, `version`, `from`, `to`, `scope` | what the store already holds from the source |

The store is itself a source of kind `store` with every price zero. A tier (3.7) has a byte price and no money.

### 7.2 Demands

**Definition 7.1 (demand).** A demand is the typed form of a program's `data` block (V-3.11) as the kernel sends it (P-3.3):

```
Demand { class, needs: [Need], require: [Attribute], prefer: [Preference], budget: Budget, dynamic: [DynamicNeed] }
Need        { relations: [name], calendar: ≼ C, span: [from, to], scope: all | list(L) | instruments([(id | symbol, from)]), attributes }
DynamicNeed { relation: quote | greeks, underlying: Entity, filters: chain filters (4.4), spot: (relation, column), horizon: Duration }
Budget      { money: Num = 0, bytes: Count = ∞, requests: Count = ∞, seconds: Num = ∞ }
Preference  = tested | latest | version(v) | finest | fewest_sources | cheapest | source(s)
```

A `Need` is satisfied by a source offering the relations on a calendar at least as fine as `C` (the coarser one being a derived view, 6.2) over a span covering [`from`, `to`], with the attributes (`reviewed = complete`, `receipt = recorded`); the kernel decides which attributes it requires (K-3.2), the resolver only matches them.

### 7.3 Cost

**Definition 7.2 (cost vector).** `cost = (money, seconds, bytes, requests)`. The cost of satisfying a need from a source is the sum over the source's priced units of the units needed times the unit price, with seconds estimated from the source's measured throughput (the registry keeps a moving average per source) and bytes from the offer's row estimate. A need satisfied from `cached` rows costs zero money and the read's seconds. A need satisfied by *derivation* (a coarser calendar computed from a finer cached one by 6.2) costs the pass's seconds.

**Definition 7.3 (order).** Plans are compared lexicographically in the order the demand's preferences give, default (money, seconds, bytes, requests).

### 7.4 Planning

**Proposition 7.4 [E] (cover).** Choosing, for each need, the sources that cover its span at minimum cost is a weighted interval cover per need, solvable by dynamic programming over the sorted interval endpoints in O(m log m) for `m` offers; needs are independent except through the budget, so the plan is the per-need optimum when its total is within budget, and otherwise the resolver reports the optimum and the budget it exceeds. Reference: weighted interval covering (Kleinberg and Tardos, Algorithm Design, ch. 6). [D] No global optimisation across needs under the budget beyond the sum: the budget is a cap, not an objective, and the per-need optimum under a cap is what a reader can check by hand.

### 7.5 Fetch, ingest, version, bind

A plan executes as: for each (need, source) with money > 0, confirm the budget once more against the quota; fetch through the adapter (7.7); ingest into the log under the source's stream; seal a version; record `cached`. The binding is then the list of (relation, version) pairs plus the parameter point and the dynamic needs' filters, hashed to the binding id (A-6). Versions in a binding are pinned (3.4).

**Proposition 7.5 [N] (resolution is reproducible).** Resolving a demand twice against the same catalog state yields the same binding iff the demand pins versions (`prefer version(v)` for every need) or no new version was ingested between the two resolutions; otherwise the bindings differ in exactly the relations whose current version changed, and both are reproducible runs. *Proof.* Plans are deterministic functions of the catalog and the demand (7.4); a binding names versions (7.5); a changed current version changes the plan's cheapest cover only for that relation. ∎ This is V-3.13 restated with the resolver in the data layer.

### 7.6 Dynamic demands and the pre-pass

A dynamic need asks for chains the program will query per barrier (A-D23). In `replay` the resolver runs a **pre-pass**: it evaluates the chain filters at every label of the decision calendar over the span, using the spot the kernel names (the kernel sends the spot relation and column with the dynamic need, so the resolver reads a stored price and interprets nothing), collects the set of (underlying, `t`) chain queries and the contract ids they admit, and resolves that set as a static need of `quote` and `greeks` for those ids over their spans. A chain query at run time that the pre-pass covered is a cache hit; one it did not cover (a filter that depends on a derived relation the resolver cannot evaluate) is served **on demand**: the kernel's `chain` query returns empty with `explain.unresolved = true`, the kernel issues a `resolve_dynamic` (P-3.3) for that (underlying, `t`), the resolver fetches under the remaining budget and the query is retried once. In `live` the dynamic need is turned into a subscription on the chain within the filters, re-evaluated at each session open.

### 7.7 Adapters

Each adapter maps a vendor's data to relations of 2.3 and ids of 2.4, and declares its offers and prices. An adapter is a trait (`fetch(need) → batches`, `subscribe(need) → stream`, `offers()`), implemented in the `abt-data` crate.

| Adapter | Source | Relations | Identity rule | Notes |
| --- | --- | --- | --- | --- |
| `norgate` | NDLake (`curated/bars_by_date`, `registry/security_master`, `registry/corporate_actions`, `indexes/index_membership`, `classification/classification_full`, `registry/trading_calendar`) | equity `open..volume`, `listed`, `ticker`, `split`, `dividend`, `delisted`, `member`, `classification`, `calendar_label`, series `value` for indexes | `asset_id` → id | prices as traded (`docs/handoff-data.md`); `dividend` with `announce = pay = ex` and attribute `dividend_dates = ex_only`; `delisted` reason inferred with attribute `delisting_reason = inferred`; ticker history attribute `final`; the reviewed-exceptions file becomes attributes on the version |
| `databento` | historical API (`ohlcv-1m`, `ohlcv-1s`, `trades` reserved, `definition`, `statistics` for settlement and OI) and the live gateway | future `open..open_interest`, `settlement`, `future_spec`, `continuous`, `session`, `calendar_label` | `instrument_id` → id; root from `definition` | `r = ts_recv` when present, else `ts_event + processing_delay` from configuration (B-10); live: DBN frames, `r` stamped at acceptance |
| `orats` | ORATS API: end-of-day strikes and intraday snapshots per underlying | option `quote`, `greeks`, `option_spec`, `chain_index`, series `value` for the underlying's quote | OCC symbol → id; underlying by ticker at date through the equity registry | `r` = the snapshot time; a chain fetch is one request per (underlying, day); the pre-pass (7.6) batches by day |
| `fix` | reserved | market data incremental refresh | `SecurityID`/`SecurityIDSource` | not implemented in this specification (A-D27) |

An adapter rejects a row that fails the schema (2.1) or the identity rule, naming the row, and never writes a partial batch.

---

## 8. Data-side checks

The data layer runs three checks and no others (A-D6):

1. **Schema conformance** at ingest and at `put`: columns, types, declared constraints (`close > 0` is a declared constraint of the equity schema; a future's prices carry no such constraint since back-adjusted series cross zero).
2. **Identity** at ingest: `vendor_id` intervals per (vendor, key) do not overlap; an id's class never changes; `ticker` intervals per symbol do not overlap when the vendor supplies a history (8.1 of the present bundle tests, kept because it is a property of the registry, not of the market).
3. **Order** at read: a stream's `seq` is gap-free and monotone (a corrupted log is refused with the first gap).

Everything that was a bundle test about market facts (action reconciliation, delisting coverage, membership, bar labels, resample reconciliation) becomes a kernel precondition (K-3.2) or a query-layer feed statistic (Q-7), reading the same relations through the query interface.

---

## 9. Performance

### 9.1 Budgets

| Operation | Bound | Mechanism |
| --- | --- | --- |
| `point`, `latest` | ≤ 5 µs resident, ≤ 50 µs from disk | zone maps, entity offset index, mmap |
| `section` at 10,000 entities | < 1 ms from the tail, < 5 ms from a resident partition | contiguous range |
| `series` of 252 rows | < 100 µs resident | offset gather |
| Ingest | ≥ 200,000 rows/s sustained | append-only log, batched index updates |
| Barrier latency live | < 1 ms from the clock to the subscriber's channel | the write-ahead log is sequential; no sort on the live path |
| Rolling aggregate, 252-row mean, 10,000 entities, 30 years @1d | < 10 s single-threaded, < 2 s on 8 threads | 6.4 |
| Resident footprint | ≤ 16 B per value | 3.2 |

### 9.2 Tests

`tests/data_store.rs`: snapshot exactness (3.3) with randomised interleavings of ingest and reads; revision semantics (2.2); late rows into compacted buckets; eviction never touches a pinned version. `tests/data_stream.rs`: Propositions 5.2 and 5.4 on recorded synthetic feeds with injected late tuples and gaps. `tests/data_agg.rs`: Proposition 6.1 against `abt-agg` applied directly to the gathered rows, for every function and window form, both numerics. `tests/data_resolver.rs`: plans against a synthetic catalog with priced sources; budget refusal names the cheapest plan; pre-pass covers every chain query of a synthetic options program. `scripts/bench.sh`: the budgets of 9.1 on the reference machine.

---

## 10. Commands

```
abt data serve   [--socket PATH] [--config FILE]
abt data ingest  --adapter norgate|databento|orats --from LOCATOR [--need ...] [--budget money=X,bytes=Y]
abt data catalog [--class C] [--relation R] [--sources]           # the registry as the class file, or the source catalog
abt data resolve --demand FILE|- [--explain] [--dry-run]            # the plan and the binding
abt data query   'TEXT' --view BINDING|VERSION [--as-of-seq N] [--json|--parquet FILE]
abt data agg     --request FILE --view ...                           # an aggregate, cached
abt data stream  --spec FILE [--from-seq N] [--paced] [--out FILE]   # an event stream to stdout or a file
abt data put     --relation NAME --schema FILE --from FILE           # writes by other layers, also over the socket
abt data pin|unpin VERSION|BINDING
abt data gc      [--disk-budget BYTES]
abt data stats   [--relation R]                                      # sizes, tail, sealed seq, cache hit rate
```

Every command takes `--json`.

---

## 11. What is established, new and decided in this document

- [E] Weighted interval cover by dynamic programming (7.4); write-ahead logging and snapshot isolation by sequence number (Bernstein, Hadzilacos and Goodman 1987, ch. 5); the aggregate classes and their online cost (F-8; Gray et al. 1997); byte-stream-split and dictionary encodings (the Parquet specification).
- [N] Identity (2.4); immutability of bindings (2.7); exact bounded snapshot reads (3.3); barriers partition the stream (5.2); replayability (5.4); determinism of answers (4.2); pushdown equality (6.1); reproducible resolution (7.5).
- [D] Two timestamps and revisions kept (2.2); time-major layout with an entity offset index (3.1); the log as the source of truth and partitions as an index (3.1); the clock as a stream of barriers with a per-calendar grace (5.2); the resolver with zero-money budgets and per-need optimal covers (7.3, 7.4); the pre-pass for dynamic demands (7.6); three checks and no semantics (8).
