# absolute-backtest

A strategy DSL for equities backtesting, implemented exactly as fixed by
[`docs/semantic-model.md`](docs/semantic-model.md): a typed intermediate
representation, a well-formedness checker with one diagnostic code per
judgment, and a kernel that evaluates a checked program bar by bar in a closed
loop with a simulated executor. One Rust crate, no dependencies.

```
abt check corpus/                                   # every strategy and library in the corpus
abt run --strategy momentum_top_n --synthetic corpus/   # backtest on a synthetic market
abt run --strategy sma_crossover --data ./market corpus/ --verify-causality
abt run --strategy momentum_top_n --synthetic --all --fills corpus/  # every decision, and the fills
abt run --strategy breakout_52w --synthetic --param hold=21d --param qty='50 shares' corpus/
abt explain --strategy breakout_52w --rule 'decide#1' --at 2023-02-24 --synthetic corpus/
abt explain --strategy breakout_52w --rule 'decide#2' --at 2023-02-24 --bind A=SPY --synthetic corpus/
abt explain --strategy breakout_52w --rule 'features::sma#1' --at 2023-02-24 --inputs SPY,20d,10 --synthetic corpus/
abt synth --env equities_1d --out ./market corpus/  # write a synthetic market as Parquet
```

Data is one Parquet file per primitive relation (`close.parquet`,
`volume.parquet`, ...), with columns named after the signature's arguments. Timestamps are bar close
instants: a 09:30 to 09:31 minute bar is labelled `09:31`, the last bar of a
session `16:00`, and a daily bar by its date. `abt synth` writes this
convention; minute data labelled by open time silently misaligns every
resampled bucket.

## Layout

| Path | What it is |
| --- | --- |
| `docs/semantic-model.md` | The v1 semantic model: domains, types, signatures, the seven literal forms, WF-1 to WF-10, the kernel contract, the causality theorem. The code cites it by section. |
| `docs/data-bundle.md` | The data bundle and validation program: closed data, the online fold kernel, the catalog, the bias audit, the study API, with the status of each section in this crate and the milestone that implements it. |
| `docs/formal-foundations.md` | The formal foundations (Oct 8, 2026): the language as stratified Datalog over the cross-section and synchronous dataflow over time, with every guarantee derived from established results, what is new, and the seven changes the theory asks of the semantic model. |
| `docs/language-v2.md` | The second formulation (Oct 8, 2026): a review of the MW14 and R8L changes against the foundations, and the proposal that follows: one data store per instrument class with every resolution derived, programs as typed blocks with no run-time flags, order-independent rule bodies with sound short-circuiting, a library of formal operators, and a query facility; MW14 and R8L rewritten in it. Not implemented; section 11 is the order. |
| `src/lexer.rs`, `src/parser.rs` | Surface syntax to IR. The parser never reorders literals. |
| `src/ir.rs` | The typed IR: dimension vectors, signatures with modes and the temporal key, rules, literals, units. |
| `src/check/` | The checker. `types.rs` is the dimensional algebra of section 2; `rule.rs` is the per-rule pass (U, E, B, M, T, F, D, X, C); `mod.rs` builds the scope and runs the program-level judgments (R, N, S, Z, W1 to W6); `dof.rs` walks a rule's literals for the degrees-of-freedom count. |
| `src/kernel/` | The kernel: `eval.rs` solves rule bodies top-down with memoisation; `executor.rs` is the executor behind a trait (`SimExecutor` is the simulated one with every realism model); `mod.rs` holds the kernel, the batch driver of section 7, `explain`, and the empirical causality check; `fold.rs` is the same evaluation driven by an availability-ordered event stream with barriers (`docs/data-bundle.md`, section 2); `time.rs` is calendar arithmetic and resolution buckets. |
| `src/data.rs` | Parquet environment instances and a deterministic synthetic market. |
| `src/table.rs` | Parquet tables: the one file format of every input and output outside a bundle's log. |
| `src/bin/abt.rs` | The command line. |
| `corpus/env` | Four environments: `equities_1d` (tier 1), `equities_1d_ext` (tier 2), `equities_1m`, and `equities_1d_v2`, the catalog of `docs/data-bundle.md` section 3 (prices as traded, `split`, `dividend`, `delisted`, `member`, `classification`, `ticker`). |
| `corpus/lib` | Feature libraries written in the DSL: `features` (@1d), `features_m` (@1m), `bars` (@1m resampled to @1d), `catalog` (total return and a point-in-time adjusted close over `equities_1d_v2`). |
| `corpus/strategies` | 23 strategies that must check clean, including the four canonical stylized-fact strategies, `dividend_capture` with its as-of join, `opening_gap` at @1m, `resampled_momentum` over @1m data at @1d and `total_return_momentum` over the catalog. |
| `corpus/negative` | 21 negative cases, one or more per judgment code; each file's `# expect:` header is asserted by `tests/corpus.rs`. |
| `tests/corpus.rs` | The corpus as the checker's test suite (section 8). |
| `tests/checker_messages.rs` | Diagnostics pinned exactly: one diagnostic per root cause, library diagnostics reported once, and the wording of the messages for builtins, wildcards and resolution mismatches. |
| `tests/kernel.rs` | Hand-computed executor outcomes, every corpus strategy run end to end, determinism, the causality theorem, runtime diagnostics. |
| `tests/data.rs`, `tests/cli.rs` | The Parquet loader's contract (duplicates, bar labels, empty files) and the command line's option validation. |

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
- A strategy's header lines are `env`, `uses`, `resolution`, `mode` and,
  optionally, `revises "<program hash>"`, which places it in the lineage of
  the program with that hash (see Studies).
