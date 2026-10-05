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

/// A CSV market for `equities_1d` with one symbol over five weekdays, with
/// no close on 2024-01-10, next to a strategy that buys on an up day and
/// sells the day after; written under a fresh temporary directory.
fn delisted_market(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("abt-cli-{}-{}", tag, std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let days = ["2024-01-08", "2024-01-09", "2024-01-10", "2024-01-11", "2024-01-12"];
    let prices = [10.0, 11.0, 10.0, 12.0, 13.0];
    let mut close = String::from("A,T,P\n");
    let mut volume = String::from("A,T,V\n");
    let mut universe = String::from("A,T\n");
    for (d, p) in days.iter().zip(prices) {
        if *d != "2024-01-10" {
            close.push_str(&format!("X,{},{}\n", d, p));
        }
        volume.push_str(&format!("X,{},1000\n", d));
        universe.push_str(&format!("X,{}\n", d));
    }
    std::fs::write(dir.join("close.csv"), close).unwrap();
    std::fs::write(dir.join("volume.csv"), volume).unwrap();
    std::fs::write(dir.join("universe.csv"), universe).unwrap();
    std::fs::write(
        dir.join("up_down.dsl"),
        r#"
strategy up_down {
  env equities_1d
  uses features
  resolution @1d
  mode delta
  param qty : Quantity<Shares> = 10 shares
  rel up(-A: Equity, @T: Timestamp)
  up(A, T) :- universe(A, T), logret(A, T, R), R > 0.
  decide(T, buy(A, qty)) :- up(A, T), flat(A, T).
  decide(T, sell(A, Q)) :- held(A, T, Q), prev(T, T0), decided(T0, buy(A, _)).
}
"#,
    )
    .unwrap();
    dir
}

/// portfolio-04, adv-09, trend-14: `abt run` lists every dropped decision
/// with the kernel's reason, including a last-bar decision that has no next
/// bar, so that decisions = fills + dropped is visible; `--quiet` keeps the
/// counts only.
#[test]
fn run_lists_dropped_decisions_with_their_reasons() {
    let dir = delisted_market("dropped");
    let (env, lib) = (corpus("env"), corpus("lib"));
    let strategy = dir.join("up_down.dsl");
    let args = [
        "run",
        "--strategy",
        "up_down",
        "--data",
        dir.to_str().unwrap(),
        env.to_str().unwrap(),
        lib.to_str().unwrap(),
        strategy.to_str().unwrap(),
    ];
    let out = abt(&args);
    let (stdout, stderr) = text(&out);
    assert!(out.status.success(), "stdout: {}\nstderr: {}", stdout, stderr);
    assert!(stdout.contains("decisions: 2   fills: 0   dropped: 2"), "stdout: {}", stdout);
    let dropped: Vec<&str> = stdout.lines().filter(|l| l.trim_start().starts_with("dropped ")).collect();
    assert_eq!(dropped.len(), 2, "stdout: {}", stdout);
    assert!(
        dropped[0].contains("2024-01-09") && dropped[0].contains("buy(X, 10)") && dropped[0].contains("no price for X at 2024-01-10"),
        "{}",
        dropped[0]
    );
    assert!(
        dropped[1].contains("2024-01-12") && dropped[1].contains("buy(X, 10)") && dropped[1].contains("no next bar"),
        "{}",
        dropped[1]
    );

    let mut quiet = args.to_vec();
    quiet.insert(1, "--quiet");
    let out = abt(&quiet);
    let (stdout, _) = text(&out);
    assert!(out.status.success() && stdout.contains("dropped: 2") && !stdout.contains("no price for"), "stdout: {}", stdout);
    let _ = std::fs::remove_dir_all(&dir);
}

/// trend-16: `--all` prints every decision instead of the first twenty, and
/// `--fills` prints the fills.
#[test]
fn run_prints_every_decision_and_the_fills_on_request() {
    let files = strategy_files("sma_crossover");
    let base = ["run", "--strategy", "sma_crossover", "--synthetic", "--seed", "1", "--days", "300"];
    let count = |stdout: &str, key: &str| -> usize {
        let line = stdout.lines().find(|l| l.starts_with("decisions:")).unwrap_or_else(|| panic!("no summary line in {}", stdout));
        let field = line.split_whitespace().skip_while(|w| *w != key).nth(1).unwrap_or_else(|| panic!("no {} in {}", key, line));
        field.parse().unwrap()
    };
    let decision_lines = |stdout: &str| stdout.lines().filter(|l| l.contains("(rule ")).count();

    let mut args = base.to_vec();
    args.extend(files.iter().map(|s| s.as_str()));
    let out = abt(&args);
    let (stdout, stderr) = text(&out);
    assert!(out.status.success(), "stdout: {}\nstderr: {}", stdout, stderr);
    let decisions = count(&stdout, "decisions:");
    let fills = count(&stdout, "fills:");
    assert!(decisions > 20, "the case needs more than twenty decisions: {}", stdout);
    assert_eq!(decision_lines(&stdout), 20, "stdout: {}", stdout);
    assert!(stdout.contains(&format!("... {} more", decisions - 20)), "stdout: {}", stdout);
    assert!(!stdout.contains("  fill "), "stdout: {}", stdout);

    let mut args = base.to_vec();
    args.extend(["--all", "--fills"]);
    args.extend(files.iter().map(|s| s.as_str()));
    let out = abt(&args);
    let (stdout, stderr) = text(&out);
    assert!(out.status.success(), "stdout: {}\nstderr: {}", stdout, stderr);
    assert_eq!(decision_lines(&stdout), decisions, "stdout: {}", stdout);
    assert!(!stdout.contains(" more"), "stdout: {}", stdout);
    let fill_lines = stdout.lines().filter(|l| l.trim_start().starts_with("fill ")).count();
    assert_eq!(fill_lines, fills, "stdout: {}", stdout);
    assert!(stdout.lines().any(|l| l.trim_start().starts_with("fill ") && l.contains(" @ ")), "stdout: {}", stdout);
}

/// trend-09: `--param name=value` (repeatable) overrides a parameter on
/// `abt run` and `abt explain`; a value outside the declared range or of the
/// wrong type is a usage error.
#[test]
fn run_takes_parameter_overrides() {
    let files = strategy_files("sma_crossover");
    let run_with = |extra: &[&str]| {
        let mut args = vec!["run", "--strategy", "sma_crossover", "--synthetic", "--seed", "1", "--days", "300", "--quiet"];
        args.extend(extra);
        args.extend(files.iter().map(|s| s.as_str()));
        abt(&args)
    };
    let out = run_with(&[]);
    let (base, stderr) = text(&out);
    assert!(out.status.success(), "stdout: {}\nstderr: {}", base, stderr);
    let out = run_with(&["--param", "fast=10d", "--param", "qty=250 shares"]);
    let (stdout, stderr) = text(&out);
    assert!(out.status.success(), "stdout: {}\nstderr: {}", stdout, stderr);
    let summary = |s: &str| s.lines().find(|l| l.starts_with("decisions:")).unwrap().to_string();
    assert_ne!(summary(&base), summary(&stdout), "the overrides should change the run:\n{}", stdout);

    let out = run_with(&["--param", "fast=5d"]);
    let (_, stderr) = text(&out);
    assert!(!out.status.success() && stderr.contains("fast") && stderr.contains("10d..60d"), "stderr: {}", stderr);
    let out = run_with(&["--param", "fast=12"]);
    let (_, stderr) = text(&out);
    assert!(!out.status.success() && stderr.contains("fast") && stderr.contains("Duration"), "stderr: {}", stderr);
    let out = run_with(&["--param", "nope=1d"]);
    let (_, stderr) = text(&out);
    assert!(!out.status.success() && stderr.contains("`nope`"), "stderr: {}", stderr);

    let mut args = vec![
        "explain",
        "--strategy",
        "sma_crossover",
        "--rule",
        "decide#1",
        "--at",
        "2022-04-14",
        "--param",
        "fast=10d",
        "--synthetic",
        "--seed",
        "1",
        "--days",
        "120",
    ];
    args.extend(files.iter().map(|s| s.as_str()));
    let out = abt(&args);
    let (stdout, stderr) = text(&out);
    assert!(out.status.success() && stdout.contains("rule sma_crossover::decide#1"), "stdout: {}\nstderr: {}", stdout, stderr);
}

/// Data-bundle doc, section 6: `abt check` prints each strategy's degrees of
/// freedom (params, in-rule literals, rules, reachable library literals), and
/// a ticker literal warns W6 without failing the check.
#[test]
fn check_prints_degrees_of_freedom_per_strategy() {
    let files = strategy_files("momentum_top_n");
    let mut args = vec!["check"];
    args.extend(files.iter().map(|s| s.as_str()));
    let out = abt(&args);
    let (stdout, _) = text(&out);
    assert_eq!(out.status.code(), Some(0), "{}", stdout);
    assert!(
        stdout.contains("momentum_top_n: degrees of freedom: 4 params, 0 literals, 5 rules (+ 2 library literals)"),
        "{}",
        stdout
    );
    let files = strategy_files("relative_strength");
    let mut args = vec!["check"];
    args.extend(files.iter().map(|s| s.as_str()));
    let out = abt(&args);
    let (stdout, _) = text(&out);
    assert_eq!(out.status.code(), Some(0), "a warning does not fail the check:\n{}", stdout);
    assert!(stdout.contains("warning [W6]") && stdout.contains("\"SPY\""), "{}", stdout);
    assert!(stdout.contains("relative_strength: degrees of freedom:"), "{}", stdout);
}
