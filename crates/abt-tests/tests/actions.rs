//! Corporate actions, delistings and membership (data-bundle doc, sections 3
//! and 4): prices are stored as traded with the actions as events; the
//! executor adjusts positions on a split's ex-date, credits dividends on the
//! pay date and force-closes a delisted name at its last trade with a haircut
//! by reason; the `catalog` library derives total return and a point-in-time
//! adjusted close from the same events.

use abt_tests as common;

use abt_kernel::data::{business_days, synthetic_daily};
use abt_kernel::kernel::time::format_timestamp;
use abt_kernel::kernel::{Action, Dataset, ExecConfig, Lot, Sym, Value};
use abt_tests::run;

/// A daily equity strategy over the catalog (bars, actions, the listing and
/// the `SPX` list) with the `catalog` library in scope.
fn equity(name: &str, params: &str, decides: &str) -> String {
    common::StrategyParts {
        name: name.into(),
        needs: "need bars @1d   need actions   need listing   need lists \"SPX\"".into(),
        uses: "calendar, bars, book, catalog".into(),
        params: params.into(),
        decides: decides.into(),
        ..Default::default()
    }
    .render()
}

/// Buy 10 of X once, when flat, and hold.
fn hold() -> String {
    equity("hold", "    qty : Quantity<Shares> = 10 shares", "    decide(T, trade(A, qty)) :- universe(A, T), flat(A, T).")
}

/// Short 10 of X once and hold.
fn short_hold() -> String {
    equity(
        "short_hold",
        "    qty : Quantity<Shares> = 10 shares",
        "    decide(T, trade(A, Q)) :- universe(A, T), flat(A, T), Q = -qty.",
    )
}

struct Market {
    ds: Dataset,
    x: Sym,
    days: Vec<i64>,
}

/// One symbol X over `n` weekdays from 2024-01-08 with the given closes (a
/// `None` day has no bar and no listing row: delisted).
fn market(closes: &[Option<f64>]) -> Market {
    let days = business_days((2024, 1, 8), closes.len());
    let mut ds = Dataset::new();
    let x = ds.add_security("X", "X", days[0], None);
    for (t, p) in days.iter().zip(closes) {
        if let Some(p) = p {
            common::add_bar(&mut ds, x, *t, *p, 1_000_000.0);
        }
    }
    ds.derive_tickers();
    Market { ds, x, days }
}

fn cfg(cash: f64) -> ExecConfig {
    ExecConfig {
        initial_cash: cash,
        cash_rate: 0.0,
        margin_rate: 0.0,
        borrow: vec![],
        ..ExecConfig::frictionless()
    }
}

fn close(a: f64, b: f64) -> bool {
    (a - b).abs() < 1e-9
}

#[test]
fn a_split_multiplies_the_position_at_the_ex_date_and_cashes_the_fraction() {
    // Bought 10 at 20 on day 2; a 2:1 split on day 3 halves the as-traded price.
    let mut m = market(&[Some(20.0), Some(20.0), Some(10.0), Some(10.0), Some(10.0)]);
    m.ds.add("split", vec![Value::Equity(m.x), Value::Time(m.days[2]), Value::Num(2.0)]);
    let p = common::program(&hold(), "hold");
    let r = run(&p, &m.ds, cfg(1000.0)).unwrap();
    assert_eq!(r.fills.len(), 1, "{:?}", r.fills);
    assert_eq!(r.final_positions.values().copied().collect::<Vec<_>>(), vec![20.0]);
    assert!(close(r.final_cash, 800.0));
    let eq: Vec<f64> = r.equity_curve.iter().map(|(_, e)| *e).collect();
    assert_eq!(eq, vec![1000.0, 1000.0, 1000.0, 1000.0, 1000.0], "a split moves no value");
    assert_eq!(r.actions.len(), 1);
    assert!(matches!(r.actions[0].action, Action::Split { factor } if factor == 2.0), "{:?}", r.actions[0]);
    assert_eq!(format_timestamp(r.actions[0].t), "2024-01-10");
    // A 3:2 split of 15 shares is 22.5: whole lots keep 22 and cash the half share at the new price.
    let mut m = market(&[Some(30.0), Some(30.0), Some(20.0), Some(20.0), Some(20.0)]);
    m.ds.add("split", vec![Value::Equity(m.x), Value::Time(m.days[2]), Value::Num(1.5)]);
    let p = common::program(&hold().replace("10 shares", "15 shares"), "hold");
    let r = run(&p, &m.ds, cfg(1000.0)).unwrap();
    assert_eq!(r.final_positions.values().copied().collect::<Vec<_>>(), vec![22.0]);
    assert!(close(r.final_cash, 1000.0 - 450.0 + 0.5 * 20.0), "{}", r.final_cash);
    assert!(close(r.actions[0].cash, 10.0), "{:?}", r.actions[0]);
    let r = run(&p, &m.ds, ExecConfig { lot: Lot::Fractional, ..cfg(1000.0) }).unwrap();
    assert_eq!(r.final_positions.values().copied().collect::<Vec<_>>(), vec![22.5]);
}

