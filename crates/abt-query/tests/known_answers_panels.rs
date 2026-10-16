//! G9 for the panel, activation and cost tests of the starting set (Q-4.4,
//! Q-4.6): `ic`, `fama_macbeth`, `coactivation`, `effective_rules` and
//! `cost_realism`, each on its known-answer source at the tolerance E-24
//! states (published values to their printed precision; hand values to the
//! rounding of their arithmetic; simulated answers to a tolerance stated
//! and justified where they are checked), and the properties of Q-4.6:
//! scale invariance where the statistic is scale-free, bit identity for a
//! fixed seed, and, since none of these tests is `iid`, the permutation
//! invariance that applies to each (the order of a panel's rows, of a
//! bar's instances, of the fills within a time).

use std::collections::BTreeMap;
use std::path::PathBuf;

use abt_query::tests::dist;
use abt_query::tests::linalg::Matrix;
use abt_query::tests::rng::Rng;
use abt_query::tests::set::coactivation::{association, Association};
use abt_query::tests::set::effective_rules::effective_number;
use abt_query::tests::{get, names, ClusterBy, Dependence, Param, Params, Table, TestError, TestResult};

fn fixture(name: &str) -> String {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests").join("fixtures").join(name);
    std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("{}: {}", p.display(), e))
}

fn with(pairs: &[(&str, Param)]) -> Params {
    pairs.iter().map(|(k, v)| (k.to_string(), v.clone())).collect()
}

fn run(test: &str, table: &Table, params: &Params, seed: u64) -> Result<TestResult, TestError> {
    get(test).unwrap_or_else(|| panic!("`{}` is registered", test)).run(std::slice::from_ref(table), params, seed)
}

/// A table of the given columns, with the time and entity of each row.
fn table(time: Option<Vec<i64>>, entity: Option<Vec<u64>>, columns: &[(&str, Vec<f64>)]) -> Table {
    Table {
        time,
        entity,
        columns: columns.iter().map(|(n, c)| (n.to_string(), c.clone())).collect::<BTreeMap<_, _>>(),
    }
}

/// The rows of `t` in an order drawn from `seed`.
fn shuffled(t: &Table, seed: u64) -> Table {
    let mut order: Vec<usize> = (0..t.len()).collect();
    Rng::stream(seed, 0).shuffle(&mut order);
    Table {
        time: t.time.as_ref().map(|v| order.iter().map(|&i| v[i]).collect()),
        entity: t.entity.as_ref().map(|v| order.iter().map(|&i| v[i]).collect()),
        columns: t.columns.iter().map(|(n, c)| (n.clone(), order.iter().map(|&i| c[i]).collect())).collect(),
    }
}

/// Two results equal to the bit.
fn identical(a: &TestResult, b: &TestResult) -> bool {
    let bits = |r: &TestResult| {
        (
            r.statistic.to_bits(),
            r.p_value.map(f64::to_bits),
            r.interval.map(|(l, h, c)| (l.to_bits(), h.to_bits(), c.to_bits())),
            r.effect.map(f64::to_bits),
            r.df.map(f64::to_bits),
            r.n,
        )
    };
    bits(a) == bits(b) && a.notes == b.notes
}

fn relative(got: f64, want: f64) -> f64 {
    ((got - want) / want).abs()
}

#[test]
fn the_group_is_registered_with_the_dependence_q_4_4_and_e_21_state() {
    let registered = names();
    for name in ["coactivation", "cost_realism", "effective_rules", "fama_macbeth", "ic"] {
        assert!(registered.contains(&name.to_string()), "{} in {:?}", name, registered);
    }
    let nw = Dependence::NeweyWest { lags: None };
    assert_eq!(get("ic").unwrap().dependence(), nw);
    assert_eq!(get("fama_macbeth").unwrap().dependence(), nw);
    assert_eq!(get("cost_realism").unwrap().dependence(), nw);
    assert_eq!(get("coactivation").unwrap().dependence(), Dependence::Clustered { by: ClusterBy::Time });
    assert_eq!(get("effective_rules").unwrap().dependence(), Dependence::Descriptive);
}

// ---------------------------------------------------------------- ic

/// A panel of `bars` bars of `entities` entities whose feature and forward
/// return are standard bivariate normal with correlation `rho`, independent
/// across entities and bars, drawn from the framework's generator.
fn normal_panel(bars: usize, entities: usize, rho: f64, seed: u64) -> Table {
    let mut rng = Rng::stream(seed, 0);
    let (mut time, mut entity, mut feature, mut forward) = (Vec::new(), Vec::new(), Vec::new(), Vec::new());
    for t in 0..bars {
        for e in 0..entities {
            let f = rng.normal();
            time.push(t as i64);
            entity.push(e as u64);
            feature.push(f);
            forward.push(rho * f + (1.0 - rho * rho).sqrt() * rng.normal());
        }
    }
    table(Some(time), Some(entity), &[("feature", feature), ("forward", forward)])
}

/// Moran's (1948) expectation of Spearman's coefficient over n draws of a
/// bivariate normal with correlation ρ: 6/(π(n + 1)) (asin ρ + (n − 2)
/// asin(ρ/2)), which tends to Pearson's (6/π) asin(ρ/2).
fn moran(rho: f64, n: usize) -> f64 {
    6.0 / (std::f64::consts::PI * (n as f64 + 1.0)) * (rho.asin() + (n as f64 - 2.0) * (rho / 2.0).asin())
}

