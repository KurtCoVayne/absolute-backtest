//! The bundle format (data-bundle doc, section 2 "Storage" and section 3
//! "Bundle tests"): a versioned directory with a JSON manifest, the security
//! table and an append-only log of Parquet partitions by month; a bundle is
//! named by the class it instantiates and a strategy may pin the version it
//! was written against; a bundle that fails its tests is not run.

mod common;

use std::fs;
use std::path::PathBuf;

use absolute_backtest::bundle::{load_bundle, read_manifest, run_tests, run_tests_with, test_bundle, write_bundle, Exception};
use absolute_backtest::check::{class_program, Program};
use absolute_backtest::data::synthetic_daily;
use absolute_backtest::kernel::{run, Dataset, ExecConfig, Value};

const SYMS: [&str; 5] = ["AAA", "BBB", "CCC", "DDD", "SPY"];

/// The program whose demand covers the equity class whole: what `abt
/// bundle build` and `abt bundle test` write and test a bundle with, so
/// that every relation of the catalog is stored and checked, not one
/// strategy's slice of it.
fn class_prog() -> Program {
    class_program(&common::base_workspace(), "equity").unwrap_or_else(|d| panic!("the equity class should check as a program:\n{}", common::text(&d)))
}

/// The synthetic daily market with the catalog (a split, dividends, a
/// renamed name and a delisted one).
fn market(days: usize, seed: u64) -> Dataset {
    synthetic_daily(&SYMS, (2022, 1, 3), days, seed)
}

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("abt-bundle-{}-{}", std::process::id(), name));
    let _ = fs::remove_dir_all(&dir);
    dir
}

fn result<'a>(results: &'a [absolute_backtest::bundle::TestResult], name: &str) -> &'a absolute_backtest::bundle::TestResult {
    results.iter().find(|r| r.name == name).unwrap_or_else(|| panic!("no bundle test `{}`", name))
}

