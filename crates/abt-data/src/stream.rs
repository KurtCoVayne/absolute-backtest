//! Streams (D-5): the events of a stream spec, totally ordered by
//! Definition D-5.3 and numbered by `seq` from 1. In replay the order is a
//! k-way merge of the relations' partitions with the clock's barriers:
//! tuples by (`r`, relation id, identity, `t`, revision), barriers by `at`,
//! a barrier after the tuples of its instant (E-13), barriers of one instant
//! in the order the spec names their calendars. A partition is sorted by
//! `t`, so the merge opens it when its frontier reaches the partition's
//! least `r` and reads it in receipt order (E-14). The clock source is the
//! `calendar_label` rows of the spec's view (Definition D-5.1): a relation of
//! the spec named `calendar_label` or `<namespace>.calendar_label`, else the
//! registry's current one.
//!
//! The order is a deterministic function of the versions, the calendars and
//! the receipt filter, so a subscription from `from_seq` yields the same
//! events with the same `seq` as every earlier one (Proposition D-5.4).

use std::cmp::Reverse;
use std::collections::{BTreeMap, BinaryHeap};
use std::sync::Arc;

use abt_wire::canonical::Canonical;
use abt_wire::generated::abt::wire::{ColumnType, RelationSchema, Role, StreamSpec};
use arrow::array::{new_null_array, Array, ArrayRef, AsArray, DictionaryArray, RecordBatch, StringArray, TimestampNanosecondArray, UInt32Array, UInt64Array, UInt8Array};
use arrow::datatypes::{DataType, Field, Schema, SchemaRef, TimeUnit, UInt32Type, UInt8Type};

use crate::error::DataError;
use crate::rows::{self, answer_type, layout, Layout};
use crate::store::{Manifest, RowFilter, Store};

/// The registry relation the clock is read from (D-2.4).
pub const CLOCK: &str = "calendar_label";

/// Whether a relation of a stream spec is a clock source.
pub fn is_clock(relation: &str) -> bool {
    relation == CLOCK || relation.ends_with(".calendar_label")
}

/// Rows per event batch (D-5.6's channel holds batches of this size).
pub const BATCH_EVENTS: usize = 65_536;

/// The event kinds, as the `kind` column's dictionary holds them (P-4).
pub const KINDS: [&str; 3] = ["tuple", "barrier", "mark"];

/// A stream spec with its versions fixed: the stream's identity.
#[derive(Clone, Debug, PartialEq, serde::Serialize)]
pub struct Resolved {
    /// The tuple relations and their versions, by name: a relation's id in
    /// the canonical order is its position here.
    pub relations: Vec<(String, String)>,
    /// The clock relations' versions.
    pub clocks: Vec<String>,
    /// The calendars barriers are emitted on, in their order at one instant.
    pub calendars: Vec<String>,
    pub r_max: Option<i64>,
}

impl Resolved {
    /// The stream's id: the hash of its canonical form.
    pub fn id(&self) -> Result<String, DataError> {
        Ok(self.canonical_hash().map_err(|e| DataError::Malformed(e.to_string()))?.hex())
    }
}

/// Fixes the versions a spec reads: its version list, else its binding's
/// pairs; the clock defaults to the registry's current `calendar_label`
/// when the spec names calendars and no clock relation.
pub fn resolve(store: &Store, spec: &StreamSpec) -> Result<Resolved, DataError> {
    let mut pairs: BTreeMap<String, String> = BTreeMap::new();
    if let Some(b) = &spec.binding {
        let binding = store.registry.binding(b).ok_or_else(|| DataError::NotFound(format!("no binding {}", b)))?;
        for p in binding.pairs.unwrap_or_default() {
            pairs.insert(p.relation, p.version);
        }
    }
    for p in spec.relations.clone().unwrap_or_default() {
        pairs.insert(p.relation, p.version);
    }
    let mut relations = Vec::new();
    let mut clocks = Vec::new();
    for (rel, version) in pairs {
        let rec = store.registry.version(&version).ok_or_else(|| DataError::UnknownVersion(version.clone()))?;
        if rec.relation != rel {
            return Err(DataError::Malformed(format!("version {} is of `{}`, not of `{}`", version, rec.relation, rel)));
        }
        if is_clock(&rel) {
            clocks.push(version);
        } else {
            relations.push((rel, version));
        }
    }
    let calendars = spec.calendars.clone().unwrap_or_default();
    if clocks.is_empty() && !calendars.is_empty() {
        let v = store
            .registry
            .current(CLOCK, None)
            .ok_or_else(|| DataError::NotFound("the stream names calendars and the store has no calendar labels".into()))?;
        clocks.push(v.to_string());
    }
    Ok(Resolved {
        relations,
        clocks,
        calendars,
        r_max: spec.receipt_filter.as_ref().and_then(|f| f.r_max),
    })
}

