//! Tier 4 (compiler design, section 6.5): invariants over temporally
//! recursive relations as constrained Horn clauses. One uninterpreted
//! predicate per derived relation of a recursive component, over the bar
//! index and the relation's numeric value columns; one clause per rule,
//! with the arithmetic part of the body, `prev`/`lag` as steps of the bar
//! index, Analyse's intervals for the relations outside the component,
//! and the candidate invariant as the query. A `sat` answer is a model: an
//! inductive invariant, kept as the certificate, which refines Analyse's
//! interval for the column (the A010 note). The tier is budgeted and never
//! load-bearing: nothing in the plan depends on it.

use std::collections::BTreeMap;

use super::obligations::{bounds, cmp, magnitude, q_of_lit, Smt};
use super::smt::{numeral, symbol, Driver, Script, SolverAnswer};
use super::{Obligation, TierReport, Verdict};
use crate::analysis::domains::Interval;
use crate::analysis::Analysis;
use crate::check::Program;
use crate::ir::{Builtin, CmpOp, Expr, Literal, Term};

/// A predicate: a relation of the component over its bar index and its
/// numeric value columns (positions and names).
struct Pred {
    name: String,
    symbol: String,
    cols: Vec<(usize, String)>,
}

/// The recursive components with rules, in component order.
fn components(prog: &Program, analysis: &Analysis) -> Vec<Vec<String>> {
    let mut by: BTreeMap<usize, Vec<String>> = BTreeMap::new();
    for r in &analysis.relations {
        if r.recursive && r.kind == "derived" && !prog.rules_for(&r.name).is_empty() {
            by.entry(r.component).or_default().push(r.name.clone());
        }
    }
    by.into_values().collect()
}

fn preds_of(prog: &Program, analysis: &Analysis, members: &[String]) -> Vec<Pred> {
    let mut out = Vec::new();
    for name in members {
        let Some(sig) = prog.relations.get(name) else { continue };
        let Some(facts) = analysis.relation(name) else { continue };
        let ids = sig.identity_positions();
        let cols: Vec<(usize, String)> = sig
            .args
            .iter()
            .enumerate()
            .filter(|(i, _)| !ids.contains(i))
            .filter(|(i, _)| facts.columns.get(*i).map(|c| c.value.numeric).unwrap_or(false))
            .map(|(i, a)| (i, a.name.clone()))
            .collect();
        out.push(Pred {
            name: name.clone(),
            symbol: format!("P_{}", symbol(name)),
            cols,
        });
    }
    out
}

fn conj(parts: &[String]) -> String {
    match parts.len() {
        0 => "true".into(),
        1 => parts[0].clone(),
        _ => format!("(and {})", parts.join(" ")),
    }
}

/// A term of an atom or head at a numeric value position.
fn value_term(ctx: &mut Smt, t: &Term, s: &mut Script) -> String {
    match t {
        Term::Var(v, _) => ctx.var(v, s),
        Term::Lit(l, _) => q_of_lit(l).map(numeral).unwrap_or_else(|| ctx.fresh("lit", s)),
        Term::Param(p, _) => ctx.param(p, s).unwrap_or_else(|| ctx.fresh("param", s)),
        Term::Wild(_) | Term::Ctor(..) => ctx.fresh("any", s),
    }
}

