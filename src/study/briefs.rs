//! The LLM-authored corpus harness (data-bundle doc, section 8): briefs in
//! plain language are given to a model, its successive attempts are kept
//! as files, and this records what happened: whether the first attempt
//! checked clean, the diagnostics of every attempt, the attempts to a
//! valid program, and how the first valid one fares on the synthetic
//! market under the default cost model. Measured and stored, never
//! asserted: a brief whose honest verdict is "does not survive costs" is
//! the system working.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::check::{check_program, Severity, Workspace};
use crate::data::{synthetic_daily, synthetic_daily_v2, synthetic_minute};
use crate::ir::UnitKind;
use crate::kernel::{run, ExecConfig, RunError};

use super::metrics::{periods_per_year, return_metrics};

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct AttemptRecord {
    pub file: String,
    pub strategy: Option<String>,
    /// Parse error text, when the attempt did not parse.
    pub parse_error: Option<String>,
    /// `(code, judgment, message)` of every diagnostic.
    pub diagnostics: Vec<(String, String, String)>,
    pub errors: usize,
    pub warnings: usize,
    pub valid: bool,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Verdict {
    pub strategy: String,
    pub bars: usize,
    pub decisions: usize,
    pub fills: usize,
    pub sharpe: f64,
    pub cagr: f64,
    pub max_drawdown: f64,
    pub degrees_of_freedom: String,
    /// `trades`, `does not trade`, `does not survive costs` or `halted: ...`.
    pub verdict: String,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct BriefRecord {
    pub brief: String,
    pub text: String,
    pub attempts: Vec<AttemptRecord>,
    pub first_attempt_valid: bool,
    /// 1-based index of the first valid attempt, if any.
    pub attempts_to_valid: Option<usize>,
    pub verdict: Option<Verdict>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct BriefsReport {
    pub briefs: Vec<BriefRecord>,
    pub first_attempt_pass_rate: f64,
    /// Diagnostic code -> occurrences across every attempt.
    pub diagnostics: BTreeMap<String, usize>,
    pub mean_attempts_to_valid: Option<f64>,
    pub briefs_without_a_valid_attempt: usize,
    pub verdicts: BTreeMap<String, usize>,
}

fn sorted_files(dir: &Path, ext: &str) -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = std::fs::read_dir(dir)
        .map(|rd| rd.filter_map(|e| e.ok().map(|e| e.path())).filter(|p| p.extension().map(|x| x == ext).unwrap_or(false)).collect())
        .unwrap_or_default();
    // Attempts are numbered: order numerically when the stems are numbers.
    files.sort_by_key(|p| {
        let stem = p.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
        (stem.parse::<u64>().unwrap_or(u64::MAX), stem)
    });
    files
}

/// Check one attempt against the workspace (the corpus environments and
/// libraries), and run the first valid one.
fn attempt(ws: &Workspace, path: &Path) -> (AttemptRecord, Option<Verdict>) {
    let file = path.file_name().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
    let src = match std::fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) => {
            return (
                AttemptRecord {
                    file,
                    strategy: None,
                    parse_error: Some(e.to_string()),
                    diagnostics: vec![],
                    errors: 1,
                    warnings: 0,
                    valid: false,
                },
                None,
            )
        }
    };
    let mut ws = Workspace { units: ws.units.clone() };
    if let Err(e) = ws.add_source(&src) {
        return (
            AttemptRecord {
                file,
                strategy: None,
                parse_error: Some(format!("{}: {}", e.span, e.message)),
                diagnostics: vec![],
                errors: 1,
                warnings: 0,
                valid: false,
            },
            None,
        );
    }
    let Some(name) = ws.units.iter().rev().find(|u| u.kind == UnitKind::Strategy).map(|u| u.name.clone()) else {
        return (
            AttemptRecord {
                file,
                strategy: None,
                parse_error: Some("no strategy unit in the attempt".into()),
                diagnostics: vec![],
                errors: 1,
                warnings: 0,
                valid: false,
            },
            None,
        );
    };
    let (prog, diags) = check_program(&ws, &name);
    let record = AttemptRecord {
        file,
        strategy: Some(name.clone()),
        parse_error: None,
        diagnostics: diags.iter().map(|d| (format!("{:?}", d.code), d.code.judgment().to_string(), d.message.clone())).collect(),
        errors: diags.iter().filter(|d| d.severity == Severity::Error).count(),
        warnings: diags.iter().filter(|d| d.severity == Severity::Warning).count(),
        valid: prog.is_some(),
    };
    let Some(prog) = prog else { return (record, None) };
    let syms = ["AAA", "BBB", "CCC", "DDD", "SPY"];
    let minute = prog.relations.values().any(|s| s.res == Some(crate::ir::Resolution::M1));
    let ds = if minute {
        synthetic_minute(&syms, (2024, 1, 2), 30, 390, 11)
    } else if prog.relations.contains_key("split") {
        synthetic_daily_v2(&syms, (2022, 1, 3), 320, 11)
    } else {
        synthetic_daily(&syms, (2022, 1, 3), 320, 11)
    };
    let dof = prog.degrees_of_freedom.summary();
    let verdict = match run(&prog, &ds, ExecConfig::default()) {
        Err(RunError::Risk { message, .. }) => Verdict {
            strategy: name,
            bars: 0,
            decisions: 0,
            fills: 0,
            sharpe: 0.0,
            cagr: 0.0,
            max_drawdown: 0.0,
            degrees_of_freedom: dof,
            verdict: format!("halted: {}", message),
        },
        Err(e) => Verdict {
            strategy: name,
            bars: 0,
            decisions: 0,
            fills: 0,
            sharpe: 0.0,
            cagr: 0.0,
            max_drawdown: 0.0,
            degrees_of_freedom: dof,
            verdict: format!("failed: {}", e),
        },
        Ok(r) => {
            let m = return_metrics(&r.metric_curve(), periods_per_year(prog.resolution));
            let verdict = if r.fills.is_empty() {
                "does not trade"
            } else if m.cagr <= 0.0 {
                "does not survive costs"
            } else {
                "trades"
            };
            Verdict {
                strategy: name,
                bars: r.bars.len(),
                decisions: r.decisions.len(),
                fills: r.fills.len(),
                sharpe: m.sharpe,
                cagr: m.cagr,
                max_drawdown: m.max_drawdown,
                degrees_of_freedom: dof,
                verdict: verdict.into(),
            }
        }
    };
    (record, Some(verdict))
}

