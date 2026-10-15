//! The aggregate pass (D-6) against `abt-agg` applied directly to the rows
//! each window holds (Proposition D-6.1's data-side half): `rows(N)` and
//! `prior_rows(N)` over anchored labels with the running sums' checkpoints
//! on the calendar's label index (E-27), `window(δ)` with a calendar
//! duration, `bucket(C)`, `min K`, per-label and whole groups, arithmetic
//! failures carried as messages, and the cache keyed by request and
//! versions.

use std::collections::BTreeMap;
use std::sync::Arc;

use abt_agg::{reduce, Function, Mode, RowsWindow};
use abt_data::agg::{aggregate, answer};
use abt_data::query::ResolvedView;
use abt_data::Store;
use abt_wire::generated::abt::wire::{AggregateRequest, Duration, Grouping, Numerics, ReceiptFilter, WindowKind};
use arrow::array::{Array, AsArray, Float64Array, RecordBatch, StringArray, TimestampNanosecondArray, UInt64Array};
use arrow::datatypes::{DataType, Field, Schema, TimeUnit};

const DAY: i64 = 86_400 * 1_000_000_000;
const T0: i64 = 19_000 * DAY;
const DAYS: i64 = 40;

fn close_rows(rows: &[(u64, i64, f64)]) -> RecordBatch {
    let schema = Arc::new(Schema::new(vec![
        Field::new("A", DataType::UInt64, false),
        Field::new("T", DataType::Timestamp(TimeUnit::Nanosecond, None), false),
        Field::new("P", DataType::Float64, false),
    ]));
    RecordBatch::try_new(
        schema,
        vec![
            Arc::new(UInt64Array::from(rows.iter().map(|r| r.0).collect::<Vec<_>>())),
            Arc::new(TimestampNanosecondArray::from(rows.iter().map(|r| r.1).collect::<Vec<_>>())),
            Arc::new(Float64Array::from(rows.iter().map(|r| r.2).collect::<Vec<_>>())),
        ],
    )
    .unwrap()
}

fn calendar_rows(rows: &[(&str, i64)]) -> RecordBatch {
    let schema = Arc::new(Schema::new(vec![
        Field::new("calendar", DataType::Utf8, false),
        Field::new("label", DataType::Timestamp(TimeUnit::Nanosecond, None), false),
        Field::new("at", DataType::Timestamp(TimeUnit::Nanosecond, None), false),
    ]));
    RecordBatch::try_new(
        schema,
        vec![
            Arc::new(StringArray::from(rows.iter().map(|r| r.0).collect::<Vec<_>>())),
            Arc::new(TimestampNanosecondArray::from(rows.iter().map(|r| r.1).collect::<Vec<_>>())),
            Arc::new(TimestampNanosecondArray::from(rows.iter().map(|r| r.1 + DAY - 1).collect::<Vec<_>>())),
        ],
    )
    .unwrap()
}

/// A price walk per instrument; the third trades every other day; one row
/// keyed off the calendar (a Sunday the calendar does not hold).
fn data() -> Vec<(u64, i64, f64)> {
    let mut x: u64 = 0x2545_f491_4f6c_dd1d;
    let mut out = Vec::new();
    for d in 0..DAYS {
        for a in 1..=3u64 {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            if a == 3 && d % 2 == 1 {
                continue;
            }
            let p = 100.0 * a as f64 + (x % 10_000) as f64 / 97.0 + d as f64 * 0.37;
            out.push((a, T0 + d * DAY, p));
        }
    }
    out.push((1, T0 + DAYS * DAY + DAY / 2, 1e6));
    out
}

fn store() -> Store {
    let mut s = Store::temporary("agg").unwrap();
    s.ingest("equity.close", &[close_rows(&data())], "test", BTreeMap::new()).unwrap();
    let mut cal: Vec<(&str, i64)> = (0..DAYS).map(|d| ("1d", T0 + d * DAY)).collect();
    cal.extend((0..DAYS).filter(|d| d % 5 == 4).map(|d| ("1w", T0 + d * DAY)));
    s.ingest("calendar_label", &[calendar_rows(&cal)], "test", BTreeMap::new()).unwrap();
    s
}

