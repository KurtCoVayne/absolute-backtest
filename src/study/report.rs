//! The study report (data-bundle doc, section 7, `study.report`): every
//! warning raised against a lineage, its trial count and reveals, the
//! author's thresholds checked, the degrees of freedom, and the bias audit
//! of sections 4 to 6 with the rows a warning touched marked, so that the
//! report reads as a bias checklist with the author's answers.

use std::collections::BTreeMap;

use crate::check::{Code, Diagnostic, Program, Severity};

use super::lineage::{Attachment, Lineage};
use super::log::{Trial, TrialKind};
use super::{objective_value, program_hash, Project, RunRecord, StudySpec, StudyWarning};

/// One row of the bias audit, from the document's tables.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Bias {
    pub name: &'static str,
    /// A: information and time; B: execution realism; C: research process.
    pub audit: char,
    pub status: &'static str,
}

/// The forty biases of sections 4 to 6, with their status column.
pub const BIASES: [Bias; 40] = [
    Bias {
        name: "look-ahead",
        audit: 'A',
        status: "guaranteed (program); data-guaranteed with availability keys",
    },
    Bias {
        name: "point-in-time",
        audit: 'A',
        status: "data-guaranteed",
    },
    Bias {
        name: "historical revision",
        audit: 'A',
        status: "data-guaranteed",
    },
    Bias {
        name: "corporate-action",
        audit: 'A',
        status: "guaranteed by design, verified by the reference engine",
    },
    Bias {
        name: "survivorship",
        audit: 'A',
        status: "data-guaranteed",
    },
    Bias {
        name: "universe leakage",
        audit: 'A',
        status: "guaranteed",
    },
    Bias {
        name: "feature leakage",
        audit: 'A',
        status: "guaranteed",
    },
    Bias {
        name: "label leakage",
        audit: 'A',
        status: "guaranteed; not applicable until v3",
    },
    Bias {
        name: "delisting",
        audit: 'A',
        status: "modeled; warned when the haircut is set to zero",
    },
    Bias {
        name: "selection",
        audit: 'A',
        status: "warned, measured",
    },
    Bias {
        name: "regime",
        audit: 'A',
        status: "measured, warned",
    },
    Bias {
        name: "transaction-cost neglect",
        audit: 'B',
        status: "modeled, warned",
    },
    Bias {
        name: "slippage",
        audit: 'B',
        status: "modeled",
    },
    Bias {
        name: "market-impact",
        audit: 'B',
        status: "modeled, warned",
    },
    Bias {
        name: "liquidity",
        audit: 'B',
        status: "modeled, measured",
    },
    Bias {
        name: "bid-ask spread",
        audit: 'B',
        status: "modeled (v1 proxy); data (target)",
    },
    Bias {
        name: "execution timing",
        audit: 'B',
        status: "guaranteed",
    },
    Bias {
        name: "bar-resolution",
        audit: 'B',
        status: "guaranteed honest; warned",
    },
    Bias {
        name: "partial-fill",
        audit: 'B',
        status: "modeled",
    },
    Bias {
        name: "latency",
        audit: 'B',
        status: "modeled; conservative by contract",
    },
    Bias {
        name: "rebalancing",
        audit: 'B',
        status: "modeled, measured",
    },
    Bias {
        name: "cash-management",
        audit: 'B',
        status: "modeled (v2); warned (v1)",
    },
    Bias {
        name: "leverage",
        audit: 'B',
        status: "guaranteed",
    },
    Bias {
        name: "funding-cost neglect",
        audit: 'B',
        status: "modeled, warned",
    },
    Bias {
        name: "shorting",
        audit: 'B',
        status: "modeled (proxy), warned",
    },
    Bias {
        name: "borrow-availability",
        audit: 'B',
        status: "proxy; open",
    },
    Bias {
        name: "currency",
        audit: 'B',
        status: "guaranteed by types",
    },
    Bias {
        name: "portfolio-construction",
        audit: 'B',
        status: "guaranteed (causality); modeled (feasibility)",
    },
    Bias {
        name: "data-snooping",
        audit: 'C',
        status: "measured; cannot be hidden",
    },
    Bias {
        name: "multiple-testing",
        audit: 'C',
        status: "measured",
    },
    Bias {
        name: "p-hacking",
        audit: 'C',
        status: "measured",
    },
    Bias {
        name: "false statistical significance",
        audit: 'C',
        status: "measured",
    },
    Bias {
        name: "overfitting",
        audit: 'C',
        status: "measured, warned",
    },
    Bias {
        name: "parameter over-optimization",
        audit: 'C',
        status: "measured",
    },
    Bias {
        name: "out-of-sample",
        audit: 'C',
        status: "measured; enforced once declared; warned when absent",
    },
    Bias {
        name: "walk-forward",
        audit: 'C',
        status: "measured",
    },
    Bias {
        name: "autocorrelation",
        audit: 'C',
        status: "measured",
    },
    Bias {
        name: "non-stationarity",
        audit: 'C',
        status: "measured, warned",
    },
    Bias {
        name: "short-sample",
        audit: 'C',
        status: "measured, warned",
    },
    Bias {
        name: "tail-risk neglect",
        audit: 'C',
        status: "measured, warned",
    },
];

