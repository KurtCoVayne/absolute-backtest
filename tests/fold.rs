//! The fold kernel (data-bundle doc, section 2) agrees with the batch kernel
//! to the bit, and a fold stopped early is the batch run on the truncated
//! dataset: the causality theorem, operationally.

mod corpus;

use std::fs;
use std::path::Path;

use absolute_backtest::check::{check_program, Program};
use absolute_backtest::data::{synthetic_daily, synthetic_daily_v2, synthetic_minute};
use absolute_backtest::kernel::{run, run_fold, Dataset, Event, EventLog, ExecConfig, Fold, Kernel, RunResult, SimExecutor, Value};

fn program(src: &str, name: &str) -> Program {
    let mut ws = corpus::base_workspace();
    ws.add_source(src).unwrap();
    let (p, diags) = check_program(&ws, name);
    let text: Vec<String> = diags.iter().map(|d| d.to_string()).collect();
    p.unwrap_or_else(|| panic!("{} should check:\n{}", name, text.join("\n")))
}

fn corpus_strategies() -> Vec<(String, Program)> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("corpus/strategies");
    let mut files: Vec<_> = fs::read_dir(dir).unwrap().map(|e| e.unwrap().path()).collect();
    files.sort();
    files
        .into_iter()
        .map(|f| {
            let name = f.file_stem().unwrap().to_string_lossy().to_string();
            let src = fs::read_to_string(&f).unwrap();
            (name.clone(), program(&src, &name))
        })
        .collect()
}

const SYMS: [&str; 5] = ["AAA", "BBB", "CCC", "DDD", "SPY"];

fn dataset_for(p: &Program, seed: u64) -> Dataset {
    if p.relations.values().any(|s| s.res == Some(absolute_backtest::Resolution::M1)) {
        synthetic_minute(&SYMS, (2024, 1, 2), 30, 320, seed)
    } else if p.relations.contains_key("split") {
        synthetic_daily_v2(&SYMS, (2022, 1, 3), 320, seed)
    } else {
        synthetic_daily(&SYMS, (2022, 1, 3), 320, seed)
    }
}

/// Everything a run produced, as comparable text and bits.
fn digest(r: &RunResult) -> Vec<String> {
    let mut out = Vec::new();
    out.push(format!("bars {:?}", r.bars));
    for d in &r.decisions {
        out.push(format!("decision {} {} {}", d.t, r.describe_decision(&d.decision), d.rule));
    }
    for f in &r.fills {
        out.push(format!(
            "fill {} {} {:x} {:x} {} {:x} {:x} {:x} {:x} {:x} {} {}",
            f.t,
            r.symbols[f.equity as usize],
            f.quantity.to_bits(),
            f.price.to_bits(),
            f.at_last_price,
            f.commission.to_bits(),
            f.fee.to_bits(),
            f.slippage.to_bits(),
            f.impact.to_bits(),
            f.participation.to_bits(),
            f.partial,
            f.forced
        ));
    }
    for (t, d, why) in &r.dropped {
        out.push(format!("dropped {} {} {}", t, r.describe_decision(d), why));
    }
    for (t, e) in &r.equity_curve {
        out.push(format!("equity {} {:x}", t, e.to_bits()));
    }
    for e in &r.exposure {
        out.push(format!(
            "exposure {} {:x} {:x} {:x} {:x}",
            e.t,
            e.cash.to_bits(),
            e.gross.to_bits(),
            e.net.to_bits(),
            e.leverage.to_bits()
        ));
    }
    for a in &r.actions {
        out.push(format!("action {} {} {:?} {:x}", a.t, r.symbols[a.equity as usize], a.action, a.cash.to_bits()));
    }
    out.push(format!("costs {:?}", r.costs));
    out.push(format!("liquidity {:?}", r.liquidity));
    out.push(format!("funding {:?}", r.funding));
    out.push(format!("warnings {:?}", r.warnings));
    out.push(format!("final_cash {:x}", r.final_cash.to_bits()));
    out.push(format!("final_positions {:?}", r.final_positions));
    out
}

fn assert_same(name: &str, a: &RunResult, b: &RunResult) {
    let (da, db) = (digest(a), digest(b));
    if da != db {
        let first = da.iter().zip(&db).position(|(x, y)| x != y).unwrap_or(da.len().min(db.len()));
        panic!(
            "{}: batch and fold differ at line {} of {} / {}:\n  batch: {}\n  fold:  {}",
            name,
            first,
            da.len(),
            db.len(),
            da.get(first).cloned().unwrap_or_default(),
            db.get(first).cloned().unwrap_or_default()
        );
    }
}

#[test]
fn the_fold_equals_the_batch_kernel_on_every_corpus_strategy() {
    for (name, p) in corpus_strategies() {
        let ds = dataset_for(&p, 11);
        let a = run(&p, &ds, ExecConfig::default()).unwrap_or_else(|e| panic!("{}: {}", name, e));
        let b = run_fold(&p, &ds, ExecConfig::default()).unwrap_or_else(|e| panic!("{} (fold): {}", name, e));
        assert_same(&name, &a, &b);
        assert!(!b.decisions.is_empty() || name == "volume_spike" || name == "breakout_52w", "{} decided nothing", name);
    }
}

