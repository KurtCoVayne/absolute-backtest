//! The aggregate pass (D-6): a rolling or bucket aggregate of one relation's
//! column, computed in one time-major pass over the view's rows with
//! `abt-agg`'s window states, per group, and kept as a cache object keyed by
//! the request's canonical form with the versions it read (D-3.5).
//!
//! The windows are the kernel's (K-4.5, Proposition D-6.1): a window walks
//! the labels of the request's `calendar` (the calendar whose labels index
//! a `rows(N)` window's checkpoints, E-27); `rows(N)` holds the group's rows
//! at its last N anchored labels, a label being anchored when the group has
//! a row there; `window(δ)` the rows with `t` in `[t − δ, t]`; the `prior_`
//! forms exclude `t`; `bucket(C)` the rows in `(ℓ_prev, ℓ]` for each label
//! ℓ of C, keyed by ℓ. A row whose `t` is no label of the calendar is in no
//! window, as in the kernel, whose windows range over its time domain.
//! The answer is keyed like the input: a value at each (group, `t`) where
//! the group has a row and the window holds at least `min` rows (all N for
//! `rows(N)` without `min`, V-7.2); an aggregate whose arithmetic fails there
//! (the correlation of a constant series) carries the message instead of a
//! value, which the kernel reports as it reports its own (ERRATA E-31).
//!
//! Protocol 1.3 (ERRATA E-156): `count` counts the window's rows, with a
//! value or not, as the kernel's `count` does; a `factor` makes the value
//! aggregated the product of the column and the factor's column over the
//! rows both relations hold at one entity and `t` (the kernel's join
//! `p(A, T, P), q(A, T, V), D = P * V`), formed as the kernel's `*` forms
//! it; `entities` restricts every read of the pass to those instruments.
//! The answer counts its inexact rows: the input rows received after the
//! barrier of the first label whose window holds them, the rows holding a
//! null the kernel would read, a second row of a group at one instant (the
//! kernel orders those by their values, the pass by the store's key), and
//! the answer rows whose arithmetic failed. With none, every answer row at
//! a label is computed from rows received by that label's barrier, and is
//! the kernel's window there.

use std::collections::{BTreeMap, HashMap};
use std::hash::BuildHasherDefault;
use std::sync::Arc;

use abt_agg::{AggError, Function, Mode, RowsWindow, WindowState};
use abt_wire::canonical::Canonical;
use abt_wire::generated::abt::wire::{AggregateRequest, ColumnSpec, ColumnType, Factor, Grouping, Numerics, RelationSchema, Role, WindowKind};
use abt_wire::hash::Hash;
use arrow::array::{Array, ArrayRef, AsArray, Float64Array, Int64Array, RecordBatch, StringArray, TimestampNanosecondArray, UInt64Array};

use crate::error::DataError;
use crate::query::ResolvedView;
use crate::rows::{self, layout};
use crate::store::{RowFilter, Store};
use crate::stream::CLOCK;

/// A group's key: its entity, or its label's code, or nothing.
type GroupKey = u64;

/// Answer rows, column by column (06-performance 2.2): the time, the
/// group, the value (none where the aggregate failed), the rows counted,
/// the failure's message, and the latest receipt of the group's rows at or
/// before `t`, from which instant the answer is what a reader of the
/// received rows computes (ERRATA E-47); and the inexact rows counted.
#[derive(Clone, Debug, Default, PartialEq)]
struct Out {
    t: Vec<i64>,
    group: Vec<GroupKey>,
    value: Vec<Option<f64>>,
    count: Vec<i64>,
    error: Vec<Option<String>>,
    r: Vec<i64>,
    inexact: u64,
}

impl Out {
    fn len(&self) -> usize {
        self.t.len()
    }

    fn push(&mut self, t: i64, group: GroupKey, value: Option<f64>, count: i64, error: Option<String>, r: i64) {
        self.t.push(t);
        self.group.push(group);
        self.value.push(value);
        self.count.push(count);
        self.error.push(error);
        self.r.push(r);
    }
}

/// The labels of a calendar in the view, in order, each with the instant of
/// its barrier: its `calendar_label` rows. A label the clock holds more than
/// once has its earliest instant, and a row without one is at its label, as
/// the stream emits its barriers (D-5.1).
pub fn calendar_labels(store: &Store, view: &ResolvedView, calendar: &str) -> Result<Vec<(i64, i64)>, DataError> {
    let version = match view.versions.iter().find(|(r, _)| crate::stream::is_clock(r)) {
        Some((_, v)) => v.clone(),
        None => view.version(store, CLOCK)?,
    };
    let m = store.manifest(&version)?;
    let s = &m.schema;
    let cc = s
        .columns
        .iter()
        .position(|c| c.name == "calendar")
        .ok_or_else(|| DataError::Schema("a clock without a `calendar` column".into()))?;
    let ac = s.columns.iter().position(|c| c.name == "at");
    let lc = layout(s).t;
    // Every revision: the stream emits a barrier for each row of a label,
    // and a row is read at the first.
    let rows = store.read(&version, &RowFilter::default())?;
    let code = store.registry.label_code(calendar);
    let mut out = Vec::new();
    for b in &rows {
        let cals = b.column(cc).as_primitive::<arrow::datatypes::UInt32Type>();
        let ts = rows::times(b, lc);
        let ats = ac.map(|c| rows::times(b, c));
        for i in 0..b.num_rows() {
            if Some(cals.value(i)) == code {
                let at = ats.filter(|a| !a.is_null(i)).map_or(ts.value(i), |a| a.value(i));
                out.push((ts.value(i), at));
            }
        }
    }
    out.sort_unstable();
    out.dedup_by_key(|(label, _)| *label);
    if out.is_empty() {
        return Err(DataError::NotFound(format!("the view has no labels of the calendar `{}`", calendar)));
    }
    Ok(out)
}

