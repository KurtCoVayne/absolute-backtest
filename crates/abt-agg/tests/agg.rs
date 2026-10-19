//! abt-agg against an exact reference (docs/spec/05-migration.md, M-1): every
//! function and mode on random windows of dyadic values (exact in f64), with
//! the exact answer computed in 128-bit rationals. `two_pass` equals the
//! two-pass formula to the bit and the exact value to rounding; `running`
//! stays within 1e-12 relative of the exact value along a long walk.

use abt_agg::order_stats::{median_of_sorted, quantile_of_sorted, sorted, SortedWindow};
use abt_agg::{reduce, AggError, Function, Mode, RowsWindow, WindowState};

/// An exact rational with an `i128` numerator and a positive denominator.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
struct Q {
    n: i128,
    d: i128,
}

fn gcd(mut a: i128, mut b: i128) -> i128 {
    a = a.abs();
    b = b.abs();
    while b != 0 {
        let t = a % b;
        a = b;
        b = t;
    }
    a.max(1)
}

impl Q {
    fn new(n: i128, d: i128) -> Q {
        assert!(d != 0);
        let s = if d < 0 { -1 } else { 1 };
        let g = gcd(n, d);
        Q { n: s * n / g, d: s * d / g }
    }
    fn int(n: i128) -> Q {
        Q { n, d: 1 }
    }
    /// The exact value of a dyadic f64 with at most 2^-20 resolution.
    fn of(x: f64) -> Q {
        let scaled = x * (1u64 << 20) as f64;
        assert_eq!(scaled.fract(), 0.0, "{} is not a multiple of 2^-20", x);
        Q::new(scaled as i128, 1 << 20)
    }
    fn add(self, o: Q) -> Q {
        Q::new(self.n * o.d + o.n * self.d, self.d * o.d)
    }
    fn sub(self, o: Q) -> Q {
        Q::new(self.n * o.d - o.n * self.d, self.d * o.d)
    }
    fn mul(self, o: Q) -> Q {
        let (g1, g2) = (gcd(self.n, o.d), gcd(o.n, self.d));
        Q::new((self.n / g1) * (o.n / g2), (self.d / g2) * (o.d / g1))
    }
    fn div(self, o: Q) -> Q {
        self.mul(Q::new(o.d, o.n))
    }
    fn f64(self) -> f64 {
        self.n as f64 / self.d as f64
    }
}

fn exact_mean(xs: &[f64]) -> Q {
    xs.iter().fold(Q::int(0), |s, &x| s.add(Q::of(x))).div(Q::int(xs.len() as i128))
}

fn exact_cov(xs: &[f64], ys: &[f64]) -> Q {
    let (mx, my) = (exact_mean(xs), exact_mean(ys));
    let s = xs.iter().zip(ys).fold(Q::int(0), |s, (&x, &y)| s.add(Q::of(x).sub(mx).mul(Q::of(y).sub(my))));
    s.div(Q::int(xs.len() as i128 - 1))
}

/// The exact answer of a function, as f64 (through a square root where the
/// function takes one).
fn exact(f: Function, xs: &[f64], ys: &[f64]) -> f64 {
    match f {
        Function::Sum => xs.iter().fold(Q::int(0), |s, &x| s.add(Q::of(x))).f64(),
        Function::Mean => exact_mean(xs).f64(),
        Function::Var => exact_cov(xs, xs).f64(),
        Function::Std => exact_cov(xs, xs).f64().sqrt(),
        Function::Cov => exact_cov(xs, ys).f64(),
        Function::Corr | Function::OlsR => exact_cov(xs, ys).f64() / (exact_cov(xs, xs).f64() * exact_cov(ys, ys).f64()).sqrt(),
        Function::OlsSlope | Function::OlsBeta => exact_cov(xs, ys).div(exact_cov(ys, ys)).f64(),
        _ => unreachable!(),
    }
}

