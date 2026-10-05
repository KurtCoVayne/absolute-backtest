# Data Bundle and Validation Program

Oct 4, 2026 · John Gonzalez

> **Status in this repository.** This document is the second design document
> after [`semantic-model.md`](semantic-model.md); it is implemented in
> milestones, each a set of pull requests against the default branch. The
> status notes in each section say what is in the crate today. Warning codes:
> the repository numbers warnings in the order they were added (W1 dead rule,
> W2 unused parameter, W3 undefined relation, W4 degenerate reduction), so
> this document's "degrees of freedom" warning is **W5** here and the ticker
> snapshot warning is **W6**; the hindsight-literal and asset-list warnings
> have nothing to fire on, because the grammar has no timestamp literal and no
> list literal (`semantic-model.md`, section 8).
>
> | Milestone | Covers | Status |
> | --- | --- | --- |
> | M0 | Section 9 (changes forced on the semantic model), W5, W6, this document | implemented |
> | M1 | Section 5 (execution realism): costs, slippage, impact, participation, margin, funding, borrow | costs and slippage implemented; the rest planned |
> | M2 | Section 3 (catalog): stable identities, `ticker`, actions, delistings, membership, the bundle format and bundle tests | planned |
> | M3 | Section 2 (online construction): the fold kernel, barriers, checkpoints, availability time, the as-of join | planned |
> | M4 | Sections 6 and 7 (research process, study API): lineage, trial log, metrics, hold-out | planned |
> | M5 | Section 8 (realism program): pandas reference, stylized facts, LLM-authored corpus | planned |

Data is closed and system-supplied, the kernel is built online so that
backtesting is replay of the same fold that will run live, and every one of
the forty biases on the list has a named owner (language, bundle, executor, or
study) and a status: guaranteed, data-guaranteed, modeled, measured, warned, or
open. This doc records the decisions of 2026-10-04 and the design they force.

## 1. Decisions

Nine decisions were taken on 2026-10-04; one of them (constraints become
warnings) has a single exception that is argued in section 9.

| Area | Decision | Consequence |
| --- | --- | --- |
| Data supply | Closed. The system ships versioned bundles; users do not supply data in v1 or v2 | Availability, point-in-time membership and corporate actions are tested once, centrally; hold-out and trial counting are enforceable |
| Catalog v1 | US equities @1d from Norgate, in-house data | Unadjusted OHLCV, actions, delistings, index membership, classifications; availability = bar close |
| Catalog target | US equities @1m from Databento; every coarser resolution resampled in-system | Bitemporal keys from the feed (ts_event, ts_recv); resampled @1d reconciled against Norgate @1d in bundle tests |
| Distribution | In-house only; no redistribution, nothing sold | Vendor licensing constrains nothing in the design |
| Construction | Online: the kernel is a fold over an availability-ordered stream; backtest is replay of that fold | Every operator is incremental; the fold state is checkpointable; live is the same program on the feed |
| Constraints | Relaxed to warnings; a warning that closes functionality is removed | Timestamp literals, large asset lists and similar are warned, never refused (section 9) |
| Hold-out | Policy is chosen by the strategy author (the LLM) in the study | The system enforces whatever was declared and logs every reveal; a study with no hold-out is warned |
| Trial accounting | Per study, per strategy lineage | A lineage is the unit for DSR and PBO; renaming a strategy does not reset its count |
| Study | A kernel API inside the checker, not a program kind | The LLM calls it through tools; the checker owns the trial log and the metrics library |
| Reference engine | Pandas reference implementation; RealTest discarded | Differential testing of fills, costs and NAV on the corpus |

## 2. Online construction

> Status: M3. Today's kernel (`src/kernel/`) evaluates top-down with
> memoisation and already runs the executor as a fold over bars; the plan
> keeps it as the batch reference and adds the fold beside it.

The kernel is a single fold `state' = step(state, event)` over a stream of
events ordered by availability time, and a backtest is that fold replayed over
the bundle's log at full speed; nothing in the kernel knows whether it is
reading history or a feed.

