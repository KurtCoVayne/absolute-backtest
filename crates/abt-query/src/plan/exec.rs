//! The executor of a plan (Q-2.2 to Q-2.6). The steps run in the normal
//! form's topological order over a pool of kernel endpoints that share one
//! data layer (A-D5); every run is asked for with its π (Q-2.5) and read
//! back from the store, never from the kernel (A-D13). A trailing hold-out
//! truncates every run's stream at the last bar before the embargo, a
//! block hold-out masks the blocks out of every table read and every
//! ledger written (Q-2.3); a grid's points run in parallel; the
//! walk-forward is the old study's (every point trained up to the test
//! window and judged on the train window, the best judged on the test
//! window, the test windows stitched); a reveal runs one committed point
//! over the whole sample and reports the embargoed bars only. Every run is
//! a trial, written to `query.trials` with the plan's outcomes and the plan
//! itself (A-D20).

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;

use super::{At, Holdout, Normal, Plan, Point, Ref, Step};
use crate::data::{Data, Error};
use crate::kernel::{Kernel, Program, RunRequest, Sample};
use crate::metrics::validate::{self, Embargo, Surface, WalkForward};
use crate::metrics::{self, DeflatedSharpe, Pbo, ReturnMetrics, Returns, TradeMetrics, TradingMetrics};
use crate::run::Run;
use crate::time::{format_timestamp, parse_timestamp};

/// Where a plan runs: the data layer it reads and writes through, and the
/// kernel endpoints its runs are spread over.
pub struct Endpoints<'a> {
    pub data: &'a Data,
    pub kernels: &'a [Kernel],
}

/// How a plan runs.
#[derive(Clone, Debug)]
pub struct Options {
    /// Runs asked for at once (each held by a kernel while it runs).
    pub threads: usize,
    /// The reveal step to run (`abt plan reveal`); a plain execution runs
    /// none.
    pub reveal: Option<String>,
    /// The execution's time, nanoseconds since the epoch (what its trials
    /// are stamped with).
    pub now: i64,
}

impl Default for Options {
    fn default() -> Options {
        Options {
            threads: std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1).min(4),
            reveal: None,
            now: now(),
        }
    }
}

/// The wall clock, in nanoseconds since the epoch.
pub fn now() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos() as i64).unwrap_or(0)
}

/// What a trial a run is counts as (Q-2.5).
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Trial,
    Reveal,
}

impl Kind {
    pub fn name(self) -> &'static str {
        match self {
            Kind::Trial => "trial",
            Kind::Reveal => "reveal",
        }
    }
}

/// The part of a walk-forward a run is: fold `k` of `of`, its train or test
/// window `[from, to)`.
#[derive(Clone, Debug, PartialEq, serde::Serialize)]
pub struct Fold {
    pub k: usize,
    pub of: usize,
    pub phase: &'static str,
    pub from: i64,
    pub to: i64,
}

/// A warning a plan raises, with the bias it relates to (Q-8).
#[derive(Clone, Debug, PartialEq, serde::Serialize)]
pub struct Warning {
    pub bias: String,
    pub message: String,
}

fn warn(bias: &str, message: String) -> Warning {
    Warning { bias: bias.to_string(), message }
}

/// One run of a plan, with what was read back from it.
#[derive(Clone, Debug, serde::Serialize)]
pub struct Outcome {
    /// The step as written, and its canonical name.
    pub step: String,
    pub canonical: String,
    pub run_id: String,
    /// `completed` or `halted(reason)`.
    pub status: String,
    pub program_hash: String,
    pub binding: String,
    pub point: Point,
    pub fold: Option<Fold>,
    pub kind: Kind,
    /// The first and last bar the metrics are over.
    pub window: Option<(i64, i64)>,
    pub periods_per_year: f64,
    pub returns: ReturnMetrics,
    pub trading: TradingMetrics,
    /// The fixed-base and round-trip rows the red flags read (E-95).
    pub trades: TradeMetrics,
    /// The plan's objective's value (NaN when undefined).
    pub objective: f64,
    pub warnings: Vec<Warning>,
    /// The equity curve over the bars kept.
    #[serde(skip)]
    pub curve: Vec<(i64, f64)>,
}

/// A grid's result (Q-3.1's multiple-testing group).
#[derive(Clone, Debug, serde::Serialize)]
pub struct GridResult {
    pub step: String,
    /// The grid's runs, by their index among the execution's outcomes, in
    /// the grid's order.
    pub runs: Vec<usize>,
    /// The best point's position in `runs`.
    pub best: usize,
    pub pbo: Option<Pbo>,
    pub surface: Option<Surface>,
    pub subperiods: Vec<(i64, f64)>,
}

/// One walk-forward fold: the point chosen in sample and how it did out.
#[derive(Clone, Debug, PartialEq, serde::Serialize)]
pub struct FoldReport {
    pub train: (i64, i64),
    pub test: (i64, i64),
    /// The chosen point's index in the grid.
    pub best: usize,
    pub in_sample: f64,
    pub out_of_sample: f64,
    pub in_sample_cagr: f64,
    pub out_of_sample_cagr: f64,
}

#[derive(Clone, Debug, serde::Serialize)]
pub struct WalkForwardResult {
    pub step: String,
    pub scheme: WalkForward,
    pub folds: Vec<FoldReport>,
    /// Pardo's walk-forward efficiency: the mean out-of-sample CAGR over the
    /// mean in-sample CAGR of the chosen points (`None` when the latter is
    /// not positive).
    pub efficiency: Option<f64>,
    /// The metrics of the test windows stitched into one curve.
    pub out_of_sample: ReturnMetrics,
}

