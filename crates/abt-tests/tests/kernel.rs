//! Kernel tests: hand-computed executor outcomes, every corpus strategy run
//! end to end, determinism, the causality theorem, and the runtime
//! diagnostics of section 7.

use abt_tests as common;

use std::collections::BTreeMap;

use abt_kernel::check::Program;
use abt_kernel::data::{business_days, synthetic_daily, synthetic_for_program, synthetic_minute};
use abt_kernel::kernel::time::format_timestamp;
use abt_kernel::kernel::{Ctor, Dataset, ExecConfig, OnLeverage, RunError, RunResult, Sym, Value};
use abt_kernel::{Duration, Lit};
use abt_tests::{run, verify_causality};

use common::day;

/// Buy ten shares of a name whose return is positive when flat; sell the
/// bar after a buy.
fn up_down() -> Program {
    common::program(&up_down_source(common::StrategyParts::default().universe), "up_down")
}

fn up_down_source(universe: String) -> String {
    common::StrategyParts {
        name: "up_down".into(),
        universe,
        params: "    qty : Quantity<Shares> = 10 shares".into(),
        rules: "    rel up(A: Equity, @T)
    up(A, T) :- universe(A, T), price_return(A, T, R), R > 0."
            .into(),
        decides: "    decide(T, trade(A, qty)) :- up(A, T), flat(A, T).
    decide(T, trade(A, Q)) :- held(A, T, Q0), prev(T, T0), decided(T0, trade(A, Q1)), Q1 > 0 shares, Q = -Q0."
            .into(),
        ..Default::default()
    }
    .render()
}

/// One symbol `X`, consecutive weekdays from 2024-01-08, crafted closes.
fn crafted_daily(prices: &[f64]) -> Dataset {
    common::market("X", prices)
}

/// A frictionless book of `initial_cash`.
fn hand(initial_cash: f64) -> ExecConfig {
    ExecConfig {
        initial_cash,
        ..ExecConfig::frictionless()
    }
}

/// The decisions of a run as `date decision`.
fn describe(r: &RunResult) -> Vec<String> {
    r.decisions.iter().map(|d| format!("{} {}", format_timestamp(d.t), r.describe_decision(&d.decision))).collect()
}

#[test]
fn executor_contract_by_hand() {
    let prog = up_down();
    let ds = crafted_daily(&[10.0, 11.0, 10.0, 12.0, 13.0]);
    let r = run(&prog, &ds, hand(1000.0)).unwrap();
    assert_eq!(
        describe(&r),
        vec!["2024-01-09 trade(X, 10)", "2024-01-10 trade(X, -10)", "2024-01-11 trade(X, 10)", "2024-01-12 trade(X, -10)"]
    );
    let fills: Vec<(String, f64, f64)> = r.fills.iter().map(|f| (format_timestamp(f.t), f.quantity, f.price)).collect();
    assert_eq!(
        fills,
        vec![("2024-01-10".to_string(), 10.0, 10.0), ("2024-01-11".to_string(), -10.0, 12.0), ("2024-01-12".to_string(), 10.0, 13.0)]
    );
    let curve: Vec<f64> = r.equity_curve.iter().map(|(_, e)| *e).collect();
    assert_eq!(curve, vec![1000.0, 1000.0, 1000.0, 1020.0, 1020.0]);
    assert_eq!(r.final_cash, 890.0);
    assert_eq!(r.final_positions.values().copied().collect::<Vec<_>>(), vec![10.0]);
}

#[test]
fn slippage_and_commission_apply_against_the_order() {
    let prog = up_down();
    let ds = crafted_daily(&[10.0, 11.0, 10.0, 12.0, 13.0]);
    let r = run(
        &prog,
        &ds,
        ExecConfig {
            initial_cash: 1000.0,
            slippage_bps: 100.0,
            commission_per_share: 0.5,
            ..ExecConfig::frictionless()
        },
    )
    .unwrap();
    // Buy 10 at 10 * 1.01 = 10.1 plus 5 commission: 1000 - 106 = 894.
    assert!((r.fills[0].price - 10.1).abs() < 1e-9);
    // Sell 10 at 12 * 0.99 = 11.88 minus 5: 894 + 113.8 = 1007.8.
    assert!((r.fills[1].price - 11.88).abs() < 1e-9);
    // Buy 10 at 13.13 plus 5: 1007.8 - 136.3 = 871.5.
    assert!((r.final_cash - 871.5).abs() < 1e-9);
}

#[test]
fn target_weight_sizes_from_equity_at_execution() {
    let src = common::strategy("half", "    w : Scalar = 0.5", "", "    decide(T, hold(A, w)) :- universe(A, T).");
    let prog = common::program(&src, "half");
    let ds = crafted_daily(&[10.0, 10.0, 20.0, 20.0, 20.0]);
    let r = run(
        &prog,
        &ds,
        ExecConfig {
            initial_cash: 1000.0,
            compounding: true,
            ..ExecConfig::frictionless()
        },
    )
    .unwrap();
    // Day 2 fill: equity 1000 at price 10 -> 50 shares. Day 3: price 20, equity
    // 500 + 1000 = 1500 -> target 37 shares (truncated), order -13. Then 37
    // shares at 20 = 740 = 0.5 * 1480: no further order (1480 * 0.5 / 20 = 37).
    let qty: Vec<f64> = r.fills.iter().map(|f| f.quantity).collect();
    assert_eq!(qty, vec![50.0, -13.0]);
    assert_eq!(r.final_positions.values().copied().collect::<Vec<_>>(), vec![37.0]);
    // On a fixed base (the default) the weight is of the capital: 50 shares
    // at 20 are 1000 = 0.5 * 2000, too much; the target is 25 shares.
    let r = run(&prog, &ds, hand(1000.0)).unwrap();
    let qty: Vec<f64> = r.fills.iter().map(|f| f.quantity).collect();
    assert_eq!(qty, vec![50.0, -25.0]);
    assert_eq!(r.base_capital, Some(1000.0));
    // The bar returns (days 2 to 5) are the NAV changes over the capital:
    // the 50 shares gain 500 on day 3, +500 / 1000; nothing moves after.
    let rets: Vec<f64> = r.bar_returns().iter().map(|(_, x)| *x).collect();
    assert_eq!(rets, vec![0.0, 0.5, 0.0, 0.0]);
}