fn request(function: &str, window: WindowKind) -> AggregateRequest {
    AggregateRequest {
        relation: "equity.close".into(),
        column: "P".into(),
        second_column: None,
        function: function.into(),
        level: None,
        window,
        n: 0,
        min: None,
        span: None,
        bucket: None,
        group: Grouping::PerEntity,
        group_column: None,
        receipt_filter: None,
        numerics: Numerics::TwoPass,
        calendar: Some("1d".into()),
        t0: None,
        t1: None,
        output: None,
    }
}

/// An answer's row: (group, t, value, count, error).
type Row = (u64, i64, Option<f64>, i64, Option<String>);

fn rows(s: &mut Store, req: &AggregateRequest) -> Vec<Row> {
    let (version, _) = aggregate(s, &ResolvedView::default(), req).unwrap();
    let b = answer(s, &version).unwrap();
    let schema = b.schema();
    let tcol = if schema.index_of("t").is_ok() { "t" } else { "label" };
    let col = |n: &str| b.column(schema.index_of(n).unwrap()).clone();
    let g = schema.index_of("entity").ok().map(|i| b.column(i).clone());
    let t = col(tcol);
    let v = col("value");
    let c = col("count");
    let e = col("error");
    let e = arrow::compute::cast(&e, &DataType::Utf8).unwrap();
    (0..b.num_rows())
        .map(|i| {
            (
                g.as_ref().map(|g| g.as_primitive::<arrow::datatypes::UInt64Type>().value(i)).unwrap_or(0),
                t.as_primitive::<arrow::datatypes::TimestampNanosecondType>().value(i),
                (!v.is_null(i)).then(|| v.as_primitive::<arrow::datatypes::Float64Type>().value(i)),
                c.as_primitive::<arrow::datatypes::Int64Type>().value(i),
                (!e.is_null(i)).then(|| e.as_string::<i32>().value(i).to_string()),
            )
        })
        .collect()
}

/// The rows of an instrument at the calendar's labels.
fn series(a: u64) -> Vec<(i64, f64)> {
    data().into_iter().filter(|r| r.0 == a && (r.1 - T0) % DAY == 0).map(|r| (r.1, r.2)).collect()
}

#[test]
fn rows_windows_are_the_reduction_of_the_last_n_anchored_rows() {
    let mut s = store();
    for f in ["mean", "std", "sum", "min", "max", "median", "last"] {
        let mut req = request(f, WindowKind::Rows);
        req.n = 5;
        let got = rows(&mut s, &req);
        let mut want = Vec::new();
        for a in 1..=3u64 {
            let xs = series(a);
            for i in 4..xs.len() {
                let w: Vec<f64> = xs[i - 4..=i].iter().map(|r| r.1).collect();
                let v = reduce(Function::parse(f, None).unwrap(), Mode::TwoPass, &[&w]).unwrap();
                want.push((a, xs[i].0, v, 5, None::<String>));
            }
        }
        want.sort_by_key(|r| (r.1, r.0));
        assert_eq!(got.len(), want.len(), "{}", f);
        for (g, w) in got.iter().zip(&want) {
            assert_eq!((g.0, g.1, g.3), (w.0, w.1, w.3), "{}", f);
            assert_eq!(g.2.map(f64::to_bits), w.2.map(f64::to_bits), "{} at {:?}", f, g);
        }
    }
}

#[test]
fn prior_rows_and_min_count() {
    let mut s = store();
    let mut req = request("mean", WindowKind::PriorRows);
    req.n = 3;
    req.min = Some(2);
    let got = rows(&mut s, &req);
    for a in 1..=3u64 {
        let xs = series(a);
        for i in 0..xs.len() {
            let lo = i.saturating_sub(3);
            let w: Vec<f64> = xs[lo..i].iter().map(|r| r.1).collect();
            let row = got.iter().find(|r| r.0 == a && r.1 == xs[i].0);
            if w.len() < 2 {
                assert!(row.is_none(), "{} at {}", a, i);
            } else {
                let v = reduce(Function::Mean, Mode::TwoPass, &[&w]).unwrap();
                assert_eq!(row.unwrap().2.map(f64::to_bits), v.map(f64::to_bits));
                assert_eq!(row.unwrap().3 as usize, w.len());
            }
        }
    }
}

