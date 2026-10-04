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
use absolute_backtest::kernel::{run, verify_causality, Ctor, Dataset, ExecConfig, Kernel, RunError, Value};

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
            ..Default::default()
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
            ..Default::default()
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
            ..Default::default()
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
            ..Default::default()
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
        assert!(a.dropped.is_empty(), "{} dropped decisions: {:?}", name, a.dropped);
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
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(r.dropped.len(), 1, "{:?}", r.dropped);
    assert_eq!(format_timestamp(r.dropped[0].0), "2024-01-09");
    // Thursday's log return needs Wednesday's close too, so the next buy is
    // Friday's, which has no bar left to fill it: no fills at all.
    let decisions: Vec<String> = r.decisions.iter().map(|d| format_timestamp(d.t)).collect();
    assert_eq!(decisions, vec!["2024-01-09", "2024-01-12"]);
    assert!(r.fills.is_empty(), "{:?}", r.fills);
    assert_eq!(r.final_cash, 1000.0);
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

/// A dropped fill of a time-based exit does not leave the position stuck:
/// the exit is a state test, so it is decided again at the next bar
/// (trend-02).
#[test]
fn corpus_time_exit_is_retried_after_a_dropped_fill() {
    let src = fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("corpus/strategies/volume_spike.dsl")).unwrap();
    let (prog, _) = program(&src, "volume_spike");
    // A Wednesday entry with hold 5d: the exit is due on the Monday after
    // next or the day after. Both bars that could fill it lack a close, so
    // the first exit decision is dropped whichever bar it lands on.
    let ds = spike_daily("2024-01-31", &["2024-02-06", "2024-02-07"]);
    let r = run(&prog, &ds, ExecConfig::default()).unwrap();
    let decisions: Vec<String> = r.decisions.iter().map(|d| format!("{} {}", format_timestamp(d.t), r.describe_decision(&d.decision))).collect();
    assert_eq!(decisions[0], "2024-01-31 buy(X, 100)", "{:?}", decisions);
    assert_eq!(r.dropped.len(), 1, "{:?}", r.dropped);
    let sells = decisions.iter().filter(|d| d.contains("sell")).count();
    assert_eq!(sells, 2, "the exit must be decided again after the drop: {:?}", decisions);
    assert_eq!(r.fills.len(), 2, "{:?}", r.fills);
    assert!(r.final_positions.is_empty(), "position stuck after a dropped exit: {:?}", r.final_positions);
}
