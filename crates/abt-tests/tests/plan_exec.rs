//! Plans executed (Q-2.2 to Q-2.6) over kernel services sharing one data
//! service, all in this process: a grid study with a trailing hold-out is
//! the old in-memory study to the bit (every point's metrics, the best
//! point, PBO, the surface, the sub-period Sharpe ratios and the deflated
//! Sharpe over the lineage's trials), and so is a walk-forward (its folds,
//! its efficiency, its stitched curve); a block hold-out masks the blocks
//! out of the metrics and the ledgers and a reveal reports them alone; a
//! trailing reveal reports the tail; every run is a trial row, counted once
//! however often the plan runs over the same binding; π is in the run's id,
//! so another plan's run of the same point is another run, and a plan
//! renamed is the same plan; one kernel or four give the same outcomes.

#![allow(clippy::result_large_err)]

use std::path::PathBuf;
use std::sync::Arc;

use abt_kernel::check::Program as Checked;
use abt_kernel::data::synthetic_daily;
use abt_kernel::kernel::service::KernelService;
use abt_kernel::kernel::{DataClient, Dataset, ExecConfig};
use abt_kernel::study::{self as old, Grid, Holdout as OldHoldout, Project, Provenance};
use abt_query::data::Data;
use abt_query::kernel::{Kernel, Program};
use abt_query::plan::exec::{execute, Endpoints, Execution, Kind, Options};
use abt_query::plan::{self, Plan};
use abt_tests as common;
use abt_wire::generated::abt::wire::SourceFile;

/// The momentum book of the old study's tests: the top `n` names by their
/// return over `lookback`, at equal weight.
fn momentum(name: &str) -> String {
    momentum_with(name, "equity_default")
}

/// An execution block with no cost and no leverage limit, the last price
/// on a delisting: the old study's tests' frictionless executor, whose
/// one-name books survive the synthetic market's bankruptcy.
const FRICTIONLESS: &str = "{\n    price close\n    orders market\n    lot whole\n    capital 1_000_000 USD fixed\n    commission none\n    slippage none\n    impact none\n    participation none\n    financing none\n    leverage unconstrained\n    actions apply\n    delisting last_price\n    oversize halt\n    ruin halt\n    numerics running\n  }";

/// `momentum` under another execution block.
fn momentum_with(name: &str, execution: &str) -> String {
    common::StrategyParts {
        name: name.into(),
        needs: "need bars @1d   need listing   need actions".into(),
        params: "    lookback : Duration = 3mo in 1mo..1y\n    skip : Duration = 0d\n    n : Count = 2 in 1..5".into(),
        rules: "    rel momentum(A: Equity, @T, M: Scalar)\n    momentum(A, T, M) :- universe(A, T), lag(T, skip, T1), close(A, T1, P1), lag(T, lookback, T0), close(A, T0, P0), M = P1 / P0 - 1.\n    rel candidate(A: Equity, @T, M: Scalar)\n    candidate(A, T, M) :- universe(A, T), momentum(A, T, M).\n    rel selected(A: Equity, @T)\n    selected(A, T) :- bar(T), top[n](candidate(A, T, M) by (M desc, A asc)) within (T).\n    rel n_selected(@T, N: Count)\n    n_selected(T, N) :- bar(T), N = count(A) over (selected(A, T)) within (T), N > 0.\n".into(),
        decides: "    decide(T, hold(A, W)) :- selected(A, T), n_selected(T, N), W = 1 / N.\n    decide(T, hold(A, 0)) :- held(A, T, _), not selected(A, T).".into(),
        execution: execution.into(),
        ..Default::default()
    }
    .render()
}

/// The catalog, the library and the strategy's text, as the kernel takes
/// them.
fn program(src: &str, name: &str) -> Program {
    let mut sources: Vec<SourceFile> = common::dsl_files("catalog")
        .into_iter()
        .chain(common::dsl_files("lib"))
        .map(|p| SourceFile {
            text: std::fs::read_to_string(&p).unwrap(),
            path: Some(p.to_string_lossy().into_owned()),
        })
        .collect();
    sources.push(SourceFile {
        text: src.to_string(),
        path: Some(format!("{}.dsl", name)),
    });
    Program { sources, strategy: name.to_string() }
}

