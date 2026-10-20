//! The futures model (K-5.4) and a futures margin call (K-5.6) on
//! hand-computed cases (K-9.3, G8). A daily root `ES` (point value 50) is
//! traded through its continuous series `ES.v.0` over the ten weekdays from
//! 2024-03-04 (d0) to 2024-03-15 (d9), its bars, contracts and settlement
//! prices ingested as the Databento adapter writes them, and run through
//! the kernel over an in-process data service. A decision at a bar fills
//! at the next bar's close.

#![allow(clippy::result_large_err)]

use std::collections::BTreeMap;
use std::sync::Arc;

use abt_data::adapter::{Security, Terms};
use abt_data::adapters::{Fetched, Keys};
use abt_data::service::{inproc, DataService};
use abt_data::Store;
use abt_kernel::data::business_days;
use abt_kernel::kernel::{DataClient, EventKind, ExecConfig, Kernel, Replay, RunEvent, RunResult, Sym, Value};
use abt_tests as common;
use arrow::array::{ArrayRef, Float64Array, RecordBatch, StringArray, TimestampNanosecondArray};
use arrow::datatypes::{DataType, Field, Schema, TimeUnit};

fn d(i: usize) -> i64 {
    business_days((2024, 3, 4), 10)[i]
}

fn ts() -> DataType {
    DataType::Timestamp(TimeUnit::Nanosecond, None)
}

/// A relation's rows keyed by vendor key: `A`, `T`, the values, `r` (the
/// row's own time: received at once).
fn keyed(rows: &[(&str, i64, Vec<f64>)], names: &[&str]) -> RecordBatch {
    let mut fields = vec![Field::new("A", DataType::Utf8, false), Field::new("T", ts(), false)];
    let mut cols: Vec<ArrayRef> = vec![
        Arc::new(StringArray::from(rows.iter().map(|r| r.0).collect::<Vec<_>>())),
        Arc::new(TimestampNanosecondArray::from(rows.iter().map(|r| r.1).collect::<Vec<_>>())),
    ];
    for (j, n) in names.iter().enumerate() {
        fields.push(Field::new(*n, DataType::Float64, true));
        cols.push(Arc::new(Float64Array::from(rows.iter().map(|r| r.2[j]).collect::<Vec<_>>())));
    }
    fields.push(Field::new("r", ts(), false));
    cols.push(Arc::new(TimestampNanosecondArray::from(rows.iter().map(|r| r.1).collect::<Vec<_>>())));
    RecordBatch::try_new(Arc::new(Schema::new(fields)), cols).unwrap()
}

/// An instrument of a market: its key, ticker, point value and closes.
type Instrument = (&'static str, &'static str, f64, Vec<f64>);

