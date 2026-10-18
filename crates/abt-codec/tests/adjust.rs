//! Adjusted views over ranges (ST-5.10; ST-13's "adjusted ranges" row):
//! split, total-return and continuous-futures views over random ranges
//! crossing events, evaluated piecewise from the event-interval summaries
//! with only the two ends decoded, against a direct reduction of the
//! adjusted decoded rows: sums exact for splits (rational, checked by
//! cross-multiplication against an `i128` rational reference) and for
//! additive adjustments, equal to the ordered definition for total return;
//! extremes with their rows; counts below thresholds by the view's own
//! comparison.

use abt_agg::exact::{Int, Wide};
use abt_codec::adjust::{self, Factor, IntervalSummaries, Piece, Steps, Sum, Threshold};
use abt_codec::summary::{Column, Decoded};
use abt_codec::BLOCK;

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E3779B97F4A7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58476D1CE4E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D049BB133111EB);
        z ^ (z >> 31)
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

const SCALE: u32 = 2;

/// Prices in cents over three blocks and a partial fourth.
fn prices(rng: &mut Rng) -> Decoded {
    let n = 3 * BLOCK + 517;
    let mut p = 10_000i64;
    let values: Vec<i64> = (0..n)
        .map(|_| {
            p += rng.below(201) as i64 - 100;
            p = p.max(100);
            p
        })
        .collect();
    Decoded::of(values.chunks(BLOCK).map(|c| c.to_vec()).collect())
}

fn rows(col: &Decoded) -> Vec<i64> {
    col.blocks.concat()
}

/// Event rows crossing block boundaries, at row 0 and in the last block.
fn event_rows() -> Vec<u64> {
    vec![0, 700, BLOCK as u64, 2 * BLOCK as u64 - 1, 2 * BLOCK as u64 + 1, 3 * BLOCK as u64 + 100]
}

fn ranges(rng: &mut Rng, n: u64) -> Vec<(u64, u64)> {
    let mut r = vec![(0, n), (0, 1), (n - 1, n), (699, 701), (BLOCK as u64 - 1, BLOCK as u64 + 1), (0, 0)];
    for _ in 0..60 {
        let a = rng.below(n);
        let b = a + rng.below(n - a + 1);
        r.push((a, b));
    }
    r
}

fn pieces_of(col: &Decoded, steps: &Steps, a: u64, b: u64) -> Vec<Piece> {
    let summaries = IntervalSummaries::of(col, col.rows(), steps);
    adjust::pieces(col, steps, &summaries, a, b)
}

/// A split view: the factors `1/1, 1/2, 1/2 · 1/3 …` (cumulative splits)
/// and a reverse split.
fn split_steps() -> Steps {
    let factors = [(1u64, 1u64), (1, 2), (1, 6), (5, 6), (5, 12), (10, 12)];
    Steps::new(
        Factor::Ratio { num: 1, den: 1 },
        event_rows().into_iter().zip(factors).skip(1).map(|(r, (num, den))| (r, Factor::Ratio { num, den })),
    )
}

#[test]
fn split_sums_are_exact_rationals() {
    let mut rng = Rng(21);
    let col = prices(&mut rng);
    let values = rows(&col);
    let steps = split_steps();
    for (a, b) in ranges(&mut rng, col.rows()) {
        let pieces = pieces_of(&col, &steps, a, b);
        let Sum::Exact(sum) = adjust::sum(&pieces, SCALE) else { panic!("rational") };
        // The reference: Σ x · num / den over the rows, over the product
        // of the denominators (2 · 6 · 12 = 144 covers every factor).
        let den_ref: i128 = 144;
        let mut num_ref: i128 = 0;
        for k in a..b {
            let Factor::Ratio { num, den } = steps.at(k) else { panic!() };
            num_ref += values[k as usize] as i128 * num as i128 * (den_ref / den as i128);
        }
        // num / den == num_ref / den_ref  ⇔  num · den_ref == num_ref · den.
        let lhs = sum.num.mul_u128(den_ref as u128);
        let rhs = Int::from_i128(num_ref).mul_u128(sum.den.low_u128());
        assert_eq!(lhs, rhs, "[{}, {})", a, b);
        assert_eq!(adjust::count(&pieces), b - a);
        // And the f64 is the correctly rounded quotient of the reference.
        let expect = abt_agg::exact::ratio_to_f64(&Int::from_i128(num_ref), &Wide::from_u128(den_ref as u128 * 100));
        assert_eq!(sum.to_f64(SCALE).to_bits(), expect.to_bits(), "[{}, {})", a, b);
    }
}

