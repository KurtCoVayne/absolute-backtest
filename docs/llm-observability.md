# LLM observability study

Oct 9, 2026

The usability study of Oct 6 (`docs/llm-usability.md`) found that Haiku
wrote four company-book programs that checked clean and decided nothing, and
that in each case one wrong construct left the decision rule empty while the
checker stayed silent. This study asks whether a small model, given the
means to see what its program derives, stops shipping such programs, and
whether it uses those means unprompted beyond being told they exist.

## Design

Twelve Haiku agents, one per task and condition, each in a sandbox holding
the language description (the README excerpt and `semantic-model.md`), the
corpus environments and libraries, a release `abt` behind a wrapper that logs
every invocation, the task, and the synthetic data the task names. The agents
were given the prompt of the Oct 6 study, with its rules (work only in the
sandbox; save every attempt; at most about 30 invocations) and, in the
observability condition, one more paragraph.

| Condition | Toolchain | Docs | Prompt |
| --- | --- | --- | --- |
| `base` | `abt` built from `0d9bc2b`, the tree after the parity work | README excerpt, `semantic-model.md` | `experiments/llm-observability/prompt-base.md`: the Oct 6 prompt |
| `obs` | `abt` of this tree (`src/observe.rs`; `docs/observability.md`) | the same plus `observability.md` | `prompt-obs.md`: the same, naming the four commands, and "a clean check with zero decisions is not done" |

Six tasks (`experiments/llm-observability/tasks/`): the two company books on
synthetic data in their environments' layouts (`scripts/synth/weekly_synth.py`,
60 names over 520 weeks; `sessions_synth.py`, 6 roots over 500 sessions), on
which the corpus programs `corpus/company/mw14.dsl` and `r8l.dsl` run and
trade, and four briefs on the seeded synthetic market (`momentum_liquid_monthly`,
`intraday_open_gap`, `breakout_with_stop`, `vol_targeted_trend`).

Scored per agent (`score.py`): attempts to the first clean check; whether the
final program checks clean; the decisions its run makes under the task's run
conventions; for the two books, the decisions shared with the corpus
program's run on the same data (the same bar, instrument and constructor);
and the inspection commands used, counted from the wrapper's log. The data is
synthetic, so "shared decisions" measures agreement with the reference
program, not with the company's canon.

What the study cannot show. Twelve runs of a sampled model are a small
sample; a difference of one or two tasks between the conditions is within
its noise, and the synthetic markets are easier than the real bundles (no
halts, no missing bars, a membership that is one label). The result to
read is the kind of failure, not the count.

## Results

(filled in from `experiments/llm-observability/runs/*/score/` once the
agents finish)
