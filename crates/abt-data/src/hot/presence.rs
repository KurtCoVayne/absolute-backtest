//! The presence file of a hot version (ST-4.7, ST-5.4): the level's
//! labels (the sorted distinct times of the relation's rows), and per
//! instrument its id, its alive interval over the labels (the first and
//! last label indices it has a row at), its row count, the sequence number
//! of its first row (rows are numbered per instrument in label order,
//! ST-8.2) and its presence bits over the interval; the rank directories
//! are rebuilt at open.
//!
//! ```text
//! magic "ABTPRS01"
//! labels u64, then i64 × labels
//! instruments u64, then per instrument: id u64, first u32, last u32,
//!     rows u64, exponent u8 × 8 (per value column, 0 past the eighth: the
//!     instrument's finest exponents), seq runs u32, (row u64, seq u64,
//!     count u64) × runs, words u64, u64 × words
//! ```
//!
//! A sequence run says that rows `[row, row + count)` carry the sequence
//! numbers `[seq, seq + count)`: one run per ingest that extended the
//! instrument, two more around a row inserted into its history.

use std::collections::HashMap;

use abt_codec::rank::Bitmap;

use crate::error::DataError;

const MAGIC: &[u8; 8] = b"ABTPRS01";

/// An instrument of a version.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Instrument {
    pub id: u64,
    /// The label indices of its first and last rows (inclusive).
    pub first: u32,
    pub last: u32,
    pub rows: u64,
    /// `(row, seq, count)`: rows `[row, row + count)` carry sequence
    /// numbers `[seq, seq + count)`; sorted by row, covering every row.
    pub seq_runs: Vec<(u64, u64, u64)>,
    /// The finest exponent of each value column over the instrument's
    /// blocks: its summaries' exponent.
    pub exponents: Vec<u8>,
    pub presence: Bitmap,
}

impl Instrument {
    /// The sequence number of row `k`.
    pub fn seq_of(&self, k: u64) -> u64 {
        let i = self.seq_runs.partition_point(|r| r.0 <= k);
        let (row, seq, _) = self.seq_runs[i.saturating_sub(1)];
        seq + (k - row)
    }

    /// The greatest sequence number of the instrument's rows.
    pub fn seq_max(&self) -> u64 {
        self.seq_runs.iter().map(|r| r.1 + r.2 - 1).max().unwrap_or(0)
    }

    /// The row of label index `label` (the number of set bits before it
    /// within the interval), and whether the label has a row.
    pub fn row_of(&self, label: u32) -> (u64, bool) {
        if label < self.first {
            return (0, false);
        }
        if label > self.last {
            return (self.rows, false);
        }
        let i = (label - self.first) as usize;
        (self.presence.rank(i), self.presence.get(i))
    }

    /// The rows whose labels are in `[lo, hi]` (label indices, inclusive).
    pub fn rows_in(&self, lo: u32, hi: u32) -> (u64, u64) {
        if hi < self.first || lo > self.last || lo > hi {
            return (0, 0);
        }
        let a = self.row_of(lo.max(self.first)).0;
        let b = if hi >= self.last { self.rows } else { self.row_of(hi + 1).0 };
        (a, b)
    }

    /// The label index of row `k`.
    pub fn label_of(&self, k: u64) -> u32 {
        self.first + self.presence.select(k).expect("a row below the count") as u32
    }
}

/// The presence of a version: labels and instruments in id order.
#[derive(Clone, Debug, Default)]
pub struct Presence {
    pub labels: Vec<i64>,
    pub instruments: Vec<Instrument>,
    by_id: HashMap<u64, u32>,
}

impl Presence {
    pub fn new(labels: Vec<i64>, instruments: Vec<Instrument>) -> Presence {
        debug_assert!(instruments.windows(2).all(|w| w[0].id < w[1].id), "instruments in id order");
        let by_id = instruments.iter().enumerate().map(|(i, x)| (x.id, i as u32)).collect();
        Presence { labels, instruments, by_id }
    }

