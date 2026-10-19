//! The null distributions the tests' p-values read (Q-4.4): the normal,
//! Student's t, χ² and F, through the log-gamma function and the
//! regularised incomplete gamma and beta functions (series and Lentz's
//! continued fractions, as in Press et al., Numerical Recipes, 3rd ed.,
//! 6.1–6.4), to about 1e-14 relative.

/// ln Γ(x) for x > 0 (Lanczos, g = 7, n = 9; Godfrey's coefficients).
pub fn ln_gamma(x: f64) -> f64 {
    const G: f64 = 7.0;
    const C: [f64; 9] = [
        0.999_999_999_999_809_9,
        676.520_368_121_885_1,
        -1_259.139_216_722_402_8,
        771.323_428_777_653_1,
        -176.615_029_162_140_6,
        12.507_343_278_686_905,
        -0.138_571_095_265_720_12,
        9.984_369_578_019_572e-6,
        1.505_632_735_149_311_6e-7,
    ];
    if x < 0.5 {
        // Reflection: Γ(x)Γ(1 − x) = π / sin(πx).
        return (std::f64::consts::PI / (std::f64::consts::PI * x).sin()).ln() - ln_gamma(1.0 - x);
    }
    let x = x - 1.0;
    let mut a = C[0];
    let t = x + G + 0.5;
    for (i, c) in C.iter().enumerate().skip(1) {
        a += c / (x + i as f64);
    }
    0.5 * (2.0 * std::f64::consts::PI).ln() + (x + 0.5) * t.ln() - t + a.ln()
}

const EPS: f64 = 1e-16;
const TINY: f64 = 1e-300;

/// The regularised lower incomplete gamma P(a, x).
pub fn gamma_p(a: f64, x: f64) -> f64 {
    if x <= 0.0 {
        return 0.0;
    }
    if x < a + 1.0 {
        gamma_series(a, x)
    } else {
        1.0 - gamma_cf(a, x)
    }
}

/// The regularised upper incomplete gamma Q(a, x) = 1 − P(a, x).
pub fn gamma_q(a: f64, x: f64) -> f64 {
    if x <= 0.0 {
        return 1.0;
    }
    if x < a + 1.0 {
        1.0 - gamma_series(a, x)
    } else {
        gamma_cf(a, x)
    }
}

fn gamma_series(a: f64, x: f64) -> f64 {
    let mut ap = a;
    let mut sum = 1.0 / a;
    let mut del = sum;
    for _ in 0..10_000 {
        ap += 1.0;
        del *= x / ap;
        sum += del;
        if del.abs() < sum.abs() * EPS {
            break;
        }
    }
    sum * (-x + a * x.ln() - ln_gamma(a)).exp()
}

fn gamma_cf(a: f64, x: f64) -> f64 {
    let mut b = x + 1.0 - a;
    let mut c = 1.0 / TINY;
    let mut d = 1.0 / b;
    let mut h = d;
    for i in 1..10_000 {
        let an = -(i as f64) * (i as f64 - a);
        b += 2.0;
        d = an * d + b;
        if d.abs() < TINY {
            d = TINY;
        }
        c = b + an / c;
        if c.abs() < TINY {
            c = TINY;
        }
        d = 1.0 / d;
        let del = d * c;
        h *= del;
        if (del - 1.0).abs() < EPS {
            break;
        }
    }
    (-x + a * x.ln() - ln_gamma(a)).exp() * h
}

/// The regularised incomplete beta I_x(a, b).
pub fn beta_i(a: f64, b: f64, x: f64) -> f64 {
    if x <= 0.0 {
        return 0.0;
    }
    if x >= 1.0 {
        return 1.0;
    }
    let ln_front = ln_gamma(a + b) - ln_gamma(a) - ln_gamma(b) + a * x.ln() + b * (1.0 - x).ln();
    if x < (a + 1.0) / (a + b + 2.0) {
        ln_front.exp() * beta_cf(a, b, x) / a
    } else {
        1.0 - ln_front.exp() * beta_cf(b, a, 1.0 - x) / b
    }
}

fn beta_cf(a: f64, b: f64, x: f64) -> f64 {
    let (qab, qap, qam) = (a + b, a + 1.0, a - 1.0);
    let mut c = 1.0;
    let mut d = 1.0 - qab * x / qap;
    if d.abs() < TINY {
        d = TINY;
    }
    d = 1.0 / d;
    let mut h = d;
    for m in 1..10_000 {
        let m = m as f64;
        let m2 = 2.0 * m;
        let aa = m * (b - m) * x / ((qam + m2) * (a + m2));
        d = 1.0 + aa * d;
        if d.abs() < TINY {
            d = TINY;
        }
        c = 1.0 + aa / c;
        if c.abs() < TINY {
            c = TINY;
        }
        d = 1.0 / d;
        h *= d * c;
        let aa = -(a + m) * (qab + m) * x / ((a + m2) * (qap + m2));
        d = 1.0 + aa * d;
        if d.abs() < TINY {
            d = TINY;
        }
        c = 1.0 + aa / c;
        if c.abs() < TINY {
            c = TINY;
        }
        d = 1.0 / d;
        let del = d * c;
        h *= del;
        if (del - 1.0).abs() < EPS {
            break;
        }
    }
    h
}

/// P(X > x) for X ~ χ²(k).
pub fn chi2_sf(x: f64, k: f64) -> f64 {
    gamma_q(k / 2.0, x / 2.0)
}

/// P(T > t) for T ~ t(ν).
pub fn t_sf(t: f64, nu: f64) -> f64 {
    let tail = 0.5 * beta_i(nu / 2.0, 0.5, nu / (nu + t * t));
    if t >= 0.0 {
        tail
    } else {
        1.0 - tail
    }
}

