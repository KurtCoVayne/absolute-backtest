//! Checker judgments pinned by the sweep: duplicate declarations, reserved
//! names, workspace-level name clashes, decided patterns, parameter ranges,
//! undefined relations, resample grouping and timestamp values.

mod corpus;

use absolute_backtest::check::{check_library, check_program, check_workspace, Code, Diagnostic, Severity};
use absolute_backtest::data::{synthetic_daily, synthetic_minute};
use absolute_backtest::kernel::time::format_timestamp;
use absolute_backtest::kernel::{run, ExecConfig};

fn check(src: &str, name: &str) -> Vec<Diagnostic> {
    let mut ws = corpus::base_workspace();
    ws.add_source(src).unwrap();
    check_program(&ws, name).1
}

fn errors(diags: &[Diagnostic]) -> Vec<&Diagnostic> {
    diags.iter().filter(|d| d.severity == Severity::Error).collect()
}

fn text(diags: &[Diagnostic]) -> String {
    diags.iter().map(|d| d.to_string()).collect::<Vec<_>>().join("\n")
}

fn assert_only(diags: &[Diagnostic], code: Code) {
    let errs = errors(diags);
    assert!(!errs.is_empty(), "expected a {:?} error, checked clean:\n{}", code, text(diags));
    assert!(errs.iter().all(|d| d.code == code), "expected only {:?} errors, got:\n{}", code, text(diags));
}

// adv-01: a second `resolution` or `env` line is an error, whatever the order.

#[test]
fn a_second_resolution_declaration_is_an_x_error() {
    let src = r#"
strategy two_res {
  env equities_1d
  uses features
  resolution @1d
  resolution @1m
  mode delta
  param qty : Quantity<Shares> = 100 shares
  decide(T, buy(A, qty)) :- universe(A, T), flat(A, T).
}
"#;
    let diags = check(src, "two_res");
    assert_only(&diags, Code::X);
    assert!(diags.iter().any(|d| d.code == Code::X && d.span.line == 6), "the second line is the offending one:\n{}", text(&diags));
}

#[test]
fn a_second_env_declaration_is_a_u_error() {
    let src = r#"
strategy two_env {
  env equities_1d
  env equities_1m
  uses features
  resolution @1d
  mode delta
  param qty : Quantity<Shares> = 100 shares
  decide(T, buy(A, qty)) :- universe(A, T), flat(A, T).
}
"#;
    let diags = check(src, "two_env");
    assert_only(&diags, Code::U);
}

// adv-02: two units with one (kind, name) in the workspace.

const IMPOSTOR: &str = r#"
strategy sma_crossover {
  env equities_1d
  uses features
  resolution @1d
  mode delta
  decide(T, buy(A, 100 shares)) :- universe(A, T), flat(A, T).
}
"#;

#[test]
fn a_strategy_declared_twice_in_the_workspace_is_a_u_error_naming_both() {
    let mut ws = corpus::base_workspace();
    let real = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/corpus/strategies/sma_crossover.dsl")).unwrap();
    ws.add_source(&real).unwrap();
    ws.add_source(IMPOSTOR).unwrap();
    let (program, diags) = check_program(&ws, "sma_crossover");
    assert!(program.is_none(), "an ambiguous strategy must not produce a program");
    assert_only(&diags, Code::U);
    let d = errors(&diags)[0];
    assert!(d.message.contains("twice"), "{}", d);
    // Both declarations are named: the corpus one and the impostor at line 2.
    assert!(d.message.contains("2:1"), "both spans should be named: {}", d);
    // Checking the whole workspace reports the clash once, not once per copy.
    let all = check_workspace(&ws);
    assert_eq!(errors(&all).iter().filter(|d| d.code == Code::U && d.message.contains("twice")).count(), 1, "{}", text(&all));
}

#[test]
fn a_library_declared_twice_in_the_workspace_is_a_u_error() {
    let mut ws = corpus::base_workspace();
    ws.add_source("library features {\n  env equities_1d\n  resolution @1d\n}\n").unwrap();
    let src = r#"
strategy uses_dup {
  env equities_1d
  uses features
  resolution @1d
  mode delta
  param qty : Quantity<Shares> = 100 shares
  decide(T, buy(A, qty)) :- universe(A, T), flat(A, T).
}
"#;
    ws.add_source(src).unwrap();
    let (_, diags) = check_program(&ws, "uses_dup");
    assert_only(&diags, Code::U);
    assert!(errors(&diags).iter().any(|d| d.message.contains("features") && d.message.contains("twice")), "{}", text(&diags));
}

// adv-16: a relation may not take a builtin's or a keyword's name.

