//! The option model (K-5.5) on hand-computed cases (K-9.3, G8): a synthetic
//! chain over a synthetic underlying, ingested as the ORATS strikes layout
//! is (each row a call and a put, keyed by their OCC symbols, with the day's
//! chain), and run through the kernel over an in-process data service.
//!
//! The underlying `XYZ` trades on the ten weekdays from 2024-03-04 (d0) to
//! 2024-03-15 (d9). Unless a case says otherwise, an option fills at the
//! touch plus a quarter of the spread, with a commission of 0.65 USD a
//! contract, and a decision at a bar fills at the next.

#![allow(clippy::result_large_err)]

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::sync::Arc;

use abt_data::adapters::orats::Orats;
use abt_data::service::{inproc, DataService};
use abt_data::Store;
use abt_kernel::data::business_days;
use abt_kernel::kernel::{DataClient, Dataset, EventKind, ExecConfig, Kernel, Replay, RunEvent, RunResult, Sym, Value};
use abt_tests as common;

const XYZ: &str = "XYZ";

fn d(i: usize) -> i64 {
    business_days((2024, 3, 4), 10)[i]
}

fn day(s: &str) -> i64 {
    common::day(s)
}

/// One ORATS row: `ticker`'s call and put of `strike` expiring `expiry`,
/// quoted on `on` (bid, ask) with the underlying at `spot`. A side without
/// a quote is `None`.
#[derive(Clone)]
struct Row {
    ticker: &'static str,
    on: usize,
    expiry: &'static str,
    strike: f64,
    spot: f64,
    call: Option<(f64, f64)>,
    put: Option<(f64, f64)>,
    call_iv: Option<f64>,
}

fn row(on: usize, expiry: &'static str, strike: f64, spot: f64, call: Option<(f64, f64)>, put: Option<(f64, f64)>) -> Row {
    Row {
        ticker: XYZ,
        on,
        expiry,
        strike,
        spot,
        call,
        put,
        call_iv: None,
    }
}

fn csv(rows: &[Row]) -> String {
    let mut out = String::from(
        "ticker,tradeDate,expirDate,strike,stockPrice,callBidPrice,callAskPrice,callBidSize,callAskSize,callVolume,callOpenInterest,putBidPrice,putAskPrice,putBidSize,putAskSize,putVolume,putOpenInterest,callMidIv,putMidIv,smvVol,delta,gamma,theta,vega,rho\n",
    );
    let side = |q: Option<(f64, f64)>| q.map(|(b, a)| format!("{},{},10,10,100,500", b, a)).unwrap_or_else(|| ",,,,,".into());
    for r in rows {
        let on = abt_kernel::kernel::time::format_timestamp(d(r.on));
        let iv = r.call_iv.map(|v| v.to_string()).unwrap_or_default();
        writeln!(
            out,
            "{},{},{},{},{},{},{},{},,0.2,0.5,0.01,-0.05,0.1,0.02",
            r.ticker,
            on,
            r.expiry,
            r.strike,
            r.spot,
            side(r.call),
            side(r.put),
            iv
        )
        .unwrap();
    }
    out
}

/// The underlyings' daily bars: per ticker, its (open, close) on d0..d9.
struct Market {
    ds: Dataset,
    syms: BTreeMap<&'static str, Sym>,
}

fn market(names: &[(&'static str, [(f64, f64); 10])]) -> Market {
    let mut ds = Dataset::new();
    let mut syms = BTreeMap::new();
    for (name, bars) in names {
        let sym = ds.add_security(name, name, d(0), None);
        for (i, (open, close)) in bars.iter().enumerate() {
            common::add_bar(&mut ds, sym, d(i), *close, 1_000_000.0);
            // The open is its own (an AM settlement reads it).
            let opens = ds.facts.get_mut("open").unwrap();
            let last = opens.len() - 1;
            opens[last][2] = Value::Num(*open);
        }
        syms.insert(*name, sym);
    }
    ds.derive_tickers();
    Market { ds, syms }
}

/// XYZ flat at `close` every day, opening there too.
fn flat(close: f64) -> [(f64, f64); 10] {
    [(close, close); 10]
}

static TAG: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// A data service holding the market and the chain, every version reviewed.
fn serve(m: &Market, rows: &[Row], terms: &str) -> Arc<DataService> {
    serve_with_margins(m, rows, terms, &[])
}

/// `serve`, with published margins: (contract, day, initial, maintenance)
/// per contract, as `option.margin` rows of the vendor's keys.
fn serve_with_margins(m: &Market, rows: &[Row], terms: &str, margins: &[(String, usize, f64, f64)]) -> Arc<DataService> {
    common::init();
    let n = TAG.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    let tag = format!("options-{}-{}", std::process::id(), n);
    let mut store = Store::temporary(&tag).unwrap();
    let mut attributes = BTreeMap::new();
    attributes.insert("reviewed".to_string(), "complete".to_string());
    attributes.insert("calendar".to_string(), "1d".to_string());
    abt::ingest_dataset(&mut store, &m.ds, "equity", &attributes).unwrap();
    let dir = std::env::temp_dir().join(format!("abt-{}-orats", tag));
    std::fs::create_dir_all(&dir).unwrap();
    let mut by_ticker: BTreeMap<&str, Vec<Row>> = BTreeMap::new();
    for r in rows {
        by_ticker.entry(r.ticker).or_default().push(r.clone());
    }
    for (ticker, rows) in &by_ticker {
        std::fs::write(dir.join(format!("{}.csv", ticker)), csv(rows)).unwrap();
    }
    let mut settings = BTreeMap::new();
    settings.insert("files".to_string(), dir.to_string_lossy().to_string());
    if !terms.is_empty() {
        settings.insert("terms".to_string(), terms.to_string());
    }
    let o = Orats::from_settings(&settings).unwrap();
    let (fetched, _) = o.read(o.options()).unwrap();
    fetched.ingest(&mut store, "orats", "orats", &attributes).unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    if !margins.is_empty() {
        use arrow::array::{Float64Array, RecordBatch, StringArray, TimestampNanosecondArray};
        use arrow::datatypes::{DataType, Field, Schema, TimeUnit};
        let ts = DataType::Timestamp(TimeUnit::Nanosecond, None);
        let schema = Schema::new(vec![
            Field::new("A", DataType::Utf8, false),
            Field::new("T", ts.clone(), false),
            Field::new("Initial", DataType::Float64, true),
            Field::new("Maintenance", DataType::Float64, true),
            Field::new("r", ts, false),
        ]);
        let batch = RecordBatch::try_new(
            Arc::new(schema),
            vec![
                Arc::new(StringArray::from(margins.iter().map(|x| x.0.clone()).collect::<Vec<_>>())),
                Arc::new(TimestampNanosecondArray::from(margins.iter().map(|x| d(x.1)).collect::<Vec<_>>())),
                Arc::new(Float64Array::from(margins.iter().map(|x| x.2).collect::<Vec<_>>())),
                Arc::new(Float64Array::from(margins.iter().map(|x| x.3).collect::<Vec<_>>())),
                Arc::new(TimestampNanosecondArray::from(margins.iter().map(|x| d(x.1)).collect::<Vec<_>>())),
            ],
        )
        .unwrap();
        abt_data::adapter::ingest_keyed(&mut store, "orats", &[], &[], &[("option.margin".to_string(), batch)], "orats", &attributes).unwrap();
    }
    DataService::new(store)
}

/// An option program on XYZ's chain: its decide rules and execution fields
/// over the defaults.
fn program(decides: &str, exec: &[(&str, &str)]) -> abt_kernel::check::Program {
    program_on(XYZ, "", decides, exec)
}

/// An option program on `underlying`'s chain, with parameters.
fn program_on(underlying: &str, params: &str, decides: &str, exec: &[(&str, &str)]) -> abt_kernel::check::Program {
    program_with_rules(underlying, params, "", decides, exec)
}

/// An option program on `underlying`'s chain, with parameters and rules.
fn program_with_rules(underlying: &str, params: &str, rules: &str, decides: &str, exec: &[(&str, &str)]) -> abt_kernel::check::Program {
    let mut fields: Vec<(String, String)> = [
        ("price", "close"),
        ("option_mark", "mid"),
        ("option_fill", "touch + 0.25 spread"),
        ("greeks", "model"),
        ("orders", "market"),
        ("lot", "whole"),
        ("capital", "100_000 USD fixed"),
        ("commission", "per_contract 0.65 USD"),
        ("slippage", "none"),
        ("impact", "none"),
        ("participation", "none"),
        ("financing", "none"),
        ("leverage", "unconstrained"),
        ("actions", "apply"),
        ("delisting", "last_price"),
        ("exercise", "{ itm_at_expiry; early_short never; early_long decide; atm_tolerance 0 }"),
        ("oversize", "allow"),
        ("ruin", "halt"),
        ("numerics", "two_pass"),
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
        name: "opt".into(),
        class: "option".into(),
        needs: format!("need bars @1d   need actions   need chain of \"{}\" within 0d..60d and 0.5..2.0 spot close", underlying),
        uses: "calendar, book".into(),
        universe: "universe(A, T) :- chain(_, T, A).".into(),
        params: params.into(),
        rules: rules.into(),
        decides: decides.into(),
        execution,
        ..Default::default()
    }
    .render();
    common::program(&src, "opt")
}

fn run(prog: &abt_kernel::check::Program, svc: &Arc<DataService>) -> RunResult {
    let client = DataClient::connect(inproc(svc)).unwrap();
    let cfg = ExecConfig::from_program(prog);
    abt_kernel::kernel::run_on(prog, None, &client, cfg, &Replay::default()).unwrap_or_else(|e| panic!("{}", e))
}

/// A run and a look at the kernel at its end.
fn run_with<T: Send>(prog: &abt_kernel::check::Program, svc: &Arc<DataService>, f: impl for<'k> FnOnce(&RunResult, &mut Kernel<'k>) -> T + Send) -> (RunResult, T) {
    let client = DataClient::connect(inproc(svc)).unwrap();
    let cfg = ExecConfig::from_program(prog);
    abt_kernel::kernel::run_keeping(prog, None, &client, cfg, &Replay::default(), |r, k| Ok(f(r, k))).unwrap_or_else(|e| panic!("{}", e))
}

/// The OCC symbol of an XYZ contract.
fn occ(expiry: &str, right: char, strike: f64) -> String {
    abt_data::options::occ_symbol(XYZ, day(expiry), if right == 'C' { "call" } else { "put" }, strike)
}

fn sym(r: &RunResult, name: &str) -> Sym {
    r.symbols.iter().position(|s| s == name).unwrap_or_else(|| panic!("no {} in {:?}", name, r.symbols)) as Sym
}

fn events<'r>(r: &'r RunResult, kind: &str) -> Vec<&'r RunEvent> {
    r.events.iter().filter(|e| e.kind.name() == kind).collect()
}

