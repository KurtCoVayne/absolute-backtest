//! K-9.3's limits cases (G8, K-3.5), as the table states them: a per-name
//! weight limit of 0.1 against a `hold(A, 0.2)` under each policy, and a
//! drawdown limit of 0.1 after a 12 % fall, which rejects the orders that
//! increase the book and lets those that reduce it through.

#![allow(clippy::result_large_err)]

use abt_kernel::contract::Status;
use abt_kernel::kernel::{EventKind, ExecConfig, RunResult};
use abt_tests as common;
use abt_tests::{run, StrategyParts};

/// Five names held at 0.2 each, every bar, under `limits`.
fn held_at_a_fifth(limits: &str) -> RunResult {
    let src = StrategyParts {
        name: "lim".into(),
        params: "    w : Scalar = 0.2".into(),
        decides: "    decide(T, hold(A, w)) :- universe(A, T).".into(),
        execution: format!("equity_default with {{ leverage unconstrained   limits {{ {} }} }}", limits),
        ..Default::default()
    }
    .render();
    let p = common::program(&src, "lim");
    let ds = abt_kernel::data::synthetic_daily(&["AAA", "BBB", "CCC", "DDD", "EEE"], (2022, 1, 3), 60, 5);
    run(&p, &ds, ExecConfig::from_program(&p)).unwrap()
}

/// Each name's weight at the first bar the book holds anything: its
/// marked value over the capital.
fn first_weights(r: &RunResult) -> Vec<f64> {
    let first = r.exposure.iter().find(|x| x.gross > 0.0).expect("the book holds something");
    r.holdings.iter().filter(|h| h.t == first.t).map(|h| h.position * h.mark.unwrap() / first.capital).collect()
}

fn triggered(r: &RunResult) -> Vec<(String, String)> {
    r.events
        .iter()
        .filter_map(|e| match &e.kind {
            EventKind::LimitTriggered { limit, policy, .. } => Some((limit.clone(), policy.clone())),
            _ => None,
        })
        .collect()
}

#[test]
fn a_weight_limit_of_a_tenth_against_a_hold_of_a_fifth_rejects_clamps_or_warns() {
    // `reject`: every order is rejected with the limit, nothing fills.
    let r = held_at_a_fifth("weight <= 0.1 per name reject");
    assert!(r.fills.is_empty(), "{:?}", r.fills.first());
    let rejected: Vec<&String> = r
        .events
        .iter()
        .filter_map(|e| match &e.kind {
            EventKind::Rejected { reason, .. } => Some(reason),
            _ => None,
        })
        .collect();
    assert_eq!(rejected.len(), r.decisions.len());
    assert!(rejected.iter().all(|w| w.contains("limit `weight <= 0.1 per name reject`")), "{:?}", rejected.first());
    // `clamp`: each name is filled to a tenth.
    let r = held_at_a_fifth("weight <= 0.1 per name clamp");
    let w = first_weights(&r);
    assert_eq!(w.len(), 5);
    assert!(w.iter().all(|w| (w - 0.1).abs() < 0.005), "{:?}", w);
    assert!(triggered(&r).iter().any(|(l, p)| l == "weight <= 0.1 per name clamp" && p == "clamp"));
    // `warn`: each name is filled to a fifth, and the limit warns.
    let r = held_at_a_fifth("weight <= 0.1 per name warn");
    let w = first_weights(&r);
    assert!(w.iter().all(|w| (w - 0.2).abs() < 0.01), "{:?}", w);
    assert!(triggered(&r).iter().any(|(l, p)| l == "weight <= 0.1 per name warn" && p == "warn"));
    assert!(matches!(r.contract.get("limit.weight").unwrap().status, Status::Warned { count, .. } if count > 0));
}

