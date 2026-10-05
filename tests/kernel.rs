//! Kernel tests: hand-computed executor outcomes, every corpus strategy run
//! end to end, determinism, the causality theorem, and the runtime
//! diagnostics of section 7.

mod corpus;

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;

use absolute_backtest::check::{check_program, Program, Workspace};
use absolute_backtest::data::{synthetic_daily, synthetic_minute};
use absolute_backtest::kernel::time::{format_timestamp, parse_timestamp};
use absolute_backtest::kernel::{run, verify_causality, Ctor, Dataset, ExecConfig, Kernel, OnLeverage, RunError, Value};
use absolute_backtest::{Duration, Lit};

fn program(extra: &str, name: &str) -> (Program, Workspace) {
    let mut ws = corpus::base_workspace();
    ws.add_source(extra).unwrap();
    let (p, diags) = check_program(&ws, name);
    let text: Vec<String> = diags.iter().map(|d| d.to_string()).collect();
    (p.unwrap_or_else(|| panic!("{} should check:\n{}", name, text.join("\n"))), ws)
}

fn day(s: &str) -> i64 {
    parse_timestamp(s).unwrap()
}

/// One symbol, five consecutive weekdays, crafted closes.
fn crafted_daily(prices: &[f64]) -> Dataset {
    let mut ds = Dataset::new();
    let x = ds.intern("X");
    let days = ["2024-01-08", "2024-01-09", "2024-01-10", "2024-01-11", "2024-01-12"];
    for (d, p) in days.iter().zip(prices) {
        let t = day(d);
        ds.add("close", vec![Value::Equity(x), Value::Time(t), Value::Num(*p)]);
        ds.add("volume", vec![Value::Equity(x), Value::Time(t), Value::Num(1000.0)]);
        ds.add("universe", vec![Value::Equity(x), Value::Time(t)]);
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

#[test]
fn executor_contract_by_hand() {
    let (prog, _) = program(UP_DOWN, "up_down");
    let ds = crafted_daily(&[10.0, 11.0, 10.0, 12.0, 13.0]);
    let r = run(
        &prog,
        &ds,
        ExecConfig {
            initial_cash: 1000.0,
            ..ExecConfig::frictionless()
        },
    )
    .unwrap();
    let decisions: Vec<String> = r.decisions.iter().map(|d| format!("{} {}", format_timestamp(d.t), r.describe_decision(&d.decision))).collect();
    assert_eq!(decisions, vec!["2024-01-09 buy(X, 10)", "2024-01-10 sell(X, 10)", "2024-01-11 buy(X, 10)", "2024-01-12 sell(X, 10)"]);
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
    let (prog, _) = program(UP_DOWN, "up_down");
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
    let src = r#"
strategy half {
  env equities_1d
  uses features
  resolution @1d
  mode target
  param w : Scalar = 0.5
  decide(T, target_weight(A, w)) :- universe(A, T).
}
"#;
    let (prog, _) = program(src, "half");
    let ds = crafted_daily(&[10.0, 10.0, 20.0, 20.0, 20.0]);
    let r = run(
        &prog,
        &ds,
        ExecConfig {
            initial_cash: 1000.0,
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
}

#[test]
fn conflicting_decisions_halt_naming_both_rules() {
    let src = r#"
strategy conflict {
  env equities_1d
  uses features
  resolution @1d
  mode delta
  param a : Quantity<Shares> = 10 shares
  param b : Quantity<Shares> = 20 shares
  decide(T, buy(A, a)) :- universe(A, T).
  decide(T, buy(A, b)) :- universe(A, T).
}
"#;
    let (prog, _) = program(src, "conflict");
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
    let src = r#"
strategy same {
  env equities_1d
  uses features
  resolution @1d
  mode delta
  param a : Quantity<Shares> = 10 shares
  decide(T, buy(A, a)) :- universe(A, T), flat(A, T).
  decide(T, buy(A, a)) :- universe(A, T), close(A, T, _), flat(A, T).
}
"#;
    let (prog, _) = program(src, "same");
    let ds = crafted_daily(&[10.0, 11.0, 10.0, 12.0, 13.0]);
    let r = run(&prog, &ds, ExecConfig::default()).unwrap();
    assert_eq!(r.decisions.len(), 1);
}

#[test]
fn partial_arithmetic_halts_with_rule_tuple_and_expression() {
    let src = r#"
strategy div0 {
  env equities_1d
  uses features
  resolution @1d
  mode delta
  param qty : Quantity<Shares> = 10 shares
  rel bad(-A: Equity, @T: Timestamp, -X: Scalar)
  bad(A, T, X) :- universe(A, T), close(A, T, P), X = P / (P - P).
  decide(T, buy(A, qty)) :- bad(A, T, X), X > 0.
}
"#;
    let (prog, _) = program(src, "div0");
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
    let src = r#"
strategy need3 {
  env equities_1d
  uses features
  resolution @1d
  mode delta
  param qty : Quantity<Shares> = 10 shares
  decide(T, buy(A, qty)) :- universe(A, T), sma(A, T, 10d, 3, M), close(A, T, P), P > M, flat(A, T).
}
"#;
    let (prog, _) = program(src, "need3");
    // Rising prices: the sma is below the close whenever it exists, and it
    // exists from the third bar on (three observations within ten days).
    let ds = crafted_daily(&[10.0, 11.0, 12.0, 13.0, 14.0]);
    let r = run(&prog, &ds, ExecConfig::default()).unwrap();
    assert_eq!(format_timestamp(r.decisions[0].t), "2024-01-10");
}

#[test]
fn delta_quantities_must_be_positive() {
    let src = r#"
strategy neg {
  env equities_1d
  uses features
  resolution @1d
  mode delta
  param qty : Quantity<Shares> = 0 shares
  decide(T, buy(A, qty)) :- universe(A, T).
}
"#;
    let (prog, _) = program(src, "neg");
    let ds = crafted_daily(&[10.0, 11.0, 10.0, 12.0, 13.0]);
    assert!(matches!(run(&prog, &ds, ExecConfig::default()), Err(RunError::BadDecision { .. })));
}

#[test]
fn explain_names_the_first_failing_literal() {
    let (prog, _) = program(UP_DOWN, "up_down");
    let ds = crafted_daily(&[10.0, 11.0, 10.0, 12.0, 13.0]);
    let mut k = Kernel::new(
        &prog,
        &ds,
        ExecConfig {
            initial_cash: 1000.0,
            ..ExecConfig::frictionless()
        },
    )
    .unwrap();
    k.run().unwrap();
    let decide_buy = (0..prog.rules.len()).find(|&i| prog.rule_label(i) == "up_down::decide#1").unwrap();
    let ex = k.explain(decide_buy, day("2024-01-10"), &[]).unwrap();
    assert_eq!(ex.failed_at.as_ref().map(|(i, _)| *i), Some(0), "{}", ex);
    let ex = k.explain(decide_buy, day("2024-01-12"), &[]).unwrap();
    assert_eq!(ex.failed_at.as_ref().map(|(i, l)| (*i, l.clone())), Some((1, "flat(A, T)".to_string())), "{}", ex);
    let ex = k.explain(decide_buy, day("2024-01-09"), &[]).unwrap();
    assert!(ex.failed_at.is_none() && ex.solutions == 1, "{}", ex);
    let up = (0..prog.rules.len()).find(|&i| prog.rule_label(i) == "up_down::up#1").unwrap();
    let ex = k.explain(up, day("2024-01-08"), &[]).unwrap();
    assert_eq!(ex.failed_at.as_ref().map(|(_, l)| l.clone()), Some("logret(A, T, R)".to_string()));
}

#[test]
fn resample_takes_the_last_fine_close_and_honours_min() {
    let src = r#"
strategy daily_from_minutes {
  env equities_1m
  uses bars
  resolution @1d
  mode target
  rel up_day(-A: Equity, @T: Timestamp)
  up_day(A, T) :- universe_d(A, T), resample(close_m(A, T1, P) to @1d as T, min 3, O = first(P), C = last(P)), C > O.
  decide(T, target_quantity(A, 1 shares)) :- up_day(A, T).
}
"#;
    let (prog, _) = program(src, "daily_from_minutes");
    let mut ds = Dataset::new();
    let y = ds.intern("Y");
    let bars = |d: &str, prices: &[f64], ds: &mut Dataset| {
        for (i, p) in prices.iter().enumerate() {
            let t = day(d) + 9 * 3600 + 31 * 60 + 60 * i as i64;
            ds.add("close_m", vec![Value::Equity(y), Value::Time(t), Value::Num(*p)]);
            ds.add("universe_m", vec![Value::Equity(y), Value::Time(t)]);
        }
    };
    bars("2024-01-08", &[10.0, 9.0, 12.0], &mut ds); // up day: 10 -> 12
    bars("2024-01-09", &[12.0, 11.0], &mut ds); // only two bars: below min 3, no bar
    bars("2024-01-10", &[11.0, 13.0, 10.0], &mut ds); // down day
    let r = run(&prog, &ds, ExecConfig::default()).unwrap();
    let ds_days: Vec<String> = r.decisions.iter().map(|d| format_timestamp(d.t)).collect();
    assert_eq!(ds_days, vec!["2024-01-08"]);
    // Filled at the next daily bar, at that day's last minute close (11.0).
    assert_eq!(r.fills.len(), 1);
    assert_eq!(format_timestamp(r.fills[0].t), "2024-01-09");
    assert_eq!(r.fills[0].price, 11.0);
}

#[test]
fn position_feedback_is_visible_one_bar_later() {
    let src = r#"
strategy see_fill {
  env equities_1d
  uses features
  resolution @1d
  mode delta
  param qty : Quantity<Shares> = 10 shares
  decide(T, buy(A, qty)) :- universe(A, T), flat(A, T), not bought_within(A, T, 30d).
  decide(T, sell(A, Q)) :- universe(A, T), fill(A, T, Q, _), Q > 0 shares.
}
"#;
    let (prog, _) = program(src, "see_fill");
    let ds = crafted_daily(&[10.0, 11.0, 10.0, 12.0, 13.0]);
    let r = run(&prog, &ds, ExecConfig::default()).unwrap();
    let decisions: Vec<String> = r.decisions.iter().map(|d| format!("{} {}", format_timestamp(d.t), r.describe_decision(&d.decision))).collect();
    // Buy on day 1, see the fill on day 2 and sell; the cooldown blocks re-entry.
    assert_eq!(decisions, vec!["2024-01-08 buy(X, 10)", "2024-01-09 sell(X, 10)"]);
}

fn corpus_strategy_sources() -> Vec<(String, String)> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("corpus/strategies");
    let mut files: Vec<_> = fs::read_dir(dir).unwrap().map(|e| e.unwrap().path()).collect();
    files.sort();
    files
        .into_iter()
        .map(|f| (f.file_stem().unwrap().to_string_lossy().to_string(), fs::read_to_string(f).unwrap()))
        .collect()
}

const DAILY_SYMBOLS: [&str; 5] = ["AAA", "BBB", "CCC", "DDD", "SPY"];

fn dataset_for(prog: &Program, seed: u64) -> Dataset {
    let minute = prog.relations.values().any(|s| s.res == Some(absolute_backtest::Resolution::M1));
    if minute {
        synthetic_minute(&DAILY_SYMBOLS, (2024, 1, 2), 30, 320, seed)
    } else {
        synthetic_daily(&DAILY_SYMBOLS, (2022, 1, 3), 320, seed)
    }
}

#[test]
fn every_corpus_strategy_runs_and_is_deterministic() {
    for (name, src) in corpus_strategy_sources() {
        let (prog, _) = program(&src, &name);
        let ds = dataset_for(&prog, 11);
        let a = run(&prog, &ds, ExecConfig::default()).unwrap_or_else(|e| panic!("{}: {}", name, e));
        let b = run(&prog, &ds, ExecConfig::default()).unwrap();
        let show = |r: &absolute_backtest::kernel::RunResult| -> Vec<String> { r.decisions.iter().map(|d| format!("{} {} {}", d.t, r.describe_decision(&d.decision), d.rule)).collect() };
        assert_eq!(show(&a), show(&b), "{} is not deterministic", name);
        assert_eq!(a.final_cash.to_bits(), b.final_cash.to_bits(), "{} is not deterministic", name);
        let modes: BTreeSet<_> = a.decisions.iter().map(|d| d.decision.ctor.mode()).collect();
        assert!(modes.iter().all(|m| *m == prog.mode), "{} emitted decisions outside its mode", name);
        // Every bar has a price, so the only drops are last-bar decisions.
        let last = *a.bars.last().unwrap();
        assert!(
            a.dropped.iter().all(|(t, _, reason)| *t == last && reason == "no next bar"),
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
    for (name, src) in corpus_strategy_sources() {
        let (prog, _) = program(&src, &name);
        let ds = match name.as_str() {
            "breakout_52w" | "volume_spike" => synthetic_daily(&DAILY_SYMBOLS, (2022, 1, 3), 900, 11),
            _ => dataset_for(&prog, 11),
        };
        let r = run(&prog, &ds, ExecConfig::default()).unwrap_or_else(|e| panic!("{}: {}", name, e));
        assert!(!r.decisions.is_empty(), "{} made no decisions", name);
        assert!(!r.fills.is_empty(), "{} had no fills", name);
    }
}

#[test]
fn causality_theorem_holds_empirically() {
    for name in [
        "sma_crossover",
        "trailing_stop",
        "momentum_top_n",
        "pairs_trading",
        "low_vol_portfolio",
        "resampled_momentum",
        "opening_gap",
    ] {
        let src = fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join(format!("corpus/strategies/{}.dsl", name))).unwrap();
        let (prog, _) = program(&src, name);
        let ds = dataset_for(&prog, 5);
        let bars = Kernel::new(&prog, &ds, ExecConfig::default()).unwrap().decision_bars();
        let n = bars.len();
        let samples: Vec<i64> = [n / 3, n / 2, 2 * n / 3, n - 1].iter().map(|&i| bars[i]).collect();
        let mismatches = verify_causality(&prog, &ds, ExecConfig::default(), &samples).unwrap();
        assert!(mismatches.is_empty(), "{}: {:?}", name, mismatches);
    }
}

#[test]
fn delta_and_target_constructors_are_distinguished() {
    assert_eq!(Ctor::parse("buy").map(|c| c.mode()), Some(absolute_backtest::DecisionMode::Delta));
    assert_eq!(Ctor::parse("target_weight").map(|c| c.mode()), Some(absolute_backtest::DecisionMode::Target));
}

#[test]
fn a_missing_fill_price_drops_the_decision() {
    let (prog, _) = program(UP_DOWN, "up_down");
    let mut ds = crafted_daily(&[10.0, 11.0, 10.0, 12.0, 13.0]);
    // Remove the close on the bar after the first buy (2024-01-10); the
    // universe and volume rows stay, so the bar still exists.
    let gone = day("2024-01-10");
    ds.facts.get_mut("close").unwrap().retain(|tu| tu[1] != Value::Time(gone));
    let r = run(
        &prog,
        &ds,
        ExecConfig {
            initial_cash: 1000.0,
            ..ExecConfig::frictionless()
        },
    )
    .unwrap();
    // Thursday's log return needs Wednesday's close too, so the next buy is
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
    let (prog, _) = program(UP_DOWN, "up_down");
    let ds = crafted_daily(&[10.0, 11.0, 10.0, 12.0, 13.0]);
    let r = run(
        &prog,
        &ds,
        ExecConfig {
            initial_cash: 1000.0,
            ..ExecConfig::frictionless()
        },
    )
    .unwrap();
    // 01-09 buy, 01-10 sell, 01-11 buy, 01-12 sell: the last cannot fill.
    assert_eq!(r.decisions.len(), 4);
    assert_eq!(r.fills.len(), 3);
    assert_eq!(r.dropped.len(), 1, "{:?}", r.dropped);
    let (t, d, reason) = &r.dropped[0];
    assert_eq!(format_timestamp(*t), "2024-01-12");
    assert_eq!(r.describe_decision(d), "sell(X, 10)");
    assert!(reason.contains("no next bar"), "{}", reason);
    assert_eq!(r.decisions.len(), r.fills.len() + r.dropped.len());
}

/// trend-09: parameters are the only values the kernel may vary between
/// runs of one program (section 3) and its sweep axis (section 1):
/// `ExecConfig.param_overrides` replaces a default, checked against the
/// parameter's type and range.
#[test]
fn parameter_overrides_change_the_run_within_type_and_range() {
    let src = r#"
strategy hold_n {
  env equities_1d
  uses features
  resolution @1d
  mode delta
  param hold : Duration = 1d in 1d..5d
  param qty : Quantity<Shares> = 10 shares
  decide(T, buy(A, qty)) :- universe(A, T), flat(A, T), not bought_within(A, T, 30d).
  decide(T, sell(A, Q)) :- held(A, T, Q), lag(T, hold, T0), decided(T0, buy(A, _)).
}
"#;
    let (prog, _) = program(src, "hold_n");
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
    let show = |r: &absolute_backtest::kernel::RunResult| -> Vec<String> { r.decisions.iter().map(|d| format!("{} {}", format_timestamp(d.t), r.describe_decision(&d.decision))).collect() };
    let days = |n: i64| Lit::Duration(Duration { months: 0, days: n });
    assert_eq!(show(&with(vec![]).unwrap()), vec!["2024-01-08 buy(X, 10)", "2024-01-09 sell(X, 10)"]);
    assert_eq!(show(&with(vec![("hold", days(3))]).unwrap()), vec!["2024-01-08 buy(X, 10)", "2024-01-11 sell(X, 10)"]);
    assert_eq!(
        show(&with(vec![("hold", days(3)), ("qty", Lit::Shares(25.0))]).unwrap()),
        vec!["2024-01-08 buy(X, 25)", "2024-01-11 sell(X, 25)"]
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
    use absolute_backtest::kernel::time::{weekday, DAY};
    for (name, hold, seed, days) in TIME_EXIT_STRATEGIES {
        let src = fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join(format!("corpus/strategies/{}.dsl", name))).unwrap();
        let (prog, _) = program(&src, name);
        let ds = synthetic_daily(&DAILY_SYMBOLS, (2022, 1, 3), days, seed);
        let r = run(&prog, &ds, ExecConfig::default()).unwrap();
        let last = *r.bars.last().unwrap();
        // The latest bar at which an entry decided at t0 must have been
        // exited: the first bar strictly after t0 + hold, which a weekend can
        // delay by at most three calendar days.
        let deadline = |t0: i64| t0 + (hold + 3) * DAY;
        let mut open: BTreeMap<String, i64> = BTreeMap::new();
        let mut monday_entries = 0;
        for d in &r.decisions {
            let eq = r.symbols[d.decision.equity as usize].clone();
            match d.decision.ctor {
                Ctor::Buy => {
                    assert!(!open.contains_key(&eq), "{}: {} bought again at {} while held", name, eq, format_timestamp(d.t));
                    if weekday(d.t) == 0 {
                        monday_entries += 1;
                    }
                    open.insert(eq, d.t);
                }
                Ctor::Sell => {
                    let t0 = open.remove(&eq).unwrap_or_else(|| panic!("{}: {} sold at {} without an entry", name, eq, format_timestamp(d.t)));
                    assert!(
                        d.t >= t0 + hold * DAY,
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
/// is withheld (the universe and volume rows stay, so the bars exist).
fn spike_daily(spike_on: &str, closes_removed: &[&str]) -> Dataset {
    use absolute_backtest::data::business_days;
    let mut ds = Dataset::new();
    let x = ds.intern("X");
    let spike = day(spike_on);
    let removed: Vec<i64> = closes_removed.iter().map(|d| day(d)).collect();
    for t in business_days((2024, 1, 8), 25) {
        let (price, volume) = if t < spike {
            (10.0, 1000.0)
        } else if t == spike {
            (10.5, 3000.0)
        } else {
            (10.5, 1000.0)
        };
        if !removed.contains(&t) {
            ds.add("close", vec![Value::Equity(x), Value::Time(t), Value::Num(price)]);
        }
        ds.add("volume", vec![Value::Equity(x), Value::Time(t), Value::Num(volume)]);
        ds.add("universe", vec![Value::Equity(x), Value::Time(t)]);
    }
    ds
}

/// A time-based exit whose fill bar has no close does not leave the position
/// stuck: the exit is a liquidation, so it fills at the last known price
/// (section 6, executor policy), flagged as such (trend-02, #16).
#[test]
fn corpus_time_exit_fills_at_the_last_price_when_the_fill_bar_has_no_close() {
    let src = fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("corpus/strategies/volume_spike.dsl")).unwrap();
    let (prog, _) = program(&src, "volume_spike");
    // A Wednesday entry with hold 5d: the exit is due on the Monday after
    // next or the day after. Both bars that could fill it lack a close.
    let ds = spike_daily("2024-01-31", &["2024-02-06", "2024-02-07"]);
    let r = run(&prog, &ds, ExecConfig::default()).unwrap();
    let decisions: Vec<String> = r.decisions.iter().map(|d| format!("{} {}", format_timestamp(d.t), r.describe_decision(&d.decision))).collect();
    assert_eq!(decisions[0], "2024-01-31 buy(X, 100)", "{:?}", decisions);
    let sells = decisions.iter().filter(|d| d.contains("sell")).count();
    assert_eq!(sells, 1, "one exit, filled at the last price: {:?}", decisions);
    assert_eq!(r.fills.len(), 2, "{:?}", r.fills);
    assert!(r.fills[1].at_last_price && r.fills[1].quantity == -100.0, "{:?}", r.fills);
    assert!(r.dropped.iter().all(|(_, _, why)| why == "no next bar"), "{:?}", r.dropped);
    assert!(r.final_positions.is_empty(), "position stuck after an unpriced exit: {:?}", r.final_positions);
}

/// Four symbols, five consecutive weekdays, one fixed close per symbol.
fn cross_section(closes: &[(&str, f64)]) -> Dataset {
    let mut ds = Dataset::new();
    for d in ["2024-01-08", "2024-01-09", "2024-01-10", "2024-01-11", "2024-01-12"] {
        let t = day(d);
        for (name, p) in closes {
            let s = ds.intern(name);
            ds.add("close", vec![Value::Equity(s), Value::Time(t), Value::Num(*p)]);
            ds.add("volume", vec![Value::Equity(s), Value::Time(t), Value::Num(1000.0)]);
            ds.add("universe", vec![Value::Equity(s), Value::Time(t)]);
        }
    }
    ds
}

fn quantile_cut(q: f64) -> String {
    format!(
        r#"
strategy cut {{
  env equities_1d
  uses features
  resolution @1d
  mode target
  param q : Scalar = {}
  rel cut(@T: Timestamp, -H: Price<USD>, -M: Price<USD>)
  cut(T, H, M) :- bar(T), H = quantile(P, q) over (universe(A, T), close(A, T, P)),
      M = median(P1) over (universe(A1, T), close(A1, T, P1)).
  decide(T, target_weight(A, 0.1)) :- universe(A, T), close(A, T, P), cut(T, H, M), H = M, P > H.
}}
"#,
        q
    )
}

/// `quantile` interpolates linearly between order statistics and `median`
/// is the midpoint on an even count, so the two agree at 0.5; a level
/// outside [0, 1] halts the run naming the whole aggregate (portfolio-10).
#[test]
fn quantile_conventions_and_the_aggregate_halt() {
    let ds = cross_section(&[("A", 1.0), ("B", 2.0), ("C", 3.0), ("D", 4.0)]);
    let (prog, _) = program(&quantile_cut(0.5), "cut");
    let r = run(&prog, &ds, ExecConfig::default()).unwrap();
    // quantile(0.5) of [1, 2, 3, 4] is 2.5, as is the median: C and D are above it.
    let first: Vec<String> = r.decisions.iter().filter(|d| format_timestamp(d.t) == "2024-01-08").map(|d| r.describe_decision(&d.decision)).collect();
    assert_eq!(first, vec!["target_weight(C, 0.1)", "target_weight(D, 0.1)"]);

    let (prog, _) = program(&quantile_cut(1.5), "cut");
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
    let src = r#"
strategy n_up {
  env equities_1d
  uses features
  resolution @1d
  mode delta
  param qty : Quantity<Shares> = 10 shares
  rel above_ten(-A: Equity, @T: Timestamp, -N: Count)
  above_ten(A, T, N) :- universe(A, T),
      N = count(T1) over (T1 in window(T, 30d, min 1), close(A, T1, P), Cost = P * qty, Cost > 100 USD).
  decide(T, buy(A, qty)) :- above_ten(A, T, N), N = 3, flat(A, T).
}
"#;
    let (prog, _) = program(src, "n_up");
    // Closes above 10 within the window: 0, 1, 1, 2, 3 on the five days.
    let ds = crafted_daily(&[10.0, 11.0, 10.0, 12.0, 13.0]);
    let r = run(&prog, &ds, ExecConfig::default()).unwrap();
    let decisions: Vec<String> = r.decisions.iter().map(|d| format_timestamp(d.t)).collect();
    assert_eq!(decisions, vec!["2024-01-12"]);
}

/// Evaluation is top-down, so a degenerate tuple halts the run only when a
/// decision demands it: `1 / N` with N = 0 on the warm-up bars is never
/// requested, because nothing is selected there (adv-15).
#[test]
fn partial_arithmetic_is_only_evaluated_for_demanded_tuples() {
    let src = r#"
strategy unguarded {
  env equities_1d
  uses features
  resolution @1d
  mode target
  param n : Count = 2
  rel candidate(-A: Equity, @T: Timestamp, -M: Scalar)
  candidate(A, T, M) :- universe(A, T), momentum(A, T, 6mo, 1mo, M).
  rel selected(-A: Equity, @T: Timestamp)
  selected(A, T) :- bar(T), top(n, candidate(A, T, M), by (M desc, A asc)).
  rel n_selected(@T: Timestamp, -N: Count)
  n_selected(T, N) :- bar(T), N = count(A) over (selected(A, T)).
  rel w(@T: Timestamp, -W: Scalar)
  w(T, W) :- n_selected(T, N), W = 1 / N.
  decide(T, target_weight(A, W)) :- selected(A, T), w(T, W).
}
"#;
    let (prog, _) = program(src, "unguarded");
    let ds = synthetic_daily(&DAILY_SYMBOLS, (2022, 1, 3), 200, 3);
    let r = run(
        &prog,
        &ds,
        ExecConfig {
            on_leverage: OnLeverage::Allow,
            ..ExecConfig::frictionless()
        },
    )
    .unwrap();
    assert!(!r.decisions.is_empty());
    // Demanded at every bar, the same tuple halts on the first one.
    let demanded = src.replace(
        "decide(T, target_weight(A, W)) :- selected(A, T), w(T, W).",
        "decide(T, target_weight(A, W)) :- bar(T), w(T, W), selected(A, T).",
    );
    let (prog, _) = program(&demanded, "unguarded");
    match run(
        &prog,
        &ds,
        ExecConfig {
            on_leverage: OnLeverage::Allow,
            ..ExecConfig::frictionless()
        },
    ) {
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
    use absolute_backtest::data::business_days;
    let mut ds = Dataset::new();
    let y = ds.intern("Y");
    for (i, (d, &n)) in business_days((2024, 1, 8), bars_per_day.len()).into_iter().zip(bars_per_day).enumerate() {
        for b in 0..n {
            let t = d + 9 * 3600 + 31 * 60 + 60 * b as i64;
            let p = start_price + step * i as f64 + 0.01 * b as f64;
            ds.add("close_m", vec![Value::Equity(y), Value::Time(t), Value::Num(p)]);
            ds.add("volume_m", vec![Value::Equity(y), Value::Time(t), Value::Num(100.0)]);
            ds.add("universe_m", vec![Value::Equity(y), Value::Time(t)]);
        }
    }
    ds
}

/// `min K` removes a bar from a relation, not from the time domain: a
/// half-day with fewer than 300 minute bars has no `close_d`, yet it is a
/// @1d bar, `prev` lands on it, the executor fills there at its last minute
/// close whatever `min` the bars library asked for, and a rule that needs
/// `close_d` at prev(T) does not fire on the day after (intraday-06,
/// intraday-09).
#[test]
fn a_short_day_stays_in_the_domain_without_a_bar() {
    let src = r#"
strategy up_vs_prev {
  env equities_1m
  uses bars
  resolution @1d
  mode target
  rel up(-A: Equity, @T: Timestamp)
  up(A, T) :- universe_d(A, T), close_d(A, T, C), prev(T, T0), close_d(A, T0, C0), C > C0.
  decide(T, target_quantity(A, 1 shares)) :- up(A, T).
}
"#;
    let (prog, _) = program(src, "up_vs_prev");
    // Mon..Fri: full, full, half-day (150 bars), full, full.
    let ds = minute_days(&[300, 300, 150, 300, 300], 50.0, 1.0);
    let bars = Kernel::new(&prog, &ds, ExecConfig::default()).unwrap().decision_bars();
    let days: Vec<String> = bars.iter().map(|t| format_timestamp(*t)).collect();
    assert_eq!(days, vec!["2024-01-08", "2024-01-09", "2024-01-10", "2024-01-11", "2024-01-12"]);
    let r = run(&prog, &ds, ExecConfig::default()).unwrap();
    // Tuesday fires (Monday has a bar); Wednesday has no close_d; Thursday's
    // prev is Wednesday, so it does not fire either; Friday fires again.
    let decisions: Vec<String> = r.decisions.iter().map(|d| format_timestamp(d.t)).collect();
    assert_eq!(decisions, vec!["2024-01-09", "2024-01-12"]);
    // Tuesday's decision is filled on the half-day at its last minute close
    // (bar 149 of the third day: 52 + 1.49).
    assert_eq!(r.fills.len(), 1);
    assert_eq!(format_timestamp(r.fills[0].t), "2024-01-10");
    assert!((r.fills[0].price - 53.49).abs() < 1e-9, "{}", r.fills[0].price);
}

/// Timestamps are bar close instants: the synthetic minute market labels
/// the first bar of a session 09:31 and the 390th 16:00, so every @5m
/// bucket holds whole bars (intraday-07).
#[test]
fn synthetic_minute_bars_are_labelled_by_close_instant() {
    use absolute_backtest::kernel::time::bucket;
    use absolute_backtest::Resolution;
    let ds = synthetic_minute(&["AAA"], (2024, 1, 8), 1, 390, 1);
    let mut times: Vec<i64> = ds.facts["close_m"].iter().map(|tu| tu[1].as_time().unwrap()).collect();
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
    let src = r#"
strategy lagz {
  env equities_1d
  resolution @1d
  mode delta
  param d : Duration = 5d in 0d..30d
  param qty : Quantity<Shares> = 1 shares
  rel cnt(+A: Equity, @T: Timestamp, -N: Scalar)
  cnt(A, T, N) :- universe(A, T), lag(T, d, T1), cnt(A, T1, M), N = M + 1.
  cnt(A, T, N) :- universe(A, T), N = 0, lag(T, 400d, T1), not universe(A, T1).
  decide(T, buy(A, qty)) :- universe(A, T), cnt(A, T, N), N > 100.
}
"#;
    let (prog, _) = program(src, "lagz");
    let ds = synthetic_daily(&["AAA"], (2023, 1, 2), 40, 3);
    let fine = ExecConfig {
        param_overrides: vec![("d".to_string(), Lit::Duration(Duration { months: 0, days: 7 }))],
        ..ExecConfig::frictionless()
    };
    run(&prog, &ds, fine).unwrap();
    let flipped = ExecConfig {
        param_overrides: vec![("d".to_string(), Lit::Duration(Duration { months: 0, days: 0 }))],
        ..ExecConfig::frictionless()
    };
    match run(&prog, &ds, flipped) {
        Err(RunError::Request(m)) => assert!(m.contains("lag") && m.contains("zero") && !m.contains("internal"), "{}", m),
        other => panic!("expected the override to be refused, got {:?}", other.map(|r| r.decisions.len())),
    }
}
