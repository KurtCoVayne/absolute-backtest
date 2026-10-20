//! Session sub-intervals and month boundaries on a futures session clock,
//! over minute bars the Databento adapter writes.
//!
//! E-106: `first(P) over (T1 in session(X, [5min, 10min)), …)` asked at a
//! bar before minute 5 is the empty window (the kernel panicked: a range
//! whose start passed its end). E-107: `month_start(T)` on a `decide on
//! session` clock holds at a month's first session, each instrument's when
//! two roots' sessions close at different times.

use std::collections::BTreeMap;
use std::sync::Arc;

use abt_data::adapters::databento::{futures, Options, Raw};
use abt_data::adapters::dbn::{self, rtype, Decoded, Header, InstrumentDef, Metadata, Record};
use abt_data::service::{inproc, DataService};
use abt_data::Store;
use abt_kernel::compiler::{compile, CompileOptions};
use abt_kernel::kernel::{DataClient, ExecConfig, Replay, RunResult};
use abt_kernel::lower::{Lowered, Numerics};
use abt_tests as common;

const MIN: i64 = 60_000_000_000;
const DAY: i64 = 86_400_000_000_000;

fn t(s: &str) -> i64 {
    abt_wire::scalar::parse_timestamp(s).unwrap()
}

fn rec(record: Record) -> Decoded {
    Decoded { record, ts_out: None }
}

fn fixed(x: f64) -> i64 {
    (x * 1e9).round() as i64
}

fn bar(start: i64, open: f64, close: f64) -> Decoded {
    bar_of(1, start, open, close)
}

fn bar_of(id: u32, start: i64, open: f64, close: f64) -> Decoded {
    rec(Record::Ohlcv {
        hd: Header {
            rtype: rtype::OHLCV_1M,
            publisher_id: 1,
            instrument_id: id,
            ts_event: start as u64,
        },
        open: fixed(open),
        high: fixed(open.max(close) + 0.25),
        low: fixed(open.min(close) - 0.25),
        close: fixed(close),
        volume: 100,
    })
}

/// The first trade dates of each session in the fixture: from 2024-01-22 to
/// 2024-03-08, weekdays (winter: the break's middle is 22:30 UTC).
fn days() -> Vec<i64> {
    let mut out = Vec::new();
    let mut day = t("2024-01-22");
    while day <= t("2024-03-08") {
        let weekday = (day / DAY + 4).rem_euclid(7);
        if weekday != 0 && weekday != 6 {
            out.push(day);
        }
        day += DAY;
    }
    out
}

fn def(id: u32, symbol: &str, asset: &str, size: f64) -> Decoded {
    rec(Record::InstrumentDef(Box::new(InstrumentDef {
        hd: Header {
            rtype: rtype::INSTRUMENT_DEF,
            publisher_id: 1,
            instrument_id: id,
            ts_event: t("2023-12-01") as u64,
        },
        ts_recv: t("2023-12-01") as u64,
        raw_symbol: symbol.into(),
        asset: asset.into(),
        instrument_class: b'F',
        expiration: t("2024-03-15T13:30:00") as u64,
        activation: t("2023-06-01") as u64,
        min_price_increment: fixed(0.25),
        unit_of_measure_qty: fixed(size),
        currency: "USD".into(),
        ..InstrumentDef::default()
    })))
}

