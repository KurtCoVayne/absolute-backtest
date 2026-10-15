//! The checker's diagnostics pinned exactly: count, codes and wording. One
//! root cause yields one diagnostic (no cascades), a library's diagnostics
//! appear once per workspace check, and messages name the thing to fix.

use abt_tests as common;

use abt_kernel::check::{check_program, check_workspace, Code, Diagnostic, Severity, Workspace};
use abt_kernel::kernel::time::format_timestamp;
use abt_kernel::kernel::ExecConfig;
use abt_tests::run;

fn errors(diags: &[Diagnostic]) -> Vec<&Diagnostic> {
    diags.iter().filter(|d| d.severity == Severity::Error).collect()
}

fn text(diags: &[Diagnostic]) -> String {
    common::text(diags)
}

fn check(src: &str, strategy: &str) -> Vec<Diagnostic> {
    common::check(src, strategy).1
}

/// Check a strategy against the base workspace with extra units (libraries)
/// added first.
fn check_with(units: &[&str], src: &str, strategy: &str) -> Vec<Diagnostic> {
    let mut ws = common::base_workspace();
    for u in units {
        ws.add_source(u).unwrap_or_else(|e| panic!("{}", e));
    }
    ws.add_source(src).unwrap_or_else(|e| panic!("{}", e));
    check_program(&ws, strategy).1
}

/// adv-05: a library written for another class is one unit-level E error,
/// not one E per primitive plus an F cascade through every rule.
#[test]
fn a_library_from_another_class_is_one_error() {
    let lib = r#"
library fut_lib {
  data { class future   need bars }
  rules {
    rel always(A: Future, @T)
    always(A, T) :- universe(A, T).
  }
}
"#;
    let src = common::StrategyParts {
        name: "b4_08".into(),
        uses: "calendar, book, fut_lib".into(),
        params: "    w : Scalar = 0.5".into(),
        decides: "    decide(T, hold(A, w)) :- always(A, T).".into(),
        ..Default::default()
    }
    .render();
    let diags = check_with(&[lib], &src, "b4_08");
    let errs = errors(&diags);
    assert_eq!(errs.len(), 1, "got:\n{}", text(&diags));
    assert_eq!(errs[0].code, Code::E);
    assert_eq!(errs[0].rule, None);
    for needle in ["`fut_lib`", "`future`", "`equity`"] {
        assert!(errs[0].message.contains(needle), "{}", errs[0]);
    }
}

/// adv-05: a class missing from the catalog is reported once, first, and
/// never as an empty name; the rules that depend on it (here the universe
/// rule over the listing) are not judged.
#[test]
fn a_missing_class_is_one_error_reported_first() {
    let src = common::StrategyParts {
        name: "p08".into(),
        class: "nosuchclass".into(),
        universe: "universe(A, T) :- listed(A, T).".into(),
        params: "    qty : Quantity<Shares> = 100 shares".into(),
        decides: "    decide(T, trade(A, qty)) :- universe(A, T), not position(A, T, _).".into(),
        ..Default::default()
    }
    .render();
    let diags = check(&src, "p08");
    let errs = errors(&diags);
    assert_eq!(errs.len(), 1, "got:\n{}", text(&diags));
    assert_eq!(errs[0].code, Code::U);
    assert!(errs[0].message.contains("class `nosuchclass` is not in the catalog"), "{}", errs[0]);
    assert_eq!(diags[0].code, Code::U, "reported first:\n{}", text(&diags));
    assert!(!text(&diags).contains("``"), "empty name printed:\n{}", text(&diags));
}

