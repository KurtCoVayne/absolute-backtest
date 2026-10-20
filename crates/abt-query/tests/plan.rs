//! The plan language (Q-2.1, Q-2.2, Q-2.4, Q-5.1): studies and templates
//! parse to their plans; a plan with no steps has the default ones; the
//! normal form, and so H(Π), does not depend on what the steps and claims
//! are called, the order independent steps are written in, the report or
//! the study's name, and does depend on everything else; what a step or a
//! claim reads must be a step of the right kind, and a cycle is refused.

use abt_query::plan::{self, normal_form, At, Expect, Holdout, Input, Op, Phase, Plan, Policy, ProgramRef, Ref, Step};

const H: &str = "0000000000000000000000000000000000000000000000000000000000000001";

fn one(text: &str, name: &str) -> Result<Plan, String> {
    plan::plan(&plan::parse(text)?, name)
}

fn hash(text: &str, name: &str) -> String {
    normal_form(&one(text, name).unwrap(), H).unwrap().hash
}

const FULL: &str = r#"
# a study with every block
study momentum {
  program  strategies/momentum.dsl
  params   { lookback = 63; top = 10 }
  grid     { lookback = 21, 63, 126; top = 5, 10 }
  holdout  trailing:2y
  walk_forward rolling:2y:6mo
  window   2005-01-03 .. 2020-12-31
  budget   { money 0; bytes 10GB; requests 100 }
  objective calmar
  claims {
    signal_has_ic : ic on feature score of warmup with { horizon = 1 } expect statistic > 0 at 0.05 before
    profitable    : dsr on returns of best(main) expect statistic > 0.95 after block
  }
  steps {
    run warmup observe score
    grid main
    walk_forward folds
    reveal final of best(main)
    test vol: arch_lm on returns of best(main) with { lags = 5 }
    report summary
  }
  report { metrics [sharpe, cagr]; tables [surface, folds]; text }
}
"#;

#[test]
fn a_study_parses_to_its_plan() {
    let p = one(FULL, "momentum").unwrap();
    assert_eq!(p.program, ProgramRef::Path("strategies/momentum.dsl".into()));
    assert_eq!(p.params, vec![("lookback".to_string(), "63".to_string()), ("top".to_string(), "10".to_string())]);
    assert_eq!(p.grid.len(), 2);
    assert_eq!(p.grid_points().len(), 6);
    assert!(p.grid_points().iter().all(|q| q.len() == 2), "{:?}", p.grid_points());
    assert_eq!(p.holdout, Holdout::Trailing { months: 24 });
    assert_eq!(p.walk_forward.as_ref().map(|w| (w.anchored, w.train_months, w.test_months)), Some((false, 24, 6)));
    assert_eq!(p.window.as_ref().map(|w| w.describe()), Some("2005-01-03 .. 2020-12-31".into()));
    assert_eq!(p.objective, "calmar");
    assert_eq!(p.budget.as_ref().and_then(|b| b.bytes.clone()), Some("10GB".into()));
    assert_eq!(p.report.metrics, vec!["sharpe", "cagr"]);
    assert!(p.report.text);
    assert_eq!(p.steps.iter().map(|s| s.kind()).collect::<Vec<_>>(), vec!["run", "grid", "walk_forward", "reveal", "test", "report"]);
    assert_eq!(
        p.steps[0],
        Step::Run {
            name: "warmup".into(),
            at: None,
            over: None,
            observe: vec!["score".into()]
        }
    );
    assert_eq!(
        p.steps[3],
        Step::Reveal {
            name: "final".into(),
            of: Ref { step: "main".into(), best: true }
        }
    );
    let c = &p.claims[1];
    assert_eq!((c.test.as_str(), c.phase, c.policy), ("dsr", Phase::After, Policy::Block));
    assert_eq!(c.expect, Expect::Statistic(Op::Gt, 0.95));
    assert_eq!(p.claims[0].params, vec![("horizon".to_string(), "1".to_string())]);
}

