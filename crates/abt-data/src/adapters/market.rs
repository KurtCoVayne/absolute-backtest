//! Markets held in memory, as the keyed ingest takes them: what the
//! kernel's `Dataset` holds, by key rather than by interned symbol. The
//! synthetic adapter makes one; the bundle adapter reads one; `tables_of`
//! turns one into the keyed tables under the registry's schemas, exactly as
//! the kernel's `bind::keyed_tables` turns a dataset (so a store the data
//! layer's binary fills is the store a run in one process reads, A-D2).

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use abt_wire::generated::abt::wire::{ColumnType, RelationSchema, Role};
use arrow::array::{ArrayRef, Float64Array, Int64Array, RecordBatch, StringArray, TimestampNanosecondArray};
use arrow::datatypes::{DataType, Field, Schema, TimeUnit};

use crate::adapter::{ingest_empty, ingest_keyed, Ingested, Security, Terms};
use crate::error::DataError;
use crate::store::Store;

/// The source a market held in memory (a dataset, a synthetic market, a
/// bundle) is ingested under; the front's `abt::SOURCE` is this string.
pub const SOURCE: &str = "dataset";

/// A value of a relation's row: an entity by its key, a label, an instant,
/// a number, a count.
#[derive(Clone, Debug, PartialEq)]
pub enum V {
    E(String),
    L(String),
    T(i64),
    N(f64),
    C(i64),
}

impl V {
    pub fn as_time(&self) -> Option<i64> {
        match self {
            V::T(t) => Some(*t),
            _ => None,
        }
    }
    pub fn as_f64(&self) -> Option<f64> {
        match self {
            V::N(x) => Some(*x),
            V::C(c) => Some(*c as f64),
            _ => None,
        }
    }
}

/// A market held in memory: what a `Dataset` holds, by key rather than by
/// interned symbol.
#[derive(Clone, Debug, Default)]
pub struct Market {
    pub securities: Vec<Security>,
    pub terms: Vec<Terms>,
    /// Each relation's rows in the order they were made (relations in name
    /// order, as a dataset's facts are).
    pub facts: BTreeMap<String, Vec<Vec<V>>>,
    /// A relation that records when each row became available, by row.
    pub available_at: BTreeMap<String, Vec<i64>>,
    /// The market's tests not passed (a loose directory, an untested bundle).
    pub unreviewed: bool,
    /// The bundle version the market was loaded from.
    pub version: Option<String>,
}