#[test]
fn a_relation_named_after_a_keyword_is_a_u_error() {
    // (A unit keyword such as `mode` is already a parse error in head position.)
    for name in ["lag", "top", "resample", "window", "buy", "sum"] {
        let src = format!(
            r#"
strategy kw {{
  env equities_1d
  uses features
  resolution @1d
  mode delta
  param qty : Quantity<Shares> = 100 shares
  rel {name}(-A: Equity, @T: Timestamp)
  {name}(A, T) :- universe(A, T).
  decide(T, buy(A, qty)) :- universe(A, T), flat(A, T).
}}
"#
        );
        let diags = check(&src, "kw");
        assert_only(&diags, Code::U);
        assert!(errors(&diags).iter().any(|d| d.message.contains(name)), "{}: {}", name, text(&diags));
    }
}

// adv-13: a parameter's range must be ordered and contain the default.

#[test]
fn an_inverted_parameter_range_is_a_t_error() {
    let src = r#"
strategy inverted {
  env equities_1d
  uses features
  resolution @1d
  mode delta
  param qty : Quantity<Shares> = 100 shares
  param n : Count = 3 in 10..1
  rel pick(-A: Equity, @T: Timestamp)
  pick(A, T) :- universe(A, T), top(n, momentum(A, T, 3mo, 0d, M), by (M desc, A asc)).
  decide(T, buy(A, qty)) :- pick(A, T), flat(A, T).
}
"#;
    let diags = check(src, "inverted");
    assert_only(&diags, Code::T);
    assert!(errors(&diags).iter().any(|d| d.message.contains("`n`")), "{}", text(&diags));
}

#[test]
fn a_default_on_the_range_bound_is_accepted() {
    let src = r#"
strategy on_bound {
  env equities_1d
  uses features
  resolution @1d
  mode delta
  param qty : Quantity<Shares> = 100 shares
  param lb : Duration = 1mo in 1mo..1y
  param k : Count = 1 in 1..50
  param z : Scalar = -1.0 in -4.0..-1.0
  rel m(-A: Equity, @T: Timestamp, -M: Price<USD>)
  m(A, T, M) :- universe(A, T), M = mean(P) over (T1 in window(T, lb, min k), close(A, T1, P)), M > 0 USD / 1 shares, z < 0.
  decide(T, buy(A, qty)) :- m(A, T, _), flat(A, T).
}
"#;
    let diags = check(src, "on_bound");
    assert!(errors(&diags).is_empty(), "{}", text(&diags));
}

// adv-12: a declared relation that no rule defines is empty for ever, so
// every rule reading it positively is dead; that deserves a warning.

#[test]
fn a_declared_relation_with_no_rules_is_warned_about() {
    let src = r#"
strategy ghost_rel {
  env equities_1d
  uses features
  resolution @1d
  mode delta
  param qty : Quantity<Shares> = 100 shares
  rel ghost(-A: Equity, @T: Timestamp)
  decide(T, buy(A, qty)) :- universe(A, T), ghost(A, T), flat(A, T).
}
"#;
    let diags = check(src, "ghost_rel");
    assert!(errors(&diags).is_empty(), "{}", text(&diags));
    let w: Vec<&Diagnostic> = diags.iter().filter(|d| d.severity == Severity::Warning).collect();
    assert_eq!(w.len(), 1, "exactly one warning, for `ghost`:\n{}", text(&diags));
    assert!(w[0].message.contains("`ghost`") && w[0].message.contains("no rule"), "{}", w[0]);
    assert_eq!(w[0].span.line, 8, "reported at the declaration: {}", w[0]);
}

fn program(src: &str, name: &str) -> absolute_backtest::check::Program {
    let mut ws = corpus::base_workspace();
    ws.add_source(src).unwrap();
    let (p, diags) = check_program(&ws, name);
    p.unwrap_or_else(|| panic!("{} should check clean:\n{}", name, text(&diags)))
}

// trend-11: a bound Timestamp may be copied into a value column with `X = T`
// (section 2: non-key timestamps are ordinary values).

