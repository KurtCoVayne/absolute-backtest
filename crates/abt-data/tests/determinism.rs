//! D-2.7 (a binding read at two times returns the same rows in the same
//! order) and D-4.2 (every query form is a deterministic function of the
//! view, `as_of_seq`, the receipt filter and its arguments), over the hot
//! store while it changes: appends that seal blocks, a consolidation, and
//! a revision that builds a new version, each between two readings of
//! every form at one sequence number.

mod hot_common;

use std::collections::BTreeMap;

use abt_data::query::{execute, ResolvedView};
use abt_data::Store;
use abt_wire::generated::abt::wire::{
    Atom, Binding, Constant, ConstantKind, CountRows, Disjunction, Distinct, Exists, Identity, Latest, Op, Point, Predicate, QueryForm, Scan, Section, Series, VersionRef, View,
};
use abt_wire::scalar::{NANOS_PER_DAY, NANOS_PER_HOUR};
use arrow::array::{Array, AsArray, RecordBatch};
use arrow::datatypes::DataType;
use hot_common::*;

const REL: &str = "equity.close";

fn entity(id: u64) -> Identity {
    Identity { values: vec![ent(id)] }
}

fn ent(id: u64) -> Constant {
    Constant {
        kind: ConstantKind::Entity,
        num: 0.0,
        int: id as i64,
        text: None,
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

fn atom(column: &str, op: Op, values: Vec<Constant>) -> Atom {
    Atom {
        column: column.to_string(),
        op,
        values: Some(values),
    }
}

fn pred(all: Vec<Vec<Atom>>) -> Option<Box<Predicate>> {
    Some(Box::new(Predicate {
        all: Some(all.into_iter().map(|atoms| Disjunction { atoms }).collect()),
    }))
}

/// The forms of D-4.1 over `ids` between `t0` and `t1`, with arguments
/// that reach the appended rows.
fn forms(ids: &[u64], t0: i64, t1: i64) -> Vec<(String, QueryForm)> {
    let p = || pred(vec![vec![atom("P", Op::Ge, vec![num(30.0)]), atom("A", Op::Eq, vec![ent(ids[0])])]]);
    let mut out = Vec::new();
    for (k, &id) in ids.iter().enumerate() {
        let t = t0 + (t1 - t0) * (k as i64 + 1) / (ids.len() as i64 + 1);
        let rel = || REL.to_string();
        out.push((
            format!("point {id} at {t}"),
            QueryForm::Point(Box::new(Point {
                relation: rel(),
                identity: Some(Box::new(entity(id))),
                t,
            })),
        ));
        out.push((
            format!("latest {id} at {t}"),
            QueryForm::Latest(Box::new(Latest {
                relation: rel(),
                identity: Some(Box::new(entity(id))),
                t_max: t,
            })),
        ));
        out.push((
            format!("series {id}"),
            QueryForm::Series(Box::new(Series {
                relation: rel(),
                identity: Some(Box::new(entity(id))),
                t0,
                t1,
            })),
        ));
        out.push((format!("section at {t}"), QueryForm::Section(Box::new(Section { relation: rel(), t }))));
    }
    out.push((
        "latest of every entity".into(),
        QueryForm::Latest(Box::new(Latest {
            relation: REL.into(),
            identity: None,
            t_max: t1,
        })),
    ));
    out.push((
        "scan with a predicate".into(),
        QueryForm::Scan(Box::new(Scan {
            relation: REL.into(),
            t0,
            t1,
            predicate: p(),
            columns: None,
        })),
    ));
    out.push((
        "scan projected".into(),
        QueryForm::Scan(Box::new(Scan {
            relation: REL.into(),
            t0,
            t1,
            predicate: None,
            columns: Some(vec!["A".into(), "P".into()]),
        })),
    ));
    out.push((
        "exists".into(),
        QueryForm::Exists(Box::new(Exists {
            relation: REL.into(),
            t0,
            t1,
            predicate: p(),
        })),
    ));
    out.push((
        "count".into(),
        QueryForm::CountRows(Box::new(CountRows {
            relation: REL.into(),
            t0,
            t1,
            predicate: p(),
        })),
    ));
    out.push((
        "distinct".into(),
        QueryForm::Distinct(Box::new(Distinct {
            relation: REL.into(),
            column: "A".into(),
            t0,
            t1,
        })),
    ));
    out
}

/// A scalar answer (`exists`) by its value; every other answer as rows
/// with their sequence numbers.
fn rendered(batches: &[RecordBatch]) -> String {
    match batches.first().map(|b| b.column(0).data_type()) {
        Some(DataType::Boolean) => format!("{}", batches[0].column(0).as_boolean().value(0)),
        _ => text_with_seq(batches),
    }
}

/// Every form's answer under `view`, without and with a receipt filter.
fn answers(s: &Store, view: &View, forms: &[(String, QueryForm)], r_max: i64) -> Vec<(String, String)> {
    let v = ResolvedView::of(s, Some(view)).unwrap();
    let mut out = Vec::new();
    for (name, form) in forms {
        for (tag, r) in [("", None), (", received by r_max", Some(r_max))] {
            let (_, batches) = execute(s, &v, form, r, true, false).unwrap();
            out.push((format!("{name}{tag}"), rendered(&batches)));
        }
    }
    out
}

fn assert_same_answers(what: &str, a: &[(String, String)], b: &[(String, String)]) {
    assert_eq!(a.len(), b.len());
    for ((n, x), (_, y)) in a.iter().zip(b) {
        assert_same(&format!("{what}: {n}"), x, y);
    }
}

fn view(version: Option<&str>, binding: Option<&str>, seq: u64) -> View {
    View {
        versions: version.map(|v| {
            vec![VersionRef {
                relation: REL.into(),
                version: v.to_string(),
            }]
        }),
        binding: binding.map(str::to_string),
        as_of_seq: Some(seq),
    }
}

#[test]
fn every_form_is_a_function_of_the_view_the_sequence_the_filter_and_its_arguments() {
    let mut rng = Rng(11);
    let ids = [3u64, 5, 8, 13, 21];
    let (start, days) = (18_000i64, 4_000i64);
    let rows = market(&mut rng, &ids, start, days);
    let split = (start + 1_100) * NANOS_PER_DAY;
    let base: Vec<(u64, i64, i64, f64)> = rows.iter().copied().filter(|r| r.1 < split).collect();
    let mut by_day: BTreeMap<i64, Vec<(u64, i64, i64, f64)>> = BTreeMap::new();
    for r in rows.iter().copied().filter(|r| r.1 >= split) {
        by_day.entry(r.1 / NANOS_PER_DAY).or_default().push(r);
    }
    let batches: Vec<Vec<(u64, i64, i64, f64)>> = by_day.into_values().collect();
    assert!(batches.len() > 2_000, "{} day batches", batches.len());
    assert!(ids.iter().all(|id| base.iter().any(|r| r.0 == *id)), "every instrument has published rows");
    let mut s = Store::temporary("det").unwrap();
    let va = s.ingest(REL, &[close_rows(&base)], "test", hot()).unwrap();
    // A third of the appends, then the sequence every reading is fixed at.
    let third = batches.len() / 3;
    for b in &batches[..third] {
        assert_eq!(s.ingest(REL, &[close_rows(b)], "test", hot()).unwrap(), va, "an append keeps the version");
    }
    let n0 = s.hot(&va).unwrap().seq_high();
    let (t0, t1) = ((start + 900) * NANOS_PER_DAY, (start + days) * NANOS_PER_DAY);
    let r_max = (start + 1_300) * NANOS_PER_DAY + 3 * NANOS_PER_HOUR;
    let forms = forms(&ids, t0, t1);
    let pinned = view(Some(&va), None, n0);
    let first = answers(&s, &pinned, &forms, r_max);
    assert!(
        first.iter().any(|(n, text)| n.starts_with("series") && text.lines().count() > 500),
        "the series reach the appended rows"
    );
    // D-2.7: the same readings through a binding of the version.
    let id = s
        .registry
        .record_binding(&Binding {
            pairs: Some(vec![VersionRef {
                relation: REL.into(),
                version: va.clone(),
            }]),
            point: None,
            dynamic: None,
            id: None,
        })
        .unwrap();
    let bound = view(None, Some(&id), n0);
    let first_bound = answers(&s, &bound, &forms, r_max);
    assert_same_answers("through the binding", &first, &first_bound);
    // The rest of the appends (tails seal into append files); every form
    // reread between them.
    for (i, b) in batches[third..].iter().enumerate() {
        assert_eq!(s.ingest(REL, &[close_rows(b)], "test", hot()).unwrap(), va);
        if i % 97 == 96 {
            assert_same_answers(&format!("after {} more appends", i + 1), &first, &answers(&s, &pinned, &forms, r_max));
        }
    }
    let sealed: usize = s.hot(&va).unwrap().overlay.lock().unwrap().instruments.iter().map(|i| i.sealed.len()).sum();
    assert!(sealed >= 2, "{sealed} blocks sealed");
    assert_same_answers("after every append", &first, &answers(&s, &pinned, &forms, r_max));
    assert_same_answers("the binding after every append", &first_bound, &answers(&s, &bound, &forms, r_max));
    // The appended rows are visible at a later sequence: the answer is a
    // function of `as_of_seq`.
    let later = answers(&s, &view(Some(&va), None, s.hot(&va).unwrap().seq_high()), &forms, r_max);
    assert!(first.iter().zip(&later).any(|((_, a), (_, b))| a != b), "later appends are visible at a later sequence");
    // Consolidation changes the layout, not an answer.
    let folded = s.consolidate(REL).unwrap();
    assert!(folded.is_some(), "the append files are folded");
    assert_same_answers("after consolidation", &first, &answers(&s, &pinned, &forms, r_max));
    assert_same_answers("the binding after consolidation", &first_bound, &answers(&s, &bound, &forms, r_max));
    // A revision builds a new version; the bound one reads as it did
    // (D-2.6, Proposition 7.1).
    let rev = (base[7].0, base[7].1, base[7].2 + 2 * NANOS_PER_DAY, base[7].3 + 1.0);
    let vb = s.ingest(REL, &[close_rows(&[rev])], "test", hot()).unwrap();
    assert_ne!(va, vb, "a revision is a new version");
    assert_same_answers("after a revision built a new version", &first, &answers(&s, &pinned, &forms, r_max));
    assert_same_answers("the binding after the new version", &first_bound, &answers(&s, &bound, &forms, r_max));
    // The arguments: a point an hour later is another answer where a bar
    // falls between (the forms are keyed by their arguments, not cached by
    // name).
    let at = |t: i64| {
        let (_, b) = execute(
            &s,
            &ResolvedView::of(&s, Some(&pinned)).unwrap(),
            &QueryForm::Point(Box::new(Point {
                relation: REL.into(),
                identity: Some(Box::new(entity(ids[0]))),
                t,
            })),
            None,
            true,
            false,
        )
        .unwrap();
        rendered(&b)
    };
    let t = base.iter().find(|r| r.0 == ids[0]).unwrap().1;
    assert_ne!(at(t), at(t - NANOS_PER_HOUR), "the point's time is an argument");
    assert_eq!(at(t), at(t), "and the same arguments give the same answer");
}
