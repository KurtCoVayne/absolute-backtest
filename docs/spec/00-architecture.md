# 00 · Architecture: the three layers

Oct 12, 2026 · specification for implementation. This document and its seven companions (`01-data.md`, `02-kernel.md`, `03-query.md`, `04-protocol.md`, `05-migration.md`, `06-performance.md`, `07-storage.md`) are the build sheet for splitting `absolute-backtest` into three independent Rust programs. Nothing in them is left to design; where a value is still the owner's to supply, section 8 names it and the default that stands until it is supplied.

Tags on claims: **[E]** an established result, with a reference. **[N]** a new proposition, with a proof or proof sketch. **[C]** a conjecture, with what would settle it. **[D]** a design decision, with the alternative it rejects and why.

Cross-references: `S-n` is `docs/semantic-model.md` section n; `F-n` is `docs/formal-foundations.md`; `V-n` is `docs/language-v2.md`; `B-n` is `docs/data-bundle.md`; `C-n` is `docs/compiler-design.md`; `D-n`, `K-n`, `Q-n`, `P-n`, `M-n`, `X-n`, `ST-n` are sections of the seven companion documents (`X` is `06-performance.md`, `ST` is `07-storage.md`). Decisions in this document are numbered `A-D1` to `A-D50` and are cited by the companions; A-D35 to A-D46 were added with `07-storage.md` (Oct 15, 2026), A-D47 onward by the iterations that implement it (`docs/iterations/`).

---

## 1. The result in one paragraph

The system becomes three programs that share nothing but a protocol. **The data layer** is a store, a query engine and a resolver: it holds typed relations with two timestamps, answers structured queries and aggregate requests in bounded time, streams events in a canonical order with explicit clock barriers, and plans the cheapest way to satisfy a demand from what it has cached, what the vendors sell and what the feeds deliver. It interprets nothing. **The kernel** is the verifier and the market simulator: it parses, checks, normalises, analyses and plans a program, derives the program's contract from its structure, binds its demand through the data layer, evaluates it as a fold over the stream, maintains the book, simulates execution for equities, futures and options with their events, and serves its state tables and query language to clients. **The query layer** is a client of both: it executes plans (studies), computes metrics, runs the developer's statistical tests against feeds, features and outcomes, judges claims, counts trials and writes reports. Each layer is a crate with a library, a binary and a `serve` mode; the front binary `abt` links all three over an in-process transport and behaves bit for bit as the three processes do over sockets. The acceptance test is unchanged: MW14's 1,852 weeks and R8L's 21,892 legs equal their references, `corpus-v2` checks and runs, and every run is a function of the program, the binding and the plan alone.

---

## 2. The three layers

**Definition 2.1 (layer).** A layer is a crate `L` with a library API, a binary `abt-L`, and a service interface `L.svc` defined in `04-protocol.md`. A layer depends on the shared crates `abt-wire` and `abt-agg` (P-1) and on no other layer's crate. Two layers interact only through a service interface, over an in-process channel or a socket (P-2).

| Layer | Crate | Owns | Does not own |
| --- | --- | --- | --- |
| Data | `abt-data` | schema registry; identity registry; the store (hot tail, partitions, indices, versions, cache); structured queries; aggregates over stored relations; event streams with clock barriers; the source catalog, cost model and resolver; vendor adapters (Norgate, Databento, ORATS; FIX market data reserved); storage of every other layer's artefacts (runs, ledgers, checkpoints, plans, trials) as relations | point-in-time semantics, as-of reads, causality, hold-out, the meaning of any relation, bundle tests of market facts |
| Kernel | `abt-kernel` | lexer, parser, IR; checker WF-1 to WF-17 and the added judgments of K-3; normaliser and program hash; analyser; contract derivation; planner with pushdown; evaluator; fold; the book; market simulation and instrument models (equity, future, option); margin models; cost models; the `Executor` trait and its simulated implementation; state tables Σ_t; the query language `?`, `why`, `why not`, `trace`, `coverage`, `show`, `ledger`; checkpoints | metrics, statistics, plans, trial counting, vendor access, storage |
| Query | `abt-query` | plan language and executor (studies); lineages and trial accounting; metrics library; the test framework and its starting set; claims; feed statistics; reports; the bias audit | evaluation of programs, the book, storage |

**Definition 2.2 (front).** `abt` is a binary that links the three crates and the in-process transport. Every subcommand of `abt` is a client call on one service; `abt` contains no logic beyond argument parsing and dispatch (A-D5).

---

## 3. Decisions record