/// One ES contract: each session opens at 23:00 UTC the evening before with
/// a bar every minute for twenty minutes, then bars at 14:30 and 21:00 UTC.
/// With `nq`, an NQ contract too, its sessions' last bar at 20:00 UTC.
fn serve(nq: bool) -> Arc<DataService> {
    common::init();
    let mut records = vec![def(1, "ESH4", "ES", 50.0)];
    if nq {
        records.push(def(2, "NQH4", "NQ", 20.0));
    }
    for (n, day) in days().into_iter().enumerate() {
        let open = day - 60 * MIN;
        let base = 4800.0 + (n as f64 * 0.7).sin() * 20.0;
        for m in 0..20 {
            let px = base + ((n * 7 + m) as f64 * 0.9).sin() * 3.0;
            records.push(bar(open + m as i64 * MIN, px - 0.5, px));
        }
        records.push(bar(day + (14 * 60 + 30) * MIN, base + 1.0, base + 1.5));
        records.push(bar(day + 21 * 60 * MIN, base + 1.5, base + 2.0));
        if nq {
            for m in 0..20 {
                records.push(bar_of(2, open + m as i64 * MIN, 3.0 * base - 0.5, 3.0 * base));
            }
            records.push(bar_of(2, day + 20 * 60 * MIN, 3.0 * base + 1.0, 3.0 * base + 1.5));
        }
    }
    let meta = Metadata {
        version: 3,
        dataset: "GLBX.MDP3".into(),
        schema: Some(6),
        start: t("2024-01-01") as u64,
        end: Some(t("2024-03-31") as u64),
        limit: None,
        stype_in: Some(1),
        stype_out: 0,
        ts_out: false,
        symbol_cstr_len: 71,
        symbols: vec![],
        partial: vec![],
        not_found: vec![],
        mappings: vec![],
    };
    let mut raw = Raw::default();
    raw.add_dbn(&dbn::encode(&meta, &records), "fixture").unwrap();
    let mut store = Store::temporary(&format!("session-window-{}-{}", nq, std::process::id())).unwrap();
    let reviewed: BTreeMap<String, String> = [("reviewed".to_string(), "complete".to_string())].into_iter().collect();
    let (fetched, _) = futures(&raw, &Options::default()).unwrap();
    fetched.ingest(&mut store, "databento", "databento", &reviewed).unwrap();
    DataService::new(store)
}

fn run(src: &str, name: &str, svc: &Arc<DataService>) -> RunResult {
    let prog = common::program(src, name);
    let client = DataClient::connect(inproc(svc)).unwrap();
    abt_kernel::kernel::run_on(&prog, None, &client, ExecConfig::from_program(&prog), &Replay::default()).unwrap_or_else(|e| panic!("{}: {}", name, e))
}

/// `run` on a plan: the written order, or the compiler's optimised plan
/// (what `abt run` runs by default).
fn run_plan(src: &str, name: &str, svc: &Arc<DataService>, optimised: bool) -> RunResult {
    let prog = common::program(src, name);
    let cfg = ExecConfig::from_program(&prog);
    let numerics = Numerics::from_running_sums(cfg.running_sums);
    let lowered = if optimised {
        compile(
            &prog,
            &CompileOptions {
                numerics,
                ..CompileOptions::default()
            },
        )
        .lowered
    } else {
        Lowered::written(&prog, numerics)
    };
    let client = DataClient::connect(inproc(svc)).unwrap();
    abt_kernel::kernel::run_on(&prog, Some(&lowered), &client, cfg, &Replay::default()).unwrap_or_else(|e| panic!("{}: {}", name, e))
}

/// A program deciding at `session + offset` whether the close is above the
/// first open of minutes [5, 10) of the session.
fn opening_window(name: &str, offset: &str) -> String {
    format!(
        "strategy {name} {{
  data {{ class future   need bars @1m from 2024-01-01   need sessions, point_value   require reviewed   prefer latest }}
  uses calendar, bars, book
  calendars {{ session = sessions }}
  clock {{ decide on base at session + {offset} }}
  universe {{ universe(A, T) :- close(A, T, _). }}
  params {{ qty : Quantity<Contracts> = 1 contracts }}
  rules {{
    rel o5(A: Future, @X: base | session, O: Price<USD>)
    o5(A, X, O) :- O = first(P) over (T1 in session(X, [5min, 10min)), open(A, T1, P)).
  }}
  decisions {{
    silence hold
    orders  market
    decide(T, trade(A, qty)) :- universe(A, T), o5(A, T, O), close(A, T, P), P > O.
  }}
  execution futures_house
  report {{ calendar session   periods 252 per year }}
}}"
    )
}