/// The planted IC: 1,000 bars of 100 entities, feature and forward return
/// bivariate normal with ρ = 0.1, so that every bar's Spearman coefficient
/// has Moran's expectation 0.0946. The bars are independent and a
/// coefficient on n entities has a variance of at most 1/(n − 1) (its
/// variance under independence, which correlation lowers), so the mean IC's
/// standard error is at most 1/√(1000 · 99) = 0.0032; the tolerance is four
/// of those, which an unbiased estimator misses with probability under
/// 10⁻⁴. The Newey–West t rejects, and its standard error agrees with
/// that bound.
#[test]
fn ic_recovers_a_planted_information_coefficient() {
    let (bars, entities, rho) = (1000, 100, 0.1);
    let planted = moran(rho, entities);
    assert!((planted - 0.0946).abs() < 1e-4, "{}", planted);
    let r = run("ic", &normal_panel(bars, entities, rho, 11), &Params::new(), 0).unwrap();
    let bound = 1.0 / ((bars * (entities - 1)) as f64).sqrt();
    let ic = r.effect.unwrap();
    assert!((ic - planted).abs() <= 4.0 * bound, "IC {} against the planted {} (tolerance {})", ic, planted, 4.0 * bound);
    assert_eq!(r.n, bars);
    assert_eq!(r.df, Some(999.0));
    assert!(r.p_value.unwrap() < 1e-12, "{:?}", r);
    let se = ic / r.statistic;
    assert!(se > 0.8 * bound && se < 1.2 * bound, "standard error {} against the bound {}", se, bound);
    let (lo, hi, level) = r.interval.unwrap();
    assert!(lo < ic && ic < hi && level == 0.95, "{:?}", r.interval);
}

/// Under the null the test holds its size: over 1,000 panels of 60 bars
/// of 30 entities without information, the share rejected at 5 % is
/// between a fifth of the nominal level and twice it (Newey–West's
/// standard error runs a little low on 60 bars, for a size near 6 % over
/// 4,000 panels; the binomial standard error over 1,000 is 0.007).
#[test]
fn ic_holds_its_size_without_information() {
    let rejected = (0..1000)
        .filter(|&s| run("ic", &normal_panel(60, 30, 0.0, 1000 + s), &Params::new(), 0).unwrap().p_value.unwrap() < 0.05)
        .count();
    let size = rejected as f64 / 1000.0;
    assert!((0.01..=0.10).contains(&size), "size {}", size);
}

/// Scale (and every increasing transformation) of the feature or the
/// returns leaves the ranks, and so the result, unchanged to the bit; so
/// does the order of the panel's rows; and the test is a function of its
/// input to the bit.
#[test]
fn ic_is_rank_invariant_row_order_free_and_reproducible() {
    let panel = normal_panel(120, 40, 0.2, 3);
    let base = run("ic", &panel, &Params::new(), 0).unwrap();
    let mut transformed = panel.clone();
    transformed.columns.get_mut("feature").unwrap().iter_mut().for_each(|v| *v = 1000.0 * *v - 7.0);
    transformed.columns.get_mut("forward").unwrap().iter_mut().for_each(|v| *v = v.exp());
    assert!(identical(&base, &run("ic", &transformed, &Params::new(), 0).unwrap()));
    assert!(identical(&base, &run("ic", &shuffled(&panel, 5), &Params::new(), 0).unwrap()));
    assert!(identical(&base, &run("ic", &panel, &Params::new(), 0).unwrap()));
    assert!(identical(&base, &run("ic", &panel, &Params::new(), 99).unwrap()), "the seed is unused");
    // A longer horizon raises the lags to cover its overlap.
    let overlapping = run("ic", &panel, &with(&[("horizon", Param::Int(9))]), 0).unwrap();
    assert!(overlapping.notes.iter().any(|n| n.ends_with("with 8 lags")), "{:?}", overlapping.notes);
}

/// A bar's coefficient is Spearman's: on Spearman's (1904) own form of
/// the coefficient, 1 − 6Σd² / (n(n² − 1)) without ties, and with ties
/// at their mean ranks, by hand.
#[test]
fn ic_takes_spearmans_coefficient_at_each_bar() {
    use abt_query::tests::set::ic::spearman;
    // Ranks (1, 2, 3, 4, 5) against (2, 1, 4, 3, 5): Σd² = 4.
    let r = spearman(&[1.0, 2.0, 3.0, 4.0, 5.0], &[20.0, 10.0, 40.0, 30.0, 50.0]).unwrap();
    assert!((r - (1.0 - 6.0 * 4.0 / (5.0 * 24.0))).abs() < 1e-15, "{}", r);
    // Ties: (1, 2.5, 2.5, 4) against (1, 2, 3, 4); centred, (−1.5, 0, 0, 1.5)
    // against (−1.5, −0.5, 0.5, 1.5): 4.5 / √(4.5 · 5).
    let r = spearman(&[1.0, 2.0, 2.0, 3.0], &[1.0, 2.0, 3.0, 4.0]).unwrap();
    assert!((r - 4.5 / (4.5f64 * 5.0).sqrt()).abs() < 1e-15, "{}", r);
    assert!(spearman(&[1.0, 1.0, 1.0], &[1.0, 2.0, 3.0]).is_none());
    // Through the test: two bars of perfect agreement and one of perfect
    // disagreement give a mean IC of 1/3 at every bar weight.
    let x = vec![1.0, 2.0, 3.0, 4.0];
    let mut feature = Vec::new();
    let mut forward = Vec::new();
    for sign in [1.0, 1.0, -1.0] {
        feature.extend(&x);
        forward.extend(x.iter().map(|v| sign * v));
    }
    let panel = table(
        Some(vec![0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 2]),
        Some(vec![0, 1, 2, 3, 0, 1, 2, 3, 0, 1, 2, 3]),
        &[("feature", feature), ("forward", forward)],
    );
    let r = run("ic", &panel, &with(&[("lags", Param::Int(0))]), 0).unwrap();
    assert!((r.effect.unwrap() - 1.0 / 3.0).abs() < 1e-15, "{:?}", r);
    // The IC series (1, 1, −1): s² = 4/3, the t of its mean (1/3)/√(4/9).
    assert!((r.statistic - 0.5).abs() < 1e-14, "{:?}", r);
}

