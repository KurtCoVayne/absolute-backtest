//! The executor's cost model (data-bundle doc, section 5: transaction-cost
//! neglect and slippage are modeled with conservative, non-zero defaults; a
//! configuration that turns a model off is warned, never silent).

mod corpus;

use absolute_backtest::check::{check_program, Program};
use absolute_backtest::data::{business_days, synthetic_daily};
use absolute_backtest::kernel::time::format_timestamp;
use absolute_backtest::kernel::{run, Dataset, ExecConfig, Value};

fn program(src: &str, name: &str) -> Program {
    let mut ws = corpus::base_workspace();
    ws.add_source(src).unwrap();
    let (p, diags) = check_program(&ws, name);
    let text: Vec<String> = diags.iter().map(|d| d.to_string()).collect();
    p.unwrap_or_else(|| panic!("{} should check:\n{}", name, text.join("\n")))
}

/// One symbol X over consecutive weekdays from 2024-01-08.
fn market(prices: &[f64]) -> Dataset {
    let mut ds = Dataset::new();
    let x = ds.intern("X");
    for (t, p) in business_days((2024, 1, 8), prices.len()).into_iter().zip(prices) {
        ds.add("close", vec![Value::Equity(x), Value::Time(t), Value::Num(*p)]);
        ds.add("volume", vec![Value::Equity(x), Value::Time(t), Value::Num(1000.0)]);
        ds.add("universe", vec![Value::Equity(x), Value::Time(t)]);
    }
    ds
}

/// Buy 10 on an up bar when flat, sell the bar after the buy.
const UP_DOWN: &str = r#"
strategy up_down {
  env equities_1d
  uses features
  resolution @1d
  mode delta
  param qty : Quantity<Shares> = 10 shares
  rel up(-A: Equity, @T: Timestamp)
  up(A, T) :- universe(A, T), logret(A, T, R), R > 0.
  decide(T, buy(A, qty)) :- up(A, T), flat(A, T).
  decide(T, sell(A, Q)) :- held(A, T, Q), prev(T, T0), decided(T0, buy(A, _)).
}
"#;

fn close(a: f64, b: f64) -> bool {
    (a - b).abs() < 1e-9
}

#[test]
fn frictionless_is_the_old_zero_cost_executor() {
    let cfg = ExecConfig::frictionless();
    assert_eq!(cfg.commission_per_share, 0.0);
    assert_eq!(cfg.commission_min_per_order, 0.0);
    assert_eq!(cfg.fee_bps_on_sells, 0.0);
    assert_eq!(cfg.slippage_bps, 0.0);
    assert_eq!(cfg.slippage_vol_mult, 0.0);
    let prog = program(UP_DOWN, "up_down");
    let ds = market(&[10.0, 11.0, 10.0, 12.0, 13.0]);
    let r = run(
        &prog,
        &ds,
        ExecConfig {
            initial_cash: 1000.0,
            ..ExecConfig::frictionless()
        },
    )
    .unwrap();
    let fills: Vec<(String, f64, f64)> = r.fills.iter().map(|f| (format_timestamp(f.t), f.quantity, f.price)).collect();
    assert_eq!(
        fills,
        vec![("2024-01-10".to_string(), 10.0, 10.0), ("2024-01-11".to_string(), -10.0, 12.0), ("2024-01-12".to_string(), 10.0, 13.0)]
    );
    assert_eq!(r.final_cash, 890.0);
    assert!(r.fills.iter().all(|f| f.commission == 0.0 && f.fee == 0.0 && f.slippage == 0.0));
    assert_eq!(r.costs.commissions, 0.0);
    assert_eq!(r.costs.turnover, 100.0 + 120.0 + 130.0);
}

#[test]
fn the_defaults_are_non_zero_and_conservative() {
    let cfg = ExecConfig::default();
    assert_eq!(cfg.commission_per_share, 0.005);
    assert_eq!(cfg.commission_min_per_order, 1.0);
    assert_eq!(cfg.fee_bps_on_sells, 0.278);
    assert_eq!(cfg.slippage_bps, 0.0);
    assert_eq!(cfg.slippage_vol_mult, 0.1);
    assert_eq!(cfg.vol_window, 20);
    assert_eq!(cfg.vol_min_obs, 10);
}

