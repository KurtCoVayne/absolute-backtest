//! The price literal `60 USD/share` (section 2: Price<USD> is currency per
//! share): it types as Price<USD>, so a Price parameter can be declared and
//! compared with Price-valued features; `60 USD` stays Notional<USD>.

mod corpus;

use absolute_backtest::check::{check_program, Code, Severity};
use absolute_backtest::kernel::time::{format_timestamp, parse_timestamp};
use absolute_backtest::kernel::{run, Dataset, ExecConfig, Value};
use absolute_backtest::{parse_units, UnitKind};

fn strategy(default: &str) -> String {
    format!(
        r#"
strategy floor_test {{
  env equities_1d
  uses features
  resolution @1d
  mode delta
  param floor : Price<USD> = {default}
  param qty : Quantity<Shares> = 10 shares
  rel pricey(-A: Equity, @T: Timestamp)
  pricey(A, T) :- universe(A, T), close(A, T, P), P > floor.
  decide(T, buy(A, qty)) :- pricey(A, T), flat(A, T).
  decide(T, sell(A, Q)) :- held(A, T, Q), prev(T, T0), decided(T0, buy(A, _)).
}}
"#
    )
}

#[test]
fn price_literal_types_as_price_per_share() {
    let src = strategy("60 USD/share in 10 USD/share..500 USD/share");
    let units = parse_units(&src).unwrap_or_else(|e| panic!("`60 USD/share` should parse: {}", e));
    let strat = units.iter().find(|u| u.kind == UnitKind::Strategy).unwrap();
    let floor = strat.params.iter().find(|p| p.name == "floor").unwrap();
    assert_eq!(floor.value.ty().to_string(), "Price<USD>");
    assert_eq!(floor.value.to_string(), "60 USD/share");
    let (lo, hi) = floor.range.as_ref().unwrap();
    assert_eq!(lo.ty().to_string(), "Price<USD>");
    assert_eq!(hi.ty().to_string(), "Price<USD>");

    // The plural spelling is the same literal.
    let units = parse_units(&strategy("60 USD/shares")).unwrap();
    let strat = units.iter().find(|u| u.kind == UnitKind::Strategy).unwrap();
    assert_eq!(strat.params[0].value.to_string(), "60 USD/share");

    let mut ws = corpus::base_workspace();
    ws.add_source(&src).unwrap();
    let (program, diags) = check_program(&ws, "floor_test");
    let text: Vec<String> = diags.iter().map(|d| d.to_string()).collect();
    assert!(diags.is_empty(), "a Price<USD> parameter with a price literal default should check clean, got:\n{}", text.join("\n"));
    assert!(program.is_some());
}

#[test]
fn money_literal_stays_notional() {
    let mut ws = corpus::base_workspace();
    ws.add_source(&strategy("60 USD")).unwrap();
    let (program, diags) = check_program(&ws, "floor_test");
    let errors: Vec<_> = diags.iter().filter(|d| d.severity == Severity::Error).collect();
    assert!(!errors.is_empty(), "`60 USD` is Notional<USD>, not a Price<USD> default");
    assert!(errors.iter().all(|d| d.code == Code::T), "{:?}", diags.iter().map(|d| d.to_string()).collect::<Vec<_>>());
    assert!(program.is_none());
}

#[test]
fn price_literal_is_a_number_in_the_kernel() {
    let mut ws = corpus::base_workspace();
    ws.add_source(&strategy("60 USD/share")).unwrap();
    let (program, diags) = check_program(&ws, "floor_test");
    let prog = program.unwrap_or_else(|| panic!("{:?}", diags.iter().map(|d| d.to_string()).collect::<Vec<_>>()));

    let mut ds = Dataset::new();
    let x = ds.intern("X");
    let days = ["2024-01-08", "2024-01-09", "2024-01-10", "2024-01-11", "2024-01-12"];
    for (d, p) in days.iter().zip([50.0, 70.0, 55.0, 61.0, 59.0]) {
        let t = parse_timestamp(d).unwrap();
        ds.add("close", vec![Value::Equity(x), Value::Time(t), Value::Num(p)]);
        ds.add("volume", vec![Value::Equity(x), Value::Time(t), Value::Num(1000.0)]);
        ds.add("universe", vec![Value::Equity(x), Value::Time(t)]);
    }
    let r = run(&prog, &ds, ExecConfig::default()).unwrap();
    let decisions: Vec<String> = r.decisions.iter().map(|d| format!("{} {}", format_timestamp(d.t), r.describe_decision(&d.decision))).collect();
    // Buy when the close is above 60 USD/share and flat, sell the bar after.
    assert_eq!(decisions, vec!["2024-01-09 buy(X, 10)", "2024-01-10 sell(X, 10)", "2024-01-11 buy(X, 10)", "2024-01-12 sell(X, 10)"]);
}
