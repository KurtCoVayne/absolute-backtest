//! G9 for the time-series tests of the starting set (Q-4.4, Q-4.6): each
//! test against its known-answer source at the tolerance E-24 states for
//! it. Published critical values are checked to 1e-3. Where no published
//! example of a statistic is reachable, the value is another program's,
//! the last resort, and its comment says so, with the agreement its printed
//! digits allow. A simulated known answer is checked at a tolerance its
//! comment states and justifies: a planted parameter within a few of its
//! estimate's standard deviations, and the rejections of a simulated null
//! within about four binomial standard deviations of the size, with the
//! size a Monte Carlo of the same design gives quoted beside the bound. The
//! property tests of Q-4.6 close the file: every statistic of the group is
//! scale-free, and every result is the same to the bit for the same seed.

use std::collections::BTreeMap;
use std::path::PathBuf;

use abt_query::tests::regression::{ols, Covariance};
use abt_query::tests::resample::autocorrelation;
use abt_query::tests::rng::Rng;
use abt_query::tests::set::johansen::{self, Statistic};
use abt_query::tests::set::kpss;
use abt_query::tests::set::timeseries::{mackinnon_critical, mackinnon_p, Deterministic};
use abt_query::tests::{get, names, Param, Params, Table, TestError, TestResult};

fn fixture(name: &str) -> String {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests").join("fixtures").join(name);
    std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("{}: {}", p.display(), e))
}

/// Relative agreement (absolute when the wanted value is zero).
fn close(got: f64, want: f64, rel: f64, what: &str) {
    let err = if want == 0.0 { got.abs() } else { ((got - want) / want).abs() };
    assert!(err <= rel, "{}: {} against {} (relative error {:e} > {:e})", what, got, want, err, rel);
}

/// Absolute agreement.
fn near(got: f64, want: f64, abs: f64, what: &str) {
    assert!((got - want).abs() <= abs, "{}: {} against {} (error {:e} > {:e})", what, got, want, (got - want).abs(), abs);
}

fn params(list: &[(&str, Param)]) -> Params {
    list.iter().map(|(k, v)| (k.to_string(), v.clone())).collect()
}

/// The registered test `test` applied through the trait.
fn try_run(test: &str, tables: &[Table], list: &[(&str, Param)]) -> Result<TestResult, TestError> {
    get(test).unwrap_or_else(|| panic!("no test `{}`", test)).run(tables, &params(list), 7)
}

/// The registered test's result, which the input must give.
fn run(test: &str, tables: &[Table], list: &[(&str, Param)]) -> TestResult {
    try_run(test, tables, list).unwrap_or_else(|e| panic!("{}: {}", test, e))
}

fn int(v: i64) -> Param {
    Param::Int(v)
}

fn text(s: &str) -> Param {
    Param::Text(s.into())
}

fn series(x: &[f64]) -> Table {
    Table::series("x", x.to_vec())
}

/// A table of named columns on one time axis.
fn system(columns: &[(&str, &[f64])]) -> Table {
    let mut t = Table::default();
    for (name, c) in columns {
        t.columns.insert(name.to_string(), c.to_vec());
    }
    t
}

/// Box and Jenkins's (1976) Series A: the numbers after the title line,
/// comment lines skipped.
fn series_a() -> Vec<f64> {
    fixture("tsdl/seriesa.dat")
        .lines()
        .skip(1)
        .filter(|l| !l.trim_start().starts_with('#'))
        .flat_map(|l| l.split_whitespace().map(|v| v.parse::<f64>().unwrap()).collect::<Vec<_>>())
        .collect()
}

/// statsmodels' macrodata (US quarterly, 1959–2009) by column.
fn macrodata() -> BTreeMap<String, Vec<f64>> {
    let text = fixture("statsmodels/macrodata.csv");
    let mut lines = text.lines();
    let header: Vec<String> = lines.next().unwrap().split(',').map(|h| h.trim_matches('"').to_string()).collect();
    let mut columns: BTreeMap<String, Vec<f64>> = header.iter().map(|h| (h.clone(), Vec::new())).collect();
    for line in lines.filter(|l| !l.trim().is_empty()) {
        for (h, v) in header.iter().zip(line.split(',')) {
            columns.get_mut(h).unwrap().push(v.parse().unwrap());
        }
    }
    columns
}

/// The residuals of the regression statsmodels' diagnostic tests check R
/// against: 400 Δln real investment on a constant, 400 Δln real GDP and
/// the previous quarter's real interest rate.
fn macro_residuals() -> Vec<f64> {
    let d = macrodata();
    let growth = |c: &str| d[c].windows(2).map(|w| 400.0 * (w[1].ln() - w[0].ln())).collect::<Vec<f64>>();
    let y = growth("realinv");
    let rate = d["realint"][..d["realint"].len() - 1].to_vec();
    ols(&[vec![1.0; y.len()], growth("realgdp"), rate], &y, &Covariance::Iid).unwrap().residuals
}

/// Gaussian draws from the framework's generator.
fn normals(rng: &mut Rng, n: usize) -> Vec<f64> {
    (0..n).map(|_| rng.normal()).collect()
}

/// An AR(1) about `mu` with coefficient `phi` and standard normal shocks,
/// after a burn-in of 500.
fn ar1(rng: &mut Rng, n: usize, phi: f64, mu: f64) -> Vec<f64> {
    let mut x = 0.0;
    let mut out = Vec::with_capacity(n);
    for t in 0..n + 500 {
        x = phi * x + rng.normal();
        if t >= 500 {
            out.push(mu + x);
        }
    }
    out
}

/// A Gaussian random walk with drift, from zero.
fn walk(rng: &mut Rng, n: usize, drift: f64) -> Vec<f64> {
    let mut x = 0.0;
    (0..n)
        .map(|_| {
            x += drift + rng.normal();
            x
        })
        .collect()
}

#[test]
fn the_registry_holds_the_time_series_tests() {
    let all = names();
    for name in ["adf", "arch_lm", "engle_granger", "half_life", "johansen", "kpss", "ljung_box", "variance_ratio"] {
        assert!(all.contains(&name.to_string()), "{} is not registered", name);
        let t = get(name).unwrap();
        assert_eq!(t.name(), name);
        assert!(!t.null().is_empty() && !t.version().is_empty());
    }
}

