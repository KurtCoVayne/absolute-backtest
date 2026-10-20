//! The data service (P-3.1) over the in-process channel and over a Unix
//! socket: keyed rows ingest with the registry's ids (D-2.4), the
//! transitional resolver binds a demand's relations, and every request's
//! answer (schema, query, explain, subscribe with its index, stream index,
//! put and get, aggregate, pin, stats) is the same over both transports
//! (Proposition P-5.1, G3 at the protocol level).

use std::collections::BTreeMap;
use std::sync::Arc;

use abt_data::adapter::{ingest_keyed, instruments, Security};
use abt_data::service::{inproc, listen, DataService};
use abt_data::Store;
use abt_wire::generated::abt::wire::{
    AggregateCall, AggregateRequest, ColumnSpec, ColumnType, Demand, GetRequest, Grouping, Identity, Layer, Message, Need, Numerics, PinRequest, Point, PutRequest, QueryForm, QueryRequest,
    RelationSchema, ResolveRequest, Role, SchemaRequest, Series, StatsRequest, StreamIndexRequest, StreamSpec, SubscribeRequest, VersionRef, View, WindowKind,
};
use abt_wire::rpc::Client;
use abt_wire::transport::Connection;
use arrow::array::{Float64Array, RecordBatch, StringArray, TimestampNanosecondArray};
use arrow::datatypes::{DataType, Field, Schema, TimeUnit};

const DAY: i64 = 86_400 * 1_000_000_000;
const T0: i64 = 19_000 * DAY;

fn close_rows(rows: &[(&str, i64, f64)]) -> RecordBatch {
    let schema = Arc::new(Schema::new(vec![
        Field::new("A", DataType::Utf8, false),
        Field::new("T", DataType::Timestamp(TimeUnit::Nanosecond, None), false),
        Field::new("P", DataType::Float64, false),
    ]));
    RecordBatch::try_new(
        schema,
        vec![
            Arc::new(StringArray::from(rows.iter().map(|r| r.0).collect::<Vec<_>>())),
            Arc::new(TimestampNanosecondArray::from(rows.iter().map(|r| r.1).collect::<Vec<_>>())),
            Arc::new(Float64Array::from(rows.iter().map(|r| r.2).collect::<Vec<_>>())),
        ],
    )
    .unwrap()
}

fn calendar_rows(labels: &[i64]) -> RecordBatch {
    let schema = Arc::new(Schema::new(vec![
        Field::new("calendar", DataType::Utf8, false),
        Field::new("label", DataType::Timestamp(TimeUnit::Nanosecond, None), false),
        Field::new("at", DataType::Timestamp(TimeUnit::Nanosecond, None), false),
    ]));
    RecordBatch::try_new(
        schema,
        vec![
            Arc::new(StringArray::from(labels.iter().map(|_| "1d").collect::<Vec<_>>())),
            Arc::new(TimestampNanosecondArray::from(labels.to_vec())),
            Arc::new(TimestampNanosecondArray::from(labels.iter().map(|l| l + DAY - 1).collect::<Vec<_>>())),
        ],
    )
    .unwrap()
}

/// Two names over ten days, the second renamed on day five, and a name the
/// security table does not hold.
fn service() -> Arc<DataService> {
    let mut s = Store::temporary("service").unwrap();
    let secs = vec![
        Security {
            key: "E1".into(),
            ticker: "AAA".into(),
            from: T0,
            to: None,
        },
        Security {
            key: "E2".into(),
            ticker: "BBB".into(),
            from: T0,
            to: Some(T0 + 5 * DAY),
        },
        Security {
            key: "E2".into(),
            ticker: "BBX".into(),
            from: T0 + 5 * DAY,
            to: None,
        },
    ];
    let mut rows = Vec::new();
    for d in 0..10 {
        rows.push(("E1", T0 + d * DAY, 10.0 + d as f64));
        rows.push(("E2", T0 + d * DAY, 20.0 + d as f64));
        rows.push(("E3", T0 + d * DAY, 30.0 + d as f64));
    }
    let got = ingest_keyed(&mut s, "test", &secs, &[], &[("equity.close".to_string(), close_rows(&rows))], "test", &BTreeMap::new()).unwrap();
    assert_eq!(got.ids.len(), 3);
    s.ingest("calendar_label", &[calendar_rows(&(0..10).map(|d| T0 + d * DAY).collect::<Vec<_>>())], "test", BTreeMap::new())
        .unwrap();
    DataService::new(s)
}