/// A program that applies actions (`actions apply`) without loading them
/// (`need actions`) cannot see the split: the run warns, naming what the
/// data carries, and the split is a price move to its book.
#[test]
fn a_split_the_program_does_not_load_is_warned() {
    let mut m = market(&[Some(20.0), Some(20.0), Some(10.0), Some(10.0), Some(10.0)]);
    m.ds.add("split", vec![Value::Equity(m.x), Value::Time(m.days[2]), Value::Num(2.0)]);
    // (The catalog library needs the actions too, so it goes with them.)
    let blind = hold().replace("need actions   ", "").replace(", catalog", "");
    assert_ne!(blind, hold());
    let p = common::program(&blind, "hold");
    let r = run(&p, &m.ds, cfg(1000.0)).unwrap();
    let w: Vec<&str> = r.warnings.iter().filter(|w| w.bias == "corporate actions").map(|w| w.message.as_str()).collect();
    assert!(w.len() == 1 && w[0].contains("split") && w[0].contains("need actions"), "{:?}", r.warnings);
    assert_eq!(r.final_positions.values().copied().collect::<Vec<_>>(), vec![10.0], "the split is not applied");
    // With the need, the split is applied and nothing is warned.
    let r = run(&common::program(&hold(), "hold"), &m.ds, cfg(1000.0)).unwrap();
    assert!(r.warnings.iter().all(|w| w.bias != "corporate actions"), "{:?}", r.warnings);
    assert_eq!(r.final_positions.values().copied().collect::<Vec<_>>(), vec![20.0]);
}

