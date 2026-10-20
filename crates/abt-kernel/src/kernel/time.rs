//! Calendar arithmetic over Unix timestamps in nanoseconds (naive wall
//! clock; docs/spec/00-architecture.md, A-D7, and docs/spec/ERRATA.md, E-18):
//! civil dates, calendar-duration subtraction and resolution buckets.

use crate::ir::{Cal, Duration, Resolution};

pub const SEC: i64 = 1_000_000_000;
pub const MIN: i64 = 60 * SEC;
pub const HOUR: i64 = 60 * MIN;
pub const DAY: i64 = 24 * HOUR;

/// A time of `s` whole seconds.
pub const fn from_secs(s: i64) -> i64 {
    s * SEC
}

/// The seconds of a span of nanoseconds, as a float. A whole number of
/// seconds converts through the integer, so a float computed from it has
/// the bits it had when times were seconds.
pub fn secs_f64(d: i64) -> f64 {
    if d % SEC == 0 {
        (d / SEC) as f64
    } else {
        d as f64 / SEC as f64
    }
}

/// The civil calendar's conversions, shared with the data layer (abt-wire).
pub use abt_wire::scalar::{civil_from_days, days_from_civil, days_in_month};

pub fn floor_div(a: i64, b: i64) -> i64 {
    let q = a / b;
    if (a % b != 0) && ((a < 0) != (b < 0)) {
        q - 1
    } else {
        q
    }
}

/// Day-of-week with Monday = 0.
pub fn weekday(t: i64) -> u32 {
    (floor_div(t, DAY).rem_euclid(7) + 3) as u32 % 7
}

/// `t − d` in calendar terms: months move the civil date (day clamped to
/// the target month), then days subtract whole days, then the clock part.
pub fn sub_duration(t: i64, d: Duration) -> i64 {
    abt_wire::scalar::sub_duration(t, d.months, d.days, d.nanos)
}

/// `t + d` in calendar terms (the inverse of `sub_duration` for whole days
/// and clock parts; months move the civil date forward, day clamped).
pub fn add_duration(t: i64, d: Duration) -> i64 {
    abt_wire::scalar::add_duration(t, d.months, d.days, d.nanos)
}

/// The civil week of `t` (Monday-based), as a count of weeks since the epoch's Monday.
pub fn week_key(t: i64) -> i64 {
    // 1970-01-01 was a Thursday: shift so that Mondays start weeks.
    floor_div(floor_div(t, DAY) + 3, 7)
}

pub fn year_key(t: i64) -> i64 {
    civil_from_days(floor_div(t, DAY)).0
}

/// The period of a civil calendar that holds `t` (Definition 3.2): equal
/// keys are one class of the calendar. Periods of `n` weeks, months or
/// years are counted from the epoch.
pub fn period_key(cal: &Cal, t: i64) -> i64 {
    match cal {
        Cal::Base => t,
        // The period whose end is the ceiling of `t`: bars are labelled at
        // their close.
        Cal::Minutes(n) => floor_div(t.saturating_sub(1), (*n).max(1) as i64 * MIN) + 1,
        Cal::Days => day_key(t),
        Cal::Weeks(n) => floor_div(week_key(t), (*n).max(1) as i64),
        Cal::Months(n) => {
            let (y, m) = month_key(t);
            floor_div(y * 12 + m as i64 - 1, (*n).max(1) as i64)
        }
        Cal::Years(n) => floor_div(year_key(t), (*n).max(1) as i64),
        Cal::Sessions | Cal::SessionsOf(_) => day_key(t),
    }
}

/// The label of the `res` bucket containing `t`: the civil date for @1d,
/// otherwise the bar's close instant (the ceiling to the bar length).
pub fn bucket(res: Resolution, t: i64) -> i64 {
    match res.nanos() {
        None if res == Resolution::W1 => (week_key(t) * 7 - 3 + 6) * DAY,
        None => floor_div(t, DAY) * DAY,
        Some(s) => {
            let r = t.rem_euclid(s);
            if r == 0 {
                t
            } else {
                t - r + s
            }
        }
    }
}

