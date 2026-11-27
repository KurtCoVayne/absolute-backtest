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

use std::collections::{BTreeMap, BTreeSet};

use abt_kernel::check::Program;
use abt_kernel::compiler::{compile, CompileOptions};
use abt_kernel::kernel::time::{format_timestamp, parse_timestamp};
use abt_kernel::kernel::{Dataset, ExecConfig, Kernel, RunResult, Sym, Value};
use abt_kernel::lower::Numerics;
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

/// The names `eligible` holds at the label `day`.
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

/// The names `eligible` holds at the label `t`.
fn eligible_at(k: &mut Kernel, t: i64) -> BTreeSet<String> {
    k.query("eligible", t, &[])
        .unwrap()
        .iter()
        .filter_map(|tu| tu.iter().find(|v| matches!(v, Value::Equity(_))).map(|v| k.show(v)))
        .collect()
}

/// A name's last bar of the week before the label (E-149, E-160): in the
/// week of Friday 2011-03-11, S06 to S09 (`E7` to `E10`) have no bar on
/// Thursday or Friday. S06's membership ends on Wednesday, S07's on
/// Thursday and S08's on Friday, their rows on the days without a bar kept
/// as a lake interval holds them; S09's ends on Tuesday. The book's
/// `week_end` for each is Wednesday: the first three are members that week,
/// the fourth is not, and so is S03 (`E4`), out on Wednesday and trading
/// to Friday.
#[test]
fn a_name_whose_last_bar_of_the_week_is_wednesday_is_a_member_by_its_membership_on_wednesday() {
    let p = mw14();
    let mut ds = market(&p);
    let names: Vec<_> = ["E4", "E7", "E8", "E9", "E10"].iter().map(|s| ds.intern(s)).collect();
    let (s03, s06, s07, s08, s09) = (names[0], names[1], names[2], names[3], names[4]);
    let day = |d: &str| parse_timestamp(d).unwrap();
    let (wednesday, thursday, friday) = (day("2011-03-09"), day("2011-03-10"), day("2011-03-11"));
    let time = |tu: &abt_kernel::kernel::Tuple| tu[1].as_time().unwrap_or(i64::MIN);
    let of = |tu: &abt_kernel::kernel::Tuple, s| tu[0] == Value::Equity(s);
    let halted = |tu: &abt_kernel::kernel::Tuple| [s06, s07, s08, s09].iter().any(|&s| of(tu, s)) && (thursday..=friday).contains(&time(tu));
    for (rel, tuples) in ds.facts.iter_mut() {
        match rel.as_str() {
            "member" => tuples.retain(|tu| {
                let t = time(tu);
                !(of(tu, s03) && t >= wednesday) && !(of(tu, s06) && (thursday..=friday).contains(&t)) && !(of(tu, s07) && t == friday) && !(of(tu, s09) && (wednesday..=friday).contains(&t))
            }),
            "open" | "high" | "low" | "close" | "volume" | "listed" => tuples.retain(|tu| !halted(tu)),
            _ => {}
        }
    }
    let (_, mut k) = common::run_kernel(&p, &ds, ExecConfig::from_program(&p)).unwrap();
    let before = eligible_at(&mut k, day("2011-03-04"));
    let week = eligible_at(&mut k, friday);
    for s in ["E4", "E7", "E8", "E9", "E10"] {
        assert!(before.contains(s), "{} is a member the week before: {:?}", s, before);
    }
    assert!(week.contains("E7"), "S06's membership ends on its last bar of the week, Wednesday: {:?}", week);
    assert!(week.contains("E8"), "S07's membership ends on Thursday, after its last bar of the week: {:?}", week);
    assert!(week.contains("E9"), "S08's membership ends on Friday, after its last bar of the week: {:?}", week);
    assert!(!week.contains("E10"), "S09's membership ended on Tuesday, before its last bar of the week: {:?}", week);
    assert!(!week.contains("E4"), "S03 left on Wednesday and closed the week out of the index: {:?}", week);
}

/// MW14 without its membership test: `eligible` holds the names above the
/// price and dollar-volume floors (its rule cut after the second floor).
fn floors_only() -> Program {
    let src = common::corpus_source("company", "mw14");
    let floor = "D >= dvol_floor";
    let at = src.find(floor).expect("mw14's `eligible` has its dollar-volume floor") + floor.len();
    let end = at + src[at..].find(".\n").expect("the rule ends");
    let cut = format!("{}{}", &src[..at], &src[end..]);
    let rule = &cut[cut.find("eligible(A, T, D) :-").expect("the rule")..at];
    assert!(!rule.contains("sp1500") && !rule.contains("member"), "the floors' rule still tests membership: {}", rule);
    common::program(&cut, "mw14")
}

