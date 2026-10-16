# absolute-backtest

A strategy language for backtesting, implemented as
[`docs/language-v2.md`](docs/language-v2.md) fixes it (the second
formulation; its grammar is `corpus-v2/README.md`): programs written as
typed blocks, an elaborator that links a program against a class catalog and
a standard library, a checker with one diagnostic code per judgment,
compiler stages from the typed IR to an executable plan, and a kernel that
evaluates a checked program bar by bar in a closed loop with a simulated
executor whose every setting is a field of the program's `execution` block.
One Rust crate; its dependencies are the Parquet and JSON libraries the
bundle format needs.

```
abt check corpus-v2/catalog corpus-v2/lib corpus-v2/strategies      # every strategy, against the catalog and the library
abt run --strategy sma_crossover --synthetic --days 200 corpus-v2/catalog corpus-v2/lib corpus-v2/strategies/sma_crossover.dsl
abt run --strategy sma_crossover --data ./market --verify-causality corpus-v2/catalog corpus-v2/lib corpus-v2/strategies/sma_crossover.dsl
abt run --strategy sma_crossover --synthetic --all --fills corpus-v2/catalog corpus-v2/lib corpus-v2/strategies/sma_crossover.dsl
abt run --strategy sma_crossover --synthetic --param fast=30d --start 2022-06-01 --end 2022-09-30 corpus-v2/catalog corpus-v2/lib corpus-v2/strategies/sma_crossover.dsl
abt query --strategy sma_crossover --text 'trace decide at 2022-04-07' --text 'why not above(E3, 2022-04-07)' --synthetic --days 200 corpus-v2/catalog corpus-v2/lib corpus-v2/strategies/sma_crossover.dsl
abt query --strategy sma_crossover --text '? above(A, T), close(A, T, P), P > 50 USD/share at T = 2022-04-07' --synthetic --days 200 corpus-v2/catalog corpus-v2/lib corpus-v2/strategies/sma_crossover.dsl
abt synth --class equity --out ./market corpus-v2/catalog corpus-v2/lib   # write a synthetic market as Parquet
```

Data is one Parquet file per primitive relation (`close.parquet`,
`listed.parquet`, ...), with columns named after the signature's arguments,
in a loose directory (`--data DIR`) or in a bundle (`--bundle DIR`, below).
Timestamps are bar close instants: a 09:30 to 09:31 minute bar is labelled
`09:31`, the last bar of a session `16:00`, and a daily bar by its date.
`abt synth` writes this convention; minute data labelled by open time
silently misaligns every bucket.

## Layout

