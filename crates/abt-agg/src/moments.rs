//! The moments: a compensated sum, the shifted running sums of a window,
//! and the two-pass formulas.

use crate::function::{AggError, Function};

/// A compensated (Neumaier) sum.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Neumaier {
    pub sum: f64,
    pub c: f64,
}

impl Neumaier {
    pub fn add(&mut self, x: f64) {
        let t = self.sum + x;
        if self.sum.abs() >= x.abs() {
            self.c += (self.sum - t) + x;
        } else {
            self.c += (x - t) + self.sum;
        }
        self.sum = t;
    }

    pub fn value(&self) -> f64 {
        self.sum + self.c
    }
}

/// Running sums over a window's rows: of a and b (a function's first two
/// columns) shifted by the window's first values at the last rebuild, their
/// squares and their product, each compensated. Entering and leaving rows are
/// O(1); the owner rebuilds the sums from the window's rows at its
/// checkpoints ([`checkpoint_period`]), so rounding cannot accumulate over a
/// long history and the value does not depend on which bars were asked for.
#[derive(Clone, Debug, PartialEq)]
pub struct ShiftedSums {
    pub shift: (f64, f64),
    pub s: [Neumaier; 5],
    /// Rows in the window with a non-finite argument: the exact path
    /// answers then, as it does for any window holding one.
    pub nonfinite: usize,
}

/// Every how many labels of the window's calendar a `rows(N)` window's sums
/// are rebuilt from its rows: `max(N, 8)`. Both layers count labels of the
/// same calendar (the data layer holds them as `calendar_label` rows), so
/// both rebuild at the same bars (docs/spec/ERRATA.md, E-27).
pub fn checkpoint_period(n: usize) -> usize {
    n.max(8)
}

impl ShiftedSums {
    /// Empty sums shifted by a window's first row (a non-finite value
    /// shifts by zero).
    pub fn new(shift: (f64, f64)) -> ShiftedSums {
        let clean = |x: f64| if x.is_finite() { x } else { 0.0 };
        ShiftedSums {
            shift: (clean(shift.0), clean(shift.1)),
            s: [Neumaier::default(); 5],
            nonfinite: 0,
        }
    }

    /// The sums of rows, shifted by the first one's values: the rebuild.
    pub fn of_rows<'a>(rows: impl IntoIterator<Item = &'a [f64]>) -> ShiftedSums {
        let mut rows = rows.into_iter().peekable();
        let shift = rows.peek().map(|r| (r.first().copied().unwrap_or(0.0), r.get(1).copied().unwrap_or(0.0))).unwrap_or((0.0, 0.0));
        let mut s = ShiftedSums::new(shift);
        for r in rows {
            s.apply(r, 1.0);
        }
        s
    }

    /// A row enters (`sign` 1) or leaves (`sign` −1) the window.
    pub fn apply(&mut self, vals: &[f64], sign: f64) {
        let a = vals.first().copied().unwrap_or(0.0);
        let b = vals.get(1).copied().unwrap_or(0.0);
        if !a.is_finite() || !b.is_finite() {
            if sign > 0.0 {
                self.nonfinite += 1;
            } else {
                self.nonfinite -= 1;
            }
            return;
        }
        let (a, b) = (a - self.shift.0, b - self.shift.1);
        for (k, x) in [a, b, a * a, b * b, a * b].into_iter().enumerate() {
            self.s[k].add(sign * x);
        }
    }

    /// The function over the `n` rows the sums hold, or `None` where the
    /// exact path must answer: fewer than two rows, a non-finite row, a
    /// function the sums do not answer, or a series too close to constant
    /// for its centred sums to mean anything.
    pub fn value(&self, f: Function, n: usize) -> Option<f64> {
        let n = n as f64;
        if n < 2.0 || self.nonfinite > 0 {
            return None;
        }
        let [sa, sb, saa, sbb, sab] = self.s.map(|x| x.value());
        let caa = saa - sa * sa / n;
        let cbb = sbb - sb * sb / n;
        let cab = sab - sa * sb / n;
        // Centred sums below this share of the raw ones are rounding noise.
        let flat = |c: f64, raw: f64| c <= 1e-10 * raw;
        match f {
            Function::Mean => Some(self.shift.0 + sa / n),
            Function::Sum => Some(self.shift.0 * n + sa),
            Function::Std if !flat(caa, saa) => Some((caa / (n - 1.0)).sqrt()),
            Function::Cov if !flat(caa, saa) && !flat(cbb, sbb) => Some(cab / (n - 1.0)),
            Function::Corr if !flat(caa, saa) && !flat(cbb, sbb) => Some(cab / (caa * cbb).sqrt()),
            Function::OlsSlope | Function::OlsBeta if !flat(cbb, sbb) => Some(cab / cbb),
            Function::OlsR if !flat(caa, saa) && !flat(cbb, sbb) => Some(cab / (caa * cbb).sqrt()),
            _ => None,
        }
    }
}

/// The arithmetic mean, summed in order.
pub fn mean(xs: &[f64]) -> f64 {
    xs.iter().sum::<f64>() / xs.len() as f64
}

/// The sample covariance by the two-pass formula, in order (divisor n − 1).
pub fn sample_cov(f: Function, xs: &[f64], ys: &[f64]) -> Result<f64, AggError> {
    if xs.len() < 2 {
        return Err(AggError::FewerThanTwo(f));
    }
    let (mx, my) = (mean(xs), mean(ys));
    Ok(xs.iter().zip(ys).map(|(x, y)| (x - mx) * (y - my)).sum::<f64>() / (xs.len() as f64 - 1.0))
}