/// The book's membership test (data.py `add_membership_and_universe`) over a
/// market where every name halts now and then, for one to four days, with or
/// without member rows on the halted days, and moves between the three
/// lists, into two at once and out of all of them, on every weekday. At every
/// week `eligible` holds exactly the names above the floors that are members
/// at their own last bar of it, as computed here from the facts; a read at
/// the label and a read of the week's latest member row each miss some of
/// those weeks. The run's decisions and fills are also those of the
/// optimised plan over trimmed stores, as `abt run` runs it.
#[test]
fn eligible_holds_the_names_above_the_floors_that_are_members_at_their_own_last_bar_of_the_week() {
    let p = mw14();
    let mut ds = market(&p);
    let lists = [ds.intern_label("SP500"), ds.intern_label("SP400"), ds.intern_label("SP600")];
    let mut state: u64 = 0x2545_f491_4f6c_dd1d;
    let mut draw = |n: usize| -> usize {
        state = state.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1_442_695_040_888_963_407);
        ((state >> 33) % n as u64) as usize
    };
    // Every name's bar days, in order.
    let mut days: BTreeMap<Sym, Vec<i64>> = BTreeMap::new();
    for tu in &ds.facts["close"] {
        if let (Some(s), Some(t)) = (tu[0].as_equity(), tu[1].as_time()) {
            days.entry(s).or_default().push(t);
        }
    }
    let mut halted: BTreeSet<(Sym, i64)> = BTreeSet::new();
    let mut member: Vec<(i64, Sym, Sym)> = Vec::new();
    for (&s, d) in days.iter_mut() {
        d.sort_unstable();
        for _ in 0..10 {
            let (from, len) = (draw(d.len()), 1 + draw(4));
            halted.extend(d[from..(from + len).min(d.len())].iter().map(|&t| (s, t)));
        }
        let mut cuts: Vec<usize> = (0..14).map(|_| draw(d.len())).collect();
        cuts.sort_unstable();
        let (mut seg, mut held) = (0, draw(5));
        for (i, &t) in d.iter().enumerate() {
            while seg < cuts.len() && cuts[seg] <= i {
                seg += 1;
                held = draw(5);
            }
            if halted.contains(&(s, t)) && draw(2) == 0 {
                continue;
            }
            // 0: out of the three; 1 to 3: one list; 4: the S&P 500 and the 400 at once.
            match held {
                0 => {}
                4 => member.extend([(t, s, lists[0]), (t, s, lists[1])]),
                k => member.push((t, s, lists[k - 1])),
            }
        }
    }
    for (rel, tuples) in ds.facts.iter_mut() {
        match rel.as_str() {
            "member" => tuples.retain(|tu| !matches!(tu[2], Value::Label(l) if lists.contains(&l))),
            "open" | "high" | "low" | "close" | "volume" | "listed" => tuples.retain(|tu| !matches!((tu[0].as_equity(), tu[1].as_time()), (Some(s), Some(t)) if halted.contains(&(s, t)))),
            _ => {}
        }
    }
    member.sort_unstable();
    for (t, s, l) in &member {
        ds.add("member", vec![Value::Equity(*s), Value::Time(*t), Value::Label(*l)]);
    }
    let members: BTreeSet<(Sym, i64)> = member.iter().map(|&(t, s, _)| (s, t)).collect();
    let (r, mut k) = common::run_kernel(&p, &ds, ExecConfig::from_program(&p)).unwrap();
    let floors = floors_only();
    let (_, mut kf) = common::run_kernel(&floors, &ds, ExecConfig::from_program(&floors)).unwrap();
    let (mut weeks, mut names, mut at_label, mut latest) = (0, 0, 0, 0);
    let mut from = i64::MIN;
    for &t in &r.bars {
        let above = eligible_at(&mut kf, t);
        let mut book: BTreeSet<String> = BTreeSet::new();
        for (&s, d) in &days {
            let name = ds.symbols.name(s).to_string();
            let traded: Vec<i64> = d.iter().copied().filter(|&x| x > from && x <= t && !halted.contains(&(s, x))).collect();
            let Some(&last) = traded.last() else { continue };
            if !above.contains(&name) {
                continue;
            }
            let is = members.contains(&(s, last));
            at_label += usize::from(is != members.contains(&(s, t)));
            latest += usize::from(is != traded.iter().any(|&x| members.contains(&(s, x))));
            if is {
                book.insert(name);
            }
        }
        names += book.len();
        assert_eq!(eligible_at(&mut k, t), book, "{}: `eligible` against the floors and the book's membership test", format_timestamp(t));
        weeks += 1;
        from = t;
    }
    assert!(weeks > 100 && names > 1000, "{} weeks, {} eligible name-weeks", weeks, names);
    assert!(
        at_label > 0 && latest > 0,
        "the market has no week where a read at the label ({}) or of the week's latest member row ({}) parts from the book",
        at_label,
        latest
    );
    let cfg = ExecConfig::from_program(&p);
    let numerics = Numerics::from_running_sums(cfg.running_sums);
    let compiled = compile(
        &p,
        &CompileOptions {
            numerics,
            ..CompileOptions::default()
        },
    );
    let trimmed = common::run_planned(&p, &compiled.lowered, &ds, ExecConfig { trim_stores: true, ..cfg }).unwrap();
    assert!(fills(&r, i64::MIN).len() > 20, "{} fills", fills(&r, i64::MIN).len());
    assert_eq!(decisions(&r, i64::MIN), decisions(&trimmed, i64::MIN));
    assert_eq!(fills(&r, i64::MIN), fills(&trimmed, i64::MIN));
}

