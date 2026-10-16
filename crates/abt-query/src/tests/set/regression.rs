//! `regression` (Q-4.4): OLS of a response on regressors with HAC standard
//! errors; the null is that every slope is zero. The engine behind `ic`,
//! `half_life` and `fama_macbeth`; its known answers are the NIST StRD
//! linear regression datasets to their certified digits.

use crate::tests::dist;
use crate::tests::framework::{param_usize, Dependence, InputSpec, Param, ParamKind, ParamSpec, Params, Shape, Table, Test, TestError, TestResult};
use crate::tests::regression::{ols, Covariance};

pub struct Regression;

const INPUTS: [InputSpec; 1] = [InputSpec {
    name: "rows",
    shape: Shape::Rows,
    columns: &["y", "x..."],
}];

const PARAMS: [ParamSpec; 2] = [
    ParamSpec {
        name: "lags",
        kind: ParamKind::Int,
        default: None,
        range: Some((0.0, 1e6)),
        doc: "Newey–West lags (default ⌊4 (n/100)^(2/9)⌋); 0 is White's heteroskedasticity-robust covariance",
    },
    ParamSpec {
        name: "intercept",
        kind: ParamKind::Bool,
        default: Some(Param::Bool(true)),
        range: None,
        doc: "include an intercept",
    },
];

impl Test for Regression {
    fn name(&self) -> &str {
        "regression"
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
        "every slope coefficient is zero"
    }
    fn dependence(&self) -> Dependence {
        Dependence::NeweyWest { lags: None }
    }
    /// The table's `y` column on its other columns, in name order; the
    /// statistic is the Wald χ² of the slopes under the HAC covariance, the
    /// effect the R², and the notes carry each coefficient with its HAC
    /// standard error.
    fn run(&self, tables: &[Table], params: &Params, _seed: u64) -> Result<TestResult, TestError> {
        let t = tables.first().ok_or_else(|| TestError::Input("regression takes one table".into()))?;
        let y = t.column("y")?.to_vec();
        let intercept = !matches!(params.get("intercept"), Some(Param::Bool(false)));
        let mut columns: Vec<Vec<f64>> = Vec::new();
        let mut names: Vec<String> = Vec::new();
        if intercept {
            columns.push(vec![1.0; y.len()]);
            names.push("const".into());
        }
        for (name, c) in t.columns.iter().filter(|(n, _)| n.as_str() != "y") {
            columns.push(c.clone());
            names.push(name.clone());
        }
        let lags = param_usize(self, params, "lags");
        let fit = ols(&columns, &y, &Covariance::NeweyWest(lags)).map_err(TestError::Numeric)?;
        let k0 = usize::from(intercept);
        let k = fit.p - k0;
        if k == 0 {
            return Err(TestError::Input("regression needs a regressor besides the intercept".into()));
        }
        // Wald: b' V⁻¹ b over the slopes.
        let slopes: Vec<f64> = fit.beta[k0..].to_vec();
        let mut v = crate::tests::linalg::Matrix::zeros(k, k);
        for i in 0..k {
            for j in 0..k {
                v[(i, j)] = fit.covariance[(k0 + i, k0 + j)];
            }
        }
        let l = crate::tests::linalg::cholesky(&v).map_err(TestError::Numeric)?;
        // Solve L z = b; W = z'z.
        let mut z = vec![0.0; k];
        for i in 0..k {
            let mut s = slopes[i];
            for j in 0..i {
                s -= l[(i, j)] * z[j];
            }
            z[i] = s / l[(i, i)];
        }
        let w: f64 = z.iter().map(|x| x * x).sum();
        let notes = names.iter().enumerate().map(|(j, n)| format!("{} {} (se {})", n, fit.beta[j], fit.se[j])).collect();
        Ok(TestResult {
            statistic: w,
            p_value: Some(dist::chi2_sf(w, k as f64)),
            interval: None,
            effect: Some(fit.r2),
            df: Some(k as f64),
            n: fit.n,
            notes,
        })
    }
}
