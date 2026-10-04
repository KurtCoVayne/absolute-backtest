//! The numeric literal grammar (adv-19): digits with `_` separators, an
//! optional fraction with digits on both sides of the point, an optional
//! exponent, and a leading `-`; no leading point and no leading `+`.

use absolute_backtest::parse_units;

fn strategy_with(default: &str) -> String {
    format!(
        r#"
strategy pn {{
  env equities_1d
  uses features
  resolution @1d
  mode delta
  param a : Scalar = {}
  decide(T, buy(A, 100 shares)) :- universe(A, T), momentum(A, T, 3mo, 0d, M), M > a, flat(A, T).
}}
"#,
        default
    )
}

#[test]
fn numeric_literals_accept_fraction_exponent_separator_and_minus() {
    for lit in ["0.5", "-0.5", "1e5", "1E5", "2.5e-3", "1_000.5", "1_000_000"] {
        assert!(parse_units(&strategy_with(lit)).is_ok(), "`{}` should parse", lit);
    }
}

#[test]
fn numeric_literals_reject_a_leading_point_or_plus() {
    for (lit, found) in [(".5", "."), ("+0.5", "+")] {
        let err = parse_units(&strategy_with(lit)).err().unwrap_or_else(|| panic!("`{}` should not parse", lit));
        let msg = err.to_string();
        assert!(msg.contains("expected a literal") && msg.contains(found), "`{}`: {}", lit, msg);
    }
}