// ljung_box

/// Q(m) of the Box–Jenkins Series A (197 two-hourly concentration readings
/// of a chemical process). No published Q of the raw series is reachable,
/// so the values are statsmodels 0.15.0's `acorr_ljungbox` (software, the
/// last resort), at 1e-10 relative: Q is a finite sum whose two
/// computations differ only by rounding. The p-values are χ² tails as small
/// as 1e-68, at 1e-8 relative.
#[test]
fn ljung_box_on_box_jenkins_series_a() {
    let a = series_a();
    assert_eq!(a.len(), 197);
    assert_eq!((a[0], a[196]), (17.0, 17.4));
    let want = [
        (1, 65.0225608252814, 7.40454554355129e-16),
        (2, 114.294845255343, 1.51771315274222e-25),
        (5, 193.813628940955, 5.97730380777406e-40),
        (10, 304.95665923286, 1.39184677460805e-59),
        (12, 318.072096803589, 7.47482949759025e-61),
        (20, 378.472065658025, 5.89046979131074e-68),
        (24, 391.235500240951, 4.71652286803172e-68),
    ];
    for (m, q, p) in want {
        let r = run("ljung_box", &[series(&a)], &[("lags", int(m))]);
        close(r.statistic, q, 1e-10, &format!("Series A Q({})", m));
        close(r.p_value.unwrap(), p, 1e-8, &format!("Series A p({})", m));
        assert_eq!(r.df, Some(m as f64));
        assert_eq!(r.n, 197);
    }
    // The default lag is min(10, n/5) = 10.
    close(run("ljung_box", &[series(&a)], &[]).statistic, 304.95665923286, 1e-10, "Series A Q(default)");
}

/// Q(4) and Q(40) of the residuals of the macrodata regression against R's
/// `Box.test` as statsmodels' `test_diagnostic.py` records it (software,
/// the last resort): printed to 15 digits, checked at 1e-10 relative.
#[test]
fn ljung_box_agrees_with_r_on_the_macrodata_residuals() {
    let u = macro_residuals();
    assert_eq!(u.len(), 202);
    let r = run("ljung_box", &[series(&u)], &[("lags", int(4))]);
    close(r.statistic, 5.23587172795227, 1e-10, "Q(4)");
    close(r.p_value.unwrap(), 0.263940335284713, 1e-10, "p(4)");
    let r = run("ljung_box", &[series(&u)], &[("lags", int(40))]);
    close(r.statistic, 51.03724531797195, 1e-10, "Q(40)");
    close(r.p_value.unwrap(), 0.11334744923390, 1e-10, "p(40)");
}

// variance_ratio

/// VR(2) is one plus the first autocorrelation of the increments, up to
/// end terms: Lo and MacKinlay's estimators give exactly
/// VR(2) = (1 + ρ̂₁ − (e₁² + e_T²) / (2 Σ e²)) / (1 − 2/T), e the demeaned
/// increments, an identity of any series, checked at 1e-12 relative. On
/// increments that are an AR(1) with the planted ρ = 0.3 and T = 200,000,
/// VR(2) is 1 + ρ = 1.3 within 0.011: the end terms are O(1/T) and ρ̂₁ has
/// the standard deviation √((1 − ρ²)/T) = 0.0021, of which 0.011 is five.
#[test]
fn variance_ratio_two_is_one_plus_the_first_autocorrelation() {
    let mut rng = Rng::stream(11, 0);
    let increments = ar1(&mut rng, 200_000, 0.3, 0.001);
    let r = run("variance_ratio", &[series(&increments)], &[("increments", Param::Bool(true))]);
    let vr = r.effect.unwrap();
    let t = increments.len() as f64;
    let mean = increments.iter().sum::<f64>() / t;
    let e: Vec<f64> = increments.iter().map(|v| v - mean).collect();
    let s0: f64 = e.iter().map(|v| v * v).sum();
    let identity = (1.0 + autocorrelation(&increments, 1) - (e[0] * e[0] + e[e.len() - 1] * e[e.len() - 1]) / (2.0 * s0)) / (1.0 - 2.0 / t);
    close(vr, identity, 1e-12, "VR(2) against 1 + ρ̂₁");
    near(vr, 1.3, 0.011, "VR(2) against the planted 1 + ρ");
    assert!(r.p_value.unwrap() < 1e-12, "{:?}", r);
    assert_eq!(r.n, 200_000);
}

/// A Gaussian random walk of T = 10,000 steps: VR(q) − 1 has the
/// asymptotic standard deviation √(2 (2q − 1)(q − 1) / (3qT)) (Lo and
/// MacKinlay 1988), and VR(q) is within five of them of one for q = 2, 4,
/// 8 and 16. Heteroskedastic increments (σ = 1 for the first half of a
/// walk, 3 for the second): over 400 walks of 1,000 steps the robust z*
/// rejects at 5 % in 3 to 37 of them (20 ± 4 binomial standard deviations;
/// a Monte Carlo of the design with arch gives 5.6 %).
#[test]
fn variance_ratio_of_a_random_walk_is_near_one() {
    let mut rng = Rng::stream(12, 0);
    let w = walk(&mut rng, 10_001, 0.0);
    for q in [2usize, 4, 8, 16] {
        let r = run("variance_ratio", &[series(&w)], &[("q", int(q as i64))]);
        let qf = q as f64;
        let sd = (2.0 * (2.0 * qf - 1.0) * (qf - 1.0) / (3.0 * qf * 10_000.0)).sqrt();
        near(r.effect.unwrap(), 1.0, 5.0 * sd, &format!("VR({}) of a random walk", q));
    }
    let mut rejected = 0;
    for rep in 0..400 {
        let mut rng = Rng::stream(13, rep);
        let mut level = vec![0.0];
        for t in 0..1000 {
            let s = if t < 500 { 1.0 } else { 3.0 };
            level.push(level[t] + s * rng.normal());
        }
        if run("variance_ratio", &[series(&level)], &[]).p_value.unwrap() < 0.05 {
            rejected += 1;
        }
    }
    assert!((3..=37).contains(&rejected), "the robust z* rejected {} of 400 heteroskedastic random walks", rejected);
}

