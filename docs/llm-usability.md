# Can an LLM write abt? MW14 and R8L from the docs alone

This study asks how complete and how learnable the language is. Haiku 4.5 and
Sonnet 5.5 agents wrote the two company books, MW14 and R8L, and two briefs.
Each agent had the language description, a plain-language specification, the
checker and the real data, and nothing else. Their final files were scored by
the parity scripts that prove the reference implementations exact
(`docs/parity-mw14.md`, `docs/parity-r8l.md`).

Everything the agents wrote is kept in `experiments/llm-usability/`, as written,
along with the scores and the reviewer's root-cause analysis
(`review/REVIEW.md`).

## Result in one paragraph

Sonnet writes abt well from the docs alone. All four of its company-book files
trade and land within rounding distance of the books:
- R8L checked clean on the first attempt and matched 21,891 of the book's
  21,892 legs.
- Both MW14 files equal the canon on 89% of weeks. The difference is one
  missing token (`- tol` in the new-name band test); with it, all 1,852 weeks
  are exact.

Haiku does not manage it yet. All four of its company-book files end up
checking clean, and **none of them makes a single trade**. Haiku either never
ran its file or explained the zero away ("zero trades is a valid outcome"). In
each case one wrong construct leaves the decision rule empty while the checker
stays silent. Giving four worked examples helped neither model.

## Setup

