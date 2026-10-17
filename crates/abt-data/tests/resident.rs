//! The resident cache (D-3.2, D-3.4; ERRATA E-74): a read through it is a
//! read from the file, row for row, under any budget. Repeated reads are
//! hits, and the cache stays within its budget by evicting.

use std::collections::BTreeMap;
use std::sync::Arc;

use abt_data::rows::SEQ;
use abt_data::{RowFilter, Store};
use arrow::array::{AsArray, Float64Array, RecordBatch, TimestampNanosecondArray, UInt64Array};
use arrow::datatypes::{DataType, Field, Schema, TimeUnit};

const DAY: i64 = 86_400 * 1_000_000_000;

fn close_rows(rows: &[(u64, i64, i64, f64)]) -> RecordBatch {
    let ts = DataType::Timestamp(TimeUnit::Nanosecond, None);
    let schema = Arc::new(Schema::new(vec![
        Field::new("A", DataType::UInt64, false),
        Field::new("T", ts.clone(), false),
        Field::new("P", DataType::Float64, false),
        Field::new("r", ts, false),
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

/// (entity, t, r, price bits, seq) of every row read, in order.
fn rows_of(batches: &[RecordBatch]) -> Vec<(u64, i64, i64, u64, u64)> {
    let mut out = Vec::new();
    for b in batches {
        let a = b.column(0).as_primitive::<arrow::datatypes::UInt64Type>();
        let t = b.column(1).as_primitive::<arrow::datatypes::TimestampNanosecondType>();
        let p = b.column(2).as_primitive::<arrow::datatypes::Float64Type>();
        let r = b.column(3).as_primitive::<arrow::datatypes::TimestampNanosecondType>();
        let s = b.column(b.schema().index_of(SEQ).unwrap()).as_primitive::<arrow::datatypes::UInt64Type>();
        for i in 0..b.num_rows() {
            out.push((a.value(i), t.value(i), r.value(i), p.value(i).to_bits(), s.value(i)));
        }
    }
    out
}

/// 3,300 names over the weekdays of two months: about 70,000 rows a
/// month, two row groups in each month's partition, with a revision of
/// some rows a day later.
fn market() -> (Store, String) {
    let mut s = Store::temporary("resident").unwrap();
    let start = 19_723 * DAY; // 2024-01-01
    let mut rows = Vec::new();
    let mut x = 0x9E37_79B9_7F4A_7C15u64;
    for d in 0..60i64 {
        let t = start + d * DAY;
        if matches!((19_723 + d + 3).rem_euclid(7), 5 | 6) {
            continue;
        }
        for e in 1..=3_300u64 {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            let p = 10.0 + (x % 100_000) as f64 / 100.0;
            rows.push((e, t, t, p));
            if x.is_multiple_of(97) {
                rows.push((e, t, t + DAY, p + 1.0));
            }
        }
    }
    let v = s.ingest("equity.close", &[close_rows(&rows)], "test", BTreeMap::new()).unwrap();
    (s, v)
}

fn filters() -> Vec<RowFilter> {
    let start = 19_723 * DAY;
    let mut out = vec![RowFilter::default()];
    let mut x = 0x2545_F491_4F6C_DD1Du64;
    let mut next = move |n: u64| {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        x % n
    };
    for _ in 0..24 {
        out.push(RowFilter {
            t0: (next(2) == 0).then(|| start + next(60) as i64 * DAY),
            t1: (next(2) == 0).then(|| start + next(60) as i64 * DAY),
            // Names at both ends of the id range: rows in both row groups.
            identity: (next(3) != 0).then(|| (0..1 + next(5)).map(|_| 1 + next(3_000)).collect()),
            r_max: (next(3) == 0).then(|| start + next(61) as i64 * DAY),
            as_of_seq: None,
            latest_only: next(2) == 0,
        });
    }
    out
}

#[test]
fn a_read_through_the_resident_cache_is_a_read_from_the_file_under_any_budget() {
    let (mut s, v) = market();
    let parts = s.manifest(&v).unwrap().partitions;
    assert!(
        parts.iter().all(|p| p.rows > 65_536),
        "every month in two row groups: {:?}",
        parts.iter().map(|p| p.rows).collect::<Vec<_>>()
    );
    let fs = filters();
    s.set_resident_budget(0);
    let from_files: Vec<_> = fs.iter().map(|f| rows_of(&s.read(&v, f).unwrap())).collect();
    assert_eq!(s.resident_stats().groups, 0);
    // A budget for everything: the first pass decodes, the second hits.
    s.set_resident_budget(1 << 30);
    for _ in 0..2 {
        for (f, want) in fs.iter().zip(&from_files) {
            assert_eq!(&rows_of(&s.read(&v, f).unwrap()), want, "{:?}", f);
        }
    }
    let st = s.resident_stats();
    assert_eq!((st.misses, st.groups, st.evictions), (4, 4, 0), "{:?}", st);
    assert!(st.hits > 0 && st.bytes <= 1 << 30, "{:?}", st);
    // A budget for one group and a half: every read evicts, and reads the same.
    let one = st.bytes / 4;
    s.set_resident_budget(one + one / 2);
    for (f, want) in fs.iter().zip(&from_files) {
        assert_eq!(&rows_of(&s.read(&v, f).unwrap()), want, "{:?}", f);
    }
    let st = s.resident_stats();
    assert!(st.evictions > 0 && st.groups <= 1 && st.bytes <= one + one / 2, "{:?}", st);
}

#[test]
fn an_entity_read_gathers_its_rows_in_the_files_order() {
    let (mut s, v) = market();
    let f = RowFilter {
        identity: Some(vec![2_999, 1, 1_500]),
        ..RowFilter::default()
    };
    s.set_resident_budget(0);
    let want = rows_of(&s.read(&v, &f).unwrap());
    assert!(want.iter().any(|r| r.0 == 1) && want.iter().any(|r| r.0 == 2_999));
    s.set_resident_budget(1 << 30);
    assert_eq!(rows_of(&s.read(&v, &f).unwrap()), want);
    // The rows are in the partition's (t, entity, r) order.
    let mut sorted = want.clone();
    sorted.sort_by_key(|r| (r.1, r.0, r.2));
    assert_eq!(want, sorted);
}