/// The canonical order key of an event (Definition D-5.3).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct Key {
    /// `r` of a tuple, `at` of a barrier.
    instant: i64,
    /// 0 tuple, 1 mark, 2 barrier: tuples first at one instant.
    kind: u8,
    /// The relation id, or the calendar's position in the spec.
    order: u32,
    identity: [u64; 2],
    t: i64,
    /// The row's sequence number in its log: the revision order.
    seq: u64,
}

/// One partition being read in receipt order.
struct Cursor {
    rel: u32,
    batch: RecordBatch,
    keys: Vec<(Key, u32)>,
    pos: usize,
    /// The batch's position in the current chunk's sources, once used.
    chunk: Option<u32>,
}

/// A barrier of the clock.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Barrier {
    pub calendar: u32,
    pub label: i64,
    pub at: i64,
}

/// An event as the merge produces it.
#[derive(Clone, Copy, Debug)]
enum Ev {
    Tuple { rel: u32, src: u32, row: u32, entity: Option<u64>, t: i64, r: i64 },
    Barrier(Barrier),
}

/// The first two columns of a row's key (entities, then labels by code).
fn identity_of(s: &RelationSchema, lay: &Layout, b: &RecordBatch, i: usize) -> [u64; 2] {
    let mut out = [0u64; 2];
    for (k, &c) in lay.key.iter().take(2).enumerate() {
        out[k] = match s.columns[c].ty {
            ColumnType::Entity => b.column(c).as_primitive::<arrow::datatypes::UInt64Type>().value(i),
            ColumnType::Label => b.column(c).as_primitive::<UInt32Type>().value(i) as u64,
            ColumnType::Count => b.column(c).as_primitive::<arrow::datatypes::Int64Type>().value(i) as u64,
            _ => 0,
        };
    }
    out
}

/// The first entity column of a relation, the event's `entity`.
fn entity_column(s: &RelationSchema) -> Option<usize> {
    s.columns.iter().position(|c| c.role == Role::Entity && c.ty == ColumnType::Entity)
}

/// The columns a tuple of a relation carries in an event batch: every
/// column but its times, named `<relation>.<column>` (P-4).
fn value_columns(s: &RelationSchema) -> Vec<usize> {
    (0..s.columns.len()).filter(|&i| !matches!(s.columns[i].role, Role::EventTime | Role::ReceiptTime)).collect()
}

/// The barriers of a resolved spec, in order.
fn barriers(store: &Store, res: &Resolved) -> Result<Vec<(Key, Barrier)>, DataError> {
    let mut out = Vec::new();
    for v in &res.clocks {
        let m = store.manifest(v)?;
        let s = &m.schema;
        let col = |name: &str| {
            s.columns
                .iter()
                .position(|c| c.name == name)
                .ok_or_else(|| DataError::Schema(format!("the clock `{}` has no column `{}`", m.relation, name)))
        };
        let (cc, lc, ac) = (col("calendar")?, col("label")?, col("at")?);
        let rows = store.read_all(v)?;
        let cals = rows.column(cc).as_primitive::<UInt32Type>();
        let labels = rows::times(&rows, lc);
        let ats = rows.column(ac).as_primitive::<arrow::datatypes::TimestampNanosecondType>();
        for i in 0..rows.num_rows() {
            let name = store.registry.label_text(cals.value(i)).unwrap_or("");
            let Some(pos) = res.calendars.iter().position(|c| c == name) else { continue };
            let at = if ats.is_null(i) { labels.value(i) } else { ats.value(i) };
            if res.r_max.is_some_and(|a| at > a) {
                continue;
            }
            let b = Barrier {
                calendar: pos as u32,
                label: labels.value(i),
                at,
            };
            out.push((
                Key {
                    instant: at,
                    kind: 2,
                    order: pos as u32,
                    identity: [0, 0],
                    t: b.label,
                    seq: 0,
                },
                b,
            ));
        }
    }
    out.sort_by_key(|(k, _)| *k);
    out.dedup_by_key(|(k, _)| *k);
    Ok(out)
}

