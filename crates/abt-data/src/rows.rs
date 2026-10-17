//! Rows as the store keeps them: Arrow batches in a relation's column order,
//! entities `UInt64`, times nanoseconds, labels as the registry's `UInt32`
//! codes, a receipt time on every row (`r = t` when the source has none,
//! D-4.2), and the row's sequence number. Helpers to bring any input to that
//! layout, to sort by (t, identity, r) and to refuse conflicting duplicates.

use std::sync::Arc;

use abt_wire::generated::abt::wire::{ColumnType, RelationSchema, Role};
use arrow::array::{Array, ArrayRef, AsArray, BooleanArray, Float64Array, Int64Array, RecordBatch, StringArray, TimestampNanosecondArray, UInt32Array, UInt64Array};
use arrow::compute::{concat_batches, lexsort_to_indices, take_record_batch, SortColumn};
use arrow::datatypes::{DataType, Field, Schema, SchemaRef, TimeUnit};
use arrow::row::{RowConverter, SortField};

use crate::error::DataError;
use crate::registry::Registry;

/// The name of the sequence-number column of stored rows.
pub const SEQ: &str = "seq";

/// The Arrow type a column is stored as.
pub fn stored_type(ty: ColumnType) -> DataType {
    match ty {
        ColumnType::Entity => DataType::UInt64,
        ColumnType::Time => DataType::Timestamp(TimeUnit::Nanosecond, Some("UTC".into())),
        ColumnType::Num => DataType::Float64,
        ColumnType::Count => DataType::Int64,
        ColumnType::Label => DataType::UInt32,
        ColumnType::Bool => DataType::Boolean,
        ColumnType::Bytes => DataType::Binary,
    }
}

/// The Arrow type a column has in an answer (D-2.1: a label is a dictionary).
pub fn answer_type(ty: ColumnType) -> DataType {
    match ty {
        ColumnType::Label => DataType::Dictionary(Box::new(DataType::UInt32), Box::new(DataType::Utf8)),
        other => stored_type(other),
    }
}

/// The stored layout of a relation: its columns, then `seq`.
pub fn stored_schema(s: &RelationSchema) -> SchemaRef {
    let mut fields: Vec<Field> = s
        .columns
        .iter()
        .map(|c| Field::new(&c.name, stored_type(c.ty), !matches!(c.role, Role::Entity | Role::EventTime | Role::ReceiptTime)))
        .collect();
    fields.push(Field::new(SEQ, DataType::UInt64, false));
    Arc::new(Schema::new(fields))
}

/// The positions of a schema's roles.
#[derive(Clone, Debug)]
pub struct Layout {
    pub t: usize,
    pub r: Option<usize>,
    /// The entity columns: what the offset index and an identity filter read.
    pub identity: Vec<usize>,
    /// The fact's key besides `t` (ERRATA E-35): the entity columns, then the
    /// label columns, so that `member(A, T, "SPX")` and `member(A, T,
    /// "SP500")` are two facts and not two revisions of one.
    pub key: Vec<usize>,
    pub seq: usize,
}

pub fn layout(s: &RelationSchema) -> Layout {
    let t = s.columns.iter().position(|c| c.role == Role::EventTime).expect("a validated schema has an event time");
    let r = s.columns.iter().position(|c| c.role == Role::ReceiptTime);
    let identity: Vec<usize> = s.columns.iter().enumerate().filter(|(_, c)| c.role == Role::Entity).map(|(i, _)| i).collect();
    let key = identity
        .iter()
        .copied()
        .chain(s.columns.iter().enumerate().filter(|(_, c)| c.role == Role::Label).map(|(i, _)| i))
        .collect();
    Layout {
        t,
        r,
        identity,
        key,
        seq: s.columns.len(),
    }
}