/// P(|T| > |t|) for T ~ t(ν).
pub fn t_two_sided(t: f64, nu: f64) -> f64 {
    beta_i(nu / 2.0, 0.5, nu / (nu + t * t))
}

/// The `p` quantile of Student's t(ν), by bisection on `t_sf` to the
/// resolution of a double.
pub fn t_ppf(p: f64, nu: f64) -> f64 {
    if p.is_nan() || nu.is_nan() || nu <= 0.0 {
        return f64::NAN;
    }
    if p <= 0.0 {
        return f64::NEG_INFINITY;
    }
    if p >= 1.0 {
        return f64::INFINITY;
    }
    if p < 0.5 {
        return -t_ppf(1.0 - p, nu);
    }
    let tail = 1.0 - p;
    let mut hi = 1.0;
    while t_sf(hi, nu) > tail && hi < 1e300 {
        hi *= 2.0;
    }
    let mut lo = 0.0;
    for _ in 0..2_000 {
        let mid = 0.5 * (lo + hi);
        if mid <= lo || mid >= hi {
            break;
        }
        if t_sf(mid, nu) > tail {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    0.5 * (lo + hi)
}

/// P(F > f) for F ~ F(d1, d2).
pub fn f_sf(f: f64, d1: f64, d2: f64) -> f64 {
    if f <= 0.0 {
        return 1.0;
    }
    beta_i(d2 / 2.0, d1 / 2.0, d2 / (d2 + d1 * f))
}

/// erfc(x) through the incomplete gamma: erfc(x) = Q(1/2, x²) for x ≥ 0.
pub fn erfc(x: f64) -> f64 {
    if x >= 0.0 {
        gamma_q(0.5, x * x)
    } else {
        1.0 + gamma_p(0.5, x * x)
    }
}

/// The standard normal distribution function.
pub fn norm_cdf(x: f64) -> f64 {
    0.5 * erfc(-x / std::f64::consts::SQRT_2)
}

/// P(Z > z).
pub fn norm_sf(z: f64) -> f64 {
    0.5 * erfc(z / std::f64::consts::SQRT_2)
}

/// The standard normal quantile (Acklam's rational approximation refined by
/// one Halley step on `norm_cdf`).
pub fn norm_ppf(p: f64) -> f64 {
    if p <= 0.0 {
        return f64::NEG_INFINITY;
    }
    if p >= 1.0 {
        return f64::INFINITY;
    }
    const A: [f64; 6] = [
        -3.969_683_028_665_376e1,
        2.209_460_984_245_205e2,
        -2.759_285_104_469_687e2,
        1.383_577_518_672_69e2,
        -3.066_479_806_614_716e1,
        2.506_628_277_459_239,
    ];
    const B: [f64; 5] = [
        -5.447_609_879_822_406e1,
        1.615_858_368_580_409e2,
        -1.556_989_798_598_866e2,
        6.680_131_188_771_972e1,
        -1.328_068_155_288_572e1,
    ];
    const C: [f64; 6] = [
        -7.784_894_002_430_293e-3,
        -3.223_964_580_411_365e-1,
        -2.400_758_277_161_838,
        -2.549_732_539_343_734,
        4.374_664_141_464_968,
        2.938_163_982_698_783,
    ];
    const D: [f64; 4] = [7.784_695_709_041_462e-3, 3.224_671_290_700_398e-1, 2.445_134_137_142_996, 3.754_408_661_907_416];
    let lo = 0.024_25;
    let x = if p < lo {
        let q = (-2.0 * p.ln()).sqrt();
        (((((C[0] * q + C[1]) * q + C[2]) * q + C[3]) * q + C[4]) * q + C[5]) / ((((D[0] * q + D[1]) * q + D[2]) * q + D[3]) * q + 1.0)
    } else if p <= 1.0 - lo {
        let q = p - 0.5;
        let r = q * q;
        (((((A[0] * r + A[1]) * r + A[2]) * r + A[3]) * r + A[4]) * r + A[5]) * q / (((((B[0] * r + B[1]) * r + B[2]) * r + B[3]) * r + B[4]) * r + 1.0)
    } else {
        let q = (-2.0 * (1.0 - p).ln()).sqrt();
        -(((((C[0] * q + C[1]) * q + C[2]) * q + C[3]) * q + C[4]) * q + C[5]) / ((((D[0] * q + D[1]) * q + D[2]) * q + D[3]) * q + 1.0)
    };
    let e = norm_cdf(x) - p;
    let u = e * (2.0 * std::f64::consts::PI).sqrt() * (x * x / 2.0).exp();
    x - u / (1.0 + x * u / 2.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_values() {
        assert!((ln_gamma(10.0) - 362_880f64.ln()).abs() < 1e-13);
        assert!((ln_gamma(0.5) - std::f64::consts::PI.sqrt().ln()).abs() < 1e-14);
        assert!((chi2_sf(3.841_458_820_694_124, 1.0) - 0.05).abs() < 1e-13);
        assert!((chi2_sf(18.307_038_053_275_146, 10.0) - 0.05).abs() < 1e-13);
        assert!((t_two_sided(2.228_138_851_986_274, 10.0) - 0.05).abs() < 1e-13);
        assert!((t_ppf(0.975, 10.0) - 2.228_138_851_986_274).abs() < 1e-12);
        assert!((t_ppf(0.025, 10.0) + 2.228_138_851_986_274).abs() < 1e-12);
        assert!((f_sf(3.325_834_530_413_011, 5.0, 10.0) - 0.05).abs() < 1e-12);
        assert!((norm_cdf(1.959_963_984_540_054) - 0.975).abs() < 1e-14);
        assert!((norm_ppf(0.975) - 1.959_963_984_540_054).abs() < 1e-12);
    }
}
