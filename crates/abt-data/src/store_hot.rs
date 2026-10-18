//! The store's hot write path (ST-7, S1 to S5) and the reads it answers
//! from a hot version: a child of `store`, so it reaches the store's
//! fields.

use std::collections::BTreeMap;
use std::sync::Arc;

use abt_wire::generated::abt::wire::RelationSchema;
use arrow::array::{ArrayRef, BooleanArray, Float64Array, Int64Array, RecordBatch, TimestampNanosecondArray, UInt32Array, UInt64Array};
use arrow::datatypes::{DataType, SchemaRef};

use super::{Manifest, PartitionRef, Store};
use crate::error::DataError;
use crate::hot::manifest::Kind;
use crate::hot::HotVersion;
use crate::registry::VersionRecord;
use crate::rows::{layout, stored_schema, Layout};
use crate::staging::{self, build, classify, normalise};

impl Store {
    /// The hot version `version` names, opened once.
    pub fn hot(&self, version: &str) -> Result<Arc<HotVersion>, DataError> {
        if let Some(v) = self.hot.lock().ok().and_then(|c| c.get(version).cloned()) {
            return Ok(v);
        }
        let rec = self.registry.version(version).ok_or_else(|| DataError::UnknownVersion(version.to_string()))?;
        let v = Arc::new(HotVersion::open(&self.root, &rec.relation, version, false)?);
        if let Ok(mut c) = self.hot.lock() {
            c.insert(version.to_string(), v.clone());
        }
        Ok(v)
    }

    /// Whether a version is held hot.
    pub fn is_hot(&self, version: &str) -> bool {
        self.manifest(version).map(|m| m.hot).unwrap_or(false)
    }

