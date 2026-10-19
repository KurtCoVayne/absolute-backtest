//! American options by Bjerksund and Stensland (2002), "Closed form
//! valuation of American options", NHH discussion paper: the value of
//! exercising when the price reaches a flat trigger X during [0, t1] or x
//! during [t1, T), t1 = ½(√5 − 1)·T. It is the value of a feasible exercise
//! strategy, hence a lower bound of the American price.
//!
//! A call, with cost of carry b = r − q, with b ≥ r (q ≤ 0) is never
//! exercised early (Merton 1973) and is worth the European value. Otherwise,
//! with β = (½ − b/σ²) + √((b/σ² − ½)² + 2r/σ²), B∞ = β/(β − 1)·K,
//! B0 = max(K, r/(r − b)·K), h(τ) = −(bτ + 2σ√τ)·K²/((B∞ − B0)·B0) and
//! I(τ) = B0 + (B∞ − B0)(1 − e^{h(τ)}), the triggers are X = I(T) and
//! x = I(T − t1), and the value is S − K when S ≥ X, otherwise
//!
//! ```text
//! C = α(X)S^β − α(X)φ(S,t1,β,X,X) + φ(S,t1,1,X,X) − φ(S,t1,1,x,X)
//!   − Kφ(S,t1,0,X,X) + Kφ(S,t1,0,x,X) + α(x)φ(S,t1,β,x,X)
//!   − α(x)ψ(S,T,β,x,X,x,t1) + ψ(S,T,1,x,X,x,t1) − ψ(S,T,1,K,X,x,t1)
//!   − Kψ(S,T,0,x,X,x,t1) + Kψ(S,T,0,K,X,x,t1),
//! ```
//!
//! α(y) = (y − K)·y^{−β}, with the paper's φ (univariate normal) and ψ
//! (bivariate normal with correlation ±√(t1/T)).
//!
//! The put comes from the put–call transformation
//! P(S, K, T, r, b, σ) = C(K, S, T, r − b, −b, σ); puts with r ≤ 0 are never
//! exercised early.
//!
//! Numerics (the value is the formula's; only its evaluation is arranged):
//! - β by the root formula that avoids cancellation, β − 1 = q/(½σ²β + b),
//!   B∞ − B0 = K/(β − 1) or K·σ²β/(2q) (as r − βb = ½σ²β(β − 1)), and
//!   1 − e^{h} by `exp_m1`.
//! - The twelve terms are summed as four groups that are each a non-negative
//!   expectation: exercise at X (the two α(X) terms), exercise at t1 between
//!   x and X (the four φ terms), exercise at x after t1 (α(x)φ − α(x)ψ), and
//!   the payoff at T (the four ψ terms). Within them, differences of normal
//!   probabilities are taken between upper tails where those are the small
//!   ones (N(a) − M(a, b; ρ) = M(a, −b; −ρ), and likewise for intervals), so
//!   that a small price keeps its relative accuracy instead of being the
//!   rounding residue of terms of the size of K.
//! - A reflected univariate term e^{κL}·N(d − 2L/(σ√t)), whose factor e^{κL}
//!   can overflow while N underflows, is evaluated as
//!   φ(d)·e^{−2L·ln(X/H)/(σ²t)}·N(d − 2L/(σ√t))/φ(d − 2L/(σ√t)), an exact
//!   rearrangement whose factors are all bounded; a bivariate term as
//!   exp(ln coefficient + ln M).
//!
//! Model choices, each documented where it is made:
//! - h(τ) is capped at 0, i.e. I(τ) ≥ B0 ≥ K, and x at X. The paper's h is
//!   positive only when b < 0 and |b|√τ > 2σ; uncapped it puts a trigger
//!   below the strike (even below zero), where the formula no longer values a
//!   strategy. Capped, the trigger is K and the strategy worthless, so there
//!   the price is the European (or intrinsic) value: the approximation itself
//!   fails in that corner (QuantLib's Bjerksund–Stensland engine does too).
//! - The price is at least max(intrinsic value, European value): both are
//!   values of feasible strategies, so the bound stays a lower bound.
//! - σ√τ below [`bsm::MIN_STD`]: the limit σ → 0, the value of the best
//!   deterministic exercise time, max over t ∈ [0, τ] of
//!   max(ω(S·e^{−qt} − K·e^{−rt}), 0) (the formula's triggers tend to B0, the
//!   deterministic optimal boundary).
//! - Greeks by central differences of [`price`] with the bumps of [`greeks`].
//!
//! Edge cases as for [`bsm`]: NaN for S ≤ 0, K ≤ 0, σ < 0 or a NaN or
//! infinite input; for τ ≤ 0 the intrinsic value, delta ω strictly in the
//! money, the other Greeks 0.

