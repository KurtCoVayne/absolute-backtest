//! `ljung_box` (Q-4.4): the series has no autocorrelation up to lag m.
//! Ljung and Box's (1978) portmanteau statistic
//! Q(m) = n (n + 2) Σ_{k ≤ m} ρ̂_k² / (n − k), the autocorrelations about
//! the sample mean, referred to χ²(m), the asymptotic distribution under
//! the null of independent draws (Dependence::Asymptotic). Its known answer
//! is the Box–Jenkins Series A.

use crate::tests::dist;
use crate::tests::framework::{Dependence, InputSpec, ParamKind, ParamSpec, Params, Shape, Table, Test, TestError, TestResult};
use crate::tests::resample::autocorrelation;

use super::timeseries::{series, usize_param};

pub struct LjungBox;

const INPUTS: [InputSpec; 1] = [InputSpec {
    name: "series",
    shape: Shape::Series,
    columns: &["x"],
}];

const PARAMS: [ParamSpec; 1] = [ParamSpec {
    name: "lags",
    kind: ParamKind::Int,
    default: None,
    range: Some((1.0, 1e6)),
    doc: "the largest lag m (default min(10, ⌊n/5⌋))",
}];

impl Test for LjungBox {
    fn name(&self) -> &str {
        "ljung_box"
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
        "the series has no autocorrelation up to lag m"
    }
    fn dependence(&self) -> Dependence {
        Dependence::Asymptotic
    }
    /// Q(m) against χ²(m) with m degrees of freedom; the effect is the first
    /// autocorrelation, and the notes carry every ρ̂_k.
    fn run(&self, tables: &[Table], params: &Params, _seed: u64) -> Result<TestResult, TestError> {
        let x = series(tables, "ljung_box", 3)?;
        let n = x.len();
        let m = match usize_param(self, params, "lags")? {
            Some(m) => m,
            None => (n / 5).min(10),
        };
        if m == 0 || m >= n {
            return Err(TestError::Precondition(format!("ljung_box needs 1 ≤ m < n, not m = {} with n = {}", m, n)));
        }
        let mean = x.iter().sum::<f64>() / n as f64;
        if x.iter().all(|v| *v == mean) {
            return Err(TestError::Precondition("ljung_box: the series is constant".into()));
        }
        let rho: Vec<f64> = (1..=m).map(|k| autocorrelation(x, k)).collect();
        let nf = n as f64;
        let q = nf * (nf + 2.0) * rho.iter().enumerate().map(|(i, r)| r * r / (nf - (i + 1) as f64)).sum::<f64>();
        let notes = vec![format!("lags {}", m), format!("autocorrelations {}", rho.iter().map(|r| r.to_string()).collect::<Vec<_>>().join(", "))];
        Ok(TestResult {
            statistic: q,
            p_value: Some(dist::chi2_sf(q, m as f64)),
            interval: None,
            effect: Some(rho[0]),
            df: Some(m as f64),
            n,
            notes,
        })
    }
}
