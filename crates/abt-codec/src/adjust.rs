//! Adjusted series over ranges (ST-5.10): views `x_adj(k) = x(k) · F(k)`
//! (split and total-return factors, a ratio-adjusted continuous future)
//! and `x_adj(k) = x(k) + A(k)` (a back-adjusted continuous future), `F`
//! and `A` step functions constant between events (ST-5.6, ST-5.5),
//! evaluated piecewise: a range is cut at the events inside it, every
//! whole piece between two events is read from the event-interval
//! summaries (the raw cumulative moments at each event row, and the raw
//! extremes of each interval with their rows), and only the range's two
//! ends decode their edges. No adjusted copy is stored (A-D43).
//!
//! - Rational factors (splits) and additive adjustments are exact: the sum
//!   over pieces is taken in 256-bit integers over the factors' common
//!   denominator.
//! - The total-return factor is an `f64`; its sum is `Σ_p F_p · f64(S_p)`
//!   over the pieces in time order, the ordered definition every layer
//!   computes alike, so it is deterministic and bit-identical across layers.
//! - Extremes: `F_p > 0` preserves order within a piece, so the range's
//!   extremes are the best of the pieces' adjusted extremes.
//! - Counts below a threshold: each threshold converted to the piece's raw
//!   ticks (`x · num < c · den` in integers for a rational factor; the same
//!   `f64` comparison as the view for total return), summed over pieces.

use abt_agg::exact::{ratio_to_f64, Int, Wide};

use crate::summary::{moments, Column, Cumulative};

/// A factor in force over a piece.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Factor {
    /// `x · num / den`, exact (a split factor): `den > 0`, `num > 0`.
    Ratio { num: u64, den: u64 },
    /// `x · f`, the `f64` product (the total-return factor): `f > 0`.
    Float(f64),
    /// `x + a` in the column's ticks (a back-adjusted continuous future).
    Shift(i64),
}

impl Factor {
    /// The adjusted value of a raw tick, as the view shows it: exact for
    /// a shift (ticks), the `f64` product otherwise (a ratio's `f64` is
    /// the correctly rounded quotient, computed through the same division
    /// as every exact answer).
    pub fn apply_f64(&self, x: i64, scale: u32) -> f64 {
        match *self {
            Factor::Ratio { num, den } => {
                let n = Int::product(x as i128, num as i128);
                let d = Wide::from_u128(den as u128).mul_u128(abt_agg::exact::pow10(scale));
                ratio_to_f64(&n, &d)
            }
            Factor::Float(f) => (x as f64 / 10f64.powi(scale as i32)) * f,
            Factor::Shift(a) => (x as i128 + a as i128) as f64 / 10f64.powi(scale as i32),
        }
    }
}

/// A step function over an instrument's rows: `factors[i]` holds from row
/// `starts[i]` to `starts[i + 1]` (to the end for the last); `starts[0]`
/// is 0 (the factor before the first event is the initial one).
#[derive(Clone, Debug, PartialEq)]
pub struct Steps {
    starts: Vec<u64>,
    factors: Vec<Factor>,
}

impl Steps {
    /// The step function with `initial` from row 0 and one change per
    /// event `(row, factor)`, events ascending by row (a second event at a
    /// row replaces the first).
    pub fn new(initial: Factor, events: impl IntoIterator<Item = (u64, Factor)>) -> Steps {
        let mut s = Steps {
            starts: vec![0],
            factors: vec![initial],
        };
        for (row, f) in events {
            let last = *s.starts.last().unwrap();
            assert!(row >= last, "events ascend");
            if row == last {
                *s.factors.last_mut().unwrap() = f;
            } else {
                s.starts.push(row);
                s.factors.push(f);
            }
        }
        s
    }

    /// The rows at which the factor changes, after the first.
    pub fn events(&self) -> &[u64] {
        &self.starts[1..]
    }

    pub fn factors(&self) -> &[Factor] {
        &self.factors
    }

    /// The piece holding `row` (its index).
    pub fn piece_of(&self, row: u64) -> usize {
        self.starts.partition_point(|&s| s <= row) - 1
    }

    /// The factor in force at `row`.
    pub fn at(&self, row: u64) -> Factor {
        self.factors[self.piece_of(row)]
    }

    /// The pieces of rows `[a, b)`: `(start, end, piece index)`, cut at
    /// the events inside, in row order. O(log E + p) for `p` pieces.
    pub fn pieces(&self, a: u64, b: u64) -> Vec<(u64, u64, usize)> {
        let mut out = Vec::new();
        if a >= b {
            return out;
        }
        let mut p = self.piece_of(a);
        let mut start = a;
        loop {
            let end = self.starts.get(p + 1).copied().unwrap_or(u64::MAX).min(b);
            out.push((start, end, p));
            if end >= b {
                return out;
            }
            start = end;
            p += 1;
        }
    }
}

