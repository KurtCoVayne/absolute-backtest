# Strategy DSL Semantic Model

Oct 3, 2026 · John Gonzalez

This model fixes the v1 semantics of the strategy DSL (equities, bar data at one
or more resolutions, backtesting): domains, the dimensional type algebra,
relation signatures, the well-formedness judgments a program must satisfy, and
the kernel's contract. Every judgment maps to exactly one checker rule, and the
corpus of strategies and negative cases under `corpus/` is its test suite.

## 1. Scope and settled decisions

v1 covers equities only, for backtesting only, over bar data at a fixed set of
resolutions with fine-to-coarse resampling. Live trading, options, and
availability time are out of scope; section 9 records the agreed direction for
each. The artifact is a typed IR plus a checker plus a kernel, in one Rust
crate; the surface syntax and any host-language embedding are front-ends to the
same IR, and the LLM authors only the surface syntax, never host code.

| Area | Decision | Consequence |
| --- | --- | --- |
| Architecture | Typed IR + checker + kernel; surface syntax and embeddings are front-ends | One place holds the semantics; diagnostics come from the checker, not rustc |
| Authoring | The LLM emits human-readable surface syntax; the IR is the canonical checked form | Round-trippable; tooling works on the IR |
| Features | Feature library written in the DSL itself | Same checks apply to library and strategy; the library doubles as examples |
| Negation | Restricted closed-world: `not R(...)` only over relations judged complete | Missing data never satisfies a condition; see WF-5 |
| Time domain | The finite set of timestamps present in the data | `prev`/`lag`/`window` are the only time arithmetic; no `T + 1` |
| Durations and windows | Duration is calendar time (d, w, mo, y); every window declares a minimum observation count: `window(T, 60d, min 40)` | A window after a data gap holds fewer bars; below the minimum the aggregate yields no tuple |
| Literal order | Body literals are written in bindable order; the front-end never reorders | The checker reports the first unbound variable at its position; the LLM learns one rule, bind before use |
| Aggregates | `sum`, `mean`, `std`, `median`, `quantile`, `max`, `min`, `count`, `corr`, `cov`, `ols_beta`, `top`; a closed, kernel-defined set | No user-defined aggregates; extending the set is a kernel change |
| Partial arithmetic | `x / 0`, `log(0)`, `std` of one value halt the run with a diagnostic naming the rule and tuple | A degenerate feature is surfaced, never silently skipped |
| Cross-resolution | Fine-to-coarse resample only, with an explicit aggregate per value column and a minimum bucket count; no other cross-resolution join in v1 | A rule's body atoms all share the head's resolution (WF-10); the as-of join arrives in v2 with availability time |
| Availability | Every fact at resolution r is available at the close of its bar, a resampled bar at the close of its bucket; decisions at T are emitted after close T | `open(T)` at @1d cannot drive a decision at the open; a strategy that must act at the open decides at a finer resolution, where the first bar of the day is available at its own close |
| Position | `position` is a kernel primitive (executor feedback), distinct from any derived intended position | The strategy can observe when execution diverged from intent |
| History | `decided(T0, D)` is kernel-supplied, complete, strictly causal (T0 < T), scoped to the strategy, pattern-matchable on D | Cooldowns and time-based exits are expressible; cross-strategy visibility is not |
| Reductions | Every reduction names a total order with a tie-break | Evaluation is deterministic; `first`/`any` do not exist |
| Decision mode | A strategy declares one mode: delta (`buy`/`sell`/`short`/`cover`) or target (`target_weight`/`target_quantity`) | Rules mixing modes are rejected |
| Decision conflicts | Two distinct decisions for the same instrument at the same T are a runtime error | The kernel never picks one silently |
| Types | Dimensional analysis over base dimensions, financial types layered on top | `Price + Volatility` fails by dimension, not by lookup table |
| Escape hatch | None; no Float cast | Every value carries its dimension end to end |
| Parameters | First-class, typed, with optional ranges | The kernel can sweep them; no magic numbers inside rules |
| Primitives | `close`, `volume`, `universe`, `position`, `cash`, `fill` | Found by dependency closure over the corpus; `fill` promoted because the kernel's executor produces it for free |
| Instruments | Equities only | Instrument relations (underlying, strike, expiry) deferred |

## 2. Domains and dimensional types

Every value is either an entity, a time, or a dimensioned quantity; a
quantity's type is its dimension vector over three base dimensions, and the
financial names are aliases for specific vectors.