/// A `compare` step's table: per run, the metrics asked for by name.
#[derive(Clone, Debug, serde::Serialize)]
pub struct Comparison {
    pub step: String,
    pub rows: Vec<(String, Vec<(String, f64)>)>,
}

/// What an execution of a plan did.
#[derive(Clone, Debug, serde::Serialize)]
pub struct Execution {
    pub plan: String,
    pub hash: String,
    pub normal_form: String,
    pub program_hash: String,
    pub lineage: String,
    pub binding: String,
    /// The decision bars' span of the sample.
    pub sample: (i64, i64),
    pub holdout: String,
    pub embargo: Embargo,
    pub outcomes: Vec<Outcome>,
    pub grids: Vec<GridResult>,
    pub walk_forwards: Vec<WalkForwardResult>,
    /// `compare` steps: per run, the metrics asked for.
    pub comparisons: Vec<Comparison>,
    /// `ledger` steps: the relation each wrote.
    pub ledgers: Vec<(String, String)>,
    /// The claims judged (in sample, and on a reveal out of sample) and the
    /// test steps run.
    pub claims: Vec<super::claims::Judged>,
    pub tests: Vec<super::claims::Judged>,
    /// The feed and activation steps' tables.
    pub sheets: Vec<crate::sheet::Sheet>,
    /// The steps that did not run, and why (a failed `block` claim).
    pub skipped: Vec<(String, String)>,
    /// The report relations written.
    pub reports: Vec<String>,
    /// The lineage's trials after this execution, by kind, the variance of
    /// their Sharpe ratios, and the deflated Sharpe ratio of the plan's main
    /// result over them (Q-3.2).
    pub trials: usize,
    pub trial_kinds: BTreeMap<String, usize>,
    pub var_trials: f64,
    pub dsr: Option<DeflatedSharpe>,
    pub warnings: Vec<Warning>,
    /// When it ran.
    pub at: i64,
}

impl Execution {
    /// The plan's main result: the first grid's best point, else the first
    /// run.
    pub fn main(&self) -> Option<&Outcome> {
        match self.grids.first() {
            Some(g) => g.runs.get(g.best).map(|&i| &self.outcomes[i]),
            None => self.outcomes.iter().find(|o| o.fold.is_none() && o.kind == Kind::Trial),
        }
    }
}

/// Which bars of a run its metrics are over.
#[derive(Clone, Debug)]
enum Keep {
    /// Every bar outside the embargo.
    Outside,
    /// The embargoed bars only (a reveal).
    Inside,
    /// The bars of `[from, to)` outside the embargo (a fold's window).
    Window(i64, i64),
}

/// One run to ask for.
#[derive(Clone, Debug)]
struct Ask {
    step: String,
    canonical: String,
    point: Point,
    fold: Option<Fold>,
    kind: Kind,
    start: Option<i64>,
    end: Option<i64>,
    horizon: Option<i64>,
    /// Nothing received after this time is read (`holdout after TIME`).
    received_by: Option<i64>,
    mask: Vec<(i64, i64)>,
    observe: Vec<String>,
    keep: Keep,
    /// Kept by its kernel after it ends (a later step queries it).
    hold: bool,
}

/// The point with `over`'s names replaced and added, in name order.
pub fn merged(base: &Point, over: &Point) -> Point {
    let mut p: Point = base.iter().filter(|(k, _)| !over.iter().any(|(o, _)| o == k)).cloned().collect();
    p.extend(over.iter().cloned());
    p.sort();
    p
}

fn window_of(w: &Option<super::Window>) -> (Option<i64>, Option<i64>) {
    match w {
        Some(w) => (parse_timestamp(&w.from), parse_timestamp(&w.to)),
        None => (None, None),
    }
}

/// The index of the outcome with the best objective among `runs` (NaN
/// skipped; the first of equals).
fn best_of(outcomes: &[Outcome], runs: &[usize], objective: &str) -> usize {
    let better = metrics::higher_is_better(objective);
    let mut best = 0;
    for (i, &r) in runs.iter().enumerate() {
        let v = outcomes[r].objective;
        let b = outcomes[runs[best]].objective;
        if v.is_nan() {
            continue;
        }
        if b.is_nan() || (better && v > b) || (!better && v < b) {
            best = i;
        }
    }
    best
}

/// A plan's execution under way.
struct Executor<'a> {
    plan: &'a Plan,
    normal: &'a Normal,
    prog: &'a Program,
    ends: &'a Endpoints<'a>,
    opts: &'a Options,
    embargo: Embargo,
    program_hash: String,
}

impl<'a> Executor<'a> {
    /// π of a run (Q-2.5): the plan's hash, the step's canonical name, the
    /// point and the fold.
    fn pi(&self, a: &Ask) -> serde_json::Value {
        let point: BTreeMap<&str, &str> = a.point.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
        serde_json::json!({
            "plan": self.normal.hash,
            "step": a.canonical,
            "point": point,
            "fold": a.fold.as_ref().map(|f| serde_json::json!({"k": f.k, "phase": f.phase})),
        })
    }

    fn keeps(&self, keep: &Keep, t: i64) -> bool {
        match keep {
            Keep::Outside => !self.embargo.contains(t),
            Keep::Inside => self.embargo.contains(t),
            Keep::Window(a, b) => !self.embargo.contains(t) && *a <= t && t < *b,
        }
    }