/// What a cache key hashes: the request and the versions it reads.
#[derive(serde::Serialize)]
struct KeyForm<'a> {
    aggregate: &'a AggregateRequest,
    versions: &'a [(String, String)],
}

/// The cache key of a request over the versions it reads (D-3.5, D-6.3).
/// The request is rendered as itself, not through a JSON value, so that a
/// field protocol 1.3 added is left out while it is absent and a 1.2
/// request keeps its key (ERRATA E-155).
pub fn cache_key(request: &AggregateRequest, versions: &[(String, String)]) -> Result<Hash, DataError> {
    KeyForm { aggregate: request, versions }.canonical_hash().map_err(|e| DataError::Malformed(e.to_string()))
}

/// The answer's schema: the group, the time (or the bucket's label), the
/// value, the window's row count, and the arithmetic's message.
pub fn answer_schema(request: &AggregateRequest) -> RelationSchema {
    let col = |name: &str, ty: ColumnType, role: Role| ColumnSpec {
        name: name.to_string(),
        ty,
        role,
        dimension: None,
    };
    let mut columns = Vec::new();
    match request.group {
        Grouping::PerEntity => columns.push(col("entity", ColumnType::Entity, Role::Entity)),
        Grouping::PerLabel => columns.push(col("group", ColumnType::Label, Role::Entity)),
        Grouping::Whole => {}
    }
    let time = if request.window == WindowKind::Bucket { "label" } else { "t" };
    columns.push(col(time, ColumnType::Time, Role::EventTime));
    columns.push(col("value", ColumnType::Num, Role::Value));
    columns.push(col("count", ColumnType::Count, Role::Value));
    columns.push(col("error", ColumnType::Label, Role::Value));
    columns.push(col("r", ColumnType::Time, Role::ReceiptTime));
    RelationSchema {
        name: request.output.clone().unwrap_or_else(|| "aggregate".to_string()),
        class: "cache".to_string(),
        columns,
        calendar: request.calendar.clone(),
        constraints: None,
        attributes: None,
        schema_version: Some(crate::catalog::SCHEMA_VERSION.to_string()),
    }
}

/// The attribute of an answer's version that holds its inexact rows.
pub const INEXACT_ROWS: &str = "inexact_rows";

/// An answered request (protocol 1.3, ERRATA E-156): its cache key, the
/// cache object holding the answer, which a stream spec can name by its
/// relation and version, the answer's rows, and its inexact rows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Answered {
    pub key: Hash,
    pub relation: String,
    pub version: String,
    pub rows: u64,
    pub inexact_rows: u64,
}

/// A cache object as an answer, when its version records its inexact rows.
/// An object written before protocol 1.3 does not, and counted the rows of
/// a `count` that hold a value: it is computed again.
fn answered(store: &Store, key: &Hash, version: &str) -> Option<Answered> {
    let rec = store.registry.version(version)?;
    let inexact_rows = rec.attributes.get(INEXACT_ROWS)?.parse().ok()?;
    Some(Answered {
        key: *key,
        relation: rec.relation.clone(),
        version: version.to_string(),
        rows: rec.rows,
        inexact_rows,
    })
}

/// Answers an aggregate request: the cache object, computed on a miss.
pub fn aggregate(store: &mut Store, view: &ResolvedView, request: &AggregateRequest) -> Result<Answered, DataError> {
    // The instruments as a set: one object whatever order they are named in.
    let mut request = request.clone();
    if let Some(e) = request.entities.as_mut() {
        e.sort_unstable();
        e.dedup();
    }
    let relation_version = view.version(store, &request.relation)?;
    let mut versions = vec![(request.relation.clone(), relation_version.clone())];
    if request.calendar.is_some() || request.window == WindowKind::Bucket {
        let clock = match view.versions.iter().find(|(r, _)| crate::stream::is_clock(r)) {
            Some((_, v)) => v.clone(),
            None => view.version(store, CLOCK)?,
        };
        versions.push((CLOCK.to_string(), clock));
    }
    if let Some(f) = &request.factor {
        versions.push((f.relation.clone(), view.version(store, &f.relation)?));
    }
    let key = cache_key(&request, &versions)?;
    if let Some(a) = store.cache_get(&key).and_then(|v| answered(store, &key, &v)) {
        return Ok(a);
    }
    let (rows, names) = compute(store, view, &request)?;
    let inexact = rows.inexact;
    let schema = answer_schema(&request);
    let batch = to_batch(&schema, rows, &names)?;
    let version = store.cache_put_with(&key, &schema, &[batch], BTreeMap::from([(INEXACT_ROWS.to_string(), inexact.to_string())]))?;
    answered(store, &key, &version).ok_or(DataError::UnknownVersion(version))
}

