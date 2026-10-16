//! `cost_realism` (Q-4.4): whether the cost model's slippage is what the
//! fills realise, on live-sim or live-broker fills priced by the model as
//! well: the paired t of realised − modelled slippage over the fills in
//! time order. One fill's slippage is correlated with the next one's (the
//! same book, the same session's volatility), so the standard error is
//! Newey–West's, with the factor n/(n − 1) of `hac::newey_west_mean`
//! under which zero lags give the textbook paired t; the t is read
//! against Student's t with n − 1 degrees of freedom.

use crate::tests::dist;
use crate::tests::framework::{param_f64, param_usize, Dependence, InputSpec, Param, ParamKind, ParamSpec, Params, Shape, Table, Test, TestError, TestResult};
use crate::tests::hac;

pub struct CostRealism;

const INPUTS: [InputSpec; 1] = [InputSpec {
    name: "fills",
    shape: Shape::Series,
    columns: &["realised", "modelled"],
}];

const PARAMS: [ParamSpec; 2] = [
    ParamSpec {
        name: "lags",
        kind: ParamKind::Int,
        default: None,
        range: Some((0.0, 1e6)),
        doc: "Newey–West lags of the differences (default ⌊4 (n/100)^(2/9)⌋); 0 is the textbook paired t",
    },
    ParamSpec {
        name: "level",
        kind: ParamKind::Float,
        default: Some(Param::Float(0.95)),
        range: Some((0.5, 0.9999)),
        doc: "the confidence level of the interval on the mean difference",
    },
];

impl Test for CostRealism {
    fn name(&self) -> &str {
        "cost_realism"
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
        "the modelled slippage equals the realised on average"
    }
    fn dependence(&self) -> Dependence {
        Dependence::NeweyWest { lags: None }
    }
    /// One row per fill with its `realised` and `modelled` slippage in one
    /// unit (a fill with either missing is left out), in time order: by
    /// `time` and then `entity` when the table has them, else as given.
    /// The statistic is the t of the mean of realised − modelled, the
    /// effect that mean (positive when the model understates the cost),
    /// the interval the mean's at `level`; `n` is the number of fills.
    fn run(&self, tables: &[Table], params: &Params, _seed: u64) -> Result<TestResult, TestError> {
        let t = tables.first().ok_or_else(|| TestError::Input("cost_realism takes one table of fills".into()))?;
        let realised = t.column("realised")?;
        let modelled = t.column("modelled")?;
        let len = realised.len();
        if modelled.len() != len || t.time.as_ref().is_some_and(|v| v.len() != len) || t.entity.as_ref().is_some_and(|v| v.len() != len) {
            return Err(TestError::Input("the fills' columns, `time` and `entity` differ in length".into()));
        }
        let lags = match param_usize(self, params, "lags") {
            Some(l) => Some(l),
            None if params.contains_key("lags") => return Err(TestError::Input("`lags` is a non-negative integer".into())),
            None => None,
        };
        let level = param_f64(self, params, "level")
            .filter(|l| *l > 0.0 && *l < 1.0)
            .ok_or_else(|| TestError::Input("`level` is a number between 0 and 1".into()))?;
        let mut order: Vec<usize> = (0..len).filter(|&i| realised[i].is_finite() && modelled[i].is_finite()).collect();
        if let Some(time) = &t.time {
            let entity = t.entity.as_deref();
            order.sort_by_key(|&i| (time[i], entity.map_or(0, |e| e[i])));
        }
        let n = order.len();
        if n < 2 {
            return Err(TestError::Precondition(format!("{} fills with both slippages; the paired t needs two", n)));
        }
        let d: Vec<f64> = order.iter().map(|&i| realised[i] - modelled[i]).collect();
        let lags = lags.unwrap_or_else(|| hac::newey_west_lags(n));
        let (mean, se) = hac::newey_west_mean(&d, lags);
        let df = (n - 1) as f64;
        let (statistic, p_value, interval) = if se > 0.0 {
            let q = dist::t_ppf(0.5 + level / 2.0, df);
            (mean / se, dist::t_two_sided(mean / se, df), (mean - q * se, mean + q * se, level))
        } else if mean == 0.0 {
            (0.0, 1.0, (0.0, 0.0, level))
        } else {
            return Err(TestError::Precondition(format!(
                "every fill's slippage differs from the model's by {}: the difference has no variance",
                mean
            )));
        };
        let average = |c: &[f64]| order.iter().map(|&i| c[i]).sum::<f64>() / n as f64;
        let sd = (d.iter().map(|v| (v - mean) * (v - mean)).sum::<f64>() / df).sqrt();
        Ok(TestResult {
            statistic,
            p_value: Some(p_value),
            interval: Some(interval),
            effect: Some(mean),
            df: Some(df),
            n,
            notes: vec![
                format!("mean realised {}, mean modelled {}", average(realised), average(modelled)),
                format!("standard deviation of the differences {}", sd),
                format!("Newey–West standard error {} with {} lags", se, lags),
                format!("{} fills with a missing slippage left out", len - n),
            ],
        })
    }
}
