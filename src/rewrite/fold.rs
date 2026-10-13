//! R6, constant folding (compiler design, section 4.6): a closed numeric
//! subexpression (parameters and literals only, no time function, at least
//! one operation) is evaluated at compile time with the kernel's own
//! arithmetic on doubles in the same operation order, so the folded constant
//! is bit-identical to what the kernel would compute; the program copy the
//! kernel evaluates carries the folded expressions.

use super::RewriteLog;
use crate::check::types::{bin_type, call_type, neg_type};
use crate::check::Program;
use crate::compiler::normalise::{core_of, initially_bound, literal_reads_binds, Nir, NirBodyLit};
use crate::compiler::print::render_expr;
use crate::ir::*;
use crate::kernel::Value;

/// The value of a closed expression by the kernel's arithmetic (`eval_expr`):
/// counts stay counts under `+`, `-` and `×`; everything else is a double;
/// a partial operation (division by zero, a log of a non-positive) is not
/// folded, since the kernel halts on it at run time.
fn closed_value(e: &Expr, prog: &Program, unit: &str) -> Option<Value> {
    match e {
        Expr::Var(..) => None,
        Expr::Lit(l, _) => match l {
            Lit::Int(i) => Some(Value::Count(*i)),
            Lit::Float(x) | Lit::Quantity(x, _) | Lit::Money(x, _) | Lit::Price(x, _) => Some(Value::Num(*x)),
            _ => None,
        },
        Expr::Param(p, _) => match prog.param(unit, p).map(|p| &p.value) {
            Some(Lit::Int(i)) => Some(Value::Count(*i)),
            Some(Lit::Float(x) | Lit::Quantity(x, _) | Lit::Money(x, _) | Lit::Price(x, _)) => Some(Value::Num(*x)),
            _ => None,
        },
        Expr::Neg(x, _) => match closed_value(x, prog, unit)? {
            Value::Num(v) => Some(Value::Num(-v)),
            Value::Count(c) => Some(Value::Count(-c)),
            _ => None,
        },
        Expr::Bin(op, a, b, _) => {
            let x = closed_value(a, prog, unit)?;
            let y = closed_value(b, prog, unit)?;
            match (op, &x, &y) {
                (BinOp::Add, Value::Count(p), Value::Count(q)) => Some(Value::Count(p + q)),
                (BinOp::Sub, Value::Count(p), Value::Count(q)) => Some(Value::Count(p - q)),
                (BinOp::Mul, Value::Count(p), Value::Count(q)) => Some(Value::Count(p * q)),
                _ => {
                    let (p, q) = (x.as_f64()?, y.as_f64()?);
                    Some(Value::Num(match op {
                        BinOp::Add => p + q,
                        BinOp::Sub => p - q,
                        BinOp::Mul => p * q,
                        BinOp::Div => {
                            if q == 0.0 {
                                return None;
                            }
                            p / q
                        }
                    }))
                }
            }
        }
        Expr::Call(f, args, _) => {
            let vals: Vec<Value> = args.iter().map(|a| closed_value(a, prog, unit)).collect::<Option<_>>()?;
            let num = |v: &Value| v.as_f64();
            match f.as_str() {
                "log" => {
                    let x = num(&vals[0])?;
                    (x > 0.0).then(|| Value::Num(x.ln()))
                }
                "exp" => Some(Value::Num(num(&vals[0])?.exp())),
                "sqrt" => {
                    let x = num(&vals[0])?;
                    (x >= 0.0).then(|| Value::Num(x.sqrt()))
                }
                "abs" => match &vals[0] {
                    Value::Count(c) => Some(Value::Count(c.abs())),
                    v => Some(Value::Num(num(v)?.abs())),
                },
                "least" | "greatest" => {
                    let mut best = vals[0].clone();
                    for v in &vals[1..] {
                        let (a, b) = (num(v)?, num(&best)?);
                        if (f == "least" && a < b) || (f == "greatest" && a > b) {
                            best = v.clone();
                        }
                    }
                    if let (Value::Count(c), true) = (&best, vals.iter().any(|v| matches!(v, Value::Num(_)))) {
                        best = Value::Num(*c as f64);
                    }
                    Some(best)
                }
                _ => None,
            }
        }
    }
}

