//! Claims, test steps, feed and activation steps and reports in a plan's
//! execution (Q-4.3, Q-5 to Q-8), over kernel services sharing one data
//! service in this process: every claim is judged with its p-value adjusted
//! within its phase and its verdict written as a row; a `before` claim
//! reads the run that runs first, a `dsr` claim the lineage's trial count;
//! the claims change no run (Proposition Q-5.2); a failed `block` claim
//! stops the runs that would follow it, and an `after` one the reveal; a
//! reveal judges every claim again over the embargoed bars, undecidable
//! where a claim reads a run that never saw them; the feed and activation
//! steps' tables and the report's, with the forty biases, are written to
//! the data layer.

#![allow(clippy::result_large_err)]

use abt_kernel::data::synthetic_daily;
use abt_kernel::kernel::Dataset;
use abt_query::data::Data;
use abt_query::kernel::{Kernel, Program};
use abt_query::plan::claims::Verdict;
use abt_query::plan::exec::{execute, Endpoints, Execution, Options};
use abt_query::plan::{self, Phase};
use abt_tests::{momentum, plan_program, plan_services, FRICTIONLESS};

fn market() -> Dataset {
    synthetic_daily(&["AAA", "BBB", "CCC", "DDD"], (2022, 1, 3), 500, 7)
}

fn setup(name: &str) -> (Program, Data, Vec<Kernel>) {
    let src = momentum(name, FRICTIONLESS);
    let prog = abt_tests::program(&src, name);
    let (data, kernels) = plan_services(&prog, &market(), 2);
    (plan_program(&src, name), data, kernels)
}

fn run(text: &str, prog: &Program, data: &Data, kernels: &[Kernel], reveal: Option<&str>) -> Result<Execution, String> {
    let units = plan::parse(text)?;
    let name = units.iter().find(|u| !matches!(u, plan::Unit::Template { .. })).map(|u| u.name().to_string()).unwrap();
    let p = plan::plan(&units, &name)?;
    let opts = Options {
        threads: 2,
        reveal: reveal.map(str::to_string),
        ..Options::default()
    };
    execute(&p, prog, &Endpoints { data, kernels }, &opts).map_err(|e| e.to_string())
}

const CLAIMED: &str = r#"
study claimed {
  program claimed
  grid { n = 1, 2 }
  holdout trailing:1y
  claims {
    normal_warmup  : jarque_bera on returns of warmup expect accept before
    pair_drifts    : engle_granger on feed close(A in (AAA, BBB)) expect accept before
    profitable     : dsr on returns of best(main) expect statistic > 0.5 after
    not_overfit    : pbo on grid main expect statistic < 0.5 after
    rules_apart    : coactivation on events of best(main) with { rules = (decide_1, decide_2), reps = 200 } expect effect < 0.9 after
  }
  steps {
    run warmup
    grid main
    test momentum_ic: ic on feature momentum of warmup with { horizon = 5 }
    feed cover: coverage on close
    feed moments: returns on close
    activation act of best(main)
    report summary
  }
  report { metrics [sharpe, cagr, calmar]; text }
}
"#;

#[test]
fn a_plans_claims_tests_feeds_activation_and_report() {
    let (prog, data, kernels) = setup("claimed");
    let ex = run(CLAIMED, &prog, &data, &kernels, None).unwrap();
    assert_eq!(ex.outcomes.len(), 3, "the warm-up and the grid's two points");
    // Every claim judged in sample, with a verdict; the reasons of the
    // undecidable ones are stated.
    assert_eq!(ex.claims.len(), 5);
    for j in &ex.claims {
        assert!(j.in_sample);
        let v = j.verdict.as_ref().unwrap_or_else(|| panic!("{} has no verdict", j.name));
        if let Verdict::Undecidable(why) = v {
            assert!(!why.is_empty(), "{}", j.name);
        }
    }
    let by = |n: &str| ex.claims.iter().find(|j| j.name == n).unwrap();
    // The before claims ran before the grid: they read the warm-up run and
    // a feed, and their p-values are adjusted between the two of them.
    assert_eq!(by("normal_warmup").phase, Some(Phase::Before));
    assert_eq!(by("normal_warmup").runs, vec![ex.outcomes[0].run_id.clone()]);
    let before: Vec<f64> = ex.claims.iter().filter(|j| j.phase == Some(Phase::Before)).filter_map(|j| j.result.as_ref()?.p_value).collect();
    let adjusted = abt_query::plan::claims::benjamini_hochberg(&before.iter().map(|p| Some(*p)).collect::<Vec<_>>());
    let recorded: Vec<Option<f64>> = ex
        .claims
        .iter()
        .filter(|j| j.phase == Some(Phase::Before) && j.result.as_ref().is_some_and(|r| r.p_value.is_some()))
        .map(|j| j.p_adjusted)
        .collect();
    assert_eq!(recorded, adjusted);
    // PBO has no p-value: its claim reads the statistic, and is decided.
    assert!(matches!(by("not_overfit").verdict, Some(Verdict::Holds | Verdict::Fails)), "{:?}", by("not_overfit"));
    assert!(by("profitable").result.as_ref().is_some_and(|r| r.statistic.is_finite()), "{:?}", by("profitable"));
    assert!(by("rules_apart").result.is_some(), "{:?}", by("rules_apart").error);
    // The test step ran on the warm-up's ledger of `momentum`, with the
    // forward returns of its names.
    assert_eq!(ex.tests.len(), 1);
    let ic = &ex.tests[0];
    assert!(ic.result.as_ref().is_some_and(|r| r.n > 10), "{:?}", ic.error);
    // The feed and activation tables.
    let sheet = |n: &str| {
        ex.sheets
            .iter()
            .find(|s| s.name == n)
            .unwrap_or_else(|| panic!("no table {} in {:?}", n, ex.sheets.iter().map(|s| &s.name).collect::<Vec<_>>()))
    };
    assert_eq!(sheet("feed.cover").rows.len(), 1);
    assert_eq!(sheet("feed.moments").rows.len(), 5, "four names and the pooled row");
    let rules = sheet("act.activation.rules");
    assert_eq!(rules.rows.len(), 2, "{}", rules.text());
    assert_eq!(sheet("act.activation.pairs").rows.len(), 1);
    // Rows of the data layer: the verdicts, the tests, the trials of every
    // kind, the report's tables with the forty biases.
    let rows = |rel: &str| abt_query::relation::read(&data, rel).unwrap().map(|b| b.num_rows()).unwrap_or(0);
    let of = |n: &str| plan::trials::relation_of(&ex.hash, n);
    assert_eq!(rows(&of("verdicts")), 5);
    assert_eq!(rows(&of("tests")), 1);
    let counted = plan::lineage::trials(&data, &ex.lineage).unwrap();
    assert_eq!(
        (counted.by_kind.get("trial"), counted.by_kind.get("claim"), counted.by_kind.get("test")),
        (Some(&3), Some(&5), Some(&1))
    );
    assert_eq!(ex.trials, 9);
    assert_eq!(rows(&of("report.biases")), 40);
    assert_eq!(rows(&of("report.summary")), 3);
    assert_eq!(rows(&of("report.claims")), 5);
    assert_eq!(rows(&of("report.text")), 1, "the report block asks for the text");
    assert!(ex.reports.iter().any(|r| r.ends_with("report.feed.cover")));
    // Claims change nothing (Proposition Q-5.2): the same plan without its
    // claims and tests runs the same points to the same metrics.
    let bare = CLAIMED
        .lines()
        .filter(|l| !l.contains(" : ") && !l.contains("test momentum_ic"))
        .collect::<Vec<_>>()
        .join("\n")
        .replace("study claimed", "study bare");
    let b = run(&bare, &prog, &data, &kernels, None).unwrap();
    assert!(b.claims.is_empty());
    for (x, y) in ex.outcomes.iter().zip(&b.outcomes) {
        assert_eq!(serde_json::to_string(&x.returns).unwrap(), serde_json::to_string(&y.returns).unwrap());
    }
}

