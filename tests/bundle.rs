//! The bundle format (data-bundle doc, section 2 "Storage" and section 3
//! "Bundle tests"): a versioned directory with a JSON manifest, the security
//! table and an append-only log of Parquet partitions by month; a strategy
//! names the bundle version it was written against; a bundle that fails its
//! tests is not run.

mod corpus;

use std::fs;
use std::path::PathBuf;

use absolute_backtest::bundle::{load_bundle, read_manifest, run_tests, run_tests_with, test_bundle, write_bundle, Exception};
use absolute_backtest::check::{check_program, Code, Program, Severity};
use absolute_backtest::data::synthetic_daily_v2;
use absolute_backtest::kernel::{run, ExecConfig, Value};

fn program(src: &str, name: &str) -> Program {
    let mut ws = corpus::base_workspace();
    ws.add_source(src).unwrap();
    let (p, diags) = check_program(&ws, name);
    let text: Vec<String> = diags.iter().map(|d| d.to_string()).collect();
    p.unwrap_or_else(|| panic!("{} should check:\n{}", name, text.join("\n")))
}

fn corpus_strategy(name: &str) -> Program {
    let src = fs::read_to_string(format!("{}/corpus/strategies/{}.dsl", env!("CARGO_MANIFEST_DIR"), name)).unwrap();
    program(&src, name)
}

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("abt-bundle-{}-{}", std::process::id(), name));
    let _ = fs::remove_dir_all(&dir);
    dir
}