- A signature marks each argument `+` (input, bound by the caller), `-`
  (output, bound by the call) or `@` (the temporal key, exactly one). A
  relation's resolution follows the signature (`@1d`); in a library or
  strategy it defaults to the unit's `resolution`.
- Literals carry units: `100 shares`, `5_000_000 USD`, `60 USD/share` (a
  `Price<USD>`, so `param floor : Price<USD> = 60 USD/share` compares with
  `close` and `sma`), `20d`, `3mo`, `1y`, `0.02`, `"SPY"` (a string: an
  `Equity` or a `Label` from the type its context expects, the way a bare
  integer is a Count or a Scalar; the checker resolves it, and a string with
  no typed context is a T error). A
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
  `resample(R(...) to @1d as T, min K, X = last(P))`, the as-of join
  `R(A, T0, X) asof T` (R's latest tuple keyed at or before T, at any
  resolution; T0 is bound causally), and the temporal
  builtins `prev(T, T1)`, `lag(T, N, T1)`, `month_start(T)`, `day_start(T)`,
  plus `T1 in window(T, N, min K)` / `prior_window` inside an aggregation.
  A builtin is not a relation, so `not month_start(T)` does not resolve; the
  idiom is `mstart(T) :- bar(T), month_start(T).` and then `not mstart(T)`.
- Decisions: `decide(T, buy(A, Q))`, `sell`, `short`, `cover` in delta mode;
  `target_weight(A, W)`, `target_quantity(A, Q)` in target mode. The kernel
  supplies `decided(T0, D)`, `position(A, T, Q)`, `cash(T, C)`,
  `nav(T, N)` (the book marked at T, before T's decisions) and
  `fill(A, T, Q, P)` at the decision resolution.

`abt check` also prints each strategy's degrees of freedom (`docs/data-bundle.md`,
section 6), the counts the study report will use:

```
momentum_top_n: degrees of freedom: 4 params, 0 literals, 5 rules (+ 2 library literals)
```

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
| T | WF-3 | dimensions balance; terms match signatures; constructors typed; a parameter's default lies within its ordered range; a string literal resolves to `Equity` or `Label` from its context (`=` only on both) |
| R | WF-4 | every positive cycle steps strictly back in time through `prev` or `lag` |
| N | WF-5 | `not R` only when R is complete; completeness propagates; reductions close |
| F | WF-6 | every temporal key is T or derived from T by a causal builtin; `decided` strictly earlier |
| D | WF-7 | `top` has `by`; the keys cover every identity column; the key is bound; no `first`/`last` outside resample |
| S | WF-8 | no cycle through `not` or an aggregate |
| Z, C | WF-9 | at least one decide; `mode` declared exactly once; constructors of that mode, in decide heads and in `decided` patterns; decide's T is a positive atom's key |
| X | WF-10 | `resolution` declared once; body atoms share the head's resolution; resample goes strictly finer to coarser with `min K`. The kernel's `position`, `cash`, `fill` and `decided` are at the strategy's decision resolution, so a library that reads them is usable only by strategies deciding at its resolution; the error names the strategy |
| W1, W2, W3, W4 | warnings | dead derived relation; unused parameter; declared relation that no rule defines (always empty); `top` whose identity columns are all bound by the outer rule (keeps every tuple, `by` is dead) |
| W5, W6 | warnings | a numeric literal inside a strategy's own rule is a degree of freedom the study counts (0 and 1 in any unit are structural and exempt; a library's literals are counted, never warned); an `Equity` literal names the security carrying that ticker at the bundle date, a snapshot rather than an identity |

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

