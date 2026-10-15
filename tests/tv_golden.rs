//! Translation-validation golden files: the stand-in for gate G1
//! (`docs/spec/00-architecture.md`, section 7.1) while the books' parity
//! inputs are not reachable from the build (`docs/spec/ERRATA.md`). Every
//! corpus-v2 strategy and both company books run on the seeded synthetic
//! market of their class, as `tests/corpus_v2.rs` runs them, under the batch
//! driver and the fold; what each run decided, filled and held is rendered
//! as text (times ISO-8601, so the rendering does not depend on the time
//! unit; floats in their shortest round-trip form, so it is exact) and
//! compared with `tests/golden/<name>.txt` byte for byte. Every step of the
//! migration (`docs/spec/05-migration.md`) keeps it equal.
//!
//! `ABT_BLESS=1 cargo test --test tv_golden` rewrites the files. A commit
//! that blesses names the semantic change it accepts, and
//! `docs/spec/ERRATA.md` records it.

use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};

use absolute_backtest::check::{check_program, Workspace};
use absolute_backtest::data::synthetic_for_program;
use absolute_backtest::kernel::time::format_timestamp;
use absolute_backtest::kernel::{run, run_fold, Action, ExecConfig, RunResult};

fn corpus() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("corpus-v2")
}

fn golden_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests").join("golden")
}

fn files(dir: &Path) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = fs::read_dir(dir)
        .unwrap_or_else(|e| panic!("{}: {}", dir.display(), e))
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().map(|x| x == "dsl").unwrap_or(false))
        .collect();
    out.sort();
    out
}

/// The catalog and the standard library, then the program; the strategy's name.
fn workspace(program: &Path) -> (Workspace, String) {
    let mut ws = Workspace::new();
    for dir in ["catalog", "lib"] {
        for f in files(&corpus().join(dir)) {
            let src = fs::read_to_string(&f).unwrap();
            ws.add_source(&src).unwrap_or_else(|e| panic!("{}: {}", f.display(), e));
        }
    }
    let src = fs::read_to_string(program).unwrap();
    ws.add_source(&src).unwrap_or_else(|e| panic!("{}: {}", program.display(), e));
    let name = src
        .lines()
        .find_map(|l| l.trim().strip_prefix("strategy ").map(|r| r.split_whitespace().next().unwrap().trim_end_matches('{').to_string()))
        .unwrap_or_else(|| panic!("{}: no strategy", program.display()));
    (ws, name)
}

/// What a run decided, filled and held, as exact text.
fn render(r: &RunResult) -> String {
    let mut out = String::new();
    let sym = |s: u32| r.symbols[s as usize].as_str();
    let (first, last) = (r.bars.first().map(|&t| format_timestamp(t)), r.bars.last().map(|&t| format_timestamp(t)));
    writeln!(out, "bars {} {} {}", r.bars.len(), first.unwrap_or_default(), last.unwrap_or_default()).unwrap();
    for d in &r.decisions {
        writeln!(out, "decision {} {}", format_timestamp(d.t), r.describe_decision(&d.decision)).unwrap();
    }
    for f in &r.fills {
        writeln!(
            out,
            "fill {} {} qty {:?} price {:?} at_last {} commission {:?} fee {:?} slippage {:?} impact {:?} participation {:?} partial {} forced {}",
            format_timestamp(f.t),
            sym(f.equity),
            f.quantity,
            f.price,
            f.at_last_price,
            f.commission,
            f.fee,
            f.slippage,
            f.impact,
            f.participation,
            f.partial,
            f.forced
        )
        .unwrap();
    }
    for (t, d, why) in &r.dropped {
        writeln!(out, "dropped {} {} {}", format_timestamp(*t), r.describe_decision(d), why).unwrap();
    }
    for e in &r.exposure {
        writeln!(
            out,
            "book {} cash {:?} gross {:?} net {:?} equity {:?} leverage {:?}",
            format_timestamp(e.t),
            e.cash,
            e.gross,
            e.net,
            e.equity,
            e.leverage
        )
        .unwrap();
    }
    for a in &r.actions {
        let what = match &a.action {
            Action::Split { factor } => format!("split {:?}", factor),
            Action::Dividend { amount, shares } => format!("dividend {:?} on {:?}", amount, shares),
            Action::Delisting { reason, haircut } => format!("delisting {} haircut {:?}", reason, haircut),
            Action::Reinvest { amount, shares, added } => format!("reinvest {:?} on {:?} added {:?}", amount, shares, added),
        };
        writeln!(out, "action {} {} {} cash {:?}", format_timestamp(a.t), sym(a.equity), what, a.cash).unwrap();
    }
    writeln!(out, "final_cash {:?}", r.final_cash).unwrap();
    for (s, q) in &r.final_positions {
        writeln!(out, "position {} {:?}", sym(*s), q).unwrap();
    }
    out
}