#[test]
fn ic_refuses_what_is_not_a_panel() {
    let mut no_time = normal_panel(5, 5, 0.0, 1);
    no_time.time = None;
    assert!(matches!(run("ic", &no_time, &Params::new(), 0), Err(TestError::Input(_))));
    let mut twice = normal_panel(5, 5, 0.0, 1);
    twice.entity.as_mut().unwrap()[1] = 0;
    assert!(matches!(run("ic", &twice, &Params::new(), 0), Err(TestError::Input(_))));
    assert!(matches!(run("ic", &normal_panel(1, 50, 0.0, 1), &Params::new(), 0), Err(TestError::Precondition(_))));
    assert!(matches!(run("ic", &normal_panel(10, 5, 0.0, 1), &with(&[("lags", Param::Int(-1))]), 0), Err(TestError::Input(_))));
    assert!(matches!(run("ic", &normal_panel(10, 5, 0.0, 1), &with(&[("level", Param::Float(1.5))]), 0), Err(TestError::Input(_))));
}

// ---------------------------------------------------------------- fama_macbeth

/// Petersen's simulated panel: 500 firms over 10 years, `y` on `x`, with a
/// true slope of 1 and firm and time effects in both (Petersen 2009). The
/// data are R's `sandwich` package's copy, `PetersenCL`, as Rdatasets
/// writes it; Petersen's file stores single precision, of which these are
/// the exact values.
fn petersen() -> Table {
    let text = fixture("rdatasets/PetersenCL.csv");
    let mut lines = text.lines();
    assert_eq!(lines.next(), Some("rownames,firm,year,x,y"));
    let (mut firm, mut year, mut x, mut y) = (Vec::new(), Vec::new(), Vec::new(), Vec::new());
    for line in lines {
        let f: Vec<&str> = line.split(',').collect();
        firm.push(f[1].parse().unwrap());
        year.push(f[2].parse().unwrap());
        x.push(f[3].parse().unwrap());
        y.push(f[4].parse().unwrap());
    }
    assert_eq!(x.len(), 5000);
    table(Some(year), Some(firm), &[("x", x), ("y", y)])
}

/// Petersen's published Fama–MacBeth estimates on his panel (his test-data
/// page, https://www.kellogg.northwestern.edu/faculty/petersen/htm/papers/se/test_data.htm,
/// computed by his `fm.ado`, whose standard error is the coefficients'
/// standard deviation over √T): x 1.0356 (0.0333, t 31.06), the intercept
/// 0.0313 (0.0234, t 1.34), printed to four decimals and the t to two.
/// The page is not reachable from the build container; FamaMacBeth.jl's
/// test suite checks the same printed values against it
/// (https://raw.githubusercontent.com/zundertj/FamaMacBeth.jl/master/test/runtests.jl).
/// With zero lags the test's standard error is Fama and MacBeth's; each
/// value is checked to half a unit of its last printed digit.
#[test]
fn fama_macbeth_on_petersens_panel_to_his_printed_digits() {
    let panel = petersen();
    for (factor, premium, se, t) in [("x", 1.0356, 0.0333, 31.06), ("const", 0.0313, 0.0234, 1.34)] {
        let r = run("fama_macbeth", &panel, &with(&[("factor", Param::Text(factor.into())), ("lags", Param::Int(0))]), 0).unwrap();
        let got = r.effect.unwrap();
        assert!((got - premium).abs() <= 0.5e-4, "{} premium {} against {}", factor, got, premium);
        assert!((got / r.statistic - se).abs() <= 0.5e-4, "{} standard error {} against {}", factor, got / r.statistic, se);
        assert!((r.statistic - t).abs() <= 0.5e-2, "{} t {} against {}", factor, r.statistic, t);
        assert_eq!((r.n, r.df), (10, Some(9.0)));
        // Petersen's design plants a slope of 1, which the interval covers.
        if factor == "x" {
            let (lo, hi, _) = r.interval.unwrap();
            assert!(lo < 1.0 && 1.0 < hi, "{:?}", r.interval);
        }
    }
    // The premium does not depend on the lags; its standard error does.
    let nw = run("fama_macbeth", &panel, &Params::new(), 0).unwrap();
    let fm = run("fama_macbeth", &panel, &with(&[("lags", Param::Int(0))]), 0).unwrap();
    assert_eq!(nw.effect.unwrap().to_bits(), fm.effect.unwrap().to_bits());
    assert_ne!(nw.statistic.to_bits(), fm.statistic.to_bits());
    assert!(nw.notes.iter().any(|n| n == "Newey–West lags 2"), "{:?}", nw.notes);
}

