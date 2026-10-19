//! Adapters (D-7.7): a source's rows into the store. This module is what
//! every adapter shares: rows whose instruments are named by the source's own
//! keys (a bundle's security ids, a generator's names) are attributed to the
//! registry's ids through `vendor_id` (Proposition D-2.4), a key the registry
//! has not seen being assigned the next id with its `instrument` row, and
//! the source's security table becomes `ticker` spans and contract terms
//! (ERRATA E-34). `bundle_v2` reads the present bundle formats into these
//! tables; the kernel's in-memory test builder produces them directly.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use abt_wire::generated::abt::wire::{ColumnType, Role};
use arrow::array::{Array, ArrayRef, AsArray, BooleanArray, Float64Array, RecordBatch, StringArray, TimestampNanosecondArray, UInt64Array};
use arrow::datatypes::{DataType, Field, Schema, TimeUnit};

use crate::error::DataError;
use crate::rows;
use crate::store::{RowFilter, Store};

/// The source the registry's own relations are written under, so that each
/// has one current version whatever adapter extended it.
pub const REGISTRY_SOURCE: &str = "registry";

/// A span of an instrument's ticker, as a source's security table holds it.
#[derive(Clone, Debug, PartialEq)]
pub struct Security {
    pub key: String,
    pub ticker: String,
    pub from: i64,
    pub to: Option<i64>,
}

/// An instrument's class and contract terms, as its source declares them
/// (a `multiplier` marks a declared contract; none is a share).
#[derive(Clone, Debug, PartialEq)]
pub struct Terms {
    pub key: String,
    pub class: String,
    pub multiplier: Option<f64>,
    pub commission_per_contract: Option<f64>,
}

/// The security table's Arrow form (`key`, `ticker`, `from`, `to`), the
/// interchange of adapters and in-memory builders.
pub fn securities_schema() -> Arc<Schema> {
    let ts = DataType::Timestamp(TimeUnit::Nanosecond, None);
    Arc::new(Schema::new(vec![
        Field::new("key", DataType::Utf8, false),
        Field::new("ticker", DataType::Utf8, false),
        Field::new("from", ts.clone(), false),
        Field::new("to", ts, true),
    ]))
}

/// The contract terms' Arrow form (`key`, `class`, `multiplier`,
/// `commission_per_contract`).
pub fn terms_schema() -> Arc<Schema> {
    Arc::new(Schema::new(vec![
        Field::new("key", DataType::Utf8, false),
        Field::new("class", DataType::Utf8, false),
        Field::new("multiplier", DataType::Float64, true),
        Field::new("commission_per_contract", DataType::Float64, true),
    ]))
}

fn column(b: &RecordBatch, n: &str, ty: &DataType) -> Result<ArrayRef, DataError> {
    let c = b.column_by_name(n).cloned().ok_or_else(|| DataError::Schema(format!("the table has no column `{}`", n)))?;
    Ok(arrow::compute::cast(&c, ty)?)
}

/// A security table from its Arrow form.
pub fn securities_of(b: &RecordBatch) -> Result<Vec<Security>, DataError> {
    let ns = DataType::Timestamp(TimeUnit::Nanosecond, None);
    let (key, ticker) = (column(b, "key", &DataType::Utf8)?, column(b, "ticker", &DataType::Utf8)?);
    let (from, to) = (column(b, "from", &ns)?, column(b, "to", &ns)?);
    let (key, ticker) = (key.as_string::<i32>(), ticker.as_string::<i32>());
    let (from, to) = (
        from.as_primitive::<arrow::datatypes::TimestampNanosecondType>(),
        to.as_primitive::<arrow::datatypes::TimestampNanosecondType>(),
    );
    Ok((0..b.num_rows())
        .map(|i| Security {
            key: key.value(i).to_string(),
            ticker: ticker.value(i).to_string(),
            from: from.value(i),
            to: (!to.is_null(i)).then(|| to.value(i)),
        })
        .collect())
}

