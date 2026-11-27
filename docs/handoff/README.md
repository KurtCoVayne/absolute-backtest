# Handoff: the three-layer engine

Nov 24, 2026. Written for the session that continues this project. No engine code has changed since Nov 6.

## Since this was written

The documents in this directory describe Nov 24 and are left as they were. This section is the log of what changed after, with dates in UTC as the machine's logs have them. Where a line here contradicts a section below, this line is the current state.

| Date | What changed | Where to read it |
| --- | --- | --- |
| Nov 25 | **The benchmark machine is reachable.** Role and instance profile `m8-bench` (the SSM core policy only), driven through SSM Run Command. Its disk was as left on Nov 6: the clone, the lake, `/data/mw14-store`, the canon. `rustfmt` and `clippy` were added to its toolchain. It is stopped between sessions | `benchmark.md`, "The machine" |
| Nov 25 | **The `SmallVec` experiment is out of the tree**, kept as `smallvec-nov6.patch`. The tree has no uncommitted experiment | `roadmap.md`, package 1 |
| Nov 25 | **Package 0's fixes.** The lint. The test regression, reproduced on Linux (3 runs of 3 at `eb3345c`) and fixed by removing the slot-map cache (E-130). The as-of memo keyed by the whole bound pattern (E-132). The log replay brought over from the amendments tree (E-128, E-129). The batch paths of Nov 6 written up (E-131). The first whole suite since `9a255d9` found one more failure, in `outputs`: wall-clock seconds in a run's stored report (E-133), fixed | `docs/spec/ERRATA.md`, E-128 to E-133 |
| Nov 25 | **The whole suite on the machine at that tree:** 859 passed, 0 failed, in process and over sockets; `fmt` and `clippy` clean | |
| Nov 25 | **Package 1's tooling:** `scripts/bench-mw14.sh` (stages the store, N cold runs, one JSON line each), `scripts/parity/mw14_series_bundles.py`, `mw14_compare.py --json`, `[profile.profiling]` with `scripts/build-profiling.sh`. The run's clock gains the rows per relation, the wait for the stream and the decode (`rows`, `phases wait= decode=` under `ABT_TIMING_RELS=1`), and `tuples=` is now the count of a served run | `benchmark.md`; `docs/assessment.md` for the results |
| Nov 25 | **CI's toolchain is not the machine's.** CI installs the newest stable (`dtolnay/rust-toolchain@stable`): 1.100.0 since mid November. The machine builds and benchmarks with the 1.99.1 it was launched with. Clippy 1.100 rejects six loops that 1.99 accepts (`needless_range_loop`), so the first push of the batch failed in CI though the machine's gate was green; the loops are rewritten. The machine's gate now runs CI's clippy too (`rustup toolchain install 1.100.0`, a target directory of its own); its tests and the benchmark stay on 1.99.1. Nothing pins the toolchain, so CI can turn red when a new stable ships | `.github/workflows/ci.yml` |
| Nov 25 | **Pull request #80 is merged** into `spec/three-layer-split` (`3be3ba4`): CI green on its head `1b3bf73`, the socket pass green on the machine. Each later package is a branch off it with a pull request of its own, and none of those is merged without the owner | |
| Nov 25 | **Package 1's record.** Five repetitions at the merged head: 379.4 s mean (373.5 to 383.3), the same weekly returns in all five, correlation 0.977595. The Nov 6 commit's binary on the same store: 372 s and 0.9777, so Nov 6 is reproduced and the head is 2% slower with package 0's fixes. Of the head's 379 s, 100 s pass before the first tuple: `bind` 55 s, the preconditions 37 s, the pushed aggregate 7.5 s. The front waits 0.9 s on the stream in the whole run, so the question of Nov 24 (is the 75 s a wait on the data service) is answered: no. The allocator is a third of the front's CPU | `docs/assessment.md`, "Package 1 (Nov 25)" |
| Nov 25 | **The `SmallVec` experiment is kept.** Measured: 302.3 s against 379.4 s (four runs), the same returns to the last bit; the whole suite passes with it, 860 tests in process and over sockets. `Tuple` is a `SmallVec` in the tree, `crates/abt-tests` compiles against it, and `smallvec-nov6.patch` is removed. Where a section below says the experiment is out of the tree, or expects it to be dropped, this row is the current state | `docs/assessment.md` |
| Nov 25 | **An allocator trial, not in the tree.** mimalloc as the `abt` binary's allocator: 315.6 s alone, 271.5 s with the `SmallVec`, the same returns; the front's peak goes from 3.5 GB to between 3.8 and 4.4 GB. A new dependency with C source: the owner's decision. The whole change is `mimalloc = { version = "0.1", default-features = false }` in the workspace's dependencies, `mimalloc.workspace = true` in `crates/abt/Cargo.toml`, and a `#[global_allocator] static ALLOCATOR: mimalloc::MiMalloc` in `crates/abt/src/main.rs` | `docs/assessment.md` |
| Nov 25 | **Two facts about the machines.** The benchmark machine's clone fetched only `perf/m8-parallel`; it now fetches every branch (`remote.origin.fetch`). On macOS, `tv_golden` differs from its golden files in the last digit of three slippage values (the platform's `log`) and local clippy 1.95 rejects five expressions CI's 1.100 accepts (`state/provenance.rs`, `state/tables.rs`, `check/link.rs`): neither is a failure of the tree, and both pass on Linux | |
| Nov 25 | **Package 2, first part.** `bind` 56.6 s to 3.5 s, the preconditions 39.7 s to 6.0 s, the run 331.6 s to 235.7 s on the same evening, the same returns; the front's peak 3.51 GB to 1.01 GB, under package 3's 1.5 GB gate. The benchmark machine varies by about 10% between evenings and nights: compare runs made together | `docs/assessment.md`, "Package 2, first part"; E-138, E-139 |
| Nov 25 | **Package 3, the row path's quick wins** (S01 to S03 of the evaluator's plan), on top of package 2's first part: 235.7 s to 185.0 s, the same returns, the front's peak 0.97 GB. The listing leaves the stream when nothing reads it (E-141); an observation of a projected relation writes an empty ledger, open (E-142) | `docs/assessment.md`, "Package 3, the row path's quick wins" |
| Nov 25 | **Package 5, parity through the adapter, first part:** the weekly correlation 0.9776 to 0.99974, 1,825 of 1,851 weeks equal to 1e-4 (E-144 to E-149). The benchmark now runs from 1990 on a store staged with the adapter's new options (`scripts/bench-mw14.sh`); that run costs 336 s, the membership test 75 s of it. The split-day dividend (D7) and the weekly against daily compounding of dividends are open, and are the owner's call | `docs/assessment.md`, "Package 5"; `docs/parity-mw14.md` |
| Nov 25 | **Package 5, second part.** MW14's membership test reads the name's last bar of the week (E-160): 76.5 s to about 3 s, the parity run 337.2 s to 251.6 s, the same returns. The residual against the canon is measured: five mechanisms (the weekly against the daily compounding of dividends, D7, two dividends in a week, a delisting's week, the re-buy after it) reproduce the run in all 1,851 weeks to 8e-15 | `docs/assessment.md`, "Package 5, second part" |
| Nov 26 | **Package 3, the column-block evaluator's first operators** (S05, S06, S07, S09, S10; E-150, E-151), behind `--evaluator block`: the parity run 249 s to 180.2 s, the same returns; `clenow`, `ma` planned; `eligible` back on the row path since E-160 | `docs/assessment.md`, "the column-block evaluator's first operators" |
| Nov 26 | **Package 2, second part: delivered weekly answers** (W1, D1, K1 to K3; E-155 to E-159, E-175), on the column-block evaluator: the parity run 180.2 s to 134 s, the same returns; `dvol`, `traded@weekly`, `close@weekly` computed by the data service | `docs/assessment.md`, "Package 2, second part" |
| Nov 26 | **Package 3, the column-block evaluator's remaining operators** (E-152 to E-154, E-170, E-171): the parity run 134 s to 81.4 s with `--evaluator block`, the same returns; S13 and S14 were not reviewed adversarially (the subagents' weekly limit), only mutation-tested | `docs/assessment.md`, "the column-block evaluator's remaining operators" |
| Nov 26 | **Package 5, third part: 1,851 of 1,851 weeks equal to the canon to 8e-15** (E-161 to E-164): every action of a bar's bucket on a coarser calendar, the dividend compounded at its ex-date (`prod`, `dividend_reinvested`), a delisting's week, the split-day dividend as the Norgate adapter's decision. On the column-block evaluator's remaining operators the run costs 90.8 s (81.4 s before: `dividend_at` reads every key of the bucket, which the calendar window's operator did not) | `docs/parity-mw14.md`; `docs/assessment.md`, "Package 5, third part" |
| Nov 26 | **The calendar window read for the whole frame** (E-165): 90.8 s to 78.7 s, the same returns, 1,851 of 1,851; `dividend_at` 15.4 s to 3.1 s. The window cache's three counters leave the stored stats row. **A connection over a socket ends with its writer** (E-166): under `ABT_TRANSPORT=socket` a test binary kept every data service it started, and `eval_set` reached the kernel's limit of mappings (`Cannot allocate memory` at 4.3 GB of 16); found by the socket pass of the gate, which the remaining operators' tests pushed over the limit | `docs/assessment.md`, "Package 5, third part" |
| Nov 26 | **A panel of every program**, so that a change made for one book is measured on all of them: the owner's instruction of Nov 26. Its first record (`r8l` on a synthetic minute futures market, the 24 corpus strategies on the S&P 500 lake from 1990 and on a synthetic minute market) is in `docs/assessment.md`; since the rule of the same day (below) `scripts/panel.sh` runs on real history only | `docs/assessment.md`, "The panel" |
| Nov 26 | **R8L read at its clock's bars** (E-167): on 26 synthetic roots and 3,000 days of minute bars, 393.8 s and 5.79 GB to 32.9 s and 0.60 GB with the same 4,618 decisions; a barrier costs 0.1 ms on average and a decision barrier 1.7 ms (p99 2.2, max 29), which `--timing` prints. An observed relation is read at a barrier of any calendar and, under a session-offset clock, at each instrument's decision bar, so R8L's ledgers hold one row a root a session. **The work before the stream runs together** (E-168): the data service computes an aggregate under its read lock, the kernel asks its pushed requests on four threads and checks the preconditions beside them | `docs/assessment.md`, "R8L read at its clock's bars" |
| Nov 26 | **Real history only, and the data brought up to date.** The owner's rule: a benchmark or a validation run reads real market history. The synthetic cell, the panel's synthetic stores and the cloned lake (`scripts/widen-lake.py`) are retired. Norgate: the lake of Oct 27 from the bucket (bars to 2026-10-26); MW14's two series carried to its last day by `scripts/mw14-series.py`. Databento: the futures pulled again to the session of 2026-11-25, and R8L's 26 roots staged from them by `scripts/r8l-store.sh`. **A log segment is compressed** (E-169, the Arrow IPC format's zstd): the real 6J's log 1,350 MB to 374 MB with the same versions, which is what let the R8L store fit the pipeline machine's disk. On the fresh lake MW14 no longer equals the canon (correlation 0.9983): the June lake lacked the bars of 77 S&P 500 members of the early 1990s, so the canon needs freezing again on a newer lake | `docs/assessment.md`, "Real history only"; `docs/handoff/benchmark.md`, "The freshest data" |
| Nov 26 | **The hot store after a restart, and the service's memory in a run.** Three faults of the append path met on the real stores: a seal record's instrument is read by its id (E-137, resolved), the presences of earlier generations are removed (E-172), a staging folds the tails so that an open replays nothing (E-173, `abt data consolidate RELATION --tails`). The equity service on the pipeline machine: 55 s and 6.2 GB to serve, then under 0.5 s and 1.03 GB. The aggregate pass reads a batch of instruments at a time (E-174): MW14's data service 8.2 GB to 4.7 GB, the run 65.0 s, the same returns. A plan's `run` step says where a step ends when two are written on one line | `docs/assessment.md`, "The hot store after a restart", "The data service's memory in a run"; `docs/handoff/benchmark.md` |
| Nov 26 | **R8L's store from Databento, and R8L does not run on it.** 26 roots, 96.3 million minute bars a relation to the session of 2026-11-25, 19 GB on the pipeline machine (`scripts/r8l-store.sh`). Its sessions are the electronic day; the book's are each root's primary session, so `r8l.dsl` on it is another rule: it halted in 2014 with its base lost, after 470 s with the kernel at 15.2 GB. Done the same day: the adapter takes each root's session window and the roots are staged again (two rows down) | `docs/handoff/benchmark.md`, "R8L's store"; `docs/assessment.md`, "R8L's store, and R8L on it" |
| Nov 26 | **The pipeline's agents on this engine**: the pipeline's runner and its real quant agent, in its side-run mode, on this tree's binaries and the two stores above. One post of three ran to a full backtest (NQ); the other two failed on things the deployed engine has too: the catalog's commission charged before the program's (E-71, E-96's open call) and a check run that holds the store's whole history in the kernel (package 7's changes (a) and (b), measured: 121.6 s and 11.6 GB for a six-month, one-ticker check, 9.2 s and 0.09 GB once the program scopes its instruments) | `docs/assessment.md`, "The pipeline's agents on this engine" |
| Nov 26 | **MW14 on the real Russell 3000 in 164.8 s**, inside the owner's bound of 3 minutes for a universe of that size: the lake's own Russell lists, 12,373 assets from 1990 to 2026-10-26, the book's rules unchanged, at `3c10fc5` with `--evaluator block`; the run's process at 2.57 GB and the data service at 8.54 GB | `docs/handoff/benchmark.md`, "A wider universe"; `docs/assessment.md`, "MW14 on the real Russell 3000" |
| Nov 26 | **R8L on its real history, on each root's primary session** (branch `data/p6-r8l-sessions`). The Databento adapter takes a root's session windows (E-176); the 26 roots staged with them are 3.0 GB where the whole-day store is 19 GB. **An engine fault found on it and corrected (E-177)**: the data layer's pass cut a root's session bars at the other roots' closes, so ES's range beside the treasuries was its last hour's; the kernel now reads the pass's answer only where it is the instrument's whole session. R8L to the session of 2026-11-25, on the repaired pulls (next row): 441.7 s, 16,348 legs, 8.1 GB, a decision barrier 5.0 ms (max 19.7 ms). Beside the Python book on TradeStation's bars over 2011-05 to 2026-08: 94.8% of the book's legs, 99.92% in the same direction, annual return 21.22% against 22.35%, Sharpe 1.321 against 1.365. `sessions of "ES"` names the root's series by its ticker (E-178), so the run's own report has its periods on a vendor store: Sharpe 1.301 on ES's 4,233 sessions. Open: the pass needs a list of labels per instrument to answer session bars for roots with different hours | `docs/handoff/benchmark.md`, "R8L's store"; `docs/assessment.md`, "R8L on each root's primary session"; ERRATA E-176 to E-178 |
| Nov 26 | **The session days a pull cut, repaired.** The pipeline's month-chunked pull keeps only the first piece of a session day that spans two chunks: in the pull of Nov 26, Sep 1 and Oct 1, 2026 were two hours long in 116 files of 59 roots, and the R8L store had no primary session on them for 16 of its 26 roots. Found from R8L's report (E-178), which came up short of its fills on days with no ES session. `scripts/r8l-repair.py` asked the vendor again through the pulls' cost gate; `scripts/r8l-views.py` takes the fuller file of a day; R8L is run again on the store staged from them. Feb 28, 2020 was cut in the history too and is repaired as far as the vendor has it. Open: the pipeline's own pulls (`dbn-extra`, `dbn-live`) carry the same cut; the store on the pipeline machine's disk is the one from before the repair (4 GB free there) | `docs/handoff/benchmark.md`, "The days a pull cut"; `docs/handoff/roadmap.md`, packages 6 and 7 |
| Nov 26 | **The agents again, and a plan's threads.** Two more posts through the pipeline's runner: neither reached a full backtest. The NQ post's grid was killed at the slot's 14 GB: `abt plan run --kernels 1` still runs 4 steps at a time, each holding the store's history (15.3 GB; 4.1 GB with `--threads 1`). The pipeline's tool passes `--kernels 1` alone. A third post, on the new build with one step at a time, stopped earlier, at its translation check: the store states the book's commission and the program its own (E-96's open call). A fourth, on a store with no commission stated, ran its check plan and stopped where the agent read `ledger fill`, the bar a program first sees a fill, as the fill's time: the deployed engine prints the same. **A fifth ran the NQ post to a full backtest and submitted it** (986 s, 16 trials, the final build with `--threads 1` on its `plan run`, the NQ store staged from the repaired pulls); the equity post was killed for memory again, which is package 7 (a) and (b) | `docs/assessment.md`, "The agents again, and a plan's threads" |

