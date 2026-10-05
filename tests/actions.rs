//! Corporate actions, delistings and membership (data-bundle doc, sections 3
//! and 4): prices are stored as traded with the actions as events; the
//! executor adjusts positions on a split's ex-date, credits dividends on the
//! pay date and force-closes a delisted name at its last trade with a haircut
//! by reason; the `catalog` library derives total return and a point-in-time
//! adjusted close from the same events.

mod corpus;

use absolute_backtest::check::{check_program, Program};
use absolute_backtest::data::{business_days, synthetic_daily_v2};
use absolute_backtest::kernel::time::format_timestamp;
use absolute_backtest::kernel::{run, Action, Dataset, ExecConfig, Lot, Value};

fn program(src: &str, name: &str) -> Program {
    let mut ws = corpus::base_workspace();
    ws.add_source(src).unwrap();
    let (p, diags) = check_program(&ws, name);
    let text: Vec<String> = diags.iter().map(|d| d.to_string()).collect();
    p.unwrap_or_else(|| panic!("{} should check:\n{}", name, text.join("\n")))
}

/// Buy 10 of X once, when flat, and hold.
const HOLD: &str = r#"
strategy hold {
  env equities_1d_v2
  uses catalog
  resolution @1d
  mode delta
  param qty : Quantity<Shares> = 10 shares
  decide(T, buy(A, qty)) :- universe(A, T), flat(A, T).
}
"#;

/// Short 10 of X once and hold.
const SHORT: &str = r#"
strategy short_hold {
  env equities_1d_v2
  uses catalog
  resolution @1d
  mode delta
  param qty : Quantity<Shares> = 10 shares
  decide(T, short(A, qty)) :- universe(A, T), flat(A, T).
}
"#;

struct Market {
    ds: Dataset,
    x: u32,
    days: Vec<i64>,
}

/// One symbol X over `n` weekdays from 2024-01-08 with the given closes (a
/// `None` day has no price and no universe row: delisted).
fn market(closes: &[Option<f64>]) -> Market {
    let days = business_days((2024, 1, 8), closes.len());
    let mut ds = Dataset::new();
    let x = ds.intern("X");
    for (t, p) in days.iter().zip(closes) {
        if let Some(p) = p {
            for rel in ["open", "high", "low", "close"] {
                ds.add(rel, vec![Value::Equity(x), Value::Time(*t), Value::Num(*p)]);
            }
            ds.add("volume", vec![Value::Equity(x), Value::Time(*t), Value::Num(1_000_000.0)]);
            ds.add("universe", vec![Value::Equity(x), Value::Time(*t)]);
        }
    }
    ds.derive_tickers();
    Market { ds, x, days }
}

fn cfg(cash: f64) -> ExecConfig {
    ExecConfig { initial_cash: cash, cash_rate: 0.0, margin_rate: 0.0, borrow: vec![], ..ExecConfig::frictionless() }
}

fn close(a: f64, b: f64) -> bool {
    (a - b).abs() < 1e-9
}

#[test]
fn a_split_multiplies_the_position_at_the_ex_date_and_cashes_the_fraction() {
    // Bought 10 at 20 on day 2; a 2:1 split on day 3 halves the as-traded price.
    let mut m = market(&[Some(20.0), Some(20.0), Some(10.0), Some(10.0), Some(10.0)]);
    m.ds.add("split", vec![Value::Equity(m.x), Value::Time(m.days[2]), Value::Num(2.0)]);
    let p = program(HOLD, "hold");
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
    let p = program(&HOLD.replace("10 shares", "15 shares"), "hold");
    let r = run(&p, &m.ds, cfg(1000.0)).unwrap();
    assert_eq!(r.final_positions.values().copied().collect::<Vec<_>>(), vec![22.0]);
    assert!(close(r.final_cash, 1000.0 - 450.0 + 0.5 * 20.0), "{}", r.final_cash);
    assert!(close(r.actions[0].cash, 10.0), "{:?}", r.actions[0]);
    let r = run(&p, &m.ds, ExecConfig { lot: Lot::Fractional, ..cfg(1000.0) }).unwrap();
    assert_eq!(r.final_positions.values().copied().collect::<Vec<_>>(), vec![22.5]);
}

