//! Checker judgments pinned by the sweep: duplicate blocks and declarations,
//! reserved names, workspace-level name clashes, parameter ranges, undefined
//! relations, degenerate reductions, degrees of freedom, ticker snapshots
//! and timestamp values.

mod common;

use absolute_backtest::check::{check_program, check_workspace, Code, Diagnostic, Program, Severity};
use absolute_backtest::data::synthetic_daily;
use absolute_backtest::kernel::time::format_timestamp;
use absolute_backtest::kernel::{run, ExecConfig};

fn check(src: &str, name: &str) -> Vec<Diagnostic> {
    common::check(src, name).1
}

fn program(src: &str, name: &str) -> Program {
    common::program(src, name)
}

fn errors(diags: &[Diagnostic]) -> Vec<&Diagnostic> {
    diags.iter().filter(|d| d.severity == Severity::Error).collect()
}

fn text(diags: &[Diagnostic]) -> String {
    common::text(diags)
}

fn assert_only(diags: &[Diagnostic], code: Code) {
    let errs = errors(diags);
    assert!(!errs.is_empty(), "expected a {:?} error, checked clean:\n{}", code, text(diags));
    assert!(errs.iter().all(|d| d.code == code), "expected only {:?} errors, got:\n{}", code, text(diags));
}

/// A library source added to the base workspace before the strategy.
fn check_with_lib(lib: &str, src: &str, name: &str) -> Vec<Diagnostic> {
    let mut ws = common::base_workspace();
    ws.add_source(lib).unwrap_or_else(|e| panic!("{}", e));
    ws.add_source(src).unwrap_or_else(|e| panic!("{}", e));
    check_program(&ws, name).1
}

// adv-01: a block written twice is a P error (WF-14), whatever the order;
// the second is the offending one.

#[test]
fn a_block_written_twice_is_a_p_error() {
    let src = r#"
strategy two_clocks {
  data { class equity   need bars @1d   need listing   require tested   prefer latest }
  uses calendar, book
  clock { decide on base }
  universe { universe(A, T) :- listed(A, T), traded@base(A, T). }
  clock { decide on base }
  params { qty : Quantity<Shares> = 100 shares }
  rules { }
  decisions {
    silence hold
    orders market
    decide(T, trade(A, qty)) :- universe(A, T), flat(A, T).
  }
  execution equity_default
  report { calendar base   periods 252 per year }
}
"#;
    let diags = check(src, "two_clocks");
    assert_only(&diags, Code::P);
    let d = errors(&diags)[0];
    assert!(d.message.contains("`clock` block is written twice"), "{}", d);
    assert_eq!(d.span.line, 7, "the second block is the offending one: {}", d);
}

// adv-01: a declaration written twice is a U error: a parameter, a relation.

#[test]
fn a_declaration_written_twice_is_a_u_error() {
    let src = common::strategy(
        "two_params",
        "    qty : Quantity<Shares> = 100 shares\n    qty : Quantity<Shares> = 10 shares",
        "",
        "    decide(T, trade(A, qty)) :- universe(A, T), flat(A, T).",
    );
    let diags = check(&src, "two_params");
    assert_only(&diags, Code::U);
    assert!(errors(&diags).iter().any(|d| d.message.contains("parameter `qty` is declared twice")), "{}", text(&diags));

    let src = common::strategy(
        "two_rels",
        "    qty : Quantity<Shares> = 100 shares",
        "    rel up(A: Equity, @T)\n    rel up(A: Equity, @T)\n    up(A, T) :- universe(A, T), price_return(A, T, R), R > 0.",
        "    decide(T, trade(A, qty)) :- up(A, T), flat(A, T).",
    );
    let diags = check(&src, "two_rels");
    assert_only(&diags, Code::U);
    assert!(errors(&diags).iter().any(|d| d.message.contains("relation `up` is declared twice")), "{}", text(&diags));
}

// adv-02: two units with one (kind, name) in the workspace.

const IMPOSTOR: &str = r#"
strategy sma_crossover {
  data { class equity   need bars @1d   need listing   require tested   prefer latest }
  uses calendar, book
  clock { decide on base }
  universe { universe(A, T) :- listed(A, T), traded@base(A, T). }
  params { qty : Quantity<Shares> = 100 shares }
  rules { }
  decisions {
    silence hold
    orders market
    decide(T, trade(A, qty)) :- universe(A, T), flat(A, T).
  }
  execution equity_default
  report { calendar base   periods 252 per year }
}
"#;