Errata E-138 and E-139 are package 2's first part (below); E-141 to E-178 are written on the branches above (E-140 and E-143 are unused), and the next free number is E-179. `status.md` section 6 cites TRACE rows by their line on Nov 24; six rows (E-128 to E-133) were inserted at lines 161 to 166 on Nov 25, so a line from 161 on is now six further down. `roadmap.md` names the rows instead.

**Open items found on Nov 25** (a review of the batch by reading; none reproduced by a test yet):

- E-134: a trimmed run restarts the chains of a name absent for longer than the horizon. It can move MW14 through the adapter, so package 5 takes it.
- E-135: a resumed run, or a run with a late tuple, under trimming reads nothing at earlier bars. With package 7's change (a).
- E-133's open part: the stats row of a run's report follows the thread count and the history kept, which are not in the run id.
- E-136: after a restart the hot store's replay keeps the earliest row of a label where the append kept the latest (two rows of one instrument at one new label in one batch). Not reproduced. E-137, found with it, was met on real data on Nov 26 and is fixed.
- `abt_agg::exact::Wide::shl` asserts nothing: its overflow check runs over the limbs `LIMBS - words..LIMBS` and passes whenever `i + words >= LIMBS`, which holds for every one of them. A wide shift that overflows is not caught.
- Three mutation patches no longer apply to the tree and so falsify nothing: `e-109-ladder-index`, `e-119-drift`, `st-5.2-ladder-receipt` (`st-8.1-snapshot` was a fourth and is regenerated).

