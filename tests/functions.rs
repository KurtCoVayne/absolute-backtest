//! Scalar functions over mixed integer literals and Scalars: `least(1, W)`
//! caps at one, whatever the enum order of the values behind it.

mod corpus;

use absolute_backtest::check::{check_program, Program, Workspace};
use absolute_backtest::data::synthetic_daily;
use absolute_backtest::kernel::{ExecConfig, Kernel, Value};

fn program(extra: &str, name: &str) -> (Program, Workspace) {
    let mut ws = corpus::base_workspace();
    ws.add_source(extra).unwrap();
    let (p, diags) = check_program(&ws, name);
    let text: Vec<String> = diags.iter().map(|d| d.to_string()).collect();
    (p.unwrap_or_else(|| panic!("{} should check:\n{}", name, text.join("\n"))), ws)
}

const CAPPED: &str = r#"
strategy capped {
  env equities_1d
  resolution @1d
  mode target
  param raw : Scalar = 2.5
  rel w(-A: Equity, @T: Timestamp, -W: Scalar)
  w(A, T, W) :- universe(A, T), W = least(1, raw).
  rel g(-A: Equity, @T: Timestamp, -W: Scalar)
  g(A, T, W) :- universe(A, T), W = greatest(0, 0 - raw).
  decide(T, target_weight(A, W)) :- w(A, T, W).
}
"#;

#[test]
fn least_and_greatest_compare_integer_literals_numerically() {
    let (prog, _) = program(CAPPED, "capped");
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
