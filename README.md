# absolute-backtest

A strategy DSL for equities backtesting, implemented exactly as fixed by
[`docs/semantic-model.md`](docs/semantic-model.md): a typed intermediate
representation, a well-formedness checker with one diagnostic code per
judgment, and a kernel that evaluates a checked program bar by bar in a closed
loop with a simulated executor. One Rust crate, no dependencies.

```
abt check corpus/                                   # every strategy and library in the corpus
abt run --strategy momentum_top_n --synthetic corpus/   # backtest on a synthetic market
abt run --strategy sma_crossover --data ./csv corpus/ --verify-causality
abt run --strategy momentum_top_n --synthetic --all --fills corpus/  # every decision, and the fills
abt run --strategy breakout_52w --synthetic --param hold=21d --param qty='50 shares' corpus/
abt explain --strategy breakout_52w --rule 'decide#1' --at 2023-02-24 --synthetic corpus/
abt explain --strategy breakout_52w --rule 'decide#2' --at 2023-02-24 --bind A=SPY --synthetic corpus/
abt explain --strategy breakout_52w --rule 'features::sma#1' --at 2023-02-24 --inputs SPY,20d,10 --synthetic corpus/
abt synth --env equities_1d --out ./csv corpus/     # write a synthetic market as CSV
```

Data is one CSV per primitive relation (`close.csv`, `volume.csv`, ...), with
a header row naming the signature's arguments. Timestamps are bar close
instants: a 09:30 to 09:31 minute bar is labelled `09:31`, the last bar of a
session `16:00`, and a daily bar by its date. `abt synth` writes this
convention; minute data labelled by open time silently misaligns every
resampled bucket.

## Layout

| Path | What it is |
| --- | --- |
| `docs/semantic-model.md` | The v1 semantic model: domains, types, signatures, the seven literal forms, WF-1 to WF-10, the kernel contract, the causality theorem. The code cites it by section. |
| `src/lexer.rs`, `src/parser.rs` | Surface syntax to IR. The parser never reorders literals. |
| `src/ir.rs` | The typed IR: dimension vectors, signatures with modes and the temporal key, rules, literals, units. |
| `src/check/` | The checker. `types.rs` is the dimensional algebra of section 2; `rule.rs` is the per-rule pass (U, E, B, M, T, F, D, X, C); `mod.rs` builds the scope and runs the program-level judgments (R, N, S, Z, W1, W2). |
| `src/kernel/` | The kernel: `eval.rs` solves rule bodies top-down with memoisation; `mod.rs` runs the executor loop of section 7, `explain`, and the empirical causality check; `time.rs` is calendar arithmetic and resolution buckets. |
| `src/data.rs` | CSV environment instances and a deterministic synthetic market. |
| `src/bin/abt.rs` | The command line. |
| `corpus/env` | Three environments: `equities_1d` (tier 1), `equities_1d_ext` (tier 2), `equities_1m`. |
| `corpus/lib` | Feature libraries written in the DSL: `features` (@1d), `features_m` (@1m), `bars` (@1m resampled to @1d). |
| `corpus/strategies` | 17 strategies that must check clean, including `opening_gap` at @1m and `resampled_momentum` over @1m data at @1d. |
| `corpus/negative` | 21 negative cases, one or more per judgment code; each file's `# expect:` header is asserted by `tests/corpus.rs`. |
| `tests/corpus.rs` | The corpus as the checker's test suite (section 8). |
| `tests/checker_messages.rs` | Diagnostics pinned exactly: one diagnostic per root cause, library diagnostics reported once, and the wording of the messages for builtins, wildcards and resolution mismatches. |
| `tests/kernel.rs` | Hand-computed executor outcomes, every corpus strategy run end to end, determinism, the causality theorem, runtime diagnostics. |
| `tests/data.rs`, `tests/cli.rs` | The CSV loader's contract (duplicates, bar labels, empty files) and the command line's option validation. |

## The surface syntax in one page

