//! `hill_tail` (Q-4.4): the tail index α of one tail of a series, by Hill's
//! (1975) estimator at the k largest values, with k chosen by Hall's (1990)
//! subsample bootstrap; the null is that α is at least α₀ (the tail is no
//! heavier than a Pareto tail of index α₀).
//!
//! With x₍₁₎ ≥ x₍₂₎ ≥ … the tail's values in decreasing order, Hill's
//! estimate of γ = 1/α is H = (1/k) Σᵢ₌₁ᵏ ln(x₍ᵢ₎ / x₍ₖ₊₁₎), and α̂ = 1/H.
//! Above the threshold x₍ₖ₊₁₎ a Pareto tail of index α makes the k log
//! excesses independent exponentials of rate α (Rényi's representation), so
//! α k H follows Gamma(k, 1): the p-value P(Gamma(k, 1) ≥ α₀ k H) and the
//! interval for α are exact given k, under independent exceedances (the
//! dependence treatment `iid`, on the exceedances).
//!
//! Hall's choice of k trades the estimator's variance, which falls with k,
//! against its bias, which grows as k reaches into the body of the
//! distribution: `reps` resamples of n₁ = ⌊n^ε⌋ < n values, drawn with
//! replacement, estimate the mean squared error of Hill's estimate at every
//! k₁ < n₁ against a pilot estimate at ⌊2√n⌋; the minimising k₁ is scaled to
//! the full sample as k = ⌊k₁ (n/n₁)^(2/3)⌋, the rate of the optimal k when
//! the tail's second-order term decays as fast as its first (Hall's model).
//! The defaults (ε = 0.955, the pilot at 2√n, 1,000 resamples) are those of
//! the `hall` procedure of R's `tea` package.

use std::sync::LazyLock;

use crate::tests::dist;
use crate::tests::framework::{param, Dependence, InputSpec, Param, ParamKind, ParamSpec, Params, Shape, Table, Test, TestError, TestResult};
use crate::tests::rng::Rng;
use crate::tests::set::samples;

pub struct HillTail;

/// Fewer values in the tail leave Hall's subsamples too short to locate the
/// minimum of the mean squared error.
const MIN_TAIL: usize = 50;

const INPUTS: [InputSpec; 1] = [InputSpec {
    name: "series",
    shape: Shape::Series,
    columns: &["x"],
}];

static PARAMS: LazyLock<[ParamSpec; 6]> = LazyLock::new(|| {
    [
        ParamSpec {
            name: "alpha0",
            kind: ParamKind::Float,
            default: Some(Param::Float(4.0)),
            range: Some((1e-6, 1e6)),
            doc: "the tail index of the null (4: the fourth moment the kurtosis-based tests assume)",
        },
        ParamSpec {
            name: "tail",
            kind: ParamKind::Text,
            default: Some(Param::Text("lower".into())),
            range: None,
            doc: "lower (the losses, −x for x < 0), upper (x for x > 0) or absolute (|x| for x ≠ 0)",
        },
        ParamSpec {
            name: "k",
            kind: ParamKind::Int,
            default: None,
            range: Some((1.0, 1e12)),
            doc: "the number of upper order statistics (default: Hall's bootstrap)",
        },
        ParamSpec {
            name: "reps",
            kind: ParamKind::Int,
            default: Some(Param::Int(1_000)),
            range: Some((1.0, 1e9)),
            doc: "Hall's bootstrap replications",
        },
        ParamSpec {
            name: "subsample",
            kind: ParamKind::Float,
            default: Some(Param::Float(0.955)),
            range: Some((0.5, 0.999)),
            doc: "the exponent ε of Hall's subsample size n^ε",
        },
        ParamSpec {
            name: "level",
            kind: ParamKind::Float,
            default: Some(Param::Float(0.95)),
            range: Some((0.5, 0.9999)),
            doc: "the confidence level of the interval for α",
        },
    ]
});

