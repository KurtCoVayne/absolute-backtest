//! The kernel service (P-3.4): `check`, `contract`, `show`, `run`, `query`,
//! `control` and `status` over the wire. A run is held by a thread of its
//! own that owns its program and kernel: it streams progress frames while
//! it runs, answers queries over the state the last barrier closed between
//! barriers (K-7.3, a query never blocks the stream: it is answered at the
//! next barrier), and once the stream ends answers them over its final
//! state until the service ends. The run writes its outputs through the
//! data layer (K-7.1).

use std::collections::HashMap;
use std::sync::mpsc;
use std::sync::{Arc, Mutex};

use abt_wire::generated::abt::wire::{KernelOp, KernelRequest, KernelResponse, Message};
use abt_wire::rpc::{error, serve, Request, Responder, Service};
use abt_wire::transport::{bind, channel_pair, Connection};
use abt_wire::{ErrorCode, ErrorMessage, Layer};
use arrow::record_batch::RecordBatch;

use super::{DataClient, ExecConfig, Progress, Replay, RunError, RunResult, SimExecutor};
use crate::check::{check_program, Program, Workspace};
use crate::query::{self, Answer, Query};

/// How the service reaches the data service: a new client per run.
pub type Connect = dyn Fn() -> Result<DataClient, RunError> + Send + Sync;

/// The service: the runs it holds, by id.
pub struct KernelService {
    connect: Arc<Connect>,
    runs: Mutex<HashMap<String, RunEntry>>,
}

struct RunEntry {
    tx: mpsc::Sender<Ask>,
    state: Arc<Mutex<RunState>>,
}

/// A run's state as `status` reports it.
#[derive(Clone, Debug, Default)]
struct RunState {
    status: String,
    barrier: Option<i64>,
    decisions: u64,
}

/// What a run's thread is asked between barriers and once it ended.
enum Ask {
    Query(String, mpsc::Sender<Result<Answer, RunError>>),
    Halt,
}

/// What a run's thread tells the request that started it.
enum Frame {
    Started(String),
    Barrier { t: i64, decisions: u64 },
    Done { run_id: String, status: String, report: String },
}

fn refused(m: impl Into<String>) -> ErrorMessage {
    error(ErrorCode::Refused, m)
}

fn run_error(e: RunError) -> ErrorMessage {
    match e {
        RunError::Request(m) | RunError::Config(m) => error(ErrorCode::Refused, m),
        RunError::Unbound(m) => error(ErrorCode::Unresolved, m),
        e => error(ErrorCode::Halted, e.to_string()),
    }
}

fn response(op: KernelOp) -> KernelResponse {
    KernelResponse {
        op,
        diagnostics: None,
        program_hash: None,
        normal_form: None,
        degrees_of_freedom: None,
        run_id: None,
        status: None,
        text: None,
        report: None,
        barrier: None,
        decisions: None,
        lag: None,
        has_table: false,
    }
}

impl KernelService {
    pub fn new(connect: Arc<Connect>) -> Arc<KernelService> {
        Arc::new(KernelService {
            connect,
            runs: Mutex::new(HashMap::new()),
        })
    }

    /// The workspace of a request's sources and its strategy, checked; a
    /// program that does not check is refused with its diagnostics.
    fn program(&self, r: &KernelRequest) -> Result<(Workspace, Program, Vec<String>), ErrorMessage> {
        let mut ws = Workspace::new();
        for f in r.sources.iter().flatten() {
            ws.add_source(&f.text)
                .map_err(|e| refused(format!("{}:{}: parse error: {}", f.path.as_deref().unwrap_or("source"), e.span, e.message)))?;
        }
        let strategy = r.strategy.as_deref().ok_or_else(|| refused("the request names no strategy"))?;
        let (prog, diags) = check_program(&ws, strategy);
        let shown: Vec<String> = diags.iter().map(|d| d.to_string()).collect();
        match prog {
            Some(p) => Ok((ws, p, shown)),
            None => Err(refused(format!("strategy `{}` does not check:\n{}", strategy, shown.join("\n")))),
        }
    }