/// A stream being read: the merge's state and the batch it is filling.
pub struct Stream {
    pub res: Resolved,
    pub id: String,
    manifests: Vec<Manifest>,
    layouts: Vec<Layout>,
    /// Partitions not yet opened: (least r, relation, partition).
    pending: BinaryHeap<Reverse<(i64, u32, u32)>>,
    cursors: Vec<Option<Cursor>>,
    heap: BinaryHeap<Reverse<(Key, u32)>>,
    barriers: Vec<(Key, Barrier)>,
    next_barrier: usize,
    /// The seq of the last event produced.
    seq: u64,
    schema: SchemaRef,
    labels: ArrayRef,
    /// The current chunk: its sources (relation, batch) and its events.
    sources: Vec<(u32, RecordBatch)>,
    events: Vec<(u64, Ev)>,
}

impl Stream {
    /// Opens a stream over a store.
    pub fn open(store: &Store, res: Resolved) -> Result<Stream, DataError> {
        let id = res.id()?;
        let mut manifests = Vec::new();
        let mut layouts = Vec::new();
        let mut pending = BinaryHeap::new();
        for (j, (_, v)) in res.relations.iter().enumerate() {
            let m = store.manifest(v)?;
            for (p, part) in m.partitions.iter().enumerate() {
                pending.push(Reverse((part.r_min, j as u32, p as u32)));
            }
            layouts.push(layout(&m.schema));
            manifests.push(m);
        }
        let barriers = barriers(store, &res)?;
        let schema = event_schema(&res, &manifests);
        let labels: ArrayRef = Arc::new(StringArray::from(store.registry.labels().to_vec()));
        Ok(Stream {
            res,
            id,
            manifests,
            layouts,
            pending,
            cursors: Vec::new(),
            heap: BinaryHeap::new(),
            barriers,
            next_barrier: 0,
            seq: 0,
            schema,
            labels,
            sources: Vec::new(),
            events: Vec::new(),
        })
    }

    /// The event batches' schema (P-4).
    pub fn schema(&self) -> SchemaRef {
        self.schema.clone()
    }

    /// The seq of the last event produced.
    pub fn seq(&self) -> u64 {
        self.seq
    }

    /// Opens a partition: its admitted rows sorted into receipt order.
    fn open_partition(&mut self, store: &Store, j: u32, p: u32) -> Result<(), DataError> {
        let m = &self.manifests[j as usize];
        let part = &m.partitions[p as usize];
        let f = RowFilter {
            r_max: self.res.r_max,
            ..RowFilter::default()
        };
        let batch = store.read_partition_rows(m, part, &f)?;
        if batch.num_rows() == 0 {
            return Ok(());
        }
        let lay = &self.layouts[j as usize];
        let ts = rows::times(&batch, lay.t);
        let rs = lay.r.map(|r| rows::times(&batch, r));
        let seqs = rows::seqs(&batch, lay.seq);
        let mut keys: Vec<(Key, u32)> = (0..batch.num_rows())
            .map(|i| {
                let t = ts.value(i);
                (
                    Key {
                        instant: rs.map(|r| r.value(i)).unwrap_or(t),
                        kind: 0,
                        order: j,
                        identity: identity_of(&m.schema, lay, &batch, i),
                        t,
                        seq: seqs.value(i),
                    },
                    i as u32,
                )
            })
            .collect();
        keys.sort_unstable();
        let first = keys[0].0;
        let c = self.cursors.len() as u32;
        self.cursors.push(Some(Cursor {
            rel: j,
            batch,
            keys,
            pos: 0,
            chunk: None,
        }));
        self.heap.push(Reverse((first, c)));
        Ok(())
    }