#[test]
fn the_specs_example_claims_parse() {
    let text = r#"
study pairs {
  program pairs
  grid { z = 1.5, 2.0 }
  claims {
    spread_stationary : adf on feature spread of warmup expect reject at 0.05 before
    signal_has_ic     : ic on feature score of warmup with { horizon = 1 } expect statistic > 0 at 0.05 before
    vol_clusters      : arch_lm on returns of warmup with { lags = 5 } expect reject before
    legs_cointegrate  : engle_granger on feed close(A in (XLE, XOP)) expect reject at 0.05 before
    profitable        : dsr on returns of best(main) expect statistic > 0.95 after
    not_overfit       : pbo on grid main expect statistic < 0.5 after
    rules_independent : coactivation on events of best(main) with { rules = (enter, exit) } expect effect < 0.3 after
    costs_right       : cost_realism on events of live_sim expect accept after
  }
  steps {
    run warmup observe spread, score
    grid main
    run live_sim at best(main)
  }
}
"#;
    let p = one(text, "pairs").unwrap();
    assert_eq!(p.program, ProgramRef::Name("pairs".into()));
    assert_eq!(p.claims.len(), 8);
    assert_eq!(
        p.claims[3].input,
        Input::Feed {
            relation: "close".into(),
            filter: Some("A in (XLE, XOP)".into())
        }
    );
    assert_eq!(p.claims[5].input, Input::Grid("main".into()));
    assert_eq!(p.claims[6].params, vec![("rules".to_string(), "(enter, exit)".to_string())]);
    assert_eq!(p.claims[6].expect, Expect::Effect(Op::Lt, 0.3));
    assert_eq!(p.claims[7].expect, Expect::Accept);
    assert_eq!(
        p.steps[2],
        Step::Run {
            name: "live_sim".into(),
            at: Some(At::Best("main".into())),
            over: None,
            observe: vec![]
        }
    );
    // Every claim's default policy is warn.
    assert!(p.claims.iter().all(|c| c.policy == Policy::Warn));
}

#[test]
fn a_plan_without_steps_has_the_default_ones() {
    let p = one("study s { program p.dsl\n grid { a = 1, 2 }\n walk_forward anchored:1y:3mo }", "s").unwrap();
    assert_eq!(
        p.steps.iter().map(|s| (s.kind(), s.name())).collect::<Vec<_>>(),
        vec![("grid", "main"), ("walk_forward", "folds"), ("report", "report")]
    );
    let q = one("study s { program p.dsl }", "s").unwrap();
    assert_eq!(q.steps.iter().map(|s| s.kind()).collect::<Vec<_>>(), vec!["run", "report"]);
    assert_eq!(q.objective, "sharpe");
    assert_eq!(q.holdout, Holdout::None);
}

#[test]
fn a_template_is_instantiated_by_substitution() {
    let text = r#"
template sweep(PROG, LB, YEARS) {
  program PROG
  grid { lookback = LB }
  holdout trailing:YEARS
}
study short = sweep(PROG = a.dsl, LB = 10, YEARS = 1y)
study long = sweep(PROG = a.dsl, LB = 100, YEARS = 3y)
"#;
    let units = plan::parse(text).unwrap();
    assert_eq!(units.len(), 3);
    let s = plan::plan(&units, "short").unwrap();
    assert_eq!(s.grid, vec![("lookback".to_string(), vec!["10".to_string()])]);
    assert_eq!(s.holdout, Holdout::Trailing { months: 12 });
    let l = plan::plan(&units, "long").unwrap();
    assert_eq!(l.holdout, Holdout::Trailing { months: 36 });
    // A template is not a plan.
    assert!(plan::plan(&units, "sweep").is_err_and(|e| e.contains("no study `sweep`")));
    let missing = "template t(A, B) { program A }\nstudy x = t(A = p.dsl)";
    assert!(one(missing, "x").is_err_and(|e| e.contains("parameter `B` has no value")), "{:?}", one(missing, "x"));
    let unknown = "template t(A) { program A }\nstudy x = t(A = p.dsl, C = 1)";
    assert!(one(unknown, "x").is_err_and(|e| e.contains("not a parameter")));
}

#[test]
fn the_normal_form_ignores_names_order_and_the_report() {
    let base = hash(FULL, "momentum");
    // The steps renamed, the claims renamed, independent steps reordered,
    // the study renamed, the report changed, comments added.
    let renamed = FULL
        .replace("study momentum", "study other")
        .replace("warmup", "w")
        .replace("best(main)", "best(g)")
        .replace("grid main", "grid g")
        .replace("signal_has_ic", "ic_claim")
        .replace("profitable", "p")
        .replace("report summary", "report r")
        .replace("metrics [sharpe, cagr]", "metrics [sortino]");
    assert_eq!(hash(&renamed, "other"), base, "{}", normal_form(&one(&renamed, "other").unwrap(), H).unwrap().text);
    let reordered = FULL.replace("    run warmup observe score\n    grid main\n", "    grid main\n    run warmup observe score\n");
    assert_eq!(hash(&reordered, "momentum"), base);
    // The claims in the other order.
    let swapped = FULL.replace(
        "    signal_has_ic : ic on feature score of warmup with { horizon = 1 } expect statistic > 0 at 0.05 before\n    profitable    : dsr on returns of best(main) expect statistic > 0.95 after block\n",
        "    profitable    : dsr on returns of best(main) expect statistic > 0.95 after block\n    signal_has_ic : ic on feature score of warmup with { horizon = 1 } expect statistic > 0 at 0.05 before\n",
    );
    assert_eq!(hash(&swapped, "momentum"), base);
    // The parameters in another order, and a window written otherwise.
    let params = FULL
        .replace("{ lookback = 63; top = 10 }", "{ top = 10, lookback = 63 }")
        .replace("2005-01-03 ..", "2005-01-03T00:00 ..");
    assert_eq!(hash(&params, "momentum"), base);
    // What changes the plan changes its hash.
    for changed in [
        FULL.replace("126", "252"),
        FULL.replace("trailing:2y", "trailing:3y"),
        FULL.replace("objective calmar", "objective sharpe"),
        FULL.replace("at 0.05 before", "at 0.01 before"),
        FULL.replace("lags = 5", "lags = 4"),
    ] {
        assert_ne!(hash(&changed, "momentum"), base);
    }
    let p = one(FULL, "momentum").unwrap();
    assert_ne!(normal_form(&p, &H.replace('1', "2")).unwrap().hash, base, "the program's hash is part of the plan's");
    let n = normal_form(&p, H).unwrap();
    assert!(n.text.contains(&format!("program {}", H)));
    assert!(!n.text.contains("report") && !n.text.contains("sortino"));
    // The steps in topological order, the report last.
    assert_eq!(n.order.last().map(|(i, _)| p.steps[*i].kind()), Some("report"));
    assert_eq!(n.canonical("warmup"), "s0");
    assert_eq!(n.canonical("main"), "s1");
}