    /// A run's configuration: the program's blocks, with the request's
    /// point, window, budget and pushdown.
    fn config(prog: &Program, r: &KernelRequest) -> Result<ExecConfig, ErrorMessage> {
        let mut cfg = ExecConfig::from_program(prog);
        cfg.pushdown = r.pushdown;
        cfg.memory_budget = r.memory_budget;
        cfg.start = r.start;
        cfg.end = r.end;
        for p in r.point.iter().flatten() {
            let lit = crate::syntax::parse_lit(&p.value).map_err(|e| refused(format!("parameter {}: `{}` is not a literal: {}", p.name, p.value, e.message)))?;
            cfg.param_overrides.push((p.name.clone(), lit));
        }
        Ok(cfg)
    }

    fn run(&self, r: &KernelRequest, out: &Responder) -> Result<(), ErrorMessage> {
        let (ws, prog, _) = self.program(r)?;
        let cfg = Self::config(&prog, r)?;
        let every = r.progress_every.unwrap_or(0);
        let to_seq = r.to_seq;
        let client = (self.connect)().map_err(run_error)?;
        let (tx, rx) = mpsc::channel::<Ask>();
        let (ptx, prx) = mpsc::channel::<Frame>();
        let state = Arc::new(Mutex::new(RunState {
            status: "running".into(),
            ..Default::default()
        }));
        let st = state.clone();
        let connect = self.connect.clone();
        std::thread::Builder::new()
            .stack_size(512 << 20)
            .spawn(move || hold(ws, prog, cfg, to_seq, client, connect, rx, ptx, st, every))
            .map_err(|e| error(ErrorCode::Internal, format!("cannot spawn a run: {}", e)))?;
        for frame in prx {
            match frame {
                Frame::Started(run_id) => {
                    let mut runs = self.runs.lock().expect("the runs");
                    if runs.get(&run_id).is_some_and(|e| e.state.lock().expect("a run's state").status == "running") {
                        let _ = tx.send(Ask::Halt);
                        return Err(error(ErrorCode::Busy, format!("run {} is running", run_id)));
                    }
                    runs.insert(run_id.clone(), RunEntry { tx: tx.clone(), state: state.clone() });
                    drop(runs);
                    let mut m = response(KernelOp::Run);
                    m.run_id = Some(run_id);
                    m.status = Some("running".into());
                    out.respond(Message::KernelResponse(Box::new(m))).map_err(|e| error(ErrorCode::Internal, e.to_string()))?;
                }
                Frame::Barrier { t, decisions } => {
                    let mut m = response(KernelOp::Run);
                    m.status = Some("running".into());
                    m.barrier = Some(t);
                    m.decisions = Some(decisions);
                    m.lag = Some(0);
                    out.respond(Message::KernelResponse(Box::new(m))).map_err(|e| error(ErrorCode::Internal, e.to_string()))?;
                }
                Frame::Done { run_id, status, report } => {
                    let mut m = response(KernelOp::Run);
                    m.run_id = Some(run_id);
                    m.status = Some(status);
                    m.report = Some(report);
                    return out.respond(Message::KernelResponse(Box::new(m))).map_err(|e| error(ErrorCode::Internal, e.to_string()));
                }
            }
        }
        Err(error(ErrorCode::Internal, "the run's thread ended without a last frame"))
    }

    fn entry(&self, r: &KernelRequest) -> Result<(mpsc::Sender<Ask>, Arc<Mutex<RunState>>), ErrorMessage> {
        let id = r.run_id.as_deref().ok_or_else(|| refused("the request names no run"))?;
        let runs = self.runs.lock().expect("the runs");
        let e = runs.get(id).ok_or_else(|| error(ErrorCode::NotFound, format!("no run {} here", id)))?;
        Ok((e.tx.clone(), e.state.clone()))
    }

    fn query(&self, r: &KernelRequest, out: &Responder) -> Result<(), ErrorMessage> {
        let text = r.text.clone().ok_or_else(|| refused("a query request carries the query's text"))?;
        let (tx, _) = self.entry(r)?;
        let (rtx, rrx) = mpsc::channel();
        tx.send(Ask::Query(text, rtx)).map_err(|_| error(ErrorCode::NotFound, "the run is no longer held"))?;
        let answer = rrx.recv().map_err(|_| error(ErrorCode::Internal, "the run ended before it answered"))?.map_err(run_error)?;
        let mut m = response(KernelOp::Query);
        m.run_id = r.run_id.clone();
        m.text = Some(answer.text.clone());
        m.has_table = answer.table.is_some();
        out.respond(Message::KernelResponse(Box::new(m))).map_err(|e| error(ErrorCode::Internal, e.to_string()))?;
        if let Some(t) = &answer.table {
            let b = batch(t).map_err(|e| error(ErrorCode::Internal, e))?;
            out.table(&b.schema(), [b]).map_err(|e| error(ErrorCode::Internal, e.to_string()))?;
        }
        Ok(())
    }
}