| Path | What it is |
| --- | --- |
| `docs/language-v2.md` | The language: one data store per instrument class with every resolution derived (section 3), programs as typed blocks with no run-time flags (4), order-independent rule bodies with sound short-circuiting (5), what the semantics licenses the kernel to do (6), the standard library (7), the query facility with its state tables and algorithms (8), MW14 and R8L rewritten in it (9), the checker's judgments (10) and the implementation order (11). The code cites it by section. |
| `corpus-v2/README.md` | The grammar the corpus assumes: the nine blocks, the `data` block's needs, universes, calendars, signatures, the literal forms, decisions, the kernel relations, literals with units, the corpus headers and the diagnostic codes. |
| `docs/semantic-model.md` | The first formulation's model. What `docs/language-v2.md` section 10 retains unchanged (WF-3 types, WF-5 closedness, WF-6 causality, WF-7 total orders, WF-8 stratification, W5 and W6, the kernel's semantics of its section 7) is specified there; its syntax, environments and corpus are gone. |
| `docs/formal-foundations.md` | The formal foundations: the language as stratified Datalog over the cross-section and synchronous dataflow over time, with every guarantee derived from established results. |
| `docs/data-bundle.md` | The data bundle and validation program: closed data, the online fold kernel, the catalog, the bias audit, the study API. |
| `docs/observability.md` | The observability commands: `abt show` and `abt graph` (the program graph, with the compiler's facts), the coverage report every run prints, `abt query` (the query language: `?`, `why`, `why not`, `trace`, `coverage`, `show`, `ledger ... into`), `--ledger`, the state tables (`abt table`, `--tables DIR`), and the compiler's `ir`, `rewrite-log`, `plan` and `verify`. |
| `docs/compiler-design.md` | The compiler design: the pipeline from the typed IR to an executable plan (Normalise, Analyse, Rewrite, Plan, Lower), the invariants that make translation validation exact, the verification tiers and the observability tables; the kernel of the three-layer split follows it (`docs/spec/02-kernel.md`, section 8, lists the deltas). |
| `docs/spec/` | The specification of the three-layer split (Oct 12, 2026): `00-architecture.md` (the three programs, the decisions record A-D1 to A-D34, cross-layer invariants, the gates G1 to G10), `01-data.md` (the store, query engine, stream and resolver), `02-kernel.md` (verifier, evaluator, contracts derived from the program, market simulation for equities, futures and options, the three modes), `03-query.md` (plans, metrics, the test framework, claims, reports), `04-protocol.md` (the shared crates and the wire protocol), `05-migration.md` (the order of landing, M-0 to M-10, each step gated by the parity runs), `06-performance.md` (the algorithms and data structures of each layer and how the targets are reached). The implementation lands step by step on this branch; `docs/spec/ERRATA.md` records where it settles what the documents leave open. |
| `docs/llm-observability.md`, `docs/llm-usability.md` | The two studies of models writing programs with the toolchain, both on the first formulation; their material is under `experiments/`. |
| `Cargo.toml`, `crates/` | The workspace of `docs/spec/05-migration.md` (M-0): `abt-wire` (the shared types and messages: the FlatBuffers schema `schema/abt.fbs` compiled by planus, frames, RFC 8785 canonical forms, BLAKE3 hashes), `abt-agg` (the aggregate kernels), `abt-data` (the data layer), `abt-kernel` (the language, its compiler and the kernel: every `crates/abt-kernel/src/` row below), `abt-query` (the query layer: the metrics, computed from a run's tables in the store, `abt metrics`; the test framework; plans, `abt plan`, `abt lineage`, `abt dispute`), `abt` (the front binary) and `abt-tests` (the suites that span layers, with the shared fixtures). The layers fill in step by step as the migration lands. |
| `crates/abt-kernel/src/syntax/` | The front end: `lexer.rs`, `ast.rs` (units are strategies, libraries, classes and execution presets; a strategy is nine blocks) and `parser.rs`. The parser never reorders literals. |
| `crates/abt-kernel/src/ir.rs` | The core IR of a linked program: domains, dimensional types, calendars, signatures, rules and the literal forms of sections 3 to 5. |
| `crates/abt-kernel/src/check/` | The elaborator and checker. `link.rs` links a strategy against the class catalog and its libraries, instantiates the library templates at every call (bracketed inputs and calendars substituted at link time), and generates the bar views, the per-calendar universes, the instruments table, the series and catalog reads and the clock; `rule.rs` is the per-rule pass (a well-moded order, B and M; T; F; D; G; W7); `config.rs` expands the `execution` block (fifteen fields, from a preset and its overrides) and the `report` block (P); `types.rs` is the dimensional algebra; `dof.rs` counts degrees of freedom (W5, W6, W9); `mod.rs` holds the workspace, the codes, the program-level judgments and the strata. |
| `crates/abt-kernel/src/compiler/` | Normalise (the normal form, canonical order and names, the program hash H(P): BLAKE3 over the RFC 8785 form of the normal form without its `report` block, what `abt hash` prints and `abt hash --show` prints the bytes of) and the stage printing behind `abt ir`; `compile` runs the pipeline. |
| `crates/abt-kernel/src/analysis/` | Analyse: strata, the open-loop partition, keyedness labels, demand, window lengths, and the abstract interpretation with its warnings. |
| `crates/abt-kernel/src/rewrite/` | The rewrites R1 to R6 with the rewrite log (`abt rewrite-log`). |
| `crates/abt-kernel/src/plan/`, `crates/abt-kernel/src/lower/` | The planner (statistics, cost model, exact order search, plan printing) and the lowering the kernel executes (`abt show plan`). |
| `crates/abt-kernel/src/verify/` | The verification tiers: exact linear feasibility, the SMT-LIB2 driver with replay files, constrained Horn clauses, translation validation, the runtime checks (`abt verify`). |
| `crates/abt-kernel/src/state/` | The state tables and the provenance of a run: extension, carried, summary, derivation, failure, coverage, plan, book, binding; derivation trees and why-not (`abt table`, `--tables`, and the query language's `why` and `why not`). |
| `crates/abt-kernel/src/observe.rs` | The program graph and the coverage report behind `abt show`, `abt graph` and the run's coverage lines. |
| `crates/abt-kernel/src/kernel/` | The kernel: `eval.rs` solves rule bodies top-down with memoisation under the three-valued reading of section 5; `executor.rs` is the executor behind a trait (`SimExecutor` is the simulated one with every realism model); `mod.rs` holds the kernel, the batch driver, `ExecConfig::from_program` (the configuration the `execution`, `decisions` and `report` blocks fix), `explain` and the empirical causality check; `fold.rs` is the same evaluation driven by an availability-ordered event stream with barriers, and its checkpoints; `time.rs` is calendar arithmetic and buckets; `value.rs` the runtime values. |
| `crates/abt-kernel/src/data.rs` | The Parquet loader for a loose data directory, and the deterministic synthetic markets of the three classes: `synthetic_daily` (equities with a split, dividends, a ticker change, a delisting, membership, a classification and the `SPXTR` and `VIX` series), `synthetic_minute` (minute equities) and `synthetic_futures` (minute futures with sessions, point values and commissions); `synthetic_for_program` picks the one a program's class and base ask for. |
| `crates/abt-kernel/src/table.rs` | Parquet tables: the one file format of every input and output outside a bundle's log. |
| `crates/abt-kernel/src/bundle.rs`, `crates/abt-kernel/src/ingest.rs` | The bundle format (manifest, security table, monthly partitions, bundle tests) and the vendor adapters behind `abt bundle build --from-norgate` and `--from-databento`. |
| `crates/abt-kernel/src/dump.rs`, `crates/abt-kernel/src/study/` | `--dump DIR` (what `reference/` replays) and the study API (lineages, hold-outs, the trial log, the metrics, the report, the briefs harness). |
| `crates/abt-kernel/src/cli.rs`, `crates/abt/src/main.rs` | The command line, and `abt`, the front binary that dispatches to it (the layers' own command lines join it as they land). |
| `corpus-v2/catalog/` | The three classes a program is written against: `equity` (bars as traded at a daily base; `listed`, `ticker`, `split`, `dividend`, `delisted`, `member`, `classification`; the vocabularies of lists, reasons and schemes), `future` (bars per root at a minute base, `session`, `point_value`, `commission`, `member`, `usable`) and `series` (`value` by name, each on its native calendar). A program never names them; its `data` block names what it needs. |
| `corpus-v2/lib/` | The standard library of section 7, one module a file: `calendar` (`bar`, `prev_row`, `first_row`, `row_number`), `catalog` (`split_factor`, `cum_split`, `adj_close`, `dividend_at`, `dividend_or_zero`, `total_return`, `log_return`, `tr_index`), `stat` (`mean_rows[N, R]` and the other rolling and recursive moments), `bars` (`true_range`, `range`, `gap_points`, `gap`, `price_return`, `log_price_return`, `dollar_volume`, `adv[N]`, `liquid[Floor]`), `xsec` (`decile[R]`, `zscore_xs[R]`, `share_above[R, Th]`), `state` (`latch[enter, stay]`, `bars_held`, `entry_price`, `high_since_entry`), `book` (`held`, `short`, `in_book`, `flat`, `weight_now`, `exposure`, `nav_return`, `peak_nav`, `drawdown`, `nav_vol[N]`, `bought_within[W]`), and `execution.dsl`, the presets `equity_default`, `book_fixed_base` and `futures_fixed_base`. |
| `corpus-v2/strategies/` | 24 strategies that must check clean, among them the four stylized-fact strategies, `dividend_capture`, `opening_gap` on a minute base, `resampled_momentum`, `total_return_momentum` over the catalog module and `scale_in`, whose rules aggregate their own past. |
| `corpus-v2/company/` | MW14 (`mw14.dsl`, the weekly S&P 1500 momentum book) and R8L (`r8l.dsl`, the intraday futures opening-range book) as section 9 gives them. |
| `corpus-v2/negative/` | 35 negative cases, one or more per judgment code; each file's `# expect:` header is asserted by `crates/abt-tests/tests/corpus_v2.rs`. |
| `crates/abt-tests/tests/corpus_v2.rs` | The acceptance test: the library checks clean; every strategy and company program checks clean modulo its `# allow:` lines; every negative case yields exactly its code and no program; every program runs on synthetic data of its class and decides something. |
| `crates/abt-tests/src/lib.rs`, `crates/abt-tests/tests/fixtures.rs` | The shared fixtures of the integration tests: the catalog and the library as the base workspace, strategies checked into programs, a strategy skeleton the tests fill in (`StrategyParts`, `strategy(name, params, rules, decides)`), crafted markets (`market`, `markets`, `market_from`); and the test that the skeleton checks and runs. |
| `crates/abt-tests/tests/*.rs`, `crates/abt/tests/*.rs` | The integration suites (those that drive the `abt` binary are the front's): the checker and its messages, the kernel's hand-computed outcomes, the executor's contracts, costs, liquidity, margin and policies, orders, actions, as-of joins, the fold with its windows, checkpoints and availability, the state tables, the compiler stages, translation validation and verification, the study, the command line. They are being ported from the first formulation's programs to this one. |
| `reference/`, `briefs/`, `experiments/` | The pandas reference engine, the plain-language briefs, and the material of the two model studies. |
| `scripts/` | `ingest/norgate_lake_to_inputs.py` writes the Norgate-style inputs of `abt bundle build --from-norgate` from the NDLake lake; `parity/` holds the comparisons of the two books with their references; `bench.sh` is the performance ladder. `ingest/ndlake_weekly.py`, `ingest/tradestation_sessions.py` and `synth/` write the first formulation's layouts and are kept for the record. |

## The surface syntax in one page

A workspace is a set of `.dsl` files holding units: the classes of the
catalog, libraries, execution presets and strategies. A strategy is nine
blocks and a `uses` line:

```
strategy sma_crossover {
  data {
    class   equity
    need    bars @1d
    need    listing
    require reviewed
    prefer  latest
  }
  uses  calendar, stat, book
  clock { decide on base }
  universe {
    universe(A, T) :- listed(A, T), traded@base(A, T).
  }

  params {
    fast     : Duration = 20d in 10d..60d
    slow     : Duration = 50d in 30d..250d
    fast_min : Count = 12
    slow_min : Count = 30
    qty      : Quantity<Shares> = 100 shares
  }

  rules {
    rel above(A: Equity, @T)
    above(A, T) :- mean_window[fast, fast_min, close](A, T, F), mean_window[slow, slow_min, close](A, T, S), F > S.
    rel below(A: Equity, @T)
    below(A, T) :- mean_window[fast, fast_min, close](A, T, F), mean_window[slow, slow_min, close](A, T, S), F <= S.
  }

  decisions {
    silence hold
    orders  market
    decide(T, trade(A, qty)) :- above(A, T), prev(T, T0), below(A, T0), flat(A, T).
    decide(T, trade(A, Q)) :- below(A, T), prev(T, T0), above(A, T0), held(A, T, Q0), Q = -Q0.
  }

  execution equity_default
  report { calendar base   periods 252 per year }
}
```

A library is a `data` block, a `uses` line when it reads other modules, and
a `rules` block; its relations are templates, instantiated at every call:

```
library stat {
  data { class any }
  uses calendar
  rules {
    rel mean_window[W: Duration, K: Count, R: rel(Entity, @T, D)](A: Entity, @T, M: D)
    mean_window[W, K, R](A, T, M) :- universe(A, T), M = mean(X) over (T1 in window[W] min K, R(A, T1, X)).
  }
}
```

- Variables start with an uppercase letter; parameters, relations and
  keywords are lowercase; `_` is a wildcard; `#` starts a comment.
- **`data`** is the demand (section 3.6): the instrument class the program
  trades and what it reads. `need bars @r [from DATE] [complete]` is open,
  high, low, close and volume at resolution `r` or finer, from the date on;
  `need actions` (split, dividend, delisted), `need listing` (listed,
  ticker), `need lists "L", ...` (member), `need series "S" @r
  [complete]`, `need sessions`, `need point_value`, `need commission`,
  `need instruments where ATTR = VALUE`, `need chain of "U" within
  D1..D2 and M1..M2` (options). `require reviewed` or `allow unreviewed`
  (the versions' `reviewed` attribute, warned when admitted without it);
  `prefer latest`, `version V` or `base finest`; an optional `budget {
  money X; bytes Y; requests Z }`. With `complete` the versions must be
  `reviewed = complete`: a missing bar means the instrument did not trade and the bar primitives
  are closed; without it a missing bar is unknown, so they may not be
  negated, directly or through a relation that reads them (N). A rule that
  reads a primitive the demand did not ask for is an E error; a label or
  series name no `need` covers is a V error. A library's `data` block says
  what the module needs (`class any` is written for every class, with
  `Entity` as the entity sort and `units` as the unit).
- **`universe`** defines the entity domain by rules for `universe(A, @T)`,
  reading the catalog (`listed(A, T), member(A, T, "SP500")`) or an
  `instruments { ES from 2000-01-01; ... }` table written in the block
  (counted as degrees of freedom and warned W9 as a hindsight constant).
  It is complete by definition, so the calendar module's `prev_row`,
  `first_row` and `bar` are closed and may be negated.
- **`calendars`** declares the calendars the program uses: `base` always
  exists; `days`, `weeks(n)`, `months(n)`, `years(n)`, `sessions` (per
  instrument) and `sessions of "ROOT"`. **`clock`** names the decision
  calendar, `decide on base`, `decide on weekly`, or `decide on base at
  session + 30min` for a sub-daily base. A relation is on the decision
  calendar unless its signature writes another after its key (`@T:
  weekly`); `@X: base | session` instantiates the rule on each. The bar
  view of a price primitive at a calendar is `close@C`, `open@C`,
  `high@C`, `low@C`, `volume@C` or `traded@C`; at `base` it is the
  primitive itself, and `traded@base(A, T)` says that A has a bar at T. A
  library relation is instantiated at the calendar of the key it is called
  with (`true_range@session`). A series or a catalog relation on a
  calendar coarser than the rule's is read as of the bar.
- **Signatures**: `rel name[INPUT: TYPE, ...](A: Equity, @T[: CALENDAR],
  X: TYPE, ...)`. The identity is the entity arguments and the key;
  bracketed inputs are bound by the caller and may be relations (`R:
  rel(Entity, @T, D)`); value arguments are outputs. An uppercase type name
  not otherwise declared (`D`) is a dimension variable, so `mean_window`
  serves prices, returns and dollar volumes alike. There are no binding
  modes.
- **Body literals**, in any order: an atom; `not atom` over a closed
  relation; a comparison; `X = expr`; `X = agg(e) over (conj) [within
  (vars)]` with `agg` one of sum, mean, std, var, median, quantile, max,
  min, count, corr, cov, ols_slope, ols_r, first, last; `K = rank(R(...) by
  (k dir, ...)) within (vars)` and `rank_average`; `top[k](R(...) by (...))
  within (vars)`; `X = case { G -> e ; ... [; else -> e] }`; the as-of join
  `R(..., T0, ...) asof T`; the builtins `prev(T, T1)`, `lag(T, DUR, T1)`,
  `month_start(T)`, `day_start(T)`, `session_of(A, T, S)`. A temporal
  constraint inside a group is `T1 in rows[N] [min K]` (the entity's own
  last N rows, all N unless `min K`), `T1 in prior_rows[N] [min K]`, `T1 in
  window[DUR] min K`, `T1 in prior_window[DUR] min K`, `T1 in bucket(C,
  T)`, or `T1 in session(X, [a, b))`. The group of a reduction, and of an
  aggregation that names one, is `within (vars)`; every variable of the
  reduced atom outside it must be fresh (G). Functions: abs, least,
  greatest, log, exp, sqrt, sign, floor, ceil, clamp, decile. `observe R
  [where ...] [trace relations | instances]` in the `rules` block adds a
  relation to every bar's demand without changing a decision.
- **Literals** carry units: `100 shares`, `2 contracts`, `0 units` (the
  class's unit), `60 USD/share`, `0 USD/contract`, `1_000_000 USD`, `20d`,
  `3mo`, `1y`, `30min`, `0.02`, `"SPX"` (a label), `"AAA"` (an instrument,
  warned W6 as a snapshot of the bundle date). A bare number is a Scalar,
  or a Count where a Count is expected; a dimensioned zero is written typed
  (`0 USD/share`, `0 shares`).
- **`decisions`**: one algebra, no mode. `hold(A, W)` with W a Scalar is a
  target weight of the capital the execution block names; `hold(A, Q)`
  with Q a quantity is a target quantity; `trade(A, Q)` is a signed delta;
  `combo([leg(A1, Q1), leg(A2, Q2), ...])` is an all-or-none multi-leg
  delta and `exercise(A, Q)` an option exercised (both checked; executed
  from the options model on). Each takes an optional trailing order (`market`, `moo`, `moc`,
  `moo_moc`, `limit(P[, TIF])`, `stop(P[, TIF])`), the block's `orders`
  when omitted. `silence hold` keeps an instrument with no decision;
  `silence flat` closes it. A sale of a held quantity is a negative delta
  bound in the body: `held(A, T, Q0), ..., Q = -Q0`. Two decisions for one
  instrument at one bar halt the run naming both rules.
- The kernel supplies, at the decision calendar, `position(A, @T, Q)`,
  `cash(@T, C)`, `nav(@T, N)`, `capital(@T, C)` (the base the execution
  block's `capital` names), `mark(A, @T, P)` (the price it names, as the
  book is marked), `fill(A, @T, Q, P)` and `decided(@T0, D)`; the `book`
  module's views of them are closed, so `not held(A, T, _)` is legal.
- **`execution`** is a preset name (`equity_default`), a preset with
  overrides (`book_fixed_base with { commission 10 bps per side }`) or the
  fifteen fields written out: `price`, `orders`, `lot`, `capital`,
  `commission`, `slippage`, `impact`, `participation`, `financing`,
  `leverage`, `actions`, `delisting`, `oversize`, `ruin`, `numerics`. Every
  field is required (P); their values are in `corpus-v2/lib/execution.dsl`
  and under Reading the kernel. An optional `limits { gross <= 1.5 reject;
  weight <= 0.1 per name clamp; ... }` field holds the pre-trade limits of
  K-3.5 (`gross`, `net`, `weight ... per name`, `loss ... per day`,
  `drawdown`, `turnover ... per day|week|month|quarter|year`,
  `participation`, `names`, each `halt`, `reject` (the default), `clamp`
  or `warn` as the kind admits). **`report`** names the reporting calendar,
  `periods N per year`, and optionally `window DATE .. DATE`, the run's
  window; every bar before it is warm-up.
- A corpus file's headers: `# expect: CODE` on a negative case, `# allow:
  Wn` on a strategy that is meant to warn.

`abt check` judges every strategy, and every library whether or not a
strategy uses it: a library is instantiated in a probe that calls each of
its relations once, with a parameter of its type for every value input and
a stub of its shape for every relation input, and its own diagnostics are
reported under its name. It prints each strategy's degrees of freedom (the
counts the study report uses) and a summary line:

```
sma_crossover: degrees of freedom: 5 params, 0 literals, 5 rules (+ 0 library literals)
1 strategies, 7 libraries checked: 0 error(s), 0 warning(s)
```

Every diagnostic names its code and judgment, for example:

```
error [N] bad_negation_incomplete_derived at 29:49 in rule bad_negation_incomplete_derived::decide#2:
  `not selected` is not permitted: `selected` reads `regime_on` reads `close` is not closed:
  `close` is a primitive of class `equity` the demand does not require complete, so a missing
  tuple is unknown, not false (add `complete` to `need bars` when the store is reviewed, so
  that a missing bar means the instrument did not trade) (WF-5 completeness)
```

## Reading the checker

| Code | Judgment | What the rule checks |
| --- | --- | --- |
| U | name resolution | every relation, parameter, library and class named is declared; no builtin, keyword or reserved name as a relation name (`not month_start(T)` does not resolve); the strategy is in the workspace |
| E | demand | a rule reads only primitives the `data` block needed |
| B | WF-1 range restriction | every head variable is bound by the body |
| M | WF-2 well-modedness | every bracketed input is bound at the call, and the body has a well-moded order (Proposition 4.2); the diagnostic names the variable no literal can bind |
| T | WF-3 types | dimensions balance; terms match signatures; decision terms are typed; a parameter's default lies within its range; a comparison of a dimensioned quantity with a bare `0` names the typed zero to write |
| R | WF-4 temporal recursion | every positive cycle steps strictly back in time (`prev`, `prev_row`, `lag`) |
| N | WF-5 completeness | `not R` only when R is closed: a complete primitive, the universe, a reduction, or a relation built from closed ones |
| F | WF-6 causality | every temporal key is T or derived from T by a causal builtin or constraint (`prev`, `lag`, windows, rows, buckets, `session(X, [a, b))`, `asof`); `decided` strictly earlier; a session-calendar relation is not read at its own session from a base bar |
| D | WF-7 determinism | a `top` or `rank` has `by`, whose keys cover every identity column |
| S | WF-8 stratification | no cycle through `not` or an aggregate |
| Z | WF-9 decisions | at least one decide rule |
| X | WF-16 calendars | every calendar named is declared or a class property; bucket aggregates go coarser; `@T: C` on a head is a declared calendar; the executor's relations are read at the decision calendar, not with a session label; a series is read in the direction its calendar allows |
| K | WF-11 keyedness | every temporally recursive relation is keyed: its rules' guards are complementary (a case form, or `enter` against `not enter`) |
| G | WF-13 grouping | `rank` and `top` name `within`; every variable of the reduced atom outside the group is fresh |
| P | WF-14 closed configuration | every block present; every field of `execution` and `report` set with an admissible value; a preset expanded |
| V | WF-15 vocabulary | every label and series name a rule uses occurs in a `need`, and in the class's vocabulary; every `instruments` name is known |
| Q | WF-17 demand | the `data` block is a well-formed demand with at least one `need` |
| O | observation | every `observe` names a relation of the program; a `where` binds identity arguments only |
| W1, W2, W3, W4 | dead rule, unused parameter, undefined relation, degenerate reduction | a derived relation decide never reaches; a parameter never referenced; a declared relation no rule defines (always empty); a reduction whose group holds every identity of its atom, so it ranks one tuple |
| W5, W6 | degrees of freedom, ticker snapshot | a numeric literal inside a strategy's own rule is a degree of freedom the study counts (a library's literals are counted, never warned); an instrument literal names the security carrying that ticker at the bundle date |
| W7, W8, W9 | window sanity, never fired, hindsight list | a calendar window's `min K` exceeds what its duration can hold on the rule's calendar (judged on the defaults, and again by the run on its `--param` overrides); a relation on a path to decide never fired (reported by the run's coverage report); an `instruments` table is counted and warned |

Diagnostics are ordered errors before warnings, whole-unit diagnostics (a
missing block, a library or class not in the workspace, no decide rule)
before any rule's, then by the dependency rank of the rule's head, so the
first error reported is the earliest offending relation, then by position;
duplicates are dropped, so `abt check` on a workspace reports a library's
warning once and not once per strategy that uses it.

## Reading the kernel

The kernel reads its data through the data layer (`docs/spec`, M-2). Bind
sends the program's demand, receives the binding, and subscribes to a stream
of the bound relations' tuples in receipt order with explicit barriers of
the program's calendars. The fold (`kernel::run`) is `state' = step(state,
event)` over that stream: a tuple is stored at its `t` (late when its
decision bucket was already decided), a barrier opens its bucket, and a
barrier of the decision calendar runs the executor's fill of the previous
bar's orders (after the bar's corporate actions, so an ex-date's orders fill
against the post-split book), the bar's open (delistings, mark, margin), the
strategy's decisions and the executor's take. Because every temporal
builtin looks back only, a time domain that grows as buckets open is
observationally the up-front one. The fold's state is serialisable
(`kernel::Checkpoint`, with the stream's seq as its cursor):
`--checkpoint-every month|N` takes one at the end of every calendar month
(or every N bars). Over `--data SOCKET` it is a cache object of the data
layer, keyed by the run's id and the seq and printed (`checkpoint KEY of
DATE`), and the run writes its outputs so far before it; in process it is
`DIR/<bar>.json` under `--checkpoint-dir DIR`. `--resume KEY|FILE`
continues from one. The checkpoint's program hash, binding, mode, run id
and configuration must be the run's, and a resume that differs is refused
naming which (ERRATA E-56). A run is identified by BLAKE3 over its program
hash, its binding (the data versions and the parameter point), its window
and the mode, 64 hex digits. The executor
is a trait (`kernel::Executor`), so a live run attaches another executor to
the same fold.

A tuple carries its receipt time `r` (`Dataset::add_available`; an
`available_at` column in a loose Parquet file or in a bundle's partitions):
a decision at a barrier sees the tuples received by the barrier's instant,
so a tuple that arrived after its bar closed is late in the backtest too. A
run truncated at a decision is the stream's `to_seq` at that barrier, and
`verify_causality` (`--verify-causality`) re-runs truncated streams for
sampled bars and compares `decide(t)`, printing `causality verified at 5
sampled bars`.

Evaluation is top-down: `decide(t, D)` is requested for each bar `t` of the
decision calendar, and every derived relation is requested with its key and
inputs bound and memoised by them. A library relation is a template
instantiated at link time, so `mean_window[fast, fast_min, close]` and
`mean_window[slow, slow_min, close]` are two relations with their own
memo, and the coverage report lists them as such. Because WF-4 makes all
positive recursion strictly time-decreasing and WF-8 keeps negation and
aggregation acyclic, every request terminates. A rule body is read under
Kleene's three-valued conjunction (section 5): a false literal excludes the
instance whatever the rest, so a guard may be written anywhere in the rule
and the planner moves tests ahead of generators (the run prints `plan:
optimised (2 of 10 rules reordered, ...)`; `--plan written` keeps the
written order); an instance stuck on a partial operation (`x / 0`, `log`
of a non-positive, `sqrt` of a negative, a statistic below its minimum
count) halts the run naming the rule, the tuple and the expression only
when a decision demands it. A windowed aggregation keeps, per group, the
rows of every bar it has solved and evicts bars that leave the window, so a
rolling feature solves each bar once (`ExecConfig::window_cache` turns the
cache off for the tests that prove it changes nothing).

The executor is configured by the program's `execution` block and nothing
else (`ExecConfig::from_program`; `docs/language-v2.md`, 4.3). Field by
field: `price` names the primitive or derived `Price` relation it fills and
marks at, with a calendar instance when written (`close`,
`tr_index@weekly`); `orders` the order of a decision that names none;
`lot whole` truncates every order's quantity toward zero, `lot fractional`
does not; `capital N USD fixed` sizes a weight from the fixed starting
capital and reports returns on that base, `compounding` from equity;
`commission` is `per_share P min M`, `per_contract from catalog` (the
catalog's commission per contract per side), `X bps per side` or `none`;
`slippage` is `fixed X bps`, `vol X over N bars` (X times the instrument's
realized volatility over the last N bars, against the order) or `none`;
`impact sqrt X over N days adv` moves the fill price against the order by
X times the square root of the filled quantity over the mean volume of the
last N bars, or `none`; `participation X of bar volume` caps a fill at that
fraction of the bar's volume, or `none`; `financing cash X debit Y short Z`
accrues annual rates on positive cash, on a debit and on short notional,
or `none` (nothing accrues and every name is shortable at no fee);
`leverage` is `gross <= X halt|reject` (a fill beyond X times equity
halts, or is rejected with a reason), `reg_t` (2x gross, 25% maintenance,
orders beyond rejected) or `unconstrained`; `actions apply` applies splits,
dividends and delistings to the book, `in_price` says the price relation
already carries them; `delisting last_price` closes a delisted holding at
its last price, `haircut { bankruptcy 1, acquisition 0, ..., other 1 }` at
the last trade less the haircut for the reason; `oversize halt|clamp|allow`
rules on a `trade` that would cross zero; `ruin halt|continue` on an
equity that is not positive while orders are pending; `numerics running`
or `two_pass` selects the aggregate implementation. There is no
regulatory-fee field, so the costs line's `fees` is always zero. A field
whose value turns a model off is reported on every run as the bias it
leaves unmodeled (`warning (cash-management): cash earns nothing (the
execution block's financing credits no cash rate)`), by reading the block.
A run also warns when the data has corporate actions (`split`, `dividend`,
`delisted`) and the program's data block does not `need actions`: the
executor cannot apply what it does not load, so a split would move the
price and not the position, and a delisted name would never be closed.
The numbers of the semantic model's conservative defaults are the
`equity_default` preset.

Every program has a contract derived from its structure (K-3): the
preconditions the binding must meet before the first barrier (a binding,
positive prices, a derived state within `--memory-budget`, the reconciled
actions, delistings and memberships the bundle tests used to check), the
invariants the kernel checks on itself at every decision barrier (the NAV
identity, one decision per instrument, no read past the barrier, keyed
stored relations, the leverage field, the limits, the event log folding to
the book, one working order per instrument), the postconditions computed at
the end, and the limits of the `limits` field, which act on a bar's orders
before they are sent. `abt contract --strategy NAME files...` prints it,
each clause with its source, discharge and policy; with data it runs the
program and prints each clause's status (`proven`, `checked`, `warned`,
`not_applicable`); a refused precondition stops the run with exit 2, a
failed invariant halts it naming the clause, the bar and the values.

A run over a data service (`abt run --data SOCKET`) writes its outputs to
the service's store under `kernel.<run>.*` and prints the run's id (K-7.1):
`events` (the event log), `book` (the book at every decision barrier),
`holdings`, `decisions` and `fills` (projections of the log), `coverage`,
`contract` (the clauses with the run's statuses), `binding` (the versions
read, the program's hash and normal form, the parameter point),
`plans`, `late`, `report` (the status, `completed` or `halted(reason)`, the
counters and the warnings), and `ledger.R` for each `observe R`. A halted
run writes what it has. `abt data query --store DIR 'kernel.<run>.book'`
prints one, and `--parquet FILE` writes it for pandas: the parity scripts
read the books' runs that way (ERRATA E-52).

The executor fills a bar's decisions at the next bar's close, at the price
relation adjusted by the cost model, then writes `fill`, `position` and
`cash` at that bar and `decided` at the decision bar. Within a bar, orders
that reduce a position fill first, so a rebalance funds its buys with its
sells. A `hold` is the difference between the target and the position at
execution, sized from the block's capital at the price it will fill at;
under `silence hold` an instrument with no decision keeps its position and
an exit is an explicit `hold(A, 0)`. A capped `hold` re-issues itself at
each following bar until it is reached or a new decision on the instrument
supersedes it; a capped `trade`'s remainder expires. Corporate actions act
on the book at the bar's open, before the mark: a split multiplies the
position at its ex-date, a dividend's receivable is recorded at the ex-date
and paid at the pay date, a delisted name is force-closed at its last trade
less the haircut for the reason. The NAV is cash, the marked positions and
the receivables. Every change to the book is an event of the run's log
(`RunResult.events`: decisions, orders with their ids, fills, drops,
replacements, expiries, actions, funding, margin calls, each with the cash
it moved), and the log folds to the book bit for bit; the decisions, fills,
drops and actions lists are projections of it. A decision the executor
cannot carry out is dropped with a reason in `RunResult.dropped` (`no price
for E3 at 2022-05-04`) and is still recorded in `decided`; `abt run` prints
`decisions: 11   fills: 9   dropped: 2` and then each dropped decision with
its reason. `--all` prints every decision instead of the first twenty,
`--fills` prints the fills (`fill 2022-02-24 E3 +100 @ 69.5578`), and
`--quiet` prints the summary only. After the metrics, every run prints the
coverage report: one line per relation `decide` reaches with the bars it
was demanded at, the bars it held a tuple at and the tuples it derived,
then the primitives and executor relations with their counts; when
`decide` derived nothing, the report says so and names the first empty
relation on each path to it (W8). A plain run also warns that it is an
untracked trial that nothing counts (see Studies).

Parameters are the only values a run may vary: `--param name=value`
(repeatable; `unit::name` for a library's) replaces a default with a
literal in the DSL's grammar (`30d`, `50 shares`, `0.02`, `"SPY"`). An
override must be of the parameter's declared type and within its declared
range, or the run is refused before it starts (`parameter `fast` of
`sma_crossover` = 5d is outside its range 10d..60d`).

A decision the executor cannot fill is not retried by the kernel; whether
the strategy retries it depends on how the rule is written. A point test on
history, `lag(T, hold, T0), decided(T0, trade(A, _))`, is one-shot: `lag`
lands on at most one bar per entry and never for an entry whose `T0 +
hold` is not a bar. A test of the current state is retried every bar,
which is why the corpus writes every time-based exit over the `book`
module's `bought_within[W]`, a count of the strategy's own buy decisions in
the prior window:

```
decide(T, trade(A, Q)) :- held(A, T, Q0), not bought_within[hold](A, T), Q = -Q0.
```

This sells at the first bar strictly later than `hold` after the entry,
closes every entry, and fires again if a fill was dropped.

`abt query --text QUERY` runs the program and asks the query language of
`docs/language-v2.md`, section 8, over it (repeat `--text` to ask several
of one run):

- `? BODY at T = DATE` is a rule body, checked with the program as any rule
  is (a stratum above it, so it changes no decision), and answered with its
  solutions at the bar. `at T` asks at every bar of the run, `over D1 ..
  D2` over a window, and `for A = KEY` binds a variable to one instrument.
  A one-atom query of a derived relation names the rule of each row.
- `why R(...)` is a tuple's derivation down to the supplied tuples.
- `why not R(A, T)` names, for each rule, the literal that excluded the
  instance, with the bindings at that point, in the plan's order or `in
  written order` (`sma_crossover::above#1 excluded at literal 3 `F > S` with
  A = E1, F = 43.49, S = 43.95, T = 2022-04-07`).
- `trace decide at DATE` lists every instance attempted at the bar: fired,
  or the literal that excluded it.
- `coverage over D1 .. D2` names the first empty relation of every path to
  `decide`, with its `why not`.
- `show program|plan R|schedule|calendars|contract` prints the program's
  forms.
- `ledger R into NAME` writes R's extension over the run through the data
  service as `kernel.<run>.NAME`.

A bar must be one of the queried relation's calendar.

The kernel is also a service (`docs/spec/04-protocol.md`, P-3.4):
`abt kernel serve --data SOCKET` reaches a data service.
`abt kernel run --strategy NAME <files...>` starts a run, which writes its
outputs to the data service's store and prints a progress line every
`--progress N` barriers. `abt kernel query --run RUN --text QUERY` asks the
run a query: between barriers while it runs, over its final state after.
`--checkpoint-every month|N` writes checkpoints through the data layer and
prints each key, `--resume KEY` runs from one, and `abt kernel control
--run RUN checkpoint` takes one while the run runs and prints its key.
`abt kernel status`, `control RUN halt`, `check`, `contract` and `show`
complete the commands (`--kernel SOCKET` names the service; ERRATA E-54,
E-56).

The query layer reads a run back from the store (`docs/spec/03-query.md`).
`abt metrics --run RUN --data SOCKET` (or `--store DIR`) computes the run's
metrics from the tables it wrote. These are the return metrics of its NAV
curve, its trading metrics, its capacity, the metrics per period of its
report block, and the Sharpe ratio of each calendar year, each printed
with the versions of the tables it read (ERRATA E-58).

Plans are studies written in the language's block syntax
(`docs/spec/03-query.md`, section 2): a program, a parameter grid, a
hold-out, a walk-forward scheme, an objective and the steps. `abt plan
check PLAN` prints the plan's normal form, its hash H(Π), its steps in order
and the trials it will count. `abt plan run` asks the kernels for every run
with its step's identity π, spreads a grid over them, truncates every run
before a trailing hold-out and masks a block hold-out out of the metrics and
the ledgers. It writes every run as a trial row of `query.trials`, the
plan's outcomes under `query.<hash>.outcomes` and the plan, which pins its
bindings, in `query.plans`. `abt plan reveal PLAN STEP` runs one committed
point over the whole sample and reports the embargoed bars; `abt plan
report` reads the outcomes back, and `abt plan drop` unpins. `abt lineage`
counts a lineage's trials, every run ever made of its members, and `abt
dispute` records a dispute of an attachment (ERRATA E-59).

## The command line

```
abt check <files...>
abt run --strategy NAME (--data DIR|SOCKET | --bundle DIR | --synthetic [--days N] [--symbols A,B,C] [--seed N])
        [--start DATE] [--end DATE] [--as-of DATE] [--param NAME=VALUE]... [--plan written|optimised]
        [--verify-causality] [--no-pushdown] [--all] [--fills] [--nav FILE] [--returns FILE] [--dump DIR] [--tables DIR] [--ledger R1,R2,... --ledger-out DIR] [--study DIR] [--quiet] [--timing] <files...>
abt run ... [--checkpoint-every month|N [--checkpoint-dir DIR]] [--resume KEY|FILE]
abt hash --strategy NAME [--json | --show] <files...>
abt show --strategy NAME <files...>
abt graph --strategy NAME <files...>
abt ir --strategy NAME [--stage parsed|checked|normalised|analysed|rewritten|planned] [--json] <files...>
abt rewrite-log --strategy NAME [--json] <files...>
abt show plan --strategy NAME [--observed] [--json] [--plan written|optimised] [(--data DIR | --bundle DIR | --synthetic ...)] <files...>
abt verify --strategy NAME [--tier N] [--solver z3|cvc5] [--replay-dir DIR] [--json] [(--data DIR | --bundle DIR | --synthetic ...)] <files...>
abt table --strategy NAME --table coverage|plan|book|binding|summary|extension|carried|derivation|failure [--rel R] [--at T] [--symbol SYM] [--out FILE] [--json] (--data DIR | --bundle DIR | --synthetic ...) <files...>
abt query --strategy NAME --text QUERY [--text QUERY]... [--json] [--out FILE] (--data DIR|SOCKET | --bundle DIR | --synthetic ...) <files...>
abt metrics --run RUN (--data SOCKET | --store DIR) [--json]
abt plan check|run|report|drop PLAN [SOURCES...] [--study NAME] [--threads N] [--dry-run] [--json] (--data SOCKET | --store DIR) [--kernel SOCKET... | --kernels N]
abt plan reveal PLAN STEP [SOURCES...] [--study NAME] [--json] (--data SOCKET | --store DIR) [--kernel SOCKET... | --kernels N]
abt lineage PROGRAM [SOURCES...] [--json] (--data SOCKET | --store DIR)
abt dispute PROGRAM --reason TEXT [SOURCES...] (--data SOCKET | --store DIR)
abt kernel serve [--socket PATH] --data SOCKET
abt kernel run|check|contract [--kernel SOCKET] --strategy NAME [--param NAME=VALUE]... [--start DATE] [--end DATE] [--progress N] [--checkpoint-every month|N] [--resume KEY] <files...>
abt kernel query|show|status|control [--kernel SOCKET] --run RUN [--text QUERY | --what FORM | halt | checkpoint]
abt synth --class CLASS --out DIR [--days N] [--symbols A,B,C] [--seed N] <files...>
abt bundle build (--from DIR | --from-norgate DIR | --from-databento DIR [--processing-delay S] | --synthetic ...) --class CLASS [--base @1m] --version V --out DIR <files...>
abt bundle test DIR <files...>
abt study declare --study DIR --strategy NAME [--holdout none|trailing:Ny|blocks:K:Nmo[:SEED]] [--objective METRIC] [--require METRIC>=X]... <files...>
abt study run --study DIR --strategy NAME (--data DIR | --synthetic ... | --bundle DIR) [--grid NAME=V1,V2,...]... [--walk-forward SCHEME] [--param NAME=VALUE]... [--threads N] <files...>
abt study reveal --study DIR --strategy NAME (--data DIR | --synthetic ... | --bundle DIR) <files...>
abt study metrics --study DIR [--strategy NAME] <files...>
abt study report --study DIR --strategy NAME <files...>
abt study dispute --study DIR --strategy NAME --reason TEXT <files...>
abt briefs report --briefs DIR --attempts DIR [--out FILE] <files...>
```

The executor is the program's `execution` block and the reporting its
`report` block: no option sets a cost, a policy, a price relation or the
periods. `abt` with no arguments prints the usage with every option
explained. `show`, `graph`, the coverage report every `run` prints,
`query`, `--ledger`, the state tables, `why-not`, `materialise`, `explain
--derivation` and the compiler's `ir`, `rewrite-log`, `plan` and `verify`
are described in `docs/observability.md`; the compiler itself in
`docs/compiler-design.md`.

`<files...>` are `.dsl` files or directories searched recursively; a run
needs the catalog, the library modules the program uses and the program
(`corpus-v2/catalog corpus-v2/lib corpus-v2/strategies/sma_crossover.dsl`).
The synthetic market is the one of the program's class and base, seeded
(`--seed`, default 7) and deterministic, over 500 days (40 for a sub-daily
base) and the symbols `AAA,BBB,CCC,DDD,SPY`; its equities are securities
`E1`, `E2`, ... carrying those tickers, which is why a decision reads
`trade(E3, 100)` and `--bind A=E3` names the instrument. The daily market
changes its second name's ticker halfway and delists its last name at four
fifths of the sample, so a strategy that names `"SPY"` runs with `--symbols
AAA,XYZ,BBB,SPY,DDD`, as the acceptance test does. `--start DATE` and
`--end DATE` bound the run as the report block's `window` does: the
executor runs and the strategy decides only at bars within them, and
earlier bars stay data. `--as-of DATE` is the bundle date a ticker literal
or a command-line name resolves at (default: the data's last bar).
`--nav FILE` writes the book at every bar as Parquet (`t, equity, cash,
gross, net, leverage`), `--returns FILE` the returns per reporting period
(`t, ret, pnl`), `--ledger R1,R2 --ledger-out DIR` the named relations'
tuples at every bar, `--tables DIR` every state table as Arrow IPC, and
`--dump DIR` what the reference engine replays. `--timing` prints one
stderr line with the load, build and run times, the bar and tuple counts
and the kernel's statistics, among them `pushed_answers`, the windows the
data layer's aggregate pass answered. An aggregate over one stored relation
in one of the pass's window forms is pushed down to the data layer, which
computes it over the whole run in one request; `abt show plan` prints, under
each aggregate, `pushdown: ...` or `local:` and the condition that keeps it
in the kernel (`docs/spec/ERRATA.md`, E-47). `--no-pushdown` evaluates every
aggregate in the kernel, a test switch: both give the same bits.

Every option value is validated: `--days abc`, `--symbols ""` or `--at
yesterday` are errors naming the option, never a silent default, and an
unknown `--option` prints the usage. Exit codes: 0 success; 1 the strategy
does not check, the run was refused or halted, or the usage is wrong; 2 an
input could not be read (a source file, a data directory, a bundle that
has not passed its tests).

## Bundles

`docs/data-bundle.md` (sections 2 and 3) makes data a versioned bundle the
system ships. A bundle is a directory: `manifest.json` (the class, the
version, the bundle date, every relation with its signature, resolution,
availability convention, row count and partitions, and the bundle tests'
report once they pass), `securities.parquet` (the security table:
`id,ticker,from,to`, `to` null while the ticker is still carried, and
optionally the contract columns `multiplier`, `asset_class` and
`commission_per_contract`) and `log/<relation>/<YYYY-MM>.parquet`,
append-only Parquet partitions of each primitive relation by the month of
its temporal key. With a security table every equity field is a security
id and the `ticker` relation is derived from the table; a ticker literal in
a program or on the command line resolves through it to the security
carrying that ticker at the bundle date. A loose directory (`--data DIR`)
is the same files without the log: one `<relation>.parquet` per primitive
of the program's class, a relation being a function of its identity
columns (two rows for one identity with different outputs are an error
naming both rows; an identical row is dropped), a missing or empty file
leaving the relation empty with a `note:`.

```
abt bundle build --synthetic --class equity --version 2026.10 --out ./bundle corpus-v2/catalog corpus-v2/lib
abt bundle build --from ./market --class equity --version 2026.10 --out ./bundle corpus-v2/catalog corpus-v2/lib
abt bundle test ./bundle corpus-v2/catalog corpus-v2/lib
abt run --strategy sma_crossover --bundle ./bundle corpus-v2/catalog corpus-v2/lib corpus-v2/strategies/sma_crossover.dsl
```

`abt bundle test` runs the six bundle tests the data can answer (identity;
every temporal key a bar label; positive prices; action reconciliation,
where a close ratio outside [0.6, 1.67] between consecutive bars must be a
split that day and every split must show one; delisting coverage, where a
name that leaves the universe before the last bar has a `delisted` record;
membership, where a member is in the universe that bar) and records a pass
in the manifest; a bundle that passed is `reviewed = complete` in the data
layer. A program's `data` block says `require reviewed`, so `abt run
--bundle` refuses a bundle that has not passed (exit 2, naming the version
and what it lacks); `allow unreviewed` runs on it with a warning. `abt synth`,
`abt bundle build` and `abt bundle test` need no strategy: they work from a
program whose demand covers the class whole (`check::class_program`). The
crate depends on `serde`, `serde_json`, `arrow` and `parquet` for all this.

Vendor exports become bundles through adapters (`crates/abt-kernel/src/ingest.rs`): `abt
bundle build --from-norgate DIR --class equity` reads a Norgate-style daily
layout of Parquet files (`prices` as traded, `symbols` for the symbol
history, `splits`, `dividends`, `delistings`, `membership`,
`classification`, and `exceptions`, reviewed bundle-test exceptions;
`scripts/ingest/norgate_lake_to_inputs.py` writes it from the NDLake lake);
`--from-databento DIR [--processing-delay S]` reads a Databento-style minute
layout (`ohlcv-1m.parquet` keyed at the bar close with an optional
`ts_recv`, `symbology.parquet`) with every tuple's availability recorded as
its receipt time plus the delay.

## Company books

The company's two production strategies are written in the language:
`corpus-v2/company/mw14.dsl`, the weekly S&P 1500 momentum book (daily bars
bucketed to `weeks(1)`, the `SPXTR` and `VIX` series, the `catalog`,
`state` and `book` modules, `book_fixed_base` execution, a weekly report
with its window), and `corpus-v2/company/r8l.dsl`, the intraday futures
opening-range book (a minute base, `sessions` and `sessions of "ES"`
calendars, `decide on base at session + 30min`, an `instruments` table of
the 26 roots with their start years, allowed W9, and `futures_fixed_base`
execution). `docs/language-v2.md` section 9 reads each file against the
book's own rules and lists the known differences. `crates/abt-tests/tests/corpus_v2.rs`
checks both and runs both on synthetic data of their class (R8L with its
session windows shortened to the synthetic market's thirty days). The
parity of the first formulation's versions with the books' reference
results, the run commands and the comparisons are `docs/parity-mw14.md`,
`docs/parity-r8l.md`, `docs/parity-inputs.md` and `scripts/parity/`;
rerunning them on these files is step 7 of section 11.

## Studies

`docs/data-bundle.md` (sections 6 and 7) makes a backtest a counted trial. A
study directory (`--study DIR`) holds `lineages.json`, the declared studies
in `studies/` and the append-only trial log `trials.jsonl`:

```
abt study declare --study DIR --strategy sma_crossover --holdout trailing:1y --objective sharpe corpus-v2/catalog corpus-v2/lib corpus-v2/strategies
abt study run     --study DIR --strategy sma_crossover --synthetic --days 600 --grid fast=15d,25d --grid slow=50d,100d corpus-v2/catalog corpus-v2/lib corpus-v2/strategies
abt study run     --study DIR --strategy sma_crossover --synthetic --grid slow=50d,100d --walk-forward anchored:2y:6mo corpus-v2/catalog corpus-v2/lib corpus-v2/strategies
abt study reveal  --study DIR --strategy sma_crossover --synthetic corpus-v2/catalog corpus-v2/lib corpus-v2/strategies
abt study metrics --study DIR --strategy sma_crossover corpus-v2/catalog corpus-v2/lib corpus-v2/strategies
abt study report  --study DIR --strategy sma_crossover corpus-v2/catalog corpus-v2/lib corpus-v2/strategies
abt study dispute --study DIR --strategy sma_crossover --reason "a different idea" corpus-v2/catalog corpus-v2/lib corpus-v2/strategies
```

A strategy belongs to a *lineage*: its own when nothing like it exists, or
the nearest one when its normalised rules are within Jaccard 0.8 of a
member's (attached with a warning; a dispute is logged and changes
nothing). The program hash (`abt hash` and `abt study declare` print it;
`abt ir --stage normalised` carries it) is over the normal form of the checked program,
with its variable names canonicalised, its literals in canonical order and
the `report` block removed, so renaming a variable or reordering a body
resets nothing. `declare` fixes the study's inputs (hold-out policy,
objective, thresholds, and the execution block) and warns about what is
missing: no hold-out, a model the block turns off. A hold-out is
`trailing:Ny` (the last N years are truncated away from every run) or
`blocks:K:Nmo[:seed]` (K random month-aligned blocks the runs cross but
whose metrics are withheld); `reveal` runs a committed version over the
whole sample and reports the embargoed bars only, logged as an
out-of-sample trial and counted. `run` logs one trial per grid point on the
sample the hold-out leaves, then reports the deflated Sharpe ratio of the
best point over the lineage's whole trial count, the probability of
backtest overfitting over the grid, the parameter surface around the best
point (smoothness, and the share of its grid neighbours within 10 % of it)
and the best point's Sharpe ratio per calendar year, with warnings for a
short sample, a Sharpe ratio below its minimum track-record length,
multiple testing, and a peak rather than a plateau. With `--walk-forward
anchored:2y:6mo` (or `rolling`), each fold picks the grid's best point on
its train window and judges it on the test window, every evaluation a
logged trial. The metrics library behind it is `crates/abt-kernel/src/study/metrics.rs`;
returns are reported on the `report` block's calendar and annualised by its
`periods`. A plain `abt run` is an untracked trial: logged when it names
`--study DIR`, warned either way. `report` reads the lineage back as a bias
checklist: its members and disputes, the studies declared, the trial count
by kind (in studies, untracked, reveals) and the study runs, the degrees of
freedom, the latest trial and every reveal, the author's thresholds judged
on them, every warning raised with its bias and how often, and the biases
of `docs/data-bundle.md` sections 4 to 6 with their status and the rows a
warning touched marked.

## The reference engine

`reference/` is a second implementation of the execution contract in plain
Python over pandas, written from the semantic model's section 6 and sharing
nothing with the kernel (`docs/data-bundle.md`, section 8). `abt run --dump
DIR` writes what a run saw and did (the data as Parquet, the configuration,
decisions, fills, dropped decisions, actions, the book at every bar and the
final state); `python3 reference/diff.py DIR` replays the decisions through
the reference executor and compares fills, book, NAV and dropped count
(`reference/README.md` gives the tolerances). `crates/abt-tests/tests/reference.rs` runs
the comparison over the daily corpus strategies and skips with a note where
`python3` with pandas is absent. `.github/workflows/ci.yml` is the GitHub
Actions workflow (format, clippy, tests with pandas installed so the
comparison runs).

## Stylized facts

`corpus-v2/strategies/` carries the four canonical strategies of
`docs/data-bundle.md` section 8, each long-short at each month start over
the index members, four tenths of the capital a leg: `momentum_12_1`,
`low_volatility`, `short_term_reversal` and `size_proxy` (market
capitalisation is not a catalog relation, so average dollar volume stands
in). `crates/abt-tests/tests/corpus_v2.rs` checks them and runs them on the synthetic daily
market; `crates/abt-tests/tests/stylized.rs` checks wide published ranges for each when
`ABT_BUNDLE_DIR` names a bundle of the point-in-time universe, and skips
the ranges with a note without one.

## Briefs

`briefs/` holds plain-language briefs for the model-authored corpus
(`docs/data-bundle.md`, section 8) and the procedure: a model writes the
strategy from a brief, its successive attempts are kept as
`attempts/<brief>/<n>.dsl`, and `abt briefs report --briefs briefs
--attempts attempts [--out report.json] corpus-v2/catalog corpus-v2/lib`
records, per brief, whether the first attempt checked clean, the
diagnostics of every attempt, the attempts to a valid program and the first
valid one's verdict on the synthetic market (trades, does not trade, does
not survive costs, halted), and across briefs the first-attempt pass rate,
the distribution of diagnostics and the mean attempts to a valid program.
Measured and stored, never asserted.

`docs/llm-usability.md` reports the study of whether a model can write the
company books from the docs alone, on the first formulation, and
`docs/llm-observability.md` the study of the observability commands on the
same books; every attempt, the scores and the root causes are in
`experiments/llm-usability/` and `experiments/llm-observability/`.
Section 11 of `docs/language-v2.md` asks for the usability experiment to be
rerun on the second formulation.

## Development

```
cargo test --test corpus_v2      # the acceptance corpus: checks, negative cases, runs on synthetic data
cargo test                       # every suite
cargo build --release
```

Adding a corpus case: a strategy goes in `corpus-v2/strategies/` and must
check clean: no errors, and no warnings beyond those it allows itself with a
`# allow: <code>` header line (one per code, warnings only; the four
strategies with an instrument literal allow W6, R8L allows W9, and a header
whose code is not raised fails the test). It must also run on the synthetic
market of its class and decide at least once. A negative case goes in
`corpus-v2/negative/` with a `# expect: <code>` header and must produce
errors of that code only, and no program. `crates/abt-tests/tests/corpus_v2.rs` asserts all
of this, with the counts (24 strategies, 2 company programs, 35 negative
cases), and `crates/abt-tests/src/lib.rs` gives a new test the catalog and the
library as a workspace, a strategy skeleton to fill in and crafted markets.
From Rust, a program is checked and run as the tests do it: a
`check::Workspace` takes every unit's source with `add_source`,
`check_workspace` judges every strategy in it, `check_program(&ws, name)`
returns the linked `Program` with its diagnostics, `data::synthetic_for_program`
builds the market of its class, `ExecConfig::from_program` the executor,
and `kernel::run` the `RunResult` (the crate is `abt_kernel`). The suites
under `crates/abt-tests/tests/` and `crates/abt/tests/` run on this
formulation's programs; `crates/abt-tests/tests/tv_golden.rs` holds every
corpus program's run to its golden file (`docs/spec/ERRATA.md`, E-1);
`.github/workflows/ci.yml` runs `cargo fmt --check`, `cargo clippy
--all-targets -- -D warnings` and `cargo test` at the workspace's virtual
root, which covers every member.

## Program

The crate follows `docs/language-v2.md`, whose section 11 orders the
implementation in seven steps: blocks and the closed configuration, with
`corpus-v2/` as the acceptance corpus; the catalog and the resolver;
calendars and bar views; sessions and the decision clock; universes, lists
and vocabularies; order-independent bodies, explicit groups and case forms;
queries and provenance; and the library and the books, whose parity scripts
are rerun on `corpus-v2/company/`. The checker judges every code of section
10, the kernel runs every program of the corpus on the synthetic market of
its class, and the observability and compiler commands of
`docs/observability.md` and `docs/compiler-design.md` run on the linked
program. `docs/data-bundle.md` remains the program for the data side
(bundles, the fold, the bias audit and the study API), all of it present
in this crate; `docs/semantic-model.md` specifies what section 10 retains
unchanged.
