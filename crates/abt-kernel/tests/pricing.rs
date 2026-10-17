//! The pricing functions (src/functions) against known answers:
//! 1. the worked examples of Haug (2007), to their printed four decimals;
//! 2. QuantLib 1.44 as a full-precision oracle: European prices and Greeks
//!    (AnalyticEuropeanEngine) and American prices (QdFpAmericanEngine,
//!    high precision) on deterministic grids;
//! 3. the documented example of TF Quant Finance's Bjerksund–Stensland 2002;
//! 4. mpmath at 40 digits: the normal distribution and its quantile in the
//!    tails, the bivariate normal, the BS2002 formula itself, and exact
//!    European prices for the implied volatility;
//! 5. implied volatility round trips on dense grids, bounds, and a
//!    cross-check against py_lets_be_rational;
//! 6. properties: put–call parity, the American call without dividends, the
//!    put–call transformation, Greeks against differences, edge cases.
//!
//! The fixtures under tests/fixtures/pricing/ are written by the scripts
//! there (gen_quantlib.py, gen_mpmath.py, gen_lbr.py), each run as
//! `python3 -I script.py /path/to/the/extracted/package`.

use abt_kernel::functions::{american, b76, bsm, iv, normal, years, Greeks, Right, Style, YEAR_NANOS};

const CALL: Right = Right::Call;
const PUT: Right = Right::Put;

/// A fixture's rows: the header skipped, fields split at commas.
fn rows(text: &str) -> Vec<Vec<&str>> {
    text.lines().skip(1).filter(|l| !l.is_empty()).map(|l| l.split(',').collect()).collect()
}

fn num(field: &str) -> f64 {
    field.parse().unwrap_or_else(|_| panic!("not a number: {field}"))
}

fn right(field: &str) -> Right {
    Right::parse(field).unwrap_or_else(|| panic!("not a right: {field}"))
}

/// |a − b| ≤ max(rel·|b|, abs).
fn close(a: f64, b: f64, rel: f64, abs: f64) -> bool {
    (a - b).abs() <= (rel * b.abs()).max(abs)
}

// ------------------------------------------------------------------ 1. Haug

/// E. G. Haug, The Complete Guide to Option Pricing Formulas, 2nd ed. (2007):
/// the worked examples, to their printed four decimals (5e-5). The values
/// were recalled, not read from the book, and each was checked against
/// QuantLib 1.44's BlackCalculator (which takes σ√T and the discount factor,
/// so T need not be a whole number of days); this module agrees with it to
/// 1e-10 on all of them. All the recalled values agree with QuantLib but one:
/// the recalled vega 18.9358 (S = 55, K = 60, T = 0.75, r = 0.105,
/// q = 0.0355, σ = 0.30) cannot be right, since vega = S·e^{−qT}·√T·n(d₁) ≤
/// S·e^{−qT}·√T·n(0) = 18.5028 for any d₁; QuantLib and this module give
/// 18.5027440891 for those inputs, and the test checks 18.5027. (The inputs
/// rather than the value may be what was misremembered.)
#[test]
fn haug_worked_examples() {
    let cases: [(&str, f64, f64); 13] = [
        ("generalized BSM call", bsm::price(60.0, 65.0, 0.25, 0.08, 0.0, 0.30, CALL), 2.1334),
        ("Merton put", bsm::price(100.0, 95.0, 0.5, 0.10, 0.05, 0.20, PUT), 2.4648),
        ("Black-76 call", b76::price(19.0, 19.0, 0.75, 0.10, 0.28, CALL), 1.7011),
        ("Black-76 put", b76::price(19.0, 19.0, 0.75, 0.10, 0.28, PUT), 1.7011),
        ("Garman-Kohlhagen call", bsm::price(1.56, 1.60, 0.5, 0.06, 0.08, 0.12, CALL), 0.0291),
        ("delta call", bsm::delta(105.0, 100.0, 0.5, 0.10, 0.10, 0.36, CALL), 0.5946),
        ("delta put", bsm::delta(105.0, 100.0, 0.5, 0.10, 0.10, 0.36, PUT), -0.3566),
        ("gamma", bsm::gamma(55.0, 60.0, 0.75, 0.10, 0.0, 0.30, CALL), 0.0278),
        ("vega (QuantLib's value; see the doc comment)", bsm::vega(55.0, 60.0, 0.75, 0.105, 0.0355, 0.30, CALL), 18.5027),
        ("theta put, per year", bsm::theta(430.0, 405.0, 0.0833, 0.07, 0.05, 0.20, PUT), -31.1924),
        ("rho call", bsm::rho(72.0, 75.0, 1.0, 0.09, 0.0, 0.19, CALL), 38.7325),
        ("Hull call", bsm::price(42.0, 40.0, 0.5, 0.10, 0.0, 0.20, CALL), 4.7594),
        ("Hull put", bsm::price(42.0, 40.0, 0.5, 0.10, 0.0, 0.20, PUT), 0.8086),
    ];
    for (name, got, printed) in cases {
        assert!((got - printed).abs() < 5e-5, "{name}: {got} against {printed}");
    }
    // The recalled vega is out of reach: the largest vega over all d₁.
    let max_vega = 55.0 * (-0.0355f64 * 0.75).exp() * 0.75f64.sqrt() * normal::pdf(0.0);
    assert!(max_vega < 18.9358 - 0.4, "{max_vega}");
}

// ------------------------------------------------------------------ 2. QuantLib

