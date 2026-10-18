//! Tick columns (ST-3): a block of up to 1,024 values of one instrument
//! carries a base `β`, a quantum `q = g · 10^(−e)` and offsets `u`, so that
//! `x = (β + g·u) · 10^(−e)`. The encoder finds the smallest exponent at
//! which every value round-trips, the greatest common divisor of the
//! offsets, and the width that minimises the packed size with patched
//! exceptions (ST-4.4); then it decodes the block with the reader's formula
//! and keeps, as an exception with its raw bits, any value that does not
//! come back bit for bit (Proposition 3.2). Sums of offsets are exact
//! integers, combined across quanta in the finest exponent (Proposition
//! 3.3).

use abt_agg::exact::{pow10, Int};

/// How a source wrote its values: an `f32` reproduces when the `f64` it
/// decodes to rounds to its bits; an `f64` must come back exactly.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    F32,
    F64,
}

/// The greatest exponent a block's quantum takes (`10^-9`).
pub const MAX_EXPONENT: u8 = 9;

/// The cost in bits of a patched exception: its 10-bit position and 64-bit
/// value (ST-4.4).
pub const EXCEPTION_BITS: u64 = 74;

/// A value a block does not hold on its grid.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Exception {
    /// An offset above the block's width (a patched exception), stored whole.
    Offset { position: u16, offset: u64 },
    /// A value off the grid (it did not reproduce): its raw `f64` bits, which
    /// the reader returns in place of the decoded value.
    Raw { position: u16, bits: u64 },
}

impl Exception {
    pub fn position(&self) -> u16 {
        match self {
            Exception::Offset { position, .. } | Exception::Raw { position, .. } => *position,
        }
    }
}

/// The header of a block (ST-4.3, 32 bytes on disk) and its offsets as
/// the packer takes them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Encoded {
    pub base: i64,
    pub g: u64,
    pub e: u8,
    pub width: u8,
    pub n: u16,
    /// Readers may multiply by the reciprocal of `10^e` (it reproduces every
    /// value of the block) rather than divide.
    pub recip_exact: bool,
    /// The source wrote `f32` values: the view is the decoded `f64` rounded
    /// to `f32`, the vendor's bits.
    pub f32: bool,
    /// The offsets below `2^width`, in row order; an exception's position
    /// holds 0.
    pub offsets: Vec<u64>,
    /// Sorted by position.
    pub exceptions: Vec<Exception>,
}

/// `x × 10^e` rounded to the nearest integer, when it is one `i64` holds.
fn scaled(x: f64, e: u8) -> Option<i64> {
    let s = (x * pow10(e as u32) as f64).round();
    if !s.is_finite() || s.abs() >= 9.0e18 {
        return None;
    }
    Some(s as i64)
}

/// Whether `x` comes back from its integer at exponent `e`.
fn round_trips(x: f64, e: u8, source: Source) -> bool {
    let Some(i) = scaled(x, e) else { return false };
    let back = i as f64 / pow10(e as u32) as f64;
    match source {
        Source::F64 => back.to_bits() == x.to_bits(),
        Source::F32 => ((back as f32) as f64).to_bits() == x.to_bits(),
    }
}

/// The smallest exponent at which every value of the block round-trips,
/// or `None` when none up to `MAX_EXPONENT` does.
pub fn exponent(values: &[f64], source: Source) -> Option<u8> {
    (0..=MAX_EXPONENT).find(|&e| values.iter().all(|&x| x.is_finite() && round_trips(x, e, source)))
}

fn gcd(mut a: u64, mut b: u64) -> u64 {
    while b != 0 {
        let t = a % b;
        a = b;
        b = t;
    }
    a
}

/// The width minimising `n·W + 74·#{u ≥ 2^W}` (ST-4.4), and the offsets
/// that become exceptions at it.
pub fn best_width(offsets: &[u64]) -> u8 {
    let n = offsets.len() as u64;
    let max = offsets.iter().copied().max().unwrap_or(0);
    let widest = if max == 0 { 0 } else { 64 - max.leading_zeros() as u8 };
    let mut best = (u64::MAX, widest);
    for w in 0..=widest {
        let above = if w == 64 { 0 } else { offsets.iter().filter(|&&u| u >= (1u64 << w)).count() as u64 };
        let cost = n * w as u64 + EXCEPTION_BITS * above;
        if cost < best.0 {
            best = (cost, w);
        }
    }
    best.1
}

/// The decoded value of row `j` as the reader computes it: the integer's
/// `f64` over `10^e` (one correctly rounded division, or the verified
/// multiply), rounded to `f32` for an `f32` source.
pub fn decode_value(base: i64, g: u64, e: u8, recip_exact: bool, f32: bool, u: u64) -> f64 {
    let i = base as i128 + g as i128 * u as i128;
    let v = if recip_exact {
        i as f64 * (1.0 / pow10(e as u32) as f64)
    } else {
        i as f64 / pow10(e as u32) as f64
    };
    if f32 {
        (v as f32) as f64
    } else {
        v
    }
}

