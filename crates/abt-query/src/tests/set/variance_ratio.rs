//! `variance_ratio` (Q-4.4): the series is a random walk. Lo and
//! MacKinlay's (1988) variance ratio VR(q), the variance of q-period
//! increments over q times that of one-period increments, from overlapping
//! increments with their unbiased estimators, and the
//! heteroskedasticity-robust z*(q) = √T (VR(q) − 1) / √θ̂(q), asymptotically
//! normal when the increments are uncorrelated but heteroskedastic
//! (Dependence::IidRobust). For T increments x_k with mean μ̂:
//! σ̂²_a = Σ (x_k − μ̂)² / (T − 1); σ̂²_c = Σ_{k ≥ q} (p_k − p_{k−q} − qμ̂)² / m
//! with m = q (T − q + 1)(1 − q/T); θ̂(q) = Σ_{j<q} (2(q − j)/q)² δ̂(j) with
//! δ̂(j) = T Σ (x_k − μ̂)² (x_{k−j} − μ̂)² / (Σ (x_k − μ̂)²)².

use crate::tests::dist;
use crate::tests::framework::{Dependence, InputSpec, Param, ParamKind, ParamSpec, Params, Shape, Table, Test, TestError, TestResult};

use super::timeseries::{bool_param, series, usize_param};

pub struct VarianceRatio;

const INPUTS: [InputSpec; 1] = [InputSpec {
    name: "series",
    shape: Shape::Series,
    columns: &["x"],
}];

const PARAMS: [ParamSpec; 2] = [
    ParamSpec {
        name: "q",
        kind: ParamKind::Int,
        default: Some(Param::Int(2)),
        range: Some((2.0, 1e6)),
        doc: "the horizon q of the ratio, in periods",
    },
    ParamSpec {
        name: "increments",
        kind: ParamKind::Bool,
        default: Some(Param::Bool(false)),
        range: None,
        doc: "the column holds the increments (returns) rather than the level (a log price)",
    },
];

impl Test for VarianceRatio {
    fn name(&self) -> &str {
        "variance_ratio"
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
        "the series is a random walk: its increments are uncorrelated"
    }
    fn dependence(&self) -> Dependence {
        Dependence::IidRobust
    }
    /// The statistic is z*(q) with its two-sided normal p-value; the effect
    /// is VR(q), above one for trending increments and below for mean
    /// reversion; n is the number of increments.
    fn run(&self, tables: &[Table], params: &Params, _seed: u64) -> Result<TestResult, TestError> {
        let x = series(tables, "variance_ratio", 2)?;
        let q = usize_param(self, params, "q")?.unwrap_or(2);
        if q < 2 {
            return Err(TestError::Input(format!("variance_ratio: q is at least 2, not {}", q)));
        }
        // The level p_0, …, p_T: the input, or the partial sums of the
        // increments from zero.
        let level: Vec<f64> = if bool_param(self, params, "increments")?.unwrap_or(false) {
            std::iter::once(0.0)
                .chain(x.iter().scan(0.0, |s, v| {
                    *s += v;
                    Some(*s)
                }))
                .collect()
        } else {
            x.to_vec()
        };
        let t = level.len() - 1;
        if t <= q {
            return Err(TestError::Precondition(format!("variance_ratio needs more than q = {} increments, not {}", q, t)));
        }
        let tf = t as f64;
        let mu = (level[t] - level[0]) / tf;
        let e: Vec<f64> = level.windows(2).map(|w| w[1] - w[0] - mu).collect();
        let e2: Vec<f64> = e.iter().map(|v| v * v).collect();
        let s0: f64 = e2.iter().sum();
        if s0.is_nan() || s0 <= 0.0 {
            return Err(TestError::Precondition("variance_ratio: the increments are constant".into()));
        }
        let sigma_a = s0 / (tf - 1.0);
        let qf = q as f64;
        let m = qf * (tf - qf + 1.0) * (1.0 - qf / tf);
        let sigma_c = (q..=t).map(|k| level[k] - level[k - q] - qf * mu).map(|d| d * d).sum::<f64>() / m;
        let vr = sigma_c / sigma_a;
        let theta: f64 = (1..q)
            .map(|j| {
                let w = 2.0 * (qf - j as f64) / qf;
                let delta = tf * (j..t).map(|k| e2[k] * e2[k - j]).sum::<f64>() / (s0 * s0);
                w * w * delta
            })
            .sum();
        if theta.is_nan() || theta <= 0.0 {
            return Err(TestError::Precondition("variance_ratio: the robust variance θ̂ is zero".into()));
        }
        let z = tf.sqrt() * (vr - 1.0) / theta.sqrt();
        Ok(TestResult {
            statistic: z,
            p_value: Some(2.0 * dist::norm_sf(z.abs())),
            interval: None,
            effect: Some(vr),
            df: None,
            n: t,
            notes: vec![format!("q {}", q), format!("VR(q) {}", vr), format!("robust variance θ̂ {}", theta)],
        })
    }
}
