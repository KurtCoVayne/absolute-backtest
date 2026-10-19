//! The chain filters of D-4.4 as one predicate. What the data layer's
//! `chain` query answers, what the resolver's pre-pass collects (D-7.6) and
//! what the kernel's `chain` relation holds (K-2.4) are the same contracts,
//! so the three read this one implementation (A-D2), as they read
//! [`crate::scalar::add_duration`] for a window's calendar arithmetic.

use crate::generated::abt::wire::{ChainFilters, Duration};
use crate::scalar::add_duration;

/// A contract's terms, as the filters read them.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Terms<'a> {
    pub expiry: i64,
    pub strike: f64,
    /// `call` or `put`.
    pub right: &'a str,
    /// `american` or `european`.
    pub style: &'a str,
    /// The open interest of the latest quote admitted, when one is.
    pub open_interest: Option<i64>,
}

fn after(t: i64, d: &Option<Box<Duration>>) -> Option<i64> {
    d.as_ref().map(|d| add_duration(t, d.months, d.days, d.nanos))
}

/// Whether the filters admit a contract at `t`, against `spot` (the
/// underlying's price as the caller read it: the data layer does not know
/// which price is spot). Each filter applies when it is stated:
///
/// - the expiry in `[t + expiry_from, t + expiry_to]`, both ends included;
/// - the moneyness, strike over spot, in `[moneyness_lo, moneyness_hi]`
///   (without a positive spot, a moneyness filter admits nothing);
/// - the right and the style, equal;
/// - the strike in `[strike_lo, strike_hi]`;
/// - the open interest at least `min_open_interest` (a contract without a
///   quote has none).
pub fn admits(f: &ChainFilters, t: i64, spot: Option<f64>, c: &Terms) -> bool {
    if after(t, &f.expiry_from).is_some_and(|lo| c.expiry < lo) || after(t, &f.expiry_to).is_some_and(|hi| c.expiry > hi) {
        return false;
    }
    if f.moneyness_lo.is_some() || f.moneyness_hi.is_some() {
        let Some(s) = spot.filter(|s| *s > 0.0 && s.is_finite()) else { return false };
        let m = c.strike / s;
        if f.moneyness_lo.is_some_and(|lo| m < lo) || f.moneyness_hi.is_some_and(|hi| m > hi) {
            return false;
        }
    }
    if f.right.as_deref().is_some_and(|r| r != c.right) || f.style.as_deref().is_some_and(|s| s != c.style) {
        return false;
    }
    if f.strike_lo.is_some_and(|k| c.strike < k) || f.strike_hi.is_some_and(|k| c.strike > k) {
        return false;
    }
    f.min_open_interest.is_none_or(|n| c.open_interest.unwrap_or(0) >= n)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scalar::parse_timestamp;

    fn d(months: i64, days: i64) -> Option<Box<Duration>> {
        Some(Box::new(Duration { months, days, nanos: 0 }))
    }

    #[test]
    fn each_filter_admits_its_range_ends_included() {
        let t = parse_timestamp("2024-03-11").unwrap();
        let f = ChainFilters {
            expiry_from: d(0, 20),
            expiry_to: d(1, 15),
            moneyness_lo: Some(0.9),
            moneyness_hi: Some(1.0),
            right: Some("put".into()),
            style: None,
            strike_lo: None,
            strike_hi: None,
            min_open_interest: Some(10),
        };
        let at = |s: &str| parse_timestamp(s).unwrap();
        let c = |expiry: i64, strike: f64, right: &'static str, oi: Option<i64>| Terms {
            expiry,
            strike,
            right,
            style: "american",
            open_interest: oi,
        };
        // 20 days on is 2024-03-31; a month and 15 days on, 2024-04-26.
        assert!(admits(&f, t, Some(100.0), &c(at("2024-03-31"), 90.0, "put", Some(10))));
        assert!(admits(&f, t, Some(100.0), &c(at("2024-04-26"), 100.0, "put", Some(10))));
        assert!(!admits(&f, t, Some(100.0), &c(at("2024-03-30"), 95.0, "put", Some(10))));
        assert!(!admits(&f, t, Some(100.0), &c(at("2024-04-27"), 95.0, "put", Some(10))));
        assert!(!admits(&f, t, Some(100.0), &c(at("2024-04-01"), 89.9, "put", Some(10))));
        assert!(!admits(&f, t, Some(100.0), &c(at("2024-04-01"), 100.1, "put", Some(10))));
        assert!(!admits(&f, t, Some(100.0), &c(at("2024-04-01"), 95.0, "call", Some(10))));
        assert!(!admits(&f, t, Some(100.0), &c(at("2024-04-01"), 95.0, "put", Some(9))));
        assert!(!admits(&f, t, Some(100.0), &c(at("2024-04-01"), 95.0, "put", None)));
        // No spot: a moneyness filter admits nothing.
        assert!(!admits(&f, t, None, &c(at("2024-04-01"), 95.0, "put", Some(10))));
        // No filter at all admits every contract.
        let none = ChainFilters::default();
        assert!(admits(&none, t, None, &c(at("2020-01-01"), 1.0, "call", None)));
    }
}
