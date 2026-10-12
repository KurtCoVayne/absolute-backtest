//! The compiler against the LLM-authored library of the two studies
//! (`experiments/llm-usability`, `experiments/llm-observability`): every
//! attempt and final that checks clean is compiled through every stage,
//! run under the written and the optimised plan on its task's data
//! (translation validation, compiler design section 6.6), verified through
//! tier 4, and queried through the state tables and why-not. A program the
//! pipeline cannot take unchanged, or two plans that differ, is a defect.
//!
//! The briefs run on the seeded synthetic markets with the study's
//! settings scaled down (a fifth of the days) unless `ABT_EXPERIMENTS_FULL`
//! is set; the two books need the synthetic data of `scripts/synth/`, named
//! by `ABT_WEEKLY_DATA` and `ABT_SESSIONS_DATA`, and are skipped with a note
//! otherwise. `ABT_EXPERIMENTS_REPORT=FILE` writes one line per program.

mod corpus;

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use absolute_backtest::check::{check_program, Program};
use absolute_backtest::compiler::{compile, CompileOptions};
use absolute_backtest::data::{load_parquet_dir, synthetic_daily, synthetic_daily_v2, synthetic_minute};
use absolute_backtest::kernel::time::parse_timestamp;
use absolute_backtest::kernel::{Dataset, ExecConfig, Kernel};
use absolute_backtest::lower::Lowered;
use absolute_backtest::plan::cost::Statistics;
use absolute_backtest::state::{provenance, tables};
use absolute_backtest::verify::tv::{first_difference, run_with_ledgers};
use absolute_backtest::verify::{verify, VerifyInputs};
use absolute_backtest::{Kind, Resolution};

/// The tasks of the studies and how each is run (`experiments/*/score.*`).
const TASKS: [&str; 6] = ["mw14", "r8l", "momentum_liquid_monthly", "intraday_open_gap", "breakout_with_stop", "vol_targeted_trend"];

fn task_of(path: &Path) -> Option<&'static str> {
    let text = path.to_string_lossy();
    TASKS.iter().copied().find(|t| text.contains(&format!("/{}-", t)))
}

fn full_scale() -> bool {
    std::env::var("ABT_EXPERIMENTS_FULL").map(|v| !v.is_empty() && v != "0").unwrap_or(false)
}

fn scaled(days: usize) -> usize {
    if full_scale() {
        days
    } else {
        (days / 5).max(10)
    }
}

/// The library: every `.dsl` under `experiments/`, sorted.
fn library() -> Vec<PathBuf> {
    fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
        let Ok(rd) = fs::read_dir(dir) else { return };
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                walk(&p, out);
            } else if p.extension().map(|x| x == "dsl").unwrap_or(false) {
                out.push(p);
            }
        }
    }
    let mut out = Vec::new();
    walk(&Path::new(env!("CARGO_MANIFEST_DIR")).join("experiments"), &mut out);
    out.sort();
    out
}

fn strategy_name(src: &str) -> Option<String> {
    let units = absolute_backtest::parse_units(src).ok()?;
    units.iter().find(|u| u.kind == absolute_backtest::UnitKind::Strategy).map(|u| u.name.clone())
}

/// The task's data for a program: the study's synthetic market settings,
/// or the book's generated data directory.
fn dataset_for(task: &str, prog: &Program) -> Result<Dataset, String> {
    let symbols = |n: usize| -> Vec<String> { (0..n).map(|i| ((b'A' + i as u8) as char).to_string()).collect() };
    let synthetic = |syms: Vec<String>, days: usize| -> Dataset {
        let s: Vec<&str> = syms.iter().map(|x| x.as_str()).collect();
        let minute = prog.relations.values().any(|r| matches!(r.kind, Kind::Primitive { .. }) && r.res == Some(Resolution::M1));
        if minute {
            synthetic_minute(&s, (2024, 1, 2), days, 390, 7)
        } else if prog.relations.contains_key("split") {
            synthetic_daily_v2(&s, (2022, 1, 3), days, 7)
        } else {
            synthetic_daily(&s, (2022, 1, 3), days, 7)
        }
    };
    match task {
        "mw14" | "r8l" => {
            let var = if task == "mw14" { "ABT_WEEKLY_DATA" } else { "ABT_SESSIONS_DATA" };
            let dir = std::env::var(var).map_err(|_| format!("{} not set", var))?;
            load_parquet_dir(prog, Path::new(&dir)).map(|(ds, _)| ds).map_err(|e| format!("{}: {}", var, e))
        }
        "momentum_liquid_monthly" => Ok(synthetic(symbols(15), scaled(1000))),
        "intraday_open_gap" => Ok(synthetic(vec!["AAA".into(), "BBB".into(), "CCC".into(), "DDD".into(), "SPY".into()], scaled(40))),
        "breakout_with_stop" => Ok(synthetic(symbols(12), scaled(1000))),
        _ => Ok(synthetic(vec!["SPY".into()], scaled(1000))),
    }
}

