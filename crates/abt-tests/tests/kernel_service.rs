//! The kernel service (P-3.4) over the wire, in process: `check`,
//! `contract` and `show` from sources; a `run` that streams its progress and
//! ends as the run in process does (G3); queries of the run it holds,
//! between barriers while it runs (K-7.3) and over its final state after;
//! `status` and `control`.

#![allow(clippy::result_large_err)]

use std::sync::Arc;

use abt_kernel::check::Program;
use abt_kernel::kernel::service::{inproc, KernelService};
use abt_kernel::kernel::{DataClient, Dataset, ExecConfig};
use abt_tests as common;
use abt_wire::generated::abt::wire::{KernelOp, KernelRequest, KernelResponse, Message, SourceFile};
use abt_wire::rpc::{Client, Reply};
use abt_wire::Layer;

/// A data service holding the market and a kernel service reaching it,
/// both in this process; the client of the kernel's.
fn services(prog: &Program, ds: &Dataset) -> Client {
    let svc = abt::serve_dataset(ds, prog).unwrap();
    let connect = Arc::new(move || DataClient::connect(abt_data::service::inproc(&svc)));
    let kernel = KernelService::new(connect);
    Client::connect(inproc(&kernel), Layer::Front, "test").unwrap()
}

/// The catalog, the library and a strategy's file, as sources.
fn sources(strategy: &str) -> Vec<SourceFile> {
    let root = common::repo_root();
    let mut paths: Vec<std::path::PathBuf> = common::dsl_files("catalog");
    paths.extend(common::dsl_files("lib"));
    paths.push(root.join("corpus-v2/strategies").join(format!("{}.dsl", strategy)));
    paths
        .into_iter()
        .map(|p| SourceFile {
            text: std::fs::read_to_string(&p).unwrap(),
            path: Some(p.to_string_lossy().into_owned()),
        })
        .collect()
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
    }
}

fn of(strategy: &str, op: KernelOp) -> KernelRequest {
    let mut r = request(op);
    r.strategy = Some(strategy.into());
    r.sources = Some(sources(strategy));
    r
}

fn next(reply: &mut Reply) -> Result<KernelResponse, String> {
    match reply.message() {
        Ok(Message::KernelResponse(m)) => Ok(*m),
        Ok(_) => Err("another message".into()),
        Err(e) => Err(e.to_string()),
    }
}

fn ask(c: &Client, r: KernelRequest) -> Result<(KernelResponse, Reply), String> {
    let mut reply = c.call(Message::KernelRequest(Box::new(r))).map_err(|e| e.to_string())?;
    Ok((next(&mut reply)?, reply))
}

/// A run to its last frame: the frames in order.
fn run(c: &Client, r: KernelRequest) -> Vec<KernelResponse> {
    let (first, mut reply) = ask(c, r).unwrap();
    let mut frames = vec![first];
    while frames.last().unwrap().status.as_deref() == Some("running") {
        frames.push(next(&mut reply).unwrap());
    }
    frames
}

fn query(c: &Client, run_id: &str, text: &str) -> Result<(KernelResponse, Option<usize>), String> {
    let mut r = request(KernelOp::Query);
    r.run_id = Some(run_id.into());
    r.text = Some(text.into());
    let (m, mut reply) = ask(c, r)?;
    let rows = if m.has_table {
        match reply.message() {
            Ok(Message::TableFollows(_)) => Some(reply.table().map_err(|e| e.to_string())?.1.iter().map(|b| b.num_rows()).sum()),
            _ => return Err("the announced table did not follow".into()),
        }
    } else {
        None
    };
    Ok((m, rows))
}

#[test]
fn check_contract_and_show_answer_from_sources() {
    let (prog, ds) = common::golden_inputs(&common::corpus_dir().join("strategies/sma_crossover.dsl")).unwrap();
    let c = services(&prog, &ds);
    let (m, _) = ask(&c, of("sma_crossover", KernelOp::Check)).unwrap();
    assert_eq!(m.program_hash.as_deref(), Some(abt_kernel::compiler::normalise(&prog).hash.as_str()));
    assert!(m.normal_form.is_some_and(|n| n.contains("decide")));
    let (m, _) = ask(&c, of("sma_crossover", KernelOp::Contract)).unwrap();
    assert!(m.text.is_some_and(|t| t.contains("inv.nav") && t.contains("pre.binding")));
    let mut r = of("sma_crossover", KernelOp::Show);
    r.what = Some("plan above".into());
    let (m, _) = ask(&c, r).unwrap();
    assert!(m.text.is_some_and(|t| t.contains("sma_crossover::above#1") && !t.contains("sma_crossover::below#1")));
    // A strategy that does not check is refused with its diagnostics.
    let mut r = of("sma_crossover", KernelOp::Run);
    r.strategy = Some("nowhere".into());
    assert!(ask(&c, r).is_err_and(|e| e.contains("nowhere")));
}