/// VR(q) and z*(q) of ln real GDP (macrodata) against arch 8.0.0's
/// `VarianceRatio` (Sheppard; overlapping, debiased, robust: Lo and
/// MacKinlay's estimators) for q = 2, 4, 8 and 16 (software, the last
/// resort: the paper's example is on CRSP returns, which are not public),
/// at 1e-10 relative.
#[test]
fn variance_ratio_agrees_with_arch_on_log_real_gdp() {
    let lgdp: Vec<f64> = macrodata()["realgdp"].iter().map(|v| v.ln()).collect();
    let want = [
        (2, 1.30509427780659, 3.64210295472063, 0.000270419872913319),
        (4, 1.76199068094613, 4.96320502265707, 6.93393187090052e-07),
        (8, 1.94767483211387, 4.01322558477332, 5.98946235192965e-05),
        (16, 1.77967356276853, 2.27836456697716, 0.0227048656724533),
    ];
    for (q, vr, z, p) in want {
        let r = run("variance_ratio", &[series(&lgdp)], &[("q", int(q))]);
        close(r.effect.unwrap(), vr, 1e-10, &format!("VR({})", q));
        close(r.statistic, z, 1e-10, &format!("z*({})", q));
        close(r.p_value.unwrap(), p, 1e-9, &format!("p({})", q));
    }
}

// arch_lm

/// Engle's LM with 4 and 12 lags of the macrodata residuals against R's
/// `FinTS::ArchTest` as statsmodels' `test_diagnostic.py` records it
/// (software, the last resort: Engle's UK inflation example is not
/// reachable): 15 digits printed, checked at 1e-10 relative. `ArchTest`
/// does not demean; the residuals of a regression with a constant have
/// mean zero to rounding.
#[test]
fn arch_lm_agrees_with_r_on_the_macrodata_residuals() {
    let u = macro_residuals();
    for (m, lm, p) in [(4, 3.43473400836259, 0.487871315392619), (12, 8.648320999014171, 0.732638635007718)] {
        let r = run("arch_lm", &[series(&u)], &[("lags", int(m))]);
        close(r.statistic, lm, 1e-10, &format!("LM({})", m));
        close(r.p_value.unwrap(), p, 1e-10, &format!("p({})", m));
        assert_eq!((r.df, r.n), (Some(m as f64), 202 - m as usize));
    }
}

/// A simulated ARCH(1), ε_t = σ_t η_t with σ²_t = 1 + 0.2 ε²_{t−1} and
/// n = 100,000: the LM with one lag rejects (p below 1e-12), and the
/// effect, the coefficient of ε²_{t−1}, is the planted 0.2 within 0.034
/// (its standard deviation over 100 samples of the design is 0.0068, and
/// 0.034 is five of them). Homoskedastic draws: over 200 samples of 500,
/// the LM with five lags rejects at 5 % in at most 22 (10 + 4 binomial
/// standard deviations; a Monte Carlo of the design gives 4.1 %).
#[test]
fn arch_lm_finds_a_planted_arch_effect_and_none_in_white_noise() {
    let mut rng = Rng::stream(21, 0);
    let mut prev: f64 = 0.0;
    let mut e = Vec::with_capacity(100_000);
    for t in 0..100_500 {
        prev = (1.0 + 0.2 * prev * prev).sqrt() * rng.normal();
        if t >= 500 {
            e.push(prev);
        }
    }
    let r = run("arch_lm", &[series(&e)], &[("lags", int(1))]);
    assert!(r.p_value.unwrap() < 1e-12, "{:?}", r);
    near(r.effect.unwrap(), 0.2, 0.034, "the ARCH(1) coefficient");
    let mut rejected = 0;
    for rep in 0..200 {
        let mut rng = Rng::stream(22, rep);
        if run("arch_lm", &[series(&normals(&mut rng, 500))], &[("lags", int(5))]).p_value.unwrap() < 0.05 {
            rejected += 1;
        }
    }
    assert!(rejected <= 22, "arch_lm rejected {} of 200 homoskedastic samples", rejected);
}

// adf and engle_granger: MacKinnon's tables

/// MacKinnon (2010), Table 2: the asymptotic critical values β∞ of τ for
/// N = 1 to 12 at 1, 5 and 10 %, with a constant and with a trend, to 1e-3
/// (E-24).
#[test]
fn mackinnon_critical_values_are_the_2010_table() {
    let constant = [
        [-3.43035, -2.86154, -2.56677],
        [-3.89644, -3.33613, -3.04445],
        [-4.29374, -3.74066, -3.45218],
        [-4.64332, -4.09600, -3.81020],
        [-4.95756, -4.41519, -4.13157],
        [-5.24568, -4.70693, -4.42501],
        [-5.51233, -4.97684, -4.69648],
        [-5.76202, -5.22924, -4.95007],
        [-5.99742, -5.46697, -5.18897],
        [-6.22103, -5.69244, -5.41533],
        [-6.43377, -5.90714, -5.63086],
        [-6.63790, -6.11279, -5.83724],
    ];
    let trend = [
        [-3.95877, -3.41049, -3.12705],
        [-4.32762, -3.78057, -3.49631],
        [-4.66305, -4.11890, -3.83511],
        [-4.96940, -4.42871, -4.14633],
        [-5.25276, -4.71537, -4.43422],
        [-5.51727, -4.98228, -4.70233],
        [-5.76537, -5.23299, -4.95405],
        [-6.00003, -5.46971, -5.19183],
        [-6.22288, -5.69447, -5.41738],
        [-6.43551, -5.90887, -5.63255],
        [-6.63894, -6.11404, -5.83850],
        [-6.83488, -6.31127, -6.03650],
    ];
    for (det, table) in [(Deterministic::Constant, &constant), (Deterministic::Trend, &trend)] {
        for (i, row) in table.iter().enumerate() {
            let cv = mackinnon_critical(i + 1, det, None).unwrap();
            for (j, want) in row.iter().enumerate() {
                near(cv[j], *want, 1e-3, &format!("N = {} `{}` level {}", i + 1, det.name(), j));
            }
        }
    }
    assert!(mackinnon_critical(13, Deterministic::Constant, None).is_none());
    assert!(mackinnon_critical(0, Deterministic::Constant, None).is_none());
    // The finite-sample surface at T = 200 for N = 1 as statsmodels 0.15.0
    // evaluates it (software; its N = 1 rows equal the table), at 1e-9.
    for (det, want) in [
        (Deterministic::Constant, [-3.463476079125, -2.876102355, -2.574532225]),
        (Deterministic::Trend, [-4.004762969375, -3.43267357175, -3.1400789225]),
    ] {
        let cv = mackinnon_critical(1, det, Some(200.0)).unwrap();
        for j in 0..3 {
            near(cv[j], want[j], 1e-9, &format!("N = 1 `{}` at T = 200", det.name()));
        }
    }
}

