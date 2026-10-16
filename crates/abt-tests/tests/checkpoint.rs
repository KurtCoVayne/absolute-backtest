//! Checkpoints through the data layer (K-4.7, G5) and the identity of a run
//! (Definition A-4.1, A-6): every monthly checkpoint, written as a cache
//! object keyed by (run id, seq) and read back by its key, resumes to the
//! unbroken run to the bit, on every corpus program and both books; the run
//! id is BLAKE3 over (H(P), β, π, mode), and a parameter point is part of β;
//! H(P) is stable (a golden file, blessed with ABT_BLESS=1 in a commit
//! naming the change).

#![allow(clippy::result_large_err)]

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;

use abt_kernel::kernel::{checkpoints, on_kernel_thread, replay, CheckpointEvery, DataClient, ExecConfig, Replay, RunError, RunResult, SimExecutor};
use abt_tests as common;
use abt_tests::golden_inputs;

fn digest(r: &RunResult) -> Vec<String> {
    let mut out = vec![format!("bars {}", r.bars.len())];
    for d in &r.decisions {
        out.push(format!("decision {} {} {}", d.t, r.describe_decision(&d.decision), d.rule));
    }
    for f in &r.fills {
        out.push(format!("fill {} {} {:x} {:x}", f.t, f.equity, f.quantity.to_bits(), f.price.to_bits()));
    }
    for (t, e) in &r.equity_curve {
        out.push(format!("equity {} {:x}", t, e.to_bits()));
    }
    for x in &r.exposure {
        out.push(format!("book {} {:x} {:x} {:x}", x.t, x.cash.to_bits(), x.equity.to_bits(), x.receivables.to_bits()));
    }
    for e in &r.events {
        out.push(format!(
            "event {} {} {} {:?} {:?} {:?}",
            e.seq,
            e.t,
            e.kind.name(),
            e.instrument,
            e.order_id,
            e.cash_flow.map(f64::to_bits)
        ));
    }
    out.push(format!("final_cash {:x}", r.final_cash.to_bits()));
    out
}

/// A replay of `p` over `client`; with checkpoints asked for, each written
/// through the data layer, its key kept.
fn replayed(p: &abt_kernel::check::Program, client: &DataClient, cfg: &ExecConfig, opts: &Replay) -> Result<(RunResult, Vec<String>), RunError> {
    on_kernel_thread(|| {
        let mut exec = SimExecutor::new(cfg.clone());
        let mut keys = Vec::new();
        let mut written: Result<(), RunError> = Ok(());
        let (r, k) = replay(p, None, client, cfg.clone(), &mut exec, opts, &mut |cp| match checkpoints::write(client, &cp) {
            Ok(key) => keys.push(key),
            Err(e) => written = Err(e),
        })?;
        std::mem::forget(k);
        written.map(|_| (r, keys))
    })
}

/// Every monthly checkpoint of one program, written through the data layer
/// and read back by its key, resumes to the unbroken run; the number of
/// checkpoints.
fn every_checkpoint_resumes(path: &Path) -> Result<usize, String> {
    let (p, ds) = golden_inputs(path)?;
    let name = p.strategy.clone();
    let cfg = ExecConfig::from_program(&p);
    let client = common::client(&p, &ds);
    let monthly = Replay {
        checkpoint_every: Some(CheckpointEvery::Month),
        ..Replay::default()
    };
    let (full, keys) = replayed(&p, &client, &cfg, &monthly).map_err(|e| format!("{}: {}", name, e))?;
    if keys.is_empty() {
        return Err(format!("{}: no checkpoint", name));
    }
    if keys.iter().collect::<BTreeSet<_>>().len() != keys.len() {
        return Err(format!("{}: two checkpoints under one key", name));
    }
    let want = digest(&full);
    for key in &keys {
        let cp = checkpoints::read(&client, key).map_err(|e| format!("{}: {}", name, e))?;
        if cp.identity.run_id != full.run_id || *key != checkpoints::key(&full.run_id, cp.cursor) {
            return Err(format!("{}: the checkpoint {} is not keyed by its run and seq", name, key));
        }
        let bar = cp.last_bar;
        let (resumed, _) = replayed(&p, &client, &cfg, &Replay { from: Some(cp), ..Replay::default() }).map_err(|e| format!("{}: {}", name, e))?;
        let got = digest(&resumed);
        if got != want {
            let at = got.iter().zip(&want).position(|(a, b)| a != b).unwrap_or(got.len().min(want.len()));
            return Err(format!(
                "{}: resumed from the checkpoint of {} differs at line {}: {:?} against {:?}",
                name,
                bar,
                at,
                got.get(at),
                want.get(at)
            ));
        }
    }
    Ok(keys.len())
}

