//! The standard normal distribution: the density, the distribution function,
//! its inverse, and the bivariate distribution function, in double precision.
//!
//! - [`erfc`] and [`erfcx`] (e^{x²}·erfc x) are W. J. Cody's rational
//!   Chebyshev approximations ("Rational Chebyshev approximations for the
//!   error function", Math. Comp. 23 (1969) 631–638) as arranged in his
//!   SPECFUN routine CALERF (1990): near-minimax rationals on |x| ≤ 0.46875,
//!   0.46875 < |x| ≤ 4 and |x| > 4, with e^{−x²} formed as
//!   e^{−y²}·e^{−(x−y)(x+y)} for y = x truncated to a multiple of 1/16, so
//!   that the factor carries no error from rounding x². The coefficients are
//!   Cody's, digit for digit.
//! - [`cdf`]`(x) = ½·erfc(−x/√2)`. Rounding −x/√2 would cost about x²·ε of
//!   relative accuracy in the lower tail (2e-13 at x = −30), so below x = −1
//!   the argument is carried as a double-double (`mul_add` gives the error of
//!   the product) and the first-order correction applied: the tail keeps a few
//!   ulps of relative accuracy until the result underflows (x ≈ −37.5).
//! - [`pdf`] forms x² as a double-double for the same reason.
//! - [`inv_cdf`] is M. J. Wichura's AS241 PPND16 (Appl. Statist. 37 (1988)
//!   477–484), about 1e-16 relative.
//! - [`bivariate_cdf`] is A. Genz's BVND ("Numerical computation of
//!   rectangular bivariate and trivariate normal and t probabilities",
//!   Statistics and Computing 14 (2004) 251–260, and his Fortran TVPACK): the
//!   Drezner–Wesolowsky (1990) integral over the angle asin ρ with 6-, 12- or
//!   20-point Gauss–Legendre for |ρ| < 0.3, < 0.75 and above, and for
//!   |ρ| ≥ 0.925 the integral in 1 − ρ² with its singular part taken out
//!   analytically; about 1e-15 absolute. The nodes and weights are the
//!   Gauss–Legendre values to double precision (Genz prints 16 digits).

use std::f64::consts::{FRAC_1_SQRT_2, FRAC_2_SQRT_PI, PI};

/// 1/√(2π).
pub(crate) const FRAC_1_SQRT_2PI: f64 = 0.3989422804014327;
/// √(2π).
const SQRT_2PI: f64 = 2.5066282746310007;
/// 1/√2 − FRAC_1_SQRT_2: the low part of 1/√2 as a double-double.
const FRAC_1_SQRT_2_LO: f64 = -4.833_646_656_726_457e-17;

// Cody's CALERF, IEEE double: the coefficients of the three intervals and the
// machine-dependent bounds (XSMALL, XBIG, XHUGE, XMAX, XNEG) of his table.
#[allow(clippy::excessive_precision)]
const A: [f64; 5] = [3.16112374387056560e00, 1.13864154151050156e02, 3.77485237685302021e02, 3.20937758913846947e03, 1.85777706184603153e-1];
#[allow(clippy::excessive_precision)]
const B: [f64; 4] = [2.36012909523441209e01, 2.44024637934444173e02, 1.28261652607737228e03, 2.84423683343917062e03];
#[allow(clippy::excessive_precision)]
const C: [f64; 9] = [
    5.64188496988670089e-1,
    8.88314979438837594e00,
    6.61191906371416295e01,
    2.98635138197400131e02,
    8.81952221241769090e02,
    1.71204761263407058e03,
    2.05107837782607147e03,
    1.23033935479799725e03,
    2.15311535474403846e-8,
];
#[allow(clippy::excessive_precision)]
const D: [f64; 8] = [
    1.57449261107098347e01,
    1.17693950891312499e02,
    5.37181101862009858e02,
    1.62138957456669019e03,
    3.29079923573345963e03,
    4.36261909014324716e03,
    3.43936767414372164e03,
    1.23033935480374942e03,
];
#[allow(clippy::excessive_precision)]
const P: [f64; 6] = [
    3.05326634961232344e-1,
    3.60344899949804439e-1,
    1.25781726111229246e-1,
    1.60837851487422766e-2,
    6.58749161529837803e-4,
    1.63153871373020978e-2,
];
#[allow(clippy::excessive_precision)]
const Q: [f64; 5] = [2.56852019228982242e00, 1.87295284992346725e00, 5.27905102951428412e-1, 6.05183413124413191e-2, 2.33520497626869185e-3];
const THRESH: f64 = 0.46875;
const XSMALL: f64 = 1.11e-16;
const XBIG: f64 = 26.543;
const XHUGE: f64 = 6.71e7;
const XMAX: f64 = 2.53e307;
const XNEG: f64 = -26.628;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Erfc,
    Erfcx,
}

