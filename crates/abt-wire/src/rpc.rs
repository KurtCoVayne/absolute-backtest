//! Requests and responses over a connection (P-2.3, P-2.4): every service
//! and every client of the three layers speaks through this. Each side opens
//! with `hello` and a different major version is refused; requests carry a
//! correlation id and may be pipelined; a response carries its request's id;
//! a table follows its control response as schema, batch and end frames;
//! `cancel` stops a streaming response, which then ends; `ping` is answered
//! with `pong`. What a request does is the service's (each layer's
//! dispatch); this module only moves it.
//!
//! A client's reader never waits on one answer: every frame goes to its
//! request at once, so answers interleave (P-2.3) whatever the client reads
//! first, and an answer held unread never holds back another. A
//! subscription's events are flow-controlled at the service instead, by the
//! client's `ack`s (P-2.4, D-5.6; ERRATA E-64).

use std::collections::HashMap;
use std::fmt;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Condvar, Mutex};

use arrow::array::RecordBatch;
use arrow::datatypes::SchemaRef;
use arrow::error::ArrowError;

use crate::convert::{decode, encode};
use crate::frame::{Frame, FrameError, FrameKind};
use crate::table::{TableDecoder, TableEncoder};
use crate::transport::{Connection, FrameTx};
use crate::{ErrorCode, ErrorMessage, Hello, Layer, Message, PROTOCOL};

#[derive(Debug)]
pub enum WireError {
    Frame(FrameError),
    Decode(String),
    Arrow(ArrowError),
    /// The peer answered with an error frame.
    Remote(ErrorMessage),
    /// The connection closed before the answer was whole.
    Closed,
    /// A frame the exchange did not expect.
    Protocol(String),
}

impl fmt::Display for WireError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            WireError::Frame(e) => write!(f, "{}", e),
            WireError::Decode(e) => write!(f, "{}", e),
            WireError::Arrow(e) => write!(f, "{}", e),
            WireError::Remote(e) => write!(f, "{:?}: {}", e.code, e.message.as_deref().unwrap_or("")),
            WireError::Closed => write!(f, "the connection closed"),
            WireError::Protocol(m) => write!(f, "protocol error: {}", m),
        }
    }
}

impl std::error::Error for WireError {}

impl From<FrameError> for WireError {
    fn from(e: FrameError) -> Self {
        WireError::Frame(e)
    }
}

impl From<ArrowError> for WireError {
    fn from(e: ArrowError) -> Self {
        WireError::Arrow(e)
    }
}

impl From<crate::convert::DecodeError> for WireError {
    fn from(e: crate::convert::DecodeError) -> Self {
        WireError::Decode(e.0)
    }
}

/// An error message with its code.
pub fn error(code: ErrorCode, message: impl Into<String>) -> ErrorMessage {
    ErrorMessage {
        code,
        message: Some(message.into()),
        need: None,
        over_budget: None,
    }
}

/// The writer half of a connection, shared by every request answered on it.
#[derive(Clone)]
struct Writer(Arc<Mutex<Box<dyn FrameTx>>>);

impl Writer {
    fn send(&self, f: Frame) -> Result<(), WireError> {
        self.0.lock().map_err(|_| WireError::Closed)?.send(&f).map_err(WireError::Frame)
    }
}

fn hello(layer: Layer, client: &str) -> Message {
    Message::Hello(Box::new(Hello {
        protocol: PROTOCOL.to_string(),
        layer,
        client: Some(client.to_string()),
        capabilities: None,
    }))
}

/// The major version of a protocol string.
fn major(v: &str) -> &str {
    v.split('.').next().unwrap_or(v)
}

/// The peer's hello, checked against ours (P-2.3).
fn read_hello(f: Option<Frame>) -> Result<Hello, WireError> {
    let f = f.ok_or(WireError::Closed)?;
    if f.kind == FrameKind::Error {
        if let Message::Error(e) = decode(&f.payload)? {
            return Err(WireError::Remote(*e));
        }
    }
    if f.kind != FrameKind::Hello {
        return Err(WireError::Protocol(format!("a session opens with hello, not {:?}", f.kind)));
    }
    match decode(&f.payload)? {
        Message::Hello(h) => Ok(*h),
        other => Err(WireError::Protocol(format!("a hello frame carried {:?}", std::mem::discriminant(&other)))),
    }
}

