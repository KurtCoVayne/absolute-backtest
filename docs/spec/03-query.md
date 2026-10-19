# 03 · The query layer (`abt-query`): plans, metrics, tests, claims

Oct 12, 2026 · build sheet for the third program. Tags and cross-references as in `00-architecture.md`.

---

## 1. Scope

The query layer is a client of the kernel's query service and of the data layer's query interface. It executes **plans** (studies): declared DAGs of runs, embargoes, grids, walk-forwards, reveals, tests, claims and reports. It computes **metrics** over the kernel's book and events. It runs the developer's **statistical tests** against feeds, features and outcomes through a framework the developer extends, judges **claims**, counts **trials** by lineage, and writes **reports** as relations in the data layer. It never evaluates a program, never touches the book, never stores a byte itself (A-D13, A-D17, A-D20).

Everything in `src/study/` today moves here, with `ExecConfig` replaced by the program's `execution` block, `trials.jsonl` by relations, and the metrics computed from the data layer's copy of the run rather than from `RunResult`.

---

## 2. Plans

### 2.1 Syntax

A plan is a unit in the block syntax of the language:

```
study NAME {
  program    PATH | NAME                       # the strategy; its normal form fixes the lineage
  params     { k = v; ... }                    # the base point (overrides the program's defaults within their ranges)
  grid       { k = v1, v2, ...; ... }          # axes; the Cartesian product is the set of points
  holdout    none | trailing:Ny | trailing:Nmo | trailing:N% | after TIME | blocks:K:Nmo[:seed]
  walk_forward anchored:TRAIN:TEST | rolling:TRAIN:TEST       # optional
  window     DATE .. DATE                      # the report window of every run (overrides the program's report block)
  budget     { money X; bytes Y; requests Z }  # what the plan's demands may spend (D-7.2)
  objective  sharpe | cagr | calmar | ...      # the metric a grid or a fold optimises (3.1's names)
  claims     { CLAIM ... }                     # section 5
  steps      { STEP ... }                      # section 2.2
  report     { metrics [NAMES]; tables [NAMES]; text }
}

template NAME(PARAM, ...) { the same blocks with PARAM in place of values }
study NAME = TEMPLATE(PARAM = value, ...)
```

A plan with no `steps` block has the default steps: `before` claims, `grid` (or a single `run`), `after` claims, `report`. Templates are instantiated by substitution before hashing; a template is not itself a plan.

### 2.2 Steps

| Step | Form | Reads | Writes | Counted |
| --- | --- | --- | --- | --- |
| `run` | `run NAME [at POINT] [over WINDOW] [observe R, ...]` | the program, the binding | `kernel.<run>.*` via the kernel service | yes |
| `grid` | `grid NAME` | the `grid` block | one run per point; `query.<plan>.outcomes` | one per point |
| `walk_forward` | `walk_forward NAME` | `walk_forward`, `grid` | one run per (fold, point); the stitched out-of-sample curve | one per (fold, point) |
| `reveal` | `reveal NAME of RUN|best(GRID)` | a committed point (one the plan has run) | a run over the whole sample reporting the embargoed bars only | yes, as a reveal |
| `test` | `test NAME: TEST on INPUT [params] [over WINDOW]` | a feed relation, a ledger, an outcome | `query.<plan>.tests` | yes |
| `claim` | the claims block, run `before` or `after` | as `test` | `query.<plan>.verdicts` | yes |
| `compare` | `compare NAME: RUNS by METRICS` | outcomes | a table | no |
| `ledger` | `ledger NAME: R of RUN into TABLE` | the kernel's observation of `R` | a relation | no |
| `feed` | `feed NAME: STATISTIC on RELATION [over WINDOW]` | the binding | a table (section 7) | no |
| `activation` | `activation NAME of RUN` | the kernel's C and F tables and `decide` ledgers | the tables of section 8 | no |
| `report` | `report NAME` | everything above | `query.<plan>.report.*` and the text | no |

A step's inputs name earlier steps; the steps form a DAG and are executed in topological order, runs of a `grid` in parallel across kernels sharing one data process (A-D5, G3).

### 2.3 Hold-out and embargo