fn close(a: f64, b: f64) -> bool {
    (a - b).abs() < 1e-9 * (1.0 + b.abs())
}

/// Buy (or sell) `q` contracts of `occ` at d0.
fn trade_at_d0(occ: &str, q: i64) -> String {
    format!("    decide(T, trade(A, {} contracts)) :- universe(A, T), T = 2024-03-04, A = \"{}\".", q, occ)
}

#[test]
fn a_long_call_in_the_money_at_expiry_is_exercised_physically() {
    // Bought 2 calls of 100 expiring Friday 2024-03-08 (d4); XYZ closes at
    // 104 there: the calls are exercised, 200 shares bought at the strike.
    let mut bars = flat(100.0);
    bars[4] = (103.0, 104.0);
    let m = market(&[(XYZ, bars)]);
    let rows: Vec<Row> = (0..5).map(|i| row(i, "2024-03-08", 100.0, bars[i].1, Some((2.0, 2.2)), Some((1.5, 1.7)))).collect();
    let svc = serve(&m, &rows, "");
    let call = occ("2024-03-08", 'C', 100.0);
    let r = run(&program(&trade_at_d0(&call, 2), &[]), &svc);
    let fills = events(&r, "fill");
    assert_eq!(fills.len(), 2, "{:#?}", r.events);
    // The buy at d1: the ask plus a quarter of the spread, 2.25, with 1.30 of commission.
    let EventKind::Fill { fill, .. } = &fills[0].kind else { panic!() };
    assert_eq!((fill.t, fill.quantity), (d(1), 2.0));
    assert!(close(fill.price, 2.25) && close(fill.commission, 1.3), "{:?}", fill);
    // The spread paid against the mid, 2.10: 0.15 a unit, 30 on the order.
    assert!(close(fill.spread, 30.0), "{:?}", fill);
    // At the expiry close: exercised, with the underlying bought at the strike.
    let ex = events(&r, "exercise");
    assert_eq!(ex.len(), 1, "{:#?}", r.events);
    let EventKind::Exercise {
        quantity,
        position,
        underlying,
        intrinsic,
        cash_settled,
        cause,
    } = &ex[0].kind
    else {
        panic!()
    };
    assert_eq!((ex[0].t, *quantity, *position, *cash_settled, cause.as_str()), (d(4), 2.0, 0.0, false, "expiry"));
    assert!(close(*underlying, 104.0) && close(*intrinsic, 4.0));
    let EventKind::Fill { fill, .. } = &fills[1].kind else { panic!() };
    assert_eq!((fill.t, fill.equity, fill.quantity, fill.price), (d(4), sym(&r, XYZ), 200.0, 100.0));
    assert!(close(fills[1].cash_flow.unwrap(), -20_000.0));
    // The option position is gone; the book holds the shares.
    assert_eq!(r.final_positions.get(&sym(&r, &call)), None);
    assert_eq!(r.final_positions.get(&sym(&r, XYZ)), Some(&200.0));
    assert!(close(r.final_cash, 100_000.0 - 450.0 - 1.3 - 20_000.0), "{}", r.final_cash);
}

