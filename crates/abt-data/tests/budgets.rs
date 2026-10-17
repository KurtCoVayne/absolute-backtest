//! The data layer's budgets (D-9.1), measured. Ignored by default: run with
//! `cargo test -p abt-data --release --test budgets -- --ignored --nocapture`
//! and record what it prints in docs/assessment.md. Nothing is asserted: the
//! budgets are the owner's machine's (G10); here they are measured.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Instant;

use abt_data::agg::aggregate;
use abt_data::query::{execute, ResolvedView};
use abt_data::Store;
use abt_wire::generated::abt::wire::{AggregateRequest, Constant, ConstantKind, Grouping, Identity, Latest, Numerics, Point, QueryForm, Section, Series, WindowKind};
use arrow::array::{Float64Array, RecordBatch, TimestampNanosecondArray, UInt64Array};
use arrow::datatypes::{DataType, Field, Schema, TimeUnit};

const DAY: i64 = 86_400 * 1_000_000_000;
const T0: i64 = 10_000 * DAY;

fn rows(entities: u64, days: i64, from_day: i64) -> RecordBatch {
    let n = (entities as i64 * days) as usize;
    let (mut a, mut t, mut p) = (Vec::with_capacity(n), Vec::with_capacity(n), Vec::with_capacity(n));
    let mut x: u64 = 0x2545_f491_4f6c_dd1d;
    for d in from_day..from_day + days {
        for e in 1..=entities {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            a.push(e);
            t.push(T0 + d * DAY);
            p.push(10.0 + (x % 100_000) as f64 / 997.0);
        }
    }
    let schema = Arc::new(Schema::new(vec![
        Field::new("A", DataType::UInt64, false),
        Field::new("T", DataType::Timestamp(TimeUnit::Nanosecond, None), false),
        Field::new("P", DataType::Float64, false),
    ]));
    RecordBatch::try_new(
        schema,
        vec![Arc::new(UInt64Array::from(a)), Arc::new(TimestampNanosecondArray::from(t)), Arc::new(Float64Array::from(p))],
    )
    .unwrap()
}

fn ent(e: u64) -> Option<Box<Identity>> {
    Some(Box::new(Identity {
        values: vec![Constant {
            kind: ConstantKind::Entity,
            num: 0.0,
            int: e as i64,
            text: None,
        }],
    }))
}

fn median_us(mut v: Vec<f64>) -> f64 {
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    v[v.len() / 2]
}

#[test]
#[ignore]
fn measure_the_budgets() {
    let (entities, days) = (2_000u64, 2_520i64);
    let mut s = Store::temporary("budgets").unwrap();
    // Ingest, by year.
    let start = Instant::now();
    let mut total = 0usize;
    for y in 0..10 {
        let b = rows(entities, 252, y * 252);
        total += b.num_rows();
        s.ingest("equity.close", &[b], "bench", BTreeMap::new()).unwrap();
    }
    let secs = start.elapsed().as_secs_f64();
    println!("ingest: {} rows in {:.2} s = {:.0} rows/s", total, secs, total as f64 / secs);
    let disk = s.disk_bytes().unwrap();
    println!("footprint on disk: {} bytes, {:.2} B per value ({} values)", disk, disk as f64 / (total * 4) as f64, total * 4);
    let view = ResolvedView::default();
    // What the aggregate pass reads first: every row, latest revisions.
    let version = view.version(&s, "equity.close").unwrap();
    let t = Instant::now();
    let read = s
        .read(
            &version,
            &abt_data::store::RowFilter {
                latest_only: true,
                ..Default::default()
            },
        )
        .unwrap();
    println!(
        "read of every row, latest revisions: {} rows in {:.2} s",
        read.iter().map(|b| b.num_rows()).sum::<usize>(),
        t.elapsed().as_secs_f64()
    );
    drop(read);
    let q = |s: &Store, f: &QueryForm| {
        let t = Instant::now();
        let (_, b) = execute(s, &view, f, None, false).unwrap();
        (t.elapsed().as_secs_f64() * 1e6, b.iter().map(|x| x.num_rows()).sum::<usize>())
    };
    // The four forms from disk (no resident cache), then resident (a pass
    // to decode, then the pass measured; E-74).
    let measure = |s: &Store, label: &str| {
        let mut point = Vec::new();
        let mut latest = Vec::new();
        let mut series = Vec::new();
        let mut section = Vec::new();
        for i in 0..50u64 {
            let e = 1 + (i * 37) % entities;
            let d = ((i * 101) % days as u64) as i64;
            point.push(
                q(
                    s,
                    &QueryForm::Point(Box::new(Point {
                        relation: "equity.close".into(),
                        identity: ent(e),
                        t: T0 + d * DAY,
                    })),
                )
                .0,
            );
            latest.push(
                q(
                    s,
                    &QueryForm::Latest(Box::new(Latest {
                        relation: "equity.close".into(),
                        identity: ent(e),
                        t_max: T0 + d * DAY + DAY / 2,
                    })),
                )
                .0,
            );
            let d0 = d.min(days - 252);
            let (us, n) = q(
                s,
                &QueryForm::Series(Box::new(Series {
                    relation: "equity.close".into(),
                    identity: ent(e),
                    t0: T0 + d0 * DAY,
                    t1: T0 + (d0 + 251) * DAY,
                })),
            );
            assert_eq!(n, 252);
            series.push(us);
            let (us, n) = q(
                s,
                &QueryForm::Section(Box::new(Section {
                    relation: "equity.close".into(),
                    t: T0 + d * DAY,
                })),
            );
            assert_eq!(n as u64, entities);
            section.push(us);
        }
        println!("point {}: median {:.0} us", label, median_us(point));
        println!("latest {}: median {:.0} us", label, median_us(latest));
        println!("series of 252 rows {}: median {:.0} us", label, median_us(series));
        println!("section of {} entities {}: median {:.0} us", entities, label, median_us(section));
    };
    s.set_resident_budget(0);
    measure(&s, "from disk");
    s.set_resident_budget(8 << 30);
    measure(&s, "on first touch");
    measure(&s, "resident");
    let st = s.resident_stats();
    println!("resident: {} row groups, {} bytes, {} hits, {} misses", st.groups, st.bytes, st.hits, st.misses);
    // The rolling aggregate: a 252-row mean per entity over the whole span.
    for (threads, numerics) in [(1, Numerics::TwoPass), (8, Numerics::TwoPass), (1, Numerics::Running), (8, Numerics::Running)] {
        abt_data::agg::set_threads(threads);
        let request = AggregateRequest {
            relation: "equity.close".into(),
            column: "P".into(),
            second_column: None,
            function: "mean".into(),
            level: None,
            window: WindowKind::Rows,
            n: 252,
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
            output: Some(format!("bench_{}_{:?}", threads, numerics)),
        };
        let t = Instant::now();
        aggregate(&mut s, &view, &request).unwrap();
        let secs = t.elapsed().as_secs_f64();
        println!(
            "rolling 252-row mean, {:?}, {} thread(s), {} entities x {} days: {:.2} s; scaled to 10,000 x 7,560: {:.1} s",
            numerics,
            threads,
            entities,
            days,
            secs,
            secs * (10_000.0 * 7_560.0) / (entities as f64 * days as f64)
        );
    }
    abt_data::agg::set_threads(0);
}