/// The answer's rows as a batch in the answer schema.
fn to_batch(schema: &RelationSchema, rows: Out, names: &BTreeMap<u64, String>) -> Result<RecordBatch, DataError> {
    let mut cols: Vec<ArrayRef> = Vec::new();
    let mut fields = Vec::new();
    let ty = |c: &ColumnSpec| match c.ty {
        ColumnType::Label => arrow::datatypes::DataType::Utf8,
        ColumnType::Time => arrow::datatypes::DataType::Timestamp(arrow::datatypes::TimeUnit::Nanosecond, None),
        other => rows::stored_type(other),
    };
    let Out { t, group, value, count, error, r, .. } = rows;
    for c in &schema.columns {
        fields.push(arrow::datatypes::Field::new(&c.name, ty(c), true));
        let col: ArrayRef = match c.name.as_str() {
            "entity" => Arc::new(UInt64Array::from(group.clone())),
            "group" => Arc::new(StringArray::from(group.iter().map(|g| names.get(g).cloned().unwrap_or_default()).collect::<Vec<_>>())),
            "t" | "label" => Arc::new(TimestampNanosecondArray::from(t.clone())),
            "r" => Arc::new(TimestampNanosecondArray::from(r.clone())),
            "value" => Arc::new(Float64Array::from(value.clone())),
            "count" => Arc::new(Int64Array::from(count.clone())),
            _ => Arc::new(StringArray::from(error.clone())),
        };
        cols.push(col);
    }
    Ok(RecordBatch::try_new(Arc::new(arrow::datatypes::Schema::new(fields)), cols)?)
}

/// A number as a column holds it: a `Num` column's float, a `Count`
/// column's integer.
#[derive(Clone, Copy, Debug)]
enum Num {
    F(f64),
    I(i64),
}

impl Num {
    /// Row `i` of column `c` of type `ty`, none where null.
    fn of(b: &RecordBatch, c: usize, ty: ColumnType, i: usize) -> Option<Num> {
        let col = b.column(c);
        if col.is_null(i) {
            return None;
        }
        Some(match ty {
            ColumnType::Count => Num::I(col.as_primitive::<arrow::datatypes::Int64Type>().value(i)),
            _ => Num::F(col.as_primitive::<arrow::datatypes::Float64Type>().value(i)),
        })
    }

    fn f64(self) -> f64 {
        match self {
            Num::F(x) => x,
            Num::I(i) => i as f64,
        }
    }

    /// The product as the kernel's `*` forms it (`eval_expr`): two counts
    /// multiply as integers, anything else as floats; none where the
    /// integers overflow.
    fn times(self, other: Num) -> Option<f64> {
        match (self, other) {
            (Num::I(a), Num::I(b)) => a.checked_mul(b).map(|p| p as f64),
            (a, b) => Some(a.f64() * b.f64()),
        }
    }
}

/// A row of a product's second relation: its time, its receipt and its
/// value, none where null.
#[derive(Clone, Copy, Debug)]
struct FactorRow {
    t: i64,
    r: i64,
    x: Option<Num>,
}

type IdMap<V> = HashMap<GroupKey, V, BuildHasherDefault<IdHasher>>;

/// A product's second relation: each entity's rows in time order, and the
/// rows of it that make the answer inexact.
#[derive(Default)]
struct FactorRows {
    by_group: IdMap<Vec<FactorRow>>,
    inexact: u64,
}

/// The columns of a relation the kernel decodes from a tuple besides its
/// times and entities: where it reads a null as NaN or refuses it.
fn decoded_columns(s: &RelationSchema) -> Vec<usize> {
    (0..s.columns.len()).filter(|&c| matches!(s.columns[c].role, Role::Value | Role::Label)).collect()
}

/// The decoded columns of a batch that hold a null.
fn nullable(b: &RecordBatch, decoded: &[usize]) -> Vec<usize> {
    decoded.iter().copied().filter(|&c| b.column(c).null_count() > 0).collect()
}

/// The identities a read of a relation keeps: the request's instruments
/// when the relation is keyed by one entity; one keyed otherwise is read
/// whole, as the stream reads it (ERRATA E-99).
fn identity_filter(s: &RelationSchema, entities: Option<&Vec<u64>>) -> Option<Vec<u64>> {
    let lay = layout(s);
    let by_entity = matches!(lay.identity.as_slice(), [c] if s.columns[*c].ty == ColumnType::Entity);
    entities.filter(|_| by_entity).cloned()
}

