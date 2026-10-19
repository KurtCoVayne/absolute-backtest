//! Printable stage results (compiler design, section 2.3): every stage
//! result renders as text for a reader and as JSON for tools. This module
//! also holds the renderer of the core IR back to the surface syntax of the
//! second formulation (the Normalise stage's canonical core text), and the
//! rendering of a checked program as written (`ir --stage checked`), which
//! is the AST's (`syntax::print`).

use crate::check::Program;
use crate::ir::*;
use crate::syntax::ast;

/// A stage result: text for a reader, JSON for tools.
pub trait Stage {
    fn text(&self) -> String;
    fn json(&self) -> serde_json::Value;
}

/// A map from variable names to the names they are rendered with.
pub type NameMap<'a> = &'a dyn Fn(&str) -> String;

pub use crate::syntax::print::{render_expr, render_lit, render_term};

pub fn render_atom(a: &Atom, names: NameMap) -> String {
    format!("{}({})", a.name, a.terms.iter().map(|t| render_term(t, names)).collect::<Vec<_>>().join(", "))
}

fn render_order(by: &[(String, Dir, Span)], names: NameMap) -> String {
    by.iter()
        .map(|(k, d, _)| format!("{} {}", names(k), if *d == Dir::Desc { "desc" } else { "asc" }))
        .collect::<Vec<_>>()
        .join(", ")
}

fn render_within(w: &Option<Vec<String>>, names: NameMap) -> String {
    match w {
        Some(vs) => format!(" within ({})", vs.iter().map(|v| names(v)).collect::<Vec<_>>().join(", ")),
        None => String::new(),
    }
}

/// A literal in the surface syntax, every variable through `names`.
pub fn render_literal(l: &Literal, names: NameMap) -> String {
    match l {
        Literal::Atom(a) => render_atom(a, names),
        Literal::Neg(a) => format!("not {}", render_atom(a, names)),
        Literal::Builtin(b, _) => match b {
            Builtin::Prev { t, t1 } => format!("prev({}, {})", render_term(t, names), render_term(t1, names)),
            Builtin::Lag { t, n, t1 } => format!("lag({}, {}, {})", render_term(t, names), render_expr(n, names), render_term(t1, names)),
            Builtin::MonthStart { t } => format!("month_start({})", render_term(t, names)),
            Builtin::DayStart { t } => format!("day_start({})", render_term(t, names)),
            Builtin::SessionOf { a, t, s } => format!("session_of({}, {}, {})", render_term(a, names), render_term(t, names), render_term(s, names)),
        },
        Literal::Window { var, kind, base, dur, min, lo, .. } => match kind {
            WindowKind::Window => format!("{} in window[{}] min {}", render_term(var, names), render_expr(dur, names), render_expr(min, names)),
            WindowKind::Prior => format!("{} in prior_window[{}] min {}", render_term(var, names), render_expr(dur, names), render_expr(min, names)),
            WindowKind::Rows => format!("{} in rows[{}] min {}", render_term(var, names), render_expr(dur, names), render_expr(min, names)),
            WindowKind::PriorRows => format!("{} in prior_rows[{}] min {}", render_term(var, names), render_expr(dur, names), render_expr(min, names)),
            WindowKind::Bucket(c) => format!("{} in bucket({}, {})", render_term(var, names), c, render_term(base, names)),
            WindowKind::Session => format!(
                "{} in session({}, [{}, {}))",
                render_term(var, names),
                render_term(base, names),
                lo.as_ref().map(|e| render_expr(e, names)).unwrap_or_else(|| "0min".into()),
                render_expr(dur, names)
            ),
        },
        Literal::Cmp { op, lhs, rhs, .. } => format!("{} {} {}", render_expr(lhs, names), op, render_expr(rhs, names)),
        Literal::Assign { var, expr, .. } => format!("{} = {}", names(var), render_expr(expr, names)),
        Literal::Agg { var, agg, args, conj, within, .. } => format!(
            "{} = {}({}) over ({}){}",
            names(var),
            agg,
            args.iter().map(|a| render_expr(a, names)).collect::<Vec<_>>().join(", "),
            conj.iter().map(|c| render_literal(c, names)).collect::<Vec<_>>().join(", "),
            render_within(within, names)
        ),
        Literal::Top { n, atom, by, rank, within, .. } => {
            let order = by.as_ref().map(|b| format!(" by ({})", render_order(b, names))).unwrap_or_default();
            match (n, rank) {
                (Some(n), _) => format!("top[{}]({}{}){}", render_expr(n, names), render_atom(atom, names), order, render_within(within, names)),
                (None, Some((k, ties, _))) => format!(
                    "{} = {}({}{}){}",
                    names(k),
                    if *ties == Ties::Average { "rank_average" } else { "rank" },
                    render_atom(atom, names),
                    order,
                    render_within(within, names)
                ),
                (None, None) => format!("top[0]({}{}){}", render_atom(atom, names), order, render_within(within, names)),
            }
        }
        Literal::AsOf { atom, at, .. } => format!("{} asof {}", render_atom(atom, names), render_term(at, names)),
        Literal::Case { var, branches, otherwise, .. } => {
            let mut parts: Vec<String> = branches
                .iter()
                .map(|b| {
                    format!(
                        "{} -> {}",
                        b.guard.iter().map(|g| render_literal(g, names)).collect::<Vec<_>>().join(", "),
                        render_expr(&b.value, names)
                    )
                })
                .collect();
            if let Some(e) = otherwise {
                parts.push(format!("else -> {}", render_expr(e, names)));
            }
            format!("{} = case {{ {} }}", names(var), parts.join(" ; "))
        }
    }
}