    fn run_one(&self, kernel: &Kernel, a: &Ask) -> Result<Outcome, Error> {
        let req = RunRequest {
            point: a.point.clone(),
            start: a.start,
            end: a.end,
            horizon: a.horizon,
            received_by: a.received_by,
            plan_step: Some(self.pi(a)),
            mask: a.mask.clone(),
            observe: a.observe.clone(),
        };
        let ran = kernel.run(self.prog, &req)?;
        let mut o = Outcome {
            step: a.step.clone(),
            canonical: a.canonical.clone(),
            run_id: ran.run_id.clone(),
            status: ran.status.clone(),
            program_hash: self.program_hash.clone(),
            binding: String::new(),
            point: a.point.clone(),
            fold: a.fold.clone(),
            kind: a.kind,
            window: None,
            periods_per_year: 0.0,
            returns: ReturnMetrics::default(),
            trading: TradingMetrics::default(),
            trades: TradeMetrics::default(),
            objective: f64::NAN,
            warnings: ran.report["warnings"]
                .as_array()
                .map(|ws| {
                    ws.iter()
                        .map(|w| warn(w["bias"].as_str().unwrap_or("kernel"), w["message"].as_str().unwrap_or_default().to_string()))
                        .collect()
                })
                .unwrap_or_default(),
            curve: vec![],
        };
        if let Ok((binding, _)) = crate::run::binding_of(self.ends.data, &ran.run_id) {
            o.binding = binding;
        }
        if ran.status == "completed" {
            let run = Run::read(self.ends.data, &ran.run_id)?;
            let ppy = run.periods_per_year();
            let curve = metrics::masked_curve(&run.metric_curve(), &|t| self.keeps(&a.keep, t));
            o.periods_per_year = ppy;
            o.returns = metrics::return_metrics(&curve, ppy);
            o.trading = metrics::trading_metrics(&run, ppy);
            o.trades = metrics::trade_metrics(&run, ppy, &|t| self.keeps(&a.keep, t));
            o.objective = metrics::objective_value(&self.plan.objective, &o.returns, &o.trading, &o.trades).unwrap_or(f64::NAN);
            o.window = curve.first().zip(curve.last()).map(|(f, l)| (f.0, l.0));
            o.curve = curve;
        } else {
            o.warnings.push(warn("execution", format!("run {} of step {}: {}", ran.run_id, a.step, ran.status)));
        }
        if !a.hold && !ran.run_id.is_empty() {
            kernel.release(&ran.run_id)?;
        }
        Ok(o)
    }

    /// The runs, on `threads` workers over the kernel pool, in the asks'
    /// order (so that the trials are written in grid order whatever
    /// finishes first). The kernel of the i-th ask is the (i mod pool)-th.
    fn run_all(&self, asks: &[Ask]) -> Result<Vec<(Outcome, usize)>, Error> {
        let pool = self.ends.kernels.len();
        let workers = self.opts.threads.max(1).min(asks.len()).max(1);
        let next = AtomicUsize::new(0);
        let slots: Vec<Mutex<Option<Result<Outcome, Error>>>> = asks.iter().map(|_| Mutex::new(None)).collect();
        std::thread::scope(|s| {
            for _ in 0..workers {
                s.spawn(|| loop {
                    let i = next.fetch_add(1, Ordering::Relaxed);
                    if i >= asks.len() {
                        break;
                    }
                    let out = self.run_one(&self.ends.kernels[i % pool], &asks[i]);
                    *slots[i].lock().expect("a run's slot") = Some(out);
                });
            }
        });
        slots
            .into_iter()
            .enumerate()
            .map(|(i, m)| {
                m.into_inner()
                    .expect("a run's slot")
                    .unwrap_or_else(|| Err(Error::Wire("a plan's worker stopped".into())))
                    .map(|o| (o, i % pool))
            })
            .collect()
    }
}

/// The receipt time a hold-out truncates every run at (`after TIME`), what
/// the kernel's `show binding` is asked for the embargo's first bar with.
pub fn received_by(holdout: &Holdout) -> Option<i64> {
    match holdout {
        Holdout::After { time } => Some(*time),
        _ => None,
    }
}

/// The embargo of a hold-out over the sample's decision bars (Q-2.3,
/// E-97): a trailing hold-out counts back from the sample's last bar; a
/// percentage is of the span between the first and last decision bars of
/// the plan's window (the sample's when the plan has none); `after TIME`
/// begins at the first bar whose barrier closes after the time, which the
/// kernel named in the sample.
pub fn embargo(holdout: &Holdout, sample: &Sample, window: &Option<super::Window>) -> Embargo {
    let (Some(first), Some(last)) = (sample.first, sample.last) else { return Embargo::none() };
    match holdout {
        Holdout::None => Embargo::none(),
        Holdout::Trailing { months } => validate::trailing_months_embargo(last, *months),
        Holdout::TrailingShare { percent } => {
            let (from, to) = window_of(window);
            let first = from.map_or(first, |f| f.max(first));
            let last = to.map_or(last, |t| t.min(last));
            validate::trailing_share_embargo(first, last, *percent)
        }
        Holdout::After { .. } => validate::after_embargo(sample.embargo_from),
        Holdout::Blocks { count, months, seed } => validate::block_embargo(first, last, *count, *months, *seed),
    }
}

