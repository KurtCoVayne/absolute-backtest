//! Implied volatility: the σ at which the model price equals a given price.
//!
//! European (P. Jäckel, "Let's Be Rational", Wilmott 2015(75) 40–53): the
//! price is normalised to the Black call b(x, s) = e^{x/2}·N(x/s + s/2) −
//! e^{−x/2}·N(x/s − s/2) on x = ln(F/K) with s = σ√τ, F = S·e^{(r−q)τ}. An
//! in-the-money option is replaced by its out-of-the-money counterpart (its
//! price less the intrinsic value e^{−rτ}·|F − K|, by put–call parity) and a
//! put by the symmetry b_put(x) = b_call(−x), so that one solves
//! b(−|x|, s) = β = time value/(e^{−rτ}·√(F·K)) with 0 < β < e^{−|x|/2}.
//! - b is evaluated as in the paper, in four regions of h = x/s and t = s/2:
//!   an asymptotic series where h + t < −10, the Taylor series in t where
//!   t < 0.21, N directly where h + t > 0.85, and otherwise by erfcx, which
//!   keeps the relative accuracy of tiny prices. The series are derived
//!   here rather than transcribed: the asymptotic one is the expansion of the
//!   Mills ratio N(z)/φ(z) at z = h ± t with each difference
//!   (h + t)^{−n} − (h − t)^{−n} formed by `exp_m1`/`ln_1p`, the Taylor one
//!   uses the recurrence Y^{(n+1)} = h·Y^{(n)} + n·Y^{(n−1)} of the Mills
//!   ratio's derivatives; both are summed until their terms are negligible.
//! - The initial guess is the paper's: four branches cut at the inflexion
//!   point s_c = √(2|x|) and the tangents' intersections s_l, s_h; in the
//!   middle two, a rational cubic (Delbourgo–Gregory) interpolation of s(β)
//!   with its second derivative fitted at s_c; in the outer two, a rational
//!   cubic of the transformed f_lower(β) ≈ (2π/√27)|x|·N(−|x|/(√3 s))³ or
//!   f_upper(β) = N(−s/2), inverted in closed form.
//! - Then Householder iterations of order three on the paper's objective
//!   for the branch (1/ln b − 1/ln β in the lowest, ln((b_max − β)/(b_max −
//!   b)) in the highest, b − β otherwise), safeguarded by the bracket the
//!   iterates build, until the price error is at most [`TOLERANCE`]
//!   relative (two iterations as a rule) or the step is below an ulp of s;
//!   should the price error then exceed 1e-12 relative, the result is None
//!   (no test grid meets it). b_max − b is formed without cancellation as
//!   e^{x/2}N(−h−t) + e^{−x/2}N(h−t).
//!
//! The bounds (None outside them, as for τ ≤ 0, S ≤ 0, K ≤ 0 or a non-finite
//! input): a price must be below S·e^{−qτ} (call) or K·e^{−rτ} (put), and not
//! below the discounted intrinsic value of the forward by more than
//! 4 ulps of max(S·e^{−qτ}, K·e^{−rτ}); a price at the lower bound within
//! that rounding gives Some(0.0), the price of σ = 0, as does a time value so
//! small (a few denormals) that β underflows to 0. A denormal β is solved
//! with its error measured against the least normal double.
//!
//! American: [`american::price`]`(σ) = p` on [1e-6, 10] by Newton's method
//! on ln(price/p) (Newton on the price near the root; far from it, deep out
//! of the money where the price spans many orders of magnitude, it does not
//! crawl), with vega by a central difference, safeguarded by bisection when
//! a step leaves the bracket or |ln(price/p)| fails to halve; it stops at a
//! price error of [`TOLERANCE`] relative. The start: because the American
//! price exceeds the European at every σ and both increase with σ, the
//! European implied vol σ_E of p bounds the American one from above; less
//! the early exercise premium at σ_E, the European inversion bounds it from
//! below (as long as the premium increases with σ), and that lower estimate
//! starts the iteration. None outside the American bounds: below the σ = 0
//! price (the deterministic value, at least the intrinsic) by more than 4
//! ulps, or at or above the most an exercise can deliver, S·max(1, e^{−qτ})
//! for a call and K·max(1, e^{−rτ}) for a put (S and K unless the yield or
//! the rate is negative); also None above the price at σ = 10. A price at
//! the σ = 0 value gives Some(0.0); one between it and the price at 1e-6
//! gives Some(1e-6).