/// MacKinnon's (1994) approximate p-value at MacKinnon's (2010) asymptotic
/// critical values is their size, to 1e-3, for N = 1 to 6 with a constant
/// and with a trend: two publications, two simulations, one distribution
/// (the largest gap is 2.4e-4). The p-value is a bound outside the 1994
/// fit's range, and absent beyond six variables.
#[test]
fn mackinnon_p_values_at_the_2010_critical_values_are_their_sizes() {
    for det in [Deterministic::Constant, Deterministic::Trend] {
        for n in 1..=6 {
            let cv = mackinnon_critical(n, det, None).unwrap();
            for (c, size) in cv.iter().zip([0.01, 0.05, 0.10]) {
                near(mackinnon_p(*c, n, det).unwrap(), size, 1e-3, &format!("p at the {} critical value, N = {} `{}`", size, n, det.name()));
            }
        }
        assert_eq!(mackinnon_p(-40.0, 1, det), Some(0.0));
        assert_eq!(mackinnon_p(5.0, 1, det), Some(1.0));
        assert_eq!(mackinnon_p(-3.0, 7, det), None);
    }
    assert_eq!(mackinnon_p(-3.0, 1, Deterministic::None), None);
}

// adf

/// τ and its p-value against Stata's `dfuller` on macrodata as statsmodels'
/// `test_stattools.py` records it (software, the last resort: Nelson and
/// Plosser's Table 5 is not reachable). Real GDP with four lags, with a
/// constant and with a trend; inflation with one lag, likewise. Stata's τ
/// differs from statsmodels' double-precision one by up to 1.6e-6
/// (presumably the data held in Stata's single precision), and from this
/// one by the same: within 5e-6, and the p-values within 1e-6.
#[test]
fn adf_agrees_with_stata_on_macrodata() {
    let d = macrodata();
    let want = [
        ("realgdp", 4, "c", 0.97505319, 0.99399563),
        ("realgdp", 4, "ct", -1.8566374, 0.67682968),
        ("infl", 1, "c", -4.3346988, 0.00038661),
        ("infl", 1, "ct", -4.425093, 0.00199633),
    ];
    for (column, lags, trend, tau, p) in want {
        let r = run("adf", &[series(&d[column])], &[("lags", int(lags)), ("trend", text(trend))]);
        near(r.statistic, tau, 5e-6, &format!("τ of {} `{}`", column, trend));
        near(r.p_value.unwrap(), p, 1e-6, &format!("p of {} `{}`", column, trend));
        assert_eq!(r.n, 203 - 1 - lags as usize);
    }
}

/// The lag order AIC chooses, with τ and its p-value, against statsmodels
/// 0.15.0's `adfuller(autolag="AIC", maxlag=12)` (software): two lags for
/// inflation and for ln real GDP, one for its difference; τ and p at 1e-9
/// relative.
#[test]
fn adf_chooses_the_lag_by_aic_as_statsmodels() {
    let d = macrodata();
    let lgdp: Vec<f64> = d["realgdp"].iter().map(|v| v.ln()).collect();
    let dlgdp: Vec<f64> = lgdp.windows(2).map(|w| w[1] - w[0]).collect();
    // (series, data, trend, lags chosen, τ, p)
    type Case<'a> = (&'a str, &'a [f64], &'a str, usize, f64, f64);
    let want: [Case; 6] = [
        ("infl", &d["infl"], "c", 2, -3.05451449625724, 0.0301076208634858),
        ("infl", &d["infl"], "ct", 2, -3.14857705028268, 0.0951437481329846),
        ("ln realgdp", &lgdp, "c", 2, -1.79535076632025, 0.38277233991493),
        ("ln realgdp", &lgdp, "ct", 2, -2.38287183872195, 0.388763544399562),
        ("Δ ln realgdp", &dlgdp, "c", 1, -6.9728713472163, 8.57509584489529e-10),
        ("Δ ln realgdp", &dlgdp, "ct", 1, -7.18550889821374, 5.35507353007166e-09),
    ];
    for (name, x, trend, lags, tau, p) in want {
        let r = run("adf", &[series(x)], &[("max_lags", int(12)), ("trend", text(trend))]);
        assert!(r.notes[0].starts_with(&format!("lags {} ", lags)), "{} `{}`: {:?}", name, trend, r.notes);
        close(r.statistic, tau, 1e-9, &format!("τ of {} `{}`", name, trend));
        close(r.p_value.unwrap(), p, 1e-9, &format!("p of {} `{}`", name, trend));
        assert_eq!(r.n, x.len() - 1 - lags);
    }
}