/// Contract terms from their Arrow form.
pub fn terms_of(b: &RecordBatch) -> Result<Vec<Terms>, DataError> {
    let (key, class) = (column(b, "key", &DataType::Utf8)?, column(b, "class", &DataType::Utf8)?);
    let (mult, comm) = (column(b, "multiplier", &DataType::Float64)?, column(b, "commission_per_contract", &DataType::Float64)?);
    let (key, class) = (key.as_string::<i32>(), class.as_string::<i32>());
    let (mult, comm) = (mult.as_primitive::<arrow::datatypes::Float64Type>(), comm.as_primitive::<arrow::datatypes::Float64Type>());
    Ok((0..b.num_rows())
        .map(|i| Terms {
            key: key.value(i).to_string(),
            class: class.value(i).to_string(),
            multiplier: (!mult.is_null(i)).then(|| mult.value(i)),
            commission_per_contract: (!comm.is_null(i)).then(|| comm.value(i)),
        })
        .collect())
}

/// The registry's ids of a vendor's keys, as `vendor_id` holds them.
fn known_ids(store: &Store, vendor: &str) -> Result<BTreeMap<String, u64>, DataError> {
    let mut out = BTreeMap::new();
    let Some(v) = store.registry.current("vendor_id", Some(REGISTRY_SOURCE)) else { return Ok(out) };
    let Some(code) = store.registry.label_code(vendor) else { return Ok(out) };
    let s = store.schema("vendor_id")?;
    let pos = |n: &str| s.columns.iter().position(|c| c.name == n).expect("the registry's vendor_id");
    for b in store.read(
        v,
        &RowFilter {
            latest_only: true,
            ..RowFilter::default()
        },
    )? {
        let ids = b.column(pos("id")).as_primitive::<arrow::datatypes::UInt64Type>();
        let vendors = b.column(pos("vendor")).as_primitive::<arrow::datatypes::UInt32Type>();
        let keys = b.column(pos("key")).as_primitive::<arrow::datatypes::UInt32Type>();
        for i in 0..b.num_rows() {
            if vendors.value(i) == code {
                out.insert(store.registry.label_text(keys.value(i)).unwrap_or("").to_string(), ids.value(i));
            }
        }
    }
    Ok(out)
}

/// The registry's ticker spans by symbol: `(from, to, id)`, `to` the last
/// instant carried (none while carried), in the order of their starts.
pub type TickerSpans = BTreeMap<String, Vec<(i64, Option<i64>, u64)>>;

/// The store's [`TickerSpans`].
pub fn ticker_spans(store: &Store) -> Result<TickerSpans, DataError> {
    let mut out = TickerSpans::new();
    let Some(v) = store.registry.current("ticker", Some(REGISTRY_SOURCE)) else { return Ok(out) };
    let s = store.schema("ticker")?;
    let pos = |n: &str| s.columns.iter().position(|c| c.name == n).expect("the registry's ticker");
    for b in store.read(
        v,
        &RowFilter {
            latest_only: true,
            ..RowFilter::default()
        },
    )? {
        let ids = b.column(pos("id")).as_primitive::<arrow::datatypes::UInt64Type>();
        let from = b.column(pos("t")).as_primitive::<arrow::datatypes::TimestampNanosecondType>();
        let to = b.column(pos("to")).as_primitive::<arrow::datatypes::TimestampNanosecondType>();
        let symbols = b.column(pos("symbol")).as_primitive::<arrow::datatypes::UInt32Type>();
        for i in 0..b.num_rows() {
            let symbol = store.registry.label_text(symbols.value(i)).unwrap_or("").to_string();
            out.entry(symbol).or_default().push((from.value(i), (!to.is_null(i)).then(|| to.value(i)), ids.value(i)));
        }
    }
    for spans in out.values_mut() {
        spans.sort();
    }
    Ok(out)
}

/// What an ingest of keyed tables wrote: each relation's new version, and
/// the registry's ids of the source's keys.
#[derive(Clone, Debug, Default)]
pub struct Ingested {
    pub versions: BTreeMap<String, String>,
    pub ids: BTreeMap<String, u64>,
}