use std::f64::consts::{FRAC_1_SQRT_2, PI};

use super::normal::{cdf, erfcx, inv_cdf, mills, FRAC_1_SQRT_2PI};
use super::{american, Right, Style};

/// The relative price error at which the iterations stop.
pub const TOLERANCE: f64 = 1e-13;

/// The volatility at which an option's model price is `p` (module
/// documentation): European by "Let's Be Rational", American by a
/// safeguarded Newton iteration.
#[allow(clippy::too_many_arguments)]
pub fn implied_vol(p: f64, s: f64, k: f64, tau: f64, r: f64, q: f64, right: Right, style: Style) -> Option<f64> {
    let finite = [p, s, k, tau, r, q].iter().all(|x| x.is_finite());
    if !finite || s <= 0.0 || k <= 0.0 || tau <= 0.0 {
        return None;
    }
    match style {
        Style::European => european(p, s, k, tau, r, q, right),
        Style::American => american_vol(p, s, k, tau, r, q, right),
    }
}

fn european(p: f64, s: f64, k: f64, tau: f64, r: f64, q: f64, right: Right) -> Option<f64> {
    // e^{−rτ}·F and e^{−rτ}·K.
    let (f, kd) = (s * (-q * tau).exp(), k * (-r * tau).exp());
    let upper = match right {
        Right::Call => f,
        Right::Put => kd,
    };
    if p >= upper {
        return None;
    }
    let time_value = p - (right.sign() * (f - kd)).max(0.0);
    if time_value < -4.0 * f64::EPSILON * f.max(kd) {
        return None;
    }
    if time_value <= 0.0 {
        return Some(0.0);
    }
    let x = -((s / k).ln() + (r - q) * tau).abs();
    let beta = time_value / (f * kd).sqrt();
    if beta == 0.0 {
        // A time value of a few denormals that normalises to zero: at the bound.
        return Some(0.0);
    }
    let sv = otm_vol(beta, x)?;
    Some(sv / tau.sqrt())
}

// ------------------------------------------------------------ the normalised Black call

/// b(x, s) for x ≤ 0: an out-of-the-money call, in the paper's four regions.
fn black(x: f64, s: f64) -> f64 {
    if s <= 0.0 {
        return 0.0;
    }
    let (h, t) = (x / s, 0.5 * s);
    if h + t < -10.0 {
        asymptotic(h, t)
    } else if t < 0.21 {
        small_t(h, t)
    } else if h + t > 0.85 {
        (0.5 * x).exp() * cdf(h + t) - (-0.5 * x).exp() * cdf(h - t)
    } else {
        0.5 * (-0.5 * (h * h + t * t)).exp() * (erfcx(-(h + t) * FRAC_1_SQRT_2) - erfcx(-(h - t) * FRAC_1_SQRT_2))
    }
}

/// e^{x/2} − b(x, s) for x ≤ 0, without cancellation.
fn black_complement(x: f64, s: f64) -> f64 {
    if s <= 0.0 {
        return (0.5 * x).exp();
    }
    let (h, t) = (x / s, 0.5 * s);
    (0.5 * x).exp() * cdf(-(h + t)) + (-0.5 * x).exp() * cdf(h - t)
}

/// ∂b/∂s = φ(x/s + s/2)·e^{x/2} = e^{−(x²/s² + s²/4)/2}/√(2π).
fn vega(x: f64, s: f64) -> f64 {
    let h = if x == 0.0 { 0.0 } else { x / s };
    FRAC_1_SQRT_2PI * (-0.5 * (h * h + 0.25 * s * s)).exp()
}

