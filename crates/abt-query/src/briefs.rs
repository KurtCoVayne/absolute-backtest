//! The briefs harness (data-bundle doc, section 8; M-5): plain-language
//! briefs given to a model, its successive attempts kept as files, and what
//! happened recorded, never asserted. Each attempt is checked by the kernel
//! (whether it parses, its diagnostics, whether it is a valid program) and
//! recorded as a row of `query.briefs.attempts`, the attempts directory as
//! a feed; the first valid attempt of a brief runs as a plan instantiated
//! from the template `briefs_harness`, and its verdict is read from the
//! run's tables: whether it trades, and whether it survives its costs.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use abt_wire::generated::abt::wire::{Role, SourceFile};

use crate::data::Data;
use crate::kernel::{Kernel, Program};
use crate::plan::{self, exec};
use crate::relation::{Col, Rows};

/// The template every brief's first valid attempt runs under: one run of
/// the program, over its binding's whole sample.
pub const TEMPLATE: &str = "template briefs_harness(PROGRAM) {\n  program PROGRAM\n}\n";

/// The relation the attempts are recorded in.
pub const ATTEMPTS: &str = "query.briefs.attempts";

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct AttemptRecord {
    pub file: String,
    pub strategy: Option<String>,
    /// The parse error, when the attempt did not parse.
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
    /// `trades`, `does not trade`, `does not survive costs`, `halted: ...`
    /// or `failed: ...`.
    pub verdict: String,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct BriefRecord {
    pub brief: String,
    pub text: String,
    pub attempts: Vec<AttemptRecord>,
    pub first_attempt_valid: bool,
    /// The 1-based index of the first valid attempt.
    pub attempts_to_valid: Option<usize>,
    pub verdict: Option<Verdict>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct BriefsReport {
    pub briefs: Vec<BriefRecord>,
    pub first_attempt_pass_rate: f64,
    /// Occurrences of every diagnostic code across the attempts, `parse`
    /// for a parse error.
    pub diagnostics: BTreeMap<String, usize>,
    pub mean_attempts_to_valid: Option<f64>,
    pub briefs_without_a_valid_attempt: usize,
    pub verdicts: BTreeMap<String, usize>,
}

fn sorted_files(dir: &Path, ext: &str) -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = std::fs::read_dir(dir)
        .map(|rd| rd.filter_map(|e| e.ok().map(|e| e.path())).filter(|p| p.extension().map(|x| x == ext).unwrap_or(false)).collect())
        .unwrap_or_default();
    // Attempts are numbered: in their numbers' order when the stems are
    // numbers.
    files.sort_by_key(|p| {
        let stem = p.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
        (stem.parse::<u64>().unwrap_or(u64::MAX), stem)
    });
    files
}

/// A diagnostic as the kernel prints it (`error [B] unit at span[ in rule
/// R]: message (judgment)`): its code, judgment and message, and whether it
/// is an error.
pub fn diagnostic(line: &str) -> Option<(String, String, String, bool)> {
    let error = line.starts_with("error");
    let open = line.find('[')?;
    let close = open + line[open..].find(']')?;
    let code = line[open + 1..close].to_string();
    let rest = &line[close + 1..];
    let at = rest.find(" at ")?;
    let msg_start = at + rest[at..].find(": ")? + 2;
    let body = &rest[msg_start..];
    let (message, judgment) = match (body.rfind(" ("), body.ends_with(')')) {
        (Some(i), true) => (body[..i].to_string(), body[i + 2..body.len() - 1].to_string()),
        _ => (body.to_string(), String::new()),
    };
    Some((code, judgment, message, error))
}

/// The last strategy an attempt declares.
fn strategy_of(text: &str) -> Option<String> {
    text.lines()
        .filter_map(|l| {
            l.trim()
                .strip_prefix("strategy ")
                .map(|r| r.split(|c: char| c.is_whitespace() || c == '{').next().unwrap_or("").to_string())
        })
        .rfind(|s| !s.is_empty())
}

/// One attempt checked against the library, and, when it is valid, run.
fn attempt(data: &Data, kernels: &[Kernel], library: &[SourceFile], brief: &str, path: &Path, run: bool) -> (AttemptRecord, Option<Verdict>) {
    let file = path.file_name().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
    let invalid = |e: String| AttemptRecord {
        file: file.clone(),
        strategy: None,
        parse_error: Some(e),
        diagnostics: vec![],
        errors: 1,
        warnings: 0,
        valid: false,
    };
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) => return (invalid(e.to_string()), None),
    };
    let Some(strategy) = strategy_of(&text) else {
        return (invalid("no strategy unit in the attempt".into()), None);
    };
    // The attempt's strategy replaces a library file declaring one of its
    // name (a corpus strategy the brief asks for again).
    let mut sources: Vec<SourceFile> = library.iter().filter(|f| strategy_of(&f.text).as_deref() != Some(strategy.as_str())).cloned().collect();
    sources.push(SourceFile {
        text,
        path: Some(path.to_string_lossy().into_owned()),
    });
    let prog = Program { sources, strategy: strategy.clone() };
    let Some(kernel) = kernels.first() else {
        return (invalid("no kernel to check the attempt with".into()), None);
    };
    let (checked, lines) = match kernel.check_full(&prog) {
        Ok(c) => (c.checked, c.diagnostics),
        // The kernel refuses what does not parse.
        Err(e) => {
            let e = e.to_string();
            let mut r = invalid(e.split_once("parse error: ").map(|(_, m)| m.to_string()).unwrap_or(e));
            r.strategy = Some(strategy);
            return (r, None);
        }
    };
    let parsed: Vec<(String, String, String, bool)> = lines.iter().filter_map(|l| diagnostic(l)).collect();
    let record = AttemptRecord {
        file,
        strategy: Some(strategy.clone()),
        parse_error: None,
        diagnostics: parsed.iter().map(|(c, j, m, _)| (c.clone(), j.clone(), m.clone())).collect(),
        errors: parsed.iter().filter(|d| d.3).count(),
        warnings: parsed.iter().filter(|d| !d.3).count(),
        valid: checked.is_some(),
    };
    let Some((_, dof)) = checked.filter(|_| run) else { return (record, None) };
    // The first valid attempt runs as an instance of the template.
    let failed = |why: String| Verdict {
        strategy: strategy.clone(),
        bars: 0,
        decisions: 0,
        fills: 0,
        sharpe: 0.0,
        cagr: 0.0,
        max_drawdown: 0.0,
        degrees_of_freedom: dof.clone(),
        verdict: why,
    };
    let name = format!("brief_{}", brief.replace(|c: char| !(c.is_alphanumeric() || c == '_'), "_"));
    let units = match plan::parse(&format!("{}study {} = briefs_harness(PROGRAM = {})\n", TEMPLATE, name, strategy)) {
        Ok(u) => u,
        Err(e) => return (record, Some(failed(format!("failed: {}", e)))),
    };
    let p = match plan::plan(&units, &name) {
        Ok(p) => p,
        Err(e) => return (record, Some(failed(format!("failed: {}", e)))),
    };
    let verdict = match exec::execute(&p, &prog, &exec::Endpoints { data, kernels }, &exec::Options::default()) {
        Err(e) => failed(format!("failed: {}", e)),
        Ok(ex) => match ex.outcomes.first() {
            None => failed("failed: the plan made no run".into()),
            Some(o) if o.status != "completed" => failed(format!("halted: {}", o.status.trim_start_matches("halted(").trim_end_matches(')'))),
            Some(o) => {
                let bars = crate::run::Run::read(data, &o.run_id).map(|r| r.book.len()).unwrap_or(0);
                let verdict = if o.trading.fills == 0 {
                    "does not trade"
                } else if o.returns.cagr <= 0.0 {
                    "does not survive costs"
                } else {
                    "trades"
                };
                Verdict {
                    strategy: strategy.clone(),
                    bars,
                    decisions: o.trading.decisions,
                    fills: o.trading.fills,
                    sharpe: o.returns.sharpe,
                    cagr: o.returns.cagr,
                    max_drawdown: o.returns.max_drawdown,
                    degrees_of_freedom: dof,
                    verdict: verdict.into(),
                }
            }
        },
    };
    (record, Some(verdict))
}

