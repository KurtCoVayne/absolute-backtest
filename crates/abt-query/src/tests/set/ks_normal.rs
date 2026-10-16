//! `ks_normal` (Q-4.4): normality by the Kolmogorov–Smirnov distance
//! between the series' empirical distribution and the normal with the
//! sample mean and standard deviation (n − 1), D = sup |Fₙ − Φ((x − x̄)/s)|.
//!
//! Estimating the mean and deviation makes the fit closer than a fixed
//! normal's, so the Kolmogorov–Smirnov table is too lenient; Lilliefors
//! (1967) corrected it by simulating D on normal samples, re-fitting each.
//! For independent observations that correction is in the notes, simulated
//! with the framework's generator (`lilliefors_null`). Returns are
//! dependent, so the p-value is the stationary bootstrap's with the null
//! imposed by re-centring the estimated empirical process (Babu and Rao
//! 2004): a resample's statistic is
//! sup |(F*ₙ − Fₙ) − (Φ((x − x̄*)/s*) − Φ((x − x̄)/s))|, the parameters
//! re-fitted to each resample as Lilliefors re-fitted them to each sample.

use crate::tests::dist::norm_cdf;
use crate::tests::framework::{Dependence, InputSpec, ParamSpec, Params, Shape, Table, Test, TestError, TestResult};
use crate::tests::resample::stationary_bootstrap;
use crate::tests::rng::Rng;
use crate::tests::set::samples::{self, Moments};

pub struct KsNormal;

/// Fewer observations leave the stationary bootstrap too few distinct
/// resamples to approximate a distribution.
const MIN_N: usize = 8;

/// Added to the test's seed for the simulated null of independent
/// observations, so that its draws are not the bootstrap's.
const NULL_SEED: u64 = 0x4C49_4C4C_4945_464F;

const INPUTS: [InputSpec; 1] = [InputSpec {
    name: "series",
    shape: Shape::Series,
    columns: &["x"],
}];

const PARAMS: [ParamSpec; 2] = [samples::BLOCK_PARAM, samples::REPS_PARAM];

/// D for values sorted in increasing order against the normal with mean
/// `mean` and deviation `sd`: the largest gap above or below a step of the
/// empirical distribution, max over i of i/n − Φ(z₍ᵢ₎) and Φ(z₍ᵢ₎) − (i − 1)/n.
/// Tied values are handled by the same maxima, which reach the step's top
/// at the tie's last value and its bottom at its first.
fn distance_sorted(sorted: &[f64], mean: f64, sd: f64) -> f64 {
    let n = sorted.len() as f64;
    let mut d: f64 = 0.0;
    for (i, v) in sorted.iter().enumerate() {
        let p = norm_cdf((v - mean) / sd);
        d = d.max((i + 1) as f64 / n - p).max(p - i as f64 / n);
    }
    d
}

/// Lilliefors' statistic: the Kolmogorov–Smirnov distance from the normal
/// with the sample mean and standard deviation (n − 1). `None` for fewer
/// than two values or a constant series, which fit no normal.
pub fn lilliefors(x: &[f64]) -> Option<f64> {
    if x.len() < 2 || samples::is_constant(x) {
        return None;
    }
    let m = Moments::of(x);
    let sd = m.sd();
    if sd <= 0.0 || !sd.is_finite() {
        return None;
    }
    let mut sorted = x.to_vec();
    sorted.sort_by(f64::total_cmp);
    Some(distance_sorted(&sorted, m.mean, sd))
}

/// Lilliefors' statistic on `reps` samples of `n` independent standard
/// normal draws, replication r drawn from `Rng::stream(seed, r)`, in
/// increasing order: its null distribution for independent observations,
/// simulated as Lilliefors simulated his table.
pub fn lilliefors_null(n: usize, reps: usize, seed: u64) -> Vec<f64> {
    let mut z = vec![0.0; n];
    let mut out: Vec<f64> = (0..reps)
        .map(|r| {
            let mut rng = Rng::stream(seed, r as u64);
            z.iter_mut().for_each(|v| *v = rng.normal());
            let m = Moments::of(&z);
            z.sort_by(f64::total_cmp);
            distance_sorted(&z, m.mean, m.sd())
        })
        .collect();
    out.sort_by(f64::total_cmp);
    out
}

/// The points where the densities of N(a, b²) and N(m, s²) are equal, at
/// most two: the roots of (x − a)²/b² − (x − m)²/s² = 2 ln(s/b), by the
/// cancellation-free form of the quadratic formula.
fn crossings(a: f64, b: f64, m: f64, s: f64) -> Vec<f64> {
    let (p, q) = (1.0 / (b * b), 1.0 / (s * s));
    let qa = p - q;
    let qb = -2.0 * (p * a - q * m);
    let qc = p * a * a - q * m * m - 2.0 * (s / b).ln();
    let roots = if qa == 0.0 {
        if qb == 0.0 {
            vec![]
        } else {
            vec![-qc / qb]
        }
    } else {
        let disc = qb * qb - 4.0 * qa * qc;
        if disc < 0.0 {
            vec![]
        } else {
            let h = -0.5 * (qb + qb.signum() * disc.sqrt());
            if h == 0.0 {
                vec![0.0]
            } else {
                vec![h / qa, qc / h]
            }
        }
    };
    roots.into_iter().filter(|x| x.is_finite()).collect()
}

/// The series' distinct values in increasing order, the number of
/// observations at or below each, and each observation's distinct value by
/// its position among them.
struct Steps {
    values: Vec<f64>,
    at_or_below: Vec<usize>,
    index: Vec<usize>,
}

