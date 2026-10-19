//! The calendar ladder (ST-5.3; A-D46): the coarser levels of a bar
//! relation precomputed from the nearest finer one, each a bar relation
//! of its own in the block format with its presence (Proposition 5.2,
//! exact ladder). A level's bucket and label follow the rules of ST-5.3:
//! intraday buckets UTC-aligned and labelled at their start; days by trade
//! date; weeks (ISO, Monday to Sunday), months, quarters and years by
//! trade date, labelled at the bucket's last trade date of the calendar;
//! a bucket with no base row has no row; every bar carries the number of
//! base rows it summarises; a bucket's receipt is its end.
//!
//! Each hot relation holds one bar column, so a level of `close` is the
//! bucket's last close, of `open` its first open, of `high` its maximum,
//! of `low` its minimum, of `volume` its sum: the rule follows the
//! relation's name (the format's, not a program's semantics).
//!
//! Trade dates are UTC dates until a session table names them (the
//! futures sessions of iteration 3, ST-5.3's `session` level).

use std::collections::BTreeMap;

use abt_wire::scalar::{civil_from_days, days_from_civil, NANOS_PER_DAY, NANOS_PER_MINUTE};

/// A level of the ladder.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Level {
    /// UTC-aligned buckets of `n` minutes (5, 15, 30, 60).
    Minutes(u32),
    Day,
    Week,
    Month,
    Quarter,
    Year,
}

/// How a relation's values fold into a bucket.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Rule {
    First,
    Last,
    Max,
    Min,
    Sum,
}

impl Rule {
    /// The rule of a bar relation, by its short name.
    pub fn of_relation(relation: &str) -> Option<Rule> {
        match relation.rsplit_once('.').map(|(_, s)| s).unwrap_or(relation) {
            "open" => Some(Rule::First),
            "high" => Some(Rule::Max),
            "low" => Some(Rule::Min),
            "close" => Some(Rule::Last),
            "volume" => Some(Rule::Sum),
            _ => None,
        }
    }

    pub fn fold(self, acc: i64, x: i64) -> i64 {
        match self {
            Rule::First => acc,
            Rule::Last => x,
            Rule::Max => acc.max(x),
            Rule::Min => acc.min(x),
            Rule::Sum => acc + x,
        }
    }
}

impl Level {
    pub fn name(self) -> String {
        match self {
            Level::Minutes(60) => "1h".to_string(),
            Level::Minutes(n) => format!("{}m", n),
            Level::Day => "1d".to_string(),
            Level::Week => "1w".to_string(),
            Level::Month => "1mo".to_string(),
            Level::Quarter => "1q".to_string(),
            Level::Year => "1y".to_string(),
        }
    }

    pub fn parse(name: &str) -> Option<Level> {
        Some(match name {
            "1h" => Level::Minutes(60),
            "1d" => Level::Day,
            "1w" => Level::Week,
            "1mo" => Level::Month,
            "1q" => Level::Quarter,
            "1y" => Level::Year,
            m if m.ends_with('m') => Level::Minutes(m.trim_end_matches('m').parse().ok()?),
            _ => return None,
        })
    }

    /// The levels precomputed over a base calendar (ST-5.3's table): a
    /// day base gives week, month, quarter and year; a minute base gives
    /// 5, 15 and 30 minutes, 1 hour, day, week and month.
    pub fn of_base(calendar: &str) -> Vec<Level> {
        match calendar {
            "1d" => vec![Level::Week, Level::Month, Level::Quarter, Level::Year],
            "1m" => vec![Level::Minutes(5), Level::Minutes(15), Level::Minutes(30), Level::Minutes(60), Level::Day, Level::Week, Level::Month],
            _ => Vec::new(),
        }
    }