/// A data service holding the market and `n` kernel services reaching it,
/// in this process; the query layer's clients of them.
fn services(prog: &Checked, ds: &Dataset, n: usize) -> (Data, Vec<Kernel>) {
    let svc = abt::serve_dataset(ds, prog).unwrap();
    let data = Data::connect(abt_data::service::inproc(&svc)).unwrap();
    let kernels = (0..n)
        .map(|_| {
            let s = svc.clone();
            let k = KernelService::new(Arc::new(move || DataClient::connect(abt_data::service::inproc(&s))));
            Kernel::connect(abt_kernel::kernel::service::inproc(&k)).unwrap()
        })
        .collect();
    (data, kernels)
}

fn study(text: &str, name: &str) -> Plan {
    plan::plan(&plan::parse(text).unwrap(), name).unwrap()
}

fn run(p: &Plan, prog: &Program, data: &Data, kernels: &[Kernel], reveal: Option<&str>) -> Execution {
    let opts = Options {
        threads: 3,
        reveal: reveal.map(str::to_string),
        ..Options::default()
    };
    execute(p, prog, &Endpoints { data, kernels }, &opts).unwrap_or_else(|e| panic!("{}: {}", p.name, e))
}

fn json<T: serde::Serialize>(x: &T) -> String {
    serde_json::to_string(x).unwrap()
}

