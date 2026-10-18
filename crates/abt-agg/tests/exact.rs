//! The `exact` numerics (docs/spec/01-data.md 6.3; 07-storage.md,
//! Proposition 3.3): tick columns reduced as integers, every `f64` the
//! correctly rounded value of an exact rational, and the result independent
//! of the order the rows are read in. The oracle for correct rounding is an
//! independent big integer of this file (comparisons only, no division):
//! a value `v` is the nearest `f64` to `p / q` iff `|2p − 2vq| ≤ ulp(v)·q`,
//! with ties going to the even mantissa.

use abt_agg::exact::{align, pow10, ratio_to_f64, reduce_ticks, tick_to_f64, ExactSums, Int, Ticks, Wide};
use abt_agg::{reduce, Function, Mode};

/// An unsigned big integer, little-endian `u32` limbs: the oracle's.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Big(Vec<u32>);

impl Big {
    fn from_u128(mut x: u128) -> Big {
        let mut v = Vec::new();
        while x > 0 {
            v.push(x as u32);
            x >>= 32;
        }
        Big(v)
    }
    fn trim(mut self) -> Big {
        while self.0.last() == Some(&0) {
            self.0.pop();
        }
        self
    }
    fn mul(&self, o: &Big) -> Big {
        let mut out = vec![0u64; self.0.len() + o.0.len() + 1];
        for (i, &a) in self.0.iter().enumerate() {
            let mut carry = 0u64;
            for (j, &b) in o.0.iter().enumerate() {
                let t = out[i + j] + a as u64 * b as u64 + carry;
                out[i + j] = t & 0xFFFF_FFFF;
                carry = t >> 32;
            }
            let mut k = i + o.0.len();
            while carry > 0 {
                let t = out[k] + carry;
                out[k] = t & 0xFFFF_FFFF;
                carry = t >> 32;
                k += 1;
            }
        }
        Big(out.into_iter().map(|x| x as u32).collect()).trim()
    }
    fn shl(&self, bits: u32) -> Big {
        self.mul(&Big::from_u128(1).shl_small(bits))
    }
    fn shl_small(&self, bits: u32) -> Big {
        let mut b = self.clone();
        for _ in 0..bits {
            b = b.mul(&Big(vec![2]));
        }
        b
    }
    fn cmp(&self, o: &Big) -> std::cmp::Ordering {
        let (a, b) = (self.clone().trim(), o.clone().trim());
        a.0.len().cmp(&b.0.len()).then_with(|| a.0.iter().rev().cmp(b.0.iter().rev()))
    }
}

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

/// Whether `v` is the nearest `f64` to `p / q` (`p` with its sign, `q > 0`),
/// by exact comparison: `v = m · 2^e` with `m` the 53-bit mantissa.
fn nearest(v: f64, negative: bool, p: u128, q: u128) -> bool {
    if p == 0 {
        return v == 0.0;
    }
    if v.is_sign_negative() != negative || v == 0.0 {
        return false;
    }
    let bits = v.abs().to_bits();
    let exp = ((bits >> 52) & 0x7FF) as i32;
    assert!(exp > 0, "subnormals are not exercised");
    let m = (bits & ((1u64 << 52) - 1)) | (1u64 << 52);
    let e = exp - 1075; // v = m · 2^e
                        // Compare 2p against (2m ± 1)·2^e·q: scale both sides to integers.
    let (p2, lo, hi, q) = (Big::from_u128(p).shl(1), Big::from_u128(2 * m as u128 - 1), Big::from_u128(2 * m as u128 + 1), Big::from_u128(q));
    let (p2, lo, hi) = if e >= 0 {
        (p2, lo.shl(e as u32).mul(&q), hi.shl(e as u32).mul(&q))
    } else {
        (p2.shl((-e) as u32), lo.mul(&q), hi.mul(&q))
    };
    use std::cmp::Ordering::*;
    match (p2.cmp(&lo), p2.cmp(&hi)) {
        (Greater, Less) => true,
        (Equal, _) => m.is_multiple_of(2),
        (_, Equal) => m.is_multiple_of(2),
        _ => false,
    }
}

