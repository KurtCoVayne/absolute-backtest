//! The executor's policies for degenerate book states (section 6, executor
//! policy): ruin and leverage halt by default, oversize delta orders halt by
//! default, liquidations fill at the last known price, and every order is
//! rounded to whole shares unless configured otherwise.

use abt_tests as common;

use abt_kernel::kernel::time::format_timestamp;
use abt_kernel::kernel::{Dataset, ExecConfig, Lot, OnLeverage, OnOversize, OnRuin, RunError, Value};
use abt_tests::run;

/// One symbol X over five weekdays; `None` leaves the bar's prices out (the
/// bar still exists through the listing and the volume).
fn market(prices: &[Option<f64>]) -> Dataset {
    let mut ds = Dataset::new();
    let days = ["2024-01-08", "2024-01-09", "2024-01-10", "2024-01-11", "2024-01-12"];
    let x = ds.add_security("X", "X", common::day(days[0]), None);
    for (d, p) in days.iter().zip(prices) {
        let t = common::day(d);
        match p {
            Some(p) => common::add_bar(&mut ds, x, t, *p, 1000.0),
            None => {
                ds.add("volume", vec![Value::Equity(x), Value::Time(t), Value::Num(1000.0)]);
                ds.add("listed", vec![Value::Equity(x), Value::Time(t)]);
            }
        }
    }
    ds.derive_tickers();
    ds
}

fn cfg(cash: f64) -> ExecConfig {
    ExecConfig {
        initial_cash: cash,
        ..ExecConfig::frictionless()
    }
}

fn risk_message(r: Result<abt_kernel::kernel::RunResult, RunError>) -> String {
    match r {
        Err(RunError::Risk { message, .. }) => message,
        Err(e) => panic!("expected a risk halt, got {}", e),
        Ok(r) => panic!("expected a risk halt, the run finished with {} fills", r.fills.len()),
    }
}

// ---- lot rounding (#19) ----

/// Buy 10.5 shares once, when flat.
fn fractional() -> String {
    common::strategy("fractional", "    q : Quantity<Shares> = 10.5 shares", "", "    decide(T, trade(A, q)) :- universe(A, T), flat(A, T).")
}

#[test]
fn orders_round_to_whole_shares_by_default_and_fractional_on_request() {
    let prog = common::program(&fractional(), "fractional");
    let ds = market(&[Some(10.0), Some(10.0), Some(10.0), Some(10.0), Some(10.0)]);
    let r = run(&prog, &ds, cfg(1000.0)).unwrap();
    assert_eq!(r.fills[0].quantity, 10.0, "{:?}", r.fills);
    assert_eq!(r.final_cash, 900.0);
    let r = run(&prog, &ds, ExecConfig { lot: Lot::Fractional, ..cfg(1000.0) }).unwrap();
    assert_eq!(r.fills[0].quantity, 10.5, "{:?}", r.fills);
    assert_eq!(r.final_cash, 895.0);
}

#[test]
fn target_quantity_rounds_like_the_other_orders() {
    let src = common::strategy("tq", "    q : Quantity<Shares> = 10.5 shares", "", "    decide(T, hold(A, q)) :- universe(A, T).");
    let prog = common::program(&src, "tq");
    let ds = market(&[Some(10.0), Some(10.0), Some(10.0), Some(10.0), Some(10.0)]);
    let r = run(&prog, &ds, cfg(1000.0)).unwrap();
    // One fill of 10, then the target 10.5 rounds to 10 again: no drift trade.
    assert_eq!(r.fills.iter().map(|f| f.quantity).collect::<Vec<_>>(), vec![10.0]);
}

// ---- oversize delta orders (#6, #33) ----

/// Buy 100 when flat (once a month), then sell 150 of them.
fn oversell() -> String {
    common::strategy(
        "oversell",
        "    qty : Quantity<Shares> = 100 shares\n    big : Quantity<Shares> = 150 shares",
        "",
        "    decide(T, trade(A, qty)) :- universe(A, T), flat(A, T), not bought_within[30d](A, T).\n    decide(T, trade(A, Q)) :- held(A, T, _), Q = -big.",
    )
}

