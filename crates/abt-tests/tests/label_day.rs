//! A library rule is calendar-polymorphic (language-v2, section 7): on the
//! calendar it is instantiated at, every bar it names is that calendar's
//! bar view, at the rule's own time and at any other (Proposition 3.4: a
//! body atom outside a bucket aggregate has the head's calendar). So on a
//! weekly calendar the close at an entity's previous row is that week's
//! last close, whether or not the name traded on the week's label day
//! (ERRATA E-144: the linker read it as the daily close on the label day,
//! and a name halted that day lost its return at the next row and its
//! total-return index, the book's execution price, from then on).
//!
//! Two names over three weeks, hand-computed. X closes at 100, 101, ...
//! every day. Y closes at 50, 51, ... and has no bar on Friday 2024-01-12,
//! the first week's label: its weekly closes are 53 (Thursday's), 59 and 64.

#![allow(clippy::result_large_err)]

use abt_kernel::check::Program;
use abt_kernel::kernel::time::{format_timestamp, parse_timestamp};
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
        if i != 4 {
            common::add_bar(&mut ds, y, *t, 50.0 + i as f64, 1000.0);
        }
    }
    ds.derive_tickers();
    ds
}

/// A weekly book on the total-return price, as MW14's: every name is bought
/// at its second row. `seen` links the bars module's relations.
fn weekly() -> Program {
    let src = StrategyParts {
        name: "w".into(),
        needs: "need bars @1d   need actions, listing".into(),
        uses: "calendar, catalog, bars, book".into(),
        calendars: Some("weekly = weeks(1)".into()),
        clock: "decide on weekly".into(),
        universe: "universe(A, T) :- traded@weekly(A, T).".into(),
        params: "    w : Scalar = 0.25".into(),
        rules: "    rel seen(A: Equity, @T, R: Scalar, G: Scalar, GP: Price<USD>, V: Price<USD>)
    seen(A, T, R, G, GP, V) :- price_return(A, T, R), gap(A, T, G), gap_points(A, T, GP), true_range(A, T, V)."
            .into(),
        orders: "moc".into(),
        decides: "    decide(T, hold(A, w)) :- row_number(A, T, K), K >= 1, not held(A, T, _).".into(),
        execution: "book_fixed_base".into(),
        report: "calendar weekly   periods 52 per year".into(),
        ..Default::default()
    }
    .render();
    common::program(&src, "w")
}

