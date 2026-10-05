//! The executor's margin, funding and borrow models (data-bundle doc,
//! section 5: leverage, funding-cost neglect, shorting, borrow availability,
//! cash management). The default stays at 1x gross, halting, by the owner's
//! ruling; Reg T (2x initial, 25% maintenance, reject and log) is a preset.

mod corpus;

use absolute_backtest::check::{check_program, Program};
use absolute_backtest::data::business_days;
use absolute_backtest::kernel::time::format_timestamp;
use absolute_backtest::kernel::{run, BorrowBucket, Dataset, ExecConfig, OnLeverage, OnMarginCall, RunError, Value};

fn program(src: &str, name: &str) -> Program {
    let mut ws = corpus::base_workspace();
    ws.add_source(src).unwrap();
    let (p, diags) = check_program(&ws, name);
    let text: Vec<String> = diags.iter().map(|d| d.to_string()).collect();
    p.unwrap_or_else(|| panic!("{} should check:\n{}", name, text.join("\n")))
}

/// One symbol X over consecutive weekdays from 2024-01-08 (Monday).
fn market(prices: &[f64], volume: f64) -> Dataset {
    let mut ds = Dataset::new();
    let x = ds.intern("X");
    for (t, p) in business_days((2024, 1, 8), prices.len()).into_iter().zip(prices) {
        ds.add("close", vec![Value::Equity(x), Value::Time(t), Value::Num(*p)]);
        ds.add("volume", vec![Value::Equity(x), Value::Time(t), Value::Num(volume)]);
        ds.add("universe", vec![Value::Equity(x), Value::Time(t)]);
    }
    ds
}

/// Frictionless, no interest, every name shortable for free: the margin
/// arithmetic alone.
fn bare(cash: f64) -> ExecConfig {
    ExecConfig {
        initial_cash: cash,
        cash_rate: 0.0,
        margin_rate: 0.0,
        short_rebate: 0.0,
        borrow: vec![BorrowBucket { adv_below: f64::INFINITY, fee_bps: 0.0, shortable: true }],
        ..ExecConfig::frictionless()
    }
}

fn levered(w: &str) -> String {
    format!(
        r#"
strategy levered {{
  env equities_1d
  uses features
  resolution @1d
  mode target
  param w : Scalar = {w}
  rel opening(@T: Timestamp)
  opening(T) :- bar(T), not has_prev(T).
  rel has_prev(@T: Timestamp)
  has_prev(T) :- bar(T), prev(T, _).
  decide(T, target_weight(A, w)) :- universe(A, T), opening(T).
}}
"#
    )
}

const HOLD_CASH: &str = r#"
strategy hold_cash {
  env equities_1d
  uses features
  resolution @1d
  mode delta
  param qty : Quantity<Shares> = 10 shares
  rel never(@T: Timestamp)
  never(T) :- bar(T), prev(T, T0), T0 > T.
  decide(T, buy(A, qty)) :- universe(A, T), never(T).
}
"#;

const SHORT_ONCE: &str = r#"
strategy short_once {
  env equities_1d
  uses features
  resolution @1d
  mode delta
  param qty : Quantity<Shares> = 50 shares
  rel opening(@T: Timestamp)
  opening(T) :- bar(T), not has_prev(T).
  rel has_prev(@T: Timestamp)
  has_prev(T) :- bar(T), prev(T, _).
  decide(T, short(A, qty)) :- universe(A, T), opening(T).
}
"#;

fn close(a: f64, b: f64) -> bool {
    (a - b).abs() < 1e-9
}

#[test]
fn the_defaults_and_the_reg_t_preset() {
    let cfg = ExecConfig::default();
    assert_eq!(cfg.max_gross, 1.0, "no borrowing by default");
    assert_eq!(cfg.maintenance_margin, 0.25);
    assert_eq!(cfg.on_margin_call, OnMarginCall::Halt);
    assert_eq!(cfg.cash_rate, 0.0);
    assert_eq!(cfg.margin_rate, 0.05);
    assert_eq!(cfg.short_rebate, 0.0);
    assert_eq!(cfg.borrow.len(), 3);
    assert!(!cfg.borrow[0].shortable, "the smallest ADV bucket is not shortable");
    assert!(cfg.borrow[1].shortable && cfg.borrow[2].shortable && cfg.borrow[1].fee_bps > cfg.borrow[2].fee_bps);
    assert_eq!(cfg.borrow[2].adv_below, f64::INFINITY);
    let reg_t = ExecConfig::reg_t();
    assert_eq!(reg_t.max_gross, 2.0);
    assert_eq!(reg_t.maintenance_margin, 0.25);
    assert_eq!(reg_t.on_leverage, OnLeverage::Reject, "orders beyond buying power are rejected and logged");
    assert_eq!(reg_t.commission_per_share, cfg.commission_per_share, "the preset changes margin only");
}