#[test]
fn conflicting_decisions_halt_naming_both_rules() {
    let src = common::strategy(
        "conflict",
        "    a : Quantity<Shares> = 10 shares
    b : Quantity<Shares> = 20 shares",
        "",
        "    decide(T, trade(A, a)) :- universe(A, T).
    decide(T, trade(A, b)) :- universe(A, T).",
    );
    let prog = common::program(&src, "conflict");
    let ds = crafted_daily(&[10.0, 11.0, 10.0, 12.0, 13.0]);
    match run(&prog, &ds, ExecConfig::default()) {
        Err(RunError::Conflict { equity, first, second, .. }) => {
            assert_eq!(equity, "X");
            assert_eq!(first.1, "conflict::decide#1");
            assert_eq!(second.1, "conflict::decide#2");
        }
        other => panic!("expected a conflict, got {:?}", other.map(|r| r.decisions.len())),
    }
}

#[test]
fn identical_decisions_from_two_rules_are_one_tuple() {
    let src = common::strategy(
        "same",
        "    a : Quantity<Shares> = 10 shares",
        "",
        "    decide(T, trade(A, a)) :- universe(A, T), flat(A, T).
    decide(T, trade(A, a)) :- universe(A, T), close(A, T, _), flat(A, T).",
    );
    let prog = common::program(&src, "same");
    let ds = crafted_daily(&[10.0, 11.0, 10.0, 12.0, 13.0]);
    let r = run(&prog, &ds, ExecConfig::default()).unwrap();
    assert_eq!(r.decisions.len(), 1);
}

#[test]
fn partial_arithmetic_halts_with_rule_tuple_and_expression() {
    let src = common::strategy(
        "div0",
        "    qty : Quantity<Shares> = 10 shares",
        "    rel bad(A: Equity, @T, X: Scalar)
    bad(A, T, X) :- universe(A, T), close(A, T, P), X = P / (P - P).",
        "    decide(T, trade(A, qty)) :- bad(A, T, X), X > 0.",
    );
    let prog = common::program(&src, "div0");
    let ds = crafted_daily(&[10.0, 11.0, 10.0, 12.0, 13.0]);
    match run(&prog, &ds, ExecConfig::default()) {
        Err(RunError::Arithmetic { rule, bindings, expr, message }) => {
            assert_eq!(rule, "div0::bad#1");
            assert!(bindings.contains("A=X") && bindings.contains("T=2024-01-08"), "{}", bindings);
            assert!(expr.contains("/"), "{}", expr);
            assert_eq!(message, "division by zero");
        }
        other => panic!("expected an arithmetic halt, got {:?}", other.map(|r| r.decisions.len())),
    }
}

#[test]
fn window_minimum_withholds_the_aggregate() {
    let src = common::StrategyParts {
        name: "need3".into(),
        uses: "calendar, stat, bars, book".into(),
        params: "    qty : Quantity<Shares> = 10 shares
    lb : Duration = 10d
    k : Count = 3"
            .into(),
        decides: "    decide(T, trade(A, qty)) :- universe(A, T), mean_window[lb, k, close](A, T, M), close(A, T, P), P > M, flat(A, T).".into(),
        ..Default::default()
    }
    .render();
    let prog = common::program(&src, "need3");
    // Rising prices: the mean is below the close whenever it exists, and it
    // exists from the third bar on (three observations within ten days).
    let ds = crafted_daily(&[10.0, 11.0, 12.0, 13.0, 14.0]);
    let r = run(&prog, &ds, ExecConfig::default()).unwrap();
    assert_eq!(format_timestamp(r.decisions[0].t), "2024-01-10");
}

#[test]
fn explain_names_the_first_failing_literal() {
    let prog = up_down();
    let ds = crafted_daily(&[10.0, 11.0, 10.0, 12.0, 13.0]);
    let (_, mut k) = abt_tests::run_kernel(&prog, &ds, hand(1000.0)).unwrap();
    let decide_buy = (0..prog.rules.len()).find(|&i| prog.rule_label(i) == "up_down::decide#1").unwrap();
    let ex = k.explain(decide_buy, day("2024-01-10"), &[]).unwrap();
    assert_eq!(ex.failed_at.as_ref().map(|(i, _)| *i), Some(0), "{}", ex);
    let ex = k.explain(decide_buy, day("2024-01-12"), &[]).unwrap();
    assert_eq!(ex.failed_at.as_ref().map(|(i, l)| (*i, l.clone())), Some((1, "flat(A, T)".to_string())), "{}", ex);
    let ex = k.explain(decide_buy, day("2024-01-09"), &[]).unwrap();
    assert!(ex.failed_at.is_none() && ex.solutions == 1, "{}", ex);
    let up = (0..prog.rules.len()).find(|&i| prog.rule_label(i) == "up_down::up#1").unwrap();
    let ex = k.explain(up, day("2024-01-08"), &[]).unwrap();
    assert_eq!(ex.failed_at.as_ref().map(|(_, l)| l.clone()), Some("price_return(A, T, R)".to_string()));
}

/// One minute bar of `sym` at `t` (open, high and low at the close).
fn add_minute(ds: &mut Dataset, sym: Sym, t: i64, close: f64, volume: f64) {
    for rel in ["open", "high", "low", "close"] {
        ds.add(rel, vec![Value::Equity(sym), Value::Time(t), Value::Num(close)]);
    }
    ds.add("volume", vec![Value::Equity(sym), Value::Time(t), Value::Num(volume)]);
}

