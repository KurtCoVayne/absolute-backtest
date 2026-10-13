//! The price literal `60 USD/share` (section 2: Price<USD> is currency per
//! share; language-v2 3.2: spelled with the class's unit, or `USD/unit`):
//! it types as Price<USD>, so a Price parameter can be declared and
//! compared with Price-valued relations; `60 USD` stays Notional<USD>.

mod common;

use absolute_backtest::check::{Code, Severity};
use absolute_backtest::kernel::time::format_timestamp;
use absolute_backtest::kernel::{run, ExecConfig};
use absolute_backtest::parse_units;
use absolute_backtest::syntax::ast::Unit;

/// A strategy with a `Price<USD>` parameter `floor` whose default is
/// `default`: buy a name whose close is above the floor, sell it the bar
/// after the buy was decided.
fn strategy(default: &str) -> String {
    common::strategy(
        "floor_test",
        &format!("    floor : Price<USD> = {}\n    qty : Quantity<Shares> = 10 shares", default),
        "    rel pricey(A: Equity, @T)\n    pricey(A, T) :- universe(A, T), close(A, T, P), P > floor.",
        "    decide(T, trade(A, qty)) :- pricey(A, T), flat(A, T).\n    decide(T, trade(A, Q)) :- held(A, T, Q0), prev(T, T0), decided(T0, trade(A, Q1)), Q1 > 0 shares, Q = -Q0.",
    )
}

/// The parsed `floor` parameter of the strategy.
fn floor_param(src: &str) -> absolute_backtest::syntax::ast::ParamDecl {
    let units = parse_units(src).unwrap_or_else(|e| panic!("the strategy should parse: {}", e));
    let strat = units
        .iter()
        .find_map(|u| match u {
            Unit::Strategy(s) => Some(s),
            _ => None,
        })
        .expect("a strategy unit");
    strat.params.as_ref().unwrap().params.iter().find(|p| p.name == "floor").cloned().unwrap()
}

#[test]
fn price_literal_types_as_price_per_share() {
    let src = strategy("60 USD/share in 10 USD/share..500 USD/share");
    let floor = floor_param(&src);
    assert_eq!(floor.value.ty().to_string(), "Price<USD>");
    // The literal is printed with the class-agnostic unit word (3.2: both
    // spellings denote the dimension).
    assert_eq!(floor.value.to_string(), "60 USD/unit");
    let (lo, hi) = floor.range.as_ref().unwrap();
    assert_eq!(lo.ty().to_string(), "Price<USD>");
    assert_eq!(hi.ty().to_string(), "Price<USD>");

    // The class-agnostic spelling is the same literal.
    let floor = floor_param(&strategy("60 USD/unit"));
    assert_eq!(floor.value.to_string(), "60 USD/unit");
    assert_eq!(floor.value.ty().to_string(), "Price<USD>");

    let (program, diags) = common::check(&src, "floor_test");
    assert!(
        diags.is_empty(),
        "a Price<USD> parameter with a price literal default should check clean, got:\n{}",
        common::text(&diags)
    );
    assert!(program.is_some());
}

#[test]
fn money_literal_stays_notional() {
    let (program, diags) = common::check(&strategy("60 USD"), "floor_test");
    let errors = common::errors(&diags);
    assert!(!errors.is_empty(), "`60 USD` is Notional<USD>, not a Price<USD> default");
    assert!(errors.iter().all(|(c, _)| *c == Code::T), "{}", common::text(&diags));
    // The diagnostic names the per-unit form to write.
    assert!(errors.iter().any(|(_, m)| m.contains("USD/share")), "{}", common::text(&diags));
    assert!(diags.iter().all(|d| d.severity == Severity::Error), "{}", common::text(&diags));
    assert!(program.is_none());
}

#[test]
fn price_literal_is_a_number_in_the_kernel() {
    let prog = common::program(&strategy("60 USD/share"), "floor_test");
    let ds = common::market("X", &[50.0, 70.0, 55.0, 61.0, 59.0]);
    let r = run(
        &prog,
        &ds,
        ExecConfig {
            initial_cash: 10_000.0,
            ..ExecConfig::frictionless()
        },
    )
    .unwrap();
    let decisions: Vec<String> = r.decisions.iter().map(|d| format!("{} {}", format_timestamp(d.t), r.describe_decision(&d.decision))).collect();
    // Buy when the close is above 60 USD/share and flat, sell the bar after.
    assert_eq!(
        decisions,
        vec!["2024-01-09 trade(X, 10)", "2024-01-10 trade(X, -10)", "2024-01-11 trade(X, 10)", "2024-01-12 trade(X, -10)"]
    );
}
