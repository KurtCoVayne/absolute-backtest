//! The checker's diagnostics pinned exactly: count, codes and wording. One
//! root cause yields one diagnostic (no cascades), a library's diagnostics
//! appear once per workspace check, and messages name the thing to fix.

mod corpus;

use absolute_backtest::check::{check_program, check_workspace, Code, Diagnostic, Severity, Workspace};

fn errors(diags: &[Diagnostic]) -> Vec<&Diagnostic> {
    diags.iter().filter(|d| d.severity == Severity::Error).collect()
}

fn text(diags: &[Diagnostic]) -> String {
    diags.iter().map(|d| d.to_string()).collect::<Vec<_>>().join("\n")
}

fn check(src: &str, strategy: &str) -> Vec<Diagnostic> {
    let mut ws = corpus::base_workspace();
    ws.add_source(src).unwrap();
    check_program(&ws, strategy).1
}

/// adv-05: a library written against another environment is one unit-level
/// E error, not one E per primitive plus an F cascade through every rule.
#[test]
fn a_library_from_another_environment_is_one_error() {
    let diags = check(
        r#"
strategy b4_08 {
  env equities_1m
  uses features
  resolution @1d
  mode target
  decide(T, target_weight(A, 0.5)) :- universe(A, T), momentum(A, T, 3mo, 0d, M), M > 0.1.
}
"#,
        "b4_08",
    );
    let errs = errors(&diags);
    assert_eq!(errs.len(), 1, "got:\n{}", text(&diags));
    assert_eq!(errs[0].code, Code::E);
    assert_eq!(errs[0].rule, None);
    for needle in ["`features`", "`equities_1d`", "`equities_1m`"] {
        assert!(errs[0].message.contains(needle), "{}", errs[0]);
    }
}

/// adv-05: an environment missing from the workspace is reported once, first,
/// and never as an empty name; the rules that depend on it are not judged.
#[test]
fn a_missing_environment_is_one_error_reported_first() {
    let diags = check(
        r#"
strategy p08 {
  env nosuchenv
  resolution @1d
  mode delta
  decide(T, buy(A, 100 shares)) :- universe(A, T), not position(A, T, _).
}
"#,
        "p08",
    );
    let errs = errors(&diags);
    assert_eq!(errs.len(), 1, "got:\n{}", text(&diags));
    assert_eq!(errs[0].code, Code::U);
    assert!(errs[0].message.contains("environment `nosuchenv` is not in the workspace"), "{}", errs[0]);
    assert!(!text(&diags).contains("``"), "empty name printed:\n{}", text(&diags));
}

/// adv-05: a strategy checked without its environment and library files gets
/// one U per missing unit and nothing per relation.
#[test]
fn a_strategy_alone_reports_only_the_missing_units() {
    let src = std::fs::read_to_string(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("corpus/strategies/sma_crossover.dsl")).unwrap();
    let mut ws = Workspace::new();
    ws.add_source(&src).unwrap();
    let diags = check_program(&ws, "sma_crossover").1;
    let errs = errors(&diags);
    assert_eq!(errs.len(), 2, "got:\n{}", text(&diags));
    assert!(errs.iter().all(|d| d.code == Code::U && d.rule.is_none()), "got:\n{}", text(&diags));
    assert!(text(&diags).contains("environment `equities_1d` is not in the workspace"), "{}", text(&diags));
    assert!(text(&diags).contains("library `features` is not in the workspace"), "{}", text(&diags));
    assert!(!text(&diags).contains("``"), "empty name printed:\n{}", text(&diags));
}

/// adv-05: an undeclared relation that binds the head time is one U error;
/// the atoms after it are not judged for causality against a time the
/// checker could not place.
#[test]
fn an_unresolved_relation_does_not_cascade_into_causality_errors() {
    let diags = check(
        r#"
strategy undeclared_first {
  env equities_1d
  uses features
  resolution @1d
  mode delta
  param qty : Quantity<Shares> = 100 shares
  decide(T, buy(A, qty)) :- rsi(A, T, 14d, R), R < 30, flat(A, T).
}
"#,
        "undeclared_first",
    );
    let errs = errors(&diags);
    assert_eq!(errs.len(), 1, "got:\n{}", text(&diags));
    assert_eq!(errs[0].code, Code::U);
    assert!(errs[0].message.contains("`rsi`"), "{}", errs[0]);
}

