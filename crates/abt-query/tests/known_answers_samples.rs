//! G9 for the distribution, resampling and Sharpe-ratio tests of the
//! starting set (Q-4.4, Q-4.6), each run through the registry, checked on
//! its known-answer source at the tolerance ERRATA E-24 states: NIST's
//! certified values to NIST's difficulty class (lower 1e-12, average 1e-9,
//! higher 1e-6, relative); published critical values to 1e-3; a paper's or a
//! textbook's example to its printed precision; a hand computation to the
//! rounding of its arithmetic; and a simulated answer, a planted parameter,
//! to a tolerance its test states and justifies. Then the properties Q-4.6
//! asks for: the same seed gives the same bits, a scale-free statistic does
//! not see the units, and the `iid` test does not see the order.

use std::path::PathBuf;

use abt_query::metrics;
use abt_query::tests::regression::{ols, Covariance};
use abt_query::tests::resample::autocorrelation;
use abt_query::tests::rng::Rng;
use abt_query::tests::set::ks_normal::{lilliefors, lilliefors_null};
use abt_query::tests::set::samples::Moments;
use abt_query::tests::set::wfe::sign_flip_p;
use abt_query::tests::{get, names, Param, Params, Table, TestError, TestResult};

/// A fixture file's text.
fn fixture(name: &str) -> String {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests").join("fixtures").join(name);
    std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("{}: {}", p.display(), e))
}

/// |got − want| ≤ tol.
fn within(got: f64, want: f64, tol: f64, what: &str) {
    assert!((got - want).abs() <= tol, "{}: {} against {} (difference {:e} > {:e})", what, got, want, (got - want).abs(), tol);
}

/// |got − want| ≤ rel · |want|.
fn close(got: f64, want: f64, rel: f64, what: &str) {
    let err = if want == 0.0 { got.abs() } else { ((got - want) / want).abs() };
    assert!(err <= rel, "{}: {} against {} (relative error {:e} > {:e})", what, got, want, err, rel);
}

/// Parameters by name.
fn params(p: &[(&str, Param)]) -> Params {
    p.iter().map(|(k, v)| (k.to_string(), v.clone())).collect()
}

/// A registered test's result, or its error.
fn attempt(test: &str, tables: &[Table], p: &[(&str, Param)], seed: u64) -> Result<TestResult, TestError> {
    get(test).unwrap_or_else(|| panic!("`{}` is not registered", test)).run(tables, &params(p), seed)
}

/// A registered test's result.
fn run(test: &str, tables: &[Table], p: &[(&str, Param)], seed: u64) -> TestResult {
    attempt(test, tables, p, seed).unwrap_or_else(|e| panic!("{}: {}", test, e))
}

/// A table of one series.
fn series(x: &[f64]) -> Table {
    Table::series("x", x.to_vec())
}

fn int(v: i64) -> Param {
    Param::Int(v)
}

fn float(v: f64) -> Param {
    Param::Float(v)
}

/// `n` independent standard normal draws, from stream (seed, rep).
fn normals(n: usize, seed: u64, rep: u64) -> Vec<f64> {
    let mut rng = Rng::stream(seed, rep);
    (0..n).map(|_| rng.normal()).collect()
}

/// A Gaussian AR(1) with coefficient `phi` and unit marginal variance,
/// started from its stationary law, from stream (seed, rep).
fn ar1(n: usize, phi: f64, seed: u64, rep: u64) -> Vec<f64> {
    let mut rng = Rng::stream(seed, rep);
    let innovation = (1.0 - phi * phi).sqrt();
    let mut v = rng.normal();
    (0..n)
        .map(|_| {
            let out = v;
            v = phi * v + innovation * rng.normal();
            out
        })
        .collect()
}

/// The half-width of a two-sided binomial band of `z` standard errors
/// around a rate `p` estimated from `m` independent trials.
fn binomial_band(p: f64, m: usize, z: f64) -> f64 {
    z * (p * (1.0 - p) / m as f64).sqrt()
}

/// The mean of paired differences and its standard error.
fn paired(d: &[f64]) -> (f64, f64) {
    let m = Moments::of(d);
    (m.mean, m.sd() / (d.len() as f64).sqrt())
}

#[test]
fn the_registry_holds_the_group() {
    let registered = names();
    for t in ["bootstrap_sharpe", "dsr", "hill_tail", "jarque_bera", "ks_normal", "min_trl", "nw_mean", "pbo", "psr", "wfe"] {
        assert!(registered.contains(&t.to_string()), "`{}` is not registered", t);
        let test = get(t).unwrap();
        assert_eq!((test.name(), test.version()), (t, "1"));
        assert!(!test.null().is_empty());
    }
}

// ---- NIST StRD univariate summary statistics ----

const LOWER: f64 = 1e-12;
const AVERAGE: f64 = 1e-9;
const HIGHER: f64 = 1e-6;

/// NIST's certified mean, standard deviation (n − 1) and lag-1
/// autocorrelation of its univariate datasets, with the tolerance of each
/// one's difficulty class (Michelso's from NIST's listing: the copy here
/// omits its model lines).
const NIST: [(&str, usize, f64, f64, f64, f64); 9] = [
    ("PiDigits", 5000, 4.53480000000000, 2.86733906028871, -0.00355099287237972, LOWER),
    ("Lottery", 218, 518.958715596330, 291.699727470969, -0.120948622967393, LOWER),
    ("Lew", 200, -177.435000000000, 277.332168044316, -0.307304800605679, LOWER),
    ("Mavro", 50, 2.00185600000000, 0.000429123454003053, 0.937989183438248, LOWER),
    ("Michelso", 100, 299.852400000000, 0.0790105478190518, 0.535199668621283, LOWER),
    ("NumAcc1", 3, 10000002.0, 1.0, -0.5, LOWER),
    ("NumAcc2", 1001, 1.2, 0.1, -0.999, AVERAGE),
    ("NumAcc3", 1001, 1000000.2, 0.1, -0.999, AVERAGE),
    ("NumAcc4", 1001, 10000000.2, 0.1, -0.999, HIGHER),
];

/// The observations of a NIST StRD univariate file: the numbers after its
/// data marker, NIST's `Data: Y` line or the `# Data` line of the files
/// Commons Math reformats with `#` comments.
fn nist_data(text: &str) -> Vec<f64> {
    let mut lines = text.lines();
    lines.by_ref().find(|l| l.trim() == "# Data" || l.trim().starts_with("Data: Y")).expect("a data marker");
    lines.filter_map(|l| l.trim().parse::<f64>().ok()).collect()
}

