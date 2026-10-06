//! Stable security identifiers (data-bundle doc, section 3 "Identity" and
//! section 4 "Identity is the survivorship fix"): an `Equity` is an id the
//! bundle assigns, surviving ticker changes and the reuse of a ticker by a
//! later company; `ticker(A, @T, S)` is a relation; a ticker literal names
//! the security carrying it at the bundle date.

mod corpus;

use std::fs;

use absolute_backtest::check::{check_program, Program};
use absolute_backtest::data::{business_days, check_identities, load_parquet_dir, write_parquet_dir};
use absolute_backtest::kernel::time::format_timestamp;
use absolute_backtest::kernel::{run, Dataset, ExecConfig, RunError, Value};
use absolute_backtest::table::{to_text, write_text_table};

fn program(src: &str, name: &str) -> Program {
    let mut ws = corpus::base_workspace();
    ws.add_source(src).unwrap();
    let (p, diags) = check_program(&ws, name);
    let text: Vec<String> = diags.iter().map(|d| d.to_string()).collect();
    p.unwrap_or_else(|| panic!("{} should check:\n{}", name, text.join("\n")))
}

/// Buy the named security once, when flat, and hold it.
const HOLD_NAMED: &str = r#"
strategy hold_named {
  env equities_1d
  uses features
  resolution @1d
  mode delta
  param target : Equity = "BBB"
  param qty : Quantity<Shares> = 10 shares
  decide(T, buy(target, qty)) :- universe(target, T), flat(target, T).
}
"#;

/// Six weekdays from 2024-01-08. E1 is AAA throughout; E2 is BBB for the
/// first three days and BBX from the fourth; E3 lists on the fifth day and
/// takes the ticker BBB.
fn market() -> Dataset {
    let days = business_days((2024, 1, 8), 6);
    let mut ds = Dataset::new();
    let e1 = ds.add_security("E1", "AAA", days[0], None);
    let e2 = ds.add_security("E2", "BBB", days[0], Some(days[3]));
    assert_eq!(ds.add_security("E2", "BBX", days[3], None), e2, "one id, two tickers");
    let e3 = ds.add_security("E3", "BBB", days[4], None);
    for (i, &t) in days.iter().enumerate() {
        for (sym, p, listed) in [(e1, 10.0, true), (e2, 20.0, true), (e3, 5.0, i >= 4)] {
            if !listed {
                continue;
            }
            ds.add("close", vec![Value::Equity(sym), Value::Time(t), Value::Num(p)]);
            ds.add("volume", vec![Value::Equity(sym), Value::Time(t), Value::Num(1_000_000.0)]);
            ds.add("universe", vec![Value::Equity(sym), Value::Time(t)]);
        }
    }
    ds.derive_tickers();
    ds
}

fn day(ds: &Dataset, i: usize) -> i64 {
    let mut keys: Vec<i64> = ds.facts["universe"].iter().map(|tu| tu[1].as_time().unwrap()).collect();
    keys.sort();
    keys.dedup();
    keys[i]
}

#[test]
fn the_ticker_relation_is_derived_from_the_security_table() {
    let ds = market();
    let rows: Vec<(String, String, String)> = ds.facts["ticker"]
        .iter()
        .map(|tu| {
            (
                ds.symbols.name(tu[0].as_equity().unwrap()).to_string(),
                format_timestamp(tu[1].as_time().unwrap()),
                ds.label_name(&tu[2]).unwrap().to_string(),
            )
        })
        .collect();
    assert!(rows.contains(&("E2".into(), "2024-01-10".into(), "BBB".into())), "{:?}", rows);
    assert!(rows.contains(&("E2".into(), "2024-01-11".into(), "BBX".into())), "{:?}", rows);
    assert!(rows.contains(&("E3".into(), "2024-01-12".into(), "BBB".into())), "{:?}", rows);
    assert!(!rows.iter().any(|(id, t, _)| id == "E3" && t.as_str() < "2024-01-12"), "E3 has no ticker before it lists: {:?}", rows);
    assert_eq!(rows.len(), 6 + 6 + 2);
    assert!(check_identities(&ds).is_empty());
    // The bundle date defaults to the last bar.
    assert_eq!(ds.bundle_date(), Some(day(&ds, 5)));
}

