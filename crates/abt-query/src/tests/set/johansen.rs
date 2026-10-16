//! `johansen` (Q-4.4): the cointegration rank of the system is at most r.
//! Johansen's (1988, 1991) reduced-rank regression of the vector error
//! correction model with an unrestricted constant,
//! Δx_t = μ + Π x_{t−1} + Σ_{i ≤ p} Γ_i Δx_{t−i} + ε_t: R₀ and R₁ are the
//! residuals of Δx_t and x_{t−1} on a constant and the p lagged differences,
//! S_ij = R_iᵀR_j / T, and λ₁ ≥ … ≥ λ_k are the eigenvalues of
//! S₁₁⁻¹ S₁₀ S₀₀⁻¹ S₀₁. The trace statistic is −T Σ_{i > r} ln(1 − λ_i) and
//! the maximum-eigenvalue statistic −T ln(1 − λ_{r+1}). Their asymptotic
//! null distributions (Dependence::Asymptotic) are those of Osterwald-Lenum
//! (1992), Table 1 (the constant unrestricted, so that the data may trend),
//! for k − r = 1 to 11 stochastic trends. The p-value interpolates linearly
//! between the tabulated quantiles (50 to 99 %); beyond them it is the
//! table's bound, and the notes say so.

use crate::tests::framework::{Dependence, InputSpec, Param, ParamKind, ParamSpec, Params, Shape, Table, Test, TestError, TestResult};
use crate::tests::linalg::{cholesky, least_squares, symmetric_eigen, Matrix};

use super::timeseries::{f64_param, system, text_param, usize_param};

pub struct Johansen;

/// The probabilities of the quantiles Osterwald-Lenum tabulates.
pub const QUANTILES: [f64; 6] = [0.50, 0.80, 0.90, 0.95, 0.975, 0.99];

/// The upper-tail probabilities of those quantiles, the p-values at them.
pub const SIZES: [f64; 6] = [0.50, 0.20, 0.10, 0.05, 0.025, 0.01];

/// Osterwald-Lenum (1992), Table 1: the quantiles of the trace statistic
/// at `QUANTILES`, for k − r = 1, …, 11, with an unrestricted constant.
/// Transcribed as Joly's `johans` for Stata carries the table (from
/// Heinecke's `mlcoint`); its 95 and 99 % columns are those Stata's own
/// `vecrank` reports.
pub const TRACE: [[f64; 6]; 11] = [
    [0.44, 1.66, 2.69, 3.76, 4.95, 6.65],
    [7.55, 11.07, 13.33, 15.41, 17.52, 20.04],
    [18.7, 23.64, 26.79, 29.68, 32.56, 35.65],
    [33.6, 40.15, 43.95, 47.21, 50.35, 54.46],
    [52.3, 60.29, 64.84, 68.52, 71.8, 76.07],
    [75.26, 84.57, 89.48, 94.15, 98.33, 103.18],
    [101.22, 112.3, 118.5, 124.24, 128.45, 133.57],
    [131.62, 143.97, 150.53, 156.0, 161.32, 168.36],
    [165.11, 178.9, 186.39, 192.89, 198.82, 204.95],
    [202.58, 217.81, 225.85, 233.13, 239.46, 247.18],
    [243.9, 260.82, 269.96, 277.71, 284.87, 293.44],
];

/// Osterwald-Lenum (1992), Table 1: the quantiles of the
/// maximum-eigenvalue statistic, as `TRACE`.
pub const MAX: [[f64; 6]; 11] = [
    [0.44, 1.66, 2.69, 3.76, 4.95, 6.65],
    [6.85, 10.04, 12.07, 14.07, 16.05, 18.63],
    [12.34, 16.2, 18.6, 20.97, 23.09, 25.52],
    [17.66, 21.98, 24.73, 27.07, 28.98, 32.24],
    [23.05, 27.85, 30.9, 33.46, 35.71, 38.77],
    [28.45, 33.67, 36.76, 39.37, 41.86, 45.1],
    [33.83, 39.12, 42.32, 45.28, 47.96, 51.57],
    [39.29, 45.05, 48.33, 51.42, 54.29, 57.69],
    [44.58, 50.55, 53.98, 57.12, 59.33, 62.8],
    [49.66, 55.97, 59.62, 62.81, 65.44, 69.09],
    [54.99, 61.55, 65.38, 68.83, 72.11, 75.95],
];

/// Which of Johansen's statistics a result reports.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Statistic {
    Trace,
    MaxEigen,
}

impl Statistic {
    fn table(self) -> &'static [[f64; 6]; 11] {
        match self {
            Statistic::Trace => &TRACE,
            Statistic::MaxEigen => &MAX,
        }
    }
}

