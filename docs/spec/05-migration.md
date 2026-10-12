# 05 · Migration: the order of landing

Oct 12, 2026 · the sequence of changes that turns the present crate into the three programs, each step leaving the parity gates green. Tags and cross-references as in `00-architecture.md`. Line estimates are for Rust and tests, by analogy with the present sources (`docs/assessment.md` and the file sizes of this tree); they are planning figures, not commitments.

---

## 1. Principles

1. **Every step ends green.** G1 (MW14 and R8L to the bit) holds at the end of every step from M-0 to M-10; a step that cannot end green is split until it can.
2. **Interfaces before implementations.** `abt-wire` types and the service messages are written at M-0 and M-2 and change afterwards only by minor protocol versions (P-7).
3. **In-process first, socket second, three processes third.** Each layer is exercised in-process the moment it exists; the socket transport is the same frames (P-2.1), so the socket tests are a transport test, not a logic test.
4. **The old path is deleted when the new path is green**, never kept beside it (A-D34): two meanings of one command is the failure mode the second formulation was written against (V-1, R4).
5. **Estimates are for a developer who has read the six documents and the present sources**; the migration assumes one developer with occasional parallelism where noted.

---

## 2. Steps

### M-0 · Workspace

Convert the crate into a Cargo workspace: `crates/abt-wire`, `crates/abt-agg` (empty), `crates/abt-data` (empty), `crates/abt-kernel` (the present `src/` moved as is), `crates/abt-query` (empty), `crates/abt` (the present `src/bin/abt.rs`, calling `abt-kernel`). `abt-wire` gets the scalar types (P-1.1), `Hash` with BLAKE3, the canonical form (P-6) and the first message types (`Hello`, `Frame`, `Error`). CI runs the workspace.

| Files | Lines |
| --- | --- |
| `Cargo.toml` (workspace), `crates/*/Cargo.toml` | 80 |
| `crates/abt-wire/src/{lib, scalar, hash, canonical, frame}.rs` | 700 |
| `crates/abt-kernel/src/**` (moved, paths fixed) | 0 new |
| `tests/` moved under `crates/abt-kernel/tests/` | 0 new |

Gate: every present test green; G1 on `corpus/company`.

### M-1 · `abt-agg`

Move `RunningSums`, `Neumaier`, `RowsState`, `running_value`, `aggregate`, `sample_cov` and the order-statistics code out of `src/kernel/eval.rs` into `abt-agg` behind the API of P-1.2; the kernel calls it. Add `tests/agg.rs` with the 128-bit rational reference (a small exact-rational type in the test crate, no dependency) and the mode tolerances.

| Files | Lines |
| --- | --- |
| `crates/abt-agg/src/{lib, window, moments, order_stats, reduce}.rs` | 1,200 |
| `crates/abt-agg/tests/agg.rs` | 500 |
| `crates/abt-kernel/src/kernel/eval.rs` (deletions and calls) | −800 / +150 |

Gate: G1; `tests/fold_windows.rs` and `tests/window_order.rs` green; a before/after diff of every corpus run's decisions and fills is empty (translation validation, C-6.6, run by `scripts/tv.sh` from this step on).

### M-2 · The data layer, replay only

`abt-data`: the registry (schemas, identity, labels), the store (log, tail, partitions, manifest, versions, compaction, snapshot reads, cache objects, pinning, eviction), the query interface, the stream with explicit barriers and the k-way merge, the aggregate pass over `abt-agg`, `put`, `stats`, the service over the socket and the in-process channel, and two adapters: `bundle_v2` (the present bundle format and Parquet directories, so every existing bundle imports) and `synthetic` (the present generators, with A9 of `docs/assessment.md` fixed: drift and volatility bounded independently of `n`). The kernel is rewired: `Dataset` becomes a client view; `Store` becomes Σ_t's stored tables fed by the stream; the `Scheduler` is deleted; `EventLog::from_dataset` is deleted; truncation is `to_seq`; the batch driver is deleted (the fold is the only driver, and G5 is fold-against-truncated-stream). `abt run` takes `--data SOCKET|inproc` and a binding built from the present `environment` (a transitional resolver: every relation of the environment at its current version; deleted at M-3).