/// A seeded stream of dyadic values around `level` (a random walk when
/// `walk`), each an integer multiple of 2^-10.
fn stream(seed: u64, level: f64, walk: bool) -> impl FnMut() -> f64 {
    let mut s = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
    let mut x = level;
    move || {
        s ^= s << 13;
        s ^= s >> 7;
        s ^= s << 17;
        let step = ((s >> 11) % 2001) as f64 - 1000.0;
        let v = if walk {
            x += step / 1024.0;
            x
        } else {
            level + step / 1024.0
        };
        (v * 1024.0).round() / 1024.0
    }
}

/// The two-pass formulas written out here, independently of the crate.
fn textbook(f: Function, xs: &[f64], ys: &[f64]) -> f64 {
    let mean = |v: &[f64]| v.iter().sum::<f64>() / v.len() as f64;
    let cov = |a: &[f64], b: &[f64]| {
        let (ma, mb) = (mean(a), mean(b));
        a.iter().zip(b).map(|(x, y)| (x - ma) * (y - mb)).sum::<f64>() / (a.len() as f64 - 1.0)
    };
    match f {
        Function::Sum => xs.iter().sum(),
        Function::Mean => mean(xs),
        Function::Var => cov(xs, xs),
        Function::Std => cov(xs, xs).sqrt(),
        Function::Cov => cov(xs, ys),
        Function::Corr | Function::OlsR => cov(xs, ys) / (cov(xs, xs).sqrt() * cov(ys, ys).sqrt()),
        Function::OlsSlope | Function::OlsBeta => cov(xs, ys) / cov(ys, ys),
        _ => unreachable!(),
    }
}

const MOMENTS: [Function; 9] = [
    Function::Sum,
    Function::Mean,
    Function::Var,
    Function::Std,
    Function::Cov,
    Function::Corr,
    Function::OlsSlope,
    Function::OlsR,
    Function::OlsBeta,
];

fn rel(a: f64, b: f64) -> f64 {
    (a - b).abs() / b.abs().max(1e-300)
}

#[test]
fn two_pass_is_the_two_pass_formula_to_the_bit_and_exact_to_rounding() {
    for seed in 1..40u64 {
        let (mut gx, mut gy) = (stream(seed, 100.0, true), stream(seed + 1000, 50.0, false));
        let n = 2 + (seed as usize * 7) % 300;
        let xs: Vec<f64> = (0..n).map(|_| gx()).collect();
        let ys: Vec<f64> = (0..n).map(|_| gy()).collect();
        for f in MOMENTS {
            let cols: Vec<&[f64]> = if f.arity() == 2 { vec![&xs, &ys] } else { vec![&xs] };
            let got = reduce(f, Mode::TwoPass, &cols).unwrap().unwrap();
            let book = textbook(f, &xs, &ys);
            assert_eq!(got.to_bits(), book.to_bits(), "{:?} on {} rows (seed {}): {} against the formula's {}", f, n, seed, got, book);
            let want = exact(f, &xs, &ys);
            assert!(rel(got, want) < 1e-12, "{:?} on {} rows (seed {}): {} against the exact {}", f, n, seed, got, want);
        }
    }
}

#[test]
fn running_sums_stay_within_1e_12_of_the_exact_value_along_a_long_walk() {
    for (seed, n) in [(3u64, 5usize), (4, 20), (5, 63), (6, 252)] {
        for f in [Function::Mean, Function::Sum, Function::Std, Function::Cov, Function::Corr, Function::OlsSlope, Function::OlsR] {
            let (mut gx, mut gy) = (stream(seed, 1000.0, true), stream(seed * 31, 10.0, true));
            let mut w = RowsWindow::new(f, Mode::Running, n);
            let mut xs: Vec<f64> = Vec::new();
            let mut ys: Vec<f64> = Vec::new();
            for i in 0..2_000u64 {
                let (x, y) = (gx(), gy());
                xs.push(x);
                ys.push(y);
                w.step(i, i as i64 * 60, true, Some(&[x, y]));
                let lo = xs.len().saturating_sub(n);
                let (wx, wy) = (&xs[lo..], &ys[lo..]);
                if wx.len() < 2 {
                    continue;
                }
                let got = w.value(2).unwrap().unwrap();
                let want = exact(f, wx, wy);
                assert!(
                    rel(got, want) < 1e-12 || (got - want).abs() < 1e-9,
                    "{:?} rows({}) at step {}: {} against the exact {}",
                    f,
                    n,
                    i,
                    got,
                    want
                );
            }
        }
    }
}

