# absolute-backtest

A backtesting system for programs written in a strategy language, built as
three programs that share nothing but a protocol
([`docs/spec/00-architecture.md`](docs/spec/00-architecture.md)):

| Program | Crate | What it does | Specification |
| --- | --- | --- | --- |
| The data layer | `crates/abt-data` | Holds typed relations with two timestamps in a store, answers structured queries and aggregate requests, streams events in a canonical order with explicit clock barriers, and plans the cheapest way to satisfy a demand from the store, the vendors and the feeds. It interprets nothing. | `docs/spec/01-data.md` |
| The kernel | `crates/abt-kernel` | Parses, checks, normalises, analyses and plans a program; derives its contract; binds its demand through the data layer; evaluates it as a fold over the stream; keeps the book and simulates execution for equities, futures and options; serves its state tables and its query language. | `docs/spec/02-kernel.md` |
| The query layer | `crates/abt-query` | A client of both: runs plans (studies), computes metrics, runs statistical tests, judges claims, counts trials and writes reports. | `docs/spec/03-query.md` |

The layers speak the protocol of `docs/spec/04-protocol.md`, over a Unix
socket or an in-process channel that carries the same frames. Two crates are
shared:
- `crates/abt-wire` holds the FlatBuffers messages, the frames, RFC 8785
  canonical JSON and BLAKE3 hashes;
- `crates/abt-agg` is the one implementation of every aggregate.

`abt` (`crates/abt`) is the front binary. Each subcommand is a call on one
layer. With no socket named, it links the layers in one process, with the
same results, bit for bit, as over sockets (G3).

The language is the second formulation of
[`docs/language-v2.md`](docs/language-v2.md); its grammar is
`corpus-v2/README.md`. A program is written as typed blocks and linked
against a class catalog and a standard library. A checker judges it, with
one diagnostic code per judgment, and the compiler takes it from the typed
IR to an executable plan. The kernel evaluates it bar by bar in a closed
loop with a simulated executor, every setting of which is a field of the
program's `execution` block.

## Quick start

In one process, the front serving a store directory itself:

```
abt check corpus-v2/catalog corpus-v2/lib corpus-v2/strategies      # every strategy, against the catalog and the library
abt data ingest --store ./store --adapter synthetic --class equity --days 500 corpus-v2/catalog corpus-v2/lib   # a synthetic market in a store
abt run --strategy sma_crossover --data ./store --verify-causality corpus-v2/catalog corpus-v2/lib corpus-v2/strategies/sma_crossover.dsl   # prints `run RUN: ...`
abt metrics --run RUN --store ./store
abt data query --store ./store 'kernel.RUN.book'
abt run --strategy sma_crossover --synthetic --all --fills corpus-v2/catalog corpus-v2/lib corpus-v2/strategies/sma_crossover.dsl
abt run --strategy sma_crossover --synthetic --param fast=30d --start 2022-06-01 --end 2022-09-30 corpus-v2/catalog corpus-v2/lib corpus-v2/strategies/sma_crossover.dsl
abt query --strategy sma_crossover --text 'trace decide at 2022-04-07' --text 'why not above(E3, 2022-04-07)' --synthetic --days 200 corpus-v2/catalog corpus-v2/lib corpus-v2/strategies/sma_crossover.dsl
abt query --strategy sma_crossover --text '? above(A, T), close(A, T, P), P > 50 USD/share at T = 2022-04-07' --synthetic --days 200 corpus-v2/catalog corpus-v2/lib corpus-v2/strategies/sma_crossover.dsl
```

As three processes over sockets (a socket's path must be shorter than 108
bytes):

```
abt data serve --store ./store --socket data.sock &
abt kernel serve --socket kernel.sock --data data.sock &
abt kernel run --kernel kernel.sock --strategy sma_crossover corpus-v2/catalog corpus-v2/lib corpus-v2/strategies/sma_crossover.dsl   # prints `run RUN`
abt kernel query --kernel kernel.sock --run RUN --text 'coverage'
abt metrics --run RUN --data data.sock
abt plan run study.plan corpus-v2/catalog corpus-v2/lib --data data.sock --kernel kernel.sock
```

A run reads its data from a store:
- `--data STORE` serves a store directory in the process, and the run's
  outputs are written to it;
- `--data SOCKET` reads a running `abt data serve`;
- `--synthetic` generates a market into a temporary store.

Data enters a store through `abt data ingest`: a synthetic market, an
imported bundle or directory of Parquet files (below), or a vendor's files.
Timestamps are bar close instants: a 09:30 to 09:31 minute bar is labelled
`09:31`, the last bar of a session `16:00`, and a daily bar by its date. The
synthetic adapter writes this convention; minute data labelled by open time
silently misaligns every bucket.

## The documents

The specification is six documents, a seventh added after them, and the
errata:

| Document | What it fixes |
| --- | --- |
| `docs/spec/00-architecture.md` | The three programs, the decisions record A-D1 to A-D34, the cross-layer invariants, the modes, the identifiers, the gates G1 to G10 and the targets of A-7.3 |
| `docs/spec/01-data.md` | The data layer (D): the data model and its registries, the storage engine, the query interface, streams, aggregates, the resolver, the data-side checks |
| `docs/spec/02-kernel.md` | The kernel (K): the program form, contracts derived from structure, the pipeline and the fold, market simulation for equities, futures and options with their margin and cost models, the executor modes, the state tables and the query service |
| `docs/spec/03-query.md` | The query layer (Q): plans, metrics, the test framework and its starting set, claims, feed and activation statistics, reports |
| `docs/spec/04-protocol.md` | The protocol (P): the shared crates, the transport, the services' requests, the Arrow schemas, ordering and determinism, canonical forms and hashing |
| `docs/spec/05-migration.md` | The order of landing, M-0 to M-10, each step gated |
| `docs/spec/06-performance.md` | The algorithms and structures of each layer and how the targets are reached (X), written after the six |
| `docs/spec/ERRATA.md` | Where the implementation settles what the documents leave open, one entry per decision (E-1 onwards), and the changes the golden runs were blessed for |

The design documents it builds on, cited by the letters in brackets:

| Document | What it is |
| --- | --- |
| `docs/language-v2.md` (V) | The language: one store per instrument class with every resolution derived, programs as typed blocks, order-independent bodies with sound short-circuiting, the standard library, the query facility, MW14 and R8L rewritten, the checker's judgments |
| `corpus-v2/README.md` | The grammar the corpus assumes: the nine blocks, the `data` block's needs, universes, calendars, signatures, the literal forms, decisions, the kernel relations, literals with units, the corpus headers and the diagnostic codes |
| `docs/compiler-design.md` (C) | The compiler: Normalise, Analyse, Rewrite, Plan and Lower, the invariants that make translation validation exact, the verification tiers and the state tables (K-8 lists the deltas) |
| `docs/formal-foundations.md` (F) | The language as stratified Datalog over the cross-section and synchronous dataflow over time, every guarantee derived from established results |
| `docs/semantic-model.md` (S) | The first formulation's model; what V-10 retains of it (types, closedness, causality, total orders, stratification, the kernel's semantics) is specified there |
| `docs/data-bundle.md` (B) | The data and validation program: closed data, the online fold, the catalog, the bias audit, the study API. A design record; its status notes say where each part lives now |
| `docs/assessment.md` | Measurements: the budgets of D-9.1, the performance ladder, memory, the targets of A-7.3 as measured here |
| `docs/parity-mw14.md`, `docs/parity-r8l.md`, `docs/parity-inputs.md` | G1: the two books' parity runs, their inputs and the comparisons |
| `docs/handoff-data.md` | The real data as found: the Norgate lake's layout and the decisions the `norgate` adapter states on its versions |
| `docs/llm-usability.md`, `docs/llm-observability.md` | The two studies of models writing programs, both on the first formulation; records, with their material under `experiments/` |

## The data layer: `abt-data`

The data layer holds every relation in a store directory and serves it
(`abt data serve`); the kernel and the query layer reach it only through
its service. A store holds:
- the registry: schemas, the identity relations (`instrument`, `vendor_id`,
  `ticker`, ...) and the labels (D-2);
- every relation's versions: each is immutable, hashed and carries its
  attributes (`reviewed`, `calendar`, the source's statements), and one per
  relation and source is current (D-2.6);
- the logs, the partitions and the content-addressed cache (D-3), with the
  decoded row groups kept resident under a budget, half the machine's
  memory or `ABT_RESIDENT_BUDGET` bytes (ERRATA E-74);
- every other layer's artefacts, under `kernel.<run>.*` and
  `query.<plan>.*`.

`abt data query` answers the query forms of D-4 in their text form, `abt data
stats` describes the versions, `abt data gc` keeps the store within its disk
budget, sparing what `abt data pin` pins, and `abt data catalog --class C`
renders a class's catalog from the registry (`corpus-v2/catalog/*.dsl` are
its output).
A run's stream (D-5) is every bound relation's tuples in receipt order, with
the barriers of the program's calendars; the aggregate pass (D-6) answers
the windowed aggregates the kernel pushes down.

### The resolver

The data layer resolves the demand (`docs/spec/01-data.md`, section 7). Each
need's relations are bound at the store's versions that satisfy the need,
chosen as before: the version a preference names, the finest calendar, else
the first source by name. A catalog of sources (`abt data serve --config
FILE`: each source's offers, its prices with A-8's placeholders given
values, its quota) lets the resolver buy the parts of a need's span that
the store does not hold. It buys the cheapest cover of the span (money
first, then seconds, bytes and requests) through the source's adapter, and
only within the program's `budget`, whose money is 0 by default: nothing is
bought unless the program allows it. What it buys is ingested, versioned
and the store's from then on. A plan over budget binds what the store holds
and reports the cheapest plan that covers every span. `abt data resolve
--demand FILE --dry-run --explain` prints a plan without buying, and `abt
data catalog --sources` prints the catalog (ERRATA E-67).

### Imports: bundles and Parquet directories

A bundle (`docs/data-bundle.md`, sections 2 and 3) is the first formulation's
data directory, and since M-9 an import format: `abt data ingest --adapter
bundle_v2 --from DIR --class CLASS` reads it, or a directory of Parquet
files, into a store, and a run reads the store. A bundle is `manifest.json`
(the class, the version, the bundle date, every relation with its signature,
resolution, availability convention, row count and partitions, and the
bundle tests' report once they passed), `securities.parquet` (the security
table: `id,ticker,from,to`, `to` null while the ticker is still carried, and
optionally the contract columns `multiplier`, `asset_class` and
`commission_per_contract`) and `log/<relation>/<YYYY-MM>.parquet`,
append-only Parquet partitions of each primitive relation by the month of
its temporal key. With a security table every equity field is a security
id and the `ticker` relation is derived from the table. A directory of
Parquet files is the same files without the log: one `<relation>.parquet`
per primitive of the program's class, a relation being a function of its
identity columns (two rows for one identity with different outputs are an
error naming both rows; an identical row is dropped), a missing or empty
file leaving the relation empty with a `note:`.

```
abt data ingest --store ./store --adapter bundle_v2 --from ./bundle --class equity corpus-v2/catalog corpus-v2/lib
abt data ingest --store ./reviewed --adapter bundle_v2 --from ./bundle --class equity --attributes reviewed=complete corpus-v2/catalog corpus-v2/lib
abt run --strategy sma_crossover --data ./store corpus-v2/catalog corpus-v2/lib corpus-v2/strategies/sma_crossover.dsl
```

A bundle whose tests passed imports `reviewed = complete`; one that did not
imports unreviewed, unless a reviewer says otherwise with `--attributes`. A
program's `data` block says `require reviewed`, so a run on an unreviewed
store is refused (exit 2, naming the version and what it lacks); `allow
unreviewed` runs on it with a warning. The bundle tests' checks of market
facts (action reconciliation, delisting coverage, membership) are the
kernel's preconditions, run at every run's start (ERRATA E-50), and the
query layer's `actions` feed statistic.

### Vendors

A vendor's data enters a store through the data layer's adapters (D-7.7,
`crates/abt-data/src/adapters/`, ERRATA E-68), named by `abt data ingest
--adapter` or by a catalog's source:

- `norgate` reads the NDLake lake directly (`--from LAKE`): the equity class
  of the members, ever, of its lists (`--lists 'S&P 500=SPX'`), keyed by
  Norgate's `asset_id`, with index series (`--series '$SPXTR=SPXTR'`) and
  the ten decisions of `docs/handoff-data.md` (section 4) stated on the
  versions; `--exceptions FILE` adds the reviewed exceptions
  (`scripts/reviewed/norgate-spx-2014.parquet`).
- `databento` reads DBN files (`.dbn`, `.dbn.zst`), the bucket's Parquet
  layout, or the historical API (`--from https://hist.databento.com` with
  `--dataset --schema --symbols --stype-in`, the key from
  `$DATABENTO_API_KEY`). Futures are contracts by `instrument_id`, rolled
  per root into a back-adjusted continuous series (`ES.v.0`, by volume, or
  `ES.c.0`, by expiry) with `continuous`, `future_spec`, sessions and point
  values; equities are joined to the store's tickers by symbol. Bars are
  keyed at their close and received at `ts_recv` or the close, plus
  `--processing-delay S`. The venue's statistics give `settlement`
  (back-adjusted as the bars) and `open_interest`, each keyed at the
  session's last close and received when published.