#[test]
fn keys_become_registry_ids_once() {
    let svc = service();
    let ins = instruments(&svc.store()).unwrap();
    assert_eq!(ins.iter().map(|i| (i.id, i.key.as_str())).collect::<Vec<_>>(), vec![(1, "E1"), (2, "E2"), (3, "E3")]);
    assert_eq!(ins[1].from, T0);
    // A second ingest of the same keys reuses the ids.
    let mut st = svc.store_mut();
    let again = ingest_keyed(
        &mut st,
        "test",
        &[],
        &[],
        &[("equity.close".to_string(), close_rows(&[("E3", T0 + 20 * DAY, 1.0), ("E4", T0 + 20 * DAY, 2.0)]))],
        "test",
        &BTreeMap::new(),
    )
    .unwrap();
    assert_eq!(again.ids["E3"], 3);
    assert_eq!(again.ids["E4"], 4);
}

fn client(conn: Connection) -> Client {
    Client::connect(conn, Layer::Kernel, "test").unwrap()
}

/// A table as text: its schema, then every row's cells.
fn render(batches: &[RecordBatch]) -> String {
    let mut out = String::new();
    for b in batches {
        out.push_str(&format!("{:?}\n", b.schema().fields().iter().map(|f| (f.name().clone(), f.data_type().clone())).collect::<Vec<_>>()));
        let opts = arrow::util::display::FormatOptions::default().with_null("null");
        // Times as their integers: a UTC zone name needs chrono-tz to print.
        let cols: Vec<arrow::array::ArrayRef> = b
            .columns()
            .iter()
            .map(|c| {
                if matches!(c.data_type(), DataType::Timestamp(_, _)) {
                    arrow::compute::cast(c, &DataType::Int64).unwrap()
                } else {
                    c.clone()
                }
            })
            .collect();
        let fmts: Vec<_> = cols.iter().map(|c| arrow::util::display::ArrayFormatter::try_new(c.as_ref(), &opts).unwrap()).collect();
        for i in 0..b.num_rows() {
            let cells: Vec<String> = fmts.iter().map(|f| f.value(i).to_string()).collect();
            out.push_str(&cells.join(" | "));
            out.push('\n');
        }
    }
    out
}

