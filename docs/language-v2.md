# The Strategy Language, Second Formulation

Oct 8, 2026 · John Gonzalez

This document reviews the changes that brought MW14 and R8L into abt (pull
request 69, merged as `0d9bc2b`) against the standard set by
[`formal-foundations.md`](formal-foundations.md), finds that the data, the
configuration and the surface form of the two programs fall short of it in
ways that are structural rather than cosmetic, and proposes the second
formulation of the language: one data store per instrument class with every
resolution derived from it, a data block that is a query over the catalog
which the kernel resolves rather than a bundle the author names, programs
that carry their whole configuration and their whole universe as typed
blocks, an evaluation semantics under which the order of a rule's literals
is irrelevant and a false guard stops work soundly, a standard library of
formal operators on data rather than trading ideas, and a query facility
over every relation at every step whose state tables and algorithms are
given, with the proof that observing a run never changes it. Section 15
records the prototype of that facility on the v1 kernel and the study in
which Haiku agents used it. MW14 and R8L are rewritten in the
proposed form (section 9) so that the gain is visible on the two programs
that matter.

## 0. Reading guide

Claims are tagged as in the foundations: [E] an established result with a
reference, applied here; [N] a new proposition with a proof or proof sketch;
[C] a conjecture with what would settle it. One tag is added: [D] a design
decision, stated with the alternative it rejects and why. Section references
of the form F-3.9 are to the foundations document; S-6 to
[`semantic-model.md`](semantic-model.md); B-3 to
[`data-bundle.md`](data-bundle.md).

The document answers five requests in order. Section 1 is the review.
Section 2 states the principle the rest follows from. Sections 3 and 4 are
the data and the program structure. Sections 5 and 6 are the evaluation
semantics and what it licenses the kernel to optimise. Section 7 is the
standard library. Section 8 is the query facility. Section 9 is the two books
rewritten. Section 10 is the checker; section 11 the implementation order;
section 12 what each change does for a smaller model and for a human reader.

## 1. Review of the incoming changes

The two books reproduce their references to the bit, and that is the result
the pull request set out to get. The review is of what had to be done to get
it, because each accommodation is a place where the program text stopped
being the program.

| # | Finding | Evidence | Fixed in |
| --- | --- | --- | --- |
| R1 | **The data's time structure is encoded in environment names and comments.** `equities_1w` declares every relation `@1d` and says in a comment that the domain is week ends "so prev and rows step a week". `futures_sessions` declares `@1d` relations labelled by session date and `@1m` relations whose keying is stated only in prose. A third book at three weeks or one month would need a third environment and a third ingest script. | `corpus/env/equities_1w.dsl` lines 1 to 8; `corpus/env/futures_sessions.dsl` lines 1 to 5; every MW14 usability run wrote `resolution @1d` for a weekly book and then `window(T, 52w, ...)`, `364d`, or the parse error on `52 w` | section 3 |
| R2 | **Strategy rules were moved into the ingest scripts and shipped as data.** `dvol` is the week's mean of close × volume; `trclose` is a total-return index; `series(T, "VIX_MA4", ·)` is a four-week mean; `open0_m`, `close20_m`, `close30_m`, `open_m`, `close_m` are session-offset aggregates of the minute bars; `clock` is the decision time and the session's eligibility; `calendar` is the reporting calendar. Each is a rule of the book written in Python. The DSL program is therefore not the program: its meaning depends on 650 lines of ingest code the checker never sees. | `scripts/ingest/ndlake_weekly.py` lines 20 to 33; `scripts/ingest/tradestation_sessions.py` lines 18 to 36; `docs/parity-r8l.md` "the prints are real bars of the grid, not computed values" | sections 3.4, 3.5, 7 |
| R3 | **The selection is invisible.** R8L trades 26 roots chosen by `HOME26_EXCLUDED` and starts each root at `DEV_USABLE_FROM`, both constants of the ingest script; the program says `env futures_sessions` and nothing else. MW14 excludes SPY and QQQ in the script, and names its index as a label whose vocabulary lives in a comment. | `tradestation_sessions.py` lines 64 to 70, 173, 238 to 249; `ndlake_weekly.py` line 37; the usability runs' `member(A, T, "SP500")` giving zero decisions silently | sections 3.6, 3.7 |
| R4 | **The run's semantics live in flags.** MW14 needs 16 flags and R8L 12, recorded in a comment at the head of each file. The study hashes the executor configuration but the program hash does not, so two lineages with different cost models are one lineage. `--frictionless` on the command line still charges a 5 % debit rate; `--data` on a bundle directory loads empty relations; the study annualises a weekly book at 252 while the command line is told 52. | `src/bin/abt.rs` lines 162 to 177 (72 value options); `src/kernel/mod.rs` 809 to 821; `src/study/mod.rs` 561, 632; the usability review, "3 of the 6 abt calls in r8l-sonnet-docs went on flags" | section 4 |
| R5 | **A declared `mode` and an undocumented silence.** Both books declare `mode target`; that silence keeps the position is stated nowhere and was inferred by one model and never by the other. The 2.5 % band is four decide rules whose difference is which side of the band they test. | `corpus/company/mw14.dsl` lines 164 to 167; usability review item 11 | section 4.4 |
| R6 | **Literal order and binding modes decide the meaning of a reduction silently.** With `+A` in the head, `rank(dvol(A, T, D), by (D asc), as K)` groups by {A, T}, so every rank is 1 and the liquidity decile never holds; the checker's W4 names `top`, not `rank`, and the model ignored it. Nine of fourteen Haiku attempts on MW14 were spent on mode errors whose message does not name the fix. | usability review items 1 and 2; `experiments/llm-usability/runs/mw14-haiku-ex/NOTES.md` | sections 4.2, 5.2 |
| R7 | **"A name's own previous row" has no construct.** MW14 spends `seen_before`, `cumf`, `row` and `lp` (14 lines, three temporal recursions and an `asof` idiom) on the previous row, the cumulative split product, the row number and a latch. No Haiku run found the idiom; the one Sonnet run that did used a constant in an `asof` output position and ran for twenty minutes. | `corpus/company/mw14.dsl` lines 66 to 83, 129 to 133; usability review item 3 | section 7.1 |
| R8 | **Calendar windows and row windows are two forms with two minimum conventions.** `window(T, 20d, min 20)` over sessions holds about fourteen rows; `rows(T, N, min K)` repeats N as K in every full window. Nothing warns. | usability review item 4; MW14's `rows(T, clenow_w, min clenow_w)` five times | section 7.2 |
| R9 | **The minute prints are keyed at different minutes and the program has to know.** `open0_m` sits at 09:31, `close30_m` and `clock` at 10:00, `open_m` at 10:01 and at the close. The reference program reads them with `asof T, T0 > S`; both Haiku runs joined them on one `T` and derived nothing. | `corpus/company/r8l.dsl` lines 74 to 82; usability review item 6 | section 3.5 |
| R10 | **Structure is written as parameters to silence W5.** `one`, `three`, `short = -1`, `full = 1`, `one_price = 1 USD/share`, `no_price = 0 USD/share`, `deciles = 10`, `liq_cut = 4` are not degrees of freedom; they are the absence of `sign`, of a typed zero, of a decile operator and of a dimensionless `log` of a price ratio. | `corpus/company/r8l.dsl` lines 38 to 48; `mw14.dsl` lines 41 to 52 | sections 7.3, 10 |
| R11 | **Partial arithmetic forces guards into the wrong rule.** The halt-on-undefined contract is right; evaluating literals left to right makes the guard's position matter, so authors guard too early (a shared `sig` with `V > no_price`) or too broadly (167 sidecar legs lost to `abs(D) > 0`). | `r8l.dsl` lines 64 and 80; usability review item 9 | section 5.1 |
| R12 | **A program can check clean and decide nothing, silently.** All four Haiku books did. `run` names no empty relation; `explain` takes no bundle and asks for a rule label and a bar. | `experiments/llm-usability/review/REVIEW.md` section 2.12; `tests/explain.rs` | sections 8, 8.1 to 8.3, 15 |
| R13 | **The floating-point contract is a flag.** `--window-sums exact` is needed for R8L's last leg; the default is "equal to rounding". A run's bit-identity depends on a flag the program does not record. | `docs/parity-r8l.md`, "`--window-sums exact` is needed for the exact match" | section 4.3 |
| R14 | **Register.** Relation names (`lp`, `cur`, `tgt`, `w`, `sig`), comments addressed to the translator ("the book's", "a halt"), and a weekly book declared daily all read as a transcription, not as a definition a reader could check against the book's docstring. | `corpus/company/*.dsl` | section 9 |

None of this is a defect of the kernel, which did exactly what was asked, and
two findings (R7, R8) are accommodations the semantic model allowed by
design. The review's conclusion is that the accommodations were made on the
wrong side of the boundary: the data was shaped to the strategy and the
command line was shaped to the run, where the standard asks that the program
be shaped to the data and that the run be a function of the program alone.

## 2. The principle

**Definition 2.1 (closed program).** A strategy P is closed when the meaning
function M of F-3 is a function of P and the bundle B alone: M(P, B) is
defined with no further input, and every name in P resolves inside P, the
libraries it names, or the bundle's manifest.

**Proposition 2.2 [N] (identity of a run).** For closed programs, two runs
with the same program hash and the same bundle version produce identical
decisions, fills and book, and the lineage of F-13(3) is a function of the
program text. *Proof.* M is a function of (P, B) by Definition 2.1;
determinism is F-5; the bundle version fixes B. ∎

Today P is not closed: M depends on the executor configuration (F-7's
determinism statement reads "a function of (P, E, parameters, executor
configuration)"), on the price relation chosen on the command line, on the
reporting conventions, and through R2 on the ingest script. The second
formulation makes P closed by moving every one of those inputs into the
program as a typed block (section 4), and makes B canonical by fixing what a
bundle holds per instrument class (section 3). The command line then takes a
program and a bundle and nothing else, and the study holds only what is not
a property of the run: the hold-out, the objective, the thresholds and the
grid.

[D] The alternative is a configuration file beside the program, which the
study would hash with it. It is rejected because a second file is a second
place to forget, for a person and for a model alike, and because a reader of
the program could still not tell what it trades at.

## 3. Data: one store per instrument class, every resolution derived

### 3.1 The time structure

**Definition 3.1 (base domain).** For each instrument class κ (equity,
future, series) the bundle holds one base time domain 𝒯_κ: the finite,
totally ordered set of bar labels at the finest resolution the bundle has for
that class, each label the bar's close instant (S-6). A bundle states its
base resolution per class in the manifest (`@1d` for the Norgate equities,
`@1m` for the TradeStation futures, weekly for the CBOE VIX series).