- `databento_live` is the live gateway's session (CRAM, the subscription,
  DBN), every record stamped at acceptance.
- `orats` reads ORATS's strikes, files or the Data API (`--from
  https://api.orats.io --tickers SPY`, the token from `$ORATS_TOKEN`), as the
  option class (ERRATA E-70): every contract an instrument keyed by its OCC
  symbol, with its quotes, its greeks, its terms (`option_spec`, `--terms
  'SPX=european:cash:am'`) and its day's chain (`chain_index`), the
  underlying joined to the store's tickers. A demand's `need chain` is
  resolved by a pre-pass that binds the quotes of the contracts the chain's
  filters admit and buys a missing day's chain within the budget.

### Live sessions

A live session (D-5.4, `crates/abt-data/src/live.rs`, ERRATA E-69) records
a feed into the store write-ahead, every row received at its acceptance.
`abt run --mode live-sim --session S --data SOCKET` reads it as it arrives:
the data service emits each barrier when its clock passes the label plus the
calendar's grace, after the bars accepted before it, and records the
instants. `abt run --replay-of RUN` replays that recorded stream (`--paced`
at the clock's pace) and decides and fills as the live run did (G4,
`crates/abt-tests/tests/replay.rs`). A bar evaluated past its deadline
(`--deadline`, one bar by default) is late: the execution block's `late send`
flags its decisions, `late reject` drops them. `abt data serve --record
synthetic --clock-rate R` records a rehearsal session of the synthetic feed.

## The kernel: `abt-kernel`

The kernel owns the language, from the parser to the plan, and the run, from
the binding to the outputs. It is a library, a binary (`abt-kernel`) and a
service (`abt kernel serve`, P-3.4).

### The language in one page

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
  greatest, log, exp, sqrt, sign, floor, ceil, clamp, decile, and the
  option pricing functions of K-2.5: `bs_price(S, K, τ, r, q, σ, right)`
  with `bs_delta` … `bs_rho` (Black–Scholes–Merton), `b76_price(F, K, τ,
  r, σ, right)` with its Greeks (Black 1976), `am_price` (Bjerksund and
  Stensland 2002) and `implied_vol(P, S, K, τ, r, q, right, style)`, τ a
  Duration (`46d`) or years. `observe R
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
  delta, filled at one bar under one order id, and `exercise(A, Q)`
  exercises Q long contracts of an option at the decision bar's close. Each
  takes an optional trailing order (`market`, `moo`, `moc`,
  `moo_moc`, `limit(P[, TIF])`, `stop(P[, TIF])`), the block's `orders`
  when omitted. `silence hold` keeps an instrument with no decision;
  `silence flat` closes it. A sale of a held quantity is a negative delta
  bound in the body: `held(A, T, Q0), ..., Q = -Q0`. Two decisions for one
  instrument at one bar halt the run naming both rules.
- The kernel supplies, at the decision calendar, `position(A, @T, Q)`,
  `cash(@T, C)`, `nav(@T, N)`, `capital(@T, C)` (the base the execution
  block's `capital` names), `mark(A, @T, P)` (the price it names, as the
  book is marked), `fill(A, @T, Q, P)` and `decided(@T0, D)`; the `book`
  module's views of them are closed, so `not held(A, T, _)` is legal. An
  option program also has `chain(U, @T, A)` (the contracts its `need
  chain` admits at the bar), `spec(A, @T, Underlying, Right, Strike,
  Expiry, Style, Settlement, Multiplier)` (as adjusted by corporate
  actions) and `greeks(A, @T, IV, Delta, Gamma, Theta, Vega, Rho)`.
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

### Reading the checker

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

### A run: the fold over the stream

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
(or every N bars). Over `--data SOCKET` or `--data STORE` it is a cache object of the data
layer, keyed by the run's id and the seq and printed (`checkpoint KEY of
DATE`), and the run writes its outputs so far before it; under `--synthetic` it is
`DIR/<bar>.json` under `--checkpoint-dir DIR`. `--resume KEY|FILE`
continues from one. The checkpoint's program hash, binding, mode, run id
and configuration must be the run's, and a resume that differs is refused
naming which (ERRATA E-56). A run is identified by BLAKE3 over its program
hash, its binding (the data versions and the parameter point), its window
and the mode, 64 hex digits. The executor
is a trait (`kernel::Executor`), so a live run attaches another executor to
the same fold.

A tuple carries its receipt time `r` (an `available_at` column in an
imported Parquet file or a bundle's partitions; a vendor's receipt stamp):
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

### The executor

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
relation on each path to it (W8). A plain run also warns of what counts it as
a trial: over a data service its program's lineage does, in a temporary
store nothing does (see Plans and trials).

### Options and futures

An option program's contracts are instruments with terms (`spec`), named by
their OCC symbols. They are marked at their quote: `option_mark mid` (half
the ask without a bid, the last mark `stale` without a quote), `last`, or
`model` (the model's price at the contract's implied volatility). They are
filled against it, `option_fill touch + F spread`, `mid + F spread` or
`last`, at `commission per_contract P` with no slippage or impact; a fill's
`spread` is its cost against the mid.

- **Combos.** A combo fills every leg at one bar under one order id, or is
  dropped naming the leg without a quote. Its `limit(P)` is the net price
  per unit of the combo.
- **Expiry.** At its expiry day's close (its open, for AM settlement) an
  option in the money beyond `atm_tolerance` is exercised (long) or
  assigned (short). A physical one delivers the underlying at the strike,
  as a fill; a cash-settled one pays its intrinsic value; any other
  expires.
- **Early exercise.** The `exercise` field's `early_short` assigns an
  American short early (a call before an ex-dividend date whose extrinsic
  value is below the dividend), and `early_long decide` carries out the
  program's `exercise(A, Q)`.
- **Corporate actions.** A split or special dividend of the underlying
  adjusts the terms the book holds by OCC's conventions (`adjustment`), and
  a delisted underlying forces its options to expire at intrinsic.
- **Greeks.** `greeks data` serves the binding's rows; `greeks model`
  computes them at the mark's implied volatility.

The hand-computed cases are `crates/abt-tests/tests/options.rs` (ERRATA
E-71). The pricing functions' known answers are
`crates/abt-kernel/tests/pricing.rs`.

A future is booked fully paid under `settlement none`, as R8L's book is.
Under `settlement daily` it is margined (ERRATA E-72): a fill moves only its
costs and the variation of the units it closes against the position's
reference price, each session's close moves the variation since the
reference to cash at the binding's `settlement` price (the close where it
has none, warned) and makes that price the reference, and the NAV counts the
position against its reference. A roll of the continuous series with a
position on the root records the two contracts' fills and their commissions
(`roll`); a contract held itself is settled and closed at its last trade
(`contract_expiry`). `contract(A, @T, Root, Expiry, PointValue)` names the
contract each series holds. The execution block's `margin published, reg_t,
portfolio, span, approx F` price the book at every decision bar, the first
model that covers an instrument setting its requirement: the binding's
published margins, Reg T's option and stock rules, a portfolio margin over
the underlying's moves with each option repriced at the volatility its mark
implies, or a fraction of the notional. `margin_req(@T, Initial,
Maintenance)` is written for every program and `margin_by` per instrument
where models are listed; a margin call below the maintenance halts, is
allowed, or liquidates whole units, the largest maintenance per unit first
(`forced_liquidation`). The cases are `crates/abt-tests/tests/futures.rs` and
the margin rules' are `crates/abt-kernel/tests/margin.rs`. K-9.3's 25 cases
(G8) are in `tests/options.rs`, `tests/futures.rs` and `tests/limits.rs`;
ERRATA E-73 lists each case's test.

### Parameters and decisions

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

### The contract

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

### What a run writes

A run over a data service or a store (`abt run --data SOCKET|STORE`) writes
its outputs to the store under `kernel.<run>.*` and prints the run's id (K-7.1):
`events` (the event log), `book` (the book at every decision barrier),
`holdings`, `decisions` and `fills` (projections of the log), `coverage`,
`contract` (the clauses with the run's statuses), `binding` (the versions
read, the program's hash and normal form, the parameter point),
`plans`, `late`, `report` (the status, `completed` or `halted(reason)`, the
counters and the warnings), and `ledger.R` for each `observe R`. A halted
run writes what it has. `abt data query --store DIR 'kernel.<run>.book'`
prints one, and `--parquet FILE` writes it for pandas: the parity scripts
read the books' runs that way (ERRATA E-52).

### The query language

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

### Observing a run: K-7's commands

A program that checks clean can still decide nothing, and a program that
decides can do so for another reason than its author believes. The kernel
therefore answers, for every relation, what it held at any bar and why, and
every run reports what every relation derived (K-7; V-8 is the design).
These commands were `docs/observability.md` until M-9 (ERRATA E-80).

**The program before a run.** `abt show --strategy NAME <files...>` prints
the program graph: every relation `decide` reaches, from `decide` upward.
For each it gives:
- its signature: `@T` the temporal key and every other argument with its
  type; a library relation appears under its instance name, such as
  `bought_within[cooldown]`;
- its kind: primitive, executor, kernel, derived or output;
- its calendar, and whether it is complete, so that `not R(...)` is
  admissible;
- its stratum, and whether it reads its own past (temporal recursion);
- whether it is in the closed loop (it reads the book: `position`, `cash`,
  `nav`, `fill`, `decided`) or the open loop;
- its depth (the longest path from a primitive);
- the relations it reads, each with its sign (`+` positive, `-` negated,
  `~` inside an aggregation or reduction), and its rules.

The primitives follow with their completeness, then the execution block
expanded field by field (a preset's defaults included) and the report
block. Read it to confirm that the relations you meant to connect are
connected (a relation `decide` never reaches is not in the graph), that a
feature you meant to be open-loop does not read the book, and that a
relation you negate is complete.

**Every run: the coverage report.** After the metrics, `abt run` prints one
line per relation `decide` reaches:

```
coverage over 468 decision bars (calls: bars and inputs the relation was demanded at; tuples: what it derived):
  decide            468 calls       229 with tuples        1057 tuples
  tgt               468 calls       374 with tuples        4405 tuples
  crowded           374 calls         0 with tuples           0 tuples   EMPTY: demanded, never derived
  flat                0 calls         0 with tuples           0 tuples   never demanded
  close                                                     27577 tuples in the data (primitive)
