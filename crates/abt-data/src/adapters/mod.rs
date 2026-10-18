//! Vendor adapters (D-7.7): each maps a source's data to the relations of
//! its class (D-2.3) and the registry's ids (D-2.4), and declares what it
//! offers. The resolver buys through them (D-7.5); what an adapter
//! returns is ingested by the keyed ingest under the source's name.

use std::borrow::Cow;
use std::collections::BTreeMap;
use std::sync::Arc;

use abt_wire::generated::abt::wire::ColumnType;
use arrow::array::{Array, ArrayRef, AsArray, RecordBatch, UInt64Array};
use arrow::datatypes::{DataType, Field, Schema, TimeUnit};

use crate::adapter::{ingest_keyed_with, ticker_spans, Ingested, Security, Terms};
use crate::error::DataError;
use crate::resolver::catalog::Offer;
use crate::store::Store;

pub mod bundle;
pub mod databento;
pub mod databento_live;
pub mod dbn;
pub mod http;
pub mod market;
pub mod mock;
pub mod norgate;
pub mod orats;
pub mod synthetic;

/// A purchase: one relation of a need over a span, as the plan priced it.
#[derive(Clone, Debug, PartialEq)]
pub struct FetchRequest {
    pub source: String,
    pub class: String,
    /// The store's names of the relations (`equity.close`).
    pub relations: Vec<String>,
    /// The bars asked for (the offer's calendar).
    pub calendar: String,
    /// The instants of the span, both included.
    pub from: i64,
    pub to: i64,
    /// The offer's scope (`all`, `list(L)`, …).
    pub scope: String,
}

/// How a fetch's keys name instruments (D-2.4's assignment rules).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Keys {
    /// The vendor's own keys: one the registry has not seen is a new
    /// instrument (Norgate's `asset_id`, a Databento future's
    /// `instrument_id`).
    #[default]
    Vendor,
    /// Symbols, each the instrument whose ticker span carries it at the
    /// row's `t`, the whole of the span's last day included (a Databento
    /// equity's `raw_symbol` joined to a Norgate asset by symbol at date);
    /// a symbol no span carries rejects the row (A-D24).
    Ticker,
}

/// A fetch's rows, keyed by the vendor's keys, as the keyed ingest takes
/// them.
#[derive(Clone, Debug, Default)]
pub struct Fetched {
    pub securities: Vec<Security>,
    pub terms: Vec<Terms>,
    /// Each relation's rows, by the store's name.
    pub relations: Vec<(String, RecordBatch)>,
    /// What the adapter states of the rows, beside its offer's attributes.
    pub attributes: BTreeMap<String, String>,
    /// What it states of one relation's rows only (`dividend_dates =
    /// ex_only` of `equity.dividend`), beside `attributes`.
    pub relation_attributes: BTreeMap<String, BTreeMap<String, String>>,
    /// What the rows' keys are.
    pub keys: Keys,
    /// Entity columns joined to the registry's tickers whatever `keys` says,
    /// as `(relation, column)`: an option's underlying, named by its symbol
    /// beside contracts named by the vendor's keys (D-2.4).
    pub ticker_columns: Vec<(String, String)>,
}

const DAY: i64 = 86_400_000_000_000;

impl Fetched {
    /// The attributes of one relation's version: the fetch's, then the
    /// relation's own.
    pub fn attributes_of(&self, relation: &str) -> BTreeMap<String, String> {
        let mut a = self.attributes.clone();
        a.extend(self.relation_attributes.get(relation).cloned().unwrap_or_default());
        a
    }

    /// Ingests the rows under `source` with the vendor's keys, each
    /// relation's version carrying `extra`, then the fetch's attributes and
    /// the relation's own.
    pub fn ingest(&self, store: &mut Store, vendor: &str, source: &str, extra: &BTreeMap<String, String>) -> Result<Ingested, DataError> {
        let mut attributes = extra.clone();
        attributes.extend(self.attributes.clone());
        let relations = match self.keys {
            Keys::Vendor if self.ticker_columns.is_empty() => Cow::Borrowed(&self.relations),
            _ => Cow::Owned(self.by_ticker(store)?),
        };
        ingest_keyed_with(store, vendor, &self.securities, &self.terms, &relations, source, &attributes, &self.relation_attributes)
    }

