//! The data layer's own markets are the kernel's, bit for bit (A-D2: the
//! data layer's binary links no kernel, so `abt-data ingest --adapter
//! synthetic|bundle_v2` makes its markets with `abt_data::adapters`; a
//! store it fills must be the store a run in one process reads, which the
//! front fills from the kernel's `Dataset` through `keyed_tables`).

use std::collections::BTreeMap;

use abt_data::adapter::{securities_of, terms_of};
use abt_data::adapters::market::{schema_in, tables_of, KeyedTables, Market};
use abt_data::adapters::{bundle, synthetic};
use abt_data::Store;
use abt_kernel::bundle::write_bundle;
use abt_kernel::check::{class_program, Program};
use abt_kernel::data::{synthetic_daily, synthetic_futures, synthetic_minute};
use abt_kernel::kernel::bind::keyed_tables;
use abt_kernel::kernel::Dataset;
use abt_wire::generated::abt::wire::RelationSchema;
use arrow::array::RecordBatch;

mod common {
    pub use abt_tests::*;
}

/// The registry's schemas (what a store ingests under).
fn schemas() -> Vec<RelationSchema> {
    Store::temporary("synthetic-schemas").unwrap().registry.schemas(None)
}

fn kernel_tables(ds: &Dataset, class: &str, schemas: &[RelationSchema]) -> KeyedTables {
    let (sec, terms, rels) = keyed_tables(ds, &|name| abt_kernel::kernel::bind::schema_in(schemas, class, name)).unwrap();
    (securities_of(&sec).unwrap(), terms_of(&terms).unwrap(), rels)
}

fn data_tables(m: &Market, class: &str, schemas: &[RelationSchema]) -> KeyedTables {
    tables_of(m, &|name| schema_in(schemas, class, name)).unwrap()
}

fn assert_same(
    ours: &(Vec<abt_data::adapter::Security>, Vec<abt_data::adapter::Terms>, Vec<(String, RecordBatch)>),
    theirs: &(Vec<abt_data::adapter::Security>, Vec<abt_data::adapter::Terms>, Vec<(String, RecordBatch)>),
    what: &str,
) {
    assert_eq!(ours.0, theirs.0, "{}: the security tables differ", what);
    assert_eq!(ours.1, theirs.1, "{}: the contract terms differ", what);
    let names: Vec<&str> = ours.2.iter().map(|(n, _)| n.as_str()).collect();
    let their_names: Vec<&str> = theirs.2.iter().map(|(n, _)| n.as_str()).collect();
    assert_eq!(names, their_names, "{}: the relations differ", what);
    for ((name, a), (_, b)) in ours.2.iter().zip(&theirs.2) {
        assert_eq!(a.schema(), b.schema(), "{}: the schema of `{}` differs", what, name);
        assert_eq!(a.num_rows(), b.num_rows(), "{}: `{}` has {} rows against {}", what, name, a.num_rows(), b.num_rows());
        assert_eq!(a, b, "{}: the rows of `{}` differ", what, name);
    }
}

const SYMS: [&str; 5] = ["AAA", "XYZ", "BBB", "SPY", "DDD"];

#[test]
fn the_daily_market_is_the_kernels_bit_for_bit() {
    let schemas = schemas();
    for (days, seed) in [(500usize, 7u64), (60, 5), (320, 11), (12, 3)] {
        let ds = synthetic_daily(&SYMS, (2022, 1, 3), days, seed);
        let m = synthetic::daily(&SYMS, (2022, 1, 3), days, seed);
        assert_same(
            &data_tables(&m, "equity", &schemas),
            &kernel_tables(&ds, "equity", &schemas),
            &format!("daily, {} days, seed {}", days, seed),
        );
    }
    // Two names and one: the rename and the delisting need a third.
    for syms in [&SYMS[..2], &SYMS[..1]] {
        let ds = synthetic_daily(syms, (2023, 1, 2), 120, 3);
        let m = synthetic::daily(syms, (2023, 1, 2), 120, 3);
        assert_same(&data_tables(&m, "equity", &schemas), &kernel_tables(&ds, "equity", &schemas), &format!("daily, {} names", syms.len()));
    }
}

#[test]
fn the_minute_and_futures_markets_are_the_kernels_bit_for_bit() {
    let schemas = schemas();
    let ds = synthetic_minute(&SYMS, (2024, 1, 2), 3, 60, 7);
    let m = synthetic::minute(&SYMS, (2024, 1, 2), 3, 60, 7);
    assert_same(&data_tables(&m, "equity", &schemas), &kernel_tables(&ds, "equity", &schemas), "minute");
    let roots = ["ES", "NQ", "ZN", "6J"];
    let ds = synthetic_futures(&roots, (2022, 1, 3), 5, 60, 11);
    let m = synthetic::futures(&roots, (2022, 1, 3), 5, 60, 11);
    assert_same(&data_tables(&m, "future", &schemas), &kernel_tables(&ds, "future", &schemas), "futures");
    // `market` picks by class and base as `synthetic_for_program` does.
    let m2 = synthetic::market("future", true, &roots, (2022, 1, 3), 5, 11);
    assert_eq!(m2.facts.len(), m.facts.len());
}

/// The equity class as a program, for the kernel's bundle writer.
fn equity_class() -> Program {
    class_program(&common::base_workspace(), "equity").unwrap_or_else(|d| panic!("the equity class should check as a program:\n{}", common::text(&d)))
}

#[test]
fn a_bundle_reads_the_same_through_the_data_layer_as_through_the_kernel() {
    let schemas = schemas();
    let cls = equity_class();
    let ds = synthetic_daily(&SYMS, (2022, 1, 3), 200, 7);
    let dir = std::env::temp_dir().join(format!("abt-bundle-dl-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    write_bundle(&cls, &ds, &dir, "equity", "2026.10").unwrap();
    let (loaded, _) = abt_kernel::bundle::load_bundle(&cls, &dir).unwrap();
    let (market, manifest) = bundle::load(&dir, "equity", &schemas).unwrap();
    assert_eq!(manifest.version, "2026.10");
    assert!(market.unreviewed && loaded.unreviewed, "an untested bundle is unreviewed either way");
    assert_eq!(market.version.as_deref(), loaded.version.as_deref());
    assert_same(&data_tables(&market, "equity", &schemas), &kernel_tables(&loaded, "equity", &schemas), "the bundle");
    // Ingested, the two give the same store: every version's rows equal.
    let mut a = Store::temporary("bundle-dl").unwrap();
    let mut b = Store::temporary("bundle-k").unwrap();
    let attrs = abt_data::adapters::market::attributes(&market, "1d", &BTreeMap::new());
    abt_data::adapters::market::ingest(&mut a, &market, "equity", &attrs).unwrap();
    abt::ingest_dataset(&mut b, &loaded, "equity", &abt::dataset_attributes(&loaded, "@1d", &BTreeMap::new())).unwrap();
    let ra: Vec<(String, String, String)> = a.registry.currents();
    let rb: Vec<(String, String, String)> = b.registry.currents();
    assert_eq!(ra.len(), rb.len());
    for ((r, s, va), (_, _, vb)) in ra.iter().zip(&rb) {
        let xa = a.read_all(va).unwrap();
        let xb = b.read_all(vb).unwrap();
        assert_eq!(xa.num_rows(), xb.num_rows(), "{} ({}) differs in rows", r, s);
        assert_eq!(xa, xb, "{} ({}) differs", r, s);
    }
    let _ = std::fs::remove_dir_all(&dir);
}