/// b = φ₀·(Y(h + t) − Y(h − t)), φ₀ = e^{−(h²+t²)/2}/√(2π), Y(z) = N(z)/φ(z),
/// with Y(z) ~ Σ (−1)^j (2j−1)!!/|z|^{2j+1} for z → −∞ (h + t < −10, so the
/// smallest term is below e^{−50}). With A = −(h + t) and B = −(h − t),
/// A^{−n} − B^{−n} = A^{−n}·(1 − (A/B)^n) and (A/B)^n = e^{n·ln(1 − 2t/B)}.
fn asymptotic(h: f64, t: f64) -> f64 {
    let (a, b) = (-(h + t), t - h);
    let ln_ratio = (-2.0 * t / b).ln_1p();
    let inv_a2 = 1.0 / (a * a);
    let mut c = 1.0 / a;
    let (mut sum, mut last) = (0.0, f64::INFINITY);
    for j in 0..64 {
        let n = (2 * j + 1) as f64;
        let term = c * -(n * ln_ratio).exp_m1();
        if term >= last {
            break;
        }
        sum += if j % 2 == 0 { term } else { -term };
        if term <= 1e-17 * sum {
            break;
        }
        last = term;
        c *= n * inv_a2;
    }
    FRAC_1_SQRT_2PI * (-0.5 * (h * h + t * t)).exp() * sum
}

/// b = 2φ₀·Σ_j t^{2j+1}/(2j+1)!·Y^{(2j+1)}(h), with Y^{(n)} = P_n + Q_n·Y and
/// P, Q from Y^{(n+1)} = h·Y^{(n)} + n·Y^{(n−1)} (Y′ = 1 + h·Y); t < 0.21.
fn small_t(h: f64, t: f64) -> f64 {
    let y = mills(h);
    let w = t * t;
    let (mut p0, mut q0, mut p1, mut q1) = (0.0, 1.0, 1.0, h);
    let mut term = t;
    let mut sum = t * (1.0 + h * y);
    let mut n = 1.0;
    for _ in 0..24 {
        for _ in 0..2 {
            let (p2, q2) = (h * p1 + n * p0, h * q1 + n * q0);
            (p0, q0, p1, q1) = (p1, q1, p2, q2);
            n += 1.0;
        }
        term *= w / ((n - 1.0) * n);
        let add = term * (p1 + q1 * y);
        sum += add;
        if add.abs() <= 1e-17 * sum {
            break;
        }
    }
    2.0 * FRAC_1_SQRT_2PI * (-0.5 * (h * h + w)).exp() * sum
}

// ------------------------------------------------------------ rational cubic interpolation

/// −(1 − √ε), √ε = 2⁻²⁶.
const MIN_RATIONAL_R: f64 = -(1.0 - 1.4901161193847656e-8);
const MAX_RATIONAL_R: f64 = 2.0 / (f64::EPSILON * f64::EPSILON);

/// An interval of the interpolation: the points (x_l, y_l), (x_r, y_r) and
/// the slopes d_l, d_r there (R. Delbourgo and J. A. Gregory, "Shape
/// preserving piecewise rational interpolation", SIAM J. Sci. Stat. Comput.
/// 6 (1985) 967–976).
#[derive(Clone, Copy)]
struct Segment {
    x_l: f64,
    x_r: f64,
    y_l: f64,
    y_r: f64,
    d_l: f64,
    d_r: f64,
}

impl Segment {
    /// The rational cubic at x with control parameter r (r = 3: the cubic
    /// Hermite; r → ∞: the straight line).
    fn at(&self, x: f64, r: f64) -> f64 {
        let Segment { x_l, x_r, y_l, y_r, d_l, d_r } = *self;
        let h = x_r - x_l;
        if h.abs() <= 0.0 {
            return 0.5 * (y_l + y_r);
        }
        let t = (x - x_l) / h;
        if r >= MAX_RATIONAL_R {
            return y_r * t + y_l * (1.0 - t);
        }
        let omt = 1.0 - t;
        let (t2, omt2) = (t * t, omt * omt);
        (y_r * t2 * t + (r * y_r - h * d_r) * t2 * omt + (r * y_l + h * d_l) * t * omt2 + y_l * omt2 * omt) / (1.0 + (r - 3.0) * t * omt)
    }