    pub fn ordinal(&self, id: u64) -> Option<u32> {
        self.by_id.get(&id).copied()
    }

    /// The label indices of the labels in `[t0, t1]` (inclusive), as
    /// `[lo, hi)`.
    pub fn label_range(&self, t0: Option<i64>, t1: Option<i64>) -> (u32, u32) {
        let lo = t0.map(|t| self.labels.partition_point(|&l| l < t)).unwrap_or(0);
        let hi = t1.map(|t| self.labels.partition_point(|&l| l <= t)).unwrap_or(self.labels.len());
        (lo as u32, hi.max(lo) as u32)
    }

    pub fn rows(&self) -> u64 {
        self.instruments.iter().map(|i| i.rows).sum()
    }

    pub fn to_bytes(&self) -> Vec<u8> {
        let mut b = Vec::new();
        b.extend_from_slice(MAGIC);
        b.extend_from_slice(&(self.labels.len() as u64).to_le_bytes());
        for l in &self.labels {
            b.extend_from_slice(&l.to_le_bytes());
        }
        b.extend_from_slice(&(self.instruments.len() as u64).to_le_bytes());
        for i in &self.instruments {
            b.extend_from_slice(&i.id.to_le_bytes());
            b.extend_from_slice(&i.first.to_le_bytes());
            b.extend_from_slice(&i.last.to_le_bytes());
            b.extend_from_slice(&i.rows.to_le_bytes());
            let mut e = [0u8; 8];
            for (k, x) in i.exponents.iter().take(8).enumerate() {
                e[k] = *x;
            }
            b.extend_from_slice(&e);
            b.extend_from_slice(&(i.seq_runs.len() as u32).to_le_bytes());
            for (row, seq, count) in &i.seq_runs {
                b.extend_from_slice(&row.to_le_bytes());
                b.extend_from_slice(&seq.to_le_bytes());
                b.extend_from_slice(&count.to_le_bytes());
            }
            let words = i.presence.words();
            b.extend_from_slice(&(words.len() as u64).to_le_bytes());
            for w in words {
                b.extend_from_slice(&w.to_le_bytes());
            }
        }
        b
    }