/// A rule in the surface syntax, its body in `order` (the written order
/// when absent).
pub fn render_rule(rule: &Rule, names: NameMap, order: Option<&[usize]>) -> String {
    let body: Vec<String> = match order {
        Some(o) => o.iter().map(|&i| render_literal(&rule.body[i], names)).collect(),
        None => rule.body.iter().map(|l| render_literal(l, names)).collect(),
    };
    if body.is_empty() {
        return format!("{}.", render_atom(&rule.head, names));
    }
    format!("{} :- {}.", render_atom(&rule.head, names), body.join(", "))
}

/// A signature in the surface syntax: `name(A: Entity, @T: cal, X: Ty)`.
pub fn render_signature(sig: &Signature, with_cal: bool) -> String {
    let args: Vec<String> = sig
        .args
        .iter()
        .map(|a| match a.mode {
            Mode::Key => {
                if with_cal {
                    format!("@{}: {}", a.name, sig.cal)
                } else {
                    format!("@{}", a.name)
                }
            }
            Mode::Out => format!("{}: {}", a.name, a.ty),
        })
        .collect();
    let mut s = format!("{}({})", sig.name, args.join(", "));
    if sig.complete && matches!(sig.kind, Kind::Primitive { .. }) {
        s.push_str(" complete");
    }
    s
}

pub fn render_param(p: &Param) -> String {
    let mut s = format!("{} : {} = {}", p.name, p.ty, render_lit(&p.value));
    if let Some((lo, hi)) = &p.range {
        s.push_str(&format!(" in {}..{}", render_lit(lo), render_lit(hi)));
    }
    s
}

/// The checked program as written (`ir --stage checked`): the strategy's
/// blocks as the AST holds them, the execution and report blocks expanded.
pub fn render_checked(prog: &Program) -> String {
    let exec = prog.exec.to_string();
    let report = prog.report.to_string();
    crate::syntax::print::render_strategy(
        &prog.source,
        &crate::syntax::print::Options {
            execution: Some(&exec),
            report: Some(&report),
            rule_style: None,
        },
    )
}

/// The JSON of a checked program: relations, parameters, rules as text,
/// strata and the complete set.
pub fn checked_json(prog: &Program) -> serde_json::Value {
    let identity = |v: &str| v.to_string();
    serde_json::json!({
        "stage": "checked",
        "strategy": prog.strategy,
        "class": prog.class,
        "base": prog.base.to_string(),
        "decision_calendar": prog.cal_name(&prog.decision_cal),
        "clock_offset": prog.clock.offset.map(|d| d.to_string()),
        "demand": prog.demand,
        "relations": prog.relations.values().map(|s| serde_json::json!({
            "name": s.name,
            "kind": kind_name(&s.kind),
            "unit": match &s.kind { Kind::Derived { unit } => Some(unit.clone()), Kind::Primitive { class } => Some(class.clone()), _ => None },
            "calendar": prog.cal_name(&s.cal),
            "signature": render_signature(s, true),
            "complete": prog.complete.contains(&s.name),
        })).collect::<Vec<_>>(),
        "params": prog.params.get(&prog.strategy).map(|ps| ps.values().map(|p| serde_json::json!({"unit": prog.strategy, "text": render_param(p)})).collect::<Vec<_>>()).unwrap_or_default(),
        "rules": prog.rules.iter().enumerate().map(|(i, r)| serde_json::json!({
            "label": prog.rule_label(i),
            "unit": r.unit,
            "span": r.span.to_string(),
            "text": render_rule(r, &identity, None),
        })).collect::<Vec<_>>(),
        "strata": prog.strata,
        "execution": prog.exec,
        "report": prog.report,
        "degrees_of_freedom": prog.degrees_of_freedom.summary(),
    })
}

pub fn kind_name(k: &Kind) -> &'static str {
    match k {
        Kind::Primitive { .. } => "primitive",
        Kind::Executor => "executor",
        Kind::KernelState => "kernel",
        Kind::Derived { .. } => "derived",
        Kind::Output => "output",
    }
}

/// The parsed units as a listing (`ir --stage parsed`): each unit's kind,
/// name and blocks, in the order given.
pub fn render_parsed(units: &[ast::Unit]) -> String {
    let mut out = String::new();
    for u in units {
        match u {
            ast::Unit::Strategy(s) => {
                let blocks: Vec<&str> = s.blocks.iter().map(|(b, _)| b.as_str()).collect();
                out.push_str(&format!("strategy {} at {}: blocks {}\n", s.name, s.span, blocks.join(", ")));
                if let Some(r) = &s.rules {
                    for d in &r.rels {
                        out.push_str(&format!("  rel {}\n", d.name));
                    }
                }
            }
            ast::Unit::Library(l) => {
                let rels: Vec<&str> = l.rules.rels.iter().map(|d| d.name.as_str()).collect();
                out.push_str(&format!("library {} at {}: {}\n", l.name, l.span, rels.join(", ")));
            }
            ast::Unit::Class(c) => {
                let rels: Vec<&str> = c.rels.iter().map(|d| d.name.as_str()).collect();
                let base = c.base.map(|r| format!("base {}, ", r)).unwrap_or_default();
                out.push_str(&format!("class {} at {}: {}{}\n", c.name, c.span, base, rels.join(", ")));
            }
            ast::Unit::Execution(e) => {
                out.push_str(&format!("execution {} at {}: {} fields\n", e.name, e.span, e.fields.len()));
            }
        }
    }
    out
}