/// e^{−y²} as e^{−y₀²}·e^{−(y−y₀)(y+y₀)}, y₀ = y truncated to a multiple of
/// 1/16 (y₀² is exact, and the second factor is close to one).
fn exp_neg_square(y: f64) -> f64 {
    let y0 = (y * 16.0).trunc() / 16.0;
    (-y0 * y0).exp() * (-(y - y0) * (y + y0)).exp()
}

/// Cody's CALERF for erfc (`JINT = 1`) and erfcx (`JINT = 2`).
fn calerf(x: f64, kind: Kind) -> f64 {
    let y = x.abs();
    if y <= THRESH {
        let ysq = if y > XSMALL { y * y } else { 0.0 };
        let mut num = A[4] * ysq;
        let mut den = ysq;
        for i in 0..3 {
            num = (num + A[i]) * ysq;
            den = (den + B[i]) * ysq;
        }
        let erf = x * (num + A[3]) / (den + B[3]);
        return match kind {
            Kind::Erfc => 1.0 - erf,
            Kind::Erfcx => ysq.exp() * (1.0 - erf),
        };
    }
    // erfc(|x|), or erfcx(|x|) when asked for.
    let r = if y <= 4.0 {
        let mut num = C[8] * y;
        let mut den = y;
        for i in 0..7 {
            num = (num + C[i]) * y;
            den = (den + D[i]) * y;
        }
        let r = (num + C[7]) / (den + D[7]);
        match kind {
            Kind::Erfc => r * exp_neg_square(y),
            Kind::Erfcx => r,
        }
    } else if y >= XBIG && (kind == Kind::Erfc || y >= XMAX) {
        0.0
    } else if y >= XHUGE {
        0.5 * FRAC_2_SQRT_PI / y
    } else {
        let ysq = 1.0 / (y * y);
        let mut num = P[5] * ysq;
        let mut den = ysq;
        for i in 0..4 {
            num = (num + P[i]) * ysq;
            den = (den + Q[i]) * ysq;
        }
        let r = (0.5 * FRAC_2_SQRT_PI - ysq * (num + P[4]) / (den + Q[4])) / y;
        match kind {
            Kind::Erfc => r * exp_neg_square(y),
            Kind::Erfcx => r,
        }
    };
    if x >= 0.0 || x.is_nan() {
        return r;
    }
    match kind {
        Kind::Erfc => 2.0 - r,
        Kind::Erfcx if x < XNEG => f64::INFINITY,
        Kind::Erfcx => {
            let y0 = (x * 16.0).trunc() / 16.0;
            let e = (y0 * y0).exp() * ((x - y0) * (x + y0)).exp();
            (e + e) - r
        }
    }
}

/// The complementary error function erfc x = (2/√π)∫ₓ^∞ e^{−t²} dt (Cody);
/// 0 from x ≈ 26.5, where it underflows.
pub fn erfc(x: f64) -> f64 {
    calerf(x, Kind::Erfc)
}

/// The scaled complementary error function e^{x²}·erfc x (Cody); +∞ below
/// x ≈ −26.6, where it overflows.
pub fn erfcx(x: f64) -> f64 {
    calerf(x, Kind::Erfcx)
}

