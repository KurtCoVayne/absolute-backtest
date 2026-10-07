//! Parquet tables: the one file format of every input and output outside a
//! bundle's log (vendor exports, loose data directories, the security
//! table, reviewed exceptions, `--dump` and `--nav`). A table is read whole
//! as Arrow batches and its cells are reached by column name and row, so a
//! reader never builds per-row maps; typed columns (integers, floats,
//! dates, timestamps) and text columns are both accepted, text being parsed
//! the way a typed column would be read.

use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use arrow::array::{
    Array, ArrayRef, BooleanArray, Date32Array, Date64Array, Float32Array, Float64Array, Int16Array, Int32Array, Int64Array, Int8Array, LargeStringArray, StringArray, TimestampMicrosecondArray,
    TimestampMillisecondArray, TimestampNanosecondArray, TimestampSecondArray, UInt16Array, UInt32Array, UInt64Array, UInt8Array,
};
use arrow::datatypes::{DataType, Field, Schema, TimeUnit};
use arrow::record_batch::RecordBatch;
use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;
use parquet::arrow::ArrowWriter;

use crate::kernel::time::{parse_timestamp, DAY};

/// One cell, as stored.
#[derive(Clone, Debug, PartialEq)]
pub enum Cell<'a> {
    Null,
    Str(&'a str),
    Int(i64),
    Float(f64),
    Bool(bool),
    /// Seconds since 1970-01-01, naive wall clock (a date is its midnight).
    Time(i64),
}

/// A Parquet file read into memory.
pub struct Table {
    path: PathBuf,
    names: Vec<String>,
    batches: Vec<RecordBatch>,
    /// The first row of each batch.
    starts: Vec<usize>,
    rows: usize,
}