    /// The next event of the merge, without its seq.
    fn next_event(&mut self, store: &Store) -> Result<Option<Ev>, DataError> {
        loop {
            let tuple = self.heap.peek().map(|Reverse((k, _))| *k);
            let barrier = self.barriers.get(self.next_barrier).map(|(k, _)| *k);
            let frontier = match (tuple, barrier) {
                (Some(a), Some(b)) => a.instant.min(b.instant),
                (Some(a), None) => a.instant,
                (None, Some(b)) => b.instant,
                (None, None) => i64::MAX,
            };
            // A partition whose least r is at or before the frontier may hold
            // the next tuple: open it first.
            if let Some(&Reverse((r_min, j, p))) = self.pending.peek() {
                if r_min <= frontier {
                    self.pending.pop();
                    self.open_partition(store, j, p)?;
                    continue;
                }
            }
            return Ok(match (tuple, barrier) {
                (None, None) => None,
                (Some(a), b) if b.is_none_or(|b| a < b) => {
                    let Reverse((_, c)) = self.heap.pop().expect("a peeked cursor");
                    let cur = self.cursors[c as usize].as_mut().expect("a live cursor");
                    let (key, row) = cur.keys[cur.pos];
                    cur.pos += 1;
                    let src = match cur.chunk {
                        Some(s) => s,
                        None => {
                            let s = self.sources.len() as u32;
                            self.sources.push((cur.rel, cur.batch.clone()));
                            cur.chunk = Some(s);
                            s
                        }
                    };
                    let rel = cur.rel;
                    let entity = entity_column(&self.manifests[rel as usize].schema).map(|e| cur.batch.column(e).as_primitive::<arrow::datatypes::UInt64Type>().value(row as usize));
                    if cur.pos < cur.keys.len() {
                        let next = cur.keys[cur.pos].0;
                        self.heap.push(Reverse((next, c)));
                    } else {
                        self.cursors[c as usize] = None;
                    }
                    Some(Ev::Tuple {
                        rel,
                        src,
                        row,
                        entity,
                        t: key.t,
                        r: key.instant,
                    })
                }
                (_, Some(_)) => {
                    let (_, b) = self.barriers[self.next_barrier];
                    self.next_barrier += 1;
                    Some(Ev::Barrier(b))
                }
                (Some(_), None) => unreachable!("covered by the tuple arm"),
            });
        }
    }

    /// The next batch of events with seq in `from..=to`, up to `max` events;
    /// `None` once the stream (or `to`) is exhausted.
    pub fn next_batch(&mut self, store: &Store, from: u64, to: Option<u64>, max: usize) -> Result<Option<RecordBatch>, DataError> {
        self.events.clear();
        for c in self.cursors.iter_mut().flatten() {
            c.chunk = None;
        }
        self.sources.clear();
        while self.events.len() < max {
            if to.is_some_and(|to| self.seq >= to) {
                break;
            }
            let Some(ev) = self.next_event(store)? else { break };
            self.seq += 1;
            if self.seq >= from {
                self.events.push((self.seq, ev));
            } else if let Ev::Tuple { .. } = ev {
                // Skipped: its source need not stay in the chunk.
                if self.sources.len() > 64 {
                    for c in self.cursors.iter_mut().flatten() {
                        c.chunk = None;
                    }
                    self.sources.clear();
                }
            }
        }
        if self.events.is_empty() {
            return Ok(None);
        }
        self.build().map(Some)
    }

