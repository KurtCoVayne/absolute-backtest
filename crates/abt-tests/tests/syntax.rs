//! The numeric literal grammar (adv-19): digits with `_` separators, an
//! optional fraction with digits on both sides of the point, an optional
//! exponent, and a leading `-`; no leading point and no leading `+`.

use abt_tests as common;

use abt_kernel::parse_units;

/// The skeleton strategy with a Scalar parameter whose default is `default`.
fn strategy_with(default: &str) -> String {
    common::strategy(
        "pn",
        &format!("    a : Scalar = {}\n    qty : Quantity<Shares> = 100 shares", default),
        "",
        "    decide(T, trade(A, qty)) :- price_return(A, T, R), R > a, flat(A, T).",
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
