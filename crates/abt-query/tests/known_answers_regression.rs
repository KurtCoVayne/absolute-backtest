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

/// The number of correct significant digits of `got` (McCullough's log
/// relative error), capped at 15, the digits NIST certifies.
fn digits(got: f64, want: f64) -> f64 {
    let err = if want == 0.0 { got.abs() } else { ((got - want) / want).abs() };
    if err == 0.0 {
        15.0
    } else {
        (-err.log10()).min(15.0)
    }
}

/// A NIST StRD linear least-squares file: its level of difficulty, the
/// certified values of its header and its data. NIST's files carry the
/// certified values with the data, so they are read from the very file
/// whose SHA-256 the fixtures README records, not transcribed.
struct Strd {
    /// `Lower`, `Average` or `Higher`.
    difficulty: String,
    /// Each parameter's estimate and standard deviation, B0 (or B1 without
    /// an intercept) first.
    params: Vec<(f64, f64)>,
    residual_sd: f64,
    r2: f64,
    /// The analysis of variance: degrees of freedom, sum of squares and
    /// mean square of the regression and of the residual, and the F
    /// statistic.
    regression: (f64, f64, f64),
    residual: (f64, f64, f64),
    f: f64,
    /// The data, one row per observation: y, then x.
    rows: Vec<Vec<f64>>,
}

impl Strd {
    fn read(name: &str) -> Strd {
        let text = fixture(&format!("nist/{}.dat", name));
        let lines: Vec<&str> = text.lines().collect();
        // The header names its data block with `Data:` too; the data follow
        // the last such line.
        let start = lines.iter().rposition(|l| l.trim_start().starts_with("Data:")).expect("a data block");
        let numbers = |fields: &[&str]| fields.iter().map(|f| f.parse::<f64>().ok()).collect::<Option<Vec<f64>>>();
        let mut s = Strd {
            difficulty: String::new(),
            params: Vec::new(),
            residual_sd: f64::NAN,
            r2: f64::NAN,
            regression: (f64::NAN, f64::NAN, f64::NAN),
            residual: (f64::NAN, f64::NAN, f64::NAN),
            f: f64::NAN,
            rows: lines[start + 1..]
                .iter()
                .filter_map(|l| numbers(&l.split_whitespace().collect::<Vec<_>>()))
                .filter(|r| !r.is_empty())
                .collect(),
        };
        let mut observations = None;
        for line in &lines[..start] {
            let fields: Vec<&str> = line.split_whitespace().collect();
            match fields.as_slice() {
                [count, "Observations"] => observations = count.parse::<usize>().ok(),
                [level, "Level", "of", "Difficulty"] => s.difficulty = level.to_string(),
                [b, est, sd] if b.starts_with('B') && b[1..].parse::<usize>().is_ok() => {
                    let v = numbers(&[est, sd]).expect("a certified parameter");
                    s.params.push((v[0], v[1]));
                }
                ["Standard", "Deviation", v] => s.residual_sd = v.parse().expect("the residual standard deviation"),
                ["R-Squared", v] => s.r2 = v.parse().expect("R²"),
                ["Regression", rest @ ..] if rest.len() == 4 => {
                    let v = numbers(rest).expect("the regression's line");
                    s.regression = (v[0], v[1], v[2]);
                    s.f = v[3];
                }
                ["Residual", rest @ ..] if rest.len() == 3 => {
                    let v = numbers(rest).expect("the residual's line");
                    s.residual = (v[0], v[1], v[2]);
                }
                _ => {}
            }
        }
        assert!(
            !s.params.is_empty() && s.residual_sd.is_finite() && s.r2.is_finite() && s.f.is_finite() && s.residual.1.is_finite(),
            "{}: the certified values",
            name
        );
        assert_eq!(observations, Some(s.rows.len()), "{}: the observations the header states", name);
        s
    }

    /// The relative tolerance of E-24 for the file's difficulty.
    fn tolerance(&self) -> f64 {
        match self.difficulty.as_str() {
            "Lower" => 1e-12,
            "Average" => 1e-9,
            "Higher" => 1e-6,
            d => panic!("a difficulty `{}`", d),
        }
    }
}

