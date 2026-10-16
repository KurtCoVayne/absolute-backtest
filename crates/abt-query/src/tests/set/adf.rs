//! `adf` (Q-4.4): the series has a unit root. The augmented Dickey–Fuller
//! test (Dickey and Fuller 1979; Said and Dickey 1984): Δx_t regressed on a
//! constant (and a linear trend), x_{t−1} and p lagged differences, the
//! order p chosen by AIC up to a stated maximum (Schwert's
//! ⌊12 (n/100)^(1/4)⌋ by default) or fixed. τ, the t-statistic of x_{t−1},
//! is referred to the Dickey–Fuller distribution, whose asymptotic theory
//! absorbs the serial correlation the lags model (Dependence::Asymptotic):
//! MacKinnon's (1994) approximate asymptotic p-value, and MacKinnon's
//! (2010) critical values at the regression's number of observations.

use crate::tests::framework::{Dependence, InputSpec, ParamKind, ParamSpec, Params, Shape, Table, Test, TestError, TestResult};

use super::timeseries::{critical_note, dickey_fuller, lag_choice, mackinnon_critical, mackinnon_p, series, Deterministic, LagChoice};

pub struct Adf;

const INPUTS: [InputSpec; 1] = [InputSpec {
    name: "series",
    shape: Shape::Series,
    columns: &["x"],
}];

const PARAMS: [ParamSpec; 3] = [
    ParamSpec {
        name: "trend",
        kind: ParamKind::Text,
        default: None,
        range: None,
        doc: "`c` for a constant (the default), `ct` for a constant and a linear trend",
    },
    ParamSpec {
        name: "lags",
        kind: ParamKind::Int,
        default: None,
        range: Some((0.0, 1e6)),
        doc: "a fixed number of lagged differences; unset, AIC chooses it",
    },
    ParamSpec {
        name: "max_lags",
        kind: ParamKind::Int,
        default: None,
        range: Some((0.0, 1e6)),
        doc: "the largest order AIC considers (default ⌊12 (n/100)^(1/4)⌋, at most about half the sample)",
    },
];

impl Test for Adf {
    fn name(&self) -> &str {
        "adf"
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
        "the series has a unit root"
    }
    fn dependence(&self) -> Dependence {
        Dependence::Asymptotic
    }
    /// τ with its MacKinnon p-value (small values reject the unit root);
    /// the effect is 1 + γ̂, the sum of the level's autoregressive
    /// coefficients; n is the regression's number of observations, and the
    /// notes carry the lag order and the critical values.
    fn run(&self, tables: &[Table], params: &Params, _seed: u64) -> Result<TestResult, TestError> {
        let x = series(tables, "adf", 6)?;
        let det = Deterministic::from_trend(self, params)?;
        let choice = lag_choice(self, params, x.len(), det)?;
        let df = dickey_fuller(x, det, choice)?;
        let mut notes = vec![match choice {
            LagChoice::Fixed(p) => format!("lags {} (fixed)", p),
            LagChoice::Aic(max) => format!("lags {} (by AIC up to {})", df.lags, max),
        }];
        notes.push(format!("deterministic terms `{}`", det.name()));
        if let Some(cv) = mackinnon_critical(1, det, Some(df.nobs as f64)) {
            notes.push(critical_note(&cv));
        }
        Ok(TestResult {
            statistic: df.tau,
            p_value: mackinnon_p(df.tau, 1, det),
            interval: None,
            effect: Some(1.0 + df.gamma),
            df: None,
            n: df.nobs,
            notes,
        })
    }
}
