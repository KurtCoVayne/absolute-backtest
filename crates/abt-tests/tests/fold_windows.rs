//! Incremental windows (data-bundle doc, section 2 "State"): a windowed
//! aggregation keeps, per group, the rows of every bar it has solved, so a
//! rolling feature solves each bar once and only evicts; the result is the
//! uncached evaluation to the bit, on both drivers. The window state is the
//! kernel's, so these runs evaluate every aggregate here, without pushdown
//! (K-4.5, which `tests/pushdown.rs` compares to this evaluation).

use abt_tests as common;

use abt_kernel::check::Program;
use abt_kernel::data::{business_days, synthetic_daily, synthetic_minute};
use abt_kernel::kernel::time::DAY;
use abt_kernel::kernel::{Dataset, ExecConfig, RunResult, Value};
use abt_tests::{run, run_socket};

/// The names of the synthetic markets, in the order the acceptance test
/// uses: the daily market renames its second name halfway and delists its
/// last, so the tickers the strategies cite (`AAA`, `BBB`, `SPY`) stay.
const SYMS: [&str; 5] = ["AAA", "XYZ", "BBB", "SPY", "DDD"];

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

/// The strategies with a `rows` group asked less often than its window is
/// long, and the share of the uncached bars the cache may solve beyond them.
/// Such a group carries every bar since its last call, so that its running
/// sums stay exact, where the uncached evaluation solves the window's last N
/// rows at each call: short_term_reversal asks `adv[20]` (behind
/// `liquid[min_adv]`) only at month starts, about one bar more a month than
/// its twenty, and its first walk back keeps the group's slack rows.
const SPARSE_ROWS: [(&str, f64); 1] = [("short_term_reversal", 0.001)];

/// The cache against the uncached evaluation on the corpus strategies
/// whose base matches `sub_daily`, on the batch driver and the fold: the
/// runs agree to the bit and, with `only_evicts`, the cache solves no more
/// bars than the uncached evaluation (within the bound of a strategy of
/// `SPARSE_ROWS`).
fn cache_is_exact(sub_daily: bool, only_evicts: bool) {
    let mut seen = 0;
    let mut more: Vec<String> = Vec::new();
    for f in common::dsl_files("strategies") {
        let name = f.file_stem().unwrap().to_string_lossy().to_string();
        let p = common::corpus_program(&name);
        if p.base.is_sub_daily() != sub_daily {
            continue;
        }
        seen += 1;
        let ds = dataset_for(&p, 11);
        // Exact window sums on both sides (the execution block's `numerics
        // running` is off): the cache (and its sorted windows) must not
        // move a bit.
        let cached = ExecConfig {
            running_sums: false,
            pushdown: false,
            ..ExecConfig::from_program(&p)
        };
        let uncached = ExecConfig {
            window_cache: false,
            ..cached.clone()
        };
        let a = run(&p, &ds, uncached.clone()).unwrap_or_else(|e| panic!("{}: {}", name, e));
        let b = run(&p, &ds, cached.clone()).unwrap_or_else(|e| panic!("{} (cached): {}", name, e));
        let c = run_socket(&p, &ds, cached).unwrap_or_else(|e| panic!("{} (fold): {}", name, e));
        assert_eq!(digest(&a), digest(&b), "{}: the cache changed the batch run", name);
        assert_eq!(digest(&a), digest(&c), "{}: the cache changed the fold", name);
        let allowed = SPARSE_ROWS
            .iter()
            .find(|(n, _)| *n == name)
            .map(|(_, share)| (a.stats.window_bars_solved as f64 * share) as usize)
            .unwrap_or(0);
        if b.stats.window_bars_solved > a.stats.window_bars_solved + allowed {
            more.push(format!("{}: cached {} > uncached {} ({:?})", name, b.stats.window_bars_solved, a.stats.window_bars_solved, b.stats));
        }
    }
    assert!(seen > 0);
    if only_evicts {
        assert!(more.is_empty(), "the cache solved more bars than the uncached evaluation:\n{}", more.join("\n"));
    }
}