    /// The current chunk as an event batch (P-4).
    fn build(&self) -> Result<RecordBatch, DataError> {
        let n = self.events.len();
        let seq = UInt64Array::from(self.events.iter().map(|(s, _)| *s).collect::<Vec<_>>());
        let kind_keys = UInt8Array::from(self.events.iter().map(|(_, e)| if let Ev::Tuple { .. } = e { 0u8 } else { 1 }).collect::<Vec<_>>());
        let kind = DictionaryArray::<UInt8Type>::try_new(kind_keys, Arc::new(StringArray::from(KINDS.to_vec())))?;
        let rel_names: Vec<&str> = self.res.relations.iter().map(|(r, _)| r.as_str()).collect();
        let rel_keys = UInt32Array::from(self.events.iter().map(|(_, e)| if let Ev::Tuple { rel, .. } = e { Some(*rel) } else { None }).collect::<Vec<_>>());
        let relation = DictionaryArray::<UInt32Type>::try_new(rel_keys, Arc::new(StringArray::from(rel_names)))?;
        let entity = UInt64Array::from(self.events.iter().map(|(_, e)| if let Ev::Tuple { entity, .. } = e { *entity } else { None }).collect::<Vec<_>>());
        let ts = |f: &dyn Fn(&Ev) -> Option<i64>| -> ArrayRef { Arc::new(TimestampNanosecondArray::from(self.events.iter().map(|(_, e)| f(e)).collect::<Vec<_>>()).with_timezone("UTC")) };
        let t = ts(&|e| if let Ev::Tuple { t, .. } = e { Some(*t) } else { None });
        let r = ts(&|e| if let Ev::Tuple { r, .. } = e { Some(*r) } else { None });
        let label = ts(&|e| if let Ev::Barrier(b) = e { Some(b.label) } else { None });
        let at = ts(&|e| if let Ev::Barrier(b) = e { Some(b.at) } else { None });
        let cal_keys = UInt32Array::from(self.events.iter().map(|(_, e)| if let Ev::Barrier(b) = e { Some(b.calendar) } else { None }).collect::<Vec<_>>());
        let calendar = DictionaryArray::<UInt32Type>::try_new(cal_keys, Arc::new(StringArray::from(self.res.calendars.clone())))?;
        let mark_kind = new_null_array(&DataType::Dictionary(Box::new(DataType::UInt32), Box::new(DataType::Utf8)), n);
        let mut cols: Vec<ArrayRef> = vec![Arc::new(seq), Arc::new(kind), Arc::new(relation), Arc::new(entity), t, r, Arc::new(calendar), label, at, mark_kind];
        // Each relation's columns: gathered from the chunk's sources, null
        // in the other relations' events and in barriers.
        for (j, m) in self.manifests.iter().enumerate() {
            for c in value_columns(&m.schema) {
                let stored_ty = rows::stored_type(m.schema.columns[c].ty);
                let null = new_null_array(&stored_ty, 1);
                let mut arrays: Vec<&dyn Array> = vec![null.as_ref()];
                let mut slot: BTreeMap<u32, usize> = BTreeMap::new();
                for (s, (rel, b)) in self.sources.iter().enumerate() {
                    if *rel as usize == j {
                        slot.insert(s as u32, arrays.len());
                        arrays.push(b.column(c).as_ref());
                    }
                }
                let idx: Vec<(usize, usize)> = self
                    .events
                    .iter()
                    .map(|(_, e)| match e {
                        Ev::Tuple { rel, src, row, .. } if *rel as usize == j => (slot[src], *row as usize),
                        _ => (0, 0),
                    })
                    .collect();
                let gathered = arrow::compute::interleave(&arrays, &idx)?;
                let col: ArrayRef = if m.schema.columns[c].ty == ColumnType::Label {
                    Arc::new(DictionaryArray::<UInt32Type>::try_new(gathered.as_primitive::<UInt32Type>().clone(), self.labels.clone())?)
                } else {
                    gathered
                };
                cols.push(col);
            }
        }
        Ok(RecordBatch::try_new(self.schema.clone(), cols)?)
    }
}

