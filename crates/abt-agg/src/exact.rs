//! The `exact` numerics (D-6.3; `07-storage.md`, Proposition 3.3): tick
//! columns reduced as integers in the window's finest exponent, so that a
//! sum, a moment or a slope is an exact rational independent of the order
//! the rows are read in, and its `f64` is one correctly rounded division.
//! Sums fit `i128`; squares and cross products are accumulated in a wider
//! integer (`Wide`), since `Σx²` of 10^5 rows of large prices passes
//! 10^38 (ST-3).
//!
//! No dependency: the wide integer is a few limbs of `u64` with the shifts,
//! comparisons and the one long division the rounding needs. Nothing here
//! runs per row of a scan in the data layer's hot loops; the per-block
//! sums of `abt-codec` call `Wide` once per block.

use std::cmp::Ordering;

use crate::function::{AggError, Function};

/// An unsigned integer of up to `LIMBS` × 64 bits (320 bits: a 256-bit
/// product shifted by the rounding's 54 bits still fits), little-endian.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Wide {
    limbs: [u64; LIMBS],
}

const LIMBS: usize = 6;

impl Wide {
    pub const ZERO: Wide = Wide { limbs: [0; LIMBS] };

    pub fn from_u128(x: u128) -> Wide {
        let mut w = Wide::ZERO;
        w.limbs[0] = x as u64;
        w.limbs[1] = (x >> 64) as u64;
        w
    }

    pub fn is_zero(&self) -> bool {
        self.limbs.iter().all(|&l| l == 0)
    }

    /// The number of significant bits (0 for zero).
    pub fn bits(&self) -> u32 {
        for i in (0..LIMBS).rev() {
            if self.limbs[i] != 0 {
                return i as u32 * 64 + (64 - self.limbs[i].leading_zeros());
            }
        }
        0
    }

    pub fn low_u128(&self) -> u128 {
        self.limbs[0] as u128 | ((self.limbs[1] as u128) << 64)
    }

    /// The limbs, least significant first.
    pub fn limbs(&self) -> &[u64; LIMBS] {
        &self.limbs
    }

    pub fn from_limbs(limbs: [u64; LIMBS]) -> Wide {
        Wide { limbs }
    }

    /// `self + other`; panics on overflow (320 bits are never reached by the
    /// sums the callers form).
    pub fn add(&self, other: &Wide) -> Wide {
        let mut out = Wide::ZERO;
        let mut carry = 0u128;
        for i in 0..LIMBS {
            let s = self.limbs[i] as u128 + other.limbs[i] as u128 + carry;
            out.limbs[i] = s as u64;
            carry = s >> 64;
        }
        assert_eq!(carry, 0, "a wide sum overflowed 320 bits");
        out
    }

    /// `self − other`, which must not be negative.
    pub fn sub(&self, other: &Wide) -> Wide {
        assert!(self >= other, "a wide difference went negative");
        let mut out = Wide::ZERO;
        let mut borrow = 0i128;
        for i in 0..LIMBS {
            let d = self.limbs[i] as i128 - other.limbs[i] as i128 - borrow;
            if d < 0 {
                out.limbs[i] = (d + (1i128 << 64)) as u64;
                borrow = 1;
            } else {
                out.limbs[i] = d as u64;
                borrow = 0;
            }
        }
        out
    }

    /// `self × m` for a 128-bit factor.
    pub fn mul_u128(&self, m: u128) -> Wide {
        let (m0, m1) = (m as u64 as u128, m >> 64);
        let mut out = [0u128; LIMBS + 2];
        for i in 0..LIMBS {
            let l = self.limbs[i] as u128;
            if l == 0 {
                continue;
            }
            let p0 = l * m0;
            let p1 = l * m1;
            out[i] += p0 & 0xFFFF_FFFF_FFFF_FFFF;
            out[i + 1] += (p0 >> 64) + (p1 & 0xFFFF_FFFF_FFFF_FFFF);
            out[i + 2] += p1 >> 64;
        }
        let mut w = Wide::ZERO;
        let mut carry = 0u128;
        for (i, slot) in out.iter().enumerate() {
            let s = slot + carry;
            if i < LIMBS {
                w.limbs[i] = s as u64;
            } else {
                assert_eq!(s, 0, "a wide product overflowed 320 bits");
            }
            carry = s >> 64;
        }
        w
    }

