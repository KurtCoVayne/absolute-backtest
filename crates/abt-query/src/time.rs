//! The civil arithmetic the metrics read: the kernel's own, through the
//! calendar abt-wire shares (A-D2), so that a day or a month of a run is
//! the same day or month in both layers.

pub use abt_wire::scalar::{civil_from_days, parse_timestamp};
use abt_wire::scalar::{NANOS_PER_DAY, NANOS_PER_HOUR, NANOS_PER_SECOND};

/// A day, in the nanoseconds times are counted in (A-D7).
pub const DAY: i64 = NANOS_PER_DAY;
const SECOND: i64 = NANOS_PER_SECOND;
const MINUTE: i64 = 60 * SECOND;
const HOUR: i64 = NANOS_PER_HOUR;

/// The day of `t`, as days since 1970-01-01.
pub fn day_key(t: i64) -> i64 {
    t.div_euclid(DAY)
}

/// The civil year and month of `t`.
pub fn month_key(t: i64) -> (i64, u32) {
    let (y, m, _) = civil_from_days(day_key(t));
    (y, m)
}

/// `t` as the kernel prints a time: the date alone at midnight, else the
/// date and the clock, with nanoseconds when there are any.
pub fn format_timestamp(t: i64) -> String {
    let days = day_key(t);
    let clock = t - days * DAY;
    let (y, m, d) = civil_from_days(days);
    if clock == 0 {
        return format!("{:04}-{:02}-{:02}", y, m, d);
    }
    let nanos = clock % SECOND;
    let hms = format!("{:04}-{:02}-{:02}T{:02}:{:02}:{:02}", y, m, d, clock / HOUR, (clock % HOUR) / MINUTE, (clock % MINUTE) / SECOND);
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
    fn times_print_as_the_kernel_prints_them() {
        let day = abt_wire::scalar::days_from_civil(2024, 3, 4) * DAY;
        assert_eq!(format_timestamp(day), "2024-03-04");
        assert_eq!(format_timestamp(day + 9 * HOUR + 31 * MINUTE + 5 * SECOND), "2024-03-04T09:31:05");
        assert_eq!(format_timestamp(day + 7), "2024-03-04T00:00:00.000000007");
        assert_eq!(format_timestamp(-DAY), "1969-12-31");
        assert_eq!(month_key(day), (2024, 3));
    }
}
