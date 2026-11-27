# absolute-backtest: the three-layer engine

This tree is the newest code of `absolute-backtest` (`abt`), a backtesting engine in Rust. Pull request #80 (`perf/m8-parallel`) was merged into `spec/three-layer-split` on Nov 25; each later package is a branch off it with a pull request of its own.

**Read `docs/handoff/README.md` before anything else.** It gives the state of the project on Nov 24, 2026, the goal and the first steps. The rest of `docs/handoff/` holds the approach, the ledger of what is and is not built, the roadmap and the benchmark recipe.

## What the engine is

Three programs that share only a protocol:

- `abt-data`: the store, the queries, the event stream and the vendor adapters. It interprets nothing.
- `abt-kernel`: the strategy language, its checker and planner, the evaluator, the market simulator and the book.
- `abt-query`: plans (studies), metrics, statistical tests, claims and trial counting.

`abt` links all three in one process. The shared crates are `abt-wire` (messages), `abt-agg` (one implementation of every aggregate) and `abt-codec` (the hot store's block format). The specification is `docs/spec/00-architecture.md` to `07-storage.md`. `docs/spec/TRACE.md` maps every decision to its code and test, and `docs/spec/ERRATA.md` records every deviation.

## Standing rules

"The owner" below is the person who owns this project and runs the sessions.

- **No heavy compute on the Mac.** Workspace builds, the full suite, ingests and book runs go on the benchmark machine, `m8-bench2` (AWS profile `signos`, region `us-east-1`), driven through SSM Run Command, not SSH. It has the `m8-bench` role since Nov 25 and is stopped between sessions: starting it needs the owner's go-ahead, and it is stopped again when the session ends. Nothing is pushed to try a change there: an uncommitted change goes over as a patch through SSM. A single targeted test binary on the Mac is fine.
- **Full history.** A book run or a benchmark uses the whole window and the whole universe. If that is too slow or too large, the engine gets fixed, not the run.
- **`/Users/alejandro/absolute-backtest` is the owner's checkout.** Read it; change nothing in it.
- **No commit, push, merge, or edit of a pull request or issue unless the owner asks.** The roadmap lists commits and a merge as steps; that is a list of work, not permission.
- **Do not `git stash`.** This tree is a worktree, and the stash list it shares with the other engine trees already holds unfinished work. The `SmallVec` experiment of Nov 6 was measured and kept on Nov 25 (package 1, `docs/assessment.md`): `Tuple` is a `SmallVec` in the tree and the patch file is gone.
- **Other AWS profiles** (`default`, `production`, `john-root`) are other accounts: never touch them. In `signos`, ask before stopping `hermes-prod` or `absolute-pipeline`.
- **A deviation from the spec gets an erratum.** Numbers E-140 to E-159 are held by open branches (stream blocks: E-141 to E-143, E-144 to E-149, E-150 to E-154, E-155 to E-159; E-140 is free); the next free number after them is E-160. Update `docs/spec/TRACE.md` in the same change.

## Cadence

From `docs/iterations/01-foundation.md`, "Cadence rules", set by the owner. A "gate" is the three commands CI runs, listed under Commands.

1. Cheap checks first, in this order: the targeted test binaries of the crates touched; the rehearsal script with release binaries; the full suite; the socket-transport pass. The rehearsal script of October was a scratch file and is not in the repository; the benchmark of `docs/handoff/roadmap.md` package 1 takes its place.
2. Batch fixes into one gated commit. The full suite runs once per batch, not once per fix.
3. Reproduce in process before touching a deployment store.
4. Rehearsals run on a fresh copy of the base store.
5. Changes to `abt-wire`, `abt-codec` and `abt-agg` go first in a batch, since they rebuild every test binary.
6. After a full gate, delete `target/debug/incremental`. Start no build under 5 GB free.
7. Docs change with the code they describe, in the same commit.
8. The final gate is one full run over the final tree plus one socket run.
9. One fix-and-gate cycle per finding. What survives it is written up as an open item.

## Commands

```
cargo fmt --check && cargo clippy --all-targets -- -D warnings && cargo test   # what CI runs
cargo test -p abt-tests --test eval_set     # the evaluators held equal to each other
cargo test -p abt-tests --test retention    # a trimmed history equals the whole history
ABT_TRANSPORT=socket cargo test             # the same suite over sockets
scripts/bench-mw14.sh LAKE SPXTR VIX CANON [N]   # the full MW14, N cold runs against the canon, one JSON line each (on the machine)
```

The full MW14 run is `scripts/bench-mw14.sh`; `docs/handoff/benchmark.md` is the record of the Nov 6 run it reproduces, with the machine's layout.