/// The plan's steps run against its program's sample, each run through the
/// endpoints, and every trial, outcome and the plan written to the data
/// layer. The order (Q-5.1): the `run` steps the `before` claims read, the
/// `before` claims, every other step (none that runs the kernel when a
/// `block` claim failed), the `after` claims, the reports.
pub fn execute(plan: &Plan, prog: &Program, ends: &Endpoints, opts: &Options) -> Result<Execution, Error> {
    let kernel = ends.kernels.first().ok_or_else(|| Error::Missing("a plan runs on at least one kernel".into()))?;
    let checked = kernel.check(prog)?;
    let normal = super::normal_form(plan, &checked.program_hash).map_err(Error::Malformed)?;
    if let Some(r) = &opts.reveal {
        match plan.step(r) {
            Some(Step::Reveal { .. }) => {}
            _ => return Err(Error::Missing(format!("study {} has no reveal step `{}`", plan.name, r))),
        }
        // A failed `block` claim after the runs bars the reveal (Q-5.1).
        if let Some(c) = super::trials::verdicts(ends.data, &normal.hash)?
            .into_iter()
            .find(|v| v.in_sample && v.policy == "block" && v.verdict == "fails")
        {
            return Err(Error::Malformed(format!("study {}: claim `{}` (block) failed, so nothing is revealed", plan.name, c.name)));
        }
    }
    let sample: Sample = kernel.sample_received_by(prog, &plan.params, received_by(&plan.holdout))?;
    let (Some(first), Some(last)) = (sample.first, sample.last) else {
        return Err(Error::Missing(format!("strategy `{}`'s binding {} has no decision bar", prog.strategy, sample.binding)));
    };
    let embargo = embargo(&plan.holdout, &sample, &plan.window);
    // A trailing embargo truncates every run before its first bar; an
    // `after` hold-out truncates at the last barrier closed by its time,
    // which the kernel finds by receipt (the embargo's first bar is its
    // answer, read back for the masks); a block embargo is a mask the
    // kernel applies to the ledgers.
    let received = received_by(&plan.holdout);
    let horizon = if embargo.truncates && received.is_none() { embargo.start().map(|h| h - 1) } else { None };
    let mask: Vec<(i64, i64)> = if embargo.truncates { vec![] } else { embargo.intervals.clone() };
    let opened = super::lineage::open(ends.data, &checked, &prog.strategy, opts.now)?;
    let ex = Executor {
        plan,
        normal: &normal,
        prog,
        ends,
        opts,
        embargo: embargo.clone(),
        program_hash: checked.program_hash.clone(),
    };
    let (start, end) = window_of(&plan.window);
    // The runs a later step queries through the kernel stay held.
    let queried: Vec<&Ref> = plan.steps.iter().filter_map(|s| if let Step::Ledger { of, .. } = s { Some(of) } else { None }).collect();
    let held = |step: &str| queried.iter().any(|r| r.step == step);
    // What the tests and claims read of a step's runs beyond its own
    // observations: the ledgers of `feature R of S`.
    let inputs: Vec<&super::Input> = plan
        .steps
        .iter()
        .filter_map(|s| if let Step::Test { input, .. } = s { Some(input) } else { None })
        .chain(plan.claims.iter().map(|c| &c.input))
        .collect();
    let features = |step: &str| -> Vec<String> {
        let mut v: Vec<String> = inputs
            .iter()
            .filter_map(|i| match i {
                super::Input::Feature { relation, of } if of.step == step => Some(relation.clone()),
                _ => None,
            })
            .collect();
        v.sort();
        v.dedup();
        v
    };
    let ask = |step: &str, point: Point, over: &Option<super::Window>, observe: Vec<String>, kind: Kind, keep: Keep| -> Ask {
        let (s, e) = match over {
            Some(_) => window_of(over),
            None => (start, end),
        };
        Ask {
            step: step.to_string(),
            canonical: normal.canonical(step).to_string(),
            point,
            fold: None,
            kind,
            start: s,
            end: e,
            horizon: if kind == Kind::Reveal { None } else { horizon },
            received_by: if kind == Kind::Reveal { None } else { received },
            mask: if kind == Kind::Reveal { vec![] } else { mask.clone() },
            observe,
            keep,
            hold: held(step),
        }
    };
    let mut out = Execution {
        plan: plan.name.clone(),
        hash: normal.hash.clone(),
        normal_form: normal.text.clone(),
        program_hash: checked.program_hash.clone(),
        lineage: opened.lineage.clone(),
        binding: sample.binding.clone(),
        sample: (first, last),
        holdout: plan.holdout.describe(),
        embargo: embargo.clone(),
        outcomes: vec![],
        grids: vec![],
        walk_forwards: vec![],
        comparisons: vec![],
        ledgers: vec![],
        claims: vec![],
        tests: vec![],
        sheets: vec![],
        skipped: vec![],
        reports: vec![],
        trials: 0,
        trial_kinds: BTreeMap::new(),
        var_trials: 0.0,
        dsr: None,
        warnings: vec![],
        at: opts.now,
    };
    if let Some(w) = &opened.warning {
        out.warnings.push(warn("data-snooping", w.clone()));
    }
    if plan.holdout == Holdout::None && opts.reveal.is_none() {
        out.warnings.push(warn(
            "out-of-sample",
            "the plan declares no hold-out: every run sees the whole sample, and nothing is left to reveal".into(),
        ));
    }
    // Where each step's runs are among the outcomes, and on which kernel.
    let mut runs_of: BTreeMap<String, Vec<(usize, usize)>> = BTreeMap::new();
    let mut best: BTreeMap<String, usize> = BTreeMap::new();
    let push = |out: &mut Execution, runs: Vec<(Outcome, usize)>| -> Vec<(usize, usize)> {
        runs.into_iter()
            .map(|(o, k)| {
                out.outcomes.push(o);
                (out.outcomes.len() - 1, k)
            })
            .collect()
    };
    let the_run = |runs_of: &BTreeMap<String, Vec<(usize, usize)>>, best: &BTreeMap<String, usize>, r: &Ref| -> Result<(usize, usize), Error> {
        let runs = runs_of.get(&r.step).ok_or_else(|| Error::Missing(format!("step `{}` made no run before it is read", r.step)))?;
        if r.best {
            let b = *best.get(&r.step).ok_or_else(|| Error::Missing(format!("grid `{}` has no best point", r.step)))?;
            runs.iter()
                .find(|(i, _)| *i == b)
                .copied()
                .ok_or_else(|| Error::Missing(format!("grid `{}`'s best point made no run", r.step)))
        } else {
            runs.last().copied().ok_or_else(|| Error::Missing(format!("step `{}` made no run", r.step)))
        }
    };
    // The `run` steps the `before` claims read run first.
    let early: std::collections::BTreeSet<String> = plan.claims.iter().filter(|c| c.phase == super::Phase::Before).flat_map(|c| c.input.steps()).collect();
    let canonical_claims = |phase: super::Phase| -> Vec<(&super::Claim, String)> { plan.claims.iter().filter(|c| c.phase == phase).map(|c| (c, normal.canonical(&c.name).to_string())).collect() };
    let in_sample = |t: i64| !embargo.contains(t);
    let mut blocked: Option<String> = None;
    let mut reports: Vec<String> = Vec::new();
    for stage in [true, false] {
        for (i, _) in &normal.order {
            let step = &plan.steps[*i];
            if early.contains(step.name()) != stage {
                continue;
            }
            // `abt plan reveal` runs its reveal step alone, and a plain
            // execution none.
            match (step, &opts.reveal) {
                (Step::Reveal { name, .. }, Some(r)) if name == r => {}
                (_, Some(_)) | (Step::Reveal { .. }, None) => continue,
                _ => {}
            }
            if let Some(why) = blocked.as_ref().filter(|_| step.runs()) {
                out.skipped.push((step.name().to_string(), why.clone()));
                continue;
            }
            if let Some(x) = step.inputs().into_iter().find(|x| out.skipped.iter().any(|(s, _)| s == x)) {
                out.skipped.push((step.name().to_string(), format!("it reads `{}`, which did not run", x)));
                continue;
            }
            match step {
                Step::Run { name, at, over, observe } => {
                    let point = match at {
                        None => merged(&plan.params, &vec![]),
                        Some(At::Point(p)) => merged(&plan.params, p),
                        Some(At::Best(g)) => {
                            let b = *best.get(g).ok_or_else(|| Error::Missing(format!("grid `{}` has no best point", g)))?;
                            out.outcomes[b].point.clone()
                        }
                    };
                    let mut obs = observe.clone();
                    obs.extend(features(name));
                    let runs = ex.run_all(&[ask(name, point, over, obs, Kind::Trial, Keep::Outside)])?;
                    let placed = push(&mut out, runs);
                    runs_of.insert(name.clone(), placed);
                }
                Step::Grid { name } => {
                    let asks: Vec<Ask> = plan.grid_points().into_iter().map(|p| ask(name, p, &None, features(name), Kind::Trial, Keep::Outside)).collect();
                    let runs = ex.run_all(&asks)?;
                    let placed = push(&mut out, runs);
                    let idx: Vec<usize> = placed.iter().map(|(i, _)| *i).collect();
                    let b = best_of(&out.outcomes, &idx, &plan.objective);
                    best.insert(name.clone(), idx[b]);
                    let ppy = out.outcomes[idx[b]].periods_per_year;
                    let pbo = if idx.len() >= 2 {
                        let rets: Vec<Returns> = idx.iter().map(|&i| Returns::from_curve(&out.outcomes[i].curve, ppy)).collect();
                        let n = rets.iter().map(|r| r.len()).min().unwrap_or(0);
                        let rows: Vec<Vec<f64>> = (0..n).map(|t| rets.iter().map(|r| r.r[t]).collect()).collect();
                        metrics::pbo_cscv(&rows, 16).or_else(|| metrics::pbo_cscv(&rows, 8))
                    } else {
                        None
                    };
                    let values: Vec<f64> = idx.iter().map(|&i| out.outcomes[i].objective).collect();
                    let surface = validate::surface(&plan.grid, &values, b);
                    let subperiods = validate::subperiod_sharpes(&out.outcomes[idx[b]].curve, ppy, 20);
                    out.grids.push(GridResult {
                        step: name.clone(),
                        runs: idx,
                        best: b,
                        pbo,
                        surface,
                        subperiods,
                    });
                    runs_of.insert(name.clone(), placed);
                }
                Step::WalkForward { name } => {
                    let scheme = plan.walk_forward.clone().expect("a walk-forward step has a scheme (checked)");
                    let wf_last = horizon.map_or(last, |h| h.min(last));
                    let folds = scheme.folds(first, wf_last);
                    if folds.is_empty() {
                        return Err(Error::Missing(format!(
                            "the sample ({} to {}) is shorter than one train and one test window of {}",
                            format_timestamp(first),
                            format_timestamp(wf_last),
                            scheme.describe()
                        )));
                    }
                    let within = |h: i64| Some(horizon.map_or(h, |s| s.min(h)));
                    let points = plan.grid_points();
                    let mut reports = Vec::new();
                    let mut stitched: Vec<(i64, f64)> = Vec::new();
                    let mut level = 1.0;
                    let mut placed_all = Vec::new();
                    let mut ppy = 0.0;
                    for (k, &(train_from, train_to, test_from, test_to)) in folds.iter().enumerate() {
                        let fold = |phase: &'static str, from: i64, to: i64| Fold {
                            k: k + 1,
                            of: folds.len(),
                            phase,
                            from,
                            to,
                        };
                        let asks: Vec<Ask> = points
                            .iter()
                            .map(|p| {
                                let mut a = ask(name, p.clone(), &None, vec![], Kind::Trial, Keep::Window(train_from, train_to));
                                a.fold = Some(fold("train", train_from, train_to));
                                a.horizon = within(test_from - 1);
                                a
                            })
                            .collect();
                        let placed = push(&mut out, ex.run_all(&asks)?);
                        let idx: Vec<usize> = placed.iter().map(|(i, _)| *i).collect();
                        placed_all.extend(placed);
                        if idx.iter().all(|&i| !out.outcomes[i].objective.is_finite()) {
                            continue;
                        }
                        let b = best_of(&out.outcomes, &idx, &plan.objective);
                        let (is_v, is_cagr) = (out.outcomes[idx[b]].objective, out.outcomes[idx[b]].returns.cagr);
                        let mut a = ask(name, points[b].clone(), &None, vec![], Kind::Trial, Keep::Window(test_from, test_to));
                        a.fold = Some(fold("test", test_from, test_to));
                        a.horizon = within(test_to - 1);
                        let placed = push(&mut out, ex.run_all(&[a])?);
                        let t = placed[0].0;
                        placed_all.extend(placed);
                        let o = &out.outcomes[t];
                        ppy = o.periods_per_year;
                        for w in o.curve.windows(2) {
                            let r = if w[0].1 > 0.0 { w[1].1 / w[0].1 - 1.0 } else { 0.0 };
                            if stitched.is_empty() {
                                stitched.push((w[0].0, level));
                            }
                            level *= 1.0 + r;
                            stitched.push((w[1].0, level));
                        }
                        reports.push(FoldReport {
                            train: (train_from, train_to),
                            test: (test_from, test_to),
                            best: b,
                            in_sample: is_v,
                            out_of_sample: o.objective,
                            in_sample_cagr: is_cagr,
                            out_of_sample_cagr: o.returns.cagr,
                        });
                    }
                    let n = reports.len().max(1) as f64;
                    let is_mean = reports.iter().map(|f| f.in_sample_cagr).sum::<f64>() / n;
                    let oos_mean = reports.iter().map(|f| f.out_of_sample_cagr).sum::<f64>() / n;
                    let efficiency = if is_mean > 0.0 { Some(oos_mean / is_mean) } else { None };
                    match efficiency {
                        Some(e) if e < 0.5 => out.warnings.push(warn(
                            "walk-forward",
                            format!("walk-forward efficiency {:.2}: out of sample the chosen points earn less than half their in-sample rate", e),
                        )),
                        None => out
                            .warnings
                            .push(warn("walk-forward", "walk-forward efficiency is undefined: the chosen points did not earn in sample".into())),
                        _ => {}
                    }
                    out.walk_forwards.push(WalkForwardResult {
                        step: name.clone(),
                        scheme,
                        folds: reports,
                        efficiency,
                        out_of_sample: metrics::return_metrics(&stitched, if ppy > 0.0 { ppy } else { 252.0 }),
                    });
                    runs_of.insert(name.clone(), placed_all);
                }
                Step::Reveal { name, of } => {
                    // The committed point: the one the plan's latest execution
                    // ran (Q-2.2).
                    let stored = super::trials::outcomes(ends.data, &normal.hash)?;
                    let point = stored
                        .iter()
                        .filter(|r| r.step == of.step && r.kind == "trial" && (!of.best || r.best))
                        .map(|r| r.point.clone())
                        .next_back()
                        .ok_or_else(|| {
                            Error::Missing(format!(
                                "study {} has not run step `{}`: a reveal names a committed point, one the plan has run (`abt plan run` first)",
                                plan.name, of.step
                            ))
                        })?;
                    if embargo.is_empty() {
                        return Err(Error::Missing(format!("study {}'s hold-out embargoes no bar of the sample: there is nothing to reveal", plan.name)));
                    }
                    let placed = push(&mut out, ex.run_all(&[ask(name, point, &None, features(&of.step), Kind::Reveal, Keep::Inside)])?);
                    runs_of.insert(name.clone(), placed);
                }
                Step::Compare { name, runs, by } => {
                    let mut rows = Vec::new();
                    for r in runs {
                        let (i, _) = the_run(&runs_of, &best, r)?;
                        let o = &out.outcomes[i];
                        let values = by
                            .iter()
                            .map(|m| {
                                metrics::objective_value(m, &o.returns, &o.trading, &o.trades)
                                    .map(|v| (m.clone(), v))
                                    .ok_or_else(|| Error::Malformed(format!("compare {}: `{}` is not a metric ({})", name, m, metrics::METRIC_NAMES.join(", "))))
                            })
                            .collect::<Result<Vec<_>, _>>()?;
                        rows.push((o.run_id.clone(), values));
                    }
                    out.comparisons.push(Comparison { step: name.clone(), rows });
                }
                Step::Ledger { name, relation, of, into } => {
                    let (i, k) = the_run(&runs_of, &best, of)?;
                    let run_id = out.outcomes[i].run_id.clone();
                    ends.kernels[k].ledger(&run_id, relation, into)?;
                    out.ledgers.push((name.clone(), format!("kernel.{}.{}", run_id, into)));
                }
                Step::Test { name, test, input, params, over } => {
                    let j = {
                        let ctx = context(&ex, &out, &runs_of, &best, &sample);
                        super::claims::apply(&ctx, name, normal.canonical(name), "test", test, input, over, params, &in_sample, true)
                    };
                    out.tests.push(j);
                }
                Step::Feed { name, statistic, relation, over } => {
                    let w = window_of(over);
                    let mut s = crate::feed::statistic(ends.data, &sample.relations, statistic, Some(relation.as_str()).filter(|r| !r.is_empty()), w)?;
                    s.name = format!("feed.{}", name);
                    out.sheets.push(s);
                }
                Step::Activation { name, of } => {
                    let (i, _) = the_run(&runs_of, &best, of)?;
                    let a = crate::activation::activation(
                        ends.data,
                        &out.outcomes[i].run_id,
                        &prog.strategy,
                        super::claims::seed(&normal.hash, normal.canonical(name)),
                        crate::activation::PHI_THRESHOLD,
                    )?;
                    for s in a.sheets() {
                        let mut s = s.clone();
                        s.name = format!("{}.{}", name, s.name);
                        out.sheets.push(s);
                    }
                    let pairs = &a.pairs;
                    if let Some(c) = pairs.column("candidate") {
                        for (k, cell) in c.iter().enumerate() {
                            if **cell == crate::sheet::Cell::Bool(true) {
                                let r = &pairs.rows[k];
                                out.warnings.push(warn(
                                    "overfitting",
                                    format!(
                                        "rules {:?} and {:?} fire together (phi above {}): a candidate for the kernel's structural check (`abt contract --implications`)",
                                        r[0],
                                        r[1],
                                        crate::activation::PHI_THRESHOLD
                                    ),
                                ));
                            }
                        }
                    }
                }
                Step::Report { name } => reports.push(name.clone()),
            }
        }
        if stage && opts.reveal.is_none() {
            // The `before` claims, on the in-sample bars.
            let judged = {
                let ctx = context(&ex, &out, &runs_of, &best, &sample);
                super::claims::judge(&ctx, &canonical_claims(super::Phase::Before), &in_sample, true)
            };
            if let Some(j) = judged.iter().find(|j| j.policy == Some(super::Policy::Block) && j.verdict == Some(super::claims::Verdict::Fails)) {
                blocked = Some(format!("claim `{}` (block) failed", j.name));
            }
            out.claims.extend(judged);
        }
    }
    if opts.reveal.is_none() {
        let judged = {
            let ctx = context(&ex, &out, &runs_of, &best, &sample);
            super::claims::judge(&ctx, &canonical_claims(super::Phase::After), &in_sample, true)
        };
        out.claims.extend(judged);
    } else if let Some(Step::Reveal { name, of }) = opts.reveal.as_ref().and_then(|r| plan.step(r)) {
        // Every claim again over the embargoed bars: what read the revealed
        // step reads the reveal's run; another step's runs never saw the
        // embargo (Q-5.1).
        if let Some(&(i, _)) = runs_of.get(name).and_then(|v| v.first()) {
            let reveal_run = out.outcomes[i].run_id.clone();
            let stored = super::trials::outcomes(ends.data, &normal.hash)?;
            let revealed = of.step.clone();
            let runs = |r: &Ref| -> Result<Vec<String>, Error> {
                if r.step == revealed {
                    Ok(vec![reveal_run.clone()])
                } else if stored.iter().any(|s| s.step == r.step) {
                    Err(Error::Missing(format!("step `{}`'s runs never saw the embargoed bars", r.step)))
                } else {
                    Err(Error::Missing(format!("step `{}` made no run", r.step)))
                }
            };
            let ledger = |s: &str| -> Result<String, Error> { Err(Error::Missing(format!("ledger step `{}` read no embargoed bar", s))) };
            let ctx = super::claims::Context {
                data: ends.data,
                strategy: &prog.strategy,
                binding: &sample.relations,
                runs: Box::new(runs),
                ledger: Box::new(ledger),
                folds: Box::new(|_: &str| None),
                defaults: lineage_defaults(ends.data, &out.lineage, &out.outcomes),
                plan_hash: &normal.hash,
            };
            let outside = |t: i64| embargo.contains(t);
            let judged: Vec<_> = [super::Phase::Before, super::Phase::After]
                .into_iter()
                .flat_map(|phase| super::claims::judge(&ctx, &canonical_claims(phase), &outside, false))
                .collect();
            drop(ctx);
            out.claims.extend(judged);
        }
    }
    for (step, why) in &out.skipped {
        out.warnings.push(warn("out-of-sample", format!("step `{}` did not run: {}", step, why)));
    }
    for j in out.claims.iter().filter(|j| j.in_sample) {
        if let Some(v @ (super::claims::Verdict::Fails | super::claims::Verdict::Undecidable(_))) = &j.verdict {
            out.warnings.push(warn("false statistical significance", format!("claim `{}` {}", j.name, v.text())));
        }
    }
    // The runs held for later steps are released.
    for (step, placed) in &runs_of {
        if held(step) {
            for &(i, k) in placed {
                let _ = ends.kernels[k].release(&out.outcomes[i].run_id);
            }
        }
    }
    for o in &out.outcomes {
        for w in &o.warnings {
            if !out.warnings.contains(w) {
                out.warnings.push(w.clone());
            }
        }
    }
    // The rows: the trials, the outcomes, the verdicts, the plan and its
    // execution.
    super::trials::write(ends.data, plan, &out)?;
    // The lineage's trials, every one ever made (Proposition Q-2.2), and the
    // deflated Sharpe ratio of the main result over them.
    let counted = super::lineage::trials(ends.data, &opened.lineage)?;
    out.trials = counted.count;
    out.trial_kinds = counted.by_kind.clone();
    out.var_trials = metrics_variance(&counted.sharpes);
    if let Some(m) = out.main().map(|o| o.returns.clone()) {
        let dsr = metrics::deflated_sharpe(m.sharpe_period, m.n, m.skew, m.kurtosis, counted.count.max(1), out.var_trials);
        let ppy = out.main().map(|o| o.periods_per_year).unwrap_or(252.0);
        sample_warnings(&mut out.warnings, &m, ppy, &dsr);
        out.dsr = Some(dsr);
    }
    for g in &out.grids {
        if let Some(p) = &g.pbo {
            if p.pbo >= 0.5 {
                out.warnings.push(warn(
                    "parameter over-optimization",
                    format!("probability of backtest overfitting {:.2} over the {}-point grid", p.pbo, p.trials),
                ));
            }
        }
        if let Some(s) = &g.surface {
            if s.stability < 0.5 {
                out.warnings.push(warn(
                    "parameter over-optimization",
                    format!(
                        "only {:.0}% of the best point's {} neighbours are within {:.3} of it: a peak, not a plateau",
                        s.stability * 100.0,
                        s.neighbours,
                        s.tolerance
                    ),
                ));
            }
        }
        let main = &out.outcomes[g.runs[g.best]];
        if g.subperiods.len() >= 2 {
            let sign = main.returns.sharpe >= 0.0;
            let agree = g.subperiods.iter().filter(|(_, s)| (*s >= 0.0) == sign).count();
            if agree * 2 < g.subperiods.len() {
                out.warnings.push(warn(
                    "non-stationarity",
                    format!(
                        "the Sharpe ratio has the whole sample's sign in only {} of {} calendar years ({})",
                        agree,
                        g.subperiods.len(),
                        g.subperiods.iter().map(|(y, s)| format!("{} {:.2}", y, s)).collect::<Vec<_>>().join(", ")
                    ),
                ));
            }
        }
    }
    // The reports, over everything above (Q-8).
    if opts.reveal.is_none() {
        for _ in &reports {
            let report = crate::report::build(ends.data, &out, &plan.report)?;
            out.reports = crate::report::write(ends.data, &out, &report, &plan.report)?;
        }
    }
    Ok(out)
}