/// Hill's estimate of γ = 1/α from the k largest of `desc`, positive values
/// in decreasing order, k < desc.len(): (1/k) Σᵢ<ₖ ln desc[i] − ln desc[k].
fn hill(desc: &[f64], k: usize) -> f64 {
    desc[..k].iter().map(|v| v.ln()).sum::<f64>() / k as f64 - desc[k].ln()
}

/// Hall's choice of k for a tail.
struct HallChoice {
    /// The number of upper order statistics for the full sample.
    k: usize,
    /// The subsample size n₁.
    subsample: usize,
    /// The k₁ minimising the bootstrap mean squared error in subsamples.
    k_subsample: usize,
    /// The k of the pilot estimate.
    pilot: usize,
}

/// Hall's (1990) bootstrap choice of k for `desc`, at least two positive
/// values in decreasing order: `reps` subsamples of ⌊n^exponent⌋ values
/// drawn with replacement, replication r from `Rng::stream(seed, r)`. The
/// values are drawn by their rank, so the choice depends on the tail's
/// values and not on their order in the series.
fn hall_k(desc: &[f64], reps: usize, exponent: f64, seed: u64) -> HallChoice {
    let n = desc.len();
    let logs: Vec<f64> = desc.iter().map(|v| v.ln()).collect();
    let pilot = ((2.0 * (n as f64).sqrt()).floor() as usize).clamp(1, n - 1);
    let gamma_pilot = hill(desc, pilot);
    let n1 = ((n as f64).powf(exponent).floor() as usize).clamp(2, n);
    let mut mse = vec![0.0; n1 - 1];
    let mut ranks = vec![0usize; n1];
    for r in 0..reps {
        let mut rng = Rng::stream(seed, r as u64);
        ranks.iter_mut().for_each(|i| *i = rng.below(n));
        // Ranks in increasing order are values in decreasing order.
        ranks.sort_unstable();
        let mut sum = 0.0;
        for k1 in 1..n1 {
            sum += logs[ranks[k1 - 1]];
            let e = sum / k1 as f64 - logs[ranks[k1]] - gamma_pilot;
            mse[k1 - 1] += e * e;
        }
    }
    let mut k_subsample = 1;
    for (i, v) in mse.iter().enumerate() {
        if *v < mse[k_subsample - 1] {
            k_subsample = i + 1;
        }
    }
    let k = ((k_subsample as f64 * (n as f64 / n1 as f64).powf(2.0 / 3.0)).floor() as usize).clamp(1, n - 1);
    HallChoice { k, subsample: n1, k_subsample, pilot }
}

/// The p quantile of Gamma(shape, 1), by bisection on the regularised
/// incomplete gamma function.
fn gamma_quantile(shape: f64, p: f64) -> f64 {
    let mut hi = shape + 10.0 * shape.sqrt() + 10.0;
    while dist::gamma_p(shape, hi) < p {
        hi *= 2.0;
    }
    let mut lo = 0.0;
    for _ in 0..200 {
        let mid = 0.5 * (lo + hi);
        if dist::gamma_p(shape, mid) < p {
            lo = mid;
        } else {
            hi = mid;
        }
        if hi - lo <= 1e-15 * hi {
            break;
        }
    }
    0.5 * (lo + hi)
}

/// The tail's positive values in decreasing order.
fn tail_values(x: &[f64], tail: &str) -> Result<Vec<f64>, TestError> {
    let mut y: Vec<f64> = match tail {
        "upper" => x.iter().copied().filter(|v| *v > 0.0).collect(),
        "lower" => x.iter().filter(|v| **v < 0.0).map(|v| -v).collect(),
        "absolute" => x.iter().filter(|v| **v != 0.0).map(|v| v.abs()).collect(),
        other => return Err(TestError::Input(format!("`tail` is lower, upper or absolute, not `{}`", other))),
    };
    y.sort_by(|a, b| b.total_cmp(a));
    Ok(y)
}

