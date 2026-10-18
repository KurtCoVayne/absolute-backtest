//! The structures precomputed over the reconstructed values of a column
//! (ST-5.1 to ST-5.3), per instrument: block summaries, cumulative
//! summaries and cross-moments at every block boundary, sparse tables over
//! block extremes, and the range rules that answer any `[a, b)` of rows
//! from them with at most two edges decoded (Proposition 6.1, range
//! closure). Values are integers at the instrument's finest exponent; a
//! caller aligns a block's integers before summarising.

use abt_agg::exact::Int;

/// What a block holds (ST-5): over its reconstructed values.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BlockSummary {
    pub n: u32,
    pub min: i64,
    pub max: i64,
    /// Rows within the block.
    pub argmin: u32,
    pub argmax: u32,
    pub first: i64,
    pub last: i64,
}

impl BlockSummary {
    pub fn of(values: &[i64]) -> BlockSummary {
        assert!(!values.is_empty());
        let (mut argmin, mut argmax) = (0usize, 0usize);
        for (i, &v) in values.iter().enumerate() {
            if v < values[argmin] {
                argmin = i;
            }
            if v > values[argmax] {
                argmax = i;
            }
        }
        BlockSummary {
            n: values.len() as u32,
            min: values[argmin],
            max: values[argmax],
            argmin: argmin as u32,
            argmax: argmax as u32,
            first: values[0],
            last: values[values.len() - 1],
        }
    }
}

/// The exact totals of a column's rows before a block boundary (ST-5.1):
/// `count`, `Σx`, `Σx²` and `Σk·x` with `k` the row's index from the
/// instrument's first row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Cumulative {
    pub count: u64,
    pub sum: i128,
    pub sum_sq: Int,
    pub sum_kx: Int,
}

impl Cumulative {
    pub const ZERO: Cumulative = Cumulative {
        count: 0,
        sum: 0,
        sum_sq: Int::ZERO,
        sum_kx: Int::ZERO,
    };

    /// `self` plus the rows `values` starting at row `k0`.
    pub fn plus(&self, values: &[i64], k0: u64) -> Cumulative {
        let mut out = self.clone();
        for (i, &x) in values.iter().enumerate() {
            out.add(k0 + i as u64, x);
        }
        out
    }

    pub fn add(&mut self, k: u64, x: i64) {
        self.count += 1;
        self.sum += x as i128;
        self.sum_sq = self.sum_sq.add(&Int::product(x as i128, x as i128));
        self.sum_kx = self.sum_kx.add(&Int::product(k as i128, x as i128));
    }

    /// `self − other` (the totals of the rows between two boundaries).
    pub fn minus(&self, other: &Cumulative) -> Cumulative {
        Cumulative {
            count: self.count - other.count,
            sum: self.sum - other.sum,
            sum_sq: self.sum_sq.sub(&other.sum_sq),
            sum_kx: self.sum_kx.sub(&other.sum_kx),
        }
    }

    pub fn plus_totals(&self, other: &Cumulative) -> Cumulative {
        Cumulative {
            count: self.count + other.count,
            sum: self.sum + other.sum,
            sum_sq: self.sum_sq.add(&other.sum_sq),
            sum_kx: self.sum_kx.add(&other.sum_kx),
        }
    }
}

/// The cumulative entries of an instrument's blocks: entry `b` holds the
/// totals of every row before block `b`; the last entry those of every
/// row. Built from the blocks' values in order.
pub fn cumulatives<'a>(blocks: impl IntoIterator<Item = &'a [i64]>) -> Vec<Cumulative> {
    let mut out = vec![Cumulative::ZERO];
    let mut k0 = 0u64;
    for b in blocks {
        let next = out.last().unwrap().plus(b, k0);
        k0 += b.len() as u64;
        out.push(next);
    }
    out
}

/// The cumulative cross-moments of a bar relation (ST-5.1): `Σ C·V` and
/// `Σ (H + L + C)·V`, exact.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CrossMoments {
    pub cv: Int,
    pub hlcv: Int,
}

impl CrossMoments {
    pub const ZERO: CrossMoments = CrossMoments { cv: Int::ZERO, hlcv: Int::ZERO };

    pub fn plus(&self, high: &[i64], low: &[i64], close: &[i64], volume: &[i64]) -> CrossMoments {
        let mut out = self.clone();
        for i in 0..close.len() {
            let v = volume[i] as i128;
            out.cv = out.cv.add(&Int::product(close[i] as i128, v));
            out.hlcv = out.hlcv.add(&Int::product(high[i] as i128 + low[i] as i128 + close[i] as i128, v));
        }
        out
    }

    pub fn minus(&self, other: &CrossMoments) -> CrossMoments {
        CrossMoments {
            cv: self.cv.sub(&other.cv),
            hlcv: self.hlcv.sub(&other.hlcv),
        }
    }
}

