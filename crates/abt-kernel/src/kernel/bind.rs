//! The Bind stage (K-4.1) and the kernel's client of the data layer: the
//! one place the kernel talks to another layer, through the protocol (A-D3).
//! Bind sends the program's demand and receives the binding, then reads
//! what the kernel needs before the stream: the binding's instruments with
//! their keys, ticker spans and contract terms (E-34), the labels its
//! relations hold, and its calendars' sources (the keys of the relations
//! read at their own bars, the sessions, the prints a session-offset clock
//! is placed on). The kernel then puts its calendars' labels and barrier
//! instants as `kernel.<run>.calendar_label` (E-32) and subscribes; the
//! fold consumes the stream's event batches.
//!
//! A caller holding a dataset in memory (the command line on a bundle, a
//! study, a test) reaches a data service through the provider the front
//! registers, which links the data layer the kernel may not depend on
//! (ERRATA E-36).

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::rc::Rc;
use std::sync::{Arc, OnceLock};

use abt_wire::generated::abt::wire::{
    AggregateCall, AggregateRequest, Attribute, Binding, Budget, Catalog, ChainFilters, ColumnSpec, ColumnType, Demand, Distinct, DynamicNeed, Layer, Message, Need, ParamValue, PlanStep, Preference,
    PreferenceKind, PutRequest, QueryForm, QueryRequest, RelationSchema, ResolveRequest, Role, Scan, SchemaRequest, StatsRequest, StreamIndexRequest, StreamSpec, SubscribeRequest, VersionRef, View,
};
use abt_wire::rpc::{Client, Reply, WireError};
use abt_wire::transport::Connection;
use arrow::array::{Array, ArrayRef, AsArray, Float64Array, Int64Array, RecordBatch, StringArray, TimestampNanosecondArray};
use arrow::datatypes::{DataType, Field, Schema, TimeUnit, UInt32Type};

use super::{time, Contract, Dataset, Kernel, RunError, RunWarning, Security, Sym, Symbols, Tuple, Value};
use crate::check::Program;
use crate::ir::{Cal, Kind, Ty};

fn wire(e: WireError) -> RunError {
    RunError::Config(format!("the data service: {}", e))
}

/// The kernel's client of a data service.
pub struct DataClient {
    client: Client,
    /// How to open another connection to the same service, when the client
    /// knows (what reads a run's outputs beside the run, E-66).
    connector: Option<Connector>,
}

/// One barrier of a stream's index: its calendar, label, instant and seq.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IndexRow {
    pub calendar: String,
    pub label: i64,
    pub at: i64,
    pub seq: u64,
}

/// A subscription being read.
pub struct Subscription {
    reply: Reply,
    pub stream: String,
}

impl Subscription {
    pub fn next_batch(&mut self) -> Result<Option<RecordBatch>, RunError> {
        self.reply.next_batch().map_err(wire)
    }
}

impl DataClient {
    pub fn connect(conn: Connection) -> Result<DataClient, RunError> {
        Ok(DataClient {
            client: Client::connect(conn, Layer::Kernel, "abt-kernel").map_err(wire)?,
            connector: None,
        })
    }

    /// A client over a connection `connector` opens, which can open more.
    pub fn connect_with(connector: Connector) -> Result<DataClient, RunError> {
        let mut c = DataClient::connect(connector().map_err(RunError::Config)?)?;
        c.connector = Some(connector);
        Ok(c)
    }

    /// Another connection to the service this client reads, when it knows
    /// how to open one.
    pub fn reconnect(&self) -> Option<Result<Connection, String>> {
        self.connector.as_ref().map(|c| c())
    }

    pub fn request(&self, m: Message) -> Result<Message, RunError> {
        self.client.request(m).map_err(wire)
    }

    pub fn table(&self, m: Message) -> Result<Vec<RecordBatch>, RunError> {
        self.client.request_table(m).map(|(_, b)| b).map_err(wire)
    }

    pub fn schemas(&self, class: Option<&str>) -> Result<Vec<RelationSchema>, RunError> {
        match self.request(Message::SchemaRequest(Box::new(SchemaRequest {
            class: class.map(str::to_string),
            relation: None,
        })))? {
            Message::SchemaResponse(r) => Ok(r.schemas.unwrap_or_default()),
            _ => Err(RunError::Config("the data service answered a schema request with another message".into())),
        }
    }

    /// The binding of a demand (D-7.2), with the plan's steps (the version
    /// each need's relations are bound at, its span and its attributes)
    /// and the resolver's notes (what it bought, what it could not cover).
    pub fn resolve(&self, demand: Demand) -> Result<(Binding, Vec<PlanStep>, Vec<String>), RunError> {
        let answer = self.client.request(Message::ResolveRequest(Box::new(ResolveRequest {
            demand: Box::new(demand),
            dry_run: false,
        })));
        let answer = match answer {
            Err(WireError::Remote(e)) if matches!(e.code, abt_wire::ErrorCode::Unresolved | abt_wire::ErrorCode::OutOfBudget) => return Err(RunError::Unbound(e.message.unwrap_or_default())),
            other => other.map_err(wire)?,
        };
        match answer {
            Message::ResolveResponse(r) => {
                let steps = r.plan.and_then(|p| p.steps).unwrap_or_default();
                let binding = r.binding.map(|b| *b).ok_or_else(|| RunError::Config("the data service resolved no binding".into()))?;
                Ok((binding, steps, r.notes.unwrap_or_default()))
            }
            _ => Err(RunError::Config("the data service answered a resolve with another message".into())),
        }
    }

    /// An aggregate the data layer's pass computes (D-6) over a view: the
    /// cache key and the answer's rows.
    pub fn aggregate(&self, view: &View, request: AggregateRequest) -> Result<(String, Vec<RecordBatch>), RunError> {
        let mut reply = self
            .client
            .call(Message::AggregateCall(Box::new(AggregateCall {
                view: Some(Box::new(view.clone())),
                request: Box::new(request),
            })))
            .map_err(wire)?;
        let key = match reply.message().map_err(wire)? {
            Message::AggregateResponse(r) => r.cache_key,
            _ => return Err(RunError::Config("the data service answered an aggregate with another message".into())),
        };
        let Message::TableFollows(_) = reply.message().map_err(wire)? else {
            return Err(RunError::Config("the data service sent an aggregate's key without its answer".into()));
        };
        let (_, batches) = reply.table().map_err(wire)?;
        Ok((key, batches))
    }

    pub fn query(&self, view: &View, form: QueryForm) -> Result<Vec<RecordBatch>, RunError> {
        self.table(Message::QueryRequest(Box::new(QueryRequest {
            view: Some(Box::new(view.clone())),
            form,
            receipt_filter: None,
            with_seq: false,
            explain: false,
        })))
    }

    /// Stores rows as a cache object under `key` (D-3.5); returns the version.
    pub fn put_cache(&self, key: &str, schema: RelationSchema, rows: &[RecordBatch]) -> Result<String, RunError> {
        self.put_as(schema, rows, Some(key.to_string()), false)
    }

    /// A cache object's rows, by its key (none when the data layer has no
    /// object under it, or evicted it).
    pub fn get_cache(&self, key: &str) -> Result<Option<Vec<RecordBatch>>, RunError> {
        let m = Message::GetRequest(Box::new(abt_wire::generated::abt::wire::GetRequest {
            cache_key: Some(key.to_string()),
            version: None,
            relation: None,
        }));
        match self.client.request_table(m) {
            Ok((_, b)) => Ok(Some(b)),
            Err(WireError::Remote(e)) if e.code == abt_wire::ErrorCode::NotFound => Ok(None),
            Err(e) => Err(wire(e)),
        }
    }

    /// Writes rows under the kernel's namespace (D-3.6), added to the
    /// relation's; returns the version.
    pub fn put(&self, schema: RelationSchema, rows: &[RecordBatch]) -> Result<String, RunError> {
        self.put_as(schema, rows, None, false)
    }

    /// Writes rows under the kernel's namespace as the relation's next
    /// version whole (E-56); returns the version.
    pub fn replace(&self, schema: RelationSchema, rows: &[RecordBatch]) -> Result<String, RunError> {
        self.put_as(schema, rows, None, true)
    }

    fn put_as(&self, schema: RelationSchema, rows: &[RecordBatch], cache_key: Option<String>, replace: bool) -> Result<String, RunError> {
        let arrow_schema = rows.first().map(|b| b.schema()).ok_or_else(|| RunError::Internal("a put of no batch".into()))?;
        let relation = schema.name.clone();
        let mut reply = self
            .client
            .call_with_table(
                Message::PutRequest(Box::new(PutRequest {
                    relation,
                    schema: Box::new(schema),
                    attributes: None,
                    cache_key,
                    replace,
                })),
                &arrow_schema,
                rows,
            )
            .map_err(wire)?;
        match reply.message().map_err(wire)? {
            Message::PutResponse(p) => Ok(p.version),
            _ => Err(RunError::Config("the data service answered a put with another message".into())),
        }
    }

    pub fn subscribe(&self, spec: StreamSpec, from: u64, to: Option<u64>) -> Result<Subscription, RunError> {
        self.subscribe_paced(spec, from, to, false)
    }