/// A planted premium, by hand: at each of 8 bars the returns of 5 entities
/// are exactly a_t + λ_t x, with λ_t = 0.5 + 0.1 (−1)^t, so that every
/// cross-section recovers (a_t, λ_t) and the premium is 0.5. The premium
/// series' deviations ±0.1 alternate: γ_l = 0.01 (−1)^l (8 − l)/8, so
/// with zero lags the standard error is √(8 · 0.01 / (8 · 7)) = 0.1/√7,
/// and with two the long-run variance is 0.01 (1 − 2 · (2/3)(7/8) + 2 ·
/// (1/3)(6/8)) = 1/300 and the standard error √(1/300 / 7). The
/// tolerance, 1e-12 relative, is the rounding of exact fits and of sums
/// over eight terms.
#[test]
fn fama_macbeth_recovers_a_planted_premium_by_hand() {
    let (mut time, mut entity, mut x, mut y) = (Vec::new(), Vec::new(), Vec::new(), Vec::new());
    for t in 0..8i64 {
        let lambda = 0.5 + 0.1 * if t % 2 == 0 { 1.0 } else { -1.0 };
        let a = 0.2 - 0.05 * t as f64;
        for e in 0..5u64 {
            let xe = e as f64 + 0.25 * t as f64 + if e == 3 { 0.5 } else { 0.0 };
            time.push(t);
            entity.push(e);
            x.push(xe);
            y.push(a + lambda * xe);
        }
    }
    let panel = table(Some(time), Some(entity), &[("x", x), ("y", y)]);
    let r0 = run("fama_macbeth", &panel, &with(&[("lags", Param::Int(0))]), 0).unwrap();
    assert!(relative(r0.effect.unwrap(), 0.5) < 1e-12, "{:?}", r0);
    assert!(relative(r0.effect.unwrap() / r0.statistic, 0.1 / 7f64.sqrt()) < 1e-12, "{:?}", r0);
    let r2 = run("fama_macbeth", &panel, &with(&[("lags", Param::Int(2))]), 0).unwrap();
    assert!(relative(r2.effect.unwrap() / r2.statistic, (1.0 / 300.0 / 7.0f64).sqrt()) < 1e-12, "{:?}", r2);
    // The intercept's premium is the mean of a_t: 0.2 − 0.05 · 3.5.
    let c = run("fama_macbeth", &panel, &with(&[("lags", Param::Int(0)), ("factor", Param::Text("const".into()))]), 0).unwrap();
    assert!(relative(c.effect.unwrap(), 0.2 - 0.05 * 3.5) < 1e-12, "{:?}", c);
}

/// The t of a premium does not depend on the units of the returns or the
/// characteristic, nor on the order of the panel's rows (to the bit, since
/// every bar's rows are put in entity order); and the test is a function of
/// its input to the bit.
#[test]
fn fama_macbeth_is_scale_free_row_order_free_and_reproducible() {
    let panel = petersen();
    let base = run("fama_macbeth", &panel, &Params::new(), 0).unwrap();
    let mut scaled = panel.clone();
    scaled.columns.get_mut("y").unwrap().iter_mut().for_each(|v| *v *= 100.0);
    scaled.columns.get_mut("x").unwrap().iter_mut().for_each(|v| *v *= 0.01);
    let s = run("fama_macbeth", &scaled, &Params::new(), 0).unwrap();
    assert!(relative(s.statistic, base.statistic) < 1e-12, "{} against {}", s.statistic, base.statistic);
    assert!(relative(s.effect.unwrap(), 1e4 * base.effect.unwrap()) < 1e-12);
    assert!(identical(&base, &run("fama_macbeth", &shuffled(&panel, 8), &Params::new(), 0).unwrap()));
    assert!(identical(&base, &run("fama_macbeth", &panel, &Params::new(), 7).unwrap()));
}

#[test]
fn fama_macbeth_refuses_an_ambiguous_or_short_panel() {
    let mut two = petersen();
    let z: Vec<f64> = two.columns["x"].iter().map(|v| v * v).collect();
    two.columns.insert("z".into(), z);
    assert!(matches!(run("fama_macbeth", &two, &Params::new(), 0), Err(TestError::Input(_))));
    assert!(run("fama_macbeth", &two, &with(&[("factor", Param::Text("z".into()))]), 0).is_ok());
    assert!(matches!(run("fama_macbeth", &two, &with(&[("factor", Param::Text("w".into()))]), 0), Err(TestError::Input(_))));
    let one_bar = table(Some(vec![0; 4]), Some(vec![0, 1, 2, 3]), &[("x", vec![1.0, 2.0, 3.0, 4.0]), ("y", vec![1.0, 3.0, 2.0, 4.0])]);
    assert!(matches!(run("fama_macbeth", &one_bar, &Params::new(), 0), Err(TestError::Precondition(_))));
}

// ---------------------------------------------------------------- coactivation

/// An activation table with the given 2×2 counts (n11, n10, n01, n00) at
/// each bar, the rules named `a` and `b`.
fn activations(bars: &[[u64; 4]]) -> Table {
    let (mut time, mut a, mut b) = (Vec::new(), Vec::new(), Vec::new());
    for (t, counts) in bars.iter().enumerate() {
        for (cell, &count) in counts.iter().enumerate() {
            for _ in 0..count {
                time.push(t as i64);
                a.push(if cell < 2 { 1.0 } else { 0.0 });
                b.push(if cell % 2 == 0 { 1.0 } else { 0.0 });
            }
        }
    }
    table(Some(time), None, &[("a", a), ("b", b)])
}