#[test]
fn a_bundle_round_trips_a_catalog_market_through_parquet() {
    let cls = class_prog();
    let p = common::corpus_program("total_return_momentum");
    let ds = market(320, 11);
    let dir = scratch("roundtrip");
    // A bundle is named by its class.
    let err = write_bundle(&cls, &ds, &dir, "equities_1d_v2", "2026.10").err().unwrap();
    assert!(err.contains("equity") && err.contains("equities_1d_v2"), "{}", err);
    let m = write_bundle(&cls, &ds, &dir, "equity", "2026.10").unwrap();
    assert_eq!(m.name, "equity");
    assert_eq!(m.version, "2026.10");
    assert!(dir.join("manifest.json").exists() && dir.join("securities.parquet").exists());
    // One partition per relation and month of the temporal key.
    let close = m.relations.iter().find(|r| r.name == "close").unwrap();
    assert_eq!(close.rows, ds.facts["close"].len());
    assert!(close.partitions.iter().any(|p| p.ends_with("2022-01.parquet")), "{:?}", close.partitions);
    assert!(dir.join("log").join("close").join("2022-01.parquet").exists());
    assert_eq!(close.availability, "bar_close");
    assert!(m.tests.is_none(), "a freshly built bundle is untested");
    // The derived ticker relation is rebuilt, not stored.
    assert!(!m.relations.iter().any(|r| r.name == "ticker"));
    // Loading needs the tests to have passed, unless asked otherwise.
    let err = load_bundle(&p, &dir, false).err().unwrap();
    assert!(err.contains("tests") && err.contains("--untested"), "{}", err);
    let (loaded, m2) = load_bundle(&cls, &dir, true).unwrap();
    assert_eq!(m2.version, "2026.10");
    assert_eq!(loaded.securities, ds.securities);
    for rel in [
        "open",
        "high",
        "low",
        "close",
        "volume",
        "listed",
        "split",
        "dividend",
        "delisted",
        "member",
        "classification",
        "ticker",
    ] {
        assert_eq!(loaded.facts[rel].len(), ds.facts[rel].len(), "{}", rel);
    }
    // Same symbols, same values: the runs agree exactly, the strategy
    // reading its own slice of the bundle under its execution block.
    let (slice, _) = load_bundle(&p, &dir, true).unwrap();
    let cfg = ExecConfig::from_program(&p);
    let a = run(&p, &ds, cfg.clone()).unwrap();
    let b = run(&p, &slice, cfg).unwrap();
    assert_eq!(a.final_cash.to_bits(), b.final_cash.to_bits());
    assert_eq!(a.decisions.len(), b.decisions.len());
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn the_bundle_tests_pass_on_the_synthetic_catalog_and_are_recorded() {
    let cls = class_prog();
    let ds = market(320, 11);
    let results = run_tests(&cls, &ds);
    let names: Vec<&str> = results.iter().map(|r| r.name.as_str()).collect();
    for expected in ["identity", "bar labels", "positive prices", "action reconciliation", "delisting coverage", "membership"] {
        assert!(names.contains(&expected), "missing bundle test {}: {:?}", expected, names);
    }
    assert!(
        results.iter().all(|r| r.passed),
        "{:?}",
        results.iter().filter(|r| !r.passed).map(|r| format!("{}: {}", r.name, r.detail)).collect::<Vec<_>>()
    );
    let dir = scratch("tested");
    write_bundle(&cls, &ds, &dir, "equity", "2026.10").unwrap();
    let (m, results) = test_bundle(&cls, &dir).unwrap();
    assert!(results.iter().all(|r| r.passed));
    assert!(m.tests.as_ref().map(|t| !t.passed_at.is_empty()).unwrap_or(false), "{:?}", m.tests);
    let on_disk = read_manifest(&dir).unwrap();
    assert!(on_disk.tests.is_some(), "the manifest records the passing tests");
    // Now the bundle loads without --untested.
    load_bundle(&cls, &dir, false).unwrap();
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn a_broken_bundle_fails_the_right_test_and_is_not_marked_tested() {
    let cls = class_prog();
    // Drop the split record: the halving on the ex-date is an unexplained gap.
    let mut ds = market(320, 11);
    ds.facts.remove("split");
    let r = run_tests(&cls, &ds);
    let rec = result(&r, "action reconciliation");
    assert!(!rec.passed && rec.detail.contains("E2"), "{:?}", rec);
    // A split with no gap in the prices.
    let mut ds3 = market(320, 11);
    let aaa = ds3.symbols.get("E1").unwrap();
    let t = ds3.facts["close"][5][1].clone();
    ds3.add("split", vec![Value::Equity(aaa), t, Value::Num(3.0)]);
    let r = run_tests(&cls, &ds3);
    assert!(!result(&r, "action reconciliation").passed);
    // A failing bundle is written, tested, and left unmarked.
    let dir = scratch("broken");
    write_bundle(&cls, &ds, &dir, "equity", "2026.10").unwrap();
    let (m, results) = test_bundle(&cls, &dir).unwrap();
    assert!(results.iter().any(|r| !r.passed));
    assert!(m.tests.is_none());
    assert!(load_bundle(&cls, &dir, false).is_err());
    let _ = fs::remove_dir_all(&dir);
}

/// Delisting coverage and membership over the catalog's listing: a name
/// whose rows stop before the data's last bar with no delisting record,
/// and a member of a list on a day it is not listed.
#[test]
fn a_missing_delisting_record_and_a_member_outside_the_listing_fail_their_tests() {
    let cls = class_prog();
    // Drop the delisting record: a name whose prices stop with no reason.
    let mut ds = market(320, 11);
    ds.facts.remove("delisted");
    let r = run_tests(&cls, &ds);
    let cov = result(&r, "delisting coverage");
    assert!(!cov.passed && cov.detail.contains("E5"), "{:?}", cov);
    // A member that is not listed that day.
    let mut ds4 = market(320, 11);
    let spx = ds4.intern_label("SPX");
    let gone = ds4.facts["delisted"][0].clone();
    ds4.add("member", vec![gone[0].clone(), gone[1].clone(), Value::Label(spx)]);
    let r = run_tests(&cls, &ds4);
    assert!(!result(&r, "membership").passed, "{:?}", result(&r, "membership"));
}

/// A strategy's `prefer version V` pins the bundle version it was written
/// against; `prefer latest` takes any version of its class.
#[test]
fn a_strategy_names_the_bundle_version_it_was_written_against() {
    let cls = class_prog();
    let src = common::corpus_source("strategies", "total_return_momentum");
    assert!(src.contains("prefer  latest"), "{}", src);
    let pinned_src = src.replace("prefer  latest", "prefer  version 2026.10");
    let p = common::program(&pinned_src, "total_return_momentum");
    assert_eq!(p.class, "equity");
    assert_eq!(p.demand.prefer, "version 2026.10");
    let unpinned = common::corpus_program("total_return_momentum");
    assert_eq!(unpinned.demand.prefer, "latest");
    let ds = market(120, 3);
    let dir = scratch("version");
    write_bundle(&cls, &ds, &dir, "equity", "2026.10").unwrap();
    test_bundle(&cls, &dir).unwrap();
    load_bundle(&p, &dir, false).unwrap();
    load_bundle(&unpinned, &dir, false).unwrap();
    let other = common::program(&pinned_src.replace("version 2026.10", "version 2026.11"), "total_return_momentum");
    let err = load_bundle(&other, &dir, false).err().unwrap();
    assert!(err.contains("2026.11") && err.contains("2026.10"), "{}", err);
    // The wrong class altogether: a futures book on an equity bundle.
    let futures = common::program(&common::corpus_source("company", "r8l"), "r8l");
    assert_eq!(futures.class, "future");
    let err = load_bundle(&futures, &dir, false).err().unwrap();
    assert!(err.contains("future") && err.contains("equity"), "{}", err);
    let _ = fs::remove_dir_all(&dir);
}

/// E1's closes from bar `k` on, scaled by `scale`, and the bar `k` itself.
fn rescale_from(ds: &mut Dataset, id: &str, k: usize, scale: f64) -> (Value, i64, f64) {
    let sym = Value::Equity(ds.symbols.get(id).unwrap());
    let mut times: Vec<i64> = ds.facts["close"].iter().filter(|tu| tu[0] == sym).filter_map(|tu| tu[1].as_time()).collect();
    times.sort_unstable();
    let (t, prev_t) = (times[k], times[k - 1]);
    let prev = ds.facts["close"].iter().find(|tu| tu[0] == sym && tu[1].as_time() == Some(prev_t)).unwrap()[2].as_f64().unwrap();
    for tu in ds.facts.get_mut("close").unwrap() {
        if tu[0] == sym && tu[1].as_time().unwrap() >= t {
            tu[2] = Value::Num(tu[2].as_f64().unwrap() * scale);
        }
    }
    (sym, t, prev)
}

#[test]
fn a_dividend_explains_the_gap_at_its_ex_date() {
    // A spin-off delivered as a distribution worth half the share: the close
    // halves at the ex-date and the dividend makes the holder whole.
    let cls = class_prog();
    let mut ds = market(320, 11);
    let (sym, t, prev) = rescale_from(&mut ds, "E1", 30, 0.5);
    let gap = run_tests(&cls, &ds);
    assert!(!result(&gap, "action reconciliation").passed, "a bare halving is unexplained");
    ds.add("dividend", vec![sym.clone(), Value::Time(t), Value::Time(t), Value::Time(t), Value::Num(prev * 0.5)]);
    let r = run_tests(&cls, &ds);
    let rec = result(&r, "action reconciliation");
    assert!(rec.passed, "{:?}", rec);
    // A reverse split on the same day as a distribution (amount per new
    // share): the close is unchanged, the position halves, the cash pays
    // for the rest.
    let mut ds = market(320, 11);
    let (sym, t, prev) = rescale_from(&mut ds, "E1", 30, 1.0);
    ds.add("split", vec![sym.clone(), Value::Time(t), Value::Num(0.5)]);
    assert!(!result(&run_tests(&cls, &ds), "action reconciliation").passed, "a split with no gap");
    ds.add("dividend", vec![sym, Value::Time(t), Value::Time(t), Value::Time(t), Value::Num(prev)]);
    let r = run_tests(&cls, &ds);
    let rec = result(&r, "action reconciliation");
    assert!(rec.passed, "{:?}", rec);
}

#[test]
fn a_reviewed_exception_admits_a_real_price_move_and_is_counted() {
    let cls = class_prog();
    let mut ds = market(320, 11);
    let (_, t, _) = rescale_from(&mut ds, "E1", 40, 0.45);
    let r = run_tests(&cls, &ds);
    assert!(!result(&r, "action reconciliation").passed);
    let exceptions = vec![Exception {
        test: "action reconciliation".into(),
        security: "E1".into(),
        t,
        reason: "real move: a short-seller report".into(),
    }];
    let r = run_tests_with(&cls, &ds, &exceptions);
    let rec = result(&r, "action reconciliation");
    assert!(rec.passed, "{:?}", rec);
    assert!(rec.detail.contains("1 reviewed exception"), "{:?}", rec);
    // An exception for another day does not excuse this one.
    let wrong = vec![Exception {
        t: t + 86_400,
        ..exceptions[0].clone()
    }];
    assert!(!result(&run_tests_with(&cls, &ds, &wrong), "action reconciliation").passed);
    // In a bundle, the exceptions live in exceptions.parquet next to the manifest.
    let dir = scratch("exceptions");
    write_bundle(&cls, &ds, &dir, "equity", "2026.10").unwrap();
    let (m, _) = test_bundle(&cls, &dir).unwrap();
    assert!(m.tests.is_none(), "without the file the bundle fails");
    absolute_backtest::bundle::write_exceptions(&dir, &exceptions).unwrap();
    let (m, results) = test_bundle(&cls, &dir).unwrap();
    assert!(m.tests.is_some(), "{:?}", results);
    let _ = fs::remove_dir_all(&dir);
}