#[test]
fn a_short_put_in_the_money_at_expiry_is_assigned_the_underlying() {
    // Sold 1 put of 100 at d0; XYZ closes at 96 on the expiry day: assigned,
    // 100 shares bought at the strike.
    let mut bars = flat(100.0);
    bars[4] = (97.0, 96.0);
    let m = market(&[(XYZ, bars)]);
    let rows: Vec<Row> = (0..5).map(|i| row(i, "2024-03-08", 100.0, bars[i].1, Some((2.0, 2.2)), Some((1.5, 1.7)))).collect();
    let svc = serve(&m, &rows, "");
    let put = occ("2024-03-08", 'P', 100.0);
    let r = run(&program(&trade_at_d0(&put, -1), &[]), &svc);
    // The sale at d1: the bid less a quarter of the spread, 1.45.
    let fills = events(&r, "fill");
    let EventKind::Fill { fill, .. } = &fills[0].kind else { panic!() };
    assert!(close(fill.price, 1.45) && fill.quantity == -1.0, "{:?}", fill);
    let a = events(&r, "assignment");
    assert_eq!(a.len(), 1, "{:#?}", r.events);
    let EventKind::Assignment {
        quantity,
        position,
        underlying,
        intrinsic,
        cash_settled,
        cause,
    } = &a[0].kind
    else {
        panic!()
    };
    assert_eq!((a[0].t, *quantity, *position, *cash_settled, cause.as_str()), (d(4), 1.0, 0.0, false, "expiry"));
    assert!(close(*underlying, 96.0) && close(*intrinsic, 4.0));
    let EventKind::Fill { fill, .. } = &fills[1].kind else { panic!() };
    assert_eq!((fill.equity, fill.quantity, fill.price), (sym(&r, XYZ), 100.0, 100.0));
    assert!(close(fills[1].cash_flow.unwrap(), -10_000.0));
    assert_eq!(r.final_positions.get(&sym(&r, XYZ)), Some(&100.0));
    assert!(close(r.final_cash, 100_000.0 + 145.0 - 0.65 - 10_000.0), "{}", r.final_cash);
}

#[test]
fn a_long_put_on_a_cash_settled_index_is_exercised_at_the_am_print() {
    // IDX: European, cash-settled, AM. Its expiry day opens at 97 and closes
    // at 99: the put of 100 is settled at the open, 3 a unit in cash.
    let mut bars = flat(100.0);
    bars[4] = (97.0, 99.0);
    let m = market(&[("IDX", bars)]);
    let rows: Vec<Row> = (0..4)
        .map(|i| Row {
            ticker: "IDX",
            ..row(i, "2024-03-08", 100.0, bars[i].1, Some((2.0, 2.2)), Some((1.5, 1.7)))
        })
        .collect();
    let svc = serve(&m, &rows, "IDX=european:cash:am");
    let put = abt_data::options::occ_symbol("IDX", day("2024-03-08"), "put", 100.0);
    let decide = format!("    decide(T, trade(A, 1 contracts)) :- universe(A, T), T = 2024-03-04, A = \"{}\".", put);
    let r = run(&program_on("IDX", "", &decide, &[]), &svc);
    let ex = events(&r, "exercise");
    assert_eq!(ex.len(), 1, "{:#?}", r.events);
    let EventKind::Exercise {
        quantity,
        position,
        underlying,
        intrinsic,
        cash_settled,
        ..
    } = &ex[0].kind
    else {
        panic!()
    };
    assert_eq!((ex[0].t, *quantity, *position, *cash_settled), (d(4), 1.0, 0.0, true));
    assert!(close(*underlying, 97.0) && close(*intrinsic, 3.0));
    assert!(close(ex[0].cash_flow.unwrap(), 300.0));
    // No delivery: the only fill is the purchase, at 1.75.
    assert_eq!(events(&r, "fill").len(), 1);
    assert!(r.final_positions.is_empty(), "{:?}", r.final_positions);
    assert!(close(r.final_cash, 100_000.0 - 175.0 - 0.65 + 300.0), "{}", r.final_cash);
}

#[test]
fn an_option_out_of_the_money_at_expiry_expires_worthless_on_either_side() {
    // A long call of 105 and a short put of 95 with XYZ at 100 at expiry.
    let m = market(&[(XYZ, flat(100.0))]);
    let rows: Vec<Row> = (0..5)
        .flat_map(|i| {
            [
                row(i, "2024-03-08", 105.0, 100.0, Some((0.5, 0.6)), Some((5.0, 5.2))),
                row(i, "2024-03-08", 95.0, 100.0, Some((5.0, 5.2)), Some((0.4, 0.5))),
            ]
        })
        .collect();
    let svc = serve(&m, &rows, "");
    let (call, put) = (occ("2024-03-08", 'C', 105.0), occ("2024-03-08", 'P', 95.0));
    let decides = format!("{}\n{}", trade_at_d0(&call, 1), trade_at_d0(&put, -1));
    let r = run(&program(&decides, &[]), &svc);
    let gone = events(&r, "option_expiry");
    assert_eq!(gone.len(), 2, "{:#?}", r.events);
    for e in &gone {
        assert_eq!((e.t, e.cash_flow), (d(4), None));
    }
    assert!(events(&r, "exercise").is_empty() && events(&r, "assignment").is_empty());
    assert!(r.final_positions.is_empty(), "{:?}", r.final_positions);
    // Only the trades moved cash: the call bought at 0.625, the put sold at 0.375.
    assert!(close(r.final_cash, 100_000.0 - 62.5 + 37.5 - 1.3), "{}", r.final_cash);
}

#[test]
fn an_option_at_the_money_within_the_tolerance_expires() {
    // XYZ at 100.4 at expiry: the call of 100 is 0.4 in the money, within
    // half a percent of its strike, so it expires; at 100.6 it is exercised.
    for (close_at, exercised) in [(100.4, false), (100.6, true)] {
        let mut bars = flat(100.0);
        bars[4] = (close_at, close_at);
        let m = market(&[(XYZ, bars)]);
        let rows: Vec<Row> = (0..5).map(|i| row(i, "2024-03-08", 100.0, bars[i].1, Some((1.0, 1.2)), Some((1.0, 1.2)))).collect();
        let svc = serve(&m, &rows, "");
        let call = occ("2024-03-08", 'C', 100.0);
        let exercise = ("exercise", "{ itm_at_expiry; early_short never; early_long never; atm_tolerance 0.005 }");
        let r = run(&program(&trade_at_d0(&call, 1), &[exercise]), &svc);
        assert_eq!(events(&r, "exercise").len(), exercised as usize, "{:#?}", r.events);
        assert_eq!(events(&r, "option_expiry").len(), !exercised as usize, "{:#?}", r.events);
    }
}

/// A covered call: 100 shares of XYZ and a short American call of 95 to
/// 2024-03-15, both bought or sold at d0. A dividend of 1.00 is announced at
/// d0 to go ex at d6 and be paid at d8. The call is quoted at 5.20 / 5.40, so
/// its extrinsic value is 0.30 with XYZ at 100.
fn covered_call(early_short: &str) -> RunResult {
    let mut m = market(&[(XYZ, flat(100.0))]);
    let x = m.syms[XYZ];
    m.ds.add("dividend", vec![Value::Equity(x), Value::Time(d(0)), Value::Time(d(6)), Value::Time(d(8)), Value::Num(1.0)]);
    let rows: Vec<Row> = (0..10).map(|i| row(i, "2024-03-15", 95.0, 100.0, Some((5.2, 5.4)), Some((0.1, 0.2)))).collect();
    let svc = serve(&m, &rows, "");
    let call = occ("2024-03-15", 'C', 95.0);
    let decides = format!("{}\n    decide(T, trade(U, 100 contracts)) :- close(U, T, _), T = 2024-03-04, U = \"XYZ\".", trade_at_d0(&call, -1));
    let exercise = format!("{{ itm_at_expiry; early_short {}; early_long never; atm_tolerance 0 }}", early_short);
    run(&program(&decides, &[("exercise", &exercise)]), &svc)
}