Two drivers share one evaluator and one executor. The batch driver
(`Kernel::run`, `abt run`) walks the decision bars of a fully loaded
dataset. The fold (`kernel::run_fold`, `abt run --kernel fold`;
`docs/data-bundle.md`, section 2) is `state' = step(state, event)` over the
dataset's event log: every primitive tuple available at its own bar's close
(the v1 convention), ordered by availability, then relation; a tuple's
arrival opens the buckets it falls in at every resolution the program needs
and closes the ones before them, and the barrier of a decision bucket runs
the executor's fill of the previous bar's orders, the bar's open (actions,
mark, margin), the strategy's decisions and the executor's take, in the
batch driver's order. Because every temporal builtin looks back only, a
time domain that grows as buckets open is observationally the up-front one,
and `tests/fold.rs` requires the two drivers to agree to the bit on every
corpus strategy; a fold stopped at `t` equals the batch run on the dataset
truncated at `t`, which is the causality theorem operationally. The
executor is a trait (`kernel::Executor`: `open_bar`, `on_decisions`,
`fill`, `finish`), so a live run attaches another executor to the same
fold.

A windowed aggregation keeps, per group (the rule, the aggregation and the
outer bindings its conjunction reads, including the window's length and
minimum), the rows of every bar it has solved, so a rolling feature solves
each bar once and evicts bars that leave the window; the rows are
re-based on the current call's bindings and sorted as a whole solve would
sort them, so the result is the uncached evaluation to the bit (`tests/fold_windows.rs`
proves it on every corpus strategy, on both drivers; `ExecConfig::window_cache`
turns the cache off only for that proof). A group whose conjunction reads
the window's base time, or has more than one window, is solved whole.
Resampled buckets and carried values (WF-4 recursion) are already evaluated
once per bucket or bar through the memo. `RunResult.stats` counts the
windowed aggregations, the bars solved and the cache's size.

The fold's state is serialisable (`kernel::Checkpoint`: the facts, the time
domains, the windowed groups' rows, the last prices, the executor's book
through the `Executor` trait's `checkpoint` and `restore`, the run so far,
and the cursor the replay resumes from; derived values are recomputed from
the facts). `abt run --kernel fold --checkpoint-every month|N
--checkpoint-dir DIR` writes one at the end of every calendar month (or
every N bars) of the decision bars, taken after the bar's barrier and
before the tuple that closed it is stored, as `DIR/<bar>.json`; `--resume
FILE` restores it over the same data and configuration (a fingerprint of
the program, its parameters and the configuration, and the dataset's
symbols, are checked) and replays the events from the cursor on.
`tests/fold_checkpoint.rs` requires a run resumed from any monthly
checkpoint to equal the unbroken fold and the batch kernel to the bit.

A tuple may carry its own availability time (`Dataset::add_available`, an
`available_at` column in a loose Parquet file or in a bundle's partitions, whose manifest
then says `recorded` rather than `bar_close`): the fold reads it from then
on, so a tuple that arrived after its bar closed is late in the backtest
too, exactly as section 2 of the data-bundle doc wants. The stream's
buckets close when its availability passes their end, never because of a
key; a late tuple is stored at its own key, everything derived from that
key on is recomputed on demand (the memo is dropped and the windowed
groups forget those bars), and decisions already emitted stand. The batch
kernel is blind to availability, so with late tuples the two drivers
differ and the fold is the one that is right; `Dataset::truncated` keeps
what was available at `t`, and `verify_causality` compares the fold's
decisions at sampled bars with the batch kernel's on that subset, which is
the v2 theorem. Nothing is ever available before its own bar.

Evaluation is top-down: `decide(t, D)` is requested for each bar `t` of the
decision resolution's time domain, and every derived relation is requested
with its temporal key and inputs bound and memoised by them. Because WF-4
makes all positive recursion strictly time-decreasing and WF-8 keeps
negation and aggregation acyclic, every request terminates and the result is
the unique model of section 7 restricted to what the decisions need. The
partial-arithmetic halt follows the same restriction: a degenerate tuple
halts the run when a decision demands it, and a tuple no decision requests
is never evaluated.

