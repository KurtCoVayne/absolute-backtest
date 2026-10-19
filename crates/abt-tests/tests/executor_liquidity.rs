//! The executor's liquidity model (data-bundle doc, section 5: market impact,
//! liquidity and partial fills): a per-bar participation cap from bar volume,
//! square-root impact in participation of average daily volume, a delta
//! order's remainder expiring and a target re-issuing itself.

use abt_tests as common;

use abt_kernel::data::business_days;
use abt_kernel::kernel::time::format_timestamp;
use abt_kernel::kernel::{Dataset, ExecConfig};
use abt_tests::run;

/// One symbol X over consecutive weekdays from 2024-01-08, with a volume per bar.
fn market(prices: &[f64], volumes: &[f64]) -> Dataset {
    let mut ds = Dataset::new();
    let bars = business_days((2024, 1, 8), prices.len());
    let x = ds.add_security("X", "X", bars[0], None);
    for ((t, p), v) in bars.iter().zip(prices).zip(volumes) {
        common::add_bar(&mut ds, x, *t, *p, *v);
    }
    ds.derive_tickers();
    ds
}

/// Buy 10 on an up bar when flat, sell the bar after the buy.
fn up_down() -> String {
    common::strategy(
        "up_down",
        "    qty : Quantity<Shares> = 10 shares",
        "    rel up(A: Equity, @T)\n    up(A, T) :- universe(A, T), log_price_return(A, T, R), R > 0.",
        "    decide(T, trade(A, qty)) :- up(A, T), flat(A, T).\n    decide(T, trade(A, Q)) :- held(A, T, Q0), prev(T, T0), decided(T0, trade(A, Q1)), Q1 > 0 shares, Q = -Q0.",
    )
}

/// Target the full book once, when flat; silent afterwards.
fn full_once() -> String {
    common::strategy("full_once", "    w : Scalar = 1.0", "", "    decide(T, hold(A, w)) :- universe(A, T), flat(A, T).")
}

fn close(a: f64, b: f64) -> bool {
    (a - b).abs() < 1e-9
}

#[test]
fn the_defaults_and_frictionless() {
    let cfg = ExecConfig::default();
    assert_eq!(cfg.participation_cap, 0.1);
    assert_eq!(cfg.impact_coef, 0.1);
    assert_eq!(cfg.adv_window, 20);
    assert_eq!(cfg.participation_warn, 0.05);
    let f = ExecConfig::frictionless();
    assert_eq!(f.participation_cap, 0.0, "0 is no cap");
    assert_eq!(f.impact_coef, 0.0);
}

#[test]
fn a_delta_order_fills_up_to_the_participation_cap_and_the_remainder_expires() {
    let prog = common::program(&up_down(), "up_down");
    // Volume 40 a bar: the cap is 4 shares of the 10 asked for.
    let ds = market(&[10.0, 11.0, 10.0, 12.0, 13.0], &[40.0; 5]);
    let cfg = ExecConfig {
        initial_cash: 1000.0,
        participation_cap: 0.1,
        ..ExecConfig::frictionless()
    };
    let r = run(&prog, &ds, cfg).unwrap();
    let fills: Vec<(String, f64)> = r.fills.iter().map(|f| (format_timestamp(f.t), f.quantity)).collect();
    // Buy 10 -> 4 filled; the sell is of what is held (4), within the cap.
    assert_eq!(fills, vec![("2024-01-10".to_string(), 4.0), ("2024-01-11".to_string(), -4.0), ("2024-01-12".to_string(), 4.0)]);
    assert!(r.fills.iter().all(|f| close(f.participation, 0.1)), "{:?}", r.fills);
    assert!(r.fills[0].partial && !r.fills[1].partial && r.fills[2].partial);
    let partials: Vec<&String> = r.dropped.iter().map(|(_, _, reason)| reason).filter(|r| r.contains("partial fill")).collect();
    assert_eq!(partials.len(), 2, "{:?}", r.dropped);
    assert_eq!(r.dropped.len(), 3, "plus the last bar's sell: {:?}", r.dropped);
    assert!(partials[0].contains("partial fill: 4 of 10 shares") && partials[0].contains("participation cap"), "{}", partials[0]);
    // Requested 10 + 4 + 10 = 24 shares, filled 12.
    assert!(close(r.liquidity.fill_ratio, 0.5), "{}", r.liquidity.fill_ratio);
    assert!(close(r.liquidity.avg_participation, 0.1) && close(r.liquidity.max_participation, 0.1));
    // Participation at the cap is above the 5% warning threshold.
    assert!(r.warnings.iter().any(|w| w.bias == "market-impact" && w.message.contains("3 fills")), "{:?}", r.warnings);
}

