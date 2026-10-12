//! Printable stage results (compiler design, section 2.3): every stage
//! result renders as text for a reader and as JSON for tools. This module
//! also holds the renderer of the typed IR back to source text, which the
//! Normalise stage uses for its re-parseable rendering, and the rendering
//! of a checked program as written (`ir --stage checked`).

use std::collections::BTreeMap;

use crate::check::Program;
use crate::ir::*;

/// A stage result: text for a reader, JSON for tools.
pub trait Stage {
    fn text(&self) -> String;
    fn json(&self) -> serde_json::Value;
}

/// A map from variable names to the names they are rendered with.
pub type NameMap<'a> = &'a dyn Fn(&str) -> String;

/// A literal value as the lexer reads it back: a float always carries a
/// point or an exponent, so that `2.0` does not come back as the integer `2`.
pub fn render_lit(l: &Lit) -> String {
    fn float(x: f64) -> String {
        if x.is_finite() && x.fract() == 0.0 && x.abs() < 1e15 {
            format!("{:.1}", x)
        } else {
            format!("{:?}", x)
        }
    }
    fn amount(x: f64) -> String {
        if x.fract() == 0.0 && x.abs() < 1e15 {
            format!("{}", x as i64)
        } else {
            format!("{:?}", x)
        }
    }
    match l {
        Lit::Int(i) => format!("{}", i),
        Lit::Float(x) => float(*x),
        Lit::Shares(x) => format!("{} shares", amount(*x)),
        Lit::Money(x, c) => format!("{} {}", amount(*x), c),
        Lit::Price(x, c) => format!("{} {}/share", amount(*x), c),
        Lit::Duration(d) => d.to_string(),
        Lit::Str(s) | Lit::Equity(s) | Lit::Label(s) => format!("\"{}\"", s),
    }
}

pub fn render_term(t: &Term, names: NameMap) -> String {
    match t {
        Term::Var(v, _) => names(v),
        Term::Wild(_) => "_".into(),
        Term::Param(p, _) => p.clone(),
        Term::Lit(l, _) => render_lit(l),
        Term::Ctor(c, args, _) => format!("{}({})", c, args.iter().map(|a| render_term(a, names)).collect::<Vec<_>>().join(", ")),
    }
}

pub fn render_expr(e: &Expr, names: NameMap) -> String {
    match e {
        Expr::Var(v, _) => names(v),
        Expr::Param(p, _) => p.clone(),
        Expr::Lit(l, _) => render_lit(l),
        Expr::Neg(x, _) => format!("-{}", render_expr(x, names)),
        Expr::Bin(op, a, b, _) => format!("({} {} {})", render_expr(a, names), op, render_expr(b, names)),
        Expr::Call(f, args, _) => format!("{}({})", f, args.iter().map(|a| render_expr(a, names)).collect::<Vec<_>>().join(", ")),
    }
}

pub fn render_atom(a: &Atom, names: NameMap) -> String {
    format!("{}({})", a.name, a.terms.iter().map(|t| render_term(t, names)).collect::<Vec<_>>().join(", "))
}

