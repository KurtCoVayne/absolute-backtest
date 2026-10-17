//! A small dense two-phase simplex, for `reg_t`'s minimum over admissible
//! pairings (K-5.6).
//!
//! The problem is to minimise c·x subject to rows a·x ≤ b, a·x ≥ b or a·x =
//! b, and x ≥ 0. Each row is first turned so that b ≥ 0 and divided by its
//! largest coefficient; the right-hand sides are then divided by the largest
//! of them (x scales with them) and the costs by the largest cost, so that
//! the tolerance [`TOL`] is relative to the data. A `≤` row gets a slack
//! variable, a `≥` row a surplus and an artificial one, an `=` row an
//! artificial one.
//!
//! Phase 1 minimises the sum of the artificial variables from the basis of
//! slacks and artificials; a minimum above `TOL` means that no point is
//! feasible. An artificial variable still basic at the end (at zero) is
//! pivoted out on the largest entry of its row among the other columns; a
//! row with none is redundant and stays, inert. Phase 2 minimises the costs
//! from that basis, the artificial columns barred from entering.
//!
//! Both phases pivot by Bland's rule (R. G. Bland, "New finite pivoting
//! rules for the simplex method", 1977): the entering column is the
//! lowest-indexed one whose reduced cost is below −`TOL`, and the leaving row
//! the one of least ratio, ties within `TOL` going to the lowest-indexed
//! basic variable. The rule cannot cycle, so a degenerate program ends; a
//! bound on the iterations guards against what rounding could still do.

/// The tolerance of the pivoting rules and of phase 1's verdict, on the
/// scaled data.
pub const TOL: f64 = 1e-9;

/// The sense of a row.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Cmp {
    Le,
    Ge,
    Eq,
}

/// A row `coef · x (cmp) rhs`.
#[derive(Clone, Debug, PartialEq)]
pub struct Constraint {
    pub coef: Vec<f64>,
    pub cmp: Cmp,
    pub rhs: f64,
}

/// A minimising point and the objective there.
#[derive(Clone, Debug, PartialEq)]
pub struct Solution {
    pub x: Vec<f64>,
    pub objective: f64,
}

/// Why no minimum was found.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LpError {
    /// A row of another length than the costs, or a number not finite.
    Malformed,
    Infeasible,
    Unbounded,
    /// The bound on the iterations was reached.
    IterationLimit,
}

/// The minimum of `c · x` subject to `rows` and `x ≥ 0`.
pub fn minimize(c: &[f64], rows: &[Constraint]) -> Result<Solution, LpError> {
    let finite = |v: &[f64]| v.iter().all(|x| x.is_finite());
    if !finite(c) || rows.iter().any(|r| r.coef.len() != c.len() || !r.rhs.is_finite() || !finite(&r.coef)) {
        return Err(LpError::Malformed);
    }
    let mut tableau = Tableau::new(c, rows);
    tableau.phase_one()?;
    tableau.phase_two()?;
    let x = tableau.primal();
    let objective = c.iter().zip(&x).map(|(c, x)| c * x).sum();
    Ok(Solution { x, objective })
}

/// The dense tableau: a row per constraint over the original columns, then
/// the slacks and surpluses, then the artificial columns, then the
/// right-hand side.
struct Tableau {
    rows: Vec<Vec<f64>>,
    basis: Vec<usize>,
    /// The number of original variables.
    n: usize,
    /// The first artificial column.
    artificial: usize,
    /// The number of columns, and the index of the right-hand side.
    cols: usize,
    /// The original variables' costs, scaled.
    cost: Vec<f64>,
    /// x = scale × the tableau's values.
    scale: f64,
}

/// The largest absolute value, or 1 when there is none.
fn largest(values: impl Iterator<Item = f64>) -> f64 {
    let big = values.fold(0.0f64, |m, v| m.max(v.abs()));
    if big > 0.0 {
        big
    } else {
        1.0
    }
}

