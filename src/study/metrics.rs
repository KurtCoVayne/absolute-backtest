//! The metrics library (data-bundle doc, section 7). Return-based metrics
//! over a NAV series, the robustness statistics of Bailey and López de Prado
//! (probabilistic and deflated Sharpe ratio, minimum track-record length,
//! probability of backtest overfitting by combinatorially symmetric
//! cross-validation), Lo's autocorrelation adjustment of the Sharpe ratio,
//! a Newey-West t-statistic, a block bootstrap, and the trading metrics of a
//! run (turnover, participation, fill ratio, exposure, capacity).
//!
//! Conventions, fixed here so that two readings of a number cannot differ:
//!
//! - A return is a simple per-bar return `e_t / e_{t-1} - 1` of the equity
//!   curve (the book marked at each bar's close, before that bar's
//!   decisions). The series has one observation fewer than the curve.
//! - Mean and standard deviation are the sample ones (n − 1). Skewness and
//!   kurtosis use population moments; `kurtosis` is excess (normal = 0), and
//!   the Bailey–López de Prado formulas, which take raw kurtosis, add 3.
//! - Annualisation multiplies a mean by `periods_per_year` and a standard
//!   deviation by its square root; `sharpe` is the iid annualised ratio.
//!   `sharpe_lo` applies Lo (2002), equation (18): with q periods in a year
//!   and ρ_k the autocorrelation at lag k, the annualised ratio is
//!   SR · q / sqrt(q + 2 Σ_{k=1}^{q-1} (q − k) ρ_k), with ρ_k estimated up to
//!   `MAX_LAG` and taken as zero beyond. `sharpe_se` is Lo's iid standard
//!   error sqrt((1 + SR²/2) / n) of the per-period ratio, scaled like the
//!   ratio itself by the same factor; it is an approximation to the full
//!   GMM error under autocorrelation.
//! - CAGR is `(end / start)^(periods_per_year / n) − 1`; drawdown is
//!   `1 − e_t / max_{s≤t} e_s`, its duration the longest run of bars below
//!   the running peak; CVaR 5 % is minus the mean of the worst 5 % of
//!   returns (a positive number is a loss); the worst month compounds bars
//!   by calendar month.
//! - The probabilistic Sharpe ratio, the deflated Sharpe ratio and the
//!   minimum track-record length take the per-period (not annualised)
//!   Sharpe ratio, as in the papers.

use std::collections::BTreeMap;

use crate::ir::Resolution;
use crate::kernel::time;
use crate::kernel::RunResult;

/// Autocorrelations beyond this lag are taken as zero in Lo's adjustment.
pub const MAX_LAG: usize = 10;

/// Bars per year at a resolution: 252 trading days of 6.5 hours.
pub fn periods_per_year(res: Resolution) -> f64 {
    match res {
        Resolution::D1 => 252.0,
        Resolution::H1 => 252.0 * 6.5,
        Resolution::M30 => 252.0 * 13.0,
        Resolution::M15 => 252.0 * 26.0,
        Resolution::M5 => 252.0 * 78.0,
        Resolution::M1 => 252.0 * 390.0,
    }
}

/// Per-bar simple returns of an equity curve, with the bar timestamps of
/// each return (the bar it ends on).
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Returns {
    pub periods_per_year: f64,
    pub t: Vec<i64>,
    pub r: Vec<f64>,
}

impl Returns {
    pub fn from_curve(curve: &[(i64, f64)], periods_per_year: f64) -> Returns {
        let mut t = Vec::new();
        let mut r = Vec::new();
        for w in curve.windows(2) {
            let (t1, e1) = w[1];
            let e0 = w[0].1;
            if e0 > 0.0 {
                t.push(t1);
                r.push(e1 / e0 - 1.0);
            }
        }
        Returns { periods_per_year, t, r }
    }

    pub fn len(&self) -> usize {
        self.r.len()
    }

    pub fn is_empty(&self) -> bool {
        self.r.is_empty()
    }
}

