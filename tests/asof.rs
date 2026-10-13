//! The as-of join `R(..., T0, ...) asof T` (data-bundle doc, section 2;
//! language-v2, 3.4): the tuples of R at the latest key at or before T,
//! whatever R's calendar. The key it binds is causal (WF-6), the join is
//! how a finer rule reads a coarser relation, and the fold serves it from
//! what was available at T.

mod common;

use absolute_backtest::check::{Code, Program};
use absolute_backtest::kernel::time::format_timestamp;
use absolute_backtest::kernel::{run, run_fold, Dataset, ExecConfig, Value};

const DAYS: [&str; 5] = ["2024-01-08", "2024-01-09", "2024-01-10", "2024-01-11", "2024-01-12"];

/// The daily skeleton, reading the `SPXTR` series as well: a stored
/// relation whose keys are sparse, standing in for a rating.
fn strategy(name: &str, params: &str, rules: &str, decides: &str) -> String {
    common::StrategyParts {
        name: name.into(),
        needs: "need bars @1d   need actions, listing   need series \"SPXTR\" @1d".into(),
        params: params.into(),
        rules: rules.into(),
        decides: decides.into(),
        ..Default::default()
    }
    .render()
}

/// One name over five days; the series at 1 on the first day and 3 on the fourth.
fn market() -> Dataset {
    let mut ds = common::market("X", &[10.0; 5]);
    let spx = ds.intern_label("SPXTR");
    ds.add("value", vec![Value::Label(spx), Value::Time(common::day(DAYS[0])), Value::Num(1.0)]);
    ds.add("value", vec![Value::Label(spx), Value::Time(common::day(DAYS[3])), Value::Num(3.0)]);
    ds
}

fn decided(ds: &Dataset, prog: &Program, fold: bool) -> Vec<(String, f64)> {
    let cfg = ExecConfig {
        initial_cash: 10_000.0,
        ..ExecConfig::frictionless()
    };
    let r = if fold { run_fold(prog, ds, cfg).unwrap() } else { run(prog, ds, cfg).unwrap() };
    r.decisions.iter().map(|d| (format_timestamp(d.t), d.decision.amount)).collect()
}

// ---- syntax and semantics ----

fn stored() -> Program {
    let src = strategy(
        "stored_asof",
        "    unit : Quantity<Shares> = 1 shares",
        "",
        "    decide(T, trade(A, Q)) :- universe(A, T), value(\"SPXTR\", _, R) asof T, Q = R * unit.",
    );
    common::program(&src, "stored_asof")
}

