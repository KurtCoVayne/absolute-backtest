//! The study API (data-bundle doc, sections 6 and 7): the only way to run a
//! strategy that counts. A study directory holds the lineages
//! (`lineages.json`), the declared studies (`studies/<id>.json`) and the
//! append-only trial log (`trials.jsonl`). `declare` fixes a study's inputs
//! (lineage, hold-out policy, objective, executor configuration,
//! thresholds) and `run` logs one trial per parameter point, reporting the
//! metrics with the deflated Sharpe ratio over the lineage's trial count
//! and the probability of backtest overfitting over the grid.
//!
//! Everything here is pure over `RunResult`; the kernel never reads it.

pub mod lineage;
pub mod log;
pub mod metrics;
pub mod validate;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::check::Program;
use crate::ir::Lit;
use crate::kernel::time::format_timestamp;
use crate::kernel::{Dataset, ExecConfig, RunError, RunResult};

pub use lineage::{jaccard, normalized_rules, program_hash, Attachment, Lineage, Lineages, Opened, SIMILARITY_THRESHOLD};
pub use log::{sharpe_variance, Trial, TrialKind, TrialLog};
pub use metrics::{
    block_bootstrap_sharpe, capacity, deflated_sharpe, masked_curve, min_track_record_length, newey_west_t, pbo_cscv, periods_per_year, probabilistic_sharpe, return_metrics, trading_metrics,
    DeflatedSharpe, Pbo, ReturnMetrics, Returns, TradingMetrics,
};
pub use validate::{block_embargo, subperiod_sharpes, surface, trailing_embargo, Embargo, Surface, WalkForward};

/// The hold-out policy a study declares (section 6, out-of-sample).
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum Holdout {
    /// No hold-out: every run sees the whole sample (warned).
    None,
    /// The last `years` of the sample are embargoed: a run is truncated
    /// before them until a committed version is revealed.
    Trailing { years: u32 },
    /// `count` random blocks of `months` months are embargoed: a run
    /// crosses them (the book's state must) but reports no metric over
    /// them until revealed.
    Blocks { count: u32, months: u32, seed: u64 },
}

impl Holdout {
    /// `none`, `trailing:Ny` or `blocks:K:Nmo[:SEED]`.
    pub fn parse(s: &str) -> Result<Holdout, String> {
        let s = s.trim();
        if s == "none" {
            return Ok(Holdout::None);
        }
        if let Some(rest) = s.strip_prefix("trailing:") {
            let years: u32 = rest.trim_end_matches('y').parse().map_err(|_| format!("`{}` is not a number of years (trailing:2y)", rest))?;
            if years == 0 {
                return Err("a trailing hold-out needs at least one year".into());
            }
            return Ok(Holdout::Trailing { years });
        }
        if let Some(rest) = s.strip_prefix("blocks:") {
            let parts: Vec<&str> = rest.split(':').collect();
            if parts.len() < 2 || parts.len() > 3 {
                return Err(format!("`{}` is not a block hold-out (blocks:4:3mo or blocks:4:3mo:seed)", s));
            }
            let count: u32 = parts[0].parse().map_err(|_| format!("`{}` is not a number of blocks", parts[0]))?;
            let months: u32 = parts[1].trim_end_matches("mo").parse().map_err(|_| format!("`{}` is not a number of months (3mo)", parts[1]))?;
            let seed: u64 = parts.get(2).map(|x| x.parse().map_err(|_| format!("`{}` is not a seed", x))).transpose()?.unwrap_or(1);
            if count == 0 || months == 0 {
                return Err("a block hold-out needs at least one block of one month".into());
            }
            return Ok(Holdout::Blocks { count, months, seed });
        }
        Err(format!("`{}` is not a hold-out policy: none, trailing:Ny or blocks:K:Nmo[:SEED]", s))
    }

    pub fn describe(&self) -> String {
        match self {
            Holdout::None => "none".into(),
            Holdout::Trailing { years } => format!("trailing:{}y", years),
            Holdout::Blocks { count, months, seed } => format!("blocks:{}:{}mo:{}", count, months, seed),
        }
    }