```

A derived relation's `calls` are the bars at which a rule or the decision
asked for it, `with tuples` the calls that derived at least one tuple, and
`tuples` all it derived. A primitive, executor or kernel relation shows what
the data, the executor or the kernel holds.
- **EMPTY: demanded, never derived** marks a relation that was asked for and
  never held: a rule reading it positively never fires.
- **never demanded** marks a relation no rule reached, because an earlier
  literal of every rule that reads it excluded every instance first.

When `decide` derived nothing, the report adds the root causes. It gives the
path of empty relations from `decide` down to the first one whose own reads
are not empty. For that relation's rules, it gives an explanation at a
sample bar naming the first literal with no solution:

```
never fired: decide derived no tuple over 500 bars.
  decide <- go <- rich: `rich` is the first empty relation on this path; what it reads is not empty: universe+ (1000 tuples), close+ (1000 tuples)
  rule never::rich#1 did not fire at 2022-12-19: literal 3 `P > floor` has no solution (a sample bar; abt query --text 'why not rich(A, BAR)' for another)
```

**Any relation at any bar: the query language.** See `abt query` above. A
query is checked as a rule of a stratum above the program, so asking it
changes no decision.

**Every bar of the run.** `observe R` in the program's `rules` block, or
`abt run ... --ledger R1,R2,... --ledger-out DIR`, records the relation's
extension at every bar of the run:
- `observe` writes it to the store as `kernel.<run>.ledger.<R>`;
- `--ledger` writes `DIR/<relation>.parquet`, with `bar` and one column per
  argument of the signature, and the coverage counts as
  `DIR/coverage.parquet`.

A library relation is named by its instance name (`--ledger 'mean_window[fast,
fast_min, close]'`). A feature the plan skipped at some bars (an earlier
literal excluded the instance) is computed for the ledger, so the ledger is
the relation's extension whatever the plan.

**The compiler's view.**
- `abt graph` is `show` with the compiler's facts (C-7.1): the keyedness
  label of every temporally recursive relation, whether a relation is
  demanded or elided, and the abstract value of each column; the last line
  carries the program hash and the plan's totals.
- `abt ir --stage S` prints a stage's result: the normalised form with its
  hash and name map, the analysis with its warnings, the rewritten form, the
  plan.
- `abt rewrite-log` prints every rewrite applied, with its law.
- `abt show plan` prints the literal order and cost of every rule, the
  aggregate implementations, the stores and strata, and each aggregate's
  pushdown; with `--observed` it adds the counts a run produced.