/// A sparse table over the blocks' minima and maxima (ST-5.2, Bender and
/// Farach-Colton): level `j` holds, for each block `b`, the extreme over
/// blocks `[b, b + 2^j)` and the block it is in.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SparseTable {
    min: Vec<Vec<(i64, u32)>>,
    max: Vec<Vec<(i64, u32)>>,
}

fn lesser(a: (i64, u32), b: (i64, u32)) -> (i64, u32) {
    if b.0 < a.0 {
        b
    } else {
        a
    }
}

fn greater(a: (i64, u32), b: (i64, u32)) -> (i64, u32) {
    if b.0 > a.0 {
        b
    } else {
        a
    }
}

impl SparseTable {
    pub fn of(summaries: &[BlockSummary]) -> SparseTable {
        let mut t = SparseTable {
            min: vec![Vec::new()],
            max: vec![Vec::new()],
        };
        for s in summaries {
            t.push(s);
        }
        t
    }

    pub fn blocks(&self) -> usize {
        self.min[0].len()
    }

    /// The minimum over blocks `[b0, b1)` and the block holding it (the
    /// first on a tie), in O(1): two overlapping cells.
    pub fn min(&self, b0: usize, b1: usize) -> (i64, u32) {
        assert!(b0 < b1 && b1 <= self.blocks());
        let j = (usize::BITS - 1 - (b1 - b0).leading_zeros()) as usize;
        lesser(self.min[j][b0], self.min[j][b1 - (1 << j)])
    }

    pub fn max(&self, b0: usize, b1: usize) -> (i64, u32) {
        assert!(b0 < b1 && b1 <= self.blocks());
        let j = (usize::BITS - 1 - (b1 - b0).leading_zeros()) as usize;
        greater(self.max[j][b0], self.max[j][b1 - (1 << j)])
    }

    /// Extends the table by one sealed block (ST-8.4): O(log m).
    pub fn push(&mut self, s: &BlockSummary) {
        let b = self.blocks();
        self.min[0].push((s.min, b as u32));
        self.max[0].push((s.max, b as u32));
        let (mut span, mut j) = (1usize, 1usize);
        while span * 2 <= b + 1 {
            if self.min.len() == j {
                self.min.push(Vec::new());
                self.max.push(Vec::new());
            }
            // Level j's cell for the start that ends at block b.
            let start = b + 1 - span * 2;
            debug_assert_eq!(self.min[j].len(), start);
            let mn = lesser(self.min[j - 1][start], self.min[j - 1][start + span]);
            let mx = greater(self.max[j - 1][start], self.max[j - 1][start + span]);
            self.min[j].push(mn);
            self.max[j].push(mx);
            span *= 2;
            j += 1;
        }
    }
}

/// A column of an instrument as the range rules read it: the block of a
/// row, its rows decoded on demand, and the structures over it.
pub trait Column {
    /// The values of block `b`, decoded whole.
    fn block(&self, b: usize) -> Vec<i64>;
    /// The number of values in block `b`.
    fn len(&self, b: usize) -> usize;
    fn blocks(&self) -> usize;
    /// The totals before block boundary `boundary` (0 to `blocks()`).
    fn cumulative(&self, boundary: usize) -> &Cumulative;
    fn summary(&self, b: usize) -> &BlockSummary;
    fn sparse(&self) -> &SparseTable;
    /// The block holding row `r` and the row's index within it.
    fn locate(&self, r: u64) -> (usize, usize) {
        ((r / crate::BLOCK as u64) as usize, (r % crate::BLOCK as u64) as usize)
    }
}

/// The exact moments of rows `[a, b)` (ST-5.1): whole blocks from two
/// cumulative entries, the edges decoded, each edge by its shorter side
/// (the prefix when it is the shorter, otherwise the block total minus the
/// suffix). A window within two blocks is decoded directly.
pub fn moments(col: &dyn Column, a: u64, b: u64) -> Cumulative {
    if a >= b {
        return Cumulative::ZERO;
    }
    let (ba, ia) = col.locate(a);
    let (bb, ib) = col.locate(b - 1);
    let direct = |blk: usize, lo: usize, hi: usize| -> Cumulative {
        let v = col.block(blk);
        Cumulative::ZERO.plus(&v[lo..hi], blk as u64 * crate::BLOCK as u64 + lo as u64)
    };
    if bb - ba <= 1 {
        if ba == bb {
            return direct(ba, ia, ib + 1);
        }
        return direct(ba, ia, col.len(ba)).plus_totals(&direct(bb, 0, ib + 1));
    }
    let whole = col.cumulative(bb).minus(col.cumulative(ba + 1));
    let head = edge(col, ba, ia, col.len(ba));
    let tail = edge(col, bb, 0, ib + 1);
    whole.plus_totals(&head).plus_totals(&tail)
}

