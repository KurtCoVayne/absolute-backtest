//! The linear algebra the tests share (Q-4.5): least squares by Householder
//! QR with column pivoting (Businger and Golub 1965), the symmetric
//! eigenproblem by cyclic Jacobi rotations, and the Cholesky factor. Dense,
//! column-major, double precision; sized for regressions of tens of
//! regressors, not for the store's tables.

/// A dense matrix, column-major: element (i, j) at `data[j * rows + i]`.
#[derive(Clone, Debug, PartialEq)]
pub struct Matrix {
    pub rows: usize,
    pub cols: usize,
    pub data: Vec<f64>,
}

impl Matrix {
    pub fn zeros(rows: usize, cols: usize) -> Matrix {
        Matrix {
            rows,
            cols,
            data: vec![0.0; rows * cols],
        }
    }

    /// From columns of equal length.
    pub fn from_columns(columns: &[Vec<f64>]) -> Matrix {
        let rows = columns.first().map(|c| c.len()).unwrap_or(0);
        let mut data = Vec::with_capacity(rows * columns.len());
        for c in columns {
            assert_eq!(c.len(), rows, "columns of one length");
            data.extend_from_slice(c);
        }
        Matrix { rows, cols: columns.len(), data }
    }

    pub fn identity(n: usize) -> Matrix {
        let mut m = Matrix::zeros(n, n);
        for i in 0..n {
            m[(i, i)] = 1.0;
        }
        m
    }

    pub fn column(&self, j: usize) -> &[f64] {
        &self.data[j * self.rows..(j + 1) * self.rows]
    }

    pub fn transpose(&self) -> Matrix {
        let mut t = Matrix::zeros(self.cols, self.rows);
        for j in 0..self.cols {
            for i in 0..self.rows {
                t[(j, i)] = self[(i, j)];
            }
        }
        t
    }

    pub fn mul(&self, b: &Matrix) -> Matrix {
        assert_eq!(self.cols, b.rows);
        let mut c = Matrix::zeros(self.rows, b.cols);
        for j in 0..b.cols {
            for k in 0..self.cols {
                let bkj = b[(k, j)];
                if bkj == 0.0 {
                    continue;
                }
                for i in 0..self.rows {
                    c.data[j * self.rows + i] += self.data[k * self.rows + i] * bkj;
                }
            }
        }
        c
    }
}

impl std::ops::Index<(usize, usize)> for Matrix {
    type Output = f64;
    fn index(&self, (i, j): (usize, usize)) -> &f64 {
        &self.data[j * self.rows + i]
    }
}

impl std::ops::IndexMut<(usize, usize)> for Matrix {
    fn index_mut(&mut self, (i, j): (usize, usize)) -> &mut f64 {
        &mut self.data[j * self.rows + i]
    }
}

/// A least-squares fit of `y` on the columns of `x`.
#[derive(Clone, Debug, PartialEq)]
pub struct LeastSquares {
    /// The coefficients, in the columns' order.
    pub beta: Vec<f64>,
    /// The fitted residuals y − Xβ.
    pub residuals: Vec<f64>,
    /// The residual sum of squares.
    pub rss: f64,
    /// (X'X)⁻¹, in the columns' order (what a covariance is built from).
    pub xtx_inv: Matrix,
    /// The numerical rank of X.
    pub rank: usize,
}