/// The event batch schema of a stream (P-4): the event columns, then each
/// relation's columns as `<relation>.<column>`, all nullable.
fn event_schema(res: &Resolved, manifests: &[Manifest]) -> SchemaRef {
    let dict = |name: &str, key: DataType| Field::new(name, DataType::Dictionary(Box::new(key), Box::new(DataType::Utf8)), true);
    let time = |name: &str| Field::new(name, DataType::Timestamp(TimeUnit::Nanosecond, Some("UTC".into())), true);
    let mut fields = vec![
        Field::new("seq", DataType::UInt64, false),
        Field::new("kind", DataType::Dictionary(Box::new(DataType::UInt8), Box::new(DataType::Utf8)), false),
        dict("relation", DataType::UInt32),
        Field::new("entity", DataType::UInt64, true),
        time("t"),
        time("r"),
        dict("calendar", DataType::UInt32),
        time("label"),
        time("at"),
        dict("mark_kind", DataType::UInt32),
    ];
    for ((rel, _), m) in res.relations.iter().zip(manifests) {
        for c in value_columns(&m.schema) {
            let col = &m.schema.columns[c];
            fields.push(Field::new(format!("{}.{}", rel, col.name), answer_type(col.ty), true));
        }
    }
    let mut meta = std::collections::HashMap::new();
    meta.insert("abt.schema".to_string(), "1.0".to_string());
    Arc::new(Schema::new_with_metadata(fields, meta))
}

/// The barrier index of a stream (P-3.1 `stream_index`): every barrier's
/// calendar, label, instant and seq, computed without building the events.
/// A barrier's seq is the number of tuples at or before its instant plus its
/// rank among the barriers (Proposition D-5.2).
pub fn index(store: &Store, res: &Resolved) -> Result<Vec<(String, Barrier, u64)>, DataError> {
    let mut rs: Vec<i64> = Vec::new();
    for (_, v) in &res.relations {
        let m = store.manifest(v)?;
        let lay = layout(&m.schema);
        let f = RowFilter {
            r_max: res.r_max,
            ..RowFilter::default()
        };
        for p in &m.partitions {
            let b = store.read_partition_rows(&m, p, &f)?;
            let col = lay.r.unwrap_or(lay.t);
            rs.extend(rows::times(&b, col).values().iter().copied());
        }
    }
    rs.sort_unstable();
    let bars = barriers(store, res)?;
    Ok(bars
        .into_iter()
        .enumerate()
        .map(|(i, (_, b))| {
            let tuples = rs.partition_point(|r| *r <= b.at) as u64;
            (res.calendars[b.calendar as usize].clone(), b, tuples + i as u64 + 1)
        })
        .collect())
}

/// The barrier index as the protocol's table (`calendar`, `label`, `at`, `seq`).
pub fn index_table(res: &Resolved, index: &[(String, Barrier, u64)]) -> Result<RecordBatch, DataError> {
    let keys = UInt32Array::from(index.iter().map(|(_, b, _)| b.calendar).collect::<Vec<_>>());
    let calendar = DictionaryArray::<UInt32Type>::try_new(keys, Arc::new(StringArray::from(res.calendars.clone())))?;
    let label = TimestampNanosecondArray::from(index.iter().map(|(_, b, _)| b.label).collect::<Vec<_>>()).with_timezone("UTC");
    let at = TimestampNanosecondArray::from(index.iter().map(|(_, b, _)| b.at).collect::<Vec<_>>()).with_timezone("UTC");
    let seq = UInt64Array::from(index.iter().map(|(_, _, s)| *s).collect::<Vec<_>>());
    let schema = Arc::new(Schema::new(vec![
        Field::new("calendar", DataType::Dictionary(Box::new(DataType::UInt32), Box::new(DataType::Utf8)), false),
        Field::new("label", DataType::Timestamp(TimeUnit::Nanosecond, Some("UTC".into())), false),
        Field::new("at", DataType::Timestamp(TimeUnit::Nanosecond, Some("UTC".into())), false),
        Field::new("seq", DataType::UInt64, false),
    ]));
    Ok(RecordBatch::try_new(schema, vec![Arc::new(calendar), Arc::new(label), Arc::new(at), Arc::new(seq)])?)
}
