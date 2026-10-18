//! The query forms (D-4.1) on a small store, each against what its rows
//! say: point, latest, series, section, scan with a predicate and a
//! projection, exists, count, distinct, catalog over the registry, the
//! series class keyed by a label (E-16), the receipt filter (D-4.2), reads
//! at a sequence number, views through a binding, and explain.

use std::collections::BTreeMap;
use std::sync::Arc;

use abt_data::query::{execute, explain, ResolvedView};
use abt_data::Store;
use abt_wire::generated::abt::wire::{
    Atom, Binding, Catalog, Chain, Constant, ConstantKind, CountRows, Disjunction, Distinct, Exists, Identity, Latest, Op, Point, Predicate, QueryForm, Scan, Section, Series, VersionRef, View,
};
use arrow::array::{Array, AsArray, Float64Array, RecordBatch, StringArray, TimestampNanosecondArray, UInt64Array};
use arrow::datatypes::{DataType, Field, Schema, TimeUnit};

const DAY: i64 = 86_400 * 1_000_000_000;
const T0: i64 = 19_000 * DAY;

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

/// Three instruments over five days, received at the bar; instrument 1's
/// third close corrected a day later.
fn store() -> (Store, String, String) {
    let mut s = Store::temporary("query").unwrap();
    let mut rows = Vec::new();
    for d in 0..5 {
        for a in 1..=3u64 {
            rows.push((a, T0 + d * DAY, T0 + d * DAY, 10.0 * a as f64 + d as f64));
        }
    }
    let v1 = s.ingest("equity.close", &[close_rows(&rows)], "test", BTreeMap::new()).unwrap();
    let v2 = s.ingest("equity.close", &[close_rows(&[(1, T0 + 2 * DAY, T0 + 3 * DAY, 99.0)])], "test", BTreeMap::new()).unwrap();
    (s, v1, v2)
}

fn entity(id: u64) -> Identity {
    Identity {
        values: vec![Constant {
            kind: ConstantKind::Entity,
            num: 0.0,
            int: id as i64,
            text: None,
        }],
    }
}

fn num(x: f64) -> Constant {
    Constant {
        kind: ConstantKind::Num,
        num: x,
        int: 0,
        text: None,
    }
}

fn ent(id: u64) -> Constant {
    Constant {
        kind: ConstantKind::Entity,
        num: 0.0,
        int: id as i64,
        text: None,
    }
}

fn label(s: &str) -> Constant {
    Constant {
        kind: ConstantKind::Label,
        num: 0.0,
        int: 0,
        text: Some(s.to_string()),
    }
}

fn atom(column: &str, op: Op, values: Vec<Constant>) -> Atom {
    Atom {
        column: column.to_string(),
        op,
        values: Some(values),
    }
}

/// A conjunction of disjunctions.
fn pred(all: Vec<Vec<Atom>>) -> Option<Box<Predicate>> {
    Some(Box::new(Predicate {
        all: Some(all.into_iter().map(|atoms| Disjunction { atoms }).collect()),
    }))
}

/// (A, T, P) of every row of an answer.
fn closes(batches: &[RecordBatch]) -> Vec<(u64, i64, f64)> {
    let mut out = Vec::new();
    for b in batches {
        let a = b.column(0).as_primitive::<arrow::datatypes::UInt64Type>();
        let t = b.column(1).as_primitive::<arrow::datatypes::TimestampNanosecondType>();
        let p = b.column(2).as_primitive::<arrow::datatypes::Float64Type>();
        for i in 0..b.num_rows() {
            out.push((a.value(i), t.value(i), p.value(i)));
        }
    }
    out
}

fn run(s: &Store, form: QueryForm, r_max: Option<i64>) -> Vec<(u64, i64, f64)> {
    let (_, b) = execute(s, &ResolvedView::default(), &form, r_max, false, false).unwrap();
    closes(&b)
}

#[test]
fn point_answers_the_latest_admitted_revision_or_nothing() {
    let (s, _, _) = store();
    let at = |t: i64, r: Option<i64>| {
        run(
            &s,
            QueryForm::Point(Box::new(Point {
                relation: "equity.close".into(),
                identity: Some(Box::new(entity(1))),
                t,
            })),
            r,
        )
    };
    assert_eq!(at(T0 + 2 * DAY, None), vec![(1, T0 + 2 * DAY, 99.0)], "the correction");
    assert_eq!(at(T0 + 2 * DAY, Some(T0 + 2 * DAY)), vec![(1, T0 + 2 * DAY, 12.0)], "as received at the bar");
    assert_eq!(at(T0 + 7 * DAY, None), vec![], "no row");
    assert_eq!(at(T0 + 2 * DAY, Some(T0 + DAY)), vec![], "nothing received yet");
}

