//! The set-at-a-time evaluator against the tuple evaluator (M-8's gate;
//! docs/spec/06-performance.md, 3.2): translation validation on every
//! corpus program and both books. Each runs under both evaluators on the
//! seeded market of its class. The two runs' renderings, every decision,
//! fill, drop, book row and action with its bits (`verify::tv::digest`),
//! and the ledgers of every derived relation at every bar, are equal.

#![allow(clippy::result_large_err)]

use abt_kernel::kernel::ExecConfig;
use abt_kernel::verify::tv;
use abt_tests::{golden_inputs, golden_programs, run_kernel};

/// The configurations held equal: the tuple evaluator, the set evaluator on
/// one thread, and the set evaluator cut across two and four worker kernels
/// with every entity-local rule taking the cut (`parallel_min_rows = 1`;
/// F-5.3, Proposition 3.1 of 06-performance.md).
const CONFIGS: [(bool, usize); 4] = [(false, 1), (true, 1), (true, 2), (true, 4)];

#[test]
fn the_set_evaluator_derives_what_the_tuple_evaluator_does_on_every_corpus_program_and_both_books() {
    let mut programs = 0;
    for path in golden_programs() {
        let (p, ds) = golden_inputs(&path).unwrap();
        let mut per = Vec::new();
        for (set, threads) in CONFIGS {
            let mut cfg = ExecConfig::from_program(&p);
            cfg.set_eval = set;
            cfg.threads = threads;
            cfg.parallel_min_rows = 1;
            per.push(match run_kernel(&p, &ds, cfg) {
                Ok((r, mut k)) => {
                    let ledgers = tv::ledgers(&mut k, &p, &r.bars);
                    Ok((tv::digest(&r), ledgers))
                }
                Err(e) => Err(e.to_string()),
            });
        }
        let tuple = &per[0];
        for (i, set) in per.iter().enumerate().skip(1) {
            let label = format!("{} (set evaluator, {} thread(s))", p.strategy, CONFIGS[i].1);
            match (tuple, set) {
                (Ok((d0, l0)), Ok((d1, l1))) => {
                    if d0 != d1 {
                        let (at, a, b) = tv::first_difference(d0, d1).unwrap_or((0, String::new(), String::new()));
                        panic!("{}: the runs differ at line {}:\n  one thread: {}\n  this run:   {}", label, at, a, b);
                    }
                    for (rel, rows) in l0 {
                        assert_eq!(Some(rows), l1.get(rel), "{}: the ledger of `{}` differs", label, rel);
                    }
                    assert_eq!(l0.len(), l1.len(), "{}", label);
                }
                (Err(a), Err(b)) => assert_eq!(a, b, "{}: the runs halt differently", label),
                _ => panic!("{}: one evaluator halts and the other does not: {:?} against {:?}", label, tuple.as_ref().err(), set.as_ref().err()),
            }
        }
        programs += 1;
    }
    assert!(programs >= 24, "{} programs ran", programs);
}

/// Each corpus program and book under each evaluator, timed. Ignored by
/// default: run with
/// `cargo test -p abt-tests --release --test eval_set -- --ignored --nocapture`
/// and record what it prints in docs/assessment.md.
#[test]
#[ignore]
fn time_the_two_evaluators() {
    let mut total = [0.0f64; 3];
    println!("| program | tuple (s) | set (s) | set, 4 threads (s) |");
    println!("| --- | ---: | ---: | ---: |");
    for path in golden_programs() {
        let (p, ds) = golden_inputs(&path).unwrap();
        let mut secs = [0.0f64; 3];
        for (i, (set, threads)) in [(false, 1), (true, 1), (true, 4)].into_iter().enumerate() {
            let mut cfg = ExecConfig::from_program(&p);
            cfg.set_eval = set;
            cfg.threads = threads;
            // The faster of three runs.
            secs[i] = (0..3)
                .map(|_| {
                    let t = std::time::Instant::now();
                    let _ = abt_tests::run(&p, &ds, cfg.clone());
                    t.elapsed().as_secs_f64()
                })
                .fold(f64::INFINITY, f64::min);
            total[i] += secs[i];
        }
        println!("| {} | {:.3} | {:.3} | {:.3} |", p.strategy, secs[0], secs[1], secs[2]);
    }
    println!("| total | {:.3} | {:.3} | {:.3} |", total[0], total[1], total[2]);
}