/// The task's executor settings (`TASK_FLAGS` of the scorer).
fn config_for(task: &str) -> ExecConfig {
    match task {
        "mw14" => ExecConfig {
            initial_cash: 1_000_000.0,
            compounding: false,
            lot: absolute_backtest::kernel::Lot::Fractional,
            commission_bps: 10.0,
            margin_rate: 0.0,
            on_leverage: absolute_backtest::kernel::OnLeverage::Allow,
            delist_at_last_price: true,
            actions_in_prices: true,
            price_relation: Some("trclose".into()),
            start: parse_timestamp("2016-01-01"),
            ..ExecConfig::frictionless()
        },
        "r8l" => ExecConfig {
            initial_cash: 1_000_000.0,
            compounding: false,
            lot: absolute_backtest::kernel::Lot::Fractional,
            margin_rate: 0.0,
            on_leverage: absolute_backtest::kernel::OnLeverage::Allow,
            on_margin_call: absolute_backtest::kernel::OnMarginCall::Allow,
            price_relation: Some("close_m".into()),
            ..ExecConfig::frictionless()
        },
        _ => ExecConfig::default(),
    }
}

/// What one program went through.
#[derive(Default)]
struct Row {
    file: String,
    task: String,
    strategy: String,
    outcome: String,
    warnings: BTreeMap<String, usize>,
    rewrites: usize,
    reordered: usize,
    rules: usize,
    infeasible: usize,
    elided: usize,
    decisions: Option<usize>,
    bars: Option<usize>,
    verify: BTreeMap<String, usize>,
    notes: Vec<String>,
}

impl Row {
    fn line(&self) -> String {
        let w: Vec<String> = self.warnings.iter().map(|(k, v)| format!("{}×{}", k, v)).collect();
        let v: Vec<String> = self.verify.iter().map(|(k, c)| format!("{} {}", c, k)).collect();
        format!(
            "{}\t{}\t{}\t{}\twarnings {}\trewrites {}\treordered {}/{}\tinfeasible {}\telided {}\tbars {}\tdecisions {}\tverify {}{}",
            self.task,
            self.file,
            self.strategy,
            self.outcome,
            if w.is_empty() { "-".into() } else { w.join(",") },
            self.rewrites,
            self.reordered,
            self.rules,
            self.infeasible,
            self.elided,
            self.bars.map(|b| b.to_string()).unwrap_or_else(|| "-".into()),
            self.decisions.map(|d| d.to_string()).unwrap_or_else(|| "-".into()),
            v.join(", "),
            if self.notes.is_empty() { String::new() } else { format!("\t{}", self.notes.join("; ")) }
        )
    }
}

fn halt_key(m: &str) -> String {
    m.split(" at tuple").next().unwrap_or(m).to_string()
}

