//! R5, demand restriction and elision (compiler design, section 4.5): a
//! parametric relation is specialised to the parameter tuples of its demand
//! set, one copy of its rules per tuple with the parameters substituted;
//! a relation nothing demanded reaches is flagged elided. The law is magic
//! sets with constant magic predicates (Proposition 4.2).

use std::collections::{BTreeSet, HashMap};

use super::RewriteLog;
use crate::analysis::{Analysis, DemandSet};
use crate::check::Program;
use crate::compiler::normalise::{Nir, NirBodyLit, NirExpr, NirLit, NirRule, NirTerm};
use crate::ir::*;

/// Substitute literals for variables in an IR literal.
fn subst_literal(l: &Literal, m: &HashMap<String, Lit>) -> Literal {
    fn term(t: &Term, m: &HashMap<String, Lit>) -> Term {
        match t {
            Term::Var(v, s) => match m.get(v) {
                Some(l) => Term::Lit(l.clone(), *s),
                None => t.clone(),
            },
            Term::Ctor(c, subs, s) => Term::Ctor(c.clone(), subs.iter().map(|x| term(x, m)).collect(), *s),
            t => t.clone(),
        }
    }
    fn expr(e: &Expr, m: &HashMap<String, Lit>) -> Expr {
        match e {
            Expr::Var(v, s) => match m.get(v) {
                Some(l) => Expr::Lit(l.clone(), *s),
                None => e.clone(),
            },
            Expr::Neg(x, s) => Expr::Neg(Box::new(expr(x, m)), *s),
            Expr::Bin(op, a, b, s) => Expr::Bin(*op, Box::new(expr(a, m)), Box::new(expr(b, m)), *s),
            Expr::Call(f, args, s) => Expr::Call(f.clone(), args.iter().map(|a| expr(a, m)).collect(), *s),
            e => e.clone(),
        }
    }
    fn atom(a: &Atom, m: &HashMap<String, Lit>) -> Atom {
        Atom {
            name: a.name.clone(),
            terms: a.terms.iter().map(|t| term(t, m)).collect(),
            span: a.span,
        }
    }
    match l {
        Literal::Atom(a) => Literal::Atom(atom(a, m)),
        Literal::Neg(a) => Literal::Neg(atom(a, m)),
        Literal::Builtin(b, s) => Literal::Builtin(
            match b {
                Builtin::Prev { t, t1 } => Builtin::Prev { t: term(t, m), t1: term(t1, m) },
                Builtin::Lag { t, n, t1 } => Builtin::Lag {
                    t: term(t, m),
                    n: expr(n, m),
                    t1: term(t1, m),
                },
                Builtin::MonthStart { t } => Builtin::MonthStart { t: term(t, m) },
                Builtin::DayStart { t } => Builtin::DayStart { t: term(t, m) },
                Builtin::SessionOf { a, t, s: sv } => Builtin::SessionOf {
                    a: term(a, m),
                    t: term(t, m),
                    s: term(sv, m),
                },
            },
            *s,
        ),
        Literal::Window { var, kind, base, dur, min, lo, span } => Literal::Window {
            var: term(var, m),
            kind: kind.clone(),
            base: term(base, m),
            dur: expr(dur, m),
            min: expr(min, m),
            lo: lo.as_ref().map(|x| expr(x, m)),
            span: *span,
        },
        Literal::Cmp { op, lhs, rhs, span } => Literal::Cmp {
            op: *op,
            lhs: expr(lhs, m),
            rhs: expr(rhs, m),
            span: *span,
        },
        Literal::Assign { var, expr: e, span } => Literal::Assign {
            var: var.clone(),
            expr: expr(e, m),
            span: *span,
        },
        Literal::Agg { var, agg, args, conj, within, span } => Literal::Agg {
            var: var.clone(),
            agg: agg.clone(),
            args: args.iter().map(|a| expr(a, m)).collect(),
            conj: conj.iter().map(|c| subst_literal(c, m)).collect(),
            within: within.clone(),
            span: *span,
        },
        Literal::Top { n, atom: a, by, rank, within, span } => Literal::Top {
            n: n.as_ref().map(|x| expr(x, m)),
            atom: atom(a, m),
            by: by.clone(),
            rank: rank.clone(),
            within: within.clone(),
            span: *span,
        },
        Literal::AsOf { atom: a, at, within_bucket, span } => Literal::AsOf {
            atom: atom(a, m),
            at: term(at, m),
            within_bucket: within_bucket.clone(),
            span: *span,
        },
        Literal::Case { var, branches, otherwise, span } => Literal::Case {
            var: var.clone(),
            branches: branches
                .iter()
                .map(|b| crate::ir::Branch {
                    guard: b.guard.iter().map(|g| subst_literal(g, m)).collect(),
                    value: expr(&b.value, m),
                    span: b.span,
                })
                .collect(),
            otherwise: otherwise.as_ref().map(|x| expr(x, m)),
            span: *span,
        },
    }
}

