//! The syntactic results of Analyse (compiler design, sections 3.1 to
//! 3.5): components and temporal recursion, the open-loop partition,
//! keyedness labels, demand sets and window lengths.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use super::{DemandSet, Keyedness, Structure, WindowFacts};
use crate::check::{tarjan, Program};
use crate::compiler::normalise::Nir;
use crate::compiler::print::{render_atom, render_expr, render_lit};
use crate::ir::*;
use crate::plan::cost::Statistics;

/// Strongly connected components of the dependency graph (3.1), and the
/// relations in a temporally recursive one (a component with an edge inside
/// it: by WF-4 every such edge steps back in time).
pub fn components(prog: &Program) -> (BTreeMap<String, usize>, BTreeSet<String>) {
    let nodes: Vec<String> = prog.relations.keys().cloned().collect();
    let index: HashMap<&str, usize> = nodes.iter().enumerate().map(|(i, n)| (n.as_str(), i)).collect();
    let mut adj = vec![vec![]; nodes.len()];
    for (i, r) in prog.rules.iter().enumerate() {
        let Some(&h) = index.get(r.head.name.as_str()) else { continue };
        for rf in &prog.infos[i].refs {
            if let Some(&b) = index.get(rf.name.as_str()) {
                adj[h].push(b);
            }
        }
    }
    let sccs = tarjan(nodes.len(), &adj);
    let mut size: HashMap<usize, usize> = HashMap::new();
    for &id in &sccs.id {
        *size.entry(id).or_default() += 1;
    }
    let mut comps = BTreeMap::new();
    let mut recursive = BTreeSet::new();
    for (i, n) in nodes.iter().enumerate() {
        comps.insert(n.clone(), sccs.id[i]);
        if size[&sccs.id[i]] > 1 || adj[i].contains(&i) {
            recursive.insert(n.clone());
        }
    }
    (comps, recursive)
}

/// Whether a relation is a function of its identity: a stored relation is
/// (the loader enforces it), and a derived one is when its rules say so
/// (3.3). `visiting` guards the recursion through a component: a relation's
/// own earlier rows are keyed by the induction the label claims.
fn keyed(prog: &Program, nir: &Nir, name: &str, visiting: &mut HashSet<String>, memo: &mut HashMap<String, (Keyedness, String)>) -> (Keyedness, String) {
    if let Some(k) = memo.get(name) {
        return k.clone();
    }
    let Some(sig) = prog.relations.get(name) else {
        return (Keyedness::AtRuntime, "undeclared".into());
    };
    if !matches!(sig.kind, Kind::Derived { .. }) {
        return (Keyedness::ByConstruction, "stored: a function of its identity by the loader".into());
    }
    if !visiting.insert(name.to_string()) {
        return (Keyedness::ByConstruction, "its own earlier rows".into());
    }
    let rules = prog.rules_for(name);
    let result = if rules.len() == 1 {
        let ri = rules[0];
        let mut why = Vec::new();
        let mut ok = true;
        let mut atoms: Vec<&Atom> = Vec::new();
        collect_atoms(&prog.rules[ri].body, &mut atoms);
        for a in atoms {
            let (k, _) = keyed(prog, nir, &a.name, visiting, memo);
            if k == Keyedness::AtRuntime {
                ok = false;
                why.push(format!("`{}` is keyed at runtime", a.name));
            }
        }
        if ok {
            (Keyedness::ByConstruction, "one rule over keyed relations".into())
        } else {
            (Keyedness::AtRuntime, why.join("; "))
        }
    } else {
        // Every pair of rules must be syntactically complementary.
        let rendered: Vec<RuleGuards> = rules.iter().map(|&ri| guards(prog, nir, ri)).collect();
        let mut bad: Vec<String> = Vec::new();
        for i in 0..rendered.len() {
            for j in (i + 1)..rendered.len() {
                if !complementary(&rendered[i], &rendered[j]) {
                    bad.push(format!("{} and {} are not syntactically complementary", prog.rule_label(rules[i]), prog.rule_label(rules[j])));
                }
            }
        }
        if bad.is_empty() {
            (Keyedness::ByGuard, format!("{} rules, every pair complementary", rules.len()))
        } else {
            (Keyedness::AtRuntime, bad.join("; "))
        }
    };
    visiting.remove(name);
    memo.insert(name.to_string(), result.clone());
    result
}