/// Every answer of a script of requests, as the client received it.
fn script(c: &Client) -> Vec<String> {
    let mut out = Vec::new();
    let show = |b: &[RecordBatch]| render(b);
    // Schemas.
    let Message::SchemaResponse(r) = c
        .request(Message::SchemaRequest(Box::new(SchemaRequest {
            class: Some("equity".into()),
            relation: None,
        })))
        .unwrap()
    else {
        panic!()
    };
    out.push(format!("{} schemas", r.schemas.unwrap().len()));
    // Resolve: the demand's relation at its current version, and identity.
    let demand = Demand {
        class: "equity".into(),
        needs: Some(vec![Need {
            relations: vec!["close".into()],
            calendar: None,
            from: None,
            to: None,
            scope: None,
            attributes: None,
            label: None,
            optional: false,
        }]),
        require: None,
        prefer: None,
        budget: None,
        dynamic: None,
        point: None,
    };
    let Message::ResolveResponse(r) = c
        .request(Message::ResolveRequest(Box::new(ResolveRequest {
            demand: Box::new(demand),
            dry_run: false,
        })))
        .unwrap()
    else {
        panic!()
    };
    let binding = r.binding.unwrap();
    let rels: Vec<String> = binding.pairs.clone().unwrap().iter().map(|p| p.relation.clone()).collect();
    assert_eq!(rels, vec!["equity.close", "instrument", "ticker", "vendor_id"]);
    let id = binding.id.clone().unwrap();
    out.push(format!("binding {}", id));
    // A query through the binding.
    let view = View {
        versions: None,
        binding: Some(id.clone()),
        as_of_seq: None,
    };
    let ent = |x: u64| Identity {
        values: vec![abt_wire::generated::abt::wire::Constant {
            kind: abt_wire::generated::abt::wire::ConstantKind::Entity,
            num: 0.0,
            int: x as i64,
            text: None,
        }],
    };
    let q = QueryRequest {
        view: Some(Box::new(view.clone())),
        form: QueryForm::Series(Box::new(Series {
            relation: "equity.close".into(),
            identity: Some(Box::new(ent(2))),
            t0: T0,
            t1: T0 + 3 * DAY,
        })),
        receipt_filter: None,
        with_seq: true,
        explain: false,
        ticks: false,
    };
    let (_, b) = c.request_table(Message::QueryRequest(Box::new(q.clone()))).unwrap();
    out.push(show(&b));
    let Message::ExplainResponse(e) = c.request(Message::QueryRequest(Box::new(QueryRequest { explain: true, ..q }))).unwrap() else {
        panic!()
    };
    out.push(format!("explain {} rows {} partitions", e.rows, e.partitions));
    let point = QueryRequest {
        view: Some(Box::new(view.clone())),
        form: QueryForm::Point(Box::new(Point {
            relation: "equity.close".into(),
            identity: Some(Box::new(ent(3))),
            t: T0 + 9 * DAY,
        })),
        receipt_filter: None,
        with_seq: false,
        explain: false,
        ticks: false,
    };
    let (_, b) = c.request_table(Message::QueryRequest(Box::new(point))).unwrap();
    out.push(show(&b));
    // A subscription with its index, in two halves.
    let close_v = binding.pairs.clone().unwrap().into_iter().find(|p| p.relation == "equity.close").unwrap().version;
    let spec = StreamSpec {
        relations: Some(vec![VersionRef {
            relation: "equity.close".into(),
            version: close_v.clone(),
        }]),
        binding: None,
        calendars: Some(vec!["1d".into()]),
        receipt_filter: None,
        clock: abt_wire::generated::abt::wire::ClockSource::Replay,
        session: None,
        entities: None,
    };
    let mut reply = c
        .call(Message::SubscribeRequest(Box::new(SubscribeRequest {
            spec: Box::new(spec.clone()),
            from_seq: 0,
            paced: false,
            to_seq: Some(20),
            with_index: true,
        })))
        .unwrap();
    let Message::SubscribeResponse(sr) = reply.message().unwrap() else { panic!() };
    out.push(format!("stream {}", sr.stream));
    let Message::TableFollows(_) = reply.message().unwrap() else { panic!() };
    let (_, ix) = reply.table().unwrap();
    out.push(show(&ix));
    let Message::TableFollows(_) = reply.message().unwrap() else { panic!() };
    let (_, ev) = reply.table().unwrap();
    out.push(show(&ev));
    let mut reply = c
        .call(Message::SubscribeRequest(Box::new(SubscribeRequest {
            spec: Box::new(spec.clone()),
            from_seq: 21,
            paced: false,
            to_seq: None,
            with_index: false,
        })))
        .unwrap();
    let Message::SubscribeResponse(_) = reply.message().unwrap() else { panic!() };
    let Message::TableFollows(_) = reply.message().unwrap() else { panic!() };
    let (_, rest) = reply.table().unwrap();
    out.push(show(&rest));
    let (_, ix2) = c.request_table(Message::StreamIndexRequest(Box::new(StreamIndexRequest { spec: Box::new(spec) }))).unwrap();
    assert_eq!(show(&ix), show(&ix2));
    // Put, then get the rows back.
    let schema = RelationSchema {
        name: "kernel.r1.book".into(),
        class: "kernel".into(),
        columns: vec![
            ColumnSpec {
                name: "t".into(),
                ty: ColumnType::Time,
                role: Role::EventTime,
                dimension: None,
            },
            ColumnSpec {
                name: "nav".into(),
                ty: ColumnType::Num,
                role: Role::Value,
                dimension: None,
            },
        ],
        calendar: None,
        constraints: None,
        attributes: None,
        schema_version: None,
    };
    let rows = RecordBatch::try_from_iter(vec![
        ("t", Arc::new(TimestampNanosecondArray::from(vec![T0, T0 + DAY])) as arrow::array::ArrayRef),
        ("nav", Arc::new(Float64Array::from(vec![1.0, 1.5])) as arrow::array::ArrayRef),
    ])
    .unwrap();
    let mut reply = c
        .call_with_table(
            Message::PutRequest(Box::new(PutRequest {
                relation: "kernel.r1.book".into(),
                schema: Box::new(schema),
                attributes: None,
                cache_key: None,
                replace: false,
            })),
            &rows.schema(),
            &[rows],
        )
        .unwrap();
    let Message::PutResponse(p) = reply.message().unwrap() else { panic!() };
    out.push(format!("put {} {}..{}", p.version, p.seq_from, p.seq_to));
    let (_, got) = c
        .request_table(Message::GetRequest(Box::new(GetRequest {
            cache_key: None,
            version: Some(p.version.clone()),
            relation: None,
        })))
        .unwrap();
    out.push(show(&got));
    // An aggregate, then its cache object by key.
    let req = AggregateRequest {
        relation: "equity.close".into(),
        column: "P".into(),
        second_column: None,
        function: "mean".into(),
        level: None,
        window: WindowKind::Rows,
        n: 3,
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
    };
    let mut reply = c
        .call(Message::AggregateCall(Box::new(AggregateCall {
            view: Some(Box::new(view)),
            request: Box::new(req),
        })))
        .unwrap();
    let Message::AggregateResponse(a) = reply.message().unwrap() else { panic!() };
    let Message::TableFollows(_) = reply.message().unwrap() else { panic!() };
    let (_, agg) = reply.table().unwrap();
    out.push(show(&agg));
    let (_, by_key) = c
        .request_table(Message::GetRequest(Box::new(GetRequest {
            cache_key: Some(a.cache_key.clone()),
            version: None,
            relation: None,
        })))
        .unwrap();
    assert_eq!(show(&agg), show(&by_key));
    // Pin the binding, then the statistics.
    let Message::Done(_) = c
        .request(Message::PinRequest(Box::new(PinRequest {
            version: None,
            binding: Some(id),
            pin: true,
        })))
        .unwrap()
    else {
        panic!()
    };
    let (_, stats) = c
        .request_table(Message::StatsRequest(Box::new(StatsRequest {
            relation: Some("equity.close".into()),
        })))
        .unwrap();
    out.push(show(&stats));
    out
}

