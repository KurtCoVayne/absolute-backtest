//! MW14 as the Python book runs it (stageanalyst `strategy/ivmom`), on the
//! synthetic market of the corpus programs:
//!
//! - the book trades from its first week, flat before it, with every row,
//!   window and latch warmed on the data before (`run_book`'s window over
//!   a panel built from the data's first week). A run with the stores
//!   trimmed reads no row before its first decision bar (E-135's path), so
//!   the run starts at the data's first week and the program holds its
//!   targets until `first_trade` (ERRATA E-146); it then decides and fills
//!   what a run of the whole history started at `first_trade` does;
//! - the target weight is the book's `(1 / N) * scale`, capped, to the bit
//!   (`engine_weighted.py`: the share `1 / N` of the latched names, times the
//!   scale, then the cap; E-148).

#![allow(clippy::result_large_err)]

use abt_kernel::check::Program;
use abt_kernel::kernel::time::{format_timestamp, parse_timestamp};
use abt_kernel::kernel::{Dataset, ExecConfig, Kernel, RunResult, Value};
use abt_kernel::Lit;
use abt_tests as common;

fn mw14() -> Program {
    common::program(&common::corpus_source("company", "mw14"), "mw14")
}

/// Forty names over 140 weeks from 2010: the gate's 52-week mean exists
/// from the second year, and the book latches up to a few dozen names.
fn market(p: &Program) -> Dataset {
    let names: Vec<String> = (0..40).map(|i| format!("S{:02}", i)).collect();
    let syms: Vec<&str> = names.iter().map(String::as_str).collect();
    abt_kernel::data::synthetic_for_program(p, &syms, (2010, 1, 4), 700, 5)
}

/// Set the strategy's parameter `name`; whether the program has it.
fn set_param(p: &mut Program, name: &str, value: Lit) -> bool {
    let mut found = false;
    for params in p.params.values_mut() {
        if let Some(x) = params.get_mut(name) {
            x.value = value.clone();
            found = true;
        }
    }
    found
}

fn fills(r: &RunResult, from: i64) -> Vec<String> {
    r.fills
        .iter()
        .filter(|f| f.t >= from)
        .map(|f| {
            format!(
                "{} {} {:x} {:x} {:x}",
                format_timestamp(f.t),
                r.symbols[f.equity as usize],
                f.quantity.to_bits(),
                f.price.to_bits(),
                f.commission.to_bits()
            )
        })
        .collect()
}

fn decisions(r: &RunResult, from: i64) -> Vec<String> {
    r.decisions
        .iter()
        .filter(|d| d.t >= from)
        .map(|d| format!("{} {} {}", format_timestamp(d.t), r.describe_decision(&d.decision), d.rule))
        .collect()
}

#[test]
fn a_run_from_the_datas_first_week_trades_from_the_books_first_week_as_the_whole_history_does() {
    let mut p = mw14();
    let ds = market(&p);
    // The book's first week here: a year and a half in, after the gate's
    // first year, so the rows, the latch and the gate are warm.
    let first = parse_timestamp("2011-07-01").unwrap();
    let guarded = set_param(&mut p, "first_trade", Lit::Time(first));
    // As `abt run` runs it (`--history window`), from the data's first week.
    let from_start = ExecConfig {
        trim_stores: true,
        start: None,
        ..ExecConfig::from_program(&p)
    };
    let r = common::run(&p, &ds, from_start).unwrap();
    let early: Vec<String> = r
        .fills
        .iter()
        .filter(|f| f.t < first)
        .map(|f| format!("{} {}", format_timestamp(f.t), r.symbols[f.equity as usize]))
        .collect();
    assert!(early.is_empty(), "the book traded before its first week ({} fills, first {:?})", early.len(), early.first());
    assert!(r.fills.iter().any(|f| f.t == first), "the book buys its latched names in its first week");
    assert!(guarded, "mw14 has no `first_trade`");
    // The spec's reading of a first decision bar (`ExecConfig::start`: the
    // bars before it remain data): the whole history, from `first_trade`.
    let whole = ExecConfig {
        trim_stores: false,
        start: Some(first),
        ..ExecConfig::from_program(&p)
    };
    let w = common::run(&p, &ds, whole).unwrap();
    assert!(fills(&w, first).len() > 20, "{} fills", fills(&w, first).len());
    assert_eq!(decisions(&r, first), decisions(&w, first));
    assert_eq!(fills(&r, first), fills(&w, first));
}

/// The scale, the count of the names in the book and the targets at `t`.
fn week(k: &mut Kernel, t: i64) -> (Option<f64>, usize, Vec<f64>) {
    let num = |tu: &[Value]| tu.iter().rev().find_map(|v| if let Value::Num(x) = v { Some(*x) } else { None });
    let scale = k.query("scale", t, &[]).unwrap().first().and_then(|tu| num(tu));
    let n = k.query("in_book", t, &[]).unwrap().len();
    let targets = k.query("target", t, &[]).unwrap().iter().filter_map(|tu| num(tu)).collect();
    (scale, n, targets)
}

