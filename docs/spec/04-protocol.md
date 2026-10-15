# 04 · The protocol and the shared crates (`abt-wire`, `abt-agg`, `abt-codec`)

Oct 12, 2026 · the only boundary between the three programs (A-D3). Tags and cross-references as in `00-architecture.md`.

---

## 1. The shared crates

### 1.1 `abt-wire`

Types and messages, with serialisation, canonical bytes and hashing, and nothing else: no evaluation, no storage, no statistics. A function enters `abt-wire` only if it is (de)serialisation, validation of a frame, or the BLAKE3 of a canonical form (A-D2).

The source of truth for every wire type is the FlatBuffers schema `abt-wire/schema/abt.fbs` (section 2.2), compiled at build time by `planus` (a pure-Rust FlatBuffers compiler) into zero-copy readers (`DemandRef`, `BindingRef`, …) and owned types (`Demand`, `Binding`, …). Services read requests through the readers without allocating; the owned types derive `Serialize`/`Deserialize` for logs, `--json` and the canonical form (section 6), and convert to and from the readers by generated code. [D] A schema file rather than Rust types as the source of truth: a zero-copy format needs a layout the compiler owns; the owned types keep the ergonomics the rest of the crates expect, at the cost of one generated conversion per message, which `planus` writes.

| Group | Types |
| --- | --- |
| Scalars | `Entity(u64)`, `Time(i64 ns)`, `Label(u32)`, `Num(f64)`, `Count(i64)`, `Bool`, `Bytes` (D-2.1) |
| Schema | `ColumnSpec { name, ty, role }`, `RelationSchema { name, class, columns, calendar, constraints, attributes }` (D-2.2) |
| Calendars | `CalendarSpec` (V-3.1 constructors), `CalendarId(Label)` |
| Dimensions | `Dim { currency: i8, unit: i8, time: i8 }` with the currency and unit labels (S-2) |
| Identity | `Hash([u8; 32])`, `VersionId(Hash)`, `BindingId(Hash)`, `RunId(Hash)`, `PlanHash(Hash)`, `TrialId(Hash)`, `Seq(u64)` (A-6) |
| Data | `Demand`, `Need`, `DynamicNeed`, `Budget`, `Preference`, `Binding { pairs: [(relation, VersionId)], point: [(name, Literal)], dynamic: [DynamicNeed], id }`, `CostVector`, `Plan` (D-7) |
| Queries | `QueryForm` (D-4.1), `Predicate` (D-4.1), `ReceiptFilter { r_max: Option<Time> }`, `View { versions | binding, as_of_seq }`, `AggregateRequest` (D-6.1), `ChainFilters` (D-4.4) |
| Streams | `StreamSpec`, `Event` (D-5.1), `Mark`, `Subscription` |
| Kernel | `Literal` (the DSL's literals, S-2), `RunRequest`, `RunMode`, `Deployment`, `Decision` (K-2.3), `KernelEvent` (K-5.2), `ContractClause`, `Status` |
| Query layer | `PlanStep`, `TestResult`, `Claim`, `Verdict`, `Trial` |
| Protocol | `Frame`, `Hello`, `Request`, `Response`, `Error` (section 2) |

Every type derives `Serialize`/`Deserialize` and implements `Canonical` (section 6).

### 1.2 `abt-agg`

The aggregate kernels, one implementation (A-D8). Pure functions and window-state structures over `f64` and `i64`:

| Class (F-8) | Functions | State | Per-row cost |
| --- | --- | --- | --- |
| distributive | `sum`, `count`, `min`, `max`, `first`, `last` | two-stacks deque (min/max); compensated running sum (Neumaier) for `sum`; counters | O(1) amortised |
| algebraic | `mean`, `var`, `std`, `cov`, `corr`, `ols_slope`, `ols_r`, `ols_beta` | `two_pass`: the window's rows, reduced in canonical order by the two-pass formulas (Chan, Golub and LeVeque); `running`: shifted compensated sliding sums rebuilt exactly every N rows (the present `RunningSums`) | O(w) / O(1) |
| holistic | `median`, `quantile(q)` | an order-statistics tree over the window | O(log w) |

API: `WindowState::new(function, mode)`, `push(t, x…)`, `evict(t)`, `value() -> Option<f64>` (`None` below the minimum count or at a partial point, which the caller treats as `⊥`), and the batch form `reduce(function, mode, columns) -> Result<Option<f64>, AggError>` over rows already in canonical order. The canonical reduction order is entity ascending by id, then `t` ascending (A-4.7); `abt-agg` never sorts, it requires the order and asserts it in debug builds. `AggError` names the partial point (one observation for a variance, a constant series for a correlation, a quantile level outside [0, 1]) as the present `aggregate` does.

**Proposition 1.1 [N] (one implementation, one answer).** For any function, mode, and sequence of rows in canonical order, the data layer's pass (D-6.4) and the kernel's evaluation (K-4) produce the same `f64`, bit for bit. *Proof.* Both call `abt-agg` on the same rows in the same order with the same mode; the functions are deterministic and use no reassociation outside the mode's stated rebuild points, which are at the same row counts in both callers (the rebuild is a function of the window's row index). ∎ `tests/agg.rs` holds every function and mode against a 128-bit rational reference on random windows within the mode's stated tolerance (`two_pass`: to the bit of the two-pass formula; `running`: 1e-12 relative, as `docs/assessment.md`), and the pushdown equality test of D-9.2 holds the two callers to each other.