/// Hand tables, to the rounding of their arithmetic (1e-15):
/// - a perfect association, (25, 0, 0, 75): phi 1, Jaccard 1, and the
///   mutual information the entropy of either rule, 2 − (3/4) log₂ 3 bits;
/// - a perfect opposition, (0, 40, 60, 0): phi −1, Jaccard 0, the
///   entropy H(0.4) bits;
/// - independence, (10, 20, 30, 60), whose cross products are equal: phi 0
///   and the information 0 exactly, Jaccard 10/60;
/// - (30, 10, 20, 40): phi (1200 − 200)/√(40 · 60 · 50 · 50) = 1/√6,
///   Jaccard 30/60, and the information 0.3 log₂(0.3/0.2) + 0.1 log₂(0.1/0.2)
///   + 0.2 log₂(0.2/0.3) + 0.4 log₂(0.4/0.3) = 0.6 − 0.3 log₂ 3 bits.
#[test]
fn coactivation_measures_hand_tables() {
    let close = |a: Association, phi: f64, jaccard: f64, mi: f64| {
        assert!((a.phi - phi).abs() < 1e-15, "phi {} against {}", a.phi, phi);
        assert!((a.jaccard - jaccard).abs() < 1e-15, "Jaccard {} against {}", a.jaccard, jaccard);
        assert!((a.mutual_information - mi).abs() < 1e-15, "information {} against {}", a.mutual_information, mi);
    };
    let log3 = 3f64.log2();
    close(association([25, 0, 0, 75]).unwrap(), 1.0, 1.0, 2.0 - 0.75 * log3);
    let h4 = -(0.4f64 * 0.4f64.log2() + 0.6 * 0.6f64.log2());
    close(association([0, 40, 60, 0]).unwrap(), -1.0, 0.0, h4);
    let independent = association([10, 20, 30, 60]).unwrap();
    assert_eq!((independent.phi, independent.mutual_information), (0.0, 0.0));
    close(independent, 0.0, 1.0 / 6.0, 0.0);
    let a = association([30, 10, 20, 40]).unwrap();
    close(a, 1.0 / 6f64.sqrt(), 0.5, 0.6 - 0.3 * log3);
    assert_eq!((a.second_given_first, a.first_given_second), (0.75, 0.6));
    // A rule that never fires, or always does, has no association.
    assert!(matches!(association([0, 0, 5, 5]), Err(TestError::Precondition(_))));
    assert!(matches!(association([5, 0, 5, 0]), Err(TestError::Precondition(_))));
    // Through the test, the statistic and the effect are phi.
    let r = run("coactivation", &activations(&[[30, 10, 20, 40]]), &Params::new(), 1).unwrap();
    assert!((r.statistic - 1.0 / 6f64.sqrt()).abs() < 1e-15 && r.effect == Some(r.statistic) && r.n == 100, "{:?}", r);
    assert!(r.interval.is_none(), "one bar has no interval over bars");
}

/// On one bar the within-bar permutation is the permutation test of
/// independence, whose exact distribution is Fisher's (1935): given the
/// margins, n11 is hypergeometric. For (12, 8, 8, 12), X ~ H(40, 20, 20)
/// and P(X ≥ 12) = Σ_{k ≥ 12} C(20, k)² / C(40, 20) = 0.1715, the lower
/// tail the larger, so the two-sided p-value is 2 · 0.1715. The test's
/// p-value from 50,000 permutations has a Monte Carlo standard error of
/// 2 √(0.1715 · 0.8285 / 50,000) = 0.0034; the tolerance is four of those.
#[test]
fn coactivation_on_one_bar_is_fishers_exact_test() {
    let binomial = |n: u64, k: u64| (0..k).fold(1.0f64, |c, i| c * (n - i) as f64 / (i + 1) as f64);
    let upper: f64 = (12..=20).map(|k| binomial(20, k) * binomial(20, 20 - k)).sum::<f64>() / binomial(40, 20);
    assert!((upper - 23_645_313_042.0 / 137_846_528_820.0).abs() < 1e-15, "{}", upper);
    let exact = 2.0 * upper;
    let reps = 50_000;
    let r = run("coactivation", &activations(&[[12, 8, 8, 12]]), &with(&[("reps", Param::Int(reps))]), 3).unwrap();
    let mc = 2.0 * (upper * (1.0 - upper) / reps as f64).sqrt();
    assert!(
        (r.p_value.unwrap() - exact).abs() <= 4.0 * mc,
        "p {} against Fisher's {} (tolerance {})",
        r.p_value.unwrap(),
        exact,
        4.0 * mc
    );
}

/// The p-value is clustered by time: two rules that fire independently
/// within every bar, both often on busy bars (each with probability 0.6)
/// and seldom on quiet ones (0.1), show a pooled phi near 0.275. Permuted
/// within bars, the test holds its size over 100 such runs (at most 12 %
/// rejected at 5 %, three binomial standard errors above); pooled into one
/// bar, as an iid test would, it rejects every one.
#[test]
fn coactivation_is_clustered_by_time() {
    let draw = |seed: u64, pooled: bool| {
        let mut rng = Rng::stream(seed, 0);
        let (mut time, mut a, mut b) = (Vec::new(), Vec::new(), Vec::new());
        for t in 0..60i64 {
            let p = if t % 2 == 0 { 0.6 } else { 0.1 };
            for _ in 0..30 {
                time.push(if pooled { 0 } else { t });
                a.push(f64::from(u8::from(rng.uniform() < p)));
                b.push(f64::from(u8::from(rng.uniform() < p)));
            }
        }
        table(Some(time), None, &[("a", a), ("b", b)])
    };
    let params = with(&[("reps", Param::Int(499))]);
    let (mut within, mut pooled, mut phi) = (0, 0, 0.0);
    for seed in 0..100 {
        let r = run("coactivation", &draw(seed, false), &params, seed).unwrap();
        within += usize::from(r.p_value.unwrap() < 0.05);
        phi += r.statistic / 100.0;
        pooled += usize::from(run("coactivation", &draw(seed, true), &params, seed).unwrap().p_value.unwrap() < 0.05);
    }
    assert!((phi - 0.275).abs() < 0.03, "pooled phi {}", phi);
    assert!(within <= 12, "{} of 100 rejected within bars", within);
    assert_eq!(pooled, 100);
}