```
environment equities_1d {
  close(+A: Equity, @T: Timestamp, -P: Price<USD>) @1d
  volume(+A: Equity, @T: Timestamp, -V: Quantity<Shares>) @1d
  universe(-A: Equity, @T: Timestamp) @1d complete
}

library features {
  env equities_1d
  resolution @1d
  rel sma(+A: Equity, @T: Timestamp, +N: Duration, +K: Count, -M: Price<USD>)
  sma(A, T, N, K, M) :- universe(A, T),
      M = mean(P) over (T1 in window(T, N, min K), close(A, T1, P)).
  rel flat(+A: Equity, @T: Timestamp)
  flat(A, T) :- universe(A, T), not position(A, T, _).
}

strategy sma_crossover {
  env equities_1d
  uses features
  resolution @1d
  mode delta
  param fast : Duration = 20d in 10d..60d
  param qty : Quantity<Shares> = 100 shares
  rel above(-A: Equity, @T: Timestamp)
  above(A, T) :- universe(A, T), sma(A, T, fast, 12, F), sma(A, T, 50d, 30, S), F > S.
  decide(T, buy(A, qty)) :- above(A, T), prev(T, T0), below(A, T0), flat(A, T).
}
```

- Variables start with an uppercase letter; parameters, relations and
  keywords are lowercase; `_` is a wildcard (output positions only, which
  include the bound position of `prev` and `lag`: `prev(T, _)` holds when T
  has a bar before it).
- A signature marks each argument `+` (input, bound by the caller), `-`
  (output, bound by the call) or `@` (the temporal key, exactly one). A
  relation's resolution follows the signature (`@1d`); in a library or
  strategy it defaults to the unit's `resolution`.
- Literals carry units: `100 shares`, `5_000_000 USD`, `60 USD/share` (a
  `Price<USD>`, so `param floor : Price<USD> = 60 USD/share` compares with
  `close` and `sma`), `20d`, `3mo`, `1y`, `0.02`, `"SPY"` (an equity). A
  bare `60 USD` is a `Notional<USD>`. A bare integer is a Count or a Scalar
  from context; a bare decimal is a Scalar. A number is digits with optional
  `_` separators, an optional fraction with digits on both sides of the
  point, an optional exponent (`1e5`, `2.5e-3`) and an optional leading `-`;
  `.5` and `+0.5` are not numbers. Durations are whole numbers of `d`, `w`,
  `mo` or `y`.
- Body literals, in the order written: positive atom, `not` atom, comparison,
  `X = expr` (including `TE = T`, which copies a bound time into a value
  column such as an entry date; the copy is a value, not a temporal key),
  `X = agg(e) over (...)`, `top(N, R(...), by (K desc, A asc))`,
  `resample(R(...) to @1d as T, min K, X = last(P))`, and the temporal
  builtins `prev(T, T1)`, `lag(T, N, T1)`, `month_start(T)`, `day_start(T)`,
  plus `T1 in window(T, N, min K)` / `prior_window` inside an aggregation.
  A builtin is not a relation, so `not month_start(T)` does not resolve; the
  idiom is `mstart(T) :- bar(T), month_start(T).` and then `not mstart(T)`.
- Decisions: `decide(T, buy(A, Q))`, `sell`, `short`, `cover` in delta mode;
  `target_weight(A, W)`, `target_quantity(A, Q)` in target mode. The kernel
  supplies `decided(T0, D)`, `position(A, T, Q)`, `cash(T, C)` and
  `fill(A, T, Q, P)` at the decision resolution.

Every diagnostic names its code and judgment, for example:

```
error [N] bad_negation_incomplete_derived at 22:51 in rule ...::decide#2:
  `not selected` is not permitted: `selected` depends on `regime_on` depends
  on `close`: `close` is a primitive of environment `equities_1d` not declared
  complete, so a missing tuple is unknown, not false (WF-5 completeness)
```

## Reading the checker

| Code | Judgment | What the rule checks |
| --- | --- | --- |
| U | name resolution | relation or parameter declared in the strategy, a used library, or the environment; heads define relations declared in their own unit; one unit per (kind, name) in the workspace; `env` declared once; no builtin or keyword as a relation name |
| E | environment | the primitive belongs to the declared environment, not another one |
| B | WF-1 | every head, negated, compared or assigned variable is bound, left to right |
| M | WF-2 | `+` arguments bound at the call; `_` only in `-` positions; inside a resample, a fresh entity variable in a `+` position of a stored relation is bound by the grouping |
| T | WF-3 | dimensions balance; terms match signatures; constructors typed; a parameter's default lies within its ordered range |
| R | WF-4 | every positive cycle steps strictly back in time through `prev` or `lag` |
| N | WF-5 | `not R` only when R is complete; completeness propagates; reductions close |
| F | WF-6 | every temporal key is T or derived from T by a causal builtin; `decided` strictly earlier |
| D | WF-7 | `top` has `by`; the keys cover every identity column; the key is bound; no `first`/`last` outside resample |
| S | WF-8 | no cycle through `not` or an aggregate |
| Z, C | WF-9 | at least one decide; `mode` declared exactly once; constructors of that mode, in decide heads and in `decided` patterns; decide's T is a positive atom's key |
| X | WF-10 | `resolution` declared once; body atoms share the head's resolution; resample goes strictly finer to coarser with `min K`. The kernel's `position`, `cash`, `fill` and `decided` are at the strategy's decision resolution, so a library that reads them is usable only by strategies deciding at its resolution; the error names the strategy |
| W1, W2, W3 | warnings | dead derived relation; unused parameter; declared relation that no rule defines (always empty) |