/// European prices and Greeks against QuantLib 1.44's AnalyticEuropeanEngine
/// (quantlib.csv: BSM on S from 60 to 140 against K = 100, τ = N/365 for N in
/// {1, 7, 30, 91, 182, 365, 730, 1825}, r in {0, 0.03, 0.08}, q in
/// {0, 0.02, 0.08}, σ in {0.05, 0.2, 0.5, 1}, both rights, thinned to 471;
/// and 247 Black-76 cases, the engine with S = F and q = r, Black's rho being
/// its rho() + dividendRho()). Each value within 1e-12 relative, or 1e-13
/// absolute (a few ulps of K) for tiny values. Theta alone has a further
/// allowance of 4ε·(|V| + S·|Δ|)/τ, a stated loosening: QuantLib forms theta as
/// −(ln(D)·V + ln(F/S)·S·Δ + ½σ²τ·S²Γ)/τ, and ln of a discount factor near 1
/// carries an absolute rounding of ~ε, so its error grows as 1/τ (for the
/// 1-day put S = 75, σ = 1 it is 3.9e-12 off the mpmath value, where this
/// module is 5e-16 off). Observed: price, delta and gamma within the floor,
/// vega 2.5e-14, rho 5.6e-15, theta 2.6e-12 relative (within its allowance).
#[test]
fn european_against_quantlib() {
    let table = include_str!("fixtures/pricing/quantlib.csv");
    let mut n = [0usize; 2];
    for row in rows(table) {
        let w = right(row[1]);
        let v: Vec<f64> = row[2..].iter().map(|x| num(x)).collect();
        let (s, k, tau, r, q, sigma) = (v[0], v[1], v[2] / 365.0, v[3], v[4], v[5]);
        let (price, g) = match row[0] {
            "bsm" => {
                n[0] += 1;
                (bsm::price(s, k, tau, r, q, sigma, w), bsm::greeks(s, k, tau, r, q, sigma, w))
            }
            "b76" => {
                n[1] += 1;
                assert_eq!(q, r);
                (b76::price(s, k, tau, r, sigma, w), b76::greeks(s, k, tau, r, sigma, w))
            }
            other => panic!("model {other}"),
        };
        let theta_allowance = 4.0 * f64::EPSILON * (v[6].abs() + s * v[7].abs()) / tau;
        let checks = [
            ("price", price, v[6], 0.0),
            ("delta", g.delta, v[7], 0.0),
            ("gamma", g.gamma, v[8], 0.0),
            ("vega", g.vega, v[9], 0.0),
            ("theta", g.theta, v[10], theta_allowance),
            ("rho", g.rho, v[11], 0.0),
        ];
        for (name, got, want, extra) in checks {
            assert!((got - want).abs() <= (1e-12 * want.abs()).max(1e-13) + extra, "{name} {row:?}: {got:e} against QuantLib {want:e}");
        }
    }
    assert_eq!(n, [471, 247]);
}

/// The relative tolerance of BS2002 below QdFp, by σ√T (see the test).
fn bs2002_tolerance(sigma_sqrt_t: f64) -> f64 {
    match sigma_sqrt_t {
        x if x <= 0.75 => 0.015,
        x if x <= 1.0 => 0.025,
        x if x <= 1.5 => 0.04,
        _ => 0.07,
    }
}

/// Bjerksund–Stensland (2002) against QuantLib's QdFpAmericanEngine with its
/// high-precision scheme (american.csv: 1764 calls with q > 0 and puts, S
/// from 70 to 130 against K = 100, τ from 7 days to 5 years, r from 0 to
/// 0.08, q from 0 to 0.1, σ from 0.1 to 1).
///
/// BS2002 is the value of a feasible exercise strategy, hence a lower bound:
/// it must not exceed QdFp by more than 1e-6 (observed maximum excess
/// 8.5e-14). From below, the approximation's error grows with σ√T; the
/// stated tolerance is 1.5% of the QdFp value for σ√T ≤ 0.75, 2.5% to 1, 4%
/// to 1.5 and 7% beyond, plus 1e-13 (QdFp's own absolute error on prices
/// near 1e-14 is about 7e-15). Observed maxima of (QdFp − BS2002)/QdFp in
/// those bands: 1.18%, 2.02%, 3.30% and 5.97% (a put, S = 70, τ = 5 years,
/// r = 0.05, q = 0.1, σ = 1: 65.917 against 70.100); below 1e-6, at most
/// 6.8e-15 absolute.
#[test]
fn american_against_qdfp() {
    let table = include_str!("fixtures/pricing/american.csv");
    let mut n = 0;
    for row in rows(table) {
        let w = right(row[0]);
        let v: Vec<f64> = row[1..].iter().map(|x| num(x)).collect();
        let (s, k, tau, r, q, sigma, qdfp) = (v[0], v[1], v[2] / 365.0, v[3], v[4], v[5], v[6]);
        let bs = american::price(s, k, tau, r, q, sigma, w);
        assert!(bs <= qdfp + 1e-6, "{row:?}: BS2002 {bs} above QdFp {qdfp}");
        let tol = bs2002_tolerance(sigma * tau.sqrt()) * qdfp + 1e-13;
        assert!(bs >= qdfp - tol, "{row:?}: BS2002 {bs} below QdFp {qdfp} by more than {tol}");
        n += 1;
    }
    assert_eq!(n, 1764);
}

// ------------------------------------------------------------------ 3. TF Quant Finance

/// TF Quant Finance's documented example for `bjerksund_stensland`
/// (tf_quant_finance/black_scholes/approximations/american_option.py): calls,
/// K = 100, T = 0.25, r = 0.08, q = 0.12, σ = 0.2, given forwards F from 80 to
/// 120, i.e. spots S = F·e^{−(r−q)T} = F·e^{0.01}. The documented outputs,
/// printed to eight decimals, to 1e-7 (observed: within 5e-9).
#[test]
fn tf_quant_finance_example() {
    let documented = [0.03931201, 0.70745419, 4.01937524, 11.31429842, 21.20602005];
    for (f, want) in [80.0f64, 90.0, 100.0, 110.0, 120.0].into_iter().zip(documented) {
        let s = f * ((0.12f64 - 0.08) * 0.25).exp();
        let got = american::price(s, 100.0, 0.25, 0.08, 0.12, 0.2, CALL);
        assert!((got - want).abs() < 1e-7, "F = {f}: {got} against {want}");
    }
}

