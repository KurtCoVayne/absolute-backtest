//! The abstract domains (compiler design, section 3.6): the reduced product
//! of dimension, sign and interval per column, with a definedness flag.
//! Interval bounds are doubles rounded outward (section 6.8), so that every
//! abstract operation over-approximates the real one.

// The arithmetic is named after the operations it over-approximates; the
// std traits would hide the outward rounding behind operators.
#![allow(clippy::should_implement_trait)]

use crate::check::types;
use crate::ir::{BinOp, CmpOp, Ty};

/// The eight-element sign lattice.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
pub enum Sign {
    Bot,
    Neg,
    Zero,
    Pos,
    NonPos,
    NonNeg,
    NonZero,
    Top,
}

impl Sign {
    fn bits(self) -> u8 {
        // bit 0: negative, bit 1: zero, bit 2: positive.
        match self {
            Sign::Bot => 0,
            Sign::Neg => 1,
            Sign::Zero => 2,
            Sign::Pos => 4,
            Sign::NonPos => 3,
            Sign::NonNeg => 6,
            Sign::NonZero => 5,
            Sign::Top => 7,
        }
    }
    fn from_bits(b: u8) -> Sign {
        match b & 7 {
            0 => Sign::Bot,
            1 => Sign::Neg,
            2 => Sign::Zero,
            4 => Sign::Pos,
            3 => Sign::NonPos,
            6 => Sign::NonNeg,
            5 => Sign::NonZero,
            _ => Sign::Top,
        }
    }
    pub fn join(self, o: Sign) -> Sign {
        Sign::from_bits(self.bits() | o.bits())
    }
    pub fn meet(self, o: Sign) -> Sign {
        Sign::from_bits(self.bits() & o.bits())
    }
    pub fn may_be_zero(self) -> bool {
        self.bits() & 2 != 0
    }
    pub fn may_be_negative(self) -> bool {
        self.bits() & 1 != 0
    }
    pub fn may_be_positive(self) -> bool {
        self.bits() & 4 != 0
    }
    pub fn neg(self) -> Sign {
        let b = self.bits();
        Sign::from_bits(((b & 1) << 2) | (b & 2) | ((b & 4) >> 2))
    }
    pub fn mul(self, o: Sign) -> Sign {
        let mut out = 0u8;
        for a in [1u8, 2, 4] {
            for b in [1u8, 2, 4] {
                if self.bits() & a != 0 && o.bits() & b != 0 {
                    out |= match (a, b) {
                        (2, _) | (_, 2) => 2,
                        (1, 1) | (4, 4) => 4,
                        _ => 1,
                    };
                }
            }
        }
        Sign::from_bits(out)
    }
    pub fn add(self, o: Sign) -> Sign {
        let mut out = 0u8;
        for a in [1u8, 2, 4] {
            for b in [1u8, 2, 4] {
                if self.bits() & a != 0 && o.bits() & b != 0 {
                    out |= match (a, b) {
                        (2, x) | (x, 2) => x,
                        (1, 1) => 1,
                        (4, 4) => 4,
                        _ => 7,
                    };
                }
            }
        }
        Sign::from_bits(out)
    }
    pub fn text(self) -> &'static str {
        match self {
            Sign::Bot => "⊥",
            Sign::Neg => "<0",
            Sign::Zero => "=0",
            Sign::Pos => ">0",
            Sign::NonPos => "≤0",
            Sign::NonNeg => "≥0",
            Sign::NonZero => "≠0",
            Sign::Top => "⊤",
        }
    }
    pub fn of_interval(i: Interval) -> Sign {
        if i.is_bottom() {
            return Sign::Bot;
        }
        let mut b = 0u8;
        if i.lo < 0.0 {
            b |= 1;
        }
        if i.lo <= 0.0 && i.hi >= 0.0 {
            b |= 2;
        }
        if i.hi > 0.0 {
            b |= 4;
        }
        Sign::from_bits(b)
    }
}

/// A closed interval of the extended reals; empty when `lo > hi`.
#[derive(Clone, Copy, Debug, PartialEq, serde::Serialize)]
pub struct Interval {
    pub lo: f64,
    pub hi: f64,
}

/// Round a lower bound down and an upper bound up (outward rounding).
fn down(x: f64) -> f64 {
    if x.is_finite() {
        x.next_down()
    } else {
        x
    }
}
fn up(x: f64) -> f64 {
    if x.is_finite() {
        x.next_up()
    } else {
        x
    }
}

