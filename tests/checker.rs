//! Checker judgments pinned by the sweep: duplicate declarations, reserved
//! names, workspace-level name clashes, decided patterns, parameter ranges,
//! undefined relations, resample grouping and timestamp values.

mod corpus;

use absolute_backtest::check::{check_program, check_workspace, Code, Diagnostic, Severity};

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
