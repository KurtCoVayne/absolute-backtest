//! `min_trl` (Q-4.4): the minimum track-record length of Bailey and López
//! de Prado (2012), as the metrics library computes it: the number of
//! returns at which the probabilistic Sharpe ratio against the benchmark
//! reaches the confidence level, given the per-period Sharpe ratio SR and
//! the moments γ₃, γ₄ of the returns observed,
//! MinTRL = 1 + (1 − γ₃ SR + (γ₄ − 1)/4 SR²) (Φ⁻¹(c) / (SR − SR*))².
//!
//! It is exact in the sample moments (the dependence treatment `exact`).
//! The null is psr's, that the Sharpe ratio is at most the benchmark, and
//! its p-value 1 − PSR is at most 1 − c exactly when the record is at least
//! MinTRL long. A Sharpe ratio at or below the benchmark has no minimum
//! length: no record is long enough.

use crate::metrics;
use crate::tests::framework::{Dependence, InputSpec, Param, ParamKind, ParamSpec, Params, Table, Test, TestError, TestResult};
use crate::tests::set::samples;

pub struct MinTrl;

const PARAMS: [ParamSpec; 2] = [
    ParamSpec {
        name: "benchmark",
        kind: ParamKind::Float,
        default: Some(Param::Float(0.0)),
        range: Some((-1e6, 1e6)),
        doc: "the benchmark Sharpe ratio SR*, per period (not annualised)",
    },
    ParamSpec {
        name: "confidence",
        kind: ParamKind::Float,
        default: Some(Param::Float(0.95)),
        range: Some((0.5, 0.999_999)),
        doc: "the confidence c the record must reach",
    },
];

impl Test for MinTrl {
    fn name(&self) -> &str {
        "min_trl"
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
    /// The statistic is MinTRL in observations; the p-value 1 − PSR against
    /// the benchmark; the effect the observations to spare, n − MinTRL
    /// (negative when the record is too short).
    fn run(&self, tables: &[Table], params: &Params, _seed: u64) -> Result<TestResult, TestError> {
        let (x, m) = samples::returns(tables)?;
        let benchmark = samples::required(self, params, "benchmark")?;
        let confidence = samples::required(self, params, "confidence")?;
        let sr = m.sharpe();
        if sr <= benchmark {
            return Err(TestError::Precondition(format!(
                "the Sharpe ratio {} is not above the benchmark {}: no track record is long enough",
                sr, benchmark
            )));
        }
        let (skew, kurt) = (m.skewness(), m.excess_kurtosis());
        let min_trl = metrics::min_track_record_length(sr, benchmark, skew, kurt, confidence);
        let psr = metrics::probabilistic_sharpe(sr, benchmark, x.len(), skew, kurt);
        let mut notes = samples::sharpe_notes(&m);
        notes.push(format!("probabilistic Sharpe ratio {} against the benchmark over {} observations", psr, x.len()));
        Ok(TestResult {
            statistic: min_trl,
            p_value: Some(1.0 - psr),
            interval: None,
            effect: Some(x.len() as f64 - min_trl),
            df: None,
            n: x.len(),
            notes,
        })
    }
}
