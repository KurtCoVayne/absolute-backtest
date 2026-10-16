# Errata and settled points of the three-layer specification

The seven documents of `docs/spec/` are the build sheet. This page records,
as the implementation lands, every place where a document is silent,
contradicts another, or had to be read against this tree, with the reading
chosen and the section it amends. An entry is never a change of intent; when
one would be, the owner decides and the entry says so.

Each entry: **id**, the sections it touches, what the documents say or omit,
what this tree does.

## Gates and baseline

- **E-1 (A-7.1, G1; M-1 principle 1).** G1 needs the books' reference inputs
  (`docs/parity-inputs.md`, `~/data/ref/` on the owner's machine), which the
  build container cannot reach. Every step is gated here instead by the
  translation-validation golden files (`tests/tv_golden.rs`,
  `tests/golden/*.txt`: every corpus-v2 strategy and both company books on the
  seeded synthetic markets, decisions, fills, book and final state rendered
  exactly; since M-2, by the fold over the stream of a data service holding
  the market, the batch driver being deleted) and by G6 (`tests/reference.rs`, the pandas reference). The owner
  runs `scripts/parity/` for G1. A golden file changes only in a commit that
  names the semantic change it accepts; each such change is listed under
  "Blessed changes" below.

## Blessed changes of the golden files

The baseline was taken after E-2 and E-3 below, which made the fold and the
batch driver agree on every corpus program. Each bless since:

- **E-46 (P-1.2, D-6.1, Definition V-3.5; the order an aggregate reads its
  window in).** The kernel gave an aggregate its window's rows sorted by the
  rule's variables in order of appearance, and an aggregate's arguments
  appear before its window's time: a window was read in the order of its
  values. `last` of a bar view was therefore the bucket's largest close and
  `first` its smallest open, `last` over a window its largest value, and
  sums added in value order. The data layer's pass, which reads a window in
  time order as D-6.1 says, disagreed with it, which is how pushdown's G3
  found it. A window is now read in its time order, the rows of one bar in
  the order of their other bindings, on every path (the general solve, the
  cached bars, the rows windows; `tests/aggregate_order.rs` holds the
  hand-computed week). Blessed with it: `resampled_momentum` (its daily
  close), `scale_in` (the last lot's close), `mw14` (the weekly close of the
  price floor), `r8l` (the close at minute 20 and 30 of a session), and in
  the last bits of their weights `low_vol_portfolio` and
  `volatility_targeting` (sums in time order). The parity runs of the books
  (G1, the owner's) were made with the first formulation's books on weekly
  and session bundles, which read no bar view; the second formulation's
  books are compared with this order.

- **E-48 (K-5.1, K-3.3 invariant 1; the NAV counts the receivables).** A
  dividend gone ex on a position held is a receivable until its pay date.
  For a short, the dividend it owes is a negative receivable. The NAV is
  now cash, plus the marked positions, plus the receivables, added in that
  order. Sizing, the leverage check, the maintenance check and the book
  record all read it.

  The leverage check's cash floor counts the receivables as cash. A book
  sized on that NAV borrows the receivable's amount until the payment, and
  that borrowing is not leverage (`leverage gross <= 1` would otherwise
  refuse every rebalance between an ex-date and its payment).
  `reference/engine.py` changes the same way in the same commit, and G6
  holds.

  Blessed with it:
  - `dividend_capture`, `low_vol_portfolio`, `low_volatility`,
    `mean_reversion_zscore`, `momentum_12_1`, `momentum_top_n`,
    `monthly_rebalance`, `pairs_trading`, `regime_momentum`,
    `relative_strength`, `size_proxy`, `sma_crossover` and `volume_spike`.
    Only their book lines change, between an ex-date and its payment.
  - `total_return_momentum`, which sizes on compounding equity. Its fills
    from the ex-date of 2023-04-17 on and its final cash change too.

## Kernel

- **E-2 (K-4.2, K-5.8; the `moc` and `moo_moc` orders).** "`session_open` and
  `session_close` drive `moo` and `moc`": a market-on-close order executes once
  its session has closed, at the session's last print. Before this entry the
  batch driver filled it at the step of the last print, which it knew because
  it had every bar of the session in memory (a look-ahead in time, not in
  price), and the fold filled it at the first bar after the decision, because
  its store held no later print yet. Both drivers now wait for the session's
  close (`Kernel::session_close`: the session's label, or on a minute base
  without sessions the day's last base label in the schedule), then fill at
  the last print, stamped with that print's time. The same condition governs a
  `moo_moc` entry that lands on the session's last print. Prices are
  unchanged; within a session, the book now holds the position until the
  close.
- **E-3 (K-4.2; the session-offset clock).** A streaming kernel (the fold)
  computed the `clock` relation of `decide on base at session + D` from its
  `close` store, which is empty when the kernel is built, so R8L never decided
  under the fold. The clock is now placed on the prints of the dataset the
  schedule is read from, as the batch driver places it.
- **E-4 (K-2.4, K-5.6; `margin_req`).** `margin_req(@T, Initial,
  Maintenance)` is the book's requirement; K-5.6 and case 17 of K-9.3 also need
  the model that priced each instrument. `margin_req` stays as written, and
  `margin_by(A, @T, Initial, Maintenance, Source)` gives the requirement per
  instrument with its source (`published`, `reg_t`, `portfolio`, `span`,
  `approx`).
- **E-5 (K-2.2, K-5.6; the margin call).** The block has no field for what
  a margin call does, while case 23 uses `liquidate`. A field
  `margin_call halt | liquidate | allow` is added, optional, defaulting to
  what the presets give today.
- **E-6 (K-2.2, K-5.5.4).** `early_short never` is used in the text and in
  case 7 but missing from the grammar; it is added.
- **E-7 (K-2.2, K-3.5; `participation`).** The execution field
  `participation X of bar volume` caps a fill (the remainder expires or
  re-issues as today); the limit `participation <= X` rejects or clamps an
  order before it is accepted. Both may be written; the limit acts first.
- **E-8 (K-2.2).** "Every field is required except `limits`": the option
  fields (`option_mark`, `option_fill`, `greeks`, `exercise`) are required only
  of a program whose demand names the option class, and `settlement` only of
  one that names futures.
- **E-9 (K-3.2, clause 8).** The state-bound precondition reads the binding's
  `distinct` counts, so it is discharged at bind, not statically.
- **E-10 (K-3.3, invariant 1; K-5.4).** With daily settlement, a future's
  term of the NAV identity is its position valued against the last settlement
  price, the variation since then being in cash; otherwise the variation is
  counted twice.
- **E-11 (K-4.1, C-5).** Plan runs before Bind in K-4.1, but the planner's
  cost model reads the data layer's `explain`; the kernel resolves the demand
  as a dry run (no fetch, no binding) before planning.
- **E-12 (X-3.3, D-6.1).** D-6.1 has no session window (`open0`,
  `close_before` of R8L are session-relative), while X-3.3 counts them as
  pushdowns. Until a session window form is added, those features are
  evaluated in the kernel.
- **E-47 (K-4.5, K-3.3, C-5, D-6.1; pushdown).** The conditions as the
  kernel reads them (`kernel::pushdown::eligibility`):
  - (a) the window's conjunction is the window and one atom of a stored
    relation, keyed by the group's instrument and the window's time. The
    instrument is the group: it is named by `within`, or shared with the
    rest of the rule. If it is not, the window reads every instrument's
    rows and gives one value.
  - (a) The atom's other arguments are distinct variables that only the
    aggregate reads. One read outside would group or filter the window's
    rows, which the pass does not do.
  - (a) `count` is evaluated here: it counts the window's rows, and the
    pass counts the rows with a value.
  - (a) A `rows[N]` window carries the checker's `universe(A, T1)` anchor
    and counts the instrument's universe bars, which are not D-6.1's
    `rows(N)` of a relation, so it stays here.
  - (a) A window over a bar view stays here. The bar view's own literal is
    pushed, and composing the two aggregates is not done.
  - (b) D-6.1's forms only, with a length and a minimum that parameters
    and literals make, and no lower bound. Session windows are E-12.
  - (c) The conjunction does not read the base time.
  - (d) The relation is held at the base calendar, the finest.

  The request of a pushed literal is sent once per run, after the stream's
  calendars are put. It walks the labels of the window variable's calendar
  with the program's numerics, and has no receipt filter. Each answer row
  carries `r`, the latest receipt among the instrument's rows at or before
  its label. At barrier T the evaluator reads an answer only when
  `r ≤ at(T)`. Then every row the pass read was received by T, so the
  answer is the one a request filtered at `r ≤ at(T)` gives. Otherwise the
  window is evaluated here, from Σ_t, which holds exactly the rows received
  by T. K-3.3's invariant that the receipt filter of every pushdown
  aggregate equals the barrier's instant is that check. One request per
  barrier and literal would be one round trip per bar.

  Where the aggregation binds the instrument (a bar view's head), the
  section at the label is read, every instrument's answer, if all of them
  are usable. A request the data layer refuses, or an answer naming an
  instrument the binding does not, leaves the literal here, with a note.

  The planner pushes every eligible literal. C-5 lets it choose by the
  `explain` cost; that choice waits for M-8's evaluator, whose local cost it
  would be weighed against.

  `abt plan` prints each aggregate's pushdown, or the condition that keeps
  it here. `abt run --no-pushdown` is the test switch and is outside the
  configuration's identity. `--timing` counts `pushed_answers`.
  `tests/pushdown.rs` runs every corpus program and both books with and
  without pushdown and compares them to the bit (G3); ten of them push
  something.

- **E-49 (K-5.2, K-3.4; the event log as the executor writes it).** The
  executor's cash, positions (with their average cost) and receivables
  change only by applying an event of the log (`kernel::events`). The log
  therefore folds to the book bit for bit (Proposition K-5.1;
  `tests/events.rs`), and each event's `cash_flow` is the amount the
  executor applied. The run's decisions, fills, drops and actions are
  projections of the log.

  Every decision becomes one order. Order ids are numbered from zero per
  run, and a re-issued target keeps its id.

  The kinds the equity and futures executor writes:
  - `decision` and `order_sent`.
  - `fill`: forced fills (a margin call, a delisting) are `fill` with
    `forced` set and no order. A capped fill is `partial_fill`, with its
    remainder and the remainder's fate (`expired` or `reissued`).
  - `dropped`: its bar is the bar of the decisions being filled, as the
    dropped list always gave it.
  - `expired_order`: the time in force ran out, or the data ended.
  - `replaced`: one kind for K-5.2's `cancelled, replaced`. A decision
    supersedes its instrument's working order or open target.
  - `satisfied`: an order the position already met. This is new;
    K-3.4's identity needs it.
  - `dividend_declared`, `split`, `dividend_paid` and `delisted`.
    `dividend_reinvested` is new, for the reinvesting dividend option.
  - `funding`: one event per flow.
  - `margin_call`: the forced fills follow it.

  K-3.4's identity reads, per order: filled, dropped, satisfied, replaced
  or expired, or open at the end (a re-issuing target or a working order).

  The book rows of K-5.1 are recorded at every decision barrier. The
  book-level row holds cash, the receivables, the NAV, the capital, gross,
  net and leverage. Until the margin models (M-7) the margin is the
  leverage model's: initial requirement = gross / gross multiple,
  maintenance = maintenance fraction × gross. Each holding's row holds its
  position, average cost and mark, with the mark's source (`price`, or
  `stale` for the last price seen).

  K-2.4's kernel relation `exposure(@T, Gross, Net, Leverage)` would clash
  with the `book` library's `exposure(@T, G)` view, so neither it nor
  `margin_req` is added to Σ_t before M-7. Their values are in the book
  rows.
- **E-50 (K-3.1 to K-3.4, K-4.2, G7; the contract as the kernel derives and
  checks it).** `contract::contract(P)` gives the 16 preconditions, 10
  invariants and 7 postconditions of K-3, under stable ids (`pre.prices`,
  `inv.nav`, `post.final_cash`), plus one clause per declared limit. A run
  sets each status; `abt contract` prints the table, and `--json` the
  records. Where the documents leave a choice:
  - `pre.vocabulary` is checked by WF-15 against the class catalog, which
    the registry generates. A label no bound row carries stays the run's
    `vocabulary` warning.
  - `pre.state_bound` estimates, per derived relation, its instruments to
    the power of its entity arguments, times its longest window in bars
    plus the memo's retention, times a row's bytes, plus its window
    state. The default budget is half the machine's memory; `abt run
    --memory-budget BYTES` sets it.
  - `pre.prices` scans every stored price column for values at or below
    zero. That covers the price relation, or the relations a derived
    price relation is computed from (MW14 marks at `tr_index`).
  - `pre.actions`, `pre.delistings` and `pre.membership` are the former
    bundle tests, read through the data layer's scans. One implementation
    (`contract::check`) serves `abt bundle test` too. Reviewed exceptions
    are not yet read from a version's attributes.
  - `pre.revisions` is not applicable until a source declares
    `revisions = none` (M-6). `pre.option_inputs` and `pre.margin_rows`
    are not applicable until M-7. `pre.calendars` holds by construction:
    the calendar labels the kernel puts are the bound bars'.
  - `inv.no_read_past`: the latest receipt among the tuples received
    before a decision barrier is at most at(T). `inv.pushdown_receipt`:
    the same among the pushed answers read there.
  - `inv.keyed_store`: a stored relation's identity is its entity
    arguments, then its labels (E-35). A later tuple of the same fact and
    time is a revision and replaces the row it revises, as K-4.2 says;
    the kernel used to keep both.
  - `inv.gross` is checked after a bar whose fills added exposure, when
    the leverage field refuses or halts (`allow` lets a fill borrow past
    the multiple). Gross is then at most the field's multiple of equity,
    or of the fixed capital, plus the bar's costs (which are never
    leverage).
  - `inv.position_limits` reads the book the accepted orders would leave
    at the decision's marks, since the limits are pre-trade.
  - `post.decisions` is E-49's identity per order. `post.final_cash` adds
    the log's flows by category. `post.truncation` is the acceptance
    suite's (G5).
  - `ExecConfig.inject` makes a named invariant fail at a bar, for G7's
    "every injected invariant violation halts" (`tests/contracts.rs`).
  - A refusal is `RunError::Precondition` (exit 2). A failed invariant is
    `RunError::Invariant`, naming the clause, the barrier and the values.
- **E-51 (K-3.5, K-2.2; the limits as the kernel enforces them).** The
  execution block's optional `limits` field is written
  `limits { KIND <= X [per WORD] [POLICY]; ... }`.
  - The checker refuses (P): an unknown kind, a kind set twice, a bound
    that is not positive (or not whole for `names`), a missing or
    unexpected `per`, a policy the kind does not admit. `reject` is the
    default policy.
  - `per` is `name` for `weight` and `day` for `loss`. For `turnover` it
    is `day`, `week`, `month`, `quarter` or `year`, the trailing period.
  - The limits act in `on_decisions`, after every decision is sent as an
    order (K-4.2), on the book the orders would leave at the bar's marks:
    the expected fills. A target weight is sized on the capital at that
    mark.
  - A breach acts on the bar's increasing orders, those that raise
    |position|. The exceptions: `names` acts on the orders that open a
    name, `participation` on each order above the bound, whether it adds
    or reduces.
  - `reject` drops the orders it acts on, each as a `rejected` event,
    which counts as dropped. `clamp` scales their deltas by the largest
    factor that meets the limit: found by bisection, since gross, |net|
    and turnover are convex in it. A `weight` clamp caps each target. A
    kind that cannot be scaled (`names`, `loss`, `drawdown`) rejects.
    `warn` lets the orders through. `halt` stops the run (`RunError::Risk`,
    naming the limit).
  - Each breach is a `limit_triggered` event. The limit's clause reports
    the bars it fired at.
  - `loss` reads the NAV the calendar day opened at: the previous day's
    last decision bar, or the initial capital. `drawdown` reads the peak
    NAV of the decision bars. `turnover` reads the notional the decisions
    traded in the trailing period.
  - A `weight` limit over decisions that are all target weights with
    constant weights within it is `proven` at tier 1. Its runtime check
    stays.

- **E-52 (K-7.1, P-4, K-9.1; the run's outputs as the registry stores
  them).** A run over a data service writes its outputs through `put`
  under `kernel.<run id>.*` when the stream ends, or when the run halts
  (the report's `status` is then `halted(reason)`, and the violated
  invariant is `violated` in its contract). The run id is the one the
  stream's calendars are put under (`kernel.<run>.calendar_label`, E-32):
  BLAKE3 over H(P), β, π and the mode (E-56).
  `abt run --data SOCKET` writes them and prints the id. A run whose data
  service holds a dataset in this process (`--synthetic`, `--bundle`, a
  Parquet directory) writes none, since its store is temporary.
  - The registry admits exactly one event time per relation, refuses a
    null entity, and keys a fact by its entities, then its labels (E-35).
    Each relation is given an identity under that rule:
    - `events` is keyed by `seq`, an entity. `decisions` and `fills` are
      projections of the log keyed by the event's `seq`, so they join the
      log on it. Their instrument is a value column.
    - K-5.1's `book`, with a null instrument for the book's rows, would
      put two kinds of row in one relation. It is two: `book`, keyed by
      the barrier's `t` (one row per decision barrier: cash, debit,
      receivables, NAV, capital, gross, net, leverage, margins, excess
      liquidity), and `holdings`, keyed by instrument and `t` (position,
      average cost, mark, mark source). `variation_account` is 0 until
      the futures model (M-7). The margins are the approximation's
      (`margin_source = approx`).
    - The relations without a time of their own (`coverage`, `contract`,
      `binding`, `plans`, `report`) carry `t` = the run's last decision
      bar. They are keyed by a label: `relation`, `clause`, `relation`,
      `rule` with `position` (an entity), and `key`.
    - `late` is keyed by the tuple's place in the stream (`seq`).
      `changed_decision` is false until live modes re-decide (M-6).
    - `ledger.<R>` is keyed by `bar` and `row`, the row's place among the
      bar's rows. R's arguments are value columns, since a stuck instance
      may leave an identity argument unbound, and a relation that is not
      keyed may hold two tuples of one identity at a bar.
  - The registry's types have no text column besides the interned label,
    so free text and JSON (`decision`, `order`, `reason`, `detail`,
    `literal`, `point`, `normal_form`, `configuration`, `value`,
    `stuck_literal`) are UTF-8 `Bytes`. `abt data query` prints them as
    text, and `--parquet FILE` writes them, and the labels, as strings.
    Hashes and versions (`program_hash`, `binding_id`, `version`) are
    labels, compared with `=`.
  - `binding` repeats the run's `program_hash`, `binding_id`, `mode`
    (`replay-sim`), `point` and `normal_form` on every (relation, version)
    row, and adds the run's `configuration` (the ExecConfig as JSON).
    `report` holds the status, the bars, the final cash, the base
    capital, the costs, liquidity, funding and kernel statistics as JSON,
    the plan's label, and each warning with its bias. Per-phase timings
    and lag wait for live modes (M-6) and the performance step (M-8),
    since a timing would make two equal runs' outputs differ.
  - The ledger of an observed relation (Definition V-8.3) is computed
    from the kernel at the end of the run, at every bar of R's calendar
    within the run, with the observation's `where` applied. This is
    Proposition V-8.4's equality: observation changes nothing decided,
    and the memo's instances are the run's. A stuck instance is a row
    with `stuck` and the halt it would have been in `stuck_literal`. A
    relation that takes inputs cannot be enumerated: its ledger is empty,
    with a warning. `post.ledgers` is `checked` with the number of
    ledgers written, or `not_applicable` for a run that writes no outputs.
  - "Incrementally at every checkpoint" is E-56.
  - The parity scripts read the run from the store (K-9.1), through
    `scripts/parity/abt_store.py`:
    - `r8l_compare.py STORE RUN LEG_FLAGS` reads the decisions, the fills
      of the event log (a fill's flow is its `cash_flow`), the book, and
      ES's sessions for the reporting calendar.
    - `mw14_compare.py STORE RUN CANON` reads the book and the report's
      base capital.
    - On a synthetic run of each book, they print what the dump-reading
      scripts printed, byte for byte.
    - `reference/diff.py` still replays the dump, because the reference
      engine reads the data the run was bound to from it. It moves with
      the dump, which M-9 retires.
  - Found and fixed on the way:
    - Over `--data SOCKET`, a reporting calendar of sessions had no days,
      since this process holds no dataset, and `--returns` was empty. The
      sessions are now read through the data service.
    - The "commissions are zero" warning is dropped from a run that
      charged a commission per contract or on notional. The CLI used to
      hide it only when printing.

- **E-53 (K-7.2, V-8, V-8.3, Definition V-8.1; the query language as
  the kernel answers it).** `abt query --strategy NAME --text QUERY ...`
  runs the program over its data and answers each query over the kernel at
  the end of the run. The forms:
  - `? BODY at T = DATE`, `at DATE`, `at T` (every bar of the run), or
    `over D1 .. D2` (a month `YYYY-MM` stands for its days); `for VAR = KEY`
    binds a variable to an instrument by its key. With no clause, the
    query is asked at the run's last bar.
  - `why R(...)`, `why not R(A, T) [in written order]`, `trace R at DATE`
    (V-8's `trace decide at T`, for any relation), `coverage [over D1 ..
    D2]`, `show program|plan [R]|schedule|calendars|contract`, and `ledger
    R into NAME`, which writes `kernel.<run>.NAME` through the data service
    (E-52's ledger shape), so it needs `--data SOCKET`.
  - A `?` query is compiled into the program, as Definition V-8.1 has it.
    Each query adds a relation `__query_<i>(@T[: CAL])` and the rule
    `__query_<i>(T) :- BODY.`, and an `observe` of it.
    - T is the variable `at` names, else the key of the body's first atom
      of a relation of the program. CAL is that atom's calendar.
    - The program with its queries is checked whole. A query that does not
      check is refused with the checker's diagnostics, before anything runs.
    - The run is of that program. By Proposition V-8.4 it decides as the
      program does; `tests/queries.rs` compares them to the bit.
  - The answer is the body's solutions at each bar, one column per variable
    in the order of first appearance. A one-atom query of a derived
    relation adds `rule`, the rule that derived each row: V-8's "a table
    with the rule of each", for `decide` and any derived relation.
  - Instances of a `?` body left stuck on a partial operation are counted
    in a note of the answer, with the first halt.
  - `why not` and `trace` report in the plan's order (`show plan`'s).
    `in written order` evaluates every rule in its written order.
  - `show schedule` is the program graph's partition into the open loop
    and the closed loop (F-11.5).
  - The query language replaces `abt explain` (the `trace` and `why`
    forms), `abt why-not`, `abt materialise` and `abt query --rel` (the `?`
    form). Each retired command now exits 2 and names its replacement.
  - The kernel service (P-3.4), which answers the same forms of a run it
    holds and between barriers while the run is live (K-7.3), lands next.
  - Fixed on the way: an observation is a demand (Definition V-8.3), so
    the analysis no longer flags an observed relation as elided.

- **E-54 (P-3.4, K-7.2, K-7.3; the kernel service as it is served).** The
  seven requests of P-3.4 are one message pair: `KernelRequest`, whose
  `op` says which request it is, and `KernelResponse`. A field a request
  does not take is left out.
  - `check` answers with the diagnostics, H(P) with the normal form
    (E-56), and the degrees of freedom.
    `contract` answers with the contract's table. `show` reads the
    request's sources, or the run `run_id` names.
  - `run` sends its id first, then a progress frame (`barrier`,
    `decisions`, `lag` 0 in replay) every `progress_every` decision
    barriers, then a last frame with `completed` or `halted(reason)` and
    the report as JSON. The run writes its outputs (K-7.1, E-52).
    - `window?` is `start` and `end`. `point` is a parameter list of
      names and literals. `mode` is replay-sim until M-6, and
      `deployment?` waits for M-10.
    - `embargo_mask` and `plan_step` wait for the query layer's plans
      (M-5). `checkpoint_every` (`month` or a number of decision
      barriers) and `from_checkpoint` (a checkpoint's key) are E-56's.
  - Each run is held by a thread that owns its program and kernel.
    Between barriers it answers the queries asked meanwhile, over the
    state the barrier closed (K-7.3), so a query waits for the next
    barrier and never blocks the stream. Once the stream ends, it answers
    over the final state until the service ends.
  - Live, these forms are answered: every form but a `?` over more than
    one atom (a rule the run must hold from its start, E-53) and `ledger
    ... into` (written once the run ends). A one-atom `?` is answered from
    the kernel's own extension of the relation, filtered by its constants
    and its `for` binding.
  - Of a run that ended, a `?` over more than one atom is answered by a
    run of the program holding the query as a rule (E-53). That run is
    refused when the data service would now bind the program otherwise
    than the run's binding.
  - A query's table follows its response in the same correlation
    (`has_table`).
  - `control halt` stops a running run at its next barrier, as a halted
    run, which writes what it has. `checkpoint` takes one at the end of
    the run's next decision bar and answers its key (E-56). `flatten` is
    refused until a live broker's book exists (M-10).
  - `status` gives a run's state, its last barrier and its decisions, or
    every held run's.
  - `abt kernel serve --data SOCKET` serves the kernel, and `abt kernel
    check|contract|show|run|query|status|control` are its client commands.

- **E-56 (A-4.1, A-6, K-4.7, K-7.1, D-3.5, P-3.1, P-3.4; the identity of a
  run, its checkpoints, and its outputs written as it goes).**
  - H(P) is BLAKE3 over the RFC 8785 form of the normal form's hash view,
    64 hex digits. The hash view is V-8.4 form 3: the program's structure
    with the strategy's own name as `self`, without spans, written names
    or the `report` block. It replaces the first formulation's SHA-256.
    - That RFC 8785 text is the rendering P-2 hashes, and `abt hash --show`
      prints exactly those bytes. The normal form as source (`abt hash
      --json`, `binding.normal_form`) is for reading. It carries the
      strategy's own name and its `report` block, which H(P) leaves out,
      so a renamed copy of a program is the same program.
    - `tests/golden/program_hashes.tsv` pins the hash of every corpus
      program and both books. A study lineage recorded under the old hash
      starts a new lineage (the study moves to the query layer at M-5).
  - β carries the parameter point (A-6). The demand sends every parameter
    of the program at its value (`Demand.point`): the strategy's by name, a
    library's as `unit::name`, so a default written as an override is the
    same point. The resolver puts the point, sorted, into the binding
    whose id it hashes. Two points are two bindings.
  - The run id is BLAKE3 over the RFC 8785 form of `{program: H(P),
    binding: β, step: π, mode}`, 64 hex digits. Outside a plan, π is null
    for a run over the whole stream. Otherwise it holds what of the
    request changes what the run decides, which Definition A-4.1 leaves to
    the plan step: the window (`start`, `end`), the bundle date (`as_of`),
    the truncation (`to_seq`, `horizon`) and an injected fault. The query
    layer's plans give π its step (M-5).
  - A checkpoint (K-4.7) is a cache object (D-3.5) keyed by BLAKE3 over
    `{checkpoint: run id, seq}`. It is its JSON in parts of at most a MiB,
    as a table (`part`, `t` = the checkpoint's last bar, `data`). A cache
    object written again under its key replaces the one there.
    - `abt run --data SOCKET --checkpoint-every month|N` prints each key,
      and `--resume KEY` reads one back. In process, checkpoints are still
      files (`--checkpoint-dir`) until M-9 retires the in-process stores.
    - `abt kernel run` takes `--checkpoint-every` and `--resume KEY`
      (`checkpoint_every`, `from_checkpoint`). It sends a frame
      `checkpoint KEY` as each one is written. `control checkpoint` takes
      one at the end of the running run's next decision bar and answers
      its key.
  - Resume compares the checkpoint's H(P), β, mode, run id and
    configuration hash with the run's, and refuses on the first that
    differs, naming it: another program; another binding (other data
    versions or another parameter point); another mode; another run of
    the program and binding (its window, truncation or bundle date);
    another configuration. The kernel service refuses a run that ends
    before it is bound (its binding, its checkpoint or a precondition
    refused) rather than reporting it as halted.
  - The outputs, "incrementally at every checkpoint" (K-7.1, E-52):
    - Before a checkpoint is handed out, the relations that grow with the
      run (`events`, `book`, `holdings`, `decisions`, `fills`, `late`) are
      written with the rows the run gained since its last write. The
      relations that describe the run as a whole (`coverage`, `contract`,
      `binding`, `plans`, `report`, the ledgers) are written when the run
      ends or halts.
    - `put` gains `replace`: the rows are the relation's next version
      whole, as a put of a new shape is (E-55). The growing relations are
      added to what is there, and a row already there must be the same
      row: a resumed run is the run, and the store refuses one that is
      not. The other relations replace the last execution's.
    - A resumed run carries its coverage and per-literal counters from
      the checkpoint, but its memo starts empty. Its coverage counts, its
      plans' observed counts and its kernel statistics therefore count the
      derived results it derives again. They describe the execution, not
      the run, and the last execution's are the run's.

- **E-57 (V-8.3, V-3; observing a relation declared on several
  calendars).** A signature that lists calendars (`@X: base | session`)
  is instantiated on each calendar the program reads it at, as
  `R@calendar`. `observe R` therefore observes every instance. Each
  instance is its own observation, with the `where` and `trace` written,
  and has its own ledger, `kernel.<run>.ledger.R@calendar`. Before this,
  the checker refused `observe R` for such a relation, as naming no
  relation.

## Data layer and protocol

- **E-13 (D-5.2, D-5.3; the instant of a barrier).** A barrier's `at` is the
  instant its bucket closes: for a calendar labelled at the bucket's end (every
  sub-daily resolution here) that is the label itself; for one labelled at the
  bucket's start (days and coarser, labelled at midnight) it is the bucket's
  last instant. A decision at ℓ sees every tuple with `r ≤ at(ℓ)`, as the
  kernel does today. At one instant the order is tuples, then marks, then
  barriers; barriers of one instant are ordered by calendar, finer first, the
  decision calendar last.
- **E-14 (D-3.1, D-5.3).** Partitions are sorted by (t, entity, r) while the
  stream is ordered by receipt. Each partition records its least and greatest
  `r`; the stream's merge opens a partition when its frontier reaches the
  partition's least `r` and reads it in receipt order, so a row whose `r` falls
  in a later bucket than its `t` is still emitted in order.
- **E-15 (D-3.1).** Partition buckets: a month for a base of one day or
  coarser, a day for a base of one minute up to one day, an hour below a minute.
- **E-30 (D-3.1, D-2.6; partition files and versions).** A partition file is
  named by its content, `part/<relation>/<bucket>.<hash>.parquet`, and is
  shared by every version that holds that bucket unchanged; a version is the
  manifest that lists its files (`manifest/<relation>/<version>.json`), and
  its id hashes the schema, the attributes and the (bucket, file hash)
  pairs. Partitions keep each row's sequence number (D-3.3), so the order a
  source delivered its rows in is part of the version: the same rows
  delivered in the same order give the same version in any store. A log is
  a sequence of Arrow IPC segments `log/<relation>/<source>/<first>-<last>.arrows`;
  the entity offset index is a sidecar `.idx` (compressed sparse rows) until
  M-8 maps it. A declared constraint such as the equity class's `close > 0`
  is enforced from M-4, with the kernel's preconditions it pairs with (K-3.2,
  clause 10); until then a bundle's own tests stand where they stood.
- **E-16 (P-3.2).** `QueryForm`'s identity is a list of entities; the
  `series` class is keyed by a label (`value(name, t, r, v)`). The identity
  admits labels.
- **E-17 (D-4.4, D-7.6, P-3.1).** `explain`'s response gains `unresolved`,
  which D-4.4 and D-7.6 read.
- **E-18 (A-D7).** Times are nanoseconds in `i64`. Civil calendars carry no
  time zone until the live clock of M-6 needs one; until then a time is naive
  wall-clock, as the bundles are.
- **E-19 (A-D4, A-6).** BLAKE3 is built with its `pure` feature: no C or
  assembly in the build.
- **E-20 (X-1, X-3).** `std::simd` is not stable; the column loops are plain
  loops the compiler vectorises, and `arrow::compute` kernels where they fit.

- **E-31 (D-4.1, P-3.2; the query forms).** `latest` without an identity
  answers every identity's latest row at or before `t_max`; `distinct` lists
  the values in their first-seen (`t`, identity) order; `catalog` refuses a
  relation outside the registry's class; a label constant names a label of
  the registry, and one it never interned matches nothing. A view's
  relation that it names no version of is read at its current version until
  the resolver (M-6) binds every relation (the transitional resolver of
  M-2). A binding is recorded under the hash of its canonical form and pins
  its versions (D-3.4).
- **E-32 (D-2.4, D-5.1, D-5.2, P-4; the clock and the event batches).**
  `calendar_label` gains `at`, the instant a label's bucket closes (E-13),
  so that the data layer reads the barrier's instant rather than computing
  it. A stream's clock is the relation of its spec named `calendar_label`
  or `<namespace>.calendar_label`, else the registry's current one: the
  kernel computes its calendars' labels from the binding as it does today
  (the base bars, the civil periods' last bars, the sessions) and puts them
  as `kernel.<run>.calendar_label` before it subscribes, so that a barrier
  is an event of the stream for every calendar the program declares, and a
  pushed-down aggregate (K-4.5) walks the same labels (E-27). A barrier
  whose `at` is past the receipt filter is not emitted. Replay emits no
  marks until the live clock (M-6); `moo` and `moc` read the sessions from
  the binding (E-2). In an event batch a relation's columns are named
  `<relation>.<column>`, its event and receipt times are the event's `t` and
  `r`, and `entity` is its first entity column; the identity of a relation
  keyed by a label is ordered by the label's code.
- **E-33 (D-6.1, D-6.2, P-4; the aggregate pass).** The answer is keyed like
  the input: a row at each (group, `t`) where the group has a row and the
  window holds at least `min` rows (`bucket` keys by the bucket's label), in
  (`t`, group) order, with `t0`, `t1` bounding the answer. A row whose `t` is
  not a label of the request's calendar is in no window, as in the kernel,
  whose windows range over its time domain. The answer gains `error: Utf8?`,
  the message of an aggregate whose arithmetic fails at that row (the
  correlation of a constant series), which the kernel reports as its own.
  `rows(N)` counts one entity's rows, so it groups per entity; per-label and
  whole groups take time windows and buckets. A bucket of the per-instrument
  sessions calendar is not served (E-12).

- **E-34 (D-2.2, D-2.3, D-2.4; names and the registry's relations).** A
  market class's relation is stored as `<class>.<relation>` (`equity.close`,
  `future.close`, `series.value`): the registry holds a schema by class and
  name, and the classes share relation names. The class file still renders
  the short names. `instrument` gains `multiplier` and
  `commission_per_contract`, the contract terms a source declares (a
  multiplier marks a declared contract; a share has none), and `ticker`
  gains `to`, the end of the span a symbol is carried over, so that a
  security table's spans are kept exactly. An adapter's rows name their
  instruments by the source's keys; `vendor_id` attributes a key to its id,
  a key first seen is assigned the next id with its `instrument` row, and the
  registry's relations are written under one source so that each has one
  current version. The transitional resolver binds a demand's relations
  qualified by its class (a relation from several sources: the first source
  by name) with the registry's `instrument`, `vendor_id` and `ticker`.
- **E-35 (D-2.1, D-2.2; the key of a fact).** The identity of Definition
  D-2.1 is the entity columns and `t`; a relation such as `member(A, T,
  List)` holds several rows of one instrument at one time, which that
  identity would refuse as conflicting revisions. A fact's key is its entity
  columns, its label columns and `t`: rows that differ in a label are two
  facts, rows that differ in a value with equal `r` conflict, and the latest
  revision is taken per key. The entity offset index stays on the entity.
  A value column a source does not carry is null.

- **E-36 (A-D3, M-2; how an in-process caller reaches the data layer).**
  The kernel depends on no other layer, so a caller holding a dataset in
  memory (the command line on a bundle, a study, a test builder) reaches a
  data service through a provider the front registers (`abt::install`): the
  dataset is ingested into a temporary store by the adapter of keyed rows
  (E-34), and the kernel talks to the service over an in-process channel,
  the same frames a socket carries. `abt run --data SOCKET` reads a running
  service instead. The kernel's `Dataset` is that in-memory builder; the
  kernel's own view of the data is Bind's (`kernel::bind::Bound`). A study's
  runs read one service and are truncated by horizon (the stream's `to_seq`
  at the decision barrier) instead of copying a truncated dataset; a run
  truncated at t resolves ticker literals at t.
- **E-37 (D-5.2, K-4.2, K-4.4; receipt after the close).** A barrier is
  emitted at its bucket's close (E-13): a sub-daily bar received after its
  close (a recorded `ts_recv` with a processing delay) is after its own
  bar's barrier, so no decision at that bar reads it, and it is read from
  the next barrier on; a daily bar received before the day ends is read at
  that day's barrier. The fold before the split opened a bucket at a late
  tuple's receipt instant, a phantom bar, which the explicit barriers
  remove. Late tuples (K-4.4) are judged against the decision calendar: a
  tuple whose decision bucket was already decided invalidates what was
  derived from its key on. The fold also evicts the memo at every decision
  bar, as the deleted batch loop did (memory, never results), and opens a
  calendar's bucket at its barrier, the running sums' walk reading the grown
  domain.

- **E-38 (D-7.7, D-10, M-2; the first adapters and commands).** `abt data`
  serves a store directory on a socket and ingests, catalogs, queries (the
  text form `REL [latest] [at T | over T0..T1] [for KEY,...] [where COL OP
  VALUE [and ...]] [r <= T]`, the typed request printed first), reports
  statistics, pins and collects. The two adapters of M-2 read the present
  formats with the kernel's loaders and generators (`bundle_v2`: a bundle
  directory with its manifest, or a directory of one Parquet file per
  relation; `synthetic`: the present generators) and ingest through the
  data layer's adapter of keyed rows; they run in the front, which links
  both layers, until the legacy formats retire (M-9). A9 of
  docs/assessment.md is fixed: the synthetic drift and volatility cycle
  through ten profiles, so markets of up to ten names, the golden runs',
  are unchanged and larger ones stay priced.

- **E-39 (K-2.1, K-3.2, D-2.1, V-3.11; `require reviewed`).** `require
  reviewed` asks every version the binding names for a `reviewed` attribute
  (a demand's `require` attribute with an empty value asks for the key with
  any value); a `complete` need asks for `reviewed = complete`, under either
  stance, since WF-5's negation rests on it. `allow untested` becomes `allow
  unreviewed`, which admits versions without the attribute and warns on
  every run. `require tested` and `allow untested` are parse errors that
  name their replacement. Who states `reviewed`: an adapter or a reviewer.
  `bundle_v2` states `reviewed = complete` for a bundle whose tests passed;
  the generators' markets are complete by construction; `abt data ingest
  --attributes K=V,...` records a reviewer's statement. The kernel's
  in-memory datasets (the command line, studies, tests) are reviewed unless
  they say otherwise (a bundle whose tests have not passed, a loose Parquet
  directory). `abt run --untested` is retired: the program's `require` and
  the version's attributes decide, and the refusal is the resolver's
  (`RunError::Unbound`, exit 2).
- **E-40 (D-7.1, D-7.2, P-3.3; the demand on the wire).** The kernel sends one
  need per need its data block writes, naming the stored relations of the
  need that the program holds as primitives, with the need's calendar,
  `from` and attributes. A relation the store has no schema for is named in
  the program's class, so that the refusal names it. `Need` gains `label`,
  the need as the program writes it, for the resolver's messages. `PlanStep`
  gains the version's span and attributes, what the bind-time preconditions
  of K-3.2 read in the kernel. `prefer version V` matches a version's
  `version` attribute (a bundle's version) or a prefix of its id of at least
  eight characters; `base finest` takes the finest calendar; `latest` the
  current version, the first source by name when several hold one. A
  version's calendar is its `calendar` attribute (an adapter's statement of
  the bars it ingested), else its schema's.
- **E-41 (K-3.2; the span of a need).** A version whose span starts after
  the need's `from` is K-3.2's warm-up clause (warn), not a refusal: the
  store is the only source until M-6, and covering a span by fetching is
  the cost model's.
- **E-42 (D-2.6, D-7.2; a relation provided empty).** A source that offers a
  relation and holds no row of it (a market without delistings) states it as
  a version with no rows (`adapter::ingest_empty`), so that a demand naming
  it is answered and reads it empty. The front's in-memory datasets provide
  every relation of their class and of the series class, as a dataset's
  relations always were. What the data holds, for the warning of actions a
  program does not load, is a relation whose current version has rows.
- **E-43 (K-2.1, D-7.1; the budget and the chain need).** `budget { money X
  [CUR]; bytes N [B|KB|MB|GB|TB]; requests N; seconds S }`, every item
  optional and money zero when absent; `seconds` is D-7.1's fourth cap.
  `need chain of "U" within D1..D2 and M1..M2 [right call|put] [spot R]`
  reads the option class's chains: the checker refuses it in another class
  (Q), an empty expiry or moneyness range (Q), and `complete` on it (Q).
  The transitional resolver refuses a dynamic need until the option class
  lands (M-7).
- **E-44 (K-9.1, M-3; the parity scripts).** M-3's step lists the parity
  scripts reading `kernel.<run>.events` and `book` from the store, and K-9.1
  says they are updated in M-4. They change at M-4, with the outputs they
  read.

- **E-45 (K-2.3; the list and the order of a combo).** A bracketed list of
  terms is a term (`[t, ...]`, the constructor `[]`); a combo's first
  argument is the list of its legs, `leg(A, Q)` each, and its optional
  second the order. The checker requires two legs or more, on distinct
  instrument terms, each a signed delta in the class's unit (T); `exercise`
  in a class other than `option` is T. The kernel refuses to run a program
  that decides either (a configuration error naming the rule) until the
  options model executes them (M-7); the denial constraint's counting of
  a combo's legs is the executor's, with them.

- **E-55 (D-3.6; a writer's relation that changes shape).** A `put` adds
  its rows to the relation's current version from that source, as an
  ingest does. A run written again under its id (Definition A-4.1) writes
  the same rows, and duplicates are dropped. A `put` whose columns differ
  from the current version's starts the relation over: the new version
  holds that write's rows only, and the earlier versions stay readable by
  their ids. This is how a writer's outputs change shape between versions
  of the writer.

## Aggregates

- **E-27 (P-1.2, Proposition 1.1; the running sums' rebuild points).** "The
  rebuild points are a function of the window's row index." A rows window
  the kernel solves lazily from a bar walks back N anchored bars plus a
  checkpoint's span, not to the group's first row, so it cannot know a row's
  index since the group began; it does know the bar's index among the labels
  of the window's calendar. The running sums of a `rows(N)` window are
  therefore rebuilt from the window's rows at every `max(N, 8)`-th label of
  the window's calendar and advanced at every anchored label in between
  (`abt_agg::checkpoint_period`, `abt_agg::RowsWindow`), as the kernel did
  before the split. The data layer holds the same labels as `calendar_label`
  rows, so a pushed-down aggregate walks the same labels and has the same
  bits. The golden runs did not change.
- **E-28 (P-1.2; `two_pass`).** "The two-pass formulas (Chan, Golub and
  LeVeque)": the mode is the plain two-pass formula of that paper,
  Σ(x − x̄)(y − ȳ)/(n − 1) with the mean summed in order, without the
  corrected variant's second term; it is the form MW14's canonical returns
  are reproduced with (G1), and the corrected form would change their bits.
  `ols_beta` is `ols_slope` under the first formulation's name. Running sums
  answer `sum`, `mean`, `std`, `cov`, `corr`, `ols_slope` and `ols_r` over
  `rows(N)` windows; `var`, the order statistics and every other window are
  reduced by the two-pass formulas in both modes.

- **E-29 (A-D2, P-2.1 to P-2.4; where the transport lives).** `abt-wire`
  holds, besides the types and messages, the moving of frames: the
  in-process channel pair and the Unix socket that carry the same encoded
  frames, Arrow tables as schema, batch and end frames, and the request and
  response exchange (hello, correlation, pipelining, cancel, ping) every
  service and client speaks (`abt_wire::{transport, table, rpc}`). Each layer
  keeps its own dispatch, what a request does. Three copies of one exchange
  would be the duplication A-D2 rejects for types.

## Query layer

- **E-21 (Q-4.1, Q-4.4).** The starting set declares treatments the
  `Dependence` enum lacks. The enum gains `asymptotic`, `iid_robust` and
  `descriptive`; `coactivation` declares `clustered(time)` with a permutation
  within bars.
- **E-22 (Q-5.1).** The claim grammar's `INPUT` gains `grid NAME`, which
  `pbo on grid main` uses.
- **E-23 (Q-8).** The bias table keeps the forty rows of
  `docs/data-bundle.md`; `currency` is owned by the kernel and warned.
- **E-24 (Q-4.6, X-4).** NIST tolerances follow NIST's own difficulty
  classes (1e-12 lower, 1e-9 average, 1e-6 higher). Known answers that are
  simulated or hand-computed state their tolerance in the registry. NIST's
  own site is not reachable from the build container; the `.dat` files, which
  carry the certified values, are taken from a public mirror and checked in
  with their SHA-256.
- **E-25 (Q-2.2, A-D6, A-D19).** Section cross-references: feed statistics
  are Q-6, activation statistics Q-7, the test framework Q-4.
- **E-58 (Q-3.1, K-7.1, P-4; the metrics from the store).** The query
  layer computes a run's metrics from the tables the kernel wrote:
  - the NAV curve and the exposure from `book`;
  - the fills from `fills`, and the decisions from `decisions`;
  - the dropped decisions from `events`;
  - the cost, liquidity and funding summaries, the base capital and the
    report block's periods from `report`.

  Two tables gain fields for this:
  - `report` gains `periods_per_year` (the report block's) and
    `period_by_day` (whether a period is a calendar day rather than a
    decision bar). On a sessions calendar it gains `period_days`, the
    days of the root's sessions as each day's first instant. When the
    report calendar is neither the decision calendar, `days` nor a
    sessions calendar, it gains `period_note`. The kernel computes these
    once per run (`RunResult.periods`), from the program's report block
    and the binding's `session` rows, and `abt run` prints from them.
  - `fills` gains `multiplier`, the instrument's contract multiplier (1
    for an equity). A round trip's cash flow needs it.

  The metrics are the kernel study's, moved without change in substance.
  `tests/metrics_store.rs` shows them equal to the bit on every corpus
  program and both books. Covered are the return metrics, the trading
  metrics, the capacity at a 5 % impact drag, the period metrics and the
  sub-period Sharpe ratios.

  `abt metrics --run RUN (--data SOCKET | --store DIR) [--json]` prints
  them with the version of every table of the run they read. The front
  serves a store directory to the query layer in its own process. The
  surface's grid axes are the literal texts of the plan's values.
- **E-59 (Q-2.1 to Q-2.6, Q-3.1, A-4.1, P-3.4, P-3.5, P-4; plans as the
  query layer runs them).**
  - **Grammar.** A block's entries are separated by `;`, `,` or new lines
    outside brackets, a grid axis's values by commas, and `#` starts a
    comment. A template's parameter is replaced as a whole word, so
    `trailing:Y` with `Y = 2y`, not `trailing:Yy`. `program PATH` is
    relative to the plan file (a path contains `/` or ends in `.dsl`; any
    other word names a strategy among the sources). A window's ends are
    written back as the kernel prints a time.
  - **Defaults.** The objective is `sharpe` and the hold-out `none`. A
    block hold-out's seed is 1, the old study's. A claim is at 0.05,
    `after` and `warn`. The default steps are the grid (or one run) named
    `main`, the walk-forward `folds` when a scheme is declared, and the
    report `report`.
  - **What a step reads** must be a step of the right kind. `best(G)`
    names a grid. A step that takes one run (`run ... at`, `reveal`,
    `compare`, `ledger`, `activation`) reads a grid by its best point
    only. A `walk_forward` step needs a scheme and a `reveal` a hold-out.
    A cycle is refused.
  - **The normal form** is `study { program H(P) ... steps { ... } }`.
    Steps are named `s0, s1, ...` in topological order, ties broken by
    the step's kind in the order of Q-2.2's table, then its inputs'
    positions, then its text. Claims are named `c0, c1, ...` in the order
    of their text. The program is named by its hash, parameters are sorted
    by name, grid axes and values are kept as written, and the study's
    name, the report block and the report steps are left out. A plan
    renamed is the same plan.
  - **π** is `{plan: H(Π), step: the canonical name, point: {name: text},
    fold: {k, phase} | null}`. The run id is over (H(P), β, π with the
    request's truncation fields, mode) (E-56). The point is also in β, so
    every point has a binding of its own, over the same versions.
  - **The kernel's run request** (P-3.4) gains `horizon`, the truncation
    by time. The kernel turns it into the barrier's seq through the
    stream's index, since the query layer does not know the kernel's
    stream and so cannot name a `to_seq`. It also gains `plan_step`,
    `embargo_mask` (spans `[from, to)`; the kernel writes no ledger row at
    a bar inside one) and `observe`, relations observed beyond the
    program's own, outside H(P). `show binding` answers the binding a run
    would get now and its decision calendar's span, which is what a plan
    places its hold-out and folds in, without a run. `control release`
    stops holding a completed run. A request's `start` and `end` replace
    the report block's window only when given.
  - **The hold-out.** The sample is the decision calendar's bars at the
    plan's base point (the old study took every fact's time). A trailing
    hold-out truncates every run at its first bar less one. A block
    hold-out is a mask: on the kernel's ledgers, and on every metric the
    query layer computes.
  - **Reveals.** `abt plan run` runs every step but the reveals. `abt
    plan reveal PLAN STEP` runs one reveal from the committed point the
    plan's latest execution recorded in its outcomes, with no truncation
    and no mask, its metrics over the embargoed bars only.
  - **Relations.** `<plan>` in `query.<plan>.*` is H(Π) in hex.
    - `query.trials` has P-4's columns and `sharpe` (per period, for the
      DSR's variance), `fold`, `phase` and `status`. A run's trial id is
      its run id.
    - `query.<plan>.outcomes` is written whole at every execution (a
      reveal adds its row). It holds the step, run, kind, status, point,
      fold, phase, `best`, window, objective, and every metric of Q-3.1's
      return and trading groups.
    - `query.<plan>.executions` has a row per execution.
    - `query.plans` is written whole, one row per plan, with P-4's
      columns and the program hash and lineage. Its bindings are every
      run's, each pinned through the data layer by binding. `abt plan
      drop` unpins those no other plan holds; the plan's trials stay.
  - **Lineages** are rows of `query.lineages`: program hash, lineage
    (its root's H(P)), strategy, attachment `root` or `similar` with the
    member it resembles and the similarity, and the rules. The rules are
    the strategy's as the kernel's normal form renders them, compared by
    Jaccard at 0.8. The language has no `revises` header, so similarity
    alone attaches a revision. Disputes are rows of `query.disputes` and
    change no count.
  - **The count** (Proposition Q-2.2) is the distinct run ids among the
    `kernel.*.binding` of the lineage's members, plus the tests and
    claims judged. A run the same as an earlier one (same point, same
    binding, same step) is the same run, not another trial. The old
    `trials.jsonl` counted every invocation.
  - **Calmar** is the CAGR over the maximum drawdown, a metric by name.
  - **Commands.** The compiler's `abt plan` is `abt show plan`; `abt plan`
    is the query layer's. Claims, `test`, `feed` and `activation` steps
    are judged in M-5d.

- **E-60 (Q-4.4, Q-4.6; the time-series tests as built).**
  - **p-values.** `adf` and `engle_granger` use MacKinnon's 1994
    asymptotic formulas, for up to six series. His 1996 response surfaces
    may not be built into a program without his written consent.
    `engle_granger` with 7 to 12 series gives no p-value; MacKinnon's 2010
    critical values in its notes judge it.
  - **The MacKinnon 2010 table** is Schaffer's transcription in Stata's
    `egranger`. statsmodels' copy differs in four coefficients of the
    constant case (N = 2 at 1 %: β₂ −22.527 against −33.527; N = 3 and N =
    5 at 5 %: β₁ in the fifth digit; N = 7 at 10 %: β₂, β₃ −6.922 and
    67.721 against −5.104 and 27.877); the paper confirms egranger's where
    checked.
  - **The T of the critical values** is the regression's observations
    (n − 1 − p) for `adf`, as in statsmodels, and n − 1 for
    `engle_granger`, as in `egranger` and `coint`.
  - **`adf`'s lag** minimises T·ln(RSS/T) + 2k on the sample the largest
    order leaves. A tie goes to the smaller order, which is then refitted
    on its full sample. The default maximum is ⌊12 (n/100)^¼⌋, capped near
    half the sample. The parameters are `lags`, `max_lags`, and `trend`
    (`c` or `ct`).
  - **Defaults.**
    - `kpss` uses the Newey–West lag of Q-4.2; beyond Table 1 its p-value
      is the table's bound, with a note.
    - `ljung_box` uses m = min(10, ⌊n/5⌋) with m degrees of freedom.
    - `arch_lm` uses 12 lags, LM = (n − m)·R² on the squared demeaned
      series.
    - `variance_ratio` reads a level (`increments = true` for returns), q =
      2, z* two-sided.
    - `half_life` is −ln 2 / ln φ̂ (0 below φ̂ = 0, ∞ at 1 and above), its
      p-value the one-sided normal one of the Newey–West t of φ − 1,
      approximate under a unit root.
    - `johansen` has the unrestricted constant only (Osterwald-Lenum's
      Table 1), 2 to 11 series, p-values interpolated between its
      quantiles and bounded at 0.5 and 0.01, the effect the rank its
      sequential tests estimate.
  - **Shape.** `engle_granger` and `johansen` take one wide table, a
    column per series on one time axis; a caller pivots a long panel.
    `engle_granger`'s dependent series is `y`, and its residual regression
    has no constant.
  - **Effects.**
    - `ljung_box`: ρ̂₁.
    - `variance_ratio`: VR(q).
    - `arch_lm`: the sum of the lag coefficients.
    - `adf`: 1 + γ̂, and for `engle_granger` the same of its residuals.
    - `half_life`: φ̂.
    - `johansen`: the rank.
    - `kpss` has none.

    A text parameter's default is in its `doc`, since `Param::Text` is
    not a constant.
  - **Software references.** Lo and MacKinlay's CRSP example, Engle's UK
    inflation example, a published Q for Series A and Nelson and Plosser's
    Table 5 are not reachable. Each is replaced by another program's
    output, which the test's comment names.

- **E-61 (Q-4.2, Q-4.4, Q-4.6; the panel, activation and cost tests as
  built).**
  - **The Newey–West standard error of a mean** is √(LRV/(n − 1)), and
    its t is read against Student's t(n − 1). Zero lags give Fama and
    MacBeth's s/√T and the textbook paired t; L lags give Stata's `xtfmb`.
  - **`ic`.** The statistic is the Newey–West t and the effect the mean
    IC, every bar weighing the same. Missing values are dropped per
    entity. A bar with fewer than `min_entities` (3) entities or a
    constant column is left out. `horizon` raises the default lags to at
    least horizon − 1.
  - **`fama_macbeth`.** The statistic is the t of the premium `factor`
    (the only regressor by default). The intercept is `const`. A bar with
    no more rows than coefficients, or a singular design, leaves the
    series.
  - **`coactivation`.**
    - Phi is both the statistic and the effect; information is in bits.
    - A missing value is an instance not attempted by both rules.
    - The p-value doubles the smaller tail of n₁₁ under permutation
      within bars, each tail (1 + k)/(1 + reps), with reps 2,000.
    - The interval is a stationary bootstrap over bars, given from ten
      bars on.
  - **`effective_rules`** is (Σλ)²/Σλ² of the activation correlations.
    Constant rules are left out with a note.
  - **`cost_realism`** tests realised − modelled slippage (positive: the
    model understates costs). Fills are taken in (time, entity) order. A
    nonzero constant difference is a precondition failure.
  - **Q-4.6's permutation invariance for iid tests**: none of the five is
    iid, so each is tested for the order invariance that applies to it.
    `Table::bars` refuses an entity twice in one bar.
  - **Accuracy.** `linalg::least_squares` refines its solution through
    the augmented system (Björck 1967), with residuals in twice the
    working precision. Pontius and Wampler5 met their NIST class only
    with it. Filip is bound at about 7.2 digits by the rounding of its
    own data, within its class.

- **E-62 (Q-4.2, Q-4.4, Q-4.6; the distribution and resampling tests as
  built).**
  - **The bootstrap tests impose their null.** `jarque_bera` re-centres
    the moments (each resample's statistic on S* − S and K* − K).
    `ks_normal` re-centres the empirical process (Babu and Rao 2004).
    - Their block, and `bootstrap_sharpe`'s, is the larger of the
      Politis–White blocks of the series and of its squared deviations.
    - A p-value is (1 + #{T* ≥ T})/(1 + R), from eight observations on.
    - On short series they are somewhat liberal: `jarque_bera` rejects 7
      % of normal series of 250 at 5 %, `ks_normal` 12 % of 200 at 10 %.
      Under dependence they are far nearer their size than the iid tests.
      The suite asserts that comparison, not the nominal size.
  - **`ks_normal`** uses the n − 1 standard deviation. Its known answer is
    Dallal and Wilkinson's corrected table, since Lilliefors' 1967 table is
    off in the third decimal. The table's 0.001 column, which a 10⁶-sample
    simulation puts up to 0.002 off, is left out.
  - **`hill_tail`.**
    - The tail is the losses by default (`upper` and `absolute` on
      request), and α₀ is 4.
    - k is Hall's bootstrap as R's `tea::hall` does it: ε 0.955, a pilot
      ⌊2√n⌋, 1,000 replications, the scaling (n/n₁)^(2/3).
    - p = P(Gamma(k, 1) ≥ α₀kH), with the exact interval given k.
    - It needs fifty tail values unless k is given.
  - **`psr`, `dsr`, `min_trl`.**
    - `benchmark` and `var_trials` are per period, and the p-value is
      1 − PSR (1 − DSR).
    - The effects are SR − benchmark, SR − SR₀ and n − MinTRL.
    - `min_trl` refuses a Sharpe ratio not above the benchmark.
  - **`pbo`** reports no p-value; a claim reads its statistic (`expect
    statistic < 0.5`). Its input is a column per trial in time order. Its
    partitions are 16, falling back to 8, even and at most 20, and its
    effect is the mean logit.
  - **Simple tests.**
    - `bootstrap_sharpe`: a percentile interval at 0.95 with 252 periods
      a year, p = (1 + #{SR* ≤ 0})/(1 + R).
    - `nw_mean`: a two-sided normal p-value, the lag rule capped at
      n − 1.
  - **`wfe`.**
    - Its input is a table of `in_sample` and `out_of_sample` annualised
      returns per fold, and WFE is mean over mean, the old study's.
    - Its p-value is Fisher's sign-flip test on out − e₀·in (e₀ 0.5):
      exact to twenty folds, drawn beyond.
  - **A constant series** has central moments of exactly zero.
  - **Sources.** NIST's univariate files are Apache Commons Math's
    copies. The Bailey–López de Prado and textbook figures, whose papers
    and book were not reachable, come from independent reproductions that
    agree, and are checked to one unit of their last printed digit (half
    a unit for the textbook).


## Layout

- **E-26 (M-0).** Integration tests that drive the `abt` binary live in
  `crates/abt/tests/`, since a binary's path is visible only to its own
  package's tests; suites that span layers live in a test-only crate
  `crates/abt-tests` (with the shared fixtures as its library); each layer's
  own suites live in its crate.