    /// The bars of a dataset the policy embargoes.
    pub fn embargo(&self, ds: &Dataset) -> Embargo {
        let bars: Vec<i64> = ds.facts.values().flat_map(|tus| tus.iter()).filter_map(|tu| tu.iter().find_map(|v| v.as_time())).collect();
        let (Some(first), Some(last)) = (bars.iter().min().copied(), bars.iter().max().copied()) else {
            return Embargo::none();
        };
        match self {
            Holdout::None => Embargo::none(),
            Holdout::Trailing { years } => validate::trailing_embargo(last, *years),
            Holdout::Blocks { count, months, seed } => validate::block_embargo(first, last, *count, *months, *seed),
        }
    }

    /// The first embargoed bar of a dataset, if any.
    pub fn embargo_start(&self, ds: &Dataset) -> Option<i64> {
        self.embargo(ds).start()
    }
}

/// A warning the study raises, with the bias it relates to.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct StudyWarning {
    pub bias: String,
    pub message: String,
}

/// A declared study (section 7, `study.declare`).
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct StudySpec {
    pub id: String,
    pub strategy: String,
    pub lineage: String,
    pub hash: String,
    pub holdout: Holdout,
    /// The metric a grid or a walk-forward optimises (`sharpe` by default).
    pub objective: String,
    /// The author's own constraints, `metric >= x` or `metric <= x`, which
    /// the report checks (section 7, "what the system does not judge").
    pub thresholds: Vec<Threshold>,
    pub exec: ExecConfig,
    pub declared_at: u64,
    pub warnings: Vec<StudyWarning>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Threshold {
    pub metric: String,
    pub at_least: bool,
    pub value: f64,
}

impl Threshold {
    /// `metric>=x` or `metric<=x`.
    pub fn parse(s: &str) -> Result<Threshold, String> {
        let (metric, at_least, value) = if let Some((m, v)) = s.split_once(">=") {
            (m, true, v)
        } else if let Some((m, v)) = s.split_once("<=") {
            (m, false, v)
        } else {
            return Err(format!("`{}` is not a threshold (metric>=x or metric<=x)", s));
        };
        let metric = metric.trim().to_string();
        if objective_value(&metric, &ReturnMetrics::default(), &TradingMetrics::default()).is_none() {
            return Err(format!("`{}` is not a metric the report knows ({})", metric, METRIC_NAMES.join(", ")));
        }
        let value: f64 = value.trim().parse().map_err(|_| format!("`{}`: `{}` is not a number", s, value.trim()))?;
        Ok(Threshold { metric, at_least, value })
    }

    pub fn describe(&self) -> String {
        format!("{} {} {}", self.metric, if self.at_least { ">=" } else { "<=" }, self.value)
    }

    pub fn holds(&self, m: &ReturnMetrics, t: &TradingMetrics) -> Option<bool> {
        let v = objective_value(&self.metric, m, t)?;
        Some(if self.at_least { v >= self.value } else { v <= self.value })
    }
}

/// The metrics a study may optimise or constrain by name.
pub const METRIC_NAMES: [&str; 12] = [
    "sharpe",
    "sharpe_lo",
    "cagr",
    "total_return",
    "sortino",
    "volatility",
    "max_drawdown",
    "max_drawdown_duration",
    "cvar_5",
    "worst_month",
    "turnover",
    "max_leverage",
];

/// A named metric's value for a run.
pub fn objective_value(name: &str, m: &ReturnMetrics, t: &TradingMetrics) -> Option<f64> {
    Some(match name {
        "sharpe" => m.sharpe,
        "sharpe_lo" => m.sharpe_lo,
        "cagr" => m.cagr,
        "total_return" => m.total_return,
        "sortino" => m.sortino,
        "volatility" => m.volatility,
        "max_drawdown" => m.max_drawdown,
        "max_drawdown_duration" => m.max_drawdown_duration as f64,
        "cvar_5" => m.cvar_5,
        "worst_month" => m.worst_month,
        "turnover" => t.turnover,
        "max_leverage" => t.max_leverage,
        _ => return None,
    })
}

/// Whether a higher value of the metric is better (drawdowns, volatility,
/// CVaR, turnover and leverage are costs).
pub fn higher_is_better(name: &str) -> bool {
    !matches!(name, "volatility" | "max_drawdown" | "max_drawdown_duration" | "cvar_5" | "turnover" | "max_leverage")
}

/// A study directory.
pub struct Project {
    pub dir: PathBuf,
}

fn now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

impl Project {
    pub fn open(dir: &Path) -> Project {
        Project { dir: dir.to_path_buf() }
    }

    pub fn lineages(&self) -> Result<Lineages, String> {
        Lineages::load(&lineage::lineages_path(&self.dir))
    }

    pub fn save_lineages(&self, l: &Lineages) -> Result<(), String> {
        l.save(&lineage::lineages_path(&self.dir))
    }

    pub fn log(&self) -> TrialLog {
        TrialLog::in_dir(&self.dir)
    }

    fn studies_dir(&self) -> PathBuf {
        self.dir.join("studies")
    }

    pub fn save_study(&self, s: &StudySpec) -> Result<(), String> {
        let dir = self.studies_dir();
        std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {}", dir.display(), e))?;
        let path = dir.join(format!("{}.json", s.id));
        let text = serde_json::to_string_pretty(s).map_err(|e| e.to_string())?;
        std::fs::write(&path, text).map_err(|e| format!("{}: {}", path.display(), e))
    }

    pub fn studies(&self) -> Result<Vec<StudySpec>, String> {
        let dir = self.studies_dir();
        if !dir.exists() {
            return Ok(vec![]);
        }
        let mut out = Vec::new();
        let mut entries: Vec<PathBuf> = std::fs::read_dir(&dir)
            .map_err(|e| format!("{}: {}", dir.display(), e))?
            .filter_map(|e| e.ok().map(|e| e.path()))
            .collect();
        entries.sort();
        for p in entries {
            if p.extension().map(|x| x == "json").unwrap_or(false) {
                let text = std::fs::read_to_string(&p).map_err(|e| format!("{}: {}", p.display(), e))?;
                out.push(serde_json::from_str(&text).map_err(|e| format!("{}: {}", p.display(), e))?);
            }
        }
        out.sort_by_key(|s: &StudySpec| s.declared_at);
        Ok(out)
    }

    /// The latest study declared for a lineage.
    pub fn study_for(&self, lineage: &str) -> Result<Option<StudySpec>, String> {
        Ok(self.studies()?.into_iter().rfind(|s| s.lineage == lineage))
    }

    /// Open the program's lineage (creating or attaching) and persist it.
    pub fn open_lineage(&self, prog: &Program) -> Result<Opened, String> {
        let mut ls = self.lineages()?;
        let opened = ls.open(prog, now())?;
        if !opened.existing {
            self.save_lineages(&ls)?;
        }
        Ok(opened)
    }

    /// `study.declare`: fix the study's inputs and warn about what is
    /// missing (no hold-out, zero costs or slippage).
    pub fn declare(&self, prog: &Program, holdout: Holdout, objective: &str, thresholds: Vec<Threshold>, exec: ExecConfig) -> Result<(StudySpec, Opened), String> {
        if objective_value(objective, &ReturnMetrics::default(), &TradingMetrics::default()).is_none() {
            return Err(format!("`{}` is not a metric a study can optimise ({})", objective, METRIC_NAMES.join(", ")));
        }
        let opened = self.open_lineage(prog)?;
        let mut warnings = Vec::new();
        if holdout == Holdout::None {
            warnings.push(StudyWarning {
                bias: "out-of-sample".into(),
                message: "the study declares no hold-out: every trial sees the whole sample, and nothing is left to reveal".into(),
            });
        }
        for w in exec.warnings() {
            warnings.push(StudyWarning { bias: w.bias, message: w.message });
        }
        if let Some(w) = &opened.warning {
            warnings.push(StudyWarning {
                bias: "data-snooping".into(),
                message: w.clone(),
            });
        }
        let key = format!(
            "{}|{}|{}|{}|{}",
            opened.lineage,
            holdout.describe(),
            objective,
            thresholds.iter().map(|t| t.describe()).collect::<Vec<_>>().join(","),
            serde_json::to_string(&exec).map_err(|e| e.to_string())?
        );
        let id = format!("{}-{}", prog.strategy, &format!("{:016x}", fnv1a(&key))[..8]);
        let spec = StudySpec {
            id,
            strategy: prog.strategy.clone(),
            lineage: opened.lineage.clone(),
            hash: opened.hash.clone(),
            holdout,
            objective: objective.to_string(),
            thresholds,
            exec,
            declared_at: now(),
            warnings,
        };
        self.save_study(&spec)?;
        Ok((spec, opened))
    }
}

fn fnv1a(text: &str) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in text.bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

/// One point of a parameter grid: overrides by parameter name.
pub type Point = Vec<(String, Lit)>;

/// `name=v1,v2,...` (a literal in the DSL's grammar per value).
pub fn parse_grid_axis(s: &str) -> Result<(String, Vec<Lit>), String> {
    let (name, values) = s.split_once('=').ok_or_else(|| format!("--grid takes NAME=V1,V2,..., not `{}`", s))?;
    let name = name.trim().to_string();
    let mut lits = Vec::new();
    for raw in values.split(',') {
        let raw = raw.trim();
        if raw.is_empty() {
            continue;
        }
        let lit = crate::parser::parse_lit(raw).map_err(|e| format!("--grid {}: `{}` is not a literal: {}", name, raw, e.message))?;
        lits.push(lit);
    }
    if lits.is_empty() {
        return Err(format!("--grid {}: no values", name));
    }
    Ok((name, lits))
}

/// The cartesian product of the axes, in axis-major order; one empty point
/// without axes.
pub fn grid_points(axes: &[(String, Vec<Lit>)]) -> Vec<Point> {
    let mut points: Vec<Point> = vec![vec![]];
    for (name, values) in axes {
        let mut next = Vec::new();
        for p in &points {
            for v in values {
                let mut q = p.clone();
                q.push((name.clone(), v.clone()));
                next.push(q);
            }
        }
        points = next;
    }
    points
}

/// Every parameter of the strategy with its effective value under the
/// configuration's overrides.
pub fn effective_params(prog: &Program, cfg: &ExecConfig) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    for (unit, params) in &prog.params {
        for (name, p) in params {
            let key = if *unit == prog.strategy { name.clone() } else { format!("{}::{}", unit, name) };
            let value = cfg
                .param_overrides
                .iter()
                .rev()
                .find(|(n, _)| *n == key || (*unit == prog.strategy && *n == *name))
                .map(|(_, l)| l.to_string())
                .unwrap_or_else(|| p.value.to_string());
            out.insert(key, value);
        }
    }
    out
}