#[test]
fn the_wide_integer_agrees_with_u128_where_both_fit() {
    let mut r = Rng(11);
    for _ in 0..2_000 {
        let (a, b) = (r.next() as u128 * (r.below(1 << 20) + 1) as u128, r.below(1 << 40) as u128 + 1);
        let w = Wide::from_u128(a);
        assert_eq!(w.mul_u128(b).low_u128(), a.wrapping_mul(b));
        let (q, rem) = w.divrem(&Wide::from_u128(b));
        assert_eq!((q.low_u128(), rem.low_u128()), (a / b, a % b));
        assert_eq!(w.add(&Wide::from_u128(b)).low_u128(), a + b);
        assert_eq!(w.shl(7).shr(7), w);
        assert_eq!(w.bits(), 128 - a.leading_zeros());
        let (x, y) = (r.next() as i128 - (1 << 63), r.next() as i128 - (1 << 63));
        let p = Int::product(x, y);
        let exact = x * y;
        if x.unsigned_abs() < 1 << 62 && y.unsigned_abs() < 1 << 62 {
            assert_eq!((p.negative, p.magnitude.low_u128()), (exact < 0, exact.unsigned_abs()));
        }
        assert_eq!(Int::from_i128(x).add(&Int::from_i128(y)), Int::from_i128(x + y));
        assert_eq!(Int::from_i128(x).sub(&Int::from_i128(y)), Int::from_i128(x - y));
    }
}

#[test]
fn a_ratio_is_the_correctly_rounded_division() {
    let mut r = Rng(5);
    // Small operands: IEEE division of exact operands is the oracle.
    for _ in 0..5_000 {
        let (p, q) = (r.below(1 << 53) as u128, r.below(1 << 53) as u128 + 1);
        let v = ratio_to_f64(&Int::from_i128(p as i128), &Wide::from_u128(q));
        assert_eq!(v, p as f64 / q as f64, "{} / {}", p, q);
        assert_eq!(ratio_to_f64(&Int::from_i128(-(p as i128)), &Wide::from_u128(q)), -(p as f64 / q as f64));
    }
    // Large operands: the comparison oracle, including exact ties.
    for _ in 0..3_000 {
        let kind = r.below(4);
        // A denominator of up to 94 bits for a random numerator (p below
        // 2^108), of up to 60 bits where the numerator is a multiple of it.
        let q = if kind == 0 {
            r.next() as u128 * (r.below(1 << 30) + 1) as u128 + 1
        } else {
            r.below(1 << 60) as u128 + 1
        };
        let p = match kind {
            // A random numerator.
            0 => r.next() as u128 * (r.next() as u128 >> 20),
            // An exact f64 times q: the ratio is representable.
            1 => (r.below(1 << 53) as u128) * q,
            // A tie: (2m + 1) · q / 2 with m random; q even makes it an integer.
            2 => (2 * r.below(1 << 52) as u128 + 1) * (q / 2),
            // Just above a tie.
            _ => (2 * r.below(1 << 52) as u128 + 1) * (q / 2) + 1,
        };
        let q = if kind >= 2 { q / 2 * 2 } else { q };
        if q == 0 {
            continue;
        }
        let v = ratio_to_f64(&Int::from_i128(p as i128), &Wide::from_u128(q));
        assert!(nearest(v, false, p, q), "{} / {} gave {}", p, q, v);
        let v = ratio_to_f64(&Int::from_i128(-(p as i128)), &Wide::from_u128(q));
        assert!(nearest(v, true, p, q), "-{} / {} gave {}", p, q, v);
    }
    // Views of ticks.
    assert_eq!(tick_to_f64(12_345, 2), 123.45);
    assert_eq!(tick_to_f64(-1, 9), -1e-9);
    assert_eq!(align(25, 2, 4), 2_500);
    assert_eq!(pow10(0), 1);
}