    pub fn shl(&self, bits: u32) -> Wide {
        let (words, rem) = ((bits / 64) as usize, bits % 64);
        let mut out = Wide::ZERO;
        for i in (0..LIMBS).rev() {
            if i < words {
                break;
            }
            let src = i - words;
            let mut v = self.limbs[src] << rem;
            if rem > 0 && src > 0 {
                v |= self.limbs[src - 1] >> (64 - rem);
            }
            out.limbs[i] = v;
        }
        if words < LIMBS {
            for i in (LIMBS - words)..LIMBS {
                assert!(self.limbs[i] == 0 || i + words >= LIMBS, "a wide shift overflowed 320 bits");
            }
        }
        out
    }

    pub fn shr(&self, bits: u32) -> Wide {
        let (words, rem) = ((bits / 64) as usize, bits % 64);
        let mut out = Wide::ZERO;
        for i in 0..LIMBS {
            let src = i + words;
            if src >= LIMBS {
                break;
            }
            let mut v = self.limbs[src] >> rem;
            if rem > 0 && src + 1 < LIMBS {
                v |= self.limbs[src + 1] << (64 - rem);
            }
            out.limbs[i] = v;
        }
        out
    }

    fn bit(&self, i: u32) -> bool {
        (self.limbs[(i / 64) as usize] >> (i % 64)) & 1 == 1
    }

    fn set_bit(&mut self, i: u32) {
        self.limbs[(i / 64) as usize] |= 1 << (i % 64);
    }

    /// The quotient and remainder of `self / d` (`d` not zero), by binary
    /// long division: a few hundred steps, once per reduction.
    pub fn divrem(&self, d: &Wide) -> (Wide, Wide) {
        assert!(!d.is_zero(), "division by zero");
        let mut q = Wide::ZERO;
        let mut r = Wide::ZERO;
        for i in (0..self.bits()).rev() {
            r = r.shl(1);
            if self.bit(i) {
                r.limbs[0] |= 1;
            }
            if r >= *d {
                r = r.sub(d);
                q.set_bit(i);
            }
        }
        (q, r)
    }
}

impl PartialOrd for Wide {
    fn partial_cmp(&self, other: &Wide) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Wide {
    fn cmp(&self, other: &Wide) -> Ordering {
        for i in (0..LIMBS).rev() {
            match self.limbs[i].cmp(&other.limbs[i]) {
                Ordering::Equal => continue,
                o => return o,
            }
        }
        Ordering::Equal
    }
}

/// A signed wide integer: a sign and a magnitude.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Int {
    pub negative: bool,
    pub magnitude: Wide,
}

impl Int {
    pub const ZERO: Int = Int {
        negative: false,
        magnitude: Wide::ZERO,
    };

    pub fn from_i128(x: i128) -> Int {
        Int {
            negative: x < 0,
            magnitude: Wide::from_u128(x.unsigned_abs()),
        }
    }

    /// The product of two 128-bit integers, exact.
    pub fn product(a: i128, b: i128) -> Int {
        Int {
            negative: (a < 0) != (b < 0) && a != 0 && b != 0,
            magnitude: Wide::from_u128(a.unsigned_abs()).mul_u128(b.unsigned_abs()),
        }
    }

    pub fn is_zero(&self) -> bool {
        self.magnitude.is_zero()
    }

    pub fn neg(&self) -> Int {
        Int {
            negative: !self.negative && !self.is_zero(),
            magnitude: self.magnitude,
        }
    }

