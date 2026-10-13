//! Order types (semantic model, section 6): a decision names how it
//! executes. `market` (the default) fills at the next bar's close, `moo` at
//! the instrument's next open, `moc` at the close of the last bar of the
//! decision's session, and `limit` / `stop` when a bar trades through their
//! level, within their time in force (`day`, `gtc`, `bars(N)`).

mod common;

use std::collections::BTreeSet;

use absolute_backtest::check::Program;
use absolute_backtest::data::business_days;
use absolute_backtest::kernel::time::format_timestamp;
use absolute_backtest::kernel::{run, Dataset, ExecConfig, Lot, OrderKind, Value};

fn checked(src: &str, name: &str) -> (Option<Program>, Vec<String>) {
    let (p, diags) = common::check(src, name);
    (p, diags.iter().map(|d| d.to_string()).collect())
}

/// Buy 10 of X on the first bar, with the given order term.
fn buy_once(order: &str) -> String {
    common::strategy(
        "buy_once",
        "    qty : Quantity<Shares> = 10 shares\n    level : Price<USD> = 9.5 USD/share",
        "    rel later(@T)\n    later(T) :- bar(T), prev(T, _).",
        &format!(
            "    decide(T, trade(A, qty{})) :- universe(A, T), flat(A, T), not later(T).",
            if order.is_empty() { String::new() } else { format!(", {}", order) }
        ),
    )
}

/// One symbol X over weekdays from 2024-01-08, bars as (open, high, low, close).
fn market(bars: &[(f64, f64, f64, f64)]) -> (Dataset, Vec<i64>) {
    let days = business_days((2024, 1, 8), bars.len());
    let mut ds = Dataset::new();
    let x = ds.add_security("X", "X", days[0], None);
    for (t, (o, h, l, c)) in days.iter().zip(bars) {
        for (rel, p) in [("open", o), ("high", h), ("low", l), ("close", c)] {
            ds.add(rel, vec![Value::Equity(x), Value::Time(*t), Value::Num(*p)]);
        }
        ds.add("volume", vec![Value::Equity(x), Value::Time(*t), Value::Num(1_000_000.0)]);
        ds.add("listed", vec![Value::Equity(x), Value::Time(*t)]);
    }
    ds.derive_tickers();
    (ds, days)
}

/// One symbol X over minute bars, each (stamp, open, close) with the high
/// and low at the wider of the two, listed on every day it has a bar.
fn minute_market(bars: &[(&str, f64, f64)]) -> Dataset {
    let mut ds = Dataset::new();
    let first = common::day(bars[0].0);
    let x = ds.add_security("X", "X", first - first.rem_euclid(86_400), None);
    let mut days = BTreeSet::new();
    for (stamp, o, c) in bars {
        let t = common::day(stamp);
        days.insert(t - t.rem_euclid(86_400));
        for (rel, p) in [("open", *o), ("high", o.max(*c)), ("low", o.min(*c)), ("close", *c)] {
            ds.add(rel, vec![Value::Equity(x), Value::Time(t), Value::Num(p)]);
        }
        ds.add("volume", vec![Value::Equity(x), Value::Time(t), Value::Num(1000.0)]);
    }
    for d in days {
        ds.add("listed", vec![Value::Equity(x), Value::Time(d)]);
    }
    ds.derive_tickers();
    ds
}

fn cfg() -> ExecConfig {
    ExecConfig {
        initial_cash: 10_000.0,
        margin_rate: 0.0,
        lot: Lot::Fractional,
        ..ExecConfig::frictionless()
    }
}

const BARS: [(f64, f64, f64, f64); 4] = [(10.0, 10.5, 9.8, 10.2), (10.4, 10.6, 9.4, 9.6), (9.0, 9.3, 8.8, 9.1), (9.2, 9.9, 9.1, 9.8)];

fn fills(src: &str, bars: &[(f64, f64, f64, f64)]) -> Vec<(String, f64)> {
    let (ds, _) = market(bars);
    let r = run(&common::program(src, "buy_once"), &ds, cfg()).unwrap();
    r.fills.iter().map(|f| (format_timestamp(f.t), f.price)).collect()
}

