//! Pushdown (K-4.5, Proposition D-6.1, G3): an eligible aggregate literal
//! is answered by the data layer's pass, and a run with pushdown decides,
//! fills and holds what a run without it does, to the bit, on every corpus
//! program and both books, whether its answers arrive as tables or in the
//! stream (ERRATA E-157) without the relations only they read (E-158), and
//! under the set evaluator or the block evaluator's operators (E-175); the
//! eligibility of every literal is printed with its reason; a window whose
//! rows arrive late is evaluated locally until they are received; a
//! delivered literal solves no window; a count reads the count a sibling's
//! answer carries (E-159).

#![allow(clippy::result_large_err)]

use std::path::Path;

use abt_kernel::check::Program;
use abt_kernel::data::synthetic_daily;
use abt_kernel::kernel::pushdown::{deliverable, describe, eligibilities, eligibility, product_of};
use abt_kernel::kernel::{Dataset, ExecConfig, Replay, RunResult, Value};
use abt_kernel::verify::tv::{self, digest, first_difference};
use abt_tests as common;
use abt_tests::StrategyParts;

/// A corpus program on its golden market.
fn program_and_market(f: &Path) -> (Program, Dataset) {
    abt_tests::golden_inputs(f).unwrap_or_else(|e| panic!("{}", e))
}

/// The configuration of a run: the pass's answers delivered in the stream
/// where they can be, read as tables, or no pushdown.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Arm {
    Delivered,
    Tables,
    Local,
}

/// The evaluators every arm runs under: the set evaluator, and the block
/// evaluator (`--evaluator block`, ERRATA E-150), whose operators read the
/// answers the pass gives, delivered or as tables (E-175).
const BLOCK: [bool; 2] = [false, true];

fn config(p: &Program, arm: Arm, block: bool) -> ExecConfig {
    ExecConfig {
        pushdown: arm != Arm::Local,
        deliver: arm == Arm::Delivered,
        block_eval: block,
        ..ExecConfig::from_program(p)
    }
}

fn run_with(p: &Program, ds: &Dataset, pushdown: bool, block: bool) -> Result<RunResult, String> {
    run_on(p, ds, if pushdown { Arm::Delivered } else { Arm::Local }, block)
}

/// A run of an arm under the set evaluator, the reference.
fn run_arm(p: &Program, ds: &Dataset, arm: Arm) -> Result<RunResult, String> {
    run_on(p, ds, arm, false)
}

fn run_on(p: &Program, ds: &Dataset, arm: Arm, block: bool) -> Result<RunResult, String> {
    common::run(p, ds, config(p, arm, block)).map_err(|e| e.to_string())
}

/// A run's result and, per relation the kernel keeps, the tuples the
/// stream delivered (a relation dropped at the fold's door, E-158, is not
/// one), and the calls derived.
#[allow(clippy::type_complexity)]
fn run_counting(p: &Program, ds: &Dataset, arm: Arm, block: bool) -> (RunResult, Vec<(String, u64)>, Vec<(String, usize)>) {
    let (r, (received, calls)) = common::run_keeping(p, None, ds, config(p, arm, block), &Replay::default(), |_, k| {
        let kept: Vec<String> = k.read_relations().into_iter().map(|r| k.relation_name(r).to_string()).collect();
        let received = k.store_counts().into_iter().filter(|(n, _, _)| kept.contains(n)).map(|(n, rows, _)| (n, rows)).collect();
        let calls = k.coverage().into_iter().map(|c| (c.name, c.calls)).collect();
        Ok((received, calls))
    })
    .unwrap_or_else(|e| panic!("{}: {}", p.strategy, e));
    (r, received, calls)
}

/// The ledgers of a run: every derived relation at every bar (`tv::ledgers`).
fn ledgers(p: &Program, ds: &Dataset, arm: Arm, block: bool) -> tv::Ledgers {
    let (_, l) = common::run_keeping(p, None, ds, config(p, arm, block), &Replay::default(), |r, k| Ok(tv::ledgers(k, p, &r.bars))).unwrap_or_else(|e| panic!("{}: {}", p.strategy, e));
    l
}

/// The relations a run under the block evaluator derived through their
/// plan at some bar (`Kernel::plan_counts`).
fn planned(p: &Program, ds: &Dataset, arm: Arm) -> Vec<String> {
    let (_, names) = common::run_keeping(p, None, ds, config(p, arm, true), &Replay::default(), |_, k| {
        Ok(k.plan_counts()
            .into_iter()
            .filter(|(_, planned, bars, _, _)| *planned && *bars > 0)
            .map(|(n, ..)| n)
            .collect::<Vec<String>>())
    })
    .unwrap_or_else(|e| panic!("{}: {}", p.strategy, e));
    names
}