Diagnostics are ordered by the dependency rank of the rule's head, so the
first error reported is the earliest offending relation; diagnostics about a
whole unit (a missing or mismatched environment or library, no decide rule)
come before any rule's. One root cause is one diagnostic: when the
environment or a used library is missing from the workspace, the checker
reports that once and does not report the names that may live there; a
library written against another environment is one E error on the `uses`
line; a relation that does not resolve binds its variables with unknown type
and unknown time, so nothing after it is judged against them; and when a
rule's head time is itself bound outside a temporal-key position, only the
head is reported, not every atom keyed by it.

## Reading the kernel

Evaluation is top-down: `decide(t, D)` is requested for each bar `t` of the
decision resolution's time domain, and every derived relation is requested
with its temporal key and inputs bound and memoised by them. Because WF-4
makes all positive recursion strictly time-decreasing and WF-8 keeps
negation and aggregation acyclic, every request terminates and the result is
the unique model of section 7 restricted to what the decisions need. The
partial-arithmetic halt follows the same restriction: a degenerate tuple
halts the run when a decision demands it, and a tuple no decision requests
is never evaluated.

The executor fills a bar's decisions at the next bar's close (slippage and
commission from `ExecConfig`), then writes `fill`, `position` and `cash` at
that bar and `decided` at the decision bar. In target mode the order is the
difference between the target and the position at execution; `target_weight`
sizes from cash plus marked positions at the execution bar, truncated to
whole shares. Two distinct decisions for one instrument at one bar halt the
run naming both rules; `x / 0`, `log` of a non-positive, `sqrt` of a
negative, `std` (or `cov`, `corr`, `ols_beta`) of one observation, `corr` of
a constant series, a `quantile` level outside [0, 1], and a non-positive delta
quantity halt it naming the rule, the tuple and the expression (for an
aggregate, the whole aggregate: `quantile(P, q) over (...)`). `median` of an
even count is the midpoint of the two middle values and `quantile` interpolates
linearly between order statistics, so `quantile(e, 0.5)` is the median. `cash` is populated at the
first bar with the initial cash so that cash-aware rules can fire from the
start.

A decision the executor cannot carry out is dropped with a reason in
`RunResult.dropped`: a decision on the last bar has no bar to fill at (`no
next bar`), and an instrument with no price at the fill bar cannot be filled
(`no price for AAA at 2022-02-09`); both are still recorded in `decided`.
`abt run` prints the counts and then each dropped decision with its reason,
so that `decisions = fills + dropped`, except that in target mode a decision
whose order is zero (the target is already held) makes neither a fill nor a
drop. `--all` prints every decision instead of the first twenty, `--fills`
prints the fills, and `--quiet` prints the summary only.

Parameters are the kernel's sweep axis (section 1) and the only values it
may vary between runs of one program (section 3): `ExecConfig.param_overrides`
replaces defaults by name (`hold`, or `unit::name` for a library's), and
`abt run` and `abt explain` take a repeatable `--param name=value` whose
value is a literal in the DSL's grammar (`21d`, `50 shares`, `0.02`, `"SPY"`
or a bare identifier for an equity). An override must be of the parameter's
declared type and within its declared range, or the run is refused before it
starts. An override also may not change what the checker judged on the
default: a `lag` length may not be overridden between zero and non-zero,
because WF-4 treats `lag` by a zero duration as causal rather than strict.