/// `row -= row[j] × pivot`, clearing column `j`.
fn eliminate(row: &mut [f64], pivot: &[f64], j: usize) {
    let f = row[j];
    if f != 0.0 {
        for (v, p) in row.iter_mut().zip(pivot) {
            *v -= f * p;
        }
        row[j] = 0.0;
    }
}

impl Tableau {
    fn new(c: &[f64], constraints: &[Constraint]) -> Tableau {
        let n = c.len();
        let turned: Vec<(Vec<f64>, Cmp, f64)> = constraints
            .iter()
            .map(|r| {
                let flip = r.rhs < 0.0;
                let sign = if flip { -1.0 } else { 1.0 };
                let k = sign / largest(r.coef.iter().copied());
                let cmp = match (r.cmp, flip) {
                    (Cmp::Le, true) => Cmp::Ge,
                    (Cmp::Ge, true) => Cmp::Le,
                    (cmp, _) => cmp,
                };
                (r.coef.iter().map(|a| a * k).collect(), cmp, r.rhs * k)
            })
            .collect();
        let scale = largest(turned.iter().map(|r| r.2));
        let slacks = turned.iter().filter(|r| r.1 != Cmp::Eq).count();
        let artificials = turned.iter().filter(|r| r.1 != Cmp::Le).count();
        let cols = n + slacks + artificials;
        let (mut slack, mut artificial) = (n, n + slacks);
        let mut rows = Vec::with_capacity(turned.len());
        let mut basis = Vec::with_capacity(turned.len());
        for (coef, cmp, rhs) in turned {
            let mut row = coef;
            row.resize(cols + 1, 0.0);
            row[cols] = rhs / scale;
            if cmp != Cmp::Eq {
                row[slack] = if cmp == Cmp::Le { 1.0 } else { -1.0 };
                slack += 1;
            }
            if cmp == Cmp::Le {
                basis.push(slack - 1);
            } else {
                row[artificial] = 1.0;
                basis.push(artificial);
                artificial += 1;
            }
            rows.push(row);
        }
        let big = largest(c.iter().copied());
        Tableau {
            rows,
            basis,
            n,
            artificial: n + slacks,
            cols,
            cost: c.iter().map(|x| x / big).collect(),
            scale,
        }
    }

    /// Phase 1: a feasible basis of the original and slack columns.
    fn phase_one(&mut self) -> Result<(), LpError> {
        let mut d = vec![0.0; self.cols + 1];
        d[self.artificial..self.cols].fill(1.0);
        for (row, &b) in self.rows.iter().zip(&self.basis) {
            if b >= self.artificial {
                for (dj, a) in d.iter_mut().zip(row) {
                    *dj -= a;
                }
            }
        }
        self.run(&mut d, self.cols)?;
        if -d[self.cols] > TOL {
            return Err(LpError::Infeasible);
        }
        self.drive_out();
        Ok(())
    }

    /// Pivots the artificial variables still basic (at zero) out of the
    /// basis where their row allows it.
    fn drive_out(&mut self) {
        let stuck: Vec<usize> = self.basis.iter().enumerate().filter(|(_, &b)| b >= self.artificial).map(|(r, _)| r).collect();
        for r in stuck {
            let row = &self.rows[r];
            let best = (0..self.artificial).max_by(|&i, &j| row[i].abs().total_cmp(&row[j].abs()));
            if let Some(j) = best.filter(|&j| row[j].abs() > TOL) {
                self.rows[r][self.cols] = 0.0;
                self.pivot(r, j, None);
            }
        }
    }

    /// Phase 2: the minimum of the costs, the artificial columns barred.
    fn phase_two(&mut self) -> Result<(), LpError> {
        let mut d = vec![0.0; self.cols + 1];
        d[..self.n].copy_from_slice(&self.cost);
        for (row, &b) in self.rows.iter().zip(&self.basis) {
            let cb = if b < self.n { self.cost[b] } else { 0.0 };
            if cb != 0.0 {
                for (dj, a) in d.iter_mut().zip(row) {
                    *dj -= cb * a;
                }
            }
        }
        self.run(&mut d, self.artificial)
    }

