//! The trial log (data-bundle doc, section 6): every run is a logged trial
//! against its lineage, with the program hash, the parameters, the period,
//! the universe, the executor configuration, the objective and the metrics,
//! appended as one JSON line to `trials.jsonl` in the study directory. The
//! log is append-only; nothing here deletes or rewrites a line.

use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};

use crate::kernel::ExecConfig;

use super::metrics::{ReturnMetrics, TradingMetrics};

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum TrialKind {
    /// A run inside a study, counted against the lineage.
    Trial,
    /// A hold-out reveal: an out-of-sample trial.
    Reveal,
    /// A plain `abt run` that named the study directory: logged, warned,
    /// and counted like any other trial.
    Untracked,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct Trial {
    /// Position in the log, from 1.
    pub seq: usize,
    /// Unix seconds.
    pub at: u64,
    pub kind: TrialKind,
    pub study: Option<String>,
    pub lineage: String,
    pub hash: String,
    pub strategy: String,
    /// Every parameter of the strategy with its effective value.
    pub params: BTreeMap<String, String>,
    /// First and last bar of the run.
    pub period: Option<(String, String)>,
    pub bars: usize,
    pub symbols: usize,
    /// `name@version` when the data came from a bundle.
    pub bundle: Option<String>,
    pub exec: ExecConfig,
    pub objective: String,
    /// The walk-forward scheme, when one was declared.
    pub scheme: Option<String>,
    pub holdout: String,
    pub metrics: ReturnMetrics,
    pub trading: TradingMetrics,
    pub note: Option<String>,
    /// The warnings the run raised (the executor's, with their biases).
    #[serde(default)]
    pub warnings: Vec<super::StudyWarning>,
}

/// The append-only trial log of a study directory.
pub struct TrialLog {
    pub path: PathBuf,
}

impl TrialLog {
    pub fn in_dir(dir: &Path) -> TrialLog {
        TrialLog { path: dir.join("trials.jsonl") }
    }

    pub fn read_all(&self) -> Result<Vec<Trial>, String> {
        if !self.path.exists() {
            return Ok(vec![]);
        }
        let text = std::fs::read_to_string(&self.path).map_err(|e| format!("{}: {}", self.path.display(), e))?;
        text.lines()
            .filter(|l| !l.trim().is_empty())
            .enumerate()
            .map(|(i, l)| serde_json::from_str(l).map_err(|e| format!("{} line {}: {}", self.path.display(), i + 1, e)))
            .collect()
    }

    /// Append a trial, assigning its sequence number; returns it.
    pub fn append(&self, mut trial: Trial) -> Result<Trial, String> {
        if let Some(dir) = self.path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| format!("{}: {}", dir.display(), e))?;
        }
        trial.seq = self.read_all()?.len() + 1;
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
            .map_err(|e| format!("{}: {}", self.path.display(), e))?;
        let line = serde_json::to_string(&trial).map_err(|e| e.to_string())?;
        writeln!(f, "{}", line).map_err(|e| format!("{}: {}", self.path.display(), e))?;
        Ok(trial)
    }

    /// The trials of a lineage, in order.
    pub fn for_lineage(&self, lineage: &str) -> Result<Vec<Trial>, String> {
        Ok(self.read_all()?.into_iter().filter(|t| t.lineage == lineage).collect())
    }
}

/// Sample variance of per-period Sharpe ratios across trials (zero below
/// two trials): what the deflated Sharpe ratio deflates by.
pub fn sharpe_variance(trials: &[Trial]) -> f64 {
    let x: Vec<f64> = trials.iter().map(|t| t.metrics.sharpe_period).collect();
    if x.len() < 2 {
        return 0.0;
    }
    let m = x.iter().sum::<f64>() / x.len() as f64;
    x.iter().map(|v| (v - m).powi(2)).sum::<f64>() / (x.len() as f64 - 1.0)
}

/// One `study run` or `reveal` invocation: the trials it logged and what
/// it reported beyond them, appended to `runs.jsonl`.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct RunRecord {
    pub seq: usize,
    pub at: u64,
    /// `run` or `reveal`.
    pub kind: String,
    pub study: String,
    pub lineage: String,
    pub trials: Vec<usize>,
    pub best_trial: Option<usize>,
    pub dsr: Option<super::DeflatedSharpe>,
    pub pbo: Option<super::Pbo>,
    pub surface: Option<super::Surface>,
    pub scheme: Option<String>,
    pub efficiency: Option<f64>,
    pub warnings: Vec<super::StudyWarning>,
}

/// The append-only run log of a study directory.
pub struct RunLog {
    pub path: PathBuf,
}

impl RunLog {
    pub fn in_dir(dir: &Path) -> RunLog {
        RunLog { path: dir.join("runs.jsonl") }
    }

    pub fn read_all(&self) -> Result<Vec<RunRecord>, String> {
        if !self.path.exists() {
            return Ok(vec![]);
        }
        let text = std::fs::read_to_string(&self.path).map_err(|e| format!("{}: {}", self.path.display(), e))?;
        text.lines()
            .filter(|l| !l.trim().is_empty())
            .enumerate()
            .map(|(i, l)| serde_json::from_str(l).map_err(|e| format!("{} line {}: {}", self.path.display(), i + 1, e)))
            .collect()
    }

    pub fn append(&self, mut record: RunRecord) -> Result<RunRecord, String> {
        if let Some(dir) = self.path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| format!("{}: {}", dir.display(), e))?;
        }
        record.seq = self.read_all()?.len() + 1;
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
            .map_err(|e| format!("{}: {}", self.path.display(), e))?;
        let line = serde_json::to_string(&record).map_err(|e| e.to_string())?;
        writeln!(f, "{}", line).map_err(|e| format!("{}: {}", self.path.display(), e))?;
        Ok(record)
    }
}