/// The summaries kept per interval between consecutive events (ST-5.10):
/// the raw cumulative moments at every event row (the totals before it)
/// and each interval's raw extremes with their rows.
#[derive(Clone, Debug, PartialEq)]
pub struct IntervalSummaries {
    /// `at[i]`: the cumulative before row `starts[i]` (`at[0]` is zero);
    /// `at[E + 1]` the cumulative at the end of the column.
    pub at: Vec<Cumulative>,
    /// Per interval `i` (rows `[starts[i], starts[i + 1])`): the minimum
    /// and its row, the maximum and its row; `None` for an empty interval.
    pub extremes: Vec<Option<(i64, u64, i64, u64)>>,
}

impl IntervalSummaries {
    /// Built over a column's rows for the pieces of `steps`.
    pub fn of(col: &dyn Column, rows: u64, steps: &Steps) -> IntervalSummaries {
        let mut at = Vec::with_capacity(steps.starts.len() + 1);
        let mut extremes = Vec::with_capacity(steps.starts.len());
        let bounds: Vec<u64> = steps.starts.iter().copied().chain(std::iter::once(rows)).collect();
        let mut running = Cumulative::ZERO;
        at.push(running.clone());
        for w in bounds.windows(2) {
            let (a, b) = (w[0].min(rows), w[1].min(rows));
            let m = moments(col, a, b);
            running = running.plus_totals(&m);
            at.push(running.clone());
            extremes.push(crate::summary::extremes(col, a, b));
        }
        IntervalSummaries { at, extremes }
    }
}

/// What one piece of an adjusted range reduces to: its raw totals and
/// extremes, and the factor over it.
#[derive(Clone, Debug, PartialEq)]
pub struct Piece {
    pub start: u64,
    pub end: u64,
    pub raw: Cumulative,
    /// `(min, its row, max, its row)` of the raw values.
    pub extremes: Option<(i64, u64, i64, u64)>,
    pub factor: Factor,
}

/// The pieces of rows `[a, b)` of an adjusted view: the whole intervals
/// inside from the summaries (two lookups each, no decoding), the two
/// partial ends from the column's range rules (which decode only their
/// edge blocks).
pub fn pieces(col: &dyn Column, steps: &Steps, summaries: &IntervalSummaries, a: u64, b: u64) -> Vec<Piece> {
    steps
        .pieces(a, b)
        .into_iter()
        .map(|(start, end, p)| {
            let whole = start == steps.starts[p] && end == steps.starts.get(p + 1).copied().unwrap_or(end);
            let whole = whole && summaries.at.len() > p + 1 && summaries.at[p + 1].count - summaries.at[p].count == end - start;
            let (raw, extremes) = if whole {
                (summaries.at[p + 1].minus(&summaries.at[p]), summaries.extremes[p])
            } else {
                (moments(col, start, end), crate::summary::extremes(col, start, end))
            };
            Piece {
                start,
                end,
                raw,
                extremes,
                factor: steps.factors[p],
            }
        })
        .collect()
}

/// An exact rational `num / den`, the sum of an adjusted range with
/// rational or additive factors.
#[derive(Clone, Debug, PartialEq)]
pub struct Exact {
    pub num: Int,
    pub den: Wide,
}

impl Exact {
    /// The correctly rounded `f64` of `num / (den · 10^scale)`.
    pub fn to_f64(&self, scale: u32) -> f64 {
        ratio_to_f64(&self.num, &self.den.mul_u128(abt_agg::exact::pow10(scale)))
    }
}

/// The sum of an adjusted range, by the kind of its factors (every piece
/// of one view has the same kind). `Exact` for rational and additive
/// factors; the ordered `f64` definition for `Float`.
#[derive(Clone, Debug, PartialEq)]
pub enum Sum {
    Exact(Exact),
    Float(f64),
}

impl Sum {
    pub fn to_f64(&self, scale: u32) -> f64 {
        match self {
            Sum::Exact(e) => e.to_f64(scale),
            Sum::Float(f) => *f,
        }
    }
}

