//! Heteroskedasticity and autocorrelation consistent covariances (Q-4.2):
//! Newey and West (1987) with the Bartlett kernel, the lag rule
//! ⌊4 (n/100)^(2/9)⌋, and standard errors clustered by entity or time
//! (Petersen 2009).

use super::linalg::Matrix;

/// Newey and West's default lag for `n` observations.
pub fn newey_west_lags(n: usize) -> usize {
    (4.0 * (n as f64 / 100.0).powf(2.0 / 9.0)).floor() as usize
}

/// The long-run variance of a series about its mean: γ₀ + 2 Σ_{l ≤ L}
/// (1 − l/(L+1)) γ_l, the autocovariances divided by n.
pub fn long_run_variance(x: &[f64], lags: usize) -> f64 {
    let n = x.len();
    if n == 0 {
        return 0.0;
    }
    let m = x.iter().sum::<f64>() / n as f64;
    let d: Vec<f64> = x.iter().map(|v| v - m).collect();
    let gamma = |l: usize| (l..n).map(|t| d[t] * d[t - l]).sum::<f64>() / n as f64;
    let mut s = gamma(0);
    for l in 1..=lags.min(n.saturating_sub(1)) {
        s += 2.0 * (1.0 - l as f64 / (lags as f64 + 1.0)) * gamma(l);
    }
    s
}

/// The Newey–West t-statistic of the mean of `x`.
pub fn newey_west_t(x: &[f64], lags: Option<usize>) -> f64 {
    let n = x.len();
    if n < 2 {
        return 0.0;
    }
    let l = lags.unwrap_or_else(|| newey_west_lags(n));
    let m = x.iter().sum::<f64>() / n as f64;
    let v = long_run_variance(x, l) / n as f64;
    if v > 0.0 {
        m / v.sqrt()
    } else {
        0.0
    }
}

/// The HAC covariance of OLS coefficients: (X'X)⁻¹ S (X'X)⁻¹ with
/// S = Σ_l w_l Σ_t (x_t u_t)(x_{t−l} u_{t−l})' (both orders for l > 0),
/// the Bartlett weights, rows in time order.
pub fn hac_covariance(x: &Matrix, residuals: &[f64], xtx_inv: &Matrix, lags: usize) -> Matrix {
    let (n, p) = (x.rows, x.cols);
    let score = |t: usize, j: usize| x[(t, j)] * residuals[t];
    let mut s = Matrix::zeros(p, p);
    for l in 0..=lags.min(n.saturating_sub(1)) {
        let w = if l == 0 { 1.0 } else { 1.0 - l as f64 / (lags as f64 + 1.0) };
        for i in 0..p {
            for j in 0..p {
                let mut g = 0.0;
                for t in l..n {
                    g += score(t, i) * score(t - l, j);
                }
                if l == 0 {
                    s[(i, j)] += g;
                } else {
                    s[(i, j)] += w * g;
                    s[(j, i)] += w * g;
                }
            }
        }
    }
    xtx_inv.mul(&s).mul(xtx_inv)
}

/// The covariance of OLS coefficients clustered by a group label of each
/// row (Petersen 2009), with the small-sample factor G/(G−1)·(n−1)/(n−p).
pub fn clustered_covariance(x: &Matrix, residuals: &[f64], xtx_inv: &Matrix, groups: &[u64]) -> Matrix {
    let (n, p) = (x.rows, x.cols);
    let mut sums: std::collections::BTreeMap<u64, Vec<f64>> = std::collections::BTreeMap::new();
    for t in 0..n {
        let e = sums.entry(groups[t]).or_insert_with(|| vec![0.0; p]);
        for j in 0..p {
            e[j] += x[(t, j)] * residuals[t];
        }
    }
    let g = sums.len() as f64;
    let mut s = Matrix::zeros(p, p);
    for v in sums.values() {
        for i in 0..p {
            for j in 0..p {
                s[(i, j)] += v[i] * v[j];
            }
        }
    }
    let factor = if g > 1.0 && n > p { g / (g - 1.0) * (n as f64 - 1.0) / (n as f64 - p as f64) } else { 1.0 };
    let mut v = xtx_inv.mul(&s).mul(xtx_inv);
    v.data.iter_mut().for_each(|x| *x *= factor);
    v
}