#[test]
fn a_dividend_is_credited_on_the_pay_date_for_the_shares_held_at_the_ex_date() {
    // Announced day 1: ex day 3, pay day 5, 1.00 a share. Bought 10 on day 2; the
    // as-traded price drops by the dividend on the ex-date.
    let mut m = market(&[Some(20.0), Some(20.0), Some(19.0), Some(19.0), Some(19.0), Some(19.0)]);
    m.ds.add(
        "dividend",
        vec![Value::Equity(m.x), Value::Time(m.days[0]), Value::Time(m.days[2]), Value::Time(m.days[4]), Value::Num(1.0)],
    );
    let p = common::program(&hold(), "hold");
    let r = run(&p, &m.ds, cfg(1000.0)).unwrap();
    let cash: Vec<f64> = r.exposure.iter().map(|e| e.cash).collect();
    // The day-1 decision fills on day 2 (cash 800 at that mark); the 10.00 arrives at the day-5 mark.
    assert_eq!(cash, vec![1000.0, 800.0, 800.0, 800.0, 810.0, 810.0], "{:?}", cash);
    // The NAV counts the receivable from the ex-date (K-5.1): the 10.00 the
    // price drops by is owed to the book until the pay date.
    let eq: Vec<f64> = r.equity_curve.iter().map(|(_, e)| *e).collect();
    assert_eq!(eq, vec![1000.0; 6], "the receivable is marked between ex and pay");
    let owed: Vec<f64> = r.exposure.iter().map(|e| e.receivables).collect();
    assert_eq!(owed, vec![0.0, 0.0, 10.0, 10.0, 0.0, 0.0], "{:?}", owed);
    let divs: Vec<&abt_kernel::kernel::ActionRecord> = r.actions.iter().filter(|a| matches!(a.action, Action::Dividend { .. })).collect();
    assert_eq!(divs.len(), 1, "{:?}", r.actions);
    assert_eq!(format_timestamp(divs[0].t), "2024-01-12");
    assert!(close(divs[0].cash, 10.0));
    // A short pays it.
    let r = run(&common::program(&short_hold(), "short_hold"), &m.ds, cfg(1000.0)).unwrap();
    let cash: Vec<f64> = r.exposure.iter().map(|e| e.cash).collect();
    assert_eq!(cash, vec![1000.0, 1200.0, 1200.0, 1200.0, 1190.0, 1190.0], "{:?}", cash);
    // What it owes is a negative receivable, so its NAV holds too.
    let eq: Vec<f64> = r.equity_curve.iter().map(|(_, e)| *e).collect();
    assert_eq!(eq, vec![1000.0; 6], "{:?}", eq);
    // The library's total return sees the dividend: (19 + 1) / 20 - 1 = 0 on
    // the ex-date, like the flat day before it; the days after move.
    let mut m2 = market(&[Some(20.0), Some(20.0), Some(19.0), Some(20.0), Some(21.0), Some(22.0)]);
    m2.ds.add(
        "dividend",
        vec![Value::Equity(m2.x), Value::Time(m2.days[0]), Value::Time(m2.days[2]), Value::Time(m2.days[4]), Value::Num(1.0)],
    );
    let src = equity(
        "hold",
        "    qty : Quantity<Shares> = 10 shares",
        "    decide(T, trade(A, qty)) :- universe(A, T), total_return(A, T, R), R = 0.",
    );
    let p = common::program(&src, "hold");
    let r = run(&p, &m2.ds, cfg(1000.0)).unwrap();
    let decided: Vec<String> = r.decisions.iter().map(|d| format_timestamp(d.t)).collect();
    assert_eq!(decided, vec!["2024-01-09", "2024-01-10"], "{:?}", decided);
}

#[test]
fn the_library_adjusts_for_splits_causally() {
    // 40, 40, 20 (2:1 split), 20, 22: total return is 0, 0, 0, 0.1 and the
    // adjusted close is continuous in the first bar's share basis.
    let mut m = market(&[Some(40.0), Some(40.0), Some(20.0), Some(20.0), Some(22.0)]);
    m.ds.add("split", vec![Value::Equity(m.x), Value::Time(m.days[2]), Value::Num(2.0)]);
    let probe = |rule: &str| equity("probe", "    up : Scalar = 0.05\n    lvl : Price<USD> = 41 USD/share", &format!("    {rule}"));
    let decided = |src: &str| -> Vec<String> {
        let p = common::program(src, "probe");
        let r = run(&p, &m.ds, cfg(1000.0)).unwrap();
        r.decisions.iter().map(|d| format_timestamp(d.t)).collect()
    };
    // The split day has zero total return; day 5 is +10%.
    assert_eq!(decided(&probe("decide(T, hold(A, 0)) :- universe(A, T), total_return(A, T, R), R > up.")), vec!["2024-01-12"]);
    // The adjusted close is 40, 40, 40, 40, 44: continuous across the split,
    // above 41 on day 5 only; the as-traded 22 never is.
    assert_eq!(decided(&probe("decide(T, hold(A, 0)) :- universe(A, T), adj_close(A, T, P), P > lvl.")), vec!["2024-01-12"]);
    assert!(decided(&probe("decide(T, hold(A, 0)) :- universe(A, T), close(A, T, P), P > lvl.")).is_empty());
    // Before the split the adjusted close equals the close.
    assert_eq!(
        decided(&probe("decide(T, hold(A, 0)) :- universe(A, T), adj_close(A, T, P), close(A, T, P0), P = P0.")),
        vec!["2024-01-08", "2024-01-09"]
    );
}

