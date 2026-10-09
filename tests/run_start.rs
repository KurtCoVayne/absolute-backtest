//! What the kernel refuses before a run starts, found by the Oct 9 study: an
//! order that needs a print the data has no relation for, and a label the
//! data never carries; and the daily reading of the average daily volume at
//! a sub-daily resolution.

mod corpus;

use absolute_backtest::check::{check_program, Program, Workspace};
use absolute_backtest::data::{synthetic_daily_v2, synthetic_minute};
use absolute_backtest::kernel::time::parse_timestamp;
use absolute_backtest::kernel::{Dataset, ExecConfig, Kernel, Value};

fn program(extra: &str, name: &str) -> (Program, Workspace) {
    let mut ws = corpus::base_workspace();
    ws.add_source(extra).unwrap();
    let (p, diags) = check_program(&ws, name);
    let text: Vec<String> = diags.iter().map(|d| d.to_string()).collect();
    (p.unwrap_or_else(|| panic!("{} should check:\n{}", name, text.join("\n"))), ws)
}

const MOO_MIN: &str = r#"
strategy moo_min {
  env equities_1m
  uses features_m
  resolution @1m
  mode delta
  param qty : Quantity<Shares> = 1 shares
  decide(T, buy(A, qty, moo)) :- universe_m(A, T), flat_m(A, T).
}
"#;

/// The minute environment has `close_m` and no `open_m`: a market-on-open
/// order could never fill, so the run is refused before it starts.
#[test]
fn an_order_without_its_open_series_is_refused_at_run_start() {
    let (prog, _) = program(MOO_MIN, "moo_min");
    let ds = synthetic_minute(&["AAA"], (2024, 1, 2), 2, 30, 7);
    let err = Kernel::new(&prog, &ds, ExecConfig::frictionless()).err().expect("refused").to_string();
    assert!(err.contains("moo_min::decide#1"), "{}", err);
    assert!(err.contains("`moo` order"), "{}", err);
    assert!(err.contains("`close_m`") && err.contains("no open companion") && err.contains("`open_m`"), "{}", err);
    assert!(err.contains("use `market` or `moc`"), "{}", err);
}

const WRONG_LABEL: &str = r#"
strategy wrong_label {
  env equities_1d_v2
  uses catalog
  resolution @1d
  mode delta
  param qty : Quantity<Shares> = 1 shares
  decide(T, buy(A, qty)) :- universe(A, T), member(A, T, "NDX"), flat(A, T).
}
"#;

/// The synthetic catalog's index is labelled `SPX`; a program naming `NDX`
/// would decide nothing, so it is refused naming the labels the data holds.
#[test]
fn an_unknown_label_is_refused_at_run_start() {
    let (prog, _) = program(WRONG_LABEL, "wrong_label");
    let ds = synthetic_daily_v2(&["AAA", "BBB"], (2022, 1, 3), 40, 7);
    let err = Kernel::new(&prog, &ds, ExecConfig::frictionless()).err().expect("refused").to_string();
    assert!(err.contains("`NDX`") && err.contains("no relation of the data"), "{}", err);
    assert!(err.contains("SPX"), "{}", err);
    // The same program with the data's label runs.
    let (prog, _) = program(&WRONG_LABEL.replace("NDX", "SPX").replace("wrong_label", "right_label"), "right_label");
    assert!(Kernel::new(&prog, &ds, ExecConfig::frictionless()).is_ok());
}

const NEVER_MIN: &str = r#"
strategy never_min {
  env equities_1m
  resolution @1m
  mode delta
  param qty : Quantity<Shares> = 1 shares
  param floor : Price<USD> = 100000 USD/share
  decide(T, buy(A, qty)) :- universe_m(A, T), close_m(A, T, P), P > floor.
}
"#;

/// Two days of three minute bars with a volume of 100 each: at a minute
/// resolution the ADV at a bar of the second day is the first day's summed
/// volume, 300, not the mean bar volume of 100.
#[test]
fn adv_at_a_minute_resolution_is_a_daily_volume() {
    let (prog, _) = program(NEVER_MIN, "never_min");
    let mut ds = Dataset::new();
    let x = ds.intern("X");
    let bars = ["2024-01-02T09:31:00", "2024-01-02T09:32:00", "2024-01-02T09:33:00", "2024-01-03T09:31:00", "2024-01-03T09:32:00"];
    for b in bars {
        let t = parse_timestamp(b).unwrap();
        ds.add("close_m", vec![Value::Equity(x), Value::Time(t), Value::Num(10.0)]);
        ds.add("volume_m", vec![Value::Equity(x), Value::Time(t), Value::Num(100.0)]);
        ds.add("universe_m", vec![Value::Equity(x), Value::Time(t)]);
    }
    let k = Kernel::new(&prog, &ds, ExecConfig::frictionless()).unwrap();
    let sym = k.symbols.get("X").unwrap();
    let bars = k.decision_bars();
    assert_eq!(bars.len(), 5);
    // On the second day: the previous day's 300 (the partial current day is left out).
    assert_eq!(k.adv(sym, &bars, 4), Some(300.0));
    // On the first day there is no previous day: the volume so far stands in.
    assert_eq!(k.adv(sym, &bars, 1), Some(200.0));
}