/// The type of a closed expression by the checker's algebra, for the
/// literal the folded value is written as.
fn closed_type(e: &Expr, prog: &Program, unit: &str) -> Option<Ty> {
    match e {
        Expr::Var(..) => None,
        Expr::Lit(l, _) => Some(l.ty()),
        Expr::Param(p, _) => prog.param(unit, p).map(|p| p.ty.clone()),
        Expr::Neg(x, _) => neg_type(&closed_type(x, prog, unit)?).ok(),
        Expr::Bin(op, a, b, _) => bin_type(*op, &closed_type(a, prog, unit)?, &closed_type(b, prog, unit)?).ok(),
        Expr::Call(f, args, _) => {
            let tys: Vec<Ty> = args.iter().map(|a| closed_type(a, prog, unit)).collect::<Option<_>>()?;
            call_type(f, &tys).ok()
        }
    }
}

/// A folded value as a literal of the expression's type, when the type has
/// a literal form.
fn literal_of(v: &Value, ty: &Ty) -> Option<Lit> {
    match (v, ty) {
        (Value::Count(c), Ty::Count | Ty::IntLit) => Some(Lit::Int(*c)),
        (Value::Count(c), Ty::Quantity(d)) if d.is_scalar() => Some(Lit::Float(*c as f64)),
        (Value::Num(x), Ty::Quantity(d)) => {
            if d.is_scalar() {
                Some(Lit::Float(*x))
            } else if *d == Dim::shares() {
                Some(Lit::Quantity(*x, "shares".into()))
            } else if d.c2 == 2 && d.s2 == -2 && d.t2 == 0 {
                Some(Lit::Price(*x, d.currency.clone()?))
            } else if d.c2 == 2 && d.s2 == 0 && d.t2 == 0 {
                Some(Lit::Money(*x, d.currency.clone()?))
            } else {
                None
            }
        }
        (Value::Num(x), Ty::IntLit) => Some(Lit::Float(*x)),
        _ => None,
    }
}

fn has_operation(e: &Expr) -> bool {
    matches!(e, Expr::Neg(..) | Expr::Bin(..) | Expr::Call(..))
}

/// Fold the closed subexpressions of an expression, recording each.
fn fold_expr(e: &Expr, prog: &Program, unit: &str, folds: &mut Vec<(String, String)>) -> Expr {
    if has_operation(e) {
        if let (Some(v), Some(ty)) = (closed_value(e, prog, unit), closed_type(e, prog, unit)) {
            if let Some(l) = literal_of(&v, &ty) {
                folds.push((render_expr(e, &|v: &str| v.to_string()), crate::compiler::print::render_lit(&l)));
                return Expr::Lit(l, e.span());
            }
        }
    }
    match e {
        Expr::Neg(x, s) => Expr::Neg(Box::new(fold_expr(x, prog, unit, folds)), *s),
        Expr::Bin(op, a, b, s) => Expr::Bin(*op, Box::new(fold_expr(a, prog, unit, folds)), Box::new(fold_expr(b, prog, unit, folds)), *s),
        Expr::Call(f, args, s) => Expr::Call(f.clone(), args.iter().map(|a| fold_expr(a, prog, unit, folds)).collect(), *s),
        e => e.clone(),
    }
}

