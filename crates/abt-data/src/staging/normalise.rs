//! S1 Normalise (ST-7): a relation's conformed rows as the hot store takes
//! them: per instrument, its rows in (time, receipt, sequence) order with
//! every value as the bits the block must reproduce (an `f64`'s bits for a
//! tick column, the integer for an int column), identical rows merged and
//! conflicting ones refused (D-2.2, as `rows::dedupe`).

use std::collections::BTreeMap;

use abt_wire::generated::abt::wire::{ColumnType, RelationSchema, Role};
use arrow::array::{Array, AsArray, RecordBatch};
use arrow::datatypes::DataType;

use crate::error::DataError;
use crate::hot::manifest::Kind;
use crate::rows::{self, Layout};

/// A row as staging holds it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Row {
    pub t: i64,
    pub r: i64,
    /// The input's provisional sequence number (its order of acceptance).
    pub seq: u64,
    /// Per value column: an `f64`'s bits (tick) or the integer (int).
    pub vals: Vec<u64>,
}

/// The rows of a relation, normalised.
#[derive(Clone, Debug)]
pub struct Normalised {
    pub kinds: Vec<Kind>,
    /// The schema indices of the value columns, in `kinds` order (the
    /// residual has none).
    pub value_cols: Vec<usize>,
    pub by_id: BTreeMap<u64, Vec<Row>>,
}

/// The kinds of a relation's value columns, then the receipt residual;
/// `f32` when the source wrote single-precision floats.
pub fn kinds(s: &RelationSchema, lay: &Layout, f32: bool) -> Result<(Vec<Kind>, Vec<usize>), DataError> {
    let mut kinds = Vec::new();
    let mut cols = Vec::new();
    for (i, c) in s.columns.iter().enumerate() {
        if i == lay.t || Some(i) == lay.r || lay.identity.contains(&i) {
            continue;
        }
        let k = match c.ty {
            ColumnType::Num => {
                if f32 {
                    Kind::TickF32
                } else {
                    Kind::Tick
                }
            }
            ColumnType::Count | ColumnType::Label | ColumnType::Bool | ColumnType::Time => Kind::Int,
            ColumnType::Tick | ColumnType::Bytes | ColumnType::Entity => return Err(DataError::Schema(format!("`{}`: column `{}` of type {:?} is not held hot", s.name, c.name, c.ty))),
        };
        kinds.push(k);
        cols.push(i);
    }
    kinds.push(Kind::Residual);
    Ok((kinds, cols))
}

/// Normalises conformed rows (the stored layout, `seq` last).
pub fn normalise(s: &RelationSchema, lay: &Layout, all: &RecordBatch, f32: bool) -> Result<Normalised, DataError> {
    let (kinds, value_cols) = kinds(s, lay, f32)?;
    let ids = all.column(lay.identity[0]).as_primitive::<arrow::datatypes::UInt64Type>();
    let ts = rows::times(all, lay.t);
    let rs = lay.r.map(|r| rows::times(all, r));
    let seqs = rows::seqs(all, lay.seq);
    let n = all.num_rows();
    // Every value column's bits.
    let mut bits: Vec<Vec<u64>> = Vec::with_capacity(value_cols.len());
    for (&c, k) in value_cols.iter().zip(&kinds) {
        let col = all.column(c);
        if col.null_count() > 0 {
            let i = (0..n).find(|&i| col.is_null(i)).unwrap_or(0);
            return Err(DataError::Schema(format!(
                "`{}`: column `{}` has a null in row {} (a hot relation holds every value)",
                s.name, s.columns[c].name, i
            )));
        }
        let v: Vec<u64> = match (k, col.data_type()) {
            (Kind::Tick | Kind::TickF32, DataType::Float64) => col.as_primitive::<arrow::datatypes::Float64Type>().values().iter().map(|x| x.to_bits()).collect(),
            (Kind::Int, DataType::Int64) => col.as_primitive::<arrow::datatypes::Int64Type>().values().iter().map(|&x| x as u64).collect(),
            (Kind::Int, DataType::UInt32) => col.as_primitive::<arrow::datatypes::UInt32Type>().values().iter().map(|&x| x as u64).collect(),
            (Kind::Int, DataType::Boolean) => (0..n).map(|i| col.as_boolean().value(i) as u64).collect(),
            (Kind::Int, DataType::Timestamp(_, _)) => col.as_primitive::<arrow::datatypes::TimestampNanosecondType>().values().iter().map(|&x| x as u64).collect(),
            (k, ty) => return Err(DataError::Schema(format!("`{}`: column `{}` is {:?} in the rows, not a {:?}", s.name, s.columns[c].name, ty, k))),
        };
        bits.push(v);
    }
    let mut by_id: BTreeMap<u64, Vec<Row>> = BTreeMap::new();
    for i in 0..n {
        let t = ts.value(i);
        let r = rs.map(|r| r.value(i)).unwrap_or(t);
        by_id.entry(ids.value(i)).or_default().push(Row {
            t,
            r,
            seq: seqs.value(i),
            vals: bits.iter().map(|b| b[i]).collect(),
        });
    }
    for (id, rows) in by_id.iter_mut() {
        rows.sort_by_key(|x| (x.t, x.r, x.seq));
        // Identical rows (one fact delivered twice) merge; two values of
        // one (t, r) are not revisions of one fact.
        let mut out: Vec<Row> = Vec::with_capacity(rows.len());
        for x in rows.drain(..) {
            if let Some(last) = out.last() {
                if last.t == x.t && last.r == x.r {
                    if last.vals != x.vals {
                        return Err(DataError::Schema(format!(
                            "`{}`: two rows of instrument {} at {} with one receipt time differ (sequence numbers {} and {})",
                            s.name,
                            id,
                            abt_wire::scalar::Time(x.t).iso(),
                            last.seq,
                            x.seq
                        )));
                    }
                    continue;
                }
            }
            out.push(x);
        }
        *rows = out;
    }
    Ok(Normalised { kinds, value_cols, by_id })
}

/// The role a schema column plays, for the batch builders.
pub fn is_value(s: &RelationSchema, lay: &Layout, i: usize) -> bool {
    i != lay.t && Some(i) != lay.r && !lay.identity.contains(&i) && s.columns[i].role != Role::Entity
}