/// Random tick windows: units below 10^9 at a scale up to 6, with a second
/// column for the bivariate functions.
fn window(r: &mut Rng, n: usize) -> (Vec<i128>, Vec<i128>, u32, u32) {
    let (sx, sy) = (r.below(7) as u32, r.below(7) as u32);
    let xs: Vec<i128> = (0..n).map(|_| r.below(1_000_000_000) as i128 - 200_000_000).collect();
    let ys: Vec<i128> = (0..n).map(|_| r.below(1_000_000) as i128).collect();
    (xs, ys, sx, sy)
}

/// The exact rational of a function over a window, with `i128` numerators
/// (the windows are small enough), as (sign, |num|, den).
fn rational(f: Function, xs: &[i128], ys: &[i128], sx: u32, sy: u32) -> Option<(bool, u128, u128)> {
    let n = xs.len() as i128;
    let sum = |v: &[i128]| v.iter().sum::<i128>();
    let (s1, s2) = (sum(xs), sum(ys));
    let sxx: i128 = xs.iter().map(|x| x * x).sum();
    let syy: i128 = ys.iter().map(|y| y * y).sum();
    let sxy: i128 = xs.iter().zip(ys).map(|(x, y)| x * y).sum();
    let p10 = |s: u32| 10i128.pow(s);
    let (num, den) = match f {
        Function::Sum => (s1, p10(sx)),
        Function::Mean => (s1, n * p10(sx)),
        Function::Var => (n * sxx - s1 * s1, n * (n - 1) * p10(2 * sx)),
        Function::Cov => (n * sxy - s1 * s2, n * (n - 1) * p10(sx + sy)),
        Function::OlsSlope => {
            let (c, d) = (n * sxy - s1 * s2, n * syy - s2 * s2);
            if d == 0 {
                return None;
            }
            let (c, d) = if sy >= sx { (c * p10(sy - sx), d) } else { (c, d * p10(sx - sy)) };
            if d < 0 {
                (-c, -d)
            } else {
                (c, d)
            }
        }
        _ => return None,
    };
    Some((num < 0, num.unsigned_abs(), den as u128))
}

#[test]
fn exact_reductions_are_the_rounded_rationals_and_do_not_depend_on_order() {
    let mut r = Rng(3);
    for _ in 0..400 {
        let n = 2 + r.below(60) as usize;
        let (xs, ys, sx, sy) = window(&mut r, n);
        let cols = [Ticks { units: &xs, scale: sx }, Ticks { units: &ys, scale: sy }];
        for f in [Function::Sum, Function::Mean, Function::Var, Function::Cov, Function::OlsSlope] {
            let v = reduce_ticks(f, &cols).unwrap().unwrap();
            let (neg, p, q) = rational(f, &xs, &ys, sx, sy).unwrap();
            assert!(
                nearest(v, neg, p, q),
                "{:?} over {} rows at scales {} {}: {} is not the nearest to {}{}/{}",
                f,
                n,
                sx,
                sy,
                v,
                if neg { "-" } else { "" },
                p,
                q
            );
            // Any order: the same bits.
            let mut idx: Vec<usize> = (0..n).collect();
            for i in (1..n).rev() {
                idx.swap(i, r.below(i as u64 + 1) as usize);
            }
            let (px, py): (Vec<i128>, Vec<i128>) = (idx.iter().map(|&i| xs[i]).collect(), idx.iter().map(|&i| ys[i]).collect());
            let w = reduce_ticks(f, &[Ticks { units: &px, scale: sx }, Ticks { units: &py, scale: sy }]).unwrap().unwrap();
            assert_eq!(v.to_bits(), w.to_bits(), "{:?} depends on the order", f);
        }
        // Std is the square root of the exact variance; count, min, max,
        // first, last and the median select units.
        let var = reduce_ticks(Function::Var, &cols).unwrap().unwrap();
        assert_eq!(reduce_ticks(Function::Std, &cols).unwrap().unwrap(), var.sqrt());
        assert_eq!(reduce_ticks(Function::Count, &cols).unwrap(), Some(n as f64));
        assert_eq!(reduce_ticks(Function::Min, &cols).unwrap().unwrap(), tick_to_f64(*xs.iter().min().unwrap(), sx));
        assert_eq!(reduce_ticks(Function::Max, &cols).unwrap().unwrap(), tick_to_f64(*xs.iter().max().unwrap(), sx));
        assert_eq!(reduce_ticks(Function::First, &cols).unwrap().unwrap(), tick_to_f64(xs[0], sx));
        assert_eq!(reduce_ticks(Function::Last, &cols).unwrap().unwrap(), tick_to_f64(xs[n - 1], sx));
        let mut s = xs.clone();
        s.sort_unstable();
        let median = reduce_ticks(Function::Median, &cols).unwrap().unwrap();
        if n % 2 == 1 {
            assert_eq!(median, tick_to_f64(s[n / 2], sx));
        } else {
            let (neg, p, q) = ((s[n / 2 - 1] + s[n / 2]) < 0, (s[n / 2 - 1] + s[n / 2]).unsigned_abs(), 2 * 10u128.pow(sx));
            assert!(nearest(median, neg, p, q));
        }
    }
    // Empty and short windows refuse as the other modes do.
    assert_eq!(reduce_ticks(Function::Sum, &[Ticks { units: &[], scale: 2 }]).unwrap(), None);
    assert_eq!(reduce_ticks(Function::Count, &[]).unwrap(), Some(0.0));
    assert!(reduce_ticks(Function::Var, &[Ticks { units: &[5], scale: 2 }]).is_err());
    assert!(reduce_ticks(Function::Cov, &[Ticks { units: &[5, 6], scale: 2 }]).is_err());
}