/// Minute closes of `sym` on the date `d` from 09:31, one a minute (labels
/// are close instants), and the day's listing.
fn minute_bars(ds: &mut Dataset, sym: Sym, d: &str, closes: &[f64]) {
    ds.add("listed", vec![Value::Equity(sym), Value::Time(day(d))]);
    for (i, p) in closes.iter().enumerate() {
        add_minute(
            ds,
            sym,
            day(d) + 9 * abt_kernel::kernel::time::HOUR + 31 * abt_kernel::kernel::time::MIN + abt_kernel::kernel::time::MIN * i as i64,
            *p,
            100.0,
        );
    }
}

/// A minute bundle decided on its daily calendar, with `rules` and `decides`.
fn daily_of_minutes(name: &str, params: &str, rules: &str, decides: &str) -> String {
    common::StrategyParts {
        name: name.into(),
        needs: "need bars @1m   need listing".into(),
        calendars: Some("daily = days".into()),
        clock: "decide on daily".into(),
        universe: "universe(A, T) :- listed(A, T), traded@daily(A, T).".into(),
        params: params.into(),
        rules: rules.into(),
        decides: decides.into(),
        report: "calendar daily   periods 252 per year".into(),
        ..Default::default()
    }
    .render()
}

/// The daily bars of a minute bundle are bucket aggregates of its minutes
/// (3.3): the first and last close of the day, over a minimum count of bars.
#[test]
fn a_bucket_aggregate_takes_the_days_closes_and_honours_a_minimum_count() {
    let src = daily_of_minutes(
        "daily_from_minutes",
        "    one : Quantity<Shares> = 1 shares
    min_bars : Count = 3",
        "    rel up_day(A: Equity, @T)
    up_day(A, T) :- universe(A, T), N = count(T1) over (T1 in bucket(daily, T), close(A, T1, _)), N >= min_bars,
        O = first(P) over (T1 in bucket(daily, T), close(A, T1, P)), C = last(P1) over (T1 in bucket(daily, T), close(A, T1, P1)), C > O.",
        "    decide(T, hold(A, one)) :- up_day(A, T).",
    );
    let prog = common::program(&src, "daily_from_minutes");
    let mut ds = Dataset::new();
    let y = ds.add_security("Y", "Y", day("2024-01-08"), None);
    minute_bars(&mut ds, y, "2024-01-08", &[10.0, 9.0, 12.0]); // up day: 10 -> 12
    minute_bars(&mut ds, y, "2024-01-09", &[12.0, 11.0]); // only two bars: below the minimum, no decision
    minute_bars(&mut ds, y, "2024-01-10", &[11.0, 13.0, 10.0]); // down day
    ds.derive_tickers();
    let r = run(&prog, &ds, ExecConfig::frictionless()).unwrap();
    // A daily label is the day's last minute bar.
    let ds_days: Vec<String> = r.decisions.iter().map(|d| format_timestamp(d.t)).collect();
    assert_eq!(ds_days, vec!["2024-01-08T09:33:00"]);
    // Filled at the next daily bar, at that day's last minute close (11.0).
    assert_eq!(r.fills.len(), 1);
    assert_eq!(format_timestamp(r.fills[0].t), "2024-01-09T09:32:00");
    assert_eq!(r.fills[0].price, 11.0);
}

#[test]
fn position_feedback_is_visible_one_bar_later() {
    let src = common::strategy(
        "see_fill",
        "    qty : Quantity<Shares> = 10 shares
    cooldown : Duration = 30d",
        "",
        "    decide(T, trade(A, qty)) :- universe(A, T), flat(A, T), not bought_within[cooldown](A, T).
    decide(T, trade(A, Q)) :- universe(A, T), fill(A, T, Q0, _), Q0 > 0 shares, Q = -Q0.",
    );
    let prog = common::program(&src, "see_fill");
    let ds = crafted_daily(&[10.0, 11.0, 10.0, 12.0, 13.0]);
    let r = run(&prog, &ds, ExecConfig::default()).unwrap();
    // Buy on day 1, see the fill on day 2 and sell; the cooldown blocks re-entry.
    assert_eq!(describe(&r), vec!["2024-01-08 trade(X, 10)", "2024-01-09 trade(X, -10)"]);
}

/// The names of the corpus strategies.
fn corpus_names() -> Vec<String> {
    common::dsl_files("strategies").iter().map(|f| f.file_stem().unwrap().to_string_lossy().to_string()).collect()
}

/// The daily market renames its second name and delists its last; the
/// names the strategies cite ("AAA", "BBB", "SPY") stay listed.
const SYMS: [&str; 5] = ["AAA", "XYZ", "BBB", "SPY", "DDD"];

/// The synthetic market of the program's class and base: `days` daily bars,
/// or thirty days of minute bars for a sub-daily base.
fn dataset_for(prog: &Program, days: usize, seed: u64) -> Dataset {
    let days = if prog.base.is_sub_daily() { 30 } else { days };
    synthetic_for_program(prog, &SYMS, (2022, 1, 3), days, seed)
}

#[test]
fn every_corpus_strategy_runs_and_is_deterministic() {
    for name in corpus_names() {
        let prog = common::corpus_program(&name);
        let ds = dataset_for(&prog, 400, 11);
        let cfg = ExecConfig::from_program(&prog);
        let rejects = cfg.on_leverage == OnLeverage::Reject;
        let a = run(&prog, &ds, cfg.clone()).unwrap_or_else(|e| panic!("{}: {}", name, e));
        let b = run(&prog, &ds, cfg).unwrap();
        let show = |r: &RunResult| -> Vec<String> { r.decisions.iter().map(|d| format!("{} {} {}", d.t, r.describe_decision(&d.decision), d.rule)).collect() };
        assert_eq!(show(&a), show(&b), "{} is not deterministic", name);
        assert_eq!(a.final_cash.to_bits(), b.final_cash.to_bits(), "{} is not deterministic", name);
        // Every bar has a price, so the only drops are last-bar decisions,
        // the decisions on the delisted name after its rows stop, and the
        // orders an execution block that rejects (`leverage ... reject`)
        // rejects as leverage.
        let last = *a.bars.last().unwrap();
        assert!(
            a.dropped
                .iter()
                .all(|(t, _, reason)| (*t == last && reason == "no next bar") || reason.contains("no price") || (rejects && reason.starts_with("rejected, would exceed"))),
            "{} dropped decisions: {:?}",
            name,
            a.dropped
        );
    }
}