A decision the executor cannot fill (no price for the instrument at the next
bar) is reported as dropped, is still recorded in `decided`, and is not
retried by the kernel; whether the strategy retries it depends on how the
rule is written. A point test on history, `lag(T, hold, T0), decided(T0,
buy(A, _))`, is one-shot: `lag` is many-to-one and partial over the bar
domain, so it fires at most once per entry and never for an entry whose `T0
+ hold` falls on a weekend or holiday, and it can land on an older entry of
the same instrument. A test of the current state is retried every bar, which
is why the corpus writes every time-based exit in the window form:

```
decide(T, sell(A, Q)) :- held(A, T, Q), not bought_within(A, T, hold).
```

This sells at the first bar strictly later than `hold` after the entry,
closes every entry, and fires again if a fill was dropped.

`Kernel::explain(rule, t, inputs)` reports the first body literal with no
solution at `t`. `t` must be a bar of the rule's time domain (a weekend, a
date before the data, or a label between two resample buckets is refused
naming the nearest bars, since no run ever evaluates a rule there). A rule
with `+` arguments needs their values: `abt explain --inputs SPY,20d,10`
passes them in signature order, parsed by the signature's types, and asking
without them names the inputs the rule takes. The literal reported is the
first with no solution over *every* binding that survived the literals
before it, so for a multi-instrument rule it can be the literal that fails
for the last surviving instrument rather than for the one you are asking
about; `Kernel::explain_with(rule, t, inputs, bindings)` and `--bind A=SPY`
(repeatable; an equity, a timestamp, or a literal, a bare integer being a
Count) pre-bind body variables so the explanation is about that instrument.
`verify_causality` re-runs truncated instances for sampled bars and compares
`decide(t)`, which `tests/kernel.rs` does for seven corpus strategies.

## The command line

```
abt check <files...>
abt run --strategy NAME (--data DIR | --synthetic [--days N] [--symbols A,B,C] [--seed N])
        [--cash X] [--slippage-bps X] [--commission X] [--price-relation REL]
        [--verify-causality] [--quiet] <files...>
abt explain --strategy NAME --rule LABEL --at TIMESTAMP (--data DIR | --synthetic ...)
        [--price-relation REL] <files...>
abt synth --env NAME --out DIR [--days N] [--symbols A,B,C] [--seed N] <files...>
```

`<files...>` are `.dsl` files or directories searched recursively. The
synthetic market is seeded (`--seed`, default 7) and deterministic.
`--price-relation REL` names the primitive the executor fills at; by default
it is the `close`-like primitive at the decision resolution (or the finest
one below it, whose last tuple in the bucket is used). `REL` must be a
primitive with an equity argument and a `Price<...>` output no coarser than
the decision resolution; anything else halts the run before it starts
rather than dropping every order.

Every option value is validated: `--days abc`, `--cash lots`, `--symbols ""`
or `--at yesterday` are errors naming the option, never a silent default,
and an unknown `--option` prints the usage. `explain` at a timestamp that is
not a bar reports `2030-01-01 is not a bar of the @1d time domain (2022-01-03
to 2023-12-01)` instead of a missing literal. Exit codes: 0 success; 1 the
strategy does not check, the run halted, or the usage is wrong; 2 an input
could not be read or parsed (a source file, a data directory, an option
value).

## Environment instances as CSV

`abt run --data DIR` loads one `<relation>.csv` per primitive of the
strategy's environment; `abt synth` writes the same layout. The header names
the signature's arguments (case-insensitive), fields are comma-separated, and
a timestamp is `YYYY-MM-DD`, optionally followed by `THH:MM[:SS]` or
` HH:MM[:SS]`. The loader enforces what the signature promises:

- The temporal key is stored as the label of the bar containing it at the
  relation's resolution (spec section 3: at @1d the trading date), so
  `2022-01-03T16:00:00` in `close.csv` and `2022-01-03` in `universe.csv`
  are one bar and share one time domain.
- A relation is a function of its identity columns (its inputs, its key and
  its entity-typed outputs; spec section 3): two rows for one identity with
  different value outputs are an error naming both lines, such as
  `close.csv:3: duplicate tuple for (AAA, 2022-01-03) with different outputs;
  line 2 already binds them`. A row identical to an earlier one is dropped.
- A field that does not parse as its type, a header lacking a column and a
  short row are errors naming the file and line.
- A missing or header-only file leaves the relation empty and prints a
  `note:`; `--data DIR` must be an existing directory.

## Decisions taken where the model left room

These are the places where implementing the model required a choice; each is
small and easy to flip.