**Entity domains.** `Equity` (opaque identifiers, totally ordered by identifier
so that tie-breaks are deterministic), `Timestamp` (the finite, totally ordered
set of timestamps present in the data), `Duration` (calendar time in days,
weeks, months, or years; `20d` means 20 calendar days, and a window of `20d`
holds whichever timestamps are present inside it), `Count` (a non-negative
integer).

**Base dimensions.** Currency C (parameterised by code, USD), Share S, Time Θ.
A quantity type is a vector of integer or half-integer exponents (c, s, θ).

| Name | Dimension | Meaning |
| --- | --- | --- |
| `Scalar` | (0, 0, 0) | dimensionless; returns, z-scores, weights, correlations |
| `Price<USD>` | (1, −1, 0) | currency per share |
| `Quantity<Shares>` | (0, 1, 0) | shares; signed (negative = short) |
| `Notional<USD>` | (1, 0, 0) | currency |
| `Count` | (0, 0, 0), integer | cardinalities; not freely convertible to Scalar |
| `Duration` | (0, 0, 1) | calendar time: d, w, mo, y; the bars inside a window are whichever timestamps are present |

**Operator typing.** Let d(e) be the dimension of expression e.

- `e1 + e2`, `e1 − e2`, `e1 < e2`, `e1 = e2`: require d(e1) = d(e2) and the same currency code.
- `e1 * e2`: d = d(e1) + d(e2). `e1 / e2`: d = d(e1) − d(e2). So Price * Quantity = Notional, Notional / Price = Quantity, Price / Price = Scalar.
- `log`, `exp`: require and return Scalar. `log(P)` for a price is rejected; `log(P1 / P0)` is accepted.
- `sqrt`: halves every exponent; the result must have exponents in ½ℤ.
- `abs`, `least`, `greatest`: preserve the dimension; all arguments must agree.
- Literals carry units: `100 shares`, `5_000_000 USD` (Notional), `60 USD/share` (Price: currency per share), `252d`, `0.02`. A bare number is Scalar.
- Count converts to Scalar only through explicit division (`W = 1 / N` is accepted because 1 / Count is defined as Scalar); Count + Scalar is rejected.

**Aggregate typing.** `sum`, `mean`, `max`, `min`, `std` preserve the dimension
of their argument, as do `median`, `quantile`, `first`, and `last`. `count`
returns Count. `corr` requires two arguments and returns Scalar regardless of
their dimensions; `cov(e1, e2)` has dimension d(e1) + d(e2), and
`ols_beta(y, x)` has dimension d(y) − d(x). `std` of a Price is a Price; a
volatility in the usual sense is `std` of a Scalar return and is therefore
Scalar at the program's resolution. v1 does not scale volatility across
resolutions, so no Θ^−½ exponent is produced; that is reserved for v2.

**Aggregate conventions.** `median` of an even count is the midpoint of the
two middle values. `quantile(e, q)` takes a bound Scalar level q in [0, 1] and
interpolates linearly between order statistics at position q · (n − 1) of the
sorted group, so `quantile(e, 0.5)` equals `median(e)` on every count; a level
outside [0, 1] is partial arithmetic (section 7). `std`, `cov`, `corr` and
`ols_beta` are sample statistics (divisor n − 1) and need at least two
observations; `corr` of a constant series and `ols_beta` against a constant
regressor have no result. `sum`, `mean`, `max`, `min` and `median` are total on
a non-empty group.

**Time.** Timestamp admits only comparison (`<`, `<=`, `=`) and the builtins
`prev`, `lag`, `window`, `prior_window`. Timestamp − Timestamp is not an
expression in v1; a bar distance is obtained with `lag`. Timestamp-typed
arguments of a relation other than its temporal key (section 3) are ordinary
values and can be compared freely; a bound time is carried into such a column
by assignment (`TE = T`), which copies the value and none of its causal
provenance.

**What this buys.** The validity of an operation is decided by arithmetic on
exponents, so the type checker has no table of allowed pairs to maintain, and
the LLM gets one rule to learn: dimensions must balance.

## 3. Relation signatures

A relation is a finite set of typed tuples, and its signature carries five
things the checker needs: the argument types, a mode per argument, which
argument is the temporal key, the resolution, and whether the relation is
complete.

```
sma(+A: Equity, @T: Timestamp, +N: Duration, -M: Price<USD>) @1d
```

**Modes.** Each argument is an input (`+`) or an output (`-`). An input must be
bound at every call site; an output is bound by the call. A relation with
inputs is a parametric family, and it is finite only because its inputs are
bound to finitely many values by the caller. `sma` with N unbound would be
infinite, which is why modes are part of the signature and not an inference
the compiler may skip.

