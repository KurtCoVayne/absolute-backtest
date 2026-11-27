//! An action goes ex in the bucket of a bar (language-v2, Definition 3.2
//! and section 3.3): on a calendar coarser than the base, a dividend going
//! ex on any day of the period is the period's dividend, as a split going
//! ex in the period is the period's split (`split_factor` reads `split` as
//! the latest in the label's bucket). The catalog's `dividend_at` read only
//! the dividends whose ex-date was the label itself (ERRATA E-145: on the
//! weekly calendar a dividend going ex on a Tuesday was lost, and with it
//! the week's total return and the index the book fills at).
//!
//! Two names over three weeks, hand-computed. X closes at 100, 101, ...
//! every day and pays 1 going ex on Tuesday 2024-01-16. Y closes at 50,
//! 51, ..., pays 0.5 announced on Wednesday 2024-01-10 and going ex on
//! Saturday 2024-01-13, which is no bar, and splits 2:1 on Wednesday
//! 2024-01-24, from which its closes halve.

#![allow(clippy::result_large_err)]

use abt_kernel::check::{Code, Program};
use abt_kernel::kernel::time::parse_timestamp;
use abt_kernel::kernel::{Dataset, ExecConfig, Kernel, Value};
use abt_tests as common;
use abt_tests::StrategyParts;

const LABELS: [&str; 3] = ["2024-01-12", "2024-01-19", "2024-01-26"];

fn market() -> Dataset {
    let mut ds = Dataset::new();
    let bars = abt_kernel::data::business_days((2024, 1, 8), 15);
    let x = ds.add_security("X", "X", bars[0], None);
    let y = ds.add_security("Y", "Y", bars[0], None);
    for (i, t) in bars.iter().enumerate() {
        common::add_bar(&mut ds, x, *t, 100.0 + i as f64, 1000.0);
        let p = 50.0 + i as f64;
        common::add_bar(&mut ds, y, *t, if i >= 12 { p / 2.0 } else { p }, 1000.0);
    }
    let day = |s: &str| Value::Time(parse_timestamp(s).unwrap());
    // Announced on Monday, ex on Tuesday, paid on the last day.
    ds.add("dividend", vec![Value::Equity(x), day("2024-01-15"), day("2024-01-16"), day("2024-01-26"), Value::Num(1.0)]);
    // Announced on Wednesday, ex on the Saturday after.
    ds.add("dividend", vec![Value::Equity(y), day("2024-01-10"), day("2024-01-13"), day("2024-01-26"), Value::Num(0.5)]);
    ds.add("split", vec![Value::Equity(y), day("2024-01-24"), Value::Num(2.0)]);
    ds.derive_tickers();
    ds
}

/// A weekly book on the total-return price, as MW14's; `seen` links the
/// catalog's relations at the weekly calendar.
fn weekly() -> Program {
    let src = StrategyParts {
        name: "w".into(),
        needs: "need bars @1d   need actions, listing".into(),
        uses: "calendar, catalog, book".into(),
        calendars: Some("weekly = weeks(1)".into()),
        clock: "decide on weekly".into(),
        universe: "universe(A, T) :- traded@weekly(A, T).".into(),
        params: "    w : Scalar = 0.25".into(),
        rules: "    rel seen(A: Equity, @T, D: Price<USD>, F: Scalar, R: Scalar)
    seen(A, T, D, F, R) :- dividend_or_zero(A, T, D), split_factor(A, T, F), total_return(A, T, R)."
            .into(),
        orders: "moc".into(),
        decides: "    decide(T, hold(A, w)) :- seen(A, T, _, _, _), not held(A, T, _).".into(),
        execution: "book_fixed_base".into(),
        report: "calendar weekly   periods 52 per year".into(),
        ..Default::default()
    }
    .render();
    common::program(&src, "w")
}

/// The same relations on the base calendar.
fn daily() -> Program {
    let src = StrategyParts {
        name: "d".into(),
        needs: "need bars @1d   need actions, listing".into(),
        uses: "calendar, catalog, bars, book".into(),
        params: "    qty : Quantity<Shares> = 1 shares".into(),
        decides: "    decide(T, trade(A, qty)) :- total_return(A, T, R), R > 0, flat(A, T).".into(),
        ..Default::default()
    }
    .render();
    common::program(&src, "d")
}

