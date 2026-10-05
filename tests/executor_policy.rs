//! The executor's policies for degenerate book states (section 6, executor
//! policy): ruin and leverage halt by default, oversize delta orders halt by
//! default, liquidations fill at the last known price, and every order is
//! rounded to whole shares unless configured otherwise.

mod corpus;

use absolute_backtest::check::{check_program, Program};
use absolute_backtest::kernel::time::{format_timestamp, parse_timestamp};
use absolute_backtest::kernel::{run, Dataset, ExecConfig, Lot, OnLeverage, OnOversize, OnRuin, RunError, Value};

fn program(src: &str, name: &str) -> Program {
    let mut ws = corpus::base_workspace();
    ws.add_source(src).unwrap();
    let (p, diags) = check_program(&ws, name);
    let text: Vec<String> = diags.iter().map(|d| d.to_string()).collect();
    p.unwrap_or_else(|| panic!("{} should check:\n{}", name, text.join("\n")))
}

fn day(s: &str) -> i64 {
    parse_timestamp(s).unwrap()
}

/// One symbol X over five weekdays; `None` leaves the close out (the bar
/// still exists through universe and volume).
fn market(prices: &[Option<f64>]) -> Dataset {
    let mut ds = Dataset::new();
    let x = ds.intern("X");
    let days = ["2024-01-08", "2024-01-09", "2024-01-10", "2024-01-11", "2024-01-12"];
    for (d, p) in days.iter().zip(prices) {
        let t = day(d);
        if let Some(p) = p {
            ds.add("close", vec![Value::Equity(x), Value::Time(t), Value::Num(*p)]);
        }
        ds.add("volume", vec![Value::Equity(x), Value::Time(t), Value::Num(1000.0)]);
        ds.add("universe", vec![Value::Equity(x), Value::Time(t)]);
    }
    ds
}

fn cfg(cash: f64) -> ExecConfig {
    ExecConfig { initial_cash: cash, ..Default::default() }
}

fn risk_message(r: Result<absolute_backtest::kernel::RunResult, RunError>) -> String {
    match r {
        Err(RunError::Risk { message, .. }) => message,
        Err(e) => panic!("expected a risk halt, got {}", e),
        Ok(r) => panic!("expected a risk halt, the run finished with {} fills", r.fills.len()),
    }
}

// ---- lot rounding (#19) ----

const FRACTIONAL: &str = r#"
strategy fractional {
  env equities_1d
  uses features
  resolution @1d
  mode delta
  param q : Quantity<Shares> = 10.5 shares
  decide(T, buy(A, q)) :- universe(A, T), flat(A, T).
}
"#;

#[test]
fn orders_round_to_whole_shares_by_default_and_fractional_on_request() {
    let prog = program(FRACTIONAL, "fractional");
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
    let src = r#"
strategy tq {
  env equities_1d
  uses features
  resolution @1d
  mode target
  param q : Quantity<Shares> = 10.5 shares
  decide(T, target_quantity(A, q)) :- universe(A, T).
}
"#;
    let prog = program(src, "tq");
    let ds = market(&[Some(10.0), Some(10.0), Some(10.0), Some(10.0), Some(10.0)]);
    let r = run(&prog, &ds, cfg(1000.0)).unwrap();
    // One fill of 10, then the target 10.5 rounds to 10 again: no drift trade.
    assert_eq!(r.fills.iter().map(|f| f.quantity).collect::<Vec<_>>(), vec![10.0]);
}

// ---- oversize delta orders (#6, #33) ----

const OVERSELL: &str = r#"
strategy oversell {
  env equities_1d
  uses features
  resolution @1d
  mode delta
  param qty : Quantity<Shares> = 100 shares
  param big : Quantity<Shares> = 150 shares
  decide(T, buy(A, qty)) :- universe(A, T), flat(A, T), not bought_within(A, T, 30d).
  decide(T, sell(A, big)) :- held(A, T, _).
}
"#;

#[test]
fn a_sell_beyond_the_position_halts_by_default() {
    let prog = program(OVERSELL, "oversell");
    let ds = market(&[Some(10.0), Some(10.0), Some(10.0), Some(10.0), Some(10.0)]);
    let m = risk_message(run(&prog, &ds, cfg(10_000.0)));
    assert!(m.contains("150") && m.contains("100") && m.contains("sell"), "{}", m);
}

#[test]
fn a_sell_beyond_the_position_is_clamped_or_allowed_on_request() {
    let prog = program(OVERSELL, "oversell");
    let ds = market(&[Some(10.0), Some(10.0), Some(10.0), Some(10.0), Some(10.0)]);
    let r = run(&prog, &ds, ExecConfig { on_oversize: OnOversize::Clamp, ..cfg(10_000.0) }).unwrap();
    let qty: Vec<f64> = r.fills.iter().map(|f| f.quantity).collect();
    assert_eq!(qty[..2], [100.0, -100.0], "{:?}", qty);
    assert!(r.dropped.iter().any(|(_, _, why)| why.contains("clamped") && why.contains("50")), "{:?}", r.dropped);
    let r = run(&prog, &ds, ExecConfig { on_oversize: OnOversize::Allow, ..cfg(10_000.0) }).unwrap();
    assert_eq!(r.fills.iter().map(|f| f.quantity).collect::<Vec<_>>()[..2], [100.0, -150.0]);
    assert_eq!(r.final_positions.values().copied().next(), Some(-50.0));
}

// ---- leverage (#17, #33) ----