**Events.** Two kinds. A tuple arrival: one primitive tuple with its
availability time, in availability order. A barrier: the close of a bar at
some resolution, emitted by the clock, after which no tuple with availability
at or before that instant will be processed as belonging to that bar. A tuple
that arrives after its bar's barrier is not lost; it carries its own
availability time and is available from then on, in backtest and live alike,
because the bundle records when data actually arrived rather than when it was
about.

**State.** Per entity and per derived relation: the incremental form of each
operator. Prefix sums and counts for `sum`, `mean`, `count`; Welford
accumulators for `std`, `cov`, `corr`, `ols_beta`; monotonic deques for `max`,
`min`; the carried value for temporal recursion (WF-4); the open bucket for
resample. Exact `median` and `quantile` keep the window's sorted multiset. The
decomposable-aggregate requirement of the kernel architecture is therefore not
an optimization but a precondition of running at all.

**Barriers and cross-sections.** Per-entity rules advance on every tuple.
Cross-sectional rules (`top`, `count` over entities, nav) and the strategy's
`decide` run only at the barrier of the decision resolution, over the state as
it stands then. Between barriers, entity folds are independent and run in
parallel; at a barrier, reductions merge in the canonical entity order, which
keeps evaluation deterministic under any thread count.

**The executor is in the loop.** At the decision barrier the fold emits
`decide(T, ·)`; the executor (simulated or real) consumes it and produces
`fill`, `position`, `cash` tuples whose availability time is the next bar,
which re-enter the stream as ordinary events. The live/backtest difference is
exactly one component: which executor is attached.

**Checkpoints.** The fold state is serializable. A checkpoint at the end of
each month gives fast partial replays for studies, restart after a crash in
live, and warm-up: the state a live run starts from is the state the replay
reached at the end of the log, so a live strategy begins with its windows
full.

**Determinism and the equivalence theorem.** The stream has a total order
(availability time, then a canonical tie-break). Given the same log and the
same executor configuration, two runs produce identical decisions. The
causality theorem of the semantic model becomes operational: decisions
computed live on the feed equal decisions computed by replaying the log that
recorded that feed. That is testable by running the two side by side in
shadow mode and diffing `decide`.

**Storage.** The bundle is an append-only log partitioned by availability date
(Arrow IPC for the active tail, Parquet for history) plus monthly state
snapshots. A historical revision is an append with a later availability time,
never an overwrite, so any past as-of view is reconstructible.

The live feed is written into the same log the backtest replays, so the only
component that differs between the two is the executor attached to the fold.

## 3. Catalog

> Status: M2. Today an `Equity` is the ticker string the CSV names, interned
> unchecked; the only action-like primitive is `dividend_announced` in
> `equities_1d_ext`.

The catalog stores what was observable, never what was derived with
hindsight: unadjusted prices plus corporate-action events, stable security
identities plus a ticker history, and membership as of each date; every
adjusted or current-looking series is derived in the library, causally.

**Identity.** `Equity` is a system-assigned security identifier that survives
ticker changes, exchange moves and reuse of a ticker by a later company. The
catalog maps Norgate's symbol history and Databento's `instrument_id` with its
symbology over time onto it, and exposes `ticker(A, @T, S)` as an ordinary
time-keyed relation. A strategy that names `AAPL` names the security that
carried that ticker at the strategy's bundle date, and the checker warns that
the name is a snapshot (W6).

**Prices and actions.** OHLCV is stored as traded. Splits, dividends,
spin-offs and cash mergers are events with announce, ex and pay dates where
the vendor supplies them. The library derives total return `ret(A, T, R)` by
applying the split factor and the cash dividend at the ex-date, and derives a
point-in-time adjusted close `close_adj(A, T, P)` that uses only events with
ex-date at or before T. A series adjusted as of today is never in the catalog,
because its level at T encodes splits that had not happened yet.

**Membership and status.** `universe(A, @T)` is the set of securities that
were listed and tradable on T; `member(A, @T, Idx)` is point-in-time index
membership from Norgate's constituent history; `delisted(A, @T, Reason)`
carries the delisting date and reason; `classification(A, @T, Scheme, Code)`
is time-varying. The trading calendar is implied by the time domain.