/// The sum of the pieces' adjusted values (ST-5.10's first row).
pub fn sum(pieces: &[Piece], scale: u32) -> Sum {
    if pieces.iter().any(|p| matches!(p.factor, Factor::Float(_))) {
        // The ordered definition: Σ_p F_p · f64(S_p), in time order.
        let mut total = 0.0f64;
        for p in pieces {
            let f = match p.factor {
                Factor::Float(f) => f,
                _ => panic!("one kind of factor per view"),
            };
            total += f * (p.raw.sum as f64 / 10f64.powi(scale as i32));
        }
        return Sum::Float(total);
    }
    // Rational and additive: over the common denominator, in 256-bit.
    let mut den: u128 = 1;
    for p in pieces {
        if let Factor::Ratio { den: d, .. } = p.factor {
            den = lcm(den, d as u128);
        }
    }
    let mut num = Int::ZERO;
    for p in pieces {
        let term = match p.factor {
            Factor::Ratio { num: n, den: d } => Int::from_i128(p.raw.sum).mul_u128(n as u128).mul_u128(den / d as u128),
            Factor::Shift(a) => Int::from_i128(p.raw.sum + p.raw.count as i128 * a as i128).mul_u128(den),
            Factor::Float(_) => unreachable!(),
        };
        num = num.add(&term);
    }
    Sum::Exact(Exact { num, den: Wide::from_u128(den) })
}

/// The number of rows of the pieces.
pub fn count(pieces: &[Piece]) -> u64 {
    pieces.iter().map(|p| p.raw.count).sum()
}

/// The adjusted extremes of the pieces: `(min, its row, max, its row)` as
/// `f64` views (the first row on a tie, as the raw extremes), from each
/// piece's raw extremes (`F_p > 0` preserves order within a piece).
pub fn extremes(pieces: &[Piece], scale: u32) -> Option<(f64, u64, f64, u64)> {
    let mut best: Option<(f64, u64, f64, u64)> = None;
    for p in pieces {
        let Some((mn, rmn, mx, rmx)) = p.extremes else { continue };
        let (amn, amx) = (p.factor.apply_f64(mn, scale), p.factor.apply_f64(mx, scale));
        best = Some(match best {
            None => (amn, rmn, amx, rmx),
            Some((bmn, brmn, bmx, brmx)) => (
                if amn < bmn { amn } else { bmn },
                if amn < bmn { rmn } else { brmn },
                if amx > bmx { amx } else { bmx },
                if amx > bmx { rmx } else { brmx },
            ),
        });
    }
    best
}

/// A threshold on the adjusted view, as a rational `num / den` in the
/// column's ticks (so `c = num / (den · 10^scale)`) for rational and
/// additive views, or as the `f64` the view compares against for `Float`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Threshold {
    Ticks { num: i128, den: u128 },
    Float(f64),
}

/// Whether raw tick `x` under `factor` is strictly below `c`, by the
/// view's own comparison: integers for rational and additive factors, the
/// `f64` product for total return.
pub fn below(factor: Factor, x: i64, c: Threshold, scale: u32) -> bool {
    match (factor, c) {
        (Factor::Ratio { num, den }, Threshold::Ticks { num: cn, den: cd }) => {
            // x · num / den < cn / cd  ⇔  x · num · cd < cn · den
            Int::product(x as i128 * num as i128, cd as i128).sub(&Int::product(cn, den as i128)).negative
        }
        (Factor::Shift(a), Threshold::Ticks { num: cn, den: cd }) => {
            // x + a < cn / cd  ⇔  (x + a) · cd < cn
            Int::product(x as i128 + a as i128, cd as i128).sub(&Int::from_i128(cn)).negative
        }
        (f, Threshold::Float(c)) => f.apply_f64(x, scale) < c,
        (Factor::Float(_), Threshold::Ticks { num, den }) => factor.apply_f64(x, scale) < ratio_to_f64(&Int::from_i128(num), &Wide::from_u128(den).mul_u128(abt_agg::exact::pow10(scale))),
    }
}

/// A count of the raw values of a piece satisfying a monotone predicate.
pub type CountRaw<'a> = dyn FnMut(&Piece, &dyn Fn(i64) -> bool) -> u64 + 'a;

/// The rows of the pieces whose adjusted value is below `c`, counting each
/// piece's raw values below the threshold converted to its own ticks:
/// `count_raw(piece, threshold_p)` summed over pieces, with `count_raw`
/// given by the caller (a wavelet matrix's count, or a scan of the tail).
pub fn count_below(pieces: &[Piece], c: Threshold, scale: u32, count_raw: &mut CountRaw<'_>) -> u64 {
    pieces.iter().map(|p| count_raw(p, &|x| below(p.factor, x, c, scale))).sum()
}

fn gcd(mut a: u128, mut b: u128) -> u128 {
    while b != 0 {
        let t = a % b;
        a = b;
        b = t;
    }
    a
}

fn lcm(a: u128, b: u128) -> u128 {
    if a == 0 || b == 0 {
        return 0;
    }
    a / gcd(a, b) * b
}
