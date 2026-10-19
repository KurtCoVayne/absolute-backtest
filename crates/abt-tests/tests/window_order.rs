//! Both orders of a window and the atoms it bounds are accepted inside an
//! aggregation and evaluate identically; outside one, a fresh key is still
//! a causality error.

use abt_tests as common;

use abt_kernel::check::{Code, Severity};
use abt_kernel::data::synthetic_daily;
use abt_kernel::kernel::ExecConfig;
use abt_tests::run;

fn strategy(order_a: bool) -> String {
    let conj = if order_a {
        "T1 in window[lb] min k, close(A, T1, P)"
    } else {
        "close(A, T1, P), T1 in window[lb] min k"
    };
    common::strategy(
        "w",
        "    qty : Quantity<Shares> = 10 shares
    lb : Duration = 30d
    k : Count = 15",
        &format!(
            "    rel above_mean(A: Equity, @T)
    above_mean(A, T) :- universe(A, T), close(A, T, C), M = mean(P) over ({}), C > M.",
            conj
        ),
        "    decide(T, trade(A, qty)) :- above_mean(A, T), flat(A, T).
    decide(T, trade(A, Q)) :- held(A, T, Q0), prev(T, T0), decided(T0, trade(A, Q1)), Q1 > 0 shares, Q = -Q0.",
    )
}

#[test]
fn window_before_or_after_its_atom_is_the_same_program() {
    let ds = synthetic_daily(&["AAA", "BBB"], (2023, 1, 2), 120, 3);
    let mut outputs = Vec::new();
    for order_a in [true, false] {
        let (prog, diags) = common::check(&strategy(order_a), "w");
        assert!(diags.is_empty(), "{}", common::text(&diags));
        let r = run(&prog.unwrap(), &ds, ExecConfig::default()).unwrap();
        assert!(!r.decisions.is_empty());
        outputs.push(r.decisions.iter().map(|d| (d.t, r.describe_decision(&d.decision))).collect::<Vec<_>>());
    }
    assert_eq!(outputs[0], outputs[1]);
}

#[test]
fn a_fresh_key_without_a_window_is_still_lookahead() {
    let src = common::strategy(
        "w2",
        "    qty : Quantity<Shares> = 10 shares",
        "    rel ever_higher(A: Equity, @T)
    ever_higher(A, T) :- universe(A, T), close(A, T, C), M = max(P) over (close(A, T1, P)), C >= M.",
        "    decide(T, trade(A, qty)) :- ever_higher(A, T), flat(A, T).",
    );
    let (_, diags) = common::check(&src, "w2");
    let errors: Vec<_> = diags.iter().filter(|d| d.severity == Severity::Error).collect();
    assert!(!errors.is_empty() && errors.iter().all(|d| d.code == Code::F), "{}", common::text(&diags));
}
