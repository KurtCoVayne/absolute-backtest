//! `abt data` (D-10) through the binary: a synthetic market ingested into a
//! store directory, the class file rendered from the store's registry, the
//! versions' statistics, queries in the text form (the typed request printed
//! first, D-4.5), and a run reading the store through `abt data serve` on a
//! socket deciding as the run that holds the market in process (G3 across
//! processes).

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

fn abt(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_abt")).args(args).current_dir(abt_tests::repo_root()).output().expect("abt runs")
}

fn text(o: &Output) -> (String, String) {
    (String::from_utf8_lossy(&o.stdout).to_string(), String::from_utf8_lossy(&o.stderr).to_string())
}

fn scratch(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("abt-data-cli-{}-{}", tag, std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

const FILES: [&str; 2] = ["corpus-v2/catalog", "corpus-v2/lib"];

#[test]
fn ingest_catalog_stats_and_query_a_store() {
    let dir = scratch("store");
    let store = dir.to_string_lossy().to_string();
    let mut args = vec!["data", "ingest", "--store", &store, "--adapter", "synthetic", "--class", "equity", "--days", "30"];
    args.extend(FILES);
    let o = abt(&args);
    assert_eq!(o.status.code(), Some(0), "{}", text(&o).1);
    assert!(text(&o).0.contains("equity.close dataset"), "{}", text(&o).0);
    // The class file is a rendering of the registry.
    let o = abt(&["data", "catalog", "--store", &store, "--class", "equity"]);
    let want = std::fs::read_to_string(abt_tests::repo_root().join("corpus-v2/catalog/equity.dsl")).unwrap();
    assert_eq!(text(&o).0, want);
    let o = abt(&["data", "stats", "--store", &store, "--relation", "equity.close"]);
    assert!(text(&o).0.contains(" current"), "{}", text(&o).0);
    // A cross-section, then one name's series.
    let o = abt(&["data", "query", "--store", &store, "equity.close at 2022-01-03"]);
    assert_eq!(o.status.code(), Some(0), "{}", text(&o).1);
    let out = text(&o).0;
    let mut lines = out.lines();
    assert!(lines.next().unwrap().contains("\"Section\""), "the typed request first: {}", out);
    assert_eq!(lines.next().unwrap(), "A | T | P | r");
    assert_eq!(lines.count(), 5, "{}", out);
    let o = abt(&["data", "query", "--store", &store, "equity.close over 2022-01-03..2022-01-07 for E1"]);
    let out = text(&o).0;
    assert_eq!(out.lines().count(), 2 + 5, "{}", out);
    assert!(out.lines().nth(2).unwrap().starts_with("1 | 2022-01-03T00:00:00.000000000Z | "), "{}", out);
    // A predicate and the receipt filter.
    let o = abt(&["data", "query", "--store", &store, "equity.close over 2022-01-03..2022-01-31 where P > 1000000"]);
    assert_eq!(text(&o).0.lines().count(), 2, "{}", text(&o).0);
    let o = abt(&["data", "query", "--store", &store, "equity.close latest at 2022-02-28 for E2 r <= 2022-01-05"]);
    assert!(text(&o).0.lines().nth(2).unwrap().contains("2022-01-05"), "{}", text(&o).0);
    let o = abt(&["data", "query", "--store", &store, "equity.close at 2022-01-03", "--explain"]);
    assert!(text(&o).0.contains("partitions 1"), "{}", text(&o).0);
    let o = abt(&["data", "query", "--store", &store, "equity.close at nonsense"]);
    assert_eq!(o.status.code(), Some(2));
    let _ = std::fs::remove_dir_all(&dir);
}

/// The line of a run's summary that holds its final state.
fn final_line(o: &Output) -> String {
    text(o).0.lines().find(|l| l.starts_with("final cash")).unwrap_or_default().to_string()
}

#[test]
fn a_run_reads_a_served_store_as_it_reads_the_market_in_process() {
    let dir = scratch("served");
    let store = dir.to_string_lossy().to_string();
    let mut args = vec!["data", "ingest", "--store", &store, "--adapter", "synthetic", "--class", "equity", "--days", "130"];
    args.extend(FILES);
    assert_eq!(abt(&args).status.code(), Some(0));
    let socket = dir.join("data.sock");
    let sock = socket.to_string_lossy().to_string();
    let mut server = Command::new(env!("CARGO_BIN_EXE_abt"))
        .args(["data", "serve", "--store", &store, "--socket", &sock])
        .current_dir(abt_tests::repo_root())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    for _ in 0..200 {
        if Path::new(&socket).exists() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(25));
    }
    let strategy = "corpus-v2/strategies/sma_crossover.dsl";
    let over_socket = abt(&["run", "--strategy", "sma_crossover", "--data", &sock, "--quiet", FILES[0], FILES[1], strategy]);
    let in_process = abt(&["run", "--strategy", "sma_crossover", "--synthetic", "--days", "130", "--quiet", FILES[0], FILES[1], strategy]);
    let _ = server.kill();
    let _ = server.wait();
    assert_eq!(over_socket.status.code(), Some(0), "{}", text(&over_socket).1);
    assert_eq!(in_process.status.code(), Some(0), "{}", text(&in_process).1);
    assert!(!final_line(&in_process).is_empty(), "{}", text(&in_process).0);
    assert_eq!(final_line(&over_socket), final_line(&in_process));
    let _ = std::fs::remove_dir_all(&dir);
}
