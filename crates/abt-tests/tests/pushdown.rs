//! Pushdown (K-4.5, Proposition D-6.1, G3): an eligible aggregate literal
//! is answered by the data layer's pass, and a run with pushdown decides,
//! fills and holds what a run without it does, to the bit, on every corpus
//! program and both books; the eligibility of every literal is printed with
//! its reason; a window whose rows arrive late is evaluated locally until
//! they are received.

#![allow(clippy::result_large_err)]

use std::fs;
use std::path::{Path, PathBuf};

use abt_kernel::check::{check_program, Program, Workspace};
use abt_kernel::data::{synthetic_daily, synthetic_for_program};
use abt_kernel::kernel::pushdown::{eligibilities, eligibility};
use abt_kernel::kernel::{Dataset, ExecConfig, RunResult, Value};
use abt_kernel::verify::tv::{digest, first_difference};
use abt_tests as common;
use abt_tests::StrategyParts;

fn corpus() -> PathBuf {
    abt_tests::repo_root().join("corpus-v2")
}

fn files(dir: &Path) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = fs::read_dir(dir).unwrap().map(|e| e.unwrap().path()).filter(|p| p.extension().is_some_and(|x| x == "dsl")).collect();
    out.sort();
    out
}

/// A corpus program with the catalog and the library, on its synthetic
/// market (as the golden runs).
fn program_and_market(f: &Path) -> (Program, Dataset) {
    let mut ws = Workspace::new();
    for dir in ["catalog", "lib"] {
        for g in files(&corpus().join(dir)) {
            ws.add_source(&fs::read_to_string(&g).unwrap()).unwrap();
        }
    }
    let src = fs::read_to_string(f).unwrap();
    ws.add_source(&src).unwrap();
    let name = src
        .lines()
        .find_map(|l| l.trim().strip_prefix("strategy ").map(|r| r.split_whitespace().next().unwrap().trim_end_matches('{').to_string()))
        .unwrap();
    let (prog, diags) = check_program(&ws, &name);
    let mut prog = prog.unwrap_or_else(|| panic!("{}: {:?}", name, diags));
    let syms: Vec<&str> = if prog.class == "future" {
        vec![
            "ES", "NQ", "KC", "6J", "ZB", "ZF", "ZN", "RTY", "ZT", "CL", "GC", "HE", "HO", "LE", "NG", "PL", "RB", "SI", "ZC", "ZL", "ZM", "ZS", "ZW", "GF", "SB", "ETH",
        ]
    } else {
        vec!["AAA", "XYZ", "BBB", "SPY", "DDD"]
    };
    let days = if prog.base.is_sub_daily() { 30 } else { 400 };
    let ds = synthetic_for_program(&prog, &syms, (2022, 1, 3), days, 11);
    if name == "r8l" {
        for (p, v) in [("atr_rows", 3), ("med_rows", 6), ("med_min", 3)] {
            for params in prog.params.values_mut() {
                if let Some(x) = params.get_mut(p) {
                    x.value = abt_kernel::Lit::Int(v);
                }
            }
        }
    }
    (prog, ds)
}

fn run_with(p: &Program, ds: &Dataset, pushdown: bool) -> Result<RunResult, String> {
    let cfg = ExecConfig {
        pushdown,
        ..ExecConfig::from_program(p)
    };
    common::run(p, ds, cfg).map_err(|e| e.to_string())
}