    /// The r giving second derivative `dd` at x_l (`left`) or at x_r: from
    /// y″(x_l) = 2(r(Δ − d_l) − (d_r − d_l))/h with Δ the chord's slope, and
    /// its mirror image.
    fn fit(&self, dd: f64, left: bool) -> f64 {
        let h = self.x_r - self.x_l;
        let num = 0.5 * h * dd + (self.d_r - self.d_l);
        if num == 0.0 {
            return 0.0;
        }
        let slope = (self.y_r - self.y_l) / h;
        let den = if left { slope - self.d_l } else { self.d_r - slope };
        if den == 0.0 {
            return if num > 0.0 { MAX_RATIONAL_R } else { MIN_RATIONAL_R };
        }
        num / den
    }

    /// The least r that keeps the interpolant monotone and convex (or
    /// concave) where the data are, (3.8) and (3.18) of the paper.
    fn min_r(&self, prefer_shape: bool) -> f64 {
        let (d_l, d_r) = (self.d_l, self.d_r);
        let slope = (self.y_r - self.y_l) / (self.x_r - self.x_l);
        let monotonic = d_l * slope >= 0.0 && d_r * slope >= 0.0;
        let convex = d_l <= slope && slope <= d_r;
        let concave = d_l >= slope && slope >= d_r;
        if !monotonic && !convex && !concave {
            return MIN_RATIONAL_R;
        }
        let (mut r1, mut r2) = (f64::MIN, f64::MIN);
        if monotonic {
            if slope != 0.0 {
                r1 = (d_r + d_l) / slope;
            } else if prefer_shape {
                r1 = MAX_RATIONAL_R;
            }
        }
        if convex || concave {
            let (a, b) = (slope - d_l, d_r - slope);
            if a != 0.0 && b != 0.0 {
                r2 = ((d_r - d_l) / b).abs().max(((d_r - d_l) / a).abs());
            } else if prefer_shape {
                r2 = MAX_RATIONAL_R;
            }
        } else if monotonic && prefer_shape {
            r2 = MAX_RATIONAL_R;
        }
        MIN_RATIONAL_R.max(r1).max(r2)
    }

    /// The interpolant at x with its second derivative fitted to `dd` at one
    /// end, unless that would cost the shape.
    fn convex_at(&self, x: f64, dd: f64, left: bool, prefer_shape: bool) -> f64 {
        self.at(x, self.fit(dd, left).max(self.min_r(prefer_shape)))
    }
}

// ------------------------------------------------------------ the transformed outer branches

const SQRT_3: f64 = 1.7320508075688772;
/// 2π/√27.
const TWO_PI_OVER_SQRT_27: f64 = 1.2091995761561452;
/// √(π/2).
const SQRT_PI_OVER_2: f64 = 1.2533141373155003;

/// f_lower = (2π/√27)·|x|·N(−z)³, z = |x|/(√3·s), and its first two
/// derivatives in β = b(x, s).
fn lower_map(x: f64, s: f64) -> (f64, f64, f64) {
    let ax = x.abs();
    let z = ax / (SQRT_3 * s);
    let y = z * z;
    let s2 = s * s;
    let big_phi = cdf(-z);
    let small_phi = FRAC_1_SQRT_2PI * (-0.5 * y).exp();
    let fpp = PI / 6.0 * y / (s2 * s) * big_phi * (8.0 * SQRT_3 * s * ax + (3.0 * s2 * (s2 - 8.0) - 8.0 * x * x) * big_phi / small_phi) * (2.0 * y + 0.25 * s2).exp();
    let fp = 2.0 * PI * y * big_phi * big_phi * (y + 0.125 * s2).exp();
    let f = TWO_PI_OVER_SQRT_27 * ax * big_phi * big_phi * big_phi;
    (f, fp, fpp)
}

/// s from f_lower, in closed form.
fn inverse_lower_map(x: f64, f: f64) -> f64 {
    if f <= 0.0 {
        return 0.0;
    }
    (x / (SQRT_3 * inv_cdf((f / (TWO_PI_OVER_SQRT_27 * x.abs())).cbrt()))).abs()
}