#[test]
fn latest_is_the_as_of_read() {
    let (s, _, _) = store();
    let latest = |a: u64, t_max: i64, r: Option<i64>| {
        run(
            &s,
            QueryForm::Latest(Box::new(Latest {
                relation: "equity.close".into(),
                identity: Some(Box::new(entity(a))),
                t_max,
            })),
            r,
        )
    };
    assert_eq!(latest(2, T0 + 3 * DAY + 5, None), vec![(2, T0 + 3 * DAY, 23.0)]);
    // Between the bars: the earlier bar.
    assert_eq!(latest(1, T0 + 2 * DAY + DAY / 2, None), vec![(1, T0 + 2 * DAY, 99.0)]);
    assert_eq!(latest(1, T0 + 2 * DAY + DAY / 2, Some(T0 + 2 * DAY + DAY / 2)), vec![(1, T0 + 2 * DAY, 12.0)]);
    // With no identity, every entity's latest row, in (t, entity) order.
    let all = run(
        &s,
        QueryForm::Latest(Box::new(Latest {
            relation: "equity.close".into(),
            identity: None,
            t_max: T0 + DAY,
        })),
        None,
    );
    assert_eq!(all, vec![(1, T0 + DAY, 11.0), (2, T0 + DAY, 21.0), (3, T0 + DAY, 31.0)]);
}

/// `latest` of one entity reads back from `t_max` bucket by bucket: past a
/// month the entity is absent from, and past a month whose row the receipt
/// filter does not admit yet.
#[test]
fn latest_reads_back_across_buckets() {
    let mut s = Store::temporary("query-latest").unwrap();
    // Entity 1 closes in January only; entity 2 in January and March, its
    // March row received in April.
    let (jan, mar) = (T0 - 7 * DAY, T0 + 60 * DAY);
    let rows = vec![(1, jan, jan, 1.0), (2, jan, jan, 2.0), (2, mar, mar + 30 * DAY, 3.0)];
    s.ingest("equity.close", &[close_rows(&rows)], "test", BTreeMap::new()).unwrap();
    let latest = |a: u64, t_max: i64, r: Option<i64>| {
        run(
            &s,
            QueryForm::Latest(Box::new(Latest {
                relation: "equity.close".into(),
                identity: Some(Box::new(entity(a))),
                t_max,
            })),
            r,
        )
    };
    assert_eq!(latest(1, mar + DAY, None), vec![(1, jan, 1.0)], "two buckets back");
    assert_eq!(latest(2, mar + DAY, None), vec![(2, mar, 3.0)]);
    assert_eq!(latest(2, mar + DAY, Some(mar + DAY)), vec![(2, jan, 2.0)], "the March row is not received yet");
    assert_eq!(latest(2, jan - DAY, None), vec![], "nothing at or before t_max");
    assert_eq!(latest(3, mar + DAY, None), vec![], "an entity with no rows");
}

#[test]
fn series_and_section() {
    let (s, _, _) = store();
    let series = run(
        &s,
        QueryForm::Series(Box::new(Series {
            relation: "equity.close".into(),
            identity: Some(Box::new(entity(3))),
            t0: T0 + DAY,
            t1: T0 + 3 * DAY,
        })),
        None,
    );
    assert_eq!(series, vec![(3, T0 + DAY, 31.0), (3, T0 + 2 * DAY, 32.0), (3, T0 + 3 * DAY, 33.0)]);
    let section = run(
        &s,
        QueryForm::Section(Box::new(Section {
            relation: "equity.close".into(),
            t: T0 + 4 * DAY,
        })),
        None,
    );
    assert_eq!(section, vec![(1, T0 + 4 * DAY, 14.0), (2, T0 + 4 * DAY, 24.0), (3, T0 + 4 * DAY, 34.0)]);
}

