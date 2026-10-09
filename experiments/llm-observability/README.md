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
