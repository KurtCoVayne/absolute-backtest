//! Loader tests: a Parquet instance of a primitive relation must be a
//! function of its identity (Definition 4.1: the entity arguments and the
//! key) and its temporal keys must be bar labels of the relation's
//! resolution (section 3 "Resolution", section 6 "Time domains").

use abt_tests as common;

use std::fs;
use std::path::PathBuf;

use abt_kernel::check::Program;
use abt_kernel::data::load_parquet_dir;
use abt_kernel::kernel::{ExecConfig, Value};
use abt_kernel::table::write_text_table;

/// A daily equity strategy that holds every name of the universe at a
/// fixed weight; its demand is the daily bars and the listing.
fn program() -> Program {
    common::program(&common::strategy("hold", "    w : Scalar = 0.5", "", "    decide(T, hold(A, w)) :- universe(A, T)."), "hold")
}

/// A fresh directory holding `<relation>.parquet` files given as text rows:
/// `close` as written, `open`, `high` and `low` as the same rows under their
/// own column name, and the `others` (volume, listed) as written.
fn table_dir(name: &str, close: &str, others: &[(&str, &str)]) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("abt-data-{}-{}", std::process::id(), name));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    let at_close = |col: &str| close.replacen("A,T,P", &format!("A,T,{}", col), 1);
    let mut files: Vec<(&str, String)> = vec![("close", close.to_string()), ("open", at_close("O")), ("high", at_close("H")), ("low", at_close("L"))];
    files.extend(others.iter().map(|(rel, body)| (*rel, body.to_string())));
    for (rel, body) in files {
        write_text_table(&dir.join(format!("{}.parquet", rel)), &body).unwrap();
    }
    dir
}