#[test]
fn the_window_cache_is_exact_on_every_daily_corpus_strategy_on_both_drivers() {
    cache_is_exact(false, false);
}

/// The cache solves each bar once and only evicts: never more bars than
/// the uncached evaluation, but for a group asked sparsely (`SPARSE_ROWS`).
#[test]
fn the_window_cache_only_evicts_on_every_daily_corpus_strategy() {
    cache_is_exact(false, true);
}

/// The strategies on a minute base (`opening_gap`, `resampled_momentum`),
/// whose daily catalog is keyed at the date.
#[test]
fn the_window_cache_is_exact_on_every_minute_corpus_strategy_on_both_drivers() {
    cache_is_exact(true, true);
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
    let cfg = ExecConfig {
        pushdown: false,
        ..ExecConfig::from_program(&p)
    };
    let cached = run(&p, &ds, cfg.clone()).unwrap();
    let uncached = run(&p, &ds, ExecConfig { window_cache: false, ..cfg }).unwrap();
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
    let cfg = ExecConfig {
        pushdown: false,
        ..ExecConfig::from_program(&p)
    };
    let cached = run(&p, &ds, cfg.clone()).unwrap();
    let uncached = run(&p, &ds, ExecConfig { window_cache: false, ..cfg }).unwrap();
    assert_eq!(digest(&cached), digest(&uncached));
    assert!(!cached.decisions.is_empty());
    // Two groups per name: fast and slow; each bar solved once per group.
    assert!(cached.stats.window_bars_solved <= 2 * 5 * 200, "{:?}", cached.stats);
    assert!(cached.stats.window_groups == 10, "{:?}", cached.stats);
}

#[test]
fn a_test_of_the_base_time_excludes_cached_rows_as_the_whole_solve_does() {
    // `Ex = T` (the dividends going ex at the bar, as the catalog's
    // `dividend_at`) reads the window's base time: the rest of the
    // conjunction is solved a bar at a time and cached, and the test is
    // applied to the cached rows at each call. A row the test excludes can
    // be stuck in the cached part (a division by zero); the whole solve
    // excludes it (Kleene conjunction), and so does the cache.
    let src = common::StrategyParts {
        name: "ex_dates".into(),
        needs: "need bars @1d   need actions, listing".into(),
        params: "    unit : Quantity<Shares> = 1 shares\n    half : Price<USD> = 0.5 USD/share".into(),
        rules:
            "    rel paid(A: Equity, @T, N: Scalar)\n    paid(A, T, N) :- universe(A, T), N = sum(R) over (T0 in window[30d] min 1, dividend(A, T0, Ex, _, X), Ex = T, R = (X - half) / (X - half))."
                .into(),
        decides: "    decide(T, trade(A, Q)) :- paid(A, T, N), Q = N * unit.".into(),
        ..Default::default()
    }
    .render();
    let p = common::program(&src, "ex_dates");
    let days = business_days((2024, 1, 8), 5);
    let mut ds = Dataset::new();
    let x = ds.add_security("X", "X", days[0], None);
    for t in &days {
        common::add_bar(&mut ds, x, *t, 10.0, 1000.0);
    }
    ds.derive_tickers();
    // Announced on the first day, 0.25 going ex on the third; on the
    // second, 0.50 (the row stuck) going ex on the Saturday after the last,
    // which is no bar.
    let saturday = days[4] + DAY;
    ds.add("dividend", vec![Value::Equity(x), Value::Time(days[0]), Value::Time(days[2]), Value::Time(days[4]), Value::Num(0.25)]);
    ds.add("dividend", vec![Value::Equity(x), Value::Time(days[1]), Value::Time(saturday), Value::Time(saturday), Value::Num(0.5)]);
    let cfg = ExecConfig {
        pushdown: false,
        ..ExecConfig::from_program(&p)
    };
    let cached = run(&p, &ds, cfg.clone()).unwrap();
    let uncached = run(&p, &ds, ExecConfig { window_cache: false, ..cfg }).unwrap();
    assert_eq!(digest(&cached), digest(&uncached));
    assert_eq!(cached.decisions.iter().map(|d| d.t).collect::<Vec<_>>(), vec![days[2]]);
}
