//! The query layer's relations (A-D20, P-4): trials, plans, lineages,
//! outcomes and reports are rows in the data layer under `query.*`, built
//! here as the kernel builds its outputs and put through the data service.
//! The query layer stores no byte itself.

use std::sync::Arc;

use abt_wire::generated::abt::wire::{ColumnSpec, ColumnType, RelationSchema, Role};
use arrow::array::{ArrayRef, BinaryArray, BooleanArray, Float64Array, Int64Array, StringArray, TimestampNanosecondArray, UInt64Array};
use arrow::datatypes::{DataType, Field, Schema, TimeUnit};
use arrow::record_batch::RecordBatch;

use crate::data::Error;

/// A column's values.
pub enum Col {
    Entity(Vec<Option<u64>>),
    Time(Vec<Option<i64>>),
    Num(Vec<Option<f64>>),
    Count(Vec<Option<i64>>),
    Label(Vec<Option<String>>),
    Bool(Vec<Option<bool>>),
    Bytes(Vec<Option<String>>),
}

impl Col {
    fn ty(&self) -> ColumnType {
        match self {
            Col::Entity(_) => ColumnType::Entity,
            Col::Time(_) => ColumnType::Time,
            Col::Num(_) => ColumnType::Num,
            Col::Count(_) => ColumnType::Count,
            Col::Label(_) => ColumnType::Label,
            Col::Bool(_) => ColumnType::Bool,
            Col::Bytes(_) => ColumnType::Bytes,
        }
    }

    fn len(&self) -> usize {
        match self {
            Col::Entity(v) => v.len(),
            Col::Time(v) | Col::Count(v) => v.len(),
            Col::Num(v) => v.len(),
            Col::Label(v) | Col::Bytes(v) => v.len(),
            Col::Bool(v) => v.len(),
        }
    }

    fn array(&self) -> (DataType, ArrayRef) {
        match self {
            Col::Entity(v) => (DataType::UInt64, Arc::new(UInt64Array::from(v.clone()))),
            Col::Time(v) => (DataType::Timestamp(TimeUnit::Nanosecond, None), Arc::new(TimestampNanosecondArray::from(v.clone()))),
            Col::Num(v) => (DataType::Float64, Arc::new(Float64Array::from(v.clone()))),
            Col::Count(v) => (DataType::Int64, Arc::new(Int64Array::from(v.clone()))),
            Col::Label(v) => (DataType::Utf8, Arc::new(StringArray::from(v.clone()))),
            Col::Bool(v) => (DataType::Boolean, Arc::new(BooleanArray::from(v.clone()))),
            Col::Bytes(v) => (DataType::Binary, Arc::new(BinaryArray::from(v.iter().map(|s| s.as_deref().map(|s| s.as_bytes())).collect::<Vec<_>>()))),
        }
    }
}

/// A relation being built: its columns with their roles. The identity of a
/// row is its entity and label columns with its time (D-2.2).
pub struct Rows {
    name: String,
    cols: Vec<(String, Role, Col)>,
}

impl Rows {
    pub fn new(name: impl Into<String>) -> Rows {
        Rows { name: name.into(), cols: Vec::new() }
    }

    pub fn col(mut self, name: &str, role: Role, c: Col) -> Rows {
        self.cols.push((name.to_string(), role, c));
        self
    }

