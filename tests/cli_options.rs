//! The command line rejects malformed option values instead of falling back
//! to a default, refuses an executor setting given as an option (the
//! program's `execution` block owns every one), reports a missing data
//! directory once, documents every option it accepts, and says when
//! `explain` is asked about a bar that is not in the time domain.

mod common;

use std::path::Path;
use std::process::Command;

/// The program files, relative to the crate root the commands run in.
const CATALOG: &str = "corpus-v2/catalog";
const LIB: &str = "corpus-v2/lib";
const SMA: &str = "corpus-v2/strategies/sma_crossover.dsl";

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
        (&["run", "--strategy", "sma_crossover", "--synthetic", "--days", "abc", "--quiet", CATALOG, LIB, SMA], "--days", "abc"),
        (&["run", "--strategy", "sma_crossover", "--synthetic", "--seed", "x", "--quiet", CATALOG, LIB, SMA], "--seed", "x"),
        (
            &["run", "--strategy", "sma_crossover", "--synthetic", "--as-of", "yesterday", "--quiet", CATALOG, LIB, SMA],
            "--as-of",
            "yesterday",
        ),
        (
            &["run", "--strategy", "sma_crossover", "--synthetic", "--start", "soon", "--quiet", CATALOG, LIB, SMA],
            "--start",
            "soon",
        ),
        (&["run", "--strategy", "sma_crossover", "--synthetic", "--end", "never", "--quiet", CATALOG, LIB, SMA], "--end", "never"),
        (
            &["explain", "--strategy", "sma_crossover", "--rule", "decide#1", "--at", "yesterday", "--synthetic", CATALOG, LIB, SMA],
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

/// Language-v2, 4.3 and 4.5: the executor's fields and the reporting are
/// the program's `execution` and `report` blocks; the options that used to
/// set them are unknown, refused before anything runs, and the refusal
/// says where the setting now lives.
#[test]
fn an_executor_setting_is_not_an_option() {
    for (opt, value) in [
        ("--cash", "1000"),
        ("--commission", "0"),
        ("--slippage-bps", "1"),
        ("--margin", "reg-t"),
        ("--price-relation", "close"),
        ("--lot", "fractional"),
        ("--report-by", "day"),
    ] {
        let (code, stdout, stderr) = abt(&["run", "--strategy", "sma_crossover", "--synthetic", opt, value, "--quiet", CATALOG, LIB, SMA]);
        assert_eq!(code, 1, "{} {}\nstdout: {}\nstderr: {}", opt, value, stdout, stderr);
        assert!(stderr.contains(&format!("unknown option {}", opt)) && stderr.contains("execution block"), "{}: {}", opt, stderr);
        assert!(!stdout.contains("strategy sma_crossover over"), "{} ran anyway:\n{}", opt, stdout);
    }
}

#[test]
fn an_empty_symbol_list_is_rejected() {
    for symbols in ["", " ", "AAA,,BBB"] {
        let (code, stdout, stderr) = abt(&["run", "--strategy", "sma_crossover", "--synthetic", "--symbols", symbols, "--quiet", CATALOG, LIB, SMA]);
        assert_eq!(code, 2, "--symbols {:?}\nstdout: {}\nstderr: {}", symbols, stdout, stderr);
        assert!(stderr.contains("--symbols"), "{}", stderr);
    }
}

#[test]
fn a_missing_data_directory_is_one_error() {
    let dir = std::env::temp_dir().join(format!("abt-cli-{}-nonexistent", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let dir = dir.to_string_lossy().into_owned();
    let (code, _, stderr) = abt(&["run", "--strategy", "sma_crossover", "--data", &dir, CATALOG, LIB, SMA]);
    assert_eq!(code, 2, "{}", stderr);
    let lines: Vec<&str> = stderr.lines().collect();
    assert_eq!(lines.len(), 1, "one error, not one note per file:\n{}", stderr);
    assert!(lines[0].contains("directory not found") && lines[0].contains(&dir), "{}", stderr);
}

/// The usage names every option that remains (the run's data, window and
/// bundle date, the compiler's choices, the outputs and the commands'
/// arguments) and none of the executor's fields.
#[test]
fn usage_documents_every_option() {
    let (code, _, stderr) = abt(&[]);
    assert_eq!(code, 1);
    for opt in [
        "--strategy",
        "--data",
        "--synthetic",
        "--days",
        "--symbols",
        "--seed",
        "--start",
        "--end",
        "--as-of",
        "--param",
        "--plan",
        "--kernel",
        "--verify-causality",
        "--all",
        "--fills",
        "--nav",
        "--returns",
        "--dump",
        "--tables",
        "--ledger",
        "--ledger-out",
        "--study",
        "--quiet",
        "--timing",
        "--rule",
        "--at",
        "--bind",
        "--bundle",
        "--untested",
        "--checkpoint-every",
        "--checkpoint-dir",
        "--resume",
        "--class",
        "--version",
        "--out",
        "--from",
        "--from-norgate",
        "--from-databento",
        "--processing-delay",
    ] {
        assert!(stderr.contains(opt), "usage lacks {}:\n{}", opt, stderr);
    }
    for gone in [
        "--cash",
        "--capital",
        "--commission",
        "--slippage-bps",
        "--margin",
        "--price-relation",
        "--lot",
        "--haircut",
        "--report-by",
        "--periods-per-year",
    ] {
        assert!(!stderr.contains(gone), "usage still offers {} (the execution and report blocks own it):\n{}", gone, stderr);
    }
    assert!(stderr.contains("`execution` block") && stderr.contains("`report` block"), "{}", stderr);
}

/// The execution block's `price` names a relation with a Price value; one
/// without is refused by the checker (WF-14) before any order could be
/// silently dropped.
#[test]
fn an_execution_price_without_a_price_value_is_rejected() {
    let dir = std::env::temp_dir().join(format!("abt-cli-options-price-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let src = common::StrategyParts {
        name: "priced".into(),
        params: "    qty : Quantity<Shares> = 10 shares".into(),
        rules: "    rel up(A: Equity, @T)\n    up(A, T) :- universe(A, T), price_return(A, T, R), R > 0.".into(),
        decides: "    decide(T, trade(A, qty)) :- up(A, T), flat(A, T).".into(),
        execution: "equity_default with { price volume }".into(),
        ..Default::default()
    }
    .render();
    let path = dir.join("priced.dsl");
    std::fs::write(&path, src).unwrap();
    let path = path.to_string_lossy().into_owned();
    let (code, stdout, stderr) = abt(&["run", "--strategy", "priced", "--synthetic", "--days", "100", "--quiet", CATALOG, LIB, &path]);
    assert_ne!(code, 0, "stdout: {}", stdout);
    assert!(stderr.contains("volume") && stderr.contains("Price"), "{}", stderr);
    assert!(!stdout.contains("dropped:"), "the run started and orders were dropped:\n{}", stdout);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn explain_outside_the_time_domain_says_so() {
    let (code, stdout, stderr) = abt(&[
        "explain",
        "--strategy",
        "sma_crossover",
        "--rule",
        "decide#1",
        "--at",
        "2030-01-01",
        "--synthetic",
        "--days",
        "300",
        CATALOG,
        LIB,
        SMA,
    ]);
    assert_eq!(code, 1, "stdout: {}\nstderr: {}", stdout, stderr);
    assert!(stderr.contains("2030-01-01") && stderr.contains("not a bar"), "{}", stderr);
    assert!(
        !stdout.contains("literal") && !stderr.contains("literal"),
        "a missing bar is not a missing literal:\n{}{}",
        stdout,
        stderr
    );
    // The synthetic market's last bar (300 weekdays from 2022-01-03) is in
    // the domain and still explains.
    let (code, stdout, _) = abt(&[
        "explain",
        "--strategy",
        "sma_crossover",
        "--rule",
        "decide#1",
        "--at",
        "2023-02-24",
        "--synthetic",
        "--days",
        "300",
        CATALOG,
        LIB,
        SMA,
    ]);
    assert_eq!(code, 0);
    assert!(stdout.contains("decide#1") && stdout.contains("2023-02-24"), "{}", stdout);
    assert!(Path::new(env!("CARGO_MANIFEST_DIR")).join(CATALOG).is_dir());
}

#[test]
fn timing_prints_one_stderr_line_with_every_field() {
    let (code, stdout, stderr) = abt(&["run", "--strategy", "sma_crossover", "--synthetic", "--days", "60", "--timing", "--quiet", CATALOG, LIB, SMA]);
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
    let (_, _, stderr) = abt(&["run", "--strategy", "sma_crossover", "--synthetic", "--days", "60", "--quiet", CATALOG, LIB, SMA]);
    assert!(!stderr.contains("timing "), "{}", stderr);
}