/// The attempts as rows of the data layer.
fn record_attempts(data: &Data, rows: &[(String, AttemptRecord)], at: i64) -> Result<(), String> {
    if rows.is_empty() {
        return Ok(());
    }
    let n = rows.len();
    let (schema, batch) = Rows::new(ATTEMPTS)
        .col("brief", Role::Label, Col::Label(rows.iter().map(|(b, _)| Some(b.clone())).collect()))
        .col("attempt", Role::Label, Col::Label(rows.iter().map(|(_, a)| Some(a.file.clone())).collect()))
        .col("t", Role::EventTime, Col::Time(vec![Some(at); n]))
        .col("strategy", Role::Value, Col::Bytes(rows.iter().map(|(_, a)| a.strategy.clone()).collect()))
        .col("valid", Role::Value, Col::Bool(rows.iter().map(|(_, a)| Some(a.valid)).collect()))
        .col("errors", Role::Value, Col::Count(rows.iter().map(|(_, a)| Some(a.errors as i64)).collect()))
        .col("warnings", Role::Value, Col::Count(rows.iter().map(|(_, a)| Some(a.warnings as i64)).collect()))
        .col("parse_error", Role::Value, Col::Bytes(rows.iter().map(|(_, a)| a.parse_error.clone()).collect()))
        .col(
            "diagnostics",
            Role::Value,
            Col::Bytes(rows.iter().map(|(_, a)| Some(serde_json::to_string(&a.diagnostics).unwrap_or_default())).collect()),
        )
        .finish()
        .map_err(|e| e.to_string())?;
    data.put(schema, &batch, true).map(|_| ()).map_err(|e| e.to_string())
}

/// Every brief (`*.md` but the README) with its attempts
/// (`attempts/<brief>/*.dsl`, numbered), each checked, the first valid one
/// run; the attempts recorded in the data layer.
pub fn report(data: &Data, kernels: &[Kernel], library: &[SourceFile], briefs_dir: &Path, attempts_dir: &Path) -> Result<BriefsReport, String> {
    if !briefs_dir.is_dir() {
        return Err(format!("{}: not a directory of briefs", briefs_dir.display()));
    }
    let mut briefs = Vec::new();
    let mut diagnostics: BTreeMap<String, usize> = BTreeMap::new();
    let mut verdicts: BTreeMap<String, usize> = BTreeMap::new();
    let mut rows = Vec::new();
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
            let (record, v) = attempt(data, kernels, library, &stem, file, attempts_to_valid.is_none());
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
            rows.push((stem.clone(), record.clone()));
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
    record_attempts(data, &rows, exec::now())?;
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