- `abt verify` prints the verification report tier by tier, every
  obligation with its verdict, budget use and replay file. Tiers 3 and 4
  need a solver on the PATH (`--solver z3`); without one their obligations
  are undecided, and `--replay-dir DIR` keeps the scripts to run by hand.

**The state tables.** After a run the state tables of C-7.2 are queryable.
`abt table --table NAME` prints one as text (the schema line, then the
rows) or JSON; `--out FILE` writes it as Arrow IPC, or Parquet for a
`.parquet` name. `abt run ... --tables DIR` writes them all under `DIR`:
`extension/<R>` for every demanded derived relation, `carried/<R>` for the
temporally recursive ones, and `derivation` and `failure` of `decide` at the
sampled bars (the last bar and nine evenly spaced ones; `abt table --at BAR`
answers for any other).

| Table | Columns | What it holds |
| --- | --- | --- |
| `coverage` | relation, derived, calls, nonempty, tuples | the coverage counts of the run |
| `plan` | rule, position, literal, kind, text, card_in, card_out, cost, selectivity, observed_in, observed_out, observed_emptied, infeasible, elided | every literal in its evaluation order, with the estimates and the observed counts |
| `book` | bar, cash, gross, net, equity, leverage | the executor's ledger at every bar |
| `binding` | relation, kind, rows, first_bar, last_bar, availability | the data the run was bound to |
| `summary` | rule, literal, aggregate, group, bar, rows, path, shift_x, shift_y, sum_x, sum_y, sum_xx, sum_yy, sum_xy, value | the window state of every aggregation group at every bar it served: the rows a calendar window held, or the shifted moment sums of a `rows` window served by running sums (`mean = shift_x + sum_x / rows`) |
| `extension` (`--rel R`) | the signature's columns | every tuple of R at every bar of its resolution, sorted by bar |
| `carried` (`--rel R`) | identity, bar, carried_from, values | at every bar, the latest earlier tuple of each identity: what a rule reading R through `prev` sees |
| `derivation` (`--rel R`, `--at T`) | tuple, bar, rule, body | one witness per derived tuple at the bar: the rule and its body tuples |
| `failure` (`--rel R`, `--at T`, `--symbol`) | rule, identity, bar, literal, literal_text, bindings, absence_driven | for every rule and attempted instrument, the first excluding literal and the bindings at that point |

**A way to work.**
1. `abt check` until clean.
2. `abt show`, and read the graph against what you meant to write.
3. `abt run`, and read the coverage report. An EMPTY relation on the way to
   `decide`, or a `never fired`, is the first thing to resolve.
4. `--text 'trace R at BAR'` on the first empty relation. The literal named
   is where the rule and the data disagree; `--text '? ...'` on that
   literal's relation at the same bar shows what it holds.
5. `observe R` or `--ledger R` for a feature's values over time, when one
   bar is not enough (a window that never fills, a value off by a unit, a
   latch that never sets). `abt table --table extension` or `carried` asks
   the same through the state tables, and `summary` gives a window's state.
6. `--text 'why not R(S, BAR)'` when a tuple you expected is absent: the
   excluding literal and its bindings, and whether the data (absence-driven)
   or a comparison decided.
7. `--text 'why R(...)'` on a tuple you did not expect: the tree of tuples
   that produced it.
8. `abt graph`, `abt show plan` and `abt verify` before trusting a program:
   the keyedness labels, the dead literals the analysis found, the plan the
   run uses, and the verification report.

### The kernel service

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

## The query layer: `abt-query`

The query layer never evaluates a program. It asks the kernel for runs and
reads their tables back from the store, through the data service like every
other reader (A-D13, A-D17). Its commands are `abt plan`, `abt metrics`,
`abt test`, `abt claim`, `abt feed`, `abt activation`, `abt lineage`,
`abt dispute` and `abt briefs`.

### Metrics

The query layer reads a run back from the store (`docs/spec/03-query.md`).
`abt metrics --run RUN --data SOCKET` (or `--store DIR`) computes the run's
metrics from the tables it wrote. These are the return metrics of its NAV
curve, its trading metrics, its capacity, the metrics per period of its
report block, and the Sharpe ratio of each calendar year, each printed
with the versions of the tables it read (ERRATA E-58). Its book metrics
are the margin's share of the NAV, the largest weight, the effective number
of names and the funding paid. A run that held options adds the options
metrics (ERRATA E-73): premium paid and collected; exercises, assignments
and expiries; days to expiry at entry and exit; the book's net Greeks at
every bar; each underlying's realised against implied volatility; and each
bar's P&L attributed to Δ, Γ, Θ and V, with the residual.

