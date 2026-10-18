//! The data service (P-3.1): what each request of the data layer does, over
//! the exchange `abt-wire` speaks, in process or on a Unix socket (P-2.1).
//! A request reads the store under a shared lock and a write (`put`, an
//! aggregate's cache object, a binding, a pin) under an exclusive one; a
//! subscription takes the shared lock per batch, so a long replay never
//! holds writers off for its whole length.
//!
//! A demand is resolved by D-7's resolver (crate::resolver) against the
//! store and the sources beside it.
//!
//! Live sessions (crate::live, D-5.4) record a feed into the store under
//! the session's name; a live stream follows a session's versions and emits
//! its barriers on the service's clock (D-5.2 live), recording them so that
//! the session replays (D-5.5); a paced replay sends each barrier when the
//! clock reaches its instant, relative to the first.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::{Arc, RwLock, RwLockReadGuard, RwLockWriteGuard};

use abt_wire::generated::abt::wire::{
    AggregateResponse, ClockSource, Done, ExplainResponse, Layer, Message, PutResponse, ResolveDynamicResponse, ResolveResponse, SchemaResponse, SubscribeRequest, SubscribeResponse,
};
use abt_wire::rpc::{error, serve, Request, Responder, Service, WireError};
use abt_wire::transport::{bind, channel_pair, Connection};
use abt_wire::{ErrorCode, ErrorMessage};
use arrow::array::{ArrayRef, AsArray, BooleanArray, RecordBatch, StringArray, TimestampNanosecondArray, UInt64Array};

use crate::adapters::{Feed, Fetched};
use crate::agg;
use crate::error::DataError;
use crate::live::{self, Clock, Session, Sessions, WallClock};
use crate::query::{self, ResolvedView};
use crate::resolver::{Resolution, Sources};
use crate::store::Store;
use crate::stream::{self, Resolved, Stream, BATCH_EVENTS, SUBSCRIPTION_WINDOW};

/// Frames in flight per direction of an in-process connection.
const INPROC_BOUND: usize = 64;

pub struct DataService {
    store: RwLock<Store>,
    /// A subscription's window of unacknowledged events (D-5.6).
    window: usize,
    /// The sources beside the store a demand may be bought from (D-7.1).
    sources: Sources,
    /// The clock of live streams and paced replays (D-5.2): the wall clock
    /// unless the service is given another.
    clock: Arc<dyn Clock>,
    /// The live sessions this service records (D-5.4).
    sessions: Sessions,
    /// A calendar's grace by its name, where it is not the default
    /// (`live::default_grace`).
    grace: BTreeMap<String, i64>,
}

/// A subscription's channel (D-5.6): the batches sent and not yet
/// acknowledged, with their last seq and size.
struct Window {
    unacked: std::collections::VecDeque<(u64, usize)>,
    held: usize,
    limit: usize,
}

impl Window {
    fn new(limit: usize) -> Window {
        Window {
            unacked: Default::default(),
            held: 0,
            limit,
        }
    }

    /// Sends a batch once the channel has room for it; `false` when the
    /// subscriber cancelled or went away while it waited.
    fn send(&mut self, out: &Responder, sink: &mut abt_wire::rpc::TableSink<'_>, b: &RecordBatch) -> Result<bool, ErrorMessage> {
        while self.held > 0 && self.held + b.num_rows() > self.limit {
            let acked = out.acked();
            while self.unacked.front().is_some_and(|(last, _)| *last <= acked) {
                self.held -= self.unacked.pop_front().map_or(0, |(_, n)| n);
            }
            if self.held > 0 && self.held + b.num_rows() > self.limit {
                let oldest = self.unacked.front().map_or(0, |(last, _)| *last);
                if !out.wait_acked(oldest) {
                    return Ok(false);
                }
            }
        }
        sink.batch(b).map_err(wire)?;
        let last = b.column(0).as_any().downcast_ref::<UInt64Array>().map_or(0, |q| q.value(q.len() - 1));
        self.unacked.push_back((last, b.num_rows()));
        self.held += b.num_rows();
        Ok(true)
    }
}