#[test]
fn a_sell_beyond_the_position_halts_by_default() {
    let prog = common::program(&oversell(), "oversell");
    let ds = market(&[Some(10.0), Some(10.0), Some(10.0), Some(10.0), Some(10.0)]);
    let m = risk_message(run(&prog, &ds, cfg(10_000.0)));
    // The one constructor names the sale by its sign: a trade of -150 against 100 held.
    assert!(m.contains("-150") && m.contains("100") && m.contains("trade") && m.contains("held"), "{}", m);
}

#[test]
fn a_sell_beyond_the_position_is_clamped_or_allowed_on_request() {
    let prog = common::program(&oversell(), "oversell");
    let ds = market(&[Some(10.0), Some(10.0), Some(10.0), Some(10.0), Some(10.0)]);
    let r = run(
        &prog,
        &ds,
        ExecConfig {
            on_oversize: OnOversize::Clamp,
            ..cfg(10_000.0)
        },
    )
    .unwrap();
    let qty: Vec<f64> = r.fills.iter().map(|f| f.quantity).collect();
    assert_eq!(qty[..2], [100.0, -100.0], "{:?}", qty);
    assert!(r.dropped.iter().any(|(_, _, why)| why.contains("clamped") && why.contains("50")), "{:?}", r.dropped);
    let r = run(
        &prog,
        &ds,
        ExecConfig {
            on_oversize: OnOversize::Allow,
            ..cfg(10_000.0)
        },
    )
    .unwrap();
    assert_eq!(r.fills.iter().map(|f| f.quantity).collect::<Vec<_>>()[..2], [100.0, -150.0]);
    assert_eq!(r.final_positions.values().copied().next(), Some(-50.0));
}

// ---- leverage (#17, #33) ----

/// Target 2.4 times the book, every bar.
fn levered() -> String {
    common::strategy("levered", "    w : Scalar = 2.4", "", "    decide(T, hold(A, w)) :- universe(A, T).")
}

#[test]
fn borrowing_to_buy_halts_by_default() {
    let prog = common::program(&levered(), "levered");
    let ds = market(&[Some(10.0), Some(10.0), Some(10.0), Some(10.0), Some(10.0)]);
    let m = risk_message(run(&prog, &ds, cfg(1000.0)));
    assert!(m.contains("equity"), "{}", m);
}

#[test]
fn borrowing_is_rejected_or_allowed_on_request() {
    let prog = common::program(&levered(), "levered");
    let ds = market(&[Some(10.0), Some(10.0), Some(10.0), Some(10.0), Some(10.0)]);
    let r = run(
        &prog,
        &ds,
        ExecConfig {
            on_leverage: OnLeverage::Reject,
            ..cfg(1000.0)
        },
    )
    .unwrap();
    assert!(r.fills.is_empty(), "{:?}", r.fills);
    assert!(r.dropped.iter().filter(|(_, _, why)| why != "no next bar").all(|(_, _, why)| why.contains("equity")), "{:?}", r.dropped);
    assert_eq!(r.final_cash, 1000.0);
    let r = run(
        &prog,
        &ds,
        ExecConfig {
            on_leverage: OnLeverage::Allow,
            ..cfg(1000.0)
        },
    )
    .unwrap();
    assert_eq!(r.fills[0].quantity, 240.0);
    assert_eq!(r.final_cash, 1000.0 - 2400.0);
}

#[test]
fn a_short_beyond_equity_halts_by_default() {
    let src = common::strategy(
        "naked",
        "    qty : Quantity<Shares> = 150 shares",
        "",
        "    decide(T, trade(A, Q)) :- universe(A, T), flat(A, T), Q = -qty.",
    );
    let prog = common::program(&src, "naked");
    let ds = market(&[Some(10.0), Some(10.0), Some(10.0), Some(10.0), Some(10.0)]);
    // Equity 1000 against 1500 of short exposure.
    let m = risk_message(run(&prog, &ds, cfg(1000.0)));
    assert!(m.contains("equity"), "{}", m);
    // 50 shares short is within equity.
    let prog = common::program(&src.replace("150 shares", "50 shares"), "naked");
    run(&prog, &ds, cfg(1000.0)).unwrap();
}