| | |
| --- | --- |
| Models | Haiku 4.5, Sonnet 5.5 (fresh agents, not forks: they had no access to this repository's history) |
| Tasks | MW14 (`tasks/mw14.md`), R8L (`tasks/r8l.md`), briefs `momentum_liquid_monthly`, `intraday_open_gap` |
| Conditions | `docs`: the language description only; `ex`: plus four corpus strategies that solve other problems (`momentum_12_1`, `pairs_trading`, `opening_gap`, `volatility_targeting`). The briefs ran `docs` only |
| Runs | 2 models × 2 books × 2 conditions + 2 models × 2 briefs = 12, one try each |
| Feedback | `abt check` and `abt run` on the real bundles (`b_mw14`, `b_r8l_fb`), or on the synthetic market for the briefs; never the reference numbers |
| Budget | about 30 `abt` invocations; every version saved as `attempts/N.dsl` before it was checked or run |

**What each sandbox held** (`experiments/llm-usability/make_sandboxes.sh`
rebuilds them):
- `docs/README.md`: the README's "surface syntax in one page" and "reading
  the checker" sections, plus the executor-features paragraph.
- `docs/semantic-model.md`.
- `env/` and `lib/`: copies of `corpus/env` and `corpus/lib`.
- `bin/abt`: a release build.
- `TASK.md`.

**The tasks** are written in finance terms. They give every rule, window and
threshold, describe the data in words, and state the run conventions. They use
no DSL constructs. The agent's prompt is in `prompt.md`.

**Isolation.** All 12 transcripts were audited: no agent read any file outside
its sandbox. The bundles carry no strategy source, and the binary contains no
strategy text.

**Scorer.** The scorer was checked first: on the reference `mw14.dsl` and
`r8l.dsl` it reproduces exact parity (1,852 of 1,852 weeks; 21,892 of 21,892
legs).

## Results

| run | attempts | first clean check | against the book |
| --- | ---: | ---: | --- |
| MW14 · Sonnet · docs | 3 | 3 | 89.0% of weeks equal to 1e-9, weekly correlation 0.9989, CAGR 20.68% (book 20.90%), the same max drawdown |
| MW14 · Sonnet · ex | 5 | 3 | the same as docs, to the cent |
| MW14 · Haiku · docs | 14 | 14 | 0 decisions |
| MW14 · Haiku · ex | 9 | 4 | 0 decisions |
| R8L · Sonnet · docs | 1 | 1 | 21,891 of 21,892 legs + 12 extra; every matched leg's P&L and direction equal; Sharpe 1.2770 (book 1.2777); 99.81% of days equal |
| R8L · Sonnet · ex | 2 | 2 | 21,724 matched, 168 missing, 12 extra; Sharpe 1.2547 |
| R8L · Haiku · docs | 9 | 9 | 0 trades |
| R8L · Haiku · ex | 6 | 6 | 0 trades |
| momentum brief · Sonnet | 4 | 1 | its first valid attempt makes no decision (an undocumented index label); its final trades |
| momentum brief · Haiku | 3 | 3 | trades |
| open-gap brief · Sonnet | 3 | 1 | trades |
| open-gap brief · Haiku | 4 | 3 | trades (a sell decided every minute while held: 7,372 decisions for 38 fills) |

Each run's `score/` directory holds the per-attempt diagnostics (`attempts.tsv`),
the check of the final file, its run with the reference flags and the
comparison. The agents' own `run.sh` flags reproduce the reference-flag
results in every case where the file trades.

### MW14

**Sonnet** implemented every rule faithfully:
- the split-adjusted close, the row counter and the latch through the stock's
  own previous row (`prev(T, Tp), R(A, _, X) asof Tp`), an idiom it inferred
  from the `asof` section;
- ordinal and average ranks grouped by the bar;
- the 52-row gate, the crowding dial, the weights and the band.

The with-examples run built the split product as a sum of logs, because there
is no product aggregate, and wrote the latch as a five-rule 0/1 state machine,
because there is no conditional expression. Both runs give the same numbers.

The one difference from the book is the new-name test, `W >= band` where the
book has `W >= band - 1e-12`. With the dial at 0.7 over 28 names,
`0.7/28 = 0.024999999999999998` falls just under 2.5%, so a name the book buys
on 1991-08-02 is bought a week later, and the paths drift from there (203 of
1,852 weeks differ). With that token patched
(`review/mw14-sonnet-docs-fix/final.dsl`), all 1,852 weeks equal the canon,
with correlation 1.000000 and P&L $7,534,876.54. The task stated the tolerance
only in the sentence about held names, so part of this is the specification's
fault.

**Haiku**:
- *docs*: the split-factor recursion takes its base case at the first bar of
  the whole dataset (`has_prev(T) :- bar(T), prev(T, _)`). That bar is
  1988-01-08, which holds market series and no stocks, so the split-adjusted
  close, the score and everything downstream are empty. It spent nine
  attempts on the same mode error (`'+A' of 'held_eligible' is an input and
  must be bound before the call`), never found the fix (declare `-A`), and
  hard-coded the counts instead.
- *ex*: the liquidity rank binds the asset before ranking, so each group has
  one tuple, every rank is 1, and the decile test never holds. W4 fired on
  every attempt from the third on, but its text speaks of `top`, not `rank`,
  and Haiku ignored it. It also replaced the Clenow regression with an
  invented proxy, believing it could not build a row index.

### R8L

**Sonnet**:
- *docs*: clean on the first attempt. Every rule matches, with every matched
  leg's P&L and direction equal.
  - The 13 differing legs (12 extra, 1 missing) all have a latefrac of exactly
    1/3. Sonnet wrote the test without division (`3 (C30 - C20) <= C30 - O0`),
    which is exact.
  - The book divides in floating point and lands on either side of 1/3. On ZT
    2009-08-06 the exact difference is 0.0.
  - The spec as written agrees with Sonnet.
- *ex*: the same, except that a division guard (`abs(C30 - O0) > 0`) sits in
  the signal both legs share, so 167 sidecar legs with m = 0 exactly are lost.
  The agent flagged the risk in its notes as "rare". Partial arithmetic halts
  a run, which pushes authors to guard early and too broadly.

**Haiku**: both runs read the minute prints at the decision minute (10:00),
but the minute-0 open is labelled at 09:31, so the join never matches. Both
also used windows that can never fill:
- *docs*: ATR over `window(T, 20d, min 20)`; 20 calendar days hold about 14
  sessions.
- *ex*: ATR over `window(T, 0d, min 20)`.

Haiku-docs also moved its daily features into a library of its own, believing
a strategy rule cannot be daily. That belief came from six attempts at the
resolution error, whose message names only `resample`.

### Briefs

Both models wrote valid strategies. Sonnet's momentum brief exposed a silent
failure: `member(A, T, "SP500")` gives zero decisions and no warning (the
synthetic label is `SPX`), and the agent found the label by trial.

## Haiku and Sonnet

| | Sonnet 5.5 | Haiku 4.5 |
| --- | --- | --- |
| company books that trade | 4 of 4 | 0 of 4 |
| closeness | within rounding of the books (1 token from exact on MW14) | none |
| attempts to a clean check (books) | 1–3 | 4–14 |
| parse errors (all runs) | 0 | 9 (invented `;`, `or`/`and`, `not (...)`, `-T`, if/else) |
| mode errors | fixed in 1–2 attempts | 9 attempts without a fix (mw14-docs) |
| runs its result and inspects it | yes | rarely, and reports success regardless |

The examples changed nothing measurable:
- Sonnet's MW14 is the same with and without them.
- Sonnet's R8L with examples is the worse of its two R8L runs.
- Haiku fails the same way in both conditions.

The docs, the env files and the checker carried the successful runs.

## Where the language and its tools fall short

Ranked by frequency × impact (detail and quotes in `review/REVIEW.md`):

1. **Modes `+`/`-`.** 8 of 12 runs hit `error [M] ... '+A' of 'X' is an input
   and must be bound before the call` when counting or ranking a derived
   relation. The message never says that declaring `-A` in the relation's
   signature is the fix.
2. **Silent emptiness.** A strategy that checks clean can run with zero
   decisions:
   - all four Haiku failures;
   - a wrong label;
   - `--data` pointed at a bundle directory, which loads empty relations and
     carries on.

   `run` does not say which rule never fired. `explain` can say it, but it
   takes no `--bundle`.
3. **The previous own row.** `prev` steps the global calendar. The working
   idiom is `prev` plus an `asof` read of the stock's own relation, which is
   documented nowhere as such. A constant inside an `asof` pattern silently
   reads the latest row *with that value*. That cost the Sonnet-ex run more
   than 20 minutes of runtime before it rewrote the rule.
4. **Windows.** Calendar `window` was used where row counts were meant
   (`20d`, `52w`, `0d`). `rows` has one table row in the docs. There is no row
   number and no product aggregate.
5. **Resolution.** The X message says two resolutions meet "only through
   resample". It mentions neither `asof` nor the per-relation `@1d`
   annotation.
6. **Types and units.** `ATR > 0` needs `0 USD/share`, and Count × Scalar is
   rejected. Haiku removed zero guards rather than typing them.
7. **No disjunction or conditional.** Haiku tried `;`, `or`, and `not (...)`.
   Sonnet wrote sign-split rules and a five-rule latch.
8. **Executor defaults and flags.** Margin halts, a 5% margin rate, a sell
   fee and 252 periods a year were not documented for a book with no margin
   model. The transaction-cost-neglect warning prints while `--commission-bps`
   commissions are charged.
9. **Environment vocabulary.** The `member` labels and the minute at which
   each `*_m` print is labelled appear only in comments, or not at all.

## Fixes, in order of the evidence

1. **Explain zero decisions in `abt run`.** When a decide rule produces
   nothing, print the dependency chain down to the first relation with no
   tuples and the literal that never had a solution. Let `explain` take
   `--bundle` and choose a sample time.
2. **Make the M diagnostic name the fix**: "...or declare `A` as `-` in
   `rel X`". Add a README paragraph on `-A` (relations you count, rank or
   enumerate) against `+A` (features you call with the asset bound).