#[test]
fn a_bundle_round_trips_a_catalog_market_through_parquet() {
    let p = corpus_strategy("total_return_momentum");
    let ds = synthetic_daily_v2(&["AAA", "BBB", "CCC", "DDD", "SPY"], (2022, 1, 3), 320, 11);
    let dir = scratch("roundtrip");
    let m = write_bundle(&p, &ds, &dir, "equities_1d_v2", "2026.10").unwrap();
    assert_eq!(m.name, "equities_1d_v2");
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
    let (loaded, m2) = load_bundle(&p, &dir, true).unwrap();
    assert_eq!(m2.version, "2026.10");
    assert_eq!(loaded.securities, ds.securities);
    for rel in [
        "open",
        "high",
        "low",
        "close",
        "volume",
        "universe",
        "split",
        "dividend",
        "delisted",
        "member",
        "classification",
        "ticker",
    ] {
        assert_eq!(loaded.facts[rel].len(), ds.facts[rel].len(), "{}", rel);
    }
    // Same symbols, same values: the runs agree exactly.
    let a = run(&p, &ds, ExecConfig::default()).unwrap();
    let b = run(&p, &loaded, ExecConfig::default()).unwrap();
    assert_eq!(a.final_cash.to_bits(), b.final_cash.to_bits());
    assert_eq!(a.decisions.len(), b.decisions.len());
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn the_bundle_tests_pass_on_the_synthetic_catalog_and_are_recorded() {
    let p = corpus_strategy("total_return_momentum");
    let ds = synthetic_daily_v2(&["AAA", "BBB", "CCC", "DDD", "SPY"], (2022, 1, 3), 320, 11);
    let results = run_tests(&p, &ds);
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
    write_bundle(&p, &ds, &dir, "equities_1d_v2", "2026.10").unwrap();
    let (m, results) = test_bundle(&p, &dir).unwrap();
    assert!(results.iter().all(|r| r.passed));
    assert!(m.tests.as_ref().map(|t| !t.passed_at.is_empty()).unwrap_or(false), "{:?}", m.tests);
    let on_disk = read_manifest(&dir).unwrap();
    assert!(on_disk.tests.is_some(), "the manifest records the passing tests");
    // Now the bundle loads without --untested.
    load_bundle(&p, &dir, false).unwrap();
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn a_broken_bundle_fails_the_right_test_and_is_not_marked_tested() {
    let p = corpus_strategy("total_return_momentum");
    let mut ds = synthetic_daily_v2(&["AAA", "BBB", "CCC", "DDD", "SPY"], (2022, 1, 3), 320, 11);
    // Drop the delisting record: a name whose prices stop with no reason.
    ds.facts.remove("delisted");
    let r = run_tests(&p, &ds);
    let cov = r.iter().find(|r| r.name == "delisting coverage").unwrap();
    assert!(!cov.passed && cov.detail.contains("E5"), "{:?}", cov);
    // Drop the split record: the halving on the ex-date is an unexplained gap.
    let mut ds2 = synthetic_daily_v2(&["AAA", "BBB", "CCC", "DDD", "SPY"], (2022, 1, 3), 320, 11);
    ds2.facts.remove("split");
    let r = run_tests(&p, &ds2);
    let rec = r.iter().find(|r| r.name == "action reconciliation").unwrap();
    assert!(!rec.passed && rec.detail.contains("E2"), "{:?}", rec);
    // A split with no gap in the prices.
    let mut ds3 = synthetic_daily_v2(&["AAA", "BBB", "CCC", "DDD", "SPY"], (2022, 1, 3), 320, 11);
    let aaa = ds3.symbols.get("E1").unwrap();
    let t = ds3.facts["close"][5][1].clone();
    ds3.add("split", vec![Value::Equity(aaa), t, Value::Num(3.0)]);
    let r = run_tests(&p, &ds3);
    assert!(!r.iter().find(|r| r.name == "action reconciliation").unwrap().passed);
    // A member that is not in the universe that day.
    let mut ds4 = synthetic_daily_v2(&["AAA", "BBB", "CCC", "DDD", "SPY"], (2022, 1, 3), 320, 11);
    let spx = ds4.intern_label("SPX");
    let gone = ds4.facts["delisted"][0].clone();
    ds4.add("member", vec![gone[0].clone(), gone[1].clone(), Value::Label(spx)]);
    let r = run_tests(&p, &ds4);
    assert!(!r.iter().find(|r| r.name == "membership").unwrap().passed);
    // A failing bundle is written, tested, and left unmarked.
    let dir = scratch("broken");
    write_bundle(&p, &ds, &dir, "equities_1d_v2", "2026.10").unwrap();
    let (m, results) = test_bundle(&p, &dir).unwrap();
    assert!(results.iter().any(|r| !r.passed));
    assert!(m.tests.is_none());
    assert!(load_bundle(&p, &dir, false).is_err());
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn a_strategy_names_the_bundle_version_it_was_written_against() {
    let pinned = fs::read_to_string(format!("{}/corpus/strategies/total_return_momentum.dsl", env!("CARGO_MANIFEST_DIR")))
        .unwrap()
        .replace("env equities_1d_v2\n", "env equities_1d_v2@2026.10\n");
    let p = program(&pinned, "total_return_momentum");
    assert_eq!(p.environment, "equities_1d_v2");
    assert_eq!(p.environment_version.as_deref(), Some("2026.10"));
    let unpinned = corpus_strategy("total_return_momentum");
    assert_eq!(unpinned.environment_version, None);
    let ds = synthetic_daily_v2(&["AAA", "BBB", "CCC", "DDD", "SPY"], (2022, 1, 3), 120, 3);
    let dir = scratch("version");
    write_bundle(&p, &ds, &dir, "equities_1d_v2", "2026.10").unwrap();
    test_bundle(&p, &dir).unwrap();
    load_bundle(&p, &dir, false).unwrap();
    load_bundle(&unpinned, &dir, false).unwrap();
    let other = program(&pinned.replace("@2026.10", "@2026.11"), "total_return_momentum");
    let err = load_bundle(&other, &dir, false).err().unwrap();
    assert!(err.contains("2026.11") && err.contains("2026.10"), "{}", err);
    // The wrong environment altogether.
    let hold = corpus_strategy("sma_crossover");
    let err = load_bundle(&hold, &dir, false).err().unwrap();
    assert!(err.contains("equities_1d") && err.contains("equities_1d_v2"), "{}", err);
    let _ = fs::remove_dir_all(&dir);
    // A library pinned to another version than its strategy is an E error.
    let lib = r#"
library pinned_lib {
  env equities_1d_v2@2026.09
  resolution @1d
  rel always(@T: Timestamp)
  always(T) :- universe(_, T).
}
"#;
    let strat = pinned.replace("uses catalog", "uses catalog, pinned_lib");
    let mut ws = corpus::base_workspace();
    ws.add_source(lib).unwrap();
    ws.add_source(&strat).unwrap();
    let (p, diags) = check_program(&ws, "total_return_momentum");
    assert!(p.is_none());
    assert!(
        diags.iter().any(|d| d.code == Code::E && d.severity == Severity::Error && d.message.contains("2026.09")),
        "{:?}",
        diags.iter().map(|d| d.to_string()).collect::<Vec<_>>()
    );
}

/// E1's closes from bar `k` on, scaled by `scale`, and the bar `k` itself.
fn rescale_from(ds: &mut absolute_backtest::kernel::Dataset, id: &str, k: usize, scale: f64) -> (Value, i64, f64) {
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
    let p = corpus_strategy("total_return_momentum");
    let mut ds = synthetic_daily_v2(&["AAA", "BBB", "CCC", "DDD", "SPY"], (2022, 1, 3), 320, 11);
    let (sym, t, prev) = rescale_from(&mut ds, "E1", 30, 0.5);
    let gap = run_tests(&p, &ds);
    assert!(!gap.iter().find(|r| r.name == "action reconciliation").unwrap().passed, "a bare halving is unexplained");
    ds.add("dividend", vec![sym.clone(), Value::Time(t), Value::Time(t), Value::Time(t), Value::Num(prev * 0.5)]);
    let r = run_tests(&p, &ds);
    let rec = r.iter().find(|r| r.name == "action reconciliation").unwrap();
    assert!(rec.passed, "{:?}", rec);
    // A reverse split on the same day as a distribution (amount per new
    // share): the close is unchanged, the position halves, the cash pays
    // for the rest.
    let mut ds = synthetic_daily_v2(&["AAA", "BBB", "CCC", "DDD", "SPY"], (2022, 1, 3), 320, 11);
    let (sym, t, prev) = rescale_from(&mut ds, "E1", 30, 1.0);
    ds.add("split", vec![sym.clone(), Value::Time(t), Value::Num(0.5)]);
    assert!(!run_tests(&p, &ds).iter().find(|r| r.name == "action reconciliation").unwrap().passed, "a split with no gap");
    ds.add("dividend", vec![sym, Value::Time(t), Value::Time(t), Value::Time(t), Value::Num(prev)]);
    let rec = run_tests(&p, &ds).into_iter().find(|r| r.name == "action reconciliation").unwrap();
    assert!(rec.passed, "{:?}", rec);
}

#[test]
fn a_reviewed_exception_admits_a_real_price_move_and_is_counted() {
    let p = corpus_strategy("total_return_momentum");
    let mut ds = synthetic_daily_v2(&["AAA", "BBB", "CCC", "DDD", "SPY"], (2022, 1, 3), 320, 11);
    let (_, t, _) = rescale_from(&mut ds, "E1", 40, 0.45);
    let rec = run_tests(&p, &ds).into_iter().find(|r| r.name == "action reconciliation").unwrap();
    assert!(!rec.passed);
    let exceptions = vec![Exception {
        test: "action reconciliation".into(),
        security: "E1".into(),
        t,
        reason: "real move: a short-seller report".into(),
    }];
    let rec = run_tests_with(&p, &ds, &exceptions).into_iter().find(|r| r.name == "action reconciliation").unwrap();
    assert!(rec.passed, "{:?}", rec);
    assert!(rec.detail.contains("1 reviewed exception"), "{:?}", rec);
    // An exception for another day does not excuse this one.
    let wrong = vec![Exception {
        t: t + 86_400,
        ..exceptions[0].clone()
    }];
    assert!(!run_tests_with(&p, &ds, &wrong).into_iter().find(|r| r.name == "action reconciliation").unwrap().passed);
    // In a bundle, the exceptions live in exceptions.parquet next to the manifest.
    let dir = scratch("exceptions");
    write_bundle(&p, &ds, &dir, "equities_1d_v2", "2026.10").unwrap();
    let (m, _) = test_bundle(&p, &dir).unwrap();
    assert!(m.tests.is_none(), "without the file the bundle fails");
    absolute_backtest::bundle::write_exceptions(&dir, &exceptions).unwrap();
    let (m, results) = test_bundle(&p, &dir).unwrap();
    assert!(m.tests.is_some(), "{:?}", results);
    let _ = fs::remove_dir_all(&dir);
}