// ---- ruin (#15, #33) ----

/// Short 100 on the first bar; once the price is above the trigger, target
/// half the book long.
fn ruined() -> String {
    common::strategy(
        "ruined",
        "    q : Quantity<Shares> = -100 shares\n    w : Scalar = 0.5\n    trigger : Price<USD> = 20 USD/share",
        "    rel has_prev(@T)\n    has_prev(T) :- bar(T), prev(T, _).",
        "    decide(T, hold(A, q)) :- universe(A, T), not has_prev(T).\n    decide(T, hold(A, w)) :- position(A, T, Q), Q < 0 shares, close(A, T, P), P > trigger.",
    )
}

#[test]
fn a_ruined_book_halts_by_default_and_targets_flat_on_request() {
    let prog = common::program(&ruined(), "ruined");
    // Short 100 at 10 (cash 2000), then the price triples: equity is -1000
    // when the re-weighting order comes to be filled.
    let ds = market(&[Some(10.0), Some(10.0), Some(30.0), Some(30.0), Some(30.0)]);
    let m = risk_message(run(&prog, &ds, cfg(1000.0)));
    assert!(m.contains("ruin") || m.contains("equity"), "{}", m);
    let r = run(
        &prog,
        &ds,
        ExecConfig {
            on_ruin: OnRuin::Continue,
            compounding: true,
            ..cfg(1000.0)
        },
    )
    .unwrap();
    // A positive weight of a non-positive equity targets flat: the short is covered.
    let qty: Vec<f64> = r.fills.iter().map(|f| f.quantity).collect();
    assert_eq!(qty, vec![-100.0, 100.0], "{:?}", r.fills);
    assert!(r.final_positions.is_empty());
    assert_eq!(r.final_cash, 2000.0 - 3000.0);
}

// ---- liquidation at the last price (#16) ----

/// Buy 10 when flat and sell them the bar after, over a universe of the
/// listed bars (an unpriced bar is still a row of the name).
fn round_trip() -> String {
    common::StrategyParts {
        name: "round_trip".into(),
        universe: "universe(A, T) :- listed(A, T).".into(),
        params: "    qty : Quantity<Shares> = 10 shares".into(),
        decides: "    decide(T, trade(A, qty)) :- universe(A, T), flat(A, T).\n    decide(T, trade(A, Q)) :- held(A, T, Q0), Q = -Q0.".into(),
        ..Default::default()
    }
    .render()
}

#[test]
fn a_liquidation_without_a_bar_price_fills_at_the_last_price() {
    let prog = common::program(&round_trip(), "round_trip");
    // Priced on the first two days only; listing and volume rows continue.
    let ds = market(&[Some(10.0), Some(12.0), None, None, None]);
    let r = run(&prog, &ds, cfg(1000.0)).unwrap();
    // Buy decided day 1, filled day 2 at 12; sell decided day 2, filled day 3
    // at the last price (12) although day 3 has no close; the re-entry
    // decided day 3 is an opening order on an unpriced instrument and drops.
    let fills: Vec<(String, f64, f64, bool)> = r.fills.iter().map(|f| (format_timestamp(f.t), f.quantity, f.price, f.at_last_price)).collect();
    assert_eq!(fills, vec![("2024-01-09".to_string(), 10.0, 12.0, false), ("2024-01-10".to_string(), -10.0, 12.0, true)]);
    assert!(r.final_positions.is_empty());
    assert!(
        r.dropped.iter().any(|(t, d, why)| format_timestamp(*t) == "2024-01-10" && d.amount == 10.0 && why.contains("no price")),
        "{:?}",
        r.dropped
    );
}
