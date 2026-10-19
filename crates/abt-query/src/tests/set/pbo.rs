//! `pbo` (Q-4.4): the probability of backtest overfitting by
//! combinatorially symmetric cross-validation (Bailey, Borwein, López de
//! Prado and Zhu 2017), as the metrics library computes it. The input is a
//! grid's per-period returns, one column per trial and rows in time order.
//! The rows are cut into S blocks; each of the C(S, S/2) halves of the
//! blocks is an in-sample set and the other half its out-of-sample set; the
//! trial best in sample by Sharpe ratio is ranked among all trials out of
//! sample, its relative rank ω = rank/(N + 1) giving the logit
//! λ = ln(ω/(1 − ω)); and PBO is the share of splits with λ ≤ 0, where the
//! in-sample best does no better than the median out of sample.
//!
//! Every split is enumerated, so the dependence treatment is `exact`
//! (combinatorial). PBO estimates the probability of overfitting itself,
//! not the probability of a statistic under the null, so there is no
//! p-value: a claim reads the statistic (`pbo on grid main expect
//! statistic < 0.5`, Q-5.2).

use crate::metrics;
use crate::tests::framework::{Dependence, InputSpec, ParamKind, ParamSpec, Params, Shape, Table, Test, TestError, TestResult};
use crate::tests::set::samples;

pub struct Pbo;

const INPUTS: [InputSpec; 1] = [InputSpec {
    name: "grid",
    shape: Shape::Series,
    columns: &["trial..."],
}];

const PARAMS: [ParamSpec; 1] = [ParamSpec {
    name: "partitions",
    kind: ParamKind::Int,
    default: None,
    range: Some((2.0, 20.0)),
    doc: "the number S of blocks, even (default 16, or 8 when there are fewer than 32 rows, as the study computes it)",
}];

impl Test for Pbo {
    fn name(&self) -> &str {
        "pbo"
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
        "the trial chosen in sample is not overfit: out of sample it ranks above the median"
    }
    fn dependence(&self) -> Dependence {
        Dependence::Exact
    }
    /// The statistic is PBO; the effect the mean logit of the in-sample
    /// best's out-of-sample rank (positive when the choice generalises); no
    /// p-value.
    fn run(&self, tables: &[Table], params: &Params, _seed: u64) -> Result<TestResult, TestError> {
        let t = samples::table(tables)?;
        if t.columns.len() < 2 {
            return Err(TestError::Precondition(format!("{} trial; PBO compares at least two", t.columns.len())));
        }
        let rows_n = t.len();
        let mut columns = Vec::with_capacity(t.columns.len());
        for (name, c) in &t.columns {
            if c.len() != rows_n {
                return Err(TestError::Input(format!("trial `{}` has {} rows, the first {}", name, c.len(), rows_n)));
            }
            columns.push(samples::finite(c, &format!("trial `{}`", name))?);
        }
        let rows: Vec<Vec<f64>> = (0..rows_n).map(|i| columns.iter().map(|c| c[i]).collect()).collect();
        let found = match samples::count(self, params, "partitions")? {
            Some(s) if s % 2 == 1 => return Err(TestError::Input(format!("`partitions` is {}; CSCV halves the blocks, so it is even", s))),
            Some(s) => metrics::pbo_cscv(&rows, s),
            None => metrics::pbo_cscv(&rows, 16).or_else(|| metrics::pbo_cscv(&rows, 8)),
        };
        let p = found.ok_or_else(|| TestError::Precondition(format!("{} rows are too few for two rows in each block", rows_n)))?;
        let used = rows_n / p.partitions * p.partitions;
        Ok(TestResult {
            statistic: p.pbo,
            p_value: None,
            interval: None,
            effect: Some(p.mean_logit),
            df: None,
            n: used,
            notes: vec![
                format!("{} splits of {} blocks of {} rows, {} trials", p.combinations, p.partitions, used / p.partitions, p.trials),
                format!("mean logit of the in-sample best's out-of-sample rank {}", p.mean_logit),
            ],
        })
    }
}