/// A keyed row: key, time, values.
type Row = (&'static str, i64, Vec<f64>);

/// A futures market: each instrument, its settlement prices where given,
/// the continuous series' contracts and adjustments by day, the contracts'
/// terms, and published margins.
#[derive(Default)]
struct Market {
    instruments: Vec<Instrument>,
    settlement: Vec<(&'static str, usize, f64)>,
    /// (root key, day, contract key, adjustment)
    continuous: Vec<(&'static str, usize, &'static str, f64)>,
    /// Nanoseconds after its day's bar each `continuous` row sits (a minute
    /// store's rows fall between a session clock's barriers).
    continuous_offset: i64,
    /// (contract key, root, expiry day, last trade day, cash settled)
    specs: Vec<(&'static str, &'static str, usize, usize, bool)>,
    /// (key, day, initial, maintenance)
    margin: Vec<(&'static str, usize, f64, f64)>,
    commission: f64,
}

static TAG: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

fn serve(m: &Market) -> Arc<DataService> {
    common::init();
    let n = TAG.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    let mut store = Store::temporary(&format!("futures-{}-{}", std::process::id(), n)).unwrap();
    let securities: Vec<Security> = m
        .instruments
        .iter()
        .map(|(key, ticker, ..)| Security {
            key: key.to_string(),
            ticker: ticker.to_string(),
            from: d(0),
            to: None,
        })
        .collect();
    let terms: Vec<Terms> = m
        .instruments
        .iter()
        .map(|(key, _, pv, _)| Terms {
            key: key.to_string(),
            class: "future".into(),
            multiplier: Some(*pv),
            commission_per_contract: Some(m.commission),
        })
        .collect();
    let bars: Vec<(&str, i64, Vec<f64>)> = m
        .instruments
        .iter()
        .flat_map(|(key, _, _, closes)| closes.iter().enumerate().map(move |(i, c)| (*key, d(i), vec![*c])))
        .collect();
    let per_day = |f: &dyn Fn(&Instrument, usize) -> f64| -> Vec<Row> { m.instruments.iter().flat_map(|x| (0..x.3.len()).map(move |i| (x.0, d(i), vec![f(x, i)]))).collect() };
    let mut relations = vec![
        ("future.open".to_string(), keyed(&bars, &["O"])),
        ("future.high".to_string(), keyed(&bars, &["H"])),
        ("future.low".to_string(), keyed(&bars, &["L"])),
        ("future.close".to_string(), keyed(&bars, &["P"])),
        ("future.volume".to_string(), keyed(&per_day(&|_, _| 1_000_000.0), &["V"])),
        ("future.point_value".to_string(), keyed(&per_day(&|x, _| x.2), &["V"])),
        ("future.commission".to_string(), keyed(&per_day(&|_, _| m.commission), &["C"])),
    ];
    if !m.settlement.is_empty() {
        let rows: Vec<(&str, i64, Vec<f64>)> = m.settlement.iter().map(|(k, i, p)| (*k, d(*i), vec![*p])).collect();
        relations.push(("future.settlement".to_string(), keyed(&rows, &["P"])));
    }
    if !m.margin.is_empty() {
        let rows: Vec<(&str, i64, Vec<f64>)> = m.margin.iter().map(|(k, i, a, b)| (*k, d(*i), vec![*a, *b])).collect();
        relations.push(("future.margin".to_string(), keyed(&rows, &["Initial", "Maintenance"])));
    }
    if !m.continuous.is_empty() {
        let c = &m.continuous;
        relations.push((
            "continuous".to_string(),
            RecordBatch::try_new(
                Arc::new(Schema::new(vec![
                    Field::new("root_id", DataType::Utf8, false),
                    Field::new("t", ts(), false),
                    Field::new("contract_id", DataType::Utf8, true),
                    Field::new("adjustment", DataType::Float64, true),
                    Field::new("r", ts(), false),
                ])),
                vec![
                    Arc::new(StringArray::from(c.iter().map(|x| x.0).collect::<Vec<_>>())),
                    Arc::new(TimestampNanosecondArray::from(c.iter().map(|x| d(x.1) + m.continuous_offset).collect::<Vec<_>>())),
                    Arc::new(StringArray::from(c.iter().map(|x| x.2).collect::<Vec<_>>())),
                    Arc::new(Float64Array::from(c.iter().map(|x| x.3).collect::<Vec<_>>())),
                    Arc::new(TimestampNanosecondArray::from(c.iter().map(|x| d(x.1) + m.continuous_offset).collect::<Vec<_>>())),
                ],
            )
            .unwrap(),
        ));
    }
    if !m.specs.is_empty() {
        let s = &m.specs;
        let pv = |key: &str| m.instruments.iter().find(|x| x.0 == key).map(|x| x.2);
        relations.push((
            "future_spec".to_string(),
            RecordBatch::try_new(
                Arc::new(Schema::new(vec![
                    Field::new("id", DataType::Utf8, false),
                    Field::new("first_trade", ts(), false),
                    Field::new("root", DataType::Utf8, true),
                    Field::new("expiry", ts(), true),
                    Field::new("last_trade", ts(), true),
                    Field::new("point_value", DataType::Float64, true),
                    Field::new("tick", DataType::Float64, true),
                    Field::new("settlement", DataType::Utf8, true),
                    Field::new("r", ts(), false),
                ])),
                vec![
                    Arc::new(StringArray::from(s.iter().map(|x| x.0).collect::<Vec<_>>())),
                    Arc::new(TimestampNanosecondArray::from(vec![d(0); s.len()])),
                    Arc::new(StringArray::from(s.iter().map(|x| x.1).collect::<Vec<_>>())),
                    Arc::new(TimestampNanosecondArray::from(s.iter().map(|x| d(x.2)).collect::<Vec<_>>())),
                    Arc::new(TimestampNanosecondArray::from(s.iter().map(|x| d(x.3)).collect::<Vec<_>>())),
                    Arc::new(Float64Array::from(s.iter().map(|x| pv(x.0)).collect::<Vec<_>>())),
                    Arc::new(Float64Array::from(vec![Some(0.25); s.len()])),
                    Arc::new(StringArray::from(s.iter().map(|x| if x.4 { "cash" } else { "physical" }).collect::<Vec<_>>())),
                    Arc::new(TimestampNanosecondArray::from(vec![d(0); s.len()])),
                ],
            )
            .unwrap(),
        ));
    }
    let mut attributes = BTreeMap::new();
    attributes.insert("reviewed".to_string(), "complete".to_string());
    attributes.insert("calendar".to_string(), "1d".to_string());
    let fetched = Fetched {
        securities,
        terms,
        relations,
        attributes: attributes.clone(),
        keys: Keys::Vendor,
        ..Fetched::default()
    };
    fetched.ingest(&mut store, "databento", "databento", &attributes).unwrap();
    // The class's sessions, empty, as a daily dataset's are.
    let _ = abt_data::adapter::ingest_empty(&mut store, "future.session", "databento", &attributes);
    DataService::new(store)
}

/// A daily futures program with decide rules over `execution` fields.
fn program(decides: &str, exec: &[(&str, &str)]) -> abt_kernel::check::Program {
    let mut fields: Vec<(String, String)> = [
        ("price", "close"),
        ("orders", "market"),
        ("lot", "whole"),
        ("capital", "100_000 USD fixed"),
        ("commission", "per_contract from catalog"),
        ("slippage", "none"),
        ("impact", "none"),
        ("participation", "none"),
        ("financing", "none"),
        ("leverage", "unconstrained"),
        ("actions", "apply"),
        ("delisting", "last_price"),
        ("oversize", "allow"),
        ("ruin", "halt"),
        ("numerics", "two_pass"),
        ("settlement", "daily"),
    ]
    .iter()
    .map(|(k, v)| (k.to_string(), v.to_string()))
    .collect();
    for (k, v) in exec {
        match fields.iter_mut().find(|(f, _)| f == k) {
            Some(f) => f.1 = v.to_string(),
            None => fields.push((k.to_string(), v.to_string())),
        }
    }
    let execution = format!("{{\n{}\n  }}", fields.iter().map(|(k, v)| format!("    {} {}", k, v)).collect::<Vec<_>>().join("\n"));
    let src = common::StrategyParts {
        name: "fut".into(),
        class: "future".into(),
        needs: "need bars @1d   need point_value, commission".into(),
        uses: "calendar, book".into(),
        universe: "universe(A, T) :- close(A, T, _).".into(),
        params: "    qty : Quantity<Contracts> = 2 contracts".into(),
        decides: decides.into(),
        execution,
        ..Default::default()
    }
    .render();
    common::program(&src, "fut")
}

fn run(prog: &abt_kernel::check::Program, svc: &Arc<DataService>) -> RunResult {
    let client = DataClient::connect(inproc(svc)).unwrap();
    let cfg = ExecConfig::from_program(prog);
    abt_kernel::kernel::run_on(prog, None, &client, cfg, &Replay::default()).unwrap_or_else(|e| panic!("{}", e))
}

fn run_with<T: Send>(prog: &abt_kernel::check::Program, svc: &Arc<DataService>, f: impl for<'k> FnOnce(&RunResult, &mut Kernel<'k>) -> T + Send) -> (RunResult, T) {
    let client = DataClient::connect(inproc(svc)).unwrap();
    let cfg = ExecConfig::from_program(prog);
    abt_kernel::kernel::run_keeping(prog, None, &client, cfg, &Replay::default(), |r, k| Ok(f(r, k))).unwrap_or_else(|e| panic!("{}", e))
}

fn events<'r>(r: &'r RunResult, kind: &str) -> Vec<&'r RunEvent> {
    r.events.iter().filter(|e| e.kind.name() == kind).collect()
}

