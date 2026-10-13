//! Incremental windows (data-bundle doc, section 2 "State"): a windowed
//! aggregation keeps, per group, the rows of every bar it has solved, so a
//! rolling feature solves each bar once and only evicts; the result is the
//! uncached evaluation to the bit, on both drivers.

mod common;

use absolute_backtest::check::Program;
use absolute_backtest::data::{synthetic_daily, synthetic_minute};
use absolute_backtest::kernel::{run, run_fold, Dataset, ExecConfig, RunResult};

const SYMS: [&str; 5] = ["AAA", "BBB", "CCC", "DDD", "SPY"];

/// The synthetic market of a program's base: minute bars for a sub-daily
/// base, else the daily market with its catalog, over the four hundred
/// bars the acceptance test runs every strategy on (on a shorter market
/// the long-short book's legs drift past the execution block's gross
/// limit and the run halts).
fn dataset_for(p: &Program, seed: u64) -> Dataset {
    if p.base.is_sub_daily() {
        synthetic_minute(&SYMS, (2024, 1, 2), 30, 320, seed)
    } else {
        synthetic_daily(&SYMS, (2022, 1, 3), 400, seed)
    }
}

fn digest(r: &RunResult) -> Vec<String> {
    let mut out = Vec::new();
    for d in &r.decisions {
        out.push(format!("decision {} {} {}", d.t, r.describe_decision(&d.decision), d.rule));
    }
    for f in &r.fills {
        out.push(format!("fill {} {} {:x} {:x}", f.t, r.symbols[f.equity as usize], f.quantity.to_bits(), f.price.to_bits()));
    }
    for (t, e) in &r.equity_curve {
        out.push(format!("equity {} {:x}", t, e.to_bits()));
    }
    out.push(format!("final_cash {:x}", r.final_cash.to_bits()));
    out
}

#[test]
fn the_window_cache_is_exact_on_every_corpus_strategy_on_both_drivers() {
    for f in common::dsl_files("strategies") {
        let name = f.file_stem().unwrap().to_string_lossy().to_string();
        let p = common::corpus_program(&name);
        let ds = dataset_for(&p, 11);
        // Exact window sums on both sides (the execution block's `numerics
        // running` is off): the cache (and its sorted windows) must not
        // move a bit.
        let cached = ExecConfig {
            running_sums: false,
            ..ExecConfig::from_program(&p)
        };
        let uncached = ExecConfig {
            window_cache: false,
            ..cached.clone()
        };
        let a = run(&p, &ds, uncached.clone()).unwrap_or_else(|e| panic!("{}: {}", name, e));
        let b = run(&p, &ds, cached.clone()).unwrap_or_else(|e| panic!("{} (cached): {}", name, e));
        let c = run_fold(&p, &ds, cached).unwrap_or_else(|e| panic!("{} (fold): {}", name, e));
        assert_eq!(digest(&a), digest(&b), "{}: the cache changed the batch run", name);
        assert_eq!(digest(&a), digest(&c), "{}: the cache changed the fold", name);
        assert!(b.stats.window_bars_solved <= a.stats.window_bars_solved, "{}: {:?} vs {:?}", name, b.stats, a.stats);
    }
}

#[test]
fn a_rolling_feature_solves_each_bar_once() {
    // A mean over 60 days with min 40, for five names over 320 bars: the
    // cache solves about one bar per name per bar; without it, every call
    // solves the whole window again.
    let src = common::StrategyParts {
        name: "rolling".into(),
        uses: "calendar, stat, book".into(),
        params: "    n : Duration = 60d\n    k : Count = 40\n    w : Scalar = 0.1".into(),
        rules: r#"
    rel above(A: Equity, @T)
    above(A, T) :- universe(A, T), mean_window[n, k, close](A, T, M), close(A, T, P), P > M.
    rel below(A: Equity, @T)
    below(A, T) :- universe(A, T), mean_window[n, k, close](A, T, M), close(A, T, P), P <= M.
"#
        .into(),
        decides: "    decide(T, hold(A, w)) :- above(A, T).\n    decide(T, hold(A, 0)) :- held(A, T, _), below(A, T).".into(),
        ..Default::default()
    }
    .render();
    let p = common::program(&src, "rolling");
    let ds = synthetic_daily(&SYMS, (2022, 1, 3), 320, 3);
    let cfg = ExecConfig::from_program(&p);
    let cached = run(&p, &ds, cfg.clone()).unwrap();
    let uncached = run(
        &p,
        &ds,
        ExecConfig {
            window_cache: false,
            ..cfg
        },
    )
    .unwrap();
    assert_eq!(digest(&cached), digest(&uncached));
    assert_eq!(cached.bars.len(), 320);
    // Every (name, bar) of the universe once (the last name is delisted at
    // four fifths of the market, so its rows stop early), plus nothing for
    // the bars before a name's first call; one group per name.
    let rows = ds.facts["close"].len();
    assert!(cached.stats.window_bars_solved <= rows, "{:?} over {} rows", cached.stats, rows);
    assert!(cached.stats.window_bars_solved >= rows - 5, "{:?} over {} rows", cached.stats, rows);
    assert_eq!(cached.stats.window_groups, 5, "{:?}", cached.stats);
    // Without the cache about forty bars per call.
    assert!(uncached.stats.window_bars_solved > 20 * cached.stats.window_bars_solved, "{:?} vs {:?}", uncached.stats, cached.stats);
    assert_eq!(cached.stats.window_calls, uncached.stats.window_calls);
    // The cache keeps only the window: no more than the window's bars per group.
    assert!(cached.stats.window_rows_cached <= 5 * 45, "{:?}", cached.stats);
}

#[test]
fn a_group_is_keyed_by_the_outer_bindings_the_conjunction_reads() {
    // Two mean lengths on one name are two groups; a feature read at
    // prev(T) reuses the group's rows.
    let src = common::StrategyParts {
        name: "two_lengths".into(),
        uses: "calendar, stat, book".into(),
        params: "    fast : Duration = 20d\n    slow : Duration = 50d\n    kf : Count = 12\n    ks : Count = 30\n    qty : Quantity<Shares> = 10 shares".into(),
        rules: r#"
    rel above(A: Equity, @T)
    above(A, T) :- universe(A, T), mean_window[fast, kf, close](A, T, F), mean_window[slow, ks, close](A, T, S), F > S.
    rel below(A: Equity, @T)
    below(A, T) :- universe(A, T), mean_window[fast, kf, close](A, T, F), mean_window[slow, ks, close](A, T, S), F <= S.
"#
        .into(),
        decides: "    decide(T, trade(A, qty)) :- above(A, T), prev(T, T0), below(A, T0), flat(A, T).\n    decide(T, trade(A, Q)) :- held(A, T, Q0), below(A, T), Q = -Q0.".into(),
        ..Default::default()
    }
    .render();
    let p = common::program(&src, "two_lengths");
    let ds = synthetic_daily(&SYMS, (2022, 1, 3), 200, 9);
    let cfg = ExecConfig::from_program(&p);
    let cached = run(&p, &ds, cfg.clone()).unwrap();
    let uncached = run(
        &p,
        &ds,
        ExecConfig {
            window_cache: false,
            ..cfg
        },
    )
    .unwrap();
    assert_eq!(digest(&cached), digest(&uncached));
    assert!(!cached.decisions.is_empty());
    // Two groups per name: fast and slow; each bar solved once per group.
    assert!(cached.stats.window_bars_solved <= 2 * 5 * 200, "{:?}", cached.stats);
    assert!(cached.stats.window_groups == 10, "{:?}", cached.stats);
}