#[test]
fn pushdown_and_local_evaluation_agree_on_every_corpus_program_and_book() {
    let programs = abt_tests::golden_programs();
    let mut answered = 0usize;
    let mut with_pushdown = Vec::new();
    let mut delivering = Vec::new();
    for f in &programs {
        let (p, ds) = program_and_market(f);
        let delivered = run_arm(&p, &ds, Arm::Delivered);
        let tables = run_arm(&p, &ds, Arm::Tables);
        let local = run_arm(&p, &ds, Arm::Local);
        match (&delivered, &tables, &local) {
            (Ok(a), Ok(t), Ok(b)) => {
                let db = digest(b);
                for (arm, r) in [("tables", t), ("delivered", a)] {
                    if let Some((line, x, y)) = first_difference(&db, &digest(r)) {
                        panic!("{}: pushdown ({}) changes line {}:\n  local:    {}\n  pushdown: {}", p.strategy, arm, line, x, y);
                    }
                }
                assert_eq!(b.stats.pushed_answers, 0, "{}: no answer is pushed with pushdown off", p.strategy);
                answered += t.stats.pushed_answers;
                if t.stats.pushed_answers > 0 {
                    with_pushdown.push(p.strategy.clone());
                }
                // A delivered answer is a relation of the stream, whose rows
                // the tables' run does not receive (a relation only delivered
                // answers read is dropped at the fold's door, E-158).
                if a.stats.received_rows != t.stats.received_rows {
                    delivering.push(p.strategy.clone());
                }
                assert_eq!(t.stats.received_rows, b.stats.received_rows, "{}: tables leave the stream as it is", p.strategy);
                // The block evaluator's operators read each arm's answers as
                // the row path does: the same run, the same answers read
                // and the same stream (E-175).
                for (arm, r) in [(Arm::Delivered, a), (Arm::Tables, t), (Arm::Local, b)] {
                    let block = run_on(&p, &ds, arm, true).unwrap_or_else(|e| panic!("{} ({:?}, block evaluator): {}", p.strategy, arm, e));
                    if let Some((line, x, y)) = first_difference(&db, &digest(&block)) {
                        panic!("{}: {:?} under the block evaluator changes line {}:\n  local:    {}\n  block:    {}", p.strategy, arm, line, x, y);
                    }
                    assert_eq!(
                        (block.stats.pushed_answers, block.stats.received_rows),
                        (r.stats.pushed_answers, r.stats.received_rows),
                        "{} ({:?}): the answers read or the rows received differ under the block evaluator",
                        p.strategy,
                        arm
                    );
                }
            }
            (Err(a), Err(t), Err(b)) => {
                assert_eq!(a, b, "{}", p.strategy);
                assert_eq!(t, b, "{}", p.strategy);
                for arm in [Arm::Delivered, Arm::Tables, Arm::Local] {
                    assert_eq!(run_on(&p, &ds, arm, true).as_ref().err(), Some(b), "{} ({:?}, block evaluator)", p.strategy, arm);
                }
            }
            _ => panic!("{}: one run failed: {:?} / {:?} / {:?}", p.strategy, delivered.err(), tables.err(), local.err()),
        }
    }
    // The comparison is not vacuous: windows were answered by the pass, and
    // answers were delivered, the weekly book's among them.
    assert!(answered > 0 && with_pushdown.len() >= 3, "pushed in {:?}", with_pushdown);
    assert!(delivering.iter().any(|s| s == "mw14"), "delivered in {:?}", delivering);
}

#[test]
fn every_aggregate_has_its_eligibility_and_its_reason() {
    let (p, _) = program_and_market(&abt_tests::corpus_dir().join("company").join("mw14.dsl"));
    let all = eligibilities(&p);
    assert!(!all.is_empty());
    let pushed = all.iter().filter(|(_, e)| e.is_ok()).count();
    assert!(pushed > 0, "{:?}", all.iter().map(|(k, e)| (k, e.as_ref().err())).collect::<Vec<_>>());
    for (_, e) in &all {
        if let Err(why) = e {
            assert!(why.starts_with("(a)") || why.starts_with("(b)") || why.starts_with("(c)") || why.starts_with("(d)"), "{}", why);
        }
    }
    let shape_of = |head: &str| {
        let ri = p.rules.iter().position(|r| r.head.name == head).unwrap_or_else(|| panic!("a rule for {}", head));
        let li = p.rules[ri].body.iter().position(|l| matches!(l, abt_kernel::Literal::Agg { .. })).unwrap();
        eligibility(&p, ri, li).unwrap_or_else(|e| panic!("{}: {}", head, e))
    };
    // A bar view is one stored atom over a bucket: pushed.
    let shape = shape_of("close@weekly");
    assert_eq!((shape.function.as_str(), shape.relation.as_str(), shape.factor.is_none()), ("last", "close", true));
    // A count counts the window's rows, as the pass does (E-156): pushed,
    // the rows of the atom's first number column counted.
    let shape = shape_of("traded@weekly");
    assert_eq!((shape.function.as_str(), shape.relation.as_str(), shape.columns.as_slice()), ("count", "close", &[2][..]));
    // The mean of a product of two stored relations is the mean over their
    // joined rows (E-156): pushed, the first factor's column times the
    // second's.
    let shape = shape_of("dvol");
    let f = shape.factor.as_ref().expect("a product");
    assert_eq!((shape.function.as_str(), shape.relation.as_str(), f.relation.as_str()), ("mean", "close", "volume"));
    assert!(f.product.starts_with("dollar_volume"), "{}", f.product);
    assert_eq!(describe(&Ok(shape.clone())), "pushdown: mean over bucket(weeks(1)) of `close` times `volume` per instrument");
    // All three are on the decision calendar with functions that cannot
    // fail: the stream can deliver them (E-157).
    for head in ["close@weekly", "traded@weekly", "dvol"] {
        assert_eq!(deliverable(&shape_of(head), &p), Ok(()), "{}", head);
    }
    // A product is a derived relation of one rule joining two stored ones.
    assert!(product_of(&p, &f.product).is_some());
    assert!(product_of(&p, "close@weekly").is_none() && product_of(&p, "close").is_none());
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
    let b = run_with(&p, &ds, false, false).unwrap();
    // Under the block evaluator the answers are joined to the universe's
    // names at a bar where all were received, and the bar is the rules'
    // where one was not (E-175).
    for block in BLOCK {
        let a = run_with(&p, &ds, true, block).unwrap();
        if let Some((line, x, y)) = first_difference(&digest(&b), &digest(&a)) {
            panic!("block {}: line {}:\n  local:    {}\n  pushdown: {}", block, line, x, y);
        }
        assert!(a.stats.pushed_answers > 0);
        assert_eq!(run_with(&p, &ds, false, block).map(|r| digest(&r)), Ok(digest(&b)), "block {}", block);
    }
}

