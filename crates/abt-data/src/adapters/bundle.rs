//! The `bundle_v2` adapter: a bundle of the first formulation
//! (`docs/data-bundle.md`, section 2: a manifest, a security table and
//! monthly Parquet partitions per relation) read into a `Market` under the
//! registry's schemas, so the books' bundles enter a store through the data
//! layer alone (M-2's gate: G1 on bundles imported through `bundle_v2`).
//! What it reads is what the kernel's `bundle::load_bundle` reads of the
//! same bundle, row for row (`crates/abt-tests/tests/synthetic.rs`).

use std::fs::File;
use std::path::Path;

use abt_wire::generated::abt::wire::{ColumnType, RelationSchema, Role};
use arrow::array::{Array, AsArray, RecordBatch};
use arrow::datatypes::{DataType, TimeUnit};
use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;
use serde::{Deserialize, Serialize};

use crate::adapter::Terms;
use crate::adapters::market::{schema_in, Market, V};
use crate::error::DataError;

/// The bundle layout this adapter reads (a text security table is version 1).
pub const FORMAT: u32 = 2;

fn format_one() -> u32 {
    1
}

/// A relation of the manifest.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct RelationEntry {
    pub name: String,
    /// The signature as declared, for the reader.
    pub signature: String,
    pub resolution: String,
    /// When a tuple is available: `bar_close`, or `recorded` with an
    /// `available_at` column after the relation's own.
    pub availability: String,
    pub rows: usize,
    /// Partition paths relative to the bundle directory.
    pub partitions: Vec<String>,
}

/// A bundle's manifest (`manifest.json`).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Manifest {
    #[serde(default = "format_one")]
    pub format: u32,
    /// The class the bundle instantiates.
    pub name: String,
    pub version: String,
    /// The bundle date (YYYY-MM-DD): what a ticker literal resolves at.
    pub as_of: Option<String>,
    pub processing_delay_seconds: i64,
    #[serde(default)]
    pub source: Option<String>,
    pub created_at: String,
    pub relations: Vec<RelationEntry>,
    /// The bundle tests' report when they passed; a bundle without one is
    /// unreviewed.
    pub tests: Option<serde_json::Value>,
}

pub fn read_manifest(dir: &Path) -> Result<Manifest, DataError> {
    let path = dir.join("manifest.json");
    let text = std::fs::read_to_string(&path).map_err(|e| DataError::Malformed(format!("{}: {}", path.display(), e)))?;
    serde_json::from_str(&text).map_err(|e| DataError::Malformed(format!("{}: {}", path.display(), e)))
}

const DAY: i64 = abt_wire::scalar::NANOS_PER_DAY;

fn floor_div(a: i64, b: i64) -> i64 {
    let q = a / b;
    if (a % b != 0) && ((a < 0) != (b < 0)) {
        q - 1
    } else {
        q
    }
}

/// The label of the bucket of a relation's calendar containing `t`, for a
/// relation keyed at or above a day: the civil date for `1d`, the week's
/// last day for `1w`; a finer calendar's keys are kept as written (the
/// kernel's `time::bucket` of a bundle's key column).
pub fn bucket(calendar: &str, t: i64) -> i64 {
    match calendar.trim_start_matches('@') {
        "1d" => floor_div(t, DAY) * DAY,
        "1w" => (floor_div(floor_div(t, DAY) + 3, 7) * 7 - 3 + 6) * DAY,
        _ => t,
    }
}

fn batches(path: &Path) -> Result<Vec<RecordBatch>, DataError> {
    let file = File::open(path).map_err(|e| DataError::Malformed(format!("{}: {}", path.display(), e)))?;
    let reader = ParquetRecordBatchReaderBuilder::try_new(file)
        .map_err(|e| DataError::Malformed(format!("{}: {}", path.display(), e)))?
        .build()?;
    reader.collect::<Result<Vec<_>, _>>().map_err(DataError::Arrow)
}

fn text_column(b: &RecordBatch, name: &str, path: &Path) -> Result<Vec<Option<String>>, DataError> {
    let c = b.column_by_name(name).ok_or_else(|| DataError::Malformed(format!("{}: no column `{}`", path.display(), name)))?;
    let c = arrow::compute::cast(c, &DataType::Utf8)?;
    let c = c.as_string::<i32>();
    Ok((0..c.len()).map(|i| (!c.is_null(i)).then(|| c.value(i).to_string())).collect())
}

