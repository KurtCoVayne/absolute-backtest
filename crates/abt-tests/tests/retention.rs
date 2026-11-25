//! The retention horizon (06-performance 3.1, `Kernel::trim_stores`): a run
//! that drops the stored rows behind its lookback at every barrier decides,
//! fills and holds what the run keeping every row does, on every corpus
//! program and both books, alone and with the worker kernels; and it holds
//! fewer rows wherever the market is longer than the lookback.

#![allow(clippy::result_large_err)]

use abt_kernel::kernel::ExecConfig;
use abt_kernel::verify::tv;
use abt_tests::{golden_inputs, golden_programs, run_kernel};

#[test]
fn a_trimmed_run_equals_the_whole_run_on_every_corpus_program_and_both_books() {
    let mut programs = 0;
    let mut trimmed_somewhere = false;
    for path in golden_programs() {
        let (p, ds) = golden_inputs(&path).unwrap();
        let whole = run_kernel(&p, &ds, ExecConfig::from_program(&p)).map(|(r, k)| (tv::digest(&r), k.store_rows()));
        for threads in [1, 4] {
            let mut cfg = ExecConfig::from_program(&p);
            cfg.trim_stores = true;
            cfg.threads = threads;
            cfg.parallel_min_rows = 1;
            let trimmed = run_kernel(&p, &ds, cfg).map(|(r, k)| (tv::digest(&r), k.store_rows(), k.stats().trimmed_rows));
            match (&whole, &trimmed) {
                (Ok((d0, rows0)), Ok((d1, rows1, dropped))) => {
                    if d0 != d1 {
                        let (at, a, b) = tv::first_difference(d0, d1).unwrap_or((0, String::new(), String::new()));
                        panic!("{} ({} thread(s)): the trimmed run differs at line {}:\n  whole:   {}\n  trimmed: {}", p.strategy, threads, at, a, b);
                    }
                    assert!(rows1 <= rows0, "{}: trimmed holds {} rows, whole {}", p.strategy, rows1, rows0);
                    if *dropped > 0 {
                        trimmed_somewhere = true;
                        assert!(rows1 < rows0, "{}: {} rows dropped but {} held of {}", p.strategy, dropped, rows1, rows0);
                    }
                }
                (Err(a), Err(b)) => assert_eq!(a.to_string(), b.to_string(), "{}: the runs halt differently", p.strategy),
                _ => panic!("{}: one run halts and the other does not: {:?} against {:?}", p.strategy, whole.as_ref().err(), trimmed.as_ref().err()),
            }
        }
        programs += 1;
    }
    assert!(programs >= 24, "{} programs ran", programs);
    assert!(trimmed_somewhere, "no program had rows behind its horizon: the test exercised nothing");
}

/// MW14 over a market longer than its horizon (the golden market's 80 weekly
/// bars are under the 71 the book looks back over, so the corpus gate never
/// trims it): 120 names over six years, so that most names are first priced
/// long after their first row (the total-return index is a recursion back
/// to it, E-126); whole against trimmed, alone and on four workers.
#[test]
fn mw14_over_a_market_longer_than_its_horizon_is_trimmed_and_equal() {
    let names: Vec<String> = (0..120).map(|i| format!("S{:03}", i)).collect();
    let syms: Vec<&str> = names.iter().map(String::as_str).collect();
    let p = abt_tests::program(&abt_tests::corpus_source("company", "mw14"), "mw14");
    let ds = abt_kernel::data::synthetic_for_program(&p, &syms, (2010, 1, 4), 1500, 11);
    let (whole, k0) = run_kernel(&p, &ds, ExecConfig::from_program(&p)).unwrap();
    let (d0, rows0) = (tv::digest(&whole), k0.store_rows());
    assert!(whole.bars.len() > 200, "{} bars", whole.bars.len());
    assert!(whole.fills.len() > 100, "{} fills", whole.fills.len());
    for threads in [1, 4] {
        let mut cfg = ExecConfig::from_program(&p);
        cfg.trim_stores = true;
        cfg.threads = threads;
        let (r, k) = run_kernel(&p, &ds, cfg).unwrap();
        let d1 = tv::digest(&r);
        if d0 != d1 {
            let (at, a, b) = tv::first_difference(&d0, &d1).unwrap_or((0, String::new(), String::new()));
            panic!("mw14 ({} thread(s)): the trimmed run differs at line {}:\n  whole:   {}\n  trimmed: {}", threads, at, a, b);
        }
        let dropped = k.stats().trimmed_rows;
        assert!(dropped > 0 && k.store_rows() < rows0, "nothing trimmed: {} dropped, {} of {} rows held", dropped, k.store_rows(), rows0);
    }
}
