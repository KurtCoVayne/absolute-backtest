//! The LLM-authored corpus harness (data-bundle doc, section 8): briefs
//! with a model's successive attempts are measured, never asserted: the
//! first-attempt pass rate, the diagnostics, the attempts to a valid
//! program and the verdict of the first valid one on the synthetic market.

mod corpus;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use absolute_backtest::study::briefs::report;

fn temp_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("abt-briefs-{}-{}", tag, std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn corpus_file(name: &str) -> String {
    fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join(format!("corpus/strategies/{}.dsl", name))).unwrap()
}

const UNBOUND: &str = r#"
strategy first_try {
  env equities_1d
  uses features
  resolution @1d
  mode delta
  param qty : Quantity<Shares> = 100 shares
  decide(T, buy(A, qty)) :- universe(A, T), flat(A, T), Z > 0.
}
"#;

fn fixture(tag: &str) -> (PathBuf, PathBuf, PathBuf) {
    let root = temp_dir(tag);
    let briefs = root.join("briefs");
    let attempts = root.join("attempts");
    fs::create_dir_all(briefs.join("")).unwrap();
    fs::write(briefs.join("README.md"), "procedure").unwrap();
    fs::write(briefs.join("crossover.md"), "A moving-average crossover.\nDaily.").unwrap();
    fs::write(briefs.join("momentum.md"), "Top momentum names, equal weight.").unwrap();
    fs::write(briefs.join("untried.md"), "Nobody has tried this one.").unwrap();
    fs::create_dir_all(attempts.join("crossover")).unwrap();
    fs::write(attempts.join("crossover/1.dsl"), UNBOUND).unwrap();
    fs::write(attempts.join("crossover/2.dsl"), "this is not a program").unwrap();
    fs::write(attempts.join("crossover/3.dsl"), corpus_file("sma_crossover")).unwrap();
    fs::create_dir_all(attempts.join("momentum")).unwrap();
    fs::write(attempts.join("momentum/1.dsl"), corpus_file("momentum_top_n")).unwrap();
    (root, briefs, attempts)
}

#[test]
fn the_harness_records_attempts_diagnostics_and_verdicts() {
    let (root, briefs, attempts) = fixture("harness");
    let ws = corpus::base_workspace();
    let r = report(&ws, &briefs, &attempts).unwrap();
    assert_eq!(r.briefs.len(), 3, "the README is not a brief");
    let crossover = r.briefs.iter().find(|b| b.brief == "crossover").unwrap();
    assert_eq!(crossover.attempts.len(), 3);
    assert!(!crossover.first_attempt_valid);
    assert_eq!(crossover.attempts_to_valid, Some(3));
    assert!(crossover.attempts[0].diagnostics.iter().any(|(code, ..)| code == "B"), "{:?}", crossover.attempts[0]);
    assert!(crossover.attempts[1].parse_error.is_some());
    assert!(crossover.attempts[2].valid);
    let v = crossover.verdict.as_ref().unwrap();
    assert_eq!(v.strategy, "sma_crossover");
    assert!(v.fills > 0 && v.bars == 320);
    assert!(v.verdict == "trades" || v.verdict == "does not survive costs", "{}", v.verdict);
    let momentum = r.briefs.iter().find(|b| b.brief == "momentum").unwrap();
    assert!(momentum.first_attempt_valid);
    assert_eq!(momentum.attempts_to_valid, Some(1));
    let untried = r.briefs.iter().find(|b| b.brief == "untried").unwrap();
    assert!(untried.attempts.is_empty() && untried.verdict.is_none());
    assert!((r.first_attempt_pass_rate - 0.5).abs() < 1e-12);
    assert_eq!(r.mean_attempts_to_valid, Some(2.0));
    assert_eq!(r.briefs_without_a_valid_attempt, 0);
    assert_eq!(r.diagnostics.get("B").copied().unwrap_or(0), 1);
    assert_eq!(r.diagnostics.get("parse").copied().unwrap_or(0), 1);
    assert_eq!(r.verdicts.values().sum::<usize>(), 2);
    let text = r.render();
    assert!(text.contains("first-attempt pass rate: 50%") && text.contains("attempt 2 (2.dsl): invalid; parse error"), "{}", text);
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn the_command_line_prints_and_writes_the_report() {
    let (root, briefs, attempts) = fixture("cli");
    let out = root.join("report.json");
    let o = Command::new(env!("CARGO_BIN_EXE_abt"))
        .args([
            "briefs",
            "report",
            "--briefs",
            &briefs.to_string_lossy(),
            "--attempts",
            &attempts.to_string_lossy(),
            "--out",
            &out.to_string_lossy(),
            "corpus/",
        ])
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&o.stdout);
    assert_eq!(o.status.code(), Some(0), "{}\n{}", stdout, String::from_utf8_lossy(&o.stderr));
    assert!(stdout.contains("briefs: 3 (2 with attempts)") && stdout.contains("verdicts:"), "{}", stdout);
    let json: serde_json::Value = serde_json::from_str(&fs::read_to_string(&out).unwrap()).unwrap();
    assert_eq!(json["briefs"].as_array().unwrap().len(), 3);
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn the_repository_briefs_are_listed_without_attempts() {
    let ws = corpus::base_workspace();
    let briefs = Path::new(env!("CARGO_MANIFEST_DIR")).join("briefs");
    let r = report(&ws, &briefs, &briefs.join("attempts")).unwrap();
    assert!(r.briefs.len() >= 8, "{} briefs", r.briefs.len());
    assert!(r.briefs.iter().all(|b| b.attempts.is_empty()));
}