/// What the client has said of one request's answer: whether it cancelled
/// it, and the greatest seq it acknowledged.
#[derive(Default)]
struct Flow {
    cancelled: AtomicBool,
    acked: Mutex<u64>,
    moved: Condvar,
}

impl Flow {
    fn cancel(&self) {
        self.cancelled.store(true, Ordering::SeqCst);
        // Under the lock, so that a waiter between its check and its wait
        // still sees the change.
        let _held = self.acked.lock();
        self.moved.notify_all();
    }

    fn ack(&self, seq: u64) {
        if let Ok(mut a) = self.acked.lock() {
            *a = (*a).max(seq);
        }
        self.moved.notify_all();
    }
}

/// What a service's handler answers through.
pub struct Responder {
    w: Writer,
    correlation: u64,
    flow: Arc<Flow>,
}

impl Responder {
    pub fn correlation(&self) -> u64 {
        self.correlation
    }

    /// Whether the client cancelled this request (a streaming answer then
    /// ends at its next batch). A closed connection cancels every request.
    pub fn cancelled(&self) -> bool {
        self.flow.cancelled.load(Ordering::SeqCst)
    }

    /// The greatest seq the client acknowledged (D-5.6), 0 before any.
    pub fn acked(&self) -> u64 {
        self.flow.acked.lock().map(|a| *a).unwrap_or(0)
    }

    /// Waits until the client acknowledges `seq`: `true` once it has,
    /// `false` if the request is cancelled first.
    pub fn wait_acked(&self, seq: u64) -> bool {
        let Ok(mut a) = self.flow.acked.lock() else { return false };
        loop {
            if *a >= seq {
                return true;
            }
            if self.cancelled() {
                return false;
            }
            a = match self.flow.moved.wait(a) {
                Ok(a) => a,
                Err(_) => return false,
            };
        }
    }

    /// A control response.
    pub fn respond(&self, m: Message) -> Result<(), WireError> {
        self.w.send(Frame::new(FrameKind::Response, self.correlation, encode(m)))
    }

    /// The request's error.
    pub fn error(&self, e: ErrorMessage) -> Result<(), WireError> {
        self.w.send(Frame::new(FrameKind::Error, self.correlation, encode(Message::Error(Box::new(e)))))
    }

    /// A whole table: `TableFollows`, its schema, its batches, `end`.
    pub fn table(&self, schema: &SchemaRef, batches: impl IntoIterator<Item = RecordBatch>) -> Result<(), WireError> {
        let mut sink = self.begin_table(schema, None)?;
        for b in batches {
            sink.batch(&b)?;
        }
        sink.end()
    }

    /// A table sent batch by batch (a stream); `rows` when known.
    pub fn begin_table(&self, schema: &SchemaRef, rows: Option<u64>) -> Result<TableSink<'_>, WireError> {
        self.respond(Message::TableFollows(Box::new(crate::generated::abt::wire::TableFollows { rows })))?;
        let (enc, head) = TableEncoder::new(schema)?;
        self.w.send(Frame::new(FrameKind::Schema, self.correlation, head))?;
        Ok(TableSink { r: self, enc })
    }
}

/// A table being sent.
pub struct TableSink<'a> {
    r: &'a Responder,
    enc: TableEncoder,
}

impl TableSink<'_> {
    pub fn batch(&mut self, b: &RecordBatch) -> Result<(), WireError> {
        for payload in self.enc.batch(b)? {
            self.r.w.send(Frame::new(FrameKind::Batch, self.r.correlation, payload))?;
        }
        Ok(())
    }

    pub fn end(self) -> Result<(), WireError> {
        self.r.w.send(Frame::new(FrameKind::End, self.r.correlation, Vec::new()))
    }
}

/// A request as a service receives it: the message and, for a request that
/// carries rows (`put`), the table that followed it.
pub struct Request {
    pub message: Message,
    pub table: Option<(SchemaRef, Vec<RecordBatch>)>,
}

/// A layer's service: what each request does.
pub trait Service: Send + Sync + 'static {
    fn layer(&self) -> Layer;
    /// Answers one request through `out`; an `Err` is sent as its error.
    fn handle(&self, request: Request, out: &Responder) -> Result<(), ErrorMessage>;
}