/// The moments jarque_bera's statistic and every Sharpe-ratio test are built
/// on, and the autocorrelation the bootstrap's block rule reads, against
/// NIST's certified values.
#[test]
fn the_moments_are_nist_certified_to_the_difficulty_tolerance() {
    for (name, n, mean, sd, r1, tol) in NIST {
        let x = nist_data(&fixture(&format!("nist/{}.txt", name)));
        assert_eq!(x.len(), n, "{}", name);
        let m = Moments::of(&x);
        close(m.mean, mean, tol, &format!("{} mean", name));
        close(m.sd(), sd, tol, &format!("{} standard deviation", name));
        close(autocorrelation(&x, 1), r1, tol, &format!("{} lag-1 autocorrelation", name));
    }
}

// ---- jarque_bera ----

/// The textbook series: the residuals of the food expenditure regression of
/// Hill, Griffiths and Lim, Principles of Econometrics (5th ed., Example
/// 4.6; 40 households' weekly food expenditure on income), for which the
/// book prints b₁ = 83.42, b₂ = 10.21, skewness −0.097, kurtosis 2.989 and
/// JB = 40/6 (0.097² + (2.989 − 3)²/4) = 0.063. To the printed precision:
/// half a unit of the last printed digit. (The book is not reachable from
/// the build's network; gretl's JB on the same data, 0.0633, as a public
/// repository records it, agrees.) The residuals are as normal as the book
/// finds them, so the bootstrap does not reject either.
#[test]
fn jarque_bera_reproduces_the_food_expenditure_example() {
    let rows: Vec<(f64, f64)> = fixture("poe/food.csv")
        .lines()
        .skip(1)
        .map(|l| {
            let f: Vec<f64> = l.split(',').map(|v| v.trim().parse().unwrap()).collect();
            (f[0], f[1])
        })
        .collect();
    assert_eq!(rows.len(), 40);
    let y: Vec<f64> = rows.iter().map(|r| r.0).collect();
    let income: Vec<f64> = rows.iter().map(|r| r.1).collect();
    let fit = ols(&[vec![1.0; 40], income], &y, &Covariance::Iid).unwrap();
    within(fit.beta[0], 83.42, 0.005, "b1");
    within(fit.beta[1], 10.21, 0.005, "b2");
    let m = Moments::of(&fit.residuals);
    within(m.skewness(), -0.097, 0.0005, "skewness");
    within(m.excess_kurtosis() + 3.0, 2.989, 0.0005, "kurtosis");
    let r = run("jarque_bera", &[series(&fit.residuals)], &[("reps", int(999))], 7);
    within(r.statistic, 0.063, 0.0005, "JB");
    assert_eq!((r.n, r.df), (40, Some(2.0)));
    assert!(r.p_value.is_some_and(|p| p > 0.5), "{:?}", r);
    assert!(r.notes.iter().any(|n| n.starts_with("asymptotic chi2(2) p-value 0.968")), "{:?}", r.notes);
}

/// A hand computation: x = 1, …, 10 has mean 5.5, m₂ = 82.5/10 = 33/4,
/// m₃ = 0 by symmetry and m₄ = 2 (0.5⁴ + 1.5⁴ + 2.5⁴ + 3.5⁴ + 4.5⁴)/10 =
/// 9669/80, so K = m₄/m₂² − 3 = 293/165 − 3 = −202/165 and
/// JB = 10/6 · (202/165)²/4 = 10201/16335 exactly; to the rounding of the
/// floating-point arithmetic.
#[test]
fn jarque_bera_on_a_hand_computed_series() {
    let x: Vec<f64> = (1..=10).map(f64::from).collect();
    let r = run("jarque_bera", &[series(&x)], &[("reps", int(99))], 1);
    close(r.statistic, 10201.0 / 16335.0, 1e-14, "JB of 1..10");
}

/// The dependence treatment, on simulated series with planted laws.
/// Independent normal series (200 of 1,000): the bootstrap rejects at 5 %
/// about 5 % of the time, to 3.5 binomial standard errors (shorter series
/// make it somewhat liberal, as it estimates the spread of K from the
/// sample's eighth moment). A Gaussian AR(1) with φ = 0.8 is normal too,
/// but its autocorrelation inflates the variance of S and K (by Σ ρₖ³ and
/// Σ ρₖ⁴, Lomnicki 1961): over 1,000 such series the χ²(2) test rejects
/// beyond 3.5 binomial standard errors of 5 %, and on 200 of them the
/// bootstrap rejects less often than χ²(2), by more than three standard
/// errors of the paired difference. Student's t with three degrees of
/// freedom (a normal over the root of an independent χ²(3)/3) is rejected.
#[test]
fn jarque_bera_holds_its_level_under_dependence() {
    let (m, n) = (200, 1_000);
    let jb = |x: &[f64]| {
        let s = Moments::of(x);
        n as f64 / 6.0 * (s.skewness().powi(2) + s.excess_kurtosis().powi(2) / 4.0)
    };
    let chi2_rejects = |x: &[f64]| jb(x) > 5.991_464_547_107_979;
    let mut boot = 0;
    for i in 0..m as u64 {
        let r = run("jarque_bera", &[series(&normals(n, 11, i))], &[("reps", int(199))], i);
        boot += usize::from(r.p_value.unwrap() < 0.05);
    }
    within(boot as f64 / m as f64, 0.05, binomial_band(0.05, m, 3.5), "the bootstrap's rejection rate on independent normal series");
    let asym = (0..1_000u64).filter(|i| chi2_rejects(&ar1(n, 0.8, 12, *i))).count() as f64 / 1_000.0;
    assert!(asym > 0.05 + binomial_band(0.05, 1_000, 3.5), "the chi2 test's rejection rate on AR(1) series is {}", asym);
    let d: Vec<f64> = (0..m as u64)
        .map(|i| {
            let x = ar1(n, 0.8, 12, i);
            let r = run("jarque_bera", &[series(&x)], &[("reps", int(199))], i);
            f64::from(u8::from(chi2_rejects(&x))) - f64::from(u8::from(r.p_value.unwrap() < 0.05))
        })
        .collect();
    let (gain, se) = paired(&d);
    assert!(gain > 3.0 * se, "on AR(1) series the bootstrap rejects {} less often than chi2 (standard error {})", gain, se);
    let t3 = normals(500, 13, 0)
        .iter()
        .zip(normals(1500, 13, 1).chunks(3))
        .map(|(z, c)| z / (c.iter().map(|v| v * v).sum::<f64>() / 3.0).sqrt())
        .collect::<Vec<f64>>();
    let r = run("jarque_bera", &[series(&t3)], &[("reps", int(499))], 3);
    assert!(r.p_value.unwrap() < 0.01, "{:?}", r);
}