#[test]
fn a_strategy_declared_twice_in_the_workspace_is_a_u_error_naming_both() {
    let mut ws = common::base_workspace();
    ws.add_source(&common::corpus_source("strategies", "sma_crossover")).unwrap();
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
#[ignore = "src: a library declared twice in the workspace is not reported; the first declaration silently wins"]
fn a_library_declared_twice_in_the_workspace_is_a_u_error() {
    let dup = "library calendar {\n  data { class any }\n  rules { }\n}\n";
    let src = common::strategy("uses_dup", "    qty : Quantity<Shares> = 100 shares", "", "    decide(T, trade(A, qty)) :- universe(A, T), flat(A, T).");
    let diags = check_with_lib(dup, &src, "uses_dup");
    assert_only(&diags, Code::U);
    assert!(errors(&diags).iter().any(|d| d.message.contains("calendar") && d.message.contains("twice")), "{}", text(&diags));
}

// adv-16: a relation may not take a builtin's or a keyword's name.

#[test]
fn a_relation_named_after_a_keyword_is_a_u_error() {
    for name in ["lag", "top", "window", "rows", "trade", "sum"] {
        let src = common::strategy(
            "kw",
            "    qty : Quantity<Shares> = 100 shares",
            &format!("    rel {name}(A: Equity, @T)\n    {name}(A, T) :- universe(A, T)."),
            "    decide(T, trade(A, qty)) :- universe(A, T), flat(A, T).",
        );
        let diags = check(&src, "kw");
        assert_only(&diags, Code::U);
        assert!(errors(&diags).iter().any(|d| d.message.contains(&format!("`{}`", name))), "{}: {}", name, text(&diags));
    }
}

// adv-13: a parameter's range must be ordered and contain the default.

#[test]
fn an_inverted_parameter_range_is_a_t_error() {
    let src = common::strategy(
        "inverted",
        "    qty : Quantity<Shares> = 100 shares\n    n : Count = 3 in 10..1",
        "    rel m(A: Equity, @T, M: Price<USD>)\n    m(A, T, M) :- universe(A, T), M = mean(P) over (T1 in rows[n], close(A, T1, P)).",
        "    decide(T, trade(A, qty)) :- m(A, T, _), flat(A, T).",
    );
    let diags = check(&src, "inverted");
    assert_only(&diags, Code::T);
    assert!(errors(&diags).iter().any(|d| d.message.contains("`n`")), "{}", text(&diags));
}

#[test]
fn a_default_on_the_range_bound_is_accepted() {
    let src = common::strategy(
        "on_bound",
        "    qty : Quantity<Shares> = 100 shares\n    lb : Duration = 1mo in 1mo..1y\n    k : Count = 1 in 1..50\n    z : Scalar = -1.0 in -4.0..-1.0",
        "    rel m(A: Equity, @T, M: Price<USD>)\n    m(A, T, M) :- universe(A, T), M = mean(P) over (T1 in window[lb] min k, close(A, T1, P)), M > 0 USD/share, z < 0.",
        "    decide(T, trade(A, qty)) :- m(A, T, _), flat(A, T).",
    );
    let diags = check(&src, "on_bound");
    assert!(errors(&diags).is_empty(), "{}", text(&diags));
}

// adv-12: a declared relation that no rule defines is empty for ever, so
// every rule reading it positively is dead; that deserves a warning.

#[test]
fn a_declared_relation_with_no_rules_is_warned_about() {
    let src = common::strategy(
        "ghost_rel",
        "    qty : Quantity<Shares> = 100 shares",
        "    rel ghost(A: Equity, @T)",
        "    decide(T, trade(A, qty)) :- universe(A, T), ghost(A, T), flat(A, T).",
    );
    let diags = check(&src, "ghost_rel");
    assert!(errors(&diags).is_empty(), "{}", text(&diags));
    let w: Vec<&Diagnostic> = diags.iter().filter(|d| d.severity == Severity::Warning).collect();
    assert_eq!(w.len(), 1, "exactly one warning, for `ghost`:\n{}", text(&diags));
    assert!(w[0].code == Code::W3 && w[0].message.contains("`ghost`") && w[0].message.contains("no rule"), "{}", w[0]);
    let decl_line = src.lines().position(|l| l.contains("rel ghost")).unwrap() as u32 + 1;
    assert_eq!(w[0].span.line, decl_line, "reported at the declaration: {}", w[0]);
}

// trend-11: a bound Timestamp may be copied into a value column with `X = T`
// (section 2: non-key timestamps are ordinary values).

#[test]
fn a_bound_timestamp_may_be_assigned_as_a_value() {
    let src = common::strategy(
        "held_since",
        "    qty : Quantity<Shares> = 100 shares",
        "    rel entry(A: Equity, @T, E: Price<USD>, TE: Timestamp)\n    entry(A, T, E, TE) :- fill(A, T, Q, P), Q > 0 shares, E = P, TE = T.\n    entry(A, T, E, TE) :- position(A, T, Q), Q > 0 shares, not fill(A, T, _, _), prev(T, T0), entry(A, T0, E, TE).",
        "    decide(T, trade(A, qty)) :- universe(A, T), flat(A, T).\n    decide(T, trade(A, Q)) :- held(A, T, Q0), entry(A, T, _, TE), lag(T, 30d, T0), TE <= T0, Q = -Q0.",
    );
    let prog = program(&src, "held_since");
    let ds = synthetic_daily(&["AAA", "BBB"], (2023, 1, 2), 120, 3);
    let r = run(&prog, &ds, ExecConfig::default()).unwrap();
    let sells: Vec<_> = r.decisions.iter().filter(|d| d.decision.amount < 0.0).collect();
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

// Review of #43: a calendar duration has no single length in days, so a
// default is outside its range only when it is outside for every length a
// month or a year can take.

#[test]
fn a_calendar_duration_default_is_rejected_only_when_no_month_length_fits() {
    let strategy = |range: &str| {
        common::strategy(
            "cal",
            &format!("    qty : Quantity<Shares> = 100 shares\n    lb : Duration = 1mo in {}", range),
            "    rel m(A: Equity, @T, M: Price<USD>)\n    m(A, T, M) :- universe(A, T), M = mean(P) over (T1 in window[lb] min 1, close(A, T1, P)).",
            "    decide(T, trade(A, qty)) :- m(A, T, _), flat(A, T).",
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
// a library's declarations as well; a library is checked under the strategy
// that uses it.

#[test]
#[ignore = "src: W3 is not reported for a library relation declared with no rule; the empty template instantiates silently"]
fn a_library_declared_relation_with_no_rules_is_warned_about() {
    let lib = "library ghost_lib {\n  data { class any }\n  rules {\n    rel ghost(A: Entity, @T)\n  }\n}\n";
    let src = common::StrategyParts {
        name: "uses_ghost".into(),
        uses: "calendar, book, ghost_lib".into(),
        params: "    qty : Quantity<Shares> = 100 shares".into(),
        decides: "    decide(T, trade(A, qty)) :- universe(A, T), ghost(A, T), flat(A, T).".into(),
        ..Default::default()
    }
    .render();
    let diags = check_with_lib(lib, &src, "uses_ghost");
    assert!(errors(&diags).is_empty(), "{}", text(&diags));
    let w: Vec<&Diagnostic> = diags.iter().filter(|d| d.severity == Severity::Warning).collect();
    assert_eq!(w.len(), 1, "exactly one warning, for `ghost`:\n{}", text(&diags));
    assert!(w[0].code == Code::W3 && w[0].unit == "ghost_lib" && w[0].message.contains("`ghost`"), "{}", w[0]);
}

#[test]
fn a_library_relation_named_after_a_builtin_is_a_u_error() {
    let lib = "library kw_lib {\n  data { class any }\n  rules {\n    rel lag(A: Entity, @T)\n    lag(A, T) :- universe(A, T).\n  }\n}\n";
    let src = common::StrategyParts {
        name: "uses_kw".into(),
        uses: "calendar, book, kw_lib".into(),
        params: "    qty : Quantity<Shares> = 100 shares".into(),
        decides: "    decide(T, trade(A, qty)) :- universe(A, T), flat(A, T).".into(),
        ..Default::default()
    }
    .render();
    let diags = check_with_lib(lib, &src, "uses_kw");
    assert_only(&diags, Code::U);
    assert!(errors(&diags).iter().any(|d| d.unit == "kw_lib" && d.message.contains("`lag`")), "{}", text(&diags));
}

// #20: a reduction whose group holds every identity of its atom keeps every
// tuple, so `top[1]` selects everything and `by` is dead (WF-13, W4).

#[test]
fn a_reduction_with_every_identity_column_bound_warns_w4() {
    let strategy = |within: &str, binder: &str| {
        common::strategy(
            "degenerate_top",
            "    w : Scalar = 0.3",
            &format!(
                "    rel cand(A: Equity, @T, P: Price<USD>)\n    cand(A, T, P) :- universe(A, T), close(A, T, P).\n    rel chosen(A: Equity, @T)\n    chosen(A, T) :- {}, top[1](cand(A, T, P) by (P desc, A asc)) within ({}).",
                binder, within
            ),
            "    decide(T, hold(A, w)) :- universe(A, T), chosen(A, T).",
        )
    };
    let diags = check(&strategy("A, T", "universe(A, T)"), "degenerate_top");
    assert!(errors(&diags).is_empty(), "{}", text(&diags));
    let w4: Vec<&Diagnostic> = diags.iter().filter(|d| d.code == Code::W4).collect();
    assert_eq!(w4.len(), 1, "{}", text(&diags));
    assert!(w4[0].message.contains("chosen") || w4[0].message.contains("cand"), "{}", w4[0].message);
    // The enumerable form selects one name and warns about nothing.
    let diags = check(&strategy("T", "bar(T)"), "degenerate_top");
    assert!(diags.is_empty(), "{}", text(&diags));
}

// Data-bundle doc, section 9 item 3 (W5 in this repo's numbering): numeric
// literals inside a strategy's rules are degrees of freedom the study must
// count, so each one warns; the structural constants 0 and 1 (a flat target,
// `W = 1 / N`, a sign test) do not.

fn dof_clean() -> String {
    common::StrategyParts {
        name: "dof_clean".into(),
        uses: "calendar, stat, book".into(),
        params: "    lb : Duration = 20d\n    k : Count = 10".into(),
        rules: "    rel chosen(A: Equity, @T)\n    chosen(A, T) :- universe(A, T), zscore_window[lb, k, close](A, T, Z), Z > 1.\n    rel n_chosen(@T, N: Count)\n    n_chosen(T, N) :- bar(T), N = count(A) over (chosen(A, T)) within (T), N > 0.".into(),
        decides: "    decide(T, hold(A, W)) :- chosen(A, T), n_chosen(T, N), W = 1 / N.\n    decide(T, hold(A, 0)) :- held(A, T, Q), Q > 0 shares, zscore_window[lb, k, close](A, T, Z), Z < 0.".into(),
        ..Default::default()
    }
    .render()
}

#[test]
fn structural_zero_and_one_do_not_warn() {
    let diags = check(&dof_clean(), "dof_clean");
    assert!(diags.is_empty(), "{}", text(&diags));
}

#[test]
fn a_numeric_literal_in_a_strategy_rule_warns_w5() {
    // Two literals: a threshold and a weight.
    let src = dof_clean()
        .replace("zscore_window[lb, k, close](A, T, Z), Z > 1.", "zscore_window[lb, k, close](A, T, Z), Z > 1.5.")
        .replace("hold(A, W)) :- chosen(A, T), n_chosen(T, N), W = 1 / N.", "hold(A, 0.25)) :- chosen(A, T), n_chosen(T, N).");
    let diags = check(&src, "dof_clean");
    assert!(errors(&diags).is_empty(), "{}", text(&diags));
    let w5: Vec<&Diagnostic> = diags.iter().filter(|d| d.code == Code::W5).collect();
    assert_eq!(w5.len(), 2, "one W5 per literal:\n{}", text(&diags));
    let msgs: Vec<&str> = w5.iter().map(|d| d.message.as_str()).collect();
    for lit in ["1.5", "0.25"] {
        assert!(msgs.iter().any(|m| m.contains(&format!("`{}`", lit))), "no W5 names `{}`:\n{}", lit, text(&diags));
    }
    assert!(w5.iter().all(|d| d.message.contains("params")), "the fix is to lift the literal into `params`:\n{}", text(&diags));
    assert!(w5.iter().all(|d| d.rule.is_some()), "W5 is reported in its rule:\n{}", text(&diags));
    assert!(diags.iter().all(|d| d.code == Code::W5), "nothing else warns:\n{}", text(&diags));
}

/// A literal written in a bracket argument of the strategy's own rule (the
/// `min` count of a library window) is a degree of freedom of the strategy
/// like any other literal of the rule.
#[test]
#[ignore = "src: a strategy's bracket literal (`30` in `zscore_window[lb, 30, close]`) is neither warned W5 nor counted for the strategy; it is substituted into the library instance and counted as a library literal"]
fn a_literal_in_a_bracket_argument_warns_w5() {
    let src = dof_clean().replace("zscore_window[lb, k, close](A, T, Z), Z > 1.", "zscore_window[lb, 30, close](A, T, Z), Z > 1.");
    let (p, diags) = common::check(&src, "dof_clean");
    assert!(errors(&diags).is_empty(), "{}", text(&diags));
    let w5: Vec<&Diagnostic> = diags.iter().filter(|d| d.code == Code::W5).collect();
    assert!(w5.iter().any(|d| d.message.contains("`30`") && d.rule.as_deref() == Some("dof_clean::chosen#1")), "{}", text(&diags));
    let dof = &p.unwrap().degrees_of_freedom;
    assert!(dof.literals.iter().any(|(rule, l)| rule == "dof_clean::chosen#1" && l.to_string() == "30"), "{:?}", dof);
}

#[test]
fn a_library_literal_is_counted_not_warned() {
    // `liquid` in `bars` hard-codes `adv[20]`.
    let src = common::StrategyParts {
        name: "dof_lib".into(),
        params: "    lookback : Duration = 1y\n    skip : Duration = 1mo\n    min_adv : Notional<USD> = 1_000_000 USD".into(),
        rules: "    rel cand(A: Equity, @T, M: Scalar)\n    cand(A, T, M) :- liquid[min_adv](A, T), lag(T, skip, T1), close(A, T1, P1), lag(T, lookback, T0), close(A, T0, P0), M = P1 / P0 - 1."
            .into(),
        decides: "    decide(T, hold(A, 0)) :- cand(A, T, M), M < 0.".into(),
        ..Default::default()
    }
    .render();
    let diags = check(&src, "dof_lib");
    assert!(diags.is_empty(), "library literals never warn:\n{}", text(&diags));
    let p = program(&src, "dof_lib");
    let dof = &p.degrees_of_freedom;
    assert_eq!(dof.params, 3);
    // The strategy's own rules: the universe rule, `cand` and the decide rule.
    assert_eq!(dof.rules, 3);
    assert!(dof.literals.is_empty(), "{:?}", dof.literals);
    let lib: Vec<String> = dof.library_literals.iter().map(|(rule, l)| format!("{} {}", rule, l)).collect();
    // A bracket argument is substituted into the instance it calls, so the
    // `20` of `liquid` is counted at the rule of `adv[20]`.
    assert!(lib.iter().any(|s| s.starts_with("bars::adv[20]") && s.ends_with(" 20")), "{:?}", lib);
    // Only reachable library rules count: nothing but `liquid` and what it
    // reads is used here.
    assert!(lib.iter().all(|s| s.starts_with("bars::adv[20]")), "{:?}", lib);
}

#[test]
fn degrees_of_freedom_counts_params_literals_and_rules() {
    let src = dof_clean().replace("Z > 1.", "Z > 1.5.");
    let p = program(&src, "dof_clean");
    let dof = &p.degrees_of_freedom;
    assert_eq!(dof.params, 2);
    // The universe rule is a rule of the strategy (3.7), with the two derived
    // rules and the two decide rules.
    assert_eq!(dof.rules, 5, "the universe rule, two derived rules and two decide rules");
    assert_eq!(dof.literals.len(), 1);
    assert_eq!(dof.literals[0].0, "dof_clean::chosen#1");
    assert_eq!(dof.literals[0].1.to_string(), "1.5");
    assert_eq!(dof.summary(), "degrees of freedom: 2 params, 1 literal, 5 rules (+ 0 library literals)");
}

// Data-bundle doc, section 9 item 2 (W6): a ticker literal names the security
// that carried the ticker at the bundle date; it is a snapshot, not an identity.

#[test]
fn an_equity_literal_warns_w6_as_a_snapshot() {
    let src = common::strategy(
        "snapshot",
        "    bench : Equity = \"SPY\"",
        "    rel up(@T)\n    up(T) :- bar(T), log_price_return(bench, T, R), R > 0.",
        "    decide(T, hold(A, 0)) :- up(T), held(A, T, _), A = \"AAA\".",
    );
    let diags = check(&src, "snapshot");
    assert!(errors(&diags).is_empty(), "{}", text(&diags));
    let w6: Vec<&Diagnostic> = diags.iter().filter(|d| d.code == Code::W6).collect();
    assert_eq!(w6.len(), 2, "one for the param default, one for the in-rule literal:\n{}", text(&diags));
    let param_line = src.lines().position(|l| l.contains("bench : Equity")).unwrap() as u32 + 1;
    assert!(w6.iter().any(|d| d.message.contains("\"SPY\"") && d.rule.is_none() && d.span.line == param_line), "{}", text(&diags));
    assert!(w6.iter().any(|d| d.message.contains("\"AAA\"") && d.rule.as_deref() == Some("snapshot::decide#1")), "{}", text(&diags));
    assert!(w6.iter().all(|d| d.message.contains("snapshot") && d.message.contains("ticker")), "{}", text(&diags));
    assert!(diags.iter().all(|d| d.code == Code::W6), "nothing else warns:\n{}", text(&diags));
    assert!(Code::W5.is_warning() && Code::W6.is_warning() && !Code::F.is_warning());
}