/// adv-05: a strategy checked without its catalog, library and preset files
/// gets one diagnostic per missing unit and nothing per relation.
#[test]
fn a_strategy_alone_reports_only_the_missing_units() {
    let src = common::corpus_source("strategies", "sma_crossover");
    let mut ws = Workspace::new();
    ws.add_source(&src).unwrap();
    let diags = check_program(&ws, "sma_crossover").1;
    let errs = errors(&diags);
    // The class, the three libraries and the execution preset.
    assert_eq!(errs.len(), 5, "got:\n{}", text(&diags));
    assert!(errs.iter().all(|d| d.rule.is_none()), "got:\n{}", text(&diags));
    assert!(text(&diags).contains("class `equity` is not in the catalog"), "{}", text(&diags));
    for lib in ["calendar", "stat", "book"] {
        assert!(
            errs.iter().any(|d| d.code == Code::U && d.message.contains(&format!("library `{}` is not in the workspace", lib))),
            "{}",
            text(&diags)
        );
    }
    // The preset is a unit of the workspace too; its absence is the closed
    // configuration judgment (WF-14).
    assert!(
        errs.iter()
            .any(|d| d.code == Code::P && d.message.contains("execution preset `equity_default` is not in the workspace")),
        "{}",
        text(&diags)
    );
    assert!(!text(&diags).contains("``"), "empty name printed:\n{}", text(&diags));
}

/// adv-05: an undeclared relation that binds the head time is one U error;
/// the atoms after it are not judged for causality against a time the
/// checker could not place.
#[test]
fn an_unresolved_relation_does_not_cascade_into_causality_errors() {
    let src = common::strategy(
        "undeclared_first",
        "    qty : Quantity<Shares> = 100 shares",
        "",
        "    decide(T, trade(A, qty)) :- rsi[14d](A, T, R), R < 30, flat(A, T).",
    );
    let diags = check(&src, "undeclared_first");
    let errs = errors(&diags);
    assert_eq!(errs.len(), 1, "got:\n{}", text(&diags));
    assert_eq!(errs[0].code, Code::U);
    assert!(errs[0].message.contains("`rsi`"), "{}", errs[0]);
}

/// adv-07: when the head time is itself bound non-causally, the atoms keyed
/// by it are not each told that `T` is not derived from `T`. The real
/// lookahead (`held` at a fresh `T1`) is the F error; binding the already
/// bound head time as the output of `prev` is a B error (the second
/// argument of prev binds a fresh variable), and `flat(A, T)` is not blamed.
#[test]
fn a_non_causal_head_time_is_not_reported_per_atom() {
    let src = common::strategy(
        "la11",
        "    qty : Quantity<Shares> = 100 shares",
        "",
        "    decide(T, trade(A, qty)) :- held(A, T1, _), prev(T1, T), flat(A, T).",
    );
    let diags = check(&src, "la11");
    let errs = errors(&diags);
    assert_eq!(errs.len(), 2, "got:\n{}", text(&diags));
    assert!(
        errs.iter().any(|d| d.code == Code::F && d.message.contains("`T1` of `held`")),
        "the real lookahead is named:\n{}",
        text(&diags)
    );
    assert!(
        errs.iter().any(|d| d.code == Code::B && d.message.contains("`T` is already bound") && d.message.contains("prev")),
        "the head's time is named:\n{}",
        text(&diags)
    );
    assert!(!text(&diags).contains("of `flat`"), "no follow-on error on flat(A, T):\n{}", text(&diags));
}

/// adv-06: a library in the workspace is checked under every using
/// strategy; its diagnostics must still be reported once.
#[test]
fn a_failing_library_used_by_a_strategy_is_reported_once() {
    let lib = r#"
library histlib {
  data { class equity }
  rules {
    rel bought_now(A: Equity, @T)
    bought_now(A, T) :- universe(A, T), decided(T, trade(A, Q)), Q > 0 shares.
  }
}
"#;
    let src = common::StrategyParts {
        name: "la09".into(),
        uses: "calendar, book, histlib".into(),
        decides: "    decide(T, trade(A, Q)) :- bought_now(A, T), held(A, T, Q0), Q = -Q0.".into(),
        ..Default::default()
    }
    .render();
    let mut ws = common::base_workspace();
    ws.add_source(lib).unwrap();
    ws.add_source(&src).unwrap();
    let diags = check_workspace(&ws);
    let errs = errors(&diags);
    assert_eq!(errs.len(), 1, "one F error on histlib::bought_now#1, got:\n{}", text(&diags));
    assert_eq!(errs[0].code, Code::F);
    assert_eq!(errs[0].unit, "histlib");
    assert_eq!(errs[0].rule.as_deref(), Some("histlib::bought_now#1"));
}