/// Every brief (`*.md` except the README) with its attempts
/// (`attempts/<brief>/*.dsl`, numbered), checked and the first valid one
/// run.
pub fn report(ws: &Workspace, briefs_dir: &Path, attempts_dir: &Path) -> Result<BriefsReport, String> {
    if !briefs_dir.is_dir() {
        return Err(format!("{}: not a directory of briefs", briefs_dir.display()));
    }
    let mut briefs = Vec::new();
    let mut diagnostics: BTreeMap<String, usize> = BTreeMap::new();
    let mut verdicts: BTreeMap<String, usize> = BTreeMap::new();
    for path in sorted_files(briefs_dir, "md") {
        let stem = path.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
        if stem.eq_ignore_ascii_case("readme") {
            continue;
        }
        let text = std::fs::read_to_string(&path).map_err(|e| format!("{}: {}", path.display(), e))?;
        let mut attempts = Vec::new();
        let mut verdict = None;
        let mut attempts_to_valid = None;
        for (i, file) in sorted_files(&attempts_dir.join(&stem), "dsl").iter().enumerate() {
            let (record, v) = attempt(ws, file);
            for (code, ..) in &record.diagnostics {
                *diagnostics.entry(code.clone()).or_insert(0) += 1;
            }
            if record.parse_error.is_some() {
                *diagnostics.entry("parse".into()).or_insert(0) += 1;
            }
            if record.valid && attempts_to_valid.is_none() {
                attempts_to_valid = Some(i + 1);
                verdict = v;
            }
            attempts.push(record);
        }
        if let Some(v) = &verdict {
            let key = v.verdict.split(':').next().unwrap_or("").to_string();
            *verdicts.entry(key).or_insert(0) += 1;
        }
        briefs.push(BriefRecord {
            brief: stem,
            text: text.trim().to_string(),
            first_attempt_valid: attempts.first().map(|a| a.valid).unwrap_or(false),
            attempts,
            attempts_to_valid,
            verdict,
        });
    }
    let with_attempts: Vec<&BriefRecord> = briefs.iter().filter(|b| !b.attempts.is_empty()).collect();
    let first_attempt_pass_rate = if with_attempts.is_empty() {
        0.0
    } else {
        with_attempts.iter().filter(|b| b.first_attempt_valid).count() as f64 / with_attempts.len() as f64
    };
    let valid: Vec<usize> = briefs.iter().filter_map(|b| b.attempts_to_valid).collect();
    let mean_attempts_to_valid = if valid.is_empty() { None } else { Some(valid.iter().sum::<usize>() as f64 / valid.len() as f64) };
    Ok(BriefsReport {
        briefs_without_a_valid_attempt: with_attempts.iter().filter(|b| b.attempts_to_valid.is_none()).count(),
        briefs,
        first_attempt_pass_rate,
        diagnostics,
        mean_attempts_to_valid,
        verdicts,
    })
}

