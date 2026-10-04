//! The corpus is the checker's test suite (spec section 8): every strategy
//! checks clean, and every negative case produces exactly its expected code.

use std::fs;
use std::path::{Path, PathBuf};

use absolute_backtest::check::{check_program, Code, Severity, Workspace};

fn corpus_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("corpus")
}

fn dsl_files(sub: &str) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = fs::read_dir(corpus_dir().join(sub))
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().map(|e| e == "dsl").unwrap_or(false))
        .collect();
    v.sort();
    v
}

/// A workspace holding every environment and library of the corpus.
pub fn base_workspace() -> Workspace {
    let mut ws = Workspace::new();
    for f in dsl_files("env").into_iter().chain(dsl_files("lib")) {
        let src = fs::read_to_string(&f).unwrap();
        ws.add_source(&src).unwrap_or_else(|e| panic!("{}: {}", f.display(), e));
    }
    ws
}

fn strategy_name(src: &str) -> String {
    let units = absolute_backtest::parse_units(src).unwrap();
    units.iter().find(|u| u.kind == absolute_backtest::UnitKind::Strategy).map(|u| u.name.clone()).expect("a strategy unit")
}

#[test]
fn every_strategy_checks_clean() {
    for f in dsl_files("strategies") {
        let src = fs::read_to_string(&f).unwrap();
        let mut ws = base_workspace();
        ws.add_source(&src).unwrap_or_else(|e| panic!("{}: {}", f.display(), e));
        let name = strategy_name(&src);
        let (program, diags) = check_program(&ws, &name);
        let text: Vec<String> = diags.iter().map(|d| d.to_string()).collect();
        assert!(diags.is_empty(), "{} should check clean (no errors, no warnings), got:\n{}", f.display(), text.join("\n"));
        assert!(program.is_some(), "{} should produce a program", f.display());
    }
}

#[test]
fn every_library_checks_clean() {
    let ws = base_workspace();
    for lib in ws.libraries() {
        let diags = absolute_backtest::check::check_library(&ws, &lib.name);
        let text: Vec<String> = diags.iter().map(|d| d.to_string()).collect();
        assert!(diags.is_empty(), "library {} should check clean, got:\n{}", lib.name, text.join("\n"));
    }
}

#[test]
fn every_negative_case_fails_with_its_code() {
    for f in dsl_files("negative") {
        let src = fs::read_to_string(&f).unwrap();
        let header = src.lines().find(|l| l.starts_with("# expect:")).unwrap_or_else(|| panic!("{} has no `# expect:` header", f.display()));
        let code = Code::parse(header.trim_start_matches("# expect:").trim()).unwrap();
        let mut ws = base_workspace();
        ws.add_source(&src).unwrap_or_else(|e| panic!("{}: {}", f.display(), e));
        let name = strategy_name(&src);
        let (program, diags) = check_program(&ws, &name);
        let text: Vec<String> = diags.iter().map(|d| d.to_string()).collect();
        let errors: Vec<&absolute_backtest::check::Diagnostic> = diags.iter().filter(|d| d.severity == Severity::Error).collect();
        assert!(!errors.is_empty(), "{} should fail with {:?}, but checked clean:\n{}", f.display(), code, text.join("\n"));
        assert!(errors.iter().all(|d| d.code == code), "{} should fail only with {:?}, got:\n{}", f.display(), code, text.join("\n"));
        assert!(program.is_none());
    }
}

#[test]
fn negative_cases_cover_every_judgment_code() {
    let mut seen = std::collections::BTreeSet::new();
    for f in dsl_files("negative") {
        let src = fs::read_to_string(&f).unwrap();
        let header = src.lines().find(|l| l.starts_with("# expect:")).unwrap();
        seen.insert(Code::parse(header.trim_start_matches("# expect:").trim()).unwrap());
    }
    for code in [Code::U, Code::E, Code::B, Code::M, Code::T, Code::R, Code::N, Code::F, Code::D, Code::S, Code::Z, Code::C, Code::X] {
        assert!(seen.contains(&code), "no negative case for code {:?}", code);
    }
}
