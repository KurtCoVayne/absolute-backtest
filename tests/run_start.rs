//! What the kernel settles before a run starts, found by the Oct 9 study: a
//! label the data never carries is warned; and the daily reading of the
//! average daily volume at a sub-daily resolution.

mod common;

use absolute_backtest::data::synthetic_daily;
use absolute_backtest::kernel::{Dataset, ExecConfig, Kernel, Value};

/// Buy one share of a member of the list `list` when flat.
fn member_of(name: &str, list: &str) -> String {
    common::StrategyParts {
        name: name.into(),
        needs: format!("need bars @1d   need actions   need listing   need lists \"{list}\""),
        params: "    qty : Quantity<Shares> = 1 shares".into(),
        decides: format!("    decide(T, trade(A, qty)) :- universe(A, T), member(A, T, \"{list}\"), flat(A, T)."),
        ..Default::default()
    }
    .render()
}

/// The synthetic catalog's lists are `SPX` and `SP500`; a program naming
/// `SP400` (a list of the class's vocabulary the data happens not to carry)
/// decides nothing, and the run says so, naming the labels the data holds.
/// A warning and not a refusal: a vocabulary member the data happens not to
/// carry is a legitimate literal that is simply false on it.
#[test]
fn an_unknown_label_is_warned_at_run_start() {
    let prog = common::program(&member_of("wrong_label", "SP400"), "wrong_label");
    let ds = synthetic_daily(&["AAA", "BBB"], (2022, 1, 3), 40, 7);
    let mut k = Kernel::new(&prog, &ds, ExecConfig::frictionless()).unwrap();
    let result = k.run().unwrap();
    assert!(result.decisions.is_empty());
    let w = result.warnings.iter().find(|w| w.bias == "vocabulary").expect("a vocabulary warning");
    assert!(w.message.contains("`SP400`") && w.message.contains("no relation of the data"), "{}", w.message);
    assert!(w.message.contains("SPX"), "{}", w.message);
    // The same program with the data's label runs without the warning.
    let prog = common::program(&member_of("right_label", "SPX"), "right_label");
    let mut k = Kernel::new(&prog, &ds, ExecConfig::frictionless()).unwrap();
    let result = k.run().unwrap();
    assert!(result.warnings.iter().all(|w| w.bias != "vocabulary"));
}

/// Two days of three minute bars with a volume of 100 each: at a minute
/// resolution the ADV at a bar of the second day is the first day's summed
/// volume, 300, not the mean bar volume of 100.
#[test]
fn adv_at_a_minute_resolution_is_a_daily_volume() {
    // Buys above a price floor no bar reaches: never trades.
    let src = common::StrategyParts {
        name: "never_min".into(),
        needs: "need bars @1m   need listing".into(),
        params: "    qty : Quantity<Shares> = 1 shares\n    floor : Price<USD> = 100000 USD/share".into(),
        decides: "    decide(T, trade(A, qty)) :- universe(A, T), close(A, T, P), P > floor.".into(),
        ..Default::default()
    }
    .render();
    let prog = common::program(&src, "never_min");
    let mut ds = Dataset::new();
    let x = ds.add_security("X", "X", common::day("2024-01-02"), None);
    for d in ["2024-01-02", "2024-01-03"] {
        ds.add("listed", vec![Value::Equity(x), Value::Time(common::day(d))]);
    }
    let bars = ["2024-01-02T09:31:00", "2024-01-02T09:32:00", "2024-01-02T09:33:00", "2024-01-03T09:31:00", "2024-01-03T09:32:00"];
    for b in bars {
        let t = common::day(b);
        for rel in ["open", "high", "low", "close"] {
            ds.add(rel, vec![Value::Equity(x), Value::Time(t), Value::Num(10.0)]);
        }
        ds.add("volume", vec![Value::Equity(x), Value::Time(t), Value::Num(100.0)]);
    }
    ds.derive_tickers();
    let k = Kernel::new(&prog, &ds, ExecConfig::frictionless()).unwrap();
    let sym = k.symbols.get("X").unwrap();
    let bars = k.decision_bars();
    assert_eq!(bars.len(), 5);
    // On the second day: the previous day's 300 (the partial current day is left out).
    assert_eq!(k.adv(sym, &bars, 4), Some(300.0));
    // On the first day there is no previous day: the volume so far stands in.
    assert_eq!(k.adv(sym, &bars, 1), Some(200.0));
}
