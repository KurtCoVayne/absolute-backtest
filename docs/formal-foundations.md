# Formal Foundations of the Strategy DSL

Oct 8, 2026 · John Gonzalez

> This is the review that the second formulation ([`language-v2.md`](language-v2.md))
> builds on. The tables that were flattened in transit have been re-set; the
> Mechanism column of the decidability table (section 10) was refilled from the
> surrounding text where the flattening had lost it.

The language is stratified Datalog with interpreted arithmetic over the
cross-section and a synchronous dataflow language over time, and that
identification is what lets a compiler guarantee termination, determinism,
causality and polynomial cost by construction. This doc derives those
guarantees from established results, marks what is new, and proposes the
changes to the semantic model that the theory asks for.

## 1. Reading guide and the result in one paragraph

Every claim below is tagged: [E] an established result with a reference,
applied to the language; [N] a new proposition stated for this language, with
a proof or proof sketch; [C] a conjecture, with what would settle it. Where a
result is established but its application needs a lemma that is ours, the
lemma is tagged [N] and the result [E].

**The result.** The language is the intersection of two well-understood
theories. Over the cross-section (all entities at one instant) it is
stratified Datalog with interpreted total functions and aggregation:
function-free, so unification is matching on constants; range-restricted and
well-moded, so built-ins are evaluated rather than solved; stratified, so
negation and aggregation have a unique perfect model. Over time it is a
synchronous dataflow language in the family of Lustre: `prev` is Lustre's
`pre`, resolutions are clocks, `resample` is `when` with aggregation, and
causality is Lustre's causality analysis. The first theory gives finiteness, a
unique model, and polynomial data complexity; the second gives bounded memory,
determinism of parallel evaluation (the Kahn principle) and the
live-equals-replay theorem. Where the two meet, temporal recursion through
negation and aggregation is locally stratified by time, which is strictly
more permissive than the semantic model's WF-8 and still has a unique model.
The one place the existing model is too weak is cost: without a
functional-dependency condition on temporally recursive relations, the model
is finite but can be exponential in the number of bars; with it, evaluation
is linear in bars times entities. The checker's judgments are all decidable
in polynomial time; the properties the checker cannot decide (program
equivalence, decision-conflict freedom, exact functional dependency) are
undecidable in general, and the design replaces each with a runtime check or
a syntactic sufficient condition.

**What is not needed.** Unification with occurs check, SLD resolution,
term-rewriting termination orders for user terms, and the well-founded or
stable-model semantics for non-stratified negation are all unnecessary,
because the restrictions that give the LLM a small language also remove the
constructs those tools exist to handle. Each is discussed once, in the section
where its absence is justified.

## 2. The formal core

The core has four primitive literal forms; the seven forms of the semantic
model are these four plus three derived forms, and keeping the core at four is
what keeps every proof below short.