const LEVERED: &str = r#"
strategy levered {
  env equities_1d
  uses features
  resolution @1d
  mode target
  param w : Scalar = 2.4
  decide(T, target_weight(A, w)) :- universe(A, T).
}
"#;

#[test]
fn borrowing_to_buy_halts_by_default() {
    let prog = program(LEVERED, "levered");
    let ds = market(&[Some(10.0), Some(10.0), Some(10.0), Some(10.0), Some(10.0)]);
    let m = risk_message(run(&prog, &ds, cfg(1000.0)));
    assert!(m.contains("equity"), "{}", m);
}

#[test]
fn borrowing_is_rejected_or_allowed_on_request() {
    let prog = program(LEVERED, "levered");
    let ds = market(&[Some(10.0), Some(10.0), Some(10.0), Some(10.0), Some(10.0)]);
    let r = run(&prog, &ds, ExecConfig { on_leverage: OnLeverage::Reject, ..cfg(1000.0) }).unwrap();
    assert!(r.fills.is_empty(), "{:?}", r.fills);
    assert!(r.dropped.iter().all(|(_, _, why)| why.contains("equity")), "{:?}", r.dropped);
    assert_eq!(r.final_cash, 1000.0);
    let r = run(&prog, &ds, ExecConfig { on_leverage: OnLeverage::Allow, ..cfg(1000.0) }).unwrap();
    assert_eq!(r.fills[0].quantity, 240.0);
    assert_eq!(r.final_cash, 1000.0 - 2400.0);
}

#[test]
fn a_short_beyond_equity_halts_by_default() {
    let src = r#"
strategy naked {
  env equities_1d
  uses features
  resolution @1d
  mode delta
  param qty : Quantity<Shares> = 150 shares
  decide(T, short(A, qty)) :- universe(A, T), flat(A, T).
}
"#;
    let prog = program(src, "naked");
    let ds = market(&[Some(10.0), Some(10.0), Some(10.0), Some(10.0), Some(10.0)]);
    // Equity 1000 against 1500 of short exposure.
    let m = risk_message(run(&prog, &ds, cfg(1000.0)));
    assert!(m.contains("equity"), "{}", m);
    // 50 shares short is within equity.
    let prog = program(&src.replace("150 shares", "50 shares"), "naked");
    run(&prog, &ds, cfg(1000.0)).unwrap();
}

// ---- ruin (#15, #33) ----

const RUINED: &str = r#"
strategy ruined {
  env equities_1d
  uses features
  resolution @1d
  mode target
  param q : Quantity<Shares> = -100 shares
  param w : Scalar = 0.5
  param trigger : Price<USD> = 20 USD/share
  decide(T, target_quantity(A, q)) :- universe(A, T), flat(A, T).
  decide(T, target_weight(A, w)) :- position(A, T, Q), Q < 0 shares, close(A, T, P), P > trigger.
}
"#;

#[test]
fn a_ruined_book_halts_by_default_and_targets_flat_on_request() {
    let prog = program(RUINED, "ruined");
    // Short 100 at 10 (cash 2000), then the price triples: equity is -1000
    // when the re-weighting order comes to be filled.
    let ds = market(&[Some(10.0), Some(10.0), Some(30.0), Some(30.0), Some(30.0)]);
    let m = risk_message(run(&prog, &ds, cfg(1000.0)));
    assert!(m.contains("ruin") || m.contains("equity"), "{}", m);
    let r = run(&prog, &ds, ExecConfig { on_ruin: OnRuin::Continue, ..cfg(1000.0) }).unwrap();
    // A positive weight of a non-positive equity targets flat: the short is covered.
    let qty: Vec<f64> = r.fills.iter().map(|f| f.quantity).collect();
    assert_eq!(qty, vec![-100.0, 100.0], "{:?}", r.fills);
    assert!(r.final_positions.is_empty());
    assert_eq!(r.final_cash, 2000.0 - 3000.0);
}

// ---- liquidation at the last price (#16) ----

const ROUND_TRIP: &str = r#"
strategy round_trip {
  env equities_1d
  uses features
  resolution @1d
  mode delta
  param qty : Quantity<Shares> = 10 shares
  decide(T, buy(A, qty)) :- universe(A, T), flat(A, T).
  decide(T, sell(A, Q)) :- held(A, T, Q).
}
"#;

#[test]
fn a_liquidation_without_a_bar_price_fills_at_the_last_price() {
    let prog = program(ROUND_TRIP, "round_trip");
    // Priced on the first two days only; universe and volume rows continue.
    let ds = market(&[Some(10.0), Some(12.0), None, None, None]);
    let r = run(&prog, &ds, cfg(1000.0)).unwrap();
    // Buy decided day 1, filled day 2 at 12; sell decided day 2, filled day 3
    // at the last price (12) although day 3 has no close; the re-entry
    // decided day 3 is an opening order on an unpriced instrument and drops.
    let fills: Vec<(String, f64, f64, bool)> = r.fills.iter().map(|f| (format_timestamp(f.t), f.quantity, f.price, f.at_last_price)).collect();
    assert_eq!(fills, vec![("2024-01-09".to_string(), 10.0, 12.0, false), ("2024-01-10".to_string(), -10.0, 12.0, true)]);
    assert!(r.final_positions.is_empty());
    assert!(r.dropped.iter().any(|(t, d, why)| format_timestamp(*t) == "2024-01-10" && d.amount == 10.0 && why.contains("no price")), "{:?}", r.dropped);
}