#[test]
fn a_delisting_force_closes_at_the_last_trade_with_a_haircut_by_reason() {
    // Decided on day 1, bought 10 at the day-2 close of 4 (cash 960); the last
    // trade is that 4; delisted from day 3 on.
    let mut m = market(&[Some(10.0), Some(4.0), None, None, None]);
    let bankrupt = m.ds.intern_label("bankruptcy");
    for &t in &m.days[2..] {
        m.ds.add("delisted", vec![Value::Equity(m.x), Value::Time(t), Value::Label(bankrupt)]);
    }
    let p = common::program(&hold(), "hold");
    let r = run(&p, &m.ds, cfg(1000.0)).unwrap();
    // Bankruptcy: a full haircut by default; the position is closed at zero.
    assert_eq!(r.fills.len(), 2, "{:?}", r.fills);
    let forced = &r.fills[1];
    assert!(forced.forced && forced.quantity == -10.0 && forced.price == 0.0, "{:?}", forced);
    assert_eq!(format_timestamp(forced.t), "2024-01-10");
    assert!(r.final_positions.is_empty());
    assert!(close(r.final_cash, 960.0), "{}", r.final_cash);
    assert!(
        matches!(&r.actions[0].action, Action::Delisting { reason, haircut } if reason == "bankruptcy" && *haircut == 1.0),
        "{:?}",
        r.actions
    );
    assert!(r.dropped.iter().all(|(_, _, why)| why.contains("no price") || why == "no next bar"), "{:?}", r.dropped);
    // An acquisition pays the last trade in full.
    let mut m2 = market(&[Some(10.0), Some(4.0), None, None, None]);
    let acq = m2.ds.intern_label("acquisition");
    for &t in &m2.days[2..] {
        m2.ds.add("delisted", vec![Value::Equity(m2.x), Value::Time(t), Value::Label(acq)]);
    }
    let r = run(&p, &m2.ds, cfg(1000.0)).unwrap();
    assert!(close(r.fills[1].price, 4.0) && close(r.final_cash, 960.0 + 40.0), "{:?}", r.fills);
    // The haircuts are configuration; a zero haircut on an involuntary reason is warned.
    let r = run(
        &p,
        &m.ds,
        ExecConfig {
            delisting_haircuts: vec![("bankruptcy".into(), 0.0)],
            ..cfg(1000.0)
        },
    )
    .unwrap();
    assert!(close(r.final_cash, 1000.0), "{}", r.final_cash);
    assert!(r.warnings.iter().any(|w| w.bias == "delisting"), "{:?}", r.warnings);
    // An unknown reason takes the conservative default.
    let mut m3 = market(&[Some(10.0), Some(4.0), None, None, None]);
    let odd = m3.ds.intern_label("merged-into-spac");
    for &t in &m3.days[2..] {
        m3.ds.add("delisted", vec![Value::Equity(m3.x), Value::Time(t), Value::Label(odd)]);
    }
    let r = run(&p, &m3.ds, cfg(1000.0)).unwrap();
    assert!(close(r.final_cash, 960.0), "{}", r.final_cash);
    assert_eq!(ExecConfig::default().delisting_haircut_default, 1.0);
}

#[test]
fn membership_and_classification_are_point_in_time_relations() {
    let src = equity(
        "in_index",
        "    qty : Quantity<Shares> = 10 shares\n    idx : Label = \"SPX\"\n    code : Label = \"tech\"",
        "    decide(T, trade(A, qty)) :- universe(A, T), member(A, T, idx), classification(A, T, \"sector\", code), flat(A, T).",
    );
    let mut m = market(&[Some(10.0); 4]);
    let spx = m.ds.intern_label("SPX");
    let scheme = m.ds.intern_label("sector");
    let tech = m.ds.intern_label("tech");
    let fin = m.ds.intern_label("fin");
    // A member from day 2; a tech name until day 3, then fin.
    for (i, &t) in m.days.iter().enumerate() {
        if i >= 1 {
            m.ds.add("member", vec![Value::Equity(m.x), Value::Time(t), Value::Label(spx)]);
        }
        m.ds.add(
            "classification",
            vec![Value::Equity(m.x), Value::Time(t), Value::Label(scheme), Value::Label(if i < 3 { tech } else { fin })],
        );
    }
    let p = common::program(&src, "in_index");
    let r = run(&p, &m.ds, cfg(1000.0)).unwrap();
    let decided: Vec<String> = r.decisions.iter().map(|d| format_timestamp(d.t)).collect();
    assert_eq!(decided, vec!["2024-01-09"], "a member and tech only on day 2 while flat: {:?}", decided);
}

