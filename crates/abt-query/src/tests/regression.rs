//! Ordinary least squares with the covariances of Q-4.2 (Q-4.4's
//! `regression`, the engine behind `ic`, `half_life` and `fama_macbeth`):
//! the coefficients by Householder QR with pivoting, their standard errors
//! under iid errors, Newey–West or clustering, R² and the regression F.

use super::hac;
use super::linalg::{least_squares, Matrix};

/// How the coefficients' covariance is estimated.
#[derive(Clone, Debug, PartialEq)]
pub enum Covariance {
    /// s² (X'X)⁻¹ with s² = RSS / (n − p).
    Iid,
    /// Newey–West with `lags` (the default rule when `None`).
    NeweyWest(Option<usize>),
    /// Clustered by the group label of each row.
    Clustered(Vec<u64>),
}

/// An OLS fit.
#[derive(Clone, Debug, PartialEq)]
pub struct Ols {
    pub beta: Vec<f64>,
    pub se: Vec<f64>,
    pub t: Vec<f64>,
    pub residuals: Vec<f64>,
    pub rss: f64,
    /// The residual standard deviation sqrt(RSS / (n − p)).
    pub sigma: f64,
    /// R² about the mean when the design has an intercept, else uncentred.
    pub r2: f64,
    pub n: usize,
    pub p: usize,
    pub df: f64,
    pub covariance: Matrix,
}

/// OLS of `y` on the columns `x` (an intercept is a column of ones the
/// caller includes, its presence detected for R²).
pub fn ols(columns: &[Vec<f64>], y: &[f64], cov: &Covariance) -> Result<Ols, String> {
    let x = Matrix::from_columns(columns);
    let (n, p) = (x.rows, x.cols);
    if n <= p {
        return Err(format!("{} observations for {} coefficients", n, p));
    }
    let fit = least_squares(&x, y)?;
    let df = (n - p) as f64;
    let s2 = fit.rss / df;
    let covariance = match cov {
        Covariance::Iid => {
            let mut v = fit.xtx_inv.clone();
            v.data.iter_mut().for_each(|e| *e *= s2);
            v
        }
        Covariance::NeweyWest(lags) => hac::hac_covariance(&x, &fit.residuals, &fit.xtx_inv, lags.unwrap_or_else(|| hac::newey_west_lags(n))),
        Covariance::Clustered(groups) => {
            if groups.len() != n {
                return Err(format!("{} cluster labels for {} rows", groups.len(), n));
            }
            hac::clustered_covariance(&x, &fit.residuals, &fit.xtx_inv, groups)
        }
    };
    let se: Vec<f64> = (0..p).map(|j| covariance[(j, j)].max(0.0).sqrt()).collect();
    let t: Vec<f64> = fit.beta.iter().zip(&se).map(|(b, s)| if *s > 0.0 { b / s } else { f64::NAN }).collect();
    let has_intercept = columns.iter().any(|c| c.iter().all(|v| *v == 1.0));
    let tss = if has_intercept {
        let m = y.iter().sum::<f64>() / n as f64;
        y.iter().map(|v| (v - m) * (v - m)).sum::<f64>()
    } else {
        y.iter().map(|v| v * v).sum::<f64>()
    };
    let r2 = if tss > 0.0 { 1.0 - fit.rss / tss } else { 0.0 };
    Ok(Ols {
        beta: fit.beta,
        se,
        t,
        residuals: fit.residuals,
        rss: fit.rss,
        sigma: s2.sqrt(),
        r2,
        n,
        p,
        df,
        covariance,
    })
}
