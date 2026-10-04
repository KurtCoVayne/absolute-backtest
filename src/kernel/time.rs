//! Calendar arithmetic over Unix timestamps (seconds, naive wall clock):
//! civil dates, calendar-duration subtraction and resolution buckets.

use crate::ir::{Duration, Resolution};

pub const DAY: i64 = 86_400;

/// Days since 1970-01-01 for a proleptic Gregorian civil date.
pub fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (m as i64 + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// Civil date for a number of days since 1970-01-01.
pub fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

pub fn days_in_month(y: i64, m: u32) -> u32 {
    match m {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        _ => {
            if (y % 4 == 0 && y % 100 != 0) || y % 400 == 0 {
                29
            } else {
                28
            }
        }
    }
}

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
/// the target month), then days subtract whole days.
pub fn sub_duration(t: i64, d: Duration) -> i64 {
    let mut t = t;
    if d.months != 0 {
        let days = floor_div(t, DAY);
        let secs = t - days * DAY;
        let (y, m, dd) = civil_from_days(days);
        let total = y * 12 + (m as i64 - 1) - d.months;
        let ny = floor_div(total, 12);
        let nm = (total - ny * 12) as u32 + 1;
        let nd = dd.min(days_in_month(ny, nm));
        t = days_from_civil(ny, nm, nd) * DAY + secs;
    }
    t - d.days * DAY
}

/// The label of the `res` bucket containing `t`: the civil date for @1d,
/// otherwise the bar's close instant (the ceiling to the bar length).
pub fn bucket(res: Resolution, t: i64) -> i64 {
    match res.seconds() {
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
    match res.seconds() {
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

/// Parse `YYYY-MM-DD`, optionally followed by ` HH:MM[:SS]` or `THH:MM[:SS]`.
pub fn parse_timestamp(s: &str) -> Option<i64> {
    let s = s.trim();
    let (date, time) = if s.len() > 10 { (&s[..10], Some(s[11..].trim())) } else { (s, None) };
    let mut it = date.split('-');
    let y: i64 = it.next()?.parse().ok()?;
    let m: u32 = it.next()?.parse().ok()?;
    let d: u32 = it.next()?.parse().ok()?;
    if it.next().is_some() || !(1..=12).contains(&m) || d == 0 || d > days_in_month(y, m) {
        return None;
    }
    let mut t = days_from_civil(y, m, d) * DAY;
    if let Some(time) = time {
        let time = time.trim_end_matches('Z');
        let mut parts = time.split(':');
        let h: i64 = parts.next()?.parse().ok()?;
        let mi: i64 = parts.next()?.parse().ok()?;
        let sec: i64 = parts.next().map(|x| x.parse().ok()).unwrap_or(Some(0))?;
        if h > 23 || mi > 59 || sec > 59 {
            return None;
        }
        t += h * 3600 + mi * 60 + sec;
    }
    Some(t)
}

pub fn format_timestamp(t: i64) -> String {
    let days = floor_div(t, DAY);
    let secs = t - days * DAY;
    let (y, m, d) = civil_from_days(days);
    if secs == 0 {
        format!("{:04}-{:02}-{:02}", y, m, d)
    } else {
        format!("{:04}-{:02}-{:02}T{:02}:{:02}:{:02}", y, m, d, secs / 3600, (secs % 3600) / 60, secs % 60)
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
        assert_eq!(format_timestamp(sub_duration(t, Duration { months: 1, days: 0 })), "2024-02-29");
        assert_eq!(format_timestamp(sub_duration(t, Duration { months: 12, days: 0 })), "2023-03-31");
        assert_eq!(format_timestamp(sub_duration(t, Duration { months: 0, days: 7 })), "2024-03-24");
        let m = parse_timestamp("2024-03-04T09:31:00").unwrap();
        assert_eq!(format_timestamp(sub_duration(m, Duration { months: 0, days: 1 })), "2024-03-03T09:31:00");
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