## The state in one screen

`absolute-backtest` was split into three programs (data, kernel, query) between Oct 12 and Oct 20. The split works. It did not make the engine fast, which was its purpose. That is the work that remains.

| | |
| --- | --- |
| Newest code | this tree, `/Users/alejandro/absolute-backtest-m8`, branch `perf/m8-parallel` at `2040b40` (Nov 6). The commit is pushed; the 16 uncommitted files are only here |
| Pull request | #80 into `spec/three-layer-split`: open, CI failing for two real reasons (below) |
| Last green commit | `3e0d002`, the first push of #80: 857 tests passed. The head is six code commits later, and every one of them fails CI |
| Uncommitted here | 16 files, an unbuilt experiment (`Tuple` as a `SmallVec`) |
| Size | 8 crates, about 99,700 lines of Rust, 859 tests |
| Spec coverage | 208 trace rows: 157 done, 40 partial, 11 missing |
| MW14 full replay | 367 s and 3.7 GB, against Python's 25 s and a 60 s target |
| MW14 against the canon | not equal on that same run: weekly correlation 0.9777 |
| R8L full replay | never run on the three-layer engine |
| In production | an Oct 20 build answers the team's backtests on the pipeline machine; nothing later is deployed |

## Words used here

| Word | Meaning |
| --- | --- |
| The owner | The person who owns this project and runs the sessions. Their instructions are the only authority for commits, merges and machine changes |
| Book | A strategy the firm trades. **MW14** is the production book: weekly momentum on the S&P 1500. **R8L** is the intraday futures book. Inside the kernel, "the book" is also a run's positions and cash |
| The canon | MW14's reference result from the firm's own Python code: 1,852 weekly returns |
| Old engine | The single-crate engine before the split. It reproduces both books exactly |
| M-0 to M-10 | The build steps of the split (`docs/spec/05-migration.md`). **M-8** is the performance step |
| Hot store | The data layer's memory-mapped store for bar data (`docs/spec/07-storage.md`) |
| The ladder | The hot store's precomputed coarser bars: 5m, 15m, 30m, 1h, 1d, 1w, 1mo levels of every bar relation |
| The front | The `abt` process that runs the kernel, as opposed to the data service it reads from |
| The amendments tree | `/Users/alejandro/absolute-backtest-amend`, the tree the pipeline's fixes were made in |
| The pipeline | `/Users/alejandro/absolute-pipeline`: the system that takes the team's requests from chat and runs them on this engine. Its AWS machine has the same name |
| Trace, errata | `docs/spec/TRACE.md` (every spec decision with its code, test and status) and `docs/spec/ERRATA.md` (every deviation, numbered `E-n`) |