#[test]
fn a_delivered_aggregate_is_never_evaluated_locally_and_equals_the_local_run() {
    // The weekly book on its synthetic market: the weekly last close, the
    // weekly count of bars and the weekly mean of the daily dollar volume
    // are answered in the stream, and the run is the local one to the bit.
    let (p, ds) = program_and_market(&abt_tests::corpus_dir().join("company").join("mw14.dsl"));
    let (a, received, calls) = run_counting(&p, &ds, Arm::Delivered, false);
    let (b, _, local_calls) = run_counting(&p, &ds, Arm::Local, false);
    let (t, _, _) = run_counting(&p, &ds, Arm::Tables, false);
    for (arm, r) in [("tables", &t), ("delivered", &a)] {
        if let Some((line, x, y)) = first_difference(&digest(&b), &digest(r)) {
            panic!("{} changes line {}:\n  local: {}\n  {}: {}", arm, line, x, y, arm);
        }
    }
    // The block evaluator reads the three answers through its operators
    // (E-175): the weekly last close and the weekly count of bars at every
    // name of the bar, the weekly mean dollar volume at the universe's.
    for arm in [Arm::Delivered, Arm::Tables, Arm::Local] {
        let (r, block_received, block_calls) = run_counting(&p, &ds, arm, true);
        if let Some((line, x, y)) = first_difference(&digest(&b), &digest(&r)) {
            panic!("{:?} under the block evaluator changes line {}:\n  local: {}\n  block: {}", arm, line, x, y);
        }
        if arm == Arm::Delivered {
            assert_eq!(
                (&block_received, &block_calls),
                (&received, &calls),
                "the block evaluator receives and derives what the set evaluator does"
            );
        }
        // Not vacuous: each reads its answers through a plan, delivered or
        // as a table (a count's as the count).
        if arm != Arm::Local {
            let names = planned(&p, &ds, arm);
            for head in ["close@weekly", "traded@weekly", "dvol"] {
                assert!(names.iter().any(|n| n == head), "{:?}: `{}` derived no bar through its plan: {:?}", arm, head, names);
            }
        }
    }
    // Each literal's answers came in the stream, into its store, and the
    // weekly count of bars read the weekly last close's, whose rows carry
    // the count (E-159).
    let rows = |head: &str| received.iter().find(|(n, _)| n.starts_with("pushed.") && n.contains(&format!("::{}#", head))).map(|(_, r)| *r);
    for head in ["close@weekly", "dvol"] {
        assert!(rows(head).is_some_and(|r| r > 0), "{}: {:?}", head, received);
    }
    assert_eq!(rows("traded@weekly"), None, "{:?}", received);
    // The daily inputs stay in the stream, each with a reader beside the
    // answers (E-158): the membership test reads the name's last daily
    // close of the week as of the label (E-160; since E-144 the total
    // return reads the weekly close), and every fill records its
    // participation of the bar's volume.
    let streamed = |name: &str| received.iter().any(|(n, r)| n == name && *r > 0);
    assert!(streamed("close") && streamed("volume"), "{:?}", received);
    // No window of them was solved: the daily dollar volume, which only the
    // weekly mean reads, was never derived.
    let calls_of = |c: &[(String, usize)], name: &str| c.iter().filter(|(n, _)| n.starts_with(name)).map(|(_, x)| *x).sum::<usize>();
    assert!(calls_of(&local_calls, "dollar_volume") > 0, "{:?}", local_calls);
    assert_eq!(calls_of(&calls, "dollar_volume"), 0, "{:?}", calls);
    assert!(a.stats.window_calls < b.stats.window_calls, "{} / {}", a.stats.window_calls, b.stats.window_calls);
}

