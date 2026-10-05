//! Loader tests: a CSV environment instance must be a function of its
//! signature's inputs and key (section 3 "Modes") and its temporal keys must
//! be bar labels of the relation's resolution (section 3 "Resolution",
//! section 6 "Time domains").

mod corpus;

use std::fs;
use std::path::PathBuf;

use absolute_backtest::check::{check_program, Program};
use absolute_backtest::data::load_csv_dir;
use absolute_backtest::kernel::time::parse_timestamp;
use absolute_backtest::kernel::{ExecConfig, Kernel, Value};

const HOLD: &str = r#"
strategy hold {
  env equities_1d
  uses features
  resolution @1d
  mode target
  decide(T, target_weight(A, 0.5)) :- universe(A, T).
}
"#;

fn program() -> Program {
    let mut ws = corpus::base_workspace();
    ws.add_source(HOLD).unwrap();
    let (p, diags) = check_program(&ws, "hold");
    let text: Vec<String> = diags.iter().map(|d| d.to_string()).collect();
    p.unwrap_or_else(|| panic!("hold should check:\n{}", text.join("\n")))
}

/// A fresh directory holding the given `<relation>.csv` files.
fn csv_dir(name: &str, files: &[(&str, &str)]) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("abt-data-{}-{}", std::process::id(), name));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    for (rel, body) in files {
        fs::write(dir.join(format!("{}.csv", rel)), body).unwrap();
    }
    dir
}

fn day(s: &str) -> i64 {
    parse_timestamp(s).unwrap()
}

#[test]
fn conflicting_duplicate_rows_are_rejected_naming_the_line() {
    let prog = program();
    let dir = csv_dir(
        "dupconflict",
        &[
            ("close", "A,T,P\nAAA,2022-01-03,10.0\nAAA,2022-01-03,15.0\n"),
            ("volume", "A,T,V\nAAA,2022-01-03,1000\n"),
            ("universe", "A,T\nAAA,2022-01-03\n"),
        ],
    );
    let err = load_csv_dir(&prog, &dir).expect_err("two prices for one (A, T) must be rejected");
    assert!(err.contains("close.csv:3"), "{}", err);
    assert!(err.contains("duplicate"), "{}", err);
    assert!(err.contains("AAA") && err.contains("2022-01-03"), "{}", err);
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn identical_duplicate_rows_are_dropped() {
    let prog = program();
    let dir = csv_dir(
        "dupsame",
        &[
            ("close", "A,T,P\nAAA,2022-01-03,10.0\nAAA,2022-01-03,10.0\n"),
            ("volume", "A,T,V\nAAA,2022-01-03,1000\n"),
            ("universe", "A,T\nAAA,2022-01-03\nAAA,2022-01-03\n"),
        ],
    );
    let (ds, _) = load_csv_dir(&prog, &dir).unwrap();
    assert_eq!(ds.facts["close"].len(), 1);
    assert_eq!(ds.facts["universe"].len(), 1);
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn two_symbols_on_one_day_are_distinct_tuples() {
    // `universe(-A, @T)` enumerates A: an entity-typed output identifies.
    let prog = program();
    let dir = csv_dir(
        "twosyms",
        &[
            ("close", "A,T,P\nAAA,2022-01-03,10.0\nBBB,2022-01-03,20.0\n"),
            ("volume", "A,T,V\nAAA,2022-01-03,1000\nBBB,2022-01-03,2000\n"),
            ("universe", "A,T\nAAA,2022-01-03\nBBB,2022-01-03\n"),
        ],
    );
    let (ds, notes) = load_csv_dir(&prog, &dir).unwrap();
    assert!(notes.is_empty(), "{:?}", notes);
    assert_eq!(ds.facts["close"].len(), 2);
    assert_eq!(ds.facts["universe"].len(), 2);
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn daily_labels_with_a_time_of_day_are_the_date_bar() {
    let prog = program();
    let dir = csv_dir(
        "mixedtimes",
        &[
            ("close", "A,T,P\nAAA,2022-01-03T16:00:00,10.0\nAAA,2022-01-04 16:00,11.0\n"),
            ("volume", "A,T,V\nAAA,2022-01-03,1000\nAAA,2022-01-04,1000\n"),
            ("universe", "A,T\nAAA,2022-01-03\nAAA,2022-01-04\n"),
        ],
    );
    let (ds, _) = load_csv_dir(&prog, &dir).unwrap();
    let keys: Vec<Value> = ds.facts["close"].iter().map(|tu| tu[1].clone()).collect();
    assert_eq!(keys, vec![Value::Time(day("2022-01-03")), Value::Time(day("2022-01-04"))]);
    // One time domain: two bars, not four.
    let bars = Kernel::new(&prog, &ds, ExecConfig::default()).unwrap().decision_bars();
    assert_eq!(bars, vec![day("2022-01-03"), day("2022-01-04")]);
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn a_time_of_day_does_not_hide_a_conflicting_duplicate() {
    let prog = program();
    let dir = csv_dir(
        "dupnormalised",
        &[
            ("close", "A,T,P\nAAA,2022-01-03,10.0\nAAA,2022-01-03T16:00:00,15.0\n"),
            ("volume", "A,T,V\nAAA,2022-01-03,1000\n"),
            ("universe", "A,T\nAAA,2022-01-03\n"),
        ],
    );
    let err = load_csv_dir(&prog, &dir).expect_err("the two rows label the same @1d bar");
    assert!(err.contains("close.csv:3") && err.contains("duplicate"), "{}", err);
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn a_header_only_file_is_noted() {
    let prog = program();
    let dir = csv_dir("headeronly", &[("close", "A,T,P\n"), ("volume", "A,T,V\nAAA,2022-01-03,1000\n"), ("universe", "A,T\nAAA,2022-01-03\n")]);
    let (ds, notes) = load_csv_dir(&prog, &dir).unwrap();
    assert!(!ds.facts.contains_key("close") || ds.facts["close"].is_empty());
    assert!(notes.iter().any(|n| n.contains("close.csv") && n.contains("no rows")), "{:?}", notes);
    let _ = fs::remove_dir_all(&dir);
}

/// A price that is not positive is a data error, not a policy (section 6,
/// executor policy): it is rejected at load time naming the line.
#[test]
fn a_non_positive_price_is_rejected_at_load() {
    let prog = program();
    let dir = csv_dir(
        "negprice",
        &[("close", "A,T,P\nAAA,2022-01-03,10.0\nAAA,2022-01-04,-5.0\n"), ("universe", "A,T\nAAA,2022-01-03\nAAA,2022-01-04\n")],
    );
    let err = load_csv_dir(&prog, &dir).expect_err("a non-positive price must not load");
    assert!(err.contains("close.csv:3") && err.contains("-5"), "{}", err);
    let dir = csv_dir("zeroprice", &[("close", "A,T,P\nAAA,2022-01-03,0\n"), ("universe", "A,T\nAAA,2022-01-03\n")]);
    assert!(load_csv_dir(&prog, &dir).is_err());
}