#[test]
fn the_synthetic_catalog_market_has_every_event_and_runs_the_corpus_strategy() {
    let ds = synthetic_daily(&["AAA", "BBB", "CCC", "DDD", "SPY"], (2022, 1, 3), 320, 11);
    assert_eq!(ds.securities.len(), 6, "five names, one of which changes ticker: {:?}", ds.securities);
    assert!(!ds.facts["split"].is_empty());
    assert!(ds.facts["dividend"].len() >= 2);
    assert!(!ds.facts["delisted"].is_empty());
    assert!(ds.facts["member"].len() > 1000 && ds.facts["classification"].len() > 1000);
    for rel in ["open", "high", "low"] {
        assert_eq!(ds.facts[rel].len(), ds.facts["close"].len());
    }
    // The delisted name leaves the listing and its prices stop.
    let delisted_sym = ds.facts["delisted"][0][0].as_equity().unwrap();
    let first_delisted = ds.facts["delisted"].iter().map(|tu| tu[1].as_time().unwrap()).min().unwrap();
    for rel in ["listed", "close"] {
        assert!(!ds.facts[rel].iter().any(|tu| tu[0].as_equity() == Some(delisted_sym) && tu[1].as_time().unwrap() >= first_delisted));
    }
    let p = common::corpus_program("total_return_momentum");
    let r = run(&p, &ds, ExecConfig::from_program(&p)).unwrap();
    assert!(!r.decisions.is_empty() && !r.fills.is_empty());
    assert!(
        r.actions.iter().any(|a| matches!(a.action, Action::Split { .. })) || r.actions.iter().any(|a| matches!(a.action, Action::Dividend { .. })),
        "{:?}",
        r.actions.len()
    );
}

/// A split on its ex-date is applied to the book before the bar's fills:
/// a `hold` order filled at the post-split price finds the post-split
/// quantity, so a book holding its whole base in the name keeps it.
#[test]
fn a_split_is_applied_before_the_bar_fills() {
    let src = common::StrategyParts {
        name: "all_in".into(),
        needs: "need bars @1d   need listing   need actions".into(),
        params: "    w : Scalar = 1.0".into(),
        decides: "    decide(T, hold(A, w)) :- universe(A, T).".into(),
        ..Default::default()
    }
    .render();
    let prog = common::program(&src, "all_in");
    let mut ds = common::market("X", &[10.0, 10.0, 10.0, 5.0, 5.0, 5.0]);
    let x = ds.symbols.get("X").unwrap();
    let ex = common::day("2024-01-11");
    ds.add("split", vec![Value::Equity(x), Value::Time(ex), Value::Num(2.0)]);
    let r = run(
        &prog,
        &ds,
        ExecConfig {
            initial_cash: 1000.0,
            ..ExecConfig::frictionless()
        },
    )
    .unwrap();
    // 100 shares at 10 become 200 at 5: nothing to trade on the ex-date,
    // and the book is worth its base throughout.
    assert!(r.fills.iter().all(|f| f.t != ex), "{:?}", r.fills.iter().map(|f| (f.t, f.quantity, f.price)).collect::<Vec<_>>());
    assert_eq!(r.final_positions.get(&x).copied(), Some(200.0));
    assert!(r.equity_curve.iter().all(|(_, e)| (e - 1000.0).abs() < 1e-9), "{:?}", r.equity_curve);
}