#[test]
fn a_short_call_is_assigned_early_before_the_ex_date_when_its_extrinsic_value_is_below_the_dividend() {
    let r = covered_call("when extrinsic < DIVIDEND");
    // Assigned at the close of d5, the bar before the ex-date: the shares
    // are delivered at the strike, so no dividend is due on them.
    let a = events(&r, "assignment");
    assert_eq!(a.len(), 1, "{:#?}", r.events);
    let EventKind::Assignment { quantity, cause, .. } = &a[0].kind else { panic!() };
    assert_eq!((a[0].t, *quantity, cause.as_str()), (d(5), 1.0, "early"));
    let delivery = events(&r, "fill").into_iter().find(|e| e.t == d(5)).expect("the delivery");
    let EventKind::Fill { fill, .. } = &delivery.kind else { panic!() };
    assert_eq!((fill.equity, fill.quantity, fill.price), (sym(&r, XYZ), -100.0, 95.0));
    assert!(events(&r, "dividend_declared").is_empty(), "{:#?}", r.events);
    assert!(r.final_positions.is_empty(), "{:?}", r.final_positions);
}

#[test]
fn with_early_short_never_the_call_is_assigned_at_expiry_only() {
    let r = covered_call("never");
    let a = events(&r, "assignment");
    assert_eq!(a.len(), 1, "{:#?}", r.events);
    let EventKind::Assignment { cause, .. } = &a[0].kind else { panic!() };
    assert_eq!((a[0].t, cause.as_str()), (d(9), "expiry"));
    // The shares were held over the ex-date: the dividend is declared on them.
    let div = events(&r, "dividend_declared");
    assert_eq!(div.len(), 1, "{:#?}", r.events);
    let EventKind::DividendDeclared { shares, receivable, .. } = &div[0].kind else { panic!() };
    assert_eq!((div[0].t, *shares, *receivable), (d(6), 100.0, 100.0));
}

#[test]
fn an_exercise_decision_on_a_long_exercises_at_that_bars_close() {
    // A long American call of 95, exercised by the program at d3.
    let m = market(&[(XYZ, flat(100.0))]);
    let rows: Vec<Row> = (0..10).map(|i| row(i, "2024-03-15", 95.0, 100.0, Some((5.2, 5.4)), Some((0.1, 0.2)))).collect();
    let svc = serve(&m, &rows, "");
    let call = occ("2024-03-15", 'C', 95.0);
    let decides = format!("{}\n    decide(T, exercise(A, 1 contracts)) :- held(A, T, _), T = 2024-03-07.", trade_at_d0(&call, 1));
    let r = run(&program(&decides, &[]), &svc);
    let ex = events(&r, "exercise");
    assert_eq!(ex.len(), 1, "{:#?}", r.events);
    let EventKind::Exercise { quantity, position, cause, .. } = &ex[0].kind else { panic!() };
    assert_eq!((ex[0].t, *quantity, *position, cause.as_str()), (d(3), 1.0, 0.0, "decision"));
    // The order the decision was sent as is the exercise's.
    let sent = events(&r, "order_sent").into_iter().find(|e| e.t == d(3)).expect("the exercise's order");
    assert_eq!(ex[0].order_id, sent.order_id);
    let delivery = events(&r, "fill").into_iter().find(|e| e.t == d(3)).expect("the delivery");
    let EventKind::Fill { fill, .. } = &delivery.kind else { panic!() };
    assert_eq!((fill.equity, fill.quantity, fill.price), (sym(&r, XYZ), 100.0, 95.0));
    assert!(close(delivery.cash_flow.unwrap(), -9_500.0));
    // `early_long never` drops it.
    let never = ("exercise", "{ itm_at_expiry; early_short never; early_long never; atm_tolerance 0 }");
    let r = run(&program(&decides, &[never]), &svc);
    // (The call is exercised only at its expiry, in the money.)
    let ex = events(&r, "exercise");
    assert!(ex.len() == 1 && ex[0].t == d(9), "{:#?}", ex);
    assert!(
        r.events.iter().any(|e| matches!(&e.kind, EventKind::Dropped { reason, .. } if reason.contains("early_long never"))),
        "{:#?}",
        r.events
    );
}

/// A vertical spread decided at d0 as one combo: the call of 100 bought
/// (quoted 3.00 / 3.40) and the call of 105 sold (1.00 / 1.20), both to
/// 2024-03-15, with `order` after the legs.
fn vertical(order: &str, quoted_short_leg: bool) -> RunResult {
    let m = market(&[(XYZ, flat(100.0))]);
    let rows: Vec<Row> = (0..10)
        .flat_map(|i| {
            let mut v = vec![row(i, "2024-03-15", 100.0, 100.0, Some((3.0, 3.4)), Some((2.8, 3.2)))];
            if quoted_short_leg || i == 0 {
                v.push(row(i, "2024-03-15", 105.0, 100.0, Some((1.0, 1.2)), Some((5.8, 6.2))));
            }
            v
        })
        .collect();
    let svc = serve(&m, &rows, "");
    let (long, short) = (occ("2024-03-15", 'C', 100.0), occ("2024-03-15", 'C', 105.0));
    let decides = format!(
        "    decide(T, combo([leg(A, qty), leg(B, Q)]{})) :- universe(A, T), universe(B, T), T = 2024-03-04, A = \"{}\", B = \"{}\", Q = -qty.",
        order, long, short
    );
    run(&program_on(XYZ, "    qty : Quantity<Contracts> = 1 contracts", &decides, &[]), &svc)
}

#[test]
fn a_combo_fills_every_leg_at_one_barrier_under_one_order_id() {
    let r = vertical("", true);
    let fills = events(&r, "fill");
    assert_eq!(fills.len(), 2, "{:#?}", r.events);
    // The long leg at the ask plus a quarter spread, the short at the bid
    // less one: 3.50 and 0.95, a net debit of 2.55.
    let legs: Vec<(f64, f64)> = fills
        .iter()
        .map(|e| match &e.kind {
            EventKind::Fill { fill, .. } => (fill.quantity, fill.price),
            _ => unreachable!(),
        })
        .collect();
    assert!(
        legs.iter().any(|(q, p)| *q == 1.0 && close(*p, 3.5)) && legs.iter().any(|(q, p)| *q == -1.0 && close(*p, 0.95)),
        "{:?}",
        legs
    );
    assert!(fills.iter().all(|e| e.t == d(1)));
    assert_eq!(fills[0].order_id, fills[1].order_id);
    assert!(fills[0].order_id.is_some());
    assert!(close(r.final_cash, 100_000.0 - 350.0 + 95.0 - 1.3), "{}", r.final_cash);
}

#[test]
fn a_combo_limit_fills_at_or_better_than_its_net_and_otherwise_works_until_its_time_in_force() {
    // A net of 2.55 against a limit of 2.60: filled at the first bar.
    let r = vertical(", limit(2.60 USD/share)", true);
    assert_eq!(events(&r, "fill").len(), 2, "{:#?}", r.events);
    // Against 2.50: working at d1, expired at d2 (a day order), nothing filled.
    let r = vertical(", limit(2.50 USD/share)", true);
    assert!(events(&r, "fill").is_empty(), "{:#?}", r.events);
    let expired = events(&r, "expired_order");
    assert_eq!(expired.len(), 1, "{:#?}", r.events);
    assert_eq!(expired[0].t, d(2));
}

#[test]
fn a_combo_with_a_leg_unquoted_is_dropped_naming_the_leg() {
    let r = vertical("", false);
    assert!(events(&r, "fill").is_empty(), "{:#?}", r.events);
    let short = occ("2024-03-15", 'C', 105.0);
    let dropped: Vec<&str> = r
        .events
        .iter()
        .filter_map(|e| match &e.kind {
            EventKind::Dropped { reason, .. } => Some(reason.as_str()),
            _ => None,
        })
        .collect();
    assert!(dropped.len() == 1 && dropped[0].contains(&format!("leg {}", short)) && dropped[0].contains("no quote"), "{:?}", dropped);
}