/// Inclusive instant range covered by the bucket with `label`.
pub fn bucket_range(res: Resolution, label: i64) -> (i64, i64) {
    match res.nanos() {
        None if res == Resolution::W1 => (label - 6 * DAY, label + DAY - 1),
        None => (label, label + DAY - 1),
        Some(s) => (label - s + 1, label),
    }
}

pub fn month_key(t: i64) -> (i64, u32) {
    let (y, m, _) = civil_from_days(floor_div(t, DAY));
    (y, m)
}

pub fn day_key(t: i64) -> i64 {
    floor_div(t, DAY)
}

/// Parse `YYYY-MM-DD`, optionally followed by ` HH:MM[:SS[.fffffffff]]` or
/// `THH:MM[:SS[.fffffffff]]` (the data layer's and the query layer's too).
pub use abt_wire::scalar::parse_timestamp;

pub fn format_timestamp(t: i64) -> String {
    let days = floor_div(t, DAY);
    // The time of day without `days * DAY`, which overflows at the ends of
    // the range (an unbounded time is `i64::MIN` or `i64::MAX`).
    let clock = t.rem_euclid(DAY);
    let (y, m, d) = civil_from_days(days);
    if clock == 0 {
        return format!("{:04}-{:02}-{:02}", y, m, d);
    }
    let secs = clock / SEC;
    let nanos = clock % SEC;
    let hms = format!("{:04}-{:02}-{:02}T{:02}:{:02}:{:02}", y, m, d, secs / 3600, (secs % 3600) / 60, secs % 60);
    if nanos == 0 {
        hms
    } else {
        format!("{}.{:09}", hms, nanos)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn civil_round_trip() {
        for z in [-1_000_000i64, -1, 0, 1, 19_000, 20_000, 2_000_000] {
            let (y, m, d) = civil_from_days(z);
            assert_eq!(days_from_civil(y, m, d), z);
        }
        assert_eq!(days_from_civil(1970, 1, 1), 0);
        assert_eq!(days_from_civil(2000, 3, 1), 11_017);
    }

    #[test]
    fn durations_are_calendar() {
        let t = parse_timestamp("2024-03-31").unwrap();
        assert_eq!(format_timestamp(sub_duration(t, Duration::months(1))), "2024-02-29");
        assert_eq!(format_timestamp(sub_duration(t, Duration::months(12))), "2023-03-31");
        assert_eq!(format_timestamp(sub_duration(t, Duration::days(7))), "2024-03-24");
        let m = parse_timestamp("2024-03-04T09:31:00").unwrap();
        assert_eq!(format_timestamp(sub_duration(m, Duration::days(1))), "2024-03-03T09:31:00");
        assert_eq!(format_timestamp(sub_duration(m, Duration::secs(1800))), "2024-03-04T09:01:00");
        assert_eq!(format_timestamp(add_duration(m, Duration::secs(1800))), "2024-03-04T10:01:00");
        assert_eq!(week_key(parse_timestamp("2024-03-04").unwrap()), week_key(parse_timestamp("2024-03-10").unwrap()));
        assert_ne!(week_key(parse_timestamp("2024-03-04").unwrap()), week_key(parse_timestamp("2024-03-11").unwrap()));
    }

    #[test]
    fn buckets() {
        let m = parse_timestamp("2024-03-04T09:31:00").unwrap();
        assert_eq!(format_timestamp(bucket(Resolution::M5, m)), "2024-03-04T09:35:00");
        assert_eq!(format_timestamp(bucket(Resolution::M5, m + 240)), "2024-03-04T09:35:00");
        assert_eq!(format_timestamp(bucket(Resolution::H1, m)), "2024-03-04T10:00:00");
        assert_eq!(format_timestamp(bucket(Resolution::D1, m)), "2024-03-04");
        let (lo, hi) = bucket_range(Resolution::D1, bucket(Resolution::D1, m));
        assert!(lo <= m && m <= hi);
        assert_eq!(weekday(parse_timestamp("2024-03-04").unwrap()), 0);
        assert_eq!(weekday(parse_timestamp("2024-03-09").unwrap()), 5);
    }
}