    /// The relations with their symbols replaced by the ids whose ticker
    /// spans carry them at each row's `t`: every entity column under
    /// `Keys::Ticker`, the `ticker_columns` otherwise.
    fn by_ticker(&self, store: &Store) -> Result<Vec<(String, RecordBatch)>, DataError> {
        let spans = ticker_spans(store)?;
        let mut out = Vec::with_capacity(self.relations.len());
        for (relation, b) in &self.relations {
            let schema = store.schema(relation)?;
            let t_name = &schema.columns[crate::rows::layout(&schema).t].name;
            let times = b
                .column_by_name(t_name)
                .map(|c| arrow::compute::cast(c, &DataType::Timestamp(TimeUnit::Nanosecond, None)))
                .transpose()?
                .ok_or_else(|| DataError::Schema(format!("`{}` has no column `{}`", relation, t_name)))?;
            let times = times.as_primitive::<arrow::datatypes::TimestampNanosecondType>();
            let mut cols: Vec<ArrayRef> = Vec::with_capacity(b.num_columns());
            let mut fields: Vec<Field> = Vec::with_capacity(b.num_columns());
            for (j, f) in b.schema().fields().iter().enumerate() {
                let entity = schema.columns.iter().any(|c| c.name == *f.name() && c.ty == ColumnType::Entity)
                    && (self.keys == Keys::Ticker || self.ticker_columns.iter().any(|(r, c)| r == relation && c == f.name()));
                if !entity || !matches!(f.data_type(), DataType::Utf8 | DataType::LargeUtf8) {
                    cols.push(b.column(j).clone());
                    fields.push(f.as_ref().clone());
                    continue;
                }
                let text = arrow::compute::cast(b.column(j), &DataType::Utf8)?;
                let text = text.as_string::<i32>();
                let mut ids = Vec::with_capacity(text.len());
                for i in 0..text.len() {
                    if text.is_null(i) {
                        ids.push(None);
                        continue;
                    }
                    let (symbol, t) = (text.value(i), times.value(i));
                    let id = spans
                        .get(symbol)
                        .and_then(|v| v.iter().rev().find(|(from, to, _)| *from <= t && to.is_none_or(|to| t < to.saturating_add(DAY))))
                        .map(|s| s.2)
                        .ok_or_else(|| {
                            DataError::Identity(format!(
                                "{} row {}: the symbol `{}` is carried by no instrument at {} (a symbol is joined to the registry's tickers, D-2.4)",
                                relation,
                                i,
                                symbol,
                                abt_wire::scalar::Time(t).iso()
                            ))
                        })?;
                    ids.push(Some(id));
                }
                cols.push(Arc::new(UInt64Array::from(ids)));
                fields.push(Field::new(f.name(), DataType::UInt64, f.is_nullable()));
            }
            out.push((relation.clone(), RecordBatch::try_new(Arc::new(Schema::new(fields)), cols)?));
        }
        Ok(out)
    }
}

/// A vendor adapter (D-7.7).
pub trait Adapter: Send + Sync {
    /// The vendor whose keys the rows carry (the registry's `vendor_id`).
    fn vendor(&self) -> &str;
    /// What the source can provide.
    fn offers(&self) -> Vec<Offer>;
    /// The rows of a purchase. A row that fails its schema or the identity
    /// rule fails the fetch, naming the row: an adapter never returns a
    /// partial batch.
    fn fetch(&self, req: &FetchRequest) -> Result<Fetched, DataError>;
    /// A live subscription to what a request names, from now on: a source
    /// without a live feed refuses it.
    fn subscribe(&self, req: &FetchRequest) -> Result<Box<dyn Feed>, DataError> {
        let _ = req;
        Err(DataError::Unresolved(format!("the vendor `{}` has no live feed", self.vendor())))
    }
}

/// A live subscription (D-7.7): the rows of what arrives, each row received
/// when it was accepted.
pub trait Feed: Send {
    /// The rows of the next records to arrive, waiting until one does;
    /// `None` when the session ends.
    fn next(&mut self) -> Result<Option<Fetched>, DataError>;
}

/// The adapters a catalog's sources are fetched by (D-7.7), each
/// configured by its source's settings and keyed by the source's name, and
/// what could not be made of the others (an adapter abt does not have, a
/// setting it refuses): such a source is listed and planned, and buying
/// from it is refused naming its adapter.
pub fn of_catalog(catalog: &crate::resolver::Catalog) -> (BTreeMap<String, Arc<dyn Adapter>>, Vec<String>) {
    let mut out: BTreeMap<String, Arc<dyn Adapter>> = BTreeMap::new();
    let mut missing: Vec<String> = Vec::new();
    for s in &catalog.sources {
        let made: Result<Arc<dyn Adapter>, String> = match s.adapter.as_str() {
            "norgate" => norgate::Norgate::from_settings(&s.settings).map(|n| Arc::new(n) as _).map_err(|e| e.to_string()),
            "databento" => databento::Databento::from_settings(&s.settings).map(|d| Arc::new(d) as _).map_err(|e| e.to_string()),
            "databento_live" => databento_live::DatabentoLive::from_settings(&s.settings).map(|d| Arc::new(d) as _).map_err(|e| e.to_string()),
            "orats" => orats::Orats::from_settings(&s.settings).map(|o| Arc::new(o) as _).map_err(|e| e.to_string()),
            other => Err(format!("`{}` (no such adapter)", other)),
        };
        match made {
            Ok(a) => {
                out.insert(s.name.clone(), a);
            }
            Err(e) => missing.push(format!("{} for the source `{}`", e, s.name)),
        }
    }
    (out, missing)
}