/// Return-based metrics of one run (data-bundle doc, section 7).
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ReturnMetrics {
    /// Return observations (bars minus one).
    pub n: usize,
    pub total_return: f64,
    pub cagr: f64,
    pub volatility: f64,
    /// Annualised, iid.
    pub sharpe: f64,
    /// Annualised with Lo's autocorrelation correction.
    pub sharpe_lo: f64,
    /// Standard error of `sharpe_lo` (see the module notes).
    pub sharpe_se: f64,
    /// Per-period Sharpe ratio, the input of PSR, DSR and MinTRL.
    pub sharpe_period: f64,
    pub sortino: f64,
    pub max_drawdown: f64,
    /// Bars from a peak to its recovery (or the end), at the longest.
    pub max_drawdown_duration: usize,
    pub cvar_5: f64,
    pub skew: f64,
    /// Excess kurtosis.
    pub kurtosis: f64,
    pub worst_month: f64,
    /// Newey-West t-statistic of the mean return.
    pub t_stat: f64,
    /// First-order autocorrelation of the returns.
    pub autocorrelation_1: f64,
}

fn mean(x: &[f64]) -> f64 {
    if x.is_empty() {
        0.0
    } else {
        x.iter().sum::<f64>() / x.len() as f64
    }
}

fn sample_std(x: &[f64]) -> f64 {
    if x.len() < 2 {
        return 0.0;
    }
    let m = mean(x);
    (x.iter().map(|v| (v - m).powi(2)).sum::<f64>() / (x.len() as f64 - 1.0)).sqrt()
}

fn central_moment(x: &[f64], k: i32) -> f64 {
    let m = mean(x);
    mean(&x.iter().map(|v| (v - m).powi(k)).collect::<Vec<_>>())
}

/// Sample skewness over population moments.
pub fn skewness(x: &[f64]) -> f64 {
    let m2 = central_moment(x, 2);
    if m2 <= 0.0 {
        0.0
    } else {
        central_moment(x, 3) / m2.powf(1.5)
    }
}

/// Excess kurtosis over population moments.
pub fn excess_kurtosis(x: &[f64]) -> f64 {
    let m2 = central_moment(x, 2);
    if m2 <= 0.0 {
        0.0
    } else {
        central_moment(x, 4) / (m2 * m2) - 3.0
    }
}

/// Autocorrelation at lag k (population form, as Lo estimates it).
pub fn autocorrelation(x: &[f64], k: usize) -> f64 {
    let n = x.len();
    if k == 0 {
        return 1.0;
    }
    if k >= n {
        return 0.0;
    }
    let m = mean(x);
    let denom: f64 = x.iter().map(|v| (v - m).powi(2)).sum();
    if denom <= 0.0 {
        return 0.0;
    }
    let num: f64 = (k..n).map(|i| (x[i] - m) * (x[i - k] - m)).sum();
    num / denom
}

/// Lo (2002): the factor that annualises a per-period Sharpe ratio over q
/// periods under autocorrelation (sqrt(q) when every ρ_k is zero).
pub fn lo_factor(x: &[f64], q: f64) -> f64 {
    let qn = q.round().max(1.0) as usize;
    let lags = MAX_LAG.min(qn.saturating_sub(1)).min(x.len().saturating_sub(2));
    let mut s = 0.0;
    for k in 1..=lags {
        s += (q - k as f64) * autocorrelation(x, k);
    }
    let denom = q + 2.0 * s;
    if denom <= 0.0 {
        q.sqrt()
    } else {
        q / denom.sqrt()
    }
}

/// Newey-West t-statistic of the mean of `x` with Bartlett weights over
/// `lags` lags (`None`: floor(4 (n/100)^(2/9))).
pub fn newey_west_t(x: &[f64], lags: Option<usize>) -> f64 {
    let n = x.len();
    if n < 2 {
        return 0.0;
    }
    let m = mean(x);
    let e: Vec<f64> = x.iter().map(|v| v - m).collect();
    let l = lags.unwrap_or_else(|| (4.0 * (n as f64 / 100.0).powf(2.0 / 9.0)).floor() as usize).min(n - 1);
    let gamma = |k: usize| -> f64 { (k..n).map(|i| e[i] * e[i - k]).sum::<f64>() / n as f64 };
    let mut var = gamma(0);
    for k in 1..=l {
        var += 2.0 * (1.0 - k as f64 / (l as f64 + 1.0)) * gamma(k);
    }
    if var <= 0.0 {
        return 0.0;
    }
    m / (var / n as f64).sqrt()
}

/// The longest run of bars under the running peak, and the deepest drawdown.
fn drawdown(curve: &[f64]) -> (f64, usize) {
    let mut peak = f64::NEG_INFINITY;
    let mut mdd: f64 = 0.0;
    let mut run = 0usize;
    let mut longest = 0usize;
    for &e in curve {
        if e >= peak {
            peak = e;
            run = 0;
        } else {
            run += 1;
            longest = longest.max(run);
        }
        if peak > 0.0 {
            mdd = mdd.max(1.0 - e / peak);
        }
    }
    (mdd, longest)
}