#[test]
fn the_fold_equals_the_batch_kernel_on_crafted_markets() {
    // Missing prices, a liquidation at the last price, a delisting, a split
    // and a dividend: the crafted cases of the executor tests.
    let round_trip = r#"
strategy round_trip {
  env equities_1d
  uses features
  resolution @1d
  mode delta
  param qty : Quantity<Shares> = 10 shares
  decide(T, buy(A, qty)) :- universe(A, T), flat(A, T).
  decide(T, sell(A, Q)) :- held(A, T, Q).
}
"#;
    let p = program(round_trip, "round_trip");
    let mut ds = Dataset::new();
    let x = ds.intern("X");
    for (d, price) in [("2024-01-08", Some(10.0)), ("2024-01-09", Some(12.0)), ("2024-01-10", None), ("2024-01-11", None), ("2024-01-12", None)] {
        let t = absolute_backtest::kernel::time::parse_timestamp(d).unwrap();
        if let Some(p) = price {
            ds.add("close", vec![Value::Equity(x), Value::Time(t), Value::Num(p)]);
        }
        ds.add("volume", vec![Value::Equity(x), Value::Time(t), Value::Num(1000.0)]);
        ds.add("universe", vec![Value::Equity(x), Value::Time(t)]);
    }
    ds.derive_tickers();
    let cfg = ExecConfig {
        initial_cash: 1000.0,
        ..ExecConfig::frictionless()
    };
    assert_same("round_trip", &run(&p, &ds, cfg.clone()).unwrap(), &run_fold(&p, &ds, cfg).unwrap());
    // The catalog market under every default, with a margin preset too.
    let p = program(
        &fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/corpus/strategies/total_return_momentum.dsl")).unwrap(),
        "total_return_momentum",
    );
    let ds = synthetic_daily_v2(&SYMS, (2022, 1, 3), 400, 5);
    for cfg in [
        ExecConfig::default(),
        ExecConfig::reg_t(),
        ExecConfig {
            participation_cap: 0.001,
            on_leverage: absolute_backtest::kernel::OnLeverage::Reject,
            ..ExecConfig::default()
        },
    ] {
        assert_same("total_return_momentum", &run(&p, &ds, cfg.clone()).unwrap(), &run_fold(&p, &ds, cfg).unwrap());
    }
}

#[test]
fn a_fold_stopped_early_is_the_batch_run_on_the_truncated_data() {
    let src = fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/corpus/strategies/momentum_top_n.dsl")).unwrap();
    let p = program(&src, "momentum_top_n");
    let ds = synthetic_daily(&SYMS, (2022, 1, 3), 200, 7);
    let kernel = Kernel::new_streaming(&p, &ds, ExecConfig::default()).unwrap();
    let log = EventLog::from_dataset(&kernel, &ds).unwrap();
    let bars: Vec<i64> = Kernel::new(&p, &ds, ExecConfig::default()).unwrap().decision_bars();
    for &cut in &[bars[60], bars[133], bars[199]] {
        let kernel = Kernel::new_streaming(&p, &ds, ExecConfig::default()).unwrap();
        let mut exec = SimExecutor::new(ExecConfig::default());
        let mut fold = Fold::new(kernel, &mut exec);
        for ev in &log.events {
            if let Event::Tuple { avail, .. } = ev {
                if *avail > cut {
                    break;
                }
            }
            fold.step(ev.clone()).unwrap();
        }
        let (early, _) = fold.finish().unwrap();
        let truncated = run(&p, &ds.truncated(&p, cut), ExecConfig::default()).unwrap();
        assert_same(&format!("cut at {}", cut), &truncated, &early);
    }
}

#[test]
fn the_event_log_is_ordered_by_availability_then_relation() {
    let p = program(
        &fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/corpus/strategies/sma_crossover.dsl")).unwrap(),
        "sma_crossover",
    );
    let ds = synthetic_daily(&SYMS, (2022, 1, 3), 20, 1);
    let kernel = Kernel::new_streaming(&p, &ds, ExecConfig::default()).unwrap();
    let log = EventLog::from_dataset(&kernel, &ds).unwrap();
    let keys: Vec<(i64, usize)> = log
        .events
        .iter()
        .map(|e| match e {
            Event::Tuple { avail, rel, .. } => (*avail, *rel),
            _ => unreachable!(),
        })
        .collect();
    let mut sorted = keys.clone();
    sorted.sort();
    assert_eq!(keys, sorted);
    assert_eq!(log.events.len(), 20 * 5 * 4, "close, volume, universe and ticker for five names over twenty bars");
    assert!(kernel.decision_bars().is_empty(), "a streaming kernel starts with no bars");
}