/// The Mills ratio of the lower tail, Φ(z)/φ(z) = √(π/2)·erfcx(−z/√2): in
/// (0, 1.26] for z ≤ 0, where it is ~1/|z|.
pub(crate) fn mills(z: f64) -> f64 {
    1.2533141373155003 * erfcx(-z * FRAC_1_SQRT_2)
}

/// The standard normal density e^{−x²/2}/√(2π).
pub fn pdf(x: f64) -> f64 {
    if x.is_nan() {
        return f64::NAN;
    }
    if x.abs() > 40.0 {
        return 0.0;
    }
    // x² = hi + lo exactly; e^{−lo/2} ≈ 1 − lo/2.
    let hi = x * x;
    let lo = x.mul_add(x, -hi);
    FRAC_1_SQRT_2PI * (-0.5 * hi).exp() * (1.0 - 0.5 * lo)
}

/// The standard normal distribution function Φ(x) = ½·erfc(−x/√2).
pub fn cdf(x: f64) -> f64 {
    if x >= -1.0 {
        return 0.5 * erfc(-x * FRAC_1_SQRT_2);
    }
    if x.is_nan() {
        return f64::NAN;
    }
    // −x/√2 = z + dz as a double-double, then
    // erfc(z + dz) = erfc(z)·(1 − dz·(2/√π)/erfcx(z) + O(dz²)).
    let z = -x * FRAC_1_SQRT_2;
    if z > 28.0 {
        return 0.0;
    }
    let dz = (-x).mul_add(FRAC_1_SQRT_2, -z) - x * FRAC_1_SQRT_2_LO;
    let ex = erfcx(z);
    0.5 * ex * exp_neg_square(z) * (1.0 - dz * FRAC_2_SQRT_PI / ex)
}

/// The standard normal quantile Φ⁻¹(p) by Wichura's AS241 (PPND16); −∞ at 0,
/// +∞ at 1, NaN outside [0, 1].
#[allow(clippy::excessive_precision)]
pub fn inv_cdf(p: f64) -> f64 {
    if !(0.0..=1.0).contains(&p) {
        return f64::NAN;
    }
    let q = p - 0.5;
    if q.abs() <= 0.425 {
        let r = 0.180625 - q * q;
        let num = ((((((2.5090809287301226727e3 * r + 3.3430575583588128105e4) * r + 6.7265770927008700853e4) * r + 4.5921953931549871457e4) * r + 1.3731693765509461125e4) * r
            + 1.9715909503065514427e3)
            * r
            + 1.3314166789178437745e2)
            * r
            + 3.3871328727963666080e0;
        let den = ((((((5.2264952788528545610e3 * r + 2.8729085735721942674e4) * r + 3.9307895800092710610e4) * r + 2.1213794301586595867e4) * r + 5.3941960214247511077e3) * r
            + 6.8718700749205790830e2)
            * r
            + 4.2313330701600911252e1)
            * r
            + 1.0;
        return q * num / den;
    }
    let tail = if q < 0.0 { p } else { 1.0 - p };
    if tail <= 0.0 {
        return if q < 0.0 { f64::NEG_INFINITY } else { f64::INFINITY };
    }
    let r = (-tail.ln()).sqrt();
    let z = if r <= 5.0 {
        let r = r - 1.6;
        let num = ((((((7.74545014278341407640e-4 * r + 2.27238449892691845833e-2) * r + 2.41780725177450611770e-1) * r + 1.27045825245236838258e0) * r + 3.64784832476320460504e0) * r
            + 5.76949722146069140550e0)
            * r
            + 4.63033784615654529590e0)
            * r
            + 1.42343711074968357734e0;
        let den = ((((((1.05075007164441684324e-9 * r + 5.47593808499534494600e-4) * r + 1.51986665636164571966e-2) * r + 1.48103976427480074590e-1) * r + 6.89767334985100004550e-1) * r
            + 1.67638483018380384940e0)
            * r
            + 2.05319162663775882187e0)
            * r
            + 1.0;
        num / den
    } else {
        let r = r - 5.0;
        let num = ((((((2.01033439929228813265e-7 * r + 2.71155556874348757815e-5) * r + 1.24266094738807843860e-3) * r + 2.65321895265761230930e-2) * r + 2.96560571828504891230e-1) * r
            + 1.78482653991729133580e0)
            * r
            + 5.46378491116411436990e0)
            * r
            + 6.65790464350110377720e0;
        let den = ((((((2.04426310338993978564e-15 * r + 1.42151175831644588870e-7) * r + 1.84631831751005468180e-5) * r + 7.86869131145613259100e-4) * r + 1.48753612908506148525e-2) * r
            + 1.36929880922735805310e-1)
            * r
            + 5.99832206555887937690e-1)
            * r
            + 1.0;
        num / den
    };
    if q < 0.0 {
        -z
    } else {
        z
    }
}