/// The rows of a product's second relation the filter admits, by entity.
fn read_factor(store: &Store, view: &ResolvedView, factor: &Factor, filter: &RowFilter, entities: Option<&Vec<u64>>) -> Result<FactorRows, DataError> {
    let version = view.version(store, &factor.relation)?;
    let m = store.manifest(&version)?;
    let s = &m.schema;
    let lay = layout(s);
    let c = s
        .columns
        .iter()
        .position(|c| c.name == factor.column)
        .ok_or_else(|| DataError::Malformed(format!("`{}` has no column `{}`", s.name, factor.column)))?;
    let ty = s.columns[c].ty;
    if !matches!(ty, ColumnType::Num | ColumnType::Count) {
        return Err(DataError::Malformed(format!("`{}` is not numeric", factor.column)));
    }
    let ec = match lay.identity.first() {
        Some(&e) if s.columns[e].ty == ColumnType::Entity => e,
        _ => return Err(DataError::Malformed(format!("`{}` has no entity to join on", s.name))),
    };
    let filter = RowFilter {
        identity: identity_filter(s, entities),
        ..filter.clone()
    };
    let decoded = decoded_columns(s);
    let mut out = FactorRows::default();
    for b in &store.read(&version, &filter)? {
        let ts = rows::times(b, lay.t);
        let rs = lay.r.map(|r| rows::times(b, r));
        let ids = b.column(ec).as_primitive::<arrow::datatypes::UInt64Type>();
        let nulls = nullable(b, &decoded);
        for i in 0..b.num_rows() {
            let t = ts.value(i);
            let rows = out.by_group.entry(ids.value(i)).or_default();
            if rows.last().is_some_and(|x| x.t == t) || nulls.iter().any(|&n| b.column(n).is_null(i)) {
                out.inexact += 1;
            }
            rows.push(FactorRow {
                t,
                r: rs.map_or(t, |r| r.value(i)),
                x: Num::of(b, c, ty, i),
            });
        }
    }
    Ok(out)
}

/// The input's rows of one group in time order: (t, values) of the rows
/// with values (every row for `count`), and (t, r) of every row, the
/// receipts an answer waits on; and the rows found inexact as they were
/// read.
struct Series {
    rows: Vec<(i64, [f64; 2])>,
    /// The rows as integer ticks when the pass runs `exact` over a hot
    /// relation (ST-10.4): units at `Ctx::scales`.
    ticks: Vec<(i64, [i128; 2])>,
    recv: Vec<(i64, i64)>,
    /// The time of the group's last row read.
    last: i64,
    /// A product's cursor in the second relation's rows of the group, and
    /// the span of them at `last`.
    next: usize,
    at_last: (usize, usize),
    inexact: u64,
}

impl Default for Series {
    fn default() -> Series {
        Series {
            rows: Vec::new(),
            ticks: Vec::new(),
            recv: Vec::new(),
            last: i64::MIN,
            next: 0,
            at_last: (0, 0),
            inexact: 0,
        }
    }
}

impl Series {
    /// The latest receipt at or before each row, in `recv` order.
    fn prefix_receipts(&self) -> Vec<i64> {
        self.recv
            .iter()
            .scan(i64::MIN, |m, &(_, r)| {
                *m = (*m).max(r);
                Some(*m)
            })
            .collect()
    }

    /// A row of a product's first relation, at `t` with value `x`, joined
    /// with `theirs`, the second relation's rows of the group: every row of
    /// either enters the receipts in time order, and each pair of rows at
    /// `t` enters the rows as its product (a pair with a null only for
    /// `count`, which counts the joined rows). The rows entered.
    fn join(&mut self, t: i64, r: i64, x: Option<Num>, theirs: &[FactorRow], counted: bool) -> usize {
        if t != self.last {
            while self.next < theirs.len() && theirs[self.next].t < t {
                self.recv.push((theirs[self.next].t, theirs[self.next].r));
                self.next += 1;
            }
            let from = self.next;
            while self.next < theirs.len() && theirs[self.next].t == t {
                self.recv.push((t, theirs[self.next].r));
                self.next += 1;
            }
            self.at_last = (from, self.next);
        }
        self.recv.push((t, r));
        let mut entered = 0;
        for y in &theirs[self.at_last.0..self.at_last.1] {
            let product = match (x, y.x) {
                (Some(x), Some(y)) => x.times(y),
                _ => None,
            };
            if product.is_none() && x.is_some() && y.x.is_some() {
                // An integer product past i64: the kernel's would not be this.
                self.inexact += 1;
            }
            if let Some(p) = product.or(counted.then_some(f64::NAN)) {
                self.rows.push((t, [p, 0.0]));
                entered += 1;
            }
        }
        entered
    }

    /// The second relation's rows after the first relation's last row of
    /// the group enter the receipts.
    fn flush(&mut self, theirs: &[FactorRow]) {
        for y in &theirs[self.next.min(theirs.len())..] {
            self.recv.push((y.t, y.r));
        }
        self.next = theirs.len();
    }
}