#[test]
fn reg_t_admits_one_and_a_half_times_and_rejects_beyond_two() {
    let ds = market(&[10.0; 5], 1e6);
    let cfg = ExecConfig { max_gross: 2.0, on_leverage: OnLeverage::Reject, ..bare(1000.0) };
    let r = run(&program(&levered("1.5"), "levered"), &ds, cfg.clone()).unwrap();
    assert_eq!(r.fills.iter().map(|f| f.quantity).collect::<Vec<_>>(), vec![150.0]);
    assert!(close(r.final_cash, -500.0));
    let r = run(&program(&levered("2.4"), "levered"), &ds, cfg).unwrap();
    assert!(r.fills.is_empty(), "{:?}", r.fills);
    assert!(r.dropped.iter().any(|(_, _, why)| why.contains("rejected") && why.contains("2")), "{:?}", r.dropped);
    // The default admits neither.
    let err = run(&program(&levered("1.5"), "levered"), &ds, bare(1000.0)).err().unwrap();
    assert!(matches!(err, RunError::Risk { .. }), "{}", err);
}

#[test]
fn a_maintenance_call_halts_liquidates_or_is_allowed() {
    // 2x long at 10 (200 shares, cash -1000); the price falls to 6: equity
    // 200 against gross 1200 is below 25% maintenance.
    let ds = market(&[10.0, 10.0, 6.0, 6.0, 6.0], 1e6);
    let cfg = ExecConfig { max_gross: 2.0, ..bare(1000.0) };
    let err = run(&program(&levered("2.0"), "levered"), &ds, cfg.clone()).err().unwrap();
    match &err {
        RunError::Risk { message, .. } => assert!(message.contains("margin call") && message.contains("25%"), "{}", message),
        e => panic!("expected a margin call halt, got {}", e),
    }
    let r = run(&program(&levered("2.0"), "levered"), &ds, ExecConfig { on_margin_call: OnMarginCall::Liquidate, ..cfg.clone() }).unwrap();
    // Sell the fraction that restores maintenance: 200 >= 0.25 * (200 - n) * 6
    // needs n >= 66.7, so 67 shares at 6; equity stays 200, gross 798.
    let fills: Vec<(String, f64, f64, bool)> = r.fills.iter().map(|f| (format_timestamp(f.t), f.quantity, f.price, f.forced)).collect();
    assert_eq!(fills, vec![("2024-01-09".to_string(), 200.0, 10.0, false), ("2024-01-10".to_string(), -67.0, 6.0, true)]);
    assert_eq!(r.final_positions.values().copied().collect::<Vec<_>>(), vec![133.0]);
    assert!(close(r.final_cash, -1000.0 + 67.0 * 6.0));
    assert!(r.warnings.iter().any(|w| w.bias == "leverage" && w.message.contains("margin call")), "{:?}", r.warnings);
    let r = run(&program(&levered("2.0"), "levered"), &ds, ExecConfig { on_margin_call: OnMarginCall::Allow, ..cfg }).unwrap();
    assert_eq!(r.fills.len(), 1);
    assert!(r.warnings.iter().any(|w| w.bias == "leverage" && w.message.contains("3 margin calls")), "{:?}", r.warnings);
}

#[test]
fn cash_earns_the_cash_rate_and_a_debit_pays_the_margin_rate() {
    // Five weekdays: four one-day accrual periods at 3.65% a year is a basis
    // point a day.
    let ds = market(&[10.0; 5], 1e6);
    let r = run(&program(HOLD_CASH, "hold_cash"), &ds, ExecConfig { cash_rate: 0.0365, ..bare(1000.0) }).unwrap();
    assert!(r.fills.is_empty());
    let expected = 1000.0 * 1.0001f64.powi(4);
    assert!(close(r.final_cash, expected), "{} vs {}", r.final_cash, expected);
    assert!(close(r.funding.cash_interest, expected - 1000.0));
    // A weekend is three days of accrual.
    let ds6 = market(&[10.0; 6], 1e6);
    let r = run(&program(HOLD_CASH, "hold_cash"), &ds6, ExecConfig { cash_rate: 0.0365, ..bare(1000.0) }).unwrap();
    assert!(close(r.final_cash, 1000.0 * 1.0001f64.powi(4) * 1.0003), "{}", r.final_cash);
    // Borrowed cash pays the margin rate: 2x long leaves -1000 of cash.
    let cfg = ExecConfig { max_gross: 2.0, margin_rate: 0.0365, ..bare(1000.0) };
    let r = run(&program(&levered("2.0"), "levered"), &ds, cfg).unwrap();
    // Filled on day 2; three accrual periods on -1000.
    assert!(close(r.final_cash, -1000.0 * 1.0001f64.powi(3)), "{}", r.final_cash);
    assert!(close(r.funding.margin_interest, 1000.0 * (1.0001f64.powi(3) - 1.0)), "{}", r.funding.margin_interest);
    assert!(r.warnings.iter().any(|w| w.bias == "funding-cost neglect"), "constant rates are a proxy: {:?}", r.warnings);
}