/// The p-value of a statistic with `d` = k − r stochastic trends under the
/// null, by linear interpolation between Osterwald-Lenum's quantiles, and
/// whether it is a bound: below the median the p-value is at least 0.5,
/// above the 99 % quantile at most 0.01. `None` outside 1 ≤ d ≤ 11.
pub fn p_value(stat: f64, d: usize, which: Statistic) -> Option<(f64, Option<&'static str>)> {
    let q = which.table().get(d.checked_sub(1)?)?;
    if stat < q[0] {
        return Some((SIZES[0], Some("below Osterwald-Lenum's median: the p-value is greater than 0.5")));
    }
    if stat > q[5] {
        return Some((SIZES[5], Some("above Osterwald-Lenum's 99 % quantile: the p-value is smaller than 0.01")));
    }
    for i in 0..5 {
        if stat <= q[i + 1] {
            let w = (stat - q[i]) / (q[i + 1] - q[i]);
            return Some((SIZES[i] + w * (SIZES[i + 1] - SIZES[i]), None));
        }
    }
    Some((SIZES[5], None))
}

/// The eigenvalues of Johansen's problem for the system `x`, its k series
/// of one length, with `p` lagged differences, in descending order, and
/// the number T of observations they come from.
pub fn eigenvalues(x: &[&[f64]], p: usize) -> Result<(Vec<f64>, usize), TestError> {
    let k = x.len();
    let n = x.first().map(|c| c.len()).unwrap_or(0);
    if k == 0 || x.iter().any(|c| c.len() != n) {
        return Err(TestError::Input("johansen: the series are not of one length".into()));
    }
    // Each equation's regression has 1 + pk regressors over T = n − p − 1
    // observations, and the k-dimensional moments of its residuals need
    // T > 1 + pk + k, that is n > pk + k + p + 2.
    let needed = p.checked_mul(k).and_then(|pk| pk.checked_add(k + 1)).and_then(|m| m.checked_add(p + 1));
    match needed {
        Some(m) if n > m => {}
        _ => return Err(TestError::Precondition(format!("johansen with {} series and {} lags needs more observations than {}", k, p, n))),
    }
    let rows: Vec<usize> = (p + 1..n).collect();
    let t = rows.len();
    let dx = |i: usize, s: usize| x[i][s] - x[i][s - 1];
    let mut w = vec![vec![1.0; t]];
    for j in 1..=p {
        for i in 0..k {
            w.push(rows.iter().map(|&s| dx(i, s - j)).collect());
        }
    }
    let w = Matrix::from_columns(&w);
    let residuals = |y: Vec<f64>| least_squares(&w, &y).map(|f| f.residuals).map_err(TestError::Numeric);
    let r0: Vec<Vec<f64>> = (0..k).map(|i| residuals(rows.iter().map(|&s| dx(i, s)).collect())).collect::<Result<_, _>>()?;
    let r1: Vec<Vec<f64>> = (0..k).map(|i| residuals(rows.iter().map(|&s| x[i][s - 1]).collect())).collect::<Result<_, _>>()?;
    let moment = |a: &[Vec<f64>], b: &[Vec<f64>]| {
        let mut s = Matrix::zeros(k, k);
        for i in 0..k {
            for j in 0..k {
                s[(i, j)] = a[i].iter().zip(&b[j]).map(|(u, v)| u * v).sum::<f64>() / t as f64;
            }
        }
        s
    };
    let (s00, s10, s11) = (moment(&r0, &r0), moment(&r1, &r0), moment(&r1, &r1));
    let l = cholesky(&s11).map_err(|e| TestError::Numeric(format!("johansen: the lagged levels are collinear ({})", e)))?;
    let g = cholesky(&s00).map_err(|e| TestError::Numeric(format!("johansen: the differences are collinear ({})", e)))?;
    // With S₁₁ = L Lᵀ and S₀₀ = G Gᵀ, the eigenvalues are those of the
    // symmetric Bᵀ B, B = G⁻¹ (L⁻¹ S₁₀)ᵀ.
    let a = forward(&l, &s10);
    let b = forward(&g, &a.transpose());
    let m = b.transpose().mul(&b);
    let (values, _) = symmetric_eigen(&m);
    let values: Vec<f64> = values.into_iter().map(|v| v.max(0.0)).collect();
    if values.iter().any(|v| *v >= 1.0) {
        return Err(TestError::Numeric("johansen: an eigenvalue is one (the system fits exactly)".into()));
    }
    Ok((values, t))
}

/// Solves L X = B for a lower-triangular L.
fn forward(l: &Matrix, b: &Matrix) -> Matrix {
    let n = l.rows;
    let mut x = Matrix::zeros(n, b.cols);
    for c in 0..b.cols {
        for i in 0..n {
            let mut s = b[(i, c)];
            for j in 0..i {
                s -= l[(i, j)] * x[(j, c)];
            }
            x[(i, c)] = s / l[(i, i)];
        }
    }
    x
}