#[test]
fn a_count_over_an_empty_bucket_is_absent_for_a_bound_instrument_and_an_unbound_one() {
    // A weekly program counting each name's daily bars in the week: AAA has
    // no bar in the third week. A bucket's minimum is 1, so the window has
    // no count there, and the pass no row: neither the instrument a rule
    // binds nor the instruments the aggregation binds read one.
    let src = StrategyParts {
        name: "empty_week".into(),
        needs: "need bars @1d complete   need listing".into(),
        calendars: Some("weekly = weeks(1)".into()),
        clock: "decide on weekly".into(),
        universe: "universe(A, T) :- listed@weekly(A, T).".into(),
        params: "    qty : Quantity<Shares> = 10 shares\n    most : Count = 3".into(),
        rules: "    rel bars_bound(A: Equity, @T, N: Count)\n    bars_bound(A, T, N) :- universe(A, T), N = count(T1) over (T1 in bucket(weekly, T), close(A, T1, _)).\n    rel bars_free(A: Equity, @T, N: Count)\n    bars_free(A, T, N) :- N = count(T1) over (T1 in bucket(weekly, T), close(A, T1, _)).\n    rel counted(A: Equity, @T)\n    counted(A, T) :- bars_bound(A, T, _).".into(),
        decides: "    decide(T, trade(A, qty)) :- universe(A, T), not counted(A, T), flat(A, T).\n    decide(T, trade(A, Q)) :- held(A, T, Q0), bars_free(A, T, N), N > most, Q = -Q0.".into(),
        report: "calendar weekly   periods 52 per year".into(),
        ..Default::default()
    }
    .render();
    let p = common::program(&src, "empty_week");
    let mut ds = common::markets((2024, 1, 8), &[("AAA", &[10.0; 40]), ("BBB", &[20.0; 40])]);
    let aaa = ds.securities.iter().find(|x| x.ticker == "AAA").map(|x| x.id).unwrap();
    let (from, to) = (common::day("2024-01-22"), common::day("2024-01-27"));
    let in_week = |tu: &[Value]| tu[0] == Value::Equity(aaa) && tu[1].as_time().is_some_and(|t| t >= from && t < to);
    for rel in ["open", "high", "low", "close", "volume"] {
        if let Some(rows) = ds.facts.get_mut(rel) {
            rows.retain(|tu| !in_week(tu));
        }
    }
    let (a, received, _) = run_counting(&p, &ds, Arm::Delivered, false);
    let b = run_arm(&p, &ds, Arm::Local).unwrap();
    if let Some((line, x, y)) = first_difference(&digest(&b), &digest(&a)) {
        panic!("line {}:\n  local:     {}\n  delivered: {}", line, x, y);
    }
    // The block evaluator reads the bound count as a join on the universe's
    // names and the unbound one as the bar's rows, through their plans
    // (E-175): AAA's empty week has no row in either, and every count is a
    // Count, in the ledgers of every relation at every bar too.
    let local_ledgers = ledgers(&p, &ds, Arm::Local, false);
    for arm in [Arm::Delivered, Arm::Tables] {
        let (r, block_received, _) = run_counting(&p, &ds, arm, true);
        if let Some((line, x, y)) = first_difference(&digest(&b), &digest(&r)) {
            panic!("{:?}, block evaluator: line {}:\n  local: {}\n  block: {}", arm, line, x, y);
        }
        if arm == Arm::Delivered {
            assert_eq!(block_received, received);
        }
        for block in BLOCK {
            assert!(ledgers(&p, &ds, arm, block) == local_ledgers, "{:?} (block {}): a ledger differs from the local run's", arm, block);
        }
        let names = planned(&p, &ds, arm);
        assert!(["bars_bound", "bars_free"].iter().all(|h| names.iter().any(|n| n == h)), "{:?}: {:?}", arm, names);
    }
    // The two counts ask what the weekly last close the book is priced at
    // asks but for the function: they read the count its rows carry
    // (E-159), eight weeks of two names less AAA's empty one. Nothing else
    // reads the daily close, which the fold drops (E-158).
    let rows = |head: &str| received.iter().find(|(n, _)| n.starts_with("pushed.") && n.contains(head)).map(|(_, r)| *r);
    assert_eq!((rows("::close@weekly#"), rows("::bars_bound#"), rows("::bars_free#")), (Some(15), None, None), "{:?}", received);
    assert!(received.iter().all(|(n, _)| n != "close"), "{:?}", received);
    // AAA's empty week has no count: it was bought at that week's end, and
    // only then (BBB always has bars, and AAA's other weeks five).
    let bought: Vec<(i64, String)> = a.decisions.iter().map(|d| (d.t, a.describe_decision(&d.decision))).filter(|(_, d)| d.contains("trade")).collect();
    assert!(bought.iter().any(|(t, d)| *t >= from && *t < to && d.contains("AAA")), "{:?}", bought);
    assert!(bought.iter().all(|(_, d)| !d.contains("BBB")), "{:?}", bought);
}

