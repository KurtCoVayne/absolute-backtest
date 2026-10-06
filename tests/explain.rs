//! The explain facility of section 7: it answers only for bars of the rule's
//! time domain, takes the rule's inputs, and can be narrowed to one binding.

mod corpus;

use absolute_backtest::check::{check_program, Program, Workspace};
use absolute_backtest::data::{synthetic_daily, synthetic_minute};
use absolute_backtest::kernel::time::parse_timestamp;
use absolute_backtest::kernel::{Dataset, ExecConfig, Kernel, Value};
use absolute_backtest::Duration;

fn program(extra: &str, name: &str) -> (Program, Workspace) {
    let mut ws = corpus::base_workspace();
    ws.add_source(extra).unwrap();
    let (p, diags) = check_program(&ws, name);
    let text: Vec<String> = diags.iter().map(|d| d.to_string()).collect();
    (p.unwrap_or_else(|| panic!("{} should check:\n{}", name, text.join("\n"))), ws)
}

fn corpus_program(name: &str) -> (Program, Workspace) {
    let src = std::fs::read_to_string(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(format!("corpus/strategies/{}.dsl", name))).unwrap();
    program(&src, name)
}

fn rule(prog: &Program, label: &str) -> usize {
    (0..prog.rules.len()).find(|&i| prog.rule_label(i) == label).unwrap_or_else(|| panic!("no rule {}", label))
}

fn ts(s: &str) -> i64 {
    parse_timestamp(s).unwrap()
}

fn ran<'p>(prog: &'p Program, ds: &Dataset) -> Kernel<'p> {
    let mut k = Kernel::new(prog, ds, ExecConfig::default()).unwrap();
    k.run().unwrap();
    k
}

/// trend-10: a Saturday, or a date before the data, is not in the @1d time
/// domain (section 2, section 6), so explain must refuse it naming the
/// nearest bars instead of reporting a body literal failing.
#[test]
fn explain_rejects_a_timestamp_that_is_not_a_bar() {
    let (prog, _) = corpus_program("breakout_52w");
    let ds = synthetic_daily(&["AAA", "BBB"], (2022, 1, 3), 120, 7);
    let mut k = ran(&prog, &ds);
    let decide = rule(&prog, "breakout_52w::decide#1");
    let err = k.explain(decide, ts("2022-04-16"), &[]).expect_err("a Saturday is not a bar").to_string();
    assert!(err.contains("2022-04-16 is not a bar at @1d"), "{}", err);
    assert!(err.contains("2022-04-15") && err.contains("2022-04-18"), "{}", err);
    assert!(!err.contains("internal"), "{}", err);
    let err = k.explain(decide, ts("2019-04-16"), &[]).expect_err("before the data is not a bar").to_string();
    assert!(err.contains("2019-04-16 is not a bar at @1d") && err.contains("2022-01-03"), "{}", err);
    // A real bar is still explained.
    assert!(k.explain(decide, ts("2022-04-14"), &[]).is_ok());
}

/// intraday-01: at a coarser decision resolution the time domain is the set
/// of bucket labels (section 6); a label between two bars must not be
/// evaluated as a phantom bucket.
#[test]
fn explain_rejects_a_phantom_resample_bucket() {
    let src = r#"
strategy bucket_counts {
  env equities_1m
  resolution @1h
  mode delta
  param qty : Quantity<Shares> = 1 shares
  param full : Count = 60
  rel hcount(-A: Equity, @T: Timestamp, -N: Count)
  hcount(A, T, N) :- resample(universe_m(A, T1) to @1h as T, min 1, N = count(T1)).
  decide(T, buy(A, qty)) :- hcount(A, T, N), N < full.
}
"#;
    let (prog, _) = program(src, "bucket_counts");
    let ds = synthetic_minute(&["AAA", "BBB"], (2024, 1, 2), 2, 390, 7);
    let mut k = ran(&prog, &ds);
    let hcount = rule(&prog, "bucket_counts::hcount#1");
    let err = k.explain(hcount, ts("2024-01-02T10:30:00"), &[]).expect_err("10:30 is not an @1h label").to_string();
    assert!(err.contains("2024-01-02T10:30:00 is not a bar at @1h"), "{}", err);
    assert!(err.contains("2024-01-02T10:00:00") && err.contains("2024-01-02T11:00:00"), "{}", err);
    // The 10:00 bucket is a real bar holding the 30 bars from 09:31.
    let ex = k.explain(hcount, ts("2024-01-02T10:00:00"), &[]).unwrap();
    assert_eq!(ex.solutions, 2, "{}", ex);
}

