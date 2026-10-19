//! `half_life` (Q-4.4): the series does not revert to a mean (φ = 1). The
//! AR(1) fit x_t = c + φ x_{t−1} + ε_t by the regression engine with its
//! Newey–West covariance (Dependence::NeweyWest), and the half-life of a
//! deviation, −ln 2 / ln φ̂ bars (the Ornstein–Uhlenbeck half-life ln 2 / θ
//! with φ = e^{−θ} per bar), with its interval: the Newey–West interval of
//! φ mapped through the half-life, which is monotone in φ. The p-value is
//! the one-sided normal p-value of the Newey–West t of φ − 1; under a unit
//! root that t has the Dickey–Fuller distribution instead, which `adf`
//! reads.

use crate::tests::dist;
use crate::tests::framework::{Dependence, InputSpec, Param, ParamKind, ParamSpec, Params, Shape, Table, Test, TestError, TestResult};
use crate::tests::hac;
use crate::tests::regression::{ols, Covariance};

use super::timeseries::{f64_param, series, usize_param};

pub struct HalfLife;

/// The half-life in bars of a deviation under the AR(1) coefficient φ:
/// −ln 2 / ln φ on (0, 1); zero at or below zero, where a deviation's
/// expectation crosses zero within one bar; infinite at or above one, where
/// it never reverts.
pub fn half_life(phi: f64) -> f64 {
    if phi <= 0.0 {
        0.0
    } else if phi >= 1.0 {
        f64::INFINITY
    } else {
        -std::f64::consts::LN_2 / phi.ln()
    }
}

const INPUTS: [InputSpec; 1] = [InputSpec {
    name: "series",
    shape: Shape::Series,
    columns: &["x"],
}];

const PARAMS: [ParamSpec; 2] = [
    ParamSpec {
        name: "lags",
        kind: ParamKind::Int,
        default: None,
        range: Some((0.0, 1e6)),
        doc: "Newey–West lags (default ⌊4 (n/100)^(2/9)⌋)",
    },
    ParamSpec {
        name: "level",
        kind: ParamKind::Float,
        default: Some(Param::Float(0.95)),
        range: Some((0.5, 0.9999)),
        doc: "the confidence level of the interval",
    },
];

impl Test for HalfLife {
    fn name(&self) -> &str {
        "half_life"
    }
    fn version(&self) -> &str {
        "1"
    }
    fn inputs(&self) -> &[InputSpec] {
        &INPUTS
    }
    fn params(&self) -> &[ParamSpec] {
        &PARAMS
    }
    fn null(&self) -> &str {
        "the series does not revert to a mean: its AR(1) coefficient is one"
    }
    fn dependence(&self) -> Dependence {
        Dependence::NeweyWest { lags: None }
    }
    /// The statistic is the half-life in bars, with its interval at the
    /// level; the effect is φ̂; n is the fit's number of observations; the
    /// notes carry φ̂ with its standard error and the t of φ − 1.
    fn run(&self, tables: &[Table], params: &Params, _seed: u64) -> Result<TestResult, TestError> {
        let x = series(tables, "half_life", 4)?;
        let level = f64_param(self, params, "level")?.unwrap_or(0.95);
        if level <= 0.0 || level >= 1.0 {
            return Err(TestError::Input(format!("half_life: level is in (0, 1), not {}", level)));
        }
        let rows = x.len() - 1;
        let lags = usize_param(self, params, "lags")?.unwrap_or_else(|| hac::newey_west_lags(rows));
        let fit = ols(&[vec![1.0; rows], x[..rows].to_vec()], &x[1..], &Covariance::NeweyWest(Some(lags))).map_err(TestError::Numeric)?;
        let (phi, se) = (fit.beta[1], fit.se[1]);
        if se.is_nan() || se <= 0.0 {
            return Err(TestError::Numeric("half_life: the AR(1) coefficient has no standard error".into()));
        }
        let z = dist::norm_ppf(0.5 + level / 2.0);
        let (lo, hi) = (phi - z * se, phi + z * se);
        let t = (phi - 1.0) / se;
        Ok(TestResult {
            statistic: half_life(phi),
            p_value: Some(dist::norm_cdf(t)),
            interval: Some((half_life(lo), half_life(hi), level)),
            effect: Some(phi),
            df: None,
            n: rows,
            notes: vec![
                format!("phi {} (Newey–West se {}, {} lags)", phi, se, lags),
                format!("t of phi − 1 {}", t),
                format!("phi interval {} to {}", lo, hi),
            ],
        })
    }
}
