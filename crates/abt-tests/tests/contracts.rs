//! Contracts derived from structure (K-3, G7): every clause of every corpus
//! program and both books is proven, checked or reported after a run; every
//! invariant, made to fail at a bar, halts the run there naming its clause;
//! the preconditions refuse before the first barrier; the pre-trade limits
//! (K-3.5) reject, scale, warn or halt as their policy says, and the checker
//! refuses a limit written wrong.

#![allow(clippy::result_large_err)]

use abt_kernel::contract::check::{preconditions, preconditions_rows};
use abt_kernel::contract::{contract, Contract, Kind, Status};
use abt_kernel::kernel::bind::{bind_at, point_of};
use abt_kernel::kernel::{DataClient, EventKind, ExecConfig, RunError, RunWarning, Value};
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

/// A program holding a tenth of each name of its universe, over daily bars
/// with the actions, the listing and the SPX list: what the data checks
/// of the preconditions read.
fn checked_program(name: &str, universe: &str, rules: &str) -> abt_kernel::check::Program {
    let src = StrategyParts {
        name: name.into(),
        needs: "need bars @1d   need actions, listing   need lists \"SPX\"".into(),
        universe: universe.into(),
        params: "    w : Scalar = 0.1".into(),
        rules: rules.into(),
        decides: "    decide(T, hold(A, w)) :- universe(A, T).".into(),
        ..Default::default()
    }
    .render();
    common::program(&src, name)
}

/// The preconditions of a program over a dataset, read as columns and read
/// a row at a time: each one's contract, and its warnings or its refusal.
type Preconditions = (Contract, Result<Vec<RunWarning>, String>);

fn both_ways(prog: &abt_kernel::check::Program, ds: &abt_kernel::kernel::Dataset) -> (Preconditions, Preconditions, Vec<abt_kernel::kernel::bind::DataCall>) {
    common::init();
    let svc = abt::serve_dataset(ds, prog).unwrap();
    let client = DataClient::connect(abt_data::service::inproc(&svc)).unwrap();
    let bound = bind_at(prog, &client, &point_of(prog, &[]), None).unwrap();
    let cfg = ExecConfig::from_program(prog);
    let mark = client.calls_made();
    let mut by_columns = contract(prog);
    let columns = preconditions(&mut by_columns, prog, &bound, &client, &cfg).map_err(|e| format!("{:?}", e));
    let calls = client.calls_since(mark);
    let mut by_rows = contract(prog);
    let rows = preconditions_rows(&mut by_rows, prog, &bound, &client, &cfg).map_err(|e| format!("{:?}", e));
    ((by_columns, columns), (by_rows, rows), calls)
}