fn sym(r: &RunResult, name: &str) -> Sym {
    r.symbols.iter().position(|s| s == name).unwrap_or_else(|| panic!("no {} in {:?}", name, r.symbols)) as Sym
}

fn close(a: f64, b: f64) -> bool {
    (a - b).abs() < 1e-6 * (1.0 + b.abs())
}

/// Buy 2 of `ticker` at d0 (filled at d1's close).
fn buy_at_d0(ticker: &str) -> String {
    format!("    decide(T, trade(A, qty)) :- universe(A, T), T = 2024-03-04, A = \"{}\".", ticker)
}

const ES: [f64; 10] = [5000.0, 5010.0, 4990.0, 5025.0, 5040.0, 5040.0, 5040.0, 5040.0, 5040.0, 5040.0];

#[test]
fn daily_settlement_moves_the_variation_to_cash_and_leaves_the_nav_unchanged() {
    // Bought 2 at d1's close, 5010. Settled at d1, d2 and d3 at prices a
    // quarter point off the closes: the three variations sum to the P&L to
    // the last settlement, and the NAV is the fully paid book's every bar.
    let m = Market {
        instruments: vec![("ES.v.0", "ES", 50.0, ES[..4].to_vec())],
        settlement: vec![("ES.v.0", 1, 5010.25), ("ES.v.0", 2, 4990.25), ("ES.v.0", 3, 5025.25)],
        commission: 0.0,
        ..Market::default()
    };
    let svc = serve(&m);
    let r = run(&program(&buy_at_d0("ES"), &[]), &svc);
    let s = events(&r, "settlement");
    assert_eq!(s.len(), 3, "{:#?}", r.events);
    let flows: Vec<f64> = s.iter().map(|e| e.cash_flow.unwrap()).collect();
    // 2 × 50 × (5010.25 − 5010), (4990.25 − 5010.25), (5025.25 − 4990.25).
    assert!(close(flows[0], 25.0) && close(flows[1], -2_000.0) && close(flows[2], 3_500.0), "{:?}", flows);
    assert!(close(flows.iter().sum::<f64>(), 2.0 * 50.0 * (5025.25 - 5010.0)));
    // The fill moved no cash: margined, not paid for.
    let fill = events(&r, "fill")[0];
    assert_eq!(fill.cash_flow, Some(0.0));
    // The NAV is the fully paid book's (`settlement none`) at every bar.
    let paid = run(&program(&buy_at_d0("ES"), &[("settlement", "none")]), &svc);
    assert!(events(&paid, "settlement").is_empty());
    let navs: Vec<f64> = r.equity_curve.iter().map(|x| x.1).collect();
    let paid_navs: Vec<f64> = paid.equity_curve.iter().map(|x| x.1).collect();
    assert_eq!(navs.len(), paid_navs.len());
    for (a, b) in navs.iter().zip(&paid_navs) {
        assert!(close(*a, *b), "{:?} against {:?}", navs, paid_navs);
    }
    // The cash is the variation only.
    assert!(close(r.final_cash, 100_000.0 + 2.0 * 50.0 * (5025.25 - 5010.0)), "{}", r.final_cash);
    // The variation account at each bar: the move from the session's
    // settlement to the close, 2 × 50 × −0.25, which the NAV counts.
    let held: Vec<&abt_kernel::kernel::ExposureRecord> = r.exposure.iter().filter(|x| x.t >= d(1)).collect();
    assert_eq!(held.len(), 3);
    for x in held {
        assert!(close(x.variation, -25.0) && close(x.equity, x.cash + x.variation), "{:?}", x);
    }
    // Without settlement prices the close stands in, warned.
    let m2 = Market { settlement: vec![], ..m };
    let r = run(&program(&buy_at_d0("ES"), &[]), &serve(&m2));
    assert!(events(&r, "settlement").iter().all(|e| matches!(e.kind, EventKind::Settlement { at_close: true, .. })));
    assert!(r.warnings.iter().any(|w| w.bias == "settlement" && w.message.contains("no settlement price")), "{:?}", r.warnings);
}

