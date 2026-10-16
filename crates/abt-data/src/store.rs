//! The storage engine (D-3): per relation and source an append-only log of
//! Arrow IPC segments with a sequence number per row (the source of truth),
//! and immutable Parquet partitions, one per bucket of the partition
//! calendar, sorted by (t, identity, r), with zone maps (row-group
//! statistics) and an entity offset index; a version is the manifest of its
//! partitions, content-addressed (D-2.6). A historical source's buckets are
//! closed as soon as they are written, so its rows are sealed into the
//! relation's next version on ingest (D-3.3); unchanged buckets keep their
//! files, which versions share.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use abt_wire::canonical::Canonical;
use abt_wire::generated::abt::wire::{ColumnType, RelationSchema, Role};
use abt_wire::hash::Hash;
use abt_wire::scalar::{civil_from_days, NANOS_PER_DAY, NANOS_PER_HOUR};
use arrow::array::{AsArray, BooleanArray, RecordBatch};
use arrow::datatypes::SchemaRef;
use arrow::ipc::writer::StreamWriter;
use parquet::arrow::arrow_reader::{ParquetRecordBatchReaderBuilder, RowSelection, RowSelector};
use parquet::arrow::ArrowWriter;
use parquet::basic::{Compression, Encoding, ZstdLevel};
use parquet::file::properties::{EnabledStatistics, WriterProperties};
use parquet::schema::types::ColumnPath;
use serde::{Deserialize, Serialize};

use crate::error::DataError;
use crate::registry::{Registry, VersionRecord};
use crate::rows::{self, layout, stored_schema, Layout};

/// Rows per Parquet row group (X-2.1).
const ROW_GROUP: usize = 65_536;

/// A partition of a version: one bucket's rows.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PartitionRef {
    pub bucket: String,
    /// The file, relative to the store's root.
    pub file: String,
    /// BLAKE3 of the file's bytes.
    pub hash: String,
    pub rows: u64,
    pub t_min: i64,
    pub t_max: i64,
    pub r_min: i64,
    pub r_max: i64,
    pub seq_max: u64,
}

/// A version's manifest (D-3.1): its partitions, schema and attributes.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Manifest {
    pub relation: String,
    pub version: String,
    pub source: String,
    pub schema: RelationSchema,
    pub partitions: Vec<PartitionRef>,
    pub rows: u64,
    pub from: Option<i64>,
    pub to: Option<i64>,
    pub attributes: BTreeMap<String, String>,
}

/// What a read keeps (D-4): a time range, identities, the receipt filter
/// `r ≤ r_max`, the snapshot `seq ≤ as_of_seq`, and whether only the latest
/// admitted revision of each (identity, t) is wanted.
#[derive(Clone, Debug, Default)]
pub struct RowFilter {
    pub t0: Option<i64>,
    pub t1: Option<i64>,
    pub identity: Option<Vec<u64>>,
    pub r_max: Option<i64>,
    pub as_of_seq: Option<u64>,
    pub latest_only: bool,
}

/// The granularity of a relation's partitions (D-3.1; ERRATA E-15).
fn bucket_of(calendar: Option<&str>, t: i64) -> String {
    let days = t.div_euclid(NANOS_PER_DAY);
    let (y, m, d) = civil_from_days(days);
    match calendar {
        Some("1d") | Some("1w") => format!("{:04}-{:02}", y, m),
        Some(c) if c.ends_with('m') || c.ends_with('h') => format!("{:04}-{:02}-{:02}", y, m, d),
        Some(c) if c.ends_with('s') => {
            let hour = t.rem_euclid(NANOS_PER_DAY) / NANOS_PER_HOUR;
            format!("{:04}-{:02}-{:02}T{:02}", y, m, d, hour)
        }
        _ => format!("{:04}", y),
    }
}

/// The store.
pub struct Store {
    root: PathBuf,
    pub registry: Registry,
    next_seq: BTreeMap<(String, String), u64>,
    ordinal: u64,
    /// A temporary store's directory is removed when the store is dropped.
    temporary: bool,
}

impl Drop for Store {
    fn drop(&mut self) {
        if self.temporary {
            let _ = fs::remove_dir_all(&self.root);
        }
    }
}