    pub fn add(&self, other: &Int) -> Int {
        if self.negative == other.negative {
            return Int {
                negative: self.negative,
                magnitude: self.magnitude.add(&other.magnitude),
            };
        }
        match self.magnitude.cmp(&other.magnitude) {
            Ordering::Equal => Int::ZERO,
            Ordering::Greater => Int {
                negative: self.negative,
                magnitude: self.magnitude.sub(&other.magnitude),
            },
            Ordering::Less => Int {
                negative: other.negative,
                magnitude: other.magnitude.sub(&self.magnitude),
            },
        }
    }

    pub fn sub(&self, other: &Int) -> Int {
        self.add(&other.neg())
    }

    pub fn mul_u128(&self, m: u128) -> Int {
        Int {
            negative: self.negative && m != 0,
            magnitude: self.magnitude.mul_u128(m),
        }
    }

    /// `self × other` for a small signed factor.
    pub fn mul_i128(&self, m: i128) -> Int {
        let p = self.mul_u128(m.unsigned_abs());
        if m < 0 {
            p.neg()
        } else {
            p
        }
    }

    /// `self / 10^s` as the nearest `f64`, ties to even: the view of an
    /// exact rational (ST-3).
    pub fn to_f64_scaled(&self, scale: u32) -> f64 {
        ratio_to_f64(self, &Wide::from_u128(pow10(scale)))
    }
}

/// `10^s` for `s ≤ 38`.
pub fn pow10(s: u32) -> u128 {
    10u128.pow(s)
}

/// The nearest `f64` to `num / den` (`den` not zero), ties to even: the
/// quotient is taken to 54 significant bits with the remainder as the
/// sticky bit, then rounded once, so the result is what IEEE 754 division
/// of the exact operands would give.
pub fn ratio_to_f64(num: &Int, den: &Wide) -> f64 {
    assert!(!den.is_zero(), "a ratio with a zero denominator");
    if num.is_zero() {
        return 0.0;
    }
    let p = num.magnitude;
    // Shift so that the integer quotient has at least 54 bits, or trim a
    // large numerator (keeping what falls off as sticky) so that it has at
    // most 54 + 64 bits of headroom.
    let (pb, db) = (p.bits() as i32, den.bits() as i32);
    let want = 55; // bits in the integer quotient: 53 + a guard bit + room for an off-by-one estimate
    let shift = want - (pb - db); // positive: shift the numerator left; negative: the denominator
    let (p, d, exp) = if shift >= 0 { (p.shl(shift as u32), *den, -shift) } else { (p, den.shl((-shift) as u32), -shift) };
    let (q, r) = p.divrem(&d);
    // q has want or want + 1 bits; r < d is the sticky remainder.
    let qbits = q.bits() as i32;
    let drop = qbits - 53; // bits of q below the 53-bit mantissa
    debug_assert!(drop >= 1);
    let mantissa = q.shr(drop as u32).low_u128() as u64;
    let dropped = q.sub(&q.shr(drop as u32).shl(drop as u32));
    let half = Wide::from_u128(1u128 << (drop as u32 - 1));
    let sticky = !r.is_zero();
    let round_up = match dropped.cmp(&half) {
        Ordering::Greater => true,
        Ordering::Less => false,
        Ordering::Equal => sticky || (mantissa & 1 == 1),
    };
    let mantissa = mantissa + u64::from(round_up);
    // mantissa × 2^(exp + drop): an exact f64 product, since 2^k is exact.
    let value = (mantissa as f64) * 2f64.powi(exp + drop);
    if num.negative {
        -value
    } else {
        value
    }
}

/// A tick column: integer units of `10^(−scale)`, the column's values
/// already aligned to the window's finest exponent.
#[derive(Clone, Copy, Debug)]
pub struct Ticks<'a> {
    pub units: &'a [i128],
    pub scale: u32,
}

