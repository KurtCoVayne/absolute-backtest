//! The set-at-a-time evaluator against the tuple evaluator (M-8's gate;
//! docs/spec/06-performance.md, 3.2), and the block evaluator against the
//! set evaluator (ERRATA E-150): translation validation on every corpus
//! program and both books. Each runs under every evaluator on the seeded
//! market of its class. The runs' renderings, every decision, fill, drop,
//! book row and action with its bits (`verify::tv::digest`), and the
//! ledgers of every derived relation at every bar, are equal.

#![allow(clippy::result_large_err)]

use abt_kernel::compiler::{compile, CompileOptions};
use abt_kernel::kernel::{Dataset, ExecConfig, Kernel, Replay, RunError, RunResult, SimExecutor, Value};
use abt_kernel::lower::{Lowered, Numerics};
use abt_kernel::verify::tv;
use abt_kernel::Program;
use abt_tests::{golden_inputs, golden_programs, run_kernel};

/// The configurations held equal: the tuple evaluator, the set evaluator on
/// one thread, the set evaluator cut across two and four worker kernels
/// with every entity-local rule taking the cut (`parallel_min_rows = 1`;
/// F-5.3, Proposition 3.1 of 06-performance.md), and the block evaluator
/// on one thread and on four, every relation without a plan of its own
/// served through the identity plan (its result to columns and back).
/// (set evaluator, block evaluator, threads)
const CONFIGS: [(bool, bool, usize); 6] = [(false, false, 1), (true, false, 1), (true, false, 2), (true, false, 4), (true, true, 1), (true, true, 4)];

#[test]
fn the_set_and_block_evaluators_derive_what_the_tuple_evaluator_does_on_every_corpus_program_and_both_books() {
    let mut programs = 0;
    for path in golden_programs() {
        let (p, ds) = golden_inputs(&path).unwrap();
        let mut per = Vec::new();
        for (set, block, threads) in CONFIGS {
            let mut cfg = ExecConfig::from_program(&p);
            cfg.set_eval = set;
            cfg.block_eval = block;
            cfg.block_identity = block;
            cfg.threads = threads;
            cfg.parallel_min_rows = 1;
            per.push(match run_kernel(&p, &ds, cfg) {
                Ok((r, mut k)) => {
                    let ledgers = tv::ledgers(&mut k, &p, &r.bars);
                    Ok((tv::digest(&r), ledgers))
                }
                Err(e) => Err(e.to_string()),
            });
        }
        let tuple = &per[0];
        for (i, other) in per.iter().enumerate().skip(1) {
            let (_, block, threads) = CONFIGS[i];
            let label = format!("{} ({} evaluator, {} thread(s))", p.strategy, if block { "block" } else { "set" }, threads);
            same_runs(&label, tuple, other);
        }
        programs += 1;
    }
    assert!(programs >= 24, "{} programs ran", programs);
}

/// The block evaluator counts what the set evaluator counts: per relation
/// its calls, the calls that derived a tuple and the tuples (the `coverage`
/// output), and per rule and literal the bindings entering and leaving it
/// and the calls it emptied (the `plan` table's observed counts), and the
/// stored stats row, with its plans and with the identity plan, on every
/// corpus program and both books, on one thread.
#[test]
fn the_block_evaluator_counts_what_the_set_evaluator_counts_on_every_corpus_program() {
    for path in golden_programs() {
        let (p, ds) = golden_inputs(&path).unwrap();
        let counted = |block: bool, identity: bool| {
            let mut cfg = ExecConfig::from_program(&p);
            cfg.block_eval = block;
            cfg.block_identity = identity;
            run_kernel(&p, &ds, cfg)
                .map(|(r, k)| (r.coverage.clone(), k.literal_counts().to_vec(), r.stats.clone()))
                .map_err(|e| e.to_string())
        };
        let set = counted(false, false);
        for identity in [false, true] {
            let block = counted(true, identity);
            match (&set, &block) {
                (Ok((c0, l0, s0)), Ok((c1, l1, s1))) => {
                    assert_eq!(c0, c1, "{} (identity {}): the coverage differs", p.strategy, identity);
                    assert_eq!(l0, l1, "{} (identity {}): the literal counts differ", p.strategy, identity);
                    // The stats row of the run's `report` output (E-133).
                    assert_eq!(
                        serde_json::to_string(s0).unwrap(),
                        serde_json::to_string(s1).unwrap(),
                        "{} (identity {}): the stats row differs",
                        p.strategy,
                        identity
                    );
                }
                (Err(a), Err(b)) => assert_eq!(a, b, "{}", p.strategy),
                _ => panic!("{}: one evaluator halts and the other does not", p.strategy),
            }
        }
    }
}

type Outcome = Result<(Vec<String>, tv::Ledgers), String>;

fn same_runs(label: &str, a: &Outcome, b: &Outcome) {
    match (a, b) {
        (Ok((d0, l0)), Ok((d1, l1))) => {
            if d0 != d1 {
                let (at, x, y) = tv::first_difference(d0, d1).unwrap_or((0, String::new(), String::new()));
                panic!("{}: the runs differ at line {}:\n  reference: {}\n  this run:  {}", label, at, x, y);
            }
            for (rel, rows) in l0 {
                if Some(rows) != l1.get(rel) {
                    let empty = Vec::new();
                    let (at, x, y) = tv::first_difference(rows, l1.get(rel).unwrap_or(&empty)).unwrap_or((0, String::new(), String::new()));
                    panic!("{}: the ledger of `{}` differs at row {}:\n  reference: {}\n  this run:  {}", label, rel, at, x, y);
                }
            }
            assert_eq!(l0.len(), l1.len(), "{}", label);
        }
        (Err(x), Err(y)) => assert_eq!(x, y, "{}: the runs halt differently", label),
        _ => panic!("{}: one evaluator halts and the other does not: {:?} against {:?}", label, a.as_ref().err(), b.as_ref().err()),
    }
}

/// A run under a plan (the optimised one when given), keeping the kernel.
fn run_planned<'p>(p: &'p Program, lowered: Option<&'p Lowered>, ds: &Dataset, cfg: ExecConfig) -> Result<(RunResult, Kernel<'p>), RunError> {
    let c = abt_tests::client(p, ds);
    let mut exec = SimExecutor::new(cfg.clone());
    abt_kernel::kernel::replay(p, lowered, &c, cfg, &mut exec, &Replay::default(), &mut |_| {})
}

