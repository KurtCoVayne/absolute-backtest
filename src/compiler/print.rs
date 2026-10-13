//! Printable stage results (compiler design, section 2.3): every stage
//! result renders as text for a reader and as JSON for tools. This module
//! also holds the renderer of the core IR back to the surface syntax of the
//! second formulation, which the Normalise stage uses for its re-parseable
//! rendering, and the rendering of a checked program as written
//! (`ir --stage checked`).

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
        Lit::Quantity(x, u) => format!("{} {}", amount(*x), u),
        Lit::Money(x, c) => format!("{} {}", amount(*x), c),
        Lit::Price(x, c) => format!("{} {}/unit", amount(*x), c),
        Lit::Duration(d) => d.to_string(),
        Lit::Time(t) if *t == i64::MIN => "1970-01-01".to_string(),
        Lit::Time(t) => crate::kernel::time::format_timestamp(*t),
        Lit::Str(s) | Lit::Entity(s) | Lit::Label(s) => format!("\"{}\"", s),
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

/// A rendering of a checked, linked program as source in the second
/// formulation: the nine blocks of the strategy with every instantiated
/// relation and view written out as a rule of the program (the linked
/// program is first order). `rule_text` renders each rule by its index;
/// `rule_order` gives the order rules are emitted in.
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
        out.push_str(&format!("strategy {} {{\n", self.strategy_name));
        // data
        let d = &prog.demand;
        out.push_str(&format!("  data {{\n    class {}\n", d.class));
        if let Some(r) = d.bars {
            out.push_str(&format!(
                "    need bars {}{}{}\n",
                r,
                d.bars_from.map(|t| format!(" from {}", crate::kernel::time::format_timestamp(t))).unwrap_or_default(),
                if d.bars_complete { " complete" } else { "" }
            ));
        }
        let mut words: Vec<&str> = Vec::new();
        if d.actions {
            words.push("actions");
        }
        if d.listing {
            words.push("listing");
        }
        if d.sessions {
            words.push("sessions");
        }
        if d.point_value {
            words.push("point_value");
        }
        if d.commission {
            words.push("commission");
        }
        if !words.is_empty() {
            out.push_str(&format!("    need {}\n", words.join(", ")));
        }
        if !d.lists.is_empty() {
            out.push_str(&format!("    need lists {}\n", d.lists.iter().map(|l| format!("\"{}\"", l)).collect::<Vec<_>>().join(", ")));
        }
        if !d.series.is_empty() {
            let items: Vec<String> = d.series.iter().map(|(n, r)| format!("\"{}\"{}", n, r.map(|r| format!(" {}", r)).unwrap_or_default())).collect();
            out.push_str(&format!("    need series {}{}\n", items.join(", "), if d.series_complete { " complete" } else { "" }));
        }
        out.push_str(&format!("    {}\n    prefer {}\n  }}\n", if d.require_tested { "require tested" } else { "allow untested" }, d.prefer));
        // calendars, clock
        let declared: Vec<String> = prog.calendars.iter().filter(|(n, _)| n.as_str() != "base").map(|(n, c)| format!("{} = {}", n, c)).collect();
        if !declared.is_empty() {
            out.push_str(&format!("  calendars {{ {} }}\n", declared.join("   ")));
        }
        out.push_str(&format!(
            "  clock {{ decide on {}{} }}\n",
            prog.clock.calendar,
            prog.clock.offset.map(|d| format!(" at session + {}", d)).unwrap_or_default()
        ));
        // universe
        out.push_str("  universe {\n");
        if !prog.instruments.is_empty() {
            let rows: Vec<String> = prog
                .instruments
                .iter()
                .map(|(n, f)| format!("{}{}", n, f.map(|t| format!(" from {}", crate::kernel::time::format_timestamp(t))).unwrap_or_default()))
                .collect();
            out.push_str(&format!("    instruments {{ {} }}\n", rows.join("; ")));
        }
        for &i in &self.rule_order {
            if prog.rules[i].head.name == "universe" {
                out.push_str(&format!("    {}\n", (self.rule_text)(i)));
            }
        }
        out.push_str("  }\n");
        // params
        out.push_str("  params {\n");
        if let Some(ps) = prog.params.get(&prog.strategy) {
            for p in ps.values() {
                out.push_str(&format!("    {}\n", render_param(p)));
            }
        }
        out.push_str("  }\n");
        // rules: every derived relation but the universe, the instruments
        // table and decide, with its rules.
        out.push_str("  rules {\n");
        let mut rels: Vec<&Signature> = prog
            .relations
            .values()
            .filter(|s| matches!(s.kind, Kind::Derived { .. }) && !matches!(s.name.as_str(), "universe" | "instruments"))
            .collect();
        rels.sort_by(|a, b| a.name.cmp(&b.name));
        for s in rels {
            out.push_str(&format!("    rel {}\n", render_signature(s, true)));
            for &i in &self.rule_order {
                if prog.rules[i].head.name == s.name {
                    out.push_str(&format!("    {}\n", (self.rule_text)(i)));
                }
            }
        }
        for o in &prog.observes {
            let wheres = if o.wheres.is_empty() {
                String::new()
            } else {
                format!(" where {}", o.wheres.iter().map(|(v, l)| format!("{} = {}", v, render_lit(l))).collect::<Vec<_>>().join(", "))
            };
            out.push_str(&format!("    observe {}{}{}\n", o.rel, wheres, if o.trace_instances { " trace instances" } else { "" }));
        }
        out.push_str("  }\n");
        // decisions
        out.push_str(&format!(
            "  decisions {{\n    silence {}\n    orders {}\n",
            if prog.silence_flat { "flat" } else { "hold" },
            prog.default_order
        ));
        for &i in &self.rule_order {
            if prog.rules[i].head.name == "decide" {
                out.push_str(&format!("    {}\n", (self.rule_text)(i)));
            }
        }
        out.push_str("  }\n");
        // execution, report
        for line in prog.exec.to_string().lines() {
            out.push_str(&format!("  {}\n", line));
        }
        out.push_str(&format!("  {}\n", prog.report));
        out.push_str("}\n");
        out
    }
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