#[test]
fn market_fills_at_the_next_close_and_moc_at_the_decision_close() {
    assert_eq!(fills(&buy_once(""), &BARS), vec![("2024-01-09".into(), 9.6)]);
    assert_eq!(fills(&buy_once("market"), &BARS), vec![("2024-01-09".into(), 9.6)]);
    // At @1d the session of the decision is its own bar: its close.
    assert_eq!(fills(&buy_once("moc"), &BARS), vec![("2024-01-08".into(), 10.2)]);
}

#[test]
fn moo_fills_at_the_next_open() {
    assert_eq!(fills(&buy_once("moo"), &BARS), vec![("2024-01-09".into(), 10.4)]);
}

#[test]
fn a_limit_fills_at_its_level_or_at_an_open_through_it_and_expires_by_day() {
    // Day 2 trades down to 9.4 through the 9.5 level: filled at 9.5.
    assert_eq!(fills(&buy_once("limit(level)"), &BARS), vec![("2024-01-09".into(), 9.5)]);
    // A level of 9.2 is not reached on day 2: a day order expires.
    let (ds, _) = market(&BARS);
    let r = run(&common::program(&buy_once("limit(9.2 USD/share)"), "buy_once"), &ds, cfg()).unwrap();
    assert!(r.fills.is_empty(), "{:?}", r.fills);
    assert!(
        r.dropped.iter().any(|(_, d, why)| matches!(d.order.kind, OrderKind::Limit(_)) && why.contains("expired")),
        "{:?}",
        r.dropped
    );
    // Good till cancelled, it works on: day 3 opens at 9.0, through the level.
    assert_eq!(fills(&buy_once("limit(9.2 USD/share, gtc)"), &BARS), vec![("2024-01-10".into(), 9.0)]);
    // Two bars of life reach day 3 too; one does not.
    assert_eq!(fills(&buy_once("limit(9.2 USD/share, bars(2))"), &BARS), vec![("2024-01-10".into(), 9.0)]);
    assert!(fills(&buy_once("limit(9.2 USD/share, bars(1))"), &BARS).is_empty());
}

#[test]
fn a_stop_buy_fills_once_the_market_trades_up_through_it() {
    // A buy stop at 10.55: day 2's high is 10.6.
    assert_eq!(fills(&buy_once("stop(10.55 USD/share)"), &BARS), vec![("2024-01-09".into(), 10.55)]);
    // At 9.0 the bar opens above the stop: filled at the open.
    assert_eq!(fills(&buy_once("stop(9.0 USD/share)"), &BARS), vec![("2024-01-09".into(), 10.4)]);
}

#[test]
fn a_new_decision_supersedes_a_working_order() {
    // A gtc limit far below the market, then a market buy the next day.
    let src = common::strategy(
        "supersede",
        "    qty : Quantity<Shares> = 10 shares",
        "    rel later(@T)\n    later(T) :- bar(T), prev(T, _).",
        "    decide(T, trade(A, qty, limit(1.0 USD/share, gtc))) :- universe(A, T), not later(T).\n    decide(T, trade(A, qty)) :- universe(A, T), prev(T, T1), not later(T1).",
    );
    let (ds, _) = market(&BARS);
    let r = run(&common::program(&src, "supersede"), &ds, cfg()).unwrap();
    let got: Vec<(String, f64)> = r.fills.iter().map(|f| (format_timestamp(f.t), f.price)).collect();
    assert_eq!(got, vec![("2024-01-10".into(), 9.1)], "only the market order fills");
}

#[test]
fn an_intraday_moc_fills_at_the_sessions_last_print() {
    let src = common::StrategyParts {
        name: "minute_moc".into(),
        needs: "need bars @1m   need listing".into(),
        params: "    qty : Quantity<Shares> = 5 shares".into(),
        decides: "    decide(T, trade(A, qty, moc)) :- universe(A, T), day_start(T), flat(A, T).".into(),
        ..Default::default()
    }
    .render();
    // Two sessions of four minute bars; the last print of day one is 09:34.
    let ds = minute_market(&[
        ("2024-01-08T09:31:00", 10.0, 10.0),
        ("2024-01-08T09:32:00", 10.1, 10.1),
        ("2024-01-08T09:33:00", 10.2, 10.2),
        ("2024-01-08T09:34:00", 10.3, 10.3),
        ("2024-01-09T09:31:00", 11.0, 11.0),
        ("2024-01-09T09:32:00", 11.1, 11.1),
    ]);
    let r = run(&common::program(&src, "minute_moc"), &ds, cfg()).unwrap();
    let got: Vec<(String, f64)> = r.fills.iter().map(|f| (format_timestamp(f.t), f.price)).collect();
    assert_eq!(got, vec![("2024-01-08T09:34:00".into(), 10.3)], "{:?}", r.dropped);
    // The position is held through the session and is in the book from the next bar.
    assert_eq!(r.final_positions.values().copied().collect::<Vec<_>>(), vec![5.0]);
}

