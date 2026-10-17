# LLM usability study (2026-10-06)

Haiku 4.5 and Sonnet 5.5 agents wrote MW14, R8L and two briefs from the abt
docs alone. The write-up, results and fixes are in `docs/llm-usability.md`;
this directory holds the materials: the prompt, the tasks, every attempt as
written, the scores, and the reviewer's root-cause analysis
(`review/REVIEW.md`). The `.dsl` files here are agent output, kept verbatim,
not corpus cases.

**A record; its commands no longer exist (M-9, `docs/spec/05-migration.md`).**
The study ran on the first formulation:
- `make_sandboxes.sh` copies `corpus/env`, `corpus/lib` and
  `corpus/strategies`. `corpus/` was deleted when the crate moved to the
  second formulation (5c04572).
- The prompt checks with `bin/abt check env lib FILE` and runs with `bin/abt
  run --strategy NAME ... env lib FILE` over a bundle or a directory of
  Parquet files. A program is now checked against `corpus-v2/catalog` and
  `corpus-v2/lib`, and a run reads a store (`abt data ingest`, then `abt run
  --data STORE`; `docs/spec/ERRATA.md`, E-78).
- The agents' programs are in the first formulation's syntax, which the
  present checker does not read.

The scripts run at the study's own commit, 740138d. A rerun on the second
formulation, on the same tasks with the same models, is to be recorded
beside these results (A-7.2).