/// Least squares by Householder QR with column pivoting, on the columns
/// scaled to unit norm (equilibration, which keeps the pivoting meaningful
/// on designs as ill-conditioned as Filip's polynomial), the solution then
/// refined (`refine`). A column whose remaining norm falls below 1e-13 of
/// its own is rank-deficient and refused, since a coefficient of a test is
/// never arbitrary.
pub fn least_squares(x: &Matrix, y: &[f64]) -> Result<LeastSquares, String> {
    let (n, p) = (x.rows, x.cols);
    if y.len() != n {
        return Err(format!("{} observations against {} rows", y.len(), n));
    }
    if n < p {
        return Err(format!("{} observations for {} coefficients", n, p));
    }
    let scale: Vec<f64> = (0..p).map(|j| x.column(j).iter().map(|v| v * v).sum::<f64>().sqrt()).collect();
    if let Some(j) = scale.iter().position(|s| *s == 0.0) {
        return Err(format!("column {} of the design is zero", j));
    }
    let mut a = x.clone();
    for j in 0..p {
        for i in 0..n {
            a[(i, j)] /= scale[j];
        }
    }
    let mut qty = y.to_vec();
    let mut perm: Vec<usize> = (0..p).collect();
    let mut norms = vec![1.0; p];
    let mut reflections: Vec<(Vec<f64>, f64)> = Vec::with_capacity(p);
    for k in 0..p {
        // The pivot: the column of largest remaining norm, recomputed
        // exactly (no downdating, which loses digits on ill-conditioned
        // designs such as Filip's).
        for (j, nj) in norms.iter_mut().enumerate().skip(k) {
            *nj = (k..n).map(|i| a[(i, j)] * a[(i, j)]).sum::<f64>().sqrt();
        }
        let (piv, &best) = norms.iter().enumerate().skip(k).fold((k, &f64::NEG_INFINITY), |acc, (j, v)| if *v > *acc.1 { (j, v) } else { acc });
        if best <= 1e-13 {
            return Err(format!("the design is rank-deficient (rank {} of {})", k, p));
        }
        if piv != k {
            for i in 0..n {
                a.data.swap(k * n + i, piv * n + i);
            }
            perm.swap(k, piv);
            norms.swap(k, piv);
        }
        // The Householder reflection taking a[k.., k] to (alpha, 0, ...).
        let alpha = {
            let s = (k..n).map(|i| a[(i, k)] * a[(i, k)]).sum::<f64>().sqrt();
            if a[(k, k)] > 0.0 {
                -s
            } else {
                s
            }
        };
        let mut v: Vec<f64> = (k..n).map(|i| a[(i, k)]).collect();
        v[0] -= alpha;
        let vnorm2: f64 = v.iter().map(|x| x * x).sum();
        for j in (k + 1)..p {
            reflect(&v, vnorm2, k, &mut a.data[j * n..(j + 1) * n]);
        }
        reflect(&v, vnorm2, k, &mut qty);
        a[(k, k)] = alpha;
        for i in (k + 1)..n {
            a[(i, k)] = 0.0;
        }
        reflections.push((v, vnorm2));
    }
    // R b = (Q'y)[..p], by back-substitution; then undo the pivoting.
    let b = solve_upper(&a, &qty[..p]);
    let mut beta = vec![0.0; p];
    for (k, &j) in perm.iter().enumerate() {
        beta[j] = b[k] / scale[j];
    }
    let residuals = refine(x, y, &a, &reflections, &perm, &scale, &mut beta);
    // (X'X)⁻¹ = P R⁻¹ R⁻ᵀ Pᵀ.
    let mut rinv = Matrix::zeros(p, p);
    for c in 0..p {
        for k in (0..=c).rev() {
            let mut s = if k == c { 1.0 } else { 0.0 };
            for j in (k + 1)..=c {
                s -= a[(k, j)] * rinv[(j, c)];
            }
            rinv[(k, c)] = s / a[(k, k)];
        }
    }
    let inner = rinv.mul(&rinv.transpose());
    let mut xtx_inv = Matrix::zeros(p, p);
    for (r, &i) in perm.iter().enumerate() {
        for (c, &j) in perm.iter().enumerate() {
            xtx_inv[(i, j)] = inner[(r, c)] / (scale[i] * scale[j]);
        }
    }
    let rss: f64 = residuals.iter().map(|v| v * v).sum();
    Ok(LeastSquares {
        beta,
        residuals,
        rss,
        xtx_inv,
        rank: p,
    })
}