/// The p-value and interval are a function of the seed, to the bit, and
/// change with it; they do not depend on the order of the instances, nor
/// on which rule is permuted (the within-bar distribution is symmetric in
/// the two); and from ten bars phi carries its bootstrap interval.
#[test]
fn coactivation_is_reproducible_and_symmetric() {
    let bars: Vec<[u64; 4]> = (0..24).map(|t| [5 + t % 3, 4, 3 + t % 2, 10]).collect();
    let t = activations(&bars);
    let a = run("coactivation", &t, &Params::new(), 17).unwrap();
    assert!(a.p_value.unwrap() < 0.01, "{:?}", a);
    assert!(identical(&a, &run("coactivation", &t, &Params::new(), 17).unwrap()));
    assert!(identical(&a, &run("coactivation", &shuffled(&t, 2), &Params::new(), 17).unwrap()));
    let swapped = run("coactivation", &t, &with(&[("rules", Param::Text("(b, a)".into()))]), 17).unwrap();
    assert_eq!(swapped.p_value.unwrap().to_bits(), a.p_value.unwrap().to_bits());
    assert!((swapped.statistic - a.statistic).abs() < 1e-15);
    let (lo, hi, level) = a.interval.unwrap();
    assert!(lo < a.statistic && a.statistic < hi && level == 0.95, "{:?}", a);
    // A weak association, whose p-value is far from its floor of
    // 2/(reps + 1), moves with the seed.
    let weak = activations(&vec![[4, 3, 3, 3]; 12]);
    let p: Vec<u64> = (0..5).map(|seed| run("coactivation", &weak, &Params::new(), seed).unwrap().p_value.unwrap().to_bits()).collect();
    assert!(p.windows(2).any(|w| w[0] != w[1]), "{:?}", p);
}

#[test]
fn coactivation_refuses_what_is_not_a_firing() {
    let mut t = activations(&[[3, 3, 3, 3]]);
    t.columns.get_mut("a").unwrap()[0] = 0.5;
    assert!(matches!(run("coactivation", &t, &Params::new(), 0), Err(TestError::Input(_))));
    let mut t = activations(&[[3, 3, 3, 3]]);
    t.time = None;
    assert!(matches!(run("coactivation", &t, &Params::new(), 0), Err(TestError::Input(_))));
    let mut t = activations(&[[3, 3, 3, 3]]);
    t.columns.insert("c".into(), vec![1.0; 12]);
    assert!(matches!(run("coactivation", &t, &Params::new(), 0), Err(TestError::Input(_))));
    assert!(matches!(run("coactivation", &t, &with(&[("rules", Param::Text("a,c".into()))]), 0), Err(TestError::Precondition(_))));
    // A missing value is an instance not attempted by both, and leaves.
    let mut t = activations(&[[3, 3, 3, 3]]);
    t.columns.get_mut("b").unwrap()[0] = f64::NAN;
    assert_eq!(run("coactivation", &t, &Params::new(), 0).unwrap().n, 11);
}

// ---------------------------------------------------------------- effective_rules

/// Hand matrices, to 1e-12: the identity of order n gives n; the all-ones
/// matrix (rules that always fire together) gives 1; the pair with
/// correlation r has eigenvalues 1 ± r, so 4 / (2 + 2r²); four rules
/// equicorrelated at 0.5 have eigenvalues 2.5 and 0.5 (three times), so
/// 16/7; two blocks of rules firing together, of two and three, have
/// eigenvalues 3, 2 and zeros, so 25/13.
#[test]
fn effective_rules_on_hand_matrices() {
    let of = |rows: &[&[f64]]| {
        let n = rows.len();
        let mut m = Matrix::zeros(n, n);
        for (i, row) in rows.iter().enumerate() {
            for (j, v) in row.iter().enumerate() {
                m[(i, j)] = *v;
            }
        }
        effective_number(&m).unwrap().0
    };
    let close = |got: f64, want: f64| assert!(relative(got, want) < 1e-12, "{} against {}", got, want);
    close(effective_number(&Matrix::identity(5)).unwrap().0, 5.0);
    close(of(&[&[1.0; 4], &[1.0; 4], &[1.0; 4], &[1.0; 4]]), 1.0);
    close(of(&[&[1.0, 0.5], &[0.5, 1.0]]), 4.0 / 2.5);
    close(of(&[&[1.0, 0.5, 0.5, 0.5], &[0.5, 1.0, 0.5, 0.5], &[0.5, 0.5, 1.0, 0.5], &[0.5, 0.5, 0.5, 1.0]]), 16.0 / 7.0);
    let blocks: [&[f64]; 5] = [
        &[1.0, 1.0, 0.0, 0.0, 0.0],
        &[1.0, 1.0, 0.0, 0.0, 0.0],
        &[0.0, 0.0, 1.0, 1.0, 1.0],
        &[0.0, 0.0, 1.0, 1.0, 1.0],
        &[0.0, 0.0, 1.0, 1.0, 1.0],
    ];
    close(of(&blocks), 25.0 / 13.0);
    assert!(matches!(effective_number(&Matrix::zeros(2, 3)), Err(TestError::Input(_))));
}

