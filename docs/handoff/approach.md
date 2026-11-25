# The approach, as built

This document restates the design of the three-layer engine for a reader who has not read the specification (eight documents, about 500 KB, in `docs/spec/`). For each part it says what was specified, what exists in the code, and where the two differ. Line numbers are those of this working tree on Nov 24, 2026; if they have moved, search for the symbol.

## 1. The idea

A backtest is a program over market data. The old engine was one crate that loaded a bundle of files, evaluated the program and printed metrics. The split, specified on Oct 12 (`docs/spec/00-architecture.md`), separates three concerns that had grown into each other:

- **Holding data** and answering questions about it quickly, without knowing what the data means.
- **Evaluating a strategy** deterministically against that data and simulating its execution.
- **Judging the result** statistically and keeping count of how many times the question was asked.

Each concern became a program. They share only a protocol, so one data process can serve many kernels, and a study of hundreds of runs is a client of both.

Two things were meant to follow from the separation. The first is honesty: a kernel that never receives held-out data cannot leak it, and a query layer that owns the trial count cannot be bypassed. The second is speed: `docs/spec/06-performance.md` §3.3 calls the stream projection "the single largest effect of the split on the two books", because a data layer that holds weekly bars and computed aggregates lets the kernel stream a few million rows instead of every daily or minute bar. The first was delivered. The second was not, and section 10 says exactly what is missing.

## 2. The three layers

| Layer | Crate | Owns | Does not own | Source lines |
| --- | --- | --- | --- | --- |
| Data | `abt-data` | The schema and identity registries, the store, structured queries, aggregates over stored relations, event streams with clock barriers, the resolver, the vendor adapters, and the storage of every other layer's outputs | Any meaning: point-in-time, as-of, causality, hold-out | 22,245 |
| Kernel | `abt-kernel` | The language (parser, checker, normaliser, analyser, planner), derived contracts, the evaluator, the fold over the stream, the book, market simulation for equities, futures and options, margin, the query language over a run's state | Metrics, statistics, plans, trial counts, vendors, storage | 55,436 |
| Query | `abt-query` | Plans (studies), lineages and trial counting, metrics, statistical tests, claims, reports | Program evaluation, the book, storage | 15,558 |

Three shared crates carry what must be identical on both sides of a boundary:

| Crate | What it is | Source lines |
| --- | --- | --- |
| `abt-wire` | The message types, the frame format, the hash. No logic beyond serialisation | 2,136 |
| `abt-agg` | One implementation of every aggregate (sums, moments, windows, order statistics), so the data layer and the kernel give bit-identical answers (decision A-D8) | 1,293 |
| `abt-codec` | The hot store's block format, as pure functions | 2,188 |

`abt` (379 lines) is the front: it links the three layer crates over an in-process transport and dispatches subcommands. `abt-tests` holds the cross-layer suites (17,724 lines of tests).

The kernel links only `abt-agg` and `abt-wire`. It cannot read the store's files; everything it knows about data arrives through the protocol. This boundary is real in the code, and it is the reason a change to what the kernel receives (package 2 of the roadmap) is a protocol change, not a function call.

## 3. The protocol

Specified in `docs/spec/04-protocol.md`; built as specified.

- **Transport.** A Unix domain socket (mode 0600) or an in-process channel pair that carries the same frames. No TCP. Inside `abt` the default is in-process; `ABT_TRANSPORT=socket` switches (`crates/abt/src/lib.rs:25`).
- **Frame.** `u32 length | u8 kind | u64 correlation | payload`. Control messages are FlatBuffers (built with `planus` from `crates/abt-wire/schema/abt.fbs`). Tables travel as Arrow IPC.
- **Hashes.** BLAKE3 over RFC 8785 canonical JSON.
- **The kernel's calls on the data layer**, all in `DataClient` (`crates/abt-kernel/src/kernel/bind.rs:39`): resolve a demand into a binding, read schemas and statistics, query, get, put (the run's outputs), request an aggregate, and subscribe to a stream described by a `StreamSpec`.

Each layer has a binary with a `serve` mode: `abt data serve`, `abt kernel serve --data SOCKET`, `abt query serve (--data SOCKET | --store DIR) [--kernel SOCKET... | --kernels N]`.

## 4. Identity and determinism