#[test]
fn corpus_strategies_trade_on_synthetic_data() {
    // A strategy that never decides on a year of five random walks is not
    // exercising the kernel; the breakout and spike cases are rarer, so they
    // get a longer history.
    for name in corpus_names() {
        let prog = common::corpus_program(&name);
        let days = match name.as_str() {
            "breakout_52w" | "volume_spike" => 900,
            _ => 400,
        };
        let ds = dataset_for(&prog, days, 11);
        let r = run(&prog, &ds, ExecConfig::from_program(&prog)).unwrap_or_else(|e| panic!("{}: {}", name, e));
        assert!(!r.decisions.is_empty(), "{} made no decisions", name);
        assert!(!r.fills.is_empty(), "{} had no fills", name);
    }
}

/// The empirical check of the causality theorem on a corpus strategy: the
/// fold's decisions at sampled bars equal those of the fold over the data
/// available at each bar; the full run must decide somewhere, or the check
/// is vacuous.
fn causality_holds(name: &str) {
    let prog = common::corpus_program(name);
    let ds = dataset_for(&prog, 400, 5);
    let cfg = ExecConfig::from_program(&prog);
    let bars = abt_tests::loaded_kernel(&prog, &ds, cfg.clone()).decision_bars();
    let n = bars.len();
    let samples: Vec<i64> = [n / 3, n / 2, 2 * n / 3, n - 1].iter().map(|&i| bars[i]).collect();
    let full = abt_tests::run_socket(&prog, &ds, cfg.clone()).unwrap();
    assert!(!full.decisions.is_empty(), "{}: the fold decides nothing over {} bars", name, full.bars.len());
    let mismatches = verify_causality(&prog, &ds, cfg, &samples).unwrap();
    assert!(mismatches.is_empty(), "{}: {:?}", name, mismatches);
}

#[test]
fn causality_theorem_holds_empirically() {
    for name in ["sma_crossover", "trailing_stop", "momentum_top_n", "pairs_trading", "low_vol_portfolio"] {
        causality_holds(name);
    }
}

#[test]
fn causality_theorem_holds_empirically_on_minute_bundles() {
    for name in ["resampled_momentum", "opening_gap"] {
        causality_holds(name);
    }
}

#[test]
fn hold_and_trade_constructors_are_distinguished() {
    assert_eq!(Ctor::parse("trade").map(|c| c.is_target()), Some(false));
    assert_eq!(Ctor::parse("hold_weight").map(|c| c.is_target()), Some(true));
    assert_eq!(Ctor::parse("hold_quantity").map(|c| c.is_target()), Some(true));
    assert_eq!(Ctor::parse("buy"), None);
}

#[test]
fn a_missing_fill_price_drops_the_decision() {
    // The universe is the listing alone, so a bar without a close is still
    // one of X's rows (the listing and the volume stay).
    let prog = common::program(&up_down_source("universe(A, T) :- listed(A, T).".into()), "up_down");
    let mut ds = crafted_daily(&[10.0, 11.0, 10.0, 12.0, 13.0]);
    // Remove the close on the bar after the first buy (2024-01-10).
    let gone = day("2024-01-10");
    ds.facts.get_mut("close").unwrap().retain(|tu| tu[1] != Value::Time(gone));
    let r = run(&prog, &ds, hand(1000.0)).unwrap();
    // Thursday's return needs Wednesday's close too, so the next buy is
    // Friday's, which has no bar left to fill it: no fills at all, and both
    // decisions are dropped, each with its reason.
    let decisions: Vec<String> = r.decisions.iter().map(|d| format_timestamp(d.t)).collect();
    assert_eq!(decisions, vec!["2024-01-09", "2024-01-12"]);
    assert_eq!(r.dropped.len(), 2, "{:?}", r.dropped);
    assert_eq!(format_timestamp(r.dropped[0].0), "2024-01-09");
    assert_eq!(r.dropped[0].2, "no price for X at 2024-01-10");
    assert_eq!(format_timestamp(r.dropped[1].0), "2024-01-12");
    assert_eq!(r.dropped[1].2, "no next bar");
    assert!(r.fills.is_empty(), "{:?}", r.fills);
    assert_eq!(r.final_cash, 1000.0);
}

/// trend-14: a decision on the last bar has no next bar to fill at (section
/// 6), so it is recorded as dropped with that reason and the run's
/// arithmetic closes: decisions = fills + dropped.
#[test]
fn a_last_bar_decision_is_dropped_for_want_of_a_next_bar() {
    let prog = up_down();
    let ds = crafted_daily(&[10.0, 11.0, 10.0, 12.0, 13.0]);
    let r = run(&prog, &ds, hand(1000.0)).unwrap();
    // 01-09 buy, 01-10 sell, 01-11 buy, 01-12 sell: the last cannot fill.
    assert_eq!(r.decisions.len(), 4);
    assert_eq!(r.fills.len(), 3);
    assert_eq!(r.dropped.len(), 1, "{:?}", r.dropped);
    let (t, d, reason) = &r.dropped[0];
    assert_eq!(format_timestamp(*t), "2024-01-12");
    assert_eq!(r.describe_decision(d), "trade(X, -10)");
    assert!(reason.contains("no next bar"), "{}", reason);
    assert_eq!(r.decisions.len(), r.fills.len() + r.dropped.len());
}