/// Serves one connection until the peer closes it: the hello exchange, then
/// every request on its own thread, so that a long answer (a subscription)
/// neither blocks the connection's other requests nor its cancellation.
pub fn serve(conn: Connection, service: Arc<dyn Service>) -> Result<(), WireError> {
    let Connection { tx, mut rx } = conn;
    let w = Writer(Arc::new(Mutex::new(tx)));
    w.send(Frame::new(FrameKind::Hello, 0, encode(hello(service.layer(), "abt"))))?;
    let peer = read_hello(rx.recv()?)?;
    if major(&peer.protocol) != major(PROTOCOL) {
        let e = error(ErrorCode::Version, format!("protocol {} is not {}", peer.protocol, PROTOCOL));
        w.send(Frame::new(FrameKind::Error, 0, encode(Message::Error(Box::new(e)))))?;
        return Ok(());
    }
    let flows: Arc<Mutex<HashMap<u64, Arc<Flow>>>> = Arc::default();
    let mut tables: HashMap<u64, (Message, TableDecoder, Vec<RecordBatch>)> = HashMap::new();
    let mut handlers = Vec::new();
    let spawn = |correlation: u64, request: Request, handlers: &mut Vec<std::thread::JoinHandle<()>>| {
        let flow = Arc::new(Flow::default());
        flows.lock().expect("the flow table").insert(correlation, flow.clone());
        let (w, service, flows) = (w.clone(), service.clone(), flows.clone());
        handlers.push(std::thread::spawn(move || {
            let out = Responder { w, correlation, flow };
            if let Err(e) = service.handle(request, &out) {
                let _ = out.error(e);
            }
            flows.lock().expect("the flow table").remove(&correlation);
        }));
    };
    let flow_of = |correlation: u64| flows.lock().expect("the flow table").get(&correlation).cloned();
    while let Some(f) = rx.recv()? {
        match f.kind {
            FrameKind::Ping => w.send(Frame::new(FrameKind::Pong, f.correlation, Vec::new()))?,
            FrameKind::Pong => {}
            FrameKind::Ack => match decode(&f.payload) {
                Ok(Message::AckMessage(a)) => {
                    if let Some(flow) = flow_of(f.correlation) {
                        flow.ack(a.seq);
                    }
                }
                _ => return Err(WireError::Protocol(format!("an ack for correlation {} without its seq", f.correlation))),
            },
            FrameKind::Cancel => {
                if let Some(flow) = flow_of(f.correlation) {
                    flow.cancel();
                }
            }
            FrameKind::Request => {
                let m = match decode(&f.payload) {
                    Ok(m) => m,
                    Err(e) => {
                        let e = error(ErrorCode::Malformed, e.0);
                        w.send(Frame::new(FrameKind::Error, f.correlation, encode(Message::Error(Box::new(e)))))?;
                        continue;
                    }
                };
                if matches!(m, Message::PutRequest(_)) {
                    tables.insert(f.correlation, (m, TableDecoder::new(), Vec::new()));
                } else {
                    spawn(f.correlation, Request { message: m, table: None }, &mut handlers);
                }
            }
            FrameKind::Schema | FrameKind::Batch => match tables.get_mut(&f.correlation) {
                Some((_, dec, batches)) => batches.extend(dec.feed(&f.payload)?),
                None => return Err(WireError::Protocol(format!("table frames for correlation {} without a request", f.correlation))),
            },
            FrameKind::End => {
                if let Some((m, dec, batches)) = tables.remove(&f.correlation) {
                    let schema = dec.schema().ok_or_else(|| WireError::Protocol("a table without a schema frame".into()))?;
                    spawn(
                        f.correlation,
                        Request {
                            message: m,
                            table: Some((schema, batches)),
                        },
                        &mut handlers,
                    );
                }
            }
            other => return Err(WireError::Protocol(format!("a service does not take {:?} frames", other))),
        }
    }
    // The peer closed: stop what is still streaming, or waiting for an ack,
    // and let it finish.
    for flow in flows.lock().expect("the flow table").values() {
        flow.cancel();
    }
    for h in handlers {
        let _ = h.join();
    }
    Ok(())
}

/// Where the reader puts each request's frames.
type Routes = Arc<Mutex<HashMap<u64, Sender<Frame>>>>;

/// A client of one service over one connection.
pub struct Client {
    w: Writer,
    pending: Routes,
    next: AtomicU64,
    peer: Hello,
}

