//! The study API (data-bundle doc, sections 6 and 7): a program hash that
//! survives renaming, lineages that a revision declares or similarity
//! attaches, an append-only trial log, a declared study whose runs are
//! counted trials with the deflated Sharpe ratio over the lineage and PBO
//! over the grid, and a trailing hold-out that truncates every run.

mod corpus;

use std::path::PathBuf;
use std::process::Command;

use absolute_backtest::check::{check_program, Program};
use absolute_backtest::data::synthetic_daily;
use absolute_backtest::kernel::time::parse_timestamp;
use absolute_backtest::kernel::{run, ExecConfig};
use absolute_backtest::study::{
    block_embargo, build_report, grid_points, parse_grid_axis, program_hash, reveal, run_study, subperiod_sharpes, surface, trailing_embargo, Attachment, Grid, Holdout, Lineages, Project, Provenance,
    Threshold, TrialKind, WalkForward, BIASES,
};

fn program(src: &str, name: &str) -> Program {
    let mut ws = corpus::base_workspace();
    ws.add_source(src).unwrap();
    let (p, diags) = check_program(&ws, name);
    let text: Vec<String> = diags.iter().map(|d| d.to_string()).collect();
    p.unwrap_or_else(|| panic!("{} should check:\n{}", name, text.join("\n")))
}