fn render_order(by: &[(String, Dir, Span)], names: NameMap) -> String {
    by.iter()
        .map(|(k, d, _)| format!("{} {}", names(k), if *d == Dir::Desc { "desc" } else { "asc" }))
        .collect::<Vec<_>>()
        .join(", ")
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
        },
        Literal::Window { var, kind, base, dur, min, .. } => format!(
            "{} in {}({}, {}, min {})",
            render_term(var, names),
            match kind {
                WindowKind::Window => "window",
                WindowKind::Prior => "prior_window",
                WindowKind::Rows => "rows",
            },
            render_term(base, names),
            render_expr(dur, names),
            render_expr(min, names)
        ),
        Literal::Cmp { op, lhs, rhs, .. } => format!("{} {} {}", render_expr(lhs, names), op, render_expr(rhs, names)),
        Literal::Assign { var, expr, .. } => format!("{} = {}", names(var), render_expr(expr, names)),
        Literal::Agg { var, agg, args, conj, .. } => format!(
            "{} = {}({}) over ({})",
            names(var),
            agg,
            args.iter().map(|a| render_expr(a, names)).collect::<Vec<_>>().join(", "),
            conj.iter().map(|c| render_literal(c, names)).collect::<Vec<_>>().join(", ")
        ),
        Literal::Top { n, atom, by, rank, .. } => {
            let order = by.as_ref().map(|b| render_order(b, names)).unwrap_or_default();
            match (n, rank) {
                (Some(n), _) => format!("top({}, {}, by ({}))", render_expr(n, names), render_atom(atom, names), order),
                (None, Some((k, ties, _))) => format!(
                    "rank({}, by ({}), {}as {})",
                    render_atom(atom, names),
                    order,
                    if *ties == Ties::Average { "ties average, " } else { "" },
                    names(k)
                ),
                (None, None) => format!("top(0, {}, by ({}))", render_atom(atom, names), order),
            }
        }
        Literal::Resample { inner, to, as_var, min, aggs, .. } => format!(
            "resample({} to {} as {}, min {}, {})",
            render_atom(inner, names),
            to,
            names(as_var),
            render_expr(min, names),
            aggs.iter().map(|(x, f, e)| format!("{} = {}({})", names(x), f, render_expr(e, names))).collect::<Vec<_>>().join(", ")
        ),
        Literal::AsOf { atom, at, .. } => format!("{} asof {}", render_atom(atom, names), render_term(at, names)),
    }
}

/// A rule in the surface syntax, its body in `order` (the written order
/// when absent).
pub fn render_rule(rule: &Rule, names: NameMap, order: Option<&[usize]>) -> String {
    let body: Vec<String> = match order {
        Some(o) => o.iter().map(|&i| render_literal(&rule.body[i], names)).collect(),
        None => rule.body.iter().map(|l| render_literal(l, names)).collect(),
    };
    format!("{} :- {}.", render_atom(&rule.head, names), body.join(", "))
}

pub fn render_signature(sig: &Signature, with_res: bool) -> String {
    let args: Vec<String> = sig
        .args
        .iter()
        .map(|a| {
            let m = match a.mode {
                Mode::In => "+",
                Mode::Out => "-",
                Mode::Key => "@",
            };
            format!("{}{}: {}", m, a.name, a.ty)
        })
        .collect();
    let mut s = format!("{}({})", sig.name, args.join(", "));
    if with_res {
        if let Some(r) = sig.res {
            s.push_str(&format!(" {}", r));
        }
    }
    if sig.complete && matches!(sig.kind, Kind::Primitive { .. }) {
        s.push_str(" complete");
    }
    s
}

pub fn render_param(p: &Param) -> String {
    let mut s = format!("param {} : {} = {}", p.name, p.ty, render_lit(&p.value));
    if let Some((lo, hi)) = &p.range {
        s.push_str(&format!(" in {}..{}", render_lit(lo), render_lit(hi)));
    }
    s
}

/// A rendering of a checked program as source: one environment unit, one
/// library unit per library whose rules or declarations the program
/// carries, and the strategy. `rule_text` renders each rule by its index;
/// `rule_order` gives the order rules are emitted in per unit.
pub struct SourceRendering<'a> {
    pub prog: &'a Program,
    pub strategy_name: &'a str,
    pub rule_text: &'a dyn Fn(usize) -> String,
    pub rule_order: Vec<usize>,
}

