//! The explain facility of section 7: it answers only for bars of the rule's
//! time domain, takes the rule's inputs, and can be narrowed to one binding.

mod corpus;

use absolute_backtest::check::{check_program, Program, Workspace};
use absolute_backtest::data::{synthetic_daily, synthetic_minute};
use absolute_backtest::kernel::time::parse_timestamp;
use absolute_backtest::kernel::{Dataset, ExecConfig, Kernel};

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