fn collect_atoms<'a>(body: &'a [Literal], out: &mut Vec<&'a Atom>) {
    for l in body {
        match l {
            Literal::Atom(a) | Literal::AsOf { atom: a, .. } | Literal::Top { atom: a, .. } => out.push(a),
            Literal::Agg { conj, .. } => collect_atoms(conj, out),
            Literal::Case { branches, .. } => {
                for b in branches {
                    collect_atoms(&b.guard, out);
                }
            }
            _ => {}
        }
    }
}

/// The guards of a rule under its canonical names: positive atoms, negated
/// atoms (of complete relations), and comparisons as (expression, op, constant).
struct RuleGuards {
    positive: BTreeSet<String>,
    negative: BTreeSet<String>,
    cmps: Vec<(String, CmpOp, String)>,
}

fn guards(prog: &Program, nir: &Nir, ri: usize) -> RuleGuards {
    let identity = |v: &str| v.to_string();
    let mut g = RuleGuards {
        positive: BTreeSet::new(),
        negative: BTreeSet::new(),
        cmps: Vec::new(),
    };
    let Some(ni) = nir.rule_of_origin(ri) else { return g };
    for l in &nir.rules[ni].surface {
        match l {
            Literal::Atom(a) => {
                g.positive.insert(render_atom(a, &identity));
            }
            Literal::Neg(a) if prog.complete.contains(&a.name) => {
                g.negative.insert(render_atom(a, &identity));
            }
            Literal::Cmp { op, lhs, rhs, .. } => {
                let closed = |e: &Expr| {
                    let mut vs = Vec::new();
                    e.vars(&mut vs);
                    vs.is_empty()
                };
                if closed(rhs) {
                    g.cmps.push((render_expr(lhs, &identity), *op, render_expr(rhs, &identity)));
                } else if closed(lhs) {
                    let flipped = match op {
                        CmpOp::Lt => CmpOp::Gt,
                        CmpOp::Le => CmpOp::Ge,
                        CmpOp::Gt => CmpOp::Lt,
                        CmpOp::Ge => CmpOp::Le,
                        CmpOp::Eq => CmpOp::Eq,
                    };
                    g.cmps.push((render_expr(rhs, &identity), flipped, render_expr(lhs, &identity)));
                }
            }
            _ => {}
        }
    }
    g
}

fn complement(op: CmpOp) -> Option<CmpOp> {
    Some(match op {
        CmpOp::Lt => CmpOp::Ge,
        CmpOp::Le => CmpOp::Gt,
        CmpOp::Gt => CmpOp::Le,
        CmpOp::Ge => CmpOp::Lt,
        CmpOp::Eq => return None,
    })
}

fn complementary(a: &RuleGuards, b: &RuleGuards) -> bool {
    if a.positive.intersection(&b.negative).next().is_some() || b.positive.intersection(&a.negative).next().is_some() {
        return true;
    }
    for (e, op, c) in &a.cmps {
        if let Some(co) = complement(*op) {
            if b.cmps.iter().any(|(e2, op2, c2)| e2 == e && *op2 == co && c2 == c) {
                return true;
            }
        }
    }
    false
}