impl Interval {
    pub const TOP: Interval = Interval {
        lo: f64::NEG_INFINITY,
        hi: f64::INFINITY,
    };
    pub const BOTTOM: Interval = Interval { lo: 1.0, hi: -1.0 };
    pub fn point(x: f64) -> Interval {
        Interval { lo: x, hi: x }
    }
    pub fn new(lo: f64, hi: f64) -> Interval {
        Interval { lo, hi }
    }
    pub fn is_bottom(&self) -> bool {
        self.lo > self.hi || self.lo.is_nan() || self.hi.is_nan()
    }
    pub fn is_top(&self) -> bool {
        self.lo == f64::NEG_INFINITY && self.hi == f64::INFINITY
    }
    pub fn contains(&self, x: f64) -> bool {
        !self.is_bottom() && self.lo <= x && x <= self.hi
    }
    pub fn join(self, o: Interval) -> Interval {
        if self.is_bottom() {
            return o;
        }
        if o.is_bottom() {
            return self;
        }
        Interval::new(self.lo.min(o.lo), self.hi.max(o.hi))
    }
    pub fn meet(self, o: Interval) -> Interval {
        if self.is_bottom() || o.is_bottom() {
            return Interval::BOTTOM;
        }
        Interval::new(self.lo.max(o.lo), self.hi.min(o.hi))
    }
    /// Outward-rounded arithmetic.
    pub fn add(self, o: Interval) -> Interval {
        if self.is_bottom() || o.is_bottom() {
            return Interval::BOTTOM;
        }
        Interval::new(down(self.lo + o.lo), up(self.hi + o.hi))
    }
    pub fn sub(self, o: Interval) -> Interval {
        if self.is_bottom() || o.is_bottom() {
            return Interval::BOTTOM;
        }
        Interval::new(down(self.lo - o.hi), up(self.hi - o.lo))
    }
    pub fn neg(self) -> Interval {
        if self.is_bottom() {
            return self;
        }
        Interval::new(-self.hi, -self.lo)
    }
    fn corners(self, o: Interval, f: impl Fn(f64, f64) -> f64) -> Interval {
        let vals = [f(self.lo, o.lo), f(self.lo, o.hi), f(self.hi, o.lo), f(self.hi, o.hi)];
        let mut lo = f64::INFINITY;
        let mut hi = f64::NEG_INFINITY;
        for v in vals {
            if v.is_nan() {
                // 0 × ∞: either sign is possible.
                return Interval::TOP;
            }
            lo = lo.min(v);
            hi = hi.max(v);
        }
        Interval::new(down(lo), up(hi))
    }
    pub fn mul(self, o: Interval) -> Interval {
        if self.is_bottom() || o.is_bottom() {
            return Interval::BOTTOM;
        }
        let zero = |i: Interval| i.lo == 0.0 && i.hi == 0.0;
        if zero(self) || zero(o) {
            return Interval::point(0.0);
        }
        self.corners(o, |a, b| a * b)
    }
    /// Standard interval division; the whole line when the divisor may be zero.
    pub fn div(self, o: Interval) -> Interval {
        if self.is_bottom() || o.is_bottom() {
            return Interval::BOTTOM;
        }
        if o.contains(0.0) {
            return Interval::TOP;
        }
        self.corners(o, |a, b| a / b)
    }
    pub fn log(self) -> Interval {
        if self.is_bottom() || self.hi <= 0.0 {
            return Interval::BOTTOM;
        }
        let lo = if self.lo <= 0.0 { f64::NEG_INFINITY } else { down(self.lo.ln()) };
        Interval::new(lo, up(self.hi.ln()))
    }
    pub fn exp(self) -> Interval {
        if self.is_bottom() {
            return self;
        }
        Interval::new(down(self.lo.exp()).max(0.0), up(self.hi.exp()))
    }
    pub fn sqrt(self) -> Interval {
        if self.is_bottom() || self.hi < 0.0 {
            return Interval::BOTTOM;
        }
        Interval::new(down(self.lo.max(0.0).sqrt()).max(0.0), up(self.hi.sqrt()))
    }
    pub fn abs(self) -> Interval {
        if self.is_bottom() {
            return self;
        }
        if self.contains(0.0) {
            Interval::new(0.0, self.lo.abs().max(self.hi.abs()))
        } else {
            Interval::new(self.lo.abs().min(self.hi.abs()), self.lo.abs().max(self.hi.abs()))
        }
    }
    pub fn least(self, o: Interval) -> Interval {
        if self.is_bottom() || o.is_bottom() {
            return Interval::BOTTOM;
        }
        Interval::new(self.lo.min(o.lo), self.hi.min(o.hi))
    }
    pub fn greatest(self, o: Interval) -> Interval {
        if self.is_bottom() || o.is_bottom() {
            return Interval::BOTTOM;
        }
        Interval::new(self.lo.max(o.lo), self.hi.max(o.hi))
    }
    /// Whether no pair of values satisfies `op` (the literal is never true).
    pub fn never(op: CmpOp, a: Interval, b: Interval) -> bool {
        if a.is_bottom() || b.is_bottom() {
            return true;
        }
        match op {
            CmpOp::Lt => a.lo >= b.hi,
            CmpOp::Le => a.lo > b.hi,
            CmpOp::Gt => a.hi <= b.lo,
            CmpOp::Ge => a.hi < b.lo,
            CmpOp::Eq => a.hi < b.lo || b.hi < a.lo,
        }
    }
    /// Whether every pair of values satisfies `op` (always true).
    pub fn always(op: CmpOp, a: Interval, b: Interval) -> bool {
        if a.is_bottom() || b.is_bottom() {
            return false;
        }
        match op {
            CmpOp::Lt => a.hi < b.lo,
            CmpOp::Le => a.hi <= b.lo,
            CmpOp::Gt => a.lo > b.hi,
            CmpOp::Ge => a.lo >= b.hi,
            CmpOp::Eq => a.lo == a.hi && b.lo == b.hi && a.lo == b.lo,
        }
    }
    /// The gap between the two sides of a never-true comparison, relative to
    /// their magnitude (the margin of section 6.8).
    pub fn gap(op: CmpOp, a: Interval, b: Interval) -> f64 {
        let scale = [a.lo.abs(), a.hi.abs(), b.lo.abs(), b.hi.abs()].into_iter().filter(|x| x.is_finite()).fold(1.0, f64::max);
        let raw = match op {
            CmpOp::Lt | CmpOp::Le => a.lo - b.hi,
            CmpOp::Gt | CmpOp::Ge => b.lo - a.hi,
            CmpOp::Eq => (a.hi - b.lo).max(b.hi - a.lo).abs().min((b.lo - a.hi).max(a.lo - b.hi)),
        };
        if raw.is_nan() {
            f64::INFINITY
        } else {
            raw / scale
        }
    }
    /// The two sides refined by a satisfied comparison (forward refinement).
    pub fn refine(op: CmpOp, a: Interval, b: Interval) -> (Interval, Interval) {
        if a.is_bottom() || b.is_bottom() {
            return (Interval::BOTTOM, Interval::BOTTOM);
        }
        match op {
            CmpOp::Lt | CmpOp::Le => (Interval::new(a.lo, a.hi.min(b.hi)), Interval::new(b.lo.max(a.lo), b.hi)),
            CmpOp::Gt | CmpOp::Ge => (Interval::new(a.lo.max(b.lo), a.hi), Interval::new(b.lo, b.hi.min(a.hi))),
            CmpOp::Eq => {
                let m = a.meet(b);
                (m, m)
            }
        }
    }
    /// Uniform-assumption selectivity of `a op b` (section 5.2), when both
    /// intervals are finite.
    pub fn selectivity(op: CmpOp, a: Interval, b: Interval) -> Option<f64> {
        if a.is_bottom() || b.is_bottom() {
            return Some(0.0);
        }
        if !(a.lo.is_finite() && a.hi.is_finite() && b.lo.is_finite() && b.hi.is_finite()) {
            return None;
        }
        if Interval::never(op, a, b) {
            return Some(0.0);
        }
        if Interval::always(op, a, b) {
            return Some(1.0);
        }
        if op == CmpOp::Eq {
            return Some(0.05);
        }
        // P(X < Y) for X ~ U(a), Y ~ U(b), by a grid over X.
        let n = 64;
        let mut p = 0.0;
        for i in 0..n {
            let x = a.lo + (a.hi - a.lo) * (i as f64 + 0.5) / n as f64;
            let frac_above = if b.hi > b.lo {
                ((b.hi - x) / (b.hi - b.lo)).clamp(0.0, 1.0)
            } else if b.lo > x {
                1.0
            } else {
                0.0
            };
            p += frac_above;
        }
        let lt = p / n as f64;
        Some(match op {
            CmpOp::Lt | CmpOp::Le => lt,
            _ => 1.0 - lt,
        })
    }
    pub fn text(&self) -> String {
        if self.is_bottom() {
            return "⊥".into();
        }
        let f = |x: f64| {
            if x == f64::NEG_INFINITY {
                "-∞".to_string()
            } else if x == f64::INFINITY {
                "+∞".to_string()
            } else {
                format!("{:.6}", x).trim_end_matches('0').trim_end_matches('.').to_string()
            }
        };
        format!("[{}, {}]", f(self.lo), f(self.hi))
    }
}