    /// S1 to S5 for one write of a hot relation: the rows conformed and
    /// numbered provisionally; returns the version and the sequence range
    /// of the rows logged.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn write_hot(
        &mut self,
        schema: &RelationSchema,
        stored: &SchemaRef,
        all: &RecordBatch,
        source: &str,
        mut attributes: BTreeMap<String, String>,
        first_seq: u64,
        previous: Option<String>,
        fresh: bool,
    ) -> Result<(String, (u64, u64)), DataError> {
        let t0 = std::time::Instant::now();
        let relation = schema.name.clone();
        let lay = layout(schema);
        let prev = match &previous {
            Some(v) if !fresh => {
                let m = self.manifest(v)?;
                if m.schema.columns != schema.columns {
                    None
                } else if m.hot {
                    Some(self.hot(v)?)
                } else {
                    return Err(DataError::Malformed(format!(
                        "`{}` is held in Parquet by `{}`; a relation does not switch to the hot store mid-history (ingest it into a new store or under another source)",
                        relation, source
                    )));
                }
            }
            _ => None,
        };
        let f32 = attributes.get("floats").is_some_and(|f| f == "f32");
        let normalised = normalise::normalise(schema, &lay, all, f32)?;
        let t1 = t0.elapsed();
        let (planned, counts, next_seq) = classify::classify(prev.as_deref(), &normalised, first_seq);
        let t2 = t0.elapsed();
        if planned.is_empty() {
            return Err(DataError::Malformed(format!("`{}`: a hot version holds at least one row", relation)));
        }
        let seq_high = next_seq.saturating_sub(1).max(prev.as_ref().map(|p| p.manifest.seq_high).unwrap_or(0));
        let value_names: Vec<String> = normalised.value_cols.iter().map(|&c| schema.columns[c].name.clone()).collect();
        let built = build::build(
            &self.root,
            &relation,
            source,
            schema,
            schema.calendar.as_deref(),
            &normalised.kinds,
            &value_names,
            &planned,
            prev.as_deref(),
            seq_high,
            &attributes,
        )?;
        let t3 = t0.elapsed();
        // S4 over the opened version: random ranges against a direct
        // reduction, the presence against the rows.
        let mut verified = built.verified.clone();
        {
            let v = HotVersion::open_manifest_at(&self.root, &built.staging_dir, built.manifest.clone(), true)?;
            build::verify_ranges(&v, &planned, 8, &mut verified)?;
        }
        let t4 = t0.elapsed();
        build::publish(&self.root, &built)?;
        let version = built.manifest.version.clone();
        // The log: the new rows in sequence order.
        let logged = build::logged(&planned, first_seq);
        let last_seq = logged.last().map(|(s, _, _)| *s).unwrap_or(first_seq.saturating_sub(1));
        if !logged.is_empty() {
            let batch = log_batch(schema, &lay, stored, &normalised.kinds, &logged)?;
            self.append_log(&relation, source, stored, &batch, first_seq, last_seq)?;
        }
        // The version's manifest in the store's form: buckets as partitions.
        let m = &built.manifest;
        let partitions: Vec<PartitionRef> = m
            .buckets
            .iter()
            .map(|b| PartitionRef {
                bucket: b.bucket.clone(),
                file: format!("hot:{}", b.bucket),
                hash: String::new(),
                rows: b.rows,
                t_min: b.t_min,
                t_max: b.t_max,
                r_min: b.r_min,
                r_max: b.r_max,
                seq_max: b.seq_max,
            })
            .collect();
        self.ordinal += 1;
        attributes.insert("_ordinal".to_string(), self.ordinal.to_string());
        let manifest = Manifest {
            relation: relation.clone(),
            version: version.clone(),
            source: source.to_string(),
            schema: schema.clone(),
            partitions,
            rows: m.rows,
            from: m.from,
            to: m.to,
            attributes: attributes.clone(),
            hot: true,
        };
        let mpath = self.root.join("manifest").join(&relation).join(format!("{}.json", version));
        std::fs::create_dir_all(mpath.parent().expect("a manifest sits in a directory"))?;
        std::fs::write(&mpath, serde_json::to_vec_pretty(&manifest)?)?;
        self.registry.record_version(VersionRecord {
            relation: relation.clone(),
            version: version.clone(),
            source: source.to_string(),
            rows: m.rows,
            from: m.from,
            to: m.to,
            attributes,
            log_prefix: None,
        });
        self.registry.set_current(&relation, source, &version);
        self.registry.save()?;
        self.next_seq.insert((relation.clone(), source.to_string()), next_seq);
        let t5 = t0.elapsed();
        staging::record(staging::Report {
            relation,
            version: version.clone(),
            counts,
            measure: built.measure.clone(),
            verified,
            millis: [
                t1.as_millis() as u64,
                (t2 - t1).as_millis() as u64,
                (t3 - t2).as_millis() as u64,
                (t4 - t3).as_millis() as u64,
                (t5 - t4).as_millis() as u64,
            ],
        });
        Ok((version, (first_seq, last_seq)))
    }

    /// A hot version's rows the filter admits, in (t, identity, r) order.
    pub(super) fn read_hot(&self, m: &Manifest, f: &super::RowFilter) -> Result<Vec<RecordBatch>, DataError> {
        let v = self.hot(&m.version)?;
        let rows = v.read(f, 0, u32::MAX)?;
        // As the Parquet store answers: no batch for no rows, except that
        // `latest_only` always gives its one batch.
        if rows.is_empty() && !f.latest_only {
            return Ok(Vec::new());
        }
        Ok(vec![v.to_batch(&rows, &stored_schema(&m.schema), &layout(&m.schema), None)?])
    }

    pub(super) fn read_hot_newest(&self, m: &Manifest, f: &super::RowFilter) -> Result<Vec<RecordBatch>, DataError> {
        let v = self.hot(&m.version)?;
        let rows = v.read_newest(f)?;
        if rows.is_empty() {
            return Ok(Vec::new());
        }
        Ok(vec![v.to_batch(&rows, &stored_schema(&m.schema), &layout(&m.schema), None)?])
    }

    pub(super) fn read_hot_bucket(&self, m: &Manifest, p: &PartitionRef, f: &super::RowFilter) -> Result<RecordBatch, DataError> {
        let v = self.hot(&m.version)?;
        let stored = stored_schema(&m.schema);
        let b = v
            .manifest
            .buckets
            .iter()
            .find(|b| b.bucket == p.bucket)
            .ok_or_else(|| DataError::NotFound(format!("`{}` has no bucket `{}`", m.relation, p.bucket)))?;
        // A partition's rows are every revision the filter admits, as the
        // Parquet store's are (the stream merges revisions at their
        // receipts, D-5.3); `latest_only` is the whole read's.
        let every = super::RowFilter { latest_only: false, ..f.clone() };
        let rows = v.read(&every, b.lo, b.hi)?;
        v.to_batch(&rows, &stored, &layout(&m.schema), None)
    }

    /// A hot version's rows with its tick columns as `Decimal128(18,
    /// scale)`, `scale` the finest exponent among the columns asked for
    /// (ST-10.4), in canonical order.
    pub fn read_ticks(&self, version: &str, f: &super::RowFilter) -> Result<(u32, Vec<RecordBatch>), DataError> {
        let m = self.manifest(version)?;
        if !m.hot {
            return Err(DataError::Malformed(format!("`{}` is not held hot: it has no tick columns to read (A-D44)", m.relation)));
        }
        let v = self.hot(version)?;
        let scale = v
            .manifest
            .columns
            .iter()
            .filter(|c| matches!(c.kind, Kind::Tick | Kind::TickF32))
            .map(|c| c.exponent as u32)
            .max()
            .unwrap_or(0);
        let rows = v.read(f, 0, u32::MAX)?;
        if rows.is_empty() {
            return Ok((scale, Vec::new()));
        }
        Ok((scale, vec![v.to_batch(&rows, &stored_schema(&m.schema), &layout(&m.schema), Some(scale))?]))
    }
}