## The goal

The owner's words on Nov 6, which still stand: "You need to achieve, first a run that finishes, but also the performance of Python, else you havent made a progress".

The run finishes since Nov 6. The performance of Python is not reached.

As a number: MW14 replayed in full (1991 to 2026, 4,240 names) from the store the Norgate adapter stages, equal to the canon, in 25 s on the benchmark machine. The specification's own target, 60 s and 1.5 GB (A-7.3 in `docs/spec/00-architecture.md`), is the gate on the way.

## Why it is slow

The three measurements are the same book on the same machine type (`m8g.xlarge`, 4 vCPU, 16 GB). The target and the projection are the specification's, stated for an Apple M4, which issue #79 puts at about 2.4 times faster per core.

| | Time | Memory | Source |
| --- | --- | --- | --- |
| Python book, cold to answer | 25 s | 9.4 GB peak | issue #79 |
| Old engine (`713ed1a`) | 366 s | 3.5 GB | issue #79 |
| Three-layer engine (PR #80) | 367 s | 3.7 GB, plus 3.8 GB in the data service | `docs/assessment.md`, "At M-8 (Nov 6)" |
| Target A-7.3 | 60 s | 1.5 GB | `docs/spec/00-architecture.md` §7.3 |
| Design projection | 6 to 10 s | under 1 GB | `docs/spec/06-performance.md` §5.1 |

The new engine matches the old one and is 15 times slower than Python. Two things explain it, both visible in the Nov 6 run's own clock (`status.md` section 3 has the table and the arithmetic):

1. **The evaluator still works one row at a time.** Every cell is a 16-byte tagged `Value`, every rule instance gets its own environment, every derived atom costs a hash lookup with a heap-allocated key. The design that removes this, the column-block evaluator of `docs/spec/06-performance.md` §3.1 and §3.2, was never started. About 216 s of the 367 s is the evaluator and the executor.
2. **The kernel receives every daily row and buckets it into weeks itself.** The store builds the ladder on every ingest, and nothing reads it. Up to about 140 s of the 367 s is here.

The split between the two is this handoff's reading of one run's clock. `docs/assessment.md` reads the same run's CPU profile as evaluator-bound and expects little time from the data path. `status.md` section 3 sets the two readings side by side, and the roadmap's package 1 measures which is right before either is acted on.

Parallel evaluation across entities was built and proven equal to the single thread. It gains nothing yet, because it covers little of the evaluator: it cuts a rule's work across threads only after the rule's first literal, and only for rules that do not read the cross-section. On a synthetic 1,000-name market, 16.3 s of a 93.8 s run was inside the steps it cuts.

## Why CI is red

Two failures on the head of #80. Both are in committed code, not in the uncommitted experiment, and neither is a toolchain or infrastructure problem (CI uses the stable Rust toolchain; the repository pins none).

1. **A lint, which is what fails today.** `clippy::unnecessary_get_then_check` at `crates/abt-kernel/src/kernel/eval.rs:610` in this working tree (line 605 at the committed head). `self.memo_stuck.get(&memo_key).is_none()` becomes `!self.memo_stuck.contains_key(&memo_key)`. It came in with commit `eb3345c`.
2. **A test regression, hidden because the lint step runs first.** `quantile_conventions_and_the_aggregate_halt` in `crates/abt-tests/tests/kernel.rs` has failed since commit `9a255d9`. The suspected cause is a cache keyed by a memory address that gets reused, so it has to be reproduced on Linux. `status.md` section 5 has the reasoning and a fix.

The test step stops at the first failing binary. So since `9a255d9` the 42 test binaries that come after `kernel` in alphabetical order have never run in CI, and the changes made after it have never been through the whole suite.

## What to do

`roadmap.md` has each package in full. The order and the gates:

| # | Package | Done when |
| --- | --- | --- |
| 0 | One green tree | CI is green on the head of #80 and #80 is merged |
| 1 | A benchmark that runs itself | A script in the repository reproduces 367 s and 0.9777 |
| 2 | Stop streaming what the kernel only buckets | The run receives on the order of 2.4M tuples, with the same results |
| 3 | The column-block evaluator | With package 2: 60 s and 1.5 GB |
| 4 | Parallel on top | Wall time at least halves from one thread to four; 25 s |
| 5 | Parity through the adapter | 1,852 of 1,852 weeks equal the canon |
| 6 | R8L on the full minute history | 3 s and 0.6 GB, 21,892 legs |
| 7 | The pipeline | A newer build is deployed and the failed requests rerun |

Package 5 is the one that closes the 0.9777 mismatch. It runs beside packages 2 and 3 and must be closed before the target is claimed. Its causes are not yet separated (`status.md` section 4).

**Package 7 needs more than a merge.** The change in #80 that bounds a run's memory is switched on by `abt run` only. The pipeline's backtests are `abt plan run`, which does not get it. Deploying #80 as it stands would not stop the pipeline's runs from being killed for memory. The roadmap lists this and two other engine changes under package 7; none of them is in a speed package.

**Package 0, step by step.**

1. Make the benchmark machine reachable. It is stopped, has no SSM role, and its SSH key no longer exists. `benchmark.md`, "The machine", has the commands. They create an IAM role and start an instance, so they need the owner's go-ahead.
2. Set the `SmallVec` experiment aside so the tree matches the head. It is saved as `smallvec-nov6.patch` beside this file; the roadmap has the two commands. Do not use `git stash`: the stash list is shared with the other trees. The experiment is kept or dropped by measurement in package 1.
3. Fix the lint, which is then at line 605.
4. Reproduce the test regression on the machine and fix it: `cargo test -p abt-tests --test kernel quantile_conventions_and_the_aggregate_halt`.
5. Run the whole suite on the machine, in process and over sockets.
6. Bring the log-replay fix over from the amendments tree. It is an uncommitted fix there, in `crates/abt-data/src/store.rs` and `crates/abt-data/src/store_hot.rs`, that stops a store from holding its whole log in memory when it opens. Its errata need new numbers.
7. Write the errata that are missing.
8. Correct the contradictions in the documents (`status.md` section 10).
9. Merge #80.

The steps include commits, a push and a merge. The standing rule is unchanged: each is done when the owner asks for it in that session. The roadmap lists them as work to do, not as permission. A change reaches the machine by a push to a work branch, which the machine fetches from GitHub.

## Reproducing 367 s

The short form. The scripts as they ran are in `benchmark.md`.

- **Where.** On `m8-bench2`. Its disk should still hold the clone, the lake, the staged store at `/data/mw14-store` and the canon, as left on Nov 6. That has not been re-checked, because the machine is stopped.
- **What it needs if staged afresh.** The lake, two series files and the canon, all on the Mac (paths below). They reached the machine by `scp` on Nov 6, which is no longer the way; `benchmark.md` says what to do instead.
- **The run.**

```
abt data serve --store /data/mw14-store --socket S.sock &
ABT_TIMING_RELS=1 abt run --strategy mw14 --data S.sock --timing --history window \
  --start 1991-01-04 --end 2026-07-02 \
  corpus-v2/catalog corpus-v2/lib corpus-v2/company/mw14.dsl
python3.12 scripts/parity/mw14_compare.py /data/mw14-store RUN /data/canon.npy
```

`RUN` is the id the run prints. The 367 s and the 0.9777 are the same run: the lake staged through the Norgate adapter.

The recovered script in `benchmark.md` begins by deleting the staged store and rebuilding it. On the kept disk, run only its last part, as that document says.

## Where everything is

### Engine trees on the Mac

The four are worktrees of one repository. They share branches and one stash list, so a stash made in one is visible in all.

| Path | Branch | What it is |
| --- | --- | --- |
| `/Users/alejandro/absolute-backtest-m8` | `perf/m8-parallel` (`2040b40`) | This tree. The newest code, PR #80, 16 uncommitted files |
| `/Users/alejandro/absolute-backtest-amend` | `amend/pipeline-mvp` (`cc3edc8`) | The amendments tree. What the pipeline machine was built from. Holds the uncommitted log-replay fix and a stash (`status.md` section 9) |
| `/Users/alejandro/absolute-backtest-hours` | `amend/hour-calendars` (`7e0269e`) | Clean. Its work is merged (PR #78) |
| `/Users/alejandro/absolute-backtest` | `perf/mw14-profile` (`45e162b`) | The owner's checkout: the old engine plus the MW14 profile data in `docs/perf/`. Read only |

### Branches on GitHub (`KurtCoVayne/absolute-backtest`)

| Branch | State |
| --- | --- |
| `spec/three-layer-split` (`cc3edc8`, Oct 20) | The integration branch. PRs #74 to #78 merged into it; #80 targets it |
| `perf/m8-parallel` (`2040b40`, Nov 6) | PR #80 |
| `main` (`8c623ba`, Oct 19) | A stale copy of the integration branch, pushed once, never a PR target. It lacks #76, #77, #78 and #80 |
| `claude/strategy-dsl-semantic-model-nfibpr` (`713ed1a`, Oct 12) | Still the repository's default branch. It is the old engine |
| `perf/mw14-profile` (`45e162b`, Nov 4) | The old engine plus `docs/perf/` |

Open issues: #79 (the MW14 profile and its ranked fixes), #73 (Norgate spin-off distributions booked a session early), #72 (the specification), #3.

### Machines (AWS profile `signos`, `us-east-1`)

| Name | Instance | Use | State on Nov 24 |
| --- | --- | --- | --- |
| `m8-bench2` | `i-01c143488531686bc`, `m8g.xlarge` | Builds, the full suite and the benchmark. Every Nov 6 number was measured here | Stopped since Nov 6, 60 GB disk kept. Not reachable as it stands (`benchmark.md`) |
| `absolute-pipeline` | `i-054398816027e565c`, `r7i.2xlarge` | The team's pipeline, on an Oct 20 engine build. Not for engine development | Running. Disk 98% full (6.2 GB free) |
| `hermes-prod` | `i-00a927cc207b393af`, `t3.medium` | The chat agent that sends requests to the pipeline | Running |

### Data and benchmark inputs on the Mac

| What | Path |
| --- | --- |
| The Norgate lake (5.6 GB, June 25 snapshot) | `/Users/alejandro/market-data/NDLake` |
| The canon | `/Users/alejandro/stageanalyst/strategy/ivmom/artifacts/_canon_wkret_mw14_full.npy` |
| The book's own series inputs | `spxtr_daily.parquet` and `vix_weekly.parquet` in the same `artifacts/` directory |
| The Python book | `/Users/alejandro/stageanalyst/strategy/ivmom/book.py` |
| The Python-against-abt benchmark driver and its results | `mk_engine_bench.py` beside the book; `artifacts/engine_bench_mw14.json` and `.xlsx` |
| The profiling scripts and data of issue #79 | `/Users/alejandro/absolute-backtest/docs/perf/` |

`docs/parity-mw14.md` names `~/data/ref/mw14/` for these inputs. That directory does not exist on this Mac; the paths above are where the files are.

### The pipeline's side

`/Users/alejandro/absolute-pipeline/docs/iterations/03-engine-handoff.md`: how a chat request reaches `abt`, how often the engine answers, and what it must do before a newer build is deployed.

## The documents here

| File | Read it for |
| --- | --- |
| `approach.md` | The design as it was built: the three layers, the protocol, one run traced through the code, how correctness is held, and what the performance design says against what exists |
| `status.md` | The evidence: timeline, what was achieved, every measurement with its source, the 51 open trace rows, open errata, the 23 engine-side problems the pipeline has met, loose ends per tree, contradictions in the docs |
| `roadmap.md` | The work packages in full, each with its reason, the code it touches, its gate and its risks |
| `benchmark.md` | The Nov 6 benchmark as it was run: the machine and how to reach it again, the staging, the run, the profile |
| `smallvec-nov6.patch` | The 16 uncommitted files of this tree as a patch, taken Nov 24, so the experiment survives the tree being cleaned |

The standing rules are in `CLAUDE.md` at the root of this tree.
