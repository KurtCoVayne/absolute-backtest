//! Wavelet matrices (ST-5.7): access, k-th smallest (so the median),
//! count below a threshold, count in `[lo, hi)` and the listing of the
//! values in `[lo, hi)` with their rows, over random row ranges of random
//! columns (few and many distinct values, negatives, one value only),
//! against the sorted slice; the empty range answers nothing.

use abt_codec::wavelet::WaveletMatrix;

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

fn column(rng: &mut Rng, n: usize, distinct: u64, offset: i64) -> Vec<i64> {
    (0..n).map(|_| rng.below(distinct) as i64 + offset).collect()
}

fn check(col: &[i64], rng: &mut Rng) {
    let m = WaveletMatrix::build(col);
    assert_eq!(m.len(), col.len());
    for (i, &v) in col.iter().enumerate() {
        assert_eq!(m.access(i), v, "access({})", i);
    }
    let ranges: Vec<(usize, usize)> = {
        let mut r = vec![(0, col.len()), (0, 0)];
        if !col.is_empty() {
            r.push((col.len() - 1, col.len()));
            r.push((0, 1));
        }
        for _ in 0..40 {
            if col.is_empty() {
                break;
            }
            let a = rng.below(col.len() as u64) as usize;
            let b = a + rng.below((col.len() - a) as u64 + 1) as usize;
            r.push((a, b));
        }
        r
    };
    for (a, b) in ranges {
        let mut s: Vec<i64> = col[a..b].to_vec();
        s.sort();
        // k-th and the median.
        for (k, &v) in s.iter().enumerate() {
            assert_eq!(m.kth(a, b, k as u64), Some(v), "kth({}, {}, {})", a, b, k);
        }
        assert_eq!(m.kth(a, b, s.len() as u64), None);
        assert_eq!(m.median_low(a, b), if s.is_empty() { None } else { Some(s[(s.len() - 1) / 2]) });
        // Counts at thresholds around every value.
        let mut thresholds: Vec<i64> = s.iter().flat_map(|&v| [v - 1, v, v + 1]).collect();
        thresholds.extend([i64::MIN, i64::MAX, 0]);
        for &c in &thresholds {
            let expect = s.iter().filter(|&&v| v < c).count() as u64;
            assert_eq!(m.count_below(a, b, c), expect, "count_below({}, {}, {})", a, b, c);
            assert_eq!(m.count_where(a, b, &|x| x < c), expect, "count_where({}, {}, {})", a, b, c);
        }
        for w in thresholds.windows(2) {
            let (lo, hi) = (w[0].min(w[1]), w[0].max(w[1]));
            let expect = s.iter().filter(|&&v| v >= lo && v < hi).count() as u64;
            assert_eq!(m.count_in(a, b, lo, hi), expect, "count_in({}, {}, {}, {})", a, b, lo, hi);
            // The listing: ascending values, each with its rows ascending.
            let listed = m.list(a, b, lo, hi);
            let mut expect: Vec<(i64, Vec<usize>)> = Vec::new();
            for (i, &v) in col[a..b].iter().enumerate() {
                if v >= lo && v < hi {
                    match expect.binary_search_by_key(&v, |e| e.0) {
                        Ok(j) => expect[j].1.push(a + i),
                        Err(j) => expect.insert(j, (v, vec![a + i])),
                    }
                }
            }
            assert_eq!(listed, expect, "list({}, {}, {}, {})", a, b, lo, hi);
        }
    }
}

#[test]
fn order_statistics_and_counts_equal_the_sorted_slice() {
    let mut rng = Rng(3);
    let cases: Vec<Vec<i64>> = vec![
        vec![],
        vec![42],
        vec![5, 5, 5, 5],
        column(&mut rng, 100, 3, -1),
        column(&mut rng, 3000, 1 << 14, 1_000_000),
        column(&mut rng, 2500, 1_000_000, -500_000),
        column(&mut rng, 1024, 2, 0),
        column(&mut rng, 777, 1 << 20, i64::MAX - (1 << 20)),
    ];
    for col in cases {
        check(&col, &mut rng);
    }
}

#[test]
fn the_bits_are_about_log_sigma_and_a_quarter_per_value() {
    let mut rng = Rng(5);
    let col = column(&mut rng, 1 << 16, 1 << 12, 0);
    let m = WaveletMatrix::build(&col);
    assert!(m.distinct() <= 1 << 12);
    // 12 levels of 2^16 bits with a 25 % directory each.
    let per_value = m.bits() as f64 / col.len() as f64;
    assert!((14.0..16.0).contains(&per_value), "{} bits a value", per_value);
}