fn temp_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("abt-study-{}-{}", tag, std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

fn momentum(name: &str, body: &str) -> String {
    format!(
        r#"
strategy {} {{
  env equities_1d
  uses features
  resolution @1d
  mode target
  param lookback : Duration = 3mo in 1mo..1y
  param skip : Duration = 0d
  param n : Count = 2 in 1..5
  rel candidate(-A: Equity, @T: Timestamp, -M: Scalar)
  candidate(A, T, M) :- universe(A, T), momentum(A, T, lookback, skip, M).
  rel selected(-A: Equity, @T: Timestamp)
  selected(A, T) :- bar(T), top(n, candidate(A, T, M), by (M desc, A asc)).
  rel n_selected(@T: Timestamp, -N: Count)
  n_selected(T, N) :- bar(T), N = count(A) over (selected(A, T)), N > 0.
  decide(T, target_weight(A, W)) :- selected(A, T), n_selected(T, N), W = 1 / N.
  decide(T, target_weight(A, 0)) :- held(A, T, _), not selected(A, T).
  {}
}}
"#,
        name, body
    )
}

// ---- hashing and lineage ----

#[test]
fn the_program_hash_ignores_the_name_and_variable_names_but_not_a_constant() {
    let a = program(&momentum("alpha", ""), "alpha");
    let b = program(&momentum("beta", ""), "beta");
    assert_eq!(program_hash(&a), program_hash(&b));
    let renamed = momentum("gamma", "").replace(
        "candidate(A, T, M) :- universe(A, T), momentum(A, T, lookback, skip, M).",
        "candidate(X, T, Mom) :- universe(X, T), momentum(X, T, lookback, skip, Mom).",
    );
    let c = program(&renamed, "gamma");
    assert_eq!(program_hash(&a), program_hash(&c));
    let d = program(&momentum("delta", "").replace("skip : Duration = 0d", "skip : Duration = 1d"), "delta");
    assert_ne!(program_hash(&a), program_hash(&d));
    assert_eq!(program_hash(&a).len(), 16);
}

#[test]
fn a_revision_joins_its_lineage_and_a_near_copy_is_attached_by_similarity() {
    let root = program(&momentum("root", ""), "root");
    let mut ls = Lineages::default();
    let o = ls.open(&root, 1).unwrap();
    assert_eq!(o.attachment, Attachment::Root);
    assert_eq!(o.lineage, program_hash(&root));
    // Opening it again finds the member.
    assert!(ls.open(&root, 2).unwrap().existing);
    // A declared revision.
    let src = momentum("v2", "")
        .replace("mode target", &format!("mode target\n  revises \"{}\"", program_hash(&root)))
        .replace("n : Count = 2", "n : Count = 3");
    let v2 = program(&src, "v2");
    let o = ls.open(&v2, 3).unwrap();
    assert_eq!(o.lineage, program_hash(&root));
    assert_eq!(o.attachment, Attachment::Revises(program_hash(&root)));
    assert!(o.warning.is_none());
    // A copy under another name with one extra rule: five of six rules shared.
    let near = program(&momentum("fresh_idea", "rel extra(@T: Timestamp)\n  extra(T) :- bar(T), n_selected(T, _)."), "fresh_idea");
    let o = ls.open(&near, 4).unwrap();
    assert_eq!(o.lineage, program_hash(&root));
    assert!(matches!(o.attachment, Attachment::Similarity { score, .. } if score > 0.8), "{:?}", o.attachment);
    assert!(o.warning.as_deref().unwrap().contains("similarity"));
    // Something else entirely is its own lineage.
    let other = program(include_str!("../corpus/strategies/sma_crossover.dsl"), "sma_crossover");
    let o = ls.open(&other, 5).unwrap();
    assert_eq!(o.attachment, Attachment::Root);
    assert_eq!(ls.lineages.len(), 2);
    // A dispute is recorded and changes nothing.
    ls.dispute(&program_hash(&near), "a different idea", 6).unwrap();
    assert_eq!(ls.lineages[0].disputes.len(), 1);
    assert_eq!(ls.lineages[0].members.len(), 3);
    assert!(ls.dispute("0000", "?", 7).is_err());
    // An unknown revision target is an error.
    let src = momentum("orphan", "")
        .replace("mode target", "mode target\n  revises \"deadbeefdeadbeef\"")
        .replace("n : Count = 2", "n : Count = 4");
    let orphan = program(&src, "orphan");
    assert!(ls.open(&orphan, 8).is_err());
}

#[test]
fn lineages_round_trip_through_their_file() {
    let dir = temp_dir("lineages");
    let project = Project::open(&dir);
    let root = program(&momentum("root", ""), "root");
    let o = project.open_lineage(&root).unwrap();
    let ls = project.lineages().unwrap();
    assert_eq!(ls.lineages.len(), 1);
    assert_eq!(ls.lineages[0].id, o.lineage);
    assert!(project.open_lineage(&root).unwrap().existing);
    std::fs::remove_dir_all(&dir).unwrap();
}

// ---- declare and run ----

#[test]
fn grid_axes_parse_and_expand() {
    let a = parse_grid_axis("lookback=1mo, 3mo,6mo").unwrap();
    assert_eq!(a.0, "lookback");
    assert_eq!(a.1.len(), 3);
    let b = parse_grid_axis("n=1,2").unwrap();
    let points = grid_points(&[a, b]);
    assert_eq!(points.len(), 6);
    assert_eq!(points[0].len(), 2);
    assert_eq!(grid_points(&[]).len(), 1);
    assert!(parse_grid_axis("lookback").is_err());
    assert!(parse_grid_axis("n=").is_err());
    assert!(Threshold::parse("sharpe>=1").is_ok());
    assert!(Threshold::parse("max_drawdown<=0.2").is_ok());
    assert!(Threshold::parse("beauty>=1").is_err());
    assert!(Holdout::parse("trailing:2y").is_ok());
    assert!(Holdout::parse("trailing:0y").is_err());
    assert!(Holdout::parse("sometimes").is_err());
}

#[test]
fn a_study_counts_its_trials_deflates_by_them_and_embargoes_the_hold_out() {
    let dir = temp_dir("run");
    let project = Project::open(&dir);
    let prog = program(&momentum("studied", ""), "studied");
    let (spec, opened) = project
        .declare(
            &prog,
            Holdout::Trailing { years: 1 },
            "sharpe",
            vec![Threshold::parse("sharpe>=0.5").unwrap()],
            ExecConfig {
                compounding: true,
                ..ExecConfig::default()
            },
        )
        .unwrap();
    assert_eq!(spec.lineage, opened.lineage);
    assert!(spec.warnings.iter().any(|w| w.bias == "cash-management"), "{:?}", spec.warnings);
    assert!(!spec.warnings.iter().any(|w| w.bias == "out-of-sample"));
    let ds = synthetic_daily(&["AAA", "BBB", "CCC", "DDD", "SPY"], (2022, 1, 3), 700, 5);
    let axis = parse_grid_axis("n=1,2,3").unwrap();
    let points = grid_points(std::slice::from_ref(&axis));
    let (outcomes, report) = run_study(
        &project,
        &spec,
        &prog,
        &ds,
        &Grid {
            axes: vec![axis.clone()],
            points: points.clone(),
        },
        &run,
        Provenance::default(),
    )
    .unwrap();
    assert_eq!(outcomes.len(), 3);
    assert_eq!(report.trials, 3);
    assert_eq!(report.dsr.trials, 3);
    assert!(report.pbo.is_some(), "a three-point grid has a PBO");
    let sf = report.surface.as_ref().expect("a three-point axis has a surface");
    assert!(sf.neighbours >= 1 && sf.neighbours <= 2);
    assert!(!report.subperiods.is_empty());
    // The hold-out: every run stops a year before the sample's end.
    let last = ds.facts["close"].iter().filter_map(|tu| tu[1].as_time()).max().unwrap();
    let embargo = report.embargoed_from().unwrap();
    assert!(embargo < last && embargo > parse_timestamp("2023-06-01").unwrap(), "{}", embargo);
    for o in &outcomes {
        assert!(*o.result.bars.last().unwrap() < embargo);
        assert_eq!(o.trial.kind, TrialKind::Trial);
        assert_eq!(o.trial.holdout, "trailing:1y");
        assert_eq!(o.trial.study.as_deref(), Some(spec.id.as_str()));
    }
    let n_values: Vec<&str> = outcomes.iter().map(|o| o.trial.params["n"].as_str()).collect();
    assert_eq!(n_values, vec!["1", "2", "3"]);
    assert_eq!(outcomes[0].trial.params["lookback"], "3mo");
    // A second run adds to the count: five trials, and the deflation grows.
    let (more, report2) = run_study(
        &project,
        &spec,
        &prog,
        &ds,
        &Grid {
            axes: vec![],
            points: points[..2].to_vec(),
        },
        &run,
        Provenance::default(),
    )
    .unwrap();
    assert_eq!(more.len(), 2);
    assert_eq!(report2.trials, 5);
    let log = project.log().read_all().unwrap();
    assert_eq!(log.iter().map(|t| t.seq).collect::<Vec<_>>(), vec![1, 2, 3, 4, 5]);
    // The declared study is found again by lineage.
    let found = project.study_for(&spec.lineage).unwrap().unwrap();
    assert_eq!(found.id, spec.id);
    // A strategy of another lineage cannot run under this study.
    let other = program(include_str!("../corpus/strategies/sma_crossover.dsl"), "sma_crossover");
    assert!(run_study(&project, &spec, &other, &ds, &Grid::single(&[]), &run, Provenance::default()).is_err());
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn a_study_without_a_hold_out_is_warned_and_runs_the_whole_sample() {
    let dir = temp_dir("nohold");
    let project = Project::open(&dir);
    let prog = program(&momentum("whole", ""), "whole");
    let (spec, _) = project.declare(&prog, Holdout::None, "cagr", vec![], ExecConfig::frictionless()).unwrap();
    assert!(spec.warnings.iter().any(|w| w.bias == "out-of-sample"));
    assert!(spec.warnings.iter().any(|w| w.bias == "transaction-cost neglect" || w.bias == "slippage"), "{:?}", spec.warnings);
    let ds = synthetic_daily(&["AAA", "BBB", "CCC"], (2022, 1, 3), 300, 2);
    let (outcomes, report) = run_study(&project, &spec, &prog, &ds, &Grid::single(&[]), &run, Provenance::default()).unwrap();
    assert_eq!(outcomes.len(), 1);
    assert!(report.embargo.is_empty());
    assert_eq!(outcomes[0].result.bars.len(), 300);
    assert!(report.pbo.is_none());
    assert!(report.warnings.iter().any(|w| w.bias == "short-sample"), "{:?}", report.warnings);
    std::fs::remove_dir_all(&dir).unwrap();
}

// ---- the command line ----

fn abt(args: &[&str]) -> (i32, String, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_abt")).args(args).current_dir(env!("CARGO_MANIFEST_DIR")).output().expect("abt runs");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

#[test]
fn the_command_line_declares_runs_and_lists_a_study() {
    let dir = temp_dir("cli");
    let d = dir.to_string_lossy().into_owned();
    let (code, out, err) = abt(&[
        "study",
        "declare",
        "--study",
        &d,
        "--strategy",
        "momentum_top_n",
        "--holdout",
        "trailing:1y",
        "--require",
        "sharpe>=0.5",
        "corpus/",
    ]);
    assert_eq!(code, 0, "{}\n{}", out, err);
    assert!(out.contains("declared") && out.contains("trailing:1y") && out.contains("sharpe >= 0.5"), "{}", out);
    let (code, out, err) = abt(&[
        "study",
        "run",
        "--study",
        &d,
        "--strategy",
        "momentum_top_n",
        "--synthetic",
        "--days",
        "600",
        "--grid",
        "n=1,2",
        "corpus/",
    ]);
    assert_eq!(code, 0, "{}\n{}", out, err);
    assert!(out.contains("point 1/2 [n=1]") && out.contains("point 2/2 [n=2]"), "{}", out);
    assert!(out.contains("2 trials") && out.contains("deflated Sharpe") && out.contains("embargoed"), "{}", out);
    assert!(out.contains("probability of backtest overfitting"), "{}", out);
    let (code, out, _) = abt(&["study", "metrics", "--study", &d, "--strategy", "momentum_top_n", "corpus/"]);
    assert_eq!(code, 0);
    assert!(out.contains("#1 Trial") && out.contains("#2 Trial") && out.trim_end().ends_with("2 trials"), "{}", out);
    // A plain run naming the study is logged as untracked; without it, warned.
    let (code, out, _) = abt(&["run", "--strategy", "momentum_top_n", "--synthetic", "--quiet", "--study", &d, "corpus/"]);
    assert_eq!(code, 0);
    assert!(out.contains("untracked trial #3"), "{}", out);
    let (_, out, _) = abt(&["run", "--strategy", "momentum_top_n", "--synthetic", "--quiet", "corpus/"]);
    assert!(out.contains("untracked trial"), "{}", out);
    // A reveal of the committed version reports the hold-out as an out-of-sample trial.
    let (code, out, err) = abt(&["study", "reveal", "--study", &d, "--strategy", "momentum_top_n", "--synthetic", "--days", "600", "corpus/"]);
    assert_eq!(code, 0, "{}\n{}", out, err);
    assert!(out.contains("out of sample:") && out.contains("sharpe >= 0.5") && out.contains("1 reveal(s)"), "{}", out);
    let (code, out, err) = abt(&[
        "study",
        "run",
        "--study",
        &d,
        "--strategy",
        "momentum_top_n",
        "--synthetic",
        "--days",
        "600",
        "--grid",
        "n=1,2",
        "--walk-forward",
        "rolling:6mo:3mo",
        "corpus/",
    ]);
    assert_eq!(code, 0, "{}\n{}", out, err);
    assert!(out.contains("walk-forward rolling:6mo:3mo") && out.contains("fold 1:") && out.contains("efficiency"), "{}", out);
    assert!(out.contains("parameter surface") && out.contains("sharpe by year"), "{}", out);
    let (code, _, err) = abt(&["study", "run", "--study", &d, "--strategy", "momentum_top_n", "--synthetic", "--walk-forward", "never", "corpus/"]);
    assert_eq!(code, 2);
    assert!(err.contains("--walk-forward"), "{}", err);
    // Running a strategy with no declared study is refused.
    let (code, _, err) = abt(&["study", "run", "--study", &d, "--strategy", "sma_crossover", "--synthetic", "corpus/"]);
    assert_eq!(code, 2);
    assert!(err.contains("no study is declared"), "{}", err);
    let (code, _, err) = abt(&["study", "declare", "--study", &d, "--strategy", "sma_crossover", "--holdout", "weekly", "corpus/"]);
    assert_eq!(code, 2);
    assert!(err.contains("--holdout"), "{}", err);
    std::fs::remove_dir_all(&dir).unwrap();
}

// ---- validation schemes ----

fn ts(s: &str) -> i64 {
    parse_timestamp(s).unwrap()
}

#[test]
fn embargoes_are_month_aligned_and_seeded() {
    let e = trailing_embargo(ts("2024-06-14"), 2);
    assert!(e.truncates);
    assert_eq!(e.intervals, vec![(ts("2022-06-14"), i64::MAX)]);
    assert!(e.contains(ts("2023-01-01")) && !e.contains(ts("2022-06-13")));
    let b = block_embargo(ts("2022-01-03"), ts("2024-12-31"), 3, 2, 7);
    assert!(!b.truncates);
    assert_eq!(b.intervals.len(), 3);
    for (a, z) in &b.intervals {
        assert!(*a >= ts("2022-01-01") && *z <= ts("2025-01-01"));
        assert_eq!(
            absolute_backtest::kernel::time::format_timestamp(*a).split('-').nth(2),
            Some("01"),
            "block starts on the first of a month"
        );
    }
    for w in b.intervals.windows(2) {
        assert!(w[0].1 <= w[1].0, "blocks do not overlap: {:?}", b.intervals);
    }
    assert_eq!(b, block_embargo(ts("2022-01-03"), ts("2024-12-31"), 3, 2, 7));
    assert_ne!(b, block_embargo(ts("2022-01-03"), ts("2024-12-31"), 3, 2, 8));
    assert!(Holdout::parse("blocks:4:3mo").is_ok());
    assert!(Holdout::parse("blocks:0:3mo").is_err());
}

#[test]
fn walk_forward_folds_follow_the_scheme() {
    let anchored = WalkForward::parse("anchored:12mo:3mo").unwrap();
    let folds = anchored.folds(ts("2022-01-03"), ts("2023-12-29"));
    // Train from January 2022; test windows of three months: four fit in 2023.
    assert_eq!(folds.len(), 4, "{:?}", folds);
    assert_eq!(folds[0], (ts("2022-01-01"), ts("2023-01-01"), ts("2023-01-01"), ts("2023-04-01")));
    assert_eq!(folds[3], (ts("2022-01-01"), ts("2023-10-01"), ts("2023-10-01"), ts("2024-01-01")));
    let rolling = WalkForward::parse("rolling:1y:6mo").unwrap();
    let folds = rolling.folds(ts("2022-01-03"), ts("2023-12-29"));
    assert_eq!(folds.len(), 2);
    assert_eq!(folds[1], (ts("2022-07-01"), ts("2023-07-01"), ts("2023-07-01"), ts("2024-01-01")));
    assert!(WalkForward::parse("anchored:2y").is_err());
    assert!(WalkForward::parse("sideways:2y:6mo").is_err());
    assert_eq!(WalkForward::parse("rolling:2y:6mo").unwrap().describe(), "rolling:24mo:6mo");
}

#[test]
fn the_surface_measures_a_peak_against_a_plateau() {
    let axis = parse_grid_axis("n=1,2,3").unwrap();
    let s = surface(std::slice::from_ref(&axis), &[1.0, 1.05, 0.2], 1).unwrap();
    assert_eq!(s.neighbours, 2);
    assert!((s.stability - 0.5).abs() < 1e-12);
    assert!((s.smoothness - (0.05 + 0.85) / 2.0 / 0.85).abs() < 1e-12, "{}", s.smoothness);
    let flat = surface(std::slice::from_ref(&axis), &[1.0, 1.02, 0.98], 1).unwrap();
    assert_eq!(flat.stability, 1.0);
    assert!(surface(&[axis], &[1.0, 2.0], 0).is_none(), "a mismatched grid has no surface");
    let two = [parse_grid_axis("a=1,2").unwrap(), parse_grid_axis("b=1,2,3").unwrap()];
    let s = surface(&two, &[1.0, 1.0, 1.0, 1.0, 5.0, 1.0], 4).unwrap();
    assert_eq!(s.neighbours, 3, "the middle of the second row has three neighbours");
    assert_eq!(s.stability, 0.0);
}

#[test]
fn sub_period_sharpes_are_per_calendar_year() {
    let mut c = Vec::new();
    let mut e = 100.0;
    let days = absolute_backtest::data::synthetic_daily(&["AAA"], (2022, 1, 3), 520, 1);
    let mut bars: Vec<i64> = days.facts["close"].iter().filter_map(|tu| tu[1].as_time()).collect();
    bars.sort();
    for (i, t) in bars.iter().enumerate() {
        // Drifting up through 2022 and down from 2023 on, with a wobble so
        // that each year has a volatility of its own.
        let year = absolute_backtest::kernel::time::month_key(*t).0;
        let drift = if year == 2022 { 0.001 } else { -0.001 };
        e *= 1.0 + drift + 0.002 * (i as f64).sin();
        c.push((*t, e));
    }
    let s = subperiod_sharpes(&c, 252.0, 20);
    // 2024 holds too few bars for a Sharpe ratio of its own.
    assert_eq!(s.iter().map(|(y, _)| *y).collect::<Vec<_>>(), vec![2022, 2023]);
    assert!(s[0].1 > 0.0 && s[1].1 < 0.0, "{:?}", s);
}

#[test]
fn a_block_hold_out_withholds_its_metrics_and_a_reveal_reports_them() {
    let dir = temp_dir("blocks");
    let project = Project::open(&dir);
    let prog = program(&momentum("blocked", ""), "blocked");
    let (spec, _) = project
        .declare(&prog, Holdout::Blocks { count: 2, months: 2, seed: 3 }, "sharpe", vec![], ExecConfig::frictionless())
        .unwrap();
    let ds = synthetic_daily(&["AAA", "BBB", "CCC"], (2022, 1, 3), 500, 4);
    let (outcomes, report) = run_study(&project, &spec, &prog, &ds, &Grid::single(&[]), &run, Provenance::default()).unwrap();
    assert!(!report.embargo.truncates);
    assert_eq!(report.embargo.intervals.len(), 2);
    let o = &outcomes[0];
    // The run crosses the blocks; the metrics leave them out.
    assert_eq!(o.result.bars.len(), 500);
    let blocked = o.result.bars.iter().filter(|t| report.embargo.contains(**t)).count();
    assert!(blocked > 20, "{}", blocked);
    assert_eq!(o.metrics.n + 1, 500 - blocked);
    assert!(o.trial.note.as_deref().unwrap().contains("exclude the embargoed blocks"));
    // The reveal reports the blocks only, as an out-of-sample trial.
    let (r, embargo) = reveal(&project, &spec, &prog, &ds, &run, Provenance::default()).unwrap();
    assert_eq!(embargo, report.embargo);
    assert_eq!(r.metrics.n + 1, blocked);
    assert_eq!(r.trial.kind, TrialKind::Reveal);
    assert_eq!(r.trial.seq, 2);
    // A version the study never ran cannot be revealed; nor can a study without a hold-out.
    let stranger = program(&momentum("stranger", "").replace("n : Count = 2", "n : Count = 4"), "stranger");
    assert!(reveal(&project, &spec, &stranger, &ds, &run, Provenance::default()).is_err());
    let (open_spec, _) = project.declare(&prog, Holdout::None, "sharpe", vec![], ExecConfig::frictionless()).unwrap();
    match reveal(&project, &open_spec, &prog, &ds, &run, Provenance::default()) {
        Err(e) => assert!(e.contains("nothing to reveal"), "{}", e),
        Ok(_) => panic!("a study without a hold-out revealed something"),
    }
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn a_trailing_reveal_reports_the_embargoed_tail() {
    let dir = temp_dir("tail");
    let project = Project::open(&dir);
    let prog = program(&momentum("tailed", ""), "tailed");
    let (spec, _) = project
        .declare(
            &prog,
            Holdout::Trailing { years: 1 },
            "sharpe",
            vec![Threshold::parse("max_drawdown<=0.9").unwrap()],
            ExecConfig::frictionless(),
        )
        .unwrap();
    let ds = synthetic_daily(&["AAA", "BBB", "CCC"], (2022, 1, 3), 600, 9);
    let (outcomes, report) = run_study(&project, &spec, &prog, &ds, &Grid::single(&[]), &run, Provenance::default()).unwrap();
    let start = report.embargoed_from().unwrap();
    let (r, _) = reveal(&project, &spec, &prog, &ds, &run, Provenance::default()).unwrap();
    assert_eq!(r.result.bars.len(), 600, "the reveal runs the whole sample");
    let tail = r.result.bars.iter().filter(|t| **t >= start).count();
    assert_eq!(r.metrics.n + 1, tail);
    assert_eq!(outcomes[0].result.bars.len() + tail, 600);
    assert_eq!(spec.thresholds[0].holds(&r.metrics, &r.trading), Some(true));
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn a_walk_forward_picks_per_fold_and_logs_every_evaluation() {
    let dir = temp_dir("wf");
    let project = Project::open(&dir);
    let prog = program(&momentum("walked", ""), "walked");
    let (spec, _) = project.declare(&prog, Holdout::None, "sharpe", vec![], ExecConfig::frictionless()).unwrap();
    let ds = synthetic_daily(&["AAA", "BBB", "CCC", "DDD"], (2022, 1, 3), 520, 6);
    let grid = Grid::new(vec![parse_grid_axis("n=1,2").unwrap()], &[]);
    let scheme = WalkForward::parse("anchored:12mo:6mo").unwrap();
    let (outcomes, report) = run_study(&project, &spec, &prog, &ds, &grid, &run, Provenance { bundle: None, scheme: Some(scheme) }).unwrap();
    assert_eq!(outcomes.len(), 2);
    let wf = report.walk_forward.as_ref().unwrap();
    // 520 bars from January 2022 end in December 2023: one twelve-month
    // train window, then two six-month test windows in 2023.
    assert_eq!(wf.folds.len(), 2, "{:?}", wf.folds);
    assert_eq!(wf.folds[0].test, (ts("2023-01-01"), ts("2023-07-01")));
    assert_eq!(wf.folds[1].train.0, ts("2022-01-01"), "anchored folds share their start");
    assert!(wf.folds.iter().all(|f| f.best < 2));
    assert!(wf.out_of_sample.n > 200, "{}", wf.out_of_sample.n);
    // Two grid points, plus per fold two train evaluations and one test: eight trials.
    let log = project.log().read_all().unwrap();
    assert_eq!(log.len(), 2 + 2 * 3);
    assert_eq!(log.iter().filter(|t| t.note.as_deref().map(|n| n.ends_with("test")).unwrap_or(false)).count(), 2);
    assert!(log.iter().skip(2).all(|t| t.scheme.as_deref() == Some("anchored:12mo:6mo")));
    assert_eq!(report.trials, 8);
    std::fs::remove_dir_all(&dir).unwrap();
}

// ---- the report ----

#[test]
fn the_report_counts_trials_reveals_and_marks_the_biases_warned() {
    let dir = temp_dir("report");
    let project = Project::open(&dir);
    let src = momentum("reported", "");
    let mut ws = corpus::base_workspace();
    ws.add_source(&src).unwrap();
    let (prog, diags) = check_program(&ws, "reported");
    let prog = prog.unwrap();
    // Nothing to report before a lineage exists.
    assert!(build_report(&project, &prog, &diags).is_err());
    let (spec, _) = project
        .declare(
            &prog,
            Holdout::Trailing { years: 1 },
            "sharpe",
            vec![Threshold::parse("max_drawdown<=0.9").unwrap()],
            ExecConfig::default(),
        )
        .unwrap();
    let ds = synthetic_daily(&["AAA", "BBB", "CCC"], (2022, 1, 3), 600, 9);
    let grid = Grid::new(vec![parse_grid_axis("n=1,2").unwrap()], &[]);
    run_study(&project, &spec, &prog, &ds, &grid, &run, Provenance::default()).unwrap();
    let r = run(&prog, &ds, ExecConfig::default()).unwrap();
    absolute_backtest::study::log_untracked(&project, &prog, &r, &ExecConfig::default(), None).unwrap();
    reveal(&project, &spec, &prog, &ds, &run, Provenance::default()).unwrap();
    let report = build_report(&project, &prog, &diags).unwrap();
    assert_eq!(report.trials, 4);
    assert_eq!(report.study_trials, 2);
    assert_eq!(report.untracked, 1);
    assert_eq!(report.reveals.len(), 1);
    assert_eq!(report.runs, 2, "one study run and one reveal");
    assert_eq!(report.members.len(), 1);
    assert!(report.dsr.is_some());
    assert_eq!(report.biases.len(), 40);
    assert_eq!(BIASES.iter().filter(|b| b.audit == 'C').count(), 12);
    // The executor's cash warning was raised on every run; its row is marked.
    let cash = report.biases.iter().find(|b| b.name == "cash-management").unwrap();
    assert_eq!(cash.raised.len(), 1, "{:?}", cash);
    assert!(report.warnings.iter().any(|w| w.bias == "cash-management" && w.times >= 4), "{:?}", report.warnings);
    assert!(report.warnings.iter().any(|w| w.bias == "short-sample"), "{:?}", report.warnings);
    let look = report.biases.iter().find(|b| b.name == "look-ahead").unwrap();
    assert!(look.raised.is_empty() && look.status.starts_with("guaranteed"));
    assert_eq!(report.thresholds.len(), 1);
    assert_eq!(report.thresholds[0].on_latest_reveal, Some(true));
    assert!(report.degrees_of_freedom.starts_with("degrees of freedom: 3 params"));
    let text = report.render();
    assert!(text.contains("trials: 4 (2 in studies, 1 untracked, 1 reveal(s)); study runs: 2"), "{}", text);
    assert!(text.contains("reveal #4") && text.contains("<- raised") && text.contains("B cash-management"), "{}", text);
    assert!(text.contains("threshold max_drawdown <= 0.9: latest trial holds; latest reveal holds"), "{}", text);
    // The command line prints the same report.
    let d = dir.to_string_lossy().into_owned();
    std::fs::write(dir.join("reported.dsl"), &src).unwrap();
    let files = [
        format!("{}/corpus/env", env!("CARGO_MANIFEST_DIR")),
        format!("{}/corpus/lib", env!("CARGO_MANIFEST_DIR")),
        dir.join("reported.dsl").to_string_lossy().into_owned(),
    ];
    let (code, out, err) = abt(&["study", "report", "--study", &d, "--strategy", "reported", &files[0], &files[1], &files[2]]);
    assert_eq!(code, 0, "{}\n{}", out, err);
    assert!(out.contains("study report: reported") && out.contains("bias audit"), "{}", out);
    std::fs::remove_dir_all(&dir).unwrap();
}