impl Client {
    /// Opens a session: our hello, the service's hello (P-2.3), and a reader
    /// that routes every answer to its request. The reader never waits on
    /// a request's reply, so no answer waits behind another's unread frames.
    pub fn connect(conn: Connection, layer: Layer, name: &str) -> Result<Client, WireError> {
        let Connection { tx, mut rx } = conn;
        let w = Writer(Arc::new(Mutex::new(tx)));
        w.send(Frame::new(FrameKind::Hello, 0, encode(hello(layer, name))))?;
        let peer = read_hello(rx.recv()?)?;
        if major(&peer.protocol) != major(PROTOCOL) {
            return Err(WireError::Remote(error(ErrorCode::Version, format!("the service speaks {}, this client {}", peer.protocol, PROTOCOL))));
        }
        let pending: Routes = Arc::default();
        let routes = pending.clone();
        std::thread::spawn(move || {
            while let Ok(Some(f)) = rx.recv() {
                if matches!(f.kind, FrameKind::Pong | FrameKind::Ping) {
                    continue;
                }
                let route = routes.lock().ok().and_then(|p| p.get(&f.correlation).cloned());
                if let Some(tx) = route {
                    let _ = tx.send(f);
                }
            }
            // Closed: every pending answer sees its channel end.
            if let Ok(mut p) = routes.lock() {
                p.clear();
            }
        });
        Ok(Client {
            w,
            pending,
            next: AtomicU64::new(1),
            peer,
        })
    }

    /// The service's hello.
    pub fn peer(&self) -> &Hello {
        &self.peer
    }

    fn register(&self, m: &Message) -> Reply {
        let correlation = self.next.fetch_add(1, Ordering::SeqCst);
        let (tx, rx) = channel();
        self.pending.lock().expect("the pending table").insert(correlation, tx);
        // A subscription owes its index when asked for, then its events.
        let owed = match m {
            Message::SubscribeRequest(s) => 1 + usize::from(s.with_index),
            _ => 0,
        };
        Reply {
            rx,
            correlation,
            w: self.w.clone(),
            pending: self.pending.clone(),
            decoder: None,
            subscription: owed > 0,
            owed,
            in_table: false,
            failed: false,
        }
    }

    /// Sends a request; its answer arrives on the reply.
    pub fn call(&self, m: Message) -> Result<Reply, WireError> {
        let reply = self.register(&m);
        self.w.send(Frame::new(FrameKind::Request, reply.correlation, encode(m)))?;
        Ok(reply)
    }

    /// Sends a request with a table after it (`put`).
    pub fn call_with_table(&self, m: Message, schema: &SchemaRef, batches: &[RecordBatch]) -> Result<Reply, WireError> {
        let reply = self.register(&m);
        let correlation = reply.correlation;
        self.w.send(Frame::new(FrameKind::Request, correlation, encode(m)))?;
        let (mut enc, head) = TableEncoder::new(schema)?;
        self.w.send(Frame::new(FrameKind::Schema, correlation, head))?;
        for b in batches {
            for payload in enc.batch(b)? {
                self.w.send(Frame::new(FrameKind::Batch, correlation, payload))?;
            }
        }
        self.w.send(Frame::new(FrameKind::End, correlation, Vec::new()))?;
        Ok(reply)
    }

    /// A request whose answer is one control message.
    pub fn request(&self, m: Message) -> Result<Message, WireError> {
        self.call(m)?.message()
    }

    /// A request whose answer is a table.
    pub fn request_table(&self, m: Message) -> Result<(SchemaRef, Vec<RecordBatch>), WireError> {
        let mut reply = self.call(m)?;
        match reply.message()? {
            Message::TableFollows(_) => reply.table(),
            _ => Err(WireError::Protocol("the answer is not a table".into())),
        }
    }

    pub fn ping(&self) -> Result<(), WireError> {
        self.w.send(Frame::new(FrameKind::Ping, 0, Vec::new()))
    }
}

/// The answer to one request.
pub struct Reply {
    rx: Receiver<Frame>,
    correlation: u64,
    w: Writer,
    pending: Routes,
    decoder: Option<TableDecoder>,
    /// A subscription: its events are acknowledged as they are taken.
    subscription: bool,
    /// The tables a subscription still owes: its index when asked for, then
    /// its events.
    owed: usize,
    /// Between a table's `TableFollows` and its `end`.
    in_table: bool,
    /// The answer ended in an error.
    failed: bool,
}

impl Reply {
    fn next(&mut self) -> Result<Frame, WireError> {
        let f = self.rx.recv().map_err(|_| WireError::Closed)?;
        if f.kind == FrameKind::Error {
            self.failed = true;
        }
        Ok(f)
    }