#[test]
fn a_ticker_change_keeps_the_position_on_the_same_id() {
    let ds = market();
    let p = program(HOLD_NAMED, "hold_named");
    // As of the bundle date (the last bar) BBB is E3; ask for E2 by its old name as of day 2.
    let cfg = ExecConfig {
        initial_cash: 10_000.0,
        as_of: Some(day(&ds, 1)),
        ..ExecConfig::frictionless()
    };
    let r = run(&p, &ds, cfg).unwrap();
    assert_eq!(r.fills.len(), 1, "{:?}", r.fills);
    assert_eq!(r.symbols[r.fills[0].equity as usize], "E2");
    // The rename on day 4 changes nothing about the position.
    assert_eq!(
        r.final_positions.iter().map(|(s, q)| (r.symbols[*s as usize].clone(), *q)).collect::<Vec<_>>(),
        vec![("E2".to_string(), 10.0)]
    );
    assert_eq!(r.decisions.len(), 1, "flat(E2) is false after the buy whatever its ticker: {:?}", r.decisions.len());
}

#[test]
fn a_reused_ticker_resolves_by_the_bundle_date() {
    let ds = market();
    let p = program(HOLD_NAMED, "hold_named");
    // Default bundle date: the last bar, when BBB is the new listing E3.
    let r = run(
        &p,
        &ds,
        ExecConfig {
            initial_cash: 10_000.0,
            ..ExecConfig::frictionless()
        },
    )
    .unwrap();
    assert_eq!(r.fills.len(), 1, "{:?}", r.fills);
    assert_eq!(r.symbols[r.fills[0].equity as usize], "E3");
    assert_eq!(format_timestamp(r.fills[0].t), "2024-01-15", "E3 is in the universe from day 5; the decision fills on day 6");
    // A bundle date before the listing resolves to E2.
    let r = run(
        &p,
        &ds,
        ExecConfig {
            initial_cash: 10_000.0,
            as_of: Some(day(&ds, 2)),
            ..ExecConfig::frictionless()
        },
    )
    .unwrap();
    assert_eq!(r.symbols[r.fills[0].equity as usize], "E2", "BBB as of day 3 is still E2");
}

#[test]
fn a_ticker_nobody_carries_at_the_bundle_date_is_a_configuration_error() {
    let ds = market();
    let p = program(&HOLD_NAMED.replace("\"BBB\"", "\"ZZZ\""), "hold_named");
    let err = run(&p, &ds, ExecConfig::frictionless()).err().unwrap();
    match &err {
        RunError::Config(m) => assert!(m.contains("ZZZ") && m.contains("2024-01-15"), "{}", m),
        e => panic!("expected a configuration error, got {}", e),
    }
    // BBX exists only from day 4: as of day 2 it is unknown.
    let p = program(&HOLD_NAMED.replace("\"BBB\"", "\"BBX\""), "hold_named");
    let err = run(
        &p,
        &ds,
        ExecConfig {
            as_of: Some(day(&ds, 1)),
            ..ExecConfig::frictionless()
        },
    )
    .err()
    .unwrap();
    assert!(err.to_string().contains("BBX"), "{}", err);
    // A dataset without a security table is the old world: the ticker is the id.
    let legacy = absolute_backtest::data::synthetic_daily(&["AAA", "BBB"], (2024, 1, 8), 6, 1);
    let r = run(&program(HOLD_NAMED, "hold_named"), &legacy, ExecConfig::frictionless()).unwrap();
    assert_eq!(r.symbols[r.fills[0].equity as usize], "BBB");
}

