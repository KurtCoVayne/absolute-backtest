//! The data service (P-3.1): what each request of the data layer does, over
//! the exchange `abt-wire` speaks, in process or on a Unix socket (P-2.1).
//! A request reads the store under a shared lock and a write (`put`, an
//! aggregate's cache object, a binding, a pin) under an exclusive one; a
//! subscription takes the shared lock per batch, so a long replay never
//! holds writers off for its whole length.
//!
//! The resolver of M-2 is transitional (docs/spec/05-migration.md): it binds
//! every relation a demand names, qualified by the demand's class, at its
//! current version, with the registry's identity relations; D-7's cost model
//! and plans come with M-6.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::{Arc, RwLock, RwLockReadGuard, RwLockWriteGuard};

use abt_wire::generated::abt::wire::{AggregateResponse, Binding, Done, ExplainResponse, Layer, Message, PutResponse, ResolveResponse, SchemaResponse, SubscribeResponse, VersionRef};
use abt_wire::rpc::{error, serve, Request, Responder, Service, WireError};
use abt_wire::transport::{bind, channel_pair, Connection};
use abt_wire::{ErrorCode, ErrorMessage};
use arrow::array::{ArrayRef, BooleanArray, RecordBatch, StringArray, TimestampNanosecondArray, UInt64Array};

use crate::adapter::REGISTRY_SOURCE;
use crate::agg;
use crate::catalog::qualified;
use crate::error::DataError;
use crate::query::{self, ResolvedView};
use crate::store::Store;
use crate::stream::{self, Stream, BATCH_EVENTS};

/// Frames in flight per direction of an in-process connection.
const INPROC_BOUND: usize = 64;

/// The registry's relations a binding always carries: what the kernel reads
/// an instrument's key, ticker spans and contract terms from.
const IDENTITY: [&str; 3] = ["instrument", "vendor_id", "ticker"];

pub struct DataService {
    store: RwLock<Store>,
}

fn wire(e: WireError) -> ErrorMessage {
    error(ErrorCode::Internal, e.to_string())
}

fn data(e: DataError) -> ErrorMessage {
    e.to_wire()
}

impl DataService {
    pub fn new(store: Store) -> Arc<DataService> {
        Arc::new(DataService { store: RwLock::new(store) })
    }

    /// The store, for the process that owns the service (an adapter run in
    /// process, a test).
    pub fn store(&self) -> RwLockReadGuard<'_, Store> {
        self.store.read().unwrap_or_else(|p| p.into_inner())
    }

    pub fn store_mut(&self) -> RwLockWriteGuard<'_, Store> {
        self.store.write().unwrap_or_else(|p| p.into_inner())
    }

    /// The binding of the transitional resolver: every relation the demand
    /// names (each need's relations, or all of its class's when it names
    /// none) at its current version, and the registry's identity relations.
    fn resolve(&self, demand: &abt_wire::generated::abt::wire::Demand, dry_run: bool) -> Result<Binding, DataError> {
        let mut pairs: BTreeMap<String, String> = BTreeMap::new();
        let mut st = self.store_mut();
        let named: Vec<String> = demand.needs.iter().flatten().flat_map(|n| n.relations.iter().cloned()).collect();
        let wanted: Vec<String> = if named.is_empty() {
            st.registry.schemas(Some(&demand.class)).into_iter().map(|s| s.name).collect()
        } else {
            named.iter().map(|r| if r.contains('.') { r.clone() } else { qualified(&demand.class, r) }).collect()
        };
        for rel in wanted {
            let Some(by) = st
                .registry
                .currents()
                .into_iter()
                .filter(|(r, _, _)| *r == rel)
                .map(|(_, s, v)| (s, v))
                .collect::<Vec<_>>()
                .into_iter()
                .reduce(|a, b| if a.0 <= b.0 { a } else { b })
            else {
                continue;
            };
            pairs.insert(rel, by.1);
        }
        for rel in IDENTITY {
            if let Some(v) = st.registry.current(rel, Some(REGISTRY_SOURCE)) {
                pairs.insert(rel.to_string(), v.to_string());
            }
        }
        let mut b = Binding {
            pairs: Some(pairs.into_iter().map(|(relation, version)| VersionRef { relation, version }).collect()),
            point: None,
            dynamic: None,
            id: None,
        };
        if !dry_run {
            let id = st.registry.record_binding(&b)?;
            st.registry.save()?;
            b.id = Some(id);
        }
        Ok(b)
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

    fn subscribe(&self, r: &abt_wire::generated::abt::wire::SubscribeRequest, out: &Responder) -> Result<(), ErrorMessage> {
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
        loop {
            if out.cancelled() {
                break;
            }
            let b = s.next_batch(&self.store(), from, r.to_seq, BATCH_EVENTS).map_err(data)?;
            match b {
                Some(b) => sink.batch(&b).map_err(wire)?,
                None => break,
            }
        }
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
                let b = self.resolve(&r.demand, r.dry_run).map_err(data)?;
                out.respond(Message::ResolveResponse(Box::new(ResolveResponse {
                    plan: None,
                    cost: None,
                    binding: Some(Box::new(b)),
                    over_budget: None,
                })))
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
                let (schema, batches) = query::execute(&st, &view, &r.form, r_max, r.with_seq).map_err(data)?;
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
                let (version, (from, to)) = st.put(&schema, &batches, "put").map_err(data)?;
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
