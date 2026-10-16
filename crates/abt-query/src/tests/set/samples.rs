//! What the distribution, resampling and Sharpe-ratio tests of the starting
//! set share (Q-4.4): the series a test reads from its input, the sample
//! moments as the metrics library computes them, the mean block of the
//! stationary bootstrap for statistics of a series' level and dispersion,
//! the reading of numeric parameters against their declared ranges, and the
//! p-value of a resampled statistic.

use crate::tests::framework::{param, InputSpec, Param, ParamKind, ParamSpec, Params, Shape, Table, Test, TestError};
use crate::tests::resample::politis_white_block;

/// The default number of bootstrap replications (Q-4.2).
pub const REPS: usize = 2_000;

/// The `block` parameter of a stationary-bootstrap test.
pub const BLOCK_PARAM: ParamSpec = ParamSpec {
    name: "block",
    kind: ParamKind::Float,
    default: None,
    range: Some((1.0, 1e9)),
    doc: "the stationary bootstrap's mean block length (default: the larger of the Politis–White blocks of the series and of its squared deviations)",
};

/// The `reps` parameter of a stationary-bootstrap test.
pub const REPS_PARAM: ParamSpec = ParamSpec {
    name: "reps",
    kind: ParamKind::Int,
    default: Some(Param::Int(REPS as i64)),
    range: Some((1.0, 1e9)),
    doc: "bootstrap replications",
};

/// The first input table, which must exist.
pub fn table(tables: &[Table]) -> Result<&Table, TestError> {
    tables.first().ok_or_else(|| TestError::Input("the test takes one table".into()))
}

/// A column checked to hold only finite numbers.
pub fn finite<'a>(x: &'a [f64], what: &str) -> Result<&'a [f64], TestError> {
    match x.iter().position(|v| !v.is_finite()) {
        Some(i) => Err(TestError::Input(format!("{} holds {} at row {}", what, x[i], i))),
        None => Ok(x),
    }
}

/// The series of a one-series test: the first column of the first table,
/// finite, with at least `min` observations.
pub fn series(tables: &[Table], min: usize) -> Result<&[f64], TestError> {
    let x = finite(table(tables)?.first()?, "the series")?;
    if x.len() < min {
        return Err(TestError::Precondition(format!("{} observations; the test needs at least {}", x.len(), min)));
    }
    Ok(x)
}

/// The sample moments of a series: the mean and the sums of the second,
/// third and fourth powers of the deviations from it. The sums run in the
/// order the metrics library (Q-3.1) runs them, so the standard deviation,
/// the skewness and the excess kurtosis of a series that is not constant
/// equal the metrics' to the bit, and a test reports the number a report
/// does.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Moments {
    pub n: usize,
    pub mean: f64,
    /// Σ (x − x̄)².
    pub s2: f64,
    /// Σ (x − x̄)³.
    pub s3: f64,
    /// Σ (x − x̄)⁴.
    pub s4: f64,
}

impl Moments {
    /// The moments of `x` by two passes: the mean, then the deviations. A
    /// constant series has its value for mean and no deviation, exactly: the
    /// rounding of a computed mean (thirty times 0.01 sums to more than 0.3)
    /// would otherwise make up a tiny variance and a skewness of rounding
    /// errors.
    pub fn of(x: &[f64]) -> Moments {
        let n = x.len();
        if n > 0 && is_constant(x) {
            return Moments {
                n,
                mean: x[0],
                s2: 0.0,
                s3: 0.0,
                s4: 0.0,
            };
        }
        let mean = if n == 0 { 0.0 } else { x.iter().sum::<f64>() / n as f64 };
        let (mut s2, mut s3, mut s4) = (0.0, 0.0, 0.0);
        for v in x {
            let d = v - mean;
            s2 += d.powi(2);
            s3 += d.powi(3);
            s4 += d.powi(4);
        }
        Moments { n, mean, s2, s3, s4 }
    }

    /// The second central moment, Σ (x − x̄)² / n.
    pub fn m2(&self) -> f64 {
        if self.n == 0 {
            0.0
        } else {
            self.s2 / self.n as f64
        }
    }

    /// The sample variance, Σ (x − x̄)² / (n − 1).
    pub fn variance(&self) -> f64 {
        if self.n < 2 {
            0.0
        } else {
            self.s2 / (self.n as f64 - 1.0)
        }
    }

    /// The sample standard deviation (n − 1).
    pub fn sd(&self) -> f64 {
        self.variance().sqrt()
    }

    /// The skewness m₃ / m₂^(3/2) over population moments (zero for a
    /// constant series), as `metrics::skewness`.
    pub fn skewness(&self) -> f64 {
        let m2 = self.m2();
        if m2 <= 0.0 {
            0.0
        } else {
            self.s3 / self.n as f64 / m2.powf(1.5)
        }
    }

    /// The excess kurtosis m₄ / m₂² − 3 over population moments (zero for a
    /// constant series), as `metrics::excess_kurtosis`.
    pub fn excess_kurtosis(&self) -> f64 {
        let m2 = self.m2();
        if m2 <= 0.0 {
            0.0
        } else {
            self.s4 / self.n as f64 / (m2 * m2) - 3.0
        }
    }

    /// The per-period Sharpe ratio, the mean over the sample standard
    /// deviation (zero for a constant series), as the metrics' `sharpe_period`.
    pub fn sharpe(&self) -> f64 {
        let sd = self.sd();
        if sd > 0.0 {
            self.mean / sd
        } else {
            0.0
        }
    }
}

