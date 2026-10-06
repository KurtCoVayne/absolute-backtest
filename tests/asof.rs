//! The as-of join `R(..., T0, ...) asof T` (data-bundle doc, section 2):
//! the tuples of R at the latest key at or before T, whatever R's
//! resolution. The key it binds is causal (WF-6), the join is exempt from
//! WF-10, and the fold serves it from what was available at T.

mod corpus;

use absolute_backtest::check::{check_program, Code, Diagnostic, Program, Severity, Workspace};
use absolute_backtest::kernel::time::{format_timestamp, parse_timestamp};
use absolute_backtest::kernel::{run, run_fold, Dataset, ExecConfig, Value};

const ENV: &str = r#"
environment asof_1d {
  close(+A: Equity, @T: Timestamp, -P: Price<USD>) @1d
  volume(+A: Equity, @T: Timestamp, -V: Quantity<Shares>) @1d
  universe(-A: Equity, @T: Timestamp) @1d complete
  rating(+A: Equity, @T: Timestamp, -R: Scalar) @1d
  event(+A: Equity, @T: Timestamp, -When: Timestamp) @1d
  close_m(+A: Equity, @T: Timestamp, -P: Price<USD>) @1m
  universe_m(-A: Equity, @T: Timestamp) @1m complete
}
"#;

fn workspace() -> Workspace {
    let mut ws = corpus::base_workspace();
    ws.add_source(ENV).unwrap();
    ws
}

fn check(src: &str, name: &str) -> (Option<Program>, Vec<Diagnostic>) {
    let mut ws = workspace();
    ws.add_source(src).unwrap();
    check_program(&ws, name)
}

fn program(src: &str, name: &str) -> Program {
    let (p, diags) = check(src, name);
    let text: Vec<String> = diags.iter().map(|d| d.to_string()).collect();
    p.unwrap_or_else(|| panic!("{} should check:\n{}", name, text.join("\n")))
}

fn errors(diags: &[Diagnostic]) -> Vec<(Code, String)> {
    diags.iter().filter(|d| d.severity == Severity::Error).map(|d| (d.code, d.to_string())).collect()
}

fn day(s: &str) -> i64 {
    parse_timestamp(s).unwrap()
}

const DAYS: [&str; 5] = ["2024-01-08", "2024-01-09", "2024-01-10", "2024-01-11", "2024-01-12"];