/// Put one program through the pipeline on its task's data.
fn exercise(path: &Path) -> Row {
    let rel = path.strip_prefix(Path::new(env!("CARGO_MANIFEST_DIR"))).unwrap_or(path).to_string_lossy().to_string();
    let mut row = Row {
        file: rel.clone(),
        ..Default::default()
    };
    let Some(task) = task_of(path) else {
        row.outcome = "skipped: no task".into();
        return row;
    };
    row.task = task.into();
    let src = fs::read_to_string(path).unwrap();
    let Some(name) = strategy_name(&src) else {
        row.outcome = "skipped: does not parse".into();
        return row;
    };
    row.strategy = name.clone();
    let mut ws = corpus::base_workspace();
    // A sandbox's own library files, next to the run (`<run>/lib/*.dsl`).
    let run_dir = path.parent().and_then(|p| if p.file_name().map(|n| n == "attempts").unwrap_or(false) { p.parent() } else { Some(p) });
    if let Some(lib) = run_dir.map(|d| d.join("lib")).filter(|d| d.is_dir()) {
        let mut libs: Vec<PathBuf> = fs::read_dir(&lib)
            .unwrap()
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().map(|x| x == "dsl").unwrap_or(false))
            .collect();
        libs.sort();
        for l in libs {
            if let Ok(text) = fs::read_to_string(&l) {
                let _ = ws.add_source(&text);
            }
        }
    }
    if let Err(e) = ws.add_source(&src) {
        row.outcome = format!("skipped: {}", e);
        return row;
    }
    let (prog, diags) = check_program(&ws, &name);
    let errors = diags.iter().filter(|d| d.severity == absolute_backtest::check::Severity::Error).count();
    let Some(prog) = prog else {
        row.outcome = format!("skipped: {} checker error(s)", errors);
        return row;
    };
    if errors > 0 {
        row.outcome = format!("skipped: {} checker error(s)", errors);
        return row;
    }
    let ds = match dataset_for(task, &prog) {
        Ok(ds) => ds,
        Err(e) => {
            row.outcome = format!("skipped: {}", e);
            return row;
        }
    };
    let cfg = config_for(task);
    let stats = Statistics::from_dataset(&prog, &ds, task).with_start(&prog, &ds, cfg.start);
    let opts = CompileOptions {
        numerics: absolute_backtest::lower::Numerics::from_running_sums(cfg.running_sums),
        stats: Some(stats),
        ..CompileOptions::default()
    };
    let mut compiled = compile(&prog, &opts);
    for w in &compiled.analysis.warnings {
        *row.warnings.entry(w.code.clone()).or_default() += 1;
    }
    row.rewrites = compiled.rewritten.log.entries.len();
    row.reordered = compiled.plan.totals.reordered;
    row.rules = compiled.plan.totals.rules;
    row.infeasible = compiled.plan.totals.infeasible;
    row.elided = compiled.plan.totals.elided;
    for n in &compiled.lowered.notes {
        row.notes.push(format!("lower: {}", n));
    }
    // Translation validation: the written plan against the optimised one.
    let written = Lowered::written(&prog, opts.numerics);
    let a = run_with_ledgers(&prog, &written, &ds, cfg.clone());
    let b = run_with_ledgers(&prog, &compiled.lowered, &ds, cfg.clone());
    row.outcome = match (&a, &b) {
        (Ok(ra), Ok(rb)) => {
            row.decisions = Some(rb.result.decisions.len());
            row.bars = Some(rb.result.bars.len());
            match first_difference(&ra.digest, &rb.digest) {
                Some((i, x, y)) => format!("TV DIFFERS at digest line {}: written `{}` optimised `{}`", i, x, y),
                None => {
                    let mut diff = None;
                    for (rel, rows) in &ra.ledgers {
                        match rb.ledgers.get(rel) {
                            Some(other) => {
                                if let Some((i, x, y)) = first_difference(rows, other) {
                                    diff = Some(format!("TV DIFFERS in the ledger of {} at tuple {}: written `{}` optimised `{}`", rel, i, x, y));
                                    break;
                                }
                            }
                            None => {
                                diff = Some(format!("TV DIFFERS: ledger of {} missing", rel));
                                break;
                            }
                        }
                    }
                    diff.unwrap_or_else(|| "tv equal".into())
                }
            }
        }
        (Err(ea), Err(eb)) => {
            if halt_key(ea) == halt_key(eb) {
                format!("both halt: {}", halt_key(ea).chars().take(90).collect::<String>())
            } else {
                format!("TV DIFFERS: written halts `{}`, optimised halts `{}`", halt_key(ea), halt_key(eb))
            }
        }
        (Ok(_), Err(e)) => format!("TV DIFFERS: the optimised plan halts `{}` and the written one runs", halt_key(e)),
        (Err(e), Ok(_)) => format!("TV DIFFERS: the written plan halts `{}` and the optimised one runs", halt_key(e)),
    };
    // Verification through tier 4 (no solver: tiers 3 and 4 undecided).
    let report = verify(
        &VerifyInputs {
            prog: &prog,
            diags: &diags,
            opts: &opts,
            max_tier: 4,
            data: None,
        },
        &mut compiled,
    );
    row.verify = report.counts();
    if report.refuted() {
        row.notes.push("verify: a refuted obligation".into());
    }
    // The state tables and why-not on the optimised run's last bar.
    if let Ok(rb) = &b {
        if let Some(&last) = rb.result.bars.last() {
            let lowered = &compiled.lowered;
            let tables_ok = std::thread::scope(|s| {
                std::thread::Builder::new()
                    .stack_size(256 << 20)
                    .spawn_scoped(s, || -> Result<String, String> {
                        let mut k = Kernel::new_planned(&prog, lowered, &ds, cfg.clone()).map_err(|e| e.to_string())?;
                        k.run().map_err(|e| e.to_string())?;
                        let d = tables::derivation(&mut k, &prog, "decide", last, None).map_err(|e| e.to_string())?;
                        let f = tables::failure(&mut k, &prog, "decide", last, None, None).map_err(|e| e.to_string())?;
                        let w = provenance::why_not(&mut k, &prog, "decide", last, None, Some(absolute_backtest::kernel::Value::Equity(0))).map_err(|e| e.to_string())?;
                        if let Some(first) = d.rows.first() {
                            if let tables::Cell::Str(id) = &first[0] {
                                let tree = provenance::derive(&mut k, &prog, id, 2).map_err(|e| e.to_string())?;
                                if tree.rule.is_none() {
                                    return Err(format!("derivation of {} has no rule: {:?}", id, tree.note));
                                }
                            }
                        }
                        Ok(format!("derivation {} failure {} why-not {} rule(s)", d.len(), f.len(), w.rules.len()))
                    })
                    .unwrap()
                    .join()
                    .unwrap()
            });
            match tables_ok {
                Ok(s) => row.notes.push(s),
                Err(e) => row.notes.push(format!("TABLES FAILED: {}", e)),
            }
        }
    }
    row
}

