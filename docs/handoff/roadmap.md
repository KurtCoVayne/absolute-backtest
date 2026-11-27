# Roadmap: speed first

Nov 24, 2026. The order the owner chose: the performance target first, deployment to the pipeline after.

## The target

MW14, full replay (1991 to 2026, 4,240 names), from the store the Norgate adapter stages, equal to the canon's 1,852 weekly returns, in **25 s wall time on the benchmark machine**. The gate before it is the specification's own: **60 s and 1.5 GB** (A-7.3).

Three choices are inside that sentence. Each is the owner's to change.

| Choice | Taken here | Why | The alternative |
| --- | --- | --- | --- |
| Reference machine | `m8-bench2` (`m8g.xlarge`, 4 vCPU, 16 GB) | Python's 25 s, the old engine's 366 s and the new engine's 367 s were all measured on this instance type (the first two on an earlier instance, Nov 4 to 5) | The Apple M4 that A-7.3 names. Issue #79 puts it at about 2.4 times faster per core, so every number here would shrink by about that factor |
| Wall time or CPU time | Wall time, threads allowed | It is what the owner asked for, and Python's 25 s uses about three cores | Single-thread parity, which would be 75 CPU-seconds |
| Which store | The lake through the Norgate adapter | It is the path with no hand-built bundle | The v1 weekly bundle, where the old engine is exact |

What the design promises on this machine: `docs/spec/06-performance.md` §5.1 projects 6 to 10 s for MW14 on the M4. That is about 14 to 24 s here if issue #79's per-core ratio carries over. Two cautions on that figure: the spec does not say how many threads its estimate assumes, and the ratio was measured on the old engine. So 25 s sits at the edge of what the design projects, and package 4 is the margin.

## The order

| # | Package | Gate | Depends on |
| --- | --- | --- | --- |
| 0 | One green tree | CI green on the head of PR #80; #80 merged | |
| 1 | A benchmark that runs itself | Reproduces 367 s and 0.9777 | 0 |
| 2 | Stop streaming what the kernel only buckets | The run receives on the order of 2.4M tuples; results unchanged | 1 |
| 3 | The column-block evaluator | Every stage equal to today's evaluator and faster at size; with package 2, 60 s and 1.5 GB | 1 |
| 4 | Parallel on top | Wall time at least halves from one thread to four; 25 s | 3 |
| 5 | Parity through the adapter | 1,852 of 1,852 weeks | 1; closed before the target is claimed |
| 6 | R8L on the full minute history | 3 s and 0.6 GB, 21,892 legs | 2, 3 |
| 7 | The pipeline | In the pipeline's document | 0 and three engine changes of its own; then 2 and 3 as they land |

Packages 2 and 3 are independent of each other and can alternate. Neither meets the 60 s gate alone: by the Nov 6 clock, package 2 can remove at most 140 s and package 3 works on the other 216 s. Package 5 runs beside them.

## Package 0: one green tree

**Why.** The branch every later change builds on has failing CI, an unbuilt experiment in its working tree, and a fix it needs sitting uncommitted in another tree. Measuring anything on it now would measure an unknown state.

**Steps.**

