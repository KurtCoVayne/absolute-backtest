//! `kpss` (Q-4.4): the series is stationary about a level (or a linear
//! trend). The statistic of Kwiatkowski, Phillips, Schmidt and Shin (1992):
//! the residuals e_t of the series on a constant (and a trend), their
//! partial sums S_t, η = Σ S_t² / (n² σ̂²), with σ̂² the long-run variance of
//! the residuals with the Bartlett kernel (the HAC engine) at the
//! Newey–West lag ⌊4 (n/100)^(2/9)⌋ or the stated one, whose correction
//! makes the asymptotic null distribution free of the serial correlation
//! (Dependence::Asymptotic). The p-value interpolates linearly in the
//! paper's Table 1, which tabulates the upper tail from 10 to 1 %; beyond
//! it the p-value is the table's bound, and the notes say so.

use crate::tests::framework::{Dependence, InputSpec, ParamKind, ParamSpec, Params, Shape, Table, Test, TestError, TestResult};
use crate::tests::hac;
use crate::tests::regression::{ols, Covariance};

use super::timeseries::{series, usize_param, Deterministic};

pub struct Kpss;

/// Kwiatkowski et al. (1992), Table 1: the upper-tail critical values of
/// η_μ (stationarity about a level) at the sizes of `SIZES`.
pub const LEVEL: [f64; 4] = [0.347, 0.463, 0.574, 0.739];

/// Kwiatkowski et al. (1992), Table 1: the upper-tail critical values of
/// η_τ (stationarity about a linear trend) at the sizes of `SIZES`.
pub const TREND: [f64; 4] = [0.119, 0.146, 0.176, 0.216];

/// The sizes Table 1 tabulates: 10, 5, 2.5 and 1 %.
pub const SIZES: [f64; 4] = [0.10, 0.05, 0.025, 0.01];

/// The p-value of η by linear interpolation in Table 1, for the level form
/// (`det` a constant) or the trend form, and whether it is a bound: below
/// the table the p-value is at least 0.10, above it at most 0.01.
pub fn p_value(eta: f64, det: Deterministic) -> (f64, Option<&'static str>) {
    let cv = if det == Deterministic::Trend { &TREND } else { &LEVEL };
    if eta < cv[0] {
        return (SIZES[0], Some("η is below Table 1: the p-value is greater than 0.10"));
    }
    if eta > cv[3] {
        return (SIZES[3], Some("η is above Table 1: the p-value is smaller than 0.01"));
    }
    for i in 0..3 {
        if eta <= cv[i + 1] {
            let w = (eta - cv[i]) / (cv[i + 1] - cv[i]);
            return (SIZES[i] + w * (SIZES[i + 1] - SIZES[i]), None);
        }
    }
    (SIZES[3], None)
}

const INPUTS: [InputSpec; 1] = [InputSpec {
    name: "series",
    shape: Shape::Series,
    columns: &["x"],
}];

const PARAMS: [ParamSpec; 2] = [
    ParamSpec {
        name: "trend",
        kind: ParamKind::Text,
        default: None,
        range: None,
        doc: "`c` for stationarity about a level (the default), `ct` about a linear trend",
    },
    ParamSpec {
        name: "lags",
        kind: ParamKind::Int,
        default: None,
        range: Some((0.0, 1e6)),
        doc: "the Bartlett bandwidth of the long-run variance (default ⌊4 (n/100)^(2/9)⌋)",
    },
];

impl Test for Kpss {
    fn name(&self) -> &str {
        "kpss"
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
        "the series is stationary about a level (or a linear trend)"
    }
    fn dependence(&self) -> Dependence {
        Dependence::Asymptotic
    }
    /// η with its interpolated p-value (large values reject stationarity);
    /// the notes carry the bandwidth, the critical values and, when the
    /// p-value is a bound of the table, which one.
    fn run(&self, tables: &[Table], params: &Params, _seed: u64) -> Result<TestResult, TestError> {
        let x = series(tables, "kpss", 4)?;
        let det = Deterministic::from_trend(self, params)?;
        let n = x.len();
        let lags = usize_param(self, params, "lags")?.unwrap_or_else(|| hac::newey_west_lags(n));
        if lags >= n {
            return Err(TestError::Precondition(format!("kpss: the bandwidth {} is not below n = {}", lags, n)));
        }
        let fit = ols(&det.columns(n), x, &Covariance::Iid).map_err(TestError::Numeric)?;
        let e = fit.residuals;
        let lrv = hac::long_run_variance(&e, lags);
        if lrv.is_nan() || lrv <= 0.0 {
            return Err(TestError::Precondition("kpss: the residuals have no variance".into()));
        }
        let mut partial = 0.0;
        let mut sum_sq = 0.0;
        for v in &e {
            partial += v;
            sum_sq += partial * partial;
        }
        let nf = n as f64;
        let eta = sum_sq / (nf * nf * lrv);
        let (p, bound) = p_value(eta, det);
        let cv = if det == Deterministic::Trend { &TREND } else { &LEVEL };
        let mut notes = vec![
            format!("lags {}", lags),
            format!("deterministic terms `{}`", det.name()),
            format!("critical values 10 % {}, 5 % {}, 2.5 % {}, 1 % {}", cv[0], cv[1], cv[2], cv[3]),
        ];
        notes.extend(bound.map(String::from));
        Ok(TestResult {
            statistic: eta,
            p_value: Some(p),
            interval: None,
            effect: None,
            df: None,
            n,
            notes,
        })
    }
}
