//! `fama_macbeth` (Q-4.4): the two-pass estimator of Fama and MacBeth
//! (1973). At every bar, a cross-sectional regression of the returns `y`
//! on the characteristics (every other column, and an intercept); then
//! the time-series mean of each coefficient, its premium, with the
//! Newey–West standard error of that mean (`hac::newey_west_mean`, whose
//! factor T/(T − 1) makes zero lags Fama and MacBeth's own s/√T, and L
//! lags the Newey–West adjustment of Stata's `xtfmb`), the t read against
//! Student's t with T − 1 degrees of freedom. The null is that the named
//! premium is zero.

use crate::tests::dist;
use crate::tests::framework::{param, param_f64, param_usize, Dependence, InputSpec, Param, ParamKind, ParamSpec, Params, Shape, Table, Test, TestError, TestResult};
use crate::tests::hac;
use crate::tests::regression::{ols, Covariance};

pub struct FamaMacBeth;

const INPUTS: [InputSpec; 1] = [InputSpec {
    name: "panel",
    shape: Shape::Panel,
    columns: &["y", "x..."],
}];

const PARAMS: [ParamSpec; 4] = [
    ParamSpec {
        name: "factor",
        kind: ParamKind::Text,
        default: None,
        range: None,
        doc: "the regressor whose premium is tested, `const` for the intercept (default the only regressor; required when there are several)",
    },
    ParamSpec {
        name: "lags",
        kind: ParamKind::Int,
        default: None,
        range: Some((0.0, 1e6)),
        doc: "Newey–West lags of the premium series (default ⌊4 (T/100)^(2/9)⌋); 0 is Fama and MacBeth's own standard error",
    },
    ParamSpec {
        name: "intercept",
        kind: ParamKind::Bool,
        default: Some(Param::Bool(true)),
        range: None,
        doc: "include an intercept in every cross-sectional regression",
    },
    ParamSpec {
        name: "level",
        kind: ParamKind::Float,
        default: Some(Param::Float(0.95)),
        range: Some((0.5, 0.9999)),
        doc: "the confidence level of the interval on the premium",
    },
];

