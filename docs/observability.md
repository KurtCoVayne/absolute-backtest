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
upward, each with its signature (`@T` the temporal key, every other
argument with its type; a library relation under its instance name, such as
`bought_within[cooldown]`), its kind (primitive, executor, kernel, derived,
output), its calendar, whether it is complete (so `not R(...)` is admissible), the
stratum it is evaluated in, whether it reads its own past (temporal
recursion), whether it is in the closed loop (reads the book: `position`,
`cash`, `nav`, `fill`, `decided`) or the open loop, its depth (the longest
path from a primitive), the relations it reads with the sign of each read
(`+` positive, `-` negated, `~` inside an aggregation or reduction), and its
rules. The primitives the program reads are listed last with their
completeness, then the execution block expanded field by field (what the
executor runs with, a preset's defaults included) and the report block. The
first line gives the counts and the depth of the whole program.

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

A derived relation's `calls` are the bars at which some rule or the
decision asked for it; `with tuples` are the calls that derived at least one tuple; `tuples` are
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
abt query --strategy NAME --rel RELATION [--at TIMESTAMP] [--symbol SYM] [--explain] (--data DIR | --bundle DIR | --synthetic ...) <files...>
```

runs the strategy and prints the tuples of `RELATION` at the bar `--at` (the
last decision bar when omitted; a timestamp that is not a bar of the
relation's calendar is refused naming the nearest bars). Any relation can
be queried: a primitive (`close`), an executor relation (`position`), a
library instance by the name `abt show` prints, a signal, `decide`. A
relation takes no inputs at a query: its bracketed inputs were bound when
the library relation was instantiated. `--symbol` keeps one instrument's
tuples, including the decisions on it (`decide` carries the instrument
inside the decision value).

`--explain` adds, for every rule of the relation at that bar, whether it
fired and with how many solutions, with the first solution's variables and
their values (every variable of the rule, so the instance can be checked
against the data), or the first body literal with no solution. With
`--symbol` the rule is explained for that instrument only. The executor is
the program's execution block, so the run examined is the one `abt run`
makes; no option changes it.

```
abt query --strategy sma_crossover --rel above --at 2023-06-01 --symbol AAA --explain --synthetic corpus-v2/catalog corpus-v2/lib corpus-v2/strategies
above at 2023-06-01 (base): 1 tuple(s) for AAA
  above(E1, 2023-06-01)
rule sma_crossover::above#1 fired at 2023-06-01 with A = E1 with 1 solution(s); first solution: A = E1, F = 38.64666666666667, S = 38.56162162162161, T = 2023-06-01
```

The synthetic market's securities are `E1`, `E2`, ... carrying the tickers
given; an instrument shows by its identifier, and `--symbol AAA` resolves
the ticker as of the run's bundle date.

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
`bar`, and the coverage counts as `DIR/coverage.parquet`. Every tuple is
written, for every instrument; a library relation is named by its instance
name (`--ledger 'mean_window[fast, fast_min, close]'`). The ledger is what
to read when a feature's values over time, not one bar, are in question; a
feature the evaluator would have skipped at some bars (because an earlier
literal excluded the instance) is computed for the ledger, so the ledger is
the relation's extension, whatever the run's plan.

## The compiler's view: `abt graph`, `abt ir`, `abt plan`, `abt verify`

```
abt graph --strategy NAME <files...>
abt ir --strategy NAME [--stage parsed|checked|normalised|analysed|rewritten|planned] [--json] <files...>
abt rewrite-log --strategy NAME [--json] <files...>
abt plan --strategy NAME [--observed] [--json] [--plan written|optimised] [(--data DIR | --bundle DIR | --synthetic ...)] <files...>
abt verify --strategy NAME [--tier N] [--solver z3|cvc5] [--replay-dir DIR] [--json] [(--data DIR | --bundle DIR | --synthetic ...)] <files...>
```

`graph` is `show` with the compiler's facts (`docs/compiler-design.md`,
section 7.1): on every relation its keyedness label when it is temporally
recursive (keyed by construction, by guard, or at runtime, with the reason),
whether it is demanded or elided, and the abstract value of each column
(the sign and interval the analysis established); the elided relations are
listed at the end with their signatures; the last line carries the program
hash and the plan's totals. `ir` prints a stage's result (the normalised
form with its hash and name map, the analysis with its warnings, the
rewritten form, the plan); `rewrite-log` every rewrite applied with its
law; `plan` the literal order and cost of every rule, the aggregate
implementations, the stores and strata, and with `--observed` the counts a
run produced next to the estimates; `verify` the verification report, tier
by tier (the checker, the abstract interpretation, linear feasibility, SMT,
constrained Horn clauses, translation validation, the runtime), every
obligation with its verdict, budget use and replay file. Tiers 3 and 4
need a solver on the PATH (`--solver z3`); without one their obligations
are undecided and `--replay-dir DIR` keeps the scripts to run by hand.

## The state tables: `abt table` and `--tables`

```
abt table --strategy NAME --table coverage|plan|book|binding|summary|extension|carried|derivation|failure
          [--rel R] [--at T] [--symbol SYM] [--out FILE] [--json] (--data DIR | --bundle DIR | --synthetic ...) <files...>
abt run ... --tables DIR
```

After a run the state tables of `docs/compiler-design.md`, section 7.2, are
queryable; each has a stable schema, prints as text (the schema line, then
the rows) or JSON, and `--out FILE` writes it as Arrow IPC (Parquet for a
`.parquet` name).

| Table | Columns | What it holds |
| --- | --- | --- |
| `coverage` | relation, derived, calls, nonempty, tuples | the coverage counts of the run |
| `plan` | rule, position, literal, kind, text, card_in, card_out, cost, selectivity, observed_in, observed_out, observed_emptied, infeasible, elided | every literal in its evaluation order with the estimates and the observed counts |
| `book` | bar, cash, gross, net, equity, leverage | the executor's ledger at every bar |
| `binding` | relation, kind, rows, first_bar, last_bar, availability | the resolved data binding |
| `summary` | rule, literal, aggregate, group, bar, rows, path, shift_x, shift_y, sum_x, sum_y, sum_xx, sum_yy, sum_xy, value | the window state of every aggregation group at every bar it served: the rows a calendar window held, or the shifted moment sums of a `rows` window served by running sums (`mean = shift_x + sum_x / rows`) |
| `extension` (`--rel R`) | the signature's columns | every tuple of R at every bar of its resolution, sorted by bar |
| `carried` (`--rel R`) | identity, bar, carried_from, values | at every bar, the latest earlier tuple of each identity: what a rule reading R through `prev` sees |
| `derivation` (`--rel R`, `--at T`) | tuple, bar, rule, body | one witness per derived tuple at the bar: the rule and its body tuples |
| `failure` (`--rel R`, `--at T`, `--symbol`) | rule, identity, bar, literal, literal_text, bindings, absence_driven | for every rule and attempted instrument, the first excluding literal and the bindings at that point |

`--tables DIR` on a run writes them all as Arrow IPC under `DIR`:
`coverage`, `plan` (with the observed counts), `book`, `binding`,
`summary`, `extension/<R>` for every demanded derived relation,
`carried/<R>` for the temporally recursive
ones, and `derivation` and `failure` of `decide` at the sampled bars (the
last bar and nine evenly spaced ones; `abt table --at BAR` answers for any
other).

## Why a tuple is there, or not: `explain --derivation`, `why-not`, `materialise`

```
abt explain --strategy NAME --derivation 'R(v1, ..., vn)@BAR' [--depth N] [--json] (--data DIR | ...) <files...>
abt why-not --strategy NAME --rel R --symbol SYM [--at T] [--json] (--data DIR | ...) <files...>
abt materialise --strategy NAME --rel R [--at T] [--json] (--data DIR | ...) <files...>
```

A tuple's id is its text with the bar it holds at, as the derivation table
prints it: `decide(2022-04-14, trade(E5, -100))@2022-04-14`.
`explain --derivation` prints its derivation tree: the rule that derived
it, that rule's body tuples, and theirs, down to supplied tuples (a
primitive's, the executor's) or `--depth` levels (default 6; a truncated
node says so and can be explained on its own). The witness is the first
derivation in canonical evaluation order, found by re-evaluating the rule
at the bar with its head bound to the tuple; by the determinism contract
two runs give the same witness, so nothing is recorded during the run.

```
decide(2022-04-14, trade(E5, -100))@2022-04-14  [trailing_stop::decide#2]
  held(E5, 2022-04-14, 100)@2022-04-14  [book::held#1]
    position(E5, 2022-04-14, 100)@2022-04-14  (supplied by the executor)
  high_since_entry(E5, 2022-04-14, 119.83)@2022-04-14  [state::high_since_entry#2]
    held(E5, 2022-04-14, 100)@2022-04-14  [book::held#1]
      position(E5, 2022-04-14, 100)@2022-04-14  (depth limit: explain this tuple to continue)
    not fill(E5, 2022-04-14, _, _)@2022-04-14  (absent, as the negation requires)
    high_since_entry(E5, 2022-04-13, 119.83)@2022-04-13  [state::high_since_entry#2]
    ...
    close(E5, 2022-04-14, 108.34)@2022-04-14  (supplied by the primitive)
  close(E5, 2022-04-14, 108.34)@2022-04-14  (supplied by the primitive)
```

`why-not` answers for an absent tuple: per rule of the relation, the first
literal that excluded the instrument at the bar and the bindings at that
point. When that literal reads a derived relation at an identity and bar the
bindings fix, the question is asked again of that relation, indented under
the rule, down to a comparison, a supplied relation, or eight levels: the
answer ends at the root, not at the next relation down. When the deepest
literal is a supplied relation's atom with no tuple there (a `fill` that
never happened, a label the data never carries), the verdict is
*absence-driven*: the data, not the rule, decided.

```
why not decide(E1) at 2022-04-14:
  trailing_stop::decide#1 excluded at literal 1 `strong(A, T)` with A = E1, T = 2022-04-14
    why not strong(E1) at 2022-04-14:
      trailing_stop::strong#1 excluded at literal 2 `M > threshold` with A = E1, M = -0.0866652710906427, T = 2022-04-14
  trailing_stop::decide#2 excluded at literal 1 `held(A, T, Q0)` with A = E1, T = 2022-04-14
    why not held(E1) at 2022-04-14:
      book::held#1 excluded at literal 1 `position(A, T, Q)` with A = E1, T = 2022-04-14: absence-driven (a supplied relation has no tuple there)
```

`materialise` evaluates a relation at a bar from its rules whether or not
the plan computed it: an elided relation (one nothing demanded reaches,
R5) keeps its rules and is evaluated on request, as section 2.4 of the
compiler design says; the note says which case it is.

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
   latch that never sets); `abt table --table extension` or `carried` is
   the same question through the state tables, `summary` for a window's
   state.
6. `abt why-not --rel R --symbol S --at BAR` when a tuple you expected is
   absent: the excluding literal and its bindings, and whether the data
   (absence-driven) or a comparison decided.
7. `abt explain --derivation ID` on a tuple you did not expect: the tree
   of tuples that produced it.
8. `abt graph`, `abt plan` and `abt verify` before trusting a program: the
   keyedness labels, the dead literals the analysis found, the plan the
   run uses, and the verification report.
