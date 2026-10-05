//! Validation schemes (data-bundle doc, section 6): the hold-out embargo,
//! walk-forward folds, the parameter surface around a grid's best point,
//! and sub-period stability. Pure over timestamps, metrics and the grid's
//! shape; `mod.rs` runs the kernel and logs.

use crate::ir::{Duration, Lit};
use crate::kernel::time;

/// Bars a study may not report on until revealed.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Embargo {
    /// Half-open `[from, to)` intervals of timestamps.
    pub intervals: Vec<(i64, i64)>,
    /// A trailing embargo truncates the data a run sees; a block embargo
    /// withholds the metrics over the blocks (the run crosses them, since
    /// the book's state must).
    pub truncates: bool,
}

impl Embargo {
    pub fn none() -> Embargo {
        Embargo { intervals: vec![], truncates: false }
    }

    pub fn is_empty(&self) -> bool {
        self.intervals.is_empty()
    }

    pub fn contains(&self, t: i64) -> bool {
        self.intervals.iter().any(|(a, b)| *a <= t && t < *b)
    }

    /// The first embargoed timestamp (a trailing embargo's start).
    pub fn start(&self) -> Option<i64> {
        self.intervals.iter().map(|(a, _)| *a).min()
    }

    pub fn describe(&self) -> String {
        self.intervals
            .iter()
            .map(|(a, b)| {
                if *b == i64::MAX {
                    format!("from {}", time::format_timestamp(*a))
                } else {
                    format!("{}..{}", time::format_timestamp(*a), time::format_timestamp(*b))
                }
            })
            .collect::<Vec<_>>()
            .join(", ")
    }
}

fn add_months(t: i64, months: i64) -> i64 {
    time::sub_duration(t, Duration { months: -months, days: 0 })
}

/// The start of the month containing `t`.
fn month_start(t: i64) -> i64 {
    let (y, m) = time::month_key(t);
    time::parse_timestamp(&format!("{:04}-{:02}-01", y, m)).unwrap_or(t)
}

/// A trailing embargo: the last `years` of a sample that ends at `last`.
pub fn trailing_embargo(last: i64, years: u32) -> Embargo {
    Embargo {
        intervals: vec![(add_months(last, -12 * years as i64), i64::MAX)],
        truncates: true,
    }
}

/// `count` blocks of `months` whole months each, chosen by `seed` among the
/// month-aligned starts the sample `[first, last]` admits, without overlap.
pub fn block_embargo(first: i64, last: i64, count: u32, months: u32, seed: u64) -> Embargo {
    let mut starts = Vec::new();
    let mut s = month_start(first);
    while add_months(s, months as i64) <= last {
        starts.push(s);
        s = add_months(s, 1);
    }
    let mut rng = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
    let mut next = move || {
        rng ^= rng >> 12;
        rng ^= rng << 25;
        rng ^= rng >> 27;
        rng.wrapping_mul(0x2545_F491_4F6C_DD1D)
    };
    let mut chosen: Vec<(i64, i64)> = Vec::new();
    let mut attempts = 0;
    while chosen.len() < count as usize && !starts.is_empty() && attempts < 10_000 {
        attempts += 1;
        let a = starts[(next() % starts.len() as u64) as usize];
        let b = add_months(a, months as i64);
        if chosen.iter().any(|(x, y)| a < *y && *x < b) {
            continue;
        }
        chosen.push((a, b));
    }
    chosen.sort();
    Embargo { intervals: chosen, truncates: false }
}

/// A walk-forward scheme: `anchored:TRAIN:TEST` or `rolling:TRAIN:TEST`
/// with durations in months or years.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct WalkForward {
    pub anchored: bool,
    pub train_months: u32,
    pub test_months: u32,
}

fn parse_months(s: &str) -> Result<u32, String> {
    let s = s.trim();
    if let Some(y) = s.strip_suffix('y') {
        return y.parse::<u32>().map(|y| 12 * y).map_err(|_| format!("`{}` is not a number of years", s));
    }
    let m = s.strip_suffix("mo").unwrap_or(s);
    m.parse::<u32>().map_err(|_| format!("`{}` is not a number of months (6mo) or years (2y)", s))
}

impl WalkForward {
    pub fn parse(s: &str) -> Result<WalkForward, String> {
        let parts: Vec<&str> = s.split(':').collect();
        if parts.len() != 3 {
            return Err(format!("`{}` is not a walk-forward scheme (anchored:2y:6mo or rolling:2y:6mo)", s));
        }
        let anchored = match parts[0].trim() {
            "anchored" => true,
            "rolling" => false,
            other => return Err(format!("`{}` is neither anchored nor rolling", other)),
        };
        let train_months = parse_months(parts[1])?;
        let test_months = parse_months(parts[2])?;
        if train_months == 0 || test_months == 0 {
            return Err("a walk-forward window needs at least one month".into());
        }
        Ok(WalkForward { anchored, train_months, test_months })
    }

    pub fn describe(&self) -> String {
        format!("{}:{}mo:{}mo", if self.anchored { "anchored" } else { "rolling" }, self.train_months, self.test_months)
    }

