//! Wavelet matrices (ST-5.7; Claude, Navarro and Ordóñez 2015) over the
//! rank-reduced values of a column: the `k`-th smallest of a row range (so
//! its median and quantiles), the number of its values in `[lo, hi)`, and
//! the listing of those values with their rows, each in O(log σ) rank
//! operations, σ the number of distinct values. One bitmap per level of
//! the values' ranks, most significant bit first; at each level the zeros
//! keep their order before the ones.

use crate::rank::Bitmap;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WaveletMatrix {
    /// The distinct values, sorted: rank `r` is `values[r]`.
    pub values: Vec<i64>,
    levels: Vec<Bitmap>,
    zeros: Vec<u64>,
    len: usize,
}

impl WaveletMatrix {
    /// Builds the matrix of a column's values (any order; O(n log σ)).
    pub fn build(column: &[i64]) -> WaveletMatrix {
        let mut values = column.to_vec();
        values.sort_unstable();
        values.dedup();
        let sigma = values.len().max(1);
        let bits = if sigma <= 1 { 1 } else { (usize::BITS - (sigma - 1).leading_zeros()) as usize };
        let mut ranks: Vec<u64> = column.iter().map(|v| values.binary_search(v).unwrap() as u64).collect();
        let mut levels = Vec::with_capacity(bits);
        let mut zeros = Vec::with_capacity(bits);
        for level in 0..bits {
            let shift = bits - 1 - level;
            let bitmap = Bitmap::of(ranks.len(), ranks.iter().enumerate().filter(|(_, &r)| (r >> shift) & 1 == 1).map(|(i, _)| i));
            let mut next = Vec::with_capacity(ranks.len());
            next.extend(ranks.iter().filter(|&&r| (r >> shift) & 1 == 0));
            let z = next.len() as u64;
            next.extend(ranks.iter().filter(|&&r| (r >> shift) & 1 == 1));
            zeros.push(z);
            levels.push(bitmap);
            ranks = next;
        }
        WaveletMatrix {
            values,
            levels,
            zeros,
            len: column.len(),
        }
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub fn distinct(&self) -> usize {
        self.values.len()
    }

    /// The value at row `i`.
    pub fn access(&self, mut i: usize) -> i64 {
        let mut rank = 0u64;
        for (level, b) in self.levels.iter().enumerate() {
            rank <<= 1;
            if b.get(i) {
                rank |= 1;
                i = (self.zeros[level] + b.rank(i)) as usize;
            } else {
                i = b.rank0(i) as usize;
            }
        }
        self.values[rank as usize]
    }

    /// The `k`-th smallest value (`k` from 0) of rows `[a, b)`.
    pub fn kth(&self, mut a: usize, mut b: usize, mut k: u64) -> Option<i64> {
        if a >= b || k >= (b - a) as u64 {
            return None;
        }
        let mut rank = 0u64;
        for (level, bm) in self.levels.iter().enumerate() {
            let (za, zb) = (bm.rank0(a), bm.rank0(b));
            let zeros_in = zb - za;
            rank <<= 1;
            if k < zeros_in {
                a = za as usize;
                b = zb as usize;
            } else {
                k -= zeros_in;
                rank |= 1;
                let z = self.zeros[level];
                a = (z + bm.rank(a)) as usize;
                b = (z + bm.rank(b)) as usize;
            }
        }
        Some(self.values[rank as usize])
    }

    /// The median of rows `[a, b)`: the lower middle value (the exact
    /// middle for an odd count).
    pub fn median_low(&self, a: usize, b: usize) -> Option<i64> {
        self.kth(a, b, ((b - a) as u64).saturating_sub(1) / 2)
    }

    /// The number of rows in `[a, b)` whose value is below `c` (strictly).
    pub fn count_below(&self, a: usize, b: usize, c: i64) -> u64 {
        if a >= b {
            return 0;
        }
        // The rank threshold: values below c are ranks below r.
        let r = self.values.partition_point(|&v| v < c) as u64;
        if r == 0 {
            return 0;
        }
        if r as usize >= self.values.len() {
            return (b - a) as u64;
        }
        self.count_rank_below(a, b, r)
    }

    /// The number of rows in `[a, b)` whose value satisfies `below`, a
    /// predicate true on a prefix of the sorted values (any monotone
    /// threshold: an adjusted view's comparison, ST-5.10).
    pub fn count_where(&self, a: usize, b: usize, below: &dyn Fn(i64) -> bool) -> u64 {
        if a >= b {
            return 0;
        }
        let r = self.values.partition_point(|&v| below(v)) as u64;
        if r == 0 {
            return 0;
        }
        if r as usize >= self.values.len() {
            return (b - a) as u64;
        }
        self.count_rank_below(a, b, r)
    }

    /// The rows in `[a, b)` whose rank is below `r` (`0 < r < σ`).
    fn count_rank_below(&self, mut a: usize, mut b: usize, r: u64) -> u64 {
        let bits = self.levels.len();
        let mut count = 0u64;
        for (level, bm) in self.levels.iter().enumerate() {
            let shift = bits - 1 - level;
            let (za, zb) = (bm.rank0(a), bm.rank0(b));
            if (r >> shift) & 1 == 1 {
                // Every value with a 0 here is below r: count them, go right.
                count += zb - za;
                let z = self.zeros[level];
                a = (z + bm.rank(a)) as usize;
                b = (z + bm.rank(b)) as usize;
            } else {
                a = za as usize;
                b = zb as usize;
            }
        }
        count
    }

    /// The number of rows in `[a, b)` with a value in `[lo, hi)`.
    pub fn count_in(&self, a: usize, b: usize, lo: i64, hi: i64) -> u64 {
        if lo >= hi {
            return 0;
        }
        self.count_below(a, b, hi) - self.count_below(a, b, lo)
    }

    /// The values in `[lo, hi)` held by rows `[a, b)`, each with its rows in
    /// order: the leaves of the trie under the range, unwound by select.
    pub fn list(&self, a: usize, b: usize, lo: i64, hi: i64) -> Vec<(i64, Vec<usize>)> {
        let mut out = Vec::new();
        if a >= b || lo >= hi {
            return out;
        }
        let rlo = self.values.partition_point(|&v| v < lo) as u64;
        let rhi = self.values.partition_point(|&v| v < hi) as u64;
        if rlo >= rhi {
            return out;
        }
        self.list_rec(0, a, b, 0, rlo, rhi, &mut out);
        out
    }

    #[allow(clippy::too_many_arguments)]
    fn list_rec(&self, level: usize, a: usize, b: usize, prefix: u64, rlo: u64, rhi: u64, out: &mut Vec<(i64, Vec<usize>)>) {
        if a >= b {
            return;
        }
        let bits = self.levels.len();
        if level == bits {
            // A leaf: every row in [a, b) holds the value of rank `prefix`.
            if prefix >= rlo && prefix < rhi {
                let rows: Vec<usize> = (a..b).map(|i| self.unwind(i)).collect();
                out.push((self.values[prefix as usize], rows));
            }
            return;
        }
        let shift = bits - 1 - level;
        // The ranks under this node: [prefix << (shift + 1), (prefix + 1) << (shift + 1)).
        let node_lo = prefix << (shift + 1);
        let node_hi = (prefix + 1) << (shift + 1);
        if node_hi <= rlo || node_lo >= rhi {
            return;
        }
        let bm = &self.levels[level];
        let (za, zb) = (bm.rank0(a), bm.rank0(b));
        self.list_rec(level + 1, za as usize, zb as usize, prefix << 1, rlo, rhi, out);
        let z = self.zeros[level];
        self.list_rec(level + 1, (z + bm.rank(a)) as usize, (z + bm.rank(b)) as usize, (prefix << 1) | 1, rlo, rhi, out);
    }

    /// The row of a position at the last level, unwound through the levels
    /// by select.
    fn unwind(&self, mut i: usize) -> usize {
        for level in (0..self.levels.len()).rev() {
            let bm = &self.levels[level];
            let z = self.zeros[level] as usize;
            i = if i < z { bm.select0(i as u64).unwrap() } else { bm.select((i - z) as u64).unwrap() };
        }
        i
    }

    /// The bits the levels take (for the budget): about `⌈log₂ σ⌉ · 1.25`
    /// per value with the directories.
    pub fn bits(&self) -> usize {
        self.levels.iter().map(|b| b.len() + 64 * b.directory_words()).sum()
    }
}
