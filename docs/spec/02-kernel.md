# 02 · The kernel (`abt-kernel`): verifier, evaluator, market simulation

Oct 12, 2026 · build sheet for the second program. Tags and cross-references as in `00-architecture.md`. The language is the second formulation (V-3 to V-10) with the extensions of section 2; the compiler pipeline is `docs/compiler-design.md` (C-2 to C-7) with the deltas of section 8. This document does not restate what those fix; it states what the split changes and what options and the three modes add.

---

## 1. Scope

The kernel takes a program, a parameter point and a mode; it checks the program, derives its contract, resolves its demand through the data layer, consumes the data layer's event stream, evaluates the rules at every barrier, decides, simulates execution (or forwards decisions to a broker adapter in `live-broker`), maintains the book, emits decision and execution events, serves its state tables and query language, and writes its outputs to the data layer under the run id. It computes no statistic (A-D13), counts no trial (A-D17), and reads no vendor (A-D21).

---

## 2. The program form

### 2.1 Blocks

A strategy is the nine blocks of V-4 with these changes:

| Block | Change in this specification |
| --- | --- |
| `data` | `need` gains the dynamic form `need chain of UNDERLYING within EXPIRY_RANGE and MONEYNESS_RANGE [right R] [spot RELATION]` (D-7.2), and a `budget { money X; bytes Y; requests Z }` clause (A-D22); `require tested` becomes `require reviewed` (a version attribute, D-2.1) since bundle tests no longer exist as such. |
| `calendars` | unchanged (V-3.1); `sessions` is read from the data layer's `session` relation (D-2.4). |
| `clock` | unchanged. |
| `universe` | unchanged; for options the universe is typically `universe(O, T) :- chain(U, T, O), ...` over the kernel relation `chain` (2.4). |
| `params` | unchanged. |
| `rules` | the function library gains the functions of 2.5; the kernel relations gain those of 2.4. |
| `decisions` | the algebra gains `combo` and `exercise` (2.3). |
| `execution` | gains the fields of 2.2 for marks, option fills, margin, exercise and late decisions. |
| `report` | unchanged; it stays outside the hash (V-4.5). |

A library is unchanged (V-7). The class catalogs the checker reads are generated from the data layer's registry (`abt data catalog --class C`, D-2.2), never written by hand.

### 2.2 The `execution` block

Every field is required unless marked optional; a preset (V-4.3) may supply any of them.

```
execution {
  price         RELATION[@CALENDAR]         # the relation the book is marked at and market orders fill at (equities, futures)
  option_mark   mid | last | model          # options: mid of `quote` (bid, ask), `quote.last`, or the model of 2.5 at the mark's IV
  option_fill   touch [+ FRACTION spread] | mid [+ FRACTION spread] | last   # the fill price of an option order against `quote`
  greeks        data | model                # `greeks` served from the binding when present, else computed (2.4)
  orders        market | moo | moc | moo_moc
  lot           whole | fractional
  capital       AMOUNT fixed | compounding
  commission    per_share P min M | per_contract P | per_contract from catalog | bps X | none
  slippage      fixed X bps | vol X over N bars | none      # equities and futures only; options use option_fill
  impact        sqrt X over N bars adv | none
  participation X of bar volume | none
  financing     cash X debit Y short Z | none
  margin        published | reg_t | portfolio | span | approx FRACTION   # 5.6; several may be listed in order of preference
  leverage      gross <= X halt|reject | reg_t | unconstrained
  actions       apply | in_price
  delisting     last_price | haircut { ... }
  settlement    daily | none                # futures: daily variation margin to cash (5.4)
  exercise      { itm_at_expiry; early_short when extrinsic < DIVIDEND|THRESHOLD; early_long never|decide; atm_tolerance X }   # 5.5
  oversize      halt | clamp | allow
  ruin          halt | continue
  late          send | reject               # a decision evaluated after its barrier's deadline (6.3)
  numerics      exact | two_pass | running      # exact: tick columns as integers (ST-3, ST-10.4); the default for new programs, two_pass pinned for MW14 and R8L
  limits        { gross <= X; net <= X; weight <= X per name; loss <= X per day; drawdown <= X; turnover <= X per PERIOD; participation <= X; names <= N; each with halt|reject|clamp|warn }   # optional; 3.5
}
```

**Proposition 2.1 [N].** With this block, the kernel's whole configuration is in the program hash except `report`, the parameter point and the deployment document. *Proof.* Every field of the present `ExecConfig` (`src/kernel/mod.rs` 589–715) maps to a field above or to the parameter point; the deployment document is excluded by A-D28; `report` by V-4.5. ∎

### 2.3 The decision algebra

```
hold(A, W)                     W : Scalar                   a target weight of capital
hold(A, Q)                     Q : Quantity<unit>           a target quantity (signed)
trade(A, Q)                    Q : Quantity<unit>           a delta (signed)
combo([leg(A1, Q1), ..., leg(An, Qn)])   n ≥ 2             an all-or-none multi-leg delta
exercise(A, Q)                 A : Option, Q : Quantity<contracts>   exercise Q long contracts now (a decision; 5.5.4)
```

each with an optional trailing order (`market`, `moo`, `moc`, `moo_moc`, `limit(P[, TIF])`, `stop(P[, TIF])`; for a combo the limit is the net price). The denial constraint is one decision per instrument per bar (F-5); a combo counts as a decision on each of its legs. `exercise` is admitted only when `A`'s class is `option`; the checker rejects it otherwise (T).

### 2.4 Kernel relations

At the decision calendar unless noted; every one is a stored relation of Σ_t (7.1):

