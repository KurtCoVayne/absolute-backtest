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
use crate::hot::append::TailRow;
use crate::hot::consolidate::{self, Consolidated};
use crate::hot::manifest::Kind;
use crate::hot::HotVersion;
use crate::query::ResolvedView;
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
        let (relation, source) = (rec.relation.clone(), rec.source.clone());
        let v = Arc::new(HotVersion::open(&self.root, &relation, version, false)?);
        self.replay_tails(&v, &relation, &source)?;
        self.residency.apply(&v);
        if let Ok(mut c) = self.hot.lock() {
            c.insert(version.to_string(), v.clone());
        }
        Ok(v)
    }

    /// Opens (prefaulting and locking under the store's residency) every
    /// current hot version: ST-8.8's fourth step. The versions opened.
    pub fn prefault_current(&self) -> Result<usize, DataError> {
        let mut n = 0;
        for (_, _, v) in self.registry.currents() {
            if self.manifest(&v).map(|m| m.hot).unwrap_or(false) {
                self.hot(&v)?;
                n += 1;
            }
        }
        Ok(n)
    }

    /// ST-8.8's third step: the tails rebuilt from the log, the rows after
    /// the version's publication that no sealed block holds (a row at or
    /// before an instrument's last sealed label is sealed or superseded).
    fn replay_tails(&self, v: &HotVersion, relation: &str, source: &str) -> Result<(), DataError> {
        let from = v.manifest.replay_from.unwrap_or(v.manifest.seq_high + 1);
        let batches = self.read_log(relation, source, from, u64::MAX)?;
        if batches.is_empty() {
            return Ok(());
        }
        let schema = &v.manifest.schema;
        let lay = layout(schema);
        let kinds: Vec<Kind> = v.manifest.columns.iter().map(|c| c.kind).collect();
        let normalised = crate::staging::normalise::normalise(
            schema,
            &lay,
            &crate::rows::concat(&stored_schema(schema), &batches)?,
            kinds.first().is_some_and(|k| *k == Kind::TickF32),
        )?;
        let mut ov = v.overlay.lock().unwrap_or_else(|p| p.into_inner());
        for (id, rows) in &normalised.by_id {
            let o = ov.instrument(*id, vec![0; kinds.len() - 1]);
            for r in rows {
                if !ov.admits(o, r.t) {
                    continue;
                }
                let row = tail_row(&mut ov, o, &kinds, r);
                ov.push(o, row);
            }
        }
        Ok(())
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
                if m.schema.columns != schema.columns || (!m.hot && m.rows == 0) {
                    // Another shape, or an empty Parquet version (a
                    // relation registered without rows): the hot history
                    // starts here.
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
        // Every row after its instrument's sealed history is an append
        // (ST-8.1): the rows join the tails under the published version;
        // anything else makes a version (S3 to S5).
        if let Some(p) = &prev {
            let appends = {
                let ov = p.overlay.lock().unwrap_or_else(|e| e.into_inner());
                normalised.by_id.iter().all(|(id, rows)| ov.by_id.get(id).is_none_or(|&o| rows.iter().all(|r| ov.admits(o, r.t))))
            };
            if appends {
                return self.append_hot(p, &normalised, schema, stored, source, first_seq);
            }
        }
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
            build::verify_ladder(&v, &planned, &mut verified)?;
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

    /// Appends (ST-8.2, ST-8.3): the rows numbered in (instrument, time,
    /// receipt) order and logged, then pushed into their instruments'
    /// tails (visible from then on), the tails due sealed; the version
    /// stands, its record's counts advanced.
    fn append_hot(
        &mut self,
        v: &Arc<HotVersion>,
        normalised: &normalise::Normalised,
        schema: &RelationSchema,
        stored: &SchemaRef,
        source: &str,
        first_seq: u64,
    ) -> Result<(String, (u64, u64)), DataError> {
        let relation = schema.name.clone();
        let lay = layout(schema);
        let mut seq = first_seq;
        let mut logged: Vec<(u64, u64, normalise::Row)> = Vec::new();
        for (id, rows) in &normalised.by_id {
            for r in rows {
                logged.push((seq, *id, normalise::Row { seq, ..r.clone() }));
                seq += 1;
            }
        }
        let last_seq = seq.saturating_sub(1);
        if logged.is_empty() {
            return Ok((v.manifest.version.clone(), (first_seq, last_seq)));
        }
        let batch = log_batch(schema, &lay, stored, &normalised.kinds, &logged)?;
        self.append_log(&relation, source, stored, &batch, first_seq, last_seq)?;
        crate::fault::point("append-after-log");
        let kinds = normalised.kinds.clone();
        {
            let mut ov = v.overlay.lock().unwrap_or_else(|p| p.into_inner());
            let mut due: Vec<usize> = Vec::new();
            for (_, id, r) in &logged {
                let o = ov.instrument(*id, vec![0; kinds.len() - 1]);
                let row = tail_row(&mut ov, o, &kinds, r);
                ov.push(o, row);
                if ov.due(o) && !due.contains(&o) {
                    due.push(o);
                }
            }
            for o in due {
                while ov.due(o) {
                    ov.seal(o)?;
                }
            }
        }
        // The version's record: its rows and span, as read now.
        let (rows, to, append_bytes) = {
            let ov = v.overlay.lock().unwrap_or_else(|p| p.into_inner());
            let extra: u64 = ov.buckets.values().map(|b| b.rows).sum();
            let to = ov.buckets.values().map(|b| b.t_max).max();
            (v.manifest.rows + extra, to, ov.append_bytes())
        };
        if let Some(rec) = self.registry.version(&v.manifest.version).cloned() {
            self.registry.record_version(VersionRecord { rows, to: to.max(rec.to), ..rec });
            self.registry.save()?;
        }
        self.next_seq.insert((relation.clone(), source.to_string()), seq);
        // ST-8.7: append files beyond the limit are folded into the base.
        if append_bytes >= self.consolidate_bytes {
            self.consolidate(&relation)?;
        }
        Ok((v.manifest.version.clone(), (first_seq, last_seq)))
    }

    /// Consolidates the current version of a hot relation (ST-8.7): its
    /// journaled blocks folded into base segments in instrument order, the
    /// layout published under the same version id, the tails rebuilt from
    /// the log. `None` when the relation has nothing sealed.
    pub fn consolidate(&mut self, relation: &str) -> Result<Option<(String, Consolidated)>, DataError> {
        let version = self
            .registry
            .current(relation, None)
            .map(str::to_string)
            .ok_or_else(|| DataError::NotFound(format!("`{}` has no current version", relation)))?;
        let m = self.manifest(&version)?;
        if !m.hot {
            return Err(DataError::Malformed(format!("`{}` is not held hot: nothing to consolidate (ST-8.7)", relation)));
        }
        let v = self.hot(&version)?;
        let Some(done) = consolidate::consolidate(&self.root, &v)? else { return Ok(None) };
        // The store's manifest of the version: its partitions are the
        // folded layout's buckets now.
        let partitions: Vec<PartitionRef> = done
            .manifest
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
        let stored = Manifest {
            partitions,
            rows: done.manifest.rows,
            from: done.manifest.from,
            to: done.manifest.to,
            ..self.manifests.lock().ok().and_then(|c| c.get(&version).map(|m| (**m).clone())).unwrap_or(m)
        };
        let mpath = self.root.join("manifest").join(relation).join(format!("{}.json", version));
        let tmp = mpath.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_vec_pretty(&stored)?)?;
        std::fs::rename(&tmp, &mpath)?;
        if let Ok(mut c) = self.manifests.lock() {
            c.remove(&version);
        }
        // The new layout replaces the old in the cache; readers holding
        // the old keep it until they release it. The old append files go
        // at the open, the old segments when no version names them.
        if let Ok(mut h) = self.hot.lock() {
            h.remove(&version);
        }
        drop(v);
        self.hot(&version)?;
        self.remove_unreferenced_hot(relation)?;
        Ok(Some((version, done)))
    }

    /// The figures of a hot version for `stats`: bytes of its segments,
    /// of its append files, the rows its tails hold, the journal's records
    /// and its generation.
    pub fn hot_figures(&self, version: &str) -> Option<HotFigures> {
        let m = self.manifest(version).ok()?;
        if !m.hot {
            return None;
        }
        let v = self.hot(version).ok()?;
        let ov = v.overlay.lock().unwrap_or_else(|p| p.into_inner());
        Some(HotFigures {
            segment_bytes: v.manifest.segments().iter().map(|s| s.bytes).sum(),
            append_bytes: ov.append_bytes(),
            tail_rows: ov.instruments.iter().map(|i| i.tail.len() as u64).sum(),
            journal_records: ov.journal_records,
            generation: ov.generation,
        })
    }

    /// ST-10.2: the segments of a hot relation at a view, for a reader on
    /// this machine to map read-only: one row per segment of the columns
    /// asked for (its file, bytes, codec version and hash), per append
    /// file (its journaled bytes) and per tail (its rows visible at the
    /// view's sequence), restricted to the instruments in `[from, to]`.
    pub fn map(&self, view: &ResolvedView, relation: &str, columns: Option<&[String]>, level: Option<&str>, from: u64, to: Option<u64>) -> Result<RecordBatch, DataError> {
        use arrow::array::{StringArray, UInt32Array};
        let version = match view.versions.get(relation) {
            Some(v) => v.clone(),
            None => self
                .registry
                .current(relation, None)
                .map(str::to_string)
                .ok_or_else(|| DataError::NotFound(format!("`{}` has no current version", relation)))?,
        };
        let m = self.manifest(&version)?;
        if !m.hot {
            return Err(DataError::Malformed(format!("map: `{}` is not held hot: it has no segments to map (ST-10.2)", relation)));
        }
        let v = self.hot(&version)?;
        let li = match level {
            Some(l) => v.level_index(l).ok_or_else(|| DataError::NotFound(format!("`{}` has no level `{}` of its ladder", relation, l)))?,
            None => 0,
        };
        let lv = &v.levels[li];
        let entries = &lv.entries;
        let wanted: Vec<usize> = match columns {
            Some(names) if !names.is_empty() => names
                .iter()
                .map(|n| {
                    entries
                        .iter()
                        .position(|e| e.name == *n)
                        .ok_or_else(|| DataError::NotFound(format!("map: `{}` has no column `{}` at this level", relation, n)))
                })
                .collect::<Result<_, _>>()?,
            _ => (0..entries.len()).collect(),
        };
        let in_range = |id: u64| id >= from && to.is_none_or(|t| id <= t);
        let mut rows: Vec<MapRow> = Vec::new();
        let seg_refs = &v.manifest.levels[li].segments;
        for &c in &wanted {
            for sref in seg_refs.get(c).map(|x| x.as_slice()).unwrap_or(&[]) {
                let seg = v.segments.iter().find(|s| s.hash == sref.hash);
                let covers = seg.is_some_and(|s| s.runs.iter().any(|r| lv.presence.instruments.get(r.instrument as usize).is_some_and(|i| in_range(i.id))));
                if !covers {
                    continue;
                }
                rows.push((
                    "segment".into(),
                    sref.file.clone(),
                    0,
                    sref.bytes,
                    abt_codec::CODEC_VERSION as u32,
                    sref.hash.clone(),
                    entries[c].name.clone(),
                    0,
                    seg.map(|s| s.runs.len() as u64).unwrap_or(0),
                ));
            }
        }
        if li == 0 {
            let ov = v.overlay.lock().unwrap_or_else(|p| p.into_inner());
            for f in ov.files.iter().filter(|f| f.len > 0) {
                let family = ov.families.get(f.column).cloned().unwrap_or_default();
                if !wanted.iter().any(|&c| entries[c].name == family) && !matches!(family.as_str(), "t" | "seq") {
                    continue;
                }
                let path = f
                    .path
                    .strip_prefix(&self.root)
                    .map(|p| p.to_string_lossy().to_string())
                    .unwrap_or_else(|_| f.path.to_string_lossy().to_string());
                rows.push(("append".into(), path, 0, f.len, abt_codec::CODEC_VERSION as u32, String::new(), family, 0, 0));
            }
            for (o, inst) in ov.instruments.iter().enumerate() {
                if inst.tail.is_empty() || !in_range(inst.id) {
                    continue;
                }
                rows.push((
                    "tail".into(),
                    String::new(),
                    0,
                    0,
                    abt_codec::CODEC_VERSION as u32,
                    String::new(),
                    String::new(),
                    inst.id,
                    ov.tail_visible(o, view.as_of_seq),
                ));
            }
        }
        let s = |f: &dyn Fn(&MapRow) -> String| -> ArrayRef { Arc::new(StringArray::from(rows.iter().map(f).collect::<Vec<_>>())) };
        Ok(RecordBatch::try_from_iter(vec![
            ("kind", s(&|r| r.0.clone())),
            ("path", s(&|r| r.1.clone())),
            ("offset", Arc::new(UInt64Array::from(rows.iter().map(|r| r.2).collect::<Vec<_>>())) as ArrayRef),
            ("length", Arc::new(UInt64Array::from(rows.iter().map(|r| r.3).collect::<Vec<_>>())) as ArrayRef),
            ("codec", Arc::new(UInt32Array::from(rows.iter().map(|r| r.4).collect::<Vec<_>>())) as ArrayRef),
            ("hash", s(&|r| r.5.clone())),
            ("column", s(&|r| r.6.clone())),
            ("instrument", Arc::new(UInt64Array::from(rows.iter().map(|r| r.7).collect::<Vec<_>>())) as ArrayRef),
            ("rows", Arc::new(UInt64Array::from(rows.iter().map(|r| r.8).collect::<Vec<_>>())) as ArrayRef),
        ])?)
    }

    /// What the appends add to a hot version's partitions (the stream's
    /// unit, E-14): a bucket's rows, time span, receipt bound and sequence
    /// high-water, and the buckets they opened beyond the published ones.
    pub(super) fn append_partitions(&self, m: &mut Manifest) {
        let Ok(v) = self.hot(&m.version) else { return };
        let ov = v.overlay.lock().unwrap_or_else(|p| p.into_inner());
        if ov.buckets.is_empty() {
            return;
        }
        for (b, st) in &ov.buckets {
            match m.partitions.iter_mut().find(|p| p.bucket == *b) {
                Some(p) => {
                    p.rows += st.rows;
                    p.t_min = p.t_min.min(st.t_min);
                    p.t_max = p.t_max.max(st.t_max);
                    p.r_max = p.r_max.max(st.r_max);
                    p.seq_max = p.seq_max.max(st.seq_max);
                }
                None => m.partitions.push(PartitionRef {
                    bucket: b.clone(),
                    file: format!("hot:{}", b),
                    hash: String::new(),
                    rows: st.rows,
                    t_min: st.t_min,
                    t_max: st.t_max,
                    r_min: st.t_min,
                    r_max: st.r_max,
                    seq_max: st.seq_max,
                }),
            }
            m.rows += st.rows;
            m.from = Some(m.from.map_or(st.t_min, |t| t.min(st.t_min)));
            m.to = Some(m.to.map_or(st.t_max, |t| t.max(st.t_max)));
        }
        m.partitions.sort_by(|a, b| a.bucket.cmp(&b.bucket));
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
        // A partition's rows are every revision the filter admits, as the
        // Parquet store's are (the stream merges revisions at their
        // receipts, D-5.3); `latest_only` is the whole read's. The bucket
        // is its time span, so the appends it holds come too (ST-8).
        let (_, from, to) = super::bucket_span(m.schema.calendar.as_deref(), p.t_min);
        let every = super::RowFilter {
            latest_only: false,
            t0: Some(f.t0.map_or(from, |t| t.max(from))),
            t1: Some(f.t1.map_or(to - 1, |t| t.min(to - 1))),
            ..f.clone()
        };
        let rows = v.read(&every, 0, u32::MAX)?;
        v.to_batch(&rows, &stored, &layout(&m.schema), None)
    }

    /// A ladder level of a hot version (ST-5.3) as a relation of its own:
    /// the level's schema (`<relation>@<level>`) and the filter's bars in
    /// canonical order.
    pub fn read_level(&self, version: &str, level: &str, f: &super::RowFilter) -> Result<(RelationSchema, Vec<RecordBatch>), DataError> {
        let v = self.hot(version)?;
        let li = v
            .level_index(level)
            .ok_or_else(|| DataError::NotFound(format!("`{}` has no level `{}` of its ladder", v.manifest.relation, level)))?;
        let schema = v.level_schema(li);
        let rows = v.read_at(li, f, 0, u32::MAX)?;
        let stored = stored_schema(&schema);
        let batch = v.to_batch_at(li, &schema, &rows, &stored, &layout(&schema), None)?;
        Ok((schema, vec![batch]))
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
        // A column holding a value off every decimal grid has no exact
        // view (A-D54): the `f64` view answers.
        if let Some(c) = v.manifest.columns.iter().find(|c| matches!(c.kind, Kind::Tick | Kind::TickF32) && c.raw) {
            return Err(DataError::Malformed(format!(
                "`{}`.`{}` holds values off any decimal grid: it has no tick view (ST-3.2, A-D54); read the `f64` view",
                m.relation, c.name
            )));
        }
        let mut scale = v
            .manifest
            .columns
            .iter()
            .filter(|c| matches!(c.kind, Kind::Tick | Kind::TickF32))
            .map(|c| c.exponent as u32)
            .max()
            .unwrap_or(0);
        {
            // An append may have brought a finer quantum (ST-8.3).
            let ov = v.overlay.lock().unwrap_or_else(|p| p.into_inner());
            for (c, _) in v.manifest.columns.iter().enumerate().filter(|(_, c)| matches!(c.kind, Kind::Tick | Kind::TickF32)) {
                for i in &ov.instruments {
                    scale = scale.max(i.exponents.get(c).copied().unwrap_or(0) as u32);
                }
            }
        }
        let rows = v.read(f, 0, u32::MAX)?;
        if rows.is_empty() {
            return Ok((scale, Vec::new()));
        }
        Ok((scale, vec![v.to_batch(&rows, &stored_schema(&m.schema), &layout(&m.schema), Some(scale))?]))
    }
}