### Plans and trials

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
report` prints the report, and `abt plan drop` unpins. `abt lineage`
counts a lineage's trials, every run ever made of its members, and `abt
dispute` records a dispute of an attachment (ERRATA E-59).

`docs/data-bundle.md` (sections 6 and 7) makes a backtest a counted trial.
The kernel's study directory is retired (ERRATA E-66): a study is a plan,
and its trials, lineages and disputes are rows of the store.

```
abt plan check  study.plan corpus-v2/catalog corpus-v2/lib corpus-v2/strategies --store DIR
abt plan run    study.plan corpus-v2/catalog corpus-v2/lib corpus-v2/strategies --store DIR --kernels 4
abt plan reveal study.plan final corpus-v2/catalog corpus-v2/lib corpus-v2/strategies --store DIR
abt plan report study.plan corpus-v2/catalog corpus-v2/lib corpus-v2/strategies --store DIR
abt lineage sma_crossover corpus-v2/catalog corpus-v2/lib corpus-v2/strategies --store DIR
abt dispute sma_crossover --reason "a different idea" corpus-v2/catalog corpus-v2/lib corpus-v2/strategies --store DIR
```

A strategy belongs to a *lineage*: its own when nothing like it exists, or
the nearest one when its normalised rules are within Jaccard 0.8 of a
member's (attached with a warning; a dispute is a row and changes nothing).
The program hash (`abt hash` prints it; `abt ir --stage normalised` carries
it) is over the normal form of the checked program, with its variable names
canonicalised, its literals in canonical order and the `report` block
removed, so renaming a variable or reordering a body resets nothing. A
lineage's trials are every run of its members the store holds, in a plan or
not, and every claim and test judged on one; the deflated Sharpe ratio of a
plan's best point is over that count. A hold-out is `trailing:Ny` (the last
N years are truncated away from every run) or `blocks:K:Nmo[:seed]` (K
random month-aligned blocks the runs cross but whose metrics are withheld);
`abt plan reveal` runs a committed point over the whole sample and reports
the embargoed bars only, counted as a trial. A grid's report gives the
probability of backtest overfitting, the parameter surface around the best
point (smoothness, and the share of its grid neighbours within 10 % of it)
and the best point's Sharpe ratio per calendar year; a walk-forward picks
each fold's best point on its train window and judges it on the test
window. The metrics library is `crates/abt-query/src/metrics/`; returns are
reported on the `report` block's calendar and annualised by its `periods`.
`abt plan report` reads the plan back as a bias checklist: its trials, the
claims and tests with their verdicts, the contract, every warning with its
bias, and the forty biases with the layer that owns each, its status and its
source.

### Tests and claims

A plan's claims are tests of the registry, 24 of them in the starting set
(`docs/spec/03-query.md`, section 4), each checked against its known
answers (ERRATA E-60 to E-62). Each claim is judged on its input with an
expected outcome. Its p-value is adjusted by Benjamini–Hochberg among the
claims of its phase, and its verdict is written as a row of
`query.<hash>.verdicts`. A `before` claim that fails under `block` stops
the runs that would follow it. A reveal judges every claim again over the
embargoed bars. Feed statistics describe the binding's data, activation
statistics a run's rules, and the report the whole execution, with the
forty biases and the layer that owns each (ERRATA E-63). `abt test`, `abt
claim`, `abt feed` and `abt activation` do the same outside a plan; a test
or a claim made there is still counted as a trial.

### Briefs

`briefs/` holds plain-language briefs for the model-authored corpus
(`docs/data-bundle.md`, section 8) and the procedure: a model writes the
strategy from a brief, its successive attempts are kept as
`attempts/<brief>/<n>.dsl`, and `abt briefs report --briefs briefs
--attempts attempts [--out report.json] corpus-v2/catalog corpus-v2/lib`
records, per brief, whether the first attempt checked clean, the
diagnostics of every attempt, the attempts to a valid program and the first
valid one's verdict on the synthetic daily market, run as an instance of
the plan template `briefs_harness` under the program's own execution block
(trades, does not trade, does not survive costs, halted), and across briefs
the first-attempt pass rate, the distribution of diagnostics and the mean
attempts to a valid program. Every attempt is a row of
`query.briefs.attempts` (ERRATA E-65). Measured and stored, never asserted.

`docs/llm-usability.md` reports the study of whether a model can write the
company books from the docs alone, on the first formulation, and
`docs/llm-observability.md` the study of the observability commands on the
same books; every attempt, the scores and the root causes are in
`experiments/llm-usability/` and `experiments/llm-observability/`.
Section 11 of `docs/language-v2.md` asks for the usability experiment to be
rerun on the second formulation.

## The command line

```
abt check <files...>
abt run --strategy NAME (--data STORE|SOCKET | --synthetic [--days N] [--symbols A,B,C] [--seed N])
        [--start DATE] [--end DATE] [--as-of DATE] [--param NAME=VALUE]... [--plan written|optimised]
        [--verify-causality] [--no-pushdown] [--evaluator set|tuple] [--all] [--fills] [--nav FILE] [--returns FILE] [--tables DIR] [--ledger R1,R2,... --ledger-out DIR] [--quiet] [--timing] <files...>
