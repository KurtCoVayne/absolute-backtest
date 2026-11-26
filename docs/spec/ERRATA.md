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
  translation-validation golden files (`crates/abt-tests/tests/tv_golden.rs`,
  `crates/abt-tests/tests/golden/*.txt`: every corpus-v2 strategy and both
  company books on the seeded synthetic markets, decisions, fills, book and
  final state rendered exactly; since M-2, by the fold over the stream of a
  data service holding the market, the batch driver being deleted) and by G6
  (`crates/abt/tests/reference.rs`, the pandas reference, E-79). The owner
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

- **E-71 (K-2.3, K-2.4, K-2.5, K-5.2, K-5.5, K-5.7, K-5.8; the option
  model and the functions of F).**
  - **Decisions.** A `combo` is a decision on its first leg's instrument
    that carries all of its legs. F-5 counts it as a decision on each leg,
    and so does `silence flat`. `exercise(A, Q)` is sent as an order and
    carried out at the decision bar's close. That is after the bar's NAV
    record, so `inv.nav` skips such a bar, as it skips a bar with a margin
    call's fills. It is dropped, naming why, under `early_long never`, for a
    European option, or for more contracts than the book holds long.
  - **The functions of F (K-2.5)** are `bs_*`, `b76_*`, `am_price` and
    `implied_vol`.
    - τ is a Duration in years (ACT/365, a month a twelfth of 365 days) or a
      Scalar already in years; the right and the style are labels.
    - A value outside a function's domain leaves the instance stuck (⊥),
      with `implied_vol` outside the no-arbitrage bounds among them.
    - Black-76 is BSM with S = F and q = r, which is exact.
    - Bjerksund–Stensland 2002: the paper's trigger is capped at h ≤ 0 with
      x ≤ X. Where b < 0 and |b|√τ > 2σ it would otherwise fall below the
      strike. American Greeks are finite differences: a five-point stencil
      in S, with σ, τ and r bumped by 1e-4.
    - The European implied volatility is Jäckel's 2015 algorithm: the
      four-branch rational guess, then Householder(3). It stops at 1e-13
      relative error in price and refuses above 1e-12. The American one is
      Newton on the log price, falling back to bisection.
  - **Known answers** (`crates/abt-kernel/tests/pricing.rs`, fixtures
    written by the scripts beside them):
    - Haug's worked examples to their printed decimals. His vega of 18.9358
      for the delta example exceeds the bound S·e^{−qT}·√T·φ(0) = 18.5028,
      so the test checks QuantLib's 18.5027 and asserts the bound.
    - QuantLib 1.44: the European engine's prices and Greeks to 1e-12. Its
      theta loses about ε/τ, so the theta comparison is widened by that
      amount.
    - QuantLib's QdFp American engine, 1764 cases. BS2002 is a lower bound
      on it (never above by more than 8.5e-14) and falls short by at most
      1.18 % at σ√T ≤ 0.75, rising to 5.97 % above 1.5.
    - TF Quant Finance's documented BS2002 example.
    - mpmath at 40 digits: the normal and bivariate normal, and BS2002's own
      formula.
    - py_lets_be_rational, to 1e-12.
    - Implied-volatility round trips to 1e-10 on dense grids.
  - **Marks (K-5.5.2)** are taken from the quote keyed at the bar.
    - `mid` is (bid + ask)/2; a quote without a positive bid marks at half
      its ask, `one_sided`.
    - A bar without a quote marks at the last mark, `stale`; an earlier
      bar's quote is not the bar's.
    - `model` is the model's price at the IV of the latest `greeks` row
      (`greeks data`), else at the IV that the contract's latest quoted mid
      implies.
  - **Fills (K-5.5.3).**
    - `touch`: a buy needs a positive ask and a sell a positive bid.
    - `mid`: needs both.
    - No price is below zero. `spread` is (price − mid) × quantity ×
      multiplier, so a fill better than the mid is a negative cost.
    - `commission per_contract P` is charged on contracts (options,
      futures) where the catalog states none.
    - An option has no slippage, impact or borrow (K-5.7).
    - Its limit order fills against its quote: a buy when its fill price is
      at or below the level, a sell at or above.
  - **Combos (K-5.5.3, K-5.8).**
    - The net is Σ qᵢ·pᵢ over the greatest common divisor of the whole
      |qᵢ| (the smallest |qᵢ| when one is fractional): the price per unit of
      the combo, a debit positive and a credit negative. `limit(P)` fills
      when the net is at or below P.
    - A combo is all or none against the participation cap and the
      leverage rule, as well as the quotes.
    - A working combo is keyed by its first leg; a decision on any leg
      replaces it.
    - The limits (K-3.5) size a combo leg by leg and reject it whole. A
      `clamp` rejects it, since it cannot be scaled.
  - **Expiry (K-5.5.4).**
    - An option is settled at its expiry day's first decision bar (AM,
      against the underlying's open there) or its last (PM, against the
      underlying's last price by the day's end). A day without a bar moves
      it to the next bar.
    - In the money means beyond `atm_tolerance · K`. A physical delivery is
      a fill of the underlying at the strike, with no commission and with
      the decided exercise's order id when there is one.
    - An option without an underlying price at expiry expires worthless,
      warned.
    - τ runs from the end of the bar's day to the expiry instant: the end
      of the expiry day for PM, its start for AM. At its expiry close a PM
      option has none left, an AM option none from the close before.
  - **Early assignment.** Only an American short in the money beyond the
    tolerance is assigned early.
    - `early_short when extrinsic < DIVIDEND`: a call on the decision bar
      before an ex-date (a dividend announced by the bar, going ex after it
      and by the next decision bar) when its extrinsic value is below the
      dividend; a put on any bar when its extrinsic value is below
      K·(1 − e^{−rτ}), with r the financing block's cash rate.
    - `when extrinsic < X`: the call on the same bar against X, the put on
      any bar against X.
  - **Adjustments (K-5.5.5)** apply at the underlying's ex-date to the
    options the book holds. A contract not held keeps the registry's terms.
    - A whole split n:1 (an integer n ≥ 2) multiplies the contracts by n and
      divides the strike by n.
    - Any other ratio r, a reverse split included, takes the multiplier to
      multiplier · r and the strike to strike / r.
    - A special dividend lowers the strike by its amount.
    - The average cost and the last mark are divided by the unit price's
      divisor (n or r). The adjusted terms are the executor's state,
      restored with it.
  - **A delisted underlying** forces its options' expiry in cash at
    intrinsic against its last price (`cause delisted`), warned.
  - **Greeks (K-5.5.6).** `greeks data` reads the binding's row keyed at the
    bar, as received by the barrier. `greeks model` is the model's at the
    IV the mark implies: BSM's closed forms for a European, BS2002's
    differences for an American. Where the IV is ⊥ the relation has no
    tuple for the contract, so a rule reading it does not fire there rather
    than halting the run.
  - **The class's kernel relations (K-2.4).** At each decision bar the
    executor writes `chain(U, T, A)`, by the chain need's filters
    (`abt_wire::chain::admits` with the bar's open interest), and `spec`
    and `greeks` for every contract in a chain or held. `spec` names the
    right, style and settlement by the class's words.
  - **Events (K-5.2).** `exercise` and `assignment` carry the contracts, the
    position left, the underlying's price, the intrinsic value, cash or
    physical, and the cause (`expiry`, `early`, `decision`, `delisted`).
    `option_expiry` carries the contracts and the intrinsic value.
    `adjustment` carries the terms before and after, the position and the
    price divisor. A fill's `spread` is the events table's column of that
    name.
  - **Nulls.** A null number in a stored relation reads as NaN: ORATS's
    last trade, and a put's theta and rho. The executor reads a quote's NaN
    as absent. A comparison over NaN is false, and a decision whose amount
    NaN reaches halts as before. A null of any other type (an entity, a
    label, a time) still refuses the tuple.
  - **The data side.**
    - An ORATS contract's ticker is its OCC symbol, from its first row and
      open-ended (the symbol carries the expiry, so it names no later
      contract). A program names a contract by it.
    - The option class's `quote` is admitted by a chain need, and
      `special_dividend` by `need actions` in an option program only (an
      equity program's relations, and so its hash, are unchanged).
  - **The golden hashes.** E-8 requires `settlement` of a futures program,
    so the preset `futures_fixed_base` states `settlement none`: the fully
    paid book R8L has always had. R8L's program hash changes, blessed in
    `crates/abt-tests/tests/golden/program_hashes.tsv`; its golden runs do
    not. A library's probe program states its class's own fields.

- **E-72 (K-2.4, K-5.4, K-5.6, D-7.1, D-7.7; the futures model and the
  margin models).**
  - **Optional needs (D-7.1).** A `Need` gains `optional`: a relation of
    an optional need that neither the store nor a source covers is left
    out of the binding, with a note, not refused. The kernel asks
    optionally for `settlement` (a futures program under `settlement
    daily`) and `margin` (`margin published`); the checker admits them
    there and nowhere else, so no other program's demand or hash changes.
  - **Daily settlement (K-5.4, E-10).**
    - Each future held has a reference price: the fill's that opened it,
      then each settlement's.
    - A fill moves −q · (price − reference) · point value, plus its costs.
    - Each session's close moves Q · (S − reference) · point value to cash
      and makes S the reference: a daily bar's close, or the bar whose
      barrier passes the root's session end. S is the binding's
      `settlement` row at the bar, else the close (warned, `at_close`).
    - The NAV counts Q · (mark − reference) · point value. So do sizing,
      the leverage check, the limits' NAV and `inv.nav`.
    - `settlement none` books a future fully paid, as before.
  - **Rolls.** A `roll` is recorded at the bar where the series' contract
    changes, with a position on the root.
    - Both contracts are filled at the series' price of the bar before,
      where the adapter measures the gap: the old less its adjustment,
      the new less its own. So the bar's P&L over the two contracts is the
      series' change, and the change of adjustment is not P&L (K-5.4).
    - Each fill takes the root's commission per contract, and only the
      commissions move cash.
    - The position stays on the root.
  - **Contract expiry.** A contract held directly (one with
    `future_spec`) is settled on its last trade's day (its expiry's when
    none is stated) and closed: at its `settlement` row, else its close.
    Under `settlement none` it closes at the close, since only `settlement
    daily` reads settlement prices. A physical contract is not delivered,
    warned.
  - **`contract(A, @T, Root, Expiry, PointValue)`**: A is the contract the
    series Root holds at T, both entities.
  - **`exposure`** of K-2.4 is not added: the book module's `exposure(@T,
    G)` has the name. The book record carries the gross, the net and the
    leverage.
  - **The margin models (K-5.6)** price the book at every decision bar
    when the execution block lists any.
    - Holdings: a stock, a future (its point value), an option (its terms,
      its expiry instant).
    - An option's underlying is a broad-based index when the option
      settles in cash, an equity otherwise.
    - An option is repriced at the volatility its mark implies.
    - SPAN is skipped: no risk arrays are bound.
    - `margin_req` is written for every program; without models it is the
      leverage model's (gross over the multiple, the maintenance fraction
      of gross). `margin_by` is written where models are listed. The book
      record's margin columns are the totals.
    - A margin call is the equity below the models' maintenance. `liquidate`
      sells whole units at the bar's marks, in the order of maintenance per
      unit (largest first), until the shortfall net of each fill's
      commission is covered; `forced_liquidation` names the fills.
    - `approx` is warned once, counting the bars it priced, where K-9.3
      says every bar: one line with the count carries the same fact. A
      model passed over is warned, at most 20 notes.
    - The leverage field still governs orders; the models judge the margin
      call.
  - **A demand file** (`abt data resolve --demand`) may leave out
    `optional`: a need without it is required, as a FlatBuffers reader
    takes the field's absence.
  - **Databento's statistics (D-7.7)** become `future.settlement` and
    `future.open_interest`, both for the contract the series holds in the
    session.
    - Settlement prices (`StatType` 3) are back-adjusted as the bars;
      open interest (9) is kept as published.
    - A row is keyed at the session's last close and received when
      published. The last one received is kept; a deleted statistic is
      left out.

- **E-75 (06-performance 3.2, K-4; the set-at-a-time evaluator).** The
  kernel solves a rule's body in one of two ways.
  - **The tuple evaluator** steps one environment at a time, as before:
    every instance a `Vec` of its slots.
  - **The set evaluator** solves over blocks: the bindings entering a
    literal are one buffer of rows, a row a slice of the rule's slots, and
    the literal writes the rows leaving it to the next block. The stored,
    derived and as-of atoms, the tests, assignments, cases, builtins and
    windows write their rows in place; no instance allocates.
  - **The same tuples in the same order.** The set evaluator takes the
    literals in the plan's order and the rows in the order the tuple
    evaluator takes its environments, each row's solutions in the order it
    yields them. An instance that meets a partial operation is stuck in
    both (Kleene conjunction, V-5), and the rows it pushed before the
    failure are withdrawn. So the two derive the same tuples in the same
    order and halt alike. `tests/eval_set.rs` holds them equal on every
    corpus program and both books: the digest of the run and the ledger of
    every derived relation at every bar.
  - **The set evaluator is the default.** `--evaluator tuple` (and
    `ExecConfig.set_eval`) selects the other; it is a test switch, outside
    the configuration's identity.
  - **The window cache shares more.** A window's rows are cached a bar at a
    time for each group of the outer bindings its conjunction reads (E-46).
    A literal that also read the window's base time made every call's rows
    its own, so the cache was skipped: `dividend_at`'s `Ex = T` (the
    dividends going ex at the bar) solved its whole year at every call.
    Such a literal is now a test, applied at each call to the cached rows,
    when all of these hold:
    - it is a comparison, or an assignment to a variable that the call or
      an atom of the conjunction binds;
    - its expressions are total: variables, parameters, literals, negation
      and arithmetic other than division;
    - everything it reads is bound by the call or by the rows.

    The rest of the conjunction is solved and cached as before. A row stuck
    in the cached part may be one the test excludes (Kleene conjunction,
    V-5). So a solve that fails there, or a test that cannot be evaluated,
    is left to the whole solve; `tests/fold_windows.rs` holds such a case.
    The rows are read in E-46's order (the bars in the window's time order,
    a bar's rows in the order of their own bindings) straight into the
    aggregate's columns: no environment per row and no sort of the window.
    What does not depend on the call (the group's variables, the cached
    conjunction in its order, the tests) is worked out on the literal's
    first call, not at every one. `momentum_12_1` at 100 names over 2,520
    days solves 0.98M window bars instead of 62.7M.
  - **What 3.2 has that this does not.** The rows hold `Value`s (16 B),
    not typed columns. A generator binds by the stored block's index per
    row, not by a merge join. Negation tests the atom per row, not a
    bitmap. The nested solves of aggregates and cases stay one environment
    at a time. The measured gain is in docs/assessment.md.

- **E-76 (06-performance 3.1, K-4.2; the kernel's stored tables).**
  - **A revision is searched for among earlier instants only.** A tuple of
    the stream replaces the row of its key that it revises (K-4.2), and the
    check searched the key's whole block for every tuple: N² comparisons
    for a bar of N names. A store now keeps the run of tuples the current
    receipt instant has brought to each key, with their identities hashed.
    A tuple searches the rows received at earlier instants, and the run's
    own only when the run has brought its identity already: two versions
    of one fact bound together. Another writer's insert, or a lookup's lazy
    sort, ends the run.
  - **A key's tuples are one buffer.** A block is its rows row-major
    (`kernel/rows.rs`): a tuple has no allocation of its own and is read as
    a slice. A bar's block is sized like the bar before and shrunk to its
    rows when the stream moves on.
  - **The command line keeps one copy of the data.** The dataset it loaded
    is dropped once the data service holds it, unless `--dump` or
    `--verify-causality` reads it after the run.
  - **What 3.1 has that this does not.** A value is a `Value` (16 B), not a
    typed column: a key column `u32`, an `f64` per price. The key's time is
    kept in every row, and there is no per-name index of the latest row
    (`latest` walks back over the bars). The measurements are in
    docs/assessment.md.

- **E-77 (06-performance 3.2 to 3.4, M-8; what M-8 leaves).** M-8 builds
  the resident layer and the aggregate pass (E-74), the set evaluator and
  the window cache (E-75), and the stored tables' blocks (E-76).
  `scripts/bench.sh` measures A-7.3's targets (docs/assessment.md). Not
  built:
  - **Entity-parallel strata (3.2, Proposition 3.1).** The evaluator's
    state is one kernel's: the memo, the window groups and the lazily
    sorted blocks, all behind `&mut`. Strata in parallel need that state
    split by entity. Each relation's tuples must then merge back in the
    order one thread derives them (rules in order, each rule's solutions
    in order), with late tuples invalidating and checkpoints holding every
    part. That is a rewrite of who owns what, not a step of M-8. The
    data layer's aggregate pass, the stream and a plan's runs are parallel
    already, each bit-identical across thread counts (G3).
  - **Stream projection (3.3).** The stream carries every primitive
    relation the binding names, read or not: a program that needs bars
    gets `open`, `high` and `low` though only the executor's order types
    read them.
  - **Typed columns (3.1, E-76),** bump arenas per barrier, and
    copy-on-write checkpoints.

- **E-78 (M-9; the first formulation's commands retired).**
  - **Gone:**
    - `abt synth`, now `abt data ingest --adapter synthetic`;
    - `abt bundle build` and `abt bundle test`, the bundle now an import
      format, `abt data ingest --adapter bundle_v2`;
    - the `explain`, `why-not` and `materialise` stubs, the query language
      having replaced them at M-4;
    - `--kernel`.
  - **A run reads a store.** `--data STORE` serves a store directory in the
    process, writing the run's outputs there, and `--data SOCKET` reads a
    data service. A bundle or a directory of Parquet files given to
    `--data` is refused, naming the import that reads it; `--bundle` is
    gone. `--verify-causality` re-runs the program over the run's own data
    service, its stream truncated at each sampled bar.
  - **Kept as libraries.** The bundle module and the Parquet loaders are
    what `bundle_v2` imports with. `test_bundle` remains the review of a
    legacy bundle: one that passed imports `reviewed = complete`.
  - **Not deleted.** `ExecConfig` is the kernel's form of the resolved
    `execution` block. No option sets it, which is what M-9 asks; deleting
    the type would rename it, not retire anything.
- **E-79 (M-9; the store is the dump).** `abt run --dump DIR` and
  `dump.rs` are gone. G6 reads a run back from the store it ran over.
  - **What is read.** `reference/store.py` reads through `abt data query
    --store STORE ... --parquet`, as any reader of the store does:
    - the configuration: the binding's `configuration`, the resolved
      `execution` block;
    - the data: each relation the reference replays, at the version the
      binding names, each instrument named by its `vendor_id` key as the
      kernel names it;
    - the decisions, the events (fills, dropped decisions, actions), the
      book, the holdings and the report, from `kernel.<run>.*`.
  - **The engine is unchanged.** `store.py` writes what it read in the
    layout `engine.py` replays. `diff.py` takes `STORE RUN [--abt PATH]`
    and exports to a temporary directory before comparing.
  - **The price and volume relations** are the execution block's, else
    `close` and `volume` where the program reads them, as the kernel takes
    them. A derived price relation is not stored, so the reference replays
    only runs that fill at a primitive one, as the dump did.
  - **Where G6 runs.** It moves to `crates/abt/tests/reference.rs`, the
    package that builds the `abt` binary the store is read with. Each case
    runs in process over a store of its own and writes its outputs there.
  - **What stays.** `--nav FILE` still writes the book at every bar as
    Parquet. The command line now always drops its dataset once the data
    service holds it (E-76's exception for `--dump` is gone).
- **E-80 (M-9; the documents).**
  - **The README** is written around the three programs and the
    documents. The data layer, the kernel and the query layer each have a
    section with their commands, followed by the command line, the gates as
    checked here, the layout and the status. Its two quick starts run as
    written: in one process over a store, and as three processes over
    sockets.
  - **`docs/observability.md` is folded into the README's section on
    K-7's commands** ("Observing a run"), as M-9 asks, updated to the query
    language's forms. The specification's documents are not edited (no
    step has edited them; these errata amend them). The retired forms are
    E-53's, and the document is in the history before this entry.
  - **Status notes.** `docs/data-bundle.md` keeps its notes as the record
    of the first formulation's crate, each ending with where its part
    lives now. `docs/language-v2.md` gains a status note on how section
    11's steps were carried out.
  - **The studies.** `experiments/llm-usability/` and
    `experiments/llm-observability/` are records, with a note that their
    commands no longer exist and the commits at which their scripts run.
    Their rerun on the second formulation (A-7.2) is the owner's.
  - **`ledger R into NAME` over a store.** It writes to the store over
    `--data STORE`, as over a socket: a run over a store lasts (E-78).
    Before, it was refused unless a socket was named. The query reuses its
    run's data service rather than opening a second one over the same
    directory. The usage says the same of checkpoints and `--resume`.
- **E-81 (P-3.5, Q-9, Definition A-2.1, M-9's gate; the query service and
  the three processes).**
  - **The query service.** `abt query serve [--socket PATH] (--data SOCKET
    | --store DIR) [--kernel SOCKET... | --kernels N]` serves the query
    layer's commands (P-3.5) and runs each against the data and kernel
    services it was started with. P-3.5's requests are one message,
    `QueryLayerRequest`, whose `op` names the command (plan, lineage,
    dispute, metrics, test, claim, feed, activation). It carries:
    - the command's words as its command line writes them, less the
      transport options;
    - what the command reads from files: a plan's text, and the program a
      plan or a lineage names (its sources and strategy). The service
      reads no file of the client's.
  - **The answer** is what the command printed, its notes and its exit
    status (`QueryLayerResponse`, in parts of at most 256 KiB, the last one
    `done`). It is the command's text, or its JSON under `--json`, rather
    than P-3.5's records per request, so a command answers the same
    through the service as in the client's process. A plan's progress is
    not streamed: the answer comes when the plan has run.
  - **The client.** A query-layer command given `--query SOCKET` is asked
    of the service. The front reads the files the command names and sends
    them with it. The command prints the same and exits the same, but for
    the instant a plan's execution records.
  - **The binaries.** Definition 2.1's `abt-data` and `abt-query` are not
    binaries of their own. Their commands and `serve` modes are the
    front's (`abt data ...`, `abt query serve`, `abt plan ...`), as `abt
    kernel serve` is beside `abt-kernel`. The data layer's synthetic and
    bundle imports use the kernel crate's generators and bundle reader,
    which `abt-data` may not depend on. Each layer's library is its crate,
    and each layer's service runs as a process of its own, which is what
    the gates need.
  - **The three-process deployment** (`crates/abt/tests/deployment.rs`)
    starts `abt data serve`, `abt kernel serve` and `abt query serve`. It
    runs a plan through the query service and a program through the kernel
    service, and the same commands in one process over a second store of
    the same market. Then:
    - the plan's execution is the same, but for its instant `at`;
    - the runs have the same ids;
    - every run's outputs hold the same rows. Those keyed by time and an
      entity or the run's seq come out byte for byte. Those keyed by a
      label come out in the order of each store's label codes (E-32),
      which two stores written in a different interleaving need not share.
  - **The suite over sockets.** `ABT_TRANSPORT=socket` connects the layers
    of one process over Unix sockets rather than in-process channels: the
    front's data services over a dataset or a store directory, its kernel
    services, and the shared fixtures'. Each connection gets a socket of its
    own, removed once connected. `ABT_TRANSPORT=socket cargo test
    --workspace` passes here. A CI job running it is the owner's to add,
    since this work does not edit `.github/workflows/`. The suites that test
    a transport (`determinism.rs`, the services' own tests) name theirs.

- **E-82 (K-6.1, K-6.4, K-6.5, Proposition A-4.3, M-10's gate; the live
  interface).**
  - **The broker interface** (`crates/abt-kernel/src/live/broker.rs`) is
    K-6.4's `Broker`: `send`, `cancel`, `replace`, `events`, `snapshot`.
    `events` returns a vector rather than an `impl Iterator`, which keeps
    the trait an object (`Box<dyn Broker>`). An `OrderRequest` carries:
    - the run's order id (K-5.2) as the client's id;
    - the instrument by its binding's name, and the signed quantity;
    - the order as decided, and the broker's time in force for its kind,
      from the deployment's `tif_map`;
    - the legs (always empty, see below), the multiplier, the price the
      order was sized at, and the destination.
  - **Polling, not `on_event`.** K-6.4 delivers fills through `on_event`,
    as stream events the data layer records (D-5.4). Here the executor
    drains `Broker::events` at every fill step, before the bar's decisions
    (K-4.2's `fill(ℓ)`), and the `Executor` trait keeps its methods. A
    broker's events are therefore not in the run's stream, and a
    live-broker run does not replay from its recording alone. Its log holds
    every fill with the broker's order id: `fills.source` is `broker`, and
    `events.detail` carries `broker_order`. Recording the broker's events
    in the stream comes with the adapter (A-D27).
  - **`BrokerExecutor`** (`live/executor.rs`) wraps `SimExecutor` and uses
    its book.
    - **What the simulation still does:** the bar's own events (corporate
      actions, delistings, expiries and assignments, a future's roll and
      settlement), the marks, the margin and the bar's records. These are
      `open_actions` and `open_marks`, the two halves `open_bar` is now
      split into.
    - **`on_decisions`** records the orders and applies the limits (K-3.5)
      as the simulation does. It sizes each decision at the decision bar's
      mark (a target against the book, a weight against the base), sends
      the reductions first, and cancels or replaces the order working on
      the instrument.
    - **Leverage** is estimated before sending, at the bar's marks, with
      the working orders and this order at the price it was sized at.
      `inv.gross` (K-3.3) is not checked on a broker's fills: their prices
      are not the kernel's, and a book sized at one bar's marks is marked
      at the next.
    - **`fill`** applies the broker's reports:
      - fills at the broker's price and commission, with no simulated
        cost;
      - a partial fill, its remainder `working` (a new `Fate`);
      - a reject as `rejected`, with the broker's reason;
      - an expiry as `expired_order`;
      - a cancel the broker made as `dropped`.
    - **`finish`** cancels what still works at the broker (`dropped`,
      "the run ended"), then takes the last reports.
    - **Not routed.** Combos and exercises are dropped, with the reason:
      the interface sends single orders, and NewOrderMultileg is the
      adapter's. The trades the instrument models make at a bar's open (a
      roll, a margin call's liquidation, an expiry) stay simulated;
      reconciliation finds them if the broker disagrees. Funding (interest,
      borrow) is the broker's, and the book takes it under `adopt`.
  - **The lifecycle** (`live/lifecycle.rs`) has the states sent, new,
    partial, filled, cancelled, rejected and expired. From the run's side,
    an order superseded by a later decision, or cancelled at the end, is
    `replaced`. A report the lifecycle does not admit is ignored and
    warned: a fill of a filled order, or a report of an order the run did
    not send. One order works per instrument (`inv.working_orders`).
  - **Reconciliation** (`live/reconcile.rs`) runs at a session's open (the
    first bar of a calendar day) and at the bar after a reconnect, when the
    deployment lists the event. It runs after the bar's own events and
    before its marks.
    - A difference is a position apart by more than a billionth of a unit,
      or cash apart by more than `cash_tolerance` (default a cent). It is
      a `reconciliation` event carrying both sides.
    - `halt` halts the run (`(reconciliation)`). `adopt` makes the book the
      broker's account: the positions, and the cash through the event's
      cash flow. `warn` keeps the book and warns once, with the count.
    - A position the broker holds in an instrument the run does not know
      cannot be adopted: the run halts.
  - **The deployment document** (`live/deployment.rs`) gives K-6.5's six
    lines a concrete grammar:
    - fields in any order, separated by white space, commas or semicolons;
      `#` comments; quoted words;
    - `routing` inline or braced; `halt_all` sources separated by `|`;
    - `reconcile POLICY [at E, E] [cash_tolerance X]`; durations in `ms`,
      `s`, `min` or `h`;
    - `account` and `broker` required.

    Its hash is BLAKE3 over the RFC 8785 form of what it parses to, so
    neither layout nor comments change it. It is outside the run's id (the
    mode is in it, A-4.1), and recorded as `binding.deployment_hash`.
    - `deadline base` is the live deadline (K-6.3) when `--deadline` gives
      none.
    - `deadline session` is how long, in the stream's time, the broker may
      stay down before the run halts.
    - `recording stream NAME` is the session a live-broker run reads when
      `--session` names none. When both are given they must agree.
  - **A rehearsal without a session.** `RunMode::LiveBroker { session:
    None }` runs over the stream a replay-sim run would read, or over a
    recorded run's stream (`replay_of`). This is what the gate runs. With a
    session the run is live: K-6.2's clock and the late policy.
  - **`broker mock`.** This build has one broker, `MockBroker`
    (`live/mock.rs`): an account in memory, and a policy saying what
    becomes of each working order whenever the broker is asked for its
    events.
    - By default an order fills in full at the price it was sized at,
      without commission. A limit or a stop the reference does not reach
      works until cancelled.
    - Its account does not carry the data's corporate actions, so a
      rehearsal over a dividend or a split finds it apart at the next
      session's open.
    - Any other broker is refused, naming the adapter it lacks.
  - **The kill switch** (`live/control.rs`) is checked at every decision
    barrier.
    - `halt_all` comes from a control request (the kernel service's
      `control halt` on a run with a deployment), from the deployment's
      file appearing, or from its signal (SIGHUP, SIGINT, SIGQUIT, SIGTERM,
      SIGUSR1, SIGUSR2; the handler only counts). It is a `control` event
      at the end of the barrier, then the run halts (`(kill switch)`).
    - `flatten_all` (`control flatten`) replaces the next decision bar's
      decisions with a closing decision on every position held and every
      instrument with an order working.
      - The program's decisions at that bar are `dropped` before they are
        orders. `post.decisions` (K-3.4) now counts such drops, which also
        accounts for `late reject`'s.
      - The closing decisions are the control's: rule `NO_RULE`, labelled
        `(control)`. `inv.one_decision` holds of the program's decisions.
        They are logged, limited and sent like any decision.
    - A run without a deployment refuses `flatten`: a replay's decisions
      are its program's.
  - **The commands.**
    - `abt run --mode live-broker --deployment FILE [--session S]`.
    - `abt kernel run` takes `--mode`, `--session` and `--deployment`. The
      document's text travels in `KernelRequest.deployment`, beside `mode`
      and `session` (P-3.4).
    - `abt kernel control --run RUN flatten`.

    `kernel::executor_for` picks the executor a run's mode takes. A live
    run the kernel service holds answers queries over its final state, but
    refuses a `?` over more than one atom: V-8.1 answers that with a run of
    the program holding it as a rule, and a live run does not run again. A broker
    without an adapter refuses the run (`run refused: ...`, exit 1).
  - **The gate** is `crates/abt-tests/tests/live_mock.rs`, with
    `kernel_service.rs` and `crates/abt/tests/cli.rs`.
    - **Proposition A-4.3**, twice:
      - six corpus programs, whose decisions read the book no further than
        `held` and `flat`, decide the same events in replay-sim and in a
        live-broker rehearsal of the same store's stream;
      - a minute program decides the same in a live-sim run of a recorded
        session and in a live-broker rehearsal of the recording.

      The fills are the mock's (another price, no commission, partial under
      a policy), and each carries the broker's id.
    - **The lifecycle's transitions**, admitted and refused. Through runs:
      new, partial and filled; rejected; expired; cancelled by the broker;
      replaced by the next decision; cancelled at the end; a report it does
      not admit, ignored.
    - **Reconciliation:**
      - `halt` at the session's open;
      - `adopt`, of a position and of cash;
      - `warn` at every open;
      - a reconnect after a drop, with the orders refused while the session
        was down;
      - the session deadline;
      - an unknown instrument under `adopt`.
    - **The kill switch:** halt by a control request, by a file, and by a
      signal (a unit test); flatten through decisions, in process and
      through the kernel service.
    - **The document:** it parses, its hash ignores layout, and it says why
      when it is refused. The modes refuse a run with no deployment, a
      broker with no adapter, and a document on a replay.