/// A state table as an Arrow batch.
fn batch(t: &crate::state::tables::Table) -> Result<RecordBatch, String> {
    crate::table::batch(t.cols())
}

impl Service for KernelService {
    fn layer(&self) -> Layer {
        Layer::Kernel
    }

    fn handle(&self, request: Request, out: &Responder) -> Result<(), ErrorMessage> {
        let Message::KernelRequest(r) = request.message else {
            return Err(error(ErrorCode::Malformed, "the kernel service takes kernel requests"));
        };
        let respond = |m: KernelResponse| out.respond(Message::KernelResponse(Box::new(m))).map_err(|e| error(ErrorCode::Internal, e.to_string()));
        match r.op {
            KernelOp::Check => {
                let mut ws = Workspace::new();
                for f in r.sources.iter().flatten() {
                    ws.add_source(&f.text)
                        .map_err(|e| refused(format!("{}:{}: parse error: {}", f.path.as_deref().unwrap_or("source"), e.span, e.message)))?;
                }
                let strategy = r.strategy.as_deref().ok_or_else(|| refused("the request names no strategy"))?;
                let (prog, diags) = check_program(&ws, strategy);
                let mut m = response(KernelOp::Check);
                m.diagnostics = Some(diags.iter().map(|d| d.to_string()).collect());
                if let Some(p) = prog {
                    let nir = crate::compiler::normalise(&p);
                    m.program_hash = Some(nir.hash.clone());
                    m.normal_form = Some(nir.source(&p));
                    m.degrees_of_freedom = Some(p.degrees_of_freedom.summary());
                }
                respond(m)
            }
            KernelOp::Contract => {
                let (_, prog, _) = self.program(&r)?;
                let mut m = response(KernelOp::Contract);
                m.text = Some(crate::contract::contract(&prog).table());
                respond(m)
            }
            KernelOp::Show => {
                let what = r.what.clone().unwrap_or_else(|| "program".into());
                // Of a run held here: answered over its state.
                if r.run_id.is_some() {
                    let (tx, _) = self.entry(&r)?;
                    let (rtx, rrx) = mpsc::channel();
                    tx.send(Ask::Query(format!("show {}", what), rtx))
                        .map_err(|_| error(ErrorCode::NotFound, "the run is no longer held"))?;
                    let a = rrx.recv().map_err(|_| error(ErrorCode::Internal, "the run ended before it answered"))?.map_err(run_error)?;
                    let mut m = response(KernelOp::Show);
                    m.text = Some(a.text);
                    return respond(m);
                }
                let (_, prog, _) = self.program(&r)?;
                let text = match crate::query::parse(&format!("show {}", what)).map_err(refused)? {
                    Query::Show(crate::query::Show::Program) => crate::observe::show_program(&prog),
                    Query::Show(crate::query::Show::Plan(rel)) => {
                        let plan = crate::compiler::compile(&prog, &crate::compiler::CompileOptions::default()).plan;
                        match rel {
                            Some(rel) if !prog.relations.contains_key(&rel) => return Err(refused(format!("`{}` is not a relation of {}", rel, prog.strategy))),
                            Some(rel) => plan.only(&prog, &rel).text(),
                            None => plan.text(),
                        }
                    }
                    Query::Show(crate::query::Show::Contract) => crate::contract::contract(&prog).table(),
                    Query::Show(crate::query::Show::Schedule) => query::schedule(&prog),
                    Query::Show(crate::query::Show::Calendars) => {
                        let mut s = String::from("calendar\n");
                        for name in prog.calendars.keys() {
                            s.push_str(&format!("  {}\n", name));
                        }
                        s
                    }
                    _ => return Err(refused(format!("`show {}` is not a form of show", what))),
                };
                let mut m = response(KernelOp::Show);
                m.text = Some(text);
                respond(m)
            }
            KernelOp::Run => self.run(&r, out),
            KernelOp::Query => self.query(&r, out),
            KernelOp::Control => {
                let (tx, state) = self.entry(&r)?;
                match r.what.as_deref() {
                    Some("halt") => {
                        if state.lock().expect("a run's state").status != "running" {
                            return Err(refused("the run is not running"));
                        }
                        tx.send(Ask::Halt).map_err(|_| error(ErrorCode::NotFound, "the run is no longer held"))?;
                        let mut m = response(KernelOp::Control);
                        m.run_id = r.run_id.clone();
                        m.status = Some("halting".into());
                        respond(m)
                    }
                    Some("checkpoint") => Err(refused("a checkpoint on request comes with the checkpoints through the data layer (M-4e)")),
                    Some("flatten") => Err(refused("flatten acts on a live broker's book (M-10); a simulated run has none")),
                    other => Err(refused(format!("control takes halt, checkpoint or flatten, not {:?}", other))),
                }
            }
            KernelOp::Status => {
                let runs = self.runs.lock().expect("the runs");
                let mut lines = Vec::new();
                for (id, e) in runs.iter().filter(|(id, _)| r.run_id.as_deref().is_none_or(|x| x == *id)) {
                    let s = e.state.lock().expect("a run's state").clone();
                    lines.push((id.clone(), s));
                }
                if lines.is_empty() && r.run_id.is_some() {
                    return Err(error(ErrorCode::NotFound, format!("no run {} here", r.run_id.as_deref().unwrap_or(""))));
                }
                lines.sort_by(|a, b| a.0.cmp(&b.0));
                let mut m = response(KernelOp::Status);
                if let [(id, s)] = lines.as_slice() {
                    m.run_id = Some(id.clone());
                    m.status = Some(s.status.clone());
                    m.barrier = s.barrier;
                    m.decisions = Some(s.decisions);
                    m.lag = Some(0);
                }
                m.text = Some(
                    lines
                        .iter()
                        .map(|(id, s)| {
                            format!(
                                "{} {} barrier {} decisions {}\n",
                                id,
                                s.status,
                                s.barrier.map(crate::kernel::time::format_timestamp).unwrap_or_else(|| "-".into()),
                                s.decisions
                            )
                        })
                        .collect(),
                );
                respond(m)
            }
        }
    }
}