/// The exact sums of a window's rows: `n`, `Σx`, `Σy`, `Σx²`, `Σy²`,
/// `Σxy`, each exact; rows enter and leave by integer arithmetic, so a
/// sliding window needs no rebuild and no order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExactSums {
    pub n: u64,
    pub sx: i128,
    pub sy: i128,
    pub sxx: Int,
    pub syy: Int,
    pub sxy: Int,
}

impl Default for ExactSums {
    fn default() -> ExactSums {
        ExactSums {
            n: 0,
            sx: 0,
            sy: 0,
            sxx: Int::ZERO,
            syy: Int::ZERO,
            sxy: Int::ZERO,
        }
    }
}

impl ExactSums {
    pub fn push(&mut self, x: i128, y: i128) {
        self.n += 1;
        self.sx += x;
        self.sy += y;
        self.sxx = self.sxx.add(&Int::product(x, x));
        self.syy = self.syy.add(&Int::product(y, y));
        self.sxy = self.sxy.add(&Int::product(x, y));
    }

    pub fn evict(&mut self, x: i128, y: i128) {
        self.n -= 1;
        self.sx -= x;
        self.sy -= y;
        self.sxx = self.sxx.sub(&Int::product(x, x));
        self.syy = self.syy.sub(&Int::product(y, y));
        self.sxy = self.sxy.sub(&Int::product(x, y));
    }

    /// `n·Σxy − Σx·Σy`: the centred cross sum times `n`, exact.
    fn centred(&self, sab: &Int, sa: i128, sb: i128) -> Int {
        sab.mul_u128(self.n as u128).sub(&Int::product(sa, sb))
    }

    /// The function over the rows the sums hold, at the scales of the two
    /// columns, as the correctly rounded `f64` of the exact rational (or of
    /// the exact factors of a quotient under a square root).
    pub fn value(&self, f: Function, scale_x: u32, scale_y: u32) -> Result<Option<f64>, AggError> {
        let n = self.n;
        if f == Function::Count {
            return Ok(Some(n as f64));
        }
        if n == 0 {
            return Ok(None);
        }
        let few = || AggError::FewerThanTwo(f);
        let v = match f {
            Function::Sum => Int::from_i128(self.sx).to_f64_scaled(scale_x),
            Function::Mean => ratio_to_f64(&Int::from_i128(self.sx), &Wide::from_u128(n as u128 * pow10(scale_x))),
            Function::Var | Function::Std => {
                if n < 2 {
                    return Err(few());
                }
                // (n·Σx² − (Σx)²) / (n(n−1)) / 10^(2s)
                let num = self.centred(&self.sxx, self.sx, self.sx);
                let den = Wide::from_u128(n as u128 * (n as u128 - 1)).mul_u128(pow10(2 * scale_x));
                let var = ratio_to_f64(&num, &den);
                if f == Function::Std {
                    var.sqrt()
                } else {
                    var
                }
            }
            Function::Cov => {
                if n < 2 {
                    return Err(few());
                }
                let num = self.centred(&self.sxy, self.sx, self.sy);
                let den = Wide::from_u128(n as u128 * (n as u128 - 1)).mul_u128(pow10(scale_x + scale_y));
                ratio_to_f64(&num, &den)
            }
            Function::Corr | Function::OlsR => {
                if n < 2 {
                    return Err(few());
                }
                let cxy = self.centred(&self.sxy, self.sx, self.sy);
                let cxx = self.centred(&self.sxx, self.sx, self.sx);
                let cyy = self.centred(&self.syy, self.sy, self.sy);
                if cxx.is_zero() || cyy.is_zero() {
                    return Err(AggError::Constant(f));
                }
                // Scale-free: the units cancel. Each factor exact, then one
                // product and one square root in f64.
                let one = Wide::from_u128(1);
                let (a, b, c) = (ratio_to_f64(&cxy, &one), ratio_to_f64(&cxx, &one), ratio_to_f64(&cyy, &one));
                a / (b * c).sqrt()
            }
            Function::OlsSlope | Function::OlsBeta => {
                if n < 2 {
                    return Err(few());
                }
                // The slope of x on y: (n·Σxy − Σx·Σy) / (n·Σy² − (Σy)²), in
                // units of x per unit of y.
                let cxy = self.centred(&self.sxy, self.sx, self.sy);
                let cyy = self.centred(&self.syy, self.sy, self.sy);
                if cyy.is_zero() {
                    return Err(AggError::Constant(f));
                }
                let num = if scale_y >= scale_x { cxy.mul_u128(pow10(scale_y - scale_x)) } else { cxy };
                let den = if scale_y >= scale_x { cyy.magnitude } else { cyy.magnitude.mul_u128(pow10(scale_x - scale_y)) };
                let v = ratio_to_f64(&num, &den);
                if cyy.negative {
                    -v
                } else {
                    v
                }
            }
            _ => return Ok(None),
        };
        Ok(Some(v))
    }
}

