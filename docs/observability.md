# Observing a strategy

Oct 9, 2026

A strategy that checks clean can still decide nothing, and a strategy that
decides can do so for a reason other than the one its author believes. The
kernel therefore answers, for every relation of a program, what it held at
any bar and why, and every run reports what every relation derived. These are
the commands; `docs/language-v2.md` section 8 is the design they prototype.

## The program before a run: `abt show`

```
abt show --strategy NAME <files...>
```

prints the program graph: every relation `decide` reaches, from `decide`
upward, each with its signature (modes `+` input, `-` output, `@` temporal
key), its kind (primitive, executor, kernel, derived, output), its
resolution, whether it is complete (so `not R(...)` is admissible), the
stratum it is evaluated in, whether it reads its own past (temporal
recursion), whether it is in the closed loop (reads the book: `position`,
`cash`, `nav`, `fill`, `decided`) or the open loop, its depth (the longest
path from a primitive), the relations it reads with the sign of each read
(`+` positive, `-` negated, `~` inside an aggregation or reduction), and its
rules. The primitives the program reads are listed last with their
completeness. The first line gives the counts and the depth of the whole
program.

Read it to confirm that the relations you meant to connect are connected
(a relation you defined but `decide` never reaches is not in the graph), that
a feature you meant to be open-loop does not read the book by accident, and
that a relation you negate is complete.

## Every run: the coverage report

`abt run` prints, after the metrics, one line per relation `decide` reaches:

```
coverage over 468 decision bars (calls: bars and inputs the relation was demanded at; tuples: what it derived):
  decide            468 calls       229 with tuples        1057 tuples
  tgt               468 calls       374 with tuples        4405 tuples
  crowded           374 calls         0 with tuples           0 tuples   EMPTY: demanded, never derived
  flat                0 calls         0 with tuples           0 tuples   never demanded
  close                                                     27577 tuples in the data (primitive)
```

A derived relation's `calls` are the bars (times the input tuples, for a
relation with `+` arguments) at which some rule or the decision asked for
it; `with tuples` are the calls that derived at least one tuple; `tuples` are
all it derived. A primitive, executor or kernel relation shows what the data,
the executor or the kernel holds.

- **EMPTY: demanded, never derived** marks a relation that was asked for and
  never held. A rule reading it positively never fires.
- **never demanded** marks a relation no rule reached, because an earlier
  literal of every rule that reads it excluded every instance first (the
  evaluator stops at the first literal with no solution).

When `decide` derived nothing the report adds the root causes: the path of
empty relations from `decide` down to the first empty relation whose own
reads are not empty, and, for that relation's rules, an explanation at a
sample bar naming the first literal with no solution:

```
never fired: decide derived no tuple over 500 bars.
  decide <- go <- rich: `rich` is the first empty relation on this path; what it reads is not empty: universe+ (1000 tuples), close+ (1000 tuples)
  rule never::rich#1 did not fire at 2022-12-19: literal 3 `P > floor` has no solution (a sample bar; abt query --rel rich --at BAR --explain for another)
```

When `decide` did derive tuples, relations that were demanded and never
derived are still listed, since a rule of yours that never fires is usually a
mistake rather than a feature.

## Any relation at any bar: `abt query`

```
abt query --strategy NAME --rel RELATION [--at TIMESTAMP] [--inputs V1,V2,...] [--symbol SYM] [--explain] (--data DIR | --bundle DIR | --synthetic ...) [executor options] <files...>
```

runs the strategy and prints the tuples of `RELATION` at the bar `--at` (the
last decision bar when omitted; a timestamp that is not a bar of the
relation's resolution is refused naming the nearest bars). Any relation can
be queried: a primitive (`close`), an executor relation (`position`), a
feature, a signal, `decide`. A relation with `+` arguments needs them in
`--inputs`, in signature order (`sma` as `--inputs AAA,20d,12`); when the
only input is the entity, `--symbol` supplies it. `--symbol` also keeps one
instrument's tuples, including the decisions on it (`decide` carries the
instrument inside the decision value).

`--explain` adds, for every rule of the relation at that bar, whether it
fired and with how many solutions, with the first solution's variables and
their values (every variable of the rule, so the instance can be checked
against the data), or the first body literal with no solution. With
`--symbol` the rule is explained for that instrument only. The executor
options (`--price-relation`, `--frictionless`, `--capital`, ...) are those of
the run being examined; pass the same ones.

```
abt query --strategy sma_crossover --rel above --at 2023-06-01 --symbol AAA --explain --synthetic corpus/
above at 2023-06-01 (@1d): 1 tuple(s) for AAA
  above(AAA, 2023-06-01)
rule sma_crossover::above#1 fired at 2023-06-01 with A = AAA with 1 solution(s); first solution: A = AAA, F = 36.906, S = 35.854, T = 2023-06-01
```

`abt explain --rule LABEL --at TIMESTAMP ...` is the same question asked of
one rule by its label (`unit::head#n`), with `--bind VAR=VALUE` for any body
variable.

## Every bar of the run: `--ledger`

```
abt run ... --ledger R1,R2,... --ledger-out DIR
```

writes, for each named relation, every tuple it held at every bar of the run
(of the relation's own resolution, inside the run's window) as
`DIR/<relation>.parquet`, one column per argument of the signature plus
`bar`, and the coverage counts as `DIR/coverage.parquet`. A relation whose
only `+` argument is its entity (`close`, `sma`'s cousins without other
inputs) is written for every symbol; a relation with other `+` arguments is
queried at a bar with `--inputs` instead. The ledger is what
to read when a feature's values over time, not one bar, are in question; a
feature the evaluator would have skipped at some bars (because an earlier
literal excluded the instance) is computed for the ledger, so the ledger is
the relation's extension, whatever the run's plan.

## A way to work

1. `abt check` until clean.
2. `abt show` and read the graph against what you meant to write.
3. `abt run`; read the coverage report. An EMPTY relation on the way to
   `decide`, or a `never fired`, is the first thing to resolve.
4. `abt query --rel R --at BAR --explain` on the first empty relation: the
   literal named is where the rule and the data disagree; query that
   literal's relation at the same bar to see what it holds.
5. `--ledger` for the feature's values over time when a single bar is not
   enough (a window that never fills, a value that is off by a unit, a
   latch that never sets).