/// The pass: the request's rows, in (t, group) order, and the texts of the
/// labels that key a per-label grouping.
fn compute(store: &Store, view: &ResolvedView, request: &AggregateRequest) -> Result<(Out, BTreeMap<u64, String>), DataError> {
    let f = Function::parse(&request.function, request.level).map_err(|e| DataError::Malformed(e.to_string()))?;
    let version = view.version(store, &request.relation)?;
    let m = store.manifest(&version)?;
    let s = &m.schema;
    let lay = layout(s);
    let column = |name: &str| {
        s.columns
            .iter()
            .position(|c| c.name == name)
            .ok_or_else(|| DataError::Malformed(format!("`{}` has no column `{}`", s.name, name)))
    };
    let vc = column(&request.column)?;
    let wc = match &request.second_column {
        Some(c) => Some(column(c)?),
        None if f.arity() == 2 => return Err(DataError::Malformed(format!("{} takes two columns", f.name()))),
        None => None,
    };
    for c in std::iter::once(vc).chain(wc) {
        if !matches!(s.columns[c].ty, ColumnType::Num | ColumnType::Count) {
            return Err(DataError::Malformed(format!("`{}` is not numeric", s.columns[c].name)));
        }
    }
    let gc = match request.group {
        Grouping::PerEntity => Some(*lay.identity.first().ok_or_else(|| DataError::Malformed(format!("`{}` has no entity to group by", s.name)))?),
        Grouping::PerLabel => Some(column(
            request.group_column.as_deref().ok_or_else(|| DataError::Malformed("a per-label group names its column".into()))?,
        )?),
        Grouping::Whole => None,
    };
    if gc.is_none_or(|c| request.group == Grouping::PerLabel && s.columns[c].ty != ColumnType::Label) && request.group != Grouping::Whole {
        return Err(DataError::Malformed("a per-label group is a label column".into()));
    }
    let rows_window = matches!(request.window, WindowKind::Rows | WindowKind::PriorRows);
    if rows_window && request.group != Grouping::PerEntity {
        return Err(DataError::Malformed("`rows(N)` counts one entity's rows: group per entity".into()));
    }
    if request.factor.is_some() && (request.group != Grouping::PerEntity || f.arity() == 2) {
        return Err(DataError::Malformed("a product of two relations is joined per entity and aggregated by a one-column function".into()));
    }
    // `count` counts the window's rows, with a value or not, as the
    // kernel's `count` does.
    let counted = f == Function::Count;
    // The rows admitted by the receipt filter, latest revision of each fact,
    // of the request's instruments.
    let filter = RowFilter {
        r_max: request.receipt_filter.as_ref().and_then(|r| r.r_max),
        as_of_seq: view.as_of_seq,
        latest_only: true,
        ..RowFilter::default()
    };
    let factor = match &request.factor {
        Some(fr) => Some(read_factor(store, view, fr, &filter, request.entities.as_ref())?),
        None => None,
    };
    let filter = RowFilter {
        identity: identity_filter(s, request.entities.as_ref()),
        ..filter
    };
    // `exact` over a hot relation reads the ticks themselves (ST-10.4):
    // every Num column at the version's finest scale, counts at scale 0.
    // A column with a value off any grid has no exact form. A product is a
    // derived series, reduced in `exact` as in `two_pass` (D-6.3).
    let exact_ticks = request.numerics == Numerics::Exact && store.is_hot(&version) && factor.is_none();
    let mut scales = [0u32; 2];
    let batches = if exact_ticks {
        let hv = store.hot(&version)?;
        for (k, c) in std::iter::once(vc).chain(wc).enumerate() {
            let entry = hv.manifest.columns.iter().find(|e| e.name == s.columns[c].name);
            if entry.is_some_and(|e| e.raw) {
                return Err(DataError::Malformed(format!(
                    "`{}`.`{}` holds values off any decimal grid: `exact` has no integer form for it (ST-3.2); ask `two_pass`",
                    s.name, s.columns[c].name
                )));
            }
            if s.columns[c].ty == ColumnType::Num {
                scales[k] = u32::MAX;
            }
        }
        let (scale, batches) = store.read_ticks(&version, &filter)?;
        for sc in scales.iter_mut() {
            if *sc == u32::MAX {
                *sc = scale;
            }
        }
        batches
    } else {
        store.read(&version, &filter)?
    };
    let units = |b: &RecordBatch, c: usize, i: usize| -> i128 {
        match b.column(c).data_type() {
            arrow::datatypes::DataType::Decimal128(_, _) => b.column(c).as_primitive::<arrow::datatypes::Decimal128Type>().value(i),
            _ => b.column(c).as_primitive::<arrow::datatypes::Int64Type>().value(i) as i128,
        }
    };
    let value = |b: &RecordBatch, c: usize, i: usize| -> f64 {
        match s.columns[c].ty {
            ColumnType::Count => b.column(c).as_primitive::<arrow::datatypes::Int64Type>().value(i) as f64,
            _ => {
                let a = b.column(c).as_primitive::<arrow::datatypes::Float64Type>();
                if a.is_null(i) {
                    f64::NAN
                } else {
                    a.value(i)
                }
            }
        }
    };
    let decoded = decoded_columns(s);
    // Each group a dense slot on first sight (06-performance 2.2), the slots
    // put in the groups' order after.
    let mut slot_of: IdMap<usize> = Default::default();
    let mut slots: Vec<(GroupKey, Series)> = Vec::new();
    let mut times: Vec<i64> = Vec::new();
    let none: &[FactorRow] = &[];
    for b in &batches {
        let ts = rows::times(b, lay.t);
        let rs = lay.r.map(|r| rows::times(b, r));
        let nulls = nullable(b, &decoded);
        for i in 0..b.num_rows() {
            let g = match gc {
                Some(c) if s.columns[c].ty == ColumnType::Entity => b.column(c).as_primitive::<arrow::datatypes::UInt64Type>().value(i),
                Some(c) => b.column(c).as_primitive::<arrow::datatypes::UInt32Type>().value(i) as u64,
                None => 0,
            };
            let t = ts.value(i);
            let r = rs.map_or(t, |r| r.value(i));
            let at = *slot_of.entry(g).or_insert_with(|| {
                slots.push((g, Series::default()));
                slots.len() - 1
            });
            let series = &mut slots[at].1;
            // A second row of the group at one instant: the kernel orders
            // the rows of a bar by their values, the pass by the store's key.
            // A null: the kernel reads it as NaN or refuses it.
            if t == series.last || nulls.iter().any(|&c| b.column(c).is_null(i)) {
                series.inexact += 1;
            }
            if let Some(fx) = &factor {
                let theirs = fx.by_group.get(&g).map_or(none, |v| v.as_slice());
                let entered = series.join(t, r, Num::of(b, vc, s.columns[vc].ty, i), theirs, counted);
                series.last = t;
                times.extend(std::iter::repeat_n(t, entered));
                continue;
            }
            series.last = t;
            series.recv.push((t, r));
            if !counted && (b.column(vc).is_null(i) || wc.is_some_and(|w| b.column(w).is_null(i))) {
                continue;
            }
            if exact_ticks {
                series.ticks.push((t, [units(b, vc, i), wc.map(|w| units(b, w, i)).unwrap_or(0)]));
            } else {
                let x = [value(b, vc, i), wc.map(|w| value(b, w, i)).unwrap_or(0.0)];
                series.rows.push((t, x));
            }
            times.push(t);
        }
    }
    let mut inexact = 0;
    if let Some(fx) = &factor {
        for (g, series) in slots.iter_mut() {
            series.flush(fx.by_group.get(g).map_or(none, |v| v.as_slice()));
        }
        inexact += fx.inexact;
    }
    slots.sort_unstable_by_key(|(g, _)| *g);
    let names: BTreeMap<u64, String> = if request.group == Grouping::PerLabel {
        slots.iter().map(|(c, _)| (*c, store.registry.label_text(*c as u32).unwrap_or("").to_string())).collect()
    } else {
        BTreeMap::new()
    };
    // The labels the windows walk, with their barriers' instants.
    let calendar = match &request.calendar {
        Some(c) => Some(calendar_labels(store, view, c)?),
        None => None,
    };
    let labels: Vec<i64> = match &calendar {
        Some(c) => c.iter().map(|&(l, _)| l).collect(),
        None => {
            times.sort_unstable();
            times.dedup();
            times
        }
    };
    let bucket_labels = if request.window == WindowKind::Bucket {
        let c = request.bucket.as_deref().ok_or_else(|| DataError::Malformed("a bucket window names its calendar".into()))?;
        Some(calendar_labels(store, view, c)?)
    } else {
        None
    };
    let buckets: Vec<i64> = bucket_labels.iter().flatten().map(|&(l, _)| l).collect();
    // The barriers an answer is read at: a bucket's, else the labels' when
    // the request names their calendar.
    let barriers = bucket_labels.as_deref().or(calendar.as_deref());
    let mode = match request.numerics {
        Numerics::Running => Mode::Running,
        Numerics::TwoPass => Mode::TwoPass,
        // Over the float columns the pass reads today, `exact` is `two_pass`
        // (A-D47); tick columns go through `abt_agg::reduce_ticks` once the
        // hot store answers them (iteration 1, M-2).
        Numerics::Exact => Mode::Exact,
    };
    let (t0, t1) = (request.t0.unwrap_or(i64::MIN), request.t1.unwrap_or(i64::MAX));
    // Per group, in parallel over disjoint ranges of groups; each group's
    // sequence is computed in time order by one thread, and the output is
    // put in (t, group) order after (A-D11).
    let groups = slots;
    let chunk = groups.len().div_ceil(threads()).max(if THREADS.load(std::sync::atomic::Ordering::Relaxed) == 0 { 64 } else { 1 });
    let ctx = Ctx {
        f,
        mode,
        scales,
        exact: exact_ticks,
        request,
        labels: &labels,
        buckets: &buckets,
        barriers,
        t0,
        t1,
    };
    let out: Vec<Out> = std::thread::scope(|sc| {
        let handles: Vec<_> = groups
            .chunks(chunk)
            .map(|part| {
                let ctx = &ctx;
                sc.spawn(move || {
                    let mut out = Out::default();
                    for (g, series) in part {
                        ctx.group(&mut out, *g, series);
                    }
                    out
                })
            })
            .collect();
        handles.into_iter().map(|h| h.join().expect("an aggregate thread")).collect()
    });
    let mut out = time_major(out);
    out.inexact += inexact;
    Ok((out, names))
}

