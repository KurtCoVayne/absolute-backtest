//! The column reads (`store_columns`) held equal to the row reads they
//! stand in for: `distinct` of an event time (a hot version from its
//! presence and appended labels, a Parquet version from the time column
//! alone), `distinct` of a key label of a Parquet version, and a scan that
//! names its columns. Each on stores with a revision, a superseded row and,
//! for the hot version, sealed append blocks and unsealed tails, over the
//! whole history and over windows; and each declined where a column cannot
//! answer (a receipt or sequence cut, a label that is a value, a hot scan).
//! And the predicate seen to keep no row (E-139): a comparison every value
//! of the column is above, by a hot version's block headers or a Parquet
//! version's statistics, answered as the row read answers it, under every
//! cut, and read from the rows wherever a value may pass.

mod hot_common;

use std::collections::BTreeMap;
use std::sync::Arc;

use abt_data::query::{execute, execute_rows, shortcut, ResolvedView};
use abt_data::{RowFilter, Store};
use abt_wire::generated::abt::wire::{Atom, Constant, ConstantKind, CountRows, Disjunction, Distinct, Exists, Op, Predicate, QueryForm, Scan};
use abt_wire::scalar::{NANOS_PER_DAY as DAY, NANOS_PER_HOUR as HOUR};
use arrow::array::{AsArray, Float64Array, RecordBatch, StringArray, TimestampNanosecondArray, UInt64Array};
use arrow::datatypes::{DataType, Field, Schema, TimeUnit};
use hot_common::*;

const CLOSE: &str = "equity.close";
const MEMBER: &str = "equity.member";

fn distinct(relation: &str, column: &str, (t0, t1): (i64, i64)) -> QueryForm {
    QueryForm::Distinct(Box::new(Distinct {
        relation: relation.into(),
        column: column.into(),
        t0,
        t1,
    }))
}

fn scan(relation: &str, (t0, t1): (i64, i64), predicate: Option<Predicate>, columns: Option<&[&str]>) -> QueryForm {
    QueryForm::Scan(Box::new(Scan {
        relation: relation.into(),
        t0,
        t1,
        predicate: predicate.map(Box::new),
        columns: columns.map(|c| c.iter().map(|c| c.to_string()).collect()),
    }))
}

fn label(s: &str) -> Constant {
    Constant {
        kind: ConstantKind::Label,
        num: 0.0,
        int: 0,
        text: Some(s.to_string()),
    }
}

fn entity(id: u64) -> Constant {
    Constant {
        kind: ConstantKind::Entity,
        num: 0.0,
        int: id as i64,
        text: None,
    }
}

fn number(x: f64) -> Constant {
    Constant {
        kind: ConstantKind::Num,
        num: x,
        int: 0,
        text: None,
    }
}

/// A conjunction of single atoms.
fn all_of(atoms: Vec<(&str, Op, Vec<Constant>)>) -> Predicate {
    Predicate {
        all: Some(
            atoms
                .into_iter()
                .map(|(column, op, values)| Disjunction {
                    atoms: vec![Atom {
                        column: column.to_string(),
                        op,
                        values: Some(values),
                    }],
                })
                .collect(),
        ),
    }
}

/// A query answered by its column read, which must be taken, and by its
/// rows: the same schema and the same batches, and `execute` gives them.
fn same(s: &Store, form: &QueryForm) -> Vec<RecordBatch> {
    let view = ResolvedView::default();
    let by_rows = execute_rows(s, &view, form, None, false, false).unwrap();
    let by_columns = shortcut(s, &view, form, None, false).unwrap().unwrap_or_else(|| panic!("no column read of {:?}", form));
    assert_eq!(by_columns.0, by_rows.0, "the schema of {:?}", form);
    assert_eq!(by_columns.1, by_rows.1, "the rows of {:?}", form);
    let answered = execute(s, &view, form, None, false, false).unwrap();
    assert_eq!((answered.0, &answered.1), (by_rows.0, &by_rows.1), "the answer of {:?}", form);
    by_rows.1
}

/// A query the column reads decline: `execute` answers it by its rows.
fn declined(s: &Store, view: &ResolvedView, form: &QueryForm, r_max: Option<i64>, with_seq: bool) {
    assert!(shortcut(s, view, form, r_max, with_seq).unwrap().is_none(), "a column read of {:?}", form);
    let by_rows = execute_rows(s, view, form, r_max, with_seq, false).unwrap();
    let answered = execute(s, view, form, r_max, with_seq, false).unwrap();
    assert_eq!((answered.0, &answered.1), (by_rows.0, &by_rows.1), "the answer of {:?}", form);
}

/// The whole history, windows that hold nothing (one of them ending before
/// it starts), one label, and spans drawn over `[start, start + days)` (in
/// days).
fn windows(rng: &mut Rng, start: i64, days: i64) -> Vec<(i64, i64)> {
    let t = |d: i64| (start + d) * DAY;
    let mut out = vec![
        (i64::MIN, i64::MAX),
        (i64::MIN, t(days / 2)),
        (t(days / 2), i64::MAX),
        (t(-40), t(-20)),
        (t(days + 20), t(days + 40)),
        (t(days / 3), t(days / 3)),
        (t(days / 3) + 1, t(days / 3) + 2),
        (t(days / 2), t(days / 3)),
    ];
    for _ in 0..40 {
        let d0 = rng.below(days as u64) as i64;
        let d1 = d0 + rng.below((days - d0) as u64 + 1) as i64;
        // Some ends fall between two labels.
        out.push((t(d0) + (rng.below(3) as i64) * HOUR, t(d1) + (rng.below(3) as i64) * HOUR));
    }
    out
}

/// The times of an answer's one column.
fn times_of(batches: &[RecordBatch]) -> Vec<i64> {
    batches
        .iter()
        .flat_map(|b| b.column(0).as_primitive::<arrow::datatypes::TimestampNanosecondType>().values().to_vec())
        .collect()
}