| Tier | Source | Resolution | Contents | Availability |
| --- | --- | --- | --- | --- |
| v1 | Norgate (in-house) | @1d | OHLCV as traded, actions, delistings, index membership, classification | bar close |
| target | Databento | @1m | OHLCV-1m per security; TBBO (top of book at each trade) for spread and fill modeling; symbology | ts_recv at the gateway plus the system's own processing delay, recorded per tuple |
| derived | in-system resample | @5m to @1d | every coarser bar from the @1m log | bucket close |
| v2 | vendor to be chosen | @1d | fundamentals with publication dates, earnings dates, rates and index series | publication time |

Two availability conventions must agree. Norgate @1d bars are available at the
bar's close; a @1d bar resampled from the Databento @1m log is available at
the bucket close, which is the same instant. Where both exist the bundle keeps
both relations under distinct names (`close_d` from Norgate, `close_d_rs`
resampled) and the bundle tests reconcile them; a strategy uses one.

Bundle tests, run on every bundle version before it is published to the
catalog:

- Availability monotone and never earlier than event time, per tuple.
- Point-in-time membership: no `member` tuple is available before the index provider's announcement; constituents include every delisted name that was a member.
- Action reconciliation: every price gap at an ex-date is explained by an action within tolerance; every action has a price gap.
- Delisting coverage: every security that stops trading has a `delisted` tuple with a reason.
- Resample reconciliation: `close_d_rs` against `close_d`, `volume_d_rs` against `volume_d`, with consolidated-versus-venue volume differences flagged rather than hidden.
- Identity: no two securities share an identifier; every ticker reuse is split across identifiers.

A bundle that fails a test is not published, and the catalog shows each
relation's test coverage so the LLM can read what is vouched for.

## 4. Bias audit A: information and time

> Status: the language rows hold today (WF-6 including the aggregation-group
> clause, no forward reference, no unbounded aggregate); the data rows wait
> for M2's catalog; delisting handling is M2; selection and regime measures
> are M4.

Eleven of the forty biases are ways for information from after T to reach a
decision at T; eight are closed by construction (language or bundle) and three
are properties of the author's choices that the study measures and warns on
rather than forbids.

Status vocabulary used in sections 4 to 6: *guaranteed* (cannot occur, by the
language or kernel); *data-guaranteed* (cannot occur given a bundle that
passed its tests); *modeled* (the executor simulates it with configurable
parameters and conservative defaults); *measured* (the study computes a
diagnostic and the author decides); *warned* (the checker or study flags it);
*open* (not handled in v1 or v2, with a home named).

| Bias | Where it enters | Handling | Status |
| --- | --- | --- | --- |
| Look-ahead | A rule reads a tuple keyed later than its own T; data keyed by event time rather than by when it was knowable | WF-6 on temporal keys, transitively; bundle keys every tuple by availability (@1d: bar close; @1m: receipt time); aggregation groups must be windowed to the head's T (section 9) | Guaranteed (program); data-guaranteed once availability keys ship |
| Point-in-time | Membership, fundamentals or classifications read as they are today | Every catalog relation is time-keyed by availability; `universe`, `member`, `classification` have no T-less form | Data-guaranteed |
| Historical revision | Restated fundamentals, corrected prices, re-adjusted series silently replace the past | Append-only log: a revision is a new tuple with a later availability time; a decision at T sees the version available at T | Data-guaranteed |
| Corporate-action | Back-adjusted prices encode future splits; dividends never credited; splits not applied to positions | Catalog stores prices as traded plus action events; `ret` and `close_adj` apply only events with ex-date at or before T; executor adjusts positions on the ex-date and credits dividends on the pay date | Guaranteed by design, verified by the reference engine |
| Survivorship | Dataset holds only names that still exist; ticker reuse hides the dead | Bundle includes delisted securities; `Equity` is a stable identifier, `ticker` is a relation; bundle test on identity | Data-guaranteed |
| Universe leakage | Selecting on today's membership, or filtering on information not available at T | `universe(A, T)` and `member` are point-in-time; liquidity filters are windowed; no un-keyed universe exists | Guaranteed |
| Feature leakage | Normalizing, scaling or fitting over the whole sample | No unbounded aggregate exists; every time variable inside an aggregation group is bound by a causal window of the head's T (WF-6 clause, section 9) | Guaranteed |
| Label leakage | Forward returns used as features; a model trained on data after T | Forward references do not exist; v1 and v2 have no fit construct; a future one must emit availability-keyed relations fitted on windows at or before T | Guaranteed; not applicable until v3 |
| Delisting | A position in a delisted name disappears, or is valued at its last print as if sold | `delisted(A, T, Reason)` in the catalog; executor force-closes at the last trade with a haircut by reason, default conservative for involuntary delistings | Modeled; warned when the haircut is set to zero |
| Selection | Universe, period or names chosen with knowledge of the outcome | Universe and period are logged study inputs; hand-listed assets and timestamp literals are warned as hindsight constants; coverage below a threshold of names or years is warned | Warned, measured |
| Regime | Results earned in one market state | Study reports per-year and per-regime breakdowns (volatility tercile, drawdown state); warns when the sample covers fewer than two regimes of each kind | Measured, warned |