#[test]
fn a_roll_records_two_fills_and_two_commissions_and_carries_the_position() {
    // ES.v.0 holds ESH4 to d2 and ESM4 from d3, ESM4 4 points above: the
    // back-adjusted series is ESH4 + 4 before the roll. Bought 2 at d1.
    let closes = vec![5004.0, 5014.0, 4994.0, 5029.0, 5044.0];
    let m = Market {
        instruments: vec![
            ("ES.v.0", "ES", 50.0, closes.clone()),
            ("ESH4", "ESH4", 50.0, vec![5000.0, 5010.0, 4990.0]),
            ("ESM4", "ESM4", 50.0, vec![5004.0, 5014.0, 4994.0, 5029.0, 5044.0]),
        ],
        continuous: vec![
            ("ES.v.0", 0, "ESH4", 4.0),
            ("ES.v.0", 1, "ESH4", 4.0),
            ("ES.v.0", 2, "ESH4", 4.0),
            ("ES.v.0", 3, "ESM4", 0.0),
            ("ES.v.0", 4, "ESM4", 0.0),
        ],
        specs: vec![("ESH4", "ES", 9, 9, true), ("ESM4", "ES", 9, 9, true)],
        commission: 2.25,
        ..Market::default()
    };
    let svc = serve(&m);
    let prog = program(&buy_at_d0("ES"), &[("settlement", "none")]);
    let (r, contract_at) = run_with(&prog, &svc, |r, k| {
        let root = Value::Equity(sym(r, "ES.v.0"));
        let at = |k: &mut Kernel, t: i64| k.query("contract", t, &[]).unwrap().into_iter().find(|tu| tu[2] == root).map(|tu| tu[0].clone());
        (at(k, d(2)), at(k, d(3)))
    });
    let rolls = events(&r, "roll");
    assert_eq!(rolls.len(), 1, "{:#?}", r.events);
    let EventKind::Roll {
        from,
        to,
        quantity,
        from_price,
        to_price,
        adjustment,
        commissions,
    } = &rolls[0].kind
    else {
        panic!()
    };
    assert_eq!((rolls[0].t, *from, *to, *quantity), (d(3), sym(&r, "ESH4"), sym(&r, "ESM4"), 2.0));
    // Both at the series' d2 price, where the gap is measured: ESH4's 4990
    // (the series less 4) and ESM4's 4994 (less 0), the raw prices.
    assert!(close(*from_price, 4990.0) && close(*to_price, 4994.0), "{} {}", from_price, to_price);
    assert!(close(*adjustment, -4.0));
    // A commission a fill, each 2.25 a contract: only they move cash.
    assert!(close(*commissions, 2.0 * 2.25 * 2.0) && close(rolls[0].cash_flow.unwrap(), -9.0));
    // The position is carried on the root.
    assert_eq!(r.final_positions.get(&sym(&r, "ES.v.0")), Some(&2.0));
    // The bar's P&L is the real contracts' less the commissions: ESH4 from
    // its d2 close to the roll's fill, ESM4 from the roll's fill to its d3
    // close. It is the series' change; the adjustment is not P&L.
    let nav = |t: i64| r.equity_curve.iter().find(|x| x.0 == t).unwrap().1;
    let real = 2.0 * 50.0 * ((from_price - 4990.0) + (5029.0 - to_price));
    assert!(close(nav(d(3)) - nav(d(2)), real - 9.0), "{} against {}", nav(d(3)) - nav(d(2)), real - 9.0);
    assert!(close(real, 2.0 * 50.0 * (5029.0 - 4994.0)));
    // `contract` names the contract the series holds.
    assert_eq!(contract_at, (Some(Value::Equity(sym(&r, "ESH4"))), Some(Value::Equity(sym(&r, "ESM4")))));
}