#[test]
fn a_bucket_whose_rows_arrive_late_is_read_as_a_table_and_locally() {
    // A weekly program's weekly mean close, every close of AAA after the
    // 60th day received five days late: the pass counts those rows inexact,
    // so the answer is not delivered (a delivered row would arrive after
    // its barrier); it is read as a table where its rows were received, and
    // locally until then, and the run is the one without pushdown.
    let src = StrategyParts {
        name: "late_week".into(),
        needs: "need bars @1d complete   need listing".into(),
        calendars: Some("weekly = weeks(1)".into()),
        clock: "decide on weekly".into(),
        universe: "universe(A, T) :- listed@weekly(A, T).".into(),
        params: "    qty : Quantity<Shares> = 10 shares".into(),
        rules: "    rel avg(A: Equity, @T, M: Price<USD>)\n    avg(A, T, M) :- universe(A, T), M = mean(P) over (T1 in bucket(weekly, T), close(A, T1, P)).".into(),
        decides: "    decide(T, trade(A, qty)) :- avg(A, T, M), close@weekly(A, T, P), P > M, flat(A, T).\n    decide(T, trade(A, Q)) :- held(A, T, Q0), avg(A, T, M), close@weekly(A, T, P), P < M, Q = -Q0.".into(),
        report: "calendar weekly   periods 52 per year".into(),
        ..Default::default()
    }
    .render();
    let p = common::program(&src, "late_week");
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
    let b = run_arm(&p, &ds, Arm::Local).unwrap();
    for block in BLOCK {
        let (a, received, _) = run_counting(&p, &ds, Arm::Delivered, block);
        if let Some((line, x, y)) = first_difference(&digest(&b), &digest(&a)) {
            panic!("block {}: line {}:\n  local:    {}\n  pushdown: {}", block, line, x, y);
        }
        assert!(a.stats.pushed_answers > 0);
        assert!(received.iter().all(|(n, _)| !n.starts_with("pushed.")), "{:?}", received);
    }
}

