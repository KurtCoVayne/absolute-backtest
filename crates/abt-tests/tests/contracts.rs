//! Contracts derived from structure (K-3, G7): every clause of every corpus
//! program and both books is proven, checked or reported after a run; every
//! invariant, made to fail at a bar, halts the run there naming its clause;
//! the preconditions refuse before the first barrier; the pre-trade limits
//! (K-3.5) reject, scale, warn or halt as their policy says, and the checker
//! refuses a limit written wrong.

#![allow(clippy::result_large_err)]

use abt_kernel::contract::{contract, Kind, Status};
use abt_kernel::kernel::{EventKind, ExecConfig, RunError};
use abt_tests as common;
use abt_tests::{golden_inputs, golden_programs, run, StrategyParts};

#[test]
fn every_clause_of_every_corpus_program_is_proven_checked_or_reported() {
    let mut programs = 0;
    for f in golden_programs() {
        let (p, ds) = golden_inputs(&f).unwrap();
        let derived = contract(&p);
        // The contract is a function of the program (Proposition K-3.2).
        assert_eq!(derived, contract(&p));
        assert_eq!(derived.clauses.iter().filter(|c| c.kind == Kind::Pre).count(), 16, "{}", p.strategy);
        assert_eq!(derived.clauses.iter().filter(|c| c.kind == Kind::Inv).count(), 10, "{}", p.strategy);
        assert_eq!(derived.clauses.iter().filter(|c| c.kind == Kind::Post).count(), 7, "{}", p.strategy);
        let Ok(r) = run(&p, &ds, ExecConfig::from_program(&p)) else { continue };
        programs += 1;
        if let Err(open) = r.contract.settled() {
            panic!("{}: clauses not settled:\n{}", p.strategy, open.join("\n"));
        }
        // Every invariant was checked at every decision barrier it ran.
        for c in r.contract.clauses.iter().filter(|c| c.kind == Kind::Inv && c.id != "inv.keyed_recursive") {
            assert!(matches!(c.status, Status::Checked { times } if times > 0), "{}: {} {}", p.strategy, c.id, c.status);
        }
    }
    assert!(programs >= 20, "{} programs ran", programs);
}

/// The sma crossover on its golden market, with a fault made at its 30th bar.
fn faulted(clause: &str) -> (Result<abt_kernel::kernel::RunResult, RunError>, i64) {
    let (p, ds) = golden_inputs(&abt_tests::corpus_dir().join("strategies").join("sma_crossover.dsl")).unwrap();
    let base = run(&p, &ds, ExecConfig::from_program(&p)).unwrap();
    let at = base.bars[30];
    let cfg = ExecConfig {
        inject: Some((clause.to_string(), at)),
        ..ExecConfig::from_program(&p)
    };
    (run(&p, &ds, cfg), at)
}

#[test]
fn every_injected_invariant_violation_halts() {
    let runtime: Vec<String> = contract(&common::corpus_program("sma_crossover"))
        .clauses
        .iter()
        .filter(|c| c.kind == Kind::Inv && c.id != "inv.keyed_recursive")
        .map(|c| c.id.clone())
        .collect();
    assert_eq!(runtime.len(), 9);
    for clause in runtime {
        let (r, at) = faulted(&clause);
        match r {
            Err(RunError::Invariant { clause: c, t, .. }) => {
                assert_eq!(c, clause);
                assert_eq!(t, at, "{} halts at the bar of the fault", clause);
            }
            Err(e) => panic!("{}: halted with another error: {}", clause, e),
            Ok(_) => panic!("{}: the fault did not halt the run", clause),
        }
    }
}

