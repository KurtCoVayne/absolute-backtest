//! The surface syntax back from the AST (corpus-v2/README.md, "The grammar
//! the corpus assumes"): a strategy as the program would write it, block by
//! block, for `ir --stage checked`, the normalised stage's re-parseable
//! rendering and the readers of a program. The rendering parses back to
//! the same AST up to spans and spelling: a float keeps its point, a
//! quantity its unit word, a version its quotes.

use std::collections::HashMap;

use super::ast::*;
use crate::ir::{Expr, Lit, Span, Term, Ties};
use crate::kernel::time::format_timestamp;

/// A map from variable names to the names they are rendered with.
pub type NameMap<'a> = &'a dyn Fn(&str) -> String;

/// How one written rule is rendered: its variables through `names` (the
/// written name to the name rendered; a name absent is rendered as
/// written) and its body literals in `order` (indices into the written
/// body; an index past the body is ignored, an index left out is appended).
pub struct RuleStyle {
    pub names: HashMap<String, String>,
    pub order: Vec<usize>,
}

/// What a rendering substitutes: the execution and report blocks' text (the
/// expanded blocks of a checked program), and per rule, by its span, a style.
#[derive(Default)]
pub struct Options<'a> {
    pub execution: Option<&'a str>,
    pub report: Option<&'a str>,
    pub rule_style: Option<&'a dyn Fn(Span) -> Option<RuleStyle>>,
}

