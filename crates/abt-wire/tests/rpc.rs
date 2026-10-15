//! The request and response exchange (P-2.3, P-2.4) over both transports: a
//! toy service answers the same requests with the same bytes in process and
//! over a Unix socket (Proposition P-5.1), streams a table until cancelled,
//! takes a table with `put`, and a client of another major version is
//! refused.

use std::sync::Arc;

use abt_wire::generated::abt::wire::{PutRequest, PutResponse, QueryForm, QueryRequest, RelationSchema, SchemaRequest, SchemaResponse, Section, StreamSpec, SubscribeRequest};
use abt_wire::rpc::{error, Client, Request, Responder, Service, WireError};
use abt_wire::transport::{bind, channel_pair, Connection};
use abt_wire::{ErrorCode, ErrorMessage, Layer, Message};
use arrow::array::{Int64Array, RecordBatch};
use arrow::datatypes::{DataType, Field, Schema, SchemaRef};

struct Toy;

fn table_schema() -> SchemaRef {
    Arc::new(Schema::new(vec![Field::new("x", DataType::Int64, false)]))
}

fn rows(from: i64, n: i64) -> RecordBatch {
    RecordBatch::try_new(table_schema(), vec![Arc::new(Int64Array::from((from..from + n).collect::<Vec<_>>()))]).unwrap()
}

impl Service for Toy {
    fn layer(&self) -> Layer {
        Layer::Data
    }

    fn handle(&self, request: Request, out: &Responder) -> Result<(), ErrorMessage> {
        let wire = |e: WireError| error(ErrorCode::Internal, e.to_string());
        match request.message {
            Message::SchemaRequest(r) => {
                let name = r.relation.unwrap_or_default();
                if name == "missing" {
                    return Err(error(ErrorCode::UnknownRelation, "no relation `missing`"));
                }
                let s = RelationSchema {
                    name,
                    class: "equity".into(),
                    ..Default::default()
                };
                out.respond(Message::SchemaResponse(Box::new(SchemaResponse { schemas: Some(vec![s]) }))).map_err(wire)
            }
            Message::QueryRequest(_) => out.table(&table_schema(), vec![rows(0, 3), rows(3, 2)]).map_err(wire),
            Message::SubscribeRequest(_) => {
                // An endless stream until the client cancels.
                let mut sink = out.begin_table(&table_schema(), None).map_err(wire)?;
                let mut i = 0;
                while !out.cancelled() {
                    sink.batch(&rows(i, 10)).map_err(wire)?;
                    i += 10;
                    std::thread::sleep(std::time::Duration::from_millis(1));
                }
                sink.end().map_err(wire)
            }
            Message::PutRequest(p) => {
                let (_, batches) = request.table.expect("a put carries a table");
                let n: usize = batches.iter().map(|b| b.num_rows()).sum();
                out.respond(Message::PutResponse(Box::new(PutResponse {
                    version: format!("{}:{}", p.relation, n),
                    seq_from: 1,
                    seq_to: n as u64,
                })))
                .map_err(wire)
            }
            _ => Err(error(ErrorCode::Malformed, "the toy does not take that")),
        }
    }
}

fn exercise(client: &Client) -> Vec<String> {
    let mut log = Vec::new();
    let r = client
        .request(Message::SchemaRequest(Box::new(SchemaRequest {
            class: None,
            relation: Some("close".into()),
        })))
        .unwrap();
    log.push(format!("{:?}", r));
    match client.request(Message::SchemaRequest(Box::new(SchemaRequest {
        class: None,
        relation: Some("missing".into()),
    }))) {
        Err(WireError::Remote(e)) => log.push(format!("error {:?} {:?}", e.code, e.message)),
        other => panic!("expected the service's error, got {:?}", other.map(|_| ())),
    }
    let q = QueryRequest {
        view: None,
        form: QueryForm::Section(Box::new(Section { relation: "close".into(), t: 0 })),
        receipt_filter: None,
        with_seq: false,
        explain: false,
    };
    let (schema, batches) = client.request_table(Message::QueryRequest(Box::new(q))).unwrap();
    log.push(format!("{:?} {:?}", schema.fields(), batches.iter().map(|b| b.num_rows()).collect::<Vec<_>>()));
    let put = Message::PutRequest(Box::new(PutRequest {
        relation: "kernel.run.fills".into(),
        schema: Default::default(),
        attributes: None,
    }));
    log.push(format!("{:?}", client.call_with_table(put, &table_schema(), &[rows(0, 7), rows(7, 5)]).unwrap().message().unwrap()));
    // A stream: read three batches, cancel, read to its end.
    let sub = SubscribeRequest {
        spec: Box::new(StreamSpec::default()),
        from_seq: 0,
        paced: false,
        to_seq: None,
        with_index: false,
    };
    let mut reply = client.call(Message::SubscribeRequest(Box::new(sub))).unwrap();
    assert!(matches!(reply.message().unwrap(), Message::TableFollows(_)));
    for _ in 0..3 {
        assert_eq!(reply.next_batch().unwrap().unwrap().num_rows(), 10);
    }
    reply.cancel().unwrap();
    while reply.next_batch().unwrap().is_some() {}
    log.push("stream cancelled".into());
    client.ping().unwrap();
    log
}

#[test]
fn the_same_exchange_in_process_and_over_a_socket() {
    let (ours, theirs) = channel_pair(16);
    std::thread::spawn(move || abt_wire::rpc::serve(theirs, Arc::new(Toy)));
    let inproc = exercise(&Client::connect(ours, Layer::Kernel, "test").unwrap());

    let dir = std::env::temp_dir().join(format!("abt-rpc-{}", std::process::id()));
    let path = dir.join("data.sock");
    let listener = bind(&path).unwrap();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let conn = Connection::socket(stream.unwrap()).unwrap();
            std::thread::spawn(move || abt_wire::rpc::serve(conn, Arc::new(Toy)));
        }
    });
    let socket = exercise(&Client::connect(Connection::connect(&path).unwrap(), Layer::Kernel, "test").unwrap());
    assert_eq!(inproc, socket);
    let mode = std::fs::metadata(&path).map(|m| std::os::unix::fs::PermissionsExt::mode(&m.permissions()) & 0o777).unwrap();
    assert_eq!(mode, 0o600, "a service's socket is its owner's only");
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn another_major_version_is_refused() {
    use abt_wire::convert::encode;
    use abt_wire::frame::{Frame, FrameKind};
    use abt_wire::Hello;
    let (mut ours, theirs) = channel_pair(16);
    let server = std::thread::spawn(move || abt_wire::rpc::serve(theirs, Arc::new(Toy)));
    // The service's hello, then ours from the future.
    let first = ours.rx.recv().unwrap().unwrap();
    assert_eq!(first.kind, FrameKind::Hello);
    let hello = Hello {
        protocol: "2.0".into(),
        layer: Layer::Front,
        client: None,
        capabilities: None,
    };
    ours.tx.send(&Frame::new(FrameKind::Hello, 0, encode(Message::Hello(Box::new(hello))))).unwrap();
    let refusal = ours.rx.recv().unwrap().unwrap();
    assert_eq!(refusal.kind, FrameKind::Error);
    match abt_wire::convert::decode(&refusal.payload).unwrap() {
        Message::Error(e) => assert_eq!(e.code, ErrorCode::Version),
        other => panic!("{:?}", other),
    }
    server.join().unwrap().unwrap();
}