    /// Bland's rule over the reduced costs `d` (the last entry minus the
    /// objective), the columns below `enter` free to enter.
    fn run(&mut self, d: &mut [f64], enter: usize) -> Result<(), LpError> {
        let limit = 50 * (self.rows.len() + self.cols) + 100;
        for _ in 0..limit {
            let Some(j) = (0..enter).find(|&j| d[j] < -TOL) else {
                return Ok(());
            };
            let Some(r) = self.leaving(j) else {
                return Err(LpError::Unbounded);
            };
            self.pivot(r, j, Some(&mut *d));
        }
        Err(LpError::IterationLimit)
    }

    /// The row of least ratio in column `j`, ties to the lowest basic
    /// variable.
    fn leaving(&self, j: usize) -> Option<usize> {
        let mut best: Option<(usize, f64)> = None;
        for (r, row) in self.rows.iter().enumerate() {
            if row[j] <= TOL {
                continue;
            }
            let ratio = row[self.cols].max(0.0) / row[j];
            best = match best {
                Some((b, q)) if ratio > q + TOL || (ratio >= q - TOL && self.basis[b] < self.basis[r]) => Some((b, q)),
                _ => Some((r, ratio)),
            };
        }
        best.map(|(r, _)| r)
    }

    fn pivot(&mut self, r: usize, j: usize, d: Option<&mut [f64]>) {
        let p = self.rows[r][j];
        for v in self.rows[r].iter_mut() {
            *v /= p;
        }
        self.rows[r][j] = 1.0;
        let pivot = self.rows[r].clone();
        for (i, row) in self.rows.iter_mut().enumerate() {
            if i != r {
                eliminate(row, &pivot, j);
            }
        }
        if let Some(d) = d {
            eliminate(d, &pivot, j);
        }
        self.basis[r] = j;
    }