// ---- ks_normal ----

/// A hand computation with ties: x = (−2, −1, −1, 0, 0, 1, 1, 2) has mean 0
/// and s² = 12/7. The largest gap is at x = −1, where the empirical
/// distribution reaches 3/8 against Φ(−1/s) = Φ(−√(7/12)) =
/// 0.222504359373368, and symmetrically at x = 1, where it leaves 5/8 below
/// 1 − 0.222504…: D = 3/8 − 0.222504359373368 = 0.152495640626632 (Φ to 16
/// digits by mpmath); to 1e-12.
#[test]
fn ks_normal_on_a_hand_computed_series() {
    let x = [-2.0, -1.0, -1.0, 0.0, 0.0, 1.0, 1.0, 2.0];
    let r = run("ks_normal", &[series(&x)], &[("reps", int(99))], 1);
    within(r.statistic, 0.152_495_640_626_632, 1e-12, "D");
}

/// A value from software, the last resort: R's `nortest::lillie.test` on
/// forty numbers, as statsmodels 0.8.0 records it beside the data in
/// `statsmodels/stats/_lilliefors.py` (D = 0.15096827429598147). The hand
/// example above covers ties; this one a sample of usual size. To 1e-12.
#[test]
fn ks_normal_agrees_with_r_on_forty_numbers() {
    let x = [
        1.138, -0.325, -1.461, -0.441, -0.005, -0.957, -1.52, 0.481, 0.713, 0.175, -1.764, -0.209, -0.681, 0.671, 0.204, 0.403, -0.165, 1.765, 0.127, -1.261, -0.101, 0.527, 1.114, -0.57, -1.172,
        0.697, 0.146, 0.704, 0.422, 0.63, 0.661, 0.025, 0.177, 0.578, 0.945, 0.211, 0.153, 0.279, 0.35, 0.396,
    ];
    let r = run("ks_normal", &[series(&x)], &[("reps", int(99))], 1);
    within(r.statistic, 0.150_968_274_295_981_47, 1e-12, "D");
}

/// Upper-tail levels of Dallal and Wilkinson's table.
const DW_LEVELS: [f64; 5] = [0.20, 0.15, 0.10, 0.05, 0.01];

/// Dallal and Wilkinson's (1986) table of corrected critical values of
/// Lilliefors' statistic (Lilliefors' own 1967 table is off in the third
/// decimal), at the levels of `DW_LEVELS`, for the sample sizes checked
/// here, with the simulated replications each gets, as statsmodels 0.8.0
/// transcribes the table (`crit_lf` in
/// https://raw.githubusercontent.com/statsmodels/statsmodels/v0.8.0/statsmodels/stats/_lilliefors.py).
/// The table's 0.001 column is left out: a simulation of 10⁶ samples
/// (outside this suite) puts it up to 0.002 from the printed values, beyond
/// their precision, the extreme tail being where the published simulation
/// itself is least accurate.
const DALLAL_WILKINSON: [(usize, usize, [f64; 5]); 9] = [
    (4, 1_000_000, [0.303, 0.321, 0.346, 0.376, 0.413]),
    (5, 800_000, [0.289, 0.303, 0.319, 0.343, 0.397]),
    (8, 500_000, [0.239, 0.250, 0.265, 0.288, 0.333]),
    (10, 400_000, [0.217, 0.228, 0.241, 0.262, 0.304]),
    (15, 270_000, [0.181, 0.190, 0.201, 0.219, 0.254]),
    (20, 200_000, [0.159, 0.166, 0.176, 0.192, 0.223]),
    (30, 135_000, [0.131, 0.138, 0.146, 0.159, 0.185]),
    (40, 100_000, [0.115, 0.120, 0.128, 0.139, 0.162]),
    (100, 40_000, [0.074, 0.077, 0.082, 0.089, 0.104]),
];

/// Lilliefors' correction, simulated as ks_normal simulates it for its
/// notes, against the published critical values to their printed
/// precision (1e-3). The simulation has its own error, so the published
/// value must lie within 1e-3 of the simulation's distribution-free
/// interval for the quantile, the order statistics R(1 − α) ± 3√(Rα(1 − α))
/// (three standard errors of the empirical quantile's rank).
fn lilliefors_matches_dallal_wilkinson(rows: &[(usize, usize, [f64; 5])]) {
    for (n, reps, crit) in rows {
        let null = lilliefors_null(*n, *reps, 2026 + *n as u64);
        for (alpha, c) in DW_LEVELS.iter().zip(crit) {
            let r = *reps as f64;
            let centre = r * (1.0 - alpha);
            let spread = 3.0 * (r * alpha * (1.0 - alpha)).sqrt();
            let lo = null[((centre - spread).floor() as usize).max(1) - 1];
            let hi = null[((centre + spread).ceil() as usize).min(*reps) - 1];
            assert!(
                lo - 1e-3 <= *c && *c <= hi + 1e-3,
                "n {} at {}: the published {} against the simulation's [{}, {}]",
                n,
                alpha,
                c,
                lo,
                hi
            );
        }
    }
}

#[test]
fn lilliefors_correction_reproduces_dallal_wilkinson_small_samples() {
    lilliefors_matches_dallal_wilkinson(&DALLAL_WILKINSON[..5]);
}

#[test]
fn lilliefors_correction_reproduces_dallal_wilkinson_larger_samples() {
    lilliefors_matches_dallal_wilkinson(&DALLAL_WILKINSON[5..]);
}