/// The curve of the kept bars only: the first kept bar's equity, then
/// each later kept bar compounding its own one-bar return (a block embargo
/// or a walk-forward window withholds the others). Every bar kept returns
/// the curve itself.
pub fn masked_curve(curve: &[(i64, f64)], keep: &dyn Fn(i64) -> bool) -> Vec<(i64, f64)> {
    if curve.iter().all(|(t, _)| keep(*t)) {
        return curve.to_vec();
    }
    let mut out: Vec<(i64, f64)> = Vec::new();
    let mut e = 0.0;
    for (i, &(t, v)) in curve.iter().enumerate() {
        if !keep(t) {
            continue;
        }
        if out.is_empty() {
            e = v;
        } else {
            let prev = curve[i - 1].1;
            let r = if prev > 0.0 { v / prev - 1.0 } else { 0.0 };
            e *= 1.0 + r;
        }
        out.push((t, e));
    }
    out
}

/// Every return-based metric of an equity curve.
pub fn return_metrics(curve: &[(i64, f64)], periods_per_year: f64) -> ReturnMetrics {
    let rets = Returns::from_curve(curve, periods_per_year);
    let r = &rets.r;
    let n = r.len();
    let mut m = ReturnMetrics { n, ..Default::default() };
    if curve.len() < 2 || n == 0 {
        return m;
    }
    let start = curve[0].1;
    let end = curve[curve.len() - 1].1;
    m.total_return = if start > 0.0 { end / start - 1.0 } else { 0.0 };
    m.cagr = if start > 0.0 && end > 0.0 { (end / start).powf(periods_per_year / n as f64) - 1.0 } else { -1.0 };
    let mu = mean(r);
    let sd = sample_std(r);
    m.volatility = sd * periods_per_year.sqrt();
    m.sharpe_period = if sd > 0.0 { mu / sd } else { 0.0 };
    m.sharpe = m.sharpe_period * periods_per_year.sqrt();
    let factor = lo_factor(r, periods_per_year);
    m.sharpe_lo = m.sharpe_period * factor;
    m.sharpe_se = ((1.0 + m.sharpe_period * m.sharpe_period / 2.0) / n as f64).sqrt() * factor;
    let downside = (r.iter().map(|v| v.min(0.0).powi(2)).sum::<f64>() / n as f64).sqrt();
    m.sortino = if downside > 0.0 { mu / downside * periods_per_year.sqrt() } else { 0.0 };
    let (mdd, dur) = drawdown(&curve.iter().map(|(_, e)| *e).collect::<Vec<_>>());
    m.max_drawdown = mdd;
    m.max_drawdown_duration = dur;
    let mut sorted = r.clone();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let k = ((0.05 * n as f64).ceil() as usize).clamp(1, n);
    m.cvar_5 = -mean(&sorted[..k]);
    m.skew = skewness(r);
    m.kurtosis = excess_kurtosis(r);
    let mut months: BTreeMap<(i64, u32), f64> = BTreeMap::new();
    for (t, v) in rets.t.iter().zip(r) {
        let g = months.entry(time::month_key(*t)).or_insert(1.0);
        *g *= 1.0 + v;
    }
    m.worst_month = months.values().map(|g| g - 1.0).fold(f64::INFINITY, f64::min);
    m.t_stat = newey_west_t(r, None);
    m.autocorrelation_1 = autocorrelation(r, 1);
    m
}

// ---- the normal distribution ----

/// erf: the Maclaurin series below 1.5, Lentz's continued fraction for
/// erfc above it; accurate to about 1e-15.
fn erf(x: f64) -> f64 {
    let sign = if x < 0.0 { -1.0 } else { 1.0 };
    let x = x.abs();
    if x >= 6.0 {
        return sign;
    }
    if x < 1.5 {
        let mut term = x;
        let mut sum = x;
        let x2 = x * x;
        for n in 1..60 {
            term *= -x2 / n as f64;
            let add = term / (2 * n + 1) as f64;
            sum += add;
            if add.abs() < 1e-17 {
                break;
            }
        }
        return sign * sum * 2.0 / std::f64::consts::PI.sqrt();
    }
    // erfc(x) = exp(-x²)/sqrt(π) · 1/(x + (1/2)/(x + 1/(x + (3/2)/(x + ...))))
    let tiny = 1e-300;
    let mut f = x;
    let mut c = x;
    let mut d = 0.0;
    for n in 1..300 {
        let a = n as f64 / 2.0;
        d = x + a * d;
        if d.abs() < tiny {
            d = tiny;
        }
        d = 1.0 / d;
        c = x + a / c;
        if c.abs() < tiny {
            c = tiny;
        }
        let delta = c * d;
        f *= delta;
        if (delta - 1.0).abs() < 1e-16 {
            break;
        }
    }
    let erfc = (-x * x).exp() / (f * std::f64::consts::PI.sqrt());
    sign * (1.0 - erfc)
}