/// An input column's values as stored values of type `ty`, interning labels.
fn conform_column(name: &str, ty: ColumnType, col: &ArrayRef, reg: &mut Registry) -> Result<ArrayRef, DataError> {
    let bad = || DataError::Schema(format!("column `{}` of type {} cannot hold {:?}", name, ty_name(ty), col.data_type()));
    Ok(match ty {
        ColumnType::Entity => match col.data_type() {
            DataType::UInt64 => col.clone(),
            DataType::Int64 | DataType::UInt32 | DataType::Int32 => arrow::compute::cast(col, &DataType::UInt64)?,
            _ => return Err(bad()),
        },
        ColumnType::Time => match col.data_type() {
            DataType::Timestamp(_, _) | DataType::Int64 | DataType::Date32 | DataType::Date64 => {
                let ns = match col.data_type() {
                    DataType::Timestamp(TimeUnit::Nanosecond, _) | DataType::Int64 => col.clone(),
                    DataType::Date32 | DataType::Date64 => arrow::compute::cast(&arrow::compute::cast(col, &DataType::Timestamp(TimeUnit::Nanosecond, None))?, &DataType::Int64)?,
                    _ => arrow::compute::cast(col, &DataType::Timestamp(TimeUnit::Nanosecond, None))?,
                };
                let ints = arrow::compute::cast(&ns, &DataType::Int64)?;
                let ints = ints.as_primitive::<arrow::datatypes::Int64Type>();
                Arc::new(TimestampNanosecondArray::from(ints.iter().collect::<Vec<_>>()).with_timezone("UTC"))
            }
            _ => return Err(bad()),
        },
        ColumnType::Num => match col.data_type() {
            DataType::Float64 => col.clone(),
            DataType::Float32 | DataType::Int64 | DataType::Int32 | DataType::UInt64 | DataType::UInt32 => arrow::compute::cast(col, &DataType::Float64)?,
            _ => return Err(bad()),
        },
        ColumnType::Count => match col.data_type() {
            DataType::Int64 => col.clone(),
            DataType::Int32 | DataType::UInt32 | DataType::UInt64 | DataType::Int16 | DataType::Int8 => arrow::compute::cast(col, &DataType::Int64)?,
            _ => return Err(bad()),
        },
        ColumnType::Bool => match col.data_type() {
            DataType::Boolean => col.clone(),
            _ => return Err(bad()),
        },
        ColumnType::Bytes => match col.data_type() {
            DataType::Binary => col.clone(),
            DataType::Utf8 => arrow::compute::cast(col, &DataType::Binary)?,
            _ => return Err(bad()),
        },
        ColumnType::Label => {
            let text = match col.data_type() {
                DataType::Utf8 => col.clone(),
                DataType::LargeUtf8 | DataType::Dictionary(_, _) => arrow::compute::cast(col, &DataType::Utf8)?,
                DataType::UInt32 => {
                    // Codes of this registry: refused when unknown.
                    let codes = col.as_primitive::<arrow::datatypes::UInt32Type>();
                    for c in codes.iter().flatten() {
                        if reg.label_text(c).is_none() {
                            return Err(DataError::Schema(format!("column `{}` holds the unknown label code {}", name, c)));
                        }
                    }
                    return Ok(col.clone());
                }
                _ => return Err(bad()),
            };
            let s = text.as_string::<i32>();
            Arc::new(UInt32Array::from(s.iter().map(|v| v.map(|v| reg.label(v))).collect::<Vec<_>>()))
        }
    })
}

fn ty_name(ty: ColumnType) -> &'static str {
    match ty {
        ColumnType::Entity => "Entity",
        ColumnType::Time => "Time",
        ColumnType::Num => "Num",
        ColumnType::Count => "Count",
        ColumnType::Label => "Label",
        ColumnType::Bool => "Bool",
        ColumnType::Bytes => "Bytes",
    }
}