/// The dependence treatment, on simulated series with planted laws, of
/// 400 observations. Independent normal series (150): the bootstrap
/// rejects at 10 % about 10 % of the time, to 3.5 binomial standard errors.
/// A Gaussian AR(1) with φ = 0.8 is normal too, but its empirical process
/// has a larger variance: over 1,000 such series Lilliefors' correction for
/// independent observations rejects beyond 3.5 binomial standard errors of
/// 10 %, and on 150 of them the bootstrap rejects less often, by more than
/// three standard errors of the paired difference (it is still liberal at
/// this length and dependence, its blocks too short to carry all of it). An
/// exponential series is rejected.
#[test]
fn ks_normal_holds_its_level_under_dependence() {
    let (m, n) = (150, 400);
    let null = lilliefors_null(n, 4_000, 5);
    let critical = null[(0.9 * null.len() as f64) as usize];
    let iid_rejects = |x: &[f64]| lilliefors(x).unwrap() > critical;
    let mut boot = 0;
    for i in 0..m as u64 {
        let r = run("ks_normal", &[series(&normals(n, 21, i))], &[("reps", int(199))], i);
        boot += usize::from(r.p_value.unwrap() < 0.10);
    }
    within(boot as f64 / m as f64, 0.10, binomial_band(0.10, m, 3.5), "the bootstrap's rejection rate on independent normal series");
    let iid = (0..1_000u64).filter(|i| iid_rejects(&ar1(n, 0.8, 22, *i))).count() as f64 / 1_000.0;
    assert!(iid > 0.10 + binomial_band(0.10, 1_000, 3.5), "Lilliefors' iid rejection rate on AR(1) series is {}", iid);
    let d: Vec<f64> = (0..m as u64)
        .map(|i| {
            let x = ar1(n, 0.8, 22, i);
            let r = run("ks_normal", &[series(&x)], &[("reps", int(199))], i);
            f64::from(u8::from(iid_rejects(&x))) - f64::from(u8::from(r.p_value.unwrap() < 0.10))
        })
        .collect();
    let (gain, se) = paired(&d);
    assert!(
        gain > 3.0 * se,
        "on AR(1) series the bootstrap rejects {} less often than Lilliefors' iid test (standard error {})",
        gain,
        se
    );
    let mut rng = Rng::stream(23, 0);
    let exponential: Vec<f64> = (0..n).map(|_| -(1.0 - rng.uniform()).ln()).collect();
    let r = run("ks_normal", &[series(&exponential)], &[("reps", int(499))], 4);
    assert!(r.p_value.unwrap() < 0.01, "{:?}", r);
}

// ---- hill_tail ----

/// Hill's estimate of 1/α from the k largest of `desc`, values in
/// decreasing order, computed here apart from the test:
/// (1/k) Σᵢ₌₁ᵏ ln(x₍ᵢ₎ / x₍ₖ₊₁₎).
fn hill(desc: &[f64], k: usize) -> f64 {
    desc[..k].iter().map(|v| (v / desc[k]).ln()).sum::<f64>() / k as f64
}

/// Pareto draws of index α above 1, U^(−1/α), from stream (seed, 0).
fn pareto(n: usize, alpha: f64, seed: u64) -> Vec<f64> {
    let mut rng = Rng::stream(seed, 0);
    (0..n).map(|_| (1.0 - rng.uniform()).powf(-1.0 / alpha)).collect()
}

/// A simulated Pareto with a planted index α = 3, n = 5,000. Above any
/// threshold the tail is exactly Pareto, so at every k the estimate has no
/// bias and the standard deviation α/√k: the tolerance is four of those at
/// the k Hall's bootstrap chose (a failure probability of 6e-5 for a fresh
/// sample, the choice of k aside). The exact interval covers α; the null
/// α ≥ 1.5 stands and α ≥ 6 is rejected.
#[test]
fn hill_tail_recovers_a_planted_pareto_index() {
    let x = pareto(5_000, 3.0, 31);
    let r = run("hill_tail", &[series(&x)], &[("tail", Param::Text("upper".into())), ("alpha0", float(6.0))], 1);
    let k = r.df.unwrap() / 2.0;
    within(r.statistic, 3.0, 4.0 * 3.0 / k.sqrt(), "alpha");
    let (lo, hi, level) = r.interval.unwrap();
    assert!(lo < 3.0 && 3.0 < hi && level == 0.95, "{:?}", r.interval);
    assert!(r.p_value.unwrap() < 1e-6, "{:?}", r);
    let r = run("hill_tail", &[series(&x)], &[("tail", Param::Text("upper".into())), ("alpha0", float(1.5))], 1);
    assert!(r.p_value.unwrap() > 0.5, "{:?}", r);
    // The loss tail of the negated draws is the same tail.
    let losses: Vec<f64> = x.iter().map(|v| -v).collect();
    let lower = run("hill_tail", &[series(&losses)], &[("alpha0", float(1.5))], 1);
    assert_eq!(lower.statistic.to_bits(), r.statistic.to_bits());
}

/// Hall's trade-off, on a simulated tail of his model with a planted index:
/// half Pareto(3), half Pareto(6), so 1 − F(x) = (x⁻³ + x⁻⁶)/2. Its second
/// term biases Hill's estimate of α by about α k/n as k grows, a bias the
/// whole sample (k = n − 1) carries in full; at Hall's k the estimate must
/// be within that bias plus four standard deviations α/√k of α, and closer
/// to it than the whole sample's.
#[test]
fn hill_tail_trades_bias_for_variance_on_halls_model() {
    let n = 5_000;
    let mut rng = Rng::stream(32, 0);
    let x: Vec<f64> = (0..n)
        .map(|_| {
            let alpha = if rng.uniform() < 0.5 { 3.0 } else { 6.0 };
            (1.0 - rng.uniform()).powf(-1.0 / alpha)
        })
        .collect();
    let r = run("hill_tail", &[series(&x)], &[("tail", Param::Text("upper".into()))], 2);
    let k = r.df.unwrap() / 2.0;
    let tolerance = 3.0 * k / n as f64 + 4.0 * 3.0 / k.sqrt();
    within(r.statistic, 3.0, tolerance, "alpha at Hall's k");
    let mut desc = x.clone();
    desc.sort_by(|a, b| b.total_cmp(a));
    let whole = 1.0 / hill(&desc, n - 1);
    assert!((r.statistic - 3.0).abs() < (whole - 3.0).abs(), "Hall's {} against the whole sample's {}", r.statistic, whole);
}