/// A call of 100 to 2024-03-15 held from d0 through an action on XYZ at d5;
/// the run and the call's `spec` rows at d4 and d5 (strike, multiplier).
fn adjusted(action: impl Fn(&mut Market), after: f64) -> (RunResult, (f64, f64), (f64, f64)) {
    let mut bars = flat(100.0);
    for b in bars.iter_mut().skip(5) {
        *b = (after, after);
    }
    let mut m = market(&[(XYZ, bars)]);
    action(&mut m);
    let rows: Vec<Row> = (0..10).map(|i| row(i, "2024-03-15", 100.0, bars[i].1, Some((3.0, 3.4)), Some((2.8, 3.2)))).collect();
    let svc = serve(&m, &rows, "");
    let call = occ("2024-03-15", 'C', 100.0);
    let prog = program(&trade_at_d0(&call, 1), &[]);
    let (r, specs) = run_with(&prog, &svc, |r, k| {
        let a = sym(r, &call);
        let spec_at = |k: &mut Kernel, t: i64| -> (f64, f64) {
            let rows = k.query("spec", t, &[]).unwrap();
            let tu = rows.iter().find(|tu| tu[0] == Value::Equity(a)).expect("the call's spec");
            (tu[4].as_f64().unwrap(), tu[8].as_f64().unwrap())
        };
        (spec_at(k, d(4)), spec_at(k, d(5)))
    });
    (r, specs.0, specs.1)
}

fn adjustment(r: &RunResult) -> (i64, f64, f64, f64, String) {
    let a = events(r, "adjustment");
    assert_eq!(a.len(), 1, "{:#?}", r.events);
    let EventKind::Adjustment { after, position, cause, .. } = &a[0].kind else { panic!() };
    (a[0].t, *position, after.strike, after.multiplier, cause.clone())
}

#[test]
fn a_two_for_one_split_doubles_the_contracts_and_halves_the_strike() {
    let (r, before, after) = adjusted(|m| m.ds.add("split", vec![Value::Equity(m.syms[XYZ]), Value::Time(d(5)), Value::Num(2.0)]), 50.0);
    let (t, position, strike, multiplier, cause) = adjustment(&r);
    assert_eq!((t, position, strike, multiplier, cause.as_str()), (d(5), 2.0, 50.0, 100.0, "split"));
    // `spec` reflects it from the ex-date.
    assert_eq!(before, (100.0, 100.0));
    assert_eq!(after, (50.0, 100.0));
}

#[test]
fn a_three_for_two_split_takes_the_multiplier_to_150_and_the_strike_to_two_thirds() {
    let (r, _, after) = adjusted(|m| m.ds.add("split", vec![Value::Equity(m.syms[XYZ]), Value::Time(d(5)), Value::Num(1.5)]), 100.0 / 1.5);
    let (t, position, strike, multiplier, _) = adjustment(&r);
    assert_eq!((t, position, multiplier), (d(5), 1.0, 150.0));
    assert!(close(strike, 100.0 * 2.0 / 3.0), "{}", strike);
    assert!(close(after.0, 100.0 * 2.0 / 3.0) && after.1 == 150.0, "{:?}", after);
}

#[test]
fn a_special_dividend_lowers_the_strike_by_its_amount() {
    let (r, _, after) = adjusted(
        |m| m.ds.add("special_dividend", vec![Value::Equity(m.syms[XYZ]), Value::Time(d(1)), Value::Time(d(5)), Value::Num(2.0)]),
        98.0,
    );
    let (t, position, strike, multiplier, cause) = adjustment(&r);
    assert_eq!((t, position, strike, multiplier, cause.as_str()), (d(5), 1.0, 98.0, 100.0, "special_dividend"));
    assert_eq!(after, (98.0, 100.0));
}

#[test]
fn a_delisted_underlying_forces_the_option_to_expire_at_intrinsic() {
    // XYZ is delisted at d5 at 108: the call of 100 is settled at 8 in cash.
    let mut bars = flat(100.0);
    bars[5] = (108.0, 108.0);
    let mut m = market(&[(XYZ, bars)]);
    let x = m.syms[XYZ];
    let reason = m.ds.labels.intern("acquisition");
    m.ds.add("delisted", vec![Value::Equity(x), Value::Time(d(5)), Value::Label(reason)]);
    let rows: Vec<Row> = (0..6).map(|i| row(i, "2024-03-15", 100.0, bars[i].1, Some((3.0, 3.4)), Some((2.8, 3.2)))).collect();
    let svc = serve(&m, &rows, "");
    let call = occ("2024-03-15", 'C', 100.0);
    let r = run(&program(&trade_at_d0(&call, 1), &[]), &svc);
    let ex = events(&r, "exercise");
    assert_eq!(ex.len(), 1, "{:#?}", r.events);
    let EventKind::Exercise { cash_settled, cause, intrinsic, .. } = &ex[0].kind else { panic!() };
    assert_eq!((ex[0].t, *cash_settled, cause.as_str()), (d(5), true, "delisted"));
    assert!(close(*intrinsic, 8.0) && close(ex[0].cash_flow.unwrap(), 800.0));
    assert!(r.warnings.iter().any(|w| w.message.contains("delisted")), "{:?}", r.warnings);
}

/// A European call of 100 to 2024-04-19 quoted at the model's price at a
/// volatility of 25 % (bid and ask a nickel either side), its data row's IV
/// 25 %; and a call of 90 quoted below its intrinsic value.
fn greeks_at_d0(greeks: &str) -> (Option<f64>, Option<f64>) {
    use abt_kernel::functions::{bsm, Right};
    let m = market(&[(XYZ, flat(100.0))]);
    // τ from the end of d0 to the end of the expiry day, 46 days (K-2.5).
    let tau = 46.0 / 365.0;
    let p = bsm::price(100.0, 100.0, tau, 0.0, 0.0, 0.25, Right::Call);
    let rows = vec![
        Row {
            call_iv: Some(0.25),
            ..row(0, "2024-04-19", 100.0, 100.0, Some((p - 0.05, p + 0.05)), Some((p - 0.05, p + 0.05)))
        },
        Row {
            call_iv: Some(0.3),
            ..row(0, "2024-04-19", 90.0, 100.0, Some((9.0, 9.4)), Some((0.1, 0.2)))
        },
    ];
    let svc = serve(&m, &rows, "XYZ=european:physical:pm");
    // (A program decides: on no bar here.)
    let prog = program("    decide(T, trade(A, 1 contracts)) :- universe(A, T), T = 2024-01-02.", &[("greeks", greeks)]);
    let (good, bad) = (occ("2024-04-19", 'C', 100.0), occ("2024-04-19", 'C', 90.0));
    run_with(&prog, &svc, |r, k| {
        let rows = k.query("greeks", d(0), &[]).unwrap();
        let iv = |name: &str| rows.iter().find(|tu| tu[0] == Value::Equity(sym(r, name))).map(|tu| tu[2].as_f64().unwrap());
        (iv(&good), iv(&bad))
    })
    .1
}

#[test]
fn the_models_greeks_reproduce_the_datas_iv_and_are_absent_outside_the_bounds() {
    let (data_good, data_bad) = greeks_at_d0("data");
    assert_eq!(data_good, Some(0.25));
    assert_eq!(data_bad, Some(0.3), "the data's row is the data's");
    let (model_good, model_bad) = greeks_at_d0("model");
    assert!(model_good.is_some_and(|v| (v - 0.25).abs() < 1e-6), "{:?}", model_good);
    // A mid of 9.20 below the intrinsic 10: no volatility prices it (⊥).
    assert_eq!(model_bad, None);
}

