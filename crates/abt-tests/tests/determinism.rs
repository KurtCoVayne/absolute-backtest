//! G3 (A-D11, Proposition P-5.1): a run's result is a function of its
//! program, binding and configuration only. The same run in process and
//! over a Unix socket, the data layer's aggregate pass on one thread and on
//! eight, and several kernels reading one data service at once all give the
//! same bits.

use abt_tests as common;

use std::collections::BTreeMap;
use std::sync::Arc;

use abt_data::agg::{aggregate, answer, set_threads};
use abt_data::query::ResolvedView;
use abt_data::Store;
use abt_kernel::data::synthetic_daily;
use abt_kernel::kernel::{run_on, ExecConfig, Replay, RunResult};
use abt_wire::generated::abt::wire::{AggregateRequest, Grouping, Numerics, WindowKind};
use arrow::array::{Float64Array, RecordBatch, StringArray, TimestampNanosecondArray};
use arrow::datatypes::{DataType, Field, Schema, TimeUnit};

const SYMS: [&str; 5] = ["AAA", "XYZ", "BBB", "SPY", "DDD"];

/// What a run decided, filled and held, exactly.
fn digest(r: &RunResult) -> Vec<String> {
    let mut out = vec![format!("bars {:?}", r.bars)];
    for d in &r.decisions {
        out.push(format!("decision {} {} {}", d.t, r.describe_decision(&d.decision), d.rule));
    }
    for f in &r.fills {
        out.push(format!(
            "fill {} {} {:x} {:x} {:x}",
            f.t,
            r.symbols[f.equity as usize],
            f.quantity.to_bits(),
            f.price.to_bits(),
            f.commission.to_bits()
        ));
    }
    for (t, e) in &r.equity_curve {
        out.push(format!("equity {} {:x}", t, e.to_bits()));
    }
    out.push(format!("final {:x} {:?}", r.final_cash.to_bits(), r.final_positions));
    out
}

#[test]
fn in_process_socket_and_concurrent_kernels_agree() {
    let ds = synthetic_daily(&SYMS, (2022, 1, 3), 300, 13);
    let names = ["momentum_top_n", "sma_crossover", "mean_reversion_zscore", "low_vol_portfolio"];
    let programs: Vec<_> = names.iter().map(|n| (n.to_string(), common::corpus_program(n))).collect();
    // One service, each program in turn.
    let client = common::client(&programs[0].1, &ds);
    let sequential: BTreeMap<String, Vec<String>> = programs
        .iter()
        .map(|(n, p)| (n.clone(), digest(&run_on(p, None, &client, ExecConfig::from_program(p), &Replay::default()).unwrap())))
        .collect();
    // Over a socket.
    for (n, p) in &programs {
        let r = common::run_socket(p, &ds, ExecConfig::from_program(p)).unwrap();
        assert_eq!(sequential[n], digest(&r), "{}: in process and over a socket", n);
    }
    // Four kernels at once on one service: their requests interleave on the
    // connection, their answers do not.
    let concurrent: BTreeMap<String, Vec<String>> = std::thread::scope(|s| {
        let handles: Vec<_> = programs
            .iter()
            .map(|(n, p)| {
                let client = &client;
                s.spawn(move || (n.clone(), digest(&run_on(p, None, client, ExecConfig::from_program(p), &Replay::default()).unwrap())))
            })
            .collect();
        handles.into_iter().map(|h| h.join().unwrap()).collect()
    });
    assert_eq!(sequential, concurrent);
}

fn close_rows(n_entities: u64, days: i64) -> RecordBatch {
    let day = 86_400 * 1_000_000_000i64;
    let t0 = 19_000 * day;
    let (mut a, mut t, mut p) = (Vec::new(), Vec::new(), Vec::new());
    let mut x: u64 = 0x9e37_79b9_7f4a_7c15;
    for d in 0..days {
        for e in 0..n_entities {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            a.push(format!("E{}", e));
            t.push(t0 + d * day);
            p.push(50.0 + (x % 10_000) as f64 / 101.0);
        }
    }
    let schema = Arc::new(Schema::new(vec![
        Field::new("A", DataType::Utf8, false),
        Field::new("T", DataType::Timestamp(TimeUnit::Nanosecond, None), false),
        Field::new("P", DataType::Float64, false),
    ]));
    RecordBatch::try_new(
        schema,
        vec![Arc::new(StringArray::from(a)), Arc::new(TimestampNanosecondArray::from(t)), Arc::new(Float64Array::from(p))],
    )
    .unwrap()
}

#[test]
fn the_aggregate_pass_on_one_thread_and_on_eight_gives_the_same_bits() {
    let mut store = Store::temporary("determinism").unwrap();
    abt_data::adapter::ingest_keyed(&mut store, "test", &[], &[], &[("equity.close".to_string(), close_rows(200, 120))], "test", &Default::default()).unwrap();
    let mut answers = Vec::new();
    for (threads, numerics) in [(1, Numerics::TwoPass), (8, Numerics::TwoPass), (1, Numerics::Running), (8, Numerics::Running)] {
        set_threads(threads);
        let request = AggregateRequest {
            relation: "equity.close".into(),
            column: "P".into(),
            second_column: None,
            function: "std".into(),
            level: None,
            window: WindowKind::Rows,
            n: 20,
            min: None,
            span: None,
            bucket: None,
            group: Grouping::PerEntity,
            group_column: None,
            receipt_filter: None,
            numerics,
            calendar: None,
            t0: None,
            t1: None,
            // A distinct output name per thread count: no cache hit.
            output: Some(format!("std_{}", threads)),
        };
        let (version, _) = aggregate(&mut store, &ResolvedView::default(), &request).unwrap();
        let b = answer(&store, &version).unwrap();
        let values: Vec<u64> = b.column(2).as_any().downcast_ref::<Float64Array>().unwrap().iter().map(|v| v.map(f64::to_bits).unwrap_or(0)).collect();
        answers.push((numerics, values));
    }
    set_threads(0);
    assert_eq!(answers[0].1, answers[1].1, "two_pass, 1 against 8 threads");
    assert_eq!(answers[2].1, answers[3].1, "running, 1 against 8 threads");
    assert_eq!(answers[0].1.len(), 200 * (120 - 19));
}