abt run ... [--checkpoint-every month|N [--checkpoint-dir DIR]] [--resume KEY|FILE]
abt run ... [--mode replay-sim|live-sim --session S] [--replay-of RUN [--paced]] [--deadline SECONDS|none] [--memory-budget BYTES]
abt hash --strategy NAME [--json | --show] <files...>
abt show --strategy NAME <files...>
abt graph --strategy NAME <files...>
abt ir --strategy NAME [--stage parsed|checked|normalised|analysed|rewritten|planned] [--json] <files...>
abt rewrite-log --strategy NAME [--json] <files...>
abt show plan --strategy NAME [--observed] [--json] [--plan written|optimised] [(--data STORE|SOCKET | --synthetic ...)] <files...>
abt verify --strategy NAME [--tier N] [--solver z3|cvc5] [--replay-dir DIR] [--json] [(--data STORE|SOCKET | --synthetic ...)] <files...>
abt contract --strategy NAME [--json] [(--data STORE|SOCKET | --synthetic ...)] <files...>
abt table --strategy NAME --table coverage|plan|book|binding|summary|extension|carried|derivation|failure [--rel R] [--at T] [--symbol SYM] [--out FILE] [--json] (--data STORE|SOCKET | --synthetic ...) <files...>
abt query --strategy NAME --text QUERY [--text QUERY]... [--json] [--out FILE] (--data STORE|SOCKET | --synthetic ...) <files...>
abt metrics --run RUN (--data SOCKET | --store DIR) [--json]
abt plan check|run|report|drop PLAN [SOURCES...] [--study NAME] [--threads N] [--dry-run] [--json] (--data SOCKET | --store DIR) [--kernel SOCKET... | --kernels N]
abt plan reveal PLAN STEP [SOURCES...] [--study NAME] [--json] (--data SOCKET | --store DIR) [--kernel SOCKET... | --kernels N]
abt lineage PROGRAM [SOURCES...] [--json] (--data SOCKET | --store DIR)
abt dispute PROGRAM --reason TEXT [SOURCES...] (--data SOCKET | --store DIR)
abt test TEST on INPUT [--run RUN] [--with NAME=VALUE]... [--over FROM..TO] [--json] (--data SOCKET | --store DIR)
abt claim 'NAME : TEST on INPUT ... expect OUTCOME' --run RUN [--json] (--data SOCKET | --store DIR)
abt feed STATISTIC (--binding B | --run RUN) [--relation R] [--over FROM..TO] [--json] (--data SOCKET | --store DIR)
abt activation --run RUN [--json] (--data SOCKET | --store DIR)
abt data serve [--store DIR] [--socket PATH] [--config FILE] [--clock-rate R] [--grace 'CAL=S;...'] [--record synthetic --session S --start T --end T ...]
abt data ingest [--store DIR] --adapter norgate|databento|orats|bundle_v2|synthetic --from LOCATOR ...   (abt data prints each adapter's options)
abt data resolve [--store DIR] --demand FILE|- [--config FILE] [--explain] [--dry-run] [--json]
abt data catalog [--store DIR] [--class CLASS | --sources [--config FILE]]
abt data query [--store DIR] 'TEXT' [--view BINDING|VERSION] [--as-of-seq N] [--explain] [--parquet FILE]
abt data stats [--store DIR] [--relation R] [--json]
abt data pin|unpin [--store DIR] VERSION | --binding B
abt data gc [--store DIR] [--disk-budget BYTES]
abt kernel serve [--socket PATH] --data SOCKET
abt kernel run|check|contract [--kernel SOCKET] --strategy NAME [--param NAME=VALUE]... [--start DATE] [--end DATE] [--progress N] [--checkpoint-every month|N] [--resume KEY] <files...>
abt kernel query|show|status|control [--kernel SOCKET] --run RUN [--text QUERY | --what FORM | halt | checkpoint]
abt briefs report --briefs DIR --attempts DIR [--out FILE] [--kernels N] <files...>
```

The executor is the program's `execution` block and the reporting its
`report` block: no option sets a cost, a policy, a price relation or the
periods. `abt` with no arguments prints the usage with every option
explained. `show`, `graph`, the coverage report every `run` prints,
`query` (whose `why`, `why not` and `trace` replace the first formulation's
`explain`, `why-not` and `materialise`), `--ledger`, the state tables and
the compiler's `ir`, `rewrite-log`, `plan` and `verify` are described
under "Observing a run" above; the compiler itself in
`docs/compiler-design.md`.

`<files...>` are `.dsl` files or directories searched recursively; a run
needs the catalog, the library modules the program uses and the program
(`corpus-v2/catalog corpus-v2/lib corpus-v2/strategies/sma_crossover.dsl`).
The synthetic market is the one of the program's class and base, seeded
(`--seed`, default 7) and deterministic, over 500 days (40 for a sub-daily
base) and the symbols `AAA,BBB,CCC,DDD,SPY`; its equities are securities
`E1`, `E2`, ... carrying those tickers, which is why a decision reads
`trade(E3, 100)` and a query names the instrument `for A = E3`. The daily market
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
tuples at every bar, and `--tables DIR` every state table as Arrow IPC.
A run over a store writes everything it did there (`kernel.<run>.*`), which
is what the reference engine reads back. `--timing` prints one
stderr line with the load, build and run times, the bar and tuple counts
and the kernel's statistics, among them `pushed_answers`, the windows the
data layer's aggregate pass answered. An aggregate over one stored relation
in one of the pass's window forms is pushed down to the data layer, which
computes it over the whole run in one request; `abt show plan` prints, under
each aggregate, `pushdown: ...` or `local:` and the condition that keeps it
in the kernel (`docs/spec/ERRATA.md`, E-47). `--no-pushdown` evaluates every
aggregate in the kernel, a test switch: both give the same bits. A rule's
body is solved over blocks of bindings, each row a slice of one buffer (the
set evaluator of `docs/spec/06-performance.md`, ERRATA E-75); `--evaluator
tuple` solves it one environment at a time, also a test switch.

Every option value is validated: `--days abc`, `--symbols ""` or `--at
yesterday` are errors naming the option, never a silent default, and an
unknown `--option` prints the usage. Exit codes: 0 success; 1 the strategy
does not check, the run was refused or halted, or the usage is wrong; 2 an
input could not be read (a source file, a store, a data service's socket),
or a store's data was refused (a binding without the attributes the
program requires).

## Verification

The gates of A-7.1, and where each is checked here:

| Gate | Here |
| --- | --- |
| G1 parity | The owner's: `scripts/parity/` against the books' references (`docs/parity-inputs.md`). Its stand-in here is `crates/abt-tests/tests/tv_golden.rs`: every corpus-v2 strategy and both books held to their golden runs, which change only in a commit that names the change, recorded in the ERRATA. |
| G2 corpus | `crates/abt-tests/tests/corpus_v2.rs` |
| G3 determinism | `crates/abt-tests/tests/determinism.rs`: in process against a socket, 1 against 8 data threads, 1 against 4 kernels |
| G4 replay | `crates/abt-tests/tests/replay.rs`, on a recorded synthetic feed; a recorded Databento session is the owner's |
| G5 fold | `crates/abt-tests/tests/fold.rs` (truncation at every sampled barrier), `checkpoint.rs` and `fold_checkpoint.rs` (resume from every monthly checkpoint) |
| G6 reference | `crates/abt/tests/reference.rs` (below) |
| G7 contracts | `crates/abt-tests/tests/contracts.rs` |
| G8 options | `crates/abt-tests/tests/options.rs`, `futures.rs` and `limits.rs`, K-9.3's 25 cases (ERRATA E-73 lists each case's test); the owner's options book when it is supplied |
| G9 tests | `crates/abt-query/tests/known_answers_*.rs` |
| G10 performance | `scripts/bench.sh`, measured here and recorded in `docs/assessment.md`; asserted on the reference machine by the owner |

### Company books

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
rerunning them on these files is G1, the owner's (step 7 of
`docs/language-v2.md` section 11).

### The reference engine

`reference/` is a second implementation of the execution contract in plain
Python over pandas, written from the semantic model's section 6 and sharing
nothing with the kernel (`docs/data-bundle.md`, section 8). It reads a run
back from the store it ran over: the store is the dump (M-9). `python3
reference/diff.py STORE RUN` reads the run's configuration, data at the
binding's versions, decisions, events and book through `abt data query`,
replays the decisions through the reference executor, and compares fills,
book, NAV and dropped count (`reference/README.md` gives the tolerances).
`crates/abt/tests/reference.rs` (G6) runs the comparison over the daily
corpus strategies, a set of policy and model variants and four action
markets, each over a store of its own, and skips with a note where `python3`
with pandas is absent. `.github/workflows/ci.yml` is the GitHub
Actions workflow (format, clippy, tests with pandas installed so the
comparison runs).

### Stylized facts

`corpus-v2/strategies/` carries the four canonical strategies of
`docs/data-bundle.md` section 8, each long-short at each month start over
the index members, four tenths of the capital a leg: `momentum_12_1`,
`low_volatility`, `short_term_reversal` and `size_proxy` (market
capitalisation is not a catalog relation, so average dollar volume stands
in). `crates/abt-tests/tests/corpus_v2.rs` checks them and runs them on the synthetic daily
market; `crates/abt-tests/tests/stylized.rs` checks wide published ranges for each when
`ABT_BUNDLE_DIR` names a bundle of the point-in-time universe, and skips
the ranges with a note without one.

## Layout

| Path | What it is |
| --- | --- |
| `Cargo.toml`, `crates/` | The workspace of M-0: the three layers, the two shared crates, the front and the cross-layer suites. |
| `crates/abt-wire/` | The protocol's types and messages: the FlatBuffers schema `schema/abt.fbs` compiled by planus, frames, the transport (Unix socket, in-process channel), RFC 8785 canonical forms, BLAKE3 hashes. |
| `crates/abt-agg/` | The aggregate kernels: windows, moments, order statistics, reductions. |
| `crates/abt-data/src/` | The data layer: `registry.rs`, `store.rs` with `resident.rs`, `query.rs`, `stream.rs`, `agg.rs` (the aggregate pass), `service.rs`, `live.rs` (live sessions), `options.rs` (the chain query), `resolver/` (catalog, cost, cover, plan, fetch, bind, pre-pass), `adapter.rs` and `adapters/`: `norgate` (the NDLake lake), `databento` (DBN files, the bucket's Parquet, the historical API), `databento_live` (the live gateway), `orats` (option chains), `dbn` (DBN versions 1 to 3), `http`, and `mock` for the resolver's tests. |
| `crates/abt-kernel/src/syntax/` | The front end: `lexer.rs`, `ast.rs` (units are strategies, libraries, classes and execution presets; a strategy is nine blocks) and `parser.rs`. The parser never reorders literals. |
| `crates/abt-kernel/src/ir.rs` | The core IR of a linked program: domains, dimensional types, calendars, signatures, rules and the literal forms of sections 3 to 5. |
| `crates/abt-kernel/src/check/` | The elaborator and checker. `link.rs` links a strategy against the class catalog and its libraries, instantiates the library templates at every call, and generates the bar views, the per-calendar universes, the instruments table, the series and catalog reads and the clock; `rule.rs` is the per-rule pass (a well-moded order, B and M; T; F; D; G; W7); `config.rs` expands the `execution` and `report` blocks (P); `types.rs` is the dimensional algebra; `dof.rs` counts degrees of freedom (W5, W6, W9); `mod.rs` holds the workspace, the codes, the program-level judgments and the strata. |
| `crates/abt-kernel/src/compiler/`, `analysis/`, `rewrite/`, `plan/`, `lower/` | Normalise (the normal form and the program hash, `abt hash`) and the stage printing behind `abt ir`; Analyse (strata, the open-loop partition, keyedness, demand, window lengths, the abstract interpretation); the rewrites R1 to R6 with their log; the planner and the lowering the kernel executes (`abt show plan`). |
| `crates/abt-kernel/src/verify/`, `contract/` | The verification tiers (`abt verify`) and the contract derived from a program (`abt contract`, K-3). |
| `crates/abt-kernel/src/kernel/` | The kernel: `bind.rs` (Bind and the client of the data layer), `fold.rs` (the fold over the stream), `eval.rs` and `eval_set.rs` (rule bodies under Kleene's three-valued conjunction, over blocks of bindings), `rows.rs` (the stored tables' blocks), `pushdown.rs`, `events.rs` (the event log), `outputs.rs` (`kernel.<run>.*`), `checkpoints.rs`, `service.rs` (the kernel service), `mod.rs` (the kernel, `ExecConfig::from_program`, the causality check), `time.rs`, `value.rs`. |
| `crates/abt-kernel/src/sim/`, `functions/` | The market simulation behind the `Executor` trait (`SimExecutor`): equities, futures, options, the margin models and the limits; the pricing functions (Black–Scholes–Merton, Black 1976, Bjerksund–Stensland 2002, implied volatility). |
| `crates/abt-kernel/src/query/`, `state/`, `observe.rs` | The query language (parse, check, answer), the state tables and the provenance of a run, the program graph and the coverage report. |
| `crates/abt-kernel/src/data.rs`, `bundle.rs`, `table.rs` | The Parquet loaders and the bundle format that `bundle_v2` imports with, the deterministic synthetic markets of the three classes (`synthetic_daily`, `synthetic_minute`, `synthetic_futures`), and Parquet tables. |
| `crates/abt-kernel/src/cli.rs`, `kernel_cli.rs` | The kernel's command line and `abt kernel ...`. |
| `crates/abt-query/src/` | The query layer: `plan/` (parse, normal form, the executor, claims, lineages, trials), `metrics/`, `tests/` (the framework, dependence, resampling, HAC, the starting set), `feed.rs`, `activation.rs`, `report.rs`, `briefs.rs`, and `kernel.rs` and `data.rs`, its clients of the other two. |
| `crates/abt/src/` | The front: `main.rs` dispatches, `lib.rs` wires the layers in one process, `data_cli.rs` and `query_cli.rs` are the data and query layers' commands. |
| `corpus-v2/catalog/` | The three classes a program is written against: `equity`, `future` and `series`. A program never names them; its `data` block names what it needs. |
| `corpus-v2/lib/` | The standard library of V-7, one module a file (`calendar`, `catalog`, `stat`, `bars`, `xsec`, `state`, `book`), and `execution.dsl`, the presets `equity_default`, `book_fixed_base` and `futures_fixed_base`. |
| `corpus-v2/strategies/`, `company/`, `negative/` | 24 strategies that must check clean; MW14 and R8L as V-9 gives them; 35 negative cases, one or more per judgment code, each file's `# expect:` header asserted by `crates/abt-tests/tests/corpus_v2.rs`. |
| `crates/*/tests/`, `crates/abt-tests/` | Each crate's own suites, and the suites that span layers with the fixtures they share (`crates/abt-tests/src/lib.rs`: the catalog and the library as the base workspace, a strategy skeleton, crafted markets). Those that drive the `abt` binary are the front's, `crates/abt/tests/`. |
| `reference/`, `briefs/`, `experiments/` | The pandas reference engine, the plain-language briefs, and the material of the two model studies (records). |
| `scripts/` | `parity/` holds the books' comparisons with their references; `reviewed/` the reviewed exceptions of the S&P 500 lake and their editor; `bench.sh` the performance ladder and A-7.3's table; `synth/` writes the first formulation's layouts and is kept for the record. |

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

## Status

The migration of `docs/spec/05-migration.md` has landed through M-9:
- Step 0 merged the specification and set up the golden runs.
- M-0 to M-8 built the workspace, `abt-agg`, the data layer, the second
  formulation over it, contracts and the kernel's outputs, the query layer,
  the resolver and the vendors, options and futures, and the performance
  work.
- M-9 retires the first formulation's commands.

M-8's entity-parallel strata were not built (ERRATA E-77 says why). M-10,
the live interface, is next.

What this tree cannot run is the owner's:
- G1's parity runs, against `~/data/ref`;
- the vendors' real data and sessions: a recorded Databento session, the
  lake rebuilt from Norgate, ORATS's chains;
- G10 on the reference machine;
- the usability and observability studies, rerun on the second formulation
  (A-7.2).