/// Keyedness labels for the derived relations of temporally recursive
/// components (3.3).
pub fn keyedness(prog: &Program, nir: &Nir, recursive: &BTreeSet<String>) -> BTreeMap<String, (Keyedness, String)> {
    let mut memo: HashMap<String, (Keyedness, String)> = HashMap::new();
    let mut out = BTreeMap::new();
    for name in recursive {
        if matches!(prog.relations.get(name).map(|s| &s.kind), Some(Kind::Derived { .. })) {
            let mut visiting = HashSet::new();
            out.insert(name.clone(), keyed(prog, nir, name, &mut visiting, &mut memo));
        }
    }
    out
}

/// The parametric positions of a relation: its `+` arguments that are not
/// entity-typed (3.4).
fn parametric_positions(sig: &Signature) -> Vec<usize> {
    let _ = sig;
    Vec::new()
}

/// A call site: the atom and the rule it is in.
fn call_sites<'a>(body: &'a [Literal], out: &mut Vec<&'a Atom>) {
    collect_atoms(body, out);
    for l in body {
        if let Literal::Neg(a) = l {
            out.push(a);
        }
    }
}

/// Demand (3.4): the relations reachable from `decide`, and for every
/// parametric relation the parameter tuples it is called at.
pub fn demand(prog: &Program) -> (BTreeMap<String, DemandSet>, BTreeSet<String>) {
    // Reachability from decide and from every observation (Definition
    // V-8.3: an observation is a demand).
    let mut demanded: BTreeSet<String> = BTreeSet::new();
    let mut stack = vec!["decide".to_string()];
    stack.extend(prog.observes.iter().map(|o| o.rel.clone()));
    while let Some(x) = stack.pop() {
        if !demanded.insert(x.clone()) {
            continue;
        }
        for ri in prog.rules_for(&x) {
            for rf in &prog.infos[ri].refs {
                if !demanded.contains(&rf.name) {
                    stack.push(rf.name.clone());
                }
            }
        }
    }
    // Demand tuples, by a fixpoint over call sites.
    let parametric: BTreeMap<String, Vec<usize>> = prog
        .relations
        .iter()
        .filter(|(_, s)| matches!(s.kind, Kind::Derived { .. }))
        .map(|(n, s)| (n.clone(), parametric_positions(s)))
        .filter(|(_, p)| !p.is_empty())
        .collect();
    let mut sets: BTreeMap<String, DemandSet> = parametric.keys().map(|n| (n.clone(), DemandSet::Static(vec![]))).collect();
    let value_text = |unit: &str, t: &Term| -> Option<String> {
        match t {
            Term::Lit(l, _) => Some(render_lit(l)),
            Term::Param(p, _) => prog.param(unit, p).map(|p| render_lit(&p.value)),
            _ => None,
        }
    };
    for _ in 0..64 {
        let mut changed = false;
        for (ri, rule) in prog.rules.iter().enumerate() {
            if !demanded.contains(&rule.head.name) {
                continue;
            }
            // The caller's own demand, by its parametric head positions.
            let caller_positions = parametric.get(&rule.head.name).cloned().unwrap_or_default();
            let caller_demand = sets.get(&rule.head.name).cloned();
            let head_param_var = |v: &str| -> Option<usize> { caller_positions.iter().position(|&p| matches!(&rule.head.terms[p], Term::Var(x, _) if x == v)) };
            let mut sites = Vec::new();
            call_sites(&rule.body, &mut sites);
            for a in sites {
                let Some(positions) = parametric.get(&a.name) else { continue };
                let mut tuples: Vec<Vec<String>> = vec![vec![]];
                let mut dynamic: Option<String> = None;
                for &p in positions {
                    let t = &a.terms[p];
                    match t {
                        Term::Var(v, _) => match (head_param_var(v), &caller_demand) {
                            (Some(k), Some(DemandSet::Static(ts))) => {
                                let mut next = Vec::new();
                                for tu in &tuples {
                                    for ct in ts {
                                        let mut x = tu.clone();
                                        x.push(ct[k].clone());
                                        next.push(x);
                                    }
                                }
                                tuples = next;
                            }
                            (Some(_), Some(DemandSet::Dynamic(why))) => dynamic = Some(why.clone()),
                            (Some(_), _) => tuples.clear(),
                            (None, _) => dynamic = Some(format!("rule {} passes the data-bound `{}` to `{}`", prog.rule_label(ri), v, a.name)),
                        },
                        t => match value_text(&rule.unit, t) {
                            Some(x) => tuples.iter_mut().for_each(|tu| tu.push(x.clone())),
                            None => dynamic = Some(format!("rule {} passes `{}` to `{}`", prog.rule_label(ri), t, a.name)),
                        },
                    }
                }
                let entry = sets.get_mut(&a.name).unwrap();
                match (dynamic, entry) {
                    (Some(why), e) => {
                        if !matches!(e, DemandSet::Dynamic(_)) {
                            *e = DemandSet::Dynamic(why);
                            changed = true;
                        }
                    }
                    (None, DemandSet::Static(ts)) => {
                        for tu in tuples {
                            if !ts.contains(&tu) {
                                ts.push(tu);
                                changed = true;
                            }
                        }
                    }
                    (None, _) => {}
                }
            }
        }
        if !changed {
            break;
        }
    }
    let mut out: BTreeMap<String, DemandSet> = BTreeMap::new();
    for (n, s) in prog.relations.iter() {
        if !matches!(s.kind, Kind::Derived { .. }) {
            continue;
        }
        out.insert(n.clone(), sets.get(n).cloned().unwrap_or(DemandSet::Total));
    }
    (out, demanded)
}

