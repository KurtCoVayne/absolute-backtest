//! E-124: N-hour bars over a minute base. `need bars @2h` named no
//! resolution and no calendar constructor counted hours, so a two-hour
//! rule had no form (three posts blocked). A minute calendar does:
//! `calendars { h2 = hours(2) }` groups the bars labelled in
//! `(k·2h, (k+1)·2h]` UTC (a bar is labelled at its close); `open@h2`,
//! `high@h2`, `low@h2` and `close@h2` are each period's first open,
//! highest high, lowest low and last close, and `decide on h2` decides at
//! each period's last bar. `need bars @2h` is refused with the calendar to
//! write; a minute calendar over a daily base is refused.

#![allow(clippy::result_large_err)]

use std::collections::BTreeMap;
use std::sync::Arc;

use abt_data::adapters::databento::{futures, Options, Raw};
use abt_data::adapters::dbn::{self, rtype, Decoded, Header, InstrumentDef, Metadata, Record};
use abt_data::service::{inproc, DataService};
use abt_data::Store;
use abt_kernel::ir::Cal;
use abt_kernel::kernel::time::period_key;
use abt_kernel::kernel::{DataClient, ExecConfig, Replay, SimExecutor, Value};
use abt_tests as common;

const MIN: i64 = 60_000_000_000;
const HOUR: i64 = 60 * MIN;
const DAY: i64 = 24 * HOUR;

fn t(s: &str) -> i64 {
    abt_wire::scalar::parse_timestamp(s).unwrap()
}

fn rec(record: Record) -> Decoded {
    Decoded { record, ts_out: None }
}

fn fixed(x: f64) -> i64 {
    (x * 1e9).round() as i64
}

/// The fixture's days: Monday 2024-01-22 to Friday 2024-01-26.
fn days() -> Vec<i64> {
    (0..5).map(|d| t("2024-01-22") + d * DAY).collect()
}

/// Minute `m` of a day's bars (from 13:00 UTC): its open and close; its high
/// and low a quarter beyond them.
fn prices(d: usize, m: usize) -> (f64, f64) {
    let close = 4800.0 + d as f64 * 10.0 + ((m * 7) % 23) as f64 * 0.25;
    let open = close - 0.25 * (m % 3) as f64 + 0.5 * (m % 2) as f64;
    (open, close)
}

/// One ES contract with a bar every minute from 13:00 to 18:59 UTC (labels
/// 13:01 to 19:00) on each day.
fn serve() -> Arc<DataService> {
    common::init();
    let mut records = vec![rec(Record::InstrumentDef(Box::new(InstrumentDef {
        hd: Header {
            rtype: rtype::INSTRUMENT_DEF,
            publisher_id: 1,
            instrument_id: 1,
            ts_event: t("2023-12-01") as u64,
        },
        ts_recv: t("2023-12-01") as u64,
        raw_symbol: "ESH4".into(),
        asset: "ES".into(),
        instrument_class: b'F',
        expiration: t("2024-03-15T13:30:00") as u64,
        activation: t("2023-06-01") as u64,
        min_price_increment: fixed(0.25),
        unit_of_measure_qty: fixed(50.0),
        currency: "USD".into(),
        ..InstrumentDef::default()
    })))];
    for (d, day) in days().into_iter().enumerate() {
        for m in 0..360 {
            let (open, close) = prices(d, m);
            records.push(rec(Record::Ohlcv {
                hd: Header {
                    rtype: rtype::OHLCV_1M,
                    publisher_id: 1,
                    instrument_id: 1,
                    ts_event: (day + 13 * HOUR + m as i64 * MIN) as u64,
                },
                open: fixed(open),
                high: fixed(open.max(close) + 0.25),
                low: fixed(open.min(close) - 0.25),
                close: fixed(close),
                volume: 100,
            }));
        }
    }
    let meta = Metadata {
        version: 3,
        dataset: "GLBX.MDP3".into(),
        schema: Some(6),
        start: t("2024-01-01") as u64,
        end: Some(t("2024-01-31") as u64),
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
    let mut store = Store::temporary(&format!("hour-bars-{}", std::process::id())).unwrap();
    let reviewed: BTreeMap<String, String> = [("reviewed".to_string(), "complete".to_string())].into_iter().collect();
    let (fetched, _) = futures(&raw, &Options::default()).unwrap();
    fetched.ingest(&mut store, "databento", "databento", &reviewed).unwrap();
    DataService::new(store)
}

fn program(name: &str, bars: &str, calendar: &str) -> String {
    format!(
        "strategy {name} {{
  data {{ class future   need bars {bars} from 2024-01-01   need point_value   require reviewed   prefer latest }}
  uses calendar, bars, book
  calendars {{ h2 = {calendar} }}
  clock {{ decide on h2 }}
  universe {{ universe(A, T) :- close@h2(A, T, _). }}
  params {{ qty : Quantity<Contracts> = 1 contracts }}
  rules {{
    rel bar2(A: Future, @T: h2, O: Price<USD>, H: Price<USD>, L: Price<USD>, C: Price<USD>)
    bar2(A, T, O, H, L, C) :- open@h2(A, T, O), high@h2(A, T, H), low@h2(A, T, L), close@h2(A, T, C).
  }}
  decisions {{
    silence hold
    orders  market
    decide(T, trade(A, qty)) :- universe(A, T), bar2(A, T, O, H, L, C), C > O.
  }}
  execution futures_house
  report {{ calendar h2   periods 3024 per year }}
}}"
    )
}