Three points that deserve more than a cell.

**Adjusted prices are a look-ahead.** An adjusted close as of today divides
every past price by the product of all split factors since, including splits
that had not been announced at T. Return ratios between two past dates are
unaffected, but price levels are: a rule such as `P > 5 USD`, a share count
`size / P`, or a notional computed from `P * Q` is wrong by the future
adjustment. The catalog therefore holds prices as traded, and `close_adj` is a
library relation whose adjustment at T uses only ex-dates at or before T. This
is the single most common look-ahead in otherwise careful backtests and it is
closed here by data design, not by the checker.

**Availability at @1m is a measurement, not a convention.** Databento stamps
each record with the exchange's event time and the time it was received at
the gateway. The bundle keys availability by receipt time plus the system's
own recorded processing delay, so a bar that arrived late is late in the
backtest too. This is what makes the equivalence theorem of section 2 hold
against a real feed rather than an idealized one.

**Identity is the survivorship fix.** A ticker-keyed dataset cannot represent
a security that changed ticker, two securities that shared one, or a name that
delisted and whose ticker was reissued. Stable identifiers with a ticker
history cost one mapping table per vendor and remove a whole class of silent
errors; the checker's warning on a literal ticker exists because the LLM will
write `AAPL`, and the bundle date fixes what that means.

## 5. Bias audit B: execution realism

> Status: M1 (costs, slippage, impact, liquidity, partial fills, margin,
> funding, borrow proxy) and M2 (delisting, actions). Implemented: the cost
> model (per-share commission with a per-order minimum, the regulatory fee on
> sells, volatility-scaled slippage, non-zero defaults, a cost summary per
> run, and a warning naming the bias when a model is turned off) and the
> ruin, leverage and oversize policies of PR #48 (halt by default). The
> leverage default stays at 1x gross by the owner's ruling; Reg T (50 %
> initial, 25 % maintenance) is a configuration preset, not the default.

Seventeen biases live in the gap between a decision and a fill; the executor
models fourteen with conservative defaults and warns when a study turns a
model off, two are closed by the execution contract itself, and one (borrow
availability) is a proxy until real locate data exists.