/// Holes in a market's bars: a few names missing a bar every few days, and
/// one absent for 300 trading days (about 60 weeks) in the middle of the
/// run, listed throughout. A name's rows are then not every bar's, its
/// previous row is not the previous bar, and it comes back after a gap.
fn with_holes(mut ds: Dataset) -> Dataset {
    let mut days: Vec<i64> = ds.facts["close"].iter().filter_map(|tu| tu[1].as_time()).collect();
    days.sort_unstable();
    days.dedup();
    let index = |t: i64| days.partition_point(|&d| d < t);
    let (gappy, absent) = ([3u32, 17, 44, 81], 50u32);
    let (from, to) = (days[600], days[900]);
    for rel in ["open", "high", "low", "close", "volume"] {
        assert!(ds.available_at.get(rel).is_none_or(|v| v.is_empty()), "the synthetic bars carry no availability");
        let rows = ds.facts.get_mut(rel).unwrap();
        rows.retain(|tu| {
            let (Some(e), Some(t)) = (tu[0].as_equity(), tu[1].as_time()) else { return true };
            let gap = gappy.iter().position(|&g| g == e).is_some_and(|k| index(t) % (5 + k) == k);
            !(gap || (e == absent && from <= t && t < to))
        });
    }
    ds
}

/// MW14 over 120 names and six years with holes in the bars (names missing
/// bars, a name absent 60 weeks and returning, a split and a dividend in a
/// name's first week): the block evaluator against
/// the set evaluator, in the written order and under the optimised plan
/// the command line runs, with the whole history and trimmed, alone and on
/// four workers. Every relation with a plan derives through it at some bar.
#[test]
fn the_block_evaluator_derives_what_the_set_evaluator_does_on_mw14_with_holes() {
    let names: Vec<String> = (0..120).map(|i| format!("S{:03}", i)).collect();
    let syms: Vec<&str> = names.iter().map(String::as_str).collect();
    let p = abt_tests::program(&abt_tests::corpus_source("company", "mw14"), "mw14");
    let mut ds = with_holes(abt_kernel::data::synthetic_for_program(&p, &syms, (2010, 1, 4), 1500, 11));
    // A split and a dividend in a name's first week: the chains of the
    // split factor and the total-return index start on them.
    let mut days: Vec<i64> = ds.facts["close"].iter().filter_map(|tu| tu[1].as_time()).collect();
    days.sort_unstable();
    days.dedup();
    ds.add("split", vec![Value::Equity(2), Value::Time(days[1]), Value::Num(3.0)]);
    ds.add("dividend", vec![Value::Equity(2), Value::Time(days[0]), Value::Time(days[2]), Value::Time(days[12]), Value::Num(0.4)]);
    // Two splits of a name in one week (the week's latest is its split),
    // and a split on a week's last bar.
    let friday = |from: usize| (from..days.len()).find(|&i| abt_kernel::kernel::time::weekday(days[i]) == 4).unwrap();
    let f = friday(400);
    ds.add("split", vec![Value::Equity(5), Value::Time(days[f - 3]), Value::Num(2.0)]);
    ds.add("split", vec![Value::Equity(5), Value::Time(days[f - 1]), Value::Num(1.5)]);
    ds.add("split", vec![Value::Equity(6), Value::Time(days[f]), Value::Num(4.0)]);
    // Three dividends of a name going ex on one day, announced over two
    // weeks (two in the first, on different days), and one announced after
    // the day it goes ex.
    let g = friday(700);
    for (announced, amount) in [(g - 4, 0.25), (g - 2, 0.5), (g + 3, 0.75)] {
        ds.add(
            "dividend",
            vec![Value::Equity(7), Value::Time(days[announced]), Value::Time(days[g + 4]), Value::Time(days[g + 20]), Value::Num(amount)],
        );
    }
    ds.add(
        "dividend",
        vec![Value::Equity(8), Value::Time(days[g + 2]), Value::Time(days[g]), Value::Time(days[g + 20]), Value::Num(1.25)],
    );
    let base = ExecConfig::from_program(&p);
    let compiled = compile(
        &p,
        &CompileOptions {
            numerics: Numerics::from_running_sums(base.running_sums),
            ..CompileOptions::default()
        },
    );
    let mut planned_rels: Vec<String> = Vec::new();
    for lowered in [None, Some(&compiled.lowered)] {
        for trim in [false, true] {
            let run = |block: bool, threads: usize| {
                let mut cfg = base.clone();
                cfg.block_eval = block;
                cfg.trim_stores = trim;
                cfg.threads = threads;
                cfg.parallel_min_rows = 1;
                match run_planned(&p, lowered, &ds, cfg) {
                    Ok((r, mut k)) => {
                        let counted = (r.coverage.clone(), k.literal_counts().to_vec(), serde_json::to_string(&r.stats).unwrap());
                        let called: Vec<String> = r.coverage.iter().filter(|c| c.calls > 0).map(|c| c.name.clone()).collect();
                        // A trimmed kernel no longer holds what the ledgers read.
                        let ledgers = if trim { Default::default() } else { tv::ledgers(&mut k, &p, &r.bars) };
                        let counts = k.plan_counts();
                        (Ok((tv::digest(&r), ledgers)), counts, Some(counted), r.bars.len(), called)
                    }
                    Err(e) => (Err(e.to_string()), Vec::new(), None, 0, Vec::new()),
                }
            };
            let (set, _, set_counted, bars, _) = run(false, 1);
            assert!(bars > 200, "{} bars", bars);
            for threads in [1, 4] {
                let (block, counts, counted, _, called) = run(true, threads);
                let label = format!(
                    "mw14 with holes ({}, {}, block evaluator, {} thread(s))",
                    if lowered.is_some() { "optimised" } else { "written" },
                    if trim { "trimmed" } else { "whole" },
                    threads
                );
                same_runs(&label, &set, &block);
                if threads == 1 {
                    assert!(set_counted == counted, "{}: the coverage, the literal counts or the stats row differ", label);
                }
                // A relation with a plan the run demands derives through it.
                // The daily dollar volume has one and is not demanded: only
                // the weekly mean reads it, which is delivered (E-157). On
                // four workers the first barrier warms every open-loop
                // relation, it among them.
                for (name, planned, bars, _, _) in counts {
                    if planned && called.contains(&name) {
                        assert!(bars > 0, "{}: `{}` has a plan and derived no bar through it", label, name);
                        if !planned_rels.contains(&name) {
                            planned_rels.push(name);
                        }
                    }
                }
                if threads == 1 {
                    assert!(!called.iter().any(|n| n.starts_with("dollar_volume")), "{}: the daily dollar volume is derived: {:?}", label, called);
                }
            }
        }
    }
    // The weekly last close, the weekly count of bars and the weekly mean
    // dollar volume read the answers the stream delivers (E-175).
    for name in [
        "adj_close",
        "scored",
        "in_book",
        "latched",
        "extension",
        "stay",
        "ma",
        "clenow",
        "close@weekly",
        "traded@weekly",
        "dvol",
        "eligible",
        "universe",
        "prev_row",
        "first_row",
        "row_number",
        "cum_split",
        "tr_index",
        "total_return",
        "latch[enter, stay]",
        "split_factor",
        // `dividend_or_zero` is planned too (E-170) and no longer demanded:
        // the total return reads `dividend_at` and `dividend_reinvested`
        // since E-163. Its case is held by
        // `the_case_and_the_calendar_window_over_actions_are_the_row_paths`.
        "dividend_at",
    ] {
        assert!(planned_rels.iter().any(|r| r == name), "`{}` has no plan; the plans: {:?}", name, planned_rels);
    }
}