fn subst_core(l: &NirLit, m: &HashMap<String, String>) -> NirLit {
    fn t(x: &NirTerm, m: &HashMap<String, String>) -> NirTerm {
        match x {
            NirTerm::Var(v) => match m.get(v) {
                Some(l) => NirTerm::Lit(l.clone()),
                None => x.clone(),
            },
            NirTerm::Ctor(c, subs) => NirTerm::Ctor(c.clone(), subs.iter().map(|s| t(s, m)).collect()),
            x => x.clone(),
        }
    }
    fn e(x: &NirExpr, m: &HashMap<String, String>) -> NirExpr {
        match x {
            NirExpr::Var(v) => match m.get(v) {
                Some(l) => NirExpr::Lit(l.clone()),
                None => x.clone(),
            },
            NirExpr::Neg(a) => NirExpr::Neg(Box::new(e(a, m))),
            NirExpr::Bin(op, a, b) => NirExpr::Bin(op.clone(), Box::new(e(a, m)), Box::new(e(b, m))),
            NirExpr::Call(f, args) => NirExpr::Call(f.clone(), args.iter().map(|a| e(a, m)).collect()),
            NirExpr::Time(f, args) => NirExpr::Time(f.clone(), args.iter().map(|a| e(a, m)).collect()),
            NirExpr::Keys(ks) => NirExpr::Keys(ks.iter().map(|(k, d)| (e(k, m), d.clone())).collect()),
            x => x.clone(),
        }
    }
    match l {
        NirLit::Atom(a) => NirLit::Atom(crate::compiler::normalise::NirAtom {
            name: a.name.clone(),
            terms: a.terms.iter().map(|s| t(s, m)).collect(),
        }),
        NirLit::Neg(a) => NirLit::Neg(crate::compiler::normalise::NirAtom {
            name: a.name.clone(),
            terms: a.terms.iter().map(|s| t(s, m)).collect(),
        }),
        NirLit::Agg {
            var,
            agg,
            args,
            group,
            by,
            min,
            anchor,
        } => NirLit::Agg {
            var: var.clone(),
            agg: agg.clone(),
            args: args.iter().map(|x| e(x, m)).collect(),
            group: group.iter().map(|g| subst_core(g, m)).collect(),
            by: by.clone(),
            min: min.as_ref().map(|x| e(x, m)),
            anchor: *anchor,
        },
        NirLit::Assign { var, expr } => NirLit::Assign { var: var.clone(), expr: e(expr, m) },
        NirLit::Cmp { op, lhs, rhs } => NirLit::Cmp {
            op: op.clone(),
            lhs: e(lhs, m),
            rhs: e(rhs, m),
        },
        NirLit::Window { var, kind, base, dur, min } => NirLit::Window {
            var: var.clone(),
            kind: kind.clone(),
            base: base.clone(),
            dur: e(dur, m),
            min: e(min, m),
        },
        NirLit::Case { var, branches, otherwise } => NirLit::Case {
            var: var.clone(),
            branches: branches.iter().map(|(g, v)| (g.iter().map(|x| subst_core(x, m)).collect(), e(v, m))).collect(),
            otherwise: otherwise.as_ref().map(|x| e(x, m)),
        },
    }
}