/// The run of one corpus program on its synthetic market, rendered; the
/// batch driver and the fold must agree.
fn golden_run(dir: &str, f: &Path) -> (String, String) {
    let (ws, name) = workspace(f);
    let (prog, diags) = check_program(&ws, &name);
    let Some(mut prog) = prog else {
        let text: Vec<String> = diags.iter().map(|d| d.to_string()).collect();
        panic!("{}/{}: does not check:\n{}", dir, name, text.join("\n"));
    };
    let syms: Vec<&str> = if prog.class == "future" {
        vec![
            "ES", "NQ", "KC", "6J", "ZB", "ZF", "ZN", "RTY", "ZT", "CL", "GC", "HE", "HO", "LE", "NG", "PL", "RB", "SI", "ZC", "ZL", "ZM", "ZS", "ZW", "GF", "SB", "ETH",
        ]
    } else {
        vec!["AAA", "XYZ", "BBB", "SPY", "DDD"]
    };
    let days = if prog.base.is_sub_daily() { 30 } else { 400 };
    let ds = synthetic_for_program(&prog, &syms, (2022, 1, 3), days, 11);
    let cfg = ExecConfig::from_program(&prog);
    // As tests/corpus_v2.rs: R8L's features need 250 sessions of history and
    // the synthetic market holds 30, so its windows are shortened.
    if name == "r8l" {
        for (p, v) in [("atr_rows", 3), ("med_rows", 6), ("med_min", 3)] {
            for params in prog.params.values_mut() {
                if let Some(x) = params.get_mut(p) {
                    x.value = absolute_backtest::Lit::Int(v);
                }
            }
        }
    }
    let batch = match run(&prog, &ds, cfg.clone()) {
        Ok(r) => render(&r),
        Err(e) => format!("error {}\n", e),
    };
    let fold = match run_fold(&prog, &ds, cfg) {
        Ok(r) => render(&r),
        Err(e) => format!("error {}\n", e),
    };
    assert!(batch == fold, "{}/{}: the fold differs from the batch driver: {}", dir, name, first_difference(&batch, &fold));
    (name, batch)
}

/// The first line where two renderings differ, with a little context.
fn first_difference(want: &str, got: &str) -> String {
    let (w, g): (Vec<&str>, Vec<&str>) = (want.lines().collect(), got.lines().collect());
    let i = w.iter().zip(&g).position(|(a, b)| a != b).unwrap_or(w.len().min(g.len()));
    format!(
        "line {} (golden {} lines, run {} lines)\n  golden: {}\n  run:    {}",
        i + 1,
        w.len(),
        g.len(),
        w.get(i).unwrap_or(&"<end>"),
        g.get(i).unwrap_or(&"<end>")
    )
}

fn check_dir(dir: &str) {
    let bless = std::env::var("ABT_BLESS").map(|v| v == "1").unwrap_or(false);
    fs::create_dir_all(golden_dir()).unwrap();
    let mut failures = Vec::new();
    for f in files(&corpus().join(dir)) {
        let (name, got) = golden_run(dir, &f);
        let path = golden_dir().join(format!("{}.txt", name));
        if bless {
            fs::write(&path, &got).unwrap();
            continue;
        }
        match fs::read_to_string(&path) {
            Ok(want) if want == got => {}
            Ok(want) => failures.push(format!("{}: differs from {}: {}", name, path.display(), first_difference(&want, &got))),
            Err(e) => failures.push(format!("{}: {}: {} (run with ABT_BLESS=1 to write it)", name, path.display(), e)),
        }
    }
    assert!(failures.is_empty(), "{} golden run(s) differ:\n{}", failures.len(), failures.join("\n"));
}

#[test]
fn strategies_equal_their_golden_runs() {
    check_dir("strategies");
}

#[test]
fn company_books_equal_their_golden_runs() {
    check_dir("company");
}