/// Simulated: over 300 Gaussian random walks of 250 steps, adf with the
/// default lag choice rejects the unit root at 5 % in at most 36, with a
/// constant and with a trend (a Monte Carlo of the design gives 3.8 % and
/// 6.6 %; 36 is 12 %, 3.7 standard deviations above the larger). A
/// stationary AR(1) with φ = 0.5 and n = 500 is rejected with p below 1e-6:
/// τ is near (φ − 1) / √((1 − φ²)/n) = −12.9, far in the left tail; the
/// effect, the sum of the autoregressive coefficients, is 0.5 within 0.15
/// (its standard deviation is about √((1 − φ²)/n) = 0.04).
#[test]
fn adf_keeps_a_random_walk_and_rejects_a_stationary_ar1() {
    for trend in ["c", "ct"] {
        let mut rejected = 0;
        for rep in 0..300 {
            let mut rng = Rng::stream(31, rep);
            if run("adf", &[series(&walk(&mut rng, 250, 0.0))], &[("trend", text(trend))]).p_value.unwrap() < 0.05 {
                rejected += 1;
            }
        }
        assert!(rejected <= 36, "adf `{}` rejected {} of 300 random walks", trend, rejected);
    }
    let mut rng = Rng::stream(32, 0);
    let r = run("adf", &[series(&ar1(&mut rng, 500, 0.5, 3.0))], &[]);
    assert!(r.p_value.unwrap() < 1e-6, "{:?}", r);
    near(r.effect.unwrap(), 0.5, 0.15, "the AR(1) persistence");
}

// kpss

/// Kwiatkowski et al. (1992), Table 1, to 1e-3: the critical values of η_μ
/// and η_τ at 10, 5, 2.5 and 1 %. The p-value at each is its size exactly;
/// between two it interpolates linearly; beyond the table it is the
/// table's bound, with a note.
#[test]
fn kpss_p_values_are_table_1() {
    let level = [0.347, 0.463, 0.574, 0.739];
    let trend = [0.119, 0.146, 0.176, 0.216];
    for (det, cv, table) in [(Deterministic::Constant, &level, &kpss::LEVEL), (Deterministic::Trend, &trend, &kpss::TREND)] {
        for i in 0..4 {
            near(table[i], cv[i], 1e-3, "Table 1");
            let (p, bound) = kpss::p_value(cv[i], det);
            near(p, [0.10, 0.05, 0.025, 0.01][i], 1e-15, "p at a critical value");
            assert!(bound.is_none());
        }
        let (p, _) = kpss::p_value((cv[0] + cv[1]) / 2.0, det);
        near(p, 0.075, 1e-12, "midway between 10 and 5 %");
        assert_eq!(kpss::p_value(cv[0] / 2.0, det).0, 0.10);
        assert!(kpss::p_value(cv[0] / 2.0, det).1.is_some_and(|n| n.contains("greater")));
        assert_eq!(kpss::p_value(cv[3] * 2.0, det).0, 0.01);
        assert!(kpss::p_value(cv[3] * 2.0, det).1.is_some_and(|n| n.contains("smaller")));
    }
}

/// η of real GDP (macrodata) with three lags against R's `tseries` as
/// statsmodels' `test_stattools.py` records it (software, the last resort):
/// η_μ 5.0169 and η_τ 1.1828, printed to four decimals, so within 5e-5;
/// both beyond the table, so p is its bound 0.01.
#[test]
fn kpss_agrees_with_r_on_real_gdp() {
    let gdp = macrodata()["realgdp"].clone();
    for (trend, eta) in [("c", 5.0169), ("ct", 1.1828)] {
        let r = run("kpss", &[series(&gdp)], &[("lags", int(3)), ("trend", text(trend))]);
        near(r.statistic, eta, 5e-5, &format!("η `{}`", trend));
        assert_eq!(r.p_value, Some(0.01));
        assert!(r.notes.iter().any(|n| n.contains("smaller than 0.01")), "{:?}", r.notes);
    }
}

/// Simulated: over 400 samples of 250 normal draws, kpss rejects
/// stationarity at 5 % in at most 37, about a level and about a trend
/// (20 + 4 binomial standard deviations; over 4,000 samples of the design
/// the rejection rates are 4.9 % and 4.85 %, and the simulated 90 and 95 %
/// quantiles of η are Table 1's within 0.006). A Gaussian random walk of
/// 1,000 steps is rejected at the table's bound: under a unit root η grows
/// with n over the bandwidth.
#[test]
fn kpss_keeps_white_noise_and_rejects_a_random_walk() {
    for trend in ["c", "ct"] {
        let mut rejected = 0;
        for rep in 0..400 {
            let mut rng = Rng::stream(41, rep);
            if run("kpss", &[series(&normals(&mut rng, 250))], &[("trend", text(trend))]).p_value.unwrap() < 0.05 {
                rejected += 1;
            }
        }
        assert!(rejected <= 37, "kpss `{}` rejected {} of 400 white noises", trend, rejected);
    }
    let mut rng = Rng::stream(42, 0);
    let r = run("kpss", &[series(&walk(&mut rng, 1000, 0.0))], &[]);
    assert_eq!(r.p_value, Some(0.01), "{:?}", r);
    assert!(r.statistic > 0.739);
}

// half_life

/// A simulated AR(1) with φ = 0.9 about 5 (an Ornstein–Uhlenbeck process
/// sampled once a bar, θ = −ln 0.9), n = 20,000: the half-life is
/// −ln 2 / ln 0.9 = 6.579 within 1.07, five standard deviations of the
/// estimate by the delta method (√((1 − φ²)/n) · ln 2 / (φ ln² φ) = 0.214);
/// the interval covers it (a 95 % event, whose frequency the replications
/// check); φ̂ is 0.9 within 0.016, five of √((1 − φ²)/n).
/// Over 400 samples of 1,000, the 95 % Newey–West interval covers the
/// planted half-life in 360 to 396 (0.95 ± 4 binomial standard deviations;
/// a Monte Carlo of the design gives 95.5 %).
#[test]
fn half_life_of_a_simulated_ar1_and_its_interval() {
    let h0 = -std::f64::consts::LN_2 / 0.9f64.ln();
    let mut rng = Rng::stream(51, 0);
    let r = run("half_life", &[series(&ar1(&mut rng, 20_000, 0.9, 5.0))], &[]);
    near(r.statistic, h0, 1.07, "the half-life");
    near(r.effect.unwrap(), 0.9, 0.016, "φ̂");
    let (lo, hi, level) = r.interval.unwrap();
    assert!(lo <= h0 && h0 <= hi && level == 0.95, "{:?}", r.interval);
    assert!(r.p_value.unwrap() < 1e-12, "{:?}", r);
    let mut covered = 0;
    for rep in 0..400 {
        let mut rng = Rng::stream(52, rep);
        let (lo, hi, _) = run("half_life", &[series(&ar1(&mut rng, 1000, 0.9, 5.0))], &[]).interval.unwrap();
        if lo <= h0 && h0 <= hi {
            covered += 1;
        }
    }
    assert!((360..=396).contains(&covered), "the interval covered the half-life in {} of 400", covered);
}