#[test]
fn an_hours_calendar_groups_minute_bars_by_their_close() {
    // A period is the bars labelled in (k·2h, (k+1)·2h]: 14:00 closes the
    // 12:00 period, 14:01 opens the next.
    let h2 = Cal::Minutes(120);
    let at = |s: &str| period_key(&h2, t(s));
    assert_eq!(at("2024-01-22T12:01:00"), at("2024-01-22T14:00:00"));
    assert_ne!(at("2024-01-22T14:00:00"), at("2024-01-22T14:01:00"));
    assert_eq!(at("2024-01-22T14:01:00"), at("2024-01-22T16:00:00"));
    assert_eq!(h2.to_string(), "hours(2)");
    assert_eq!(Cal::Minutes(90).to_string(), "minutes(90)");
    assert!(Cal::Minutes(30).refines(&h2) && Cal::Base.refines(&h2));
    assert!(!h2.refines(&Cal::Minutes(90)) && !h2.refines(&Cal::Days) && !Cal::Days.refines(&h2));
}

#[test]
fn two_hour_bars_are_their_minutes_first_open_extremes_and_last_close() {
    let svc = serve();
    let name = "two_hour_bars";
    let prog = common::program(&program(name, "@1m", "hours(2)"), name);
    let client = DataClient::connect(inproc(&svc)).unwrap();
    let cfg = ExecConfig::from_program(&prog);
    let mut exec = SimExecutor::new(cfg.clone());
    let (r, mut k) = abt_kernel::kernel::replay(&prog, None, &client, cfg, &mut exec, &Replay::default(), &mut |_| {}).unwrap_or_else(|e| panic!("{}: {}", name, e));
    // The decision bars: each period's last bar, 14:00, 16:00, 18:00 and the
    // day's last at 19:00.
    let want: Vec<i64> = days().iter().flat_map(|d| [14, 16, 18, 19].map(|h| d + h * HOUR)).collect();
    assert_eq!(r.bars, want);
    assert!(!r.decisions.is_empty() && r.decisions.iter().all(|x| want.contains(&x.t)));
    for (d, day) in days().into_iter().enumerate() {
        // 14:00 to 16:00: the bars from 14:00 to 15:59 (minutes 60 to 179).
        let bars: Vec<(f64, f64)> = (60..180).map(|m| prices(d, m)).collect();
        let high = bars.iter().map(|(o, c)| o.max(*c) + 0.25).fold(f64::MIN, f64::max);
        let low = bars.iter().map(|(o, c)| o.min(*c) - 0.25).fold(f64::MAX, f64::min);
        let got: Vec<Vec<f64>> = k
            .query("bar2", day + 16 * HOUR, &[])
            .unwrap()
            .into_iter()
            .map(|tu| {
                tu.iter()
                    .filter_map(|v| match v {
                        Value::Num(x) => Some(*x),
                        _ => None,
                    })
                    .collect()
            })
            .collect();
        assert_eq!(got, vec![vec![bars[0].0, high, low, bars[119].1]], "day {}", d);
    }
}

#[test]
fn n_hour_resolutions_point_to_the_calendar_and_need_a_minute_base() {
    // `need bars @2h`: the calendar to declare.
    let e = common::base_workspace().add_source(&program("at_2h", "@2h", "hours(2)")).expect_err("`@2h` is no resolution");
    assert!(e.message.contains("unknown resolution `@2h`") && e.message.contains("calendars { c = hours(2) }"), "{}", e.message);
    // Over a daily base a minute calendar has no bars to group.
    let src = common::StrategyParts {
        name: "daily_h2".into(),
        calendars: Some("h2 = hours(2)".into()),
        params: "    qty : Quantity<Shares> = 10 shares".into(),
        rules: "    rel c2(A: Equity, @T: h2, C: Price<USD>)\n    c2(A, T, C) :- close@h2(A, T, C).".into(),
        decides: "    decide(T, trade(A, qty)) :- universe(A, T), flat(A, T).".into(),
        ..Default::default()
    }
    .render();
    let (prog, diags) = common::check(&src, "daily_h2");
    let text = common::text(&diags);
    assert!(prog.is_none() && text.contains("needs a sub-daily base"), "{}", text);
}
