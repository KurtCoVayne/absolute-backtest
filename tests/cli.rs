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
    let mut args = vec!["explain", "--strategy", "breakout_52w", "--rule", "decide#1", "--at", "2022-04-16", "--synthetic", "--days", "120", "--symbols", "AAA,BBB"];
    args.extend(files.iter().map(|s| s.as_str()));
    let out = abt(&args);
    let (stdout, stderr) = text(&out);
    assert!(!out.status.success(), "stdout: {}\nstderr: {}", stdout, stderr);
    assert!(stderr.contains("2022-04-16 is not a bar at @1d"), "stderr: {}", stderr);
    assert!(stderr.contains("2022-04-14") && stderr.contains("2022-04-18"), "stderr: {}", stderr);
    assert!(!stdout.contains("did not fire"), "stdout: {}", stdout);
}