    /// The next control message of the answer; an error frame is an `Err`.
    pub fn message(&mut self) -> Result<Message, WireError> {
        let f = self.next()?;
        match f.kind {
            FrameKind::Response => {
                let m = decode(&f.payload)?;
                if matches!(m, Message::TableFollows(_)) {
                    self.in_table = true;
                }
                Ok(m)
            }
            FrameKind::Error => match decode(&f.payload)? {
                Message::Error(e) => Err(WireError::Remote(*e)),
                _ => Err(WireError::Protocol("an error frame without an error".into())),
            },
            other => Err(WireError::Protocol(format!("expected a response, got {:?}", other))),
        }
    }

    /// The next batch of a table being received; `None` at its end. A
    /// subscription's batch of events is acknowledged as it is taken, which
    /// lets the service send the next (D-5.6).
    pub fn next_batch(&mut self) -> Result<Option<RecordBatch>, WireError> {
        loop {
            let f = self.next()?;
            match f.kind {
                FrameKind::Schema => {
                    let mut dec = TableDecoder::new();
                    dec.feed(&f.payload)?;
                    self.decoder = Some(dec);
                }
                FrameKind::Batch => {
                    let dec = self.decoder.as_mut().ok_or_else(|| WireError::Protocol("a batch before its schema".into()))?;
                    let mut got = dec.feed(&f.payload)?;
                    match got.len() {
                        0 => {}
                        1 => {
                            let b = got.pop();
                            if self.subscription {
                                if let Some(seq) = b.as_ref().and_then(last_seq) {
                                    // A closed connection shows at the next frame.
                                    let _ = self.ack(seq);
                                }
                            }
                            return Ok(b);
                        }
                        _ => return Err(WireError::Protocol("a batch frame held several batches".into())),
                    }
                }
                FrameKind::End => {
                    self.in_table = false;
                    self.owed = self.owed.saturating_sub(1);
                    return Ok(None);
                }
                FrameKind::Error => match decode(&f.payload)? {
                    Message::Error(e) => return Err(WireError::Remote(*e)),
                    _ => return Err(WireError::Protocol("an error frame without an error".into())),
                },
                other => return Err(WireError::Protocol(format!("expected a table frame, got {:?}", other))),
            }
        }
    }

    /// The rest of a table: its schema and every batch.
    pub fn table(&mut self) -> Result<(SchemaRef, Vec<RecordBatch>), WireError> {
        let mut batches = Vec::new();
        while let Some(b) = self.next_batch()? {
            batches.push(b);
        }
        let schema = self.decoder.as_ref().and_then(|d| d.schema()).ok_or_else(|| WireError::Protocol("a table without a schema".into()))?;
        Ok((schema, batches))
    }

    /// The table's schema, once its schema frame arrived.
    pub fn schema(&self) -> Option<SchemaRef> {
        self.decoder.as_ref().and_then(|d| d.schema())
    }

    /// Stops a streaming answer (P-2.4); the service then sends `end`.
    pub fn cancel(&self) -> Result<(), WireError> {
        self.w.send(Frame::new(FrameKind::Cancel, self.correlation, Vec::new()))
    }

    /// Acknowledges a subscription's events through `seq` (D-5.6).
    pub fn ack(&self, seq: u64) -> Result<(), WireError> {
        let m = Message::AckMessage(Box::new(crate::generated::abt::wire::AckMessage { seq }));
        self.w.send(Frame::new(FrameKind::Ack, self.correlation, encode(m)))
    }

    /// Whether the service may still be sending: a table not ended, or a
    /// subscription's tables still owed.
    fn open(&self) -> bool {
        !self.failed && (self.in_table || self.owed > 0)
    }
}

/// The seq of a batch's last event, when the batch is a stream's (its
/// first column is `seq`, P-4).
fn last_seq(b: &RecordBatch) -> Option<u64> {
    if b.num_rows() == 0 || b.schema().fields().first().map(|f| f.name().as_str()) != Some("seq") {
        return None;
    }
    let seq = b.column(0).as_any().downcast_ref::<arrow::array::UInt64Array>()?;
    Some(seq.value(seq.len() - 1))
}

impl Drop for Reply {
    fn drop(&mut self) {
        // An answer left before its end is cancelled: a subscription waiting
        // for acks that will not come then ends.
        if self.open() {
            let _ = self.cancel();
        }
        if let Ok(mut p) = self.pending.lock() {
            p.remove(&self.correlation);
        }
    }
}