impl<'a> SourceRendering<'a> {
    pub fn render(&self) -> String {
        let prog = self.prog;
        let mut out = String::new();
        // The environment.
        let prims: Vec<&Signature> = prog.relations.values().filter(|s| matches!(s.kind, Kind::Primitive { .. })).collect();
        out.push_str(&format!("environment {} {{\n", prog.environment));
        for s in prims {
            out.push_str(&format!("  {}\n", render_signature(s, true)));
        }
        out.push_str("}\n\n");
        // Libraries, in the order their first declaration or rule appears.
        let mut libs: Vec<String> = Vec::new();
        for s in prog.relations.values() {
            if let Kind::Derived { unit } = &s.kind {
                if unit != &prog.strategy && !libs.contains(unit) {
                    libs.push(unit.clone());
                }
            }
        }
        for r in &prog.rules {
            if r.unit != prog.strategy && !libs.contains(&r.unit) {
                libs.push(r.unit.clone());
            }
        }
        for unit in prog.params.keys() {
            if unit != &prog.strategy && !libs.contains(unit) {
                libs.push(unit.clone());
            }
        }
        let unit_of = |name: &str| -> Option<String> {
            match prog.relations.get(name).map(|s| &s.kind) {
                Some(Kind::Derived { unit }) => Some(unit.clone()),
                _ => None,
            }
        };
        for lib in &libs {
            out.push_str(&format!(
                "library {} {{\n  env {}{}\n",
                lib,
                prog.environment,
                prog.environment_version.as_ref().map(|v| format!("@{}", v)).unwrap_or_default()
            ));
            // Libraries this one reads.
            let mut uses: Vec<String> = Vec::new();
            for (i, r) in prog.rules.iter().enumerate() {
                if r.unit != *lib {
                    continue;
                }
                for rf in &prog.infos[i].refs {
                    if let Some(u) = unit_of(&rf.name) {
                        if u != *lib && !uses.contains(&u) {
                            uses.push(u);
                        }
                    }
                }
            }
            if !uses.is_empty() {
                out.push_str(&format!("  uses {}\n", uses.join(", ")));
            }
            let res = unit_resolution(prog, lib);
            out.push_str(&format!("  resolution {}\n", res));
            if let Some(ps) = prog.params.get(lib) {
                for p in ps.values() {
                    out.push_str(&format!("  {}\n", render_param(p)));
                }
            }
            for s in prog.relations.values() {
                if matches!(&s.kind, Kind::Derived { unit } if unit == lib) {
                    out.push_str(&format!("  rel {}\n", render_signature(s, true)));
                }
            }
            for &i in &self.rule_order {
                if prog.rules[i].unit == *lib {
                    out.push_str(&format!("  {}\n", (self.rule_text)(i)));
                }
            }
            out.push_str("}\n\n");
        }
        // The strategy.
        out.push_str(&format!(
            "strategy {} {{\n  env {}{}\n",
            self.strategy_name,
            prog.environment,
            prog.environment_version.as_ref().map(|v| format!("@{}", v)).unwrap_or_default()
        ));
        if let Some(h) = &prog.revises {
            out.push_str(&format!("  revises \"{}\"\n", h));
        }
        if !libs.is_empty() {
            out.push_str(&format!("  uses {}\n", libs.join(", ")));
        }
        out.push_str(&format!("  resolution {}\n  mode {}\n", prog.resolution, prog.mode));
        if let Some(ps) = prog.params.get(&prog.strategy) {
            for p in ps.values() {
                out.push_str(&format!("  {}\n", render_param(p)));
            }
        }
        for s in prog.relations.values() {
            if matches!(&s.kind, Kind::Derived { unit } if *unit == prog.strategy) {
                out.push_str(&format!("  rel {}\n", render_signature(s, true)));
            }
        }
        for &i in &self.rule_order {
            if prog.rules[i].unit == prog.strategy {
                out.push_str(&format!("  {}\n", (self.rule_text)(i)));
            }
        }
        out.push_str("}\n");
        out
    }
}