fn wire(e: WireError) -> ErrorMessage {
    error(ErrorCode::Internal, e.to_string())
}

fn data(e: DataError) -> ErrorMessage {
    e.to_wire()
}

impl DataService {
    pub fn new(store: Store) -> Arc<DataService> {
        DataService::with_window(store, SUBSCRIPTION_WINDOW)
    }

    /// A service whose subscriptions hold `window` events unacknowledged
    /// (D-5.6's configuration), at least one.
    pub fn with_window(store: Store, window: usize) -> Arc<DataService> {
        Arc::new(DataService {
            store: RwLock::new(store),
            window: window.max(1),
            sources: Sources::store_only(),
            clock: Arc::new(WallClock),
            sessions: Sessions::default(),
            grace: BTreeMap::new(),
        })
    }

    /// A service whose demands may be bought from `sources` beside the
    /// store (D-7): the catalog of `abt data serve --config FILE` and the
    /// adapters its sources name.
    pub fn with_sources(store: Store, sources: Sources) -> Arc<DataService> {
        DataService::with_clock(store, sources, Arc::new(WallClock), BTreeMap::new())
    }

    /// A service whose live streams and paced replays run on `clock`, with
    /// `grace` the graces of the calendars it names (D-5.1's configuration).
    pub fn with_clock(store: Store, sources: Sources, clock: Arc<dyn Clock>, grace: BTreeMap<String, i64>) -> Arc<DataService> {
        Arc::new(DataService {
            store: RwLock::new(store),
            window: SUBSCRIPTION_WINDOW,
            sources,
            clock,
            sessions: Sessions::default(),
            grace,
        })
    }

    /// The clock of the service's live streams.
    pub fn clock(&self) -> Arc<dyn Clock> {
        self.clock.clone()
    }

    fn grace(&self, calendar: &str) -> i64 {
        self.grace.get(calendar).copied().unwrap_or_else(|| live::default_grace(calendar))
    }

    /// Opens a live session (D-5.4): what its feed states before its first
    /// record (the names, every relation it records, empty, so that a
    /// demand binds the session's versions before they hold a row) ingested
    /// under the session's name, with `vendor`'s keys; and the bars the
    /// session plans (the base calendar's labels of the exchange's session),
    /// as `live.<session>.plan`.
    pub fn open_session(&self, name: &str, vendor: &str, prelude: &Fetched, plan: &[i64]) -> Result<Arc<Session>, DataError> {
        let mut st = self.store_mut();
        prelude.ingest(&mut st, vendor, name, &BTreeMap::new())?;
        let rows: Vec<(&str, i64, i64)> = plan.iter().map(|&l| ("base", l, l)).collect();
        let rel = live::plan_relation(name);
        st.put(&live::clock_schema(&rel, "live"), &[live::clock_rows(&rows)?], name)?;
        st.registry.save()?;
        drop(st);
        let s = self.sessions.open(name);
        s.state.lock().unwrap_or_else(|p| p.into_inner()).relations = prelude.relations.iter().map(|(r, _)| r.clone()).collect();
        Ok(s)
    }

    /// The session of a name.
    pub fn session(&self, name: &str) -> Option<Arc<Session>> {
        self.sessions.get(name)
    }

    /// Records a feed into the session (D-5.4), on a thread until the feed
    /// ends: every batch received at its acceptance (`r` the clock, never
    /// below the last row's and always past the last barrier emitted) and
    /// in the store before any subscriber is sent it.
    pub fn record(self: &Arc<Self>, name: &str, vendor: &str, mut feed: Box<dyn Feed>) -> live::Recording {
        let svc = self.clone();
        let (name, vendor) = (name.to_string(), vendor.to_string());
        std::thread::spawn(move || {
            let session = svc.sessions.open(&name);
            let run = (|| -> Result<(), DataError> {
                while let Some(mut f) = feed.next()? {
                    let mut state = session.state.lock().unwrap_or_else(|p| p.into_inner());
                    let r = svc.clock.now().max(state.last_r).max(state.last_at + 1);
                    live::stamp(&mut f, r)?;
                    let rows: usize = f.relations.iter().map(|(_, b)| b.num_rows()).sum();
                    f.ingest(&mut svc.store_mut(), &vendor, &name, &BTreeMap::new())?;
                    state.last_r = r;
                    state.batches += 1;
                    state.rows += rows as u64;
                    drop(state);
                    session.changed.notify_all();
                }
                Ok(())
            })();
            let mut state = session.state.lock().unwrap_or_else(|p| p.into_inner());
            state.ended = true;
            if let Err(e) = &run {
                state.error = Some(e.to_string());
            }
            let out = state.clone();
            drop(state);
            session.changed.notify_all();
            let _ = svc.store_mut().registry.save();
            run.map(|_| out)
        })
    }