/// The `iid` test does not see the order of the series, nor its units; the
/// same seed gives the same bits; a fixed k skips the bootstrap.
#[test]
fn hill_tail_is_free_of_order_and_scale() {
    let x = pareto(2_000, 2.5, 33);
    let upper = [("tail", Param::Text("upper".into()))];
    let a = run("hill_tail", &[series(&x)], &upper, 9);
    let b = run("hill_tail", &[series(&x)], &upper, 9);
    assert_eq!(a, b);
    let mut shuffled = x.clone();
    Rng::stream(34, 0).shuffle(&mut shuffled);
    assert_eq!(run("hill_tail", &[series(&shuffled)], &upper, 9), a, "permutation");
    let scaled: Vec<f64> = x.iter().map(|v| v * 1000.0).collect();
    let c = run("hill_tail", &[series(&scaled)], &upper, 9);
    assert_eq!(c.df, a.df, "k");
    close(c.statistic, a.statistic, 1e-12, "alpha in other units");
    let fixed = run("hill_tail", &[series(&x)], &[("tail", Param::Text("upper".into())), ("k", int(200))], 9);
    assert_eq!(fixed.df, Some(400.0));
    let mut desc = x.clone();
    desc.sort_by(|p, q| q.total_cmp(p));
    close(fixed.statistic, 1.0 / hill(&desc, 200), 1e-12, "alpha at k = 200");
    assert!(attempt("hill_tail", &[series(&x)], &[("tail", Param::Text("left".into()))], 9).is_err());
    assert!(attempt("hill_tail", &[series(&x[..20])], &upper, 9).is_err());
}

// ---- psr, dsr, min_trl ----

/// A series with population skewness 0 and kurtosis 3 exactly, the normal's,
/// and per-period Sharpe ratio `sharpe`: zeros, ones and minus ones in the
/// proportions 4 : 1 : 1 (kurtosis m₄/m₂² = (2/6)/(2/6)² = 3), shifted by the
/// mean that gives the ratio. `n` is a multiple of 6.
fn normal_moments(n: usize, sharpe: f64) -> Vec<f64> {
    let z: Vec<f64> = (0..n).map(|i| [0.0, 1.0, 0.0, -1.0, 0.0, 0.0][i % 6]).collect();
    let shift = sharpe * Moments::of(&z).sd();
    z.iter().map(|v| v + shift).collect()
}

/// The examples of Bailey and López de Prado (2012, "The Sharpe ratio
/// efficient frontier"): a hedge fund's monthly Sharpe ratio of 0.458 over
/// two years has PSR(0) = 0.982 with normal returns, 0.913 with skewness
/// −2.448 and kurtosis 10.164, and 0.953 over three years; an annualised
/// Sharpe ratio of 2 against a benchmark of 1 needs 2.73 years of daily
/// returns (252 a year), 2.83 of weekly and 3.24 of monthly ones with
/// normal returns, and 4.99 years of monthly ones with the HFR aggregate
/// index's skewness −0.72 and kurtosis 5.78. To the printed precision: one
/// unit of the last printed digit, since the paper's inputs are rounded too
/// (the three-year PSR it prints as 0.953 is 0.95351 from the rounded
/// 0.458). The paper is not reachable from the build's network; these are
/// the printed values as independent reproductions (QuantLite's reference
/// check, the tests of the `deflated-sharpe` package) transcribe them
/// alike. The tests apply these metrics to the moments of their input;
/// where the moments are a finite series' (the normal ones), the examples
/// run through the tests themselves.
#[test]
fn psr_and_min_trl_reproduce_the_efficient_frontier_examples() {
    within(metrics::probabilistic_sharpe(0.458, 0.0, 24, 0.0, 0.0), 0.982, 1e-3, "PSR, two years, normal");
    within(metrics::probabilistic_sharpe(0.458, 0.0, 24, -2.448, 7.164), 0.913, 1e-3, "PSR, two years");
    within(metrics::probabilistic_sharpe(0.458, 0.0, 36, -2.448, 7.164), 0.953, 1e-3, "PSR, three years");
    let r = run("psr", &[series(&normal_moments(24, 0.458))], &[], 0);
    within(r.statistic, 0.982, 1e-3, "psr on two years of normal moments");
    within(r.p_value.unwrap(), 1.0 - r.statistic, 0.0, "the p-value");
    for (q, years) in [(252.0, 2.73), (52.0, 2.83), (12.0, 3.24)] {
        let (sr, benchmark) = (2.0 / f64::sqrt(q), 1.0 / f64::sqrt(q));
        within(
            metrics::min_track_record_length(sr, benchmark, 0.0, 0.0, 0.95) / q,
            years,
            0.01,
            &format!("MinTRL years at {} a year", q),
        );
        let r = run("min_trl", &[series(&normal_moments(1200, sr))], &[("benchmark", float(benchmark))], 0);
        within(r.statistic / q, years, 0.01, &format!("min_trl years at {} a year", q));
        within(r.effect.unwrap(), 1200.0 - r.statistic, 1e-9, "observations to spare");
    }
    let (sr, benchmark) = (2.0 / 12f64.sqrt(), 1.0 / 12f64.sqrt());
    within(metrics::min_track_record_length(sr, benchmark, -0.72, 2.78, 0.95) / 12.0, 4.99, 0.01, "MinTRL years, HFR moments");
}