/// A weekly book reading each name at its own last bar of the week through
/// an as-of read of a stored relation, and its close there through a
/// relation of the base calendar (`seen`); `early` holds the names read
/// before the label.
fn last_bar_book() -> String {
    let rules = "    rel px(A: Equity, @T: base, P: Price<USD>)\n    px(A, T, P) :- close(A, T, P).\n    rel seen(A: Equity, @T, L: Timestamp, P: Price<USD>)\n    seen(A, T, L, P) :- universe(A, T), close@base(A, L, _) asof T, px(A, L, P).\n    rel early(A: Equity, @T)\n    early(A, T) :- seen(A, T, L, _), L < T.\n";
    abt_tests::StrategyParts {
        name: "lastbar".into(),
        needs: "need bars @1d complete   need listing".into(),
        calendars: Some("weekly = weeks(1)".into()),
        clock: "decide on weekly".into(),
        universe: "universe(A, T) :- traded@weekly(A, T).".into(),
        params: "    w : Scalar = 0.01\n    hi : Price<USD> = 100 USD/share".into(),
        rules: rules.into(),
        decides: "    decide(T, hold(A, w)) :- seen(A, T, L, P), P >= hi, not early(A, T).\n    decide(T, hold(A, 0)) :- seen(A, T, L, P), P < hi.\n    decide(T, hold(A, 0)) :- early(A, T).".into(),
        execution: abt_tests::FRICTIONLESS.into(),
        report: "calendar weekly   periods 52 per year".into(),
        ..Default::default()
    }
    .render()
}

/// A daily book over an entity's previous row, its first rows and its row
/// count: the close at the previous row (`back`) and the first three rows
/// (`fresh`, through `row_number`).
fn previous_row_book() -> String {
    let rules = "    rel back(A: Equity, @T, T0: Timestamp, P0: Price<USD>)\n    back(A, T, T0, P0) :- universe(A, T), prev_row(A, T, T0), close(A, T0, P0).\n    rel fresh(A: Equity, @T, K: Count)\n    fresh(A, T, K) :- universe(A, T), row_number(A, T, K), K < 3.\n";
    let decides = "    decide(T, hold(A, w)) :- back(A, T, T0, P0), P0 > 100 USD/share, not fresh(A, T, _).\n    decide(T, hold(A, 0)) :- back(A, T, T0, P0), P0 <= 100 USD/share.\n    decide(T, hold(A, 0)) :- fresh(A, T, K).";
    abt_tests::StrategyParts {
        name: "previous".into(),
        needs: "need bars @1d complete   need listing".into(),
        params: "    w : Scalar = 0.01".into(),
        rules: rules.into(),
        decides: decides.into(),
        execution: abt_tests::FRICTIONLESS.into(),
        ..Default::default()
    }
    .render()
}

/// A weekly book over the catalog's corporate actions, as MW14 reads them:
/// the week's split factor and dividend (`split_factor`, a case over an
/// as-of read of `split` within the week; `dividend_or_zero`, a case over
/// `dividend_at`), the dividends going ex in the week (`dividend_at`, a
/// window over a year of weeks of `dividend` read as of each, tested
/// against the week at each call), and `paid`: a sum of an expression of
/// the dividends over half a year, with a test of what only the call binds,
/// and a count over the quarter before, with none. Its numerics are
/// two-pass, as both books' are: under running sums no window has a plan.
fn dividend_book() -> String {
    let rules = "    rel paid(A: Equity, @T, S: Price<USD>, N: Count)\n    paid(A, T, S, N) :- universe(A, T), prev(T, T1),\n        S = sum(Amt * 2 - 0.01 USD/share) over (T0 in window[6mo] min 1, dividend(A, T0, Ex, _, Amt), Ex > T1),\n        N = count(Amt) over (T0 in prior_window[3mo] min 0, dividend(A, T0, _, _, Amt)).\n    rel seen(A: Equity, @T, D: Price<USD>, F: Scalar)\n    seen(A, T, D, F) :- dividend_or_zero(A, T, D), split_factor(A, T, F).\n";
    let decides = "    decide(T, hold(A, w)) :- seen(A, T, D, F), paid(A, T, S, N), D > 0 USD/share, N > 0.\n    decide(T, hold(A, 0)) :- seen(A, T, D, F), paid(A, T, S, N), D <= 0 USD/share, S * F > 0.5 USD/share.";
    abt_tests::StrategyParts {
        name: "dividends".into(),
        needs: "need bars @1d complete   need actions, listing".into(),
        uses: "calendar, catalog, book".into(),
        calendars: Some("weekly = weeks(1)".into()),
        clock: "decide on weekly".into(),
        universe: "universe(A, T) :- traded@weekly(A, T).".into(),
        params: "    w : Scalar = 0.01".into(),
        rules: rules.into(),
        decides: decides.into(),
        execution: abt_tests::FRICTIONLESS.replace("numerics running", "numerics two_pass"),
        report: "calendar weekly   periods 52 per year".into(),
        ..Default::default()
    }
    .render()
}