A run is identified by the hash of four things (`docs/spec/00-architecture.md` §4 and §6):

- `H(P)`: the hash of the program's normal form. The whole execution configuration is inside the program, so it is inside the hash.
- `β`: the binding, the list of (relation, version) pairs the resolver pinned, plus the parameter point.
- `π`: the plan step that asked for the run, or for a run outside a plan, its window and truncation.
- the run mode.

Versions are immutable, the stream order is canonical (receipt time, relation, identity, sequence number), and time advances in the kernel only when a `Barrier` event arrives. Two runs with the same program and binding therefore produce byte-identical outputs, whatever the transport, the thread count or the number of kernels sharing a data process. This is tested (section 9), and every performance change must keep it.

## 5. One run, traced

`abt run --strategy mw14 --data S.sock --history window corpus-v2/catalog corpus-v2/lib corpus-v2/company/mw14.dsl`, step by step:

1. **Parse and check.** The kernel's command line (`crates/abt-kernel/src/cli.rs`) parses the files, checks them and links them. Linking generates the bar-view rules: a program that reads `close@1w` over a daily base gets a rule of the form `view(A, T, V) :- V = agg(X) over (T1 in bucket(C, T), prim(A, T1, X))` (`crates/abt-kernel/src/check/link.rs:1016`).
2. **Compile.** Normalise (which gives `H(P)`), analyse, rewrite, plan, lower (`compiler/`, `analysis/`, `rewrite/`, `plan/`, `lower/` under `crates/abt-kernel/src/`). The result is a plan applied on top of the interpreter: a literal order per rule, rules skipped, windows shared, constants folded. There is no code generation.
3. **Connect.** `data_client` (`cli.rs:139`) connects to the socket, or starts an in-process data service over a store directory.
4. **Bind.** `replay_observed` (`crates/abt-kernel/src/kernel/mod.rs:4428`) drives the rest. `demand_of` (`bind.rs:1361`) turns the program's `data` block into a demand; the data layer's resolver answers with the binding.
5. **Decide what streams.** `stream_spec` (`bind.rs:1621`) lists every bound primitive that `Kernel::read_relations` (`mod.rs:3324`) keeps. `push_down` (`bind.rs:1181`) sends the aggregates that qualify (`kernel/pushdown.rs`) to the data layer.
6. **Fold.** The kernel subscribes. Each tuple goes through `Fold::step_inner` (`kernel/fold.rs:362`) into `insert_fact` (`mod.rs:3191`), which appends it to the relation's `Store`. Each barrier closes a bar: the simulated executor fills and marks, and on a decision bar the kernel evaluates the decision rules.
7. **Evaluate.** A decision rule demands derived relations through `call` (`kernel/eval.rs:461`). A memo miss runs `derive_rules` (`eval.rs:668`), which solves each rule body with `solve_kleene_block` (`eval.rs:958`), which calls `step` (`eval.rs:1609`) for each literal of each row.
8. **Forget.** After the barrier, `evict_memo` (`mod.rs:3235`) drops memo buckets behind the horizon and `trim_stores` (`mod.rs:3273`) drops stored rows no reader can reach.
9. **Write and report.** The event log, the book, coverage and ledgers are put to the data layer as `kernel.<run>.*` relations (`kernel/outputs.rs`). The query layer's metrics over the book are printed.

On MW14 this path stores and later trims 59.2M rows, to decide at 1,904 weekly bars over 4,240 names.

## 6. The data layer

**Specified** (`docs/spec/01-data.md`, `docs/spec/07-storage.md`). Typed relations with two timestamps per tuple: event time and receipt time, both nanoseconds. Immutable versions. Eight query forms. Event streams with explicit barriers. A resolver that finds the cheapest cover of a demand, with a money budget that defaults to zero. For Norgate and Databento bars, a "hot" store: integer ticks in blocks of 1,024 values, memory-mapped and locked, with every program-independent structure precomputed, including a calendar ladder (5m, 15m, 30m, 1h, 1d, 1w, 1mo levels of every bar relation).

**Built** (`crates/abt-data/src/`).