impl Steps {
    fn of(x: &[f64]) -> Steps {
        let mut order: Vec<usize> = (0..x.len()).collect();
        order.sort_by(|a, b| x[*a].total_cmp(&x[*b]));
        let mut values: Vec<f64> = Vec::new();
        let mut at_or_below: Vec<usize> = Vec::new();
        let mut index = vec![0; x.len()];
        for (pos, i) in order.into_iter().enumerate() {
            if values.last() != Some(&x[i]) {
                values.push(x[i]);
                at_or_below.push(0);
            }
            let j = values.len() - 1;
            at_or_below[j] = pos + 1;
            index[i] = j;
        }
        Steps { values, at_or_below, index }
    }
}

/// The re-centred distance of one resample, given as the positions of its
/// observations' distinct values: the supremum over x of
/// |(F*ₙ − Fₙ)(x) − (Φ((x − x̄*)/s*) − Φ((x − x̄)/s))|. The step part is
/// constant between distinct values and the smooth part's extrema there lie
/// where the two normal densities cross, so the supremum is a maximum over
/// both sides of each distinct value and those crossings. A constant
/// resample fits no normal and counts as 1, the largest distance.
fn recentred_distance(steps: &Steps, phi: &[f64], mean: f64, sd: f64, resample: &[f64]) -> f64 {
    let n = resample.len() as f64;
    let mut counts = vec![0usize; steps.values.len()];
    for v in resample {
        counts[*v as usize] += 1;
    }
    if counts.iter().filter(|c| **c > 0).count() < 2 {
        return 1.0;
    }
    let mean_star = counts.iter().zip(&steps.values).map(|(c, u)| *c as f64 * u).sum::<f64>() / n;
    let ss = counts.iter().zip(&steps.values).map(|(c, u)| *c as f64 * (u - mean_star) * (u - mean_star)).sum::<f64>();
    let sd_star = (ss / (n - 1.0)).sqrt();
    if sd_star <= 0.0 || !sd_star.is_finite() {
        return 1.0;
    }
    let mut step = Vec::with_capacity(counts.len());
    let mut cum = 0usize;
    let mut d: f64 = 0.0;
    let mut below = 0.0;
    for (j, c) in counts.iter().enumerate() {
        cum += c;
        let s = (cum as f64 - steps.at_or_below[j] as f64) / n;
        let delta = norm_cdf((steps.values[j] - mean_star) / sd_star) - phi[j];
        d = d.max((s - delta).abs()).max((below - delta).abs());
        step.push(s);
        below = s;
    }
    for c in crossings(mean_star, sd_star, mean, sd) {
        let j = steps.values.partition_point(|u| *u <= c);
        let s = if j == 0 { 0.0 } else { step[j - 1] };
        let delta = norm_cdf((c - mean_star) / sd_star) - norm_cdf((c - mean) / sd);
        d = d.max((s - delta).abs());
    }
    d
}

impl Test for KsNormal {
    fn name(&self) -> &str {
        "ks_normal"
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
        "the series is normally distributed"
    }
    fn dependence(&self) -> Dependence {
        Dependence::BlockBootstrap { block: None, reps: samples::REPS }
    }
    /// The statistic is D; the p-value the re-centred stationary
    /// bootstrap's; the notes carry Lilliefors' p-value for independent
    /// observations (simulated with as many replications), the block and
    /// the replications.
    fn run(&self, tables: &[Table], params: &Params, seed: u64) -> Result<TestResult, TestError> {
        let x = samples::series(tables, MIN_N)?;
        let m = Moments::of(x);
        let sd = m.sd();
        let d = lilliefors(x).ok_or_else(|| TestError::Precondition("a constant series fits no normal".into()))?;
        let (block, reps) = samples::bootstrap_plan(self, params, x)?;
        let steps = Steps::of(x);
        let phi: Vec<f64> = steps.values.iter().map(|u| norm_cdf((u - m.mean) / sd)).collect();
        let positions: Vec<f64> = steps.index.iter().map(|j| *j as f64).collect();
        let resampled = stationary_bootstrap(&positions, block, reps, seed, &|r: &[f64]| recentred_distance(&steps, &phi, m.mean, sd, r));
        let null = lilliefors_null(x.len(), reps, seed.wrapping_add(NULL_SEED));
        let p_iid = (1 + null.len() - null.partition_point(|v| *v < d)) as f64 / (1 + null.len()) as f64;
        Ok(TestResult {
            statistic: d,
            p_value: Some(samples::upper_p(&resampled, d)),
            interval: None,
            effect: None,
            df: None,
            n: x.len(),
            notes: vec![
                format!("Lilliefors p-value {} (independent observations, {} simulated normal samples)", p_iid, reps),
                format!("stationary bootstrap: mean block {}, {} replications", block, reps),
            ],
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_densities_cross_where_they_are_equal() {
        let density = |x: f64, a: f64, b: f64| (-(x - a) * (x - a) / (2.0 * b * b)).exp() / b;
        for (a, b, m, s) in [(0.1, 1.2, 0.0, 1.0), (-0.3, 0.7, 0.2, 0.9), (0.5, 1.0, -0.5, 1.0)] {
            let roots = crossings(a, b, m, s);
            assert!(!roots.is_empty());
            for c in roots {
                assert!((density(c, a, b) - density(c, m, s)).abs() < 1e-12, "{} {} {} {}: {}", a, b, m, s, c);
            }
        }
        assert!(crossings(0.0, 1.0, 0.0, 1.0).is_empty());
    }

    #[test]
    fn the_steps_of_tied_values_count_every_observation() {
        let s = Steps::of(&[2.0, 1.0, 2.0, 3.0]);
        assert_eq!(s.values, vec![1.0, 2.0, 3.0]);
        assert_eq!(s.at_or_below, vec![1, 3, 4]);
        assert_eq!(s.index, vec![1, 0, 1, 2]);
    }
}