fn time_column(b: &RecordBatch, name: &str, path: &Path) -> Result<Vec<Option<i64>>, DataError> {
    let c = b.column_by_name(name).ok_or_else(|| DataError::Malformed(format!("{}: no column `{}`", path.display(), name)))?;
    let c = match c.data_type() {
        DataType::Utf8 | DataType::LargeUtf8 => {
            let s = arrow::compute::cast(c, &DataType::Utf8)?;
            let s = s.as_string::<i32>();
            let parsed: Vec<Option<i64>> = (0..s.len())
                .map(|i| {
                    if s.is_null(i) || s.value(i).is_empty() {
                        Ok(None)
                    } else {
                        abt_wire::scalar::parse_timestamp(s.value(i))
                            .map(Some)
                            .ok_or_else(|| DataError::Malformed(format!("{}: `{}` is not a time", path.display(), s.value(i))))
                    }
                })
                .collect::<Result<_, _>>()?;
            return Ok(parsed);
        }
        _ => arrow::compute::cast(c, &DataType::Timestamp(TimeUnit::Nanosecond, None))?,
    };
    let c = c.as_primitive::<arrow::datatypes::TimestampNanosecondType>();
    Ok((0..c.len()).map(|i| (!c.is_null(i)).then(|| c.value(i))).collect())
}

fn float_column(b: &RecordBatch, name: &str) -> Result<Option<Vec<Option<f64>>>, DataError> {
    let Some(c) = b.column_by_name(name) else { return Ok(None) };
    let c = arrow::compute::cast(c, &DataType::Float64)?;
    let c = c.as_primitive::<arrow::datatypes::Float64Type>();
    Ok(Some((0..c.len()).map(|i| (!c.is_null(i)).then(|| c.value(i))).collect()))
}

/// The security table (`securities.parquet`: `id`, `ticker`, `from`, `to`,
/// and the contract columns `multiplier`, `asset_class`,
/// `commission_per_contract` when the bundle declares contracts).
fn read_securities(m: &mut Market, path: &Path) -> Result<(), DataError> {
    for b in batches(path)? {
        let ids = text_column(&b, "id", path)?;
        let tickers = text_column(&b, "ticker", path)?;
        let from = time_column(&b, "from", path)?;
        let to = time_column(&b, "to", path)?;
        let mult = float_column(&b, "multiplier")?;
        let comm = float_column(&b, "commission_per_contract")?;
        let class = b.column_by_name("asset_class").map(|_| text_column(&b, "asset_class", path)).transpose()?;
        for i in 0..b.num_rows() {
            let (Some(id), Some(ticker)) = (ids[i].as_deref().filter(|s| !s.is_empty()), tickers[i].as_deref().filter(|s| !s.is_empty())) else {
                return Err(DataError::Identity(format!("{}: id and ticker must be non-empty (row {})", path.display(), i)));
            };
            let Some(from) = from[i] else {
                return Err(DataError::Identity(format!("{}: `from` of {} is empty", path.display(), id)));
            };
            m.add_security(id, ticker, from, to[i]);
            let multiplier = mult.as_ref().map(|c| c[i].unwrap_or(1.0));
            let future = class.as_ref().map(|c| c[i].as_deref() == Some("future")).unwrap_or(false);
            let commission = comm.as_ref().and_then(|c| c[i]);
            // A declared contract: any contract column that differs from a
            // share's defaults.
            if multiplier.map(|x| x != 1.0).unwrap_or(false) || future || commission.is_some() {
                m.terms.push(Terms {
                    key: id.to_string(),
                    class: if future { "future" } else { "equity" }.to_string(),
                    multiplier: Some(multiplier.unwrap_or(1.0)),
                    commission_per_contract: commission,
                });
            }
        }
    }
    Ok(())
}

