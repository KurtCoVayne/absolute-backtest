//! `nw_mean` (Q-4.4): the t-statistic of a series' mean with Newey and
//! West's (1987) standard error, as the metrics library computes it: the
//! long-run variance γ₀ + 2 Σₗ₌₁ᴸ (1 − l/(L + 1)) γₗ of the autocovariances
//! γₗ (divided by n) under Bartlett weights, L = ⌊4 (n/100)^(2/9)⌋ unless
//! given, and t = x̄ / √(LRV / n).
//!
//! The null is that the mean is zero; the p-value is two-sided against the
//! standard normal, the t-statistic's asymptotic law under the dependence
//! the long-run variance admits, and the interval x̄ ± z √(LRV / n).

use crate::metrics;
use crate::tests::dist;
use crate::tests::framework::{Dependence, InputSpec, Param, ParamKind, ParamSpec, Params, Table, Test, TestError, TestResult};
use crate::tests::hac;
use crate::tests::set::samples::{self, Moments};

pub struct NwMean;

const PARAMS: [ParamSpec; 2] = [
    ParamSpec {
        name: "lags",
        kind: ParamKind::Int,
        default: None,
        range: Some((0.0, 1e9)),
        doc: "the Bartlett kernel's lags (default ⌊4 (n/100)^(2/9)⌋); 0 is the iid standard error with divisor n",
    },
    ParamSpec {
        name: "level",
        kind: ParamKind::Float,
        default: Some(Param::Float(0.95)),
        range: Some((0.5, 0.9999)),
        doc: "the confidence level of the interval for the mean",
    },
];

impl Test for NwMean {
    fn name(&self) -> &str {
        "nw_mean"
    }
    fn version(&self) -> &str {
        "1"
    }
    fn inputs(&self) -> &[InputSpec] {
        &samples::RETURNS
    }
    fn params(&self) -> &[ParamSpec] {
        &PARAMS
    }
    fn null(&self) -> &str {
        "the mean of the series is zero"
    }
    fn dependence(&self) -> Dependence {
        Dependence::NeweyWest { lags: None }
    }
    /// The statistic is the Newey–West t; the p-value two-sided normal; the
    /// interval the mean's at `level`; the effect the mean.
    fn run(&self, tables: &[Table], params: &Params, _seed: u64) -> Result<TestResult, TestError> {
        let x = samples::series(tables, 2)?;
        if samples::is_constant(x) {
            return Err(TestError::Precondition("a constant series has no standard error".into()));
        }
        let n = x.len();
        let lags = samples::count(self, params, "lags")?.unwrap_or_else(|| hac::newey_west_lags(n)).min(n - 1);
        let level = samples::required(self, params, "level")?;
        let lrv = hac::long_run_variance(x, lags);
        if lrv <= 0.0 {
            return Err(TestError::Precondition(format!("the long-run variance at {} lags is {}, not positive", lags, lrv)));
        }
        let mean = Moments::of(x).mean;
        let se = (lrv / n as f64).sqrt();
        let t = metrics::newey_west_t(x, Some(lags));
        let z = dist::norm_ppf((1.0 + level) / 2.0);
        Ok(TestResult {
            statistic: t,
            p_value: Some(2.0 * dist::norm_sf(t.abs())),
            interval: Some((mean - z * se, mean + z * se, level)),
            effect: Some(mean),
            df: None,
            n,
            notes: vec![format!("Newey–West standard error {} at {} lags", se, lags), format!("long-run variance {}", lrv)],
        })
    }
}
