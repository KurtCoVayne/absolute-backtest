//! The store (D-2, D-3): the class files render from the registry; rows
//! ingest into content-addressed versions; revisions are kept and the
//! receipt filter picks the latest admitted one; snapshot reads see exactly
//! the rows with seq ≤ N; duplicates are dropped and conflicts refused; the
//! entity index answers what a full read answers; pins survive gc; and a
//! model-based test checks every read against a naive store over random
//! interleavings of ingests and reads.

use std::collections::BTreeMap;
use std::sync::Arc;

use abt_data::catalog::{classes, render_class};
use abt_data::rows::SEQ;
use abt_data::{RowFilter, Store};
use abt_wire::generated::abt::wire::{ColumnSpec, ColumnType, RelationSchema, Role};
use abt_wire::hash::Hash;
use arrow::array::{AsArray, Float64Array, RecordBatch, StringArray, TimestampNanosecondArray, UInt64Array};
use arrow::datatypes::{DataType, Field, Schema, TimeUnit};

const DAY: i64 = 86_400 * 1_000_000_000;

fn repo() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

#[test]
fn the_class_files_render_from_the_registry() {
    for c in classes() {
        let path = repo().join("corpus-v2/catalog").join(format!("{}.dsl", c.name));
        let want = std::fs::read_to_string(&path).unwrap();
        assert_eq!(render_class(&c), want, "{} does not render as {}", c.name, path.display());
    }
}