/// trend-09: parameters are the only values the kernel may vary between
/// runs of one program (section 3) and its sweep axis (section 1):
/// `ExecConfig.param_overrides` replaces a default, checked against the
/// parameter's type and range.
/// W7 on the values a run uses: an override that leaves a calendar window
/// unable to hold its `min` rows is warned, naming the rule (the checker
/// judged the defaults); one that keeps the window feasible is not.
#[test]
fn an_override_that_empties_a_window_is_warned() {
    let prog = common::corpus_program("sma_crossover");
    let ds = synthetic_daily(&["AAA", "XYZ", "BBB"], (2022, 1, 3), 120, 3);
    let run_with = |v: &str| {
        let cfg = ExecConfig {
            param_overrides: vec![("fast".into(), abt_kernel::syntax::parse_lit(v).unwrap())],
            ..ExecConfig::from_program(&prog)
        };
        run(&prog, &ds, cfg).unwrap()
    };
    let r = run_with("10d");
    let w: Vec<&str> = r.warnings.iter().filter(|w| w.bias == "window sanity").map(|w| w.message.as_str()).collect();
    assert!(
        w.len() == 1 && w[0].contains("mean_window[fast, fast_min, close]") && w[0].contains("`min 12`") && w[0].contains("at most 10 bars"),
        "{:?}",
        r.warnings
    );
    assert!(r.decisions.is_empty(), "the fast window never fills, so nothing crosses");
    let r = run_with("30d");
    assert!(r.warnings.iter().all(|w| w.bias != "window sanity"), "{:?}", r.warnings);
}

#[test]
fn parameter_overrides_change_the_run_within_type_and_range() {
    let src = common::strategy(
        "hold_n",
        "    hold : Duration = 1d in 1d..5d
    qty : Quantity<Shares> = 10 shares
    cooldown : Duration = 30d",
        "",
        "    decide(T, trade(A, qty)) :- universe(A, T), flat(A, T), not bought_within[cooldown](A, T).
    decide(T, trade(A, Q)) :- held(A, T, Q0), lag(T, hold, T0), decided(T0, trade(A, Q1)), Q1 > 0 shares, Q = -Q0.",
    );
    let prog = common::program(&src, "hold_n");
    let ds = crafted_daily(&[10.0, 11.0, 10.0, 12.0, 13.0]);
    let with = |overrides: Vec<(&str, Lit)>| {
        run(
            &prog,
            &ds,
            ExecConfig {
                param_overrides: overrides.into_iter().map(|(n, l)| (n.to_string(), l)).collect(),
                ..ExecConfig::frictionless()
            },
        )
        .map_err(|e| e.to_string())
    };
    let days = |n: i64| {
        Lit::Duration(Duration {
            months: 0,
            days: n,
            ..Duration::default()
        })
    };
    assert_eq!(describe(&with(vec![]).unwrap()), vec!["2024-01-08 trade(X, 10)", "2024-01-09 trade(X, -10)"]);
    assert_eq!(describe(&with(vec![("hold", days(3))]).unwrap()), vec!["2024-01-08 trade(X, 10)", "2024-01-11 trade(X, -10)"]);
    assert_eq!(
        describe(&with(vec![("hold", days(3)), ("qty", Lit::Quantity(25.0, "shares".into()))]).unwrap()),
        vec!["2024-01-08 trade(X, 25)", "2024-01-11 trade(X, -25)"]
    );
    // Outside the range, of another type, or not a parameter: a request error, not a run.
    let err = with(vec![("hold", days(10))]).expect_err("10d is outside 1d..5d");
    assert!(err.contains("hold") && err.contains("1d..5d") && !err.contains("internal"), "{}", err);
    let err = with(vec![("hold", Lit::Int(3))]).expect_err("3 is not a Duration");
    assert!(err.contains("hold") && err.contains("Duration") && !err.contains("internal"), "{}", err);
    let err = with(vec![("nope", days(3))]).expect_err("no such parameter");
    assert!(err.contains("`nope`") && err.contains("hold_n") && !err.contains("internal"), "{}", err);
}

/// The corpus strategies with a time-based exit, their `hold` in days and
/// a synthetic run long enough to enter on every weekday.
const TIME_EXIT_STRATEGIES: [(&str, i64, u64, usize); 3] = [("volume_spike", 5, 7, 500), ("breakout_52w", 20, 1, 400), ("cash_buffer", 30, 11, 320)];

/// A time-based exit closes every entry, whatever weekday it was entered
/// on, once `hold` has elapsed since that entry and not before (trend-01).
#[test]
fn corpus_time_exits_close_every_entry_after_hold() {
    use abt_kernel::kernel::time::{weekday, DAY};
    for (name, hold, seed, days) in TIME_EXIT_STRATEGIES {
        let prog = common::corpus_program(name);
        let ds = synthetic_daily(&SYMS, (2022, 1, 3), days, seed);
        let r = run(&prog, &ds, ExecConfig::from_program(&prog)).unwrap();
        let last = *r.bars.last().unwrap();
        // The last bar each name is listed at: the market delists its last
        // name, which then leaves the universe and is sold at the next bar,
        // whatever `hold`.
        let mut last_listed: BTreeMap<String, i64> = BTreeMap::new();
        for tu in &ds.facts["listed"] {
            let eq = ds.symbols.name(tu[0].as_equity().unwrap()).to_string();
            let t = tu[1].as_time().unwrap();
            let e = last_listed.entry(eq).or_insert(t);
            *e = (*e).max(t);
        }
        // The latest bar at which an entry decided at t0 must have been
        // exited: the first bar strictly after t0 + hold, which a weekend can
        // delay by at most three calendar days.
        let deadline = |t0: i64| t0 + (hold + 3) * DAY;
        let mut open: BTreeMap<String, i64> = BTreeMap::new();
        let mut monday_entries = 0;
        for d in &r.decisions {
            let eq = r.symbols[d.decision.equity as usize].clone();
            match d.decision.ctor {
                Ctor::Trade if d.decision.amount > 0.0 => {
                    assert!(!open.contains_key(&eq), "{}: {} bought again at {} while held", name, eq, format_timestamp(d.t));
                    if weekday(d.t) == 0 {
                        monday_entries += 1;
                    }
                    open.insert(eq, d.t);
                }
                Ctor::Trade => {
                    let t0 = open.remove(&eq).unwrap_or_else(|| panic!("{}: {} sold at {} without an entry", name, eq, format_timestamp(d.t)));
                    let delisted = d.t > last_listed[&eq];
                    assert!(
                        delisted || d.t >= t0 + hold * DAY,
                        "{}: {} entered {} sold {} before hold elapsed",
                        name,
                        eq,
                        format_timestamp(t0),
                        format_timestamp(d.t)
                    );
                    assert!(d.t <= deadline(t0), "{}: {} entered {} sold only at {}", name, eq, format_timestamp(t0), format_timestamp(d.t));
                }
                other => panic!("{}: unexpected {:?}", name, other),
            }
        }
        for (eq, t0) in open {
            let dow = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"][weekday(t0) as usize];
            assert!(deadline(t0) > last, "{}: {} entered {} ({}) was never exited", name, eq, format_timestamp(t0), dow);
        }
        assert!(monday_entries > 0, "{}: no Monday entry in the run, so the weekend case is not exercised", name);
    }
}