#[test]
fn scan_exists_count_and_distinct_take_a_predicate() {
    let (s, _, _) = store();
    // (P >= 30 or A = 1) and T < T0 + 2d.
    let p = || {
        pred(vec![
            vec![atom("P", Op::Ge, vec![num(30.0)]), atom("A", Op::Eq, vec![ent(1)])],
            vec![atom(
                "T",
                Op::Lt,
                vec![Constant {
                    kind: ConstantKind::Time,
                    num: 0.0,
                    int: T0 + 2 * DAY,
                    text: None,
                }],
            )],
        ])
    };
    let scan = run(
        &s,
        QueryForm::Scan(Box::new(Scan {
            relation: "equity.close".into(),
            t0: T0,
            t1: T0 + 4 * DAY,
            predicate: p(),
            columns: None,
        })),
        None,
    );
    assert_eq!(scan, vec![(1, T0, 10.0), (3, T0, 30.0), (1, T0 + DAY, 11.0), (3, T0 + DAY, 31.0)]);
    let (_, b) = execute(
        &s,
        &ResolvedView::default(),
        &QueryForm::Exists(Box::new(Exists {
            relation: "equity.close".into(),
            t0: T0,
            t1: T0 + 4 * DAY,
            predicate: p(),
        })),
        None,
        false,
        false,
    )
    .unwrap();
    assert!(b[0].column(0).as_boolean().value(0));
    let none = pred(vec![vec![atom("P", Op::Gt, vec![num(1e9)])]]);
    let (_, b) = execute(
        &s,
        &ResolvedView::default(),
        &QueryForm::Exists(Box::new(Exists {
            relation: "equity.close".into(),
            t0: T0,
            t1: T0 + 4 * DAY,
            predicate: none,
        })),
        None,
        false,
        false,
    )
    .unwrap();
    assert!(!b[0].column(0).as_boolean().value(0));
    let (_, b) = execute(
        &s,
        &ResolvedView::default(),
        &QueryForm::CountRows(Box::new(CountRows {
            relation: "equity.close".into(),
            t0: T0,
            t1: T0 + 4 * DAY,
            predicate: p(),
        })),
        None,
        false,
        false,
    )
    .unwrap();
    assert_eq!(b[0].column(0).as_primitive::<arrow::datatypes::Int64Type>().value(0), 4);
    // `in` and `is null`.
    let p_in = pred(vec![vec![atom("A", Op::In, vec![ent(2), ent(3)])]]);
    let (_, b) = execute(
        &s,
        &ResolvedView::default(),
        &QueryForm::CountRows(Box::new(CountRows {
            relation: "equity.close".into(),
            t0: T0,
            t1: T0 + 4 * DAY,
            predicate: p_in,
        })),
        None,
        false,
        false,
    )
    .unwrap();
    assert_eq!(b[0].column(0).as_primitive::<arrow::datatypes::Int64Type>().value(0), 10);
    let p_null = pred(vec![vec![atom("P", Op::IsNull, vec![])]]);
    let (_, b) = execute(
        &s,
        &ResolvedView::default(),
        &QueryForm::CountRows(Box::new(CountRows {
            relation: "equity.close".into(),
            t0: T0,
            t1: T0 + 4 * DAY,
            predicate: p_null,
        })),
        None,
        false,
        false,
    )
    .unwrap();
    assert_eq!(b[0].column(0).as_primitive::<arrow::datatypes::Int64Type>().value(0), 0);
    // A projection.
    let (schema, b) = execute(
        &s,
        &ResolvedView::default(),
        &QueryForm::Scan(Box::new(Scan {
            relation: "equity.close".into(),
            t0: T0,
            t1: T0,
            predicate: None,
            columns: Some(vec!["P".into(), "A".into()]),
        })),
        None,
        false,
        false,
    )
    .unwrap();
    assert_eq!(schema.fields().iter().map(|f| f.name().as_str()).collect::<Vec<_>>(), vec!["P", "A"]);
    assert_eq!(b[0].num_rows(), 3);
    // The entities present.
    let (_, b) = execute(
        &s,
        &ResolvedView::default(),
        &QueryForm::Distinct(Box::new(Distinct {
            relation: "equity.close".into(),
            column: "A".into(),
            t0: T0,
            t1: T0 + 4 * DAY,
        })),
        None,
        false,
        false,
    )
    .unwrap();
    assert_eq!(b[0].column(0).as_primitive::<arrow::datatypes::UInt64Type>().values().to_vec(), vec![1, 2, 3]);
}