/// What runs a program on a dataset (the batch kernel, the fold, ...).
pub type Runner<'a> = dyn Fn(&Program, &Dataset, ExecConfig) -> Result<RunResult, RunError> + 'a;

/// Where a run's data came from and how it was scheduled, for the log.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Provenance {
    /// `name@version` when the data came from a bundle.
    pub bundle: Option<String>,
    /// The walk-forward scheme, when one was declared.
    pub scheme: Option<WalkForward>,
}

/// A parameter grid: its axes and the points to run, each carrying the
/// base overrides first.
#[derive(Clone, Debug, PartialEq)]
pub struct Grid {
    pub axes: Vec<(String, Vec<Lit>)>,
    pub points: Vec<Point>,
}

impl Grid {
    pub fn new(axes: Vec<(String, Vec<Lit>)>, base: &[(String, Lit)]) -> Grid {
        let points = grid_points(&axes)
            .into_iter()
            .map(|p| {
                let mut all: Point = base.to_vec();
                all.extend(p);
                all
            })
            .collect();
        Grid { axes, points }
    }

    /// One point: the base overrides alone.
    pub fn single(base: &[(String, Lit)]) -> Grid {
        Grid::new(vec![], base)
    }
}

/// One trial's outcome: the run, its metrics and the logged record.
pub struct Outcome {
    pub result: RunResult,
    pub metrics: ReturnMetrics,
    pub trading: TradingMetrics,
    pub trial: Trial,
}

