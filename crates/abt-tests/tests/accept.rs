//! The acceptance test (compiler design, section 8, items 4 and 5):
//! determinism, every corpus-v2 strategy run under two thread counts with
//! bit-identical outputs; and truncation, each binding cut at ten sampled
//! bars with `decide` equal up to each. The books (items 1 and 3) run in
//! `tests/tv.rs` when their bundles are named; the corpus and the warnings
//! (item 2) in `tests/corpus_v2.rs` and `tests/analysis.rs`; the usability
//! study (item 6) is a separate experiment.

use abt_tests as common;

use std::collections::BTreeMap;

use abt_kernel::check::Program;
use abt_kernel::compiler::{compile, CompileOptions};
use abt_kernel::data::synthetic_for_program;
use abt_kernel::kernel::{Dataset, ExecConfig};
use abt_kernel::lower::Lowered;
use abt_kernel::verify::tv::{digest, first_difference, run_with_ledgers, Ledgers};
use abt_tests::run_planned;

fn corpus_strategies() -> Vec<(String, Program)> {
    common::dsl_files("strategies")
        .into_iter()
        .map(|f| {
            let name = f.file_stem().unwrap().to_string_lossy().to_string();
            let p = common::corpus_program(&name);
            (name, p)
        })
        .collect()
}

/// The names of the synthetic market: its second name is renamed halfway
/// and its last delisted at four fifths, so the names the strategies cite
/// ("AAA", "BBB", "SPY") stay listed throughout.
const SYMS: [&str; 5] = ["AAA", "XYZ", "BBB", "SPY", "DDD"];

/// The synthetic market of a program's class and base: 400 daily bars
/// with the catalog, or 30 sessions of minute bars.
fn dataset_for(p: &Program, seed: u64) -> Dataset {
    if p.base.is_sub_daily() {
        synthetic_for_program(p, &SYMS, (2024, 1, 2), 30, seed)
    } else {
        synthetic_for_program(p, &SYMS, (2022, 1, 3), 400, seed)
    }
}

/// A run's comparable text: the digest and the ledgers, or the halt.
type Outcome = Result<(Vec<String>, Ledgers), String>;

fn outcome(p: &Program, lowered: &Lowered, ds: &Dataset) -> Outcome {
    run_with_ledgers(p, lowered, ds, ExecConfig::from_program(p)).map(|r| (r.digest, r.ledgers))
}

fn assert_same_outcome(name: &str, what: &str, a: &Outcome, b: &Outcome) {
    match (a, b) {
        (Ok((da, la)), Ok((db, lb))) => {
            if let Some((i, x, y)) = first_difference(da, db) {
                panic!("{}: {} differ at line {}:\n  a: {}\n  b: {}", name, what, i, x, y);
            }
            assert_eq!(la.keys().collect::<Vec<_>>(), lb.keys().collect::<Vec<_>>(), "{}: ledger sets differ", name);
            for (rel, rows) in la {
                if let Some((i, x, y)) = first_difference(rows, &lb[rel]) {
                    panic!("{}: ledger of {} differs at tuple {} ({}):\n  a: {}\n  b: {}", name, rel, i, what, x, y);
                }
            }
        }
        (Err(ea), Err(eb)) => assert_eq!(ea, eb, "{}: both halted, differently", name),
        (a, b) => panic!("{}: one run halted and the other did not: {:?} / {:?}", name, a.as_ref().map(|_| ()), b.as_ref().map(|_| ())),
    }
}

