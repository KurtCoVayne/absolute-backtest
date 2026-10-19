//! Resampling under dependence (Q-4.2): the stationary bootstrap (Politis
//! and Romano 1994) with the mean block length of Politis and White (2004),
//! as corrected by Patton, Politis and White (2009), and permutations. Each
//! replication draws from its own stream (`rng::Rng::stream(seed, rep)`).

use super::rng::Rng;

/// The sample autocorrelation of `x` at lag `k` (over the full-sample mean
/// and variance).
pub fn autocorrelation(x: &[f64], k: usize) -> f64 {
    let n = x.len();
    if k >= n {
        return 0.0;
    }
    let m = x.iter().sum::<f64>() / n as f64;
    let c0: f64 = x.iter().map(|v| (v - m) * (v - m)).sum();
    if c0 == 0.0 {
        return 0.0;
    }
    let ck: f64 = (k..n).map(|t| (x[t] - m) * (x[t - k] - m)).sum();
    ck / c0
}

/// The autocovariance at lag `k` (divided by n).
fn autocovariance(x: &[f64], k: usize) -> f64 {
    let n = x.len();
    let m = x.iter().sum::<f64>() / n as f64;
    (k..n).map(|t| (x[t] - m) * (x[t - k] - m)).sum::<f64>() / n as f64
}

/// The flat-top (trapezoidal) lag window of Politis and Romano (1995).
fn flat_top(t: f64) -> f64 {
    let a = t.abs();
    if a <= 0.5 {
        1.0
    } else if a <= 1.0 {
        2.0 * (1.0 - a)
    } else {
        0.0
    }
}

/// The optimal mean block length of the stationary bootstrap for the mean
/// of `x` (Politis and White 2004, with the correction of Patton, Politis
/// and White 2009), bounded by min(3√n, n/3).
pub fn politis_white_block(x: &[f64]) -> f64 {
    let n = x.len();
    if n < 4 {
        return 1.0;
    }
    let nf = n as f64;
    let kn = ((nf.log10().sqrt()).ceil() as usize).max(5);
    let threshold = 2.0 * (nf.log10() / nf).sqrt();
    let max_lag = ((nf.sqrt().ceil() as usize) + kn).min(n - 1);
    let rho: Vec<f64> = (0..=max_lag).map(|k| autocorrelation(x, k)).collect();
    // m̂: the smallest lag after which kn consecutive autocorrelations are
    // insignificant.
    let mut m_hat = max_lag.saturating_sub(kn);
    for m in 1..=max_lag.saturating_sub(kn) {
        if (1..=kn).all(|k| rho[m + k].abs() < threshold) {
            m_hat = m;
            break;
        }
    }
    let big_m = (2 * m_hat).clamp(1, n - 1);
    let mut g = 0.0;
    let mut g0 = 0.0;
    for k in 0..=big_m {
        let w = flat_top(k as f64 / big_m as f64);
        let r = autocovariance(x, k);
        if k == 0 {
            g0 += w * r;
        } else {
            g += 2.0 * w * k as f64 * r;
            g0 += 2.0 * w * r;
        }
    }
    let d_sb = 2.0 * g0 * g0;
    let upper = (3.0 * nf.sqrt()).min(nf / 3.0);
    if d_sb <= 0.0 || g == 0.0 {
        return 1.0;
    }
    ((2.0 * g * g / d_sb).powf(1.0 / 3.0) * nf.powf(1.0 / 3.0)).clamp(1.0, upper.max(1.0))
}

/// One stationary-bootstrap resample of `x` with mean block `block`: blocks
/// start uniformly, wrap around, and end with probability 1/block after each
/// element.
pub fn stationary_resample(x: &[f64], block: f64, rng: &mut Rng, out: &mut Vec<f64>) {
    let n = x.len();
    out.clear();
    if n == 0 {
        return;
    }
    let p = 1.0 / block.max(1.0);
    let mut i = rng.below(n);
    while out.len() < n {
        out.push(x[i]);
        if rng.uniform() < p {
            i = rng.below(n);
        } else {
            i = (i + 1) % n;
        }
    }
}

/// The bootstrap distribution of a statistic: `reps` stationary resamples,
/// replication r drawn from `Rng::stream(seed, r)`.
pub fn stationary_bootstrap(x: &[f64], block: f64, reps: usize, seed: u64, stat: &dyn Fn(&[f64]) -> f64) -> Vec<f64> {
    let mut buf = Vec::with_capacity(x.len());
    (0..reps)
        .map(|r| {
            let mut rng = Rng::stream(seed, r as u64);
            stationary_resample(x, block, &mut rng, &mut buf);
            stat(&buf)
        })
        .collect()
}

/// The `p` quantile of sorted values (linear interpolation between order
/// statistics, the "type 7" definition).
pub fn quantile(sorted: &[f64], p: f64) -> f64 {
    if sorted.is_empty() {
        return f64::NAN;
    }
    let h = (sorted.len() - 1) as f64 * p.clamp(0.0, 1.0);
    let lo = h.floor() as usize;
    let hi = h.ceil() as usize;
    sorted[lo] + (h - lo as f64) * (sorted[hi] - sorted[lo])
}