/// The names of `rel` at `t`.
fn names_at(k: &mut Kernel, rel: &str, t: i64) -> BTreeSet<String> {
    k.query(rel, t, &[])
        .unwrap()
        .iter()
        .filter_map(|tu| tu.iter().find(|v| matches!(v, Value::Equity(_))).map(|v| k.show(v)))
        .collect()
}

/// `name` prints for the last time at `last` and is delisted from the next
/// trading day on, a row a day, as the Norgate adapter writes it (its bars,
/// its listing, its membership and its classification stop).
fn delist_after(ds: &mut Dataset, name: &str, last: i64, days: &BTreeSet<i64>) {
    let s = ds.intern(name);
    let reason = ds.intern_label("acquisition");
    let after = |tu: &abt_kernel::kernel::Tuple| tu[0] == Value::Equity(s) && tu[1].as_time().is_some_and(|t| t > last);
    for (rel, tuples) in ds.facts.iter_mut() {
        if matches!(rel.as_str(), "open" | "high" | "low" | "close" | "volume" | "listed" | "member" | "classification") {
            tuples.retain(|tu| !after(tu));
        }
    }
    for &t in days.range(last + 1..) {
        ds.add("delisted", vec![Value::Equity(s), Value::Time(t), Value::Label(reason)]);
    }
}