/// A literal value as the lexer reads it back: a float always carries a
/// point or an exponent, so that `2.0` does not come back as the integer
/// `2`; a quantity, a notional and a price carry their unit words.
pub fn render_lit(l: &Lit) -> String {
    fn float(x: f64) -> String {
        if x.is_finite() && x.fract() == 0.0 && x.abs() < 1e15 {
            format!("{:.1}", x)
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
        Lit::Time(t) => format_timestamp(*t),
        Lit::Str(s) | Lit::Entity(s) | Lit::Label(s) => format!("\"{}\"", s),
    }
}

/// A number as the parser reads it back: whole as an integer, else shortest.
fn amount(x: f64) -> String {
    if x.is_finite() && x.fract() == 0.0 && x.abs() < 1e15 {
        format!("{}", x as i64)
    } else {
        format!("{:?}", x)
    }
}

pub fn render_term(t: &Term, names: NameMap) -> String {
    match t {
        Term::Var(v, _) => names(v),
        Term::Wild(_) => "_".into(),
        Term::Param(p, _) => p.clone(),
        Term::Lit(l, _) => render_lit(l),
        Term::Ctor(c, args, _) => crate::ir::ctor_text(c, &args.iter().map(|a| render_term(a, names)).collect::<Vec<_>>()),
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

/// `name[brackets]@cal`: an atom before its terms, a bracketed relation.
fn render_head(a: &Atom) -> String {
    let mut s = a.name.clone();
    if !a.brackets.is_empty() {
        s.push('[');
        s.push_str(&a.brackets.iter().map(render_bracket).collect::<Vec<_>>().join(", "));
        s.push(']');
    }
    if let Some((c, _)) = &a.cal {
        s.push('@');
        s.push_str(c);
    }
    s
}

fn render_bracket(b: &BracketArg) -> String {
    match b {
        BracketArg::Lit(l, _) => render_lit(l),
        BracketArg::Name(n, _) => n.clone(),
        BracketArg::Nested(a) => render_head(a),
    }
}

pub fn render_atom(a: &Atom, names: NameMap) -> String {
    format!("{}({})", render_head(a), a.terms.iter().map(|t| render_term(t, names)).collect::<Vec<_>>().join(", "))
}

fn render_within(w: &Option<Vec<(String, Span)>>, names: NameMap) -> String {
    match w {
        Some(vs) => format!(" within ({})", vs.iter().map(|(v, _)| names(v)).collect::<Vec<_>>().join(", ")),
        None => String::new(),
    }
}

fn render_window(w: &WindowSpec, names: NameMap) -> String {
    let min = |m: &Option<Expr>| m.as_ref().map(|e| format!(" min {}", render_expr(e, names))).unwrap_or_default();
    match w {
        WindowSpec::Rows { n, min: m } => format!("rows[{}]{}", render_expr(n, names), min(m)),
        WindowSpec::PriorRows { n, min: m } => format!("prior_rows[{}]{}", render_expr(n, names), min(m)),
        WindowSpec::Window { dur, min: m } => format!("window[{}]{}", render_expr(dur, names), min(m)),
        WindowSpec::PriorWindow { dur, min: m } => format!("prior_window[{}]{}", render_expr(dur, names), min(m)),
        WindowSpec::Bucket { cal: (c, _), base } => format!("bucket({}, {})", c, render_term(base, names)),
        WindowSpec::Session { base, lo, hi } => format!("session({}, [{}, {}))", render_term(base, names), render_expr(lo, names), render_expr(hi, names)),
    }
}

/// A literal in the surface syntax, every variable through `names`.
pub fn render_literal(l: &Literal, names: NameMap) -> String {
    match l {
        Literal::Atom(a) => render_atom(a, names),
        Literal::Neg(a) => format!("not {}", render_atom(a, names)),
        Literal::Prev { t, t1, .. } => format!("prev({}, {})", render_term(t, names), render_term(t1, names)),
        Literal::Lag { t, n, t1, .. } => format!("lag({}, {}, {})", render_term(t, names), render_expr(n, names), render_term(t1, names)),
        Literal::MonthStart { t, .. } => format!("month_start({})", render_term(t, names)),
        Literal::DayStart { t, .. } => format!("day_start({})", render_term(t, names)),
        Literal::SessionOf { a, t, s, .. } => format!("session_of({}, {}, {})", render_term(a, names), render_term(t, names), render_term(s, names)),
        Literal::Window { var, spec, .. } => format!("{} in {}", render_term(var, names), render_window(spec, names)),
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
            let order = by
                .as_ref()
                .map(|b| {
                    let keys: Vec<String> = b.iter().map(|(k, d, _)| format!("{} {}", names(k), if *d == crate::ir::Dir::Desc { "desc" } else { "asc" })).collect();
                    format!(" by ({})", keys.join(", "))
                })
                .unwrap_or_default();
            let within = render_within(within, names);
            match (n, rank) {
                (Some(n), _) => format!("top[{}]({}{}){}", render_expr(n, names), render_atom(atom, names), order, within),
                (None, Some((k, ties, _))) => format!(
                    "{} = {}({}{}){}",
                    names(k),
                    if *ties == Ties::Average { "rank_average" } else { "rank" },
                    render_atom(atom, names),
                    order,
                    within
                ),
                (None, None) => format!("top[0]({}{}){}", render_atom(atom, names), order, within),
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
/// when absent; a literal `order` leaves out follows the ordered ones).
pub fn render_rule(r: &Rule, names: NameMap, order: Option<&[usize]>) -> String {
    let mut idx: Vec<usize> = match order {
        Some(o) => o.iter().copied().filter(|&i| i < r.body.len()).collect(),
        None => (0..r.body.len()).collect(),
    };
    for i in 0..r.body.len() {
        if !idx.contains(&i) {
            idx.push(i);
        }
    }
    let body: Vec<String> = idx.iter().map(|&i| render_literal(&r.body[i], names)).collect();
    let head = render_atom(&r.head, names);
    if body.is_empty() {
        format!("{}.", head)
    } else {
        format!("{} :- {}.", head, body.join(", "))
    }
}

fn render_ty(t: &TyName) -> String {
    match &t.arg {
        Some(a) => format!("{}<{}>", t.name, a),
        None => t.name.clone(),
    }
}

/// `@T`, `@T: weekly`, `@X: base | session`, `A: Equity`, or a shape's
/// bare type.
fn render_arg(a: &ArgDecl) -> String {
    if a.key {
        let mut s = format!("@{}", a.name);
        if !a.cals.is_empty() {
            s.push_str(": ");
            s.push_str(&a.cals.iter().map(|(c, _)| c.as_str()).collect::<Vec<_>>().join(" | "));
        }
        s
    } else {
        let ty = a.ty.as_ref().map(render_ty).unwrap_or_default();
        if a.name.is_empty() {
            ty
        } else {
            format!("{}: {}", a.name, ty)
        }
    }
}

/// `name[IN: TY, R: rel(Entity, @T, D)](A: Equity, @T, X: TY) [complete]`.
pub fn render_rel_decl(d: &RelDecl) -> String {
    let mut s = d.name.clone();
    if !d.inputs.is_empty() {
        let ins: Vec<String> = d
            .inputs
            .iter()
            .map(|i| match &i.ty {
                InputTy::Value(t) => format!("{}: {}", i.name, render_ty(t)),
                InputTy::Rel(shape) => format!("{}: rel({})", i.name, shape.iter().map(render_arg).collect::<Vec<_>>().join(", ")),
            })
            .collect();
        s.push('[');
        s.push_str(&ins.join(", "));
        s.push(']');
    }
    s.push('(');
    s.push_str(&d.args.iter().map(render_arg).collect::<Vec<_>>().join(", "));
    s.push(')');
    if d.complete {
        s.push_str(" complete");
    }
    s
}

pub fn render_param(p: &ParamDecl) -> String {
    let mut s = format!("{} : {} = {}", p.name, render_ty(&p.ty), render_lit(&p.value));
    if let Some((lo, hi)) = &p.range {
        s.push_str(&format!(" in {}..{}", render_lit(lo), render_lit(hi)));
    }
    s
}

fn render_need(n: &Need) -> String {
    let body = match &n.kind {
        NeedKind::Bars { res, from } => format!(
            "bars{}{}",
            res.map(|r| format!(" {}", r)).unwrap_or_default(),
            from.map(|t| format!(" from {}", format_timestamp(t))).unwrap_or_default()
        ),
        NeedKind::Actions => "actions".into(),
        NeedKind::Listing => "listing".into(),
        NeedKind::Lists(ls) => format!("lists {}", ls.iter().map(|l| format!("\"{}\"", l)).collect::<Vec<_>>().join(", ")),
        NeedKind::Series(items) => format!(
            "series {}",
            items
                .iter()
                .map(|(s, r)| format!("\"{}\"{}", s, r.map(|r| format!(" {}", r)).unwrap_or_default()))
                .collect::<Vec<_>>()
                .join(", ")
        ),
        NeedKind::Sessions => "sessions".into(),
        NeedKind::PointValue => "point_value".into(),
        NeedKind::Tick => "tick".into(),
        NeedKind::Industry => "industry".into(),
        NeedKind::Commission => "commission".into(),
        NeedKind::Instruments { attr, value } => format!("instruments where {} = \"{}\"", attr, value),
        NeedKind::Chain(c) => format!(
            "chain of \"{}\" within {}..{} and {}..{}{}{}",
            c.underlying,
            c.expiry.0,
            c.expiry.1,
            amount(c.moneyness.0),
            amount(c.moneyness.1),
            c.right.as_ref().map(|r| format!(" right {}", r)).unwrap_or_default(),
            c.spot.as_ref().map(|r| format!(" spot {}", r)).unwrap_or_default()
        ),
    };
    format!("need {}{}", body, if n.complete { " complete" } else { "" })
}

fn render_budget(b: &Budget) -> String {
    let mut items = Vec::new();
    if let Some(m) = b.money {
        items.push(format!("money {}", amount(m)));
    }
    if let Some(x) = b.bytes {
        items.push(format!("bytes {}", x));
    }
    if let Some(x) = b.requests {
        items.push(format!("requests {}", x));
    }
    if let Some(x) = b.seconds {
        items.push(format!("seconds {}", amount(x)));
    }
    format!("budget {{ {} }}", items.join("; "))
}

fn render_observe(o: &Observe) -> String {
    let mut s = format!("observe {}", o.rel);
    if !o.wheres.is_empty() {
        s.push_str(&format!(
            " where {}",
            o.wheres.iter().map(|(v, l, _)| format!("{} = {}", v, render_lit(l))).collect::<Vec<_>>().join(", ")
        ));
    }
    if let Some(t) = &o.trace {
        s.push_str(&format!(" trace {}", t));
    }
    s
}

/// One field of an execution block, as the parser reads it.
pub fn render_exec_field(f: &ExecField) -> String {
    let value = match &f.value {
        ExecValue::Price { rel, cal } => format!("{}{}", rel, cal.as_ref().map(|c| format!("@{}", c)).unwrap_or_default()),
        ExecValue::Word(w) => w.clone(),
        ExecValue::Capital { amount: a, compounding } => format!("{} USD {}", amount(*a), if *compounding { "compounding" } else { "fixed" }),
        ExecValue::CommissionPerShare { per_share, min } => format!("per_share {} USD min {} USD", amount(*per_share), amount(*min)),
        ExecValue::CommissionPerContract => "per_contract from catalog".into(),
        ExecValue::CommissionPerContractFixed(x) => format!("per_contract {} USD", amount(*x)),
        ExecValue::OptionFill { base, spread } => {
            if *spread == 0.0 {
                base.clone()
            } else {
                format!("{} + {} spread", base, amount(*spread))
            }
        }
        ExecValue::Margin(models) => models
            .iter()
            .map(|(m, f)| match f {
                Some(f) => format!("{} {}", m, amount(*f)),
                None => m.clone(),
            })
            .collect::<Vec<_>>()
            .join(", "),
        ExecValue::Exercise(r) => crate::check::exercise_text(r),
        ExecValue::CommissionBps(x) => format!("{} bps per side", amount(*x)),
        ExecValue::SlippageFixed(x) => format!("fixed {} bps", amount(*x)),
        ExecValue::SlippageVol { mult, bars } => format!("vol {} over {} bars", amount(*mult), bars),
        ExecValue::Impact { coef, days } => format!("sqrt {} over {} days adv", amount(*coef), days),
        ExecValue::Participation(x) => format!("{} of bar volume", amount(*x)),
        ExecValue::Financing { cash, debit, short } => format!("cash {} debit {} short {}", amount(*cash), amount(*debit), amount(*short)),
        ExecValue::LeverageGross { max, policy } => format!("gross <= {} {}", amount(*max), policy),
        ExecValue::Haircuts(hs) => format!("haircut {{ {} }}", hs.iter().map(|(r, x)| format!("{} {}", r, amount(*x))).collect::<Vec<_>>().join(", ")),
        ExecValue::Limits(items) => {
            let parts: Vec<String> = items
                .iter()
                .map(|l| {
                    let mut s = format!("{} <= {}", l.kind, amount(l.bound));
                    if let Some(p) = &l.per {
                        s.push_str(&format!(" per {}", p));
                    }
                    if let Some(p) = &l.policy {
                        s.push_str(&format!(" {}", p));
                    }
                    s
                })
                .collect();
            format!("{{ {} }}", parts.join("; "))
        }
        ExecValue::None => "none".into(),
    };
    format!("{} {}", f.name, value)
}

/// A strategy as the program writes it: the nine blocks, the rules of a
/// relation under its declaration.
pub fn render_strategy(s: &Strategy, opts: &Options) -> String {
    let rule = |r: &Rule| -> String {
        match opts.rule_style.and_then(|f| f(r.span)) {
            Some(st) => {
                let names = |v: &str| st.names.get(v).cloned().unwrap_or_else(|| v.to_string());
                render_rule(r, &names, Some(&st.order))
            }
            None => render_rule(r, &|v: &str| v.to_string(), None),
        }
    };
    let mut out = format!("strategy {} {{\n", s.name);
    if let Some(d) = &s.data {
        out.push_str("  data {\n");
        if let Some((c, _)) = &d.class {
            out.push_str(&format!("    class {}\n", c));
        }
        for n in &d.needs {
            out.push_str(&format!("    {}\n", render_need(n)));
        }
        match d.require {
            Some((Require::Reviewed, _)) => out.push_str("    require reviewed\n"),
            Some((Require::AllowUnreviewed, _)) => out.push_str("    allow unreviewed\n"),
            None => {}
        }
        match &d.prefer {
            Some((Prefer::Latest, _)) => out.push_str("    prefer latest\n"),
            Some((Prefer::Version(v), _)) => out.push_str(&format!("    prefer version \"{}\"\n", v)),
            Some((Prefer::BaseFinest, _)) => out.push_str("    prefer base finest\n"),
            None => {}
        }
        if let Some((b, _)) = &d.budget {
            out.push_str(&format!("    {}\n", render_budget(b)));
        }
        out.push_str("  }\n");
    }
    if !s.uses.is_empty() {
        out.push_str(&format!("  uses {}\n", s.uses.iter().map(|(u, _)| u.as_str()).collect::<Vec<_>>().join(", ")));
    }
    if let Some(c) = &s.calendars {
        let decls: Vec<String> = c.decls.iter().map(|(n, cal, _)| format!("{} = {}", n, cal)).collect();
        out.push_str(&format!("  calendars {{ {} }}\n", decls.join("   ")));
    }
    if let Some(c) = &s.clock {
        out.push_str(&format!(
            "  clock {{ decide on {}{} }}\n",
            c.on.0,
            c.offset.as_ref().map(|(l, _)| format!(" at session + {}", render_lit(l))).unwrap_or_default()
        ));
    }
    if let Some(u) = &s.universe {
        out.push_str("  universe {\n");
        if let Some((rows, _)) = &u.instruments {
            let rows: Vec<String> = rows
                .iter()
                .map(|(n, from, _)| format!("{}{}", n, from.map(|t| format!(" from {}", format_timestamp(t))).unwrap_or_default()))
                .collect();
            out.push_str(&format!("    instruments {{ {} }}\n", rows.join("; ")));
        }
        for r in &u.rules {
            out.push_str(&format!("    {}\n", rule(r)));
        }
        out.push_str("  }\n");
    }
    if let Some(p) = &s.params {
        out.push_str("  params {\n");
        for d in &p.params {
            out.push_str(&format!("    {}\n", render_param(d)));
        }
        out.push_str("  }\n");
    }
    if let Some(b) = &s.rules {
        out.push_str("  rules {\n");
        let mut written: Vec<bool> = vec![false; b.rules.len()];
        for d in &b.rels {
            out.push_str(&format!("    rel {}\n", render_rel_decl(d)));
            for (i, r) in b.rules.iter().enumerate() {
                if !written[i] && r.head.name == d.name {
                    written[i] = true;
                    out.push_str(&format!("    {}\n", rule(r)));
                }
            }
        }
        for (i, r) in b.rules.iter().enumerate() {
            if !written[i] {
                out.push_str(&format!("    {}\n", rule(r)));
            }
        }
        for o in &b.observes {
            out.push_str(&format!("    {}\n", render_observe(o)));
        }
        out.push_str("  }\n");
    }
    if let Some(d) = &s.decisions {
        out.push_str("  decisions {\n");
        if let Some((x, _)) = &d.silence {
            out.push_str(&format!("    silence {}\n", x));
        }
        if let Some((x, _)) = &d.orders {
            out.push_str(&format!("    orders {}\n", x));
        }
        for r in &d.rules {
            out.push_str(&format!("    {}\n", rule(r)));
        }
        out.push_str("  }\n");
    }
    match (opts.execution, &s.execution) {
        (Some(text), _) => {
            for line in text.lines() {
                out.push_str(&format!("  {}\n", line));
            }
        }
        (None, Some(ExecutionBlock::Preset { name, with, .. })) => {
            if with.is_empty() {
                out.push_str(&format!("  execution {}\n", name));
            } else {
                out.push_str(&format!("  execution {} with {{\n", name));
                for f in with {
                    out.push_str(&format!("    {}\n", render_exec_field(f)));
                }
                out.push_str("  }\n");
            }
        }
        (None, Some(ExecutionBlock::Fields { fields, .. })) => {
            out.push_str("  execution {\n");
            for f in fields {
                out.push_str(&format!("    {}\n", render_exec_field(f)));
            }
            out.push_str("  }\n");
        }
        (None, None) => {}
    }
    match (opts.report, &s.report) {
        (Some(text), _) => out.push_str(&format!("  {}\n", text)),
        (None, Some(r)) => {
            let mut parts: Vec<String> = Vec::new();
            if let Some((c, _)) = &r.calendar {
                parts.push(format!("calendar {}", c));
            }
            if let Some((n, _)) = r.periods {
                parts.push(format!("periods {} per year", amount(n)));
            }
            if let Some((lo, hi, _)) = r.window {
                parts.push(format!("window {} .. {}", format_timestamp(lo), format_timestamp(hi)));
            }
            out.push_str(&format!("  report {{ {} }}\n", parts.join("   ")));
        }
        (None, None) => {}
    }
    out.push_str("}\n");
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::syntax::parser::parse_units;

    /// The parser test's strategy renders to a text that parses to the same
    /// AST up to spans, and renders again to the same text.
    #[test]
    fn a_strategy_round_trips_through_its_rendering() {
        let src = r#"
strategy s {
  data { class equity   need bars @1d from 1990-01-01 complete   need actions, listing   need lists "SP500", "SP400"   need series "SPXTR" @1d, "VIX" @1w complete   require reviewed   prefer version "2026.10" }
  uses calendar, stat
  calendars { weekly = weeks(1)   es = sessions of "ES" }
  clock { decide on weekly at session + 5min }
  universe { instruments { ES from 2000-01-01; 6J from 2001-01-01; ETH }   universe(A, T) :- listed(A, T), traded@base(A, T). }
  params { n : Count = 7   late : Scalar = 1/3   floor : Price<USD> = 3 USD/share in 1 USD/share..10 USD/share   entry : Scalar = -2.0 in -4.0..-1.0 }
  rules {
    rel sma[N: Count, R: rel(Entity, @T, D)](A: Equity, @T, M: D)
    sma[N, R](A, T, M) :- universe(A, T), M = mean(X) over (T1 in rows[N] min 3, R(A, T1, X)).
    rel open0(A: Future, @X: base | session, O: Price<USD>)
    open0(A, X, O) :- O = first(P) over (T1 in session(X, [0min, 1min)), open(A, T1, P)).
    rel ranked(A: Equity, @T, K: Count)
    ranked(A, T, K) :- K = rank(scored(A, T, S) by (S desc, A asc)) within (T), zscore_rows[20, mean_rows[5, close]](A, T, Z).
    rel scale(@T, S: Scalar)
    scale(T, S) :- bar(T), S = case { not gate(T), not calm(T) -> 0 ; crowded(T) -> crowd_dial ; else -> 1 }.
    rel sel(A: Equity, @T)
    sel(A, T) :- bar(T), top[n](cand(A, T, M) by (M desc, A asc)) within (T), dividend(A, _, Ex, _, _) asof T, Ex > T, prev(T, T0), lag(T, 20d, T2), month_start(T), session_of(A, T, S1), T3 in bucket(weekly, T), W = (M + 1) * -2 / abs(M).
    observe scale
    observe latched where A = "ES" trace instances
  }
  decisions { silence hold   orders moc   decide(T, hold(A, W, limit(P, gtc))) :- target(A, T, W), abs(W - C) >= band - band_tol. }
  execution book_fixed_base with { commission 10 bps per side   leverage gross <= 2 halt   delisting haircut { bankruptcy 1, other 0.5 } }
  report { calendar weekly   periods 52 per year   window 1991-01-01 .. 2026-07-02 }
}
"#;
        let units = parse_units(src).unwrap_or_else(|e| panic!("{}", e));
        let Unit::Strategy(s) = &units[0] else { panic!() };
        let text = render_strategy(s, &Options::default());
        let again = parse_units(&text).unwrap_or_else(|e| panic!("the rendering does not parse: {}\n{}", e, text));
        let Unit::Strategy(s2) = &again[0] else { panic!() };
        assert_eq!(render_strategy(s2, &Options::default()), text);
        assert_eq!(s2.params.as_ref().unwrap().params[1].value, Lit::Float(1.0 / 3.0));
        assert_eq!(s2.data.as_ref().unwrap().needs.len(), s.data.as_ref().unwrap().needs.len());
        assert_eq!(s2.rules.as_ref().unwrap().rules.len(), 5);
        assert_eq!(s2.rules.as_ref().unwrap().rules[4].body.len(), s.rules.as_ref().unwrap().rules[4].body.len());
        assert!(text.contains("prefer version \"2026.10\"") && text.contains("at session + 5min"), "{}", text);
        // A style renames the variables of one rule and reorders its body.
        let span = s.rules.as_ref().unwrap().rules[0].span;
        let style = move |sp: Span| -> Option<RuleStyle> {
            (sp == span).then(|| RuleStyle {
                names: [("A".to_string(), "V0".to_string()), ("T".to_string(), "V1".to_string())].into_iter().collect(),
                order: vec![1, 0],
            })
        };
        let styled = render_strategy(
            s,
            &Options {
                rule_style: Some(&style),
                ..Default::default()
            },
        );
        assert!(
            styled.contains("sma[N, R](V0, V1, M) :- M = mean(X) over (T1 in rows[N] min 3, R(V0, T1, X)), universe(V0, V1)."),
            "{}",
            styled
        );
        parse_units(&styled).unwrap_or_else(|e| panic!("the styled rendering does not parse: {}\n{}", e, styled));
    }
}
