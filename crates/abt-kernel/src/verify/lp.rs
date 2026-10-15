//! Tier 2, linear feasibility (compiler design, section 6.3): an exact
//! rational simplex with Bland's rule, in the crate, for systems of at
//! most 64 constraints and 32 variables, with Fourier–Motzkin elimination
//! as the fallback for at most 6 variables. The verdict licenses a rewrite
//! (R2), so it is exact: rationals over 128-bit integers with checked
//! arithmetic, and an overflow is "undecided", never a wrong answer.

#![allow(clippy::should_implement_trait)]

use std::cmp::Ordering;

/// A rational over `i128`, normalised (positive denominator, reduced).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Q {
    n: i128,
    d: i128,
}

fn gcd(a: i128, b: i128) -> i128 {
    let (mut a, mut b) = (a.abs(), b.abs());
    while b != 0 {
        let t = a % b;
        a = b;
        b = t;
    }
    a
}

impl Q {
    pub const ZERO: Q = Q { n: 0, d: 1 };
    pub const ONE: Q = Q { n: 1, d: 1 };

    pub fn new(n: i128, d: i128) -> Option<Q> {
        if d == 0 {
            return None;
        }
        let g = gcd(n, d).max(1);
        let (mut n, mut d) = (n / g, d / g);
        if d < 0 {
            n = n.checked_neg()?;
            d = d.checked_neg()?;
        }
        Some(Q { n, d })
    }
    pub fn int(n: i128) -> Q {
        Q { n, d: 1 }
    }
    /// The numerator and the (positive) denominator.
    pub fn parts(self) -> (i128, i128) {
        (self.n, self.d)
    }
    pub fn abs(self) -> Q {
        if self.n < 0 {
            Q { n: -self.n, d: self.d }
        } else {
            self
        }
    }
    /// The exact rational value of a double (every finite double is one),
    /// when it fits in 128 bits.
    pub fn of_f64(x: f64) -> Option<Q> {
        if !x.is_finite() {
            return None;
        }
        if x == 0.0 {
            return Some(Q::ZERO);
        }
        let bits = x.to_bits();
        let sign = if bits >> 63 == 0 { 1i128 } else { -1i128 };
        let exp = ((bits >> 52) & 0x7ff) as i64;
        let frac = (bits & ((1u64 << 52) - 1)) as i128;
        let (mantissa, exp2) = if exp == 0 { (frac, -1074i64) } else { (frac | (1i128 << 52), exp - 1075) };
        if exp2 >= 0 {
            if exp2 > 70 {
                return None;
            }
            let n = mantissa.checked_mul(1i128 << exp2)?;
            Q::new(sign * n, 1)
        } else {
            let shift = -exp2;
            if shift > 120 {
                return None;
            }
            Q::new(sign * mantissa, 1i128 << shift)
        }
    }
    /// The rational of a double's shortest decimal form (`0.01` is
    /// `1/100`, not the double's own dyadic value): the rational a program
    /// literal denotes, whose double rounding lies far below the margin of
    /// section 6.8. Long decimals fall back to the exact dyadic form.
    pub fn of_decimal(x: f64) -> Option<Q> {
        if !x.is_finite() {
            return None;
        }
        let text = format!("{}", x);
        let (neg, body) = match text.strip_prefix('-') {
            Some(b) => (true, b),
            None => (false, text.as_str()),
        };
        let (int, frac) = body.split_once('.').unwrap_or((body, ""));
        if int.len() + frac.len() > 30 || body.contains('e') {
            return Q::of_f64(x);
        }
        let mut n: i128 = 0;
        for c in int.chars().chain(frac.chars()) {
            n = n.checked_mul(10)?.checked_add(c.to_digit(10)? as i128)?;
        }
        let d = 10i128.checked_pow(frac.len() as u32)?;
        Q::new(if neg { -n } else { n }, d)
    }
    /// The rational rounded down (`up` false) or up to a multiple of
    /// `1/scale`: a shorter rational on the loose side of a bound.
    pub fn round_to(self, scale: i128, up: bool) -> Option<Q> {
        let num = self.n.checked_mul(scale)?;
        let q = num.div_euclid(self.d);
        let r = num.rem_euclid(self.d);
        let k = if up && r != 0 { q.checked_add(1)? } else { q };
        Q::new(k, scale)
    }
    /// A decimal rendering when the denominator divides a power of ten,
    /// `n/d` otherwise.
    pub fn decimal_text(self) -> String {
        if self.d == 1 {
            return format!("{}", self.n);
        }
        let mut scale: i128 = 1;
        for k in 1..=24u32 {
            scale = match scale.checked_mul(10) {
                Some(s) => s,
                None => break,
            };
            if scale % self.d == 0 {
                let m = scale / self.d;
                let num = match self.n.checked_mul(m) {
                    Some(v) => v,
                    None => break,
                };
                let neg = num < 0;
                let digits = format!("{:0width$}", num.abs(), width = k as usize + 1);
                let (int, frac) = digits.split_at(digits.len() - k as usize);
                let frac = frac.trim_end_matches('0');
                return format!("{}{}{}{}", if neg { "-" } else { "" }, int, if frac.is_empty() { "" } else { "." }, frac);
            }
        }
        format!("{}", self)
    }
    pub fn add(self, o: Q) -> Option<Q> {
        let n = self.n.checked_mul(o.d)?.checked_add(o.n.checked_mul(self.d)?)?;
        Q::new(n, self.d.checked_mul(o.d)?)
    }
    pub fn sub(self, o: Q) -> Option<Q> {
        self.add(o.neg())
    }
    pub fn mul(self, o: Q) -> Option<Q> {
        Q::new(self.n.checked_mul(o.n)?, self.d.checked_mul(o.d)?)
    }
    pub fn div(self, o: Q) -> Option<Q> {
        if o.n == 0 {
            return None;
        }
        Q::new(self.n.checked_mul(o.d)?, self.d.checked_mul(o.n)?)
    }
    pub fn neg(self) -> Q {
        Q { n: -self.n, d: self.d }
    }
    pub fn is_zero(self) -> bool {
        self.n == 0
    }
    pub fn is_negative(self) -> bool {
        self.n < 0
    }
    pub fn is_positive(self) -> bool {
        self.n > 0
    }
    pub fn to_f64(self) -> f64 {
        self.n as f64 / self.d as f64
    }
}