/// The actions the dividend book reads, on `names` names over the market's
/// days: about every 13 weeks a name's dividend, going ex a few days after
/// it is announced or, one in seven, two days before; one in four with a
/// second announced two days after it, in the same week or the next; a
/// split of every fifth name, every tenth's with a second three days later;
/// name 9's dividends received three days after their announcement. A day
/// a name's dividend is already announced on is left as it is.
fn add_actions(ds: &mut Dataset, names: u32, seed: u64) {
    let mut days: Vec<i64> = ds.facts["close"].iter().filter_map(|tu| tu[1].as_time()).collect();
    days.sort_unstable();
    days.dedup();
    let mut x = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
    let mut next = move |n: usize| {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        (x % n as u64) as usize
    };
    let day = 86_400_000_000_000i64;
    // A name holds one dividend per announcement day (its identity): the
    // market's own are kept.
    let held: Vec<(Option<u32>, Option<i64>)> = ds.facts.get("dividend").map(|v| v.iter().map(|tu| (tu[0].as_equity(), tu[1].as_time())).collect()).unwrap_or_default();
    for e in 0..names {
        let mut i = 5 + (e as usize * 7) % 60;
        while i + 30 < days.len() {
            let ex = if next(7) == 0 { i - 2 } else { i + 3 + next(12) };
            let amount = 0.05 + next(100) as f64 / 400.0;
            let mut events = vec![(i, ex, amount)];
            if next(4) == 0 {
                events.push((i + 2, ex + next(3), amount / 2.0));
            }
            for (at, ex, amount) in events {
                if held.contains(&(Some(e), Some(days[at]))) {
                    continue;
                }
                let tu = vec![Value::Equity(e), Value::Time(days[at]), Value::Time(days[ex]), Value::Time(days[ex + 10]), Value::Num(amount)];
                if e == 9 {
                    ds.add_available("dividend", tu, days[at] + 3 * day);
                } else {
                    ds.add("dividend", tu);
                }
            }
            i += 55 + next(20);
        }
        if e % 5 == 0 {
            let at = 100 + next(days.len() - 200);
            ds.add("split", vec![Value::Equity(e), Value::Time(days[at]), Value::Num(2.0)]);
            if e % 10 == 0 {
                ds.add("split", vec![Value::Equity(e), Value::Time(days[at + 3]), Value::Num(1.5)]);
            }
        }
    }
}

/// The case over a read with the entity bound and the calendar window over
/// an as-of read of a stored relation (ERRATA E-170, E-171), on the
/// dividend book over names that miss bars, with random actions (two
/// dividends announced in one week, dividends announced after they go ex,
/// two splits in a week, a name's dividends received late): the block
/// evaluator against the set evaluator, whole and trimmed, alone and on
/// four workers, the counts and the stats row on one thread; each relation
/// is planned.
#[test]
fn the_case_and_the_calendar_window_over_actions_are_the_row_paths() {
    let p = abt_tests::program(&dividend_book(), "dividends");
    let names: Vec<String> = (0..60).map(|i| format!("S{:03}", i)).collect();
    let syms: Vec<&str> = names.iter().map(String::as_str).collect();
    let mut ds = with_holes(abt_kernel::data::synthetic_for_program(&p, &syms, (2018, 1, 2), 1000, 21));
    add_actions(&mut ds, 60, 21);
    for trim in [false, true] {
        let run = |block: bool, threads: usize| {
            let mut cfg = ExecConfig::from_program(&p);
            cfg.block_eval = block;
            cfg.trim_stores = trim;
            cfg.threads = threads;
            cfg.parallel_min_rows = 1;
            let (r, mut k) = run_kernel(&p, &ds, cfg).expect("the book runs");
            let counted = (r.coverage.clone(), k.literal_counts().to_vec(), serde_json::to_string(&r.stats).unwrap());
            let planned: Vec<(String, u64, u64)> = k.plan_counts().into_iter().filter(|c| c.1).map(|c| (c.0, c.2, c.3)).collect();
            let ledgers = if trim { Default::default() } else { tv::ledgers(&mut k, &p, &r.bars) };
            let outcome: Outcome = Ok((tv::digest(&r), ledgers));
            (outcome, counted, planned, r.late.len())
        };
        let (set, set_counted, _, late) = run(false, 1);
        assert!(late > 5, "{} late tuples", late);
        if !trim {
            let ledger = &set.as_ref().unwrap().1;
            let split = ledger["split_factor"].iter().filter(|row| !row.ends_with(" 1")).count();
            assert!(
                ledger["dividend_at"].len() > 300 && ledger["paid"].len() > 1000 && split > 5,
                "{} dividends, {} paid, {} splits",
                ledger["dividend_at"].len(),
                ledger["paid"].len(),
                split
            );
        }
        for threads in [1, 4] {
            let (block, counted, planned, _) = run(true, threads);
            let name = format!("actions ({}, block evaluator, {} thread(s))", if trim { "trimmed" } else { "whole" }, threads);
            same_runs(&name, &set, &block);
            if threads == 1 {
                assert!(set_counted == counted, "{}: the coverage, the literal counts or the stats row differ", name);
            }
            for rel in ["split_factor", "dividend_or_zero", "dividend_at", "paid", "seen"] {
                let bars = planned.iter().find(|(r, ..)| r == rel).map(|(_, b, _)| *b).unwrap_or(0);
                assert!(bars > 100, "{}: `{}` planned at {} bars: {:?}", name, rel, bars, planned);
            }
        }
    }
}

