//! Generalized Black–Scholes–Merton (F. Black and M. Scholes 1973, R. C.
//! Merton 1973): a European option on an asset with a continuous yield q,
//! cost of carry b = r − q (E. G. Haug, The Complete Guide to Option Pricing
//! Formulas, 2nd ed., 2007, §1.1.6 and §2.1):
//!
//! V = ω·(S·e^{−qτ}·N(ωd₁) − K·e^{−rτ}·N(ωd₂)),
//! d₁ = (ln(S/K) + (r − q)τ)/(σ√τ) + ½σ√τ, d₂ = d₁ − σ√τ,
//!
//! with ω = +1 for a call and −1 for a put. With n = N′:
//! - delta = ω·e^{−qτ}·N(ωd₁);
//! - gamma = e^{−qτ}·n(d₁)/(S·σ√τ);
//! - vega = S·e^{−qτ}·n(d₁)·√τ, per unit σ;
//! - theta = −S·e^{−qτ}·n(d₁)·σ/(2√τ) + ω·(q·S·e^{−qτ}·N(ωd₁) − r·K·e^{−rτ}·N(ωd₂)),
//!   ∂V/∂t = −∂V/∂τ per year;
//! - rho = ω·K·τ·e^{−rτ}·N(ωd₂), ∂V/∂r per unit rate with q held fixed.
//!
//! Edge cases:
//! - S ≤ 0, K ≤ 0, σ < 0, or a NaN or infinite input: NaN, Greeks included.
//! - τ ≤ 0 (expired): the intrinsic value max(ω(S − K), 0); delta ω when
//!   strictly in the money, else 0; the other Greeks 0.
//! - σ√τ < [`MIN_STD`] (no diffusion left): the discounted intrinsic value of
//!   the forward F = S·e^{(r−q)τ}, max(ω(S·e^{−qτ} − K·e^{−rτ}), 0). Where the
//!   forward is strictly in the money the Greeks are that function's: delta
//!   ω·e^{−qτ}, theta ω·(q·S·e^{−qτ} − r·K·e^{−rτ}), rho ω·K·τ·e^{−rτ}; elsewhere
//!   0; gamma and vega 0. These are the limits of the closed form as σ√τ → 0
//!   away from F = K. Just above the threshold the closed form already
//!   returns them to the bit, since |d₁| ≈ |ln(F/K)|·1e100 puts N(±d₁) at 0
//!   or 1 unless |ln(F/K)| < ~1e-98; the branch spares the 0/0 of F = K.
//! - Rounding never makes a price negative (it is floored at 0).

use super::normal::{cdf, pdf};
use super::{Greeks, Right, NAN_GREEKS};

/// σ√τ below which a European value is the discounted intrinsic value of the
/// forward.
pub const MIN_STD: f64 = 1e-100;

/// What the inputs leave to evaluate.
enum Case {
    Nan,
    Expired,
    Flat,
    Live { d1: f64, d2: f64 },
}

fn case(s: f64, k: f64, tau: f64, r: f64, q: f64, sigma: f64) -> Case {
    let finite = [s, k, tau, r, q, sigma].iter().all(|x| x.is_finite());
    if !finite || s <= 0.0 || k <= 0.0 || sigma < 0.0 {
        return Case::Nan;
    }
    if tau <= 0.0 {
        return Case::Expired;
    }
    let v = sigma * tau.sqrt();
    if v < MIN_STD {
        return Case::Flat;
    }
    let d1 = ((s / k).ln() + (r - q) * tau) / v + 0.5 * v;
    Case::Live { d1, d2: d1 - v }
}

/// The value of a European option (module documentation).
pub fn price(s: f64, k: f64, tau: f64, r: f64, q: f64, sigma: f64, right: Right) -> f64 {
    let w = right.sign();
    match case(s, k, tau, r, q, sigma) {
        Case::Nan => f64::NAN,
        Case::Expired => (w * (s - k)).max(0.0),
        Case::Flat => (w * (s * (-q * tau).exp() - k * (-r * tau).exp())).max(0.0),
        Case::Live { d1, d2 } => (w * (s * (-q * tau).exp() * cdf(w * d1) - k * (-r * tau).exp() * cdf(w * d2))).max(0.0),
    }
}

/// All five Greeks at once (module documentation).
pub fn greeks(s: f64, k: f64, tau: f64, r: f64, q: f64, sigma: f64, right: Right) -> Greeks {
    let w = right.sign();
    match case(s, k, tau, r, q, sigma) {
        Case::Nan => NAN_GREEKS,
        Case::Expired => Greeks {
            delta: if w * (s - k) > 0.0 { w } else { 0.0 },
            ..Greeks::default()
        },
        Case::Flat => {
            let (dq, dr) = ((-q * tau).exp(), (-r * tau).exp());
            if w * (s * dq - k * dr) > 0.0 {
                Greeks {
                    delta: w * dq,
                    gamma: 0.0,
                    theta: w * (q * s * dq - r * k * dr),
                    vega: 0.0,
                    rho: w * k * tau * dr,
                }
            } else {
                Greeks::default()
            }
        }
        Case::Live { d1, d2 } => {
            let (dq, dr) = ((-q * tau).exp(), (-r * tau).exp());
            let sqrt_tau = tau.sqrt();
            let n1 = pdf(d1);
            let (nd1, nd2) = (cdf(w * d1), cdf(w * d2));
            Greeks {
                delta: w * dq * nd1,
                gamma: dq * n1 / (s * sigma * sqrt_tau),
                theta: -s * dq * n1 * sigma / (2.0 * sqrt_tau) + w * (q * s * dq * nd1 - r * k * dr * nd2),
                vega: s * dq * n1 * sqrt_tau,
                rho: w * k * tau * dr * nd2,
            }
        }
    }
}

/// ∂V/∂S.
pub fn delta(s: f64, k: f64, tau: f64, r: f64, q: f64, sigma: f64, right: Right) -> f64 {
    greeks(s, k, tau, r, q, sigma, right).delta
}

/// ∂²V/∂S².
pub fn gamma(s: f64, k: f64, tau: f64, r: f64, q: f64, sigma: f64, right: Right) -> f64 {
    greeks(s, k, tau, r, q, sigma, right).gamma
}

/// ∂V/∂σ per unit σ.
pub fn vega(s: f64, k: f64, tau: f64, r: f64, q: f64, sigma: f64, right: Right) -> f64 {
    greeks(s, k, tau, r, q, sigma, right).vega
}

/// ∂V/∂t = −∂V/∂τ per year.
pub fn theta(s: f64, k: f64, tau: f64, r: f64, q: f64, sigma: f64, right: Right) -> f64 {
    greeks(s, k, tau, r, q, sigma, right).theta
}

/// ∂V/∂r per unit rate, q held fixed.
pub fn rho(s: f64, k: f64, tau: f64, r: f64, q: f64, sigma: f64, right: Right) -> f64 {
    greeks(s, k, tau, r, q, sigma, right).rho
}