    /// A subscription; `paced`, a replay sent at the data layer's clock's
    /// pace (D-5.5).
    pub fn subscribe_paced(&self, spec: StreamSpec, from: u64, to: Option<u64>, paced: bool) -> Result<Subscription, RunError> {
        let mut reply = self
            .client
            .call(Message::SubscribeRequest(Box::new(SubscribeRequest {
                spec: Box::new(spec),
                from_seq: from,
                paced,
                to_seq: to,
                with_index: false,
            })))
            .map_err(wire)?;
        let stream = match reply.message().map_err(wire)? {
            Message::SubscribeResponse(r) => r.stream,
            _ => return Err(RunError::Config("the data service answered a subscription with another message".into())),
        };
        match reply.message().map_err(wire)? {
            Message::TableFollows(_) => Ok(Subscription { reply, stream }),
            _ => Err(RunError::Config("a subscription's events did not follow".into())),
        }
    }

    pub fn stream_index(&self, spec: StreamSpec) -> Result<Vec<IndexRow>, RunError> {
        let mut out = Vec::new();
        for b in self.table(Message::StreamIndexRequest(Box::new(StreamIndexRequest { spec: Box::new(spec) })))? {
            let cal = b.column(0).as_dictionary::<UInt32Type>();
            let names = cal.values().as_string::<i32>();
            let label = b.column(1).as_primitive::<arrow::datatypes::TimestampNanosecondType>();
            let at = b.column(2).as_primitive::<arrow::datatypes::TimestampNanosecondType>();
            let seq = b.column(3).as_primitive::<arrow::datatypes::UInt64Type>();
            for i in 0..b.num_rows() {
                out.push(IndexRow {
                    calendar: names.value(cal.keys().value(i) as usize).to_string(),
                    label: label.value(i),
                    at: at.value(i),
                    seq: seq.value(i),
                });
            }
        }
        Ok(out)
    }

    /// Every version's row count and span (the `stats` table).
    pub fn stats(&self) -> Result<Vec<RecordBatch>, RunError> {
        self.table(Message::StatsRequest(Box::new(StatsRequest { relation: None })))
    }
}

/// Opens a connection to one data service.
pub type Connector = std::sync::Arc<dyn Fn() -> Result<Connection, String> + Send + Sync>;

/// How a caller holding a dataset in memory reaches a data service holding
/// it: the front registers the data layer's in-process service (E-36),
/// which a connector reaches as often as asked.
pub type Provider = dyn Fn(&Dataset, &Program) -> Result<Connector, String> + Send + Sync;

static PROVIDER: OnceLock<Box<Provider>> = OnceLock::new();

/// Registers the provider; the first registration wins.
pub fn install_provider(p: Box<Provider>) -> bool {
    PROVIDER.set(p).is_ok()
}

/// A client of a data service holding `ds`.
pub fn connect_dataset(ds: &Dataset, prog: &Program) -> Result<DataClient, RunError> {
    let p = PROVIDER
        .get()
        .ok_or_else(|| RunError::Config("no data service: the kernel reads its data through the data layer, and no in-process service is registered (abt::install, or --data SOCKET)".into()))?;
    DataClient::connect_with(p(ds, prog).map_err(RunError::Config)?)
}

/// The short name of a stored relation (`equity.close` is `close`).
fn short(stored: &str) -> &str {
    stored.rsplit_once('.').map(|(_, s)| s).unwrap_or(stored)
}

/// The column of a schema with a role.
fn column_of(s: &RelationSchema, role: Role) -> Option<&ColumnSpec> {
    s.columns.iter().find(|c| c.role == role)
}

/// What the kernel holds of the data layer before the stream (K-4.1).
#[derive(Clone, Debug)]
pub struct Bound {
    pub binding: Binding,
    pub view: View,
    pub class: String,
    /// The program's primitive relations bound, by name, with the store's
    /// name and schema.
    pub stored: BTreeMap<String, (String, RelationSchema)>,
    /// Every relation of the program's class the binding holds (what the
    /// data has, whether or not the program reads it).
    pub present: BTreeSet<String>,
    /// The instruments by their keys, with the data layer's ids.
    pub symbols: Symbols,
    pub ids: Vec<u64>,
    pub labels: Symbols,
    pub securities: Vec<Security>,
    pub contracts: BTreeMap<Sym, Contract>,
    /// The last time any bound relation holds (the bundle date).
    pub last_time: Option<i64>,
    /// The keys of the relations read at their own bars (the base calendar),
    /// the sessions by instrument, and the prints a session-offset clock is
    /// placed on.
    pub base_bars: BTreeSet<i64>,
    pub sessions: Vec<(Sym, i64, i64)>,
    pub prints: BTreeMap<Sym, BTreeSet<i64>>,
    /// What the binding's preconditions warn of (K-3.2): versions no
    /// reviewer vouched for under `allow unreviewed`, a span that starts
    /// after the need's `from`.
    pub warnings: Vec<RunWarning>,
    /// Each underlying's chain snapshots (D-2.4 `chain_index`): the
    /// contracts known at each instant, with the instant each row was
    /// received.
    pub chains: BTreeMap<Sym, BTreeMap<i64, Vec<(Sym, i64)>>>,
    /// Each futures root's contracts (D-2.4 `continuous`): from each bar
    /// on, the contract its series holds and the series' additive
    /// adjustment, with the bar's receipt.
    pub rolls: BTreeMap<Sym, Vec<Roll>>,
    /// Under `greeks data`, the binding's `greeks` rows by contract: `(t,
    /// r, [iv, delta, gamma, theta, vega, rho])`, admitted at a barrier by
    /// their receipt as the stream admits its rows (ERRATA E-71).
    pub greeks: BTreeMap<Sym, Vec<(i64, i64, [f64; 6])>>,
}

/// A bar of a futures root's continuous series and the contract it holds.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Roll {
    pub t: i64,
    pub r: i64,
    pub contract: Sym,
    pub adjustment: f64,
}

impl Bound {
    /// The security carrying `ticker` at `as_of` (the ticker is the key when
    /// the data has no security table).
    pub fn has_relation(&self, name: &str) -> bool {
        self.present.contains(name)
    }

    pub fn bundle_date(&self) -> Option<i64> {
        self.last_time
    }

    pub fn resolve_ticker(&self, ticker: &str, as_of: Option<i64>) -> Result<Sym, String> {
        let at = as_of.or(self.last_time).unwrap_or(0);
        super::resolve_ticker(&self.securities, ticker, at)
    }
}

/// One row of a bound stored relation read back through the data layer:
/// its instrument, its time, and its value columns after the time in the
/// schema's order, the numeric ones (a count as a number), the times and
/// the labels.
#[derive(Clone, Debug)]
pub struct ScanRow {
    pub sym: Option<Sym>,
    pub t: i64,
    pub nums: Vec<Option<f64>>,
    pub times: Vec<Option<i64>>,
    pub labels: Vec<Option<String>>,
}

impl Bound {
    /// The rows of a program relation as the binding holds it (a `scan`,
    /// filtered by `predicate` when one is given); none when the relation
    /// is not bound.
    pub fn scan(&self, client: &DataClient, relation: &str, predicate: Option<abt_wire::generated::abt::wire::Predicate>) -> Result<Vec<ScanRow>, RunError> {
        let Some((stored_rel, schema)) = self.stored.get(relation) else { return Ok(Vec::new()) };
        let by_id: HashMap<u64, Sym> = self.ids.iter().enumerate().map(|(s, &id)| (id, s as Sym)).collect();
        let entity = column_of(schema, Role::Entity).map(|c| c.name.clone());
        let time = column_of(schema, Role::EventTime).map(|c| c.name.clone()).unwrap_or_default();
        let values: Vec<&ColumnSpec> = schema.columns.iter().filter(|c| c.role == Role::Value).collect();
        let batches = client.query(
            &self.view,
            QueryForm::Scan(Box::new(Scan {
                relation: stored_rel.clone(),
                t0: i64::MIN,
                t1: i64::MAX,
                predicate: predicate.map(Box::new),
                columns: None,
            })),
        )?;
        let mut out = Vec::new();
        for b in batches {
            let ts = times(col(&b, &time)?);
            let ids = entity
                .as_ref()
                .and_then(|e| b.column_by_name(e))
                .and_then(|c| c.as_primitive_opt::<arrow::datatypes::UInt64Type>().cloned());
            let mut nums: Vec<Vec<Option<f64>>> = Vec::new();
            let mut tcols: Vec<Vec<Option<i64>>> = Vec::new();
            let mut lcols: Vec<Vec<Option<String>>> = Vec::new();
            for c in &values {
                let a = col(&b, &c.name)?;
                match c.ty {
                    ColumnType::Num => {
                        let x = a.as_primitive::<arrow::datatypes::Float64Type>();
                        nums.push((0..x.len()).map(|i| (!x.is_null(i)).then(|| x.value(i))).collect());
                    }
                    ColumnType::Count => {
                        let x = a.as_primitive::<arrow::datatypes::Int64Type>();
                        nums.push((0..x.len()).map(|i| (!x.is_null(i)).then(|| x.value(i) as f64)).collect());
                    }
                    ColumnType::Time => tcols.push(times(a)),
                    ColumnType::Label => lcols.push(strings(a)?),
                    _ => {}
                }
            }
            for (i, t) in ts.into_iter().enumerate() {
                let Some(t) = t else { continue };
                out.push(ScanRow {
                    sym: ids.as_ref().and_then(|c| by_id.get(&c.value(i)).copied()),
                    t,
                    nums: nums.iter().map(|c| c[i]).collect(),
                    times: tcols.iter().map(|c| c[i]).collect(),
                    labels: lcols.iter().map(|c| c[i].clone()).collect(),
                });
            }
        }
        Ok(out)
    }
}

/// A column of a batch as the strings it holds (a dictionary decoded).
fn strings(c: &ArrayRef) -> Result<Vec<Option<String>>, RunError> {
    let s = arrow::compute::cast(c, &DataType::Utf8).map_err(|e| RunError::Internal(e.to_string()))?;
    let s = s.as_string::<i32>();
    Ok((0..s.len()).map(|i| (!s.is_null(i)).then(|| s.value(i).to_string())).collect())
}

