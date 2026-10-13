//! The verification tiers (compiler design, section 6): tier 2's exact
//! linear feasibility with the margin of 6.8 licensing R2 inside
//! `compile`; tier 3's SMT-LIB2 scripts for guard disjointness, conflict
//! freedom, rewrite equivalence and nonlinear feasibility, written as
//! replay files and driven through a solver subprocess (a stand-in
//! solver here, since the build bundles none); tier 4's CHC encoding and
//! the refinement a proved invariant makes; tiers 5 and 6 on a synthetic
//! binding; the report's text and JSON.

mod common;

use std::fs;
use std::path::PathBuf;

use absolute_backtest::analysis::Keyedness;
use absolute_backtest::check::{Diagnostic, Program};
use absolute_backtest::compiler::{compile, CompileOptions, Stage};
use absolute_backtest::data::synthetic_daily;
use absolute_backtest::kernel::ExecConfig;
use absolute_backtest::verify::{verify, Verdict, VerifyInputs};

fn program(src: &str, name: &str) -> (Program, Vec<Diagnostic>) {
    let (p, diags) = common::check(src, name);
    (p.unwrap_or_else(|| panic!("{} should check:\n{}", name, common::text(&diags))), diags)
}

fn corpus_program(sub: &str, name: &str) -> (Program, Vec<Diagnostic>) {
    program(&common::corpus_source(sub, name), name)
}

/// A daily strategy named `v` with ten-share lots around the given
/// parameters, rules and decide rules.
fn with_rules(params: &str, rules: &str, decides: &str) -> String {
    let params = format!("    qty : Quantity<Shares> = 10 shares\n{}", params);
    common::strategy("v", &params, rules, decides)
}

fn scratch(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("abt-verify-test-{}-{}", name, std::process::id()));
    let _ = fs::remove_dir_all(&d);
    fs::create_dir_all(&d).unwrap();
    d
}

/// Section 6.3's example: `R > R + delta` with `delta ∈ [0.01, 0.1]` is
/// infeasible over the reals beyond the margin, which the non-relational
/// abstract interpretation cannot see; tier 2 sees it inside `compile`
/// and R2 removes the rule.
#[test]
fn tier2_decides_the_example_of_section_6_3_and_licenses_r2() {
    let src = with_rules(
        "    delta : Scalar = 0.05 in 0.01..0.1",
        r#"
    rel dead(A: Equity, @T)
    dead(A, T) :- universe(A, T), log_price_return(A, T, R), R > R + delta.
"#,
        "    decide(T, trade(A, qty)) :- dead(A, T), flat(A, T).",
    );
    let (p, _) = program(&src, "v");
    let ri = (0..p.rules.len()).find(|&i| p.rule_label(i) == "v::dead#1").unwrap();
    let compiled = compile(&p, &CompileOptions::default());
    assert!(
        compiled.analysis.never_true.iter().all(|n| n.rule != ri),
        "the abstract interpretation is non-relational: {:?}",
        compiled.analysis.never_true
    );
    let t2 = compiled.tiers.iter().find(|t| t.tier == 2).expect("tier 2 runs in compile");
    let o = t2.obligations.iter().find(|o| o.rule_index == Some(ri)).expect("an obligation for the rule");
    assert_eq!(o.verdict, Verdict::Infeasible, "{:?}", o);
    assert_eq!(o.licenses.as_deref(), Some("R2"), "{:?}", o);
    assert!(o.statement.contains("`R > (R + delta)`") && o.statement.contains("0.01 ≤ delta ≤ 0.1"), "{}", o.statement);
    assert!(o.evidence.contains("relaxed by"), "{}", o.evidence);
    assert_eq!(compiled.dead.len(), 1);
    assert!(compiled.rewritten.infeasible.contains_key(&ri), "{:?}", compiled.rewritten.infeasible);
    assert!(compiled.rewritten.infeasible[&ri].contains("tier 2 (linear feasibility)"), "{}", compiled.rewritten.infeasible[&ri]);
    assert!(
        compiled.rewritten.log.entries.iter().any(|e| e.rewrite == "R2" && e.rule == "v::dead#1"),
        "{}",
        compiled.rewritten.log.table()
    );
    assert!(compiled.lowered.skip[ri], "the lowered plan skips the rule");
    // The written plan (no rewrite) keeps the rule, as the unoptimised
    // path of section 6.6 must.
    let unopt = compile(
        &p,
        &CompileOptions {
            optimise: false,
            ..CompileOptions::default()
        },
    );
    assert!(!unopt.lowered.skip[ri]);
    assert!(unopt.tiers.iter().any(|t| t.tier == 2), "tier 2 reports regardless");
}