#[test]
fn a_bound_timestamp_may_be_assigned_as_a_value() {
    let src = r#"
strategy held_since {
  env equities_1d
  uses features
  resolution @1d
  mode delta
  param qty : Quantity<Shares> = 100 shares
  rel entry(+A: Equity, @T: Timestamp, -E: Price<USD>, -TE: Timestamp)
  entry(A, T, E, TE) :- fill(A, T, Q, P), Q > 0 shares, E = P, TE = T.
  entry(A, T, E, TE) :- position(A, T, Q), Q > 0 shares, not fill(A, T, _, _), prev(T, T0), entry(A, T0, E, TE).
  decide(T, buy(A, qty)) :- universe(A, T), flat(A, T).
  decide(T, sell(A, Q)) :- held(A, T, Q), entry(A, T, _, TE), lag(T, 30d, T0), TE <= T0.
}
"#;
    let prog = program(src, "held_since");
    let ds = synthetic_daily(&["AAA", "BBB"], (2023, 1, 2), 120, 3);
    let r = run(&prog, &ds, ExecConfig::default()).unwrap();
    let sells: Vec<_> = r.decisions.iter().filter(|d| r.describe_decision(&d.decision).starts_with("sell")).collect();
    assert!(!sells.is_empty(), "the time-based exit should fire");
    // The first buy is decided on bar 1 and filled on bar 2 (the entry time);
    // the first sell comes once a bar 30 calendar days before T is at or
    // after that entry, so at least 30 days after the fill.
    let first_fill = r.fills[0].t;
    assert!(
        sells[0].t - first_fill >= 30 * 86_400,
        "first sell {} vs first fill {}",
        format_timestamp(sells[0].t),
        format_timestamp(first_fill)
    );
}

// intraday-03: a resample over a stored relation binds its entity variables
// by grouping, even in `+` positions (section 4); a derived inner relation
// with a fresh `+` input stays an M error that says why.

const FRESH_INPUT_RESAMPLE: &str = r#"
strategy five_minute_bars {
  env equities_1m
  resolution @5m
  mode delta
  param qty : Quantity<Shares> = 100 shares
  rel c5(-A: Equity, @T: Timestamp, -C: Price<USD>)
  c5(A, T, C) :- resample(close_m(A, T1, P) to @5m as T, min 5, C = last(P)).
  decide(T, buy(A, qty)) :- c5(A, T, _), not position(A, T, _).
}
"#;

#[test]
fn a_resample_over_a_primitive_groups_a_fresh_input_entity() {
    let prog = program(FRESH_INPUT_RESAMPLE, "five_minute_bars");
    let ds = synthetic_minute(&["AAA", "BBB"], (2024, 1, 2), 1, 30, 1);
    let r = run(&prog, &ds, ExecConfig::default()).unwrap();
    // One bucket per symbol: both are bought at the first five-minute bar and
    // then held, so exactly two decisions, at one bar, for two instruments.
    assert_eq!(
        r.decisions.len(),
        2,
        "{:?}",
        r.decisions.iter().map(|d| (format_timestamp(d.t), r.describe_decision(&d.decision))).collect::<Vec<_>>()
    );
    assert_eq!(r.decisions[0].t, r.decisions[1].t);
    let names: std::collections::BTreeSet<String> = r.decisions.iter().map(|d| r.describe_decision(&d.decision)).collect();
    assert_eq!(names.len(), 2);
}

#[test]
fn a_resample_over_a_derived_relation_with_a_fresh_input_is_an_m_error() {
    let src = r#"
strategy derived_inner {
  env equities_1m
  resolution @5m
  mode delta
  param qty : Quantity<Shares> = 100 shares
  rel up_m(+A: Equity, @T: Timestamp) @1m
  up_m(A, T) :- universe_m(A, T), close_m(A, T, P), prev(T, T0), close_m(A, T0, P0), P > P0.
  rel ups5(-A: Equity, @T: Timestamp, -N: Count)
  ups5(A, T, N) :- resample(up_m(A, T1) to @5m as T, min 1, N = count(T1)).
  decide(T, buy(A, qty)) :- ups5(A, T, N), N > 3.
}
"#;
    let diags = check(src, "derived_inner");
    assert_only(&diags, Code::M);
    let d = errors(&diags)[0];
    assert!(d.message.contains("up_m") && d.message.contains("derived"), "{}", d);
}

// Review of #43: a library has no mode, so a `decided` pattern written in it
// is judged by each strategy that reaches the rule (WF-9), at the library's
// span, naming the strategy.

const WAS_BUY_LIB: &str = r#"
library recall {
  env equities_1d
  resolution @1d
  rel was_buy(-A: Equity, @T: Timestamp)
  was_buy(A, T) :- universe(A, T), prev(T, T0), decided(T0, buy(A, _)).
  rel calm(-A: Equity, @T: Timestamp)
  calm(A, T) :- universe(A, T).
}
"#;

fn check_with_lib(src: &str, name: &str) -> Vec<Diagnostic> {
    let mut ws = corpus::base_workspace();
    ws.add_source(WAS_BUY_LIB).unwrap();
    ws.add_source(src).unwrap();
    check_program(&ws, name).1
}