/// Rows of `close(A, T, P)` with a receipt time.
fn close_rows(rows: &[(u64, i64, i64, f64)]) -> RecordBatch {
    let schema = Arc::new(Schema::new(vec![
        Field::new("A", DataType::UInt64, false),
        Field::new("T", DataType::Timestamp(TimeUnit::Nanosecond, None), false),
        Field::new("P", DataType::Float64, false),
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

/// (entity, t, r, price, seq) of every row read.
fn rows_of(batches: &[RecordBatch]) -> Vec<(u64, i64, i64, f64, u64)> {
    let mut out = Vec::new();
    for b in batches {
        let a = b.column(0).as_primitive::<arrow::datatypes::UInt64Type>();
        let t = b.column(1).as_primitive::<arrow::datatypes::TimestampNanosecondType>();
        let p = b.column(2).as_primitive::<arrow::datatypes::Float64Type>();
        let r = b.column(3).as_primitive::<arrow::datatypes::TimestampNanosecondType>();
        let s = b.column(b.schema().index_of(SEQ).unwrap()).as_primitive::<arrow::datatypes::UInt64Type>();
        for i in 0..b.num_rows() {
            out.push((a.value(i), t.value(i), r.value(i), p.value(i), s.value(i)));
        }
    }
    out
}

#[test]
fn revisions_are_kept_and_the_receipt_filter_picks_the_latest_admitted() {
    let mut s = Store::temporary("revisions").unwrap();
    let t = 19_000 * DAY;
    // A close at t, received at t, then corrected a day later.
    let v1 = s.ingest("equity.close", &[close_rows(&[(1, t, t, 10.0), (2, t, t, 20.0)])], "test", BTreeMap::new()).unwrap();
    let v2 = s.ingest("equity.close", &[close_rows(&[(1, t, t + DAY, 10.5)])], "test", BTreeMap::new()).unwrap();
    assert_ne!(v1, v2, "new rows make a new version");
    let all = rows_of(&s.read(&v2, &RowFilter::default()).unwrap());
    assert_eq!(all.len(), 3, "both revisions are kept: {:?}", all);
    // Latest overall: the correction.
    let latest = rows_of(
        &s.read(
            &v2,
            &RowFilter {
                latest_only: true,
                ..RowFilter::default()
            },
        )
        .unwrap(),
    );
    assert_eq!(latest.iter().map(|r| (r.0, r.3)).collect::<Vec<_>>(), vec![(1, 10.5), (2, 20.0)]);
    // As received by t: the original.
    let then = rows_of(
        &s.read(
            &v2,
            &RowFilter {
                latest_only: true,
                r_max: Some(t),
                ..RowFilter::default()
            },
        )
        .unwrap(),
    );
    assert_eq!(then.iter().map(|r| (r.0, r.3)).collect::<Vec<_>>(), vec![(1, 10.0), (2, 20.0)]);
    // The first version still reads as it did (D-2.6: immutable).
    assert_eq!(rows_of(&s.read(&v1, &RowFilter::default()).unwrap()).len(), 2);
    // Snapshot reads: the correction has the third sequence number.
    let snap = rows_of(
        &s.read(
            &v2,
            &RowFilter {
                as_of_seq: Some(2),
                ..RowFilter::default()
            },
        )
        .unwrap(),
    );
    assert_eq!(snap.len(), 2);
    assert!(snap.iter().all(|r| r.4 <= 2));
}

#[test]
fn duplicates_are_dropped_conflicts_refused_and_versions_content_addressed() {
    let mut a = Store::temporary("dup-a").unwrap();
    let mut b = Store::temporary("dup-b").unwrap();
    let t = 19_000 * DAY;
    let rows = close_rows(&[(1, t, t, 10.0), (1, t + DAY, t + DAY, 11.0), (2, t, t, 20.0)]);
    let va = a.ingest("equity.close", std::slice::from_ref(&rows), "x", BTreeMap::new()).unwrap();
    // The same rows in the same order, in another store: the same version.
    // The partitions keep each row's sequence number (D-3.3), so the order
    // the source delivered them in is part of the version.
    let vb = b.ingest("equity.close", std::slice::from_ref(&rows), "x", BTreeMap::new()).unwrap();
    assert_eq!(va, vb, "a version is a function of its rows and their order of delivery");
    let mut c = Store::temporary("dup-c").unwrap();
    let reordered = close_rows(&[(2, t, t, 20.0), (1, t + DAY, t + DAY, 11.0), (1, t, t, 10.0)]);
    let vc = c.ingest("equity.close", &[reordered], "x", BTreeMap::new()).unwrap();
    assert_ne!(va, vc, "another order of delivery is another log");
    // The same fact again: dropped, the version unchanged.
    let again = a.ingest("equity.close", &[close_rows(&[(1, t, t, 10.0)])], "x", BTreeMap::new()).unwrap();
    assert_eq!(again, va);
    // A different value at the same identity, time and receipt: refused.
    let err = a.ingest("equity.close", &[close_rows(&[(1, t, t, 99.0)])], "x", BTreeMap::new()).unwrap_err();
    assert!(err.to_string().contains("differ"), "{}", err);
    // An unknown relation is refused by name.
    assert!(a.ingest("nope", &[rows], "x", BTreeMap::new()).unwrap_err().to_string().contains("nope"));
}

#[test]
fn unchanged_buckets_keep_their_files_and_the_index_answers_as_a_full_read() {
    let mut s = Store::temporary("buckets").unwrap();
    let start = 19_000 * DAY;
    let mut rows = Vec::new();
    for d in 0..90 {
        for e in 1..=20u64 {
            let t = start + d * DAY;
            rows.push((e, t, t, 100.0 + e as f64 + d as f64 / 10.0));
        }
    }
    let v1 = s.ingest("equity.close", &[close_rows(&rows)], "x", BTreeMap::new()).unwrap();
    let m1 = s.manifest(&v1).unwrap();
    assert!(m1.partitions.len() >= 3, "monthly buckets for a daily relation");
    let later = start + 200 * DAY;
    let v2 = s.ingest("equity.close", &[close_rows(&[(1, later, later, 1.0)])], "x", BTreeMap::new()).unwrap();
    let m2 = s.manifest(&v2).unwrap();
    for p in &m1.partitions {
        assert!(m2.partitions.contains(p), "bucket {} rewritten", p.bucket);
    }
    // Reading three entities through the index equals filtering a full read.
    let ids = vec![3u64, 7, 20];
    let indexed = rows_of(
        &s.read(
            &v2,
            &RowFilter {
                identity: Some(ids.clone()),
                ..RowFilter::default()
            },
        )
        .unwrap(),
    );
    let full: Vec<_> = rows_of(&s.read(&v2, &RowFilter::default()).unwrap()).into_iter().filter(|r| ids.contains(&r.0)).collect();
    assert_eq!(indexed, full);
    assert_eq!(indexed.len(), 3 * 90);
    // The log holds every row in acceptance order with gap-free seq.
    let log = s.read_log("equity.close", "x", 1, u64::MAX).unwrap();
    let seqs: Vec<u64> = log
        .iter()
        .flat_map(|b| b.column(b.num_columns() - 1).as_primitive::<arrow::datatypes::UInt64Type>().values().to_vec())
        .collect();
    assert_eq!(seqs, (1..=seqs.len() as u64).collect::<Vec<_>>());
}

#[test]
fn pinned_versions_survive_gc_and_the_cache_returns_what_was_put() {
    let mut s = Store::temporary("gc").unwrap();
    let t = 19_000 * DAY;
    let v1 = s.ingest("equity.close", &[close_rows(&[(1, t, t, 1.0)])], "x", BTreeMap::new()).unwrap();
    let _v2 = s.ingest("equity.close", &[close_rows(&[(1, t + 40 * DAY, t + 40 * DAY, 2.0)])], "x", BTreeMap::new()).unwrap();
    let v3 = s.ingest("equity.close", &[close_rows(&[(1, t + 80 * DAY, t + 80 * DAY, 3.0)])], "x", BTreeMap::new()).unwrap();
    s.pin(&v1, "plan:test").unwrap();
    let evicted = s.gc(0).unwrap();
    assert_eq!(evicted.len(), 1, "only the unpinned, non-current version goes");
    assert!(s.manifest(&v1).is_ok() && s.manifest(&v3).is_ok());
    assert_eq!(rows_of(&s.read(&v1, &RowFilter::default()).unwrap()).len(), 1, "a pinned version still reads");
    // A cache object by key.
    let key = Hash::of(b"bar view weeks(1) of close@v3");
    let schema = s.schema("equity.close").unwrap();
    let v = s.cache_put(&key, &schema, &[close_rows(&[(9, t, t, 9.0)])]).unwrap();
    assert_eq!(s.cache_get(&key), Some(v));
    assert_eq!(s.cache_get(&Hash::of(b"other")), None);
}

/// A writer's relation (D-3.6): a `put` of its schema again adds to it; a
/// `put` whose columns changed starts it over with that write's rows, and
/// the earlier version still reads by its id.
#[test]
fn a_put_of_a_new_shape_starts_the_relation_over() {
    let mut s = Store::temporary("reshape").unwrap();
    let t = 19_000 * DAY;
    let col = |name: &str, ty: ColumnType, role: Role| ColumnSpec {
        name: name.into(),
        ty,
        role,
        dimension: None,
    };
    let schema = |cols: Vec<ColumnSpec>| RelationSchema {
        name: "kernel.r1.close".into(),
        class: "kernel".into(),
        columns: cols,
        calendar: None,
        constraints: None,
        attributes: None,
        schema_version: Some("1.0".into()),
    };
    let wide = schema(vec![
        col("A", ColumnType::Entity, Role::Entity),
        col("T", ColumnType::Time, Role::EventTime),
        col("P", ColumnType::Num, Role::Value),
        col("r", ColumnType::Time, Role::ReceiptTime),
    ]);
    let (v1, _) = s.put(&wide, &[close_rows(&[(1, t, t, 1.0)])], "kernel").unwrap();
    let (v2, _) = s.put(&wide, &[close_rows(&[(2, t, t, 2.0)])], "kernel").unwrap();
    assert_eq!(rows_of(&s.read(&v2, &RowFilter::default()).unwrap()).len(), 2, "the same shape adds to the relation");
    // A put that replaces starts the relation over from its rows (E-56).
    let (vr, _) = s.replace(&wide, &[close_rows(&[(2, t, t, 5.0)])], "kernel").unwrap();
    assert_eq!(rows_of(&s.read(&vr, &RowFilter::default()).unwrap()).len(), 1, "a replacing put holds its own rows only");
    assert_eq!(rows_of(&s.read(&v2, &RowFilter::default()).unwrap()).len(), 2, "the version it replaced still reads");
    let (v2, _) = s.put(&wide, &[close_rows(&[(1, t, t, 1.0)])], "kernel").unwrap();
    assert_eq!(rows_of(&s.read(&v2, &RowFilter::default()).unwrap()).len(), 2);
    let narrow = schema(vec![
        col("A", ColumnType::Entity, Role::Entity),
        col("T", ColumnType::Time, Role::EventTime),
        col("r", ColumnType::Time, Role::ReceiptTime),
    ]);
    let rows = RecordBatch::try_new(
        Arc::new(Schema::new(vec![
            Field::new("A", DataType::UInt64, false),
            Field::new("T", DataType::Timestamp(TimeUnit::Nanosecond, None), false),
            Field::new("r", DataType::Timestamp(TimeUnit::Nanosecond, None), false),
        ])),
        vec![
            Arc::new(UInt64Array::from(vec![3u64])),
            Arc::new(TimestampNanosecondArray::from(vec![t])),
            Arc::new(TimestampNanosecondArray::from(vec![t])),
        ],
    )
    .unwrap();
    let (v3, _) = s.put(&narrow, &[rows], "kernel").unwrap();
    let got = s.read(&v3, &RowFilter::default()).unwrap();
    assert_eq!(got.iter().map(|b| b.num_rows()).sum::<usize>(), 1, "a new shape holds its own rows only");
    assert_eq!(s.manifest(&v3).unwrap().schema.columns.len(), 3);
    assert_eq!(rows_of(&s.read(&v1, &RowFilter::default()).unwrap()).len(), 1, "the earlier version still reads");
}

#[test]
fn labels_are_interned_once_with_stable_codes() {
    let mut s = Store::temporary("labels").unwrap();
    let schema = Arc::new(Schema::new(vec![
        Field::new("A", DataType::UInt64, false),
        Field::new("T", DataType::Timestamp(TimeUnit::Nanosecond, None), false),
        Field::new("List", DataType::Utf8, false),
    ]));
    let t = 19_000 * DAY;
    let b = RecordBatch::try_new(
        schema,
        vec![
            Arc::new(UInt64Array::from(vec![1, 2, 1])),
            Arc::new(TimestampNanosecondArray::from(vec![t, t, t + DAY])),
            Arc::new(StringArray::from(vec!["SP500", "SP400", "SP500"])),
        ],
    )
    .unwrap();
    let v = s.ingest("equity.member", &[b], "x", BTreeMap::new()).unwrap();
    let code = s.registry.label_code("SP500").unwrap();
    assert_eq!(s.registry.label("SP500"), code, "interning again gives the same code");
    let rows = s.read_all(&v).unwrap();
    // The receipt time defaulted to the event time.
    let r = rows.column(3).as_primitive::<arrow::datatypes::TimestampNanosecondType>();
    let t_col = rows.column(1).as_primitive::<arrow::datatypes::TimestampNanosecondType>();
    assert!((0..rows.num_rows()).all(|i| r.value(i) == t_col.value(i)));
    // An answer decodes labels through the dictionary.
    let ans = abt_data::rows::answer(&s.schema("equity.member").unwrap(), &rows, s.registry.labels(), false).unwrap();
    let dict = ans.column(2).as_dictionary::<arrow::datatypes::UInt32Type>();
    let values = dict.values().as_string::<i32>();
    let texts: Vec<&str> = (0..ans.num_rows()).map(|i| values.value(dict.keys().value(i) as usize)).collect();
    assert_eq!(texts.len(), 3);
    assert!(texts.contains(&"SP400"));
}

/// A naive store: every row ever ingested, filtered by brute force.
#[derive(Default)]
struct Naive {
    rows: Vec<(u64, i64, i64, f64, u64)>,
}

impl Naive {
    fn read(&self, f: &RowFilter) -> Vec<(u64, i64, i64, f64)> {
        let mut v: Vec<_> = self
            .rows
            .iter()
            .filter(|r| {
                f.t0.is_none_or(|t0| r.1 >= t0)
                    && f.t1.is_none_or(|t1| r.1 <= t1)
                    && f.r_max.is_none_or(|m| r.2 <= m)
                    && f.as_of_seq.is_none_or(|n| r.4 <= n)
                    && f.identity.as_ref().is_none_or(|ids| ids.contains(&r.0))
            })
            .cloned()
            .collect();
        v.sort_by_key(|a| (a.1, a.0, a.2, a.4));
        if f.latest_only {
            let mut out: Vec<(u64, i64, i64, f64, u64)> = Vec::new();
            for r in v {
                if let Some(last) = out.last_mut() {
                    if last.0 == r.0 && last.1 == r.1 {
                        *last = r;
                        continue;
                    }
                }
                out.push(r);
            }
            v = out;
        }
        v.into_iter().map(|r| (r.0, r.1, r.2, r.3)).collect()
    }
}

#[test]
fn reads_equal_a_naive_store_over_random_interleavings() {
    let mut rng = 0x2545F4914F6CDD1Du64;
    let mut next = move |n: u64| {
        rng ^= rng << 13;
        rng ^= rng >> 7;
        rng ^= rng << 17;
        rng % n
    };
    let mut s = Store::temporary("model").unwrap();
    let mut naive = Naive::default();
    let mut seq = 0u64;
    let start = 19_000 * DAY;
    let mut version = None;
    for round in 0..12 {
        // An ingest of new facts and revisions of old ones.
        let mut batch = Vec::new();
        for _ in 0..(5 + next(40)) {
            let e = 1 + next(6);
            let d = next(120) as i64;
            let t = start + d * DAY;
            let lag = next(5) as i64 * DAY;
            let price = (next(10_000) as f64) / 100.0;
            let r = t + lag;
            // Within one ingest, one row per (e, t, r) (else a conflict).
            if batch.iter().any(|b: &(u64, i64, i64, f64)| b.0 == e && b.1 == t && b.2 == r) || naive.rows.iter().any(|n| n.0 == e && n.1 == t && n.2 == r) {
                continue;
            }
            batch.push((e, t, r, price));
        }
        let rows: Vec<(u64, i64, i64, f64)> = batch.clone();
        version = Some(s.ingest("equity.close", &[close_rows(&rows)], "model", BTreeMap::new()).unwrap());
        for (e, t, r, p) in batch {
            seq += 1;
            naive.rows.push((e, t, r, p, seq));
        }
        // Reads under random filters.
        for _ in 0..10 {
            let f = RowFilter {
                t0: (next(2) == 0).then(|| start + next(120) as i64 * DAY),
                t1: (next(2) == 0).then(|| start + next(120) as i64 * DAY),
                identity: (next(2) == 0).then(|| vec![1 + next(6), 1 + next(6)]),
                r_max: (next(2) == 0).then(|| start + next(125) as i64 * DAY),
                as_of_seq: (next(3) == 0).then(|| next(seq + 1)),
                latest_only: next(2) == 0,
            };
            let got: Vec<(u64, i64, i64, f64)> = rows_of(&s.read(version.as_ref().unwrap(), &f).unwrap()).into_iter().map(|r| (r.0, r.1, r.2, r.3)).collect();
            assert_eq!(got, naive.read(&f), "round {} filter {:?}", round, f);
        }
    }
    let _ = version.map(|v| s.manifest(&v).unwrap().partitions.iter().map(|p| p.rows).sum::<u64>());
    let _: Vec<ColumnSpec> = vec![ColumnSpec {
        name: "x".into(),
        ty: ColumnType::Num,
        role: Role::Value,
        dimension: None,
    }];
    let _ = RelationSchema::default;
}

/// A temporary store left by a process that ended is removed; one of a
/// running process is kept.
#[test]
fn the_temporary_stores_of_ended_processes_are_reaped() {
    let tmp = std::env::temp_dir();
    let dead = tmp.join("abt-store-reap-4294967295-0");
    let alive = tmp.join(format!("abt-store-reap-{}-999", std::process::id()));
    for d in [&dead, &alive] {
        std::fs::create_dir_all(d).unwrap();
    }
    abt_data::store::reap_temporary();
    if cfg!(unix) {
        assert!(!dead.exists(), "the store of an ended process is removed");
    }
    let _ = std::fs::remove_dir_all(&dead);
    assert!(alive.exists(), "a running process's store is kept");
    let _ = std::fs::remove_dir_all(&alive);
}