#[test]
fn a_failed_block_claim_stops_what_follows_it() {
    let (prog, data, kernels) = setup("stopped");
    let text = "study stopped { program stopped\n grid { n = 1, 2 }\n holdout trailing:1y\n claims { impossible : jarque_bera on returns of warmup expect statistic > 1e300 before block }\n steps { run warmup\n grid main\n compare c: best(main) by sharpe\n reveal final of best(main) } }";
    let ex = run(text, &prog, &data, &kernels, None).unwrap();
    assert_eq!(ex.claims[0].verdict, Some(Verdict::Fails));
    assert_eq!(ex.outcomes.len(), 1, "only the warm-up ran");
    let skipped: Vec<&str> = ex.skipped.iter().map(|(s, _)| s.as_str()).collect();
    assert_eq!(skipped, vec!["main", "c"]);
    assert!(ex.skipped[0].1.contains("impossible"));
    assert!(ex.warnings.iter().any(|w| w.message.contains("did not run")), "{:?}", ex.warnings);
    // An after claim that fails with `block` bars the reveal.
    let text = "study barred { program stopped\n grid { n = 1, 2 }\n holdout trailing:1y\n claims { rich : dsr on returns of best(main) expect statistic > 2 after block }\n steps { grid main\n reveal final of best(main) } }";
    let ex = run(text, &prog, &data, &kernels, None).unwrap();
    assert_eq!(ex.claims[0].verdict, Some(Verdict::Fails), "{:?}", ex.claims[0]);
    let refused = run(text, &prog, &data, &kernels, Some("final")).unwrap_err();
    assert!(refused.contains("rich") && refused.contains("nothing is revealed"), "{}", refused);
}

#[test]
fn a_reveal_judges_every_claim_again_over_the_embargoed_bars() {
    let (prog, data, kernels) = setup("revealed");
    let text = "study revealed { program revealed\n grid { n = 1, 2 }\n holdout trailing:1y\n claims { steady : ljung_box on returns of warmup expect accept before\n profitable : nw_mean on returns of best(main) expect statistic > 0 after }\n steps { run warmup\n grid main\n reveal final of best(main) } }";
    let ex = run(text, &prog, &data, &kernels, None).unwrap();
    assert_eq!(ex.claims.len(), 2);
    let r = run(text, &prog, &data, &kernels, Some("final")).unwrap();
    assert_eq!(r.claims.len(), 2);
    assert!(r.claims.iter().all(|j| !j.in_sample));
    let by = |n: &str| r.claims.iter().find(|j| j.name == n).unwrap();
    // The claim on the revealed grid reads the reveal's run, over the
    // embargo only.
    let reveal_run = &r.outcomes[0].run_id;
    assert_eq!(&by("profitable").runs, &vec![reveal_run.clone()]);
    let w = by("profitable").window.unwrap();
    assert!(w.0 >= ex.embargo.start().unwrap(), "{:?} against {:?}", w, ex.embargo);
    // The warm-up never saw the embargoed bars.
    match &by("steady").verdict {
        Some(Verdict::Undecidable(why)) => assert!(why.contains("never saw the embargoed bars"), "{}", why),
        v => panic!("{:?}", v),
    }
    // Both pairs of verdicts are rows.
    let verdicts = plan::trials::verdicts(&data, &ex.hash).unwrap();
    assert_eq!(verdicts.iter().filter(|v| v.in_sample).count(), 2);
    assert_eq!(verdicts.iter().filter(|v| !v.in_sample).count(), 2);
}