fn times(c: &ArrayRef) -> Vec<Option<i64>> {
    let t = c.as_primitive::<arrow::datatypes::TimestampNanosecondType>();
    (0..t.len()).map(|i| (!t.is_null(i)).then(|| t.value(i))).collect()
}

fn col<'b>(b: &'b RecordBatch, name: &str) -> Result<&'b ArrayRef, RunError> {
    b.column_by_name(name).ok_or_else(|| RunError::Internal(format!("the data service's answer has no column `{}`", name)))
}

/// The Bind stage: the binding of the program's demand and what the kernel
/// reads before the stream.
pub fn bind(prog: &Program, client: &DataClient) -> Result<Bound, RunError> {
    bind_at(prog, client, &point_of(prog, &[]))
}

/// The parameter point of a run (D-7.5): every parameter of the program at
/// its value, the run's override or the declared default, as its literal's
/// text, by name (`unit::name` for a library's), in name order.
pub fn point_of(prog: &Program, overrides: &[(String, crate::Lit)]) -> Vec<ParamValue> {
    let mut out = Vec::new();
    for (unit, ps) in &prog.params {
        for (name, p) in ps {
            let key = if *unit == prog.strategy { name.clone() } else { format!("{}::{}", unit, name) };
            let value = overrides.iter().rev().find(|(n, _)| *n == key).map(|(_, l)| l).unwrap_or(&p.value);
            out.push(ParamValue { name: key, value: value.to_string() });
        }
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

/// `bind` at a parameter point: the binding carries it (D-7.5), so two
/// points are two bindings.
pub fn bind_at(prog: &Program, client: &DataClient, point: &[ParamValue]) -> Result<Bound, RunError> {
    let all = client.schemas(None)?;
    let by_name: BTreeMap<String, RelationSchema> = all.iter().map(|s| (s.name.clone(), s.clone())).collect();
    // The store's name of a relation: the program's class's, else the one
    // class that has it (the series class's `value`).
    let stored_name = |name: &str| -> Option<String> {
        let own = format!("{}.{}", prog.class, name);
        if by_name.contains_key(&own) {
            return Some(own);
        }
        if let Some(b) = borrowed(&prog.class, name).filter(|b| by_name.contains_key(b)) {
            return Some(b);
        }
        let mut others = by_name
            .keys()
            .filter(|n| short(n) == name && n.contains('.') && !n.starts_with("kernel.") && !n.starts_with("query.") && !n.starts_with("cache."));
        let first = others.next().cloned();
        first.filter(|_| others.next().is_none())
    };
    let primitives: Vec<&str> = prog.relations.iter().filter(|(_, s)| matches!(s.kind, Kind::Primitive { .. })).map(|(n, _)| n.as_str()).collect();
    let mut demand = demand_of(prog, &stored_name);
    demand.point = Some(point.to_vec());
    let (binding, steps, notes) = client.resolve(demand)?;
    let mut warnings = binding_warnings(prog, &steps);
    // What the resolver bought, or could not cover, is stated with the run.
    warnings.extend(notes.into_iter().map(|message| RunWarning { bias: "binding".into(), message }));
    let view = View {
        versions: None,
        binding: binding.id.clone(),
        as_of_seq: None,
    };
    let pairs: BTreeMap<String, String> = binding.pairs.clone().unwrap_or_default().into_iter().map(|p| (p.relation, p.version)).collect();
    // What the data holds of the class, bound or not: the relations whose
    // current version has rows (a run warns of actions it holds and does
    // not load).
    let mut present: BTreeSet<String> = BTreeSet::new();
    for b in client.stats()? {
        let rel = strings(col(&b, "relation")?)?;
        let current = col(&b, "current")?.as_boolean().clone();
        let rows = col(&b, "rows")?.as_primitive::<arrow::datatypes::UInt64Type>().clone();
        for (i, r) in rel.iter().enumerate() {
            if let Some(r) = r {
                if current.value(i) && rows.value(i) > 0 && r.starts_with(&format!("{}.", prog.class)) {
                    present.insert(short(r).to_string());
                }
            }
        }
    }
    let mut stored = BTreeMap::new();
    for p in &primitives {
        if let Some(n) = stored_name(p) {
            if pairs.contains_key(&n) {
                stored.insert(p.to_string(), (n.clone(), by_name[&n].clone()));
            }
        }
    }
    // Instruments: their keys (vendor_id), class and contract terms.
    let mut keys: BTreeMap<u64, String> = BTreeMap::new();
    if pairs.contains_key("vendor_id") {
        for b in client.query(
            &view,
            QueryForm::Catalog(Box::new(Catalog {
                relation: "vendor_id".into(),
                predicate: None,
            })),
        )? {
            let ids = col(&b, "id")?.as_primitive::<arrow::datatypes::UInt64Type>().clone();
            let k = strings(col(&b, "key")?)?;
            for i in 0..b.num_rows() {
                if let Some(k) = &k[i] {
                    keys.entry(ids.value(i)).or_insert_with(|| k.clone());
                }
            }
        }
    }
    let mut symbols = Symbols::new();
    let mut ids: Vec<u64> = Vec::new();
    let mut by_id: HashMap<u64, Sym> = HashMap::new();
    let mut contracts = BTreeMap::new();
    if pairs.contains_key("instrument") {
        for b in client.query(
            &view,
            QueryForm::Catalog(Box::new(Catalog {
                relation: "instrument".into(),
                predicate: None,
            })),
        )? {
            let id_col = col(&b, "id")?.as_primitive::<arrow::datatypes::UInt64Type>().clone();
            let class = strings(col(&b, "class")?)?;
            let mult = col(&b, "multiplier")?.as_primitive::<arrow::datatypes::Float64Type>().clone();
            let comm = col(&b, "commission_per_contract")?.as_primitive::<arrow::datatypes::Float64Type>().clone();
            for (i, class) in class.iter().enumerate() {
                let id = id_col.value(i);
                if by_id.contains_key(&id) {
                    continue;
                }
                let key = keys.get(&id).cloned().unwrap_or_else(|| id.to_string());
                let sym = symbols.intern(&key);
                by_id.insert(id, sym);
                if ids.len() <= sym as usize {
                    ids.resize(sym as usize + 1, 0);
                }
                ids[sym as usize] = id;
                if !mult.is_null(i) {
                    contracts.insert(
                        sym,
                        Contract {
                            multiplier: mult.value(i),
                            future: class.as_deref() == Some("future"),
                            commission_per_contract: (!comm.is_null(i)).then(|| comm.value(i)),
                            ..Contract::default()
                        },
                    );
                }
            }
        }
    }
    // The security table, as ticker spans.
    let mut securities = Vec::new();
    if pairs.contains_key("ticker") {
        for b in client.query(
            &view,
            QueryForm::Catalog(Box::new(Catalog {
                relation: "ticker".into(),
                predicate: None,
            })),
        )? {
            let id_col = col(&b, "id")?.as_primitive::<arrow::datatypes::UInt64Type>().clone();
            let from = times(col(&b, "t")?);
            let to = times(col(&b, "to")?);
            let sym = strings(col(&b, "symbol")?)?;
            for i in 0..b.num_rows() {
                let Some(&s) = by_id.get(&id_col.value(i)) else { continue };
                securities.push(Security {
                    id: s,
                    ticker: sym[i].clone().unwrap_or_default(),
                    from: from[i].unwrap_or(i64::MIN),
                    to: to[i],
                });
            }
        }
        securities.sort_by_key(|s| (s.id, s.from));
    }
    // The labels the bound relations hold.
    let mut labels = Symbols::new();
    for (stored_rel, version) in &pairs {
        let Some(s) = by_name.get(stored_rel) else { continue };
        if !(stored_rel.starts_with(&format!("{}.", prog.class)) || stored.values().any(|(n, _)| n == stored_rel)) {
            continue;
        }
        let _ = version;
        for c in s.columns.iter().filter(|c| c.ty == ColumnType::Label) {
            let form = QueryForm::Distinct(Box::new(Distinct {
                relation: stored_rel.clone(),
                column: c.name.clone(),
                t0: i64::MIN,
                t1: i64::MAX,
            }));
            for b in client.query(&view, form)? {
                for l in strings(b.column(0))?.into_iter().flatten() {
                    labels.intern(&l);
                }
            }
        }
    }
    // The bundle date: the last time of any bound relation of the data.
    let bound_versions: BTreeSet<&String> = pairs.iter().filter(|(r, _)| !matches!(r.as_str(), "instrument" | "vendor_id" | "ticker")).map(|(_, v)| v).collect();
    let mut last_time: Option<i64> = None;
    for b in client.stats()? {
        let v = strings(col(&b, "version")?)?;
        let to = times(col(&b, "to")?);
        for i in 0..b.num_rows() {
            if v[i].as_ref().is_some_and(|v| bound_versions.contains(v)) {
                if let Some(t) = to[i] {
                    last_time = Some(last_time.map_or(t, |l: i64| l.max(t)));
                }
            }
        }
    }
    // The calendars' sources.
    let entity_sym = |b: &RecordBatch, name: &str, i: usize| -> Option<Sym> {
        b.column_by_name(name)
            .and_then(|c| c.as_primitive_opt::<arrow::datatypes::UInt64Type>())
            .and_then(|c| by_id.get(&c.value(i)).copied())
    };
    let mut base_bars = BTreeSet::new();
    for (name, (stored_rel, s)) in &stored {
        let sig = &prog.relations[name];
        if sig.cal != Cal::Base || sig.native.is_some() {
            continue;
        }
        let t = column_of(s, Role::EventTime).map(|c| c.name.clone()).unwrap_or_default();
        let form = QueryForm::Distinct(Box::new(Distinct {
            relation: stored_rel.clone(),
            column: t,
            t0: i64::MIN,
            t1: i64::MAX,
        }));
        for b in client.query(&view, form)? {
            base_bars.extend(times(b.column(0)).into_iter().flatten());
        }
    }
    let scan = |stored_rel: &str| -> Result<Vec<RecordBatch>, RunError> {
        client.query(
            &view,
            QueryForm::Scan(Box::new(Scan {
                relation: stored_rel.to_string(),
                t0: i64::MIN,
                t1: i64::MAX,
                predicate: None,
                columns: None,
            })),
        )
    };
    let mut sessions = Vec::new();
    if let Some((stored_rel, s)) = stored.get("session") {
        let e = column_of(s, Role::Entity).map(|c| c.name.clone()).unwrap_or_default();
        let t = column_of(s, Role::EventTime).map(|c| c.name.clone()).unwrap_or_default();
        let t_pos = s.columns.iter().position(|c| c.role == Role::EventTime).unwrap_or(0);
        let open = s
            .columns
            .iter()
            .skip(t_pos + 1)
            .find(|c| c.ty == ColumnType::Time && c.role != Role::ReceiptTime)
            .map(|c| c.name.clone());
        for b in scan(stored_rel)? {
            let labels_t = times(col(&b, &t)?);
            let opens = match &open {
                Some(o) => times(col(&b, o)?),
                None => vec![None; b.num_rows()],
            };
            for i in 0..b.num_rows() {
                if let (Some(sym), Some(open), Some(label)) = (entity_sym(&b, &e, i), opens[i], labels_t[i]) {
                    sessions.push((sym, open, label));
                }
            }
        }
    }
    let mut prints: BTreeMap<Sym, BTreeSet<i64>> = BTreeMap::new();
    if prog.clock.offset.is_some() {
        if let Some((stored_rel, s)) = stored.get("close") {
            let e = column_of(s, Role::Entity).map(|c| c.name.clone()).unwrap_or_default();
            let t = column_of(s, Role::EventTime).map(|c| c.name.clone()).unwrap_or_default();
            for b in scan(stored_rel)? {
                let ts = times(col(&b, &t)?);
                for (i, t) in ts.into_iter().enumerate() {
                    if let (Some(sym), Some(t)) = (entity_sym(&b, &e, i), t) {
                        prints.entry(sym).or_default().insert(t);
                    }
                }
            }
        }
    }
    // The registry's contract terms (D-2.4): an option's from
    // `option_spec`, its underlying's chains from `chain_index`, a futures
    // contract's from `future_spec`, a root's contracts from `continuous`.
    let catalog = |relation: &str| -> Result<Vec<RecordBatch>, RunError> {
        if !pairs.contains_key(relation) {
            return Ok(Vec::new());
        }
        client.query(
            &view,
            QueryForm::Catalog(Box::new(Catalog {
                relation: relation.into(),
                predicate: None,
            })),
        )
    };
    let ids_of = |b: &RecordBatch, name: &str| -> Result<Vec<Option<Sym>>, RunError> {
        let a = col(b, name)?.as_primitive::<arrow::datatypes::UInt64Type>().clone();
        Ok((0..a.len()).map(|i| (!a.is_null(i)).then(|| by_id.get(&a.value(i)).copied()).flatten()).collect())
    };
    let floats = |b: &RecordBatch, name: &str| -> Result<Vec<Option<f64>>, RunError> {
        let a = arrow::compute::cast(col(b, name)?, &DataType::Float64).map_err(|e| RunError::Internal(e.to_string()))?;
        let a = a.as_primitive::<arrow::datatypes::Float64Type>();
        Ok((0..a.len()).map(|i| (!a.is_null(i)).then(|| a.value(i))).collect())
    };
    for b in catalog("option_spec")? {
        let (syms, unds) = (ids_of(&b, "id")?, ids_of(&b, "underlying")?);
        let (listed, expiry) = (times(col(&b, "listed")?), times(col(&b, "expiry")?));
        let (strike, mult) = (floats(&b, "strike")?, floats(&b, "multiplier")?);
        let (right, style, settlement, occ, stime) = (
            strings(col(&b, "right")?)?,
            strings(col(&b, "style")?)?,
            strings(col(&b, "settlement")?)?,
            strings(col(&b, "occ")?)?,
            strings(col(&b, "settlement_time")?)?,
        );
        for i in 0..b.num_rows() {
            let (Some(sym), Some(underlying), Some(expiry), Some(strike)) = (syms[i], unds[i], expiry[i], strike[i]) else {
                continue;
            };
            let Some(right) = right[i].as_deref().and_then(crate::functions::Right::parse) else { continue };
            let multiplier = mult[i].unwrap_or(100.0);
            let terms = super::OptionTerms {
                underlying,
                right,
                strike,
                expiry,
                style: style[i].as_deref().and_then(crate::functions::Style::parse).unwrap_or(crate::functions::Style::American),
                cash: settlement[i].as_deref() == Some("cash"),
                am: stime[i].as_deref() == Some("am"),
                multiplier,
                occ: occ[i].clone().unwrap_or_default(),
                listed: listed[i].unwrap_or(i64::MIN),
            };
            let c = contracts.entry(sym).or_insert_with(|| Contract { multiplier, ..Contract::default() });
            c.multiplier = multiplier;
            c.option = Some(terms);
        }
    }
    let mut chains: BTreeMap<Sym, BTreeMap<i64, Vec<(Sym, i64)>>> = BTreeMap::new();
    for b in catalog("chain_index")? {
        let (unds, syms) = (ids_of(&b, "underlying")?, ids_of(&b, "id")?);
        let (ts, rs) = (times(col(&b, "t")?), times(col(&b, "r")?));
        for i in 0..b.num_rows() {
            if let (Some(u), Some(sym), Some(t)) = (unds[i], syms[i], ts[i]) {
                chains.entry(u).or_default().entry(t).or_default().push((sym, rs[i].unwrap_or(t)));
            }
        }
    }
    for b in catalog("future_spec")? {
        let syms = ids_of(&b, "id")?;
        let (expiry, last_trade) = (times(col(&b, "expiry")?), times(col(&b, "last_trade")?));
        let pv = floats(&b, "point_value")?;
        let (root, settlement) = (strings(col(&b, "root")?)?, strings(col(&b, "settlement")?)?);
        for i in 0..b.num_rows() {
            let (Some(sym), Some(expiry)) = (syms[i], expiry[i]) else { continue };
            let point_value = pv[i].filter(|x| x.is_finite()).unwrap_or(1.0);
            let c = contracts.entry(sym).or_insert_with(|| Contract {
                multiplier: point_value,
                future: true,
                ..Contract::default()
            });
            c.future_spec = Some(super::FutureSpec {
                root: root[i].clone().unwrap_or_default(),
                expiry,
                last_trade: last_trade[i],
                point_value,
                cash: settlement[i].as_deref() == Some("cash"),
            });
        }
    }
    let mut rolls: BTreeMap<Sym, Vec<Roll>> = BTreeMap::new();
    for b in catalog("continuous")? {
        let (roots, cons) = (ids_of(&b, "root_id")?, ids_of(&b, "contract_id")?);
        let (ts, rs) = (times(col(&b, "t")?), times(col(&b, "r")?));
        let adj = floats(&b, "adjustment")?;
        for i in 0..b.num_rows() {
            if let (Some(root), Some(contract), Some(t)) = (roots[i], cons[i], ts[i]) {
                rolls.entry(root).or_default().push(Roll {
                    t,
                    r: rs[i].unwrap_or(t),
                    contract,
                    adjustment: adj[i].unwrap_or(0.0),
                });
            }
        }
    }
    for v in rolls.values_mut() {
        v.sort_by_key(|r| r.t);
    }
    // `greeks data` (K-5.5.6): the binding's rows, read whole.
    let mut greeks: BTreeMap<Sym, Vec<(i64, i64, [f64; 6])>> = BTreeMap::new();
    if prog.exec.greeks.as_deref() == Some("data") {
        if let Some(stored_rel) = pairs.keys().find(|r| r.as_str() == "option.greeks").cloned() {
            for b in scan(&stored_rel)? {
                let syms = ids_of(&b, "A")?;
                let (ts, rs) = (times(col(&b, "T")?), times(col(&b, "r")?));
                let cols: Vec<Vec<Option<f64>>> = ["IV", "Delta", "Gamma", "Theta", "Vega", "Rho"].iter().map(|c| floats(&b, c)).collect::<Result<_, _>>()?;
                for i in 0..b.num_rows() {
                    let (Some(sym), Some(t)) = (syms[i], ts[i]) else { continue };
                    // A value the source does not carry reads as NaN (E-71).
                    let v: Vec<f64> = cols.iter().map(|c| c[i].unwrap_or(f64::NAN)).collect();
                    greeks.entry(sym).or_default().push((t, rs[i].unwrap_or(t), [v[0], v[1], v[2], v[3], v[4], v[5]]));
                }
            }
        }
        for v in greeks.values_mut() {
            v.sort_by_key(|x| (x.0, x.1));
        }
    }
    Ok(Bound {
        binding,
        view,
        class: prog.class.clone(),
        stored,
        present,
        symbols,
        ids,
        labels,
        securities,
        contracts,
        last_time,
        base_bars,
        sessions,
        prints,
        warnings,
        chains,
        rolls,
        greeks,
    })
}

/// Pushes a kernel's eligible aggregate literals down to the data layer
/// (K-4.5): one request each over the binding and the kernel's calendars,
/// walking the labels of the window variable's calendar with the program's
/// numerics; its answer is held by (instrument, label) for the evaluator. A
/// literal the layer cannot answer is evaluated here; the result says which
/// and why.
pub fn push_down(k: &mut Kernel, bound: &Bound, client: &DataClient, spec: &StreamSpec) -> Result<Vec<String>, RunError> {
    let mut notes = Vec::new();
    if !k.cfg.pushdown {
        return Ok(notes);
    }
    let Some(clock) = spec.relations.iter().flatten().find(|v| v.relation.ends_with(".calendar_label")).cloned() else {
        return Ok(notes);
    };
    let view = View {
        versions: Some(vec![clock]),
        binding: bound.binding.id.clone(),
        as_of_seq: None,
    };
    // The kernel's symbols, in its own order (as the stream's decoder maps them).
    let by_id: HashMap<u64, Sym> = bound.ids.iter().enumerate().map(|(s, &id)| (id, k.remap_syms[s])).collect();
    let numerics = if k.cfg.running_sums {
        abt_wire::generated::abt::wire::Numerics::Running
    } else {
        abt_wire::generated::abt::wire::Numerics::TwoPass
    };
    for ((ri, li), e) in super::pushdown::eligibilities(k.prog) {
        let Ok(shape) = e else { continue };
        let label = k.prog.rule_label(ri);
        let Some((stored_rel, schema)) = bound.stored.get(&shape.relation) else { continue };
        let values: Vec<&ColumnSpec> = schema.columns.iter().filter(|c| c.role != Role::ReceiptTime).collect();
        let cols: Vec<String> = shape.columns.iter().filter_map(|&p| values.get(p).map(|c| c.name.clone())).collect();
        if cols.len() != shape.columns.len() {
            continue;
        }
        let cr = k.compiled[ri].clone();
        let (Some(&entity_slot), Some(&base_slot)) = (cr.slots.get(&shape.entity_var), cr.slots.get(&shape.base_var)) else {
            continue;
        };
        // A length or minimum that does not evaluate leaves the window here,
        // to fail as the rule does.
        let (Ok(dur), Ok(min)) = (k.static_value(ri, &shape.dur), k.static_value(ri, &shape.min)) else {
            continue;
        };
        let min = min.as_f64().map(|m| m.max(0.0) as u64);
        let wcal = k.prog.var_cals[ri].get(&shape.window_var).cloned().unwrap_or(Cal::Base);
        use abt_wire::generated::abt::wire::WindowKind as W;
        let as_count = |v: &Value| v.as_f64().map(|x| x.max(0.0) as u64).unwrap_or(0);
        let as_span = |v: &Value| match v {
            Value::Dur(d) => Some(Box::new(abt_wire::generated::abt::wire::Duration {
                months: d.months,
                days: d.days,
                nanos: d.nanos,
            })),
            _ => None,
        };
        let (window, n, span, bucket) = match &shape.window {
            crate::ir::WindowKind::Rows => (W::Rows, as_count(&dur), None, None),
            crate::ir::WindowKind::PriorRows => (W::PriorRows, as_count(&dur), None, None),
            crate::ir::WindowKind::Window => (W::Span, 0, as_span(&dur), None),
            crate::ir::WindowKind::Prior => (W::PriorSpan, 0, as_span(&dur), None),
            crate::ir::WindowKind::Bucket(c) => (W::Bucket, 0, None, Some(c.to_string())),
            crate::ir::WindowKind::Session => continue,
        };
        if matches!(window, W::Span | W::PriorSpan) && span.is_none() {
            continue;
        }
        let request = AggregateRequest {
            relation: stored_rel.clone(),
            column: cols[0].clone(),
            second_column: cols.get(1).cloned(),
            function: shape.function.clone(),
            level: shape.level,
            window,
            n,
            min,
            span,
            bucket,
            group: abt_wire::generated::abt::wire::Grouping::PerEntity,
            group_column: None,
            receipt_filter: None,
            numerics,
            calendar: Some(wcal.to_string()),
            t0: None,
            t1: None,
            output: None,
        };
        let batches = match client.aggregate(&view, request) {
            Ok((_, b)) => b,
            Err(e) => {
                notes.push(format!("rule {}: the aggregate is evaluated here: {}", label, e));
                continue;
            }
        };
        let mut by_time: super::hash::FxHashMap<i64, Vec<(Sym, super::pushdown::Answer)>> = super::hash::FxHashMap::default();
        let mut unmapped = false;
        for b in &batches {
            let ids = col(b, "entity")?.as_primitive::<arrow::datatypes::UInt64Type>().clone();
            let ts = times(
                b.column_by_name("t")
                    .or_else(|| b.column_by_name("label"))
                    .ok_or_else(|| RunError::Internal("an aggregate answer without its time".into()))?,
            );
            let rs = times(col(b, "r")?);
            let value = col(b, "value")?.as_primitive::<arrow::datatypes::Float64Type>().clone();
            let count = col(b, "count")?.as_primitive::<arrow::datatypes::Int64Type>().clone();
            let error = col(b, "error")?.clone();
            for i in 0..b.num_rows() {
                let (Some(t), Some(r)) = (ts[i], rs[i]) else { continue };
                let Some(&sym) = by_id.get(&ids.value(i)) else {
                    unmapped = true;
                    continue;
                };
                by_time.entry(t).or_default().push((
                    sym,
                    super::pushdown::Answer {
                        value: (!value.is_null(i)).then(|| value.value(i)),
                        count: count.value(i),
                        error: !error.is_null(i),
                        r,
                    },
                ));
            }
        }
        // An instrument of the answer the binding does not name would be
        // missing from a section: the window is evaluated here.
        if unmapped {
            notes.push(format!("rule {}: the aggregate is evaluated here: its answer names an instrument the binding does not", label));
            continue;
        }
        for section in by_time.values_mut() {
            section.sort_by_key(|(s, _)| *s);
        }
        k.pushed.insert(
            (ri, li),
            Rc::new(super::pushdown::Pushed {
                shape,
                entity_slot,
                base_slot,
                by_time,
            }),
        );
    }
    Ok(notes)
}

/// The store's name of a relation a class reads from another class (D-2.3,
/// ERRATA E-70): the option class's underlyings are the equity class's
/// bars and actions.
pub fn borrowed(class: &str, name: &str) -> Option<String> {
    let equity = ["open", "high", "low", "close", "volume", "split", "dividend", "special_dividend", "delisted"];
    (class == "option" && equity.contains(&name)).then(|| format!("equity.{}", name))
}

/// The relations of a class each need of a data block admits (Definition
/// 3.11; `Demand::need_for`).
fn need_relations(word: &str) -> &'static [&'static str] {
    match word {
        "bars" => &["open", "high", "low", "close", "volume"],
        "actions" => &["split", "dividend", "special_dividend", "delisted"],
        "listing" => &["listed", "ticker", "usable"],
        "lists" => &["member", "classification"],
        "series" => &["value"],
        "sessions" => &["session"],
        "point_value" => &["point_value"],
        "commission" => &["commission"],
        _ => &[],
    }
}

/// The program's demand as the kernel sends it (D-7.1). One need per need
/// its data block writes, naming the stored relations the need admits that
/// the program holds as primitives, with the calendar and first time it
/// asks for and the attributes their versions must carry (K-3.2:
/// `reviewed = complete` for a `complete` need); `require reviewed` asks
/// every version for a `reviewed` attribute (an empty value: any). A
/// relation the store has no schema for is named in the program's class,
/// so that the resolver refuses it by name.
pub fn demand_of(prog: &Program, stored_name: &dyn Fn(&str) -> Option<String>) -> Demand {
    let d = &prog.demand;
    let mut needs = Vec::new();
    let mut need = |word: &str, label: String, calendar: Option<String>, from: Option<i64>, complete: bool| {
        let relations: Vec<String> = need_relations(word)
            .iter()
            .filter(|n| matches!(prog.relations.get(**n).map(|s| &s.kind), Some(Kind::Primitive { .. })))
            .map(|n| stored_name(n).unwrap_or_else(|| format!("{}.{}", prog.class, n)))
            .collect();
        if relations.is_empty() {
            return;
        }
        needs.push(Need {
            relations,
            calendar,
            from,
            to: None,
            scope: None,
            attributes: complete.then(|| {
                vec![Attribute {
                    key: "reviewed".into(),
                    value: "complete".into(),
                }]
            }),
            label: Some(label),
        });
    };
    let done = |complete: bool| if complete { " complete" } else { "" };
    if let Some(r) = d.bars {
        let label = format!(
            "bars {}{}{}",
            r,
            d.bars_from.map(|t| format!(" from {}", time::format_timestamp(t))).unwrap_or_default(),
            done(d.bars_complete)
        );
        need("bars", label, Some(r.to_string().trim_start_matches('@').to_string()), d.bars_from, d.bars_complete);
    }
    if d.actions {
        need("actions", "actions".into(), None, None, false);
    }
    if d.listing {
        need("listing", "listing".into(), None, None, false);
    }
    if !d.lists.is_empty() {
        let names: Vec<String> = d.lists.iter().map(|l| format!("\"{}\"", l)).collect();
        need("lists", format!("lists {}", names.join(", ")), None, None, false);
    }
    if !d.series.is_empty() {
        let names: Vec<String> = d.series.iter().map(|(n, r)| format!("\"{}\"{}", n, r.map(|r| format!(" {}", r)).unwrap_or_default())).collect();
        need("series", format!("series {}{}", names.join(", "), done(d.series_complete)), None, None, d.series_complete);
    }
    if d.sessions {
        need("sessions", "sessions".into(), None, None, false);
    }
    if d.point_value {
        need("point_value", "point_value".into(), None, None, false);
    }
    if d.commission {
        need("commission", "commission".into(), None, None, false);
    }
    let prefer = match d.prefer.as_str() {
        "base finest" => Preference {
            kind: PreferenceKind::Finest,
            version: None,
        },
        p if p.starts_with("version ") => Preference {
            kind: PreferenceKind::Version,
            version: Some(p["version ".len()..].to_string()),
        },
        _ => Preference {
            kind: PreferenceKind::Latest,
            version: None,
        },
    };
    // A chain's quotes, and its greeks under `greeks data` (K-5.5.6).
    let mut chain_relations = vec!["quote"];
    if prog.exec.greeks.as_deref() == Some("data") {
        chain_relations.push("greeks");
    }
    let dynamic: Vec<DynamicNeed> = d
        .chains
        .iter()
        .flat_map(|c| chain_relations.iter().map(move |r| (c, *r)))
        .map(|(c, r)| DynamicNeed {
            relation: stored_name(r).unwrap_or_else(|| format!("option.{}", r)),
            underlying: 0,
            underlying_symbol: Some(c.underlying.clone()),
            filters: Some(Box::new(chain_filters(c))),
            spot_relation: Some(stored_name(&c.spot).unwrap_or_else(|| c.spot.clone())),
            // The spot is the relation's price: its first value argument.
            spot_column: prog
                .relations
                .get(&c.spot)
                .and_then(|s| s.args.iter().find(|a| matches!(a.ty, crate::ir::Ty::Quantity(_))).map(|a| a.name.clone())),
            horizon: None,
        })
        .collect();
    Demand {
        class: prog.class.clone(),
        needs: Some(needs),
        require: d.require_reviewed.then(|| {
            vec![Attribute {
                key: "reviewed".into(),
                value: String::new(),
            }]
        }),
        prefer: Some(vec![prefer]),
        budget: Some(Box::new(Budget {
            money: d.budget.as_ref().and_then(|b| b.money).unwrap_or(0.0),
            bytes: d.budget.as_ref().and_then(|b| b.bytes).map(|x| x as i64),
            requests: d.budget.as_ref().and_then(|b| b.requests).map(|x| x as i64),
            seconds: d.budget.as_ref().and_then(|b| b.seconds),
        })),
        dynamic: (!dynamic.is_empty()).then_some(dynamic),
        point: None,
    }
}

/// A chain need's filters as the data layer's chain query takes them.
pub fn chain_filters(c: &crate::check::ChainSpec) -> ChainFilters {
    ChainFilters {
        expiry_from: Some(Box::new(wire_duration(&c.expiry.0))),
        expiry_to: Some(Box::new(wire_duration(&c.expiry.1))),
        moneyness_lo: Some(c.moneyness.0),
        moneyness_hi: Some(c.moneyness.1),
        right: c.right.clone(),
        style: None,
        strike_lo: None,
        strike_hi: None,
        min_open_interest: None,
    }
}

fn wire_duration(d: &crate::ir::Duration) -> abt_wire::generated::abt::wire::Duration {
    abt_wire::generated::abt::wire::Duration {
        months: d.months,
        days: d.days,
        nanos: d.nanos,
    }
}

/// The preconditions on a binding that warn (K-3.2): a version no reviewer
/// vouched for, admitted by `allow unreviewed` (warned on every run, V-3.11);
/// a span that starts after the need's `from`, so the first decisions read
/// less history than the program asked for.
fn binding_warnings(prog: &Program, steps: &[PlanStep]) -> Vec<RunWarning> {
    let mut out = Vec::new();
    let reviewed = |s: &PlanStep| s.attributes.iter().flatten().any(|a| a.key == "reviewed");
    let unreviewed: Vec<String> = steps.iter().filter(|s| !reviewed(s)).map(|s| format!("{} ({})", s.relation, s.source)).collect();
    if !prog.demand.require_reviewed && !unreviewed.is_empty() {
        out.push(RunWarning {
            bias: "unreviewed data".into(),
            message: format!(
                "the program allows unreviewed data, and no reviewer vouched for {}: a gap in them may be missing data rather than an instrument that did not trade",
                unreviewed.join(", ")
            ),
        });
    }
    if let Some(from) = prog.demand.bars_from {
        let late: Vec<String> = steps
            .iter()
            .filter(|s| matches!(short(&s.relation), "open" | "high" | "low" | "close" | "volume"))
            .filter_map(|s| s.from.filter(|f| *f > from).map(|f| (s.relation.clone(), f)))
            .map(|(r, f)| format!("{} from {}", r, time::format_timestamp(f)))
            .collect();
        if !late.is_empty() {
            out.push(RunWarning {
                bias: "warm-up".into(),
                message: format!(
                    "the data block needs bars from {}, and the binding holds {}: the first decisions read less history than the program asks for",
                    time::format_timestamp(from),
                    late.join(", ")
                ),
            });
        }
    }
    out
}

/// The instant a calendar label's bucket closes (E-13): the base bar's
/// close, the label itself on a calendar labelled at its close.
pub fn barrier_at(prog: &Program, label: i64) -> i64 {
    time::bucket_range(prog.base, label).1.max(label)
}

/// The calendars the fold receives barriers on, finer first and the decision
/// calendar last (E-13), with every label and its instant.
pub fn calendars(k: &Kernel) -> Vec<(Cal, Vec<i64>)> {
    let mut needed: Vec<Cal> = k.schedule.keys().cloned().collect();
    for c in [Cal::Base, k.prog.decision_cal.clone()] {
        if !needed.contains(&c) {
            needed.push(c);
        }
    }
    needed.sort();
    let d = k.prog.decision_cal.clone();
    needed.retain(|c| *c != d);
    needed.push(d);
    needed
        .into_iter()
        .map(|c| (c.clone(), k.schedule.get(&c).map(|s| s.iter().copied().collect()).unwrap_or_default()))
        .collect()
}

/// The schema of the kernel's calendar labels under its namespace.
fn calendar_schema(run: &str) -> RelationSchema {
    let col = |name: &str, ty: ColumnType, role: Role| ColumnSpec {
        name: name.to_string(),
        ty,
        role,
        dimension: None,
    };
    RelationSchema {
        name: format!("kernel.{}.calendar_label", run),
        class: "kernel".to_string(),
        columns: vec![
            col("calendar", ColumnType::Label, Role::Entity),
            col("label", ColumnType::Time, Role::EventTime),
            col("at", ColumnType::Time, Role::Value),
            col("r", ColumnType::Time, Role::ReceiptTime),
        ],
        calendar: None,
        constraints: None,
        attributes: None,
        schema_version: Some("1.0".to_string()),
    }
}

/// The stream a kernel folds over: its relations, its calendars' labels put
/// under the run's namespace, its calendars in barrier order.
pub fn stream_spec(k: &Kernel, bound: &Bound, client: &DataClient, run: &str) -> Result<(StreamSpec, BTreeMap<String, Cal>), RunError> {
    let cals = calendars(k);
    let mut names = Vec::new();
    let mut cal_of = BTreeMap::new();
    let (mut c_col, mut l_col, mut a_col) = (Vec::new(), Vec::new(), Vec::new());
    for (cal, labels) in &cals {
        let name = cal.to_string();
        for &l in labels {
            c_col.push(name.clone());
            l_col.push(l);
            a_col.push(barrier_at(k.prog, l));
        }
        cal_of.insert(name.clone(), cal.clone());
        names.push(name);
    }
    let mut relations: Vec<VersionRef> = Vec::new();
    let pairs: BTreeMap<String, String> = bound.binding.pairs.clone().unwrap_or_default().into_iter().map(|p| (p.relation, p.version)).collect();
    for (name, (stored_rel, _)) in &bound.stored {
        if k.relation_id(name).is_some_and(|r| k.is_primitive(r)) {
            relations.push(VersionRef {
                relation: stored_rel.clone(),
                version: pairs[stored_rel].clone(),
            });
        }
    }
    if !c_col.is_empty() {
        let schema = calendar_schema(run);
        let batch = RecordBatch::try_new(
            Arc::new(Schema::new(vec![
                Field::new("calendar", DataType::Utf8, false),
                Field::new("label", DataType::Timestamp(TimeUnit::Nanosecond, None), false),
                Field::new("at", DataType::Timestamp(TimeUnit::Nanosecond, None), false),
            ])),
            vec![
                Arc::new(StringArray::from(c_col)) as ArrayRef,
                Arc::new(TimestampNanosecondArray::from(l_col)),
                Arc::new(TimestampNanosecondArray::from(a_col)),
            ],
        )
        .map_err(|e| RunError::Internal(e.to_string()))?;
        let version = client.put(schema.clone(), &[batch])?;
        relations.push(VersionRef { relation: schema.name, version });
    }
    Ok((
        StreamSpec {
            relations: Some(relations),
            binding: None,
            calendars: Some(names),
            receipt_filter: None,
            clock: abt_wire::generated::abt::wire::ClockSource::Replay,
            session: None,
        },
        cal_of,
    ))
}

/// The current version of a relation, from the data layer's statistics.
fn current_version(client: &DataClient, relation: &str) -> Result<Option<String>, RunError> {
    for b in client.stats()? {
        let (rel, version, current) = (strings(col(&b, "relation")?)?, strings(col(&b, "version")?)?, col(&b, "current")?.as_boolean().clone());
        for i in 0..b.num_rows() {
            if rel[i].as_deref() == Some(relation) && current.value(i) {
                return Ok(version[i].clone());
            }
        }
    }
    Ok(None)
}

/// The labels of a clock relation's calendar (`base`), read at its current
/// version.
fn clock_labels(client: &DataClient, relation: &str, calendar: &str) -> Result<BTreeSet<i64>, RunError> {
    let view = View {
        versions: None,
        binding: None,
        as_of_seq: None,
    };
    let mut out = BTreeSet::new();
    let form = QueryForm::Scan(Box::new(Scan {
        relation: relation.to_string(),
        t0: i64::MIN,
        t1: i64::MAX,
        predicate: None,
        columns: None,
    }));
    for b in client.query(&view, form)? {
        let cals = strings(col(&b, "calendar")?)?;
        let labels = times(col(&b, "label")?);
        for i in 0..b.num_rows() {
            if cals[i].as_deref() == Some(calendar) {
                out.extend(labels[i]);
            }
        }
    }
    Ok(out)
}

/// The bars a live session plans (`live.<session>.plan`, D-5.2 live): a
/// live-sim run's base bars, which its data does not hold yet.
pub fn session_bars(client: &DataClient, session: &str) -> Result<BTreeSet<i64>, RunError> {
    let bars = clock_labels(client, &format!("live.{}.plan", session), "base")?;
    if bars.is_empty() {
        return Err(RunError::Config(format!("the live session `{}` plans no bar (live.{}.plan)", session, session)));
    }
    Ok(bars)
}

/// The clock a live-sim run's stream recorded (`kernel.<run>.clock`, D-5.5)
/// and its base bars: what a replay of that run reads.
pub fn recorded_clock(client: &DataClient, run: &str) -> Result<(VersionRef, BTreeSet<i64>), RunError> {
    let relation = format!("kernel.{}.clock", run);
    let version = current_version(client, &relation)?.ok_or_else(|| RunError::Config(format!("the run `{}` recorded no live stream ({})", run, relation)))?;
    let bars = clock_labels(client, &relation, "base")?;
    Ok((VersionRef { relation, version }, bars))
}

/// A stream spec reading a recorded clock in place of the run's own.
pub fn with_recorded_clock(spec: &mut StreamSpec, clock: VersionRef) {
    let mut relations: Vec<VersionRef> = spec.relations.take().unwrap_or_default().into_iter().filter(|v| !v.relation.ends_with(".calendar_label")).collect();
    relations.push(clock);
    spec.relations = Some(relations);
}

/// How an event batch's columns become a kernel tuple of one relation.
struct Converter {
    rel: usize,
    /// Per argument: the key, or a column of the batch with its kind.
    args: Vec<Option<(usize, Ty)>>,
}

/// An event of the stream, as the fold takes it: a barrier with the instant
/// it was emitted at (its bucket's end in replay, D-5.1; the clock's instant
/// in a live stream, after the grace).
#[derive(Clone, Debug)]
pub enum StreamEvent {
    Tuple { seq: u64, rel: usize, key: i64, r: i64, tuple: Tuple },
    Barrier { seq: u64, cal: Cal, label: i64, at: i64 },
}

/// Turns event batches into the fold's events: relation columns into the
/// kernel's tuples, instrument ids into the kernel's symbols, label codes
/// into its labels.
pub struct Decoder {
    converters: HashMap<String, Converter>,
    cal_of: BTreeMap<String, Cal>,
    entity_syms: HashMap<u64, Sym>,
    label_syms: HashMap<String, Sym>,
}

impl Decoder {
    pub fn new(k: &Kernel, bound: &Bound, cal_of: BTreeMap<String, Cal>) -> Decoder {
        let mut entity_syms = HashMap::new();
        for (bound_sym, id) in bound.ids.iter().enumerate() {
            entity_syms.insert(*id, k.remap_syms[bound_sym]);
        }
        let label_syms = k.labels.names().iter().enumerate().map(|(i, n)| (n.clone(), i as Sym)).collect();
        Decoder {
            converters: HashMap::new(),
            cal_of,
            entity_syms,
            label_syms,
        }
    }

    fn converter(&mut self, k: &Kernel, bound: &Bound, batch: &RecordBatch, stored_rel: &str) -> Option<&Converter> {
        if !self.converters.contains_key(stored_rel) {
            let name = bound.stored.iter().find(|(_, (s, _))| s == stored_rel).map(|(n, _)| n.clone())?;
            let rel = k.relation_id(&name)?;
            let sig = &k.prog.relations[&name];
            let key = sig.key_pos()?;
            let schema = batch.schema();
            let args = sig
                .args
                .iter()
                .enumerate()
                .map(|(i, a)| {
                    if i == key {
                        None
                    } else {
                        schema.index_of(&format!("{}.{}", stored_rel, a.name)).ok().map(|c| (c, a.ty.clone()))
                    }
                })
                .collect();
            self.converters.insert(stored_rel.to_string(), Converter { rel, args });
        }
        self.converters.get(stored_rel)
    }

    /// The events of a batch, in order.
    pub fn events(&mut self, k: &mut Kernel, bound: &Bound, b: &RecordBatch) -> Result<Vec<StreamEvent>, RunError> {
        let bad = |m: &str| RunError::Internal(format!("an event batch {}", m));
        let seq = b.column_by_name("seq").ok_or_else(|| bad("without seq"))?.as_primitive::<arrow::datatypes::UInt64Type>().clone();
        let kind = b.column_by_name("kind").ok_or_else(|| bad("without kind"))?.as_dictionary::<arrow::datatypes::UInt8Type>().clone();
        let kinds = strings(kind.values())?;
        let relation = b.column_by_name("relation").ok_or_else(|| bad("without relation"))?.as_dictionary::<UInt32Type>().clone();
        let rel_names = strings(relation.values())?;
        let t = times(b.column_by_name("t").ok_or_else(|| bad("without t"))?);
        let r = times(b.column_by_name("r").ok_or_else(|| bad("without r"))?);
        let calendar = b.column_by_name("calendar").ok_or_else(|| bad("without calendar"))?.as_dictionary::<UInt32Type>().clone();
        let cal_names = strings(calendar.values())?;
        let label = times(b.column_by_name("label").ok_or_else(|| bad("without label"))?);
        let at = times(b.column_by_name("at").ok_or_else(|| bad("without at"))?);
        let mut out = Vec::with_capacity(b.num_rows());
        for i in 0..b.num_rows() {
            let s = seq.value(i);
            match kinds[kind.keys().value(i) as usize].as_deref() {
                Some("tuple") => {
                    let Some(stored_rel) = rel_names.get(relation.keys().value(i) as usize).cloned().flatten() else {
                        continue;
                    };
                    let (key, rt) = (t[i].ok_or_else(|| bad("tuple without t"))?, r[i].unwrap_or(i64::MIN));
                    let Some(conv) = self.converter(k, bound, b, &stored_rel) else { continue };
                    let rel = conv.rel;
                    let args = conv.args.clone();
                    let mut tuple: Tuple = Vec::with_capacity(args.len());
                    for a in &args {
                        let v = match a {
                            None => Value::Time(key),
                            Some((c, ty)) => self.value(k, b.column(*c), i, ty)?,
                        };
                        tuple.push(v);
                    }
                    out.push(StreamEvent::Tuple { seq: s, rel, key, r: rt, tuple });
                }
                Some("barrier") => {
                    let name = cal_names.get(calendar.keys().value(i) as usize).cloned().flatten().ok_or_else(|| bad("barrier without a calendar"))?;
                    let cal = self.cal_of.get(&name).cloned().ok_or_else(|| bad("barrier of an unknown calendar"))?;
                    let l = label[i].ok_or_else(|| bad("barrier without a label"))?;
                    out.push(StreamEvent::Barrier {
                        seq: s,
                        cal,
                        label: l,
                        at: at[i].unwrap_or(l),
                    });
                }
                _ => {}
            }
        }
        Ok(out)
    }

    fn value(&mut self, k: &mut Kernel, c: &ArrayRef, i: usize, ty: &Ty) -> Result<Value, RunError> {
        let null = || RunError::Internal("a null in a stored relation's column the kernel reads".into());
        if c.is_null(i) {
            // A number the source does not carry reads as NaN (an ORATS
            // quote's last trade, a put's theta): ERRATA E-71.
            return match ty {
                Ty::Quantity(_) | Ty::IntLit => Ok(Value::Num(f64::NAN)),
                _ => Err(null()),
            };
        }
        Ok(match ty {
            Ty::Entity => {
                let id = c.as_primitive::<arrow::datatypes::UInt64Type>().value(i);
                Value::Equity(*self.entity_syms.get(&id).ok_or_else(|| RunError::Internal(format!("instrument {} is not in the binding", id)))?)
            }
            Ty::Label => {
                let d = c.as_dictionary::<UInt32Type>();
                let text = d.values().as_string::<i32>().value(d.keys().value(i) as usize);
                let sym = match self.label_syms.get(text) {
                    Some(&s) => s,
                    None => {
                        let s = k.labels.intern(text);
                        self.label_syms.insert(text.to_string(), s);
                        s
                    }
                };
                Value::Label(sym)
            }
            Ty::Timestamp => Value::Time(c.as_primitive::<arrow::datatypes::TimestampNanosecondType>().value(i)),
            Ty::Count => match c.data_type() {
                DataType::Int64 => Value::Count(c.as_primitive::<arrow::datatypes::Int64Type>().value(i)),
                _ => Value::Count(c.as_primitive::<arrow::datatypes::Float64Type>().value(i) as i64),
            },
            _ => match c.data_type() {
                DataType::Int64 => Value::Num(c.as_primitive::<arrow::datatypes::Int64Type>().value(i) as f64),
                _ => Value::Num(c.as_primitive::<arrow::datatypes::Float64Type>().value(i)),
            },
        })
    }
}

/// The run's id (Definition A-4.1, A-6): BLAKE3 over the canonical form of
/// (H(P), β, π, mode), 64 hex digits. π is the plan step that asked for the
/// run; a run outside a plan has none, and its window, truncation and
/// bundle date stand in its place (ERRATA E-56).
pub fn run_id(program_hash: &str, binding_id: &str, step: &serde_json::Value, mode: &str) -> String {
    let v = serde_json::json!({"program": program_hash, "binding": binding_id, "step": step, "mode": mode});
    abt_wire::Hash::of(abt_wire::to_canonical_json(&v).expect("an identity is JSON").as_bytes()).hex()
}

/// π of a run outside a plan: what of its request, beyond the program and
/// the binding, changes what it decides; none for a run over the whole
/// stream.
pub fn run_step(cfg: &super::ExecConfig, to_seq: Option<u64>, horizon: Option<i64>) -> serde_json::Value {
    let fields = [
        ("start", cfg.start.map(|t| serde_json::json!(t))),
        ("end", cfg.end.map(|t| serde_json::json!(t))),
        ("as_of", cfg.as_of.map(|t| serde_json::json!(t))),
        ("to_seq", to_seq.map(|s| serde_json::json!(s))),
        ("horizon", horizon.map(|t| serde_json::json!(t))),
        ("fault", cfg.inject.as_ref().map(|(c, t)| serde_json::json!([c, t]))),
        ("mask", (!cfg.embargo_mask.is_empty()).then(|| serde_json::json!(cfg.embargo_mask))),
    ];
    let set: serde_json::Map<String, serde_json::Value> = fields.into_iter().filter_map(|(k, v)| v.map(|v| (k.to_string(), v))).collect();
    if set.is_empty() {
        serde_json::Value::Null
    } else {
        serde_json::Value::Object(set)
    }
}

/// π of a run (Q-2.5): the plan step that asked for it, with what of the
/// request beyond the step changes what it decides; a run outside a plan
/// has `run_step`'s alone.
pub fn plan_step(step: Option<&serde_json::Value>, cfg: &super::ExecConfig, to_seq: Option<u64>, horizon: Option<i64>) -> serde_json::Value {
    let own = run_step(cfg, to_seq, horizon);
    match (step, own) {
        (None, own) => own,
        (Some(pi), serde_json::Value::Object(extra)) => {
            let mut v = serde_json::Map::new();
            v.insert("plan_step".into(), pi.clone());
            v.extend(extra);
            serde_json::Value::Object(v)
        }
        (Some(pi), _) => serde_json::json!({ "plan_step": pi }),
    }
}

/// A dataset in memory as an adapter's keyed tables (E-34): its security
/// table, its contract terms, and each relation's rows with instruments
/// named by their keys and labels by their text, columns named as the
/// store's schema names them; a relation the store has no schema of is
/// refused.
#[allow(clippy::type_complexity)]
pub fn keyed_tables(ds: &Dataset, schema_of: &dyn Fn(&str) -> Option<RelationSchema>) -> Result<(RecordBatch, RecordBatch, Vec<(String, RecordBatch)>), String> {
    let ns = DataType::Timestamp(TimeUnit::Nanosecond, None);
    let securities = RecordBatch::try_new(
        Arc::new(Schema::new(vec![
            Field::new("key", DataType::Utf8, false),
            Field::new("ticker", DataType::Utf8, false),
            Field::new("from", ns.clone(), false),
            Field::new("to", ns.clone(), true),
        ])),
        vec![
            Arc::new(StringArray::from(ds.securities.iter().map(|s| ds.symbols.name(s.id).to_string()).collect::<Vec<_>>())) as ArrayRef,
            Arc::new(StringArray::from(ds.securities.iter().map(|s| s.ticker.clone()).collect::<Vec<_>>())),
            Arc::new(TimestampNanosecondArray::from(ds.securities.iter().map(|s| s.from).collect::<Vec<_>>())),
            Arc::new(TimestampNanosecondArray::from(ds.securities.iter().map(|s| s.to).collect::<Vec<_>>())),
        ],
    )
    .map_err(|e| e.to_string())?;
    let contracts: Vec<(&Sym, &Contract)> = ds.contracts.iter().collect();
    let terms = RecordBatch::try_new(
        Arc::new(Schema::new(vec![
            Field::new("key", DataType::Utf8, false),
            Field::new("class", DataType::Utf8, false),
            Field::new("multiplier", DataType::Float64, true),
            Field::new("commission_per_contract", DataType::Float64, true),
        ])),
        vec![
            Arc::new(StringArray::from(contracts.iter().map(|(s, _)| ds.symbols.name(**s).to_string()).collect::<Vec<_>>())) as ArrayRef,
            Arc::new(StringArray::from(contracts.iter().map(|(_, c)| if c.future { "future" } else { "equity" }).collect::<Vec<_>>())),
            Arc::new(Float64Array::from(contracts.iter().map(|(_, c)| Some(c.multiplier)).collect::<Vec<_>>())),
            Arc::new(Float64Array::from(contracts.iter().map(|(_, c)| c.commission_per_contract).collect::<Vec<_>>())),
        ],
    )
    .map_err(|e| e.to_string())?;
    let mut relations = Vec::new();
    for (name, tuples) in &ds.facts {
        let schema = schema_of(name).ok_or_else(|| format!("the data layer has no relation `{}` in the dataset's classes", name))?;
        let cols: Vec<&ColumnSpec> = schema.columns.iter().filter(|c| c.role != Role::ReceiptTime).collect();
        if let Some(tu) = tuples.iter().find(|tu| tu.len() != cols.len()) {
            return Err(format!("`{}` has {} columns in the data layer and a tuple of {} values", schema.name, cols.len(), tu.len()));
        }
        let mut fields = Vec::new();
        let mut arrays: Vec<ArrayRef> = Vec::new();
        for (i, c) in cols.iter().enumerate() {
            let (field, array): (Field, ArrayRef) = match c.ty {
                ColumnType::Entity => (
                    Field::new(&c.name, DataType::Utf8, true),
                    Arc::new(StringArray::from(tuples.iter().map(|tu| tu[i].as_equity().map(|s| ds.symbols.name(s).to_string())).collect::<Vec<_>>())),
                ),
                ColumnType::Label => (
                    Field::new(&c.name, DataType::Utf8, true),
                    Arc::new(StringArray::from(tuples.iter().map(|tu| ds.label_name(&tu[i]).map(str::to_string)).collect::<Vec<_>>())),
                ),
                ColumnType::Time => (
                    Field::new(&c.name, ns.clone(), true),
                    Arc::new(TimestampNanosecondArray::from(tuples.iter().map(|tu| tu[i].as_time()).collect::<Vec<_>>())),
                ),
                ColumnType::Count => (
                    Field::new(&c.name, DataType::Int64, true),
                    Arc::new(Int64Array::from(
                        tuples
                            .iter()
                            .map(|tu| match &tu[i] {
                                Value::Count(n) => Some(*n),
                                v => v.as_f64().map(|x| x as i64),
                            })
                            .collect::<Vec<_>>(),
                    )),
                ),
                _ => (
                    Field::new(&c.name, DataType::Float64, true),
                    Arc::new(Float64Array::from(
                        tuples
                            .iter()
                            .map(|tu| match &tu[i] {
                                Value::Count(n) => Some(*n as f64),
                                v => v.as_f64(),
                            })
                            .collect::<Vec<_>>(),
                    )),
                ),
            };
            fields.push(field);
            arrays.push(array);
        }
        // The receipt time: what the dataset records, else the event time.
        if let (Some(avail), Some(rc)) = (ds.availability_of(name), schema.columns.iter().find(|c| c.role == Role::ReceiptTime)) {
            let key = cols.iter().position(|c| c.role == Role::EventTime).unwrap_or(0);
            fields.push(Field::new(&rc.name, ns.clone(), false));
            arrays.push(Arc::new(TimestampNanosecondArray::from(
                tuples
                    .iter()
                    .enumerate()
                    .map(|(i, tu)| {
                        let t = tu[key].as_time().unwrap_or(i64::MIN);
                        avail.get(i).copied().filter(|a| *a != i64::MIN).unwrap_or(t).max(t)
                    })
                    .collect::<Vec<_>>(),
            )));
        }
        let batch = RecordBatch::try_new(Arc::new(Schema::new(fields)), arrays).map_err(|e| e.to_string())?;
        relations.push((schema.name.clone(), batch));
    }
    Ok((securities, terms, relations))
}

/// The store's schema of a dataset relation of `class`: the class's own,
/// else the one class that has it (the series class's `value`).
pub fn schema_in(schemas: &[RelationSchema], class: &str, name: &str) -> Option<RelationSchema> {
    let own = format!("{}.{}", class, name);
    if let Some(s) = schemas.iter().find(|s| s.name == own) {
        return Some(s.clone());
    }
    if let Some(s) = borrowed(class, name).and_then(|b| schemas.iter().find(|s| s.name == b)) {
        return Some(s.clone());
    }
    let mut others = schemas
        .iter()
        .filter(|s| short(&s.name) == name && s.name.contains('.') && matches!(s.class.as_str(), "equity" | "future" | "option" | "series"));
    let first = others.next().cloned();
    first.filter(|_| others.next().is_none())
}
