//! The kernel service (P-3.4) over the wire, in process: `check`,
//! `contract` and `show` from sources; a `run` that streams its progress and
//! ends as the run in process does (G3); queries of the run it holds,
//! between barriers while it runs (K-7.3) and over its final state after;
//! `status` and `control`; checkpoints through the data layer, and a run
//! resumed from one by its key (K-4.7); a `live-broker` run over the mock
//! broker, flattened and halted by `control` (K-6.5).

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
    services_over(prog, ds).0
}

/// `services`, and the data service.
fn services_over(prog: &Program, ds: &Dataset) -> (Client, Arc<abt_data::service::DataService>) {
    let svc = abt::serve_dataset(ds, prog).unwrap();
    let data = svc.clone();
    let connect = Arc::new(move || DataClient::connect(abt_data::service::inproc(&data)));
    let kernel = KernelService::new(connect);
    (Client::connect(inproc(&kernel), Layer::Front, "test").unwrap(), svc)
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
        checkpoint_every: None,
        from_checkpoint: None,
        horizon: None,
        plan_step: None,
        embargo_mask: None,
        observe: None,
        mode: None,
        session: None,
        deployment: None,
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

/// The checkpoint keys among a run's frames.
fn checkpoint_keys(frames: &[KernelResponse]) -> Vec<String> {
    frames.iter().filter_map(|f| f.text.as_deref()?.strip_prefix("checkpoint ").map(str::to_string)).collect()
}

/// K-4.7 over the service: a run asked for monthly checkpoints sends each
/// one's key as the data layer holds it; a run from one of them is the same
/// run, and ends as the unbroken run did.
#[test]
fn a_run_checkpoints_through_the_data_layer_and_resumes_from_a_key() {
    let (prog, ds) = common::golden_inputs(&common::corpus_dir().join("strategies/sma_crossover.dsl")).unwrap();
    let c = services(&prog, &ds);
    let mut r = of("sma_crossover", KernelOp::Run);
    r.checkpoint_every = Some("month".into());
    let frames = run(&c, r);
    let keys = checkpoint_keys(&frames);
    assert!(keys.len() >= 3, "{} checkpoints", keys.len());
    let full = frames.last().unwrap();
    assert_eq!(full.status.as_deref(), Some("completed"));
    let full_report: serde_json::Value = serde_json::from_str(full.report.as_deref().unwrap()).unwrap();
    for key in [&keys[0], &keys[keys.len() / 2], keys.last().unwrap()] {
        let mut r = of("sma_crossover", KernelOp::Run);
        r.from_checkpoint = Some(key.clone());
        let resumed = run(&c, r);
        assert_eq!(resumed[0].run_id, frames[0].run_id, "the same run");
        let last = resumed.last().unwrap();
        assert_eq!(last.status.as_deref(), Some("completed"), "{:?}", last.report);
        let report: serde_json::Value = serde_json::from_str(last.report.as_deref().unwrap()).unwrap();
        assert_eq!(report, full_report, "resumed from {}", key);
    }
    // Another window is another run: its checkpoint is refused, naming why.
    let mut r = of("sma_crossover", KernelOp::Run);
    r.from_checkpoint = Some(keys[0].clone());
    r.start = Some(common::run(&prog, &ds, ExecConfig::from_program(&prog)).unwrap().bars[40]);
    assert!(ask(&c, r).is_err_and(|e| e.contains("window")));
    let mut r = of("sma_crossover", KernelOp::Run);
    r.from_checkpoint = Some("0".repeat(64));
    assert!(ask(&c, r).is_err_and(|e| e.contains("no checkpoint")));
    let mut r = of("sma_crossover", KernelOp::Run);
    r.checkpoint_every = Some("fortnight".into());
    assert!(ask(&c, r).is_err_and(|e| e.contains("checkpoint_every")));
}

/// `control checkpoint` takes a checkpoint at the end of the running run's
/// next decision bar and answers its key; the run sends the same key.
#[test]
fn control_checkpoint_answers_the_key_of_a_live_run() {
    let (prog, _) = common::golden_inputs(&common::corpus_dir().join("strategies/sma_crossover.dsl")).unwrap();
    let ds = abt_kernel::data::synthetic_for_program(&prog, &["AAA", "XYZ", "BBB", "SPY", "DDD"], (2000, 1, 3), 4000, 11);
    let c = services(&prog, &ds);
    let mut r = of("sma_crossover", KernelOp::Run);
    r.progress_every = Some(1);
    let (first, mut reply) = ask(&c, r).unwrap();
    let run_id = first.run_id.clone().unwrap();
    let mut r = request(KernelOp::Control);
    r.run_id = Some(run_id.clone());
    r.what = Some("checkpoint".into());
    let (m, _) = ask(&c, r).unwrap();
    assert_eq!(m.status.as_deref(), Some("checkpointed"));
    let key = m.text.unwrap();
    assert_eq!(key.len(), 64);
    let mut r = request(KernelOp::Control);
    r.run_id = Some(run_id.clone());
    r.what = Some("halt".into());
    ask(&c, r).unwrap();
    let mut frames = vec![first];
    while frames.last().unwrap().status.as_deref() == Some("running") {
        frames.push(next(&mut reply).unwrap());
    }
    assert!(checkpoint_keys(&frames).contains(&key), "the run sent the key");
    assert!(frames.last().unwrap().status.as_deref().is_some_and(|s| s.starts_with("halted(")));
}

/// A run's events as the data layer holds them: each one's kind and reason.
fn logged(svc: &Arc<abt_data::service::DataService>, run: &str) -> Vec<(String, Option<String>)> {
    use abt_wire::generated::abt::wire::{QueryForm, Scan, View};
    use arrow::array::{Array, AsArray};
    let client = DataClient::connect(abt_data::service::inproc(svc)).unwrap();
    let view = View {
        versions: None,
        binding: None,
        as_of_seq: None,
    };
    let form = QueryForm::Scan(Box::new(Scan {
        relation: format!("kernel.{}.events", run),
        t0: i64::MIN,
        t1: i64::MAX,
        predicate: None,
        columns: None,
    }));
    let mut out = Vec::new();
    for b in client.query(&view, form).unwrap() {
        let col = |name: &str| arrow::compute::cast(b.column_by_name(name).unwrap(), &arrow::datatypes::DataType::Utf8).unwrap();
        let (kind, reason) = (col("kind"), col("reason"));
        let (kind, reason) = (kind.as_string::<i32>(), reason.as_string::<i32>());
        for i in 0..b.num_rows() {
            out.push((kind.value(i).to_string(), (!reason.is_null(i)).then(|| reason.value(i).to_string())));
        }
    }
    out
}

/// K-6.5 over the service: a `live-broker` run (the deployment's broker,
/// the mock) is flattened by `control flatten`, its closing decisions
/// logged as the control's, then halted by `control halt` through its kill
/// switch.
#[test]
fn a_live_broker_run_is_flattened_and_halted_by_control() {
    let (prog, _) = common::golden_inputs(&common::corpus_dir().join("strategies/sma_crossover.dsl")).unwrap();
    let ds = abt_kernel::data::synthetic_for_program(&prog, &["AAA", "XYZ", "BBB", "SPY", "DDD"], (2000, 1, 3), 4000, 11);
    let (c, svc) = services_over(&prog, &ds);
    let mut r = of("sma_crossover", KernelOp::Run);
    r.progress_every = Some(1);
    r.mode = Some("live-broker".into());
    r.deployment = Some("deployment { account DU0001 broker mock reconcile warn }".into());
    let (first, mut reply) = ask(&c, r).unwrap();
    let run_id = first.run_id.clone().unwrap();
    let control = |what: &str| {
        let mut r = request(KernelOp::Control);
        r.run_id = Some(run_id.clone());
        r.what = Some(what.into());
        ask(&c, r).unwrap().0.status
    };
    assert_eq!(control("flatten").as_deref(), Some("flattening"));
    // Two barriers on, the flatten has been taken; then the halt.
    for _ in 0..2 {
        assert_eq!(next(&mut reply).unwrap().status.as_deref(), Some("running"));
    }
    assert_eq!(control("halt").as_deref(), Some("halting"));
    let mut last = next(&mut reply).unwrap();
    while last.status.as_deref() == Some("running") {
        last = next(&mut reply).unwrap();
    }
    let status = last.status.unwrap_or_default();
    assert!(status.starts_with("halted(") && status.contains("kill switch"), "{}", status);
    let controls: Vec<Option<String>> = logged(&svc, &run_id).into_iter().filter(|(k, _)| k == "control").map(|(_, r)| r).collect();
    assert_eq!(controls, vec![Some("flatten_all".to_string()), Some("halt_all".to_string())]);
}

/// What `run` and `control` refuse of the live modes: flatten on a run
/// without a deployment, a broker this build has no adapter for, a
/// live-broker run without its document, a document on a replay.
#[test]
fn the_live_modes_refuse_what_they_cannot_do() {
    let (prog, ds) = common::golden_inputs(&common::corpus_dir().join("strategies/sma_crossover.dsl")).unwrap();
    let c = services(&prog, &ds);
    let frames = run(&c, of("sma_crossover", KernelOp::Run));
    let mut r = request(KernelOp::Control);
    r.run_id = frames[0].run_id.clone();
    r.what = Some("flatten".into());
    assert!(ask(&c, r).is_err_and(|e| e.contains("flatten acts on a live run")));
    let with = |mode: Option<&str>, deployment: Option<&str>| {
        let mut r = of("sma_crossover", KernelOp::Run);
        r.mode = mode.map(str::to_string);
        r.deployment = deployment.map(str::to_string);
        ask(&c, r).err().unwrap_or_default()
    };
    // A live run answers over its final state, and does not run again for
    // a `?` over more than one atom.
    let mut r = of("sma_crossover", KernelOp::Run);
    r.mode = Some("live-broker".into());
    r.deployment = Some("deployment { account DU0001 broker mock reconcile warn }".into());
    let frames = run(&c, r);
    let last = frames.last().unwrap();
    assert_eq!(last.status.as_deref(), Some("completed"), "{:?}", last.report);
    let report: serde_json::Value = serde_json::from_str(last.report.as_deref().unwrap()).unwrap();
    let live = last.run_id.clone().unwrap();
    let (m, rows) = query(&c, &live, "? decide(T, D) at T").unwrap();
    assert_eq!(rows.map(|n| n as u64), report["decisions"].as_u64(), "{:?}", m.text);
    let refused = query(&c, &live, "? above(A, T), close(A, T, P), P > 1 USD/share at T = 2022-04-07").unwrap_err();
    assert!(refused.contains("a live-broker run does not run again"), "{}", refused);
    let e = with(Some("live-broker"), Some("deployment { account U1 broker ibkr }"));
    assert!(e.contains("broker `ibkr`: this build has no adapter for it"), "{}", e);
    assert!(with(Some("live-broker"), None).contains("takes its deployment document"));
    assert!(with(None, Some("deployment { account U1 broker mock }")).contains("a deployment document is a live run's"));
    assert!(with(Some("live-broker"), Some("deployment { broker mock }")).contains("names its `account`"));
    assert!(with(Some("paper"), None).contains("is not replay-sim, live-sim or live-broker"));
}