#[test]
fn a_precondition_refuses_before_the_first_barrier() {
    // The derived state is bounded by the memory budget (K-4.6).
    let p = common::corpus_program("sma_crossover");
    let (_, ds) = golden_inputs(&abt_tests::corpus_dir().join("strategies").join("sma_crossover.dsl")).unwrap();
    let cfg = ExecConfig {
        memory_budget: Some(1),
        ..ExecConfig::from_program(&p)
    };
    match run(&p, &ds, cfg) {
        Err(RunError::Precondition { clause, message }) => {
            assert_eq!(clause, "pre.state_bound");
            assert!(message.contains("memory budget"), "{}", message);
        }
        other => panic!("{:?}", other.map(|r| r.bars.len())),
    }
    // A price that is not positive, on a share, is refused, naming the row.
    let src = common::strategy("pos", "    qty : Quantity<Shares> = 10 shares", "", "    decide(T, trade(A, qty)) :- universe(A, T), flat(A, T).");
    let p = common::program(&src, "pos");
    let ds = common::market("AAA", &[10.0, 11.0, 0.0, 12.0]);
    match run(&p, &ds, ExecConfig::from_program(&p)) {
        Err(RunError::Precondition { clause, message }) => {
            assert_eq!(clause, "pre.prices");
            assert!(message.contains("AAA") && message.contains("2024-01-10"), "{}", message);
        }
        other => panic!("{:?}", other.map(|r| r.bars.len())),
    }
}

/// Five names held at `w` each, every bar, under `limits`.
fn held(w: &str, limits: &str) -> abt_kernel::check::Program {
    let src = StrategyParts {
        name: "lim".into(),
        params: format!("    w : Scalar = {}", w),
        decides: "    decide(T, hold(A, w)) :- universe(A, T).".into(),
        execution: format!("equity_default with {{ leverage unconstrained   limits {{ {} }} }}", limits),
        ..Default::default()
    }
    .render();
    common::program(&src, "lim")
}

fn market() -> abt_kernel::kernel::Dataset {
    abt_kernel::data::synthetic_daily(&["AAA", "BBB", "CCC", "DDD", "EEE"], (2022, 1, 3), 60, 5)
}

fn triggered(r: &abt_kernel::kernel::RunResult) -> Vec<(String, String)> {
    r.events
        .iter()
        .filter_map(|e| match &e.kind {
            EventKind::LimitTriggered { limit, policy, .. } => Some((limit.clone(), policy.clone())),
            _ => None,
        })
        .collect()
}

#[test]
fn a_gross_limit_rejects_clamps_warns_or_halts() {
    let ds = market();
    // Five names at 0.4 are a gross of 2: rejected, every increasing order.
    let p = held("0.4", "gross <= 1 reject");
    let r = run(&p, &ds, ExecConfig::from_program(&p)).unwrap();
    assert!(r.fills.is_empty(), "{:?}", r.fills.first());
    let rejected = r.events.iter().filter(|e| matches!(e.kind, EventKind::Rejected { .. })).count();
    assert_eq!(rejected, r.decisions.len());
    assert!(r.dropped.iter().all(|(_, _, why)| why.contains("limit `gross <= 1 reject`")), "{:?}", r.dropped.first());
    let c = r.contract.get("limit.gross").unwrap();
    // It fires at every bar: the limits act when the orders are sent.
    assert_eq!(c.fired.len(), r.bars.len());
    // Scaled to the limit: each name at 0.2 of the capital at the decision's mark.
    let p = held("0.4", "gross <= 1 clamp");
    let r = run(&p, &ds, ExecConfig::from_program(&p)).unwrap();
    assert!(!r.fills.is_empty());
    let first = r.exposure.iter().find(|x| x.gross > 0.0).unwrap();
    assert!(first.gross / first.capital < 1.03 && first.gross / first.capital > 0.95, "{:?}", first);
    assert!(triggered(&r).iter().any(|(l, p)| l == "gross <= 1 clamp" && p == "clamp"));
    // A warning lets the orders through.
    let p = held("0.4", "gross <= 1 warn");
    let r = run(&p, &ds, ExecConfig::from_program(&p)).unwrap();
    let first = r.exposure.iter().find(|x| x.gross > 0.0).unwrap();
    assert!(first.gross / first.capital > 1.9, "{:?}", first);
    assert!(matches!(r.contract.get("limit.gross").unwrap().status, Status::Warned { count, .. } if count > 0));
    // A halt stops the run at the first bar that would pass it.
    let p = held("0.4", "gross <= 1 halt");
    match run(&p, &ds, ExecConfig::from_program(&p)) {
        Err(RunError::Risk { message, .. }) => assert!(message.contains("limit `gross <= 1 halt`"), "{}", message),
        other => panic!("{:?}", other.map(|r| r.bars.len())),
    }
}