/// f_upper = N(−s/2) and its first two derivatives in β.
fn upper_map(x: f64, s: f64) -> (f64, f64, f64) {
    let f = cdf(-0.5 * s);
    let w = (x / s) * (x / s);
    (f, -0.5 * (0.5 * w).exp(), SQRT_PI_OVER_2 * (w + 0.125 * s * s).exp() * w / s)
}

// ------------------------------------------------------------ the iteration

#[derive(Clone, Copy, PartialEq, Eq)]
enum Objective {
    /// g = 1/ln b − 1/ln β.
    Lower,
    /// g = b − β.
    Middle,
    /// g = ln((b_max − β)/(b_max − b)).
    Upper,
}

/// s = σ√τ with b(x, s) = β for x ≤ 0 and 0 < β < e^{x/2}: the paper's
/// initial guess and Householder iterations.
fn otm_vol(beta: f64, x: f64) -> Option<f64> {
    otm_vol_steps(beta, x).map(|(s, _)| s)
}

/// [`otm_vol`] with the number of steps the iteration took.
fn otm_vol_steps(beta: f64, x: f64) -> Option<(f64, usize)> {
    let b_max = (0.5 * x).exp();
    if !(beta > 0.0 && beta < b_max) {
        return None;
    }
    let s_c = (2.0 * x.abs()).sqrt();
    let b_c = black(x, s_c);
    let v_c = vega(x, s_c);
    let (s, left, right, objective);
    if beta < b_c {
        let s_l = s_c - b_c / v_c;
        let b_l = black(x, s_l);
        if beta < b_l {
            // f_lower(β) through (0, 0) with slope 1 and (b_l, f_l), its second
            // derivative fitted at b_l; then s in closed form.
            let (f_l, fp_l, fpp_l) = lower_map(x, s_l);
            let segment = Segment {
                x_l: 0.0,
                x_r: b_l,
                y_l: 0.0,
                y_r: f_l,
                d_l: 1.0,
                d_r: fp_l,
            };
            let mut f = segment.convex_at(beta, fpp_l, false, true);
            if f.is_nan() || f <= 0.0 {
                // Quadratic through f(0) = 0 with f′(0) = 1 and f(b_l).
                let t = beta / b_l;
                f = (f_l * t + b_l * (1.0 - t)) * t;
            }
            (s, left, right, objective) = (inverse_lower_map(x, f), 0.0, s_l, Objective::Lower);
        } else {
            // s(β) between (b_l, s_l) and (b_c, s_c), slopes 1/b′, s″ = 0 at the inflexion.
            let v_l = vega(x, s_l);
            let segment = Segment {
                x_l: b_l,
                x_r: b_c,
                y_l: s_l,
                y_r: s_c,
                d_l: 1.0 / v_l,
                d_r: 1.0 / v_c,
            };
            (s, left, right, objective) = (segment.convex_at(beta, 0.0, false, false), s_l, s_c, Objective::Middle);
        }
    } else {
        let s_h = if v_c > f64::MIN_POSITIVE { s_c + (b_max - b_c) / v_c } else { s_c };
        let b_h = black(x, s_h);
        if beta <= b_h {
            let v_h = vega(x, s_h);
            let segment = Segment {
                x_l: b_c,
                x_r: b_h,
                y_l: s_c,
                y_r: s_h,
                d_l: 1.0 / v_c,
                d_r: 1.0 / v_h,
            };
            (s, left, right, objective) = (segment.convex_at(beta, 0.0, true, false), s_c, s_h, Objective::Middle);
        } else {
            // f_upper(β) through (b_h, f_h), its second derivative fitted there,
            // and (b_max, 0) with slope −1/2; then s = −2·N⁻¹(f).
            let (f_h, fp_h, fpp_h) = upper_map(x, s_h);
            let mut f = f64::NAN;
            if fpp_h.is_finite() {
                let segment = Segment {
                    x_l: b_h,
                    x_r: b_max,
                    y_l: f_h,
                    y_r: 0.0,
                    d_l: fp_h,
                    d_r: -0.5,
                };
                f = segment.convex_at(beta, fpp_h, true, true);
            }
            if f.is_nan() || f <= 0.0 {
                // Quadratic through f(b_h), f(b_max) = 0 with f′(b_max) = −1/2.
                let h = b_max - b_h;
                let t = (beta - b_h) / h;
                f = (f_h * (1.0 - t) + 0.5 * h * t) * (1.0 - t);
            }
            let upper = if beta > 0.5 * b_max { Objective::Upper } else { Objective::Middle };
            (s, left, right, objective) = (-2.0 * inv_cdf(f), s_h, f64::INFINITY, upper);
        }
    }
    iterate(beta, x, b_max, s, left, right, objective)
}