/// The numerical example of Bailey and López de Prado (2014, "The deflated
/// Sharpe ratio"): an annualised Sharpe ratio of 2.5 over five years of
/// daily returns (T = 1,250, 250 a year), the best of N = 100 trials whose
/// annualised Sharpe ratios have variance 1/2, with skewness −3 and
/// kurtosis 10, gives SR₀ = 0.1132 per period and DSR = 0.9004; 46 trials
/// give 0.9505, and normal returns 0.9505 at 88 trials. To one unit of the
/// last printed digit; the printed values as the reproductions named above
/// transcribe them alike. Skewness −3 with kurtosis 10 lies on the bound
/// γ₄ ≥ γ₃² + 1, which only a two-point law with an irrational weight
/// attains, so no finite series has those moments; the normal case runs
/// through the test on a series of 1,250 with normal moments, 830 zeros,
/// 200 each of ±1 and 10 each of ±√(10/7), whose kurtosis is 3.
#[test]
fn dsr_reproduces_the_deflated_sharpe_example() {
    let sr = 2.5 / 250f64.sqrt();
    let v = 0.5 / 250.0;
    let d = metrics::deflated_sharpe(sr, 1250, -3.0, 7.0, 100, v);
    within(d.expected_max_sr, 0.1132, 1e-4, "SR0");
    within(d.dsr, 0.9004, 1e-4, "DSR at 100 trials");
    within(metrics::deflated_sharpe(sr, 1250, -3.0, 7.0, 46, v).dsr, 0.9505, 1e-4, "DSR at 46 trials");
    within(metrics::deflated_sharpe(sr, 1250, 0.0, 0.0, 88, v).dsr, 0.9505, 1e-4, "DSR at 88 trials, normal returns");
    let t = (10.0f64 / 7.0).sqrt();
    let mut z = vec![0.0; 830];
    z.extend([1.0, -1.0].repeat(200));
    z.extend([t, -t].repeat(10));
    let base = Moments::of(&z);
    assert!(base.skewness().abs() < 1e-12 && base.excess_kurtosis().abs() < 1e-12, "{:?}", base);
    let x: Vec<f64> = z.iter().map(|v| v + sr * base.sd()).collect();
    let r = run("dsr", &[series(&x)], &[("trials", int(88)), ("var_trials", float(v))], 0);
    within(r.statistic, 0.9505, 1e-4, "dsr at 88 trials on normal moments");
    within(r.effect.unwrap(), sr - 0.1111, 1e-4, "the Sharpe ratio above SR0");
    // One trial deflates nothing: DSR is PSR against zero.
    let one = run("dsr", &[series(&x)], &[], 0);
    let psr = run("psr", &[series(&x)], &[], 0);
    assert_eq!(one.statistic.to_bits(), psr.statistic.to_bits());
}

/// The Sharpe-ratio tests report what the metrics report for the same
/// returns, to the bit: psr, dsr and min_trl the metrics' formulas on the
/// per-period Sharpe ratio, skewness and excess kurtosis of
/// `return_metrics`, bootstrap_sharpe its annualised ratio and nw_mean its
/// Newey–West t.
#[test]
fn the_sharpe_ratio_tests_are_the_metrics_to_the_bit() {
    let mut rng = Rng::stream(41, 0);
    let mut e = 100.0;
    let curve: Vec<(i64, f64)> = (0..600)
        .map(|i| {
            e *= 1.0 + 0.0015 + 0.01 * rng.normal();
            (i * 86_400_000_000_000, e)
        })
        .collect();
    let m = metrics::return_metrics(&curve, 252.0);
    let r = metrics::Returns::from_curve(&curve, 252.0).r;
    let t = [series(&r)];
    let psr = run("psr", &t, &[("benchmark", float(0.01))], 0);
    assert_eq!(psr.statistic.to_bits(), metrics::probabilistic_sharpe(m.sharpe_period, 0.01, m.n, m.skew, m.kurtosis).to_bits());
    let dsr = run("dsr", &t, &[("trials", int(30)), ("var_trials", float(0.002))], 0);
    assert_eq!(dsr.statistic.to_bits(), metrics::deflated_sharpe(m.sharpe_period, m.n, m.skew, m.kurtosis, 30, 0.002).dsr.to_bits());
    let trl = run("min_trl", &t, &[], 0);
    assert_eq!(trl.statistic.to_bits(), metrics::min_track_record_length(m.sharpe_period, 0.0, m.skew, m.kurtosis, 0.95).to_bits());
    let boot = run("bootstrap_sharpe", &t, &[("reps", int(99))], 0);
    assert_eq!(boot.statistic.to_bits(), m.sharpe.to_bits());
    let nw = run("nw_mean", &t, &[], 0);
    assert_eq!(nw.statistic.to_bits(), m.t_stat.to_bits());
    // The Sharpe ratio and the moments do not see the units.
    let scaled: Vec<f64> = r.iter().map(|v| v * 3.0).collect();
    close(run("psr", &[series(&scaled)], &[("benchmark", float(0.01))], 0).statistic, psr.statistic, 1e-12, "psr in other units");
    close(
        run("dsr", &[series(&scaled)], &[("trials", int(30)), ("var_trials", float(0.002))], 0).statistic,
        dsr.statistic,
        1e-12,
        "dsr in other units",
    );
    close(run("min_trl", &[series(&scaled)], &[], 0).statistic, trl.statistic, 1e-12, "min_trl in other units");
    close(run("nw_mean", &[series(&scaled)], &[], 0).statistic, nw.statistic, 1e-12, "nw_mean in other units");
    // A Sharpe ratio below its benchmark has no minimum track record.
    assert!(matches!(attempt("min_trl", &t, &[("benchmark", float(1.0))], 0), Err(TestError::Precondition(_))));
    assert!(attempt("psr", &[series(&[0.01; 30])], &[], 0).is_err());
}

// ---- pbo ----

/// Three trials over four blocks of two rows (S = 4, C(4, 2) = 6 splits),
/// each trial's rows in a block being m + 1 and m − 1 with block means
/// A = (0.6, 0.6, −0.2, −0.2), B = (−0.2, −0.2, 0.6, 0.6), C = 0.25 always.
/// Over two blocks with means m₁, m₂ the Sharpe ratio is
/// M/√(4(1 + d²)/3), M = (m₁ + m₂)/2, d = (m₁ − m₂)/2. In sample on blocks
/// {1, 2}, A is best (0.6·√3/2 = 0.520 against C's 0.217) and out of sample
/// worst (−0.173): rank 1 of 3, ω = 1/4, λ = ln(1/3) < 0. Likewise B on
/// {3, 4}. On the four mixed splits A and B have M = 0.2 and d = 0.4
/// (0.161), C is best (0.217) and best again out of sample: ω = 3/4,
/// λ = ln 3 > 0. So PBO = 2/6 = 1/3 and the mean logit
/// (2 ln(1/3) + 4 ln 3)/6 = ln(3)/3. By hand; to the rounding of the
/// arithmetic.
#[test]
fn pbo_on_a_hand_computed_grid() {
    let means = [[0.6, 0.6, -0.2, -0.2], [-0.2, -0.2, 0.6, 0.6], [0.25, 0.25, 0.25, 0.25]];
    let mut t = Table::default();
    for (name, m) in ["a", "b", "c"].iter().zip(means) {
        t.columns.insert(name.to_string(), m.iter().flat_map(|v| [v + 1.0, v - 1.0]).collect());
    }
    let r = run("pbo", std::slice::from_ref(&t), &[("partitions", int(4))], 0);
    within(r.statistic, 1.0 / 3.0, 1e-15, "PBO");
    within(r.effect.unwrap(), 3f64.ln() / 3.0, 1e-15, "mean logit");
    assert_eq!((r.n, r.p_value), (8, None));
    assert!(r.notes[0].starts_with("6 splits of 4 blocks of 2 rows, 3 trials"), "{:?}", r.notes);
    // Units change nothing; nor does running it again.
    let mut scaled = t.clone();
    scaled.columns.values_mut().for_each(|c| c.iter_mut().for_each(|v| *v *= 10.0));
    assert_eq!(run("pbo", &[scaled], &[("partitions", int(4))], 0), r);
    assert!(attempt("pbo", std::slice::from_ref(&t), &[("partitions", int(3))], 0).is_err());
    assert!(attempt("pbo", std::slice::from_ref(&t), &[("partitions", int(6))], 0).is_err());
}

