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
abt explain --strategy breakout_52w --rule 'decide#1' --at 2023-02-24 --synthetic corpus/
abt explain --strategy breakout_52w --rule 'decide#2' --at 2023-02-24 --bind A=SPY --synthetic corpus/
abt explain --strategy breakout_52w --rule 'features::sma#1' --at 2023-02-24 --inputs SPY,20d,10 --synthetic corpus/
abt synth --env equities_1d --out ./csv corpus/     # write a synthetic market as CSV
```

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
| `corpus/strategies` | 16 strategies that must check clean, including `opening_gap` at @1m and `resampled_momentum` over @1m data at @1d. |
| `corpus/negative` | 17 negative cases, one or more per judgment code; each file's `# expect:` header is asserted by `tests/corpus.rs`. |
| `tests/corpus.rs` | The corpus as the checker's test suite (section 8). |
| `tests/kernel.rs` | Hand-computed executor outcomes, every corpus strategy run end to end, determinism, the causality theorem, runtime diagnostics. |

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
  keywords are lowercase; `_` is a wildcard (output positions only).
- A signature marks each argument `+` (input, bound by the caller), `-`
  (output, bound by the call) or `@` (the temporal key, exactly one). A
  relation's resolution follows the signature (`@1d`); in a library or
  strategy it defaults to the unit's `resolution`.
- Literals carry units: `100 shares`, `5_000_000 USD`, `20d`, `3mo`, `1y`,
  `0.02`, `"SPY"` (an equity). A bare integer is a Count or a Scalar from
  context; a bare decimal is a Scalar.
- Body literals, in the order written: positive atom, `not` atom, comparison,
  `X = expr`, `X = agg(e) over (...)`, `top(N, R(...), by (K desc, A asc))`,
  `resample(R(...) to @1d as T, min K, X = last(P))`, and the temporal
  builtins `prev(T, T1)`, `lag(T, N, T1)`, `month_start(T)`, `day_start(T)`,
  plus `T1 in window(T, N, min K)` / `prior_window` inside an aggregation.
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
| U | name resolution | relation or parameter declared in the strategy, a used library, or the environment; heads define relations declared in their own unit |
| E | environment | the primitive belongs to the declared environment, not another one |
| B | WF-1 | every head, negated, compared or assigned variable is bound, left to right |
| M | WF-2 | `+` arguments bound at the call; `_` only in `-` positions |
| T | WF-3 | dimensions balance; terms match signatures; constructors typed |
| R | WF-4 | every positive cycle steps strictly back in time through `prev` or `lag` |
| N | WF-5 | `not R` only when R is complete; completeness propagates; reductions close |
| F | WF-6 | every temporal key is T or derived from T by a causal builtin; `decided` strictly earlier |
| D | WF-7 | `top` has `by`; the keys cover every identity column; the key is bound; no `first`/`last` outside resample |
| S | WF-8 | no cycle through `not` or an aggregate |
| Z, C | WF-9 | at least one decide; one mode; constructors of that mode; decide's T is a positive atom's key |
| X | WF-10 | body atoms share the head's resolution; resample goes strictly finer to coarser with `min K` |
| W1, W2 | warnings | dead derived relation; unused parameter |

Diagnostics are ordered by the dependency rank of the rule's head, so the
first error reported is the earliest offending relation.

## Reading the kernel

Evaluation is top-down: `decide(t, D)` is requested for each bar `t` of the
decision resolution's time domain, and every derived relation is requested
with its temporal key and inputs bound and memoised by them. Because WF-4
makes all positive recursion strictly time-decreasing and WF-8 keeps
negation and aggregation acyclic, every request terminates and the result is
the unique model of section 7 restricted to what the decisions need.

The executor fills a bar's decisions at the next bar's close (slippage and
commission from `ExecConfig`), then writes `fill`, `position` and `cash` at
that bar and `decided` at the decision bar. In target mode the order is the
difference between the target and the position at execution; `target_weight`
sizes from cash plus marked positions at the execution bar, truncated to
whole shares. Two distinct decisions for one instrument at one bar halt the
run naming both rules; `x / 0`, `log` of a non-positive, `sqrt` of a
negative, `std` of one observation, and a non-positive delta quantity halt it
naming the rule, the tuple and the expression. `cash` is populated at the
first bar with the initial cash so that cash-aware rules can fire from the
start.

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

## Development

```
cargo test            # 31 tests: type algebra, time, corpus, kernel
cargo build --release
```

Adding a corpus case: a strategy goes in `corpus/strategies/` and must check
clean (no errors and no warnings); a negative case goes in `corpus/negative/`
with a `# expect: <code>` header and must produce errors of that code only.
`tests/corpus.rs` asserts both and that every judgment code has a case.