/// The margin of section 6.8: `R > R` is infeasible over the reals but
/// feasible once relaxed, so the literal is kept; a satisfiable guard is
/// feasible; a parameter range makes `delta < 0` infeasible (A009 and
/// tier 2 agree); a nonlinear comparison is left to tier 3.
#[test]
fn tier2_margin_feasible_ranges_and_nonlinear_parts() {
    let src = with_rules(
        "    delta : Scalar = 0.05 in 0.01..0.1",
        r#"
    rel touching(A: Equity, @T)
    touching(A, T) :- universe(A, T), log_price_return(A, T, R), R > R.
    rel open(A: Equity, @T)
    open(A, T) :- universe(A, T), log_price_return(A, T, R), R > delta.
    rel ranged(A: Equity, @T)
    ranged(A, T) :- universe(A, T), delta < 0.
    rel curved(A: Equity, @T)
    curved(A, T) :- universe(A, T), log_price_return(A, T, R), R * R < 0, R > 1.
"#,
        "    decide(T, trade(A, qty)) :- touching(A, T), open(A, T), flat(A, T).\n    decide(T, trade(A, Q)) :- ranged(A, T), curved(A, T), held(A, T, Q0), Q = -Q0.",
    );
    let (p, diags) = program(&src, "v");
    let mut compiled = compile(&p, &CompileOptions::default());
    let t2 = compiled.tiers.iter().find(|t| t.tier == 2).unwrap();
    let by = |label: &str| {
        let ri = (0..p.rules.len()).find(|&i| p.rule_label(i) == label).unwrap();
        (ri, t2.obligations.iter().find(|o| o.rule_index == Some(ri)).unwrap_or_else(|| panic!("{} has an obligation", label)))
    };
    let (touching, o) = by("v::touching#1");
    assert_eq!(o.verdict, Verdict::Infeasible);
    assert!(o.licenses.is_none() && o.evidence.contains("within the rounding margin"), "{:?}", o);
    assert!(!compiled.rewritten.infeasible.contains_key(&touching));
    let (_, o) = by("v::open#1");
    assert_eq!(o.verdict, Verdict::Feasible, "{:?}", o);
    let (ranged, o) = by("v::ranged#1");
    assert_eq!(o.verdict, Verdict::Infeasible);
    assert_eq!(o.licenses.as_deref(), Some("R2"));
    assert!(compiled.rewritten.infeasible.contains_key(&ranged));
    let (_, o) = by("v::curved#1");
    assert_eq!(o.verdict, Verdict::Feasible, "the linear part `R > 1` alone is feasible: {:?}", o);
    assert!(o.logic.as_deref().unwrap_or("").contains("1 nonlinear left to tier 3"), "{:?}", o.logic);
    // Without a solver the tier 3 obligation is undecided, and the rule stays.
    let opts = CompileOptions::default();
    let report = verify(
        &VerifyInputs {
            prog: &p,
            diags: &diags,
            opts: &opts,
            max_tier: 3,
            data: None,
        },
        &mut compiled,
    );
    let t3 = report.tier(3).unwrap();
    let o = t3
        .obligations
        .iter()
        .find(|o| o.rule.as_deref() == Some("v::curved#1") && o.kind == "rule feasibility")
        .expect("tier 3 attempts the nonlinear guard");
    assert_eq!(o.verdict, Verdict::Undecided);
    assert_eq!(o.logic.as_deref(), Some("QF_NRA"));
    assert!(t3.notes.iter().any(|n| n.contains("no solver named")), "{:?}", t3.notes);
    assert!(report.tier(0).unwrap().obligations.iter().filter(|o| o.kind == "judgment").count() == 12);
    assert!(report.tier(1).unwrap().obligations.iter().any(|o| o.statement.contains("[A009]")), "{:?}", report.tier(1));
    assert_eq!(report.json()["stage"], "verified");
    assert!(report.text().contains("tier 2 linear feasibility"));
}