### 1.3 `abt-codec`

Added by `07-storage.md` (A-D35, A-D36). The resident block format of Norgate and Databento and the structures precomputed over it, as pure functions over byte slices: no I/O, no registry, no semantics.

| Module | Contents |
| --- | --- |
| `tick` | the tick column (ST-3): per-block exponent and GCD quantum, the encoder's verified reconstruction (`recip_exact`), exact `i128` sums across blocks of different quanta |
| `block` | the lane-interleaved layout (ST-4.2), width-specialised `pack`/`unpack` for widths 0 to 64 (fully unrolled), row-range decoding for short edges, single-value extraction, patched exceptions (ST-4.4), the horizontal tail and its in-place append (ST-8.3) |
| `predict` | OHLC from the close (ST-4.5) |
| `summary` | block summaries, cumulative summaries and cross-moments, sparse tables with argmin/argmax, the range rules of ST-5.1 to ST-5.3 |
| `rank` | presence bitmaps with rank and select directories (ST-5.4) |
| `wavelet` | wavelet matrices over rank-reduced offsets: k-th, count and listing over a row range (ST-5.7) |

The data layer builds and queries with it; the kernel decodes mapped segments with it (`map`, 3.1); `abt-agg`'s reductions run on its decoded blocks, and `abt-agg` gains the `exact` mode for tick columns (D-6.3). Property tests: encode then decode is the identity at every width with and without exceptions; extraction and row-range decoding equal full decoding; every range operation equals a direct reduction; exact sums equal a 128-bit rational reference (ST-13). [D] One implementation of the format shared by its writer and its readers, for the reason `abt-agg` is one implementation of the aggregates: a reader's decoder and the writer's encoder cannot drift.

---

## 2. Transport

### 2.1 Endpoints