/// The numbers of `rel`'s tuple for `name` at `day`, if it has one.
fn nums(k: &mut Kernel, rel: &str, day: &str, name: &str) -> Option<Vec<f64>> {
    let tuples = k.query(rel, parse_timestamp(day).unwrap(), &[]).unwrap();
    let found: Vec<Vec<f64>> = tuples
        .iter()
        .filter(|tu| tu.iter().any(|v| matches!(v, Value::Equity(_)) && k.show(v) == name))
        .map(|tu| {
            tu.iter()
                .filter_map(|v| match v {
                    Value::Num(x) => Some(*x),
                    _ => None,
                })
                .collect()
        })
        .collect();
    assert!(found.len() <= 1, "{} holds {} tuples of {} at {}", rel, found.len(), name, day);
    found.into_iter().next()
}

fn near(got: Option<Vec<f64>>, want: &[f64], what: &str) {
    let got = got.unwrap_or_else(|| panic!("{}: no tuple", what));
    assert_eq!(got.len(), want.len(), "{}: {:?}", what, got);
    for (g, w) in got.iter().zip(want) {
        assert!((g - w).abs() <= 1e-12 * w.abs().max(1.0), "{}: {:?}, expected {:?}", what, got, want);
    }
}

#[test]
fn a_weekly_row_takes_every_action_going_ex_in_its_week() {
    let p = weekly();
    let (_, mut k) = common::run_kernel(&p, &market(), ExecConfig::from_program(&p)).unwrap();
    let [w1, w2, w3] = LABELS;
    // X: weekly closes 104, 109, 114; the Tuesday dividend is the second week's.
    assert_eq!(nums(&mut k, "dividend_at", w1, "X"), None, "no dividend goes ex in the first week");
    near(nums(&mut k, "dividend_at", w2, "X"), &[1.0], "X's dividend of the second week");
    assert_eq!(nums(&mut k, "dividend_at", w3, "X"), None, "no dividend goes ex in the third week");
    near(nums(&mut k, "total_return", w2, "X"), &[(109.0 + 1.0) / 104.0 - 1.0], "X's return of the second week");
    near(nums(&mut k, "total_return", w3, "X"), &[114.0 / 109.0 - 1.0], "X's return of the third week");
    near(nums(&mut k, "tr_index", w2, "X"), &[110.0], "X's index at the second week");
    near(nums(&mut k, "tr_index", w3, "X"), &[110.0 * 114.0 / 109.0], "X's index at the third week");
    // Y: weekly closes 54, 59 and 32 after the split. Saturday's dividend
    // is in the bucket of the next label, the second week's.
    near(nums(&mut k, "dividend_at", w2, "Y"), &[0.5], "Y's Saturday dividend, at the next label");
    near(nums(&mut k, "total_return", w2, "Y"), &[(59.0 + 0.5) / 54.0 - 1.0], "Y's return of the second week");
    // The split of the third week's Wednesday, read as the latest in the bucket.
    near(nums(&mut k, "seen", w3, "Y"), &[0.0, 2.0, 32.0 * 2.0 / 59.0 - 1.0], "Y's actions and return of the third week");
    near(nums(&mut k, "tr_index", w3, "Y"), &[59.5 * 64.0 / 59.0], "Y's index at the third week");
}

/// On the base calendar a bar's bucket is the bar: a dividend counts on
/// its ex-date, as before, and one announced on a bar and going ex on a
/// day with no bar counts at the next bar, the first close after it is ex.
/// One keyed at such a day would make the day a bar of the base and be
/// lost there (E-145).
#[test]
fn a_daily_row_takes_the_dividends_going_ex_since_the_previous_bar() {
    let p = daily();
    let (_, mut k) = common::run_kernel(&p, &market(), ExecConfig::from_program(&p)).unwrap();
    assert_eq!(nums(&mut k, "dividend_at", "2024-01-15", "X"), None);
    near(nums(&mut k, "dividend_at", "2024-01-16", "X"), &[1.0], "X's dividend at its ex-date");
    assert_eq!(nums(&mut k, "dividend_at", "2024-01-17", "X"), None);
    near(nums(&mut k, "total_return", "2024-01-16", "X"), &[(106.0 + 1.0) / 105.0 - 1.0], "X's return at its ex-date");
    assert_eq!(nums(&mut k, "dividend_at", "2024-01-12", "Y"), None, "Friday's close is not ex yet");
    near(nums(&mut k, "dividend_at", "2024-01-15", "Y"), &[0.5], "Y's Saturday dividend, at Monday's bar");
    near(nums(&mut k, "total_return", "2024-01-15", "Y"), &[(55.0 + 0.5) / 54.0 - 1.0], "Y's return over the weekend");
    assert_eq!(nums(&mut k, "dividend_at", "2024-01-16", "Y"), None);
}