/// `X` with flat volume and price and one up day on a 3x volume: the corpus
/// `volume_spike` entry on that day. `closes_removed` names bars whose close
/// is withheld (the listing and the volume stay, so the bars exist).
fn spike_daily(spike_on: &str, closes_removed: &[&str]) -> Dataset {
    let mut ds = Dataset::new();
    let bars = business_days((2024, 1, 8), 25);
    let x = ds.add_security("X", "X", bars[0], None);
    let spike = day(spike_on);
    let removed: Vec<i64> = closes_removed.iter().map(|d| day(d)).collect();
    for t in bars {
        let (price, volume) = if t < spike {
            (10.0, 1000.0)
        } else if t == spike {
            (10.5, 3000.0)
        } else {
            (10.5, 1000.0)
        };
        if removed.contains(&t) {
            ds.add("volume", vec![Value::Equity(x), Value::Time(t), Value::Num(volume)]);
            ds.add("listed", vec![Value::Equity(x), Value::Time(t)]);
        } else {
            common::add_bar(&mut ds, x, t, price, volume);
        }
    }
    ds.derive_tickers();
    ds
}

/// A time-based exit whose fill bar has no close does not leave the position
/// stuck: the exit is a liquidation, so it fills at the last known price
/// (section 6, executor policy), flagged as such (trend-02, #16).
#[test]
fn corpus_time_exit_fills_at_the_last_price_when_the_fill_bar_has_no_close() {
    let prog = common::corpus_program("volume_spike");
    // A Wednesday entry with hold 5d: the exit is due on the Tuesday after
    // next or the day after. Both bars that could fill it lack a close.
    let ds = spike_daily("2024-01-31", &["2024-02-06", "2024-02-07"]);
    let r = run(&prog, &ds, ExecConfig::from_program(&prog)).unwrap();
    let decisions = describe(&r);
    assert_eq!(decisions[0], "2024-01-31 trade(X, 100)", "{:?}", decisions);
    let sells = r.decisions.iter().filter(|d| d.decision.amount < 0.0).count();
    assert_eq!(sells, 1, "one exit, filled at the last price: {:?}", decisions);
    assert_eq!(r.fills.len(), 2, "{:?}", r.fills);
    assert!(r.fills[1].at_last_price && r.fills[1].quantity == -100.0, "{:?}", r.fills);
    assert!(r.dropped.iter().all(|(_, _, why)| why == "no next bar"), "{:?}", r.dropped);
    assert!(r.final_positions.is_empty(), "position stuck after an unpriced exit: {:?}", r.final_positions);
}

/// Four symbols, five consecutive weekdays, one fixed close per symbol.
fn cross_section(closes: &[(&str, f64)]) -> Dataset {
    let series: Vec<(&str, Vec<f64>)> = closes.iter().map(|(n, p)| (*n, vec![*p; 5])).collect();
    let names: Vec<(&str, &[f64])> = series.iter().map(|(n, v)| (*n, v.as_slice())).collect();
    common::markets((2024, 1, 8), &names)
}

fn quantile_cut(q: f64) -> String {
    common::strategy(
        "cut",
        &format!(
            "    q : Scalar = {}
    w : Scalar = 0.1",
            q
        ),
        "    rel cut(@T, H: Price<USD>, M: Price<USD>)
    cut(T, H, M) :- bar(T), H = quantile(P, q) over (close(_, T, P)) within (T), M = median(P1) over (close(_, T, P1)) within (T).",
        "    decide(T, hold(A, w)) :- universe(A, T), close(A, T, P), cut(T, H, M), H = M, P > H.",
    )
}

/// `quantile` interpolates linearly between order statistics and `median`
/// is the midpoint on an even count, so the two agree at 0.5; a level
/// outside [0, 1] halts the run naming the whole aggregate (portfolio-10).
#[test]
fn quantile_conventions_and_the_aggregate_halt() {
    let ds = cross_section(&[("A", 1.0), ("B", 2.0), ("C", 3.0), ("D", 4.0)]);
    let prog = common::program(&quantile_cut(0.5), "cut");
    let r = run(&prog, &ds, ExecConfig::default()).unwrap();
    // quantile(0.5) of [1, 2, 3, 4] is 2.5, as is the median: C and D are above it.
    let first: Vec<String> = r.decisions.iter().filter(|d| format_timestamp(d.t) == "2024-01-08").map(|d| r.describe_decision(&d.decision)).collect();
    assert_eq!(first, vec!["hold(C, 0.1)", "hold(D, 0.1)"]);

    let prog = common::program(&quantile_cut(1.5), "cut");
    match run(&prog, &ds, ExecConfig::default()) {
        Err(RunError::Arithmetic { rule, expr, message, .. }) => {
            assert_eq!(rule, "cut::cut#1");
            assert_eq!(expr, "quantile(P, q) over (...)");
            assert_eq!(message, "quantile level 1.5 is outside [0, 1]");
        }
        other => panic!("expected an arithmetic halt, got {:?}", other.map(|r| r.decisions.len())),
    }
}