/// Standard normal cumulative distribution.
pub fn norm_cdf(x: f64) -> f64 {
    0.5 * (1.0 + erf(x / std::f64::consts::SQRT_2))
}

/// Standard normal quantile (Acklam's algorithm with a Newton refinement).
pub fn norm_ppf(p: f64) -> f64 {
    if p <= 0.0 {
        return f64::NEG_INFINITY;
    }
    if p >= 1.0 {
        return f64::INFINITY;
    }
    const A: [f64; 6] = [
        -3.969683028665376e1,
        2.209460984245205e2,
        -2.759285104469687e2,
        1.38357751867269e2,
        -3.066479806614716e1,
        2.506628277459239,
    ];
    const B: [f64; 5] = [-5.447609879822406e1, 1.615858368580409e2, -1.556989798598866e2, 6.680131188771972e1, -1.328068155288572e1];
    const C: [f64; 6] = [
        -7.784894002430293e-3,
        -3.223964580411365e-1,
        -2.400758277161838,
        -2.549732539343734,
        4.374664141464968,
        2.938163982698783,
    ];
    const D: [f64; 4] = [7.784695709041462e-3, 3.224671290700398e-1, 2.445134137142996, 3.754408661907416];
    let plow = 0.02425;
    let x = if p < plow {
        let q = (-2.0 * p.ln()).sqrt();
        (((((C[0] * q + C[1]) * q + C[2]) * q + C[3]) * q + C[4]) * q + C[5]) / ((((D[0] * q + D[1]) * q + D[2]) * q + D[3]) * q + 1.0)
    } else if p <= 1.0 - plow {
        let q = p - 0.5;
        let r = q * q;
        (((((A[0] * r + A[1]) * r + A[2]) * r + A[3]) * r + A[4]) * r + A[5]) * q / (((((B[0] * r + B[1]) * r + B[2]) * r + B[3]) * r + B[4]) * r + 1.0)
    } else {
        let q = (-2.0 * (1.0 - p).ln()).sqrt();
        -(((((C[0] * q + C[1]) * q + C[2]) * q + C[3]) * q + C[4]) * q + C[5]) / ((((D[0] * q + D[1]) * q + D[2]) * q + D[3]) * q + 1.0)
    };
    // One Newton step against the cdf.
    let e = norm_cdf(x) - p;
    let pdf = (-x * x / 2.0).exp() / (2.0 * std::f64::consts::PI).sqrt();
    if pdf > 0.0 {
        x - e / pdf
    } else {
        x
    }
}

// ---- Bailey and López de Prado ----

/// The probabilistic Sharpe ratio (Bailey and López de Prado, 2012): the
/// probability that the true per-period Sharpe ratio exceeds `benchmark`,
/// given the estimate `sr` over `n` observations with the returns' skewness
/// and excess kurtosis.
pub fn probabilistic_sharpe(sr: f64, benchmark: f64, n: usize, skew: f64, excess_kurtosis: f64) -> f64 {
    if n < 2 {
        return 0.0;
    }
    let raw_kurt = excess_kurtosis + 3.0;
    let var = 1.0 - skew * sr + (raw_kurt - 1.0) / 4.0 * sr * sr;
    if var <= 0.0 {
        return if sr > benchmark { 1.0 } else { 0.0 };
    }
    norm_cdf((sr - benchmark) * ((n - 1) as f64).sqrt() / var.sqrt())
}

/// The minimum track-record length (Bailey and López de Prado, 2012): the
/// observations needed for PSR(benchmark) to reach `confidence`.
pub fn min_track_record_length(sr: f64, benchmark: f64, skew: f64, excess_kurtosis: f64, confidence: f64) -> f64 {
    if sr <= benchmark {
        return f64::INFINITY;
    }
    let raw_kurt = excess_kurtosis + 3.0;
    let var = (1.0 - skew * sr + (raw_kurt - 1.0) / 4.0 * sr * sr).max(0.0);
    1.0 + var * (norm_ppf(confidence) / (sr - benchmark)).powi(2)
}

