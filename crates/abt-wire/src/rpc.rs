//! Requests and responses over a connection (P-2.3, P-2.4): every service
//! and every client of the three layers speaks through this. Each side opens
//! with `hello` and a different major version is refused; requests carry a
//! correlation id and may be pipelined; a response carries its request's id;
//! a table follows its control response as schema, batch and end frames;
//! `cancel` stops a streaming response, which then ends; `ping` is answered
//! with `pong`. What a request does is the service's (each layer's
//! dispatch); this module only moves it.

use std::collections::HashMap;
use std::fmt;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{sync_channel, Receiver, SyncSender};
use std::sync::{Arc, Mutex};

use arrow::array::RecordBatch;
use arrow::datatypes::SchemaRef;
use arrow::error::ArrowError;

use crate::convert::{decode, encode};
use crate::frame::{Frame, FrameError, FrameKind};
use crate::table::{TableDecoder, TableEncoder};
use crate::transport::{Connection, FrameTx};
use crate::{ErrorCode, ErrorMessage, Hello, Layer, Message, PROTOCOL};

/// Frames a reply holds before the reader blocks (backpressure, P-2.4).
const REPLY_BOUND: usize = 64;

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

/// What a service's handler answers through.
pub struct Responder {
    w: Writer,
    correlation: u64,
    cancelled: Arc<AtomicBool>,
}

impl Responder {
    pub fn correlation(&self) -> u64 {
        self.correlation
    }

    /// Whether the client cancelled this request (a streaming answer then
    /// ends at its next batch).
    pub fn cancelled(&self) -> bool {
        self.cancelled.load(Ordering::SeqCst)
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
    let cancels: Arc<Mutex<HashMap<u64, Arc<AtomicBool>>>> = Arc::default();
    let mut tables: HashMap<u64, (Message, TableDecoder, Vec<RecordBatch>)> = HashMap::new();
    let mut handlers = Vec::new();
    let spawn = |correlation: u64, request: Request, handlers: &mut Vec<std::thread::JoinHandle<()>>| {
        let flag = Arc::new(AtomicBool::new(false));
        cancels.lock().expect("the cancel table").insert(correlation, flag.clone());
        let (w, service, cancels) = (w.clone(), service.clone(), cancels.clone());
        handlers.push(std::thread::spawn(move || {
            let out = Responder { w, correlation, cancelled: flag };
            if let Err(e) = service.handle(request, &out) {
                let _ = out.error(e);
            }
            cancels.lock().expect("the cancel table").remove(&correlation);
        }));
    };
    while let Some(f) = rx.recv()? {
        match f.kind {
            FrameKind::Ping => w.send(Frame::new(FrameKind::Pong, f.correlation, Vec::new()))?,
            FrameKind::Pong | FrameKind::Ack => {}
            FrameKind::Cancel => {
                if let Some(flag) = cancels.lock().expect("the cancel table").get(&f.correlation) {
                    flag.store(true, Ordering::SeqCst);
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
    // The peer closed: stop what is still streaming and let it finish.
    for flag in cancels.lock().expect("the cancel table").values() {
        flag.store(true, Ordering::SeqCst);
    }
    for h in handlers {
        let _ = h.join();
    }
    Ok(())
}

/// A client of one service over one connection.
pub struct Client {
    w: Writer,
    pending: Arc<Mutex<HashMap<u64, SyncSender<Frame>>>>,
    next: AtomicU64,
    peer: Hello,
}

impl Client {
    /// Opens a session: our hello, the service's hello (P-2.3), and a reader
    /// that routes every answer to its request.
    pub fn connect(conn: Connection, layer: Layer, name: &str) -> Result<Client, WireError> {
        let Connection { tx, mut rx } = conn;
        let w = Writer(Arc::new(Mutex::new(tx)));
        w.send(Frame::new(FrameKind::Hello, 0, encode(hello(layer, name))))?;
        let peer = read_hello(rx.recv()?)?;
        if major(&peer.protocol) != major(PROTOCOL) {
            return Err(WireError::Remote(error(ErrorCode::Version, format!("the service speaks {}, this client {}", peer.protocol, PROTOCOL))));
        }
        let pending: Arc<Mutex<HashMap<u64, SyncSender<Frame>>>> = Arc::default();
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

    fn register(&self) -> (u64, Receiver<Frame>) {
        let correlation = self.next.fetch_add(1, Ordering::SeqCst);
        let (tx, rx) = sync_channel(REPLY_BOUND);
        self.pending.lock().expect("the pending table").insert(correlation, tx);
        (correlation, rx)
    }

    /// Sends a request; its answer arrives on the reply.
    pub fn call(&self, m: Message) -> Result<Reply, WireError> {
        let (correlation, rx) = self.register();
        self.w.send(Frame::new(FrameKind::Request, correlation, encode(m)))?;
        Ok(Reply {
            rx,
            correlation,
            w: self.w.clone(),
            pending: self.pending.clone(),
            decoder: None,
        })
    }

    /// Sends a request with a table after it (`put`).
    pub fn call_with_table(&self, m: Message, schema: &SchemaRef, batches: &[RecordBatch]) -> Result<Reply, WireError> {
        let (correlation, rx) = self.register();
        self.w.send(Frame::new(FrameKind::Request, correlation, encode(m)))?;
        let (mut enc, head) = TableEncoder::new(schema)?;
        self.w.send(Frame::new(FrameKind::Schema, correlation, head))?;
        for b in batches {
            for payload in enc.batch(b)? {
                self.w.send(Frame::new(FrameKind::Batch, correlation, payload))?;
            }
        }
        self.w.send(Frame::new(FrameKind::End, correlation, Vec::new()))?;
        Ok(Reply {
            rx,
            correlation,
            w: self.w.clone(),
            pending: self.pending.clone(),
            decoder: None,
        })
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
    pending: Arc<Mutex<HashMap<u64, SyncSender<Frame>>>>,
    decoder: Option<TableDecoder>,
}

impl Reply {
    fn next(&mut self) -> Result<Frame, WireError> {
        self.rx.recv().map_err(|_| WireError::Closed)
    }

    /// The next control message of the answer; an error frame is an `Err`.
    pub fn message(&mut self) -> Result<Message, WireError> {
        let f = self.next()?;
        match f.kind {
            FrameKind::Response => Ok(decode(&f.payload)?),
            FrameKind::Error => match decode(&f.payload)? {
                Message::Error(e) => Err(WireError::Remote(*e)),
                _ => Err(WireError::Protocol("an error frame without an error".into())),
            },
            other => Err(WireError::Protocol(format!("expected a response, got {:?}", other))),
        }
    }

    /// The next batch of a table being received; `None` at its end.
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
                        1 => return Ok(got.pop()),
                        _ => return Err(WireError::Protocol("a batch frame held several batches".into())),
                    }
                }
                FrameKind::End => return Ok(None),
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
}

impl Drop for Reply {
    fn drop(&mut self) {
        if let Ok(mut p) = self.pending.lock() {
            p.remove(&self.correlation);
        }
    }
}
