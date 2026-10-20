//! The order search (compiler design, section 5.3): dynamic programming
//! over subsets of a rule's literals, exact under the well-modedness
//! constraint, with a greedy fallback for rules of more than fourteen
//! literals. The binding model is the kernel's: the head's inputs and key
//! are bound on entry; every literal reads the variables it mentions that
//! an earlier literal (in the written order) bound, and binds the rest.

use std::collections::HashMap;

use super::cost::{LiteralCost, Statistics};
use super::SearchKind;
use crate::check::Program;
use crate::ir::*;
use crate::kernel::eval::literal_vars;

/// What one literal needs bound and what it binds.
#[derive(Clone, Debug, Default)]
pub struct LitDeps {
    pub reads: Vec<String>,
    pub binds: Vec<String>,
}

/// The binding structure of a rule's body.
#[derive(Clone, Debug, Default)]
pub struct RuleDeps {
    pub initially_bound: Vec<String>,
    pub literals: Vec<LitDeps>,
}

/// Above this many literals the search is greedy.
pub const EXACT_LIMIT: usize = 14;

/// The variables a literal could bind if they are not yet bound: every
/// variable it mentions, except those a literal only ever reads.
fn bindable(lit: &Literal) -> Vec<String> {
    let mut vars = Vec::new();
    match lit {
        Literal::Neg(_) | Literal::Cmp { .. } => {}
        Literal::Builtin(b, _) => match b {
            Builtin::Prev { t1, .. } | Builtin::Lag { t1, .. } => {
                if let Term::Var(v, _) = t1 {
                    vars.push(v.clone());
                }
            }
            Builtin::MonthStart { .. } | Builtin::DayStart { .. } => {}
            Builtin::SessionOf { s, .. } => {
                if let Term::Var(v, _) = s {
                    vars.push(v.clone());
                }
            }
        },
        Literal::Assign { var, .. } => vars.push(var.clone()),
        Literal::Agg { var, .. } => vars.push(var.clone()),
        Literal::Atom(a) | Literal::AsOf { atom: a, .. } => {
            for t in &a.terms {
                term_vars(t, &mut vars);
            }
        }
        Literal::Top { atom, rank, .. } => {
            for t in &atom.terms {
                term_vars(t, &mut vars);
            }
            if let Some((k, _, _)) = rank {
                vars.push(k.clone());
            }
        }
        Literal::Case { var, .. } => vars.push(var.clone()),
        Literal::Window { var, .. } => {
            if let Term::Var(v, _) = var {
                vars.push(v.clone());
            }
        }
    }
    vars
}

fn term_vars(t: &Term, out: &mut Vec<String>) {
    match t {
        Term::Var(v, _) => out.push(v.clone()),
        Term::Ctor(_, subs, _) => subs.iter().for_each(|s| term_vars(s, out)),
        _ => {}
    }
}

