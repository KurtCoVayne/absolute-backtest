//! Option pricing functions: the normal distribution ([`normal`]), European
//! options by generalized Black–Scholes–Merton ([`bsm`]) and Black (1976) on
//! futures ([`b76`]), American options by Bjerksund and Stensland (2002)
//! ([`american`]), and implied volatility ([`iv`]) by Jäckel's "Let's Be
//! Rational" (2015) for European prices and a safeguarded Newton iteration
//! for American ones.
//!
//! Conventions shared by every function:
//! - time to expiry τ in years, ACT/365 fixed ([`years`]), rates and yields
//!   continuously compounded per year, volatility σ per √year;
//! - Greeks per unit of their variable: delta ∂V/∂S (∂V/∂F for Black-76),
//!   gamma ∂²V/∂S², vega ∂V/∂σ, theta ∂V/∂t = −∂V/∂τ per year, rho ∂V/∂r
//!   (the dividend yield held fixed; for Black-76 the futures price);
//! - prices are NaN for a non-positive spot or strike, a negative volatility,
//!   or an input that is NaN or infinite; an expired option (τ ≤ 0) is worth
//!   its intrinsic value.

pub mod american;
pub mod b76;
pub mod bsm;
pub mod iv;
pub mod normal;

/// The right an option gives: to buy (call) or to sell (put).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Right {
    Call,
    Put,
}

impl Right {
    /// ω: +1 for a call, −1 for a put, so that the payoff is max(ω(S − K), 0).
    pub fn sign(self) -> f64 {
        match self {
            Right::Call => 1.0,
            Right::Put => -1.0,
        }
    }

    /// "call" or "put".
    pub fn parse(s: &str) -> Option<Right> {
        match s {
            "call" => Some(Right::Call),
            "put" => Some(Right::Put),
            _ => None,
        }
    }
}

/// When an option may be exercised: at expiry only, or at any time before.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Style {
    European,
    American,
}

impl Style {
    /// "european" or "american".
    pub fn parse(s: &str) -> Option<Style> {
        match s {
            "european" => Some(Style::European),
            "american" => Some(Style::American),
            _ => None,
        }
    }
}

/// The sensitivities of an option's value (see the module documentation for
/// their units).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Greeks {
    pub delta: f64,
    pub gamma: f64,
    pub theta: f64,
    pub vega: f64,
    pub rho: f64,
}

/// Every Greek NaN: the value for invalid inputs.
pub(crate) const NAN_GREEKS: Greeks = Greeks {
    delta: f64::NAN,
    gamma: f64::NAN,
    theta: f64::NAN,
    vega: f64::NAN,
    rho: f64::NAN,
};

/// Nanoseconds in a year of 365 days (ACT/365 fixed); exact in f64.
pub const YEAR_NANOS: f64 = 365.0 * crate::kernel::time::DAY as f64;

/// A duration in nanoseconds as years, ACT/365 fixed: N whole days give
/// exactly `N as f64 / 365.0`.
pub fn years(nanos: i64) -> f64 {
    nanos as f64 / YEAR_NANOS
}
