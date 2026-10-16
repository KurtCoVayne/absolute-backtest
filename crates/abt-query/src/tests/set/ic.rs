//! `ic` (Q-4.4): the information coefficient of a feature (Grinold and
//! Kahn 2000), the mean over bars of the cross-sectional Spearman rank
//! correlation of the feature with the forward returns; the null is that
//! it is zero. The IC series is a time series of cross-sectional
//! statistics, as Fama and MacBeth's premia are, and is tested as they
//! are: its mean against its Newey–West standard error with the factor
//! T/(T − 1) (`hac::newey_west_mean`), the t read against Student's t
//! with T − 1 degrees of freedom.

use crate::tests::dist;
use crate::tests::framework::{param_f64, param_usize, Dependence, InputSpec, Param, ParamKind, ParamSpec, Params, Shape, Table, Test, TestError, TestResult};
use crate::tests::hac;

pub struct Ic;

const INPUTS: [InputSpec; 1] = [InputSpec {
    name: "panel",
    shape: Shape::Panel,
    columns: &["feature", "forward"],
}];

const PARAMS: [ParamSpec; 4] = [
    ParamSpec {
        name: "lags",
        kind: ParamKind::Int,
        default: None,
        range: Some((0.0, 1e6)),
        doc: "Newey–West lags of the IC series (default the larger of ⌊4 (T/100)^(2/9)⌋ and horizon − 1)",
    },
    ParamSpec {
        name: "horizon",
        kind: ParamKind::Int,
        default: Some(Param::Int(1)),
        range: Some((1.0, 1e6)),
        doc: "the bars the forward returns span; overlapping horizons make the IC series a moving average of order horizon − 1, which the default lags cover",
    },
    ParamSpec {
        name: "min_entities",
        kind: ParamKind::Int,
        default: Some(Param::Int(3)),
        range: Some((3.0, 1e9)),
        doc: "the fewest entities with both values a bar needs to enter the IC series",
    },
    ParamSpec {
        name: "level",
        kind: ParamKind::Float,
        default: Some(Param::Float(0.95)),
        range: Some((0.5, 0.9999)),
        doc: "the confidence level of the interval on the mean IC",
    },
];