#[test]
fn the_identity_bundle_tests_catch_overlaps() {
    let days = business_days((2024, 1, 8), 6);
    let mut ds = Dataset::new();
    ds.add_security("E1", "AAA", days[0], None);
    ds.add_security("E2", "AAA", days[2], None);
    let errs = check_identities(&ds);
    assert_eq!(errs.len(), 1, "{:?}", errs);
    assert!(errs[0].contains("AAA") && errs[0].contains("E1") && errs[0].contains("E2"), "{}", errs[0]);
    let mut ds = Dataset::new();
    ds.add_security("E1", "AAA", days[0], Some(days[3]));
    ds.add_security("E1", "AAB", days[2], None);
    let errs = check_identities(&ds);
    assert!(errs.iter().any(|e| e.contains("E1") && e.contains("overlap")), "{:?}", errs);
    let mut ds = Dataset::new();
    ds.add_security("E1", "AAA", days[3], Some(days[1]));
    assert!(check_identities(&ds).iter().any(|e| e.contains("before")), "{:?}", check_identities(&ds));
}

#[test]
fn the_security_table_loads_and_round_trips_through_parquet() {
    let p = program(HOLD_NAMED, "hold_named");
    let dir = std::env::temp_dir().join(format!("abt-identity-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    write_parquet_dir(&p, &market(), &dir).unwrap();
    let securities = to_text(&dir.join("securities.parquet")).unwrap();
    assert!(securities.starts_with("id,ticker,from,to\n"), "{}", securities);
    assert!(securities.contains("E2,BBB,2024-01-08,2024-01-11\n") && securities.contains("E2,BBX,2024-01-11,\n"), "{}", securities);
    // The derived ticker relation is not written: it is rebuilt from the table.
    assert!(!dir.join("ticker.parquet").exists());
    let (ds, notes) = load_parquet_dir(&p, &dir).unwrap();
    assert!(notes.is_empty(), "{:?}", notes);
    assert_eq!(ds.securities.len(), 4);
    assert_eq!(ds.facts["ticker"].len(), 14);
    let r = run(
        &p,
        &ds,
        ExecConfig {
            initial_cash: 10_000.0,
            ..ExecConfig::frictionless()
        },
    )
    .unwrap();
    assert_eq!(r.symbols[r.fills[0].equity as usize], "E3");
    // An equity field that is not a security id is an error naming the row.
    write_text_table(&dir.join("close.parquet"), "A,T,P\nE9,2024-01-08,1.0\n").unwrap();
    let err = load_parquet_dir(&p, &dir).expect_err("E9 is not in the table");
    assert!(err.contains("close.parquet row 1") && err.contains("E9") && err.contains("securities.parquet"), "{}", err);
    // A malformed table is an error too.
    write_text_table(&dir.join("securities.parquet"), "id,ticker,from,to\nE1,AAA,2024-01-08,\nE2,AAA,2024-01-09,\n").unwrap();
    let err = load_parquet_dir(&p, &dir).expect_err("overlapping tickers");
    assert!(err.contains("AAA"), "{}", err);
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn an_input_or_binding_names_a_security_by_id_or_by_ticker() {
    let ds = market();
    let p = program(HOLD_NAMED, "hold_named");
    let k = absolute_backtest::kernel::Kernel::new(&p, &ds, ExecConfig::frictionless()).unwrap();
    assert_eq!(k.parse_binding("E2").unwrap(), Value::Equity(ds.symbols.get("E2").unwrap()));
    assert_eq!(k.parse_binding("BBX").unwrap(), Value::Equity(ds.symbols.get("E2").unwrap()), "a ticker resolves as of the bundle date");
    assert_eq!(k.parse_binding("BBB").unwrap(), Value::Equity(ds.symbols.get("E3").unwrap()));
    assert!(k.parse_binding("ZZZ").is_err());
}
