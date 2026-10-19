//! The blocks of the kernel's stored tables (M-8b; docs/spec/06-performance.md,
//! 3.1): a key's tuples row-major in one buffer, read as slices. A tuple of
//! the store has no allocation of its own, and a bar's block is sized like
//! the bar before and shrunk to its rows when the stream moves on.

use super::{Tuple, Value};

/// The tuples of one key of a stored table, row-major in one buffer
/// (M-8b): a tuple is a slice of `width` values with no allocation of its
/// own, read as `&[Value]` as a `Tuple` is.
#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
pub struct Rows {
    width: usize,
    cells: Vec<Value>,
}

impl Rows {
    /// Rows of `width` values with room for `rows` of them.
    pub fn with_capacity(width: usize, rows: usize) -> Rows {
        Rows {
            width,
            cells: Vec::with_capacity(width * rows),
        }
    }

    pub fn len(&self) -> usize {
        self.cells.len().checked_div(self.width).unwrap_or(0)
    }

    pub fn is_empty(&self) -> bool {
        self.cells.is_empty()
    }

    pub fn row(&self, i: usize) -> &[Value] {
        &self.cells[i * self.width..(i + 1) * self.width]
    }

    pub fn iter(&self) -> std::slice::ChunksExact<'_, Value> {
        self.cells.chunks_exact(self.width.max(1))
    }

    pub fn iter_mut(&mut self) -> std::slice::ChunksExactMut<'_, Value> {
        self.cells.chunks_exact_mut(self.width.max(1))
    }

    pub fn push(&mut self, tuple: Tuple) {
        if self.cells.is_empty() {
            self.width = tuple.len();
        }
        debug_assert_eq!(tuple.len(), self.width, "a tuple of another width");
        self.cells.extend(tuple);
    }

    /// Row `i` replaced by `tuple`.
    pub fn set(&mut self, i: usize, tuple: Tuple) {
        let w = self.width;
        for (c, v) in self.cells[i * w..(i + 1) * w].iter_mut().zip(tuple) {
            *c = v;
        }
    }

    pub fn clear(&mut self) {
        self.cells.clear();
    }

    pub fn shrink_to_fit(&mut self) {
        self.cells.shrink_to_fit();
    }

    /// The rows from `lo` to `hi`, excluded.
    pub fn slice(&self, lo: usize, hi: usize) -> RowSlice<'_> {
        RowSlice {
            width: self.width,
            cells: &self.cells[lo * self.width..hi * self.width],
        }
    }

    pub fn all(&self) -> RowSlice<'_> {
        self.slice(0, self.len())
    }

    /// The first row for which `pred` is false, the rows being partitioned
    /// by it.
    pub fn partition_point(&self, pred: impl Fn(&[Value]) -> bool) -> usize {
        let (mut lo, mut hi) = (0, self.len());
        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            if pred(self.row(mid)) {
                lo = mid + 1;
            } else {
                hi = mid;
            }
        }
        lo
    }

    /// The rows sorted by column `c`, stably.
    pub fn sort_by_column(&mut self, c: usize) {
        let n = self.len();
        let w = self.width;
        if (1..n).all(|i| self.cells[(i - 1) * w + c] <= self.cells[i * w + c]) {
            return;
        }
        let mut order: Vec<usize> = (0..n).collect();
        order.sort_by(|&a, &b| self.cells[a * w + c].cmp(&self.cells[b * w + c]));
        let mut cells = Vec::with_capacity(self.cells.len());
        for i in order {
            cells.extend_from_slice(&self.cells[i * w..(i + 1) * w]);
        }
        self.cells = cells;
    }
}

/// Some rows of a key, as `lookup` answers them.
#[derive(Clone, Copy, Debug, Default)]
pub struct RowSlice<'a> {
    width: usize,
    cells: &'a [Value],
}

impl<'a> RowSlice<'a> {
    pub fn len(&self) -> usize {
        self.cells.len().checked_div(self.width).unwrap_or(0)
    }

    pub fn is_empty(&self) -> bool {
        self.cells.is_empty()
    }

    pub fn iter(&self) -> std::slice::ChunksExact<'a, Value> {
        self.cells.chunks_exact(self.width.max(1))
    }

    /// The rows as tuples of their own.
    pub fn to_vec(&self) -> Vec<Tuple> {
        self.iter().map(|t| t.to_vec()).collect()
    }
}

impl<'a> IntoIterator for RowSlice<'a> {
    type Item = &'a [Value];
    type IntoIter = std::slice::ChunksExact<'a, Value>;
    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

impl<'a> IntoIterator for &'a Rows {
    type Item = &'a [Value];
    type IntoIter = std::slice::ChunksExact<'a, Value>;
    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tu(e: u32, v: f64) -> Tuple {
        vec![Value::Equity(e), Value::Time(7), Value::Num(v)]
    }

    fn values(rows: RowSlice<'_>) -> Vec<(u32, f64)> {
        rows.iter().map(|t| (t[0].as_equity().unwrap(), t[2].as_f64().unwrap())).collect()
    }

    #[test]
    fn rows_are_read_as_pushed_and_replaced_in_place() {
        let mut r = Rows::with_capacity(3, 2);
        for (e, v) in [(2, 1.0), (0, 2.0), (1, 3.0)] {
            r.push(tu(e, v));
        }
        assert_eq!((r.len(), r.is_empty()), (3, false));
        assert_eq!(r.row(1), &tu(0, 2.0)[..]);
        r.set(1, tu(0, 2.5));
        assert_eq!(values(r.all()), vec![(2, 1.0), (0, 2.5), (1, 3.0)]);
        assert_eq!(r.all().to_vec(), vec![tu(2, 1.0), tu(0, 2.5), tu(1, 3.0)]);
        r.shrink_to_fit();
        r.clear();
        assert!(r.is_empty() && r.all().iter().next().is_none());
    }

    #[test]
    fn a_sort_by_a_column_is_stable_and_bisected() {
        let mut r = Rows::default();
        for (e, v) in [(2, 1.0), (0, 2.0), (2, 3.0), (1, 4.0), (0, 5.0)] {
            r.push(tu(e, v));
        }
        r.sort_by_column(0);
        assert_eq!(values(r.all()), vec![(0, 2.0), (0, 5.0), (1, 4.0), (2, 1.0), (2, 3.0)]);
        let key = Value::Equity(2);
        let (lo, hi) = (r.partition_point(|t| t[0] < key), r.partition_point(|t| t[0] <= key));
        assert_eq!(values(r.slice(lo, hi)), vec![(2, 1.0), (2, 3.0)]);
        let none = Value::Equity(9);
        assert_eq!(r.partition_point(|t| t[0] < none), 5);
        assert!(r.slice(5, 5).is_empty());
    }

    #[test]
    fn no_rows_read_as_none() {
        let r = Rows::default();
        assert_eq!(r.len(), 0);
        assert!(RowSlice::default().iter().next().is_none());
        assert_eq!(r.partition_point(|_| true), 0);
    }
}
