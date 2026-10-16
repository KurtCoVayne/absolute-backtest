//! The query layer's client of the kernel service (P-3.4): it checks the
//! program, reads the sample a run would get, and asks for runs, each
//! carrying the plan step that asks for it (π, Q-2.5). It never evaluates a
//! program (A-D13): what a run computes it reads back from the data layer.

use abt_wire::generated::abt::wire::{KernelOp, KernelRequest, KernelResponse, Message, ParamValue, SourceFile, Span};
use abt_wire::rpc::{Client, Reply};
use abt_wire::transport::Connection;
use abt_wire::Layer;

use crate::data::Error;
use crate::plan::Point;

/// A program as the kernel takes it: the workspace's sources and the
/// strategy's name.
#[derive(Clone, Debug)]
pub struct Program {
    pub sources: Vec<SourceFile>,
    pub strategy: String,
}

/// The kernel's check of a program.
#[derive(Clone, Debug)]
pub struct Checked {
    pub program_hash: String,
    pub normal_form: String,
    pub diagnostics: Vec<String>,
}

/// What a run of the program would read now (`show binding`): its binding
/// and the span of its decision bars.
#[derive(Clone, Debug, PartialEq)]
pub struct Sample {
    pub binding: String,
    pub relations: Vec<(String, String)>,
    pub calendar: String,
    pub bars: usize,
    pub first: Option<i64>,
    pub last: Option<i64>,
}

/// A run to ask for.
#[derive(Clone, Debug, Default)]
pub struct RunRequest {
    pub point: Point,
    pub start: Option<i64>,
    pub end: Option<i64>,
    /// Stop after the decision at this bar (Q-2.3's truncation).
    pub horizon: Option<i64>,
    /// π, canonical JSON (Q-2.5).
    pub plan_step: Option<serde_json::Value>,
    /// A block hold-out's spans, `[from, to)`.
    pub mask: Vec<(i64, i64)>,
    pub observe: Vec<String>,
}

/// A run's last frame.
#[derive(Clone, Debug)]
pub struct Ran {
    pub run_id: String,
    /// `completed` or `halted(reason)`.
    pub status: String,
    pub report: serde_json::Value,
}

/// A client of one kernel service.
pub struct Kernel {
    client: Client,
}

fn request(op: KernelOp) -> KernelRequest {
    KernelRequest {
        op,
        sources: None,
        strategy: None,
        point: None,
        what: None,
        run_id: None,
        text: None,
        to_seq: None,
        start: None,
        end: None,
        memory_budget: None,
        pushdown: true,
        progress_every: None,
        checkpoint_every: None,
        from_checkpoint: None,
        horizon: None,
        plan_step: None,
        embargo_mask: None,
        observe: None,
    }
}

fn wire(e: abt_wire::rpc::WireError) -> Error {
    Error::Wire(e.to_string())
}

fn point(p: &Point) -> Vec<ParamValue> {
    p.iter()
        .map(|(name, value)| ParamValue {
            name: name.clone(),
            value: value.clone(),
        })
        .collect()
}

impl Kernel {
    pub fn connect(conn: Connection) -> Result<Kernel, Error> {
        Ok(Kernel {
            client: Client::connect(conn, Layer::Query, "abt-query").map_err(wire)?,
        })
    }

    fn ask(&self, r: KernelRequest) -> Result<(KernelResponse, Reply), Error> {
        let mut reply = self.client.call(Message::KernelRequest(Box::new(r))).map_err(wire)?;
        match reply.message().map_err(wire)? {
            Message::KernelResponse(m) => Ok((*m, reply)),
            _ => Err(Error::Wire("the kernel service answered with another message".into())),
        }
    }

    fn of(prog: &Program, op: KernelOp) -> KernelRequest {
        let mut r = request(op);
        r.sources = Some(prog.sources.clone());
        r.strategy = Some(prog.strategy.clone());
        r
    }