/// The pass's rows in (t, group) order. Each part holds whole groups, the
/// parts are in group order and each group's rows are in time order, so a
/// counting sort on the rank of `t` among the answer's times, stable over
/// the input, puts every row where a stable sort by (t, group) does, in
/// O(n) after the times' sort, column by column.
fn time_major(parts: Vec<Out>) -> Out {
    let n: usize = parts.iter().map(|p| p.len()).sum();
    let mut ts: Vec<i64> = parts.iter().flat_map(|p| p.t.iter().copied()).collect();
    ts.sort_unstable();
    ts.dedup();
    // Each row's rank: a group's times rise, so a cursor walks forward
    // within a group and a binary search places a group's first row.
    let mut ranks: Vec<u32> = Vec::with_capacity(n);
    for p in &parts {
        let mut at = 0usize;
        for i in 0..p.len() {
            let t = p.t[i];
            if i == 0 || p.group[i] != p.group[i - 1] || t < ts[at] {
                at = ts.partition_point(|&x| x < t);
            } else {
                while ts[at] < t {
                    at += 1;
                }
            }
            ranks.push(at as u32);
        }
    }
    let mut next = vec![0usize; ts.len() + 1];
    for &r in &ranks {
        next[r as usize + 1] += 1;
    }
    for i in 1..next.len() {
        next[i] += next[i - 1];
    }
    // Every row's place, then each column scattered there in turn.
    let places: Vec<u32> = ranks
        .iter()
        .map(|&r| {
            let at = next[r as usize];
            next[r as usize] += 1;
            at as u32
        })
        .collect();
    fn scatter<T: Clone + Default>(places: &[u32], cols: impl Iterator<Item = Vec<T>>, n: usize) -> Vec<T> {
        let mut out = vec![T::default(); n];
        let mut i = 0usize;
        for col in cols {
            for v in col {
                out[places[i] as usize] = v;
                i += 1;
            }
        }
        out
    }
    let mut t = Vec::new();
    let mut group = Vec::new();
    let mut value = Vec::new();
    let mut count = Vec::new();
    let mut error = Vec::new();
    let mut r = Vec::new();
    let inexact = parts.iter().map(|p| p.inexact).sum();
    for p in parts {
        t.push(p.t);
        group.push(p.group);
        value.push(p.value);
        count.push(p.count);
        error.push(p.error);
        r.push(p.r);
    }
    Out {
        t: scatter(&places, t.into_iter(), n),
        group: scatter(&places, group.into_iter(), n),
        value: scatter(&places, value.into_iter(), n),
        count: scatter(&places, count.into_iter(), n),
        error: scatter(&places, error.into_iter(), n),
        r: scatter(&places, r.into_iter(), n),
        inexact,
    }
}