/// The labels of an answer's one column, a null as `None`.
fn labels_of(batches: &[RecordBatch]) -> Vec<Option<String>> {
    let mut out = Vec::new();
    for b in batches {
        let text = arrow::compute::cast(b.column(0), &DataType::Utf8).unwrap();
        out.extend(text.as_string::<i32>().iter().map(|s| s.map(str::to_string)));
    }
    out
}

/// `equity.close` twice from the same batches, hot and as Parquet: five
/// instruments published over 1,000 days, a sixth with one row at an hour
/// no other row has; that row and a published row of the first instrument
/// revised a day later; then 1,400 days appended twenty at a time, which
/// seals blocks of 1,024 rows and leaves tails. Returns the stores, their
/// versions, the first day and the days.
fn closes() -> (Store, String, Store, String, i64, i64) {
    let mut rng = Rng(7);
    let ids = [3u64, 5, 8, 13, 21];
    let (start, days, split) = (18_000i64, 2_400i64, 1_000i64);
    let rows = market(&mut rng, &ids, start, days);
    let t = |d: i64| (start + d) * DAY;
    let alone = (34u64, t(500) + HOUR, t(500) + 2 * HOUR, 7.25);
    let mut base: Vec<_> = rows.iter().filter(|r| r.1 < t(split)).cloned().collect();
    base.push(alone);
    let revised = *base.iter().find(|r| r.0 == 3 && r.1 >= t(400)).expect("a published row of instrument 3");
    let corrections = [(alone.0, alone.1, alone.2 + DAY, 7.5), (revised.0, revised.1, revised.2 + DAY, revised.3 + 1.0)];
    let mut hot_store = Store::temporary("columns-hot").unwrap();
    let mut cold = Store::temporary("columns-cold").unwrap();
    hot_store.ingest(CLOSE, &[close_rows(&base)], "test", hot()).unwrap();
    cold.ingest(CLOSE, &[close_rows(&base)], "test", BTreeMap::new()).unwrap();
    let mut vh = hot_store.ingest(CLOSE, &[close_rows(&corrections)], "test", hot()).unwrap();
    let mut vc = cold.ingest(CLOSE, &[close_rows(&corrections)], "test", BTreeMap::new()).unwrap();
    let mut d = split;
    while d < days {
        let batch: Vec<_> = rows.iter().filter(|r| r.1 >= t(d) && r.1 < t(d + 20)).cloned().collect();
        if !batch.is_empty() {
            vh = hot_store.ingest(CLOSE, &[close_rows(&batch)], "test", hot()).unwrap();
            vc = cold.ingest(CLOSE, &[close_rows(&batch)], "test", BTreeMap::new()).unwrap();
        }
        d += 20;
    }
    (hot_store, vh, cold, vc, start, days)
}

#[test]
fn distinct_times_of_a_hot_relation_with_appends_equal_the_row_read() {
    let (s, v, _, _, start, days) = closes();
    // The store is the one the read is for: superseded rows kept beside
    // the published ones, sealed append blocks, tails not yet sealed.
    {
        let h = s.hot(&v).unwrap();
        assert!(!h.manifest.revisions.is_empty(), "a superseded row");
        let ov = h.overlay.lock().unwrap();
        assert!(ov.instruments.iter().any(|i| !i.sealed.is_empty()), "a sealed append block");
        assert!(ov.instruments.iter().any(|i| !i.tail.is_empty()), "a tail");
    }
    let mut rng = Rng(3);
    let alone = (start + 500) * DAY + HOUR;
    for w in windows(&mut rng, start, days) {
        let got = times_of(&same(&s, &distinct(CLOSE, "T", w)));
        assert!(got.windows(2).all(|p| p[0] < p[1]), "ascending in {:?}", w);
        // The label whose one row was superseded is still a time of the
        // relation: its correction is a row.
        assert_eq!(got.contains(&alone), w.0 <= alone && alone <= w.1, "the lone label in {:?}", w);
        // And the read's own account of itself: the times of the rows.
        let f = RowFilter {
            t0: Some(w.0),
            t1: Some(w.1),
            latest_only: true,
            ..RowFilter::default()
        };
        let mut of_rows: Vec<i64> = s
            .read(&v, &f)
            .unwrap()
            .iter()
            .flat_map(|b| b.column(1).as_primitive::<arrow::datatypes::TimestampNanosecondType>().values().to_vec())
            .collect();
        of_rows.sort_unstable();
        of_rows.dedup();
        assert_eq!(s.distinct_times(&v, &f).unwrap(), Some(of_rows), "the times in {:?}", w);
    }
    assert!(times_of(&same(&s, &distinct(CLOSE, "T", (i64::MIN, i64::MAX)))).len() > 2_000);
}

#[test]
fn a_cut_by_receipt_sequence_or_identity_is_not_answered_from_columns() {
    let (s, v, cold, vc, start, _) = closes();
    let whole = (i64::MIN, i64::MAX);
    let alone = (start + 500) * DAY + HOUR;
    for (store, version) in [(&s, &v), (&cold, &vc)] {
        let view = ResolvedView::default();
        // Received by the lone row's own hour, before any row of that day's
        // close was: the cut leaves the label's first row and no other.
        declined(store, &view, &distinct(CLOSE, "T", whole), Some(alone + HOUR), false);
        declined(store, &view, &distinct(CLOSE, "T", whole), Some((start + 300) * DAY), false);
        let at = ResolvedView {
            as_of_seq: Some(40),
            ..ResolvedView::default()
        };
        declined(store, &at, &distinct(CLOSE, "T", whole), None, false);
        declined(store, &at, &scan(CLOSE, whole, None, Some(&["A"])), None, false);
        declined(store, &view, &scan(CLOSE, whole, None, Some(&["A"])), Some((start + 300) * DAY), false);
        // A scan's answer with `seq`, a scan of every column, and a scan
        // that names none read their rows.
        declined(store, &view, &scan(CLOSE, whole, None, Some(&["A", "T"])), None, true);
        declined(store, &view, &scan(CLOSE, whole, None, Some(&["A", "T", "P", "r"])), None, false);
        declined(store, &view, &scan(CLOSE, whole, None, None), None, false);
        // The entity column is not a label: its distinct values are read
        // from rows.
        declined(store, &view, &distinct(CLOSE, "A", whole), None, false);
        for f in [
            RowFilter {
                identity: Some(vec![3]),
                latest_only: true,
                ..RowFilter::default()
            },
            RowFilter {
                r_max: Some((start + 300) * DAY),
                latest_only: true,
                ..RowFilter::default()
            },
            RowFilter {
                as_of_seq: Some(40),
                latest_only: true,
                ..RowFilter::default()
            },
        ] {
            assert_eq!(store.distinct_times(version, &f).unwrap(), None, "{:?}", f);
        }
    }
    // A hot version's scan reads its blocks, not columns of a file.
    declined(&s, &ResolvedView::default(), &scan(CLOSE, whole, None, Some(&["A"])), None, false);
}

