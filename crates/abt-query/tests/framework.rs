//! The test framework (Q-4.1, Q-4.5, Q-4.6): the registry holds the
//! starting set and takes a developer's test under a new name only; a test
//! is reproducible to the bit; the shared engines behave as their theory
//! says (the stationary bootstrap's draws are a function of the seed and
//! the replication, its block grows with persistence, the long-run variance
//! of independent draws is their variance).

use std::sync::Arc;

use abt_query::tests::resample::{politis_white_block, stationary_bootstrap};
use abt_query::tests::rng::Rng;
use abt_query::tests::{get, hac, names, register, Dependence, InputSpec, ParamSpec, Params, Table, Test, TestError, TestResult};

/// A test a developer adds: the mean of a series against zero, nothing
/// more.
struct MeanAboveZero;

impl Test for MeanAboveZero {
    fn name(&self) -> &str {
        "mean_above_zero"
    }
    fn version(&self) -> &str {
        "1"
    }
    fn inputs(&self) -> &[InputSpec] {
        &[]
    }
    fn params(&self) -> &[ParamSpec] {
        &[]
    }
    fn null(&self) -> &str {
        "the mean is at most zero"
    }
    fn dependence(&self) -> Dependence {
        Dependence::Iid
    }
    fn run(&self, tables: &[Table], _: &Params, _: u64) -> Result<TestResult, TestError> {
        let x = tables.first().ok_or_else(|| TestError::Input("one series".into()))?.first()?;
        Ok(TestResult {
            statistic: x.iter().sum::<f64>() / x.len() as f64,
            n: x.len(),
            ..Default::default()
        })
    }
}

#[test]
fn the_registry_holds_the_starting_set_and_takes_a_new_test_once() {
    assert!(names().contains(&"regression".to_string()));
    assert!(get("nowhere").is_none());
    register(Arc::new(MeanAboveZero)).unwrap();
    assert_eq!(get("mean_above_zero").map(|t| t.version().to_string()), Some("1".into()));
    assert!(register(Arc::new(MeanAboveZero)).is_err_and(|e| e.contains("registered")));
}

/// A noisy line, its noise drawn from the framework's generator.
fn line(n: usize, slope: f64, seed: u64) -> Table {
    let mut rng = Rng::stream(seed, 0);
    let x: Vec<f64> = (0..n).map(|i| i as f64 / n as f64).collect();
    let y: Vec<f64> = x.iter().map(|v| 2.0 + slope * v + rng.normal()).collect();
    let mut t = Table::series("y", y);
    t.columns.insert("x".into(), x);
    t
}

#[test]
fn regression_through_the_trait_is_reproducible_and_scale_free() {
    let test = get("regression").unwrap();
    let t = line(400, 3.0, 7);
    let a = test.run(std::slice::from_ref(&t), &Params::new(), 1).unwrap();
    let b = test.run(std::slice::from_ref(&t), &Params::new(), 1).unwrap();
    assert_eq!(a.statistic.to_bits(), b.statistic.to_bits(), "bit identity");
    assert!(a.p_value.is_some_and(|p| p < 1e-6), "{:?}", a);
    // The Wald statistic of the slopes does not depend on the units.
    let mut scaled = t.clone();
    scaled.columns.get_mut("y").unwrap().iter_mut().for_each(|v| *v *= 1000.0);
    scaled.columns.get_mut("x").unwrap().iter_mut().for_each(|v| *v *= 0.001);
    let c = test.run(&[scaled], &Params::new(), 1).unwrap();
    assert!(((c.statistic - a.statistic) / a.statistic).abs() < 1e-9, "{} against {}", c.statistic, a.statistic);
    // Without a slope the null is not rejected at 1 %.
    let flat = test.run(&[line(400, 0.0, 7)], &Params::new(), 1).unwrap();
    assert!(flat.p_value.is_some_and(|p| p > 0.01), "{:?}", flat);
    assert!(test.run(&[Table::series("y", vec![1.0; 10])], &Params::new(), 1).is_err());
}

#[test]
fn the_stationary_bootstrap_is_a_function_of_the_seed() {
    let mut rng = Rng::stream(3, 0);
    let x: Vec<f64> = (0..500).map(|_| rng.normal()).collect();
    let mean = |s: &[f64]| s.iter().sum::<f64>() / s.len() as f64;
    let a = stationary_bootstrap(&x, 5.0, 200, 11, &mean);
    let b = stationary_bootstrap(&x, 5.0, 200, 11, &mean);
    assert!(a.iter().zip(&b).all(|(p, q)| p.to_bits() == q.to_bits()));
    assert_ne!(a, stationary_bootstrap(&x, 5.0, 200, 12, &mean));
    // The resampled means centre on the mean, with the spread of a mean.
    let m = mean(&a);
    assert!((m - mean(&x)).abs() < 0.05, "{}", m);
    // A persistent series asks for longer blocks than independent draws.
    let mut ar = vec![0.0; 500];
    let mut r2 = Rng::stream(4, 0);
    for t in 1..500 {
        ar[t] = 0.9 * ar[t - 1] + r2.normal();
    }
    assert!(
        politis_white_block(&ar) > 2.0 * politis_white_block(&x),
        "{} against {}",
        politis_white_block(&ar),
        politis_white_block(&x)
    );
}

#[test]
fn the_long_run_variance_of_independent_draws_is_their_variance() {
    let mut rng = Rng::stream(5, 0);
    let x: Vec<f64> = (0..20_000).map(|_| 2.0 * rng.normal()).collect();
    let lrv = hac::long_run_variance(&x, hac::newey_west_lags(x.len()));
    assert!((lrv - 4.0).abs() < 0.3, "{}", lrv);
    assert_eq!(hac::newey_west_lags(100), 4);
    assert!(hac::newey_west_t(&x, None).abs() < 4.0);
}