/// One rule as a clause `∀ vars. body ⇒ P(b, values)`; `None` when the
/// head's bar is not a variable (nothing to step on).
fn clause(prog: &Program, analysis: &Analysis, preds: &[Pred], ri: usize, nonlinear: &mut bool) -> Option<String> {
    let rule = &prog.rules[ri];
    let sig = prog.relations.get(&rule.head.name)?;
    let pred = preds.iter().find(|p| p.name == rule.head.name)?;
    let mut s = Script::new("HORN");
    let mut ctx = Smt::new(prog, &rule.unit, "c_");
    ctx.quantified = Some(BTreeMap::new());
    let key = sig.key_pos()?;
    let Term::Var(tv, _) = &rule.head.terms[key] else { return None };
    let b = ctx.bind_sort(tv, "Int", &mut s);
    let mut body: Vec<String> = Vec::new();
    let mut consts: Vec<f64> = Vec::new();
    let facts = analysis.rules.iter().find(|r| r.rule == ri);
    for (li, l) in rule.body.iter().enumerate() {
        match l {
            Literal::Builtin(Builtin::Prev { t, t1 }, _) => {
                if let (Term::Var(x, _), Term::Var(y, _)) = (t, t1) {
                    let (bx, by) = (ctx.bind_sort(x, "Int", &mut s), ctx.bind_sort(y, "Int", &mut s));
                    body.push(format!("(= {} (- {} 1))", by, bx));
                }
            }
            Literal::Builtin(Builtin::Lag { t, n, t1 }, _) => {
                if let (Term::Var(x, _), Term::Var(y, _)) = (t, t1) {
                    let (bx, by) = (ctx.bind_sort(x, "Int", &mut s), ctx.bind_sort(y, "Int", &mut s));
                    match crate::analysis::structure::static_number(prog, &rule.unit, n) {
                        Some(k) if k.fract() == 0.0 && k >= 1.0 => body.push(format!("(= {} (- {} {}))", by, bx, k as i64)),
                        _ => body.push(format!("(< {} {})", by, bx)),
                    }
                }
            }
            Literal::Builtin(..) => {}
            Literal::Window { var, base, .. } => {
                if let (Term::Var(x, _), Term::Var(y, _)) = (var, base) {
                    let (bx, by) = (ctx.bind_sort(x, "Int", &mut s), ctx.bind_sort(y, "Int", &mut s));
                    body.push(format!("(<= {} {})", bx, by));
                }
            }
            Literal::Atom(a) => {
                if let Some(p) = preds.iter().find(|p| p.name == a.name) {
                    let Some(kp) = prog.relations.get(&a.name).and_then(|s| s.key_pos()) else { continue };
                    if let Some(Term::Var(v, _)) = a.terms.get(kp) {
                        let bt = ctx.bind_sort(v, "Int", &mut s);
                        let mut args = vec![bt];
                        for (ci, _) in &p.cols {
                            args.push(a.terms.get(*ci).map(|t| value_term(&mut ctx, t, &mut s)).unwrap_or_else(|| ctx.fresh("any", &mut s)));
                        }
                        body.push(format!("({} {})", p.symbol, args.join(" ")));
                    }
                } else if let Some(rf) = analysis.relation(&a.name) {
                    for (i, t) in a.terms.iter().enumerate() {
                        if let (Term::Var(v, _), Some(col)) = (t, rf.columns.get(i)) {
                            if col.value.numeric {
                                let name = ctx.var(v, &mut s);
                                body.extend(bounds(&name, col.value.interval, None, &mut consts));
                            }
                        }
                    }
                }
            }
            Literal::Cmp { op, lhs, rhs, .. } => {
                if let (Some(x), Some(y)) = (ctx.term(lhs, &mut s), ctx.term(rhs, &mut s)) {
                    body.push(cmp(*op, &x, &y, None));
                }
            }
            Literal::Assign { var, expr, .. } => {
                if let Some(e) = ctx.term(expr, &mut s) {
                    let v = ctx.var(var, &mut s);
                    body.push(cmp(CmpOp::Eq, &v, &e, None));
                }
            }
            Literal::Agg { var, .. } => {
                if let Some(st) = facts.and_then(|f| f.literals.iter().find(|x| x.index == li)) {
                    if let Some((_, val)) = st.state.iter().find(|(n, _)| n == var) {
                        if val.numeric {
                            let name = ctx.var(var, &mut s);
                            body.extend(bounds(&name, val.interval, None, &mut consts));
                        }
                    }
                }
            }
            Literal::Neg(_) | Literal::Top { .. } | Literal::AsOf { .. } | Literal::Case { .. } => {}
        }
    }
    let mut args = vec![b];
    for (ci, _) in &pred.cols {
        args.push(rule.head.terms.get(*ci).map(|t| value_term(&mut ctx, t, &mut s)).unwrap_or_else(|| ctx.fresh("any", &mut s)));
    }
    body.append(&mut ctx.pending);
    if ctx.nonlinear {
        *nonlinear = true;
    }
    let head = format!("({} {})", pred.symbol, args.join(" "));
    let binders: Vec<String> = ctx.quantified.as_ref().map(|q| q.iter().map(|(n, sort)| format!("({} {})", n, sort)).collect()).unwrap_or_default();
    Some(if binders.is_empty() {
        format!("(=> {} {})", conj(&body), head)
    } else {
        format!("(forall ({}) (=> {} {}))", binders.join(" "), conj(&body), head)
    })
}