| Bias | Where it enters | Handling | Status |
| --- | --- | --- | --- |
| Transaction-cost neglect | Zero commissions and fees | Executor charges per-share commission, per-order minimum and regulatory fees; defaults non-zero; a study run at zero cost is warned | Modeled, warned |
| Slippage | Fill at the price that generated the signal | Fill at the next bar at a modeled price: @1d close plus volatility-scaled slippage; @1m next-bar VWAP, or crossing the TBBO spread when quotes exist | Modeled |
| Market-impact | Large orders filled at the quoted price | Square-root impact in participation (order / ADV) with a configurable coefficient; orders above a participation threshold are warned | Modeled, warned |
| Liquidity | Trading names that could not absorb the order | Per-bar participation cap from bar volume; the remainder is unfilled; `liquid` library filter; capacity reported by the study | Modeled, measured |
| Bid-ask spread | @1d data carries no quotes | v1: spread proxy from an ADV bucket or a high-low estimator; target: TBBO gives the actual spread at each trade and fills cross it | Modeled (v1 proxy); data (target) |
| Execution timing | Fill at the bar that produced the signal | Decisions are emitted after bar close and filled at the next bar; no same-bar fill exists in the kernel | Guaranteed |
| Bar-resolution | Intrabar path unknown; a stop assumed filled at its level | No intrabar order exists in the language: a stop is a bar-close decision and is simulated as one; @1m shrinks the gap; a rule shaped like an intrabar stop is warned so the author knows what it is | Guaranteed honest; warned |
| Partial-fill | Every order fully filled | Fill is at most the participation cap times bar volume; `fill` records the actual quantity; a delta order expires at bar end, a target re-issues itself | Modeled |
| Latency | Decision and fill at the same instant | Availability is receipt time; the decision at a barrier sees only what had arrived; fill at the next bar plus a configurable latency offset | Modeled; conservative by contract |
| Rebalancing | Free, instantaneous, perfectly timed rebalances | A rebalance is a calendar decision filled next bar with costs; turnover and cost sensitivity reported | Modeled, measured |
| Cash-management | Cash drag ignored, or cash earning nothing forever | Cash accrues a configurable rate; v2 catalog supplies a T-bill series; v1 default is zero and warned | Modeled (v2); warned (v1) |
| Leverage | Targets or deltas exceed capital without anyone noticing | Executor enforces a configurable buying power (default: no borrowing, halt; preset: Reg T, 50 % initial, 25 % maintenance, reject and log); gross and net leverage reported | Guaranteed |
| Funding-cost neglect | Margin interest and short rebate ignored | Executor charges margin interest and credits a short rebate from a rate series plus spread; v1 uses a constant rate and warns | Modeled, warned |
| Shorting | Any name shortable at no cost | Borrow fee by ADV bucket as a v1 proxy; locate rules out of scope; warned that fees are modeled, not observed | Modeled (proxy), warned |
| Borrow-availability | Hard-to-borrow names shorted freely | v1: the smallest ADV bucket is not shortable; real locate and fee data are not in the catalog and are the first candidate for v3 user data at reduced trust | Proxy; open |
| Currency | Mixing currencies, or ignoring conversion | Dimensional types carry the currency code; there is no implicit conversion; the v1 catalog is USD only | Guaranteed by types |
| Portfolio-construction | Weights or covariances estimated with future data; infeasible weights | Only causal windows exist; the v2 matrix aggregates are causal by construction; the executor enforces feasibility against buying power and lot sizes | Guaranteed (causality); modeled (feasibility) |

**Defaults are conservative on purpose.** Every model above has a default
that costs the strategy something: non-zero commissions, non-zero slippage,
impact above a participation threshold, a delisting haircut, zero interest on
cash in v1. An author who wants a frictionless run can set every model to zero
and will get a study report that says so on every page. The failure mode this
design accepts is a strategy that looks slightly worse than reality; the
failure mode it refuses is one that looks better.

**What the @1m target changes.** Spread, slippage and latency stop being
proxies: TBBO carries the actual top of book at each trade, so a fill can
cross the spread that existed, and the receipt timestamp gives the delay that
existed. The executor's @1m fill model is therefore the one that gets
validated against real executions first, and the @1d proxies are calibrated
from it.

## 6. Bias audit C: research process

> Status: M4, except the degrees-of-freedom count, which the checker computes
> today (`Program.degrees_of_freedom`, printed by `abt check`; W5 on each
> in-rule literal of a strategy).

Twelve biases are about how a strategy was found rather than how it runs;
none can be forbidden, all can be counted, and the closed bundle is what makes
the counting honest.