- **E-96 (K-2.2, A-D32; the house presets, and the execution grammar as
  K-2.2 writes it).** The pipeline's guideline (section 15,
  `absolute-pipeline/docs/design/agent-guidelines.md`) fixes two
  execution presets every agent program names, `futures_house`
  (`futures_fixed_base` plus 3.75 a contract a side, no slippage for
  starters) and `equities_house` (half a cent a share, one dollar at the
  least, a tenth of the book per name, clamped); both are appended to
  `corpus-v2/lib/execution.dsl` as the guideline writes them, comments
  included. The parser accepted less than K-2.2's grammar: it required
  a currency after `per_share P`, `min M` and `per_contract P`
  (`per_share 0.005 USD min 1 USD`), where K-2.2 writes none, and it
  refused `numerics exact`, which K-2.2 lists first. Both are the
  parser's now (`syntax/parser.rs`): a currency after an amount is
  admitted and not required (the book is USD, A-D32; `abt show` prints
  the block with it, as before), and `exact` is a word of `numerics`,
  which the lowering already read as `two_pass` (D-6.3: exact over what
  has no integer form is two-pass; the kernel's exact windows over
  ticks are iteration 2's), so the normal form keeps the word and the
  evaluation is `two_pass`'s until then. `limits { weight <= 0.10 per
  name clamp }` was accepted as written (K-3.5). Nothing of the
  guideline's text differs from what the library holds. Rejected:
  writing the presets in the stricter syntax (every agent would have
  to learn a unit the grammar does not ask for). One point stays the
  owner's: by E-71, `commission per_contract P` is charged where the
  catalog states no rate per contract, and the catalog's rate otherwise
  (`sim/mod.rs::commission`), so on a store whose roots state one (the
  Databento adapter's, D-2.3) `futures_house` pays the catalog's rate,
  not 3.75; the guideline's "7.50 round-turn, half per side" reads the
  written rate as the one paid, with `from catalog` the form that
  defers. This tree keeps E-71's rule, since changing which rate a
  program pays is a change of intent. A second: `equities_house` writes
  `orders moo_moc`, an intraday order the checker admits under a
  sub-daily base alone (WF-14), so a daily equity program names the
  preset with the one-field variant the harness allows,
  `equities_house with { orders moc }` (or `moo`, `market`); the owner
  may rather have the preset say `moc`. Tests:
  `abt-tests/tests/corpus_v2.rs::the_house_presets_check_clean_run_and_show_their_fields`
  (R8L on `futures_house`, `opening_gap` on `equities_house` and
  `momentum_top_n` on `equities_house with { orders moc }` check clean,
  run on the synthetic market of their class, decide and fill, pay the
  commission the rules above give, and `abt show` prints the preset's
  fields expanded); the library check reads the presets. Falsified by
  `scripts/mutations/k-2.2-commission-unit.patch` (the parser requires
  the currency again, and the library no longer parses).

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
- **E-64 (P-2.3, P-2.4, D-5.6; flow control on a shared connection).**
  P-2.3 lets answers interleave on one connection. P-2.4 has the service
  block on a full channel, and the spec does not say where that channel's
  bound is kept.
  - **The bug.** Before this entry, the bound was kept at the client:
    - The client's reader blocked once one reply held 64 unread frames.
    - Every other answer on the connection then waited behind that reply.
    - A client that read one answer while another streamed hung. Examples:
      a run's progress held unread while its run was halted; a kernel
      writing its outputs at a checkpoint while its own stream ran ahead.
  - **The fix.**
    - The reader never waits: each frame goes to its request at once.
    - The bound is kept at the service, for the one answer whose size
      grows with the data, a subscription. Its channel is the events sent
      and not yet acknowledged: `SUBSCRIPTION_WINDOW`, 65,536 by default,
      which `DataService::with_window` configures.
    - The producer waits for an `ack` before sending past the window. A
      cancel, or the connection closing, ends the wait.
    - The wire client acknowledges each batch of events as it hands the
      batch over. "May ack" (D-5.6) is therefore "does": a subscriber that
      never acknowledged would receive one window.
    - A reply dropped before its end sends `cancel`.
  - **Unchanged.** Other answers (tables, a run's frames) are bounded by
    their request, so the client holds them as they come.
- **E-67 (D-7.1 to D-7.6, A-8, A-D22; the resolver as built).**
  - **The catalog** is a JSON file, named by `abt data serve --config FILE`
    (and `abt data resolve|catalog --config FILE`):
    - `prices`: A-8's symbolic unit prices, given values;
    - `sources`: each with its name, kind, adapter, freshness, the bytes a
      second it delivers, its quota, and its offers. An offer states its
      class, relations, calendar, first and last day, scope, receipt
      times, supplies, attributes, and the instruments a request covers.
    - A price named but not given is refused at load.
    - The store is the source `store`; what it holds of each source
      (`cached`) is the registry's versions.
  - **The cost of a purchase.** D-7.2 leaves the units' counts open. One
    relation of an offer over `[from, to]` is estimated as:
    - rows: the days, times the bars a day (one at a day or coarser, a
      390-minute session's bars below), times the offer's instruments (one
      when absent);
    - 32 bytes a row, and one request;
    - money: the unit prices times their units (requests; bytes / 1e9;
      instruments × days; ⌈days / 30.44⌉ months);
    - seconds: the bytes over the source's throughput (1e7 B/s when not
      stated), plus 0.1 s a request.
  - **A version the store holds costs nothing,** its read included. So the
    store's versions are chosen as the transitional resolver chose them:
    the version a preference names, the finest under `finest`, reviewed
    first under `reviewed`, else the first source by name.
  - **Spans.** A need's span is `[from, to]`. An unstated end is the
    store's: the first or last instant it holds. Two spans abut within four
    days (a weekend and a holiday), at the need's ends too. An offer is
    priced only over the parts of the span no held version reaches.
  - **The cover** (Proposition D-7.4) is computed per need and relation,
    over the candidates in the order of their ends, in O(m²) for the
    handful there are. Ties go to the candidates listed first: the store's,
    by source name.
  - **No cover.** A relation whose span no source covers whole is bound to
    the store's version, and the kernel's precondition on the span warns
    of it (K-3.2). The resolver adds a note only when some source offers
    the relation.
  - **The budget** caps the whole demand: money 0 unless given (A-D22),
    then bytes, requests and seconds when given.
    - A plan over budget buys nothing. Each relation a purchase would
      complete is bound to what the store holds. The response carries the
      cheapest full plan (`over_budget`) and a note.
    - A relation the store holds none of is refused with `out_of_budget`,
      naming that plan's cost.
    - A plan whose requests of one source exceed the source's quota for
      its window is refused before anything is bought.
  - **Fetch, ingest, version.** The `Adapter` trait (`vendor`, `offers`,
    `fetch`) returns keyed rows. They are ingested under the source's name
    with the offer's attributes and calendar. The version is the store's
    from then on, so resolving again buys nothing.
  - **Merged versions.** A relation covered by parts of several versions is
    bound to one version of their rows, written under the source `cover`
    (`Store::ingest_stored`). Each part contributes the rows from the end
    of the part before it to its own end. The version states what every
    part states, and `cover` names the parts.
    - The version is content-addressed, so resolving again gives the same
      binding (Proposition D-7.5).
  - **Notes.** `ResolveResponse` gains `notes`: what was bought, what could
    not be covered, the budget. The kernel adds them to the run's warnings
    under the bias `binding`.
  - **Not yet.**
    - Dynamic needs: the pre-pass waits for the option class (M-7). A
      demand with one, and `resolve_dynamic`, are refused, naming it.
    - A source naming an adapter abt lacks (or settings its adapter
      refuses) is listed and planned, and buying from it is refused,
      naming the adapter. The adapters are E-68's.
- **E-68 (D-7.7, D-2.4, B-10, A-D24; the vendor adapters as built).**
  - **Configuration.** A catalog source's `settings` (free key-value
    pairs: a lake's root, a dataset, the environment variable that holds a
    key, never a key) configure its adapter, which is kept under the
    source's name. `abt data ingest --adapter norgate|databento --from
    LOCATOR` reads what the locator holds over the dates given, whole: a
    need and a budget go through `abt data resolve` against a catalog that
    names the source.
  - **Purchases.** The relations a plan buys from one offer over one span
    are one request. What the adapter returns beside them (the registry's
    relations that key them, `continuous` and `future_spec`) is ingested
    with them. A fetch may state attributes of one relation only
    (`Fetched::relation_attributes`: `dividend_dates = ex_only` is said of
    `equity.dividend` alone).
  - **Keys.** A fetch's keys are the vendor's (a key the registry has not
    seen is a new instrument: Norgate's `asset_id`, a future's
    `instrument_id`), or symbols joined to the registry's ticker spans at
    each row's `t`, a span's last day included (a Databento equity's
    `raw_symbol`). A symbol no span carries rejects the read, naming the row
    (A-D24).
  - **Norgate** reads the lake through the Parquet reader and makes the
    ten decisions of `docs/handoff-data.md` (section 4) as the retired
    script made them.
    - Several lists (`--lists 'S&P 500=SP500;S&P MidCap 400=SP400'`): the
      universe is the members, ever, of any; a member of two lists is a
      member of each.
    - Index series (`--series '$SPXTR=SPXTR'`, a `series_type = index`
      asset) become the series class's `value`, keyed at the day.
    - `equity.ticker` holds the ticker each asset carries on every day of
      its span through its last bar, beside the registry's spans.
    - `registry/trading_calendar` is not read. The sessions are the days
      the bars have, as the bundles' were, and the kernel puts its own
      calendars (M-2h).
    - The reviewed exceptions become the attribute `reviewed_exceptions`,
      JSON rows keyed by `asset_id`. The file and its editor move to
      `scripts/reviewed/`.
    - The source is a local mirror: reading the bucket (`s3://`) waits for
      an S3 store tier (D-3.7; the owner's open item, A-8).
  - **Databento.**
    - DBN versions 1 to 3 are decoded by `adapters::dbn` (the standard
      library only). Its known answers are fixtures written by Databento's
      reference implementation (`dbn` 0.72, `crates/abt-data/tests/fixtures/dbn`).
      A `.dbn.zst` file and a compressed response are decompressed with
      `zstd`, which Parquet already links.
    - The bucket's Parquet layout is read too (bars with decoded prices,
      definitions per root).
    - The historical API (`timeseries.get_range`) is asked with the key as
      the basic-auth user. `https://` goes through `curl`, given the key and
      the form on its standard input (abt carries no TLS); `http://` is
      spoken to directly (the tests' server).
    - **Receipt.** An OHLCV record's `ts_event` is the start of its
      interval, so `t` is the bar's close and `r = max(ts_recv, t)` plus the
      processing delay, or `t` plus it when the rows carry no `ts_recv`.
      B-10's `ts_event + processing_delay` would make a bar readable before
      it ended.
    - **Futures.** Contracts are keyed by `instrument_id`, the root
      (`asset`) from the definition; a bar of an instrument without one is
      refused, and spreads and options are left out. Each root becomes one
      back-adjusted continuous series, keyed by Databento's continuous
      symbol (`ES.v.0`, `ES.c.0`):
      - Sessions are the root's bars' runs; a gap over 45 minutes ends one
        (CME Globex's daily break is an hour; `--session-gap`). A session
        is labelled at its last bar's close and opens at its first bar's
        start.
      - By volume, a session holds the contract that traded the most in the
        session before, never one expiring before the contract held. The
        first session holds the nearest expiry, as its own volume is not
        known until it ends. By expiry, it holds the nearest expiry that
        trades in it.
      - The gap at a roll is the new contract's last close minus the old's,
        in the session before. When the new contract did not trade then, it
        is the new contract's first open. The series before a roll is
        shifted by the later gaps (additive), so the last contract is
        unadjusted.
      - `continuous` names each bar's contract and adjustment.
        `future_spec` holds the terms: `first_trade` the activation,
        `expiry` and `last_trade` the expiration, `tick` the minimum
        increment, and `point_value` the `unit_of_measure_qty` unless
        `--point-values` gives the root's (a bond's points are a percent of
        its size). `settlement` is as configured per root, else `unstated`.
      - `point_value` is stated per root at each session's day, and
        `commission` when `--commission` gives it. `member` and `usable` are
        research's, not the vendor's.
    - Statistics (settlement prices, open interest) are decoded and not yet
      stored: the future class gains `settlement` and `open_interest` with
      the futures model (M-7).
    - **Equities** are named by `raw_symbol` (the metadata's mappings, or
      the Parquet's `symbol`) and joined to the store's tickers, so the
      Norgate lake is ingested first. Their listing is the Norgate
      versions'.
    - **Live.** `databento_live` speaks the gateway's protocol: the
      challenge answered with the hex SHA-256 of `challenge|key` and the
      key's bucket (`sha2`), the subscription, `start_session`, then DBN.
      Each record is stamped at acceptance, its rows' `r`. A continuous
      symbol is subscribed with `stype_in = continuous`, and its rows are
      keyed by it with adjustment 0. The live clock, recording and the
      subscription to the kernel are M-6c's.
  - **Retired.** These are gone (M principle 4):
    - The kernel's `ingest.rs` (`norgate_daily`, `databento_minute`) and
      `abt bundle build --from-norgate|--from-databento|--processing-delay`.
    - `scripts/ingest/*`: the lake conversion, the reconciliation report
      (the `actions` feed statistic of Q-6 reports what it did), and the
      first formulation's weekly and session layouts, which no class reads
      any more. They are in the history.
    - `scripts/bench.sh` ingests the lake with `abt data ingest` and runs
      over the served store.
- **E-69 (D-5.2 live, D-5.4, D-5.5, K-6.2, K-6.3, A-4.4; live-sim as
  built).**
  - **Sessions.** A live session records a feed (`adapters::Feed`, an
    adapter's `subscribe`) into the store, write-ahead, with the session's
    name as the source. Opening it ingests two things:
    - its prelude: the names it trades, and every relation it records,
      empty, so that a demand binds the session's versions before they
      hold a row;
    - its planned bars, `live.<session>.plan`: the base labels of the
      exchange's session. A live-sim run builds its schedule from them,
      since its data does not hold them yet.
  - **Receipt.** A batch is received at its acceptance: `r` is the clock,
    never below the last row's and always past the last barrier emitted.
    So the order of acceptance is the canonical order (D-5.3).
  - **The live stream.** A subscription whose clock is `Live` follows the
    session's versions of the spec's relations (`StreamSpec.session`, new).
    It emits the barriers of the spec's clock relation (the kernel's
    labels):
    - a barrier is due at its label plus its calendar's grace (`abt data
      serve --grace`; by default 0 for sessions and 250 ms otherwise);
    - it is emitted at the clock's instant, after every row accepted
      before it;
    - it is recorded as `kernel.<run>.clock`, under the source `live`.
    A barrier's events are the merge of the session's versions with the
    recorded clock up to it. So the live stream is the replay of its
    recording, seq for seq (Proposition D-5.4).
  - **Replay of a live run.** `abt run --replay-of RUN` reads the session's
    versions, now whole, and RUN's recorded clock. Its base bars are the
    recorded barriers', so its schedule is the live run's. `--paced` sends
    each barrier once the data layer's clock has gone as far past the first
    barrier's instant as the barrier is (D-5.5).
  - **The kernel.**
    - `abt run --mode live-sim --session S`; the run's identity names the
      mode `live-sim`.
    - The instant a barrier was emitted at is the kernel's `now`; in replay
      that instant is the bucket's end, as before.
    - Pushdown is off for both runs of a live session. The data layer
      answers a pushed aggregate at the bucket's end, not at the instant a
      live barrier was emitted at.
  - **Lag and `late`.** The kernel measures each decision bar's lag against
    the clock it shares with the data layer (the wall clock, or a test's):
    the instant it evaluates the bar minus the barrier's `at`.
    - The deadline is one bar of the decision calendar by default
      (`--deadline S|none`). Past it, the bar's decisions are late.
    - `late send` sends them flagged (`decisions.late`); it is the default,
      and the field is optional, so no program's hash changes. `late
      reject` drops them with the reason `late`.
    - When over 1 % of the bars are late, the run is warned under the bias
      `latency`, with the lag measured.
    - Lag marks on the control channel (D-5.4) are not sent. The kernel's
      own measurement is what its rule reads, and a mark in the stream
      would give a replay other seqs.
  - **Replay equals live (G4)** within the deadline, as the gate runs it.
    The replay reads no lag, so a decision dropped as late live is not
    dropped in its replay.
  - **Rehearsal.** `abt data serve --record synthetic --session S --symbols
    A,B --start T --end T [--clock-rate R]` records the synthetic feed
    while the service serves.
    - A vendor's live feed is recorded through `DataService::record` with
      its adapter's subscription (`databento_live`'s feed). The command
      line for a vendor source comes with the live deployment (M-10).
    - With `--clock-rate`, a kernel in another process does not share the
      service's clock: it runs with `--deadline none`.
  - **Not yet.** The names a session first meets mid-session are not in a
    live-sim run's binding, whose symbols are fixed when it binds: a
    session declares its names in its prelude. A tuple of a closed bucket
    that arrives after its barrier is late and is handled by the fold's
    late rule (K-4.4), as in replay.
- **E-70 (D-2.3, D-2.4, D-4.4, D-7.6, D-7.7, Q-6; the option class's data
  as built).**
  - **The option class** (`corpus-v2/catalog/option.dsl`, rendered from the
    registry like the others).
    - Its own relations are `quote` and `margin` (per contract).
    - `greeks` is stored by the class but read through the kernel's
      relation of the same name (K-2.4). The class file shows it as a
      comment, so that no program reads it as a primitive.
    - The underlyings are the equity class's bars and actions. The class
      file lists them, and the store keeps them once, as `equity.*`. A
      catalog relation records the class that stores it (`of`). The kernel
      reads an option program's `close` as `equity.close`.
    - The kernel's checker reads the class (the vocabularies `rights`,
      `styles`, `settlements`, the sort `Option`). Libraries written for any
      class are not probed in it: its `close` is its underlyings', not its
      instruments'.
  - **Relations added to the other classes:**
    - equity: `special_dividend(A, @T, Ex, Amount)`, what OCC adjusts
      strikes for (K-5.5.5). It is a relation of its own because a
      `special` column would change the arity of `dividend`, which every
      program reads. Also `margin`, per share.
    - future: `settlement`, `open_interest`, and `margin` per contract.
      Settlement is back-adjusted as the bars are.
  - **The registry.**
    - `option_spec` gains `settlement_time` (`am`: the expiry day's
      opening prints, as for AM-settled index options; `pm`).
    - `chain_index` keys on both of its entities, since a snapshot holds
      many contracts of one underlying at one instant.
    - Every adapter's registry relations (a contract's terms, a chain, a
      root's `continuous`) are written under the registry's source, so
      each has one current version. A binding names `option_spec`,
      `chain_index`, `future_spec` and `continuous` whenever the store
      holds them.
  - **The chain filters** have one implementation, `abt_wire::chain::admits`,
    read by the data layer's chain query, the resolver's pre-pass and the
    kernel's `chain`.
    - Moneyness is strike over spot. Both ends of every range are
      included.
    - A moneyness filter without a spot admits nothing.
    - A contract with no quote has no open interest.
  - **The chain query** answers from the `chain_index` snapshot of `t`'s
    day taken at or before `t`. Without one it is empty, and `explain`
    says `unresolved`. Its rows are each admitted contract's id and terms.
  - **`DynamicNeed`** gains `underlying_symbol`. The kernel names the
    underlying by its ticker and the spot by its store name and column
    (`equity.close`, `P`); the resolver finds the id among the ticker
    spans.
  - **The pre-pass** evaluates the filters on each day the spot relation
    holds a row of the underlying. The demand does not carry the decision
    calendar's labels; a daily decision calendar's days are those rows.
    - The contracts admitted become a static need of the dynamic need's
      relation over the span.
    - A day without a snapshot is bought from a source whose offers have
      the `chain` scope, at one request per underlying and day, within the
      money budget. An unpriced placeholder (A-8) is never within a budget.
    - The binding records the dynamic needs, their underlyings by id.
  - **`resolve_dynamic`** buys a chain on demand only when a request costs
    nothing, since the request carries no budget of its own; otherwise it
    is refused out of budget, naming the cost. The kernel's use of it
    lands with the option model.
  - **The ORATS adapter** reads the strikes layout of the Data API by its
    field names (`ticker`, `tradeDate`, `expirDate`, `strike`,
    `stockPrice`, `callBidPrice` … `putOpenInterest`, `callMidIv`,
    `putMidIv`, `smvVol`, `delta` … `rho`, `quoteDate`, `updatedAt`), as
    CSV or JSON, from files or from the API (`/datav2/hist/strikes`, one GET
    per underlying and weekday, the token passed on curl's standard input).
    - A row is two contracts, keyed by their OCC symbols. The layout has no
      last trade, so `Last` is null.
    - The put's delta is the call's less one. The layout gives no put theta
      or rho, which are null.
    - ORATS's theta per day, and vega and rho per point, are stored per year
      and per unit, as the kernel's model states them. **The owner checks
      these units against a real file.**
    - The style and settlement are a setting per underlying (American,
      physical, PM by default).
    - The underlying's price is `series.value`, named by its ticker.
    - Rows are received at `quoteDate`, else `updatedAt`, else 21:00 UTC of
      the trade date. They are labelled at the trade date, or at the quote
      instant with `snapshots`.
    - `abt data ingest --adapter orats` reads it.
  - **The `chains` feed statistic** (Q-6) reports, per underlying:
    - the days it has a snapshot, and its contracts a day (mean and most);
    - its distinct expiries, and its lowest and highest strike;
    - the share of contract-days quoted both ways (a positive bid below
      the ask);
    - the mean open interest.
- **E-74 (D-3.2, D-3.4, X-2.1; the resident cache).** A store keeps
  decoded row groups resident under `resident_budget` bytes.
  - **The budget** defaults to half of physical memory: `/proc/meminfo`
    where there is one, else 4 GiB. `ABT_RESIDENT_BUDGET` (bytes) sets it,
    and 0 turns the cache off.
  - **The key** is a row group's file and the group's index. A partition
    file is named by its content's hash and never written again (D-2.6),
    so a resident group is the group on disk.
  - **Eviction** is the clock (second chance) over a slab. A group's
    reference bit is set when it is hit and cleared as the hand passes; the
    hand evicts the first group it finds cleared. A group read once (a
    scan) is not referenced, so it is the first to go.
  - **A read through the cache** decodes a partition by row group on first
    touch. An entity read gathers its rows from the resident group by the
    entity offset index, in the file's order, so it returns the rows a read
    from the file does.
  - **The index** is memory-mapped and kept with the partition's footer
    (its row groups' row counts). Both are read once per file.
  - **The counters** (hits, misses, evictions, resident bytes and groups)
    are the store's `resident_stats`.
  - **Manifests** are read once per version, a version being immutable;
    the registry is still asked first, so a collected version is unknown.
  - **Measured** by `crates/abt-data/tests/budgets.rs` (2,000 names ×
    2,520 days):

    | Read | From disk | Resident |
    | --- | ---: | ---: |
    | `point` | 721 µs | 40 µs |
    | `latest` | 689 µs | 41 µs |
    | `series` of 252 rows | 6.1 ms | 158 µs |
    | `section` of 2,000 names | 1.6 ms | 303 µs |

    84 row groups of 5.04M rows are resident in 202 MB: 8 B per value,
    within A-7.3's 16.
  - **The aggregate pass** (D-6.4; 06-performance 2.2) gains three things,
    the answer unchanged.
    - Groups take dense slots on first sight.
    - The answer is column by column. Its (t, group) order comes from a
      counting sort on t's rank, stable over the groups, rather than from
      a sort of rows.
    - The put path names a bucket once per span of rows. It leaves rows
      already sorted and unique as they are, instead of sorting and
      comparing their values.
    The rolling 252-row mean of 5.04M rows on eight threads takes 4.7 s
    (`running`) and 5.3 s (`two_pass`), from 5.6 s and 6.1 s. Writing the
    answer's log segment is 1.3 s of it: this VM's first write of fresh
    pages runs at about 190 MB/s.
- **E-99 (D-7.2, D-5.1, P-3.1, K-4.5; a demand scoped to the instruments
  a program names; protocol 1.2).** A futures run streamed every staged
  root's minute bars whatever its universe: one root over eight years held
  14.5 GB and the pipeline's machine killed its data service. `StreamSpec`
  gains `entities` (optional, a minor change: protocol 1.2), the
  instruments whose tuples the stream carries. The kernel fills it from
  the program: the rows of its universe's `instruments` table and the
  values of `need instruments where root | symbol | ticker = "…"`
  (parsed since V-3.11, used nowhere until now), resolved through the
  binding's ticker table to the data layer's ids, each futures root with
  the contracts its continuous series holds. The data layer reads each
  partition with those ids as the identity filter (a hot version skips
  the other instruments' blocks; a Parquet partition reads through its
  entity index; a relation keyed by no entity is read whole), and the
  barrier index counts the same tuples, so `to_seq` (a hold-out's
  truncation) numbers the scoped stream. The binding reads what it loads
  per instrument (the base bars' times, the sessions, the prints a
  session-offset clock needs, a root's `continuous` rows) as one series per
  named instrument, which a hot version answers from that instrument's
  blocks, and names are resolved within the program's class first (a
  futures root and a stock can share a ticker: ES, CL). Pushed-down
  aggregate answers of other instruments are dropped by the kernel; the data layer still
  computes them (an `AggregateRequest` has no instrument filter yet). An
  unscoped stream's canonical form, hence its id, is unchanged; a live
  session carries every instrument it records. [D] The scope from the
  program's own text rather than a run option: a program that names its
  instruments reads only them, so its hash already fixes what it reads,
  and a rule reading another instrument finds no rows, as a universe
  outside the table would. Rejected: a scope in the binding (the binding
  pins versions, not rows, and its id would change for every universe).
- **E-94 (D-2.3, D-2.2, D-3.6, P-3.1; the `pipeline` class).** The
  pipeline design (`absolute-pipeline/docs/design/pipeline.md`, PL-7.2)
  puts the provenance of what its agents submit in the data layer
  (A-D20): a class `pipeline` joins the `kernel` and `query` namespaces
  of D-2.3, and, unlike them, is a class of the registry (`catalog.rs`
  `classes()`, `corpus-v2/catalog/pipeline.dsl` its rendering, `abt data
  catalog --class pipeline`), so a fresh store holds its five schemas
  and the gateway writes against them rather than making its own up.
  The relations, each with the receipt time `r` every stored row carries
  (D-4.2; a put without it is received at its event time):
  `document(DocId, Source, Author, @P, Size: Count)`;
  `hypothesis(Id, DocIds, @H, Agent)`; `submission(Id, HypothesisId,
  ProgramHash, PlanHash, Lineage, @H)`; `outcome(SubmissionId, @T,
  Reveal: Count, ForwardFrom, ForwardTo: Timestamp, Status)`;
  `audit(Job, @T, Kind, Subject, Bytes, DurationNs: Count, Outcome)`.
  They are written only through `put` (D-3.6, P-3.1) like `kernel.*`
  and read with `scan`, `series` and `catalog`; no program reads them,
  and the class has no base calendar and no unit, like `series`.
  - **Ids are labels, not bytes.** PL-7.2 leaves the ids' type open
    (the task named `Bytes` or `Label`, whichever the put path keys
    by). A row's identity is its entity and label columns with its time
    (D-2.2): a label id keys the row, as `query.trials.trial_id` and
    `query.plans.hash` already do, while a bytes column is a value
    outside the identity, outside every predicate of the query forms
    (`query.rs`) and never held hot. The class file is also read by the
    kernel's checker, which has no bytes type (`Ty::parse`). `DocIds`,
    the JSON list of a hypothesis's documents, is a label for the same
    reason; its texts are as many as the hypotheses, as the trial ids
    are as many as the runs. Rejected: a `Bytes` id (no identity, so a
    second put of the same row would not be the same row), and a
    numeric entity id (the ids are content hashes and ULIDs of the
    pipeline, not the registry's instruments).
  - **Event times.** Every relation needs one (`validate_schema`):
    `document`'s is its publication `P`, `hypothesis`'s and
    `submission`'s their horizon `H`, `outcome`'s and `audit`'s the
    reveal's and the request's instant `T`, beside the listed columns.
    The names are the class file's (`DocId`, `ForwardFrom`), as the
    market classes' columns are (`A`, `T`, `P`, `BidSize`): D-2.3's
    table writes them in lower case for every class.
  - **The statuses** `insample_only`, `forward` and `retired` (PL-7.3)
    are the `outcome` relation's note rather than a vocabulary: the
    kernel's class grammar admits its own vocabularies only (`lists`,
    `reasons`, `names`, ...), and no program checks a label against
    them.
  - **Tests.** `abt-tests/tests/pipeline_class.rs` (the class and its
    five schemas in the catalog and the file; a row of each relation
    put and read back in process and over a socket; a put without `r`),
    `abt/tests/data_cli.rs::the_pipeline_class_file_renders_from_the_registry`,
    `abt-data/tests/store.rs::the_class_files_render_from_the_registry`
    (the file equals the rendering), and the corpus library check
    (the kernel reads the file). Falsified by
    `scripts/mutations/d-2.3-pipeline-schemas.patch` (the class is
    rendered but its schemas are not registered with a fresh store).
- **E-155 (P-3.1, P-6, P-7; protocol 1.3: the fields the delivery of
  aggregate answers needs).** 3.1 answers `aggregate` with a table and
  `{ cache_key }`, and a stream names the relations it carries by version
  only. Protocol 1.3, a minor version, appends to `schema/abt.fbs`:
  `AggregateRequest.factor` (a `Factor { relation, column }`: the value
  aggregated is the request's column times the factor's column over the
  rows both relations hold at one entity and time) and
  `AggregateRequest.entities` (the instruments the pass reads, E-99's
  scope); `AggregateCall.answer` (true by default; false asks for the
  response alone, without the table); `AggregateResponse.relation`,
  `version`, `rows` and `inexact_rows` (the answer's cache object, which a
  stream spec can name, and its inexact rows, E-156); a `runs` request
  (`RunsCall { view, RunsRequest { relation, calendar, entities, t0, t1 } }`,
  answered by an `AggregateResponse`), the last member of the message
  union; and `StreamSpec.from` and `whole` (a stream bounded below in
  event time, and the relations exempt from the bound). The data service
  answers `factor`, `entities` and `answer`, and fills the response's new
  fields (E-156); `runs`, `from` and `whole` are on the wire only: the
  service refuses a `runs` call as a request it does not take and reads
  no stream bound yet. `PROTOCOL` is `1.3`.
  [D] An added field is left out of its table's canonical form while it is
  absent or at its default (`abt_wire::canonical::ADDED`, read by the owned
  type's name in the canonical serializer: the types planus generates
  carry no serde attributes), so the canonical form of every message 1.2
  could write is the bytes 1.2 rendered, and every cache key stands. A
  stream's id is the hash of `stream::Resolved`, which the new spec fields
  do not reach. `agg::cache_key` renders the request itself: through a
  `serde_json::Value`, as it did, the type's name is lost and the new
  fields would be written as `null`, moving every key.
  `abt-wire/tests/rpc.rs::a_1_2_aggregate_request_and_stream_spec_keep_their_canonical_bytes`
  pins the canonical bytes of a request, a call, a response and a stream
  spec as a 1.2 tree rendered them, shows the new fields in the form when
  set, and round trips every new message;
  `abt-data/tests/agg.rs::a_1_2_request_keeps_its_cache_key` pins a cache
  key a 1.2 tree computed. Falsified by
  `scripts/mutations/e-155-canonical.patch` (an added field written while
  absent: the pinned bytes move).

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
- **E-156 (D-6.1, D-6.5, K-4.5, E-47 (a); the pass's `count`, a product
  of two relations, the instrument filter and the inexact rows; protocol
  1.3).** D-6.1 aggregates one relation's column, and E-47 (a) leaves
  `count` in the kernel because the pass counted the rows with a value.
  MW14's weekly `dvol` is the mean of the derived `dollar_volume(A, T, D)
  :- close(A, T, P), volume(A, T, V), D = P * V`, and `traded@weekly` is a
  `count`, so the kernel derives both from every daily row. The pass
  (`abt-data/src/agg.rs`) now:
  - counts, for `count`, every row of the window, with a value or not, as
    the kernel's `count` counts the rows its window solves. A cache object
    of the same key written before holds the other count; one whose
    version records no inexact rows is computed again.
  - with `factor`, reads the second relation through the same filter and
    joins it per entity on `t`: a row of the product wherever both hold
    one (the kernel's join), its value `P * V` formed as `eval_expr` forms
    it (two `Count` columns as an `i64` product, anything else as the `f64`
    product), its receipts both rows'. The products are reduced by abt-agg
    as one column, in time order. A product is a derived series: under
    `exact` it is reduced as `two_pass` (D-6.3), never as ticks. It is
    grouped per entity, by a one-column function; the factor's version is
    in the cache key.
  - with `entities`, keeps those instruments in every read of the pass (a
    relation keyed by one entity; one keyed otherwise is read whole, as the
    stream reads it). They are sorted and deduplicated before the key: a
    scoped answer is one object of its own.
  - counts the answer's inexact rows: an input row (of either relation)
    received after the barrier of the first label at or after its time on
    the calendar the answer is read at (the bucket's for `bucket(C)`, else
    the request's `calendar`; a request naming neither has no barrier, and
    every row is inexact); a row with a null in a column the kernel decodes;
    a second row of a group at one instant; an integer product past `i64`;
    an answer row whose arithmetic failed. A label's barrier is its
    earliest `at` in the clock, every revision read, where the stream
    first emits it. A row after the calendar's last label is in no window
    and in no answer's receipt, and is not counted. When none is inexact,
    every answer row at a label is computed from rows received by that
    label's barrier and is the kernel's window there; its receipt is at or
    before the barrier, so in a stream it precedes the barrier. The count
    is stored as the cache version's attribute `inexact_rows`
    (`Store::cache_put_with`), so a hit returns it.
  - answers an `AggregateCall` with the object's relation (`cache.<key16>`),
    version, rows and inexact rows, and sends no table when `answer` is
    false.
  [D] Two readings are stricter than the plan of package 2. A null is
  inexact for `count` too: the kernel's decoder reads a null as NaN only in
  a quantity or an integer column and refuses it in any other
  (`Decoder::value`), and the pass does not know the kernel's types; for
  the same reason every decoded column of the relation is checked, not the
  aggregated one alone. Two rows of a group at one instant are inexact: the
  kernel orders a bar's rows by their bindings (E-46), the pass by the
  store's key. `WindowState::with_scales` under `TwoPass` over floats
  keeps its scales and never reads them (`impl Value for f64` ignores
  them): the window holds its rows and reduces them with
  `abt_agg::reduce(f, TwoPass, ..)` at each value, the function the
  kernel's `aggregate` calls, so a product entered as its `f64` is reduced
  as the kernel reduces its window of `dollar_volume` rows. Nothing reads
  the new forms yet: the kernel's pushdown asks the 1.2 forms (package 2,
  K1 to K3).
  `abt-data/tests/agg.rs::{count_counts_the_windows_rows_with_or_without_a_value,
  a_product_of_two_relations_is_the_joined_rows_reduced_by_abt_agg_bit_for_bit,
  an_instrument_filter_answers_only_those_instruments_under_its_own_key,
  inexact_rows_counts_rows_received_after_their_barrier_and_nulls}` (the
  product over Parquet and over the hot store, against the values the
  stream carries) and
  `abt-data/tests/service.rs::an_answer_asked_without_its_table_names_a_version_a_stream_can_carry`.
  Falsified by `scripts/mutations/e-156-late.patch` (a row received after
  its barrier not counted) and `scripts/mutations/e-156-count.patch` (a
  `count` skips the rows without a value again).

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

- **E-63 (Q-4.3, Q-5 to Q-9, P-4; claims, statistics, reports and their
  commands as built).**
  - **The tables a test reads**, by input:
    - `returns of S`: the run's bar returns as `returns`.
    - A grid read whole (`returns of G`, `grid G`): a column per point,
      `trial0`…, on the bars every point has.
    - `grid W` of a walk-forward: its folds' `in_sample` and
      `out_of_sample` CAGR, which `wfe` reads.
    - `events of S`: one row per instance (a bar and an instrument a rule
      decided on), a column per rule (`decide_1`, or `unit__head_k` for a
      library's rule, since `#` starts a comment). For `cost_realism`,
      the fills' realised and modelled slippage per unit of notional,
      equal in a simulated run.
    - `feature R of S`: R's ledger, its numeric arguments as columns. When
      the test reads `feature` and `forward`, R's last value column, and
      each instrument's forward return over `horizon` bars of the binding's
      close. A step whose runs a `feature R of` reads observes R, outside
      H(P).
    - `feed R`: R's rows as a panel. With `(A in (T1, T2))`, a column per
      instrument by ticker on the times they share, in the filter's
      order.
  - **Phases.** A `before` claim reads a feed or a `run` step; those run
    steps run first. The adjustment is Benjamini–Hochberg within the
    claims of a phase, since a `before` claim decides before the `after`
    ones exist.
  - **The level is written or none.** `reject` and `accept` take 0.05
    without one. A comparison without one is the comparison alone, and
    with one it also asks the adjusted p-value to be below the level.
  - **Seeds.** A claim's or a test's seed is the first eight bytes of
    BLAKE3(H(Π):canonical name).
  - **`dsr`** is given the lineage's trial count, this execution's runs
    included, and the variance of their Sharpe ratios, unless the claim
    writes them (Q-3.2).
  - **A failed `block` claim.** Before the runs, it stops the steps that
    ask the kernel for runs and every step that reads one. After them, it
    bars the reveal. An undecidable verdict blocks nothing. Failed and
    undecidable verdicts are warned under false statistical significance.
  - **A reveal judges every claim again** over the embargoed bars. A claim
    that reads the revealed step reads the reveal's run. A claim that
    reads another step's runs is undecidable, since those runs never saw
    the embargo.
  - **Rows.**
    - `query.<plan>.verdicts` and `.tests` have P-4's columns, plus
      `policy` and `expect`, keyed by name and time. An execution's
      in-sample rows replace the last, and a reveal adds its own.
    - Claims and tests are trials in `query.trials` (kinds `claim` and
      `test`), each identified by BLAKE3 over its plan, canonical name,
      sample, runs and window.
    - `abt test` and `abt claim` record the same outside a plan.
  - **Feed statistics.**
    - `coverage` counts gaps against the binding's close bars, each
      entity within its own span.
    - `revisions` counts the rows received after their bar's day: the
      store returns a fact's latest revision.
    - `latency` gives quantiles of r − t in seconds.
    - `returns` gives simple returns' moments per name and pooled.
    - `liquidity` gives ADV, dollar volume and the zero-volume share; the
      spread needs quotes, which no class binds yet.
    - `survivorship` gives listings and delistings per year with their
      reasons, and the share of the first bar's listed names still listed
      at the last.
    - `actions` gives splits and dividends per year, and the splits whose
      close across the ex-date moves by the factor's inverse within a
      quarter.
    - `membership` gives list sizes, entries, exits, and member rows on
      bars without a close.
    - `chains` is empty until M-7.
  - **Activation statistics** read the run's decisions, so the attempted
    instances are those any rule decided on. The failure table F of
    `trace instances` is not read yet. Phi's interval is `coactivation`'s
    bootstrap. A pair above phi 0.9 is warned as a candidate for `abt
    contract --implications`. The cross-lag co-activation is over decision
    bars, for k from 1 to 20.
  - **Reports.**
    - `query.<plan>.report.*` holds `summary` (the report block's metrics,
      else Sharpe, CAGR, maximum drawdown, volatility, Sortino, Calmar and
      turnover), `trials`, `claims`, `tests`, `contract`, `warnings`,
      `biases`, `surface`, `folds`, `subperiods`, and the feed and
      activation steps' tables.
    - Each row is keyed by its position and the execution's time; `text`
      holds the rendering when the report block asks for it.
    - The forty biases carry their owner layer, status and source.
      Out-of-sample is `warned` without a hold-out.
    - `abt plan report` prints the text, else the tables.
  - **Commands.** `abt test TEST on INPUT` and `abt claim 'CLAIM'` name
    their run by id, in the input or by `--run`. `abt feed STATISTIC`
    takes `--binding B`, found through a run recorded over it, or `--run`.
    `abt activation --run RUN` prints the tables.

- **E-65 (M-5, Q-2.1; the briefs harness as a plan template).** The
  data-bundle doc's harness (section 8) is the query layer's.
  - **Checking.** An attempt is checked by the kernel service (`check`).
    One that does not parse is the kernel's refusal. The rest carry the
    kernel's diagnostics as it prints them, read back into code, judgment
    and message. The attempt's strategy replaces a library file declaring
    a strategy of its name, so a brief may ask for a corpus program again.
  - **Running.** A brief's first valid attempt runs as an instance of
    `template briefs_harness(PROGRAM) { program PROGRAM }`, under the
    instance `study brief_<brief> = briefs_harness(PROGRAM = <strategy>)`:
    - It is one run over its binding's whole sample, and a counted trial
      of its lineage.
    - The run's own execution block applies; the old harness ran every
      attempt under the default executor.
    - The verdict reads the run's outcome: `does not trade` without a
      fill, `does not survive costs` at a CAGR of zero or less, else
      `trades`; `halted: …` or `failed: …` otherwise.
  - **The market.** One synthetic daily equity market: AAA, BBB, CCC, DDD
    and SPY over 320 business days from 2022-01-03, seed 11, the old
    harness's daily market, in a temporary store. The old harness gave a
    sub-daily attempt a minute market. One store holds one market here, so
    such an attempt's binding is refused, and its verdict says so.
  - **Rows.** The attempts are rows of `query.briefs.attempts` (brief,
    attempt, t, strategy, valid, errors, warnings, parse error, the
    diagnostics as JSON), replaced by each report.
  - **The command.** `abt briefs report --briefs DIR --attempts DIR [--out
    FILE] FILES…` is the front's. It builds the market and runs `--kernels
    N` kernel services in the process (one by default).
- **E-66 (M-5, M principle 4; the kernel's study retired).**
  `abt-kernel/src/study` is deleted. The query layer's metrics, plans and
  briefs harness replace it.
  - **Retired.** `abt study` and `abt run --study` go, as does `--threads`,
    which was the study's.
  - **`abt run` writes its outputs.** It writes them under `kernel.<run>.*`
    on whatever data service it reads, including the temporary store of a
    dataset held in the process. This amends E-52, under which that store
    received none.
  - **`abt run`'s metrics.** It prints the same lines as before: the NAV's
    start, end, total return and deepest drawdown, the accounting, and the
    report block's period metrics.
    - They are the query layer's, computed from the run's tables by a
      reporter the front installs (`abt_kernel::cli::install_reporter`).
    - The reporter reaches the run's data service over a second
      connection. The kernel's data client can open one: the in-process
      provider returns a connector rather than one connection.
    - `--returns` writes the reporter's period returns. The kernel
      computes no metric.
  - **The untracked-trial warning.** A run outside a plan is warned as
    before, now naming what counts it. Over a data service, its program's
    lineage counts it (Proposition Q-2.2). In a temporary store, nothing
    does.
  - **The frozen records.** Three tests held the old study's values equal
    to the query layer's, to the bit: `tests/metrics_store.rs`, and the
    grid and walk-forward cases of `tests/plan_exec.rs`. They now compare
    with records frozen in `golden/study/` (`metrics.json`, `grid.json`,
    `walk_forward.json`).
    - The records were written while the old study still computed its
      values and the tests held the two equal.
    - `ABT_BLESS=1` rewrites them, in a commit that names its change, as
      for the golden runs.
  - **The study's tests.**
    - Those of its pure functions (embargoes, folds, the surface, Sharpe
      ratios per year) are `crates/abt-query/tests/validate.rs`.
    - Its split test moves to `tests/actions.rs`.
    - Its metrics tests read the query layer's metrics, a run's from the
      store.
    - The rest (the old command line, `lineages.json`, `trials.jsonl`)
      goes with what it tested. `plan_exec`, `claims_exec`, `plan_cli` and
      `normalise` cover the plans' equivalents.
- **E-73 (Q-3.1, K-5.1, K-9.3, P-4; the options and book metrics, G8's
  cases).**
  - **The tables they read.** Two of the kernel's outputs gain what the
    metrics need:
    - An option program's `holdings` gains, for each option held, its
      `underlying`, the underlying's price at the bar (`spot`), its
      `expiry` date (as `spec`), its `multiplier`, and its `iv`, `delta`,
      `gamma`, `theta` and `vega`. These are the `greeks` relation's at the
      bar: theta per year, vega per unit of volatility. They are null where
      the model has none, and for any other instrument. This is Q-3.1's
      "`greeks` relation times positions", joined in the kernel, which has
      both. Other programs' holdings are unchanged.
    - `book.variation_account` (E-52 left it 0) is the variation of the
      futures margined daily since their last settlement, Σ Q · (mark −
      reference) · point value, which the NAV counts (E-72).
    - `book.margin_source` names the model the requirement came from:
      - the one every holding's came from, or `mixed` when they differ;
      - the first listed while nothing is held;
      - `approx` for the leverage model's, as before.
  - **The options metrics** are those of a run that held an option.
    - The premium paid and collected: Σ |q| · price · multiplier over the
      option fills that buy, and over those that sell.
    - The exercises, assignments and expiries: the events' counts.
    - The days to expiry at entry and at exit: the mean calendar days from
      a fill's day to the expiry date. Entry is over the fills that open a
      position. Exit is over the fills and events that close one.
    - The net Greeks at every barrier an option is held:
      - Σ q · m · Δ · S (the delta in money);
      - Σ q · m · Γ;
      - Σ q · m · V, per unit of volatility;
      - Σ q · m · Θ, per year.
    - Realised against implied volatility, per underlying of the options
      held.
      - Realised is the annualised sample standard deviation of its log
        returns between consecutive barriers at which an option on it was
        held, at the report block's periods per year.
      - Implied is the mean implied volatility of those holdings.
    - The P&L attribution covers each bar an option is held at both
      barriers with its multiplier unchanged.
      - The P&L is q · m · (V₁ − V₀) between the marks.
      - Its terms are Δ · dS, ½ · Γ · dS², Θ · dt and V · dσ, all at the
        first barrier's Greeks. dt is in years of 365 days, as τ.
      - The residual is the P&L less the four.
      - A bar without the Greeks or the underlying's prices is
        unattributed; its P&L is reported.
    - An option opened and closed between two barriers is never held at
      one, so these metrics do not count it.
  - **The book metrics** are those of every run.
    - The margin utilisation is margin_initial over NAV: its mean and
      maximum over the barriers with a positive NAV, and the maximum of
      the maintenance over NAV.
    - The concentration of the holdings' marked notional values v:
      - the largest |v| over NAV;
      - the effective number of names, (Σ |v|)² / Σ v², averaged over the
        barriers that hold anything.
    - The funding paid: margin interest and borrow fees, less cash
      interest and the short rebate.
  - **Output.** `abt metrics` prints a `book` line, and the `options` lines
    for a run that held one. `--json` carries both groups as `book` and
    `options`.
  - **G8.** K-9.3's 25 cases are in three files, each by a test named for
    its case:
    - Options, all in `tests/options.rs`:
      - expiry: `a_long_call_in_the_money_at_expiry_is_exercised_physically`,
        `a_short_put_in_the_money_at_expiry_is_assigned_the_underlying`,
        `a_long_put_on_a_cash_settled_index_is_exercised_at_the_am_print`,
        `an_option_out_of_the_money_at_expiry_expires_worthless_on_either_side`,
        `an_option_at_the_money_within_the_tolerance_expires`;
      - early exercise:
        `a_short_call_is_assigned_early_before_the_ex_date_when_its_extrinsic_value_is_below_the_dividend`,
        `with_early_short_never_the_call_is_assigned_at_expiry_only`,
        `an_exercise_decision_on_a_long_exercises_at_that_bars_close`;
      - combos:
        `a_combo_fills_every_leg_at_one_barrier_under_one_order_id`,
        `a_combo_limit_fills_at_or_better_than_its_net_and_otherwise_works_until_its_time_in_force`,
        `a_combo_with_a_leg_unquoted_is_dropped_naming_the_leg`;
      - adjustments:
        `a_two_for_one_split_doubles_the_contracts_and_halves_the_strike`,
        `a_three_for_two_split_takes_the_multiplier_to_150_and_the_strike_to_two_thirds`,
        `a_special_dividend_lowers_the_strike_by_its_amount`;
      - margin:
        `a_naked_short_put_under_reg_t_needs_twenty_percent_of_the_underlying_less_otm_plus_the_premium`,
        `a_vertical_spread_under_reg_t_needs_its_maximum_loss`,
        `a_straddle_under_portfolio_needs_its_worst_loss_over_the_grid_at_its_volatility`,
        `published_margins_price_the_contracts_they_cover_and_reg_t_the_rest`,
        `approx_alone_is_a_fraction_of_the_underlyings_notional_warned_at_every_bar`;
      - Greeks:
        `the_models_greeks_reproduce_the_datas_iv_and_are_absent_outside_the_bounds`.
    - Futures, in `tests/futures.rs`:
      `daily_settlement_moves_the_variation_to_cash_and_leaves_the_nav_unchanged`,
      `a_roll_records_two_fills_and_two_commissions_and_carries_the_position`,
      `a_cash_settled_contract_held_at_its_last_trade_expires_at_its_final_settlement`,
      `a_futures_margin_call_liquidates_the_largest_requirement_first`.
    - Limits, in `tests/limits.rs`:
      `a_weight_limit_of_a_tenth_against_a_hold_of_a_fifth_rejects_clamps_or_warns`,
      `a_drawdown_limit_of_a_tenth_after_a_twelve_percent_fall_rejects_increases_and_passes_reductions`.
    - Their known answers are hand-computed in the tests. The pricing
      functions' are Haug's tables and the other references of E-71
      (`crates/abt-kernel/tests/pricing.rs`).
    - K-9.3 puts every case in `tests/options.rs`. The futures and the
      limits have their own files, as the migration's plan lists them
      (M-7).
- **E-100 (D-7.7; assets beyond the index lists).** The Norgate adapter's
  universe was the members of its index lists, so the lake's
  exchange-traded products (QQQ, SPY, GLD, TLT, IWM, the sector SPDRs)
  were never staged though their bars are in the lake. A `tickers`
  setting (`--tickers 'QQQ=ETF;SPY=ETF'`) adds the asset of each named
  ticker's latest listing, read from `registry/security_master`, as a
  member of the label named over its listing span (`member(A, T, "ETF")`,
  a label the equity catalog declares). [D] By name rather than every
  exchange-traded product of the lake: the hot store's budget (ST-9) and
  the adapter's in-memory read hold the named set, not the lake's
  thousands.
- **E-101 (D-2.4, D-7.7; a continuous series through a session without the
  contract held).** The Databento adapter's volume roll picked, per
  session, among the contracts that traded in it and expire no earlier
  than the one held, and ended the root's series at the first session with
  none: on a root whose deferred month trades thinly (SOL's monthly
  contracts from the bucket's live prefix) the session after the roll
  into J5, in which only the expiring H5 printed, ended the series, and
  1.8 years of SOL bars became 364 (ETH's and BTC's extensions stopped
  after their rolls the same way). Such a session now keeps the contract
  held and has no bar of it, the series going on when that contract (or a
  later one) trades again; a roll's gap is taken at the last session the
  outgoing contract traded in. [D] No bar rather than a bar of the expiring
  contract: the series holds one contract at a time and the roll was
  decided on the session before; going back to H5 for a day would roll
  twice and put an expiring contract's prices in the series.
- **E-102 (D-2.3, D-7.2, K-2.1; a future's tick, and the need that admits
  it).** The future class exposed `point_value` but not the tick, so the
  house sizing floor `ATR$ = point_value x max(ATR20, 10 x tick)` could not
  be written. A relation `tick(A: Future, @T, Size: Price<USD>) complete`
  holds the minimum price increment of the contract the root's series holds
  (the definition's `min_price_increment`, which A-D53 reads for the grid),
  stated at each session's day like `point_value`, through the roll and
  through a session without a bar of the held contract (E-101). It is
  admitted by a need of its own, `need tick`. [D] Rejected: bringing it with
  `need point_value`, since every class relation is a primitive of every
  program of the class, so every program writing `need point_value` would
  demand `future.tick` and fail to bind on a store staged before this
  erratum. E-98's `industry` likewise gets a need of its own, `need
  industry` (no need admitted it: a program reading it was refused). Both
  are left out of the demand's serialized form when false, and neither is
  a primitive of a program that does not write its need: a program's hash
  covers its demand and its primitives (`hash_view`), and admitting
  `industry` under `need lists` changed the hash of every program writing
  `need lists` (MW14 among them; `checkpoint.rs::program_hashes_are_stable`
  on the machine's Rust 1.99 caught it), which would have cut every such
  program's lineage.
- **E-103 (D-2.5, WF-15; the lists and series the full store stages).**
  The equity class's `lists` vocabulary held the parity bundle's labels
  and `SPX`, and the series class's `names` held `SPXTR`, `VIX` and
  `VIX_MA4`, so WF-15 refused programs naming the index lists and daily
  index series the Norgate adapter stages from the lake. The `lists`
  vocabulary gains `SP1500`, `SP100`, `R3000`, `R1000`, `R2000`, `RMID`,
  `RMICRO`, `RTOP200`, `RALL`, `NASDAQ`, `NDX`, `NBI`, `NYSE`, `DJIA` and
  `SPDA`; the `names` vocabulary gains, each `@1d`, `SPX`, `NDX`, `NDXTR`,
  `RUT`, `RUTTR`, `VIX_D`, `DJI`, `DJITR`, `RUA`, `RUATR`, `COMP`, `XCMP`,
  `SML`, `MID` and `OEX` (`NDX` is a list and a series: two vocabularies).
  [D] `VIX` stays the parity bundle's weekly close and the lake's daily
  `$VIX` is `VIX_D`: MW14 reads `series "VIX" @1w` over weekly rows
  (`rows[vix_rows]`), and a daily series under the same name would make
  its four rows four days on a store staged from the lake, silently.
  Rejected: the daily one as `VIX` with MW14 rewritten to a weekly view
  (the parity book is G1's and does not change for a store's naming).
  With it, a fixture's demand under `need lists` notes `industry` with
  `classification` (`abt-tests/tests/labels.rs`).
- **E-104 (D-2.4, D-7.7; the volume roll against fragment sessions, and
  spreads named as spreads).** RB's continuous series from the bucket held
  56,832 of 36,460,210 bars over 5,273 sessions and rolled 70 times in
  sixteen years (`2010-07-11 RBU0 -> RBZ0`, `2010-07-20 RBZ0 -> RBM1`): at a
  session in which only a far month printed (a fragment of a session, a
  thin holiday), the roll compared the contracts printing in that session
  by their volume in the session before, the far month was the only
  candidate and became the series, and the rule that a roll never goes to
  an earlier expiry kept it there. The roll now compares, in the latest
  session in which the contract held traded, the contracts that traded
  there and have not expired by the session's open; with nothing traded
  then, the contract held stays while it has not expired, and an expired
  one gives way to the nearest live contract printing. The 22,326,635 bars
  reported `no_definition` were spreads (the bucket's definition files
  hold the outrights only; 105 sampled days of RB, and samples of HO, CL,
  ZB, NG, GC and ES, show no outright bar without its definition): a bar
  whose symbol names a spread is now counted `not_an_outright_future`.
  The fragments themselves came from the sessions: a gap over
  `session_gap` (45 minutes) ended one, and RB's thin nights (51, 60, 82
  minutes without a trade around 04:00 UTC) split each trade date, 5,273
  sessions in sixteen years. A gap now ends a session only when it holds
  the middle of the venue's daily break, `session_break` (17:30 in New
  York, CME's break being 17:00 to 18:00 there, US daylight saving
  applied), as every gap between two trade dates does, a weekend's
  included; a long gap inside a trade date stays in it. On RB 2010-06-07
  to 2012-12-31 the series keeps 495,107 bars (20,140 before) over 664
  sessions with 31 monthly rolls, each to the contract d20-research's
  daily volumes name, 29 on the same day or the next and two within two
  days (near-equal volumes). [D] Rejected: synthesizing contracts for
  undefined bars from their symbols (no outright bar lacks a definition
  in the bucket); the previous session's volumes over every contract (a
  fragment's volumes still name the far month); a UTC window for the
  break (21:00 to 23:00 covers both offsets, and in summer its second
  hour is the evening's trading).
- **E-105 (D-7.2, D-7.4, A-D46, V-3.3; a daily futures program reads session
  bars).** A daily futures program (a `decide on session` clock, `@session`
  views, windows over sessions) streamed every minute bar of its roots,
  which made a house-universe plan of fifteen runs take most of an hour.
  The hot store's 1d ladder level cannot stand in: its buckets are UTC
  dates (E-89) and a CME session runs from about 22:00 UTC the evening
  before to 21:00 UTC, so a UTC day holds parts of two trade dates and
  `@session` views would change. What stands in exactly is the session's
  bar: the Databento adapter's `level session` (`--level session`) writes
  one bar per root and session from the held contract's bars (the first
  open, the highest high, the lowest low, the last close, the summed
  volume, back-adjusted as the minutes are), labelled at the session's
  close and received with its last bar, which is what a `@session` view of
  the minutes computes; its versions say `calendar = 1d` and `bars =
  session` (a day being the trade date's session, A-D46), under a source
  of their own beside the minutes. The resolver now orders a need's fitting
  versions coarsest first (the finest under `prefer base finest`), so
  `need bars @1d` binds the session bars and `need bars @1m` the minutes.
  `abt-tests/tests/session_level.rs`: Connors' Double Seven on two roots
  over forty sessions decides, fills and ends with the same cash, bit for
  bit, on either level, reading one close a session against eight. A
  finding the test makes visible and does not fix: no run over
  adapter-ingested contracts emits a roll (K-5.4's two fills and
  commissions), on either level. [D] Rejected: the UTC-day ladder level
  (not the session); building session bars in the kernel from the minutes
  (the stream would still carry every minute).
- **E-106 (V-3.5, K-4.3; a session sub-interval not yet begun).** `T1 in
  session(X, [a, b))` asked at a bar before minute `a` of its session
  (`[5min, 10min)` at a clock of `session + 3min`) aborted the kernel: the
  base bars' range started past its end ("range start is greater than
  range end in BTreeSet"). Such a window has no bar yet and is now empty,
  as the rule reading it then has no solution at that bar.
  `abt-tests/tests/session_window.rs`: the window is empty at minute 3 and
  read at minute 12, over minute bars the Databento adapter writes.
- **E-107 (compiler design 5.3; a time builtin on the sessions calendar
  ordered before its instrument).** `ms(A, T) :- session(A, T, _),
  month_start(T).` on a `decide on session` clock decided 6 times on ES
  2025 H1 through the served store with `--plan written` and never on the
  default optimised plan. The search took `month_start(T)` as reading `T`
  alone (the head's key, bound on entry) and placed it first, as the
  cheapest literal; the kernel steps it over the sessions of the head's
  instrument, which unbound there fell back to every instrument's
  sessions at once, where another root's close the same day preceded ES's
  month's first session. A time builtin (`prev`, `lag`, `month_start`,
  `day_start`) or a duration window on the per-instrument sessions
  calendar now reads the head's entity variable when a literal of the
  rule binds it, so the search places it after that literal.
  `abt-tests/tests/session_window.rs`: with ES and NQ closing an hour
  apart, both roots decide at their months' first sessions on the
  written and the optimised plans (the optimised one decided NQ's alone).
  [D] Open: under `--plan written` a rule that writes the builtin before
  the literal binding its instrument still steps over every instrument's
  sessions there.
- **E-108 (D-2.4, D-7.7; the daily break in CME's earlier hours).**
  E-104's defaults, a gap over `session_gap` (45 minutes, from close to
  close) that holds `session_break` (17:30 in New York), fit CME Globex's
  break since 2015, 17:00 to 18:00. ES's minutes show the earlier ones:
  until 2012-11-16 the equity indexes broke 17:30 to 18:00, the 17:29
  bar's close 31 minutes before the reopening's, a gap neither over 45
  minutes nor holding 17:30, where it starts; until 2015 they broke 17:15
  to 18:00, and a bar printing in its first minute (2013-07-08) left 45.
  ES's session bars over 2010-06-07 to 2026-09-30 were 3,693 sessions,
  weekly to 2012-11 (34, 58 and 81 in 2010 to 2012) and 16 of two days
  from 2012-12 to 2015-08; a volume roll then came a week late, the
  series holding the expiring contract through its expiry (ESM0 to ESU0
  on 2010-06-25, a week after ESM0's last day). The defaults are now
  17:45, inside every break since 2010, and 20 minutes, under the
  shortest's 31: ES has 4,200 sessions, one per trade date of the
  bucket's files (149 in 2010 from June, 250 to 259 a year after), none
  over 30 hours; its ten rolls to 2012-09 move to the Monday of the roll
  week (2010-06-14), the 56 after keep their dates. NQ and NKD have 4,200
  sessions too, GC 4,195, ZN 4,204 and RTY 2,385 from 2017-07, none over
  30 hours. The rule is unchanged, so an ingest by an earlier binary
  given `--session-break 17:45 --session-gap 20` stages the same
  sessions. [D] Rejected: a gap starting at the break's middle counted
  as holding it (17:30 would then serve 2010's break with no room, and
  the 45-minute gap would still keep 2013's print).
- **E-109 (ST-5.3, A-D46; a ladder level's index of its buckets).**
  Staging a minute relation's ladder looked each bar of a source level up
  in an index of that level's bucket keys rebuilt for the bar: from the
  15-minute level up, bars times labels, quadratic in a root's history.
  The pipeline's GLBX roots never finished staging (CL spent 23 minutes
  on one core in `ladder::index_of` after `future.open`'s presence, some
  17 hours for that relation's 15-minute level alone). Each level's index
  is now built once with the level and kept beside its labels: a lookup
  is logarithmic, and staging builds one index per level and relation.
  `abt-data/tests/ladder_index.rs`: 20,000 minute bars of one root stage
  with at most 200 indexes built (4,000 per relation at the 15-minute
  level before).
- **E-110 (P-2.1, P-3.1, P-3.4, D-3.4; the memory a request or a run
  freed).** The services handle each request on a thread of its own, and
  the kernel each run; glibc gives a thread a heap arena of its own (up to
  eight per core) and keeps the pages an allocation frees inside it for
  that arena's next one. Every arena kept the peak of the largest request
  or run it had served: the data service of the pipeline's store held
  18.2 GB of anonymous memory with a resident budget of 6 GiB, the Norgate
  futures one 10.5 GB with 2 GiB (98 arena heaps of 64 MB and 68 partly
  grown, 41 MB in the main heap), and a plan's trials grew `abt plan run`
  by gigabytes each. A request's thread now returns the free pages of
  every arena when it ends (`malloc_trim`, at most every 100 ms), and a
  run's thread when the run is released. A query over more than one atom
  of a run that ended no longer leaks the kernel it ran for the answer
  (`mem::forget`). The store's maps of open partition files and of
  manifests were never evicted: every run a service writes adds versions
  of its outputs and of its aggregates' cache objects, read once, and the
  Norgate service held 69,457 index maps; they start over past 8,192
  files and 4,096 manifests. And a schema listing without a class named
  every relation of the store, the runs' outputs, the query layer's
  records and the cache objects included: on the Norgate store 4,354
  schemas (3,376 `kernel.*`), a 1,050,903-byte response past the control
  payload's 1 MiB (P-2.2), so every binding (which asks it and never read
  those families) was refused. The class-less listing leaves `kernel.*`,
  `query.*` and `cache.*` out; asked by name or by class they are
  answered (`abt-data/tests/service.rs`). Those relations also wrote
  every row to the log as well as to their partitions: an aggregate's
  cache object logged 30 to 45 MB, 20 GB of the Norgate store's 22 GB of
  log in an evening, and no reader asks a log of them (only a hot
  version replays its tails from it). They write their partitions
  only. And nothing removed them: a store keeps the outputs of its last
  256 runs and its last 512 cache objects, the oldest past these (and an
  eighth more) dropped with their manifests, partitions and logs when a
  derived relation is written; a pinned version keeps its relation and
  the query layer's records (plans, lineages) stay
  (`abt-data/tests/store.rs`). A plan that reads a trial's outputs long
  after it ran, past 256 later runs, reruns it. `abt-tests/tests/service_memory.rs`: a
  cross-sectional strategy over 300 names and 1,000 days, run and
  released through the kernel service; of the 360 to 414 MB each later
  run takes, at most a quarter stays (146 MB before run 2, 508 at its
  peak, 102 after).
- **E-111 (ST-7.2, ST-8.4; an append at a finer exponent).** An append
  whose rows need a finer exponent than an instrument's stored ones (the
  Norgate equities of 2015 onto 1990 to 2014, `equity.open` at six
  decimals after three) rebuilds the instrument into a new run at the
  finer exponent, and the version keeps the previous segments for the
  instruments it did not touch, the rebuilt one's earlier run among them.
  Opening the version checked every run against the presence's exponent
  as it was read, before the new run superseded the earlier one, and
  refused the version ("instrument 0's `O` run is at exponent 3, its
  presence says 6"). The check now applies to the blocks each column
  keeps. It was not new: the staging that E-109 made fast reached the
  open in 138 s where it had not in 504. `abt-data/tests/hot.rs`: three
  instruments at three decimals; an ingest that is no pure append (one
  instrument's next days at six decimals, another's row revised) stages a
  version that rebuilds the first at the finer exponent and keeps the
  third in the previous segments; it opens and reads as its Parquet twin.
  (A pure append joins the tails under the published version and stages
  nothing; the first form of this test appended only and checked
  nothing.)
- **E-112 (Q-5.1; a claim on one side of an interval).** The house
  claim `survives_costs` (`bootstrap_sharpe on returns of base expect
  interval excludes 0`) held for a reliably negative Sharpe: an
  in-sample Sharpe of -0.36 with the interval [-0.70, -0.04] excludes 0.
  `excludes` holds on either side, as Q-5.1 defines it; a claim that the
  statistic is reliably above a value now says so: `interval OP VALUE`
  holds when every point of the interval does (its lower end for `>`
  and `>=`, its upper end for `<` and `<=`). The house claim reads
  `expect interval > 0`. `abt-query/tests/claims.rs`.
- **E-113 (D-2.5, WF-15; Cboe's volatility indexes).** The series
  vocabulary names the daily Cboe volatility indexes the full store
  stages as `series.value`: VXN, VVIX, VIX9D, VIX3M, VIX6M, VIX1Y,
  VIX1D, RVX, OVX, GVZ, VXEEM, VXEFA, VXEWZ and VXTLT at `@1d`, so that
  `need series "VXN" @1d` binds as `need series "VIX_D" @1d` does.
  `abt-tests/tests/needs.rs`.
- **E-114 (Q-6.2; a trade open at a window's start).** The trade
  metrics of a window (a reveal's out-of-sample bars, a plan's in-sample
  ones) paired the window's fills alone into round trips: a window
  opening on a held position read its exit as an entry, and the market's
  position count stayed off for the rest of the window, so none of its
  trades closed. A reveal over one market held across the boundary
  reported 0 trades (17 by its NAV), and markets held at the boundary
  vanished from the trade counts, profit factor, hit rate and per-entity
  rows, their P&L still in the net profit. The round trips are the
  run's, each the window's when the fill that closes it is, with the
  whole round trip's P&L. `abt-query/tests/trades.rs`.
- **E-115 (compiler design 3.8, 4.2, 4.6; a run at a parameter point).**
  The compiler read a parameter as its default: the analysis took a
  parameter without a range as the point of its default, so a guard on
  it (`side > 0`, `member_gate = 0`) never held there and R2 dropped the
  rule, and R6 folded an expression over it (`1 - weekly_tf`) at the
  default. A run at another point (`--param`, a plan's point, a grid's
  cell, `at { }`) then ran the default's rules: grids and variants over
  such a parameter repeated the default, silently (`weekly_tf * Dw + (1 -
  weekly_tf) * Dd` was 0 at every bar of a run at `weekly_tf = 1`; a
  rule gated by `member_gate = 0, only_coin = 1` derived nothing, while
  the explanation, evaluating at the point, said it fired). A compilation
  for a run now reads the run's point (`CompileOptions::params`, the
  program at that point for the analysis, the rewrites and the plan);
  the normal form and its hash stay the program's. `abt run`, the kernel
  service's runs (every plan's trials) and its answers over a run pass
  the point; `show plan` and `verify` without one describe the defaults.
  `abt-tests/tests/param_point.rs`.
- **E-116 (K-5.4; a roll between two barriers).** The executor rolled a
  held root only where a `continuous` entry fell on the barrier itself and
  changed contract from the entry before. A minute store's entries change
  contract at a minute inside a session: on a session clock no barrier
  saw a change, and a position held across some 34 quarterly rolls paid
  the commissions of its entry and its exit alone (the house charges per
  contract, per side and per roll). The executor now keeps each root's
  entry at the last barrier and rolls when the barrier's entry names
  another contract, with the two fills and two commissions K-5.4 states.
  A root's entry at a barrier is found by a search, not a scan of a
  minute store's entry per bar. `abt-tests/tests/futures.rs`: the
  entries six hours after each bar, the roll charged at the next
  barrier.
- **E-117 (K-2.1, W6; a ticker that lists after the bundle date).** A
  ticker literal resolved to the security carrying it at the bundle date,
  and none was refused: a plan whose instruments table names a root that
  lists after its in-sample end (`ETH_N`, from 2021, in a study whose
  in-sample run ends in 2017) refused to bind. A ticker no security
  carries at the bundle date now resolves to the first security carrying
  it after: the root enters the universe when it lists. A ticker carried
  at the date resolves to its carrier then, as before.
  `abt-kernel/src/kernel/mod.rs` (ticker_tests).
- **E-118 (Q-2.2; a compare step's metrics bracketed).** `compare NAME:
  RUNS by [m1, m2]`, as the house template writes it, took `[m1, m2]` for
  one metric's name and was refused; the list may be bracketed.
  `abt-query/tests/plan.rs`.
- **E-119 (K-3.5, K-9.3; a held position past a weight limit).** The
  invariant `inv.position_limits` checked every position of the book the
  bar's accepted orders would leave against a weight limit, the positions
  no order touched and those an order reduced included, while a `clamp`
  scales only what an order adds. A position bought at the bound that a
  price rise carried past it halted the run at the next bar with any
  order ("… 100977.45 after the accepted orders, above `weight <= 0.1 per
  name clamp`"). The limit being pre-trade, the invariant now judges the
  positions the bar's orders grow. Growth was then measured from the
  book before the bar's fills: an entry sized to the bound at a bar's
  close, filled at the next bar's open (`moo`) and marked at its close,
  counted as the next bar's growth, and a rise from close to close halted
  the run ("138863 at 103388.49 after the accepted orders, above `weight
  <= 0.1 per name clamp`": six posts of one evening, and a share added to
  a name already past the bound, which the clamp had in fact zeroed). The
  executor now states, with the book a bar's orders would leave, the
  instruments those orders grow from the book the limits sized them
  against, and the invariant judges those: the clamp holds at entry and a
  drift past the bound stands. `abt-tests/tests/limits.rs`: five names
  bought at a tenth, reduced by a share a bar while their prices drift
  past it; the same entered `moo` (under the growth from the book before
  the fills, the second halts at its first bar).
- **E-123 (ST-7.1; a rewrite of the kept runs dropped the segments it
  filled).** A new instrument whose id falls between stored ones shifts
  the kept instruments' ordinals, and S3 rewrites their runs at the new
  ones, block for block. The level's segment lists were reset to the
  rebuilt instruments' segments after the rewrite, not before it: a
  segment the rewrite filled to the rotation size (64 MiB) and finished on
  the way was dropped from the lists, its instruments' blocks with it, and
  the version did not open (S4 refused the ingest: an instrument with
  fewer rows in its runs than in its presence). Any store of more than
  64 MiB of a column's kept blocks met it at its first such ingest. The
  lists are reset before the rewrite. `abt-data/tests/segment_rotation.rs`:
  segments rotated at 4 KiB, 40 instruments of even ids, instrument 3
  ingested; every instrument reads its rows (under the mutation the
  ingest fails).
- **E-122 (ST-7, S2 to S4; an ingest's memory followed the store).** An
  ingest that is no append staged its version from every instrument's
  rows: S2 read each stored instrument back to plan it, kept or not; S3
  built the ladder level by level, every instrument's bars of every level
  held at once; S4 checked every instrument's bars against those rows. A
  Databento futures store's ingests grew 0.21 GB per million rows stored
  (29.4 GB at 50 roots and 137 million rows), whatever they wrote. S2 now
  reads back the instruments it rebuilds alone: a kept instrument's runs,
  sequence runs and exponents are the version's, its presence remapped
  onto the new labels, its buckets' counts taken from it. S3 builds the
  ladder instrument by instrument. A bucket is labelled at its latest base
  row of any instrument, so a kept instrument whose buckets keep their
  labels has the bars it had: their runs are the version's, in its
  segments, which each level's lists start from as the base's do (copied
  block for block where the ordinals shifted), and its rows are not read;
  at a level where the ingest moved one of its buckets' labels its bars
  are rebuilt from its rows, their runs superseding the ones before. S4
  checks a copied instrument's blocks and labels against the version's, a
  rebuilt one's bars against its rows. `abt-data/tests/build_memory.rs`:
  a new instrument and a revision ingested into stores of 6 and 96
  instruments read back the revised instrument alone (4,806 and 3,892
  rows; under the mutation every stored row) and leave every other instrument's
  blocks at every level the version's, the memory added 2 and 5 MB;
  `hot.rs::a_moved_bucket_label_rebuilds_the_kept_bars_under_it`: a new
  instrument later in five weeks than the stored ones moves those weeks'
  labels, the kept instruments' weekly bars there are rebuilt, and every
  read is the Parquet store's (under its mutation, bars copied under moved
  labels, S4 refuses the ingest).
- **E-121 (ST-7, ST-8.1; a rebuild drops the instruments appended since
  publication).** An instrument a hot relation's published version does
  not hold is, when ingested, an append: its rows join the overlay (the
  append files, the journal, the tails) under that version. A later
  ingest that is no append (a revision, a late row, the retry of an
  ingest killed mid-write, whose rows meet its own earlier ones) stages a
  new version from the published instruments and the input, S2 iterating
  the version's presence alone: an instrument in the overlay alone was
  never planned, and the version dropped it with every row it held. A
  Databento futures store, one version published for its first root and
  every later root appended, lost all roots but four from `high` and
  `low` at two retries (17.8 million rows of 102.4; `close` and `volume`,
  never rebuilt, kept them). S2 now plans the overlay's instruments too,
  rebuilt from their rows as read, and an input instrument the overlay
  alone holds is classified against those rows.
  `abt-data/tests/appends.rs`: one instrument published, a second
  appended, a revision of the first; both keep their 20 rows and the
  version reads as its Parquet twin (under the mutation the second keeps
  none).
- **E-120 (D-4, P-3.1; what one binding costs the data service).** A
  run's binding on a store of every US equity 1990 to 2026 took the
  service past 44 GB and the machine down (a 16 GB scope kills it, from
  2014 too). Three of its requests read whole relations: `Distinct` of
  every label column of the bound relations (`equity.member`, 183 million
  rows of every list: read, answered as strings, keyed row by row, three
  copies), `stats` (each version's figures opened every hot version of
  the store, the superseded ones of each ingest chunk included), and the
  contract's data checks (`pre.actions`, `pre.membership`: every close,
  every membership row). `Distinct` now reads a version bucket by bucket
  and keeps the values seen; the service's `stats` reports a hot version
  it has not opened from its manifest; `ABT_TRACE_REQUESTS=1` traces each
  request with its time and the service's memory. A universe whose every
  rule reads `member(A, T, "L")` of its instrument (or keeps one the book
  holds) is gated by those lists: its binding is scoped to the instruments
  they ever held and those the program names by ticker, and the binding's
  reads, its stream and the contract's checks read those alone (a scan
  whose predicate keeps some instruments reads their blocks through the
  store's identity filter); `pre.membership` reads the program's lists'
  rows. `abt-tests/tests/list_scope.rs`: a list-gated universe binds its
  three members of six and runs as the same universe through a derived
  relation, bound to every instrument, does. [D] Open: a hard bound per
  binding in the service. (The suspect race in
  `appends.rs::readers_at_a_sequence_keep_their_view_under_appends` was
  the test's: E-129.)
- **E-124 (K-3.1, Definition 3.2; N-hour bars over a minute base).**
  `need bars @2h` named no resolution (the lexer knows `@1m` to `@1w`),
  and no calendar constructor counted hours or minutes, so a rule on
  two-hour bars had no form: three posts blocked (`@2h`, a two-hour
  calendar, `decide on base at session + 2h every 2h`). A calendar
  `minutes(n)` (and `hours(n)`, `60n` minutes) groups a sub-daily base's
  bars into periods of `n` minutes counted from the epoch in UTC. A bar is
  labelled at its close, so a period holds the bars labelled in
  `(k·n, (k+1)·n]`: the 14:00 bar closes the 12:00 to 14:00 period. Its
  labels are each period's last bar, as a civil calendar's are; `x@c`
  reads its bar views (the first open, the highest high, the lowest low,
  the last close) and `decide on c` decides at those labels. It refines
  the minute calendars whose length its own divides, and no civil one (a
  period ending at midnight belongs to the next day). The checker refuses
  one over a daily base, or not a whole number of the base's bars; `@2h`
  stays no resolution, and its message names the calendar to declare.
  `abt-tests/tests/hour_bars.rs`: five days of ES minute bars, 13:00 to
  19:00 UTC; `hours(2)` decides at 14:00, 16:00, 18:00 and 19:00, and its
  14:00 to 16:00 bar is the first open, extremes and last close of the
  bars from 14:00 to 15:59 (under periods keyed on their start, the bar is
  one minute off). [D] UTC-aligned periods, not session-aligned: a
  session-relative N-hour clock is a later form.
- **E-125 (06-performance 3.2, Proposition 3.1; M-8, F-5.3; entity-parallel
  evaluation between barriers as built).** 3.2 describes the cut over
  entity-local strata of a set-at-a-time evaluator over column blocks; the
  tree's set evaluator still solves one environment at a time over a block
  of bindings (`eval_set.rs`, E-75), so the cut lands one level down, on a
  rule application: `Kernel::solve_block_maybe_parallel`
  (`kernel/parallel.rs`) runs the first literal of the plan's order on the
  main thread, cuts the bindings it produced by the entity they bind
  (`entity % threads`, so a window group stays on one worker across bars),
  solves the rest of the body on worker kernels, and merges their rows by
  the index of the binding each descends from (`Block::origins`) and their
  stuck instances by (literal, binding), which is the order the single
  thread lists them in. A rule takes the cut when no literal of its body
  reads the cross-section (`plan::planner::cross_sectional`), its head
  binds an entity, and the first literal bound at least
  `parallel_min_rows` of them (64; the suites set 1). [D] A worker is a
  clone of the main kernel sharing its stores and memo through `Shared`
  (an `Arc` the main kernel writes only while no worker holds a share: the
  workers give theirs back before the step returns) and owning its
  overlay memo, window groups and counters, absorbed after the step; the
  stores are sealed (every dirty block sorted) before a step. Rejected: a
  split of `Kernel` into shared and per-worker halves (every evaluator
  method's receiver would change for the same sharing). [D] Before a
  decision (`decide_at`, after its clock check: a bar that decides nothing
  warms nothing) the kernel derives, in stratum order, the open-loop
  derived relations the previous decision demanded (`warm_strata`,
  `note_demanded`; every one at the first), so that each rule's inputs are
  memoised when it is cut and no worker derives a whole relation on its
  own, and a relation a program reads at some bars only costs nothing at
  the others; a halt met there is memoised and raised by the decision that
  demands it, as the Kleene reading requires. Rejected: warming every
  open-loop relation at every barrier (R8L, deciding at one minute of each
  session, took 4.4x longer on four threads before the clock check). [D] A
  rule with a window literal takes the cut at any row count, so that each
  entity's window group lives on one worker for the whole run (cut at some
  bars and solved on the main thread at others, a group would exist twice
  and re-walk its window). The run does not depend on the thread count:
  `tests/eval_set.rs` holds the tuple evaluator, the set evaluator and the
  set evaluator on two and four workers equal on every corpus program and
  both books, decisions and ledgers; `tests/determinism.rs` one thread
  against four. `--threads N` (`ExecConfig::threads`, `1` by default, `0`
  the cores up to four) is not part of the run's identity (A-D11).

- **E-126 (06-performance 3.1, "Stored tables E[R] … over the retention
  horizon"; A3 of `docs/assessment.md`; the kernel's copy of the stream
  bounded).** The kernel kept every tuple the stream delivered
  (`Fold::step_inner` → `insert_fact`) for the whole run, so the front grew
  with the history streamed to it (MW14's run, E-93: 27 MB a second,
  killed past 8.5 GB) while the data layer held the same bytes. [D]
  `Kernel::trim_stores`, at every barrier after `evict_memo` when
  `ExecConfig::trim_stores` (`abt run --history window`, the default of
  that command and of no other path: the switch is off in `ExecConfig`
  itself and not serialised, the kernel service builds a run's
  configuration without it and `KernelRequest` has no field for it, so a
  run made by `abt plan run` keeps its whole history; change (a) under
  package 7 of `docs/handoff/roadmap.md`):
  every stored block keyed before the oldest bar any reader reaches from
  the barrier is dropped -- the memo's horizon, each `rows` window's count
  plus the slack a cold group walks back (`ROWS_SLACK + 3` bars of its
  calendar), the executor's own reach (`Executor::retention_floor`: its
  ADV and realised-volatility windows over its own bars, in whole days for
  a sub-daily decision calendar), the decision bucket's bars, less a week.
  A relation read `asof` keeps the latest row of each identity among the
  dropped rows (`Store::kept`, read when the as-of walk reaches the
  horizon). A program whose lookback is unbounded trims nothing; an
  executor that cannot state its reach trims nothing. [D] The history
  stays whole (`--history all`, forced for a ledger, the state tables and
  a live run) for a kernel read after its run, since a query at an old bar
  re-derives from the stores. Rejected: trimming by a fixed count of bars
  (the executor's ADV on a sub-daily base walks back a count of *days* with
  volume, which `tests/retention.rs` caught at `opening_gap`'s first fill
  behind the cutoff). The coverage report's "tuples in the data" counts
  rows ever inserted (`Store::inserted`). `tests/retention.rs` holds the
  trimmed run equal to the whole run on every corpus program and both
  books, alone and on four workers. The ADV walk now runs only when
  impact is on (sim/mod.rs; it ran and was discarded before). [D] A
  temporally recursive relation (a rule reads its head at a strictly
  earlier bar, the checker's own test: the total-return index the
  executor marks at, the cumulative split) reaches back to each entity's
  first row, so a name first priced long after its first row would walk
  behind the horizon (a served MW14 run on a thousand synthetic names
  expired its fills with `moc: no close`, the whole run did not): before
  the stores are trimmed the kernel derives every such relation at the
  bar for the whole cross-section (`Kernel::recursive_rels`), so each
  entity's chain is extended while its inputs are held; with the stores
  trimmed the memo is evicted at every barrier (what it would keep is
  unreachable, and its entries are keys, a few per bar, so the count
  threshold would never fire), and the eviction keeps each entity's
  latest tuples of each derived relation (`Memo::latest_rows`), which a
  derivation behind the memo's oldest bucket is answered from rather
  than from rows the horizon dropped. `tests/retention.rs` adds MW14 over
  120 names and six years (most names first priced long after their
  first row), whole against trimmed, alone and on four workers.
  [D] The observed relations' ledgers (`observe scale`, `observe
  latched`) were written after the run by re-deriving every bar
  (`outputs::ledgers`, `observe_at` per bar): under trimming that read
  1,900 bars of MW14 from trimmed stores -- the rows wrong, and every rows
  window walked from scratch at every bar (the run took twice as long,
  20M extra reads of `universe`). A run that writes outputs now observes
  at each barrier, before the trim (`Fold::observe_as_it_goes`,
  `outputs::observe_bar`, `ledgers_from`): the same rows in the same
  order, read while the bar's inputs are held. An observed relation off
  the decision calendar is still read after the run, and such a program
  keeps its history whole (withdrawn by E-167: it is read at the
  barriers too). The served comparison on the real MW14 holds
  `ledger.scale` and `ledger.latched` equal between the two histories.

- **E-127 (06-performance 3.3, the stream projection; E-77; the
  primitives nothing reads).** 3.3 says the stream carries only the
  relations the program reads outside pushdown aggregates; E-77 recorded
  that it carried every primitive the binding names. [D] The first step:
  `Kernel::read_relations` keeps every relation but the pure data a run
  can prove it never reads: the bar companions `open`/`high`/`low` when no
  rule names them, no decision can fill at the open or on a limit or stop
  (`orders moo|moo_moc`, or `moo`, `moo_moc`, `limit`, `stop` named in a
  decide rule's head) and the class has no contracts (an index option
  exercises at the AM print), and the identity relations `ticker` and
  `classification` when no rule names them; everything else streams,
  because the executor and the kernel read relations by name that no rule
  does (the settlements, the margins, the quotes: `tests/futures.rs` and
  `tests/options.rs` caught a projection that reasoned from the rules
  alone). `bind::stream_spec` streams a bound primitive only when it is
  kept. A book that fills at the close (MW14,
  `moc`) no longer receives its open, high, low, listing, tickers or
  classification (the listing since E-141 only: the code kept it until
  then). [D] Not done, and recorded for the next step: a
  primitive read only inside a pushed-down aggregate still streams (the
  E-47 fallback to local evaluation needs its rows), so MW14's `close` and
  `volume` -- the weekly `dvol` mean of daily close×volume has no
  data-layer form yet, and `traded@weekly` is a `count` the pass refuses
  -- stream whole; delivering pushed answers as stream relations and
  dropping the fallback is the spec decision 3.3 needs.
  `tests/fold.rs::the_stream_carries_the_programs_primitives_in_receipt_order_with_its_barriers`
  asserts the projected stream (`listed` read by the universe, `open` and
  `ticker` not). [D] The kernel also drops an unread relation's tuple at
  its door (`Fold::step_inner`, `Kernel::read_rels`): a live session
  streams whole, so the kernel service and the in-process replay hold the
  same rows either way (`abt/tests/deployment.rs`), and the front is
  bounded whatever the data layer sends. `threads`, `parallel_min_rows`
  and `trim_stores` are `#[serde(skip)]` on `ExecConfig`, as `pushdown`:
  none changes the result, so none is part of the configuration's
  identity (the binding table's `configuration` column, D-6.1).

- **E-128 (ST-8.8; a replay held the log after a consolidation).** Opening
  a hot version replays the log from `replay_from` into its tails. After
  a consolidation that is the least sequence any tail held, so one
  instrument's tail early in the log makes the replay read every later
  append, most of them sealed. A Databento futures store's consolidated
  `future.close` and `future.volume` replayed from sequence 11,139,746 of
  137,347,262. Each open read about 126 million rows (5 GB of arrows),
  concatenated them and normalised every row, then kept the few thousand
  after their instruments' sealed labels. An ingest of one root (GDK, 176
  contracts) grew from 1 GB to past 20 GB in that replay, and E-122's
  build never ran. The served futures service carried the same peak. The
  replay now reads the log batch by batch (`Store::each_log_batch`) and
  cuts each batch to the rows after their instrument's last sealed label
  before keeping it. The overlay never admits the others, and each
  instrument's labels are cut whole, so the tails are those of before.
  `abt-data/tests/replay.rs`: two instruments published, one with a
  three-row tail early in the log, the other with 5,000 rows appended,
  then a consolidation and a fresh open. The replay keeps exactly the
  tails' 907 rows of the 5,003 after its start, and every answer is
  unchanged; under the mutation it keeps all 5,003. [D] Open: the replay
  still reads the log after `replay_from` at each open; consolidation
  folding the tails into the base would end it. (Written in the
  amendments tree on Oct 20 as its E-125, a number PR #80 had taken for
  entity-parallel evaluation; the pipeline's `01-mvp.md:182` cites it
  under the old number.)
- **E-129 (ST-8.1's test; readers that never read).**
  `appends.rs::readers_at_a_sequence_keep_their_view_under_appends`
  appended a batch per write lock, took the lock again at once, and
  asserted at the end that its three reader threads had checked a
  snapshot. A waiting writer blocks new readers, so on a busy machine the
  readers reached the end having checked none: two failures in four full
  runs, then five in five alone. No read was wrong; the test checked
  nothing. The writer now waits after each append, up to 5 s, until a
  reader has checked a snapshot, so every append is interleaved with
  reads. (The amendments tree's E-126; this is the race E-120 left open.)
- **E-130 (06-performance 3.2; a slot map cached under a literal's
  address).** Commit `9a255d9` kept each atom's slot map (the position in
  the rule's environment that each of its terms binds) on the kernel
  under `(rule, the atom's address)`, on the comment that "a literal's
  address is stable for the run". It is not: a window's conjunction is
  reordered into a temporary on every evaluation (`eval::conj_order`
  returns a vector of cloned literals, at the three window paths), the
  temporary is freed, and the next one's atoms can land at the same
  addresses. Under one rule index `close(_, T, P1)` was then handed the
  map of `close(_, T, P)`, bound `P` twice and left `P1` unbound:
  `kernel.rs::quantile_conventions_and_the_aggregate_halt` panicked with
  ``Internal("variable `P1` unbound in rule cut::cut#1")`` in CI from
  that commit on. Whether an address is reused is the allocator's, so it
  was reproduced on Linux (the benchmark machine at `eb3345c`: three
  failures in three runs) and passes there with the cache gone; a pass
  on macOS says nothing. Because `cargo test` stops at the first failing binary, the 42
  `abt-tests` binaries after `kernel` and every crate tested after
  `abt-tests` did not run in CI from `9a255d9` to the fix. [D] The cache
  is removed and the map computed at each use, as before the commit: its
  own measurement was 114.2 to 113.1 user-seconds on 1,000 synthetic
  names for the three changes together, within noise. Rejected: a key
  that does not depend on an address (the rule and the literal's position
  in its body), since `atom_slots` is not handed the position and the
  window paths solve reordered sub-conjunctions whose positions are not
  the body's.
- **E-131 (06-performance 3.2; the block evaluator's first increments,
  as built).** 3.2 lowers a rule to a plan over column blocks. Until that
  evaluator exists (package 3 of `docs/handoff/roadmap.md`), four paths
  of the set evaluator answer for a whole block of bindings what `step`
  answered per row, each falling back to the row-at-a-time path when its
  form does not hold, and each yielding the rows the fallback would, in
  its order. (a) `eval::merge_join_atom` (`09f0c0b`): a positive atom
  with no constructor term and no inputs, over a block of at least 32
  rows that bind its entity in ascending order and its time to one bar,
  is demanded once, whole, and walked beside the block; a halt the whole
  demand meets, or any stuck instance at the bar, leaves the literal to
  the row-at-a-time path, which places the halt on the rows it belongs
  to. (b) `eval::prefetch_rows_window` (`5dd1a64`): when every row of a
  block asks a `rows` window at the same bar with the window's time
  free, the anchor and the conjunction are solved once over the block and
  each group is fed its own row's solutions; a row on which an instance
  stuck is solved alone. An anchor that is the conjunction's first
  literal is solved once, the rest of the conjunction going on from its
  block; and the solve is left out when every row's group is already at
  the bar (the second aggregate over a shared window, R3), since no row
  reads it then, provided it would only read: positive atoms, the derived
  ones memoised at the window's bar with no stuck instance, and at most a
  comparison or an assignment last (`KernelStats::prefetch_skipped`). (c) `eval::prefetch_bucket_window` (`479782a`):
  the bars the first row's group misses of a bucket or span window, each
  solved over the whole block. (d) `Kernel::call_first` (`eb3345c`): the
  executor's mark reads the one tuple of an entity from a memoised block
  of more than `SORTED_MIN` tuples by binary search, without building the
  result set; any other case is `call`. The loop over the block's rows
  remains in (b) and (c): each row's `step` reads the prefetched result.
  [D] These are increments under today's row-major `Value` blocks, not
  the plan of 3.2; `docs/assessment.md` credited them to E-125, whose
  text is the parallel cut. Held equal by `tests/eval_set.rs` on every
  corpus program and both books. Measured: 377 s to 367 s on the full
  MW14; 27.8 to 23.1 CPU-seconds on a served store of 1,000 synthetic
  names.
- **E-132 (K-3.4, the as-of join on a derived relation; a walk's answer
  served to another entity).** `R(...) asof T` on a derived relation
  walks R's own bars back from T and remembers, for every bar walked,
  the bar the walk ended at (`Kernel::asof_memo`). The memo was keyed by
  (relation, the relation's inputs, bar), without the atom's other bound
  terms, though the walk's answer is "the latest bar with a tuple
  matching this pattern": with the entity bound, one entity's answer was
  served to the next entity asking as of the same bar -- a bar where that
  entity has no tuple, or "no tuple at or before here" though it has one
  -- and it read nothing. PR #80 listed the hazard. No synthetic market of
  the suite met it (every name there has a row at every bar, and a read
  of the previous row is answered by the walk's first step, without the
  memo); a name with a missing bar does, which the real lake has: the
  full MW14 through the Norgate adapter decides 6,481 times with this
  batch against 6,455 on Nov 6 (E-130's fix is in the same batch, and
  the two are not separated). The key is now the atom's whole
  pattern with the time unbound. `abt-tests/tests/asof.rs::
  a_derived_relation_is_walked_back_for_each_entity`: two names whose
  only tuples sit at different bars each read their own from the second
  day on; under the old key the first name is handed, on the second day,
  the second name's "no tuple at or before the first day" and decides
  nothing, and nothing is decided after
  (`scripts/mutations/e-132-asof-entity.patch`).
- **E-133 (A-4, K-7.1, G3; wall-clock seconds in a run's outputs).**
  Commit `54b6640` put the fold's phase clocks (`insert_secs`,
  `fill_secs`, `decide_secs`, `exec_secs`, later `warm_secs`, `trim_secs`,
  `evict_secs`) in `KernelStats`, beside `parallel_secs` of E-125. The
  stats are serialised: they are the `stats` row of a run's `report`
  output (`outputs.rs`) and a part of every checkpoint. So two runs of one
  program over one binding wrote different `report` tables, against the
  identity of A-4 (the same run id, the same bytes):
  `outputs.rs::a_run_writes_its_outputs_at_every_checkpoint`, which holds
  a checkpointing run's outputs equal to a plain run's, failed from that
  commit on. It was not seen: `outputs` sorts after `kernel`, where E-130
  stopped every CI run, and the first whole suite since (the benchmark
  machine, Nov 25) found it. [D] Every `*_secs` field of `KernelStats` is
  `#[serde(skip)]`: `--timing` reads them from the result in process, and
  nothing stored or sent carries a clock. A run made through the kernel
  service reports no phase seconds to its caller; a resumed run's clock
  starts at its checkpoint. The clock gains what the fold did not time: `bind_secs`,
  `check_secs` (the preconditions), `push_secs` (the pushed aggregates
  asked of the data layer), `open_secs` (the subscription), `wait_secs`
  (blocked on the stream's next batch), `decode_secs` (a batch turned
  into tuples), `stream_secs` (the whole stream loop) and `write_secs`
  (the ledgers and outputs after the last barrier); and the stats gain
  `received_rows`, the tuples the stream delivered, counted at the
  fold's door (the rows the executor writes into the stores are not
  among them), which is not a clock and is serialised. Under
  `ABT_TIMING_RELS=1`: `phases bind= check= push= open= stream= write=
  wait= decode= …` (later with `build=`, `spec=`, `post=`, `inv=`,
  `observe=` and `loop_other=`, so that the parts of `stream` add up to
  it, and `insert=` timed once per batch) and `rows <relation> received=
  held=`; `tuples=` on
  the `timing` line of a served run is that count, where it printed 0
  (it counted the local dataset, and a served run has none).
  **Open:** the stats row still carries counters that follow a run's
  options and not its identity (`parallel_steps`, `parallel_rows`,
  `trimmed_rows`, `memo_entries`, `rows_state_groups`, the window
  counters, `pushed_answers`, and since E-141 `received_rows`, which
  counts the listing when `--ledger` names it), and `threads`, `trim_stores` and
  `pushdown` are not in the run id: two runs of one program over one
  binding write the same `report` only when made with the same options,
  and a second run with another thread count or history into the same
  store is refused at its write ("two rows of one identity ... differ").
  The same refusal meets a later build that decides differently on a
  store an earlier build's run was written to: `scripts/bench-mw14.sh`
  refuses a store that holds `kernel.*`.
- **E-134 (E-126, A-D56; a trimmed run and an entity absent for longer
  than the horizon) -- open, found by reading (Nov 25), not reproduced.**
  With the stores trimmed, a call of a derived relation behind the
  memo's oldest bucket with its entity bound is answered from the
  latest tuples the eviction kept for that entity (`eval.rs`, "Behind
  the memo's oldest bucket"), and the eviction keeps them for the
  temporally recursive relations only (`Memo::evict_before`). So
  `prev_row`'s `universe(A, T1) asof T0` finds nothing for a name whose
  last row is behind the horizon: `first_row` holds again and the
  name's chains (`row_number`, `cum_split`, `tr_index`) start over,
  where the whole history steps from the name's last row. Keeping the
  latest tuples of the relations read through `asof` as well is not
  enough: a rule that reads a non-recursive relation at the previous
  row (`total_return`'s `close(A, T0, P0)`) would then find `prev_row`
  and no close, and the chain would have no tuple from there on. The
  eviction has to keep the latest tuples per entity of every derived
  relation, which is a cost on a phase that is 17 s of the full MW14
  today. `tests/retention.rs` does not meet the case: every synthetic
  name has a row at every bar. To do with package 5 of
  `docs/handoff/roadmap.md`, with a retention case of a name absent for
  longer than the horizon and the count of such names in the lake.
- **E-135 (K-7.2, K-4.4, E-126; the memo cleared in mid-run under
  trimming) -- open, found by reading (Nov 25), not reproduced.**
  `Fold::restore` (a resumed run) and `Kernel::invalidate_from` (a late
  tuple) clear the memo. The trimmed-read path of E-126 takes the
  memo's oldest bucket as the sign that rows were dropped, and after a
  clear the oldest bucket is the first bar memoised since: an
  entity-bound read of a derived relation at any earlier bar is then
  answered with nothing though the stores hold its rows, and the
  latest tuples the eviction had kept are gone (neither is in the
  checkpoint). `abt run` keeps trimming on for `--checkpoint-every` and
  `--resume`. `tests/fold_checkpoint.rs` resumes with trimming off and
  the command-line resume tests compare the final cash. The fix is the
  eviction's cutoff kept on the kernel and in the checkpoint with the
  kept tuples, and read in place of the oldest bucket; it belongs with
  change (a) of package 7, which puts the horizon on the plan path,
  where runs are resumed.
- **E-136 (ST-8.3, E-90; the replay keeps the earliest row of a label
  where the append kept the latest) -- open, found by reading (Nov 25),
  not reproduced.** One appended batch holding two rows of an
  instrument at one new label (a bar and its correction in one file) is
  admitted as an append (`write_hot` tests each row against the overlay
  before either is pushed); `Tail::insert` keeps the later. At the next
  open `replay_tails` pushes the earlier and skips the later, since the
  tail then holds the label: the store answers the superseded value
  after a restart. E-128 did not change this (its cut drops whole
  labels). Such a batch should be a revision, as `admits` says.
- **E-137 (ST-8.4; a seal record's ordinal after a restart) -- open,
  found by reading (Nov 25), not reproduced.** A seal record stores its
  instrument's overlay ordinal; a restart numbers the unpublished
  instruments in the order of their first seal record. When an
  unpublished instrument sealed before one that arrived earlier,
  `apply_record` reports the mismatch and `replay_journal` cuts the
  journal there: the blocks recorded after stop being sealed, their
  rows come back from the log as tails held in memory, and E-128's cut
  has no sealed label to drop them by. Answers hold; the memory E-128
  bounds does not. No test has two unpublished instruments.
- **E-138 (D-4.1, D-3.2, P-3.1, E-120; what a binding asks is read from
  the columns that answer it, and every call it makes is clocked).**
  MW14's binding took 55.6 s of a 381 s run on the 1990 to 2026 store
  (4,240 instruments, relations of up to 15.69 million rows). `bind_at`
  asks some seventeen whole-history `distinct`s: the event time of every
  bound relation at the base calendar and every label column. E-120's
  read answers each bucket by bucket from rows: every column of every
  row decoded, a hot version's sealed append blocks once for each
  monthly bucket they overlap, every row keyed through a row converter
  into a set. [D] `query::execute` first asks `store_columns` for the
  answer from the columns alone, and reads rows where it declines. (1) The
  event time of a hot version: the published labels some instrument's
  presence marks in the window, and the labels of the appended rows (of
  a sealed block only its block of times is decoded; the tails' labels
  as they stand). It is the row read's set because a read cut by neither
  receipt, sequence nor identity gives a row for every marked label of
  every instrument, a revised label keeping its current row: a version
  with `revisions` needs no restriction. (2) The event time of a Parquet
  version: its time column alone (a projection), a row group whose
  statistics put it outside the window not read. (3) A label column of a
  Parquet version's key (the `Label` role, or a series' name): its
  column alone, in the order of each value's first row, a null a value;
  a row group whose statistics give one label is not decoded, and a
  partition the window cuts is read with its times. The revisions of a
  fact share its key, so the latest one is met where the first is. (4) A
  `scan` that names its columns, over a Parquet version: those columns,
  its predicate's, and the time and key that tell each fact's latest
  revision. Declined, and read as before: a receipt filter or an
  `as_of_seq` (a time's or a label's rows may all be cut); an identity
  under a `distinct`; a label that is a value (a revision may change
  it); any column but the time of a hot version, and a hot scan;
  partitions whose spans overlap, and a file whose times are not in
  order; a scan with `seq`, or one that names every column (the row read
  keeps what it decodes resident, E-74; the column reads neither fill
  nor use that cache). The answers are the same tables, row for row:
  `query::execute_rows` keeps the row path as the oracle and
  `query::shortcut` is the column path alone. Beside it: a predicate's
  constants are resolved once a batch, and an `=` or `in` of more than
  eight is a set (a label's code was looked up, and each constant
  compared, once a row); `bind_at` asks `stats` once. The clock: the
  kernel's `DataClient` notes every call it makes (`resolve`, `schemas`,
  `stats`, `query` with its form, window, predicate and projection,
  `aggregate`, `put`, `get`, `index`, `subscribe`) with the rows of its
  answer and its seconds, the last 4,096 of them; under
  `ABT_TIMING_RELS=1`, `bind_at` and the preconditions print theirs to
  standard error, `data <call> <relation> <detail> rows=<n> secs=<s>` a
  line and `data total <stage> calls= rows= secs=` after them. The log
  is the client's: no stored output carries it (E-133).
  `abt-data/tests/column_reads.rs` (each column read held equal to the
  row read on a hot version with superseded rows, sealed append blocks
  and tails, on its Parquet twin, on a membership with revisions and a
  null label, on partitions of two row groups, on an empty relation,
  over the whole history and windows; each cut declined),
  `abt-data/tests/query.rs::distinct_times_of_a_hot_relation_from_its_labels_equal_the_row_read`,
  `abt-tests/tests/list_scope.rs::a_client_logs_every_call_a_binding_and_its_preconditions_make`;
  falsified by `scripts/mutations/e-138-hot-times.patch`,
  `e-138-parquet-times.patch`, `e-138-key-labels.patch` and
  `e-138-scan-columns.patch`. **Open:** not yet measured on the
  benchmark machine. A list-gated universe's `list_members` still
  answers a row a fact where the instruments are wanted: `Distinct`
  takes no predicate on the wire. The preconditions' scans named no
  columns and read rows; E-139 has them name theirs.
- **E-139 (K-3.2, D-4.1, D-3.1, P-4; the preconditions read the columns
  they look at, a comparison no value passes reads no row, a whole hot
  read is ordered without a sort, and an event batch's relations are
  resolved once).** MW14's preconditions took 38.3 s of the same 381 s
  run. Each data check read its relation through `Bound::scan`: every
  column of every row sent, and a `ScanRow` made of each (its numbers,
  times and labels, three vectors a row) to read an instrument, a time
  and at most one value; the closes went into a map of maps, the listing
  into a vector and then an ordered set of its 15.69 million pairs, the
  membership's label was decoded and copied a row; and the price check
  read each of four hot price relations whole to find no row at or below
  zero. [K] `Bound::scan_columns` asks the same scan with its columns
  named (the instrument, the time, and the first numbers and times the
  check counts) and hands each row to a visitor, with no allocation a
  row. `contract::check::preconditions` reads through it:
  (1) `pre.prices` asks for the instrument and the time of the rows at
  or below zero, and is answered as (5) says. (2) `pre.actions` keeps
  the closes as a run of (time, close) per instrument and walks each
  instrument's splits and dividends beside its closes. Every
  instrument's closes are still read: a close that moves with no action
  to explain it is a problem of an instrument that has no action, so
  the check cannot be narrowed to the instruments that have one. (3) The
  listing is read once and not held. The membership (the program's
  lists' rows, E-120) is read before it, its instrument and time alone;
  each listed row marks the member rows it answers, in a structure the
  size of the membership (per instrument its member times in order and
  a mark each), and moves its instrument's last listed bar, which is all
  `pre.delistings` reads of the listing. So the membership is asked
  before the listing, where it was asked after: the calls change order,
  no status or warning does. The texts are the bundle functions' own
  (`action_problems`, `delisting_problems` and `membership_problems`
  share them), and the statuses, the warnings, their order and their
  first-five detail are those of the row path, which is kept as
  `preconditions_rows` and held equal to it. [D] (4) A scan, `exists` or
  `count` whose predicate has a conjunct made only of `<` and `<=`
  comparisons of number columns with constants every value of the column
  is above is answered as the row read answers a predicate that kept
  nothing, with no row read (`query::keeps_no_row`,
  `Store::least_value`). (5) The bound below a column's values: of a
  Parquet version, the least value the statistics of each row group
  state, over the partitions the window overlaps (a writer leaves a
  value that is not a number out of them, and such a value passes no
  comparison, as a null passes none); of a hot version, the least over
  its published blocks, its superseded rows (`revisions`), its sealed
  append blocks and its tails. A tick block's values on its grid are
  `decode_value` of the base plus a multiple of the quantum, which grows
  with the multiple, so the header's base bounds them without a word
  unpacked; a block with raw exceptions (ST-3.2) is bounded by their
  bits too. The bound is over every row the version holds, so it holds
  under a receipt filter, an `as_of_seq` and an identity. Not taken, and
  read as before: a row group without the statistic, or with nothing but
  values that are not numbers; a hot block or tail holding a value that
  is not a number; a column that is not `Num`; a tick answer (ST-10.4:
  the read alone knows its columns' types); a predicate naming a column
  the relation has not (the row read refuses it); any other operator.
  (6) `HotVersion::read_at` ordered a read with appended rows by sorting
  its rows' indices by (t, id, r, seq) read through them, and copied the
  rows out two vectors a row. The order is now made in two counting
  passes (`canonical_order`: by the rank of the instrument's id, then by
  the rank of the time, rows of one instrument at one time then sorted
  by receipt and sequence), which is the stable sort's order, and the
  rows are copied a column at a time; an ordinal that does not stand for
  one id falls back to the sort. [K] (7) `Decoder::events` looked every
  tuple's relation up by a copy of its name, copied its arguments' plan
  and found each cell's Arrow type at the cell. A batch's relations are
  now resolved at their first tuple in the batch, with each argument's
  column as the type it is read at, and a label's text is looked up once
  a batch and code; `entity_syms` is hashed by the kernel's `FxHasher`.
  The events are the same, in the same order, with the same labels
  interned in the same order; `Decoder::events_by_cell` keeps the former
  decoding as the oracle.
  `abt-tests/tests/contracts.rs::the_preconditions_give_the_same_statuses_and_warnings_from_columns_as_from_rows`
  (a market in order; one with a split the closes do not show, a close
  a dividend does not explain, a split on a bar with no close, an
  instrument that leaves the listing with no delisted record and stays a
  member; a zero close and a negative open; every corpus program on its
  golden market; scoped and unscoped bindings),
  `abt-data/tests/column_reads.rs::a_comparison_no_value_passes_is_answered_with_no_row_read`
  and `::a_value_that_may_pass_wherever_the_store_keeps_it_is_read_from_the_rows`
  (a hot version with superseded rows, sealed append blocks and tails
  and its Parquet twin, under windows, a receipt cut, a sequence cut and
  `seq`; a raw exception, a zero in a tail, a superseded zero, a
  negative value in a sealed block, values that are not numbers, nulls,
  an empty relation),
  `abt-data/src/hot/version.rs::tests::the_counting_order_is_the_stable_sort_by_time_identity_receipt_and_sequence`,
  `abt-tests/tests/fold.rs::a_batch_decodes_to_the_same_events_with_its_relations_resolved_once`
  (every corpus strategy's stream and a list-gated one's, a batch that
  interleaves two relations' tuples among them);
  falsified by `scripts/mutations/e-139-floor.patch`,
  `e-139-superseded.patch`, `e-139-listed-marks.patch`,
  `e-139-shared-key.patch` and `e-139-decoder-key.patch`. **Open:** not
  yet measured on the benchmark machine. A hot scan still gathers every
  value column, the receipt and the sequence of every row it reads, the
  columns it was not asked for among them: the closes of `pre.actions`
  are read so, and so is whatever a predicate that may pass reads. The
  listing is still sent whole (its instrument and time), as the
  delisting check needs every instrument's last listed bar.
  `report_periods` still reads `session` a `ScanRow` a row, for a
  sessions report calendar alone; a run whose report calendar is its
  decision calendar or `days` reads nothing there.
- **E-141 (06-performance 3.3, E-127; the listing stayed in the
  stream).** E-127's text says a book that fills at the close no longer
  receives its listing. The code kept it: `Kernel::read_relations`
  projected the companions, the tickers and the classification, and
  `listed` fell under "everything else streams", so a book whose
  universe is the names that traded (MW14) received a listing row per
  name and day and read none of them. [D] `listed` is kept when a rule
  names it (the standard universe does, and so does the one of the
  strategy the checker writes over a class's whole demand,
  `class_strategy_source`) or when an observation reads it
  (`outputs::observe_bar` reads an observed relation's rows at every
  bar); otherwise it leaves the stream and is dropped at the fold's
  door. Nothing else in a run read the kernel's listing: the
  preconditions scan it through the binding (`Bound::scan`), and the
  calendars are the binding's bars. In a run whose rules do not name
  the listing this changes the stream's specification (so the stream's
  id; the run's id is taken before it), `received_rows` in the stats
  row of the `report` output (`tuples=` under `--timing`), `trimmed_rows`
  there when the stores are trimmed (the listing's rows are no longer
  held, so none is dropped) and the coverage's count of `listed` tuples,
  which is 0; decisions, fills and the observations' ledgers are the same
  (the synthetic MW14 of 120 names: 900,625 tuples received become
  720,925, the listing's 179,700 fewer). [D] A command that reads the
  listing after the run names it before the run begins, and keeps it
  (`Replay::read_after`, `Kernel::keep_read_after`): `abt run --ledger
  listed`, an `abt query` form that names it (`ledger listed into NAME`,
  `why`, `why not`, `trace`; a `?` is a rule of the run and keeps it by
  the rule above) and `abt table --rel listed`. Such a run is the run
  before this change, every output with it; `received_rows` then follows
  the command as `trimmed_rows` follows `--history` (E-133's open part).
  The review of Nov 25 found `--ledger listed` writing a ledger with no
  row before this was done. The kernel service's queries name their
  relation once the run has begun (`service::answer_live` between
  barriers, `answer_held` after the run, a study's `ledger` step among
  them, whose `kernel.<run>.<NAME>` is a stored output): of a run whose
  rules and observations do not name the listing they read no row there,
  as one that names an unread companion has since E-127. The service
  could keep it only if the run's request named the relations its later
  queries read, which `KernelRequest` does not carry.
  [D] `Kernel::read_relations` now answers what the kernel keeps
  (`read_rels`, computed once by `relations_read`), so the stream's
  specification and the fold's door read one answer, and
  `Replay::keep_unread` keeps every relation of the binding (a test's
  switch, like `force_memo_eviction`).
  `tests/fold.rs::a_listing_no_rule_names_is_not_streamed_and_one_a_rule_names_is`:
  three books over one market, the stream of the first without the
  listing and those of the other two (an observation, the standard
  universe) with it, and the first book's run equal to its run with
  every relation kept;
  `abt/tests/cli_options.rs::a_ledger_of_the_listing_has_its_rows_when_no_rule_names_it`:
  `--ledger listed,close` of a book of the names that traded writes the
  rows the book of the listed names writes on the same market, and
  `abt table --rel listed` and `why listed(...)` answer as they do there.
- **E-142 (06-performance 3.3, E-127, K-7.1; an observation of a
  projected relation) -- open, reproduced (Nov 25).** E-127's
  projection counts the rules' atoms as readers and not the
  observations: `observe open` (or `high`, `low`, `ticker`,
  `classification`) in a program none of whose rules names the
  relation, and whose orders do not read the companions, writes a
  ledger with no row and no warning, where the relation has a row per
  name and bar in the data (a program of three names and thirty bars:
  84 rows in the data, 0 in the kernel and 0 in the ledger; `observe
  close` and, since E-141, `observe listed` write 84). The fix is the
  observed relations added to those the rules name in
  `Kernel::relations_read`; it changes the ledgers such a program
  writes, so it is left for a change of its own. A command that reads
  such a relation after the run meets the same: `abt run --ledger open`
  of `sma_crossover` (market orders) writes a ledger with no row, as it
  did before E-141. `Kernel::keep_read_after` keeps the listing alone, so
  that no ledger changes from what it was before E-141; the companions,
  the tickers and the classification belong to this fix.
- **E-144 (Proposition V-3.4, Definition V-3.5, V-7; a library rule's
  bar at a time other than its own).** V-3.4 gives every body atom
  outside a bucket aggregate the head's calendar, and V-7 makes a library
  relation calendar-polymorphic: on the calendar it is instantiated at, a
  bar primitive it names is that calendar's bar view (`link.rs`,
  `resolve_rrel`). The linker did so only for an atom whose key already
  had a calendar: the head's key, or a time bound by `prev`, `lag` or a
  window. A key bound by another atom's value, as `T0` in `prev_row(A, T,
  T0), close(A, T0, P0)`, was put on the base calendar (`infer_cals`: a
  bar primitive's fresh key on the primitive's own calendar), and the
  atom stayed the primitive. On a weekly calendar that is the daily close
  on the label's day, where `close(A, T, P)` in the same rule is the
  week's last close. A name with no bar on that day (a halt, a listing
  that begins or ends within the week) had its weekly row and its weekly
  close, and at its next row no previous close: no `total_return`,
  `price_return`, `gap`, `gap_points` or `true_range` there, and no
  `tr_index` from there on, since the index compounds through the missing
  return. MW14 fills and marks at `tr_index@weekly`: in the full run of
  Nov 25 (`1b3bf73`) 4,695 weekly rows had no close on their label day,
  330,952 of 3,190,176 rows (10.4%) had no price, 1,981 orders expired
  with `moc: no close`, and the canon held such a name in 1,101 weeks;
  a replica of the book with this one difference accounts for 3.95 of
  the 5.51 summed absolute weekly difference from the canon. [D] In a
  library rule a bar primitive has no calendar of its own, so its fresh
  key takes the rule's (`check/link.rs::infer_cals`) and the atom is the
  bar view at every time the rule names. A rule on the base calendar
  links as before, so no program deciding on its base moves: of the 26
  pinned hashes (`tests/golden/program_hashes.tsv`) only `mw14` and
  `r8l` change. What moves: `total_return`, `log_return`, `tr_index`,
  `price_return`, `log_price_return`, `gap`, `gap_points` and
  `true_range` on a calendar coarser than the base, at the row after one
  whose label has no bar of the name; for R8L that is `true_range@session`
  after a session whose label has no print of the root, and nowhere on
  the golden market (both books' golden runs are unchanged). Rejected:
  the view written in the library (a library rule cannot name the
  calendar it is instantiated on); a close carried from row to row in the
  library (a recursion for a value the view holds).
  `abt-tests/tests/label_day.rs`: two names over three weeks, one with no
  bar on the first week's Friday; its returns, its index, the bars
  module's steps and its fill at the second week's close, by hand, and
  the daily program's rules as linked before
  (`scripts/mutations/e-144-library-bar.patch`). **Open:** (1) the
  strategy's own rules keep the base reading: `prev_row(A, T, T0),
  close(A, T0, P0)` written in a weekly strategy is accepted and reads
  the daily close on the label's day, where V-3.4 would have WF-16
  refuse it as it refuses `close(A, T, P)`; no corpus program writes it
  (MW14's membership test names `close@base` for this reason, E-160).
  (2) With the stores trimmed (E-126) the view read at a previous row is
  a non-recursive derived relation read behind the memo, which is E-134's
  case: a name whose previous row is the bucket just evicted finds
  `prev_row` and no close, where the base close was still held.
- **E-145 (Definition V-3.2, V-3.3, V-7, data-bundle doc "Prices and
  actions"; a dividend going ex in a bar's bucket).** The catalog's
  `dividend_at` summed the dividends announced within the year whose
  ex-date was the bar itself, `Ex = T`. The data-bundle doc applies the
  cash dividend at its ex-date, and on a calendar C an ex-date at base
  bar e is an event of β_C(e), the label of its bucket (Definition 3.2),
  as `split_factor` already reads `split` on a coarser calendar: the
  latest split in the label's bucket (`link.rs::resolve_rrel`,
  `Read::Bucket`). On a weekly calendar only a dividend going ex on the
  label counted: in the parity run of Nov 25, 24,193 of the store's
  141,805 dividends (17.1%); the run held 684 dividend asset-weeks and
  missed 550, −0.122 on its holdings, and a replica of the book with this
  one difference has 386 weeks off the canon by more than 1e-4 and a CAGR
  0.45 pp lower. [D] `dividend_at` sums the dividends going ex after the
  calendar's previous bar and at or before T (`prev(T, T1)`, `Ex > T1,
  Ex <= T`): T's bucket as `bucket(C, T)` selects it
  (`eval.rs::window_times`), written without naming C, which a library
  rule cannot. On the base calendar that is the bar's dividend, as
  before; one announced on a bar and going ex on a day that is no bar (a
  weekend) now counts at the next bar, the first close that is ex, where
  it was in none. One keyed at such an ex-date (the Norgate adapter keys
  every dividend at its ex-date) is not: on a daily base a catalog
  relation's keys are base bars (`bind.rs`, the base domain), so the day
  becomes a bar; on the base calendar the dividend is then still lost,
  that day being the next bar's previous one, and on a coarser calendar
  the period is labelled at that day and the dividend counts against the
  cum-dividend close before it, a period before its first ex close
  (a probe on `dividend_week.rs`'s market with Y's dividend keyed at
  Saturday 2024-01-20: 0.5 at that week's label, 2024-01-20, against
  Friday's close; daily, keyed at 2024-01-13, none at Monday). The lake
  has no such ex-date (0 of 351,604, Nov 25). The calendar's first bar
  has no previous one and reads no dividend; no return steps into it.
  On a calendar finer than the
  actions' daily one (a sub-daily base deciding on the base, `hours(n)`,
  sessions) the window read `dividend` as of each of its bars (3.4), so
  one dividend stood at every window bar from its key on and was summed
  once per bar: 15.25 for 0.25 at the first minute of its ex-date, over
  61 bars (`Ex = T` gave none there, no minute label being midnight).
  The checker refuses an aggregate whose window reads a catalog relation
  as of the window's bars (`link.rs::lower_literal`, code X), so
  `dividend_at` and what reads it (`dividend_or_zero`, `total_return`,
  `log_return`, `tr_index`) do not link below the actions' calendar; on
  a minute base they are read on `daily`, where each bar of the window
  reads its own bucket and the dividend counts once. The refusal holds
  for every catalog relation (a count of `member` minutes too: derive
  the relation at the bar outside the window and aggregate that). No
  corpus program reads the catalog on a sub-daily base. No daily program
  moves: `low_volatility`, `total_return_momentum`, `momentum_12_1` and
  `short_term_reversal`
  (the corpus programs reading `log_return`) render their golden runs
  byte for byte as under `Ex = T`, their synthetic dividends going ex on
  bars. MW14's golden run moves: its first name's dividends go ex within
  the week, so its index and three of its fills change. The window cache
  (E-75) defers a test of the base time to the rows each call assembles;
  `Ex > T1` reads a time only the call binds, so the cache was keyed by
  it, one group per call (1,435 groups and 206,523 bars solved and held
  for five names over 300 days, where five groups and each bar once is
  the case). Such a comparison is now deferred too
  (`eval.rs::window_plan`: a total comparison reading a variable no
  other literal of the conjunction mentions), which E-75's conditions
  ("everything it reads is bound by the call or by the rows") already
  admit. `tests/dividend_week.rs`: a weekly book with a dividend going ex
  on a Tuesday, one on a Saturday and a split on a Wednesday, the daily
  program on the same market, the cache's groups, and the minute window
  refused (`scripts/mutations/e-145-dividend-bucket.patch`,
  `e-145-call-test.patch`, `e-145-finer-window.patch`).
  `tests/golden/program_hashes.tsv` changes for the programs linking
  `dividend_at`. **Open** (for dividends, (1) is E-162's and (2) E-163's
  and E-164's): (1) on a coarser
  calendar `split` and `dividend` are read as the latest key of the name
  in each bucket, so a week with two splits applies the later, and two
  dividends announced in one week on different days (Norgate announces
  on the ex-date) count the later; (2) the weekly total return is one
  period on the week's closes, `(P F + D) / P0 − 1`, where the book
  compounds the daily
  `(close + dividend) split / previous close` through the week, so a
  dividend going ex before the week's last day is reinvested at the
  week's close and not at the ex-day's: on `dividend_week.rs`'s X,
  0.0576923 against the book's 0.0579644 (2.7e-4, about D / P0 times
  the return from the ex-day to the week's close), and a dividend going
  ex after a split in the same week is per new share where the formula
  takes it per old share; parity to 8e-15 needs a daily total-return
  relation or a staged total-return close; (3) a dividend of a week in
  which the name has no row is lost, as the book, which has no bar for
  it, loses it; (4) below the actions' calendar `split_factor` reads
  `split` as of each bar, outside a window and so not refused: a split
  stands at every bar after it, and `cum_split` and `adj_close` apply it
  once per bar there (as before this erratum; no corpus program).
- **E-146 (`ExecConfig::start`, K-3.2's warm-up clause, V-3.4; MW14's
  start and its gate's history).** The book computes its features, its
  latch and its gate from the data's first week and trades from
  1991-01-04, flat before (`run_book`'s window over a panel built from
  1990). Two things kept the run from it. (1) A run with the stores
  trimmed (`--history window`, `abt run`'s default and the benchmark's)
  reads no row before its first decision bar: `Kernel::call` answers a
  read behind the memo's oldest bucket from what the eviction kept, and
  at the first decision bar that bucket is the bar itself (E-135's
  path). So with `--start 1991-01-04` every name's rows began there: in
  the Nov 25 run the 1,316 rows of bar 0 were all first rows, the first
  score came at bar 17 and the first 40-row mean at bar 39, and the book
  held nothing before 1991-05-03 where the canon latched 6, 11 and 23
  names at bars 0, 2 and 17 (a replica: 162 weeks off by more than 1e-4,
  CAGR 0.52 pp lower). `ExecConfig::start` says the bars before the
  start remain data, and K-3.2 asks the binding to cover the window
  plus the longest lookback so that the first decisions are warm. `--history all` warms up and does not fit the
  benchmark machine (docs/assessment.md, Nov 6). (2) The book's gate is
  `$SPXTR`'s weekly index against its 52-week mean over the index's own
  weeks from 1988 (`data.py` `attach_gate_index`, `book.py` 334);
  `rows[52]` over the weekly calendar starts with the equities, so there
  was no gate in 1990 (alone: 86 weeks off by more than 1e-4). [D] The
  run starts at the data's first week (`scripts/bench-mw14.sh`,
  `START=1990-01-01`) and MW14 holds its targets until `first_trade`
  (1991-01-04): its rows, latch and gate warm up on 1990, and from its
  first week it decides and fills what a run of the whole history
  started there does. The gate reads two weekly series the parity
  bundle stages, `SPXTR_W` and `SPXTR_W_MA52`, the book's index (daily
  ratios, weekly products and their running product in float32) and its
  52-week mean, from 1988 (`scripts/parity/mw14_series_bundles.py`,
  equal to the book's polars code to the bit), two new names of the
  series class; `gate_rows` leaves the program. `mw14_compare.py`
  aligns the two series on the decision week from the canon's first and
  reports the weeks decided before it, whose returns must be 0.
  Rejected: `--history all` (does not fit); a 1990 start without the
  guard (the book would trade in 1990 and enter 1991 holding drifted
  weights where the canon buys at 1 / N). E-135's fix, the eviction's
  cutoff read in place of the memo's oldest bucket, is the cure for (1)
  and makes the guard redundant, not wrong. MW14's golden market
  (2022-2023) is after `first_trade` and its synthetic `SPXTR_W` and
  `SPXTR_W_MA52` give the gate `rows[52]` gave, so the golden run's
  decisions do not move by this. `tests/mw14_parity.rs::
  a_run_from_the_datas_first_week_trades_from_the_books_first_week_as_the_whole_history_does`
  (without the guard, 56 fills before the first week);
  `scripts/parity/test_mw14_series_bundles.py`
  (`scripts/mutations/e-146-first-trade.patch`).
- **E-147 (V-3.4; MW14's weekly VIX keyed at the week's Friday).**
  `mw14_series_bundles.py` keyed each weekly VIX row at its week's
  Monday plus four days. The kernel labels a week at its last trading
  day and reads the series as of the label, so in a week whose Friday is
  a holiday the Thursday label read the previous week's VIX, and the
  program's own 4-row mean carried it for four weeks: in the Nov 25 run
  65 holiday-label bars, the scale on against off in 7 of them
  (1994-03-31, 1994-04-15, 2001-04-12, 2008-03-20, 2009-04-24,
  2009-07-02, 2025-04-17), −0.077 (a replica: 22 weeks off by more than
  1e-4, CAGR 0.32 pp lower). [D] Every weekly series row is keyed at
  its week's last `$SPXTR` date (68 weeks of 1988-2026 end before the
  Friday); a VIX week with no `$SPXTR` date keeps its Friday (none in the
  book's files). `VIX_MA4` is staged from the mart's own column and
  `calm` reads it, as the book does (`vix_rows` leaves the program).
  `scripts/parity/test_mw14_series_bundles.py::
  test_a_holiday_week_is_keyed_at_its_last_trading_day` (under the
  Friday keying, 2008-03-21 for 2008-03-20). No mutation: the test is a
  script's, and runs only where pandas is. **Open:** a week whose last
  equity bar differs from its last `$SPXTR` date reads the week before.
  The data's last week is one: the lake's equity bars end on Wednesday
  2026-06-24 and `spxtr_daily.parquet` runs to 2026-07-09, so that
  week's four rows are keyed at Friday 2026-06-26 and the run's last
  label, 2026-06-24, reads the previous week's gate, VIX and `VIX_MA4`
  where the book reads that week's. It decides only the book's last
  week, cost only, which is not compared; keying a partial last week at
  the equities' last day would take the lake into the series script, and
  is not done. Before it there is none (Nov 25: no equity day off
  `$SPXTR`; the 26 `$SPXTR` days off the lake are all from 2026-06-25).
  `mw14_compare.py` checks it per run (`label_key_misses`): every label
  but the last is an `SPXTR_W` key (`labels_off_key`, 0 expected), and
  the last is reported when it is not (`last_label_off_key`, 2026-06-24
  expected); `test_mw14_series_bundles.py::
  test_every_label_but_the_last_is_a_weekly_key`.
- **E-148 (MW14's target weight, `engine_weighted.py`).** `target` was
  `least(S / N, name_cap)`; the book takes the share 1 / N (unit weights
  over their sum), times the scale, then the cap, as the first
  formulation did (`docs/parity-mw14.md`, R4, "in the book's order of
  operations"). With the dial's 0.7 the two part in the last bit for N
  of 9, 13, 18, 21, ...: 187 of 1,744 canon weeks in the Nov 25
  comparison, none by more than 1e-9, but a weight at the band's edge
  can flip a trade. [D] `W = least(1 / N * S, name_cap)`.
  `tests/mw14_parity.rs::the_target_is_one_over_the_count_times_the_scale_capped`
  (under `S / N`: 9 names at 0.7, 0.07777777777777778 against the
  book's 0.07777777777777777; `scripts/mutations/e-148-target-order.patch`).
- **E-149 (D-7.7; `docs/handoff-data.md` section 4, decisions 1, 2, 3
  and 9; the Norgate adapter read as the Python book reads the lake).**
  Four readings the adapter made per ingest span or against the book,
  measured on Nov 25 (D4, D5, D8 of the parity measurements). Each new
  reading is an option of the adapter, default unchanged, and
  `scripts/bench-mw14.sh` passes all of them (its store key names them).
  (a) The universe was the members of the span (`norgate.rs` 1). Staged in
  five-year spans, a span lacked every bar of an asset not a member in it:
  4,437,783 of the lake's bars from 1990, of 2,303 of its 4,240 assets (a
  replica: 192 weeks off by more than 1e-4, CAGR 0.34 pp). [D]
  `universe_from` and `universe_to` (`--universe-start`, `--universe-end`)
  name the window whose members are the universe; the member rows are
  unchanged. (b) With such a window past the span, an asset whose last
  print is the span's last trading day was taken for a current listing,
  and the next span, holding no bar of it, emitted nothing: never
  delisted (decision 3 says the first trading day after the last print).
  [D] The lake's next year is read for the first trading day after the
  span and the assets that trade again; one that does not is delisted on
  that day, as that day's row alone (the next span cannot carry it). A
  span read alone, or the last of its window, is read as before. (c) The
  lake's `member_to` is the first trading day out
  (`pipeline/ndlake/membership.py`: "the trading day AFTER the last member
  day"), as decision 2 reads it; the book counts a name a member when
  `member_from <= its week_end <= member_to`. And MW14 read `member` at
  the week's label: `sp1500` is derived for the week's names at once, and
  a bucket read with the entity unbound answers the bucket's last day with
  any row (`Kernel::asof_matches`), so a member without a bar on the label
  day was out that week. Nov 25 counted 578 store-only and 23 book-only
  member asset-weeks (113 weeks off by more than 1e-4), under the
  measurement's reading of any row in the week; in process the store-only
  direction (out on a Wednesday, trading to Friday) does not reproduce.
  [D] `member_to = last_day` (`--member-to last_day`) counts `member_to`
  as a member day, stated on the member version, and the universe's test
  reads it so: a spell whose last member day is the window's first is in
  the universe (`member_to < from`, where the half-open reading drops
  `member_to <= from`; before, such an asset had no bar and no member
  row that day, as `two_span_lake`'s 16 in a span from 2015-01-05; not
  met by `bench-mw14.sh`, whose window starts on a holiday). MW14 tests
  the three lists at the name's own last bar of the week (here
  `sp1500_day@base` and its `last` over the bucket; since E-160
  `sp1500@base` read in `eligible` at that bar). (d) Decision 9
  rounded prices and dividend
  amounts to six decimals; the lake's are single-precision floats widened
  (2017: none of 976,389 closes off `f32`, 909,946 off the 1e-6 grid): on
  Nov 25 10,369,369 closes and 133,440 amounts differed from the lake's,
  779 weeks by more than 1e-9 against the 8e-15 gate. The storage
  specification has the store give the vendor's bits (07-storage, 2.1 and
  "Floats on read"). [D] `values = lake` (`--values lake`) keeps them,
  stated on the versions; the hot store holds a value off every decimal
  grid as its raw bits (A-D54), exact in the `f64` view through a staged
  version and an append, without an exact view or a ladder, about ten
  bytes a value (inferred, not measured). Tests: `abt-data/tests/
  norgate.rs::a_staging_in_spans_holds_every_asset_of_the_universe_window_in_every_span`,
  `::an_asset_whose_last_print_is_a_spans_last_day_is_delisted_on_the_next_spans_first`,
  `::member_to_counted_as_a_member_day_is_the_books_test_at_the_names_last_bar_of_the_week`,
  `::the_lakes_values_reach_the_store_bit_for_bit_through_a_staging_and_an_append`;
  `abt-tests/tests/mw14_parity.rs::a_name_is_a_member_in_a_week_when_it_is_one_at_its_last_bar_of_the_week`
  (before: S05, halted on the label day, out of `eligible`). Mutations
  `scripts/mutations/e-149-{universe-window,span-end-delisting,member-to,member-to-window,lake-values,member-week}.patch`.
  Other programs: no default moves; the corpus's synthetic market has a
  member row on every bar, so MW14's golden run is unchanged by (c) and
  only its program hash moves. **Open:** the `f32` form (`floats = f32`
  on the price versions, 07-storage's design for NDLake, no exception)
  is not used: `staging/build.rs::value_int` (the ladder) and
  `store_hot.rs::tail_row` (appended rows) take the `f64` grid test for an
  `f32` column, so they would count such values as 0 (found by reading).
  A bucket read whose answer depends on whether its entity is bound is
  the evaluator's, not fixed here. A split day's dividend (decision 6,
  D7): the adapter stores it per new share (`amount / factor`), the
  library's `total_return` adds it to the old-share price (`P F + D`), and
  the book computes `(P + D) F` with the lake's amount; the lake's 42
  split-day dividends put the three apart (MSFT 2003-02-18: the book
  3.685%, the engine 3.437%; CSR 2018-12-28, a 1:10 reverse split: 4.04%
  against 144%). Not changed: none fell on a held name's label on Nov 25,
  and the vendor's convention (per old or new share) is the owner's call.
- **E-160 (E-149 (c); MW14's membership at a name's last bar of the
  week, at the cost of a weekly test).** E-149 wrote the book's test as a
  daily relation, `sp1500_day@base`, derived at every base bar for every
  name with a close (about 9,300 bars), and `sp1500` as its `last` over
  the week's bucket: exact, and in the run of wave 1's tree on m8-bench2
  (`p5res-1`, 337.2 s) 76.5 s of it (`sp1500` 47.8 s, `sp1500_day`
  28.7 s) for a book that decides weekly. [D] `sp1500@base` is the union
  of the three lists at a day, and `eligible` reads it at the name's last
  bar of the week, bound as the key of its latest daily close as of the
  label, `close@base(A, L, _) asof T`, after the two floors. The read names
  the base calendar, so `L` is a base time and `sp1500(A, L)` a plain read
  at `L` whatever becomes of E-144's open item (1) (a bare `close` would
  read `close@weekly` there if that item is closed as V-3.4 reads, and `L`
  would be the label); the as-of read of a stored
  relation with the name bound (`universe(A, T)` binds it first, and the
  schedule and the planner keep it first, since the as-of reads `A`)
  probes the label's day and, for a name without a bar there, the days
  before it, at most the week's. `sp1500` is derived only at the bars it
  is read at, the label's day and the last bars of the names halted
  before it. The same answers: `member_from <= week_end <= member_to`
  with `week_end` the name's own last bar (`data.py` `to_weekly`,
  `add_membership_and_universe`). Measured on m8-bench2 against the
  parity store, fingerprint unchanged (`6e72c9bb…`): 251.6 s
  (`p5b-member-2`), `sp1500_day` gone, `eligible` 18.2 s against 16.1 s
  in `p5res-1` and 15.7 s in `p5b-member-1`, and `sp1500` under 2.1 s (not
  among the slowest sixteen relations printed; about 1 s by the ratio
  measured in process): about 3.3 s for the test, at most 4.6 s, where it
  took 76.5 s. Measured on the way: the same read in a weekly `sp1500`, the
  three lists in one case form (`universe(A, T), close(A, L, _) asof T,
  F = case { member(A, L, "SP500") -> 1 ; ... }, F > 0`): 258.5 s, that
  relation 8.8 s (`p5b-member-1`). MW14 no longer reads `member` in a
  bucket but in the universe's ETF test, so E-149's open item (a bucket
  read's answer depends on whether its entity is bound) no longer
  touches its membership. Rejected: the week's latest member row with
  the name bound (`universe(A, T), member(A, T, "SP500")`, a bucket
  read), which keeps a name whose membership ends mid-week while it
  trades on, the store-only direction of Nov 25's 578 asset-weeks; the
  time of the last bar as a bucket aggregate (`max(T1) over (T1 in
  bucket(weekly, T), close(A, T1, _))`), which walks the week's daily
  rows per name in the evaluator, since the pushdown takes only an
  aggregate of a value column (`pushdown.rs`, condition (a)), as
  `dvol`'s bucket mean does in 48.6 s; the label's day bound once per
  week (`bar@base(L) asof T`) with the names halted on it in a second
  rule behind `not close(A, L, _)`, in process a third cheaper than the
  case form and no cheaper than this formulation; a change of the linker or the
  evaluator, which the language does not need for this. Tests:
  `tests/mw14_parity.rs::a_name_whose_last_bar_of_the_week_is_wednesday_is_a_member_by_its_membership_on_wednesday`
  (no bar after Wednesday, the membership ending on Wednesday, Thursday,
  Friday or Tuesday),
  `::eligible_holds_the_names_above_the_floors_that_are_members_at_their_own_last_bar_of_the_week`
  (40 names halting for one to four days and moving between the lists,
  into two and out of all of them, on every weekday: 140 weeks and 4,456
  eligible name-weeks against the floors of the program without its
  membership test and the book's test computed from the facts, where a
  read at the label parts from it in 81 name-weeks and any member row of
  the week in 69; the optimised plan over trimmed stores decides and
  fills as the written plan over the whole history). Mutations: the
  label read (`scripts/mutations/e-149-member-week.patch`, rebased on
  this) and any member row of the week
  (`e-160-member-latest.patch`) each fail
  `a_name_whose_last_bar_of_the_week_is_wednesday_is_a_member_by_its_membership_on_wednesday`,
  `eligible_holds_the_names_above_the_floors_that_are_members_at_their_own_last_bar_of_the_week`
  and E-149's `a_name_is_a_member_in_a_week_when_it_is_one_at_its_last_bar_of_the_week`.
  Other programs: only MW14's program hash moves; its golden run is
  unchanged (the corpus's synthetic market has a member row on every
  bar). **Open:** the answer depends on the literals' order, which
  Proposition V-5.5 says it must not: written first in the body, the
  as-of read is planned as the generator with `A` unbound, and
  `asof_matches` then stops at the latest key with any close, the label's
  day, so names halted on it leave `eligible`. `abt check` accepts that
  order with no warning. Either the evaluator walks back per entity when
  the entity is unbound, or the checker refuses an as-of read whose entity
  no earlier literal binds (E-149's open item, a bucket read's answer
  depending on whether its entity is bound, is the same defect).
  writes, so it is left for a change of its own.
- **E-150 (06-performance 3.1, 3.2; the block evaluator's seam and its
  first plans).** 3.2 compiles every rule to a block plan run stratum by
  stratum over column chunks that replace the stores and the memo
  (3.1). [D] The block evaluator is built at the miss of `Kernel::call`,
  one (relation, bar) at a time, beside the row path, before any typed
  store: under `--evaluator block` (`ExecConfig::block_eval`, not in the
  configuration's identity) a derived relation with one entity argument
  and no input, open-loop and not temporally recursive, whose every
  rule's literals in the plan's order are a first atom at the bar
  (`Scan`), later atoms at the bar with the entity bound (`Join`),
  assignments (`Let`), comparisons (`Test`) or, first, an aggregate the
  data layer answers (`Pushed`; after the entity's atom, `PushedJoin`,
  E-175), is derived over the bar's columns
  (`kernel/colplan.rs`), its expressions compiled once
  (`kernel/cexpr.rs`) over the one arithmetic `eval_expr` applies
  (`bin_value`, `call1_value`, `least_greatest`, `compare_num`). An
  operator declines the (relation, bar) to `derive_rules` wherever it
  could not do exactly what the rules do: an input that is not one tuple
  per entity in entity order (a result of 16 tuples or fewer from
  several rules is in derivation order), a partial operation on any row
  (the rules then raise or exclude the instance), a pushed answer not
  yet received. No plan runs and nothing is read as columns behind
  `Kernel::horizon_floor` (the last cutoff of the eviction and of the
  trim), before the memo's oldest bucket when the stores are trimmed,
  or after the memo is cleared under trimming until the next trim, so
  E-134 and E-135 keep today's answers. A plan's literal counts and the
  pushed answers it read are the kernel's only when it does not
  decline. Its result is memoised as tuples built from its columns, at
  once (every planned relation of MW14 is also read as rows), and its
  columns in a block memo inside `Memo` (`kernel/col.rs`), which the
  eviction, the clears and the workers' shares cover and which
  `Memo::len` does not count; another relation read as columns has its
  result turned into a block once, kept there; a stored relation's bar
  is read into columns at each read, not kept. A column holds one kind
  unboxed, or the values when its rows' kinds differ (`split_factor` is
  the Count 1 where there is no split and a number where there is one).
  Only the main kernel plans; a worker derives by the rules. Stage 3.5
  of `docs/handoff/roadmap.md` is not built and typed stores (3.1) come
  last. On one thread the `report` output's stats row, the coverage and
  the literal counts equal the set evaluator's on every corpus program
  and on MW14 (below); on worker kernels a planned relation is not cut,
  so `parallel_steps` and `parallel_rows` follow the evaluator as they
  follow the thread count (E-133's open part). `tests/eval_set.rs`: six
  legs (tuple; set on 1, 2, 4 workers; block on 1 and 4 with every
  relation without a plan served through its columns, the identity
  plan of `ExecConfig::block_identity`) on every corpus program and both
  books; `the_block_evaluator_counts_what_the_set_evaluator_counts_on_every_corpus_program`;
  `the_block_evaluator_derives_what_the_set_evaluator_does_on_mw14_with_holes`
  (120 names over six years, four names missing bars and one absent 60
  weeks, written and optimised plans, whole and trimmed, 1 and 4
  workers; `adj_close`, `close@weekly`, `scored`, `stay`, `latched`,
  `extension`, `in_book` planned and derived through their plans, and
  since E-175 `traded@weekly` and `dvol`; `dollar_volume@base` has a
  plan and, its weekly mean delivered since E-157, is not demanded);
  `compiled_expressions_give_the_row_paths_values_and_halts`;
  `tests/retention.rs` with block legs; unit tests in `col.rs` and
  `cexpr.rs`. Falsified: `scripts/mutations/e-150-partial-declines.patch`
  (a division by zero over a column gives NaN instead of declining).
  Measured on the full MW14, one cold run each under `--evaluator
  block`, the weekly returns' fingerprint unchanged: the seam with no
  plan 282.3 s (280.2 s without the flag); with the plans 274.3 s,
  `dollar_volume@base` 8.1 s to 1.2 s, `adj_close` 1.6 s to under
  0.9 s, `decide` 119.3 s to 110.2 s. Every planned relation derives
  through its plan at every bar it is asked for but `dollar_volume@base`
  at one, and `latched` and `in_book` at 157 of 1,852, where the latch
  holds 16 names or fewer in its rules' order.

- **E-151 (06-performance 3.2, 3.4; the block evaluator's rows
  windows).** 3.4 plans a window as an operator over the column chunks
  of 3.2. [D] Under `--evaluator block` a `rows[n]` aggregate is an
  operator of its relation's plan (`ColOp::RowsAgg`, `kernel/colops.rs`)
  when its group is the rule's entity and is bound by the plan before
  it, its count and minimum are constants, its conjunction in the row
  path's solve order is atoms of derived relations at the window's bar
  with that entity, the anchor first, each binding fresh variables, its
  arguments are expressions over those and the rule's other bindings,
  and the numerics are two-pass (under running sums no window has a
  plan). The operator keeps each entity's window state where the row
  path keeps it (`eval::EntRows`, a table per window dense by entity,
  which the row path uses too) and brings it to the bar with the row
  path's functions (`RowsState::feed`, `window`, `trim`,
  `Kernel::rows_serve`); a bar the state lacks is read from the
  conjunction's relations there as columns (`Kernel::cols_of`), each
  fetched when an entity first reaches it, as the row path demands it;
  over a frame of 32 rows or more the bar's own come first, the anchor's
  and then each later atom's while an entity of the frame is in the
  ones before, as the row path's solve of the bar over the whole block
  (`prefetch_rows_window`) derives them before it brings any entity
  over older bars, so that a relation of the conjunction with a window
  of its own is asked at those bars after this one, behind its groups'
  last bar, as there (a bar whose own relations are not columns
  declines). The arguments are computed by the arithmetic `eval_expr` applies
  (`CExpr::value_in`: the same logarithm, division and Count
  conversion). Where the columns cannot answer an entity as the row
  path would (no state yet, whose walk back is the row path's; a bar
  behind the horizon, or whose relation is not one tuple per entity; a
  partial operation on its row; a request behind its state; a window
  its buffer does not hold; a reduction that halts) the operator runs
  the row path's step for that entity alone, which goes on from the
  state where the columns left it. A step that halts declines the bar:
  the steps run before it are kept by their bindings
  (`Kernel::rows_replay`) and `derive_rules` meets each outcome as it
  was instead of running the step twice, so the bar's window calls,
  bars solved, exclusions, coverage and literal counts are the row
  path's. A bar whose own relations are not columns there, or hold a
  stuck instance or a halt, declines whole. A rule with such an
  operator is not cut across worker kernels under the block evaluator,
  so that each entity has one state whether a bar is planned or not.
  Assignments and comparisons of constants before the plan's first atom
  are operators too (`clenow` binds `P0` first). `ma` and `clenow` are
  planned (`clenow`'s two regressions keep a state each, as the row path
  keeps them: the plan shares none between them); `eligible` was until
  E-160 gave it an as-of read of a stored relation, which no operator
  plans, and derives by the rules.
  `tests/eval_set.rs`:
  `the_window_operator_feeds_what_the_row_path_feeds_under_every_asking_pattern`
  (windows asked at every bar, at some, first late; a mean, a count
  with a minimum of one, two regressions over `log(Q / P0 - x)` and a
  Count; whole and trimmed, 1 and 4 workers, the counts and the stats
  row on one; a price planted where the logarithm is partial declines
  the bars that read it and leaves the same stuck instances),
  `the_window_operator_derives_a_windowed_relation_in_the_row_paths_order`
  (a window over a windowed relation asked at month starts over 40
  names, the memo kept and evicted at every barrier, whole and trimmed:
  the stats row with its requests behind a group's last bar, and the
  coverage), and the MW14 test with holes with `ma` and `clenow`
  planned (and `eligible`, whose as-of read E-152 plans);
  `tests/retention.rs` block legs; `kernel/eval.rs`
  `rows_state_tests` (the flat state against the deque it replaced).
  Falsified: `scripts/mutations/e-151-window-replay.patch` (a declined
  bar's steps run twice: the counts differ) and
  `scripts/mutations/e-151-prefetch-order.patch` (the bar's relations
  read only as entities reach them: the inner windows are asked in
  order, and the stats row differs). Measured on the full MW14,
  one cold run each, the weekly returns' fingerprint unchanged: the row
  path's state dense and flat, without the flag, 258.5 s (280.2 s
  before; `clenow` 34.6 s to 21.0 s, `ma` 15.7 s to 8.4 s, `eligible`
  13.1 s to 8.8 s); under `--evaluator block` with `ma` and `eligible`
  planned 239.4 s (3.4 s and 3.3 s), with `clenow` too 226.6 s
  (`clenow` 8.3 s, `decide` 62.3 s), and with a new group's walk back
  reading its anchor as `call` reads it (`Kernel::anchor_holds`: a
  memoised result, or the latest tuples the eviction kept, checked
  against the solve in debug builds) 222.2 s (`clenow` 6.7 s, `ma` and
  `eligible` 2.6 s each, `decide` 59.2 s); the three planned at every
  bar and none declined, the stats row's counters those of the set
  evaluator.
- **E-157 (K-4.5, E-47, 06-performance 3.3; a delivered answer: a
  relation of the stream read with no local window).** E-47 reads a
  pushed answer only when its receipt is at or before the barrier, and
  evaluates the window here otherwise, because the answer arrives as a
  table at bind, outside receipt order; the fallback keeps the window's
  inputs in the stream. 06-performance 3.3 has a pushed aggregate arrive
  as a relation of its own. The kernel now delivers a pushed literal in
  the stream when (`kernel::pushdown::deliverable`, `bind::push_down`):
  - its window is a bucket. The pass answers a bucket at every label
    whose bucket holds a row, and a bucket's minimum is 1, so the window
    has a value exactly there; a `rows` or span window is answered only
    at the group's own rows, and the rule reads it at any label.
  - its function is `first`, `last`, `min`, `max`, `sum`, `mean`,
    `count`, `median` or `quantile`, none of which fails where the rule
    would halt.
  - its rule is on the decision calendar, and its bucket is that
    calendar's at the rule's own label (`Shape::base_at_head`). The fold
    evaluates rules at decision barriers alone, so such a label is read
    after its barrier, its bucket closed. A bucket of another calendar, or
    at a label the body binds, is read while it is open: `bucket(session,
    S)` with `session_of(A, T, S)` in a program decided at every minute
    reads the session so far, where the pass answers the closed session
    alone. The review of Nov 25 reported such a literal delivered (a minute
    book's high of the session so far met at its session's last bar
    alone, 2 decisions where the local run takes about 50); it is read as
    a table where its receipt allows, and locally before.
  - the run has no cut (`horizon`, `received_by`, `to_seq`: the cut is a
    seq of the stream without the answers), is not live and replays no
    recording, and `ExecConfig::deliver` is on (a test switch outside the
    configuration's identity, as `pushdown` is).
  - the pass reports no inexact row (E-156). Every answer row at a label
    is then computed from rows received by that label's barrier, and its
    receipt is at or before it.
  The kernel asks the request with `answer` false and names the cache
  object (`cache.<key16>`, its version) in the stream spec; two literals
  asking one request read one object. The stream merges its rows by
  receipt, before the barrier of their label. `Kernel::build` appends,
  after the program's relations and outside its signatures, a stored
  relation of the kernel's per deliverable literal (`pushed.<rule>.<lit>`:
  instrument, label, value), whether or not a run delivers it, so the
  program's hash, the run's id and the stores a checkpoint holds depend on
  the program alone; it is read only once delivered, and is trimmed,
  checkpointed and shared with the workers as any store. The decoder maps
  the object's `entity`, label and `value` (a `count`'s `count`) to it,
  and leaves out a row whose value is null. `pushed_answers` reads it with
  no receipt test: the bound instrument's row at the label, or with the
  instrument unbound every row of the label in instrument order; no row
  is no answer, as an empty bucket has none. Under the block evaluator
  its operators read the same rows (E-175). `solve_kleene_block` solves
  no window for a delivered literal. A delivered tuple keyed at or before
  the last decided label is the service's fault: the fold halts with an
  internal error. A late record (K-4.4) is numbered in the stream without
  the answers (`Fold::step_inner`: its seq less the rows the kernel's
  stores of answers hold), the seq the run that reads its answers as
  tables gives it, since the stream merges the answers' rows among the
  others by receipt and moves none of them; the late table (`kernel.<run>
  .late`, keyed by seq) is the run's without delivery. A checkpoint
  records the objects its stream carried, and a run whose stream carries
  others refuses it (its cursor is a seq of the stream with them). The
  data service holds every version an open stream reads against the
  pruning of derived writes (E-110: `Store::hold`, `release`,
  `prune_derived`, held by `DataService::subscribe` until the stream
  ends), as the stream opens an object's partitions as it goes; the
  review of Nov 25 reported the answers pruned by a run's own checkpoints
  past 576 cache objects (`--checkpoint-every 1` over MW14) while its
  stream still read them.
  [D] The plan of package 2 also delivered rows and span windows, and
  had a `count` of minimum 0 count an empty bucket 0; the pass's answers
  of those windows are keyed at the group's rows, and a bucket's minimum
  is always 1, so neither is done. E-47 (a)'s `count` and E-47's one
  stored atom are widened: `count` is pushed (the pass counts the rows,
  E-156), its request naming the atom's first number column, and so is a
  window over a product of two stored relations (`R(A, T, D) :- p(A, T,
  P), q(A, T, V), D = P * V`, one rule, both at the base calendar), asked
  with `factor`. Both are pushed as tables too where the stream does not
  deliver them, `count`'s answer read as `Value::Count`, but a product the
  pass finds inexact is evaluated here, as before: the pass leaves out a
  pair holding a null, which the kernel reads as NaN (E-71), so its
  answer is not the window's even where received (the review of Nov 25:
  a null volume made a weekly mean dollar volume NaN here and a number
  there). A pushed request carries the run's scope as `entities`. The
  plans table's `pushdown` column, `pushed_answers`, `received_rows`,
  `window_calls` and the coverage's calls and bars with tuples follow the
  delivery, as they followed pushdown before (a window the pass answers
  is not solved, and its atoms are not called): a product relation no
  window reads any more has none (MW14's `dollar_volume@base`, never
  demanded); decisions, fills, the book, the ledgers and the late
  records do not. The single-relation answers have the null's gap since
  E-47: a null last close makes the kernel's weekly last close NaN and
  the pass's the last close with a value, so the table is read where the
  local window is NaN (open; the pass entering a null as NaN closes it).
  The inputs are dropped at the fold's door where no other reader reads
  them (E-158).
  `abt-tests/tests/pushdown.rs::{pushdown_and_local_evaluation_agree_on_every_corpus_program_and_book
  (delivered, tables and local, every corpus program and both books),
  a_delivered_aggregate_is_never_evaluated_locally_and_equals_the_local_run,
  a_count_over_an_empty_bucket_is_absent_for_a_bound_instrument_and_an_unbound_one,
  a_late_row_is_read_locally_until_it_is_received,
  a_bucket_read_before_its_label_is_not_delivered_and_equals_the_local_run,
  a_product_over_a_null_is_evaluated_locally_and_equals_the_local_run,
  a_late_tuple_is_numbered_as_in_the_stream_without_the_delivered_answers}`
  (each also under the block evaluator, whose operators read the answers
  as `delivered_answers` does, E-175),
  `abt-tests/tests/fold_checkpoint.rs::a_resumed_run_reads_the_answers_its_checkpoint_held`,
  `abt-data/tests/service.rs::a_stream_holds_the_cache_object_it_carries_until_it_ends`.
  Falsified by `scripts/mutations/e-157-inexact.patch` (an answer with
  inexact rows delivered), `e-157-open-bucket.patch` (a bucket at a label
  the body binds delivered), `e-157-product-null.patch` (an inexact
  product read as a table), `e-157-late-seq.patch` (a late record numbered
  in the stream with the answers) and `e-157-held.patch` (a stream holds
  nothing).
- **E-158 (06-performance 3.3, E-127, E-141, E-157; the inputs only
  delivered answers read are not kept).** 06-performance 3.3 streams
  what the program reads outside its pushed aggregates. Since E-157 a
  delivered answer is read with no local window, so its inputs need not
  be held when no other reader reads them. [D] `bind::push_down` asks
  `Kernel::unread_after_delivery` last, and drops at the fold's door
  (`read_rels`) a stored relation that:
  - is the relation a delivered literal aggregates. Its pass read every
    row of it in the run's scope and proved each received by the barrier
    of the first label whose bucket holds it (E-156), so no row of it is
    late. A product's second relation is read only at the instruments of
    the first, and is kept.
  - every rule reads only inside the window of a delivered literal,
    directly or through the one rule of a product that only such windows
    read (walked as `relations_read` walks the bodies).
  - nothing reads by name: no observation names it; it is not the price
    relation (the invariants mark the book at it) nor the clock of a
    session-offset decision (`decide_at`); it is not one the executor
    reads (`Executor::reads_stores`). The simulation names the price
    relation when it is stored; the volume relation, as every fill records
    its participation of the bar's volume, a column of the fills output,
    whether or not a cap is set; the open, high and low when an order
    fills at the open or on a limit or stop; `close` on a sub-daily base
    decided at the base (a market-on-close order's last print); the
    splits and dividends the book applies; the delistings; the published
    margins. With options or futures it names every stored relation, the
    trait's default (a broker's executor keeps it).
  The relation stays in the stream's specification: leaving it out would
  move the seq of every later event, which the late table keys its rows
  by and a checkpoint's cursor holds, from the run's without delivery
  (the review of Nov 25: a weekly book's late volumes numbered 661 where
  the local run numbers them 919). With the answers' rows discounted
  (E-157) a late record is the local run's, seq and all, and the stream's
  id and `received_rows` are as they were. Nothing is dropped in a run
  whose kernel a command reads after it, as such a read may name any
  relation or derive one again from inputs only a delivered literal read
  during the run (`Replay::read_after` naming any relation; or
  `Replay::read_after_any`, set by the kernel service, whose queries name
  their relation once the run has begun, by `abt query`'s `coverage`,
  which reads every relation at every bar, by `--tables`, which writes
  every derived relation's extension, and by the translation validation's
  ledgers, `verify::tv::run_with_ledgers`). The review of Nov 25 reported
  `abt query --text coverage` and a service query of the daily close
  reading no row of a relation dropped (a debug build stopped at the
  accessor). `Replay::keep_unread` keeps every relation. A run with a
  cut, a live run and a replay of a recording deliver nothing (E-157), so
  drop nothing. `Kernel::store` and `store_mut` are the accessor of the
  evaluator's reads (`store_lookup`, `asof_matches`), of the executor's
  (`bar_price_quiet`, `bar_volume`, `bar_field`, `has_price`, the
  actions, the delistings, the margins) and of `decide_at`'s clock (the
  options' and futures' reads, under which nothing is dropped, read the
  stores as before); in a debug build they stop at a read of a relation
  dropped, so that a reader the rule misses fails there instead of
  reading nothing. A checkpoint records the relations dropped beside the
  objects (`Checkpoint::delivered`), and a run that drops others refuses
  it, its stores being another run's. [D] Package 2's plan had MW14
  stream neither its daily close nor its daily volume. Neither is
  dropped: the membership test reads the name's last daily close of the
  week as of the label (`close@base(A, L, _) asof T` in `eligible`,
  E-160, read by `Kernel::asof_matches`; since E-144 the catalog's total
  return reads the weekly close at the previous row's label, and without
  the as-of read the daily close is dropped), and the simulation reads
  the daily volume at every fill (`Kernel::bar_volume`, the week's sum
  for `FillRecord::participation` and the liquidity summary; the plan had
  it read only under a cap, an impact or an ADV limit, and MW14's
  `book_fixed_base` sets `participation none`). A weekly book that reads its bars only
  through delivered answers holds none of them, and still streams them:
  taking them out of the stream needs seqs numbered without them, which
  the data layer does not give. Where a relation is dropped,
  `trimmed_rows` in the stats row and the coverage's count of the
  relation's tuples (0, as the listing's since E-141) change; decisions,
  fills, the book, the ledgers and the late records do not.
  `abt-tests/tests/fold.rs::a_stream_whose_aggregates_are_delivered_carries_their_answers_and_not_their_inputs`
  (a weekly book's open and close dropped with market orders, its open
  kept when it fills at the open, its volume kept; MW14's close and
  volume kept; each run equal to the run that keeps every relation),
  `abt-tests/tests/pushdown.rs::{pushdown_and_local_evaluation_agree_on_every_corpus_program_and_book,
  a_count_over_an_empty_bucket_is_absent_for_a_bound_instrument_and_an_unbound_one,
  a_late_tuple_is_numbered_as_in_the_stream_without_the_delivered_answers}`,
  `abt-tests/tests/kernel_service.rs::a_held_run_answers_a_query_of_a_relation_only_delivered_answers_read`.
  Falsified by `scripts/mutations/e-158-companions.patch` (the executor
  does not name the companions an order at the open reads) and
  `e-158-read-after.patch` (the kernel service's runs drop the relations
  only delivered answers read).
- **E-159 (K-4.5, E-157; a `count` reads the count a sibling's answer
  carries).** E-157 asks every delivered literal its own request, and an
  answer's rows carry the window's row count beside the value. [D] In
  `bind::push_down` a deliverable `count` whose request is a deliverable
  sibling's but for the function (the relation and its column, the
  bucket, the calendars, the numerics, the instruments; neither of them a
  product; the sibling's minimum no greater than the count's) is asked
  after the other literals and, when the sibling's answer is delivered,
  is not asked: `Pushed::delivered` names the sibling's store and object,
  the argument 3 and the count's own minimum (`Delivered::column`,
  `Delivered::min`), the decoder fills that store's rows with the count as
  their fourth argument, and `delivered_answers` reads it as a count at
  the count's minimum. With no inexact row the relation holds no null
  (E-156), so the sibling's rows with a value are the window's rows and
  its count is the count's; its rows are where the window has a value,
  which with no null is where it has a row. A count whose sibling's
  answer is not delivered is asked alone, as before; the run's notes of
  its pushed literals keep the program's order. MW14's `traded@weekly`
  reads `close@weekly`'s answer: one pass fewer at bind and one weekly
  relation fewer in the stream. The stream's specification and
  `received_rows` change; decisions, fills, the book, the plans table and
  `pushed_answers` do not. A bucket's minimum is always 1, so the count's
  own minimum filters no row today.
  `abt-tests/tests/pushdown.rs::{a_count_that_shares_an_answer_equals_the_count_asked_alone,
  a_delivered_aggregate_is_never_evaluated_locally_and_equals_the_local_run,
  a_count_over_an_empty_bucket_is_absent_for_a_bound_instrument_and_an_unbound_one}`.
  Falsified by `scripts/mutations/e-159-sibling.patch` (a count shares
  the answer of a literal over another relation).
- **E-175 (06-performance 3.2, 3.3, E-150, E-156, E-157, E-159; the
  block evaluator reads the answers the pass gives).** E-150's `Pushed`
  read a pushed literal's answers from the table bind receives. E-157
  delivers them in the stream into a store of the kernel's instead, with
  no table, and E-156 pushes a `count`, whose answer the row path binds
  as the window's row count, a Count (`Kernel::pushed_answers`). Built
  apart and merged, the two left a delivered literal's plan declining at
  every bar (MW14's `close@weekly` and `traded@weekly`), and a `count`
  read as a table bound under the block evaluator as its value, a
  number: a ledger of such a count differed between the evaluators,
  though no decision did. [D] Under `--evaluator block` an aggregate the
  data layer answers is read by the operators as the row path reads it,
  through the same store lookup and the same reading of a store's tuple
  (`Delivered::answer`: the argument the literal reads, none below a
  shared count's minimum, E-159):
  - first in its rule (`ColOp::Pushed`): delivered, every row of the
    label its store holds (`Kernel::delivered_block`), as
    `delivered_answers` reads them with the instrument unbound; as a
    table, the label's answers when every one was received by the
    barrier, a count's as its Count (`table_answers`).
  - after an atom bound the rule's entity (`ColOp::PushedJoin`), for a
    literal `pushdown::eligibility` accepts that groups by that entity at
    the rule's own bar, its window not a rows window (those stay
    `ColOp::RowsAgg`'s): delivered, each row's entity's answer at the
    label, a row without one leaving, as `delivered_answers` reads them
    with the instrument bound; as a table, each row's answer when every
    row's was received by the barrier, a count's as its Count, and the
    bar declines to the rules otherwise, where the row path evaluates
    that row's window here. Read as a table, it also declines where the
    row path solves the window's conjunction over the whole block
    entering the literal before it reads the answers
    (`prefetch_bucket_window`): `PREFETCH_MIN_ROWS` (32) rows or more,
    the window cache on, the literal not delivered, its window a bucket
    or a span with no late test whose bars do not depend on the
    instrument. That solve derives what the conjunction reads, a
    product's relation (E-156) among them, though every row's answer is
    then read from the table: the coverage, the memo (`memo_entries`,
    E-133) and that relation's literal counts hold it. Both evaluators
    make the one test, `Kernel::bucket_prefetch`; what
    `prefetch_bucket_window` tests after it (each row's bindings, the
    bars cached) only withholds the solve, so a bar that solves it is
    never planned. Merged first without it, a span over a product read
    as a table on 40 names (`liq`, seven days of `dollar_volume`) gave
    the daily dollar volume 300 calls, 300 nonempty and 11,940 tuples
    under the set evaluator and none under the block evaluator, and 66
    memo entries against 55. MW14's `dvol` has that shape wherever it is
    read as a table (a store the pass finds inexact rows in). `Pushed`
    cannot meet the case: first in its rule, it is entered by the one
    starting binding.
  A label whose store holds two rows of one instrument, or a row with no
  instrument, declines (bound, the row path reads the first row;
  unbound, every row); the pass gives one row per instrument and label.
  An operator counts the pushed answers the row path counts: one per
  literal and bar first in the rule, one per row after an atom. A plan
  with such an operator runs only where the run asked the data layer for
  the literal (`Kernel::answered`), so with pushdown off or for a
  product the pass finds inexact the relation has no plan. MW14's
  `close@weekly` and `traded@weekly` (the count its rows carry) are
  planned with `Pushed`, and `dvol` with a `Scan` of `universe` and a
  `PushedJoin`; `dollar_volume@base` keeps its plan and is not demanded,
  its weekly mean delivered. The stream, the stores, `received_rows`,
  `pushed_answers`, the coverage, the literal counts and the stats row
  on one thread are the set evaluator's. Workers derive by the rules, as
  for every plan; on four workers the first barrier warms every
  open-loop relation (`Kernel::warm_strata`), `dollar_volume@base`
  among them, under either evaluator.
  `abt-tests/tests/pushdown.rs` (every arm, delivered, tables and local,
  under both evaluators on every corpus program and both books, the
  answers read and the rows received equal to the set evaluator's;
  MW14's three planned under the delivered and the tables arms; the
  bound and the unbound count over an empty bucket, their ledgers equal
  to the local run's under both evaluators and both arms; the late
  rows, the shared count, the open bucket, the product over a null and
  the late records under the block evaluator;
  `a_product_read_as_a_table_over_many_names_counts_what_the_set_evaluator_counts`,
  a product's span read as a table on 40 names and on 5, the coverage,
  the literal counts and the stats row equal between the evaluators),
  `abt-tests/tests/eval_set.rs::the_block_evaluator_derives_what_the_set_evaluator_does_on_mw14_with_holes`
  (`close@weekly`, `traded@weekly` and `dvol` derived through their
  plans; the daily dollar volume not derived on one thread),
  `abt-tests/tests/fold_checkpoint.rs::{a_resumed_run_reads_the_answers_its_checkpoint_held,
  a_checkpoint_records_the_relations_its_stream_left_out}` (resumed under
  the block evaluator),
  `abt-tests/tests/kernel_service.rs::a_held_run_answers_a_query_of_a_relation_only_delivered_answers_read`
  (a run held as the service holds it, every relation read after it,
  under the block evaluator in process: a service request names no
  evaluator). Falsified by `scripts/mutations/e-175-table-count.patch`
  (a count read as a table bound as its value),
  `e-175-delivered-rows.patch` (a label's first delivered row left out)
  and `e-175-table-prefetch.patch` (the table read's decline where the
  row path solves the window over the block removed).
  Measured on the full MW14 against the parity store under `--evaluator
  block`, one cold run (`d4-1`), the weekly returns' fingerprint
  unchanged (`6e72c9bb…`): 133.7 s where the block evaluator on the
  parity stack took 180.2 s; `decide` 42.6 s, `fill` 44.3 s, `push`
  15.7 s, `decode` 3.4 s, `insert` 5.5 s; `dvol` (39.7 s) and
  `traded@weekly` (15.7 s) are not among the sixteen slowest relations
  (the sixteenth 1.1 s), and `close@weekly`, `traded@weekly` and `dvol`
  derive through their plans at all 1,904 bars, none declined; 63.4M
  tuples, the daily close and volume 20.1M each, each delivered answer
  4.2M.

- **E-152 (06-performance 3.2; the block evaluator's as-of read of a
  stored relation, and a read at a time a row binds).** 3.2's block plan
  has no operator for an as-of join or for an atom keyed at a time other
  than the rule's. [D] Under `--evaluator block` two more literals are
  operators of a relation's plan (`kernel/colplan.rs`). An as-of read of
  a stored relation whose entity an earlier literal binds, at a bound
  time, its key and other variables fresh and its other terms wildcards
  or constants (`close@base(A, L, _) asof T`, MW14's membership test since
  E-160), is `ColOp::AsofStored`: per row of the frame, what the row
  path's walk finds for the entity and the constants, through the one
  function both take (`Kernel::asof_stored_into`, factored out of
  `asof_matches`: the rows of the latest key at or before the time, no
  earlier than the floor of a read within a bucket (`asof_floor`), that
  has one, and with no floor the latest rows the trim kept behind the
  horizon); an entity with two tuples there declines the bar. An atom of
  a relation with the frame's entity keyed at a time a row binds (the
  key of that read, `sp1500(A, L)`; a previous row's bar,
  `close@weekly(A, T0, P0)` in `total_return`) is `ColOp::GatherAt`: the
  relation's columns at each time the rows bind, fetched in the order the
  rows first reach it (as the row path's calls derive it), each row
  found in them by its entity; a time whose relation is not one tuple per
  entity there, or is behind the horizon, declines the bar. `eligible`
  and `total_return` are planned on MW14. `tests/eval_set.rs`:
  `an_as_of_read_with_the_entity_bound_finds_the_row_paths_bar_and_the_read_there_its_tuple`
  (a weekly book over names missing bars, some on the week's last day,
  each read at its own last bar of the week and its close read there
  through a relation of the base calendar; whole and trimmed, 1 and 4
  workers, the counts and the stats row on one), and the MW14 test with
  holes with `eligible` planned. Falsified:
  `scripts/mutations/e-152-asof-walk.patch` (only a row at the read's own
  time is kept) and `scripts/mutations/e-152-gather-time.patch` (the
  relation is read at the rule's bar). Measured on the full MW14, one cold
  run under `--evaluator block` against the parity store, the weekly
  returns' fingerprint unchanged (`6e72c9bb…`): 167.1 s where the tree
  before took 180.2 s (`k3-asof-1`), `eligible` 13.2 s to 3.9 s, planned
  at its 1,904 bars with none declined, `total_return` 5.7 s to under
  0.9 s (planned at 1,902 of 1,903), `decide` 83.9 s to 75.5 s.

- **E-153 (06-performance 3.2; the block evaluator's previous row: an
  as-of read of a derived relation, a negated read, `prev`).** [D] Under
  `--evaluator block` three more literals are operators of a relation's
  plan (`kernel/colplan.rs`). An as-of read of a derived relation whose
  entity an earlier literal binds, its key and other variables fresh, on
  a calendar that is not an instrument's (`universe(A, T1) asof T0` in
  `prev_row`), is `ColOp::AsofDerived`: per row, the bar the row path's
  walk stops at, through the one walk both take
  (`Kernel::asof_walk`, factored out of `asof_matches`: the relation's
  labels from the latest at or before the read's time back to its
  floor, the answers of earlier walks read from `asof_memo` and this
  walk's written there for every label walked, keyed by the whole
  pattern as E-132 keys it). The relation is read at the first label
  from its columns once for the rows, and at each label walked as the
  row path reads it: from its columns where `cols_of` holds them, else by
  the row path's own call for the entity, whose answer behind the
  horizon is the one E-126 gives (`Kernel::asof_derived_tuple`); so the
  walk asks, derives and memoises at the bars the row path's does, and no
  bar declines for an entity whose previous row is behind the horizon. A
  call that halts, or two tuples of the entity at the bar, declines the
  bar. A negated atom at the bar with the frame's entity and its other
  terms wildcards or constants (`not member@weekly(A, T, "ETF")` in
  `universe`, `not prev_row(A, T, _)` in `first_row`) is
  `ColOp::AntiJoin`: the rows whose entity has no tuple of the relation's
  columns there that the constants match, a literal's value taken from
  `lit_value` when the operator first reads it, as the row path's
  pattern takes it. `prev(T, T0)` of a time the frame binds once is
  `ColOp::Prev`, a constant: the label before it on the time's
  calendar (not an instrument's), or no row. `universe`, `prev_row` and
  `first_row` are planned on MW14. `tests/eval_set.rs`:
  `the_previous_row_over_columns_is_the_row_paths_across_holes_and_the_horizon`
  (a daily book over names missing bars, one absent 300 days, longer
  than the horizon; `prev_row`, `first_row`, `row_number` and the close
  at the previous row; whole and trimmed, 1 and 4 workers, the counts and
  the stats row on one), and the MW14 test with holes with the three
  planned. Falsified: `scripts/mutations/e-153-asof-derived-walk.patch`
  (the walk never finds the entity in the columns),
  `scripts/mutations/e-153-antijoin-consts.patch` (a negation excludes
  the entity whatever its constants), `scripts/mutations/e-153-prev.patch`
  (`prev` binds the label two before); E-132's
  `scripts/mutations/e-132-asof-entity.patch` is rebased on the walk's
  new place, and `tests/asof.rs::a_derived_relation_is_walked_back_for_each_entity`
  still fails under it. Measured on the full MW14 (`k3-s12-1`), the
  fingerprint unchanged: 159.2 s where E-152's tree took 167.1 s,
  `prev_row` 4.5 s to 1.4 s, `universe` 2.5 s and `first_row` 2.1 s to
  under 0.8 s each, the three planned at every one of the 1,904 bars and
  none declined, `fill` 55.3 s to 48.1 s.

- **E-154 (06-performance 3.2; the block evaluator's chains).** E-150
  plans no temporally recursive relation. [D] A temporally recursive
  relation is planned as any other (`Kernel::compile_col_plan`): its
  read of itself at an earlier bar a row binds (`row_number(A, T1, K0)`
  after `prev_row(A, T, T1)`; `cum_split`, `tr_index`, the latch) is
  E-152's `GatherAt`, which reads the relation's columns there, memoised
  by the chain's extension at each barrier (`Kernel::trim_stores`) or
  derived there first through `cols_of` as the row path's call would
  derive them, by the plan when it has one; a bar whose earlier bar is
  behind the horizon declines, and the row path reads the entity's
  latest tuples there (E-126). The relation's rules are united as E-150
  unites them (a first-row rule's rows, then the step rule's, in entity
  order above 16 tuples, `RuleTag::Per`). No depth bound is put on the
  recursion: a chain derived down to its first rows (a memo cleared by a
  late tuple) recurses through the plans as through the rules, and needs
  less stack so (a daily book of 950 bars with its memo cleared ran on
  4 MB through the plans and needed 8 MB through the rules; a run's
  kernel thread has 512 MB). `call_first` is unchanged: a plan's result
  is memoised as tuples at once (E-150), which it reads by binary
  search; what the executor's price read costs under `fill` is the
  derivation at the bar of what it is the first to ask for (the
  universe's relations, the total-return chain and its dividends).
  `tests/eval_set.rs`: the MW14 test with holes with `row_number`,
  `cum_split`, `tr_index`, `total_return` and the latch planned, and a
  split and a dividend in a name's first week;
  `the_block_evaluator_resumed_or_fed_late_tuples_decides_as_the_set_evaluator`
  (E-152's and E-153's books with a name's closes received three days
  late, on every weekday in turn, whole and trimmed, and resumed from
  monthly checkpoints against the set evaluator resumed from the same
  one, which with the whole history is the unbroken run);
  `tests/retention.rs`'s long-market MW14 test (the index recursing to
  each name's first row, E-126) with block legs. Measured on the full
  MW14 (`k3-s11-1`), the fingerprint unchanged: 149.4 s where E-153's
  tree took 159.2 s, `tr_index` 4.4 s to 1.3 s, `cum_split` 4.4 s to
  1.2 s, `row_number` 3.7 s to 0.8 s, the latch 2.6 s to under 0.7 s,
  each planned at all but one or (the latch) 173 of 1,904 bars, `fill`
  48.1 s to 45.4 s, `decide` 75.2 s to 67.9 s.

- **E-170 (06-performance 3.2; the block evaluator's case).** 3.2's block
  plan has no operator for a case form. [D] Under `--evaluator block` a
  case whose variable is unbound and whose every guard is one read with
  the frame's entity bound is `ColOp::Case` (`kernel/colplan.rs`): an atom
  at the bar whose other terms are wildcards or fresh variables
  (`dividend_at(A, T, D0)` in `dividend_or_zero`), read from its columns as
  a `Join` reads them, or an as-of read of a stored relation
  (`split(A, _, F0) asof T` within the week in `split_factor`), read as
  E-152's `AsofStored` reads it. Per row the first branch whose guard
  finds the entity's tuple gives the value, computed over the guard's
  bindings and the row's with the row path's arithmetic (`cexpr.rs`); each
  branch's guard is read for the rows no earlier branch took, as
  `case_value` tries a row's branches in turn, so a derived relation a
  later guard names is derived at the bar only when a row reaches it; the
  `else` value is the remaining rows', and without one they have no row. A
  guard with two tuples for a row (the row path takes the first; the
  stores hold one per identity, so `split` and `dividend` never have two),
  a guard that cannot be read as columns, and a value partial on any row
  decline the bar. The as-of read of a stored relation at a time every row
  shares reads the relation for every entity at once where the bucket's
  blocks hold no more tuples than the frame has rows
  (`Kernel::asof_bucket`, `kernel/colops.rs`): per entity the tuples at
  the latest key of `floor..=t` that holds one the pattern admits, in the
  block's order -- what `asof_stored_into` finds walking the entity's runs
  back; a read with no floor (which reaches the rows the trim kept), a
  block not yet sorted by the entity (the walk's lookup sorts it) and a
  denser bucket are walked entity by entity as before
  (`Kernel::asof_stored_frame`, which `AsofStored` now calls too).
  `split_factor` and `dividend_or_zero` are planned on MW14.
  `tests/eval_set.rs`:
  `the_case_and_the_calendar_window_over_actions_are_the_row_paths` (a
  weekly book over names missing bars with random actions: two dividends
  announced in one week, dividends announced after they go ex, two splits
  in a week, a name's dividends received late; whole and trimmed, 1 and 4
  workers, the counts and the stats row on one), the MW14 test with holes
  with two splits of a name in one week and a split on a week's last bar,
  and the resumed and late-tuple test over the same book. Falsified:
  `scripts/mutations/e-170-case-first-branch.patch` (a row a guard took is
  offered to the `else` again) and
  `scripts/mutations/e-170-bucket-latest-key.patch` (the earliest key of
  the bucket answers). MEASURED-S13

- **E-171 (06-performance 3.2; the block evaluator's calendar window over
  an as-of read).** [D] Under `--evaluator block` an aggregate over a
  calendar window of the rule's calendar (`window`, `prior_window` or a
  bucket; not a rows window, not an instrument's sessions) whose base is
  the rule's key, whose length and minimum are constants, which the row
  path serves from its per-bar cache (`Kernel::window_plan`), and whose
  cached conjunction is one as-of read of a stored relation at the
  window's bar with the frame's entity bound, its other variables fresh,
  is `ColOp::WindowAgg` (`kernel/colops.rs`, `Kernel::run_window_agg`):
  `dividend_at`'s sum of the dividends announced over a year of weeks,
  each week read as of its label within it, tested at each call against
  the call's week (`Ex > T1`, `Ex <= T`). The tests the call applies
  (comparisons) and the arguments compile to `CExpr`. Per entity, in the
  frame's order, it does what `window_columns` does for that entity's
  call: the group's cache taken from `Kernel::windows` by the row path's
  key (`WindowKey`, the plan's literal and outer slots), the window's
  bars it lacks read and kept with their rows as the row path's solve
  binds them (the call's binding, the window's bar, the read's tuples,
  the arguments' values) and counted as solved, the rows of the window's
  bars in time order and those of one bar in the order of the slots the
  call leaves unbound, each tested on the call's binding, the bars before
  the window forgotten and the cache put back; then the minimum and the
  row path's reduction (`eval::rows_reduce`). So the caches, the
  checkpoints that carry them, `window_bars_solved`, `window_groups` and
  `window_rows_cached` are the set evaluator's, and the row order within
  a bar, which decides a float sum, is the cache's own. A bar the caches
  lack is read for every entity of the frame at once by E-170's
  `asof_bucket`, or entity by entity by `asof_stored_into` (the window's
  first label, whose bucket has no start). An argument that is partial or
  not a number, a test that cannot be evaluated and a reduction that halts
  decline the bar where the row path's call would solve the window whole
  or halt, with the caches as the row path's own call would have left
  them by then: the label the argument fails on is neither kept nor
  counted, so that the rules, deriving the bar, meet it as the row path
  does. Its rule is not cut across the workers under the block evaluator,
  as E-151's. A cached conjunction of more than the one read (a join, a
  test of its own) keeps the row path. `dividend_at` is planned on MW14.
  `tests/eval_set.rs`: E-170's test, whose book adds a sum of an
  expression of the dividends over half a year with a test of what only
  the call binds and a count over the quarter before with none; the MW14
  test with holes with three dividends of a name going ex on one day,
  announced over two weeks, and one announced after it goes ex.
  Falsified: `scripts/mutations/e-171-window-tests.patch` (the call's
  tests are not applied). `scripts/mutations/e-171-bars-solved.patch`
  (the bars read are not counted as solved: the stats row differed) is
  withdrawn by E-165, which takes the counter out of the stored row.
  MEASURED-S14

- **E-161 (02-kernel 5.3, README "The executor", `docs/handoff-data.md`
  section 4 decision 3; a delisting on a calendar coarser than the
  base).** The executor applied a delisting at the decision bar keyed at
  a `delisted` row (`by_time.get(&t)`), once per name whether it was held
  or not (`delisted_done`). The Norgate adapter writes the rows from the
  first trading day after the last print on (decision 3, E-149 (b)), so on
  MW14's weekly calendar the bar of the week that holds the delisting
  session also holds the name's last print: the name was closed there at
  no cost before the week's decisions and, still having a row that week
  and in the target, bought back at the same close at 1/N; a name bought
  that week was not held when the delisting came, was marked done and
  was never closed. Either way it stayed in the book, marked at its last
  price, its exit expiring every week. In wave 1's run (`p5res-1`, the
  Nov 25 residual, the replica's switches 4 and 5): 105 such names, 95
  closed and bought back at that bar and 10 bought there (257803: last
  print Thursday 1995-10-12, delisted from Friday 1995-10-13, bought on
  1995-10-13), 83,830 expired exits, 94% of the run's 88,874 decisions,
  and 106 weeks moved, 2 of them by more than 1e-4. The book
  (`engine_weighted.py`, `simulate_weighted`: "drop names with no row this
  week (delist) at zero cost", `hold = {a: v for a, v in hold.items() if a
  in idpos}` before the week's trades) trades a name normally at its last
  row, where its forward return is NaN and it earns nothing, and drops it
  at no cost the week after. [D] The executor remembers a delisting from
  the first bar whose bucket holds a row of it (the rows since the
  previous bar, so a single row on a day that is no decision bar, E-149
  (b)'s span end, is no longer lost) and applies it at any bar where the
  name is held: on the base calendar at the bar of its date, as before; on
  a calendar coarser than the base at the first bar where the name has no
  price (`Kernel::bar_price`: no print in the bar's bucket), at the last
  price. The bar that holds the last print is the name's last bar; its
  price there is that print, as for any name without a bar on the label
  day, and it trades there like any other name; a name bought there
  leaves at the next bar. This is the executor's rule, for every program,
  and not MW14's: a delisting is a fact of the data the executor applies
  (02-kernel 5.3; README: "a delisted name is force-closed at its last
  trade"), and on a coarse calendar the bar holding the delisting date
  also holds that last trade, so the name has not left the market there.
  The book's rule also drops a name halted for a week that trades again;
  that is no delisting and stays out of the executor (MW14's header
  lists it; no held name halts in the run). Other programs: on the base
  calendar the bar of the date is the first without a print under
  decision 3, so nothing moves, and a delisting dated on a day with a
  print (`executor_contracts.rs::a_delisting_at_the_last_price_has_no_haircut_and_no_cost`)
  still closes there; the corpus programs deciding on a coarser calendar
  (MW14, `resampled_momentum`) keep their golden runs (`tv_golden`); no
  golden and no program hash moves. Tests:
  `abt-tests/tests/actions.rs::on_a_weekly_calendar_a_delisting_applies_at_the_first_bar_without_a_print`
  (a name held through its last week and one bought in it, its last print
  a Wednesday: before, the first was closed that week and bought back,
  the second never closed),
  `mw14_parity.rs::a_name_trades_at_its_last_row_and_leaves_the_book_at_no_cost_at_the_next_week`
  (MW14 on the corpus market: before, E17 was closed at its last week and
  bought back at 1/13, and E20, bought in its last week, was never
  delisted, each with an exit expiring every week after). Mutations:
  `scripts/mutations/e-161-delisting-bar.patch` (the bar of the date
  again) and `e-161-not-held.patch` (a delisting first seen while the name
  is not held dropped for good) each fail both tests. Measured on
  m8-bench2 against the parity store (`p5c-exec-1`, one run): 4,862
  decisions where wave 2's tree made 88,874, 3 orders dropped (the last
  bar's, no next bar) where it dropped or expired 83,938, 149 delistings
  applied where it applied 155 (a name out of the target in its last
  week is sold there by its exit, at cost, as the book sells it, before
  the delisting comes); 1,827 weeks equal to the canon to 1e-4 (1,825), 494 to 8e-15 (479),
  correlation 0.9997368427 (0.9997367319), fingerprint `5493f41f…`. These
  are exactly the replica's with switches 1 to 3 and without 4 and 5
  (`/data/parity2/waterfall.parquet`, its column "3": 494, 1,827,
  0.9997368427): what the 106 weeks the two switches moved still differ
  by is the dividend switches 1 to 3. 257.3 s against
  251.6 s, within one run's spread (wave 2's tree took 258.5 s and
  251.6 s); the fill phase 55.8 s against 61.0 s.
  **Open:** a held name whose prints stop with no `delisted` row at all
  (the bundle's delisting-coverage test and `pre.delistings` warn of it)
  is still kept and marked at its last price, its exits expiring each
  week, where the book drops it; the run holds none (all 105 names had a
  row).
- **E-162 (V-3.4, Definition V-3.5, E-145's open item (1); an action read
  in a group on a calendar coarser than its own).** A catalog relation
  held at a finer calendar than the rule's is read at a bar as its bar
  view, the tuples of the bucket's latest key that has one
  (`eval.rs::asof_matches` with `within_bucket`), and so it was inside an
  aggregate too: at each weekly bar of `dividend_at`'s window one dividend
  of a name counted, the later of two going ex in one week. The Norgate
  registry books 127678's $0.12 on 1994-02-16 and again on 02-17, and 349
  asset-weeks of the parity store hold two dividends; the book
  (`data.py` `load_daily`, the bars' `div_cash` on every day) takes both.
  The Nov 25 residual's switch 3: 7 weeks moved, 5 of them owned, between
  8e-15 and 1e-4. [D] In a group, a corporate action (`split`, `dividend`,
  `special_dividend`, `delisted`: the catalog's "corporate actions as
  events", `Demand::need_for`) read on a calendar coarser than its own is
  every distinct tuple of the bar's bucket, at a window's bar or at the
  head's, as a bucket aggregate takes every base row of its bucket
  (Definition V-3.5): `Literal::AsOf::every`, set by
  `link.rs::lower_literal` on the group's written atoms, makes
  `asof_matches` read every key of the bucket. Two tuples equal but for
  the key (one dividend announced twice in its week) are one row: the key
  is no term of the read. Outside a group the read is the bar view, as
  before (`split_factor`'s split of the week, the latest); a status
  constant over the bucket (`delisted`) gives the one row it gave. Other
  programs: on the base calendar no read is a bucket read and nothing
  moves; MW14 is the one corpus program aggregating an action on a coarser
  calendar, and its golden run does not move (its corpus market pays one
  dividend a quarter). Measured on m8-bench2 against the parity store
  (`p5c-div-1`, one run, on E-161's tree): 258.97 s (257.30), `dividend_at`
  17.83 s (17.40), 1,827 weeks equal to the canon to 1e-4 and 494 to 8e-15
  (both unchanged), correlation 0.9997368479 (0.9997368427), fingerprint
  `9dbd5812…`: the weeks the second dividends move still differ by the
  one-period arithmetic E-163 replaces. Test:
  `tests/dividend_week.rs::a_weekly_row_takes_every_dividend_of_one_name_going_ex_in_its_week`
  (two dividends on a Wednesday and a Thursday, the Thursday's announced
  again on the Tuesday: 0.24, where the bar view gave 0.12). Mutations:
  `scripts/mutations/e-162-latest-action.patch` (the bar view again: 0.12)
  and `e-162-key-twice.patch` (no merging by key: 0.36). **Open:** one
  dividend announced in two buckets counts in both; two splits of one week
  still apply the later (`split_factor` reads outside a group).
- **E-163 (V-3.3, B-3, P-1.2, WF-6, E-145's open item (2); the total
  return of a coarser bar compounds each dividend at its ex-date's close,
  the `prod` aggregate, and a time bounded by a comparison).**
  `total_return` was one period on the bucket's closes,
  `(P F + D) / P0 − 1`: a dividend going ex before the bucket's last bar
  was reinvested at the bucket's close. The book compounds the days
  (`data.py`: `load_daily` `trf = (close + div_cash) * split /
  close.shift(1)`, `to_weekly` `pl.col("trf").product()`, `add_features`
  `tri` its running product). The Nov 25 residual's switch 1: 24 weeks
  beyond 1e-4 and 1,235 between, 141857's special dividend of 2000-02-11
  (93.8% of the week's opening price, the close not dropping on the
  ex-day) giving the name +11.3% in the run and −66.0% in the book. [D]
  `1 + R = (P + X) F / P0 × G`: G the product, over the bucket's
  dividends announced in the bucket and going ex at a bar of the name
  before L, its last bar at or before T, of `1 + D_i F_i / C_i`, C_i the
  close of the ex-date and F_i that day's split factor (1 with none); X
  the bucket's other dividends, `D − S` with S the compounded ones' sum,
  reinvested at L's close and times L's split factor
  (`dividend_reinvested`, read from the stored `close` and `split` at the
  day, not from a relation of the base calendar, which would be derived at
  every daily bar: 15.7 s in `p5c-div-2`). The closes telescope, so this is the book's
  product of the days, equal to rounding and not to the bit (the week's
  daily factors are not formed from the closes read). On the base
  calendar L is the bar and G is 1: `(P + D F) F / P0 − 1`, the book's day
  to the bit and, on a day with no split, the former formula's bits.
  Three things the formulation needs. (a) A product aggregate: `prod` is
  added to abt-agg's set (`Function::Prod`, distributive: the rows
  multiplied in their order from 1 in every mode, tick columns through
  their views, having no integer form at their scale), so the data
  service and the kernel share it; the checker takes it of a Scalar alone
  (`check/types.rs::agg_type`: a product's dimension would be its
  argument's to the group's size). Every other reduction is unchanged
  (abt-agg's tests). (b) A read at the ex-date: a time a comparison
  bounds above by one derived from the head time is derived from it too
  (`check/rule.rs::Analyzer::bound_time`; WF-6 refused `close` at a
  dividend's `Ex`, a value of the action), so after `Ex < L` the ex-date's
  close is a causal read; the bound comes before the read. (c) No
  negation of what reads `close`: bars not declared complete make such a
  relation open (WF-5), so the defaults are rules, not case forms: the
  return with no dividend in the bucket (`not dividend_at`, closed) and a
  count of the compounded dividends. The compounded set is the dividends
  announced in the bucket, every Norgate dividend (keyed at its
  ex-date): a window over the year's announcements is cached a bar at a
  time when the bar enters it (E-75), and a dividend announced there may
  go ex later, its close then not yet read. `dividend_reinvested` holds a
  tuple only in a bucket with a dividend, and `total_return`'s two rules
  test `dividend_at` second, so a bar without one reads what it read
  before. Other programs: the four
  daily programs reading `log_return` (`low_volatility`, `momentum_12_1`,
  `short_term_reversal`, `total_return_momentum`) render their golden
  runs as before, their synthetic dividends going ex on no split's day
  (`tv_golden`: only the three Mac libm slippage lines), and their program
  hashes move with the library; MW14's golden run does not move: its
  corpus market announces each dividend ten bars before its ex-date, so
  it is reinvested at the week's close as before (open item 1).
  `tests/golden/program_hashes.tsv` changes for `low_volatility`,
  `momentum_12_1`, `short_term_reversal`, `total_return_momentum` and
  `mw14`. Measured on m8-bench2 against the parity store, one run each:
  `p5c-div-2` (this arithmetic with the ex-date's close and split read
  through a base-calendar relation) and `p5c-div-3` (read from the stored
  relations, the tree of this entry) both have every one of the 1,851
  compared weeks equal to the canon to 8e-15, correlation
  0.9999999999999999, fingerprint `be605991…`, 4,862 decisions;
  `p5c-div-3` 258.24 s against `p5c-div-1`'s 258.97 s, `total_return`
  7.13 s and `dividend_reinvested` 2.79 s where `total_return` and
  `dividend_or_zero` took 5.67 s and 3.56 s; `p5c-div-2` 275.09 s, its
  base relation 15.69 s. The package 5 gate holds. Tests:
  `tests/dividend_week.rs::a_weekly_return_compounds_each_dividend_at_the_close_of_its_ex_date`
  (two dividends compounded, a split day's on a Wednesday and on the
  label, each against the product of the book's daily factors to 1e-14),
  `::a_weekly_row_takes_every_action_going_ex_in_its_week` (X's Tuesday
  dividend reinvested at Tuesday's close: E-145's 0.0576923 is now the
  book's 0.0579644), `::a_daily_return_is_the_books_daily_factor_to_the_bit`,
  `abt-agg/tests/agg.rs::a_product_is_its_rows_multiplied_in_order`,
  `abt-tests/tests/checker.rs::a_time_bounded_above_by_the_head_time_may_key_a_read`.
  Mutations: `scripts/mutations/e-163-one-period.patch` (nothing
  compounded), `e-163-prod-sum.patch` (`prod` sums),
  `e-163-unbounded-time.patch` (no bound from a comparison: the library
  does not check). **Open:** (1) a dividend announced in an earlier
  bucket than its ex-date's (a vendor with declaration dates, the
  synthetic market) is reinvested at the bucket's last close; (2) a
  dividend going ex on a day the name has no bar is reinvested at L's
  close, where the book, with no bar, has no dividend (the lake has none);
  (3) two splits in a bucket (E-162).
- **E-164 (the Norgate adapter's decision 6 against the book; a dividend
  going ex on a split's day, the residual's D7).** The adapter stores a
  dividend going ex on its split's day as the vendor's amount over the
  factor (`norgate.rs`, `amount = c / factor`), the amount per share after
  the split, and the library added it to `P F`: `P F + D / F`. The book
  adds the lake's amount to the day's close, already after the split, and
  multiplies by the factor: `(close + D) F` (`data.py` `load_daily`), the
  amount per share after the split times the shares a share before it
  became. On the 42 split days of the lake the bars' `div_cash` is the
  registry amount (MSFT 2003-02-18: the book 3.685%, the engine 3.437%;
  CSR 2018-12-28: 4.04% against 144%); the Nov 25 residual's switch 2: 3
  weeks between 8e-15 and 1e-4. [D] The library follows the book: the
  store's amount times the ex-date's split factor undoes the adapter's
  division, and the day's close takes it before the split,
  `(P + D F) F` at the bucket's last bar and `1 + D F / C` at an earlier
  one (E-163). Why the two differ: the adapter reads Norgate's amount on a
  split's day as per share before the split and expresses it in the
  shares the day's close is in; the book reads it as per share after.
  Which one Norgate means is the vendor's convention and the owner's call
  (the adapter's report, Nov 25); the adapter is unchanged, so the
  other reading is the library's one factor. Tests:
  `tests/dividend_week.rs::a_daily_return_is_the_books_daily_factor_to_the_bit`
  (W's Wednesday `(36 + 0.5) 2 / 71 − 1` and V's Friday `(47 + 1) 2 / 93
  − 1`, the book's to the bit) and the weekly test of E-163. Mutation:
  `scripts/mutations/e-164-store-amount.patch` (the store's amount: W's
  day 0.0211 against the book's 0.0282).

- **E-165 (06-performance 3.2, E-133, E-162, E-171; the calendar window
  read for the whole frame, and the window cache's counters out of the
  stored stats row).** E-171's operator did, entity by entity, what the
  row path's call does: the group's cache taken by its key, the window's
  bars it lacks read, every bar of the window walked for its rows. On the
  full MW14 that is 2,200 names times a year of weekly bars at each of
  1,904 weeks to find at most four dividends a name, 10.3 s; and under
  E-162 the read takes every key of the bucket, which E-171 does not
  read, so the rule went back to the row path, 15.4 s. [D]
  `ColOp::WindowAgg` is first `Kernel::run_window_frame`
  (`kernel/colops.rs`): the window's bars in order, each bar's bucket
  read once from the store for every entity (the tuples of an entity's
  latest key in the bucket that the pattern admits, as
  `Kernel::asof_bucket` finds them; with `every` each tuple of the
  bucket's keys in time order, two equal but for the key being one row,
  as `asof_matches` reads them), each tuple given to its entity's row of
  the frame by bisection, bound as the row path's solve binds it (the
  call's binding, the window's bar, the read's variables), its arguments
  computed, the rows of one entity at one bar put in the order of the
  slots the call leaves unbound, tested on the call's binding and
  appended to the entity's columns; then the minimum and the row path's
  reduction (`eval::rows_reduce`). What a call costs is the tuples the
  relation holds in the window, not the frame's entities times the
  window's bars. It reads and writes no entity's cache. It declines
  (`None`) where the walk cannot say what the rules would: a frame with
  two rows of one entity, a worker kernel, a read with no floor or whose
  first bucket starts behind the trimmed horizon
  (`Kernel::horizon_floor`), a store not sorted by the read's entity, an
  argument that is partial or not a number, a test without a value, a
  reduction that halts. Where it declines, a read of the latest key goes
  on to E-171's operator and a read of every key to the rules.
  The counters of the per-group cache -- `window_bars_solved`,
  `window_groups`, `window_rows_cached` -- say how a window was computed,
  not what the run derived, and an operator that keeps no cache counts
  none: stored, they would make the `stats` row of a run's `report`
  depend on the evaluator, against A-4 (one run id, the same bytes) as
  the clocks did (E-133). They are `#[serde(skip)]` like the clocks and
  like `rows_walks`: `--timing` prints them from the result in process,
  no stored row and no checkpoint carries them, and a resumed run counts
  them from its checkpoint. `window_calls` stays stored: one per entity
  of the frame, as the row path counts a call. E-171's sentence "the
  caches, the checkpoints that carry them, `window_bars_solved`,
  `window_groups` and `window_rows_cached` are the set evaluator's" holds
  for its operator only, and its mutation
  `scripts/mutations/e-171-bars-solved.patch` is withdrawn: no stored
  row tells the two apart any more. Measured on m8-bench2, the full MW14
  on the parity store with `--evaluator block`: `dividend_at` 15.4 s to
  3.1 s, the run 90.8 s to 78.7 s, the weekly returns' fingerprint
  unchanged (`be605991...`), 1,851 of 1,851 weeks equal to the canon at
  8e-15. `tests/eval_set.rs` holds the set and the block evaluators equal
  on the books with the window planned (the coverage, the literal counts
  and the stored stats row); `tests/dividend_week.rs`. Falsified:
  `scripts/mutations/e-165-frame-latest-key.patch` (a read of every key
  takes the latest key only).
- **E-166 (P-2.1, P-2.4, M-9, E-81; a connection over a socket ends with
  its writing half).** `Connection::socket` splits a Unix socket into two
  descriptors of the one socket, a writer and a reader, and
  `rpc::Client::connect` gives the reader to a thread. Dropping the
  client closed the writer's descriptor and nothing else: the reader's
  thread, blocked in a read, held the other for ever, the service never
  saw its client go, and its connection thread, the service and the
  store the service holds (for a dataset held in memory, a temporary
  store and its mappings) outlived every client. In process, dropping a
  client drops the channel's sender and the service's loop ends. Under
  `ABT_TRANSPORT=socket` a test binary therefore kept every service it
  ever started: `tests/eval_set.rs` reached 546 threads and 58,308
  mappings at `75c19f9` and, with the tests E-152 to E-171 add, the
  kernel's limit of 65,530 (`vm.max_map_count`), where the next service
  failed with `Cannot allocate memory (os error 12)` at 4.3 GB of 16 GB
  resident. [D] `SocketTx` shuts the socket down in both directions when
  it is dropped (`abt-wire/src/transport.rs`): the last writer of a
  connection (the client and every reply it has open hold one) ends the
  read at both ends, as the channel does. A service process whose client
  process exits saw the end already, by the system closing the socket.
  `abt-wire/tests/rpc.rs::a_dropped_client_ends_its_connection_over_a_socket`
  (the service's loop returns and the service is released); falsified:
  `scripts/mutations/e-166-socket-held.patch` (the writer does not shut
  the socket down).
- **E-167 (language-v2 Definition 8.3 and section 9.2, E-126, A-7.3; an
  observed relation is read at the barriers whatever its calendar, and
  under a session-offset clock at the clock's bars; the barrier's
  latency printed).** Two things kept R8L from running in a time and a
  memory a live run can live with. (1) E-126 read at the barriers only
  the observed relations on the decision calendar. A program observing
  one on another calendar (R8L's `observe m_median`, on its markets'
  sessions under a minute clock) had every ledger read after the run and
  kept its whole history. [D] `outputs::observe_bar` reads a relation of
  another calendar at each bar of its own calendar after the previous
  barrier and up to this one (`Kernel::bars_between`): its inputs at
  such a bar are all received, and still held, at the first barrier at
  or after it. A run trims whatever it observes; E-126's sentence "An
  observed relation off the decision calendar is still read after the
  run, and such a program keeps its history whole" is withdrawn. (2) The
  ledgers of `observe signal`, `aligned` and `settled` listed the
  relation at every minute bar of the run, for every root, where section
  9.2 and the book's own comment say "at every decision bar". Definition
  8.3 adds R(·, t, ·) to the goals of every bar t; under `clock { decide
  on base at session + 30min }` the bars that have a goal are the
  clock's, an instrument's own (3.5: the `decide` rules carry `clock(A,
  T)`, `check/link.rs`), and 06-performance 3.3 gives the kernel those
  barriers and no other. [D] Under a session-offset clock an observed
  relation on the decision calendar is read at each bar some
  instrument's clock strikes at, for those instruments
  (`outputs::observed_at`, `Kernel::clock_syms_at`): `signal` has one row
  a root a session, at the bar the decision reads it, and the
  observation costs what Proposition 8.5 says, the instances the
  decision did not ask for. A relation with no instrument among its
  arguments is read at every bar some clock strikes at. The reading
  after the run (`outputs::ledgers`, a run that keeps its history) lists
  the same rows; `ledger R into NAME` is still R's whole extension at
  every bar of its calendar (V-8.3). R8L's golden ledgers change and are
  blessed again; nothing a run decides does (Proposition 8.4). A program
  without a session offset is read as before.
  The memo's eviction, which a trimmed run makes at every barrier,
  walked each rows window's lookback at every one: the walk is kept per
  bar of the window's calendar, and a cutoff that has not moved evicts
  nothing (`Kernel::evict_memo`; what was memoised behind it since is
  dropped when it moves).
  `abt run --timing` prints two more lines on standard error, `barriers`
  and `decision barriers`: the count, the mean, the median, the 99th
  percentile and the greatest of the seconds a barrier took from its
  arrival to its end (the fills, the decision, the invariants, the
  observation, the eviction and the trim), over the run's barriers and
  over those a decision was scheduled at (under a session-offset clock,
  those some clock strikes at). They are `KernelStats::barrier_secs` and
  `decision_barrier_secs`, kept out of the stored stats row: a clock is
  no output of a run (E-133). This is the instrument of A-7.3's "barrier
  to decision", measured in a replay: the transport of a live bar's
  tuples is not in it.
  Measured on the benchmark machine, R8L over 26 synthetic roots and
  3,000 days of minute bars (180,000 barriers, 23.6M tuples; the real
  futures history is not on this machine), `--evaluator block`, the same
  4,618 decisions and 9,236 fills in each: at `3be3ba4` 393.8 s and
  5.79 GB; with (1) alone 244.3 s and 2.61 GB, 204.1 s of it the
  observation at every minute; with (2) and the eviction 32.9 s and
  0.60 GB (the last two beside a one-core ingest, so not a clean clock).
  A barrier takes 0.106 ms on average (median 0.017 ms, 99th percentile
  3.3 ms, greatest 29.0 ms), a decision barrier 1.66 ms (median 1.68 ms,
  99th percentile 2.2 ms, greatest 29.0 ms), against A-7.3's 50 ms. What
  is left is in the open items of `docs/assessment.md`: the session view
  of the universe counts the universe at every minute of the session
  (7.6 s of the 32.9), the session windows walk their minutes in the
  kernel (5.5 s), and the kernel still receives every minute as a
  barrier, where 06-performance 3.3 gives it the clock's bars.
  `tests/outputs.rs::a_trimmed_run_writes_the_ledgers_of_the_whole_run_on_every_calendar`
  and `::an_observation_under_a_session_clock_lists_an_instrument_at_its_decision_bar`,
  `abt/tests/cli_options.rs::timing_prints_one_stderr_line_with_every_field`;
  falsified: `scripts/mutations/e-167-observed-bar.patch` (a bar of
  another calendar that is the barrier's own is not read) and
  `e-167-observed-off-clock.patch` (the relation is read at every bar of
  the decision calendar).
- **E-168 (06-performance 2 and 3.3, D-6.3, K-3.2; what a run asks of the
  data layer before its stream, it asks together).** Before its stream
  opened a run made its preconditions' reads and then asked its pushed
  aggregates one after another, and the data service computed an
  aggregate under the store's write lock, so that it served nothing else
  meanwhile: on MW14 over the parity store 8.1 s of preconditions and
  then 14.6 s of pushed aggregates, of a run of 78.7 s. [D] The pass is
  two halves (`abt-data/src/agg.rs`): `prepare` reads the store and
  computes, which the service does under its read lock; `finish` stores
  the answer under the write lock, and a request whose key another
  stored meanwhile is answered by that object and writes none (the key
  names the versions the pass read, so the object is that request's
  answer whatever was written since). `aggregate` is the two in turn.
  [D] The kernel asks its distinct pushed requests together, on up to
  four threads (`bind::push_down`, `PUSH_THREADS`), before the loop that
  reads them in the order the answers decide, which then finds each in
  the data layer's cache; a request that fails there fails again in the
  loop, where it is told as before. [D] The preconditions run on a
  thread of their own beside the kernel's build, the stream's
  specification and the pushed aggregates (`kernel::replay`). Both end
  before the stream opens, and a refusal is the run's error whatever the
  other thread met, as when they ran in turn. The seconds of `check` and
  of `build`, `spec` and `push` on the `phases` line therefore overlap:
  they no longer add up to the clock before the stream. A data call
  records its thread (`DataCall::thread`) and a stage's printed calls
  (`ABT_TIMING_RELS`) are its thread's.
  `abt-data/tests/agg.rs::two_passes_of_one_request_that_both_missed_store_one_object`;
  falsified: `scripts/mutations/e-168-pass-stored-twice.patch` (the
  second half stores without looking). The pushed answers and the
  preconditions' statuses are held equal by the tests that held them
  (`tests/pushdown.rs`, `tests/contracts.rs`, `tests/list_scope.rs`).
  Measured on MW14's parity store: 78.7 s to 68.1 s with the same
  returns (1,851 of 1,851 weeks to 8e-15), 16.6 s before the stream where
  it was 26.4 (`docs/assessment.md`, "The work before the stream,
  together").
- **E-98 (D-2.3, D-7.7; the GICS levels below the sector).** The Norgate
  adapter emitted only level 1 of `classification/classification_full`
  (the sector), while the lake holds the four GICS levels of its snapshot.
  A new relation `equity.industry(A, @T, Group, Industry, SubIndustry)`
  carries levels 2 to 4 over the same days and membership intervals as
  `equity.classification`, with the same `classification = snapshot DATE`
  attribute; `need lists` brings it with `member` and `classification`.
  [D] A relation of its own rather than three more columns of
  `classification`: a program reading the sector keeps its arity. The
  lake's classification is one snapshot (2026-09-21 today): no row exists
  before its date, so an idea whose horizon precedes it cannot read an
  industry, which is the point-in-time rule, not a gap.
- **E-97 (Q-2.1, Q-2.3, P-3.4, A-4.1; the hold-out forms the pipeline
  needs: `after TIME`, `trailing:N%`, `trailing:Nmo`).** The pipeline
  design (`absolute-pipeline/docs/design/pipeline.md`, PL-6.6 item 2)
  asks for a forward hold-out on everything received after a time and a
  trailing hold-out sized from the window. Q-2.1's grammar gains
  `holdout after TIME`, `trailing:N%` and `trailing:Nmo` beside
  `trailing:Ny` (`plan::Holdout::{After, TrailingShare, Trailing}`,
  `parse::holdout`); Proposition Q-2.1 holds unchanged, since every form
  but `blocks` truncates the stream.
  - **`after TIME`.** TIME is an ISO-8601 date or datetime, UTC, with
    optional nanoseconds (`abt_wire::scalar::parse_timestamp`), written
    back as the kernel prints a time, so one instant has one text in the
    normal form. The embargo begins at the last barrier whose receipt
    time is at or before TIME: the kernel's run request (P-3.4) gains
    `received_by`, which it turns into `to_seq` through `stream_index`
    (the last decision barrier whose `at` is ≤ TIME; `kernel/mod.rs`),
    exactly as `horizon` is turned into one, taking the earlier seq when
    a request gives both; the query layer sends an `after` plan's runs
    `received_by` alone, so the receipt is what truncates them.
    `received_by` is in π (`bind::run_step`), so a run truncated by
    receipt is its own run (A-4.1). `show binding` takes
    `received_by` too and answers `embargo_from`, the first decision bar
    whose barrier closes after TIME (`bind::barrier_at`, E-13), which is
    where the query layer's embargo begins (`validate::after_embargo`):
    its masks, its reveal (`Keep::Inside`) and its claims read the same
    boundary the kernel truncates at. A daily bar labelled at midnight
    closes at its day's end, so `after 2023-09-15` embargoes the bar of
    the 15th. A TIME after the sample embargoes nothing (a reveal is
    refused: nothing to reveal); one before it leaves no bar to read.
    Rejected: computing the boundary in the query layer from the labels
    (it does not know the calendars' bucket rule, which the kernel and
    the data layer share), and truncating by the label alone (a bar
    received after TIME would be read when its label precedes it).
  - **`trailing:N%`.** N is between 1 and 99. The embargo is the last N
    % of the span between the first and last decision bars of the plan's
    window (`validate::trailing_share_embargo`, the tail rounded up to
    the nanosecond): the window's ends clipped to the sample's when the
    plan has one, the sample's otherwise. `trailing:Ny` and `trailing:Nmo`
    keep counting back from the sample's last bar, as E-59 built them,
    so no existing plan's embargo moves. Rejected: a percentage of the
    sample whatever the window (the guideline sizes the hold-out from
    the span before H, which is the window).
  - **`trailing:Nmo`.** Months, as `blocks` and `walk_forward` already
    count them (`validate::trailing_months_embargo`); `Holdout::Trailing`
    holds months, and a whole number of years is still written
    `trailing:Ny` (`describe`), so `trailing:24mo` and `trailing:2y` are
    one plan and every existing normal form is unchanged.
  - **Tests.** `abt-tests/tests/plan_holdout.rs` (every run under `after`
    reads no bar received after TIME and the reveal reports exactly the
    bars after it; the percentage embargoes its share of the sample and
    of the window; the forms parse, round-trip and the malformed are
    refused) and `abt/tests/plan_cli.rs` (`abt plan check` prints each
    form). Falsified by `scripts/mutations/q-2.1-after-receipt.patch`
    (the kernel cuts by the label rather than the barrier's instant).
- **E-95 (Q-3.1, Q-8; the metric rows the red flags need).** The
  pipeline's rationale names every red flag that fires (guideline
  sections 12 and 13, `absolute-pipeline/docs/design/agent-guidelines.md`;
  PL-8.1), and the report block of its plan template asks for the rows
  by name. The metrics library gains them (`metrics/trades.rs`,
  `TradeMetrics`, computed in `plan::exec::run_one` over the bars and
  fills a run's mask keeps, and in `abt metrics` over the whole run),
  and `METRIC_NAMES` and `objective_value` know their names, so a
  `report` block, a `compare` step and an `objective` take them:
  - `return`: the mean bar P&L over the fixed base (the report block's
    capital, else the first NAV kept), times the periods a year, which is
    the guideline's "average return % a year on the fixed base";
    `mar`: `return` over the deepest fall of the cumulative P&L from its
    peak as a share of the base (NaN without a drawdown);
    `drawdown_duration`: `max_drawdown_duration` under the name the
    guideline uses.
  - The round trips (`convention_metrics`'s: a security's position from
    flat back to flat among the fills kept, its P&L the cash flows net
    of commissions and fees, closed at its last fill): `trades`,
    `profit_factor` (the winners' P&L over the losers'; ∞ without a
    loss, NaN without a trade), `hit_rate` (a trade of exactly zero is
    no win), `avg_win_loss`, `avg_trade`, `avg_trade_to_cost` (over
    twice the fills' commissions and fees per unit filled, the
    round-turn cost per contract or per share), `largest_loss` (a
    positive number, 0 without a losing trade), `trade_skew`.
  - The concentration: `top_market_share` (the largest entity's share
    of the net profit over every trade) and `profitable_market_share`
    (the share of entities traded whose trades made money);
    `profit_before_1990_share` (of the bars' net profit, the part made
    before 1990-01-01). A share of a net profit that is not positive is
    NaN, which the report shows as null.
  - **Tables.** `summary` shows the rows by name as any metric;
    `contribution` is new (per run and entity, by ticker where the
    store's `ticker` relation names the id: net profit, share, trades,
    hit rate); `subperiods` keeps its columns and gains the best run's
    `net_profit` and `trades` per calendar year. `abt metrics` prints a
    `trades` line and carries the group as `trades` in `--json`.
  - **Decisions.** The rows live in a struct of their own rather than
    in `ReturnMetrics` or `TradingMetrics`, so the frozen records of
    E-66 (`golden/study/*.json`, which serialise those two) and
    `query.<plan>.outcomes` keep their shapes. A run's masked rows are
    over the bars and fills kept: a position opened outside the mask
    and closed inside never flattens inside it and is no trade. The
    yearly net profit is the bars' (what the book earned), the yearly
    trade count the round trips closed in the year; the contribution by
    entity is the trades' (a bar's P&L has no entity). Rejected: the
    yearly rows from trades alone (the book's open positions would
    leave the years), and `profit_factor` per period (the guideline's
    thin-edge row is a trade statistic; `convention_metrics` keeps both
    under its own names).
  - **Tests.** `abt-tests/tests/metrics.rs::the_red_flag_rows_are_computed_as_stated_from_hand_built_fills`
    (six bars across 1990 and four round trips, every row against a
    hand computation, the mask and the empty case) and
    `report_tables.rs` (the three tables of a grid plan). Falsified by
    `scripts/mutations/q-3.1-hit-rate.patch` (a flat trade counted as a
    win).

## Layout

- **E-26 (M-0).** Integration tests that drive the `abt` binary live in
  `crates/abt/tests/`, since a binary's path is visible only to its own
  package's tests; suites that span layers live in a test-only crate
  `crates/abt-tests` (with the shared fixtures as its library); each layer's
  own suites live in its crate.

- **E-83 (A-D2, Definition A-2.1, D-7.7, M-0; the three programs).**
  Definition A-2.1 gives each layer a binary `abt-L` that links no other
  layer; until iteration 1 (`docs/iterations/01-foundation.md`) the three
  services were subcommands of the front alone (E-81). Now
  `crates/abt-data/src/bin/abt-data.rs`, `crates/abt-kernel/src/bin/abt-kernel.rs`
  and `crates/abt-query/src/bin/abt-query.rs` are the programs, and the
  front's `abt data|kernel|query ...` and `abt plan|metrics|...` are doors to
  the same functions. What this moved:
  - The data layer's commands live in `abt_data::cli`. Its `synthetic` and
    `bundle_v2` adapters are the data layer's own (`adapters::synthetic`,
    `adapters::bundle`, over `adapters::market`): the kernel's generators and
    bundle loader were the front's (E-38), and a layer's binary cannot link
    them. The data layer's markets are the kernel's bit for bit
    (`crates/abt-tests/tests/synthetic.rs` holds every generator and a
    written bundle equal through both), so the golden runs are unchanged and
    a store either fills is the same store. `abt data ingest` no longer
    takes the class files: `--class C` names the class the registry knows
    and `--base CAL` the bars' calendar (the class's finest by default).
    The directory-of-Parquet-files form of `bundle_v2` (a loose directory
    without a manifest, read by the kernel's `load_parquet_dir`) is not an
    adapter of the data layer: it is a test loader, and the front keeps it
    (`abt::loose_ingest`, which `abt data ingest` tries first); `abt-data`
    refuses such a directory naming the manifest it needs.
  - The query layer's commands live in `abt_query::dispatch`, which reaches
    sockets alone; the front passes it `Links` that add a store served in
    process (`--store DIR`) and kernels in process (`--kernels N`).
    `abt-query briefs` reads the briefs market from the data service it is
    given; the front makes that market itself.
  - `abt-kernel` reads its data at `--data SOCKET` only: `--synthetic` and a
    store directory are the front's, which links the data layer (the
    kernel's provider hooks, `kernel::bind::install_provider`, stay empty in
    the binary and refuse with the message naming `--data SOCKET`).
  - `crates/abt/tests/deployment.rs` runs the three programs, found beside
    the front's binary; `cargo test --workspace` builds them.

- **E-84 (P-1.1, P-3.1, P-7, D-2.1, D-6.3, K-2.2, ST-10.2, ST-10.4; the
  protocol's storage additions).** Protocol `1.1` (a minor version: a
  client of 1.0 is served, the new fields absent). `abt.fbs` gains
  `ColumnType::Tick` (an exact decimal, `i64` units of `10^(-s)`, carried
  as Arrow `Decimal128(18, s)` with the scale per batch, A-D44),
  `Numerics::Exact`, `QueryRequest.ticks` and the `MapRequest` of ST-10.2
  (answered as a table of segment descriptors once a relation is held hot;
  until the hot store lands, a known relation is answered as having no
  segments). `Numerics::Exact` is refused by the aggregate pass until it
  reduces tick columns (M-1, M-2 of iteration 1), with the message naming
  the iteration. The stored form of a `Tick` column is `Decimal128(18, 9)`
  (the finest scale); an answer may carry a coarser scale, which readers
  take from the batch's type. `ColumnType::Tick` is not a type the registry
  gives a relation's column: a price or quantity column is `Num`, and crosses
  as ticks when a request asks for them (01 §2.1 as amended); the variant
  names the column's form on the wire.

- **E-85 (P-1.2, D-6.3, A-D47; the exact numerics as built).** `abt-agg`
  gains `Mode::Exact` and the module `exact`: tick columns (`Ticks`:
  integer units at a scale, aligned to the window's finest exponent by the
  caller) reduced as integers. Sums are `i128`; squares and cross products
  are a 320-bit integer of the crate's own (`exact::Wide`, no dependency);
  every `f64` is one correctly rounded division of the exact rational
  (`exact::ratio_to_f64`: the quotient to 54 bits, the remainder as the
  sticky bit, ties to even), which `crates/abt-agg/tests/exact.rs` holds
  against an independent big-integer oracle on random ratios, ties and
  near-ties. `ExactSums` slides a window by integer arithmetic alone, so
  the exact mode needs no rebuild schedule (E-27 is `running`'s). Order
  statistics select units; `median` of an even count is the rounded
  rational; `quantile`'s interpolation is the two-pass formula over the
  neighbours' views, as its level is a float. Over float columns `exact`
  is `two_pass` (A-D47). The data layer's pass maps `Numerics::Exact` to
  `Mode::Exact`; it reduces tick columns through `reduce_ticks` once the
  hot store answers them (iteration 1, M-2). The kernel's `numerics exact`
  is iteration 2's (K-2.2).

- **E-86 (ST-10.1, A-D36, A-D43; `abt-codec` as built).** The crate's
  modules follow ST-3 to ST-5 and ST-5.10 as pure functions over slices.
  What the documents leave open or that differs: the block header's
  `flags` carry a fourth bit, `f32`, so a block of a source that wrote
  `f32` values decodes to those bits (ST-4.3 names three); a value that
  does not reproduce on the block's grid is kept as a raw-bit exception
  (`Exception::Raw`) and the block's sums refuse `exact` (ST-3.2's
  "listed"); widths 0 to 64 have one unrolled kernel each, generated by a
  macro over a dispatch table, with 32-bit words to width 32 and 64-bit
  above; the wavelet matrix answers a monotone predicate (`count_where`),
  the form an adjusted view's threshold takes (ST-5.10), beside the value
  threshold; `adjust` evaluates pieces from the event-interval summaries
  and the two decoded ends and leaves the k-th smallest of an adjusted
  range to the data layer's query path (a binary search on the counts,
  ST-5.10), with the matrix over the adjusted column for more than eight
  pieces; Criterion (no default features) is the bench harness, the rows
  of ST-2.4 under `cargo bench -p abt-codec`. The segment byte layout and
  the directory are the data layer's (M-2, A-D48).

- **E-87 (ST-4 to ST-7, A-D36 to A-D38, A-D48 to A-D50; the hot store as
  built, first part).** `crates/abt-data/src/hot/` holds a version of a
  hot relation as `hot/<relation>/<version>/` (its manifest and its
  presence file: labels, instruments in id order with their alive
  intervals, sequence runs, exponents and bitmaps) over content-addressed
  segments under `hot/segments/` (A-D48), mapped read-only and opened once
  per process; the block tables and sparse tables are rebuilt at open from
  the runs; a decoded-block cache (`ABT_BLOCK_CACHE`, 256 MB) backs the
  range rules of `abt-codec`. `crates/abt-data/src/staging/` runs S1 to S5
  on every ingest of a hot relation (`Store::write_hot`): S1 normalises the
  conformed rows per instrument (identical rows merge, conflicts refuse);
  S2 classifies them against the current version (appends, duplicates,
  revisions, late rows, counted); S3 encodes every rebuilt instrument into
  new segments and keeps the others' runs (a change of the id order copies
  the kept blocks byte for byte, so the version's id is unaffected); S4
  decodes every new block against its source, recomputes its summaries and
  cumulatives, and checks random ranges and the presence of the opened
  version; S5 renames the staging directory into place; the registry then
  records the version as before. What differs from the documents or is
  not yet built:
  - A version's rows are the stream's unit through its buckets (the
    partition calendar's, E-14): a bucket is a virtual partition that
    `read_partition_rows` decodes and sorts by receipt. The per-instrument
    cursors of ST-6.3's `subscribe` row are iteration 3's (M-8).
  - A revision keeps the superseded row whole in the manifest (its values,
    receipt, sequence and the receipt that superseded it), not one entry
    per column (ST-5.9); a receipt-filtered read substitutes it, a read of
    every revision emits it, and a revised instrument is rebuilt whole
    (copy-on-write at the instrument, not the block).
  - S0's landing log exists (`staging::land`) and no adapter writes it
    yet; S1's batches stay in memory; S6's cold copy is not built. OHLC
    prediction (ST-4.5), cross-moments, the ladder (ST-5.3), wavelet
    matrices at staging (ST-5.7), the catalog structures and event-interval
    summaries follow in the next commits of M-2; appends, tails, the seal
    journal, consolidation, recovery and residency are M-2c's.
  - The adapters switched: the dataset source (synthetic markets and
    `bundle_v2` bundles, through both the data layer's CLI and the front)
    stages its five bar relations hot; the golden runs are unchanged. The
    vendor adapters follow, each in its own commit.
  - `crates/abt-data/tests/hot.rs` holds the hot store against the
    Parquet store on every read form of the store for the same rows,
    through a second ingest with appends, a new instrument, a revision and
    a late row: equal byte for byte but for the sequence numbers, whose
    numbering differs by design (A-D50) and whose semantics (a snapshot is
    exactly the rows with seq ≤ N) are checked per store.

- **E-88 (ST-10.4, D-6.3, P-1.1, A-D44, A-D47; ticks on the wire and the
  exact pass as built).** A `query` with `ticks = true` (and `abt data
  query --ticks`) over a hot relation answers its `Num` columns as
  `Decimal128(18, s)`, `s` the finest exponent among the version's tick
  columns, with predicates compared on the decimal view; over a Parquet
  relation `ticks` is ignored and the floats come, as A-D44 says a reader
  must accept. The aggregate pass under `numerics = exact` over a hot
  relation reads the ticks (`Store::read_ticks`) and walks them through
  `abt-agg`'s windows, which are now generic over their value (`Value`:
  `f64` reduced by `two_pass` or `running`, `i128` units reduced by
  `reduce_ticks`), so the pass and a direct `abt-agg` reduction of the
  same ticks give one answer (P-1.1 on the data layer's side). A column
  holding a value off any decimal grid (a raw exception, ST-3.2) refuses
  `exact` by name and answers `two_pass`; `exact` over a Parquet relation
  stays `two_pass` (A-D47). The running sums of `rows(N)` windows remain
  the floats' (E-27); the exact window re-reduces its rows at every
  label, O(N) per output, which the sliding `ExactSums` of E-85 can
  replace when phase 4 measures the need.

- **E-89 (ST-5.3, A-D46, Proposition 5.2; the calendar ladder as built).**
  A hot bar relation (one value column, a bar rule by its name: `open`
  first, `high` max, `low` min, `close` last, `volume` sum) carries its
  ladder: for a day base the week, month, quarter and year levels; for a
  minute base 5, 15 and 30 minutes, 1 hour, day, week and month. Each
  level is a relation of its own in the block format (`<relation>@<level>`:
  the bar's value at the instrument's exponent, the base rows it
  summarises, the sequence number of its last base row, and the receipt
  residual from the bucket's start) with a presence over the level's
  labels, built from the nearest finer level and verified at S4 against
  the base rows of every instrument on every ingest. What differs from
  ST-5.3 or is not yet built: trade dates are UTC dates until the session
  table names them (the `session` level and the futures' day by session
  are iteration 3's, with the Databento adapter); every instrument's bars
  are rebuilt at every version (a level's segments are not shared between
  versions, since a coarser bucket's label and receipt move when base
  labels are appended; M-2c's open buckets replace this); no reader
  consumes the levels yet: the aggregate pass's bucket windows keep their
  generic walk, since a bucket window's receipt there is the latest
  receipt among the rows read (the pass's rule) and not the bucket's end
  (the ladder's, ST-5.3), and the kernel's bar views over coarser
  calendars reach the ladder with M-8. `Store::read_level` answers a
  level's bars for tests and tools.

- **E-90 (ST-8, ST-9, ST-10.2, A-D39, A-D41, A-D42, A-D45, A-D51; appends,
  recovery, consolidation, residency and the map request as built).**
  An append (a row after its instrument's last stored label, at a label
  its tail does not hold) is logged, then pushed into its instrument's
  tail in memory under the published version, with its own sequence
  number (ST-8.2); the version id stands, and the version's partitions
  (the stream's buckets) grow with the appended rows' spans and counts. A
  tail of 1,024 rows is sealed: one block per column family (the value
  columns, the receipt residual, the labels and the sequence numbers)
  appended to the family's append file and synced, then a journal record
  (BLAKE3-framed) synced; only then is the block visible (ST-8.3). Rather
  than appending in place into the published tail block (A-D39), the
  published blocks stay immutable in their content-addressed segments
  (A-D48) and the tail is a structure of its own; the spec's in-place
  append is deferred to the measurements of phase 4 (the live path's
  figure of ST-8.5 is per-row work either way). A row at a label the tail
  already holds is a revision, not an in-tail replacement: the presence
  holds one row per label, and a superseded row belongs in the revision
  table a version build maintains (S2 to S5), so ST-8.5's "inserted by
  re-encoding the tail" covers a new label inside the tail's span only.
  Restart (ST-8.8): the manifest, then the journal (bytes of the append
  files past the last good record truncated), then the tails from the
  log, from the manifest's `replay_from` (after a consolidation, the
  least sequence number a tail held; else after the published
  high-water): `crates/abt-data/tests/appends.rs` (a clean restart, a
  torn journal, junk past a journaled block) and
  `crates/abt-data/tests/recovery.rs` (a child process aborted at each
  fault point of `src/fault.rs` or killed at a random moment; every
  answer at every durable sequence equals a never-killed twin's; no
  unjournaled block visible; the durable batches whole). Consolidation
  (ST-8.7, `hot/consolidate.rs`; `abt data consolidate`, and on its own
  past `CONSOLIDATE_BYTES` of append files): the sealed blocks folded into
  base segments in instrument order, each copied as its encoder produced
  it where it follows full blocks (its hash carries over, checked), the
  rows from a partial published block on re-encoded into full blocks
  (A-D51; checked by decoding), the presence and buckets extended, the
  manifest renamed into place under the next journal generation (the old
  journal and append files removed at the next open), the tails rebuilt
  from the log; the version id is the published state's and is never
  recomputed. The block hash excludes the header's exception locator (a
  position in the run, not content). A relation a source holds hot stays
  hot: a write without the `storage` attribute follows the current
  version, and a write asking for Parquet is refused; an empty Parquet
  version (a relation registered without rows) does not pin it. An
  instrument appended to since publication is rebuilt by the next version
  build (its runs are not reused). Residency (ST-9.1, `hot/residency.rs`):
  `abt-data serve --profile workstation|dedicated` prefaults and locks
  every current hot version's segments before binding the socket, best
  effort; the bytes locked and refused, the major page faults, the
  restart-to-serving time, the profile and its kernel limit are the
  `_data` row of `stats` (a row under that relation, left out when stats
  are asked for one relation); every version's row carries its storage
  and, when hot, its segment and append bytes, tail rows and journal
  records. The shedding order of ST-9.2 is not built: nothing is shed
  until the sizing run says what must be. The `map` request (ST-10.2)
  answers a table: one row per segment of the columns asked for (file,
  bytes, codec version, hash), per append file (its journaled bytes) and
  per tail (its instrument and the rows visible at the view's sequence),
  restricted to an instrument range;
  `tests/appends.rs::the_map_request_describes_what_a_reader_decodes_with_the_codec`
  maps a segment and decodes a block with `abt-codec` to what a query
  answers (ST-10.3). Not built: the live feed's minute barriers sealing
  ladder buckets (the ladder is rebuilt at the next version, E-89); the
  open ladder buckets of ST-8.4; consolidation as a background task
  (it runs under the store's write lock); the extra-memory bound of the
  consolidation test row (one segment plus the append files) is by
  construction (one column at a time) and not measured.

- **E-91 (ST-7.4, ST-9, ST-13, K-6.5, A-D49, A-D52; iteration 1, phases 3
  to 5).** The falsification pass of `docs/spec/TRACE.md` is
  `scripts/mutations/*.patch` run by `scripts/mutate.sh`: each patch breaks
  one proposition's implementation and names the test that must then
  fail; the pass fails when a patch does not apply, when its test passes
  under the mutation, or when the tree is left changed. The vendor
  adapters' bar relations are held hot as A-D49 states for every market:
  `adapters::Fetched::ingest` merges `adapter::hot_bars` into the
  per-relation attributes (a relation's own attributes come after), so
  Norgate and Databento ingests stage through S1 to S5 like the synthetic
  and bundle markets; the orats option relations stay in Parquet (ST-11).
  G-ST2's harness is two commands rather than `abt data stage --measure`:
  `abt data ingest --measure` prints every hot version's staging report
  (S2 counts, S3 bytes per structure, S4 checks, the milliseconds of S1 to
  S5) as JSON lines, and `abt data stats --measure` prints ST-9's lines of
  each hot version from its files (every level's columns and presence,
  the manifest, the directory's locators, the append files, the tails)
  with bytes per row. A `live-sim` run takes the deployment document
  (A-D52): `--session S` or the document's `recording stream S` names the
  session, and the two must agree. `scripts/bench.sh` measures the
  ingest's peak RSS only where `/usr/bin/time` exists; the encoding
  study's scripts skip their ORATS sections when no ORATS sample is
  present. Not built: the live feed's appends through the vendor adapters
  (iteration 3); the Databento half of the sizing run on the full history
  (the bucket's per-day objects; `docs/iterations/01-foundation.md`).
- **E-92 (ST-3.2, ST-5.1, ST-13, D-2.4, K-6.5, A-4.1, A-D52, A-D53,
  A-D54, A-D55, A-D56; iteration 1, phase 5).** Five findings of the
  phase 4 and 5 runs, each fixed with its test. The kernel service's `live-sim` arm took the
  session from the request alone, so a run whose session the deployment's
  `recording stream` names was refused (phase 5's rehearsal went through
  the front, which resolved it): `kernel/service.rs` now derives it with
  `live::session_of` as the `live-broker` arm does, and
  `kernel_service.rs::the_live_modes_refuse_what_they_cannot_do` covers
  the three cases (the name and the stream agree, they differ, neither is
  given). The Databento subset failed S4 ("moments over [1026, 3544)
  differ from the direct reduction"): the adapter back-adjusted prices in
  `f64`, so cent-grid prices (CL) came out an ulp off every decimal grid
  after a roll and their blocks held them as raw exceptions, and for such
  a position the writer's summaries counted 0 (`staging/build.rs`:
  `Encoded::integer` is `None`) while both readers decoded the block's
  base (`hot/version.rs::decode`, `hot/segment.rs::integers`: `patch`
  restores offset exceptions only). A-D54 states the rule: a raw position
  is 0 in the decoded integers too (`abt_codec::block::zero_raw`, called
  by both readers), the `f64` view keeps the bits, and a `ticks` read of a
  relation whose tick column holds one is refused naming the column, as
  `numerics exact` already was (`store_hot.rs::read_ticks`);
  `tests/hot.rs::a_value_off_every_grid_keeps_its_bits_counts_as_zero_and_has_no_tick_view`
  and the mutation `scripts/mutations/st-3.2-raw.patch`. A-D53 removes
  the cause: `adapters/databento.rs::futures` computes the roll gaps, the
  sessions' adjustments and the adjusted prices as integers on the root's
  grid (the smallest decimal exponent at which its prices and its
  contracts' tick sizes are integers: a definition's tick can be coarser
  than the prices traded, as the subset's GC bars at 0.01 under a 0.1
  tick showed), divided once, so the stored series stays on the vendor's
  grid
  (`tests/databento.rs::back_adjustment_in_tick_units_keeps_cent_prices_on_the_grid`).
  The `--undefined refuse|drop` option of the Databento adapter leaves
  bars without a definition out and counts them, since the subset's
  definitions do not cover every contract that traded; `refuse` stays the
  default (D-2.4). The second rehearsal of phase 5 (the same program,
  binding and mode, a new session) got the first's run id and halted when
  the data layer refused its `book` rows as a conflicting revision of the
  first run's: a run's id was (H(P), β, π, mode) with π blind to the
  session. A-D55 puts the session a live run reads, and the run id a
  `--replay-of` run replays, into π (`bind::run_step`); replay-sim runs
  over a binding alone keep their ids, so no golden moves
  (`replay.rs::a_run_id_names_the_session_it_reads_or_the_recording_it_replays`).
  With its own id, that rehearsal ran through the kernel service to the
  end, but the replay of its recording decided differently: the store
  held three sessions of the same two instruments, and the resolver,
  with no preference to go by, bound the first source by name (`s1`) for
  the live run and its replay alike, so the replay read the oldest
  session's rows under the newest's barriers. A-D56 adds `source(s)` to
  the demand's preferences (`PreferenceKind::Source` on the wire, the
  named source's versions first in `resolver/plan.rs`), the kernel
  names its session in a live run and, in a replay, the session the
  recording read, which `service.rs` now records by putting
  `kernel.<run>.clock` under the session's name rather than `live`
  (`bind::recorded_session`); the resolver also holds each current
  version under the source it is current for, where it read the
  version record's source, which two sessions' empty preludes (one
  content, one version, one record) had made the same; `resolver.rs::a_source_preference_binds_that_source_first`
  and `replay.rs::a_live_run_and_its_replay_bind_the_session_among_other_sources`
  (a decoy source sorting first in the store).

- **E-93 (ST-8.3, ST-8.8, ST-9, P-2.2, A-7.3; iteration 1, the books).**
  Two findings of running MW14 on the three programs, each fixed with its
  test. (1) The data service holding MW14's store
  (`data/store/mw14-lake-1990`: the lake since 1990 staged in seven
  five-year chunks, each an ingest appended to the current versions, so
  9.2M of each bar relation's 15.9M rows sit in tails) took 7.49 GB of
  anonymous memory right after opening, before any request, and the run's
  bind-time reads (whole-relation `Distinct` and `Scan` queries over
  day-keyed relations of 15.9M rows, 1.6 GB transient each) took it past
  the container's 13 GB; the kernel killed it at 11.8 GB resident. A tail
  was a `Vec<TailRow>` per instrument, each row a struct with two heap
  vectors (its integers, the source's values): 163 bytes a row measured,
  46M rows. ST-8.3 holds a tail as one packed horizontal block. The tail
  is now `hot::append::Tail`, the rows as columns (labels, receipts,
  sequence numbers, one integer vector per value column) at 32 bytes a
  row, the source's bits kept beside only where the integer does not
  reproduce them at the instrument's exponent (a value off every grid,
  A-D54, or an integer past 2^53 after a raise), trimmed after the
  restart's rebuild; the hot read path visits tail rows without
  allocating per row. Measured on the same store: 2.83 GB anonymous after
  opening (`abt-data stats --measure`: `tails (columns in memory)
  294,703,968 B, 9,209,499 rows, 32.000 B/row` per relation). The packed
  tail of ST-8.3 (`W` bits a value) is the remaining distance, and the
  restart's rebuild of 46M tail rows from the log takes 34 to 70 s on
  this VM against A-7.3's 30 s (92 ms on the 2014 store, whose tails are
  empty): both iteration 2's, with a consolidation that folds tails.
  `hot/append.rs::tests::a_tail_gives_back_every_bit_of_its_rows`;
  falsified by `scripts/mutations/e-93-tail-bits.patch`. (2) The first
  event batch of MW14's stream was refused on the wire: `Value at
  position 0 out of bounds: 3193 (should be in [0, -1])`.
  `abt_wire::table::TableEncoder::batch` wrote a batch and, when its IPC
  bytes passed a frame's 64 MiB, discarded them and wrote the two halves;
  the `StreamWriter`'s dictionary tracker had counted the batch's
  dictionaries (the label columns' registry labels) as sent, so the
  halves went without them and the reader validated their keys against
  an empty dictionary. No stream before MW14's had carried a label column
  in a batch that large. The bytes are now cut at the frame's limit (the
  `StreamDecoder` reads a byte stream cut anywhere; the client's
  `next_batch` already took a frame that completes no batch).
  `abt-wire/tests/table.rs::a_batch_larger_than_a_frame_keeps_its_dictionaries`
  (6M rows, a 4,000-label dictionary column and a float column, 72 MB of
  IPC) failed with the same error before the fix; falsified by
  `scripts/mutations/p-2.2-frame-split.patch`.