impl Table {
    /// Read a whole file; column names are lower-cased.
    pub fn read(path: &Path) -> Result<Table, String> {
        let file = File::open(path).map_err(|e| format!("{}: {}", path.display(), e))?;
        let builder = ParquetRecordBatchReaderBuilder::try_new(file).map_err(|e| format!("{}: {}", path.display(), e))?;
        let names = builder.schema().fields().iter().map(|f| f.name().to_lowercase()).collect();
        let reader = builder.build().map_err(|e| format!("{}: {}", path.display(), e))?;
        let mut batches = Vec::new();
        let mut starts = Vec::new();
        let mut rows = 0;
        for batch in reader {
            let batch = batch.map_err(|e| format!("{}: {}", path.display(), e))?;
            starts.push(rows);
            rows += batch.num_rows();
            batches.push(batch);
        }
        Ok(Table {
            path: path.to_path_buf(),
            names,
            batches,
            starts,
            rows,
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn len(&self) -> usize {
        self.rows
    }

    pub fn is_empty(&self) -> bool {
        self.rows == 0
    }

    pub fn names(&self) -> &[String] {
        &self.names
    }

    pub fn has(&self, name: &str) -> bool {
        self.names.iter().any(|n| n == name)
    }

    /// The position of a column, or an error naming the file.
    pub fn col(&self, name: &str) -> Result<usize, String> {
        self.names.iter().position(|n| n == name).ok_or_else(|| format!("{}: no column `{}`", self.path.display(), name))
    }

    /// Where a row is, for messages: `path row N` (1-based).
    pub fn at(&self, row: usize) -> String {
        format!("{} row {}", self.path.display(), row + 1)
    }

    fn locate(&self, row: usize) -> (&RecordBatch, usize) {
        let b = self.starts.partition_point(|&s| s <= row) - 1;
        (&self.batches[b], row - self.starts[b])
    }

    pub fn cell(&self, c: usize, row: usize) -> Cell<'_> {
        let (batch, r) = self.locate(row);
        cell_of(batch.column(c), r)
    }

    /// A cell as text; null is `None`, and so is an empty string.
    pub fn text(&self, c: usize, row: usize) -> Option<String> {
        match self.cell(c, row) {
            Cell::Null => None,
            Cell::Str(s) if s.trim().is_empty() => None,
            Cell::Str(s) => Some(s.trim().to_string()),
            Cell::Int(i) => Some(i.to_string()),
            Cell::Float(x) => Some(x.to_string()),
            Cell::Bool(b) => Some(b.to_string()),
            Cell::Time(t) => Some(crate::kernel::time::format_timestamp(t)),
        }
    }

    /// A required text cell.
    pub fn string(&self, c: usize, row: usize) -> Result<String, String> {
        self.text(c, row).ok_or_else(|| format!("{}: empty `{}`", self.at(row), self.names[c]))
    }

    /// A number: a float or integer column, or text that parses.
    pub fn number(&self, c: usize, row: usize) -> Result<f64, String> {
        self.number_opt(c, row)?.ok_or_else(|| format!("{}: empty `{}`", self.at(row), self.names[c]))
    }

    pub fn number_opt(&self, c: usize, row: usize) -> Result<Option<f64>, String> {
        Ok(match self.cell(c, row) {
            Cell::Null => None,
            Cell::Float(x) => Some(x),
            Cell::Int(i) => Some(i as f64),
            Cell::Str(s) if s.trim().is_empty() => None,
            Cell::Str(s) => Some(s.trim().parse().map_err(|_| format!("{}: `{}` is not a number in `{}`", self.at(row), s, self.names[c]))?),
            other => return Err(format!("{}: {:?} is not a number in `{}`", self.at(row), other, self.names[c])),
        })
    }

    /// An integer: an integer column, or text that parses.
    pub fn integer(&self, c: usize, row: usize) -> Result<i64, String> {
        match self.cell(c, row) {
            Cell::Int(i) => Ok(i),
            Cell::Str(s) => s.trim().parse().map_err(|_| format!("{}: `{}` is not an integer in `{}`", self.at(row), s, self.names[c])),
            other => Err(format!("{}: {:?} is not an integer in `{}`", self.at(row), other, self.names[c])),
        }
    }

    /// A timestamp: a date or timestamp column, an Int64 of seconds, or
    /// text (`YYYY-MM-DD` or `YYYY-MM-DDTHH:MM:SS`); null or empty is `None`.
    pub fn time_opt(&self, c: usize, row: usize) -> Result<Option<i64>, String> {
        Ok(match self.cell(c, row) {
            Cell::Null => None,
            Cell::Time(t) | Cell::Int(t) => Some(t),
            Cell::Str(s) if s.trim().is_empty() => None,
            Cell::Str(s) => Some(parse_timestamp(s.trim()).ok_or_else(|| format!("{}: `{}` is not a timestamp in `{}`", self.at(row), s, self.names[c]))?),
            other => return Err(format!("{}: {:?} is not a timestamp in `{}`", self.at(row), other, self.names[c])),
        })
    }

    pub fn time(&self, c: usize, row: usize) -> Result<i64, String> {
        self.time_opt(c, row)?.ok_or_else(|| format!("{}: empty `{}`", self.at(row), self.names[c]))
    }
}

fn cell_of(col: &ArrayRef, r: usize) -> Cell<'_> {
    if col.is_null(r) {
        return Cell::Null;
    }
    macro_rules! get {
        ($t:ty) => {
            col.as_any().downcast_ref::<$t>().unwrap().value(r)
        };
    }
    match col.data_type() {
        DataType::Utf8 => Cell::Str(get!(StringArray)),
        DataType::LargeUtf8 => Cell::Str(get!(LargeStringArray)),
        DataType::Int8 => Cell::Int(get!(Int8Array) as i64),
        DataType::Int16 => Cell::Int(get!(Int16Array) as i64),
        DataType::Int32 => Cell::Int(get!(Int32Array) as i64),
        DataType::Int64 => Cell::Int(get!(Int64Array)),
        DataType::UInt8 => Cell::Int(get!(UInt8Array) as i64),
        DataType::UInt16 => Cell::Int(get!(UInt16Array) as i64),
        DataType::UInt32 => Cell::Int(get!(UInt32Array) as i64),
        DataType::UInt64 => Cell::Int(get!(UInt64Array) as i64),
        DataType::Float32 => Cell::Float(get!(Float32Array) as f64),
        DataType::Float64 => Cell::Float(get!(Float64Array)),
        DataType::Boolean => Cell::Bool(get!(BooleanArray)),
        DataType::Date32 => Cell::Time(get!(Date32Array) as i64 * DAY),
        DataType::Date64 => Cell::Time(get!(Date64Array).div_euclid(1000)),
        DataType::Timestamp(TimeUnit::Second, _) => Cell::Time(get!(TimestampSecondArray)),
        DataType::Timestamp(TimeUnit::Millisecond, _) => Cell::Time(get!(TimestampMillisecondArray).div_euclid(1_000)),
        DataType::Timestamp(TimeUnit::Microsecond, _) => Cell::Time(get!(TimestampMicrosecondArray).div_euclid(1_000_000)),
        DataType::Timestamp(TimeUnit::Nanosecond, _) => Cell::Time(get!(TimestampNanosecondArray).div_euclid(1_000_000_000)),
        _ => Cell::Null,
    }
}

/// A column to write.
pub enum Col {
    Str(Vec<Option<String>>),
    Int(Vec<Option<i64>>),
    Float(Vec<Option<f64>>),
    Bool(Vec<Option<bool>>),
    /// Seconds since 1970-01-01, written as a naive millisecond timestamp.
    Time(Vec<Option<i64>>),
}

impl Col {
    fn len(&self) -> usize {
        match self {
            Col::Str(v) => v.len(),
            Col::Int(v) => v.len(),
            Col::Float(v) => v.len(),
            Col::Bool(v) => v.len(),
            Col::Time(v) => v.len(),
        }
    }
}

/// Write named columns of equal length as one Parquet file.
pub fn write_table(path: &Path, cols: Vec<(&str, Col)>) -> Result<(), String> {
    let n = cols.first().map(|(_, c)| c.len()).unwrap_or(0);
    if let Some((name, _)) = cols.iter().find(|(_, c)| c.len() != n) {
        return Err(format!("{}: column `{}` has a different length", path.display(), name));
    }
    let mut fields = Vec::new();
    let mut arrays: Vec<ArrayRef> = Vec::new();
    for (name, col) in cols {
        let (ty, arr): (DataType, ArrayRef) = match col {
            Col::Str(v) => (DataType::Utf8, Arc::new(StringArray::from(v))),
            Col::Int(v) => (DataType::Int64, Arc::new(Int64Array::from(v))),
            Col::Float(v) => (DataType::Float64, Arc::new(Float64Array::from(v))),
            Col::Bool(v) => (DataType::Boolean, Arc::new(BooleanArray::from(v))),
            // Parquet has no second unit: milliseconds keep the logical type.
            Col::Time(v) => (
                DataType::Timestamp(TimeUnit::Millisecond, None),
                Arc::new(TimestampMillisecondArray::from(v.into_iter().map(|t| t.map(|t| t * 1000)).collect::<Vec<_>>())),
            ),
        };
        fields.push(Field::new(name, ty, true));
        arrays.push(arr);
    }
    let schema = Arc::new(Schema::new(fields));
    if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {}", dir.display(), e))?;
    }
    let file = File::create(path).map_err(|e| format!("{}: {}", path.display(), e))?;
    let mut writer = ArrowWriter::try_new(file, schema.clone(), None).map_err(|e| e.to_string())?;
    if n > 0 {
        let batch = RecordBatch::try_new(schema, arrays).map_err(|e| format!("{}: {}", path.display(), e))?;
        writer.write(&batch).map_err(|e| e.to_string())?;
    }
    writer.close().map_err(|e| e.to_string())?;
    Ok(())
}

