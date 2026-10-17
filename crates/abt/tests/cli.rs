//! The `abt` command line, driven as a subprocess over the corpus-v2
//! catalog, standard library and strategies. The executor is the program's
//! `execution` block and the reporting its `report` block, so a test of an
//! executor setting writes a small strategy into a temporary directory and
//! runs that.

use abt_tests as common;

use std::path::PathBuf;
use std::process::{Command, Output};

fn corpus(sub: &str) -> PathBuf {
    common::corpus_dir().join(sub)
}

fn abt(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_abt")).args(args).output().expect("abt runs")
}

fn text(out: &Output) -> (String, String) {
    (String::from_utf8_lossy(&out.stdout).to_string(), String::from_utf8_lossy(&out.stderr).to_string())
}

/// The catalog classes, the standard library (directories are read
/// recursively) and one corpus strategy, as the program files of a command.
fn strategy_files(name: &str) -> Vec<String> {
    vec![
        corpus("catalog").to_string_lossy().to_string(),
        corpus("lib").to_string_lossy().to_string(),
        corpus(&format!("strategies/{}.dsl", name)).to_string_lossy().to_string(),
    ]
}

/// A fresh temporary directory for one test (tests run in parallel, so the
/// tag tells them apart).
fn scratch(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("abt-cli-{}-{}", tag, std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// A daily strategy that buys a name on an up day and sells it the day
/// after, with the given `up` rule body and execution block; written to
/// `dir/<name>.dsl`, whose path is returned.
fn up_down_strategy(dir: &std::path::Path, name: &str, up_body: &str, execution: &str) -> String {
    let src = common::StrategyParts {
        name: name.into(),
        params: "    qty : Quantity<Shares> = 10 shares".into(),
        rules: format!("    rel up(A: Equity, @T)\n    up(A, T) :- {}.", up_body),
        decides: "    decide(T, trade(A, qty)) :- up(A, T), flat(A, T).\n    decide(T, trade(A, Q)) :- held(A, T, Q0), prev(T, T0), decided(T0, trade(A, Q1)), Q1 > 0 shares, Q = -Q0.".into(),
        execution: execution.into(),
        ..Default::default()
    }
    .render()
    // The fixtures are loose Parquet directories, which no reviewer vouched
    // for (K-2.1): the program admits them, warned.
    .replace("require reviewed", "allow unreviewed");
    let path = dir.join(format!("{}.dsl", name));
    std::fs::write(&path, src).unwrap();
    path.to_string_lossy().into_owned()
}

/// trend-10: a query at a Saturday is refused, naming the nearest bars, with
/// a non-zero exit.
#[test]
fn a_query_at_a_non_bar_fails_naming_the_nearest_bars() {
    let files = strategy_files("breakout_52w");
    let mut args = vec![
        "query",
        "--strategy",
        "breakout_52w",
        "--text",
        "trace decide at 2022-04-16",
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
    assert!(stderr.contains("2022-04-16 is not a bar"), "stderr: {}", stderr);
    assert!(stderr.contains("2022-04-15") && stderr.contains("2022-04-18"), "stderr: {}", stderr);
    assert!(!stdout.contains("trace("), "stdout: {}", stdout);
}

/// `abt query` on trailing_stop over a small synthetic market, with extra
/// options placed before the files.
fn query_trailing_stop(query: &str, extra: &[&str]) -> Output {
    let files = strategy_files("trailing_stop");
    let mut args = vec!["query", "--strategy", "trailing_stop", "--text", query, "--synthetic", "--days", "120", "--symbols", "AAA,BBB"];
    args.extend(extra);
    args.extend(files.iter().map(|s| s.as_str()));
    abt(&args)
}

/// trend-05: `trace` of a library relation lists its rules by their
/// `unit::head#n` labels; a relation the program does not have is refused
/// naming it; the retired `explain` says what replaced it.
#[test]
fn a_trace_names_a_library_relations_rules_by_their_labels() {
    let out = query_trailing_stop("trace high_since_entry at 2022-04-14", &[]);
    let (stdout, stderr) = text(&out);
    assert!(out.status.success(), "stdout: {}\nstderr: {}", stdout, stderr);
    assert!(stdout.contains("state::high_since_entry#1") && stdout.contains("2022-04-14"), "stdout: {}", stdout);

    let out = query_trailing_stop("trace decide at 2022-04-14", &["--inputs", "AAA,20d"]);
    let (_, stderr) = text(&out);
    assert!(!out.status.success() && stderr.contains("unknown option") && stderr.contains("--inputs"), "stderr: {}", stderr);

    let out = query_trailing_stop("trace nope at 2022-04-14", &[]);
    let (_, stderr) = text(&out);
    assert!(!out.status.success() && stderr.contains("`nope`"), "stderr: {}", stderr);
}

/// trend-04: `for A = KEY` binds a variable of the query to one instrument;
/// the synthetic market's securities are `E1..` carrying the tickers, and
/// the kernel shows a security by its identifier. An unknown key is
/// refused naming it.
#[test]
fn a_query_binds_a_variable() {
    let out = query_trailing_stop("? momentum(A, T, M) for A = AAA over 2022-04-11 .. 2022-04-14", &[]);
    let (stdout, stderr) = text(&out);
    assert!(out.status.success(), "stdout: {}\nstderr: {}", stdout, stderr);
    assert!(stdout.contains("4 row(s)") && stdout.contains("trailing_stop::momentum#1"), "stdout: {}", stdout);
    let rows: Vec<&str> = stdout.lines().filter(|l| l.contains("2022-04-1")).collect();
    assert!(rows.len() == 4 && rows.iter().all(|l| l.trim_start().starts_with("E1 ")), "stdout: {}", stdout);

    let out = query_trailing_stop("? momentum(A, T, M) for A = ZZZ over 2022-04-11 .. 2022-04-14", &[]);
    let (_, stderr) = text(&out);
    assert!(!out.status.success() && stderr.contains("ZZZ"), "stderr: {}", stderr);
}

/// A Parquet market for the equity class with one symbol over five
/// weekdays, with no close on 2024-01-10 (the bar exists: its volume and
/// listing do), next to a strategy that buys on an up day against the
/// previous bar's close and sells the day after; written under a fresh
/// temporary directory.
fn delisted_market(tag: &str) -> PathBuf {
    let dir = scratch(tag);
    let days = ["2024-01-08", "2024-01-09", "2024-01-10", "2024-01-11", "2024-01-12"];
    let prices = [10.0, 11.0, 10.0, 12.0, 13.0];
    // A relation's columns are named after its signature (`close(A, @T, P)`).
    let mut bars = String::new();
    let mut volume = String::from("A,T,V\n");
    let mut listed = String::from("A,T\n");
    for (d, p) in days.iter().zip(prices) {
        if *d != "2024-01-10" {
            bars.push_str(&format!("X,{},{}\n", d, p));
        }
        volume.push_str(&format!("X,{},1000\n", d));
        listed.push_str(&format!("X,{}\n", d));
    }
    for (rel, col) in [("open", "O"), ("high", "H"), ("low", "L"), ("close", "P")] {
        abt_kernel::table::write_text_table(&dir.join(format!("{}.parquet", rel)), &format!("A,T,{}\n{}", col, bars)).unwrap();
    }
    abt_kernel::table::write_text_table(&dir.join("volume.parquet"), &volume).unwrap();
    abt_kernel::table::write_text_table(&dir.join("listed.parquet"), &listed).unwrap();
    up_down_strategy(&dir, "up_down", "universe(A, T), close(A, T, P), prev(T, T0), close(A, T0, P0), P > P0", "equity_default");
    dir
}

/// A directory of Parquet files imported into a store beside it, as a run
/// reads its data (`abt data ingest --adapter bundle_v2`); the store.
fn imported(dir: &std::path::Path, extra: &[&str]) -> PathBuf {
    let store = dir.join("store");
    let _ = std::fs::remove_dir_all(&store);
    let (catalog, lib) = (corpus("catalog"), corpus("lib"));
    let mut args = vec![
        "data",
        "ingest",
        "--store",
        store.to_str().unwrap(),
        "--adapter",
        "bundle_v2",
        "--from",
        dir.to_str().unwrap(),
        "--class",
        "equity",
    ];
    args.extend_from_slice(extra);
    args.extend([catalog.to_str().unwrap(), lib.to_str().unwrap()]);
    let out = abt(&args);
    assert!(out.status.success(), "{}\n{}", text(&out).0, text(&out).1);
    store
}

/// portfolio-04, adv-09, trend-14: `abt run` lists every dropped decision
/// with the kernel's reason, including a last-bar decision that has no next
/// bar, so that decisions = fills + dropped is visible; `--quiet` keeps the
/// counts only.
#[test]
fn run_lists_dropped_decisions_with_their_reasons() {
    let dir = delisted_market("dropped");
    let store = imported(&dir, &[]);
    let (catalog, lib) = (corpus("catalog"), corpus("lib"));
    let strategy = dir.join("up_down.dsl");
    let args = [
        "run",
        "--strategy",
        "up_down",
        "--data",
        store.to_str().unwrap(),
        catalog.to_str().unwrap(),
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
        dropped[0].contains("2024-01-09") && dropped[0].contains("trade(X, 10)") && dropped[0].contains("no price for X at 2024-01-10"),
        "{}",
        dropped[0]
    );
    assert!(
        dropped[1].contains("2024-01-12") && dropped[1].contains("trade(X, 10)") && dropped[1].contains("no next bar"),
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

/// K-3.7: `abt contract` prints the contract derived from the program, its
/// static clauses proven and the others open; with data, a run's status of
/// every clause; `--json` the clauses as records.
#[test]
fn contract_prints_the_clauses_and_a_runs_statuses() {
    let files = strategy_files("sma_crossover");
    let mut args = vec!["contract", "--strategy", "sma_crossover"];
    args.extend(files.iter().map(|s| s.as_str()));
    let out = abt(&args);
    let (stdout, stderr) = text(&out);
    assert!(out.status.success(), "{}{}", stdout, stderr);
    for want in ["pre.prices", "inv.nav", "post.decisions", "proven(tier 0)", "open"] {
        assert!(stdout.contains(want), "lacks {}:\n{}", want, stdout);
    }
    let mut args = vec!["contract", "--strategy", "sma_crossover", "--synthetic", "--days", "120", "--json"];
    args.extend(files.iter().map(|s| s.as_str()));
    let out = abt(&args);
    let (stdout, stderr) = text(&out);
    assert!(out.status.success(), "{}{}", stdout, stderr);
    let v: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    let clauses = v["clauses"].as_array().unwrap();
    assert_eq!(clauses.len(), 33);
    let nav = clauses.iter().find(|c| c["id"] == "inv.nav").unwrap();
    assert_eq!(nav["status"]["status"], "checked", "{}", nav);
    assert!(clauses.iter().all(|c| c["status"]["status"] != "open" && c["status"]["status"] != "violated"), "{}", stdout);
}

/// K-4.5: `--no-pushdown` evaluates every aggregate in the kernel; the run is
/// the same, decision for decision and fill for fill, and only the count of
/// the data layer's answers differs.
#[test]
fn no_pushdown_is_the_same_run() {
    let files = strategy_files("sma_crossover");
    let run = |extra: &[&str]| {
        let mut args = vec!["run", "--strategy", "sma_crossover", "--synthetic", "--seed", "1", "--days", "300", "--all", "--fills", "--timing"];
        args.extend(extra);
        args.extend(files.iter().map(|s| s.as_str()));
        let out = abt(&args);
        let (stdout, stderr) = text(&out);
        assert!(out.status.success(), "stdout: {}\nstderr: {}", stdout, stderr);
        let pushed: usize = stderr
            .split_whitespace()
            .find_map(|kv| kv.strip_prefix("pushed_answers="))
            .unwrap_or_else(|| panic!("{}", stderr))
            .parse()
            .unwrap();
        (stdout, pushed)
    };
    let (pushed_run, pushed) = run(&[]);
    let (local_run, local) = run(&["--no-pushdown"]);
    assert_eq!(pushed_run, local_run);
    assert!(pushed > 0 && local == 0, "pushed {} / {}", pushed, local);
}

/// trend-09: `--param name=value` (repeatable) overrides a parameter on
/// `abt run` and `abt explain`; a value outside the declared range or of the
/// wrong type, or an unknown name, is refused. (`fast=30d`: the strategy's
/// `fast_min` of 12 rows cannot fill in a 10-day window, which would leave
/// the run with no decisions at all.)
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
    let out = run_with(&["--param", "fast=30d", "--param", "qty=250 shares"]);
    let (stdout, stderr) = text(&out);
    assert!(out.status.success(), "stdout: {}\nstderr: {}", stdout, stderr);
    let summary = |s: &str| s.lines().find(|l| l.starts_with("decisions:")).unwrap().to_string();
    assert_ne!(summary(&base), summary(&stdout), "the overrides should change the run:\n{}", stdout);
    assert!(!stdout.contains("decisions: 0 "), "the overridden run still decides:\n{}", stdout);

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
        "query",
        "--strategy",
        "sma_crossover",
        "--text",
        "trace decide at 2022-04-14",
        "--param",
        "fast=30d",
        "--synthetic",
        "--seed",
        "1",
        "--days",
        "120",
    ];
    args.extend(files.iter().map(|s| s.as_str()));
    let out = abt(&args);
    let (stdout, stderr) = text(&out);
    assert!(out.status.success() && stdout.contains("sma_crossover::decide#1"), "stdout: {}\nstderr: {}", stdout, stderr);
}

/// Data-bundle doc, section 6: `abt check` prints each strategy's degrees of
/// freedom (params, in-rule literals, rules, reachable library literals), and
/// a ticker literal warns W6 without failing the check. (The v2
/// momentum_top_n writes its universe rule itself: 7 rules.)
#[test]
fn check_prints_degrees_of_freedom_per_strategy() {
    let files = strategy_files("momentum_top_n");
    let mut args = vec!["check"];
    args.extend(files.iter().map(|s| s.as_str()));
    let out = abt(&args);
    let (stdout, _) = text(&out);
    assert_eq!(out.status.code(), Some(0), "{}", stdout);
    assert!(stdout.contains("momentum_top_n: degrees of freedom: 4 params, 0 literals, 7 rules (+ 1 library literal)"), "{}", stdout);
    let files = strategy_files("relative_strength");
    let mut args = vec!["check"];
    args.extend(files.iter().map(|s| s.as_str()));
    let out = abt(&args);
    let (stdout, _) = text(&out);
    assert_eq!(out.status.code(), Some(0), "a warning does not fail the check:\n{}", stdout);
    assert!(stdout.contains("warning [W6]") && stdout.contains("\"SPY\""), "{}", stdout);
    assert!(stdout.contains("relative_strength: degrees of freedom:"), "{}", stdout);
}

/// Data-bundle doc, section 5: `--nav FILE` writes the book at every bar
/// as Parquet for the study and the reference engine to consume; `reg_t`
/// is a value of the execution block's `leverage` field, which a strategy
/// sets over a preset with `with { }` and the checked program prints
/// expanded; a value the field does not admit is refused naming the field.
#[test]
fn nav_writes_the_book_per_bar_and_reg_t_is_a_leverage_value() {
    let dir = scratch("nav");
    let (catalog, lib) = (corpus("catalog"), corpus("lib"));
    let (catalog, lib) = (catalog.to_string_lossy().into_owned(), lib.to_string_lossy().into_owned());
    let strategy = up_down_strategy(&dir, "regt", "universe(A, T), price_return(A, T, R), R > 0", "equity_default with { leverage reg_t }");
    let nav = dir.join("book.parquet");
    let nav_s = nav.to_string_lossy().into_owned();
    let args = ["run", "--strategy", "regt", "--synthetic", "--days", "120", "--quiet", "--nav", &nav_s, &catalog, &lib, &strategy];
    let out = abt(&args);
    let (stdout, stderr) = text(&out);
    assert_eq!(out.status.code(), Some(0), "{}\n{}", stdout, stderr);
    let book = abt_kernel::table::to_text(&nav).unwrap();
    assert!(book.starts_with("t,equity,cash,gross,net,leverage\n2022-01-03,"), "{}", book);
    assert!(stdout.contains("funding: cash interest") && stdout.contains("exposure: max gross"), "{}", stdout);
    let rows = book.lines().filter(|l| l.starts_with("2022-") || l.starts_with("2023-")).count();
    assert!(rows > 100, "{} rows", rows);
    assert!(!stdout.contains("exposure: max gross 0.00"), "the book traded:\n{}", stdout);
    // The checked program carries the expanded block, the override applied.
    let out = abt(&["ir", "--strategy", "regt", "--stage", "checked", &catalog, &lib, &strategy]);
    let (stdout, stderr) = text(&out);
    assert_eq!(out.status.code(), Some(0), "{}\n{}", stdout, stderr);
    assert!(stdout.contains("leverage      reg_t") && stdout.contains("commission    per_share 0.005 USD min 1 USD"), "{}", stdout);
    // A leverage the field does not admit.
    let strategy = up_down_strategy(&dir, "portfolio", "universe(A, T), price_return(A, T, R), R > 0", "equity_default with { leverage portfolio }");
    let out = abt(&["run", "--strategy", "portfolio", "--synthetic", "--days", "120", "--quiet", &catalog, &lib, &strategy]);
    assert_eq!(out.status.code(), Some(2));
    assert!(text(&out).1.contains("leverage") && text(&out).1.contains("reg_t"), "{}", text(&out).1);
    let _ = std::fs::remove_dir_all(&dir);
}

/// A bundle is an import (M-9): `abt data ingest --adapter bundle_v2` reads
/// it into a store, and a run reads the store. A bundle whose tests have not
/// passed imports unreviewed (K-2.1): refused under `require reviewed`,
/// admitted and warned under `allow unreviewed`, and reviewed when a
/// reviewer says so (`--attributes reviewed=complete`).
#[test]
fn an_imported_bundle_runs_under_its_review() {
    let dir = scratch("bundle");
    let _ = std::fs::remove_dir_all(&dir);
    let class = abt_kernel::check::class_program(&common::base_workspace(), "equity").unwrap_or_else(|d| panic!("{}", common::text(&d)));
    let ds = abt_kernel::data::synthetic_daily(&["AAA", "XYZ", "BBB", "SPY", "DDD"], (2022, 1, 3), 150, 7);
    let bundle = dir.join("bundle");
    abt_kernel::bundle::write_bundle(&class, &ds, &bundle, "equity", "2026.10").unwrap();
    let files = strategy_files("total_return_momentum");
    let import = |tag: &str, extra: &[&str]| -> String {
        let store = dir.join(tag).to_string_lossy().to_string();
        let mut args = vec!["data", "ingest", "--store", &store, "--adapter", "bundle_v2", "--from", bundle.to_str().unwrap(), "--class", "equity"];
        args.extend_from_slice(extra);
        args.extend(files[..2].iter().map(|s| s.as_str()));
        let out = abt(&args);
        assert!(out.status.success(), "{}\n{}", text(&out).0, text(&out).1);
        store
    };
    let untested = import("untested", &[]);
    // Untested: refused under `require reviewed`, naming what is missing.
    let mut args = vec!["run", "--strategy", "total_return_momentum", "--data", &untested, "--quiet"];
    args.extend(files.iter().map(|s| s.as_str()));
    let refused = abt(&args);
    assert_eq!(refused.status.code(), Some(2));
    let why = text(&refused).1;
    assert!(why.contains("no binding") && why.contains("no `reviewed` attribute") && why.contains("need `bars @1d`"), "{}", why);
    // Admitted under `allow unreviewed`, warned.
    let src = std::fs::read_to_string(&files[2]).unwrap().replace("require reviewed", "allow unreviewed");
    let unreviewed = dir.join("unreviewed.dsl");
    std::fs::write(&unreviewed, src).unwrap();
    let unreviewed = unreviewed.to_string_lossy().to_string();
    let ran = abt(&["run", "--strategy", "total_return_momentum", "--data", &untested, "--quiet", &files[0], &files[1], &unreviewed]);
    assert_eq!(ran.status.code(), Some(0), "{}", text(&ran).1);
    assert!(text(&ran).0.contains("warning (unreviewed data)"), "{}", text(&ran).0);
    // A reviewer's statement: the run needs no flag.
    let reviewed = import("reviewed", &["--attributes", "reviewed=complete"]);
    let mut args = vec!["run", "--strategy", "total_return_momentum", "--data", &reviewed, "--quiet"];
    args.extend(files.iter().map(|s| s.as_str()));
    let ran = abt(&args);
    assert_eq!(ran.status.code(), Some(0), "{}", text(&ran).1);
    assert!(text(&ran).0.contains("decisions:"), "{}", text(&ran).0);
    // A bundle is not read by a run itself.
    let mut args = vec!["run", "--strategy", "total_return_momentum", "--data", bundle.to_str().unwrap(), "--quiet"];
    args.extend(files.iter().map(|s| s.as_str()));
    let refused = abt(&args);
    assert_eq!(refused.status.code(), Some(2));
    assert!(text(&refused).1.contains("not a store") && text(&refused).1.contains("abt data ingest"), "{}", text(&refused).1);
    let _ = std::fs::remove_dir_all(&dir);
}

/// Data-bundle doc, section 2: monthly checkpoints are written by the fold
/// and a run resumed from one ends where the unbroken run ends. (The
/// sma_crossover trades within the 130 bars; momentum_top_n's one-year
/// lookback would decide nothing in them.)
#[test]
fn checkpoints_are_written_and_a_resumed_run_matches() {
    let dir = scratch("checkpoints");
    let _ = std::fs::remove_dir_all(&dir);
    let out = dir.to_string_lossy().to_string();
    let files = strategy_files("sma_crossover");
    let base = ["run", "--strategy", "sma_crossover", "--synthetic", "--days", "130", "--quiet"];
    let mut args: Vec<&str> = base.to_vec();
    args.extend(["--checkpoint-every", "month", "--checkpoint-dir", &out]);
    args.extend(files.iter().map(|s| s.as_str()));
    let full = abt(&args);
    assert_eq!(full.status.code(), Some(0), "{}", text(&full).1);
    assert!(!text(&full).0.contains("fills: 0 "), "the run trades:\n{}", text(&full).0);
    let mut written: Vec<String> = std::fs::read_dir(&dir).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().to_string()).collect();
    written.sort();
    assert!(written.len() >= 4, "{:?}", written);
    assert!(written[0].starts_with("2022-01-") && written[0].ends_with(".json"), "{:?}", written);
    let last = dir.join(written.last().unwrap()).to_string_lossy().to_string();
    let mut args: Vec<&str> = base.to_vec();
    args.extend(["--resume", &last]);
    args.extend(files.iter().map(|s| s.as_str()));
    let resumed = abt(&args);
    assert_eq!(resumed.status.code(), Some(0), "{}", text(&resumed).1);
    assert!(text(&resumed).1.contains("resuming from"), "{}", text(&resumed).1);
    let final_line = |o: &Output| text(o).0.lines().find(|l| l.starts_with("final cash")).unwrap_or_default().to_string();
    assert!(final_line(&full).contains("positions: E"), "{}", final_line(&full));
    assert_eq!(final_line(&full), final_line(&resumed));
    // The batch kernel is gone (M-2), and so is the option that chose it
    // (M-9): every run is the fold over the stream.
    let mut args = vec!["run", "--strategy", "sma_crossover", "--synthetic", "--days", "130", "--quiet", "--kernel", "batch"];
    args.extend(files.iter().map(|s| s.as_str()));
    let gone = abt(&args);
    assert_eq!(gone.status.code(), Some(1));
    assert!(text(&gone).1.contains("unknown option") && text(&gone).1.contains("--kernel"), "{}", text(&gone).1);
    let _ = std::fs::remove_dir_all(&dir);
}

/// `abt hash` prints H(P), and `--show` the bytes it is BLAKE3 over (P-2).
#[test]
fn hash_prints_the_program_hash_and_shows_what_it_hashed() {
    let files = strategy_files("sma_crossover");
    let mut args = vec!["hash", "--strategy", "sma_crossover"];
    args.extend(files.iter().map(|s| s.as_str()));
    let o = abt(&args);
    assert_eq!(o.status.code(), Some(0), "{}", text(&o).1);
    let out = text(&o).0;
    let (hash, name) = out.trim_end().split_once("  ").unwrap_or_else(|| panic!("{}", out));
    assert_eq!((hash.len(), name), (64, "sma_crossover"));
    let mut args = vec!["hash", "--strategy", "sma_crossover", "--show"];
    args.extend(files.iter().map(|s| s.as_str()));
    let shown = abt(&args);
    assert_eq!(shown.status.code(), Some(0), "{}", text(&shown).1);
    let bytes = text(&shown).0;
    assert_eq!(abt_wire::Hash::of(bytes.trim_end_matches('\n').as_bytes()).hex(), hash);
}
