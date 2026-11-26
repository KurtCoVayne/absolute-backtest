//! The request and response exchange (P-2.3, P-2.4) over both transports: a
//! toy service answers the same requests with the same bytes in process and
//! over a Unix socket (Proposition P-5.1), streams a table until cancelled,
//! takes a table with `put`, and a client of another major version is
//! refused. An answer held unread holds back no other answer on its
//! connection; a subscription sends no more than its window of events
//! unacknowledged, the client acknowledging each batch as it is taken, and
//! one dropped before its end is cancelled (D-5.6, ERRATA E-64).

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use abt_wire::generated::abt::wire::{PutRequest, PutResponse, QueryForm, QueryRequest, RelationSchema, SchemaRequest, SchemaResponse, Section, StreamSpec, SubscribeRequest, SubscribeResponse};
use abt_wire::rpc::{error, Client, Request, Responder, Service, WireError};
use abt_wire::transport::{bind, channel_pair, Connection};
use abt_wire::{ErrorCode, ErrorMessage, Layer, Message};
use arrow::array::{Int64Array, RecordBatch, UInt64Array};
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
        ticks: false,
    };
    let (schema, batches) = client.request_table(Message::QueryRequest(Box::new(q))).unwrap();
    log.push(format!("{:?} {:?}", schema.fields(), batches.iter().map(|b| b.num_rows()).collect::<Vec<_>>()));
    let put = Message::PutRequest(Box::new(PutRequest {
        relation: "kernel.run.fills".into(),
        schema: Default::default(),
        attributes: None,
        cache_key: None,
        replace: false,
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

fn subscription() -> Message {
    Message::SubscribeRequest(Box::new(SubscribeRequest {
        spec: Box::new(StreamSpec::default()),
        from_seq: 0,
        paced: false,
        to_seq: None,
        with_index: false,
    }))
}

fn schema_request() -> Message {
    Message::SchemaRequest(Box::new(SchemaRequest {
        class: None,
        relation: Some("close".into()),
    }))
}

/// Waits up to ten seconds for `what`.
fn eventually(what: &str, f: impl Fn() -> bool) {
    let start = Instant::now();
    while !f() {
        assert!(start.elapsed() < Duration::from_secs(10), "never: {}", what);
        std::thread::sleep(Duration::from_millis(2));
    }
}

#[test]
fn an_answer_held_unread_holds_back_no_other() {
    let (ours, theirs) = channel_pair(16);
    std::thread::spawn(move || abt_wire::rpc::serve(theirs, Arc::new(Toy)));
    let client = Arc::new(Client::connect(ours, Layer::Kernel, "test").unwrap());
    let mut stream = client.call(subscription()).unwrap();
    assert!(matches!(stream.message().unwrap(), Message::TableFollows(_)));
    // The toy sends a batch a millisecond; none is read for long past what
    // the connection holds in flight.
    std::thread::sleep(Duration::from_millis(300));
    // Asked on a thread of its own, so that a reader stuck behind the stream
    // fails the test rather than hanging it.
    let (tx, rx) = std::sync::mpsc::channel();
    let asker = client.clone();
    std::thread::spawn(move || {
        let _ = tx.send(asker.request(schema_request()).is_ok());
    });
    let answered = rx.recv_timeout(Duration::from_secs(30));
    assert_eq!(answered, Ok(true), "the schema request is answered while the stream waits unread");
    stream.cancel().unwrap();
    while stream.next_batch().unwrap().is_some() {}
}

/// A toy whose subscription is a numbered stream through D-5.6's window:
/// batches of five events, at most `window` of them unacknowledged.
struct Windowed {
    window: u64,
    sent: Arc<AtomicU64>,
    ended: Arc<AtomicBool>,
}

fn seq_schema() -> SchemaRef {
    Arc::new(Schema::new(vec![Field::new("seq", DataType::UInt64, false)]))
}

impl Service for Windowed {
    fn layer(&self) -> Layer {
        Layer::Data
    }

    fn handle(&self, request: Request, out: &Responder) -> Result<(), ErrorMessage> {
        let wire = |e: WireError| error(ErrorCode::Internal, e.to_string());
        match request.message {
            Message::SubscribeRequest(_) => {
                out.respond(Message::SubscribeResponse(Box::new(SubscribeResponse { stream: "toy".into() }))).map_err(wire)?;
                let mut sink = out.begin_table(&seq_schema(), None).map_err(wire)?;
                let mut seq = 0;
                while seq < 500 && !out.cancelled() {
                    if seq + 5 > self.window && !out.wait_acked(seq + 5 - self.window) {
                        break;
                    }
                    let b = RecordBatch::try_new(seq_schema(), vec![Arc::new(UInt64Array::from((seq + 1..=seq + 5).collect::<Vec<_>>()))]).unwrap();
                    sink.batch(&b).map_err(wire)?;
                    seq += 5;
                    self.sent.store(seq, Ordering::SeqCst);
                }
                self.ended.store(true, Ordering::SeqCst);
                sink.end().map_err(wire)
            }
            Message::SchemaRequest(_) => out.respond(Message::SchemaResponse(Box::new(SchemaResponse { schemas: None }))).map_err(wire),
            _ => Err(error(ErrorCode::Malformed, "the toy does not take that")),
        }
    }
}

#[test]
fn a_subscription_waits_for_its_acks_and_a_dropped_one_ends() {
    let sent = Arc::new(AtomicU64::new(0));
    let ended = Arc::new(AtomicBool::new(false));
    let (ours, theirs) = channel_pair(16);
    let toy = Windowed {
        window: 20,
        sent: sent.clone(),
        ended: ended.clone(),
    };
    std::thread::spawn(move || abt_wire::rpc::serve(theirs, Arc::new(toy)));
    let client = Client::connect(ours, Layer::Kernel, "test").unwrap();
    let mut reply = client.call(subscription()).unwrap();
    assert!(matches!(reply.message().unwrap(), Message::SubscribeResponse(_)));
    assert!(matches!(reply.message().unwrap(), Message::TableFollows(_)));
    // Unread, the stream stops at its window.
    eventually("the window sent", || sent.load(Ordering::SeqCst) == 20);
    std::thread::sleep(Duration::from_millis(100));
    assert_eq!(sent.load(Ordering::SeqCst), 20, "no event past the window before an ack");
    // A batch taken is acknowledged, and lets the next one through.
    let first = reply.next_batch().unwrap().unwrap();
    assert_eq!(first.column(0).as_any().downcast_ref::<UInt64Array>().unwrap().values(), &[1, 2, 3, 4, 5]);
    eventually("one batch more", || sent.load(Ordering::SeqCst) == 25);
    std::thread::sleep(Duration::from_millis(100));
    assert_eq!(sent.load(Ordering::SeqCst), 25);
    // The connection answers while the stream waits.
    assert!(client.request(schema_request()).is_ok());
    // Read to its end, every event arrives once and in order.
    let mut seen = 5;
    for _ in 0..10 {
        let b = reply.next_batch().unwrap().unwrap();
        let col = b.column(0).as_any().downcast_ref::<UInt64Array>().unwrap();
        for v in col.values() {
            seen += 1;
            assert_eq!(*v, seen);
        }
    }
    // Dropped before its end, it is cancelled: the service stops waiting.
    drop(reply);
    eventually("the stream ended", || ended.load(Ordering::SeqCst));
    assert!(sent.load(Ordering::SeqCst) < 500);
}

/// An aggregate request as protocol 1.2 wrote it: every field 1.2 knew.
fn request_1_2() -> abt_wire::generated::abt::wire::AggregateRequest {
    use abt_wire::generated::abt::wire::{AggregateRequest, Duration as CalendarDuration, Grouping, Numerics, ReceiptFilter, WindowKind};
    AggregateRequest {
        relation: "equity.close".into(),
        column: "P".into(),
        second_column: None,
        function: "quantile".into(),
        level: Some(0.25),
        window: WindowKind::Bucket,
        n: 0,
        min: Some(1),
        span: Some(Box::new(CalendarDuration { months: 1, days: -2, nanos: 3 })),
        bucket: Some("weekly".into()),
        group: Grouping::PerEntity,
        group_column: None,
        receipt_filter: Some(Box::new(ReceiptFilter { r_max: Some(-7) })),
        numerics: Numerics::TwoPass,
        calendar: Some("base".into()),
        t0: None,
        t1: Some(1_700_000_000_000_000_000),
        output: None,
        ..Default::default()
    }
}

/// A stream spec as protocol 1.2 wrote it.
fn spec_1_2() -> StreamSpec {
    use abt_wire::generated::abt::wire::{ClockSource, VersionRef};
    StreamSpec {
        relations: Some(vec![
            VersionRef {
                relation: "equity.close".into(),
                version: "ab12".into(),
            },
            VersionRef {
                relation: "kernel.r.calendar_label".into(),
                version: "cd34".into(),
            },
        ]),
        binding: None,
        calendars: Some(vec!["base".into(), "weekly".into()]),
        receipt_filter: None,
        clock: ClockSource::Replay,
        session: None,
        entities: Some(vec![3, 1]),
        ..Default::default()
    }
}

/// Protocol 1.3 adds optional fields (P-7, ERRATA E-155). The canonical
/// forms of a request, a call, a response and a stream spec that do not set
/// them are the bytes 1.2 rendered (pinned here from a 1.2 tree), so every
/// cache key and stream id stands; set, the new fields are in the canonical
/// form and round trip through a control payload.
#[test]
fn a_1_2_aggregate_request_and_stream_spec_keep_their_canonical_bytes() {
    use abt_wire::canonical::Canonical;
    use abt_wire::convert::{decode, encode};
    use abt_wire::generated::abt::wire::{AggregateCall, AggregateResponse, Factor, RunsCall, RunsRequest};
    const REQUEST: &str = r#"{"bucket":"weekly","calendar":"base","column":"P","function":"quantile","group":"PerEntity","group_column":null,"level":0.25,"min":1,"n":0,"numerics":"TwoPass","output":null,"receipt_filter":{"r_max":-7},"relation":"equity.close","second_column":null,"span":{"days":-2,"months":1,"nanos":3},"t0":null,"t1":1700000000000000000,"window":"Bucket"}"#;
    let request = request_1_2();
    assert_eq!(request.canonical().unwrap(), REQUEST);
    let call = AggregateCall {
        view: None,
        request: Box::new(request.clone()),
        answer: true,
    };
    assert_eq!(call.canonical().unwrap(), format!(r#"{{"request":{},"view":null}}"#, REQUEST));
    let response = AggregateResponse {
        cache_key: "00ff".into(),
        ..Default::default()
    };
    assert_eq!(response.canonical().unwrap(), r#"{"cache_key":"00ff"}"#);
    let spec = spec_1_2();
    assert_eq!(
        spec.canonical().unwrap(),
        r#"{"binding":null,"calendars":["base","weekly"],"clock":"Replay","entities":[3,1],"receipt_filter":null,"relations":[{"relation":"equity.close","version":"ab12"},{"relation":"kernel.r.calendar_label","version":"cd34"}],"session":null}"#
    );
    // A hash over a request renders the request itself: a map holding it
    // keeps the omission (the cache key's form, D-3.5).
    let versions = vec![("equity.close".to_string(), "ab12".to_string())];
    let keyed = KeyForm {
        aggregate: &request,
        versions: &versions,
    };
    assert_eq!(keyed.canonical().unwrap(), format!(r#"{{"aggregate":{},"versions":[["equity.close","ab12"]]}}"#, REQUEST));

    // The new fields, set: in the canonical form, and through the wire.
    let mut scoped = request.clone();
    scoped.factor = Some(Box::new(Factor {
        relation: "equity.volume".into(),
        column: "V".into(),
    }));
    scoped.entities = Some(vec![7, 2]);
    let text = scoped.canonical().unwrap();
    assert!(
        text.contains(r#""entities":[7,2]"#) && text.contains(r#""factor":{"column":"V","relation":"equity.volume"}"#),
        "{}",
        text
    );
    assert_ne!(scoped.canonical_hash().unwrap(), request.canonical_hash().unwrap());
    let meta = AggregateCall {
        view: None,
        request: Box::new(scoped.clone()),
        answer: false,
    };
    assert!(meta.canonical().unwrap().contains(r#""answer":false"#));
    let answered = AggregateResponse {
        cache_key: "00ff".into(),
        relation: Some("cache.00ff".into()),
        version: Some("v".into()),
        rows: Some(12),
        inexact_rows: Some(0),
    };
    assert_eq!(
        answered.canonical().unwrap(),
        r#"{"cache_key":"00ff","inexact_rows":0,"relation":"cache.00ff","rows":12,"version":"v"}"#
    );
    let mut bounded = spec.clone();
    bounded.from = Some(-1);
    bounded.whole = Some(vec!["equity.member".into()]);
    assert!(bounded.canonical().unwrap().ends_with(r#""from":-1,"receipt_filter":null,"relations":[{"relation":"equity.close","version":"ab12"},{"relation":"kernel.r.calendar_label","version":"cd34"}],"session":null,"whole":["equity.member"]}"#));
    let runs = RunsCall {
        view: None,
        request: Box::new(RunsRequest {
            relation: "equity.member".into(),
            calendar: "base".into(),
            entities: Some(vec![1]),
            t0: None,
            t1: Some(5),
        }),
    };
    for m in [
        Message::AggregateCall(Box::new(meta)),
        Message::AggregateCall(Box::new(call)),
        Message::AggregateResponse(Box::new(answered)),
        Message::AggregateResponse(Box::new(response)),
        Message::SubscribeRequest(Box::new(SubscribeRequest {
            spec: Box::new(bounded),
            from_seq: 0,
            paced: false,
            to_seq: None,
            with_index: false,
        })),
        Message::RunsCall(Box::new(runs)),
    ] {
        assert_eq!(decode(&encode(m.clone())).unwrap(), m);
    }
}

/// The cache key's form: the request and the versions it reads.
#[derive(serde::Serialize)]
struct KeyForm<'a> {
    aggregate: &'a abt_wire::generated::abt::wire::AggregateRequest,
    versions: &'a [(String, String)],
}