#[test]
fn a_run_streams_its_progress_and_ends_as_the_run_in_process() {
    let (prog, ds) = common::golden_inputs(&common::corpus_dir().join("strategies/sma_crossover.dsl")).unwrap();
    let local = common::run(&prog, &ds, ExecConfig::from_program(&prog)).unwrap();
    let c = services(&prog, &ds);
    let mut r = of("sma_crossover", KernelOp::Run);
    r.progress_every = Some(50);
    let frames = run(&c, r);
    assert_eq!(frames[0].run_id.as_deref(), Some(local.run_id.as_str()), "the run's id first");
    let barriers: Vec<i64> = frames.iter().filter_map(|f| f.barrier).collect();
    assert_eq!(barriers.len(), local.bars.len() / 50, "a frame every 50 barriers");
    assert!(barriers.windows(2).all(|w| w[0] < w[1]));
    let last = frames.last().unwrap();
    assert_eq!(last.status.as_deref(), Some("completed"));
    let report: serde_json::Value = serde_json::from_str(last.report.as_deref().unwrap()).unwrap();
    assert_eq!(report["decisions"], serde_json::json!(local.decisions.len()));
    assert_eq!(report["final_cash"].as_f64().map(f64::to_bits), Some(local.final_cash.to_bits()));

    // The run it holds answers queries over its final state.
    let run_id = local.run_id.clone();
    let (m, rows) = query(&c, &run_id, "? decide(T, D) at T").unwrap();
    assert_eq!(rows, Some(local.decisions.len()), "{:?}", m.text);
    let d = &local.decisions[0];
    let day = abt_kernel::kernel::time::format_timestamp(d.t);
    let (m, rows) = query(&c, &run_id, &format!("trace decide at {}", day)).unwrap();
    assert_eq!(rows, Some(2 * local.symbols.len()), "{:?}", m.text);
    let (m, _) = query(&c, &run_id, "show calendars").unwrap();
    assert!(m.text.is_some_and(|t| t.contains("base")));
    // A `?` over more than one atom: the run holds it as a rule, run again
    // on the same binding (Definition V-8.1).
    let (m, rows) = query(&c, &run_id, &format!("? above(A, T), close(A, T, P), P > 1 USD/share at T = {}", day)).unwrap();
    let (_, above) = query(&c, &run_id, &format!("? above(A, T) at T = {}", day)).unwrap();
    assert_eq!(rows, above, "{:?}", m.text);
    // A query that does not check is refused.
    assert!(query(&c, &run_id, "? close(A, T, P), P > 1 at T = 2022-04-07").is_err_and(|e| e.contains("[T]")));
    // Status, and control of a run that ended.
    let mut r = request(KernelOp::Status);
    r.run_id = Some(run_id.clone());
    let (m, _) = ask(&c, r).unwrap();
    assert_eq!(m.status.as_deref(), Some("completed"));
    assert_eq!(m.decisions, Some(local.decisions.len() as u64));
    let mut r = request(KernelOp::Control);
    r.run_id = Some(run_id.clone());
    r.what = Some("halt".into());
    assert!(ask(&c, r).is_err_and(|e| e.contains("not running")));
    let mut r = request(KernelOp::Status);
    r.run_id = Some("0000000000000000".into());
    assert!(ask(&c, r).is_err_and(|e| e.contains("no run")));
}

/// K-7.3: while the run runs, a query is answered at the next barrier over
/// the state that barrier closed; a halt stops the run at the next barrier,
/// and the halted run's report says so.
#[test]
fn a_live_run_answers_between_barriers_and_halts_on_request() {
    let (prog, _) = common::golden_inputs(&common::corpus_dir().join("strategies/sma_crossover.dsl")).unwrap();
    // A market long enough that the run is still going when asked.
    let ds = abt_kernel::data::synthetic_for_program(&prog, &["AAA", "XYZ", "BBB", "SPY", "DDD"], (2000, 1, 3), 4000, 11);
    let c = services(&prog, &ds);
    let mut r = of("sma_crossover", KernelOp::Run);
    r.progress_every = Some(1);
    let (first, mut reply) = ask(&c, r).unwrap();
    let run_id = first.run_id.clone().unwrap();
    assert_eq!(first.status.as_deref(), Some("running"));
    let (m, rows) = query(&c, &run_id, "? decide(T, D) at T").unwrap();
    assert!(rows.is_some(), "{:?}", m.text);
    let mut r = request(KernelOp::Control);
    r.run_id = Some(run_id.clone());
    r.what = Some("halt".into());
    let (m, _) = ask(&c, r).unwrap();
    assert_eq!(m.status.as_deref(), Some("halting"));
    let mut last = next(&mut reply).unwrap();
    let mut barriers = 0;
    while last.status.as_deref() == Some("running") {
        barriers += 1;
        last = next(&mut reply).unwrap();
    }
    assert!(last.status.as_deref().is_some_and(|s| s.starts_with("halted(") && s.contains("control")), "{:?}", last.status);
    assert!(barriers < 4000, "the halt stopped the run");
    let mut r = request(KernelOp::Status);
    r.run_id = Some(run_id);
    let (m, _) = ask(&c, r).unwrap();
    assert!(m.status.is_some_and(|s| s.starts_with("halted(")));
}