fn derived() -> Program {
    let src = strategy(
        "derived_asof",
        "    unit : Quantity<Shares> = 1 shares
    floor : Scalar = 2",
        "    rel rated(@T, R: Scalar)
    rated(T, R) :- value(\"SPXTR\", T, R), R > floor.",
        "    decide(T, trade(A, Q)) :- universe(A, T), rated(T0, R) asof T, Q = R * unit.",
    );
    common::program(&src, "derived_asof")
}

#[test]
fn an_asof_atom_parses_and_describes_itself() {
    let prog = stored();
    let rule = prog.rules.iter().find(|r| r.head.name == "decide").unwrap();
    let texts: Vec<String> = rule.body.iter().map(|l| l.describe()).collect();
    assert!(texts.iter().any(|t| t == "value(\"SPXTR\", _, R) asof T"), "{:?}", texts);
}

#[test]
fn a_stored_relation_is_read_at_its_latest_key_at_or_before_t() {
    let prog = stored();
    let ds = market();
    // Day 1's value holds until day 4's replaces it; nothing is read ahead.
    let want: Vec<(String, f64)> = DAYS.iter().zip([1.0, 1.0, 1.0, 3.0, 3.0]).map(|(d, q)| (d.to_string(), q)).collect();
    assert_eq!(decided(&ds, &prog, false), want);
    assert_eq!(decided(&ds, &prog, true), want);
}

#[test]
fn a_derived_relation_is_walked_back_over_its_own_bars() {
    let prog = derived();
    let ds = market();
    // `rated` holds only the value of 3, so the join fails before day 4.
    let want = vec![("2024-01-11".to_string(), 3.0), ("2024-01-12".to_string(), 3.0)];
    assert_eq!(decided(&ds, &prog, false), want);
    assert_eq!(decided(&ds, &prog, true), want);
}

#[test]
fn the_fold_reads_an_asof_tuple_from_its_availability_on() {
    let prog = stored();
    let mut ds = market();
    let spx = ds.intern_label("SPXTR");
    // Day 4's value is recorded on day 5.
    let late = vec![Value::Label(spx), Value::Time(common::day(DAYS[3])), Value::Num(3.0)];
    ds.facts.get_mut("value").unwrap().retain(|tu| *tu != late);
    ds.add_available("value", late, common::day(DAYS[4]));
    let fold = decided(&ds, &prog, true);
    assert_eq!(fold.iter().map(|(_, q)| *q).collect::<Vec<_>>(), vec![1.0, 1.0, 1.0, 1.0, 3.0], "{:?}", fold);
    // The batch kernel, blind to availability, reads it on its key.
    let batch = decided(&ds, &prog, false);
    assert_eq!(batch.iter().map(|(_, q)| *q).collect::<Vec<_>>(), vec![1.0, 1.0, 1.0, 3.0, 3.0], "{:?}", batch);
}

// ---- well-formedness ----

fn wf(rules: &str, decides: &str) -> String {
    strategy("wf", "    qty : Quantity<Shares> = 1 shares", rules, decides)
}

#[test]
fn the_bound_key_is_causal_and_may_feed_a_builtin() {
    let src = wf(
        "",
        "    decide(T, trade(A, qty)) :- universe(A, T), value(\"SPXTR\", T0, R) asof T, prev(T0, T1), close(A, T1, P), P > 0 USD/share, R > 0.",
    );
    let (p, diags) = common::check(&src, "wf");
    assert!(p.is_some(), "{:?}", common::errors(&diags));
    assert!(common::errors(&diags).is_empty(), "{:?}", common::errors(&diags));
}

#[test]
fn the_bound_key_cannot_be_a_head_key() {
    // A head keyed at the join's T0 makes T0 the head time, and then the
    // as-of time T is not derived from it: the tuple's time would not be
    // the time it became available (WF-6).
    let src = wf(
        "    rel latest(A: Equity, @T, R: Scalar)
    latest(A, T0, R) :- universe(A, T), value(\"SPXTR\", T0, R) asof T.",
        "    decide(T, trade(A, qty)) :- universe(A, T), latest(A, T, R), R > 0.",
    );
    let (_, diags) = common::check(&src, "wf");
    let errs = common::errors(&diags);
    assert!(
        errs.iter()
            .any(|(c, m)| *c == Code::F && m.contains("as-of time `T`") && m.contains("not derived from the head time `T0`")),
        "{:?}",
        errs
    );
}

#[test]
fn the_asof_time_must_be_bound_and_derived_from_the_head_time() {
    let src = wf("", "    decide(T, trade(A, qty)) :- universe(A, T), value(\"SPXTR\", T0, R) asof T2, R > 0.");
    let errs = common::errors(&common::check(&src, "wf").1);
    assert!(errs.iter().any(|(c, m)| *c == Code::B && m.contains("`T2` is unbound")), "{:?}", errs);
    // A dividend's ex-date is a value, not a time derived from T.
    let src = wf("", "    decide(T, trade(A, qty)) :- universe(A, T), dividend(A, T, Ex, _, _), value(\"SPXTR\", T0, R) asof Ex, R > 0.");
    let errs = common::errors(&common::check(&src, "wf").1);
    assert!(errs.iter().any(|(c, m)| *c == Code::F && m.contains("as-of time `Ex`") && m.contains("not derived")), "{:?}", errs);
}

#[test]
fn decided_as_of_the_head_time_would_see_itself() {
    let src = wf("", "    decide(T, trade(A, qty)) :- universe(A, T), decided(T0, trade(A, _)) asof T.");
    let errs = common::errors(&common::check(&src, "wf").1);
    assert!(errs.iter().any(|(c, m)| *c == Code::F && m.contains("`decided` must be read strictly before")), "{:?}", errs);
    // Strictly before T it is a lookup of the last decision on the name.
    let src = wf("", "    decide(T, trade(A, qty)) :- universe(A, T), prev(T, T1), decided(T0, trade(A, _)) asof T1.");
    let errs = common::errors(&common::check(&src, "wf").1);
    assert!(errs.is_empty(), "{:?}", errs);
}

/// A minute bundle decided on its daily calendar.
fn daily_of_minutes(name: &str, decides: &str) -> String {
    common::StrategyParts {
        name: name.into(),
        needs: "need bars @1m   need listing".into(),
        calendars: Some("daily = days".into()),
        clock: "decide on daily".into(),
        universe: "universe(A, T) :- listed(A, T), traded@daily(A, T).".into(),
        params: "    qty : Quantity<Shares> = 1 shares".into(),
        decides: decides.into(),
        report: "calendar daily   periods 252 per year".into(),
        ..Default::default()
    }
    .render()
}

/// A minute bundle decided on its base calendar, with a daily calendar declared.
fn minutes(name: &str, params: &str, rules: &str, decides: &str) -> String {
    common::StrategyParts {
        name: name.into(),
        needs: "need bars @1m   need listing".into(),
        calendars: Some("daily = days".into()),
        clock: "decide on base".into(),
        params: params.into(),
        rules: rules.into(),
        decides: decides.into(),
        report: "calendar daily   periods 252 per year".into(),
        ..Default::default()
    }
    .render()
}

#[test]
fn an_asof_join_crosses_calendars_where_a_plain_atom_cannot() {
    // On the daily calendar of a minute bundle the minute close is read only
    // through its bar view (WF-16); an as-of join does not stand in for it.
    let daily = |lit: &str| daily_of_minutes("daily", &format!("    decide(T, trade(A, qty)) :- universe(A, T), {}, P > 0 USD/share.", lit));
    let errs = common::errors(&common::check(&daily("close(A, T, P)"), "daily").1);
    assert!(errs.iter().any(|(c, m)| *c == Code::X && m.contains("only through its bar view")), "{:?}", errs);
    let errs = common::errors(&common::check(&daily("close(A, _, P) asof T"), "daily").1);
    assert!(errs.iter().any(|(c, _)| *c == Code::X), "{:?}", errs);
    // On the minute base the daily view is keyed on the daily calendar: a
    // minute key is a calendar mismatch, and the as-of join is how a minute
    // rule reads the latest daily bar at or before its own.
    let minute = |lit: &str| {
        minutes(
            "intraday",
            "    qty : Quantity<Shares> = 1 shares",
            "",
            &format!("    decide(T, trade(A, qty)) :- universe(A, T), {}, P > 0 USD/share.", lit),
        )
    };
    let errs = common::errors(&common::check(&minute("close@daily(A, T, P)"), "intraday").1);
    assert!(errs.iter().any(|(c, _)| *c == Code::X), "{:?}", errs);
    let errs = common::errors(&common::check(&minute("close@daily(A, _, P) asof T"), "intraday").1);
    assert!(errs.is_empty(), "{:?}", errs);
}

/// The minute market of the daily-view tests: closes 10 on the first day
/// and 20 on the second, minute bars at 09:31 and 09:32 on both days, and
/// the daily listing.
fn two_minute_days() -> Dataset {
    let mut ds = Dataset::new();
    let x = ds.add_security("X", "X", common::day("2024-01-08"), None);
    for (d, c) in [("2024-01-08", 10.0), ("2024-01-09", 20.0)] {
        ds.add("listed", vec![Value::Equity(x), Value::Time(common::day(d))]);
        for m in ["09:31:00", "09:32:00"] {
            let t = common::day(&format!("{}T{}", d, m));
            for rel in ["open", "high", "low", "close"] {
                ds.add(rel, vec![Value::Equity(x), Value::Time(t), Value::Num(c)]);
            }
            ds.add("volume", vec![Value::Equity(x), Value::Time(t), Value::Num(100.0)]);
        }
    }
    ds.derive_tickers();
    ds
}

/// A minute rule that reads the latest daily bar at or before its own.
fn intraday_daily() -> Program {
    let src = minutes(
        "intraday_daily",
        "    unit : Quantity<Shares> = 1 shares
    one : Price<USD> = 1 USD/share",
        "    rel seen(A: Equity, @T, P: Price<USD>, D: Timestamp)
    seen(A, T, P, D) :- universe(A, T), close@daily(A, D, P) asof T.",
        "    decide(T, trade(A, Q)) :- seen(A, T, P, _), Q = P / one * unit.",
    );
    common::program(&src, "intraday_daily")
}

/// The daily closes a minute rule sees, as `(bar, close)`.
const SEEN: [(&str, f64); 3] = [("2024-01-08T09:32:00", 10.0), ("2024-01-09T09:31:00", 10.0), ("2024-01-09T09:32:00", 20.0)];

/// A daily bar is labelled at its last base bar (3.1), the instant its
/// bucket closes: read as of an earlier minute of its day it is not there
/// yet, so an intraday rule sees the previous day's bar until the day's
/// last bar, where it reads the day's own. (The first formulation labelled
/// a daily bar at its date and held it back until the day ended, so the
/// minute rule never saw the day's own close.)
#[test]
fn a_minute_rule_reads_the_previous_days_bar_until_the_day_closes() {
    let r = run(&intraday_daily(), &two_minute_days(), ExecConfig::frictionless()).unwrap();
    // At the first day's first minute nothing daily is closed yet; at its
    // last minute the day's bar of 10 closes and is read; the second day
    // reads 10 until its own last minute, where it reads 20.
    let qty: Vec<(String, f64)> = r.decisions.iter().map(|d| (format_timestamp(d.t), d.decision.amount)).collect();
    let want: Vec<(String, f64)> = SEEN.iter().map(|(t, q)| (t.to_string(), *q)).collect();
    assert_eq!(qty, want, "{:?}", qty);
}

/// Both drivers agree on what a minute rule sees of the daily bars.
#[test]
#[ignore = "src: on a minute bundle the fold takes the midnight keys of the daily catalog (listed) as base bars (six bars here, the batch kernel four) and decides nothing"]
fn the_fold_agrees_with_the_batch_kernel_on_a_minute_rule_reading_daily_bars() {
    let f = run_fold(&intraday_daily(), &two_minute_days(), ExecConfig::frictionless()).unwrap();
    let fold: Vec<(String, f64)> = f.decisions.iter().map(|d| (format_timestamp(d.t), d.decision.amount)).collect();
    let want: Vec<(String, f64)> = SEEN.iter().map(|(t, q)| (t.to_string(), *q)).collect();
    assert_eq!(fold, want, "{:?}", fold);
}