#[test]
fn running_rows_windows_walk_the_calendar_labels() {
    let mut s = store();
    let mut req = request("std", WindowKind::Rows);
    req.n = 6;
    req.numerics = Numerics::Running;
    let got = rows(&mut s, &req);
    let labels: Vec<i64> = (0..DAYS).map(|d| T0 + d * DAY).collect();
    for a in 1..=3u64 {
        let xs = series(a);
        // The walk the pass makes, by hand: every label, anchored where the
        // instrument has a row.
        let mut w = RowsWindow::new(Function::Std, Mode::Running, 6);
        for (i, &t) in labels.iter().enumerate() {
            let row = xs.iter().find(|r| r.0 == t).map(|r| [r.1]);
            if t < xs[0].0 {
                continue;
            }
            w.step(i as u64, t, row.is_some(), row.as_ref().map(|x| x.as_slice()));
            if row.is_some() {
                let v = w.value(6).unwrap();
                let g = got.iter().find(|r| r.0 == a && r.1 == t);
                assert_eq!(g.and_then(|g| g.2).map(f64::to_bits), v.map(f64::to_bits), "{} at {}", a, i);
                // And within rounding of the exact reduction.
                if let (Some(v), Some(k)) = (v, xs.iter().position(|r| r.0 == t)) {
                    if k >= 5 {
                        let exact: Vec<f64> = xs[k - 5..=k].iter().map(|r| r.1).collect();
                        let e = reduce(Function::Std, Mode::TwoPass, &[&exact]).unwrap().unwrap();
                        assert!((v - e).abs() <= 1e-9 * e.abs().max(1.0), "{} vs {}", v, e);
                    }
                }
            }
        }
    }
}

#[test]
fn span_windows_use_calendar_durations() {
    let mut s = store();
    let mut req = request("sum", WindowKind::Span);
    req.span = Some(Box::new(Duration { months: 1, days: 0, nanos: 0 }));
    let got = rows(&mut s, &req);
    for a in 1..=3u64 {
        let xs = series(a);
        for &(t, _) in &xs {
            let lo = abt_wire::scalar::sub_duration(t, 1, 0, 0);
            let w: Vec<f64> = xs.iter().filter(|r| r.0 >= lo && r.0 <= t).map(|r| r.1).collect();
            let v = reduce(Function::Sum, Mode::TwoPass, &[&w]).unwrap();
            let g = got.iter().find(|r| r.0 == a && r.1 == t).unwrap();
            assert_eq!(g.2.map(f64::to_bits), v.map(f64::to_bits));
            assert_eq!(g.3 as usize, w.len());
        }
    }
    // The prior form excludes t; the row off the calendar is in no window.
    req.window = WindowKind::PriorSpan;
    req.span = Some(Box::new(Duration { months: 0, days: 3, nanos: 0 }));
    let got = rows(&mut s, &req);
    let xs = series(1);
    let t = xs[10].0;
    let w: Vec<f64> = xs.iter().filter(|r| r.0 >= t - 3 * DAY && r.0 < t).map(|r| r.1).collect();
    let g = got.iter().find(|r| r.0 == 1 && r.1 == t).unwrap();
    assert_eq!(g.2.map(f64::to_bits), reduce(Function::Sum, Mode::TwoPass, &[&w]).unwrap().map(f64::to_bits));
    assert!(got.iter().all(|r| r.2 != Some(1e6) && r.1 < T0 + DAYS * DAY));
}