#[test]
fn a_short_pays_the_borrow_fee_of_its_adv_bucket_or_is_not_shortable() {
    let ds = market(&[10.0; 5], 1e6);
    // One bucket, 365 bps a year: 500 of short notional pays 0.05 a day.
    let cfg = ExecConfig {
        borrow: vec![BorrowBucket { adv_below: f64::INFINITY, fee_bps: 365.0, shortable: true }],
        ..bare(1000.0)
    };
    let r = run(&program(SHORT_ONCE, "short_once"), &ds, cfg).unwrap();
    assert_eq!(r.fills.iter().map(|f| f.quantity).collect::<Vec<_>>(), vec![-50.0]);
    // Shorted on day 2; three days of fee.
    assert!(close(r.funding.borrow_fees, 3.0 * 0.05), "{}", r.funding.borrow_fees);
    assert!(close(r.final_cash, 1500.0 - 0.15), "{}", r.final_cash);
    assert!(r.warnings.iter().any(|w| w.bias == "shorting"), "fees are modeled, not observed: {:?}", r.warnings);
    // The short rebate is credited on the same notional.
    let r = run(
        &program(SHORT_ONCE, "short_once"),
        &ds,
        ExecConfig { short_rebate: 0.0365, borrow: vec![BorrowBucket { adv_below: f64::INFINITY, fee_bps: 0.0, shortable: true }], ..bare(1000.0) },
    )
    .unwrap();
    assert!(close(r.funding.short_rebate, 3.0 * 0.05), "{}", r.funding.short_rebate);
    // Under the default buckets a 1000-share ADV is not shortable.
    let thin = market(&[10.0; 5], 1000.0);
    let r = run(&program(SHORT_ONCE, "short_once"), &thin, ExecConfig { initial_cash: 1000.0, ..ExecConfig::default() }).unwrap();
    assert!(r.fills.is_empty(), "{:?}", r.fills);
    assert!(r.dropped.iter().any(|(_, _, why)| why.contains("not shortable") && why.contains("ADV")), "{:?}", r.dropped);
    assert!(r.warnings.iter().any(|w| w.bias == "borrow-availability"), "{:?}", r.warnings);
}

#[test]
fn exposure_is_recorded_at_every_bar() {
    let ds = market(&[10.0, 10.0, 12.0, 12.0, 12.0], 1e6);
    let r = run(&program(&levered("0.5"), "levered"), &ds, bare(1000.0)).unwrap();
    assert_eq!(r.exposure.len(), 5);
    let day3 = &r.exposure[2];
    assert_eq!(format_timestamp(day3.t), "2024-01-10");
    // 50 shares bought at 10 on day 2, marked at 12 on day 3.
    assert!(close(day3.cash, 500.0) && close(day3.gross, 600.0) && close(day3.net, 600.0) && close(day3.equity, 1100.0));
    assert!(close(day3.leverage, 600.0 / 1100.0), "{}", day3.leverage);
    assert!(close(r.exposure[0].gross, 0.0) && close(r.exposure[0].leverage, 0.0));
    assert!(r.exposure.iter().zip(&r.equity_curve).all(|(e, (t, eq))| e.t == *t && close(e.equity, *eq)));
}

#[test]
fn a_zero_cash_rate_is_warned_as_cash_management() {
    let ds = market(&[10.0; 5], 1e6);
    let r = run(&program(HOLD_CASH, "hold_cash"), &ds, ExecConfig { initial_cash: 1000.0, ..ExecConfig::default() }).unwrap();
    assert!(r.warnings.iter().any(|w| w.bias == "cash-management"), "{:?}", r.warnings);
    let r = run(&program(HOLD_CASH, "hold_cash"), &ds, ExecConfig { initial_cash: 1000.0, cash_rate: 0.02, ..ExecConfig::default() }).unwrap();
    assert!(r.warnings.iter().all(|w| w.bias != "cash-management"), "{:?}", r.warnings);
}