/// Brings an input batch to the stored layout of `s` (D-8.1, schema
/// conformance): columns by name, cast to their stored types, labels
/// interned, a missing receipt time filled with the event time and a missing
/// value column with nulls, nulls refused in the identity and the times; `seq` numbers the rows from `first_seq` in
/// their input order.
pub fn conform(s: &RelationSchema, input: &RecordBatch, reg: &mut Registry, first_seq: u64) -> Result<RecordBatch, DataError> {
    let lay = layout(s);
    let mut cols: Vec<ArrayRef> = Vec::with_capacity(s.columns.len() + 1);
    for (i, c) in s.columns.iter().enumerate() {
        let found = input.schema().index_of(&c.name).ok().map(|j| input.column(j).clone());
        let col = match (found, c.role) {
            (Some(col), _) => conform_column(&c.name, c.ty, &col, reg)?,
            // D-4.2: a relation's rows without a receipt time are received at t.
            (None, Role::ReceiptTime) => cols[lay.t].clone(),
            // A value column the rows do not carry is null (the optional
            // columns of the registry's relations).
            (None, Role::Value) => arrow::array::new_null_array(&stored_type(c.ty), input.num_rows()),
            (None, _) => return Err(DataError::Schema(format!("`{}`: the rows have no column `{}`", s.name, c.name))),
        };
        if matches!(c.role, Role::Entity | Role::EventTime | Role::ReceiptTime) && col.null_count() > 0 {
            return Err(DataError::Schema(format!(
                "`{}`: column `{}` has a null in row {}",
                s.name,
                c.name,
                (0..col.len()).find(|&r| col.is_null(r)).unwrap_or(0)
            )));
        }
        debug_assert_eq!(i, cols.len());
        cols.push(col);
    }
    let n = input.num_rows() as u64;
    cols.push(Arc::new(UInt64Array::from((first_seq..first_seq + n).collect::<Vec<_>>())));
    Ok(RecordBatch::try_new(stored_schema(s), cols)?)
}

/// The rows sorted by (t, key, r, seq): the partitions' order (D-3.1).
pub fn sort_rows(s: &RelationSchema, rows: &RecordBatch) -> Result<RecordBatch, DataError> {
    if rows.num_rows() < 2 {
        return Ok(rows.clone());
    }
    let lay = layout(s);
    let mut keys: Vec<SortColumn> = Vec::new();
    keys.push(SortColumn {
        values: rows.column(lay.t).clone(),
        options: None,
    });
    for &i in &lay.key {
        keys.push(SortColumn {
            values: rows.column(i).clone(),
            options: None,
        });
    }
    if let Some(r) = lay.r {
        keys.push(SortColumn {
            values: rows.column(r).clone(),
            options: None,
        });
    }
    keys.push(SortColumn {
        values: rows.column(lay.seq).clone(),
        options: None,
    });
    // Rows already in order (a writer's answer in (t, entity) order, an
    // ingest in time order) are returned as they are: one comparison of
    // neighbours instead of a sort.
    let fields: Vec<SortField> = keys.iter().map(|k| SortField::new(k.values.data_type().clone())).collect();
    let converted = RowConverter::new(fields)?.convert_columns(&keys.iter().map(|k| k.values.clone()).collect::<Vec<_>>())?;
    if (1..converted.num_rows()).all(|i| converted.row(i - 1) <= converted.row(i)) {
        return Ok(rows.clone());
    }
    let idx = lexsort_to_indices(&keys, None)?;
    Ok(take_record_batch(rows, &idx)?)
}

/// One batch of every batch's rows (all of one stored layout).
pub fn concat(schema: &SchemaRef, batches: &[RecordBatch]) -> Result<RecordBatch, DataError> {
    Ok(concat_batches(schema, batches)?)
}