/// The replay files of tier 3 and tier 4, written without a solver: the
/// guard-disjointness script of a relation keyed at runtime aligns the
/// heads, carries the complete relation's membership as a Boolean, and
/// both arithmetic parts; the CHC script declares one predicate per
/// relation of the component over the bar and the numeric value columns,
/// steps the bar through `prev`, and queries the candidate bound.
#[test]
fn the_scripts_of_tiers_3_and_4_replay_from_their_files() {
    let (p, diags) = corpus_program("strategies", "trailing_stop");
    let dir = scratch("scripts");
    let opts = CompileOptions {
        replay_dir: Some(dir.clone()),
        ..CompileOptions::default()
    };
    let mut compiled = compile(&p, &opts);
    assert_eq!(
        compiled.analysis.relation("high_since_entry").unwrap().keyedness,
        Keyedness::AtRuntime,
        "the wildcards keep the pair from syntactic complementarity"
    );
    let report = verify(
        &VerifyInputs {
            prog: &p,
            diags: &diags,
            opts: &opts,
            max_tier: 4,
            data: None,
        },
        &mut compiled,
    );
    let t3 = report.tier(3).unwrap();
    for o in &t3.obligations {
        eprintln!("t3 {} {} {:?} {:?} {}", o.id, o.kind, o.rule, o.logic, o.statement);
    }
    let pair = t3.obligations.iter().find(|o| o.kind == "guard disjointness").expect("a pair obligation");
    assert_eq!(pair.verdict, Verdict::Undecided);
    let text = fs::read_to_string(pair.replay_file.as_ref().expect("a replay file")).unwrap();
    assert!(text.contains("(set-logic QF_LRA)"), "{}", text);
    assert!(text.contains("(assert (= a_A b_A))") && text.contains("(assert (= a_T b_T))"), "{}", text);
    assert!(text.contains("(assert in_fill__b_A_b_T)") && text.contains("(assert (not in_fill__b_A_b_T))"), "{}", text);
    assert!(text.contains("(assert (> a_Q 0.0))") && text.contains("(ite (>= b_H0 b_P) b_H0 b_P)"), "{}", text);
    assert!(text.ends_with("(check-sat)\n(exit)\n"), "{}", text);
    let conflict = t3.obligations.iter().find(|o| o.kind == "conflict freedom").expect("the decide pair");
    let text = fs::read_to_string(conflict.replay_file.as_ref().unwrap()).unwrap();
    assert!(text.contains("(set-logic QF_NRA)"), "H · (1 − stop) with a ranged parameter is nonlinear: {}", text);
    assert!(text.contains("p_trailing_stop__stop"), "{}", text);
    let fold = t3.obligations.iter().find(|o| o.kind == "rewrite equivalence (R6)").expect("the fold of 1 - stop");
    assert!(fold.statement.contains("`(1 - stop)` = 0.92"), "{}", fold.statement);
    let text = fs::read_to_string(fold.replay_file.as_ref().unwrap()).unwrap();
    assert!(text.contains("(assert (or (> (- (- 1.0"), "{}", text);
    let feas = t3
        .obligations
        .iter()
        .find(|o| o.kind == "rule feasibility" && o.rule.as_deref() == Some("trailing_stop::decide#2"))
        .expect("the nonlinear guard of decide#2");
    assert_eq!(feas.logic.as_deref(), Some("QF_NRA"));
    assert!(
        !t3.obligations.iter().any(|o| o.kind == "rule feasibility" && o.rule.as_deref() == Some("trailing_stop::momentum#1")),
        "an assignment alone is no guard"
    );
    let t4 = report.tier(4).unwrap();
    for o in &t4.obligations {
        eprintln!("t4 {} {} {:?} {}", o.id, o.kind, o.rule, o.statement);
    }
    let inv = t4.obligations.iter().find(|o| o.id == "t4-high_since_entry-H-ge-1").expect("the lower-bound candidate");
    assert!(inv.statement.contains("`high_since_entry.H ≥ 0`"), "{}", inv.statement);
    assert_eq!(inv.verdict, Verdict::Undecided);
    let text = fs::read_to_string(inv.replay_file.as_ref().unwrap()).unwrap();
    assert!(text.contains("(set-logic HORN)"), "{}", text);
    assert!(text.contains("(declare-fun P_high_since_entry (Int Real) Bool)"), "{}", text);
    assert!(text.contains("(= c_T0 (- c_T 1))") && text.contains("(P_high_since_entry c_T0 c_H0)"), "{}", text);
    assert!(text.contains("(=> (and (P_high_since_entry b v2) (not (>= v2 0.0))) false)"), "{}", text);
    assert!(text.contains("(get-model)"), "{}", text);
    let _ = fs::remove_dir_all(&dir);
}