/// Every program of the library, through the pipeline; a plan difference
/// or a table failure anywhere fails the test with the whole list.
#[test]
fn the_llm_authored_library_compiles_runs_and_validates() {
    let files = library();
    assert!(!files.is_empty(), "no experiment programs found");
    let mut rows = Vec::new();
    for f in &files {
        let row = exercise(f);
        println!("{}", row.line());
        rows.push(row);
    }
    if let Ok(report) = std::env::var("ABT_EXPERIMENTS_REPORT") {
        let text: String = rows.iter().map(|r| r.line() + "\n").collect();
        fs::write(&report, text).unwrap();
    }
    let total = rows.len();
    let exercised: Vec<&Row> = rows.iter().filter(|r| !r.outcome.starts_with("skipped")).collect();
    let skipped_books = rows.iter().filter(|r| r.outcome.contains("not set")).count();
    let equal = exercised.iter().filter(|r| r.outcome == "tv equal").count();
    let halts = exercised.iter().filter(|r| r.outcome.starts_with("both halt")).count();
    let defects: Vec<&&Row> = exercised
        .iter()
        .filter(|r| r.outcome.starts_with("TV DIFFERS") || r.notes.iter().any(|n| n.starts_with("TABLES FAILED") || n.starts_with("verify: a refuted")))
        .collect();
    let deciding = exercised.iter().filter(|r| r.decisions.map(|d| d > 0).unwrap_or(false)).count();
    println!(
        "summary: {} files, {} exercised ({} equal, {} halt alike on both plans, {} deciding), {} skipped ({} book(s) without data), {} defect(s)",
        total,
        exercised.len(),
        equal,
        halts,
        deciding,
        total - exercised.len(),
        skipped_books,
        defects.len()
    );
    assert!(exercised.len() >= 40, "the library should exercise at least forty programs, {} did", exercised.len());
    assert!(defects.is_empty(), "defects:\n{}", defects.iter().map(|r| r.line()).collect::<Vec<_>>().join("\n"));
}