/// An as-of read of a stored relation with the entity bound, then a read
/// of a daily relation at the time it bound (ERRATA E-152): a weekly book
/// over names that miss bars, some on the week's last day, each read at
/// its own last bar of the week (`close@base(A, L, _) asof T`, MW14's
/// membership test) and its close read there through a relation of the
/// base calendar (`px(A, L, P)`); a name absent 60 weeks returns. The block
/// evaluator against the set evaluator, whole and trimmed, alone and on
/// four workers, the counts and the stats row on one thread; the relation
/// is planned, and reads bars other than the label.
#[test]
fn an_as_of_read_with_the_entity_bound_finds_the_row_paths_bar_and_the_read_there_its_tuple() {
    let p = abt_tests::program(&last_bar_book(), "lastbar");
    let names: Vec<String> = (0..60).map(|i| format!("S{:03}", i)).collect();
    let syms: Vec<&str> = names.iter().map(String::as_str).collect();
    let ds = with_holes(abt_kernel::data::synthetic_for_program(&p, &syms, (2018, 1, 2), 1000, 3));
    for trim in [false, true] {
        let run = |block: bool, threads: usize| {
            let mut cfg = ExecConfig::from_program(&p);
            cfg.block_eval = block;
            cfg.trim_stores = trim;
            cfg.threads = threads;
            cfg.parallel_min_rows = 1;
            let (r, mut k) = run_kernel(&p, &ds, cfg).expect("the book runs");
            let counted = (r.coverage.clone(), k.literal_counts().to_vec(), serde_json::to_string(&r.stats).unwrap());
            let planned = k.plan_counts().into_iter().find(|c| c.0 == "seen").map(|c| (c.1, c.2, c.3));
            let ledgers = if trim { Default::default() } else { tv::ledgers(&mut k, &p, &r.bars) };
            let outcome: Outcome = Ok((tv::digest(&r), ledgers));
            (outcome, counted, planned)
        };
        let (set, set_counted, _) = run(false, 1);
        if !trim {
            let ledger = &set.as_ref().unwrap().1;
            assert!(ledger["early"].len() > 50 && ledger["seen"].len() > 5000, "{} names read before the label", ledger["early"].len());
        }
        for threads in [1, 4] {
            let (block, counted, planned) = run(true, threads);
            let name = format!("last bars ({}, block evaluator, {} thread(s))", if trim { "trimmed" } else { "whole" }, threads);
            same_runs(&name, &set, &block);
            if threads == 1 {
                assert!(set_counted == counted, "{}: the coverage, the literal counts or the stats row differ", name);
            }
            let (has, bars, _) = planned.expect("`seen` is counted");
            assert!(has && bars > 100, "{}: `seen` planned at {} bars", name, bars);
        }
    }
}

/// An entity's previous row and its first row over columns (ERRATA E-153):
/// a daily book whose names miss bars, one absent 300 days, longer than
/// the run's horizon, so that its previous row on its return is behind it
/// (read as the row path reads it there, E-126); `prev_row` (`prev`, then
/// an as-of read of `universe` with the entity bound), `first_row` (a
/// negated read of `prev_row`), the close at the previous row and a count
/// of rows over the chain (`row_number`). The block evaluator against the
/// set evaluator, whole and trimmed, alone and on four workers, the counts
/// and the stats row on one thread; each relation is planned.
#[test]
fn the_previous_row_over_columns_is_the_row_paths_across_holes_and_the_horizon() {
    let p = abt_tests::program(&previous_row_book(), "previous");
    let names: Vec<String> = (0..60).map(|i| format!("S{:03}", i)).collect();
    let syms: Vec<&str> = names.iter().map(String::as_str).collect();
    let ds = with_holes(abt_kernel::data::synthetic_for_program(&p, &syms, (2018, 1, 2), 1000, 13));
    for trim in [false, true] {
        let run = |block: bool, threads: usize| {
            let mut cfg = ExecConfig::from_program(&p);
            cfg.block_eval = block;
            cfg.trim_stores = trim;
            cfg.threads = threads;
            cfg.parallel_min_rows = 1;
            let (r, mut k) = run_kernel(&p, &ds, cfg).expect("the book runs");
            let counted = (r.coverage.clone(), k.literal_counts().to_vec(), serde_json::to_string(&r.stats).unwrap());
            let planned: Vec<(String, u64)> = k.plan_counts().into_iter().filter(|c| c.1).map(|c| (c.0, c.2)).collect();
            let ledgers = if trim { Default::default() } else { tv::ledgers(&mut k, &p, &r.bars) };
            let outcome: Outcome = Ok((tv::digest(&r), ledgers));
            (outcome, counted, planned)
        };
        let (set, set_counted, _) = run(false, 1);
        if !trim {
            let ledger = &set.as_ref().unwrap().1;
            assert!(
                ledger["back"].len() > 20_000 && ledger["first_row"].len() >= 60,
                "{} rows back, {} first rows",
                ledger["back"].len(),
                ledger["first_row"].len()
            );
        }
        for threads in [1, 4] {
            let (block, counted, planned) = run(true, threads);
            let name = format!("previous rows ({}, block evaluator, {} thread(s))", if trim { "trimmed" } else { "whole" }, threads);
            same_runs(&name, &set, &block);
            if threads == 1 {
                assert!(set_counted == counted, "{}: the coverage, the literal counts or the stats row differ", name);
            }
            for rel in ["prev_row", "first_row", "back"] {
                let bars = planned.iter().find(|(r, _)| r == rel).map(|(_, b)| *b).unwrap_or(0);
                assert!(bars > 100, "{}: `{}` planned at {} bars: {:?}", name, rel, bars, planned);
            }
        }
    }
}

