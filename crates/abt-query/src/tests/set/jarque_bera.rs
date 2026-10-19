//! `jarque_bera` (Q-4.4): normality by Jarque and Bera's (1987) statistic
//! JB = n/6 · (S² + K²/4) on the sample skewness S and excess kurtosis K
//! (population moments, as the metrics library computes them).
//!
//! The textbook p-value, χ²(2), assumes independent observations; returns
//! are serially dependent and their dispersion clusters, which widens the
//! sampling distribution of S and K. The p-value is therefore the
//! stationary bootstrap's (Politis and Romano 1994) with the null imposed
//! by re-centring: a resample's statistic is n/6 · ((S* − S)² + (K* − K)²/4),
//! the distribution of (S, K) about the null's (0, 0) approximated by that
//! of the resampled (S*, K*) about the sample's own. The χ²(2) p-value is in
//! the notes.

use crate::tests::dist;
use crate::tests::framework::{Dependence, InputSpec, ParamSpec, Params, Shape, Table, Test, TestError, TestResult};
use crate::tests::resample::stationary_bootstrap;
use crate::tests::set::samples::{self, Moments};

pub struct JarqueBera;

/// Fewer observations leave the stationary bootstrap too few distinct
/// resamples to approximate a distribution.
const MIN_N: usize = 8;

const INPUTS: [InputSpec; 1] = [InputSpec {
    name: "series",
    shape: Shape::Series,
    columns: &["x"],
}];

const PARAMS: [ParamSpec; 2] = [samples::BLOCK_PARAM, samples::REPS_PARAM];

/// The Jarque–Bera statistic of `n` observations with skewness `s` and
/// excess kurtosis `k`.
pub fn statistic(n: usize, s: f64, k: f64) -> f64 {
    n as f64 / 6.0 * (s * s + k * k / 4.0)
}

impl Test for JarqueBera {
    fn name(&self) -> &str {
        "jarque_bera"
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
        "the series is normally distributed: its skewness and excess kurtosis are zero"
    }
    fn dependence(&self) -> Dependence {
        Dependence::BlockBootstrap { block: None, reps: samples::REPS }
    }
    /// The statistic is JB; the p-value the re-centred stationary
    /// bootstrap's; the notes carry S, K, the asymptotic χ²(2) p-value, the
    /// block and the replications.
    fn run(&self, tables: &[Table], params: &Params, seed: u64) -> Result<TestResult, TestError> {
        let x = samples::series(tables, MIN_N)?;
        let m = Moments::of(x);
        if m.m2() <= 0.0 {
            return Err(TestError::Precondition("a constant series has no skewness or kurtosis".into()));
        }
        let n = x.len();
        let (s, k) = (m.skewness(), m.excess_kurtosis());
        let jb = statistic(n, s, k);
        let (block, reps) = samples::bootstrap_plan(self, params, x)?;
        let resampled = stationary_bootstrap(x, block, reps, seed, &|r: &[f64]| {
            let b = Moments::of(r);
            statistic(n, b.skewness() - s, b.excess_kurtosis() - k)
        });
        Ok(TestResult {
            statistic: jb,
            p_value: Some(samples::upper_p(&resampled, jb)),
            interval: None,
            effect: None,
            df: Some(2.0),
            n,
            notes: vec![
                format!("skewness {}", s),
                format!("excess kurtosis {}", k),
                format!("asymptotic chi2(2) p-value {} (independent observations)", dist::chi2_sf(jb, 2.0)),
                format!("stationary bootstrap: mean block {}, {} replications", block, reps),
            ],
        })
    }
}