/// A hasher for the dense slots' integer keys: a multiply by the golden
/// ratio, as in Fibonacci hashing.
#[derive(Default)]
struct IdHasher(u64);

impl std::hash::Hasher for IdHasher {
    fn finish(&self) -> u64 {
        self.0
    }
    fn write(&mut self, bytes: &[u8]) {
        for b in bytes {
            self.0 = (self.0.rotate_left(8) ^ *b as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15);
        }
    }
    fn write_u64(&mut self, x: u64) {
        self.0 = x.wrapping_mul(0x9E37_79B9_7F4A_7C15);
    }
}

/// Threads the pass runs groups on; 0 is the machine's cores, at most eight.
static THREADS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// Sets the pass's threads (G3 compares one with eight).
pub fn set_threads(n: usize) {
    THREADS.store(n, std::sync::atomic::Ordering::Relaxed);
}

fn threads() -> usize {
    match THREADS.load(std::sync::atomic::Ordering::Relaxed) {
        0 => std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1).min(8),
        n => n,
    }
}

/// What every group's walk shares.
struct Ctx<'a> {
    f: Function,
    mode: Mode,
    /// The tick scales of the two columns under `exact` over a hot relation.
    scales: [u32; 2],
    exact: bool,
    request: &'a AggregateRequest,
    labels: &'a [i64],
    buckets: &'a [i64],
    /// The labels an answer is read at and their barriers' instants: the
    /// buckets', else the labels' when the request names their calendar.
    barriers: Option<&'a [(i64, i64)]>,
    t0: i64,
    t1: i64,
}

