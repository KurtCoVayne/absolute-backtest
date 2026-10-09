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

Every one of the twelve programs checks clean and trades, and the four
company-book programs make exactly the decisions of the corpus books on the
same data: the same bars, instruments, sides and magnitudes, 1,057 of 1,057
for MW14 and 243 of 243 for R8L, in both conditions, with programs that
share no relation name with the corpus books. The scores are
`experiments/llm-observability/runs/<name>/score/`.

| Task | Condition | Attempts | Clean at | Decisions | Shared with the reference | abt calls | check / run / show / query / explain / ledger |
| --- | --- | ---: | ---: | ---: | ---: | ---: | --- |
| MW14 | base | 2 | 2 | 1,057 | 1,057 of 1,057 | 17 | 3 / 7 / 0 / 0 / 6 / 0 |
| MW14 | obs | 4 (1 strategy, 3 audit) | 1 | 1,057 | 1,057 of 1,057 | 22 | 5 / 7 / 1 / 8 / 0 / 4 |
| R8L | base | 6 (3 strategy, 3 probes) | 2 | 243 | 243 of 243 | 17 | 7 / 8 / 0 / 0 / 1 / 0 |
| R8L | obs | 2 | 1 | 243 | 243 of 243 | 15 | 2 / 5 / 1 / 6 / 0 / 1 |
| breakout_with_stop | base | 3 | 1 | 28 | — | 12 | 4 / 7 / 0 / 0 / 0 / 0 |
| breakout_with_stop | obs | 3 | 2 | 28 | — | 22 | 4 / 10 / 2 / 5 / 0 / 3 |
| intraday_open_gap | base | 4 | 1 | 64 | — | 17 | 5 / 8 / 0 / 0 / 3 / 0 |
| intraday_open_gap | obs | 1 | 1 | 64 | — | 18 | 2 / 8 / 1 / 6 / 0 / 3 |
| momentum_liquid_monthly | base | 4 | 1 | 362 | — | 14 | 4 / 9 / 0 / 0 / 0 / 0 |
| momentum_liquid_monthly | obs | 3 | 1 | 362 | — | 18 | 3 / 7 / 1 / 6 / 0 / 1 |
| vol_targeted_trend | base | 5 | 3 | 801 | — | 16 | 5 / 10 / 0 / 0 / 0 / 0 |
| vol_targeted_trend | obs | 3 | 2 | 801 | — | 19 | 4 / 9 / 1 / 4 / 0 / 2 |

**The zero-trade failure did not recur, in either condition.** This is not
the Oct 6 result and it is not attributable to the tools: the model is the
harness's current Haiku (the environment lists Haiku 5.5; the Oct 6 study
ran Haiku 4.5), the markets are synthetic and clean (no halts, no missing
bars, one membership label), and the task texts state the minute at which
each print is keyed, which the Oct 6 review had identified as the fatal gap.
The controlled comparison is within this study, base against obs, and on
the outcome measures it is flat: the same decision counts, the same
reference agreement, one to three attempts to a clean check either way.
What differs is what each agent could establish about its program, and
what it found.

**What the agents did with the tools.** Every agent in the obs condition
used all three new commands unprompted beyond the one paragraph: `show`
once or twice, `query --explain` four to eight times, `--ledger` one to
four times, and read the coverage report of every run. The findings they
record, quoted from their notes:

- MW14. The coverage report flagged `crowded   374 calls   0 with tuples   0 tuples   EMPTY: demanded, never derived`. The agent audited the crowding rule from ledgers and established that the book's 25 % test is never met on this data (the maximum is 20 %, two of ten latched names on 2018-10-05), then ran a sensitivity at 10 % to show the branch reaches trades. From the same ledgers it found that the data's total-return index "doubles at every split while the split-adjusted close stays continuous" and quantified it: $290,768, 23 % of the additive P&L, from four held names crossing splits. That is a defect of `scripts/synth/weekly_synth.py` as it stood when the data was generated (fixed afterwards; the study's data is kept as run), and the corpus book inherits it equally, which is why the decisions still agree. The base agent, with identical decisions, wrote "nothing was checked against the raw data".
- R8L. `query --rel ctx --explain` showed the as-of reads binding the previous session (`P = 2023-06-20, K1 = K2 = K3 = K4 = 2023-06-20`) and never the decision day, and reproduced a decision by hand from the bindings (m, 2 m_med, |g|, latefrac, size). A near miss found by its own recomputation was explained as `literal 11 L <= (1 / late_den) has no solution`, the intended reason. The base agent wrote that `explain` "prints only fired ... with N solution(s), with no bindings, so arithmetic could not be checked that way" and verified through the dump instead.
- breakout_with_stop. The coverage line `highest 12000 calls 9600 with tuples` with a first entry forty weeks into the history showed a 52-week high computed over a partial window; the agent added a full-history guard, after which `highest 8880 calls 8880 with tuples` and the decisions fell from 30 to 28. The base agent found the same partial window from the date of its first signal.
- momentum_liquid_monthly. A ledger recompute matched the top-ten selection in all 33 months and showed that the membership literal was inert on this data (681 of 681 pairs pass it); `query --rel adv` showed the brief's $1M screen about 1,700 times below the data's dollar volumes, and a $2B sensitivity cut the liquid pairs from 666 to 24. A deliberate wrong-label probe (`"NDX"`) produced `never fired: decide derived no tuple over 1000 bars` with the rule line `cand#1 ... literal 2 member(A, T, "NDX") has no solution`, the diagnosis the Oct 6 review asked for. The base agent met the same label problem with the real label (`"SP500"` gave zero decisions silently) and found `"SPX"` by trial with `--param`.
- vol_targeted_trend. `query --rel decide --explain` showed the cap branch dormant at a 10 % target; a stress run at 20 % halted on leverage with a weight of 1.21, exposing that `least(1, W)` did not cap. The base agent found the same defect through the same halt and guessed its cause correctly ("the bare integer 1 is typed Count and compared wrongly with a Scalar").
- intraday_open_gap. Coverage confirmed 23 gap sessions and no empty relation; `--fills` with the ledger confirmed the 09:32 buys and 16:00 sells; `query --explain` named `R > gap` for a 0.96 % gap and `prev(T, T0)` for the first session.

**What the tools did not do.** They did not shorten the path to a clean
check, which the checker already governs, and they did not change any
agent's final rules except the breakout guard. Their effect is on the
second question of the Oct 6 review, whether a program that checks clean
does what its author believes: in the obs condition every agent could
state, with bindings, why each branch of its decision fired or not, and two
of them found things about the data (the split artefact, the inert
membership) that the base agents, with the same decisions, could not see.

**Defects the agents surfaced, and their status.**

| Finding | Reported by | Status |
| --- | --- | --- |
| `least(1, W)` passes the checker and does not cap (Count against Num by variant order) | vol_targeted_trend, both conditions | fixed (`e5bed9b`), `tests/functions.rs` |
| `query --symbol` on `decide` prints zero tuples while the rule lines show it fired | MW14, R8L, momentum, open gap (obs) | fixed (`e5bed9b`) |
| `--ledger` refuses a relation whose only input is its entity | MW14, R8L, breakout, open gap, vol (obs) | fixed (`73c5c69`): written per symbol |
| `--ledger` cannot be combined with `--dump` | R8L, breakout, open gap (obs) | fixed: the kept kernel copies the data when something reads it after the run |
| a label the data never carries (`"SP500"` for an index labelled `SPX`) decides nothing, silently | momentum, both conditions | fixed: refused before the run, naming the labels the data holds |
| the run warns "commissions and fees are zero" while charging a per-notional or per-contract commission | MW14, R8L, both conditions | fixed (`73c5c69`): the warning is dropped when commissions were charged; `--frictionless` is described as what it is |
| the never-fired path summary reads as a contradiction when the failing literal is a bound read (a label none of the tuples carries) | momentum (obs) | fixed (`73c5c69`): reworded |
| `explain` reports no bindings | base condition | the obs binary reports the first solution's bindings |
| `rows`, `least`, `greatest`, `abs`, `median` absent from the README's one page; no function list; no `rank` example; order terms on delta constructors undocumented | all | fixed (README, the one-page syntax) |
| the semantic model's table gives `position(+A, ...)`, the checker `-A`; WF-6 omits `rows` | MW14, open gap, R8L, breakout | fixed (semantic model, sections 5 and 6) |
| `asof` carries an older value silently; a daily read from a minute rule needs a key-equality test | R8L, both conditions | by design; `prev(S, S1)` on the session calendar in the second formulation (`language-v2.md` 3.5) |
| no per-entity previous bar; no product aggregate; Count cannot multiply a Scalar | MW14, R8L, both conditions | `prev_row`, `cumprod` and `decile` in the second formulation (7.1, 7.3) |
| fixed-base accounting sizes weights against starting capital, so "equal weight" and "capped at one" need `--compounding on` | momentum, vol, breakout, both conditions | documented (README, sizing and accounting); the run's accounting line says which figures compound; the `execution` block's `capital ... fixed | compounding` makes the choice visible in the second formulation (4.3) |
| `moo` and `moo_moc` drop every order on an environment with no open series, and the checker is silent | open gap, both conditions | fixed: a run with such an order and no open (or high and low, for `limit` and `stop`) companion is refused before it starts, naming the relation expected |
| the per-bar annualisation of a minute strategy (98,280 periods) | open gap (base) | the `report` block (4.5); `--report-by day` today |
| impact at minute resolution uses a 20-bar ADV | open gap, both conditions | fixed: at a sub-daily resolution the ADV is the mean over the previous `--adv-window` days of each day's summed volume |

The data defect of the weekly generator is fixed in `scripts/synth/weekly_synth.py`;
`experiments/llm-observability/runs/` holds what the agents saw and wrote.