/// The function over tick columns (`cols[0]` its argument, `cols[1]` the
/// second argument of a bivariate one), exactly: the moments from exact
/// sums, the selections and order statistics on the integer units, each
/// `f64` one correctly rounded division. `Ok(None)` is "no value": an empty
/// group for anything but `count`, which is 0.
pub fn reduce_ticks(f: Function, cols: &[Ticks]) -> Result<Option<f64>, AggError> {
    let Some(x) = cols.first() else {
        return Ok(if f == Function::Count { Some(0.0) } else { None });
    };
    if f == Function::Count {
        return Ok(Some(x.units.len() as f64));
    }
    if x.units.is_empty() {
        return Ok(None);
    }
    let view = |u: i128| Int::from_i128(u).to_f64_scaled(x.scale);
    let v = match f {
        Function::Min => view(*x.units.iter().min().unwrap()),
        Function::Max => view(*x.units.iter().max().unwrap()),
        Function::First => view(x.units[0]),
        Function::Last => view(x.units[x.units.len() - 1]),
        Function::Median => {
            let mut s = x.units.to_vec();
            s.sort_unstable();
            let n = s.len();
            if n % 2 == 1 {
                view(s[n / 2])
            } else {
                ratio_to_f64(&Int::from_i128(s[n / 2 - 1] + s[n / 2]), &Wide::from_u128(2 * pow10(x.scale)))
            }
        }
        Function::Quantile(q) => {
            if !(0.0..=1.0).contains(&q) {
                return Err(AggError::LevelOutside(q));
            }
            // The interpolation of the two neighbouring units is the
            // two-pass formula over their views (the level is a float).
            let mut s = x.units.to_vec();
            s.sort_unstable();
            let views: Vec<f64> = s.iter().map(|&u| view(u)).collect();
            crate::order_stats::quantile_of_sorted(&views, q)
        }
        _ => {
            let mut sums = ExactSums::default();
            let y = cols.get(1);
            if f.arity() == 2 && y.is_none() {
                return Err(AggError::FewerThanTwo(f));
            }
            for (i, &u) in x.units.iter().enumerate() {
                sums.push(u, y.map(|c| c.units[i]).unwrap_or(0));
            }
            return sums.value(f, x.scale, y.map(|c| c.scale).unwrap_or(0));
        }
    };
    Ok(Some(v))
}

/// The units of `x` at `scale` from a coarser one: `x × 10^(scale − from)`,
/// exact (`scale ≥ from`).
pub fn align(x: i64, from: u32, scale: u32) -> i128 {
    x as i128 * pow10(scale - from) as i128
}

/// The exact decimal value of `units × 10^(−scale)` as the nearest `f64`.
pub fn tick_to_f64(units: i128, scale: u32) -> f64 {
    Int::from_i128(units).to_f64_scaled(scale)
}