/// Sylvester's Hadamard matrix of order 8.
fn hadamard8() -> Vec<Vec<f64>> {
    let mut h = vec![vec![1.0]];
    while h.len() < 8 {
        let n = h.len();
        h = (0..2 * n).map(|i| (0..2 * n).map(|j| h[i % n][j % n] * if i >= n && j >= n { -1.0 } else { 1.0 }).collect()).collect();
    }
    h
}

/// From activations: seven rules firing on the patterns of a Hadamard
/// matrix's columns (each on half of eight instances, every pair on a
/// quarter) are uncorrelated, so 7; a copy of a rule adds nothing to two
/// independent ones, which with the copy give eigenvalues 2, 1, 0 and so
/// 9/5; a rule that never varies is left out.
#[test]
fn effective_rules_from_activations() {
    let h = hadamard8();
    let fire = |j: usize| h.iter().map(|row| (row[j] + 1.0) / 2.0).collect::<Vec<f64>>();
    let columns: Vec<(String, Vec<f64>)> = (1..8).map(|j| (format!("r{}", j), fire(j))).collect();
    let t = Table {
        time: None,
        entity: None,
        columns: columns.into_iter().collect(),
    };
    let r = run("effective_rules", &t, &Params::new(), 0).unwrap();
    assert!(relative(r.statistic, 7.0) < 1e-12 && r.p_value.is_none() && r.n == 8, "{:?}", r);
    let copy = table(None, None, &[("a", fire(1)), ("b", fire(2)), ("c", fire(1)), ("never", vec![0.0; 8])]);
    let r = run("effective_rules", &copy, &Params::new(), 0).unwrap();
    assert!(relative(r.statistic, 9.0 / 5.0) < 1e-12, "{:?}", r);
    assert!(r.notes.iter().any(|n| n.contains("never")), "{:?}", r.notes);
    let only = run("effective_rules", &copy, &with(&[("rules", Param::Text("a,c".into()))]), 0).unwrap();
    assert!(relative(only.statistic, 1.0) < 1e-12, "{:?}", only);
    assert!(matches!(
        run("effective_rules", &copy, &with(&[("rules", Param::Text("never".into()))]), 0),
        Err(TestError::Precondition(_))
    ));
}

/// The effective number does not depend on the units or origin of any
/// rule's activation, nor on the order of the rows (both to the rounding
/// of the correlations); and it is a function of its input to the bit.
#[test]
fn effective_rules_is_scale_free_and_reproducible() {
    let mut rng = Rng::stream(21, 0);
    let base: Vec<Vec<f64>> = (0..4).map(|_| (0..200).map(|_| f64::from(u8::from(rng.uniform() < 0.3))).collect()).collect();
    let mut columns: Vec<(&str, Vec<f64>)> = vec![("a", base[0].clone()), ("b", base[1].clone()), ("c", base[2].clone())];
    columns.push(("d", base[0].iter().zip(&base[3]).map(|(x, y)| f64::from(u8::from(*x == 1.0 || *y == 1.0))).collect()));
    let t = table(None, None, &columns);
    let r = run("effective_rules", &t, &Params::new(), 0).unwrap();
    assert!(r.statistic > 1.0 && r.statistic < 4.0, "{:?}", r);
    let mut affine = t.clone();
    affine.columns.get_mut("b").unwrap().iter_mut().for_each(|v| *v = -250.0 * *v + 3.0);
    affine.columns.get_mut("d").unwrap().iter_mut().for_each(|v| *v *= 0.001);
    assert!(relative(run("effective_rules", &affine, &Params::new(), 0).unwrap().statistic, r.statistic) < 1e-12);
    assert!(relative(run("effective_rules", &shuffled(&t, 4), &Params::new(), 0).unwrap().statistic, r.statistic) < 1e-12);
    assert!(identical(&r, &run("effective_rules", &t, &Params::new(), 9).unwrap()));
}

// ---------------------------------------------------------------- cost_realism

/// Synthetic fills: the model's slippage 5 bps plus noise, the realised
/// slippage the model's plus `bias` plus an AR(1) error (coefficient 0.3,
/// innovations of 2 bps, started from its stationary law), the fills in
/// time order.
fn fills(n: usize, bias: f64, seed: u64) -> Table {
    let mut rng = Rng::stream(seed, 0);
    let mut e = 2.0 / (1.0f64 - 0.09).sqrt() * rng.normal();
    let (mut realised, mut modelled) = (Vec::with_capacity(n), Vec::with_capacity(n));
    for _ in 0..n {
        let m = 5.0 + rng.normal().abs();
        e = 0.3 * e + 2.0 * rng.normal();
        modelled.push(m);
        realised.push(m + bias + e);
    }
    table(Some((0..n as i64).collect()), None, &[("realised", realised), ("modelled", modelled)])
}

/// A planted bias of 0.5 bps over 2,000 fills, and none, on the same
/// fills. The error's long-run standard deviation is 2/(1 − 0.3) = 2.857
/// bps, so the mean difference has a standard error of 2.857/√2000 = 0.064
/// bps; the tolerance on the recovered bias is four of those. The planted
/// bias is rejected, and the Newey–West standard error agrees with the
/// long-run one; the two estimates differ by the planted bias to the
/// rounding of their sums. (How often the test rejects without a bias is
/// its size, checked over many runs below.)
#[test]
fn cost_realism_recovers_a_planted_bias_and_its_absence() {
    let se = 2.0 / 0.7 / 2000f64.sqrt();
    let mut effects = Vec::new();
    for bias in [0.5, 0.0] {
        let r = run("cost_realism", &fills(2000, bias, 31), &Params::new(), 0).unwrap();
        let got = r.effect.unwrap();
        assert!((got - bias).abs() <= 4.0 * se, "bias {} against the planted {} (tolerance {})", got, bias, 4.0 * se);
        if bias > 0.0 {
            assert!(r.p_value.unwrap() < 1e-6, "{:?}", r);
            let nw = got / r.statistic;
            assert!(nw > 0.75 * se && nw < 1.25 * se, "standard error {} against {}", nw, se);
        }
        assert_eq!((r.n, r.df), (2000, Some(1999.0)));
        effects.push(got);
    }
    assert!((effects[0] - effects[1] - 0.5).abs() < 1e-12, "{:?}", effects);
}