impl Test for FamaMacBeth {
    fn name(&self) -> &str {
        "fama_macbeth"
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
        "the risk premium of the named characteristic is zero"
    }
    fn dependence(&self) -> Dependence {
        Dependence::NeweyWest { lags: None }
    }
    /// The panel's `y` on its other columns, in name order, bar by bar;
    /// a row with a missing value leaves its bar's regression, and a bar
    /// with no more rows than coefficients, or whose design is singular,
    /// leaves the premium series. The statistic is the t of the `factor`'s
    /// premium, the effect the premium, the interval the premium's at
    /// `level`; `n` is the number of bars in the series, and the notes
    /// carry every coefficient's premium with its standard error and t.
    fn run(&self, tables: &[Table], params: &Params, _seed: u64) -> Result<TestResult, TestError> {
        let t = tables.first().ok_or_else(|| TestError::Input("fama_macbeth takes one panel".into()))?;
        let y = t.column("y")?;
        let regressors: Vec<&String> = t.columns.keys().filter(|n| n.as_str() != "y").collect();
        if regressors.is_empty() {
            return Err(TestError::Input("fama_macbeth needs a characteristic besides `y`".into()));
        }
        let intercept = !matches!(param(self, params, "intercept"), Some(Param::Bool(false)));
        if intercept && t.columns.contains_key("const") {
            return Err(TestError::Input("a column named `const` would share its name with the intercept's premium".into()));
        }
        let mut labels: Vec<String> = if intercept { vec!["const".to_string()] } else { Vec::new() };
        labels.extend(regressors.iter().map(|n| n.to_string()));
        let factor = match param(self, params, "factor") {
            Some(Param::Text(f)) => labels
                .iter()
                .position(|l| l == f)
                .ok_or_else(|| TestError::Input(format!("no regressor `{}` among {}", f, labels.join(", "))))?,
            Some(other) => return Err(TestError::Input(format!("`factor` names a regressor, not {}", other))),
            None if regressors.len() == 1 => labels.len() - 1,
            None => return Err(TestError::Input(format!("name the premium to test with `factor`: one of {}", labels.join(", ")))),
        };
        let lags = match param_usize(self, params, "lags") {
            Some(l) => Some(l),
            None if params.contains_key("lags") => return Err(TestError::Input("`lags` is a non-negative integer".into())),
            None => None,
        };
        let level = param_f64(self, params, "level")
            .filter(|l| *l > 0.0 && *l < 1.0)
            .ok_or_else(|| TestError::Input("`level` is a number between 0 and 1".into()))?;
        let columns: Vec<&[f64]> = regressors.iter().map(|n| t.column(n)).collect::<Result<_, _>>()?;
        let bars = t.bars()?;
        let p = labels.len();
        let mut premia: Vec<Vec<f64>> = (0..p).map(|_| Vec::with_capacity(bars.len())).collect();
        let mut r2 = Vec::with_capacity(bars.len());
        let mut missing = 0usize;
        for (_, rows) in &bars {
            let keep: Vec<usize> = rows.iter().copied().filter(|&i| y[i].is_finite() && columns.iter().all(|c| c[i].is_finite())).collect();
            missing += rows.len() - keep.len();
            if keep.len() <= p {
                continue;
            }
            let mut design: Vec<Vec<f64>> = Vec::with_capacity(p);
            if intercept {
                design.push(vec![1.0; keep.len()]);
            }
            design.extend(columns.iter().map(|c| keep.iter().map(|&i| c[i]).collect::<Vec<f64>>()));
            let returns: Vec<f64> = keep.iter().map(|&i| y[i]).collect();
            if let Ok(fit) = ols(&design, &returns, &Covariance::Iid) {
                for (series, b) in premia.iter_mut().zip(&fit.beta) {
                    series.push(*b);
                }
                r2.push(fit.r2);
            }
        }
        let bars_used = r2.len();
        if bars_used < 2 {
            return Err(TestError::Precondition(format!(
                "{} of {} bars have a cross-sectional regression (more rows than the {} coefficients and a design of full rank); the premium's t needs two",
                bars_used,
                bars.len(),
                p
            )));
        }
        let lags = lags.unwrap_or_else(|| hac::newey_west_lags(bars_used));
        let df = (bars_used - 1) as f64;
        let estimates: Vec<(f64, f64)> = premia.iter().map(|s| hac::newey_west_mean(s, lags)).collect();
        let (mean, se) = estimates[factor];
        let (statistic, p_value, interval) = if se > 0.0 {
            let q = dist::t_ppf(0.5 + level / 2.0, df);
            (mean / se, dist::t_two_sided(mean / se, df), (mean - q * se, mean + q * se, level))
        } else if mean == 0.0 {
            (0.0, 1.0, (0.0, 0.0, level))
        } else {
            return Err(TestError::Precondition(format!(
                "the premium of `{}` is {} at every bar: its standard error is zero",
                labels[factor], mean
            )));
        };
        let mut notes: Vec<String> = labels.iter().zip(&estimates).map(|(l, (m, s))| format!("{} premium {} (se {}, t {})", l, m, s, m / s)).collect();
        notes.push(format!("{} cross-sections in the premium series, {} left out", bars_used, bars.len() - bars_used));
        notes.push(format!("{} rows with a missing value left out of their bar", missing));
        notes.push(format!("mean cross-sectional R² {}", r2.iter().sum::<f64>() / bars_used as f64));
        notes.push(format!("Newey–West lags {}", lags));
        Ok(TestResult {
            statistic,
            p_value: Some(p_value),
            interval: Some(interval),
            effect: Some(mean),
            df: Some(df),
            n: bars_used,
            notes,
        })
    }
}