#[test]
fn a_weight_limit_is_proven_when_the_weights_are_within_it_and_clamps_when_not() {
    let ds = market();
    let p = held("0.1", "weight <= 0.15 per name reject");
    let r = run(&p, &ds, ExecConfig::from_program(&p)).unwrap();
    assert_eq!(r.contract.get("limit.weight").unwrap().status, Status::Proven { tier: 1 });
    assert!(triggered(&r).is_empty());
    let p = held("0.3", "weight <= 0.2 per name clamp");
    assert!(matches!(contract(&p).get("limit.weight").unwrap().status, Status::Open));
    let r = run(&p, &ds, ExecConfig::from_program(&p)).unwrap();
    let first = r.exposure.iter().find(|x| x.gross > 0.0).unwrap();
    // Five names at 0.2 each.
    assert!(first.gross / first.capital < 1.03 && first.gross / first.capital > 0.95, "{:?}", first);
}

#[test]
fn a_names_limit_and_a_participation_limit_act_on_the_orders_they_name() {
    let ds = market();
    // Opening five names against two: the opening orders are rejected.
    let p = held("0.1", "names <= 2 reject");
    let r = run(&p, &ds, ExecConfig::from_program(&p)).unwrap();
    assert!(r.final_positions.is_empty());
    assert!(triggered(&r).iter().all(|(l, _)| l == "names <= 2 reject"));
    // A participation limit scales each order to its share of the bar's volume.
    let p = held("0.1", "participation <= 0.00001 clamp");
    let r = run(&p, &ds, ExecConfig::from_program(&p)).unwrap();
    assert!(triggered(&r).iter().any(|(l, _)| l.starts_with("participation")));
    let unclamped = held("0.1", "names <= 10 warn");
    let r0 = run(&unclamped, &ds, ExecConfig::from_program(&unclamped)).unwrap();
    let bought = |r: &abt_kernel::kernel::RunResult| r.fills.iter().map(|f| f.quantity.abs()).sum::<f64>();
    assert!(bought(&r) < bought(&r0), "{} vs {}", bought(&r), bought(&r0));
}

#[test]
fn a_drawdown_limit_lets_only_reducing_orders_through() {
    // A falling name bought every bar: once the NAV is 5% under its peak,
    // the purchases are rejected.
    let closes: Vec<f64> = (0..30).map(|i| 100.0 * 0.97f64.powi(i)).collect();
    let ds = common::market("AAA", &closes);
    let src = StrategyParts {
        name: "dd".into(),
        params: "    qty : Quantity<Shares> = 1000 shares".into(),
        decides: "    decide(T, trade(A, qty)) :- universe(A, T).".into(),
        execution: "equity_default with { leverage unconstrained   participation none   limits { drawdown <= 0.05 reject } }".into(),
        ..Default::default()
    }
    .render();
    let p = common::program(&src, "dd");
    let r = run(&p, &ds, ExecConfig::from_program(&p)).unwrap();
    let rejected: Vec<&String> = r
        .events
        .iter()
        .filter_map(|e| match &e.kind {
            EventKind::Rejected { reason, .. } => Some(reason),
            _ => None,
        })
        .collect();
    assert!(!rejected.is_empty() && rejected.iter().all(|w| w.contains("drawdown")), "{:?}", rejected.first());
    assert!(!r.fills.is_empty(), "the first purchases fill before the drawdown");
}

#[test]
fn a_limit_written_wrong_is_refused_by_the_checker() {
    let cases = [
        ("leverage <= 1 reject", "is not a limit"),
        ("names <= 2 clamp", "admits halt, reject, warn"),
        ("weight <= 0.1 reject", "per name"),
        ("loss <= 0.02 per week reject", "per day"),
        ("gross <= 1 reject; gross <= 2 warn", "set twice"),
        ("names <= 2.5 reject", "whole number"),
        ("gross <= 1 per day reject", "takes no `per`"),
    ];
    for (limits, want) in cases {
        let src = StrategyParts {
            name: "bad".into(),
            params: "    w : Scalar = 0.1".into(),
            decides: "    decide(T, hold(A, w)) :- universe(A, T).".into(),
            execution: format!("equity_default with {{ limits {{ {} }} }}", limits),
            ..Default::default()
        }
        .render();
        let (p, diags) = common::check(&src, "bad");
        assert!(p.is_none(), "{} should not check", limits);
        let text = common::text(&diags);
        assert!(text.contains(want), "{}: {}", limits, text);
    }
}
