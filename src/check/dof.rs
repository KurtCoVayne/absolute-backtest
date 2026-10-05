//! Degrees of freedom (data-bundle doc, section 6 "Degrees of freedom are
//! known exactly", and section 9 item 3): the literals a program hides in
//! its rules, counted alongside its parameters and rule count. The checker
//! warns on each one in a strategy's own rules (W5) and on every ticker
//! literal (W6, a snapshot of the bundle date); the study report reads the
//! counts.

use crate::ir::*;

/// What a strategy leaves free: its parameters, the numeric literals written
/// inside its own rules, its rule count, and the literals of the library
/// rules it reaches (counted, never warned: a library is a shared definition).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct DegreesOfFreedom {
    pub params: usize,
    /// `(rule label, literal)` for every non-structural numeric literal in a
    /// rule of the strategy itself.
    pub literals: Vec<(String, Lit)>,
    pub rules: usize,
    /// The same, for the library rules the strategy's decisions reach.
    pub library_literals: Vec<(String, Lit)>,
}

impl DegreesOfFreedom {
    pub fn summary(&self) -> String {
        let plural = |n: usize, s: &str| if n == 1 { s.to_string() } else { format!("{}s", s) };
        format!(
            "degrees of freedom: {} {}, {} {}, {} {} (+ {} library {})",
            self.params,
            plural(self.params, "param"),
            self.literals.len(),
            plural(self.literals.len(), "literal"),
            self.rules,
            plural(self.rules, "rule"),
            self.library_literals.len(),
            plural(self.library_literals.len(), "literal"),
        )
    }
}

/// A literal that is part of the rule's shape rather than a tunable: zero
/// and one in any unit (a flat target, `W = 1 / N`, a sign test, a zero
/// skip), and a zero duration.
pub fn is_structural(lit: &Lit) -> bool {
    match lit {
        Lit::Int(i) => *i == 0 || *i == 1,
        Lit::Float(x) | Lit::Shares(x) | Lit::Money(x, _) | Lit::Price(x, _) => *x == 0.0 || *x == 1.0,
        Lit::Duration(d) => d.is_zero(),
        Lit::Str(_) | Lit::Equity(_) | Lit::Label(_) => true,
    }
}

/// Every literal of a rule, mutably, with its span: the checker's rewrite
/// of string literals into the type their context resolved.
pub fn literals_in_rule_mut(rule: &mut Rule, f: &mut dyn FnMut(&mut Lit, Span)) {
    fn terms(ts: &mut [Term], f: &mut dyn FnMut(&mut Lit, Span)) {
        for t in ts {
            match t {
                Term::Lit(l, sp) => f(l, *sp),
                Term::Ctor(_, subs, _) => terms(subs, f),
                Term::Var(..) | Term::Wild(_) | Term::Param(..) => {}
            }
        }
    }
    fn expr(e: &mut Expr, f: &mut dyn FnMut(&mut Lit, Span)) {
        match e {
            Expr::Lit(l, sp) => f(l, *sp),
            Expr::Var(..) | Expr::Param(..) => {}
            Expr::Neg(a, _) => expr(a, f),
            Expr::Bin(_, a, b, _) => {
                expr(a, f);
                expr(b, f);
            }
            Expr::Call(_, args, _) => args.iter_mut().for_each(|a| expr(a, f)),
        }
    }
    fn literal(l: &mut Literal, f: &mut dyn FnMut(&mut Lit, Span)) {
        match l {
            Literal::Atom(a) | Literal::Neg(a) => terms(&mut a.terms, f),
            Literal::Builtin(Builtin::Lag { n, .. }, _) => expr(n, f),
            Literal::Builtin(..) => {}
            Literal::Window { dur, min, .. } => {
                expr(dur, f);
                expr(min, f);
            }
            Literal::Cmp { lhs, rhs, .. } => {
                expr(lhs, f);
                expr(rhs, f);
            }
            Literal::Assign { expr: e, .. } => expr(e, f),
            Literal::Agg { args, conj, .. } => {
                args.iter_mut().for_each(|a| expr(a, f));
                conj.iter_mut().for_each(|c| literal(c, f));
            }
            Literal::Top { n, atom, .. } => {
                expr(n, f);
                terms(&mut atom.terms, f);
            }
            Literal::Resample { inner, min, aggs, .. } => {
                terms(&mut inner.terms, f);
                expr(min, f);
                aggs.iter_mut().for_each(|(_, _, e)| expr(e, f));
            }
        }
    }
    terms(&mut rule.head.terms, f);
    rule.body.iter_mut().for_each(|l| literal(l, f));
}

/// Every literal written in a rule, head and body, in source order.
pub fn literals_in_rule(rule: &Rule) -> Vec<(Lit, Span)> {
    let mut out = Vec::new();
    terms(&rule.head.terms, &mut out);
    for l in &rule.body {
        literal(l, &mut out);
    }
    out
}

fn literal(l: &Literal, out: &mut Vec<(Lit, Span)>) {
    match l {
        Literal::Atom(a) | Literal::Neg(a) => terms(&a.terms, out),
        Literal::Builtin(b, _) => {
            if let Builtin::Lag { n, .. } = b {
                expr(n, out);
            }
        }
        Literal::Window { dur, min, .. } => {
            expr(dur, out);
            expr(min, out);
        }
        Literal::Cmp { lhs, rhs, .. } => {
            expr(lhs, out);
            expr(rhs, out);
        }
        Literal::Assign { expr: e, .. } => expr(e, out),
        Literal::Agg { args, conj, .. } => {
            for a in args {
                expr(a, out);
            }
            for c in conj {
                literal(c, out);
            }
        }
        Literal::Top { n, atom, .. } => {
            expr(n, out);
            terms(&atom.terms, out);
        }
        Literal::Resample { inner, min, aggs, .. } => {
            terms(&inner.terms, out);
            expr(min, out);
            for (_, _, e) in aggs {
                expr(e, out);
            }
        }
    }
}

fn terms(ts: &[Term], out: &mut Vec<(Lit, Span)>) {
    for t in ts {
        match t {
            Term::Lit(l, sp) => out.push((l.clone(), *sp)),
            Term::Ctor(_, subs, _) => terms(subs, out),
            Term::Var(..) | Term::Wild(_) | Term::Param(..) => {}
        }
    }
}

fn expr(e: &Expr, out: &mut Vec<(Lit, Span)>) {
    match e {
        Expr::Lit(l, sp) => out.push((l.clone(), *sp)),
        Expr::Var(..) | Expr::Param(..) => {}
        Expr::Neg(a, _) => expr(a, out),
        Expr::Bin(_, a, b, _) => {
            expr(a, out);
            expr(b, out);
        }
        Expr::Call(_, args, _) => {
            for a in args {
                expr(a, out);
            }
        }
    }
}