/// Reads a bundle into a market of `class` under the registry's schemas:
/// the manifest must name the class; a relation the class does not have
/// is skipped; a relation's key column is bucketed to its calendar's
/// label; a relation recorded with availability carries it. The market is
/// unreviewed unless the bundle's tests passed, and carries the bundle's
/// version; the program's `require` and `prefer` are the resolver's to
/// judge (K-2.1, D-7.2).
pub fn load(dir: &Path, class: &str, schemas: &[RelationSchema]) -> Result<(Market, Manifest), DataError> {
    let m = read_manifest(dir)?;
    if m.name != class {
        return Err(DataError::Malformed(format!(
            "bundle {} is `{}@{}`, but the class asked for is `{}`",
            dir.display(),
            m.name,
            m.version,
            class
        )));
    }
    if m.format < FORMAT {
        return Err(DataError::Malformed(format!(
            "bundle {} is layout version {} (text security table); this abt reads version {}: an older abt reads it",
            dir.display(),
            m.format,
            FORMAT
        )));
    }
    let mut market = Market::default();
    let table = dir.join("securities.parquet");
    if table.exists() {
        read_securities(&mut market, &table)?;
    }
    for entry in &m.relations {
        let Some(schema) = schema_in(schemas, class, &entry.name) else { continue };
        let cols: Vec<_> = schema.columns.iter().filter(|c| c.role != Role::ReceiptTime).collect();
        let key_pos = cols.iter().position(|c| c.role == Role::EventTime).unwrap_or(0);
        let records = entry.availability == "recorded";
        for part in &entry.partitions {
            let path = dir.join(part);
            for batch in batches(&path)? {
                let expected = cols.len() + usize::from(records);
                if batch.num_columns() != expected {
                    return Err(DataError::Schema(format!(
                        "{}: {} columns for the {}-column relation `{}`",
                        path.display(),
                        batch.num_columns(),
                        expected,
                        entry.name
                    )));
                }
                let avail = records.then(|| ints(&batch, cols.len(), &path)).transpose()?;
                let columns: Vec<Vec<V>> = cols
                    .iter()
                    .enumerate()
                    .map(|(i, c)| -> Result<Vec<V>, DataError> {
                        Ok(match c.ty {
                            ColumnType::Entity => texts(&batch, i, &path)?.into_iter().map(V::E).collect(),
                            ColumnType::Label => texts(&batch, i, &path)?.into_iter().map(V::L).collect(),
                            ColumnType::Time => ints(&batch, i, &path)?
                                .into_iter()
                                .map(|t| V::T(if i == key_pos { bucket(schema.calendar.as_deref().unwrap_or(""), t) } else { t }))
                                .collect(),
                            ColumnType::Count => ints(&batch, i, &path)?.into_iter().map(V::C).collect(),
                            _ => floats(&batch, i, &path)?.into_iter().map(V::N).collect(),
                        })
                    })
                    .collect::<Result<_, _>>()?;
                for row in 0..batch.num_rows() {
                    let tuple: Vec<V> = columns.iter().map(|c| c[row].clone()).collect();
                    match &avail {
                        Some(a) => market.add_available(&entry.name, tuple, a[row]),
                        None => market.add(&entry.name, tuple),
                    }
                }
            }
        }
    }
    if schema_in(schemas, class, "ticker").is_some() {
        market.derive_tickers();
    }
    market.unreviewed = m.tests.is_none();
    market.version = Some(m.version.clone());
    Ok((market, m))
}

fn texts(b: &RecordBatch, i: usize, path: &Path) -> Result<Vec<String>, DataError> {
    let c = arrow::compute::cast(b.column(i), &DataType::Utf8).map_err(|_| DataError::Schema(format!("{}: column {} is not text", path.display(), i)))?;
    let c = c.as_string::<i32>();
    Ok((0..c.len()).map(|r| c.value(r).to_string()).collect())
}

fn ints(b: &RecordBatch, i: usize, path: &Path) -> Result<Vec<i64>, DataError> {
    let c = b.column(i);
    let c = match c.data_type() {
        DataType::Timestamp(_, _) => arrow::compute::cast(c, &DataType::Timestamp(TimeUnit::Nanosecond, None))
            .and_then(|c| arrow::compute::cast(&c, &DataType::Int64))
            .map_err(|_| DataError::Schema(format!("{}: column {} is not a time", path.display(), i)))?,
        _ => arrow::compute::cast(c, &DataType::Int64).map_err(|_| DataError::Schema(format!("{}: column {} is not Int64", path.display(), i)))?,
    };
    Ok(c.as_primitive::<arrow::datatypes::Int64Type>().values().to_vec())
}

fn floats(b: &RecordBatch, i: usize, path: &Path) -> Result<Vec<f64>, DataError> {
    let c = b.column(i);
    if c.data_type() != &DataType::Float64 {
        return Err(DataError::Schema(format!("{}: column {} is not Float64", path.display(), i)));
    }
    Ok(c.as_primitive::<arrow::datatypes::Float64Type>().values().to_vec())
}