/// What the tests and claims resolve their inputs against.
fn context<'a>(ex: &'a Executor<'a>, out: &'a Execution, runs_of: &'a BTreeMap<String, Vec<(usize, usize)>>, best: &'a BTreeMap<String, usize>, sample: &'a Sample) -> super::claims::Context<'a> {
    let runs = Box::new(move |r: &Ref| -> Result<Vec<String>, Error> {
        let placed = runs_of.get(&r.step).ok_or_else(|| Error::Missing(format!("step `{}` made no run before it is read", r.step)))?;
        if r.best {
            let b = *best.get(&r.step).ok_or_else(|| Error::Missing(format!("grid `{}` has no best point", r.step)))?;
            Ok(vec![out.outcomes[b].run_id.clone()])
        } else {
            Ok(placed.iter().map(|(i, _)| out.outcomes[*i].run_id.clone()).collect())
        }
    });
    let ledger = Box::new(move |s: &str| -> Result<String, Error> {
        out.ledgers
            .iter()
            .find(|(n, _)| n == s)
            .map(|(_, r)| r.clone())
            .ok_or_else(|| Error::Missing(format!("ledger step `{}` wrote no table", s)))
    });
    let folds = Box::new(move |s: &str| -> Option<Vec<(f64, f64)>> {
        out.walk_forwards
            .iter()
            .find(|w| w.step == s)
            .map(|w| w.folds.iter().map(|f| (f.in_sample_cagr, f.out_of_sample_cagr)).collect())
    });
    super::claims::Context {
        data: ex.ends.data,
        strategy: &ex.prog.strategy,
        binding: &sample.relations,
        runs,
        ledger,
        folds,
        defaults: lineage_defaults(ex.ends.data, &out.lineage, &out.outcomes),
        plan_hash: &ex.normal.hash,
    }
}

