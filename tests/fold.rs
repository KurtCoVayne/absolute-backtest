//! The fold kernel (data-bundle doc, section 2) agrees with the batch kernel
//! to the bit, and a fold stopped early is the batch run on the truncated
//! dataset: the causality theorem, operationally.

mod common;

use absolute_backtest::check::Program;
use absolute_backtest::data::{synthetic_daily, synthetic_minute};
use absolute_backtest::kernel::{run, run_fold, Dataset, Event, EventLog, ExecConfig, Fold, Kernel, RunResult, SimExecutor, Value};
use absolute_backtest::Kind;

fn corpus_strategies() -> Vec<(String, Program)> {
    common::dsl_files("strategies")
        .into_iter()
        .map(|f| {
            let name = f.file_stem().unwrap().to_string_lossy().to_string();
            (name.clone(), common::corpus_program(&name))
        })
        .collect()
}

/// The names of the synthetic markets, in the order the acceptance test
/// uses: the daily market renames its second name halfway and delists its
/// last, so the tickers the strategies cite (`AAA`, `BBB`, `SPY`) stay.
const SYMS: [&str; 5] = ["AAA", "XYZ", "BBB", "SPY", "DDD"];

/// The synthetic market of a program's base: minute bars for a sub-daily
/// base, else the daily market with its catalog. Four hundred daily bars,
/// so that every strategy, the twelve-month momentum included, decides.
fn dataset_for(p: &Program, seed: u64) -> Dataset {
    if p.base.is_sub_daily() {
        synthetic_minute(&SYMS, (2024, 1, 2), 30, 320, seed)
    } else {
        synthetic_daily(&SYMS, (2022, 1, 3), 400, seed)
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

/// The fold against the batch kernel on the corpus strategies whose base
/// matches `sub_daily`, every one of which must decide something.
fn fold_equals_batch(sub_daily: bool) {
    let mut seen = 0;
    for (name, p) in corpus_strategies() {
        if p.base.is_sub_daily() != sub_daily {
            continue;
        }
        seen += 1;
        let ds = dataset_for(&p, 11);
        let cfg = ExecConfig::from_program(&p);
        let a = run(&p, &ds, cfg.clone()).unwrap_or_else(|e| panic!("{}: {}", name, e));
        let b = run_fold(&p, &ds, cfg).unwrap_or_else(|e| panic!("{} (fold): {}", name, e));
        assert_same(&name, &a, &b);
        assert!(!b.decisions.is_empty(), "{} decided nothing", name);
    }
    assert!(seen > 0);
}

#[test]
fn the_fold_equals_the_batch_kernel_on_every_daily_corpus_strategy() {
    fold_equals_batch(false);
}

/// The strategies on a minute base (`opening_gap`, `resampled_momentum`),
/// whose daily catalog is keyed at the date.
#[test]
fn the_fold_equals_the_batch_kernel_on_every_minute_corpus_strategy() {
    fold_equals_batch(true);
}

#[test]
fn the_fold_equals_the_batch_kernel_on_crafted_markets() {
    // Missing prices, a liquidation at the last price, a delisting, a split
    // and a dividend: the crafted cases of the executor tests. The name is
    // in the universe while listed, so that a bar with no price is still a
    // bar at which its position is decided on.
    let round_trip = common::StrategyParts {
        name: "round_trip".into(),
        universe: "universe(A, T) :- listed(A, T).".into(),
        params: "    qty : Quantity<Shares> = 10 shares".into(),
        decides: "    decide(T, trade(A, qty)) :- universe(A, T), flat(A, T).\n    decide(T, trade(A, Q)) :- held(A, T, Q0), Q = -Q0.".into(),
        ..Default::default()
    }
    .render();
    let p = common::program(&round_trip, "round_trip");
    let mut ds = Dataset::new();
    let x = ds.add_security("X", "X", common::day("2024-01-08"), None);
    for (d, price) in [("2024-01-08", Some(10.0)), ("2024-01-09", Some(12.0)), ("2024-01-10", None), ("2024-01-11", None), ("2024-01-12", None)] {
        let t = common::day(d);
        if let Some(p) = price {
            for rel in ["open", "high", "low", "close"] {
                ds.add(rel, vec![Value::Equity(x), Value::Time(t), Value::Num(p)]);
            }
        }
        ds.add("volume", vec![Value::Equity(x), Value::Time(t), Value::Num(1000.0)]);
        ds.add("listed", vec![Value::Equity(x), Value::Time(t)]);
    }
    ds.derive_tickers();
    let cfg = ExecConfig {
        initial_cash: 1000.0,
        ..ExecConfig::frictionless()
    };
    let batch = run(&p, &ds, cfg.clone()).unwrap();
    assert_same("round_trip", &batch, &run_fold(&p, &ds, cfg).unwrap());
    assert!(!batch.decisions.is_empty());
    // The catalog market under the execution block, with a margin preset
    // and a tight participation cap too.
    let p = common::corpus_program("total_return_momentum");
    let ds = synthetic_daily(&SYMS, (2022, 1, 3), 400, 5);
    let block = ExecConfig::from_program(&p);
    for cfg in [
        block.clone(),
        ExecConfig::reg_t(),
        ExecConfig {
            participation_cap: 0.001,
            on_leverage: absolute_backtest::kernel::OnLeverage::Reject,
            ..block
        },
    ] {
        assert_same("total_return_momentum", &run(&p, &ds, cfg.clone()).unwrap(), &run_fold(&p, &ds, cfg).unwrap());
    }
}

#[test]
fn a_fold_stopped_early_is_the_batch_run_on_the_truncated_data() {
    let p = common::corpus_program("momentum_top_n");
    let ds = synthetic_daily(&SYMS, (2022, 1, 3), 200, 7);
    let cfg = ExecConfig::from_program(&p);
    let kernel = Kernel::new_streaming(&p, &ds, cfg.clone()).unwrap();
    let log = EventLog::from_dataset(&kernel, &ds).unwrap();
    let bars: Vec<i64> = Kernel::new(&p, &ds, cfg.clone()).unwrap().decision_bars();
    for &cut in &[bars[60], bars[133], bars[199]] {
        let kernel = Kernel::new_streaming(&p, &ds, cfg.clone()).unwrap();
        let mut exec = SimExecutor::new(cfg.clone());
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
        let truncated = run(&p, &ds.truncated(&p, cut), cfg.clone()).unwrap();
        assert_same(&format!("cut at {}", cut), &truncated, &early);
    }
}

#[test]
fn the_event_log_is_ordered_by_availability_then_relation() {
    let p = common::corpus_program("sma_crossover");
    let ds = synthetic_daily(&SYMS, (2022, 1, 3), 20, 1);
    let cfg = ExecConfig::from_program(&p);
    let kernel = Kernel::new_streaming(&p, &ds, cfg).unwrap();
    let log = EventLog::from_dataset(&kernel, &ds).unwrap();
    let keys: Vec<(i64, usize)> = log
        .events
        .iter()
        .map(|e| match e {
            Event::Tuple { avail, rel, .. } => (*avail, *rel),
            Event::Barrier { .. } => unreachable!("a dataset's log holds tuples only"),
        })
        .collect();
    let mut sorted = keys.clone();
    sorted.sort();
    assert_eq!(keys, sorted);
    // Every tuple of a primitive the program reads (the bars, the listing
    // and the tickers of five names over twenty bars), once; the catalog
    // the demand does not ask for is not in the log.
    let expected: usize = ds
        .facts
        .iter()
        .filter(|(name, _)| matches!(p.relations.get(name.as_str()).map(|s| &s.kind), Some(Kind::Primitive { .. })))
        .map(|(_, tuples)| tuples.len())
        .sum();
    assert!(ds.facts.contains_key("split") && !p.relations.contains_key("split"), "the market has actions the demand does not read");
    assert_eq!(log.events.len(), expected);
    assert!(expected >= 7 * 5 * 16, "open, high, low, close, volume, listed and ticker: {}", expected);
    assert!(kernel.decision_bars().is_empty(), "a streaming kernel starts with no bars");
}