/// The constants the rules of a relation mention: candidate bounds.
fn constants_of(prog: &Program, rules: &[usize]) -> Vec<f64> {
    fn walk(e: &Expr, out: &mut Vec<f64>) {
        match e {
            Expr::Lit(l, _) => out.extend(magnitude(l)),
            Expr::Var(..) | Expr::Param(..) => {}
            Expr::Neg(x, _) => walk(x, out),
            Expr::Bin(_, a, b, _) => {
                walk(a, out);
                walk(b, out);
            }
            Expr::Call(_, args, _) => args.iter().for_each(|a| walk(a, out)),
        }
    }
    let mut out = vec![0.0];
    for &ri in rules {
        let r = &prog.rules[ri];
        for l in &r.body {
            match l {
                Literal::Cmp { lhs, rhs, .. } => {
                    walk(lhs, &mut out);
                    walk(rhs, &mut out);
                }
                Literal::Assign { expr, .. } => walk(expr, &mut out),
                _ => {}
            }
        }
        for t in &r.head.terms {
            if let Term::Lit(l, _) = t {
                out.extend(magnitude(l));
            }
        }
        for ps in prog.params.values() {
            for p in ps.values() {
                if !r.body.iter().any(|l| mentions_param(l, &p.name)) {
                    continue;
                }
                if let Some((lo, hi)) = &p.range {
                    out.extend(magnitude(lo));
                    out.extend(magnitude(hi));
                }
                out.extend(magnitude(&p.value));
            }
        }
    }
    out.retain(|x| x.is_finite());
    out.sort_by(|a, b| a.total_cmp(b));
    out.dedup();
    out
}

fn mentions_param(l: &Literal, name: &str) -> bool {
    let mut ps = Vec::new();
    match l {
        Literal::Cmp { lhs, rhs, .. } => {
            lhs.params(&mut ps);
            rhs.params(&mut ps);
        }
        Literal::Assign { expr, .. } => expr.params(&mut ps),
        _ => {}
    }
    ps.iter().any(|p| p == name)
}

