//! The metrics of the query layer (Q-3.1) are computed from the tables the
//! kernel wrote to the store, and are the metrics the kernel's study
//! computed from the run in memory, to the bit: on every corpus program and
//! both books, the return metrics of the metric curve, the trading metrics,
//! the capacity, the report block's period metrics and the sub-period
//! Sharpe ratios.

#![allow(clippy::result_large_err)]

use abt_kernel::kernel::{DataClient, ExecConfig, Replay, RunResult};
use abt_kernel::study::metrics as old;
use abt_tests as common;
use abt_tests::golden_inputs;

fn json<T: serde::Serialize>(x: &T) -> String {
    serde_json::to_string(x).unwrap()
}

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
    for path in common::golden_programs() {
        let (p, r, data) = run_writing(&path);
        let name = p.strategy.clone();
        let m = abt_query::cli::run_metrics(&data, &r.run_id).unwrap_or_else(|e| panic!("{}: {}", name, e));
        let ppy = p.report.periods_per_year;
        assert_eq!(m.periods_per_year.to_bits(), ppy.to_bits(), "{}", name);
        assert_eq!(json(&m.returns), json(&old::return_metrics(&r.metric_curve(), ppy)), "{}: returns", name);
        assert_eq!(json(&m.trading), json(&old::trading_metrics(&r, ppy)), "{}: trading", name);
        assert_eq!(json(&m.capacity), json(&old::capacity(&r, ppy, 0.05)), "{}: capacity", name);
        let (by_day, days) = (r.periods.by_day, r.periods.days.clone().map(|d| d.into_iter().collect::<std::collections::BTreeSet<i64>>()));
        assert_eq!(json(&m.convention), json(&old::convention_metrics_on(&r, ppy, by_day, days.as_ref())), "{}: periods", name);
        assert_eq!(
            json(&m.subperiods),
            json(&abt_kernel::study::validate::subperiod_sharpes(&r.metric_curve(), ppy, 20)),
            "{}: sub-periods",
            name
        );
        assert!(
            m.versions.keys().any(|k| k.ends_with(".book")) && m.versions.keys().any(|k| k.ends_with(".report")),
            "{}: {:?}",
            name,
            m.versions
        );
    }
}