1. Make the benchmark machine reachable (`benchmark.md`, "The machine"). It creates an IAM role and starts an instance, so it needs the owner's go-ahead.
2. Set the `SmallVec` experiment aside, so the tree matches the head. It is already saved as `docs/handoff/smallvec-nov6.patch` (taken Nov 24: 16 files, +107 −76). Confirm the file still matches the tree with `git apply --check --reverse docs/handoff/smallvec-nov6.patch`, then `git restore .`. **Do not use `git stash` for this.** The four engine trees are worktrees of one repository and share one stash list, which already holds the amendments tree's unfinished E-120 work as `stash@{0}`; a new stash would take that place. The experiment is decided in package 1.
3. Fix the lint. With the experiment aside it is at `crates/abt-kernel/src/kernel/eval.rs:605`: `self.memo_stuck.get(&memo_key).is_none()` becomes `!self.memo_stuck.contains_key(&memo_key)`.
4. Reproduce the test regression **on the machine**: `cargo test -p abt-tests --test kernel quantile_conventions_and_the_aggregate_halt`. The suspected cause is a memory address being reused (`status.md` section 5), which depends on the allocator, so a pass on macOS clears nothing. The cheapest fix is to remove the slot-map cache: the commit that added it measured its three changes together at 114.2 to 113.1 user-seconds on 1,000 synthetic names, which PR #80 itself calls within noise. Keeping the cache needs a key that does not depend on an address, and `atom_slots` is not handed the literal's position today.
5. Run the whole suite on the machine, in process and with `ABT_TRANSPORT=socket`. Since commit `9a255d9`, the 42 `abt-tests` binaries that come after `kernel` in alphabetical order, and the crates tested after `abt-tests`, have never run. Expect findings.
6. Bring the log-replay fix over from `/Users/alejandro/absolute-backtest-amend`: the diff of `crates/abt-data/src/store.rs`, `crates/abt-data/src/store_hot.rs` and `crates/abt-data/tests/appends.rs`, plus the untracked `crates/abt-data/tests/replay.rs` and `scripts/mutations/e-125-replay.patch`. Its two errata become E-128 and E-129 here, and the mutation patch is renamed to match. PR #80 changes no file in `abt-data`, so the code applies cleanly.
7. Write the errata that are missing: the three batch paths of Nov 6, and the `asof_memo` hazard (or fix it: the key needs the entity).
8. Correct the document contradictions listed in `status.md` section 10.
9. Merge #80.

**How a change reaches the machine.** SSH is not used, and nothing is pushed unless the owner asks. So the loop is: the owner approves a push to a work branch, and the machine fetches it from GitHub (the repository clones over https without credentials). Without a push, a patch file goes through S3 (`benchmark.md`, "Inputs"). A single test binary can be built and run on the Mac in the meantime.

**Gate.** `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings` and `cargo test` green in CI on the PR head; the socket pass green on the machine.

**The owner's calls here.** Whether `spec/three-layer-split` or `main` becomes the repository's default branch, and whether `main` is fast-forwarded. Today the default is the old engine.

## Package 1: a benchmark that runs itself

**Why.** Every Nov 6 number came from scripts in a scratch directory that no longer exists. `benchmark.md` holds them as recovered from the session log. Until they are a script in the repository, no change can be judged.

**What to build.** `scripts/bench-mw14.sh`, taking a lake directory, the two series files, the canon and a repetition count, and doing four things:

