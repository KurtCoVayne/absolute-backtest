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
  exactly, under the batch driver and the fold until the batch driver is
  deleted) and by G6 (`tests/reference.rs`, the pandas reference). The owner
  runs `scripts/parity/` for G1. A golden file changes only in a commit that
  names the semantic change it accepts; each such change is listed under
  "Blessed changes" below.

## Blessed changes of the golden files

None yet. The baseline was taken after E-2 and E-3 below, which made the fold
and the batch driver agree on every corpus program.

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

## Layout

- **E-26 (M-0).** Integration tests that drive the `abt` binary live in
  `crates/abt/tests/`, since a binary's path is visible only to its own
  package's tests; suites that span layers live in a test-only crate
  `crates/abt-tests` (with the shared fixtures as its library); each layer's
  own suites live in its crate.
