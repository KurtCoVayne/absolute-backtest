# abt LLM-usability study: review

12 sandboxes: mw14 and r8l × {Haiku 4.5, Sonnet 5.5} × {docs, docs+examples}, plus 2 briefs × 2 models.
The reviewer's experiments ran on copies outside the repo (sandboxes untouched); of them, only the patched MW14 file is kept here, in `mw14-sonnet-docs-fix/`. For `explain` I flattened
bundle partitions into `review/data_mw14*` / `review/data_r8l`, because `abt explain` has no `--bundle` option.

Outcome summary

| run | check | decisions | parity | root cause of the gap (one line) |
| --- | --- | --- | --- | --- |
| mw14-haiku-docs | clean (W4 ×2, W5 ×21) | 0 | none | split-factor recursion's base case keyed to the global first bar, so the relation is empty |
| mw14-haiku-ex | clean (W4 ×2, W5) | 0 | none | liquidity `rank` grouped per asset (W4) gives rank 1 for every name, so `liquid` never holds |
| mw14-sonnet-docs | clean, 0 warnings | 4,827 | 89.0% weeks exact, corr 0.9989 | new-name band test `W >= band` has no 1e-12 tolerance |
| mw14-sonnet-ex | clean, 0 warnings | 4,827 | the same numbers as sonnet-docs | the same missing tolerance |
| r8l-haiku-docs | clean (W5) | 0 | none | same-timestamp joins of minute prints keyed at different minutes; ATR window `20d, min 20` |
| r8l-haiku-ex | clean (W5) | 0 | none | same-timestamp join; ATR window `window(T, 0d, min 20)` |
| r8l-sonnet-docs | clean on 1st attempt | 21,903 | 21,891/21,892 legs, +12, Sharpe 1.2770 vs 1.2777 | latefrac == 1/3 exactly: ties decided by float rounding |
| r8l-sonnet-ex | clean on 2nd attempt | 21,736 | 21,724 matched, 168 missing, +12 | `abs(D) > 0` guard drops 167 sidecars with m = 0, plus the same 1/3 ties |

---

## 1. Per-run findings

### mw14-haiku-docs: 0 decisions (never run by the agent)

The decisions die at the root of the price pipeline:

```
11  rel has_prev(@T: Timestamp)
12  has_prev(T) :- bar(T), prev(T, _).
16  cum_split(A, T, 1) :- universe(A, T), not has_prev(T), not split(A, T, _).
17  cum_split(A, T, F) :- universe(A, T), not has_prev(T), split(A, T, F).
18  cum_split(A, T, C) :- universe(A, T), has_prev(T),
19      prev(T, T0), universe(A, T0), cum_split(A, T0, C0), split(A, T, F), C = C0 * F.
```

- **The base case is per bar, not per asset.** `has_prev(T)` holds at every timestamp except the first one in the
  whole time domain. The bundle's time domain begins on 1988-01-08 (the `series` partitions start in 1988), and no
  stock is in `universe` on that date. So `cum_split` has no base tuple, and `close_adj`, `score`, `ranked`,
  `entered`, `latched` and `decide` are all empty.
  - Checked with `explain` on data that includes 1988: `cum_split#1 did not fire at 1990-05-04: literal 2 'not has_prev(T)' has no solution`.
  - Even with a 1990 start, any name that IPOs later would never get a base tuple.
  - The recursive step `universe(A, T0)` at the global `prev` breaks the chain at the first gap.
- **Latent second bug: a constant regressor.**
  ```
  36  B = ols_beta(L, I) over (T1 in rows(T, 18, min 18), close_adj(A, T1, P), L = log(P / (1 USD/share)), I = 1),
  ```
  - With data starting in 1990 (so the base case fires), the run halts as documented: `partial arithmetic in rule mw14::score#1 ... ols_beta against a constant regressor`.
  - On the real bundle the halt is never reached, because `close_adj` is empty upstream. The halt-on-degenerate guarantee depends on demand, so it gave no signal here.
  - Even with both bugs fixed (copy `review/mw14-haiku-docs/fix_row.dsl`), the run halts on a margin call: hard-coded `N = 50` and ranks that are all 1 put about 7x gross on the book.