/// Ingests a source whose entities are named by `vendor`'s keys: the
/// security table (may be empty: then each key is its own symbol and no
/// ticker span is recorded), the contract terms it declares, and each
/// relation's rows, entity columns as text keys or as registry ids.
/// Relation names are the store's (`equity.close`); a key first seen in a
/// relation's rows becomes an instrument of that relation's class, listed
/// from its first row. The relations' versions carry `attributes` (what the
/// adapter or a reviewer states of them: `reviewed = complete`, the
/// `calendar` of the bars, D-2.1).
pub fn ingest_keyed(
    store: &mut Store,
    vendor: &str,
    securities: &[Security],
    terms: &[Terms],
    relations: &[(String, RecordBatch)],
    source: &str,
    attributes: &BTreeMap<String, String>,
) -> Result<Ingested, DataError> {
    ingest_keyed_with(store, vendor, securities, terms, relations, source, attributes, &BTreeMap::new())
}

/// The bar relations of a market held hot (07-storage.md; A-D49): the
/// `per` entry of `ingest_keyed_with` naming `storage = hot` for the
/// open, high, low, close and volume relations `relations` carries.
pub fn hot_bars(relations: &[(String, RecordBatch)]) -> BTreeMap<String, BTreeMap<String, String>> {
    relations
        .iter()
        .filter(|(name, _)| matches!(name.rsplit_once('.').map(|(_, s)| s).unwrap_or(name), "open" | "high" | "low" | "close" | "volume"))
        .map(|(name, _)| (name.clone(), BTreeMap::from([("storage".to_string(), "hot".to_string())])))
        .collect()
}