| Bias | Where it enters | Handling | Status |
| --- | --- | --- | --- |
| Data-snooping | Many strategies tried, the best one reported | Every run is a logged trial against its lineage: program hash, parameters, period, universe, executor configuration; DSR and PBO use the real count | Measured; cannot be hidden |
| Multiple-testing | A parameter grid with no correction | DSR with the grid as the trial set and the variance of trial Sharpes; PBO via CSCV over the grid | Measured |
| P-hacking | Metric, period or universe changed until significant | Each change to any study input is a trial in the lineage; the metric is a logged input, not a free choice after the fact | Measured |
| False statistical significance | Point estimates without error bars | Sharpe with Lo (2002) standard errors, block-bootstrap intervals, minimum track-record length (MinTRL) alongside | Measured |
| Overfitting | Too many free parameters and constants for the sample | The IR reports degrees of freedom: parameters, numeric literals in rules, rule count; PBO; neighbour stability | Measured, warned |
| Parameter over-optimization | The best grid point reported as the strategy | Parameter-surface smoothness, neighbour stability within a tolerance, plateau detection; the report shows the surface, not the peak | Measured |
| Out-of-sample | Repeated peeks at the hold-out | Hold-out is declared by the author; once declared it is embargoed until a committed version is named; each reveal is logged and counted as an out-of-sample trial | Measured; enforced once declared; warned when absent |
| Walk-forward | Scheme chosen after seeing results; fitted windows overlap | The scheme (anchored or rolling, window lengths) is a logged input; walk-forward efficiency reported; changing the scheme is a trial | Measured |
| Autocorrelation | Sharpe errors and t-statistics assume independence | Lo-adjusted standard errors, Newey-West t-statistics, block bootstrap | Measured |
| Non-stationarity | Parameters that drift across the sample | Rolling sub-period stability and regime breakdown; instability beyond a threshold warned | Measured, warned |
| Short-sample | Too few observations for the claimed confidence | MinTRL for the target confidence compared with sample length; warned below it | Measured, warned |
| Tail-risk neglect | Sharpe alone | Drawdown depth and duration, CVaR, skew and kurtosis (which also enter DSR), worst month; a study reporting Sharpe without tail metrics is warned | Measured, warned |

**Why the closed bundle matters here.** Trial accounting is only as good as
the system's knowledge of what was tried. With user data, a researcher can run
a hundred variations privately and submit the survivor; the count is one. With
closed data and a kernel API as the only way to run, the count is a hundred,
because there is no other way to have run them. The same holds for hold-out:
a declared embargo can be enforced only when the author cannot read the data
by other means.

**Lineage.** A strategy revision belongs to the lineage it declares with
`revises <hash>`. Because renaming is the obvious way to reset a count, the
checker also compares a new strategy's IR against existing lineages and
attaches it to the nearest one when the similarity is above a threshold, with
a warning that says so. The author can dispute the attachment; the dispute is
logged.

**Degrees of freedom are known exactly.** A human researcher estimates how
many knobs a strategy has; the IR counts them: declared parameters, numeric
literals inside rules, and rule count, each a separate line in the report. The
LLM will tune literals inside rules when parameters are warned; counting
literals as degrees of freedom is what keeps that honest.

**References for the library.** Deflated Sharpe ratio and minimum
track-record length: Bailey and López de Prado (2014, 2012). Probability of
backtest overfitting via combinatorially symmetric cross-validation: Bailey,
Borwein, López de Prado and Zhu (2014). Sharpe-ratio standard errors under
autocorrelation: Lo (2002).

## 7. Study API

> Status: M4.

The study API is the only way to run a strategy, it lives inside the checker
so that every run passes validation first, and it owns three things the author
cannot touch: the trial log, the hold-out embargo, and the metrics library.

| Call | Inputs | Returns | Logged as |
| --- | --- | --- | --- |
| `lineage.open` | checked strategy IR, bundle version, optional `revises` | lineage id, similarity attachment if any | lineage record |
| `study.declare` | lineage, hold-out policy (trailing years, random blocks, none), period, universe, executor configuration, metric set | study id, warnings (no hold-out, zero costs, narrow sample) | study inputs |
| `study.run` | study, parameter point or grid, walk-forward scheme (optional) | run ids, per-run relations on request | one trial per parameter point; one per scheme change |
| `study.metrics` | run or lineage | metrics with standard errors; DSR over the lineage's trial count; PBO over the grid; stability surface; degrees of freedom | read; not a trial |
| `study.explain` | run, `decide(T, D)` | derivation tree to primitive tuples | read |
| `holdout.reveal` | lineage, committed IR hash | out-of-sample metrics for that version | one out-of-sample trial |
| `study.report` | lineage | every warning raised, the trial count, the reveals, the bias statuses that apply | read |