/// The resolution a unit declares: the one most of its relations carry,
/// else the program's.
pub fn unit_resolution(prog: &Program, unit: &str) -> Resolution {
    let mut counts: BTreeMap<Resolution, usize> = BTreeMap::new();
    for s in prog.relations.values() {
        if matches!(&s.kind, Kind::Derived { unit: u } if u == unit) {
            if let Some(r) = s.res {
                *counts.entry(r).or_default() += 1;
            }
        }
    }
    for (i, r) in prog.rules.iter().enumerate() {
        if r.unit == unit {
            *counts.entry(prog.infos[i].resolution).or_default() += 1;
        }
    }
    counts.into_iter().max_by_key(|(_, n)| *n).map(|(r, _)| r).unwrap_or(prog.resolution)
}

/// The checked program as written (`ir --stage checked`).
pub fn render_checked(prog: &Program) -> String {
    let identity = |v: &str| v.to_string();
    let text = |i: usize| render_rule(&prog.rules[i], &identity, None);
    SourceRendering {
        prog,
        strategy_name: &prog.strategy,
        rule_text: &text,
        rule_order: (0..prog.rules.len()).collect(),
    }
    .render()
}

/// The JSON of a checked program: relations, parameters, rules as text,
/// strata and the complete set.
pub fn checked_json(prog: &Program) -> serde_json::Value {
    let identity = |v: &str| v.to_string();
    serde_json::json!({
        "stage": "checked",
        "strategy": prog.strategy,
        "environment": prog.environment,
        "environment_version": prog.environment_version,
        "resolution": prog.resolution.to_string(),
        "mode": prog.mode.to_string(),
        "relations": prog.relations.values().map(|s| serde_json::json!({
            "name": s.name,
            "kind": kind_name(&s.kind),
            "unit": match &s.kind { Kind::Derived { unit } => Some(unit.clone()), Kind::Primitive { env } => Some(env.clone()), _ => None },
            "signature": render_signature(s, true),
            "complete": prog.complete.contains(&s.name),
        })).collect::<Vec<_>>(),
        "params": prog.params.iter().flat_map(|(u, ps)| ps.values().map(move |p| serde_json::json!({"unit": u, "text": render_param(p)}))).collect::<Vec<_>>(),
        "rules": prog.rules.iter().enumerate().map(|(i, r)| serde_json::json!({
            "label": prog.rule_label(i),
            "unit": r.unit,
            "span": r.span.to_string(),
            "text": render_rule(r, &identity, None),
        })).collect::<Vec<_>>(),
        "strata": prog.strata,
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

/// The parsed units as source (`ir --stage parsed`): the same renderer over
/// the units of a workspace, in the order given.
pub fn render_parsed(units: &[Unit]) -> String {
    let identity = |v: &str| v.to_string();
    let mut out = String::new();
    for u in units {
        out.push_str(&format!("{} {} {{\n", u.kind, u.name));
        if let Some((e, _)) = &u.env {
            out.push_str(&format!("  env {}{}\n", e, u.env_version.as_ref().map(|v| format!("@{}", v)).unwrap_or_default()));
        }
        if let Some(h) = &u.revises {
            out.push_str(&format!("  revises \"{}\"\n", h));
        }
        if !u.uses.is_empty() {
            out.push_str(&format!("  uses {}\n", u.uses.iter().map(|(l, _)| l.clone()).collect::<Vec<_>>().join(", ")));
        }
        if let Some((r, _)) = u.resolution {
            out.push_str(&format!("  resolution {}\n", r));
        }
        if let Some((m, _)) = u.mode {
            out.push_str(&format!("  mode {}\n", m));
        }
        for p in &u.params {
            out.push_str(&format!("  {}\n", render_param(p)));
        }
        for s in &u.rels {
            let prefix = if u.kind == UnitKind::Environment { "" } else { "rel " };
            out.push_str(&format!("  {}{}\n", prefix, render_signature(s, true)));
        }
        for r in &u.rules {
            out.push_str(&format!("  {}\n", render_rule(r, &identity, None)));
        }
        out.push_str("}\n\n");
    }
    out
}