/// A row of the `map` answer: kind, path, offset, length, codec, hash,
/// column, instrument, rows.
type MapRow = (String, String, u64, u64, u32, String, String, u64, u64);

/// A hot version's figures for `stats`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct HotFigures {
    pub segment_bytes: u64,
    pub append_bytes: u64,
    pub tail_rows: u64,
    pub journal_records: u64,
    pub generation: u64,
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

/// A normalised row as a tail row of its instrument: its integers at the
/// instrument's exponents (raised when the row's quantum is finer).
#[allow(clippy::needless_range_loop)]
fn tail_row(ov: &mut crate::hot::append::Overlay, o: usize, kinds: &[Kind], r: &normalise::Row) -> TailRow {
    let ncols = kinds.len() - 1;
    let mut floats = Vec::with_capacity(ncols);
    for c in 0..ncols {
        match kinds[c] {
            Kind::Tick | Kind::TickF32 => {
                let v = f64::from_bits(r.vals[c]);
                let source = if kinds[c] == Kind::TickF32 { abt_codec::tick::Source::F32 } else { abt_codec::tick::Source::F64 };
                if let Some(e) = abt_codec::tick::exponent(&[v], source) {
                    ov.raise(o, c, e);
                }
                floats.push(v);
            }
            Kind::Int | Kind::Residual => floats.push(r.vals[c] as i64 as f64),
        }
    }
    let exps = ov.instruments[o].exponents.clone();
    let ints: Vec<i64> = (0..ncols)
        .map(|c| match kinds[c] {
            Kind::Tick | Kind::TickF32 => abt_codec::tick::scaled_or_zero(floats[c], exps[c]),
            Kind::Int | Kind::Residual => r.vals[c] as i64,
        })
        .collect();
    TailRow {
        label: r.t,
        r: r.r,
        seq: r.seq,
        ints,
        floats,
    }
}