#[test]
fn a_count_that_shares_an_answer_equals_the_count_asked_alone() {
    // A weekly book counting each name's daily highs and daily volumes in
    // the week, with a weekly highest high beside the counts or without it:
    // AAA has no volume on two days of the second week and no bar on two
    // days of the fourth. With the highest high, the count of highs asks
    // what it asks but for the function, so it reads the count that
    // answer's rows carry (E-159); the count of volumes asks of another
    // relation and has an answer of its own, as the count of highs has in
    // the book without the highest high. Each book's run is its run without
    // pushdown, and the two decide alike. Only the answers read the daily
    // high, which the fold drops in both (E-158).
    let book = |name: &str, extra: &str| {
        let src = StrategyParts {
            name: name.into(),
            needs: "need bars @1d complete   need listing".into(),
            calendars: Some("weekly = weeks(1)".into()),
            clock: "decide on weekly".into(),
            universe: "universe(A, T) :- listed@weekly(A, T).".into(),
            params: "    qty : Quantity<Shares> = 10 shares\n    full : Count = 5".into(),
            rules: format!(
                "    rel days(A: Equity, @T, N: Count)\n    days(A, T, N) :- universe(A, T), N = count(T1) over (T1 in bucket(weekly, T), high(A, T1, _)).\n    rel volume_days(A: Equity, @T, N: Count)\n    volume_days(A, T, N) :- universe(A, T), N = count(T1) over (T1 in bucket(weekly, T), volume(A, T1, _)).{}",
                extra
            ),
            decides: "    decide(T, trade(A, qty)) :- days(A, T, N), volume_days(A, T, M), N > M, flat(A, T).\n    decide(T, trade(A, Q)) :- held(A, T, Q0), days(A, T, N), N < full, Q = -Q0.".into(),
            report: "calendar weekly   periods 52 per year".into(),
            ..Default::default()
        }
        .render();
        common::program(&src, name)
    };
    let shared = book(
        "shared",
        "\n    rel week_high(A: Equity, @T, H: Price<USD>)\n    week_high(A, T, H) :- universe(A, T), H = max(X) over (T1 in bucket(weekly, T), high(A, T1, X)).",
    );
    let alone = book("alone", "");
    let mut ds = common::markets((2024, 1, 8), &[("AAA", &[10.0; 40]), ("BBB", &[20.0; 40])]);
    let aaa = ds.securities.iter().find(|x| x.ticker == "AAA").map(|x| x.id).unwrap();
    let on = |tu: &[Value], days: &[&str]| tu[0] == Value::Equity(aaa) && days.iter().any(|d| tu[1].as_time() == Some(common::day(d)));
    if let Some(rows) = ds.facts.get_mut("volume") {
        rows.retain(|tu| !on(tu, &["2024-01-17", "2024-01-18"]));
    }
    for rel in ["open", "high", "low", "close", "volume"] {
        if let Some(rows) = ds.facts.get_mut(rel) {
            rows.retain(|tu| !on(tu, &["2024-01-30", "2024-01-31"]));
        }
    }
    let (a, received, _) = run_counting(&shared, &ds, Arm::Delivered, false);
    let (b, received_alone, _) = run_counting(&alone, &ds, Arm::Delivered, false);
    for (p, r) in [(&shared, &a), (&alone, &b)] {
        let local = run_arm(p, &ds, Arm::Local).unwrap();
        if let Some((line, x, y)) = first_difference(&digest(&local), &digest(r)) {
            panic!("{} changes line {}:\n  local:     {}\n  delivered: {}", p.strategy, line, x, y);
        }
        // The block evaluator reads a shared count's column and an own
        // count's as the row path does (E-175).
        for arm in [Arm::Delivered, Arm::Tables] {
            let block = run_on(p, &ds, arm, true).unwrap();
            if let Some((line, x, y)) = first_difference(&digest(&local), &digest(&block)) {
                panic!("{} ({:?}, block evaluator) changes line {}:\n  local: {}\n  block: {}", p.strategy, arm, line, x, y);
            }
        }
    }
    let rows = |received: &[(String, u64)], head: &str| received.iter().find(|(n, _)| n.starts_with("pushed.") && n.contains(&format!("::{}#", head))).map(|(_, r)| *r);
    // Eight weeks of two names in each answer.
    assert_eq!(
        (rows(&received, "week_high"), rows(&received, "days"), rows(&received, "volume_days")),
        (Some(16), None, Some(16)),
        "{:?}",
        received
    );
    assert_eq!((rows(&received_alone, "days"), rows(&received_alone, "volume_days")), (Some(16), Some(16)), "{:?}", received_alone);
    assert!(received.iter().chain(&received_alone).all(|(n, _)| n != "high"), "{:?} / {:?}", received, received_alone);
    // AAA was bought at the second week's end (five highs, three volumes)
    // and sold at the fourth's (three highs), in both books.
    let decided = |r: &RunResult| r.decisions.iter().map(|d| (d.t, r.describe_decision(&d.decision))).collect::<Vec<_>>();
    assert_eq!(decided(&a), decided(&b));
    let trades: Vec<(i64, String)> = decided(&a).into_iter().filter(|(_, d)| d.contains("trade")).collect();
    assert!(trades.len() == 2 && trades.iter().all(|(_, d)| d.contains("AAA")), "{:?}", trades);
    assert!(trades[0].0 >= common::day("2024-01-19") && trades[0].0 < common::day("2024-01-20"), "{:?}", trades);
    assert!(trades[1].0 >= common::day("2024-02-02") && trades[1].0 < common::day("2024-02-03"), "{:?}", trades);
}

#[test]
fn a_bucket_read_before_its_label_is_not_delivered_and_equals_the_local_run() {
    // A session-so-far high on minute bars decided at every bar: the bucket
    // is the session the bar belongs to, labelled at its last bar, so the
    // rule reads it while it is open, from the bars received so far. The
    // pass answers the closed session alone: the literal is not delivered
    // (it is read as a table where its receipt allows and locally before),
    // and every arm is the run without pushdown.
    let src = "strategy so_far {
  data { class future   need bars @1m   need sessions, point_value, commission   require reviewed   prefer latest }
  uses calendar, bars
  calendars { session = sessions   es_days = sessions of \"ES\" }
  clock { decide on base }
  universe {
    instruments { ES from 2000-01-01 }
    universe(A, T) :- instruments(A, From), session_of(A, T, _), T >= From.
  }
  params { lot : Quantity<Contracts> = 1 contracts }
  rules {
    rel high_so_far(A: Future, @T, H: Price<USD>)
    high_so_far(A, T, H) :- universe(A, T), session_of(A, T, S), H = max(P) over (T1 in bucket(session, S), close(A, T1, P)).
  }
  decisions {
    silence flat
    orders market
    decide(T, hold(A, lot)) :- high_so_far(A, T, H), close(A, T, P), P >= H.
  }
  execution futures_fixed_base
  report { calendar es_days   periods 252 per year }
}
";
    let p = common::program(src, "so_far");
    let ds = abt_kernel::data::synthetic_futures(&["ES"], (2022, 1, 3), 4, 60, 11);
    let local = run_arm(&p, &ds, Arm::Local).unwrap();
    for block in BLOCK {
        for arm in [Arm::Delivered, Arm::Tables] {
            let r = run_on(&p, &ds, arm, block).unwrap();
            if let Some((line, x, y)) = first_difference(&digest(&local), &digest(&r)) {
                panic!("{:?} (block {}) changes line {}:\n  local: {}\n  pushdown: {}", arm, block, line, x, y);
            }
            assert!(r.stats.pushed_answers > 0, "{:?}: the pass answers the closed sessions", arm);
        }
    }
    // The comparison is not vacuous: the high so far is met at many bars of
    // a session, not at its last alone.
    assert!(local.decisions.len() > 8, "{} decisions", local.decisions.len());
    let ri = p.rules.iter().position(|r| r.head.name == "high_so_far").unwrap();
    let li = p.rules[ri].body.iter().position(|l| matches!(l, abt_kernel::Literal::Agg { .. })).unwrap();
    let shape = eligibility(&p, ri, li).unwrap();
    assert!(deliverable(&shape, &p).is_err_and(|why| why.contains("open")), "{:?}", deliverable(&shape, &p));
}