#[test]
fn the_preconditions_give_the_same_statuses_and_warnings_from_columns_as_from_rows() {
    let gated = checked_program("gated", "universe(A, T) :- listed(A, T), traded@base(A, T), member(A, T, \"SPX\").", "");
    // The same universe through a derived relation: its binding is not
    // scoped to the list's instruments.
    let open = checked_program(
        "open",
        "universe(A, T) :- listed(A, T), traded@base(A, T), in_spx(A, T).",
        "    rel in_spx(A: Equity, @T)\n    in_spx(A, T) :- member(A, T, \"SPX\").",
    );
    // A market whose actions, delisting and membership are in order: every
    // data check holds, read either way.
    let sound = abt_kernel::data::synthetic_daily(&["AAA", "BBB", "CCC", "DDD", "EEE"], (2022, 1, 3), 120, 3);
    for prog in [&gated, &open] {
        let (columns, rows, calls) = both_ways(prog, &sound);
        assert_eq!(columns, rows, "{}", prog.strategy);
        assert_eq!(columns.1, Ok(vec![]));
        for id in ["pre.actions", "pre.delistings", "pre.membership"] {
            assert!(
                matches!(columns.0.get(id).unwrap().status, Status::Checked { times: 1 }),
                "{}: {}",
                id,
                columns.0.get(id).unwrap().status
            );
        }
        // Every price column was asked for its rows at or below zero, the
        // instrument and the time of each: none came back.
        let prices: Vec<&abt_kernel::kernel::bind::DataCall> = calls.iter().filter(|c| c.detail.contains("where=") && c.detail.contains(".le(1)")).collect();
        assert!(matches!(columns.0.get("pre.prices").unwrap().status, Status::Checked { times } if times as usize == prices.len() && times >= 4));
        assert!(
            prices.iter().all(|c| c.detail.starts_with("scan(A,T;where=") && c.rows == 0),
            "{:?}",
            prices.iter().map(|c| c.line()).collect::<Vec<_>>()
        );
        // And no check read a column it does not look at.
        let read = |relation: &str| {
            calls
                .iter()
                .filter(|c| c.relation == relation && !c.detail.contains(".le(1)"))
                .map(|c| c.detail.clone())
                .collect::<Vec<_>>()
        };
        assert_eq!(read("equity.close").len(), 1);
        assert!(read("equity.close")[0].starts_with("scan(A,T,P"), "{:?}", read("equity.close"));
        assert_eq!(read("equity.listed").len(), 1);
        assert!(
            read("equity.listed")[0].starts_with("scan(A,T") && !read("equity.listed")[0].starts_with("scan(A,T,"),
            "{:?}",
            read("equity.listed")
        );
        assert!(read("equity.member")[0].starts_with("scan(A,T;where=List.in(1)"), "{:?}", read("equity.member"));
    }
    // The same market with each check's problems in it. The bars and the
    // instruments by their keys (E1 to E5).
    let mut ds = sound.clone();
    let sym = |ds: &abt_kernel::kernel::Dataset, key: &str| Value::Equity(ds.symbols.get(key).unwrap());
    let bars: Vec<i64> = {
        let first = sym(&ds, "E1");
        ds.facts["close"]
            .iter()
            .filter(|tu| tu[0] == first)
            .filter_map(|tu| if let Value::Time(t) = tu[1] { Some(t) } else { None })
            .collect()
    };
    assert_eq!(bars.len(), 120);
    // A split whose factor the closes do not show (E2 halves on a 3:1).
    for tu in ds.facts.get_mut("split").unwrap().iter_mut() {
        tu[2] = Value::Num(3.0);
    }
    // A close halved on a dividend's ex-date (E1, ten bars after the first
    // announcement): the dividend does not explain it, and nothing explains
    // the bar after.
    let (e1, ex) = (sym(&ds, "E1"), Value::Time(bars[20]));
    assert!(ds.facts["dividend"].iter().any(|tu| tu[0] == e1 && tu[2] == ex));
    for tu in ds.facts.get_mut("close").unwrap().iter_mut().filter(|tu| tu[0] == e1 && tu[1] == ex) {
        tu[2] = Value::Num(tu[2].as_f64().unwrap() / 2.0);
    }
    // A split on a bar with no close (E5 after its delisting).
    let e5 = sym(&ds, "E5");
    ds.add("split", abt_kernel::kernel::Tuple::from_iter([e5, Value::Time(bars[100]), Value::Num(4.0)]));
    // An instrument that leaves the listing with no delisted record (E3
    // from the 70th bar), still a member of the list on the fifty bars
    // after.
    let e3 = sym(&ds, "E3");
    ds.facts.get_mut("listed").unwrap().retain(|tu| !(tu[0] == e3 && matches!(tu[1], Value::Time(t) if t >= bars[70])));
    ds.available_at.remove("listed");
    for prog in [&gated, &open] {
        let (columns, rows, _) = both_ways(prog, &ds);
        assert_eq!(columns, rows, "{}", prog.strategy);
        let (c, warnings) = (columns.0, columns.1.unwrap());
        assert_eq!(warnings.iter().map(|w| w.bias.as_str()).collect::<Vec<_>>(), vec!["corporate-action", "delisting", "survivorship"]);
        let detail = |id: &str| match &c.get(id).unwrap().status {
            Status::Warned { count, detail } => (*count, detail.clone()),
            other => panic!("{}: {}", id, other),
        };
        let (count, actions) = detail("pre.actions");
        assert_eq!(count, 4, "{}", actions);
        for part in [
            "E1 at 2022-01-31: the close moved from",
            "does not explain it",
            "E1 at 2022-02-01: the close moved from",
            "with no action to explain it",
            "E2 at 2022-03-10: a 3:1 split but the close moved from",
            "E5 at 2022-05-23: a 4:1 split on a bar with no close",
        ] {
            assert!(actions.contains(part), "{}: {}", part, actions);
        }
        let (count, delistings) = detail("pre.delistings");
        assert_eq!((count, delistings.as_str()), (1, "E3 leaves the listing at 2022-04-08 with no delisted record"));
        let (count, membership) = detail("pre.membership");
        assert_eq!(count, 50, "{}", membership);
        assert!(
            membership.starts_with("E3 is a member at 2022-04-11 but not listed; E3 is a member at 2022-04-12 but not listed;") && membership.ends_with("(+45 more)"),
            "{}",
            membership
        );
        assert!(warnings[2].message.contains("(50 found): E3 is a member at 2022-04-11 but not listed"), "{}", warnings[2].message);
    }
    // A price that is not positive refuses, naming the first such row of
    // the first relation that has one: a zero close, and where the closes
    // are sound a negative open.
    let mut zero = sound.clone();
    let e4 = sym(&zero, "E4");
    for tu in zero
        .facts
        .get_mut("close")
        .unwrap()
        .iter_mut()
        .filter(|tu| tu[0] == e4 && (tu[1] == Value::Time(bars[30]) || tu[1] == Value::Time(bars[31])))
    {
        tu[2] = Value::Num(0.0);
    }
    let mut negative = sound.clone();
    let e2 = sym(&negative, "E2");
    for tu in negative.facts.get_mut("open").unwrap().iter_mut().filter(|tu| tu[0] == e2 && tu[1] == Value::Time(bars[9])) {
        tu[2] = Value::Num(-1.5);
    }
    for (ds, relation, who, day) in [(&zero, "close", "E4", "2022-02-14"), (&negative, "open", "E2", "2022-01-14")] {
        for prog in [&gated, &open] {
            let (columns, rows, _) = both_ways(prog, ds);
            assert_eq!(columns, rows, "{}", prog.strategy);
            let refusal = columns.1.unwrap_err();
            assert!(
                refusal.contains("pre.prices") && refusal.contains(&format!("`{}` of {} at {} is not a positive price", relation, who, day)),
                "{}",
                refusal
            );
        }
    }
    // Every program of the corpus on its golden market, read both ways.
    let mut programs = 0;
    for f in golden_programs() {
        let (p, ds) = golden_inputs(&f).unwrap();
        let (columns, rows, _) = both_ways(&p, &ds);
        assert_eq!(columns, rows, "{}", p.strategy);
        programs += 1;
    }
    assert!(programs >= 20, "{} programs", programs);
}