// ------------------------------------------------------------------ 4. mpmath

/// Φ, φ and erfcx against mpmath (normal.csv, x from −37.5 to 9, the
/// boundaries of Cody's intervals included): within 1e-15 relative wherever
/// the value is a normal double (observed: Φ 4.5e-16, φ 2.6e-16, erfcx
/// 3.1e-16); below that, denormal Φ within 1e-12 of its own magnitude.
#[test]
fn normal_against_mpmath() {
    let table = include_str!("fixtures/pricing/normal.csv");
    for row in rows(table) {
        let (x, c, p, e) = (num(row[0]), num(row[1]), num(row[2]), num(row[3]));
        if c >= f64::MIN_POSITIVE {
            assert!(close(normal::cdf(x), c, 1e-15, 0.0), "cdf({x}) = {:e}, mpmath {c:e}", normal::cdf(x));
        } else {
            assert!((normal::cdf(x) - c).abs() <= 1e-12 * c.max(1e-320), "cdf({x}) = {:e}, mpmath {c:e}", normal::cdf(x));
        }
        if p >= f64::MIN_POSITIVE {
            assert!(close(normal::pdf(x), p, 1e-15, 0.0), "pdf({x}) = {:e}, mpmath {p:e}", normal::pdf(x));
        }
        if e.is_finite() {
            assert!(close(normal::erfcx(x), e, 1e-15, 0.0), "erfcx({x}) = {:e}, mpmath {e:e}", normal::erfcx(x));
        }
    }
    assert_eq!(normal::cdf(f64::NEG_INFINITY), 0.0);
    assert_eq!(normal::cdf(f64::INFINITY), 1.0);
    assert!(normal::cdf(f64::NAN).is_nan() && normal::pdf(f64::NAN).is_nan());
    assert_eq!(normal::cdf(-40.0), 0.0);
    assert_eq!(normal::erfc(30.0), 0.0);
}

/// AS241 against mpmath (quantile.csv, p from 1e-300 to 1 − 1e-16): within
/// 1e-15 relative (observed 4.0e-16, two ulps).
#[test]
fn quantile_against_mpmath() {
    for row in rows(include_str!("fixtures/pricing/quantile.csv")) {
        let (p, x) = (num(row[0]), num(row[1]));
        let got = normal::inv_cdf(p);
        assert!(if x == 0.0 { got == 0.0 } else { close(got, x, 1e-15, 0.0) }, "inv_cdf({p}) = {got}, mpmath {x}");
    }
    assert_eq!(normal::inv_cdf(0.0), f64::NEG_INFINITY);
    assert_eq!(normal::inv_cdf(1.0), f64::INFINITY);
    assert!(normal::inv_cdf(-0.1).is_nan() && normal::inv_cdf(1.1).is_nan() && normal::inv_cdf(f64::NAN).is_nan());
}

/// Genz's BVND against mpmath (bivariate.csv: 696 cases, h and k from −8 to 5
/// and the tails BS2002 meets, ρ across the 0.3, 0.75 and 0.925 switches and
/// at ±1): within 1e-15 absolute (observed 1.4e-16).
#[test]
fn bivariate_against_mpmath() {
    let table = include_str!("fixtures/pricing/bivariate.csv");
    for row in rows(table) {
        let (h, k, rho, want) = (num(row[0]), num(row[1]), num(row[2]), num(row[3]));
        let got = normal::bivariate_cdf(h, k, rho);
        assert!((got - want).abs() <= 1e-15, "M({h}, {k}; {rho}) = {got:e}, mpmath {want:e}");
    }
    // Limits and symmetry.
    assert_eq!(normal::bivariate_cdf(f64::INFINITY, 0.3, 0.5), normal::cdf(0.3));
    assert_eq!(normal::bivariate_cdf(-0.2, f64::INFINITY, -0.5), normal::cdf(-0.2));
    assert_eq!(normal::bivariate_cdf(f64::NEG_INFINITY, 0.3, 0.5), 0.0);
    assert!(normal::bivariate_cdf(0.0, 0.0, 1.5).is_nan() && normal::bivariate_cdf(f64::NAN, 0.0, 0.0).is_nan());
    for (h, k, rho) in [(0.3, -1.2, 0.4), (1.5, 0.5, -0.95), (-2.0, 1.0, 0.99)] {
        assert!((normal::bivariate_cdf(h, k, rho) - normal::bivariate_cdf(k, h, rho)).abs() < 1e-16);
    }
}

/// The BS2002 price against the same formula in 40-digit arithmetic
/// (bs2002.csv: 218 calls and puts over S from 60 to 140, τ from a week to 3
/// years, r to 0.08, q to 0.12, σ from 0.05 to 1.2, and the TF example):
/// within 1e-12 relative or 1e-13 absolute. This checks the evaluation (the
/// grouping into non-negative terms, logarithms, the reflection identity),
/// not the approximation. Observed: 7.1e-14 relative above a price of 1e-3,
/// 4.9e-13 above 1e-8, 8.5e-14 absolute overall (relative errors up to
/// 2.6e-8 on prices below 1e-35, as in the European formula there).
#[test]
fn bs2002_against_mpmath() {
    for row in rows(include_str!("fixtures/pricing/bs2002.csv")) {
        let v: Vec<f64> = row.iter().enumerate().filter(|(i, _)| *i != 6).map(|(_, x)| num(x)).collect();
        let got = american::price(v[0], v[1], v[2], v[3], v[4], v[5], right(row[6]));
        assert!(close(got, v[6], 1e-12, 1e-13), "{row:?}: {got}");
    }
}