#[test]
fn the_checker_admits_an_order_only_in_a_decide_head_and_judges_its_shape() {
    for (order, needle) in [
        ("limit", "an order is"),
        ("limit(qty)", "parameter `qty` is Quantity<Shares>"),
        ("limit(level, forever)", "not a time in force"),
        ("stop(level, bars(1.5))", "bars"),
        ("fok", "an order is"),
    ] {
        let src = buy_once(order).replace("level : Price<USD> = 9.5 USD/share", "level : Price<USD> = 9.5 USD/share\n    n : Count = 2");
        let (p, diags) = checked(&src, "buy_once");
        assert!(p.is_none() && diags.iter().any(|d| d.contains(needle)), "{}: {:?}", order, diags);
    }
    let (p, diags) = checked(&buy_once("stop(level, bars(n))").replace("    level", "    n : Count = 2\n    level"), "buy_once");
    assert!(p.is_some(), "{:?}", diags);
    // A decided pattern takes the two fields and an optional order, nothing more.
    let pattern = |ctor: &str| {
        common::strategy(
            "pattern",
            "    qty : Quantity<Shares> = 10 shares",
            "",
            &format!("    decide(T, trade(A, qty)) :- universe(A, T), prev(T, T0), not decided(T0, {}).", ctor),
        )
    };
    let (p, diags) = checked(&pattern("trade(A, _, moc)"), "pattern");
    assert!(p.is_some(), "{:?}", diags);
    let (p, diags) = checked(&pattern("trade(A, _, moc, moc)"), "pattern");
    assert!(p.is_none() && diags.iter().any(|d| d.contains("an instrument and an amount, with an optional order")), "{:?}", diags);
}

/// `moo_moc`: an intraday position, opened at the instrument's next open
/// and flattened at the close of that session; when the entry is the
/// session's last print, it is flat again at that print's close.
#[test]
fn a_moo_moc_order_opens_at_the_next_open_and_is_flat_at_the_session_close() {
    let src = common::StrategyParts {
        name: "day_trade".into(),
        needs: "need bars @1m   need listing".into(),
        params: "    qty : Quantity<Shares> = 2 shares".into(),
        rules: "    rel first_bar(A: Equity, @T)\n    first_bar(A, T) :- universe(A, T), day_start(T).".into(),
        decides: "    decide(T, trade(A, qty, moo_moc)) :- first_bar(A, T).".into(),
        ..Default::default()
    }
    .render();
    let p = common::program(&src, "day_trade");
    // Day one: three bars (open, close); day two: two bars.
    let ds = minute_market(&[
        ("2024-01-08T09:31:00", 10.0, 10.5),
        ("2024-01-08T09:32:00", 10.6, 11.0),
        ("2024-01-08T09:33:00", 11.1, 12.0),
        ("2024-01-09T09:31:00", 20.0, 20.5),
        ("2024-01-09T09:32:00", 20.6, 21.0),
    ]);
    let r = run(&p, &ds, cfg()).unwrap();
    let got: Vec<(String, f64, f64)> = r.fills.iter().map(|f| (format_timestamp(f.t), f.quantity, f.price)).collect();
    assert_eq!(
        got,
        vec![
            // Day one: in at 09:32's open, out at 09:33's close.
            ("2024-01-08T09:32:00".into(), 2.0, 10.6),
            ("2024-01-08T09:33:00".into(), -2.0, 12.0),
            // Day two: in at 09:32's open, the session's last print, out at its close.
            ("2024-01-09T09:32:00".into(), 2.0, 20.6),
            ("2024-01-09T09:32:00".into(), -2.0, 21.0),
        ],
        "{:?}",
        r.dropped
    );
    assert!(r.final_positions.is_empty());
}