#[test]
fn a_view_reads_its_versions_at_its_sequence_number() {
    let (mut s, v1, v2) = store();
    let form = || {
        QueryForm::Point(Box::new(Point {
            relation: "equity.close".into(),
            identity: Some(Box::new(entity(1))),
            t: T0 + 2 * DAY,
        }))
    };
    let read = |s: &Store, view: &View| {
        let v = ResolvedView::of(s, Some(view)).unwrap();
        closes(&execute(s, &v, &form(), None, false, false).unwrap().1)
    };
    let pinned = View {
        versions: Some(vec![VersionRef {
            relation: "equity.close".into(),
            version: v1.clone(),
        }]),
        binding: None,
        as_of_seq: None,
    };
    assert_eq!(read(&s, &pinned), vec![(1, T0 + 2 * DAY, 12.0)], "the first version has no correction");
    let snap = View {
        versions: Some(vec![VersionRef {
            relation: "equity.close".into(),
            version: v2.clone(),
        }]),
        binding: None,
        as_of_seq: Some(15),
    };
    assert_eq!(read(&s, &snap), vec![(1, T0 + 2 * DAY, 12.0)], "the correction is the sixteenth row");
    // Through a binding.
    let id = s
        .registry
        .record_binding(&Binding {
            pairs: Some(vec![VersionRef {
                relation: "equity.close".into(),
                version: v1.clone(),
            }]),
            point: None,
            dynamic: None,
            id: None,
        })
        .unwrap();
    assert!(s.registry.pinned(&v1), "a binding pins its versions");
    let through = View {
        versions: None,
        binding: Some(id.clone()),
        as_of_seq: None,
    };
    assert_eq!(read(&s, &through), vec![(1, T0 + 2 * DAY, 12.0)]);
    // The same binding recorded twice has the same id.
    let again = s
        .registry
        .record_binding(&Binding {
            pairs: Some(vec![VersionRef {
                relation: "equity.close".into(),
                version: v1,
            }]),
            point: None,
            dynamic: None,
            id: None,
        })
        .unwrap();
    assert_eq!(id, again);
    let missing = View {
        versions: None,
        binding: Some("0".repeat(64)),
        as_of_seq: None,
    };
    assert!(ResolvedView::of(&s, Some(&missing)).is_err());
}

#[test]
fn the_series_class_is_keyed_by_a_label() {
    let mut s = Store::temporary("query-series").unwrap();
    let schema = Arc::new(Schema::new(vec![
        Field::new("Name", DataType::Utf8, false),
        Field::new("T", DataType::Timestamp(TimeUnit::Nanosecond, None), false),
        Field::new("V", DataType::Float64, false),
    ]));
    let b = RecordBatch::try_new(
        schema,
        vec![
            Arc::new(StringArray::from(vec!["VIX", "SPXTR", "VIX", "SPXTR"])),
            Arc::new(TimestampNanosecondArray::from(vec![T0, T0, T0 + DAY, T0 + DAY])),
            Arc::new(Float64Array::from(vec![20.0, 5000.0, 21.0, 5010.0])),
        ],
    )
    .unwrap();
    s.ingest("series.value", &[b], "test", BTreeMap::new()).unwrap();
    let id = Identity { values: vec![label("VIX")] };
    let (_, out) = execute(
        &s,
        &ResolvedView::default(),
        &QueryForm::Series(Box::new(Series {
            relation: "series.value".into(),
            identity: Some(Box::new(id)),
            t0: T0,
            t1: T0 + DAY,
        })),
        None,
        false,
        false,
    )
    .unwrap();
    let names = out[0].column(0).as_dictionary::<arrow::datatypes::UInt32Type>();
    let values = names.values().as_string::<i32>();
    let got: Vec<(String, f64)> = (0..out[0].num_rows())
        .map(|i| {
            (
                values.value(names.keys().value(i) as usize).to_string(),
                out[0].column(2).as_primitive::<arrow::datatypes::Float64Type>().value(i),
            )
        })
        .collect();
    assert_eq!(got, vec![("VIX".to_string(), 20.0), ("VIX".to_string(), 21.0)]);
    // An unknown label is an empty answer, not an error.
    let id = Identity { values: vec![label("NOPE")] };
    let (_, out) = execute(
        &s,
        &ResolvedView::default(),
        &QueryForm::Series(Box::new(Series {
            relation: "series.value".into(),
            identity: Some(Box::new(id)),
            t0: T0,
            t1: T0 + DAY,
        })),
        None,
        false,
        false,
    )
    .unwrap();
    assert_eq!(out.iter().map(|b| b.num_rows()).sum::<usize>(), 0);
}