/// Two symbols over five weekdays with crafted closes.
fn two_symbols(x: &[f64], y: &[f64]) -> Dataset {
    let mut ds = Dataset::new();
    let days = ["2024-01-08", "2024-01-09", "2024-01-10", "2024-01-11", "2024-01-12"];
    for (name, prices) in [("X", x), ("Y", y)] {
        let s = ds.intern(name);
        for (d, p) in days.iter().zip(prices) {
            let t = ts(d);
            ds.add("close", vec![Value::Equity(s), Value::Time(t), Value::Num(*p)]);
            ds.add("volume", vec![Value::Equity(s), Value::Time(t), Value::Num(1000.0)]);
            ds.add("universe", vec![Value::Equity(s), Value::Time(t)]);
        }
    }
    ds
}

const UP_DOWN: &str = r#"
strategy up_down {
  env equities_1d
  uses features
  resolution @1d
  mode delta
  param qty : Quantity<Shares> = 10 shares
  rel up(-A: Equity, @T: Timestamp)
  up(A, T) :- universe(A, T), logret(A, T, R), R > 0.
  decide(T, buy(A, qty)) :- up(A, T), flat(A, T).
  decide(T, sell(A, Q)) :- held(A, T, Q), prev(T, T0), decided(T0, buy(A, _)).
}
"#;

/// trend-05: a rule with `+` arguments needs its inputs; asking without them
/// is a usage error naming the inputs and their types, not an internal
/// kernel error. With the inputs the rule is explained.
#[test]
fn explain_without_the_rule_inputs_names_them() {
    let (prog, _) = program(UP_DOWN, "up_down");
    let ds = two_symbols(&[10.0, 11.0, 12.0, 13.0, 14.0], &[10.0, 9.0, 8.0, 7.0, 6.0]);
    let mut k = ran(&prog, &ds);
    let sma = rule(&prog, "features::sma#1");
    let err = k.explain(sma, ts("2024-01-10"), &[]).expect_err("sma takes inputs").to_string();
    assert!(!err.contains("internal"), "{}", err);
    assert!(err.contains("features::sma#1") && err.contains("3 inputs"), "{}", err);
    assert!(err.contains("A: Equity") && err.contains("N: Duration") && err.contains("K: Count"), "{}", err);
    let x = k.symbols.get("X").unwrap();
    let ten_days = Value::Dur(Box::new(Duration { months: 0, days: 10 }));
    let ex = k.explain(sma, ts("2024-01-10"), &[Value::Equity(x), ten_days.clone(), Value::Count(3)]).unwrap();
    assert_eq!(ex.solutions, 1, "{}", ex);
    let ex = k.explain(sma, ts("2024-01-09"), &[Value::Equity(x), ten_days, Value::Count(3)]).unwrap();
    assert!(ex.failed_at.is_some(), "{}", ex);
}

/// trend-04: the first literal with no solution over the union of every
/// binding can hide the literal that fails for one instrument; binding a
/// body variable narrows the explanation to that instrument.
#[test]
fn explain_with_a_binding_narrows_to_one_instrument() {
    let (prog, _) = program(UP_DOWN, "up_down");
    // X falls every day; Y rises every day and is bought on 01-09, so on
    // 01-10 Y is up but held (literal 2 fails) while X is not up (literal 1).
    let ds = two_symbols(&[10.0, 9.0, 8.0, 7.0, 6.0], &[10.0, 11.0, 12.0, 13.0, 14.0]);
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
