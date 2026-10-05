//! Both orders of a window and the atoms it bounds are accepted inside an
//! aggregation and evaluate identically; outside one, a fresh key is still
//! a causality error.

mod corpus;

use absolute_backtest::check::{check_program, Code, Severity};
use absolute_backtest::data::synthetic_daily;
use absolute_backtest::kernel::{run, ExecConfig};

fn strategy(order_a: bool) -> String {
    let conj = if order_a {
        "T1 in window(T, lb, min k), close(A, T1, P)"
    } else {
        "close(A, T1, P), T1 in window(T, lb, min k)"
    };
    format!(
        r#"
strategy w {{
  env equities_1d
  uses features
  resolution @1d
  mode delta
  param qty : Quantity<Shares> = 10 shares
  param lb : Duration = 30d
  param k : Count = 15
  rel above_mean(-A: Equity, @T: Timestamp)
  above_mean(A, T) :- universe(A, T), close(A, T, C), M = mean(P) over ({}), C > M.
  decide(T, buy(A, qty)) :- above_mean(A, T), flat(A, T).
  decide(T, sell(A, Q)) :- held(A, T, Q), prev(T, T0), decided(T0, buy(A, _)).
}}
"#,
        conj
    )
}

#[test]
fn window_before_or_after_its_atom_is_the_same_program() {
    let ds = synthetic_daily(&["AAA", "BBB"], (2023, 1, 2), 120, 3);
    let mut outputs = Vec::new();
    for order_a in [true, false] {
        let mut ws = corpus::base_workspace();
        ws.add_source(&strategy(order_a)).unwrap();
        let (prog, diags) = check_program(&ws, "w");
        assert!(diags.is_empty(), "{:?}", diags.iter().map(|d| d.to_string()).collect::<Vec<_>>());
        let r = run(&prog.unwrap(), &ds, ExecConfig::default()).unwrap();
        assert!(!r.decisions.is_empty());
        outputs.push(r.decisions.iter().map(|d| (d.t, r.describe_decision(&d.decision))).collect::<Vec<_>>());
    }
    assert_eq!(outputs[0], outputs[1]);
}

#[test]
fn a_fresh_key_without_a_window_is_still_lookahead() {
    let src = r#"
strategy w2 {
  env equities_1d
  uses features
  resolution @1d
  mode delta
  param qty : Quantity<Shares> = 10 shares
  rel ever_higher(-A: Equity, @T: Timestamp)
  ever_higher(A, T) :- universe(A, T), close(A, T, C), M = max(P) over (close(A, T1, P)), C >= M.
  decide(T, buy(A, qty)) :- ever_higher(A, T), flat(A, T).
}
"#;
    let mut ws = corpus::base_workspace();
    ws.add_source(src).unwrap();
    let (_, diags) = check_program(&ws, "w2");
    let errors: Vec<_> = diags.iter().filter(|d| d.severity == Severity::Error).collect();
    assert!(
        !errors.is_empty() && errors.iter().all(|d| d.code == Code::F),
        "{:?}",
        diags.iter().map(|d| d.to_string()).collect::<Vec<_>>()
    );
}