#[test]
fn a_target_re_issues_itself_until_reached() {
    let prog = common::program(&full_once(), "full_once");
    // 1000 cash at 10 is 100 shares; volume 300 caps a bar at 30.
    let ds = market(&[10.0; 8], &[300.0; 8]);
    let cfg = ExecConfig {
        initial_cash: 1000.0,
        participation_cap: 0.1,
        ..ExecConfig::frictionless()
    };
    let r = run(&prog, &ds, cfg).unwrap();
    let fills: Vec<(String, f64)> = r.fills.iter().map(|f| (format_timestamp(f.t), f.quantity)).collect();
    assert_eq!(
        fills,
        vec![
            ("2024-01-09".to_string(), 30.0),
            ("2024-01-10".to_string(), 30.0),
            ("2024-01-11".to_string(), 30.0),
            ("2024-01-12".to_string(), 10.0)
        ]
    );
    // The strategy decided once; the executor carried the rest.
    assert_eq!(r.decisions.len(), 1);
    assert!(r.dropped.is_empty(), "a re-issued target is not a drop: {:?}", r.dropped);
    assert_eq!(r.final_positions.values().copied().collect::<Vec<_>>(), vec![100.0]);
    assert!(close(r.final_cash, 0.0));
    assert!(close(r.liquidity.fill_ratio, 1.0), "every requested share was eventually filled: {}", r.liquidity.fill_ratio);
}

#[test]
fn a_new_decision_supersedes_an_open_target() {
    // Target the full book, then flat the next bar: the open remainder of the
    // first target is dropped in favour of the liquidation.
    let src = common::strategy(
        "flip",
        "    w : Scalar = 1.0",
        "    rel opening(@T)\n    opening(T) :- bar(T), not has_prev(T).\n    rel has_prev(@T)\n    has_prev(T) :- bar(T), prev(T, _).",
        "    decide(T, hold(A, w)) :- universe(A, T), opening(T).\n    decide(T, hold(A, 0)) :- held(A, T, _), has_prev(T).",
    );
    let prog = common::program(&src, "flip");
    let ds = market(&[10.0; 6], &[300.0; 6]);
    let cfg = ExecConfig {
        initial_cash: 1000.0,
        participation_cap: 0.1,
        ..ExecConfig::frictionless()
    };
    let r = run(&prog, &ds, cfg).unwrap();
    let fills: Vec<(String, f64)> = r.fills.iter().map(|f| (format_timestamp(f.t), f.quantity)).collect();
    // Bar 1 buys 30 of 100; bar 2's flat target sells 30 and the open 70 is gone.
    assert_eq!(fills, vec![("2024-01-09".to_string(), 30.0), ("2024-01-10".to_string(), -30.0)]);
    assert!(r.final_positions.is_empty());
}

#[test]
fn impact_is_square_root_in_participation_of_adv() {
    let prog = common::program(&up_down(), "up_down");
    // ADV 10_000 and 100 shares: participation 1%, impact 0.1 * sqrt(0.01) = 1%.
    let big = common::program(&up_down().replace("10 shares", "100 shares"), "up_down");
    let ds = market(&[10.0, 11.0, 10.0, 12.0, 13.0], &[10_000.0; 5]);
    let cfg = ExecConfig {
        initial_cash: 100_000.0,
        impact_coef: 0.1,
        adv_window: 20,
        ..ExecConfig::frictionless()
    };
    let r = run(&big, &ds, cfg.clone()).unwrap();
    assert!(close(r.fills[0].price, 10.0 * 1.01), "buy at 10 moved up 1%: {}", r.fills[0].price);
    assert!(close(r.fills[0].impact, 100.0 * 10.0 * 0.01), "{}", r.fills[0].impact);
    assert!(close(r.fills[1].price, 12.0 * 0.99), "sell at 12 moved down 1%: {}", r.fills[1].price);
    assert!(close(r.costs.impact, r.fills.iter().map(|f| f.impact).sum::<f64>()));
    // Impact and slippage add: 50 bps fixed slippage on top.
    let r2 = run(&big, &ds, ExecConfig { slippage_bps: 50.0, ..cfg.clone() }).unwrap();
    assert!(close(r2.fills[0].price, 10.0 * (1.0 + 0.005 + 0.01)), "{}", r2.fills[0].price);
    assert!(close(r2.fills[0].slippage, 100.0 * 10.0 * 0.005) && close(r2.fills[0].impact, 100.0 * 10.0 * 0.01));
    // 10 shares: participation 0.1%, impact 0.1 * sqrt(0.001).
    let r3 = run(&prog, &ds, cfg).unwrap();
    assert!(close(r3.fills[0].price, 10.0 * (1.0 + 0.1 * (0.001f64).sqrt())), "{}", r3.fills[0].price);
}

#[test]
fn turning_liquidity_models_off_is_warned() {
    let prog = common::program(&up_down(), "up_down");
    let ds = market(&[10.0, 11.0, 10.0, 12.0, 13.0], &[40.0; 5]);
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
    assert!(biases.contains(&"liquidity"), "{:?}", r.warnings);
    assert!(biases.contains(&"market-impact"), "{:?}", r.warnings);
    // Without a cap the whole order fills.
    assert_eq!(r.fills[0].quantity, 10.0);
    assert!(close(r.fills[0].participation, 0.25));
    // A volume relation that is not in the program is a configuration error.
    let err = run(
        &prog,
        &ds,
        ExecConfig {
            volume_relation: Some("turnover".into()),
            ..ExecConfig::default()
        },
    )
    .err()
    .unwrap();
    assert!(err.to_string().contains("turnover"), "{}", err);
}