#[test]
fn catalog_reads_the_registry_and_only_it() {
    let mut s = Store::temporary("query-catalog").unwrap();
    let schema = Arc::new(Schema::new(vec![
        Field::new("id", DataType::UInt64, false),
        Field::new("from", DataType::Timestamp(TimeUnit::Nanosecond, None), false),
        Field::new("to", DataType::Timestamp(TimeUnit::Nanosecond, None), true),
        Field::new("class", DataType::Utf8, false),
        Field::new("exchange", DataType::Utf8, false),
        Field::new("currency", DataType::Utf8, false),
    ]));
    let b = RecordBatch::try_new(
        schema,
        vec![
            Arc::new(UInt64Array::from(vec![1, 2, 3])),
            Arc::new(TimestampNanosecondArray::from(vec![T0, T0, T0])),
            Arc::new(TimestampNanosecondArray::from(vec![None, None, Some(T0 + DAY)])),
            Arc::new(StringArray::from(vec!["equity", "future", "equity"])),
            Arc::new(StringArray::from(vec!["XNYS", "XCME", "XNAS"])),
            Arc::new(StringArray::from(vec!["USD", "USD", "USD"])),
        ],
    )
    .unwrap();
    s.ingest("instrument", &[b], "registry", BTreeMap::new()).unwrap();
    let p = pred(vec![vec![atom("class", Op::Eq, vec![label("equity")])], vec![atom("to", Op::IsNull, vec![])]]);
    let (_, out) = execute(
        &s,
        &ResolvedView::default(),
        &QueryForm::Catalog(Box::new(Catalog {
            relation: "instrument".into(),
            predicate: p,
        })),
        None,
        false,
        false,
    )
    .unwrap();
    assert_eq!(out[0].column(0).as_primitive::<arrow::datatypes::UInt64Type>().values().to_vec(), vec![1]);
    assert!(out[0].column(2).is_null(0));
    let err = execute(
        &s,
        &ResolvedView::default(),
        &QueryForm::Catalog(Box::new(Catalog {
            relation: "equity.close".into(),
            predicate: None,
        })),
        None,
        false,
        false,
    );
    assert!(err.is_err(), "close is not a registry relation");
}

#[test]
fn explain_counts_what_a_query_touches() {
    let (s, _, _) = store();
    let e = explain(
        &s,
        &ResolvedView::default(),
        &QueryForm::Scan(Box::new(Scan {
            relation: "equity.close".into(),
            t0: T0,
            t1: T0 + 4 * DAY,
            predicate: None,
            columns: None,
        })),
    )
    .unwrap();
    assert_eq!(e.rows, 16, "fifteen bars and one correction");
    assert!(e.partitions >= 1 && e.bytes > 0);
    assert!(!e.unresolved);
    let none = explain(
        &s,
        &ResolvedView::default(),
        &QueryForm::Scan(Box::new(Scan {
            relation: "equity.close".into(),
            t0: T0 + 400 * DAY,
            t1: T0 + 500 * DAY,
            predicate: None,
            columns: None,
        })),
    )
    .unwrap();
    assert_eq!((none.rows, none.partitions), (0, 0));
    // No chain is indexed before the option class lands (D-4.4, E-17).
    let chain = explain(
        &s,
        &ResolvedView::default(),
        &QueryForm::Chain(Box::new(Chain {
            underlying: 1,
            t: T0,
            filters: None,
            spot: None,
        })),
    )
    .unwrap();
    assert!(chain.unresolved);
}

#[test]
fn answers_are_deterministic_and_carry_seq_on_request() {
    let (s, _, _) = store();
    let form = QueryForm::Scan(Box::new(Scan {
        relation: "equity.close".into(),
        t0: T0,
        t1: T0 + 4 * DAY,
        predicate: None,
        columns: None,
    }));
    let (a, x) = execute(&s, &ResolvedView::default(), &form, None, true, false).unwrap();
    let (b, y) = execute(&s, &ResolvedView::default(), &form, None, true, false).unwrap();
    assert_eq!(a, b);
    assert_eq!(x, y);
    assert_eq!(a.fields().last().unwrap().name(), "seq");
    assert_eq!(x[0].num_rows(), 15, "the latest revision of each fact");
}
