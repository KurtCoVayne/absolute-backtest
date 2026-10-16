//! `arch_lm` (Q-4.4): the series has no ARCH effects up to lag m. Engle's
//! (1982) Lagrange multiplier statistic: the squared demeaned series ε²_t
//! regressed on a constant and ε²_{t−1}, …, ε²_{t−m} (the regression
//! engine), LM = (n − m) R² over its n − m observations, referred to χ²(m),
//! its asymptotic distribution under conditional homoskedasticity
//! (Dependence::Asymptotic).

use crate::tests::dist;
use crate::tests::framework::{Dependence, InputSpec, Param, ParamKind, ParamSpec, Params, Shape, Table, Test, TestError, TestResult};
use crate::tests::regression::{ols, Covariance};

use super::timeseries::{series, usize_param};

pub struct ArchLm;

const INPUTS: [InputSpec; 1] = [InputSpec {
    name: "series",
    shape: Shape::Series,
    columns: &["x"],
}];

const PARAMS: [ParamSpec; 1] = [ParamSpec {
    name: "lags",
    kind: ParamKind::Int,
    default: Some(Param::Int(12)),
    range: Some((1.0, 1e6)),
    doc: "the number m of lagged squares in the auxiliary regression",
}];

impl Test for ArchLm {
    fn name(&self) -> &str {
        "arch_lm"
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
        "the series has no autoregressive conditional heteroskedasticity up to lag m"
    }
    fn dependence(&self) -> Dependence {
        Dependence::Asymptotic
    }
    /// LM against χ²(m); the effect is the sum of the lagged squares'
    /// coefficients (how persistent the variance is), and n is the
    /// auxiliary regression's number of observations.
    fn run(&self, tables: &[Table], params: &Params, _seed: u64) -> Result<TestResult, TestError> {
        let x = series(tables, "arch_lm", 3)?;
        let m = usize_param(self, params, "lags")?.unwrap_or(12);
        if m == 0 {
            return Err(TestError::Input("arch_lm: lags is at least 1".into()));
        }
        let n = x.len();
        if n <= 2 * m + 1 {
            return Err(TestError::Precondition(format!("arch_lm with {} lags needs more than {} observations, not {}", m, 2 * m + 1, n)));
        }
        let mean = x.iter().sum::<f64>() / n as f64;
        let e2: Vec<f64> = x.iter().map(|v| (v - mean) * (v - mean)).collect();
        let y = e2[m..].to_vec();
        if y.iter().all(|v| *v == y[0]) {
            return Err(TestError::Precondition("arch_lm: the squared series is constant".into()));
        }
        let rows = n - m;
        let mut columns = vec![vec![1.0; rows]];
        for j in 1..=m {
            columns.push(e2[m - j..n - j].to_vec());
        }
        let fit = ols(&columns, &y, &Covariance::Iid).map_err(TestError::Numeric)?;
        let lm = rows as f64 * fit.r2;
        let persistence: f64 = fit.beta[1..].iter().sum();
        Ok(TestResult {
            statistic: lm,
            p_value: Some(dist::chi2_sf(lm, m as f64)),
            interval: None,
            effect: Some(persistence),
            df: Some(m as f64),
            n: rows,
            notes: vec![format!("lags {}", m), format!("R² {}", fit.r2)],
        })
    }
}