    /// The catalog of the sources beside the store.
    pub fn catalog(&self) -> &crate::resolver::Catalog {
        &self.sources.catalog
    }

    /// The store, for the process that owns the service (an adapter run in
    /// process, a test).
    pub fn store(&self) -> RwLockReadGuard<'_, Store> {
        self.store.read().unwrap_or_else(|p| p.into_inner())
    }

    pub fn store_mut(&self) -> RwLockWriteGuard<'_, Store> {
        self.store.write().unwrap_or_else(|p| p.into_inner())
    }

    /// The binding of a demand (crate::resolver), recorded under its id
    /// unless the request is a dry run.
    /// A demand resolved (D-7.4, D-7.5): bought and bound unless a dry run;
    /// the binding recorded, its versions pinned.
    pub fn resolve(&self, demand: &abt_wire::generated::abt::wire::Demand, dry_run: bool) -> Result<Resolution, DataError> {
        let mut st = self.store_mut();
        let mut r = crate::resolver::resolve_with(&mut st, &self.sources, demand, dry_run)?;
        if !dry_run {
            let id = st.registry.record_binding(&r.binding)?;
            st.registry.save()?;
            r.binding.id = Some(id);
        }
        Ok(r)
    }

    /// `resolve_dynamic` (P-3.3): a chain the pre-pass did not cover, bought
    /// on demand, and the binding again with it, recorded.
    pub fn resolve_dynamic(&self, r: &abt_wire::generated::abt::wire::ResolveDynamicRequest) -> Result<abt_wire::generated::abt::wire::Binding, DataError> {
        let mut st = self.store_mut();
        let binding = match r.binding.id.as_deref() {
            Some(id) => st.registry.binding(id).ok_or_else(|| DataError::NotFound(format!("no binding {}", id)))?,
            None => (*r.binding).clone(),
        };
        let mut out = crate::resolver::prepass::resolve_dynamic(&mut st, &self.sources, &binding, &r.need, r.underlying, r.t)?;
        let id = st.registry.record_binding(&out)?;
        st.registry.save()?;
        out.id = Some(id);
        Ok(out)
    }

    fn stats(&self, relation: Option<&str>) -> Result<RecordBatch, DataError> {
        let st = self.store();
        let current: std::collections::BTreeSet<String> = st.registry.currents().into_iter().map(|(_, _, v)| v).collect();
        let mut recs: Vec<_> = st.registry.versions().filter(|v| relation.is_none_or(|r| v.relation == r)).cloned().collect();
        recs.sort_by(|a, b| (&a.relation, &a.source, &a.version).cmp(&(&b.relation, &b.source, &b.version)));
        let col = |f: &dyn Fn(&crate::registry::VersionRecord) -> String| -> ArrayRef { Arc::new(StringArray::from(recs.iter().map(f).collect::<Vec<_>>())) };
        Ok(RecordBatch::try_from_iter(vec![
            ("relation", col(&|v| v.relation.clone())),
            ("source", col(&|v| v.source.clone())),
            ("version", col(&|v| v.version.clone())),
            ("rows", Arc::new(UInt64Array::from(recs.iter().map(|v| v.rows).collect::<Vec<_>>())) as ArrayRef),
            ("from", Arc::new(TimestampNanosecondArray::from(recs.iter().map(|v| v.from).collect::<Vec<_>>())) as ArrayRef),
            ("to", Arc::new(TimestampNanosecondArray::from(recs.iter().map(|v| v.to).collect::<Vec<_>>())) as ArrayRef),
            (
                "current",
                Arc::new(BooleanArray::from(recs.iter().map(|v| current.contains(&v.version)).collect::<Vec<_>>())) as ArrayRef,
            ),
        ])?)
    }

    fn subscribe(&self, r: &SubscribeRequest, out: &Responder) -> Result<(), ErrorMessage> {
        if r.spec.clock == ClockSource::Live {
            return self.subscribe_live(r, out);
        }
        let (res, mut s) = {
            let st = self.store();
            let res = stream::resolve(&st, &r.spec).map_err(data)?;
            let s = Stream::open(&st, res.clone()).map_err(data)?;
            (res, s)
        };
        out.respond(Message::SubscribeResponse(Box::new(SubscribeResponse { stream: s.id.clone() }))).map_err(wire)?;
        if r.with_index {
            let ix = stream::index(&self.store(), &res).map_err(data)?;
            let t = stream::index_table(&res, &ix).map_err(data)?;
            out.table(&t.schema(), [t]).map_err(wire)?;
        }
        let mut sink = out.begin_table(&s.schema(), None).map_err(wire)?;
        let from = r.from_seq.max(1);
        let mut window = Window::new(self.window);
        // A paced replay (D-5.5) stops before each barrier until the clock
        // has gone as far past the first barrier's instant as the barrier is.
        let stops: Vec<(u64, i64)> = if r.paced {
            stream::index(&self.store(), &res)
                .map_err(data)?
                .into_iter()
                .map(|(_, b, seq)| (seq, b.at))
                .filter(|(seq, _)| *seq >= from)
                .collect()
        } else {
            Vec::new()
        };
        let start = (self.clock.now(), stops.first().map(|s| s.1));
        let mut sent = from - 1;
        let mut open = true;
        for &(seq, at) in &stops {
            if r.to_seq.is_some_and(|t| seq > t) {
                break;
            }
            // The tuples before the barrier as they come, the barrier at its
            // time.
            open = self.send_through(&mut s, out, &mut sink, &mut window, &mut sent, Some(seq - 1))?;
            if !open {
                break;
            }
            if let Some(first) = start.1 {
                self.clock.sleep_until(start.0 + (at - first));
            }
            open = self.send_through(&mut s, out, &mut sink, &mut window, &mut sent, Some(seq))?;
            if !open {
                break;
            }
        }
        if open {
            self.send_through(&mut s, out, &mut sink, &mut window, &mut sent, r.to_seq)?;
        }
        sink.end().map_err(wire)
    }

    /// Sends a stream's events after `sent` through `upto` (to its end when
    /// none); `false` when the subscriber cancelled or went away.
    fn send_through(&self, s: &mut Stream, out: &Responder, sink: &mut abt_wire::rpc::TableSink<'_>, window: &mut Window, sent: &mut u64, upto: Option<u64>) -> Result<bool, ErrorMessage> {
        loop {
            if out.cancelled() {
                return Ok(false);
            }
            if upto.is_some_and(|u| *sent >= u) {
                return Ok(true);
            }
            let Some(b) = s.next_batch(&self.store(), *sent + 1, upto, BATCH_EVENTS.min(self.window)).map_err(data)? else {
                return Ok(true);
            };
            if !window.send(out, sink, &b)? {
                return Ok(false);
            }
            *sent = b.column(0).as_any().downcast_ref::<UInt64Array>().map_or(*sent, |q| q.value(q.len() - 1));
        }
    }

    /// A live stream (D-5.2 live): the barriers of the spec's clock
    /// relation (the subscriber's labels) emitted when the clock passes each
    /// label plus its calendar's grace, each at the instant it is emitted,
    /// after every row the session accepted before; the rows are the
    /// session's versions of the spec's relations as they grow. The barriers
    /// are recorded as `<namespace>.clock` (live::recorded_clock), so the
    /// merge of the session's versions with them is the same stream.
    fn subscribe_live(&self, r: &SubscribeRequest, out: &Responder) -> Result<(), ErrorMessage> {
        let name = r.spec.session.clone().ok_or_else(|| error(ErrorCode::Malformed, "a live stream names the session it follows"))?;
        let session = self.sessions.get(&name).ok_or_else(|| error(ErrorCode::NotFound, format!("no live session `{}`", name)))?;
        let calendars = r.spec.calendars.clone().unwrap_or_default();
        let mut relations: Vec<String> = Vec::new();
        let mut planned_rel: Option<(String, String)> = None;
        for v in r.spec.relations.iter().flatten() {
            if stream::is_clock(&v.relation) {
                planned_rel = Some((v.relation.clone(), v.version.clone()));
            } else {
                relations.push(v.relation.clone());
            }
        }
        relations.sort();
        relations.dedup();
        let (planned_name, planned_version) = planned_rel.ok_or_else(|| error(ErrorCode::Malformed, "a live stream names the clock relation of its labels"))?;
        // The barriers due, in the order they are emitted: by the instant
        // they are due, then in the spec's order of calendars.
        let mut due: Vec<(i64, usize, i64)> = Vec::new();
        {
            let st = self.store();
            let rows = st.read_all(&planned_version).map_err(data)?;
            let pos = |n: &str| rows.schema().index_of(n).map_err(|e| error(ErrorCode::Malformed, e.to_string()));
            let (cc, lc) = (pos("calendar")?, pos("label")?);
            let cals = rows.column(cc).as_primitive::<arrow::datatypes::UInt32Type>();
            let labels = rows.column(lc).as_primitive::<arrow::datatypes::TimestampNanosecondType>();
            for i in 0..rows.num_rows() {
                let cal = st.registry.label_text(cals.value(i)).unwrap_or("");
                if let Some(k) = calendars.iter().position(|c| c == cal) {
                    due.push((labels.value(i) + self.grace(cal), k, labels.value(i)));
                }
            }
            for rel in &relations {
                if st.registry.current(rel, Some(&name)).is_none() {
                    return Err(error(ErrorCode::NotFound, format!("the session `{}` records no `{}`", name, rel)));
                }
            }
        }
        due.sort();
        let recorded = live::recorded_clock(&planned_name);
        let clock_schema = live::clock_schema(&recorded, "kernel");
        out.respond(Message::SubscribeResponse(Box::new(SubscribeResponse {
            stream: format!("live:{}:{}", name, recorded),
        })))
        .map_err(wire)?;
        // The events' schema is the relations': known before any barrier.
        let current = |st: &Store| -> Vec<(String, String)> { relations.iter().filter_map(|rel| st.registry.current(rel, Some(&name)).map(|v| (rel.clone(), v.to_string()))).collect() };
        let schema = {
            let st = self.store();
            let res = Resolved {
                relations: current(&st),
                clocks: vec![],
                calendars: calendars.clone(),
                r_max: None,
            };
            Stream::open(&st, res).map_err(data)?.schema()
        };
        let mut sink = out.begin_table(&schema, None).map_err(wire)?;
        let mut window = Window::new(self.window);
        let mut sent = 0u64;
        for (at_due, k, label) in due {
            if out.cancelled() {
                break;
            }
            self.clock.sleep_until(at_due);
            // The barrier, at the instant it is emitted: after every row
            // accepted so far, before every row accepted after.
            let version = {
                let mut state = session.state.lock().unwrap_or_else(|p| p.into_inner());
                let at = self.clock.now().max(state.last_r);
                state.last_at = state.last_at.max(at);
                let rows = live::clock_rows(&[(calendars[k].as_str(), label, at)]).map_err(data)?;
                let (v, _) = self.store_mut().put(&clock_schema, &[rows], "live").map_err(data)?;
                v
            };
            let res = Resolved {
                relations: current(&self.store()),
                clocks: vec![version],
                calendars: calendars.clone(),
                r_max: None,
            };
            let last = stream::index(&self.store(), &res).map_err(data)?.last().map_or(0, |(_, _, seq)| *seq);
            let mut s = Stream::open(&self.store(), res).map_err(data)?;
            if !self.send_through(&mut s, out, &mut sink, &mut window, &mut sent, Some(last))? {
                break;
            }
        }
        let _ = self.store_mut().registry.save();
        sink.end().map_err(wire)
    }
}