    /// The kernel's check of the program: its hash and normal form, or the
    /// diagnostics it does not check with.
    pub fn check(&self, prog: &Program) -> Result<Checked, Error> {
        let (m, _) = self.ask(Self::of(prog, KernelOp::Check))?;
        let diagnostics = m.diagnostics.unwrap_or_default();
        match (m.program_hash, m.normal_form) {
            (Some(program_hash), Some(normal_form)) => Ok(Checked {
                program_hash,
                normal_form,
                diagnostics,
            }),
            _ => Err(Error::Missing(format!("strategy `{}` does not check:\n{}", prog.strategy, diagnostics.join("\n")))),
        }
    }

    /// The binding a run at `at` would get now, and its decision bars' span.
    pub fn sample(&self, prog: &Program, at: &Point) -> Result<Sample, Error> {
        let mut r = Self::of(prog, KernelOp::Show);
        r.what = Some("binding".into());
        r.point = Some(point(at));
        let (m, _) = self.ask(r)?;
        let v: serde_json::Value = serde_json::from_str(m.text.as_deref().unwrap_or("null")).map_err(|e| Error::Malformed(format!("show binding: {}", e)))?;
        Ok(Sample {
            binding: v["binding"].as_str().unwrap_or_default().to_string(),
            relations: v["relations"]
                .as_array()
                .map(|a| {
                    a.iter()
                        .map(|p| (p["relation"].as_str().unwrap_or_default().to_string(), p["version"].as_str().unwrap_or_default().to_string()))
                        .collect()
                })
                .unwrap_or_default(),
            calendar: v["calendar"].as_str().unwrap_or_default().to_string(),
            bars: v["bars"].as_u64().unwrap_or(0) as usize,
            first: v["first"].as_i64(),
            last: v["last"].as_i64(),
        })
    }

    /// A run, to its last frame; a run the kernel refuses is an error.
    pub fn run(&self, prog: &Program, run: &RunRequest) -> Result<Ran, Error> {
        let mut r = Self::of(prog, KernelOp::Run);
        r.point = Some(point(&run.point));
        r.start = run.start;
        r.end = run.end;
        r.horizon = run.horizon;
        r.plan_step = run.plan_step.as_ref().map(|v| abt_wire::to_canonical_json(v).unwrap_or_default());
        if !run.mask.is_empty() {
            r.embargo_mask = Some(run.mask.iter().map(|&(from, to)| Span { from, to }).collect());
        }
        if !run.observe.is_empty() {
            r.observe = Some(run.observe.clone());
        }
        let (mut m, mut reply) = self.ask(r)?;
        let mut run_id = m.run_id.clone().unwrap_or_default();
        while m.status.as_deref() == Some("running") {
            m = match reply.message().map_err(wire)? {
                Message::KernelResponse(next) => *next,
                _ => return Err(Error::Wire("a run's frames were interrupted by another message".into())),
            };
            // A halted run's last frame names none: its id is the first's.
            if let Some(id) = m.run_id.as_ref().filter(|id| !id.is_empty()) {
                run_id = id.clone();
            }
        }
        let report = serde_json::from_str(m.report.as_deref().unwrap_or("null")).unwrap_or(serde_json::Value::Null);
        Ok(Ran {
            run_id,
            status: m.status.unwrap_or_default(),
            report,
        })
    }

    /// `ledger R into NAME` of a run the kernel holds: R's ledger written as
    /// `kernel.<run>.NAME` (V-8.3).
    pub fn ledger(&self, run_id: &str, relation: &str, into: &str) -> Result<String, Error> {
        let mut r = request(KernelOp::Query);
        r.run_id = Some(run_id.to_string());
        r.text = Some(format!("ledger {} into {}", relation, into));
        let (m, _) = self.ask(r)?;
        Ok(m.text.unwrap_or_default())
    }

    /// The kernel no longer holds the run (its outputs stay in the data
    /// layer).
    pub fn release(&self, run_id: &str) -> Result<(), Error> {
        let mut r = request(KernelOp::Control);
        r.run_id = Some(run_id.to_string());
        r.what = Some("release".into());
        self.ask(r).map(|_| ())
    }
}