/// The totals of rows `[lo, hi)` of block `blk`, by the shorter side.
fn edge(col: &dyn Column, blk: usize, lo: usize, hi: usize) -> Cumulative {
    let n = col.len(blk);
    let k0 = blk as u64 * crate::BLOCK as u64;
    let v = col.block(blk);
    if hi - lo <= n / 2 {
        Cumulative::ZERO.plus(&v[lo..hi], k0 + lo as u64)
    } else {
        let total = col.cumulative(blk + 1).minus(col.cumulative(blk));
        let before = Cumulative::ZERO.plus(&v[..lo], k0);
        let after = Cumulative::ZERO.plus(&v[hi..], k0 + hi as u64);
        total.minus(&before).minus(&after)
    }
}

/// The minimum and maximum of rows `[a, b)` with the row of each (the
/// first on a tie; ST-5.2): the sparse table over whole blocks, the edges
/// decoded. `(min, its row, max, its row)`.
pub fn extremes(col: &dyn Column, a: u64, b: u64) -> Option<(i64, u64, i64, u64)> {
    if a >= b {
        return None;
    }
    let (ba, ia) = col.locate(a);
    let (bb, ib) = col.locate(b - 1);
    let mut best: Option<(i64, u64, i64, u64)> = None;
    let mut take = |v: i64, row: u64| {
        best = Some(match best {
            None => (v, row, v, row),
            Some((mn, rmn, mx, rmx)) => (if v < mn { v } else { mn }, if v < mn { row } else { rmn }, if v > mx { v } else { mx }, if v > mx { row } else { rmx }),
        });
    };
    let scan = |blk: usize, lo: usize, hi: usize, take: &mut dyn FnMut(i64, u64)| {
        let v = col.block(blk);
        for (i, &x) in v[lo..hi].iter().enumerate() {
            take(x, blk as u64 * crate::BLOCK as u64 + (lo + i) as u64);
        }
    };
    if ba == bb {
        scan(ba, ia, ib + 1, &mut take);
        return best;
    }
    scan(ba, ia, col.len(ba), &mut take);
    if bb - ba >= 2 {
        let (mn, bmn) = col.sparse().min(ba + 1, bb);
        let (mx, bmx) = col.sparse().max(ba + 1, bb);
        let smn = col.summary(bmn as usize);
        let smx = col.summary(bmx as usize);
        take(mn, bmn as u64 * crate::BLOCK as u64 + smn.argmin as u64);
        take(mx, bmx as u64 * crate::BLOCK as u64 + smx.argmax as u64);
    }
    scan(bb, 0, ib + 1, &mut take);
    best
}

/// The value at row `r`.
pub fn point(col: &dyn Column, r: u64) -> i64 {
    let (b, i) = col.locate(r);
    col.block(b)[i]
}

/// A bar over rows `[a, b)` of a bar relation's columns (ST-5.3): open at
/// `a`, close at `b − 1`, the high's maximum, the low's minimum and the
/// volume's sum: `(open, high, low, close, volume)`.
pub fn bar(open: &dyn Column, high: &dyn Column, low: &dyn Column, close: &dyn Column, volume: &dyn Column, a: u64, b: u64) -> Option<(i64, i64, i64, i64, i128)> {
    if a >= b {
        return None;
    }
    let o = point(open, a);
    let c = point(close, b - 1);
    let (_, _, h, _) = extremes(high, a, b)?;
    let (l, _, _, _) = extremes(low, a, b)?;
    let v = moments(volume, a, b).sum;
    Some((o, h, l, c, v))
}

/// A column held in memory as its blocks' values: the test and staging
/// form of `Column`, with its structures built on construction.
#[derive(Clone, Debug)]
pub struct Decoded {
    pub blocks: Vec<Vec<i64>>,
    pub cumulatives: Vec<Cumulative>,
    pub summaries: Vec<BlockSummary>,
    pub sparse: SparseTable,
}

impl Decoded {
    pub fn of(blocks: Vec<Vec<i64>>) -> Decoded {
        assert!(blocks.iter().all(|b| !b.is_empty() && b.len() <= crate::BLOCK));
        assert!(blocks.iter().take(blocks.len().saturating_sub(1)).all(|b| b.len() == crate::BLOCK), "every block but the last is full");
        let cumulatives = cumulatives(blocks.iter().map(|b| b.as_slice()));
        let summaries: Vec<BlockSummary> = blocks.iter().map(|b| BlockSummary::of(b)).collect();
        let sparse = SparseTable::of(&summaries);
        Decoded {
            blocks,
            cumulatives,
            summaries,
            sparse,
        }
    }

    pub fn rows(&self) -> u64 {
        self.cumulatives.last().unwrap().count
    }
}

impl Column for Decoded {
    fn block(&self, b: usize) -> Vec<i64> {
        self.blocks[b].clone()
    }
    fn len(&self, b: usize) -> usize {
        self.blocks[b].len()
    }
    fn blocks(&self) -> usize {
        self.blocks.len()
    }
    fn cumulative(&self, boundary: usize) -> &Cumulative {
        &self.cumulatives[boundary]
    }
    fn summary(&self, b: usize) -> &BlockSummary {
        &self.summaries[b]
    }
    fn sparse(&self) -> &SparseTable {
        &self.sparse
    }
}