    /// The number of rows (that of the first column).
    pub fn len(&self) -> usize {
        self.cols.first().map(|(_, _, c)| c.len()).unwrap_or(0)
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn finish(self) -> Result<(RelationSchema, RecordBatch), Error> {
        let schema = RelationSchema {
            name: self.name.clone(),
            class: "query".into(),
            columns: self
                .cols
                .iter()
                .map(|(n, role, c)| ColumnSpec {
                    name: n.clone(),
                    ty: c.ty(),
                    role: *role,
                    dimension: None,
                })
                .collect(),
            calendar: None,
            constraints: None,
            attributes: None,
            schema_version: Some("1.0".into()),
        };
        let (fields, arrays): (Vec<Field>, Vec<ArrayRef>) = self
            .cols
            .iter()
            .map(|(n, _, c)| {
                let (ty, a) = c.array();
                (Field::new(n, ty, true), a)
            })
            .unzip();
        let batch = RecordBatch::try_new(Arc::new(Schema::new(fields)), arrays).map_err(|e| Error::Malformed(format!("{}: {}", self.name, e)))?;
        Ok((schema, batch))
    }
}

/// A relation's rows read back: every batch of its current version
/// concatenated, none when the store has no such relation.
pub fn read(data: &crate::data::Data, relation: &str) -> Result<Option<RecordBatch>, Error> {
    let batches = match data.scan(relation) {
        Ok(b) => b,
        Err(Error::Missing(_)) => return Ok(None),
        Err(e) => return Err(e),
    };
    let Some(schema) = batches.first().map(|b| b.schema()) else { return Ok(None) };
    arrow::compute::concat_batches(&schema, &batches).map(Some).map_err(|e| Error::Malformed(e.to_string()))
}

fn column<'a>(b: &'a RecordBatch, name: &str) -> Result<&'a ArrayRef, Error> {
    b.column_by_name(name)
        .ok_or_else(|| Error::Malformed(format!("a relation the query layer reads has no column `{}`", name)))
}

/// A text column's values: a label (read as a dictionary or as text) or
/// bytes.
pub fn texts(b: &RecordBatch, name: &str) -> Result<Vec<Option<String>>, Error> {
    use arrow::array::{Array, AsArray};
    let c = column(b, name)?;
    if let DataType::Binary = c.data_type() {
        let a = c.as_binary::<i32>();
        return Ok((0..a.len()).map(|i| (!a.is_null(i)).then(|| String::from_utf8_lossy(a.value(i)).into_owned())).collect());
    }
    let s = arrow::compute::cast(c, &DataType::Utf8).map_err(|e| Error::Malformed(format!("{}: {}", name, e)))?;
    let a = s.as_string::<i32>();
    Ok((0..a.len()).map(|i| (!a.is_null(i)).then(|| a.value(i).to_string())).collect())
}

/// A number column's values.
pub fn nums(b: &RecordBatch, name: &str) -> Result<Vec<Option<f64>>, Error> {
    use arrow::array::Array;
    let c = column(b, name)?;
    let s = arrow::compute::cast(c, &DataType::Float64).map_err(|e| Error::Malformed(format!("{}: {}", name, e)))?;
    let a = s.as_any().downcast_ref::<Float64Array>().ok_or_else(|| Error::Malformed(format!("{} is not a number", name)))?;
    Ok((0..a.len()).map(|i| (!a.is_null(i)).then(|| a.value(i))).collect())
}

/// A time or count column's values as integers.
pub fn ints(b: &RecordBatch, name: &str) -> Result<Vec<Option<i64>>, Error> {
    use arrow::array::Array;
    let c = column(b, name)?;
    let s = arrow::compute::cast(c, &DataType::Int64).map_err(|e| Error::Malformed(format!("{}: {}", name, e)))?;
    let a = s.as_any().downcast_ref::<Int64Array>().ok_or_else(|| Error::Malformed(format!("{} is not an integer", name)))?;
    Ok((0..a.len()).map(|i| (!a.is_null(i)).then(|| a.value(i))).collect())
}

/// A boolean column's values.
pub fn bools(b: &RecordBatch, name: &str) -> Result<Vec<Option<bool>>, Error> {
    use arrow::array::Array;
    let c = column(b, name)?;
    let a = c.as_any().downcast_ref::<BooleanArray>().ok_or_else(|| Error::Malformed(format!("{} is not a boolean", name)))?;
    Ok((0..a.len()).map(|i| (!a.is_null(i)).then(|| a.value(i))).collect())
}