| Part | Where | State |
| --- | --- | --- |
| Registry, Parquet partitions, versions | `store.rs` | Built. Holds every non-bar relation and the other layers' outputs |
| Hot store: blocks, segments, versions | `store_hot.rs`, `hot/segment.rs`, `hot/version.rs`, `abt-codec` | Built. Content-addressed segment files, decoded bit for bit in tests |
| Appends and consolidation | `hot/append.rs`, `hot/consolidate.rs` | Built, with a deviation: appended rows live in tails beside the immutable blocks, not in place (A-D51) |
| The calendar ladder | `hot/ladder.rs` | Built and verified on every ingest. **Read by nothing** (below) |
| Staging pipeline | `staging/` | Built, except its first stage (S0, landing the vendor's objects) and its last (S6, the cold export) |
| Query forms, aggregates | `query.rs`, `agg.rs` | Built over decoded rows. The sparse tables and wavelet matrices that would answer ranges without decoding are not behind them |
| Stream, service | `stream.rs`, `service.rs` | Built |
| Resolver | `resolver/` | Built. Among the stored versions that satisfy a need it binds the coarsest (`resolver/plan.rs:320`), so a daily program binds daily bars when a daily version was ingested separately |
| Adapters | `adapters/` | Norgate (the NDLake Parquet files), Databento historical and live, ORATS, plus `bundle_v2`, `synthetic`, `market`, `mock` |

**The gaps that matter for the goal.**

- **The ladder has no reader.** `Store::read_level` (`crates/abt-data/src/store_hot.rs:600`) is called only from tests. A weekly or daily view of a finer base is computed by the kernel from the base rows, or by the data layer's aggregate pass walking the base rows. ERRATA E-89 records it.
- **A pushed-down aggregate does not stop its input from streaming.** The kernel keeps a fallback that evaluates the aggregate locally, and the fallback needs the rows (ERRATA E-127).
- **One binding has no memory bound.** A query or a subscription on a hot relation can map far more than it returns (ERRATA E-120; 15 to 21 GB observed on the pipeline machine).
- **Opening a store replays its log.** A tail-heavy store takes 34 to 70 s and gigabytes to open (ERRATA E-93). A fix for one case sits uncommitted in the amendments tree (`status.md`, "Loose ends").
- **A stream has no start.** A `StreamSpec` names relations, calendars and instruments, and no start time (`crates/abt-wire/schema/abt.fbs`), so a run over six months receives each bound version from its first row.

The first two are the data path's share of MW14's time. The third and fourth are why the pipeline's data services hold tens of gigabytes. The fifth, with the second, is why the pipeline's short checks on minute data are killed (`status.md` section 8).

## 7. The kernel

### The language

The second formulation (`docs/language-v2.md`). A program is nine typed blocks: `data`, `calendars`, `clock`, `universe`, `params`, `rules`, `decisions`, `execution`, `report`. The `data` block is a demand, which is what the resolver binds. Rule bodies are order-independent. The `execution` block replaces the old engine's 72 command-line options. `corpus-v2/` is the acceptance corpus: 24 strategies, 35 negative cases, 8 libraries, 5 catalogs and the two company books, `corpus-v2/company/mw14.dsl` and `corpus-v2/company/r8l.dsl`.

### The evaluator

This is where the time goes, so it is described as it is.

**Storage.** A stored relation is a `Store` (`kernel/mod.rs:1832`): a `BTreeMap` from the time key to a `Rows` block (`kernel/rows.rs:12`), row-major, each cell a 16-byte tagged `Value` (`kernel/value.rs:195`), sorted by entity on first indexed lookup. Entities are already interned as `u32` symbols, but nothing is laid out as a column or indexed by that number.

**The memo.** Derived relations are computed on demand and remembered in `Memo` (`mod.rs:48`): per time key, a hash map from `(relation, Vec<Value>)` to the result. The key is heap-allocated on every call, hit or miss.

**Two evaluators.** The tuple evaluator, `solve_kleene` (`eval.rs:904`), carries one environment at a time. The set evaluator, `solve_kleene_block` (`eval.rs:958`), carries a `Block` of environments (`kernel/eval_set.rs:47`) and is the default. Despite its name it still calls `step` once per row for every literal, with three exceptions added on Nov 6:

- `merge_join_atom` (`eval.rs:1126`): a positive atom over at least 32 rows in entity order at one bar is joined in one pass.
- `prefetch_rows_window` (`eval.rs:2533`) and `prefetch_bucket_window` (`eval.rs:2211`): a window's inner conjunction is solved once for the whole block at the bar. The loop over the block's rows remains; each row's `step` reads the prefetched result.

`--evaluator tuple` selects the old one. The two are held equal by `crates/abt-tests/tests/eval_set.rs`.

**Windows.** A rolling aggregate over rows keeps a `RowsState` per group (`eval.rs:314`): the window's last N rows, each a `Vec<Value>`, found through a hash map keyed by the group's values (`kernel/mod.rs:2273`). Under `numerics running` it also carries `RunningSums` (`eval.rs:351`), built on `abt-agg`'s `ShiftedSums`, which advance in O(1). Both books run under `numerics two_pass`, where the value at each bar is a reduction over the rows held. `abt-agg` has window types of its own, `WindowState` for time extents and `RowsWindow` for `rows(N)` (`crates/abt-agg/src/window.rs:62`, `window.rs:122`); only the data layer uses them (`crates/abt-data/src/agg.rs:604`).

**Parallel evaluation** (ERRATA E-125). `solve_block_maybe_parallel` (`kernel/parallel.rs:180`) runs a rule's first literal on the main thread, cuts the rows by entity across worker kernels, and merges the results in the single thread's order. `warm_strata` (`parallel.rs:142`) derives the inputs first so that no worker derives a whole relation alone. `--threads N`, default 1.

**Retention** (ERRATA E-126). `trim_stores` drops every stored block older than the oldest bar any reader can reach. `--history window` is the default of `abt run`; `--history all` keeps everything and is forced for ledgers and live runs. This is what let the full MW14 finish in 3.7 GB instead of being killed past 8.5 GB. It is `abt run`'s only: the switch is set in the command line (`cli.rs:586`) and nowhere else, so a run made through the kernel service, which is every run of `abt plan run`, keeps the whole history (`kernel/service.rs:122`; `status.md` section 5).

**Stream projection, first step** (ERRATA E-127). `read_relations` drops `open`, `high`, `low`, `ticker` and `classification` from the stream when the run provably never reads them.

### The simulator and the book

`crates/abt-kernel/src/sim/` holds the executor and the instrument models (equity, future, option) with their corporate actions, rolls, exercise and margin. The kernel owns the book (positions, cash, NAV, margin, working orders) as a state table that queries read like any relation. Three modes share one evaluator: `replay-sim`, `live-sim` (a live feed with simulated fills) and `live-broker` (interface only; no broker adapter exists).

## 8. The query layer

Built as specified (`docs/spec/03-query.md`; `crates/abt-query/src/plan/`, `metrics/`, `tests/`).

- A **plan** is a declared graph of runs, grids, walk-forwards, reveals, tests and claims, with a normal form and a hash.
- Every run a plan makes is a **counted trial** of its lineage. A program joins a lineage when its rule set is at least 0.8 similar (Jaccard).
- A **hold-out** (`trailing:20%`, `after TIME`) is enforced by truncating the stream at a sequence number, so the kernel never receives the held-out bars. A reveal is logged and is itself a trial.
- A **claim** is a statistical statement with a test, an expectation and a level. The deflated Sharpe ratio uses the lineage's full trial count.
- The pipeline's house presets, `futures_house` and `equities_house`, are in `corpus-v2/lib/execution.dsl`.

The kernel computes no statistic and the query layer evaluates no program.

## 9. How correctness is held

The next phase replaces the evaluator. These are the instruments that will say whether the replacement is right.

| Instrument | Where | What it holds |
| --- | --- | --- |
| Translation validation between evaluators | `crates/abt-tests/tests/eval_set.rs` | Tuple evaluator = set evaluator = set evaluator on 2 and 4 workers, decisions and ledgers, on every corpus program and both books |
| Retention | `crates/abt-tests/tests/retention.rs` | A trimmed run equals the whole-history run |
| Golden renderings | `crates/abt-tests/tests/tv_golden.rs`, `scripts/tv.sh` | Every corpus program and both books rendered exactly |
| Determinism (gate G3) | `crates/abt-tests/tests/determinism.rs` | In process against sockets, 1 against 8 data threads, 1 against 4 kernels |
| Truncation and resume (G5) | `crates/abt-tests/tests/fold.rs` and its siblings | A run stopped at a barrier equals a run over the truncated stream |
| Reference differential (G6) | `crates/abt/tests/reference.rs`, `reference/engine.py` | Fills, book and NAV agree with an independent pandas executor |
| Mutation patches | `scripts/mutations/`, `scripts/mutate.sh` | About 50 patches, each breaking one feature; each must fail its test |
| Parity with the firm's books (G1) | `scripts/parity/mw14_compare.py`, `scripts/parity/r8l_compare.py` | MW14's 1,852 weekly returns to 8e-15; R8L's 21,892 legs |

Two cautions. The first three instruments compare the engine with itself. They catch a new evaluator that differs from the old one, but not an adapter that stages different data. Only G1 catches that, and G1 does not hold on the new data path today. And the suites run on small synthetic markets, so a change can pass all of them and still be slow, or run out of memory, on the real book. The benchmark of the roadmap's package 1 is the check at full size.

## 10. The performance design, against what exists

`docs/spec/06-performance.md` §3 describes the kernel that would meet the targets. Its own estimate for MW14 is 6 to 10 s. This table is the gap, item by item.

The spec's word for an entity's dense index within a run is "slot". The code also uses "slot" for a variable's position in a rule's environment (`atom_slots`), and the pipeline for one of its four memory-capped run slots. This handoff says "entity id" for the first.

| Design | In the code |
| --- | --- |
| Stored tables as one chunk per bar of typed columns (`Vec<f64>`, entity ids as `Vec<u32>`) | `Store` of row-major `Rows` of tagged `Value`s. Trimmed behind the horizon, not re-laid as chunks |
| Derived blocks memoised by (relation id, bar index): an array index | `Memo`, a hash map keyed by `(usize, Vec<Value>)`, allocated per call |
| A bitmap per bar for the universe and closed relations; negation as `andnot` | Not built. Negation is tested per row |
| Sorted-merge joins on entity ids | `merge_join_atom`, for one shape of literal |
| Tests and assignments vectorised over a block's columns | Not built. `step` per row |
| Rolling aggregates as `abt-agg` window states per (rule, literal), indexed by entity id, advanced once a bar | Partly. A `RowsState` per group holds the window's rows, with O(1) sums under `numerics running` only. It is found by a hash of the group's values, holds `Vec<Value>` rows, and is not `abt-agg`'s type. Two prefetch paths batch the inner solve |
| An index for `asof` on derived relations | `asof_memo` (`mod.rs:2209`), a hash map whose key omits the entity (a known hazard, listed in PR #80) |
| A bump arena reset at every barrier | Not built. The allocator is 14% of the Nov 6 profile |
| The executor's book as vectors over entity ids | Not built. Fills and marks are 54 s of the 367 s |
| Entity-local strata in parallel over fixed ranges of entity ids with a persistent pool | Built one level down: per rule application, threads spawned per step. No wall-clock gain |
| The stream carries only relations read outside pushed-down aggregates, and only the columns read (§3.3) | First step only. Primitives inside pushed aggregates still stream; columns are not projected |
| Bar views read the ladder | Not built |

Everything in the left column except the last row and the stream row is one piece of work, the column-block evaluator. It is package 3 of the roadmap. The last two rows are package 2.

## 11. Reading the specification

| Prefix | Document |
| --- | --- |
| `A-D1` to `A-D56`, `A-n` | `docs/spec/00-architecture.md`: decisions, invariants, gates G1 to G10, the targets of §7.3 |
| `D-` | `docs/spec/01-data.md` |
| `K-` | `docs/spec/02-kernel.md` |
| `Q-` | `docs/spec/03-query.md` |
| `P-` | `docs/spec/04-protocol.md` |
| `M-0` to `M-10` | `docs/spec/05-migration.md`: the build steps |
| `X-` | `docs/spec/06-performance.md` |
| `ST-` | `docs/spec/07-storage.md` |
| `E-1` to `E-127` | `docs/spec/ERRATA.md`: every deviation, with its reason and the rejected alternative |
| `V-`, `F-`, `S-`, `C-` | `docs/language-v2.md`, `docs/formal-foundations.md`, `docs/semantic-model.md`, `docs/compiler-design.md` |

`docs/spec/TRACE.md` has one row per decision, proposition, gate and target, with the code, the test and a status. `docs/assessment.md` records every performance measurement. `docs/iterations/01-foundation.md` is the record of the iteration that built the hot store, and holds the owner's cadence rules.