| Relation | Meaning |
| --- | --- |
| `position(A, @T, Q)`, `cash(@T, C)`, `nav(@T, N)`, `capital(@T, C)`, `mark(A, @T, P)`, `fill(A, @T, Q, P)`, `decided(@T0, D)`, `decide(@T, D)` | as V (corpus-v2 README, kernel relations) |
| `margin_req(@T, Initial, Maintenance)` | the book's margin requirement at the bar's mark under the selected model (5.6) |
| `exposure(@T, Gross, Net, Leverage)` | the book's exposure at the mark |
| `greeks(A, @T, IV, Delta, Gamma, Theta, Vega, Rho)` | from the binding's `greeks` relation when present and `greeks data`; else computed from the mark and the model at the bar (2.5); the program reads one relation either way |
| `chain(U, @T, A)` | the contracts of `U`'s chain at `T` admitted by the program's dynamic need (D-4.4); enumerable |
| `spec(A, @T, Underlying, Right, Strike, Expiry, Style, Settlement, Multiplier)` | the option's contract terms as of `T` (adjusted by corporate actions, 5.5.5) |
| `contract(A, @T, Root, Expiry, PointValue)` | the future contract the root's continuous series holds at `T` (D-2.4 `continuous`) |
| `session_of(A, T, S)`, `expiry_of(A, E)`, `underlying_of(A, U)` | builtins |

### 2.5 Functions added to F

Total functions, each with a documented algorithm and a known-answer test (9.3):

| Function | Model |
| --- | --- |
| `bs_price(S, K, τ, r, q, σ, right)`, `bs_delta`, `bs_gamma`, `bs_vega`, `bs_theta`, `bs_rho` | Black–Scholes–Merton with continuous dividend yield `q` (equities, European) |
| `b76_price(F, K, τ, r, σ, right)` and Greeks | Black 1976 (options on futures) |
| `am_price(S, K, τ, r, q, σ, right)` | Bjerksund–Stensland 2002 for American options; the early-exercise premium is `am_price − bs_price` |
| `implied_vol(P, S, K, τ, r, q, right, style)` | Jäckel's rational initial guess with Newton–Halley to 1e-12 on price; `⊥` outside no-arbitrage bounds (a stuck instance, V-5.1) |
| `sign`, `floor`, `ceil`, `clamp`, `decile` | V-7.3 |

`τ` is a Duration converted to years by the day-count the execution block's `price` calendar implies (ACT/365 fixed; a [D] with the alternative ACT/ACT rejected as adding nothing at the precision the data has). `r` is the `financing` block's cash rate; `q` is the dividend yield relation when the program binds one, else 0.

---

## 3. Contracts derived from structure

