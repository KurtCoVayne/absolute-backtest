//! Checkpoints (K-4.7): the fold's state is serialisable with the stream's
//! cursor; a replay that restores one into a fresh kernel and subscribes
//! from its cursor produces the unbroken run to the bit, and a checkpoint is
//! refused by another program, configuration or binding (G5).

#![allow(clippy::result_large_err)]

use abt_tests as common;

use abt_kernel::check::Program;
use abt_kernel::data::synthetic_daily;
use abt_kernel::kernel::time::{format_timestamp, month_key};
use abt_kernel::kernel::{replay, Checkpoint, CheckpointEvery, ExecConfig, Replay, RunError, RunResult, SimExecutor};
use abt_tests::{run, run_socket};

const SYMS: [&str; 5] = ["AAA", "BBB", "CCC", "DDD", "SPY"];

fn digest(r: &RunResult) -> Vec<String> {
    let mut out = Vec::new();
    out.push(format!("bars {}", r.bars.len()));
    for d in &r.decisions {
        out.push(format!("decision {} {} {}", d.t, r.describe_decision(&d.decision), d.rule));
    }
    for f in &r.fills {
        out.push(format!(
            "fill {} {} {:x} {:x} {:x}",
            f.t,
            r.symbols[f.equity as usize],
            f.quantity.to_bits(),
            f.price.to_bits(),
            f.commission.to_bits()
        ));
    }
    for (t, e) in &r.equity_curve {
        out.push(format!("equity {} {:x}", t, e.to_bits()));
    }
    out.push(format!("dropped {}", r.dropped.len()));
    out.push(format!("costs {:?} funding {:?} liquidity {:?}", r.costs, r.funding, r.liquidity));
    out.push(format!("warnings {:?}", r.warnings));
    out.push(format!("final_cash {:x} positions {:?}", r.final_cash.to_bits(), r.final_positions));
    out
}

fn momentum() -> Program {
    common::corpus_program("momentum_top_n")
}

/// A replay of `p` over `client` with the options, every checkpoint kept.
fn replayed(p: &Program, client: &abt_kernel::kernel::DataClient, cfg: &ExecConfig, opts: &Replay) -> Result<(RunResult, Vec<Checkpoint>), RunError> {
    let mut exec = SimExecutor::new(cfg.clone());
    let mut kept = Vec::new();
    let (r, _) = replay(p, None, client, cfg.clone(), &mut exec, opts, &mut |cp| kept.push(cp))?;
    Ok((r, kept))
}

#[test]
fn a_replay_resumed_from_a_checkpoint_is_the_unbroken_run() {
    let p = momentum();
    let ds = synthetic_daily(&SYMS, (2022, 1, 3), 320, 7);
    let cfg = ExecConfig::from_program(&p);
    let reference = run(&p, &ds, cfg.clone()).unwrap();
    let unbroken = run_socket(&p, &ds, cfg.clone()).unwrap();
    assert_eq!(digest(&reference), digest(&unbroken));
    // The fold with monthly checkpoints; keep them all.
    let client = common::client(&p, &ds);
    let monthly = Replay {
        checkpoint_every: Some(CheckpointEvery::Month),
        ..Replay::default()
    };
    let (with_checkpoints, checkpoints) = replayed(&p, &client, &cfg, &monthly).unwrap();
    assert_eq!(digest(&reference), digest(&with_checkpoints), "taking checkpoints changes nothing");
    // One per month boundary of the decision bars, taken at the month's
    // last bar, resuming at the event after it.
    let months: std::collections::BTreeSet<(i64, u32)> = reference.bars.iter().map(|t| month_key(*t)).collect();
    assert_eq!(
        checkpoints.len(),
        months.len() - 1,
        "{:?}",
        checkpoints.iter().map(|c| format_timestamp(c.last_bar)).collect::<Vec<_>>()
    );
    for cp in &checkpoints {
        let i = reference.bars.iter().position(|b| *b == cp.last_bar).unwrap();
        assert_ne!(month_key(reference.bars[i + 1]), month_key(cp.last_bar), "a checkpoint ends a month");
        assert!(cp.cursor > 1);
    }
    // Resume from each checkpoint through JSON: the same run.
    for cp in &checkpoints {
        let text = serde_json::to_string(cp).unwrap();
        let cp: Checkpoint = serde_json::from_str(&text).unwrap();
        let last = cp.last_bar;
        let (resumed, _) = replayed(&p, &client, &cfg, &Replay { from: Some(cp), ..Replay::default() }).unwrap();
        let (da, db) = (digest(&reference), digest(&resumed));
        if da != db {
            let first = da.iter().zip(&db).position(|(x, y)| x != y).unwrap_or(da.len().min(db.len()));
            panic!(
                "resumed after {}: differs at line {} of {} / {}:\n  unbroken: {}\n  resumed:  {}",
                format_timestamp(last),
                first,
                da.len(),
                db.len(),
                da.get(first).cloned().unwrap_or_default(),
                db.get(first).cloned().unwrap_or_default()
            );
        }
    }
}

#[test]
fn a_checkpoint_of_another_program_or_configuration_is_refused() {
    let p = momentum();
    let ds = synthetic_daily(&SYMS, (2022, 1, 3), 120, 7);
    let cfg = ExecConfig::from_program(&p);
    let client = common::client(&p, &ds);
    let every30 = Replay {
        checkpoint_every: Some(CheckpointEvery::Bars(30)),
        ..Replay::default()
    };
    let (_, cps) = replayed(&p, &client, &cfg, &every30).unwrap();
    let cp = cps.into_iter().next().expect("a checkpoint after thirty bars");
    assert_eq!(cp.result.bars.len(), 30);
    let from = |cp: &Checkpoint| Replay {
        from: Some(cp.clone()),
        ..Replay::default()
    };
    // Another configuration.
    let other_cfg = ExecConfig { initial_cash: 5.0, ..cfg.clone() };
    let err = replayed(&p, &client, &other_cfg, &from(&cp)).err().unwrap();
    assert!(matches!(err, RunError::Config(_)), "{}", err);
    assert!(err.to_string().contains("another configuration"), "{}", err);
    // Another binding (more symbols).
    let ds2 = synthetic_daily(&["AAA", "BBB", "CCC", "DDD", "SPY", "ZZZ"], (2022, 1, 3), 120, 7);
    let client2 = common::client(&p, &ds2);
    let err = replayed(&p, &client2, &cfg, &from(&cp)).err().unwrap();
    assert!(err.to_string().contains("another binding"), "{}", err);
    // Another program.
    let sma = common::corpus_program("sma_crossover");
    let err = replayed(&sma, &client, &cfg, &from(&cp)).err().unwrap();
    assert!(err.to_string().contains("another program"), "{}", err);
}
