//! The validation schemes a plan's steps use (Q-2.3, Q-2.4, Q-3.1), moved
//! with the kernel study's tests of them: hold-outs month-aligned and
//! seeded, walk-forward folds as their scheme states, the surface's
//! stability and smoothness around the best point, and Sharpe ratios per
//! calendar year.

use abt_query::metrics::validate::{block_embargo, subperiod_sharpes, surface, trailing_embargo, WalkForward};
use abt_query::plan::parse::holdout;
use abt_query::plan::Holdout;
use abt_query::time::{day_key, format_timestamp, month_key, parse_timestamp, DAY};

fn ts(s: &str) -> i64 {
    parse_timestamp(s).unwrap_or_else(|| panic!("`{}` is not a date", s))
}

#[test]
fn embargoes_are_month_aligned_and_seeded() {
    let e = trailing_embargo(ts("2024-06-14"), 2);
    assert!(e.truncates);
    assert_eq!(e.intervals, vec![(ts("2022-06-14"), i64::MAX)]);
    assert!(e.contains(ts("2023-01-01")) && !e.contains(ts("2022-06-13")));
    let b = block_embargo(ts("2022-01-03"), ts("2024-12-31"), 3, 2, 7);
    assert!(!b.truncates);
    assert_eq!(b.intervals.len(), 3);
    for (a, z) in &b.intervals {
        assert!(*a >= ts("2022-01-01") && *z <= ts("2025-01-01"));
        assert_eq!(format_timestamp(*a).split('-').nth(2), Some("01"), "a block starts on the first of a month");
    }
    for w in b.intervals.windows(2) {
        assert!(w[0].1 <= w[1].0, "blocks do not overlap: {:?}", b.intervals);
    }
    // The seed decides the blocks.
    assert_eq!(b.intervals, block_embargo(ts("2022-01-03"), ts("2024-12-31"), 3, 2, 7).intervals);
    assert_ne!(b.intervals, block_embargo(ts("2022-01-03"), ts("2024-12-31"), 3, 2, 8).intervals);
    assert_eq!(holdout("trailing:2y"), Ok(Holdout::Trailing { months: 24 }));
    assert!(holdout("trailing:0y").is_err());
    assert!(holdout("blocks:4:3mo").is_ok());
    assert!(holdout("blocks:0:3mo").is_err());
    assert!(holdout("sometimes").is_err());
}

#[test]
fn walk_forward_folds_follow_the_scheme() {
    let anchored = WalkForward::parse("anchored:12mo:3mo").unwrap();
    let folds = anchored.folds(ts("2022-01-03"), ts("2023-12-29"));
    // Train from January 2022; test windows of three months: four fit in 2023.
    assert_eq!(folds.len(), 4, "{:?}", folds);
    assert_eq!(folds[0], (ts("2022-01-01"), ts("2023-01-01"), ts("2023-01-01"), ts("2023-04-01")));
    assert_eq!(folds[3], (ts("2022-01-01"), ts("2023-10-01"), ts("2023-10-01"), ts("2024-01-01")));
    let rolling = WalkForward::parse("rolling:1y:6mo").unwrap();
    let folds = rolling.folds(ts("2022-01-03"), ts("2023-12-29"));
    assert_eq!(folds.len(), 2);
    assert_eq!(folds[1], (ts("2022-07-01"), ts("2023-07-01"), ts("2023-07-01"), ts("2024-01-01")));
    assert!(WalkForward::parse("anchored:2y").is_err());
    assert!(WalkForward::parse("sideways:2y:6mo").is_err());
    assert_eq!(WalkForward::parse("rolling:2y:6mo").unwrap().describe(), "rolling:24mo:6mo");
}

fn axis(name: &str, values: &[&str]) -> (String, Vec<String>) {
    (name.to_string(), values.iter().map(|v| v.to_string()).collect())
}

#[test]
fn the_surface_measures_a_peak_against_a_plateau() {
    let n = axis("n", &["1", "2", "3"]);
    let s = surface(std::slice::from_ref(&n), &[1.0, 1.05, 0.2], 1).unwrap();
    assert_eq!(s.neighbours, 2);
    assert!((s.stability - 0.5).abs() < 1e-12);
    assert!((s.smoothness - (0.05 + 0.85) / 2.0 / 0.85).abs() < 1e-12, "{}", s.smoothness);
    let flat = surface(std::slice::from_ref(&n), &[1.0, 1.02, 0.98], 1).unwrap();
    assert_eq!(flat.stability, 1.0);
    assert!(surface(&[n], &[1.0, 2.0], 0).is_none(), "a mismatched grid has no surface");
    let two = [axis("a", &["1", "2"]), axis("b", &["1", "2", "3"])];
    let s = surface(&two, &[1.0, 1.0, 1.0, 1.0, 5.0, 1.0], 4).unwrap();
    assert_eq!(s.neighbours, 3, "the middle of the second row has three neighbours");
    assert_eq!(s.stability, 0.0);
}

/// `n` business days from `start` (the synthetic market's bars).
fn business_days(start: &str, n: usize) -> Vec<i64> {
    let mut out = Vec::new();
    let mut t = ts(start);
    while out.len() < n {
        // 1970-01-01 was a Thursday: day 0 is weekday 3 counted from Monday.
        if (day_key(t) + 3).rem_euclid(7) < 5 {
            out.push(t);
        }
        t += DAY;
    }
    out
}

#[test]
fn sub_period_sharpes_are_per_calendar_year() {
    let mut c = Vec::new();
    let mut e = 100.0;
    for (i, t) in business_days("2022-01-03", 520).into_iter().enumerate() {
        // Drifting up through 2022 and down from 2023 on, with a wobble so
        // that each year has a volatility of its own.
        let drift = if month_key(t).0 == 2022 { 0.001 } else { -0.001 };
        e *= 1.0 + drift + 0.002 * (i as f64).sin();
        c.push((t, e));
    }
    let s = subperiod_sharpes(&c, 252.0, 20);
    // 2024 holds too few bars for a Sharpe ratio of its own.
    assert_eq!(s.iter().map(|(y, _)| *y).collect::<Vec<_>>(), vec![2022, 2023]);
    assert!(s[0].1 > 0.0 && s[1].1 < 0.0, "{:?}", s);
}