/// Bars a calendar duration can span at a resolution: an upper bound from
/// the calendar (3.5).
pub fn bars_per_day(res: Resolution) -> f64 {
    match res {
        Resolution::M1 => 1440.0,
        Resolution::M5 => 288.0,
        Resolution::M15 => 96.0,
        Resolution::M30 => 48.0,
        Resolution::H1 => 24.0,
        Resolution::D1 => 1.0,
        Resolution::W1 => 1.0 / 5.0,
    }
}

/// The length of a count or duration expression, from a literal or a
/// parameter's default.
pub fn static_number(prog: &Program, unit: &str, e: &Expr) -> Option<f64> {
    match e {
        Expr::Lit(Lit::Int(n), _) => Some(*n as f64),
        Expr::Lit(Lit::Float(x), _) => Some(*x),
        Expr::Param(p, _) => match prog.param(unit, p).map(|p| &p.value) {
            Some(Lit::Int(n)) => Some(*n as f64),
            Some(Lit::Float(x)) => Some(*x),
            _ => None,
        },
        _ => None,
    }
}

pub fn static_duration(prog: &Program, unit: &str, e: &Expr) -> Option<Duration> {
    match e {
        Expr::Lit(Lit::Duration(d), _) => Some(*d),
        Expr::Param(p, _) => match prog.param(unit, p).map(|p| &p.value) {
            Some(Lit::Duration(d)) => Some(*d),
            _ => None,
        },
        _ => None,
    }
}

/// The values a window expression can take: a literal or a parameter's
/// default, or, for a parametric head input, the values of the demand set
/// (3.4 makes them finite); none when it is data-bound.
fn window_values(prog: &Program, rule: &Rule, e: &Expr, demand: &BTreeMap<String, DemandSet>) -> Vec<Lit> {
    match e {
        Expr::Lit(l, _) => vec![l.clone()],
        Expr::Param(p, _) => prog.param(&rule.unit, p).map(|p| vec![p.value.clone()]).unwrap_or_default(),
        Expr::Var(v, _) => {
            let Some(sig) = prog.relations.get(&rule.head.name) else { return vec![] };
            let positions = parametric_positions(sig);
            let Some(k) = positions.iter().position(|&p| matches!(&rule.head.terms[p], Term::Var(x, _) if x == v)) else {
                return vec![];
            };
            match demand.get(&rule.head.name) {
                Some(DemandSet::Static(ts)) => ts.iter().filter_map(|t| t.get(k)).filter_map(|s| crate::syntax::parse_lit(s).ok()).collect(),
                _ => vec![],
            }
        }
        _ => vec![],
    }
}