/// A weekly book at the signal close on the total-return index, as MW14's,
/// buying every name of the week from `from` and selling what left the
/// universe.
fn weekly_book(from: &str) -> abt_kernel::check::Program {
    let src = common::StrategyParts {
        name: "weekly_book".into(),
        needs: "need bars @1d   need actions, listing".into(),
        uses: "calendar, catalog, book".into(),
        calendars: Some("weekly = weeks(1)".into()),
        clock: "decide on weekly".into(),
        universe: "universe(A, T) :- traded@weekly(A, T).".into(),
        params: format!("    w : Scalar = 0.25\n    from : Timestamp = {}", from),
        orders: "moc".into(),
        decides: "    decide(T, hold(A, w)) :- universe(A, T), T >= from, not held(A, T, _).\n    decide(T, hold(A, 0)) :- held(A, T, _), not universe(A, T).".into(),
        execution: "book_fixed_base".into(),
        report: "calendar weekly   periods 52 per year".into(),
        ..Default::default()
    }
    .render();
    common::program(&src, "weekly_book")
}

/// On a calendar coarser than the base, the bar whose bucket holds a name's
/// last print is its last bar, and the delisting applies at the next, at the
/// last price (ERRATA E-161). X prints from Monday 2024-01-08 to Wednesday
/// 2024-01-17 and is delisted from Thursday on, a row a day as the Norgate
/// adapter writes it; Y prints through Friday 2024-01-26. X's last bar is
/// the week of 2024-01-19, at its Wednesday close of 107. The delisting
/// applied at that week's bar: a name held there was closed before the
/// week's decisions and bought back at the same close, and a name bought
/// there was not held when the delisting came, so it was never applied;
/// either way X stayed in the book for good, its exits expiring every week.
#[test]
fn on_a_weekly_calendar_a_delisting_applies_at_the_first_bar_without_a_print() {
    let days = business_days((2024, 1, 8), 15);
    let mut ds = Dataset::new();
    let x = ds.add_security("X", "X", days[0], None);
    let y = ds.add_security("Y", "Y", days[0], None);
    let acquisition = ds.intern_label("acquisition");
    for (i, &t) in days.iter().enumerate() {
        if i <= 7 {
            common::add_bar(&mut ds, x, t, 100.0 + i as f64, 1000.0);
        } else {
            ds.add("delisted", vec![Value::Equity(x), Value::Time(t), Value::Label(acquisition)]);
        }
        common::add_bar(&mut ds, y, t, 50.0 + i as f64, 1000.0);
    }
    ds.derive_tickers();
    let day = |s: &str| common::day(s);
    // Held from the first week, and bought in its last week.
    for (from, bought) in [("2024-01-12", "2024-01-12"), ("2024-01-19", "2024-01-19")] {
        let p = weekly_book(from);
        let r = run(&p, &ds, ExecConfig::from_program(&p)).unwrap();
        let is = |s: Sym, name: &str| r.symbols[s as usize] == name;
        let on_x: Vec<(String, f64, f64, bool)> = r.fills.iter().filter(|f| is(f.equity, "X")).map(|f| (format_timestamp(f.t), f.quantity, f.price, f.forced)).collect();
        let buys: Vec<String> = r.fills.iter().filter(|f| is(f.equity, "X") && f.quantity > 0.0).map(|f| format_timestamp(f.t)).collect();
        assert_eq!(buys, vec![bought.to_string()], "from {}: X is bought once: {:?}", from, on_x);
        let forced: Vec<_> = r.fills.iter().filter(|f| is(f.equity, "X") && f.forced).collect();
        assert_eq!(forced.len(), 1, "from {}: {:?}", from, on_x);
        assert_eq!(forced[0].t, day("2024-01-26"), "from {}: the delisting applies at the first bar without a print of X: {:?}", from, on_x);
        assert!(close(forced[0].price, 107.0) && forced[0].commission == 0.0, "from {}: at the last price, at no cost: {:?}", from, on_x);
        let delistings: Vec<i64> = r.actions.iter().filter(|a| is(a.equity, "X") && matches!(a.action, Action::Delisting { .. })).map(|a| a.t).collect();
        assert_eq!(delistings, vec![day("2024-01-26")], "from {}", from);
        let held: Vec<&str> = r.final_positions.keys().map(|s| r.symbols[*s as usize].as_str()).collect();
        assert_eq!(held, vec!["Y"], "from {}: X left the book", from);
        let expired = r.events.iter().filter(|e| matches!(e.kind, abt_kernel::kernel::EventKind::ExpiredOrder { .. })).count();
        assert_eq!(expired, 0, "from {}: no order on X waits for a print that never comes", from);
    }
}