A service listens on a Unix domain socket (`--socket PATH`, default `$XDG_RUNTIME_DIR/abt/<layer>.sock`, mode 0600) or is reached in-process through a channel pair that carries the same frames (A-D3). TCP is not offered; the layers are in-house and co-located (A-D4's premise); a later remote deployment adds TLS over TCP without changing a frame.

### 2.2 Frames

```
Frame = length: u32 (little-endian, bytes that follow) | kind: u8 | correlation: u64 (little-endian) | payload
kind  = 0 hello | 1 request (FlatBuffer) | 2 response (FlatBuffer) | 3 schema (Arrow IPC schema message) | 4 batch (Arrow IPC record batch)
      | 5 end (end of a streaming response or stream) | 6 error (FlatBuffer) | 7 cancel | 8 ack (FlatBuffer) | 9 ping | 10 pong
```

Control payloads (`hello`, `request`, `response`, `error`, `ack`) are FlatBuffers with the root type `Message`, a union over every request and response type of section 3, size-prefixed and verified once on receipt (the `planus` verifier is linear in the buffer and rejects any out-of-bounds offset); fields are then read in place with no parsing and no allocation. Tabular payloads are Arrow IPC streaming format, one `schema` frame then `batch` frames, with the correlation id of the request they answer; Arrow IPC's own framing is FlatBuffers, so the whole protocol has one metadata encoding. A request's response is one `response` frame, or a `response` followed by `schema`/`batch`… frames and `end`. A subscription is a response followed by batches of events (D-5.1, one Arrow batch per up to 4,096 events, flushed at every barrier and at least every 10 ms live) that ends only on `cancel` or on the stream's end. Maximum frame size 64 MiB; larger tables are split into batches.

**The size rule.** A control payload holds only what scales with the program, the plan or the request: a demand, a binding, a query form, a status. Anything whose size scales with the data (a barrier index, a list of versions per contract, a catalog answer) is an Arrow table following the response. A control payload over 1 MiB is a protocol error.

[D] FlatBuffers over JSON, CBOR, MessagePack, protobuf, Cap'n Proto and SBE for the control plane. The owner asked for the fastest standard encoding. Zero-copy formats (FlatBuffers, Cap'n Proto, SBE) read without parsing or allocation, which the parse-based formats cannot match; among them FlatBuffers is already a dependency through Arrow IPC, has a pure-Rust compiler, and expresses nested, optional and variable-length messages (a `Demand`, a `Predicate`) that SBE's fixed layouts do not; Cap'n Proto needs the C++ `capnp` compiler and its RPC layer is not wanted. Protobuf is parse-and-allocate, and Arrow Flight is protobuf over gRPC, slower than this envelope on the same socket. The cost is a schema file as a second artefact beside the Rust types, which generation makes mechanical, and the loss of readable frames in a raw socket dump, which `abt wire decode` restores by rendering any frame through the owned types.

### 2.3 Sessions

A connection opens with `hello { protocol: "1.0", layer, client, capabilities }` from each side; a major-version mismatch is refused with `error { code: version }`. Requests on a connection may be pipelined; responses carry the request's correlation id and may interleave (a long aggregate and a point lookup). `ping`/`pong` every 5 s on idle connections; a connection without `pong` for 30 s is closed and its subscriptions released.

### 2.4 Cancellation and backpressure

`cancel { correlation }` stops a streaming response; the service answers `end`. A subscriber acknowledges consumed events with `ack { seq }` (D-5.6); the service blocks production when the channel is full and never drops.

### 2.5 Errors

```
error { code, message, detail }
code ∈ { version, malformed, unknown_relation, unknown_version, out_of_budget(plan), unresolved(need), refused(precondition), halted(reason), not_found, busy, internal }
```

Every error names what the caller can act on (the need, the plan over budget, the clause) in `detail` as the `abt-wire` type that carries it.

---

## 3. Services

### 3.1 Data service (`abt-data`)