Every decision below was taken in the design conversation of Oct 12, 2026 or follows from one that was. Each names the alternative it rejects.

| # | Decision | Rejected alternative, and why |
| --- | --- | --- |
| A-D1 | **The split lands the second formulation** (V-4: blocks, `data` demand, calendars, `clock`, `universe`, `execution`, `report`; no modes; order-independent bodies). `corpus-v2/` is the acceptance corpus. | Splitting on v1's `environment` files and `ExecConfig` builds the kernel–data interface twice; the v2 `data` block *is* that interface. |
| A-D2 | **Three crates in one Cargo workspace**, plus two shared crates `abt-wire` (types and messages, no functions beyond serialisation) and `abt-agg` (aggregate kernels). | Three repositories with wire-only contracts duplicate the type definitions in three places and make the one-implementation rule for aggregates (A-D8) impossible to enforce. |
| A-D3 | **The protocol is the only boundary**, in-process or over a socket; the in-process transport is a channel pair carrying the same frames. | Direct crate calls between layers would make "independent program" a fiction the first convenience violates. |
| A-D4 | **All Rust.** No new non-Rust program. The existing pandas reference (`reference/`) stays as the executor's differential test; the query layer's tests are verified against certified known-answer data and property tests (Q-4.6). | A statsmodels reference for the test library: a second implementation whose numerics are not ours and whose version is not pinned by the build. |
| A-D5 | **`abt` is a thin front**; each layer also ships its own binary. | A single binary only: the study case (one data process serving many kernels) needs the processes. |
| A-D6 | **The data layer has no semantics.** Point-in-time, as-of, causality, hold-out and truncation are the kernel's and the query layer's, expressed as filters on data-layer queries (receipt ≤ A, event time ≤ t). | Bundle tests of market facts (membership, survivorship) in the data layer: they interpret relations the data layer must not understand; they become kernel preconditions (K-3.2) and query-layer feed statistics (Q-7). |
| A-D7 | **Two timestamps per tuple**: event time `t` (the bar label) and receipt time `r` (when the source delivered it; `r = t` when the source has none). Nanoseconds since the Unix epoch, UTC, `i64`. | Seconds (the present `i64`): sub-minute bars and receipt times need the resolution. |
| A-D8 | **One implementation of every aggregate** in `abt-agg`, used by both layers; the data layer materialises aggregates over stored relations, the kernel over derived relations, and the kernel's planner pushes an aggregate down when its input is stored and its window is closed at the barrier. | Aggregates in one layer only: all in the kernel wastes the data layer's columnar locality on the commonest case (bar views, rolling moments of primitives); all in the data layer cannot see derived relations. Two implementations break bit identity. |
| A-D9 | **Explicit clock barriers.** The data layer emits `Barrier{calendar, label}` events from a clock source (the stored labels in replay; the exchange calendar and the wall clock live); the kernel never infers a bucket close from a tuple. | The present `Scheduler`, which closes buckets when a tuple's availability passes them: correct in replay, wrong live when a bar closes with no tuple. |
| A-D10 | **Canonical stream order** (receipt, relation id, identity, sequence), with a monotone sequence number per event; a checkpoint's cursor is a sequence number. | The present tie-break by insertion index, which is not stable across rebuilds. |
| A-D11 | **Determinism across processes**: a run's output is bit-identical whatever the number of kernels sharing a data process, the number of threads anywhere, or the transport. Every data-layer answer in replay is as of a sequence number; nothing depends on arrival timing. | "Latest available" answers in replay: convenient and non-reproducible. |
| A-D12 | **The kernel owns the book** (positions, cash, NAV, exposure, margin, receivables, working orders) as the state table X of Σ_t, exposed to queries like any relation, and emits the event log beside it. | The query layer reconstructing the book from events: the book is a kernel concern (sizing, limits, margin) and must not exist twice. The event-fold-equals-book identity remains a kernel self-postcondition (K-3.4). |
| A-D13 | **The kernel emits decisions and execution events; the query layer computes metrics.** Sharpe, volatility, drawdown, DSR, PBO and every statistic are query-layer. | Metrics in the kernel (the present `RunResult` helpers): a statistic is not a property of a run, it is a property of a question about a run. |
| A-D14 | **Contracts are derived from the program's structure**, never authored: preconditions, invariants, postconditions and risk limits follow from the blocks, the rules, the types and the analysis (K-3). | A `contracts` block: a second place to write what the program already says, and a second thing for a model to get wrong. |
| A-D15 | **Deterministic in the kernel, statistical in the query layer.** A property with a p-value, an interval or a distribution is a claim (Q-5), never a contract. | `ensure sharpe >= 1` as a kernel postcondition: it is a statistical statement and belongs with the trial accounting that makes it honest. |
| A-D16 | **The query language is the kernel's** (a query is a rule judged by the checker, V-8.1); the query layer is its client. | The parser in the query layer: a query must be checked for causality and types by the checker that knows the program. |
| A-D17 | **A study is a plan over the query layer** (Q-2): a declared DAG of runs, embargoes, grids, walk-forwards, reveals, tests, claims and reports, hashed, executed, logged; every step that reads an outcome is a counted trial. | The study as a kernel API inside the checker (B-1): revised by the split; the kernel does not count trials. |
| A-D18 | **Claims live in the plan**, not in the program. | Claims in the program: they are the developer's questions about the implementation, not the implementation. |
| A-D19 | **The test library is a framework the developer extends** (Q-6): a trait, a registry, a starting set. | A closed set: the library is the developer's method for questioning the implementation and cannot be enumerated in advance. |
| A-D20 | **The study database is the data layer.** Plans, trials, claims, verdicts, lineages, run manifests, ledgers and event logs are relations in the data layer's store; one writer process. | SQLite or DuckDB: a second storage engine, a second query interface, a C dependency. |
| A-D21 | **The data layer resolves demands** (D-7): it maintains a source catalog with costs and freshness, plans the cheapest cover of a demand from the local store, the vendors and the feeds, fetches under a budget, versions what it fetched, and returns a binding that pins versions. | Resolution in the kernel: the kernel would have to know vendors, prices and caches, which are facts about data supply, not about programs. |
| A-D22 | **Budgets default to zero money.** A demand that needs a purchase names the budget; the resolver refuses to exceed it and prints the cheapest plan that would. | Buying silently: a backtest that costs money without a line saying so. |
| A-D23 | **Static and dynamic demands.** A static demand (bars of a listed universe over a span) is resolved before the run; a dynamic demand (an option chain by moneyness at bar t) is resolved per barrier by a structured query served from cache or fetched under the budget, with a pre-pass in replay as the default. | Resolving everything before the run: impossible for option chains at the needed scale (full OPRA); resolving everything on demand: stalls every barrier of a replay. |
| A-D24 | **Vendors in scope: Norgate (NDLake), Databento (historical and live), ORATS (options).** FIX market data is reserved in the source catalog. | Any other mapping on day one: not needed. |
| A-D25 | **Options are in scope** with the full event model (K-5.4): quotes-based marks and fills from ORATS, Greeks from the data layer when supplied and from the kernel's documented models otherwise, exercise and assignment rules, corporate-action adjustments, combo orders, margin by the models of K-5.6. | Reserving the vocabulary only: the owner asked for options now. |
| A-D26 | **Margin is not simplified** (K-5.6): published margins from data when present, Reg T for securities, a scenario method for portfolio margin, SPAN from risk arrays when present, documented approximations only as warned fallbacks. | Reg T formulas alone: options strategies are as realistic as their margin. |
| A-D27 | **Three run modes, one evaluator**: `replay-sim`, `live-sim` (live feed, simulated executor), `live-broker` (live feed, broker executor). Decisions are a function of the event stream in every mode (A-4.3). `live-sim` is in scope; `live-broker` is specified at the interface only (K-6.4) and implemented later, IBKR first. | Specifying the FIX adapter now: the owner deferred the live layer; the interface is fixed so nothing is retrofitted. |
| A-D28 | **Deployment is separate**: account, routing, session parameters, kill switch and reconciliation policy are a `deployment` document outside the program hash, taken by the kernel in live modes only (K-6.5). | In the program: a strategy's hash would change with the account it trades in. |
| A-D29 | **One program per kernel instance.** No multi-strategy allocator in this specification. | A book of books: a later layer above the kernel. |
| A-D30 | **The kernel refuses a program whose derived state bound exceeds a declared memory budget** (K-4.6). | Discovering it at run time. |
| A-D31 | **Scale and latency targets** (section 7.3) are the owner's draft numbers; the data layer's layout is sized by them. | None. |
| A-D32 | **USD only.** The dimension system carries currencies; FX conversion is reserved as an event kind (K-5.2) and not implemented. | Multi-currency now: needs rates, conversion events and a second book convention. |
| A-D33 | **No MCP server in this specification.** Every command has `--json`; the socket protocol is the agent's API when one is wanted. | None now. |
| A-D34 | **v1 is retired by the migration** (M-9): `corpus/`, `environment` files, `ExecConfig`, the 72 options and the `study` subcommands as they exist. `--param` survives for plans. | Keeping both surfaces: two meanings of every command during the migration. |
| A-D35 | **Prices and quantities are stored as integer ticks** with a per-block quantum (a decimal exponent times a GCD found from the data); `f64` is a view computed on read; every vendor value round-trips bit for bit (ST-3). | `f64` or `f32` storage: `f64` sums are not associative, Databento's ticks are not `f32`, and the GCD saves 5 to 19 bits a value on binary-fraction ticks (ES's 0.25 to ZT's 1/256, ST-2.2). |
| A-D36 | **The resident form is instrument-contiguous, lane-interleaved blocks** of 1,024 values, offsets from a base with patched exceptions and OHLC predicted from the close, memory-mapped from immutable segments (ST-4). Decode-and-sum runs at 0.18 to 0.38 ns a value, faster than an uncompressed scan. | Uncompressed columns (about 26 GB and slower); entropy coding in memory (pcodec: 10 to 20 times slower to decode); time-major order (2.4 times larger, ST-2.3). |
| A-D37 | **Everything independent of a program is precomputed and hot** for Norgate and Databento: cumulative block summaries and cross-moments, block min/max with sparse tables, the full calendar ladder, presence with rank directories, catalog structures with interval indexes, standing event aggregates, a revision table for receipt-time reads, wavelet matrices for close and volume (ST-5). Every encoding and structure answers a range without decoding outside the blocks it covers (ST-6, audited and measured). Program-dependent results are recomputed in run-scoped memory. | Caching rolling results: unbounded in number and incompressible. |
| A-D38 | **Data enters through a seven-stage staging pipeline** (land, normalise, classify, build, verify by decoding, publish atomically, export and collect) with content-addressed segments shared between versions (ST-7). | Rebuilding the store per update: minutes instead of milliseconds, and no structural sharing. |
| A-D39 | **Appends are written in place into per-instrument tail blocks** and become visible by sequence number; revisions of history are copy-on-write versions (ST-8). | Appends as new versions: a version a minute live. |
| A-D40 | **ORATS is not held hot**; its holding is a separate decision (ST-11). Norgate and Databento together are about 6.8 to 7.2 GB hot. | Any hot ORATS design: about 87 GB at the smallest lossless encoding. |
| A-D41 | **Durability comes from the log and a seal journal**; tails are never persisted on their own and are rebuilt from the log on restart, serving again within 30 s (ST-8.8). | Persisting tails in place: torn writes, and a second source of truth beside the log. |
| A-D42 | **Sealed blocks go to append segments, and consolidation is the only compaction**: weekly, a byte-for-byte copy back into instrument order. Version ids are logical (block hashes), so bindings and pins follow a version across layouts (ST-8.7, ST-7.1). | Writing into base segments in place (breaks immutability and mapped readers); never consolidating (locality decays every day). |
| A-D43 | **Adjusted series are views** (split, total return, continuous futures) evaluated piecewise between events over precomputed event-interval summaries; exact for splits and additive adjustments, a fixed `f64` definition for total return (ST-5.10). | Stored adjusted copies: one per adjustment, and every split or dividend rewrites an instrument's history. |
| A-D44 | **Ticks cross the protocol as Arrow `Decimal128(18, s)`**, so `exact` aggregates agree bit for bit across layers (ST-10.4). | `Float64` only (`exact` could not cross layers); `Int64` with scale metadata (not a standard Arrow type). |
| A-D45 | **Hot means locked**: the store is prefaulted and `mlock`ed; memory profiles (`dedicated`, `workstation`) and a fixed shedding order keep the peak inside the machine (ST-9.1, ST-9.2). | Relying on the page cache: a dropped page costs tens of microseconds and raises no error. |
| A-D46 | **Futures calendars**: UTC-aligned intraday buckets, sessions and days by trade date, every receipt at its bucket's end, a Databento bar at its close (ST-5.3, ST-4.7). | Exchange-local alignment (identical for CME and ICE, but time-zone data on the hot path); UTC days (they cut every Globex session in two). |
| A-D47 | **`exact` over what has no integer form is `two_pass`**: a float column or a derived series is reduced as `two_pass` under `numerics exact`, and `std`, `corr` and `ols_r` take their exact centred sums to `f64` before the square root (D-6.3; added in iteration 1, Oct 18, 2026). | Refusing such a request: a program that reads a tick column and a float series would need two numerics blocks; rounding the floats to a grid: a value the program never wrote. |
| A-D48 | **The segment file and the directory rule** (ST-4.8, ST-7.1; iteration 1): a segment is little-endian, `ABTSEG\0\1`, the codec version, then runs (one instrument's consecutive blocks of one column: the 32-byte headers, the packed words 64-byte aligned, the exception lists, the 48-byte block summaries and the 88-byte cumulatives with `Σx²` and `Σk·x` in 256-bit two's complement), a run table and a trailer with the BLAKE3 of everything before it; named by that hash under `hot/segments/`. A version's directory reads its segment list in order: a run starting at block 0 replaces an instrument's table, a later run extends it from its first block. A block's hash is BLAKE3 over its header, words and exceptions; a version's id hashes them in (column, instrument, block) order with the revisions, the schema and the sequence high-water. | One file per instrument or block (millions of files, no sequential prefault); Parquet pages (no range closure, ST-6.1); per-block hashes stored (4 M × 32 B for a check that recomputation gives). |
| A-D49 | **Which relations are hot**: a version whose attribute `storage = hot`, which the adapters set for the bar relations (`open`, `high`, `low`, `close`, `volume`) of the markets they stage; every other relation, the registry's and the other layers' `put`s stay in the Parquet partitions of D-3.1. A relation whose rows the blocks cannot hold (no row, a null value, a bytes or second entity column) stays in Parquet and its version says `storage = parquet` (iteration 1). | Every relation hot: the registry's relations have optional columns and the event relations are small; refusing the rows: an adapter's market would fail to ingest for a null in a dividend. |
| A-D50 | **Receipts and sequences in a hot version** (ST-4.7, ST-8.2; iteration 1): a row's receipt time is stored as the integer residual `r − t`, a column of the block format like any other (constant under a receipt rule, so it packs to nothing); rows are numbered per instrument in label order, the log's acceptance order for a hot relation being (instrument, time, receipt), and each instrument carries its sequence runs `(row, sequence, count)`, the (sequence, visible count) form of ST-8.2 that a snapshot read filters by. | A per-row `seq` column (2 to 4 bytes a value against ST-9's 2); per-row receipts (8 bytes); numbering in the vendor's delivery order (time-major), which makes a snapshot's rows a non-prefix of every instrument. |
| A-D51 | **Appends beside immutable blocks** (ST-8.3, ST-8.5, ST-8.7, ST-8.8; iteration 1): a published block is never rewritten (A-D48); an instrument's appended rows live in a tail of their own in memory (a function of the log) and, sealed by 1,024, in append files of one column family each (the value columns, the receipt residual, the labels, the sequence numbers) under a journal; a row at a label the tail holds is a revision (S2 to S5), since the presence holds one row per label and superseded rows live in the revision table. Consolidation copies every sealed block that follows full blocks as it is (its hash carries over) and re-encodes the rows from a partial published block on into full blocks (every block but an instrument's last is full, ST-4.1), checked by decoding; it starts a new journal generation, and the version id is the published state's. The block hash leaves out the header's exception locator. A relation a source holds hot stays hot (a write without the attribute follows the current version). | Appending in place into the published tail block (a mutable byte range inside a content-addressed segment: the segment's hash and every mapped reader's view would change under it); variable-size blocks inside a run (every range operation would need a row-to-block search instead of a shift); in-tail replacement of a row at a held label (a second row per label has no bit in the presence and would be lost at the seal); recomputing the version id at consolidation (pins and bindings name it, Proposition 7.1). |
| A-D52 | **One deployment document for the rehearsal and the live run** (K-6.5; iteration 1): a `live-sim` run takes the deployment document a `live-broker` run takes, with the same hash: its kill switch, deadlines and recording stream apply, and its account, broker session, routing and reconciliation fields are logged but not acted on, there being no broker in `live-sim`. Rejected: a document of its own for `live-sim`, or refusing the broker fields in it, since then the rehearsal would not prove the document the live run uses; and taking the session from `--session` only, since then the document's recording stream would be unchecked until the live run. |
| A-D53 | **A continuous future's back-adjustment is computed in the contract's tick units** (D-2.4, ST-3.2; iteration 1): the Databento adapter takes each roll's gap and each session's cumulative adjustment as integers at the root's grid, the smallest decimal exponent at which every price of its bars and its contracts' tick sizes (`min_price_increment`, which can be coarser than the prices traded) are integers, adds them to a bar's prices as integers and divides once, so every adjusted price stays on the vendor's grid and the stored series keeps its exact view; the `continuous` relation's `adjustment` is the same quantity. A root with a price off every decimal grid falls back to the `f64` addition, and such values are raw exceptions in their blocks (A-D54). Rejected: `f64` addition throughout, which puts cent-grid prices (CL) one ulp off every decimal grid after a roll, costs those blocks their exact view, and makes the stored series depend on the order of the additions. |
| A-D54 | **A value off every decimal grid counts as 0 in the exact structures** (ST-3.2, ST-5.1; iteration 1): a block keeps such a value as its raw bits (the `f64` view returns them, Proposition 3.2) and, in the block's summaries, its cumulatives and its decoded integers alike, the position counts as 0, so S4's range checks compare the same number on both sides; a column holding one is flagged in the manifest and has no exact view: `numerics exact` and a `ticks` read of its relation are refused naming the column, and the `f64` view answers. Rejected: the block's base at the position (what an unpatched decode gives: the summaries and the decoded rows then disagree, as the Databento subset's S4 showed), and refusing the whole relation (its other columns are exact). |
| A-D55 | **A run's id names the live session it reads or the recording it replays** (A-4.1, K-6.2, D-5.5; iteration 1): π, which for a run outside a plan stands for what of the request changes what the run decides (E-56), carries the session of a `live-sim` or `live-broker` run and the run id a `--replay-of` run replays, so two sessions of one program over one binding are two runs, each with its outputs under its own id, and so are the replays of their recordings; a replay-sim run over the binding alone is unchanged. Rejected: the mode alone in the id, under which a second rehearsal on the same binding collided with the first's outputs (the data layer refused its rows as a conflicting revision of the first run's); and the session in β, which pins versions of relations, while a live session's rows are not a version until recorded. |
| A-D56 | **A live run binds its session's versions, and a replay those of the session its recording read** (D-5.5, D-7.1, K-6.2; iteration 1): the demand's `prefer` list gains `source(s)`, which puts the versions put under source `s` first where `s` holds the relation and leaves every other need's order as it was; the kernel names its session there in `live-sim` and `live-broker`, and in a `--replay-of` run the session the recording read, which the data layer records by putting the run's clock (`kernel.<run>.clock`) under the session's name; the resolver holds each current version under the source it is current for, since a version two sources share (the same content: two sessions' empty preludes) has one record. Rejected: requiring the session as an attribute of every need (a need the session does not cover, such as actions or a calendar the base store holds, would be unresolvable); binding a replay to the live run's recorded pairs (the live run itself had bound the first session by name); the resolver's default order (the first source by name: a store holding several sessions of the same instruments bound the oldest, and the rehearsal's replay diverged from its run). |

---

## 4. Cross-layer invariants

The following hold across the three layers and are tested by the acceptance suite of section 7.

**Definition 4.1 (identity of a run).** A run is identified by the triple (H(P), β, π): the program hash of the normal form (V-8.4, form 3), the binding returned by the resolver (D-7.6), which pins a version of every relation the program reads and the parameter point, and the plan step π that requested it (Q-2.5), or `∅` for a run outside a plan. A run id is the hash of the triple and the run mode. For a run outside a plan, π stands for what of its request changes what it decides: its window, truncation and bundle date (ERRATA E-56), and the live session it reads or the recording it replays (A-D55).

**Proposition 4.2 [N] (reproduction).** Two runs with equal (H(P), β) in `replay-sim` produce byte-identical event logs, books, coverage tables and ledgers. *Proof.* The program is closed (V-2.1): its meaning is a function of P and the data it reads. β pins immutable versions (D-3.5), so the data read is identical. The stream order is canonical (A-D10) and barriers are data (A-D9), so the sequence of events is identical. The evaluator and the simulation are deterministic (F-5; K-4.1), the aggregate kernels are one implementation on one reduction order (A-D8), and no answer of the data layer depends on timing (A-D11). ∎

**Proposition 4.3 [N] (mode invariance of decisions).** For a fixed program and parameter point, the decision events emitted by the kernel are a function of the sequence of market events and barriers it has consumed, in every mode. *Proof.* The evaluator reads Σ_t only (V-8.8); Σ_t is a function of the events consumed and of the executor's writes to X; the executor's writes are fills, which in `replay-sim` and `live-sim` are a deterministic function of the same events and in `live-broker` are events of the stream (K-6.4). In every mode the decision at a barrier is therefore determined by the events up to it. ∎ What differs between modes is the fills; what is identical is the mapping from consumed events to decisions.

**Proposition 4.4 [N] (replay equals live).** A `live-sim` run and a `replay-sim` run over the recorded stream of the same session, with the same program and binding, produce identical decision and execution events. *Proof.* The data layer records every live event with its receipt time and sequence number (D-5.4); replay re-emits the recorded sequence (D-5.5); by Proposition 4.3 the decisions are equal, and the simulated executor is a function of the same events. ∎ This is Corollary F-7.3 across the process boundary and is tested by M-6's gate.

**Definition 4.5 (the clock is data).** Time advances in the kernel only on barrier events. A kernel that receives no barrier does not evaluate. A tuple whose event time precedes the open bucket of its calendar is late (D-5.3) and is handled by the kernel's late-data rule (K-4.4), never by the data layer.

**Proposition 4.6 [N] (observation and queries change nothing).** A query from any client, an observation declared in a program, a ledger written for a plan, or a statistic computed by the query layer changes no decision and no fill of the run it reads. *Proof.* V-8.4 and V-8.8 for the kernel's own queries; a query-layer computation reads relations the data layer stores after the kernel wrote them and never writes to the kernel. ∎

**Definition 4.7 (the numerics contract).** Every floating-point reduction in every layer is performed by `abt-agg` in the canonical reduction order (entity ascending by id, then event time ascending), with the `numerics` field of the program's `execution` block selecting the exact two-pass or the running form (V-4.3; C-6.8). No layer reassociates a sum. A data-layer aggregate carries the mode it was computed under in its cache key (D-6.3).

---

## 5. Modes

| Mode | Stream source | Executor | Deployment document | Fills | In scope |
| --- | --- | --- | --- | --- | --- |
| `replay-sim` | the store, replayed from a cursor | simulated (K-5) | none | simulated | yes |
| `live-sim` | a live feed, recorded as it arrives | simulated | optional (kill switch only) | simulated | yes |
| `live-broker` | a live feed, recorded | broker adapter (K-6.4) | required | the broker's, as events | interface only |

In every mode the kernel's output is the same set of relations (K-7.1) written to the data layer under the run id, and the same query service.

---

## 6. Identifiers

| Identifier | Computed by | Over | Form |
| --- | --- | --- | --- |
| `version` | data layer | the bytes of a relation's immutable dataset | 32-byte BLAKE3, hex |
| `H(P)` | kernel | the normal form of a program (V-8.4 form 3), the `report` block and comments removed | 32-byte BLAKE3, hex |
| `β` (binding id) | data layer | the sorted list of (relation, version) pairs plus the parameter point and the dynamic-demand filters | 32-byte BLAKE3, hex |
| `run id` | kernel | (H(P), β, π, mode), π naming the live session read or the recording replayed (A-D55) | 32-byte BLAKE3, hex |
| `H(Π)` (plan hash) | query layer | the normal form of a plan (Q-2.4) | 32-byte BLAKE3, hex |
| `trial id` | query layer | (H(Π), step, run id) | 32-byte BLAKE3, hex |
| `seq` | data layer | position in a stream | `u64`, monotone per stream |

[D] BLAKE3 over FNV-1a (the present `fingerprint`): the present hash is 64 bits over a `Debug` rendering, which changes with the Rust formatting of the types; the specification needs a hash that is stable across releases and collision-free in practice. BLAKE3 is pure Rust.

---

## 7. Acceptance

### 7.1 Gates

Every step of `05-migration.md` keeps these green; the specification is accepted when all hold on the three-process deployment.

| Gate | Statement | Where |
| --- | --- | --- |
| G1 parity | `corpus-v2/company/mw14.dsl` reproduces 1,852 of 1,852 weekly returns to 8e-15; `corpus-v2/company/r8l.dsl` reproduces 21,892 of 21,892 legs and 6,850 of 6,850 days | `scripts/parity/` |
| G2 corpus | every strategy of `corpus-v2/strategies` and `corpus-v2/company` checks clean modulo its `# allow:` lines; every negative case produces its `# expect:` code and no other error | `tests/corpus_v2.rs` |
| G3 determinism | a run repeated in-process, over sockets, with 1 and 8 data-layer threads, with 1 and 4 kernels sharing the data process, produces byte-identical outputs (Prop. 4.2) | `tests/determinism.rs` |
| G4 replay | a `live-sim` session on a recorded feed replayed as `replay-sim` produces identical events (Prop. 4.4) | `tests/replay.rs` |
| G5 fold | a run stopped at any barrier equals a run over the stream truncated at that barrier (the causality theorem, F-7.1, operationally); a run resumed from any checkpoint equals the unbroken run | `tests/fold.rs` (rewritten) |
| G6 reference | every daily strategy of the corpus and the four action markets agree with `reference/engine.py` on fills, book and NAV (B-8) | `tests/reference.rs` |
| G7 contracts | every derived contract clause of every corpus program is proven, checked or reported (never silently absent); every invariant violation injected by the test suite halts | `tests/contracts.rs` |
| G8 options | the options cases of K-9.3 (exercise, assignment, expiry, combos, margin, adjustments) produce their hand-computed events; the owner's options book, when supplied (section 8), reproduces its reference | `tests/options.rs` |
| G9 tests | every statistical test of the starting set reproduces its known-answer values to the stated tolerance (Q-4.6) | `tests/known_answers.rs` |
| G10 performance | the targets of 7.3 on the reference machine | `scripts/bench.sh` |

### 7.2 What is not a gate

The usability and observability studies (`docs/llm-usability.md`, `docs/llm-observability.md`) are rerun after M-7 and recorded, never asserted.

### 7.3 Targets

Reference machine: Apple M4, 10 cores, 16 GB (as `docs/assessment.md`). The owner's draft numbers stand until replaced.

| Target | Value |
| --- | --- |
| Instruments live | 10,000 at @1m |
| History resident | 30 years @1d; 5 years @1m; for Norgate and Databento, all history (ST-9) |
| Barrier to decision, MW14-class program, live | < 50 ms |
| Sustained ingest | 200,000 rows/s |
| Cross-section of one bar from the hot tail | < 1 ms |
| Point lookup, range scan per entity | O(log n), O(log n + k) |
| MW14 full replay (1991–2026) | ≤ 60 s, ≤ 1.5 GB (today 126 s, 3.1 GB) |
| R8L full replay (2000–2026) | ≤ 3 s, ≤ 0.6 GB |
| Storage per stored value | Norgate and Databento: ≤ 2 B resident on average with every precomputed structure, about 6.8 to 7.2 GB in all, locked (ST-9); other sources: ≤ 16 B resident, ≤ 4 B on disk after compression |
| Hot data layer + kernels + staging, peak | ≈ 14 GB in the `dedicated` profile, ≈ 11 GB in `workstation` (ST-9.2) |
| Restart to serving | ≤ 30 s (ST-8.8) |
| Full-minute futures bundle (47M bars) | resident on the reference machine |

---

## 8. Inputs still owed by the owner, and the defaults that stand

| Item | Default until supplied | Where it is used |
| --- | --- | --- |
| Vendor price lists (Databento per-GB and per-request, ORATS tier and rate limits) | cost vectors with symbolic unit prices `databento.gb`, `databento.req`, `orats.req`, resolved from a configuration file at start; a demand with money budget 0 never calls a vendor | D-7.3 |
| Futures margin source (CME performance-bond files, IBKR's published table, or a hand-maintained relation) | `margin(instrument, t, initial, maintenance)` is a relation the data layer may hold from any source; when absent the kernel uses the approximation of K-5.6 (model 5) and warns | K-5.6 |
| An options book with known results | gate G8 rests on hand-computed cases only | A-7.1 |
| S3 as a second store tier | the local store only; the tier interface of D-3.7 is specified so S3 is an adapter; S6 of the staging pipeline already keeps cold copies of versions in S3 | D-3.7, ST-7 |
| `exact` as the default numerics for new programs | `exact` for programs written after M-3; `two_pass` pinned for MW14 and R8L so G1 holds | ST-3, D-6.3, K-2.2 |
| How ORATS is held | not hot; demand-driven: the chains a run binds are loaded hot before it, the full history on NVMe in pcodec chain-day blocks | ST-11 |
| Retention | vendor objects kept in S3 indefinitely; the cold copy of every version a run, plan or claim names, at least 90 days; hot: current and pinned versions | ST-7.5 |
| The reference machine's memory profile | `workstation` (at most two kernels at a time, ≈ 11 GB peak) until a dedicated machine exists | ST-9.2 |

---

## 9. What is established, new and decided in this document

- [E] Conjunctive-query evaluation for demand resolution (V-3.12); Kleene conjunction for order-independent bodies (V-5); the causality theorem (F-7.1); BLAKE3 (O'Connor, Aumasson, Neves, Wilcox-O'Hearn 2020).
- [N] Reproduction across processes (4.2); mode invariance of decisions (4.3); replay equals live across the boundary (4.4); observation and queries change nothing across layers (4.6).
- [D] A-D1 to A-D56.