/// The reads and binds of every literal of a rule, by the written order.
pub fn literal_deps(prog: &Program, rule: usize) -> RuleDeps {
    let r = &prog.rules[rule];
    let mut bound: Vec<String> = Vec::new();
    if let Some(sig) = prog.relations.get(&r.head.name) {
        for (term, arg) in r.head.terms.iter().zip(sig.args.iter()) {
            if let Term::Var(v, _) = term {
                if arg.mode == Mode::Key {
                    bound.push(v.clone());
                }
            }
        }
    }
    let initially_bound = bound.clone();
    let mut literals = Vec::with_capacity(r.body.len());
    for lit in &r.body {
        // The variables local to an aggregation's group are neither read nor
        // bound at the rule's level.
        let mentioned: Vec<String> = match lit {
            Literal::Agg { var, args, conj, .. } => {
                let mut inner = Vec::new();
                for c in conj {
                    literal_vars(c, &mut inner);
                }
                for a in args {
                    let mut vs = Vec::new();
                    a.vars(&mut vs);
                    inner.extend(vs.into_iter().map(|(v, _)| v));
                }
                let mut m: Vec<String> = inner.into_iter().filter(|v| bound.contains(v)).collect();
                m.push(var.clone());
                m
            }
            // A case form's guards bind their own locals (the branch values
            // read them): at the rule's level it reads what is bound outside
            // and binds its variable.
            Literal::Case { var, branches, otherwise, .. } => {
                let mut inner = Vec::new();
                for b in branches {
                    for g in &b.guard {
                        literal_vars(g, &mut inner);
                    }
                    let mut vs = Vec::new();
                    b.value.vars(&mut vs);
                    inner.extend(vs.into_iter().map(|(v, _)| v));
                }
                if let Some(e) = otherwise {
                    let mut vs = Vec::new();
                    e.vars(&mut vs);
                    inner.extend(vs.into_iter().map(|(v, _)| v));
                }
                let mut m: Vec<String> = inner.into_iter().filter(|v| bound.contains(v)).collect();
                m.push(var.clone());
                m
            }
            _ => {
                let mut m = Vec::new();
                literal_vars(lit, &mut m);
                m
            }
        };
        let can_bind = bindable(lit);
        let mut reads = Vec::new();
        let mut binds = Vec::new();
        for v in mentioned {
            if bound.contains(&v) {
                if !reads.contains(&v) {
                    reads.push(v);
                }
            } else if can_bind.contains(&v) {
                if !binds.contains(&v) {
                    binds.push(v);
                }
            } else if !reads.contains(&v) {
                // Read before bound: the checker would have refused it; the
                // order search then keeps the literal where it is written.
                reads.push(v);
            }
        }
        for v in &binds {
            bound.push(v.clone());
        }
        literals.push(LitDeps { reads, binds });
    }
    // A time builtin or a duration window on the per-instrument sessions
    // calendar steps over the sessions of the head's instrument (the
    // kernel's `domain_of_term`): it reads the head's entity variable, so it
    // waits for the literal that binds it. Placed before it, the instrument
    // unbound, it stepped over every instrument's sessions at once (E-107).
    let entity = prog.relations.get(&r.head.name).and_then(|sig| {
        sig.args.iter().zip(&r.head.terms).find_map(|(a, t)| match t {
            Term::Var(v, _) if a.ty.is_entity() => Some(v.clone()),
            _ => None,
        })
    });
    if let Some(e) = entity.filter(|e| initially_bound.contains(e) || literals.iter().any(|d| d.binds.contains(e))) {
        for (lit, d) in r.body.iter().zip(literals.iter_mut()) {
            let t = match lit {
                Literal::Builtin(Builtin::Prev { t, .. } | Builtin::Lag { t, .. } | Builtin::MonthStart { t } | Builtin::DayStart { t }, _) => t,
                Literal::Window {
                    kind: WindowKind::Window | WindowKind::Prior,
                    base,
                    ..
                } => base,
                _ => continue,
            };
            let cal = match t {
                Term::Var(v, _) => prog.var_cals.get(rule).and_then(|m| m.get(v)).cloned(),
                _ => None,
            }
            .unwrap_or_else(|| prog.infos[rule].cal.clone());
            if cal.per_entity() && !d.reads.contains(&e) && !d.binds.contains(&e) {
                d.reads.push(e.clone());
            }
        }
    }
    RuleDeps { initially_bound, literals }
}

/// The result of an order search.
#[derive(Clone, Debug)]
pub struct Searched {
    pub order: Vec<usize>,
    pub cost: f64,
    pub kind: SearchKind,
    pub considered: usize,
}

/// Whether literal `li` may be placed next, given the set of placed literals.
fn available(deps: &RuleDeps, placed: u32, li: usize) -> bool {
    let d = &deps.literals[li];
    d.reads
        .iter()
        .all(|v| deps.initially_bound.contains(v) || (0..deps.literals.len()).any(|j| placed & (1 << j) != 0 && deps.literals[j].binds.contains(v)))
}