#[test]
fn a_dividend_is_credited_on_the_pay_date_for_the_shares_held_at_the_ex_date() {
    // Announced day 1: ex day 3, pay day 5, 1.00 a share. Bought 10 on day 2; the
    // as-traded price drops by the dividend on the ex-date.
    let mut m = market(&[Some(20.0), Some(20.0), Some(19.0), Some(19.0), Some(19.0), Some(19.0)]);
    m.ds.add("dividend", vec![Value::Equity(m.x), Value::Time(m.days[0]), Value::Time(m.days[2]), Value::Time(m.days[4]), Value::Num(1.0)]);
    let p = program(HOLD, "hold");
    let r = run(&p, &m.ds, cfg(1000.0)).unwrap();
    let cash: Vec<f64> = r.exposure.iter().map(|e| e.cash).collect();
    // Cash 800 after the buy; the 10.00 arrives at the day-5 mark.
    assert_eq!(cash, vec![1000.0, 1000.0, 800.0, 800.0, 810.0, 810.0], "{:?}", cash);
    let eq: Vec<f64> = r.equity_curve.iter().map(|(_, e)| *e).collect();
    assert_eq!(eq, vec![1000.0, 1000.0, 990.0, 990.0, 1000.0, 1000.0], "equity dips between ex and pay (the receivable is not marked)");
    let divs: Vec<&absolute_backtest::kernel::ActionRecord> = r.actions.iter().filter(|a| matches!(a.action, Action::Dividend { .. })).collect();
    assert_eq!(divs.len(), 1, "{:?}", r.actions);
    assert_eq!(format_timestamp(divs[0].t), "2024-01-12");
    assert!(close(divs[0].cash, 10.0));
    // A short pays it.
    let r = run(&program(SHORT, "short_hold"), &m.ds, cfg(1000.0)).unwrap();
    let cash: Vec<f64> = r.exposure.iter().map(|e| e.cash).collect();
    assert_eq!(cash, vec![1000.0, 1000.0, 1200.0, 1200.0, 1190.0, 1190.0], "{:?}", cash);
    // The library's total return sees the dividend: (19 + 1) / 20 - 1 = 0 on the ex-date.
    let src = HOLD.replace("decide(T, buy(A, qty)) :- universe(A, T), flat(A, T).", "decide(T, buy(A, qty)) :- universe(A, T), flat(A, T), ret(A, T, R), R = 0.");
    let p = program(&src, "hold");
    let r = run(&p, &m.ds, cfg(1000.0)).unwrap();
    let decided: Vec<String> = r.decisions.iter().map(|d| format_timestamp(d.t)).collect();
    assert_eq!(decided, vec!["2024-01-09", "2024-01-10"], "flat on days 2 (20/20) and 3 (ex-date, total return zero): {:?}", decided);
}

#[test]
fn the_library_adjusts_for_splits_causally() {
    // 40, 40, 20 (2:1 split), 20, 22: total return is 0, 0, 0, 0.1 and the
    // adjusted close is continuous in the first bar's share basis.
    let mut m = market(&[Some(40.0), Some(40.0), Some(20.0), Some(20.0), Some(22.0)]);
    m.ds.add("split", vec![Value::Equity(m.x), Value::Time(m.days[2]), Value::Num(2.0)]);
    let src = r#"
strategy probe {
  env equities_1d_v2
  uses catalog
  resolution @1d
  mode target
  param up : Scalar = 0.05
  param lvl : Price<USD> = 41 USD/share
  decide(T, target_weight(A, 0)) :- ret(A, T, R), R > up.
  decide(T, target_weight(A, 1)) :- close_adj(A, T, P), P > lvl.
}
"#;
    let p = program(src, "probe");
    let r = run(&p, &m.ds, ExecConfig { on_leverage: absolute_backtest::kernel::OnLeverage::Allow, ..cfg(1000.0) }).unwrap();
    let decided: Vec<String> = r.decisions.iter().map(|d| format!("{} {}", format_timestamp(d.t), r.describe_decision(&d.decision))).collect();
    // The split day has zero total return; day 5 is +10%; the adjusted close
    // 44 on day 5 (22 * 2) is above 41, the as-traded 22 is not.
    assert_eq!(decided, vec!["2024-01-12 target_weight(X, 0)", "2024-01-12 target_weight(X, 1)"], "{:?}", decided);
}

