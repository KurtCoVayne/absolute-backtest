//! ST-13: exact windows from cumulative summaries equal a direct reduction
//! (Proposition 5.1); range extremes from the sparse table and the edges
//! equal a scan (ST-5.2); a bar over any range equals the bar of its rows
//! (ST-5.3); prediction then reconstruction is the identity, violations
//! kept raw (ST-4.5); and every range reads only the blocks it intersects
//! (Proposition 6.1, counted).

use std::cell::Cell;

use abt_agg::exact::Int;
use abt_codec::predict::{predict, reconstruct, Column as Predicted};
use abt_codec::summary::{bar, cumulatives, extremes, moments, point, BlockSummary, Column, CrossMoments, Cumulative, Decoded, SparseTable};
use abt_codec::BLOCK;

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

/// A random instrument: a walk of integer ticks over `rows` rows, cut into
/// blocks of 1,024, the last one partial.
fn instrument(r: &mut Rng, rows: usize) -> Vec<Vec<i64>> {
    let mut x = 100_000i64;
    let values: Vec<i64> = (0..rows)
        .map(|_| {
            x += r.below(201) as i64 - 100;
            x
        })
        .collect();
    values.chunks(BLOCK).map(|c| c.to_vec()).collect()
}

/// A column counting the blocks it decodes.
struct Counting<'a> {
    inner: &'a Decoded,
    decoded: Cell<usize>,
}

impl Column for Counting<'_> {
    fn block(&self, b: usize) -> Vec<i64> {
        self.decoded.set(self.decoded.get() + 1);
        self.inner.block(b)
    }
    fn len(&self, b: usize) -> usize {
        self.inner.len(b)
    }
    fn blocks(&self) -> usize {
        self.inner.blocks()
    }
    fn cumulative(&self, boundary: usize) -> Cumulative {
        self.inner.cumulative(boundary)
    }
    fn summary(&self, b: usize) -> BlockSummary {
        self.inner.summary(b)
    }
    fn sparse(&self) -> &SparseTable {
        self.inner.sparse()
    }
}

fn direct(values: &[i64], a: usize, b: usize) -> Cumulative {
    Cumulative::ZERO.plus(&values[a..b], a as u64)
}

#[test]
fn windows_from_cumulative_summaries_equal_direct_reductions_and_decode_the_edges_only() {
    let mut r = Rng(17);
    for rows in [1usize, 5, 1024, 1025, 3_000, 10_250, 40_000] {
        let blocks = instrument(&mut r, rows);
        let flat: Vec<i64> = blocks.concat();
        let col = Decoded::of(blocks);
        assert_eq!(col.rows(), rows as u64);
        let counting = Counting { inner: &col, decoded: Cell::new(0) };
        for _ in 0..200 {
            let a = r.below(rows as u64) as usize;
            let b = a + 1 + r.below((rows - a) as u64) as usize;
            // Moments.
            let m = moments(&counting, a as u64, b as u64);
            let d = direct(&flat, a, b);
            assert_eq!(m, d, "rows {}..{} of {}", a, b, rows);
            // Extremes with their first rows.
            let (mn, rmn, mx, rmx) = extremes(&counting, a as u64, b as u64).unwrap();
            let smin = *flat[a..b].iter().min().unwrap();
            let smax = *flat[a..b].iter().max().unwrap();
            assert_eq!((mn, mx), (smin, smax));
            assert_eq!(rmn as usize, a + flat[a..b].iter().position(|&x| x == smin).unwrap());
            assert_eq!(rmx as usize, a + flat[a..b].iter().position(|&x| x == smax).unwrap());
            assert_eq!(point(&counting, a as u64), flat[a]);
            // Range closure: at most the two edge blocks are decoded by the
            // moments, at most two by the extremes, one by the point.
            let spanned = b.div_ceil(BLOCK) - a / BLOCK;
            let limit = 2 * spanned.min(2) + 1;
            assert!(counting.decoded.get() <= limit, "{} blocks decoded for rows {}..{} spanning {}", counting.decoded.get(), a, b, spanned);
            counting.decoded.set(0);
        }
        // Aligned ranges need no decoding at all.
        if rows >= 3 * BLOCK {
            let m = moments(&counting, BLOCK as u64, 3 * BLOCK as u64);
            assert_eq!(m, direct(&flat, BLOCK, 3 * BLOCK));
        }
        // Empty ranges.
        assert_eq!(moments(&col, 5, 5), Cumulative::ZERO);
        assert!(extremes(&col, 5, 5).is_none());
    }
}