// ------------------------------------------------------------------ 5. implied volatility

/// The round-trip rule: σ recovered to 1e-10, or, where the price hardly
/// depends on σ (vega negligible: deep in or out of the money, or a time value
/// below the price's rounding), the recovered σ reprices to within 1e-12
/// relative. Prices below 1e-280, which near the underflow carry fewer than
/// 12 significant digits (the grid's least is a denormal 1e-320, a put 45%
/// out of the money for one day), need only reprice to within 1e-290.
fn recovered(sigma: f64, got: f64, p: f64, reprice: impl Fn(f64) -> f64) -> bool {
    let error = (reprice(got) - p).abs();
    (got - sigma).abs() <= 1e-10 || error <= 1e-12 * p || (p < 1e-280 && error <= 1e-290)
}

/// European round trips on a dense grid: K/F from 0.5 to 2 (16 points,
/// geometric), τ from 1 day to 5 years (12), σ from 0.01 to 3 (19), both
/// rights, three (r, q): 21888 prices from bsm::price, each inverted and
/// held to the round-trip rule.
#[test]
fn european_round_trips() {
    let kf: Vec<f64> = (0..16).map(|i| 0.5 * 4f64.powf(i as f64 / 15.0)).collect();
    let days = [1.0, 2.0, 7.0, 14.0, 30.0, 61.0, 91.0, 182.0, 365.0, 730.0, 1095.0, 1825.0];
    let sigmas = [0.01, 0.015, 0.02, 0.03, 0.05, 0.075, 0.1, 0.15, 0.2, 0.3, 0.45, 0.6, 0.8, 1.0, 1.25, 1.5, 2.0, 2.5, 3.0];
    let s = 100.0;
    let mut n = 0;
    for (r, q) in [(0.0f64, 0.0f64), (0.05, 0.02), (0.01, 0.07)] {
        for &m in &kf {
            for &d in &days {
                let tau = d / 365.0;
                let k = m * s * ((r - q) * tau).exp();
                for &sigma in &sigmas {
                    for w in [CALL, PUT] {
                        let p = bsm::price(s, k, tau, r, q, sigma, w);
                        let got = iv::implied_vol(p, s, k, tau, r, q, w, Style::European).unwrap_or_else(|| panic!("None for {p} K/F {m} {d}d σ {sigma} {w:?}"));
                        assert!(recovered(sigma, got, p, |v| bsm::price(s, k, tau, r, q, v, w)), "K/F {m} {d}d r {r} q {q} σ {sigma} {w:?}: {got}");
                        n += 1;
                    }
                }
            }
        }
    }
    assert_eq!(n, 21888);
}

/// American round trips: K/F from 0.5 to 2, τ from 1 day to 5 years, σ from
/// 0.01 to 3, both rights, (r, q) = (0.05, 0.02) and (0.02, 0.06), under the
/// same rule with american::price as the pricer.
#[test]
fn american_round_trips() {
    let s = 100.0;
    let mut n = 0;
    for (r, q) in [(0.05f64, 0.02f64), (0.02, 0.06)] {
        for m in [0.5, 0.7, 0.9, 1.0, 1.1, 1.4, 2.0] {
            for d in [1.0, 7.0, 30.0, 182.0, 730.0, 1825.0] {
                let tau: f64 = d / 365.0;
                let k = m * s * ((r - q) * tau).exp();
                for sigma in [0.01, 0.05, 0.15, 0.3, 0.6, 1.2, 3.0] {
                    for w in [CALL, PUT] {
                        let p = american::price(s, k, tau, r, q, sigma, w);
                        let got = iv::implied_vol(p, s, k, tau, r, q, w, Style::American).unwrap_or_else(|| panic!("None for {p} K/F {m} {d}d σ {sigma} {w:?}"));
                        assert!(
                            recovered(sigma, got, p, |v| american::price(s, k, tau, r, q, v, w)),
                            "K/F {m} {d}d r {r} q {q} σ {sigma} {w:?}: {got} (p {p})"
                        );
                        n += 1;
                    }
                }
            }
        }
    }
    assert_eq!(n, 1176);
}

/// Prices at known volatilities, exact to the double (implied.csv from
/// mpmath): the recovered σ within 1e-10, or within 1e-12·p/vega with the
/// exact vega, i.e. a price error of 1e-12 relative against the exact price
/// function (observed: at most 4.7e-14 where σ is off by more than 1e-10).
#[test]
fn implied_vol_of_exact_prices() {
    let mut n = 0;
    for row in rows(include_str!("fixtures/pricing/implied.csv")) {
        let v: Vec<f64> = row.iter().enumerate().filter(|(i, _)| *i != 6).map(|(_, x)| num(x)).collect();
        let (s, k, tau, r, q, sigma, p, vega) = (v[0], v[1], v[2], v[3], v[4], v[5], v[6], v[7]);
        let got = iv::implied_vol(p, s, k, tau, r, q, right(row[6]), Style::European).unwrap_or_else(|| panic!("None for {row:?}"));
        let ds = (got - sigma).abs();
        assert!(ds <= 1e-10 || ds * vega <= 1e-12 * p, "{row:?}: {got}");
        n += 1;
    }
    assert_eq!(n, 1161);
}

/// py_lets_be_rational 1.1.2 (an independent implementation of the same
/// paper) on 348 undiscounted Black prices (lbr.csv): the same implied
/// volatility within 1e-12 relative (observed: all within it).
#[test]
fn implied_vol_against_py_lets_be_rational() {
    for row in rows(include_str!("fixtures/pricing/lbr.csv")) {
        let (f, k, tau, p, lbr) = (num(row[0]), num(row[1]), num(row[2]), num(row[4]), num(row[6]));
        let got = iv::implied_vol(p, f, k, tau, 0.0, 0.0, right(row[3]), Style::European).unwrap_or_else(|| panic!("None for {row:?}"));
        assert!(close(got, lbr, 1e-12, 0.0), "{row:?}: {got}");
    }
}