/// The parameters a plan supplies a claim with (Q-3.2): the lineage's trial
/// count, this execution's runs among them, and the variance of their
/// Sharpe ratios.
fn lineage_defaults(data: &Data, lineage: &str, outcomes: &[Outcome]) -> crate::tests::Params {
    let extra: Vec<(String, f64)> = outcomes
        .iter()
        .filter(|o| o.returns.sharpe_period.is_finite() && !o.run_id.is_empty())
        .map(|o| (o.run_id.clone(), o.returns.sharpe_period))
        .collect();
    let mut p = crate::tests::Params::new();
    if let Ok(c) = super::lineage::trials_with(data, lineage, &extra) {
        p.insert("trials".into(), crate::tests::Param::Int(c.count.max(1) as i64));
        p.insert("var_trials".into(), crate::tests::Param::Float(metrics_variance(&c.sharpes)));
    }
    p
}

/// The sample variance of the trials' Sharpe ratios (Q-3.2), 0 below two.
fn metrics_variance(x: &[f64]) -> f64 {
    if x.len() < 2 {
        return 0.0;
    }
    let m = x.iter().sum::<f64>() / x.len() as f64;
    x.iter().map(|v| (v - m).powi(2)).sum::<f64>() / (x.len() as f64 - 1.0)
}

/// The short-sample and multiple-testing warnings of the main result.
fn sample_warnings(warnings: &mut Vec<Warning>, m: &ReturnMetrics, ppy: f64, dsr: &DeflatedSharpe) {
    let years = m.n as f64 / ppy;
    if years < 2.0 {
        warnings.push(warn("short-sample", format!("the sample holds {:.1} years of bars; confidence in any metric is low below two", years)));
    }
    let mintrl = metrics::min_track_record_length(m.sharpe_period, 0.0, m.skew, m.kurtosis, 0.95);
    if mintrl.is_finite() && (m.n as f64) < mintrl {
        warnings.push(warn(
            "short-sample",
            format!(
                "{} observations, but a Sharpe ratio of {:.2} needs {:.0} to be distinguished from zero at 95 % (MinTRL)",
                m.n, m.sharpe, mintrl
            ),
        ));
    }
    if dsr.trials > 1 && dsr.dsr < 0.95 {
        warnings.push(warn(
            "multiple-testing",
            format!(
                "deflated Sharpe ratio {:.2} over {} trials in the lineage (expected maximum per-period Sharpe {:.3})",
                dsr.dsr, dsr.trials, dsr.expected_max_sr
            ),
        ));
    }
}
