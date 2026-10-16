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

use std::collections::BTreeMap;
use std::sync::Arc;

use abt_agg::{AggError, Function, Mode, RowsWindow, WindowState};
use abt_wire::canonical::Canonical;
use abt_wire::generated::abt::wire::{AggregateRequest, ColumnSpec, ColumnType, Grouping, Numerics, RelationSchema, Role, WindowKind};
use abt_wire::hash::Hash;
use arrow::array::{ArrayRef, AsArray, Float64Array, Int64Array, RecordBatch, StringArray, TimestampNanosecondArray, UInt64Array};

use crate::error::DataError;
use crate::query::ResolvedView;
use crate::rows::{self, layout};
use crate::store::{RowFilter, Store};
use crate::stream::CLOCK;

/// A group's key: its entity, or its label's code, or nothing.
type GroupKey = u64;

/// One output row.
#[derive(Clone, Debug, PartialEq)]
struct Out {
    t: i64,
    group: GroupKey,
    value: Option<f64>,
    count: i64,
    error: Option<String>,
    /// The latest receipt of the group's rows at or before `t`: from that
    /// instant on, the answer is what a reader of the received rows computes
    /// (ERRATA E-47).
    r: i64,
}

/// The labels of a calendar in the view: its `calendar_label` rows.
pub fn calendar_labels(store: &Store, view: &ResolvedView, calendar: &str) -> Result<Vec<i64>, DataError> {
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
    let lc = layout(s).t;
    let rows = store.read(
        &version,
        &RowFilter {
            latest_only: true,
            ..RowFilter::default()
        },
    )?;
    let code = store.registry.label_code(calendar);
    let mut out = Vec::new();
    for b in &rows {
        let cals = b.column(cc).as_primitive::<arrow::datatypes::UInt32Type>();
        let ts = rows::times(b, lc);
        for i in 0..b.num_rows() {
            if Some(cals.value(i)) == code {
                out.push(ts.value(i));
            }
        }
    }
    out.sort_unstable();
    out.dedup();
    if out.is_empty() {
        return Err(DataError::NotFound(format!("the view has no labels of the calendar `{}`", calendar)));
    }
    Ok(out)
}