impl Market {
    pub fn add(&mut self, relation: &str, row: Vec<V>) {
        let key = row.iter().find_map(V::as_time);
        if let Some(a) = self.available_at.get_mut(relation) {
            a.push(key.unwrap_or(i64::MIN));
        }
        self.facts.entry(relation.to_string()).or_default().push(row);
    }
    /// Adds a row available at `avail`; the relation then records
    /// availability for every row.
    pub fn add_available(&mut self, relation: &str, row: Vec<V>, avail: i64) {
        if !self.available_at.contains_key(relation) {
            let existing: Vec<i64> = self
                .facts
                .get(relation)
                .map(|rows| rows.iter().map(|r| r.iter().find_map(V::as_time).unwrap_or(i64::MIN)).collect())
                .unwrap_or_default();
            self.available_at.insert(relation.to_string(), existing);
        }
        self.available_at.get_mut(relation).unwrap().push(avail);
        self.facts.entry(relation.to_string()).or_default().push(row);
    }
    pub fn add_security(&mut self, key: &str, ticker: &str, from: i64, to: Option<i64>) {
        self.securities.push(Security {
            key: key.to_string(),
            ticker: ticker.to_string(),
            from,
            to,
        });
    }
    /// Materialises `ticker(A, T, S)` from the security table over the
    /// bars of `universe` (or of every fact): one row per security and bar
    /// in its interval, in (security, bar) order; without a table every
    /// key is its own ticker at every bar it has a row. As the kernel's
    /// `Dataset::derive_tickers`.
    pub fn derive_tickers(&mut self) {
        let source: Vec<String> = if self.facts.contains_key("universe") {
            vec!["universe".to_string()]
        } else {
            self.facts.keys().cloned().collect()
        };
        let mut bars: BTreeSet<i64> = BTreeSet::new();
        for rel in &source {
            for row in &self.facts[rel] {
                bars.extend(row.iter().find_map(V::as_time));
            }
        }
        // Keys in the order they were first named, as a dataset's symbols
        // are interned.
        let mut order: BTreeMap<String, usize> = BTreeMap::new();
        for s in &self.securities {
            let n = order.len();
            order.entry(s.key.clone()).or_insert(n);
        }
        for rel in &source {
            for row in &self.facts[rel] {
                if let Some(V::E(k)) = row.iter().find(|v| matches!(v, V::E(_))) {
                    let n = order.len();
                    order.entry(k.clone()).or_insert(n);
                }
            }
        }
        let rows: Vec<(usize, i64, String, String)> = if self.securities.is_empty() {
            let mut present: BTreeSet<(usize, i64, String)> = BTreeSet::new();
            for rel in &source {
                for row in &self.facts[rel] {
                    let key = row.iter().find_map(|v| if let V::E(k) = v { Some(k) } else { None });
                    if let (Some(k), Some(t)) = (key, row.iter().find_map(V::as_time)) {
                        present.insert((order[k], t, k.clone()));
                    }
                }
            }
            present.into_iter().map(|(n, t, k)| (n, t, k.clone(), k)).collect()
        } else {
            let mut rows = Vec::new();
            for s in &self.securities {
                for &t in bars.range(s.from..) {
                    if s.to.map(|to| t >= to).unwrap_or(false) {
                        break;
                    }
                    rows.push((order[&s.key], t, s.key.clone(), s.ticker.clone()));
                }
            }
            rows.sort_by_key(|r| (r.0, r.1));
            rows
        };
        let out = rows.into_iter().map(|(_, t, key, ticker)| vec![V::E(key), V::T(t), V::L(ticker)]).collect();
        self.facts.insert("ticker".to_string(), out);
    }
}

fn short(stored: &str) -> &str {
    stored.rsplit_once('.').map(|(_, s)| s).unwrap_or(stored)
}

/// The relations a class reads from another's store (the option class
/// reads its underlyings' bars and actions from the equity class).
pub fn borrowed(class: &str, name: &str) -> Option<String> {
    let equity = ["open", "high", "low", "close", "volume", "split", "dividend", "special_dividend", "delisted"];
    (class == "option" && equity.contains(&name)).then(|| format!("equity.{}", name))
}

/// The store's schema of a market's relation of `class`: the class's own,
/// a borrowed one, else the one class that has it (the series class's
/// `value`). The kernel's `bind::schema_in`.
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

/// The keyed tables of a market: its security table, its contract terms
/// and each relation's rows.
pub type KeyedTables = (Vec<Security>, Vec<Terms>, Vec<(String, RecordBatch)>);