/// One walk-forward fold: the point chosen in sample and how it did out.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct FoldReport {
    pub train: (i64, i64),
    pub test: (i64, i64),
    /// Index of the chosen grid point.
    pub best: usize,
    pub in_sample: f64,
    pub out_of_sample: f64,
    pub in_sample_cagr: f64,
    pub out_of_sample_cagr: f64,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct WalkForwardReport {
    pub scheme: WalkForward,
    pub folds: Vec<FoldReport>,
    /// Pardo's walk-forward efficiency: mean out-of-sample CAGR over mean
    /// in-sample CAGR of the chosen points (`None` when the latter is not
    /// positive).
    pub efficiency: Option<f64>,
    /// Metrics of the out-of-sample windows stitched into one curve.
    pub out_of_sample: ReturnMetrics,
}

/// What a study run reports beyond the per-point metrics.
#[derive(Clone, Debug)]
pub struct RunReport {
    /// Trials in the lineage after this run.
    pub trials: usize,
    pub dsr: DeflatedSharpe,
    pub pbo: Option<Pbo>,
    /// Index of the best point by the objective.
    pub best: usize,
    pub warnings: Vec<StudyWarning>,
    /// The bars the hold-out embargoes.
    pub embargo: Embargo,
    /// The parameter surface around the best point, on a grid.
    pub surface: Option<Surface>,
    /// The best point's annualised Sharpe ratio per calendar year.
    pub subperiods: Vec<(i64, f64)>,
    pub walk_forward: Option<WalkForwardReport>,
}

