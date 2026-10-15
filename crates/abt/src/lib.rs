//! The front (docs/spec/00-architecture.md, Definition 2.2): the one crate
//! that links the three layers, so the one place they are wired together in
//! a process. `install` registers with the kernel the data service it reaches
//! a dataset held in memory through (ERRATA E-36): the dataset is ingested
//! into a temporary store by the data layer's adapter of keyed rows, and the
//! kernel talks to the service over an in-process channel, the same frames as
//! a socket carries.

use std::sync::Arc;

pub mod data_cli;

use abt_data::adapter::{ingest_keyed, securities_of, terms_of};
use abt_data::service::{inproc, DataService};
use abt_data::Store;
use abt_kernel::check::Program;
use abt_kernel::kernel::bind::{keyed_tables, schema_in};
use abt_kernel::kernel::Dataset;
use abt_wire::transport::Connection;

/// The source a dataset held in memory is ingested under.
pub const SOURCE: &str = "dataset";

/// Ingests a dataset of `class` into a store.
pub fn ingest_dataset(store: &mut Store, ds: &Dataset, class: &str) -> Result<(), String> {
    let schemas = store.registry.schemas(None);
    let (sec, terms, relations) = keyed_tables(ds, &|name| schema_in(&schemas, class, name))?;
    let securities = securities_of(&sec).map_err(|e| e.to_string())?;
    let terms = terms_of(&terms).map_err(|e| e.to_string())?;
    ingest_keyed(store, SOURCE, &securities, &terms, &relations, SOURCE).map_err(|e| e.to_string())?;
    Ok(())
}

/// A data service holding a dataset, over a temporary store removed when
/// the service ends.
pub fn serve_dataset(ds: &Dataset, class: &str) -> Result<Arc<DataService>, String> {
    let mut store = Store::temporary("run").map_err(|e| e.to_string())?;
    ingest_dataset(&mut store, ds, class)?;
    Ok(DataService::new(store))
}

/// The provider the kernel reaches a dataset through: a service in this
/// process holding it.
pub fn provider(ds: &Dataset, prog: &Program) -> Result<Connection, String> {
    let svc = serve_dataset(ds, &prog.class)?;
    Ok(inproc(&svc))
}

/// Registers the provider with the kernel (idempotent).
pub fn install() {
    abt_kernel::kernel::install_provider(Box::new(provider));
}