// Gauss–Legendre nodes (the negative half) and weights on [−1, 1].
const X6: [f64; 3] = [-0.932469514203152, -0.6612093864662645, -0.2386191860831969];
const W6: [f64; 3] = [0.17132449237917036, 0.3607615730481386, 0.46791393457269104];
const X12: [f64; 6] = [
    -0.9815606342467192,
    -0.9041172563704749,
    -0.7699026741943047,
    -0.5873179542866175,
    -0.3678314989981802,
    -0.1252334085114689,
];
const W12: [f64; 6] = [
    0.04717533638651183,
    0.10693932599531843,
    0.16007832854334622,
    0.20316742672306592,
    0.2334925365383548,
    0.24914704581340277,
];
const X20: [f64; 10] = [
    -0.9931285991850949,
    -0.9639719272779138,
    -0.912234428251326,
    -0.8391169718222188,
    -0.7463319064601508,
    -0.636053680726515,
    -0.5108670019508271,
    -0.37370608871541955,
    -0.22778585114164507,
    -0.07652652113349734,
];
const W20: [f64; 10] = [
    0.017614007139152118,
    0.04060142980038694,
    0.06267204833410907,
    0.08327674157670475,
    0.10193011981724044,
    0.11819453196151841,
    0.13168863844917664,
    0.14209610931838204,
    0.14917298647260374,
    0.15275338713072584,
];

/// P(X ≤ h, Y ≤ k) for a standard bivariate normal (X, Y) with correlation
/// ρ ∈ [−1, 1] (Genz's BVND); NaN for a NaN argument or |ρ| > 1.
pub fn bivariate_cdf(h: f64, k: f64, rho: f64) -> f64 {
    if h.is_nan() || k.is_nan() || rho.is_nan() || rho.abs() > 1.0 {
        return f64::NAN;
    }
    if h == f64::NEG_INFINITY || k == f64::NEG_INFINITY {
        return 0.0;
    }
    if h == f64::INFINITY {
        return cdf(k);
    }
    if k == f64::INFINITY {
        return cdf(h);
    }
    // By the symmetry (X, Y) → (−X, −Y), P(X ≤ h, Y ≤ k) = P(X > −h, Y > −k).
    bvnd(-h, -k, rho)
}

fn rule(r: f64) -> (&'static [f64], &'static [f64]) {
    if r.abs() < 0.3 {
        (&X6, &W6)
    } else if r.abs() < 0.75 {
        (&X12, &W12)
    } else {
        (&X20, &W20)
    }
}

/// The Drezner–Wesolowsky quadrature of BVND for one correlation with
/// |ρ| < 0.925: the sines of its angles, which depend on ρ alone, so that a
/// caller with a fixed ρ (Bjerksund–Stensland's √(t1/T)) computes them once.
/// It gives [`bivariate_cdf`]'s values to the bit.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Angles {
    asr: f64,
    n: usize,
    w: [f64; 20],
    sn: [f64; 20],
}

impl Angles {
    pub(crate) fn new(r: f64) -> Angles {
        debug_assert!(r.abs() < 0.925);
        let asr = r.asin();
        let (xs, ws) = rule(r);
        let (mut w, mut sn) = ([0.0; 20], [0.0; 20]);
        for (i, (&x, &wi)) in xs.iter().zip(ws).enumerate() {
            (w[2 * i], sn[2 * i]) = (wi, (asr * (x + 1.0) / 2.0).sin());
            (w[2 * i + 1], sn[2 * i + 1]) = (wi, (asr * (1.0 - x) / 2.0).sin());
        }
        Angles { asr, n: 2 * xs.len(), w, sn }
    }