use std::sync::OnceLock;

use super::normal::{cdf, mills, pdf, Angles};
use super::{bsm, Greeks, Right, NAN_GREEKS};

/// t1/T = ½(√5 − 1).
const SPLIT: f64 = 0.6180339887498949;
/// (T − t1)/T = ½(3 − √5).
const REST: f64 = 0.38196601125010515;
/// √(t1/T).
const RHO: f64 = 0.7861513777574233;

/// The bivariate normal's quadrature at ρ = +√(t1/T) and −√(t1/T), whose
/// angles are computed once.
fn bivariate() -> &'static (Angles, Angles) {
    static ANGLES: OnceLock<(Angles, Angles)> = OnceLock::new();
    ANGLES.get_or_init(|| (Angles::new(RHO), Angles::new(-RHO)))
}

/// The American value (module documentation).
pub fn price(s: f64, k: f64, tau: f64, r: f64, q: f64, sigma: f64, right: Right) -> f64 {
    let finite = [s, k, tau, r, q, sigma].iter().all(|x| x.is_finite());
    if !finite || s <= 0.0 || k <= 0.0 || sigma < 0.0 {
        return f64::NAN;
    }
    let w = right.sign();
    let intrinsic = (w * (s - k)).max(0.0);
    if tau <= 0.0 {
        return intrinsic;
    }
    let european = bsm::price(s, k, tau, r, q, sigma, right);
    let early = match right {
        Right::Call => q > 0.0,
        Right::Put => r > 0.0,
    };
    if !early {
        return european.max(intrinsic);
    }
    if sigma * tau.sqrt() < bsm::MIN_STD {
        return deterministic(s, k, tau, r, q, w);
    }
    let raw = match right {
        Right::Call => call_2002(s, k, tau, r, r - q, sigma),
        Right::Put => call_2002(k, s, tau, q, q - r, sigma),
    };
    raw.max(european).max(intrinsic)
}

/// American less European value.
pub fn early_exercise_premium(s: f64, k: f64, tau: f64, r: f64, q: f64, sigma: f64, right: Right) -> f64 {
    price(s, k, tau, r, q, sigma, right) - bsm::price(s, k, tau, r, q, sigma, right)
}

/// Greeks by central differences of [`price`]. The bumps are a model
/// choice:
/// - delta and gamma from the five-point stencil S + jh, j = −2..2, whose
///   truncation error is O(h⁴), with h = 3e-3·S·min(max(σ√τ, 1e-4), 1)
///   (the price varies in S on the scale S·σ√τ). Truncation is then about
///   1e-12·d⁴ relative (d the distance to the strike in standard
///   deviations), and the rounding of a price good to ~1e-14 relative costs
///   gamma about 6e-9·V/(Γ·(Sσ√τ)²): 6e-9 at the money, ~3e-6 deep in it.
///   Within 2h of the exercise boundary the stencil measures the average
///   slope across it.
/// - vega over σ·(1 ± 1e-4) (0 at σ = 0), theta over τ·(1 ± 1e-4)
///   (∂V/∂t = −∂V/∂τ per year), rho over r ± 1e-4 with q fixed.
pub fn greeks(s: f64, k: f64, tau: f64, r: f64, q: f64, sigma: f64, right: Right) -> Greeks {
    let p0 = price(s, k, tau, r, q, sigma, right);
    if p0.is_nan() {
        return NAN_GREEKS;
    }
    if tau <= 0.0 {
        let delta = if right.sign() * (s - k) > 0.0 { right.sign() } else { 0.0 };
        return Greeks { delta, ..Greeks::default() };
    }
    let at = |s: f64, tau: f64, r: f64, sigma: f64| price(s, k, tau, r, q, sigma, right);
    let hs = 3e-3 * s * (sigma * tau.sqrt()).clamp(1e-4, 1.0);
    let [m2, m1, p1, p2] = [-2.0, -1.0, 1.0, 2.0].map(|j| at(s + j * hs, tau, r, sigma));
    let delta = (8.0 * (p1 - m1) - (p2 - m2)) / (12.0 * hs);
    let gamma = (16.0 * (p1 + m1) - (p2 + m2) - 30.0 * p0) / (12.0 * hs * hs);
    let hv = 1e-4 * sigma;
    let vega = if hv > 0.0 { (at(s, tau, r, sigma + hv) - at(s, tau, r, sigma - hv)) / (2.0 * hv) } else { 0.0 };
    let ht = 1e-4 * tau;
    let theta = -(at(s, tau + ht, r, sigma) - at(s, tau - ht, r, sigma)) / (2.0 * ht);
    let hr = 1e-4;
    let rho = (at(s, tau, r + hr, sigma) - at(s, tau, r - hr, sigma)) / (2.0 * hr);
    Greeks { delta, gamma, theta, vega, rho }
}

