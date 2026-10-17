//! The query service (P-3.5; Q-9's `abt query serve`): the query layer's
//! commands over the wire. A request carries what its command reads: its
//! words, a plan's text, the program a plan or a lineage names. The service
//! runs the command against the data and kernel services it was started
//! with, and answers with what the command printed and its exit status
//! (ERRATA E-81).

use std::sync::Arc;

use abt_wire::generated::abt::wire::{Message, QueryLayerOp, QueryLayerRequest, QueryLayerResponse};
use abt_wire::rpc::{error, serve, Client, Request, Responder, Service};
use abt_wire::transport::{bind, channel_pair, Connection};
use abt_wire::{ErrorCode, ErrorMessage, Layer};

use crate::cli::{self, Captured};
use crate::kernel::Program;

/// A new connection to the data service.
pub type ConnectData = dyn Fn() -> Result<Connection, String> + Send + Sync;

/// New connections to the kernels a plan's runs are spread over.
pub type ConnectKernels = dyn Fn() -> Result<Vec<Connection>, String> + Send + Sync;

/// The most of a command's output one response carries.
const PART: usize = 256 * 1024;

pub struct QueryService {
    data: Arc<ConnectData>,
    kernels: Arc<ConnectKernels>,
}

impl QueryService {
    pub fn new(data: Arc<ConnectData>, kernels: Arc<ConnectKernels>) -> Arc<QueryService> {
        Arc::new(QueryService { data, kernels })
    }

    /// Runs a request's command; its exit status.
    fn run(&self, r: &QueryLayerRequest, io: &mut Captured) -> Result<i32, String> {
        let args: Vec<String> = r.args.clone().unwrap_or_default();
        let data = (self.data)()?;
        let program = || Program {
            sources: r.sources.clone().unwrap_or_default(),
            strategy: r.strategy.clone().unwrap_or_default(),
        };
        Ok(match r.op {
            QueryLayerOp::Plan => cli::plan_served(&args, r.plan.as_deref().unwrap_or(""), program(), data, (self.kernels)()?, io),
            QueryLayerOp::Lineage => cli::lineage_served("lineage", &args, program(), data, (self.kernels)()?, io),
            QueryLayerOp::Dispute => cli::lineage_served("dispute", &args, program(), data, (self.kernels)()?, io),
            QueryLayerOp::Metrics => cli::metrics_main_io(&args, data, io),
            QueryLayerOp::Test => cli::query_main_io("test", &args, data, io),
            QueryLayerOp::Claim => cli::query_main_io("claim", &args, data, io),
            QueryLayerOp::Feed => cli::query_main_io("feed", &args, data, io),
            QueryLayerOp::Activation => cli::query_main_io("activation", &args, data, io),
        })
    }
}

/// A text in parts of at most `PART` bytes, each cut at a character.
fn parts(text: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut rest = text;
    while rest.len() > PART {
        let mut cut = PART;
        while !rest.is_char_boundary(cut) {
            cut -= 1;
        }
        out.push(&rest[..cut]);
        rest = &rest[cut..];
    }
    if !rest.is_empty() {
        out.push(rest);
    }
    out
}

fn response(op: QueryLayerOp, out: Option<&str>, err: Option<&str>, done: bool, status: i32) -> Message {
    Message::QueryLayerResponse(Box::new(QueryLayerResponse {
        op,
        out: out.map(str::to_string),
        err: err.map(str::to_string),
        done,
        status,
    }))
}

impl Service for QueryService {
    fn layer(&self) -> Layer {
        Layer::Query
    }

    fn handle(&self, request: Request, out: &Responder) -> Result<(), ErrorMessage> {
        let Message::QueryLayerRequest(r) = request.message else {
            return Err(error(ErrorCode::Malformed, "the query service takes query-layer requests"));
        };
        let mut io = Captured::default();
        let status = self.run(&r, &mut io).map_err(|e| error(ErrorCode::Refused, e))?;
        let send = |m: Message| out.respond(m).map_err(|e| error(ErrorCode::Internal, e.to_string()));
        for p in parts(&io.out) {
            send(response(r.op, Some(p), None, false, 0))?;
        }
        for p in parts(&io.err) {
            send(response(r.op, None, Some(p), false, 0))?;
        }
        send(response(r.op, None, None, true, status))
    }
}

/// What a command printed to its output and to its notes, and its exit
/// status, as the query service answered.
#[derive(Clone, Debug, Default)]
pub struct Answer {
    pub out: String,
    pub err: String,
    pub status: i32,
}

/// One command, asked of the query service at the other end of `conn`.
pub fn call(conn: Connection, request: QueryLayerRequest) -> Result<Answer, String> {
    let client = Client::connect(conn, Layer::Front, "abt").map_err(|e| e.to_string())?;
    let mut reply = client.call(Message::QueryLayerRequest(Box::new(request))).map_err(|e| e.to_string())?;
    let mut a = Answer::default();
    loop {
        match reply.message().map_err(|e| e.to_string())? {
            Message::QueryLayerResponse(r) => {
                a.out.push_str(r.out.as_deref().unwrap_or(""));
                a.err.push_str(r.err.as_deref().unwrap_or(""));
                if r.done {
                    a.status = r.status;
                    return Ok(a);
                }
            }
            other => return Err(format!("the query service answered {:?}", std::mem::discriminant(&other))),
        }
    }
}

/// A connection to the service in this process, the service's end served on
/// a thread of its own.
pub fn inproc(service: &Arc<QueryService>) -> Connection {
    let (client, server) = channel_pair(1024);
    let s: Arc<dyn Service> = service.clone();
    std::thread::spawn(move || {
        let _ = serve(server, s);
    });
    client
}

/// Serves the service on a Unix socket: a thread per connection.
pub fn listen(path: &std::path::Path, service: Arc<QueryService>) -> std::io::Result<std::thread::JoinHandle<()>> {
    let listener = bind(path)?;
    Ok(std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(stream) = stream else { break };
            let Ok(conn) = Connection::socket(stream) else { continue };
            let s: Arc<dyn Service> = service.clone();
            std::thread::spawn(move || {
                let _ = serve(conn, s);
            });
        }
    }))
}