    /// P(X ≤ h, Y ≤ k), as [`bivariate_cdf`] at this correlation.
    pub(crate) fn cdf(&self, h: f64, k: f64) -> f64 {
        if h == f64::NEG_INFINITY || k == f64::NEG_INFINITY {
            0.0
        } else if h == f64::INFINITY {
            cdf(k)
        } else if k == f64::INFINITY {
            cdf(h)
        } else {
            self.upper(-h, -k)
        }
    }

    /// P(X > h, Y > k).
    fn upper(&self, h: f64, k: f64) -> f64 {
        let hk = h * k;
        let hs = (h * h + k * k) / 2.0;
        let mut sum = 0.0;
        for (&w, &sn) in self.w[..self.n].iter().zip(&self.sn[..self.n]) {
            sum += w * ((sn * hk - hs) / (1.0 - sn * sn)).exp();
        }
        sum * self.asr / (4.0 * PI) + cdf(-h) * cdf(-k)
    }
}

/// Genz's BVND: P(X > h, Y > k).
fn bvnd(h: f64, k: f64, r: f64) -> f64 {
    if r.abs() < 0.925 {
        return Angles::new(r).upper(h, k);
    }
    let (xs, ws) = rule(r);
    let k = if r < 0.0 { -k } else { k };
    let hk = h * k;
    let mut bvn = 0.0;
    if r.abs() < 1.0 {
        let a2 = (1.0 - r) * (1.0 + r);
        let a = a2.sqrt();
        let bs = (h - k) * (h - k);
        let c = (4.0 - hk) / 8.0;
        let d = (12.0 - hk) / 16.0;
        bvn = a * (-(bs / a2 + hk) / 2.0).exp() * (1.0 - c * (bs - a2) * (1.0 - d * bs / 5.0) / 3.0 + c * d * a2 * a2 / 5.0);
        if hk > -160.0 {
            let b = bs.sqrt();
            bvn -= (-hk / 2.0).exp() * SQRT_2PI * cdf(-b / a) * b * (1.0 - c * bs * (1.0 - d * bs / 5.0) / 3.0);
        }
        let a = a / 2.0;
        for (&x, &w) in xs.iter().zip(ws) {
            for x in [x, -x] {
                // The two forms of Genz's loop are the same expression; this is the factored one.
                let xs = (a * (x + 1.0)) * (a * (x + 1.0));
                let rs = (1.0 - xs).sqrt();
                bvn += a * w * (-(bs / xs + hk) / 2.0).exp() * ((-hk * xs / (2.0 * (1.0 + rs) * (1.0 + rs))).exp() / rs - (1.0 + c * xs * (1.0 + d * xs)));
            }
        }
        bvn = -bvn / (2.0 * PI);
    }
    if r > 0.0 {
        return bvn + cdf(-h.max(k));
    }
    let mut v = -bvn;
    if k > h {
        v += if h < 0.0 { cdf(k) - cdf(h) } else { cdf(-h) - cdf(-k) };
    }
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The quadrature with its angles computed once gives bivariate_cdf's
    /// values to the bit (Bjerksund–Stensland relies on it).
    #[test]
    fn angles_match_bivariate_cdf() {
        let args = [f64::NEG_INFINITY, -30.0, -5.0, -1.0, 0.0, 0.3, 2.0, 8.0, f64::INFINITY];
        for rho in [-0.92, -0.7861513777574233, -0.5, -0.2, 0.0, 0.1, 0.4, 0.7861513777574233, 0.92] {
            let angles = Angles::new(rho);
            for h in args {
                for k in args {
                    assert_eq!(angles.cdf(h, k).to_bits(), bivariate_cdf(h, k, rho).to_bits(), "{h} {k} {rho}");
                }
            }
        }
    }
}
