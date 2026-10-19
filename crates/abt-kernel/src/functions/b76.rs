//! Black (1976): a European option on a futures (or forward) price F,
//! V = ω·e^{−rτ}·(F·N(ωd₁) − K·N(ωd₂)), d₁ = ln(F/K)/(σ√τ) + ½σ√τ,
//! d₂ = d₁ − σ√τ (F. Black, "The pricing of commodity contracts", J.
//! Financial Economics 3 (1976) 167–179; Haug 2007, §1.1.4).
//!
//! It is generalized Black–Scholes–Merton with S = F and q = r (the futures
//! price has no drift; e^{(r−q)τ} = 1 exactly), so the price, delta (∂V/∂F),
//! gamma (∂²V/∂F²), vega (per unit σ) and theta (∂V/∂t per year with F held
//! fixed, −F·e^{−rτ}·n(d₁)·σ/(2√τ) + r·V) are [`bsm`]'s with those arguments.
//! Rho moves only the discount factor: ∂V/∂r with F fixed is −τ·V.
//!
//! Edge cases are [`bsm`]'s: NaN for F ≤ 0, K ≤ 0, σ < 0 or a NaN or infinite
//! input; for τ ≤ 0 the intrinsic value max(ω(F − K), 0), delta ω strictly in
//! the money and the other Greeks 0; for σ√τ < [`bsm::MIN_STD`] the discounted
//! intrinsic value e^{−rτ}·max(ω(F − K), 0), with delta ω·e^{−rτ} strictly in
//! the money (else 0), gamma and vega 0, theta r·V and rho −τ·V.

use super::{bsm, Greeks, Right};

/// The value of a European option on a futures price.
pub fn price(f: f64, k: f64, tau: f64, r: f64, sigma: f64, right: Right) -> f64 {
    bsm::price(f, k, tau, r, r, sigma, right)
}

/// All five Greeks at once (module documentation).
pub fn greeks(f: f64, k: f64, tau: f64, r: f64, sigma: f64, right: Right) -> Greeks {
    let g = bsm::greeks(f, k, tau, r, r, sigma, right);
    // Expired (or invalid): bsm's rho is already 0 (or NaN).
    let rho = if tau > 0.0 { -tau * price(f, k, tau, r, sigma, right) } else { g.rho };
    Greeks { rho, ..g }
}

/// ∂V/∂F.
pub fn delta(f: f64, k: f64, tau: f64, r: f64, sigma: f64, right: Right) -> f64 {
    greeks(f, k, tau, r, sigma, right).delta
}

/// ∂²V/∂F².
pub fn gamma(f: f64, k: f64, tau: f64, r: f64, sigma: f64, right: Right) -> f64 {
    greeks(f, k, tau, r, sigma, right).gamma
}

/// ∂V/∂σ per unit σ.
pub fn vega(f: f64, k: f64, tau: f64, r: f64, sigma: f64, right: Right) -> f64 {
    greeks(f, k, tau, r, sigma, right).vega
}

/// ∂V/∂t = −∂V/∂τ per year, F held fixed.
pub fn theta(f: f64, k: f64, tau: f64, r: f64, sigma: f64, right: Right) -> f64 {
    greeks(f, k, tau, r, sigma, right).theta
}

/// ∂V/∂r per unit rate, F held fixed: −τ·V.
pub fn rho(f: f64, k: f64, tau: f64, r: f64, sigma: f64, right: Right) -> f64 {
    greeks(f, k, tau, r, sigma, right).rho
}