#[test]
fn a_running_window_does_not_depend_on_which_labels_were_asked() {
    let run = |ask_every: u64| -> Vec<(u64, u64)> {
        let mut g = stream(9, 500.0, true);
        let mut w = RowsWindow::new(Function::Std, Mode::Running, 20);
        let mut out = Vec::new();
        for i in 0..600u64 {
            // Every third label is not anchored and holds no row.
            let anchored = i % 3 != 2;
            let x = g();
            w.step(i, i as i64, anchored, anchored.then_some(&[x][..]));
            if i % ask_every == 0 {
                if let Ok(Some(v)) = w.value(2) {
                    out.push((i, v.to_bits()));
                }
            }
        }
        out
    };
    let every = run(1);
    let sparse = run(7);
    for (i, bits) in sparse {
        let same = every.iter().find(|(j, _)| *j == i).map(|(_, b)| *b);
        assert_eq!(same, Some(bits), "label {}", i);
    }
}

#[test]
fn a_time_window_reduces_the_rows_it_holds() {
    let mut g = stream(11, 20.0, false);
    let mut w = WindowState::new(Function::Mean, Mode::TwoPass);
    let mut held: Vec<(i64, f64)> = Vec::new();
    for t in 0..200i64 {
        let x = g();
        w.push(t, &[x]);
        held.push((t, x));
        w.evict(t - 9);
        held.retain(|(u, _)| *u >= t - 9);
        let xs: Vec<f64> = held.iter().map(|h| h.1).collect();
        assert_eq!(w.value(1).unwrap(), reduce(Function::Mean, Mode::TwoPass, &[&xs]).unwrap());
        assert_eq!(w.len(), xs.len());
    }
    assert_eq!(w.value(100).unwrap(), None, "below the minimum count");
}

#[test]
fn order_statistics_follow_the_sorted_window() {
    let mut g = stream(13, 0.0, false);
    let mut s = SortedWindow::default();
    let mut held: Vec<f64> = Vec::new();
    for i in 0..300 {
        let x = g();
        s.insert(x);
        held.push(x);
        if i % 4 == 3 {
            let out = held.remove(0);
            assert!(s.remove(out));
        }
        let want = sorted(&held);
        assert_eq!(s.as_slice(), want.as_slice());
        assert_eq!(reduce(Function::Median, Mode::TwoPass, &[&held]).unwrap(), Some(median_of_sorted(&want)));
        assert_eq!(reduce(Function::Quantile(0.3), Mode::TwoPass, &[&held]).unwrap(), Some(quantile_of_sorted(&want, 0.3)));
    }
}

#[test]
fn partial_points_are_refused_alike() {
    assert_eq!(reduce(Function::Std, Mode::TwoPass, &[&[1.0]]), Err(AggError::FewerThanTwo(Function::Std)));
    assert_eq!(reduce(Function::Corr, Mode::TwoPass, &[&[1.0, 1.0, 1.0], &[1.0, 2.0, 3.0]]), Err(AggError::Constant(Function::Corr)));
    assert_eq!(reduce(Function::Quantile(1.5), Mode::TwoPass, &[&[1.0, 2.0]]), Err(AggError::LevelOutside(1.5)));
    assert_eq!(reduce(Function::Count, Mode::TwoPass, &[&[]]), Ok(Some(0.0)));
    assert_eq!(reduce(Function::Mean, Mode::TwoPass, &[&[]]), Ok(None));
    assert_eq!(AggError::FewerThanTwo(Function::Std).to_string(), "std of fewer than two observations");
    assert_eq!(AggError::Constant(Function::OlsSlope).to_string(), "ols_slope against a constant regressor");
    // A flat window under running numerics is answered by the exact path.
    let mut w = RowsWindow::new(Function::Std, Mode::Running, 5);
    for i in 0..10u64 {
        w.step(i, i as i64, true, Some(&[7.0]));
    }
    assert_eq!(w.value(2).unwrap(), Some(0.0));
}