/// The steps of iterative refinement `least_squares` takes: one brings
/// every NIST StRD linear dataset to the rounding of its data, the second
/// covers designs nearer the limit of double precision.
const REFINEMENTS: usize = 2;

/// Björck's iterative refinement of the least-squares solution through the
/// augmented system r + Xβ = y, X'r = 0 (Björck 1967; Björck and Golub
/// 1967), each step's residuals of that system computed in twice the
/// working precision (`dot2`) and its correction solved with the QR factors
/// already in hand. A backward-stable QR solution is accurate only to
/// about κ²ε when the residuals are large (Wampler5 keeps under six
/// digits); the refined one to the rounding of its data. Returns the
/// residuals y − Xβ of the refined coefficients.
fn refine(x: &Matrix, y: &[f64], r_factor: &Matrix, reflections: &[(Vec<f64>, f64)], perm: &[usize], scale: &[f64], beta: &mut [f64]) -> Vec<f64> {
    let (n, p) = (x.rows, x.cols);
    let residual = |beta: &[f64], i: usize, r: f64| dot2([(y[i], 1.0), (r, -1.0)].into_iter().chain((0..p).map(|j| (x[(i, j)], -beta[j]))));
    let mut r: Vec<f64> = (0..n).map(|i| residual(beta, i, 0.0)).collect();
    for _ in 0..REFINEMENTS {
        // The augmented system's residuals: f = y − r − Xβ, and g = −X'r in
        // the scaled, pivoted coordinates of the factors.
        let mut d: Vec<f64> = (0..n).map(|i| residual(beta, i, r[i])).collect();
        let g: Vec<f64> = perm.iter().map(|&j| -dot2(x.column(j).iter().copied().zip(r.iter().copied())) / scale[j]).collect();
        // R'h = g; d = Q'f; R u = d₁ − h; δr = Q (h, d₂).
        let h = solve_upper_transposed(r_factor, &g);
        for (k, (v, norm2)) in reflections.iter().enumerate() {
            reflect(v, *norm2, k, &mut d);
        }
        let top: Vec<f64> = d.iter().zip(&h).map(|(dk, hk)| dk - hk).collect();
        let u = solve_upper(r_factor, &top);
        d[..p].copy_from_slice(&h);
        for (k, (v, norm2)) in reflections.iter().enumerate().rev() {
            reflect(v, *norm2, k, &mut d);
        }
        r.iter_mut().zip(&d).for_each(|(ri, di)| *ri += di);
        for (k, &j) in perm.iter().enumerate() {
            beta[j] += u[k] / scale[j];
        }
    }
    (0..n).map(|i| residual(beta, i, 0.0)).collect()
}

/// y[k..] ← H y[k..] for the Householder reflection H = I − 2vv'/(v'v).
fn reflect(v: &[f64], norm2: f64, k: usize, y: &mut [f64]) {
    if norm2 > 0.0 {
        let dot: f64 = v.iter().zip(&y[k..]).map(|(vt, yt)| vt * yt).sum();
        let f = 2.0 * dot / norm2;
        for (yt, vt) in y[k..].iter_mut().zip(v) {
            *yt -= f * vt;
        }
    }
}

/// The solution of R u = b for the upper-triangular R in the leading rows
/// of `r`, by back-substitution.
fn solve_upper(r: &Matrix, b: &[f64]) -> Vec<f64> {
    let p = b.len();
    let mut u = vec![0.0; p];
    for k in (0..p).rev() {
        let mut s = b[k];
        for j in (k + 1)..p {
            s -= r[(k, j)] * u[j];
        }
        u[k] = s / r[(k, k)];
    }
    u
}

/// The solution of R'h = g for the same R, by forward substitution.
fn solve_upper_transposed(r: &Matrix, g: &[f64]) -> Vec<f64> {
    let p = g.len();
    let mut h = vec![0.0; p];
    for k in 0..p {
        let mut s = g[k];
        for j in 0..k {
            s -= r[(j, k)] * h[j];
        }
        h[k] = s / r[(k, k)];
    }
    h
}