#[test]
fn additive_sums_are_exact() {
    let mut rng = Rng(22);
    let col = prices(&mut rng);
    let values = rows(&col);
    let shifts = [0i64, -250, 310, -1_000, 45, 0];
    let steps = Steps::new(Factor::Shift(0), event_rows().into_iter().zip(shifts).skip(1).map(|(r, s)| (r, Factor::Shift(s))));
    for (a, b) in ranges(&mut rng, col.rows()) {
        let pieces = pieces_of(&col, &steps, a, b);
        let Sum::Exact(sum) = adjust::sum(&pieces, SCALE) else { panic!("additive") };
        let expect: i128 = (a..b)
            .map(|k| {
                let Factor::Shift(s) = steps.at(k) else { panic!() };
                values[k as usize] as i128 + s as i128
            })
            .sum();
        assert_eq!(sum.den, Wide::from_u128(1));
        assert_eq!(sum.num, Int::from_i128(expect), "[{}, {})", a, b);
    }
}

#[test]
fn total_return_sums_follow_the_ordered_definition() {
    let mut rng = Rng(23);
    let col = prices(&mut rng);
    let values = rows(&col);
    let factors = [1.0f64, 1.0123, 1.0123 * 1.0098, 1.0123 * 1.0098 * 0.9971, 1.07, 1.0701];
    let steps = Steps::new(Factor::Float(1.0), event_rows().into_iter().zip(factors).skip(1).map(|(r, f)| (r, Factor::Float(f))));
    for (a, b) in ranges(&mut rng, col.rows()) {
        let pieces = pieces_of(&col, &steps, a, b);
        if pieces.is_empty() {
            assert_eq!(adjust::sum(&pieces, SCALE).to_f64(SCALE), 0.0);
            continue;
        }
        let Sum::Float(sum) = adjust::sum(&pieces, SCALE) else { panic!("float") };
        // The definition: Σ_p F_p · f64(S_p) in time order, S_p the piece's
        // exact raw sum.
        let mut expect = 0.0f64;
        for (start, end, p) in steps.pieces(a, b) {
            let Factor::Float(f) = steps.factors()[p] else { panic!() };
            let s: i128 = values[start as usize..end as usize].iter().map(|&x| x as i128).sum();
            expect += f * (s as f64 / 100.0);
        }
        assert_eq!(sum.to_bits(), expect.to_bits(), "[{}, {})", a, b);
    }
}

