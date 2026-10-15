//! The shared fixtures of the integration tests (tests/common/mod.rs): the
//! strategy skeleton checks, runs on a crafted market and gives the
//! hand-computed executor outcomes.

use abt_tests as common;

use abt_kernel::kernel::{run, ExecConfig};

#[test]
fn skeleton_checks_and_runs() {
    let src = common::strategy(
        "up_down",
        "    qty : Quantity<Shares> = 10 shares",
        "    rel up(A: Equity, @T)\n    up(A, T) :- universe(A, T), price_return(A, T, R), R > 0.",
        "    decide(T, trade(A, qty)) :- up(A, T), flat(A, T).\n    decide(T, trade(A, Q)) :- held(A, T, Q0), prev(T, T0), decided(T0, trade(A, Q1)), Q1 > 0 shares, Q = -Q0.",
    );
    let prog = common::program(&src, "up_down");
    let ds = common::market("X", &[10.0, 11.0, 10.0, 12.0, 13.0]);
    let r = run(
        &prog,
        &ds,
        ExecConfig {
            initial_cash: 1000.0,
            ..ExecConfig::frictionless()
        },
    )
    .unwrap();
    let decisions: Vec<String> = r
        .decisions
        .iter()
        .map(|d| format!("{} {}", abt_kernel::kernel::time::format_timestamp(d.t), r.describe_decision(&d.decision)))
        .collect();
    eprintln!("{:?}", decisions);
    eprintln!(
        "fills {:?}",
        r.fills.iter().map(|f| (abt_kernel::kernel::time::format_timestamp(f.t), f.quantity, f.price)).collect::<Vec<_>>()
    );
    eprintln!("curve {:?} cash {}", r.equity_curve.iter().map(|(_, e)| *e).collect::<Vec<_>>(), r.final_cash);
    assert_eq!(decisions.len(), 4);
}