#[test]
fn pushdown_and_local_evaluation_agree_on_every_corpus_program_and_book() {
    let mut programs: Vec<PathBuf> = files(&corpus().join("strategies"));
    programs.extend(files(&corpus().join("company")));
    let mut answered = 0usize;
    let mut with_pushdown = Vec::new();
    for f in &programs {
        let (p, ds) = program_and_market(f);
        let pushed = run_with(&p, &ds, true);
        let local = run_with(&p, &ds, false);
        match (&pushed, &local) {
            (Ok(a), Ok(b)) => {
                let (da, db) = (digest(a), digest(b));
                if let Some((line, x, y)) = first_difference(&db, &da) {
                    panic!("{}: pushdown changes line {}:\n  local:    {}\n  pushdown: {}", p.strategy, line, x, y);
                }
                assert_eq!(b.stats.pushed_answers, 0, "{}: no answer is pushed with pushdown off", p.strategy);
                answered += a.stats.pushed_answers;
                if a.stats.pushed_answers > 0 {
                    with_pushdown.push(p.strategy.clone());
                }
            }
            (Err(a), Err(b)) => assert_eq!(a, b, "{}", p.strategy),
            _ => panic!("{}: one run failed: {:?} / {:?}", p.strategy, pushed.err(), local.err()),
        }
    }
    // The comparison is not vacuous: windows were answered by the pass.
    assert!(answered > 0 && with_pushdown.len() >= 3, "pushed in {:?}", with_pushdown);
}

#[test]
fn every_aggregate_has_its_eligibility_and_its_reason() {
    let (p, _) = program_and_market(&corpus().join("company").join("mw14.dsl"));
    let all = eligibilities(&p);
    assert!(!all.is_empty());
    let pushed = all.iter().filter(|(_, e)| e.is_ok()).count();
    assert!(pushed > 0, "{:?}", all.iter().map(|(k, e)| (k, e.as_ref().err())).collect::<Vec<_>>());
    for (_, e) in &all {
        if let Err(why) = e {
            assert!(why.starts_with("(a)") || why.starts_with("(b)") || why.starts_with("(c)") || why.starts_with("(d)"), "{}", why);
        }
    }
    // A bar view is one stored atom over a bucket: pushed.
    let view = p.rules.iter().position(|r| r.head.name.starts_with("close@")).expect("a bar view of close");
    let li = p.rules[view].body.iter().position(|l| matches!(l, abt_kernel::Literal::Agg { .. })).unwrap();
    let shape = eligibility(&p, view, li).unwrap();
    assert_eq!((shape.function.as_str(), shape.relation.as_str()), ("last", "close"));
}

#[test]
fn a_window_over_two_atoms_or_the_universes_rows_stays_local() {
    let src = common::strategy(
        "w",
        "    qty : Quantity<Shares> = 10 shares",
        "    rel spread(A: Equity, @T, S: Price<USD>)\n    spread(A, T, S) :- universe(A, T), S = mean(P) over (T1 in window[10d] min 1, close(A, T1, P), open(A, T1, O)).\n    rel counted(A: Equity, @T, S: Price<USD>)\n    counted(A, T, S) :- universe(A, T), S = mean(P) over (T1 in rows[5], close(A, T1, P)).\n    rel dated(A: Equity, @T, S: Price<USD>)\n    dated(A, T, S) :- universe(A, T), S = mean(P) over (T1 in window[10d] min 1, close(A, T1, P)).",
        "    decide(T, trade(A, qty)) :- spread(A, T, S), counted(A, T, S1), dated(A, T, S2), S > S1, S1 > S2, flat(A, T).",
    );
    let p = common::program(&src, "w");
    let find = |head: &str| {
        let ri = p.rules.iter().position(|r| r.head.name == head).unwrap();
        let li = p.rules[ri].body.iter().position(|l| matches!(l, abt_kernel::Literal::Agg { .. })).unwrap();
        eligibility(&p, ri, li)
    };
    assert!(find("spread").unwrap_err().starts_with("(a)"), "{:?}", find("spread"));
    // A rows window counts the instrument's universe bars (the anchor the
    // checker adds), which no stored relation holds (ERRATA E-47).
    assert!(find("counted").unwrap_err().starts_with("(a)"), "{:?}", find("counted"));
    let dated = find("dated").unwrap();
    assert_eq!((dated.function.as_str(), dated.relation.as_str()), ("mean", "close"));
}