**Definition 3.1 (contract).** The contract of a checked program `P` with analysis `𝒜(P)` (C-3) is the quadruple (pre, inv, post, lim) computed by a total function `contract(P, 𝒜(P))`. Each clause has a *source* (the block, rule or analysis fact it follows from), a *discharge* (`static` with the tier that proved it, `bind` for what the resolver's answer settles, `runtime` with the barrier at which it is checked, `report` for what is computed after the run), and a *policy* (what happens on violation). Nothing in a contract is written by the author (A-D14); `abt contract` prints it (3.7).

### 3.2 Preconditions

Checked before the first barrier; a failure refuses the run with the clause and its source, except where the policy says `warn`.

| Clause | Source | Discharge | Policy |
| --- | --- | --- | --- |
| The demand has a binding | `data` block | bind (D-7.5) | refuse, naming the unsatisfied need and the cheapest plan over budget |
| Every version the binding names has the attributes the program requires (`reviewed = complete` for a `complete` need; `receipt = recorded` for `available recorded`) | `data` block | bind | refuse |
| Every label and series name a rule uses is in the binding's vocabulary (WF-15) | rules | bind | refuse |
| Every instrument of an `instruments` table is an instrument of the binding | `universe` | bind | refuse |
| The binding's span covers the `report` window plus the program's longest lookback (`𝒜.lookback`) | analysis | bind | warn (warm-up truncated; the first decisions are reported as under-warmed) |
| Every parameter value is in its declared range and of its type | `params` | static | refuse |
| The execution block is complete and admissible (WF-14) | `execution` | static | refuse |
| The derived state bound `𝒜.state_bound` ≤ the memory budget (4.6) | analysis | static | refuse |
| Versions declaring `revisions = none` hold at most one row per identity and `t` | binding | bind (a `count` query) | refuse |
| Prices of the `price` relation are positive on non-future instruments over the span | binding | bind (a `scan` with `≤ 0`) | refuse, naming the first row |
| Every bar label of the program's calendars is a label of the binding's `calendar_label` | binding | bind | refuse |
| Action reconciliation: a close ratio outside [0.6, 1.67] between consecutive bars of an instrument is explained by a `split` or a `dividend` that bar, or is listed in the version's reviewed exceptions | binding | bind (a pass over `close`, `split`, `dividend`) | warn with the count and the first five rows (the former bundle test, B-3; now a precondition that warns, per the warnings rule) |
| Delisting coverage: an instrument whose last bar precedes the span's end has a `delisted` row | binding | bind | warn with the count |
| Membership: a `member` row's instrument has a bar that day | binding | bind | warn with the count |
| An option program's `option_fill` needs `quote`; `greeks data` needs `greeks` | `execution`, binding | bind | refuse |
| `margin published` needs `margin` rows covering the instruments traded | `execution`, binding | bind | fall back to the next listed model, warned |

### 3.3 Invariants

Checked at every decision barrier after the executor's take; a violation halts the run naming the barrier, the clause and the values. These are the kernel's own correctness, so none is a warning.

| Clause | Source |
| --- | --- |
| `nav(T) = cash(T) + Σ_A position(A, T) · mark(A, T) · multiplier(A) + receivables(T)` | the book (5.1) |
| One decision per instrument per bar; a combo's legs count once each | F-5 |
| No read past the barrier: every data-layer query issued while evaluating barrier `T` carries `r ≤ at(T)` (4.3) | WF-6, A-4.5 |
| Every stored relation of Σ_t at `T` holds at most one tuple per identity (keyedness of kernel relations) | WF-11 |
| Every temporally recursive derived relation is keyed (WF-11) | analysis |
| `gross(T) ≤ limit` under the `leverage` field after every fill | `execution` |
| `|position(A, T)|` never exceeds the `names`/`weight` limits after a fill when the limit's policy is `reject` or `clamp` | `execution.limits` |
| The event log's fold equals the book: positions, cash and receivables recomputed from the events up to `T` equal X at `T` | 5.2 |
| The receipt filter of every pushdown aggregate equals the barrier's instant | 4.5 |
| Working orders are at most one per instrument | 5.8 |

### 3.4 Postconditions

Computed at `finish`; reported in the run report (7.1); a `halt` policy applies only in the acceptance suite (G5, G7).

| Clause | Source | Policy |
| --- | --- | --- |
| `decisions = fills + dropped + expired + working_at_end`, per instrument and per bar, with combos counted per leg | the executor | report; halt in CI |
| Every relation on a path to `decide` derived at least one tuple (coverage) | observe (7.2) | warn (W8), naming the first empty relation and its `why not` at a sample bar |
| Every observed relation's ledger was written over the bars the mode permits | observation | report |
| A run stopped at any barrier equals the run over the stream truncated there; a run resumed from a checkpoint equals the unbroken run | A-4.2, G5 | halt in CI |
| Final cash = initial + Σ fills (signed notional) − Σ costs + Σ dividends and settlements + Σ funding | the event log | report; halt in CI |
| Every clause of 3.2 with discharge `bind` that was warned is listed with its count | | report |
| Every limit of 3.5 reports its trigger count and the bars it fired | | report |

### 3.5 Limits

Pre-trade, in every mode, enforced by the executor before an order is sent to the simulation or the broker (A-D27, K-6.4). Each limit comes from a field of `execution.limits`; its policy is the field's.

| Limit | Test, before accepting the bar's orders | Policies |
| --- | --- | --- |
| `gross <= X` | gross exposure after the bar's orders at their expected fills ≤ `X` · capital | `halt`, `reject` (drop the bar's increasing orders, keep reducing ones), `clamp` (scale increasing orders), `warn` |
| `net <= X` | \|net exposure\| after orders ≤ `X` · capital | same |
| `weight <= X per name` | \|position · mark\| per instrument after orders ≤ `X` · capital | same |
| `names <= N` | instruments with non-zero position after orders ≤ `N` | `halt`, `reject`, `warn` |
| `loss <= X per day` | the day's realised plus unrealised loss so far ≤ `X` · capital; when exceeded, only reducing orders pass | `halt`, `reject`, `warn` |
| `drawdown <= X` | NAV ≥ (1 − X) · peak NAV; when breached, only reducing orders pass | `halt`, `reject`, `warn` |
| `turnover <= X per PERIOD` | traded notional in the trailing period after the bar's orders ≤ `X` · capital | `reject`, `clamp`, `warn` |
| `participation <= X` | each order ≤ `X` · bar volume (the present participation cap, now a limit) | `reject`, `clamp`, `warn` |

Default policy when a limit is listed without one: `reject`. A limit is never `warn`-only by default (the one place the warnings rule yields, as agreed: a limit that only warns is not a limit). The `leverage`, `oversize` and `ruin` fields remain as they are, since they describe degenerate book states rather than limits.

### 3.6 Discharge

The analyser's abstract interpretation (C-3) proves what it can: a `hold(A, W)` whose `W` has interval `[0, 0.15]` under a `weight <= 0.15` limit is `static(tier 1)`; a limit the intervals cannot bound is `runtime`. A static proof never removes the runtime check (C-6.8); it only lets the report say `proven`. The statuses are `proven(tier)`, `checked` (held at every barrier), `violated(barrier, values)`, `warned(count)`, `not_applicable`.

### 3.7 `abt contract`

Prints the contract as a table (clause, source with the block or rule label and span, discharge, policy, status when a run id is given), and `--json` as a relation the run writes to the data layer (`kernel.<run>.contract`, 7.1).

**Proposition 3.2 [N] (the contract is a function of the program).** Two programs with equal normal form have equal contracts up to the binding-dependent statuses. *Proof.* `contract` reads the IR, the analysis and the execution block, all of which the normal form fixes (V-8.4); statuses with discharge `bind` or `runtime` depend on the binding and the run, which are outside the normal form by construction. ∎

---

## 4. Pipeline and evaluation

### 4.1 Stages

Parse → Check → Normalise → Analyse → Contract → Plan → Bind → Execute, with the invariants I1–I5 of C-2.2 and the printable results of C-2.3. Contract is new (section 3) and sits between Analyse and Plan because the planner uses the contract's static proofs to elide nothing and the runtime clauses to place its guards. Bind sends the demand (D-7.2) and receives the binding; it is the only stage that talks to the data layer before the stream.

### 4.2 The fold over the stream

The kernel subscribes to a stream (D-5) whose spec it builds from the binding: the relations of the binding, the calendars the program declares, the receipt filter `r ≤ at(barrier)` applied per barrier, and the clock source of the mode. It consumes events in order:

- `Tuple`: insert into the stored relation's Σ_t table at its `t` (a revision replaces the earlier row of the same identity and `t`; the earlier row is kept in the event log the kernel writes, 7.1); if `t`'s bucket is before the last barrier of its calendar, apply the late rule (4.4).
- `Barrier{C, ℓ}` for the decision calendar, inside the `report` window: `executor.fill(ℓ)` (the previous bar's orders and working orders at this bar), `executor.open_bar(ℓ)` (actions, settlements, expiries, marks, margin, the limits' state), `decide_at(ℓ)` (evaluate `decide(ℓ, ·)` and every observation), `executor.on_decisions(ℓ, …)` (limits, then the orders become pending or are sent), the invariants of 3.3, then the checkpoint rule. For other calendars: open the bucket and run the aggregate pushdowns whose windows closed.
- `Mark`: `feed_down`/`feed_up`/`gap` are recorded and, in live modes, feed the falling-behind rule (6.3); `session_open`/`session_close` drive `moo`/`moc` orders.

The order within a barrier is the present driver's (`src/kernel/fold.rs` `barrier`), so `tests/fold.rs`'s equality carries over; what changes is that bucket opening and closing are events, not inferences.

### 4.3 The receipt filter is the barrier

Every data-layer query the evaluator issues while evaluating barrier `ℓ` of the decision calendar carries `as_of_seq` = the barrier's `seq` and `r ≤ at(ℓ)`. The as-of join `R(…) asof T` on a stored relation is a `latest` query with that filter; on a derived relation it is the memoised walk of the present kernel. This is the whole of point-in-time correctness in the split: the data layer compares, the kernel chooses what to compare against (A-D6).

**Proposition 4.1 [N] (causality across the boundary).** The decisions at `ℓ` depend only on events with `seq ≤ seq(barrier ℓ)`. *Proof.* Every read is a snapshot read at that `seq` (D-3.3) or a Σ_t read, and Σ_t at `ℓ` was built from events with smaller `seq`; F-7.1 gives the rest. ∎

### 4.4 Late data

A tuple whose bucket closed before it arrived is stored at its `t`; every derived tuple that read that key or a later one is invalidated (the present `invalidate_from`), windowed groups forget those bars, and decisions already emitted stand. The run counts late tuples per relation and reports them (`late_tuples` in the run report); in live modes a late tuple that would have changed an emitted decision is detected by re-evaluating the affected `decide` instances and is reported as a `late_decision_change` event without retracting anything (prefix monotonicity, F-7.2).

### 4.5 Pushdown

The planner pushes an aggregate literal down to the data layer (D-6) when: its inner conjunction is one stored-relation atom (or a bar view of one) with the group's entity bound; the window is one of D-6.1's forms; the conjunction does not read the window's base time; and the relation's calendar is at or finer than the rule's. The pushed request carries the barrier's receipt filter and the program's numerics. Proposition D-6.1 guarantees equality; `show plan` prints `pushdown` on the literal; `--no-pushdown` is a test flag only (G3 compares both).

### 4.6 The state bound

`𝒜.state_bound = Σ_R |Ent_R|^{a_R} · (w_R + h) · width_R` over derived relations (V-8.7), with `|Ent_R|` from the binding's `distinct` count, `w_R` the longest window in bars, `h` the retention horizon, `width_R` the row's bytes; plus the windowed groups' state (F-7.4). The kernel refuses a program whose bound exceeds `--memory-budget` (default: half of physical memory) before binding the stream (A-D30).

### 4.7 Checkpoints and resume

A checkpoint is Σ_t serialised (V-8.7) plus the executor's state and the stream cursor (`seq`), written as a cache object through the data layer (D-3.5) keyed by (run id, `seq`); `--checkpoint-every month|N` as today. Resume opens the checkpoint, verifies (H(P), β, mode) and subscribes from the cursor (D-5.4). G5 holds.

---

## 5. Market simulation

### 5.1 The book X

