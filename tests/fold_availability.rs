//! Availability time per tuple (data-bundle doc, sections 2 and 4, and the
//! semantic model's v2 item 1): a tuple is available from its own
//! availability time, which may be after its bar's close; the fold reads it
//! from then on, nothing is rewound, and the causality theorem is judged on
//! availability: the fold's decisions at t equal the batch kernel's on the
//! tuples available at t.

mod corpus;

use std::fs;

use absolute_backtest::check::{check_program, Program};
use absolute_backtest::data::{load_csv_dir, synthetic_daily, write_csv_dir};
use absolute_backtest::kernel::time::{format_timestamp, parse_timestamp};
use absolute_backtest::kernel::{run, run_fold, verify_causality, Dataset, ExecConfig, Value};

fn program(src: &str, name: &str) -> Program {
    let mut ws = corpus::base_workspace();
    ws.add_source(src).unwrap();
    let (p, diags) = check_program(&ws, name);
    let text: Vec<String> = diags.iter().map(|d| d.to_string()).collect();
    p.unwrap_or_else(|| panic!("{} should check:\n{}", name, text.join("\n")))
}

const SYMS: [&str; 5] = ["AAA", "BBB", "CCC", "DDD", "SPY"];

/// The synthetic market with every tenth close of each name available one
/// bar late (at the next bar's close).
fn delayed() -> Dataset {
    let base = synthetic_daily(&SYMS, (2022, 1, 3), 200, 7);
    let mut bars: Vec<i64> = base.facts["universe"].iter().map(|tu| tu[1].as_time().unwrap()).collect();
    bars.sort();
    bars.dedup();
    let mut ds = Dataset::new();
    ds.symbols = base.symbols.clone();
    ds.labels = base.labels.clone();
    for (rel, tuples) in &base.facts {
        for (i, tu) in tuples.iter().enumerate() {
            let key = tu[1].as_time().unwrap();
            if rel == "close" && i % 10 == 3 {
                let k = bars.iter().position(|&b| b == key).unwrap();
                let avail = bars.get(k + 1).copied().unwrap_or(key);
                ds.add_available(rel, tu.clone(), avail);
            } else {
                ds.add(rel, tu.clone());
            }
        }
    }
    ds
}

#[test]
fn the_fold_is_causal_on_availability_and_the_batch_kernel_is_not() {
    let p = program(
        &fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/corpus/strategies/sma_crossover.dsl")).unwrap(),
        "sma_crossover",
    );
    let ds = delayed();
    assert!(ds.has_availability());
    let full = run_fold(&p, &ds, ExecConfig::frictionless()).unwrap();
    let batch = run(&p, &ds, ExecConfig::frictionless()).unwrap();
    let describe = |r: &absolute_backtest::kernel::RunResult, t: i64| -> Vec<String> {
        let mut v: Vec<String> = r.decisions_at(t).into_iter().map(|d| r.describe_decision(d)).collect();
        v.sort();
        v
    };
    let mut differs = 0;
    for &t in &full.bars {
        // What was available at t: the fold over that subset decides at t as
        // the full fold did (the executor's own fills honour availability too,
        // which the batch kernel's do not).
        let available = ds.truncated(&p, t);
        let partial = run_fold(&p, &available, ExecConfig::frictionless()).unwrap();
        assert_eq!(describe(&full, t), describe(&partial, t), "at {}", format_timestamp(t));
        if describe(&full, t) != describe(&batch, t) {
            differs += 1;
        }
    }
    assert!(differs > 0, "the batch kernel read the late closes as if they were on time");
    // The empirical check of the theorem uses the fold and availability.
    let samples: Vec<i64> = [50usize, 100, 150, 199].iter().map(|&i| full.bars[i]).collect();
    assert!(verify_causality(&p, &ds, ExecConfig::frictionless(), &samples).unwrap().is_empty());
}