impl BriefsReport {
    pub fn render(&self) -> String {
        let mut out = String::new();
        let mut line = |s: String| {
            out.push_str(&s);
            out.push('\n');
        };
        let attempted = self.briefs.iter().filter(|b| !b.attempts.is_empty()).count();
        line(format!("briefs: {} ({} with attempts)", self.briefs.len(), attempted));
        for b in &self.briefs {
            let first = b.text.lines().next().unwrap_or("").to_string();
            line(format!("  {}: {}", b.brief, first));
            if b.attempts.is_empty() {
                line("    no attempts".into());
                continue;
            }
            for (i, a) in b.attempts.iter().enumerate() {
                let codes: Vec<String> = a.diagnostics.iter().map(|(c, ..)| c.clone()).collect();
                line(format!(
                    "    attempt {} ({}): {}{}",
                    i + 1,
                    a.file,
                    if a.valid { "valid" } else { "invalid" },
                    match (&a.parse_error, codes.is_empty()) {
                        (Some(e), _) => format!("; parse error: {}", e),
                        (None, false) => format!("; {}", codes.join(" ")),
                        (None, true) => String::new(),
                    }
                ));
            }
            if let Some(v) = &b.verdict {
                line(format!(
                    "    verdict: {} ({} decisions, {} fills, sharpe {:.3}, cagr {:.4}, max drawdown {:.4}; {})",
                    v.verdict, v.decisions, v.fills, v.sharpe, v.cagr, v.max_drawdown, v.degrees_of_freedom
                ));
            }
        }
        line(format!("first-attempt pass rate: {:.0}%", self.first_attempt_pass_rate * 100.0));
        line(format!(
            "attempts to a valid program: {}; briefs without one: {}",
            self.mean_attempts_to_valid.map(|m| format!("{:.2} on average", m)).unwrap_or_else(|| "none valid".into()),
            self.briefs_without_a_valid_attempt
        ));
        line(format!("diagnostics: {}", self.diagnostics.iter().map(|(c, n)| format!("{} x{}", c, n)).collect::<Vec<_>>().join(", ")));
        line(format!("verdicts: {}", self.verdicts.iter().map(|(v, n)| format!("{} x{}", v, n)).collect::<Vec<_>>().join(", ")));
        out
    }
}
