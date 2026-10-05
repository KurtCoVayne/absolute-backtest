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
use absolute_backtest::study::{grid_points, parse_grid_axis, program_hash, run_study, Attachment, Holdout, Lineages, Project, Provenance, Threshold, TrialKind};

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
        .declare(&prog, Holdout::Trailing { years: 1 }, "sharpe", vec![Threshold::parse("sharpe>=0.5").unwrap()], ExecConfig::default())
        .unwrap();
    assert_eq!(spec.lineage, opened.lineage);
    assert!(spec.warnings.iter().any(|w| w.bias == "cash-management"), "{:?}", spec.warnings);
    assert!(!spec.warnings.iter().any(|w| w.bias == "out-of-sample"));
    let ds = synthetic_daily(&["AAA", "BBB", "CCC", "DDD", "SPY"], (2022, 1, 3), 700, 5);
    let axis = parse_grid_axis("n=1,2,3").unwrap();
    let points = grid_points(&[axis]);
    let (outcomes, report) = run_study(&project, &spec, &prog, &ds, &points, &run, Provenance::default()).unwrap();
    assert_eq!(outcomes.len(), 3);
    assert_eq!(report.trials, 3);
    assert_eq!(report.dsr.trials, 3);
    assert!(report.pbo.is_some(), "a three-point grid has a PBO");
    // The hold-out: every run stops a year before the sample's end.
    let last = ds.facts["close"].iter().filter_map(|tu| tu[1].as_time()).max().unwrap();
    let embargo = report.embargoed_from.unwrap();
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
    let (more, report2) = run_study(&project, &spec, &prog, &ds, &points[..2], &run, Provenance::default()).unwrap();
    assert_eq!(more.len(), 2);
    assert_eq!(report2.trials, 5);
    let log = project.log().read_all().unwrap();
    assert_eq!(log.iter().map(|t| t.seq).collect::<Vec<_>>(), vec![1, 2, 3, 4, 5]);
    // The declared study is found again by lineage.
    let found = project.study_for(&spec.lineage).unwrap().unwrap();
    assert_eq!(found.id, spec.id);
    // A strategy of another lineage cannot run under this study.
    let other = program(include_str!("../corpus/strategies/sma_crossover.dsl"), "sma_crossover");
    assert!(run_study(&project, &spec, &other, &ds, &grid_points(&[]), &run, Provenance::default()).is_err());
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
    let (outcomes, report) = run_study(&project, &spec, &prog, &ds, &grid_points(&[]), &run, Provenance::default()).unwrap();
    assert_eq!(outcomes.len(), 1);
    assert!(report.embargoed_from.is_none());
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
    // Running a strategy with no declared study is refused.
    let (code, _, err) = abt(&["study", "run", "--study", &d, "--strategy", "sma_crossover", "--synthetic", "corpus/"]);
    assert_eq!(code, 2);
    assert!(err.contains("no study is declared"), "{}", err);
    let (code, _, err) = abt(&["study", "declare", "--study", &d, "--strategy", "sma_crossover", "--holdout", "weekly", "corpus/"]);
    assert_eq!(code, 2);
    assert!(err.contains("--holdout"), "{}", err);
    std::fs::remove_dir_all(&dir).unwrap();
}
