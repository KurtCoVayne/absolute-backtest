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
