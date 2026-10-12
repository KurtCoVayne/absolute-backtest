# compiler-design.md

Compiler design for the strategy language: from typed IR to executable plan.

Tags on claims: **[E]** an established result, with a reference in section 11. **[N]** a new proposition, with a proof or proof sketch. **[C]** a conjecture, with what would settle it. **[D]** a design decision, with the alternative it rejects and why.

---

## 1. Reading guide and the result in one paragraph

Each section is self-contained to the degree the pipeline allows. Section 2 fixes the stages and their contracts and specifies the Normalise stage, which has no analysis of its own. Sections 3 to 5 specify Analyse, Rewrite and Plan. Section 6 specifies verification by tier. Section 7 specifies observability. Sections 8 and 9 specify acceptance and the order of implementation. Section 10 separates established, new and conjectured claims. Section 11 lists references and, for each research item of the brief, what applies and what does not. Section 12 records the status of each piece in this crate.

**The result.** The compiler is a sequence of six total functions on the typed IR, each with a printable result, ending in a plan the existing kernel executes. Four facts about the semantics make the design short. First, Kleene conjunction is commutative, so the written order of literals is void; this licenses a canonical normal form, a program hash for lineage, test hoisting, and an exact search over literal orders. Second, temporal stratification with keyedness makes evaluation a time-ordered fold that is linear in bars times entities; this makes the cost model additive over literals and the planner's search exact, since rules are short. Third, the determinism contract makes two plans comparable by exact diff of their decisions; this makes translation validation a test rather than a proof, and it is the only equivalence check available, since program equivalence is undecidable. Fourth, every rewrite is restricted to removing duplicate or dead computation, never to changing a formula; this is what keeps the diff exact in double precision. Verification is tiered by decidability: the checker's judgments and abstract interpretation are polynomial and always run; linear feasibility is polynomial and always runs; SMT and constrained-Horn tiers are budgeted and never promised; translation validation and the runtime tier cover what depends on data. Every static verdict is stated over the reals, and section 6.8 states where the double-precision kernel can differ and how the program's numerics field owns the difference.

---

## 2. The pipeline

### 2.1 Stages

| Stage | Input | Output | Exists | Prints |
| --- | --- | --- | --- | --- |
| Parse | source text | typed IR with spans | yes | IR |
| Check | typed IR | `Program` (judgments, strata, closedness, degrees of freedom, diagnostics) | yes | diagnostics |
| Normalise | `Program` | `NIR` (normal form) and its hash | no | NIR text, NIR JSON, hash |
| Analyse | `NIR` | `Analysis` (keyedness, open-loop partition, demand, abstract values, window lengths, warnings) | no | per-relation and per-column tables, warnings |
| Rewrite | `NIR`, `Analysis` | `NIR'` and the rewrite log | no | NIR' text, log |
| Plan | `NIR'`, `Analysis`, bundle statistics | `Plan` | no | plan text, plan JSON, estimated costs |
| Lower | `Plan` | kernel slot assignments, literal orders, aggregate implementations | partly | lowered form |
| Execute | lowered form, binding | run state, book, ledgers | yes | observed costs, coverage |

Each stage is a total function on its input. **[D]** A stage never fails after Check; a stage that cannot improve its input returns it unchanged with a log entry saying so. The alternative, stages that reject programs, is rejected because every property that could justify rejection is already a Check judgment or an undecidable one with a runtime diagnostic.

### 2.2 Invariants across stages

- **I1 (semantics).** For every stage `S` and program `P`, the perfect model of `S(P)` over every database equals that of `P` on every relation that is demanded, observed or queried. Elided relations are absent from the model of `S(P)` but materialisable (section 2.4).
- **I2 (determinism).** The lowered form fixes, for every reduction, the canonical order of identity columns, and no stage introduces a reassociation of floating-point operations. Two runs of the same lowered form on the same binding are bit-identical.
- **I3 (formula identity).** No rewrite changes the sequence of floating-point operations that produces any demanded value; a rewrite may remove a duplicate sequence or an unreachable one. This is stronger than I1 and is what makes section 6.6 exact.
- **I4 (acceptance).** After every stage is enabled, the two production books equal their references to the bit.
- **I5 (one crate).** All stages, the kernel and the verification tiers live in one crate; the SMT and CHC tiers call an external solver process and degrade to "undecided" when it is absent.

### 2.3 Printable results

Every stage result implements one trait with two methods: text rendering for a reader and JSON rendering for tools. The text rendering of `NIR` is valid source of the language, so a normalised program can be re-parsed; this is the round-trip test of the Normalise stage. The JSON rendering carries source spans through every stage, so a warning at Analyse points to the written text.

### 2.4 Elision and materialisation

**[D]** The Rewrite stage may mark a relation *elided* when no demanded, observed or queried relation reaches it. An elided relation keeps its rules in `NIR'` with a flag; the planner emits no work for it. A query or observation that names an elided relation at bar `t` materialises it by evaluating its rules at `t` from the stored state tables; for a temporally recursive elided relation, the kernel replays its rules from the first bar of the run, which costs the relation's own evaluation and nothing more. The alternative, never eliding, is rejected because the open-loop partition and parametric specialisation give most of their benefit by not computing what is not asked for; the alternative of deleting elided rules is rejected because the observability commands must be able to answer about any relation in the program.

### 2.5 Normalise: purpose

Normalise produces a canonical form of a checked program such that two programs with the same meaning under renaming and reordering have the same form, and such that later stages operate on four literal forms only.

### 2.6 Normalise: reduction of derived forms

Each derived form is replaced by core forms. The reductions are those of the semantic model and are repeated here because the Normalise stage is their implementation.