#[test]
fn the_per_order_minimum_binds_on_a_small_order() {
    let prog = program(UP_DOWN, "up_down");
    let ds = market(&[10.0, 11.0, 10.0, 12.0, 13.0]);
    let cfg = ExecConfig {
        initial_cash: 1000.0,
        commission_per_share: 0.005,
        commission_min_per_order: 1.0,
        ..ExecConfig::frictionless()
    };
    let r = run(&prog, &ds, cfg).unwrap();
    // 10 shares at 0.005 is 0.05, below the 1.00 minimum: the minimum binds.
    assert!(close(r.fills[0].commission, 1.0), "{}", r.fills[0].commission);
    assert!(close(r.fills[0].fee, 0.0));
    // Buy 10 at 10 (+1), sell 10 at 12 (+1), buy 10 at 13 (+1): 1000 - 101 + 119 - 131 = 887.
    assert!(close(r.final_cash, 887.0), "{}", r.final_cash);
    assert!(close(r.costs.commissions, 3.0));
    // 300 shares at 0.005 = 1.50 is above the minimum.
    let big = program(&UP_DOWN.replace("10 shares", "300 shares"), "up_down");
    let r = run(
        &big,
        &ds,
        ExecConfig {
            initial_cash: 100_000.0,
            commission_per_share: 0.005,
            commission_min_per_order: 1.0,
            ..ExecConfig::frictionless()
        },
    )
    .unwrap();
    assert!(close(r.fills[0].commission, 1.5), "{}", r.fills[0].commission);
}

#[test]
fn the_regulatory_fee_is_charged_on_sells_only() {
    let prog = program(UP_DOWN, "up_down");
    let ds = market(&[10.0, 11.0, 10.0, 12.0, 13.0]);
    let cfg = ExecConfig {
        initial_cash: 1000.0,
        fee_bps_on_sells: 0.278,
        ..ExecConfig::frictionless()
    };
    let r = run(&prog, &ds, cfg).unwrap();
    assert!(close(r.fills[0].fee, 0.0), "a buy pays no fee");
    // Sell 10 at 12: notional 120, fee 120 * 0.278 / 10_000.
    let fee = 120.0 * 0.278 / 10_000.0;
    assert!(close(r.fills[1].fee, fee), "{}", r.fills[1].fee);
    assert!(close(r.fills[1].price, 12.0), "the fee is a cash charge, not a price change");
    assert!(close(r.final_cash, 890.0 - fee), "{}", r.final_cash);
    assert!(close(r.costs.fees, fee));
}

/// Sample standard deviation of log returns over the last `window` bars
/// ending at `end` inclusive, as the executor defines realized volatility.
fn vol(prices: &[f64], end: usize, window: usize) -> f64 {
    let lo = end.saturating_sub(window);
    let rets: Vec<f64> = (lo + 1..=end).map(|i| (prices[i] / prices[i - 1]).ln()).collect();
    let n = rets.len() as f64;
    let mean = rets.iter().sum::<f64>() / n;
    (rets.iter().map(|r| (r - mean).powi(2)).sum::<f64>() / (n - 1.0)).sqrt()
}