#[test]
fn a_product_over_a_null_is_evaluated_locally_and_equals_the_local_run() {
    // A weekly book's mean daily dollar volume, the product of the daily
    // close and volume, with one of AAA's volumes null: the kernel reads it
    // as NaN (E-71), so that week's mean is NaN and AAA is sold. The pass
    // leaves the pair out and counts it inexact, so the product is neither
    // delivered nor read as a table, but evaluated here, and every arm is
    // the run without pushdown.
    let src = StrategyParts {
        name: "null_dv".into(),
        needs: "need bars @1d complete   need listing".into(),
        calendars: Some("weekly = weeks(1)".into()),
        clock: "decide on weekly".into(),
        universe: "universe(A, T) :- listed@weekly(A, T).".into(),
        params: "    qty : Quantity<Shares> = 10 shares\n    floor : Notional<USD> = 1 USD".into(),
        rules: "    rel dv(A: Equity, @T, D: Notional<USD>)\n    dv(A, T, D) :- universe(A, T), D = mean(X) over (T1 in bucket(weekly, T), dollar_volume(A, T1, X)).\n    rel liquid(A: Equity, @T)\n    liquid(A, T) :- dv(A, T, D), D > floor.".into(),
        decides: "    decide(T, trade(A, qty)) :- liquid(A, T), flat(A, T).\n    decide(T, trade(A, Q)) :- held(A, T, Q0), universe(A, T), not liquid(A, T), Q = -Q0.".into(),
        report: "calendar weekly   periods 52 per year".into(),
        ..Default::default()
    }
    .render();
    let p = common::program(&src, "null_dv");
    let mut ds = common::markets((2024, 1, 8), &[("AAA", &[10.0; 40]), ("BBB", &[20.0; 40])]);
    let aaa = ds.securities.iter().find(|x| x.ticker == "AAA").map(|x| x.id).unwrap();
    let null_day = common::day("2024-01-24");
    let mut nulled = 0;
    for tu in ds.facts.get_mut("volume").unwrap().iter_mut() {
        if tu[0] == Value::Equity(aaa) && tu[1].as_time() == Some(null_day) {
            // Not a number: the store holds a null.
            tu[2] = Value::Time(0);
            nulled += 1;
        }
    }
    assert_eq!(nulled, 1);
    let local = run_arm(&p, &ds, Arm::Local).unwrap();
    for block in BLOCK {
        let (a, received, _) = run_counting(&p, &ds, Arm::Delivered, block);
        let tables = run_on(&p, &ds, Arm::Tables, block).unwrap();
        for (arm, r) in [("delivered", &a), ("tables", &tables)] {
            if let Some((line, x, y)) = first_difference(&digest(&local), &digest(r)) {
                panic!("{} (block {}) changes line {}:\n  local: {}\n  pushdown: {}", arm, block, line, x, y);
            }
        }
        assert!(received.iter().all(|(n, _)| !n.contains("::dv#")), "{:?}", received);
    }
    // AAA, held from the first week, is decided again (sold) only at the end
    // of the week its volume is null.
    let aaa_decided: Vec<(i64, String)> = local.decisions.iter().map(|d| (d.t, local.describe_decision(&d.decision))).filter(|(_, d)| d.contains("AAA")).collect();
    assert!(
        aaa_decided.iter().any(|(t, _)| *t >= null_day && *t < null_day + 7 * abt_kernel::kernel::time::DAY),
        "{:?}",
        aaa_decided
    );
}