/// A stand-in solver (a shell script answering by the obligation's file
/// name) drives the verdicts: proved disjointness upgrades the keyedness
/// label, proved conflict freedom is noted, a proved CHC invariant refines
/// the analysis interval and keeps its certificate, and a `sat` feasibility
/// answer leaves the rule in place.
#[cfg(unix)]
#[test]
fn a_solver_subprocess_settles_the_obligations() {
    use std::os::unix::fs::PermissionsExt;
    let dir = scratch("solver");
    let fake = dir.join("fake-solver");
    fs::write(
        &fake,
        "#!/bin/sh\nf=\"$1\"\nfor a in \"$@\"; do f=\"$a\"; done\ncase \"$f\" in\n  *t3-o4*) echo sat ;;\n  *-ge-*) echo sat; echo '(model (define-fun P_high_since_entry ((b Int) (h Real)) Bool (>= h 0.0)))' ;;\n  *-le-*) echo unsat ;;\n  *t3-o1*|*t3-o2*|*t3-o3*) echo unsat ;;\n  *) echo unknown ;;\nesac\n",
    )
    .unwrap();
    fs::set_permissions(&fake, fs::Permissions::from_mode(0o755)).unwrap();
    let (p, diags) = corpus_program("strategies", "trailing_stop");
    let opts = CompileOptions {
        solver: Some(fake.display().to_string()),
        replay_dir: Some(dir.clone()),
        ..CompileOptions::default()
    };
    let mut compiled = compile(&p, &opts);
    let t3c = compiled.tiers.iter().find(|t| t.tier == 3).expect("tier 3 feasibility runs in compile with a solver");
    for o in &t3c.obligations {
        eprintln!("t3c {} {} {:?} {:?} {:?}", o.id, o.kind, o.rule, o.verdict, o.replay_file);
    }
    assert!(
        t3c.obligations.iter().all(|o| o.verdict == Verdict::Feasible),
        "{:?}",
        t3c.obligations.iter().map(|o| (&o.id, o.verdict)).collect::<Vec<_>>()
    );
    assert!(compiled.rewritten.infeasible.is_empty());
    let report = verify(
        &VerifyInputs {
            prog: &p,
            diags: &diags,
            opts: &opts,
            max_tier: 4,
            data: None,
        },
        &mut compiled,
    );
    let t3 = report.tier(3).unwrap();
    for o in &t3.obligations {
        eprintln!("t3 {} {} {:?} {:?} {:?}", o.id, o.kind, o.rule, o.verdict, o.replay_file);
    }
    let pair = t3.obligations.iter().find(|o| o.kind == "guard disjointness").unwrap();
    assert_eq!(pair.verdict, Verdict::Proved, "{:?}", pair);
    assert!(pair.elapsed_ms <= pair.budget_ms + 500);
    assert_eq!(compiled.analysis.relation("high_since_entry").unwrap().keyedness, Keyedness::ByGuard);
    assert!(t3.notes.iter().any(|n| n.contains("upgraded to keyed by guard")), "{:?}", t3.notes);
    assert!(t3.notes.iter().any(|n| n.contains("conflict-free by guard")), "{:?}", t3.notes);
    assert!(t3.obligations.iter().filter(|o| o.kind.starts_with("rewrite equivalence")).all(|o| o.verdict == Verdict::Proved));
    let t4 = report.tier(4).unwrap();
    let ge = t4.obligations.iter().find(|o| o.id == "t4-high_since_entry-H-ge-1").unwrap();
    assert_eq!(ge.verdict, Verdict::Proved, "{:?}", ge);
    assert!(ge.evidence.contains("certificate"), "{}", ge.evidence);
    let cert = dir.join("trailing_stop-t4-high_since_entry-H-ge-1.model");
    assert!(fs::read_to_string(&cert).unwrap().contains("define-fun"));
    let le = t4.obligations.iter().find(|o| o.id == "t4-high_since_entry-H-le-1").unwrap();
    assert_eq!(le.verdict, Verdict::NotProved, "{:?}", le);
    let h = &compiled.analysis.relation("high_since_entry").unwrap().columns[2];
    assert_eq!(h.value.interval.lo, 0.0, "the proved invariant refines the interval: {:?}", h.value);
    assert!(t4.notes.iter().any(|n| n.contains("refined")), "{:?}", t4.notes);
    assert_eq!(report.counts().get("undecided"), None, "{:?}", report.counts());
    let _ = fs::remove_dir_all(&dir);
}