/// The dedupe keeps distinct diagnostics on the same span: two strategies
/// on different decision calendars using one library give that library two
/// different messages (here W7: a five-day window holds no weekly bar and
/// no monthly bar).
#[test]
fn distinct_library_diagnostics_are_both_kept() {
    let lib = r#"
library wlib {
  data { class any   need bars }
  rules {
    rel m(A: Entity, @T, M: Price<USD>)
    m(A, T, M) :- universe(A, T), M = mean(P) over (T1 in window[5d] min 4, close(A, T1, P)).
  }
}
"#;
    let strategy = |name: &str, cal: &str, ctor: &str| {
        common::StrategyParts {
            name: name.into(),
            uses: "calendar, book, wlib".into(),
            calendars: Some(format!("{} = {}", cal, ctor)),
            clock: format!("decide on {}", cal),
            universe: format!("universe(A, T) :- listed(A, T), traded@{}(A, T).", cal),
            params: "    w : Scalar = 0.5".into(),
            decides: "    decide(T, hold(A, w)) :- m(A, T, _).".into(),
            report: format!("calendar {}   periods 52 per year", cal),
            ..Default::default()
        }
        .render()
    };
    let mut ws = common::base_workspace();
    ws.add_source(lib).unwrap();
    ws.add_source(&strategy("on_weeks", "weekly", "weeks(1)")).unwrap();
    ws.add_source(&strategy("on_months", "monthly", "months(1)")).unwrap();
    let diags = check_workspace(&ws);
    assert!(errors(&diags).is_empty(), "{}", text(&diags));
    let on_m: Vec<&Diagnostic> = diags.iter().filter(|d| d.unit == "wlib" && d.code == Code::W7).collect();
    assert_eq!(on_m.len(), 2, "one W7 per using strategy, got:\n{}", text(&diags));
    assert_ne!(on_m[0].message, on_m[1].message);
    assert!(
        on_m.iter().any(|d| d.message.contains("`weekly`")) && on_m.iter().any(|d| d.message.contains("`monthly`")),
        "{}",
        text(&diags)
    );
}

/// intraday-08: a library rule reading an executor relation on a calendar
/// other than the decision calendar is an X error in the library that names
/// the decision calendar and the calendar it was read on, since the library
/// checks clean at the decision calendar.
#[test]
fn an_executor_relation_in_a_library_names_the_calendar_it_is_read_on() {
    let lib = r#"
library booklib {
  data { class any }
  rules {
    rel held_at(A: Entity, @T, Q: Quantity<Unit>)
    held_at(A, T, Q) :- position(A, T, Q).
  }
}
"#;
    let src = r#"
strategy off_clock {
  data { class future   need bars @1m   need sessions, point_value, commission   require tested   prefer latest }
  uses calendar, booklib
  calendars { session = sessions   es_days = sessions of "ES" }
  clock { decide on base at session + 30min }
  universe {
    instruments { ES from 2000-01-01; NQ from 2000-01-01 }
    universe(A, T) :- instruments(A, From), session_of(A, T, _), T >= From.
  }
  params { lot : Quantity<Contracts> = 1 contracts }
  rules { }
  decisions {
    silence flat
    orders moo_moc
    decide(T, hold(A, lot)) :- universe(A, T), session_of(A, T, S), prev(S, S1), held_at@session(A, S1, Q), Q = 0 contracts.
  }
  execution futures_fixed_base
  report { calendar es_days   periods 252 per year }
}
"#;
    let diags = check_with(&[lib], src, "off_clock");
    let errs = errors(&diags);
    assert!(!errs.is_empty() && errs.iter().all(|d| d.code == Code::X), "got:\n{}", text(&diags));
    let in_lib: Vec<&&Diagnostic> = errs.iter().filter(|d| d.unit == "booklib").collect();
    assert_eq!(in_lib.len(), 1, "got:\n{}", text(&diags));
    assert!(in_lib[0].message.contains("`position` is supplied at the decision calendar `base`"), "{}", in_lib[0]);
    assert!(in_lib[0].message.contains("key on `session`"), "{}", in_lib[0]);
}