/// [`ingest_keyed`], with what the source states of some relations only:
/// a relation's version carries `attributes` and then its own `per` entry.
#[allow(clippy::too_many_arguments)]
pub fn ingest_keyed_with(
    store: &mut Store,
    vendor: &str,
    securities: &[Security],
    terms: &[Terms],
    relations: &[(String, RecordBatch)],
    source: &str,
    attributes: &BTreeMap<String, String>,
    per: &BTreeMap<String, BTreeMap<String, String>>,
) -> Result<Ingested, DataError> {
    let mut ids = known_ids(store, vendor)?;
    // New keys, in the order the source names them: its security table,
    // then its relations' rows.
    struct New {
        key: String,
        class: String,
        from: i64,
        to: Option<i64>,
        multiplier: Option<f64>,
        commission: Option<f64>,
    }
    let mut fresh: Vec<New> = Vec::new();
    let mut fresh_at: BTreeMap<String, usize> = BTreeMap::new();
    let terms_of: BTreeMap<&str, &Terms> = terms.iter().map(|t| (t.key.as_str(), t)).collect();
    for s in securities {
        match fresh_at.get(&s.key) {
            Some(&i) => {
                let n = &mut fresh[i];
                n.from = n.from.min(s.from);
                n.to = match (n.to, s.to) {
                    (Some(a), Some(b)) => Some(a.max(b)),
                    _ => None,
                };
            }
            None if !ids.contains_key(&s.key) => {
                let t = terms_of.get(s.key.as_str());
                fresh_at.insert(s.key.clone(), fresh.len());
                fresh.push(New {
                    key: s.key.clone(),
                    class: t.map(|t| t.class.clone()).unwrap_or_default(),
                    from: s.from,
                    to: s.to,
                    multiplier: t.and_then(|t| t.multiplier),
                    commission: t.and_then(|t| t.commission_per_contract),
                });
            }
            None => {}
        }
    }
    let mut converted: Vec<(String, RecordBatch)> = Vec::new();
    for (relation, batch) in relations {
        let schema = store.schema(relation)?;
        let class = schema.class.clone();
        let lay = rows::layout(&schema);
        let t_col = batch.column_by_name(&schema.columns[lay.t].name).cloned();
        let times: Option<Vec<i64>> = match &t_col {
            Some(c) => {
                let c = arrow::compute::cast(c, &DataType::Timestamp(TimeUnit::Nanosecond, None))?;
                Some(c.as_primitive::<arrow::datatypes::TimestampNanosecondType>().values().to_vec())
            }
            None => None,
        };
        let mut cols: Vec<ArrayRef> = Vec::with_capacity(batch.num_columns());
        let mut fields: Vec<Field> = Vec::with_capacity(batch.num_columns());
        for (j, f) in batch.schema().fields().iter().enumerate() {
            let col = batch.column(j);
            let spec = schema.columns.iter().find(|c| c.name == *f.name());
            let is_key = spec.is_some_and(|c| c.ty == ColumnType::Entity) && matches!(col.data_type(), DataType::Utf8 | DataType::LargeUtf8 | DataType::Dictionary(_, _));
            if !is_key {
                cols.push(col.clone());
                fields.push(f.as_ref().clone());
                continue;
            }
            let text = arrow::compute::cast(col, &DataType::Utf8)?;
            let text = text.as_string::<i32>();
            let mut out = Vec::with_capacity(text.len());
            for i in 0..text.len() {
                if text.is_null(i) {
                    out.push(None);
                    continue;
                }
                let k = text.value(i);
                let t = times.as_ref().map(|v| v[i]).unwrap_or(i64::MAX);
                if let Some(&id) = ids.get(k) {
                    out.push(Some(id));
                    continue;
                }
                let at = match fresh_at.get(k) {
                    Some(&at) => at,
                    None => {
                        let terms = terms_of.get(k);
                        fresh_at.insert(k.to_string(), fresh.len());
                        fresh.push(New {
                            key: k.to_string(),
                            class: terms.map(|x| x.class.clone()).unwrap_or_else(|| class.clone()),
                            from: t,
                            to: None,
                            multiplier: terms.and_then(|x| x.multiplier),
                            commission: terms.and_then(|x| x.commission_per_contract),
                        });
                        fresh.len() - 1
                    }
                };
                // A key without a security row is listed from its first row.
                if !securities.iter().any(|s| s.key == k) {
                    fresh[at].from = fresh[at].from.min(t);
                }
                out.push(Some(u64::MAX - at as u64));
            }
            cols.push(Arc::new(UInt64Array::from(out)));
            fields.push(Field::new(f.name(), DataType::UInt64, f.is_nullable()));
        }
        converted.push((relation.clone(), RecordBatch::try_new(Arc::new(Schema::new(fields)), cols)?));
    }
    // Keys only the terms name.
    for t in terms {
        if !ids.contains_key(&t.key) && !fresh_at.contains_key(&t.key) {
            fresh_at.insert(t.key.clone(), fresh.len());
            fresh.push(New {
                key: t.key.clone(),
                class: t.class.clone(),
                from: i64::MIN,
                to: None,
                multiplier: t.multiplier,
                commission: t.commission_per_contract,
            });
        }
    }
    // Assign ids in the order the keys were first seen, and register them.
    let mut placeholder: BTreeMap<u64, u64> = BTreeMap::new();
    for (at, n) in fresh.iter().enumerate() {
        let id = store.registry.new_id();
        ids.insert(n.key.clone(), id);
        placeholder.insert(u64::MAX - at as u64, id);
    }
    if !fresh.is_empty() {
        let ts = |v: Vec<Option<i64>>| -> ArrayRef { Arc::new(TimestampNanosecondArray::from(v)) };
        let strs = |v: Vec<&str>| -> ArrayRef { Arc::new(StringArray::from(v)) };
        let idcol: ArrayRef = Arc::new(UInt64Array::from(fresh.iter().map(|n| ids[&n.key]).collect::<Vec<_>>()));
        let instrument = RecordBatch::try_from_iter(vec![
            ("id", idcol.clone()),
            ("from", ts(fresh.iter().map(|n| Some(n.from)).collect())),
            ("to", ts(fresh.iter().map(|n| n.to).collect())),
            ("class", strs(fresh.iter().map(|n| n.class.as_str()).collect())),
            ("exchange", strs(fresh.iter().map(|_| "").collect())),
            ("currency", strs(fresh.iter().map(|_| "USD").collect())),
            ("multiplier", Arc::new(Float64Array::from(fresh.iter().map(|n| n.multiplier).collect::<Vec<_>>())) as ArrayRef),
            (
                "commission_per_contract",
                Arc::new(Float64Array::from(fresh.iter().map(|n| n.commission).collect::<Vec<_>>())) as ArrayRef,
            ),
        ])?;
        store.ingest("instrument", &[instrument], REGISTRY_SOURCE, BTreeMap::new())?;
        let vendor_rows = RecordBatch::try_from_iter(vec![
            ("id", idcol),
            ("from", ts(fresh.iter().map(|n| Some(n.from)).collect())),
            ("to", ts(fresh.iter().map(|n| n.to).collect())),
            ("vendor", strs(fresh.iter().map(|_| vendor).collect())),
            ("key", strs(fresh.iter().map(|n| n.key.as_str()).collect())),
        ])?;
        store.ingest("vendor_id", &[vendor_rows], REGISTRY_SOURCE, BTreeMap::new())?;
    }
    if !securities.is_empty() {
        let ticker = RecordBatch::try_from_iter(vec![
            ("id", Arc::new(UInt64Array::from(securities.iter().map(|s| ids[&s.key]).collect::<Vec<_>>())) as ArrayRef),
            ("t", Arc::new(TimestampNanosecondArray::from(securities.iter().map(|s| s.from).collect::<Vec<_>>())) as ArrayRef),
            ("symbol", Arc::new(StringArray::from(securities.iter().map(|s| s.ticker.as_str()).collect::<Vec<_>>())) as ArrayRef),
            ("to", Arc::new(TimestampNanosecondArray::from(securities.iter().map(|s| s.to).collect::<Vec<_>>())) as ArrayRef),
        ])?;
        store.ingest("ticker", &[ticker], REGISTRY_SOURCE, BTreeMap::new())?;
    }
    // The relations, their placeholders replaced by the ids.
    let mut versions = BTreeMap::new();
    for (relation, b) in converted {
        let mut cols: Vec<ArrayRef> = Vec::with_capacity(b.num_columns());
        for c in b.columns() {
            if c.data_type() == &DataType::UInt64 {
                let a = c.as_primitive::<arrow::datatypes::UInt64Type>();
                cols.push(Arc::new(a.iter().map(|v| v.map(|v| placeholder.get(&v).copied().unwrap_or(v))).collect::<UInt64Array>()));
            } else {
                cols.push(c.clone());
            }
        }
        let b = RecordBatch::try_new(b.schema(), cols)?;
        let mut a = attributes.clone();
        a.extend(per.get(&relation).cloned().unwrap_or_default());
        // The registry's relations (a contract's terms, a chain) have one
        // current version, whatever source extended them (D-2.4).
        let into = if store.schema(&relation)?.class == "registry" { REGISTRY_SOURCE } else { source };
        let v = store.ingest(&relation, &[b], into, a)?;
        versions.insert(relation, v);
    }
    Ok(Ingested { versions, ids })
}