#[test]
fn a_drawdown_limit_of_a_tenth_after_a_twelve_percent_fall_rejects_increases_and_passes_reductions() {
    // 9,500 shares bought at 100 (filled at d1's close) are 95 % of the
    // million: the fall to 88 by d6 takes the NAV 11.4 % under its peak.
    // 10 more shares at d2 (no drawdown) fill; 10 more at d6 are rejected;
    // a sale of 100 at d7 fills.
    let closes = [100.0, 100.0, 100.0, 97.0, 94.0, 91.0, 88.0, 88.0, 88.0, 88.0];
    let ds = common::market("AAA", &closes);
    let src = StrategyParts {
        name: "dd".into(),
        params: "    big : Quantity<Shares> = 9500 shares\n    small : Quantity<Shares> = 10 shares\n    cut : Quantity<Shares> = 100 shares".into(),
        decides: [
            "    decide(T, trade(A, big)) :- universe(A, T), T = 2024-01-08.",
            "    decide(T, trade(A, small)) :- universe(A, T), T = 2024-01-10.",
            "    decide(T, trade(A, small)) :- universe(A, T), T = 2024-01-16.",
            "    decide(T, trade(A, Q)) :- universe(A, T), T = 2024-01-17, Q = -cut.",
        ]
        .join("\n"),
        execution: "equity_default with { leverage unconstrained   participation none   slippage none   impact none   limits { drawdown <= 0.1 reject } }".into(),
        ..Default::default()
    }
    .render();
    let p = common::program(&src, "dd");
    let r = run(&p, &ds, ExecConfig::from_program(&p)).unwrap();
    let day = |s: &str| abt_wire::scalar::parse_timestamp(s).unwrap();
    // The NAV at d6's mark is 11.4 % under the peak.
    let nav = |t: i64| r.equity_curve.iter().find(|x| x.0 == t).unwrap().1;
    let peak = r.equity_curve.iter().filter(|x| x.0 <= day("2024-01-16")).map(|x| x.1).fold(f64::MIN, f64::max);
    let dd = 1.0 - nav(day("2024-01-16")) / peak;
    assert!(dd > 0.1 && (dd - 0.114).abs() < 0.001, "{}", dd);
    // The fills: 9,500 and 10 before the fall, the sale of 100 after it.
    let filled: Vec<f64> = r.fills.iter().map(|f| f.quantity).collect();
    assert_eq!(filled, vec![9_500.0, 10.0, -100.0], "{:?}", r.fills);
    // The rejected order is d6's increase, rejected by the limit.
    let rejected: Vec<(i64, &String)> = r
        .events
        .iter()
        .filter_map(|e| match &e.kind {
            EventKind::Rejected { reason, .. } => Some((e.t, reason)),
            _ => None,
        })
        .collect();
    assert_eq!(rejected.len(), 1, "{:?}", rejected);
    assert_eq!(rejected[0].0, day("2024-01-16"));
    assert!(rejected[0].1.contains("limit `drawdown <= 0.1 reject`"), "{}", rejected[0].1);
}

/// E-119: positions bought at a weight limit's bound that a price rise
/// carries past it do not halt the run when the bar's orders only reduce.
/// The invariant checked every position of the book the bar's orders would
/// leave, the held ones and the reduced ones included, while a clamp only
/// scales what an order adds: the next bar with any order halted ("… above
/// `weight <= 0.1 per name clamp`"). A position an order grows past the
/// bound still fails.
#[test]
fn a_held_position_drifting_past_a_weight_limit_holds() {
    let src = StrategyParts {
        name: "drift".into(),
        params: "    w : Scalar = 0.1".into(),
        decides: "    decide(T, hold(A, w)) :- universe(A, T), flat(A, T).\n    decide(T, trade(A, -1 shares)) :- universe(A, T), held(A, T, Q), Q > 1000 shares.".into(),
        execution: "equity_default with { leverage unconstrained   limits { weight <= 0.1 per name clamp } }".into(),
        ..Default::default()
    }
    .render();
    let p = common::program(&src, "drift");
    let ds = abt_kernel::data::synthetic_daily(&["AAA", "BBB", "CCC", "DDD", "EEE"], (2022, 1, 3), 60, 5);
    // The run reaches its end (a violated invariant halts it).
    let r = run(&p, &ds, ExecConfig::from_program(&p)).unwrap_or_else(|e| panic!("{}", e));
    // Some name's weight went past the bound while held.
    let over = r.holdings.iter().any(|h| {
        let x = r.exposure.iter().find(|x| x.t == h.t).unwrap();
        h.position * h.mark.unwrap() / x.capital > 0.1 + 1e-6
    });
    assert!(over, "no held name drifted past a tenth: the test checks nothing");
    assert!(!matches!(r.contract.get("inv.position_limits").map(|c| &c.status), Some(Status::Violated { .. })));
}
