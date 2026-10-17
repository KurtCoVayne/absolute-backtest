//! The option class's registry and its chains (D-2.4 `option_spec` and
//! `chain_index`, D-4.4). A contract's terms are the latest `option_spec`
//! row of its id; the chain of an underlying at `t` is the `chain_index`
//! snapshot of `t`'s day taken at or before `t`, each contract kept when the
//! filters admit it against the spot the caller supplies
//! (`abt_wire::chain::admits`, the one implementation the kernel's `chain`
//! relation reads too). A chain with no snapshot that day is empty and
//! unresolved, which is what the resolver's dynamic demand reads to decide a
//! fetch (D-7.6).

use std::collections::BTreeMap;
use std::sync::Arc;

use abt_wire::generated::abt::wire::Chain;
use arrow::array::{Array, ArrayRef, AsArray, Float64Array, RecordBatch, StringArray, TimestampNanosecondArray, UInt64Array};
use arrow::datatypes::{DataType, Field, Schema, SchemaRef, TimeUnit};

use crate::adapter::REGISTRY_SOURCE;
use crate::error::DataError;
use crate::query::ResolvedView;
use crate::rows;
use crate::store::{RowFilter, Store};

const DAY: i64 = 86_400_000_000_000;

/// A contract's terms (D-2.4 `option_spec`).
#[derive(Clone, Debug, PartialEq)]
pub struct OptionContract {
    pub id: u64,
    /// When its terms were first known.
    pub listed: i64,
    pub underlying: u64,
    /// `call` or `put`.
    pub right: String,
    pub strike: f64,
    pub expiry: i64,
    /// `american` or `european`.
    pub style: String,
    /// `physical` or `cash`.
    pub settlement: String,
    pub multiplier: f64,
    pub occ: String,
    /// `am` or `pm` (E-70).
    pub settlement_time: String,
}

/// The version a view reads a registry relation at: the view's, else the
/// registry's current one; none when the store holds none.
fn registry_version(store: &Store, view: &ResolvedView, relation: &str) -> Option<String> {
    view.versions
        .get(relation)
        .cloned()
        .or_else(|| store.registry.current(relation, Some(REGISTRY_SOURCE)).map(str::to_string))
}

fn label(store: &Store, b: &RecordBatch, col: usize, i: usize) -> String {
    let codes = b.column(col).as_primitive::<arrow::datatypes::UInt32Type>();
    if codes.is_null(i) {
        return String::new();
    }
    store.registry.label_text(codes.value(i)).unwrap_or("").to_string()
}

/// Every contract's terms in the view, received by `r_max`: the latest row
/// of each id.
pub fn contracts(store: &Store, view: &ResolvedView, r_max: Option<i64>) -> Result<BTreeMap<u64, OptionContract>, DataError> {
    let mut out = BTreeMap::new();
    let Some(v) = registry_version(store, view, "option_spec") else { return Ok(out) };
    let s = store.schema("option_spec")?;
    let pos = |n: &str| {
        s.columns
            .iter()
            .position(|c| c.name == n)
            .ok_or_else(|| DataError::Schema(format!("option_spec has no column `{}`", n)))
    };
    let (id, listed, und, right, strike, expiry) = (pos("id")?, pos("listed")?, pos("underlying")?, pos("right")?, pos("strike")?, pos("expiry")?);
    let (style, settlement, mult, occ, stime) = (pos("style")?, pos("settlement")?, pos("multiplier")?, pos("occ")?, pos("settlement_time")?);
    let filter = RowFilter {
        r_max,
        as_of_seq: view.as_of_seq,
        latest_only: true,
        ..RowFilter::default()
    };
    for b in store.read(&v, &filter)? {
        let ids = b.column(id).as_primitive::<arrow::datatypes::UInt64Type>();
        let unds = b.column(und).as_primitive::<arrow::datatypes::UInt64Type>();
        let (lst, exp) = (rows::times(&b, listed), rows::times(&b, expiry));
        let (k, m) = (rows::floats(&b, strike), rows::floats(&b, mult));
        for i in 0..b.num_rows() {
            // Rows are in (t, id) order: a later row of an id replaces it.
            out.insert(
                ids.value(i),
                OptionContract {
                    id: ids.value(i),
                    listed: lst.value(i),
                    underlying: unds.value(i),
                    right: label(store, &b, right, i),
                    strike: k.value(i),
                    expiry: exp.value(i),
                    style: label(store, &b, style, i),
                    settlement: label(store, &b, settlement, i),
                    multiplier: if m.is_null(i) { 100.0 } else { m.value(i) },
                    occ: label(store, &b, occ, i),
                    settlement_time: label(store, &b, stime, i),
                },
            );
        }
    }
    Ok(out)
}