/// The dependence treatment matters: over 1,000 runs of 500 fills without
/// bias, the Newey–West test rejects at 5 % in at most 10 % of them (its
/// size is near 7 % over 30,000 runs, the Bartlett kernel's known excess,
/// and the binomial standard error over 1,000 is 0.008), while the
/// textbook paired t (zero lags), whose variance ignores the AR(1) error's
/// (1 + 0.3)/(1 − 0.3), rejects in about 15 %; and over 100 runs with a
/// bias of 0.5 bps the Newey–West test rejects in at least 80 %.
#[test]
fn cost_realism_holds_its_size_under_dependent_fills() {
    let reject = |bias: f64, seed: u64, params: &Params| run("cost_realism", &fills(500, bias, seed), params, 0).unwrap().p_value.unwrap() < 0.05;
    let (nw, naive) = (Params::new(), with(&[("lags", Param::Int(0))]));
    let size = (0..1000).filter(|&s| reject(0.0, 500 + s, &nw)).count() as f64 / 1000.0;
    let naive_size = (0..1000).filter(|&s| reject(0.0, 500 + s, &naive)).count() as f64 / 1000.0;
    assert!(size <= 0.10, "Newey–West size {}", size);
    assert!(naive_size > 0.10 && naive_size > size, "textbook size {} against {}", naive_size, size);
    let power = (0..100).filter(|&s| reject(0.5, 2000 + s, &nw)).count() as f64 / 100.0;
    assert!(power >= 0.8, "power {}", power);
}

/// With zero lags the statistic is the textbook paired t. On Cushny and
/// Peebles's sleep data as Student (1908) tabulates them (hours gained on
/// two hypnotics by ten patients), the mean difference is 1.58 with
/// Σd² about the mean 13.616, which Student prints as the mean and the
/// standard deviation 1.17 (divisor n); by hand, t = 1.58 / √(13.616/90)
/// = 4.0621 on 9 degrees of freedom. Checked to 1e-12 relative.
#[test]
fn cost_realism_with_zero_lags_is_the_paired_t() {
    let first = vec![0.7, -1.6, -0.2, -1.2, -0.1, 3.4, 3.7, 0.8, 0.0, 2.0];
    let second = vec![1.9, 0.8, 1.1, 0.1, -0.1, 4.4, 5.5, 1.6, 4.6, 3.4];
    let t = table(None, None, &[("realised", second), ("modelled", first)]);
    let r = run("cost_realism", &t, &with(&[("lags", Param::Int(0))]), 0).unwrap();
    assert!(relative(r.effect.unwrap(), 1.58) < 1e-12, "{:?}", r);
    assert!(relative(r.statistic, 1.58 / (13.616f64 / 90.0).sqrt()) < 1e-12, "{:?}", r);
    assert!((r.statistic - 4.0621).abs() < 0.5e-4);
    assert_eq!(r.df, Some(9.0));
    assert!(relative(r.p_value.unwrap(), dist::t_two_sided(r.statistic, 9.0)) < 1e-15);
}

/// The t does not depend on the unit of the slippage (bps or a fraction);
/// exchanging the realised and the modelled negates it exactly; the order
/// of the rows does not matter where the fills carry their time; and the
/// test is a function of its input to the bit.
#[test]
fn cost_realism_is_scale_free_signed_and_reproducible() {
    let t = fills(300, 0.2, 41);
    let base = run("cost_realism", &t, &Params::new(), 0).unwrap();
    let mut fraction = t.clone();
    fraction.columns.values_mut().for_each(|c| c.iter_mut().for_each(|v| *v *= 1e-4));
    assert!(relative(run("cost_realism", &fraction, &Params::new(), 0).unwrap().statistic, base.statistic) < 1e-12);
    let mut swapped = t.clone();
    let realised = swapped.columns.remove("realised").unwrap();
    let modelled = swapped.columns.insert("modelled".into(), realised).unwrap();
    swapped.columns.insert("realised".into(), modelled);
    assert_eq!(run("cost_realism", &swapped, &Params::new(), 0).unwrap().statistic.to_bits(), (-base.statistic).to_bits());
    assert!(identical(&base, &run("cost_realism", &shuffled(&t, 6), &Params::new(), 0).unwrap()));
    assert!(identical(&base, &run("cost_realism", &t, &Params::new(), 3).unwrap()));
    // A model that prices every fill exactly is accepted, not undecidable.
    let exact = table(None, None, &[("realised", vec![1.0, 2.0, 3.0]), ("modelled", vec![1.0, 2.0, 3.0])]);
    let r = run("cost_realism", &exact, &Params::new(), 0).unwrap();
    assert_eq!((r.statistic, r.p_value), (0.0, Some(1.0)));
    let one = table(None, None, &[("realised", vec![1.0]), ("modelled", vec![1.0])]);
    assert!(matches!(run("cost_realism", &one, &Params::new(), 0), Err(TestError::Precondition(_))));
}