/// The run's report rows as JSON: the outputs' `report` (K-7.1).
fn report(r: &RunResult, status: &str) -> String {
    serde_json::json!({
        "status": status,
        "bars": r.bars.len(),
        "decisions": r.decisions.len(),
        "fills": r.fills.len(),
        "final_cash": r.final_cash,
        "warnings": r.warnings.iter().map(|w| serde_json::json!({"bias": w.bias, "message": w.message})).collect::<Vec<_>>(),
    })
    .to_string()
}

/// A run held by its thread: runs it, then answers queries until the
/// service drops it.
#[allow(clippy::too_many_arguments)]
fn hold(
    ws: Workspace,
    prog: Program,
    cfg: ExecConfig,
    to_seq: Option<u64>,
    client: DataClient,
    connect: Arc<Connect>,
    rx: mpsc::Receiver<Ask>,
    ptx: mpsc::Sender<Frame>,
    state: Arc<Mutex<RunState>>,
    every: u64,
) {
    let compiled = crate::compiler::compile(&prog, &crate::compiler::CompileOptions::default());
    let mut exec = SimExecutor::new(cfg.clone());
    let opts = Replay {
        write_outputs: true,
        to_seq,
        ..Default::default()
    };
    let mut barriers = 0u64;
    let outcome = super::replay_observed(&prog, Some(&compiled.lowered), &client, cfg.clone(), &mut exec, &opts, &mut |_| {}, &mut |p, k, result| {
        match p {
            Progress::Started { run_id } => {
                let _ = ptx.send(Frame::Started(run_id));
            }
            Progress::Barrier { t } => {
                barriers += 1;
                let decisions = result.decisions.len() as u64;
                {
                    let mut s = state.lock().expect("a run's state");
                    s.barrier = Some(t);
                    s.decisions = decisions;
                }
                if every > 0 && barriers.is_multiple_of(every) {
                    let _ = ptx.send(Frame::Barrier { t, decisions });
                }
                // Between barriers: the queries asked meanwhile, over Σ_t at
                // the barrier just closed (K-7.3).
                while let Ok(ask) = rx.try_recv() {
                    match ask {
                        Ask::Query(text, reply) => {
                            let _ = reply.send(answer_live(k, &prog, result, &compiled.plan, &text));
                        }
                        Ask::Halt => return Err(RunError::Request("a control request halted the run".into())),
                    }
                }
            }
        }
        Ok(())
    });
    match outcome {
        Ok((result, mut kernel)) => {
            state.lock().expect("a run's state").status = "completed".into();
            let _ = ptx.send(Frame::Done {
                run_id: result.run_id.clone(),
                status: "completed".into(),
                report: report(&result, "completed"),
            });
            drop(ptx);
            for ask in rx {
                if let Ask::Query(text, reply) = ask {
                    let _ = reply.send(answer_held(&mut kernel, &ws, &prog, &result, &compiled.plan, &client, &connect, &cfg, &text));
                }
            }
        }
        Err(e) => {
            let status = format!("halted({})", e);
            state.lock().expect("a run's state").status = status.clone();
            let _ = ptx.send(Frame::Done {
                run_id: String::new(),
                status: status.clone(),
                report: serde_json::json!({ "status": status }).to_string(),
            });
            drop(ptx);
            for ask in rx {
                if let Ask::Query(_, reply) = ask {
                    let _ = reply.send(Err(RunError::Request(format!("the run {}", status))));
                }
            }
        }
    }
}