#[test]
fn the_same_answers_in_process_and_over_a_socket() {
    let a = service();
    let inproc_answers = script(&client(inproc(&a)));
    let b = service();
    let path = std::env::temp_dir().join(format!("abt-data-service-{}.sock", std::process::id()));
    let _server = listen(&path, b).unwrap();
    let socket_answers = script(&client(Connection::connect(&path).unwrap()));
    assert_eq!(inproc_answers.len(), socket_answers.len());
    for (x, y) in inproc_answers.iter().zip(&socket_answers) {
        assert_eq!(x, y);
    }
    let _ = std::fs::remove_file(&path);
    // The answers are what they should be.
    assert!(
        inproc_answers[2].contains("20.0") && inproc_answers[2].contains("23.0") && !inproc_answers[2].contains("24.0"),
        "{}",
        inproc_answers[2]
    );
    assert!(inproc_answers[4].contains("39.0"), "{}", inproc_answers[4]);
    assert!(inproc_answers.iter().any(|s| s.starts_with("put ")));
}

#[test]
fn errors_carry_their_codes() {
    let svc = service();
    let c = client(inproc(&svc));
    let q = QueryRequest {
        view: None,
        form: QueryForm::Point(Box::new(Point {
            relation: "equity.nope".into(),
            identity: None,
            t: T0,
        })),
        receipt_filter: None,
        with_seq: false,
        explain: false,
        ticks: false,
    };
    match c.request_table(Message::QueryRequest(Box::new(q))) {
        Err(abt_wire::rpc::WireError::Remote(e)) => assert_eq!(e.code, abt_wire::ErrorCode::UnknownRelation),
        other => panic!("{:?}", other.map(|_| ())),
    }
    let v = View {
        versions: None,
        binding: Some("0".repeat(64)),
        as_of_seq: None,
    };
    let q = QueryRequest {
        view: Some(Box::new(v)),
        form: QueryForm::Point(Box::new(Point {
            relation: "equity.close".into(),
            identity: None,
            t: T0,
        })),
        receipt_filter: None,
        with_seq: false,
        explain: false,
        ticks: false,
    };
    match c.request_table(Message::QueryRequest(Box::new(q))) {
        Err(abt_wire::rpc::WireError::Remote(e)) => assert_eq!(e.code, abt_wire::ErrorCode::NotFound),
        other => panic!("{:?}", other.map(|_| ())),
    }
}

/// E-110: a schema listing without a class leaves out the relations the
/// layers derive (a cache object here; a run's outputs and the query layer's
/// records alike), which a day of runs made pass the control payload's
/// 1 MiB and refuse every binding; asked by name or by class, a derived
/// relation's schema is answered.
#[test]
fn a_classless_schema_listing_leaves_derived_relations_out() {
    let svc = service();
    let name = {
        let mut st = svc.store_mut();
        let schema = st.schema("calendar_label").unwrap();
        st.cache_put(&abt_wire::Hash::of(b"an aggregate"), &schema, &[calendar_rows(&[T0])]).unwrap();
        st.registry.schemas(None).into_iter().map(|s| s.name).find(|n| n.starts_with("cache.")).expect("the cache object's relation")
    };
    let c = client(inproc(&svc));
    let names = |class: Option<&str>, relation: Option<&str>| -> Vec<String> {
        let Message::SchemaResponse(r) = c
            .request(Message::SchemaRequest(Box::new(SchemaRequest {
                class: class.map(str::to_string),
                relation: relation.map(str::to_string),
            })))
            .unwrap()
        else {
            panic!("not a schema response")
        };
        r.schemas.unwrap_or_default().into_iter().map(|s| s.name).collect()
    };
    let all = names(None, None);
    assert!(all.contains(&"equity.close".to_string()), "{:?}", all);
    assert!(!all.iter().any(|n| n.starts_with("cache.") || n.starts_with("kernel.") || n.starts_with("query.")), "{:?}", all);
    assert_eq!(names(None, Some(&name)), vec![name.clone()]);
    assert_eq!(names(Some("cache"), None), vec![name]);
}
