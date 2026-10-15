//! R1, test hoisting (compiler design, section 4.1): a comparison, negation
//! or assignment moves to the earliest position at which all its inputs
//! are bound. The law is Kleene commutativity. A test bubbles leftward past
//! the generators before it while its inputs stay bound, and never past
//! another test, so tests keep their relative order; the measure is the
//! number of (generator, test) pairs in which the test could precede the
//! generator, which every swap decreases by one.

use std::collections::HashSet;

use super::RewriteLog;
use crate::check::Program;
use crate::compiler::normalise::{initially_bound, literal_reads_binds, Nir};
use crate::ir::Literal;

/// Whether a surface literal is a test the rule can hoist: a comparison, a
/// negation, an assignment, or a builtin (an assignment of a partial time
/// function in the core).
fn is_test(lit: &Literal) -> bool {
    matches!(lit, Literal::Cmp { .. } | Literal::Neg(_) | Literal::Assign { .. } | Literal::Builtin(..))
}

/// Apply R1 to every rule; true when something moved.
pub fn hoist(prog: &Program, nir: &mut Nir, log: &mut RewriteLog) -> bool {
    let mut any = false;
    for r in nir.rules.iter_mut() {
        let rule = &prog.rules[r.origin];
        let n = r.surface.len();
        let initial: HashSet<String> = initially_bound(prog, rule)
            .into_iter()
            .map(|v| r.names.iter().find(|(_, w)| *w == v).map(|(c, _)| c.clone()).unwrap_or(v))
            .collect();
        // Where each written literal started, for the log.
        let start: Vec<usize> = r.surface_origin.clone();
        let mut moved: Vec<(usize, usize)> = Vec::new();
        let mut swaps = 0usize;
        loop {
            let tests: Vec<bool> = r.surface.iter().map(is_test).collect();
            let mut bound: Vec<HashSet<String>> = Vec::with_capacity(n + 1);
            bound.push(initial.clone());
            let mut reads: Vec<Vec<String>> = Vec::with_capacity(n);
            for lit in &r.surface {
                let before = bound.last().unwrap().clone();
                let (rd, binds) = literal_reads_binds(prog, rule, lit, &before);
                let mut next = before;
                next.extend(binds);
                bound.push(next);
                reads.push(rd);
            }
            let Some(i) = (1..n).find(|&i| tests[i] && !tests[i - 1] && reads[i].iter().all(|v| bound[i - 1].contains(v))) else {
                break;
            };
            r.surface.swap(i - 1, i);
            r.surface_origin.swap(i - 1, i);
            for b in r.body.iter_mut() {
                if b.unit == i {
                    b.unit = i - 1;
                } else if b.unit == i - 1 {
                    b.unit = i;
                }
            }
            r.body.sort_by_key(|b| b.unit);
            swaps += 1;
            let origin = r.surface_origin[i - 1];
            match moved.iter_mut().find(|(o, _)| *o == origin) {
                Some(m) => m.1 = i - 1,
                None => moved.push((origin, i - 1)),
            }
            if swaps > n * n + 1 {
                break;
            }
        }
        for (origin, to) in moved {
            let from = start.iter().position(|&o| o == origin).unwrap_or(to);
            let pos = r.surface_origin.iter().position(|&o| o == origin).unwrap_or(to);
            let text = crate::compiler::print::render_literal(&r.surface[pos], &|v: &str| v.to_string());
            log.push(
                "R1",
                &r.id,
                &r.span,
                format!("hoist {}", text),
                format!("position {} → {}", from + 1, pos + 1),
                "Kleene commutativity of the body (compiler design, 4.1)",
                [from, pos],
            );
            any = true;
        }
    }
    any
}
