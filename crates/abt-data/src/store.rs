//! The storage engine (D-3): per relation and source an append-only log of
//! Arrow IPC segments with a sequence number per row (the source of truth),
//! and immutable Parquet partitions, one per bucket of the partition
//! calendar, sorted by (t, identity, r), with zone maps (row-group
//! statistics) and an entity offset index; a version is the manifest of its
//! partitions, content-addressed (D-2.6). A historical source's buckets are
//! closed as soon as they are written, so its rows are sealed into the
//! relation's next version on ingest (D-3.3); unchanged buckets keep their
//! files, which versions share.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

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

#[path = "store_hot.rs"]
mod store_hot;
pub use store_hot::HotFigures;

use crate::error::DataError;
use crate::hot::residency::Residency;
use crate::hot::HotVersion;
use crate::registry::{Registry, VersionRecord};
use crate::resident::{self, GroupKey, Resident, ResidentStats};
use crate::rows::{self, layout, stored_schema, Layout};

/// Rows per Parquet row group (X-2.1).
const ROW_GROUP: usize = 65_536;
/// Append files beyond which a relation is consolidated (ST-8.7).
pub const CONSOLIDATE_BYTES: u64 = 512 << 20;

/// The partition files and the manifests a store keeps open (E-110): past
/// these, the maps start over. Every run a service writes adds a version
/// of each of its outputs and of each aggregate's cache object, read once;
/// kept for good, they held 69,000 index maps in a day's service.
const FILES_KEPT: usize = 8192;
const MANIFESTS_KEPT: usize = 4096;

/// The families of relations the layers derive rather than ingest (E-110):
/// a run's outputs, the query layer's records, the aggregates' cache
/// objects. They are no run's inputs, and their Parquet partitions are
/// their only copy: no log segment is written for them (an aggregate's
/// cache object logged some 30 MB of rows no reader asks of a log).
pub const DERIVED: [&str; 3] = ["kernel.", "query.", "cache."];

/// Whether a relation is one the layers derive (`DERIVED`).
pub fn derived(relation: &str) -> bool {
    DERIVED.iter().any(|d| relation.starts_with(d))
}

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
    /// Held in the hot store (07-storage.md): the partitions are the
    /// buckets of its labels, read from its blocks.
    #[serde(default)]
    pub hot: bool,
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

/// A bucket of a relation's partitions (D-3.1; ERRATA E-15): its name and
/// the instants it spans, `[from, to)`: a row whose
/// time is in the span is in the bucket, so a run of rows in time order
/// names its bucket once.
pub(crate) fn bucket_span(calendar: Option<&str>, t: i64) -> (String, i64, i64) {
    let days = t.div_euclid(NANOS_PER_DAY);
    let (y, m, d) = civil_from_days(days);
    let day = |y: i64, m: u32, d: u32| abt_wire::scalar::days_from_civil(y, m, d) * NANOS_PER_DAY;
    let next_month = |y: i64, m: u32| if m == 12 { (y + 1, 1) } else { (y, m + 1) };
    match calendar {
        Some("1d") | Some("1w") => {
            let (ny, nm) = next_month(y, m);
            (format!("{:04}-{:02}", y, m), day(y, m, 1), day(ny, nm, 1))
        }
        Some(c) if c.ends_with('m') || c.ends_with('h') => (format!("{:04}-{:02}-{:02}", y, m, d), days * NANOS_PER_DAY, (days + 1) * NANOS_PER_DAY),
        Some(c) if c.ends_with('s') => {
            let hour = t.rem_euclid(NANOS_PER_DAY) / NANOS_PER_HOUR;
            let from = days * NANOS_PER_DAY + hour * NANOS_PER_HOUR;
            (format!("{:04}-{:02}-{:02}T{:02}", y, m, d, hour), from, from + NANOS_PER_HOUR)
        }
        _ => (format!("{:04}", y), day(y, 1, 1), day(y + 1, 1, 1)),
    }
}