/// An aggregation's conjunction admits comparisons and assignments over
/// the variables bound inside it, and they filter the group (trend-15).
#[test]
fn comparisons_and_assignments_inside_an_aggregation_filter_the_group() {
    let src = common::strategy(
        "n_up",
        "    qty : Quantity<Shares> = 10 shares
    lb : Duration = 30d
    floor : Notional<USD> = 100 USD
    n : Count = 3",
        "    rel above_ten(A: Equity, @T, N: Count)
    above_ten(A, T, N) :- universe(A, T),
        N = count(T1) over (T1 in window[lb] min 1, close(A, T1, P), Cost = P * qty, Cost > floor).",
        "    decide(T, trade(A, qty)) :- above_ten(A, T, N), N = n, flat(A, T).",
    );
    let prog = common::program(&src, "n_up");
    // Closes above 10 within the window: 0, 1, 1, 2, 3 on the five days.
    let ds = crafted_daily(&[10.0, 11.0, 10.0, 12.0, 13.0]);
    let r = run(&prog, &ds, ExecConfig::default()).unwrap();
    let decisions: Vec<String> = r.decisions.iter().map(|d| format_timestamp(d.t)).collect();
    assert_eq!(decisions, vec!["2024-01-12"]);
}

/// Evaluation is by demand, so a degenerate tuple halts the run only when a
/// decision asks for it: `1 / N` with N = 0 on the warm-up bars is never
/// requested, because nothing is selected there (adv-15).
#[test]
fn partial_arithmetic_is_only_evaluated_for_demanded_tuples() {
    let unguarded = |decide: &str| {
        common::strategy(
            "unguarded",
            "    lookback : Duration = 6mo
    skip : Duration = 1mo
    n : Count = 2",
            "    rel momentum(A: Equity, @T, M: Scalar)
    momentum(A, T, M) :- universe(A, T), lag(T, skip, T1), close(A, T1, P1), lag(T, lookback, T0), close(A, T0, P0), M = P1 / P0 - 1.
    rel selected(A: Equity, @T)
    selected(A, T) :- bar(T), top[n](momentum(A, T, M) by (M desc, A asc)) within (T).
    rel n_selected(@T, N: Count)
    n_selected(T, N) :- bar(T), N = count(A) over (selected(A, T)) within (T).
    rel w(@T, W: Scalar)
    w(T, W) :- n_selected(T, N), W = 1 / N.",
            decide,
        )
    };
    let prog = common::program(&unguarded("    decide(T, hold(A, W)) :- selected(A, T), w(T, W)."), "unguarded");
    let ds = synthetic_daily(&SYMS, (2022, 1, 3), 200, 3);
    let allow = || ExecConfig {
        on_leverage: OnLeverage::Allow,
        ..ExecConfig::frictionless()
    };
    let r = run(&prog, &ds, allow()).unwrap();
    assert!(!r.decisions.is_empty());
    // Demanded at every bar ahead of `selected`, the undefined `w` is read
    // on the warm-up bars, but the body is a Kleene conjunction (language-v2,
    // 5.1): `selected(A, T)` is false there, so the instance is excluded, not
    // stuck, and the run equals the one above. The written order of a body
    // never decides whether a run halts.
    let prog2 = common::program(&unguarded("    decide(T, hold(A, W)) :- bar(T), w(T, W), selected(A, T)."), "unguarded");
    let r2 = run(&prog2, &ds, allow()).unwrap();
    let show = |r: &RunResult| -> Vec<String> { r.decisions.iter().map(|d| format!("{} {}", d.t, r.describe_decision(&d.decision))).collect() };
    assert_eq!(show(&r), show(&r2));
    assert!(r2.stats.excluded_after_undefined > 0, "{:?}", r2.stats);
    // With nothing after it to exclude the instance, the undefined `w` is
    // stuck and halts the run on the first bar it is demanded at.
    let prog3 = common::program(&unguarded("    decide(T, hold(A, W)) :- universe(A, T), w(T, W)."), "unguarded");
    match run(&prog3, &ds, allow()) {
        Err(RunError::Arithmetic { rule, message, .. }) => {
            assert_eq!(rule, "unguarded::w#1");
            assert_eq!(message, "division by zero");
        }
        other => panic!("expected an arithmetic halt, got {:?}", other.map(|r| r.decisions.len())),
    }
}

/// Minute bars for `Y` on consecutive weekdays from 2024-01-08:
/// `bars_per_day[i]` bars on day i from 09:31, closes `start_price + step *
/// i + 0.01 * bar`. Labels are close instants.
fn minute_days(bars_per_day: &[usize], start_price: f64, step: f64) -> Dataset {
    let mut ds = Dataset::new();
    let days = business_days((2024, 1, 8), bars_per_day.len());
    let y = ds.add_security("Y", "Y", days[0], None);
    for (i, (d, &n)) in days.into_iter().zip(bars_per_day).enumerate() {
        ds.add("listed", vec![Value::Equity(y), Value::Time(d)]);
        for b in 0..n {
            let t = d + 9 * abt_kernel::kernel::time::HOUR + 31 * abt_kernel::kernel::time::MIN + abt_kernel::kernel::time::MIN * b as i64;
            add_minute(&mut ds, y, t, start_price + step * i as f64 + 0.01 * b as f64, 100.0);
        }
    }
    ds.derive_tickers();
    ds
}

