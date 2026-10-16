# The corpus of the second formulation

This directory holds the examples of the language as `docs/language-v2.md`
defines it, in five parts.

| Directory | Contents |
| --- | --- |
| `catalog/` | The classes a program is written against, as `abt bundle env` would generate them from a bundle's manifest: `equity`, `future`, `series`. A program never names them; its `data` block names what it needs. |
| `lib/` | The standard library of section 7, one module a file: `calendar`, `catalog`, `stat`, `bars`, `xsec`, `state`, `book`, and the execution presets `equity_default`, `book_fixed_base`, `futures_fixed_base`. |
| `strategies/` | The 23 strategies of `corpus/` rewritten, and `scale_in`, whose rules aggregate their own past. |
| `company/` | MW14 and R8L, the programs of section 9, which reproduces them verbatim. |
| `negative/` | 35 cases, one or more per judgment of section 10 and of the retained ones; each file's `# expect:` header names the code it must produce. |

It is the acceptance corpus of the implementation ordered in section 11 of
that document: `tests/corpus_v2.rs` asserts that every strategy and company
program here checks clean (modulo its `# allow:` lines), that every
negative case produces its code, and that every program runs on synthetic
data of its class and decides.

## The grammar the corpus assumes

A program is a sequence of units. A strategy is nine blocks; a library is a
`data` block, a `uses` line when it reads other modules, and a `rules`
block; an execution preset is a named `execution` block. `uses` names the
libraries a unit reads directly.

```
strategy NAME {
  data       { class CLASS   need ...   require reviewed | allow unreviewed   prefer latest | version V | base finest   [budget { ... }] }
  uses       LIB, LIB, ...
  calendars  { NAME = base | days | weeks(n) | months(n) | years(n) | sessions | sessions of "ROOT"   ... }
  clock      { decide on CALENDAR [at session + OFFSET] }
  universe   { [instruments { NAME [from DATE]; ... }]   rules for universe(A, @T) }
  params     { NAME : TYPE = LITERAL [in LO..HI]   ... }
  rules      { rel ...   rules ...   observe R [where ...] [trace relations | instances]   ... }
  decisions  { silence hold | flat   orders ORDER   decide rules }
  execution  PRESET [with { FIELD VALUE ... }]  |  { the fifteen fields }
  report     { calendar CALENDAR   periods N per year   [window DATE .. DATE] }
}
```

**Data.** `need` names what the program reads, as `docs/language-v2.md` 3.6
defines it: `need bars @r [from DATE] [complete]` (open, high, low, close,
volume at resolution r or finer, from the date on), `need actions` (split,
dividend, delisted), `need listing` (listed, ticker), `need lists "L", ...`
(member), `need series "S" @r [complete]`, `need sessions`, `need
point_value`, `need commission`, `need instruments where ATTR = VALUE`, and
for options `need chain of "U" within D1..D2 and M1..M2 [right call|put]
[spot RELATION]` (docs/spec/01-data.md, 7.2). Several needs may share a
line, comma-separated. With `complete` the program requires versions a
reviewer marked `reviewed = complete`, so that a missing bar means the
instrument did not trade and WF-5 treats the primitives as closed; without it
a missing bar is unknown, and the bar primitives may not be negated, directly
or through a relation that reads them. `require reviewed` asks every version
the binding names for a reviewer's `reviewed` attribute; `allow unreviewed`
admits the others, warned on every run. An optional `budget { money X; bytes
Y; requests Z; seconds S }` caps what resolving the demand may spend (money
is zero unless written). A library's
`data` block says what the library needs; a strategy's demand must cover its
libraries'. `class any` in a library means the module is written for every
class, with `Entity` as the class's entity sort and `units` as its unit. A
rule that reads a primitive the demand did not ask for is an error (E).

**Universe.** The `universe` block defines the entity domain: `universe(A,
@T)` holds at the bars that are the entity's own rows. It is complete by
definition (3.7), whatever its rules read, so the calendar module's
`prev_row`, `first_row` and `bar` are closed and may be negated. A catalog
relation on a coarser calendar than the rule's (listing, membership or
actions read at a minute bar) is read as of the bar (3.4).

**Calendars.** `base` always exists, and the decision calendar is the one
`clock` names. A relation is on the decision calendar unless its signature
writes another after its key: `@T: weekly`; a finite set, `@X: base |
session`, instantiates the rule on each (3.5). `prev`, `lag`, windows and rows
range over the head's calendar. A bar view of the price primitives at a
calendar C is `close@C`, `open@C`, `high@C`, `low@C`, `volume@C`, `traded@C`
(3.3); at `base` the view is the primitive itself, and `traded@base(A, T)`
says that A has a bar at T. A library relation is calendar-polymorphic: it
is instantiated at the calendar of the key it is called with, and `R@C`
names the instance on C (`true_range@session`). The `sessions` calendar is
per instrument; `session_of(A, T, S)` binds the session label of base bar T,
which is a value and not a causal key (3.5).