#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct DeflatedSharpe {
    /// The expected maximum per-period Sharpe ratio among `trials` with the
    /// given variance of trial Sharpe ratios (zero for one trial).
    pub expected_max_sr: f64,
    /// PSR against that expectation.
    pub dsr: f64,
    pub trials: usize,
}

/// The deflated Sharpe ratio (Bailey and López de Prado, 2014): PSR against
/// the expected maximum of `trials` Sharpe ratios whose variance across
/// trials is `var_trials` (per period).
pub fn deflated_sharpe(sr: f64, n: usize, skew: f64, excess_kurtosis: f64, trials: usize, var_trials: f64) -> DeflatedSharpe {
    const EULER: f64 = std::f64::consts::EULER_GAMMA;
    let trials = trials.max(1);
    let expected_max_sr = if trials == 1 || var_trials <= 0.0 {
        0.0
    } else {
        let nf = trials as f64;
        var_trials.sqrt() * ((1.0 - EULER) * norm_ppf(1.0 - 1.0 / nf) + EULER * norm_ppf(1.0 - 1.0 / (nf * std::f64::consts::E)))
    };
    DeflatedSharpe {
        expected_max_sr,
        dsr: probabilistic_sharpe(sr, expected_max_sr, n, skew, excess_kurtosis),
        trials,
    }
}

#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Pbo {
    /// The share of partition combinations in which the in-sample best trial
    /// ranks below the median out of sample.
    pub pbo: f64,
    pub combinations: usize,
    pub trials: usize,
    pub partitions: usize,
    /// Mean out-of-sample logit of the in-sample best.
    pub mean_logit: f64,
}

fn combinations(n: usize, k: usize, f: &mut dyn FnMut(&[usize])) {
    fn rec(start: usize, n: usize, k: usize, cur: &mut Vec<usize>, f: &mut dyn FnMut(&[usize])) {
        if cur.len() == k {
            f(cur);
            return;
        }
        for i in start..n {
            cur.push(i);
            rec(i + 1, n, k, cur, f);
            cur.pop();
        }
    }
    let mut cur = Vec::with_capacity(k);
    rec(0, n, k, &mut cur, f);
}

/// Per-period Sharpe ratio over a slice of rows.
fn sharpe_rows(rows: &[&[f64]], col: usize) -> f64 {
    let x: Vec<f64> = rows.iter().map(|r| r[col]).collect();
    let sd = sample_std(&x);
    if sd > 0.0 {
        mean(&x) / sd
    } else {
        0.0
    }
}

/// The probability of backtest overfitting by combinatorially symmetric
/// cross-validation (Bailey, Borwein, López de Prado and Zhu, 2014). `rows`
/// holds one period per row and one trial per column; the rows are cut into
/// `partitions` (even) blocks, every half of the blocks is an in-sample set,
/// the trial best in sample (by Sharpe ratio) is ranked out of sample, and
/// PBO is the share of combinations where that rank is below the median.
pub fn pbo_cscv(rows: &[Vec<f64>], partitions: usize) -> Option<Pbo> {
    let n_rows = rows.len();
    let trials = rows.first().map(|r| r.len()).unwrap_or(0);
    let partitions = partitions - partitions % 2;
    if trials < 2 || partitions < 2 || n_rows < partitions * 2 {
        return None;
    }
    let block = n_rows / partitions;
    let blocks: Vec<&[Vec<f64>]> = (0..partitions).map(|i| &rows[i * block..(i + 1) * block]).collect();
    let mut below = 0usize;
    let mut count = 0usize;
    let mut logit_sum = 0.0;
    combinations(partitions, partitions / 2, &mut |is| {
        let mut is_rows: Vec<&[f64]> = Vec::new();
        let mut oos_rows: Vec<&[f64]> = Vec::new();
        for (i, b) in blocks.iter().enumerate() {
            let rows_of: Vec<&[f64]> = b.iter().map(|r| r.as_slice()).collect();
            if is.contains(&i) {
                is_rows.extend(rows_of);
            } else {
                oos_rows.extend(rows_of);
            }
        }
        let is_sr: Vec<f64> = (0..trials).map(|c| sharpe_rows(&is_rows, c)).collect();
        let best = (0..trials).max_by(|a, b| is_sr[*a].partial_cmp(&is_sr[*b]).unwrap().then(b.cmp(a))).unwrap();
        let oos_sr: Vec<f64> = (0..trials).map(|c| sharpe_rows(&oos_rows, c)).collect();
        let rank = 1 + oos_sr.iter().enumerate().filter(|(i, s)| *i != best && **s < oos_sr[best]).count();
        let omega = rank as f64 / (trials as f64 + 1.0);
        let logit = (omega / (1.0 - omega)).ln();
        if logit <= 0.0 {
            below += 1;
        }
        logit_sum += logit;
        count += 1;
    });
    Some(Pbo {
        pbo: below as f64 / count as f64,
        combinations: count,
        trials,
        partitions,
        mean_logit: logit_sum / count as f64,
    })
}