impl Service for DataService {
    fn layer(&self) -> Layer {
        Layer::Data
    }

    fn handle(&self, request: Request, out: &Responder) -> Result<(), ErrorMessage> {
        match request.message {
            Message::SchemaRequest(r) => {
                let st = self.store();
                let schemas = match &r.relation {
                    Some(rel) => vec![st.schema(rel).map_err(data)?],
                    None => st.registry.schemas(r.class.as_deref()),
                };
                out.respond(Message::SchemaResponse(Box::new(SchemaResponse { schemas: Some(schemas) }))).map_err(wire)
            }
            Message::ResolveRequest(r) => {
                let res = self.resolve(&r.demand, r.dry_run).map_err(data)?;
                let cost = res.plan.cost.clone();
                out.respond(Message::ResolveResponse(Box::new(ResolveResponse {
                    plan: Some(Box::new(res.plan)),
                    cost,
                    binding: Some(Box::new(res.binding)),
                    over_budget: res.over_budget.map(Box::new),
                    notes: (!res.notes.is_empty()).then_some(res.notes),
                })))
                .map_err(wire)
            }
            Message::ResolveDynamicRequest(r) => {
                let binding = self.resolve_dynamic(&r).map_err(data)?;
                out.respond(Message::ResolveDynamicResponse(Box::new(ResolveDynamicResponse { binding: Some(Box::new(binding)) })))
                    .map_err(wire)
            }
            Message::QueryRequest(r) => {
                let st = self.store();
                let view = ResolvedView::of(&st, r.view.as_deref()).map_err(data)?;
                if r.explain {
                    let e: ExplainResponse = query::explain(&st, &view, &r.form).map_err(data)?;
                    return out.respond(Message::ExplainResponse(Box::new(e))).map_err(wire);
                }
                let r_max = r.receipt_filter.as_ref().and_then(|f| f.r_max);
                let (schema, batches) = query::execute(&st, &view, &r.form, r_max, r.with_seq, r.ticks).map_err(data)?;
                drop(st);
                out.table(&schema, batches).map_err(wire)
            }
            Message::AggregateCall(r) => {
                let (key, batch) = {
                    let mut st = self.store_mut();
                    let view = ResolvedView::of(&st, r.view.as_deref()).map_err(data)?;
                    let (version, key) = agg::aggregate(&mut st, &view, &r.request).map_err(data)?;
                    (key, agg::answer(&st, &version).map_err(data)?)
                };
                out.respond(Message::AggregateResponse(Box::new(AggregateResponse { cache_key: key.hex() }))).map_err(wire)?;
                out.table(&batch.schema(), [batch]).map_err(wire)
            }
            Message::SubscribeRequest(r) => self.subscribe(&r, out),
            Message::StreamIndexRequest(r) => {
                let st = self.store();
                let res = stream::resolve(&st, &r.spec).map_err(data)?;
                let ix = stream::index(&st, &res).map_err(data)?;
                let t = stream::index_table(&res, &ix).map_err(data)?;
                drop(st);
                out.table(&t.schema(), [t]).map_err(wire)
            }
            Message::PutRequest(r) => {
                let (_, batches) = request.table.ok_or_else(|| error(ErrorCode::Malformed, "a put carries its rows"))?;
                let mut schema = *r.schema;
                schema.name = r.relation.clone();
                let mut st = self.store_mut();
                // A cache object (D-3.5): kept under its key, evictable by gc.
                if let Some(k) = &r.cache_key {
                    let key = abt_wire::Hash::from_hex(k).map_err(|e| error(ErrorCode::Malformed, e.0))?;
                    let version = st.cache_put(&key, &schema, &batches).map_err(data)?;
                    drop(st);
                    return out.respond(Message::PutResponse(Box::new(PutResponse { version, seq_from: 0, seq_to: 0 }))).map_err(wire);
                }
                let (version, (from, to)) = if r.replace { st.replace(&schema, &batches, "put") } else { st.put(&schema, &batches, "put") }.map_err(data)?;
                drop(st);
                out.respond(Message::PutResponse(Box::new(PutResponse { version, seq_from: from, seq_to: to }))).map_err(wire)
            }
            Message::GetRequest(r) => {
                let st = self.store();
                let version = match (&r.cache_key, &r.version, &r.relation) {
                    (Some(k), _, _) => {
                        let h = abt_wire::Hash::from_hex(k).map_err(|e| error(ErrorCode::Malformed, e.0))?;
                        st.cache_get(&h).ok_or_else(|| error(ErrorCode::NotFound, format!("no cache object {}", k)))?
                    }
                    (None, Some(v), _) => v.clone(),
                    (None, None, Some(rel)) => st
                        .registry
                        .current(rel, None)
                        .map(|v| v.to_string())
                        .ok_or_else(|| error(ErrorCode::NotFound, format!("`{}` has no current version", rel)))?,
                    _ => return Err(error(ErrorCode::Malformed, "a get names a cache key, a version or a relation")),
                };
                let b = agg::answer(&st, &version).map_err(data)?;
                drop(st);
                out.table(&b.schema(), [b]).map_err(wire)
            }
            Message::PinRequest(r) => {
                let mut st = self.store_mut();
                let by = r.binding.clone().map(|b| format!("binding:{}", b)).unwrap_or_else(|| "manual".to_string());
                let versions: Vec<String> = match (&r.version, &r.binding) {
                    (Some(v), _) => vec![v.clone()],
                    (None, Some(b)) => st
                        .registry
                        .binding(b)
                        .ok_or_else(|| error(ErrorCode::NotFound, format!("no binding {}", b)))?
                        .pairs
                        .unwrap_or_default()
                        .into_iter()
                        .map(|p| p.version)
                        .collect(),
                    _ => return Err(error(ErrorCode::Malformed, "a pin names a version or a binding")),
                };
                for v in versions {
                    if r.pin {
                        st.pin(&v, &by).map_err(data)?;
                    } else {
                        st.unpin(&v, &by).map_err(data)?;
                    }
                }
                drop(st);
                out.respond(Message::Done(Box::new(Done {}))).map_err(wire)
            }
            Message::StatsRequest(r) => {
                let b = self.stats(r.relation.as_deref()).map_err(data)?;
                out.table(&b.schema(), [b]).map_err(wire)
            }
            // ST-10.2: the segments of a hot relation. Nothing is hot until
            // the hot store lands (iteration 1, M-2); a known relation is
            // answered as having no segments, an unknown one as unknown.
            Message::MapRequest(r) => {
                self.store().schema(&r.relation).map_err(data)?;
                Err(error(ErrorCode::Malformed, format!("map: `{}` has no segments to map: no relation is held hot yet", r.relation)))
            }
            _ => Err(error(ErrorCode::Malformed, "the data service does not take this request")),
        }
    }
}

/// A connection to the service in this process: the service's end is served
/// on a thread of its own, the caller holds the other.
pub fn inproc(service: &Arc<DataService>) -> Connection {
    let (client, server) = channel_pair(INPROC_BOUND);
    let s: Arc<dyn Service> = service.clone();
    std::thread::spawn(move || {
        let _ = serve(server, s);
    });
    client
}

/// Serves the service on a Unix socket until the listener fails: one thread
/// per connection.
pub fn listen(path: &Path, service: Arc<DataService>) -> std::io::Result<std::thread::JoinHandle<()>> {
    let listener = bind(path)?;
    Ok(std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(stream) = stream else { break };
            let Ok(conn) = Connection::socket(stream) else { continue };
            let s: Arc<dyn Service> = service.clone();
            std::thread::spawn(move || {
                let _ = serve(conn, s);
            });
        }
    }))
}