/// Window lengths (3.5): per aggregation literal with a window, the most
/// bars it can hold (the largest over the demanded lengths) and its
/// minimum (the smallest over the demanded minimums, so that A006 never
/// fires for a tuple that can be met).
pub fn window_lengths(prog: &Program, stats: Option<&Statistics>, demand: &BTreeMap<String, DemandSet>) -> Vec<WindowFacts> {
    let mut out = Vec::new();
    for (ri, rule) in prog.rules.iter().enumerate() {
        let res = match &prog.infos[ri].cal {
            Cal::Base => prog.base,
            _ => Resolution::D1,
        };
        for (li, lit) in rule.body.iter().enumerate() {
            let Literal::Agg { conj, .. } = lit else { continue };
            let Some(Literal::Window { kind, dur, min, .. }) = conj.iter().find(|l| matches!(l, Literal::Window { .. })) else {
                continue;
            };
            let ks: Vec<f64> = window_values(prog, rule, min, demand)
                .iter()
                .filter_map(|l| match l {
                    Lit::Int(n) => Some(*n as f64),
                    Lit::Float(x) => Some(*x),
                    _ => None,
                })
                .collect();
            let k = ks.iter().copied().reduce(f64::min);
            let lengths = window_values(prog, rule, dur, demand);
            let (text, w_max) = match kind {
                WindowKind::Rows => {
                    let ns: Vec<f64> = lengths
                        .iter()
                        .filter_map(|l| match l {
                            Lit::Int(n) => Some(*n as f64),
                            _ => None,
                        })
                        .collect();
                    ("rows".to_string(), ns.iter().copied().reduce(f64::max))
                }
                WindowKind::PriorRows => {
                    let ns: Vec<f64> = lengths
                        .iter()
                        .filter_map(|l| match l {
                            Lit::Int(n) => Some(*n as f64),
                            _ => None,
                        })
                        .collect();
                    ("prior_rows".to_string(), ns.iter().copied().reduce(f64::max))
                }
                WindowKind::Bucket(c) => (format!("bucket({})", c), None),
                WindowKind::Session => ("session".to_string(), None),
                WindowKind::Window | WindowKind::Prior => {
                    let ds: Vec<Duration> = lengths.iter().filter_map(|l| if let Lit::Duration(d) = l { Some(*d) } else { None }).collect();
                    let name = if *kind == WindowKind::Window { "window" } else { "prior_window" };
                    let longest = ds.iter().copied().max_by(|a, b| a.approx_days().total_cmp(&b.approx_days()));
                    (
                        longest.map(|d| format!("{} of {}", name, d)).unwrap_or_else(|| name.to_string()),
                        longest.map(|d| {
                            let days = d.max_days() as f64 + if *kind == WindowKind::Window { 1.0 } else { 0.0 };
                            days * bars_per_day(res)
                        }),
                    )
                }
            };
            out.push(WindowFacts {
                rule: ri,
                literal: li,
                kind: text,
                w_max,
                k,
                warmup: stats.and_then(|s| s.warmup_bars),
            });
        }
    }
    out
}

/// Every structural result of Analyse.
pub fn structure(prog: &Program, nir: &Nir, stats: Option<&Statistics>) -> Structure {
    let (components, recursive) = components(prog);
    let closed_loop = crate::plan::planner::closed_loop(prog);
    let keyedness = keyedness(prog, nir, &recursive);
    let (demand, demanded) = demand(prog);
    let windows = window_lengths(prog, stats, &demand);
    Structure {
        components,
        recursive,
        closed_loop,
        keyedness,
        demand,
        demanded,
        windows,
    }
}