| Files | Lines |
| --- | --- |
| `crates/abt-data/src/registry/{schema, identity, labels, versions}.rs` | 1,400 |
| `crates/abt-data/src/store/{log, tail, partition, manifest, compact, snapshot, cache, gc}.rs` | 3,200 |
| `crates/abt-data/src/query/{forms, predicate, exec, explain}.rs` | 1,400 |
| `crates/abt-data/src/stream/{spec, clock, merge, subscribe, record}.rs` | 1,200 |
| `crates/abt-data/src/agg/pass.rs` | 500 |
| `crates/abt-data/src/adapters/{bundle_v2, synthetic}.rs` | 700 |
| `crates/abt-data/src/service/{socket, inproc, dispatch}.rs` + `abt-wire` messages for 3.1–3.3 | 1,300 |
| `crates/abt-data/tests/{store, stream, agg, query}.rs` | 1,800 |
| `crates/abt-kernel/src/kernel/{mod, fold}.rs` (rewire; deletions of `Dataset`, `Store`, `Scheduler`, the batch driver) | −1,900 / +900 |
| `crates/abt-kernel/tests/{fold, fold_availability, fold_checkpoint, asof, data, bundle, memo, store}.rs` rewritten over the service | 1,200 |

Gate: G1 (the books' bundles imported through `bundle_v2`); G5 in its new form; G3 in-process against socket, 1 against 8 data threads; D-9.1's budgets measured and recorded in `docs/assessment.md`. This is the largest step; M-3's parser and checker work can proceed in parallel from its second week since they touch neither the store nor the stream.

### M-3 · The second formulation in the kernel

V-11 steps 1 to 5, in the order given there, over the data service: blocks and the closed configuration (`execution` and `report` replace `ExecConfig`; WF-14); the demand built from the `data` block and the transitional resolver of M-2 extended to answer it from the registry (versions by `prefer latest|version`; D-7's cost model comes at M-6); calendars and bar views as pushdown aggregates (WF-16); sessions and the decision clock; universes, lists and vocabularies (WF-15, W9); order-independent bodies, `within`, `case`, keyedness (WF-11, WF-13), the plan chooser of V-5.2 with pushdown eligibility (K-4.5). `corpus-v2` becomes the corpus (its `require tested` lines become `require reviewed`, K-2.1); `tests/corpus_v2.rs` lands; `corpus/` stays only for G1 until the books are rewritten and compared (V-11 step 7), then is deleted at M-9. `scripts/parity/*` read `kernel.<run>.events` and `book` from the store (`abt data query … --parquet`) instead of `--dump`; `reference/diff.py` likewise.

| Files | Lines |
| --- | --- |
| `crates/abt-kernel/src/{lexer, parser, ir}.rs` (blocks, `within`, `case`, bracketed inputs, calendars, dynamic needs, combos) | 1,600 |
| `crates/abt-kernel/src/check/{mod, rule, blocks, demand, calendars, grouping, keyed}.rs` | 2,000 |
| `crates/abt-kernel/src/kernel/{eval, plan}.rs` (Kleene evaluation, plan chooser, pushdown) | 1,100 |
| `crates/abt-kernel/src/kernel/executor.rs` (configuration from the block; order types unchanged) | 400 |
| `crates/abt-kernel/tests/{corpus_v2, checker_v2, plan}.rs` | 1,300 |
| `scripts/parity/*.py`, `reference/diff.py` (read from the store) | 200 |

Gate: G2; G1 on `corpus-v2/company` (1,852/1,852; 21,892/21,892 and 6,850/6,850); G6; G5.

### M-4 · Contracts, state tables, the query service

K-3 (contract derivation, preconditions at bind, invariants at every barrier, postconditions at finish, limits pre-trade), the state tables with Δ and F (V-8.2), the query language served over the kernel service (V-8.3: `?`, `why`, recursive `why not`, `trace`, `coverage`, `show`, `ledger … into`), `observe` in the program, checkpoints as cache objects through the data layer, `abt contract`, `abt ir`, `abt hash` (BLAKE3). `abt explain` is retired into `query`. `tests/observe.rs` and `tests/explain.rs` are rewritten over the service.

| Files | Lines |
| --- | --- |
| `crates/abt-kernel/src/contract/{derive, pre, inv, post, limits, report}.rs` | 1,500 |
| `crates/abt-kernel/src/state/{sigma, provenance, failures, coverage}.rs` | 1,100 |
| `crates/abt-kernel/src/query/{parse, check, exec, why, whynot, trace}.rs` | 1,400 |
| `crates/abt-kernel/src/service/{socket, inproc, dispatch}.rs` + `abt-wire` messages for 3.4 | 900 |
| `crates/abt-kernel/src/kernel/checkpoint.rs` (over the data layer) | 300 |
| `crates/abt-kernel/tests/{contracts, queries, observe, checkpoint}.rs` | 1,400 |

Gate: G7; G5 (resume from every monthly checkpoint); the query tests (`query_every_bar_is_the_ledger` kept); G1.

### M-5 · The query layer

`abt-query`: the plan language (parser over the block syntax, normal form, hash, DAG, templates), the executor over the kernel service with parallel runs, trials and lineages as relations, hold-out by `to_seq` and block masks, reveals, walk-forwards, grids and surfaces; the metrics moved from `src/study/metrics.rs` and `validate.rs` and computed from the store's tables; the test framework and the starting set with the known-answer suite; claims; feed statistics; activation statistics; reports and the bias table; `abt plan|metrics|test|claim|feed|activation|lineage|dispute`. `src/study/` is deleted from the kernel crate; `briefs.rs` becomes a plan template (`study briefs_harness = …`) and a feed of the attempts directory.

| Files | Lines |
| --- | --- |
| `crates/abt-query/src/plan/{parse, normal, dag, exec, holdout, lineage, trials}.rs` | 2,200 |
| `crates/abt-query/src/metrics/{returns, trading, book, options, convention, surface}.rs` (moved and adapted) | 1,500 |
| `crates/abt-query/src/tests/{framework, registry, dependence, resample, regression, hac}.rs` | 1,400 |
| `crates/abt-query/src/tests/set/*.rs` (the starting set, 4.4) | 2,600 |
| `crates/abt-query/src/{claims, feed, activation, report, bias}.rs` | 1,600 |
| `crates/abt-query/src/service/*` + `abt-wire` messages for 3.5 | 700 |
| `crates/abt-query/tests/{plans, known_answers, claims, study_parity}.rs` | 1,800 |
| `crates/abt-kernel/src/study/**` deleted | −3,500 |

Gate: G9; study parity (`tests/study.rs`'s DSR, PBO, surface, WFE and sub-period values reproduced on the same fixed set of runs, now read from the store); G3 with a grid run across four kernel processes against one data process; G1.

### M-6 · The resolver, the vendors, live-sim

D-7: the source catalog, prices and quotas from a configuration file (A-8 placeholders), the cost model, the per-need cover, budgets, fetch → ingest → version → bind, pinning by plans, the pre-pass for dynamic demands, `abt data resolve --explain`; the adapters `norgate` (from `scripts/ingest/norgate_lake_to_inputs.py` and `src/ingest.rs`, now reading the lake directly through the Parquet reader), `databento` historical and live (DBN decoding; `r = ts_recv` or `ts_event + delay`), the live clock source with per-calendar grace, stream recording, `live-sim` mode in the kernel (the mode switch, the lag marks, the `late` policy), `--paced` replay. The transitional resolver of M-2/M-3 is deleted.

| Files | Lines |
| --- | --- |
| `crates/abt-data/src/resolver/{catalog, cost, cover, plan, fetch, bind, prepass}.rs` | 1,900 |
| `crates/abt-data/src/adapters/{norgate, databento, databento_live, dbn}.rs` | 2,200 |
| `crates/abt-data/src/stream/{live_clock, record}.rs` | 500 |
| `crates/abt-kernel/src/kernel/{mode, lag}.rs` | 400 |
| `crates/abt-data/tests/{resolver, adapters, live_replay}.rs` | 1,300 |
| `scripts/ingest/*` (retired into the adapters; `reconcile_report.py` becomes the `actions` feed statistic) | −700 |

Gate: G4 on a recorded synthetic feed and on one recorded Databento futures session; the S&P 500 point-in-time store rebuilt from the lake by `abt data ingest --adapter norgate` with the same row counts as `docs/handoff-data.md`; MW14 run from the rebuilt store (G1); resolver tests; a demand with money budget 0 never opens a vendor connection (asserted by a mock adapter).

### M-7 · Options

The registry's option relations and the `orats` adapter with the chain pre-pass; the kernel's option model (K-2.4 relations, K-2.5 functions, K-5.5 marks, fills, combos, exercise and assignment, adjustments, Greeks from data or model), the margin models of K-5.6 in preference order, the `limits` of K-3.5, the futures model of K-5.4 (daily settlement, rolls, expiry, published margins), the option metrics of Q-3.1, the chain feed statistic, `tests/options.rs` with the cases of K-9.3, the pricing known-answer tests.

| Files | Lines |
| --- | --- |
| `crates/abt-data/src/adapters/orats.rs`, `registry/options.rs` | 1,100 |
| `crates/abt-kernel/src/sim/{equity, future, option, combo, exercise, adjust, margin/{published, reg_t, portfolio, span, approx}, limits}.rs` (the present `executor.rs` split by instrument model) | 4,200 |
| `crates/abt-kernel/src/functions/{bsm, b76, american, iv}.rs` | 900 |
| `crates/abt-kernel/src/check/options.rs` (types for `Option`, `combo`, `exercise`, dynamic needs) | 400 |
| `crates/abt-query/src/metrics/options.rs`, `feed/chains.rs` | 500 |
| `crates/abt-kernel/tests/{options, futures, margin, limits, pricing}.rs` | 2,400 |

Gate: G8 (hand-computed cases; the owner's options book when supplied, A-8); G7 extended to limits; G1 unchanged (R8L's futures through the new futures model: 21,892/21,892).

### M-8 · Performance

A2 of `docs/assessment.md` (typed column blocks for Σ_t's stored tables), set-at-a-time evaluation of the cross-section between barriers (C-5; a second evaluator kept equal to the first by translation validation on every corpus program and both books), entity-parallel evaluation between barriers (F-5.3), pushdown wherever K-4.5 admits, the resident budget and mmap paths of D-3.2, the parallel aggregate pass (D-6.4). Benchmarks in `scripts/bench.sh` extended to the targets of A-7.3.

| Files | Lines |
| --- | --- |
| `crates/abt-kernel/src/state/columnar.rs`, `src/kernel/eval_set.rs`, `src/kernel/parallel.rs` | 2,800 |
| `crates/abt-data/src/store/{mmap, parallel_pass}.rs` | 600 |
| `scripts/bench.sh`, `scripts/tv.sh` | 150 |

Gate: G10; G1 and G3 under every thread count; translation validation between the two evaluators empty on the corpus and the books.

### M-9 · Retire v1

Delete `corpus/`, `corpus/env`, the v1 parser paths, `ExecConfig` and the 72 options, `abt study`, `abt explain`, `abt synth` (now `abt data ingest --adapter synthetic`), `abt bundle` (now `abt data ingest --adapter bundle_v2` for imports only), `src/dump.rs` (the store is the dump), `experiments/llm-usability` kept as a record with a note that its commands no longer exist. Rewrite `README.md` around the three programs and the six documents; update the status notes of `docs/data-bundle.md` and `docs/language-v2.md`; move `docs/observability.md` into K-7's commands. Rerun the usability and observability studies on the same tasks with the same models and record the results (A-7.2).

| Files | Lines |
| --- | --- |
| deletions across the kernel crate | −6,000 |
| `README.md`, docs status notes | 600 |

Gate: all of G1–G10 on the three-process deployment; CI runs the test suite in-process and over sockets.

### M-10 · The live interface freeze

`Broker` and `BrokerExecutor` (K-6.4) with a mock broker in tests; the `deployment` document parser (K-6.5); reconciliation (`halt`, `adopt`, `warn`), order lifecycle (new, partial, filled, cancelled, rejected, expired), the kill switch through `control`, `flatten_all` through decisions; the live-broker mode switch without a real adapter. No FIX (A-D27).

| Files | Lines |
| --- | --- |
| `crates/abt-kernel/src/live/{broker, executor, deployment, reconcile, lifecycle, control}.rs` | 1,600 |
| `crates/abt-kernel/tests/live_mock.rs` | 900 |

Gate: Proposition A-4.3 tested on the mock (decisions identical between `live-sim` and `live-broker` on the same recorded stream, fills differing as the mock dictates); reconciliation policies; lifecycle transitions; G1 unchanged.

---

## 3. Dependencies and parallelism

```
M-0 → M-1 → M-2 → M-3 → M-4 → M-5 → M-6 → M-7 → M-8 → M-9 → M-10
                 └──(parser/checker of M-3 from M-2's second week)──┘
                        M-5's test framework and known-answer suite can start at M-3 (they need only the store's tables)
                        M-7's pricing functions and margin formulas can start at M-4 (pure functions with known answers)
```

Totals: about 55,000 lines added across crates and tests, about 13,000 deleted; the present 28,500-line crate becomes roughly 70,000 lines across six crates, two thirds of which are tests, adapters and the starting set of statistical tests.

---

## 4. Risks and their mitigations

| Risk | Where | Mitigation |
| --- | --- | --- |
| The store's performance budgets are not met at M-2 | D-9.1 | the budgets are measured at M-2 and the layout decisions (time-major, offset index, tail) are the ones with the fewest unknowns; M-8 is where the remaining gap is closed, and no later step depends on the budgets |
| Pushdown breaks bit identity | K-4.5, D-6.5 | the equality test D-9.2 and `--no-pushdown` under G3 from M-3 on |
| The second formulation's books do not reproduce the first's | V-11 step 7 | the books are rewritten against `corpus/company` under translation validation; a difference is a defect in the rewrite or a documented accommodation of V-1, never accepted silently |
| The resolver buys data by mistake | D-7, A-D22 | the mock-adapter assertion at M-6; money budget 0 by default; `--dry-run` prints the plan |
| Options margin wrong against the broker's | K-5.6 | the conjecture of K-11 is tested on a live-sim account once M-10 and a broker adapter exist; until then `margin_req`'s source column makes every number traceable to its formula |
| Known-answer sources with ambiguous precision | Q-4.6 | each test states its tolerance and its source in the registry; a test without a known answer is not in the starting set |