/// The value as σ → 0: the best deterministic exercise time t ∈ [0, τ] for
/// the payoff max(ω(S·e^{−qt} − K·e^{−rt}), 0), whose stationary point solves
/// q·S·e^{−qt} = r·K·e^{−rt}.
fn deterministic(s: f64, k: f64, tau: f64, r: f64, q: f64, w: f64) -> f64 {
    let f = |t: f64| w * (s * (-q * t).exp() - k * (-r * t).exp());
    let mut best = f(0.0).max(f(tau)).max(0.0);
    let ratio = q * s / (r * k);
    if q != r && ratio > 0.0 {
        let t = ratio.ln() / (q - r);
        if t > 0.0 && t < tau {
            best = best.max(f(t));
        }
    }
    best
}

/// β and the triggers X = I(T) and x = I(T − t1) of a call with b < r.
fn triggers(k: f64, t: f64, r: f64, b: f64, sigma: f64) -> (f64, f64, f64) {
    let s2 = sigma * sigma;
    let q = r - b;
    let p = 0.5 - b / s2;
    let disc = (p * p + 2.0 * r / s2).sqrt();
    // The larger root of ½σ²β(β − 1) + bβ − r = 0, without cancellation.
    let beta = if p >= 0.0 { p + disc } else { 2.0 * r / s2 / (disc - p) };
    // B0/K and (B∞ − B0)/K.
    let (b0, spread) = if r > q { (r / q, s2 * beta / (2.0 * q)) } else { (1.0, (0.5 * s2 * beta + b) / q) };
    let trigger = |tau: f64| {
        // Capped at 0 (module documentation): I(τ) ≥ B0.
        let h = (-(b * tau + 2.0 * sigma * tau.sqrt()) / (spread * b0)).min(0.0);
        k * (b0 - spread * h.exp_m1())
    };
    // x ≤ X holds unless the cap binds for X alone; the formula needs it.
    let xb = trigger(t);
    (beta, xb, trigger(REST * t).min(xb))
}

/// e^{κL}·N(d − c), the reflection of N(d) in a barrier: `kl` = κL and
/// `extra` = κL + dc − c²/2 in closed form (≤ 0), so that for d − c ≤ 0 the
/// value is φ(d)·e^{extra}·N(d − c)/φ(d − c) with every factor bounded. For
/// d − c > 0 the factor e^{κL} is below N(d)/N(d − c) < 2.
fn reflected(d: f64, c: f64, kl: f64, extra: f64) -> f64 {
    let e = d - c;
    if e <= 0.0 {
        pdf(d) * extra.exp() * mills(e)
    } else {
        kl.exp() * cdf(e)
    }
}

/// N(hi) − N(lo) for lo ≤ hi, between upper tails when both are positive.
fn ndiff(lo: f64, hi: f64) -> f64 {
    if lo >= 0.0 {
        cdf(-lo) - cdf(-hi)
    } else {
        cdf(hi) - cdf(lo)
    }
}

/// P(X ≤ a, lo < Y ≤ hi) for the correlation of `m` (`flip` that of the
/// opposite sign), between upper tails of Y when lo ≥ 0.
fn mdiff(a: f64, lo: f64, hi: f64, m: &Angles, flip: &Angles) -> f64 {
    if lo >= 0.0 {
        flip.cdf(a, -lo) - flip.cdf(a, -hi)
    } else {
        m.cdf(a, hi) - m.cdf(a, lo)
    }
}

/// m·e^{ln_c}, through logarithms so that a large coefficient meets a small
/// probability without overflow.
fn scaled(ln_c: f64, m: f64) -> f64 {
    if m == 0.0 {
        0.0
    } else {
        m.signum() * (ln_c + m.abs().ln()).exp()
    }
}

