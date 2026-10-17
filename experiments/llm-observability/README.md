# LLM observability study (2026-10-09)

Haiku agents write six strategies from the abt docs alone, in two
conditions: `base`, the toolchain and docs as they were after the MW14 and
R8L parity work (commit `0d9bc2b`), and `obs`, the same with the
observability commands of `docs/observability.md` (`abt show`, the coverage
report of every run, `abt query --explain`, `--ledger`). The question is
whether a small model, given the means to see what its program derives,
stops shipping programs that check clean and decide nothing (the result of
the 2026-10-06 study, `docs/llm-usability.md`), and whether it uses them.

The data is synthetic (`scripts/synth/weekly_synth.py` and
`sessions_synth.py` for the two company books; the seeded synthetic market
for the briefs), so the reference for MW14 and R8L is the corpus program run
on the same data, not the company's canon. Materials: `prompt-base.md`,
`prompt-obs.md`, `tasks/`, `make_sandboxes.sh`, `score.py`; every agent's
attempts, notes, command log and scores are under `runs/<task>-haiku-<cond>/`.
The write-up is `docs/llm-observability.md`.

**A record; its commands no longer exist (M-9, `docs/spec/05-migration.md`).**
The study ran on the first formulation and the v1 kernel:
- `make_sandboxes.sh` copies `corpus/env` and `corpus/lib`. `corpus/` was
  deleted when the crate moved to the second formulation (5c04572).
- The prompts check and run with `bin/abt check env lib FILE` and `bin/abt run
  ... env lib FILE`. `abt query --rel R --at BAR --explain` is now the query
  language's `?`, `trace` and `why not` (`abt query --text QUERY`; ERRATA
  E-53). The `--dump` the agents verified against is gone: a run writes its
  outputs to its store (E-79).
- `docs/observability.md`, which the `obs` condition read, is folded into
  the README's section on the kernel's observation commands (K-7; E-80).

The scripts run at the study's own commit, c6c1deb. A rerun on the second
formulation is to be recorded beside these results (A-7.2).