impl Test for Ic {
    fn name(&self) -> &str {
        "ic"
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
        "the information coefficient, the mean cross-sectional rank correlation of the feature with the forward returns, is zero"
    }
    fn dependence(&self) -> Dependence {
        Dependence::NeweyWest { lags: None }
    }
    /// At every bar, the Spearman correlation of `feature` with `forward`
    /// over the entities with both values finite (ties at their mean rank);
    /// a bar with fewer than `min_entities` such entities, or on which
    /// either column is constant, is left out. The statistic is the
    /// Newey–West t of the mean IC, the effect the mean IC, the interval
    /// the mean IC's at `level`; `n` is the number of bars in the series.
    fn run(&self, tables: &[Table], params: &Params, _seed: u64) -> Result<TestResult, TestError> {
        let t = tables.first().ok_or_else(|| TestError::Input("ic takes one panel".into()))?;
        let feature = t.column("feature")?;
        let forward = t.column("forward")?;
        let bars = t.bars()?;
        let horizon = count(self, params, "horizon")?.unwrap_or(1);
        let min_entities = count(self, params, "min_entities")?.unwrap_or(3);
        if horizon == 0 || min_entities < 3 {
            return Err(TestError::Input("`horizon` is at least 1 and `min_entities` at least 3".into()));
        }
        let level = param_f64(self, params, "level")
            .filter(|l| *l > 0.0 && *l < 1.0)
            .ok_or_else(|| TestError::Input("`level` is a number between 0 and 1".into()))?;
        let mut ic = Vec::with_capacity(bars.len());
        let mut entities = 0usize;
        for (_, rows) in &bars {
            let (f, r): (Vec<f64>, Vec<f64>) = rows.iter().map(|&i| (feature[i], forward[i])).filter(|(f, r)| f.is_finite() && r.is_finite()).unzip();
            if f.len() < min_entities {
                continue;
            }
            if let Some(rho) = spearman(&f, &r) {
                ic.push(rho);
                entities += f.len();
            }
        }
        let bars_used = ic.len();
        if bars_used < 2 {
            return Err(TestError::Precondition(format!(
                "{} of {} bars have at least {} entities with both values and neither column constant; the t of the mean IC needs two",
                bars_used,
                bars.len(),
                min_entities
            )));
        }
        let lags = match count(self, params, "lags")? {
            Some(l) => l,
            None => hac::newey_west_lags(bars_used).max(horizon - 1),
        };
        let (mean, se) = hac::newey_west_mean(&ic, lags);
        let df = (bars_used - 1) as f64;
        let (statistic, p_value, interval) = if se > 0.0 {
            let q = dist::t_ppf(0.5 + level / 2.0, df);
            (mean / se, dist::t_two_sided(mean / se, df), (mean - q * se, mean + q * se, level))
        } else if mean == 0.0 {
            (0.0, 1.0, (0.0, 0.0, level))
        } else {
            return Err(TestError::Precondition(format!("the IC is {} at every bar: its standard error is zero", mean)));
        };
        let sd = (ic.iter().map(|v| (v - mean) * (v - mean)).sum::<f64>() / df).sqrt();
        Ok(TestResult {
            statistic,
            p_value: Some(p_value),
            interval: Some(interval),
            effect: Some(mean),
            df: Some(df),
            n: bars_used,
            notes: vec![
                format!("{} bars in the IC series, {} left out", bars_used, bars.len() - bars_used),
                format!("{:.1} entities per bar on average", entities as f64 / bars_used as f64),
                format!("IC standard deviation {}, mean over standard deviation (ICIR) {}", sd, if sd > 0.0 { mean / sd } else { 0.0 }),
                format!("Newey–West standard error {} with {} lags", se, lags),
            ],
        })
    }
}

/// A non-negative integer parameter, or its default; a value of another
/// kind is refused rather than ignored.
fn count(test: &dyn Test, params: &Params, name: &str) -> Result<Option<usize>, TestError> {
    match param_usize(test, params, name) {
        Some(v) => Ok(Some(v)),
        None if params.contains_key(name) => Err(TestError::Input(format!("`{}` is a non-negative integer", name))),
        None => Ok(None),
    }
}

/// The ranks of `x` from 1, tied values at the mean of the ranks they
/// span (Spearman's convention for ties).
fn ranks(x: &[f64]) -> Vec<f64> {
    let mut order: Vec<usize> = (0..x.len()).collect();
    order.sort_by(|&i, &j| x[i].total_cmp(&x[j]));
    let mut r = vec![0.0; x.len()];
    let mut k = 0;
    while k < order.len() {
        let mut e = k;
        while e + 1 < order.len() && x[order[e + 1]] == x[order[k]] {
            e += 1;
        }
        let rank = (k + e) as f64 / 2.0 + 1.0;
        for &i in &order[k..=e] {
            r[i] = rank;
        }
        k = e + 1;
    }
    r
}

/// Pearson's correlation of two equally long vectors, or none when either
/// is constant.
fn correlation(a: &[f64], b: &[f64]) -> Option<f64> {
    let n = a.len() as f64;
    let (ma, mb) = (a.iter().sum::<f64>() / n, b.iter().sum::<f64>() / n);
    let (mut sab, mut saa, mut sbb) = (0.0, 0.0, 0.0);
    for (x, y) in a.iter().zip(b) {
        let (dx, dy) = (x - ma, y - mb);
        sab += dx * dy;
        saa += dx * dx;
        sbb += dy * dy;
    }
    (saa > 0.0 && sbb > 0.0).then(|| sab / (saa * sbb).sqrt())
}

/// Spearman's rank correlation of two equally long vectors, or none when
/// either is constant.
pub fn spearman(a: &[f64], b: &[f64]) -> Option<f64> {
    correlation(&ranks(a), &ranks(b))
}