/// A minute bundle, thirty bars a day from 09:31, whose one name pays 0.25
/// going ex on Thursday 2024-01-04, announced on the Tuesday.
fn minute_market() -> Dataset {
    let mut ds = abt_kernel::data::synthetic_minute(&["AAA"], (2024, 1, 2), 5, 30, 1);
    let a = ds.intern("AAA");
    let day = |s: &str| Value::Time(parse_timestamp(s).unwrap());
    ds.add("dividend", vec![Value::Equity(a), day("2024-01-02"), day("2024-01-04"), day("2024-01-08"), Value::Num(0.25)]);
    ds
}

/// A program on that bundle reading `dividend_at`, deciding on `clock`.
fn on_minutes(clock: &str) -> String {
    StrategyParts {
        name: "m".into(),
        needs: "need bars @1m   need actions, listing".into(),
        uses: "calendar, catalog, book".into(),
        calendars: Some("daily = days".into()),
        clock: format!("decide on {}", clock),
        universe: format!("universe(A, T) :- listed(A, T), traded@{}(A, T).", clock),
        params: "    qty : Quantity<Shares> = 1 shares".into(),
        rules: "    rel seen(A: Equity, @T, D: Price<USD>)\n    seen(A, T, D) :- dividend_at(A, T, D).".into(),
        decides: "    decide(T, trade(A, qty)) :- seen(A, T, _), flat(A, T).".into(),
        report: "calendar daily   periods 252 per year".into(),
        ..Default::default()
    }
    .render()
}

/// On a calendar finer than the actions' (a minute base deciding on the
/// base), the window of `dividend_at` read `dividend` as of each of its
/// bars: the one dividend stood at every bar from its key on, and the first
/// minute of its ex-date summed it once per bar, 61 times (15.25). The
/// checker refuses such a window (E-145). On the daily calendar of the same
/// bundle each bar of the window reads its own bucket, and the dividend
/// counts once, on the day it goes ex.
#[test]
fn a_window_on_a_calendar_finer_than_the_actions_is_refused() {
    let (p, diags) = common::check(&on_minutes("base"), "m");
    assert!(p.is_none(), "the program on the minute calendar checks");
    let errs = common::errors(&diags);
    assert!(
        errs.iter().any(|(c, m)| *c == Code::X && m.contains("`dividend` is held at") && m.contains("once per bar")),
        "{}",
        common::text(&diags)
    );
    let p = common::program(&on_minutes("daily"), "m");
    let (_, mut k) = common::run_kernel(&p, &minute_market(), ExecConfig::from_program(&p)).unwrap();
    assert_eq!(nums(&mut k, "dividend_at", "2024-01-03T10:00:00", "AAA"), None);
    near(nums(&mut k, "dividend_at", "2024-01-04T10:00:00", "AAA"), &[0.25], "the dividend on its ex-date");
    assert_eq!(nums(&mut k, "dividend_at", "2024-01-05T10:00:00", "AAA"), None);
}

/// The test after the calendar's previous bar is applied to the cached
/// rows of the window, as the test at the bar is (E-75): one group of the
/// window cache per name, each bar solved once, and the run equal to the
/// uncached one.
#[test]
fn the_dividend_window_keeps_one_group_per_name() {
    let p = daily();
    let names = ["AAA", "XYZ", "BBB", "SPY", "DDD"];
    let ds = abt_kernel::data::synthetic_daily(&names, (2022, 1, 3), 300, 5);
    let cfg = ExecConfig {
        pushdown: false,
        ..ExecConfig::from_program(&p)
    };
    let cached = common::run(&p, &ds, cfg.clone()).unwrap();
    let uncached = common::run(&p, &ds, ExecConfig { window_cache: false, ..cfg }).unwrap();
    let fills = |r: &abt_kernel::kernel::RunResult| r.fills.iter().map(|f| (f.t, f.equity, f.quantity.to_bits(), f.price.to_bits())).collect::<Vec<_>>();
    assert_eq!(fills(&cached), fills(&uncached));
    assert!(!cached.fills.is_empty());
    assert!(cached.stats.window_groups <= names.len(), "{:?}", cached.stats);
    let rows = ds.facts["close"].len();
    assert!(cached.stats.window_bars_solved <= rows, "{:?} over {} rows", cached.stats, rows);
}