/// The ids of `underlying`'s chain at `t`: the `chain_index` snapshot of
/// `t`'s day taken at or before `t` and received by `r_max`; `None` when the
/// view holds no row of that day.
pub fn chain_ids(store: &Store, view: &ResolvedView, underlying: u64, t: i64, r_max: Option<i64>) -> Result<Option<Vec<u64>>, DataError> {
    let Some(v) = registry_version(store, view, "chain_index") else { return Ok(None) };
    let s = store.schema("chain_index")?;
    let pos = |n: &str| {
        s.columns
            .iter()
            .position(|c| c.name == n)
            .ok_or_else(|| DataError::Schema(format!("chain_index has no column `{}`", n)))
    };
    let (und, id) = (pos("underlying")?, pos("id")?);
    let tc = rows::layout(&s).t;
    // The key is (underlying, id): the underlying is matched here.
    let filter = RowFilter {
        t0: Some(t.div_euclid(DAY) * DAY),
        t1: Some(t),
        identity: None,
        r_max,
        as_of_seq: view.as_of_seq,
        latest_only: true,
    };
    // The snapshot is the latest instant of the day with rows.
    let mut by_time: BTreeMap<i64, Vec<u64>> = BTreeMap::new();
    for b in store.read(&v, &filter)? {
        let unds = b.column(und).as_primitive::<arrow::datatypes::UInt64Type>();
        let ids = b.column(id).as_primitive::<arrow::datatypes::UInt64Type>();
        let ts = rows::times(&b, tc);
        for i in (0..b.num_rows()).filter(|&i| unds.value(i) == underlying) {
            by_time.entry(ts.value(i)).or_default().push(ids.value(i));
        }
    }
    Ok(by_time.pop_last().map(|(_, mut ids)| {
        ids.sort_unstable();
        ids.dedup();
        ids
    }))
}

/// The open interest of each id's latest `option.quote` row at or before
/// `t`, received by `r_max`.
pub fn open_interest(store: &Store, view: &ResolvedView, ids: &[u64], t: i64, r_max: Option<i64>) -> Result<BTreeMap<u64, i64>, DataError> {
    let mut out = BTreeMap::new();
    let Ok(v) = view.version(store, "option.quote") else { return Ok(out) };
    let s = store.schema("option.quote")?;
    let lay = rows::layout(&s);
    let Some(oi) = s.columns.iter().position(|c| c.name == "OI") else { return Ok(out) };
    let filter = RowFilter {
        t0: Some(t.saturating_sub(7 * DAY)),
        t1: Some(t),
        identity: Some(ids.to_vec()),
        r_max,
        as_of_seq: view.as_of_seq,
        latest_only: true,
    };
    for b in store.read(&v, &filter)? {
        let e = b.column(lay.identity[0]).as_primitive::<arrow::datatypes::UInt64Type>();
        let n = rows::floats(&b, oi);
        for i in 0..b.num_rows() {
            if !n.is_null(i) {
                out.insert(e.value(i), n.value(i) as i64);
            }
        }
    }
    Ok(out)
}