#[test]
fn distinct_times_of_a_parquet_relation_from_its_time_column_equal_the_row_read() {
    let (_, _, s, v, start, days) = closes();
    // The partitions hold both rows of a revised fact.
    let all = s.read_all(&v).unwrap().num_rows();
    let latest = s
        .read(
            &v,
            &RowFilter {
                latest_only: true,
                ..RowFilter::default()
            },
        )
        .unwrap()
        .iter()
        .map(|b| b.num_rows())
        .sum::<usize>();
    assert_eq!(all, latest + 2, "two superseded rows");
    let mut rng = Rng(5);
    let alone = (start + 500) * DAY + HOUR;
    for w in windows(&mut rng, start, days) {
        let got = times_of(&same(&s, &distinct(CLOSE, "T", w)));
        assert!(got.windows(2).all(|p| p[0] < p[1]), "ascending in {:?}", w);
        assert_eq!(got.contains(&alone), w.0 <= alone && alone <= w.1, "the lone label in {:?}", w);
    }
}

/// `(A, T, r, List)` rows of `equity.member`.
fn member_rows(rows: &[(u64, i64, i64, &str)]) -> RecordBatch {
    let schema = Arc::new(Schema::new(vec![
        Field::new("A", DataType::UInt64, false),
        Field::new("T", DataType::Timestamp(TimeUnit::Nanosecond, None), false),
        Field::new("List", DataType::Utf8, false),
        Field::new("r", DataType::Timestamp(TimeUnit::Nanosecond, None), false),
    ]));
    RecordBatch::try_new(
        schema,
        vec![
            Arc::new(UInt64Array::from(rows.iter().map(|r| r.0).collect::<Vec<_>>())),
            Arc::new(TimestampNanosecondArray::from(rows.iter().map(|r| r.1).collect::<Vec<_>>())),
            Arc::new(StringArray::from(rows.iter().map(|r| r.3).collect::<Vec<_>>())),
            Arc::new(TimestampNanosecondArray::from(rows.iter().map(|r| r.2).collect::<Vec<_>>())),
        ],
    )
    .unwrap()
}

/// A membership of forty instruments over 150 days in four lists: `LATE`
/// is interned first and listed last, so the order of a first row is not
/// the order of the codes; an instrument is in two lists at once; rows of
/// three facts are delivered again a day later (revisions: `member` has no
/// value, so a revision differs by its receipt alone).
fn members() -> (Store, String, i64, i64) {
    let (start, days) = (19_000i64, 150i64);
    let t = |d: i64| (start + d) * DAY;
    let mut rows: Vec<(u64, i64, i64, &str)> = Vec::new();
    for d in 100..days {
        rows.push((40, t(d), t(d), "LATE"));
    }
    for d in 0..days {
        for a in 1..40u64 {
            if a % 2 == 0 && (d + a as i64) % 11 != 0 {
                rows.push((a, t(d), t(d), "SP500"));
            }
            if a % 3 == 0 && d >= 20 {
                rows.push((a, t(d), t(d), "SP400"));
            }
            if a % 7 == 0 && (40..90).contains(&d) {
                rows.push((a, t(d), t(d), "SP600"));
            }
        }
    }
    let mut s = Store::temporary("columns-member").unwrap();
    s.ingest(MEMBER, &[member_rows(&rows)], "test", BTreeMap::new()).unwrap();
    let again = [(6u64, t(30), t(31), "SP500"), (6, t(30), t(31), "SP400"), (14, t(60), t(61), "SP600")];
    let v = s.ingest(MEMBER, &[member_rows(&again)], "test", BTreeMap::new()).unwrap();
    assert_eq!(s.read_all(&v).unwrap().num_rows(), rows.len() + again.len(), "the superseded rows are kept");
    (s, v, start, days)
}

#[test]
fn distinct_labels_of_a_parquet_relation_from_its_label_column_equal_the_row_read() {
    let (s, _, start, days) = members();
    let text = |l: &[&str]| l.iter().map(|x| Some(x.to_string())).collect::<Vec<_>>();
    // In the order of their first rows, which is not the codes'.
    assert_eq!(labels_of(&same(&s, &distinct(MEMBER, "List", (i64::MIN, i64::MAX)))), text(&["SP500", "SP400", "SP600", "LATE"]));
    let t = |d: i64| (start + d) * DAY;
    assert_eq!(labels_of(&same(&s, &distinct(MEMBER, "List", (t(95), t(110))))), text(&["SP500", "SP400", "LATE"]));
    assert_eq!(labels_of(&same(&s, &distinct(MEMBER, "List", (t(0), t(5))))), text(&["SP500"]));
    assert_eq!(labels_of(&same(&s, &distinct(MEMBER, "List", (t(-9), t(-3))))), text(&[]));
    let mut rng = Rng(9);
    for w in windows(&mut rng, start, days) {
        same(&s, &distinct(MEMBER, "List", w));
        same(&s, &distinct(MEMBER, "T", w));
    }
}