impl RunReport {
    /// The bars a trailing embargo removed, if any.
    pub fn embargoed_from(&self) -> Option<i64> {
        if self.embargo.truncates {
            self.embargo.start()
        } else {
            None
        }
    }
}

/// A run of one point with its metrics over the kept bars.
struct Evaluated {
    result: RunResult,
    curve: Vec<(i64, f64)>,
    metrics: ReturnMetrics,
    trading: TradingMetrics,
}

fn evaluate(prog: &Program, data: &Dataset, cfg: ExecConfig, runner: &Runner, keep: &dyn Fn(i64) -> bool) -> Result<Evaluated, String> {
    let ppy = periods_per_year(prog.resolution);
    let result = runner(prog, data, cfg).map_err(|e| e.to_string())?;
    let curve = masked_curve(&result.equity_curve, keep);
    let metrics = return_metrics(&curve, ppy);
    let trading = trading_metrics(&result, ppy);
    Ok(Evaluated { result, curve, metrics, trading })
}

#[allow(clippy::too_many_arguments)]
fn record(log: &TrialLog, study: &StudySpec, prog: &Program, hash: &str, cfg: &ExecConfig, ev: &Evaluated, kind: TrialKind, from: &Provenance, note: Option<String>) -> Result<Trial, String> {
    log.append(Trial {
        seq: 0,
        at: now(),
        kind,
        study: Some(study.id.clone()),
        lineage: study.lineage.clone(),
        hash: hash.to_string(),
        strategy: prog.strategy.clone(),
        params: effective_params(prog, cfg),
        period: ev.result.bars.first().zip(ev.result.bars.last()).map(|(a, b)| (format_timestamp(*a), format_timestamp(*b))),
        bars: ev.result.bars.len(),
        symbols: ev.result.symbols.len(),
        bundle: from.bundle.clone(),
        exec: cfg.clone(),
        objective: study.objective.clone(),
        scheme: from.scheme.as_ref().map(|s| s.describe()),
        holdout: study.holdout.describe(),
        metrics: ev.metrics.clone(),
        trading: ev.trading.clone(),
        note,
    })
}

fn point_cfg(study: &StudySpec, point: &Point) -> ExecConfig {
    let mut cfg = study.exec.clone();
    cfg.param_overrides.extend(point.iter().cloned());
    cfg
}