/// A random walk has a long half-life: under a unit root n (φ̂ − 1) has the
/// Dickey–Fuller distribution, below −20.7 with probability 1 % (Fuller's
/// table, with a constant), so for n = 2,000 the half-life exceeds
/// −ln 2 / ln(1 − 20.7/2000) = 66.6 bars in all but one sample in a
/// hundred. The half-life is zero at or below φ = 0 and infinite at or
/// above one.
#[test]
fn half_life_of_a_random_walk_is_long() {
    let mut rng = Rng::stream(53, 0);
    let r = run("half_life", &[series(&walk(&mut rng, 2001, 0.0))], &[]);
    assert!(r.statistic > 66.6, "{:?}", r);
    assert!(r.interval.unwrap().1 > r.statistic);
    assert_eq!(abt_query::tests::set::half_life::half_life(1.0), f64::INFINITY);
    assert_eq!(abt_query::tests::set::half_life::half_life(-0.3), 0.0);
    close(abt_query::tests::set::half_life::half_life(0.5), 1.0, 1e-15, "the half-life at φ = 1/2");
}

// engle_granger

/// τ against Stata's `egranger` (Schaffer) as statsmodels'
/// `test_stattools.py` records it (software, the last resort). The data
/// are that test's: numpy's `RandomState(123)`, a random walk shared by
/// `y0` and `y1` plus noise in all four, 200 rows rounded to four
/// decimals (`coint_egranger.csv`), with four fixed lags. `y0` on `y1`, on
/// (`y1`, `y2`), on (`y2`, `y3`) and on all three, with a constant and with
/// a trend: τ printed to 13 digits, checked at 1e-10 relative; the critical
/// values at T = 199 (12 digits) at 1e-9. Stata's 1 % value for N = 2 with
/// a constant is MacKinnon's β₂ = −22.527, the paper's, not statsmodels'
/// −33.527.
#[test]
fn engle_granger_agrees_with_stata() {
    let data = fixture("statsmodels/coint_egranger.csv");
    let rows: Vec<Vec<f64>> = data.lines().skip(1).map(|l| l.split(',').map(|v| v.parse().unwrap()).collect()).collect();
    assert_eq!(rows.len(), 200);
    let col = |j: usize| rows.iter().map(|r| r[j]).collect::<Vec<f64>>();
    let (y0, y1, y2, y3) = (col(0), col(1), col(2), col(3));
    let sets: [Vec<(&str, &[f64])>; 4] = [
        vec![("y0", &y0), ("y1", &y1)],
        vec![("y0", &y0), ("y1", &y1), ("y2", &y2)],
        vec![("y0", &y0), ("y2", &y2), ("y3", &y3)],
        vec![("y0", &y0), ("y1", &y1), ("y2", &y2), ("y3", &y3)],
    ];
    let stata = [
        (
            "c",
            [
                [-5.760696844656, -3.952043522638, -3.367006313729, -3.065831247948],
                [-5.781087068772, -4.367111915942, -3.783961136005, -3.483501524709],
                [-2.477444137366, -4.367111915942, -3.783961136005, -3.483501524709],
                [-5.778205811661, -4.735249216434, -4.152738973763, -3.852480848968],
            ],
        ),
        (
            "ct",
            [
                [-5.615251442239, -4.406102369132, -3.82866685109, -3.532082997903],
                [-5.63591313706, -4.758609717199, -4.179130554708, -3.880909696863],
                [-2.892029275027, -4.758609717199, -4.179130554708, -3.880909696863],
                [-5.626932544079, -5.08363327039, -4.502469783057, -4.2031051091],
            ],
        ),
    ];
    for (trend, results) in stata {
        let det = if trend == "c" { Deterministic::Constant } else { Deterministic::Trend };
        for (set, want) in sets.iter().zip(results) {
            let r = run("engle_granger", &[system(set)], &[("y", text("y0")), ("lags", int(4)), ("trend", text(trend))]);
            close(r.statistic, want[0], 1e-10, &format!("τ of y0 on {} series `{}`", set.len() - 1, trend));
            let cv = mackinnon_critical(set.len(), det, Some(199.0)).unwrap();
            for j in 0..3 {
                near(cv[j], want[j + 1], 1e-9, &format!("critical value {} for N = {} `{}`", j, set.len(), trend));
            }
            assert!(r.notes.iter().any(|n| n.contains("T = 199")), "{:?}", r.notes);
        }
    }
}

/// Simulated: x a Gaussian random walk and y = 1 + 2x + u with u an AR(1)
/// (φ = 0.5), n = 500: no cointegration is rejected with p below 1e-4, and
/// the cointegrating slope is 2 within 0.06 (the regression is
/// superconsistent; over 2,000 samples of the design its error exceeded
/// 0.052 in one in a thousand). Over 200 pairs of independent
/// random walks of 250 steps, no cointegration is rejected at 5 % in at
/// most 22 (10 + 4 binomial standard deviations; a Monte Carlo of the
/// design gives 4.8 %).
#[test]
fn engle_granger_finds_a_cointegrated_pair_and_not_independent_walks() {
    let mut rng = Rng::stream(61, 0);
    let x = walk(&mut rng, 500, 0.0);
    let u = ar1(&mut rng, 500, 0.5, 0.0);
    let y: Vec<f64> = x.iter().zip(&u).map(|(x, u)| 1.0 + 2.0 * x + u).collect();
    let r = run("engle_granger", &[system(&[("x", &x), ("y", &y)])], &[]);
    assert!(r.p_value.unwrap() < 1e-4, "{:?}", r);
    let slope: f64 = r.notes[0].rsplit("x ").next().unwrap().parse().unwrap();
    near(slope, 2.0, 0.06, "the cointegrating slope");
    let mut rejected = 0;
    for rep in 0..200 {
        let mut rng = Rng::stream(62, rep);
        let (a, b) = (walk(&mut rng, 250, 0.0), walk(&mut rng, 250, 0.0));
        if run("engle_granger", &[system(&[("a", &a), ("b", &b)])], &[]).p_value.unwrap() < 0.05 {
            rejected += 1;
        }
    }
    assert!(rejected <= 22, "engle_granger rejected {} of 200 independent pairs", rejected);
}

