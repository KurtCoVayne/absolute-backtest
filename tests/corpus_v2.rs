//! The acceptance corpus (`corpus-v2/`): every strategy and company
//! program checks clean modulo its `# allow:` lines, and every negative
//! case produces exactly the code its `# expect:` header names.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use absolute_backtest::check::{check_program, check_workspace, Code, Diagnostic, Severity, Workspace};

fn corpus() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("corpus-v2")
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

/// The catalog and the standard library, then the program.
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

fn header_codes(src: &str, key: &str) -> BTreeSet<String> {
    src.lines()
        .filter_map(|l| l.trim().strip_prefix('#').map(str::trim))
        .filter_map(|l| l.strip_prefix(key).map(str::trim))
        .map(|l| l.split(|c: char| c.is_whitespace() || c == '(').next().unwrap().to_string())
        .collect()
}

fn render(ds: &[Diagnostic]) -> String {
    ds.iter().map(|d| format!("  {}", d)).collect::<Vec<_>>().join("\n")
}

fn check_clean(program: &Path) -> Result<(), String> {
    let src = fs::read_to_string(program).unwrap();
    let allowed = header_codes(&src, "allow:");
    let (ws, name) = workspace(program);
    let ws_diags = check_workspace(&ws);
    if ws_diags.iter().any(|d| d.severity == Severity::Error) {
        return Err(format!("{}: workspace errors:\n{}", program.display(), render(&ws_diags)));
    }
    let (prog, diags) = check_program(&ws, &name);
    let bad: Vec<&Diagnostic> = diags.iter().filter(|d| d.severity == Severity::Error || !allowed.contains(&d.code.to_string())).collect();
    if !bad.is_empty() || prog.is_none() {
        return Err(format!("{}: not clean (allowed {:?}):\n{}", program.display(), allowed, render(&diags)));
    }
    // An `# allow:` line names a warning the program raises; a stale one is dropped.
    for code in &allowed {
        if !diags.iter().any(|d| d.code.to_string() == *code) {
            return Err(format!("{}: allows {} but does not raise it; drop the header", program.display(), code));
        }
    }
    Ok(())
}

fn all_clean(dir: &str, count: usize) {
    let fs = files(&corpus().join(dir));
    assert_eq!(fs.len(), count);
    let failures: Vec<String> = fs.iter().filter_map(|f| check_clean(f).err()).collect();
    assert!(failures.is_empty(), "{} program(s) not clean:\n{}", failures.len(), failures.join("\n"));
}

#[test]
fn library_checks_clean() {
    let mut ws = Workspace::new();
    for dir in ["catalog", "lib"] {
        for f in files(&corpus().join(dir)) {
            ws.add_source(&fs::read_to_string(&f).unwrap()).unwrap_or_else(|e| panic!("{}: {}", f.display(), e));
        }
    }
    let diags = check_workspace(&ws);
    assert!(diags.iter().all(|d| d.severity != Severity::Error), "library:\n{}", render(&diags));
}

#[test]
fn strategies_check_clean() {
    all_clean("strategies", 24);
}

#[test]
fn company_programs_check_clean() {
    all_clean("company", 2);
}

#[test]
fn negative_cases_produce_their_code() {
    let fs = files(&corpus().join("negative"));
    assert_eq!(fs.len(), 35);
    let mut failures = Vec::new();
    for f in fs {
        let src = fs::read_to_string(&f).unwrap();
        let expect = header_codes(&src, "expect:");
        assert_eq!(expect.len(), 1, "{}: one expected code", f.display());
        let expect = expect.into_iter().next().unwrap();
        assert!(Code::parse(&expect).is_some(), "{}: unknown code {}", f.display(), expect);
        let (ws, name) = workspace(&f);
        let (prog, diags) = check_program(&ws, &name);
        assert!(prog.is_none(), "{}: a program with an error is not produced", f.display());
        let errors: BTreeSet<String> = diags.iter().filter(|d| d.severity == Severity::Error).map(|d| d.code.to_string()).collect();
        if errors != BTreeSet::from([expect.clone()]) {
            failures.push(format!("{}: expected {}, got {:?}\n{}", f.display(), expect, errors, render(&diags)));
        }
    }
    assert!(failures.is_empty(), "{} negative case(s) wrong:\n{}", failures.len(), failures.join("\n"));
}

/// Every strategy and company program runs on synthetic data of its class
/// and base, with the execution block it writes, and decides something.
fn runs_on_synthetic_data(dir: &str) {
    use absolute_backtest::data::synthetic_for_program;
    use absolute_backtest::kernel::{run, ExecConfig};
    let mut failures = Vec::new();
    for f in files(&corpus().join(dir)) {
        let (ws, name) = workspace(&f);
        let (prog, diags) = check_program(&ws, &name);
        let Some(prog) = prog else {
            failures.push(format!("{}: does not check:\n{}", f.display(), render(&diags)));
            continue;
        };
        // The daily market renames its second name and delists its last;
        // the names the strategies cite ("AAA", "BBB", "SPY") stay listed.
        // The futures market holds every HOME-26 root the books table.
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
        // R8L's features need 250 sessions of history; the synthetic market
        // holds 30, so its windows are shortened for the run.
        let mut prog = prog;
        if name == "r8l" {
            for (p, v) in [("atr_rows", 3), ("med_rows", 6), ("med_min", 3)] {
                for params in prog.params.values_mut() {
                    if let Some(x) = params.get_mut(p) {
                        x.value = absolute_backtest::Lit::Int(v);
                    }
                }
            }
        }
        match run(&prog, &ds, cfg) {
            Ok(r) => {
                eprintln!("{}: {} bars, {} decisions, {} fills, {} dropped", name, r.bars.len(), r.decisions.len(), r.fills.len(), r.dropped.len());
                if r.bars.is_empty() {
                    failures.push(format!("{}: the run has no bars", f.display()));
                } else if r.decisions.is_empty() {
                    failures.push(format!("{}: the run decides nothing over {} bars", f.display(), r.bars.len()));
                }
            }
            Err(e) => failures.push(format!("{}: run failed: {}", f.display(), e)),
        }
    }
    assert!(failures.is_empty(), "{} program(s) failed to run:\n{}", failures.len(), failures.join("\n"));
}

#[test]
fn strategies_run_on_synthetic_data() {
    runs_on_synthetic_data("strategies");
}

#[test]
fn company_programs_run_on_synthetic_data() {
    runs_on_synthetic_data("company");
}