/// G5: every monthly checkpoint of every corpus program and both books
/// resumes to the unbroken run.
#[test]
fn every_monthly_checkpoint_resumes_to_the_unbroken_run() {
    let programs = common::golden_programs();
    let next = AtomicUsize::new(0);
    let outcomes = Mutex::new(Vec::new());
    std::thread::scope(|s| {
        for _ in 0..4 {
            s.spawn(|| {
                while let Some(path) = programs.get(next.fetch_add(1, Ordering::SeqCst)) {
                    let r = every_checkpoint_resumes(path);
                    outcomes.lock().unwrap().push((path.clone(), r));
                }
            });
        }
    });
    let outcomes = outcomes.into_inner().unwrap();
    assert_eq!(outcomes.len(), programs.len());
    let failed: Vec<String> = outcomes.iter().filter_map(|(_, r)| r.as_ref().err().cloned()).collect();
    assert!(failed.is_empty(), "{}", failed.join("\n"));
    // The books are among them, and a daily program checkpoints monthly.
    assert!(outcomes.iter().any(|(p, r)| p.ends_with("company/mw14.dsl") && r.is_ok()));
    assert!(outcomes.iter().any(|(p, r)| p.ends_with("strategies/sma_crossover.dsl") && r.as_ref().is_ok_and(|n| *n >= 12)));
}

#[test]
fn a_run_is_identified_by_its_program_binding_step_and_mode() {
    let (p, ds) = golden_inputs(&common::corpus_dir().join("strategies/sma_crossover.dsl")).unwrap();
    let client = common::client(&p, &ds);
    let cfg = ExecConfig::from_program(&p);
    let a = replayed(&p, &client, &cfg, &Replay::default()).unwrap().0;
    assert_eq!(a.run_id.len(), 64, "32 bytes of BLAKE3, hex");
    assert!(a.run_id.chars().all(|c| c.is_ascii_hexdigit()));
    let b = replayed(&p, &client, &cfg, &Replay::default()).unwrap().0;
    assert_eq!((a.run_id.as_str(), a.binding_id.as_str()), (b.run_id.as_str(), b.binding_id.as_str()), "the same run is the same id");
    // A parameter point is part of β (D-7.5): another point, another binding.
    let mut moved = cfg.clone();
    moved.param_overrides = vec![("fast".into(), abt_kernel::syntax::parse_lit("15d").unwrap())];
    let c = replayed(&p, &client, &moved, &Replay::default()).unwrap().0;
    assert_ne!(c.binding_id, a.binding_id);
    assert_ne!(c.run_id, a.run_id);
    // The default written as an override is the same point.
    let mut same = cfg.clone();
    same.param_overrides = vec![("fast".into(), abt_kernel::syntax::parse_lit("20d").unwrap())];
    assert_eq!(replayed(&p, &client, &same, &Replay::default()).unwrap().0.run_id, a.run_id);
    // Another window is another run of the same program and binding.
    let mut window = cfg.clone();
    window.start = a.bars.get(40).copied();
    let d = replayed(&p, &client, &window, &Replay::default()).unwrap().0;
    assert_eq!(d.binding_id, a.binding_id);
    assert_ne!(d.run_id, a.run_id);
    // A checkpoint of one is refused by the other, naming why.
    let every = Replay {
        checkpoint_every: Some(CheckpointEvery::Bars(30)),
        ..Replay::default()
    };
    let (_, keys) = replayed(&p, &client, &cfg, &every).unwrap();
    let cp = checkpoints::read(&client, &keys[0]).unwrap();
    let from = Replay { from: Some(cp), ..Replay::default() };
    let e = replayed(&p, &client, &moved, &from).err().unwrap();
    assert!(e.to_string().contains("another binding"), "{}", e);
    let e = replayed(&p, &client, &window, &from).err().unwrap();
    assert!(e.to_string().contains("window"), "{}", e);
    assert!(checkpoints::read(&client, &"0".repeat(64)).is_err_and(|e| e.to_string().contains("no checkpoint")));
}

fn golden() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests").join("golden").join("program_hashes.tsv")
}

/// H(P) is stable across releases (A-6): every corpus program's and both
/// books' hashes as written. A change to the normal form changes them all
/// and is blessed with ABT_BLESS=1 in a commit that says so.
#[test]
fn program_hashes_are_stable() {
    let mut text = String::new();
    for path in common::golden_programs() {
        let (p, _) = golden_inputs(&path).unwrap();
        let h = abt_kernel::compiler::normalise(&p).hash;
        assert_eq!(h.len(), 64);
        text.push_str(&format!("{}\t{}\n", p.strategy, h));
    }
    if std::env::var_os("ABT_BLESS").is_some() {
        std::fs::write(golden(), &text).unwrap();
        return;
    }
    let want = std::fs::read_to_string(golden()).expect("tests/golden/program_hashes.tsv (ABT_BLESS=1 writes it)");
    assert_eq!(text, want, "H(P) changed; bless with ABT_BLESS=1 in a commit naming the change");
}