/// One name over five days; a rating of 1 on the first day and 3 on the fourth.
fn market() -> Dataset {
    let mut ds = Dataset::new();
    let x = ds.intern("X");
    for (i, d) in DAYS.iter().enumerate() {
        let t = day(d);
        ds.add("close", vec![Value::Equity(x), Value::Time(t), Value::Num(10.0)]);
        ds.add("volume", vec![Value::Equity(x), Value::Time(t), Value::Num(1_000_000.0)]);
        ds.add("universe", vec![Value::Equity(x), Value::Time(t)]);
        if i == 0 {
            ds.add("rating", vec![Value::Equity(x), Value::Time(t), Value::Num(1.0)]);
        }
        if i == 3 {
            ds.add("rating", vec![Value::Equity(x), Value::Time(t), Value::Num(3.0)]);
        }
    }
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

const STORED: &str = r#"
strategy stored_asof {
  env asof_1d
  resolution @1d
  mode delta
  param unit : Quantity<Shares> = 1 shares
  decide(T, buy(A, Q)) :- universe(A, T), rating(A, _, R) asof T, Q = R * unit.
}
"#;

const DERIVED: &str = r#"
strategy derived_asof {
  env asof_1d
  resolution @1d
  mode delta
  param unit : Quantity<Shares> = 1 shares
  param floor : Scalar = 2
  rel rated(+A: Equity, @T: Timestamp, -R: Scalar)
  rated(A, T, R) :- rating(A, T, R), R > floor.
  decide(T, buy(A, Q)) :- universe(A, T), rated(A, T0, R) asof T, Q = R * unit.
}
"#;

#[test]
fn an_asof_atom_parses_and_describes_itself() {
    let prog = program(STORED, "stored_asof");
    let rule = prog.rules.iter().find(|r| r.head.name == "decide").unwrap();
    let texts: Vec<String> = rule.body.iter().map(|l| l.describe()).collect();
    assert!(texts.iter().any(|t| t == "rating(A, _, R) asof T"), "{:?}", texts);
}

#[test]
fn a_stored_relation_is_read_at_its_latest_key_at_or_before_t() {
    let prog = program(STORED, "stored_asof");
    let ds = market();
    // Day 1's rating holds until day 4's replaces it; nothing is read ahead.
    let want: Vec<(String, f64)> = DAYS.iter().zip([1.0, 1.0, 1.0, 3.0, 3.0]).map(|(d, q)| (d.to_string(), q)).collect();
    assert_eq!(decided(&ds, &prog, false), want);
    assert_eq!(decided(&ds, &prog, true), want);
}

#[test]
fn a_derived_relation_is_walked_back_over_its_own_bars() {
    let prog = program(DERIVED, "derived_asof");
    let ds = market();
    // `rated` holds only the rating of 3, so the join fails before day 4.
    let want = vec![("2024-01-11".to_string(), 3.0), ("2024-01-12".to_string(), 3.0)];
    assert_eq!(decided(&ds, &prog, false), want);
    assert_eq!(decided(&ds, &prog, true), want);
}

#[test]
fn the_fold_reads_an_asof_tuple_from_its_availability_on() {
    let prog = program(STORED, "stored_asof");
    let mut ds = market();
    let x = ds.intern("X");
    // Day 4's rating is recorded on day 5.
    let late = vec![Value::Equity(x), Value::Time(day(DAYS[3])), Value::Num(3.0)];
    ds.facts.get_mut("rating").unwrap().retain(|tu| *tu != late);
    ds.add_available("rating", late, day(DAYS[4]));
    let fold = decided(&ds, &prog, true);
    assert_eq!(fold.iter().map(|(_, q)| *q).collect::<Vec<_>>(), vec![1.0, 1.0, 1.0, 1.0, 3.0], "{:?}", fold);
    // The batch kernel, blind to availability, reads it on its key.
    let batch = decided(&ds, &prog, false);
    assert_eq!(batch.iter().map(|(_, q)| *q).collect::<Vec<_>>(), vec![1.0, 1.0, 1.0, 3.0, 3.0], "{:?}", batch);
}

// ---- well-formedness ----

fn strategy(body: &str) -> String {
    format!(
        r#"
strategy wf {{
  env asof_1d
  resolution @1d
  mode delta
  param qty : Quantity<Shares> = 1 shares
  {}
}}
"#,
        body
    )
}

#[test]
fn the_bound_key_is_causal_and_may_feed_a_builtin() {
    let src = strategy("decide(T, buy(A, qty)) :- universe(A, T), rating(A, T0, R) asof T, prev(T0, T1), close(A, T1, P), P > 0 USD/share, R > 0.");
    let (p, diags) = check(&src, "wf");
    assert!(p.is_some(), "{:?}", errors(&diags));
    assert!(errors(&diags).is_empty(), "{:?}", errors(&diags));
}

#[test]
fn the_bound_key_cannot_be_a_head_key() {
    // A head keyed at the join's T0 makes T0 the head time, and then the
    // as-of time T is not derived from it: the tuple's time would not be
    // the time it became available (WF-6).
    let src = strategy(
        "rel latest(+A: Equity, @T: Timestamp, -R: Scalar)\n  latest(A, T0, R) :- universe(A, T), rating(A, T0, R) asof T.\n  decide(T, buy(A, qty)) :- universe(A, T), latest(A, T, R), R > 0.",
    );
    let (_, diags) = check(&src, "wf");
    let errs = errors(&diags);
    assert!(
        errs.iter()
            .any(|(c, m)| *c == Code::F && m.contains("as-of time `T`") && m.contains("not derived from the head time `T0`")),
        "{:?}",
        errs
    );
}

#[test]
fn the_asof_time_must_be_bound_and_derived_from_the_head_time() {
    let src = strategy("decide(T, buy(A, qty)) :- universe(A, T), rating(A, T0, R) asof T2, R > 0.");
    let errs = errors(&check(&src, "wf").1);
    assert!(errs.iter().any(|(c, m)| *c == Code::B && m.contains("`T2` is unbound")), "{:?}", errs);
    let src = strategy("decide(T, buy(A, qty)) :- universe(A, T), event(A, T, When), rating(A, T0, R) asof When, R > 0.");
    let errs = errors(&check(&src, "wf").1);
    assert!(errs.iter().any(|(c, m)| *c == Code::F && m.contains("as-of time `When`") && m.contains("not derived")), "{:?}", errs);
}

#[test]
fn a_bound_key_in_an_asof_atom_is_an_x_error() {
    let src = strategy("decide(T, buy(A, qty)) :- universe(A, T), rating(A, T, R) asof T, R > 0.");
    let errs = errors(&check(&src, "wf").1);
    assert!(errs.iter().any(|(c, m)| *c == Code::X && m.contains("bound by the join")), "{:?}", errs);
}

#[test]
fn decided_as_of_the_head_time_would_see_itself() {
    let src = strategy("decide(T, buy(A, qty)) :- universe(A, T), decided(T0, buy(A, _)) asof T.");
    let errs = errors(&check(&src, "wf").1);
    assert!(errs.iter().any(|(c, m)| *c == Code::F && m.contains("`decided` must be read strictly before")), "{:?}", errs);
    // Strictly before T it is a lookup of the last decision on the name.
    let src = strategy("decide(T, buy(A, qty)) :- universe(A, T), prev(T, T1), decided(T0, buy(A, _)) asof T1.");
    let errs = errors(&check(&src, "wf").1);
    assert!(errs.is_empty(), "{:?}", errs);
}

#[test]
fn an_asof_join_crosses_resolutions_where_a_plain_atom_cannot() {
    let minute = |lit: &str| {
        format!(
            r#"
strategy intraday {{
  env asof_1d
  resolution @1m
  mode delta
  param qty : Quantity<Shares> = 1 shares
  decide(T, buy(A, qty)) :- universe_m(A, T), {}, P > 0 USD/share.
}}
"#,
            lit
        )
    };
    let errs = errors(&check(&minute("close(A, T, P)"), "intraday").1);
    assert!(errs.iter().any(|(c, m)| *c == Code::X && m.contains("two resolutions meet only through resample")), "{:?}", errs);
    let errs = errors(&check(&minute("close(A, _, P) asof T"), "intraday").1);
    assert!(errs.is_empty(), "{:?}", errs);
}

/// A daily bar is available at its close (section 3, bar labels): read as
/// of a minute of its own day it is not there yet, so an intraday rule sees
/// the previous day's bar until the day ends, and both drivers agree.
#[test]
fn a_minute_rule_reads_the_previous_days_bar_until_the_day_closes() {
    let src = r#"
strategy intraday_daily {
  env asof_1d
  resolution @1m
  mode delta
  param unit : Quantity<Shares> = 1 shares
  rel seen(+A: Equity, @T: Timestamp, -P: Price<USD>, -D: Timestamp)
  seen(A, T, P, D) :- universe_m(A, T), close(A, D, P) asof T.
  decide(T, buy(A, Q)) :- universe_m(A, T), seen(A, T, P, _), Q = P / (1 USD/share) * unit.
}
"#;
    let p = program(src, "intraday_daily");
    let mut ds = Dataset::new();
    let x = ds.intern("X");
    // Daily closes 10 then 20; minute bars at 09:31 on both days.
    for (d, c) in [("2024-01-08", 10.0), ("2024-01-09", 20.0)] {
        ds.add("close", vec![Value::Equity(x), Value::Time(day(d)), Value::Num(c)]);
        ds.add("universe", vec![Value::Equity(x), Value::Time(day(d))]);
        for m in ["09:31:00", "09:32:00"] {
            let t = day(&format!("{}T{}", d, m));
            ds.add("close_m", vec![Value::Equity(x), Value::Time(t), Value::Num(c)]);
            ds.add("universe_m", vec![Value::Equity(x), Value::Time(t)]);
        }
    }
    ds.derive_tickers();
    let r = run(&p, &ds, ExecConfig::frictionless()).unwrap();
    // On the first day nothing daily is closed yet; on the second the first
    // day's close of 10 is what a minute rule sees, never 20.
    let qty: Vec<(String, f64)> = r.decisions.iter().map(|d| (format_timestamp(d.t), d.decision.amount)).collect();
    assert_eq!(qty, vec![("2024-01-09T09:31:00".into(), 10.0), ("2024-01-09T09:32:00".into(), 10.0)], "{:?}", qty);
    let f = run_fold(&p, &ds, ExecConfig::frictionless()).unwrap();
    assert_eq!(f.decisions.len(), r.decisions.len());
}