// ---- the block bootstrap ----

/// A small deterministic generator (xorshift64*), so that intervals are
/// reproducible from a seed.
struct Rng(u64);

impl Rng {
    fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next_u64() % n as u64) as usize
    }
}

/// A circular block bootstrap confidence interval for the annualised Sharpe
/// ratio: `reps` resamples of blocks of `block` bars (`None`: n^(1/3)),
/// returning the `(1 − level) / 2` and `(1 + level) / 2` quantiles.
pub fn block_bootstrap_sharpe(r: &[f64], periods_per_year: f64, block: Option<usize>, reps: usize, level: f64, seed: u64) -> Option<(f64, f64)> {
    let n = r.len();
    if n < 4 || reps == 0 {
        return None;
    }
    let block = block.unwrap_or_else(|| ((n as f64).powf(1.0 / 3.0).round() as usize).max(1)).clamp(1, n);
    let mut rng = Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1);
    let mut samples = Vec::with_capacity(reps);
    let mut buf = Vec::with_capacity(n + block);
    for _ in 0..reps {
        buf.clear();
        while buf.len() < n {
            let start = rng.below(n);
            for j in 0..block {
                buf.push(r[(start + j) % n]);
            }
        }
        buf.truncate(n);
        let sd = sample_std(&buf);
        samples.push(if sd > 0.0 { mean(&buf) / sd * periods_per_year.sqrt() } else { 0.0 });
    }
    samples.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let q = |p: f64| samples[((p * (reps - 1) as f64).round() as usize).min(reps - 1)];
    Some((q((1.0 - level) / 2.0), q((1.0 + level) / 2.0)))
}

// ---- trading metrics ----

#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct TradingMetrics {
    /// Annualised Σ |quantity| · price over the mean equity.
    pub turnover: f64,
    pub fill_ratio: f64,
    pub avg_participation: f64,
    pub max_participation: f64,
    /// Mean gross and net exposure as a fraction of equity.
    pub avg_gross: f64,
    pub avg_net: f64,
    pub max_leverage: f64,
    /// Annualised costs (commissions, fees, slippage, impact, funding) over
    /// the mean equity.
    pub cost_drag: f64,
    /// Annualised impact alone over the mean equity (what capacity scales).
    pub impact_drag: f64,
    pub fills: usize,
    pub decisions: usize,
    pub dropped: usize,
}

fn mean_equity(r: &RunResult) -> f64 {
    let e: Vec<f64> = r.equity_curve.iter().map(|(_, e)| *e).collect();
    mean(&e)
}

fn years(r: &RunResult, periods_per_year: f64) -> f64 {
    (r.equity_curve.len().saturating_sub(1) as f64 / periods_per_year).max(1.0 / periods_per_year)
}

pub fn trading_metrics(r: &RunResult, periods_per_year: f64) -> TradingMetrics {
    let eq = mean_equity(r);
    let y = years(r, periods_per_year);
    let per_equity = |x: f64| if eq > 0.0 { x / eq / y } else { 0.0 };
    let funding = r.funding.margin_interest + r.funding.borrow_fees - r.funding.cash_interest - r.funding.short_rebate;
    let costs = r.costs.commissions + r.costs.fees + r.costs.slippage + r.costs.impact + funding;
    let gross: Vec<f64> = r.exposure.iter().filter(|e| e.equity > 0.0).map(|e| e.gross / e.equity).collect();
    let net: Vec<f64> = r.exposure.iter().filter(|e| e.equity > 0.0).map(|e| e.net / e.equity).collect();
    TradingMetrics {
        turnover: per_equity(r.costs.turnover),
        fill_ratio: r.liquidity.fill_ratio,
        avg_participation: r.liquidity.avg_participation,
        max_participation: r.liquidity.max_participation,
        avg_gross: mean(&gross),
        avg_net: mean(&net),
        max_leverage: r.exposure.iter().map(|e| e.leverage).fold(0.0, f64::max),
        cost_drag: per_equity(costs),
        impact_drag: per_equity(r.costs.impact),
        fills: r.fills.len(),
        decisions: r.decisions.len(),
        dropped: r.dropped.len(),
    }
}