#[test]
fn a_late_tuple_is_read_from_its_availability_on() {
    // One name, prices 10, 11, 12, 13, 14; the close of day 2 arrives at the
    // close of day 3. A rule reading today's close sees nothing on day 2; a
    // rule reading yesterday's close sees it on day 3.
    let days: Vec<i64> = ["2024-01-08", "2024-01-09", "2024-01-10", "2024-01-11", "2024-01-12"]
        .iter()
        .map(|d| parse_timestamp(d).unwrap())
        .collect();
    let mut ds = Dataset::new();
    let x = ds.intern("X");
    for (i, &t) in days.iter().enumerate() {
        let p = 10.0 + i as f64;
        if i == 1 {
            ds.add_available("close", vec![Value::Equity(x), Value::Time(t), Value::Num(p)], days[2]);
        } else {
            ds.add("close", vec![Value::Equity(x), Value::Time(t), Value::Num(p)]);
        }
        ds.add("volume", vec![Value::Equity(x), Value::Time(t), Value::Num(1000.0)]);
        ds.add("universe", vec![Value::Equity(x), Value::Time(t)]);
    }
    ds.derive_tickers();
    let src = r#"
strategy reads {
  env equities_1d
  uses features
  resolution @1d
  mode target
  param w : Scalar = 0.1
  rel today(@T: Timestamp)
  today(T) :- universe(A, T), close(A, T, _).
  rel yesterday(@T: Timestamp)
  yesterday(T) :- universe(A, T), prev(T, T0), close(A, T0, _).
  decide(T, target_weight(A, w)) :- universe(A, T), today(T), yesterday(T).
}
"#;
    let p = program(src, "reads");
    let r = run_fold(
        &p,
        &ds,
        ExecConfig {
            initial_cash: 1000.0,
            ..ExecConfig::frictionless()
        },
    )
    .unwrap();
    let decided: Vec<String> = r.decisions.iter().map(|d| format_timestamp(d.t)).collect();
    // Day 2: its close is not available, so `today` fails. Day 3: today's
    // close and, by then, yesterday's late one. Days 4 and 5 as usual.
    assert_eq!(decided, vec!["2024-01-10", "2024-01-11", "2024-01-12"], "{:?}", decided);
    // The batch kernel, blind to availability, decides on day 2 as well.
    let b = run(
        &p,
        &ds,
        ExecConfig {
            initial_cash: 1000.0,
            ..ExecConfig::frictionless()
        },
    )
    .unwrap();
    assert_eq!(b.decisions.len(), 4);
    // The first decision, day 3's, fills at day 4's close.
    assert!((r.fills[0].price - 13.0).abs() < 1e-9 && format_timestamp(r.fills[0].t) == "2024-01-11", "{:?}", r.fills);
}

#[test]
fn availability_loads_from_csv_and_truncation_follows_it() {
    let p = program(
        &fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/corpus/strategies/sma_crossover.dsl")).unwrap(),
        "sma_crossover",
    );
    let ds = delayed();
    let dir = std::env::temp_dir().join(format!("abt-avail-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    write_csv_dir(&p, &ds, &dir).unwrap();
    let close = fs::read_to_string(dir.join("close.csv")).unwrap();
    assert!(close.lines().next().unwrap().ends_with(",available_at"), "{}", close.lines().next().unwrap());
    let (loaded, _) = load_csv_dir(&p, &dir).unwrap();
    assert!(loaded.has_availability());
    assert_eq!(loaded.availability_of("close").unwrap().len(), ds.facts["close"].len());
    let a = run_fold(&p, &ds, ExecConfig::frictionless()).unwrap();
    let b = run_fold(&p, &loaded, ExecConfig::frictionless()).unwrap();
    assert_eq!(a.decisions.len(), b.decisions.len());
    assert_eq!(a.final_cash.to_bits(), b.final_cash.to_bits());
    // Truncation at t keeps what was available at t, not what was keyed at t.
    let t = a.bars[33];
    let keyed: usize = ds.facts["close"].iter().filter(|tu| tu[1].as_time().unwrap() <= t).count();
    let available = ds.truncated(&p, t).facts["close"].len();
    assert!(available < keyed, "{} available of {} keyed at {}", available, keyed, format_timestamp(t));
    let _ = fs::remove_dir_all(&dir);
}