- **Head inputs are bound on entry.** WF-2 says the caller binds every `+`
  argument, so inside a rule's body the head's `+` variables are bound from
  the start. This is what makes `flat(+A, @T) :- universe(A, T), not
  position(A, T, _)` well-formed. The key and the outputs must still be bound
  by the body.
- **`universe` is declared `-A`.** The model's table writes `+A` for every
  primitive, but with `+A` on the domain relation no rule could ever range
  over the universe. The corpus environments declare `universe(-A, @T)`; the
  other primitives keep `+A`.
- **`position` and `fill` are enumerable (`-A`).** A strategy must be able to
  range over what it holds; the model's `+A` would forbid `held(A, T, Q) :-
  position(A, T, Q), Q > 0 shares`. Identity columns for WF-7 are therefore
  every entity-typed argument plus the key, not only the inputs, so a
  reduction over such a relation still has to name the entity as a tie-break.
- **A window may precede or follow the atoms it bounds.** Inside an
  aggregation, `T1 in window(T, ...)` is registered before the conjunction is
  judged, so an atom written ahead of its window (the model's bivariate
  example) is causal by the window's provenance; the kernel evaluates the
  window first either way, so the atom is a lookup, never a scan.
- **A reduction's key is bound before the `top`.** The group of a `top` is
  the bound outer variables, so `bar(T)` (or any atom at the head's
  resolution) precedes `top(n, candidate(A, T, M), ...)`; otherwise the group
  would be global and the declarative and operational readings would differ.
- **`day_start(T)`** joins `month_start(T)` as a builtin, so that an opening
  strategy at @1m can find the first bar of the day without a calendar
  primitive.
- **Windowed library features take their minimum count as an input**
  (`sma(+A, @T, +N, +K, -M)`), because every window must declare `min K` and
  there is no Duration-to-Count conversion.
- **`lag(T, 0d, T1)`** is causal rather than strict (it lands on T itself).
- **An unpriced position is marked at its last price.** When the price
  relation has no tuple for a held instrument at a bar (a delisting that
  removes its rows), the equity curve and `target_weight` sizing value it at
  the last price seen for it, its last close or fill, rather than at zero.
  The executor never trades at that stale price: a decision on the
  instrument is dropped with `no price for ...` until a price reappears.
- **A library is usable only on its own environment.** Section 4 says both
  libraries and strategies name the environment they are written against; a
  `uses` of a library written against another environment is one E error,
  even when the two environments share primitive names.
- **A resample groups a stored inner relation by every fresh entity
  variable**, including one in a `+` position (`resample(close_m(A, T1, P)
  to @5m as T, ...)` with `A` fresh yields one bucket per symbol), because
  section 4 says the form binds R's entity variables by grouping and a stored
  relation can be enumerated. A derived inner relation is a call: its `+`
  inputs must be bound before the resample (M), or it is declared with `-A`.
- **A duration parameter's range is judged under every calendar length**:
  a month is 28 to 31 days and a year 365 or 366, so `1mo in 31d..60d` is
  accepted and `1mo in 32d..60d` is a T error; the kernel still orders
  durations by their mean length.
- **`X = T` copies a bound Timestamp** into a value column (the "held since"
  idiom, `entry(A, T, E, TE) :- fill(A, T, Q, P), ..., TE = T`); the copy
  carries no causal provenance, so it compares freely but cannot serve as a
  body atom's temporal key.
- **`min K` removes a bar from a relation, not from the time domain.** The
  @1d domain over minute data is every day with at least one minute bar
  (section 6), so a half-day with fewer than the bars library's 300 bars
  has no `close_d` but is still a bar: `prev` from the next day lands on
  it, and a rule needing `close_d` at prev(T) does not fire on the day
  after it either. Skip bar-less days with `lag` or a window, or resample
  with `min 1` and a `count` output and gate on the count.
- **The executor prices from the data, not from the strategy's bars.** When
  the decision resolution is coarser than the price data, a decision is
  filled at the last fine close inside the next decision bucket, whatever
  `min K` the strategy's bar rules declare; a bucket with no fine tuple for
  the instrument drops the decision.

## Development

```
cargo test            # type algebra, time, corpus, checker, kernel, syntax, CSV loader, command line
cargo build --release
```

Adding a corpus case: a strategy goes in `corpus/strategies/` and must check
clean (no errors and no warnings); a negative case goes in `corpus/negative/`
with a `# expect: <code>` header and must produce errors of that code only.
`tests/corpus.rs` asserts both and that every judgment code has a case.
