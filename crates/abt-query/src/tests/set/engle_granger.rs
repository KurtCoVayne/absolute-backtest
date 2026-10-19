//! `engle_granger` (Q-4.4): the series are not cointegrated. Engle and
//! Granger's (1987) two steps: the cointegrating regression of one series
//! on a constant (and a linear trend) and the others, then the augmented
//! Dickey–Fuller τ of its residuals, without deterministic terms and with
//! the lag order chosen by AIC or fixed. τ is referred to the distribution
//! of residual-based tests for N series (Dependence::Asymptotic):
//! MacKinnon's (2010) critical values at T = n − 1, the convention of
//! Stata's `egranger` and statsmodels' `coint`, and MacKinnon's (1994)
//! approximate asymptotic p-value, which exists for N ≤ 6; for more series
//! the critical values alone judge the test.

use crate::tests::framework::{Dependence, InputSpec, ParamKind, ParamSpec, Params, Shape, Table, Test, TestError, TestResult};
use crate::tests::regression::{ols, Covariance};

use super::timeseries::{critical_note, dickey_fuller, lag_choice, mackinnon_critical, mackinnon_p, system, text_param, Deterministic, LagChoice};

pub struct EngleGranger;

const INPUTS: [InputSpec; 1] = [InputSpec {
    name: "series",
    shape: Shape::Series,
    columns: &["y", "x..."],
}];

const PARAMS: [ParamSpec; 4] = [
    ParamSpec {
        name: "y",
        kind: ParamKind::Text,
        default: None,
        range: None,
        doc: "the column regressed on the others (default `y`, or the first column by name)",
    },
    ParamSpec {
        name: "trend",
        kind: ParamKind::Text,
        default: None,
        range: None,
        doc: "`c` for a constant in the cointegrating regression (the default), `ct` for a constant and a linear trend",
    },
    ParamSpec {
        name: "lags",
        kind: ParamKind::Int,
        default: None,
        range: Some((0.0, 1e6)),
        doc: "a fixed number of lagged differences in the residuals' regression; unset, AIC chooses it",
    },
    ParamSpec {
        name: "max_lags",
        kind: ParamKind::Int,
        default: None,
        range: Some((0.0, 1e6)),
        doc: "the largest order AIC considers (default ⌊12 (n/100)^(1/4)⌋, at most about half the sample)",
    },
];

impl Test for EngleGranger {
    fn name(&self) -> &str {
        "engle_granger"
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
        "the series are not cointegrated"
    }
    fn dependence(&self) -> Dependence {
        Dependence::Asymptotic
    }
    /// The table's columns are the series on one time axis. τ of the
    /// residuals with its p-value (small values reject); the effect is
    /// 1 + γ̂, the residuals' persistence; the notes carry the cointegrating
    /// coefficients, the lag order and the critical values.
    fn run(&self, tables: &[Table], params: &Params, _seed: u64) -> Result<TestResult, TestError> {
        let columns = system(tables, "engle_granger", 6)?;
        let k = columns.len();
        if k < 2 {
            return Err(TestError::Input(format!("engle_granger needs two or more series, not {}", k)));
        }
        if k > 12 {
            return Err(TestError::Precondition(format!("MacKinnon (2010) tabulates up to twelve series, not {}", k)));
        }
        let dependent = match text_param(self, params, "y")? {
            Some(name) => name,
            None if columns.iter().any(|(n, _)| *n == "y") => "y",
            None => columns[0].0,
        };
        let y = columns
            .iter()
            .find(|(n, _)| *n == dependent)
            .map(|(_, c)| *c)
            .ok_or_else(|| TestError::Input(format!("engle_granger: no column `{}`", dependent)))?;
        let det = Deterministic::from_trend(self, params)?;
        let n = y.len();
        let mut regressors = det.columns(n);
        let mut names: Vec<&str> = ["const", "trend"][..det.terms()].to_vec();
        for (name, c) in columns.iter().filter(|(name, _)| *name != dependent) {
            regressors.push(c.to_vec());
            names.push(name);
        }
        let fit = ols(&regressors, y, &Covariance::Iid).map_err(TestError::Numeric)?;
        let choice = lag_choice(self, params, n, Deterministic::None)?;
        let df = dickey_fuller(&fit.residuals, Deterministic::None, choice)?;
        let t = (n - 1) as f64;
        let cv = mackinnon_critical(k, det, Some(t)).ok_or_else(|| TestError::Precondition(format!("no MacKinnon (2010) surface for {} series", k)))?;
        let p = mackinnon_p(df.tau, k, det);
        let mut notes = vec![format!(
            "cointegrating regression of `{}`: {}",
            dependent,
            names.iter().zip(&fit.beta).map(|(n, b)| format!("{} {}", n, b)).collect::<Vec<_>>().join(", ")
        )];
        notes.push(match choice {
            LagChoice::Fixed(p) => format!("lags {} (fixed)", p),
            LagChoice::Aic(max) => format!("lags {} (by AIC up to {})", df.lags, max),
        });
        notes.push(format!("{} (N = {}, `{}`, T = {})", critical_note(&cv), k, det.name(), n - 1));
        if p.is_none() {
            notes.push(format!("MacKinnon (1994) fits the p-value for up to six series, not {}: the critical values judge the test", k));
        }
        Ok(TestResult {
            statistic: df.tau,
            p_value: p,
            interval: None,
            effect: Some(1.0 + df.gamma),
            df: None,
            n,
            notes,
        })
    }
}