// johansen

/// Osterwald-Lenum (1992), Table 1, to 1e-3: the 95 and 99 % quantiles of
/// both statistics for k − r = 1 to 11 as Stata's `vecrank` reports them
/// (a transcription of the table independent of the one embedded). The
/// p-value at every tabulated quantile is its upper-tail probability, and
/// beyond the table it is the bound, with a note.
#[test]
fn johansen_quantiles_are_osterwald_lenums_table_1() {
    let trace95 = [3.76, 15.41, 29.68, 47.21, 68.52, 94.15, 124.24, 156.00, 192.89, 233.13, 277.71];
    let trace99 = [6.65, 20.04, 35.65, 54.46, 76.07, 103.18, 133.57, 168.36, 204.95, 247.18, 293.44];
    let max95 = [3.76, 14.07, 20.97, 27.07, 33.46, 39.37, 45.28, 51.42, 57.12, 62.81, 68.83];
    let max99 = [6.65, 18.63, 25.52, 32.24, 38.77, 45.10, 51.57, 57.69, 62.80, 69.09, 75.95];
    for d in 0..11 {
        near(johansen::TRACE[d][3], trace95[d], 1e-3, "trace 95 %");
        near(johansen::TRACE[d][5], trace99[d], 1e-3, "trace 99 %");
        near(johansen::MAX[d][3], max95[d], 1e-3, "max 95 %");
        near(johansen::MAX[d][5], max99[d], 1e-3, "max 99 %");
        for (which, table) in [(Statistic::Trace, &johansen::TRACE), (Statistic::MaxEigen, &johansen::MAX)] {
            for (i, q) in johansen::QUANTILES.iter().enumerate() {
                let (p, bound) = johansen::p_value(table[d][i], d + 1, which).unwrap();
                near(p, 1.0 - q, 1e-12, "p at a quantile");
                assert!(bound.is_none());
            }
            assert!(table[d].windows(2).all(|w| w[0] < w[1]), "quantiles in order");
            let (p, bound) = johansen::p_value(table[d][5] + 1.0, d + 1, which).unwrap();
            assert!(p == 0.01 && bound.is_some());
        }
    }
    assert!(johansen::p_value(10.0, 12, Statistic::Trace).is_none());
}

/// The trace and maximum-eigenvalue statistics against LeSage's `jplv`
/// (Spatial Econometrics Toolbox for MATLAB) as statsmodels'
/// `test_coint.py` records it (software, the last resort): the eight series
/// of `test_coint.csv` (173 rows), a constant and nine lagged differences,
/// r = 0 to 7, at 1e-8 relative (a 163-observation problem with 73
/// regressors per equation; the Python computation of the same agrees to
/// about 1e-11).
#[test]
fn johansen_agrees_with_jplv() {
    let data = fixture("statsmodels/test_coint.csv");
    let rows: Vec<Vec<f64>> = data
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| l.split_whitespace().map(|v| v.parse().unwrap()).collect())
        .collect();
    assert_eq!(rows.len(), 173);
    let names = ["a", "b", "c", "d", "e", "f", "g", "h"];
    let columns: Vec<Vec<f64>> = (0..8).map(|j| rows.iter().map(|r| r[j]).collect()).collect();
    let table = system(&names.iter().zip(&columns).map(|(n, c)| (*n, c.as_slice())).collect::<Vec<_>>());
    let trace = [
        307.6888935095814,
        205.3839229398245,
        129.1330243009336,
        83.3101865760208,
        52.51955460357912,
        30.20027050520502,
        13.84158157562689,
        0.4117390188204866,
    ];
    let max = [
        102.3049705697569,
        76.25089863889085,
        45.82283772491284,
        30.7906319724417,
        22.31928409837409,
        16.35868892957814,
        13.4298425568064,
        0.4117390188204866,
    ];
    for r in 0..8 {
        let t = run("johansen", std::slice::from_ref(&table), &[("lags", int(9)), ("rank", int(r as i64))]);
        close(t.statistic, trace[r], 1e-8, &format!("trace, r = {}", r));
        let m = run("johansen", std::slice::from_ref(&table), &[("lags", int(9)), ("rank", int(r as i64)), ("statistic", text("max"))]);
        close(m.statistic, max[r], 1e-8, &format!("max, r = {}", r));
        assert_eq!(t.n, 163);
    }
}

/// Simulated: two Gaussian random walks with drift 0.2, and a third series
/// that is their mean plus an AR(1) (φ = 0.5): one cointegrating vector.
/// Over 200 systems of 250 steps, the sequential trace tests at 5 % find
/// rank 1 in at least 176 (0.95 − 4 binomial standard deviations; a Monte
/// Carlo of the design with Osterwald-Lenum's quantiles gives 95 %), and
/// rank 0 in none.
#[test]
fn johansen_finds_rank_one_in_a_simulated_system() {
    let mut found = [0; 4];
    for rep in 0..200 {
        let mut rng = Rng::stream(71, rep);
        let (a, b) = (walk(&mut rng, 250, 0.2), walk(&mut rng, 250, 0.2));
        let u = ar1(&mut rng, 250, 0.5, 0.0);
        let c: Vec<f64> = a.iter().zip(&b).zip(&u).map(|((a, b), u)| 0.5 * a + 0.5 * b + u).collect();
        let r = run("johansen", &[system(&[("a", &a), ("b", &b), ("c", &c)])], &[]);
        found[r.effect.unwrap() as usize] += 1;
    }
    assert!(found[1] >= 176 && found[0] == 0, "ranks found {:?}", found);
}

// Q-4.6: properties and refusals

/// A test's name, its input tables and its parameters.
type Application = (&'static str, Vec<Table>, Vec<(&'static str, Param)>);