/// A minimum count removes a bar from a relation, not from the time domain:
/// a half-day with fewer than 300 minute bars has no `close_d`, yet it is a
/// daily bar, `prev` lands on it, the executor fills there at its last
/// minute close, and a rule that needs `close_d` at prev(T) does not fire on
/// the day after (intraday-06, intraday-09).
#[test]
fn a_short_day_stays_in_the_domain_without_a_bar() {
    let src = daily_of_minutes(
        "up_vs_prev",
        "    one : Quantity<Shares> = 1 shares
    full : Count = 300",
        "    # The day's close, only on a day with a full session of bars.
    rel close_d(A: Equity, @T, C: Price<USD>)
    close_d(A, T, C) :- universe(A, T), N = count(T1) over (T1 in bucket(daily, T), close(A, T1, _)), N >= full, close@daily(A, T, C).
    rel up(A: Equity, @T)
    up(A, T) :- universe(A, T), close_d(A, T, C), prev(T, T0), close_d(A, T0, C0), C > C0.",
        "    decide(T, hold(A, one)) :- up(A, T).",
    );
    let prog = common::program(&src, "up_vs_prev");
    // Mon..Fri: full, full, half-day (150 bars), full, full.
    let ds = minute_days(&[300, 300, 150, 300, 300], 50.0, 1.0);
    let bars = abt_tests::loaded_kernel(&prog, &ds, ExecConfig::default()).decision_bars();
    // A daily label is the day's last minute bar.
    let days: Vec<String> = bars.iter().map(|t| format_timestamp(*t)[..10].to_string()).collect();
    assert_eq!(days, vec!["2024-01-08", "2024-01-09", "2024-01-10", "2024-01-11", "2024-01-12"]);
    let r = run(&prog, &ds, ExecConfig::frictionless()).unwrap();
    // Tuesday fires (Monday has a bar); Wednesday has no close_d; Thursday's
    // prev is Wednesday, so it does not fire either; Friday fires again.
    let decisions: Vec<String> = r.decisions.iter().map(|d| format_timestamp(d.t)[..10].to_string()).collect();
    assert_eq!(decisions, vec!["2024-01-09", "2024-01-12"]);
    // Tuesday's decision is filled on the half-day at its last minute close
    // (bar 149 of the third day: 52 + 1.49).
    assert_eq!(r.fills.len(), 1);
    assert_eq!(format_timestamp(r.fills[0].t), "2024-01-10T12:00:00");
    assert!((r.fills[0].price - 53.49).abs() < 1e-9, "{}", r.fills[0].price);
}

/// Timestamps are bar close instants: the synthetic minute market labels
/// the first bar of a session 09:31 and the 390th 16:00, so every @5m
/// bucket holds whole bars (intraday-07).
#[test]
fn synthetic_minute_bars_are_labelled_by_close_instant() {
    use abt_kernel::kernel::time::bucket;
    use abt_kernel::Resolution;
    let ds = synthetic_minute(&["AAA"], (2024, 1, 8), 1, 390, 1);
    let mut times: Vec<i64> = ds.facts["close"].iter().map(|tu| tu[1].as_time().unwrap()).collect();
    times.sort();
    assert_eq!(format_timestamp(times[0]), "2024-01-08T09:31:00");
    assert_eq!(format_timestamp(times[389]), "2024-01-08T16:00:00");
    assert_eq!(bucket(Resolution::M5, times[0]), times[4]);
    assert_eq!(bucket(Resolution::M5, times[4]), times[4]);
}

/// A parameter override may not change what the checker judged on the
/// default: `lag` by a zero duration is causal rather than strict (section 5,
/// WF-4), so flipping a lag duration between zero and non-zero would turn a
/// well-formed recursion into a same-time cycle. The kernel refuses it with a
/// user-facing error instead of halting with an internal one.
#[test]
fn a_parameter_override_may_not_flip_a_lag_duration_to_zero() {
    let src = common::strategy(
        "lagz",
        "    d : Duration = 5d in 0d..30d
    qty : Quantity<Shares> = 1 shares
    limit : Scalar = 100",
        "    rel cnt(A: Equity, @T, N: Scalar)
    cnt(A, T, N) :- universe(A, T), lag(T, d, T1), cnt(A, T1, M), N = M + 1.
    cnt(A, T, N) :- universe(A, T), lag(T, d, T1), not cnt(A, T1, _), N = 0.",
        "    decide(T, trade(A, qty)) :- universe(A, T), cnt(A, T, N), N > limit.",
    );
    let prog = common::program(&src, "lagz");
    let ds = synthetic_daily(&["AAA"], (2023, 1, 2), 40, 3);
    let fine = ExecConfig {
        param_overrides: vec![(
            "d".to_string(),
            Lit::Duration(Duration {
                months: 0,
                days: 7,
                ..Duration::default()
            }),
        )],
        ..ExecConfig::frictionless()
    };
    run(&prog, &ds, fine).unwrap();
    let flipped = ExecConfig {
        param_overrides: vec![(
            "d".to_string(),
            Lit::Duration(Duration {
                months: 0,
                days: 0,
                ..Duration::default()
            }),
        )],
        ..ExecConfig::frictionless()
    };
    match run(&prog, &ds, flipped) {
        Err(RunError::Request(m)) => assert!(m.contains("lag") && m.contains("zero") && !m.contains("internal"), "{}", m),
        other => panic!("expected the override to be refused, got {:?}", other.map(|r| r.decisions.len())),
    }
}

/// Data-bundle doc, section 7: the run's window. Bars before `start` are
/// data only (a lag reaches them), the executor and the decisions start at
/// `start`, and nothing runs after `end`; both drivers agree.
#[test]
fn a_run_window_warms_up_on_earlier_bars_and_trades_inside_it() {
    let src = common::strategy(
        "windowed",
        "    w : Scalar = 0.5",
        "    rel up(A: Equity, @T)
    up(A, T) :- universe(A, T), close(A, T, P), prev(T, T0), close(A, T0, P0), P > P0.",
        "    decide(T, hold(A, w)) :- up(A, T).",
    );
    let prog = common::program(&src, "windowed");
    let ds = crafted_daily(&[10.0, 11.0, 12.0, 13.0, 14.0, 15.0]);
    let days = business_days((2024, 1, 8), 6);
    let cfg = ExecConfig {
        start: Some(days[2]),
        end: Some(days[4]),
        ..ExecConfig::frictionless()
    };
    let r = run(&prog, &ds, cfg.clone()).unwrap();
    assert_eq!(r.bars, days[2..=4].to_vec());
    // The first decision bar sees day 2 through prev: it decides at once.
    assert_eq!(r.decisions.first().map(|d| d.t), Some(days[2]));
    assert_eq!(r.equity_curve.first().map(|(t, _)| *t), Some(days[2]));
    let f = abt_tests::run_socket(&prog, &ds, cfg).unwrap();
    assert_eq!(f.bars, r.bars);
    assert_eq!(f.final_cash.to_bits(), r.final_cash.to_bits());
}
