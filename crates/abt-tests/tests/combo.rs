//! The decision algebra's multi-leg and exercise decisions (K-2.3):
//! `combo([leg(A1, Q1), ..., leg(An, Qn)])` with an optional order, at least
//! two legs on distinct instruments; `exercise(A, Q)` admitted only in the
//! option class (T). Both parse, print and check; the kernel refuses to run
//! them until the options model executes them (M-7).

use abt_kernel::check::Code;
use abt_kernel::data::synthetic_daily;
use abt_kernel::kernel::{ExecConfig, RunError};
use abt_tests as common;
use abt_tests::StrategyParts;

fn program_with(decides: &str) -> String {
    StrategyParts {
        params: "    qty : Quantity<Shares> = 10 shares".into(),
        decides: decides.into(),
        ..Default::default()
    }
    .render()
}

fn t_errors(src: &str) -> Vec<String> {
    let (_, diags) = common::check(src, "s");
    common::errors(&diags).into_iter().filter(|(c, _)| *c == Code::T).map(|(_, m)| m).collect()
}

#[test]
fn a_combo_of_two_legs_checks_and_prints_back() {
    let src = program_with("    decide(T, combo([leg(\"AAA\", qty), leg(\"BBB\", Q)])) :- bar(T), Q = -qty.");
    let (p, diags) = common::check(&src, "s");
    assert!(p.is_some(), "{}", common::text(&diags));
    // With an order: the limit is the combo's net price.
    let src = program_with("    decide(T, combo([leg(\"AAA\", qty), leg(\"BBB\", Q)], limit(1 USD/share))) :- bar(T), Q = -qty.");
    assert!(t_errors(&src).is_empty(), "{:?}", t_errors(&src));
    let units = abt_kernel::parse_units(&src).unwrap();
    let Some(abt_kernel::syntax::Unit::Strategy(s)) = units.into_iter().next() else { panic!() };
    let printed = abt_kernel::syntax::print::render_strategy(
        &s,
        &abt_kernel::syntax::print::Options {
            execution: None,
            report: None,
            rule_style: None,
        },
    );
    assert!(printed.contains("combo([leg(\"AAA\", qty), leg(\"BBB\", Q)], limit("), "{}", printed);
}

#[test]
fn a_combo_has_two_distinct_legs_of_deltas() {
    let one = program_with("    decide(T, combo([leg(\"AAA\", qty)])) :- bar(T).");
    assert!(t_errors(&one).iter().any(|m| m.contains("at least two legs")), "{:?}", t_errors(&one));
    let same = program_with("    decide(T, combo([leg(\"AAA\", qty), leg(\"AAA\", qty)])) :- bar(T).");
    assert!(t_errors(&same).iter().any(|m| m.contains("distinct instruments")), "{:?}", t_errors(&same));
    let not_leg = program_with("    decide(T, combo([leg(\"AAA\", qty), trade(\"BBB\", qty)])) :- bar(T).");
    assert!(t_errors(&not_leg).iter().any(|m| m.contains("is not a leg")), "{:?}", t_errors(&not_leg));
    let weight = program_with("    decide(T, combo([leg(\"AAA\", 0.5), leg(\"BBB\", qty)])) :- bar(T).");
    assert!(t_errors(&weight).iter().any(|m| m.contains("delta of `leg`")), "{:?}", t_errors(&weight));
    let bare = program_with("    decide(T, combo(leg(\"AAA\", qty), leg(\"BBB\", qty))) :- bar(T).");
    assert!(t_errors(&bare).iter().any(|m| m.contains("a combo is `combo([")), "{:?}", t_errors(&bare));
}

#[test]
fn exercise_is_the_option_classs() {
    let src = program_with("    decide(T, exercise(A, qty)) :- universe(A, T).");
    let errs = t_errors(&src);
    assert!(errs.iter().any(|m| m.contains("`exercise` takes an option") && m.contains("`equity`")), "{:?}", errs);
}

#[test]
fn the_kernel_refuses_to_run_a_combo_until_it_executes_one() {
    let src = program_with("    decide(T, combo([leg(\"AAA\", qty), leg(\"BBB\", Q)])) :- bar(T), Q = -qty.");
    let p = common::program(&src, "s");
    let ds = synthetic_daily(&["AAA", "BBB"], (2022, 1, 3), 30, 3);
    match common::run(&p, &ds, ExecConfig::from_program(&p)) {
        Err(RunError::Config(m)) => assert!(m.contains("combo") && m.contains("does not execute"), "{}", m),
        other => panic!("{:?}", other.map(|r| r.decisions.len())),
    }
}