/// A query between barriers: every form but a `?` over more than one atom
/// (which the run would have to hold as a rule from its start) and `ledger`
/// (written once the run ends).
fn answer_live(k: &mut super::Kernel, prog: &Program, result: &RunResult, plan: &crate::plan::Plan, text: &str) -> Result<Answer, RunError> {
    let q = query::parse(text).map_err(RunError::Request)?;
    if matches!(q, Query::Ledger { .. }) {
        return Err(RunError::Request("`ledger ... into` is asked of a run that ended".into()));
    }
    let prepared = query::Prepared::direct(prog, std::slice::from_ref(&q)).map_err(RunError::Request)?;
    let cx = query::Context {
        prog,
        result,
        prepared: &prepared,
        plan: Some(plan),
        out: None,
    };
    query::answer(k, &cx, 0, &q)
}

/// A query of a run that ended: over its final state, or, for a `?` over
/// more than one atom, over a run of the program holding the query as a
/// rule (Definition V-8.1), on the same binding.
#[allow(clippy::too_many_arguments)]
fn answer_held(
    k: &mut super::Kernel,
    ws: &Workspace,
    prog: &Program,
    result: &RunResult,
    plan: &crate::plan::Plan,
    client: &DataClient,
    connect: &Arc<Connect>,
    cfg: &ExecConfig,
    text: &str,
) -> Result<Answer, RunError> {
    let q = query::parse(text).map_err(RunError::Request)?;
    if let Ok(prepared) = query::Prepared::direct(prog, std::slice::from_ref(&q)) {
        let cx = query::Context {
            prog,
            result,
            prepared: &prepared,
            plan: Some(plan),
            out: Some((client, result.run_id.as_str())),
        };
        return query::answer(k, &cx, 0, &q);
    }
    let prepared = query::prepare(ws, &prog.strategy, prog, std::slice::from_ref(&q)).map_err(RunError::Request)?;
    let p = &prepared.program;
    let client = connect()?;
    let compiled = crate::compiler::compile(p, &crate::compiler::CompileOptions::default());
    let mut exec = SimExecutor::new(cfg.clone());
    let (r, mut k2) = super::replay(p, Some(&compiled.lowered), &client, cfg.clone(), &mut exec, &Replay::default(), &mut |_| {})?;
    if r.binding_id != result.binding_id {
        return Err(RunError::Request(format!(
            "the data service binds the program to {} now, and the run read {}: a query over more than one atom is asked again of a run on the same binding",
            r.binding_id, result.binding_id
        )));
    }
    let cx = query::Context {
        prog: p,
        result: &r,
        prepared: &prepared,
        plan: Some(&compiled.plan),
        out: None,
    };
    let a = query::answer(&mut k2, &cx, 0, &q);
    std::mem::forget(k2);
    a
}

/// A connection to the service in this process, the service's end served on
/// a thread of its own.
pub fn inproc(service: &Arc<KernelService>) -> Connection {
    let (client, server) = channel_pair(1024);
    let s: Arc<dyn Service> = service.clone();
    std::thread::spawn(move || {
        let _ = serve(server, s);
    });
    client
}

/// Serves the service on a Unix socket: a thread per connection.
pub fn listen(path: &std::path::Path, service: Arc<KernelService>) -> std::io::Result<std::thread::JoinHandle<()>> {
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
