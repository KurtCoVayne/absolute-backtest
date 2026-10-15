//! The reduction of whole columns (P-1.2): a group's rows in canonical order
//! to one value. `TwoPass` is the textbook two-pass formula in order, the
//! form the books' references are reproduced with (docs/spec/ERRATA.md,
//! E-28); `Running` is the value the running sums give right after a
//! rebuild over the rows, falling back to `TwoPass` where they refuse.

use crate::function::{AggError, Function, Mode};
use crate::moments::{mean, sample_cov, ShiftedSums};
use crate::order_stats::{quantile_of_sorted, sorted};

/// The function over the columns (`cols[0]` its argument, `cols[1]` the
/// second argument of a bivariate one). `Ok(None)` is "no value": an empty
/// group for anything but `count`, which is 0.
pub fn reduce(f: Function, mode: Mode, cols: &[&[f64]]) -> Result<Option<f64>, AggError> {
    let xs: &[f64] = cols.first().copied().unwrap_or(&[]);
    if f == Function::Count {
        return Ok(Some(xs.len() as f64));
    }
    if xs.is_empty() {
        return Ok(None);
    }
    if mode == Mode::Running && f.running() {
        let rows: Vec<[f64; 2]> = (0..xs.len()).map(|i| [xs[i], cols.get(1).map(|c| c[i]).unwrap_or(0.0)]).collect();
        let sums = ShiftedSums::of_rows(rows.iter().map(|r| r.as_slice()));
        if let Some(v) = sums.value(f, xs.len()) {
            return Ok(Some(v));
        }
    }
    two_pass(f, cols)
}

fn two_pass(f: Function, cols: &[&[f64]]) -> Result<Option<f64>, AggError> {
    let xs = cols[0];
    let second = || cols.get(1).copied().ok_or(AggError::FewerThanTwo(f));
    let v = match f {
        Function::Count => xs.len() as f64,
        Function::Sum => xs.iter().sum(),
        Function::Mean => mean(xs),
        Function::Std => {
            if xs.len() < 2 {
                return Err(AggError::FewerThanTwo(Function::Std));
            }
            sample_cov(f, xs, xs)?.sqrt()
        }
        Function::Var => sample_cov(f, xs, xs)?,
        Function::Median => {
            let s = sorted(xs);
            let n = s.len();
            if n % 2 == 1 {
                s[n / 2]
            } else {
                (s[n / 2 - 1] + s[n / 2]) / 2.0
            }
        }
        Function::Quantile(q) => {
            if !(0.0..=1.0).contains(&q) {
                return Err(AggError::LevelOutside(q));
            }
            quantile_of_sorted(&sorted(xs), q)
        }
        Function::Max => xs.iter().cloned().fold(f64::NEG_INFINITY, f64::max),
        Function::Min => xs.iter().cloned().fold(f64::INFINITY, f64::min),
        Function::First => xs[0],
        Function::Last => xs[xs.len() - 1],
        Function::Corr | Function::OlsR => {
            let ys = second()?;
            let c = sample_cov(f, xs, ys)?;
            let (sx, sy) = (sample_cov(f, xs, xs)?.sqrt(), sample_cov(f, ys, ys)?.sqrt());
            if sx == 0.0 || sy == 0.0 {
                return Err(AggError::Constant(f));
            }
            c / (sx * sy)
        }
        Function::Cov => sample_cov(f, xs, second()?)?,
        Function::OlsSlope | Function::OlsBeta => {
            // The slope of the first column on the second.
            let x = second()?;
            let vx = sample_cov(f, x, x)?;
            if vx == 0.0 {
                return Err(AggError::Constant(f));
            }
            sample_cov(f, xs, x)? / vx
        }
    };
    Ok(Some(v))
}