    /// The level this one is built from, the finest built level it
    /// coarsens (never the base when one exists); `None` is the base.
    pub fn source(self, base: &str) -> Option<Level> {
        let built = Level::of_base(base);
        let candidate = match self {
            Level::Minutes(5) => None,
            Level::Minutes(15) => Some(Level::Minutes(5)),
            Level::Minutes(30) => Some(Level::Minutes(15)),
            Level::Minutes(60) => Some(Level::Minutes(30)),
            Level::Minutes(_) => None,
            Level::Day => Some(Level::Minutes(60)),
            Level::Week | Level::Month => Some(Level::Day),
            Level::Quarter => Some(Level::Month),
            Level::Year => Some(Level::Quarter),
        };
        candidate.filter(|c| built.contains(c))
    }

    /// The start of the bucket holding `t` (the bucket's key).
    pub fn bucket_start(self, t: i64) -> i64 {
        let days = t.div_euclid(NANOS_PER_DAY);
        match self {
            Level::Minutes(n) => {
                let p = n as i64 * NANOS_PER_MINUTE;
                t.div_euclid(p) * p
            }
            Level::Day => days * NANOS_PER_DAY,
            Level::Week => {
                // 1970-01-01 was a Thursday: Monday is 0.
                let weekday = (days + 3).rem_euclid(7);
                (days - weekday) * NANOS_PER_DAY
            }
            Level::Month => {
                let (y, m, _) = civil_from_days(days);
                days_from_civil(y, m, 1) * NANOS_PER_DAY
            }
            Level::Quarter => {
                let (y, m, _) = civil_from_days(days);
                days_from_civil(y, (m - 1) / 3 * 3 + 1, 1) * NANOS_PER_DAY
            }
            Level::Year => {
                let (y, _, _) = civil_from_days(days);
                days_from_civil(y, 1, 1) * NANOS_PER_DAY
            }
        }
    }

    /// Whether a bucket's label is its start (intraday buckets and days)
    /// or the calendar's last base label in it (weeks and coarser).
    pub fn labelled_at_start(self) -> bool {
        matches!(self, Level::Minutes(_) | Level::Day)
    }

    /// The level's labels over a base's labels: per bucket with a base
    /// label, `(bucket start, label)`, in order.
    pub fn labels(self, base: &[i64]) -> Vec<(i64, i64)> {
        let mut out: Vec<(i64, i64)> = Vec::new();
        for &t in base {
            let key = self.bucket_start(t);
            let label = if self.labelled_at_start() { key } else { t };
            match out.last_mut() {
                Some(last) if last.0 == key => last.1 = last.1.max(label),
                _ => out.push((key, label)),
            }
        }
        out
    }
}

/// A source row of a level: its label, value, the base rows it stands
/// for, its receipt, its sequence number, and the label and receipt of
/// its last base row (its own for a base row), from which a bucket's
/// receipt delay is taken.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Source {
    pub label: i64,
    pub value: i64,
    pub count: i64,
    pub r: i64,
    pub seq: u64,
    pub last_label: i64,
    pub last_r: i64,
}

impl Source {
    /// A base row.
    pub fn row(label: i64, value: i64, r: i64, seq: u64) -> Source {
        Source {
            label,
            value,
            count: 1,
            r,
            seq,
            last_label: label,
            last_r: r,
        }
    }
}

/// A row of a level for one instrument: the bucket's key, its value, the
/// base rows it summarises, the latest receipt among them, the label and
/// receipt of its last source row, and the greatest sequence number.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Bar {
    pub key: i64,
    pub value: i64,
    pub count: i64,
    pub max_r: i64,
    pub last_label: i64,
    pub last_r: i64,
    pub seq: u64,
}

impl Bar {
    /// The bar's receipt at `label` (ST-5.3): the bucket's end as the
    /// calendar sees it.
    pub fn receipt(&self, label: i64) -> i64 {
        receipt(label, self.last_label, self.last_r, self.max_r)
    }
}