/// Prices outside the no-arbitrage bounds, and invalid inputs, give None.
#[test]
fn implied_vol_bounds() {
    let (s, k, tau, r, q) = (100.0, 110.0, 0.5, 0.05, 0.02);
    let eu = |p: f64, w: Right| iv::implied_vol(p, s, k, tau, r, q, w, Style::European);
    let (fwd, kd) = (s * (-q * tau).exp(), k * (-r * tau).exp());
    // At or above the upper bounds.
    assert_eq!(eu(fwd, CALL), None);
    assert_eq!(eu(fwd + 1.0, CALL), None);
    assert_eq!(eu(kd, PUT), None);
    // Below the discounted intrinsic value beyond rounding; at it, σ = 0.
    let intrinsic_put = kd - fwd;
    assert_eq!(eu(intrinsic_put - 1e-9, PUT), None);
    assert_eq!(eu(intrinsic_put, PUT), Some(0.0));
    assert_eq!(eu(-1e-9, CALL), None);
    assert_eq!(eu(0.0, CALL), Some(0.0));
    // Invalid inputs.
    for (p, s, k, tau) in [
        (1.0, 100.0, 110.0, 0.0),
        (1.0, 100.0, 110.0, -1.0),
        (1.0, 0.0, 110.0, 0.5),
        (1.0, 100.0, -1.0, 0.5),
        (f64::NAN, 100.0, 110.0, 0.5),
        (1.0, f64::INFINITY, 110.0, 0.5),
    ] {
        for style in [Style::European, Style::American] {
            assert_eq!(iv::implied_vol(p, s, k, tau, r, q, CALL, style), None);
        }
    }
    assert_eq!(iv::implied_vol(1.0, 100.0, 110.0, 0.5, f64::NAN, q, CALL, Style::European), None);
    // American: at or above S (call) or K (put), below the σ = 0 value, above the σ = 10 value.
    let am = |p: f64, w: Right| iv::implied_vol(p, s, k, tau, r, q, w, Style::American);
    assert_eq!(am(s, CALL), None);
    assert_eq!(am(k, PUT), None);
    let floor = american::price(s, k, tau, r, q, 0.0, PUT);
    assert_eq!(floor, k - s);
    assert_eq!(am(floor - 1e-9, PUT), None);
    assert_eq!(am(floor, PUT), Some(0.0));
    let top = american::price(s, k, tau, r, q, 10.0, CALL);
    assert!(top < s);
    assert_eq!(am(0.5 * (top + s), CALL), None);
    // A deep in-the-money American put worth more than the European bound
    // K·e^{−rτ}: no European implied vol, but the American one inverts. In the
    // exercise region (S = 20, σ = 1) the price is the σ = 0 value: Some(0.0).
    let bound = 100.0 * (-0.4f64).exp();
    for sigma in [1.5, 2.0, 3.0] {
        let p = american::price(25.0, 100.0, 5.0, 0.08, 0.0, sigma, PUT);
        assert!(p > bound);
        assert_eq!(iv::implied_vol(p, 25.0, 100.0, 5.0, 0.08, 0.0, PUT, Style::European), None);
        let got = iv::implied_vol(p, 25.0, 100.0, 5.0, 0.08, 0.0, PUT, Style::American).unwrap_or_else(|| panic!("None at σ {sigma}"));
        assert!((got - sigma).abs() <= 1e-10, "σ {sigma}: {got}");
    }
    let p = american::price(20.0, 100.0, 5.0, 0.08, 0.0, 1.0, PUT);
    assert_eq!(p, 80.0);
    assert_eq!(iv::implied_vol(p, 20.0, 100.0, 5.0, 0.08, 0.0, PUT, Style::American), Some(0.0));
}

// ------------------------------------------------------------------ 6. properties

/// Put–call parity: C − P = S·e^{−qτ} − K·e^{−rτ} (BSM) and e^{−rτ}(F − K)
/// (Black-76), to 1e-12 of max(S, K).
#[test]
fn put_call_parity() {
    for s in [50.0, 90.0, 100.0, 111.0, 200.0] {
        for tau in [1.0 / 365.0, 0.25, 1.0, 5.0] {
            for (r, q) in [(0.0, 0.0), (0.05, 0.02), (-0.01, 0.03), (0.08, 0.12)] {
                for sigma in [0.0, 0.05, 0.3, 1.5] {
                    let k = 100.0;
                    let tol = 1e-12 * f64::max(s, k);
                    let lhs = bsm::price(s, k, tau, r, q, sigma, CALL) - bsm::price(s, k, tau, r, q, sigma, PUT);
                    assert!((lhs - (s * (-q * tau).exp() - k * (-r * tau).exp())).abs() <= tol, "bsm {s} {tau} {r} {q} {sigma}");
                    let lhs = b76::price(s, k, tau, r, sigma, CALL) - b76::price(s, k, tau, r, sigma, PUT);
                    assert!((lhs - (-r * tau).exp() * (s - k)).abs() <= tol, "b76 {s} {tau} {r} {sigma}");
                }
            }
        }
    }
}