/// The block evaluator resumed from a checkpoint, and fed tuples received
/// after their bar (ERRATA E-150: a memo the late tuple or the resume
/// cleared plans nothing behind the horizon until the next trim): the
/// last-bar book, the previous-row book and the dividend book (whose window
/// caches the late tuple cuts back) over names missing bars, with a name's
/// closes of some days received three days late, decide as the
/// set evaluator does, whole and trimmed; resumed from monthly
/// checkpoints, as the set evaluator resumed from the same checkpoint
/// does (with the whole history, as the unbroken run).
#[test]
fn the_block_evaluator_resumed_or_fed_late_tuples_decides_as_the_set_evaluator() {
    use abt_kernel::kernel::CheckpointEvery;
    for (src, name) in [(previous_row_book(), "previous"), (last_bar_book(), "lastbar"), (dividend_book(), "dividends")] {
        let p = abt_tests::program(&src, name);
        let names: Vec<String> = (0..60).map(|i| format!("S{:03}", i)).collect();
        let syms: Vec<&str> = names.iter().map(String::as_str).collect();
        let mut ds = with_holes(abt_kernel::data::synthetic_for_program(&p, &syms, (2018, 1, 2), 950, 17));
        if name == "dividends" {
            add_actions(&mut ds, 60, 17);
        }
        // Name 7's close every 37th bar (on every weekday in turn) arrives
        // three days after its bar.
        let closes = std::mem::take(ds.facts.get_mut("close").unwrap());
        let mut late = 0;
        for (i, tu) in closes.into_iter().enumerate() {
            match (tu[0].as_equity(), tu[1].as_time()) {
                (Some(7), Some(t)) if i % 37 == 0 => {
                    ds.add_available("close", tu, t + 3 * 86_400_000_000_000);
                    late += 1;
                }
                _ => ds.add("close", tu),
            }
        }
        assert!(late > 5, "{} late closes", late);
        for trim in [false, true] {
            let cfg = |block: bool| {
                let mut cfg = ExecConfig::from_program(&p);
                cfg.block_eval = block;
                cfg.trim_stores = trim;
                cfg
            };
            let c = abt_tests::client(&p, &ds);
            // On the kernel's own thread, as a run is: a memo the late
            // tuple cleared is derived again down each name's chains.
            let replayed = |cfg: ExecConfig, opts: &Replay| {
                abt_kernel::kernel::on_kernel_thread(|| {
                    let mut exec = SimExecutor::new(cfg.clone());
                    let mut kept = Vec::new();
                    let (r, _) = abt_kernel::kernel::replay(&p, None, &c, cfg, &mut exec, opts, &mut |cp| kept.push(cp))?;
                    Ok((tv::digest(&r), r.late.len(), kept))
                })
                .expect("the book runs")
            };
            let monthly = Replay {
                checkpoint_every: Some(CheckpointEvery::Month),
                ..Replay::default()
            };
            let (set, set_late, checkpoints) = replayed(cfg(false), &monthly);
            let (block, block_late, _) = replayed(cfg(true), &monthly);
            let label = format!("{} ({})", name, if trim { "trimmed" } else { "whole" });
            assert!(set_late > 5 && set_late == block_late, "{}: {} late tuples, {} under the block evaluator", label, set_late, block_late);
            same_runs(&format!("{}, late tuples", label), &Ok((set.clone(), Default::default())), &Ok((block, Default::default())));
            assert!(checkpoints.len() > 6, "{}: {} checkpoints", label, checkpoints.len());
            // Resumed, each evaluator from the same checkpoint: what the
            // horizon dropped before the resume is read as the row path
            // reads it (E-135), so the two resumed runs are compared.
            let mut resumed_apart = 0;
            for cp in checkpoints.into_iter().step_by(5) {
                let at = cp.last_bar;
                let (by_set, _, _) = replayed(
                    cfg(false),
                    &Replay {
                        from: Some(cp.clone()),
                        ..Replay::default()
                    },
                );
                let (by_block, _, _) = replayed(cfg(true), &Replay { from: Some(cp), ..Replay::default() });
                resumed_apart += usize::from(by_set != set);
                same_runs(&format!("{}, resumed after {}", label, at), &Ok((by_set, Default::default())), &Ok((by_block, Default::default())));
            }
            if !trim {
                assert_eq!(resumed_apart, 0, "{}: a resumed run differs from the unbroken one", label);
            }
        }
    }
}