#[test]
fn sliding_exact_sums_equal_the_sums_of_the_rows_they_hold() {
    let mut r = Rng(9);
    let (xs, ys, sx, sy) = window(&mut r, 500);
    let mut sums = ExactSums::default();
    let w = 37;
    for i in 0..xs.len() {
        sums.push(xs[i], ys[i]);
        if i >= w {
            sums.evict(xs[i - w], ys[i - w]);
        }
        if i >= w + 5 {
            let (lo, hi) = (i + 1 - w, i + 1);
            let cols = [Ticks { units: &xs[lo..hi], scale: sx }, Ticks { units: &ys[lo..hi], scale: sy }];
            for f in [
                Function::Sum,
                Function::Mean,
                Function::Var,
                Function::Std,
                Function::Cov,
                Function::Corr,
                Function::OlsSlope,
                Function::OlsR,
            ] {
                let whole = reduce_ticks(f, &cols).unwrap();
                let slid = sums.value(f, sx, sy).unwrap();
                assert_eq!(whole.map(f64::to_bits), slid.map(f64::to_bits), "{:?} at {}", f, i);
            }
        }
    }
    // Σ x² of 10^5 rows of large prices passes i128: the wide sums hold it.
    let mut big = ExactSums::default();
    let x = 999_999_999_999_999_999i128; // 10^18 − 1 units
    for _ in 0..100_000 {
        big.push(x, x);
    }
    assert_eq!(big.value(Function::Var, 9, 9).unwrap(), Some(0.0));
    assert_eq!(big.value(Function::Mean, 9, 9).unwrap(), Some(tick_to_f64(x, 9)));
}

#[test]
fn the_exact_mode_over_float_columns_is_two_pass() {
    // Floats have no integer form (D-6.3 as amended): `exact` reduces a
    // float column as `two_pass` does, bit for bit.
    let xs: Vec<f64> = (0..100).map(|i| (i as f64 * 0.37).sin() * 100.0).collect();
    let ys: Vec<f64> = (0..100).map(|i| (i as f64 * 0.11).cos() * 10.0).collect();
    for f in [
        Function::Sum,
        Function::Mean,
        Function::Var,
        Function::Std,
        Function::Cov,
        Function::Corr,
        Function::OlsSlope,
        Function::Median,
        Function::Quantile(0.3),
        Function::Min,
        Function::Max,
    ] {
        assert_eq!(
            reduce(f, Mode::Exact, &[&xs, &ys]).unwrap().map(f64::to_bits),
            reduce(f, Mode::TwoPass, &[&xs, &ys]).unwrap().map(f64::to_bits),
            "{:?}",
            f
        );
    }
    assert_eq!(Mode::Exact.name(), "exact");
}