    /// The folds over a sample `[first, last]`: `(train_from, train_to,
    /// test_from, test_to)` half-open, every test window that starts inside
    /// the sample (the last one may end after it).
    pub fn folds(&self, first: i64, last: i64) -> Vec<(i64, i64, i64, i64)> {
        let mut out = Vec::new();
        let start = month_start(first);
        let mut k = 0i64;
        loop {
            let test_from = add_months(start, self.train_months as i64 + k * self.test_months as i64);
            if test_from > last {
                break;
            }
            let test_to = add_months(test_from, self.test_months as i64);
            let train_from = if self.anchored { start } else { add_months(test_from, -(self.train_months as i64)) };
            out.push((train_from, test_from, test_from, test_to));
            k += 1;
        }
        out
    }
}

/// The parameter surface around a grid's best point.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Surface {
    /// Mean absolute difference of the objective between neighbouring grid
    /// points, over the objective's range (0 flat, 1 as jumpy as the range).
    pub smoothness: f64,
    /// The share of the best point's neighbours within `tolerance` of it.
    pub stability: f64,
    pub neighbours: usize,
    pub tolerance: f64,
}

/// Grid points are neighbours when they differ in one axis by one step.
fn are_neighbours(a: &[usize], b: &[usize]) -> bool {
    let mut diff = 0;
    for (x, y) in a.iter().zip(b) {
        let d = x.abs_diff(*y);
        if d > 1 {
            return false;
        }
        diff += d;
    }
    diff == 1
}

/// The index vector of every point of a grid with the given axis lengths,
/// in the order `grid_points` produces them (axis-major).
pub fn grid_indices(axis_lens: &[usize]) -> Vec<Vec<usize>> {
    let mut out: Vec<Vec<usize>> = vec![vec![]];
    for &n in axis_lens {
        let mut next = Vec::new();
        for p in &out {
            for i in 0..n {
                let mut q = p.clone();
                q.push(i);
                next.push(q);
            }
        }
        out = next;
    }
    out
}

/// The surface of `values` (one per grid point, axis-major) around `best`,
/// with a relative tolerance of 10 % (absolute 0.1 when the best is near
/// zero). `None` without neighbours (a single point or one axis of one).
pub fn surface(axes: &[(String, Vec<Lit>)], values: &[f64], best: usize) -> Option<Surface> {
    let lens: Vec<usize> = axes.iter().map(|(_, v)| v.len()).collect();
    let idx = grid_indices(&lens);
    if idx.len() != values.len() || values.len() < 2 {
        return None;
    }
    let finite: Vec<f64> = values.iter().copied().filter(|v| v.is_finite()).collect();
    let range = finite.iter().cloned().fold(f64::NEG_INFINITY, f64::max) - finite.iter().cloned().fold(f64::INFINITY, f64::min);
    let mut diffs = Vec::new();
    let mut near = 0usize;
    let mut neighbours = 0usize;
    let b = values[best];
    let tolerance = if b.abs() < 1.0 { 0.1 } else { 0.1 * b.abs() };
    for i in 0..values.len() {
        for j in (i + 1)..values.len() {
            if are_neighbours(&idx[i], &idx[j]) && values[i].is_finite() && values[j].is_finite() {
                diffs.push((values[i] - values[j]).abs());
                if i == best || j == best {
                    neighbours += 1;
                    let other = if i == best { values[j] } else { values[i] };
                    if (other - b).abs() <= tolerance {
                        near += 1;
                    }
                }
            }
        }
    }
    if diffs.is_empty() || neighbours == 0 {
        return None;
    }
    let smoothness = if range > 0.0 { diffs.iter().sum::<f64>() / diffs.len() as f64 / range } else { 0.0 };
    Some(Surface {
        smoothness,
        stability: near as f64 / neighbours as f64,
        neighbours,
        tolerance,
    })
}

/// The annualised Sharpe ratio of each calendar year of a curve with at
/// least `min_bars` return observations in it.
pub fn subperiod_sharpes(curve: &[(i64, f64)], periods_per_year: f64, min_bars: usize) -> Vec<(i64, f64)> {
    let mut years: Vec<(i64, Vec<(i64, f64)>)> = Vec::new();
    for &(t, e) in curve {
        let y = time::month_key(t).0;
        match years.last_mut() {
            Some((ly, v)) if *ly == y => v.push((t, e)),
            _ => years.push((y, vec![(t, e)])),
        }
    }
    let mut out = Vec::new();
    for (i, (y, points)) in years.iter().enumerate() {
        // The year's first return starts from the previous year's last bar.
        let mut c = Vec::new();
        if i > 0 {
            if let Some(p) = years[i - 1].1.last() {
                c.push(*p);
            }
        }
        c.extend(points.iter().copied());
        if c.len() < min_bars + 1 {
            continue;
        }
        let m = super::metrics::return_metrics(&c, periods_per_year);
        out.push((*y, m.sharpe));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn neighbours_differ_in_one_axis_by_one() {
        assert!(are_neighbours(&[0, 1], &[0, 2]));
        assert!(!are_neighbours(&[0, 1], &[1, 2]));
        assert!(!are_neighbours(&[0, 0], &[0, 2]));
        assert_eq!(grid_indices(&[2, 3]).len(), 6);
        assert_eq!(grid_indices(&[2, 3])[4], vec![1, 1]);
    }
}