/// A z-score mean reversion whose two aggregations share one window state
/// in the lowered plan (R3), which the comparison of the plans exercises.
/// The library's `zscore_window` splits the mean and the deviation into
/// two relations, so the rule is written out here.
fn zscore_strategy() -> String {
    common::StrategyParts {
        name: "zscore".into(),
        uses: "calendar, book".into(),
        params: "    n : Duration = 20d in 10d..60d\n    k : Count = 12\n    entry : Scalar = -2.0 in -4.0..-1.0\n    exit : Scalar = 0.0\n    qty : Quantity<Shares> = 100 shares".into(),
        rules: r#"
    rel zscore(A: Equity, @T, Z: Scalar)
    zscore(A, T, Z) :- universe(A, T), close(A, T, P),
        M = mean(P1) over (T1 in window[n] min k, close(A, T1, P1)),
        S = std(P1) over (T1 in window[n] min k, close(A, T1, P1)),
        Z = (P - M) / S.
    rel oversold(A: Equity, @T)
    oversold(A, T) :- zscore(A, T, Z), Z < entry.
    rel recovered(A: Equity, @T)
    recovered(A, T) :- zscore(A, T, Z), Z > exit.
"#
        .into(),
        decides: "    decide(T, trade(A, qty)) :- oversold(A, T), flat(A, T).\n    decide(T, trade(A, Q)) :- recovered(A, T), held(A, T, Q0), Q = -Q0.".into(),
        ..Default::default()
    }
    .render()
}

/// Tiers 5 and 6 on a synthetic binding: the written and the optimised
/// plan agree on `decide`, the ledgers and the book, and the runtime tier
/// reports coverage. The zscore rule's two aggregations share one window
/// state in the lowered plan (R3), which the comparison exercises.
#[test]
fn tiers_5_and_6_run_the_two_plans_on_a_binding() {
    let (p, diags) = program(&zscore_strategy(), "zscore");
    let opts = CompileOptions::default();
    let mut compiled = compile(&p, &opts);
    assert_eq!(compiled.lowered.shared_windows.len(), 1, "{:?}", compiled.lowered.notes);
    let ds = synthetic_daily(&["AAA", "BBB", "CCC", "DDD", "SPY"], (2022, 1, 3), 120, 7);
    let cfg = ExecConfig::from_program(&p);
    let report = verify(
        &VerifyInputs {
            prog: &p,
            diags: &diags,
            opts: &opts,
            max_tier: 6,
            data: Some((&ds, cfg)),
        },
        &mut compiled,
    );
    assert!(!report.refuted(), "{}", report.text());
    let t5 = report.tier(5).unwrap();
    assert!(t5.obligations.iter().all(|o| o.verdict == Verdict::Proved), "{}", report.text());
    assert!(t5.obligations.iter().any(|o| o.id == "t5-decide-book"));
    assert!(t5.obligations.iter().any(|o| o.id == "t5-ledger-oversold"));
    assert!(t5.notes.iter().any(|n| n.contains("1 shared window state(s)")), "{:?}", t5.notes);
    let t6 = report.tier(6).unwrap();
    assert_eq!(t6.obligations.iter().find(|o| o.id == "t6-run").unwrap().verdict, Verdict::Proved);
    assert!(t6.obligations.iter().any(|o| o.kind == "coverage"));
    // Without a binding the two tiers are skipped with a note.
    let report = verify(
        &VerifyInputs {
            prog: &p,
            diags: &diags,
            opts: &opts,
            max_tier: 6,
            data: None,
        },
        &mut compiled,
    );
    assert!(report.tier(5).unwrap().obligations.is_empty());
    assert!(report.tier(5).unwrap().notes[0].contains("no binding"));
}

/// Every corpus strategy compiles with tier 2 inside `compile` and
/// verifies through tier 4 without a solver and without a panic; no
/// corpus rule is dead.
#[test]
fn every_corpus_program_verifies() {
    for sub in ["strategies", "company"] {
        for f in common::dsl_files(sub) {
            let name = f.file_stem().unwrap().to_string_lossy().to_string();
            let (p, diags) = corpus_program(sub, &name);
            let opts = CompileOptions::default();
            let mut compiled = compile(&p, &opts);
            assert!(compiled.dead.is_empty(), "{}: {:?}", name, compiled.dead);
            let report = verify(
                &VerifyInputs {
                    prog: &p,
                    diags: &diags,
                    opts: &opts,
                    max_tier: 4,
                    data: None,
                },
                &mut compiled,
            );
            assert_eq!(report.tiers.len(), 5, "{}", name);
            assert!(!report.refuted(), "{}: {}", name, report.text());
            let text = report.text();
            assert!(text.contains(&format!("verification of {}", name)));
            let json = report.json();
            assert_eq!(json["tiers"].as_array().map(|t| t.len()), Some(5));
        }
    }
}