fn householder(newton: f64, halley: f64, hh3: f64) -> f64 {
    (1.0 + 0.5 * halley * newton) / (1.0 + newton * (halley + hh3 * newton / 6.0))
}

/// Householder(3) on the branch's objective within the bracket (left, right),
/// bisecting where a step would leave it or after three reversals; s and the
/// number of steps taken.
fn iterate(beta: f64, x: f64, b_max: f64, mut s: f64, mut left: f64, mut right: f64, objective: Objective) -> Option<(f64, usize)> {
    if !(left <= s && s <= right) && right.is_finite() {
        // A guess the interpolation could not place, NaN included (x ≈ 0
        // makes s_l ≈ 0 and 1/b′(s_l) infinite).
        s = 0.5 * (left + right);
    }
    let (mut ds, mut ds_previous, mut reversals, mut steps) = (0.0f64, 0.0f64, 0, 0);
    for i in 0..64 {
        steps = i;
        if ds * ds_previous < 0.0 {
            reversals += 1;
        }
        if i > 0 && (reversals == 3 || !(s > left && s < right)) {
            s = if right.is_finite() { 0.5 * (left + right) } else { 2.0 * left.max(1.0) };
            if right - left <= f64::EPSILON * s {
                break;
            }
            reversals = 0;
            ds = 0.0;
        }
        ds_previous = ds;
        let (b, bp) = (black(x, s), vega(x, s));
        if b > beta && s < right {
            right = s;
        } else if b < beta && s > left {
            left = s;
        }
        if (b - beta).abs() <= TOLERANCE * beta {
            return Some((s, i));
        }
        let bisect = |s: f64| if right.is_finite() { 0.5 * (left + right) - s } else { s };
        let halley_b = (x / s) * (x / s) / s - 0.25 * s;
        let hh3_b = halley_b * halley_b - 3.0 * (x / (s * s)) * (x / (s * s)) - 0.25;
        ds = match objective {
            Objective::Lower if b > 0.0 && bp > 0.0 => {
                let (ln_b, ln_beta, bpob) = (b.ln(), beta.ln(), bp / b);
                let newton = (ln_beta - ln_b) * ln_b / ln_beta / bpob;
                let halley = halley_b - bpob * (1.0 + 2.0 / ln_b);
                let hh3 = hh3_b + 2.0 * bpob * bpob * (1.0 + 3.0 / ln_b * (1.0 + 1.0 / ln_b)) - 3.0 * halley_b * bpob * (1.0 + 2.0 / ln_b);
                newton * householder(newton, halley, hh3)
            }
            Objective::Upper if b < b_max && bp > f64::MIN_POSITIVE => {
                let c = black_complement(x, s);
                let g = ((b_max - beta) / c).ln();
                let gp = bp / c;
                let newton = -g / gp;
                newton * householder(newton, halley_b + gp, hh3_b + gp * (2.0 * gp + 3.0 * halley_b))
            }
            Objective::Middle if bp > 0.0 => {
                let newton = (beta - b) / bp;
                newton * householder(newton, halley_b, hh3_b)
            }
            _ => bisect(s),
        };
        ds = ds.max(-0.5 * s);
        s += ds;
        if ds.abs() <= f64::EPSILON * s {
            steps = i + 1;
            break;
        }
    }
    // A denormal β carries fewer than 52 bits: its error is measured against
    // the least normal double instead.
    let err = (black(x, s) - beta).abs();
    (s.is_finite() && s > 0.0 && err <= 1e-12 * beta.max(f64::MIN_POSITIVE)).then_some((s, steps))
}

// ------------------------------------------------------------ American

const AM_LOW: f64 = 1e-6;
const AM_HIGH: f64 = 10.0;