#[test]
fn a_library_decided_pattern_of_the_other_mode_is_a_c_error_in_the_strategy_that_reaches_it() {
    let src = r#"
strategy target_recall {
  env equities_1d
  uses recall
  resolution @1d
  mode target
  decide(T, target_weight(A, 0.5)) :- was_buy(A, T).
}
"#;
    let diags = check_with_lib(src, "target_recall");
    assert_only(&diags, Code::C);
    let e = errors(&diags);
    assert_eq!(e.len(), 1, "{}", text(&diags));
    assert_eq!(e[0].unit, "recall", "reported at the library: {}", e[0]);
    assert_eq!(e[0].span.line, 6, "reported at the pattern: {}", e[0]);
    assert!(e[0].message.contains("target_recall") && e[0].message.contains("buy"), "{}", e[0]);
}

#[test]
fn a_library_decided_pattern_is_fine_for_a_strategy_of_that_mode_or_one_that_does_not_reach_it() {
    let delta = r#"
strategy delta_recall {
  env equities_1d
  uses recall
  resolution @1d
  mode delta
  param qty : Quantity<Shares> = 100 shares
  decide(T, sell(A, qty)) :- was_buy(A, T).
}
"#;
    let diags = check_with_lib(delta, "delta_recall");
    assert!(errors(&diags).is_empty(), "{}", text(&diags));
    let unreached = r#"
strategy target_calm {
  env equities_1d
  uses recall
  resolution @1d
  mode target
  decide(T, target_weight(A, 0.5)) :- calm(A, T).
}
"#;
    let diags = check_with_lib(unreached, "target_calm");
    assert!(errors(&diags).is_empty(), "{}", text(&diags));
}

// Review of #43: a calendar duration has no single length in days, so a
// default is outside its range only when it is outside for every length a
// month or a year can take.

#[test]
fn a_calendar_duration_default_is_rejected_only_when_no_month_length_fits() {
    let strategy = |range: &str| {
        format!(
            r#"
strategy cal {{
  env equities_1d
  uses features
  resolution @1d
  mode delta
  param qty : Quantity<Shares> = 100 shares
  param lb : Duration = 1mo in {range}
  rel m(-A: Equity, @T: Timestamp, -M: Price<USD>)
  m(A, T, M) :- universe(A, T), M = mean(P) over (T1 in window(T, lb, min 1), close(A, T1, P)).
  decide(T, buy(A, qty)) :- m(A, T, _), flat(A, T).
}}
"#
        )
    };
    for ok in ["31d..60d", "10d..28d", "4w..5w", "1mo..1mo", "20d..1y"] {
        let diags = check(&strategy(ok), "cal");
        assert!(errors(&diags).is_empty(), "`1mo in {}` should be accepted:\n{}", ok, text(&diags));
    }
    for bad in ["32d..60d", "10d..27d", "2mo..1y"] {
        let diags = check(&strategy(bad), "cal");
        assert_only(&diags, Code::T);
        assert!(errors(&diags).iter().any(|d| d.message.contains("`lb`")), "`1mo in {}`: {}", bad, text(&diags));
    }
}

// Review of #43 (non-blocking): the W3 and reserved-name judgments apply to
// a library checked on its own as well.

fn check_library_src(src: &str, name: &str) -> Vec<Diagnostic> {
    let mut ws = corpus::base_workspace();
    ws.add_source(src).unwrap();
    check_library(&ws, name)
}

#[test]
fn a_library_declared_relation_with_no_rules_is_warned_about() {
    let src = "library ghost_lib {\n  env equities_1d\n  resolution @1d\n  rel ghost(-A: Equity, @T: Timestamp)\n}\n";
    let diags = check_library_src(src, "ghost_lib");
    assert!(errors(&diags).is_empty(), "{}", text(&diags));
    let w: Vec<&Diagnostic> = diags.iter().filter(|d| d.severity == Severity::Warning).collect();
    assert_eq!(w.len(), 1, "exactly one warning, for `ghost`:\n{}", text(&diags));
    assert!(w[0].code == Code::W3 && w[0].message.contains("`ghost`"), "{}", w[0]);
}

#[test]
fn a_library_relation_named_after_a_builtin_is_a_u_error() {
    let src = "library kw_lib {\n  env equities_1d\n  resolution @1d\n  rel lag(-A: Equity, @T: Timestamp)\n  lag(A, T) :- universe(A, T).\n}\n";
    let diags = check_library_src(src, "kw_lib");
    assert_only(&diags, Code::U);
    assert!(errors(&diags).iter().any(|d| d.message.contains("`lag`")), "{}", text(&diags));
}