/// Item 4: every corpus program run once on a single thread and once
/// among four threads running the corpus concurrently (each run on its own
/// kernel) gives bit-identical decisions, fills, book and ledgers.
#[test]
fn every_corpus_program_is_bit_identical_under_two_thread_counts() {
    let programs = corpus_strategies();
    let compiled: Vec<(String, Program, Lowered, Dataset)> = programs
        .into_iter()
        .map(|(name, p)| {
            let lowered = compile(&p, &CompileOptions::default()).lowered;
            let ds = dataset_for(&p, 11);
            (name, p, lowered, ds)
        })
        .collect();
    // One thread, in order.
    let sequential: BTreeMap<&str, Outcome> = compiled.iter().map(|(name, p, l, ds)| (name.as_str(), outcome(p, l, ds))).collect();
    // Four threads, each over a slice of the corpus, at once.
    let threads = 4;
    let chunk = compiled.len().div_ceil(threads);
    let concurrent: BTreeMap<&str, Outcome> = std::thread::scope(|s| {
        let handles: Vec<_> = compiled
            .chunks(chunk)
            .map(|slice| s.spawn(move || slice.iter().map(|(name, p, l, ds)| (name.as_str(), outcome(p, l, ds))).collect::<Vec<_>>()))
            .collect();
        handles.into_iter().flat_map(|h| h.join().unwrap()).collect()
    });
    assert_eq!(sequential.len(), concurrent.len());
    for (name, a) in &sequential {
        assert_same_outcome(name, "one thread against four", a, &concurrent[name]);
    }
}

/// Item 5: for every corpus program, the binding truncated at ten sampled
/// bars gives the same decisions up to each bar as the full binding, under
/// the optimised plan.
#[test]
fn decisions_are_the_same_on_every_truncated_binding() {
    for (name, p) in corpus_strategies() {
        let lowered = compile(&p, &CompileOptions::default()).lowered;
        let ds = dataset_for(&p, 11);
        let cfg = ExecConfig::from_program(&p);
        let full = match run_planned(&p, &lowered, &ds, cfg.clone()) {
            Ok(r) => r,
            Err(e) => panic!("{}: {}", name, e),
        };
        let describe = |r: &abt_kernel::kernel::RunResult, upto: i64| -> Vec<String> {
            let mut v: Vec<String> = r
                .decisions
                .iter()
                .filter(|d| d.t <= upto)
                .map(|d| format!("{} {} {}", d.t, r.describe_decision(&d.decision), d.rule))
                .collect();
            v.sort();
            v
        };
        let samples = abt_kernel::state::tables::sampled_bars(&full.bars, 10);
        assert!(!samples.is_empty(), "{}", name);
        let client = abt_tests::client(&p, &ds);
        for t in samples {
            let partial = match abt_kernel::kernel::run_on(
                &p,
                Some(&lowered),
                &client,
                cfg.clone(),
                &abt_kernel::kernel::Replay {
                    horizon: Some(t),
                    ..Default::default()
                },
            ) {
                Ok(r) => r,
                Err(abt_kernel::kernel::RunError::NoBars) => continue,
                Err(e) => panic!("{} truncated at {}: {}", name, t, e),
            };
            let (a, b) = (describe(&full, t), describe(&partial, t));
            if let Some((i, x, y)) = first_difference(&a, &b) {
                panic!(
                    "{}: decisions up to {} differ at {} ({} full, {} truncated):\n  full:      {}\n  truncated: {}",
                    name,
                    t,
                    i,
                    a.len(),
                    b.len(),
                    x,
                    y
                );
            }
        }
    }
}

/// The digest a run exports is the same text the harness compares, so a
/// difference in either is visible to both.
#[test]
fn the_digest_is_deterministic_across_runs() {
    let (name, p) = corpus_strategies().into_iter().find(|(n, _)| n == "momentum_top_n").unwrap();
    let lowered = compile(&p, &CompileOptions::default()).lowered;
    let ds = dataset_for(&p, 3);
    let cfg = ExecConfig::from_program(&p);
    let a = run_planned(&p, &lowered, &ds, cfg.clone()).unwrap();
    let b = run_planned(&p, &lowered, &ds, cfg).unwrap();
    assert_eq!(digest(&a), digest(&b), "{}", name);
    assert!(digest(&a).iter().any(|l| l.starts_with("decision ")), "{}", name);
}
