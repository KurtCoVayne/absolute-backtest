//! Incremental windows (data-bundle doc, section 2 "State"): a windowed
//! aggregation keeps, per group, the rows of every bar it has solved, so a
//! rolling feature solves each bar once and only evicts; the result is the
//! uncached evaluation to the bit, on both drivers.

mod corpus;

use std::fs;
use std::path::Path;

use absolute_backtest::check::{check_program, Program};
use absolute_backtest::data::{synthetic_daily, synthetic_daily_v2, synthetic_minute};
use absolute_backtest::kernel::{run, run_fold, Dataset, ExecConfig, RunResult};

fn program(src: &str, name: &str) -> Program {
    let mut ws = corpus::base_workspace();
    ws.add_source(src).unwrap();
    let (p, diags) = check_program(&ws, name);
    let text: Vec<String> = diags.iter().map(|d| d.to_string()).collect();
    p.unwrap_or_else(|| panic!("{} should check:\n{}", name, text.join("\n")))
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
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("corpus/strategies");
    let mut files: Vec<_> = fs::read_dir(dir).unwrap().map(|e| e.unwrap().path()).collect();
    files.sort();
    for f in files {
        let name = f.file_stem().unwrap().to_string_lossy().to_string();
        let p = program(&fs::read_to_string(&f).unwrap(), &name);
        let ds = dataset_for(&p, 11);
        let cached = ExecConfig::default();
        let uncached = ExecConfig {
            window_cache: false,
            ..ExecConfig::default()
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
    // sma over 60 days with min 40, for five names over 320 bars: the cache
    // solves about one bar per name per bar; without it, every call solves
    // the whole window again.
    let src = r#"
strategy rolling {
  env equities_1d
  uses features
  resolution @1d
  mode target
  param n : Duration = 60d
  param k : Count = 40
  param w : Scalar = 0.1
  rel above(-A: Equity, @T: Timestamp)
  above(A, T) :- universe(A, T), sma(A, T, n, k, M), close(A, T, P), P > M.
  rel below(-A: Equity, @T: Timestamp)
  below(A, T) :- universe(A, T), sma(A, T, n, k, M), close(A, T, P), P <= M.
  decide(T, target_weight(A, w)) :- above(A, T).
  decide(T, target_weight(A, 0)) :- held(A, T, _), below(A, T).
}
"#;
    let p = program(src, "rolling");
    let ds = synthetic_daily(&SYMS, (2022, 1, 3), 320, 3);
    let cached = run(&p, &ds, ExecConfig::default()).unwrap();
    let uncached = run(
        &p,
        &ds,
        ExecConfig {
            window_cache: false,
            ..ExecConfig::default()
        },
    )
    .unwrap();
    assert_eq!(digest(&cached), digest(&uncached));
    let bars = cached.bars.len();
    assert_eq!(bars, 320);
    // Every (name, bar) once: 5 * 320, plus nothing for the bars before a name's first call.
    assert!(cached.stats.window_bars_solved <= 5 * bars, "{:?}", cached.stats);
    assert!(cached.stats.window_bars_solved >= 5 * (bars - 1), "{:?}", cached.stats);
    // Without the cache about forty bars per call.
    assert!(uncached.stats.window_bars_solved > 20 * cached.stats.window_bars_solved, "{:?} vs {:?}", uncached.stats, cached.stats);
    assert_eq!(cached.stats.window_calls, uncached.stats.window_calls);
    // The cache keeps only the window: no more than the window's bars per group.
    assert!(cached.stats.window_rows_cached <= 5 * 45, "{:?}", cached.stats);
}

#[test]
fn a_group_is_keyed_by_the_outer_bindings_the_conjunction_reads() {
    // Two sma lengths on one name are two groups; a feature read at prev(T)
    // reuses the group's rows.
    let src = r#"
strategy two_lengths {
  env equities_1d
  uses features
  resolution @1d
  mode delta
  param fast : Duration = 20d
  param slow : Duration = 50d
  param kf : Count = 12
  param ks : Count = 30
  param qty : Quantity<Shares> = 10 shares
  rel above(-A: Equity, @T: Timestamp)
  above(A, T) :- universe(A, T), sma(A, T, fast, kf, F), sma(A, T, slow, ks, S), F > S.
  rel below(-A: Equity, @T: Timestamp)
  below(A, T) :- universe(A, T), sma(A, T, fast, kf, F), sma(A, T, slow, ks, S), F <= S.
  decide(T, buy(A, qty)) :- above(A, T), prev(T, T0), below(A, T0), flat(A, T).
  decide(T, sell(A, Q)) :- held(A, T, Q), below(A, T).
}
"#;
    let p = program(src, "two_lengths");
    let ds = synthetic_daily(&SYMS, (2022, 1, 3), 200, 9);
    let cached = run(&p, &ds, ExecConfig::default()).unwrap();
    let uncached = run(
        &p,
        &ds,
        ExecConfig {
            window_cache: false,
            ..ExecConfig::default()
        },
    )
    .unwrap();
    assert_eq!(digest(&cached), digest(&uncached));
    assert!(!cached.decisions.is_empty());
    // Two groups per name: fast and slow; each bar solved once per group.
    assert!(cached.stats.window_bars_solved <= 2 * 5 * 200, "{:?}", cached.stats);
    assert!(cached.stats.window_groups == 10, "{:?}", cached.stats);
}