**Temporal key.** Exactly one argument is marked `@`. It is the availability
time of the tuple: the time at which the fact may be used by a decision.
Causality (WF-6) is judged on this argument only. Any other Timestamp argument
is data; `dividend_announced(A, @T, Ex, D)` carries its ex-date as a value, so
"buy the day before the ex-date" is a comparison on values and not a forward
reference.

**Resolution.** Every relation carries a resolution from the fixed set `@1m`,
`@5m`, `@15m`, `@30m`, `@1h`, `@1d`. Resolutions are ordered by alignment: r1
is finer than r2 when every r2 bar is a union of whole r1 bars within the
trading session, which holds along this chain. A timestamp at resolution r is
a bar label (the bar's close instant; at @1d, the trading date), so natively
supplied @1d data and data resampled to @1d share one time domain. The
resample form (section 4) is the only construct that changes resolution, and
only from finer to coarser; outside it, a rule's body atoms share the head's
resolution (WF-10).

**Completeness.** A relation is complete when the absence of a tuple means the
fact is false rather than unknown. Completeness is declared on primitives and
derived for everything else (WF-5). Negation is permitted only over complete
relations.

**Kinds.** Every relation is one of four kinds, and the kind decides who may
define it.

| Kind | Defined by | Examples | Complete? |
| --- | --- | --- | --- |
| Primitive | The environment; immutable | `close`, `volume`, `universe`, `position`, `cash`, `fill` | As declared |
| Kernel state | The kernel, from the strategy's own output | `decided` | Always |
| Derived | Rules in a library or strategy | `sma`, `held`, `flat`, `selected` | By WF-5 |
| Output | The strategy's decide rules | `decide` | Always (it is what the kernel evaluates) |

**Parameters.** A `param` is a named constant with a type and an optional range
(`param n : Duration = 20d in 5d..250d`). Within rules it behaves as a bound
value of that type. The range is ordered and contains the default (a default
outside it is a type error, WF-3; a calendar duration has no single length in
days, so `1mo` lies within `31d..60d` and outside `32d..60d`). Parameters are
the only values the kernel may vary between runs of the same program.

**Identity columns.** For reductions (WF-7) the checker needs to know which
arguments identify a tuple. v1 rule: the identity of a tuple is its
entity-typed inputs plus its temporal key; value outputs never identify. For
`mom_candidate(+A, @T, +Lookback, +Skip, +MinAdv, -M)` the identity at a fixed
T and fixed parameters is A.

## 4. Rules

A program is a set of rules `head :- body`, and a body is a conjunction of
seven literal forms; nothing else is a literal, so the checker's job is to
judge these seven.

| Literal | Form | Binds | Notes |
| --- | --- | --- | --- |
| Positive atom | `R(t1, ..., tn)` | R's output positions | Inputs must already be bound |
| Negated atom | `not R(t1, ..., tn)` | nothing | R must be complete (WF-5); all terms bound |
| Comparison | `e1 op e2` | nothing | op in `<`, `<=`, `=`, `>`, `>=`; both sides bound, same dimension |
| Assignment | `X = e` | X | e built from bound variables, params, literals, scalar functions |
| Aggregation | `X = agg(e) over (conj)` | X | conj is a conjunction of positive atoms, temporal constraints, and comparisons and assignments over variables bound inside it; e uses variables bound inside conj; variables shared with the outer rule are inputs to the group |
| Reduction | `top(N, R(...), by (k1 dir, ..., km dir))` | R's variables | Keeps at most N tuples per group of bound outer variables; the order must be total (WF-7) |
| Resample | `resample(R(...) to @r as T, min K, X1 = agg1(e1), ...)` | R's entity variables, the bucket label T, each Xi | R strictly finer than and aligned to @r; aggregates in `first`, `last`, `max`, `min`, `sum`, `mean`, `count`; a group below K yields no bucket (WF-10) |

**Aggregation semantics.** `agg(e) over (conj)` evaluates conj with the outer
bound variables fixed, collects the multiset of e over the resulting tuples,
and applies the aggregate. conj is judged like a rule body (WF-1 to WF-3,
WF-6): its atoms are positive atoms of the rule, and a comparison or an
assignment inside it filters or extends the group's tuples, so "since entry"
and "above a threshold" groups are written as `TE <= T1` or `Cost = P * Q,
Cost > limit` inside the conjunction. The group is empty when conj has no solutions;
`count` of an empty group is 0, every other aggregate over an empty group
yields no tuple (the rule does not fire). This is what makes `sma` undefined,
rather than zero, before the window is full. A windowed group holding fewer
observations than its declared minimum likewise yields no tuple; `min K` is
required on every window and has no default, so a 60-day average over three
points cannot be written by accident.

**Bivariate aggregates.** `corr(e1, e2) over (conj)` pairs e1 and e2 from the
same tuple of conj; the pairing is a join, so
`corr(RY, RX) over (logret(y, T1, RY), logret(x, T1, RX), T1 in window(T, 252d))`
aligns the two series on T1 by construction.

**Resampling semantics.**
`resample(R(...) to @r as T, min K, X1 = agg1(e1), ..., Xk = aggk(ek))` groups
the tuples of R by its entity columns and by the @r bucket containing each
tuple's temporal key, binds T to the bucket's label, and binds each Xi to aggi
over the group. `first` and `last` are ordered by the fine temporal key, so
they are deterministic; the other aggregates are as in section 2. A group with
fewer than K tuples yields no bucket. The result's temporal key is the bucket
label, available at the bucket's close, so a resampled tuple depends only on
fine tuples at or before it and WF-6 holds by construction. R may be a
primitive or a derived relation; a stored R is grouped by every entity
variable left fresh in its atom, whatever that argument's mode, while a
derived R is a call whose `+` inputs must be bound before the form (WF-2).
Standard bars are one rule each:

```
open_d(A, T, O)   :- resample(close_m(A, T1, P) to @1d as T, min 300, O = first(P)).
close_d(A, T, C)  :- resample(close_m(A, T1, P) to @1d as T, min 300, C = last(P)).
high_d(A, T, H)   :- resample(close_m(A, T1, P) to @1d as T, min 300, H = max(P)).
volume_d(A, T, V) :- resample(volume_m(A, T1, Q) to @1d as T, min 300, V = sum(Q)).
```

**Temporal builtins.** These are the only operations on Timestamp and all of
them are causal.

| Builtin | Meaning | Binds |
| --- | --- | --- |
| `prev(T, T1)` | T1 is the latest timestamp present strictly before T | T1 |
| `lag(T, N, T1)` | T1 is the latest timestamp present at or before T − N, with N a calendar duration | T1 |
| `T1 in window(T, N, min K)` | T − N ≤ T1 ≤ T over timestamps present; the group must hold at least K of them | T1 (inside an aggregation) |
| `T1 in prior_window(T, N, min K)` | T − N ≤ T1 < T over timestamps present; at least K of them | T1 (inside an aggregation) |
| `month_start(T)` | T is the first timestamp present in its calendar month | nothing |
| `day_start(T)` | T is the first timestamp present in its calendar day (meaningful below @1d) | nothing |

`prev` and `lag` fail (no tuple) when the data does not reach back far enough;
a rule using them does not fire for the first bars, which is the intended
behaviour rather than a warm-up special case. The position they bind is an
output, so it may be `_` (WF-2) when only the existence of the earlier bar
matters: `prev(T, _)` holds exactly when T has a bar before it.

`lag` is a function of T that is many-to-one and partial over the bar domain:
when T − N falls in a gap (a weekend, a holiday), every T whose T − N falls in
the same gap lands on the same T1, and a bar T0 is reached by `lag(T, N, T0)`
only when some bar T has T0 ≤ T − N < next(T0). A point test on history such
as `lag(T, hold, T0), decided(T0, buy(A, _))` therefore fires at most once per
entry and, for an entry whose T0 + N is not a bar, never; it can also land on
an older entry of the same instrument. A time-based exit is written as a
window test on the current state, which is evaluated afresh at every bar:

```
decide(T, sell(A, Q)) :- held(A, T, Q), not bought_within(A, T, hold).
```

with `bought_within` a complete derived relation counting `decided` buys over
`prior_window(T, hold, min 1)`. It fires at the first bar strictly later than
T0 + hold, for every entry, and again if the executor could not carry it out.

**Decisions.** The output relation `decide(@T, D)` takes a decision value D
built from one constructor of the strategy's declared mode:

- delta mode: `buy(A, Q)`, `sell(A, Q)`, `short(A, Q)`, `cover(A, Q)` with Q : Quantity<Shares>, Q > 0.
- target mode: `target_weight(A, W)` with W : Scalar, or `target_quantity(A, Q)` with Q : Quantity<Shares> (signed).

A decision is a value, not an instruction; the executor decides how, and
whether, it is carried out. Decision constructors may be pattern-matched in
`decided(T0, D)` with `_` wildcards and bound variables, e.g.
`decided(T0, buy(A, _))`.

**Libraries and strategies.** A library is a set of rules with no `decide`; a
strategy is a set of rules with at least one `decide`, a declared mode, and
parameters. Both name the environment they are written against, and a strategy
may use only libraries written against its own environment (judgment E
otherwise). A strategy may use any number of libraries; name resolution is
strategy, then libraries in `uses` order, then the environment's primitives,
then builtins.

## 5. Well-formedness judgments

A program is valid when every rule satisfies WF-1 to WF-4 and the program as a
whole satisfies WF-5 to WF-10; together they imply finiteness, determinism, and
causality, and each one is a single checker rule (section 8).

**WF-1 Range restriction.** Every variable in a rule's head, in a negated atom,
in a comparison, or on the right of an assignment is bound: it is a parameter,
a literal, an output of a positive atom in the body, the left side of an
earlier assignment or aggregation, or bound by a temporal builtin. Bound-ness
is checked left to right in the order written; the front-end never reorders
literals, and the diagnostic names the first unbound variable and its
position. With WF-2 this makes every derived relation finite: all values come
from finitely many primitive tuples through total functions.

**WF-2 Mode correctness.** At every call site, each `+` argument of the callee
is bound before the call. A wildcard `_` is permitted only in `-` positions.

**WF-3 Types.** Every expression has a dimension by the rules of section 2;
every atom's terms match the signature's types; every decision constructor's
fields match its declared types. There is no implicit conversion.

**WF-4 Temporal recursion.** Build the dependency graph on derived relations.
For every cycle, every rule on the cycle whose body refers to a relation on
that cycle must bind the referenced tuple's temporal key to a strictly earlier
timestamp than the head's, through `prev` or `lag`. Since the time domain is
finite and every step is strictly earlier, derivation depth is bounded by the
number of timestamps, so recursion terminates even when it creates values (an
entry price carried forward is the corpus example). A cycle without such a
step is rejected; there is no fixpoint iteration over value-creating rules.

**WF-5 Completeness.** Define complete(R) as the least relation satisfying:

- complete(R) if R is a primitive declared complete, or kernel state, or the output relation;
- complete(R) if R is derived and, for every rule of R, every positive atom in the body is complete (negated atoms are complete by WF-5 itself; comparisons and assignments do not affect completeness). The atoms inside an aggregation's conjunction and the inner relation of a resample are positive atoms of the rule for this purpose: `N = count(P) over (close(A, T1, P), ...)` makes its rule incomplete even though `count` yields 0 on an empty group, because a missing `close` leaves the count unknown, not small;
- complete(R) if every rule of R is a reduction rule (a `top` over any relation yields a complete relation: the kernel knows exactly which tuples it kept).

The judgment: `not R(...)` is permitted only when complete(R). The effect is
that a missing price can never satisfy a condition by absence; it can only
prevent rules from firing. The corpus case `regime_momentum` shows the rule's
teeth: a `selected` relation that includes a benchmark-dependent condition is
not complete, so `not selected(A, T)` would be rejected, and a missing
benchmark close cannot liquidate the book.

**WF-6 Causality.** Assign every atom in a rule a time term: the term in its
temporal key position. For a rule with head time T, every body atom's time
term must be provably <= T, where provability is syntactic: T itself, or a
variable bound by `prev(T, ·)`, `lag(T, ·, ·)`, `window(T, ·)`,
`prior_window(T, ·)`, or transitively from such a variable. For `decided`, the
bound must be strict (< T). No construct produces a later timestamp, so the
only way to violate WF-6 is to bind a time variable in a body atom's key
position to something not derived from T; that is rejected. Timestamp-typed
arguments that are not temporal keys are exempt. Causality is therefore
transitive through the dependency graph by construction, and the checker
reports the first rule in dependency order that fails, which is the "earliest
offending relation" the conceptual spec asked for.

**WF-7 Deterministic reduction.** Every `top(N, R(...), by (k1 d1, ..., km dm))`
must satisfy: N is bound; every ki is bound by R's atom; and the key list is
total on R's tuples at fixed outer bindings. Totality is decided syntactically:
the key list must include every identity column of R (section 3) that is not
bound by the outer rule. The checker does not accept an order that is total
only because values happen to be distinct. `first`, `any`, and `argmax`
without a tie-break do not exist in the language.

**WF-8 Stratification.** The dependency graph with negation and aggregation
edges marked must have no cycle through a negation or aggregation edge.
Temporal recursion through positive atoms is allowed by WF-4; recursion
through `not` or through an aggregate is not.

**WF-9 Decisions.** A strategy declares exactly one decision mode and every
decide rule uses constructors of that mode, as does every `decided` pattern
written in the strategy (`decided` holds only the strategy's own decisions,
so a pattern with the other mode's constructor could never match; a library
has no mode and its patterns are judged by the strategies whose decide rules
reach them, each reporting at the library's rule);
the head of a decide rule has the
form `decide(T, D)` with T a variable that is the temporal key of at least one
positive body atom; a strategy with no decide rule is rejected; and a decide
rule may not refer to `decided(T, ·)` at its own T (this is WF-6's strictness,
restated because it is the LLM's most likely mistake). Decision conflicts (two
distinct decisions for one instrument at one T) are not statically decidable
in general and are a kernel runtime error, never resolved by choice.

**WF-10 Resolution compatibility.** A rule's resolution is that of its head's
temporal key. Every body atom outside a resample form has the same resolution;
`prev`, `lag`, `window`, and `prior_window` range over that resolution's time
domain; a resample form's inner relation is strictly finer than, and aligned
to, the head's resolution, and the form carries a `min K`. A strategy declares
one decision resolution, and `decide`, `decided`, `position`, `cash`, and
`fill` are at that resolution. Two relations at different resolutions can meet
only through resample; there is no implicit alignment and no coarse-to-fine
direction in v1.

**Warnings, not errors.** A derived relation that no decide rule reaches is
reported as dead. A parameter never used is reported. A declared relation
that no rule defines is reported: it is always empty, so every rule reading
it positively can never fire. None of these affects validity.

## 6. Environment interface and kernel loop

The kernel supplies six primitives and one kernel-state relation, runs the
strategy bar by bar, and feeds the simulated executor's results back as
primitives at the next bar; that closed loop is the whole environment
interface.

| Relation | Signature | Complete | Supplied by |
| --- | --- | --- | --- |
| `close` | `(+A: Equity, @T, -P: Price<USD>)` | no | market data |
| `volume` | `(+A: Equity, @T, -V: Quantity<Shares>)` | no | market data |
| `universe` | `(+A: Equity, @T)` | yes | market data (membership as of T) |
| `position` | `(+A: Equity, @T, -Q: Quantity<Shares>)` | yes | executor |
| `cash` | `(@T, -C: Notional<USD>)` | yes | executor |
| `fill` | `(+A: Equity, @T, -Q: Quantity<Shares>, -P: Price<USD>)` | yes | executor |
| `decided` | `(@T0, -D: Decision)` | yes | kernel, from the strategy's own output |

Each market-data primitive comes at the native resolution of its source,
declared in the environment (`close` @1m and `close` @1d are different
relations and are named differently). The strategy declares its decision
resolution; the executor relations and `decided` are kept at that resolution,
and market data finer than it is reached through resample.

`universe` is load-bearing for every strategy in the corpus even when no rule
reads it for data: it is the complete domain relation that range-restricts
negation (`flat(A, T) :- universe(A, T), not position(A, T, _)`). An
environment without a complete entity domain cannot express "has no position".

**Execution contract (v1).** Decisions at T are emitted after close T. The
simulated executor fills them at close prev⁻¹(T), the next bar at the decision
resolution, at that bar's close price, optionally adjusted by a configured
slippage and commission model that is part of the kernel configuration and
not of the program. Consequently `position(A, T)` is the position held at
close T, after fills of decisions made at prev(T); `fill(A, T, Q, P)` records
those fills; `cash(T, C)` is cash after them; `decided(T0, D)` holds every
decision the strategy emitted at T0. A decision rule at T therefore sees the
result of its decision at prev(T), and never its own.

`decided` records what the strategy emitted, not what the executor did: a
decision the executor could not carry out (no price at the fill bar) is still
in `decided` and is not retried by the kernel. Whether it is retried is the
strategy's choice of exit form: a rule that tests `decided` at one point in
time (`lag(T, hold, T0), decided(T0, buy(A, _))`) is one-shot, while a rule
that tests the current state (`held(A, T, Q)`, `position`, `not
bought_within(A, T, hold)`) is re-evaluated at every bar until the state
changes, so a dropped exit is decided again at the next bar (section 4,
temporal builtins).

In delta mode a decision is a signed order; in target mode the executor
computes the order as the difference between the target and position at the
time of execution, using the bar's close for `target_weight`. Conflicting
decisions for one instrument at one T halt the run with a diagnostic naming
both rules.

When the decision resolution is coarser than the price data, the executor's
"close of the next bar" is the last fine close inside the next decision
bucket, for whichever fine tuples the data holds: the strategy's own bar
rules and their `min K` do not apply to execution, so a decision can be
filled on a day the bars library yields no bar for. A bucket with no fine
tuple for the instrument has no price, and the decision is dropped.

**Availability convention (v1).** Every fact at resolution r is available at
the close of its bar; a resampled bar is available at the close of its bucket.
A strategy that reads a @1d open to decide at the open (the corpus case
`opening_gap`) is still not expressible at @1d; it is written at a finer
resolution, where the first bar of the day is available at that bar's close,
which is the honest model of what an opening strategy knows. v2 adds an
availability time per tuple and with it fundamentals and corporate actions.

**Time domains.** For each native resolution, the time domain is the sorted
set of distinct @T values across the primitives supplied at it; for a coarser
resolution reached by resample, it is the set of bucket labels that contain at
least one finer timestamp. `prev`, `lag`, `window`, and `month_start` are
defined over the domain of the rule's resolution, so holidays, half-days, and
missing bars need no calendar primitive.

The domain is a property of the data, not of any relation: a resample's `min
K` removes a bucket from that relation when it holds fewer than K fine tuples,
but the bucket stays in the time domain as long as it holds one. A half-day
with 150 minute bars under a `min 300` bars library is therefore a @1d bar
with no `close_d`: `prev` from the next day lands on it, `day_start` and
`month_start` count it, the executor fills there (execution contract), and a
rule that needs `close_d` at prev(T) does not fire on the day after it. A
strategy that wants to skip bar-less days reaches back with `lag` or a
window, or resamples with `min 1` and a `count` output and gates on the count
itself.

Timestamps are bar labels, and a label is the bar's close instant (section
3): the 09:30 to 09:31 minute bar is labelled 09:31, the session's last bar
16:00, and a daily bar its date. Data supplied to the kernel must follow this
convention; minute data labelled by open time (09:30 to 15:59) puts the first
bar of each day in a bucket of its own and shifts every other bucket by one
bar, silently.

Market data and the executor's state enter the strategy as relations;
decisions leave as a set, are recorded as `decided`, and are filled at the next
bar, so nothing a rule can read postdates its own T.

## 7. Evaluation semantics and the causality theorem

The meaning of a program is the unique model of its rules over the
environment's facts, and the kernel is correct when the `decide` relation it
produces equals that model's `decide`; how the kernel computes it is not part
of the semantics.

**Model.** Given an environment instance E (the primitive relations over a
time domain 𝒯) and a valid program P, stratify P by WF-8 and take, stratum by
stratum, the least fixpoint of the positive rules with negation and
aggregation evaluated against the completed lower strata. WF-1, WF-2, and WF-4
guarantee each fixpoint is reached in finitely many steps; WF-5 guarantees the
negations are meaningful; WF-7 guarantees the reductions are functions. The
result is a unique set of derived relations, M(P, E).

**Closed loop.** Because `position`, `cash`, `fill`, and `decided` depend on
earlier decisions, E is not given up front. Define the kernel's run inductively
over 𝒯 = t₁ < t₂ < ... < tₙ:

1. E₁ holds market data for all of 𝒯, empty executor relations, and empty `decided`.
2. At step k, compute Dₖ = decide(tₖ, ·) in M(P, Eₖ).
3. The executor maps Dₖ and Eₖ to fills at tₖ₊₁; Eₖ₊₁ extends Eₖ with those fills, the resulting position and cash at tₖ₊₁, and decided(tₖ, d) for each d ∈ Dₖ.

WF-6 guarantees Dₖ does not depend on anything in Eₖ with temporal key later
than tₖ, so the order of the loop is well-defined and Dₖ is the same whether
the kernel evaluates one bar at a time or materialises everything it can up
front.

**Causality theorem.** For every valid program P, environment E, and timestamp
t: let E|ₜ be E with every tuple whose temporal key exceeds t removed. Then
decide(t, ·) in M(P, E) equals decide(t, ·) in M(P, E|ₜ).

*Proof sketch.* By induction on strata and derivation depth: every atom in the
derivation of a decide(t, d) tuple has temporal key <= t by WF-6, so the
derivation uses only tuples present in E|ₜ; and E|ₜ ⊆ E with negation only
over complete relations means no negated literal changes truth value between
the two (a complete relation restricted to keys <= t is still complete for
those keys). The theorem is what the checker proves statically; the kernel
also tests it empirically by re-running truncated instances for sampled t,
which catches kernel bugs that the checker cannot.

**Determinism.** M(P, E) is a function of (P, E, parameters, executor
configuration). Two runs with the same inputs produce identical decisions;
there is no randomness, no iteration-order dependence (reductions are total
orders), and no floating-point nondeterminism that the kernel does not control
(aggregates are evaluated in the total order of the group's identity columns).

**Diagnostics.** When a rule does not fire at some t, the kernel can explain
why by naming the first body literal with no solution; because every literal
is one of seven forms, the explanation is mechanical. This is the runtime
counterpart of the checker's static report and is the main debugging tool the
LLM will have.

**Partial arithmetic.** `x / 0`, `log` of a non-positive value, `sqrt` of a
negative value, `std`, `cov`, `corr` or `ols_beta` of fewer than two
observations, `corr` of a constant series, `ols_beta` against a constant
regressor, and a `quantile` level outside [0, 1] have no result. The kernel
halts the run with a diagnostic naming the rule, the tuple, and the offending
expression; for an aggregate the expression is the whole aggregate
(`quantile(P, q) over (...)`), not its argument. The kernel evaluates the
model top-down, restricted to what the decisions need, so the halt is
guaranteed for every degenerate tuple that some `decide(t, ·)` demands; a
degenerate tuple no decision requests (an unguarded `W = 1 / N` in a rule
only called once something has been selected) is never evaluated and does
not halt the run. A degenerate feature is a data problem the author must
see; it is never a silent non-firing, which would let a strategy appear to
work while a condition quietly never triggers.

## 8. Checker rule map

Each judgment is one checker rule with one diagnostic code. The checker in
`src/check/` implements every row; the corpus under `corpus/negative/` holds
one case per row (each file's `# expect:` header names the code it must
produce), and `corpus/strategies/` must check clean.

| Judgment | Code | Checks | Corpus case |
| --- | --- | --- | --- |
| Name resolution | U | relation declared in strategy, a used library, or the environment; a builtin is not a relation | `bad_undeclared`, `bad_negated_builtin` |
| Environment | E | primitive provided by the declared environment; used libraries written against it | `bad_tier2_in_tier1` |
| WF-1 Range restriction | B | every head/negated/compared/assigned variable bound, in the order written | `bad_unbound_head` |
| WF-2 Modes | M | `+` arguments bound at call site; `_` only in `-` positions | `bad_unbound_input` |
| WF-3 Types | T | dimensions balance; signatures match; constructors typed | `bad_price_plus_scalar` |
| WF-4 Temporal recursion | R | every cycle steps strictly back in time via `prev`/`lag` | `bad_recursion_no_step` |
| WF-5 Completeness | N | `not R` only when complete(R); completeness propagated; reductions close | `bad_negation_incomplete_primitive`, `bad_negation_incomplete_derived` |
| WF-6 Causality | F | temporal keys bound from T through causal builtins only; `decided` strictly earlier | `bad_lookahead`, `bad_same_time_history` |
| WF-7 Determinism | D | `top` has `by`; keys cover identity columns; no `first`/`any` | `bad_nondeterministic_reduction`, `bad_unordered_top`, `bad_top_missing_identity` |
| WF-8 Stratification | S | no cycle through `not` or an aggregate | `bad_negation_cycle` |
| WF-9 Decisions | Z, C | at least one decide; one declared mode; constructors match mode | `bad_no_decision`, `bad_mixed_modes` |
| WF-10 Resolution | X | body atoms share the head's resolution; resample strictly finer to coarser, aligned, with `min K` | `bad_resolution_mix`, `bad_lib_executor_resolution` |
| Dead rules | W1 | derived relation not reached from decide | (warning) |
| Unused parameter | W2 | parameter not referenced | (warning) |
| Undefined relation | W3 | declared relation with no defining rule | (warning) |

The six negative cases the first draft asked for before the typed checker was
built (an unbound head variable, an unbound `+` argument, a Price + Scalar
expression, a `top` whose keys omit the entity column, a negation cycle, and a
rule that joins a @1m atom with a @1d atom without resample) are in the corpus,
as is a positive strategy over @1m data that resamples to @1d
(`resampled_momentum`).

## 9. Deferred to v2

Four of the seven questions in the first draft were settled on 2026-10-03 and
recorded in section 1 (literal order, windows, aggregate set, partial
arithmetic), and resampling was pulled into v1 (sections 3 to 6); nothing in
this section is open. The two items below are v2 work, each with its direction
already agreed, kept here so the v1 model says what it deliberately leaves
out.

1. **Availability time (v2).** Each tuple carries an event time and an availability time; causality is judged on availability; the cross-resolution join becomes "latest available at or before T", which adds the coarse-to-fine direction that v1's resample deliberately lacks. This unlocks fundamentals and corporate actions.
2. **Instruments (v2+).** Options and futures as entity domains with instrument relations (underlying, strike, expiry) and their own decision constructors under a Decision sum type.
