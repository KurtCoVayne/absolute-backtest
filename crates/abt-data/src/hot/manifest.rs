//! The manifest of a hot version (ST-7.1): what the version holds and
//! where. Its id is the hash of its logical content, the blocks' hashes in
//! (column, instrument, block) order with the revisions and the sequence
//! high-water, so consolidation (a new layout of the same blocks) keeps it
//! (Proposition 7.1); the segment list is the layout.

use std::collections::BTreeMap;

use abt_wire::generated::abt::wire::RelationSchema;
use serde::{Deserialize, Serialize};

/// How a value column is held.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Kind {
    /// A `Num` column: ticks found from `f64` values (ST-3).
    Tick,
    /// A `Num` column whose source wrote `f32` values.
    TickF32,
    /// An integer column (a count, a label code, a bool, a time) at
    /// exponent 0.
    Int,
    /// The receipt residual `r − t`, at exponent 0.
    Residual,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ColumnEntry {
    pub name: String,
    pub kind: Kind,
    /// The finest exponent over the version's blocks of the column: the
    /// scale its ticks cross the wire at (ST-10.4).
    pub exponent: u8,
    /// Whether some block holds a value off any grid (`exact` refused).
    pub raw: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SegmentRef {
    pub hash: String,
    /// Relative to the store's root.
    pub file: String,
    pub bytes: u64,
}

/// A level of the ladder (ST-5.3); `levels[0]` is the base.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LevelEntry {
    pub calendar: String,
    /// The presence file, relative to the version's directory.
    pub presence: String,
    /// A ladder level's columns (its bar's value, the base rows it
    /// summarises, the sequence of its last base row, the receipt
    /// residual from the bucket's start); empty for the base, whose
    /// columns are the manifest's.
    #[serde(default)]
    pub columns: Vec<ColumnEntry>,
    /// Per value column (the manifest's order), the segments holding it, in
    /// the order the directory reads them.
    pub segments: Vec<Vec<SegmentRef>>,
}

/// A bucket of the partition calendar as a virtual partition (the
/// stream's unit, E-14): a range of label indices `[lo, hi)`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Bucket {
    pub bucket: String,
    pub lo: u32,
    pub hi: u32,
    pub t_min: i64,
    pub t_max: i64,
    pub r_min: i64,
    pub r_max: i64,
    pub rows: u64,
    pub seq_max: u64,
}

/// A row a later row of the same (instrument, label) superseded (ST-5.9):
/// kept whole, with its receipt interval `[r, r_to)`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Revision {
    pub id: u64,
    pub label: i64,
    pub r: i64,
    pub r_to: i64,
    pub seq: u64,
    /// Per value column, the superseded value's bits (`f64` bits for a
    /// tick column, the integer for an int column).
    pub values: Vec<u64>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct HotManifest {
    pub relation: String,
    pub version: String,
    pub source: String,
    pub schema: RelationSchema,
    /// The value columns in schema order, then the receipt residual.
    pub columns: Vec<ColumnEntry>,
    pub levels: Vec<LevelEntry>,
    pub rows: u64,
    pub from: Option<i64>,
    pub to: Option<i64>,
    pub seq_first: u64,
    pub seq_high: u64,
    pub buckets: Vec<Bucket>,
    pub revisions: Vec<Revision>,
    pub attributes: BTreeMap<String, String>,
}

impl HotManifest {
    /// The directory of the version, relative to the store's root.
    pub fn dir(relation: &str, version: &str) -> String {
        format!("hot/{}/{}", relation, version)
    }

    /// A segment's file, relative to the store's root.
    pub fn segment_file(hash: &str) -> String {
        format!("hot/segments/{}.seg", hash)
    }

    /// Every segment the version names, once each.
    pub fn segments(&self) -> Vec<&SegmentRef> {
        let mut out: Vec<&SegmentRef> = Vec::new();
        for l in &self.levels {
            for col in &l.segments {
                for s in col {
                    if !out.iter().any(|x| x.hash == s.hash) {
                        out.push(s);
                    }
                }
            }
        }
        out
    }
}