/// The capacity at a cost threshold: with square-root impact, scaling the
/// book by k scales the impact drag by sqrt(k), so the NAV at which the
/// annual impact drag reaches `threshold` is the initial NAV times
/// (threshold / drag)². `None` when the run paid no impact (the model was
/// off or nothing traded): capacity is then unmeasured, not infinite.
pub fn capacity(r: &RunResult, periods_per_year: f64, threshold: f64) -> Option<f64> {
    let tm = trading_metrics(r, periods_per_year);
    let start = r.equity_curve.first().map(|(_, e)| *e)?;
    if tm.impact_drag <= 0.0 || threshold <= 0.0 {
        return None;
    }
    Some(start * (threshold / tm.impact_drag).powi(2))
}

/// A run's returns per reporting period, with the NAV change behind each:
/// per decision bar, or per calendar day (the bars of a day summed on a
/// fixed base, compounded otherwise).
pub fn period_returns(r: &RunResult, by_day: bool) -> Vec<(i64, f64, f64)> {
    period_returns_on(r, by_day, None)
}

/// The period returns per day on a reporting calendar: every calendar day
/// from the run's first to its last, a day without activity counting zero,
/// and only those days (a book reported on its lead market's sessions).
pub fn period_returns_on(r: &RunResult, by_day: bool, calendar: Option<&std::collections::BTreeSet<i64>>) -> Vec<(i64, f64, f64)> {
    let rows = period_returns_raw(r, by_day);
    let Some(cal) = calendar.filter(|_| by_day) else { return rows };
    let (Some(&(first, _)), Some(&(last, _))) = (r.equity_curve.first(), r.equity_curve.last()) else {
        return vec![];
    };
    let (lo, hi) = (time::day_key(first) * time::DAY, time::day_key(last) * time::DAY);
    let by: BTreeMap<i64, (f64, f64)> = rows.into_iter().map(|(t, x, p)| (t, (x, p))).collect();
    cal.range(lo..=hi)
        .map(|d| {
            let (x, p) = by.get(d).copied().unwrap_or((0.0, 0.0));
            (*d, x, p)
        })
        .collect()
}

fn period_returns_raw(r: &RunResult, by_day: bool) -> Vec<(i64, f64, f64)> {
    let pnl: Vec<f64> = r.equity_curve.windows(2).map(|w| w[1].1 - w[0].1).collect();
    let bars: Vec<(i64, f64, f64)> = r.bar_returns().into_iter().zip(pnl).map(|((t, x), p)| (t, x, p)).collect();
    if !by_day {
        return bars;
    }
    let mut days: Vec<(i64, f64, f64)> = Vec::new();
    for (t, x, p) in bars {
        let d = time::day_key(t) * time::DAY;
        match days.last_mut() {
            Some(last) if last.0 == d => {
                last.1 = if r.base_capital.is_some() { last.1 + x } else { (1.0 + last.1) * (1.0 + x) - 1.0 };
                last.2 += p;
            }
            _ => days.push((d, x, p)),
        }
    }
    days
}

/// The two conventions a book is reported in (data-bundle doc, section 7):
/// returns compounded into a curve (CAGR and drawdown of the compounded
/// curve, Sharpe with the sample and the population deviation), and the
/// additive figures of a fixed base (the mean return a year, the drawdown
/// of the cumulative sum, profit factors and trades).
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ConventionMetrics {
    pub periods: usize,
    pub periods_per_year: f64,
    pub cagr: f64,
    pub max_drawdown: f64,
    pub sharpe: f64,
    /// Mean over the population standard deviation, annualised.
    pub sharpe_population: f64,
    pub volatility_population: f64,
    /// Mean period return times the periods a year.
    pub annual_return: f64,
    /// Largest fall of the cumulative return from its running peak.
    pub max_drawdown_additive: f64,
    pub total_pnl: f64,
    /// Gains over losses of the periods.
    pub profit_factor: f64,
    /// Round trips: a position from flat back to flat, per security.
    pub trades: usize,
    pub profit_factor_trades: f64,
    pub win_rate: f64,
}