/// `study.run`: one logged trial per parameter point, on the sample the
/// hold-out leaves (a trailing embargo truncates the data, a block embargo
/// masks the metrics), then the lineage's deflated Sharpe ratio for the
/// best point, the grid's PBO and parameter surface, the best point's
/// sub-period stability, and the walk-forward folds when a scheme is given.
pub fn run_study(project: &Project, study: &StudySpec, prog: &Program, ds: &Dataset, grid: &Grid, runner: &Runner, from: Provenance) -> Result<(Vec<Outcome>, RunReport), String> {
    let hash = program_hash(prog);
    let opened = project.open_lineage(prog)?;
    if opened.lineage != study.lineage {
        return Err(format!(
            "strategy `{}` ({}) belongs to lineage {}, but study {} was declared for lineage {}; declare a study for it",
            prog.strategy, hash, opened.lineage, study.id, study.lineage
        ));
    }
    if grid.points.is_empty() {
        return Err("no parameter point to run".into());
    }
    let embargo = study.holdout.embargo(ds);
    let sample = match (embargo.truncates, embargo.start()) {
        (true, Some(h)) => ds.truncated(prog, h - 1),
        _ => ds.clone(),
    };
    let keep = |t: i64| !embargo.contains(t);
    let ppy = periods_per_year(prog.resolution);
    let log = project.log();
    let mut outcomes = Vec::new();
    let mut curves = Vec::new();
    let mut warnings = Vec::new();
    if let Some(w) = opened.warning {
        warnings.push(StudyWarning {
            bias: "data-snooping".into(),
            message: w,
        });
    }
    let masked_note = if embargo.truncates || embargo.is_empty() {
        None
    } else {
        Some(format!("metrics exclude the embargoed blocks {}", embargo.describe()))
    };
    for point in &grid.points {
        let cfg = point_cfg(study, point);
        let ev = evaluate(prog, &sample, cfg.clone(), runner, &keep)?;
        let trial = record(&log, study, prog, &hash, &cfg, &ev, TrialKind::Trial, &from, masked_note.clone())?;
        curves.push(ev.curve);
        outcomes.push(Outcome {
            result: ev.result,
            metrics: ev.metrics,
            trading: ev.trading,
            trial,
        });
    }
    let best = best_point(&outcomes, &study.objective);
    let values: Vec<f64> = outcomes.iter().map(|o| objective_value(&study.objective, &o.metrics, &o.trading).unwrap_or(f64::NAN)).collect();
    let pbo = if outcomes.len() >= 2 {
        let rets: Vec<Returns> = curves.iter().map(|c| Returns::from_curve(c, ppy)).collect();
        let n = rets.iter().map(|r| r.len()).min().unwrap_or(0);
        let rows: Vec<Vec<f64>> = (0..n).map(|i| rets.iter().map(|r| r.r[i]).collect()).collect();
        pbo_cscv(&rows, 16).or_else(|| pbo_cscv(&rows, 8))
    } else {
        None
    };
    let surface = validate::surface(&grid.axes, &values, best);
    let subperiods = validate::subperiod_sharpes(&curves[best], ppy, 20);
    let walk_forward = match &from.scheme {
        Some(scheme) => Some(walk_forward(&log, study, prog, &hash, &sample, grid, runner, &from, scheme, &keep)?),
        None => None,
    };
    let lineage_trials = log.for_lineage(&study.lineage)?;
    let var = sharpe_variance(&lineage_trials);
    let m = &outcomes[best].metrics;
    let dsr = deflated_sharpe(m.sharpe_period, m.n, m.skew, m.kurtosis, lineage_trials.len(), var);
    let years = (m.n as f64) / ppy;
    if years < 2.0 {
        warnings.push(StudyWarning {
            bias: "short-sample".into(),
            message: format!("the sample holds {:.1} years of bars; confidence in any metric is low below two", years),
        });
    }
    let mintrl = min_track_record_length(m.sharpe_period, 0.0, m.skew, m.kurtosis, 0.95);
    if mintrl.is_finite() && (m.n as f64) < mintrl {
        warnings.push(StudyWarning {
            bias: "short-sample".into(),
            message: format!(
                "{} observations, but a Sharpe ratio of {:.2} needs {:.0} to be distinguished from zero at 95 % (MinTRL)",
                m.n, m.sharpe, mintrl
            ),
        });
    }
    if dsr.trials > 1 && dsr.dsr < 0.95 {
        warnings.push(StudyWarning {
            bias: "multiple-testing".into(),
            message: format!(
                "deflated Sharpe ratio {:.2} over {} trials in the lineage (expected maximum per-period Sharpe {:.3})",
                dsr.dsr, dsr.trials, dsr.expected_max_sr
            ),
        });
    }
    if let Some(p) = &pbo {
        if p.pbo >= 0.5 {
            warnings.push(StudyWarning {
                bias: "parameter over-optimization".into(),
                message: format!("probability of backtest overfitting {:.2} over the {}-point grid", p.pbo, p.trials),
            });
        }
    }
    if let Some(s) = &surface {
        if s.stability < 0.5 {
            warnings.push(StudyWarning {
                bias: "parameter over-optimization".into(),
                message: format!(
                    "only {:.0}% of the best point's {} neighbours are within {:.3} of it: a peak, not a plateau",
                    s.stability * 100.0,
                    s.neighbours,
                    s.tolerance
                ),
            });
        }
    }
    if subperiods.len() >= 2 {
        let sign = m.sharpe >= 0.0;
        let agree = subperiods.iter().filter(|(_, s)| (*s >= 0.0) == sign).count();
        if agree * 2 < subperiods.len() {
            warnings.push(StudyWarning {
                bias: "non-stationarity".into(),
                message: format!(
                    "the Sharpe ratio has the whole sample's sign in only {} of {} calendar years ({})",
                    agree,
                    subperiods.len(),
                    subperiods.iter().map(|(y, s)| format!("{} {:.2}", y, s)).collect::<Vec<_>>().join(", ")
                ),
            });
        }
    }
    if let Some(wf) = &walk_forward {
        match wf.efficiency {
            Some(e) if e < 0.5 => warnings.push(StudyWarning {
                bias: "walk-forward".into(),
                message: format!("walk-forward efficiency {:.2}: out of sample the chosen points earn less than half their in-sample rate", e),
            }),
            None => warnings.push(StudyWarning {
                bias: "walk-forward".into(),
                message: "walk-forward efficiency is undefined: the chosen points did not earn in sample".into(),
            }),
            _ => {}
        }
    }
    for w in &outcomes[best].result.warnings {
        warnings.push(StudyWarning {
            bias: w.bias.clone(),
            message: w.message.clone(),
        });
    }
    Ok((
        outcomes,
        RunReport {
            trials: lineage_trials.len(),
            dsr,
            pbo,
            best,
            warnings,
            embargo,
            surface,
            subperiods,
            walk_forward,
        },
    ))
}