fn american_vol(p: f64, s: f64, k: f64, tau: f64, r: f64, q: f64, right: Right) -> Option<f64> {
    let price = |sigma: f64| american::price(s, k, tau, r, q, sigma, right);
    // The most any exercise time can deliver: S·max over t of e^{−qt}, or K·e^{−rt}.
    let upper = match right {
        Right::Call => s * (-q * tau).exp().max(1.0),
        Right::Put => k * (-r * tau).exp().max(1.0),
    };
    let floor = price(0.0);
    if p >= upper || p < floor - 4.0 * f64::EPSILON * s.max(k) {
        return None;
    }
    if p <= floor {
        return Some(0.0);
    }
    let (mut lo, mut hi) = (AM_LOW, AM_HIGH);
    let (p_lo, p_hi) = (price(lo), price(hi));
    if p > p_hi {
        return None;
    }
    if p <= p_lo {
        return Some(lo);
    }
    // σ_E(p) is an upper bound; σ_E(p − premium(σ_E)) a lower estimate.
    let guess = match european(p, s, k, tau, r, q, right) {
        Some(se) if se > 0.0 => {
            let premium = price(se) - super::bsm::price(s, k, tau, r, q, se, right);
            european(p - premium, s, k, tau, r, q, right).filter(|g| *g > 0.0).unwrap_or(se)
        }
        // No European implied vol (the price is beyond the European bounds).
        _ => 0.5,
    };
    // Newton on g = ln(price/p), with vega by a central difference: near the
    // root it is Newton on the price, and far from it it does not crawl
    // where the price spans orders of magnitude (deep out of the money).
    // Bisection when the step leaves the bracket or |g| fails to halve.
    let mut sigma = guess.clamp(lo, hi);
    let mut last = f64::INFINITY;
    for _ in 0..100 {
        let value = price(sigma);
        let f = value - p;
        if f.abs() <= TOLERANCE * p.max(f64::MIN_POSITIVE) {
            return Some(sigma);
        }
        if f > 0.0 {
            hi = sigma;
        } else {
            lo = sigma;
        }
        if hi - lo <= 4.0 * f64::EPSILON * hi {
            return Some(sigma);
        }
        let g = (value / p).ln();
        let h = 1e-5 * sigma;
        let vega = (price(sigma + h) - price(sigma - h)) / (2.0 * h);
        let next = sigma - g * value / vega;
        let progress = g.abs() <= 0.5 * last;
        last = g.abs();
        sigma = if progress && vega > 0.0 && next > lo && next < hi { next } else { 0.5 * (lo + hi) };
    }
    Some(sigma)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rows(text: &str) -> Vec<Vec<f64>> {
        text.lines().skip(1).map(|l| l.split(',').map(|x| x.parse().unwrap()).collect()).collect()
    }

    /// b(x, s) and e^{x/2} − b against 80-digit mpmath values (black.csv) in
    /// all four regions, x from −1e-12 to −500 and s from 1e-6 to 50: within
    /// 2e-13 and 1e-15 relative (observed 7.8e-14 and 3.9e-16; the largest
    /// errors are where |h| = |x/s| ≈ 33 and b's condition number in its
    /// arguments is h² ≈ 1100, so that rounding h alone costs ~1e-13).
    #[test]
    fn normalised_black_against_mpmath() {
        let table = rows(include_str!("../../tests/fixtures/pricing/black.csv"));
        assert_eq!(table.len(), 132);
        for v in table {
            let (x, s, b, c) = (v[0], v[1], v[2], v[3]);
            assert!((black(x, s) - b).abs() <= 2e-13 * b, "b({x}, {s}) = {:e}, mpmath {b:e}", black(x, s));
            assert!((black_complement(x, s) - c).abs() <= 1e-15 * c, "e^(x/2) - b({x}, {s})");
        }
    }

    /// ∂b/∂s against a central difference of b.
    #[test]
    fn normalised_vega_against_differences() {
        for (x, s) in [(-0.3, 0.2), (-2.0, 1.1), (0.0, 0.5), (-0.01, 3.0)] {
            let h = 1e-6 * s;
            let fd = (black(x, s + h) - black(x, s - h)) / (2.0 * h);
            assert!((vega(x, s) - fd).abs() <= 1e-8 * fd, "{x} {s}: {} against {fd}", vega(x, s));
        }
    }

    /// The transformed maps' derivatives in β, as derived (f′ = (df/ds)/(db/ds)
    /// and likewise f″ from f′), against differences in s.
    #[test]
    fn outer_map_derivatives() {
        let check = |map: fn(f64, f64) -> (f64, f64, f64), x: f64, s: f64| {
            let h = 1e-5 * s;
            let (bu, bd) = (black(x, s + h), black(x, s - h));
            let ((fu, fpu, _), (fd, fpd, _)) = (map(x, s + h), map(x, s - h));
            let (_, fp, fpp) = map(x, s);
            assert!((fp - (fu - fd) / (bu - bd)).abs() <= 1e-7 * fp.abs(), "f′ at {x} {s}");
            assert!((fpp - (fpu - fpd) / (bu - bd)).abs() <= 1e-6 * fpp.abs(), "f″ at {x} {s}: {fpp} against {}", (fpu - fpd) / (bu - bd));
        };
        for (x, s) in [(-0.5, 0.2), (-2.0, 0.6), (-0.1, 0.05), (-5.0, 1.0)] {
            check(lower_map, x, s);
        }
        for (x, s) in [(-0.5, 3.0), (-2.0, 4.0), (-0.1, 2.5)] {
            check(upper_map, x, s);
        }
    }

    /// The rational cubic meets its end values and slopes, and the fitted r
    /// gives the asked second derivative at either end.
    #[test]
    fn rational_cubic() {
        let segment = Segment {
            x_l: 0.5,
            x_r: 2.0,
            y_l: 1.0,
            y_r: 3.0,
            d_l: 0.4,
            d_r: 2.5,
        };
        let h = 1e-5;
        for r in [0.0, 1.0, 3.0, 10.0] {
            assert!((segment.at(0.5, r) - 1.0).abs() < 1e-15 && (segment.at(2.0, r) - 3.0).abs() < 1e-14);
            let slope = |x: f64| (segment.at(x + h, r) - segment.at(x - h, r)) / (2.0 * h);
            assert!((slope(0.5) - 0.4).abs() < 1e-8 && (slope(2.0) - 2.5).abs() < 1e-8, "r = {r}");
        }
        for (x, left) in [(0.5, true), (2.0, false)] {
            let r = segment.fit(1.7, left);
            let second = (segment.at(x + h, r) - 2.0 * segment.at(x, r) + segment.at(x - h, r)) / (h * h);
            assert!((second - 1.7).abs() < 1e-4, "left {left}: {second}");
        }
    }

    /// Jäckel's claim for the 2015 initial guess: at most two Householder
    /// steps to a price error of 1e-13 relative, in all four branches, over x
    /// from −1e-6 to −10 and s from 0.01 to 4.
    #[test]
    fn two_steps_from_the_rational_guess() {
        let mut branches = [false; 4];
        for x in [-1e-6, -0.01, -0.1, -0.5, -1.0, -2.0, -5.0, -10.0] {
            for s in [0.01, 0.05, 0.1, 0.3, 0.5, 1.0, 2.0, 4.0] {
                let beta = black(x, s);
                if beta < 1e-300 {
                    continue;
                }
                let (got, steps) = otm_vol_steps(beta, x).unwrap_or_else(|| panic!("None at {x} {s}"));
                assert!(steps <= 2, "{steps} steps at x = {x}, s = {s}");
                assert!((black(x, got) - beta).abs() <= 1e-13 * beta, "{x} {s}: {got}");
                // Which branch: below s_l, to s_c, to s_h, above.
                let s_c = (2.0 * x.abs()).sqrt();
                let (b_c, v_c) = (black(x, s_c), vega(x, s_c));
                let s_l = s_c - b_c / v_c;
                let s_h = s_c + ((0.5 * x).exp() - b_c) / v_c;
                branches[[s > s_l, s > s_c, s > s_h].iter().filter(|b| **b).count()] = true;
            }
        }
        assert_eq!(branches, [true; 4]);
    }
}
