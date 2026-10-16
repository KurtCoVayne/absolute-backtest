//! The event log (K-5.2, Proposition K-5.1): on every corpus program and
//! both books, the log folds from the initial capital to the run's final
//! book, bit for bit; the run's decisions, fills, drops and actions are its
//! projections; every decision becomes one order, and every order ends in
//! one outcome (filled, dropped, met, replaced, expired) or is still open
//! when the data ends.

#![allow(clippy::result_large_err)]

use std::collections::BTreeMap;

use abt_kernel::kernel::events::{Fate, OrderId};
use abt_kernel::kernel::{Book, EventKind, ExecConfig, RunResult};
use abt_tests::{golden_inputs, golden_programs, run};

/// What an order's last event says about it.
#[derive(Debug, PartialEq)]
enum Outcome {
    Filled,
    Dropped,
    Met,
    Replaced,
    Expired,
    /// A target whose capped fill re-issues it, or an order still working.
    Open,
}

fn outcomes(r: &RunResult) -> BTreeMap<OrderId, Outcome> {
    let mut out = BTreeMap::new();
    for e in &r.events {
        let Some(id) = e.order_id else { continue };
        let o = match &e.kind {
            EventKind::OrderSent { .. } => Outcome::Open,
            EventKind::Fill {
                remainder: Some((_, Fate::Reissued)), ..
            } => Outcome::Open,
            // The expired remainder of a capped delta order is dropped next.
            EventKind::Fill {
                remainder: Some((_, Fate::Expired)), ..
            } => Outcome::Open,
            EventKind::Fill { .. } => Outcome::Filled,
            EventKind::Dropped { .. } | EventKind::Rejected { .. } => Outcome::Dropped,
            EventKind::Satisfied { .. } => Outcome::Met,
            EventKind::Replaced { .. } => Outcome::Replaced,
            EventKind::ExpiredOrder { .. } => Outcome::Expired,
            _ => continue,
        };
        out.insert(id, o);
    }
    out
}

#[test]
fn the_log_folds_to_the_book_and_every_order_ends_once() {
    let mut programs = 0;
    for f in golden_programs() {
        let (p, ds) = golden_inputs(&f).unwrap();
        let cfg = ExecConfig::from_program(&p);
        let initial = cfg.initial_cash;
        let Ok(r) = run(&p, &ds, cfg) else { continue };
        programs += 1;
        let name = &p.strategy;
        // Numbered from zero, in order.
        for (i, e) in r.events.iter().enumerate() {
            assert_eq!(e.seq, i as u64, "{}", name);
        }
        // Proposition K-5.1: the fold of the log is the book.
        let book = Book::fold(initial, &r.events);
        assert_eq!(book.cash.to_bits(), r.final_cash.to_bits(), "{}: cash", name);
        let folded: Vec<(u32, u64)> = book.positions.iter().map(|(s, q)| (*s, q.to_bits())).collect();
        let held: Vec<(u32, u64)> = r.final_positions.iter().map(|(s, q)| (*s, q.to_bits())).collect();
        assert_eq!(folded, held, "{}: positions", name);
        // The projections.
        let count = |f: &dyn Fn(&EventKind) -> bool| r.events.iter().filter(|e| f(&e.kind)).count();
        assert_eq!(count(&|k| matches!(k, EventKind::Decision { .. })), r.decisions.len(), "{}", name);
        assert_eq!(count(&|k| matches!(k, EventKind::Fill { .. })), r.fills.len(), "{}", name);
        assert_eq!(
            count(&|k| matches!(k, EventKind::Dropped { .. } | EventKind::ExpiredOrder { .. } | EventKind::Rejected { .. })),
            r.dropped.len(),
            "{}",
            name
        );
        assert_eq!(count(&|k| matches!(k, EventKind::Action { .. })), r.actions.len(), "{}", name);
        // Every decision is one order; every order ends once, or is open
        // at the end (a target re-issuing, an order working).
        let sent = count(&|k| matches!(k, EventKind::OrderSent { .. }));
        assert_eq!(sent, r.decisions.len(), "{}", name);
        let ends = outcomes(&r);
        assert_eq!(ends.len(), sent, "{}: every order has an outcome", name);
        // At most one order an instrument is open at the end.
        let open = ends.values().filter(|o| **o == Outcome::Open).count();
        assert!(open <= r.symbols.len(), "{}: {} orders open at the end", name, open);
        // A fill, a drop and an expiry name their order; a forced fill none.
        for e in &r.events {
            match &e.kind {
                EventKind::Fill { fill, .. } => assert_eq!(e.order_id.is_none(), fill.forced, "{}: {:?}", name, e),
                EventKind::Dropped { .. } | EventKind::ExpiredOrder { .. } | EventKind::Satisfied { .. } | EventKind::Replaced { .. } | EventKind::Rejected { .. } => {
                    assert!(e.order_id.is_some(), "{}: {:?}", name, e)
                }
                _ => {}
            }
        }
    }
    assert!(programs >= 20, "{} programs ran", programs);
}

#[test]
fn the_nav_counts_a_dividend_from_its_ex_date_to_its_payment() {
    let (p, ds) = golden_inputs(&abt_tests::corpus_dir().join("strategies").join("dividend_capture.dsl")).unwrap();
    let r = run(&p, &ds, ExecConfig::from_program(&p)).unwrap();
    let declared: Vec<(i64, f64)> = r
        .events
        .iter()
        .filter_map(|e| match &e.kind {
            EventKind::DividendDeclared { receivable, .. } => Some((e.t, *receivable)),
            _ => None,
        })
        .collect();
    assert!(!declared.is_empty(), "the market pays dividends on a held name");
    // On every bar the NAV is cash, the marked positions and the receivables
    // (summed one holding at a time, hence the tolerance).
    let mut with_receivable = 0;
    for x in &r.exposure {
        let parts = x.cash + x.net + x.receivables;
        assert!((x.equity - parts).abs() <= 1e-9 * x.equity.abs().max(1.0), "{:?}", x);
        if x.receivables > 0.0 {
            with_receivable += 1;
        }
    }
    assert!(with_receivable > 0);
}