/// A sum of products in twice the working precision, rounded once (Ogita,
/// Rump and Oishi's Dot2, 2005): each product split exactly by a fused
/// multiply-add, each addition by Knuth's TwoSum.
fn dot2(terms: impl Iterator<Item = (f64, f64)>) -> f64 {
    let (mut s, mut c) = (0.0f64, 0.0f64);
    for (a, b) in terms {
        let prod = a * b;
        let prod_err = a.mul_add(b, -prod);
        let t = s + prod;
        let z = t - s;
        c += (s - (t - z)) + (prod - z) + prod_err;
        s = t;
    }
    s + c
}

/// The eigenvalues and eigenvectors of a symmetric matrix by cyclic Jacobi
/// rotations, eigenvalues in descending order with their vectors as
/// columns.
pub fn symmetric_eigen(m: &Matrix) -> (Vec<f64>, Matrix) {
    let n = m.rows;
    assert_eq!(n, m.cols, "a square matrix");
    let mut a = m.clone();
    let mut v = Matrix::identity(n);
    for _sweep in 0..100 {
        let off: f64 = (0..n).flat_map(|i| (0..n).filter(move |&j| j != i).map(move |j| (i, j))).map(|(i, j)| a[(i, j)] * a[(i, j)]).sum();
        if off < 1e-30 {
            break;
        }
        for p in 0..n {
            for q in (p + 1)..n {
                let apq = a[(p, q)];
                if apq.abs() < 1e-300 {
                    continue;
                }
                let theta = (a[(q, q)] - a[(p, p)]) / (2.0 * apq);
                let t = theta.signum() / (theta.abs() + (theta * theta + 1.0).sqrt());
                let t = if theta == 0.0 { 1.0 } else { t };
                let c = 1.0 / (t * t + 1.0).sqrt();
                let s = t * c;
                for k in 0..n {
                    let (akp, akq) = (a[(k, p)], a[(k, q)]);
                    a[(k, p)] = c * akp - s * akq;
                    a[(k, q)] = s * akp + c * akq;
                }
                for k in 0..n {
                    let (apk, aqk) = (a[(p, k)], a[(q, k)]);
                    a[(p, k)] = c * apk - s * aqk;
                    a[(q, k)] = s * apk + c * aqk;
                }
                for k in 0..n {
                    let (vkp, vkq) = (v[(k, p)], v[(k, q)]);
                    v[(k, p)] = c * vkp - s * vkq;
                    v[(k, q)] = s * vkp + c * vkq;
                }
            }
        }
    }
    let mut order: Vec<usize> = (0..n).collect();
    order.sort_by(|&i, &j| a[(j, j)].partial_cmp(&a[(i, i)]).unwrap_or(std::cmp::Ordering::Equal));
    let values: Vec<f64> = order.iter().map(|&i| a[(i, i)]).collect();
    let mut vectors = Matrix::zeros(n, n);
    for (c, &i) in order.iter().enumerate() {
        for r in 0..n {
            vectors[(r, c)] = v[(r, i)];
        }
    }
    (values, vectors)
}

/// The lower Cholesky factor L of a symmetric positive-definite matrix
/// (L Lᵀ = m), or why it is not one.
pub fn cholesky(m: &Matrix) -> Result<Matrix, String> {
    let n = m.rows;
    let mut l = Matrix::zeros(n, n);
    for j in 0..n {
        let mut d = m[(j, j)];
        for k in 0..j {
            d -= l[(j, k)] * l[(j, k)];
        }
        if d <= 0.0 {
            return Err(format!("the matrix is not positive definite (pivot {} is {})", j, d));
        }
        l[(j, j)] = d.sqrt();
        for i in (j + 1)..n {
            let mut s = m[(i, j)];
            for k in 0..j {
                s -= l[(i, k)] * l[(j, k)];
            }
            l[(i, j)] = s / l[(j, j)];
        }
    }
    Ok(l)
}