/// An American call on an asset without a yield (q = 0, r ≥ 0) is never
/// exercised early: its price is the European one exactly, its premium 0.
/// With q > 0 or for puts, American ≥ European (the safeguard makes it exact).
#[test]
fn american_and_european() {
    for s in [60.0, 95.0, 100.0, 130.0] {
        for tau in [0.01, 0.5, 3.0] {
            for r in [0.0, 0.02, 0.1] {
                for sigma in [0.0, 0.1, 0.4, 2.0] {
                    let eu = bsm::price(s, 100.0, tau, r, 0.0, sigma, CALL);
                    assert_eq!(american::price(s, 100.0, tau, r, 0.0, sigma, CALL), eu);
                    assert_eq!(american::early_exercise_premium(s, 100.0, tau, r, 0.0, sigma, CALL), 0.0);
                    for q in [0.0, 0.01, 0.05, 0.15] {
                        for w in [CALL, PUT] {
                            let am = american::price(s, 100.0, tau, r, q, sigma, w);
                            assert!(am >= bsm::price(s, 100.0, tau, r, q, sigma, w), "{s} {tau} {r} {q} {sigma} {w:?}");
                            assert!(am >= (w.sign() * (s - 100.0)).max(0.0));
                            assert!(american::early_exercise_premium(s, 100.0, tau, r, q, sigma, w) >= 0.0);
                        }
                    }
                }
            }
        }
    }
}

/// The put–call transformation P(S, K, T, r, q, σ) = C(K, S, T, q, r, σ)
/// (in Bjerksund–Stensland's terms P(S, K, T, r, b) = C(K, S, T, r − b, −b)):
/// American and European, to 1e-12 relative.
#[test]
fn put_call_transformation() {
    for s in [70.0, 100.0, 125.0] {
        for tau in [0.1, 1.0, 4.0] {
            for (r, q) in [(0.05, 0.0), (0.03, 0.06), (0.08, 0.02), (0.0, 0.04)] {
                for sigma in [0.15, 0.4, 1.0] {
                    let put = american::price(s, 100.0, tau, r, q, sigma, PUT);
                    let call = american::price(100.0, s, tau, q, r, sigma, CALL);
                    assert!(close(put, call, 1e-12, 1e-13), "american {s} {tau} {r} {q} {sigma}: {put} {call}");
                    let put = bsm::price(s, 100.0, tau, r, q, sigma, PUT);
                    let call = bsm::price(100.0, s, tau, q, r, sigma, CALL);
                    assert!(close(put, call, 1e-12, 1e-13), "european {s} {tau} {r} {q} {sigma}");
                }
            }
        }
    }
}

/// Central differences of a price function: delta, gamma, vega, theta (−∂/∂τ)
/// and rho, and for each the rounding error of its stencil (4ε·|V|/h, or
/// 16ε·|V|/h² for the second difference). In S the bumps scale with S·σ√τ,
/// the scale on which the price varies (1e-4 of it for delta, 5e-4 for
/// gamma); 1e-4 relative in σ and τ, 1e-5 in r.
fn differences(price: impl Fn(f64, f64, f64, f64) -> f64, s: f64, tau: f64, r: f64, sigma: f64) -> (Greeks, Greeks) {
    let scale = s * sigma * tau.sqrt();
    let (hs, hg, hv, ht, hr) = (1e-4 * scale, 5e-4 * scale, 1e-4 * sigma, 1e-4 * tau, 1e-5);
    let p0 = price(s, tau, r, sigma);
    let e = 4.0 * f64::EPSILON * p0.abs();
    let fd = Greeks {
        delta: (price(s + hs, tau, r, sigma) - price(s - hs, tau, r, sigma)) / (2.0 * hs),
        gamma: (price(s + hg, tau, r, sigma) - 2.0 * p0 + price(s - hg, tau, r, sigma)) / (hg * hg),
        vega: (price(s, tau, r, sigma + hv) - price(s, tau, r, sigma - hv)) / (2.0 * hv),
        theta: -(price(s, tau + ht, r, sigma) - price(s, tau - ht, r, sigma)) / (2.0 * ht),
        rho: (price(s, tau, r + hr, sigma) - price(s, tau, r - hr, sigma)) / (2.0 * hr),
    };
    let rounding = Greeks {
        delta: e / hs,
        gamma: 4.0 * e / (hg * hg),
        vega: e / hv,
        theta: e / ht,
        rho: e / hr,
    };
    (fd, rounding)
}

/// Each Greek within `rel` of the reference, or within `abs` plus the
/// stencil's rounding.
fn greeks_close(name: &str, got: Greeks, want: Greeks, rounding: Greeks, rel: f64, abs: f64) {
    let pairs = [
        ("delta", got.delta, want.delta, rounding.delta),
        ("gamma", got.gamma, want.gamma, rounding.gamma),
        ("vega", got.vega, want.vega, rounding.vega),
        ("theta", got.theta, want.theta, rounding.theta),
        ("rho", got.rho, want.rho, rounding.rho),
    ];
    for (g, a, b, e) in pairs {
        assert!(close(a, b, rel, abs + e), "{name} {g}: {a:e} against {b:e}");
    }
}