The executor has four policies, all set in `ExecConfig` and on the command
line, and every default halts the run with a diagnostic naming the bar, the
decision and its rule (`RunError::Risk`): `on_ruin` (equity at the execution
bar not positive while orders are pending: `halt` or `continue`, where a
positive `target_weight` of a non-positive equity targets flat); `on_leverage`
(a fill that would make cash negative or gross exposure exceed equity: `halt`,
`reject` with a reason, or `allow`); `on_oversize` (a `sell` beyond the long
or a `cover` beyond the short: `halt`, `clamp` at the position dropping the
remainder, or `allow` the signed order); and `lot` (`whole`, truncating every
order's quantity toward zero, or `fractional`). Within a bar, orders that
reduce a position fill first, so a rebalance funds its buys with its sells,
and a reducing order is never leverage. A liquidation whose fill bar has no
price for the instrument fills at the last known price, flagged on the fill
(`--fills` prints `(last price)`); an opening or adding order without a price
is dropped. Most backtesters reject an order beyond buying power and continue;
the halt default is stricter on purpose, so that a backtest cannot look
plausible on a book the model never defined.

The executor fills a bar's decisions at the next bar's close, at that bar's
price adjusted by the cost model of `ExecConfig` (below), then writes `fill`,
`position` and `cash` at that bar and `decided` at the decision bar. In
target mode the order is the difference between the target and the position
at execution; `target_weight` sizes from cash plus marked positions at the
execution bar, rounded by the configured lot, and a long that is bought is
sized at the price it will fill at, so the cash it spends is the weight of
equity.

The cost model (`docs/data-bundle.md`, section 5) has conservative non-zero
defaults: a commission of 0.005 per share with a 1.00 per-order minimum, a
regulatory fee of 0.278 basis points on the notional of sells, and slippage
against the order of a fixed part (0 basis points) plus 0.1 times the
instrument's realized volatility, the sample standard deviation of its log
returns over the 20 bars ending at the fill bar (fewer than 10 returns: the
fixed part only). Each fill records its commission, fee and slippage;
`RunResult.costs` sums them with the turnover, and `abt run` prints the line.
A bar's transaction costs are never leverage: a fully invested book stays
fully invested after paying them, carrying a debit of at most the bar's
costs, which the next sizing sees. `ExecConfig::frictionless()` (or
`--frictionless`) turns every model off, and the run then carries a warning
per model naming the bias it leaves unmodeled (`RunResult.warnings`, printed
as `warning (slippage): ...`).

The liquidity model (same section) caps a fill at 0.1 of the bar's volume
(from the `volume`-like primitive at the decision resolution, or
`--volume-relation`; a coarser decision bar sums the fine volumes in its
bucket). A delta order's remainder expires, reported as dropped with
`partial fill: 4 of 10 shares (participation cap 10% of volume 40)`; a
target's remainder re-issues itself at each following bar, sized afresh,
until the target is reached or a new decision on the instrument supersedes
it. Impact moves the fill price against the order by 0.1 times the square
root of the filled quantity over average daily volume (the mean bar volume
over the 20 bars ending at the fill bar). Each fill records its impact,
participation and whether it was partial; `RunResult.liquidity` reports the
fill ratio (filled over requested, a re-issue counting its fills and not a
new request) and the mean and maximum participation, and a run with fills
above 5% of bar volume carries a `market-impact` warning. A cap can leave a
reduction partially filled while the bar's buys fill to their targets, which
is leverage: the leverage policy decides (halt by default; `reject` drops the
buy).

The margin and funding models (same section) are configuration too. Gross
exposure may reach `max_gross` times equity after a fill (1 by default: no
borrowing, as ruled for the executor policies; `ExecConfig::reg_t()` or
`--margin reg-t` is Reg T: 2x gross, 25% maintenance, orders beyond rejected
and logged). At every bar's mark, positive equity below `maintenance_margin`
of gross exposure is a margin call: halt by default, `liquidate` sells the
fraction of every position that restores maintenance at the bar's close
(fills flagged forced), `allow` carries on and the run counts the calls.
Funding accrues over the calendar time between consecutive decision bars at
annual rates: positive cash earns `cash_rate` (0 in v1 and warned as cash
management), a debit pays `margin_rate` (5%; a run that paid it is warned
that the rate is constant), short notional pays the borrow fee of its
average-daily-volume bucket (below 100k shares: not shortable, the order is
dropped with `not shortable: ADV ...`; below 1M: 300 bps; above: 25 bps, all
a proxy and warned) and earns `short_rebate` (0). `RunResult.funding` sums
the four; `RunResult.exposure` records cash, gross, net, equity and leverage
at every bar's mark, written as Parquet by `--nav FILE` for the study and
the reference engine. Two distinct decisions for one instrument at one bar halt the
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
        [--cash X] [--slippage-bps X] [--slippage-vol X] [--vol-window N]
        [--commission X] [--commission-min X] [--fee-bps X] [--frictionless]
        [--participation X] [--impact X] [--adv-window N] [--volume-relation REL]
        [--margin none|reg-t] [--max-gross X] [--maintenance X] [--on-margin-call halt|liquidate|allow]
        [--cash-rate X] [--margin-rate X] [--short-rebate X] [--nav]
        [--price-relation REL]
        [--on-leverage halt|reject|allow] [--on-oversize halt|clamp|allow]
        [--on-ruin halt|continue] [--lot whole|fractional]
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

## Environment instances as Parquet

`abt run --data DIR` loads one `<relation>.parquet` per primitive of the
strategy's environment; `abt synth` writes the same layout with typed
columns. Column names are the signature's arguments (case-insensitive);
a column may be typed (integer, float, date, timestamp) or text, a text
timestamp being `YYYY-MM-DD`, optionally followed by `THH:MM[:SS]` or
` HH:MM[:SS]` (`src/table.rs`). The loader enforces what the signature promises:

- The temporal key is stored as the label of the bar containing it at the
  relation's resolution (spec section 3: at @1d the trading date), so
  `2022-01-03T16:00:00` in `close.parquet` and `2022-01-03` in `universe.parquet`
  are one bar and share one time domain.
- A relation is a function of its identity columns (its inputs, its key and
  its entity-typed outputs; spec section 3): two rows for one identity with
  different value outputs are an error naming both rows, such as
  `close.parquet row 2: duplicate tuple for (AAA, 2022-01-03) with different
  outputs; row 1 already binds them`. A row identical to an earlier one is dropped.
- A cell that does not parse as its type and a missing column are errors
  naming the file and row.
- `securities.parquet` (`id,ticker,from,to`, `to` null while the ticker is
  still carried, and optionally the contract columns `multiplier`,
  `asset_class` and `commission_per_contract`) is the bundle's security table (`docs/data-bundle.md`,
  section 3): with it, every equity field is a security id (an unknown id is
  an error naming the row), the identity bundle tests run at load (one id
  carries one ticker at a time, one ticker is carried by one id at a time,
  every interval ends after it starts), and the `ticker(A, @T, S)` relation
  is derived from the table over the bars of `universe` rather than read from
  a file. Without a table the ticker is the id and `ticker` is derived from
  the symbols.