/// The same library relations on the base calendar.
fn daily() -> Program {
    let src = StrategyParts {
        name: "d".into(),
        needs: "need bars @1d   need actions, listing".into(),
        uses: "calendar, catalog, bars, book".into(),
        params: "    qty : Quantity<Shares> = 1 shares".into(),
        decides: "    decide(T, trade(A, qty)) :- total_return(A, T, R), price_return(A, T, R1), R > 0, R1 > 0, flat(A, T).".into(),
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

/// The body of the one linked rule of `head` that reads `prev_row`.
fn stepping_rule(p: &Program, head: &str) -> Vec<String> {
    let rule = p
        .rules
        .iter()
        .find(|r| r.head.name == head && r.body.iter().any(|l| l.describe().starts_with("prev_row(")))
        .unwrap_or_else(|| panic!("no rule of `{}` steps through prev_row", head));
    rule.body.iter().map(|l| l.describe()).collect()
}

#[test]
fn a_weekly_library_rule_reads_the_weeks_bar_at_the_previous_row() {
    let p = weekly();
    for (head, atom) in [
        ("total_return", "close@weekly(A, T0, P0)"),
        ("price_return", "close@weekly(A, T0, P0)"),
        ("gap", "close@weekly(A, T0, C)"),
        ("gap_points", "close@weekly(A, T0, C)"),
        ("true_range", "close@weekly(A, T0, C)"),
    ] {
        let body = stepping_rule(&p, head);
        assert!(body.iter().any(|l| l == atom), "{}: {:?}", head, body);
        assert!(!body.iter().any(|l| l.starts_with("close(")), "{} reads the daily close: {:?}", head, body);
    }
}

#[test]
fn a_name_without_a_bar_on_the_label_day_keeps_its_return_and_its_index() {
    let p = weekly();
    let (_, mut k) = common::run_kernel(&p, &market(), ExecConfig::from_program(&p)).unwrap();
    let [w1, w2, w3] = LABELS;
    // The week's close is its last close: Thursday's 53 for Y in the first week.
    near(nums(&mut k, "close@weekly", w1, "Y"), &[53.0], "Y's first weekly close");
    near(nums(&mut k, "close@weekly", w1, "X"), &[104.0], "X's first weekly close");
    // The return of the second week steps from the first week's close.
    assert_eq!(nums(&mut k, "total_return", w1, "Y"), None, "a first row has no return");
    near(nums(&mut k, "total_return", w2, "Y"), &[59.0 / 53.0 - 1.0], "Y's return of the second week");
    near(nums(&mut k, "total_return", w3, "Y"), &[64.0 / 59.0 - 1.0], "Y's return of the third week");
    near(nums(&mut k, "total_return", w2, "X"), &[109.0 / 104.0 - 1.0], "X's return of the second week");
    near(nums(&mut k, "total_return", w3, "X"), &[114.0 / 109.0 - 1.0], "X's return of the third week");
    // The index starts at the first weekly close and compounds the returns:
    // with no action it is the weekly close.
    near(nums(&mut k, "tr_index", w1, "Y"), &[53.0], "Y's index at its first row");
    near(nums(&mut k, "tr_index", w2, "Y"), &[59.0], "Y's index at the second week");
    near(nums(&mut k, "tr_index", w3, "Y"), &[64.0], "Y's index at the third week");
    near(nums(&mut k, "tr_index", w3, "X"), &[114.0], "X's index at the third week");
    // The bars module's steps from the previous close, the same: Y's second
    // week opens at 55 and ranges over 55 to 59 after a close of 53.
    near(nums(&mut k, "price_return", w2, "Y"), &[59.0 / 53.0 - 1.0], "Y's price return");
    near(nums(&mut k, "gap", w2, "Y"), &[55.0 / 53.0 - 1.0], "Y's gap");
    near(nums(&mut k, "gap_points", w2, "Y"), &[2.0], "Y's gap in points");
    near(nums(&mut k, "true_range", w2, "Y"), &[6.0], "Y's true range");
    near(nums(&mut k, "true_range", w2, "X"), &[5.0], "X's true range");
}

/// The book, with the whole history and with the stores trimmed behind the
/// lookback as `abt run` trims them (E-126).
#[test]
fn the_book_fills_such_a_name_at_its_index() {
    let p = weekly();
    for trim_stores in [false, true] {
        let cfg = ExecConfig {
            trim_stores,
            ..ExecConfig::from_program(&p)
        };
        let r = common::run(&p, &market(), cfg).unwrap();
        let dropped: Vec<String> = r.dropped.iter().map(|(t, d, why)| format!("{} {} {}", format_timestamp(*t), r.describe_decision(d), why)).collect();
        assert!(dropped.is_empty(), "{:?}", dropped);
        // Both names are bought at their second row, a quarter of the fixed
        // million each, at the index: 109 and 59.
        let fills: Vec<(String, String, f64, f64)> = r.fills.iter().map(|f| (format_timestamp(f.t), r.symbols[f.equity as usize].clone(), f.price, f.quantity)).collect();
        assert_eq!(fills.len(), 2, "{:?}", fills);
        for ((day, name, price, quantity), (want_name, want_price)) in fills.iter().zip([("X", 109.0), ("Y", 59.0)]) {
            assert_eq!((day.as_str(), name.as_str()), (LABELS[1], want_name), "{:?}", fills);
            assert!((price - want_price).abs() <= 1e-9, "{:?}", fills);
            assert!((quantity - 250_000.0 / want_price).abs() <= 1e-6, "{:?}", fills);
        }
    }
}

/// A daily program's rows are the bars a name has, so its previous row has
/// a close: the same rules read the primitive itself and step over the
/// missing day.
#[test]
fn a_daily_program_reads_the_primitive_and_steps_over_a_missing_day() {
    let p = daily();
    for (head, atom) in [("total_return", "close(A, T0, P0)"), ("price_return", "close(A, T0, P0)")] {
        let body = stepping_rule(&p, head);
        assert!(body.iter().any(|l| l == atom), "{}: {:?}", head, body);
    }
    let (_, mut k) = common::run_kernel(&p, &market(), ExecConfig::from_program(&p)).unwrap();
    // Y has no row on Friday the 12th; Monday's return steps from Thursday's 53.
    assert_eq!(nums(&mut k, "total_return", "2024-01-12", "Y"), None);
    near(nums(&mut k, "total_return", "2024-01-15", "Y"), &[55.0 / 53.0 - 1.0], "Y's return over the missing day");
    near(nums(&mut k, "price_return", "2024-01-15", "Y"), &[55.0 / 53.0 - 1.0], "Y's price return over the missing day");
    near(nums(&mut k, "total_return", "2024-01-15", "X"), &[105.0 / 104.0 - 1.0], "X's return");
}