/// The analytic Greeks of BSM and Black-76 against central differences of
/// their prices, to 1e-6 relative (with a floor of 1e-9 for Greeks near 0).
#[test]
fn analytic_greeks_against_differences() {
    for s in [80.0, 100.0, 120.0] {
        for tau in [0.05, 0.5, 2.0] {
            for (r, q) in [(0.0, 0.0), (0.05, 0.02), (0.02, 0.07)] {
                for sigma in [0.1, 0.35, 1.0] {
                    for w in [CALL, PUT] {
                        let name = format!("bsm {s} {tau} {r} {q} {sigma} {w:?}");
                        let (fd, rounding) = differences(|s, tau, r, sigma| bsm::price(s, 100.0, tau, r, q, sigma, w), s, tau, r, sigma);
                        greeks_close(&name, bsm::greeks(s, 100.0, tau, r, q, sigma, w), fd, rounding, 1e-6, 1e-9);
                        let name = format!("b76 {s} {tau} {r} {sigma} {w:?}");
                        let (fd, rounding) = differences(|f, tau, r, sigma| b76::price(f, 100.0, tau, r, sigma, w), s, tau, r, sigma);
                        greeks_close(&name, b76::greeks(s, 100.0, tau, r, sigma, w), fd, rounding, 1e-6, 1e-9);
                        // The single-Greek functions are the fields of greeks().
                        let g = bsm::greeks(s, 100.0, tau, r, q, sigma, w);
                        let one = [bsm::delta, bsm::gamma, bsm::vega, bsm::theta, bsm::rho].map(|f| f(s, 100.0, tau, r, q, sigma, w));
                        assert_eq!(one, [g.delta, g.gamma, g.vega, g.theta, g.rho]);
                        let g = b76::greeks(s, 100.0, tau, r, sigma, w);
                        let one = [b76::delta, b76::gamma, b76::vega, b76::theta, b76::rho].map(|f| f(s, 100.0, tau, r, sigma, w));
                        assert_eq!(one, [g.delta, g.gamma, g.vega, g.theta, g.rho]);
                        assert!(close(g.rho, -tau * b76::price(s, 100.0, tau, r, sigma, w), 1e-15, 1e-300));
                    }
                }
            }
        }
    }
}

/// American Greeks (central differences of the price): where the American
/// price is the European one (calls with q = 0) they match BSM's analytic
/// Greeks to 1e-6 relative, or within the rounding of the stencils (a deep
/// in-the-money gamma is small against V/(Sσ√τ)²); elsewhere they have the
/// signs of an American option (call delta in [0, 1], put delta in [−1, 0],
/// gamma and vega ≥ 0).
#[test]
fn american_greeks() {
    for s in [80.0, 100.0, 120.0] {
        for tau in [0.1, 1.0] {
            for r in [0.0, 0.04] {
                for sigma in [0.2, 0.6] {
                    let name = format!("american call q = 0 {s} {tau} {r} {sigma}");
                    // The rounding of american::greeks' stencils (its documented bumps) on a
                    // price good to 4ε.
                    let v = american::price(s, 100.0, tau, r, 0.0, sigma, CALL);
                    let e = 4.0 * f64::EPSILON * v;
                    let h = 3e-3 * s * sigma * tau.sqrt();
                    let rounding = Greeks {
                        delta: 1.5 * e / h,
                        gamma: 64.0 / 12.0 * e / (h * h),
                        vega: e / (1e-4 * sigma),
                        theta: e / (1e-4 * tau),
                        rho: e / 1e-4,
                    };
                    greeks_close(
                        &name,
                        american::greeks(s, 100.0, tau, r, 0.0, sigma, CALL),
                        bsm::greeks(s, 100.0, tau, r, 0.0, sigma, CALL),
                        rounding,
                        1e-6,
                        1e-9,
                    );
                    for (w, q) in [(CALL, 0.06), (PUT, 0.0), (PUT, 0.03)] {
                        let g = american::greeks(s, 100.0, tau, r.max(0.03), q, sigma, w);
                        // The exercise region is linear in S; its difference rounds to ±1 ± 1e-12.
                        let delta_range = if w == CALL { -1e-9..=1.0 + 1e-9 } else { -1.0 - 1e-9..=1e-9 };
                        assert!(delta_range.contains(&g.delta), "{s} {tau} {q} {sigma} {w:?}: {g:?}");
                        assert!(g.gamma >= -1e-8 && g.vega >= -1e-8, "{s} {tau} {q} {sigma} {w:?}: {g:?}");
                    }
                }
            }
        }
    }
}

// ------------------------------------------------------------------ edge cases

/// τ ≤ 0: the intrinsic value; delta ω strictly in the money, else 0; the other
/// Greeks 0. For BSM, Black-76 and the American price alike.
#[test]
fn expired() {
    for tau in [0.0, -0.5] {
        for (s, w, value, delta) in [
            (110.0, CALL, 10.0, 1.0),
            (90.0, CALL, 0.0, 0.0),
            (100.0, CALL, 0.0, 0.0),
            (90.0, PUT, 10.0, -1.0),
            (110.0, PUT, 0.0, 0.0),
            (100.0, PUT, 0.0, 0.0),
        ] {
            let expect = Greeks { delta, ..Greeks::default() };
            assert_eq!(bsm::price(s, 100.0, tau, 0.05, 0.02, 0.3, w), value);
            assert_eq!(bsm::greeks(s, 100.0, tau, 0.05, 0.02, 0.3, w), expect);
            assert_eq!(b76::price(s, 100.0, tau, 0.05, 0.3, w), value);
            assert_eq!(b76::greeks(s, 100.0, tau, 0.05, 0.3, w), expect);
            assert_eq!(american::price(s, 100.0, tau, 0.05, 0.02, 0.3, w), value);
            assert_eq!(american::greeks(s, 100.0, tau, 0.05, 0.02, 0.3, w), expect);
        }
    }
}