- A ticker literal in a program (`param bench : Equity = "SPY"`), a
  `--param` value, a `--bind` or an `--inputs` name resolves through the
  table to the security carrying that ticker at the bundle date: the data's
  last bar, or `--as-of DATE` (`ExecConfig::as_of`). A ticker nobody carries
  at that date is a configuration error naming the date, never a silent
  empty symbol. A security id is accepted anywhere a ticker is.
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
- **Ruin and leverage halt by default.** The model defines no margin; rather
  than borrow silently, the executor halts when a fill would make cash
  negative or gross exposure exceed equity, or when equity is not positive
  with orders pending. `--on-leverage reject|allow` and `--on-ruin continue`
  relax this per run.
- **An oversize delta order halts by default.** A `sell` larger than the
  long position or a `cover` larger than the short would cross zero; `clamp`
  fills up to the position and drops the rest, `allow` keeps the signed-order
  reading.
- **Liquidations fill at the last price.** An order that shrinks a position
  without crossing zero fills at the instrument's last known price when the
  fill bar has none, flagged as such, so a delisted holding can always be
  closed; opening or adding orders without a price still drop.
- **Whole shares by default.** Every order's quantity (a delta amount or a
  target's quantity) is truncated toward zero unless `--lot fractional`;
  literals stay real-valued, and this rounding is where a contract size for
  futures would later apply.
- **Transaction costs are never leverage.** The leverage check exempts the
  bar's commissions, fees and slippage, so a book targeting weights that sum
  to one is not halted for paying its costs; the debit it carries is at most
  the bar's costs and the next sizing sees it. A bought long is sized at its
  expected fill price for the same reason; a reduction or a short is sized at
  the bar price it is marked at.
- **A capped target re-issues itself; a capped delta order expires.** The
  doc says so; the kernel keeps the open target per instrument, re-sizes it
  at each bar from that bar's equity and price, and drops it when a new
  decision names the instrument or the instrument has no price. Re-issues
  are fills without decisions: `decided` still holds only what the strategy
  emitted.
