//! The metrics of the query layer (Q-3.1) are computed from the tables the
//! kernel wrote to the store, and are the metrics the kernel's study
//! computed from the run in memory, to the bit: on every corpus program and
//! both books, the return metrics of the metric curve, the trading metrics,
//! the capacity, the report block's period metrics and the sub-period
//! Sharpe ratios. The study's values are frozen in
//! `golden/study/metrics.json`, written while it still computed them and
//! this test held the two equal (ERRATA E-66).

#![allow(clippy::result_large_err)]

use abt_kernel::kernel::{DataClient, ExecConfig, Replay, RunResult};
use abt_tests as common;
use abt_tests::golden_inputs;

/// A run over a data service holding the program's market, writing its
/// outputs there; the client the query layer reads the store through.
fn run_writing(path: &std::path::Path) -> (abt_kernel::check::Program, RunResult, abt_query::data::Data) {
    let (p, ds) = golden_inputs(path).unwrap();
    let svc = abt::serve_dataset(&ds, &p).unwrap();
    let kernel_client = DataClient::connect(abt_data::service::inproc(&svc)).unwrap();
    let opts = Replay {
        write_outputs: true,
        ..Default::default()
    };
    let r = abt_kernel::kernel::run_on(&p, None, &kernel_client, ExecConfig::from_program(&p), &opts).unwrap();
    let data = abt_query::data::Data::connect(abt_data::service::inproc(&svc)).unwrap();
    (p, r, data)
}

#[test]
fn the_metrics_from_the_store_are_the_metrics_of_the_run() {
    let mut frozen = serde_json::Map::new();
    for path in common::golden_programs() {
        let (p, r, data) = run_writing(&path);
        let name = p.strategy.clone();
        let m = abt_query::cli::run_metrics(&data, &r.run_id).unwrap_or_else(|e| panic!("{}: {}", name, e));
        frozen.insert(
            name.clone(),
            serde_json::json!({
                "periods_per_year": m.periods_per_year,
                "returns": m.returns,
                "trading": m.trading,
                "capacity": m.capacity,
                "convention": m.convention,
                "subperiods": m.subperiods,
            }),
        );
        assert_eq!(m.periods_per_year.to_bits(), p.report.periods_per_year.to_bits(), "{}", name);
        assert_eq!(m.trading.fills, r.fills.len(), "{}: the fills the run made", name);
        assert!(
            m.versions.keys().any(|k| k.ends_with(".book")) && m.versions.keys().any(|k| k.ends_with(".report")),
            "{}: {:?}",
            name,
            m.versions
        );
    }
    common::golden_json("study/metrics", &serde_json::Value::Object(frozen));
}