**Definition 3.2 (calendar).** A calendar C over 𝒯_κ is a partition of 𝒯_κ
into intervals (in the order of 𝒯_κ), each class labelled by its last
element. The labels form the domain 𝒯_C ⊆ 𝒯_κ, and the bucket map β_C : 𝒯_κ
→ 𝒯_C sends a base bar to the label of its class. The base domain is the
calendar of singletons. Calendars are ordered by refinement: C ≼ C' when
every class of C' is a union of classes of C.

**Proposition 3.3 [E].** Refinement is a partial order and the set of
calendars over 𝒯_κ is a lattice (the partition lattice restricted to interval
partitions, which is closed under meet and join). Two calendars are *aligned*
in the sense of S-3 exactly when they are comparable. Reference: the lattice
of interval partitions is a standard sublattice of the partition lattice;
Stanley, Enumerative Combinatorics vol. 1, ch. 3.

Calendars are declared, not fixed. The declarable constructors are:

| Constructor | Classes | Label |
| --- | --- | --- |
| `base` | singletons | the bar |
| `days` (for a sub-daily base) | the base bars of one civil date | the day's last bar |
| `weeks(n)`, `months(n)`, `years(n)` | n consecutive civil weeks, months, years, anchored at the domain's first element or at a named anchor date | the period's last bar |
| `sessions` (a class property, futures and intraday equities) | the base bars of one trading session of the instrument | the session's last bar |
| `on(E)` for an event relation E(@T) | the bars from one event to the next | the bar before the next event |

`weeks(3)` is as expressible as `weeks(1)`, and `equities_1w` ceases to exist
as a name: it is the equity class on the calendar `weeks(1)`.

**Proposition 3.4 [N] (clock calculus over declared calendars).** Replace the
fixed chain of S-3 by the lattice of Proposition 3.3. The clock judgment of
F-9 is unchanged: a rule's body atoms outside a bucket aggregate have the
head's calendar; a bucket aggregate takes C to C' for C ≼ C'; `prev`, `lag`,
windows and `rows` range over the head's calendar. Temporal stratification
(F-3.8) and causality (F-7.1) hold with β_{C→C'} in place of β_{r→r'},
because every proof there uses only that β maps a base bar to a label at or
after it, which Definition 3.2 guarantees. ∎

A rule declared on `weeks(1)` therefore sees `prev` step a week, `rows` step
a week, and `window(T, 1y)` hold about 52 bars, with no comment needed.

### 3.2 Primitives are declared by class, not by strategy

The bundle manifest declares, per class, the primitive relations the class
holds at its base calendar. The environment file is generated from the
manifest by `abt bundle env` and is never written by hand; its name is the
class and the bundle version, not a resolution.

| Class | Primitives at the base calendar | Catalog relations | Vocabularies |
| --- | --- | --- | --- |
| `equity` | `open`, `high`, `low`, `close`, `volume` (as traded) | `listed(A, @T)`, `ticker`, `split`, `dividend`, `delisted`, `member(A, @T, +List)`, `classification` | the labels of `member`, `delisted`, `classification` |
| `future` | `open`, `high`, `low`, `close`, `volume` per root (back-adjusted continuous; may cross zero); `session(A, @S, Open)` | `point_value(A, @T, V)`, `member(A, @T, +List)`, `usable(A, @T)` (a research-defined availability relation), `commission(A, @T, C)` | the labels of `member` |
| `series` | `value(+Name, @T, V)` | — | the names |
| executor (every class) | `position`, `cash`, `nav`, `fill`, `decided` | — | — |

Everything in the current `equities_1w` and `futures_sessions` that is not in
this table is a rule (R2) and moves to section 7 or into the program.

The Share dimension of S-2 names the class's unit: a share for `equity`, a
contract for `future`. A price literal is spelled with the class's unit
(`3 USD/share`, `0 USD/contract`) and both spellings denote the dimension
(1, −1, 0); the checker accepts the spelling of the program's class. A
catalog relation at a coarser calendar than the decision calendar (`member`
on a daily catalog read at a minute decision bar) is read as of the bar, as
a series is (3.4).

### 3.3 Bars at any calendar are derived views

**Definition 3.5 (bar view).** For a class with price primitives at the base
calendar and a calendar C, the bar view at C is the family of derived
relations

```
open@C(A, T, O)   :- O = first(P) over (T1 in bucket(C, T), open(A, T1, P)).
high@C(A, T, H)   :- H = max(P)   over (T1 in bucket(C, T), high(A, T1, P)).
low@C(A, T, L)    :- L = min(P)   over (T1 in bucket(C, T), low(A, T1, P)).
close@C(A, T, P)  :- P = last(P1) over (T1 in bucket(C, T), close(A, T1, P1)).
volume@C(A, T, V) :- V = sum(Q)   over (T1 in bucket(C, T), volume(A, T1, Q)).
traded@C(A, T)    :- N = count(T1) over (T1 in bucket(C, T), close(A, T1, _)), N > 0.
```