1. Stage the store once, keyed by a hash of the lake's bar files so a second call reuses it.
2. Serve it, and run the book N times under `/usr/bin/time -v` with `--timing` and `ABT_TIMING_RELS=1`.
3. Compare each run with the canon (`scripts/parity/mw14_compare.py`).
4. Print one JSON line per run: wall and user seconds of the front, the data service's CPU seconds and peak memory, the front's peak memory, the tuples received (`tuples=` on the run's `timing` line) and trimmed (`trimmed_rows`), seconds per phase and per derived relation, the weekly correlation, the count of weeks equal to 1e-4.

The run's clock does not count rows per relation. Add that count to the `timing` output: package 2's gate is read from it.

Add a `[profile.profiling]` section to `Cargo.toml` (frame pointers, debug info), which issue #79 asked for, so a profile does not need `RUSTFLAGS`.

There is no faster cell for iteration between full runs since Nov 26: the synthetic 1,000-name market that served as one (95.8 s on Nov 5 before PR #80's changes, 93.8 s with four threads after the first of them) is retired by the owner's rule that a benchmark reads real history (`benchmark.md`, "Real history only"). The full run takes about 70 s.

**Gate.** Five repetitions reproduce 367 s within 3% and a weekly correlation of 0.9777. Record them in `docs/assessment.md`. If they do not reproduce, that is the first finding and nothing else starts.

**Also settled here.**

- **How much of the run is the data path.** `status.md` section 3 groups one run's clock into up to 140 s on the data path and 216 s in the evaluator and executor, and records that `docs/assessment.md` reads the same run as evaluator-bound. The data service's own CPU time, beside the front's wait, decides between them.
- **The `SmallVec` experiment.** Apply `docs/handoff/smallvec-nov6.patch`, make `crates/abt-tests` compile against the new `Tuple` (61 call sites), and measure. The expectation is that it is dropped: the mechanical changes of the same night bought nothing measurable, and package 3 takes `Tuple` off the hot path.

## Package 2: stop streaming what the kernel only buckets

**Why.** MW14 decides weekly. The kernel receives daily rows, drops 59.2M stored rows behind its horizon over the run, and derives the weekly bars itself, while the data layer holds those weekly bars already. The design's figure for this book is about 2.4M rows (`docs/spec/06-performance.md` §3.3: the weekly views, the catalog relations and the series). Up to about 140 s of the 367 s is attributable to this, of which 55 s is the three derivations that only bucket.

It is also one recorded cause of the pipeline's killed runs: on the futures store, whose base is one minute, a daily program receives every minute bar (`/Users/alejandro/absolute-pipeline/docs/iterations/02-chat.md:205`).

**Three parts.** By rows, 2c is about half: `equity.listed` and `equity.member` are 28.9M of the rows, by the counts of `docs/iterations/01-foundation.md`, open question 7. The gate needs all three.

**2a. Bar views come from the ladder.** When a program reads `close@1w` (or any open, high, low, close, volume view at a ladder level) and the bound version is hot, the stream delivers the level's rows as the view relation, and the base rows do not stream for that view.

- Data side: `Store::read_level` (`crates/abt-data/src/store_hot.rs:600`) exists and is tested; nothing calls it from the stream or the service. The levels are built by `crates/abt-data/src/hot/ladder.rs`.
- Protocol: a relation entry of the stream request needs to name a level (`crates/abt-wire/schema/abt.fbs`). This is a wire change, so it goes first in its batch (cadence rule 5).
- Kernel side: `stream_spec` (`crates/abt-kernel/src/kernel/bind.rs:1621`) asks for the level; the view rule that linking generates (`crates/abt-kernel/src/check/link.rs:1016`) is replaced by the delivered relation when one is bound.
- Two things to settle before building, both in ERRATA E-89. The ladder buckets on UTC days, while the book keys a week at its last trading day (package 5 needs the same answer). And a ladder bar's receipt time is the bucket's end, while a bucket computed from the rows takes the latest receipt among them. The stream is ordered by receipt time, so the two rules give different orders until one is chosen.

**2b. Pushed-down aggregates arrive as relations.** Today a pushed aggregate still streams its input, because the kernel keeps a local fallback (ERRATA E-127 names this as "the spec decision 3.3 needs"). Deliver the data layer's answer as a stream relation in barrier order, drop the fallback for that literal, and stop streaming the input.

- Two forms MW14 needs are missing in the data layer's pass (`crates/abt-data/src/agg.rs`): the mean of a product of two columns (`dvol` is the weekly mean of daily close times volume) and `count` (`traded@weekly`).
- An aggregate request has no instrument filter (ERRATA E-99), so the pass computes every instrument in the store. Add the filter.
- Bit identity: sums of floats must go through `abt-agg` in the canonical order on both sides (decision A-D8), or `eval_set` and `tv_golden` will differ in the last bit.

**2c. Membership and listing as spans.** `equity.listed`, `equity.ticker` and `equity.member` are stored one row per ticker per day: 15.9M, 15.9M and 13.0M rows. `equity.ticker` no longer streams for MW14 (ERRATA E-127); the other two do. The lake holds them as date ranges. As spans the kernel answers `member(A, T, L)` by an interval lookup and the stream carries thousands of rows. Open question 7 leaves it to the owner: "the owner's call is whether the listing relations stay day-keyed (01-data's Norgate rows) or become spans". It changes what the adapter writes, so it is a store rebuild.

**Gate.** The run's `tuples=` count falls to the order of 2.4M, and `trimmed_rows` falls from 59.2M with it. The Nov 6 value of `tuples=` was not recorded in any document; package 1 records it. Results are unchanged: `eval_set`, `retention`, `tv_golden`, `determinism` and the benchmark's comparison with the canon give the same numbers as before the change. TRACE rows `ST-5.2` (the exact ladder) and `06-performance 3.3 stream projection` move to done.

**Expectation, not a measurement.** Removing the whole share would leave about 230 s. The firm part is the 55 s of bucketing derivations. The 75 s wait and the 10 s of insertion should fall with the row count, which is an inference until package 1 measures the data service. The memory of the front should fall well under 3.7 GB, since most of it is stored daily rows.

## Package 3: the column-block evaluator

**Why.** About 216 s of the 367 s is the evaluator and the executor working one row at a time: a tagged 16-byte value per cell, an environment per rule instance, a hash lookup with an allocated key per derived atom. `docs/spec/06-performance.md` §3.1 and §3.2 specify the replacement, and `approach.md` section 10 lists what exists of it, which is little. This is the largest piece of work in the project.

**How to land it.** In six stages, each merged on its own, each held equal to today's evaluator. The instrument is `crates/abt-tests/tests/eval_set.rs`, which already holds the tuple evaluator, the set evaluator and the parallel set evaluator equal on every corpus program and both books. The new evaluator becomes a further leg of that test, selected with `--evaluator block`, and becomes the default only at the end. A literal kind the block evaluator does not yet handle falls back to today's `step` over the block's rows, so coverage grows one literal kind at a time and every stage runs the whole corpus.

The last column splits the Nov 6 clock without counting anything twice. The 10 s of insertion is in package 2's share.

| Stage | What | Replaces | Its share of the Nov 6 clock |
| --- | --- | --- | --- |
| 3.1 | **Typed chunks for stored relations.** One chunk per bar: sorted entity ids as `Vec<u32>`, one typed vector per value column. Chunks held in a deque over the retention horizon. A row-shaped read adaptor keeps the present evaluator working on top | `Store` and `Rows` (`crates/abt-kernel/src/kernel/mod.rs:1832`, `crates/abt-kernel/src/kernel/rows.rs:12`); `trim_stores` becomes popping the deque | No phase of its own; `Value::cmp` is 6.6% of the profile |
| 3.2 | **An integer-keyed block memo.** A derived relation's result at a bar is one chunk, found by (relation id, bar index). An index of each entity's last bar replaces the as-of walk | `Memo` and `MemoKey` (`mod.rs:48`, `mod.rs:41`), `asof_memo` (`mod.rs:2209`) and its cross-entity hazard, `evict_memo` | 17 s evicting; `Memo::get` is 3.8% of the profile |
| 3.3 | **Block plans.** Each rule lowered to the plan of §3.2: generators as merge joins on sorted ids, tests as filters over columns, assignments as computed columns, negation as a bitmap operation, cross-sectional reductions (`rank`, `top`, `within`) sequential over the block | `solve_kleene_block` calling `step` per row (`crates/abt-kernel/src/kernel/eval.rs:958`, `eval.rs:1609`) | 74 s: `prev_row` 12, `dividend_at` 12, and 50 s of derivations the clock does not itemise |
| 3.4 | **Windows over chunks.** A window's state per entity, found by entity id and fed once a bar from the bar's chunk (below) | `RowsState` behind a hash of the group's values, and the two prefetch paths (`eval.rs:314`, `eval.rs:2533`, `eval.rs:2211`) | 71 s: `clenow` 38, `eligible` 17, `ma` 16 |
| 3.5 | **The executor's book over entity ids.** Positions, marks and working orders as vectors; corporate actions read from the bar's chunk | The scans in `bar_field`, `has_price`, `bar_volume` (`mod.rs:3537`, `mod.rs:3607`, `mod.rs:3642`) and the executor in `crates/abt-kernel/src/sim/` | 54 s of fills and marks |
| 3.6 | **A bump arena per barrier**, and removal of what per-instance allocation remains | The global allocator on the barrier path | `malloc` and `free` are 14% of the profile |

Order the literal kinds of stage 3.3 by what MW14 uses: positive atoms, comparisons, arithmetic assignments, negation, the previous-row read. `corpus-v2/company/mw14.dsl` is the list.

**Stage 3.4 and the numerics.** The kernel already keeps the last N rows of each window group (`RowsState`), and under `numerics running` it advances sums in O(1) with `abt-agg`'s `ShiftedSums`. Both books run under `numerics two_pass` (their execution presets, `book_fixed_base` and `futures_fixed_base` in `corpus-v2/lib/execution.dsl`), so that the weeks equal the canon to the last bit. Under `two_pass` a window's value is a reduction over the rows held, at every bar: 4 to 52 rows for MW14. So the O(1) advances of the design's estimate (`docs/spec/06-performance.md` §5.1) are not available to the books as they are pinned. What stage 3.4 can remove is what surrounds the reduction: the per-row solve of the window's inner conjunction, the rows held as `Vec<Value>`, the hash lookup of the group, the allocated memo key. `abt-agg` has the layout to reuse: `RowsWindow` (`crates/abt-agg/src/window.rs:122`), the `rows(N)` window the data layer's pass uses. `WindowState` in the same file is for time extents and reduces every held row at each read.

Whether `two_pass` stays pinned is the owner's call. `docs/parity-mw14.md` records that on the old engine the running sums gave the same 1,852 weeks, with the caution that a last-bit difference could move a name across a band edge.

**Gate.** At every stage: `eval_set`, `retention`, `tv_golden`, `determinism` green, the benchmark's comparison with the canon unchanged, and the benchmark faster. At the end, with package 2: 60 s and 1.5 GB on the benchmark machine. TRACE rows `F-5.3 / M-8 entity-parallel evaluation`, `06-performance 3.1 retention horizon` and `A-7.3 MW14 full replay` move to done.

**Risk.** A stage that passes the suites and is slower at size. The suites run small synthetic markets; only the benchmark shows it. Run the benchmark at every stage, and revert a stage that does not pay.

## Package 4: parallel on top

**Why.** The parallel cut that exists (ERRATA E-125) works per rule application and spawns threads at each step. On a row-at-a-time evaluator it gains nothing: on the synthetic 1,000-name market, 16.3 s of a 93.8 s run was inside the steps it cuts. On block plans the entity-local work is the bulk of the run, and the design's schedule applies: entity-local strata over fixed ranges of entity ids on a persistent pool, cross-sectional strata sequential (`docs/spec/06-performance.md` §3.2, Proposition 3.1).

**Where.** `crates/abt-kernel/src/kernel/parallel.rs` is rewritten on top of package 3's chunks. `warm_strata` and the per-entity ordering it guarantees stay.

**Gate.** `crates/abt-tests/tests/determinism.rs` holds one thread equal to four. Wall time on the benchmark falls by at least half from one thread to four, and reaches 25 s. TRACE row `X-3.1` moves to done.

## Package 5: parity through the adapter

**Why.** The target says "equal to the canon". Through the Norgate adapter the run correlates 0.9777 with it. Speed on a book that gives different returns is not the goal.

**Steps.**

1. **Separate the kernel from the adapter.** `docs/parity-mw14.md` says the kernel is at the canon "through the v1 bundle", and no full-size run of the three-layer engine on that bundle is recorded. Its recipe ("Reproduce", second block) ingests the old engine's weekly bundle with `--adapter bundle_v2 --class equity` and runs `corpus-v2/company/mw14.dsl` on it. **That recipe may not bind today, and it has not been tried**: the program demands `bars @1d … complete` and `require reviewed`, the bundle is weekly and unreviewed, and the resolver refuses a stored calendar coarser than the one asked for (`crates/abt-data/src/resolver/plan.rs:79`). Try it first; it is one ingest and one run. The bundle is built with the old engine (branch `perf/mw14-profile`): `scripts/ingest/ndlake_weekly.py`, then `abt bundle build`, which are stages A0 and A1 of `/Users/alejandro/stageanalyst/strategy/ivmom/mk_engine_bench.py`. If it binds and is exact, the kernel is right and the difference is in what the adapter stages. If it does not bind, step 2 carries the separation alone.
2. **Compare the two inputs relation by relation** at weekly keys: the adapter store's weekly views (`abt data query --parquet`) against the weekly bundle's files. The weekly close, the dollar volume, membership, the total-return price the executor fills at, the split factor. The rules the old ingest script applied are listed in `docs/parity-mw14.md`, "How the book maps onto abt". Each is a candidate: the week keyed at Friday for a name that last traded on Thursday, membership at the name's own week end, the total-return index as the fill price, the float32 `$SPXTR`.
3. **Compare holdings week by week** once tickers can be read from the store (PR #80 notes that no `equity.ticker` relation is exposed).
4. Check whether issue #73 (spin-off distributions booked a session early) is one of the causes.
5. Fix in the adapter or in `corpus-v2/company/mw14.dsl`, whichever the comparison points at.

**Gate.** 1,852 of 1,852 weeks to 8e-15 through the adapter's store. TRACE rows `G1` and `ST-13 parity` move to done.

Do steps 1 and 2 before package 2a. They say how a week must be keyed, which 2a needs.

## Package 6: R8L on the full minute history

**Why.** R8L is the second company book and the only intraday one. It has never run in full on the three-layer engine: the trace records a seven-root, one-year subset that made no decisions. The pipeline's intraday requests fail for the same reason (1.2 ms a minute bar per root).

**What it needs.** The design's path for R8L (`docs/spec/06-performance.md` §3.3): the kernel receives 178,000 decision barriers and the session features as pushed-down relations, and the executor's fills are 44,000 point queries, instead of 47M minute tuples. That is package 2b applied to session calendars, with package 3 underneath. Two known defects are in the way: `true_range@session` is empty on the Databento store, and the futures trade date is a UTC date until a session table exists (TRACE row `A-D46`).

**Data.** The staged futures history exists only on the pipeline machine. The reference legs are described in `docs/parity-r8l.md`.

**Gate.** 21,892 of 21,892 legs; 3 s and 0.6 GB (A-7.3). TRACE row `A-7.3 R8L full replay` moves to done.

**State on Nov 26.** R8L runs in full on real history: the 26 roots staged from Databento with each root's primary session (E-176, `scripts/r8l-store.sh`), to the session of 2026-11-25, in 449.6 s and 8.1 GB with a decision barrier at 5.0 ms (`benchmark.md`, "R8L's store"). The gate's count is the parity bundle's, TradeStation's bars from 2000; on Databento's from 2010 the run makes 94.8% of the book's legs over the days both cover, 99.92% of them in the same direction. Open, in the order of what they cost:

1. **A list of labels per instrument in the aggregate request.** The pass walks one list for every instrument, and the sessions calendar's is every root's closes together, so the kernel reads the pass's session bars only for a root no other label cuts (E-177) and evaluates the others itself: about 70 s of R8L's 450 and 4 ms of its 5 ms decision barrier. A change to `abt-wire`, `abt-data` and `abt-agg`.
2. **The memory.** The kernel peaks at 8.1 GB (3.6 GB at its first barrier) and the data service at 5.5 GB, against the gate's 0.6 GB. The design's path above (the session features pushed, the fills as point queries) is not built.
3. **`sessions of "ROOT"` on a vendor store.** The calendar names an instrument by the symbol's name and the root's series there is `ROOT.v.0`, so R8L's report calendar is empty and its per-period metrics and `--returns` with it. The instruments table resolves the root by its ticker; the calendar should too.
4. **The roots where the vendors differ.** KC and SB (80% of the book's legs), HE (84%), LE and PL (87%): holidays, early closes and missing minutes, not looked at one by one.

## Package 7: the pipeline

Specified in `/Users/alejandro/absolute-pipeline/docs/iterations/03-engine-handoff.md`. In short: build the merged tree on the pipeline machine, verify and serve the store with the corrected point values, and rerun the posts that failed.

**Three engine changes it needs that no package above builds.** They do not move MW14's time, which is why the speed-first order does not contain them. Each is small beside package 3.

| | Change | Where the code stands | Why the pipeline needs it |
| --- | --- | --- | --- |
| a | The retention horizon on the plan path | `trim_stores` is set only by `abt run`'s command line (`crates/abt-kernel/src/cli.rs:586`). It is off by default (`crates/abt-kernel/src/kernel/mod.rs:1120`), the kernel service builds a run's configuration without it (`crates/abt-kernel/src/kernel/service.rs:122`), and `KernelRequest` has no field for it (`crates/abt-wire/schema/abt.fbs`) | The pipeline's backtests are `abt plan run`, which goes through the kernel service. **As PR #80 stands, merging and deploying it does not bound a plan run's memory.** A service is also read after its run, which is why `abt run` keeps the whole history for ledgers and state tables; the same decision has to be made for the service. `crates/abt-tests/tests/retention.rs` should then hold a plan's runs too |
| b | A lower bound on the stream | `StreamSpec` (`abt.fbs`) names relations, calendars and instruments, and no start time. A stream carries each bound version from its first row | A six-month check on a minute base receives every bar since 2010 (`02-chat.md:205`). The reach the retention horizon already computes gives the bound for a program whose lookback is bounded. Not designed here. Measured with a real agent's program on Nov 26 (`docs/assessment.md`, "The pipeline's agents on this engine"): a six-month, one-ticker check on the equity store receives 256.8 million tuples and holds 11.6 GB, 121.6 s of which 1.9 s decide. Two things found with it belong to the same change: the stream has no upper bound either (a run reads to the store's last row whatever its `end`, and holds what follows its window: R8L cut at 2014-06 was killed at 22.3 GB), and `abt run` trims nothing before its window opens (`Fold::barrier` returns before the trim while the label is before `start`). `StreamSpec.from` and `whole` are on the wire since protocol 1.3 (E-155) and read by nothing |
| c | A bound on what one binding costs the data service | ERRATA E-120. Unfinished work on it is in the amendments tree's stash | One query maps 15 to 21 GB. The machine lives with this today, so it does not block a first redeploy. It blocks re-enabling the full equity store and the move to a smaller machine |

The fourth precondition is package 0's step 6: a store whose open does not hold its log in memory.

**A plan's threads (Nov 26).** `abt plan run --kernels 1` bounds the kernels and not the work: the executor runs `threads` steps at a time on them, 4 by default (`abt-query/src/plan/exec.rs`), each holding its run's history until change (a) is in. A real agent's NQ grid peaks at 15.3 GB that way and at 4.1 GB with `--threads 1` (2:44 where it took about 1:45). The pipeline's tool passes `--kernels 1` alone, so its plans need `--threads 1` beside it on a minute store, or the default should follow `--kernels`: the owner's call.

**When.** It does not have to wait for 25 s. A newer build first does something for the killed runs once package 0 is merged with changes (a) and (b). Package 2a then removes the case of a daily program on a minute base. Deploying package 0 alone would change nothing the team would notice.

## Not in this roadmap

Recorded so that their absence is a decision and not an oversight. All are open rows in `status.md`.

- The kernel's exact numerics (it evaluates `exact` as `two_pass`).
- The store's sparse tables and wavelet matrices behind its query forms; the staging stages S0 and S6; the packed tail (ERRATA E-93); the shedding order.
- Storage at 2 bytes a value (it is at 4.5 to 11).
- Every live target, and a broker adapter.
- The gates that need the M4.

## What is measured and what is projected

| Statement | Basis |
| --- | --- |
| 367 s, 3.7 GB, correlation 0.9777 | Measured, one run on one machine, Nov 6 |
| 25 s for Python, 366 s for the old engine | Measured, ten repetitions each, Nov 4 to 5 (issue #79) |
| The per-phase seconds | One run's own clock |
| "Up to 140 s is the data path, 216 s the evaluator and executor" | This handoff's grouping of that clock. `docs/assessment.md` reads the same run as evaluator-bound |
| "Package 2 leaves about 230 s" | Arithmetic on the grouping |
| "`listed` and `member` are 28.9M of the rows" | Counts taken in iteration 1, not on the Nov 6 run |
| "6 to 10 s on the M4" | The design's projection, written before any of it was built |
| "14 to 24 s on the benchmark machine" | That projection times the 2.4 per-core ratio of issue #79 |

Package 1 exists to replace the last six rows with measurements.
