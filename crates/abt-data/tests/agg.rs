//! The aggregate pass (D-6) against `abt-agg` applied directly to the rows
//! each window holds (Proposition D-6.1's data-side half): `rows(N)` and
//! `prior_rows(N)` over anchored labels with the running sums' checkpoints
//! on the calendar's label index (E-27), `window(δ)` with a calendar
//! duration, `bucket(C)`, `min K`, per-label and whole groups, arithmetic
//! failures carried as messages, and the cache keyed by request and
//! versions.

use std::collections::BTreeMap;
use std::sync::Arc;

use abt_agg::{reduce, reduce_ticks, Function, Mode, RowsWindow};
use abt_data::agg::{aggregate, answer, set_batch_rows};
use abt_data::query::ResolvedView;
use abt_data::Store;
use abt_wire::generated::abt::wire::{AggregateRequest, Duration, Factor, Grouping, Numerics, ReceiptFilter, WindowKind};
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
        factor: None,
        entities: None,
    }
}

/// An answer's row: (group, t, value, count, error).
type Row = (u64, i64, Option<f64>, i64, Option<String>);

fn rows(s: &mut Store, req: &AggregateRequest) -> Vec<Row> {
    let version = aggregate(s, &ResolvedView::default(), req).unwrap().version;
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

fn rows_of(s: &mut Store, req: &AggregateRequest) -> Vec<Row> {
    rows(s, req)
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
    let a1 = aggregate(&mut s, &ResolvedView::default(), &req).unwrap();
    let a2 = aggregate(&mut s, &ResolvedView::default(), &req).unwrap();
    assert_eq!(a1, a2, "a hit");
    let (v1, k1) = (a1.version, a1.key);
    req.numerics = Numerics::Running;
    let k3 = aggregate(&mut s, &ResolvedView::default(), &req).unwrap().key;
    assert_ne!(k1, k3, "the numerics are part of the key (D-6.3)");
    // New input rows: a new version of the input, a new object.
    s.ingest("equity.close", &[close_rows(&[(1, T0 + 50 * DAY, 1.0)])], "test", BTreeMap::new()).unwrap();
    req.numerics = Numerics::TwoPass;
    let a4 = aggregate(&mut s, &ResolvedView::default(), &req).unwrap();
    let (v4, k4) = (a4.version, a4.key);
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

/// The pass in its two halves (ERRATA E-168): `prepare` reads the store
/// and computes, `finish` stores. Two requests that both missed give one
/// object, the second finding what the first stored; and the two halves
/// give `aggregate`'s answer.
#[test]
fn two_passes_of_one_request_that_both_missed_store_one_object() {
    use abt_data::agg::{finish, prepare, Looked};
    let mut s = store();
    let mut req = request("mean", WindowKind::Rows);
    req.n = 3;
    let view = ResolvedView::default();
    let pending = |s: &Store| match prepare(s, &view, &req).unwrap() {
        Looked::Computed(p) => *p,
        Looked::Hit(_) => panic!("nothing is cached yet"),
    };
    let (first, second) = (pending(&s), pending(&s));
    let a1 = finish(&mut s, first).unwrap();
    let versions = |s: &Store| s.registry.versions().filter(|v| v.relation == a1.relation).count();
    let stored = versions(&s);
    let a2 = finish(&mut s, second).unwrap();
    assert_eq!(a1, a2, "the second pass is answered by the first's object");
    assert_eq!(versions(&s), stored, "and writes none of its own");
    assert!(matches!(prepare(&s, &view, &req).unwrap(), Looked::Hit(a) if a == a1));
    // The same rows as the pass in one call, on a store of its own.
    let mut whole = store();
    let b = aggregate(&mut whole, &view, &req).unwrap();
    assert_eq!((a1.key, a1.rows, a1.inexact_rows), (b.key, b.rows, b.inexact_rows));
    assert_eq!(answer(&s, &a1.version).unwrap(), answer(&whole, &b.version).unwrap());
}

/// A request protocol 1.2 could write keeps the cache key a 1.2 tree gave
/// it (pinned from one): the fields 1.3 added are left out of its canonical
/// form while absent (ERRATA E-155), so every cached answer is found again.
#[test]
fn a_1_2_request_keeps_its_cache_key() {
    let mut req = request("mean", WindowKind::Bucket);
    req.bucket = Some("1w".into());
    req.min = Some(2);
    let versions = vec![("equity.close".to_string(), "ab12".to_string()), ("calendar_label".to_string(), "cd34".to_string())];
    let key = abt_data::agg::cache_key(&req, &versions).unwrap();
    assert_eq!(key.hex(), "e031b5180917ad66d4a6784cdc2df42ab4b4ac7b437f5a8c5ec36712e8fda317");
    req.entities = Some(vec![1]);
    assert_ne!(abt_data::agg::cache_key(&req, &versions).unwrap(), key, "a scoped request is its own object");
}

// ---------------------------------------------------------------- exact

/// The price walk on cents: values on a decimal grid, as a vendor's are.
fn data_cents() -> Vec<(u64, i64, f64)> {
    data().into_iter().map(|(a, t, p)| (a, t, (p * 100.0).round() / 100.0)).collect()
}

/// A store holding the cents walk hot (07-storage.md).
fn hot_store(rows: &[(u64, i64, f64)]) -> Store {
    let mut s = Store::temporary("agg-hot").unwrap();
    let hot = BTreeMap::from([("storage".to_string(), "hot".to_string())]);
    s.ingest("equity.close", &[close_rows(rows)], "test", hot).unwrap();
    let mut cal: Vec<(&str, i64)> = (0..DAYS).map(|d| ("1d", T0 + d * DAY)).collect();
    cal.extend((0..DAYS).filter(|d| d % 5 == 4).map(|d| ("1w", T0 + d * DAY)));
    s.ingest("calendar_label", &[calendar_rows(&cal)], "test", BTreeMap::new()).unwrap();
    s
}

/// An instrument's rows at the calendar's labels, as cents.
fn units(rows: &[(u64, i64, f64)], a: u64) -> Vec<(i64, i128)> {
    rows.iter().filter(|r| r.0 == a && (r.1 - T0) % DAY == 0).map(|r| (r.1, (r.2 * 100.0).round() as i128)).collect()
}

#[test]
fn exact_rows_windows_over_a_hot_relation_are_the_exact_reduction_of_the_ticks() {
    let rows = data_cents();
    let mut s = hot_store(&rows);
    assert!(s.is_hot(s.registry.current("equity.close", Some("test")).unwrap()));
    let labels: Vec<i64> = (0..DAYS).map(|d| T0 + d * DAY).collect();
    for f in ["sum", "mean", "var", "std", "median", "min", "max", "first", "last", "count"] {
        let mut req = request(f, WindowKind::Rows);
        req.n = 5;
        req.numerics = Numerics::Exact;
        let got = rows_of(&mut s, &req);
        for a in 1..=3u64 {
            let ticks = units(&rows, a);
            // The same walk through abt-agg over the ticks (P-1.1).
            let func = Function::parse(f, None).unwrap();
            let mut w = RowsWindow::<i128>::with_scales(func, Mode::Exact, [2, 0], 5);
            let mut expect: Vec<(i64, Option<f64>)> = Vec::new();
            let mut next = 0usize;
            for (index, &t) in labels.iter().enumerate() {
                while next < ticks.len() && ticks[next].0 < t {
                    next += 1;
                }
                let row = (next < ticks.len() && ticks[next].0 == t).then(|| [ticks[next].1, 0i128]);
                if t < ticks[0].0 {
                    continue;
                }
                w.step(index as u64, t, row.is_some(), row.as_ref().map(|x| x.as_slice()));
                if row.is_some() {
                    // The pass emits a value or an error, never an empty window.
                    match w.value(5) {
                        Ok(Some(v)) => expect.push((t, Some(v))),
                        Ok(None) => {}
                        Err(_) => expect.push((t, None)),
                    }
                }
            }
            let mine: Vec<(i64, Option<f64>)> = got.iter().filter(|r| r.0 == a).map(|r| (r.1, r.2)).collect();
            assert_eq!(mine, expect, "{} of instrument {}", f, a);
        }
        // Exact and two-pass agree on the values that are exact in both
        // (a selection), and the exact mean is the correctly rounded
        // rational of cents.
        if f == "mean" {
            for r in got.iter().filter(|r| r.2.is_some()) {
                let ticks = units(&rows, r.0);
                let at = ticks.iter().position(|x| x.0 == r.1).unwrap();
                let window: Vec<i128> = ticks[at + 1 - 5..=at].iter().map(|x| x.1).collect();
                let sum: i128 = window.iter().sum();
                let expect = abt_agg::exact::ratio_to_f64(&abt_agg::exact::Int::from_i128(sum), &abt_agg::exact::Wide::from_u128(500));
                assert_eq!(r.2.unwrap().to_bits(), expect.to_bits());
            }
        }
    }
}

#[test]
fn exact_span_and_bucket_windows_over_a_hot_relation() {
    let rows = data_cents();
    let mut s = hot_store(&rows);
    for f in ["sum", "mean", "var", "median", "quantile", "max"] {
        let mut req = request(f, WindowKind::Bucket);
        req.bucket = Some("1w".into());
        req.numerics = Numerics::Exact;
        if f == "quantile" {
            req.level = Some(0.25);
        }
        let got = rows_of(&mut s, &req);
        let func = Function::parse(f, if f == "quantile" { Some(0.25) } else { None }).unwrap();
        let buckets: Vec<i64> = (0..DAYS).filter(|d| d % 5 == 4).map(|d| T0 + d * DAY).collect();
        for a in 1..=3u64 {
            let ticks = units(&rows, a);
            let mut expect = Vec::new();
            let mut lo = i64::MIN;
            for &b in &buckets {
                let w: Vec<i128> = ticks.iter().filter(|x| x.0 > lo && x.0 <= b).map(|x| x.1).collect();
                if !w.is_empty() {
                    match reduce_ticks(func, &[abt_agg::Ticks { units: &w, scale: 2 }]) {
                        Ok(Some(v)) => expect.push((b, Some(v))),
                        Ok(None) => {}
                        Err(_) => expect.push((b, None)),
                    }
                }
                lo = b;
            }
            let mine: Vec<(i64, Option<f64>)> = got.iter().filter(|r| r.0 == a).map(|r| (r.1, r.2)).collect();
            assert_eq!(mine, expect, "{} of instrument {}", f, a);
        }
        // A span window of 3 days.
        let mut req = request(f, WindowKind::Span);
        req.span = Some(Box::new(Duration { months: 0, days: 3, nanos: 0 }));
        req.numerics = Numerics::Exact;
        if f == "quantile" {
            req.level = Some(0.25);
        }
        let got = rows_of(&mut s, &req);
        for a in 1..=3u64 {
            let ticks = units(&rows, a);
            let mut expect = Vec::new();
            for &(t, _) in &ticks {
                let w: Vec<i128> = ticks.iter().filter(|x| x.0 >= t - 3 * DAY && x.0 <= t).map(|x| x.1).collect();
                match reduce_ticks(func, &[abt_agg::Ticks { units: &w, scale: 2 }]) {
                    Ok(Some(v)) => expect.push((t, Some(v))),
                    Ok(None) => {}
                    Err(_) => expect.push((t, None)),
                }
            }
            let mine: Vec<(i64, Option<f64>)> = got.iter().filter(|r| r.0 == a).map(|r| (r.1, r.2)).collect();
            assert_eq!(mine, expect, "{} span of instrument {}", f, a);
        }
    }
}

#[test]
fn exact_is_refused_over_values_off_any_grid_and_two_pass_still_answers() {
    // The raw walk's prices are quotients: off every decimal grid.
    let mut s = hot_store(&data());
    let mut req = request("mean", WindowKind::Rows);
    req.n = 5;
    req.numerics = Numerics::Exact;
    let err = aggregate(&mut s, &ResolvedView::default(), &req).expect_err("exact refused");
    assert!(err.to_string().contains("off any decimal grid"), "{}", err);
    req.numerics = Numerics::TwoPass;
    assert!(!rows_of(&mut s, &req).is_empty());
}

// ------------------------------------------------- protocol 1.3 (E-156)

/// A row of a relation keyed by an instrument and a time: (instrument, t,
/// receipt, value), the value none where null.
type Fact = (u64, i64, i64, Option<f64>);

/// Rows of a relation whose value column is `column`, with their receipts.
fn fact_rows(column: &str, rows: &[Fact]) -> RecordBatch {
    let schema = Arc::new(Schema::new(vec![
        Field::new("A", DataType::UInt64, false),
        Field::new("T", DataType::Timestamp(TimeUnit::Nanosecond, None), false),
        Field::new(column, DataType::Float64, true),
        Field::new("r", DataType::Timestamp(TimeUnit::Nanosecond, None), false),
    ]));
    RecordBatch::try_new(
        schema,
        vec![
            Arc::new(UInt64Array::from(rows.iter().map(|r| r.0).collect::<Vec<_>>())),
            Arc::new(TimestampNanosecondArray::from(rows.iter().map(|r| r.1).collect::<Vec<_>>())),
            Arc::new(Float64Array::from(rows.iter().map(|r| r.3).collect::<Vec<_>>())),
            Arc::new(TimestampNanosecondArray::from(rows.iter().map(|r| r.2).collect::<Vec<_>>())),
        ],
    )
    .unwrap()
}

/// A store of the relations given, (name, value column, rows), held hot or
/// not, with `store`'s calendars.
fn store_of(relations: &[(&str, &str, &[Fact])], hot: bool) -> Store {
    let mut s = Store::temporary("agg-of").unwrap();
    let attributes = if hot { BTreeMap::from([("storage".to_string(), "hot".to_string())]) } else { BTreeMap::new() };
    for (name, column, rows) in relations {
        s.ingest(name, &[fact_rows(column, rows)], "test", attributes.clone()).unwrap();
    }
    let mut cal: Vec<(&str, i64)> = (0..DAYS).map(|d| ("1d", T0 + d * DAY)).collect();
    cal.extend((0..DAYS).filter(|d| d % 5 == 4).map(|d| ("1w", T0 + d * DAY)));
    s.ingest("calendar_label", &[calendar_rows(&cal)], "test", BTreeMap::new()).unwrap();
    s
}

/// The labels of the weekly calendar of `store`.
fn weeks() -> Vec<i64> {
    (0..DAYS).filter(|d| d % 5 == 4).map(|d| T0 + d * DAY).collect()
}

/// Whether a time is a label of the daily calendar.
fn daily(t: i64) -> bool {
    (t - T0) % DAY == 0 && t < T0 + DAYS * DAY
}

/// A relation's column as a stream carries it: (instrument, t) to value.
fn streamed(s: &Store, relation: &str, column: &str) -> BTreeMap<(u64, i64), f64> {
    use abt_wire::generated::abt::wire::{ClockSource, StreamSpec, VersionRef};
    let spec = StreamSpec {
        relations: Some(vec![VersionRef {
            relation: relation.into(),
            version: s.registry.current(relation, Some("test")).unwrap().to_string(),
        }]),
        binding: None,
        calendars: None,
        receipt_filter: None,
        clock: ClockSource::Replay,
        session: None,
        entities: None,
        from: None,
        whole: None,
    };
    let mut stream = abt_data::stream::Stream::open(s, abt_data::stream::resolve(s, &spec).unwrap()).unwrap();
    let mut out = BTreeMap::new();
    while let Some(b) = stream.next_batch(s, 0, None, 1_000).unwrap() {
        let col = |n: &str| b.column(b.schema().index_of(n).unwrap()).clone();
        let (entity, t, v) = (col("entity"), col("t"), col(&format!("{}.{}", relation, column)));
        for i in 0..b.num_rows() {
            out.insert(
                (
                    entity.as_primitive::<arrow::datatypes::UInt64Type>().value(i),
                    t.as_primitive::<arrow::datatypes::TimestampNanosecondType>().value(i),
                ),
                v.as_primitive::<arrow::datatypes::Float64Type>().value(i),
            );
        }
    }
    out
}

/// `count` counts the window's rows, with a value or not, as the kernel's
/// `count` counts the rows its window solves; a null is still an inexact
/// row, since the kernel reads it as NaN or refuses it.
#[test]
fn count_counts_the_windows_rows_with_or_without_a_value() {
    let nulls = [(2u64, T0 + 3 * DAY), (1u64, T0 + 11 * DAY)];
    let rows: Vec<Fact> = data().into_iter().map(|(a, t, p)| (a, t, t, (!nulls.contains(&(a, t))).then_some(p))).collect();
    let mut s = store_of(&[("equity.close", "P", &rows)], false);
    let mut req = request("count", WindowKind::Bucket);
    req.bucket = Some("1w".into());
    let answered = aggregate(&mut s, &ResolvedView::default(), &req).unwrap();
    assert_eq!(answered.inexact_rows, nulls.len() as u64);
    let got = rows_of(&mut s, &req);
    let mut want: Vec<Row> = Vec::new();
    for a in 1..=3u64 {
        let mut prev = i64::MIN;
        for l in weeks() {
            let n = rows.iter().filter(|r| r.0 == a && r.1 > prev && r.1 <= l && daily(r.1)).count() as i64;
            if n > 0 {
                want.push((a, l, Some(n as f64), n, None));
            }
            prev = l;
        }
    }
    want.sort_by_key(|r| (r.1, r.0));
    assert_eq!(got, want);
    // A rows window: three rows at every row from the third, a null one too.
    req.window = WindowKind::Rows;
    req.bucket = None;
    req.n = 3;
    let got = rows_of(&mut s, &req);
    for a in 1..=3u64 {
        let ts: Vec<i64> = rows.iter().filter(|r| r.0 == a && daily(r.1)).map(|r| r.1).collect();
        for t in &ts[2..] {
            let g = got.iter().find(|r| r.0 == a && r.1 == *t).unwrap_or_else(|| panic!("no count for {} at {}", a, t));
            assert_eq!((g.2, g.3), (Some(3.0), 3));
        }
    }
    // A mean leaves the nulls out, as before.
    let mut req = request("mean", WindowKind::Bucket);
    req.bucket = Some("1w".into());
    let got = rows_of(&mut s, &req);
    let g = got.iter().find(|r| r.0 == 2 && r.1 == T0 + 4 * DAY).unwrap();
    assert_eq!(g.3, 4, "five rows in the week, one null");
}

/// A product of two relations: its value is abt-agg's reduction, in time
/// order, of the products `P * V` of the rows both relations hold at one
/// instrument and day, as the stream carries their values (a close
/// without its volume, and a volume without its close, join nothing); bit
/// for bit over Parquet and over the hot store.
#[test]
fn a_product_of_two_relations_is_the_joined_rows_reduced_by_abt_agg_bit_for_bit() {
    let close: Vec<Fact> = data_cents().into_iter().map(|(a, t, p)| (a, t, t, Some(p))).collect();
    let mut volume: Vec<Fact> = close
        .iter()
        .filter(|r| (r.0, r.1) != (2, T0 + 7 * DAY))
        .map(|&(a, t, r, _)| (a, t, r, Some((1_000 + (t / DAY + a as i64 * 37) % 5_000) as f64)))
        .collect();
    // The third instrument trades on even days only.
    volume.push((3, T0 + DAY, T0 + DAY, Some(5.0)));
    volume.sort_by_key(|r| (r.1, r.0));
    for hot in [false, true] {
        let mut s = store_of(&[("equity.close", "P", &close), ("equity.volume", "V", &volume)], hot);
        for r in ["equity.close", "equity.volume"] {
            assert_eq!(s.is_hot(s.registry.current(r, Some("test")).unwrap()), hot, "{}", r);
        }
        let (p, v) = (streamed(&s, "equity.close", "P"), streamed(&s, "equity.volume", "V"));
        let products = |a: u64, lo: i64, hi: i64| -> Vec<f64> {
            p.iter()
                .filter(|((e, t), _)| *e == a && *t > lo && *t <= hi && daily(*t))
                .filter_map(|(k, x)| v.get(k).map(|y| x * y))
                .collect()
        };
        let mut req = request("mean", WindowKind::Bucket);
        req.bucket = Some("1w".into());
        req.factor = Some(Box::new(Factor {
            relation: "equity.volume".into(),
            column: "V".into(),
        }));
        let answered = aggregate(&mut s, &ResolvedView::default(), &req).unwrap();
        assert_eq!(answered.inexact_rows, 0, "hot {}", hot);
        let got = rows_of(&mut s, &req);
        let mut want = Vec::new();
        for a in 1..=3u64 {
            let mut prev = i64::MIN;
            for l in weeks() {
                let xs = products(a, prev, l);
                if !xs.is_empty() {
                    want.push((a, l, reduce(Function::Mean, Mode::TwoPass, &[&xs]).unwrap().map(f64::to_bits), xs.len() as i64));
                }
                prev = l;
            }
        }
        want.sort_by_key(|r| (r.1, r.0));
        let mine: Vec<(u64, i64, Option<u64>, i64)> = got.iter().map(|r| (r.0, r.1, r.2.map(f64::to_bits), r.3)).collect();
        assert_eq!(mine, want, "hot {}", hot);
        let missing = got.iter().find(|r| r.0 == 2 && r.1 == T0 + 9 * DAY).unwrap();
        assert_eq!(missing.3, 4, "the day without a volume joins nothing");
        // A rows window over the joined rows: the product relation's last
        // three rows.
        req.window = WindowKind::Rows;
        req.bucket = None;
        req.n = 3;
        let got = rows_of(&mut s, &req);
        for a in 1..=3u64 {
            let xs = products(a, i64::MIN, i64::MAX);
            let ts: Vec<i64> = p.keys().filter(|(e, t)| *e == a && daily(*t) && v.contains_key(&(a, *t))).map(|k| k.1).collect();
            for (window, t) in xs.windows(3).zip(&ts[2..]) {
                let g = got.iter().find(|r| r.0 == a && r.1 == *t).unwrap();
                assert_eq!(g.2.map(f64::to_bits), reduce(Function::Mean, Mode::TwoPass, &[window]).unwrap().map(f64::to_bits));
            }
        }
    }
}

/// `entities` restricts every read of the pass to those instruments: the
/// answer is the unscoped one's rows of them, under a key of its own, and
/// the instruments are a set.
#[test]
fn an_instrument_filter_answers_only_those_instruments_under_its_own_key() {
    let mut s = store();
    let mut req = request("mean", WindowKind::Rows);
    req.n = 3;
    let whole = aggregate(&mut s, &ResolvedView::default(), &req).unwrap();
    let all = rows_of(&mut s, &req);
    req.entities = Some(vec![3, 1]);
    let scoped = aggregate(&mut s, &ResolvedView::default(), &req).unwrap();
    assert_ne!(scoped.key, whole.key);
    assert_ne!(scoped.relation, whole.relation);
    let got = rows_of(&mut s, &req);
    assert!(got.iter().any(|r| r.0 == 3));
    assert_eq!(got, all.iter().filter(|r| r.0 != 2).cloned().collect::<Vec<_>>());
    req.entities = Some(vec![1, 3, 3]);
    assert_eq!(aggregate(&mut s, &ResolvedView::default(), &req).unwrap(), scoped);
    // A product reads the second relation's rows of those instruments only.
    let close: Vec<Fact> = data().into_iter().map(|(a, t, p)| (a, t, t, Some(p))).collect();
    let volume: Vec<Fact> = close.iter().map(|&(a, t, r, _)| (a, t, r, Some(a as f64))).collect();
    let mut s = store_of(&[("equity.close", "P", &close), ("equity.volume", "V", &volume)], false);
    let mut req = request("sum", WindowKind::Bucket);
    req.bucket = Some("1w".into());
    req.factor = Some(Box::new(Factor {
        relation: "equity.volume".into(),
        column: "V".into(),
    }));
    let all = rows_of(&mut s, &req);
    req.entities = Some(vec![2]);
    let got = rows_of(&mut s, &req);
    assert!(!got.is_empty());
    assert_eq!(got, all.into_iter().filter(|r| r.0 == 2).collect::<Vec<_>>());
}

/// A pass over a hot relation reads and reduces a batch of instruments at a
/// time (`set_batch_rows`), so that it holds one batch's rows and not the
/// relation's. Whatever the batches, the answer is the one of the relation
/// read whole, column for column and bit for bit, with its inexact rows:
/// rows and bucket windows, the three numerics, a product of two relations
/// (the second holds an instrument the first has no row of), and a request
/// scoped to instruments.
#[test]
fn a_pass_in_batches_of_instruments_is_the_pass_of_the_relation_read_whole() {
    let close: Vec<Fact> = data_cents().into_iter().map(|(a, t, p)| (a, t, t, Some(p))).collect();
    let mut volume: Vec<Fact> = close
        .iter()
        .filter(|r| (r.0, r.1) != (2, T0 + 7 * DAY))
        .map(|&(a, t, r, _)| (a, t, r, Some((1_000 + (t / DAY + a as i64 * 37) % 5_000) as f64)))
        .collect();
    // A late receipt, after its week's barrier, and an instrument the
    // close has no row of.
    volume.iter_mut().filter(|r| (r.0, r.1) == (1, T0 + 2 * DAY)).for_each(|r| r.2 = T0 + 6 * DAY);
    volume.extend((0..10).map(|d| (4, T0 + d * DAY, T0 + d * DAY, Some(7.0 + d as f64))));
    volume.sort_by_key(|r| (r.1, r.0));
    let mut s = store_of(&[("equity.close", "P", &close), ("equity.volume", "V", &volume)], true);
    for r in ["equity.close", "equity.volume"] {
        assert!(s.is_hot(s.registry.current(r, Some("test")).unwrap()), "{}", r);
    }
    let product = |mut req: AggregateRequest| {
        req.factor = Some(Box::new(Factor {
            relation: "equity.volume".into(),
            column: "V".into(),
        }));
        req
    };
    let weekly = |function: &str| {
        let mut req = request(function, WindowKind::Bucket);
        req.bucket = Some("1w".into());
        req
    };
    let last = |function: &str, n: u64| {
        let mut req = request(function, WindowKind::Rows);
        req.n = n;
        req
    };
    let mut requests = vec![last("mean", 3), weekly("sum"), product(weekly("mean")), product(last("mean", 3))];
    let mut running = last("std", 5);
    running.numerics = Numerics::Running;
    let mut exact = weekly("sum");
    exact.numerics = Numerics::Exact;
    let mut scoped = last("mean", 3);
    scoped.entities = Some(vec![3, 1]);
    let mut scoped_product = product(weekly("mean"));
    scoped_product.entities = Some(vec![4, 2]);
    requests.extend([running, exact, scoped, scoped_product]);
    // 0 reads the relation whole; 1 makes every instrument a batch; 45
    // puts the first two (40 rows each) in one and the third in another.
    let sizes = [0u64, 1, 45, 1 << 20];
    let mut answers = Vec::new();
    for size in sizes {
        set_batch_rows(size);
        let mut got = Vec::new();
        for (k, req) in requests.iter().enumerate() {
            let mut req = req.clone();
            // A name of its own for each size: no cache hit.
            req.output = Some(format!("batch_{}_{}", size, k));
            let a = aggregate(&mut s, &ResolvedView::default(), &req).unwrap();
            let b = answer(&s, &a.version).unwrap();
            assert!(b.num_rows() > 0, "request {}", k);
            got.push((b.columns().to_vec(), a.inexact_rows));
        }
        answers.push(got);
    }
    set_batch_rows(1 << 20);
    for (size, got) in sizes.iter().zip(&answers).skip(1) {
        for (k, (mine, whole)) in got.iter().zip(&answers[0]).enumerate() {
            assert_eq!(mine.1, whole.1, "the inexact rows of request {} in batches of {} rows", k, size);
            assert!(mine.0 == whole.0, "request {} in batches of {} rows", k, size);
        }
    }
    // The late volume is an inexact row of the weekly product, in batches
    // as whole.
    assert!(answers[0][2].1 >= 1, "{}", answers[0][2].1);
}

/// An answer's inexact rows: a row received after the barrier of the first
/// label whose window holds it, a null, and an answer row whose arithmetic
/// failed; a row received at the barrier's instant is on time, and a
/// request naming no calendar has no barrier to prove any row by.
#[test]
fn inexact_rows_counts_rows_received_after_their_barrier_and_nulls() {
    let on_time: Vec<Fact> = (0..10)
        .flat_map(|d| (1..=2u64).map(move |a| (a, T0 + d * DAY, T0 + d * DAY, Some(10.0 * a as f64 + d as f64))))
        .collect();
    let mut bucket = request("mean", WindowKind::Bucket);
    bucket.bucket = Some("1w".into());
    let mut daily_rows = request("mean", WindowKind::Rows);
    daily_rows.n = 2;
    let mut s = store_of(&[("equity.close", "P", &on_time)], false);
    for req in [&bucket, &daily_rows] {
        assert_eq!(aggregate(&mut s, &ResolvedView::default(), req).unwrap().inexact_rows, 0);
    }
    // The barriers are at a label's last instant: day 2's row received on
    // day 6 is after its week's (day 4's) and its day's; day 7's received on
    // day 10 after day 9's; day 9's received at day 9's barrier is on time;
    // day 8's value is null.
    let late: Vec<Fact> = on_time
        .iter()
        .map(|&(a, t, r, x)| match (a, (t - T0) / DAY) {
            (1, 2) => (a, t, T0 + 6 * DAY, x),
            (2, 7) => (a, t, T0 + 10 * DAY, x),
            (2, 9) => (a, t, T0 + 10 * DAY - 1, x),
            (1, 8) => (a, t, r, None),
            _ => (a, t, r, x),
        })
        .collect();
    let mut s = store_of(&[("equity.close", "P", &late)], false);
    for req in [&bucket, &daily_rows] {
        assert_eq!(aggregate(&mut s, &ResolvedView::default(), req).unwrap().inexact_rows, 3, "{:?}", req.window);
    }
    // The late row's week waits on it: the answer's receipt is day 6.
    let a = aggregate(&mut s, &ResolvedView::default(), &bucket).unwrap();
    let b = answer(&s, &a.version).unwrap();
    let (e, l, r) = (b.column_by_name("entity").unwrap(), b.column_by_name("label").unwrap(), b.column_by_name("r").unwrap());
    let at = (0..b.num_rows())
        .find(|&i| e.as_primitive::<arrow::datatypes::UInt64Type>().value(i) == 1 && l.as_primitive::<arrow::datatypes::TimestampNanosecondType>().value(i) == T0 + 4 * DAY)
        .unwrap();
    assert_eq!(r.as_primitive::<arrow::datatypes::TimestampNanosecondType>().value(at), T0 + 6 * DAY);
    // No calendar: every row inexact.
    let mut bare = daily_rows.clone();
    bare.calendar = None;
    let s_rows = on_time.len() as u64;
    let mut s = store_of(&[("equity.close", "P", &on_time)], false);
    assert_eq!(aggregate(&mut s, &ResolvedView::default(), &bare).unwrap().inexact_rows, s_rows);
    // The correlation of a constant series fails at every answer row.
    let flat: Vec<Fact> = (0..6).map(|d| (1, T0 + d * DAY, T0 + d * DAY, Some(5.0))).collect();
    let mut s = store_of(&[("equity.close", "P", &flat)], false);
    let mut corr = request("corr", WindowKind::Rows);
    corr.second_column = Some("P".into());
    corr.n = 3;
    let a = aggregate(&mut s, &ResolvedView::default(), &corr).unwrap();
    assert_eq!((a.rows, a.inexact_rows), (4, 4));
}