#[test]
fn the_target_is_one_over_the_count_times_the_scale_capped() {
    let mut p = mw14();
    let ds = market(&p);
    set_param(&mut p, "first_trade", Lit::Time(i64::MIN));
    // Every latched name counts as extended, so every week with a latched
    // name is crowded and its scale is the dial's 0.7 where the gate or the
    // calm test holds: the scale at which S / N and (1 / N) S part.
    assert!(set_param(&mut p, "crowd_ext", Lit::Float(-1.0)));
    let (r, mut k) = common::run_kernel(&p, &ds, ExecConfig::from_program(&p)).unwrap();
    let (mut checked, mut parted) = (0, 0);
    for &t in &r.bars {
        let (scale, n, targets) = week(&mut k, t);
        let Some(s) = scale else { continue };
        for w in targets {
            let book = (1.0 / n as f64 * s).min(0.15);
            assert_eq!(w.to_bits(), book.to_bits(), "{}: {} names at scale {}: {} against the book's {}", format_timestamp(t), n, s, w, book);
            checked += 1;
            if s / n as f64 != 1.0 / n as f64 * s {
                parted += 1;
            }
        }
    }
    assert!(checked > 100, "{} targets", checked);
    assert!(parted > 0, "no week where S / N and (1 / N) S part: the test proves nothing");
}

/// The names `eligible` holds at the label `day`: `sp1500` read with the
/// name bound, as the program reads it (unbound, a bucket read answers the
/// bucket's latest day with any row, here the label's).
fn members_at(k: &mut Kernel, day: &str) -> Vec<String> {
    let t = parse_timestamp(day).unwrap();
    let mut out: Vec<String> = k
        .query("eligible", t, &[])
        .unwrap()
        .iter()
        .filter_map(|tu| tu.iter().find(|v| matches!(v, Value::Equity(_))).map(|v| k.show(v)))
        .collect();
    out.sort();
    out
}

/// D5 of the Nov 25 parity measurements: the book counts a name a member in
/// a week when `member_from <= its own last bar of the week <= member_to`
/// (data.py `add_membership_and_universe`). MW14 read `member` at the week's
/// label: `sp1500` is derived for every name of the week at once, and a
/// bucket read with the name unbound answers the bucket's last day with any
/// row, the label's. So a member with no bar on the label day (a halt, a
/// delisting during the week) was out of the book's universe that week. Here S03 (`E4`) leaves on Wednesday 2011-03-09 and trades
/// on, S04 (`E5`) joins on that Wednesday, and S05 (`E6`), a member, has no
/// bar after Tuesday that week.
/// (The lake's `member_to` is the first day out; the adapter's `member_to =
/// last_day` gives the book's inclusive reading, ERRATA E-149.)
#[test]
fn a_name_is_a_member_in_a_week_when_it_is_one_at_its_last_bar_of_the_week() {
    let p = mw14();
    let mut ds = market(&p);
    let (s03, s04, s05) = (ds.intern("E4"), ds.intern("E5"), ds.intern("E6"));
    let wednesday = parse_timestamp("2011-03-09").unwrap();
    let friday = parse_timestamp("2011-03-11").unwrap();
    let at = |tu: &abt_kernel::kernel::Tuple, s| tu[0] == Value::Equity(s);
    let time = |tu: &abt_kernel::kernel::Tuple| match tu[1] {
        Value::Time(t) => t,
        _ => i64::MIN,
    };
    for (rel, tuples) in ds.facts.iter_mut() {
        match rel.as_str() {
            // The adapter writes a member row on a day the name has a bar.
            "member" => tuples.retain(|tu| !(at(tu, s03) && time(tu) >= wednesday) && !(at(tu, s04) && time(tu) < wednesday) && !(at(tu, s05) && time(tu) >= wednesday && time(tu) <= friday)),
            "open" | "high" | "low" | "close" | "volume" | "listed" => tuples.retain(|tu| !(at(tu, s05) && time(tu) >= wednesday && time(tu) <= friday)),
            _ => {}
        }
    }
    let (_, mut k) = common::run_kernel(&p, &ds, ExecConfig::from_program(&p)).unwrap();
    let before = members_at(&mut k, "2011-03-04");
    let week = members_at(&mut k, "2011-03-11");
    let after = members_at(&mut k, "2011-03-18");
    let has = |v: &[String], s: &str| v.iter().any(|x| x == s);
    assert!(has(&before, "E4") && !has(&before, "E5") && has(&before, "E6"), "{:?}", before);
    assert!(!has(&week, "E4"), "S03 left on Wednesday and closed the week out of the index: {:?}", week);
    assert!(has(&week, "E5"), "S04 joined on Wednesday: {:?}", week);
    assert!(has(&week, "E6"), "S05's last bar of the week, Tuesday, is a member day: {:?}", week);
    assert!(!has(&after, "E4") && has(&after, "E5") && has(&after, "E6"), "{:?}", after);
}
