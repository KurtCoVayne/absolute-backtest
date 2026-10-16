//! `bootstrap_sharpe` (Q-4.4): the annualised Sharpe ratio of a return
//! series, mean over standard deviation (n − 1) times √periods, as the
//! metrics library computes it, with the stationary bootstrap's (Politis
//! and Romano 1994) percentile interval: the (1 − level)/2 and
//! (1 + level)/2 quantiles of the ratio over the resamples, whose blocks
//! keep the serial dependence of the returns and of their dispersion.
//!
//! The null is that the Sharpe ratio is at most zero; its p-value is the
//! share of resampled ratios at or below zero, (1 + #{SR* ≤ 0}) / (1 + R),
//! the one-sided test the percentile interval inverts: the interval's lower
//! end is above zero at level 1 − 2α when the p-value is below α (up to the
//! interpolation of the quantile and the count of the observed statistic).

use crate::tests::framework::{Dependence, InputSpec, Param, ParamKind, ParamSpec, Params, Table, Test, TestError, TestResult};
use crate::tests::resample::{quantile, stationary_bootstrap};
use crate::tests::set::samples::{self, Moments};

pub struct BootstrapSharpe;

/// Fewer observations leave the stationary bootstrap too few distinct
/// resamples to approximate a distribution.
const MIN_N: usize = 8;

const PARAMS: [ParamSpec; 4] = [
    ParamSpec {
        name: "periods_per_year",
        kind: ParamKind::Float,
        default: Some(Param::Float(252.0)),
        range: Some((1e-6, 1e9)),
        doc: "the periods a year that annualise the ratio by their square root",
    },
    ParamSpec {
        name: "level",
        kind: ParamKind::Float,
        default: Some(Param::Float(0.95)),
        range: Some((0.5, 0.9999)),
        doc: "the confidence level of the interval",
    },
    samples::BLOCK_PARAM,
    samples::REPS_PARAM,
];

impl Test for BootstrapSharpe {
    fn name(&self) -> &str {
        "bootstrap_sharpe"
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
        "the Sharpe ratio is at most zero"
    }
    fn dependence(&self) -> Dependence {
        Dependence::BlockBootstrap { block: None, reps: samples::REPS }
    }
    /// The statistic is the annualised Sharpe ratio; the interval the
    /// bootstrap percentile interval at `level`; the p-value the share of
    /// resampled ratios at or below zero.
    fn run(&self, tables: &[Table], params: &Params, seed: u64) -> Result<TestResult, TestError> {
        let x = samples::series(tables, MIN_N)?;
        let m = Moments::of(x);
        if m.sd() <= 0.0 {
            return Err(TestError::Precondition("a constant series has no Sharpe ratio".into()));
        }
        let scale = samples::required(self, params, "periods_per_year")?.sqrt();
        let level = samples::required(self, params, "level")?;
        let (block, reps) = samples::bootstrap_plan(self, params, x)?;
        let sharpe = m.sharpe() * scale;
        let mut resampled = stationary_bootstrap(x, block, reps, seed, &|r: &[f64]| Moments::of(r).sharpe() * scale);
        resampled.sort_by(f64::total_cmp);
        let at_or_below_zero = resampled.partition_point(|s| *s <= 0.0);
        Ok(TestResult {
            statistic: sharpe,
            p_value: Some((1 + at_or_below_zero) as f64 / (1 + reps) as f64),
            interval: Some((quantile(&resampled, (1.0 - level) / 2.0), quantile(&resampled, (1.0 + level) / 2.0), level)),
            effect: None,
            df: None,
            n: x.len(),
            notes: vec![
                format!("per-period Sharpe ratio {}", m.sharpe()),
                format!("stationary bootstrap: mean block {}, {} replications", block, reps),
            ],
        })
    }
}