#[test]
fn the_sparse_table_answers_block_ranges_in_constant_time_and_grows_by_a_block() {
    let mut r = Rng(5);
    let blocks = instrument(&mut r, 70 * BLOCK);
    let summaries: Vec<BlockSummary> = blocks.iter().map(|b| BlockSummary::of(b)).collect();
    let whole = SparseTable::of(&summaries);
    let mut grown = SparseTable::of(&[]);
    for s in &summaries {
        grown.push(s);
    }
    assert_eq!(whole, grown);
    for _ in 0..500 {
        let b0 = r.below(70) as usize;
        let b1 = b0 + 1 + r.below(70 - b0 as u64) as usize;
        let (mn, bmn) = whole.min(b0, b1);
        let (mx, bmx) = whole.max(b0, b1);
        let want_min = summaries[b0..b1].iter().map(|s| s.min).min().unwrap();
        let want_max = summaries[b0..b1].iter().map(|s| s.max).max().unwrap();
        assert_eq!((mn, mx), (want_min, want_max));
        assert_eq!(bmn as usize, b0 + summaries[b0..b1].iter().position(|s| s.min == want_min).unwrap());
        assert_eq!(bmx as usize, b0 + summaries[b0..b1].iter().position(|s| s.max == want_max).unwrap());
    }
    // Cumulatives: entry b is the totals before block b.
    let cum = cumulatives(blocks.iter().map(|b| b.as_slice()));
    let flat: Vec<i64> = blocks.concat();
    for b in [0usize, 1, 7, 70] {
        assert_eq!(cum[b], direct(&flat, 0, b * BLOCK));
    }
    // Cross-moments.
    let (h, l, c, v): (Vec<i64>, Vec<i64>, Vec<i64>, Vec<i64>) =
        (0..500)
            .map(|i| (110 + i % 7, 90 - i % 5, 100 + (i * 3) % 11, 1_000 + i * 17))
            .fold((vec![], vec![], vec![], vec![]), |mut acc, (a, b, c, d)| {
                acc.0.push(a);
                acc.1.push(b);
                acc.2.push(c);
                acc.3.push(d);
                acc
            });
    let x = CrossMoments::ZERO.plus(&h, &l, &c, &v);
    let cv: i128 = c.iter().zip(&v).map(|(&c, &v)| c as i128 * v as i128).sum();
    let hlcv: i128 = (0..500).map(|i| (h[i] + l[i] + c[i]) as i128 * v[i] as i128).sum();
    assert_eq!(x.cv, Int::from_i128(cv));
    assert_eq!(x.hlcv, Int::from_i128(hlcv));
    assert_eq!(x.minus(&x), CrossMoments::ZERO);
}

#[test]
fn a_bar_over_any_range_is_the_bar_of_its_rows_and_prediction_is_the_identity() {
    let mut r = Rng(29);
    let rows = 5 * BLOCK + 700;
    // Bars: a close walk, open near the previous close, high and low around.
    let mut c = 500_000i64;
    let (mut o, mut h, mut l, mut cl, mut v) = (vec![], vec![], vec![], vec![], vec![]);
    for i in 0..rows {
        let open = if i == 0 { c } else { c + r.below(41) as i64 - 20 };
        c += r.below(301) as i64 - 150;
        let high = open.max(c) + r.below(50) as i64;
        let low = open.min(c) - r.below(50) as i64;
        o.push(open);
        h.push(high);
        l.push(low);
        cl.push(c);
        v.push(r.below(10_000) as i64);
    }
    // Two vendor errors: a high below the open, a low above the close.
    h[777] = o[777] - 5;
    l[4_100] = cl[4_100] + 3;
    let res = predict(&o, &h, &l, &cl, None);
    assert_eq!(res.violations.len(), 2);
    assert!(res.violations.iter().any(|x| x.row == 777 && x.column == Predicted::High && x.raw == h[777]));
    assert!(res.violations.iter().any(|x| x.row == 4_100 && x.column == Predicted::Low && x.raw == l[4_100]));
    assert!(res.high.iter().all(|&x| x >= 0) && res.low.iter().all(|&x| x >= 0), "residuals are non-negative");
    assert_eq!(res.open[0], 0);
    let (o2, h2, l2) = reconstruct(&res, None, Some(o[0]));
    assert_eq!((o2, h2, l2), (o.clone(), h.clone(), l.clone()));
    // A later run of rows: the previous close is known, no first open needed.
    let res2 = predict(&o[1000..], &h[1000..], &l[1000..], &cl[1000..], Some(cl[999]));
    let (o3, h3, l3) = reconstruct(&res2, Some(cl[999]), None);
    assert_eq!((&o3[..], &h3[..], &l3[..]), (&o[1000..], &h[1000..], &l[1000..]));

    let col = |x: &Vec<i64>| Decoded::of(x.chunks(BLOCK).map(|c| c.to_vec()).collect());
    let (oc, hc, lc, cc, vc) = (col(&o), col(&h), col(&l), col(&cl), col(&v));
    for _ in 0..300 {
        let a = r.below(rows as u64) as usize;
        let b = a + 1 + r.below((rows - a) as u64) as usize;
        let got = bar(&oc, &hc, &lc, &cc, &vc, a as u64, b as u64).unwrap();
        let want = (
            o[a],
            *h[a..b].iter().max().unwrap(),
            *l[a..b].iter().min().unwrap(),
            cl[b - 1],
            v[a..b].iter().map(|&x| x as i128).sum::<i128>(),
        );
        assert_eq!(got, want, "bar over {}..{}", a, b);
    }
}
