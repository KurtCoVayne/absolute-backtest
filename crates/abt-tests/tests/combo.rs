//! The decision algebra's multi-leg and exercise decisions (K-2.3):
//! `combo([leg(A1, Q1), ..., leg(An, Qn)])` with an optional order, at least
//! two legs on distinct instruments; `exercise(A, Q)` admitted only in the
//! option class (T). Both parse, print and check; a combo of equities fills
//! all or none (K-5.5.3), and a decision on one of its legs is a decision on
//! each (F-5). The option cases are tests/options.rs's.

use abt_kernel::check::Code;
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

/// AAA and BBB over ten weekdays, at 10 and 20.
fn two_names() -> abt_kernel::kernel::Dataset {
    common::markets((2022, 1, 3), &[("AAA", &[10.0; 10]), ("BBB", &[20.0; 10])])
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
fn a_combo_of_equities_fills_both_legs_at_the_next_bar_under_one_order() {
    let src = program_with("    decide(T, combo([leg(\"AAA\", qty), leg(\"BBB\", Q)])) :- bar(T), first_row(\"AAA\", T), Q = -qty.");
    let p = common::program(&src, "s");
    let ds = two_names();
    let r = common::run(&p, &ds, ExecConfig::from_program(&p)).unwrap_or_else(|e| panic!("{}", e));
    let fills: Vec<_> = r.events.iter().filter(|e| e.kind.name() == "fill").collect();
    assert_eq!(fills.len(), 2, "{:#?}", r.events);
    assert_eq!(fills[0].t, fills[1].t);
    assert_eq!(fills[0].order_id, fills[1].order_id);
    let qty: Vec<f64> = r.fills.iter().map(|f| f.quantity).collect();
    assert_eq!(qty, vec![10.0, -10.0]);
}

#[test]
fn a_decision_on_a_combos_leg_is_a_second_decision_on_it() {
    let src =
        program_with("    decide(T, combo([leg(\"AAA\", qty), leg(\"BBB\", Q)])) :- bar(T), first_row(\"AAA\", T), Q = -qty.\n    decide(T, trade(\"BBB\", qty)) :- bar(T), first_row(\"BBB\", T).");
    let p = common::program(&src, "s");
    let ds = two_names();
    match common::run(&p, &ds, ExecConfig::from_program(&p)) {
        Err(RunError::Conflict { equity, .. }) => assert_eq!(equity, "BBB"),
        other => panic!("{:?}", other.map(|r| r.decisions.len())),
    }
}