/// A warning as the report shows it: once, with how often it was raised.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct RaisedWarning {
    pub bias: String,
    pub message: String,
    pub times: usize,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct BiasRow {
    pub name: String,
    pub audit: char,
    pub status: String,
    /// Distinct warnings raised under this bias.
    pub raised: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ThresholdCheck {
    pub threshold: String,
    pub on_latest_trial: Option<bool>,
    pub on_latest_reveal: Option<bool>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct TrialSummary {
    pub seq: usize,
    pub kind: TrialKind,
    pub strategy: String,
    pub hash: String,
    pub note: Option<String>,
    pub bars: usize,
    pub sharpe: f64,
    pub sharpe_se: f64,
    pub cagr: f64,
    pub max_drawdown: f64,
    pub cvar_5: f64,
    pub objective: String,
    pub objective_value: Option<f64>,
}

impl TrialSummary {
    fn of(t: &Trial) -> TrialSummary {
        TrialSummary {
            seq: t.seq,
            kind: t.kind,
            strategy: t.strategy.clone(),
            hash: t.hash.clone(),
            note: t.note.clone(),
            bars: t.bars,
            sharpe: t.metrics.sharpe,
            sharpe_se: t.metrics.sharpe_se,
            cagr: t.metrics.cagr,
            max_drawdown: t.metrics.max_drawdown,
            cvar_5: t.metrics.cvar_5,
            objective: t.objective.clone(),
            objective_value: objective_value(&t.objective, &t.metrics, &t.trading),
        }
    }
}

/// The report of a lineage.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Report {
    pub strategy: String,
    pub hash: String,
    pub lineage: String,
    /// `(hash, strategy, attachment)` of every member.
    pub members: Vec<(String, String, String)>,
    pub disputes: usize,
    pub studies: Vec<String>,
    pub trials: usize,
    pub study_trials: usize,
    pub untracked: usize,
    pub reveals: Vec<TrialSummary>,
    pub runs: usize,
    pub latest: Option<TrialSummary>,
    /// The deflated Sharpe ratio of the latest study run, if any.
    pub dsr: Option<f64>,
    pub degrees_of_freedom: String,
    pub thresholds: Vec<ThresholdCheck>,
    pub warnings: Vec<RaisedWarning>,
    pub biases: Vec<BiasRow>,
}

fn attachment_text(a: &Attachment) -> String {
    match a {
        Attachment::Root => "root".into(),
        Attachment::Revises(h) => format!("revises {}", h),
        Attachment::Similarity { to, score } => format!("by similarity {:.2} with {}", score, to),
    }
}

/// Build the report of the program's lineage from the project's files. The
/// checker's diagnostics, when given, add the W5 and W6 counts to the
/// overfitting and selection rows.
pub fn build_report(project: &Project, prog: &Program, diags: &[Diagnostic]) -> Result<Report, String> {
    let hash = program_hash(prog);
    let ls = project.lineages()?;
    let lineage: Lineage = ls
        .lineage_of(&hash)
        .cloned()
        .ok_or_else(|| format!("strategy `{}` ({}) has no lineage in {}; declare a study or run it first", prog.strategy, hash, project.dir.display()))?;
    let trials = project.log().for_lineage(&lineage.id)?;
    let runs: Vec<RunRecord> = project.runs()?.into_iter().filter(|r| r.lineage == lineage.id).collect();
    let studies: Vec<StudySpec> = project.studies()?.into_iter().filter(|s| s.lineage == lineage.id).collect();
    let mut raised: BTreeMap<(String, String), usize> = BTreeMap::new();
    let mut bump = |w: &StudyWarning| *raised.entry((w.bias.to_lowercase(), w.message.clone())).or_insert(0) += 1;
    for s in &studies {
        s.warnings.iter().for_each(&mut bump);
    }
    for t in &trials {
        t.warnings.iter().for_each(&mut bump);
    }
    for r in &runs {
        r.warnings.iter().for_each(&mut bump);
    }
    let w5 = diags.iter().filter(|d| d.severity == Severity::Warning && d.code == Code::W5).count();
    let w6 = diags.iter().filter(|d| d.severity == Severity::Warning && d.code == Code::W6).count();
    if w5 > 0 {
        *raised
            .entry((
                "overfitting".into(),
                format!("{} numeric literal(s) in the strategy's rules are degrees of freedom the parameters do not declare (W5)", w5),
            ))
            .or_insert(0) += 1;
    }
    if w6 > 0 {
        *raised
            .entry(("selection".into(), format!("{} ticker literal(s) name securities by a snapshot of the bundle date (W6)", w6)))
            .or_insert(0) += 1;
    }
    let warnings: Vec<RaisedWarning> = raised
        .iter()
        .map(|((b, m), n)| RaisedWarning {
            bias: b.clone(),
            message: m.clone(),
            times: *n,
        })
        .collect();
    let biases: Vec<BiasRow> = BIASES
        .iter()
        .map(|b| BiasRow {
            name: b.name.to_string(),
            audit: b.audit,
            status: b.status.to_string(),
            raised: warnings.iter().filter(|w| w.bias == b.name).map(|w| w.message.clone()).collect(),
        })
        .collect();
    let latest = trials.last().map(TrialSummary::of);
    let reveals: Vec<TrialSummary> = trials.iter().filter(|t| t.kind == TrialKind::Reveal).map(TrialSummary::of).collect();
    let latest_reveal = trials.iter().rfind(|t| t.kind == TrialKind::Reveal);
    let mut thresholds = Vec::new();
    for s in &studies {
        for th in &s.thresholds {
            thresholds.push(ThresholdCheck {
                threshold: th.describe(),
                on_latest_trial: trials.last().and_then(|t| th.holds(&t.metrics, &t.trading)),
                on_latest_reveal: latest_reveal.and_then(|t| th.holds(&t.metrics, &t.trading)),
            });
        }
    }
    Ok(Report {
        strategy: prog.strategy.clone(),
        hash,
        lineage: lineage.id.clone(),
        members: lineage.members.iter().map(|m| (m.hash.clone(), m.strategy.clone(), attachment_text(&m.attachment))).collect(),
        disputes: lineage.disputes.len(),
        studies: studies.iter().map(|s| format!("{} (hold-out {}, objective {})", s.id, s.holdout.describe(), s.objective)).collect(),
        trials: trials.len(),
        study_trials: trials.iter().filter(|t| t.kind == TrialKind::Trial).count(),
        untracked: trials.iter().filter(|t| t.kind == TrialKind::Untracked).count(),
        reveals,
        runs: runs.len(),
        latest,
        dsr: runs.iter().rev().find_map(|r| r.dsr.as_ref().map(|d| d.dsr)),
        degrees_of_freedom: prog.degrees_of_freedom.summary(),
        thresholds,
        warnings,
        biases,
    })
}

impl Report {
    /// The report as text, one section per line group.
    pub fn render(&self) -> String {
        let mut out = String::new();
        let mut line = |s: String| {
            out.push_str(&s);
            out.push('\n');
        };
        line(format!("study report: {} ({}) in lineage {}", self.strategy, self.hash, self.lineage));
        line(format!(
            "members: {}{}",
            self.members.iter().map(|(h, s, a)| format!("{} {} ({})", s, h, a)).collect::<Vec<_>>().join("; "),
            if self.disputes > 0 { format!("; {} dispute(s) logged", self.disputes) } else { String::new() }
        ));
        line(format!("studies: {}", if self.studies.is_empty() { "none declared".to_string() } else { self.studies.join("; ") }));
        line(format!(
            "trials: {} ({} in studies, {} untracked, {} reveal(s)); study runs: {}",
            self.trials,
            self.study_trials,
            self.untracked,
            self.reveals.len(),
            self.runs
        ));
        line(self.degrees_of_freedom.clone());
        if let Some(t) = &self.latest {
            line(format!(
                "latest trial #{} ({:?}{}): {} bars; sharpe {:.3} (se {:.3})   cagr {:.4}   max drawdown {:.4}   cvar5 {:.4}{}",
                t.seq,
                t.kind,
                t.note.as_ref().map(|n| format!(", {}", n)).unwrap_or_default(),
                t.bars,
                t.sharpe,
                t.sharpe_se,
                t.cagr,
                t.max_drawdown,
                t.cvar_5,
                self.dsr.map(|d| format!("; deflated Sharpe ratio of the latest run {:.3}", d)).unwrap_or_default()
            ));
        }
        for r in &self.reveals {
            line(format!(
                "reveal #{} of {}: {} bars out of sample; sharpe {:.3} (se {:.3})   cagr {:.4}   max drawdown {:.4}",
                r.seq, r.hash, r.bars, r.sharpe, r.sharpe_se, r.cagr, r.max_drawdown
            ));
        }
        for th in &self.thresholds {
            let show = |x: Option<bool>| match x {
                Some(true) => "holds",
                Some(false) => "fails",
                None => "not judged",
            };
            line(format!(
                "threshold {}: latest trial {}; latest reveal {}",
                th.threshold,
                show(th.on_latest_trial),
                show(th.on_latest_reveal)
            ));
        }
        line(format!("warnings raised: {}", self.warnings.len()));
        for w in &self.warnings {
            line(format!("  ({}) {}{}", w.bias, w.message, if w.times > 1 { format!(" [{} times]", w.times) } else { String::new() }));
        }
        line("bias audit (A information and time, B execution realism, C research process):".into());
        for b in &self.biases {
            line(format!(
                "  {} {:<32} {}{}",
                b.audit,
                b.name,
                b.status,
                if b.raised.is_empty() { String::new() } else { format!("  <- raised ({})", b.raised.len()) }
            ));
        }
        out
    }
}