impl Store {
    /// Opens (or creates) the store at `root`.
    pub fn open(root: &Path) -> Result<Store, DataError> {
        fs::create_dir_all(root)?;
        let registry = Registry::open(&root.join("registry"))?;
        let ordinal = registry.versions().filter_map(|v| v.attributes.get("_ordinal").and_then(|o| o.parse::<u64>().ok())).max().unwrap_or(0);
        Ok(Store {
            root: root.to_path_buf(),
            registry,
            next_seq: BTreeMap::new(),
            ordinal,
            temporary: false,
        })
    }

    /// A fresh store in a new directory under the system's temporary
    /// directory (what tests and in-process runs without a store use),
    /// removed when the store is dropped.
    pub fn temporary(tag: &str) -> Result<Store, DataError> {
        static COUNT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let n = COUNT.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let dir = std::env::temp_dir().join(format!("abt-store-{}-{}-{}", tag, std::process::id(), n));
        if dir.exists() {
            fs::remove_dir_all(&dir)?;
        }
        let mut s = Store::open(&dir)?;
        s.temporary = true;
        Ok(s)
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// A relation's latest schema.
    pub fn schema(&self, relation: &str) -> Result<RelationSchema, DataError> {
        self.registry.schema(relation).ok_or_else(|| DataError::UnknownRelation(relation.to_string()))
    }

    // ------------------------------------------------------------ writing

    /// A historical source's rows of a registered relation: logged, then
    /// sealed into the relation's next version from that source, which
    /// becomes current (D-2.6, D-3.3). Returns the version.
    pub fn ingest(&mut self, relation: &str, input: &[RecordBatch], source: &str, attributes: BTreeMap<String, String>) -> Result<String, DataError> {
        let schema = self.schema(relation)?;
        let (version, _) = self.write(&schema, input, source, attributes, false)?;
        Ok(version)
    }

    /// Another layer's rows under its namespace (D-3.6): the schema is the
    /// writer's and is registered; returns the version and the rows'
    /// sequence numbers. The rows are added to the relation's current
    /// version (a row already there must be the same row).
    pub fn put(&mut self, schema: &RelationSchema, input: &[RecordBatch], source: &str) -> Result<(String, (u64, u64)), DataError> {
        self.registry.register_schema(schema)?;
        self.write(schema, input, source, BTreeMap::new(), false)
    }

    /// A `put` whose rows are the relation's next version whole: the
    /// relation starts over from them, as when a writer's rows change shape
    /// (a writer's summary written again, a cache object written again
    /// under its key); its earlier versions stay readable by id (E-56).
    pub fn replace(&mut self, schema: &RelationSchema, input: &[RecordBatch], source: &str) -> Result<(String, (u64, u64)), DataError> {
        self.registry.register_schema(schema)?;
        self.write(schema, input, source, BTreeMap::new(), true)
    }

    fn write(&mut self, schema: &RelationSchema, input: &[RecordBatch], source: &str, mut attributes: BTreeMap<String, String>, fresh: bool) -> Result<(String, (u64, u64)), DataError> {
        let relation = schema.name.clone();
        let key = (relation.clone(), source.to_string());
        let first_seq = match self.next_seq.get(&key) {
            Some(&s) => s,
            None => self.scan_log(&relation, source)?,
        };
        // Conform every input batch; the sequence numbers follow the input.
        let mut conformed = Vec::new();
        let mut seq = first_seq;
        for b in input {
            let c = rows::conform(schema, b, &mut self.registry, seq)?;
            seq += c.num_rows() as u64;
            conformed.push(c);
        }
        let stored = stored_schema(schema);
        let all = rows::concat(&stored, &conformed)?;
        let last_seq = seq.saturating_sub(1);
        // The next version, computed and validated before anything is written.
        let previous = self.registry.current(&relation, Some(source)).map(|v| v.to_string());
        // A writer whose rows changed shape, or that replaces them, starts
        // the relation over: the new version holds this write's rows only
        // (its earlier versions stay readable by id).
        let prev_manifest = match &previous {
            Some(v) if !fresh => Some(self.manifest(v)?).filter(|m| m.schema.columns == schema.columns),
            _ => None,
        };
        let lay = layout(schema);
        let mut by_bucket: BTreeMap<String, Vec<u32>> = BTreeMap::new();
        {
            let ts = rows::times(&all, lay.t);
            for i in 0..all.num_rows() {
                by_bucket.entry(bucket_of(schema.calendar.as_deref(), ts.value(i))).or_default().push(i as u32);
            }
        }
        let mut partitions: BTreeMap<String, PartitionRef> = prev_manifest.as_ref().map(|m| m.partitions.iter().map(|p| (p.bucket.clone(), p.clone())).collect()).unwrap_or_default();
        let mut files: Vec<EncodedPartition> = Vec::new();
        for (bucket, idx) in &by_bucket {
            let new_rows = arrow::compute::take_record_batch(&all, &arrow::array::UInt32Array::from(idx.clone()))?;
            let mut parts = vec![new_rows];
            if let Some(p) = partitions.get(bucket) {
                parts.extend(self.read_partition(&stored, p, None)?);
            }
            let merged = rows::concat(&stored, &parts)?;
            let sorted = rows::dedupe(schema, &rows::sort_rows(schema, &merged)?)?;
            let (part, bytes, index) = encode_partition(schema, &relation, bucket, &sorted, &lay)?;
            partitions.insert(bucket.clone(), part.clone());
            files.push((part, bytes, index));
        }
        let parts: Vec<PartitionRef> = partitions.into_values().collect();
        let rows_total: u64 = parts.iter().map(|p| p.rows).sum();
        let from = parts.iter().map(|p| p.t_min).min();
        let to = parts.iter().map(|p| p.t_max).max();
        let identity = serde_json::json!({
            "relation": relation,
            "schema": schema,
            "partitions": parts.iter().map(|p| (p.bucket.clone(), p.hash.clone())).collect::<Vec<_>>(),
            "attributes": attributes,
        });
        let version = identity.canonical_hash().map_err(|e| DataError::Malformed(e.to_string()))?.hex();
        // Write: the log segment, the partitions, the manifest, the registry.
        if all.num_rows() > 0 {
            self.append_log(&relation, source, &stored, &all, first_seq, last_seq)?;
        }
        for (p, bytes, index) in files {
            let path = self.root.join(&p.file);
            if !path.exists() {
                fs::create_dir_all(path.parent().expect("a partition sits in a directory"))?;
                fs::write(&path, &bytes)?;
                if let Some(ix) = index {
                    fs::write(path.with_extension("idx"), ix)?;
                }
            }
        }
        self.ordinal += 1;
        attributes.insert("_ordinal".to_string(), self.ordinal.to_string());
        let manifest = Manifest {
            relation: relation.clone(),
            version: version.clone(),
            source: source.to_string(),
            schema: schema.clone(),
            partitions: parts,
            rows: rows_total,
            from,
            to,
            attributes: attributes.clone(),
        };
        let mpath = self.root.join("manifest").join(&relation).join(format!("{}.json", version));
        fs::create_dir_all(mpath.parent().expect("a manifest sits in a directory"))?;
        fs::write(&mpath, serde_json::to_vec_pretty(&manifest)?)?;
        self.registry.record_version(VersionRecord {
            relation: relation.clone(),
            version: version.clone(),
            source: source.to_string(),
            rows: rows_total,
            from,
            to,
            attributes,
            log_prefix: None,
        });
        self.registry.set_current(&relation, source, &version);
        self.registry.save()?;
        self.next_seq.insert(key, last_seq + 1);
        Ok((version, (first_seq, last_seq)))
    }

    /// The next sequence number of a log, from its segments' names.
    fn scan_log(&mut self, relation: &str, source: &str) -> Result<u64, DataError> {
        let dir = self.root.join("log").join(relation).join(source);
        let mut next = 1;
        if dir.exists() {
            for e in fs::read_dir(&dir)? {
                let name = e?.file_name().to_string_lossy().to_string();
                if let Some((_, last)) = name.trim_end_matches(".arrows").split_once('-') {
                    if let Ok(l) = last.parse::<u64>() {
                        next = next.max(l + 1);
                    }
                }
            }
        }
        self.next_seq.insert((relation.to_string(), source.to_string()), next);
        Ok(next)
    }

    /// Appends a segment to a log (D-3.1): the rows in acceptance order.
    fn append_log(&self, relation: &str, source: &str, schema: &SchemaRef, rows: &RecordBatch, first: u64, last: u64) -> Result<(), DataError> {
        let dir = self.root.join("log").join(relation).join(source);
        fs::create_dir_all(&dir)?;
        let path = dir.join(format!("{:020}-{:020}.arrows", first, last));
        let tmp = path.with_extension("arrows.tmp");
        {
            let f = fs::File::create(&tmp)?;
            let mut w = StreamWriter::try_new(std::io::BufWriter::new(f), schema)?;
            w.write(rows)?;
            w.finish()?;
        }
        fs::rename(&tmp, &path)?;
        Ok(())
    }

    /// A log's rows with seq in `from..=to`, in acceptance order (D-5.4),
    /// refusing a gap or a reversal in the sequence (D-8.3).
    pub fn read_log(&self, relation: &str, source: &str, from: u64, to: u64) -> Result<Vec<RecordBatch>, DataError> {
        let dir = self.root.join("log").join(relation).join(source);
        let mut segments: Vec<(u64, u64, PathBuf)> = Vec::new();
        if dir.exists() {
            for e in fs::read_dir(&dir)? {
                let p = e?.path();
                let name = p.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
                if let Some((a, b)) = name.strip_suffix(".arrows").and_then(|n| n.split_once('-')) {
                    if let (Ok(a), Ok(b)) = (a.parse::<u64>(), b.parse::<u64>()) {
                        segments.push((a, b, p));
                    }
                }
            }
        }
        segments.sort();
        let mut out = Vec::new();
        let mut expect = segments.first().map(|s| s.0).unwrap_or(1);
        for (a, b, p) in segments {
            if a != expect {
                return Err(DataError::Order(format!("log {}/{}: sequence {} follows {} (a gap in the log)", relation, source, a, expect - 1)));
            }
            expect = b + 1;
            if b < from || a > to {
                continue;
            }
            let reader = arrow::ipc::reader::StreamReader::try_new(std::io::BufReader::new(fs::File::open(&p)?), None)?;
            for batch in reader {
                let batch = batch?;
                let seq_col = batch.num_columns() - 1;
                let seqs = rows::seqs(&batch, seq_col);
                let keep: BooleanArray = seqs.iter().map(|s| Some(s.is_some_and(|s| s >= from && s <= to))).collect();
                out.push(arrow::compute::filter_record_batch(&batch, &keep)?);
            }
        }
        Ok(out)
    }

    // ------------------------------------------------------------ reading

    pub fn manifest(&self, version: &str) -> Result<Manifest, DataError> {
        let rec = self.registry.version(version).ok_or_else(|| DataError::UnknownVersion(version.to_string()))?;
        let path = self.root.join("manifest").join(&rec.relation).join(format!("{}.json", version));
        Ok(serde_json::from_slice(&fs::read(&path)?)?)
    }

    /// One partition's rows, those of `identity` only when given (through
    /// the entity offset index).
    fn read_partition(&self, stored: &SchemaRef, p: &PartitionRef, identity: Option<&[u64]>) -> Result<Vec<RecordBatch>, DataError> {
        let path = self.root.join(&p.file);
        let file = fs::File::open(&path)?;
        let mut builder = ParquetRecordBatchReaderBuilder::try_new(file)?;
        if let Some(ids) = identity {
            let ix = path.with_extension("idx");
            if ix.exists() {
                let wanted = read_index_rows(&fs::read(&ix)?, ids)?;
                builder = builder.with_row_selection(selection(&wanted, p.rows as usize));
            }
        }
        let reader = builder.with_batch_size(65_536).build()?;
        let mut out = Vec::new();
        for b in reader {
            let b = b?;
            // Partitions keep the stored layout; align the schema object.
            out.push(RecordBatch::try_new(stored.clone(), b.columns().to_vec())?);
        }
        Ok(out)
    }

    /// The rows of a version the filter admits, in (t, identity, r) order.
    pub fn read(&self, version: &str, f: &RowFilter) -> Result<Vec<RecordBatch>, DataError> {
        let m = self.manifest(version)?;
        let stored = stored_schema(&m.schema);
        let lay = layout(&m.schema);
        let mut out = Vec::new();
        for p in &m.partitions {
            if f.t0.is_some_and(|t0| p.t_max < t0) || f.t1.is_some_and(|t1| p.t_min > t1) {
                continue;
            }
            for b in self.read_partition(&stored, p, f.identity.as_deref())? {
                let kept = filter_rows(&m.schema, &lay, &b, f)?;
                if kept.num_rows() > 0 {
                    out.push(kept);
                }
            }
        }
        if f.latest_only {
            let all = rows::concat(&stored, &out)?;
            return Ok(vec![latest_revisions(&lay, &all)?]);
        }
        Ok(out)
    }

    /// The latest admitted revisions in the newest bucket at or before `t1`
    /// that holds a row the filter admits. This is `latest` for an identity
    /// that fixes the fact's key: no older bucket can hold a later fact of it.
    pub fn read_newest(&self, version: &str, f: &RowFilter) -> Result<Vec<RecordBatch>, DataError> {
        let m = self.manifest(version)?;
        let stored = stored_schema(&m.schema);
        let lay = layout(&m.schema);
        let mut buckets: BTreeMap<&str, Vec<&PartitionRef>> = BTreeMap::new();
        for p in &m.partitions {
            if f.t1.is_none_or(|t1| p.t_min <= t1) {
                buckets.entry(&p.bucket).or_default().push(p);
            }
        }
        let mut newest_first: Vec<Vec<&PartitionRef>> = buckets.into_values().collect();
        newest_first.sort_by_key(|ps| std::cmp::Reverse(ps.iter().map(|p| p.t_min).min()));
        for ps in newest_first {
            let mut out = Vec::new();
            for p in &ps {
                for b in self.read_partition(&stored, p, f.identity.as_deref())? {
                    let kept = filter_rows(&m.schema, &lay, &b, f)?;
                    if kept.num_rows() > 0 {
                        out.push(kept);
                    }
                }
            }
            if !out.is_empty() {
                let mut all = rows::concat(&stored, &out)?;
                if ps.len() > 1 {
                    all = rows::sort_rows(&m.schema, &all)?;
                }
                return Ok(vec![latest_revisions(&lay, &all)?]);
            }
        }
        Ok(Vec::new())
    }

    /// One partition's rows the filter admits, in the partition's order
    /// (what the stream opens partition by partition, E-14).
    pub fn read_partition_rows(&self, m: &Manifest, p: &PartitionRef, f: &RowFilter) -> Result<RecordBatch, DataError> {
        let stored = stored_schema(&m.schema);
        let lay = layout(&m.schema);
        let mut out = Vec::new();
        for b in self.read_partition(&stored, p, f.identity.as_deref())? {
            out.push(filter_rows(&m.schema, &lay, &b, f)?);
        }
        rows::concat(&stored, &out)
    }

    /// Every row of a version as one batch.
    pub fn read_all(&self, version: &str) -> Result<RecordBatch, DataError> {
        let m = self.manifest(version)?;
        let parts = self.read(version, &RowFilter::default())?;
        rows::concat(&stored_schema(&m.schema), &parts)
    }

    // ------------------------------------------------------ cache and pins

    /// The version a cache key names (D-3.5).
    pub fn cache_get(&self, key: &Hash) -> Option<String> {
        self.registry.cache_lookup(&key.hex()).map(|(_, v)| v.clone())
    }

    /// Stores a cache object under its key.
    pub fn cache_put(&mut self, key: &Hash, schema: &RelationSchema, rows: &[RecordBatch]) -> Result<String, DataError> {
        let mut schema = schema.clone();
        schema.name = format!("cache.{}", &key.hex()[..16]);
        schema.class = "cache".to_string();
        let (version, _) = self.replace(&schema, rows, "cache")?;
        self.registry.cache_record(&key.hex(), &schema.name, &version);
        self.registry.save()?;
        Ok(version)
    }

    pub fn pin(&mut self, version: &str, by: &str) -> Result<(), DataError> {
        self.registry.version(version).ok_or_else(|| DataError::UnknownVersion(version.to_string()))?;
        self.registry.pin(version, by);
        self.registry.save()
    }

    pub fn unpin(&mut self, version: &str, by: &str) -> Result<(), DataError> {
        self.registry.unpin(version, by);
        self.registry.save()
    }

    /// Evicts versions that are neither current nor pinned, oldest first,
    /// until the partitions on disk fit `disk_budget` bytes (D-3.4); files no
    /// remaining version references are removed. Returns the evicted versions.
    pub fn gc(&mut self, disk_budget: u64) -> Result<Vec<String>, DataError> {
        let current: BTreeSet<String> = self.registry.currents().into_iter().map(|(_, _, v)| v).collect();
        let mut candidates: Vec<(u64, String)> = self
            .registry
            .versions()
            .filter(|v| !current.contains(&v.version) && !self.registry.pinned(&v.version))
            .map(|v| (v.attributes.get("_ordinal").and_then(|o| o.parse().ok()).unwrap_or(0), v.version.clone()))
            .collect();
        candidates.sort();
        let mut evicted = Vec::new();
        for (_, v) in candidates {
            if self.disk_bytes()? <= disk_budget {
                break;
            }
            let m = self.manifest(&v)?;
            fs::remove_file(self.root.join("manifest").join(&m.relation).join(format!("{}.json", v)))?;
            self.registry.drop_version(&v);
            self.remove_unreferenced(&m.relation)?;
            evicted.push(v);
        }
        self.registry.save()?;
        Ok(evicted)
    }

    fn remove_unreferenced(&self, relation: &str) -> Result<(), DataError> {
        let mut used: BTreeSet<String> = BTreeSet::new();
        for v in self.registry.versions().filter(|v| v.relation == relation) {
            for p in self.manifest(&v.version)?.partitions {
                used.insert(p.file);
            }
        }
        let dir = self.root.join("part").join(relation);
        if dir.exists() {
            for e in fs::read_dir(&dir)? {
                let p = e?.path();
                if p.extension().is_some_and(|x| x == "parquet") {
                    let rel = p.strip_prefix(&self.root).map(|r| r.to_string_lossy().to_string()).unwrap_or_default();
                    if !used.contains(&rel) {
                        fs::remove_file(&p)?;
                        let _ = fs::remove_file(p.with_extension("idx"));
                    }
                }
            }
        }
        Ok(())
    }

    /// Bytes of every partition file on disk.
    pub fn disk_bytes(&self) -> Result<u64, DataError> {
        fn walk(d: &Path, total: &mut u64) -> std::io::Result<()> {
            if d.exists() {
                for e in fs::read_dir(d)? {
                    let p = e?.path();
                    if p.is_dir() {
                        walk(&p, total)?;
                    } else {
                        *total += p.metadata()?.len();
                    }
                }
            }
            Ok(())
        }
        let mut total = 0;
        walk(&self.root.join("part"), &mut total)?;
        Ok(total)
    }
}

/// A filter's rows of one batch.
fn filter_rows(s: &RelationSchema, lay: &Layout, b: &RecordBatch, f: &RowFilter) -> Result<RecordBatch, DataError> {
    let ts = rows::times(b, lay.t);
    let rs = lay.r.map(|r| rows::times(b, r));
    let seqs = rows::seqs(b, lay.seq);
    let ids: Option<&arrow::array::UInt64Array> = match lay.identity.first() {
        Some(&i) if lay.identity.len() == 1 && s.columns[i].ty == ColumnType::Entity => Some(b.column(i).as_primitive::<arrow::datatypes::UInt64Type>()),
        _ => None,
    };
    let labels: Option<&arrow::array::UInt32Array> = match lay.identity.first() {
        Some(&i) if lay.identity.len() == 1 && s.columns[i].ty == ColumnType::Label => Some(b.column(i).as_primitive::<arrow::datatypes::UInt32Type>()),
        _ => None,
    };
    let want: Option<BTreeSet<u64>> = f.identity.as_ref().map(|v| v.iter().copied().collect());
    let keep: BooleanArray = (0..b.num_rows())
        .map(|i| {
            let t = ts.value(i);
            let mut ok = f.t0.is_none_or(|t0| t >= t0) && f.t1.is_none_or(|t1| t <= t1);
            if let (Some(rmax), Some(rs)) = (f.r_max, rs) {
                ok &= rs.value(i) <= rmax;
            } else if let Some(rmax) = f.r_max {
                ok &= t <= rmax;
            }
            ok &= f.as_of_seq.is_none_or(|n| seqs.value(i) <= n);
            if let Some(w) = &want {
                let id = ids.map(|a| a.value(i)).or_else(|| labels.map(|a| a.value(i) as u64));
                ok &= id.is_some_and(|id| w.contains(&id));
            }
            Some(ok)
        })
        .collect();
    Ok(arrow::compute::filter_record_batch(b, &keep)?)
}

/// Sorted rows with only the last revision of each (t, key) kept: the
/// latest admitted one (D-2.2).
fn latest_revisions(lay: &Layout, sorted: &RecordBatch) -> Result<RecordBatch, DataError> {
    if sorted.num_rows() < 2 {
        return Ok(sorted.clone());
    }
    let cols: Vec<usize> = std::iter::once(lay.t).chain(lay.key.iter().copied()).collect();
    let fields: Vec<arrow::row::SortField> = cols.iter().map(|&i| arrow::row::SortField::new(sorted.column(i).data_type().clone())).collect();
    let conv = arrow::row::RowConverter::new(fields)?;
    let arrays: Vec<arrow::array::ArrayRef> = cols.iter().map(|&i| sorted.column(i).clone()).collect();
    let keys = conv.convert_columns(&arrays)?;
    let n = sorted.num_rows();
    let keep: BooleanArray = (0..n).map(|i| Some(i + 1 == n || keys.row(i) != keys.row(i + 1))).collect();
    Ok(arrow::compute::filter_record_batch(sorted, &keep)?)
}

/// A partition, its file's bytes and its entity offset index.
type EncodedPartition = (PartitionRef, Vec<u8>, Option<Vec<u8>>);

/// A partition's file: Parquet with zstd level 3, delta-encoded times,
/// byte-stream-split floats and dictionary-encoded entities (D-3.2), rows in
/// row groups of 65,536, and the entity offset index beside it.
fn encode_partition(s: &RelationSchema, relation: &str, bucket: &str, sorted: &RecordBatch, lay: &Layout) -> Result<EncodedPartition, DataError> {
    let mut props = WriterProperties::builder()
        .set_compression(Compression::ZSTD(ZstdLevel::try_new(3)?))
        .set_max_row_group_row_count(Some(ROW_GROUP))
        .set_statistics_enabled(EnabledStatistics::Chunk)
        .set_created_by("abt-data".to_string());
    for c in &s.columns {
        let path = ColumnPath::from(c.name.as_str());
        match c.ty {
            ColumnType::Time => props = props.set_column_dictionary_enabled(path.clone(), false).set_column_encoding(path, Encoding::DELTA_BINARY_PACKED),
            ColumnType::Num => props = props.set_column_dictionary_enabled(path.clone(), false).set_column_encoding(path, Encoding::BYTE_STREAM_SPLIT),
            _ => {}
        }
    }
    let seq = ColumnPath::from(rows::SEQ);
    props = props.set_column_dictionary_enabled(seq.clone(), false).set_column_encoding(seq, Encoding::DELTA_BINARY_PACKED);
    let mut bytes = Vec::new();
    {
        let mut w = ArrowWriter::try_new(&mut bytes, sorted.schema(), Some(props.build()))?;
        w.write(sorted)?;
        w.close()?;
    }
    let hash = Hash::of(&bytes).hex();
    let ts = rows::times(sorted, lay.t);
    let rs = lay.r.map(|r| rows::times(sorted, r));
    let seqs = rows::seqs(sorted, lay.seq);
    let n = sorted.num_rows();
    let (t_min, t_max) = (0..n).fold((i64::MAX, i64::MIN), |(lo, hi), i| (lo.min(ts.value(i)), hi.max(ts.value(i))));
    let (r_min, r_max) = match rs {
        Some(rs) => (0..n).fold((i64::MAX, i64::MIN), |(lo, hi), i| (lo.min(rs.value(i)), hi.max(rs.value(i)))),
        None => (t_min, t_max),
    };
    let seq_max = (0..n).map(|i| seqs.value(i)).max().unwrap_or(0);
    let index = match lay.identity.as_slice() {
        [i] => Some(encode_index(sorted, *i, s.columns[*i].ty)),
        _ => None,
    };
    let file = format!("part/{}/{}.{}.parquet", relation, bucket, &hash[..16]);
    let part = PartitionRef {
        bucket: bucket.to_string(),
        file,
        hash,
        rows: n as u64,
        t_min,
        t_max,
        r_min,
        r_max,
        seq_max,
    };
    Ok((part, bytes, index))
}

const INDEX_MAGIC: &[u8; 8] = b"ABTIDX1\0";

/// The entity offset index of a partition (D-3.1): every identity value's
/// row offsets, as compressed sparse rows: `n`, the sorted keys, `n + 1`
/// offsets into the row list, the rows (all little-endian).
fn encode_index(sorted: &RecordBatch, col: usize, ty: ColumnType) -> Vec<u8> {
    let mut by: BTreeMap<u64, Vec<u32>> = BTreeMap::new();
    let c = sorted.column(col);
    for i in 0..sorted.num_rows() {
        let key = match ty {
            ColumnType::Entity => c.as_primitive::<arrow::datatypes::UInt64Type>().value(i),
            _ => c.as_primitive::<arrow::datatypes::UInt32Type>().value(i) as u64,
        };
        by.entry(key).or_default().push(i as u32);
    }
    let mut out = Vec::with_capacity(16 + by.len() * 12 + sorted.num_rows() * 4);
    out.extend_from_slice(INDEX_MAGIC);
    out.extend_from_slice(&(by.len() as u32).to_le_bytes());
    for k in by.keys() {
        out.extend_from_slice(&k.to_le_bytes());
    }
    let mut offset = 0u32;
    out.extend_from_slice(&offset.to_le_bytes());
    for v in by.values() {
        offset += v.len() as u32;
        out.extend_from_slice(&offset.to_le_bytes());
    }
    for v in by.values() {
        for r in v {
            out.extend_from_slice(&r.to_le_bytes());
        }
    }
    out
}

/// The rows of `ids` in a partition, from its index, ascending.
fn read_index_rows(ix: &[u8], ids: &[u64]) -> Result<Vec<u32>, DataError> {
    let bad = || DataError::Order("a corrupt entity offset index".into());
    if ix.len() < 12 || &ix[..8] != INDEX_MAGIC {
        return Err(bad());
    }
    let n = u32::from_le_bytes(ix[8..12].try_into().map_err(|_| bad())?) as usize;
    let keys_at = 12;
    let offsets_at = keys_at + 8 * n;
    let rows_at = offsets_at + 4 * (n + 1);
    if ix.len() < rows_at {
        return Err(bad());
    }
    let key = |i: usize| u64::from_le_bytes(ix[keys_at + 8 * i..keys_at + 8 * i + 8].try_into().expect("eight bytes"));
    let off = |i: usize| u32::from_le_bytes(ix[offsets_at + 4 * i..offsets_at + 4 * i + 4].try_into().expect("four bytes")) as usize;
    let mut out = Vec::new();
    for id in ids {
        // Binary search over the sorted keys.
        let (mut lo, mut hi) = (0usize, n);
        while lo < hi {
            let mid = (lo + hi) / 2;
            if key(mid) < *id {
                lo = mid + 1;
            } else {
                hi = mid;
            }
        }
        if lo < n && key(lo) == *id {
            for j in off(lo)..off(lo + 1) {
                let at = rows_at + 4 * j;
                out.push(u32::from_le_bytes(ix[at..at + 4].try_into().map_err(|_| bad())?));
            }
        }
    }
    out.sort_unstable();
    out.dedup();
    Ok(out)
}

/// A row selection of the given rows out of `total`.
fn selection(rows: &[u32], total: usize) -> RowSelection {
    let mut sel = Vec::new();
    let mut at = 0usize;
    for &r in rows {
        let r = r as usize;
        if r > at {
            sel.push(RowSelector::skip(r - at));
        }
        sel.push(RowSelector::select(1));
        at = r + 1;
    }
    if total > at {
        sel.push(RowSelector::skip(total - at));
    }
    RowSelection::from(sel)
}

/// The stored rows' event times at a position (for callers that hold a batch).
pub fn row_time(b: &RecordBatch, col: usize, i: usize) -> i64 {
    rows::times(b, col).value(i)
}

/// Whether a schema's identity is one entity column.
pub fn entity_keyed(s: &RelationSchema) -> bool {
    let ids: Vec<_> = s.columns.iter().filter(|c| c.role == Role::Entity).collect();
    ids.len() == 1 && ids[0].ty == ColumnType::Entity
}

/// The stored layout of a schema, shared for callers.
pub fn stored(s: &RelationSchema) -> SchemaRef {
    Arc::clone(&stored_schema(s))
}