fn temp_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("abt-plan-{}-{}", tag, std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

#[test]
fn a_grid_plan_is_the_old_study_to_the_bit() {
    let src = momentum("studied");
    let prog = common::program(&src, "studied");
    let ds = synthetic_daily(&["AAA", "BBB", "CCC", "DDD", "SPY"], (2022, 1, 3), 700, 5);
    // The old study, in memory, with the program's own execution block.
    let dir = temp_dir("parity");
    let project = Project::open(&dir);
    let (spec, _) = project.declare(&prog, OldHoldout::Trailing { years: 1 }, "sharpe", vec![], ExecConfig::from_program(&prog)).unwrap();
    let axis = old::parse_grid_axis("n=1,2,3").unwrap();
    let grid = Grid::new(vec![axis], &[]);
    let (outcomes, report) = old::run_study(&project, &spec, &prog, &common::Sampled::new(&prog, &ds).sample(), &grid, &common::study_run, Provenance::default()).unwrap();
    // The plan, over the services.
    let (data, kernels) = services(&prog, &ds, 2);
    let p = study("study studied { program studied\n grid { n = 1, 2, 3 }\n holdout trailing:1y\n objective sharpe }", "studied");
    let ex = run(&p, &program(&src, "studied"), &data, &kernels, None);
    assert_eq!(ex.outcomes.len(), 3);
    assert_eq!(
        (ex.embargo.intervals.clone(), ex.embargo.truncates),
        (report.embargo.intervals.clone(), report.embargo.truncates),
        "the hold-out's embargo"
    );
    for (o, n) in outcomes.iter().zip(&ex.outcomes) {
        assert_eq!(json(&n.returns), json(&o.metrics), "the point {:?}", n.point);
        assert_eq!(json(&n.trading), json(&o.trading), "the point {:?}", n.point);
        assert_eq!(n.kind, Kind::Trial);
    }
    let g = &ex.grids[0];
    assert_eq!(g.best, report.best);
    assert_eq!(json(&g.pbo), json(&report.pbo), "PBO");
    assert_eq!(json(&g.surface), json(&report.surface), "the surface");
    assert_eq!(json(&g.subperiods), json(&report.subperiods), "the sub-periods");
    assert_eq!(ex.trials, 3);
    assert_eq!(json(&ex.dsr), json(&Some(report.dsr.clone())), "the deflated Sharpe ratio");
    // The trials are rows of the data layer, and the plan pins its binding.
    let counted = plan::lineage::trials(&data, &ex.lineage).unwrap();
    assert_eq!((counted.count, counted.by_kind.get("trial").copied()), (3, Some(3)));
    // Every run's binding (its point is in it) is the plan's, pinned.
    let plans = plan::trials::read_plans(&data).unwrap();
    assert!(ex.outcomes.iter().all(|o| plans[&ex.hash].bindings.contains(&o.binding)), "{:?}", plans[&ex.hash].bindings);
    let stored = plan::trials::outcomes(&data, &ex.hash).unwrap();
    assert_eq!(stored.iter().filter(|s| s.best).count(), 1);
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn a_walk_forward_plan_is_the_old_walk_forward() {
    let src = momentum_with("walked", FRICTIONLESS);
    let prog = common::program(&src, "walked");
    let ds = synthetic_daily(&["AAA", "BBB", "CCC", "DDD"], (2022, 1, 3), 520, 6);
    let dir = temp_dir("wf");
    let project = Project::open(&dir);
    let (spec, _) = project.declare(&prog, OldHoldout::None, "sharpe", vec![], ExecConfig::from_program(&prog)).unwrap();
    let grid = Grid::new(vec![old::parse_grid_axis("n=1,2").unwrap()], &[]);
    let scheme = old::WalkForward::parse("anchored:12mo:6mo").unwrap();
    let (_, report) = old::run_study(
        &project,
        &spec,
        &prog,
        &common::Sampled::new(&prog, &ds).sample(),
        &grid,
        &common::study_run,
        Provenance { bundle: None, scheme: Some(scheme) },
    )
    .unwrap();
    let wf_old = report.walk_forward.unwrap();
    let (data, kernels) = services(&prog, &ds, 1);
    let p = study("study walked { program walked\n grid { n = 1, 2 }\n walk_forward anchored:12mo:6mo }", "walked");
    assert_eq!(p.steps.iter().map(|s| s.kind()).collect::<Vec<_>>(), vec!["grid", "walk_forward", "report"]);
    let ex = run(&p, &program(&src, "walked"), &data, &kernels, None);
    let wf = &ex.walk_forwards[0];
    assert_eq!(json(&wf.folds), json(&wf_old.folds), "the folds");
    assert_eq!(json(&wf.efficiency), json(&wf_old.efficiency), "the efficiency");
    assert_eq!(json(&wf.out_of_sample), json(&wf_old.out_of_sample), "the stitched curve");
    // Two grid points, and per fold two train runs and one test: eight
    // trials, each a run of its own.
    assert_eq!(ex.outcomes.len(), 2 + 2 * 3);
    assert_eq!(ex.trials, 8);
    assert_eq!(ex.outcomes.iter().filter(|o| o.fold.as_ref().is_some_and(|f| f.phase == "test")).count(), 2);
    let ids: std::collections::BTreeSet<&str> = ex.outcomes.iter().map(|o| o.run_id.as_str()).collect();
    assert_eq!(ids.len(), 8, "π makes every fold's run a run of its own");
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn a_block_hold_out_masks_its_blocks_and_a_reveal_reports_them() {
    let src = momentum_with("blocked", FRICTIONLESS);
    let prog = common::program(&src, "blocked");
    let ds = synthetic_daily(&["AAA", "BBB", "CCC"], (2022, 1, 3), 500, 4);
    let (data, kernels) = services(&prog, &ds, 1);
    let text = "study blocked { program blocked\n holdout blocks:2:2mo:3\n steps { run main observe selected\n reveal final of main } }";
    let p = study(text, "blocked");
    let pg = program(&src, "blocked");
    let ex = run(&p, &pg, &data, &kernels, None);
    assert!(!ex.embargo.truncates);
    assert_eq!(ex.embargo.intervals.len(), 2);
    assert_eq!(ex.outcomes.len(), 1, "a plain execution reveals nothing");
    let o = &ex.outcomes[0];
    let run = abt_query::run::Run::read(&data, &o.run_id).unwrap();
    // The run crosses the blocks; its metrics leave them out.
    assert_eq!(run.book.len(), 500);
    let blocked = run.book.iter().filter(|b| ex.embargo.contains(b.t)).count();
    assert!(blocked > 20, "{}", blocked);
    assert_eq!(o.returns.n + 1, 500 - blocked);
    // Its ledger is written outside the blocks only.
    let ledger = abt_query::relation::read(&data, &format!("kernel.{}.ledger.selected", o.run_id)).unwrap().unwrap();
    let bars = abt_query::relation::ints(&ledger, "bar").unwrap();
    assert!(!bars.is_empty() && bars.iter().flatten().all(|t| !ex.embargo.contains(*t)));
    // The reveal reports the blocks only, as a reveal.
    let r = run_reveal(&p, &pg, &data, &kernels, "final");
    assert_eq!(r.outcomes.len(), 1);
    assert_eq!(r.outcomes[0].kind, Kind::Reveal);
    assert_eq!(r.outcomes[0].returns.n + 1, blocked);
    assert_eq!(r.outcomes[0].point, o.point);
    let counted = plan::lineage::trials(&data, &ex.lineage).unwrap();
    assert_eq!(counted.by_kind.get("reveal"), Some(&1));
    // A plan without a hold-out has nothing to reveal.
    let open = plan::parse("study open { program blocked\n steps { run main\n reveal final of main } }").unwrap();
    assert!(plan::plan(&open, "open").is_err_and(|e| e.contains("nothing to reveal")));
}

fn run_reveal(p: &Plan, prog: &Program, data: &Data, kernels: &[Kernel], step: &str) -> Execution {
    run(p, prog, data, kernels, Some(step))
}

#[test]
fn a_trailing_reveal_reports_the_embargoed_tail() {
    let src = momentum_with("tailed", FRICTIONLESS);
    let prog = common::program(&src, "tailed");
    let ds = synthetic_daily(&["AAA", "BBB", "CCC"], (2022, 1, 3), 600, 9);
    let (data, kernels) = services(&prog, &ds, 1);
    let p = study(
        "study tailed { program tailed\n holdout trailing:1y\n grid { n = 1, 2 }\n steps { grid main\n reveal final of best(main) } }",
        "tailed",
    );
    let pg = program(&src, "tailed");
    // A reveal before the plan ran names no committed point.
    let opts = Options {
        reveal: Some("final".into()),
        ..Options::default()
    };
    assert!(execute(&p, &pg, &Endpoints { data: &data, kernels: &kernels }, &opts).is_err_and(|e| e.to_string().contains("has not run step")));
    let ex = run(&p, &pg, &data, &kernels, None);
    let start = ex.embargo.start().unwrap();
    // Every run stops before the embargo.
    for o in &ex.outcomes {
        let run = abt_query::run::Run::read(&data, &o.run_id).unwrap();
        assert!(run.book.iter().all(|b| b.t < start), "{}", o.run_id);
    }
    let best = ex.outcomes[ex.grids[0].runs[ex.grids[0].best]].clone();
    let r = run_reveal(&p, &pg, &data, &kernels, "final");
    let revealed = &r.outcomes[0];
    assert_eq!(revealed.point, best.point, "the reveal runs the grid's best point");
    let run = abt_query::run::Run::read(&data, &revealed.run_id).unwrap();
    assert_eq!(run.book.len(), 600, "the reveal runs the whole sample");
    let tail = run.book.iter().filter(|b| b.t >= start).count();
    assert_eq!(revealed.returns.n + 1, tail);
}

#[test]
fn every_run_is_a_trial_counted_once_and_pi_is_in_its_id() {
    let src = momentum_with("counted", FRICTIONLESS);
    let prog = common::program(&src, "counted");
    let ds = synthetic_daily(&["AAA", "BBB", "CCC"], (2022, 1, 3), 300, 2);
    let (data, kernels) = services(&prog, &ds, 1);
    let pg = program(&src, "counted");
    let text = "study a { program counted\n grid { n = 1, 2 } }";
    let a = run(&study(text, "a"), &pg, &data, &kernels, None);
    assert_eq!(a.trials, 2);
    assert!(a.warnings.iter().any(|w| w.bias == "out-of-sample"), "{:?}", a.warnings);
    assert!(a.warnings.iter().any(|w| w.bias == "short-sample"), "{:?}", a.warnings);
    // The same plan again over the same binding: the same runs, no new trial.
    let again = run(&study(text, "a"), &pg, &data, &kernels, None);
    assert_eq!(again.outcomes.iter().map(|o| &o.run_id).collect::<Vec<_>>(), a.outcomes.iter().map(|o| &o.run_id).collect::<Vec<_>>());
    assert_eq!(again.trials, 2);
    // The plan renamed is the same plan: the same hash, the same runs.
    let renamed = run(&study(&text.replace("study a", "study b"), "b"), &pg, &data, &kernels, None);
    assert_eq!(renamed.hash, a.hash);
    assert_eq!(renamed.outcomes[0].run_id, a.outcomes[0].run_id);
    // Another plan's run of the same point is another run, and a trial.
    let other = run(&study("study c { program counted\n holdout trailing:1y\n grid { n = 1, 2 } }", "c"), &pg, &data, &kernels, None);
    assert_ne!(other.hash, a.hash);
    assert!(other.outcomes.iter().all(|o| !a.outcomes.iter().any(|x| x.run_id == o.run_id)));
    assert_eq!(other.trials, 4);
    // A run outside any plan counts too (Proposition Q-2.2).
    let r = abt_query::kernel::RunRequest {
        point: vec![("n".into(), "3".into())],
        ..Default::default()
    };
    let ran = kernels[0].run(&pg, &r).unwrap();
    assert_eq!(ran.status, "completed");
    let counted = plan::lineage::trials(&data, &a.lineage).unwrap();
    assert_eq!(counted.count, 5);
    assert_eq!(counted.by_kind.get("run"), Some(&1));
    // The plans pin their binding; dropping one keeps the pin the other
    // holds.
    let plans = plan::trials::read_plans(&data).unwrap();
    assert_eq!(plans.len(), 2);
    let dropped = plan::trials::drop_plan(&data, &a.hash).unwrap();
    assert!(a.outcomes.iter().all(|o| dropped.bindings.contains(&o.binding)));
    assert_eq!(plan::trials::read_plans(&data).unwrap().keys().collect::<Vec<_>>(), vec![&other.hash]);
    assert_eq!(plan::lineage::trials(&data, &a.lineage).unwrap().count, 5, "a dropped plan's trials stay");
}

#[test]
fn a_near_copy_joins_the_lineage_and_a_dispute_is_a_row() {
    let src = momentum_with("original", FRICTIONLESS);
    let prog = common::program(&src, "original");
    let ds = synthetic_daily(&["AAA", "BBB", "CCC"], (2022, 1, 3), 300, 2);
    let (data, kernels) = services(&prog, &ds, 1);
    let a = run(&study("study a { program original }", "a"), &program(&src, "original"), &data, &kernels, None);
    // One rule added: most rules are the original's.
    let copy = momentum_with("copy", FRICTIONLESS).replace(
        "    rel n_selected(@T, N: Count)",
        "    rel unused(@T, N: Count)\n    unused(T, N) :- bar(T), N = count(A) over (universe(A, T)) within (T).\n    rel n_selected(@T, N: Count)",
    );
    let b = run(&study("study b { program copy }", "b"), &program(&copy, "copy"), &data, &kernels, None);
    assert_ne!(b.program_hash, a.program_hash);
    assert_eq!(b.lineage, a.lineage, "the copy joins the original's lineage");
    assert!(b.warnings.iter().any(|w| w.bias == "data-snooping" && w.message.contains("original")), "{:?}", b.warnings);
    assert_eq!(b.trials, 2, "the lineage's trials count both");
    let d = plan::lineage::dispute(&data, &b.program_hash, "a different idea", 1).unwrap();
    assert_eq!(d.lineage, a.lineage);
    assert_eq!(plan::lineage::disputes(&data).unwrap(), vec![d]);
    assert!(plan::lineage::dispute(&data, "nowhere", "?", 2).is_err());
}

#[test]
fn one_kernel_or_four_give_the_same_outcomes() {
    let src = momentum_with("parallel", FRICTIONLESS);
    let prog = common::program(&src, "parallel");
    let ds = synthetic_daily(&["AAA", "BBB", "CCC", "DDD"], (2022, 1, 3), 400, 3);
    let p = study("study g { program parallel\n grid { n = 1, 2, 3; lookback = 2mo, 3mo } }", "g");
    let pg = program(&src, "parallel");
    let (d1, k1) = services(&prog, &ds, 1);
    let one = run(&p, &pg, &d1, &k1, None);
    let (d4, k4) = services(&prog, &ds, 4);
    let four = run(&p, &pg, &d4, &k4, None);
    assert_eq!(one.outcomes.len(), 6);
    for (a, b) in one.outcomes.iter().zip(&four.outcomes) {
        assert_eq!(a.run_id, b.run_id);
        assert_eq!(json(&a.returns), json(&b.returns));
        assert_eq!(json(&a.trading), json(&b.trading));
    }
    assert_eq!(json(&one.grids), json(&four.grids));
}