/// Write a small table given as text, one row per line with comma-separated
/// fields and a header line, every column as text (empty fields are null).
/// For fixtures and hand-made inputs; readers parse text cells as they read
/// typed ones.
pub fn write_text_table(path: &Path, text: &str) -> Result<(), String> {
    let mut lines = text.lines().map(str::trim).filter(|l| !l.is_empty());
    let header: Vec<&str> = lines.next().map(|h| h.split(',').map(str::trim).collect()).unwrap_or_default();
    let mut cols: Vec<Vec<Option<String>>> = vec![Vec::new(); header.len()];
    for line in lines {
        let fields: Vec<&str> = line.split(',').map(str::trim).collect();
        for (i, col) in cols.iter_mut().enumerate() {
            col.push(fields.get(i).filter(|f| !f.is_empty()).map(|f| f.to_string()));
        }
    }
    write_table(path, header.into_iter().zip(cols).map(|(h, c)| (h, Col::Str(c))).collect())
}

/// A table rendered as text: a header line, then one line per row with
/// comma-joined cells (null as empty, timestamps as the loader reads them).
/// For messages, tests and eyeballing a file.
pub fn to_text(path: &Path) -> Result<String, String> {
    let t = Table::read(path)?;
    let mut out = t.names().join(",");
    out.push('\n');
    for r in 0..t.len() {
        let cells: Vec<String> = (0..t.names().len()).map(|c| t.text(c, r).unwrap_or_default()).collect();
        out.push_str(&cells.join(","));
        out.push('\n');
    }
    Ok(out)
}