/// The cost of an order: Σ card(i−1) · cost(L_i), card multiplying by each
/// literal's cardinality factor (section 5.2).
pub fn order_cost(costs: &[LiteralCost], order: &[usize]) -> f64 {
    let mut card = 1.0;
    let mut total = 0.0;
    for &li in order {
        total += card * costs[li].cost;
        card *= costs[li].factor;
    }
    total
}

/// Find the cheapest well-moded order: exact dynamic programming over
/// subsets for up to `EXACT_LIMIT` literals (the cheapest order of a subset
/// is extended one literal at a time; the cardinality after a subset is
/// the product of its factors, so the sub-problem is well defined), greedy
/// above it.
pub fn search(deps: &RuleDeps, costs: &[LiteralCost], _stats: &Statistics) -> Searched {
    let n = deps.literals.len();
    if n == 0 {
        return Searched {
            order: vec![],
            cost: 0.0,
            kind: SearchKind::Exact,
            considered: 1,
        };
    }
    if n > EXACT_LIMIT {
        let mut placed = 0u32;
        let mut order = Vec::with_capacity(n);
        let mut considered = 0;
        while order.len() < n {
            let mut best: Option<(usize, f64)> = None;
            for (li, lc) in costs.iter().enumerate() {
                if placed & (1 << li) != 0 || !available(deps, placed, li) {
                    continue;
                }
                considered += 1;
                // Cheapest next literal, by its cost times its factor
                // (an exclusion is worth taking early).
                let key = lc.cost * lc.factor.max(1e-9);
                if best.map(|(_, k)| key < k).unwrap_or(true) {
                    best = Some((li, key));
                }
            }
            let Some((li, _)) = best else {
                // Nothing is available: keep the written order for the rest.
                for li in 0..n {
                    if placed & (1 << li) == 0 {
                        order.push(li);
                        placed |= 1 << li;
                    }
                }
                break;
            };
            order.push(li);
            placed |= 1 << li;
        }
        let cost = order_cost(costs, &order);
        return Searched {
            order,
            cost,
            kind: SearchKind::Greedy,
            considered,
        };
    }
    // best[mask] = (cost of the cheapest order of the literals in mask, the last literal placed).
    let full: u32 = (1u32 << n) - 1;
    let mut best: HashMap<u32, (f64, Option<usize>, u32)> = HashMap::new();
    best.insert(0, (0.0, None, 0));
    let mut considered = 0;
    // Masks in order of population count so that every predecessor is done.
    let mut masks: Vec<u32> = (0..=full).collect();
    masks.sort_by_key(|m| m.count_ones());
    for mask in masks {
        let Some(&(cost, _, _)) = best.get(&mask) else { continue };
        let card: f64 = (0..n).filter(|j| mask & (1 << j) != 0).map(|j| costs[j].factor).product();
        for (li, lc) in costs.iter().enumerate() {
            if mask & (1 << li) != 0 || !available(deps, mask, li) {
                continue;
            }
            considered += 1;
            let next = mask | (1 << li);
            let c = cost + card * lc.cost;
            let better = best.get(&next).map(|(bc, _, _)| c < *bc).unwrap_or(true);
            if better {
                best.insert(next, (c, Some(li), mask));
            }
        }
    }
    let Some(&(cost, _, _)) = best.get(&full) else {
        // The written order is the only well-moded one the model admits.
        let order: Vec<usize> = (0..n).collect();
        let cost = order_cost(costs, &order);
        return Searched {
            order,
            cost,
            kind: SearchKind::Written,
            considered,
        };
    };
    let mut order = Vec::with_capacity(n);
    let mut mask = full;
    while let Some(&(_, Some(li), prev)) = best.get(&mask) {
        order.push(li);
        mask = prev;
    }
    order.reverse();
    Searched {
        order,
        cost,
        kind: SearchKind::Exact,
        considered,
    }
}