/// E-116: a roll between two barriers (a minute store's `continuous` rows
/// on a session clock) is the next barrier's roll, with its two fills and
/// two commissions. The executor rolled only where a row fell on the
/// barrier itself: a position held across some 34 quarterly rolls of a
/// minute store paid its entry's and exit's commissions alone.
#[test]
fn a_roll_between_barriers_is_charged_at_the_next() {
    let m = Market {
        instruments: vec![
            ("ES.v.0", "ES", 50.0, vec![5004.0, 5014.0, 4994.0, 5029.0, 5044.0]),
            ("ESH4", "ESH4", 50.0, vec![5000.0, 5010.0, 4990.0]),
            ("ESM4", "ESM4", 50.0, vec![5004.0, 5014.0, 4994.0, 5029.0, 5044.0]),
        ],
        // ESM4 from six hours after d2's bar: seen at d3's barrier.
        continuous: vec![
            ("ES.v.0", 0, "ESH4", 4.0),
            ("ES.v.0", 1, "ESH4", 4.0),
            ("ES.v.0", 2, "ESM4", 0.0),
            ("ES.v.0", 3, "ESM4", 0.0),
            ("ES.v.0", 4, "ESM4", 0.0),
        ],
        continuous_offset: 6 * 3_600_000_000_000,
        specs: vec![("ESH4", "ES", 9, 9, true), ("ESM4", "ES", 9, 9, true)],
        commission: 2.25,
        ..Market::default()
    };
    let svc = serve(&m);
    let prog = program(&buy_at_d0("ES"), &[("settlement", "none")]);
    let r = run(&prog, &svc);
    let rolls = events(&r, "roll");
    assert_eq!(rolls.len(), 1, "{:#?}", r.events);
    let EventKind::Roll { from, to, quantity, commissions, .. } = &rolls[0].kind else { panic!() };
    assert_eq!((rolls[0].t, *from, *to, *quantity), (d(3), sym(&r, "ESH4"), sym(&r, "ESM4"), 2.0));
    assert!(close(*commissions, 2.0 * 2.25 * 2.0));
    assert_eq!(r.final_positions.get(&sym(&r, "ES.v.0")), Some(&2.0));
}