/// The tier over every recursive component: for each numeric column with
/// an infinite bound, the candidate bounds strongest first, until one is
/// proved.
pub fn tier4(prog: &Program, analysis: &mut Analysis, driver: &Driver) -> TierReport {
    let mut t = TierReport::new(4);
    if let Some(n) = driver.absent_note() {
        t.notes.push(n);
    }
    let comps = components(prog, analysis);
    if comps.is_empty() {
        t.notes.push("no temporally recursive derived relation".into());
        return t;
    }
    for members in comps {
        let preds = preds_of(prog, analysis, &members);
        if preds.iter().all(|p| p.cols.is_empty()) {
            t.notes.push(format!("component {{{}}}: no numeric value column", members.join(", ")));
            continue;
        }
        let mut base = Script::new("HORN");
        for p in &preds {
            let sorts: Vec<&str> = std::iter::once("Int").chain(p.cols.iter().map(|_| "Real")).collect();
            base.declare_fun(&p.symbol, &sorts, "Bool");
        }
        let mut nonlinear = false;
        let mut clauses = 0;
        for name in &members {
            for ri in prog.rules_for(name) {
                if let Some(c) = clause(prog, analysis, &preds, ri, &mut nonlinear) {
                    base.assert(c);
                    clauses += 1;
                }
            }
        }
        if clauses == 0 {
            t.notes.push(format!("component {{{}}}: no rule encodes as a clause", members.join(", ")));
            continue;
        }
        if nonlinear {
            t.notes
                .push(format!("component {{{}}}: the clauses carry nonlinear arithmetic; the solver may not decide them", members.join(", ")));
        }
        for p in &preds {
            let rules = prog.rules_for(&p.name);
            let consts = constants_of(prog, &rules);
            for (ci, col) in &p.cols {
                let interval = analysis.relation(&p.name).and_then(|f| f.columns.get(*ci)).map(|c| c.value.interval).unwrap_or(Interval::TOP);
                let mut sides: Vec<(&str, Vec<f64>)> = Vec::new();
                // Zero first (the sign invariants are the common case), then
                // the program's constants strongest first.
                let zero_first = |mut v: Vec<f64>| -> Vec<f64> {
                    if let Some(z) = v.iter().position(|c| *c == 0.0) {
                        let z = v.remove(z);
                        v.insert(0, z);
                    }
                    v.truncate(3);
                    v
                };
                if interval.lo == f64::NEG_INFINITY {
                    let mut lows: Vec<f64> = consts.iter().cloned().filter(|c| *c <= interval.hi).collect();
                    lows.sort_by(|a, b| b.total_cmp(a));
                    sides.push((">=", zero_first(lows)));
                }
                if interval.hi == f64::INFINITY {
                    let mut highs: Vec<f64> = consts.iter().cloned().filter(|c| *c >= interval.lo).collect();
                    highs.sort_by(|a, b| a.total_cmp(b));
                    sides.push(("<=", zero_first(highs)));
                }
                for (op, cands) in sides {
                    for (k, c) in cands.iter().enumerate() {
                        let Some(q) = crate::verify::lp::Q::of_f64(*c) else { continue };
                        let mut script = base.clone();
                        let vars: Vec<String> = p.cols.iter().map(|(i, _)| format!("v{}", i)).collect();
                        let binders: Vec<String> = std::iter::once("(b Int)".to_string()).chain(vars.iter().map(|v| format!("({} Real)", v))).collect();
                        let target = format!("v{}", ci);
                        let app = format!("({} b {})", p.symbol, vars.join(" "));
                        script.assert(format!("(forall ({}) (=> (and {} (not ({} {} {}))) false))", binders.join(" "), app, op, target, numeral(q)));
                        script.tail.push("(get-model)".into());
                        let id = format!("t4-{}-{}-{}-{}", symbol(&p.name), symbol(col), if op == ">=" { "ge" } else { "le" }, k + 1);
                        let statement = format!(
                            "`{}.{} {} {}` is an inductive invariant of the component {{{}}}",
                            p.name,
                            col,
                            if op == ">=" { "≥" } else { "≤" },
                            c,
                            members.join(", ")
                        );
                        script.comment(format!("obligation {}: {}", id, statement));
                        script.comment("sat: the invariant holds (the model is the certificate); unsat: a derivation leaves the bound; unknown: undecided");
                        let out = driver.run(&id, &script);
                        let mut o = Obligation::new(4, id.clone(), "invariant", statement);
                        o.rule = Some(rules.iter().map(|&r| prog.rule_label(r)).collect::<Vec<_>>().join(", "));
                        o.logic = Some("HORN".into());
                        o.budget_ms = driver.budget_ms;
                        o.elapsed_ms = out.elapsed_ms;
                        o.replay_file = out.replay_file.clone();
                        let proved = match &out.answer {
                            SolverAnswer::Sat => {
                                let cert = driver.keep_certificate(&id, &out.output);
                                o = o.with(
                                    Verdict::Proved,
                                    format!("sat: the model is an inductive invariant{}", cert.map(|c| format!("; certificate {}", c)).unwrap_or_default()),
                                );
                                true
                            }
                            SolverAnswer::Unsat => {
                                o = o.with(
                                    Verdict::NotProved,
                                    "unsat: a derivation of the clauses leaves the bound (under the over-approximation of the relational part)",
                                );
                                false
                            }
                            other => {
                                o = o.with(Verdict::Undecided, other.text());
                                false
                            }
                        };
                        t.obligations.push(o);
                        if proved {
                            if let Some(rf) = analysis.relations.iter_mut().find(|r| r.name == p.name) {
                                if let Some(column) = rf.columns.get_mut(*ci) {
                                    let refined = if op == ">=" {
                                        Interval::new(*c, column.value.interval.hi)
                                    } else {
                                        Interval::new(column.value.interval.lo, *c)
                                    };
                                    column.value = column.value.with_interval(column.value.interval.meet(refined));
                                    if column.value.interval.lo.is_finite() && column.value.interval.hi.is_finite() {
                                        rf.widened.retain(|w| w != col);
                                    }
                                }
                            }
                            t.notes.push(format!(
                                "`{}.{}`: interval refined to {} by the proved invariant",
                                p.name,
                                col,
                                analysis.relation(&p.name).and_then(|f| f.columns.get(*ci)).map(|c| c.value.interval.text()).unwrap_or_default()
                            ));
                            break;
                        }
                        if driver.solver.is_none() {
                            // Without a solver one script per side documents the
                            // obligation; the rest would be the same answer.
                            break;
                        }
                    }
                }
            }
        }
    }
    if t.obligations.is_empty() {
        t.notes.push("every recursive column has finite bounds: nothing to refine".into());
    }
    t
}