/// The chain query (D-4.4): the contracts of the chain admitted by the
/// filters, as `(id, right, strike, expiry, style, settlement, multiplier,
/// occ, settlement_time)`, in id order; and whether the chain had a snapshot
/// (an empty, unresolved chain otherwise).
pub fn chain(store: &Store, view: &ResolvedView, q: &Chain, r_max: Option<i64>) -> Result<(SchemaRef, Vec<RecordBatch>, bool), DataError> {
    let schema = chain_schema();
    let Some(ids) = chain_ids(store, view, q.underlying, q.t, r_max)? else {
        return Ok((schema.clone(), vec![RecordBatch::new_empty(schema)], false));
    };
    let terms = contracts(store, view, r_max)?;
    let filters = q.filters.as_deref().cloned().unwrap_or_default();
    let oi = if filters.min_open_interest.is_some() {
        open_interest(store, view, &ids, q.t, r_max)?
    } else {
        BTreeMap::new()
    };
    let admitted: Vec<&OptionContract> = ids
        .iter()
        .filter_map(|id| terms.get(id))
        .filter(|c| c.expiry >= q.t)
        .filter(|c| {
            abt_wire::chain::admits(
                &filters,
                q.t,
                q.spot,
                &abt_wire::chain::Terms {
                    expiry: c.expiry,
                    strike: c.strike,
                    right: &c.right,
                    style: &c.style,
                    open_interest: oi.get(&c.id).copied(),
                },
            )
        })
        .collect();
    let strs = |f: fn(&OptionContract) -> &str| -> ArrayRef { Arc::new(StringArray::from(admitted.iter().map(|c| f(c)).collect::<Vec<_>>())) };
    let b = RecordBatch::try_new(
        schema.clone(),
        vec![
            Arc::new(UInt64Array::from(admitted.iter().map(|c| c.id).collect::<Vec<_>>())),
            strs(|c| &c.right),
            Arc::new(Float64Array::from(admitted.iter().map(|c| c.strike).collect::<Vec<_>>())),
            Arc::new(TimestampNanosecondArray::from(admitted.iter().map(|c| c.expiry).collect::<Vec<_>>()).with_timezone("UTC")),
            strs(|c| &c.style),
            strs(|c| &c.settlement),
            Arc::new(Float64Array::from(admitted.iter().map(|c| c.multiplier).collect::<Vec<_>>())),
            strs(|c| &c.occ),
            strs(|c| &c.settlement_time),
        ],
    )?;
    Ok((schema, vec![b], true))
}

/// The columns of a chain query's answer.
pub fn chain_schema() -> SchemaRef {
    Arc::new(Schema::new(vec![
        Field::new("id", DataType::UInt64, false),
        Field::new("right", DataType::Utf8, false),
        Field::new("strike", DataType::Float64, false),
        Field::new("expiry", DataType::Timestamp(TimeUnit::Nanosecond, Some("UTC".into())), false),
        Field::new("style", DataType::Utf8, false),
        Field::new("settlement", DataType::Utf8, false),
        Field::new("multiplier", DataType::Float64, false),
        Field::new("occ", DataType::Utf8, false),
        Field::new("settlement_time", DataType::Utf8, false),
    ]))
}

/// An OCC option symbol (the OSI format): the root left-aligned in six
/// characters, the expiry as YYMMDD, `C` or `P`, and the strike in
/// thousandths in eight digits (`SPY   240315C00510000`).
pub fn occ_symbol(root: &str, expiry: i64, right: &str, strike: f64) -> String {
    let (y, m, d) = abt_wire::scalar::civil_from_days(expiry.div_euclid(DAY));
    format!(
        "{:<6}{:02}{:02}{:02}{}{:08}",
        root,
        y.rem_euclid(100),
        m,
        d,
        if right == "put" { "P" } else { "C" },
        (strike * 1000.0).round() as i64
    )
}