/// Fold the expressions of a literal (not the lengths of windows and lags,
/// which are durations and counts the kernel reads as they are).
pub fn fold_literal(l: &Literal, prog: &Program, unit: &str, folds: &mut Vec<(String, String)>) -> Literal {
    match l {
        Literal::Cmp { op, lhs, rhs, span } => Literal::Cmp {
            op: *op,
            lhs: fold_expr(lhs, prog, unit, folds),
            rhs: fold_expr(rhs, prog, unit, folds),
            span: *span,
        },
        Literal::Assign { var, expr, span } => Literal::Assign {
            var: var.clone(),
            expr: fold_expr(expr, prog, unit, folds),
            span: *span,
        },
        Literal::Agg { var, agg, args, conj, within, span } => Literal::Agg {
            var: var.clone(),
            agg: agg.clone(),
            args: args.iter().map(|a| fold_expr(a, prog, unit, folds)).collect(),
            conj: conj.iter().map(|c| fold_literal(c, prog, unit, folds)).collect(),
            within: within.clone(),
            span: *span,
        },
        Literal::Case { var, branches, otherwise, span } => Literal::Case {
            var: var.clone(),
            branches: branches
                .iter()
                .map(|b| crate::ir::Branch {
                    guard: b.guard.iter().map(|g| fold_literal(g, prog, unit, folds)).collect(),
                    value: fold_expr(&b.value, prog, unit, folds),
                    span: b.span,
                })
                .collect(),
            otherwise: otherwise.as_ref().map(|x| fold_expr(x, prog, unit, folds)),
            span: *span,
        },
        l => l.clone(),
    }
}

/// Apply R6 to every rule: the program copy the kernel evaluates and the
/// normal form's expressions.
pub fn fold(prog: &Program, nir: &mut Nir, folded: &mut Option<Program>, log: &mut RewriteLog) -> bool {
    if folded.is_some() {
        return false;
    }
    let mut copy = prog.clone();
    let mut any = false;
    // The measure: closed non-literal expressions, counted before folding.
    let mut closed_before: usize = 0;
    for r in nir.rules.iter() {
        let rule = &prog.rules[r.origin];
        let mut probe = Vec::new();
        for l in &r.surface {
            fold_literal(l, prog, &rule.unit, &mut probe);
        }
        closed_before += probe.len();
    }
    let mut remaining = closed_before;
    for r in nir.rules.iter_mut() {
        let rule = &prog.rules[r.origin];
        let mut folds: Vec<(String, String)> = Vec::new();
        // The surface literals (canonical names; folding touches no variable).
        let new_surface: Vec<Literal> = r.surface.iter().map(|l| fold_literal(l, prog, &rule.unit, &mut folds)).collect();
        if folds.is_empty() {
            continue;
        }
        // Re-reduce the changed literals' core forms.
        let initial = initially_bound(prog, rule)
            .into_iter()
            .map(|v| r.names.iter().find(|(_, w)| *w == v).map(|(c, _)| c.clone()).unwrap_or(v))
            .collect::<std::collections::HashSet<_>>();
        let mut bound = initial;
        let mut counter = 1000;
        let mut body: Vec<NirBodyLit> = Vec::new();
        for (i, lit) in new_surface.iter().enumerate() {
            let (_, core) = core_of(prog, lit, &bound, &mut counter);
            let (_, binds) = literal_reads_binds(prog, rule, lit, &bound);
            bound.extend(binds);
            for c in core {
                body.push(NirBodyLit { lit: c, unit: i });
            }
        }
        r.surface = new_surface;
        r.body = body;
        // The program copy, in written names and written positions.
        let back: std::collections::HashMap<String, String> = r.names.iter().map(|(c, w)| (c.clone(), w.clone())).collect();
        for (pos, &written) in r.surface_origin.iter().enumerate() {
            copy.rules[r.origin].body[written] = crate::compiler::normalise::rename_literal(&r.surface[pos], &back);
        }
        for (before, after) in &folds {
            log.push(
                "R6",
                &r.id,
                &r.span,
                before.clone(),
                after.clone(),
                "evaluation of closed terms with the kernel's arithmetic (4.6)",
                [remaining, remaining.saturating_sub(1)],
            );
            remaining = remaining.saturating_sub(1);
        }
        any = true;
    }
    if any {
        *folded = Some(copy);
    }
    any
}