/// The window operator over columns (ERRATA E-151) against the row path,
/// on a book whose windows are asked at bars that vary by name: names
/// asked at every bar, names asked at some bars and not at others as their
/// price crosses a floor (a group brought up over the bars it was not
/// asked at), names first asked late (a group walking back). A mean, a
/// count with a minimum of one row, and two regressions over an expression
/// of the rows and a Count (the shape of MW14's Clenow score), each over
/// its own window; whole and trimmed, one thread and four, the counts and
/// the stats row on one. Then a price planted on a name asked around it
/// where the regressions' logarithm is partial: the bars whose windows
/// read it decline, and the rules leave its instances stuck as they do
/// without a plan, which no decision demands.
#[test]
fn the_window_operator_feeds_what_the_row_path_feeds_under_every_asking_pattern() {
    let params = "    k : Count = 6\n    lo : Price<USD> = 90 USD/share\n    hi : Price<USD> = 150 USD/share\n    x : Scalar = 0.25\n    w : Scalar = 0.02";
    let rules = "    rel px(A: Equity, @T, P: Price<USD>)\n    px(A, T, P) :- close(A, T, P).\n    rel win(A: Equity, @T, M: Price<USD>, N: Count, B: Scalar, R: Scalar)\n    win(A, T, M, N, B, R) :- px(A, T, P), P >= lo, P0 = 1 USD/share,\n        M = mean(Q) over (T1 in rows[k], px(A, T1, Q)),\n        N = count(Q) over (T1 in rows[k] min 1, px(A, T1, Q)),\n        B = ols_slope(log(Q / P0 - x), J) over (T1 in rows[k], px(A, T1, Q), row_number(A, T1, J)),\n        R = ols_r(log(Q / P0 - x), J) over (T1 in rows[k], px(A, T1, Q), row_number(A, T1, J)).\n    rel rich(A: Equity, @T)\n    rich(A, T) :- px(A, T, P), P >= hi.\n";
    let decides = "    decide(T, hold(A, w)) :- rich(A, T), win(A, T, M, N, B, R), B > 0.\n    decide(T, hold(A, 0)) :- rich(A, T), win(A, T, M, N, B, R), B <= 0.";
    // The book's numerics are two-pass, as both books' are: under running
    // sums no window has a plan.
    let src = abt_tests::StrategyParts {
        name: "windows".into(),
        params: params.into(),
        rules: rules.into(),
        decides: decides.into(),
        execution: abt_tests::FRICTIONLESS.replace("numerics running", "numerics two_pass"),
        ..Default::default()
    }
    .render();
    let p = abt_tests::program(&src, "windows");
    let names: Vec<String> = (0..24).map(|i| format!("S{:03}", i)).collect();
    let syms: Vec<&str> = names.iter().map(String::as_str).collect();
    let clean = abt_kernel::data::synthetic_for_program(&p, &syms, (2018, 1, 2), 400, 5);
    // The name to plant on: asked at many bars (its price above `lo`) and
    // never above `hi`, so that no decision reads it; a price below `x`
    // (the store refuses one that is not positive) goes on a bar after the
    // middle that a bar asked follows.
    let closes = |e: u32| -> Vec<(i64, f64)> {
        let mut v: Vec<(i64, f64)> = clean.facts["close"]
            .iter()
            .filter(|tu| tu[0].as_equity() == Some(e))
            .filter_map(|tu| Some((tu[1].as_time()?, tu[2].as_f64()?)))
            .collect();
        v.sort_by_key(|x| x.0);
        v
    };
    let (target, at) = (0..24u32)
        .find_map(|e| {
            let c = closes(e);
            let asked = c.iter().filter(|x| x.1 >= 90.0).count();
            let top = c.iter().map(|x| x.1).fold(f64::MIN, f64::max);
            let at = (c.len() / 2..c.len() - 1).find(|&i| c[i + 1].1 >= 90.0)?;
            (asked > 40 && asked + 40 < c.len() && top < 140.0).then(|| (e, c[at].0))
        })
        .expect("a name asked at some bars and never above the decision's floor");
    let mut planted = clean.clone();
    for tu in planted.facts.get_mut("close").unwrap().iter_mut() {
        if tu[0].as_equity() == Some(target) && tu[1].as_time() == Some(at) {
            tu[2] = Value::Num(0.1);
        }
    }
    for (ds, label) in [(&clean, "clean"), (&planted, "a price below the logarithm's domain planted")] {
        for trim in [false, true] {
            let run = |block: bool, threads: usize| {
                let mut cfg = ExecConfig::from_program(&p);
                cfg.block_eval = block;
                cfg.trim_stores = trim;
                cfg.threads = threads;
                cfg.parallel_min_rows = 1;
                match run_kernel(&p, ds, cfg) {
                    Ok((r, mut k)) => {
                        let counted = (r.coverage.clone(), k.literal_counts().to_vec(), serde_json::to_string(&r.stats).unwrap());
                        let ledgers = if trim { Default::default() } else { tv::ledgers(&mut k, &p, &r.bars) };
                        let win = k.plan_counts().into_iter().find(|c| c.0 == "win").map(|c| (c.2, c.3));
                        (Ok((tv::digest(&r), ledgers)), Some(counted), win)
                    }
                    Err(e) => (Err(e.to_string()), None, None),
                }
            };
            let (set, set_counted, _) = run(false, 1);
            assert!(set.is_ok(), "{}: {:?}", label, set.as_ref().err());
            for threads in [1, 4] {
                let (block, counted, win) = run(true, threads);
                let name = format!("windows ({}, {}, block evaluator, {} thread(s))", label, if trim { "trimmed" } else { "whole" }, threads);
                same_runs(&name, &set, &block);
                if threads == 1 {
                    assert!(set_counted == counted, "{}: the coverage, the literal counts or the stats row differ", name);
                }
                let (bars, declined) = win.expect("`win` has a plan");
                assert!(bars > 100, "{}: {} bars planned", name, bars);
                if std::ptr::eq(ds, &planted) {
                    assert!(declined > 0, "{}: the planted price declines no bar", name);
                }
            }
        }
    }
}

/// A window over a relation with a window of its own, asked at month
/// starts only, over more names than the row path's solve of a window's
/// bar over the whole block takes (`prefetch_rows_window`, 32 rows): the
/// row path derives the inner relation at the bar before it brings any
/// name's outer window over the month's older bars, whose inner windows
/// are then asked behind their last bar. The window operator derives the
/// relations in the same order, so that the stats row (the requests
/// behind a group's last bar, the bars solved) and the coverage are the
/// set evaluator's, with the memo kept and with it evicted at every
/// barrier, the history whole and trimmed.
#[test]
fn the_window_operator_derives_a_windowed_relation_in_the_row_paths_order() {
    let rules = "    rel px(A: Equity, @T, P: Price<USD>)\n    px(A, T, P) :- close(A, T, P).\n    rel inner(A: Equity, @T, M: Price<USD>)\n    inner(A, T, M) :- px(A, T, P), M = mean(Q) over (T1 in rows[3], px(A, T1, Q)).\n    rel outer(A: Equity, @T, S: Price<USD>)\n    outer(A, T, S) :- universe(A, T), S = mean(M) over (T1 in rows[5], inner(A, T1, M)).\n    rel rebalance(@T)\n    rebalance(T) :- bar(T), month_start(T).\n";
    let src = abt_tests::StrategyParts {
        name: "nested".into(),
        params: "    w : Scalar = 0.01".into(),
        rules: rules.into(),
        decides: "    decide(T, hold(A, w)) :- rebalance(T), outer(A, T, S), S > 0 USD/share.".into(),
        execution: abt_tests::FRICTIONLESS.replace("numerics running", "numerics two_pass"),
        ..Default::default()
    }
    .render();
    let p = abt_tests::program(&src, "nested");
    let names: Vec<String> = (0..40).map(|i| format!("S{:03}", i)).collect();
    let syms: Vec<&str> = names.iter().map(String::as_str).collect();
    let ds = abt_kernel::data::synthetic_for_program(&p, &syms, (2018, 1, 2), 300, 5);
    for evict in [false, true] {
        for trim in [false, true] {
            let run = |block: bool| {
                let mut cfg = ExecConfig::from_program(&p);
                cfg.block_eval = block;
                cfg.trim_stores = trim;
                if evict {
                    cfg.memo_min = 0;
                }
                let (r, mut k) = run_kernel(&p, &ds, cfg).expect("the book runs");
                let counted = (r.coverage.clone(), k.literal_counts().to_vec(), serde_json::to_string(&r.stats).unwrap());
                let planned: Vec<(String, u64)> = k.plan_counts().into_iter().filter(|c| c.1).map(|c| (c.0, c.2)).collect();
                let ledgers = if trim { Default::default() } else { tv::ledgers(&mut k, &p, &r.bars) };
                let outcome: Outcome = Ok((tv::digest(&r), ledgers));
                (outcome, counted, r.stats.window_backward, planned)
            };
            let name = format!(
                "nested windows ({}, {})",
                if evict { "the memo evicted" } else { "the memo kept" },
                if trim { "trimmed" } else { "whole" }
            );
            let (set, set_counted, backward, _) = run(false);
            assert!(backward > 0, "{}: no inner window is asked behind its last bar", name);
            let (block, counted, _, planned) = run(true);
            same_runs(&name, &set, &block);
            let bars = |rel: &str| planned.iter().find(|(r, _)| r == rel).map(|(_, b)| *b).unwrap_or(0);
            assert!(bars("outer") > 0 && bars("inner") > 0, "{}: {:?}", name, planned);
            assert_eq!(set_counted.0, counted.0, "{}: the coverage differs", name);
            assert_eq!(set_counted.1, counted.1, "{}: the literal counts differ", name);
            assert_eq!(set_counted.2, counted.2, "{}: the stats row differs", name);
        }
    }
}