impl PartialOrd for Q {
    fn partial_cmp(&self, o: &Q) -> Option<Ordering> {
        Some(self.cmp(o))
    }
}
impl Ord for Q {
    fn cmp(&self, o: &Q) -> Ordering {
        // Both denominators positive: compare cross products in i128 when
        // they fit, else by the floating-point values (only used to order
        // pivots, where a mistake costs time, never the verdict).
        match (self.n.checked_mul(o.d), o.n.checked_mul(self.d)) {
            (Some(a), Some(b)) => a.cmp(&b),
            _ => self.to_f64().partial_cmp(&o.to_f64()).unwrap_or(Ordering::Equal),
        }
    }
}

impl std::fmt::Display for Q {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.d == 1 {
            write!(f, "{}", self.n)
        } else {
            write!(f, "{}/{}", self.n, self.d)
        }
    }
}

/// A linear constraint `Σ coef[i]·x[i] (op) rhs`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Rel {
    Le,
    Lt,
    Eq,
}

#[derive(Clone, Debug)]
pub struct Constraint {
    pub coef: Vec<Q>,
    pub rel: Rel,
    pub rhs: Q,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Feasibility {
    Feasible,
    Infeasible,
    /// Beyond the size limits or the arithmetic's range.
    Undecided,
}

pub const MAX_CONSTRAINTS: usize = 64;
pub const MAX_VARIABLES: usize = 32;
pub const FM_VARIABLES: usize = 6;

/// Feasibility over the reals of a system of linear constraints in `nvars`
/// free variables: Fourier–Motzkin for small systems, exact simplex
/// otherwise.
pub fn feasible(nvars: usize, constraints: &[Constraint]) -> Feasibility {
    if constraints.is_empty() {
        return Feasibility::Feasible;
    }
    if constraints.len() > MAX_CONSTRAINTS || nvars > MAX_VARIABLES {
        return Feasibility::Undecided;
    }
    if nvars <= FM_VARIABLES {
        if let Some(v) = fourier_motzkin(nvars, constraints) {
            return v;
        }
    }
    simplex(nvars, constraints)
}

/// Strict inequalities are handled by a slack: `a·x < b` becomes
/// `a·x + s ≤ b` with a shared `s > 0`, feasible over the reals iff the
/// system with `s` free and the optimisation `max s` admits `s > 0`. The
/// simplex here maximises `s` under the non-strict system and reads the
/// verdict off its optimum.
fn simplex(nvars: usize, constraints: &[Constraint]) -> Feasibility {
    // Variables: x_i = p_i − q_i (free), plus s (the strictness slack,
    // bounded by 1), as non-negative variables; equalities as two
    // inequalities. Phase 1 of a two-phase simplex decides feasibility
    // with s ≥ 0; the strict ones need the optimum of s to be positive.
    let has_strict = constraints.iter().any(|c| c.rel == Rel::Lt);
    let mut rows: Vec<(Vec<Q>, Q)> = Vec::new();
    // Columns: 2·nvars (p, q), then s.
    let ncols = 2 * nvars + 1;
    for c in constraints {
        let mut row = vec![Q::ZERO; ncols];
        for (i, a) in c.coef.iter().enumerate() {
            row[2 * i] = *a;
            row[2 * i + 1] = a.neg();
        }
        match c.rel {
            Rel::Le => rows.push((row, c.rhs)),
            Rel::Lt => {
                row[ncols - 1] = Q::ONE;
                rows.push((row, c.rhs));
            }
            Rel::Eq => {
                let neg: Vec<Q> = row.iter().map(|q| q.neg()).collect();
                rows.push((row, c.rhs));
                rows.push((neg, c.rhs.neg()));
            }
        }
    }
    // s ≤ 1 keeps the optimum bounded.
    let mut srow = vec![Q::ZERO; ncols];
    srow[ncols - 1] = Q::ONE;
    rows.push((srow, Q::ONE));
    // Standard form: A y ≤ b, y ≥ 0; add slacks to make b ≥ 0 and start
    // with artificials where b < 0 (phase 1).
    let m = rows.len();
    let n = ncols;
    // Tableau columns: y (n), slacks (m), artificials (m), rhs.
    let width = n + 2 * m + 1;
    let mut t: Vec<Vec<Q>> = Vec::with_capacity(m + 1);
    let mut basis: Vec<usize> = Vec::with_capacity(m);
    let mut artificial: Vec<bool> = vec![false; m];
    for (i, (row, rhs)) in rows.iter().enumerate() {
        let mut r = vec![Q::ZERO; width];
        let flip = rhs.is_negative();
        for (j, a) in row.iter().enumerate() {
            r[j] = if flip { a.neg() } else { *a };
        }
        // slack
        r[n + i] = if flip { Q::int(-1) } else { Q::ONE };
        let b = if flip { rhs.neg() } else { *rhs };
        r[width - 1] = b;
        if flip {
            r[n + m + i] = Q::ONE;
            basis.push(n + m + i);
            artificial[i] = true;
        } else {
            basis.push(n + i);
        }
        t.push(r);
    }
    // Phase 1 objective: minimise the sum of artificials = maximise −Σ a_i.
    let mut obj = vec![Q::ZERO; width];
    for i in 0..m {
        if artificial[i] {
            for j in 0..width {
                obj[j] = match obj[j].sub(t[i][j]) {
                    Some(v) => v,
                    None => return Feasibility::Undecided,
                };
            }
        }
    }
    for i in 0..m {
        if artificial[i] {
            obj[n + m + i] = Q::ZERO;
        }
    }
    if run_simplex(&mut t, &mut obj, &mut basis, width).is_none() {
        return Feasibility::Undecided;
    }
    // Infeasible when artificials remain positive: the phase-1 optimum is below zero.
    if obj[width - 1].is_negative() {
        return Feasibility::Infeasible;
    }
    if !has_strict {
        return Feasibility::Feasible;
    }
    // Phase 2: maximise s (column n − 1) over the feasible region, with the
    // artificial columns forbidden.
    let forbidden: Vec<usize> = (n + m..n + 2 * m).collect();
    let mut obj2 = vec![Q::ZERO; width];
    obj2[n - 1] = Q::int(-1);
    // Express the objective in terms of the non-basic variables.
    for (i, &b) in basis.iter().enumerate() {
        if b == n - 1 {
            for j in 0..width {
                obj2[j] = match obj2[j].add(t[i][j]) {
                    Some(v) => v,
                    None => return Feasibility::Undecided,
                };
            }
        }
    }
    if run_simplex_forbidding(&mut t, &mut obj2, &mut basis, width, &forbidden).is_none() {
        return Feasibility::Undecided;
    }
    // The optimum of s is the objective's value.
    if obj2[width - 1].is_positive() {
        Feasibility::Feasible
    } else {
        Feasibility::Infeasible
    }
}

fn run_simplex(t: &mut [Vec<Q>], obj: &mut [Q], basis: &mut [usize], width: usize) -> Option<()> {
    run_simplex_forbidding(t, obj, basis, width, &[])
}

/// Maximise −obj over the tableau by Bland's rule (smallest entering index
/// with a negative reduced cost, smallest leaving row on ties); `None` on
/// an arithmetic overflow.
fn run_simplex_forbidding(t: &mut [Vec<Q>], obj: &mut [Q], basis: &mut [usize], width: usize, forbidden: &[usize]) -> Option<()> {
    let m = t.len();
    for _ in 0..10_000 {
        // Entering: the smallest column with a negative reduced cost.
        let Some(col) = (0..width - 1).find(|&j| obj[j].is_negative() && !forbidden.contains(&j)) else {
            return Some(());
        };
        // Leaving: the minimum ratio, ties to the smallest basis index.
        let mut best: Option<(usize, Q)> = None;
        for i in 0..m {
            if t[i][col].is_positive() {
                let ratio = t[i][width - 1].div(t[i][col])?;
                let better = match &best {
                    None => true,
                    Some((bi, br)) => ratio < *br || (ratio == *br && basis[i] < basis[*bi]),
                };
                if better {
                    best = Some((i, ratio));
                }
            }
        }
        let Some((row, _)) = best else {
            // Unbounded in this direction: the objective can grow without bound.
            return Some(());
        };
        // Pivot.
        let p = t[row][col];
        for v in t[row].iter_mut().take(width) {
            *v = v.div(p)?;
        }
        let pivot_row: Vec<Q> = t[row].clone();
        for (i, row_i) in t.iter_mut().enumerate().take(m) {
            if i != row && !row_i[col].is_zero() {
                let f = row_i[col];
                for (v, r) in row_i.iter_mut().zip(pivot_row.iter()).take(width) {
                    *v = v.sub(r.mul(f)?)?;
                }
            }
        }
        if !obj[col].is_zero() {
            let f = obj[col];
            for j in 0..width {
                let v = t[row][j].mul(f)?;
                obj[j] = obj[j].sub(v)?;
            }
        }
        basis[row] = col;
    }
    None
}

/// Fourier–Motzkin elimination (exact): eliminate variables one by one;
/// a contradiction `0 < 0` or `0 ≤ c` with c < 0 is infeasibility. `None`
/// when the system grows past the budget or the arithmetic overflows.
fn fourier_motzkin(nvars: usize, constraints: &[Constraint]) -> Option<Feasibility> {
    // Normalise to `a·x (≤ | <) b`.
    let mut sys: Vec<(Vec<Q>, bool, Q)> = Vec::new();
    for c in constraints {
        match c.rel {
            Rel::Le => sys.push((c.coef.clone(), false, c.rhs)),
            Rel::Lt => sys.push((c.coef.clone(), true, c.rhs)),
            Rel::Eq => {
                sys.push((c.coef.clone(), false, c.rhs));
                sys.push((c.coef.iter().map(|q| q.neg()).collect(), false, c.rhs.neg()));
            }
        }
    }
    for v in 0..nvars {
        let (pos, rest): (Vec<_>, Vec<_>) = sys.into_iter().partition(|(a, _, _)| a[v].is_positive());
        let (neg, zero): (Vec<_>, Vec<_>) = rest.into_iter().partition(|(a, _, _)| a[v].is_negative());
        let mut next = zero;
        for (ap, sp, bp) in &pos {
            for (an, sn, bn) in &neg {
                // ap·x ≤ bp with ap[v] > 0, an·x ≤ bn with an[v] < 0:
                // combine as (−an[v])·(ap·x) + ap[v]·(an·x) ≤ (−an[v])·bp + ap[v]·bn.
                let fp = an[v].neg();
                let fn_ = ap[v];
                let mut row = Vec::with_capacity(nvars);
                for j in 0..nvars {
                    row.push(ap[j].mul(fp)?.add(an[j].mul(fn_)?)?);
                }
                let rhs = bp.mul(fp)?.add(bn.mul(fn_)?)?;
                next.push((row, *sp || *sn, rhs));
                if next.len() > 4096 {
                    return None;
                }
            }
        }
        sys = next;
    }
    for (a, strict, b) in &sys {
        if a.iter().all(|q| q.is_zero()) && (b.is_negative() || (*strict && b.is_zero())) {
            return Some(Feasibility::Infeasible);
        }
    }
    Some(Feasibility::Feasible)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn c(coef: &[i128], rel: Rel, rhs: i128) -> Constraint {
        Constraint {
            coef: coef.iter().map(|&x| Q::int(x)).collect(),
            rel,
            rhs: Q::int(rhs),
        }
    }

    #[test]
    fn decimal_forms_round_and_print() {
        assert_eq!(Q::of_decimal(0.01), Q::new(1, 100));
        assert_eq!(Q::of_decimal(-2.5), Q::new(-5, 2));
        assert_eq!(Q::of_decimal(3.0), Some(Q::int(3)));
        assert_eq!(Q::of_decimal(0.1).unwrap().decimal_text(), "0.1");
        assert_eq!(Q::new(-1, 8).unwrap().decimal_text(), "-0.125");
        assert_eq!(Q::new(1, 3).unwrap().decimal_text(), "1/3");
        assert_eq!(Q::int(7).decimal_text(), "7");
        let q = Q::of_f64(46.10512345678).unwrap();
        let down = q.round_to(1000, false).unwrap();
        let up = q.round_to(1000, true).unwrap();
        assert!(down <= q && q <= up);
        assert_eq!(down.decimal_text(), "46.105");
        assert_eq!(up.decimal_text(), "46.106");
        assert_eq!(Q::new(-7, 2).unwrap().round_to(1, false).unwrap(), Q::int(-4));
        assert_eq!(Q::new(-7, 2).unwrap().round_to(1, true).unwrap(), Q::int(-3));
    }

    #[test]
    fn rationals_are_exact() {
        let a = Q::new(1, 3).unwrap();
        let b = Q::new(2, 3).unwrap();
        assert_eq!(a.add(b).unwrap(), Q::ONE);
        assert_eq!(Q::of_f64(0.5).unwrap(), Q::new(1, 2).unwrap());
        assert_eq!(Q::of_f64(0.1).unwrap(), Q::new(3602879701896397, 36028797018963968).unwrap());
        assert!(Q::of_f64(1e-300).is_none());
    }

    #[test]
    fn the_example_of_section_6_3_is_infeasible() {
        // P > P + delta with delta in (0, 0.1]: −delta > 0 and delta > 0.
        // Variables: P, delta.
        let sys = vec![
            // P − (P + delta) > 0  ⇔  delta < 0
            c(&[0, 1], Rel::Lt, 0),
            // delta > 0  ⇔  −delta < 0
            c(&[0, -1], Rel::Lt, 0),
            // delta ≤ 1/10
            Constraint {
                coef: vec![Q::ZERO, Q::ONE],
                rel: Rel::Le,
                rhs: Q::new(1, 10).unwrap(),
            },
        ];
        assert_eq!(feasible(2, &sys), Feasibility::Infeasible);
        assert_eq!(simplex(2, &sys), Feasibility::Infeasible);
    }

    #[test]
    fn feasible_systems_are_feasible_by_both_procedures() {
        // x > 0, y > 0, x + y ≤ 1.
        let sys = vec![c(&[-1, 0], Rel::Lt, 0), c(&[0, -1], Rel::Lt, 0), c(&[1, 1], Rel::Le, 1)];
        assert_eq!(fourier_motzkin(2, &sys), Some(Feasibility::Feasible));
        assert_eq!(simplex(2, &sys), Feasibility::Feasible);
        // Touching: x ≤ 0 and x ≥ 0 is feasible (x = 0), x < 0 and x ≥ 0 is not.
        assert_eq!(feasible(1, &[c(&[1], Rel::Le, 0), c(&[-1], Rel::Le, 0)]), Feasibility::Feasible);
        assert_eq!(feasible(1, &[c(&[1], Rel::Lt, 0), c(&[-1], Rel::Le, 0)]), Feasibility::Infeasible);
        assert_eq!(simplex(1, &[c(&[1], Rel::Lt, 0), c(&[-1], Rel::Le, 0)]), Feasibility::Infeasible);
        // Equalities.
        assert_eq!(feasible(2, &[c(&[1, 1], Rel::Eq, 2), c(&[1, -1], Rel::Eq, 0), c(&[1, 0], Rel::Le, 1)]), Feasibility::Feasible);
        assert_eq!(feasible(2, &[c(&[1, 1], Rel::Eq, 2), c(&[1, -1], Rel::Eq, 0), c(&[1, 0], Rel::Lt, 1)]), Feasibility::Infeasible);
    }

    #[test]
    fn larger_systems_go_to_the_simplex() {
        // Eight variables in a chain x0 < x1 < ... < x7 < x0: infeasible.
        let n = 8;
        let mut sys = Vec::new();
        for i in 0..n {
            let mut coef = vec![0i128; n];
            coef[i] = 1;
            coef[(i + 1) % n] = -1;
            sys.push(c(&coef, Rel::Lt, 0));
        }
        assert_eq!(feasible(n, &sys), Feasibility::Infeasible);
        sys.pop();
        assert_eq!(feasible(n, &sys), Feasibility::Feasible);
    }
}
