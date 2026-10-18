//! OHLC prediction (ST-4.5): a bar relation stores the close `C` and the
//! residuals `H′ = H − max(O, C)`, `L′ = min(O, C) − L` and `O′ = O −
//! C_prev`, computed on integers at the finest exponent of the four columns,
//! so they are exact; the first row of an instrument stores `O′ = 0` with
//! its open kept in the directory. `H′ ≥ 0` and `L′ ≥ 0` hold on vendor
//! data; a row that violates them is a vendor error, kept raw (its residual
//! 0 and its value in the violations list, which staging reports).

/// The predicted columns of a bar relation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Column {
    Open,
    High,
    Low,
}

/// A row whose high or low contradicts its open and close: kept raw.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Violation {
    pub row: usize,
    pub column: Column,
    /// The vendor's value (not a residual).
    pub raw: i64,
}

/// The residual columns of a run of rows, with the violations found.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Residuals {
    pub close: Vec<i64>,
    pub high: Vec<i64>,
    pub low: Vec<i64>,
    pub open: Vec<i64>,
    /// In row order; the residual at a violating row is 0 and the raw value
    /// is here.
    pub violations: Vec<Violation>,
}

/// Predicts the rows of one instrument from their close: `prev_close` is
/// the close of the row before the first (`None` for the instrument's
/// first row, whose open the directory keeps).
pub fn predict(open: &[i64], high: &[i64], low: &[i64], close: &[i64], prev_close: Option<i64>) -> Residuals {
    let n = close.len();
    assert!(open.len() == n && high.len() == n && low.len() == n);
    let mut out = Residuals {
        close: close.to_vec(),
        high: Vec::with_capacity(n),
        low: Vec::with_capacity(n),
        open: Vec::with_capacity(n),
        violations: Vec::new(),
    };
    let mut prev = prev_close;
    for i in 0..n {
        let (o, h, l, c) = (open[i], high[i], low[i], close[i]);
        let hp = h - o.max(c);
        let lp = o.min(c) - l;
        if hp < 0 {
            out.violations.push(Violation { row: i, column: Column::High, raw: h });
            out.high.push(0);
        } else {
            out.high.push(hp);
        }
        if lp < 0 {
            out.violations.push(Violation { row: i, column: Column::Low, raw: l });
            out.low.push(0);
        } else {
            out.low.push(lp);
        }
        out.open.push(match prev {
            Some(p) => o - p,
            None => 0,
        });
        prev = Some(c);
    }
    out
}

/// The vendor's columns back from the residuals: `first_open` is the
/// directory's open of the instrument's first row when `prev_close` is
/// `None`. Row-local but for the previous close, so a range needs the
/// close at its first row minus one only (Proposition 6.1).
pub fn reconstruct(r: &Residuals, prev_close: Option<i64>, first_open: Option<i64>) -> (Vec<i64>, Vec<i64>, Vec<i64>) {
    let n = r.close.len();
    let (mut open, mut high, mut low) = (Vec::with_capacity(n), Vec::with_capacity(n), Vec::with_capacity(n));
    let mut prev = prev_close;
    let mut v = r.violations.iter().peekable();
    for i in 0..n {
        let c = r.close[i];
        let o = match prev {
            Some(p) => r.open[i] + p,
            None => first_open.expect("the first row's open is in the directory"),
        };
        let mut h = r.high[i] + o.max(c);
        let mut l = o.min(c) - r.low[i];
        while let Some(x) = v.peek() {
            if x.row != i {
                break;
            }
            match x.column {
                Column::High => h = x.raw,
                Column::Low => l = x.raw,
                Column::Open => {}
            }
            v.next();
        }
        open.push(o);
        high.push(h);
        low.push(l);
        prev = Some(c);
    }
    (open, high, low)
}