#[test]
fn a_window_over_every_instrument_or_a_column_read_outside_stays_local() {
    let src = StrategyParts {
        name: "g".into(),
        needs: "need bars @1d   need listing   need actions".into(),
        params: "    qty : Quantity<Shares> = 10 shares".into(),
        rules: "    rel total(A: Equity, @T, S: Quantity<Shares>)\n    total(A, T, S) :- universe(A, T), S = sum(V) over (T1 in window[5d] min 1, volume(B, T1, V)).\n    rel paid(A: Equity, @T, S: Price<USD>)\n    paid(A, T, S) :- universe(A, T), S = sum(X) over (T1 in window[30d] min 1, dividend(A, T1, E, P, X)).\n    rel by_ex(A: Equity, @T, E: Timestamp, S: Price<USD>)\n    by_ex(A, T, E, S) :- universe(A, T), S = sum(X) over (T1 in window[30d] min 1, dividend(A, T1, E, P, X)).".into(),
        decides: "    decide(T, trade(A, qty)) :- total(A, T, S), paid(A, T, S1), by_ex(A, T, E, S2), S1 > S2, flat(A, T).".into(),
        ..Default::default()
    }
    .render();
    let p = common::program(&src, "g");
    let find = |head: &str| {
        let ri = p.rules.iter().position(|r| r.head.name == head).unwrap();
        let li = p.rules[ri].body.iter().position(|l| matches!(l, abt_kernel::Literal::Agg { .. })).unwrap();
        eligibility(&p, ri, li)
    };
    // The sum over every instrument's volume is one value, not one per instrument.
    assert!(find("total").unwrap_err().contains("every instrument"), "{:?}", find("total"));
    // The columns the aggregate does not read may be anything when only it reads them.
    let paid = find("paid").unwrap();
    assert_eq!((paid.function.as_str(), paid.relation.as_str()), ("sum", "dividend"));
    // A column the head reads groups the window's rows, which the pass does not.
    assert!(find("by_ex").unwrap_err().contains("`E` is read outside"), "{:?}", find("by_ex"));
}

#[test]
fn a_late_row_is_read_locally_until_it_is_received() {
    // Every close of AAA after the 60th day is received five days late: the
    // pass's answers wait on them, so the windows that read them are
    // evaluated locally at the barriers before, and the run is the one
    // without pushdown.
    let src = common::strategy(
        "late",
        "    qty : Quantity<Shares> = 10 shares",
        "    rel avg(A: Equity, @T, M: Price<USD>)\n    avg(A, T, M) :- universe(A, T), M = mean(P) over (T1 in window[14d] min 1, close(A, T1, P)).",
        "    decide(T, trade(A, qty)) :- avg(A, T, M), close(A, T, P), P > M, flat(A, T).\n    decide(T, trade(A, Q)) :- held(A, T, Q0), avg(A, T, M), close(A, T, P), P < M, Q = -Q0.",
    );
    let p = common::program(&src, "late");
    let mut ds = synthetic_daily(&["AAA", "BBB"], (2022, 1, 3), 140, 5);
    let aaa = ds.securities.iter().find(|x| x.ticker == "AAA").map(|x| x.id).unwrap();
    let day = abt_kernel::kernel::time::DAY;
    let closes = ds.facts["close"].clone();
    let mut times = Vec::new();
    for (i, tu) in closes.iter().enumerate() {
        let t = tu[1].as_time().unwrap();
        let late = tu[0] == Value::Equity(aaa) && i > 120;
        times.push(if late { t + 5 * day } else { t + day - 1 });
    }
    ds.available_at.insert("close".into(), times);
    let a = run_with(&p, &ds, true).unwrap();
    let b = run_with(&p, &ds, false).unwrap();
    if let Some((line, x, y)) = first_difference(&digest(&b), &digest(&a)) {
        panic!("line {}:\n  local:    {}\n  pushdown: {}", line, x, y);
    }
    assert!(a.stats.pushed_answers > 0);
}