/// One line of ST-9's table for a hot version, measured from its files
/// (ST-7.4, G-ST2's harness): a structure, its bytes, the rows it covers
/// and the blocks it holds.
#[derive(Clone, Debug, serde::Serialize)]
pub struct Measured {
    pub structure: String,
    pub bytes: u64,
    pub rows: u64,
    pub blocks: u64,
}

impl Store {
    /// The lines of ST-9 for a hot version as its files measure them: each
    /// level's columns (the segment bytes holding their blocks: the values,
    /// a level's counts and sequences, the receipt residual) and its
    /// presence file; the manifest; the directory (one locator per block,
    /// in memory); the append files and the tails (ST-8). `None` for a
    /// Parquet version.
    pub fn measure(&self, version: &str) -> Result<Option<Vec<Measured>>, DataError> {
        if !self.manifest(version)?.hot {
            return Ok(None);
        }
        let v = self.hot(version)?;
        let hm = &v.manifest;
        let dir = self.root.join(crate::hot::manifest::HotManifest::dir(&hm.relation, &hm.version));
        let size = |p: &std::path::Path| std::fs::metadata(p).map(|m| m.len()).unwrap_or(0);
        let mut out = Vec::new();
        let mut locators = 0u64;
        for (li, level) in hm.levels.iter().enumerate() {
            let data = &v.levels[li];
            let name = if li == 0 { "base".to_string() } else { format!("level {}", level.calendar) };
            let rows: u64 = data.presence.instruments.iter().map(|i| i.rows).sum();
            let cols = if li == 0 { &hm.columns } else { &level.columns };
            for (ci, segs) in level.segments.iter().enumerate() {
                let col = cols.get(ci).map(|c| c.name.as_str()).unwrap_or("?");
                let blocks: u64 = data.columns.get(ci).map(|c| c.blocks.iter().map(|b| b.len() as u64).sum()).unwrap_or(0);
                locators += blocks;
                out.push(Measured {
                    structure: format!("{}: column {}", name, col),
                    bytes: segs.iter().map(|s| s.bytes).sum(),
                    rows,
                    blocks,
                });
            }
            out.push(Measured {
                structure: format!("{}: presence", name),
                bytes: size(&dir.join(&level.presence)),
                rows,
                blocks: 0,
            });
        }
        out.push(Measured {
            structure: "manifest".into(),
            bytes: size(&dir.join("manifest.json")),
            rows: hm.rows,
            blocks: 0,
        });
        out.push(Measured {
            structure: "directory (one locator per block, in memory)".into(),
            bytes: locators * std::mem::size_of::<crate::hot::version::Loc>() as u64,
            rows: hm.rows,
            blocks: locators,
        });
        let ov = v.overlay.lock().unwrap_or_else(|p| p.into_inner());
        let tail_rows: u64 = ov.instruments.iter().map(|i| i.tail.len() as u64).sum();
        let values = v.value_columns() as u64;
        out.push(Measured {
            structure: "append files (sealed blocks)".into(),
            bytes: ov.append_bytes(),
            rows: ov.instruments.iter().map(|i| i.sealed.len() as u64).sum::<u64>() * abt_codec::BLOCK as u64,
            blocks: ov.instruments.iter().map(|i| i.sealed.len() as u64).sum(),
        });
        out.push(Measured {
            structure: "tails (rows in memory)".into(),
            bytes: tail_rows * (std::mem::size_of::<TailRow>() as u64 + 16 * values),
            rows: tail_rows,
            blocks: 0,
        });
        Ok(Some(out))
    }
}