#[test]
fn a_null_label_and_a_label_keyed_series_are_answered_alike() {
    let t = |d: i64| (19_000 + d) * DAY;
    let mut s = Store::temporary("columns-labels").unwrap();
    // A classification whose code is unknown on some days: a null is one
    // of the column's values, met where its first row is.
    let schema = Arc::new(Schema::new(vec![
        Field::new("A", DataType::UInt64, false),
        Field::new("T", DataType::Timestamp(TimeUnit::Nanosecond, None), false),
        Field::new("Scheme", DataType::Utf8, false),
        Field::new("Code", DataType::Utf8, true),
    ]));
    let rows: Vec<(u64, i64, &str, Option<&str>)> = (0..70)
        .flat_map(|d| {
            [
                (1u64, t(d), "GICS", if d < 30 { Some("4510") } else { Some("2550") }),
                (2, t(d), "GICS", if (10..15).contains(&d) { None } else { Some("1010") }),
                (2, t(d), "TRBC", Some("57")),
            ]
        })
        .collect();
    let b = RecordBatch::try_new(
        schema,
        vec![
            Arc::new(UInt64Array::from(rows.iter().map(|r| r.0).collect::<Vec<_>>())),
            Arc::new(TimestampNanosecondArray::from(rows.iter().map(|r| r.1).collect::<Vec<_>>())),
            Arc::new(StringArray::from(rows.iter().map(|r| r.2).collect::<Vec<_>>())),
            Arc::new(StringArray::from(rows.iter().map(|r| r.3).collect::<Vec<_>>())),
        ],
    )
    .unwrap();
    s.ingest("equity.classification", &[b], "test", BTreeMap::new()).unwrap();
    let whole = (i64::MIN, i64::MAX);
    let codes = labels_of(&same(&s, &distinct("equity.classification", "Code", whole)));
    assert_eq!(codes, vec![Some("4510".to_string()), Some("1010".to_string()), Some("57".to_string()), None, Some("2550".to_string())]);
    assert_eq!(labels_of(&same(&s, &distinct("equity.classification", "Scheme", whole))).len(), 2);
    for w in [(t(11), t(12)), (t(12), t(40)), (t(31), t(69)), (t(0), t(9))] {
        same(&s, &distinct("equity.classification", "Code", w));
        same(&s, &distinct("equity.classification", "Scheme", w));
        same(&s, &distinct("equity.classification", "T", w));
        same(&s, &scan("equity.classification", w, None, Some(&["Code"])));
        same(&s, &scan("equity.classification", w, Some(all_of(vec![("Scheme", Op::Eq, vec![label("GICS")])])), Some(&["A", "Code"])));
    }
    // A series keyed by its name (E-16): the name is the fact's identity,
    // one value revised.
    let schema = Arc::new(Schema::new(vec![
        Field::new("Name", DataType::Utf8, false),
        Field::new("T", DataType::Timestamp(TimeUnit::Nanosecond, None), false),
        Field::new("V", DataType::Float64, false),
        Field::new("r", DataType::Timestamp(TimeUnit::Nanosecond, None), false),
    ]));
    let series = |rows: &[(&str, i64, i64, f64)]| {
        RecordBatch::try_new(
            schema.clone(),
            vec![
                Arc::new(StringArray::from(rows.iter().map(|r| r.0).collect::<Vec<_>>())),
                Arc::new(TimestampNanosecondArray::from(rows.iter().map(|r| r.1).collect::<Vec<_>>())),
                Arc::new(Float64Array::from(rows.iter().map(|r| r.3).collect::<Vec<_>>())),
                Arc::new(TimestampNanosecondArray::from(rows.iter().map(|r| r.2).collect::<Vec<_>>())),
            ],
        )
        .unwrap()
    };
    let mut rows: Vec<(&str, i64, i64, f64)> = Vec::new();
    for d in 0..800 {
        if d % 7 == 4 && d >= 21 {
            rows.push(("VIX", t(d), t(d), 20.0 + (d % 13) as f64));
        }
        if d % 7 < 5 {
            rows.push(("SPXTR", t(d), t(d), 5_000.0 + d as f64));
        }
    }
    s.ingest("series.value", &[series(&rows)], "test", BTreeMap::new()).unwrap();
    s.ingest("series.value", &[series(&[("VIX", t(25), t(26), 99.0), ("SPXTR", t(400), t(401), 1.0)])], "test", BTreeMap::new())
        .unwrap();
    assert_eq!(labels_of(&same(&s, &distinct("series.value", "Name", whole))), vec![Some("SPXTR".to_string()), Some("VIX".to_string())]);
    for w in [whole, (t(0), t(20)), (t(24), t(26)), (t(300), t(500)), (t(399), t(400)), (t(900), t(950))] {
        same(&s, &distinct("series.value", "Name", w));
        same(&s, &distinct("series.value", "T", w));
        same(&s, &scan("series.value", w, None, Some(&["T", "V"])));
        same(&s, &scan("series.value", w, Some(all_of(vec![("Name", Op::Eq, vec![label("VIX")])])), Some(&["V"])));
        same(&s, &scan("series.value", w, Some(all_of(vec![("V", Op::Lt, vec![number(100.0)])])), Some(&["Name"])));
    }
}

#[test]
fn a_relation_without_a_row_is_answered_alike() {
    let mut s = Store::temporary("columns-empty").unwrap();
    let v = s.ingest(MEMBER, &[member_rows(&[])], "test", BTreeMap::new()).unwrap();
    assert_eq!(s.manifest(&v).unwrap().rows, 0);
    for w in [(i64::MIN, i64::MAX), (19_000 * DAY, 19_100 * DAY)] {
        assert_eq!(labels_of(&same(&s, &distinct(MEMBER, "List", w))), vec![]);
        assert_eq!(times_of(&same(&s, &distinct(MEMBER, "T", w))), Vec::<i64>::new());
        let got = same(&s, &scan(MEMBER, w, Some(all_of(vec![("List", Op::In, vec![label("SP500")])])), Some(&["A"])));
        assert_eq!(got.iter().map(|b| b.num_rows()).sum::<usize>(), 0);
    }
}