/// The 2002 value of a call with b < r (module documentation), as the sum of
/// its four non-negative groups.
fn call_2002(s: f64, k: f64, t: f64, r: f64, b: f64, sigma: f64) -> f64 {
    let (beta, xb, xs) = triggers(k, t, r, b, sigma);
    if s >= xb {
        return s - k;
    }
    let q = r - b;
    let s2 = sigma * sigma;
    let t1 = SPLIT * t;
    let (v1, vt) = (sigma * t1.sqrt(), sigma * t.sqrt());
    // Logarithms of the ratios of S, K and the triggers: L_X > 0, L_x, L_xX ≥ 0.
    let lx = (xb / s).ln();
    let lsx = (xs / s).ln();
    let lxx = (xb / xs).ln();
    let lk = (k / s).ln();
    // The first-period reflection in X: shift and exponent of the x-side term.
    let c1 = 2.0 * lx / v1;
    let extra_x = -2.0 * lx * lxx / (s2 * t1);
    let mu = |g: f64| b + (g - 0.5) * s2;

    // Exercise at X during [0, t1]: α(X)S^β·(1 − B(β, X)), with λ(β) = 0.
    let mb = mu(beta);
    let kb = 2.0 * mb / s2;
    let d = (lx - mb * t1) / v1;
    let g1 = (xb - k) * (-beta * lx).exp() * (cdf(-d) + reflected(d, c1, kb * lx, 0.0));

    // Exercise at t1 with x < S(t1) ≤ X: the φ terms in γ = 1 and γ = 0.
    let interval = |g: f64| {
        let m = mu(g);
        let kl = 2.0 * m / s2 * lx;
        let (d_big, d_small) = ((lx - m * t1) / v1, (lsx - m * t1) / v1);
        ndiff(d_small, d_big) - (reflected(d_big, c1, kl, 0.0) - reflected(d_small, c1, kl, extra_x))
    };
    let g2 = s * (-q * t1).exp() * interval(1.0) - k * (-r * t1).exp() * interval(0.0);

    // The arguments of ψ for γ and H (as ln(H/S)): −e1..−e4 and −f1..−f4.
    let args = |g: f64, lh: f64| {
        let m = mu(g);
        let e = [(lsx - m * t1) / v1, -(lx + lxx + m * t1) / v1, (lsx + m * t1) / v1, -(lx + lxx - m * t1) / v1];
        let f1 = (lh - m * t) / vt;
        let f = [f1, f1 - 2.0 * lx / vt, f1 - 2.0 * lsx / vt, f1 + 2.0 * lxx / vt];
        (e, f, 2.0 * m / s2)
    };

    // Exercise at x during [t1, T): α(x)·(φ(S,t1,β,x,X) − ψ(S,T,β,x,X,x,t1)), as
    // M(−e1, f1; −ρ) − e^{κL_X}M(−e2, f2; −ρ) + e^{κL_x}M(−e3, −f3; −ρ) − e^{−κL_xX}M(−e4, −f4; −ρ).
    let (plus, minus) = bivariate();
    let (e, f, kap) = args(beta, lsx);
    let lc = (xs - k).ln() - beta * lsx;
    let g3 = scaled(lc, minus.cdf(e[0], -f[0])) - scaled(lc + kap * lx, minus.cdf(e[1], -f[1])) + scaled(lc + kap * lsx, minus.cdf(e[2], f[2])) - scaled(lc - kap * lxx, minus.cdf(e[3], f[3]));

    // The payoff at T with K < S(T) ≤ x: the ψ terms in γ = 1 and γ = 0, as
    // differences over H = K .. x of the four bivariate terms.
    let payoff = |g: f64, lc: f64| {
        let (e, hi, kap) = args(g, lsx);
        let (_, lo, _) = args(g, lk);
        scaled(lc, mdiff(e[0], lo[0], hi[0], plus, minus)) - scaled(lc + kap * lx, mdiff(e[1], lo[1], hi[1], plus, minus)) - scaled(lc + kap * lsx, mdiff(e[2], lo[2], hi[2], minus, plus))
            + scaled(lc - kap * lxx, mdiff(e[3], lo[3], hi[3], minus, plus))
    };
    let g4 = payoff(1.0, s.ln() - q * t) - payoff(0.0, k.ln() - r * t);

    g1 + g2 + g3 + g4
}