/// portfolio-06: the bound position of prev and lag is an output (section 4,
/// "Binds"), so `_` is permitted there (WF-2) and means the same as a fresh
/// variable that is never read.
#[test]
fn a_wildcard_in_the_bound_position_of_prev_and_lag_is_a_fresh_variable() {
    let strategy = |wild: bool| {
        let (p, l) = if wild { ("_", "_") } else { ("T0", "T5") };
        common::strategy(
            "has_history",
            "    qty : Quantity<Shares> = 10 shares\n    depth : Duration = 5d",
            &format!(
                "    rel bar_before(@T)\n    bar_before(T) :- bar(T), prev(T, {}).\n    rel deep(@T)\n    deep(T) :- bar(T), lag(T, depth, {}).",
                p, l
            ),
            "    decide(T, trade(A, qty)) :- universe(A, T), bar_before(T), not deep(T), flat(A, T).\n    decide(T, trade(A, Q)) :- held(A, T, Q0), deep(T), Q = -Q0.",
        )
    };
    let ds = common::market("AAA", &[10.0, 11.0, 12.0, 11.0, 10.0, 12.0, 13.0, 12.0, 11.0, 10.0]);
    let mut outputs = Vec::new();
    for wild in [true, false] {
        let (prog, diags) = common::check(&strategy(wild), "has_history");
        assert!(diags.is_empty(), "wild = {}: should check clean (no errors, no warnings), got:\n{}", wild, text(&diags));
        let r = run(
            &prog.unwrap(),
            &ds,
            ExecConfig {
                initial_cash: 10_000.0,
                ..ExecConfig::frictionless()
            },
        )
        .unwrap();
        let out: Vec<String> = r.decisions.iter().map(|d| format!("{} {}", format_timestamp(d.t), r.describe_decision(&d.decision))).collect();
        outputs.push(out);
    }
    assert_eq!(outputs[0], outputs[1]);
    // The second bar has a bar before it and no five days of history: a buy;
    // once there are, the position is sold.
    assert_eq!(outputs[0][0], "2024-01-09 trade(AAA, 10)");
    assert!(outputs[0].iter().any(|d| d.contains("trade(AAA, -10)")), "{:?}", outputs[0]);
}

/// trend-12: `not month_start(T)` is rejected as an undeclared relation, and
/// the message says why (a builtin is not a relation) and shows the idiom,
/// which checks clean.
#[test]
fn a_negated_builtin_explains_itself_and_names_the_idiom() {
    let diags = check(
        &common::strategy(
            "not_builtin",
            "    qty : Quantity<Shares> = 100 shares",
            "",
            "    decide(T, trade(A, qty)) :- universe(A, T), flat(A, T), not month_start(T).\n    decide(T, trade(A, Q)) :- held(A, T, Q0), month_start(T), Q = -Q0.",
        ),
        "not_builtin",
    );
    let errs = errors(&diags);
    assert_eq!(errs.len(), 1, "got:\n{}", text(&diags));
    assert_eq!(errs[0].code, Code::U);
    let m = &errs[0].message;
    assert!(m.contains("`month_start` is a temporal builtin, not a relation"), "{}", m);
    assert!(m.contains("mstart(T) :- bar(T), month_start(T)."), "{}", m);
    assert!(m.contains("can be negated"), "{}", m);

    let diags = check(
        &common::strategy(
            "mstart_idiom",
            "    qty : Quantity<Shares> = 100 shares",
            "    rel mstart(@T)\n    mstart(T) :- bar(T), month_start(T).",
            "    decide(T, trade(A, qty)) :- universe(A, T), flat(A, T), not mstart(T).\n    decide(T, trade(A, Q)) :- held(A, T, Q0), mstart(T), Q = -Q0.",
        ),
        "mstart_idiom",
    );
    assert!(diags.is_empty(), "the idiom checks clean, got:\n{}", text(&diags));
}