#[test]
fn a_session_sub_interval_after_the_bar_asked_is_empty() {
    let svc = serve(false);
    // At minute 3 the window [5min, 10min) has no bar yet: nothing decided,
    // and no panic.
    let early = run(&opening_window("open_window_early", "3min"), "open_window_early", &svc);
    assert!(early.decisions.is_empty(), "{:?}", early.decisions.len());
    // At minute 12 the window holds its five bars: the rule decides.
    let late = run(&opening_window("open_window_late", "12min"), "open_window_late", &svc);
    assert!(!late.decisions.is_empty(), "the window is read at minute 12");
}

/// A program on a session clock trading one contract at the sessions where
/// `ms` holds: `month_start` over the head's session (`form` a body).
fn month_program(name: &str, head: &str, body: &str, decide_on: &str) -> String {
    format!(
        "strategy {name} {{
  data {{ class future   need bars @1m from 2024-01-01   need sessions, point_value   require reviewed   prefer latest }}
  uses calendar, bars, book
  calendars {{ session = sessions }}
  clock {{ decide on session }}
  universe {{ universe(A, T) :- session(A, T, _). }}
  params {{ qty : Quantity<Contracts> = 1 contracts }}
  rules {{
    rel {head}
    {body}
  }}
  decisions {{
    silence hold
    orders  market
    decide(T, trade(A, qty)) :- universe(A, T), {decide_on}.
  }}
  execution futures_house
  report {{ calendar session   periods 252 per year }}
}}"
    )
}

#[test]
fn month_start_holds_at_a_months_first_session() {
    let svc = serve(false);
    // The months' first sessions, labelled at their last bar's close.
    let firsts = vec![t("2024-02-01T21:01:00"), t("2024-03-01T21:01:00")];
    for (name, head, body, on) in [
        ("ms_session", "ms(A: Future, @T)", "ms(A, T) :- session(A, T, _), month_start(T).", "ms(A, T)"),
        ("ms_bar", "ms(@T)", "ms(T) :- bar(T), month_start(T).", "ms(T)"),
    ] {
        let r = run(&month_program(name, head, body, on), name, &svc);
        let at: Vec<i64> = r.decisions.iter().map(|d| d.t).filter(|t| *t > days()[0] + DAY).collect();
        eprintln!("{}: decisions at {:?}, the firsts {:?}", name, at, firsts);
        assert_eq!(at, firsts, "{}", name);
    }
}

/// E-107: with two roots whose sessions close an hour apart (NQ at 20:01,
/// ES at 21:01), the optimised plan placed `month_start(T)` before
/// `session(A, T, _)` bound A, and it stepped over both roots' sessions:
/// ES's month's first session followed NQ's of the same day and never
/// began a month (on the served store, ES 2025 H1 decided 6 times on the
/// written plan and never on the optimised one). On either plan each root
/// now decides at its own months' first sessions.
#[test]
fn month_start_steps_over_the_head_instruments_own_sessions() {
    let svc = serve(true);
    let name = "ms_two_roots";
    let src = month_program(name, "ms(A: Future, @T)", "ms(A, T) :- session(A, T, _), month_start(T).", "ms(A, T)");
    let firsts = vec![t("2024-02-01T20:01:00"), t("2024-02-01T21:01:00"), t("2024-03-01T20:01:00"), t("2024-03-01T21:01:00")];
    for optimised in [false, true] {
        let r = run_plan(&src, name, &svc, optimised);
        let mut at: Vec<i64> = r.decisions.iter().map(|d| d.t).filter(|t| *t > days()[0] + DAY).collect();
        at.sort();
        assert_eq!(at, firsts, "optimised: {}", optimised);
    }
}