#[test]
fn bucket_windows_are_keyed_by_the_bucket_label() {
    let mut s = store();
    let mut req = request("max", WindowKind::Bucket);
    req.bucket = Some("1w".into());
    let got = rows(&mut s, &req);
    let weeks: Vec<i64> = (0..DAYS).filter(|d| d % 5 == 4).map(|d| T0 + d * DAY).collect();
    assert_eq!(got.len(), weeks.len() * 3);
    for a in 1..=3u64 {
        let xs = series(a);
        let mut prev = i64::MIN;
        for &l in &weeks {
            let w: Vec<f64> = xs.iter().filter(|r| r.0 > prev && r.0 <= l).map(|r| r.1).collect();
            let g = got.iter().find(|r| r.0 == a && r.1 == l).unwrap();
            assert_eq!(g.2.map(f64::to_bits), reduce(Function::Max, Mode::TwoPass, &[&w]).unwrap().map(f64::to_bits));
            prev = l;
        }
    }
}

#[test]
fn whole_groups_and_two_columns_and_failures() {
    let mut s = store();
    // A cross-sectional mean over a one-day window: every instrument's rows.
    let mut req = request("mean", WindowKind::Span);
    req.group = Grouping::Whole;
    req.span = Some(Box::new(Duration { months: 0, days: 0, nanos: 0 }));
    let got = rows(&mut s, &req);
    let t = T0 + 7 * DAY;
    let xs: Vec<f64> = data().into_iter().filter(|r| r.1 == t).map(|r| r.2).collect();
    let g = got.iter().find(|r| r.1 == t).unwrap();
    assert_eq!(g.2.map(f64::to_bits), reduce(Function::Mean, Mode::TwoPass, &[&xs]).unwrap().map(f64::to_bits));
    assert_eq!(g.3, 2, "the third instrument does not trade on odd days");
    // The correlation of a column with itself is 1; of a constant, a failure.
    let mut req = request("corr", WindowKind::Rows);
    req.second_column = Some("P".into());
    req.n = 4;
    let got = rows(&mut s, &req);
    assert!(got.iter().all(|r| r.2.is_some_and(|v| (v - 1.0).abs() < 1e-12)));
    let mut flat = Store::temporary("agg-flat").unwrap();
    flat.ingest("equity.close", &[close_rows(&(0..6).map(|d| (1, T0 + d * DAY, 5.0)).collect::<Vec<_>>())], "test", BTreeMap::new())
        .unwrap();
    let mut req = request("corr", WindowKind::Rows);
    req.second_column = Some("P".into());
    req.n = 3;
    req.calendar = None;
    let got = rows(&mut flat, &req);
    assert_eq!(got.len(), 4);
    assert!(got.iter().all(|r| r.2.is_none() && r.4.as_deref() == Some("correlation of a constant series")), "{:?}", got);
}

#[test]
fn answers_are_cache_objects_keyed_by_request_and_versions() {
    let mut s = store();
    let mut req = request("mean", WindowKind::Rows);
    req.n = 3;
    let (v1, k1) = aggregate(&mut s, &ResolvedView::default(), &req).unwrap();
    let (v2, k2) = aggregate(&mut s, &ResolvedView::default(), &req).unwrap();
    assert_eq!((v1.clone(), k1), (v2, k2), "a hit");
    req.numerics = Numerics::Running;
    let (_, k3) = aggregate(&mut s, &ResolvedView::default(), &req).unwrap();
    assert_ne!(k1, k3, "the numerics are part of the key (D-6.3)");
    // New input rows: a new version of the input, a new object.
    s.ingest("equity.close", &[close_rows(&[(1, T0 + 50 * DAY, 1.0)])], "test", BTreeMap::new()).unwrap();
    req.numerics = Numerics::TwoPass;
    let (v4, k4) = aggregate(&mut s, &ResolvedView::default(), &req).unwrap();
    assert_ne!(k1, k4);
    assert_ne!(v1, v4);
    // A receipt filter is part of the request.
    req.receipt_filter = Some(Box::new(ReceiptFilter { r_max: Some(T0 + 10 * DAY) }));
    let got = rows(&mut s, &req);
    assert!(got.iter().all(|r| r.1 <= T0 + 10 * DAY));
    // A range of the answer.
    req.receipt_filter = None;
    req.t0 = Some(T0 + 20 * DAY);
    req.t1 = Some(T0 + 22 * DAY);
    let got = rows(&mut s, &req);
    assert!(got.iter().all(|r| r.1 >= T0 + 20 * DAY && r.1 <= T0 + 22 * DAY) && !got.is_empty());
}