// ---- bootstrap_sharpe ----

/// The coverage of the interval on simulated series with a planted Sharpe
/// ratio: 400 independent normal series of 500 returns with mean 0.05 and
/// deviation 1, an annualised Sharpe ratio of 0.05·√252. The 90 % interval
/// must cover the planted ratio in 90 % of the series, to 3.5 binomial
/// standard errors of the coverage over 400 series (±0.0525).
#[test]
fn bootstrap_sharpe_covers_a_planted_sharpe_ratio() {
    let m = 400;
    let truth = 0.05 * 252f64.sqrt();
    let mut covered = 0;
    for i in 0..m as u64 {
        let x: Vec<f64> = normals(500, 51, i).iter().map(|v| v + 0.05).collect();
        let r = run("bootstrap_sharpe", &[series(&x)], &[("level", float(0.9)), ("reps", int(399))], i);
        let (lo, hi, _) = r.interval.unwrap();
        covered += usize::from(lo <= truth && truth <= hi);
    }
    within(covered as f64 / m as f64, 0.90, binomial_band(0.90, m, 3.5), "coverage");
}

/// Under dependence the blocks carry the coverage: on 300 Gaussian AR(1)
/// series of 500 returns (φ = 0.3, unit marginal deviation, mean 0.05), the
/// bootstrap of independent draws (`block` 1) estimates the ratio's
/// variance as if the returns were independent, about (1 − φ)/(1 + φ) of
/// the truth, and its 90 % interval covers about 2Φ(1.645 √(0.7/1.3)) − 1
/// = 0.77 of the time; the stationary bootstrap's interval covers the
/// planted ratio in more of the same series, by more than three standard
/// errors of the paired difference (a block bootstrap's variance is still
/// short by a term of order 1/block, so it covers somewhat less than 90 %).
#[test]
fn bootstrap_sharpe_blocks_carry_the_dependence() {
    let truth = 0.05 * 252f64.sqrt();
    let covers = |r: &TestResult| r.interval.is_some_and(|(lo, hi, _)| lo <= truth && truth <= hi);
    let d: Vec<f64> = (0..300u64)
        .map(|i| {
            let x: Vec<f64> = ar1(500, 0.3, 52, i).iter().map(|v| v + 0.05).collect();
            let p = [("level", float(0.9)), ("reps", int(399))];
            let blocks = run("bootstrap_sharpe", &[series(&x)], &p, i);
            let iid = run("bootstrap_sharpe", &[series(&x)], &[p[0].clone(), p[1].clone(), ("block", float(1.0))], i);
            f64::from(u8::from(covers(&blocks))) - f64::from(u8::from(covers(&iid)))
        })
        .collect();
    let (gain, se) = paired(&d);
    assert!(gain > 3.0 * se, "the stationary bootstrap covers {} more often than the iid one (standard error {})", gain, se);
}

/// The p-value inverts the interval; the same seed gives the same bits; the
/// units change nothing.
#[test]
fn bootstrap_sharpe_is_reproducible_and_scale_free() {
    let x: Vec<f64> = normals(500, 53, 0).iter().map(|v| 0.01 * v + 0.0015).collect();
    let a = run("bootstrap_sharpe", &[series(&x)], &[], 5);
    let b = run("bootstrap_sharpe", &[series(&x)], &[], 5);
    assert_eq!(a.statistic.to_bits(), b.statistic.to_bits());
    assert_eq!(a.interval.map(|i| (i.0.to_bits(), i.1.to_bits())), b.interval.map(|i| (i.0.to_bits(), i.1.to_bits())));
    assert_eq!(a.p_value.map(f64::to_bits), b.p_value.map(f64::to_bits));
    let (lo, hi, _) = a.interval.unwrap();
    assert!(lo < a.statistic && a.statistic < hi);
    assert_eq!(
        lo > 0.0,
        a.p_value.unwrap() < 0.025,
        "the 95 % interval excludes zero when the one-sided p-value is below 2.5 %: {:?}",
        a
    );
    let scaled: Vec<f64> = x.iter().map(|v| v * 40.0).collect();
    let c = run("bootstrap_sharpe", &[series(&scaled)], &[], 5);
    close(c.statistic, a.statistic, 1e-12, "the ratio in other units");
    close(c.interval.unwrap().0, lo, 1e-12, "the interval's lower end in other units");
    close(c.interval.unwrap().1, hi, 1e-12, "the interval's upper end in other units");
}

// ---- nw_mean ----

/// A hand computation: x = 1, …, 8 at two lags. The mean is 9/2 and the
/// deviations ±1/2, ±3/2, ±5/2, ±7/2, so γ₀ = 42/8 = 21/4, γ₁ = 26.25/8 =
/// 105/32 and γ₂ = 11.5/8 = 23/16; with Bartlett weights 2/3 and 1/3 the
/// long-run variance is 21/4 + 2·(2/3)·105/32 + 2·(1/3)·23/16 = 127/12, the
/// standard error √(127/96) and t = (9/2)√(96/127) = 3.912427202411979
/// (mpmath), p = 2(1 − Φ(t)) = 9.1373087065390e-5. To 1e-14 relative.
/// Greene's example (Econometric Analysis) is not reachable from the
/// build's network.
#[test]
fn nw_mean_on_a_hand_computed_series() {
    let x: Vec<f64> = (1..=8).map(f64::from).collect();
    let r = run("nw_mean", &[series(&x)], &[("lags", int(2))], 0);
    close(r.statistic, 3.912_427_202_411_979, 1e-14, "t");
    close(r.p_value.unwrap(), 9.137_308_706_539_0e-5, 1e-9, "p");
    close(r.effect.unwrap(), 4.5, 1e-15, "mean");
    let (lo, hi, level) = r.interval.unwrap();
    close(hi - lo, 2.0 * 1.959_963_984_540_054 * (127.0f64 / 96.0).sqrt(), 1e-12, "the interval's width");
    assert_eq!(level, 0.95);
    // At zero lags it is the iid t with divisor n: √(21/4 / 8) = √(21/32).
    let r = run("nw_mean", &[series(&x)], &[("lags", int(0))], 0);
    close(r.statistic, 4.5 / (21.0f64 / 32.0).sqrt(), 1e-14, "t at zero lags");
}