/// Rows to write: a writer's, conformed to the schema on the way in, or
/// rows read from the store, conformed already.
enum Rows<'a> {
    Input(&'a [RecordBatch]),
    Stored(&'a [RecordBatch]),
}

/// The store.
pub struct Store {
    root: PathBuf,
    pub registry: Registry,
    next_seq: BTreeMap<(String, String), u64>,
    ordinal: u64,
    /// A temporary store's directory is removed when the store is dropped.
    temporary: bool,
    /// Decoded row groups resident under the budget (D-3.2, ERRATA E-74).
    resident: Resident,
    /// Each partition file's row groups and its entity offset index,
    /// memory-mapped: read once per file while kept (`FILES_KEPT`).
    files: Mutex<HashMap<PathBuf, Arc<FileInfo>>>,
    /// The manifests read, by version.
    manifests: Mutex<HashMap<String, Arc<Manifest>>>,
    /// The hot versions opened, by version.
    hot: Mutex<HashMap<String, Arc<HotVersion>>>,
    /// What is done with a hot version's segments at open (ST-9.1).
    residency: Residency,
    /// Append files beyond which an append consolidates its relation.
    consolidate_bytes: u64,
}

/// A partition file's row groups (their rows, in order) and its entity
/// offset index.
struct FileInfo {
    groups: Vec<usize>,
    index: Option<memmap2::Mmap>,
}

/// Removes the temporary stores (`abt-store-<tag>-<pid>-<n>`) of processes
/// that are no longer running.
pub fn reap_temporary() {
    let Ok(entries) = fs::read_dir(std::env::temp_dir()) else { return };
    for e in entries.flatten() {
        let name = e.file_name();
        let Some(rest) = name.to_str().and_then(|n| n.strip_prefix("abt-store-")) else { continue };
        let mut fields = rest.rsplitn(3, '-');
        let (_, pid) = (fields.next(), fields.next());
        let Some(pid) = pid.and_then(|p| p.parse::<u32>().ok()) else { continue };
        if pid != std::process::id() && !process_alive(pid) {
            let _ = fs::remove_dir_all(e.path());
        }
    }
}

/// Staging work a process that is no longer running left in flight
/// (`hot/<relation>/.staging-<pid>-<seq>`, S0 to S4 of ST-8.8): discarded;
/// the ingest that made it reruns.
fn reap_staging(root: &Path) -> Result<(), DataError> {
    let hot = root.join("hot");
    if !hot.exists() {
        return Ok(());
    }
    for rel in fs::read_dir(&hot)? {
        let rel = rel?.path();
        if !rel.is_dir() || rel.file_name().is_some_and(|n| n == "segments") {
            continue;
        }
        for e in fs::read_dir(&rel)? {
            let p = e?.path();
            let name = p.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
            let pid = name.strip_prefix(".staging-").and_then(|r| r.split('-').next()).and_then(|p| p.parse::<u32>().ok());
            if let Some(pid) = pid {
                if pid != std::process::id() && !process_alive(pid) {
                    fs::remove_dir_all(&p)?;
                }
            }
        }
    }
    Ok(())
}

/// Whether a process is running: Linux's `/proc`, else `kill -0` on a Unix;
/// elsewhere every process counts as running, so nothing is reaped.
fn process_alive(pid: u32) -> bool {
    if cfg!(target_os = "linux") {
        Path::new(&format!("/proc/{}", pid)).exists()
    } else if cfg!(unix) {
        std::process::Command::new("kill")
            .args(["-0", &pid.to_string()])
            .stderr(std::process::Stdio::null())
            .status()
            .map_or(true, |s| s.success())
    } else {
        true
    }
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
        reap_staging(root)?;
        let registry = Registry::open(&root.join("registry"))?;
        let ordinal = registry.versions().filter_map(|v| v.attributes.get("_ordinal").and_then(|o| o.parse::<u64>().ok())).max().unwrap_or(0);
        Ok(Store {
            root: root.to_path_buf(),
            registry,
            next_seq: BTreeMap::new(),
            ordinal,
            temporary: false,
            resident: Resident::new(resident::default_budget()),
            files: Mutex::new(HashMap::new()),
            manifests: Mutex::new(HashMap::new()),
            hot: Mutex::new(HashMap::new()),
            residency: Residency::off(),
            consolidate_bytes: CONSOLIDATE_BYTES,
        })
    }

    /// Sets what happens to a hot version's segments when it is opened
    /// (ST-9.1): a served store prefaults and locks them; what is open
    /// already is prefaulted now.
    pub fn set_residency(&mut self, residency: Residency) {
        self.residency = residency;
        if let Ok(h) = self.hot.lock() {
            for v in h.values() {
                self.residency.apply(v);
            }
        }
    }

    pub fn residency(&self) -> &Residency {
        &self.residency
    }

    /// Sets the append bytes beyond which an append consolidates its
    /// relation (ST-8.7; `CONSOLIDATE_BYTES` by default).
    pub fn set_consolidate_bytes(&mut self, bytes: u64) {
        self.consolidate_bytes = bytes;
    }

    /// Sets the resident budget in bytes (0 decodes every read from its
    /// file), dropping what is resident.
    pub fn set_resident_budget(&mut self, bytes: u64) {
        self.resident = Resident::new(bytes);
    }

    /// What the resident cache has done.
    pub fn resident_stats(&self) -> ResidentStats {
        self.resident.stats()
    }

    /// A fresh store in a new directory under the system's temporary
    /// directory (what tests and in-process runs without a store use),
    /// removed when the store is dropped. A process that ends while it still
    /// holds one (a service it never dropped) leaves the directory behind:
    /// the first temporary store of a process removes those of processes
    /// that are no longer running.
    pub fn temporary(tag: &str) -> Result<Store, DataError> {
        static COUNT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        static REAP: std::sync::Once = std::sync::Once::new();
        REAP.call_once(reap_temporary);
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
        let (version, _) = self.write_rows(&schema, Rows::Input(input), source, attributes, false)?;
        Ok(version)
    }

    /// Rows read from the store's own versions (`read`'s batches), sealed
    /// whole as the relation's next version from `source`: what the
    /// resolver writes when a relation's span is covered by several
    /// versions (D-7.5). The rows keep their identities and receipt times
    /// and are numbered anew in the source's log.
    pub fn ingest_stored(&mut self, relation: &str, rows: &[RecordBatch], source: &str, attributes: BTreeMap<String, String>) -> Result<String, DataError> {
        let schema = self.schema(relation)?;
        let (version, _) = self.write_rows(&schema, Rows::Stored(rows), source, attributes, true)?;
        Ok(version)
    }

    /// Another layer's rows under its namespace (D-3.6): the schema is the
    /// writer's and is registered; returns the version and the rows'
    /// sequence numbers. The rows are added to the relation's current
    /// version (a row already there must be the same row).
    pub fn put(&mut self, schema: &RelationSchema, input: &[RecordBatch], source: &str) -> Result<(String, (u64, u64)), DataError> {
        self.registry.register_schema(schema)?;
        self.write_rows(schema, Rows::Input(input), source, BTreeMap::new(), false)
    }

    /// A `put` whose rows are the relation's next version whole: the
    /// relation starts over from them, as when a writer's rows change shape
    /// (a writer's summary written again, a cache object written again
    /// under its key); its earlier versions stay readable by id (E-56).
    pub fn replace(&mut self, schema: &RelationSchema, input: &[RecordBatch], source: &str) -> Result<(String, (u64, u64)), DataError> {
        self.registry.register_schema(schema)?;
        self.write_rows(schema, Rows::Input(input), source, BTreeMap::new(), true)
    }

    fn write_rows(&mut self, schema: &RelationSchema, input: Rows<'_>, source: &str, mut attributes: BTreeMap<String, String>, fresh: bool) -> Result<(String, (u64, u64)), DataError> {
        let relation = schema.name.clone();
        let key = (relation.clone(), source.to_string());
        let first_seq = match self.next_seq.get(&key) {
            Some(&s) => s,
            None => self.scan_log(&relation, source)?,
        };
        // Conform every input batch (rows of the store's are conformed
        // already); the sequence numbers follow the input.
        let mut conformed = Vec::new();
        let mut seq = first_seq;
        match input {
            Rows::Input(input) => {
                for b in input {
                    let c = rows::conform(schema, b, &mut self.registry, seq)?;
                    seq += c.num_rows() as u64;
                    conformed.push(c);
                }
            }
            Rows::Stored(input) => {
                let stored = stored_schema(schema);
                for b in input {
                    let n = b.num_rows() as u64;
                    let mut cols: Vec<arrow::array::ArrayRef> = b.columns().to_vec();
                    let last = cols.len().checked_sub(1).ok_or_else(|| DataError::Schema(format!("`{}`: a stored batch without its seq", relation)))?;
                    cols[last] = std::sync::Arc::new(arrow::array::UInt64Array::from((seq..seq + n).collect::<Vec<_>>()));
                    conformed.push(RecordBatch::try_new(stored.clone(), cols)?);
                    seq += n;
                }
            }
        }
        let stored = stored_schema(schema);
        let all = rows::concat(&stored, &conformed)?;
        let last_seq = seq.saturating_sub(1);
        // The next version, computed and validated before anything is written.
        let previous = self.registry.current(&relation, Some(source)).map(|v| v.to_string());
        // A writer whose rows changed shape, or that replaces them, starts
        // the relation over: the new version holds this write's rows only
        // (its earlier versions stay readable by id).
        // A relation a source holds hot (07-storage.md): staged through
        // S1 to S5 into blocks instead of Parquet partitions. Rows the
        // blocks cannot hold (none, or a null value) stay in Parquet, and
        // the version says so.
        // A relation a source holds hot stays hot: a write without the
        // attribute follows the current version (the live path and the
        // daily update, ST-8.5, ST-8.6).
        let held_hot = !fresh && previous.as_ref().is_some_and(|v| self.manifest(v).map(|m| m.hot).unwrap_or(false));
        match attributes.get("storage").map(String::as_str) {
            Some("hot") => {}
            None if held_hot => {
                attributes.insert("storage".to_string(), "hot".to_string());
            }
            Some(other) if held_hot => {
                return Err(DataError::Malformed(format!(
                    "`{}` is held hot by `{}`; a write with storage = {} does not take it back to Parquet mid-history (ingest it under another source)",
                    relation, source, other
                )));
            }
            _ => {}
        }
        if attributes.get("storage").is_some_and(|v| v == "hot") {
            let lay = layout(schema);
            let rows_qualify = crate::hot::version::qualifies_rows(&all, &lay) || (held_hot && all.num_rows() == 0);
            if crate::hot::version::qualifies(schema, &lay) && rows_qualify {
                return self.write_hot(schema, &stored, &all, source, attributes, first_seq, previous, fresh);
            }
            if held_hot {
                return Err(DataError::Malformed(format!(
                    "`{}` is held hot by `{}`; these rows cannot be held hot (a null, or a value off every grid) and the relation does not go back to Parquet mid-history",
                    relation, source
                )));
            }
            attributes.insert("storage".to_string(), "parquet".to_string());
        }
        let prev_manifest = match &previous {
            Some(v) if !fresh => Some(self.manifest(v)?).filter(|m| m.schema.columns == schema.columns),
            _ => None,
        };
        let lay = layout(schema);
        let mut by_bucket: BTreeMap<String, Vec<u32>> = BTreeMap::new();
        {
            let ts = rows::times(&all, lay.t);
            let mut span: Option<(String, i64, i64)> = None;
            for i in 0..all.num_rows() {
                let t = ts.value(i);
                if !span.as_ref().is_some_and(|(_, from, to)| *from <= t && t < *to) {
                    span = Some(bucket_span(schema.calendar.as_deref(), t));
                }
                let (bucket, _, _) = span.as_ref().expect("a span for every row");
                match by_bucket.get_mut(bucket) {
                    Some(rows) => rows.push(i as u32),
                    None => {
                        by_bucket.insert(bucket.clone(), vec![i as u32]);
                    }
                }
            }
        }
        let mut partitions: BTreeMap<String, PartitionRef> = prev_manifest.as_ref().map(|m| m.partitions.iter().map(|p| (p.bucket.clone(), p.clone())).collect()).unwrap_or_default();
        let mut files: Vec<EncodedPartition> = Vec::new();
        for (bucket, idx) in &by_bucket {
            let new_rows = arrow::compute::take_record_batch(&all, &arrow::array::UInt32Array::from(idx.clone()))?;
            let mut parts = vec![new_rows];
            if let Some(p) = partitions.get(bucket) {
                parts.extend(self.read_partition_file(&stored, p, None)?);
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
        // Write: the log segment (an ingested relation's), the partitions,
        // the manifest, the registry.
        if all.num_rows() > 0 && !derived(&relation) {
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
            hot: false,
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
            // The segment's group commit (D-3.1, ST-8.8): durable before
            // its name says it is in the log.
            w.into_inner()?.into_inner().map_err(|e| e.into_error())?.sync_all()?;
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
        // A version is immutable (D-2.6): its manifest is read once. A hot
        // version's appends open buckets beyond its published ones, which
        // its partitions gain (ST-8).
        if let Some(m) = self.manifests.lock().ok().and_then(|c| c.get(version).cloned()) {
            return Ok(self.with_appends((*m).clone()));
        }
        let path = self.root.join("manifest").join(&rec.relation).join(format!("{}.json", version));
        let m: Manifest = serde_json::from_slice(&fs::read(&path)?)?;
        if let Ok(mut c) = self.manifests.lock() {
            if c.len() >= MANIFESTS_KEPT {
                c.clear();
            }
            c.insert(version.to_string(), Arc::new(m.clone()));
        }
        Ok(self.with_appends(m))
    }

    fn with_appends(&self, mut m: Manifest) -> Manifest {
        if m.hot {
            self.append_partitions(&mut m);
        }
        m
    }

    /// A partition's rows, `identity`'s only when its index names them:
    /// through the resident cache, its row groups decoded on first touch and
    /// an entity's rows gathered from them in the file's order (E-74).
    fn read_partition(&self, stored: &SchemaRef, p: &PartitionRef, identity: Option<&[u64]>) -> Result<Vec<RecordBatch>, DataError> {
        if self.resident.budget() == 0 {
            return self.read_partition_file(stored, p, identity);
        }
        let path = self.root.join(&p.file);
        let info = self.file_info(&path)?;
        let wanted = match (identity, &info.index) {
            (Some(ids), Some(ix)) => Some(read_index_rows(&ix[..], ids)?),
            _ => None,
        };
        let mut out = Vec::new();
        let mut start = 0usize;
        for (g, &n) in info.groups.iter().enumerate() {
            let end = start + n;
            let rows: Option<Vec<u32>> = wanted.as_ref().map(|w| {
                let (lo, hi) = (w.partition_point(|&r| (r as usize) < start), w.partition_point(|&r| (r as usize) < end));
                w[lo..hi].iter().map(|r| r - start as u32).collect()
            });
            if rows.as_ref().is_some_and(|r| r.is_empty()) {
                start = end;
                continue;
            }
            let key = GroupKey { file: path.clone(), group: g };
            let group = self.resident.group(key, || decode_group(&path, g))?;
            let b = match rows {
                Some(rows) if rows.len() < n => arrow::compute::take_record_batch(&group, &arrow::array::UInt32Array::from(rows))?,
                _ => (*group).clone(),
            };
            // Partitions keep the stored layout; align the schema object.
            out.push(RecordBatch::try_new(stored.clone(), b.columns().to_vec())?);
            start = end;
        }
        Ok(out)
    }

    /// A partition file's row groups and its index, read once.
    fn file_info(&self, path: &Path) -> Result<Arc<FileInfo>, DataError> {
        if let Some(info) = self.files.lock().ok().and_then(|f| f.get(path).cloned()) {
            return Ok(info);
        }
        let builder = ParquetRecordBatchReaderBuilder::try_new(fs::File::open(path)?)?;
        let groups = builder.metadata().row_groups().iter().map(|g| g.num_rows() as usize).collect();
        let ix = path.with_extension("idx");
        let index = if ix.exists() {
            let file = fs::File::open(&ix)?;
            // SAFETY: the index is written once, beside a partition named by
            // its content's hash, and never written again (D-2.6); the map
            // is read only.
            Some(unsafe { memmap2::Mmap::map(&file)? })
        } else {
            None
        };
        let info = Arc::new(FileInfo { groups, index });
        if let Ok(mut f) = self.files.lock() {
            if f.len() >= FILES_KEPT {
                f.clear();
            }
            f.insert(path.to_path_buf(), info.clone());
        }
        Ok(info)
    }

    /// A partition's rows read from its file, `identity`'s only by its
    /// index's row selection.
    fn read_partition_file(&self, stored: &SchemaRef, p: &PartitionRef, identity: Option<&[u64]>) -> Result<Vec<RecordBatch>, DataError> {
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
        if m.hot {
            return self.read_hot(&m, f);
        }
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
        if m.hot {
            return self.read_hot_newest(&m, f);
        }
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
        if m.hot {
            return self.read_hot_bucket(m, p, f);
        }
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
        self.remove_unreferenced_hot(relation)?;
        let dir = self.root.join("part").join(relation);
        if dir.exists() {
            for e in fs::read_dir(&dir)? {
                let p = e?.path();
                if p.extension().is_some_and(|x| x == "parquet") {
                    let rel = p.strip_prefix(&self.root).map(|r| r.to_string_lossy().to_string()).unwrap_or_default();
                    if !used.contains(&rel) {
                        // The file's index map goes with it; its resident
                        // groups are never asked for again and age out.
                        if let Ok(mut f) = self.files.lock() {
                            f.remove(&p);
                        }
                        fs::remove_file(&p)?;
                        let _ = fs::remove_file(p.with_extension("idx"));
                    }
                }
            }
        }
        Ok(())
    }

    /// The hot versions of a relation the registry no longer names, and
    /// the segments no version of any relation names (ST-7.5's collection).
    fn remove_unreferenced_hot(&self, relation: &str) -> Result<(), DataError> {
        let dir = self.root.join("hot").join(relation);
        if dir.exists() {
            for e in fs::read_dir(&dir)? {
                let p = e?.path();
                let name = p.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
                if p.is_dir() && !name.starts_with('.') && self.registry.version(&name).is_none() {
                    if let Ok(mut h) = self.hot.lock() {
                        h.remove(&name);
                    }
                    fs::remove_dir_all(&p)?;
                }
            }
        }
        let segments = self.root.join("hot").join("segments");
        if segments.exists() {
            let mut used: BTreeSet<String> = BTreeSet::new();
            for v in self.registry.versions() {
                let m = self.manifest(&v.version)?;
                if m.hot {
                    if let Ok(h) = self.hot(&v.version) {
                        for s in h.manifest.segments() {
                            used.insert(s.file.clone());
                        }
                    }
                }
            }
            for e in fs::read_dir(&segments)? {
                let p = e?.path();
                let rel = p.strip_prefix(&self.root).map(|r| r.to_string_lossy().to_string()).unwrap_or_default();
                if p.extension().is_some_and(|x| x == "seg") && !used.contains(&rel) {
                    fs::remove_file(&p)?;
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
        walk(&self.root.join("hot"), &mut total)?;
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

/// Row group `g` of a partition file, decoded as one batch.
fn decode_group(path: &Path, g: usize) -> Result<RecordBatch, DataError> {
    let reader = ParquetRecordBatchReaderBuilder::try_new(fs::File::open(path)?)?
        .with_row_groups(vec![g])
        .with_batch_size(ROW_GROUP)
        .build()?;
    let batches = reader.collect::<Result<Vec<_>, _>>()?;
    match batches.as_slice() {
        [one] => Ok(one.clone()),
        _ => {
            let schema = reader_schema(path)?;
            Ok(arrow::compute::concat_batches(&schema, &batches)?)
        }
    }
}

/// A partition file's Arrow schema.
fn reader_schema(path: &Path) -> Result<SchemaRef, DataError> {
    Ok(ParquetRecordBatchReaderBuilder::try_new(fs::File::open(path)?)?.schema().clone())
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
