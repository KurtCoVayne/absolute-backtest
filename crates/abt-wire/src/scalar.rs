//! The scalar types of every relation (D-2.1): an instrument id, a time in
//! nanoseconds since the Unix epoch (A-D7), a label code, a sequence number.
//! `Num` is `f64` and `Count` is `i64`; their dimensions are schema metadata,
//! never a runtime tag.

use std::fmt;

use serde::{Deserialize, Serialize};

/// An instrument id, assigned by the data layer's registry and never reused
/// (D-2.4).
#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Entity(pub u64);

/// A code into the registry's label dictionary (D-2.5).
#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Label(pub u32);

/// A position in a stream, monotone and gap-free per stream (A-D10, D-5.1).
#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Seq(pub u64);

/// Nanoseconds since the Unix epoch, UTC (A-D7). The range of `i64` is the
/// years 1678 to 2262.
#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Time(pub i64);

pub const NANOS_PER_SECOND: i64 = 1_000_000_000;
pub const NANOS_PER_MINUTE: i64 = 60 * NANOS_PER_SECOND;
pub const NANOS_PER_HOUR: i64 = 3_600 * NANOS_PER_SECOND;
pub const NANOS_PER_DAY: i64 = 86_400 * NANOS_PER_SECOND;

impl Time {
    pub fn from_secs(s: i64) -> Time {
        Time(s * NANOS_PER_SECOND)
    }

    /// The ISO-8601 rendering with nanoseconds that `--json` prints (P-6):
    /// `2024-01-08T10:30:00.000000000Z`.
    pub fn iso(self) -> String {
        let days = self.0.div_euclid(NANOS_PER_DAY);
        let rem = self.0.rem_euclid(NANOS_PER_DAY);
        let (y, m, d) = civil_from_days(days);
        let secs = rem / NANOS_PER_SECOND;
        let nanos = rem % NANOS_PER_SECOND;
        format!("{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.{:09}Z", y, m, d, secs / 3600, (secs % 3600) / 60, secs % 60, nanos)
    }

    /// Parses `YYYY-MM-DD`, `YYYY-MM-DDTHH:MM[:SS[.fffffffff]]` with an
    /// optional `Z`; the inverse of [`Time::iso`] and of the date forms the
    /// command line takes.
    pub fn parse_iso(s: &str) -> Option<Time> {
        let s = s.strip_suffix('Z').unwrap_or(s);
        let (date, clock) = match s.split_once(['T', ' ']) {
            Some((d, c)) => (d, Some(c)),
            None => (s, None),
        };
        let mut parts = date.splitn(3, '-');
        let y: i64 = parts.next()?.parse().ok()?;
        let m: u32 = parts.next()?.parse().ok()?;
        let d: u32 = parts.next()?.parse().ok()?;
        if !(1..=12).contains(&m) || d == 0 || d > days_in_month(y, m) {
            return None;
        }
        let mut t = days_from_civil(y, m, d) * NANOS_PER_DAY;
        if let Some(c) = clock {
            let (hms, frac) = match c.split_once('.') {
                Some((a, f)) => (a, Some(f)),
                None => (c, None),
            };
            let mut it = hms.split(':');
            let h: i64 = it.next()?.parse().ok()?;
            let mi: i64 = it.next()?.parse().ok()?;
            let sec: i64 = it.next().map(|x| x.parse().ok()).unwrap_or(Some(0))?;
            if it.next().is_some() || h > 23 || mi > 59 || sec > 59 {
                return None;
            }
            t += (h * 3600 + mi * 60 + sec) * NANOS_PER_SECOND;
            if let Some(f) = frac {
                if f.is_empty() || f.len() > 9 || !f.bytes().all(|b| b.is_ascii_digit()) {
                    return None;
                }
                let scale = 10i64.pow(9 - f.len() as u32);
                t += f.parse::<i64>().ok()? * scale;
            }
        }
        Some(Time(t))
    }
}

impl fmt::Display for Time {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.iso())
    }
}

/// Days since 1970-01-01 of a proleptic Gregorian date.
pub fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (m as i64 + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// The proleptic Gregorian date of a day count since 1970-01-01.
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
        _ if (y % 4 == 0 && y % 100 != 0) || y % 400 == 0 => 29,
        _ => 28,
    }
}

