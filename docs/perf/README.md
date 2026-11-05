# MW14 profiling, Nov 5 2026

The data behind the MW14 performance issue. Every figure here has its numbers in a
CSV beside it, and every cell can be re-run with `run_prof.sh`.

## Machines

| | |
| --- | --- |
| benchmark (the engine comparison, 10 repetitions) | `m8g.xlarge`, Graviton4, 4 vCPU, 16 GB |
| profile (perf, rungs, eras, ablations) | `m8g.large`, Graviton4, 2 vCPU, 8 GB, cloned from the first |

Both are Amazon Linux 2023, rustc 1.95.0, abt `713ed1a`, `--window-sums exact`. The two
boxes agree to ~5% on the same work (`bundle build` 49.5 s against 49.0 s; the full run
366.3 s against 344.6–349.3 s), but absolute seconds from the two are never mixed in one
number: the comparison table is the first box, the profile and the shares are the second.

## Files

| file | what it is |
| --- | --- |
| `mw14-engine-stages.png` | wall and CPU per stage, both engines (medians of 10 reps) |
| `mw14-rep-spread.png`, `mw14-rep-ecdf.png` | the 10 repetitions, per stage and cold-to-answer |
| `mw14-relation-calls.png`, `relations.csv` | derivations per relation, from the run's own coverage table |
| `mw14-universe-scaling.png`, `scaling.csv` | 250 / 500 / 1,000 / 2,000 / 4,240 names, full window |
| `mw14-era-cost.png`, `eras.csv` | cumulative `--end` sweep, differenced into eras |
| `mw14-relation-ablation.png`, `ablation.csv` | one relation stubbed at a time, full window |
| `mw14-flamegraph.svg` | `perf record -e cpu-clock -F 499 -g --call-graph fp`, full run |
| `perf_self.csv` | self time per symbol from the same profile |
| `profiling-cells.tsv` | every cell's raw `--timing` line, as measured |
| `run_prof.sh`, `prof_prep.py`, `prof_summarize.py`, `mk_figures.py` | the suite, the subset/ablation generator, the summariser, the figures |

## What is a result and what is not

The only cell that is a *result* is `parity`: the untouched DSL over the whole window,
reproducing the frozen canon (1,852 weeks, max absolute difference 7.85e-15).

**The rung and ablation cells produce wrong returns by construction** — a subset of the
universe moves every cross-sectional rank, and a stubbed relation changes the book. They
are performance cells. They are also not additive: stubbing `gate` or `rank` makes the
run *slower* because the book it then runs is a different book.

The profiled binary is not the timed one: it is built with
`RUSTFLAGS="-C force-frame-pointers=yes -C debuginfo=2"` into a separate target
directory, because `[profile.release]` carries `debug = "line-tables-only"` and no frame
pointers, so neither `fp` nor `dwarf` unwinding works on the shipped binary. The cost of
that instrumentation is small: 349.6 s under `perf` against 344.6 s without.