with `bucket(C, T)` the temporal constraint β_C(T1) = T of F-2 (the
`resample` derived form). Every other bar-level quantity (the week's mean
daily dollar volume, the week's split product) is one more such rule.

**Proposition 3.6 [N] (cost of derived resolutions).** Materialising the bar
views at every calendar a program names costs O(|𝒯_κ| · |Ent|) per
calendar, once per bundle version, and is shared by every program on that
bundle. *Proof.* Each view is a bucket aggregate of a distributive aggregate
(F-8.1), maintained in O(1) per base tuple by F-8.2; by F-11.4 the result
depends on the bundle and the calendar only, so it is a feature-cache
entry. ∎

So the base can be the minute, as asked, and the weekly book costs what it
costs today: the weekly bars are computed once from the daily (or the
minute) data and stored beside the bundle under the calendar's hash.
`equities_1w` becomes a cache entry nobody has to name.

[D] The alternative is to keep storing vendor-supplied coarse bars beside the
fine ones (B-3 keeps `close_d` and `close_d_rs`). It is retained only as a
bundle test: where the vendor supplies a coarse bar, the test reconciles it
against the view; the program reads the view.

### 3.4 Series and the as-of read

A series has its own native calendar (SPXTR daily, VIX weekly). On a decision
calendar that is coarser or equal, a series is read as a bar view (its last
value in the bucket); on one that is finer, it is read as of the bar (the
as-of join of S-4). Both are the two directions of F-7's table, `when` and
`current`, and the checker picks the direction from the calendars. A program
never names a series' resolution. The same rule serves every catalog relation
whose native calendar differs from the rule's.

### 3.5 Sessions and offsets

**Definition 3.7 (session calendar and offsets).** For a class with
`session(A, @S, Open)`, `sessions` is the calendar whose classes are, per
instrument, the base bars with Open < label ≤ S. The offset of a base bar T
in its session is label(T) − Open, a Duration. The temporal constraint
`T1 in session(X, [a, b))`, with X either a base bar or a session label,
holds for the base bars T1 of X's session with offset in [a, b) and, when X
is a base bar, T1 ≤ X.

**Proposition 3.8 [N].** `session(X, [a, b))` is a causal temporal
constraint: every T1 it admits has label ≤ X. *Proof.* For X a session label,
every bar of the session has label ≤ S by Definition 3.2; for X a base bar,
by the clause T1 ≤ X. ∎ It therefore joins window, prior_window and bucket in
F-2's list of admissible constraints, and F-3.8(i) and F-7.1 are unaffected.

The `sessions` calendar is per instrument: its domain for A is A's session
labels, and `prev`, `rows` and windows on it step A's own sessions. The
builtin `session_of(A, T, S)` binds the label S of the session of base bar T.
S is the session's close and lies after T, so S is a value, not a causal key:
it may anchor `session(T, [a, b))` and feed `prev(S, S1)`, and the checker
derives S1 < Open(S) < T, so S1 is a causal key for session-calendar
relations. A rule at a base bar that reads `atr(A, S, V)` at its own session
is rejected by WF-6, which is the static form of the as-of fix recorded in
`docs/assessment.md` ("a minute rule could read the day's own bar intraday").

A signature may list a finite set of calendars, `@X: session | base`; the
rule is instantiated on each (calendar polymorphism over a listed finite set,
F-13 conjecture 4 in its trivially decidable form). `open0` and
`close_before` in section 9.2 are written once this way and read at the
decision bar and at the session's close alike.

The five minute-print relations of `futures_sessions` are then rules at the
decision bar, each one line (section 9.2), and the two keyings of R9 become
one: the head's. The decision clock `decide at session + 30min` names the
base bar whose label is Open + 30 minutes, or the first traded bar after it;
which of the two is a declared choice of the `clock` block, not a property of
the ingest.

### 3.6 The catalog is a database and the data block is a query

**Definition 3.10 (catalog).** The catalog 𝒦 is a relational database that
describes what the system holds, with the schema

| Relation | Meaning |
| --- | --- |
| `class(κ, r_base)` | an instrument class and the finest resolution any bundle holds for it |
| `bundle(b, version, built_at, tested)` | a bundle, its version, when it was built, whether its tests passed |
| `provides(b, κ, R, C, from, to, availability, rows)` | bundle b holds relation R of class κ on calendar C over [from, to], with its availability convention and row count |
| `instrument(b, κ, A, from, to, attr, value)` | an instrument of b with its listing span and its attributes (asset class, point value, currency, exchange, a classification snapshot) |
| `list(b, L, A, from, to, reviewed)` | a named point-in-time list (an index's constituents, a research universe) with the date it was reviewed |
| `label(b, R, col, ℓ)` | a label a column of R holds (the vocabulary) |
| `series(b, N, C, from, to)` | a named series on its native calendar |
| `test(b, name, passed_at, exceptions)` | a bundle test's result |

𝒦 is a relational database over finite domains, so it is read with the
language's own query form (section 8): `? provides(B, equity, close, C, F,
T)` lists every bundle that holds a close of equities, on which calendar and
over which span; `? instrument(B, future, A, F, T, point_value, V)` lists the
contracts. A bundle is the view of 𝒦 restricted to one b, and the manifest
of `src/bundle.rs` is one row of `bundle` with its `provides` rows.

**Definition 3.11 (demand).** The `data` block of a strategy is a conjunctive
query D over 𝒦 whose answers are bindings β: for every primitive relation,
series and list the program reads, a concrete (bundle, relation, calendar)
the program reads it from.

```
data {
  class    equity
  need     bars @1d from 1990-01-01          # open, high, low, close, volume at @1d or finer, from the date on
  need     actions, listing                  # split, dividend, delisted; listed
  need     lists "SP500", "SP400", "SP600"
  need     series "SPXTR" @1d, "VIX" @1w
  require  tested                            # or: allow untested (warned on every run)
  prefer   latest                            # or: version 2026.10; or: base finest
}
```

Each `need` is a conjunct provides(b, κ, R, C, f, t, …) ∧ C ≼ C_need ∧ f ≤
from ∧ t ≥ to: a bundle satisfies a need on a finer calendar, from which the
needed one is a derived view (3.3), and over a span that covers the one
asked for. `require` adds predicates on `bundle` and `test`; `prefer` is a
total preorder on the answers (tested before untested, a pinned version, the
latest version, the finest base, the fewest bundles). The demand is closed
under the catalog's vocabulary: every label literal and series name a rule
uses must occur in a `need`, and WF-15 checks it against the binding.

**Proposition 3.12 [E] (resolution).** Answering D over 𝒦 is conjunctive-query
evaluation: polynomial in |𝒦| for a fixed D (its data complexity is in
AC⁰), and the preferred answer is a sort of the answers. Reference: Chandra
and Merlin 1977; Vardi 1982; Abiteboul, Hull and Vianu 1995, ch. 6. The
kernel resolves D when a run starts, records β with the run, and refuses to
run when D has no answer, naming the `need` no bundle satisfies and the
nearest bundles with the span and calendar each holds.

**Proposition 3.13 [N] (identity of a run, restated).** With D in the
program, M(P, 𝒦) = M(P, β(D, 𝒦)), and two runs with equal H(P) and equal β
produce identical decisions. The program hash covers D and not β: a strategy
that must reproduce bit for bit pins `version` in D, which makes β a
function of D alone; a strategy that floats records β per trial, and the
study treats a change of β as a change of data and never as a change of
program. *Proof.* Proposition 2.2 with B = β(D, 𝒦). ∎

What this replaces. `env equities_1w` named a shape of data built for one
book (R1, R2); `bundle ndlake@2026.10`, the first draft of this document,
named one build of data. The demand names what the strategy needs and lets
the kernel find it: MW14 runs on any bundle that holds daily equity bars,
actions, listing and the three index lists from 1990, whatever its base
resolution, and a bundle that arrives later (a minute feed) satisfies the
same demand through the views of 3.3 while the program reads unchanged.

**Expressibility.** `need` admits the catalog's whole vocabulary: an
instrument predicate (`need instruments where asset_class = future and
exchange in {CME, CBOT}`), an availability convention (`need bars @1m
available recorded`, so a strategy that reasons about arrival refuses data
stamped at bar close), a minimum history (`from`), a resolution bound. What
it does not admit is data that is not in 𝒦: a value typed into a program is a
parameter (W5), and a table of values is inadmissible (B-9, item 4).

**The developer's side.** The same `?` form reads a bundle's contents
without a strategy: `abt data query "? close(A, T, P) at 2020-03-06 for A =
AAPL" --bundle B` reads the store; `abt data resolve` prints the binding a
`data` block would get and why each other answer ranked below it. The
storage is laid out for the kernel (section 8.2: columnar, blocked by bar,
sorted by identity, partitioned by month), and that layout is what the
queries read, so there is no second copy for analysis.

[D] A per-strategy environment file was the v1 design and is R1 and R2 of
the review; a strategy naming one bundle was this document's first draft.
Both are rejected because they tie a program to a build of data rather
than to a description of what it needs.

### 3.7 The universe is explicit

Definition 3.9 stands: `universe(A, @T)` is the entity domain, defined by
rules in the program's `universe` block, the complete relation that
range-restricts negation (S-6) and whose rows are an entity's own rows
(7.1). Two forms of rule are admitted, both written out in the program:

- a catalog predicate, reading a list or an attribute the `data` block
  needed: `universe(A, T) :- listed(A, T), member(A, T, "SP500").`;
- an instrument table, written in the block itself, each instrument with the
  first bar it may be traded from:

```
universe {
  instruments {
    ES from 2000-01-01;  NQ from 2000-01-01;  KC from 2001-01-01;  6J from 2001-01-01
    ZB from 2001-01-01;  ZF from 2001-01-01;  ZN from 2001-01-01;  RTY from 2002-01-01
    ZT from 2003-01-01;  CL from 2007-01-01;  GC from 2007-01-01;  HE from 2007-01-01
    HO from 2007-01-01;  LE from 2007-01-01;  NG from 2007-01-01;  PL from 2007-01-01
    RB from 2007-01-01;  SI from 2007-01-01;  ZC from 2007-01-01;  ZL from 2007-01-01
    ZM from 2007-01-01;  ZS from 2007-01-01;  ZW from 2007-01-01;  GF from 2008-01-01
    SB from 2008-01-01;  ETH
  }
  universe(A, T) :- instruments(A, From), session_of(A, T, _), T >= From.
}
```

`instruments` is a relation of the program (identity A, one value `From`, a
Timestamp; an instrument without a date is tradable from its first bar),
checked against the `instrument` rows of the resolved binding: a name the
data does not hold is an error naming it. The checker counts the table as
degrees of freedom (here 26 names and 25 dates) and warns W-list (B-4: an
asset list is a hindsight constant), which is the honest status of HOME-26,
a research-defined universe with start years chosen after the fact. The
first draft of this document put the same 26 names and dates in a bundle
list, `member(A, T, "HOME-26")`, where no reader of the program would see
them; a hindsight constant written out and warned is better than one that is
not.

[D] A list literal inside a rule body stays inadmissible (B-9): the
`instruments` table is a declaration of the `universe` block and not an
expression, so it cannot carry the result of a run made elsewhere, and it
is hashed, counted and warned.

## 4. The program as blocks

A strategy is a sequence of named blocks, each with a fixed schema. Every
field that changes M is required; a missing field is an error naming it and
its admissible values, never a default. A block may be imported from a
library by name and overridden field by field, so that a company's execution
conventions are written once, and the program still shows every field in
`abt show`.

```
strategy NAME {
  data       { class CLASS  need ...  require ...  prefer ... }        # the demand of 3.11
  calendars  { NAME = CONSTRUCTOR ... }
  clock      { decide on CALENDAR [at OFFSET] }
  universe   { [instruments { ... }]  rules for universe(A, @T) }
  params     { NAME : TYPE = LITERAL [in LO..HI] ... }
  rules      { rel ... ; rules ... ; observe ... }
  decisions  { silence hold|flat   orders DEFAULT   decide rules }
  execution  { ... section 4.3 }
  report     { ... section 4.5 }
}
```

### 4.1 `data`, `calendars`, `clock`

`data` is the demand of Definition 3.11: the instrument class the program
trades, the relations, lists and series it needs with the calendar and the
history each must reach, and the predicates and preference the kernel
resolves them by. It names no bundle unless it pins a version. `calendars`
declares the calendars the program uses, from section 3.1's constructors. `clock` names the decision calendar and, for a
sub-daily base, the offset in the session at which decisions are taken; `@T`
in a signature with no calendar is the decision calendar, and `@T: weekly`
names another.

### 4.2 Signatures without modes

A signature lists its entity arguments, its temporal key and its value
arguments; parametric inputs are written in brackets and are always bound by
the caller:

```
rel adj(A: Equity, @T, P: Price<USD>)
rel sma[N: Count](A: Equity, @T, M: Price<USD>)
```

**Definition 4.1 (identity, inputs, outputs).** The identity of a relation is
its entity arguments and its temporal key; its inputs are its bracketed
parameters; its outputs are its value arguments. A call binds the outputs;
the identity arguments may be bound or free at a call.

**Proposition 4.2 [N] (well-modedness is recovered).** With Definition 4.1,
a rule body admits a well-moded order (F-4.1) iff the binding graph (an edge
from each literal to the literals that bind its inputs and its identity
arguments it needs bound) is acyclic over the rule's parametric inputs, and
the checker finds one by topological sort in O(|rule|). *Proof.* Entity and
key arguments are enumerable at every call (the relation is finite by F-4.4),
so the only binding obligations are bracketed inputs, assignments' right
sides, comparisons, negations and aggregations' free variables; these form
the graph, and an acyclic graph has a topological order that is well-moded by
construction. ∎

The `+`/`-` annotations of S-3 are therefore unnecessary for safety; what
they also did, silently, was decide the group of a reduction (R6). That job
moves to the syntax:

```
K = rank(scored(A, T, S) by (S desc, A asc)) within (T)
N = count(A) over (inbook(A, T)) within (T)
```

**WF-13 (explicit grouping).** A reduction or an aggregation names its group
in `within (...)`; every variable of the reduced atom not in the group must
be fresh in the rule, and every variable in the group must be bound. The
Haiku error of R6 is then the error "`A` occurs in the head and is not in
`within (T)`: a rank that varies over `A` must not bind it; write `within (A,
T)` to rank one tuple, or remove `A` from the head". The group of an
aggregation with no `within` is the set of its free variables, as today; the
form is mandatory for `rank` and `top`, and advisory elsewhere.

### 4.3 `execution`

The executor's whole configuration, every field required:

```
execution {
  price        tr_index            # a primitive or a derived Price relation the executor fills and marks at
  orders       moc                 # the order of a decision that names none: market | moo | moc | moo_moc
  lot          fractional          # whole | fractional
  capital      1_000_000 USD fixed # fixed | compounding
  commission   10 bps per side     # per_share P min M | per_contract from catalog | bps X | none
  slippage     none                # fixed X bps | vol X over N bars | none
  impact       none                # sqrt X over N bars adv | none
  participation none               # X of bar volume | none
  financing    none                # cash X debit Y short Z | none
  leverage     unconstrained       # gross <= X (halt|reject) | reg_t | unconstrained
  actions      in_price            # apply | in_price (the price relation carries splits and dividends)
  delisting    last_price          # last_price | haircut { bankruptcy 1, acquisition 0, ... }
  oversize     halt                # halt | clamp | allow
  ruin         halt                # halt | continue
  numerics     two_pass            # two_pass | running   (R13: the floating-point contract is in the program)
}
```

A library may define `execution book_fixed_base { ... }` and a strategy may
write `execution book_fixed_base with { commission 10 bps per side }`; the
checker expands it and `abt show` prints the expanded block. A field whose
value turns a model off (`slippage none`) is reported on every run, as B-5
asks, by reading the block rather than the flags.

**Proposition 4.3 [N].** With `execution` in the program, M(P, B) is a
function of (P, B) and the program hash covers the executor configuration.
*Proof.* Every field of `ExecConfig` (`src/kernel/mod.rs` 572 to 700) is a
field of the block or is derived from one; the study's `spec.exec` becomes
the block's expansion. ∎ The 72 command-line options of `abt run` reduce to
the program path, the bundle path and output paths.

### 4.4 `decisions`

One decision algebra, no mode. A decision is a value of the sum type

```
hold(A, W)        W : Scalar                 a target weight of capital
hold(A, Q)        Q : Quantity<Unit>         a target quantity (signed)
trade(A, Q)       Q : Quantity<Unit>         a delta (signed; buy, sell, short, cover are its signs)
```

each with an optional order term as today. `silence hold` states that an
instrument with no decision at T keeps its position; `silence flat` that it
is closed. The denial constraint of F-5 (one decision per instrument per bar)
is unchanged and is what made the mode declaration unnecessary: two decision
kinds on one instrument at one bar conflict like two targets do.

[D] The alternative, keeping `mode`, was rejected because the mode was never
information about the strategy: it was a check that two decide rules agree,
and the runtime constraint already performs it with a better diagnostic (the
two rules, the bar, the instrument).

MW14's four band rules become three, the symmetric pair collapsed into one test:

```
decide(T, hold(A, W)) :- target(A, T, W), weight_now(A, T, C), abs(W - C) >= band - band_tol.
decide(T, hold(A, W)) :- target(A, T, W), not held(A, T), W >= band - band_tol.
decide(T, hold(A, 0)) :- held(A, T), not target(A, T, _).
```

### 4.5 `report`

```
report {
  calendar   weekly                # the periods the returns are reported on
  periods    52 per year
  window     1991-01-01 .. 2026-07-02   # warm-up is every bar before the window
}
```

The window is the one place a date literal is admitted; it is logged with
every trial and is not part of the program hash, since it is a property of
the run, not of the strategy (S-5's W5 reasoning). The reporting calendar is
a declared calendar, so R8L reports on `sessions of "ES"` and MW14 on
`weekly`, and the study annualises by the block, not by a resolution table
(R4).

## 5. Evaluation: order-independent bodies and sound short-circuiting

The request was a formal ground for "a result that requires A and B should
stop when A is not true" that keeps the system correct. The ground is that a
rule body is a conjunction, that conjunction is commutative, and that the
one place where the current semantics made order matter, partial arithmetic,
can be given an order-independent reading with Kleene's strong three-valued
logic. The kernel is then free to evaluate in any well-moded order and to
stop at the first false literal.

### 5.1 Three-valued literals

**Definition 5.1 (literal value).** For a ground instance of a literal L
over an interpretation I, its value v(L) ∈ {t, f, ⊥}: an atom or negated atom
is t or f by F-2; a comparison or assignment whose expression is defined is t
or f (an assignment is t and binds); an assignment or comparison whose
expression hits a partial point of F (division by zero, log of a
non-positive, sqrt of a negative, a statistic below its minimum count) is ⊥.

**Definition 5.2 (Kleene conjunction).** v(L_1 ∧ … ∧ L_n) = f if some v(L_i)
= f; otherwise ⊥ if some v(L_i) = ⊥; otherwise t. Reference: Kleene 1952,
strong three-valued logic; Fitting 1985 for its use in logic programming.

A ground rule instance *fires* when its body is t, is *excluded* when f, and
is *stuck* when ⊥. M(P, B) is built from fired instances exactly as in F-3,
and a stuck instance contributes nothing.

**Definition 5.3 (demand).** The demanded instances at bar t are the ground
head atoms in the least model of the magic-sets rewriting of P with respect
to the goal decide(t, ·) (F-11, Bancilhon et al. 1986): the atoms some
derivation of a decision at t asks for. Demand is a model-level notion and
does not depend on how the kernel evaluates.

**Definition 5.4 (halting).** A run halts at bar t iff some demanded head
atom has a stuck instance whose relational literals (atoms, negations,
closed-world tests) are all t.

**Proposition 5.5 [N] (order independence).** The sets of fired, excluded and
stuck instances, and the halting condition, are invariant under any
permutation of a rule's body literals. *Proof.* Kleene conjunction is
commutative and associative; Definition 5.3 is a least model, independent of
order; Definition 5.4 quantifies over instances, not over an evaluation. ∎

**Proposition 5.6 [N] (sound short-circuiting).** An evaluator that visits a
rule's literals in any well-moded order, stops at the first literal with value
f, and otherwise visits every literal, computes exactly the fired instances
and detects exactly the halting instances. *Proof.* f absorbs in Definition
5.2, so after an f the body's value is f whatever the rest; an instance with
no f is t or ⊥ according to whether a ⊥ occurs, which the evaluator sees
because it visits every remaining literal; its stuck instances are the
demanded ones by construction of top-down evaluation with memoisation, which
is magic sets with lazy demand (F-11). ∎

What this changes for the author. Today `X = 1 / N, N > 0` halts and `N > 0,
X = 1 / N` does not (S-7, "evaluated top-down ... in the order written").
Under Definition 5.2 both exclude the N = 0 instance, because the guard is f
and f absorbs. A guard may be written anywhere in the rule, and only in the
rule that needs it (R11). What it changes for the kernel: it may reorder
freely, which section 6 uses.

[D] The alternative is to keep left-to-right evaluation and teach "guard
before divide". It is rejected because the semantics of a declarative rule
should not depend on the order of its conjuncts, because the usability runs
show that the rule is not learned, and because the reordering freedom is
what the optimiser needs anyway.

### 5.2 Classes of literals and the plan

**Definition 5.7.** In a well-moded order, a literal is a *generator* when it
binds a variable (an atom with a free identity argument, an aggregation, a
reduction, a temporal builtin), a *test* when it binds nothing (a comparison,
a negation, an atom with every argument bound), and a *computation* when it
is an assignment.

A *plan* for a rule is a well-moded order. Its cost, with the usual
selectivity estimates (Selinger et al. 1979), is the sum over generators of
the expected number of bindings reaching them times their unit cost, plus the
tests reached. The optimiser chooses the plan of least estimated cost; by
Proposition 5.5 every plan computes the same instances.

**Proposition 5.8 [N] (tests first).** Moving a test to the earliest position
at which it is well-moded never increases the cost and never changes the
result. *Proof.* A test reached earlier excludes the same instances (5.5)
before they reach later generators; the test's own cost is paid at most as
many times as before. ∎

This is the formal content of "stop when A is not true": the guard is a test,
it is placed as early as its bindings allow, and the generators behind it are
not reached for excluded instances. The two books gain from it directly:
MW14's `eligible` is a test on `member`, `close` and `dvol` that excludes
most of 4,240 names before the 18-row regression of `clenow` is computed for
them, which today happens because `scored` reads `eligible` first only
because the author wrote it so.

### 5.3 Rule sets as cases

**Definition 5.9 (case form).** For a keyed relation (F-4.6),

```
R(ū, X) :- B, X = case { G_1 -> e_1 ; ... ; G_k -> e_k ; else -> e_0 }.
```

desugars to k + 1 rules, the i-th guarded by G_i ∧ ¬G_1 ∧ … ∧ ¬G_{i−1}, which
requires every G_j to be a conjunction of closed-relation atoms and
comparisons so that its negation is admissible (F-6). The `else` branch is
optional; without it an instance no guard admits is excluded (the rule has
no solution for it), never stuck.

**Proposition 5.10 [N].** The rules of a case form are syntactically
complementary in the sense of F-4's keyedness condition, so the relation is
keyed by construction and the kernel may evaluate the guards in order and
the body of the first that holds only. *Proof.* Guards i and j < i differ by
G_j positive in j and negated in i. Evaluating in order, the first t guard's
branch fires and every later guard is f by its ¬G conjunct; by 5.6 the later
bodies need not be visited. ∎

The latch of MW14 (`lp`, four rules) and the direction of R8L's leg (three
rules with `one` and `short` as parameters) are each one case form, and
`sign(M)` is a function of F (section 7.3).

### 5.4 The two trees

**Definition 5.11 (program tree).** The static structure of a strategy is the
dependency DAG of F-3.4 with its nodes annotated by calendar, identity,
closedness (F-6.3), keyedness (F-4.6), aggregate class (F-8.1), open-loop or
closed-loop membership (F-11.5), and, per rule, its plan and cost class. Its
depth is the longest path from a primitive to `decide`.

**Definition 5.12 (decision tree).** The dynamic structure of a decision
decide(t, d) is its proof DAG in M(P, B): the fired instance of its rule, and
recursively the fired instances of the atoms in that instance's body, down to
primitive tuples; a negated atom's child is the why-not record of F-6
(the first literal with no solution in every attempted instance of the
positive counterpart).

**Proposition 5.13 [N] (size of the trees).** The program tree has at most |P|
nodes and depth at most the number of program strata, which is at most the
number of relations. The decision tree of one decision has depth at most the
program depth times one per keyed temporal step unrolled, and size at most
the number of body atoms along it times the window lengths; with keyed
recursion displayed as one node carrying its previous value, the displayed
tree is at most |P| · w nodes, w the longest window. *Proof.* Immediate from
F-4.7 and the DAG structure. ∎

How a person or a model sees what the program says about the data is the
program tree printed from `decide` upward, each node in the form "relation,
on calendar, keyed by, reads, closed/open, class"; section 8 gives the
command. For MW14 (section 9.1) the longest path has ten nodes: `close` →
`adj_close` → `clenow` → `scored` → `ranked` → `enter` → `latched` →
`in_book` → `target` → `decide`.

## 6. What the semantics licenses the kernel to do

Each optimisation below is a rewrite or a schedule that Proposition 5.5 or a
law of F-8 shows to preserve M(P, B); none is a flag.

| Optimisation | Licence | Where it bites |
| --- | --- | --- |
| Tests first, cheapest generator first | Prop. 5.8; F-5.1 (any order of a stratum's rules) | MW14's eligibility before its regression; R8L's `usable` before any session feature |
| Case forms evaluated as guards then one body | Prop. 5.10 | latches, directions, every 0/1 state |
| Bar views and library features cached per (bundle, calendar, parameters) | F-11.4, Prop. 3.6 | the weekly bars, the TR index, the ATR |
| Open-loop prefix vectorised over time, closed-loop part bar by bar | F-11.5 | everything in MW14 above `held` and `weight_now`; everything in R8L above `decide` |
| Incremental windows and bucket aggregates | F-8.2, F-8.3 | every `rows` and `window` |
| Entity-parallel evaluation between barriers | F-5.3 | the cross-section at each weekly bar |
| Demand restriction of parametric relations | F-11, magic sets | `sma[40]` evaluated for the names that need it |
| Short-circuit on a false guard | Prop. 5.6 | every rule |

What it does not license, and the checker refuses: reassociating floating
point (F-5), fusing finalised aggregates (F-8, non-laws), reading a bucket
before its close (F-7.1).

## 7. The standard library

The library offers formal treatments of the data and nothing that is a
trading idea. `momentum`, `mom_candidate`, `highest`, `sma` as a trend
feature and `bought_within` leave; a strategy that wants a twelve-one
momentum writes it as a return over a lag in its own rules, where the reader
expects it. Every library relation is dimension-polymorphic (F-9) and
calendar-polymorphic (F-13 conjecture 4, adopted as a design assumption and
to be proven by writing the rules), so one `mean_rows` serves prices,
returns and dollar volumes at any calendar.

| Module | Relations | Treatment |
| --- | --- | --- |
| `calendar` | `bar@C`, `traded@C`, `prev_row`, `row_number`, `lag_rows`, `since(E)`, `month_start`, `day_start`, `session_offset` | the calendars of section 3 and an entity's own rows (7.1) |
| `catalog` | `split_factor`, `cum_split`, `adj_close`, `total_return`, `tr_index`, `dollar_volume`, `listed_before`, `delisted_before` | corporate actions applied causally (B-3); `tr_index` replaces the `trclose` data of R2 |
| `stat` | `mean`, `var`, `std`, `cov`, `corr`, `ols_slope`, `ols_r`, `median`, `quantile`, `zscore`, `ewm[λ]`, `cumsum`, `cumprod`, `rolling_max`, `rolling_min`, each over `rows[N]` or `window[δ]` | rolling and recursive moments and order statistics; the aggregate classes of F-8 decide their cost |
| `bars` | `true_range`, `range`, `gap`, `log_return`, `simple_return`, `vwap@C` | bar-level identities |
| `xsec` | `rank`, `rank_average`, `decile[d]`, `quantile_bucket[q]`, `zscore_xs`, `top[k]` | cross-sectional operators, each `within` an explicit group |
| `state` | `latch[enter, stay]`, `carry[R]`, `count_since[E]`, `bars_held`, `entry_price` | keyed temporal recursions (F-4.6) written once |
| `book` | `held`, `flat`, `weight_now`, `exposure`, `drawdown`, `nav_return`, `nav_vol[N]` | views of the executor relations; `weight_now` reads the `execution` block's price and capital |

A bracketed parameter may be a relation (`latch[enter, stay]`): the library
relation is a module template instantiated by substitution at link time
(F-11.1), so the linked program is first order and every theorem applies to
it unchanged.

### 7.1 An entity's own rows

With the universe the entity domain (Definition 3.9), an entity's rows are
the bars at which `universe(A, T)` holds, and the `calendar` module defines,
in the language itself:

```
prev_row(A, T, T1)  :- universe(A, T), prev(T, T0), universe(A, T1) asof T0.
row_number(A, T, 0) :- universe(A, T), not prev_row(A, T, _).
row_number(A, T, K) :- prev_row(A, T, T1), row_number(A, T1, K0), K = K0 + 1.
```

`rows[N]` windows count rows of the universe by default, so MW14's `rows(T,
clenow_w, min clenow_w)` written five times becomes `rows[clenow_rows]`,
and "needs the full window" is the default (7.2). On a calendar C other than
the decision calendar, an entity's rows are the labels of C whose bucket
holds one of its universe bars, so R8L's `rows[250]` on the session calendar
counts the root's sessions whether or not a value was derived for them,
which is the book's "a session with no m still takes a place". The latch is
the two-rule relation

```
latch[enter, stay](A, T) :- universe(A, T), enter(A, T).
latch[enter, stay](A, T) :- universe(A, T), not enter(A, T), stay(A, T), prev_row(A, T, T1), latch[enter, stay](A, T1).
```

whose guards are complementary (`enter` against `not enter`), so it is keyed
by F-4's syntactic condition; it is a temporal recursion through `prev_row`,
admitted by WF-4 since `prev_row` binds a strictly earlier bar.

### 7.2 Windows

Two forms remain, with one minimum convention each. `rows[N]` is the entity's
last N rows and requires all N unless `min K` is written; `window[δ]` is a
calendar duration and requires `min K` always, because a calendar window has
no natural count. The checker warns when a calendar window's `min K` exceeds
the bars the duration can hold on the rule's calendar, and suggests `rows`
(usability fix 4).

### 7.3 Functions added to F

`sign`, `floor`, `ceil`, `clamp(x, lo, hi)`, and `decile(rank, n)` =
ceil(rank / n · 10) are total functions and join F. `log` and `exp` take a
Scalar; a price ratio `P / P0` is a Scalar by F-9, so no `one_price`
parameter is needed. Typed zeros are written as today (`0 USD/share`, `0
shares`), and the checker's T diagnostic for `V > 0` names the typed form.

## 8. Querying the system at every step

The request is a facility to report, at any step, on every relation, every
feature and every variable, and to see what results it offers. The facility
is a query language over M(P, B) at a bar, with provenance.

**Definition 8.1 (query).** A query is a rule whose head is the reserved
relation `?` and whose body is judged by the checker as any rule of P is
(well-modedness, types, causality, closedness), evaluated against the fold's
state at a bar t. A query never adds to P and never changes M.

**Proposition 8.2 [N].** Query evaluation is sound and complete for M(P, B)
at t and runs in the bounds of F-4.7 for its own body. *Proof.* A query is a
rule of a stratum above P's; F-11.2 (splitting) gives M(P ∪ {q}, B) = M(q,
M(P, B)). ∎

The forms, each with its reading:

| Form | Reads | Answer |
| --- | --- | --- |
| `? R(A, T, X) at T = 2020-03-06` | the tuples of R at a bar | a table |
| `? R(A, T, X) for A = "ES" over 2020-01 .. 2020-03` | an entity's history | a table |
| `? decide(T, D) at T` | the decisions | a table with the rule of each |
| `why R(a, t, x)` | the decision tree of Definition 5.12 | the proof DAG to primitive tuples, each node a fired instance with its bindings |
| `why not R(a, t)` | why-not provenance (F-6; Chapman and Jagadish 2009) | for every rule of R, the first literal with no solution under the instance's bindings, and recursively for the atom that failed, down to a missing primitive tuple or a false test |
| `trace decide at T` | every candidate instance of every decide rule at t | per rule, the instances that fired and, for each excluded instance, the literal that excluded it |
| `coverage over WINDOW` | the fired-count of every relation per bar | the "never fired" report of usability fix 1: the first relation on every path to `decide` that is empty over the window, with its `why not` at the first bar |
| `show program` | the program tree (Definition 5.11) | the annotated DAG from `decide` upward |
| `show plan R` | the chosen plan of each rule of R | the order, the class of each literal, the cost estimate |
| `show schedule` | F-11.5 | the open-loop and closed-loop partition |
| `show calendars` | section 3 | each calendar, its bar count, the first and last label |
| `ledger R into FILE` | the whole extension of R over the run | Parquet, one row per tuple per bar |

**Provenance cost.** `why` and `why not` require the kernel to keep, per
derived tuple, the rule and the identities of the body tuples: the
how-provenance of Green, Karvounarakis and Tannen 2007 restricted to one
derivation (the fold produces one, since the model is unique and the plan is
fixed). The record is O(body length) per tuple and is kept inside the memo's
retention window (`docs/assessment.md`, "bounded memo"), so `why` answers for
any bar the memo still holds and `ledger` writes the record out for the rest.

**At every step, live.** The fold exposes the same queries at its current bar
through a socket; a live run answers `? decide(T, D) at now` and `why`
exactly as a replay would, which is Corollary F-7.3 made inspectable.

### 8.1 Observation is demand

**Definition 8.3 (observation).** An `observe` declaration in the `rules`
block, `observe R [where ...] [trace relations | instances]`, adds to the
goals of every bar t the atoms R(·, t, ·), restricted by `where` to bound
identity arguments, so that the demand of Definition 5.3 becomes D_t ∪ O_t.

**Proposition 8.4 [N] (observation changes nothing that is decided).** For
every program, catalog and set of observations O, the decide tuples of the
run with O equal those of the run without it. *Proof.* Observations add
goals, not rules; M(P, B) is the perfect model of P and does not depend on
goals (F-3); by Propositions 5.5 and 5.6 the plan for D ∪ O computes the same
fired instances for every goal in D; the executor reads `decide` only. ∎

**Proposition 8.5 [N] (the cost of observing).** cost(D_t ∪ O_t) − cost(D_t)
is at most the cost of evaluating every rule of each observed R at t over
its identity domain, which for keyed R is O(|Ent|^a · c(w)) by F-4.7.
*Proof.* Memoisation shares every instance D and O have in common; what
remains is O's own instances, bounded by F-4.7. ∎

This is the researcher's case. A rule the plan reaches at few bars, because
an earlier literal excluded most instances (R8L's `aligned` is reached only
where `signal` holds, and `signal` only where the morning qualified), still
has a definite extension at every bar, and observing it materialises that
extension without touching the plan for `decide`: the optimiser elides what
nobody demands, and an observation is a demand. The ledger of section 8 is
the observation of a relation over a run; `tests/observe.rs`
(`query_every_bar_is_the_ledger`) pins, on the v1 prototype, that the ledger
equals what the run derived.

**Stuck instances under observation.** Definition 5.4 restricts halting to
the instances a decision demands. An observed instance that is stuck is not a
halt, since no decision needed it: it is a row of the ledger flagged `stuck`
with the literal and the expression, so a feature that divides by zero
where no decision looks is visible without stopping a run that does not
depend on it.

**Trace levels.** `trace relations`, the default of every run, keeps the
counters C of 8.2; `trace instances` keeps, per rule and bar, the first
excluding literal of every instance attempted (the failure table F of 8.2),
at a cost linear in the instances attempted, which F-4.7 bounds. The coverage
report of the prototype is `trace relations`; `trace instances` is what `why
not` reads without re-evaluating.

### 8.2 The state tables

**Definition 8.6 (kernel state).** The state Σ_t of a run after bar t is the
family of tables below. Every query of section 8 is a read of Σ_t, and every
step of the fold is a function Σ_{t−1} × events_t → Σ_t (B-2). Columns are
typed by the signatures (F-9); `id` identifies a tuple within a relation and
a run.

| Table | Key | Columns | Written by | Retained |
| --- | --- | --- | --- | --- |
| E[R], the extension of R | (bar, identity) | id, the signature's arguments | the bundle (primitives), the executor (the book), the evaluator (derived relations) | primitives: the bundle; derived: the memo's retention horizon (the program's longest lookback plus slack), and the ledger when observed |
| W[g], window state | (rule, literal, group key) | the summary of F-8 for the aggregate (sum and count; Welford moments; the six sums; a monotone deque; an order-statistics tree), and for `rows[N]` the last N rows | the evaluator | the window's length |
| K[R], the carried value | (R, identity) | the previous bar's tuple of a keyed recursive relation | the evaluator | one bar |
| Δ[R], derivations | id | the rule and the ids of the body tuples the fired instance read: how-provenance, one derivation per tuple since the plan is fixed | the evaluator | as E[R] |
| F[ρ], failures | (rule, bar, instance) | the first literal with value f or ⊥ and the bindings at that point | the evaluator at `trace instances` | as E |
| C[R], coverage | (R, bar) | calls, calls with tuples, tuples | the evaluator | the run |
| Π[ρ], plans | rule | the literal order, each literal's class and estimated cost; after the run, the observed calls and tuples from C | the planner, then the evaluator | the run |
| X, the book | bar | positions, cash, nav, fills, working orders, actions | the executor | the run |
| β, the binding | — | the resolved demand (3.11), H(P), the `report` block | the resolver | the run |

**Layout.** E[R] is columnar: one block per bar, blocks sorted by identity
(the v1 `Store` is this with a `Vec<Value>` per tuple; A2 of
`docs/assessment.md` makes the columns typed), so a lookup by (bar, entity)
is a binary search in a block (the indexed stores of the performance work),
a lookup by bar is a block, and a scan over bars is sequential. A derived
E[R] is the memo bucketed by bar, which is what makes eviction linear in what
is dropped. Δ is an append-only array of (rule, ids) parallel to E[R]'s ids,
so a tuple's derivation is an index, not a search. W and K are per group and
per entity, so the per-entity folds between barriers (F-5.3) touch disjoint
rows and need no locks. The bundle's partitions (`log/<relation>/<month>`)
are E[R] for the primitives at rest, so the query layer and the kernel read
one layout.

**Proposition 8.7 [N] (bounded state).** Under WF-4, WF-6, WF-10 and the
retention horizon h (in bars), |Σ_t| without the primitives and the book is
O(|P| · |Ent|^a · (w + h)), w the longest window in bars, independent of t.
*Proof.* A derived E[R] holds at most h bars of at most |Ent|^a tuples each
(keyed, F-4.7), Δ the same, W is bounded by F-7.4, K by one tuple per
identity, C and Π by |P| per bar and per rule. ∎ A checkpoint (B-2) is Σ_t
serialised, and this is its size.

### 8.3 The query algorithms

Each form of section 8 with what it reads and what it costs; n is the number
of tuples of the relation at the bar and d the program depth of 5.11.

| Form | Algorithm | Cost |
| --- | --- | --- |
| `? R(ū) at t` | if E[R] holds bar t, an index lookup; otherwise evaluate R's rules at t under Π (the memo path, magic sets with lazy demand, F-11) and insert into E[R] | O(log n + k) on a hit; the per-bar bound of F-4.7 on a miss |
| `? R(ū) for A over [t₁, t₂]` | the lookup per bar, or the observation of R over the range (8.1) | bars times the above |
| `why R(ā, t)` | from the tuple's id, a depth-first walk of Δ: each node a fired instance (its rule, its bindings reconstructed from the body tuples), its children the body tuples' derivations, a primitive tuple a leaf, a negated atom's child the `why not` of the positive atom | O(size of the proof DAG) ≤ O(d · body length · w) by 5.13; a bar beyond the retention horizon is first replayed from the nearest checkpoint at or before it |
| `why not R(ā, t)` | for each rule of R: bind the head to ā, evaluate the body in Π's order with a recorder; the first literal with value f (or ⊥) is the answer; when that literal is an atom S(…) whose instance is bound, recurse into `why not S(…, t′)`; at `trace instances` the recorder's result is read from F instead | O(cost of R's instances at t) per level, at most d levels; the recursion is finite since each step descends the dependency DAG or, through a temporal builtin, strictly in time (WF-4) |
| `trace decide at t` | evaluate every decide rule at t with the instance recorder: for each instance the first generator produces, the literal that excluded it or the decision it yielded | O(instances attempted at t) |
| `coverage over [t₁, t₂]` | read C; a relation with calls > 0 and tuples = 0 is empty; the root causes are the leaves of the empty subgraph reached from `decide` (the prototype's `coverage_report`) | O(\|P\| · bars) |
| `show program`, `show plan ρ`, `show schedule`, `show calendars` | static, from the IR, Π and the calendars | O(\|P\|) |
| `ledger R into FILE` | the observation of R over the run (8.1), written from E[R] as it is derived, so nothing need be retained | O(\|𝒯\| · \|Ent\|^a · c(w)) |

**Proposition 8.8 [N] (queries do not interfere).** A query never changes a
decision of the run it reads, in replay or live. *Proof.* A `?` on a miss
inserts into E[R] what the evaluator would have derived (the model is
unique); by Proposition 8.4 the added demand changes no decide tuple; `why`,
`why not`, `trace` and `coverage` read Σ_t only. Live, a query runs between
barriers over the Σ_t the barrier closed (F-5.2), which is the state the
replay reaches at t by F-7.3. ∎

**`why not` and the plan.** `why not` reports the first excluding literal in
Π's order. By Proposition 5.5 the set of excluded instances does not depend
on the order, but which literal is reported does; the report therefore names
the plan's order, which `show plan` prints, so a reader is never shown a
literal the run did not evaluate first. `why not ... in written order`
answers in the terms of the text when the plan moved a guard ahead of a
generator.

### 8.4 Program representations

A program exists in five forms, each a function of the one before it, each
readable with one command.

| Form | What it is | Command | Invariant |
| --- | --- | --- | --- |
| 1. text | the blocks of section 4 | the file | — |
| 2. typed IR | the units, signatures, parameters, blocks and rules with their literals and spans, as JSON | `abt ir` | round-trips to text up to whitespace and comments |
| 3. normal form | the IR with variable names canonicalised by first occurrence in a fixed traversal, each body's literals in a canonical order (a sort key of literal kind, relation and argument shape, not the written order, which 5.5 makes semantically void), and the `report` block removed | `abt hash` | H(P) is its hash; equal normal forms are one program for the lineage (F-13 conjecture 3 in its decidable, under-approximating form) |
| 4. program graph | the annotated DAG of 5.11, as the text of `abt show` and as two tables | `abt show`; `abt show --tables DIR` writes `program.parquet` (relation, kind, calendar, identity, complete, stratum, recursive, loop, depth, rules, aggregate class) and `edges.parquet` (from, to, sign, rule) | acyclic after contracting the SCCs; depth as in 5.13 |
| 5. plan | per rule, Π: the order, each literal's class (generator, test, computation) and estimated cost, and after a run the observed calls and tuples | `abt show plan`; `plans.parquet` | every order is well-moded; estimates are monotone in the statistics β carries |

A reader sees a program through form 4; a model is given form 1 to write and
form 4 to check its writing against; the lineage is form 3; the optimiser's
work is form 5; the kernel runs form 2. The prototype on the v1 kernel has
forms 1, 2 (the checker's `Program`) and 4 (`abt show`, `src/observe.rs`);
form 3 is the normalisation of `src/study/lineage.rs` and form 5 the
written order until the planner of 5.2 exists.

### 8.5 The study

The study API (B-7) keeps its calls and extends what every trial records and
what the record answers.

Recorded per trial: H(P) (form 3), the binding β (3.11), the `report` block,
the parameter point, the metrics, the coverage table C summarised over the
run (a relation demanded and never derived is a trial warning in the bias
checklist: a rule that never fires is a degree of freedom that did nothing),
and, when the study declares `observe`, the ledgers of the observed relations
under the trial's directory. The hold-out embargo applies to ledgers as to
metrics: an observed relation is written over the bars the trial may see.

Asked of the record, in the same `?` form over the study's own tables:
`? trial(Id, Lineage, Param, Value, Metric, X)` for the grid; `? coverage(Trial,
R, Tuples)` for which relations held across trials; `? binding(Trial, R, Bundle,
Version)` for which data each trial read, so a lineage whose trials ran on
different bundles is visible as such. The study directory is thereby a
database with the query language of the catalog and of the kernel, and a
report (B-7, `study.report`) is a query over it rather than a format.

## 9. The two books in the second formulation

The programs below are the proposed surface syntax applied to the two books;
they are the acceptance test of the formulation, since a run of each must
equal the Python reference as the current programs do (section 11, step 7).
Both are written to be read against the books' own rule lists.

### 9.1 MW14

```
# MW14: the weekly long-only S&P 1500 momentum book (stageanalyst book.py,
# canon re-frozen 2026-08-14). Rules R1 to R7 are the book's numbering.
strategy mw14 {
  data {
    class    equity
    need     bars @1d from 1990-01-01
    need     actions, listing
    need     lists "SP500", "SP400", "SP600", "ETF"
    need     series "SPXTR" @1d, "VIX" @1w
    require  tested
    prefer   latest
  }
  calendars { weekly = weeks(1) }
  clock     { decide on weekly }

  universe {
    # A name's rows are the weeks it traded; SPY and QQQ are not in the book.
    universe(A, T) :- traded@weekly(A, T), not member(A, T, "ETF").
  }

  params {
    n_enter      : Count = 7
    n_stay       : Count = 28
    clenow_rows  : Count = 18
    ma_rows      : Count = 40
    gate_rows    : Count = 52
    dvol_rows    : Count = 4
    vix_rows     : Count = 4
    rows_a_year  : Scalar = 52
    price_floor  : Price<USD> = 3 USD/share
    dvol_floor   : Notional<USD> = 2_000_000 USD
    liq_decile   : Count = 5
    crowd_ext    : Scalar = 0.5
    crowd_share  : Scalar = 0.25
    crowd_dial   : Scalar = 0.7
    name_cap     : Scalar = 0.15
    min_weight   : Scalar = 1e-6
    band         : Scalar = 0.025
    band_tol     : Scalar = 1e-12
  }

  rules {
    # Weekly inputs, as views of the daily bars (catalog and calendar modules).
    rel dvol(A: Equity, @T, D: Notional<USD>)
    dvol(A, T, D) :- D = mean(X) over (T1 in bucket(weekly, T), dollar_volume(A, T1, X)).

    # R1: the Clenow score over the name's last 18 rows.
    rel clenow(A: Equity, @T, S: Scalar)
    clenow(A, T, S) :- universe(A, T), P0 = 1 USD/share,
        B = ols_slope(log(P / P0), K) over (T1 in rows[clenow_rows], adj_close@weekly(A, T1, P), row_number(A, T1, K)),
        R = ols_r(log(P / P0), K)     over (T1 in rows[clenow_rows], adj_close@weekly(A, T1, P), row_number(A, T1, K)),
        S = (exp(B * rows_a_year) - 1) * R * R.

    # The S&P 1500 is the union of the three indices.
    rel sp1500(A: Equity, @T)
    sp1500(A, T) :- member(A, T, "SP500").
    sp1500(A, T) :- member(A, T, "SP400").
    sp1500(A, T) :- member(A, T, "SP600").

    # The universe of the book: members above the price floor and the dollar-volume floor.
    rel eligible(A: Equity, @T, D: Notional<USD>)
    eligible(A, T, D) :- universe(A, T), sp1500(A, T),
        close@weekly(A, T, P), P >= price_floor,
        D = mean(X) over (T1 in rows[dvol_rows], dvol(A, T1, X)), D >= dvol_floor.

    rel scored(A: Equity, @T, S: Scalar)
    scored(A, T, S) :- eligible(A, T, _), clenow(A, T, S).
    rel ranked(A: Equity, @T, K: Count)
    ranked(A, T, K) :- K = rank(scored(A, T, S) by (S desc, A asc)) within (T).

    # The dollar-volume decile, ties averaged.
    rel liquid(A: Equity, @T)
    liquid(A, T) :- K = rank_average(eligible(A, T, D) by (D asc)) within (T),
        N = count(A1) over (eligible(A1, T, _)) within (T),
        decile(K, N) >= liq_decile.

    # The gate and the calm test on the market series.
    rel gate(@T)
    gate(T) :- value("SPXTR", T, V), M = mean(X) over (T1 in rows[gate_rows], value("SPXTR", T1, X)), V > M.
    rel calm(@T)
    calm(T) :- value("VIX", T, V), M = mean(X) over (T1 in rows[vix_rows], value("VIX", T1, X)), V <= M.

    # R2, R3 and the latch.
    rel ma(A: Equity, @T, M: Price<USD>)
    ma(A, T, M) :- M = mean(P) over (T1 in rows[ma_rows], adj_close@weekly(A, T1, P)).
    rel enter(A: Equity, @T)
    enter(A, T) :- ranked(A, T, K), K <= n_enter, gate(T), liquid(A, T).
    rel stay(A: Equity, @T)
    stay(A, T) :- ranked(A, T, K), K <= n_stay.
    stay(A, T) :- adj_close@weekly(A, T, P), ma(A, T, M), P > M.
    rel latched(A: Equity, @T)
    latched(A, T) :- latch[enter, stay](A, T).

    # R5 and R7: the scale.
    rel extension(A: Equity, @T, E: Scalar)
    extension(A, T, E) :- adj_close@weekly(A, T, P), ma(A, T, M), E = P / M - 1.
    rel crowded(@T)
    crowded(T) :- N = count(A) over (latched(A, T), extension(A, T, _)) within (T), N > 0,
                  C = count(A) over (latched(A, T), extension(A, T, E), E > crowd_ext) within (T),
                  C / N > crowd_share.
    rel scale(@T, S: Scalar)
    scale(T, S) :- bar(T), S = case { not gate(T), not calm(T) -> 0 ;
                                     crowded(T)                -> crowd_dial ;
                                     else                      -> 1 }.

    # R4: equal weights over the latched eligible names, capped.
    rel in_book(A: Equity, @T)
    in_book(A, T) :- eligible(A, T, _), latched(A, T).
    rel target(A: Equity, @T, W: Scalar)
    target(A, T, W) :- in_book(A, T), N = count(A1) over (in_book(A1, T)) within (T),
        scale(T, S), W = least(S / N, name_cap), W > min_weight.
  }

  decisions {
    silence hold
    orders  moc
    # R6: trade at the week's close when the change clears the band; exits always.
    decide(T, hold(A, W)) :- target(A, T, W), weight_now(A, T, C), abs(W - C) >= band - band_tol.
    decide(T, hold(A, W)) :- target(A, T, W), not held(A, T), W >= band - band_tol.
    decide(T, hold(A, 0)) :- held(A, T), not target(A, T, _).
  }

  execution {
    price         tr_index@weekly
    orders        moc
    lot           fractional
    capital       1_000_000 USD fixed
    commission    10 bps per side
    slippage      none
    impact        none
    participation none
    financing     none
    leverage      unconstrained
    actions       in_price
    delisting     last_price
    oversize      halt
    ruin          halt
    numerics      two_pass
  }

  report {
    calendar weekly
    periods  52 per year
    window   1991-01-01 .. 2026-07-02
  }
}
```

What changed and why it is the same book: the data block asks for daily
equity bars, actions, listing, three index lists and two series from 1990
and lets the kernel find them (3.6); the weekly facts are the daily facts
bucketed (3.3); the total-return price is the catalog's `tr_index` derived
from close, dividend and split at the week's last bar (B-3); the S&P 1500 is
written as the union of its three indices rather than a label built in an
ingest script; the VIX four-week mean is a rule; the latch is the library's
`latch`; the decile is a function; the scale is one case form; and the
sixteen flags are the `execution` and `report` blocks. The book's "a stock's
own rows" is the universe, declared.

### 9.2 R8L

```
# MORNIGHT-R8L: the intraday futures opening-range book (d20-research PB-5,
# 2026-09-02 amendment). One leg per root and session at most.
strategy r8l {
  data {
    class    future
    need     bars @1m from 2000-01-01
    need     sessions, point_value, commission
    require  tested
    prefer   latest
  }
  calendars {
    session = sessions                 # each root's own primary sessions
    es_days = sessions of "ES"         # the reporting calendar
  }
  clock { decide on base at session + 30min }   # the bar labelled open + 30 minutes, or the first traded bar after it

  universe {
    # The HOME-26 roots, each from the first year the book trades it (W-list: a hindsight constant, counted).
    instruments {
      ES from 2000-01-01;  NQ from 2000-01-01;  KC from 2001-01-01;  6J from 2001-01-01
      ZB from 2001-01-01;  ZF from 2001-01-01;  ZN from 2001-01-01;  RTY from 2002-01-01
      ZT from 2003-01-01;  CL from 2007-01-01;  GC from 2007-01-01;  HE from 2007-01-01
      HO from 2007-01-01;  LE from 2007-01-01;  NG from 2007-01-01;  PL from 2007-01-01
      RB from 2007-01-01;  SI from 2007-01-01;  ZC from 2007-01-01;  ZL from 2007-01-01
      ZM from 2007-01-01;  ZS from 2007-01-01;  ZW from 2007-01-01;  GF from 2008-01-01
      SB from 2008-01-01;  ETH
    }
    universe(A, T) :- instruments(A, From), session_of(A, T, _), T >= From.
  }

  params {
    atr_rows  : Count = 20
    med_rows  : Count = 250
    med_min   : Count = 100
    k_main    : Scalar = 2
    gap_free  : Scalar = 1
    late_max  : Scalar = 1/3
    side_gap  : Scalar = -0.75
    risk      : Notional<USD> = 6493.912071090746 USD    # 0.6494 % of the base per ATR per leg
  }

  rules {
    # Session features, on the session calendar, from the minute bars.
    rel atr(A: Future, @S: session, V: Price<USD>)
    atr(A, S, V) :- V = mean(R) over (S1 in rows[atr_rows], true_range@session(A, S1, R)).

    # The opening prints, read at a decision bar (the session so far) or at a
    # session's close (the whole session): the same rule on either calendar.
    rel open0(A: Future, @X: base | session, O: Price<USD>)
    open0(A, X, O) :- O = first(P) over (T1 in session(X, [0min, 1min)), open(A, T1, P)).
    rel close_before[M: Duration](A: Future, @X: base | session, C: Price<USD>)
    close_before[M](A, X, C) :- C = last(P) over (T1 in session(X, [0min, M)), close(A, T1, P)).

    # m of a session, for the median; keyed at the session's close.
    rel session_m(A: Future, @S: session, M: Scalar)
    session_m(A, S, M) :- open0(A, S, O), close_before[30min](A, S, C), prev(S, S1), atr(A, S1, V),
        V > 0 USD/contract, M = (C - O) / V.
    rel m_median(A: Future, @S: session, Q: Scalar)
    m_median(A, S, Q) :- Q = median(abs(M)) over (S1 in rows[med_rows] min med_min, session_m(A, S1, M)).

    # At the decision bar: today's prints and the previous session's features
    # (a feature the previous session lacks is missing, not carried).
    rel signal(A: Future, @T, M: Scalar, G: Scalar, V: Price<USD>, Q: Scalar, Late: Scalar)
    signal(A, T, M, G, V, Q, Late) :- universe(A, T),
        session_of(A, T, S), prev(S, S1),
        atr(A, S1, V), V > 0 USD/contract, m_median(A, S1, Q), close@session(A, S1, PC),
        open0(A, T, O), close_before[30min](A, T, C30), close_before[20min](A, T, C20),
        M = (C30 - O) / V, G = (O - PC) / V, Late = (C30 - C20) / (C30 - O).

    rel aligned(A: Future, @T)
    aligned(A, T) :- signal(A, T, _, G, _, _, _), abs(G) < gap_free.
    aligned(A, T) :- signal(A, T, M, G, _, _, _), G * M > 0.

    rel leg(A: Future, @T, Dir: Scalar, V: Price<USD>)
    leg(A, T, Dir, V) :- signal(A, T, M, G, V, Q, Late),
        Dir = case { abs(M) >= k_main * Q, aligned(A, T), Late <= late_max -> sign(M) ;
                     abs(M) <  k_main * Q, G <= side_gap               -> -1 }.
  }

  decisions {
    silence flat
    orders  moo_moc
    decide(T, hold(A, N)) :- leg(A, T, Dir, V), point_value(A, T, PV), N = Dir * risk / (V * PV).
  }

  execution {
    price         close
    orders        moo_moc
    lot           fractional
    capital       1_000_000 USD fixed
    commission    per_contract from catalog
    slippage      none
    impact        none
    participation none
    financing     none
    leverage      unconstrained
    actions       apply
    delisting     last_price
    oversize      halt
    ruin          halt
    numerics      two_pass
  }

  report {
    calendar es_days
    periods  252 per year
    window   2000-01-01 .. 2026-08-14
  }
}
```

The six precomputed print relations and the `clock` relation are gone; what
replaces them is `open0`, `close_before[M]` and the `clock` block, each one
line, and the selection and the usable years are the `instruments` table of
`universe`, where a reader sees the 26 roots and their dates and the checker
counts them. The entry at
"the first traded minute from minute 30" and the exit at "the session's last
traded minute" are what `moo_moc` on the session calendar means (S-6, order
types), stated in the execution block instead of in two data relations.
`late_max : Scalar = 1/3` is a literal expression, admitted in `params` so
that the book's third is not an `one / three`.

## 10. The checker

Judgments added or changed, each one rule with one code as S-8 requires.

| Judgment | Code | Checks | Replaces |
| --- | --- | --- | --- |
| WF-11 keyedness | K | every temporally recursive relation is keyed; case forms are keyed by Prop. 5.10; other rule sets are warned | F-12 item 2 |
| WF-12 open-loop partition | L | reported, never an error | F-12 item 7 |
| WF-13 explicit grouping | G | `rank`, `top` and cross-sectional library calls name `within`; variables outside it are fresh | W4 and the silent grouping of R6 |
| WF-14 closed configuration | P | every block present and every required field set; a field's value admissible; a library block expanded | the 72 options |
| WF-15 vocabulary | V | every label and series name a rule uses occurs in a `need` of `data`, and exists in the resolved binding; every `instruments` name is an instrument of the binding | the silent `"SP500"` |
| WF-17 demand | Q | the `data` block is a conjunctive query of 3.11 with at least one `need`; with a catalog at hand (`abt check --catalog`) it has an answer, else resolution is deferred to the run and the check says so | the environment name |
| W-list | W9 | an `instruments` table is counted as degrees of freedom and warned as a hindsight constant (B-4) | — |
| Observation | O | every `observe` names a relation of the program; a `where` binds identity arguments only | — |
| WF-16 calendars | X (extended) | every calendar named is declared or a class property; bucket aggregates go coarser; `@T: C` on a head is a declared calendar; a series is read in the direction its calendar allows | WF-10 |
| Well-modedness | B, M (extended) | a well-moded order exists (Prop. 4.2); the diagnostic names the variable that no literal can bind and the literal that needs it | bind-before-use in written order |
| Window sanity | W7 | a calendar window's `min K` exceeds what its duration can hold on the rule's calendar | — |
| Typed zero | T (reworded) | a comparison of a dimensioned quantity with a bare 0 says which typed zero to write | — |
| Never-fired | W8 (on run) | the coverage report of section 8 runs on every `abt run` and prints the first empty relation on each path to `decide` | the silence of R12 |

Dropped: the mode declaration and its codes Z and C beyond "at least one
decide"; `+`/`-` on identity arguments; the order-dependent reading of
partial arithmetic. Retained unchanged: WF-3 types, WF-5 closedness, WF-6
causality with the new constraint of Prop. 3.8, WF-7 total orders, WF-8 as
temporal stratification (F-12 item 1), W5 and W6.

## 11. Implementation order

Each step is a change to the crate that leaves the two parity runs equal to
their references; step 7 is the proof that the formulation lost nothing.

1. **Blocks and closed configuration.** Parser and IR for the nine blocks; `ExecConfig` built from `execution`; the study reads the block; `abt run` takes a program and a catalog (or one bundle, the degenerate catalog) and nothing else, with `--param` kept for studies. The current corpus is migrated mechanically (a strategy's header lines become its `data`, `clock` and `decisions` blocks; a default execution library holds today's defaults, expanded in every file). WF-14, WF-17.
1a. **The catalog and the resolver.** The manifest's rows become the catalog relations of 3.10 over every bundle a directory holds; `abt data` lists and queries them; the `data` block resolves to a binding recorded with every run and trial; `abt data resolve` explains the choice.
2. **Calendars and bar views.** `Resolution` becomes a calendar handle; the manifest declares classes and base resolutions; the `calendar` module and the bar-view cache; WF-16; `abt bundle env` generates the environment. MW14's bundle is rebuilt from the daily lake with no weekly step in the ingest.
3. **Sessions and the decision clock.** `session` as a class property, `session(X, [a, b))` as a temporal constraint, `clock { decide on base at session + OFFSET }`. R8L's bundle is rebuilt from the minute bars with no print relations; the memory question of `docs/assessment.md` A2 (columnar stores) is a prerequisite for the 47M-bar bundle and is done here.
4. **Universes, lists and vocabularies.** The `universe` block as the entity domain; `member`, `usable` and the manifest vocabulary; WF-15.
5. **Order-independent bodies, explicit groups, case forms.** Definition 5.2 in the evaluator; the plan chooser of 5.2 (tests first, then by estimated cost); `within`; `case`; keyedness guard; WF-11, WF-13; the `+`/`-` annotations become optional, then removed.
6. **Queries and provenance.** Done on the v1 kernel (section 15): `show`, the coverage report with its root causes and sample-bar explanations, `query --explain` (the `?` and `why not` forms at one level, with the fired instance's bindings), `--ledger` (observation over a run). To do: Δ and F (8.2), `why` over Δ, the recursive `why not`, `trace`, `observe` in the program, `show --tables`, `abt ir` and `abt hash`, and the socket on the fold. `abt explain` is retired into `query --explain`.
7. **Library and the books.** The modules of section 7 replace `corpus/lib`; `corpus/company/*.dsl` are replaced by section 9's programs; the parity scripts are rerun and must match (1,852 of 1,852 weeks; 21,892 of 21,892 legs). The usability experiment is rerun on the same tasks with the same models and its first-attempt pass rate and attempts-to-clean are recorded beside the old ones.

Steps 1, 4 and 5 are parser, checker and evaluator work inside the crate's
present structure; steps 2 and 3 touch the bundle format and the ingest and
are the larger ones; step 6 is new code over the memo. The order is chosen so
that every step is usable on its own and the books can be rerun after each.

## 12. What this does for a small model and for a reader

The usability study's findings, matched to the change that removes each:

| Failure (usability review) | Attempts it cost Haiku | Removed by |
| --- | --- | --- |
| `+A` in the head of a counted or ranked relation; the M message does not name the fix | 9 of 14 on MW14 | no modes on identity arguments (4.2); WF-13's message |
| `rank` grouped by the bound entity; W4 names `top` | fatal on MW14 (every rank 1) | `within` is mandatory for reductions (WF-13) |
| the previous own row, the row number, the cumulative product, the latch | fatal on MW14 (base case on the first bar of the dataset) | `prev_row`, `row_number`, `cumprod`, `latch` in the library (7.1) |
| calendar windows where rows were meant; `52w`; `min N` repeated | fatal on both R8L runs | `rows[N]` with the full window as default; W7 |
| `@1d` features in a `@1m` strategy; X names neither `asof` nor the annotation | 6 attempts on R8L | `@S: session` on the head; the calendar lattice; X's message names the head calendar |
| the minute prints keyed at different minutes | fatal on both R8L runs | the prints are rules at the head's bar (3.5) |
| `ATR > 0`, `log(P)`, Count × Scalar | 2 to 6 attempts per run | typed-zero message; `sign`, `decile`; `log` of a price ratio |
| `;`, `or`, `if ... else` | 9 parse failures | `case` (5.3) |
| guards placed in the wrong rule; legs lost to a broad guard | 167 legs | order independence (5.1) |
| `"SP500"` silently empty | — | WF-15 |
| sixteen flags, three of six invocations | — | `execution` and `report` (4.3, 4.5) |
| a clean check with zero trades, unnoticed | all four Haiku books | the coverage report on every run (8); `why not` |

For a reader, the program now answers the questions a reviewer asks of a
book in the order they ask them: what data (the `data` block and the
calendars), which names (the `universe` block), what the parameters are,
what is computed from what (the `rules`, with `show program` as the diagram),
what is decided (the `decisions` block), how it is filled and charged (the
`execution` block), and how it is scored (the `report` block). Nothing is in
a comment, a flag or an ingest script.

## 13. What is established, new and conjectured in this document

- [E] The partition lattice (3.3); the data complexity of conjunctive queries (3.12); Kleene's strong three-valued logic (5.2); magic sets as demand (5.3); Selinger's cost model (5.2); how-provenance (8, 8.2); the laws of F-8 reused in section 6.
- [N] Closed programs and the identity of a run (2.2, 3.13); the clock calculus over declared calendars (3.4); the cost of derived resolutions (3.6); the causality of session constraints (3.8); well-modedness without modes (4.2); the executor configuration in the hash (4.3); order independence, sound short-circuiting, tests-first, case forms (5.5, 5.6, 5.8, 5.10); the size of the two trees (5.13); query soundness (8.2); observation changes nothing decided and its cost (8.4, 8.5); bounded state (8.7); queries do not interfere (8.8).
- [C] Calendar polymorphism for library relations keeps the checker polynomial (F-13 conjecture 4), assumed by section 7 and settled by writing the rules in step 7. Whether the plan chooser's selectivity estimates need data statistics beyond the bundle manifest's row counts to beat the author's order on the corpus is an empirical question for step 5.

## 14. References added to the foundations' list

Kleene, Introduction to Metamathematics, 1952, §64. Fitting, A Kripke–Kleene
semantics for logic programs, JLP 1985. Abiteboul, Hull and Vianu,
Foundations of Databases, 1995, ch. 6 (conjunctive queries). Selinger, Astrahan, Chamberlin, Lorie
and Price, Access path selection in a relational database management system,
SIGMOD 1979. Green, Karvounarakis and Tannen, Provenance semirings, PODS
2007. Stanley, Enumerative Combinatorics, vol. 1, 2nd ed., 2011, ch. 3.

## 15. The prototype and the Haiku study

Sections 8.1 to 8.3 were prototyped on the v1 kernel before this document
was finished, so that the claim that observability serves a small model
could be tested rather than argued: `abt show` (the program graph, form 4),
the coverage report on every run (`trace relations`, with the never-fired
root causes and a sample-bar explanation), `abt query --explain` (the `?`
form and a one-level `why not`, with the fired instance's bindings) and
`--ledger` (the observation of a relation over a run). `docs/observability.md`
describes them; `src/observe.rs` and `tests/observe.rs` hold them.

Twelve Haiku agents then wrote the two company books on synthetic data in
their environments' layouts and four briefs, six with the commands and six
without (`docs/llm-observability.md`). All twelve programs trade, and the
four company-book programs make exactly the reference decisions, so the
outcome measures did not separate the conditions. What separated them is
what the agents could establish: with the commands, every agent stated with
bindings why each branch of its decision fired or did not, one found and
quantified a defect in the synthetic total-return index that the identical
decisions of its counterpart concealed, one showed its membership literal
inert on the data, and the coverage report caught a partial window and a
dormant branch. The agents also surfaced defects of the tools, the kernel
and the documentation; `docs/llm-observability.md` records each with its
status, and every one is closed in this tree: `least` across an integer
literal and a Scalar, the symbol filter on decisions, the ledger of
entity-input relations and its combination with `--dump`, a run refused
before it starts when an order needs a print the data has no relation for
or a label the data never carries, the daily reading of the average daily
volume at a sub-daily resolution, the false zero-commission warning, the
executor relations' modes in the semantic model, and the builtins the
README's one page lacked. The implementation order of section 11 is
updated accordingly: step 6's first half is done.
