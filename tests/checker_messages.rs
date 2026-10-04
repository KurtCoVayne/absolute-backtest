//! The checker's diagnostics pinned exactly: count, codes and wording. One
//! root cause yields one diagnostic (no cascades), a library's diagnostics
//! appear once per workspace check, and messages name the thing to fix.

mod corpus;

use absolute_backtest::check::{check_workspace, Code, Diagnostic, Severity, Workspace};

fn errors(diags: &[Diagnostic]) -> Vec<&Diagnostic> {
    diags.iter().filter(|d| d.severity == Severity::Error).collect()
}

fn text(diags: &[Diagnostic]) -> String {
    diags.iter().map(|d| d.to_string()).collect::<Vec<_>>().join("\n")
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