**Sorts and structures.** A structure 𝔄 fixes: finitely many entity sorts
(Equity), each a finite set of constants with a total order and the
unique-names assumption; a time sort per resolution r, 𝒯_r, a finite totally
ordered set with partial functions prev_r and lag_{r,δ} (the latest element at
or before t − δ for a calendar duration δ) and a bucket map β_{r→r'} : 𝒯_r →
𝒯_{r'} for aligned r ≺ r'; quantity sorts Q⟨d⟩ for each dimension vector d in
the abelian group D = ℤ^3 (currency, share, time; ½ℤ where sqrt is admitted),
interpreted over ℝ; Count, Duration, Bool. Interpreted functions F = {+, −, ×,
÷, log, exp, sqrt, abs, least, greatest} and comparisons are total except at
the points the semantic model names (division by zero, non-positive
logarithm), where evaluation is undefined and the kernel halts.

**Signatures.** A relation symbol R has a sorted arity, a mode vector in {+,
−}^n, one argument marked @ (the temporal key), a resolution r, a closedness
flag (section 6), and, for relations in a temporally recursive component, a
key set K ⊆ args (section 4). Primitive relations have extensions given by the
database E; derived relations are defined by rules; kernel-state and output
relations are as in the semantic model.

**Terms.** Variables, constants of entity sorts, parameters, and literals of
quantity sorts. There are no function symbols in terms: f(x) never appears
inside an atom. Arithmetic lives only in the assignment form. This single
decision makes the Herbrand universe of the relational part finite and
reduces unification to equality of constants.

**The four primitive literal forms.** A rule is H ← L_1, …, L_n with each L_i
one of:

| Form | Syntax | Semantics in an interpretation I |
| --- | --- | --- |
| Atom | R(ū) | ūθ ∈ I(R) |
| Negated atom | ¬ R(ū) | ūθ ∉ I(R), R closed, all of ū bound |
| Assignment | x = f(ȳ) | xθ = f^𝔄(ȳθ), ȳ bound; comparisons are the Boolean case |
| Aggregation | x = α⟨e⟩{ B ; W } | xθ = α over the multiset { eθσ : I ⊨ Bθσ ∧ Wθσ }, where B is a conjunction of atoms, W a conjunction of temporal constraints on the group's time variables relative to the head's @ variable, α a fixed aggregate; the rule has no solution when the multiset is empty (count excepted) or smaller than the declared minimum |

Temporal constraints are t' = t, t' ∈ window_δ(t), t' ∈ prior_window_δ(t),
β(t') = t. They are the only way a time variable other than the head's may be
bound inside a group.

**Derived forms.** Three surface forms reduce to the core, which is why they
inherit its theorems without separate proofs.

- top_k(R(ū), by ō) ≡ R(ū) ∧ n < k where n = count⟨·⟩{ R(ū') ; ō(ū') <_lex ō(ū) }, i.e. a rank aggregate followed by a comparison. Totality of <_lex on R's tuples (WF-7) is what makes n a function of ū.
- resample(R(ū) to r' as t, min K, x = α(e)) ≡ an aggregation whose temporal constraint is β_{r→r'}(t_R) = t, with first/last defined as the aggregate that orders the group by t_R.
- prior_window_δ(t) ≡ window_δ(t) ∧ t' < t; lag and prev are interpreted partial functions of the time sort, i.e. assignments.

**Programs.** A program is a finite set of rules over a signature; a strategy
is a program with a designated output relation decide and a declared decision
resolution. The meaning of a program is a function from databases to
interpretations, M(P, E), defined in section 3 and shown there to be unique
and finite.

## 3. Model semantics

The meaning of a program is the perfect model of a locally stratified
program, which exists, is unique, and is computed by the time-ordered fold;
the semantic model's WF-8 can be relaxed from "no cycle through negation or
aggregation" to "every such cycle descends strictly in time" without losing
any of that.

**Definition 3.1 (ground instance).** For an interpretation I and rule ρ, a
ground instance ρθ is a substitution θ of constants for variables that is
generated by I: it is built left to right, each atom's outputs bound by a
tuple of I, each assignment's output computed, each aggregation's output
computed from I. Well-modedness (section 4) guarantees every ground instance
arises this way and that there are finitely many for finite I.

**Definition 3.2 (immediate consequence).** T_P(I) = I ∪ { Hθ : ρθ a ground
instance of some ρ ∈ P generated by I, all literals of ρθ true in I }.

**Theorem 3.3 [E] (least fixpoint for positive programs).** If P has no
negated atoms and no aggregation, T_P is monotone and finitary on the complete
lattice of interpretations, so by Knaster–Tarski it has a least fixpoint, and
by Kleene's theorem that fixpoint is ⋃_n T_P^n(E). This is the least Herbrand
model of van Emden and Kowalski, with the Herbrand base replaced by the set of
tuples over the active domain extended by evaluated functions. Reference: van
Emden and Kowalski 1976; Lloyd 1987.

**Definition 3.4 (dependency graph).** Nodes are relation symbols; an edge R →
S for each rule with head R and S in the body, labelled − if S occurs negated
or inside an aggregation group, + otherwise. P is stratified if no cycle
contains a − edge.

**Theorem 3.5 [E] (perfect model of stratified programs).** A stratified
program has a unique perfect model, obtained by partitioning into strata P_1,
…, P_k with all − edges pointing to lower strata and taking M_i = lfp(T_{P_i})
over M_{i−1}, negation and aggregation evaluated against M_{i−1}. It is
independent of the chosen stratification. References: Apt, Blair and Walker
1988 for negation; Mumick, Pirahesh and Ramakrishnan 1990 for stratified
aggregation.

The semantic model's WF-8 is exactly Definition 3.4, and Theorem 3.5 is the
justification for section 7 of that doc. The following is the relaxation.

**Definition 3.6 (ground dependency graph; local stratification).** For a
database E, the ground graph G(P, E) has ground atoms as nodes and an edge
from Hθ to each body atom of a ground instance, labelled as above. P is
locally stratified for E if G(P, E) has no cycle with a − edge. Reference:
Przymusinski 1988.

**Theorem 3.7 [E].** A locally stratified program has a unique perfect model;
it coincides with its well-founded model, which is total, and with its unique
stable model. References: Przymusinski 1988; Van Gelder, Ross and Schlipf
1991; Gelfond and Lifschitz 1988.

**Definition 3.8 (temporally stratified).** P is temporally stratified if (i)
every rule satisfies WF-6, so every body atom's temporal key is bound to a
value ≤ the head's, and (ii) for every − edge R → S with R and S in the same
strongly connected component of the (program-level) dependency graph, the rule
binds S's temporal key to a value strictly below the head's, through prev,
lag, prior_window or a bucket β(t') = t with t' at a strictly finer resolution.

**Proposition 3.9 [N].** Every temporally stratified program is locally
stratified for every database E.

*Proof.* Suppose G(P, E) has a cycle a_0 → a_1 → … → a_n = a_0 containing a −
edge. Every edge joins atoms whose relation symbols lie in one SCC of the
program graph (a cycle of ground atoms projects to a closed walk of symbols).
By (i), the temporal key is non-increasing along every edge; by (ii), it
strictly decreases along the − edge. A closed walk with a non-increasing and
somewhere strictly decreasing key is impossible. For the resample case, a
strictly finer resolution's keys map to the coarse key by β, and the fine
bar's close is at or before the coarse bar's close with equality only when the
fine bar is the last in its bucket; treat the pair (resolution, key)
lexicographically, finest-resolution-last, and the argument is unchanged. ∎

**Corollary 3.10 [N].** The meaning M(P, E) of a temporally stratified program
is its unique perfect model, and it is computed exactly by the time-ordered
fold of the kernel: for each t in increasing order, evaluate the program-level
strata restricted to key t, with all atoms of key < t already final.

*Proof sketch.* The fold is the iterated fixpoint of Theorem 3.5 applied to
the local stratification by time; within one t the − edges all point to
earlier keys or to lower program strata, so the per-t computation is a
stratified program over a fixed lower part. Induction on t. ∎

This is the semantics Dedalus assigns to Datalog with a successor relation on
time, where the same argument gives unique models for programs whose negation
is "deductive across time". Reference: Alvaro et al. 2011; Marczak et al.
2012. The practical gain is real: a rule such as "enter only below the mean of
my own past entry prices over the last year" aggregates a recursive relation
across time, is rejected by WF-8 as written, and is accepted and well-defined
under Definition 3.8. The checker's cost is unchanged: SCC decomposition plus a
label check on each edge, O(|P|).

**On the alternatives.** The well-founded semantics (3-valued) and the
stable-model semantics exist precisely for programs that are not locally
stratified. Under 3.8 they coincide with the perfect model, so adopting either
would change nothing for valid programs; adopting them instead of 3.8 would
admit programs whose model has undefined atoms, which is the one outcome an
LLM-authored strategy must never produce silently. Section 6 returns to this.

## 4. Finiteness, termination, and cost

Finiteness follows from safety plus well-foundedness of time, but finiteness
is not enough: a temporally recursive relation can hold exponentially many
tuples per bar unless its value columns are functionally determined by its
keys, and that condition is the one addition this doc asks the semantic model
to make.

**Why the question is non-trivial.** Datalog over a finite Herbrand universe
is trivially finite. This language has interpreted arithmetic, so its universe
is ℝ, and Datalog with arithmetic over an infinite domain is Turing-complete:
nat(0). nat(X+1) ← nat(X). has an infinite least model, and with two counters
one simulates a Minsky machine. Whether a Datalog program with infinite
built-in relations has a finite answer is undecidable in general, and the
literature's response is syntactic sufficient conditions. References:
Ramakrishnan, Bancilhon and Silberschatz 1987; Kifer, Ramakrishnan and
Silberschatz 1988 (finiteness dependencies).

**Definition 4.1 (well-moded rule; WF-1 and WF-2 together).** Write in(L) and
out(L) for the input and output variables of a literal. A rule H ← L_1, …,
L_n is well-moded if in(L_i) ⊆ Params ∪ ⋃_{j<i} out(L_j) for every i, and
vars(H) ⊆ Params ∪ ⋃_j out(L_j). Inputs of an atom are its + positions;
outputs its − positions; an assignment outputs its left side and inputs the
rest; a negated atom and a comparison have only inputs; an aggregation outputs
its result and inputs the free variables of its group. Reference: Dembinski
and Maluszynski 1985; Apt and Pellegrini 1994 for well-modedness; Ullman 1988
for range restriction (safety).

**Lemma 4.2 [E, adapted].** In a well-moded rule, every ground instance
generated by a finite interpretation I binds every variable to a value
computed from finitely many tuples of I, and the number of ground instances is
at most ∏_i |I(R_i)| over the positive atoms. So T_P is finitary and, for a
non-recursive stratum, lfp(T_P) is reached in one round and is finite.
*Proof.* Left-to-right induction over the literals: each atom with bound
inputs contributes finitely many tuples; each assignment one value; each
aggregation one value. ∎

**Definition 4.3 (temporal well-foundedness; WF-4).** In every cycle of the
dependency graph, every rule whose body refers to a relation on that cycle
binds that relation's temporal key strictly below the head's.

**Proposition 4.4 [N] (finiteness and termination).** For a well-moded,
temporally stratified program over a database with finitely many bars |𝒯|,
M(P, E) is finite and the fold terminates after |𝒯| steps, each step a finite
number of rule applications.

*Proof.* Induction on t. At bar t, every recursive reference points to keys <
t, which are final by hypothesis and finite; the rules with head key t form a
non-recursive stratified program over a finite lower part, finite by Lemma
4.2. ∎

**Proposition 4.5 [N] (finite is not polynomial).** There is a well-moded,
temporally stratified program whose model has 2^{|𝒯|} tuples for one entity.

*Proof.* v(A, T, X) ← prev(T, T_1), v(A, T_1, X_1), X = X_1 + 1 together with
v(A, T, X) ← prev(T, T_1), v(A, T_1, X_1), X = 2 · X_1 and a seed v(a, t_0,
1). Every bar doubles the set of values; all values are distinct for X ≥ 1. ∎

Nothing in the semantic model forbids this program. It is contrived, but the
mechanism is not: any temporally recursive relation with two rules whose
guards can both hold for the same key creates a branching value history. The
cure is to require that recursive relations be keyed.

**Definition 4.6 (keyed relation; functional dependency).** A relation R with
key set K (its entity-sorted inputs plus its temporal key) is keyed in I if
I(R) satisfies the functional dependency K → args ∖ K: at most one tuple per
key value.

**Proposition 4.7 [N] (polynomial bound).** If every derived relation in a
temporally recursive SCC is keyed in M(P, E), and every window has calendar
length at most δ_max (so at most w bars at the rule's resolution), then |M(P,
E)| ≤ |P| · |Ent|^a · |𝒯| where a is the maximum number of entity arguments
of a relation, and the fold performs O(|𝒯| · |Ent|^a · |P| · c(w)) operations,
where c(w) is the per-update cost of the aggregates used: O(1) amortized for
associative aggregates, O(log w) for holistic ones (section 8).

*Proof sketch.* By induction on t with Lemma 4.2: at bar t, a keyed relation
holds at most |Ent|^a tuples; a rule's positive atoms with bound keys each
contribute at most one tuple (keyed) or at most w tuples (a window group,
whose aggregate is maintained incrementally rather than re-read); so each rule
does O(|Ent|^a) work per bar beyond the aggregate maintenance. Non-recursive
strata are bounded by Lemma 4.2's product, which is polynomial in the data
since there are finitely many rules. ∎

**How to enforce keyedness.** Deciding statically whether two rules for the
same keyed relation can fire on the same key is query disjointness with
arithmetic, undecidable in general (and NP-complete even for pure conjunctive
queries, by Chandra and Merlin 1977). The design therefore uses two
mechanisms: a syntactic sufficient condition the checker accepts (the rules
for R are pairwise guarded by complementary literals: S(ū) against ¬ S(ū) with
S closed, or e < c against e ≥ c; a single rule whose positive atoms are all
keyed is keyed), and a runtime guard in the kernel (hash on the key; a second
distinct value for a key halts the run with a diagnostic naming both rules,
like a decision conflict). With the guard, Proposition 4.7's bound holds for
every run that completes, which is the honest form of the guarantee. The
corpus example entry(A, T, P) has guards flat(A, T_1) and held(A, T_1, _),
which are disjoint but not syntactically complementary; it passes the runtime
guard and is warned statically.

**What this replaces.** Flix and Datafun obtain termination from recursion
over lattices of finite height and monotone functions. That route is closed
here, since X = X_1 + 1 over ℝ has no finite height; well-foundedness of time
is the substitute, and keyedness is what converts "finite" into "linear in
bars". References: Madsen, Yee and Lhoták 2016; Arntzenius and Krishnaswami
2016.

## 5. Determinism and confluence

The model is unique (section 3), so the semantic question is settled; what
remains is that every evaluation strategy the kernel may use computes that
model, and that the implementation over floating point is a deterministic
function of its inputs.

**Theorem 5.1 [E] (chaotic iteration).** For a monotone operator on a
complete lattice, any iteration that applies the operator's components in an
arbitrary order, provided every component is applied infinitely often
(fairness), converges to the least fixpoint. Reference: Cousot and Cousot
1977. Applied here: semi-naive evaluation, naive evaluation, per-entity
evaluation and the time-ordered fold all compute lfp(T_{P_i}) for each
stratum.

**Theorem 5.2 [E] (Kahn principle).** A network of deterministic stream
processors connected by unbounded FIFO channels, each computing a continuous
function of its input streams, computes a unique least fixpoint regardless of
scheduling. Reference: Kahn 1974. The online kernel between two barriers is
such a network: per-entity folds are nodes, no channel crosses entities, and
barrier reductions are nodes with all entity streams as inputs. Hence the
parallel fold is deterministic under any thread schedule.

**Proposition 5.3 [N] (entity-parallel evaluation).** Let P be temporally
stratified and let P_e be the rules whose atoms all share one entity variable
bound to e. Evaluating P_e for all e in parallel between barriers, then the
remaining rules at the barrier, yields M(P, E).

*Proof.* The rules of P_e and P_{e'} for e ≠ e' share no derived atoms, so
T_{P_e} and T_{P_{e'}} commute; their joint least fixpoint is the product of
the separate ones. The barrier rules form a stratum above them. ∎

**Reductions.** top_k is a function of its input only when the ordering <_lex
is total on the tuples of R at fixed outer bindings. WF-7's syntactic
criterion (the order's keys cover R's identity columns, and identifiers are
totally ordered) is sufficient: two distinct tuples differ in an identity
column, so they differ in the key list. It is not necessary (values could
happen to be distinct), and the checker deliberately does not accept orders
that are total only by accident of the data.

**Floating point is an implementation contract, not a semantic one.** The
semantics is over ℝ; the implementation uses IEEE 754 doubles, which are not
associative. Determinism therefore requires that the order of every reduction
be fixed by the IR: aggregation groups are reduced in the canonical order of
their identity columns (entity, then time), parallel partial reductions are
merged in that same order, and the optimizer (section 11) contains no rule
that reassociates floating-point arithmetic. Under that contract, two runs
with the same inputs are bit-identical, and the interpreted and compiled tiers
agree. This is engineering, but it is what makes section 7's equivalence
theorem testable by diffing.

**Decision conflicts are an integrity constraint, not an inconsistency.** The
perfect model always exists, since no rule has a negated head. "Two distinct
decisions for one instrument at one T" is a denial constraint on the model.
Checking it on a given model is linear; deciding statically that a program
can never violate it is deciding disjointness of two conjunctive queries with
arithmetic, undecidable in general. Reference: Shmueli 1993 for Datalog
equivalence; Chandra and Merlin 1977 for the conjunctive case. The runtime
check in the semantic model is the correct choice, and this is the proof that
it cannot be replaced by a static one.

## 6. Negation

The semantic model's "complete" flag conflates two different reasons a
negation is safe; separating them makes the soundness statement precise and
exposes a diagnostic the kernel should emit.

**Definition 6.1 (intended database).** Let E* be the database that would
hold if every primitive relation were recorded perfectly; E is what the bundle
holds. A primitive R is world-closed if E(R) = E*(R). This is the bundle's
claim for universe, position, cash, fill, delisted, and is tested, not proven.

**Definition 6.2 (self-closed).** A derived relation R is self-closed if its
intended meaning is by definition its computed extension: decide, decided, and
every relation defined by a reduction. "The strategy did not select a" is true
exactly when a ∉ selected, whatever the reason.

**Definition 6.3 (closed).** R is closed if it is world-closed, self-closed,
or derived by rules all of whose body relations are closed. Negation is
permitted only over closed relations (WF-5).

**Proposition 6.4 [N] (closedness propagates).** If R is derived by rules
whose positive and negated atoms and aggregation groups refer only to
world-closed relations, then M(P, E)(R) = M(P, E*)(R); i.e. R is world-closed.

*Proof.* Induction on strata. The hypothesis gives E(S) = E*(S) for every S in
the bodies; T_P restricted to R's rules is a function of those extensions and
of total functions, so the fixpoints agree. ∎

**Proposition 6.5 [N] (soundness of restricted negation).** For a closed R, ¬
R(ū) evaluated as negation-as-failure in M(P, E) agrees with classical
negation in the intended model: for world-closed R, by 6.4 and Definition 6.1;
for self-closed R, by Definition 6.2. For an incomplete relation (say close),
negation-as-failure would assert ¬ close(a, t, ·) whenever the bar is missing,
which is not a classical truth about the world; WF-5 forbids exactly these.

**Corollary 6.6 [N] (two-valued is enough).** Interpret every incomplete
relation three-valued (true if derived, false if its world-closed negation is
derivable, unknown otherwise). Under WF-5, no rule body ever tests an unknown
atom negatively, so the two-valued perfect model and the three-valued reading
agree on every atom a decision depends on. This is why the well-founded
semantics is not needed: its undefined truth value cannot arise on the closed
fragment, and outside it the language admits no negation at all.

**The diagnostic that falls out.** Self-closedness is sound and still lets
missing data drive a trade: with selected defined by top_k over candidates
computed from close, a missing bar for a held name makes it drop out of
selected, and held(a, t) ∧ ¬ selected(a, t) sells it. The program's semantics
are right; the author would want to know. Why-not provenance answers the
question "why is a not in selected": the kernel records, for each negated
self-closed atom that enables a decision, whether the positive counterpart
failed on a missing primitive. Reference: Chapman and Jagadish 2009; Huang et
al. 2008. This is the coverage diagnostic of the bundle doc's roadmap, given a
precise definition: a decision is absence-driven if its derivation passes
through ¬ R(ū) where R(ū) has a failed derivation whose first failing literal
is a primitive atom with no tuple.

**What classical negation would cost.** Allowing ¬ R in heads (explicit
negative facts, as the conceptual spec first suggested) gives extended logic
programs, whose answer-set semantics can be inconsistent and whose consistency
check is coNP-hard in the propositional case. Reference: Gelfond and Lifschitz
1991. The closed-relation discipline achieves the spec's intent (absence is
not falsity) without leaving the stratified fragment.

## 7. Time as synchronous dataflow

Over time the language is a synchronous dataflow language, and that
correspondence supplies causality analysis, a clock calculus, bounded memory
and the live-equals-replay theorem as inherited results rather than new ones.

**The correspondence.** In Lustre a program defines streams by equations; pre
x is the previous value of x; a stream has a clock, and an operator may
combine only streams on the same clock; x when c subsamples to a slower
clock; current holds a slow stream on a faster clock. Lustre's compiler
rejects an equation whose current value depends on itself without a pre
(causality analysis), and a Lustre program runs in bounded memory with no
dynamic allocation. References: Caspi, Pilaud, Halbwachs and Plaice 1987;
Halbwachs et al. 1991; Benveniste et al. 2003 (survey); Colaço and Pouzet 2003
(clocks as types).

| This language | Lustre | Judgment |
| --- | --- | --- |
| a keyed derived relation R(e, @t, v) | a stream per entity e | |
| prev(T, T_1) | pre | WF-4, WF-6 |
| lag, window | bounded delay lines | bounded memory |
| resolution @r | clock | WF-10 |
| resample to coarser r' | when with aggregation | WF-10 |
| as-of join (v2) | current | WF-10 (v2) |
| recursion without prev | instantaneous cycle, rejected | WF-4 |

The cross-section is where the correspondence stops: Lustre has no notion of
a finite set of entities joined at an instant, and that is what the Datalog
half supplies. The two halves meet at the barrier.

**Theorem 7.1 [N] (causality).** Let E|_{≤t} be E with every tuple whose
temporal key exceeds t removed (at every resolution, by bar close). For a
temporally stratified program, every relation R and every t' ≤ t: M(P, E)(R)|_{≤t'}
= M(P, E|_{≤t})(R)|_{≤t'}.

*Proof.* By Corollary 3.10, M(P, E) is the output of the fold; the fold's
state after bar t is a function of the events with key ≤ t only, since WF-6
forbids any body atom with key > t in a rule with head key t, and resample
buckets close at or before the coarse key. The two runs see the same event
prefix up to t and so have the same state. ∎

This is the theorem the semantic model states in section 7 of that doc; the
proof is the fold. The semantic model's empirical re-run on truncated data is
a test of the kernel against this theorem, not of the theorem.

**Proposition 7.2 [N] (prefix monotonicity).** For a temporally stratified
program and an append-only, availability-keyed log L, the map prefix(L) ↦ M(P,
prefix(L)) is monotone: appending events never removes a derived tuple with
key at or before the previous end.

*Proof.* Theorem 7.1 with t the previous end: tuples with key ≤ t are
determined by the prefix. ∎

**Consequence.** Historical revisions, modelled as new tuples with later
availability (bundle doc, section 2), never cause retraction. Semi-naive
evaluation is exact; the DRed deletion-and-rederivation algorithm and
differential dataflow's retractions are unnecessary. Reference: Gupta, Mumick
and Subrahmanian 1993 (DRed); McSherry et al. 2013 (differential dataflow), as
the general machinery the design avoids needing.

**Corollary 7.3 [N] (live equals replay).** If the live feed is written to
the log with the availability times the fold observed, then the decisions of
the live run and of a later replay of the log are identical. *Proof.* Both are
the same fold over the same event sequence; determinism by section 5. ∎ This
is testable by shadow mode, and it is the whole justification for
"construction must be online".

**Where the temporal logics sit.** DatalogMTL extends Datalog with metric
temporal operators (⊡_{[0,20]} "at every instant in the last 20", ◇ "at some
instant"), with past and future directions, recursion through them, and a
rational timeline; its data complexity is PSPACE-complete, and its
forward-propagating fragment remains PSPACE-hard with a falsum predicate.
Reference: Wałęga, Cuenca Grau, Kaminski and Kostylev 2019. LARS adds window
operators over streams with similar hardness in general. Reference: Beck,
Dao-Tran and Eiter 2018. This language's windows are the past-only, discrete,
finite-timeline, aggregate-returning special case: x = α⟨e⟩{ R(…, t', …) ; t'
∈ window_δ(t) } is ⊡-shaped but produces a value rather than a truth value,
and there is no future operator and no dense time. That is why the data
complexity drops from PSPACE to the linear bound of Proposition 4.7: the
hardness of DatalogMTL comes from unbounded propagation along a dense line in
both directions, and every one of those sources is removed.

**Proposition 7.4 [N] (bounded memory).** Under WF-4, WF-6 and WF-10, the
fold state for one entity is bounded by O(Σ_rules w_rule) cells, where w_rule
is the bar count of the rule's longest window at its resolution, independent
of the length of the history. *Proof.* Every reference to the past is through
prev, lag_δ, window_δ, prior_window_δ or a bucket, each bounded by a declared
calendar duration; the incremental summaries of section 8 are of bounded
size; temporal recursion carries one value per keyed relation. ∎ This is
Lustre's bounded-memory property, re-derived for calendar windows.

## 8. Aggregates

An aggregate's algebraic class decides its online cost and which
compositional laws hold for it; the classification is Gray's, the
sliding-window bounds are Tangwongsan, Hirzel and Schneider's, and the laws
and non-laws below are what the optimizer may and may not assume.

**Definition 8.1 [E] (Gray's classes).** An aggregate α on finite multisets is
distributive if α(X ⊎ Y) = α(X) ⊕ α(Y) for some associative ⊕; algebraic if
there is a summary s of bounded size with s(X ⊎ Y) = s(X) ⊕ s(Y) and α = fin ∘
s; holistic otherwise. Reference: Gray et al. 1997.

| Aggregate | Class | Summary s | Notes |
| --- | --- | --- | --- |
| sum, count, max, min | distributive | the value | sum, count invertible (a group); max, min not |
| first, last | distributive over time-ordered concatenation | the value and its time | not commutative; order is the temporal key, so deterministic |
| mean | algebraic | (n, Σx) | |
| std, var | algebraic | (n, mean, M_2) | merge by Chan, Golub and LeVeque 1983, not by Σx^2, for numerical stability |
| cov, corr, ols_beta | algebraic | (n, Σx, Σy, Σxx, Σyy, Σxy) or the Welford pair form | |
| median, quantile | holistic | none bounded | exact: an order-statistics structure over the window |

**Theorem 8.2 [E] (sliding windows).** For any associative ⊕, a sliding
window over a stream supports insert, evict and query in O(1) amortized time
(two-stacks) and O(1) worst-case (DABA); for invertible ⊕ subtraction gives
O(1) worst-case trivially; for holistic aggregates an order-statistics tree
gives O(log w) per operation. The algorithms accept arbitrary insert/evict
sequences, so calendar windows with variable bar counts are covered.
References: Tangwongsan, Hirzel, Schneider and Wu 2015; Tangwongsan, Hirzel
and Schneider 2017, 2019.

**Proposition 8.3 [N] (online evaluability).** An aggregate admits
bounded-state online evaluation over calendar windows iff it is distributive
or algebraic. *Proof.* If: Theorem 8.2 with the summary as the monoid
element. Only if: a holistic aggregate has, by definition, no bounded summary
from which the result is recoverable, so the state must grow with the window.
∎ This is the formal content of the kernel's "decomposable aggregates only"
requirement; median and quantile are admitted at O(log w) state proportional
to the window, which is bounded by Proposition 7.4 and is the cost the cost
model reports.

**Laws the optimizer may use.** Each is an identity of multisets, so it holds
in the semantics over ℝ; the floating-point contract of section 5 restricts
which are applied at the summary level.

1. Resample composition. For r ≺ r' ≺ r'' aligned and α distributive or algebraic: resample_{r''}(resample_{r'}(R, s), ⊕) = resample_{r''}(R, s) at the summary level. For mean this means the inner resample must keep (n, Σx), not the finalized mean: a daily mean of hourly means is not the daily mean.
2. Window fusion. α⟨e⟩{window_δ(t)} over a relation that is itself α'⟨e'⟩{window_{δ'}} fuses only when both are summaries of the same monoid and the outer aggregate is applied to summaries; for finalized values there is no fusion law. A 20-day mean of 5-day means is not a 25-day mean.
3. Partition. Aggregation distributes over disjoint union of groups: the basis of entity parallelism (Proposition 5.3).
4. Reduction idempotence. top_k ∘ top_m = top_{min(k,m)} under one total order; top_k is idempotent.
5. Selection push-down and join reordering as in relational algebra, unchanged.

**Non-laws the LLM will assume.** (1) and (2) at the finalized level; std of
resampled bars equals std of fine bars (false: volatility scales with
resolution, and the type system's lack of a Θ^{-½} exponent in v1 is
deliberate so that the compiler never pretends to know the scaling); corr
over a union of windows from corr over the parts (false; only the six-sum
summary composes). The checker should carry these as warnings when it sees
the pattern, since the semantics are correct and only the author's expectation
is wrong.

## 9. Type system

Three type disciplines are layered: dimension types for quantities, modes for
binding, and clocks for resolution; each is decidable in polynomial time, and
the dimension layer gives a free theorem that is directly meaningful for
strategies.

**Dimension types.** Quantities have types Q⟨d⟩ with d in the abelian group D,
written additively. The typing rules, with Γ the environment of variables and
parameters:

```
(lit)   ────────────────────────        (var)   Γ(x) = τ
        Γ ⊢ n·u : Q⟨dim(u)⟩                       ────────────
                                                   Γ ⊢ x : τ

(add)   Γ ⊢ e₁ : Q⟨d⟩   Γ ⊢ e₂ : Q⟨d⟩              (mul)   Γ ⊢ e₁ : Q⟨d₁⟩   Γ ⊢ e₂ : Q⟨d₂⟩
        ───────────────────────────────────────        ───────────────────────────────────────────
        Γ ⊢ e₁ ± e₂ : Q⟨d⟩                             Γ ⊢ e₁ · e₂ : Q⟨d₁ + d₂⟩

(div)   Γ ⊢ e₁ : Q⟨d₁⟩   Γ ⊢ e₂ : Q⟨d₂⟩              (cmp)   Γ ⊢ e₁ : Q⟨d⟩   Γ ⊢ e₂ : Q⟨d⟩
        ───────────────────────────────────────        ───────────────────────────────────────
        Γ ⊢ e₁ / e₂ : Q⟨d₁ − d₂⟩                       Γ ⊢ e₁ < e₂ : Bool

(log)   Γ ⊢ e : Q⟨0⟩                                 (sqrt)  Γ ⊢ e : Q⟨d⟩   d ∈ 2D
        ──────────────────────────                       ────────────────────────────────
        Γ ⊢ log e : Q⟨0⟩                                    Γ ⊢ sqrt e : Q⟨d/2⟩

(agg)   Γ, Γ_B ⊢ e : Q⟨d⟩    α ∈ {sum, mean, max, min, std, median, quantile, first, last}
        ──────────────────────────────────────────────────────────────────────────
        Γ ⊢ α⟨e⟩{B ; W} : Q⟨d⟩          (count : Count;  corr : Q⟨0⟩;  cov : Q⟨d₁ + d₂⟩;  ols_beta : Q⟨d_y − d_x⟩)
```

Count is not a Q⟨0⟩: 1 / n for n : Count is Q⟨0⟩ by a dedicated rule and n +
e for e : Q⟨0⟩ is rejected, which is what keeps a cardinality from being
mistaken for a weight.

**Theorem 9.1 [E] (soundness).** A well-typed expression evaluates, when it
evaluates, to a quantity of its stated dimension; in particular no addition or
comparison of unequal dimensions occurs at runtime. Reference: Kennedy 1994.
Proof is the usual preservation argument; progress is trivial since the only
stuck states are the partial points of F, which halt by decision.

**Dimension polymorphism and inference.** Library relations such as sma(+A,
@T, +N, −M) are used at Price and at Scalar. Their signatures are quantified
over dimension variables: ∀δ. sma : (Equity, Time, Duration, Q⟨δ⟩) → Q⟨δ⟩ with
let-polymorphism at the library boundary. Type inference reduces to
unification in the free abelian group D, which is decidable and unitary (a
unique most general unifier up to isomorphism), computed by solving linear
Diophantine systems. References: Kennedy 1994 (ESOP), 1996 (PhD thesis), 1997
(POPL). Cost is polynomial in the number of dimension variables; for this
language's signatures it is negligible.

**Theorem 9.2 [E] (dimensional invariance).** For a closed well-typed program
P and any scaling φ of the base units (a group homomorphism D → ℝ_{>0}),
⟦P⟧(φ · E) = φ · ⟦P⟧(E): scaling every input quantity by the factor for its
dimension scales every output quantity by the factor for its dimension.
Reference: Kennedy 1997, as a consequence of relational parametricity. For a
strategy: decisions in shares are invariant under re-quoting prices in cents,
and a threshold like P > 100 type-checks only as 100 USD, which scales with
the prices. There is no way to write a strategy that silently depends on the
unit system. This is the free theorem that the "no Float escape hatch"
decision buys, and it fails the moment one is added.

**Modes.** Each argument of a signature carries + or −; a rule is well-moded
by Definition 4.1. Mode checking with declared modes is a single
left-to-right pass per rule, O(|rule|). Mode inference (choosing modes for
library relations from their uses) is Mercury's problem and is also
decidable, with worst-case exponential behaviour that does not arise for
declared signatures. Reference: Somogyi, Henderson and Conway 1996. Modes are
also Ullman's adornments: sma^{bbbf} is the relation evaluated for bound A, T,
N only, which section 11 uses.

**Clocks.** The resolution judgment Γ ⊢ R : τ @ r has three rules: an atom has
the resolution of its signature; a rule's body atoms outside a resample must
all have the head's resolution; resample takes @r to @r' for r ≺ r' aligned.
This is a monomorphic clock calculus, decidable by a syntactic pass. Clock
polymorphism (a library relation usable at any resolution) is sound in the
same way as dimension polymorphism and should be added when the @1m catalog
ships; Colaço and Pouzet's treatment of clocks as dependent types is the
reference for doing it without losing decidability.

## 10. Decidability and cost

Every judgment the checker makes is decidable in polynomial time in the
program's size; every property that is undecidable in general has been
replaced by a sufficient condition plus a runtime check, and the table says
which is which.

| Property | Decidable? | Checker cost | Mechanism | Status |
| --- | --- | --- | --- | --- |
| WF-1 range restriction, WF-2 modes | yes | O(\|P\|) | one left-to-right pass per rule | [E] |
| WF-3 dimension typing (checking) | yes | O(\|P\|) | exponent arithmetic | [E] Kennedy |
| dimension inference with polymorphism | yes | polynomial | unification in a free abelian group | [E] Kennedy |
| WF-4 temporal recursion | yes | O(\|P\|) | SCCs of the dependency graph, a strict-descent label on every edge inside one | [N] |
| WF-5 closedness | yes | O(\|P\|²) worst case | greatest fixpoint over the dependency graph | [N] Defs. 6.1 to 6.3 |
| WF-6 causality | yes | O(\|P\|) | syntactic provenance of every time variable from the head's | [N] |
| WF-7 total order for reductions | yes (sufficient condition) | O(\|P\|) | the key list covers the identity columns | [N] |
| WF-8 relaxed: temporal stratification | yes | O(\|P\|) | SCCs plus a label check on each − edge | [N] Prop. 3.9 |
| general local stratification | data-dependent; undecidable program-level in general with arithmetic | — | not attempted; Def. 3.8 is the sufficient condition | [E] |
| WF-9 decision mode, at least one decide | yes | O(\|P\|) | syntactic | — |
| decision-conflict freedom | no (query disjointness with arithmetic) | — | runtime denial constraint, O(\|decide\|) per bar | [E] Shmueli 1993; Chandra and Merlin 1977 |
| WF-10 clocks | yes | O(\|P\|) | monomorphic clock calculus | [E] |
| WF-11 keyedness (new) | no in general; yes for the syntactic condition | O(\|P\|) static; O(1) per tuple at runtime | complementary guards; a hash on the key at runtime | [N] Prop. 4.7 |
| finiteness of the model | yes, by construction | — | Prop. 4.4 | [N]; general problem undecidable [E] |
| polynomial evaluation | yes, given WF-11 | — | Prop. 4.7 | [N] |
| static cost estimate | yes | O(\|P\|) given data statistics | per-rule cost from the aggregate classes of section 8 | — |
| program equivalence (lineage) | no | — | syntactic similarity of IR; declared revises | [E] Shmueli 1993 |
| boundedness (recursion depth independent of data) | no in general | — | not needed: depth is \|𝒯\| | [E] Gaifman et al. 1987 |
| partial arithmetic (division by zero) | no statically | — | runtime halt (decided) | [E] |

**Data complexity.** Stratified Datalog with negation over ordered finite
structures captures exactly PTIME. References: Immerman 1986; Vardi 1982.
This language's temporally stratified fragment with keyedness evaluates in
O(|𝒯| · |Ent|^a) up to aggregate costs (Proposition 4.7), which is a strict
subset of PTIME; without keyedness it is still finite but EXPTIME-bounded in
the worst case (Proposition 4.5). Combined complexity (program as input) is
not relevant here: programs are small and the data is large.

**What the checker is.** Sound: a program it accepts has every property
above, by the propositions cited. Incomplete: it rejects or warns on programs
that happen to be fine (a non-syntactically-complementary keyed relation; a
reduction whose order is total only in the data). The incompleteness is the
price of decidability, and every incomplete judgment has a runtime guard so
that the kernel's guarantee is "the property holds for every run that
completes, and a run that would violate it halts with the violating rule
named".

## 11. Compositionality

Libraries compose by union, and the theorems above survive union exactly
when the import graph is acyclic; that condition, checked at link time, is
what justifies the feature cache and the open-loop/closed-loop partition.

**Definition 11.1 (module).** A module is a program over a signature that
names its imports (relations it uses but does not define) and exports
(relations it defines). Well-formedness of a module is checked against its
imports' signatures alone.

**Theorem 11.2 [E] (splitting).** Let P = P_1 ∪ P_2 where no head of P_1
depends on a relation defined in P_2 (a splitting set separates them). Then
M(P, E) = M(P_2, M(P_1, E)): evaluate the bottom part, then the top part over
its result. Reference: Lifschitz and Turner 1994 for stable models; for
stratified programs it is immediate from Theorem 3.5, and Ross 1994 treats the
modular case for Datalog with negation.

**Proposition 11.3 [N] (link-time stratification).** If modules import
acyclically, the union is temporally stratified iff each module is. *Proof.*
A cycle in the union's dependency graph projects to a cycle in the import
graph or lies within one module; the first is excluded, the second is checked
per module. ∎ So the per-module check suffices and linking costs one SCC pass
over the import graph. With cyclic imports the union must be re-checked as a
whole; the design should forbid cyclic imports to keep checks local.

**Corollary 11.4 (feature cache).** By 11.2, a library's derived relations
depend only on the environment and the library's own parameters; their
extensions can be computed once per (library hash, parameter values, bundle
version) and shared by every strategy that imports them. This is the formal
basis of the kernel architecture's feature cache.

**Proposition 11.5 [N] (open-loop prefix).** Let X be the set of relations
reachable (transitively, through any edge) from the executor relations
position, cash, fill, decided. P ∖ rules(X) is a splitting-set lower part of
P, so its model is computable before any decision is made, fully vectorized
over time; rules(X) is the closed-loop part evaluated bar by bar. *Proof.* No
rule outside X depends on a rule inside X by construction of X as a
reachability closure; apply 11.2. ∎ The partition is a taint analysis, O(|P|),
and it is the judgment the kernel architecture called WF-11 before this doc
renumbered keyedness as WF-11; call the partition WF-12.

**Demand-driven evaluation of parametric relations.** A relation with +
arguments (sma with N) has, in principle, infinitely many tuples; it is
evaluated only for the bound inputs that occur. This is the magic-sets
transformation with the modes as adornments: rewrite sma^{bbbf} with a
magic_sma(A, T, N) relation of demanded inputs, derived from the callers, and
restrict evaluation to it. Because every caller binds N to a parameter or a
literal, the demanded set is the finite set of parameter values in the
program. References: Bancilhon, Maier, Sagiv and Ullman 1986; Beeri and
Ramakrishnan 1991. The kernel need not implement magic sets as a rewrite:
because modes are declared, parametric relations can simply be memoised per
bound-input tuple, which is magic sets with the demand computed lazily.

**Why SLD resolution is the wrong evaluation strategy here.** Prolog's
top-down SLD resolution is sound but incomplete for Datalog (left recursion
loops; termination depends on clause order), and it recomputes shared
subgoals. Bottom-up semi-naive evaluation is complete, terminates on finite
models, and is what the fold is. The one thing top-down evaluation offers,
goal-directedness, is recovered by magic sets as above. Reference: Ullman
1989.

**The optimizer as a rewrite system.** Section 8's laws, selection push-down,
join reordering and window fusion are rewrite rules on the IR. Two properties
are wanted: soundness (each rule is a semantic equivalence over ℝ and respects
the floating-point contract) and termination. Confluence is not required,
since the optimizer may pick any equivalent program; a terminating, sound,
non-confluent system is acceptable and much easier to maintain than a
confluent one. Termination is obtained by a strictly decreasing well-founded
measure on the IR (estimated cost from the static cost model, with a tie-break
on term size); a rule that could increase the measure is applied only under a
guard. If confluence is ever wanted (for canonical forms, e.g. to improve
lineage similarity), Newman's lemma reduces it to local confluence, and
Knuth–Bendix critical-pair checking decides local confluence for a
terminating system. References: Newman 1942; Knuth and Bendix 1970; Baader
and Nipkow 1998.

## 12. Proposed changes to the semantic model

Seven changes, ordered by how much they alter what programs are accepted; the
first widens the language, the second narrows it, and the rest are precision.

| # | Change | Replaces | Justification | Effect on accepted programs |
| --- | --- | --- | --- | --- |
| 1 | WF-8 becomes temporal stratification (Def. 3.8): a cycle through not or an aggregate is allowed if every such edge descends strictly in time | WF-8 as written | Prop. 3.9, Thm. 3.7, Cor. 3.10 | Widens: scaling-in rules, hit-rate gates, any aggregate over a relation's own past |
| 2 | WF-11 keyedness: every derived relation in a temporally recursive SCC declares a key and is keyed; syntactic sufficient condition, runtime guard otherwise | nothing | Props. 4.5 and 4.7 | Narrows: rejects programs that are finite but exponential; warns where it cannot decide |
| 3 | WF-6 gains the aggregation clause (already applied): every group time variable is the head's T or bound by a window of it | WF-6 | Def. 3.8(i); feature leakage in the bias audit | Narrows: rejects whole-sample aggregates |
| 4 | "Complete" splits into world-closed and self-closed (Defs. 6.1 to 6.3); negation permitted over closed relations; absence-driven decisions get a provenance flag | WF-5's single flag | Props. 6.4, 6.5; why-not provenance | Same programs; clearer diagnostics |
| 5 | Dimension polymorphism for library signatures with inference by abelian-group unification | monomorphic library signatures | Section 9; Kennedy | Same programs; libraries need not be duplicated per dimension |
| 6 | The core is four forms; top, resample, prior_window are derived forms with their reductions stated | seven primitive forms | Section 2 | Same programs; shorter proofs; smaller IR |
| 7 | WF-12 open-loop partition as a named judgment with a diagnostic | kernel-architecture prose | Prop. 11.5 | Same programs; the LLM sees which rules force bar-by-bar evaluation |

**Two things deliberately not proposed.** A well-founded or stable-model
semantics for non-temporally-stratified negation (section 3: it would admit
undefined atoms); and a user-defined aggregate construct (section 8: an
aggregate's class decides its cost, and only a closed set can be classified
at compile time).

**Reformulations with no semantic effect.** WF-10 restated as a clock
calculus; WF-1 and WF-2 merged as well-modedness (Def. 4.1); the causality
theorem's proof moved from the truncation argument to the fold (Thm. 7.1),
which is what the kernel actually does.

## 13. Conjectures and open problems

Four conjectures, each with what would settle it; none is needed for v1, and
the first two would make the design more defensible if true.

1. [C] **Expressive completeness for causal, bounded-memory queries.** Every query over bar data that is (a) causal in the sense of Theorem 7.1, (b) computable in bounded per-entity memory with cross-sectional reductions at barriers, and (c) in PTIME, is expressible in the temporally stratified, keyed fragment with the given aggregate set. What would settle it: an encoding of a suitable finite-state transducer model into the language, or a counterexample query. The direction "expressible implies (a) to (c)" is Props. 4.7, 7.1 and 7.4; the converse is the conjecture.
2. [C] **Keyedness is the right boundary.** Among programs that are well-moded and temporally stratified, those with polynomial-size models are exactly those that are keyed after a syntactic renaming of value columns into keys where values are bounded-cardinality. What would settle it: a characterization of when a non-keyed temporally recursive relation stays polynomial (it does when the value domain reachable per key is bounded, as for a relation holding at most k distinct flags); the conjecture is that this is the only way.
3. [C] **Lineage by canonical form.** With a confluent optimizer producing canonical IR (section 11), syntactic identity of canonical forms is a decidable under-approximation of program equivalence that captures the LLM's typical rewrites (renaming, reordering literals, inlining a library relation). What would settle it: an empirical rate on the LLM-authored corpus of equivalent-but-not-identical lineages, before and after canonicalization. Equivalence itself stays undecidable (Shmueli 1993), so this is the best available.
4. [C] **Clock polymorphism preserves decidability of the full check.** Adding universally quantified resolution variables to library signatures keeps WF-10 decidable in polynomial time, by the same argument as dimension polymorphism, provided resolution variables range over the finite aligned chain. What would settle it: writing the rules; the chain is finite, so decidability is near-certain and only the interaction with resample's strict-descent requirement needs care.

**Open problem, not a conjecture.** The semantic model fixes the evaluation
order of floating-point reductions for determinism (section 5). Whether the
chosen order (identity columns, entity then time) is also the numerically best
order for the aggregates in section 8 is an open numerical-analysis question;
compensated summation makes it matter less, and the pandas reference tests
make any drift visible.

## 14. References

Established results cited above, by topic. Where a result is used through a
textbook treatment, the textbook is listed.

**Logic programming and Datalog.** van Emden and Kowalski, The semantics of predicate logic as a programming language, JACM 1976. Lloyd, Foundations of Logic Programming, 1987. Apt, Blair and Walker, Towards a theory of declarative knowledge, 1988. Przymusinski, On the declarative semantics of deductive databases and logic programs, 1988. Van Gelder, Ross and Schlipf, The well-founded semantics for general logic programs, JACM 1991. Gelfond and Lifschitz, The stable model semantics for logic programming, 1988; Classical negation in logic programs and disjunctive databases, 1991. Mumick, Pirahesh and Ramakrishnan, The magic of duplicates and aggregates, VLDB 1990. Ullman, Principles of Database and Knowledge-Base Systems, 1988–1989. Bancilhon, Maier, Sagiv and Ullman, Magic sets and other strange ways to implement logic programs, PODS 1986. Beeri and Ramakrishnan, On the power of magic, JLP 1991. Ross, Modular stratification and magic sets for Datalog programs with negation, JACM 1994. Lifschitz and Turner, Splitting a logic program, ICLP 1994. Gupta, Mumick and Subrahmanian, Maintaining views incrementally, SIGMOD 1993.

**Finiteness, safety, decidability, complexity.** Ramakrishnan, Bancilhon and Silberschatz, Safety of recursive Horn clauses with infinite relations, PODS 1987. Kifer, Ramakrishnan and Silberschatz, An axiomatic approach to deciding query safety in deductive databases, PODS 1988. Gaifman, Mairson, Sagiv and Vardi, Undecidable optimization problems for database logic programs, LICS 1987. Shmueli, Equivalence of Datalog queries is undecidable, JLP 1993. Levy, Mumick, Sagiv and Shmueli, Equivalence, query-reachability, and satisfiability in Datalog extensions, PODS 1993. Chandra and Merlin, Optimal implementation of conjunctive queries in relational data bases, STOC 1977. Immerman, Relational queries computable in polynomial time, 1986. Vardi, The complexity of relational query languages, STOC 1982. Dantsin, Eiter, Gottlob and Voronkov, Complexity and expressive power of logic programming, ACM CS 2001.

**Fixpoints and determinism.** Tarski, A lattice-theoretical fixpoint theorem and its applications, 1955. Cousot and Cousot, Automatic synthesis of optimal invariant assertions: mathematical foundations (chaotic iteration), 1977. Kahn, The semantics of a simple language for parallel programming, IFIP 1974.

**Time.** Alvaro, Marczak, Conway, Hellerstein, Maier and Sears, Dedalus: Datalog in Time and Space, UC Berkeley EECS-2009-173 and the 2011 Datalog 2.0 version; Marczak, Alvaro, Conway, Hellerstein and Maier, Confluence analysis for distributed programs: a model-theoretic approach, EECS-2011-120. Caspi, Pilaud, Halbwachs and Plaice, LUSTRE: a declarative language for programming synchronous systems, POPL 1987. Halbwachs, Caspi, Raymond and Pilaud, The synchronous data flow programming language LUSTRE, Proc. IEEE 1991. Benveniste, Caspi, Edwards, Halbwachs, Le Guernic and de Simone, The synchronous languages 12 years later, Proc. IEEE 2003. Colaço and Pouzet, Clocks as first class abstract types, EMSOFT 2003. Wałęga, Cuenca Grau, Kaminski and Kostylev, DatalogMTL: Computational Complexity and Expressive Power, IJCAI 2019; Tractable Fragments of Datalog with Metric Temporal Operators, IJCAI 2020; Stratified Negation in Datalog with Metric Temporal Operators, AAAI 2021. Beck, Dao-Tran and Eiter, LARS: A logic-based framework for analytic reasoning over streams, AIJ 2018. McSherry, Murray, Isaacs and Isard, Differential dataflow, CIDR 2013.

**Aggregates.** Gray, Chaudhuri, Bosworth, Layman, Reichart, Venkatrao, Pellow and Pirahesh, Data cube: a relational aggregation operator generalizing group-by, cross-tab, and sub-totals, 1997. Chan, Golub and LeVeque, Algorithms for computing the sample variance, 1983. Tangwongsan, Hirzel, Schneider and Wu, General incremental sliding-window aggregation, VLDB 2015. Tangwongsan, Hirzel and Schneider, Low-latency sliding-window aggregation in worst-case constant time, DEBS 2017; In-order sliding-window aggregation in worst-case constant time, VLDB Journal 2021; arXiv 1810.11308, arXiv 2009.13768.

**Types.** Kennedy, Dimension types, ESOP 1994. Kennedy, Programming languages and dimensions, PhD thesis, Cambridge UCAM-CL-TR-391, 1996. Kennedy, Relational parametricity and units of measure, POPL 1997. Dembinski and Maluszynski, AND-parallelism with intelligent backtracking for annotated logic programs, 1985. Apt and Pellegrini, On the occur-check-free Prolog programs, TOPLAS 1994. Somogyi, Henderson and Conway, The execution algorithm of Mercury, JLP 1996. Madsen, Yee and Lhoták, From Datalog to Flix, PLDI 2016. Arntzenius and Krishnaswami, Datafun: a functional Datalog, ICFP 2016.

**Provenance and rewriting.** Chapman and Jagadish, Why not?, SIGMOD 2009. Huang, Chen, Doan and Naughton, On the provenance of non-answers to queries over extracted data, VLDB 2008. Newman, On theories with a combinatorial definition of "equivalence", 1942. Knuth and Bendix, Simple word problems in universal algebras, 1970. Baader and Nipkow, Term Rewriting and All That, 1998.