#[test]
fn a_scan_of_named_columns_reads_them_and_answers_as_the_row_read() {
    let (s, _, start, days) = members();
    let whole = (i64::MIN, i64::MAX);
    // What a binding asks of a membership: the instruments of some lists.
    let lists = all_of(vec![("List", Op::In, vec![label("SP400"), label("SP600"), label("NONE")])]);
    let got = same(&s, &scan(MEMBER, whole, Some(lists.clone()), Some(&["A"])));
    let mut ids: Vec<u64> = got.iter().flat_map(|b| b.column(0).as_primitive::<arrow::datatypes::UInt64Type>().values().to_vec()).collect();
    let rows = ids.len();
    ids.sort_unstable();
    ids.dedup();
    assert_eq!(ids, (1..40u64).filter(|a| a % 3 == 0 || a % 7 == 0).collect::<Vec<_>>());
    // One row a fact: the three delivered twice are answered once.
    let facts = (1..40u64).filter(|a| a % 3 == 0).count() * 130 + (1..40u64).filter(|a| a % 7 == 0).count() * 50;
    assert_eq!(rows, facts);
    let many: Vec<Constant> = (1..30u64).map(entity).collect();
    let predicates = [
        None,
        Some(lists),
        Some(all_of(vec![("List", Op::Eq, vec![label("LATE")])])),
        Some(all_of(vec![("List", Op::Ne, vec![label("SP500")])])),
        // An instrument's rows, through the partitions' indexes.
        Some(all_of(vec![("A", Op::In, vec![entity(6), entity(14), entity(77)])])),
        Some(all_of(vec![("A", Op::Eq, vec![entity(6)]), ("List", Op::In, vec![label("SP400")])])),
        // More constants than a row is compared with one by one.
        Some(all_of(vec![("A", Op::In, many), ("List", Op::In, vec![label("SP500"), label("SP600")])])),
        Some(all_of(vec![(
            "T",
            Op::Ge,
            vec![Constant {
                kind: ConstantKind::Time,
                num: 0.0,
                int: (start + 60) * DAY,
                text: None,
            }],
        )])),
    ];
    let projections: [&[&str]; 6] = [&["A"], &["A", "T"], &["List"], &["T", "A"], &["List", "A", "T"], &["T"]];
    let mut rng = Rng(21);
    let spans = windows(&mut rng, start, days);
    for (i, p) in predicates.iter().enumerate() {
        for (j, columns) in projections.iter().enumerate() {
            // Every predicate and projection over the whole history, and
            // each over a share of the windows.
            for w in spans.iter().enumerate().filter(|(k, _)| *k == 0 || (k + i + j) % 5 == 0).map(|(_, w)| *w) {
                same(&s, &scan(MEMBER, w, p.clone(), Some(columns)));
            }
        }
    }
    // A relation with a value: the price read without the receipt, the
    // instrument without the price, on the store with revised closes.
    let (_, _, cold, _, start, days) = closes();
    let cheap = all_of(vec![("P", Op::Le, vec![number(30.0)])]);
    let one = all_of(vec![("A", Op::In, vec![entity(3), entity(34)])]);
    let mut rng = Rng(22);
    for w in windows(&mut rng, start, days).into_iter().take(16) {
        same(&cold, &scan(CLOSE, w, Some(cheap.clone()), Some(&["A", "P"])));
        same(&cold, &scan(CLOSE, w, None, Some(&["T"])));
        same(&cold, &scan(CLOSE, w, Some(one.clone()), Some(&["P"])));
        same(&cold, &scan(CLOSE, w, Some(one.clone()), Some(&["A", "T"])));
    }
    // The corrected closes are the ones answered.
    let alone = (start + 500) * DAY + HOUR;
    let got = same(&cold, &scan(CLOSE, (alone, alone), None, Some(&["A", "P"])));
    assert_eq!(got[0].num_rows(), 1);
    assert_eq!(got[0].column(1).as_primitive::<arrow::datatypes::Float64Type>().value(0), 7.5);
}

#[test]
fn row_groups_of_one_label_or_outside_the_window_are_answered_without_being_read() {
    // A month of 3,400 members a day is past one row group of 65,536 rows:
    // the first month's groups each hold one label (their statistics say
    // so), the second's two, the third's the first again. The days are
    // January to March 2023.
    let (start, per_day) = (19_358i64, 3_400u64);
    let t = |d: i64| (start + d) * DAY;
    let mut rows: Vec<(u64, i64, i64, &str)> = Vec::new();
    for d in 0..90 {
        for a in 1..=per_day {
            let list = if (31..59).contains(&d) && a > 3_000 { "SP600" } else { "SP500" };
            rows.push((a, t(d), t(d), list));
        }
    }
    let mut s = Store::temporary("columns-groups").unwrap();
    let v = s.ingest(MEMBER, &[member_rows(&rows)], "test", BTreeMap::new()).unwrap();
    let m = s.manifest(&v).unwrap();
    assert_eq!(m.partitions.iter().map(|p| p.rows).collect::<Vec<_>>(), vec![31 * per_day, 28 * per_day, 31 * per_day]);
    // What the files say of themselves: two row groups each, the label's
    // least and greatest code equal in the first month's and not in the
    // second's.
    let one_label: Vec<Vec<bool>> = m
        .partitions
        .iter()
        .map(|p| {
            let file = std::fs::File::open(s.root().join(&p.file)).unwrap();
            let reader = parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder::try_new(file).unwrap();
            let groups = reader.metadata().row_groups();
            groups
                .iter()
                .map(|g| match g.column(2).statistics() {
                    Some(parquet::file::statistics::Statistics::Int32(st)) => st.min_opt() == st.max_opt() && st.min_opt().is_some(),
                    other => panic!("no statistics of the label: {:?}", other),
                })
                .collect()
        })
        .collect();
    assert_eq!(one_label, vec![vec![true, true], vec![false, false], vec![true, true]]);
    let whole = (i64::MIN, i64::MAX);
    assert_eq!(labels_of(&same(&s, &distinct(MEMBER, "List", whole))), vec![Some("SP500".to_string()), Some("SP600".to_string())]);
    assert_eq!(times_of(&same(&s, &distinct(MEMBER, "T", whole))), (0..90).map(t).collect::<Vec<_>>());
    for w in [
        (t(25), t(29)),
        (t(28), t(33)),
        (t(0), t(2)),
        (t(40), t(74)),
        (t(60), i64::MAX),
        (t(30) + HOUR, t(31) - HOUR),
        (i64::MIN, t(19)),
        (t(85), t(89)),
    ] {
        same(&s, &distinct(MEMBER, "List", w));
        let got = times_of(&same(&s, &distinct(MEMBER, "T", w)));
        assert_eq!(got, (0..90).map(t).filter(|x| w.0 <= *x && *x <= w.1).collect::<Vec<_>>(), "{:?}", w);
    }
    let some = all_of(vec![("List", Op::Eq, vec![label("SP600")])]);
    let got = same(&s, &scan(MEMBER, whole, Some(some), Some(&["A"])));
    assert_eq!(got.iter().map(|b| b.num_rows()).sum::<usize>(), 400 * 28);
}