impl Test for HillTail {
    fn name(&self) -> &str {
        "hill_tail"
    }
    fn version(&self) -> &str {
        "1"
    }
    fn inputs(&self) -> &[InputSpec] {
        &INPUTS
    }
    fn params(&self) -> &[ParamSpec] {
        &PARAMS[..]
    }
    fn null(&self) -> &str {
        "the tail index of the chosen tail is at least alpha0"
    }
    fn dependence(&self) -> Dependence {
        Dependence::Iid
    }
    /// The statistic is α̂ at Hall's k (or the given k); the p-value
    /// P(Gamma(k, 1) ≥ α₀ k H); the interval the exact one for α at `level`;
    /// `df` the 2k degrees of freedom of the χ² form 2 α k H ~ χ²(2k).
    fn run(&self, tables: &[Table], params: &Params, seed: u64) -> Result<TestResult, TestError> {
        let x = samples::series(tables, 1)?;
        let tail = match param(self, params, "tail") {
            Some(Param::Text(t)) => t.as_str(),
            Some(other) => return Err(TestError::Input(format!("`tail` is lower, upper or absolute, not `{}`", other))),
            None => "lower",
        };
        let desc = tail_values(x, tail)?;
        let n = desc.len();
        let alpha0 = samples::required(self, params, "alpha0")?;
        let level = samples::required(self, params, "level")?;
        let (k, how) = match samples::count(self, params, "k")? {
            Some(k) if k < n => (k, format!("k {} fixed by the parameter", k)),
            Some(k) => return Err(TestError::Precondition(format!("k = {} needs more than {} values in the tail; it has {}", k, k, n))),
            None => {
                if n < MIN_TAIL {
                    return Err(TestError::Precondition(format!("{} values in the {} tail; Hall's bootstrap needs at least {}", n, tail, MIN_TAIL)));
                }
                let reps = samples::count(self, params, "reps")?.unwrap_or(1_000);
                let exponent = samples::required(self, params, "subsample")?;
                let h = hall_k(&desc, reps, exponent, seed);
                (
                    h.k,
                    format!(
                        "k {} by Hall's bootstrap: subsamples of {}, their k {}, pilot k {}, {} replications",
                        h.k, h.subsample, h.k_subsample, h.pilot, reps
                    ),
                )
            }
        };
        let gamma = hill(&desc, k);
        if desc[0] == desc[k] || gamma <= 0.0 {
            return Err(TestError::Precondition(format!("the {} largest values of the tail are equal: Hill's estimate is undefined", k + 1)));
        }
        let kf = k as f64;
        let spacings = alpha0 * kf * gamma;
        let interval = (gamma_quantile(kf, (1.0 - level) / 2.0) / (kf * gamma), gamma_quantile(kf, (1.0 + level) / 2.0) / (kf * gamma), level);
        Ok(TestResult {
            statistic: 1.0 / gamma,
            p_value: Some(dist::gamma_q(kf, spacings)),
            interval: Some(interval),
            effect: None,
            df: Some(2.0 * kf),
            n,
            notes: vec![
                format!("{} tail: {} of {} observations", tail, n, x.len()),
                how,
                format!("threshold {} (order statistic {} from the top)", desc[k], k + 1),
                format!("Hill's gamma = 1/alpha {}", gamma),
            ],
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_gamma_quantile_inverts_the_distribution() {
        for (shape, p) in [(1.0, 0.025), (5.0, 0.5), (400.0, 0.975)] {
            let q = gamma_quantile(shape, p);
            assert!((dist::gamma_p(shape, q) - p).abs() < 1e-12, "{} {}: {}", shape, p, q);
        }
        // Gamma(1, 1) is the unit exponential.
        assert!((gamma_quantile(1.0, 0.5) - std::f64::consts::LN_2).abs() < 1e-13);
    }

    #[test]
    fn hill_on_a_pareto_staircase_is_its_log_spacing() {
        // Values e³, e², e, 1: the log excesses over 1 are 3, 2, 1.
        let desc = [3f64.exp(), 2f64.exp(), 1f64.exp(), 1.0];
        assert!((hill(&desc, 3) - 2.0).abs() < 1e-14);
    }
}