- **Corporate actions act on the book at the bar's open, before the mark.**
  A `split(A, T, F)` multiplies the position by F at its ex-date T; under
  whole lots the fraction is cashed at the bar's price. A `dividend(A, T0,
  Ex, Pay, Amount)` creates a receivable of Amount times the shares held at
  Ex (a short owes it), credited at Pay; the receivable is not marked, so
  equity dips between the ex-date and the pay date, and a pay date beyond
  the data is never credited. A `delisted(A, T, Reason)` force-closes the
  position at the first bar it holds, at the last trade less the haircut for
  the reason (`--haircut REASON=X`; bankruptcy and regulatory 1, acquisition
  and voluntary 0, anything else 1), with commission and no slippage, as a
  forced fill; a zero haircut on an involuntary reason is warned. Every
  action is in `RunResult.actions`.
- **Without a security table the ticker is the id.** A dataset that has no
  `securities.parquet` (the synthetic markets) is the v1
  world: equity fields are tickers, a ticker literal interns its own symbol,
  and `ticker(A, T, S)` is derived with every symbol as its own ticker, so
  `not ticker(...)` keeps its meaning. With a table, resolution is by bundle
  date and strict.
- **Ruin is not a margin call.** The maintenance check fires on positive
  equity below the margin of gross exposure; a non-positive equity is ruin
  and is judged by the ruin policy when an order comes to be filled, so a
  book that is already ruined and silent is not halted twice over.
- **A forced liquidation pays commission and fee, not slippage or impact**,
  and fills at the mark bar's close; it is the kernel's order, flagged
  `forced`, and never a decision of the strategy.
- **A non-positive price is a data error**, rejected at load with the file
  and row, except a future's (`asset_class = future`), whose back-adjusted
  series may cross zero.

## Bundles

`docs/data-bundle.md` (sections 2 and 3) makes data a versioned bundle the
system ships. A bundle is a directory: `manifest.json` (the environment name,
the version, the bundle date, every relation with its signature, resolution,
availability convention, row count and partitions, and the bundle tests'
report once they pass, and the layout `format`, now 2), `securities.parquet`
(the security table), and
`log/<relation>/<YYYY-MM>.parquet`, append-only Parquet partitions of each
primitive relation by the month of its temporal key (`snapshots/` is
reserved for the fold kernel's checkpoints). The derived `ticker` relation is
rebuilt from the table, never stored. A bundle of layout 1 (text security
table) is refused with a request to rebuild it.

```
abt bundle build --synthetic --env equities_1d_v2 --version 2026.10 --out ./bundle corpus/
abt bundle build --from ./market --env equities_1d --version 2026.10 --out ./bundle corpus/
abt bundle test ./bundle corpus/
abt run --strategy total_return_momentum --bundle ./bundle corpus/
```

`abt bundle test` runs the bundle tests of the doc's section 3 that the data
can answer today (identity; every temporal key a bar label; positive prices;
action reconciliation, where a close ratio outside [0.6, 1.67] between
consecutive bars must be a split that day and every split must show one;
delisting coverage, where a name that leaves the universe before the last bar
has a `delisted` record; membership, where a member is in the universe that
bar) and records a pass in the manifest; `abt run --bundle` refuses a bundle
that has not passed unless `--untested`, and then notes that its decisions
are not causality-certified. A strategy or library may name the version it
is written against, `env equities_1d_v2@2026.10`: the bundle must match, and
a library pinned to another version than its strategy is an E error. The
crate depends on `serde`, `serde_json`, `arrow` and `parquet` for this.

Vendor exports become bundles through adapters (`src/ingest.rs`): `abt
bundle build --from-norgate DIR` reads a Norgate-style daily layout
of Parquet files (`prices` as traded, `symbols` for the symbol history,
`splits`, `dividends`, `delistings`, `membership` and `classification`, the
last two expanded over trading days, and `exceptions`, reviewed bundle-test
exceptions; `scripts/ingest/norgate_lake_to_inputs.py` writes it from the
NDLake lake) into
`equities_1d_v2`; `--from-databento DIR [--processing-delay S]` reads a
Databento-style minute layout (`ohlcv-1m.parquet` keyed at the bar close
with an optional `ts_recv`, `symbology.parquet`) into `equities_1m` with every
tuple's availability recorded as its receipt time plus the delay. The
manifest records the source and the schema decisions of
`docs/data-bundle.md` section 10 (consolidated bars, the availability
offset as the processing delay).

## Company books

Two of the company's production strategies run in abt with their reference
results reproduced: `corpus/company/mw14.dsl`, the weekly S&P 1500 momentum
book (exact to its canonical weekly returns, 1991–2026), and
`corpus/company/r8l.dsl`, the intraday futures opening-range book (exact to its
legs and daily P&L, 2000–2026). How each maps onto the DSL, the run command,
the comparison and the known differences: `docs/parity-mw14.md`,
`docs/parity-r8l.md`; inputs and checksums: `docs/parity-inputs.md`. Their
data comes from `scripts/ingest/ndlake_weekly.py` (env `equities_1w`) and
`scripts/ingest/tradestation_sessions.py` (env `futures_sessions`), and
`scripts/parity/` holds the comparisons.

They use what the executor and the DSL gained for them: fixed-base
accounting (the default; `--compounding on` compounds), order types with a
time in force (`decide(T, target_weight(A, W, moc))`; `market`, `moo`,
`moc`, `moo_moc`, `limit(P, tif)`, `stop(P, tif)`), futures contracts in the
security table, `rows(T, N, min K)` windows over a group's own rows,
`rank(R(...), by (...), as K)`, `--actions in-prices`, `--delist-proceeds`,
`--dividends reinvest`, `--commission-bps`, `--start`/`--end`, and metrics in
both conventions (`--report-by day --report-calendar REL`,
`--periods-per-year`, `--returns FILE`).

## Studies

`docs/data-bundle.md` (sections 6 and 7) makes a backtest a counted trial. A
study directory (`--study DIR`) holds `lineages.json`, the declared studies
in `studies/` and the append-only trial log `trials.jsonl`:

```
abt study declare --study DIR --strategy NAME [--holdout trailing:2y] [--objective sharpe] [--require sharpe>=1] corpus/
abt study run     --study DIR --strategy NAME --synthetic --grid lookback=3mo,6mo,1y --grid n=2,5 corpus/
abt study run     --study DIR --strategy NAME --synthetic --grid n=2,5 --walk-forward anchored:2y:6mo corpus/
abt study reveal  --study DIR --strategy NAME --synthetic corpus/
abt study metrics --study DIR --strategy NAME corpus/
abt study report  --study DIR --strategy NAME corpus/
abt study dispute --study DIR --strategy NAME --reason "a different idea" corpus/
```

A strategy belongs to a *lineage*: its own when nothing like it exists, the
one it declares with a `revises "<hash>"` header line, or the nearest one
when its normalised rules are within Jaccard 0.8 of a member's (attached
with a warning; a dispute is logged and changes nothing). The program hash
(`abt study declare` prints it) is over the checked program with the
strategy's name and its variable names normalised away, so renaming resets
nothing. `declare` fixes the study's inputs (hold-out policy, objective,
thresholds, executor configuration) and warns about what is missing: no
hold-out, zero costs or slippage. A hold-out is `trailing:Ny` (the last N
years are truncated away from every run) or `blocks:K:Nmo[:seed]` (K random
month-aligned blocks the runs cross, since the book's state must, but whose
metrics are withheld); `reveal` runs a committed version (one the study has
run) over the whole sample and reports the embargoed bars only, logged as
an out-of-sample trial and counted. `run` logs one trial per grid point on
the sample the hold-out leaves, then reports the deflated Sharpe ratio of
the best point over the lineage's whole trial count, the probability of
backtest overfitting over the grid, the parameter surface around the best
point (smoothness, and the share of its grid neighbours within 10 % of it)
and the best point's Sharpe ratio per calendar year, with warnings for a
short sample, a Sharpe ratio below its minimum track-record length,
deflation below 0.95, PBO at or above 0.5, a peak rather than a plateau,
and a sign that flips across years. With `--walk-forward anchored:2y:6mo`
(or `rolling`), each fold picks the grid's best point on its train window
and judges it on the test window, every evaluation a logged trial, and the
test windows are stitched into one out-of-sample curve with Pardo's
walk-forward efficiency (mean out-of-sample over mean in-sample CAGR). The metrics library behind it is `src/study/metrics.rs` (its
conventions are in the module notes); `corpus/lib/metrics.dsl` writes the
book's return, running peak, drawdown and trailing volatility in the DSL
over the executor's `nav`. A plain `abt run` is an untracked trial: logged
when it names `--study DIR`, warned either way. `report` reads the lineage
back as a bias checklist: its members and disputes, the studies declared,
the trial count by kind (in studies, untracked, reveals) and the study runs
(`runs.jsonl` keeps each run's DSR, PBO, surface, efficiency and warnings),
the degrees of freedom, the latest trial and every reveal, the author's
thresholds judged on the latest trial and the latest reveal, every warning
raised with its bias and how often (the checker's W5 and W6 included), and
the forty biases of `docs/data-bundle.md` sections 4 to 6 with their status
and the rows a warning touched marked.

## The reference engine

`reference/` is a second implementation of the execution contract in plain
Python over pandas, written from `docs/semantic-model.md` section 6 and
sharing nothing with the kernel (`docs/data-bundle.md`, section 8). `abt run
--dump DIR` writes what a run saw and did (the data as Parquet, the
configuration, decisions, fills, dropped decisions, actions, the book at
every bar and the final state); `python3 reference/diff.py DIR` replays the
decisions through the reference executor and requires fills identical in
quantity (exactly under whole lots) and in price to floating tolerance, the
book identical at the end, the NAV within tolerance at every bar and the
same dropped count. `tests/reference.rs` runs every daily corpus strategy,
policy and model variants and the four action markets through it, skipping
with a note where python3 with pandas is absent. `.github/workflows/ci.yml`
is the GitHub Actions workflow (format, clippy, tests with pandas installed
so the comparison runs).

## Stylized facts

`corpus/strategies/` carries the four canonical strategies of
`docs/data-bundle.md` section 8, each long-short with four tenths of equity a
leg (slack for the legs' drift under the default 1x gross policy) and a
monthly rebalance over the catalog environment: `momentum_12_1`,
`low_volatility`, `short_term_reversal` and `size_proxy` (market
capitalisation is not a catalog relation, so average dollar volume stands
in). `tests/stylized.rs` shows them trading both legs on the synthetic
catalog market, and, when `ABT_BUNDLE_DIR` names a bundle of the
point-in-time universe (`ABT_STYLIZED_N` sets the names a leg, 50 by
default), runs them with the default cost model and checks wide published
ranges for the long-short return, its volatility, the deepest drawdown and
the known failure years (momentum's 2009, low volatility's 2020): gross
execution or data errors, not strategy merit. Without a bundle the ranges
are skipped with a note.

## Briefs

`briefs/` holds plain-language briefs for the LLM-authored corpus
(`docs/data-bundle.md`, section 8) and the procedure: a model writes the
strategy from a brief, its successive attempts are kept as
`attempts/<brief>/<n>.dsl`, and `abt briefs report --briefs briefs
--attempts attempts [--out report.json] corpus/` records, per brief, whether
the first attempt checked clean, the diagnostics of every attempt, the
attempts to a valid program and the first valid one's verdict on the
synthetic market under the default cost model (trades, does not trade, does
not survive costs, halted), and across briefs the first-attempt pass rate,
the distribution of diagnostics and the mean attempts to a valid program.
Measured and stored, never asserted.

`docs/llm-usability.md` reports the study of whether an LLM can write the
company books from the docs alone: Haiku 4.5 and Sonnet 5.5 agents given only
the language description, a plain-language spec, the checker and the data
wrote MW14, R8L and two briefs. Sonnet's files land within rounding of both
books; Haiku's check clean and never trade. Every attempt, the scores, the
root causes and the ranked fixes are in `experiments/llm-usability/`.

## Development

```
cargo test            # type algebra, time, corpus, checker, kernel, syntax, Parquet loader, command line
cargo build --release
```

Adding a corpus case: a strategy goes in `corpus/strategies/` and must check
clean: no errors, and no warnings beyond those it allows itself with a
`# allow: <code>` header line (one per code, warnings only; the four
strategies with an `Equity` param allow W6, and a header whose code is not
raised fails the test). A negative case goes in `corpus/negative/` with a
`# expect: <code>` header and must produce errors of that code only.
`tests/corpus.rs` asserts all of this and that every judgment code has a case.

## Program

The crate follows two design documents: `docs/semantic-model.md` (the v1
language and kernel, implemented) and `docs/data-bundle.md` (closed data
bundles, the online fold kernel, the catalog, the bias audit and the study
API), whose status table names the milestone each section lands in: M0
section 9 and the warnings W5, W6; M1 execution realism; M2 catalog,
identities and the bundle format; M3 the fold kernel and availability time
(all done); M4 the study API (done); M5 the realism program (done: the
pandas reference, the stylized-fact tests, the briefs harness and the vendor
adapters).