/// Whether any row passes a predicate, and how many do.
fn exists(relation: &str, (t0, t1): (i64, i64), predicate: Predicate) -> QueryForm {
    QueryForm::Exists(Box::new(Exists {
        relation: relation.into(),
        t0,
        t1,
        predicate: Some(Box::new(predicate)),
    }))
}

fn count(relation: &str, (t0, t1): (i64, i64), predicate: Predicate) -> QueryForm {
    QueryForm::CountRows(Box::new(CountRows {
        relation: relation.into(),
        t0,
        t1,
        predicate: Some(Box::new(predicate)),
    }))
}

/// A query whose predicate is seen to keep no row: answered with no row
/// read, as the row read answers it and as `execute` does. The rows of the
/// answer.
fn kept_none(s: &Store, view: &ResolvedView, form: &QueryForm, r_max: Option<i64>, with_seq: bool) {
    let by_rows = execute_rows(s, view, form, r_max, with_seq, false).unwrap();
    let unread = shortcut(s, view, form, r_max, with_seq).unwrap().unwrap_or_else(|| panic!("rows read for {:?}", form));
    assert_eq!(unread.0, by_rows.0, "the schema of {:?}", form);
    assert_eq!(unread.1, by_rows.1, "the rows of {:?}", form);
    let answered = execute(s, view, form, r_max, with_seq, false).unwrap();
    assert_eq!((answered.0, &answered.1), (by_rows.0, &by_rows.1), "the answer of {:?}", form);
    match form {
        QueryForm::Exists(_) => assert!(!by_rows.1[0].column(0).as_boolean().value(0), "{:?}", form),
        QueryForm::CountRows(_) => assert_eq!(by_rows.1[0].column(0).as_primitive::<arrow::datatypes::Int64Type>().value(0), 0, "{:?}", form),
        _ => assert_eq!(by_rows.1.iter().map(|b| b.num_rows()).sum::<usize>(), 0, "{:?}", form),
    }
}

/// The forms a predicate is asked through, over a window: a scan of every
/// column, a scan of two, `exists` and `count`.
fn asked(relation: &str, w: (i64, i64), p: &Predicate) -> [QueryForm; 4] {
    [
        scan(relation, w, Some(p.clone()), None),
        scan(relation, w, Some(p.clone()), Some(&["A", "T"])),
        exists(relation, w, p.clone()),
        count(relation, w, p.clone()),
    ]
}

/// How many rows pass a predicate over a window, read from the rows (the
/// predicate may keep some): `execute` and the row read agree.
fn passing(s: &Store, view: &ResolvedView, w: (i64, i64), p: &Predicate, r_max: Option<i64>) -> i64 {
    for form in [scan(CLOSE, w, Some(p.clone()), None), exists(CLOSE, w, p.clone())] {
        declined(s, view, &form, r_max, false);
    }
    let form = count(CLOSE, w, p.clone());
    declined(s, view, &form, r_max, false);
    execute(s, view, &form, r_max, false, false).unwrap().1[0]
        .column(0)
        .as_primitive::<arrow::datatypes::Int64Type>()
        .value(0)
}