/// The abstract value of a column or variable.
#[derive(Clone, Debug, PartialEq, serde::Serialize)]
pub struct AbsVal {
    /// The dimension, when the value is numeric and the typing is known.
    pub ty: Option<String>,
    pub sign: Sign,
    pub interval: Interval,
    pub may_undef: bool,
    /// The value is not numeric (an entity, a time, a label, a decision).
    pub numeric: bool,
}

impl AbsVal {
    pub fn non_numeric(ty: &Ty) -> AbsVal {
        AbsVal {
            ty: Some(ty.to_string()),
            sign: Sign::Top,
            interval: Interval::TOP,
            may_undef: false,
            numeric: false,
        }
    }
    pub fn top(ty: Option<&Ty>) -> AbsVal {
        AbsVal {
            ty: ty.map(|t| t.to_string()),
            sign: Sign::Top,
            interval: Interval::TOP,
            may_undef: false,
            numeric: true,
        }
    }
    pub fn bottom() -> AbsVal {
        AbsVal {
            ty: None,
            sign: Sign::Bot,
            interval: Interval::BOTTOM,
            may_undef: false,
            numeric: true,
        }
    }
    pub fn of_interval(ty: Option<&Ty>, i: Interval) -> AbsVal {
        AbsVal {
            ty: ty.map(|t| t.to_string()),
            sign: Sign::of_interval(i),
            interval: i,
            may_undef: false,
            numeric: true,
        }
        .reduce()
    }
    pub fn point(ty: Option<&Ty>, x: f64) -> AbsVal {
        AbsVal::of_interval(ty, Interval::point(x))
    }
    /// The reduced product: the interval refined by the sign at zero, the
    /// sign by the interval.
    pub fn reduce(mut self) -> AbsVal {
        if !self.numeric {
            return self;
        }
        let mut i = self.interval;
        if !i.is_bottom() {
            if !self.sign.may_be_negative() && i.lo < 0.0 {
                i.lo = 0.0;
            }
            if !self.sign.may_be_positive() && i.hi > 0.0 {
                i.hi = 0.0;
            }
            if !self.sign.may_be_zero() {
                if i.lo == 0.0 && i.hi > 0.0 {
                    i.lo = up(0.0);
                }
                if i.hi == 0.0 && i.lo < 0.0 {
                    i.hi = down(0.0);
                }
            }
        }
        self.interval = i;
        self.sign = self.sign.meet(Sign::of_interval(i));
        if self.sign == Sign::Bot {
            self.interval = Interval::BOTTOM;
        }
        self
    }
    pub fn join(&self, o: &AbsVal) -> AbsVal {
        if !self.numeric || !o.numeric {
            return AbsVal {
                ty: self.ty.clone().or_else(|| o.ty.clone()),
                sign: Sign::Top,
                interval: Interval::TOP,
                may_undef: self.may_undef || o.may_undef,
                numeric: false,
            };
        }
        if self.sign == Sign::Bot {
            return o.clone();
        }
        if o.sign == Sign::Bot {
            return self.clone();
        }
        AbsVal {
            ty: self.ty.clone().or_else(|| o.ty.clone()),
            sign: self.sign.join(o.sign),
            interval: self.interval.join(o.interval),
            may_undef: self.may_undef || o.may_undef,
            numeric: true,
        }
        .reduce()
    }
    pub fn meet(&self, o: &AbsVal) -> AbsVal {
        if !self.numeric || !o.numeric {
            return self.clone();
        }
        AbsVal {
            ty: self.ty.clone().or_else(|| o.ty.clone()),
            sign: self.sign.meet(o.sign),
            interval: self.interval.meet(o.interval),
            may_undef: self.may_undef || o.may_undef,
            numeric: true,
        }
        .reduce()
    }
    pub fn with_interval(&self, i: Interval) -> AbsVal {
        AbsVal {
            interval: i,
            sign: self.sign.meet(Sign::of_interval(i)),
            ..self.clone()
        }
        .reduce()
    }
    pub fn binop(&self, op: BinOp, o: &AbsVal) -> AbsVal {
        let ty = match (&self.ty, &o.ty) {
            (Some(a), Some(b)) => match (Ty::parse_text(a), Ty::parse_text(b)) {
                (Some(ta), Some(tb)) => types::bin_type(op, &ta, &tb).ok().map(|t| t.to_string()),
                _ => None,
            },
            _ => None,
        };
        let (interval, sign) = match op {
            BinOp::Add => (self.interval.add(o.interval), self.sign.add(o.sign)),
            BinOp::Sub => (self.interval.sub(o.interval), self.sign.add(o.sign.neg())),
            BinOp::Mul => (self.interval.mul(o.interval), self.sign.mul(o.sign)),
            BinOp::Div => {
                let s = if o.sign.may_be_zero() { Sign::Top } else { self.sign.mul(o.sign) };
                (self.interval.div(o.interval), s)
            }
        };
        AbsVal {
            ty,
            sign,
            interval,
            may_undef: self.may_undef || o.may_undef || (op == BinOp::Div && (o.sign.may_be_zero() || o.interval.contains(0.0))),
            numeric: true,
        }
        .reduce()
    }
    pub fn text(&self) -> String {
        if !self.numeric {
            return self.ty.clone().unwrap_or_else(|| "non-numeric".into());
        }
        let mut s = format!("{} {}", self.sign.text(), self.interval.text());
        if self.may_undef {
            s.push_str(" may be ⊥");
        }
        s
    }
}