/// `t` moved by `months` civil months, the day clamped to the month's last
/// and the clock kept: what a calendar duration's month part means, for the
/// kernel's windows and the data layer's alike (one implementation, A-D2).
pub fn add_months(t: i64, months: i64) -> i64 {
    if months == 0 {
        return t;
    }
    let days = t.div_euclid(NANOS_PER_DAY);
    let clock = t - days * NANOS_PER_DAY;
    let (y, m, d) = civil_from_days(days);
    let total = y * 12 + (m as i64 - 1) + months;
    let ny = total.div_euclid(12);
    let nm = (total - ny * 12) as u32 + 1;
    days_from_civil(ny, nm, d.min(days_in_month(ny, nm))) * NANOS_PER_DAY + clock
}

/// `YYYY-MM-DD`, optionally followed by ` HH:MM[:SS[.fffffffff]]` or
/// `THH:MM[:SS[.fffffffff]]`, as nanoseconds since the epoch; what every
/// layer reads a written time with (A-D2).
pub fn parse_timestamp(s: &str) -> Option<i64> {
    let s = s.trim();
    let (date, time) = if s.len() > 10 { (s.get(..10)?, Some(s.get(11..)?.trim())) } else { (s, None) };
    let mut it = date.split('-');
    let y: i64 = it.next()?.parse().ok()?;
    let m: u32 = it.next()?.parse().ok()?;
    let d: u32 = it.next()?.parse().ok()?;
    if it.next().is_some() || !(1..=12).contains(&m) || d == 0 || d > days_in_month(y, m) {
        return None;
    }
    let mut t = days_from_civil(y, m, d) * NANOS_PER_DAY;
    if let Some(time) = time {
        let time = time.trim_end_matches('Z');
        let mut parts = time.split(':');
        let h: i64 = parts.next()?.parse().ok()?;
        let mi: i64 = parts.next()?.parse().ok()?;
        let (sec, frac) = match parts.next() {
            Some(x) => match x.split_once('.') {
                Some((s, f)) => (s.parse::<i64>().ok()?, Some(f)),
                None => (x.parse::<i64>().ok()?, None),
            },
            None => (0, None),
        };
        if h > 23 || mi > 59 || sec > 59 {
            return None;
        }
        t += h * NANOS_PER_HOUR + mi * NANOS_PER_MINUTE + sec * NANOS_PER_SECOND;
        if let Some(f) = frac {
            if f.is_empty() || f.len() > 9 || !f.bytes().all(|b| b.is_ascii_digit()) {
                return None;
            }
            t += f.parse::<i64>().ok()? * 10i64.pow(9 - f.len() as u32);
        }
    }
    Some(t)
}

/// `t − δ` for a calendar duration of `months`, `days` and `nanos`: months
/// first (civil, the day clamped), then the fixed parts.
pub fn sub_duration(t: i64, months: i64, days: i64, nanos: i64) -> i64 {
    add_months(t, -months) - days * NANOS_PER_DAY - nanos
}

/// `t + δ`, the forward form of [`sub_duration`].
pub fn add_duration(t: i64, months: i64, days: i64, nanos: i64) -> i64 {
    add_months(t, months) + days * NANOS_PER_DAY + nanos
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn iso_round_trips_with_nanoseconds() {
        for t in [0i64, 1, -1, 1_704_706_200 * NANOS_PER_SECOND + 123_456_789, -86_400 * NANOS_PER_SECOND * 365 * 250] {
            let s = Time(t).iso();
            assert_eq!(Time::parse_iso(&s), Some(Time(t)), "{}", s);
        }
        assert_eq!(Time::from_secs(1_704_706_200).iso(), "2024-01-08T09:30:00.000000000Z");
        assert_eq!(Time::parse_iso("2024-01-08"), Some(Time::from_secs(1_704_672_000)));
        assert_eq!(Time::parse_iso("2024-01-08T09:30"), Some(Time::from_secs(1_704_706_200)));
        assert_eq!(Time::parse_iso("2024-02-30"), None);
        assert_eq!(Time::parse_iso("2024-01-08T09:30:00.5"), Some(Time(1_704_706_200 * NANOS_PER_SECOND + 500_000_000)));
    }
}