#[test]
fn a_comparison_no_value_passes_is_answered_with_no_row_read() {
    let (hot_store, vh, cold, vc, start, days) = closes();
    let whole = (i64::MIN, i64::MAX);
    let view = ResolvedView::default();
    let at = ResolvedView {
        as_of_seq: Some(40),
        ..ResolvedView::default()
    };
    let cut = Some((start + 300) * DAY);
    let mut rng = Rng(31);
    let spans: Vec<(i64, i64)> = windows(&mut rng, start, days).into_iter().take(14).collect();
    for (s, v) in [(&hot_store, &vh), (&cold, &vc)] {
        // The least close the version holds, its superseded rows among
        // them, is what the blocks' headers or the files' statistics give;
        // the other columns are not numbers.
        let all = s.read_all(v).unwrap();
        let least = all.column(2).as_primitive::<arrow::datatypes::Float64Type>().values().iter().copied().fold(f64::INFINITY, f64::min);
        assert!(least >= 1.0 && least.is_finite());
        assert_eq!(s.least_value(v, &RowFilter::default(), 2).unwrap(), Some(least));
        for c in [0, 1, 3, 4] {
            assert_eq!(s.least_value(v, &RowFilter::default(), c).unwrap(), None, "column {}", c);
        }
        for (op, x) in [(Op::Le, 0.0), (Op::Lt, 0.0), (Op::Le, -3.5), (Op::Lt, least), (Op::Le, least - 0.005)] {
            let p = all_of(vec![("P", op, vec![number(x)])]);
            for w in &spans {
                for form in asked(CLOSE, *w, &p) {
                    kept_none(s, &view, &form, None, false);
                }
            }
            // A cut by receipt or by sequence admits rows of the version
            // and no other: none passes there either.
            for form in asked(CLOSE, whole, &p) {
                kept_none(s, &view, &form, cut, false);
                kept_none(s, &at, &form, None, false);
                kept_none(s, &view, &form, None, true);
            }
        }
        // One conjunct no row passes is enough; a disjunction needs every
        // atom to keep none.
        let none_of_one = all_of(vec![("P", Op::Le, vec![number(0.0)]), ("A", Op::In, vec![entity(3), entity(34)])]);
        let either = Predicate {
            all: Some(vec![Disjunction {
                atoms: vec![
                    Atom {
                        column: "P".into(),
                        op: Op::Le,
                        values: Some(vec![number(0.0), number(-1.0)]),
                    },
                    Atom {
                        column: "P".into(),
                        op: Op::Lt,
                        values: Some(vec![number(least)]),
                    },
                ],
            }]),
        };
        for p in [&none_of_one, &either] {
            for form in asked(CLOSE, whole, p) {
                kept_none(s, &view, &form, None, false);
            }
        }
        // A constant a value reaches, another comparison, and a disjunction
        // with an atom a row may pass are read from the rows.
        let reach = |op: Op, x: f64| all_of(vec![("P", op, vec![number(x)])]);
        assert!(passing(s, &view, whole, &reach(Op::Le, 30.0), None) >= 1);
        assert!(
            passing(s, &view, whole, &reach(Op::Lt, least + 0.005), None) >= 1
                || s.read(
                    v,
                    &RowFilter {
                        latest_only: true,
                        ..RowFilter::default()
                    }
                )
                .unwrap()[0]
                    .num_rows()
                    < all.num_rows()
        );
        passing(s, &view, whole, &reach(Op::Le, least), cut);
        passing(s, &at, whole, &reach(Op::Le, least), None);
        assert!(passing(s, &view, whole, &reach(Op::Ge, 0.0), None) > 2_000);
        assert_eq!(passing(s, &view, whole, &reach(Op::Le, f64::NAN), None), 0);
        let or_one = Predicate {
            all: Some(vec![Disjunction {
                atoms: vec![
                    Atom {
                        column: "P".into(),
                        op: Op::Le,
                        values: Some(vec![number(0.0)]),
                    },
                    Atom {
                        column: "A".into(),
                        op: Op::Eq,
                        values: Some(vec![entity(34)]),
                    },
                ],
            }]),
        };
        assert_eq!(passing(s, &view, whole, &or_one, None), 1);
        // A predicate naming a column the relation has not is refused as
        // the row read refuses it.
        let unknown = all_of(vec![("P", Op::Le, vec![number(0.0)]), ("Nope", Op::Le, vec![number(0.0)])]);
        let form = scan(CLOSE, whole, Some(unknown), None);
        assert!(shortcut(s, &view, &form, None, false).unwrap().is_none());
        assert_eq!(execute(s, &view, &form, None, false, false).is_err(), execute_rows(s, &view, &form, None, false, false).is_err());
        assert!(execute(s, &view, &form, None, false, false).is_err());
    }
    // A hot version asked for its ticks answers decimals: the read makes
    // the answer.
    let p = all_of(vec![("P", Op::Le, vec![number(0.0)])]);
    let form = scan(CLOSE, whole, Some(p), None);
    let ticks = execute(&hot_store, &view, &form, None, false, true).unwrap();
    assert_eq!(ticks, execute_rows(&hot_store, &view, &form, None, false, true).unwrap());
}

/// `(A, day, P)` closes received an hour after their bar, a price absent
/// where `None`.
fn priced(start: i64, rows: &[(u64, i64, Option<f64>)]) -> RecordBatch {
    let schema = Arc::new(Schema::new(vec![
        Field::new("A", DataType::UInt64, false),
        Field::new("T", DataType::Timestamp(TimeUnit::Nanosecond, None), false),
        Field::new("P", DataType::Float64, true),
        Field::new("r", DataType::Timestamp(TimeUnit::Nanosecond, None), false),
    ]));
    RecordBatch::try_new(
        schema,
        vec![
            Arc::new(UInt64Array::from(rows.iter().map(|r| r.0).collect::<Vec<_>>())),
            Arc::new(TimestampNanosecondArray::from(rows.iter().map(|r| (start + r.1) * DAY).collect::<Vec<_>>())),
            Arc::new(Float64Array::from(rows.iter().map(|r| r.2).collect::<Vec<_>>())),
            Arc::new(TimestampNanosecondArray::from(rows.iter().map(|r| (start + r.1) * DAY + HOUR).collect::<Vec<_>>())),
        ],
    )
    .unwrap()
}