#[test]
fn an_option_is_marked_at_its_mid_half_its_ask_one_sided_its_last_mark_stale_or_the_model() {
    // A call of 100 to 2024-04-19 bought at d0: quoted both ways at d1, with
    // no bid at d2, not at all at d3.
    let m = market(&[(XYZ, flat(100.0))]);
    let rows = vec![
        row(0, "2024-04-19", 100.0, 100.0, Some((3.0, 3.4)), Some((2.8, 3.2))),
        row(1, "2024-04-19", 100.0, 100.0, Some((3.0, 3.4)), Some((2.8, 3.2))),
        row(2, "2024-04-19", 100.0, 100.0, Some((0.0, 3.4)), Some((2.8, 3.2))),
    ];
    let svc = serve(&m, &rows, "");
    let call = occ("2024-04-19", 'C', 100.0);
    let source = |r: &RunResult, t: i64| -> (f64, String) {
        let h = r.holdings.iter().find(|h| h.t == t && h.instrument == sym(r, &call)).expect("the holding");
        (h.mark.unwrap(), h.mark_source.clone())
    };
    let r = run(&program(&trade_at_d0(&call, 1), &[]), &svc);
    assert_eq!(source(&r, d(1)), (3.2, "mid".to_string()));
    assert_eq!(source(&r, d(2)), (1.7, "one_sided".to_string()));
    assert_eq!(source(&r, d(3)), (1.7, "stale".to_string()));
    // Under `option_mark model`: priced at the volatility the last mid implied.
    let r = run(&program(&trade_at_d0(&call, 1), &[("option_mark", "model")]), &svc);
    let (p1, s1) = source(&r, d(1));
    assert!(close(p1, 3.2) && s1 == "model", "{} {}", p1, s1);
    let (p3, s3) = source(&r, d(3));
    assert_eq!(s3, "model");
    // Two days nearer expiry at the same volatility and spot: worth less.
    assert!(p3 < 1.8 && p3 > 0.0, "{}", p3);
}

#[test]
fn a_rule_prices_an_option_with_the_functions_of_f() {
    use abt_kernel::functions::{bsm, Right};
    let m = market(&[(XYZ, flat(100.0))]);
    let rows = vec![row(0, "2024-04-19", 100.0, 100.0, Some((3.0, 3.4)), Some((2.8, 3.2)))];
    let svc = serve(&m, &rows, "XYZ=european:physical:pm");
    let src_rules = "    rel fair(A: Option, @T, V: Price<USD>)\n    fair(A, T, V) :- spec(A, T, U, R, K, _, _, _, _), close(U, T, S), V = bs_price(S, K, 46d, 0, 0, 0.25, R).\n    rel vol(A: Option, @T, V: Scalar)\n    vol(A, T, V) :- spec(A, T, U, R, K, _, Y, _, _), close(U, T, S), quote(A, T, B, Q, _, _, _, _, _), V = implied_vol((B + Q) / 2, S, K, 46d, 0, 0, R, Y).";
    let prog = program_with_rules(XYZ, "", src_rules, "    decide(T, trade(A, 1 contracts)) :- universe(A, T), T = 2024-01-02.", &[]);
    let call = occ("2024-04-19", 'C', 100.0);
    let (_, (fair, vol)) = run_with(&prog, &svc, |r, k| {
        let a = Value::Equity(sym(r, &call));
        let fair = k.query("fair", d(0), &[]).unwrap().into_iter().find(|tu| tu[0] == a).map(|tu| tu[2].as_f64().unwrap());
        let vol = k.query("vol", d(0), &[]).unwrap().into_iter().find(|tu| tu[0] == a).map(|tu| tu[2].as_f64().unwrap());
        (fair, vol)
    });
    assert_eq!(fair, Some(bsm::price(100.0, 100.0, 46.0 / 365.0, 0.0, 0.0, 0.25, Right::Call)));
    let v = vol.expect("the mid's volatility");
    assert!(close(bsm::price(100.0, 100.0, 46.0 / 365.0, 0.0, 0.0, v, Right::Call), 3.2), "{}", v);
}

/// A `margin_by` row as the tests read it: contract, maintenance, source.
type MarginRow = (String, f64, String);

/// The margin of a book at d1 under `margin`: a decided book of XYZ
/// options (`decides`), quoted every day at the model's prices at a
/// volatility of 25 % (bid and ask a nickel either side of the model), the
/// options European; `margin_req` and the `margin_by` rows at d1.
fn margin_at_d1(decides: &str, margin: &str, margins: &[(String, usize, f64, f64)]) -> (RunResult, (f64, f64), Vec<MarginRow>) {
    use abt_kernel::functions::{bsm, Right};
    let m = market(&[(XYZ, flat(100.0))]);
    let mut rows = Vec::new();
    for i in 0..10 {
        // τ from the end of the day to the end of 2024-04-19 (K-2.5).
        let tau = ((day("2024-04-19") - d(i)) / abt_kernel::kernel::time::DAY) as f64 / 365.0;
        for k in [95.0, 100.0, 105.0] {
            let c = bsm::price(100.0, k, tau, 0.0, 0.0, 0.25, Right::Call);
            let p = bsm::price(100.0, k, tau, 0.0, 0.0, 0.25, Right::Put);
            rows.push(row(i, "2024-04-19", k, 100.0, Some((c - 0.05, c + 0.05)), Some((p - 0.05, p + 0.05))));
        }
    }
    let svc = serve_with_margins(&m, &rows, "XYZ=european:physical:pm", margins);
    let prog = program_on(XYZ, "    qty : Quantity<Contracts> = 1 contracts", decides, &[("margin", margin), ("margin_call", "allow")]);
    let (r, (req, by)) = run_with(&prog, &svc, |r, k| {
        let req = k
            .query("margin_req", d(1), &[])
            .unwrap()
            .first()
            .map(|tu| (tu[1].as_f64().unwrap(), tu[2].as_f64().unwrap()))
            .expect("margin_req");
        let names = r.symbols.clone();
        let by: Vec<MarginRow> = k
            .query("margin_by", d(1), &[])
            .unwrap()
            .into_iter()
            .map(|tu| (names[tu[0].as_equity().unwrap() as usize].clone(), tu[3].as_f64().unwrap(), k.show(&tu[4])))
            .collect();
        (req, by)
    });
    (r, req, by)
}

/// Sell (or buy) `q` contracts of the XYZ `right` of `strike` to 2024-04-19 at d0.
fn leg_at_d0(right: char, strike: f64, q: &str) -> String {
    format!("    decide(T, trade(A, Q)) :- universe(A, T), T = 2024-03-04, A = \"{}\", Q = {}.", occ("2024-04-19", right, strike), q)
}

/// The mid of a contract's quote at d1 (the model's price at 25 %).
fn mid_at_d1(right: abt_kernel::functions::Right, strike: f64) -> f64 {
    let tau = ((day("2024-04-19") - d(1)) / abt_kernel::kernel::time::DAY) as f64 / 365.0;
    abt_kernel::functions::bsm::price(100.0, strike, tau, 0.0, 0.0, 0.25, right)
}

