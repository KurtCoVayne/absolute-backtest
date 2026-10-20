//! The `pipeline` class (D-2.3 amended, ERRATA E-94; absolute-pipeline
//! PL-7.2): five relations the gateway writes through `put` (P-3.1) like
//! the kernel's and the query layer's namespaces, registered with every
//! fresh store from the catalog, read back with `scan` and seen in the
//! catalog, over the in-process transport and over a socket alike.

use std::sync::Arc;

use abt_data::catalog::{builtin_schemas, classes, render_class};
use abt_data::service::{inproc, listen, DataService};
use abt_data::Store;
use abt_query::data::Data;
use abt_query::relation::{self, Col, Rows};
use abt_wire::generated::abt::wire::{ColumnType, RelationSchema, Role};
use abt_wire::transport::Connection;

const DAY: i64 = 86_400 * 1_000_000_000;

fn ts(days: i64) -> i64 {
    abt_wire::scalar::days_from_civil(2026, 10, 1) * DAY + days * DAY
}

/// A column: its name, type and role.
type Column = (&'static str, ColumnType, Role);

/// The columns of every pipeline relation as the class file declares
/// them, with the receipt time the store appends (D-4.2).
fn expected() -> Vec<(&'static str, Vec<Column>)> {
    use ColumnType::{Count, Label, Time};
    let r = ("r", Time, Role::ReceiptTime);
    vec![
        (
            "pipeline.document",
            vec![
                ("DocId", Label, Role::Label),
                ("Source", Label, Role::Label),
                ("Author", Label, Role::Label),
                ("P", Time, Role::EventTime),
                ("Size", Count, Role::Value),
                r,
            ],
        ),
        (
            "pipeline.hypothesis",
            vec![
                ("Id", Label, Role::Label),
                ("DocIds", Label, Role::Label),
                ("H", Time, Role::EventTime),
                ("Agent", Label, Role::Label),
                r,
            ],
        ),
        (
            "pipeline.submission",
            vec![
                ("Id", Label, Role::Label),
                ("HypothesisId", Label, Role::Label),
                ("ProgramHash", Label, Role::Label),
                ("PlanHash", Label, Role::Label),
                ("Lineage", Label, Role::Label),
                ("H", Time, Role::EventTime),
                r,
            ],
        ),
        (
            "pipeline.outcome",
            vec![
                ("SubmissionId", Label, Role::Label),
                ("T", Time, Role::EventTime),
                ("Reveal", Count, Role::Value),
                ("ForwardFrom", Time, Role::Value),
                ("ForwardTo", Time, Role::Value),
                ("Status", Label, Role::Label),
                r,
            ],
        ),
        (
            "pipeline.audit",
            vec![
                ("Job", Label, Role::Label),
                ("T", Time, Role::EventTime),
                ("Kind", Label, Role::Label),
                ("Subject", Label, Role::Label),
                ("Bytes", Count, Role::Value),
                ("DurationNs", Count, Role::Value),
                ("Outcome", Label, Role::Label),
                r,
            ],
        ),
    ]
}

fn columns(s: &RelationSchema) -> Vec<(&str, ColumnType, Role)> {
    s.columns.iter().map(|c| (c.name.as_str(), c.ty, c.role)).collect()
}

/// One row of each relation, tagged by the transport that writes it, as
/// the gateway would put them: the texts the registry interns as labels,
/// the counts, the times.
fn rows(tag: &str) -> Vec<Rows> {
    let label = |s: &str| Col::Label(vec![Some(s.to_string())]);
    let count = |n: i64| Col::Count(vec![Some(n)]);
    let time = |t: i64| Col::Time(vec![Some(t)]);
    vec![
        Rows::new("pipeline.document")
            .col("DocId", Role::Label, label(&format!("doc-{}", tag)))
            .col("Source", Role::Label, label("x"))
            .col("Author", Role::Label, label("quant_anon"))
            .col("P", Role::EventTime, time(ts(0)))
            .col("Size", Role::Value, count(1_234))
            .col("r", Role::ReceiptTime, time(ts(0) + 7)),
        Rows::new("pipeline.hypothesis")
            .col("Id", Role::Label, label(&format!("hyp-{}", tag)))
            .col("DocIds", Role::Label, label(&format!("[\"doc-{}\"]", tag)))
            .col("H", Role::EventTime, time(ts(1)))
            .col("Agent", Role::Label, label("analyst@v1"))
            .col("r", Role::ReceiptTime, time(ts(1) + 9)),
        Rows::new("pipeline.submission")
            .col("Id", Role::Label, label(&format!("sub-{}", tag)))
            .col("HypothesisId", Role::Label, label(&format!("hyp-{}", tag)))
            .col("ProgramHash", Role::Label, label("0000000000000000000000000000000000000000000000000000000000000001"))
            .col("PlanHash", Role::Label, label("0000000000000000000000000000000000000000000000000000000000000002"))
            .col("Lineage", Role::Label, label("0000000000000000000000000000000000000000000000000000000000000001"))
            .col("H", Role::EventTime, time(ts(1)))
            .col("r", Role::ReceiptTime, time(ts(2))),
        Rows::new("pipeline.outcome")
            .col("SubmissionId", Role::Label, label(&format!("sub-{}", tag)))
            .col("T", Role::EventTime, time(ts(9)))
            .col("Reveal", Role::Value, count(1))
            .col("ForwardFrom", Role::Value, time(ts(1)))
            .col("ForwardTo", Role::Value, time(ts(8)))
            .col("Status", Role::Label, label("forward"))
            .col("r", Role::ReceiptTime, time(ts(9))),
        Rows::new("pipeline.audit")
            .col("Job", Role::Label, label(&format!("job-{}", tag)))
            .col("T", Role::EventTime, time(ts(1) + 500))
            .col("Kind", Role::Label, label("plan_run"))
            .col("Subject", Role::Label, label("idea_breakout"))
            .col("Bytes", Role::Value, count(4_096))
            .col("DurationNs", Role::Value, count(2_500_000_000))
            .col("Outcome", Role::Label, label("ok"))
            .col("r", Role::ReceiptTime, time(ts(1) + 500)),
    ]
}

/// The rows put through `data` under the gateway's schemas, and read back
/// by `scan`: the ids by the tag, the counts and times as written.
fn put_and_read(data: &Data, tag: &str) {
    let schemas = data.schemas(Some("pipeline")).unwrap();
    for rows in rows(tag) {
        let (built, batch) = rows.finish().unwrap();
        // The schema the registry holds is the one written against: the
        // class's, not one the writer makes up.
        let registered = schemas.iter().find(|s| s.name == built.name).unwrap_or_else(|| panic!("{} is registered", built.name));
        assert_eq!(columns(registered), columns(&built), "{}: the rows are built as the class declares", built.name);
        data.put(registered.clone(), &batch, false).unwrap_or_else(|e| panic!("put {}: {}", built.name, e));
    }
    let doc = relation::read(data, "pipeline.document").unwrap().unwrap();
    let ids = relation::texts(&doc, "DocId").unwrap();
    assert!(ids.contains(&Some(format!("doc-{}", tag))), "{:?}", ids);
    let i = ids.iter().position(|d| d.as_deref() == Some(&format!("doc-{}", tag))).unwrap();
    assert_eq!(relation::texts(&doc, "Author").unwrap()[i].as_deref(), Some("quant_anon"));
    assert_eq!(relation::ints(&doc, "Size").unwrap()[i], Some(1_234));
    let hyp = relation::read(data, "pipeline.hypothesis").unwrap().unwrap();
    let j = relation::texts(&hyp, "Id").unwrap().iter().position(|d| d.as_deref() == Some(&format!("hyp-{}", tag))).unwrap();
    assert_eq!(relation::texts(&hyp, "DocIds").unwrap()[j].as_deref(), Some(format!("[\"doc-{}\"]", tag).as_str()));
    let sub = relation::read(data, "pipeline.submission").unwrap().unwrap();
    let k = relation::texts(&sub, "Id").unwrap().iter().position(|d| d.as_deref() == Some(&format!("sub-{}", tag))).unwrap();
    assert_eq!(relation::texts(&sub, "HypothesisId").unwrap()[k].as_deref(), Some(format!("hyp-{}", tag).as_str()));
    let out = relation::read(data, "pipeline.outcome").unwrap().unwrap();
    let l = relation::texts(&out, "SubmissionId")
        .unwrap()
        .iter()
        .position(|d| d.as_deref() == Some(&format!("sub-{}", tag)))
        .unwrap();
    assert_eq!(relation::ints(&out, "Reveal").unwrap()[l], Some(1));
    assert_eq!(relation::texts(&out, "Status").unwrap()[l].as_deref(), Some("forward"));
    let audit = relation::read(data, "pipeline.audit").unwrap().unwrap();
    let m = relation::texts(&audit, "Job").unwrap().iter().position(|d| d.as_deref() == Some(&format!("job-{}", tag))).unwrap();
    assert_eq!(relation::ints(&audit, "DurationNs").unwrap()[m], Some(2_500_000_000));
    assert_eq!(relation::texts(&audit, "Kind").unwrap()[m].as_deref(), Some("plan_run"));
}

#[test]
fn the_pipeline_class_is_in_the_catalog_with_its_five_relations() {
    let class = classes().into_iter().find(|c| c.name == "pipeline").expect("the pipeline class joins kernel and query");
    let names: Vec<&str> = class.groups.iter().flat_map(|g| &g.relations).map(|r| r.name).collect();
    assert_eq!(names, ["document", "hypothesis", "submission", "outcome", "audit"]);
    let file = std::fs::read_to_string(abt_tests::repo_root().join("corpus-v2/catalog/pipeline.dsl")).unwrap();
    assert_eq!(render_class(&class), file, "`abt data catalog --class pipeline` prints the class file");
    let schemas = builtin_schemas();
    for (name, cols) in expected() {
        let s = schemas.iter().find(|s| s.name == name).unwrap_or_else(|| panic!("{} is a built-in schema", name));
        assert_eq!(s.class, "pipeline");
        assert_eq!(columns(s), cols, "{}", name);
    }
}

#[test]
fn a_row_of_each_pipeline_relation_is_put_and_read_in_process_and_over_a_socket() {
    abt_tests::init();
    let store = Store::temporary(&format!("pipeline-{}", std::process::id())).unwrap();
    let svc: Arc<DataService> = DataService::new(store);
    // In process: the registry of a fresh store holds the class's schemas.
    let data = Data::connect(inproc(&svc)).unwrap();
    let mut registered: Vec<String> = data.schemas(Some("pipeline")).unwrap().iter().map(|s| s.name.clone()).collect();
    registered.sort();
    assert_eq!(registered, ["pipeline.audit", "pipeline.document", "pipeline.hypothesis", "pipeline.outcome", "pipeline.submission"]);
    put_and_read(&data, "inproc");
    // Over a socket: the same frames, the same rows, added beside the
    // first transport's.
    let path = std::env::temp_dir().join(format!("abt-pipeline-{}.sock", std::process::id()));
    let _server = listen(&path, svc.clone()).unwrap();
    let over_socket = Data::connect(Connection::connect(&path).unwrap()).unwrap();
    put_and_read(&over_socket, "socket");
    let doc = relation::read(&over_socket, "pipeline.document").unwrap().unwrap();
    let mut ids: Vec<String> = relation::texts(&doc, "DocId").unwrap().into_iter().flatten().collect();
    ids.sort();
    assert_eq!(ids, ["doc-inproc", "doc-socket"], "both transports' rows are in the current version");
    // A row without its receipt time is received at its event time
    // (D-4.2), and a count column left out is null, as for every put.
    let (schema, batch) = Rows::new("pipeline.audit")
        .col("Job", Role::Label, Col::Label(vec![Some("job-late".into())]))
        .col("T", Role::EventTime, Col::Time(vec![Some(ts(3))]))
        .col("Kind", Role::Label, Col::Label(vec![Some("query".into())]))
        .col("Subject", Role::Label, Col::Label(vec![Some("equity.close".into())]))
        .col("Bytes", Role::Value, Col::Count(vec![Some(10)]))
        .col("DurationNs", Role::Value, Col::Count(vec![Some(20)]))
        .col("Outcome", Role::Label, Col::Label(vec![Some("ok".into())]))
        .finish()
        .unwrap();
    let registered = over_socket.schemas(Some("pipeline")).unwrap().into_iter().find(|s| s.name == schema.name).unwrap();
    over_socket.put(registered, &batch, false).unwrap();
    let audit = relation::read(&data, "pipeline.audit").unwrap().unwrap();
    let jobs = relation::texts(&audit, "Job").unwrap();
    let n = jobs.iter().position(|j| j.as_deref() == Some("job-late")).unwrap();
    let r = audit.column_by_name("r").unwrap();
    let r = arrow::array::AsArray::as_primitive::<arrow::datatypes::TimestampNanosecondType>(r.as_ref());
    assert_eq!(r.value(n), ts(3));
    let _ = std::fs::remove_file(&path);
}