**Metrics library.** Return-based: CAGR, volatility, Sharpe with Lo standard
error, Sortino, maximum drawdown and its duration, CVaR at 5 %, skew,
kurtosis, worst month. Robustness: DSR, MinTRL, PBO, walk-forward efficiency,
parameter-surface smoothness, neighbour stability. Trading: turnover, average
participation, capacity estimate at a cost threshold, fill ratio, gross and
net exposure, leverage. Every metric is a relation over `nav`, `fill`,
`position` and `decide`, written in the DSL's library where the language can
express it, so it is checked like everything else, and in the kernel where it
cannot (CSCV, bootstrap).

**Warnings policy.** The checker and the study emit warnings, never refusals,
for: timestamp literals in rules; asset lists above a size; a study with no
hold-out; zero-cost or zero-slippage executor configuration; participation
above a threshold; a rule shaped like an intrabar stop; coverage below a
threshold of names or years; degrees of freedom high relative to sample
length; instability across sub-periods; a lineage attachment by similarity. A
warning carries the bias it relates to, so the report reads as a bias
checklist with the author's answers.

**What the system does not judge.** Whether a Sharpe of 1.2 is good, whether
DSR above 0.95 is the right bar, whether a momentum strategy should survive
2009: those are the author's constraints, declared in `study.declare` as
thresholds the report checks. The system's commitments end at computing them
honestly.

## 8. Realism program

> Status: M5.

Realism is the provider's claim and is proven three ways: the kernel agrees
with an independent slow implementation on the corpus, it reproduces the
stylized facts of canonical strategies on the point-in-time universe, and
strategies written by an LLM from plain briefs pass through the whole pipeline
and come out looking like what they are.

**Pandas reference.** A second implementation of the executor and the feature
library in pandas, written from the semantic model and not from the kernel's
code, deliberately slow, with no shared modules. Differential tests run every
corpus strategy on the same bundle through both and require: fills identical
in quantity and price; position and cash identical; nav within floating
tolerance. A disagreement is a bug in one of them and the semantic model is
the referee. The reference also covers corporate actions explicitly: a split,
a dividend, a spin-off and a delisting each have a tiny synthetic bundle with
a hand-computed expected nav.