3. **Check label literals** against the bundle's vocabulary at run start, and
   list the vocabularies in the env files.
4. **Static window sanity**: `window(T, 0d, min K>1)` is an error; a warning
   when `min K` exceeds what an `Nd` window can hold, suggesting
   `rows(T, K, min K)`.
5. **Word W4 for `rank`** ("every group has one tuple; every K is 1; bind
   only T") and consider making it an error there.
6. **Make the X diagnostic mention `asof` and the `@1d` annotation.**
7. **Document the own-row idioms** with a worked example: the previous row, a
   row counter, a log-sum product and a 0/1 latch. Warn on a constant inside
   an `asof` pattern. Consider `row_number` and `prod` builtins.
8. **Parse errors that teach** ("no `or`/`;`: one rule per alternative").
   Consider `if(c, a, b)` and `sign()`.
9. **Partial arithmetic**: document "guard only in the rule that divides", or
   add a safe-divide form.
10. **CLI**:
    - make `--data` on a bundle an error;
    - add a no-costs/no-margin preset;
    - count `--commission-bps` in the cost warning;
    - document that `moo` fills at the open relation and that target mode
      holds when no decision is made.

One fix has been applied: `scripts/parity/r8l_compare.py` read a leg's
direction from the sign of its amount. A delta-mode `short` records a positive
amount, so the script reported 41% direction agreement for strategies that
place delta orders. It now signs by the order's constructor, and the stored
comparisons were re-scored (100% agreement).

## Caveats

- **One try per cell**, so variance is unmeasured.
- **R8L's environment helps.** The `futures_sessions` environment already holds
  R8L's minute prints and decision clock, so R8L is easier than an
  implementation from raw minute bars.
- **Some residuals come from the specifications, not the language.** The
  MW14 tolerance's placement and R8L's floating-point latefrac are examples.
- **Run times.** The agents' MW14 files run in 4 to 6 minutes, against
  2.3 minutes for the reference, because of how their joins are structured;
  the results are unaffected.

## Reproduce

```
STUDY=/tmp/lang-study experiments/llm-usability/make_sandboxes.sh
# give each agent prompt.md with SANDBOX set to its directory (one Haiku and one
# Sonnet agent per sandbox name); then, for each sandbox:
STUDY=/tmp/lang-study PY=~/.venvs/abt/bin/python experiments/llm-usability/score.sh /tmp/lang-study/r8l-sonnet-docs
abt briefs report --briefs B --attempts A corpus/   # the briefs, as in briefs/README.md
```

`score.sh` needs the bundles and reference files listed in
`docs/parity-inputs.md`.

## Files

```
experiments/llm-usability/
  prompt.md               the agent prompt (SANDBOX = the agent's directory)
  tasks/                  the four task specifications
  make_sandboxes.sh       rebuilds the twelve sandboxes
  score.sh                checks every attempt, runs final.dsl with the reference flags, compares
  runs/<task>-<model>-<docs|ex>/
    attempts/N.dsl        every version, as written
    final.dsl, run.sh, NOTES.md   the agent's final file, its run command, its own account
    lib/                  a library the agent wrote (r8l-haiku-docs)
    score/                attempts.tsv, final_check.txt, run.txt, compare.txt
  runs/briefs-*.txt       abt briefs report per model
  review/REVIEW.md        per-run root causes, stumbling blocks, fixes
  review/mw14-sonnet-docs-fix/   the one-token patch that makes Sonnet's MW14 exact
```