/// The logged rows as a batch in the stored layout.
fn log_batch(s: &RelationSchema, lay: &Layout, stored: &SchemaRef, kinds: &[Kind], rows: &[(u64, u64, normalise::Row)]) -> Result<RecordBatch, DataError> {
    let mut cols: Vec<ArrayRef> = Vec::with_capacity(s.columns.len() + 1);
    let mut vc = 0usize;
    for (i, c) in s.columns.iter().enumerate() {
        let col: ArrayRef = if i == lay.t {
            Arc::new(TimestampNanosecondArray::from(rows.iter().map(|(_, _, r)| r.t).collect::<Vec<_>>()).with_timezone("UTC"))
        } else if Some(i) == lay.r {
            Arc::new(TimestampNanosecondArray::from(rows.iter().map(|(_, _, r)| r.r).collect::<Vec<_>>()).with_timezone("UTC"))
        } else if lay.identity.contains(&i) {
            Arc::new(UInt64Array::from(rows.iter().map(|(_, id, _)| *id).collect::<Vec<_>>()))
        } else {
            let k = kinds[vc];
            let bits: Vec<u64> = rows.iter().map(|(_, _, r)| r.vals[vc]).collect();
            vc += 1;
            match (k, stored.field(i).data_type()) {
                (Kind::Tick | Kind::TickF32, _) => Arc::new(Float64Array::from(bits.iter().map(|&b| f64::from_bits(b)).collect::<Vec<_>>())),
                (_, DataType::Int64) => Arc::new(Int64Array::from(bits.iter().map(|&b| b as i64).collect::<Vec<_>>())),
                (_, DataType::UInt32) => Arc::new(UInt32Array::from(bits.iter().map(|&b| b as u32).collect::<Vec<_>>())),
                (_, DataType::Boolean) => Arc::new(BooleanArray::from(bits.iter().map(|&b| b != 0).collect::<Vec<_>>())),
                (_, DataType::Timestamp(_, _)) => Arc::new(TimestampNanosecondArray::from(bits.iter().map(|&b| b as i64).collect::<Vec<_>>()).with_timezone("UTC")),
                (_, ty) => return Err(DataError::Schema(format!("`{}`: column `{}` of type {:?} is not logged from the hot path", s.name, c.name, ty))),
            }
        };
        cols.push(col);
    }
    cols.push(Arc::new(UInt64Array::from(rows.iter().map(|(s, _, _)| *s).collect::<Vec<_>>())));
    Ok(RecordBatch::try_new(stored.clone(), cols)?)
}