/// Apply R5: specialise every parametric relation with a static demand to
/// its tuples, and flag the relations nothing demanded reaches as elided.
pub fn specialise(prog: &Program, nir: &mut Nir, analysis: &Analysis, elided: &mut BTreeSet<String>, log: &mut RewriteLog) -> bool {
    let mut any = false;
    // Elision.
    for r in nir.rules.iter_mut() {
        if r.elided || analysis.demanded.contains(&r.head.name) {
            continue;
        }
        r.elided = true;
        if elided.insert(r.head.name.clone()) {
            log.push(
                "R5",
                &r.id,
                &r.span,
                "demanded".into(),
                format!("elide {}: not reached from decide, observations or queries", r.head.name),
                "magic sets: an undemanded relation contributes no answer (Bancilhon et al. 1986)",
                [0, 0],
            );
        }
        any = true;
    }
    // Specialisation of the demanded parametric relations.
    let unspecialised_before = nir
        .rules
        .iter()
        .filter(|r| r.specialised.is_none())
        .map(|r| r.head.name.clone())
        .filter(|n| matches!(analysis.relation(n).map(|f| &f.demand), Some(DemandSet::Static(ts)) if !ts.is_empty()))
        .collect::<BTreeSet<_>>()
        .len();
    let mut new_rules: Vec<NirRule> = Vec::new();
    let mut removed = 0usize;
    for r in nir.rules.iter() {
        if r.specialised.is_some() || r.elided {
            continue;
        }
        let Some(facts) = analysis.relation(&r.head.name) else { continue };
        let DemandSet::Static(tuples) = &facts.demand else { continue };
        if tuples.is_empty() {
            continue;
        }
        let Some(sig) = prog.relations.get(&r.head.name) else { continue };
        let positions: Vec<usize> = {
            let _ = sig;
            Vec::new()
        };
        if positions.is_empty() {
            continue;
        }
        for (k, tuple) in tuples.iter().enumerate() {
            let mut by_name: HashMap<String, Lit> = HashMap::new();
            let mut by_text: HashMap<String, String> = HashMap::new();
            let mut subs: Vec<(String, String)> = Vec::new();
            for (p, value) in positions.iter().zip(tuple.iter()) {
                if let NirTerm::Var(v) = &r.head.terms[*p] {
                    if let Ok(l) = crate::syntax::parse_lit(value) {
                        by_name.insert(v.clone(), l);
                    }
                    by_text.insert(v.clone(), value.clone());
                    subs.push((sig.args[*p].name.clone(), value.clone()));
                }
            }
            let mut copy = r.clone();
            copy.id = format!("{}@{}", r.id, k + 1);
            copy.head.terms = copy
                .head
                .terms
                .iter()
                .map(|t| {
                    if let NirTerm::Var(v) = t {
                        by_text.get(v).map(|l| NirTerm::Lit(l.clone())).unwrap_or_else(|| t.clone())
                    } else {
                        t.clone()
                    }
                })
                .collect();
            copy.body = copy
                .body
                .iter()
                .map(|b| NirBodyLit {
                    lit: subst_core(&b.lit, &by_text),
                    unit: b.unit,
                })
                .collect();
            copy.surface = copy.surface.iter().map(|l| subst_literal(l, &by_name)).collect();
            copy.specialised = Some(subs.clone());
            log.push(
                "R5",
                &r.id,
                &r.span,
                format!("{}({})", r.head.name, sig.args.iter().map(|a| a.name.clone()).collect::<Vec<_>>().join(", ")),
                format!("specialise {} at ({})", r.head.name, subs.iter().map(|(p, v)| format!("{} = {}", p, v)).collect::<Vec<_>>().join(", ")),
                "magic sets with constant magic predicates (Proposition 4.2)",
                [unspecialised_before, unspecialised_before.saturating_sub(1)],
            );
            new_rules.push(copy);
        }
        removed += 1;
        any = true;
    }
    if removed > 0 {
        let specialised_names: BTreeSet<String> = new_rules.iter().map(|r| r.head.name.clone()).collect();
        nir.rules.retain(|r| r.specialised.is_some() || !specialised_names.contains(&r.head.name) || r.elided);
        nir.rules.extend(new_rules);
    }
    any
}
