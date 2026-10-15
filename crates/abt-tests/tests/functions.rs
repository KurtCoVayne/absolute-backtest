//! Scalar functions over mixed integer literals and Scalars: `least(1, W)`
//! caps at one, whatever the enum order of the values behind it.

use abt_tests as common;

use abt_kernel::data::synthetic_daily;
use abt_kernel::kernel::{ExecConfig, Kernel, Value};

fn capped() -> String {
    common::strategy(
        "capped",
        "    raw : Scalar = 2.5",
        "    rel w(A: Equity, @T, W: Scalar)
    w(A, T, W) :- universe(A, T), W = least(1, raw).
    rel g(A: Equity, @T, W: Scalar)
    g(A, T, W) :- universe(A, T), W = greatest(0, 0 - raw).",
        "    decide(T, hold(A, W)) :- w(A, T, W).",
    )
}

#[test]
fn least_and_greatest_compare_integer_literals_numerically() {
    let prog = common::program(&capped(), "capped");
    let ds = synthetic_daily(&["AAA"], (2022, 1, 3), 5, 7);
    let mut k = Kernel::new(&prog, &ds, ExecConfig::frictionless()).unwrap();
    let result = k.run().unwrap();
    let t = result.bars[0];
    let w = k.query("w", t, &[]).unwrap();
    assert_eq!(w[0][2], Value::Num(1.0), "least(1, 2.5) is 1, a Scalar");
    let g = k.query("g", t, &[]).unwrap();
    assert_eq!(g[0][2], Value::Num(0.0), "greatest(0, -2.5) is 0, a Scalar");
    assert!(result.decisions.iter().all(|d| (d.decision.amount - 1.0).abs() < 1e-12), "every target is capped at one");
}