/// A level's bars of one instrument from its source level's rows in label
/// order (the base's rows count one each): a regrouping by bucket
/// (Proposition 5.2).
pub fn build(level: Level, rule: Rule, source: &[Source]) -> Vec<Bar> {
    let mut out: Vec<Bar> = Vec::new();
    for x in source {
        let key = level.bucket_start(x.label);
        match out.last_mut() {
            Some(bar) if bar.key == key => {
                bar.value = rule.fold(bar.value, x.value);
                bar.count += x.count;
                bar.max_r = bar.max_r.max(x.r);
                bar.last_label = x.last_label;
                bar.last_r = x.last_r;
                bar.seq = bar.seq.max(x.seq);
            }
            _ => out.push(Bar {
                key,
                value: x.value,
                count: x.count,
                max_r: x.r,
                last_label: x.last_label,
                last_r: x.last_r,
                seq: x.seq,
            }),
        }
    }
    out
}

/// A bucket's receipt (ST-5.3): its end as the calendar sees it, the
/// bucket's label plus the receipt delay its last row carried, or the
/// latest receipt among its rows when that is later.
pub fn receipt(label: i64, last_row_label: i64, last_row_r: i64, max_r: i64) -> i64 {
    (label + (last_row_r - last_row_label)).max(max_r)
}