/// Whether every value of `x` is the first's.
pub fn is_constant(x: &[f64]) -> bool {
    x.iter().all(|v| *v == x[0])
}

/// The input of the Sharpe-ratio tests: a return series of at least three
/// observations (the fewest with a skewness other than zero), not constant,
/// with its moments.
pub fn returns(tables: &[Table]) -> Result<(&[f64], Moments), TestError> {
    let x = series(tables, 3)?;
    let m = Moments::of(x);
    if m.sd() <= 0.0 {
        return Err(TestError::Precondition("a constant series has no Sharpe ratio".into()));
    }
    Ok((x, m))
}

/// The inputs of the Sharpe-ratio tests.
pub const RETURNS: [InputSpec; 1] = [InputSpec {
    name: "returns",
    shape: Shape::Series,
    columns: &["r"],
}];

/// The notes on a return series' Sharpe ratio and moments.
pub fn sharpe_notes(m: &Moments) -> Vec<String> {
    vec![
        format!("per-period Sharpe ratio {}", m.sharpe()),
        format!("skewness {}", m.skewness()),
        format!("excess kurtosis {}", m.excess_kurtosis()),
    ]
}

/// The mean block of the stationary bootstrap for a statistic of a series'
/// level and dispersion (its moments, its Sharpe ratio, its empirical
/// distribution): the larger of the Politis–White (2004) blocks of the
/// series and of its squared deviations. The rule for the mean alone would
/// miss the dependence that matters most in returns, the clustering of
/// their dispersion, which a statistic of the second and higher moments
/// inherits.
pub fn dispersion_block(x: &[f64]) -> f64 {
    let mean = Moments::of(x).mean;
    let squares: Vec<f64> = x.iter().map(|v| (v - mean) * (v - mean)).collect();
    politis_white_block(x).max(politis_white_block(&squares))
}

/// The declared range of a test's parameter.
fn range(test: &dyn Test, name: &str) -> Option<(f64, f64)> {
    test.params().iter().find(|p| p.name == name).and_then(|p| p.range)
}

/// A numeric parameter's value or its declared default, checked to be a
/// finite number inside its declared range; `None` when it is unset and has
/// no default.
pub fn number(test: &dyn Test, params: &Params, name: &str) -> Result<Option<f64>, TestError> {
    let v = match param(test, params, name) {
        None => return Ok(None),
        Some(Param::Float(x)) => *x,
        Some(Param::Int(x)) => *x as f64,
        Some(other) => return Err(TestError::Input(format!("`{}` takes a number, not `{}`", name, other))),
    };
    if !v.is_finite() {
        return Err(TestError::Input(format!("`{}` is {}", name, v)));
    }
    if let Some((lo, hi)) = range(test, name) {
        if v < lo || v > hi {
            return Err(TestError::Input(format!("`{}` is {}, outside [{}, {}]", name, v, lo, hi)));
        }
    }
    Ok(Some(v))
}

/// A whole-number parameter's value or its declared default, inside its
/// declared range; `None` when it is unset and has no default.
pub fn count(test: &dyn Test, params: &Params, name: &str) -> Result<Option<usize>, TestError> {
    match number(test, params, name)? {
        None => Ok(None),
        Some(v) if v >= 0.0 && v.fract() == 0.0 => Ok(Some(v as usize)),
        Some(v) => Err(TestError::Input(format!("`{}` takes a whole number, not {}", name, v))),
    }
}

/// A numeric parameter's value, which is given or has a default.
pub fn required(test: &dyn Test, params: &Params, name: &str) -> Result<f64, TestError> {
    number(test, params, name)?.ok_or_else(|| TestError::Input(format!("`{}` is required", name)))
}

/// The stationary bootstrap's mean block and replications for `x`: the
/// parameters when given, else the dispersion block and `REPS`.
pub fn bootstrap_plan(test: &dyn Test, params: &Params, x: &[f64]) -> Result<(f64, usize), TestError> {
    let block = match number(test, params, "block")? {
        Some(b) => b,
        None => dispersion_block(x),
    };
    let reps = count(test, params, "reps")?.unwrap_or(REPS);
    Ok((block, reps))
}

/// The bootstrap p-value of an upper-tail statistic: the share of
/// resampled statistics at least the observed one, counting the observed
/// as one of them, (1 + #{T* ≥ T}) / (1 + R), so that it is never zero.
pub fn upper_p(resampled: &[f64], observed: f64) -> f64 {
    let at_least = resampled.iter().filter(|t| **t >= observed).count();
    (1 + at_least) as f64 / (1 + resampled.len()) as f64
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::metrics;

    #[test]
    fn the_moments_are_the_metrics_to_the_bit() {
        let x: Vec<f64> = (0..97).map(|i| ((i * 37 % 101) as f64 / 13.0).sin() * 0.02 + 0.0004 * (i % 7) as f64).collect();
        let m = Moments::of(&x);
        assert_eq!(m.skewness().to_bits(), metrics::skewness(&x).to_bits());
        assert_eq!(m.excess_kurtosis().to_bits(), metrics::excess_kurtosis(&x).to_bits());
        let flat = Moments::of(&[2.0; 5]);
        assert_eq!((flat.sd(), flat.skewness(), flat.excess_kurtosis(), flat.sharpe()), (0.0, 0.0, 0.0, 0.0));
    }

    #[test]
    fn the_p_value_counts_the_observed_statistic() {
        assert_eq!(upper_p(&[1.0, 2.0, 3.0], 2.0), 0.75);
        assert_eq!(upper_p(&[1.0, 2.0, 3.0], 9.0), 0.25);
    }
}