    pub fn from_bytes(b: &[u8], columns: usize) -> Result<Presence, DataError> {
        let bad = |what: &str| DataError::Schema(format!("presence file: {}", what));
        let mut at = 0usize;
        let mut take = |n: usize| -> Result<&[u8], DataError> {
            if at + n > b.len() {
                return Err(bad("truncated"));
            }
            let s = &b[at..at + n];
            at += n;
            Ok(s)
        };
        if take(8)? != MAGIC {
            return Err(bad("not a presence file"));
        }
        let n = u64::from_le_bytes(take(8)?.try_into().unwrap()) as usize;
        let mut labels = Vec::with_capacity(n);
        for _ in 0..n {
            labels.push(i64::from_le_bytes(take(8)?.try_into().unwrap()));
        }
        let m = u64::from_le_bytes(take(8)?.try_into().unwrap()) as usize;
        let mut instruments = Vec::with_capacity(m);
        for _ in 0..m {
            let id = u64::from_le_bytes(take(8)?.try_into().unwrap());
            let first = u32::from_le_bytes(take(4)?.try_into().unwrap());
            let last = u32::from_le_bytes(take(4)?.try_into().unwrap());
            let rows = u64::from_le_bytes(take(8)?.try_into().unwrap());
            let e = take(8)?;
            let exponents: Vec<u8> = e[..columns.min(8)].to_vec();
            let runs = u32::from_le_bytes(take(4)?.try_into().unwrap()) as usize;
            let mut seq_runs = Vec::with_capacity(runs);
            for _ in 0..runs {
                let row = u64::from_le_bytes(take(8)?.try_into().unwrap());
                let seq = u64::from_le_bytes(take(8)?.try_into().unwrap());
                let count = u64::from_le_bytes(take(8)?.try_into().unwrap());
                seq_runs.push((row, seq, count));
            }
            if seq_runs.iter().map(|r| r.2).sum::<u64>() != rows || seq_runs.first().is_none_or(|r| r.0 != 0) {
                return Err(bad("an instrument's sequence runs do not cover its rows"));
            }
            let words = u64::from_le_bytes(take(8)?.try_into().unwrap()) as usize;
            let mut ws = Vec::with_capacity(words);
            for _ in 0..words {
                ws.push(u64::from_le_bytes(take(8)?.try_into().unwrap()));
            }
            if last < first || last as usize >= labels.len() {
                return Err(bad("an instrument's interval is outside the labels"));
            }
            let len = (last - first + 1) as usize;
            if words < len.div_ceil(64) {
                return Err(bad("an instrument's bits are short"));
            }
            let presence = Bitmap::from_words(len, &ws);
            if presence.ones() != rows {
                return Err(bad("an instrument's bits do not count its rows"));
            }
            instruments.push(Instrument {
                id,
                first,
                last,
                rows,
                seq_runs,
                exponents,
                presence,
            });
        }
        Ok(Presence::new(labels, instruments))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn presence_round_trips_and_ranks_labels_to_rows() {
        let labels: Vec<i64> = (0..100).map(|i| i * abt_wire::scalar::NANOS_PER_DAY).collect();
        let bits: Vec<bool> = (0..60).map(|i| i % 3 != 1).collect();
        let a = Instrument {
            id: 7,
            first: 10,
            last: 69,
            rows: bits.iter().filter(|&&b| b).count() as u64,
            seq_runs: vec![(0, 1, bits.iter().filter(|&&b| b).count() as u64)],
            exponents: vec![2, 0],
            presence: Bitmap::from_bools(&bits),
        };
        let b = Instrument {
            id: 9,
            first: 0,
            last: 0,
            rows: 1,
            seq_runs: vec![(0, 41, 1)],
            exponents: vec![4, 0],
            presence: Bitmap::from_bools(&[true]),
        };
        let p = Presence::new(labels.clone(), vec![a.clone(), b.clone()]);
        let again = Presence::from_bytes(&p.to_bytes(), 2).unwrap();
        assert_eq!(again.labels, labels);
        assert_eq!(again.instruments, vec![a.clone(), b]);
        assert_eq!(again.ordinal(9), Some(1));
        assert_eq!(again.ordinal(8), None);
        // Label 10 is row 0 and present; label 11 absent, row 1 is label 12.
        assert_eq!(a.row_of(10), (0, true));
        assert_eq!(a.row_of(11), (1, false));
        assert_eq!(a.row_of(12), (1, true));
        assert_eq!(a.label_of(1), 12);
        assert_eq!(a.seq_of(0), 1);
        assert_eq!(a.seq_of(5), 6);
        let split = Instrument {
            seq_runs: vec![(0, 1, 3), (3, 90, 1), (4, 4, a.rows - 4)],
            ..a.clone()
        };
        assert_eq!((split.seq_of(2), split.seq_of(3), split.seq_of(4), split.seq_of(10)), (3, 90, 4, 10));
        assert_eq!(split.seq_max(), 90);
        assert_eq!(a.row_of(5), (0, false));
        assert_eq!(a.row_of(80), (a.rows, false));
        // Rows of a label range: [10, 12] holds labels 10 and 12.
        assert_eq!(a.rows_in(10, 12), (0, 2));
        assert_eq!(a.rows_in(0, 9), (0, 0));
        assert_eq!(a.rows_in(60, 99), (a.row_of(60).0, a.rows));
        assert_eq!(p.label_range(Some(abt_wire::scalar::NANOS_PER_DAY), Some(3 * abt_wire::scalar::NANOS_PER_DAY)), (1, 4));
        assert_eq!(p.label_range(None, None), (0, 100));
        assert_eq!(p.label_range(Some(1), Some(1)), (1, 1));
    }
}