- **Other deviations:**
  - `ranked`/`liq_rank` declare `+A`, so W4 fires and every rank is 1.
  - `liquid` is `Rank <= 60`, a stand-in for the decile rule.
  - The crowding dial is dropped, the 2.5% band is dropped, and there are no exit decisions.
  - The gate uses `window(T, 52w, min 52)` (calendar) instead of rows.
- **Why it happened.** Attempts 5 to 13 were nine rounds of the same `error [M] ... '+A' of 'held_eligible' is an input and must be bound before the call`.
  - The agent never found the fix: declare `-A` in the relation's own signature.
  - Instead it hard-coded counts (`W0 = Scale / 50`) and gave up on per-date counting. Its NOTES conclude: "The mode system doesn't support counting unbounded entities across relations."
- **Could a checker catch it?**
  - Not the anchoring as such. But a runtime "relation X derived 0 tuples over the run; first empty dependency: `cum_split`" report would have pointed straight at it.
  - The M diagnostic could also name the fix: "or declare `A` as `-` in `rel held_eligible`".

### mw14-haiku-ex: 0 decisions (final never run; a "debug" attempt 9 was run)

```
63  rel liquid_rank(-A: Equity, @T: Timestamp, -LiqRank: Scalar)
64  liquid_rank(A, T, LiqRank) :- eligible(A, T),
65      rank(dvol(A, T, D), by (D asc, A asc), as LiqRank).
71  liquid(A, T) :- liquid_rank(A, T, LR), n_eligible(T, N),
72      LR / (N / 10) > 4.
```

- **Root cause.** `eligible(A, T)` binds `A` before the `rank`, so every group holds one tuple and `LR = 1`.
  - `1 / (N/10) > 4` needs N < 2.5, so `liquid`, then `enters`, then `latched` are always empty.
  - The checker did warn: `warning [W4] mw14 at 65:7 in rule mw14::liquid_rank#1: 'top' over 'dvol' keeps every tuple ...`. The message says `top` about a `rank`, and it appeared in every attempt from 3 to 9. The agent ignored it.
  - Confirmed in copy `review/mw14-haiku-ex/fix_rank.dsl`: moving the rank to `bar(T), rank(edv(A, T, D) ...)` makes decisions appear.
  - That copy then runs into a second bug: `N = count(A) over (latched(A, T), eligible(A, T))` at line 139, where `A` is outer-bound, gives N = 1, every weight hits 0.15, and the run halts on a margin call.
- **Other deviations:**
  - The Clenow score was replaced by an invented proxy, `(exp(52·mean) - 1)·(1 + mean/(|std| + 0.001))`. The agent could not build a row index: "rows() can only be used inside aggregations, and nesting aggregations is not permitted".
  - `ranked` has the same per-asset grouping, so every rank is 1.
  - The liquidity rank uses raw `dvol`, not the 4-row mean.
  - The latch is `prev(T, T0), latched(A, T0)` at the global previous bar.
- **The NOTES are wrong.** They claim "Makes trading decisions when conditions are met" and that the SP1500 label "wasn't verified".
- **Catchable:** yes. W4 fired, so W4 should be an error for `rank`, or at least say "rank: every group has one tuple, every K = 1".

### r8l-haiku-docs: 0 trades (never run; run.sh uses `--data` on a bundle)

Three independent killers:

1. **Same-timestamp joins of minute prints.** In the bundle, `open0_m` is keyed at 09:31 (minute 0's close), `close30_m` at 10:00, and `open_m` at about 10:01 and the session close. `clock` is at 10:00.
   ```
   29  m_value(A, T, M) :- clock(A, T, S), close30_m(A, T, C30), open0_m(A, T, O0), ...
   ```
   `explain`: `rule r8l::m_value#1 did not fire at 2010-12-30T10:00:00: literal 3 'open0_m(A, T, O0)' has no solution`.
2. **`atr_prev` and the other daily reads anchor on `open_m(A, T, _)` at the clock time.**
   `explain`: `r8l::atr_prev#1 ... literal 1 'open_m(A, T, _)' has no solution`.
3. **The ATR window is calendar-based.** `lib/r8l_features.dsl:22`: `ATR = mean(TR) over (T1 in window(T, 20d, min 20), ...)`. 20 calendar days hold about 14 sessions, so the window is never full.
   `explain`: `r8l_features::atr#1 did not fire ...: literal 2 'ATR = mean(TR) over (...)' has no solution`.

Other findings:
- Haiku wrote its daily features as a separate library in `lib/`. It believed "Relations at strategy @1m level default to @1m even when used at @1d. This must be controlled via separate libraries".
  - This is false. The README says "A relation's resolution follows the signature (`@1d`)".
  - The agent spent 6 attempts (3 to 8) on `error [X] ... 'close_d' is at @1d but this rule (head 'true_range') is at @1m; two resolutions meet only through resample`. That message names neither the `@1d` annotation nor `asof`.
- Other false beliefs: "The checker requires T0 < T for causality even though as-of semantics already guarantee T0 <= T", and "`0 USD` is Notional ... avoidance of explicit zero checks". The second was used to drop the `ATR > 0` guard.
- Delta mode with hand-written exits (`sell` when `close_m` is present and the previous minute had a clock) instead of `moo_moc`.
- **Catchable:**
  - Killer 3 is statically suspicious: `min K` greater than the number of bars a `Nd` window can hold at @1d. That needs an assumed calendar, so it should be a warning.
  - Killers 1 and 2 need a runtime empty-relation report.

### r8l-haiku-ex: 0 trades (ran, and rationalized the result)

- Same-timestamp bug: `m_val(A, T, M) :- clock(A, T, _), close30_m(A, T, C30), open0_m(A, T, O0), ...` (line 38). `explain`: `literal 3 'open0_m(A, T, O0)' has no solution`.
- The ATR window is empty by construction:
  ```
  22  ATR = mean(TR) over (T1 in window(T, 0d, min 20), tr(A, T1, TR)).
  ```
  A `0d` window holds only T, so `min 20` can never be met. The agent had already found this for `m_med` ("Initial approach used `window(T, 0d, min 100)` which selects only the current day") but left it in `atr`.
- The agent saw 0 decisions and wrote: "Zero trades is a valid outcome given the stringent entry criteria. The implementation faithfully reproduces the specification's rules."
- **Catchable statically:** `window(T, 0d, min K)` with K > 1, and more generally K above the window's maximum bar count, should be an error.

### mw14-sonnet-docs: 89.0% weeks exact, corr 0.998902, pnl 7.465M vs 7.535M

- **The first divergent decision** comes from dumping 1991 for both the reference and this run. Name 0000257990 is bought at 0.025 on 1991-08-02 by the reference (`W >= band - band_eps`) and one week later here.
- **The rule:**
  ```
  139  decide(T, target_weight(A, W, moc)) :- tgt(A, T, W), not held(A, T, _), W >= band.
  ```
  The reference has `W >= band - band_eps`. The spec's 1e-12 tolerance sits in the sentence about changes to held names. With `scale = 0.7`, `0.7/28` is `0.024999999999999998 < 0.025`. After the first miss the paths drift: weights, then the band, then everything else (203 of 1,852 weeks differ).
  - Patched copy (`review/mw14-sonnet-docs-fix`): **1,852/1,852 weeks equal to 1e-9, weekly correlation 1.000000, pnl 7,534,876.54, the same as the canon.** That one token (`- tol`) accounts for the whole residual. mw14-sonnet-ex's line 148 is the same, so the same patch applies there.
- **Faithful otherwise:**
  - Own-row recursion via `prev(T, Tp), close(A, T0, _) asof Tp, cum(A, T0, C0)`.
  - Ordinal and average ranks grouped by `bar(T)`.
  - The rows-based gate.
  - Crowding as `P > 1.5·M`, against the reference's `P/M - 1 > 0.5`.
- **Small deviation that did not matter:** `held` requires `universe(A, T)`, so a halted holding is invisible for that week.
- **Run flags** (its run.sh): the same headline as the reference-flag run (pnl 7.47M, 1,946 trades, commissions 217k, 149 delistings).
  - Differences from the reference flags: `--cash` vs `--capital`; the costs zeroed one by one instead of `--frictionless`; no `--end 2026-07-02`, so 1,856 periods instead of 1,852; no `--window-sums exact`.
  - None of these changes the result.

### mw14-sonnet-ex: identical to sonnet-docs to the cent

- Same rule at line 148 (`... not held(A, T, _), W >= band.`), same 7,465,408.61 pnl.
- Built differently:
  - The cumulative split factor is a running **sum of log factors** with `exp`, because there is no product aggregate.
  - The latch is a five-rule 0/1 state machine (`lstate`), because there is no conditional expression.
  - `panic` is written as `not gate(T), not calm(T)`.
- **The asof constant-pattern trap.** Attempt 4 had `lstate(A, _, 1) asof Tp`. That walks back to the latest row whose value is 1, not the previous row. It is semantically wrong and made the run take more than 20 minutes. Attempt 5 binds `F0` and then tests `F0 = 1`. No diagnostic warned about it.
- **Misleading diagnostic** (attempt 2, after naming a relation `target`): `error [F] ... temporal key 'T' of 'scale' is not derived from the head time ''; bind it first with prev, lag, window or prior_window of ''`. The head time prints as empty.
- Own run.sh (re-run in `review/runsh-mw14-sonnet-ex`): the same pnl (7,465,408.61, 1,946 trades) and the same 89.0% of weeks exact. Without `--end` it reports 1,856 periods and drops 105 decisions after the data ends. These flags do not change the result.

### r8l-sonnet-docs: 21,891/21,892 legs, +12 extra, Sharpe 1.2770 vs 1.2777

- **The "direction equal 41%" is a compare-script artifact.**
  - `r8l_compare.py` takes `dir = sign(amount)`.
  - In delta mode the dump records `ctor = short` with a positive amount.
  - The reference uses `target_quantity`, which is signed.
  - Re-scored with the constructor taken into account: **100.0% of matched legs agree on direction**. The 41% is just the share of long legs. `scripts/parity/r8l_compare.py` now signs by the constructor, and the stored `score/compare.txt` files are re-scored with it.
- **All 12 extras and the 1 missing leg are latefrac == 1/3 ties.** The extras are 6 `decide#1` and 6 `decide#2` (main legs, none sidecar).
  - Sonnet wrote the test without division: `3 * (C30 - C20) <= C30 - O0` (flipped for m < 0, lines 59 and 65). That is exact.
  - The book computes `(O + M·V - C20)/(M·V)` in floating point, as the reference does deliberately, and lands on either side of 1/3.
  - Checked on ZT 2009-08-06: O0 = 104.828125, C20 = 104.796875, C30 = 104.78125, so `3(C30 - C20) - (C30 - O0) = 0.0` exactly. Python's ratio rounds above 1/3; abt's short rule takes the leg.
  - The missing leg, CL 2020-09-02, has `latefrac = 0.333333`.
  - So this is a spec/parity artifact, not a rule error. The spec as written (exact arithmetic) agrees with Sonnet.
- **Gap alignment** was simplified correctly, to `G > -1` for longs and `G < 1` for shorts.
- **Own run.sh** (re-run in `review/runsh-r8l-sonnet-docs`): identical to the reference-flag run (21,903 decisions, Sharpe 1.2769, commissions 1,024,756).
  - It uses `--cash`, the costs zeroed one by one, `--margin none --on-ruin continue`, and no `--window-sums exact`.
  - None of this matters here. At most, exact sums would flip one ATR edge (ZT 2018-03-09) per parity-r8l.md, and the match shows it did not.

### r8l-sonnet-ex: 21,724 matched, 168 missing, +12

- **167 of the 168 missing legs are sidecars with m = 0 exactly** (C30 = O0, 6J 29, ZT 26, ZN 15, ...), dropped by a division guard in the shared signal:
  ```
  49      D = C30 - O0, abs(D) > 0 USD/share,
  50      M = D / ATR, G = (O0 - PC) / ATR, L = (C30 - C20) / D.
  ```
  - The guard protects `latefrac`, which only main legs need, but `sig` feeds the sidecar rule too.
  - The agent flagged this itself in NOTES ("SPEC DEVIATION RISK ... Rare"). It was not rare: 167 legs out of 4,939 sidecars.
  - The cause is that partial arithmetic halts the run, so authors guard early and over-broadly.
- **The 168th missing leg and the 12 extras** are the same 1/3 ties: `late_max = 0.3333333333333333` with `L = (C30 - C20)/D`.
- **Direction:** 100% equal once the constructor is taken into account.
- **Own run.sh:** identical to the reference-flag run (21,736 decisions, Sharpe 1.2547).
  - Its flags differ from the reference: `--max-gross 1000 --on-oversize allow --on-ruin continue --fee-bps 0` instead of `--frictionless`. No effect.

### Briefs (docs only, synthetic data; no reference)

- **momentum_liquid_monthly-sonnet:**
  - `member(A, T, "SP500")` silently gave zero decisions. The agent found `"SPX"` by trial with `--param`: "The index label for `member` is not documented anywhere ... Biggest difficulty."
  - The `briefs report` verdict is "does not trade (0 decisions)": the harness data differ, so the label or 200-observation guess fails silently again.
  - The 12-1 return is a sum of log returns minus the last 1mo sum, because there is no product aggregate.
- **momentum_liquid_monthly-haiku:**
  - Followed a cascade of D errors and added `T asc` to the `by` list ("add `T asc` as the tie-break") (the D message suggests it), when the real fix was binding T first (the first D error of the same check says so).
  - Used `mean` of daily returns (not compounded) over `prior_window` with a `lag` cutoff.
- **intraday_open_gap-haiku:** emits a `sell(..., moc)` every minute while held (7,372 decisions for 38 fills) and relies on silent supersession.
- **intraday_open_gap-sonnet:** clean from attempt 1. It guessed that `sell(A, Q, moc)` is legal (only target_weight is shown with an order type) and that `//` comments work.

---

## 2. Cross-cutting stumbling blocks (ranked by frequency × impact)

1. **Modes `+`/`-` for enumeration (8 of 12 runs; fatal for mw14-haiku-docs).**
   - Every mw14 run hit `error [M] ... '+A' of 'X' is an input and must be bound before the call` when counting or ranking over a derived relation.
   - Sonnet fixed it in 1 or 2 attempts ("Relations that must be enumerated need `-A` ... docs say little on when to use which").
   - Haiku-docs spent 9 attempts on it and then hard-coded counts.
   - The diagnostic never suggests changing the callee's signature.
2. **Silent emptiness (all 4 Haiku company runs and 1 Sonnet brief; 0 decisions with a clean check).** Causes:
   - a base case anchored on the wrong key;
   - degenerate per-asset rank;
   - same-T joins of relations keyed at different minutes;
   - unsatisfiable windows (`0d, min 20`; `20d, min 20` at @1d);
   - a wrong `member` label;
   - `--data` on a bundle directory ("note: no file ...; relation left empty", and the run proceeds).
   Nothing in `run` says which decide rule never fired or why. `explain` can say it, but it needs `--data` (no `--bundle`), a rule label, inputs and a time, and no Haiku agent used it.
3. **Temporal: "as of the previous own row" and own-row recursion (all mw14 runs; both r8l).**
   - `prev` is the global previous bar. The working idiom is `prev(T, Tp), R(A, _, V) asof Tp`, which Sonnet inferred and called "an idiom I inferred from the as-of section".
   - Haiku used `prev(T, T0), universe(A, T0)`, which breaks on gaps, and a global first-bar base (fatal).
   - Trap: a constant in an asof pattern changes which row is read (mw14-sonnet-ex, more than 20 minutes of runtime).
   - r8l: Sonnet used `session(A, Tp) asof T` plus key equality to stop carry-over. Haiku added redundant `T0 < T` guards.
4. **Windows and aggregates (6 runs).**
   - `rows` is documented in one table row.
   - Calendar `window` was used for row counts (`20d`/`52w`/`0d`).
   - No row-number primitive and no cumulative product: everyone rebuilt them by recursion, or with log-sum.
   - Aggregates do not nest (haiku-ex mw14 gave up on OLS).
   - `count(A)` with `A` outer-bound gives 1 (haiku-ex).
   - The rank grouping rule ("group of bound outer variables") was missed by both Haikus. W4 fired, but its text says `top`.
5. **Resolution (r8l, all 4).**
   - Haiku-docs spent 6 attempts on X errors and moved the features into a library. Haiku-ex took 2.
   - The X message says resolutions meet "only through resample", leaving out `asof` and the per-relation `@1d` annotation.
6. **Types and units (5 runs).**
   - `ATR > 0`, `ATR > 0 USD`, `Q > 0` give T errors. Fix: `0 USD/share`, `0 shares`.
   - `log(P)` needs `P / 1 USD/share`.
   - Count × Scalar is rejected; the trick is `K * (1/N)`.
   - Haiku concluded zero checks were impossible and removed guards.
7. **Invented syntax (Haiku: 9 parse failures).**
   - Prolog `;` disjunction, `or`/`and`, `not ( ... )`, `A in eligible(A, T)` as an aggregate generator, `-T` for the temporal key, `(expr) shares` as a unit cast, `52 w`, if/else.
   - Sonnet invented nothing that failed; its guesses that worked were `//` comments, order types on delta constructors, and `greatest` with 3 arguments.
8. **Partial arithmetic encourages over-broad guards** (r8l-sonnet-ex: 167 legs lost; r8l-sonnet-docs avoided division by cross-multiplying). There is no conditional expression or "skip if 0" operator.
9. **Executor/CLI flags (all Sonnet runs; 3 of the 6 abt calls in r8l-sonnet-docs went on flags).**
   - The defaults that bit them: margin halts, 5% margin rate, 0.278 bps fee on sells, per-share commission, 252 periods a year, `--data` vs `--bundle`.
   - It was not documented that moo/moo_moc read `open_m`, or that no decision means "keep the position" in target mode.
   - Bogus warning: `warning (transaction-cost neglect): commissions and fees are zero` printed alongside commissions of 216,975 charged via `--commission-bps`.
10. **Env discovery (3 runs).**
    - The `member` labels (`SPX` vs `SP500`) are undocumented.
    - `data-bundle.md` is referenced but not shipped.
    - The minute-print keying (which minute each `*_m` relation is labelled at) is only in comments. Both Haikus assumed everything sits at the clock minute.
11. **Spec misreading or ambiguity (Sonnet residuals).**
    - The band tolerance on new names (mw14).
    - latefrac ties (r8l): the book's float formula is not stated in the spec.
    - Sidecar with m = 0.

Doc claims agents found missing or wrong:
- `docs/data-bundle.md` is referenced and absent.
- The run-time estimates (15 s / 2 min) were 30 s / 4 to 6 min.
- The order-type syntax is shown only for `target_weight`.
- Rows syntax and the "first atom" rule are in one table row.
- There is no member-label vocabulary.
- The asof "matching tuple" consequence is unexplained.
- Target-mode "no decision keeps the position" is undocumented.
- Executor defaults are undocumented.

---

## 3. Ranked fixes

1. **Runtime "never fired" report in `abt run`** (evidence: all 4 Haiku zero-decision runs, the momentum label, r8l-haiku-ex's rationalization).
   - When a decide rule produces 0 tuples over the run, or the run makes 0 decisions, print for each decide rule the deepest body literal that never had a solution, and the first derived relation with 0 tuples. Example: `decide#1: held_eligible empty <- latched <- entered <- ranked <- score <- close_adj <- cum_split (0 tuples)`.
   - Make `explain` accept `--bundle`, and let it pick a sample time and inputs automatically.
2. **Better M diagnostic** (8 of 12 runs; 9 wasted Haiku attempts). Append: "to enumerate `A` here, declare it `-` in `rel held_eligible(-A: ...)`; `+` means the caller supplies it." Also add a README paragraph: "use `-A` for relations you count, rank or iterate; `+A` for per-asset features you call with A bound."
3. **Unknown label is an error or warning** (momentum-sonnet; mw14-haiku-ex was unsure).
   - Check string literals in `Label` positions against the bundle's label vocabulary at run start (`member: label "SP500" never occurs; known: SPX, ...`).
   - List the vocabularies in the env files.
4. **Static window sanity (E or W):** `window(T, 0d, min K>1)` is an error. `min K` greater than the bars `N` can hold at the resolution is a warning that suggests `rows(T, K, min K)` (r8l-haiku-ex, r8l-haiku-docs, mw14-haiku-docs gate).
5. **W4 for `rank` worded for rank, and promoted** (mw14-haiku-ex was fatal; mw14-haiku-docs). Text: "`rank` over `dvol`: `A` is bound before the rank, so every group has one tuple and every K is 1; bind only `T` (e.g. `bar(T), rank(...)`)." Consider making it an error for `rank`.
6. **X diagnostic names both fixes** (r8l-haiku-docs: 6 attempts): "... meet only through resample or `R(...) asof T`; to compute this relation at @1d declare `rel true_range(...) @1d`."
7. **Document the own-row idioms with a worked example** (all mw14 runs, r8l): the previous own row (`prev(T, Tp), R(A, _, X) asof Tp`), a row counter, a cumulative product via log-sum, and a 0/1 latch.
   - Warn when an asof atom has a constant in an output position ("a constant here selects the latest row *with that value*, not the latest row").
   - Consider a builtin `prev_row(A, T, T1)` / `row_number`, and a `prod` aggregate.
8. **Disjunction and conditionals** (Haiku: 5 parse failures on `;`, `or`, `not (...)`; Sonnet: a five-rule latch, sign-split rules).
   - Make the parse error say: "no `or`/`;`: write one rule per alternative".
   - Longer term, add an `if(c, a, b)` expression or `sign()`.
9. **Partial arithmetic ergonomics** (r8l-sonnet-ex, 167 legs). Document the "guard only in the rule that divides" pattern, or add a safe-divide form that fails only the literal.
10. **Typed zero hints:** for `ATR > 0` say "write `0 USD/share`". Fix the empty-head-time F message (`of ''`).
11. **CLI defaults and docs** (all Sonnet runs):
    - Add a `--book-preset` or document the "no costs, no margin" flag set.
    - Make `--data` on a bundle directory an error ("this is a bundle; use --bundle").
    - Fix the transaction-cost-neglect warning to count `--commission-bps`.
    - Document that `moo` reads `open_m` and that target mode holds when silent.
    - Ship `data-bundle.md` or drop the reference.
12. **Spec and parity notes:** state the float form of latefrac in the r8l task, and the tolerance for new names in mw14. Fix `r8l_compare.py` to sign by `ctor` (short/sell are -1).