| Column set | Content |
| --- | --- |
| positions | per instrument: quantity (signed), average cost, multiplier, class |
| cash | cash; debit balance; receivables (pay date, instrument, amount); futures variation margin account (5.4) |
| marks | per instrument: the mark (price relation, or `option_mark`), its source (`price`, `mid`, `last`, `model`, `stale(last_seen)`) |
| margin | initial and maintenance requirement under the selected model; excess liquidity |
| orders | pending (this bar's decisions), working (beyond the bar), open targets, combos awaiting all legs |
| limits state | the day's P&L, peak NAV, trailing turnover |
| counters | the present summaries (costs, liquidity, funding, margin calls, late) |

The book is written to Σ_t as the kernel relations of 2.4 at every decision barrier and queried like any relation.

### 5.2 Events

Every event has `run`, `seq` (the run's own counter), `t` (the barrier), `instrument` where applicable, and `rule` where a decision caused it. The vocabulary:

| Event | Fields | Emitted by |
| --- | --- | --- |
| `decision` | decision term, rule, order | `decide_at` |
| `order_sent` | order id, decision, order type, tif, legs | executor (`live-broker`: when acknowledged) |
| `fill` | order id, quantity, price, components {commission, fee, slippage, impact, spread}, participation, partial, forced, source (`sim`, `broker`) | executor |
| `partial_fill` | as `fill` plus remainder and its fate (`expired`, `reissued`, `working`) | executor |
| `dropped` | decision, reason | executor |
| `rejected` | order id, reason (a limit of 3.5, or the broker's) | executor |
| `cancelled`, `replaced` | order id, by decision | executor |
| `expired_order` | order id, tif | executor |
| `split`, `dividend_declared`, `dividend_paid`, `delisted` | as today's `ActionRecord` | executor |
| `settlement` | instrument, settlement price, variation margin cash flow | futures (5.4) |
| `roll` | root, from contract, to contract, the two fills, adjustment | futures (5.4) |
| `contract_expiry` | instrument, final settlement, cash flow or delivery | futures (5.4) |
| `exercise`, `assignment`, `option_expiry` | instrument, quantity, intrinsic, settlement (`cash`: amount; `physical`: the underlying fill at strike) | options (5.5) |
| `adjustment` | option instrument, old spec, new spec, cause (split, special dividend) | options (5.5.5) |
| `margin_call`, `forced_liquidation` | requirement, equity, the fills | margin (5.6) |
| `funding` | cash interest, debit interest, borrow fee, short rebate | executor |
| `limit_triggered` | limit, policy, the orders affected | limits (3.5) |
| `reconciliation` | broker vs book differences, policy applied | `live-broker` (6.4) |
| `late_decision_change` | decision, the late tuple | 4.4 |
| `fx` | reserved (A-D32) | — |

**Proposition 5.1 [N] (the event log is sufficient).** The book at any barrier is a function of the event log up to it and the initial capital. *Proof.* Every change to positions, cash, receivables and the variation account is one of the events above with its amount; marks are the data (recomputable from the binding at the barrier); margin is a function of positions, marks and the model. ∎ This is the invariant of 3.3 the kernel checks on itself.

### 5.3 Equities

As the present executor (`README.md`, "Reading the kernel"), with these restatements: the cost, liquidity, impact, funding, borrow, delisting and action models are unchanged and are selected by the execution block; working orders (`moo`, `moc`, `limit`, `stop`) are unchanged; whole or fractional lots as declared.

### 5.4 Futures

- **Instrument.** A program trades a root through the data layer's continuous series (`contract(A, T, Root, Expiry, PointValue)`, D-2.4 `continuous`); the book holds the root's position and knows the contract it stands in.
- **Daily settlement** (`settlement daily`): at each session close of the root's calendar the position is marked at the `settlement` price (or the close when the binding has none, warned) and the variation `Q · (S_t − S_{t−1}) · point_value` flows to cash as a `settlement` event; `nav` is unchanged by settlement (cash and mark move together), the debit balance and funding see the cash.
- **Roll.** When `continuous` changes contract at `t`, the executor emits a `roll`: a closing fill of the old contract at its last price and an opening fill of the new at its first, each with the per-contract commission, and the position carries over; the back-adjusted series' additive adjustment is recorded so that the P&L of the roll bar equals the real contracts' (the adjustment is not P&L).
- **Expiry.** A contract held at `last_trade` with no roll is settled at the final settlement price (cash-settled) or, for physical delivery, closed at that price with a `contract_expiry` event and a warning (the simulation does not deliver).
- **Margin.** From the `margin` relation when `margin published` is listed and rows exist for the contract and day; else `span` from risk arrays when the binding has them; else `approx F` (5.6, model 5), warned.
- **Prices across zero**: permitted; the positive-price precondition exempts futures (3.2).

### 5.5 Options

#### 5.5.1 Instrument and universe

An option is an instrument with `spec` (2.4). The universe is written over `chain`, whose extension at `T` is what the dynamic need admits (D-4.4): expiries in a range, moneyness in a range relative to the spot the need names, right, style, open interest. The resolver's pre-pass makes the chain's quotes present before the run in replay (D-7.6).

#### 5.5.2 Marks

`option_mark mid`: (bid + ask)/2 from the latest `quote` row admitted at the barrier; a one-sided quote (bid 0) marks at the ask/2 and is flagged `one_sided`; no quote marks at the last fill or last mark, flagged `stale`. `last`: `quote.last`. `model`: the model price of 2.5 at the IV of the latest `greeks` row (data) or the last solved IV, flagged `model`.

#### 5.5.3 Fills

`option_fill touch + f spread`: a buy fills at `ask + f · (ask − bid)`, a sell at `bid − f · (ask − bid)`, with `f ∈ [0, 1]` (`f = 0` is the touch); `mid + f spread`: mid plus or minus `f` times the half-spread; `last`: the last trade. The `spread` component of the fill event records the cost against mid. Commission `per_contract`. Participation applies to `quote.volume` when a `participation` limit is set. A **combo** fills all-or-none: every leg's fill price is computed as above at the same barrier; if any leg has no quote the combo is dropped with the leg named; a combo `limit(P)` is filled when the net of the leg prices is at or better than `P`, else it works under its tif.

#### 5.5.4 Exercise, assignment, expiry

At the expiry session's close (`settlement_time am` settles at the open of the expiry day on the opening prints, for AM-settled index options; `pm` at the close), per held option:

- long, ITM beyond `atm_tolerance` (a fraction of the strike, default 0): exercised; `physical`: a fill of the underlying at the strike for `Q · multiplier` with no commission unless `commission per_contract` lists an exercise fee; `cash`: intrinsic × multiplier to cash. OTM or within tolerance: `option_expiry` worthless.
- short, ITM beyond tolerance: assigned, the mirror of the above. OTM: expires.
- **Early assignment** of an American short (`early_short when extrinsic < DIVIDEND`): on the bar before an ex-dividend date of the underlying, a short call whose extrinsic value (mark − intrinsic) is below the dividend is assigned at that bar's close; `when extrinsic < X` uses a fixed threshold instead; `never` disables. Puts are assigned early when extrinsic < the financing of the strike to expiry under the same rule's threshold form (deep ITM puts), documented as the approximation it is.
- **Early exercise** of longs: `never`, or `decide`, in which case the program emits `exercise(A, Q)` and the executor exercises at that bar's close.
- Intrinsic uses the underlying's `price` relation mark at the settlement instant (the AM print for `am`).

#### 5.5.5 Corporate-action adjustments

Per OCC conventions, applied at the ex-date before marks: a whole-ratio split `n:1` multiplies the contract quantity by `n` and divides the strike by `n`; a non-whole ratio keeps the quantity and adjusts the multiplier (`100 · ratio`) and the strike (`strike / ratio`); a reverse split adjusts the deliverable; a cash dividend adjusts the strike by its amount only when it is *special* (the version's `dividend` row carries `special = true`, else no adjustment); mergers and spin-offs are not modelled (a `delisted` underlying forces the option's expiry at intrinsic against the delisting price, warned). Every adjustment is an `adjustment` event and `spec` reflects it from that bar.

#### 5.5.6 Greeks

`greeks data`: the relation from the binding (ORATS: iv, delta, gamma, theta, vega, rho at the snapshot). `greeks model`: computed at the barrier from the mark's implied volatility (2.5 `implied_vol` on the mark; `⊥` when the mark violates bounds, so a rule reading `greeks` for that instrument is stuck, not wrong). A program reads `greeks(A, T, …)` either way (A-D25).

### 5.6 Margin models

The execution block lists models in order of preference; at each barrier the first applicable model is used per instrument, and the choice is recorded in `margin_req`'s source column.

1. **`published`.** `margin(instrument, t, initial, maintenance)` from the binding (per contract for futures; per contract or per underlying for options when the source supplies it).
2. **`reg_t`** (securities, as FINRA Rule 4210 and the OCC's strategy-based rules state them, each a documented formula): long stock 50 % initial / 25 % maintenance; short stock 150 % / the greater of 30 % of market value or $5 per share; long options paid in full (no loan value under 9 months); naked short equity option: 20 % of the underlying's market value − the OTM amount + the premium, with a floor of 10 % of the underlying's value (puts: of the strike) + premium; naked short broad-based index option: 15 % / floor 10 %; spreads: the maximum loss of the spread (long and short of the same class and expiry ordering); straddles and strangles: the greater of the two naked requirements plus the other side's premium; covered calls and cash-secured puts: no additional requirement when the cover is held. The strategy recognition is by instrument and position sign per underlying; the requirement is the minimum over admissible pairings (a small assignment problem per underlying, solved exactly since the legs per underlying are few).
3. **`portfolio`** (a TIMS-like scenario method): per underlying class, the book's P&L is computed under price shocks on an eleven-point grid of ±15 % (broad-based indices ±8 %, −6/+8 % for narrow sectors, per OCC classes) with options repriced by the model of 2.5 at their current IV; the requirement is the worst loss over the grid, floored at $0.375 per option contract times multiplier; no offsets across classes.
4. **`span`.** Risk arrays (16 scenarios per contract) from the binding's `span_risk_array` relation when the vendor supplies them; the requirement is the maximum scenario loss per combined commodity plus the inter-month and inter-commodity charges and credits the file states; absent the file, the model is skipped.
5. **`approx F`.** `F` × notional per contract (futures) or `F` × underlying notional (options), warned at every bar it is used, and reported as the owner's open item (A-8).

A margin call is the present rule (equity below maintenance): `halt`, `liquidate`, or `allow` with the count. A **futures margin call** liquidates the smallest set of contracts restoring maintenance, largest requirement first.

### 5.7 Cost models

As the present executor for equities and futures; options use `option_fill` for the spread cost, `per_contract` commission and no vol-scaled slippage or impact (both `none` for the option class regardless of the fields, since the spread model is the cost).

### 5.8 Working orders and combos

One working order per instrument; a new decision on the instrument cancels and replaces it (`replaced` event). A combo is one working order on each leg's instrument with a shared id; a decision on any leg's instrument cancels the whole combo.

---

## 6. Executor modes

### 6.1 The trait

```
trait Executor {
    fn open_bar(&mut self, k: &mut Kernel, t: Time) -> Result<(), RunError>;        // actions, settlements, expiries, marks, margin, limits state
    fn on_decisions(&mut self, k: &mut Kernel, t: Time, by_instrument: &Decisions) -> Result<(), RunError>;   // limits, then pend or send
    fn fill(&mut self, k: &mut Kernel, t: Time) -> Result<(), RunError>;            // fills at t of what was pending or working
    fn on_event(&mut self, k: &mut Kernel, ev: &Event) -> Result<(), RunError>;     // broker events in live-broker; no-op otherwise
    fn finish(&mut self, k: &mut Kernel);
    fn checkpoint(&self) -> Result<Bytes, String>;
    fn restore(&mut self, state: Bytes) -> Result<(), String>;
}
```

`open_bar`, `on_decisions` and `fill` write the book and emit events through the kernel's event sink (7.1). `SimExecutor` implements section 5. The order of calls at a barrier is 4.2.

### 6.2 `replay-sim` and `live-sim`

Identical: the stream differs (D-5.5 against D-5.2 live), the executor is `SimExecutor`. `live-sim` is the rehearsal mode and is in scope (A-D27). The kernel records the run as any other; Proposition A-4.4 is G4.

### 6.3 Falling behind

The stream's `lag` marks (D-5.4) give the kernel the delay between a barrier's instant and its consumption. A barrier consumed later than `at + deadline(C)` (configuration, default one bar length) marks the bar's decisions `late`; the execution block's `late send | reject` decides whether `on_decisions` passes them (`reject` drops them with reason `late`). Evaluation is never skipped (prefix monotonicity needs every barrier). A kernel whose lag exceeds the deadline on more than a declared fraction of barriers is a warned state with the measured lag in the run report.

### 6.4 `live-broker`: the interface

```
trait Broker {
    fn send(&mut self, order: OrderRequest) -> Result<OrderId, BrokerError>;        // NewOrderSingle / NewOrderMultileg
    fn cancel(&mut self, id: OrderId) -> Result<(), BrokerError>;
    fn replace(&mut self, id: OrderId, order: OrderRequest) -> Result<OrderId, BrokerError>;
    fn events(&mut self) -> impl Iterator<Item = BrokerEvent>;                      // ExecutionReport{new, partial, filled, cancelled, rejected, expired}, PositionReport, CashReport, Disconnect, Reconnect
    fn snapshot(&mut self) -> Result<(Positions, Cash), BrokerError>;               // for reconciliation
}
```

`BrokerExecutor` implements `Executor` over a `Broker`: `on_decisions` applies the limits (3.5) then `send`s; `fill` is driven by `ExecutionReport`s delivered through `on_event` as stream events (the data layer records them in the run's stream so they replay, D-5.4); the book is driven by broker fills with the broker's commissions, and the simulated cost models are not applied; **reconciliation** runs at every session open and on reconnect: the broker's snapshot against the book, policy `halt` (default), `adopt` (the book becomes the snapshot, a `reconciliation` event), `warn`. Order lifecycle as agreed: one working order per instrument; a new decision cancels/replaces; rejects are `rejected` events with the broker's reason. The first implementation targets IBKR (FIX 4.4 session, or the TWS API behind the same trait); the adapter is not part of this specification (A-D27) and nothing above depends on its dialect.

### 6.5 The deployment document

Taken by the kernel in live modes only (A-D28), hashed separately, logged with the run:

```
deployment {
  account      ID            broker IBKR            session { host, port, sender, target, version }
  routing      destination   DEFAULT               tif_map { market -> DAY, moc -> MOC, ... }
  kill_switch  { halt_all on SIGNAL | file PATH | socket; flatten_all through decisions }
  reconcile    halt | adopt | warn      at session_open, reconnect
  deadline     { base 55s; session 5min }
  recording    stream NAME               # the data-layer stream the live session records into
}
```

`flatten_all` emits closing decisions through the normal path so they are logged and limited like any decision; `halt_all` stops evaluation after the current barrier.

In `live-sim` the same document serves the rehearsal (A-D52): its kill switch, deadlines and recording stream apply (`recording stream S` names the session `--session S` would, and the two must agree when both are given); its account, broker session, routing and reconciliation fields are hashed and logged with the run but not acted on, since the executor is `SimExecutor` (6.2) and no broker is connected.

---

## 7. State tables and the query service

### 7.1 Σ_t and the run's outputs

The tables of V-8.2 (E[R], W, K, Δ, F, C, Π, X, β), with X as 5.1. At `finish`, and incrementally at every checkpoint, the kernel writes through `put` (D-3.6) under `kernel.<run id>.*`:

| Relation | Content |
| --- | --- |
| `events` | the event log of 5.2 |
| `book` | X at every decision barrier (positions, cash, nav, capital, mark, exposure, margin_req) |
| `decisions`, `fills` | projections of `events` for the common queries |
| `coverage` | C summarised per relation |
| `contract` | the contract with statuses (3.7) |
| `binding` | β, the parameter point, the mode, the deployment hash, H(P), the normal form |
| `plans` | Π per rule (V-8.4 form 5) with observed calls and tuples |
| `ledger.<R>` | the extension of every observed relation over the run (V-8.1) |
| `late` | late tuples and `late_decision_change` events |
| `report` | the run report: warnings with their biases, counters, timings, lag |

The schemas are P-4. A run that halts writes what it has with `status = halted(reason)`.

### 7.2 The query language

Forms, parsing, checking and algorithms are V-8 and V-8.3, served over the kernel service (P-3.4): `? R(…) at|for|over`, `why`, `why not [in written order]`, `trace decide at t`, `coverage over W`, `show program|plan|schedule|calendars|contract`, `ledger R into NAME` (which writes a relation through the data layer rather than a file). A query is parsed and checked by the kernel as a rule of a stratum above the program (V-8.1); the answer is an Arrow table. Provenance Δ and failures F are kept within the retention horizon; `why` beyond it replays from the nearest checkpoint (V-8.3). The query layer is a client and adds nothing to this language (A-D16).

### 7.3 Live queries

Between barriers the kernel answers queries over the Σ_t the last barrier closed (V-8.8); during a barrier's evaluation queries wait. A query never blocks the stream: the service runs on its own thread with a read view of Σ_t taken at the barrier.

---

## 8. Analyser, planner and verification: deltas to `compiler-design.md`

| C- section | Delta |
| --- | --- |
| C-3 Analyse | adds `state_bound` (4.6) and the contract inputs: per decision rule, the interval of every decision argument (weights, quantities) for 3.6; per aggregate literal, the pushdown eligibility predicate of 4.5 |
| C-4 Rewrite | unchanged |
| C-5 Plan | the cost model gains the data layer's `explain` estimates for stored-relation generators and pushdowns (D-4.1); the planner may choose pushdown or local evaluation by cost; both are equal by D-6.1 |
| C-6 Verification | tier 1 discharges contract clauses (3.6); tier 5 (translation validation) runs over the socket as well as in-process (G3); tier 6 (runtime) is the invariants of 3.3 and the limits of 3.5 |
| C-7 Observability | the state tables are served over the protocol (7.2); `materialise` becomes `ledger … into` through the data layer |
| C-9 Order | superseded by `05-migration.md` |

Everything else in that document stands: normal form and hash (now BLAKE3, A-6), abstract interpretation, R1–R6, the exact DP planner, the SMT and CHC tiers by subprocess with replay files, the 2⁻⁴⁰ margin, no static verdict removing a runtime guard.

---

## 9. Acceptance

### 9.1 Corpus and books

G1 and G2 of A-7.1, on `corpus-v2/`. The parity scripts take the run's `events` and `book` relations from the data layer instead of the present `--dump` directory (`scripts/parity/*` are updated in M-4).

### 9.2 Fold, resume, determinism, reference

G3, G5, G6 as A-7.1; `tests/fold.rs` is rewritten over the stream service with the stream truncated at a barrier (D-5.5 `--to-seq`).

### 9.3 Options cases (G8)

Each case is a synthetic chain over a synthetic underlying with hand-computed expected events, in `tests/options.rs`:

| Case | Expected |
| --- | --- |
| long call, ITM at expiry, physical | `exercise` with the underlying fill at the strike for `Q · 100`; cash −`Q · K · 100`; the option position is gone |
| short put, ITM at expiry, physical | `assignment`: the underlying bought at the strike; cash −`Q · K · 100` |
| long put, cash-settled index option, AM settlement | `exercise` with cash +intrinsic × multiplier at the AM print |
| OTM at expiry, both sides | `option_expiry` worthless; no cash |
| ATM within tolerance | expires |
| short call before ex-dividend, extrinsic < dividend, `early_short when extrinsic < DIVIDEND` | `assignment` at the bar before the ex-date; no dividend owed |
| the same with `early_short never` | assignment only at expiry |
| `exercise(A, Q)` decision on a long | `exercise` at that bar's close |
| vertical spread `combo` with quotes on both legs, `touch + 0.25 spread` | one `fill` per leg at touch ± a quarter spread, both at the same barrier, one order id; `limit(P)` better than net: filled; worse: working then expired at tif |
| a combo with one leg unquoted | `dropped` naming the leg |
| 2:1 split on the underlying | `adjustment`: quantity doubled, strike halved; `spec` reflects it from the ex-date |
| 3:2 split | multiplier 150, strike × 2/3 |
| special dividend 2.00 | strike − 2.00 |
| naked short put under `reg_t` | `margin_req` = max(20 % · S − OTM + premium, 10 % · K + premium) × 100 × Q |
| vertical spread under `reg_t` | maximum loss |
| `portfolio` on a straddle | the worst loss over the ±15 % grid at the current IV |
| `published` present for one contract and absent for another, `margin published, reg_t` | source column `published` and `reg_t` respectively |
| `approx 0.2` only | warned every bar |
| `greeks data` vs `greeks model` on a quoted chain | the model's IV reproduces the data's IV within 1e-6 where the data is consistent; `⊥` where the mark is outside bounds |
| futures: daily settlement over three sessions | three `settlement` events whose cash flows sum to the P&L; `nav` unchanged by each |
| futures: roll | one `roll` with two fills and two commissions; position carried; P&L of the bar equals the real contracts' |
| futures: expiry without roll, cash-settled | `contract_expiry` with the final settlement |
| futures margin call under `published`, `liquidate` | the smallest set of contracts restoring maintenance, largest first, as `forced_liquidation` |
| limits: `weight <= 0.1 per name reject` with a `hold(A, 0.2)` | `rejected` with the limit; `clamp` fills 0.1; `warn` fills 0.2 and warns |
| limits: `drawdown <= 0.1 reject` after a 12 % fall | increasing orders rejected, reducing orders pass |

The pricing functions of 2.5 are checked against Haug's *Option Pricing Formulas* tables (BSM, Black-76, Bjerksund–Stensland) and the implied-vol solver against round trips to 1e-10.

---

## 10. Commands

```
abt kernel serve    [--socket PATH] [--data SOCKET|inproc]
abt check           PROGRAM... [--catalog CLASS_FILE|--data ...] [--json]
abt contract        PROGRAM [--run ID] [--json]
abt show            PROGRAM [program|plan|schedule|calendars|contract] [--tables]
abt ir | abt hash   PROGRAM
abt run             PROGRAM [--param k=v]... [--mode replay-sim|live-sim|live-broker] [--deployment FILE]
                    [--from SEQ|--to SEQ] [--checkpoint-every month|N] [--resume RUN:SEQ] [--observe R]... [--memory-budget BYTES] [--quiet] [--json]
abt query           --run ID 'QUERY'            # the forms of 7.2, against a finished run (replayed to the bar) or a live run
abt stream-check    PROGRAM --to SEQ            # G5: run to a barrier and compare with the truncated stream
```

`abt run` takes a program and, implicitly, the data service it is connected to; no data path, no executor option, no price relation. `--param` is kept for plans (A-D34).

---

## 11. What is established, new and conjectured in this document

- [E] Black–Scholes–Merton, Black 1976, Bjerksund–Stensland 2002, Jäckel 2015 (implied volatility); FINRA Rule 4210 and the OCC strategy-based margin rules; the TIMS scenario method (OCC); SPAN (CME); Kleene three-valued evaluation and magic-sets demand (V-5); how-provenance (V-8).
- [N] The configuration is in the hash (2.1); the contract is a function of the program (3.2); causality across the boundary (4.1); the event log is sufficient (5.1).
- [C] That `reg_t`'s strategy recognition by minimum over admissible pairings matches a broker's house computation on every book: settled by comparing `margin_req` with IBKR's reported requirement on a live-sim account over a month, once the live layer exists.
- [D] Order independence retained (V-5); the `execution` block extended rather than a second configuration block; `greeks` as one relation with two sources; limits default to `reject`; `approx` margin always warned; the broker adapter behind one trait with the dialect outside this specification.
