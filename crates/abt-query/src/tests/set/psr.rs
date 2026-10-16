//! `psr` (Q-4.4): the probabilistic Sharpe ratio of Bailey and López de
//! Prado (2012), as the metrics library computes it: the probability that
//! the true per-period Sharpe ratio exceeds a benchmark, given the estimate
//! SR over n returns with skewness γ₃ and kurtosis γ₄,
//! PSR = Φ((SR − SR*) √(n − 1) / √(1 − γ₃ SR + (γ₄ − 1)/4 SR²)).
//!
//! The standard error is Mertens' (2002) for independent non-normal returns,
//! exact in the sample moments (the dependence treatment `exact`); the null
//! that the Sharpe ratio is at most the benchmark has the p-value 1 − PSR.

use crate::metrics;
use crate::tests::framework::{Dependence, InputSpec, Param, ParamKind, ParamSpec, Params, Table, Test, TestError, TestResult};
use crate::tests::set::samples;

pub struct Psr;

const PARAMS: [ParamSpec; 1] = [ParamSpec {
    name: "benchmark",
    kind: ParamKind::Float,
    default: Some(Param::Float(0.0)),
    range: Some((-1e6, 1e6)),
    doc: "the benchmark Sharpe ratio SR*, per period (not annualised)",
}];

impl Test for Psr {
    fn name(&self) -> &str {
        "psr"
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
        "the true Sharpe ratio is at most the benchmark"
    }
    fn dependence(&self) -> Dependence {
        Dependence::Exact
    }
    /// The statistic is PSR; the p-value 1 − PSR; the effect the per-period
    /// Sharpe ratio less the benchmark.
    fn run(&self, tables: &[Table], params: &Params, _seed: u64) -> Result<TestResult, TestError> {
        let (x, m) = samples::returns(tables)?;
        let benchmark = samples::required(self, params, "benchmark")?;
        let sr = m.sharpe();
        let psr = metrics::probabilistic_sharpe(sr, benchmark, x.len(), m.skewness(), m.excess_kurtosis());
        Ok(TestResult {
            statistic: psr,
            p_value: Some(1.0 - psr),
            interval: None,
            effect: Some(sr - benchmark),
            df: None,
            n: x.len(),
            notes: samples::sharpe_notes(&m),
        })
    }
}