A `trailing:Ny` hold-out truncates every run's stream at the last barrier before the embargo (`--to SEQ` on the kernel, D-5.5), so the kernel never sees the embargoed bars: the embargo is enforced by data, not by discipline. A `trailing:Nmo` hold-out counts months, a `trailing:N%` hold-out embargoes the last N % of the span between the first and last decision bars of the plan's window, and `after TIME` (an ISO-8601 date or datetime, UTC, nanoseconds admitted) begins the embargo at the last barrier whose receipt time is at or before TIME, so that no non-reveal run reads anything received after it and a reveal reports the bars after it (E-97). A `blocks` hold-out lets runs cross the blocks (the book's state must) and withholds every metric, test input and ledger row inside them: the query layer applies the block mask to every table it reads from the run, and the kernel's observation ledgers for the run are written only outside the blocks (the kernel is told the mask with the run request, P-3.4). A `reveal` lifts the embargo for one committed point and is logged as a reveal.

**Proposition 2.1 [N] (the embargo is enforced).** Under `trailing`, no decision, fill, ledger row or metric of a non-reveal run depends on embargoed data. *Proof.* The stream is truncated at the embargo's first barrier (D-5.5); by K-4.1 every output depends on events at or before the barriers consumed. ∎ Under `blocks` the book crosses the blocks by design, and what is withheld is every statistic over them; the proposition does not hold and the report says so, which is the honest status of a block hold-out.

### 2.4 Normal form and hash

A plan's normal form is its blocks with the template substituted, names canonicalised by first occurrence, steps in topological order with a canonical tie-break (step kind, then inputs), `report` removed. `H(Π)` is the BLAKE3 of the normal form (A-6). A plan with equal normal form is one plan; its execution log distinguishes executions by time and binding.

### 2.5 Step identity and trials

Every run request carries π = (H(Π), step name, point, fold) and the run id includes it (A-4.1). A **trial** is any step that reads an outcome's metric or runs a test: one trial per run of a grid or walk-forward, one per test, one per claim judged, one per reveal. Trials are rows of `query.trials` (the data layer, A-D20) with: trial id, plan hash, step, lineage, program hash, binding id, point, window, holdout, kind (`trial`, `reveal`, `test`, `claim`), the metric or verdict, the time.

**Lineage** is as `src/study/lineage.rs` today: a program belongs to its own lineage, to the one its `revises` header names, or to the nearest by Jaccard ≥ 0.8 of normalised rules, attached with a warning; disputes are rows.

**Proposition 2.2 [N] (every run is a counted trial).** Every kernel run is recorded in the data layer under its run id with H(P) (K-7.1), so the query layer counts, per lineage, every run ever made against the program's normal form, in or out of a plan. *Proof.* The kernel writes `binding` with H(P) for every run (K-7.1); the data layer is the single store (A-D20); a lineage query is `scan` over `kernel.*.binding` by H(P) and its revisions. ∎ The present "untracked trial" is therefore not a category: a run outside a plan is a trial of kind `run` in the lineage's count, and the DSR of 3.2 uses the full count.

### 2.6 Pinning

A plan pins the bindings of its runs for as long as it exists in the data layer (`query.plans`); `abt plan drop` unpins. A reveal re-resolves nothing: it runs over the binding of the committed point with the embargo lifted.

---

## 3. Metrics

### 3.1 Library

The present `src/study/metrics.rs` and `validate.rs` move unchanged in substance, computed from `kernel.<run>.book` (the NAV curve, the exposure) and `kernel.<run>.events` (fills, costs, funding), with the period convention from the program's `report` block (`periods N per year`, `calendar`) rather than a resolution table (V-4.5):

| Group | Metrics |
| --- | --- |
| Return | CAGR, volatility, Sharpe with Lo's autocorrelation-adjusted standard error, Sortino, Calmar, max drawdown and its duration, skewness, excess kurtosis, autocorrelation to lag 10, Newey–West t of the mean, probabilistic Sharpe, minimum track-record length, sub-period Sharpe by calendar year |
| Multiple testing | deflated Sharpe over the lineage's trial count with the variance of trial Sharpes; PBO by CSCV over a grid; walk-forward efficiency; parameter surface (smoothness, share of neighbours within 10 % of the best) |
| Trading | turnover, number of trades, hit rate, average win/loss, holding period, costs as a share of gross P&L, fill ratio, mean and max participation, capacity at a cost threshold |
| Book | gross, net and leverage over time; margin utilisation (`margin_req` over NAV); concentration (max weight, effective names); funding paid |
| Options | premium collected and paid, assignment and exercise counts, days to expiry at entry and exit, net Greek exposure over time (Δ·S, Γ, V, Θ from the kernel's `greeks` relation times positions), realised versus implied volatility of the underlyings traded, P&L attribution to Δ, Γ, Θ, V by the discrete Taylor expansion over each bar with the residual reported |
| Convention | both conventions (`--report-by day`, a reporting calendar) as today's `convention_metrics` |
| Red flags | on the fixed base, `return` (the mean bar P&L over the base, a year) and `mar` (over the drawdown of the cumulative P&L as a share of the base); `drawdown_duration`; the round trips' `trades`, `profit_factor`, `hit_rate`, `avg_win_loss`, `avg_trade`, `avg_trade_to_cost` (over the round-turn commission per contract or share), `largest_loss`, `trade_skew`; the profit's concentration, `top_market_share` and `profitable_market_share`; `profit_before_1990_share`; the contribution by entity and by calendar year (E-95; the pipeline's guideline §12) |

Every metric is a pure function of tables and is recorded with the tables' versions.

### 3.2 Trial counts in the deflated Sharpe

`trials` = the lineage's count by Proposition 2.2; `var_trials` = the variance of the Sharpe ratios of those trials. A plan's report states both and the count by kind.

---

## 4. The test framework

### 4.1 The interface

```
trait Test {
    fn name(&self) -> &str;                       // registry key, lowercase, stable
    fn version(&self) -> &str;                    // changes when the statistic or its algorithm changes; recorded with every result
    fn inputs(&self) -> &[InputSpec];             // each: a table shape (columns and types) and whether it is a series, a panel (entity × time) or a cross-section
    fn params(&self) -> &[ParamSpec];             // name, type, default, range
    fn null(&self) -> &str;                       // the null hypothesis in one sentence
    fn dependence(&self) -> Dependence;           // iid | newey_west { lags } | block_bootstrap { block, reps } | clustered { by: entity | time } | exact
    fn run(&self, tables: &[Table], params: &Params, seed: u64) -> Result<TestResult, TestError>;
}
struct TestResult { statistic: f64, p_value: Option<f64>, interval: Option<(f64, f64, f64)>  /* lo, hi, level */, effect: Option<f64>, df: Option<f64>, n: usize, notes: Vec<String> }
```

A test is a pure function of its inputs, parameters and seed; resampling uses the framework's seeded generator (a 64-bit xorshift*, as today's `metrics.rs`), so a result is reproducible to the bit. The **registry** maps names to implementations; the starting set (4.4) registers at start, and a developer adds a test by implementing the trait in a module of `abt-query` (or in a crate that depends on it and registers through `abt_query::register`). The plan refers to a test by name; the trial record carries `name@version`.

### 4.2 Dependence

Returns are serially correlated and cross-sectionally correlated within a bar, so textbook p-values are wrong here. The framework offers, and each test declares, one of: `iid` (the textbook statistic, admitted only where the test's own theory covers dependence or the input is a cross-section at one bar); `newey_west { lags }` (HAC standard errors, Bartlett kernel, lags by Newey–West's 4(n/100)^{2/9} rule when unset); `block_bootstrap { block, reps }` (stationary bootstrap, Politis and Romano 1994, block by Politis and White 2004 when unset, reps 2,000 by default); `clustered { by }` (standard errors clustered by entity or by time, Petersen 2009, for panels); `exact` (a permutation test where exchangeability holds, as for co-activation at one bar). A test's `dependence` is recorded with its result, and a plan may override it per test where the trait admits the override.

### 4.3 Multiple testing

Within a plan, every p-value of a claim is adjusted by Benjamini–Hochberg over the plan's claims (false discovery rate 0.05 unless the plan states a level), and the raw and adjusted values are both recorded; `bonferroni` is available by plan option. Across plans, the lineage's trial count is the only accounting, and it is what the deflated Sharpe reads.

### 4.4 The starting set

Each with its null, statistic, the dependence treatment it ships with, the reference, and its known-answer source (G9):

| Name | Null | Statistic | Dependence | Reference | Known answer |
| --- | --- | --- | --- | --- | --- |
| `jarque_bera` | normality | JB on skewness and kurtosis | block bootstrap | Jarque and Bera 1987 | textbook series; NIST StRD univariate moments |
| `ks_normal` | normality | Kolmogorov–Smirnov against the fitted normal (Lilliefors correction) | block bootstrap | Lilliefors 1967 | Lilliefors' table |
| `hill_tail` | tail index ≥ α₀ | Hill estimator at k order statistics with Hall's bootstrap for k | iid on exceedances | Hill 1975; Hall 1990 | simulated Pareto with known α |
| `ljung_box` | no autocorrelation to lag m | Q(m) | exact asymptotic | Ljung and Box 1978 | Box–Jenkins Series A |
| `variance_ratio` | random walk | Lo–MacKinlay VR(q) with heteroskedasticity-robust z | iid robust | Lo and MacKinlay 1988 | the paper's CRSP example |
| `arch_lm` | no ARCH effects to lag m | Engle's LM | asymptotic | Engle 1982 | the paper's UK inflation example |
| `adf` | unit root | augmented Dickey–Fuller with lag by AIC | asymptotic, MacKinnon p-values | Dickey and Fuller 1979; MacKinnon 1996 | MacKinnon's critical values; Nelson–Plosser series |
| `kpss` | stationarity | KPSS with Bartlett bandwidth | asymptotic | Kwiatkowski et al. 1992 | the paper's table |
| `half_life` | no mean reversion (φ = 1) | OU half-life from the AR(1) fit with its Newey–West interval | newey_west | standard | simulated OU with known θ |
| `engle_granger` | no cointegration | ADF on the residual of the cointegrating regression | asymptotic, MacKinnon | Engle and Granger 1987; MacKinnon 2010 | MacKinnon's tables |
| `johansen` | rank ≤ r | trace and max-eigenvalue | asymptotic | Johansen 1991; Osterwald-Lenum 1992 | the tables |
| `ic` | information coefficient = 0 | mean cross-sectional Spearman rank correlation of a feature with forward returns, with its Newey–West t | newey_west | Grinold and Kahn 2000 | simulated panel with planted IC |
| `fama_macbeth` | risk premium = 0 | Fama–MacBeth with Newey–West on the premium series | newey_west | Fama and MacBeth 1973 | Petersen's simulated panel |
| `psr`, `dsr`, `min_trl` | Sharpe ≤ benchmark | as `metrics.rs` | exact (moments) | Bailey and López de Prado 2012, 2014 | the papers' examples |
| `pbo` | no overfitting | CSCV | exact (combinatorial) | Bailey et al. 2017 | the paper's example |
| `bootstrap_sharpe` | Sharpe ≤ 0 | stationary bootstrap interval | block bootstrap | Politis and Romano 1994 | coverage on simulated series |
| `nw_mean` | mean = 0 | Newey–West t | newey_west | Newey and West 1987 | Greene's example |
| `wfe` | efficiency ≤ e₀ | Pardo's walk-forward efficiency | exact | Pardo 2008 | hand example |
| `coactivation` | independence of two rules' firing | phi, Jaccard, mutual information on the 2×2 table | clustered by time, plus permutation within bars | Ilyas et al. 2004 (CORDS) | hand tables |
| `effective_rules` | — | the effective number of independent rules from the eigenvalues of the activation correlation matrix | — | standard | hand matrices |
| `cost_realism` | modelled slippage = realised (live-sim or live-broker fills against the model) | paired t with Newey–West | newey_west | — | synthetic fills |
| `regression` | coefficients = 0 | OLS with HAC; the engine behind `ic`, `half_life`, `fama_macbeth` | newey_west | — | NIST StRD Norris, Pontius, Longley, Filip, Wampler 1–5 to the certified digits |

### 4.5 Extending

A developer's test is a type implementing `Test`, registered by name; its `version` string is in every result; it may reuse the framework's resampling, HAC and regression engines. Nothing else is required, and no test is privileged: the starting set uses the same interface.

### 4.6 Verification (G9)

`tests/known_answers.rs` runs every registered test of the starting set on its known-answer source at the tolerance the source allows (NIST certified values to their stated digits; published critical values to 1e-3; paper examples to the paper's printed precision), and property tests: scale invariance where the statistic is scale-free, permutation invariance for `iid` tests, monotonicity of the adjusted p-value in the number of claims, bit identity across thread counts and seeds held fixed.

---

## 5. Claims

### 5.1 Syntax

```
claims {
  CLAIM_NAME : TEST on INPUT [over WINDOW] [with { param = value, ... }] expect OUTCOME [at LEVEL] [before | after] [warn | block]
}
INPUT   = feed RELATION[(entity filter)] | feature R of RUN|grid | returns of RUN|grid | events of RUN | ledger NAME
OUTCOME = reject | accept | statistic OP VALUE | effect OP VALUE | interval excludes VALUE
```

`before` claims run on the in-sample window before any run of the plan (a feed or a feature computed by a warm-up run with no decisions, `observe` only); `after` claims run on outcomes. A claim's failure policy is `warn` (default) or `block` (the steps that depend on it do not run and the report says why). Every claim is re-tested on every reveal, over the embargoed bars, and the pair (in-sample verdict, out-of-sample verdict) is a row of `verdicts`.

### 5.2 Examples

```
claims {
  spread_stationary : adf on feature spread of warmup expect reject at 0.05 before
  signal_has_ic     : ic on feature score of warmup with { horizon = 1 } expect statistic > 0 at 0.05 before
  vol_clusters      : arch_lm on returns of warmup with { lags = 5 } expect reject before
  legs_cointegrate  : engle_granger on feed close(A in (XLE, XOP)) expect reject at 0.05 before
  profitable        : dsr on returns of best(main) expect statistic > 0.95 after
  not_overfit       : pbo on grid main expect statistic < 0.5 after
  rules_independent : coactivation on events of best(main) with { rules = (enter, exit) } expect effect < 0.3 after
  costs_right       : cost_realism on events of live_sim expect accept after
}
```

### 5.3 Semantics

**Definition 5.1 (claim).** A claim is (test, input, window, params, expectation, level, phase, policy). Its verdict is `holds`, `fails`, or `undecidable(reason)` (the input is empty, the test's preconditions fail, the window is inside the embargo). A verdict is a trial (2.5).

**Proposition 5.2 [N] (claims change nothing).** A claim's evaluation changes no run it reads. *Proof.* A-4.6. ∎ A `block` policy changes which runs *exist*, not what any run computes.

---

## 6. Feed statistics

Queries over the binding through the data layer, each a named statistic producing a table, available as the `feed` step and as `abt feed`:

| Statistic | Content |
| --- | --- |
| `coverage` | per relation: rows, entities, span, bars with no row per entity (gaps), the share of the calendar covered |
| `revisions` | per relation: revised facts, the distribution of `r − t`, the latest revision lag |
| `latency` | the distribution of receipt minus event time (what the kernel's late rule will see) |
| `returns` | per entity and pooled: moments, tails, autocorrelation, the tests of 4.4 on request |
| `liquidity` | ADV, dollar volume, the share of bars with zero volume, spread statistics from `quote` |
| `survivorship` | listings and delistings per year, the share of the universe at the span's start still listed at its end, delisting reasons |
| `actions` | splits and dividends per year; action reconciliation counts (the former bundle test, as a statistic) |
| `membership` | list sizes over time, turnover of membership, members without bars |
| `chains` | per underlying: contracts per day, expiries, strike coverage, quoted share, open interest |

These are the data-side facts the kernel's preconditions warn about (K-3.2), reported in full rather than counted.

---

## 7. Activation statistics

From the kernel's coverage table C, failure table F (when `trace instances` was declared) and the `decide` ledger of a run: per rule, the firing rate per bar and per entity-bar; per rule pair, the 2×2 contingency table over the attempted instances, phi, Jaccard, mutual information and `P(r₂ | r₁)`; cross-lag co-activation `P(r₂ at t | r₁ at t − k)` for `k ≤ 20`; activation autocorrelation; drift of firing rates across calendar years; the effective number of rules. Pairs with phi above a plan-stated threshold (default 0.9) are reported as candidates for the kernel's structural check (`abt contract --implications`, which asks the kernel's tier-3 verifier whether one guard implies the other; C-6.4), so that a proven pair is a program warning and an unproven one a data-relative note. Effect sizes carry block-bootstrap intervals; no bare p-values (4.2).

---

## 8. Reports

A report is a set of relations under `query.<plan>.report.*` and a text rendering:

| Table | Content |
| --- | --- |
| `summary` | per run: the metrics of 3.1 chosen by the `report` block |
| `trials` | the lineage's trials by kind, the DSR inputs |
| `claims` | every claim with its in-sample and out-of-sample verdicts, raw and adjusted p-values |
| `contract` | the kernel's contract statuses per run (K-3.7), aggregated |
| `warnings` | every warning raised by any layer for the plan's runs, with the bias it maps to and the count |
| `biases` | the forty biases of B-4 to B-6 with their owner layer and status source, rows touched marked |
| `surface`, `folds`, `subperiods` | as today; `subperiods` also carries the best run's net profit and trades per calendar year (E-95) |
| `contribution` | per run and entity: the net profit of its trades, its share of the run's, its trades and their hit rate (E-95) |
| `feed`, `activation` | the tables of sections 6 and 7 the plan asked for |

The forty-bias table's owners after the split: *data* (historical revision, point in time by receipt, latency: measurable because the store keeps `r`), *kernel* (look-ahead, bar resolution, execution timing, partial fill, transaction cost, slippage, impact, liquidity, spread, leverage, funding, shorting, borrow, corporate action, delisting, cash management, portfolio construction, rebalancing), *query* (survivorship and selection as feed statistics; data snooping, multiple testing, overfitting, parameter over-optimisation, p-hacking, false significance, short sample, out-of-sample, walk-forward, autocorrelation, non-stationarity, regime, tail risk, label and feature leakage through the embargo and the claims), *program* (universe leakage through `instruments` tables, W9). The report lists each with its status (`guaranteed`, `data-guaranteed`, `modeled`, `measured`, `warned`, `open`) and the source of that status.

---

## 9. Commands

```
abt query serve   [--socket PATH] [--data SOCKET|inproc] [--kernel SOCKET|inproc]
abt plan check    PLAN                         # the plan's normal form, hash, DAG and the trials it will count
abt plan run      PLAN [--threads N] [--dry-run]
abt plan reveal   PLAN STEP
abt plan report   PLAN [--text|--json|--tables]
abt plan drop     PLAN
abt metrics       --run ID [--periods N] [--calendar C]
abt test          TEST on INPUT ... [--json]    # one test, outside a plan, counted
abt claim         'CLAIM' --run ID
abt feed          STATISTIC --binding B [--over WINDOW]
abt activation    --run ID
abt lineage       PROGRAM                      # members, trials by kind, disputes
abt dispute       PROGRAM --reason TEXT
```

Every command takes `--json`.

---

## 10. What is established, new and decided in this document

- [E] The tests of 4.4 with their references; Benjamini–Hochberg 1995; Newey–West 1987; Politis–Romano 1994 and Politis–White 2004; Petersen 2009; CORDS (Ilyas et al. 2004); the deflated Sharpe and PBO (Bailey and López de Prado); Pardo's walk-forward efficiency; NIST StRD.
- [N] The embargo is enforced by truncation (2.1); every run is a counted trial (2.2); claims change nothing (5.2).
- [D] Plans in the block syntax with templates (2.1); the embargo by stream truncation rather than by masking for trailing hold-outs; the trait with declared dependence and a seeded generator (4.1); BH within a plan and lineage counts across plans (4.3); claims with `warn` default and `block` as the only other policy (5.1); no bare p-values in activation statistics (7); the bias owners re-mapped onto the layers (8).