/// Walk forward over the sample: in each fold every grid point is run on
/// the data up to the test window and judged on the train window (each a
/// logged trial), the best is judged on the test window (a logged trial),
/// and the test windows are stitched into one out-of-sample curve.
#[allow(clippy::too_many_arguments)]
fn walk_forward(
    log: &TrialLog,
    study: &StudySpec,
    prog: &Program,
    hash: &str,
    sample: &Dataset,
    grid: &Grid,
    runner: &Runner,
    from: &Provenance,
    scheme: &WalkForward,
    keep: &dyn Fn(i64) -> bool,
) -> Result<WalkForwardReport, String> {
    let bars: Vec<i64> = sample.facts.values().flat_map(|tus| tus.iter()).filter_map(|tu| tu.iter().find_map(|v| v.as_time())).collect();
    let (Some(first), Some(last)) = (bars.iter().min().copied(), bars.iter().max().copied()) else {
        return Err("the sample has no bars".into());
    };
    let folds = scheme.folds(first, last);
    if folds.is_empty() {
        return Err(format!(
            "the sample ({} to {}) is shorter than one train and one test window of {}",
            format_timestamp(first),
            format_timestamp(last),
            scheme.describe()
        ));
    }
    let ppy = periods_per_year(prog.resolution);
    let mut reports = Vec::new();
    let mut stitched: Vec<(i64, f64)> = Vec::new();
    let mut level = 1.0;
    for (k, &(train_from, train_to, test_from, test_to)) in folds.iter().enumerate() {
        let train_data = sample.truncated(prog, test_from - 1);
        let mut best: Option<(usize, f64, f64)> = None;
        for (i, point) in grid.points.iter().enumerate() {
            let cfg = point_cfg(study, point);
            let in_train = |t: i64| keep(t) && train_from <= t && t < train_to;
            let ev = evaluate(prog, &train_data, cfg.clone(), runner, &in_train)?;
            record(
                log,
                study,
                prog,
                hash,
                &cfg,
                &ev,
                TrialKind::Trial,
                from,
                Some(format!("walk-forward fold {}/{} train", k + 1, folds.len())),
            )?;
            let v = objective_value(&study.objective, &ev.metrics, &ev.trading).unwrap_or(f64::NAN);
            let better = higher_is_better(&study.objective);
            if v.is_finite() && best.map(|(_, b, _)| if better { v > b } else { v < b }).unwrap_or(true) {
                best = Some((i, v, ev.metrics.cagr));
            }
        }
        let Some((bi, is_v, is_cagr)) = best else { continue };
        let cfg = point_cfg(study, &grid.points[bi]);
        let test_data = sample.truncated(prog, test_to - 1);
        let in_test = |t: i64| keep(t) && test_from <= t && t < test_to;
        let ev = evaluate(prog, &test_data, cfg.clone(), runner, &in_test)?;
        record(
            log,
            study,
            prog,
            hash,
            &cfg,
            &ev,
            TrialKind::Trial,
            from,
            Some(format!("walk-forward fold {}/{} test", k + 1, folds.len())),
        )?;
        let oos_v = objective_value(&study.objective, &ev.metrics, &ev.trading).unwrap_or(f64::NAN);
        for w in ev.curve.windows(2) {
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
            best: bi,
            in_sample: is_v,
            out_of_sample: oos_v,
            in_sample_cagr: is_cagr,
            out_of_sample_cagr: ev.metrics.cagr,
        });
    }
    let n = reports.len().max(1) as f64;
    let is_mean = reports.iter().map(|f| f.in_sample_cagr).sum::<f64>() / n;
    let oos_mean = reports.iter().map(|f| f.out_of_sample_cagr).sum::<f64>() / n;
    let efficiency = if is_mean > 0.0 { Some(oos_mean / is_mean) } else { None };
    Ok(WalkForwardReport {
        scheme: scheme.clone(),
        folds: reports,
        efficiency,
        out_of_sample: return_metrics(&stitched, ppy),
    })
}