#[test]
fn a_naked_short_put_under_reg_t_needs_twenty_percent_of_the_underlying_less_otm_plus_the_premium() {
    use abt_kernel::functions::Right;
    let (_, (_, maintenance), by) = margin_at_d1(&leg_at_d0('P', 100.0, "-qty"), "reg_t", &[]);
    // max(20 % · 100 − 0 + p, 10 % · 100 + p) × 100 × 1, the premium at its mark.
    let p = mid_at_d1(Right::Put, 100.0);
    let expected = (0.2f64 * 100.0 - 0.0 + p).max(0.1 * 100.0 + p) * 100.0;
    assert!(close(maintenance, expected), "{} against {}", maintenance, expected);
    assert_eq!(by.len(), 1);
    assert_eq!(by[0].2, "reg_t");
}

#[test]
fn a_vertical_spread_under_reg_t_needs_its_maximum_loss() {
    use abt_kernel::functions::Right;
    // A credit spread: the call of 100 sold, the call of 105 bought: the
    // width less the credit at the marks.
    let decides = format!("{}\n{}", leg_at_d0('C', 100.0, "-qty"), leg_at_d0('C', 105.0, "qty"));
    let (_, (_, maintenance), _) = margin_at_d1(&decides, "reg_t", &[]);
    let credit = mid_at_d1(Right::Call, 100.0) - mid_at_d1(Right::Call, 105.0);
    assert!(close(maintenance, (5.0 - credit) * 100.0), "{} against {}", maintenance, (5.0 - credit) * 100.0);
    // A debit spread: the debit.
    let decides = format!("{}\n{}", leg_at_d0('C', 100.0, "qty"), leg_at_d0('C', 105.0, "-qty"));
    let (_, (_, maintenance), _) = margin_at_d1(&decides, "reg_t", &[]);
    assert!(close(maintenance, credit * 100.0), "{} against {}", maintenance, credit * 100.0);
}

#[test]
fn a_straddle_under_portfolio_needs_its_worst_loss_over_the_grid_at_its_volatility() {
    use abt_kernel::functions::{bsm, Right};
    let decides = format!("{}\n{}", leg_at_d0('C', 100.0, "-qty"), leg_at_d0('P', 100.0, "-qty"));
    let (_, (_, maintenance), by) = margin_at_d1(&decides, "portfolio", &[]);
    let tau = ((day("2024-04-19") - d(1)) / abt_kernel::kernel::time::DAY) as f64 / 365.0;
    // The straddle's value per unit at the underlying's price s.
    let v = |s: f64| bsm::price(s, 100.0, tau, 0.0, 0.0, 0.25, Right::Call) + bsm::price(s, 100.0, tau, 0.0, 0.0, 0.25, Right::Put);
    // The two shorts lose what the straddle gains: the worst over eleven
    // points from −15 % to +15 %, floored at 0.375 × 100 a contract.
    let worst = (0..11).map(|i| (v(100.0 * (0.85 + 0.03 * i as f64)) - v(100.0)) * 100.0).fold(0.0, f64::max);
    let want = worst.max(2.0 * 0.375 * 100.0);
    assert!((maintenance - want).abs() < 1e-6 * want, "{} against {}", maintenance, want);
    assert!(by.iter().all(|x| x.2 == "portfolio"), "{:?}", by);
}

#[test]
fn published_margins_price_the_contracts_they_cover_and_reg_t_the_rest() {
    let (put100, put95) = (occ("2024-04-19", 'P', 100.0), occ("2024-04-19", 'P', 95.0));
    let margins: Vec<(String, usize, f64, f64)> = (0..10).map(|i| (put100.clone(), i, 1_500.0, 1_200.0)).collect();
    let decides = format!("{}\n{}", leg_at_d0('P', 100.0, "-qty"), leg_at_d0('P', 95.0, "-qty"));
    let (_, _, by) = margin_at_d1(&decides, "published, reg_t", &margins);
    let source = |name: &str| by.iter().find(|x| x.0 == name).map(|x| (x.1, x.2.clone()));
    assert_eq!(source(&put100), Some((1_200.0, "published".to_string())), "{:?}", by);
    assert_eq!(source(&put95).map(|x| x.1), Some("reg_t".to_string()), "{:?}", by);
}

#[test]
fn approx_alone_is_a_fraction_of_the_underlyings_notional_warned_at_every_bar() {
    let (r, (_, maintenance), by) = margin_at_d1(&leg_at_d0('P', 100.0, "-qty"), "approx 0.2", &[]);
    assert!(close(maintenance, 0.2 * 100.0 * 100.0), "{}", maintenance);
    assert_eq!(by[0].2, "approx");
    let w: Vec<&str> = r.warnings.iter().filter(|w| w.bias == "margin").map(|w| w.message.as_str()).collect();
    assert!(w.iter().any(|m| m.contains("approx") && m.contains("of 10 bars")), "{:?}", r.warnings);
}

// ---- the options and book metrics of Q-3.1, read back from the store (E-73) ----

/// A run writing its outputs to the data service, and its metrics read back
/// from the store by the query layer.
fn run_metrics(prog: &abt_kernel::check::Program, svc: &Arc<DataService>) -> (RunResult, abt_query::cli::RunMetrics) {
    let client = DataClient::connect(inproc(svc)).unwrap();
    let opts = Replay {
        write_outputs: true,
        ..Default::default()
    };
    let r = abt_kernel::kernel::run_on(prog, None, &client, ExecConfig::from_program(prog), &opts).unwrap_or_else(|e| panic!("{}", e));
    let data = abt_query::data::Data::connect(inproc(svc)).unwrap();
    let m = abt_query::cli::run_metrics(&data, &r.run_id).unwrap_or_else(|e| panic!("{}", e));
    (r, m)
}

#[test]
fn the_options_metrics_count_the_premium_the_exercises_the_expiries_and_the_days_to_expiry() {
    // 2 calls of 100 bought and 1 put of 100 sold at d0, both expiring
    // Friday 2024-03-08 (d4), filled at d1 (Tuesday 2024-03-05): XYZ closes
    // at 104 there, the calls are exercised and the put expires.
    let mut bars = flat(100.0);
    bars[4] = (103.0, 104.0);
    let m = market(&[(XYZ, bars)]);
    let rows: Vec<Row> = (0..5).map(|i| row(i, "2024-03-08", 100.0, bars[i].1, Some((2.0, 2.2)), Some((1.5, 1.7)))).collect();
    let svc = serve(&m, &rows, "");
    let (call, put) = (occ("2024-03-08", 'C', 100.0), occ("2024-03-08", 'P', 100.0));
    let decides = format!("{}\n{}", trade_at_d0(&call, 2), trade_at_d0(&put, -1));
    let (r, metrics) = run_metrics(&program(&decides, &[]), &svc);
    assert_eq!(events(&r, "exercise").len(), 1, "{:#?}", r.events);
    let o = metrics.options.expect("the run held options");
    // Paid: 2 × 2.25 (the ask and a quarter spread) × 100; collected: 1 ×
    // 1.45 (the bid less a quarter spread) × 100.
    assert!(close(o.premium_paid, 450.0) && close(o.premium_collected, 145.0), "{:?}", o);
    assert_eq!((o.exercises, o.assignments, o.expiries), (1, 0, 1));
    // Both opened 3 days before their expiry and closed on its day.
    assert_eq!((o.days_to_expiry_entry, o.days_to_expiry_exit), (Some(3.0), Some(0.0)));
    // The book: the two options at d1 to d3 (marked at the mids, 2.10 and
    // 1.60), then the 200 shares the exercise bought.
    let (calls, puts) = (2.0 * 100.0 * 2.1, 100.0 * 1.6);
    let options_names = (calls + puts) * (calls + puts) / (calls * calls + puts * puts);
    let b = &metrics.book;
    assert!(close(b.effective_names, (3.0 * options_names + 6.0) / 9.0), "{:?}", b);
    // The largest weight is the shares at 104 on the expiry day.
    let nav_d4 = r.exposure.iter().find(|x| x.t == d(4)).unwrap().equity;
    assert!(close(b.max_weight, 200.0 * 104.0 / nav_d4), "{:?} against {}", b, 200.0 * 104.0 / nav_d4);
    assert!(close(b.funding_paid, 0.0));
}

