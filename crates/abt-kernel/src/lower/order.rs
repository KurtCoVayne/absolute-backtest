//! Lowering of a plan's literal orders onto the kernel (compiler design,
//! section 9, step 7): the planner orders the surface literals of each
//! rule; this module turns a `Plan` into the `Lowered` form the kernel
//! applies, checking every order against well-modedness once more, so that
//! a defective plan can never bind a variable before its producer.

use std::collections::BTreeMap;

use super::{Lowered, Numerics};
use crate::check::Program;
use crate::plan::Plan;

/// A well-modedness check on an order: every literal's reads (the variables
/// it needs bound, by the written order's binding model) are bound by the
/// head's inputs and key or by an earlier literal of the order.
pub fn well_moded(prog: &Program, rule: usize, order: &[usize]) -> Result<(), String> {
    let deps = crate::plan::search::literal_deps(prog, rule);
    let r = &prog.rules[rule];
    if order.len() != r.body.len() {
        return Err(format!("order of rule {} has {} positions for {} literals", prog.rule_label(rule), order.len(), r.body.len()));
    }
    let mut seen = vec![false; r.body.len()];
    let mut bound: Vec<String> = deps.initially_bound.clone();
    for &li in order {
        if li >= r.body.len() || seen[li] {
            return Err(format!("order of rule {} is not a permutation", prog.rule_label(rule)));
        }
        seen[li] = true;
        let d = &deps.literals[li];
        if let Some(v) = d.reads.iter().find(|v| !bound.contains(v)) {
            return Err(format!("order of rule {} evaluates `{}` before `{}` is bound", prog.rule_label(rule), r.body[li].describe(), v));
        }
        for v in &d.binds {
            if !bound.contains(v) {
                bound.push(v.clone());
            }
        }
    }
    Ok(())
}

/// Lower a plan: its orders (each re-checked), its infeasible rules, the
/// aggregation literals that share a window state, and the folded program.
pub fn lower(prog: &Program, plan: &Plan, folded: Option<Program>, numerics: Numerics) -> Lowered {
    let mut low = Lowered::written(prog, numerics);
    low.label = "optimised".into();
    low.program = folded;
    for rp in &plan.rules {
        match well_moded(prog, rp.rule, &rp.order) {
            Ok(()) => low.order[rp.rule] = rp.order.clone(),
            Err(m) => low.notes.push(format!("{}; the written order is kept", m)),
        }
        if rp.infeasible {
            low.skip[rp.rule] = true;
        }
    }
    let mut shared: BTreeMap<(usize, usize), usize> = BTreeMap::new();
    for a in &plan.aggregates {
        if let Some(primary) = a.shares_state_of {
            if crate::kernel::eval::shareable_windows(&prog.rules[a.rule], primary, a.literal) {
                shared.insert((a.rule, a.literal), primary);
            } else {
                low.notes.push(format!(
                    "rule {}: aggregation literals {} and {} merge in NIR' but their variable slots differ, so they keep separate window states",
                    prog.rule_label(a.rule),
                    primary + 1,
                    a.literal + 1
                ));
            }
        }
    }
    low.shared_windows = shared;
    low
}
