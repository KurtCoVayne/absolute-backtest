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

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::check::Program;
use crate::ir::{Duration, Lit};
use crate::kernel::time::{self, format_timestamp};
use crate::kernel::{Dataset, ExecConfig, RunError, RunResult};

pub use lineage::{jaccard, normalized_rules, program_hash, Attachment, Lineage, Lineages, Opened, SIMILARITY_THRESHOLD};
pub use log::{sharpe_variance, Trial, TrialKind, TrialLog};
pub use metrics::{
    block_bootstrap_sharpe, capacity, deflated_sharpe, min_track_record_length, newey_west_t, pbo_cscv, periods_per_year, probabilistic_sharpe, return_metrics, trading_metrics, DeflatedSharpe, Pbo,
    ReturnMetrics, Returns, TradingMetrics,
};

/// The hold-out policy a study declares (section 6, out-of-sample).
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum Holdout {
    /// No hold-out: every run sees the whole sample (warned).
    None,
    /// The last `years` of the sample are embargoed: a run is truncated
    /// before them until a committed version is revealed.
    Trailing { years: u32 },
}

impl Holdout {
    /// `none` or `trailing:Ny`.
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
        Err(format!("`{}` is not a hold-out policy: none or trailing:Ny", s))
    }

    pub fn describe(&self) -> String {
        match self {
            Holdout::None => "none".into(),
            Holdout::Trailing { years } => format!("trailing:{}y", years),
        }
    }

    /// The first embargoed bar of a dataset, if any.
    pub fn embargo_start(&self, ds: &Dataset) -> Option<i64> {
        match self {
            Holdout::None => None,
            Holdout::Trailing { years } => {
                let last = ds.facts.values().flat_map(|tus| tus.iter()).filter_map(|tu| tu.iter().find_map(|v| v.as_time())).max()?;
                Some(time::sub_duration(last, Duration { months: 12 * *years as i64, days: 0 }))
            }
        }
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
    pub scheme: Option<String>,
}

/// One trial's outcome: the run, its metrics and the logged record.
pub struct Outcome {
    pub result: RunResult,
    pub metrics: ReturnMetrics,
    pub trading: TradingMetrics,
    pub trial: Trial,
}

/// What a study run reports beyond the per-point metrics.
#[derive(Clone, Debug, PartialEq)]
pub struct RunReport {
    /// Trials in the lineage after this run.
    pub trials: usize,
    pub dsr: DeflatedSharpe,
    pub pbo: Option<Pbo>,
    /// Index of the best point by the objective.
    pub best: usize,
    pub warnings: Vec<StudyWarning>,
    /// The bars the hold-out embargo removed, if any.
    pub embargoed_from: Option<i64>,
}

/// `study.run`: one logged trial per parameter point, on the sample the
/// hold-out leaves, then the lineage's deflated Sharpe ratio for the best
/// point and the grid's PBO.
pub fn run_study(project: &Project, study: &StudySpec, prog: &Program, ds: &Dataset, points: &[Point], runner: &Runner, from: Provenance) -> Result<(Vec<Outcome>, RunReport), String> {
    let hash = program_hash(prog);
    let opened = project.open_lineage(prog)?;
    if opened.lineage != study.lineage {
        return Err(format!(
            "strategy `{}` ({}) belongs to lineage {}, but study {} was declared for lineage {}; declare a study for it",
            prog.strategy, hash, opened.lineage, study.id, study.lineage
        ));
    }
    let embargoed_from = study.holdout.embargo_start(ds);
    let sample = match embargoed_from {
        Some(h) => ds.truncated(prog, h - 1),
        None => ds.clone(),
    };
    let ppy = periods_per_year(prog.resolution);
    let log = project.log();
    let mut outcomes = Vec::new();
    let mut warnings = Vec::new();
    if let Some(w) = opened.warning {
        warnings.push(StudyWarning {
            bias: "data-snooping".into(),
            message: w,
        });
    }
    for point in points {
        let mut cfg = study.exec.clone();
        cfg.param_overrides.extend(point.iter().cloned());
        let result = runner(prog, &sample, cfg.clone()).map_err(|e| e.to_string())?;
        let metrics = return_metrics(&result.equity_curve, ppy);
        let trading = trading_metrics(&result, ppy);
        let trial = log.append(Trial {
            seq: 0,
            at: now(),
            kind: TrialKind::Trial,
            study: Some(study.id.clone()),
            lineage: study.lineage.clone(),
            hash: hash.clone(),
            strategy: prog.strategy.clone(),
            params: effective_params(prog, &cfg),
            period: result.bars.first().zip(result.bars.last()).map(|(a, b)| (format_timestamp(*a), format_timestamp(*b))),
            bars: result.bars.len(),
            symbols: result.symbols.len(),
            bundle: from.bundle.clone(),
            exec: cfg,
            objective: study.objective.clone(),
            scheme: from.scheme.clone(),
            holdout: study.holdout.describe(),
            metrics: metrics.clone(),
            trading: trading.clone(),
            note: None,
        })?;
        outcomes.push(Outcome { result, metrics, trading, trial });
    }
    if outcomes.is_empty() {
        return Err("no parameter point to run".into());
    }
    let best = best_point(&outcomes, &study.objective);
    let lineage_trials = log.for_lineage(&study.lineage)?;
    let var = sharpe_variance(&lineage_trials);
    let m = &outcomes[best].metrics;
    let dsr = deflated_sharpe(m.sharpe_period, m.n, m.skew, m.kurtosis, lineage_trials.len(), var);
    let pbo = if outcomes.len() >= 2 {
        let rets: Vec<Returns> = outcomes.iter().map(|o| Returns::from_curve(&o.result.equity_curve, ppy)).collect();
        let n = rets.iter().map(|r| r.len()).min().unwrap_or(0);
        let rows: Vec<Vec<f64>> = (0..n).map(|i| rets.iter().map(|r| r.r[i]).collect()).collect();
        pbo_cscv(&rows, 16).or_else(|| pbo_cscv(&rows, 8))
    } else {
        None
    };
    let years = (outcomes[best].result.bars.len() as f64) / ppy;
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
            embargoed_from,
        },
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
