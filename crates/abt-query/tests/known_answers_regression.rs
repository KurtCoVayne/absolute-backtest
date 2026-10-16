//! G9 for the regression engine (Q-4.4's `regression`, Q-4.6): the NIST
//! StRD linear least-squares datasets to their certified values, at the
//! tolerance of NIST's difficulty class (E-24: lower 1e-12, average 1e-9,
//! higher 1e-6, relative).

use std::path::PathBuf;

use abt_query::tests::regression::{ols, Covariance};

fn fixture(name: &str) -> String {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests").join("fixtures").join(name);
    std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("{}: {}", p.display(), e))
}

fn close(got: f64, want: f64, rel: f64, what: &str) {
    let err = if want == 0.0 { got.abs() } else { ((got - want) / want).abs() };
    assert!(err <= rel, "{}: {} against the certified {} (relative error {:e} > {:e})", what, got, want, err, rel);
}

/// The data lines of a NIST StRD `.dat` file: every line after `Data:`
/// whose fields are all numbers.
fn nist_rows(text: &str) -> Vec<Vec<f64>> {
    let mut rows = Vec::new();
    let mut data = false;
    for line in text.lines() {
        if line.trim_start().starts_with("Data:") {
            data = true;
            continue;
        }
        if !data {
            continue;
        }
        let fields: Option<Vec<f64>> = line.split_whitespace().map(|f| f.parse().ok()).collect();
        if let Some(f) = fields.filter(|f| !f.is_empty()) {
            rows.push(f);
        }
    }
    rows
}

/// Norris (lower difficulty): y = B0 + B1 x on 36 calibration points.
#[test]
fn norris_to_its_certified_values() {
    let rows = nist_rows(&fixture("nist/Norris.dat"));
    assert_eq!(rows.len(), 36);
    let y: Vec<f64> = rows.iter().map(|r| r[0]).collect();
    let x: Vec<f64> = rows.iter().map(|r| r[1]).collect();
    let fit = ols(&[vec![1.0; 36], x], &y, &Covariance::Iid).unwrap();
    let rel = 1e-12;
    close(fit.beta[0], -0.262323073774029, rel, "B0");
    close(fit.beta[1], 1.00211681802045, rel, "B1");
    close(fit.se[0], 0.232818234301152, 1e-9, "sd(B0)");
    close(fit.se[1], 0.429796848199937e-03, 1e-9, "sd(B1)");
    close(fit.sigma, 0.884796396144373, 1e-9, "residual sd");
    close(fit.r2, 0.999993745883712, rel, "R²");
    close(fit.rss, 26.6173985294224, 1e-9, "residual SS");
}

/// Longley (higher difficulty): employment on six collinear economic
/// series, 16 years. The data are statsmodels' copy of NIST's, and the
/// certified values NIST's as statsmodels transcribes them
/// (`statsmodels/regression/tests/results/results_regression.py`).
#[test]
fn longley_to_its_certified_values() {
    let text = fixture("statsmodels/longley.csv");
    let rows: Vec<Vec<f64>> = text.lines().skip(1).map(|l| l.split(',').skip(1).map(|f| f.trim_matches('"').parse().unwrap()).collect()).collect();
    assert_eq!(rows.len(), 16);
    let y: Vec<f64> = rows.iter().map(|r| r[0]).collect();
    let mut columns = vec![vec![1.0; 16]];
    for j in 1..7 {
        columns.push(rows.iter().map(|r| r[j]).collect());
    }
    let fit = ols(&columns, &y, &Covariance::Iid).unwrap();
    let want = [
        -3482258.63459582,
        15.0618722713733,
        -0.358191792925910e-01,
        -2.02022980381683,
        -1.03322686717359,
        -0.511041056535807e-01,
        1829.15146461355,
    ];
    let want_se = [
        890420.383607373,
        84.9149257747669,
        0.334910077722432e-01,
        0.488399681651699,
        0.214274163161675,
        0.226073200069370,
        455.478499142212,
    ];
    for j in 0..7 {
        close(fit.beta[j], want[j], 1e-6, &format!("B{}", j));
        close(fit.se[j], want_se[j], 1e-6, &format!("sd(B{})", j));
    }
    close(fit.sigma * fit.sigma, 92936.0061673238, 1e-6, "residual variance");
    close(fit.r2, 0.995479004577296, 1e-6, "R²");
    close(fit.rss, 836424.055505915, 1e-6, "residual SS");
}

/// Wampler1 and Wampler2 (higher difficulty): exact quintics in x = 0..20,
/// their certified coefficients the polynomial's and their residuals zero.
#[test]
fn wampler_1_and_2_are_their_polynomials() {
    let x: Vec<f64> = (0..=20).map(f64::from).collect();
    let powers = |x: &[f64]| (0..6).map(|k| x.iter().map(|v| v.powi(k)).collect::<Vec<f64>>()).collect::<Vec<_>>();
    for (name, coef) in [("Wampler1", [1.0, 1.0, 1.0, 1.0, 1.0, 1.0]), ("Wampler2", [1.0, 0.1, 0.01, 0.001, 0.0001, 0.00001])] {
        let y: Vec<f64> = x.iter().map(|v| (0..6).map(|k| coef[k] * v.powi(k as i32)).sum()).collect();
        let fit = ols(&powers(&x), &y, &Covariance::Iid).unwrap();
        for (k, (b, c)) in fit.beta.iter().zip(coef).enumerate() {
            close(*b, c, 1e-6, &format!("{} B{}", name, k));
        }
        assert!(fit.sigma < 1e-6 * y.iter().cloned().fold(0.0, f64::max), "{}: residual sd {}", name, fit.sigma);
    }
}