pub fn convention_metrics(r: &RunResult, periods_per_year: f64, by_day: bool) -> ConventionMetrics {
    convention_metrics_on(r, periods_per_year, by_day, None)
}

pub fn convention_metrics_on(r: &RunResult, periods_per_year: f64, by_day: bool, calendar: Option<&std::collections::BTreeSet<i64>>) -> ConventionMetrics {
    let rows = period_returns_on(r, by_day, calendar);
    let x: Vec<f64> = rows.iter().map(|(_, x, _)| *x).collect();
    let n = x.len();
    let mut m = ConventionMetrics {
        periods: n,
        periods_per_year,
        ..Default::default()
    };
    if n == 0 {
        return m;
    }
    let mu = mean(&x);
    let pop = (x.iter().map(|v| (v - mu).powi(2)).sum::<f64>() / n as f64).sqrt();
    let sd = sample_std(&x);
    let mut e = 1.0;
    let mut peak = 1.0;
    let mut mdd: f64 = 0.0;
    let mut cum = 0.0;
    let mut cum_peak: f64 = 0.0;
    let mut mdd_add: f64 = 0.0;
    for v in &x {
        e *= 1.0 + v;
        peak = f64::max(peak, e);
        mdd = mdd.max(1.0 - e / peak);
        cum += v;
        cum_peak = cum_peak.max(cum);
        mdd_add = mdd_add.max(cum_peak - cum);
    }
    m.cagr = if e > 0.0 { e.powf(periods_per_year / n as f64) - 1.0 } else { -1.0 };
    m.max_drawdown = mdd;
    m.sharpe = if sd > 0.0 { mu / sd * periods_per_year.sqrt() } else { 0.0 };
    m.sharpe_population = if pop > 0.0 { mu / pop * periods_per_year.sqrt() } else { 0.0 };
    m.volatility_population = pop * periods_per_year.sqrt();
    m.annual_return = mu * periods_per_year;
    m.max_drawdown_additive = mdd_add;
    m.total_pnl = rows.iter().map(|(_, _, p)| *p).sum();
    let factor = |vals: &mut dyn Iterator<Item = f64>| {
        let (mut gain, mut loss) = (0.0, 0.0);
        for v in vals {
            if v > 0.0 {
                gain += v;
            } else {
                loss -= v;
            }
        }
        if loss > 0.0 {
            gain / loss
        } else {
            f64::INFINITY
        }
    };
    m.profit_factor = factor(&mut rows.iter().map(|(_, _, p)| *p));
    // Round trips from the fills: cash flows of a security from flat to flat.
    let mut open: BTreeMap<u32, (f64, f64)> = BTreeMap::new();
    let mut trades: Vec<f64> = Vec::new();
    for f in &r.fills {
        let mult = r.multipliers.get(&f.equity).copied().unwrap_or(1.0);
        let (pos, flow) = open.entry(f.equity).or_insert((0.0, 0.0));
        *pos += f.quantity;
        *flow += -f.quantity * f.price * mult - f.commission - f.fee;
        if pos.abs() < 1e-9 {
            trades.push(*flow);
            open.remove(&f.equity);
        }
    }
    m.trades = trades.len();
    m.profit_factor_trades = factor(&mut trades.iter().copied());
    m.win_rate = if trades.is_empty() {
        0.0
    } else {
        trades.iter().filter(|p| **p > 0.0).count() as f64 / trades.len() as f64
    };
    m
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_normal_distribution_round_trips() {
        for &p in &[0.001, 0.025, 0.1, 0.5, 0.9, 0.975, 0.999] {
            let x = norm_ppf(p);
            assert!((norm_cdf(x) - p).abs() < 1e-9, "p {} x {} cdf {}", p, x, norm_cdf(x));
        }
        assert!((norm_ppf(0.975) - 1.959_963_984_540_054).abs() < 1e-8, "{}", norm_ppf(0.975));
        assert!((norm_cdf(1.0) - 0.841_344_746_068_543).abs() < 1e-9, "{}", norm_cdf(1.0));
        assert!((norm_cdf(-2.5) - 0.006_209_665_325_776_132).abs() < 1e-9, "{}", norm_cdf(-2.5));
    }

    #[test]
    fn combinations_count_binomially() {
        let mut n = 0;
        combinations(6, 3, &mut |_| n += 1);
        assert_eq!(n, 20);
    }
}
