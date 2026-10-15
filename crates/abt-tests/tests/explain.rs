//! The explain facility (`query --explain`, `docs/language-v2.md` section
//! 8): it answers only for bars of the rule's time domain, and can be
//! narrowed to one binding.

use abt_tests as common;

use abt_kernel::check::Program;
use abt_kernel::data::{synthetic_daily, synthetic_minute};
use abt_kernel::kernel::time::format_timestamp;
use abt_kernel::kernel::{Dataset, ExecConfig, Kernel, Value};

fn rule(prog: &Program, label: &str) -> usize {
    (0..prog.rules.len()).find(|&i| prog.rule_label(i) == label).unwrap_or_else(|| panic!("no rule {}", label))
}

fn ts(s: &str) -> i64 {
    common::day(s)
}

fn ran<'p>(prog: &'p Program, ds: &Dataset) -> Kernel<'p> {
    let mut k = Kernel::new(prog, ds, ExecConfig::default()).unwrap();
    k.run().unwrap();
    k
}

/// trend-10: a Saturday, or a date before the data, is not in the base
/// calendar's time domain (sections 3.1 and 6), so explain must refuse it
/// naming the nearest bars instead of reporting a body literal failing.
#[test]
fn explain_rejects_a_timestamp_that_is_not_a_bar() {
    let prog = common::corpus_program("breakout_52w");
    let ds = synthetic_daily(&["AAA", "BBB"], (2022, 1, 3), 120, 7);
    let mut k = ran(&prog, &ds);
    let decide = rule(&prog, "breakout_52w::decide#1");
    let err = k.explain(decide, ts("2022-04-16"), &[]).expect_err("a Saturday is not a bar").to_string();
    assert!(err.contains("2022-04-16 is not a bar of calendar base"), "{}", err);
    assert!(err.contains("2022-04-15") && err.contains("2022-04-18"), "{}", err);
    assert!(!err.contains("internal"), "{}", err);
    let err = k.explain(decide, ts("2019-04-16"), &[]).expect_err("before the data is not a bar").to_string();
    assert!(err.contains("2019-04-16 is not a bar of calendar base") && err.contains("2022-01-03"), "{}", err);
    // A real bar is still explained.
    assert!(k.explain(decide, ts("2022-04-14"), &[]).is_ok());
}

/// intraday-01: on a coarser decision calendar the time domain is the set
/// of bucket labels (sections 3.1 and 6); a minute between two daily labels
/// must not be evaluated as a phantom bucket.
#[test]
fn explain_rejects_a_phantom_bucket() {
    let src = common::StrategyParts {
        name: "bucket_counts".into(),
        needs: "need bars @1m   need listing".into(),
        uses: "calendar, book".into(),
        calendars: Some("daily = days".into()),
        clock: "decide on daily".into(),
        universe: "universe(A, T) :- listed(A, T), traded@daily(A, T).".into(),
        params: "    qty : Quantity<Shares> = 1 shares\n    full : Count = 390".into(),
        rules: "    rel mcount(A: Equity, @T, N: Count)\n    mcount(A, T, N) :- universe(A, T), N = count(T1) over (T1 in bucket(daily, T), close(A, T1, _)).".into(),
        decides: "    decide(T, trade(A, qty)) :- mcount(A, T, N), N < full.".into(),
        report: "calendar daily   periods 252 per year".into(),
        ..Default::default()
    }
    .render();
    let prog = common::program(&src, "bucket_counts");
    let ds = synthetic_minute(&["AAA", "BBB"], (2024, 1, 2), 2, 390, 7);
    let mut k = ran(&prog, &ds);
    // A day's label is its last bar; a minute between two labels is no bar.
    let labels = k.bars_at(&prog.decision_cal);
    assert_eq!(labels.len(), 2, "one label a day: {:?}", labels);
    let mcount = rule(&prog, "bucket_counts::mcount#1");
    let err = k.explain(mcount, ts("2024-01-03T10:30:00"), &[]).expect_err("10:30 is not a daily label").to_string();
    assert!(err.contains("2024-01-03T10:30:00 is not a bar of calendar daily"), "{}", err);
    assert!(err.contains(&format_timestamp(labels[0])) && err.contains(&format_timestamp(labels[1])), "{}", err);
    // A label is a real bar holding the day's 390 minute bars of each name.
    let ex = k.explain(mcount, labels[0], &[]).unwrap();
    assert_eq!(ex.solutions, 2, "{}", ex);
}

/// Buy 10 on an up bar when flat, sell the bar after the buy.
fn up_down() -> String {
    common::strategy(
        "up_down",
        "    qty : Quantity<Shares> = 10 shares",
        "    rel up(A: Equity, @T)\n    up(A, T) :- universe(A, T), log_price_return(A, T, R), R > 0.",
        "    decide(T, trade(A, qty)) :- up(A, T), flat(A, T).\n    decide(T, trade(A, Q)) :- held(A, T, Q0), prev(T, T0), decided(T0, trade(A, Q1)), Q1 > 0 shares, Q = -Q0.",
    )
}

/// trend-04: the first literal with no solution over the union of every
/// binding can hide the literal that fails for one instrument; binding a
/// body variable narrows the explanation to that instrument.
#[test]
fn explain_with_a_binding_narrows_to_one_instrument() {
    let prog = common::program(&up_down(), "up_down");
    // X falls every day; Y rises every day and is bought on 01-09, so on
    // 01-10 Y is up but held (literal 2 fails) while X is not up (literal 1).
    let x_closes: &[f64] = &[10.0, 9.0, 8.0, 7.0, 6.0];
    let y_closes: &[f64] = &[10.0, 11.0, 12.0, 13.0, 14.0];
    let ds = common::markets((2024, 1, 8), &[("X", x_closes), ("Y", y_closes)]);
    let mut k = ran(&prog, &ds);
    let decide = rule(&prog, "up_down::decide#1");
    let t = ts("2024-01-10");
    let union = k.explain(decide, t, &[]).unwrap();
    assert_eq!(union.failed_at.as_ref().map(|(i, _)| *i), Some(1), "{}", union);
    let x = k.symbols.get("X").unwrap();
    let y = k.symbols.get("Y").unwrap();
    let for_x = k.explain_with(decide, t, &[], &[("A".to_string(), Value::Equity(x))]).unwrap();
    assert_eq!(for_x.failed_at.as_ref().map(|(i, l)| (*i, l.clone())), Some((0, "up(A, T)".to_string())), "{}", for_x);
    let for_y = k.explain_with(decide, t, &[], &[("A".to_string(), Value::Equity(y))]).unwrap();
    assert_eq!(for_y.failed_at.as_ref().map(|(i, l)| (*i, l.clone())), Some((1, "flat(A, T)".to_string())), "{}", for_y);
    // On 01-09 Y fires; bound to Y the rule has one solution.
    let on_9 = k.explain_with(decide, ts("2024-01-09"), &[], &[("A".to_string(), Value::Equity(y))]).unwrap();
    assert_eq!(on_9.solutions, 1, "{}", on_9);
    // A variable the rule does not have is a usage error naming the rule.
    let err = k.explain_with(decide, t, &[], &[("Z".to_string(), Value::Count(1))]).expect_err("no variable Z").to_string();
    assert!(err.contains("`Z`") && err.contains("up_down::decide#1") && !err.contains("internal"), "{}", err);
}