/// Fits a NIST polynomial model y = Σ B_k x^k (k from 0, or from 1 without
/// an intercept, to `degree`) and checks every certified value at the
/// tolerance of the file's difficulty, which must be `difficulty`. Returns
/// the fewest correct digits among the values, which the caller bounds
/// from below where the engine reaches more than the class asks.
fn certified(name: &str, difficulty: &str, degree: i32, intercept: bool) -> f64 {
    let s = Strd::read(name);
    assert_eq!(s.difficulty, difficulty, "{}: NIST's difficulty class", name);
    let y: Vec<f64> = s.rows.iter().map(|r| r[0]).collect();
    let x: Vec<f64> = s.rows.iter().map(|r| r[1]).collect();
    let first = if intercept { 0 } else { 1 };
    let columns: Vec<Vec<f64>> = (first..=degree).map(|k| x.iter().map(|v| v.powi(k)).collect()).collect();
    assert_eq!(columns.len(), s.params.len(), "{}: the parameters", name);
    let fit = ols(&columns, &y, &Covariance::Iid).unwrap();
    // The analysis of variance: about the mean with an intercept, about
    // zero without one (NIST's convention for NoInt1 and NoInt2).
    let centre = if intercept { y.iter().sum::<f64>() / y.len() as f64 } else { 0.0 };
    let tss: f64 = y.iter().map(|v| (v - centre) * (v - centre)).sum();
    let df_reg = (fit.p - usize::from(intercept)) as f64;
    let ss_reg = tss - fit.rss;
    let ms_res = fit.rss / fit.df;
    let mut got: Vec<(String, f64, f64)> = Vec::new();
    for (j, (b, sd)) in s.params.iter().enumerate() {
        let k = j + first as usize;
        got.push((format!("B{}", k), fit.beta[j], *b));
        got.push((format!("sd(B{})", k), fit.se[j], *sd));
    }
    got.push(("residual sd".into(), fit.sigma, s.residual_sd));
    got.push(("R²".into(), fit.r2, s.r2));
    got.push(("regression df".into(), df_reg, s.regression.0));
    got.push(("regression SS".into(), ss_reg, s.regression.1));
    got.push(("regression MS".into(), ss_reg / df_reg, s.regression.2));
    got.push(("residual df".into(), fit.df, s.residual.0));
    got.push(("residual SS".into(), fit.rss, s.residual.1));
    got.push(("residual MS".into(), ms_res, s.residual.2));
    got.push(("F".into(), ss_reg / df_reg / ms_res, s.f));
    let rel = s.tolerance();
    let fewest = got.iter().map(|(_, g, w)| digits(*g, *w)).fold(f64::INFINITY, f64::min);
    println!("{} ({}, tolerance {:e}): {:.1} digits at the fewest", name, s.difficulty, rel, fewest);
    for (what, g, w) in &got {
        println!("  {:14} {:>24e} certified {:>24e} ({:.1} digits)", what, g, w, digits(*g, *w));
        close(*g, *w, rel, &format!("{} {}", name, what));
    }
    fewest
}

/// Norris (lower difficulty): y = B0 + B1 x on 36 calibration points.
#[test]
fn norris_to_its_certified_values() {
    let fewest = certified("Norris", "Lower", 1, true);
    assert!(fewest >= 13.5, "{}", fewest);
}

/// Pontius (lower difficulty): a quadratic load-cell calibration, 40
/// points, the loads in the millions, so that x² spans 10¹⁰ to 10¹³.
#[test]
fn pontius_to_its_certified_values() {
    let fewest = certified("Pontius", "Lower", 2, true);
    assert!(fewest >= 13.0, "{}", fewest);
}

/// NoInt1 and NoInt2 (average difficulty): a line through the origin,
/// R² and the analysis of variance about zero.
#[test]
fn noint_1_and_2_to_their_certified_values() {
    for name in ["NoInt1", "NoInt2"] {
        let fewest = certified(name, "Average", 1, false);
        assert!(fewest >= 14.0, "{}: {}", name, fewest);
    }
}

/// Filip (higher difficulty): a tenth-degree polynomial in x on 82 points,
/// a design so near singular that many packages refuse it. The engine
/// reaches 7.2 digits on the standard deviations and 7.6 to 7.8 on the
/// estimates. What bounds it is the data in double precision, not the
/// solver: moving every x by one unit in its last place moves the solution
/// in its eighth digit (7.3 to 8.0 digits of agreement over five draws of
/// the signs), and the refinement that makes Wampler5 exact gains Filip
/// under half a digit.
#[test]
fn filip_to_its_certified_values() {
    let fewest = certified("Filip", "Higher", 10, true);
    assert!(fewest >= 7.0, "{}", fewest);
}

/// Wampler3, Wampler4 and Wampler5 (higher difficulty): the quintic of
/// Wampler1 with errors of growing size added, so that the residuals
/// dwarf the fit (Wampler5's R² is 0.002); the certified coefficients are
/// exactly 1. The data are integers, exact in double precision, and the
/// refined solution recovers the coefficients to the last digit.
#[test]
fn wampler_3_4_and_5_to_their_certified_values() {
    for name in ["Wampler3", "Wampler4", "Wampler5"] {
        let fewest = certified(name, "Higher", 5, true);
        assert!(fewest >= 12.0, "{}: {}", name, fewest);
    }
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
