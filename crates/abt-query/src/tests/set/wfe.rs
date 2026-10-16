//! `wfe` (Q-4.4): Pardo's (2008) walk-forward efficiency, the annualised
//! out-of-sample return over the annualised in-sample return, over the folds
//! of a walk-forward: with one row per fold holding the chosen point's
//! annualised returns in sample and out of sample, WFE is the mean of the
//! latter over the mean of the former, as the kernel's study computes it
//! from the folds' CAGRs.
//!
//! The null is that the efficiency is at most e₀, that is that the folds'
//! excesses dₖ = outₖ − e₀ · inₖ have a mean at most zero (the in-sample
//! mean being positive). The p-value is Fisher's randomisation test of that
//! mean: the folds' test windows do not overlap and their excesses are taken
//! as independent and, at the null's boundary, symmetric about zero, so
//! every assignment of signs to their magnitudes is as likely as the
//! observed one, and the p-value is the share of the 2ᴷ assignments whose
//! sum is at least the observed sum, enumerated exactly (the dependence
//! treatment `exact`). Beyond 20 folds `reps` assignments are drawn instead,
//! the observed one counted among them, which keeps the test's level exact.

use crate::tests::framework::{Dependence, InputSpec, Param, ParamKind, ParamSpec, Params, Shape, Table, Test, TestError, TestResult};
use crate::tests::rng::Rng;
use crate::tests::set::samples;

pub struct Wfe;

/// The most folds whose sign assignments are enumerated.
const MAX_EXACT: usize = 20;

const INPUTS: [InputSpec; 1] = [InputSpec {
    name: "folds",
    shape: Shape::Rows,
    columns: &["in_sample", "out_of_sample"],
}];

const PARAMS: [ParamSpec; 2] = [
    ParamSpec {
        name: "e0",
        kind: ParamKind::Float,
        default: Some(Param::Float(0.5)),
        range: Some((-1e6, 1e6)),
        doc: "the efficiency of the null (0.5: Pardo's threshold of a robust strategy)",
    },
    ParamSpec {
        name: "reps",
        kind: ParamKind::Int,
        default: Some(Param::Int(samples::REPS as i64)),
        range: Some((1.0, 1e9)),
        doc: "sign assignments drawn beyond 20 folds",
    },
];

/// The randomisation p-value of the sum of `d`: the share of sign
/// assignments s with Σ sₖ |dₖ| ≥ Σ dₖ, all 2ᴷ of them up to `MAX_EXACT`
/// values and otherwise `reps` drawn ones (replication r from
/// `Rng::stream(seed, r)`) with the observed assignment counted. Sums that
/// differ from the observed by rounding alone count as equal to it.
pub fn sign_flip_p(d: &[f64], reps: usize, seed: u64) -> f64 {
    let observed: f64 = d.iter().sum();
    let magnitude: Vec<f64> = d.iter().map(|v| v.abs()).collect();
    let tolerance = 1e-12 * magnitude.iter().sum::<f64>();
    let at_least = |s: f64| s >= observed - tolerance;
    if d.len() <= MAX_EXACT {
        let total = 1u64 << d.len();
        let hits = (0..total)
            .filter(|&bits| at_least(magnitude.iter().enumerate().map(|(k, m)| if (bits >> k) & 1 == 1 { -m } else { *m }).sum()))
            .count();
        hits as f64 / total as f64
    } else {
        let hits = (0..reps)
            .filter(|r| {
                let mut rng = Rng::stream(seed, *r as u64);
                at_least(magnitude.iter().map(|m| if rng.next_u64() >> 63 == 1 { -m } else { *m }).sum())
            })
            .count();
        (1 + hits) as f64 / (1 + reps) as f64
    }
}

impl Test for Wfe {
    fn name(&self) -> &str {
        "wfe"
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
        "the walk-forward efficiency is at most e0"
    }
    fn dependence(&self) -> Dependence {
        Dependence::Exact
    }
    /// The statistic is WFE; the p-value the randomisation test's; the
    /// effect the mean excess out of sample over e₀ times in sample.
    fn run(&self, tables: &[Table], params: &Params, seed: u64) -> Result<TestResult, TestError> {
        let t = samples::table(tables)?;
        let is = samples::finite(t.column("in_sample")?, "`in_sample`")?;
        let oos = samples::finite(t.column("out_of_sample")?, "`out_of_sample`")?;
        if is.len() != oos.len() {
            return Err(TestError::Input(format!("{} in-sample returns and {} out-of-sample", is.len(), oos.len())));
        }
        if is.is_empty() {
            return Err(TestError::Input("no folds".into()));
        }
        let k = is.len() as f64;
        let (mean_is, mean_oos) = (is.iter().sum::<f64>() / k, oos.iter().sum::<f64>() / k);
        if mean_is <= 0.0 {
            return Err(TestError::Precondition(format!(
                "the mean in-sample annualised return is {}: the efficiency of a strategy that did not earn in sample is undefined",
                mean_is
            )));
        }
        let e0 = samples::required(self, params, "e0")?;
        let reps = samples::count(self, params, "reps")?.unwrap_or(samples::REPS);
        let excess: Vec<f64> = is.iter().zip(oos).map(|(i, o)| o - e0 * i).collect();
        let how = if excess.len() <= MAX_EXACT {
            format!("all {} sign assignments", 1u64 << excess.len())
        } else {
            format!("{} drawn sign assignments", reps)
        };
        Ok(TestResult {
            statistic: mean_oos / mean_is,
            p_value: Some(sign_flip_p(&excess, reps, seed)),
            interval: None,
            effect: Some(excess.iter().sum::<f64>() / k),
            df: None,
            n: is.len(),
            notes: vec![
                format!("mean annualised return {} in sample, {} out of sample", mean_is, mean_oos),
                format!("randomisation test over {}", how),
            ],
        })
    }
}