#[test]
fn the_options_metrics_attribute_the_pnl_to_the_greeks_and_compare_realised_with_implied_volatility() {
    use abt_kernel::functions::{bsm, Right};
    // XYZ closes at 100 and 101 on alternate days; the chain of 2024-04-19
    // is quoted every day at the model's prices at 25 % (a nickel either
    // side). 2 calls of 100 bought at d0 are held from d1 to d9.
    let closes: Vec<f64> = (0..10).map(|i| if i % 2 == 0 { 100.0 } else { 101.0 }).collect();
    let mut bars = flat(100.0);
    for (i, c) in closes.iter().enumerate() {
        bars[i] = (*c, *c);
    }
    let m = market(&[(XYZ, bars)]);
    let tau = |i: usize| ((day("2024-04-19") - d(i)) / abt_kernel::kernel::time::DAY) as f64 / 365.0;
    let mut rows = Vec::new();
    for (i, s) in closes.iter().enumerate() {
        for k in [95.0, 100.0, 105.0] {
            let c = bsm::price(*s, k, tau(i), 0.0, 0.0, 0.25, Right::Call);
            let p = bsm::price(*s, k, tau(i), 0.0, 0.0, 0.25, Right::Put);
            rows.push(row(i, "2024-04-19", k, *s, Some((c - 0.05, c + 0.05)), Some((p - 0.05, p + 0.05))));
        }
    }
    let svc = serve(&m, &rows, "XYZ=european:physical:pm");
    let prog = program_on(XYZ, "    qty : Quantity<Contracts> = 2 contracts", &leg_at_d0('C', 100.0, "qty"), &[]);
    let (_, metrics) = run_metrics(&prog, &svc);
    let o = metrics.options.expect("the run held options");
    let rel = |a: f64, b: f64| (a - b).abs() <= 1e-7 * (1.0 + b.abs());
    let g = |i: usize| bsm::greeks(closes[i], 100.0, tau(i), 0.0, 0.0, 0.25, Right::Call);
    // The net Greeks at each barrier from d1: 2 × 100 × the model's at 25 %.
    assert_eq!(o.greeks.len(), 9);
    for (n, x) in o.greeks.iter().enumerate() {
        let i = n + 1;
        assert_eq!(x.t, d(i));
        assert!(rel(x.delta_money, 200.0 * g(i).delta * closes[i]) && rel(x.gamma, 200.0 * g(i).gamma), "{:?}", x);
        assert!(rel(x.vega, 200.0 * g(i).vega) && rel(x.theta, 200.0 * g(i).theta), "{:?}", x);
    }
    // The P&L carried over d1..d9 and its expansion at each bar's first
    // barrier: Δ·dS, ½Γ·dS², Θ·dt; the volatility does not move.
    let (mut delta, mut gamma, mut theta) = (0.0, 0.0, 0.0);
    for i in 1..9 {
        let ds = closes[i + 1] - closes[i];
        let dt = (d(i + 1) - d(i)) as f64 / (365.0 * abt_kernel::kernel::time::DAY as f64);
        delta += 200.0 * g(i).delta * ds;
        gamma += 200.0 * 0.5 * g(i).gamma * ds * ds;
        theta += 200.0 * g(i).theta * dt;
    }
    let price = |i: usize| bsm::price(closes[i], 100.0, tau(i), 0.0, 0.0, 0.25, Right::Call);
    let a = &o.attribution;
    assert_eq!((a.bars, a.unattributed), (8, 0));
    assert!(rel(a.pnl, 200.0 * (price(9) - price(1))), "{:?}", a);
    assert!(rel(a.delta, delta) && rel(a.gamma, gamma) && rel(a.theta, theta), "{:?} against {} {} {}", a, delta, gamma, theta);
    assert!(a.vega.abs() < 1e-6, "{:?}", a);
    assert!(close(a.residual, a.pnl - (a.delta + a.gamma + a.theta + a.vega)), "{:?}", a);
    // What the expansion leaves is of the third order.
    assert!(a.residual.abs() < 0.02 * (a.delta.abs() + a.gamma.abs() + a.theta.abs()), "{:?}", a);
    // XYZ's log returns over the barriers held, ±ln(1.01), against 25 %.
    let v = &o.volatility;
    assert_eq!((v.len(), v[0].returns), (1, 8));
    let x: Vec<f64> = (1..9).map(|i| (closes[i + 1] / closes[i]).ln()).collect();
    let mean = x.iter().sum::<f64>() / 8.0;
    let realised = (x.iter().map(|r| (r - mean).powi(2)).sum::<f64>() / 7.0 * metrics.periods_per_year).sqrt();
    assert!(rel(v[0].realised.unwrap(), realised), "{:?} against {}", v[0], realised);
    assert!(rel(v[0].implied.unwrap(), 0.25), "{:?}", v[0]);
}

#[test]
fn the_book_metrics_read_the_margin_models_requirement_and_its_source() {
    // A naked put of 100 sold at d0 under reg_t: the book's margin over its
    // NAV, and the book's margin source, `reg_t`.
    use abt_kernel::functions::{bsm, Right};
    let m = market(&[(XYZ, flat(100.0))]);
    let mut rows = Vec::new();
    for i in 0..10 {
        let tau = ((day("2024-04-19") - d(i)) / abt_kernel::kernel::time::DAY) as f64 / 365.0;
        let p = bsm::price(100.0, 100.0, tau, 0.0, 0.0, 0.25, Right::Put);
        rows.push(row(i, "2024-04-19", 100.0, 100.0, Some((1.0, 1.2)), Some((p - 0.05, p + 0.05))));
    }
    let svc = serve(&m, &rows, "XYZ=european:physical:pm");
    let prog = program_on(
        XYZ,
        "    qty : Quantity<Contracts> = 1 contracts",
        &leg_at_d0('P', 100.0, "-qty"),
        &[("margin", "reg_t"), ("margin_call", "allow")],
    );
    let (r, metrics) = run_metrics(&prog, &svc);
    let u: Vec<f64> = r.exposure.iter().filter(|x| x.equity > 0.0).map(|x| x.margin_initial / x.equity).collect();
    let b = &metrics.book;
    assert!(close(b.margin_utilisation, u.iter().sum::<f64>() / u.len() as f64), "{:?} against {:?}", b, u);
    assert!(close(b.max_margin_utilisation, u.iter().copied().fold(0.0, f64::max)) && b.max_margin_utilisation > 0.0, "{:?}", b);
    // The book's rows name the model from the first holding on.
    assert!(
        r.exposure.iter().all(|x| x.margin_source == "reg_t"),
        "{:?}",
        r.exposure.iter().map(|x| &x.margin_source).collect::<Vec<_>>()
    );
    let data = abt_query::data::Data::connect(inproc(&svc)).unwrap();
    let book = abt_query::relation::read(&data, &format!("kernel.{}.book", r.run_id)).unwrap().unwrap();
    let sources = abt_query::relation::texts(&book, "margin_source").unwrap();
    assert!(sources.iter().all(|s| s.as_deref() == Some("reg_t")), "{:?}", sources);
}