/// A relation a source provides with no rows (a market without
/// delistings), as a version of its own, so that a demand naming it is
/// answered (D-7.2) and reads it empty. A relation the source already holds
/// a version of is left as it is.
pub fn ingest_empty(store: &mut Store, relation: &str, source: &str, attributes: &BTreeMap<String, String>) -> Result<Option<String>, DataError> {
    if store.registry.current(relation, Some(source)).is_some() {
        return Ok(None);
    }
    let s = store.schema(relation)?;
    let input = |ty: ColumnType| match ty {
        ColumnType::Entity => DataType::UInt64,
        ColumnType::Time => DataType::Timestamp(TimeUnit::Nanosecond, None),
        ColumnType::Num => DataType::Float64,
        ColumnType::Count => DataType::Int64,
        ColumnType::Label => DataType::Utf8,
        ColumnType::Bool => DataType::Boolean,
        ColumnType::Bytes => DataType::Binary,
        ColumnType::Tick => DataType::Decimal128(18, 9),
    };
    let fields: Vec<Field> = s.columns.iter().filter(|c| c.role != Role::ReceiptTime).map(|c| Field::new(&c.name, input(c.ty), true)).collect();
    let empty = RecordBatch::new_empty(Arc::new(Schema::new(fields)));
    Ok(Some(store.ingest(relation, &[empty], source, attributes.clone())?))
}