#[test]
fn what_a_step_or_claim_reads_must_be_a_step_of_its_kind() {
    let refused = |steps: &str, extra: &str| {
        let text = format!(
            "study s {{ program p.dsl\n holdout trailing:1y\n grid {{ a = 1, 2 }}\n walk_forward rolling:1y:3mo\n{}\n steps {{\n{}\n }} }}",
            extra, steps
        );
        one(&text, "s").and_then(|p| normal_form(&p, H).map(|_| ())).err().unwrap_or_default()
    };
    assert!(refused("run a\nrun a", "").contains("two steps are named `a`"));
    assert!(refused("reveal r of nowhere", "").contains("no step is"));
    assert!(refused("run a\nreveal r of best(a)", "").contains("not a grid"));
    assert!(refused("feed f: coverage on close\nactivation x of f", "").contains("makes none"));
    assert!(refused("grid g\ntest t: adf on ledger g", "").contains("no ledger step"));
    assert!(refused("run a", "claims { c : pbo on grid a expect statistic < 0.5 }").contains("no grid or walk-forward step"));
    assert!(refused("reveal r of r", "").contains("cycle"));
    assert!(refused("run a\nrun b at best(a) observe x", "").contains("not a grid"));
    assert!(refused("grid g\nreveal r of g", "").contains("name it best(g)"));
    // A claim before the runs reads a feed or a run step.
    assert!(refused("grid g", "claims { c : pbo on grid g expect statistic < 0.5 before }").contains("judged before the runs"));
    // A run step before, a walk-forward's folds after: accepted.
    assert_eq!(
        refused(
            "run w\ngrid g\nwalk_forward f",
            "claims { c : adf on returns of w expect reject before\n d : wfe on grid f expect statistic > 0.5 }"
        ),
        ""
    );
    assert_eq!(refused("grid g\nwalk_forward f\nreveal r of best(g)\nrun x at best(g) over 2010-01-01 .. 2011-01-01", ""), "");
    let no_holdout = one("study s { program p.dsl\n steps { run a\n reveal r of a } }", "s");
    assert!(no_holdout.is_err_and(|e| e.contains("nothing to reveal")));
    let no_scheme = one("study s { program p.dsl\n steps { walk_forward f } }", "s");
    assert!(no_scheme.is_err_and(|e| e.contains("no walk_forward scheme")));
    assert!(one("study s { grid { a = 1 } }", "s").is_err_and(|e| e.contains("names no program")));
    assert!(one("study s { program p.dsl\n window 2020-01-01 .. 2019-01-01 }", "s").is_err_and(|e| e.contains("ends before")));
    assert!(one("study s { program p.dsl\n claims { c : adf on returns of main expect maybe } }", "s").is_err_and(|e| e.contains("not an outcome")));
}

/// E-118: a compare step's metrics bracketed, as the house template writes
/// them, are its metrics; `[return, ...]` was taken for one metric's name.
#[test]
fn a_compare_steps_metrics_may_be_bracketed() {
    let study = |by: &str| {
        format!(
            "study s {{\n  program  p.dsl\n  steps {{\n    run base\n    run alt at {{ n = 2 }}\n    compare variants: base, alt by {}\n  }}\n}}\n",
            by
        )
    };
    for by in ["[return, max_drawdown, sharpe]", "return, max_drawdown, sharpe"] {
        let p = one(&study(by), "s").unwrap();
        let Some(Step::Compare { by, .. }) = p.steps.iter().find(|s| s.kind() == "compare") else {
            panic!("{:?}", p.steps)
        };
        assert_eq!(by, &vec!["return".to_string(), "max_drawdown".to_string(), "sharpe".to_string()]);
    }
}
