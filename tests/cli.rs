//! The `abt` command line, driven as a subprocess over the corpus.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn corpus(sub: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("corpus").join(sub)
}

fn abt(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_abt")).args(args).output().expect("abt runs")
}

fn text(out: &Output) -> (String, String) {
    (String::from_utf8_lossy(&out.stdout).to_string(), String::from_utf8_lossy(&out.stderr).to_string())
}

fn strategy_files(name: &str) -> Vec<String> {
    vec![
        corpus("env").to_string_lossy().to_string(),
        corpus("lib").to_string_lossy().to_string(),
        corpus(&format!("strategies/{}.dsl", name)).to_string_lossy().to_string(),
    ]
}

/// trend-10: `--at` on a Saturday is refused, naming the nearest bars, with a
/// non-zero exit.
#[test]
fn explain_at_a_non_bar_fails_naming_the_nearest_bars() {
    let files = strategy_files("breakout_52w");
    let mut args = vec![
        "explain",
        "--strategy",
        "breakout_52w",
        "--rule",
        "decide#1",
        "--at",
        "2022-04-16",
        "--synthetic",
        "--days",
        "120",
        "--symbols",
        "AAA,BBB",
    ];
    args.extend(files.iter().map(|s| s.as_str()));
    let out = abt(&args);
    let (stdout, stderr) = text(&out);
    assert!(!out.status.success(), "stdout: {}\nstderr: {}", stdout, stderr);
    assert!(stderr.contains("2022-04-16 is not a bar at @1d"), "stderr: {}", stderr);
    assert!(stderr.contains("2022-04-15") && stderr.contains("2022-04-18"), "stderr: {}", stderr);
    assert!(!stdout.contains("did not fire"), "stdout: {}", stdout);
}

/// `abt explain` on trailing_stop over a small synthetic market, with extra
/// options placed before the files.
fn explain_trailing_stop(rule: &str, extra: &[&str]) -> Output {
    let files = strategy_files("trailing_stop");
    let mut args = vec![
        "explain",
        "--strategy",
        "trailing_stop",
        "--rule",
        rule,
        "--at",
        "2022-04-14",
        "--synthetic",
        "--days",
        "120",
        "--symbols",
        "AAA,BBB",
    ];
    args.extend(extra);
    args.extend(files.iter().map(|s| s.as_str()));
    abt(&args)
}

/// trend-05: a library rule with `+` arguments is explained with `--inputs`;
/// without them the CLI says which inputs the rule takes.
#[test]
fn explain_takes_inputs_for_a_rule_with_input_arguments() {
    let out = explain_trailing_stop("features::sma#1", &[]);
    let (stdout, stderr) = text(&out);
    assert!(!out.status.success(), "stdout: {}\nstderr: {}", stdout, stderr);
    assert!(!stderr.contains("internal"), "stderr: {}", stderr);
    assert!(stderr.contains("3 inputs") && stderr.contains("A: Equity") && stderr.contains("--inputs"), "stderr: {}", stderr);

    let out = explain_trailing_stop("features::sma#1", &["--inputs", "AAA,20d,10"]);
    let (stdout, stderr) = text(&out);
    assert!(out.status.success(), "stdout: {}\nstderr: {}", stdout, stderr);
    assert!(stdout.contains("rule features::sma#1 fired at 2022-04-14 with 1 solution(s)"), "stdout: {}", stdout);

    // A wrong count, or a value that is not of the input's type, is a usage error.
    let out = explain_trailing_stop("features::sma#1", &["--inputs", "AAA,20d"]);
    let (_, stderr) = text(&out);
    assert!(!out.status.success() && stderr.contains("3 inputs"), "stderr: {}", stderr);
    let out = explain_trailing_stop("features::sma#1", &["--inputs", "AAA,20d,ten"]);
    let (_, stderr) = text(&out);
    assert!(!out.status.success() && stderr.contains("`ten`") && stderr.contains("Count"), "stderr: {}", stderr);
}

/// trend-04: `--bind VAR=value` narrows a decide rule to one instrument.
#[test]
fn explain_binds_a_body_variable() {
    let out = explain_trailing_stop("decide#1", &["--bind", "A=AAA"]);
    let (stdout, stderr) = text(&out);
    assert!(out.status.success(), "stdout: {}\nstderr: {}", stdout, stderr);
    assert!(stdout.contains("rule trailing_stop::decide#1") && stdout.contains("A = AAA"), "stdout: {}", stdout);

    let out = explain_trailing_stop("decide#1", &["--bind", "A=ZZZ"]);
    let (_, stderr) = text(&out);
    assert!(!out.status.success() && stderr.contains("ZZZ"), "stderr: {}", stderr);
}