**Stylized-fact tests.** Canonical strategies on the point-in-time US universe
with the default cost model: 12-1 momentum deciles, low-volatility deciles,
short-term reversal, size. Each has published ranges for its long-short
premium, its volatility and its known failures (momentum's 2009 crash,
low-vol's 2020 drawdown). The tolerances are wide; the purpose is to catch
gross execution or data errors (a missing delisting return, dividends never
credited, a survivorship-tainted universe), not to validate the strategies.

**LLM-authored corpus.** Briefs in plain language ("a cross-sectional momentum
strategy on liquid US equities, monthly rebalance, vol-targeted") are given to
the LLM, which writes the strategy and the study. Measured: checker rejection
rate and the distribution of diagnostics on first attempt; attempts to a valid
program; study verdicts; whether the resulting strategies' metrics fall inside
the stylized-fact ranges. A rising first-attempt pass rate and stable verdicts
across LLM versions are the evidence that the language, the catalog and the
warnings are doing their job. The negative result matters too: a brief whose
honest study verdict is "does not survive costs" is the system working.

**Bundle-level realism.** Resampled @1d from the Databento log reconciled
against Norgate @1d; TBBO spreads compared against the @1d spread proxy to
calibrate it; later, the executor's @1m fill model compared against actual
executions imported from the broker, which is the only ground truth for
slippage and impact.

## 9. Changes forced on the semantic model

> Status: M0, implemented. Item 1 was already enforced by the checker (code
> F; the corpus case `bad_full_sample_aggregate` pins it). Item 2 is the W6
> warning today and the stable identifier in M2. Item 3 is W5 and the
> degrees-of-freedom count; the two warnings on constructs the grammar lacks
> are not applicable. Item 4 needs no code.

Four changes, three of them small; the fourth is the one exception to
"constraints become warnings", because it is not a constraint on top of the
closed-data decision but the decision itself.

1. **WF-6 gains a clause on aggregation groups.** Inside `agg(e) over (conj)`, every temporal-key variable of `conj` must be either the head's T or bound by `window`, `prior_window` or bucket of T. Without this, `M = mean(P1) over (close(A, T1, P1))` is a full-sample mean and feature leakage is one unconstrained variable away. The corpus already obeys it; the checker enforces it as F.
2. **`Equity` is a stable security identifier.** Section 2 of the model said "opaque identifiers"; it now says identifiers assigned by the bundle that survive ticker changes, with `ticker(A, @T, S)` a catalog relation. A ticker literal in a strategy resolves against the bundle version named in `env` and is warned as a snapshot (W6).
3. **Warnings replace the parameter-channel constraints.** Numeric literals inside a strategy's rules are counted as degrees of freedom and each one warns (W5; the structural constants 0 and 1 excepted); timestamp literals and asset lists do not exist in the grammar, so their warnings are not applicable. None refuses a program.
4. **Relation-valued parameters stay out of the language.** A parameter holding a table of (Timestamp, value) pairs is a data upload with a different name; admitting it would reopen every bias in section 4 through the side door and make the trial accounting of section 6 unenforceable, since the table can encode the results of runs made elsewhere. This does not close functionality the closed-data decision had not already closed: anything such a table could carry belongs in the catalog (v2) or in v3 user relations at reduced trust, where its decisions are marked as uncertified. If you want this relaxed anyway, the honest form is to make it exactly that: a v3 user relation, not a parameter.

The model's section 6 (environment) also inherits the catalog's distinct names
per source and resolution, and its section 1 table already carries the
closed-data row.

## 10. Open questions

Six decisions remain, ordered by how early they are needed. The defaults the
implementation plan takes are in brackets; each is configuration and recorded
in the bundle manifest or the study, so changing it is not a code change.

1. **Relation-valued parameters.** Keep them out (section 9, item 4), or admit them as v3 user relations at reduced trust? The design assumes out. [Out.]
2. **Delisting defaults.** The haircut by reason (bankruptcy, acquisition, voluntary, regulatory) for names without a recorded delisting return. A conservative default is proposed; the numbers need the team's judgment. [Bankruptcy and regulatory 100 %, acquisition and voluntary 0 %; a zero haircut on an involuntary reason warns.]
3. **Databento schema.** Consolidated (EQUS-style) or single-venue @1m bars? Consolidated volume is what ADV filters and participation caps assume; a single venue understates it and the resample reconciliation against Norgate will show the gap. [Consolidated.]
4. **@1d availability offset.** Norgate's daily update lands after the close; the convention "available at bar close" assumes decisions are made in the evening. If the team runs decisions the next morning, the offset should be recorded as the bundle's processing delay rather than assumed away. [Zero; the manifest's `processing_delay` records any offset.]
5. **Lineage similarity threshold.** How close must a new IR be to an existing lineage to be attached by the checker, and does the author's dispute override it or only annotate it? [0.8 Jaccard over normalised rules; a dispute annotates.]
6. **Hold-out reveal budget.** The policy is the author's, but should the system cap reveals per lineage at all, or only count them? The design only counts. [Counted, not capped.]

**Sources.** Bailey and López de Prado, *The Deflated Sharpe Ratio* (2014)
and *The Sharpe Ratio Efficient Frontier* (2012); Bailey, Borwein, López de
Prado and Zhu, *The Probability of Backtest Overfitting* (2014); Lo, *The
Statistics of Sharpe Ratios* (2002). The semantic model doc holds the
language-side definitions this doc refers to.