/// The book's delisting (engine_weighted.py, `simulate_weighted`: "drop
/// names with no row this week (delist) at zero cost"): a name trades
/// normally at its last row, the week whose bucket holds its last print,
/// and leaves the book at no cost the week after. X, held and in the target,
/// prints for the last time on the Wednesday of its week and Y, not held, is
/// bought in its last week, which also ends on a Wednesday; each is delisted
/// from the Thursday. The executor applied the delisting at the bar of the
/// week holding the delisting session: X was closed there, before the
/// week's decisions, and bought back at the same close at 1/N; Y was not
/// held when the delisting came and was bought after it, so the delisting
/// was never applied. Neither could be sold again, and each week's exit
/// expired (the Nov 25 residual: 105 names, 83,830 expired orders).
#[test]
fn a_name_trades_at_its_last_row_and_leaves_the_book_at_no_cost_at_the_next_week() {
    let p = mw14();
    let base = market(&p);
    let cfg = ExecConfig::from_program(&p);
    let (r0, mut k0) = common::run_kernel(&p, &base, cfg.clone()).unwrap();
    let bars = r0.bars.clone();
    let days: BTreeSet<i64> = base.facts["close"].iter().filter_map(|tu| tu[1].as_time()).collect();
    // The names with an action of their own (a dividend, a split and a
    // rename, the synthetic delisting) are left alone.
    let plain = |s: &String| !["E1", "E2", "E40"].contains(&s.as_str());
    let wednesday = |i: usize| *days.range(bars[i - 1] + 1..=bars[i]).nth(2).unwrap();
    let (mut held_in, mut bought_in) = (Vec::new(), Vec::new());
    for i in 60..bars.len() - 4 {
        let (before, target) = (names_at(&mut k0, "held", bars[i - 1]), names_at(&mut k0, "target", bars[i]));
        held_in.extend(before.intersection(&target).filter(|s| plain(s)).map(|s| (s.clone(), i)));
        bought_in.extend(target.difference(&before).filter(|s| plain(s)).map(|s| (s.clone(), i)));
    }
    let mut checked = false;
    for ((x, i), (y, j)) in held_in.iter().step_by(7).zip(bought_in.iter().step_by(5)).take(12) {
        let (i, j) = (*i, *j);
        if x == y {
            continue;
        }
        let mut ds = base.clone();
        delist_after(&mut ds, x, wednesday(i), &days);
        delist_after(&mut ds, y, wednesday(j), &days);
        let (r, mut k) = common::run_kernel(&p, &ds, cfg.clone()).unwrap();
        let of = |s: Sym, name: &str| r.symbols[s as usize] == name;
        // The cut moved the week's closes: the cases hold on this market.
        let bought = |name: &str, t: i64| r.fills.iter().any(|f| of(f.equity, name) && f.t == t && f.quantity > 0.0 && !f.forced);
        if !(names_at(&mut k, "held", bars[i - 1]).contains(x) && names_at(&mut k, "target", bars[i]).contains(x)) {
            continue;
        }
        if names_at(&mut k, "held", bars[j - 1]).contains(y) || !bought(y, bars[j]) {
            continue;
        }
        for (name, w, case) in [(x, i, "held"), (y, j, "bought")] {
            let at = |t: i64| format_timestamp(t);
            let forced: Vec<_> = r.fills.iter().filter(|f| of(f.equity, name) && f.forced).collect();
            let delisted: Vec<i64> = r.actions.iter().filter(|a| of(a.equity, name)).map(|a| a.t).collect();
            let story: Vec<String> = r
                .events
                .iter()
                .filter(|e| e.instrument.is_some_and(|s| of(s, name)) && e.t >= bars[w - 1] && e.t <= bars[w + 2])
                .map(|e| format!("{} {:?}", at(e.t), e.kind))
                .collect();
            if case == "held" {
                assert!(
                    names_at(&mut k, "held", bars[w]).contains(name),
                    "{} ({}): held at its last week {}, its last print {}: {:#?}",
                    name,
                    case,
                    at(bars[w]),
                    at(wednesday(w)),
                    story
                );
            }
            assert_eq!(delisted, vec![bars[w + 1]], "{} ({}): delisted the week after its last, {}: {:#?}", name, case, at(bars[w + 1]), story);
            assert_eq!(forced.len(), 1, "{} ({}): {:#?}", name, case, story);
            // Its price at its last week: its mark there, or the close it was bought at.
            let last = r
                .holdings
                .iter()
                .find(|h| of(h.instrument, name) && h.t == bars[w])
                .and_then(|h| h.mark)
                .or_else(|| r.fills.iter().find(|f| of(f.equity, name) && f.t == bars[w]).map(|f| f.price));
            assert_eq!(forced[0].t, bars[w + 1], "{} ({}): {:#?}", name, case, story);
            assert_eq!(Some(forced[0].price.to_bits()), last.map(f64::to_bits), "{} ({}): at its last price", name, case);
            assert_eq!(forced[0].commission, 0.0, "{} ({}): at no cost", name, case);
            assert!(
                r.decisions.iter().all(|d| !of(d.decision.equity, name) || d.t <= bars[w]),
                "{} ({}): nothing decided on it after its last week: {:#?}",
                name,
                case,
                story
            );
            assert!(
                r.holdings.iter().all(|h| !of(h.instrument, name) || h.t <= bars[w]),
                "{} ({}): not held after its last week",
                name,
                case
            );
            assert!(
                !r.events
                    .iter()
                    .any(|e| e.instrument.is_some_and(|s| of(s, name)) && matches!(e.kind, abt_kernel::kernel::EventKind::ExpiredOrder { .. })),
                "{} ({}): no exit waits for a print that never comes",
                name,
                case
            );
        }
        checked = true;
        break;
    }
    assert!(checked, "no held name in the target and no entering name on this market: the test proves nothing");
}