/// Sorted rows with exact duplicates dropped (the same fact delivered twice)
/// and conflicting ones refused: two rows of one key, `t` and `r` with
/// different values are not revisions of one fact (Definition D-2.2), and the
/// error names the row.
pub fn dedupe(s: &RelationSchema, sorted: &RecordBatch) -> Result<RecordBatch, DataError> {
    if sorted.num_rows() < 2 {
        return Ok(sorted.clone());
    }
    let lay = layout(s);
    let key_cols: Vec<usize> = std::iter::once(lay.t).chain(lay.key.iter().copied()).chain(lay.r).collect();
    let value_cols: Vec<usize> = (0..s.columns.len()).filter(|i| !key_cols.contains(i)).collect();
    let conv = |cols: &[usize]| -> Result<arrow::row::Rows, DataError> {
        let fields: Vec<SortField> = cols.iter().map(|&i| SortField::new(sorted.column(i).data_type().clone())).collect();
        let converter = RowConverter::new(fields)?;
        let arrays: Vec<ArrayRef> = cols.iter().map(|&i| sorted.column(i).clone()).collect();
        Ok(converter.convert_columns(&arrays)?)
    };
    let keys = conv(&key_cols)?;
    // The values are compared only where two neighbours share a key.
    if (1..sorted.num_rows()).all(|i| keys.row(i) != keys.row(i - 1)) {
        return Ok(sorted.clone());
    }
    let values = if value_cols.is_empty() { None } else { Some(conv(&value_cols)?) };
    let mut keep = vec![true; sorted.num_rows()];
    for (i, k) in keep.iter_mut().enumerate().skip(1) {
        if keys.row(i) == keys.row(i - 1) {
            let same = values.as_ref().map(|v| v.row(i) == v.row(i - 1)).unwrap_or(true);
            if !same {
                return Err(DataError::Schema(format!(
                    "`{}`: two rows of one identity at one time and receipt time differ (rows {} and {} of the sorted input)",
                    s.name,
                    i - 1,
                    i
                )));
            }
            *k = false;
        }
    }
    if keep.iter().all(|k| *k) {
        return Ok(sorted.clone());
    }
    Ok(arrow::compute::filter_record_batch(sorted, &BooleanArray::from(keep))?)
}

/// The rows' event times.
pub fn times(rows: &RecordBatch, col: usize) -> &TimestampNanosecondArray {
    rows.column(col).as_primitive::<arrow::datatypes::TimestampNanosecondType>()
}

/// The rows' sequence numbers.
pub fn seqs(rows: &RecordBatch, col: usize) -> &UInt64Array {
    rows.column(col).as_primitive::<arrow::datatypes::UInt64Type>()
}

/// A stored batch as an answer (D-4.1): labels decoded to a dictionary over
/// the registry's labels, `seq` kept when asked.
pub fn answer(s: &RelationSchema, rows: &RecordBatch, labels: &[String], with_seq: bool) -> Result<RecordBatch, DataError> {
    let mut fields = Vec::new();
    let mut cols: Vec<ArrayRef> = Vec::new();
    let dict_values: ArrayRef = Arc::new(StringArray::from(labels.to_vec()));
    for (i, c) in s.columns.iter().enumerate() {
        let col = rows.column(i);
        let col: ArrayRef = if c.ty == ColumnType::Label {
            let keys = col.as_primitive::<arrow::datatypes::UInt32Type>().clone();
            Arc::new(arrow::array::DictionaryArray::<arrow::datatypes::UInt32Type>::try_new(keys, dict_values.clone())?)
        } else {
            col.clone()
        };
        fields.push(Field::new(&c.name, col.data_type().clone(), col.null_count() > 0 || rows.schema().field(i).is_nullable()));
        cols.push(col);
    }
    if with_seq {
        fields.push(Field::new(SEQ, DataType::UInt64, false));
        cols.push(rows.column(s.columns.len()).clone());
    }
    Ok(RecordBatch::try_new(Arc::new(Schema::new(fields)), cols)?)
}

/// A float column (for the aggregate pass and tests).
pub fn floats(rows: &RecordBatch, col: usize) -> &Float64Array {
    rows.column(col).as_primitive::<arrow::datatypes::Float64Type>()
}

/// An integer column.
pub fn ints(rows: &RecordBatch, col: usize) -> &Int64Array {
    rows.column(col).as_primitive::<arrow::datatypes::Int64Type>()
}