/// σ√τ ≈ 0: the discounted intrinsic value of the forward and its limits; the
/// closed form just above the threshold returns the same values.
#[test]
fn no_volatility() {
    let (k, tau, r, q) = (100.0, 0.5f64, 0.05f64, 0.02f64);
    for (s, w) in [(110.0, CALL), (90.0, CALL), (90.0, PUT), (110.0, PUT)] {
        let (dq, dr) = ((-q * tau).exp(), (-r * tau).exp());
        let value = (w.sign() * (s * dq - k * dr)).max(0.0);
        let itm = value > 0.0;
        for sigma in [0.0, 1e-120] {
            assert_eq!(bsm::price(s, k, tau, r, q, sigma, w), value);
            let g = bsm::greeks(s, k, tau, r, q, sigma, w);
            let expect = if itm {
                Greeks {
                    delta: w.sign() * dq,
                    gamma: 0.0,
                    theta: w.sign() * (q * s * dq - r * k * dr),
                    vega: 0.0,
                    rho: w.sign() * k * tau * dr,
                }
            } else {
                Greeks::default()
            };
            assert_eq!(g, expect, "{s} {w:?} σ {sigma}");
            let v76 = b76::price(s, k, tau, r, sigma, w);
            assert!(close(v76, ((-r * tau).exp() * w.sign() * (s - k)).max(0.0), 1e-15, 0.0));
            let g = b76::greeks(s, k, tau, r, sigma, w);
            // ω·(r·F·D − r·K·D) against r·V: a cancellation of a few ulps.
            assert!(close(g.theta, r * v76, 1e-14, 0.0) && g.rho == -tau * v76 && g.gamma == 0.0 && g.vega == 0.0, "{s} {w:?}: {g:?}");
        }
        // The closed form at σ√τ = 1e-60 gives the same price and delta.
        let sigma = 1e-60 / tau.sqrt();
        assert_eq!(bsm::price(s, k, tau, r, q, sigma, w), value);
        assert_eq!(bsm::delta(s, k, tau, r, q, sigma, w), bsm::delta(s, k, tau, r, q, 0.0, w));
    }
    // American, σ = 0: the best deterministic exercise time. A put with r > q
    // in the money: exercise now. A call with q > r: exercise now if in the
    // money; one out of the money stays worthless.
    assert_eq!(american::price(90.0, 100.0, 1.0, 0.05, 0.0, 0.0, PUT), 10.0);
    assert_eq!(american::price(110.0, 100.0, 1.0, 0.02, 0.08, 0.0, CALL), 10.0);
    assert_eq!(american::price(90.0, 100.0, 1.0, 0.02, 0.08, 0.0, CALL), 0.0);
    // A call with r > q > 0 grows into the money: exercise at the boundary rK/q.
    let (s, k, r, q) = (100.0f64, 100.0, 0.08, 0.02);
    let t_star = (q * s / (r * k)).ln() / (q - r);
    let best = s * (-q * t_star).exp() - k * (-r * t_star).exp();
    assert!(t_star > 0.0 && t_star < 30.0);
    assert!(close(american::price(s, k, 30.0, r, q, 0.0, CALL), best, 1e-15, 0.0));
    assert!(best > bsm::price(s, k, 30.0, r, q, 0.0, CALL));
    // Continuity into the formula as σ → 0.
    let p0 = american::price(100.0, 100.0, 1.0, 0.05, 0.02, 0.0, CALL);
    assert!(close(american::price(100.0, 100.0, 1.0, 0.05, 0.02, 1e-8, CALL), p0, 1e-12, 0.0));
}

/// S ≤ 0, K ≤ 0, σ < 0, NaN or infinite inputs: NaN, Greeks included.
#[test]
fn invalid_inputs() {
    let bad = [
        (0.0, 100.0, 1.0, 0.3),
        (-1.0, 100.0, 1.0, 0.3),
        (100.0, 0.0, 1.0, 0.3),
        (100.0, -5.0, 1.0, 0.3),
        (100.0, 100.0, 1.0, -0.1),
        (f64::NAN, 100.0, 1.0, 0.3),
        (100.0, f64::NAN, 1.0, 0.3),
        (100.0, 100.0, f64::NAN, 0.3),
        (100.0, 100.0, 1.0, f64::NAN),
        (f64::INFINITY, 100.0, 1.0, 0.3),
        (100.0, 100.0, f64::INFINITY, 0.3),
    ];
    for (s, k, tau, sigma) in bad {
        for w in [CALL, PUT] {
            assert!(bsm::price(s, k, tau, 0.05, 0.02, sigma, w).is_nan(), "{s} {k} {tau} {sigma}");
            assert!(b76::price(s, k, tau, 0.05, sigma, w).is_nan());
            assert!(american::price(s, k, tau, 0.05, 0.02, sigma, w).is_nan());
            for g in [
                bsm::greeks(s, k, tau, 0.05, 0.02, sigma, w),
                b76::greeks(s, k, tau, 0.05, sigma, w),
                american::greeks(s, k, tau, 0.05, 0.02, sigma, w),
            ] {
                assert!(
                    g.delta.is_nan() && g.gamma.is_nan() && g.theta.is_nan() && g.vega.is_nan() && g.rho.is_nan(),
                    "{s} {k} {tau} {sigma}: {g:?}"
                );
            }
        }
    }
    assert!(bsm::price(100.0, 100.0, 1.0, f64::NAN, 0.0, 0.2, CALL).is_nan());
    assert!(american::price(100.0, 100.0, 1.0, 0.05, f64::NAN, 0.2, PUT).is_nan());
}

#[test]
fn units_and_parsing() {
    assert_eq!(YEAR_NANOS, 31_536_000_000_000_000.0);
    assert_eq!(years(86_400_000_000_000), 1.0 / 365.0);
    for n in [1i64, 7, 30, 91, 182, 365, 730, 1825] {
        assert_eq!(years(n * 86_400_000_000_000), n as f64 / 365.0);
    }
    assert_eq!(years(0), 0.0);
    assert_eq!(years(-86_400_000_000_000), -1.0 / 365.0);
    assert_eq!(Right::parse("call"), Some(CALL));
    assert_eq!(Right::parse("put"), Some(PUT));
    assert_eq!(Right::parse("Call"), None);
    assert_eq!((CALL.sign(), PUT.sign()), (1.0, -1.0));
    assert_eq!(Style::parse("european"), Some(Style::European));
    assert_eq!(Style::parse("american"), Some(Style::American));
    assert_eq!(Style::parse("bermudan"), None);
}
