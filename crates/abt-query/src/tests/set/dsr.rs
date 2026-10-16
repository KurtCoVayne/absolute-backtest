//! `dsr` (Q-4.4): the deflated Sharpe ratio of Bailey and López de Prado
//! (2014), as the metrics library computes it: the probabilistic Sharpe
//! ratio against the Sharpe ratio the best of N unskilled trials is
//! expected to reach, SR₀ = √V ((1 − γ) Φ⁻¹(1 − 1/N) + γ Φ⁻¹(1 − 1/(N e))),
//! with V the variance of the trials' Sharpe ratios and γ Euler's constant.
//!
//! N and V are the lineage's (Q-3.2), given as parameters, per period. The
//! deflation is exact in the sample moments (the dependence treatment
//! `exact`); the null that the Sharpe ratio is at most SR₀ has the p-value
//! 1 − DSR.

use crate::metrics;
use crate::tests::framework::{Dependence, InputSpec, Param, ParamKind, ParamSpec, Params, Table, Test, TestError, TestResult};
use crate::tests::set::samples;

pub struct Dsr;

const PARAMS: [ParamSpec; 2] = [
    ParamSpec {
        name: "trials",
        kind: ParamKind::Int,
        default: Some(Param::Int(1)),
        range: Some((1.0, 1e15)),
        doc: "the number of trials N the Sharpe ratio was selected from (the lineage's count, Q-3.2)",
    },
    ParamSpec {
        name: "var_trials",
        kind: ParamKind::Float,
        default: Some(Param::Float(0.0)),
        range: Some((0.0, 1e6)),
        doc: "the variance V of the trials' Sharpe ratios, per period (not annualised)",
    },
];

impl Test for Dsr {
    fn name(&self) -> &str {
        "dsr"
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
        "the true Sharpe ratio is at most the expected maximum Sharpe ratio of the unskilled trials"
    }
    fn dependence(&self) -> Dependence {
        Dependence::Exact
    }
    /// The statistic is DSR; the p-value 1 − DSR; the effect the per-period
    /// Sharpe ratio less SR₀.
    fn run(&self, tables: &[Table], params: &Params, _seed: u64) -> Result<TestResult, TestError> {
        let (x, m) = samples::returns(tables)?;
        let trials = samples::count(self, params, "trials")?.unwrap_or(1);
        let var_trials = samples::required(self, params, "var_trials")?;
        let sr = m.sharpe();
        let d = metrics::deflated_sharpe(sr, x.len(), m.skewness(), m.excess_kurtosis(), trials, var_trials);
        let mut notes = samples::sharpe_notes(&m);
        notes.push(format!("expected maximum Sharpe ratio of {} trials with variance {}: {}", d.trials, var_trials, d.expected_max_sr));
        Ok(TestResult {
            statistic: d.dsr,
            p_value: Some(1.0 - d.dsr),
            interval: None,
            effect: Some(sr - d.expected_max_sr),
            df: None,
            n: x.len(),
            notes,
        })
    }
}
