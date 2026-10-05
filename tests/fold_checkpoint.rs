//! Checkpoints (data-bundle doc, section 2): the fold's state is
//! serialisable; a replay that stops at a checkpoint, restores it into a
//! fresh kernel and continues produces the run the unbroken fold and the
//! batch kernel produce, to the bit.

mod corpus;

use std::fs;

use absolute_backtest::check::{check_program, Program};
use absolute_backtest::data::synthetic_daily;
use absolute_backtest::kernel::time::{format_timestamp, month_key};
use absolute_backtest::kernel::{run, run_fold, Checkpoint, CheckpointEvery, Event, EventLog, ExecConfig, Fold, Kernel, RunError, RunResult, SimExecutor};

fn program(src: &str, name: &str) -> Program {
    let mut ws = corpus::base_workspace();
    ws.add_source(src).unwrap();
    let (p, diags) = check_program(&ws, name);
    let text: Vec<String> = diags.iter().map(|d| d.to_string()).collect();
    p.unwrap_or_else(|| panic!("{} should check:\n{}", name, text.join("\n")))
}

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
    program(
        &fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/corpus/strategies/momentum_top_n.dsl")).unwrap(),
        "momentum_top_n",
    )
}

#[test]
fn a_replay_resumed_from_a_checkpoint_is_the_unbroken_run() {
    let p = momentum();
    let ds = synthetic_daily(&SYMS, (2022, 1, 3), 320, 7);
    let cfg = ExecConfig::default();
    let reference = run(&p, &ds, cfg.clone()).unwrap();
    let unbroken = run_fold(&p, &ds, cfg.clone()).unwrap();
    assert_eq!(digest(&reference), digest(&unbroken));
    // Fold with monthly checkpoints; keep them all.
    let kernel = Kernel::new_streaming(&p, &ds, cfg.clone()).unwrap();
    let log = EventLog::from_dataset(&kernel, &ds).unwrap();
    let mut exec = SimExecutor::new(cfg.clone());
    let mut fold = Fold::new(kernel, &mut exec).with_checkpoints(CheckpointEvery::Month);
    let mut checkpoints: Vec<Checkpoint> = Vec::new();
    for ev in &log.events {
        fold.step(ev.clone()).unwrap();
        if let Some(cp) = fold.take_checkpoint() {
            checkpoints.push(cp);
        }
    }
    let (with_checkpoints, _) = fold.finish().unwrap();
    assert_eq!(digest(&reference), digest(&with_checkpoints), "taking checkpoints changes nothing");
    // One per month boundary of the decision bars.
    let months: std::collections::BTreeSet<(i64, u32)> = reference.bars.iter().map(|t| month_key(*t)).collect();
    assert_eq!(checkpoints.len(), months.len() - 1, "{:?}", checkpoints.iter().map(|c| format_timestamp(c.cursor)).collect::<Vec<_>>());
    for cp in &checkpoints {
        assert!(cp.cursor > cp.last_bar, "the cursor is the first event after the checkpointed bar");
        assert_ne!(month_key(cp.cursor), month_key(cp.last_bar));
    }
    // Resume from each checkpoint through JSON, replay the rest: the same run.
    for cp in &checkpoints {
        let text = serde_json::to_string(cp).unwrap();
        let cp: Checkpoint = serde_json::from_str(&text).unwrap();
        let cursor = cp.cursor;
        let kernel = Kernel::new_streaming(&p, &ds, cfg.clone()).unwrap();
        let mut exec = SimExecutor::new(cfg.clone());
        let mut fold = Fold::restore(kernel, &mut exec, cp).unwrap();
        for ev in &log.events {
            if let Event::Tuple { avail, .. } = ev {
                if *avail < cursor {
                    continue;
                }
            }
            fold.step(ev.clone()).unwrap();
        }
        let (resumed, _) = fold.finish().unwrap();
        let (da, db) = (digest(&reference), digest(&resumed));
        if da != db {
            let first = da.iter().zip(&db).position(|(x, y)| x != y).unwrap_or(da.len().min(db.len()));
            panic!(
                "resumed from {}: differs at line {} of {} / {}:\n  unbroken: {}\n  resumed:  {}",
                format_timestamp(cursor),
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
    let cfg = ExecConfig::default();
    let kernel = Kernel::new_streaming(&p, &ds, cfg.clone()).unwrap();
    let log = EventLog::from_dataset(&kernel, &ds).unwrap();
    let mut exec = SimExecutor::new(cfg.clone());
    let mut fold = Fold::new(kernel, &mut exec).with_checkpoints(CheckpointEvery::Bars(30));
    let mut cp = None;
    for ev in &log.events {
        fold.step(ev.clone()).unwrap();
        if let Some(c) = fold.take_checkpoint() {
            cp = Some(c);
            break;
        }
    }
    let cp = cp.expect("a checkpoint after thirty bars");
    assert_eq!(cp.result.bars.len(), 30);
    // Another configuration.
    let other_cfg = ExecConfig { initial_cash: 5.0, ..cfg.clone() };
    let kernel = Kernel::new_streaming(&p, &ds, other_cfg.clone()).unwrap();
    let mut exec = SimExecutor::new(other_cfg);
    let err = Fold::restore(kernel, &mut exec, cp.clone()).err().unwrap();
    assert!(matches!(err, RunError::Config(_)), "{}", err);
    assert!(err.to_string().contains("checkpoint"), "{}", err);
    // Another dataset (more symbols).
    let ds2 = synthetic_daily(&["AAA", "BBB", "CCC", "DDD", "SPY", "ZZZ"], (2022, 1, 3), 120, 7);
    let kernel = Kernel::new_streaming(&p, &ds2, cfg.clone()).unwrap();
    let mut exec = SimExecutor::new(cfg.clone());
    let err = Fold::restore(kernel, &mut exec, cp.clone()).err().unwrap();
    assert!(err.to_string().contains("symbols"), "{}", err);
    // Another program.
    let sma = program(
        &fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/corpus/strategies/sma_crossover.dsl")).unwrap(),
        "sma_crossover",
    );
    let kernel = Kernel::new_streaming(&sma, &ds, cfg.clone()).unwrap();
    let mut exec = SimExecutor::new(cfg);
    assert!(Fold::restore(kernel, &mut exec, cp).is_err());
}
