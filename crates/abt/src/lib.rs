//! The front (docs/spec/00-architecture.md, Definition 2.2): the one crate
//! that links the three layers, so the one place they are wired together in
//! a process. `install` registers with the kernel the data service it reaches
//! a dataset held in memory through (ERRATA E-36): the dataset is ingested
//! into a temporary store by the data layer's adapter of keyed rows, and the
//! kernel talks to the service over an in-process channel, the same frames as
//! a socket carries.

use std::collections::BTreeMap;
use std::sync::Arc;

pub mod data_cli;
pub mod query_cli;

use abt_data::adapter::{ingest_empty, ingest_keyed, securities_of, terms_of};
use abt_data::service::{inproc, DataService};
use abt_data::Store;
use abt_kernel::check::Program;
use abt_kernel::kernel::bind::{keyed_tables, schema_in};
use abt_kernel::kernel::Dataset;
use abt_wire::transport::Connection;

/// The source a dataset held in memory is ingested under.
pub const SOURCE: &str = "dataset";

/// What a dataset's versions state of it (D-2.1): the calendar of its bars,
/// `reviewed = complete` unless it is unreviewed, the bundle version it was
/// loaded from; `extra` (a reviewer's statements) last.
pub fn dataset_attributes(ds: &Dataset, base: &str, extra: &BTreeMap<String, String>) -> BTreeMap<String, String> {
    let mut a = BTreeMap::new();
    a.insert("calendar".to_string(), base.trim_start_matches('@').to_string());
    if !ds.unreviewed {
        a.insert("reviewed".to_string(), "complete".to_string());
    }
    if let Some(v) = &ds.version {
        a.insert("version".to_string(), v.clone());
    }
    a.extend(extra.iter().map(|(k, v)| (k.clone(), v.clone())));
    a
}

/// Ingests a dataset of `class` into a store, its versions carrying
/// `attributes`. A dataset holds its class's relations, and the series
/// class's: one it has no tuples of is provided empty (a market without
/// delistings), as a relation of a dataset always was.
pub fn ingest_dataset(store: &mut Store, ds: &Dataset, class: &str, attributes: &BTreeMap<String, String>) -> Result<(), String> {
    let schemas = store.registry.schemas(None);
    let (sec, terms, relations) = keyed_tables(ds, &|name| schema_in(&schemas, class, name))?;
    let securities = securities_of(&sec).map_err(|e| e.to_string())?;
    let terms = terms_of(&terms).map_err(|e| e.to_string())?;
    ingest_keyed(store, SOURCE, &securities, &terms, &relations, SOURCE, attributes).map_err(|e| e.to_string())?;
    for s in schemas.iter().filter(|s| s.class == class || s.class == "series") {
        ingest_empty(store, &s.name, SOURCE, attributes).map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// A data service holding a dataset of a program's class at its base, over
/// a temporary store removed when the service ends.
pub fn serve_dataset(ds: &Dataset, prog: &Program) -> Result<Arc<DataService>, String> {
    let mut store = Store::temporary("run").map_err(|e| e.to_string())?;
    let attributes = dataset_attributes(ds, &prog.base.to_string(), &BTreeMap::new());
    ingest_dataset(&mut store, ds, &prog.class, &attributes)?;
    Ok(DataService::new(store))
}

/// The provider the kernel reaches a dataset through: a service in this
/// process holding it.
pub fn provider(ds: &Dataset, prog: &Program) -> Result<Connection, String> {
    let svc = serve_dataset(ds, prog)?;
    Ok(inproc(&svc))
}

/// Registers the provider with the kernel (idempotent).
pub fn install() {
    abt_kernel::kernel::install_provider(Box::new(provider));
}