/// One application of each test of the group, its input built from `x`, a
/// stationary AR(1), `w`, a random walk, and `v`, a series cointegrated
/// with `w`.
fn applications(x: &[f64], w: &[f64], v: &[f64]) -> Vec<Application> {
    let pair = system(&[("w", w), ("y", v)]);
    let triple = system(&[("w", w), ("x", x), ("y", v)]);
    vec![
        ("ljung_box", vec![series(x)], vec![]),
        ("variance_ratio", vec![series(w)], vec![("q", int(4))]),
        ("arch_lm", vec![series(x)], vec![("lags", int(3))]),
        ("adf", vec![series(x)], vec![("trend", text("ct"))]),
        ("kpss", vec![series(x)], vec![]),
        ("half_life", vec![series(x)], vec![]),
        ("engle_granger", vec![pair], vec![]),
        ("johansen", vec![triple], vec![("lags", int(2))]),
    ]
}

/// The inputs of the property tests: an AR(1), a random walk, and a second
/// walk cointegrated with the first.
fn property_inputs() -> (Vec<f64>, Vec<f64>, Vec<f64>) {
    let mut rng = Rng::stream(81, 0);
    let x = ar1(&mut rng, 400, 0.7, 2.0);
    let w = walk(&mut rng, 400, 0.1);
    let u = ar1(&mut rng, 400, 0.3, 0.0);
    let v: Vec<f64> = w.iter().zip(&u).map(|(w, u)| 3.0 + 0.5 * w + u).collect();
    (x, w, v)
}

/// Every statistic of the group is scale-free: multiplying each series by
/// its own factor and shifting it leaves the statistic, the p-value, the
/// effect and the interval unchanged, to 1e-9 relative (rounding).
#[test]
fn every_series_statistic_is_scale_free() {
    let (x, w, v) = property_inputs();
    let affine = |s: &[f64], a: f64, b: f64| s.iter().map(|z| a * z + b).collect::<Vec<f64>>();
    let base = applications(&x, &w, &v);
    let (xs, ws, vs) = (affine(&x, 1000.0, 7.0), affine(&w, 0.01, -3.0), affine(&v, 250.0, 11.0));
    let scaled = applications(&xs, &ws, &vs);
    for ((name, tables, ps), (_, scaled_tables, _)) in base.iter().zip(&scaled) {
        let a = run(name, tables, ps);
        let b = run(name, scaled_tables, ps);
        close(b.statistic, a.statistic, 1e-9, &format!("{} statistic", name));
        if let (Some(p), Some(q)) = (a.p_value, b.p_value) {
            close(q, p, 1e-9, &format!("{} p-value", name));
        }
        if let (Some(e), Some(f)) = (a.effect, b.effect) {
            close(f, e, 1e-9, &format!("{} effect", name));
        }
        if let (Some(i), Some(j)) = (a.interval, b.interval) {
            close(j.0, i.0, 1e-9, &format!("{} interval", name));
            close(j.1, i.1, 1e-9, &format!("{} interval", name));
        }
    }
}

/// A result is the same to the bit for the same seed (Q-4.1).
#[test]
fn every_series_test_is_bit_identical_for_a_seed() {
    let (x, w, v) = property_inputs();
    let bits = |r: &TestResult| {
        (
            r.statistic.to_bits(),
            r.p_value.map(f64::to_bits),
            r.effect.map(f64::to_bits),
            r.interval.map(|(a, b, c)| (a.to_bits(), b.to_bits(), c.to_bits())),
        )
    };
    for (name, tables, ps) in applications(&x, &w, &v) {
        let test = get(name).unwrap();
        let a = test.run(&tables, &params(&ps), 99).unwrap();
        let b = test.run(&tables, &params(&ps), 99).unwrap();
        assert_eq!(bits(&a), bits(&b), "{}", name);
        assert_eq!(a.notes, b.notes, "{}", name);
    }
}

/// Unfit input is refused with an error, never a panic: no table, a
/// non-finite value, too few observations, a constant series, an unknown
/// parameter value.
#[test]
fn unfit_input_is_refused() {
    let (x, w, v) = property_inputs();
    for (name, tables, ps) in applications(&x, &w, &v) {
        assert!(matches!(try_run(name, &[], &ps), Err(TestError::Input(_))), "{} without a table", name);
        let mut nan = tables.clone();
        nan[0].columns.values_mut().next().unwrap()[5] = f64::NAN;
        assert!(matches!(try_run(name, &nan, &ps), Err(TestError::Input(_))), "{} with a NaN", name);
        let mut short = tables.clone();
        short[0].columns.values_mut().for_each(|c| c.truncate(3));
        assert!(try_run(name, &short, &ps).is_err(), "{} with three rows", name);
        let mut flat = tables.clone();
        flat[0].columns.values_mut().for_each(|c| c.iter_mut().for_each(|z| *z = 1.5));
        assert!(try_run(name, &flat, &ps).is_err(), "{} on a constant", name);
    }
    assert!(matches!(try_run("adf", &[series(&x)], &[("trend", text("quadratic"))]), Err(TestError::Input(_))));
    assert!(matches!(try_run("adf", &[series(&x)], &[("lags", int(-1))]), Err(TestError::Input(_))));
    assert!(matches!(try_run("variance_ratio", &[series(&w)], &[("q", int(1))]), Err(TestError::Input(_))));
    assert!(matches!(try_run("johansen", &[system(&[("w", &w), ("y", &v)])], &[("rank", int(2))]), Err(TestError::Input(_))));
    assert!(matches!(
        try_run("johansen", &[system(&[("w", &w), ("y", &v)])], &[("statistic", text("both"))]),
        Err(TestError::Input(_))
    ));
    assert!(matches!(try_run("engle_granger", &[series(&w)], &[]), Err(TestError::Input(_))));
    assert!(matches!(try_run("engle_granger", &[system(&[("w", &w), ("y", &v)])], &[("y", text("z"))]), Err(TestError::Input(_))));
    assert!(matches!(try_run("half_life", &[series(&x)], &[("level", Param::Float(1.5))]), Err(TestError::Input(_))));
    assert!(matches!(try_run("ljung_box", &[series(&x)], &[("lags", int(400))]), Err(TestError::Precondition(_))));
}