/// A market's keyed tables under the registry's schemas: the security
/// table, the contract terms and each relation's rows typed by its schema
/// (entities and labels as text, times as nanoseconds, counts as integers,
/// the rest as `f64`), with the receipt column when the relation records
/// availability (the greater of the recorded instant and the row's time).
/// Exactly the kernel's `bind::keyed_tables` of the same market.
pub fn tables_of(m: &Market, schema_of: &dyn Fn(&str) -> Option<RelationSchema>) -> Result<KeyedTables, DataError> {
    let ns = DataType::Timestamp(TimeUnit::Nanosecond, None);
    let mut relations = Vec::new();
    for (name, rows) in &m.facts {
        let schema = schema_of(name).ok_or_else(|| DataError::Schema(format!("the data layer has no relation `{}` in the market's classes", name)))?;
        let cols: Vec<_> = schema.columns.iter().filter(|c| c.role != Role::ReceiptTime).collect();
        if let Some(r) = rows.iter().find(|r| r.len() != cols.len()) {
            return Err(DataError::Schema(format!(
                "`{}` has {} columns in the data layer and a row of {} values",
                schema.name,
                cols.len(),
                r.len()
            )));
        }
        let mut fields = Vec::new();
        let mut arrays: Vec<ArrayRef> = Vec::new();
        for (i, c) in cols.iter().enumerate() {
            let (field, array): (Field, ArrayRef) = match c.ty {
                ColumnType::Entity => (
                    Field::new(&c.name, DataType::Utf8, true),
                    Arc::new(StringArray::from(rows.iter().map(|r| if let V::E(k) = &r[i] { Some(k.clone()) } else { None }).collect::<Vec<_>>())),
                ),
                ColumnType::Label => (
                    Field::new(&c.name, DataType::Utf8, true),
                    Arc::new(StringArray::from(rows.iter().map(|r| if let V::L(l) = &r[i] { Some(l.clone()) } else { None }).collect::<Vec<_>>())),
                ),
                ColumnType::Time => (
                    Field::new(&c.name, ns.clone(), true),
                    Arc::new(TimestampNanosecondArray::from(rows.iter().map(|r| r[i].as_time()).collect::<Vec<_>>())),
                ),
                ColumnType::Count => (
                    Field::new(&c.name, DataType::Int64, true),
                    Arc::new(Int64Array::from(
                        rows.iter()
                            .map(|r| match &r[i] {
                                V::C(n) => Some(*n),
                                v => v.as_f64().map(|x| x as i64),
                            })
                            .collect::<Vec<_>>(),
                    )),
                ),
                _ => (
                    Field::new(&c.name, DataType::Float64, true),
                    Arc::new(Float64Array::from(rows.iter().map(|r| r[i].as_f64()).collect::<Vec<_>>())),
                ),
            };
            fields.push(field);
            arrays.push(array);
        }
        if let (Some(avail), Some(rc)) = (m.available_at.get(name), schema.columns.iter().find(|c| c.role == Role::ReceiptTime)) {
            let key = cols.iter().position(|c| c.role == Role::EventTime).unwrap_or(0);
            fields.push(Field::new(&rc.name, ns.clone(), false));
            arrays.push(Arc::new(TimestampNanosecondArray::from(
                rows.iter()
                    .enumerate()
                    .map(|(i, r)| {
                        let t = r[key].as_time().unwrap_or(i64::MIN);
                        avail.get(i).copied().filter(|a| *a != i64::MIN).unwrap_or(t).max(t)
                    })
                    .collect::<Vec<_>>(),
            )));
        }
        let batch = RecordBatch::try_new(Arc::new(Schema::new(fields)), arrays)?;
        relations.push((schema.name.clone(), batch));
    }
    Ok((m.securities.clone(), m.terms.clone(), relations))
}

/// What a market's versions state of it (D-2.1): the calendar of its bars,
/// `reviewed = complete` unless it is unreviewed, the bundle version it was
/// loaded from; `extra` (a reviewer's statements) last.
pub fn attributes(m: &Market, base: &str, extra: &BTreeMap<String, String>) -> BTreeMap<String, String> {
    let mut a = BTreeMap::new();
    a.insert("calendar".to_string(), base.trim_start_matches('@').to_string());
    if !m.unreviewed {
        a.insert("reviewed".to_string(), "complete".to_string());
    }
    if let Some(v) = &m.version {
        a.insert("version".to_string(), v.clone());
    }
    a.extend(extra.iter().map(|(k, v)| (k.clone(), v.clone())));
    a
}

/// Ingests a market of `class` into a store, its versions carrying
/// `attributes`. A market holds its class's relations, and the series
/// class's: one it has no rows of is provided empty (a market without
/// delistings), as a relation of a dataset always was.
pub fn ingest(store: &mut Store, m: &Market, class: &str, attributes: &BTreeMap<String, String>) -> Result<Ingested, DataError> {
    let schemas = store.registry.schemas(None);
    let (securities, terms, relations) = tables_of(m, &|name| schema_in(&schemas, class, name))?;
    let out = ingest_keyed(store, SOURCE, &securities, &terms, &relations, SOURCE, attributes)?;
    for s in schemas.iter().filter(|s| s.class == class || s.class == "series") {
        ingest_empty(store, &s.name, SOURCE, attributes)?;
    }
    Ok(out)
}