impl Ty {
    /// A type back from its rendering (what the abstract values carry).
    pub fn parse_text(s: &str) -> Option<Ty> {
        if let Some(inner) = s.strip_prefix("Price<").and_then(|r| r.strip_suffix('>')) {
            return Ty::parse("Price", Some(inner));
        }
        if let Some(inner) = s.strip_prefix("Notional<").and_then(|r| r.strip_suffix('>')) {
            return Ty::parse("Notional", Some(inner));
        }
        if s == "Quantity<Shares>" {
            return Ty::parse("Quantity", Some("Shares"));
        }
        if s == "integer literal" {
            return Some(Ty::IntLit);
        }
        Ty::parse(s, None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interval_arithmetic_is_outward_and_sound() {
        let a = Interval::new(1.0, 2.0);
        let b = Interval::new(-1.0, 3.0);
        let s = a.add(b);
        assert!(s.lo <= 0.0 && s.hi >= 5.0);
        let p = a.mul(b);
        assert!(p.lo <= -2.0 && p.hi >= 6.0);
        assert!(a.div(b).is_top());
        let d = a.div(Interval::new(2.0, 4.0));
        assert!(d.lo <= 0.25 && d.hi >= 1.0);
        assert!(Interval::never(CmpOp::Gt, Interval::new(0.0, 1.0), Interval::new(2.0, 3.0)));
        assert!(Interval::always(CmpOp::Lt, Interval::new(0.0, 1.0), Interval::new(2.0, 3.0)));
        let (x, y) = Interval::refine(CmpOp::Lt, Interval::new(0.0, 10.0), Interval::new(5.0, 6.0));
        assert_eq!((x.lo, x.hi, y.lo, y.hi), (0.0, 6.0, 5.0, 6.0));
    }

    #[test]
    fn signs_and_the_reduced_product() {
        assert_eq!(Sign::Pos.mul(Sign::Neg), Sign::Neg);
        assert_eq!(Sign::NonNeg.add(Sign::Pos), Sign::Pos);
        assert_eq!(Sign::Top.meet(Sign::Pos), Sign::Pos);
        let v = AbsVal {
            ty: None,
            sign: Sign::Pos,
            interval: Interval::new(-1.0, 5.0),
            may_undef: false,
            numeric: true,
        }
        .reduce();
        assert!(v.interval.lo > 0.0 && v.interval.hi == 5.0);
        let sel = Interval::selectivity(CmpOp::Lt, Interval::new(0.0, 1.0), Interval::new(0.0, 1.0)).unwrap();
        assert!((sel - 0.5).abs() < 0.05, "{}", sel);
    }
}