#[test]
fn a_delisting_force_closes_at_the_last_trade_with_a_haircut_by_reason() {
    // Held 10 bought at 10; the last trade is 4 on day 2; delisted day 3 on.
    let mut m = market(&[Some(10.0), Some(4.0), None, None, None]);
    let bankrupt = m.ds.intern_label("bankruptcy");
    for &t in &m.days[2..] {
        m.ds.add("delisted", vec![Value::Equity(m.x), Value::Time(t), Value::Label(bankrupt)]);
    }
    let p = program(HOLD, "hold");
    let r = run(&p, &m.ds, cfg(1000.0)).unwrap();
    // Bankruptcy: a full haircut by default; the position is closed at zero.
    assert_eq!(r.fills.len(), 2, "{:?}", r.fills);
    let forced = &r.fills[1];
    assert!(forced.forced && forced.quantity == -10.0 && forced.price == 0.0, "{:?}", forced);
    assert_eq!(format_timestamp(forced.t), "2024-01-10");
    assert!(r.final_positions.is_empty());
    assert!(close(r.final_cash, 900.0));
    assert!(matches!(&r.actions[0].action, Action::Delisting { reason, haircut } if reason == "bankruptcy" && *haircut == 1.0), "{:?}", r.actions);
    assert!(r.dropped.iter().all(|(_, _, why)| why.contains("no price") || why == "no next bar"), "{:?}", r.dropped);
    // An acquisition pays the last trade in full.
    let mut m2 = market(&[Some(10.0), Some(4.0), None, None, None]);
    let acq = m2.ds.intern_label("acquisition");
    for &t in &m2.days[2..] {
        m2.ds.add("delisted", vec![Value::Equity(m2.x), Value::Time(t), Value::Label(acq)]);
    }
    let r = run(&p, &m2.ds, cfg(1000.0)).unwrap();
    assert!(close(r.fills[1].price, 4.0) && close(r.final_cash, 900.0 + 40.0), "{:?}", r.fills);
    // The haircuts are configuration; a zero haircut on an involuntary reason is warned.
    let r = run(&p, &m.ds, ExecConfig { delisting_haircuts: vec![("bankruptcy".into(), 0.0)], ..cfg(1000.0) }).unwrap();
    assert!(close(r.final_cash, 940.0), "{}", r.final_cash);
    assert!(r.warnings.iter().any(|w| w.bias == "delisting"), "{:?}", r.warnings);
    // An unknown reason takes the conservative default.
    let mut m3 = market(&[Some(10.0), Some(4.0), None, None, None]);
    let odd = m3.ds.intern_label("merged-into-spac");
    for &t in &m3.days[2..] {
        m3.ds.add("delisted", vec![Value::Equity(m3.x), Value::Time(t), Value::Label(odd)]);
    }
    let r = run(&p, &m3.ds, cfg(1000.0)).unwrap();
    assert!(close(r.final_cash, 900.0), "{}", r.final_cash);
    assert_eq!(ExecConfig::default().delisting_haircut_default, 1.0);
}

#[test]
fn membership_and_classification_are_point_in_time_relations() {
    let src = r#"
strategy in_index {
  env equities_1d_v2
  uses catalog
  resolution @1d
  mode delta
  param qty : Quantity<Shares> = 10 shares
  param idx : Label = "SPX"
  param sector : Label = "tech"
  decide(T, buy(A, qty)) :- member(A, T, idx), classification(A, T, "scheme", sector), flat(A, T).
}
"#;
    let mut m = market(&[Some(10.0); 4]);
    let spx = m.ds.intern_label("SPX");
    let scheme = m.ds.intern_label("scheme");
    let tech = m.ds.intern_label("tech");
    let fin = m.ds.intern_label("fin");
    // A member from day 2; a tech name until day 3, then fin.
    for (i, &t) in m.days.iter().enumerate() {
        if i >= 1 {
            m.ds.add("member", vec![Value::Equity(m.x), Value::Time(t), Value::Label(spx)]);
        }
        m.ds.add("classification", vec![Value::Equity(m.x), Value::Time(t), Value::Label(scheme), Value::Label(if i < 3 { tech } else { fin })]);
    }
    let p = program(src, "in_index");
    let r = run(&p, &m.ds, cfg(1000.0)).unwrap();
    let decided: Vec<String> = r.decisions.iter().map(|d| format_timestamp(d.t)).collect();
    assert_eq!(decided, vec!["2024-01-09"], "a member and tech only on day 2 while flat: {:?}", decided);
}

#[test]
fn the_synthetic_catalog_market_has_every_event_and_runs_the_corpus_strategy() {
    let ds = synthetic_daily_v2(&["AAA", "BBB", "CCC", "DDD", "SPY"], (2022, 1, 3), 320, 11);
    assert_eq!(ds.securities.len(), 6, "five names, one of which changes ticker: {:?}", ds.securities);
    assert!(ds.facts["split"].len() >= 1);
    assert!(ds.facts["dividend"].len() >= 2);
    assert!(!ds.facts["delisted"].is_empty());
    assert!(ds.facts["member"].len() > 1000 && ds.facts["classification"].len() > 1000);
    for rel in ["open", "high", "low"] {
        assert_eq!(ds.facts[rel].len(), ds.facts["close"].len());
    }
    // The delisted name leaves the universe and its prices stop.
    let delisted_sym = ds.facts["delisted"][0][0].as_equity().unwrap();
    let first_delisted = ds.facts["delisted"].iter().map(|tu| tu[1].as_time().unwrap()).min().unwrap();
    assert!(!ds.facts["universe"].iter().any(|tu| tu[0].as_equity() == Some(delisted_sym) && tu[1].as_time().unwrap() >= first_delisted));
    // Lows at or below closes, highs at or above.
    let src = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/corpus/strategies/total_return_momentum.dsl")).unwrap();
    let p = program(&src, "total_return_momentum");
    let r = run(&p, &ds, ExecConfig::default()).unwrap();
    assert!(!r.decisions.is_empty() && !r.fills.is_empty());
    assert!(r.actions.iter().any(|a| matches!(a.action, Action::Split { .. })) || r.actions.iter().any(|a| matches!(a.action, Action::Dividend { .. })), "{:?}", r.actions.len());
}
