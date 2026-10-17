//! Vendor adapters (D-7.7): each maps a source's data to the relations of
//! its class (D-2.3) and the registry's ids (D-2.4), and declares what it
//! offers. The resolver buys through them (D-7.5); what an adapter
//! returns is ingested by the keyed ingest under the source's name.

use std::collections::BTreeMap;

use arrow::array::RecordBatch;

use crate::adapter::{Security, Terms};
use crate::error::DataError;
use crate::resolver::catalog::Offer;

pub mod mock;

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
}