/// adv-07: when the head time is itself bound non-causally, the atoms keyed
/// by it are not each told that `T` is not derived from `T`.
#[test]
fn a_non_causal_head_time_is_not_reported_per_atom() {
    let diags = check(
        r#"
strategy la11 {
  env equities_1d
  uses features
  resolution @1d
  mode delta
  param qty : Quantity<Shares> = 100 shares
  decide(T, buy(A, qty)) :- held(A, T1, _), prev(T1, T), flat(A, T).
}
"#,
        "la11",
    );
    let errs = errors(&diags);
    assert_eq!(errs.len(), 2, "got:\n{}", text(&diags));
    assert!(errs.iter().all(|d| d.code == Code::F), "got:\n{}", text(&diags));
    assert!(errs.iter().any(|d| d.message.contains("`T1` of `held`")), "the real lookahead is named:\n{}", text(&diags));
    assert!(errs.iter().any(|d| d.message.contains("head temporal key `T`")), "the head's time is named:\n{}", text(&diags));
    assert!(!text(&diags).contains("of `flat`"), "no follow-on error on flat(A, T):\n{}", text(&diags));
}

/// adv-06: a library in the workspace is checked once standalone and once per
/// using strategy; its diagnostics must still be reported once.
#[test]
fn a_failing_library_used_by_a_strategy_is_reported_once() {
    let src = r#"
library histlib {
  env equities_1d
  resolution @1d
  rel bought_now(+A: Equity, @T: Timestamp)
  bought_now(A, T) :- universe(A, T), decided(T, buy(A, _)).
}
strategy la09 {
  env equities_1d
  uses histlib
  resolution @1d
  mode delta
  param qty : Quantity<Shares> = 100 shares
  decide(T, buy(A, qty)) :- universe(A, T), not position(A, T, _), not bought_now(A, T).
}
"#;
    let mut ws = corpus::base_workspace();
    ws.add_source(src).unwrap();
    let diags = check_workspace(&ws);
    let errs = errors(&diags);
    assert_eq!(errs.len(), 1, "one F error on histlib::bought_now#1, got:\n{}", text(&diags));
    assert_eq!(errs[0].code, Code::F);
    assert_eq!(errs[0].unit, "histlib");
    assert_eq!(errs[0].rule.as_deref(), Some("histlib::bought_now#1"));
}

/// The dedupe keeps distinct diagnostics on the same span: two strategies at
/// different resolutions using one library give that library two different
/// messages.
#[test]
fn distinct_library_diagnostics_are_both_kept() {
    let mut ws = Workspace::new();
    for f in ["env/equities_1m.dsl", "lib/features_m.dsl"] {
        let p = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("corpus").join(f);
        ws.add_source(&std::fs::read_to_string(p).unwrap()).unwrap();
    }
    ws.add_source(
        r#"
strategy at_5m {
  env equities_1m
  uses features_m
  resolution @5m
  mode delta
  param qty : Quantity<Shares> = 100 shares
  rel u5(-A: Equity, @T: Timestamp)
  u5(A, T) :- resample(universe_m(A, T1) to @5m as T, min 1, N = count(T1)).
  decide(T, buy(A, qty)) :- u5(A, T), flat_m(A, T).
}
strategy at_15m {
  env equities_1m
  uses features_m
  resolution @15m
  mode delta
  param qty : Quantity<Shares> = 100 shares
  rel u15(-A: Equity, @T: Timestamp)
  u15(A, T) :- resample(universe_m(A, T1) to @15m as T, min 1, N = count(T1)).
  decide(T, buy(A, qty)) :- u15(A, T), flat_m(A, T).
}
"#,
    )
    .unwrap();
    let diags = check_workspace(&ws);
    let on_flat_m: Vec<&Diagnostic> = diags.iter().filter(|d| d.rule.as_deref() == Some("features_m::flat_m#1")).collect();
    assert_eq!(on_flat_m.len(), 2, "one X per using strategy, got:\n{}", text(&diags));
    assert_ne!(on_flat_m[0].message, on_flat_m[1].message);
}