/// The instruments of a store: id, key (of the first vendor naming it),
/// class and contract terms, from the registry's relations.
#[derive(Clone, Debug, PartialEq)]
pub struct Instrument {
    pub id: u64,
    pub key: String,
    pub class: String,
    pub from: i64,
    pub to: Option<i64>,
    pub multiplier: Option<f64>,
    pub commission_per_contract: Option<f64>,
}

/// Every instrument of the store.
pub fn instruments(store: &Store) -> Result<Vec<Instrument>, DataError> {
    let mut keys: BTreeMap<u64, String> = BTreeMap::new();
    if let Some(v) = store.registry.current("vendor_id", Some(REGISTRY_SOURCE)) {
        let s = store.schema("vendor_id")?;
        let pos = |n: &str| s.columns.iter().position(|c| c.name == n).expect("the registry's vendor_id");
        for b in store.read(
            v,
            &RowFilter {
                latest_only: true,
                ..RowFilter::default()
            },
        )? {
            let ids = b.column(pos("id")).as_primitive::<arrow::datatypes::UInt64Type>();
            let k = b.column(pos("key")).as_primitive::<arrow::datatypes::UInt32Type>();
            for i in 0..b.num_rows() {
                keys.entry(ids.value(i)).or_insert_with(|| store.registry.label_text(k.value(i)).unwrap_or("").to_string());
            }
        }
    }
    let mut out = Vec::new();
    let Some(v) = store.registry.current("instrument", Some(REGISTRY_SOURCE)) else { return Ok(out) };
    let s = store.schema("instrument")?;
    let pos = |n: &str| s.columns.iter().position(|c| c.name == n).expect("the registry's instrument");
    let mut seen = BTreeSet::new();
    for b in store.read(
        v,
        &RowFilter {
            latest_only: true,
            ..RowFilter::default()
        },
    )? {
        let ids = b.column(pos("id")).as_primitive::<arrow::datatypes::UInt64Type>();
        let from = rows::times(&b, pos("from"));
        let to = b.column(pos("to")).as_primitive::<arrow::datatypes::TimestampNanosecondType>();
        let class = b.column(pos("class")).as_primitive::<arrow::datatypes::UInt32Type>();
        let mult = rows::floats(&b, pos("multiplier"));
        let comm = rows::floats(&b, pos("commission_per_contract"));
        for i in 0..b.num_rows() {
            let id = ids.value(i);
            if !seen.insert(id) {
                continue;
            }
            out.push(Instrument {
                id,
                key: keys.get(&id).cloned().unwrap_or_else(|| id.to_string()),
                class: store.registry.label_text(class.value(i)).unwrap_or("").to_string(),
                from: from.value(i),
                to: (!to.is_null(i)).then(|| to.value(i)),
                multiplier: (!mult.is_null(i)).then(|| mult.value(i)),
                commission_per_contract: (!comm.is_null(i)).then(|| comm.value(i)),
            });
        }
    }
    out.sort_by_key(|x| x.id);
    Ok(out)
}

/// A boolean column of `n` values (for adapters building tables).
pub fn bools(v: Vec<bool>) -> ArrayRef {
    Arc::new(BooleanArray::from(v))
}

/// The roles of a relation's columns, for adapters that map by name.
pub fn column_roles(store: &Store, relation: &str) -> Result<Vec<(String, ColumnType, Role)>, DataError> {
    Ok(store.schema(relation)?.columns.iter().map(|c| (c.name.clone(), c.ty, c.role)).collect())
}