#[test]
fn a_cash_settled_contract_held_at_its_last_trade_expires_at_its_final_settlement() {
    // ESH4 itself, last trade at d3, its final settlement 5026.5.
    let m = Market {
        instruments: vec![("ESH4", "ESH4", 50.0, ES[..6].to_vec())],
        settlement: vec![("ESH4", 3, 5026.5)],
        specs: vec![("ESH4", "ES", 3, 3, true)],
        commission: 0.0,
        ..Market::default()
    };
    let svc = serve(&m);
    let r = run(&program(&buy_at_d0("ESH4"), &[]), &svc);
    let x = events(&r, "contract_expiry");
    assert_eq!(x.len(), 1, "{:#?}", r.events);
    let EventKind::ContractExpiry { quantity, price, cash_settled } = &x[0].kind else { panic!() };
    assert_eq!((x[0].t, *quantity, *price, *cash_settled), (d(3), 2.0, 5026.5, true));
    // The variation from the last settlement (d2's close, 4990) to the final one.
    assert!(close(x[0].cash_flow.unwrap(), 2.0 * 50.0 * (5026.5 - 4990.0)), "{:?}", x[0].cash_flow);
    assert!(r.final_positions.is_empty(), "{:?}", r.final_positions);
    assert!(close(r.final_cash, 100_000.0 + 2.0 * 50.0 * (5026.5 - 5010.0)), "{}", r.final_cash);
    // Fully paid (`settlement none`): the position's value at its close
    // (no settlement price is read) to cash, the same P&L to the close.
    let r = run(&program(&buy_at_d0("ESH4"), &[("settlement", "none")]), &svc);
    let x = events(&r, "contract_expiry");
    assert!(x.len() == 1 && close(x[0].cash_flow.unwrap(), 2.0 * 50.0 * 5025.0), "{:#?}", x);
}