/// The cache key of a request over the versions it reads (D-3.5, D-6.3).
pub fn cache_key(request: &AggregateRequest, versions: &[(String, String)]) -> Result<Hash, DataError> {
    let canon = serde_json::json!({ "aggregate": request, "versions": versions });
    canon.canonical_hash().map_err(|e| DataError::Malformed(e.to_string()))
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

/// Answers an aggregate request: the cache object's version and its rows,
/// computed on a miss.
pub fn aggregate(store: &mut Store, view: &ResolvedView, request: &AggregateRequest) -> Result<(String, Hash), DataError> {
    let relation_version = view.version(store, &request.relation)?;
    let mut versions = vec![(request.relation.clone(), relation_version.clone())];
    if request.calendar.is_some() || request.window == WindowKind::Bucket {
        let clock = match view.versions.iter().find(|(r, _)| crate::stream::is_clock(r)) {
            Some((_, v)) => v.clone(),
            None => view.version(store, CLOCK)?,
        };
        versions.push((CLOCK.to_string(), clock));
    }
    let key = cache_key(request, &versions)?;
    if let Some(v) = store.cache_get(&key) {
        return Ok((v, key));
    }
    let (rows, names) = compute(store, view, request)?;
    let schema = answer_schema(request);
    let batch = to_batch(&schema, &rows, &names)?;
    let version = store.cache_put(&key, &schema, &[batch])?;
    Ok((version, key))
}

/// The answer's rows as a batch in the answer schema.
fn to_batch(schema: &RelationSchema, rows: &[Out], names: &BTreeMap<u64, String>) -> Result<RecordBatch, DataError> {
    let mut cols: Vec<ArrayRef> = Vec::new();
    let mut fields = Vec::new();
    let ty = |c: &ColumnSpec| match c.ty {
        ColumnType::Label => arrow::datatypes::DataType::Utf8,
        ColumnType::Time => arrow::datatypes::DataType::Timestamp(arrow::datatypes::TimeUnit::Nanosecond, None),
        other => rows::stored_type(other),
    };
    for c in &schema.columns {
        fields.push(arrow::datatypes::Field::new(&c.name, ty(c), true));
        let col: ArrayRef = match c.name.as_str() {
            "entity" => Arc::new(UInt64Array::from(rows.iter().map(|r| r.group).collect::<Vec<_>>())),
            "group" => Arc::new(StringArray::from(rows.iter().map(|r| names.get(&r.group).cloned().unwrap_or_default()).collect::<Vec<_>>())),
            "t" | "label" => Arc::new(TimestampNanosecondArray::from(rows.iter().map(|r| r.t).collect::<Vec<_>>())),
            "r" => Arc::new(TimestampNanosecondArray::from(rows.iter().map(|r| r.r).collect::<Vec<_>>())),
            "value" => Arc::new(Float64Array::from(rows.iter().map(|r| r.value).collect::<Vec<_>>())),
            "count" => Arc::new(Int64Array::from(rows.iter().map(|r| r.count).collect::<Vec<_>>())),
            _ => Arc::new(StringArray::from(rows.iter().map(|r| r.error.clone()).collect::<Vec<_>>())),
        };
        cols.push(col);
    }
    Ok(RecordBatch::try_new(Arc::new(arrow::datatypes::Schema::new(fields)), cols)?)
}

/// The input's rows of one group in time order: (t, values) of the rows
/// with values, and (t, r) of every row, the receipts an answer waits on.
#[derive(Default)]
struct Series {
    rows: Vec<(i64, [f64; 2])>,
    recv: Vec<(i64, i64)>,
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
}

/// The pass: the request's rows, in (t, group) order, and the texts of the
/// labels that key a per-label grouping.
fn compute(store: &Store, view: &ResolvedView, request: &AggregateRequest) -> Result<(Vec<Out>, BTreeMap<u64, String>), DataError> {
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
    // The rows admitted by the receipt filter, latest revision of each fact.
    let filter = RowFilter {
        r_max: request.receipt_filter.as_ref().and_then(|r| r.r_max),
        as_of_seq: view.as_of_seq,
        latest_only: true,
        ..RowFilter::default()
    };
    let batches = store.read(&version, &filter)?;
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
    use arrow::array::Array;
    let mut groups: BTreeMap<GroupKey, Series> = BTreeMap::new();
    let mut times: Vec<i64> = Vec::new();
    for b in &batches {
        let ts = rows::times(b, lay.t);
        let rs = lay.r.map(|r| rows::times(b, r));
        for i in 0..b.num_rows() {
            let g = match gc {
                Some(c) if s.columns[c].ty == ColumnType::Entity => b.column(c).as_primitive::<arrow::datatypes::UInt64Type>().value(i),
                Some(c) => b.column(c).as_primitive::<arrow::datatypes::UInt32Type>().value(i) as u64,
                None => 0,
            };
            let t = ts.value(i);
            let series = groups.entry(g).or_default();
            series.recv.push((t, rs.map_or(t, |r| r.value(i))));
            if b.column(vc).is_null(i) || wc.is_some_and(|w| b.column(w).is_null(i)) {
                continue;
            }
            let x = [value(b, vc, i), wc.map(|w| value(b, w, i)).unwrap_or(0.0)];
            series.rows.push((t, x));
            times.push(t);
        }
    }
    let names: BTreeMap<u64, String> = if request.group == Grouping::PerLabel {
        groups.keys().map(|&c| (c, store.registry.label_text(c as u32).unwrap_or("").to_string())).collect()
    } else {
        BTreeMap::new()
    };
    // The labels the windows walk.
    let labels: Vec<i64> = match &request.calendar {
        Some(c) => calendar_labels(store, view, c)?,
        None => {
            times.sort_unstable();
            times.dedup();
            times
        }
    };
    let buckets = if request.window == WindowKind::Bucket {
        let c = request.bucket.as_deref().ok_or_else(|| DataError::Malformed("a bucket window names its calendar".into()))?;
        calendar_labels(store, view, c)?
    } else {
        Vec::new()
    };
    let mode = match request.numerics {
        Numerics::Running => Mode::Running,
        Numerics::TwoPass => Mode::TwoPass,
    };
    let (t0, t1) = (request.t0.unwrap_or(i64::MIN), request.t1.unwrap_or(i64::MAX));
    // Per group, in parallel over disjoint ranges of groups; each group's
    // sequence is computed in time order by one thread, and the output is
    // put in (t, group) order after (A-D11).
    let groups: Vec<(GroupKey, Series)> = groups.into_iter().collect();
    let chunk = groups.len().div_ceil(threads()).max(if THREADS.load(std::sync::atomic::Ordering::Relaxed) == 0 { 64 } else { 1 });
    let ctx = Ctx {
        f,
        mode,
        request,
        labels: &labels,
        buckets: &buckets,
        t0,
        t1,
    };
    let mut out: Vec<Out> = std::thread::scope(|sc| {
        let handles: Vec<_> = groups
            .chunks(chunk)
            .map(|part| sc.spawn(|| part.iter().flat_map(|(g, series)| ctx.group(*g, series)).collect::<Vec<Out>>()))
            .collect();
        handles.into_iter().flat_map(|h| h.join().expect("an aggregate thread")).collect()
    });
    out.sort_by_key(|o| (o.t, o.group));
    Ok((out, names))
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
    request: &'a AggregateRequest,
    labels: &'a [i64],
    buckets: &'a [i64],
    t0: i64,
    t1: i64,
}

impl Ctx<'_> {
    #[allow(clippy::too_many_arguments)]
    fn emit(&self, out: &mut Vec<Out>, series: &Series, received: &[i64], t: i64, group: GroupKey, count: usize, v: Result<Option<f64>, AggError>) {
        if t < self.t0 || t > self.t1 {
            return;
        }
        let upto = series.recv.partition_point(|x| x.0 <= t);
        let r = if upto == 0 { i64::MIN } else { received[upto - 1] };
        match v {
            Ok(Some(x)) => out.push(Out {
                t,
                group,
                value: Some(x),
                count: count as i64,
                error: None,
                r,
            }),
            Ok(None) => {}
            Err(e) => out.push(Out {
                t,
                group,
                value: None,
                count: count as i64,
                error: Some(e.to_string()),
                r,
            }),
        }
    }

    /// One group's answer rows, in time order.
    fn group(&self, g: GroupKey, all: &Series) -> Vec<Out> {
        let r = self.request;
        let mut out = Vec::new();
        let received = all.prefix_receipts();
        let series = &all.rows;
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
                let mut w = RowsWindow::new(self.f, mode, n);
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
                        self.emit(&mut out, all, &received, t, g, count, v);
                    }
                    w.step(index as u64, t, row.is_some(), row.as_ref().map(|x| x.as_slice()));
                    if !prior && row.is_some() {
                        let count = w.count();
                        let v = w.value(min);
                        self.emit(&mut out, all, &received, t, g, count, v);
                    }
                }
            }
            WindowKind::Span | WindowKind::PriorSpan => {
                let d = r.span.as_deref().cloned().unwrap_or_default();
                let min = r.min.map(|m| m as usize).unwrap_or(1);
                let prior = r.window == WindowKind::PriorSpan;
                let mut w = WindowState::new(self.f, Mode::TwoPass);
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
                            self.emit(&mut out, all, &received, t, g, w.len(), v);
                        }
                        for row in &series[i..j] {
                            w.push(t, &row.1);
                        }
                        if !prior {
                            let v = w.value(min);
                            self.emit(&mut out, all, &received, t, g, w.len(), v);
                        }
                    }
                    i = j;
                }
            }
            WindowKind::Bucket => {
                let min = r.min.map(|m| m as usize).unwrap_or(1);
                let mut w = WindowState::new(self.f, Mode::TwoPass);
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
                        self.emit(&mut out, all, &received, l, g, w.len(), v);
                    }
                    w = WindowState::new(self.f, Mode::TwoPass);
                }
            }
        }
        out
    }
}

/// The answer of a cache object as the protocol carries it (P-4).
pub fn answer(store: &Store, version: &str) -> Result<RecordBatch, DataError> {
    let m = store.manifest(version)?;
    let b = store.read_all(version)?;
    rows::answer(&m.schema, &b, store.registry.labels(), false)
}