/// `holdout.reveal`: the committed program (a member of the study's
/// lineage) runs over the whole sample and reports the embargoed bars
/// only; logged as an out-of-sample trial, counted like any other.
pub fn reveal(project: &Project, study: &StudySpec, prog: &Program, ds: &Dataset, runner: &Runner, from: Provenance) -> Result<(Outcome, Embargo), String> {
    let hash = program_hash(prog);
    let ls = project.lineages()?;
    match ls.lineage_of(&hash) {
        Some(l) if l.id == study.lineage => {}
        Some(l) => return Err(format!("strategy `{}` ({}) belongs to lineage {}, not the study's {}", prog.strategy, hash, l.id, study.lineage)),
        None => {
            return Err(format!(
                "strategy `{}` ({}) is not a member of lineage {}: a reveal names a committed version, one the study has run",
                prog.strategy, hash, study.lineage
            ))
        }
    }
    let embargo = study.holdout.embargo(ds);
    if embargo.is_empty() {
        return Err(format!("study {} declares no hold-out; there is nothing to reveal", study.id));
    }
    let cfg = study.exec.clone();
    let ev = evaluate(prog, ds, cfg.clone(), runner, &|t| embargo.contains(t))?;
    let log = project.log();
    let trial = record(
        &log,
        study,
        prog,
        &hash,
        &cfg,
        &ev,
        TrialKind::Reveal,
        &from,
        Some(format!("reveal of {} over {}", hash, embargo.describe())),
    )?;
    Ok((
        Outcome {
            result: ev.result,
            metrics: ev.metrics,
            trading: ev.trading,
            trial,
        },
        embargo,
    ))
}

/// The index of the outcome with the best objective.
pub fn best_point(outcomes: &[Outcome], objective: &str) -> usize {
    let better = higher_is_better(objective);
    let mut best = 0;
    for (i, o) in outcomes.iter().enumerate() {
        let v = objective_value(objective, &o.metrics, &o.trading).unwrap_or(f64::NAN);
        let b = objective_value(objective, &outcomes[best].metrics, &outcomes[best].trading).unwrap_or(f64::NAN);
        if v.is_nan() {
            continue;
        }
        if b.is_nan() || (better && v > b) || (!better && v < b) {
            best = i;
        }
    }
    best
}

/// Log a plain `abt run` as an untracked trial (section 7: allowed, logged,
/// warned).
pub fn log_untracked(project: &Project, prog: &Program, result: &RunResult, cfg: &ExecConfig, bundle: Option<String>) -> Result<Trial, String> {
    let opened = project.open_lineage(prog)?;
    let ppy = periods_per_year(prog.resolution);
    project.log().append(Trial {
        seq: 0,
        at: now(),
        kind: TrialKind::Untracked,
        study: None,
        lineage: opened.lineage,
        hash: opened.hash,
        strategy: prog.strategy.clone(),
        params: effective_params(prog, cfg),
        period: result.bars.first().zip(result.bars.last()).map(|(a, b)| (format_timestamp(*a), format_timestamp(*b))),
        bars: result.bars.len(),
        symbols: result.symbols.len(),
        bundle,
        exec: cfg.clone(),
        objective: "sharpe".into(),
        scheme: None,
        holdout: "none".into(),
        metrics: return_metrics(&result.equity_curve, ppy),
        trading: trading_metrics(result, ppy),
        note: Some("untracked: run outside a study".into()),
    })
}