#[test]
fn extremes_and_counts_equal_a_direct_reduction_of_the_adjusted_rows() {
    let mut rng = Rng(24);
    let col = prices(&mut rng);
    let values = rows(&col);
    let views = [
        split_steps(),
        Steps::new(Factor::Float(1.0), [(700u64, Factor::Float(1.5)), (2 * BLOCK as u64, Factor::Float(0.25))]),
        Steps::new(Factor::Shift(0), [(BLOCK as u64, Factor::Shift(-5_000)), (3 * BLOCK as u64 + 100, Factor::Shift(7_000))]),
    ];
    for steps in &views {
        for (a, b) in ranges(&mut rng, col.rows()) {
            let pieces = pieces_of(&col, steps, a, b);
            let adjusted: Vec<(f64, u64)> = (a..b).map(|k| (steps.at(k).apply_f64(values[k as usize], SCALE), k)).collect();
            let got = adjust::extremes(&pieces, SCALE);
            let expect = adjusted.iter().fold(None, |best: Option<(f64, u64, f64, u64)>, &(v, k)| {
                Some(match best {
                    None => (v, k, v, k),
                    Some((mn, rmn, mx, rmx)) => (if v < mn { v } else { mn }, if v < mn { k } else { rmn }, if v > mx { v } else { mx }, if v > mx { k } else { rmx }),
                })
            });
            assert_eq!(got, expect, "[{}, {})", a, b);
            // Counts below thresholds, each piece counted by a scan of its
            // raw rows with the converted comparison.
            let thresholds: Vec<Threshold> = match steps.factors()[0] {
                Factor::Float(_) => vec![Threshold::Float(90.0), Threshold::Float(100.25), Threshold::Float(1.0e9), Threshold::Float(-1.0)],
                _ => vec![
                    Threshold::Ticks { num: 9_000, den: 1 },
                    Threshold::Ticks { num: 10_025, den: 1 },
                    Threshold::Ticks { num: 10_025, den: 3 },
                    Threshold::Ticks { num: 1, den: 1 },
                    Threshold::Ticks { num: 2_000_000, den: 1 },
                ],
            };
            for c in thresholds {
                let mut count_raw = |p: &Piece, below: &dyn Fn(i64) -> bool| values[p.start as usize..p.end as usize].iter().filter(|&&x| below(x)).count() as u64;
                let got = adjust::count_below(&pieces, c, SCALE, &mut count_raw);
                let expect = (a..b)
                    .filter(|&k| {
                        let x = values[k as usize];
                        match (steps.at(k), c) {
                            (Factor::Ratio { num, den }, Threshold::Ticks { num: cn, den: cd }) => (x as i128 * num as i128 * cd as i128) < cn * (den as i128),
                            (Factor::Shift(s), Threshold::Ticks { num: cn, den: cd }) => (x as i128 + s as i128) * (cd as i128) < cn,
                            (f, Threshold::Float(c)) => f.apply_f64(x, SCALE) < c,
                            _ => unreachable!(),
                        }
                    })
                    .count() as u64;
                assert_eq!(got, expect, "[{}, {}) below {:?}", a, b, c);
            }
        }
    }
}

#[test]
fn whole_pieces_come_from_the_summaries_and_only_the_ends_decode() {
    // A column that counts the blocks it decodes.
    use std::cell::Cell;
    struct Counting<'a>(&'a Decoded, Cell<usize>);
    impl Column for Counting<'_> {
        fn block(&self, b: usize) -> Vec<i64> {
            self.1.set(self.1.get() + 1);
            self.0.block(b)
        }
        fn len(&self, b: usize) -> usize {
            self.0.len(b)
        }
        fn blocks(&self) -> usize {
            self.0.blocks()
        }
        fn cumulative(&self, boundary: usize) -> &abt_codec::summary::Cumulative {
            self.0.cumulative(boundary)
        }
        fn summary(&self, b: usize) -> &abt_codec::summary::BlockSummary {
            self.0.summary(b)
        }
        fn sparse(&self) -> &abt_codec::summary::SparseTable {
            self.0.sparse()
        }
    }
    let mut rng = Rng(25);
    let col = prices(&mut rng);
    let steps = split_steps();
    let summaries = IntervalSummaries::of(&col, col.rows(), &steps);
    let counting = Counting(&col, Cell::new(0));
    // [10, 3 · BLOCK + 200): the first piece [10, 700) and the last
    // [3 · BLOCK + 100, 3 · BLOCK + 200) decode their edges; the four
    // whole pieces between decode nothing.
    let pieces = adjust::pieces(&counting, &steps, &summaries, 10, 3 * BLOCK as u64 + 200);
    assert_eq!(pieces.len(), 6);
    assert_eq!(
        pieces.iter().map(|p| (p.start, p.end)).collect::<Vec<_>>(),
        [(10, 700), (700, 1024), (1024, 2047), (2047, 2049), (2049, 3172), (3172, 3272)]
    );
    // Block 0 for the first piece (moments and extremes), block 3 for the
    // last: at most a few decodes of two blocks, never the middle ones.
    assert!(counting.1.get() <= 8, "{} decodes", counting.1.get());
    let one = Steps::new(Factor::Ratio { num: 1, den: 1 }, []);
    let direct = adjust::pieces(&col, &one, &IntervalSummaries::of(&col, col.rows(), &one), 10, 3 * BLOCK as u64 + 200);
    assert_eq!(direct.len(), 1);
    assert_eq!(pieces.iter().map(|p| p.raw.count).sum::<u64>(), direct[0].raw.count);
    assert_eq!(pieces.iter().map(|p| p.raw.sum).sum::<i128>(), direct[0].raw.sum);
}