// ---- wfe ----

/// Two hand examples. Folds earning (0.20, 0.10, 0.30, 0.20) in sample and
/// (0.10, 0.08, 0.12, −0.02) out of sample have WFE = 0.07/0.20 = 0.35;
/// against e₀ = 0.5 the excesses are (0, 0.03, −0.03, −0.12), summing to
/// −0.12, and of the 16 sign assignments of (0, 0.03, 0.03, 0.12) the sums
/// 0.18 (2), 0.12 (4), 0.06 (2), −0.06 (2) and −0.12 (4) reach it and
/// −0.18 (2) does not: p = 14/16. Folds earning (0.10, 0.12, 0.08, 0.10,
/// 0.10) and (0.08, 0.09, 0.07, 0.10, 0.06) have WFE = 0.08/0.10 = 0.8 and
/// excesses (0.03, 0.03, 0.03, 0.05, 0.01), all positive: only the observed
/// assignment reaches their sum, p = 1/32. To the rounding of the arithmetic.
#[test]
fn wfe_on_hand_computed_folds() {
    let folds = |is: &[f64], oos: &[f64]| {
        let mut t = Table::default();
        t.columns.insert("in_sample".into(), is.to_vec());
        t.columns.insert("out_of_sample".into(), oos.to_vec());
        t
    };
    let t = folds(&[0.20, 0.10, 0.30, 0.20], &[0.10, 0.08, 0.12, -0.02]);
    let r = run("wfe", std::slice::from_ref(&t), &[], 0);
    close(r.statistic, 0.35, 1e-14, "WFE");
    assert_eq!(r.p_value, Some(14.0 / 16.0));
    close(r.effect.unwrap(), -0.03, 1e-14, "mean excess");
    let robust = folds(&[0.10, 0.12, 0.08, 0.10, 0.10], &[0.08, 0.09, 0.07, 0.10, 0.06]);
    let r = run("wfe", std::slice::from_ref(&robust), &[], 0);
    close(r.statistic, 0.8, 1e-14, "WFE");
    assert_eq!(r.p_value, Some(1.0 / 32.0));
    // Units change nothing.
    let scaled = folds(&[2.0, 1.0, 3.0, 2.0], &[1.0, 0.8, 1.2, -0.2]);
    let s = run("wfe", &[scaled], &[], 0);
    close(s.statistic, 0.35, 1e-14, "WFE in other units");
    assert_eq!(s.p_value, Some(14.0 / 16.0));
    // A strategy that did not earn in sample has no efficiency.
    assert!(matches!(attempt("wfe", &[folds(&[-0.1, 0.05], &[0.1, 0.1])], &[], 0), Err(TestError::Precondition(_))));
}

/// Beyond twenty folds the assignments are drawn: thirty folds whose
/// excesses are all positive are reached only by the observed assignment
/// (exact p = 2⁻³⁰), which no draw repeats in practice, so the p-value is
/// that of the observed assignment alone, 1/(1 + R); the same seed gives the
/// same draws.
#[test]
fn wfe_draws_assignments_beyond_twenty_folds() {
    assert_eq!(sign_flip_p(&[1.0; 30], 2000, 3), 1.0 / 2001.0);
    let d: Vec<f64> = normals(30, 61, 0);
    assert_eq!(sign_flip_p(&d, 500, 4).to_bits(), sign_flip_p(&d, 500, 4).to_bits());
    let exact = sign_flip_p(&d[..12], 0, 0);
    let mut t = Table::default();
    t.columns.insert("in_sample".into(), vec![1.0; 12]);
    t.columns.insert("out_of_sample".into(), d[..12].iter().map(|v| v + 0.5).collect());
    assert_eq!(run("wfe", &[t], &[], 0).p_value, Some(exact));
}

// ---- reproducibility ----

/// The stationary-bootstrap tests give the same bits for the same seed
/// (hill_tail's and wfe's seeds are checked with them above); the statistic
/// does not depend on the seed, the resamples do.
#[test]
fn the_seeded_tests_are_bit_identical_for_a_seed() {
    let x: Vec<f64> = ar1(300, 0.4, 71, 0).iter().map(|v| 0.01 * v + 0.001).collect();
    for test in ["jarque_bera", "ks_normal", "bootstrap_sharpe"] {
        let p = [("reps", int(299))];
        let a = run(test, &[series(&x)], &p, 17);
        let b = run(test, &[series(&x)], &p, 17);
        assert_eq!(a.statistic.to_bits(), b.statistic.to_bits(), "{}", test);
        assert_eq!(a.p_value.map(f64::to_bits), b.p_value.map(f64::to_bits), "{}", test);
        assert_eq!(a.notes, b.notes, "{}", test);
        let c = run(test, &[series(&x)], &p, 18);
        assert_eq!(a.statistic.to_bits(), c.statistic.to_bits(), "{}: the statistic does not resample", test);
        if test == "bootstrap_sharpe" {
            assert_ne!(a.interval, c.interval, "another seed draws other resamples");
        }
        // The scale-free statistics do not see the units; their bootstrap
        // draws the same indices, so the p-value moves by at most a tie.
        let scaled: Vec<f64> = x.iter().map(|v| v * 250.0).collect();
        let d = run(test, &[series(&scaled)], &p, 17);
        close(d.statistic, a.statistic, 1e-11, &format!("{} in other units", test));
        within(d.p_value.unwrap(), a.p_value.unwrap(), 2.0 / 300.0, &format!("{} p-value in other units", test));
    }
}