#[test]
fn a_value_that_may_pass_wherever_the_store_keeps_it_is_read_from_the_rows() {
    let start = 19_000i64;
    let whole = (i64::MIN, i64::MAX);
    let view = ResolvedView::default();
    let le = |x: f64| all_of(vec![("P", Op::Le, vec![number(x)])]);
    let lt = |x: f64| all_of(vec![("P", Op::Lt, vec![number(x)])]);
    let none = |s: &Store, p: &Predicate| {
        for form in asked(CLOSE, whole, p) {
            kept_none(s, &view, &form, None, false);
        }
    };
    // A hot version whose published block holds a value off every grid:
    // its bits are among the block's exceptions, and they bound it too.
    let off_grid = 0.1 + 0.2;
    let mut rows: Vec<(u64, i64, Option<f64>)> = (0..60).map(|d| (1u64, d, Some(20.0 + d as f64 / 4.0))).collect();
    rows.extend((0..60).map(|d| (2u64, d, Some(if d == 7 { off_grid } else { 8.5 + d as f64 }))));
    rows.sort_by_key(|r| (r.1, r.0));
    let mut s = Store::temporary("columns-floor-hot").unwrap();
    let v = s.ingest(CLOSE, &[priced(start, &rows)], "test", hot()).unwrap();
    assert!(s.is_hot(&v));
    assert!(s.hot(&v).unwrap().column(0, 1).raw(), "a raw exception in the published block");
    assert_eq!(s.least_value(&v, &RowFilter::default(), 2).unwrap(), Some(off_grid));
    none(&s, &le(0.0));
    none(&s, &lt(off_grid));
    assert_eq!(passing(&s, &view, whole, &le(off_grid), None), 1);
    // Appended rows in a tail, all above: still none. Then a zero in the
    // tail, which a read finds.
    let v = s.ingest(CLOSE, &[priced(start, &[(1, 60, Some(35.5)), (2, 60, Some(70.0))])], "test", hot()).unwrap();
    assert!(!s.hot(&v).unwrap().overlay.lock().unwrap().is_empty(), "appended rows");
    none(&s, &le(0.0));
    let v = s.ingest(CLOSE, &[priced(start, &[(2, 61, Some(0.0))])], "test", hot()).unwrap();
    assert_eq!(s.least_value(&v, &RowFilter::default(), 2).unwrap(), Some(0.0));
    assert_eq!(passing(&s, &view, whole, &le(0.0), None), 1);
    none(&s, &lt(0.0));
    // The zero corrected a day later: the superseded row is still a row of
    // the version, which a cut by receipt before the correction reads.
    let correction = RecordBatch::try_new(
        priced(start, &[]).schema(),
        vec![
            Arc::new(UInt64Array::from(vec![2u64])),
            Arc::new(TimestampNanosecondArray::from(vec![(start + 61) * DAY])),
            Arc::new(Float64Array::from(vec![71.0])),
            Arc::new(TimestampNanosecondArray::from(vec![(start + 62) * DAY])),
        ],
    )
    .unwrap();
    let v = s.ingest(CLOSE, &[correction], "test", hot()).unwrap();
    assert_eq!(s.least_value(&v, &RowFilter::default(), 2).unwrap(), Some(0.0));
    assert_eq!(passing(&s, &view, whole, &le(0.0), None), 0);
    assert_eq!(passing(&s, &view, whole, &le(0.0), Some((start + 61) * DAY + 2 * HOUR)), 1);
    // A sealed append block with a negative value among its 1,024 rows.
    let mut s = Store::temporary("columns-floor-sealed").unwrap();
    s.ingest(CLOSE, &[priced(start, &(0..40).map(|d| (5u64, d, Some(12.0 + d as f64))).collect::<Vec<_>>())], "test", hot())
        .unwrap();
    let mut v = String::new();
    for d0 in (40..1_240).step_by(50) {
        let batch: Vec<(u64, i64, Option<f64>)> = (d0..d0 + 50).map(|d| (5u64, d, Some(if d == 333 { -2.0 } else { 12.0 + d as f64 }))).collect();
        v = s.ingest(CLOSE, &[priced(start, &batch)], "test", hot()).unwrap();
    }
    {
        let h = s.hot(&v).unwrap();
        let ov = h.overlay.lock().unwrap();
        assert!(
            ov.instruments.iter().any(|i| !i.sealed.is_empty()) && ov.instruments.iter().any(|i| !i.tail.is_empty()),
            "a sealed block and a tail"
        );
    }
    assert_eq!(s.least_value(&v, &RowFilter::default(), 2).unwrap(), Some(-2.0));
    assert_eq!(passing(&s, &view, whole, &le(0.0), None), 1);
    assert_eq!(passing(&s, &view, whole, &le(-2.0), None), 1);
    none(&s, &lt(-2.0));
    // A value that is not a number passes no comparison, and a hot block
    // holding one is not bounded: the rows are read.
    let mut s = Store::temporary("columns-floor-nan").unwrap();
    let v = s
        .ingest(CLOSE, &[priced(start, &[(1, 0, Some(4.0)), (1, 1, Some(f64::NAN)), (1, 2, Some(5.0))])], "test", hot())
        .unwrap();
    assert!(s.is_hot(&v));
    assert_eq!(s.least_value(&v, &RowFilter::default(), 2).unwrap(), None);
    assert_eq!(passing(&s, &view, whole, &le(0.0), None), 0);
    assert_eq!(passing(&s, &view, whole, &le(4.5), None), 1);
    // A Parquet version: a null and a value that is not a number are left
    // out of a row group's statistics, as they pass no comparison; a month
    // of nulls alone holds no value; a month of nothing but a value that is
    // not a number is not bounded.
    let mut s = Store::temporary("columns-floor-cold").unwrap();
    let rows = [(1u64, 0i64, Some(3.0)), (2, 0, None), (3, 0, Some(f64::NAN)), (1, 1, Some(4.0)), (1, 40, None), (2, 40, None)];
    let v = s.ingest(CLOSE, &[priced(start, &rows)], "test", BTreeMap::new()).unwrap();
    assert!(!s.is_hot(&v));
    assert_eq!(s.manifest(&v).unwrap().partitions.len(), 2);
    assert_eq!(s.least_value(&v, &RowFilter::default(), 2).unwrap(), Some(3.0));
    none(&s, &le(0.0));
    none(&s, &lt(3.0));
    assert_eq!(passing(&s, &view, whole, &le(3.0), None), 1);
    assert_eq!(passing(&s, &view, whole, &le(1e9), None), 2);
    // The window's partitions alone are asked: the month of nulls holds
    // nothing a comparison could pass, whatever the constant.
    let late = ((start + 35) * DAY, (start + 45) * DAY);
    assert_eq!(
        s.least_value(
            &v,
            &RowFilter {
                t0: Some(late.0),
                t1: Some(late.1),
                ..RowFilter::default()
            },
            2
        )
        .unwrap(),
        Some(f64::INFINITY)
    );
    for form in asked(CLOSE, late, &le(1e9)) {
        kept_none(&s, &view, &form, None, false);
    }
    let v = s.ingest(CLOSE, &[priced(start, &[(1, 80, Some(f64::NAN))])], "test", BTreeMap::new()).unwrap();
    assert_eq!(s.least_value(&v, &RowFilter::default(), 2).unwrap(), None);
    assert_eq!(passing(&s, &view, whole, &le(0.0), None), 0);
    // A relation without a row holds no value.
    let mut s = Store::temporary("columns-floor-empty").unwrap();
    let v = s.ingest(CLOSE, &[priced(start, &[])], "test", BTreeMap::new()).unwrap();
    assert_eq!(s.manifest(&v).unwrap().rows, 0);
    assert_eq!(s.least_value(&v, &RowFilter::default(), 2).unwrap(), Some(f64::INFINITY));
    none(&s, &le(0.0));
    none(&s, &le(1e9));
}
