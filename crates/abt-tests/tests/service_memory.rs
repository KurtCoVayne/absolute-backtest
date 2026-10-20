//! E-110: the memory a run takes goes back to the system once the run is
//! released. The services handle each request, and the kernel each run, on
//! a thread of its own; glibc kept every thread's arena at the peak of what
//! it served (a plan's trials grew a process by gigabytes each, the data
//! services held 10 and 18 GB past their resident budgets). One test in its
//! own binary: it reads the process's resident anonymous memory.

#![allow(clippy::result_large_err)]

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use abt_kernel::kernel::service::{inproc, KernelService};
use abt_kernel::kernel::DataClient;
use abt_tests as common;
use abt_wire::generated::abt::wire::{KernelOp, KernelRequest, KernelResponse, Message, SourceFile};
use abt_wire::memory::anonymous_bytes;
use abt_wire::rpc::{Client, Reply};
use abt_wire::Layer;

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
        received_by: None,
        plan_step: None,
        embargo_mask: None,
        observe: None,
        mode: None,
        session: None,
        deployment: None,
    }
}

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

fn next(reply: &mut Reply) -> KernelResponse {
    match reply.message() {
        Ok(Message::KernelResponse(m)) => *m,
        other => panic!("not a kernel response: {:?}", other.map(|_| ())),
    }
}

/// A run to its last frame, then released: its id.
fn run_and_release(c: &Client, strategy: &str) -> String {
    let mut r = request(KernelOp::Run);
    r.strategy = Some(strategy.into());
    r.sources = Some(sources(strategy));
    let mut reply = c.call(Message::KernelRequest(Box::new(r))).unwrap();
    let mut m = next(&mut reply);
    let mut run_id = m.run_id.clone().unwrap_or_default();
    while m.status.as_deref() == Some("running") {
        m = next(&mut reply);
        if let Some(id) = m.run_id.clone().filter(|id| !id.is_empty()) {
            run_id = id;
        }
    }
    assert_eq!(m.status.as_deref(), Some("completed"), "{:?}", m.report);
    let mut rel = request(KernelOp::Control);
    rel.run_id = Some(run_id.clone());
    rel.what = Some("release".into());
    let mut reply = c.call(Message::KernelRequest(Box::new(rel))).unwrap();
    assert_eq!(next(&mut reply).status.as_deref(), Some("released"));
    run_id
}

#[test]
fn a_released_runs_memory_goes_back_to_the_system() {
    if anonymous_bytes().is_none() {
        eprintln!("no /proc/self/status here: nothing measured");
        return;
    }
    let strategy = "momentum_top_n";
    let names: Vec<String> = (0..300).map(|i| format!("S{:03}", i)).collect();
    let syms: Vec<&str> = names.iter().map(|s| s.as_str()).collect();
    let ds = abt_kernel::data::synthetic_daily(&syms, (2018, 1, 2), 1000, 7);
    let prog = common::corpus_program(strategy);
    let svc = abt::serve_dataset(&ds, &prog).unwrap();
    drop(ds);
    let data = svc.clone();
    let connect = Arc::new(move || DataClient::connect(abt_data::service::inproc(&data)));
    let kernel = KernelService::new(connect);
    let c = Client::connect(inproc(&kernel), Layer::Front, "test").unwrap();
    // The first run fills what stays: the data service's resident row
    // groups, the registry, the store's maps.
    run_and_release(&c, strategy);
    std::thread::sleep(Duration::from_millis(500));
    // Each later run: its peak, sampled while it runs, and what stays once
    // it is released and its thread ended.
    let mb = |b: u64| b / (1 << 20);
    for k in 2..=4 {
        let before = anonymous_bytes().unwrap();
        let peak = Arc::new(AtomicU64::new(before));
        let running = Arc::new(AtomicBool::new(true));
        let sampler = {
            let (peak, running) = (peak.clone(), running.clone());
            std::thread::spawn(move || {
                while running.load(Ordering::Relaxed) {
                    peak.fetch_max(anonymous_bytes().unwrap_or(0), Ordering::Relaxed);
                    std::thread::sleep(Duration::from_millis(5));
                }
            })
        };
        run_and_release(&c, strategy);
        running.store(false, Ordering::Relaxed);
        sampler.join().unwrap();
        let peak = peak.load(Ordering::Relaxed);
        let grew = peak.saturating_sub(before);
        // The released run's thread ends and trims: wait for it.
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut after = anonymous_bytes().unwrap();
        while after.saturating_sub(before) > grew / 4 && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(50));
            after = anonymous_bytes().unwrap();
        }
        eprintln!("run {}: before {} MB, peak {} MB, after {} MB", k, mb(before), mb(peak), mb(after));
        assert!(grew > 16 << 20, "run {} grew the process by {} MB only: the test measures nothing", k, mb(grew));
        assert!(
            after.saturating_sub(before) <= grew / 4,
            "run {}: {} MB of the {} MB it took stayed after its release",
            k,
            mb(after.saturating_sub(before)),
            mb(grew)
        );
    }
}