/// The compiled expressions against the row path's on the values a book
/// meets: Counts with Counts (`+`, `-`, `*` stay Counts, `/` is a number),
/// numbers with Counts, the functions of one number, `least` and
/// `greatest` across the two kinds, comparisons across them; a division by
/// zero on every row, excluded by a later test (Kleene: the plan declines
/// and the rules exclude the instances), and a log of a negative value a
/// decision demands, which halts both runs alike.
#[test]
fn compiled_expressions_give_the_row_paths_values_and_halts() {
    let rules = "    rel px(A: Equity, @T, P: Price<USD>, V: Quantity<Shares>)\n    px(A, T, P, V) :- close(A, T, P), volume(A, T, V).\n    rel score(A: Equity, @T, R: Scalar, N: Count, M: Scalar, G: Scalar)\n    score(A, T, R, N, M, G) :- px(A, T, P, V), P0 = 1 USD/share, R = log(P / P0) - abs(P / P0 - x) / k + sqrt(P / P0) * sign(P / P0 - x),\n        N = k * 2 - 1 + k, M = k / 2 - floor(P / P0) + ceil(P / P0), G = least(P / P0, x) + greatest(P / P0, 3) + exp(-P / P0), N > k, R > -100.\n    rel nothing(A: Equity, @T, Q: Scalar)\n    nothing(A, T, Q) :- px(A, T, P, V), P0 = 1 USD/share, Q = 1 / (P / P0 - P / P0), P < 0 USD/share.\n";
    let decides = "    decide(T, hold(A, 0.1)) :- score(A, T, R, N, M, G), R > 0.\n    decide(T, hold(A, 0)) :- nothing(A, T, Q).";
    let params = "    k : Count = 3\n    x : Scalar = 40";
    let good = abt_tests::strategy("expressions", params, rules, decides);
    let bad_rules = format!(
        "{}    rel bad(A: Equity, @T, X: Scalar)\n    bad(A, T, X) :- px(A, T, P, V), P0 = 1 USD/share, X = log(P / P0 - 1000).\n",
        rules
    );
    let bad = abt_tests::strategy("expressions_halt", params, &bad_rules, "    decide(T, hold(A, 0.1)) :- bad(A, T, X), X > 0.");
    let names: Vec<String> = (0..24).map(|i| format!("S{:03}", i)).collect();
    let syms: Vec<&str> = names.iter().map(String::as_str).collect();
    for (src, name, halts) in [(good, "expressions", false), (bad, "expressions_halt", true)] {
        let p = abt_tests::program(&src, name);
        let ds = abt_kernel::data::synthetic_for_program(&p, &syms, (2022, 1, 3), 120, 7);
        let run = |block: bool| {
            let mut cfg = ExecConfig::from_program(&p);
            cfg.block_eval = block;
            match run_kernel(&p, &ds, cfg) {
                Ok((r, mut k)) => {
                    let ledgers = tv::ledgers(&mut k, &p, &r.bars);
                    let planned: Vec<(String, u64)> = k.plan_counts().into_iter().filter(|c| c.1).map(|c| (c.0, c.2)).collect();
                    (Ok((tv::digest(&r), ledgers)), planned)
                }
                Err(e) => (Err(e.to_string()), Vec::new()),
            }
        };
        let (set, _) = run(false);
        let (block, planned) = run(true);
        same_runs(name, &set, &block);
        assert_eq!(set.is_err(), halts, "{}: {:?}", name, set.as_ref().err());
        if !halts {
            let bars = |rel: &str| planned.iter().find(|(r, _)| r == rel).map(|(_, b)| *b);
            assert!(bars("px").is_some_and(|b| b > 0) && bars("score").is_some_and(|b| b > 0), "{:?}", planned);
            assert_eq!(bars("nothing"), Some(0), "a division by zero on every row declines every bar: {:?}", planned);
            let ledger = &set.as_ref().unwrap().1;
            assert!(ledger["score"].len() > 100 && ledger["nothing"].is_empty(), "{} score rows", ledger["score"].len());
            // N = 3 * 2 - 1 + 3 is shown as a Count; a number would be its bits.
            assert!(ledger["score"].iter().all(|row| row.split(' ').any(|c| c == "8")), "N is a Count: {}", ledger["score"][0]);
        }
    }
}

/// Each corpus program and book under each evaluator, timed. Ignored by
/// default: run with
/// `cargo test -p abt-tests --release --test eval_set -- --ignored --nocapture`
/// and record what it prints in docs/assessment.md.
#[test]
#[ignore]
fn time_the_two_evaluators() {
    let mut total = [0.0f64; 3];
    println!("| program | tuple (s) | set (s) | set, 4 threads (s) |");
    println!("| --- | ---: | ---: | ---: |");
    for path in golden_programs() {
        let (p, ds) = golden_inputs(&path).unwrap();
        let mut secs = [0.0f64; 3];
        for (i, (set, threads)) in [(false, 1), (true, 1), (true, 4)].into_iter().enumerate() {
            let mut cfg = ExecConfig::from_program(&p);
            cfg.set_eval = set;
            cfg.threads = threads;
            // The faster of three runs.
            secs[i] = (0..3)
                .map(|_| {
                    let t = std::time::Instant::now();
                    let _ = abt_tests::run(&p, &ds, cfg.clone());
                    t.elapsed().as_secs_f64()
                })
                .fold(f64::INFINITY, f64::min);
            total[i] += secs[i];
        }
        println!("| {} | {:.3} | {:.3} | {:.3} |", p.strategy, secs[0], secs[1], secs[2]);
    }
    println!("| total | {:.3} | {:.3} | {:.3} |", total[0], total[1], total[2]);
}