/// Encodes a block of up to 1,024 values of one instrument (ST-3). Values
/// that do not round-trip at any exponent, or that would overflow, are
/// kept as raw exceptions; a block with no value on a grid is encoded at
/// exponent 0 with every value an exception.
pub fn encode(values: &[f64], source: Source) -> Encoded {
    assert!(values.len() <= crate::BLOCK, "a block holds at most {} values", crate::BLOCK);
    let n = values.len();
    // The exponent of the values on a grid; the others are exceptions.
    let on_grid = |e: u8| values.iter().filter(|&&x| x.is_finite() && round_trips(x, e, source)).count();
    let e = exponent(values, source).unwrap_or_else(|| (0..=MAX_EXPONENT).max_by_key(|&e| on_grid(e)).unwrap_or(0));
    let ints: Vec<Option<i64>> = values.iter().map(|&x| if x.is_finite() && round_trips(x, e, source) { scaled(x, e) } else { None }).collect();
    let base = ints.iter().flatten().copied().min().unwrap_or(0);
    let mut g = 0u64;
    for i in ints.iter().flatten() {
        g = gcd(g, (*i - base) as u64);
    }
    if g == 0 {
        g = 1;
    }
    let raw_offsets: Vec<u64> = ints.iter().map(|i| i.map(|i| ((i - base) as u64) / g).unwrap_or(0)).collect();
    let on_grid_offsets: Vec<u64> = ints.iter().zip(&raw_offsets).filter(|(i, _)| i.is_some()).map(|(_, &u)| u).collect();
    let width = best_width(&on_grid_offsets);
    let limit = if width == 64 { u64::MAX } else { (1u64 << width) - 1 };
    // The multiply form, tested on every value on the grid: it must give
    // the division's bits.
    let (recip, p10) = (1.0 / pow10(e as u32) as f64, pow10(e as u32) as f64);
    let recip_exact = ints.iter().flatten().all(|&i| ((i as f64) * recip).to_bits() == (i as f64 / p10).to_bits());
    let f32 = source == Source::F32;
    let mut offsets = Vec::with_capacity(n);
    let mut exceptions = Vec::new();
    for (j, (&x, &u)) in values.iter().zip(&raw_offsets).enumerate() {
        match ints[j] {
            None => {
                offsets.push(0);
                exceptions.push(Exception::Raw {
                    position: j as u16,
                    bits: x.to_bits(),
                });
            }
            Some(_) if u > limit => {
                offsets.push(0);
                exceptions.push(Exception::Offset { position: j as u16, offset: u });
            }
            Some(_) => {
                // Verified: the reader's formula gives the source's bits.
                let back = decode_value(base, g, e, recip_exact, f32, u);
                let same = back.to_bits() == x.to_bits();
                if same {
                    offsets.push(u);
                } else {
                    offsets.push(0);
                    exceptions.push(Exception::Raw {
                        position: j as u16,
                        bits: x.to_bits(),
                    });
                }
            }
        }
    }
    Encoded {
        base,
        g,
        e,
        width,
        n: n as u16,
        recip_exact,
        f32,
        offsets,
        exceptions,
    }
}

impl Encoded {
    /// The offset of row `j`, exceptions patched.
    pub fn offset(&self, j: usize) -> Option<u64> {
        match self.exceptions.binary_search_by_key(&(j as u16), Exception::position) {
            Ok(k) => match self.exceptions[k] {
                Exception::Offset { offset, .. } => Some(offset),
                Exception::Raw { .. } => None,
            },
            Err(_) => Some(self.offsets[j]),
        }
    }

    /// The integer `β + g·u` of row `j` at the block's exponent, or `None`
    /// for a value off the grid.
    pub fn integer(&self, j: usize) -> Option<i64> {
        self.offset(j).map(|u| self.base + (self.g * u) as i64)
    }

    /// Every value as the source wrote it (Proposition 3.2).
    pub fn decode(&self) -> Vec<f64> {
        (0..self.n as usize).map(|j| self.value(j)).collect()
    }

    /// The value of row `j`.
    pub fn value(&self, j: usize) -> f64 {
        match self.exceptions.binary_search_by_key(&(j as u16), Exception::position) {
            Ok(k) => match self.exceptions[k] {
                Exception::Offset { offset, .. } => decode_value(self.base, self.g, self.e, self.recip_exact, self.f32, offset),
                Exception::Raw { bits, .. } => f64::from_bits(bits),
            },
            Err(_) => decode_value(self.base, self.g, self.e, self.recip_exact, self.f32, self.offsets[j]),
        }
    }

    /// Whether every value of the block is on its grid (an exact sum exists).
    pub fn exact(&self) -> bool {
        !self.exceptions.iter().any(|x| matches!(x, Exception::Raw { .. }))
    }

    /// The exact sum of rows `[a, b)` in the block's own exponent, as the
    /// integer `Σ(β + g·u)` (`None` when a raw exception lies in the range).
    pub fn sum(&self, a: usize, b: usize) -> Option<i128> {
        let mut su: u128 = 0;
        for j in a..b {
            su += self.offset(j)? as u128;
        }
        Some((b - a) as i128 * self.base as i128 + (self.g as u128 * su) as i128)
    }
}

/// A contribution to a sum across blocks of different quanta: an integer
/// at a block's exponent, aligned to the finest exponent `e_star` of the
/// sum (Proposition 3.3).
pub fn aligned(integer: i128, e: u8, e_star: u8) -> Int {
    debug_assert!(e_star >= e);
    Int::from_i128(integer).mul_u128(pow10((e_star - e) as u32))
}

/// The nearest `f64` of an exact integer at an exponent.
pub fn to_f64(integer: &Int, e: u8) -> f64 {
    integer.to_f64_scaled(e as u32)
}