#[test]
fn conflicting_duplicate_rows_are_rejected_naming_the_row() {
    let prog = program();
    let dir = table_dir(
        "dupconflict",
        "A,T,P\nAAA,2022-01-03,10.0\nAAA,2022-01-03,15.0\n",
        &[("volume", "A,T,V\nAAA,2022-01-03,1000\n"), ("listed", "A,T\nAAA,2022-01-03\n")],
    );
    let err = load_parquet_dir(&prog, &dir).expect_err("two prices for one (A, T) must be rejected");
    assert!(err.contains("close.parquet row 2"), "{}", err);
    assert!(err.contains("duplicate"), "{}", err);
    assert!(err.contains("AAA") && err.contains("2022-01-03"), "{}", err);
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn identical_duplicate_rows_are_dropped() {
    let prog = program();
    let dir = table_dir(
        "dupsame",
        "A,T,P\nAAA,2022-01-03,10.0\nAAA,2022-01-03,10.0\n",
        &[("volume", "A,T,V\nAAA,2022-01-03,1000\n"), ("listed", "A,T\nAAA,2022-01-03\nAAA,2022-01-03\n")],
    );
    let (ds, _) = abt_tests::load_fixture(&prog, &dir).unwrap();
    assert_eq!(ds.facts["close"].len(), 1);
    assert_eq!(ds.facts["listed"].len(), 1);
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn two_symbols_on_one_day_are_distinct_tuples() {
    // The entity argument is part of the identity (Definition 4.1).
    let prog = program();
    let dir = table_dir(
        "twosyms",
        "A,T,P\nAAA,2022-01-03,10.0\nBBB,2022-01-03,20.0\n",
        &[("volume", "A,T,V\nAAA,2022-01-03,1000\nBBB,2022-01-03,2000\n"), ("listed", "A,T\nAAA,2022-01-03\nBBB,2022-01-03\n")],
    );
    let (ds, notes) = abt_tests::load_fixture(&prog, &dir).unwrap();
    assert!(notes.is_empty(), "{:?}", notes);
    assert_eq!(ds.facts["close"].len(), 2);
    assert_eq!(ds.facts["listed"].len(), 2);
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn daily_labels_with_a_time_of_day_are_the_date_bar() {
    let prog = program();
    let dir = table_dir(
        "mixedtimes",
        "A,T,P\nAAA,2022-01-03T16:00:00,10.0\nAAA,2022-01-04 16:00,11.0\n",
        &[("volume", "A,T,V\nAAA,2022-01-03,1000\nAAA,2022-01-04,1000\n"), ("listed", "A,T\nAAA,2022-01-03\nAAA,2022-01-04\n")],
    );
    let (ds, _) = abt_tests::load_fixture(&prog, &dir).unwrap();
    let keys: Vec<Value> = ds.facts["close"].iter().map(|tu| tu[1].clone()).collect();
    assert_eq!(keys, vec![Value::Time(common::day("2022-01-03")), Value::Time(common::day("2022-01-04"))]);
    // One time domain: two bars, not four.
    let bars = abt_tests::loaded_kernel(&prog, &ds, ExecConfig::default()).decision_bars();
    assert_eq!(bars, vec![common::day("2022-01-03"), common::day("2022-01-04")]);
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn a_time_of_day_does_not_hide_a_conflicting_duplicate() {
    let prog = program();
    let dir = table_dir(
        "dupnormalised",
        "A,T,P\nAAA,2022-01-03,10.0\nAAA,2022-01-03T16:00:00,15.0\n",
        &[("volume", "A,T,V\nAAA,2022-01-03,1000\n"), ("listed", "A,T\nAAA,2022-01-03\n")],
    );
    let err = load_parquet_dir(&prog, &dir).expect_err("the two rows label the same @1d bar");
    assert!(err.contains("close.parquet row 2") && err.contains("duplicate"), "{}", err);
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn an_empty_table_is_noted() {
    let prog = program();
    let dir = table_dir("headeronly", "A,T,P\n", &[("volume", "A,T,V\nAAA,2022-01-03,1000\n"), ("listed", "A,T\nAAA,2022-01-03\n")]);
    let (ds, notes) = abt_tests::load_fixture(&prog, &dir).unwrap();
    assert!(!ds.facts.contains_key("close") || ds.facts["close"].is_empty());
    assert!(notes.iter().any(|n| n.contains("close.parquet") && n.contains("no rows")), "{:?}", notes);
    let _ = fs::remove_dir_all(&dir);
}

/// A price that is not positive is a data error, not a policy (section 6,
/// executor policy): it is rejected at load time naming the row.
#[test]
fn a_non_positive_price_is_rejected_at_load() {
    let prog = program();
    let dir = table_dir("negprice", "A,T,P\nAAA,2022-01-03,10.0\nAAA,2022-01-04,-5.0\n", &[("listed", "A,T\nAAA,2022-01-03\nAAA,2022-01-04\n")]);
    let err = load_parquet_dir(&prog, &dir).expect_err("a non-positive price must not load");
    assert!(err.contains("close.parquet row 2") && err.contains("-5"), "{}", err);
    let _ = fs::remove_dir_all(&dir);
    let dir = table_dir("zeroprice", "A,T,P\nAAA,2022-01-03,0\n", &[("listed", "A,T\nAAA,2022-01-03\n")]);
    assert!(load_parquet_dir(&prog, &dir).is_err());
    let _ = fs::remove_dir_all(&dir);
}

/// A9 of docs/assessment.md: drift and volatility no longer grow with the
/// number of names, so a market of hundreds of names stays priced over ten
/// years (before, prices collapsed to the floor within months).
#[test]
fn a_large_synthetic_market_stays_priced() {
    let names: Vec<String> = (0..200).map(|i| format!("N{:03}", i)).collect();
    let syms: Vec<&str> = names.iter().map(String::as_str).collect();
    let ds = abt_kernel::data::synthetic_daily(&syms, (2010, 1, 4), 2_500, 3);
    let closes: Vec<f64> = ds.facts["close"].iter().filter_map(|tu| tu[2].as_f64()).collect();
    assert!(closes.len() > 400_000);
    let floored = closes.iter().filter(|p| **p <= 0.01).count();
    assert_eq!(floored, 0, "{} closes at the floor", floored);
    let mut last: Vec<f64> = closes[closes.len() - 150..].to_vec();
    last.sort_by(|a, b| a.partial_cmp(b).unwrap());
    assert!(last[75] > 1.0 && last[75] < 1e5, "median final close {}", last[75]);
}