const INPUTS: [InputSpec; 1] = [InputSpec {
    name: "system",
    shape: Shape::Series,
    columns: &["x..."],
}];

const PARAMS: [ParamSpec; 4] = [
    ParamSpec {
        name: "lags",
        kind: ParamKind::Int,
        default: Some(Param::Int(1)),
        range: Some((0.0, 1e6)),
        doc: "the number p of lagged differences in the VECM",
    },
    ParamSpec {
        name: "rank",
        kind: ParamKind::Int,
        default: Some(Param::Int(0)),
        range: Some((0.0, 10.0)),
        doc: "the rank r under the null",
    },
    ParamSpec {
        name: "statistic",
        kind: ParamKind::Text,
        default: None,
        range: None,
        doc: "`trace` (the default) or `max`, the maximum-eigenvalue statistic",
    },
    ParamSpec {
        name: "level",
        kind: ParamKind::Float,
        default: Some(Param::Float(0.05)),
        range: Some((0.01, 0.5)),
        doc: "the size of the sequential tests that estimate the rank",
    },
];

impl Test for Johansen {
    fn name(&self) -> &str {
        "johansen"
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
        "the cointegration rank of the system is at most r"
    }
    fn dependence(&self) -> Dependence {
        Dependence::Asymptotic
    }
    /// The table's columns are the system's series on one time axis. The
    /// statistic (trace or maximum eigenvalue) for rank ≤ r with its
    /// p-value; the effect is the rank the sequential tests at `level`
    /// estimate (the first r not rejected); n is T; the notes carry the
    /// eigenvalues and both statistics for every r with their 90, 95 and 99
    /// % quantiles.
    fn run(&self, tables: &[Table], params: &Params, _seed: u64) -> Result<TestResult, TestError> {
        let columns = system(tables, "johansen", 4)?;
        let k = columns.len();
        if !(2..=11).contains(&k) {
            return Err(TestError::Precondition(format!("johansen takes 2 to 11 series (Osterwald-Lenum's table), not {}", k)));
        }
        let p = usize_param(self, params, "lags")?.unwrap_or(1);
        let r = usize_param(self, params, "rank")?.unwrap_or(0);
        if r >= k {
            return Err(TestError::Input(format!("johansen: the null rank {} is not below the {} series", r, k)));
        }
        let which = match text_param(self, params, "statistic")? {
            None | Some("trace") => Statistic::Trace,
            Some("max") => Statistic::MaxEigen,
            Some(other) => return Err(TestError::Input(format!("`statistic` is `trace` or `max`, not `{}`", other))),
        };
        let level = f64_param(self, params, "level")?.unwrap_or(0.05);
        if level <= 0.0 || level >= 1.0 {
            return Err(TestError::Input(format!("johansen: level is in (0, 1), not {}", level)));
        }
        let x: Vec<&[f64]> = columns.iter().map(|(_, c)| *c).collect();
        let (lambda, t) = eigenvalues(&x, p)?;
        let tf = t as f64;
        let trace: Vec<f64> = (0..k).map(|r| -tf * lambda[r..].iter().map(|l| (1.0 - l).ln()).sum::<f64>()).collect();
        let max: Vec<f64> = (0..k).map(|r| -tf * (1.0 - lambda[r]).ln()).collect();
        let stat = |r: usize| match which {
            Statistic::Trace => trace[r],
            Statistic::MaxEigen => max[r],
        };
        let pv = |r: usize| p_value(stat(r), k - r, which).ok_or_else(|| TestError::Precondition(format!("johansen: no quantiles for {} trends", k - r)));
        let mut rank = k;
        for h in 0..k {
            if pv(h)?.0 > level {
                rank = h;
                break;
            }
        }
        let (p_value, bound) = pv(r)?;
        let mut notes = vec![format!("lags {}", p), format!("eigenvalues {}", lambda.iter().map(|l| l.to_string()).collect::<Vec<_>>().join(", "))];
        for h in 0..k {
            let (tq, mq) = (&TRACE[k - h - 1], &MAX[k - h - 1]);
            notes.push(format!(
                "rank ≤ {}: trace {} (90 % {}, 95 % {}, 99 % {}), max {} (90 % {}, 95 % {}, 99 % {})",
                h, trace[h], tq[2], tq[3], tq[5], max[h], mq[2], mq[3], mq[5]
            ));
        }
        notes.extend(bound.map(String::from));
        Ok(TestResult {
            statistic: stat(r),
            p_value: Some(p_value),
            interval: None,
            effect: Some(rank as f64),
            df: None,
            n: t,
            notes,
        })
    }
}