| Request | Fields | Response |
| --- | --- | --- |
| `catalog` | `predicate` over registry relations (D-2.4, D-7.1) | a table |
| `schema` | `class`, `relation?` | `RelationSchema[]` |
| `resolve` | `Demand`, `dry_run: bool` | `{ plan: Plan, cost: CostVector, binding: Binding? , over_budget: Plan? }` |
| `resolve_dynamic` | `binding`, `DynamicNeed`, `underlying`, `t` | `{ binding': Binding }` (the binding extended with the fetched versions) |
| `query` | `View`, `QueryForm`, `ReceiptFilter`, `with_seq: bool` | a table (streamed) |
| `explain` | as `query` | `{ rows, bytes, partitions, cached }` |
| `aggregate` | `View`, `AggregateRequest` | a table (streamed) and `{ cache_key }` |
| `subscribe` | `StreamSpec`, `from_seq`, `paced: bool`, `to_seq?`, `with_index: bool` | `{ stream }`, then, when `with_index`, the barrier index as an Arrow table (`calendar`, `label`, `seq`) before the first event batch; then event batches until `cancel` or `to_seq` |
| `stream_index` | `StreamSpec` | the barrier index alone, as an Arrow table, computed from the merge without streaming (what the query layer reads to turn an embargo into a `to_seq`, Q-2.3) |
| `map` | `View`, `relation`, `columns`, `level`, entity range | segment descriptors as an Arrow table (`path`, `offset`, `length`, `codec_version`, the directory and header arrays' locations) and the tail descriptors with their visible counts at the view's sequence; the reader maps the immutable segments read-only and decodes them with `abt-codec` (ST-10.2). Same machine only; tails and append logs are read through `query` and `subscribe` |
| `ack` | `seq` | — |
| `put` | `relation`, `RelationSchema`, batches | `{ version, seq_range }` |
| `get` | `cache_key` or `version` | a table (streamed) or `not_found` |
| `pin` / `unpin` | `version | binding` | — |
| `stats` | `relation?` | a table |

### 3.2 Query forms

```
QueryForm = Point   { relation, identity: [Entity], t }
          | Latest  { relation, identity, t_max }
          | Series  { relation, identity, t0, t1 }
          | Section { relation, t }
          | Scan    { relation, t0, t1, predicate, columns? }
          | Exists  { relation, t0, t1, predicate } | Count { … } | Distinct { relation, column, t0, t1 }
          | Chain   { underlying, t, filters: ChainFilters, spot: Option<Num> }
```

Every form carries `View { versions | binding, as_of_seq }` and `ReceiptFilter`. Answers are in the relation's schema, ordered as D-4.1 states, with `seq` appended when asked.

### 3.3 Demands and bindings

`Demand` and `Binding` are D-7.2 and D-7.5 verbatim as types. The kernel builds the `Demand` from the `data` block (K-2.1) and sends it in `resolve`; the data layer returns the `Binding`, which the kernel records in `kernel.<run>.binding` and the query layer in `query.trials`.

### 3.4 Kernel service (`abt-kernel`)

| Request | Fields | Response |
| --- | --- | --- |
| `check` | `sources: [(path, text)]`, `strategy`, `catalog: [RelationSchema]` | `{ diagnostics, program_hash?, normal_form?, degrees_of_freedom }` |
| `contract` | as `check` | `{ clauses: [ContractClause] }` |
| `show` | as `check`, `what` | text and tables (V-8.4) |
| `run` | `sources`, `strategy`, `point`, `mode`, `deployment?`, `window?`, `embargo_mask?`, `to_seq?`, `from_checkpoint?`, `observe: [name]`, `checkpoint_every?`, `memory_budget?`, `plan_step?` (π) | `{ run_id }` then progress frames `{ barrier, seq, decisions, lag }` every N barriers and a final `{ status, report }` |
| `query` | `run_id`, `text` | a table, or `error { refused }` with the checker's diagnostic |
| `control` | `run_id`, `halt | flatten | checkpoint` | `{ ack }` |
| `status` | `run_id?` | the run's state, its barrier, its lag |

A `run` with `to_seq` is the truncation G5 and Q-2.3 use; `embargo_mask` is the block hold-out's mask the kernel applies to ledgers (Q-2.3); `from_checkpoint` names a cache key (K-4.7).

### 3.5 Query service (`abt-query`)

| Request | Fields | Response |
| --- | --- | --- |
| `plan_check` | `text` | `{ hash, normal_form, dag, trials_expected }` |
| `plan_run` | `text`, `threads`, `dry_run` | progress frames per step and a final `{ report tables }` |
| `plan_reveal` | `hash`, `step` | as `plan_run` |
| `plan_report` | `hash`, `format` | tables and text |
| `test` | `name`, `inputs: [TableRef]`, `params`, `seed?` | `TestResult` |
| `claim` | `Claim`, `run_id?` | `Verdict` |
| `feed` | `statistic`, `binding`, `window?` | a table |
| `activation` | `run_id` | tables |
| `lineage` | `program_hash` | tables |

`TableRef` names a relation in the data layer (`kernel.<run>.ledger.<R>`, a feed relation by binding, `query.<plan>.outcomes`), never bytes: the query layer reads through the data layer like everyone else.

---

## 4. Arrow schemas

Column types are D-2.1's; every table has `seq: UInt64` available on request. Field names are stable identifiers; a schema version string is in the IPC schema's metadata (`abt.schema = "1.0"`).

| Table | Columns |
| --- | --- |
| event stream batch (D-5.1) | `seq`, `kind: Dictionary(tuple|barrier|mark)`, `relation: Dictionary`, `entity: UInt64?`, `t: Timestamp?`, `r: Timestamp?`, `calendar: Dictionary?`, `label: Timestamp?`, `at: Timestamp?`, `mark_kind: Dictionary?`, then one column per value column of the relations the stream carries (nullable; a tuple fills its own relation's columns) |
| `kernel.<run>.events` (K-5.2) | `seq`, `t`, `kind: Dictionary`, `instrument: UInt64?`, `rule: Utf8?`, `order_id: UInt64?`, `decision: Binary?`, `quantity: Float64?`, `price: Float64?`, `commission`, `fee`, `slippage`, `impact`, `spread`, `participation`, `partial: Boolean?`, `forced: Boolean?`, `source: Dictionary?`, `reason: Utf8?`, `cash_flow: Float64?`, `legs: List<Struct{instrument, quantity, price}>?`, `detail: Utf8?` (JSON for the rare fields) |
| `kernel.<run>.book` | `t`, `instrument: UInt64?` (null for book-level rows), `position`, `avg_cost`, `mark`, `mark_source: Dictionary`, `cash`, `debit`, `receivables`, `variation_account`, `nav`, `capital`, `gross`, `net`, `leverage`, `margin_initial`, `margin_maintenance`, `margin_source: Dictionary`, `excess_liquidity` |
| `kernel.<run>.decisions` | `t`, `instrument`, `ctor: Dictionary`, `amount`, `order: Utf8`, `rule`, `late: Boolean` |
| `kernel.<run>.fills` | `t`, `instrument`, `quantity`, `price`, the cost components, `order_id`, `forced`, `partial`, `source` |
| `kernel.<run>.coverage` | `relation`, `calls`, `with_tuples`, `tuples`, `empty: Boolean`, `never_demanded: Boolean` |
| `kernel.<run>.contract` | `clause`, `kind: Dictionary(pre|inv|post|limit)`, `source`, `span`, `discharge: Dictionary`, `tier: Int8?`, `policy: Dictionary`, `status: Dictionary`, `detail` |
| `kernel.<run>.binding` | `program_hash`, `binding_id`, `mode`, `deployment_hash?`, `relation`, `version`, `point: Utf8` (JSON), `normal_form: Utf8` |
| `kernel.<run>.plans` | `rule`, `position`, `literal`, `class: Dictionary`, `estimated_cost`, `pushdown: Boolean`, `observed_calls`, `observed_tuples` |
| `kernel.<run>.ledger.<R>` | `bar`, then R's columns, `stuck: Boolean`, `stuck_literal: Utf8?` |
| `kernel.<run>.late` | `seq`, `relation`, `entity`, `t`, `r`, `changed_decision: Boolean` |
| `kernel.<run>.report` | `key`, `value: Utf8` (JSON), `bias: Dictionary?` |
| `query.plans` | `hash`, `name`, `normal_form`, `created`, `bindings: List<Hash>` |
| `query.trials` | `trial_id`, `plan_hash?`, `step?`, `lineage`, `program_hash`, `binding_id`, `run_id?`, `point`, `window_from`, `window_to`, `holdout`, `kind: Dictionary`, `objective`, `value`, `test`, `test_version`, `at` |
| `query.<plan>.outcomes` | `run_id`, `point`, `fold?`, every metric of Q-3.1 as a column |
| `query.<plan>.tests`, `verdicts` | `name`, `test`, `test_version`, `input`, `window`, `params`, `statistic`, `p_value`, `p_adjusted`, `interval_lo`, `interval_hi`, `level`, `effect`, `n`, `dependence`, `phase`, `verdict`, `in_sample: Boolean`, `notes` |
| `query.<plan>.report.*` | as Q-8 |
| aggregate answer (D-6.1) | `entity?`, `t` (or `label`), `value: Float64`, `count: Int64` |

---

## 5. Ordering and determinism across the protocol

**Proposition 5.1 [N] (transport invisibility).** A client's sequence of responses, and the contents of every table and stream it receives, are identical over the in-process channel and over the socket, and identical across any number of server threads. *Proof.* Every answer is a deterministic function of (view, `as_of_seq`, request) (D-4.2) or of a recorded stream prefix (D-5.4); the frames carry the same bytes in both transports; interleaving of responses to pipelined requests is by correlation id, which the client matches, so the per-request content is unchanged. ∎ This is G3's statement at the protocol level.

**Definition 5.2 (what may differ).** Timing, the `lag` marks' values, and the order in which responses to *different* requests arrive. Nothing a run writes to the data layer depends on any of these (K-6.3 records lag in the report, not in decisions, except through the declared `late` policy whose effect is itself recorded as a `late` flag on the decision).

---

## 6. Canonical forms and hashing

Every `abt-wire` type has a canonical byte form, distinct from its wire encoding: JSON per RFC 8785 (JSON Canonicalization Scheme: sorted member names, no whitespace, shortest round-trip number rendering, UTF-16 code-unit string ordering) rendered from the owned type, with `f64` NaN and infinities forbidden in canonical forms (an error at construction) and `i64` written as decimal integers. A hash is BLAKE3 over the canonical bytes. The wire encoding is never hashed: a FlatBuffer's bytes depend on the builder's layout and padding, so two equal messages need not be byte-equal, while their canonical forms are. Hashing is rare (once per program, plan and binding), so the cost of the rendering is irrelevant; readability is not, since `abt hash --show` prints the hashed bytes. The normal form of a program (V-8.4) and of a plan (Q-2.4) are rendered as text by the kernel and the query layer respectively and hashed as UTF-8 bytes, so that `abt hash` and `abt plan check` print what was hashed.

`--json` on the command line renders the owned types with `Time` as ISO-8601 strings with nanoseconds and hashes as hex, because readers outside Rust lose integers above 2⁵³; the canonical form keeps the integer, and the two renderings are never confused because only `Canonical` feeds a hash.

[E] RFC 8785 (Rundgren, Jordan, Erdtman 2020); BLAKE3.

---

## 7. Compatibility

The protocol version is `major.minor`. A minor change adds optional fields, frame kinds or request kinds; a major change changes or removes any. A service refuses a client of another major (2.3). Table schemas carry their own version in metadata; a reader refuses a major it does not know. Versions written by an older schema are read through the data layer's `schema` registry, which keeps every schema version a relation was ever written with (D-2.2), so an old version remains readable.

---

## 8. What is established, new and decided in this document

- [E] Arrow IPC streaming format; FlatBuffers (the Arrow IPC metadata encoding); RFC 8785; BLAKE3; the aggregate classes (F-8); Chan–Golub–LeVeque; Neumaier summation.
- [N] One implementation, one answer (1.1); transport invisibility (5.1).
- [D] Unix sockets and in-process channels only (2.1); FlatBuffers control frames and Arrow data frames, the fastest standard pairing, with the reasons and the rejected alternatives in 2.2; the size rule that keeps data-sized payloads out of control frames (2.2); the schema file as the source of truth with generated owned types (1.1); canonical forms separate from the wire encoding (6); no authentication (in-house, socket permissions); the `abt-wire` no-logic rule (1.1); `abt-agg` requires and asserts canonical order rather than sorting (1.2).
