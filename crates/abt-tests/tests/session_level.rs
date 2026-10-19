//! E-105: a daily futures program decides the same on the session bars the
//! Databento adapter writes (`--level session`, calendar `1d`) as on the
//! minute bars they are built from. The program reads the minutes through
//! `@session` views on a `decide on session` clock under `need bars @1m`,
//! and the session bars under `need bars @1d`, which the resolver binds
//! because it prefers the coarsest version that satisfies a need.

use std::collections::BTreeMap;
use std::sync::Arc;

use abt_data::adapters::databento::{futures, Level, Options, Raw};
use abt_data::adapters::dbn::{self, rtype, Decoded, Header, InstrumentDef, Metadata, Record};
use abt_data::service::{inproc, DataService};
use abt_data::Store;
use abt_kernel::kernel::{DataClient, ExecConfig, Replay, RunResult};
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

fn bar(id: u32, start: i64, open: f64, close: f64, volume: u64) -> Decoded {
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
        volume,
    })
}

fn def(id: u32, symbol: &str, root: &str, expiration: &str) -> Decoded {
    rec(Record::InstrumentDef(Box::new(InstrumentDef {
        hd: Header {
            rtype: rtype::INSTRUMENT_DEF,
            publisher_id: 1,
            instrument_id: id,
            ts_event: t("2023-12-01") as u64,
        },
        ts_recv: t("2023-12-01") as u64,
        raw_symbol: symbol.into(),
        asset: root.into(),
        instrument_class: b'F',
        expiration: t(expiration) as u64,
        activation: t("2023-06-01") as u64,
        min_price_increment: fixed(0.25),
        unit_of_measure_qty: fixed(50.0),
        currency: "USD".into(),
        ..InstrumentDef::default()
    })))
}

/// Forty winter trade dates of ES (H4 rolling to M4 by volume from the
/// twenty-sixth, while the program holds it) and NQ (H4), each a session from 23:00 UTC the evening
/// before to 21:30 UTC, eight bars a contract at fixed times, the prices a
/// deterministic swing that makes seven-session highs and lows.
fn stream() -> Vec<u8> {
    let mut records = vec![
        def(1, "ESH4", "ES", "2024-03-15T13:30:00"),
        def(2, "ESM4", "ES", "2024-06-21T13:30:00"),
        def(3, "NQH4", "NQ", "2024-03-15T13:30:00"),
    ];
    let mut day = t("2024-01-02");
    let mut n = 0;
    while n < 40 {
        let weekday = (day / DAY + 4).rem_euclid(7);
        if weekday == 0 || weekday == 6 {
            day += DAY;
            continue;
        }
        let minutes = [-60, 180, 540, 870, 930, 990, 1080, 1290];
        for (k, m) in minutes.iter().enumerate() {
            let start = day + m * MIN;
            let swing = |phase: f64, scale: f64| 4800.0 + scale * ((n as f64 + phase) * 0.45).sin() * 40.0 + k as f64 * 0.25;
            let es = swing(0.0, 1.0);
            let nq = swing(1.7, 2.5) + 12000.0;
            let (vh, vm) = if n < 25 { (900, 100) } else { (100, 900) };
            records.push(bar(1, start, es - 0.5, es, vh));
            records.push(bar(2, start, es + 9.5, es + 10.0, vm));
            records.push(bar(3, start, nq - 0.5, nq, 500));
        }
        n += 1;
        day += DAY;
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
    dbn::encode(&meta, &records)
}

/// A store with both levels of the same bars: the minutes under source
/// `databento`, the session bars under `databento_1d`.
fn serve() -> Arc<DataService> {
    common::init();
    let mut store = Store::temporary(&format!("session-level-{}", std::process::id())).unwrap();
    let mut raw = Raw::default();
    raw.add_dbn(&stream(), "fixture").unwrap();
    let reviewed: BTreeMap<String, String> = [("reviewed".to_string(), "complete".to_string())].into_iter().collect();
    for (level, source) in [(Level::Bars, "databento"), (Level::Session, "databento_1d")] {
        let (fetched, _) = futures(&raw, &Options { level, ..Options::default() }).unwrap();
        fetched.ingest(&mut store, "databento", source, &reviewed).unwrap();
    }
    DataService::new(store)
}

/// Connors' Double Seven on a session clock: long at a close below the
/// seven-session low of the closes before it, flat at a close above the
/// seven-session high; one contract, the next session's open.
fn program(bars: &str) -> abt_kernel::check::Program {
    let src = format!(
        "strategy double_seven_moo {{
  data {{ class future   need bars {bars} from 2024-01-01   need sessions, point_value   require reviewed   prefer latest }}
  uses calendar, bars, book
  calendars {{ session = sessions }}
  clock {{ decide on session }}
  universe {{ universe(A, T) :- session(A, T, _). }}
  params {{
    n   : Count = 7
    qty : Quantity<Contracts> = 1 contracts
  }}
  rules {{
    rel lo7(A: Future, @T, L: Price<USD>)
    lo7(A, T, L) :- universe(A, T), L = min(P) over (T1 in prior_rows[n], close@session(A, T1, P)).
    rel hi7(A: Future, @T, H: Price<USD>)
    hi7(A, T, H) :- universe(A, T), H = max(P) over (T1 in prior_rows[n], close@session(A, T1, P)).
  }}
  decisions {{
    silence hold
    orders  moo
    decide(T, hold(A, qty)) :- close@session(A, T, P), lo7(A, T, L), P < L, flat(A, T).
    decide(T, hold(A, 0 contracts)) :- held(A, T, Q), Q > 0 contracts, close@session(A, T, P), hi7(A, T, H), P > H.
  }}
  execution futures_house with {{ orders moo }}
  report {{ calendar session   periods 252 per year }}
}}"
    );
    common::program(&src, "double_seven_moo")
}