**Signatures.** `rel name[INPUT: TYPE, ...](A: Equity, @T[: CALENDAR], X:
TYPE, ...)`. Identity is the entity arguments and the key; bracketed inputs
are bound by the caller; value arguments are outputs. An input may be a
relation, `R: rel(Entity, @T, D)`, with its shape; an uppercase type name not
otherwise declared (`D`, `D1`) is a dimension variable quantified over the
signature (F-9, dimension polymorphism). There are no binding modes (4.2).

**Literals of a body**, in any order (5.1): an atom; `not atom` over a closed
relation; a comparison; `X = expr`; `X = agg(e) over (conj) [within (vars)]`
with `agg` in sum, mean, std, var, median, quantile, max, min, count, corr,
cov, ols_slope, ols_r, first, last; `K = rank(R(...) by (k dir, ...)) within
(vars)` and `rank_average`; `top[k](R(...) by (...)) within (vars)`, a
reduction binding R's variables; `X = case { G -> e ; ... [; else -> e] }`
(5.3); `R(..., T0, ...) asof T`; the builtins `prev(T, T1)`, `lag(T, DUR,
T1)`, `month_start(T)`, `day_start(T)`, `session_of(A, T, S)`. A temporal
constraint inside a group is `T1 in rows[N] [min K]` (the entity's own last N
rows, all N unless `min K`), `T1 in prior_rows[N] [min K]`, `T1 in window[DUR]
min K`, `T1 in prior_window[DUR] min K`, `T1 in bucket(C, T)`, or `T1 in
session(X, [a, b))`. The group of a reduction, and of an aggregation that
names one, is `within (vars)`; every variable of the reduced atom outside it
must be fresh (WF-13). Functions: abs, least, greatest, log, exp, sqrt, sign,
floor, ceil, clamp, decile. A bare number is a Scalar (or a Count where a
Count is expected); a dimensioned zero is written typed: `0 USD/share`, `0
shares`, `0 units`.

**Decisions.** One algebra, no mode (4.4): `hold(A, W)` with W a Scalar
weight of capital, `hold(A, Q)` with Q a signed quantity, `trade(A, Q)` a
signed delta; each with an optional trailing order (`market`, `moo`, `moc`,
`moo_moc`, `limit(P[, TIF])`, `stop(P[, TIF])`), the block's `orders` when
omitted. `silence hold` keeps an instrument with no decision; `silence flat`
closes it. A sale of a held quantity is a negative delta bound in the body:
`decide(T, trade(A, Q)) :- held(A, T, Q0), ..., Q = -Q0.` A decision term's
arguments are variables, parameters or literals.

**Kernel relations**, at the decision calendar: `position(A, @T, Q)`,
`cash(@T, C)`, `nav(@T, N)`, `capital(@T, C)` (the base the execution block's
`capital` names: the fixed capital or the equity), `mark(A, @T, P)` (the
price the execution block's `price` names, as the book is marked),
`fill(A, @T, Q, P)`, `decided(@T0, D)`, `decide(@T, D)`.

**Literals with units.** `100 shares`, `2 contracts`, `0 units` (the class's
unit), `60 USD/share`, `0 USD/contract`, `1_000_000 USD`, `20d`, `3mo`, `1y`,
`30min`, `0.02`, `"SPX"` (a label), `"AAA"` (an instrument).

**Headers.** A negative case's first line is `# expect: CODE`, the code it
must produce. A strategy that is meant to warn carries `# allow: Wn` lines
naming the warnings the acceptance test tolerates (`W6` for a ticker literal,
`W9` for an `instruments` table).

**Codes.** The negative cases name the codes of `docs/language-v2.md`
section 10 and of the semantic model's section 8 where unchanged: U
(undeclared or reserved name), E (a primitive outside the demand), B
(unbound head), M (unbound input), T (types), R (recursion without a step),
N (negation over an open relation), F (causality), D (determinism), S
(stratification), Z (no decide rule), X (calendars), K (WF-11 keyedness), G
(WF-13 grouping), P (WF-14 closed configuration), V (WF-15 vocabulary), Q
(WF-17 demand), O (observation); the warnings W1 to W9. The mode codes C of
the first formulation have no counterpart: two decision kinds on one
instrument at one bar are the runtime conflict of F-5.