#[test]
fn slippage_scales_with_realized_volatility() {
    let ds = synthetic_daily(&["X"], (2024, 1, 1), 40, 5);
    let prices: Vec<f64> = ds.facts["close"].iter().map(|tu| tu[2].as_f64().unwrap()).collect();
    let bars: Vec<i64> = ds.facts["close"]
        .iter()
        .map(|tu| match tu[1] {
            Value::Time(t) => t,
            _ => unreachable!(),
        })
        .collect();
    let prog = program(UP_DOWN, "up_down");
    let cfg = ExecConfig {
        initial_cash: 100_000.0,
        slippage_vol_mult: 0.1,
        vol_window: 20,
        vol_min_obs: 10,
        ..ExecConfig::frictionless()
    };
    let r = run(&prog, &ds, cfg).unwrap();
    let mut checked_fixed = 0;
    let mut checked_scaled = 0;
    for f in &r.fills {
        let k = bars.iter().position(|&b| b == f.t).unwrap();
        let p = prices[k];
        let sign = if f.quantity > 0.0 { 1.0 } else { -1.0 };
        if k < 10 {
            // Fewer than vol_min_obs returns: the fixed part only, which is zero here.
            assert!(close(f.price, p), "bar {} fill {} vs close {}", k, f.price, p);
            assert!(close(f.slippage, 0.0));
            checked_fixed += 1;
        } else {
            let expected = p * (1.0 + sign * 0.1 * vol(&prices, k, 20));
            assert!(close(f.price, expected), "bar {}: fill {} vs expected {}", k, f.price, expected);
            assert!(close(f.slippage, f.quantity.abs() * (f.price - p).abs()), "{}", f.slippage);
            checked_scaled += 1;
        }
    }
    assert!(checked_fixed > 0 && checked_scaled > 0, "{} fixed, {} scaled", checked_fixed, checked_scaled);
    assert!(close(r.costs.slippage, r.fills.iter().map(|f| f.slippage).sum::<f64>()));
    // The fixed part adds to the scaled part.
    let cfg2 = ExecConfig {
        slippage_bps: 50.0,
        initial_cash: 100_000.0,
        slippage_vol_mult: 0.1,
        ..ExecConfig::frictionless()
    };
    let r2 = run(&prog, &ds, cfg2).unwrap();
    let f = r2.fills.iter().find(|f| bars.iter().position(|&b| b == f.t).unwrap() >= 10).unwrap();
    let k = bars.iter().position(|&b| b == f.t).unwrap();
    let sign = if f.quantity > 0.0 { 1.0 } else { -1.0 };
    assert!(close(f.price, prices[k] * (1.0 + sign * (0.005 + 0.1 * vol(&prices, k, 20)))), "{}", f.price);
}

#[test]
fn target_weights_size_at_the_expected_fill_price() {
    let src = r#"
strategy full {
  env equities_1d
  uses features
  resolution @1d
  mode target
  param w : Scalar = 1.0
  decide(T, target_weight(A, w)) :- universe(A, T), flat(A, T).
}
"#;
    let prog = program(src, "full");
    let ds = market(&[10.0, 10.0, 10.0, 10.0, 10.0]);
    // 100 bps slippage: a fully invested book must fit after slippage, and
    // the commission it pays is never leverage.
    let cfg = ExecConfig {
        initial_cash: 1000.0,
        slippage_bps: 100.0,
        impact_coef: 0.0,
        margin_rate: 0.0,
        ..ExecConfig::default()
    };
    let r = run(&prog, &ds, cfg).unwrap();
    assert_eq!(r.fills.len(), 1, "{:?}", r.dropped);
    // 1000 / 10.1 = 99.0099 -> 99 shares at 10.10 = 999.90, plus the 1.00 minimum commission.
    assert_eq!(r.fills[0].quantity, 99.0);
    assert!(close(r.fills[0].price, 10.1));
    assert!(close(r.final_cash, 1000.0 - 999.9 - 1.0), "{}", r.final_cash);
}

#[test]
fn a_frictionless_run_is_warned_and_a_default_run_is_not() {
    let prog = program(UP_DOWN, "up_down");
    let ds = market(&[10.0, 11.0, 10.0, 12.0, 13.0]);
    let r = run(
        &prog,
        &ds,
        ExecConfig {
            initial_cash: 1000.0,
            ..ExecConfig::frictionless()
        },
    )
    .unwrap();
    let biases: Vec<&str> = r.warnings.iter().map(|w| w.bias.as_str()).collect();
    assert!(biases.contains(&"transaction-cost neglect"), "{:?}", r.warnings);
    assert!(biases.contains(&"slippage"), "{:?}", r.warnings);
    assert!(r.warnings.iter().all(|w| !w.message.is_empty()));
    let r = run(
        &prog,
        &ds,
        ExecConfig {
            initial_cash: 1000.0,
            ..ExecConfig::default()
        },
    )
    .unwrap();
    // The v1 cash rate is zero and warned (cash management); no cost model is off.
    assert!(r.warnings.iter().all(|w| w.bias == "cash-management"), "{:?}", r.warnings);
    // Commission off but the minimum on is still a cost model; slippage off entirely warns once.
    let r = run(
        &prog,
        &ds,
        ExecConfig {
            initial_cash: 1000.0,
            commission_per_share: 0.0,
            slippage_vol_mult: 0.0,
            ..ExecConfig::default()
        },
    )
    .unwrap();
    let biases: Vec<&str> = r.warnings.iter().map(|w| w.bias.as_str()).filter(|b| *b != "cash-management").collect();
    assert_eq!(biases, vec!["slippage"], "{:?}", r.warnings);
}