/// Per bucket key, the index of its label in the level's label list.
pub fn index_of(labels: &[(i64, i64)]) -> BTreeMap<i64, u32> {
    labels.iter().enumerate().map(|(i, (k, _))| (*k, i as u32)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use abt_wire::scalar::NANOS_PER_HOUR;

    fn date(y: i64, m: u32, d: u32) -> i64 {
        days_from_civil(y, m, d) * NANOS_PER_DAY
    }

    #[test]
    fn buckets_follow_the_rules() {
        // 2024-01-03 is a Wednesday: its ISO week starts Monday 2024-01-01.
        assert_eq!(Level::Week.bucket_start(date(2024, 1, 3) + 5 * NANOS_PER_HOUR), date(2024, 1, 1));
        // A Sunday belongs to the week of the preceding Monday.
        assert_eq!(Level::Week.bucket_start(date(2024, 1, 7)), date(2024, 1, 1));
        assert_eq!(Level::Week.bucket_start(date(2024, 1, 8)), date(2024, 1, 8));
        assert_eq!(Level::Month.bucket_start(date(2024, 2, 29)), date(2024, 2, 1));
        assert_eq!(Level::Quarter.bucket_start(date(2024, 5, 17)), date(2024, 4, 1));
        assert_eq!(Level::Year.bucket_start(date(2024, 12, 31)), date(2024, 1, 1));
        assert_eq!(Level::Day.bucket_start(date(2024, 3, 10) + 23 * NANOS_PER_HOUR), date(2024, 3, 10));
        let t = date(2024, 3, 10) + 9 * NANOS_PER_HOUR + 47 * NANOS_PER_MINUTE;
        assert_eq!(Level::Minutes(5).bucket_start(t), date(2024, 3, 10) + 9 * NANOS_PER_HOUR + 45 * NANOS_PER_MINUTE);
        assert_eq!(Level::Minutes(15).bucket_start(t), date(2024, 3, 10) + 9 * NANOS_PER_HOUR + 45 * NANOS_PER_MINUTE);
        assert_eq!(Level::Minutes(30).bucket_start(t), date(2024, 3, 10) + 9 * NANOS_PER_HOUR + 30 * NANOS_PER_MINUTE);
        assert_eq!(Level::Minutes(60).bucket_start(t), date(2024, 3, 10) + 9 * NANOS_PER_HOUR);
        // Before the epoch too.
        assert_eq!(Level::Week.bucket_start(date(1969, 12, 31)), date(1969, 12, 29));
    }

    #[test]
    fn levels_name_parse_and_chain() {
        for l in Level::of_base("1d").into_iter().chain(Level::of_base("1m")) {
            assert_eq!(Level::parse(&l.name()), Some(l));
        }
        assert_eq!(Level::Week.source("1d"), None, "weeks come from the day base");
        assert_eq!(Level::Quarter.source("1d"), Some(Level::Month));
        assert_eq!(Level::Year.source("1d"), Some(Level::Quarter));
        assert_eq!(Level::Minutes(15).source("1m"), Some(Level::Minutes(5)));
        assert_eq!(Level::Day.source("1m"), Some(Level::Minutes(60)));
        assert_eq!(Level::Week.source("1m"), Some(Level::Day));
        assert_eq!(Level::Month.source("1m"), Some(Level::Day));
        assert!(Level::of_base("1h").is_empty());
    }

    #[test]
    fn labels_are_the_last_trade_date_or_the_bucket_start() {
        // A week of trading days Mon to Thu, the Friday a holiday.
        let base: Vec<i64> = [1, 2, 3, 4, 8, 9].iter().map(|&d| date(2024, 1, d)).collect();
        assert_eq!(Level::Week.labels(&base), vec![(date(2024, 1, 1), date(2024, 1, 4)), (date(2024, 1, 8), date(2024, 1, 9))]);
        assert_eq!(Level::Month.labels(&base), vec![(date(2024, 1, 1), date(2024, 1, 9))]);
        let minutes: Vec<i64> = (0..7).map(|i| date(2024, 1, 2) + 14 * NANOS_PER_HOUR + 31 * NANOS_PER_MINUTE + i * NANOS_PER_MINUTE).collect();
        let five = Level::Minutes(5).labels(&minutes);
        assert_eq!(five, vec![(minutes[0] - NANOS_PER_MINUTE, minutes[0] - NANOS_PER_MINUTE), (minutes[4], minutes[4])]);
    }

    #[test]
    fn a_level_is_the_regrouping_of_its_source() {
        let rows: Vec<Source> = (0..10)
            .map(|d| Source::row(date(2024, 1, 1) + d * NANOS_PER_DAY, 100 + d, date(2024, 1, 1) + d * NANOS_PER_DAY + NANOS_PER_HOUR, 10 + d as u64))
            .collect();
        let weeks = build(Level::Week, Rule::Last, &rows);
        assert_eq!(weeks.len(), 2);
        assert_eq!(
            weeks[0],
            Bar {
                key: date(2024, 1, 1),
                value: 106,
                count: 7,
                max_r: rows[6].r,
                last_label: rows[6].label,
                last_r: rows[6].r,
                seq: 16
            }
        );
        assert_eq!((weeks[1].value, weeks[1].count, weeks[1].seq), (109, 3, 19));
        let high = build(Level::Week, Rule::Max, &rows);
        assert_eq!(high[0].value, 106);
        let first = build(Level::Week, Rule::First, &rows);
        assert_eq!((first[0].value, first[1].value), (100, 107));
        let sum = build(Level::Month, Rule::Sum, &rows);
        assert_eq!((sum[0].value, sum[0].count), ((100..110).sum(), 10));
        // Quarters from months regroup exactly.
        let months: Vec<Source> = sum
            .iter()
            .map(|b| Source {
                label: b.last_label,
                value: b.value,
                count: b.count,
                r: b.receipt(b.last_label),
                seq: b.seq,
                last_label: b.last_label,
                last_r: b.last_r,
            })
            .collect();
        let quarters = build(Level::Quarter, Rule::Sum, &months);
        assert_eq!(quarters[0].value, sum[0].value);
        // The bucket's receipt is its end: the label plus the last row's
        // delay, or a later receipt among its rows.
        assert_eq!(weeks[0].receipt(date(2024, 1, 7)), date(2024, 1, 7) + NANOS_PER_HOUR);
        assert_eq!(receipt(date(2024, 1, 5), date(2024, 1, 4), rows[3].r, rows[3].r + 10 * NANOS_PER_DAY), rows[3].r + 10 * NANOS_PER_DAY);
    }
}