fn run(prog: &abt_kernel::check::Program, svc: &Arc<DataService>) -> RunResult {
    let client = DataClient::connect(inproc(svc)).unwrap();
    abt_kernel::kernel::run_on(prog, None, &client, ExecConfig::from_program(prog), &Replay::default()).unwrap_or_else(|e| panic!("{}", e))
}

/// What a run decided and filled, exactly, and its rolls.
fn digest(r: &RunResult) -> (Vec<String>, Vec<String>) {
    let decisions = r.decisions.iter().map(|d| format!("{} {} {}", d.t, r.describe_decision(&d.decision), d.rule)).collect();
    let mut fills: Vec<String> = r
        .fills
        .iter()
        .map(|f| {
            format!(
                "{} {} {:x} {:x} {:x}",
                f.t,
                r.symbols[f.equity as usize],
                f.quantity.to_bits(),
                f.price.to_bits(),
                f.commission.to_bits()
            )
        })
        .collect();
    fills.extend(r.events.iter().filter(|e| e.kind.name() == "roll").map(|e| format!("roll {:?}", e)));
    (decisions, fills)
}

#[test]
fn a_daily_program_decides_the_same_on_session_bars_as_on_their_minutes() {
    let svc = serve();
    let minutes = run(&program("@1m"), &svc);
    let sessions = run(&program("@1d"), &svc);
    let (dm, fm) = digest(&minutes);
    let (ds, fs) = digest(&sessions);
    assert!(dm.len() >= 4, "the fixture decides: {:?}", dm);
    assert_eq!(dm, ds, "decisions");
    // Rolls are compared with the fills; neither run emits one here, as no
    // run over adapter-ingested contracts does (the finding of E-105).
    assert_eq!(fm, fs, "fills and rolls");
    assert_eq!(minutes.final_cash.to_bits(), sessions.final_cash.to_bits());
    // The session run bound and read one close a session: far fewer tuples
    // than the minutes' eight a session.
    let closes = |r: &RunResult| r.coverage.iter().find(|c| c.name == "close").map(|c| c.tuples).unwrap_or(0);
    assert!(closes(&sessions) > 0 && closes(&sessions) * 4 < closes(&minutes), "{} against {}", closes(&sessions), closes(&minutes));
}