impl Ctx<'_> {
    #[allow(clippy::too_many_arguments)]
    fn emit(&self, out: &mut Out, series: &Series, received: &[i64], t: i64, group: GroupKey, count: usize, v: Result<Option<f64>, AggError>) {
        if t < self.t0 || t > self.t1 {
            return;
        }
        let upto = series.recv.partition_point(|x| x.0 <= t);
        let r = if upto == 0 { i64::MIN } else { received[upto - 1] };
        match v {
            Ok(Some(x)) => out.push(t, group, Some(x), count as i64, None, r),
            Ok(None) => {}
            Err(e) => {
                // The kernel evaluates the window to fail as the rule does.
                out.inexact += 1;
                out.push(t, group, None, count as i64, Some(e.to_string()), r)
            }
        }
    }

    /// The rows of a group received after the barrier of the first label
    /// at or after their time, the first an answer that waits on them is
    /// read at; every row when the request names no barrier calendar.
    fn late(&self, all: &Series) -> u64 {
        let Some(barriers) = self.barriers else { return all.recv.len() as u64 };
        let late = all.recv.iter().filter(|&&(t, r)| {
            let first = barriers.partition_point(|b| b.0 < t);
            barriers.get(first).is_some_and(|b| r > b.1)
        });
        late.count() as u64
    }

    /// One group's answer rows, in time order, after `out`'s.
    fn group(&self, out: &mut Out, g: GroupKey, all: &Series) {
        out.inexact += all.inexact + self.late(all);
        if self.exact {
            self.walk(out, g, all, &all.ticks)
        } else {
            self.walk(out, g, all, &all.rows)
        }
    }

    /// The walk over a group's rows, as floats or as ticks (`Value`).
    fn walk<V: abt_agg::Value>(&self, out: &mut Out, g: GroupKey, all: &Series, series: &[(i64, [V; 2])]) {
        let r = self.request;
        let received = all.prefix_receipts();
        // A group's rows at the labels only, one per label for rows windows.
        let at_label = |t: i64| self.labels.binary_search(&t).is_ok();
        match r.window {
            WindowKind::Rows | WindowKind::PriorRows => {
                let n = r.n as usize;
                let min = r.min.map(|m| m as usize).unwrap_or(n);
                let prior = r.window == WindowKind::PriorRows;
                // E-28: running sums answer `rows(N)`; a prior window is reduced
                // by the two-pass formulas.
                let mode = if prior { Mode::TwoPass } else { self.mode };
                let mut w = RowsWindow::<V>::with_scales(self.f, mode, self.scales, n);
                let mut next = 0usize;
                let first = series.first().map(|x| x.0).unwrap_or(i64::MAX);
                for (index, &t) in self.labels.iter().enumerate() {
                    while next < series.len() && series[next].0 < t {
                        next += 1;
                    }
                    let row = (next < series.len() && series[next].0 == t).then(|| series[next].1);
                    if t < first {
                        continue;
                    }
                    if prior && row.is_some() {
                        let count = w.count();
                        let v = w.value(min);
                        self.emit(out, all, &received, t, g, count, v);
                    }
                    w.step(index as u64, t, row.is_some(), row.as_ref().map(|x| x.as_slice()));
                    if !prior && row.is_some() {
                        let count = w.count();
                        let v = w.value(min);
                        self.emit(out, all, &received, t, g, count, v);
                    }
                }
            }
            WindowKind::Span | WindowKind::PriorSpan => {
                let d = r.span.as_deref().cloned().unwrap_or_default();
                let min = r.min.map(|m| m as usize).unwrap_or(1);
                let prior = r.window == WindowKind::PriorSpan;
                let mut w = WindowState::<V>::with_scales(self.f, Mode::TwoPass, self.scales);
                let mut i = 0usize;
                while i < series.len() {
                    let t = series[i].0;
                    // Rows of one instant enter together (a whole group may
                    // hold several).
                    let mut j = i;
                    while j < series.len() && series[j].0 == t {
                        j += 1;
                    }
                    if at_label(t) {
                        w.evict(abt_wire::scalar::sub_duration(t, d.months, d.days, d.nanos));
                        if prior {
                            let v = w.value(min);
                            self.emit(out, all, &received, t, g, w.len(), v);
                        }
                        for row in &series[i..j] {
                            w.push(t, &row.1);
                        }
                        if !prior {
                            let v = w.value(min);
                            self.emit(out, all, &received, t, g, w.len(), v);
                        }
                    }
                    i = j;
                }
            }
            WindowKind::Bucket => {
                let min = r.min.map(|m| m as usize).unwrap_or(1);
                let mut w = WindowState::<V>::with_scales(self.f, Mode::TwoPass, self.scales);
                let mut i = 0usize;
                for &l in self.buckets {
                    while i < series.len() && series[i].0 <= l {
                        if at_label(series[i].0) {
                            w.push(series[i].0, &series[i].1);
                        }
                        i += 1;
                    }
                    if !w.is_empty() {
                        let v = w.value(min);
                        self.emit(out, all, &received, l, g, w.len(), v);
                    }
                    w = WindowState::<V>::with_scales(self.f, Mode::TwoPass, self.scales);
                }
            }
        }
    }
}

/// The answer of a cache object as the protocol carries it (P-4).
pub fn answer(store: &Store, version: &str) -> Result<RecordBatch, DataError> {
    let m = store.manifest(version)?;
    let b = store.read_all(version)?;
    rows::answer(&m.schema, &b, store.registry.labels(), false)
}
