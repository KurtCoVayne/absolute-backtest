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
use abt_kernel::kernel::{Connector, Dataset};

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
/// process holding it, reached by a new in-process connection each time
/// one is asked for.
pub fn provider(ds: &Dataset, prog: &Program) -> Result<Connector, String> {
    let svc = serve_dataset(ds, prog)?;
    Ok(Arc::new(move || Ok(inproc(&svc))))
}

/// Registers with the kernel the provider of in-process data services, and
/// the query layer's metrics as what `abt run` prints a run's returns with
/// (E-66); idempotent.
pub fn install() {
    abt_kernel::kernel::install_provider(Box::new(provider));
    abt_kernel::cli::install_reporter(Box::new(|conn, run, calendar| abt_query::cli::run_lines(conn, run, calendar).map(|r| (r.lines, r.returns))));
}

/// The names of the market the briefs harness runs attempts on.
pub const BRIEFS_SYMBOLS: [&str; 5] = ["AAA", "BBB", "CCC", "DDD", "SPY"];

/// The market the briefs harness runs attempts on (E-65): the synthetic
/// daily equity market of its five names over 320 business days from
/// 2022-01-03 (seed 11), in a temporary store.
pub fn briefs_market() -> Result<Arc<DataService>, String> {
    let ds = abt_kernel::data::synthetic_daily(&BRIEFS_SYMBOLS, (2022, 1, 3), 320, 11);
    let mut store = Store::temporary("briefs").map_err(|e| e.to_string())?;
    ingest_dataset(&mut store, &ds, "equity", &dataset_attributes(&ds, "@1d", &BTreeMap::new()))?;
    Ok(DataService::new(store))
}

/// The adapters a catalog's sources are fetched by (D-7.7), by name, and
/// the adapters it names that abt does not have: such a source is listed
/// and planned, and buying from it is refused naming its adapter.
pub fn adapters_of(catalog: &abt_data::resolver::Catalog) -> (BTreeMap<String, Arc<dyn abt_data::adapters::Adapter>>, Vec<String>) {
    let out: BTreeMap<String, Arc<dyn abt_data::adapters::Adapter>> = BTreeMap::new();
    let mut missing: Vec<String> = catalog.sources.iter().map(|s| s.adapter.clone()).filter(|a| !out.contains_key(a)).collect();
    missing.sort();
    missing.dedup();
    (out, missing)
}