| Derived form | Core form | Note |
| --- | --- | --- |
| `top(N, R(ū), by (ō))` | `R(ū), K = count(·) over (R(ū'), ō(ū') <lex ō(ū)), K < N` | the rank aggregate `K` has the same group identity as the rule; the comparison `K < N` is a core comparison |
| `rank(R(ū), by (ō), as K)` | `R(ū), K0 = count(·) over (R(ū'), ō(ū') <lex ō(ū)), K = K0 + 1` | |
| `resample(R(ū) to cal as T, min K, X = agg(e))` | `X = agg(e) over (R(ū), bucket_cal(T_R) = T, min K)` | the bucket map is a temporal constraint of the aggregation |
| `prev(T, T1)`, `lag(T, d, T1)` | assignments `T1 = prev(T)`, `T1 = lag_d(T)` | partial functions of time; undefined at the first bars |
| `R(ū, T0, v̄) asof T` | `T0 = asof_R(k̄, T), R(ū, T0, v̄)` with `k̄` the identity terms of `ū` | `asof_R` is an interpreted partial function served by the kernel's as-of index; its law is `asof_R(k̄, T) = max{T' ≤ T : R(k̄, T', ·)}` |
| `case` | one rule per arm, each arm's guard conjoined with the negation of every earlier arm's guard | the rules are syntactically complementary by construction (section 3.3) |

**[D]** `asof` is reduced to an assignment rather than to an aggregation with an unbounded window, because an unbounded window would break the bounded-memory property of the kernel, while the as-of index already exists and is bounded by the relation's own extension. The alternative reduction, `T0 = max(T') over (R(k̄, T', ·), T' ≤ T)`, is rejected for that reason.

### 2.7 Normalise: canonical literal order

**[E]** Kleene conjunction is commutative and associative, so the set of fired, excluded and stuck instances of a rule is independent of literal order (the semantics, section 2 of the brief). **[D]** Normalise therefore sorts literals by a key that does not depend on variable names:

1. literal kind, in the order atom, aggregation, assignment, comparison, negation;
2. relation name for atoms and negations, aggregate name then group's relation names for aggregations, function name for assignments, operator for comparisons;
3. argument shape: the pattern of equalities among argument positions and of which positions hold constants, encoded as the sequence of first-occurrence indices of each argument within the literal, with constants rendered by value.

Ties after these three keys are broken by the variable-naming pass below, and a final stable sort on the full key including canonical names makes the order unique. The alternative, keeping the written order, is rejected because the hash must identify a program up to the reorderings the semantics make void.

### 2.8 Normalise: canonical variable names

Variables are renamed `v0, v1, …` in order of first occurrence in a fixed traversal: head arguments left to right, then literals in the canonical order of 2.7, within each literal left to right, within an aggregation the result variable, then the group's literals in canonical order. The traversal is well-defined because 2.7's first three keys do not depend on names. Parameters keep their declared names; they are part of the program's identity.

### 2.9 Normalise: the program hash

**[D]** The hash is a 256-bit digest of the JSON rendering of the `NIR` after 2.6 to 2.8, with the following excluded: source spans, the `report` block, and comments. Included: the `data`, `calendars`, `clock`, `universe`, `params` (names, types, ranges and defaults), `rules`, `decisions` and `execution` blocks. The hash identifies a program for its lineage. The alternative of hashing the source text is rejected because renaming a variable would start a new lineage; the alternative of hashing after Rewrite is rejected because rewrites depend on bundle statistics, which are not part of the program.

**[N] Proposition 2.1.** Two checked programs that differ only by renaming of variables, reordering of literals within rules, reordering of rules, or the written form of a derived form have the same hash. *Proof.* 2.6 maps each derived form to one core form; 2.7 orders literals by a name-free key with a name-dependent tie-break applied after 2.8's naming, which is itself determined by the name-free order; rules are sorted by their canonical rendering before hashing. Each step is a function of the program's structure only. ∎

**[C]** Normalise's canonical form equates most of the rewrites an LLM makes to a strategy it is revising (renaming, reordering, inlining a library relation). What would settle it: the rate, on the LLM-authored corpus, of pairs the authors declare as revisions whose hashes differ after normalisation. Inlining is not equated by Proposition 2.1 and would need an additional, optional inlining pass before hashing.

### 2.10 Normalise: what it prints

The `NIR` as source text (re-parseable), the `NIR` as JSON, the hash, and a table mapping each canonical variable to its written names per rule, so that diagnostics from later stages can be rendered in the author's names.

---

## 3. The analysis stage

Analyse computes six results: keyedness, the open-loop partition, demand, window lengths, abstract values, and warnings. The first four are syntactic; abstract values are the abstract interpretation the brief asks for.

### 3.1 Strata and closedness

Strata and the set of complete relations come from Check and are not recomputed. Analyse adds, for each relation, its strongly connected component in the dependency graph and whether the component is temporally recursive (contains an edge whose rule binds the callee's key through `prev`, `lag`, `prior_window` or a bucket from a finer calendar).

### 3.2 Open-loop partition

Let `X` be the set of relations from which an executor-supplied or kernel-supplied relation (`position`, `cash`, `nav`, `fill`, `decided`) is reachable in the dependency graph, i.e. the relations that read, directly or transitively, the executor's state. **[E]** The rules whose heads are not in `X` form a lower part of the program in the sense of the splitting theorem (Lifschitz and Turner 1994; for stratified programs, immediate from Apt, Blair and Walker 1988): their model is computable before any decision is made. Analyse labels every relation *open-loop* or *closed-loop*. The computation is a reachability closure, `O(|P|)`.

### 3.3 Keyedness

A derived relation in a temporally recursive component must be keyed: at most one tuple per identity and bar. Analyse decides keyedness syntactically and labels each such relation *keyed by construction*, *keyed by guard*, or *keyed at runtime*.

- *By construction*: the relation has one rule, and every positive atom in its body whose variables reach a value column of the head is itself keyed or primitive-with-declared-key.
- *By guard*: the relation has several rules, and every pair is syntactically complementary: one contains `S(ū)` and the other `not S(ū)` with the same terms and `S` complete; or one contains `e < c` and the other `e ≥ c` (or the three other comparison pairs) with the same expression and constant; or the rules come from one `case` form.
- *At runtime*: otherwise. The kernel's guard halts on a second distinct value for a key, with both rules named. The SMT tier (section 6.4) may upgrade *at runtime* to *by guard* when it proves the guards disjoint.

**[E]** Deciding keyedness exactly is deciding disjointness of conjunctive queries with arithmetic, undecidable in general (Shmueli 1993 for equivalence; Levy, Mumick, Sagiv and Shmueli 1993 for the extensions); hence the three labels rather than a verdict.

### 3.4 Demand

For every parametric relation (one with a `+` argument that is not an identity argument), Analyse collects the set of parameter tuples at which it is called, transitively from `decide`, the observed relations, and the queried relations. **[E]** Identity arguments are enumerable at every call (the semantics), so demand on them is total; demand on parametric arguments is the finite set of values the program names, since every call binds a parametric argument to a parameter or a literal. The demand set is therefore static and finite, and the magic-sets rewriting reduces to specialisation per parameter tuple (section 4.5). Analyse also computes the *demanded relation set* `Dm`: relations reachable backward from `decide`, observations and queries. Relations outside `Dm` are candidates for elision.

### 3.5 Window lengths

For every window literal, Analyse computes from the calendar the maximum number of bars the window can contain at the rule's resolution, `w_max`, and the number of bars at the start of history before the window can first hold `K` rows. For `rows(T, N, min K)` the maximum is `N`. These feed the cost model and warning A006.

### 3.6 Abstract interpretation: domains

**[D]** The abstract domain is the reduced product of three non-relational domains per column: dimension, sign and interval, together with a definedness flag.

- *Dimension*: the flat lattice over dimension vectors; it is exact and is carried for completeness, since Check has already established it.
- *Sign*: the eight-element lattice `{⊥, <0, =0, >0, ≤0, ≥0, ≠0, ⊤}`.
- *Interval*: `[l, u]` with `l, u ∈ ℝ ∪ {−∞, +∞}`, `l ≤ u`, and `⊥` for the empty interval. Bounds are stored as doubles rounded outward (section 6.8).
- *Definedness*: `may_undef ∈ {false, true}`; true when the value may be ⊥ in the three-valued semantics.

The reduced product refines the interval by the sign at zero and the sign by the interval. Column values are joined at relation boundaries: a relation's abstract column value is the join over its rules of the abstract value of the head term.

**[D]** Octagons are not used. The relational facts the language produces are comparisons between arbitrary expressions, and the linear ones are decided exactly by the LP tier (section 6.3) at a cost comparable to an octagon closure; the nonlinear ones are outside octagons in any case. The octagon domain would add a widening and a closure algorithm without covering a case the LP tier does not. **[E]** Octagons: Miné 2006. This decision is revisited in section 10 as a conjecture about precision.

### 3.7 Abstract interpretation: transfer functions

Transfer functions map the abstract state before a literal to the state after it. The state is a map from variables to abstract values plus a rule-level `may_undef` flag.

**Atom `R(ū)`.** Each output variable receives the abstract value of the corresponding column of `R`; each input variable's value is met with the column's value. Constants narrow the column.

**Negated atom.** No change to variable values. **[D]** Refinement by negation (for a complete relation with a single-tuple extension) is rejected as rarely applicable.

**Assignment `X = f(ȳ)`.** Interval arithmetic with outward rounding; sign arithmetic by the usual tables; dimension by the typing rules. Specific cases:

| `f` | Interval result | `may_undef` set when |
| --- | --- | --- |
| `+`, `−` | `[l1+l2, u1+u2]`, `[l1−u2, u1−l2]` | never |
| `×` | min and max of the four corner products | never |
| `÷` | standard interval division when `0 ∉ [l2, u2]`; `[−∞, +∞]` otherwise | `0 ∈ [l2, u2]` → warning A001 |
| `log` | `[log l, log u]` on the positive part | `l ≤ 0` → A002 |
| `exp` | `[exp l, exp u]`, sign `>0` | never |
| `sqrt` | `[sqrt max(l,0), sqrt u]` | `l < 0` → A003 |
| `abs` | `[0, max(|l|,|u|)]` if `0 ∈ [l,u]`, else `[min(|l|,|u|), max(|l|,|u|)]` | never |
| `least(x, c)` | `[min(l_x, l_c), min(u_x, u_c)]` | never |
| `greatest(x, c)` | `[max(l_x, l_c), max(u_x, u_c)]` | never |
| `1 / N` for `N : Count` | `(0, 1]` when `N ≥ 1` is known | `N` may be 0 → A001 |
| `prev`, `lag`, `asof` | time; no numeric value | first bars; not a warning |

**Comparison `e1 op e2`.** With intervals `I1`, `I2`: if `I1` and `I2` admit no pair satisfying `op`, the literal is *never true*, warning A004, and the rule's body is `f` in every instance. If every pair satisfies `op`, the literal is *always true*, note A005. Otherwise the comparison refines both sides: for `e1 < e2`, `u1 := min(u1, u2)` and `l2 := max(l2, l1)`, and the refinement is propagated backward through the expression trees of `e1` and `e2` by the inverse interval operations (standard backward propagation; **[E]** Cousot and Cousot 1977; Granger 1992 for the backward operators).

**Aggregation `X = agg(e) over (B; W)`.** Let `[l, u]` be the abstract value of `e` under `B`, and let `n ∈ [K, w_max]` be the group size. The aggregates' abstract results:

| Aggregate | Interval | Sign and notes |
| --- | --- | --- |
| `count` | `[K, w_max]` | `≥ 0`; `Count` dimension |
| `sum` | interval product of `[K, w_max]` and `[l, u]` | |
| `mean`, `first`, `last`, `max`, `min`, `median`, `quantile` | `[l, u]` | order statistics and means lie within the argument's range **[E]** |
| `std` | `[0, (u − l)/2]` | Popoviciu's inequality: variance of values in `[l, u]` is at most `(u−l)²/4` **[E]**; `may_undef` if `K < 2` |
| `cov(x, y)` | `[−c, c]` with `c = (u_x−l_x)(u_y−l_y)/4` | Cauchy–Schwarz with Popoviciu **[E]** |
| `corr` | `[−1, 1]` | note A008 |
| `ols_beta(y, x)` | `cov / var_x` by interval division | `var_x ∈ [0, …]` contains zero; A008 is emitted as an informational note for `ols_beta` and `corr`, since constancy of `x` over a window is never refutable from intervals |
| rank (from `top`, `rank`) | `[1, n]` | |

The group's `min K` and the window's `w_max` are known from 3.5. If `K > w_max` the aggregate can never be defined, warning A006.

### 3.8 Sources of known bounds

1. *Loader invariants*, declared per bundle and per column: prices positive except for instruments the bundle marks as spread-like; volumes and counts non-negative; a bar's `high ≥ max(open, close)`, `low ≤ min(open, close)`.
2. *Aggregate semantics*, as in 3.7.
3. *Declared parameter ranges*: a parameter with range `[a, b]` has that interval; a parameter without a range has its default as a point interval **[D]**, because the program and binding determine the run, and a sweep is a different program hash per point.
4. *Bundle statistics*, optional: per-column minima and maxima observed in the bound bundle. Verdicts that use them are labelled *bundle-relative* in the warning text, since they are sound only for that binding.
5. *Calendar*: `w_max` and `K` per window.

### 3.9 Fixpoint and widening

Analyse runs the transfer functions over the strata in evaluation order. Within a non-recursive stratum one pass suffices. Within a temporally recursive component the rules are iterated to a fixpoint on the abstract state of the component's relations. **[E]** Termination is forced by widening (Cousot and Cousot 1977): after `k` iterations (default `k = 3`), an interval bound that has moved is set to the nearest *threshold* beyond it, where the thresholds are the numeric literals of the program and the parameter range bounds, and to `±∞` when no threshold remains (widening with thresholds, Blanchet et al. 2003). One narrowing pass follows. **[E]** The result is a post-fixpoint of the abstract transfer and hence a sound over-approximation of the concrete collecting semantics (Cousot and Cousot 1977).

**[N] Proposition 3.1 (soundness).** For every rule, every database satisfying the loader invariants, and every ground instance generated during evaluation, the concrete value of every variable lies in its abstract interval, has its abstract sign, and is undefined only if `may_undef` is true. *Proof sketch.* Each transfer function in 3.7 over-approximates the concrete operation on the reals (interval arithmetic is inclusion-monotone; the aggregate bounds are the cited inequalities; backward refinement only removes values that violate a satisfied comparison). The fixpoint with widening is a post-fixpoint. Induction on the evaluation order within a bar and on bars. ∎ Over doubles the statement holds under the error model of 6.8.

### 3.10 Warnings emitted

| Code | Condition | Text template |
| --- | --- | --- |
| A001 | divisor interval contains zero | `divisor <expr> may be zero: interval [l, u]` |
| A002 | `log` argument interval meets `(−∞, 0]` | `log of <expr> may be undefined: interval [l, u]` |
| A003 | `sqrt` argument interval meets `(−∞, 0)` | `sqrt of <expr> may be undefined: interval [l, u]` |
| A004 | comparison never true | `<lit> can never hold: [l1, u1] <op> [l2, u2]; rule <r> never fires` |
| A005 | comparison always true (note) | `<lit> always holds and can be removed` |
| A006 | `K > w_max` | `window <w> holds at most <w_max> bars; min <K> can never be met` |
| A007 | `K` exceeds the rows available before the decision clock's first bar | `window <w> cannot fill before the first decision bar` |
| A008 | `corr` or `ols_beta` with a possibly constant argument (note) | `<agg> is undefined when <x> is constant over the window` |
| A009 | a parameter's declared range makes a guard infeasible | `guard <lit> is infeasible for every value of <param> in [a, b]` |
| A010 | widening reached `±∞` on a recursive relation (note) | `no finite bound for <R>.<col>; the CHC tier may refine it` |
| A011 | a decision quantity or weight has an interval that permits a sign the decision kind forbids | `<decision> may carry a negative <field>` |

A001 to A003 and A008 are reported with the rule and literal; they never remove a runtime check. A004 licenses the dead-rule rewrite (section 4.2) under the margin condition of 6.8. A006, A007 and A009 are reported as warnings because the brief forbids refusals outside Check.

### 3.11 What Analyse prints

Per relation: kind, calendar, stratum, component, temporally recursive or not, keyedness label, open- or closed-loop, demanded or elidable, demand set for parametric relations, and per column its abstract value. Per rule: the abstract state after each literal. All warnings with spans.

---

## 4. The rewrite stage

Each rewrite is a function from `NIR` to `NIR` licensed by a stated law, with a termination measure, and writing one log entry per application. **[D]** The rewrites are applied in the fixed order R6, R1, R2, R5, R3, R4, to a fixpoint of the whole sequence; confluence is not required, since any sound result is acceptable, and a fixed order makes the log reproducible. The alternative, a confluent rewrite system with critical-pair completion, is rejected as unnecessary for correctness and costly to maintain.

### 4.1 R1: test hoisting

*Rule.* A comparison, negation or assignment is moved to the earliest position at which all its inputs are bound, i.e. immediately after the last literal that binds one of its inputs.

*Law.* **[E]** Kleene commutativity of the body (the semantics). The set of fired, excluded and stuck instances is unchanged; the evaluator, which stops at the first `f`, enumerates fewer generator bindings.

*Interaction with ⊥.* The evaluator stops only at `f`. On ⊥ it continues with every remaining literal whose inputs are bound, so that a later `f` still excludes the instance; the instance is stuck only if no literal is `f`. Hoisting does not change this outcome.

*Termination measure.* The sum over hoistable literals of their distance from their earliest position; each application decreases it by at least one.

*Log entry.* `R1 hoist <literal> in <rule>: position <i> → <j>; law: Kleene commutativity`.

### 4.2 R2: dead rule removal

*Rule.* A rule containing a literal with verdict *never true* from Analyse (A004, with the margin of 6.8) or from the LP tier (6.3) or SMT tier (6.4) is removed from the plan: it is kept in `NIR'` with an *infeasible* flag and contributes no work.

*Law.* **[N] Proposition 4.1.** A rule whose body contains a literal that is `f` in every ground instance contributes no tuple to the immediate-consequence operator and changes no instance's status, since `f` dominates ⊥ in Kleene conjunction. *Proof.* Immediate from the definition of the operator and the body value. ∎

*Termination measure.* The number of unflagged rules.

*Log entry.* `R2 infeasible <rule>: <literal> never holds (<tier>: <evidence>)`.

*Observability.* A query on a relation whose rules are all infeasible reports the flag and the evidence rather than an empty extension.

### 4.3 R3: aggregation merging

*Rule.* Two aggregations in one rule with identical group conjunction, identical window and identical `min K`, whose aggregates belong to the distributive or algebraic class with a common summary, are replaced by one summary computation and two finalisations. Across rules in one stratum with identical groups, a derived summary relation keyed by the group's identity is introduced and both rules read it.

*Law.* **[E]** Algebraic aggregates factor through a bounded summary (Gray et al. 1997). **[D]** Under invariant I3, each aggregate's finalisation from the summary is the canonical formula the numerics field selects, independent of which other aggregates share the summary; for example `mean` is always the running mean of the Welford state when numerics selects running sums, and the two-pass mean when it selects two-pass. Merging therefore removes a duplicate summary computation and changes no operation sequence. The alternative, letting `mean` be computed as `Σ/n` when alone and from Welford when merged with `std`, is rejected because it would break exact translation validation.

*Termination measure.* The number of aggregation literals.

*Log entry.* `R3 merge <agg1>, <agg2> in <rule> over <group>; summary <class>; law: Gray 1997 algebraic factoring`.

### 4.4 R4: window and resample fusion at the summary level

*Rule.* An aggregation over a relation that is itself defined as a `resample` or window aggregation of the same algebraic class, where the inner relation is demanded only as a summary (no other reader finalises it), is replaced by one aggregation over the inner relation's input with the merged summary.

*Law.* **[E]** The summary is a monoid homomorphism: `s(X ⊎ Y) = s(X) ⊕ s(Y)` (Gray et al. 1997). Finalised values do not compose; the rewrite applies only when the inner value is never finalised, which Analyse's demand set establishes.

*Operation-sequence condition.* The fused summary must perform the same floating-point operations in the same order as the two-level computation for I3 to hold. For `sum` and `count` with naive summation the sequences differ (associativity), so **[D]** R4 is enabled only for the numerics settings under which the kernel defines the summary merge as the canonical operation (the Chan–Golub–LeVeque merge for moments, the pairwise-tree order for sums), and is disabled otherwise with a log entry stating why. The alternative, enabling R4 unconditionally and accepting a non-exact diff, is rejected.

*Termination measure.* The number of aggregation nodes.

*Log entry.* `R4 fuse <outer> over <inner> → <fused>; summary <class>; numerics <setting>; law: monoid homomorphism`.

### 4.5 R5: demand restriction and elision

*Rule.* Every parametric relation is specialised to the parameter tuples in its demand set (3.4): one copy of its rules per tuple, with the parameter substituted; relations outside the demanded set `Dm` are flagged *elided*.

*Law.* **[E]** Magic sets preserve the answers to the demanded goals (Bancilhon, Maier, Sagiv and Ullman 1986; Beeri and Ramakrishnan 1991). For stratified programs the transformation must respect strata (Balbin, Port, Ramamohanarao and Meenakshi 1991; Ross 1994). **[N] Proposition 4.2.** Specialisation per static parameter tuple is a magic-sets transformation whose magic predicates are constant, so it preserves temporal stratification. *Proof.* A specialised rule has the same body relations as the original, with a parametric argument replaced by a constant; the dependency graph is unchanged up to relation renaming, so its SCCs and edge labels are unchanged. ∎

*Termination measure.* The number of unspecialised parametric relations.

*Log entry.* `R5 specialise <R> at <tuple>` and `R5 elide <R>: not reached from decide, observations or queries`.

### 4.6 R6: constant folding

*Rule.* An assignment whose right side is closed (parameters and literals only) is evaluated at compile time; the result replaces the expression.

*Law.* Evaluation of closed terms. **[D]** Folding uses the kernel's own arithmetic on doubles in the same operation order, so the folded constant is bit-identical to what the kernel would compute. An expression involving a time function is never closed.

*Termination measure.* The number of closed non-literal expressions.

*Log entry.* `R6 fold <expr> → <value> in <rule>`.

### 4.7 Termination of the stage

**[N] Proposition 4.3.** The Rewrite stage terminates. *Proof.* Order the measures lexicographically: unspecialised parametric relations, unflagged original rules, aggregation nodes, aggregation literals, closed expressions, hoisting distance. R5 decreases the first and leaves the rest unchanged or decreases them (specialisation copies rules; the second measure counts original rules, and copies carry their origin). R2 decreases the second. R4 and R3 decrease the third and fourth. R6 decreases the fifth. R1 decreases the sixth and changes none of the others. Each application strictly decreases the tuple; the order is well-founded. ∎

### 4.8 The rewrite log

JSON lines, one per application: `{ "rewrite": "R3", "rule": "<canonical rule id>", "span": …, "before": "<text>", "after": "<text>", "law": "<citation>", "measure": [before, after] }`. The log is part of the run's record and is printed by the `rewrite-log` command (section 7).

---

## 5. The planner

### 5.1 What the planner decides

For every rule: the evaluation order of its literals, subject to well-modedness. For every aggregation: its implementation. For every relation: its store layout, indexes and memo policy. For every stratum: ahead-of-time or per-bar scheduling and the entity partition for parallelism.

### 5.2 Cost model

The cost of a rule per bar is the sum over positions `i` of `card(i−1) · cost(L_i)`, where `card(i−1)` is the estimated number of partial bindings after the first `i−1` literals and `cost(L_i)` is the estimated cost of evaluating `L_i` once. Estimates:

- An atom with all identity inputs bound and keyed: `cost = 1` lookup; `card` multiplies by the relation's fill ratio (the probability that a tuple exists at that identity and bar).
- An atom with a free identity argument: `card` multiplies by the relation's rows per bar for the bound part (entities per bar for an entity-keyed relation; rows per entity-bar for a non-keyed one).
- A comparison: `cost = 1`; `card` multiplies by a selectivity: from the abstract intervals under a uniform assumption when both sides have finite intervals, otherwise 1/3 **[D]** (the System R default for range predicates, Selinger et al. 1979).
- A negation: `cost = 1`; selectivity from the relation's fill ratio.
- An assignment: `cost = 1`; selectivity 1 except for partial functions of time, whose selectivity is the fraction of bars at which they are defined.
- An aggregation: `cost` by class: distributive and algebraic `O(1)` per bar after warm-up when the window is incremental, `O(w)` when numerics selects two-pass; holistic `O(log w)`.

The statistics the planner needs from the bundle and the binding: bar count; entities per bar (mean and maximum of the universe's size over the run); for each primitive relation, rows per bar and rows per entity-bar and fill ratio; optional per-column minima and maxima (also used by Analyse); for each window, `w_max`. After a run, observed values of every estimated quantity are available from the coverage counters and are stored with the plan (5.6).

### 5.3 Search

**[E]** Join ordering by dynamic programming over subsets is exact and costs `O(n · 2^n)` for `n` literals (Selinger et al. 1979; Moerkotte and Neumann 2006 for the connected-subgraph variant). **[D]** Rules in this language are short (the corpus maximum is nine literals), so the planner enumerates orders by dynamic programming over subsets with the well-modedness constraint as a pruning rule, which is exact. For `n > 14` it falls back to a greedy order (cheapest next literal) and logs the fallback. The alternative, heuristic ordering for all rules, is rejected because exactness is cheap here and removes a source of plan variance between versions.

### 5.4 Aggregate implementation selection

| Class | Numerics `running` | Numerics `two_pass` |
| --- | --- | --- |
| `sum`, `count` | invertible running total with the summation method the numerics field names (naive, Kahan, Neumaier, pairwise) | recompute over the window each bar |
| `max`, `min` | two-stacks sliding window **[E]** (Tangwongsan, Hirzel, Schneider and Wu 2015) | recompute |
| `first`, `last` | the window's end rows | same |
| `mean`, `std`, `cov`, `corr`, `ols_beta` | Welford/Chan–Golub–LeVeque state with evict by the two-stacks scheme over summaries | two-pass over the window |
| `median`, `quantile` | order-statistics tree over the window (insert, evict, select in `O(log w)`) | sort the window |

**[D]** DABA (worst-case constant time) is not selected over two-stacks because the kernel's windows are evicted one bar at a time and amortised bounds suffice; the alternative is reconsidered if latency per bar in live mode is observed to spike at two-stacks' flip.

### 5.5 Layout, indexes and scheduling

Stores remain columnar, blocked by bar, sorted by identity (the existing kernel). The planner chooses, per relation, whether to keep an as-of index (any `asof` reader), a per-entity row window (any `rows` reader), and the memo eviction horizon (the program's lookback, from 3.5). Open-loop strata are scheduled ahead of the decision loop, bar-vectorised, and partitioned by entity across threads; closed-loop strata run per bar; cross-sectional literals (a free entity argument in a rule with a bound bar) form a barrier. **[E]** The entity partition is sound by the Kahn principle (Kahn 1974): per-entity folds share no derived tuples between barriers.

### 5.6 How the plan is exposed

The `plan` command prints, per rule, the chosen literal order with each literal's estimated cardinality and cost, the aggregate implementations, the indexes, and the schedule class; per stratum, the schedule; totals. After a run, the same table carries the observed cardinalities beside the estimates and a ratio column; a ratio outside `[1/4, 4]` is highlighted as an estimate to revisit. The JSON rendering carries the same fields.

---

## 6. Verification

### 6.1 Tiers and labels

Every property is labelled in one of three ways: *decidable* with its complexity; *decidable but costly* with its budget; *undecidable* with the runtime diagnostic that replaces it. The tiers run in order and each may discharge obligations the next would otherwise attempt.

| Tier | Procedure | Label | Obligations |
| --- | --- | --- | --- |
| 0 | Check's judgments | decidable, polynomial | the twelve judgments and the six warnings of the existing checker |
| 1 | abstract interpretation (section 3) | decidable, polynomial; sound, incomplete | A001–A011; dead literals with margin |
| 2 | linear feasibility | decidable; polynomial by interior point, exponential worst case by simplex with fast practice | infeasible linear guards; dead rules; parameter-range infeasibility |
| 3 | SMT: QF_LRA, QF_NRA | QF_LRA decidable, polynomial for conjunctions, NP-complete with Boolean structure; QF_NRA decidable, doubly exponential (Tarski), practical via nlsat | guard implication; guard disjointness for keyedness and conflict freedom; rewrite equivalence in LRA; bounded nonlinear guards |
| 4 | CHC solving (Spacer) | semi-decidable; budgeted | invariants over temporal recursion; refined bounds for A010 |
| 5 | translation validation | decidable per run, exact under I2 and I3 | optimised plan equals unoptimised plan on the corpus and the bundles |
| 6 | runtime | data-dependent | coverage; why-not; causality by truncation; the reference implementation |

### 6.2 Tier 0

The existing checker's judgments and their complexities: name resolution, environment, range restriction, modes, types, temporal recursion, causality, deterministic reduction, stratification, decisions and resolution compatibility are each linear or near-linear in the program; completeness is a least fixpoint over rules, quadratic in the worst case. They always run and are never budgeted.

### 6.3 Tier 2: linear feasibility

*Obligation.* For a rule, the conjunction of its comparisons whose expressions are linear in the rule's variables, with parameter ranges as bounds, is tested for feasibility over the reals. Infeasibility licenses R2.

*Procedure.* **[D]** An exact rational simplex (Bland's rule, `num-rational` arithmetic) implemented in the crate for systems of at most 64 constraints and 32 variables, which covers every rule in the corpus. Rationale: the verdict must be exact, since it licenses a rewrite; floating-point LP solvers (`minilp`, `good_lp`, `highs`) return verdicts subject to tolerances; Fourier–Motzkin elimination is exact but doubly exponential in the number of variables in the worst case **[E]** and is used only as the fallback for systems with at most 6 variables where it is faster than simplex setup. The alternative, an external LP library, is rejected for the verdict's exactness and the crate's build simplicity.

*Complexity.* Simplex with Bland's rule terminates; its worst case is exponential; on systems of this size it runs in microseconds. Interior-point methods are polynomial but inexact in floating point and are not used.

*Example.* `P > P + delta` with `delta ∈ (0, 0.1]` is infeasible: the system `P − P − delta > 0, delta > 0` reduces to `−delta > 0, delta > 0`, infeasible. Abstract interpretation cannot see this because its domain is non-relational; tier 2 does.

*When it cannot decide.* A comparison with a nonlinear term is passed to tier 3 with its linear part as context.

### 6.4 Tier 3: SMT

*Obligations.*

1. *Guard implication*: for rules `r1, r2` of a keyed relation, whether `guard(r1) ⇒ ¬guard(r2)` over the arithmetic part, under parameter ranges and Analyse's intervals as assumptions; success upgrades the keyedness label.
2. *Guard disjointness for conflict freedom*: for two `decide` rules on the same decision kind and instrument, whether their guards are disjoint; success records *conflict-free by guard*; failure leaves the runtime error in place.
3. *Rewrite equivalence in LRA*: for R6 and for R3's finalisation identities, that the rewritten expression equals the original over the reals; this is a check on the rewriter, not on the program.
4. *Bounded nonlinear guards*: feasibility of guards with products or ratios of variables, through QF_NRA.

*Interface.* **[D]** The crate emits SMT-LIB2 and drives a solver as a subprocess, using the `easy-smt` crate's model: commands over the solver's REPL, a replay file per obligation so that any verdict can be reproduced from the file alone, and no link against `libz3`. The alternative, the `z3` crate's bindings, is rejected because it ties the build to a solver version, prevents reproducing verdicts from files, and offers no better control of budgets than a subprocess timeout. Solver absence yields *undecided* for every obligation and a note in the verification report.

*Budget.* Two seconds per obligation, thirty seconds per compilation, both set in the program's `report` block; an obligation that times out is *undecided*. **[E]** QF_LRA conjunctions are polynomial; with Boolean structure NP-complete; QF_NRA is decided by nlsat (Jovanović and de Moura 2012) in practice for small systems.

*Soundness of the reduction.* A disjointness verdict on the arithmetic part is sufficient for disjointness of the rules, since the relational part can only restrict instances further. An implication verdict is likewise sufficient. Neither is necessary, so a failed verdict never produces a warning, only the absence of an upgrade.

### 6.5 Tier 4: constrained Horn clauses

*Obligation.* An invariant over a temporally recursive relation, such as `entry.P > 0` or `peak.V` non-decreasing in time, encoded as a CHC system: one uninterpreted predicate per keyed recursive relation over its value columns and the bar index; one clause per rule; the invariant as the query. A solved system yields a model, which is an inductive invariant **[E]** (Komuravelli, Gurfinkel and Chaki 2014, Spacer; Bjørner, Gurfinkel, McMillan and Rybalchenko 2015 for the CHC framing).

*Use.* A proven invariant refines Analyse's intervals for that relation (replacing a widened `±∞` bound, note A010) and is recorded in the verification report with the solver's certificate file. **[D]** The tier is optional, budgeted at ten seconds per obligation, and never promised: Spacer may not terminate, and the design treats its result as an improvement when present and as nothing when absent. The alternative, making CHC results load-bearing for a rewrite, is rejected because a budgeted semi-decision procedure cannot be a precondition of a deterministic compilation.

### 6.6 Tier 5: translation validation

*Why.* **[E]** Program equivalence for Datalog is undecidable (Shmueli 1993), so no procedure can certify in general that `NIR'` equals `NIR`. Translation validation (Pnueli, Siegel and Singerman 1998; Necula 2000) certifies a particular compilation instead. Alive2 (Lopes, Lee, Hur, Liu and Regehr 2021) does this for LLVM optimisations by SMT over bounded executions; CompCert (Leroy 2009) proves the compiler once in Coq. **[D]** Neither approach is adopted as such: the SMT route would need a symbolic model of the kernel, and a proof assistant is out of scope. Instead the determinism contract makes a stronger and cheaper check possible: the unoptimised and the optimised plans are both executed and their outputs compared for exact equality.

*Procedure.* For every program in the corpus and for the two books, run the plan produced with every rewrite disabled and the written literal order, and the plan produced by the full pipeline, on the same binding. Compare `decide` at every bar, the ledgers of every demanded relation, and the book. **[N] Proposition 6.1.** Under I2 and I3, the two runs are bit-identical if and only if the rewrites and the planner preserved the demanded model. *Proof.* If: I3 guarantees the same operation sequence per demanded value, and I2 guarantees the same reduction order; identical inputs and operations give identical doubles. Only if: a difference in any demanded value is a difference in the model or in the operation sequence, either of which is a defect. ∎

*What it does not cover.* Programs outside the corpus; bindings other than the tested ones. The corpus is therefore part of the compiler's test suite and grows with every defect found.

### 6.7 Tier 6: runtime

Coverage counters (existing), the first empty relation on each path to `decide` (existing), why-not explanations (section 7.4), causality by truncation (run on the binding truncated at sampled bars and diff `decide` up to those bars), and the two Python references. These are the only checks for properties that depend on data: fill, absence-driven decisions, and the bundle's own claims.

### 6.8 Static verdicts over the reals and the double-precision kernel

Every static verdict in tiers 1 to 4 is about the model over the reals. The kernel computes in doubles with correctly rounded IEEE operations in a fixed order (I2). Three consequences, and how the numerics field owns them:

1. *Abstract intervals.* Interval bounds are computed with outward rounding, and every aggregate's interval is widened by the error bound of the summation method the numerics field selects: for naive summation of `n` terms, `n·ε·Σ|x|`; for Kahan or Neumaier, `2ε·|Σx| + O(nε²)·Σ|x|`; for pairwise, `ε·log₂n·Σ|x|` **[E]** (Higham 1993). With this widening, Proposition 3.1 holds over doubles as well as over the reals.
2. *Dead-literal verdicts.* A comparison is removed by R2 only when the real-valued verdict holds with a margin: the gap between the two sides' intervals exceeds `2⁻⁴⁰` relative to their magnitude, a value the numerics field may raise but not lower. Below the margin the literal is kept and A004 is downgraded to a note. The margin bounds the rounding of the compared expressions; translation validation is the backstop.
3. *Runtime guards.* No static verdict removes a runtime check: division by zero, keyedness and decision conflicts are checked by the kernel regardless of tier 1 to 4 results. Static verdicts change what is reported and what is rewritten, never what is guarded.

Moment merges use the Chan–Golub–LeVeque form **[E]** (Chan, Golub and LeVeque 1983), whose relative error in the variance is bounded independently of the mean's magnitude, unlike the sums-of-squares form, which cancels catastrophically when the mean is large relative to the spread. The numerics field selects between running and two-pass formulas; the compiler records the selection in the plan and in the verification report.

---

## 7. Observability

### 7.1 Stage inspection

| Command | Output |
| --- | --- |
| `ir --stage parsed|checked|normalised|analysed|rewritten|planned` | the stage's result as text, or JSON with `--json` |
| `rewrite-log` | the JSON lines of section 4.8, or a text table |
| `plan [--observed <run>]` | section 5.6 |
| `graph` | the existing program graph, extended with keyedness label, open- or closed-loop, demanded or elided, and the abstract value of each column |
| `verify [--tier n]` | the verification report: every obligation, its tier, its verdict, its budget use, and the replay file path |

### 7.2 State tables

After a run, the following tables are queryable. Each has a stable schema and is exported as Arrow IPC with the run.

| Table | Columns | Note |
| --- | --- | --- |
| `extension(R)` | identity columns, bar, value columns | columnar, blocked by bar, sorted by identity |
| `summary(R, window)` | identity, bar, the summary state | the Welford or two-stacks state at each bar |
| `carried(R)` | identity, bar, carried value | the last value of a temporally recursive relation |
| `derivation` | derived tuple id, rule id, body tuple ids | one witness per derived tuple |
| `failure` | rule id, identity, bar, first excluding literal, its bindings | for attempted instances |
| `coverage` | relation, calls, calls with tuples, tuples | existing |
| `plan` | the plan JSON with observed counts | |
| `book` | the executor's ledger | existing |
| `binding` | the resolved data binding | existing |

### 7.3 Provenance

**[E]** How-provenance by semiring annotation (Green, Karvounarakis and Tannen 2007) records, for each derived tuple, a polynomial over the base tuples encoding every derivation. **[D]** The design records one witness per derived tuple (the first derivation in canonical evaluation order), which is the *why*-provenance of a single witness and costs one row per derived tuple with a bounded number of body ids. Full semiring polynomials are rejected because their size is unbounded in the number of alternative derivations, and the questions the commands answer (why is this tuple here; why is that tuple absent) are answered by one witness and by the failure table. The witness is deterministic by I2, so two runs record the same witness.

**[E]** Why-not provenance (Chapman and Jagadish 2009) explains an absent tuple by the operator that removed it. The failure table is the instance-level form of this: for an attempted instance that did not fire, the first excluding literal in evaluation order and the bindings at that point. **[D]** The failure table is recorded for demanded instances at every bar by default, and for all instances at the bars the `report` block samples; full recording is a `report` setting, since the brief forbids run-time flags.

### 7.4 Queries

| Command | Output |
| --- | --- |
| `query <R> <bar> [<identity>]` | the extension at that bar; per rule, the first solution's bindings or the first literal with no solution (existing, extended with the derivation id) |
| `explain <derivation id>` | the derivation tree to primitive tuples, rendered as nested text or JSON |
| `why-not <R> <identity> <bar>` | per rule, the first excluding literal and its bindings; if the excluding literal is a primitive atom with no tuple, the verdict *absence-driven* |
| `ledger <R>` | the extension at every bar (existing) |
| `materialise <R> <bar>` | evaluates an elided relation at that bar (section 2.4) |

Queries read state and change nothing; an observation adds goals before the run and changes no decision, since goals are not rules and the model is unique.

### 7.5 Output formats

Text tables for readers; JSON for tools; Arrow IPC for extensions and ledgers. Every row of every table carries the rule id and span where applicable, rendered in the author's variable names through the Normalise stage's name map.

---

## 8. The acceptance test

1. **The two books.** The weekly cross-sectional equity book (1,852 weekly returns) and the intraday futures book (21,892 legs) must equal their Python references to the bit after every stage is enabled, and after every change thereafter. A stage that breaks either is reverted.
2. **The corpus.** Every positive program compiles through all stages; every negative case is rejected by Check with its expected code; the warnings of section 3.10 are asserted on programs written to trigger each code.
3. **Translation validation.** Section 6.6 on the corpus and the books, run in continuous integration; a difference is a defect in the pipeline until shown otherwise.
4. **Determinism.** Every program in the corpus is run twice with different thread counts; the outputs must be bit-identical.
5. **Truncation.** For each book, the binding is truncated at ten sampled bars and `decide` compared up to each; equality is required.
6. **The usability study, rerun.** Small models write strategies from the study's briefs with the new tools available: `ir`, `plan`, `verify`, `why-not`, `explain`. Measured: first-attempt validity, attempts to a valid program, use of each tool per attempt, and the rate of A-series warnings the model resolves without a human. The comparison is against the study's previous run without these tools. A tool that is never used or never changes an outcome is a candidate for removal.

---

## 9. Implementation order

Each step leaves the crate releasable and the books equal. Sizes are estimates in lines of Rust, including tests, and are expected to be wrong by a factor of two.

| Step | Content | Files | Size |
| --- | --- | --- | --- |
| 1 | Normalise: reductions, canonical order, naming, hash; round-trip test | `compiler/normalise.rs`, `ir/canon.rs` | 900 |
| 2 | Stage printing trait; `ir --stage`; name map | `compiler/print.rs` | 500 |
| 3 | Translation-validation harness on corpus and books, with rewrites disabled (asserts equality of the unoptimised path with the current kernel) | `tests/tv.rs` | 400 |
| 4 | Analyse: components, open-loop partition, keyedness labels, demand, window lengths | `analysis/structure.rs` | 700 |
| 5 | Analyse: abstract domains, transfer functions, fixpoint with widening, warnings A001–A011 | `analysis/ai.rs`, `analysis/domains.rs` | 1,600 |
| 6 | Rewrite: R1 and R5 with the log; TV must pass | `rewrite/mod.rs`, `rewrite/hoist.rs`, `rewrite/demand.rs` | 700 |
| 7 | Planner: statistics, cost model, exact order search, plan printing; lowering reorders literals | `plan/cost.rs`, `plan/search.rs`, `plan/print.rs`, `lower/order.rs` | 1,300 |
| 8 | Canonical aggregate formulas per numerics setting in the kernel; R3, R4, R6; TV must pass | `kernel/agg.rs`, `rewrite/merge.rs`, `rewrite/fuse.rs`, `rewrite/fold.rs` | 1,000 |
| 9 | Tier 2: exact rational simplex, Fourier–Motzkin fallback; R2 | `verify/lp.rs`, `rewrite/dead.rs` | 800 |
| 10 | Tier 3: SMT-LIB2 emission, subprocess driver, replay files, obligations 1–4 | `verify/smt.rs`, `verify/obligations.rs` | 1,000 |
| 11 | Observability: state tables, derivation and failure recording, `explain`, `why-not`, `materialise`, Arrow export | `state/tables.rs`, `state/provenance.rs`, `cli/query.rs` | 1,700 |
| 12 | Tier 4: CHC encoding and Spacer driver; A010 refinement | `verify/chc.rs` | 600 |
| 13 | Acceptance: determinism and truncation tests in CI; usability study rerun | `tests/accept.rs` | 400 |

Steps 1 to 3 change no behaviour and establish the harness. Steps 4 to 7 are the pipeline. Step 8 is the only step that touches the kernel's arithmetic and is the one most likely to disturb the books; it is scheduled after the planner so that the TV harness is mature. Steps 9 to 12 add tiers. Step 13 closes.

---

## 10. What is established, new and conjectured

**Established and applied.** Perfect models of stratified programs; the splitting theorem; Kleene commutativity; magic sets and their stratified form; dynamic-programming join ordering; Gray's aggregate classes; two-stacks sliding windows; Welford and Chan–Golub–LeVeque moments; Kahan, Neumaier and pairwise summation bounds; interval and sign domains with widening and thresholds; Cousot–Cousot soundness; backward interval propagation; Popoviciu's and Cauchy–Schwarz bounds; exact simplex and Fourier–Motzkin; QF_LRA and QF_NRA decision procedures; Spacer; semiring and why-not provenance; translation validation; the Kahn principle; the undecidability of Datalog equivalence and of conjunctive-query disjointness with arithmetic.

**New, with proof or sketch.** Proposition 2.1 (hash invariance); Proposition 3.1 (soundness of the analysis, including the double-precision widening); Proposition 4.1 (dead rules); Proposition 4.2 (specialisation preserves temporal stratification); Proposition 4.3 (termination of the rewrite stage); Proposition 6.1 (exact translation validation under the determinism and formula-identity invariants).

**Conjectured.** (a) The canonical form of section 2 equates most revisions an LLM makes to a strategy; settled by the corpus measurement in 2.9. (b) The non-relational domain of section 3 loses no warning the octagon domain would find on this language's programs once the LP tier is present; settled by implementing octagons behind a flag and comparing warning sets on the corpus. (c) The exact DP search of 5.3 never needs its greedy fallback on programs an LLM writes; settled by the literal-count distribution of the LLM-authored corpus. (d) Spacer discharges the invariants an author would want for the corpus's recursive relations within the budget; settled by running tier 4 over the corpus and counting timeouts.

**Design decisions, with the alternative rejected.** Total stages (2.1); elision with materialisation (2.4); `asof` as an assignment (2.6); hashing the normalised form (2.9); non-relational domains (3.6); point intervals for unranged parameters (3.8); fixed rewrite order without confluence (4); canonical finalisation formulas (4.3); R4 gated on numerics (4.4); exact in-crate simplex (6.3); SMT by subprocess and replay file (6.4); CHC never load-bearing (6.5); execution-based translation validation (6.6); single-witness provenance (7.3).

---

## 11. References, and what applies

For each research item of the brief: the sources read, what applies, and what does not.

**Datalog evaluation and optimisation.** Bancilhon, Maier, Sagiv and Ullman, *Magic sets and other strange ways to implement logic programs*, PODS 1986; Beeri and Ramakrishnan, *On the power of magic*, JLP 1991; Balbin, Port, Ramamohanarao and Meenakshi, *Efficient bottom-up computation of queries on stratified databases*, JLP 1991; Ross, *Modular stratification and magic sets for Datalog programs with negation*, JACM 1994; Ullman, *Principles of Database and Knowledge-Base Systems*, 1988–1989 (semi-naive evaluation, sideways information passing); Selinger et al., *Access path selection in a relational database management system*, SIGMOD 1979; Moerkotte and Neumann, *Analysis of two existing and one new dynamic programming algorithm for the generation of optimal bushy join trees*, VLDB 2006; Jordan, Scholz and Subotić, *Soufflé: on synthesis of program analyzers*, CAV 2016 ([mirror](https://static.aminer.org/pdf/20170130/pdfs/cav/i5oalygn1b0zj7m3ku9iwrc62ubpwtt8.pdf)); Scholz, Jordan, Subotić and Westmann, *On fast large-scale program analysis in Datalog*, CC 2016; Madsen, Yee and Lhoták, *From Datalog to Flix*, PLDI 2016; Sahebolamri, Gilray and Micinski, *Seamless deductive inference via macros* (Ascent), CC 2022. *Applies:* semi-naive evaluation is the fold's per-bar step; magic sets reduce to static specialisation because demand on parametric arguments is a finite set of constants; sideways information passing is the well-modedness constraint on literal order; dynamic-programming join ordering is exact for short rules; Soufflé's choice of specialised index structures per relation and its parallel semi-naive loop are the model for the kernel's stores and entity partition. *Does not apply:* Soufflé's compilation to C++ and Flix's compilation to the JVM target general recursion over unbounded domains, where the fixpoint loop dominates; here recursion is temporal and the loop is the bar loop, so a relational-algebra machine is unnecessary and the existing slot-assignment kernel suffices; Flix's lattice semantics do not apply since recursion here is well-founded by time rather than by lattice height; Ascent's macro embedding is a different product shape (Datalog inside Rust source) and is not adopted.

**Abstract interpretation.** Cousot and Cousot, *Abstract interpretation: a unified lattice model for static analysis of programs by construction or approximation of fixpoints*, POPL 1977; Cousot and Cousot, *Comparing the Galois connection and widening/narrowing approaches to abstract interpretation*, PLILP 1992; Granger, *Improving the results of static analyses of programs by local decreasing iterations*, FSTTCS 1992; Miné, *The octagon abstract domain*, HOSC 2006; Blanchet et al., *A static analyzer for large safety-critical software* (Astrée), PLDI 2003. *Applies:* interval and sign domains, widening with thresholds, narrowing, backward test refinement, the soundness theorem. *Does not apply:* octagons (section 3.6). The Rust ecosystem offers interval-arithmetic crates with directed rounding (`inari`, which implements IEEE 1788) but no maintained general abstract-interpretation framework, so the domains are implemented in-crate on top of a directed-rounding interval type.

**Decision procedures.** Dantzig's simplex with Bland's rule (Bland 1977); Fourier–Motzkin elimination (Schrijver, *Theory of Linear and Integer Programming*, 1986, for the doubly-exponential bound); de Moura and Bjørner, *Z3: an efficient SMT solver*, TACAS 2008; Jovanović and de Moura, *Solving non-linear arithmetic*, IJCAR 2012 (nlsat); Komuravelli, Gurfinkel and Chaki, *SMT-based model checking for recursive programs*, CAV 2014 ([arXiv](https://arxiv.org/pdf/1405.4028)); Bjørner, Gurfinkel, McMillan and Rybalchenko, *Horn clause solvers for program verification*, 2015; the `easy-smt` crate ([docs.rs](https://docs.rs/easy-smt)) for the subprocess-and-replay interface; the `z3` crate for the rejected bindings route. *Applies:* exact simplex for linear guards; SMT-LIB2 over a subprocess with replay files; nlsat for bounded nonlinear guards; Spacer as a budgeted invariant finder. *Does not apply:* interior-point LP (inexact); bindings against `libz3` (section 6.4); CHC as a load-bearing step (section 6.5).

**Translation validation and equivalence.** Shmueli, *Equivalence of Datalog queries is undecidable*, JLP 1993; Levy, Mumick, Sagiv and Shmueli, *Equivalence, query-reachability, and satisfiability in Datalog extensions*, PODS 1993; Pnueli, Siegel and Singerman, *Translation validation*, TACAS 1998; Necula, *Translation validation for an optimizing compiler*, PLDI 2000; Leroy, *Formal verification of a realistic compiler* (CompCert), CACM 2009; Lopes, Lee, Hur, Liu and Regehr, *Alive2: bounded translation validation for LLVM*, PLDI 2021. *Applies:* the undecidability result motivates per-compilation validation; the determinism contract makes validation an exact diff. *Does not apply:* SMT-based validation of optimisations (Alive2) would require a symbolic semantics of the kernel; a verified compiler (CompCert) is out of scope.

**Incremental evaluation.** Gray et al., *Data cube: a relational aggregation operator generalizing group-by, cross-tab, and sub-totals*, Data Mining and Knowledge Discovery 1997; Tangwongsan, Hirzel, Schneider and Wu, *General incremental sliding-window aggregation*, VLDB 2015; Tangwongsan, Hirzel and Schneider, *Low-latency sliding-window aggregation in worst-case constant time*, DEBS 2017; McSherry, Murray, Isaacs and Isard, *Differential dataflow*, CIDR 2013; Budiu, Chajed, McSherry, Ryzhyk and Tannen, *DBSP: automatic incremental view maintenance for rich query languages*, VLDB 2023 ([PVLDB](https://www.vldb.org/pvldb/vol16/p1601-budiu.pdf)); Gupta, Mumick and Subrahmanian, *Maintaining views incrementally*, SIGMOD 1993. *Applies:* Gray's classes decide implementations; two-stacks for non-invertible distributive aggregates. *Does not apply:* retraction machinery (DRed, differential dataflow's negative multiplicities, DBSP's Z-sets), because appending data with later availability never removes a derived tuple with an earlier key; DABA, for the reason in 5.4.

**Provenance.** Green, Karvounarakis and Tannen, *Provenance semirings*, PODS 2007 ([pdf](https://www.cs.ucdavis.edu/~green/papers/pods07.pdf)); Chapman and Jagadish, *Why not?*, SIGMOD 2009; Zhao, Subotić and Scholz, *Provenance for large-scale Datalog*, 2019 ([arXiv](https://arxiv.org/pdf/1907.05045)), for a single-witness design in a production Datalog engine. *Applies:* single-witness why-provenance in the derivation table; instance-level why-not in the failure table. *Does not apply:* full semiring polynomials (section 7.3).

**Synchronous languages.** Caspi, Pilaud, Halbwachs and Plaice, *LUSTRE: a declarative language for programming synchronous systems*, POPL 1987; Halbwachs, Caspi, Raymond and Pilaud, *The synchronous data flow programming language LUSTRE*, Proc. IEEE 1991; Colaço and Pouzet, *Clocks as first class abstract types*, EMSOFT 2003. *Applies:* the calendar layer is a clock calculus; causality analysis is the acyclicity check on instantaneous dependencies; compiled Lustre allocates all state statically, which the kernel matches by sizing window state from the calendar at plan time rather than growing it at run time; compiled Lustre schedules equations once by topological sort, which the planner does per stratum. *Does not apply:* Lustre's single global clock and absence of a cross-section; the barrier is the construct the language adds.

**Floating point.** Kahan, *Further remarks on reducing truncation errors*, CACM 1965; Neumaier, *Rundungsfehleranalyse einiger Verfahren zur Summation endlicher Summen*, ZAMM 1974; Higham, *The accuracy of floating point summation*, SIAM J. Sci. Comput. 1993; Chan, Golub and LeVeque, *Algorithms for computing the sample variance*, The American Statistician 1983; Welford, *Note on a method for calculating corrected sums of squares and products*, Technometrics 1962; IEEE 754-2019. *Applies:* the error bounds in 6.8; the merge formula for moments; correctly rounded operations as the basis of the determinism contract. *Does not apply:* a static reasoner cannot bound the accumulated error of an arbitrary program without the per-operation bounds the numerics field names, which is why the field exists.