    /// The original variables' values.
    fn primal(&self) -> Vec<f64> {
        let mut x = vec![0.0; self.n];
        for (row, &b) in self.rows.iter().zip(&self.basis) {
            if b < self.n {
                x[b] = row[self.cols].max(0.0) * self.scale;
            }
        }
        x
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(coef: &[f64], cmp: Cmp, rhs: f64) -> Constraint {
        Constraint { coef: coef.to_vec(), cmp, rhs }
    }

    fn close(a: f64, b: f64) -> bool {
        (a - b).abs() <= 1e-9 * (1.0 + b.abs())
    }

    #[test]
    fn a_textbook_program() {
        // min −x − y, x + 2y ≤ 4, 3x + y ≤ 6: the vertex where both bind,
        // x = 8/5, y = 6/5, objective −14/5.
        let s = minimize(&[-1.0, -1.0], &[row(&[1.0, 2.0], Cmp::Le, 4.0), row(&[3.0, 1.0], Cmp::Le, 6.0)]).unwrap();
        assert!(close(s.x[0], 1.6) && close(s.x[1], 1.2) && close(s.objective, -2.8), "{s:?}");
    }

    #[test]
    fn equalities_and_lower_bounds_need_phase_one() {
        // min x + y, x + y ≥ 2, x − y = 0: x = y = 1, objective 2.
        let s = minimize(&[1.0, 1.0], &[row(&[1.0, 1.0], Cmp::Ge, 2.0), row(&[1.0, -1.0], Cmp::Eq, 0.0)]).unwrap();
        assert!(close(s.x[0], 1.0) && close(s.x[1], 1.0) && close(s.objective, 2.0), "{s:?}");
        // −x ≤ −3 is x ≥ 3: min x is 3.
        let s = minimize(&[1.0], &[row(&[-1.0], Cmp::Le, -3.0)]).unwrap();
        assert!(close(s.objective, 3.0), "{s:?}");
    }

    #[test]
    fn infeasible_unbounded_and_malformed() {
        assert_eq!(minimize(&[1.0], &[row(&[1.0], Cmp::Le, 1.0), row(&[1.0], Cmp::Ge, 2.0)]), Err(LpError::Infeasible));
        assert_eq!(minimize(&[-1.0], &[row(&[1.0], Cmp::Ge, 1.0)]), Err(LpError::Unbounded));
        assert_eq!(minimize(&[1.0, 2.0], &[row(&[1.0], Cmp::Le, 1.0)]), Err(LpError::Malformed));
        assert_eq!(minimize(&[f64::NAN], &[]), Err(LpError::Malformed));
        // A row of zeros: 0 = 0 is redundant, 0 ≥ 1 infeasible.
        let s = minimize(&[1.0], &[row(&[0.0], Cmp::Eq, 0.0), row(&[1.0], Cmp::Ge, 0.5)]).unwrap();
        assert!(close(s.objective, 0.5), "{s:?}");
        assert_eq!(minimize(&[1.0], &[row(&[0.0], Cmp::Ge, 1.0)]), Err(LpError::Infeasible));
    }

    #[test]
    fn redundant_equalities_are_driven_out() {
        // x + y = 1 twice and 2x + 2y = 2: min x + 2y is 1 at x = 1.
        let rows = [row(&[1.0, 1.0], Cmp::Eq, 1.0), row(&[1.0, 1.0], Cmp::Eq, 1.0), row(&[2.0, 2.0], Cmp::Eq, 2.0)];
        let s = minimize(&[1.0, 2.0], &rows).unwrap();
        assert!(close(s.x[0], 1.0) && close(s.x[1], 0.0) && close(s.objective, 1.0), "{s:?}");
    }

    #[test]
    fn beales_cycling_example_terminates() {
        // Beale (1955), on which the largest-coefficient rule cycles: min
        // −3/4 x1 + 150 x2 − 1/50 x3 + 6 x4 subject to 1/4 x1 − 60 x2 − 1/25
        // x3 + 9 x4 ≤ 0, 1/2 x1 − 90 x2 − 1/50 x3 + 3 x4 ≤ 0, x3 ≤ 1. The
        // optimum is x1 = 1/25, x3 = 1: −3/4 · 1/25 − 1/50 = −1/20.
        let rows = [
            row(&[0.25, -60.0, -0.04, 9.0], Cmp::Le, 0.0),
            row(&[0.5, -90.0, -0.02, 3.0], Cmp::Le, 0.0),
            row(&[0.0, 0.0, 1.0, 0.0], Cmp::Le, 1.0),
        ];
        let s = minimize(&[-0.75, 150.0, -0.02, 6.0], &rows).unwrap();
        assert!(close(s.objective, -0.05) && close(s.x[0], 0.04) && close(s.x[2], 1.0), "{s:?}");
    }

    /// SplitMix64: a small deterministic generator.
    struct Rng(u64);

    impl Rng {
        fn next(&mut self) -> u64 {
            self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
            let mut z = self.0;
            z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
            z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
            z ^ (z >> 31)
        }
        fn int(&mut self, lo: i64, hi: i64) -> f64 {
            (lo + (self.next() % (hi - lo + 1) as u64) as i64) as f64
        }
    }

    /// The solution of the square system `a x = b`, by Gaussian elimination
    /// with partial pivoting; None when singular.
    fn solve(mut a: Vec<Vec<f64>>, mut b: Vec<f64>) -> Option<Vec<f64>> {
        let m = b.len();
        for k in 0..m {
            let p = (k..m).max_by(|&i, &j| a[i][k].abs().total_cmp(&a[j][k].abs()))?;
            if a[p][k].abs() < 1e-10 {
                return None;
            }
            a.swap(k, p);
            b.swap(k, p);
            for i in k + 1..m {
                let f = a[i][k] / a[k][k];
                let (top, rest) = a.split_at_mut(i);
                for (x, y) in rest[0].iter_mut().zip(&top[k]) {
                    *x -= f * y;
                }
                b[i] -= f * b[k];
            }
        }
        let mut x = vec![0.0; m];
        for k in (0..m).rev() {
            let s: f64 = (k + 1..m).map(|j| a[k][j] * x[j]).sum();
            x[k] = (b[k] - s) / a[k][k];
        }
        Some(x)
    }

    /// The least objective over the vertices, by enumerating every basis of
    /// the standard form (one slack column per inequality); None when no
    /// basic solution is feasible.
    fn vertices(c: &[f64], rows: &[Constraint]) -> Option<f64> {
        let n = c.len();
        let mut columns: Vec<Vec<f64>> = (0..n).map(|j| rows.iter().map(|r| r.coef[j]).collect()).collect();
        for (i, r) in rows.iter().enumerate() {
            if r.cmp != Cmp::Eq {
                let mut col = vec![0.0; rows.len()];
                col[i] = if r.cmp == Cmp::Le { 1.0 } else { -1.0 };
                columns.push(col);
            }
        }
        let b: Vec<f64> = rows.iter().map(|r| r.rhs).collect();
        let m = rows.len();
        let mut best: Option<f64> = None;
        for mask in 0u32..(1 << columns.len()) {
            if mask.count_ones() as usize != m {
                continue;
            }
            let basis: Vec<usize> = (0..columns.len()).filter(|&j| (mask >> j) & 1 == 1).collect();
            let a: Vec<Vec<f64>> = (0..m).map(|i| basis.iter().map(|&j| columns[j][i]).collect()).collect();
            let Some(x) = solve(a, b.clone()) else { continue };
            if x.iter().all(|&v| v >= -1e-9) {
                let objective: f64 = basis.iter().zip(&x).filter(|(&j, _)| j < n).map(|(&j, &v)| c[j] * v).sum();
                best = Some(best.map_or(objective, |b: f64| b.min(objective)));
            }
        }
        best
    }

    #[test]
    fn random_programs_match_vertex_enumeration() {
        // Up to four variables, each boxed so the minimum is a vertex, up to
        // three random rows of either inequality and at most one equality
        // (so the rows stay independent and every basis is enumerated).
        let mut rng = Rng(7);
        let mut solved = 0;
        for _ in 0..400 {
            let n = rng.int(1, 4) as usize;
            let c: Vec<f64> = (0..n).map(|_| rng.int(-5, 5)).collect();
            let mut rows: Vec<Constraint> = (0..n)
                .map(|j| row(&(0..n).map(|k| if k == j { 1.0 } else { 0.0 }).collect::<Vec<_>>(), Cmp::Le, rng.int(1, 5)))
                .collect();
            let mut equality = false;
            for _ in 0..rng.int(1, 3) as usize {
                let coef: Vec<f64> = (0..n).map(|_| rng.int(-3, 3)).collect();
                let cmp = match rng.int(0, 2) as u8 {
                    0 => Cmp::Le,
                    1 => Cmp::Ge,
                    _ if !equality && coef.iter().any(|&a| a != 0.0) => {
                        equality = true;
                        Cmp::Eq
                    }
                    _ => Cmp::Ge,
                };
                rows.push(row(&coef, cmp, rng.int(-4, 8)));
            }
            match (vertices(&c, &rows), minimize(&c, &rows)) {
                (Some(best), Ok(s)) => {
                    assert!((s.objective - best).abs() <= 1e-7 * (1.0 + best.abs()), "{c:?} {rows:?}: {s:?} against {best}");
                    for r in &rows {
                        let lhs: f64 = r.coef.iter().zip(&s.x).map(|(a, x)| a * x).sum();
                        let ok = match r.cmp {
                            Cmp::Le => lhs <= r.rhs + 1e-7,
                            Cmp::Ge => lhs >= r.rhs - 1e-7,
                            Cmp::Eq => (lhs - r.rhs).abs() <= 1e-7,
                        };
                        assert!(ok && s.x.iter().all(|&x| x >= 0.0), "{rows:?}: {s:?}");
                    }
                    solved += 1;
                }
                (None, Err(LpError::Infeasible)) => {}
                (v, s) => panic!("{c:?} {rows:?}: enumeration {v:?}, simplex {s:?}"),
            }
        }
        assert!(solved > 100, "only {solved} feasible programs");
    }
}
