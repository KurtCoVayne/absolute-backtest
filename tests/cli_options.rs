//! The command line rejects malformed options instead of falling back to a
//! default, reports a missing data directory once, documents every option
//! it accepts, and says when `explain` is asked about a bar that is not in
//! the time domain.

use std::path::Path;
use std::process::Command;

fn abt(args: &[&str]) -> (i32, String, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_abt")).args(args).current_dir(env!("CARGO_MANIFEST_DIR")).output().expect("abt runs");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

#[test]
fn malformed_option_values_are_errors_naming_the_option() {
    let cases: &[(&[&str], &str, &str)] = &[
        (&["run", "--strategy", "sma_crossover", "--synthetic", "--days", "abc", "--quiet", "corpus/"], "--days", "abc"),
        (&["run", "--strategy", "sma_crossover", "--synthetic", "--seed", "x", "--quiet", "corpus/"], "--seed", "x"),
        (&["run", "--strategy", "sma_crossover", "--synthetic", "--cash", "lots", "--quiet", "corpus/"], "--cash", "lots"),
        (
            &["run", "--strategy", "sma_crossover", "--synthetic", "--slippage-bps", "1e", "--quiet", "corpus/"],
            "--slippage-bps",
            "1e",
        ),
        (&["run", "--strategy", "sma_crossover", "--synthetic", "--commission", "-", "--quiet", "corpus/"], "--commission", "-"),
        (
            &["explain", "--strategy", "sma_crossover", "--rule", "decide#1", "--at", "yesterday", "--synthetic", "corpus/"],
            "--at",
            "yesterday",
        ),
    ];
    for (args, opt, value) in cases {
        let (code, stdout, stderr) = abt(args);
        assert_eq!(code, 2, "{:?}\nstdout: {}\nstderr: {}", args, stdout, stderr);
        assert!(stderr.contains(opt) && stderr.contains(value), "{:?}: stderr should name {} and `{}`:\n{}", args, opt, value, stderr);
        assert!(!stdout.contains("strategy sma_crossover over"), "{:?} ran anyway:\n{}", args, stdout);
    }
}

#[test]
fn an_empty_symbol_list_is_rejected() {
    for symbols in ["", " ", "AAA,,BBB"] {
        let (code, stdout, stderr) = abt(&["run", "--strategy", "sma_crossover", "--synthetic", "--symbols", symbols, "--quiet", "corpus/"]);
        assert_eq!(code, 2, "--symbols {:?}\nstdout: {}\nstderr: {}", symbols, stdout, stderr);
        assert!(stderr.contains("--symbols"), "{}", stderr);
    }
}

#[test]
fn a_missing_data_directory_is_one_error() {
    let dir = std::env::temp_dir().join(format!("abt-cli-{}-nonexistent", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let dir = dir.to_string_lossy().into_owned();
    let (code, _, stderr) = abt(&["run", "--strategy", "sma_crossover", "--data", &dir, "corpus/"]);
    assert_eq!(code, 2, "{}", stderr);
    let lines: Vec<&str> = stderr.lines().collect();
    assert_eq!(lines.len(), 1, "one error, not one note per file:\n{}", stderr);
    assert!(lines[0].contains("directory not found") && lines[0].contains(&dir), "{}", stderr);
}

#[test]
fn usage_documents_every_option() {
    let (code, _, stderr) = abt(&[]);
    assert_eq!(code, 1);
    for opt in [
        "--price-relation",
        "--quiet",
        "--verify-causality",
        "--days",
        "--symbols",
        "--seed",
        "--cash",
        "--slippage-bps",
        "--commission",
        "--on-leverage",
        "--on-oversize",
        "--on-ruin",
        "--lot",
        "--commission-min",
        "--fee-bps",
        "--slippage-vol",
        "--vol-window",
        "--frictionless",
        "--participation",
        "--impact",
        "--adv-window",
        "--volume-relation",
        "--margin",
        "--max-gross",
        "--maintenance",
        "--on-margin-call",
        "--cash-rate",
        "--margin-rate",
        "--short-rebate",
        "--nav",
        "--as-of",
        "--haircut",
        "--bundle",
        "--untested",
        "--kernel",
        "--checkpoint-every",
        "--checkpoint-dir",
        "--resume",
    ] {
        assert!(stderr.contains(opt), "usage lacks {}:\n{}", opt, stderr);
    }
}

#[test]
fn a_price_relation_without_a_price_column_is_rejected() {
    let (code, stdout, stderr) = abt(&["run", "--strategy", "sma_crossover", "--synthetic", "--price-relation", "volume", "--days", "100", "--quiet", "corpus/"]);
    assert_ne!(code, 0, "stdout: {}", stdout);
    assert!(stderr.contains("volume") && stderr.contains("Price"), "{}", stderr);
    assert!(!stdout.contains("dropped: 8"), "orders were silently dropped:\n{}", stdout);
}

#[test]
fn explain_outside_the_time_domain_says_so() {
    let (code, stdout, stderr) = abt(&["explain", "--strategy", "sma_crossover", "--rule", "decide#1", "--at", "2030-01-01", "--synthetic", "corpus/"]);
    assert_eq!(code, 1, "stdout: {}\nstderr: {}", stdout, stderr);
    assert!(stderr.contains("2030-01-01") && stderr.contains("not a bar"), "{}", stderr);
    assert!(
        !stdout.contains("literal") && !stderr.contains("literal"),
        "a missing bar is not a missing literal:\n{}{}",
        stdout,
        stderr
    );
    // The synthetic market's last bar is in the domain and still explains.
    let (code, stdout, _) = abt(&["explain", "--strategy", "sma_crossover", "--rule", "decide#1", "--at", "2023-02-24", "--synthetic", "corpus/"]);
    assert_eq!(code, 0);
    assert!(stdout.contains("decide#1") && stdout.contains("2023-02-24"), "{}", stdout);
    assert!(Path::new(env!("CARGO_MANIFEST_DIR")).join("corpus").is_dir());
}

#[test]
fn timing_prints_one_stderr_line_with_every_field() {
    let (code, stdout, stderr) = abt(&["run", "--strategy", "sma_crossover", "--synthetic", "--days", "60", "--timing", "--quiet", "corpus/"]);
    assert_eq!(code, 0, "{}\n{}", stdout, stderr);
    let lines: Vec<&str> = stderr.lines().filter(|l| l.starts_with("timing ")).collect();
    assert_eq!(lines.len(), 1, "{}", stderr);
    let fields: Vec<(&str, &str)> = lines[0]["timing ".len()..].split(' ').filter_map(|kv| kv.split_once('=')).collect();
    let names: Vec<&str> = fields.iter().map(|(k, _)| *k).collect();
    for want in [
        "load_s",
        "build_s",
        "run_s",
        "total_s",
        "bars",
        "symbols",
        "tuples",
        "decisions",
        "bars_per_s",
        "peak_rss_mb",
        "window_calls",
        "window_bars_solved",
        "window_groups",
        "window_rows_cached",
        "late_tuples",
    ] {
        assert!(names.contains(&want), "timing lacks {}: {}", want, lines[0]);
    }
    let get = |k: &str| fields.iter().find(|(n, _)| *n == k).unwrap().1;
    assert_eq!(get("bars"), "60", "{}", lines[0]);
    assert!(get("tuples").parse::<usize>().unwrap() > 0, "{}", lines[0]);
    for k in ["load_s", "build_s", "run_s", "total_s"] {
        assert!(get(k).parse::<f64>().unwrap() >= 0.0, "{}", lines[0]);
    }
    let rss = get("peak_rss_mb");
    assert!(rss == "unavailable" || rss.parse::<f64>().unwrap() > 0.0, "{}", lines[0]);
    // Without the flag there is no timing line.
    let (_, _, stderr) = abt(&["run", "--strategy", "sma_crossover", "--synthetic", "--days", "60", "--quiet", "corpus/"]);
    assert!(!stderr.contains("timing "), "{}", stderr);
}