#[test]
fn a_futures_margin_call_liquidates_the_largest_requirement_first() {
    // ES (point value 50) and NQ (20), 3 and 2 contracts bought at d1;
    // published maintenance 12,000 and 18,000 a contract. At d2 ES falls
    // 200 points and NQ 400: equity 100,000 − 30,000 − 16,000 = 54,000
    // against 72,000. One NQ sold restores it.
    let m = Market {
        instruments: vec![
            ("ES.v.0", "ES", 50.0, vec![5000.0, 5000.0, 4800.0, 4800.0, 4800.0]),
            ("NQ.v.0", "NQ", 20.0, vec![18_000.0, 18_000.0, 17_600.0, 17_600.0, 17_600.0]),
        ],
        margin: (0..5).flat_map(|i| [("ES.v.0", i, 13_000.0, 12_000.0), ("NQ.v.0", i, 19_000.0, 18_000.0)]).collect(),
        commission: 0.0,
        ..Market::default()
    };
    let svc = serve(&m);
    let decides = "    decide(T, trade(A, Q)) :- universe(A, T), T = 2024-03-04, A = \"ES\", Q = 3 contracts.\n    decide(T, trade(A, qty)) :- universe(A, T), T = 2024-03-04, A = \"NQ\".";
    let prog = program(decides, &[("margin", "published"), ("margin_call", "liquidate")]);
    let (r, req) = run_with(&prog, &svc, |_, k| {
        let row = |k: &mut Kernel, t: i64| k.query("margin_req", t, &[]).unwrap().first().map(|tu| (tu[1].as_f64().unwrap(), tu[2].as_f64().unwrap()));
        (row(k, d(1)), row(k, d(2)))
    });
    // At d1, before the fall: 3 × 13,000 + 2 × 19,000 initial.
    assert_eq!(req.0, Some((3.0 * 13_000.0 + 2.0 * 19_000.0, 3.0 * 12_000.0 + 2.0 * 18_000.0)));
    let calls = events(&r, "margin_call");
    assert_eq!(calls.len(), 1, "{:#?}", r.events);
    assert_eq!(calls[0].t, d(2));
    let liq = events(&r, "forced_liquidation");
    assert_eq!(liq.len(), 1);
    let EventKind::ForcedLiquidation { requirement, equity, fills } = &liq[0].kind else { panic!() };
    assert!(close(*requirement, 72_000.0) && close(*equity, 54_000.0), "{} {}", requirement, equity);
    assert_eq!(fills.len(), 1);
    let EventKind::Fill { fill, .. } = &r.events[fills[0] as usize].kind else { panic!() };
    assert_eq!((fill.equity, fill.quantity, fill.forced), (sym(&r, "NQ.v.0"), -1.0, true));
    // After it: 3 × 12,000 + 1 × 18,000 = 54,000, covered.
    assert_eq!(req.1.map(|x| x.1), Some(54_000.0));
    assert_eq!(r.final_positions.get(&sym(&r, "NQ.v.0")), Some(&1.0));
    assert_eq!(r.final_positions.get(&sym(&r, "ES.v.0")), Some(&3.0));
    // The book's requirement is the published margins'.
    assert!(r.exposure.iter().filter(|x| x.gross > 0.0).all(|x| x.margin_source == "published"), "{:?}", r.exposure);
}