#[test]
fn a_product_read_as_a_table_over_many_names_counts_what_the_set_evaluator_counts() {
    // A daily book's mean dollar volume over seven days, the product of the
    // daily close and volume (E-156): a span is not delivered, so its
    // answers are read as a table. From `PREFETCH_MIN_ROWS` (32) names
    // entering the literal the row path solves the window over all of them
    // before it reads their answers (`Kernel::bucket_prefetch`), deriving
    // the daily dollar volume, and the block evaluator's operator declines
    // there (E-175): on 40 names, as on 5, the run, the coverage, the
    // literal counts and the stats row are the set evaluator's.
    let src = common::strategy(
        "liq_day",
        "    qty : Quantity<Shares> = 10 shares",
        "    rel liq(A: Equity, @T, D: Notional<USD>)\n    liq(A, T, D) :- universe(A, T), D = mean(X) over (T1 in window[7d] min 1, dollar_volume(A, T1, X)).",
        "    decide(T, trade(A, qty)) :- liq(A, T, D), close(A, T, P), D > P * qty, flat(A, T).\n    decide(T, trade(A, Q)) :- held(A, T, Q0), liq(A, T, D), close(A, T, P), D < P * qty, Q = -Q0.",
    );
    let p = common::program(&src, "liq_day");
    let ri = p.rules.iter().position(|r| r.head.name == "liq").unwrap();
    let li = p.rules[ri].body.iter().position(|l| matches!(l, abt_kernel::Literal::Agg { .. })).unwrap();
    let shape = eligibility(&p, ri, li).unwrap();
    assert!(shape.factor.is_some() && deliverable(&shape, &p).is_err(), "{:?}", deliverable(&shape, &p));
    for n in [40usize, 5] {
        let names: Vec<String> = (0..n).map(|i| format!("S{:02}", i)).collect();
        let syms: Vec<&str> = names.iter().map(String::as_str).collect();
        let ds = abt_kernel::data::synthetic_for_program(&p, &syms, (2022, 1, 3), 120, 5);
        for arm in [Arm::Tables, Arm::Delivered] {
            let counted = |block: bool| {
                common::run_keeping(&p, None, &ds, config(&p, arm, block), &Replay::default(), |r, k| {
                    Ok((r.coverage.clone(), k.literal_counts().to_vec(), serde_json::to_string(&r.stats).unwrap()))
                })
                .unwrap_or_else(|e| panic!("{} names, {:?} (block {}): {}", n, arm, block, e))
            };
            let (set_run, set) = counted(false);
            let (block_run, block) = counted(true);
            assert_eq!(digest(&block_run), digest(&set_run), "{} names, {:?}", n, arm);
            assert_eq!(block.0, set.0, "{} names, {:?}: the coverage differs", n, arm);
            assert_eq!(block.1, set.1, "{} names, {:?}: the literal counts differ", n, arm);
            assert_eq!(block.2, set.2, "{} names, {:?}: the stats row differs", n, arm);
            // Not vacuous: the answers were read, and from 32 names the set
            // evaluator derives the daily dollar volume though every
            // answer is read; under 32 it derives none, and the block
            // evaluator reads the answers through its plan.
            assert!(set_run.stats.pushed_answers > 0, "{} names, {:?}", n, arm);
            let derived: usize = set.0.iter().filter(|c| c.name.starts_with("dollar_volume")).map(|c| c.calls).sum();
            assert_eq!(derived > 0, n >= 32, "{} names, {:?}: {} calls of the daily dollar volume", n, arm, derived);
            if n < 32 {
                assert!(planned(&p, &ds, arm).iter().any(|r| r == "liq"), "{} names, {:?}: `liq` derived no bar through its plan", n, arm);
            }
        }
    }
}

#[test]
fn a_late_tuple_is_numbered_as_in_the_stream_without_the_delivered_answers() {
    // A weekly book priced at its weekly last close, delivered (E-157), that
    // reads its weekly volume; every volume of AAA after the 60th day is
    // received nine days late, so the weekly volume is not delivered. The
    // late records are those of the run without pushdown, their seqs among
    // them: the place of each in the stream without the answers' rows.
    let src = StrategyParts {
        name: "late_volume".into(),
        needs: "need bars @1d complete   need listing".into(),
        calendars: Some("weekly = weeks(1)".into()),
        clock: "decide on weekly".into(),
        universe: "universe(A, T) :- listed@weekly(A, T).".into(),
        params: "    qty : Quantity<Shares> = 10 shares".into(),
        decides: "    decide(T, trade(A, qty)) :- volume@weekly(A, T, V), V > 0 shares, close@weekly(A, T, P), P > 0 USD/share, flat(A, T).".into(),
        report: "calendar weekly   periods 52 per year".into(),
        ..Default::default()
    }
    .render();
    let p = common::program(&src, "late_volume");
    let mut ds = synthetic_daily(&["AAA", "BBB"], (2022, 1, 3), 140, 5);
    let aaa = ds.securities.iter().find(|x| x.ticker == "AAA").map(|x| x.id).unwrap();
    let day = abt_kernel::kernel::time::DAY;
    let volumes = ds.facts["volume"].clone();
    let times: Vec<i64> = volumes
        .iter()
        .enumerate()
        .map(|(i, tu)| {
            let t = tu[1].as_time().unwrap();
            if tu[0] == Value::Equity(aaa) && i > 120 {
                t + 9 * day
            } else {
                t + day - 1
            }
        })
        .collect();
    ds.available_at.insert("volume".into(), times);
    let local = run_arm(&p, &ds, Arm::Local).unwrap();
    assert!(!local.late.is_empty(), "volumes arrive late");
    for block in BLOCK {
        let (a, received, _) = run_counting(&p, &ds, Arm::Delivered, block);
        assert!(received.iter().any(|(n, r)| n.starts_with("pushed.") && *r > 0), "{:?}", received);
        assert_eq!(a.late, local.late, "block {}", block);
        assert_eq!(digest(&a), digest(&local), "block {}", block);
    }
}
