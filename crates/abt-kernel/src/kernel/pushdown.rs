//! Pushdown (K-4.5): an aggregate literal the data layer's pass computes
//! (D-6) instead of the evaluator's window. A literal is eligible when
//!
//! - (a) its window's conjunction is the window and one atom of a stored
//!   relation, keyed by the group's instrument and the window's time, its
//!   other arguments distinct variables only the aggregate reads; or one
//!   atom of a product of two stored relations, a relation whose one rule
//!   is `R(A, T, D) :- p(A, T, P), q(A, T, V), D = P * V` (`count` counts
//!   the window's rows, as the pass does since protocol 1.3, ERRATA E-156);
//! - (b) the window is one of D-6.1's forms: `rows[N]`, `prior_rows[N]`,
//!   `window[D]`, `prior_window[D]` or `bucket(C, T)`, with a length and a
//!   minimum the parameters fix;
//! - (c) the conjunction does not read the window's base time;
//! - (d) the relation is held at the base calendar, the finest.
//!
//! The pushed request carries the program's numerics and walks the labels of
//! the rule's calendar, so its bits are the window's (Proposition D-6.1,
//! ERRATA E-27). The evaluator reads a pushed answer for an instrument and a
//! label only when the latest receipt of the rows it read is no later than
//! the barrier being evaluated (ERRATA E-47): what the window would read locally is
//! then exactly what the pass read. Otherwise, and for an instrument the
//! aggregation itself binds, it evaluates the window locally.
//!
//! A literal is delivered instead (ERRATA E-157) when its function meets no
//! partial arithmetic, its rule is on the decision calendar and its window
//! is that calendar's bucket at the rule's own label (the fold evaluates
//! rules at decision barriers only, so every such label read is a closed
//! bucket) and the pass proves no input row late (`inexact_rows` is
//! 0): the answer is a relation of the stream, merged in receipt order
//! before the barrier of its label, held in a store of the kernel's own and
//! read with no receipt test and no local window.

use super::hash::FxHashMap;

use super::{Sym, Value};
use crate::check::Program;
use crate::ir::{BinOp, Cal, Expr, Kind, Lit, Literal, Term, WindowKind};

/// One answer row of a pushed request: the value, the window's row count,
/// whether the pass met partial arithmetic (the window is then evaluated
/// here, to halt as the rule would), and the latest receipt it waits on.
#[derive(Clone, Debug, PartialEq)]
pub struct Answer {
    pub value: Option<f64>,
    pub count: i64,
    pub error: bool,
    pub r: i64,
}

/// A pushed literal: its shape, the slots its lookup reads, whether it
/// counts, and the answer rows by label, each label's in instrument order;
/// or, for a delivered literal, the store the stream fills with them.
#[derive(Clone, Debug)]
pub struct Pushed {
    pub shape: Shape,
    pub entity_slot: usize,
    pub base_slot: usize,
    /// A `count`: its answer is the window's row count (`Value::Count`).
    pub count: bool,
    pub by_time: FxHashMap<i64, Vec<(Sym, Answer)>>,
    pub delivered: Option<Delivered>,
}

/// Where a delivered literal's answers are (ERRATA E-157): the kernel's
/// stored relation the stream's tuples of the answer go to, the cache
/// object the stream names, and the argument of those tuples the literal
/// reads: 2, the answer's value (a `count`'s count), or 3, the count of a
/// sibling's answer a `count` shares (E-159), read at the literal's own
/// minimum.
#[derive(Clone, Debug, PartialEq)]
pub struct Delivered {
    pub rel: usize,
    pub relation: String,
    pub column: usize,
    pub min: i64,
}

impl Delivered {
    /// The literal's answer in one of its store's tuples: the argument it
    /// reads, none where that is a count below the literal's minimum (a
    /// count sharing a sibling's answer, E-159). Every reader of a delivered
    /// literal, the row path's and the block evaluator's, reads it so.
    pub fn answer(&self, tu: &[Value]) -> Option<Value> {
        match tu.get(self.column) {
            Some(Value::Count(n)) if *n < self.min => None,
            v => v.cloned(),
        }
    }
}

/// What makes a literal's window a pushed request (the parts the program
/// fixes; the relation's store name and columns come with the binding).
#[derive(Clone, Debug, PartialEq)]
pub struct Shape {
    /// The stored relation the window reads.
    pub relation: String,
    /// The atom's argument positions the aggregate reads (one, or two for a
    /// two-column function).
    pub columns: Vec<usize>,
    pub function: String,
    pub level: Option<f64>,
    pub window: WindowKind,
    /// The window's length (`rows[N]`, `window[D]`) and minimum, as written.
    pub dur: Expr,
    pub min: Expr,
    /// The variables the lookup reads: the instrument and the base time;
    /// and the window's time variable, whose calendar the window walks.
    pub entity_var: String,
    pub base_var: String,
    pub window_var: String,
    /// The calendar the rule's labels are on.
    pub cal: Cal,
    /// Whether the window's base is the head's time: the label the rule is
    /// derived at, rather than another label the body binds (the session a
    /// bar belongs to).
    pub base_at_head: bool,
    /// A product's second factor, when the window's atom is a product of
    /// two stored relations: `relation` and `columns` are then the first
    /// factor's.
    pub factor: Option<Factor>,
}

/// The second factor of a product (`q` in `R(A, T, D) :- p(A, T, P), q(A,
/// T, V), D = P * V`): its stored relation and the argument position of its
/// value; and `product`, the relation the window's atom names.
#[derive(Clone, Debug, PartialEq)]
pub struct Factor {
    pub product: String,
    pub relation: String,
    pub column: usize,
}

/// The two factors of `name` when it is the product of two stored
/// relations: a derived relation at the base calendar of the form
/// `R(A, T, D)`, with exactly one rule,
/// `R(A, T, D) :- p(A, T, P), q(A, T, V), D = P * V`, where `p` and `q`
/// are stored relations at the base calendar of the form `p(A, T, P)`,
/// joined on the instrument and the time and on nothing else. Each factor
/// with the argument position of its value.
pub fn product_of(prog: &Program, name: &str) -> Option<((String, usize), (String, usize))> {
    let sig = prog.relations.get(name)?;
    if !matches!(sig.kind, Kind::Derived { .. }) || sig.cal != Cal::Base || sig.native.is_some() {
        return None;
    }
    // The three positions of an `R(A, T, D)`: the instrument, the time and the value.
    let positions = |s: &crate::ir::Signature| -> Option<(usize, usize, usize)> {
        let key = s.key_pos()?;
        let entity = s.args.iter().position(|a| a.ty.is_entity())?;
        let value = (0..s.args.len()).find(|&i| i != key && i != entity)?;
        (s.args.len() == 3).then_some((entity, key, value))
    };
    let (e, k, v) = positions(sig)?;
    let mut rules = prog.rules.iter().filter(|r| r.head.name == name);
    let (Some(rule), None) = (rules.next(), rules.next()) else { return None };
    let var = |t: &Term| match t {
        Term::Var(v, _) => Some(v.clone()),
        _ => None,
    };
    let (a, t, d) = (var(rule.head.terms.get(e)?)?, var(rule.head.terms.get(k)?)?, var(rule.head.terms.get(v)?)?);
    let [Literal::Atom(p), Literal::Atom(q), Literal::Assign {
        var: dv,
        expr: Expr::Bin(BinOp::Mul, x, y, _),
        ..
    }] = rule.body.as_slice()
    else {
        return None;
    };
    let (Expr::Var(x, _), Expr::Var(y, _)) = (x.as_ref(), y.as_ref()) else { return None };
    // A stored relation's atom `p(A, T, X)`: the position of its value.
    let factor = |atom: &crate::ir::Atom, value: &str| -> Option<usize> {
        let s = prog.relations.get(&atom.name)?;
        if !matches!(s.kind, Kind::Primitive { .. }) || s.cal != Cal::Base || s.native.is_some() {
            return None;
        }
        let (fe, fk, fv) = positions(s)?;
        let terms: Vec<Option<String>> = atom.terms.iter().map(var).collect();
        (terms.len() == 3 && terms[fe].as_ref() == Some(&a) && terms[fk].as_ref() == Some(&t) && terms[fv].as_deref() == Some(value)).then_some(fv)
    };
    let distinct = [&a, &t, &d, x, y].iter().enumerate().all(|(i, u)| [&a, &t, &d, x, y][..i].iter().all(|w| w != u));
    if dv != &d || !distinct || p.name == q.name {
        return None;
    }
    // Either atom may bind either variable: the product is the same bits
    // in either order (IEEE multiplication commutes).
    match (factor(p, x), factor(q, y)) {
        (Some(px), Some(qy)) => Some(((p.name.clone(), px), (q.name.clone(), qy))),
        _ => match (factor(p, y), factor(q, x)) {
            (Some(py), Some(qx)) => Some(((q.name.clone(), qx), (p.name.clone(), py))),
            _ => None,
        },
    }
}

/// The shape of rule `rule`'s literal `lit` when it is eligible, else why
/// not, in the words `show plan` prints.
pub fn eligibility(prog: &Program, rule: usize, lit: usize) -> Result<Shape, String> {
    let r = &prog.rules[rule];
    let Some(Literal::Agg { agg, args, conj, within, .. }) = r.body.get(lit) else {
        return Err("not an aggregate".into());
    };
    let windows: Vec<&Literal> = conj.iter().filter(|l| matches!(l, Literal::Window { .. })).collect();
    let atoms: Vec<&crate::ir::Atom> = conj
        .iter()
        .filter_map(|l| match l {
            Literal::Atom(a) => Some(a),
            _ => None,
        })
        .collect();
    let Some(Literal::Window { var, kind, base, dur, min, lo, .. }) = windows.first().copied() else {
        return Err("(b) the aggregate has no window: it reads its group at the bar".into());
    };
    if windows.len() > 1 {
        return Err("(a) the conjunction holds more than one window".into());
    }
    // A rows window counts the instrument's universe bars, the anchor the
    // checker adds, which are not a stored relation's rows (D-6.1 `rows(N)`).
    if matches!(kind, WindowKind::Rows | WindowKind::PriorRows) && atoms.iter().any(|a| a.name == "universe") {
        return Err("(a) a rows window counts the instrument's universe bars (the checker's anchor), not a stored relation's rows".into());
    }
    let ([atom], 2) = (atoms.as_slice(), conj.len()) else {
        return Err("(a) the window's conjunction is not the window and one atom".into());
    };
    // (b) the window's form.
    match kind {
        WindowKind::Rows | WindowKind::PriorRows | WindowKind::Window | WindowKind::Prior | WindowKind::Bucket(_) => {}
        WindowKind::Session => return Err("(b) a session window is not one of D-6.1's forms (E-12)".into()),
    }
    if lo.is_some() {
        return Err("(b) a window with a lower bound is not one of D-6.1's forms".into());
    }
    for e in [dur, min] {
        let mut vs = Vec::new();
        e.vars(&mut vs);
        if !vs.is_empty() {
            return Err("(b) the window's length or minimum reads a variable".into());
        }
    }
    let (Term::Var(wvar, _), Term::Var(bvar, _)) = (var, base) else {
        return Err("(b) the window's time and base are not variables".into());
    };
    // (a) one atom of a stored relation, or of a product of two, keyed by
    // the entity and the window's time.
    let Some(sig) = prog.relations.get(&atom.name) else {
        return Err(format!("(a) `{}` is not a relation of the program", atom.name));
    };
    let product = product_of(prog, &atom.name);
    if !matches!(sig.kind, Kind::Primitive { .. }) && product.is_none() {
        return Err(format!("(a) `{}` is not a stored relation, nor a product of two", atom.name));
    }
    // (d) the relation at the base calendar.
    if sig.cal != Cal::Base || sig.native.is_some() {
        return Err(format!("(d) `{}` is not held at the base calendar", atom.name));
    }
    let Some(key) = sig.key_pos() else {
        return Err(format!("(a) `{}` has no time key", atom.name));
    };
    let entity_pos = sig.args.iter().position(|a| a.ty.is_entity());
    let Some(entity_pos) = entity_pos else {
        return Err(format!("(a) `{}` has no instrument to group by", atom.name));
    };
    let mut entity_var: Option<String> = None;
    let mut value_vars: Vec<(String, usize)> = Vec::new();
    for (i, t) in atom.terms.iter().enumerate() {
        match t {
            Term::Var(v, _) if i == key => {
                if v != wvar {
                    return Err("(a) the atom's time is not the window's".into());
                }
            }
            Term::Var(v, _) if i == entity_pos => entity_var = Some(v.clone()),
            Term::Var(v, _) => {
                if v == bvar {
                    return Err("(c) the conjunction reads the window's base time".into());
                }
                if value_vars.iter().any(|(x, _)| x == v) || Some(v) == entity_var.as_ref() || v == wvar {
                    return Err("(a) the atom repeats a variable".into());
                }
                value_vars.push((v.clone(), i));
            }
            Term::Wild(_) => {}
            _ => return Err("(a) the atom filters on a constant".into()),
        }
    }
    let Some(entity_var) = entity_var else {
        return Err("(a) the atom's instrument is not a variable".into());
    };
    // The group is the instrument: named by `within`, or shared with the
    // rest of the rule (else the window reads every instrument's rows); and
    // no other variable of the atom is read outside the aggregate, where it
    // would group or filter the window's rows.
    let outer = super::eval::outer_vars(r, &r.body[lit]);
    match within {
        Some(w) if w.as_slice() != std::slice::from_ref(&entity_var) => return Err("(a) the group is not the instrument".into()),
        None if !outer.contains(&entity_var) => return Err("(a) the window reads every instrument's rows: the group is not the instrument".into()),
        _ => {}
    }
    if let Some(v) = value_vars.iter().map(|(v, _)| v).chain(std::iter::once(wvar)).find(|v| outer.contains(v)) {
        return Err(format!("(a) `{}` is read outside the aggregate, which would group or filter the window's rows", v));
    }
    // The aggregate's arguments: the atom's value variables (numbers, WF-3).
    // A `count` reads no value: it counts the window's rows, and the pass
    // is asked for the atom's first number column, whose rows it counts
    // with a value or not.
    let mut columns = Vec::new();
    let mut level = None;
    if agg == "count" {
        let number = |a: &crate::ir::Arg| matches!(a.ty, crate::ir::Ty::Quantity(_) | crate::ir::Ty::Count | crate::ir::Ty::IntLit);
        let Some(first) = (0..sig.args.len()).find(|&i| i != key && i != entity_pos && number(&sig.args[i])) else {
            return Err(format!("(a) `{}` has no number column whose rows the pass counts", atom.name));
        };
        columns.push(first);
    }
    for (k, a) in args.iter().enumerate().filter(|_| agg != "count") {
        match a {
            Expr::Var(v, _) => match value_vars.iter().find(|(x, _)| x == v) {
                Some((_, pos)) => columns.push(*pos),
                None => return Err(format!("(a) the aggregate reads `{}`, which the atom does not bind", v)),
            },
            Expr::Lit(Lit::Float(x), _) if agg == "quantile" && k == 1 => level = Some(*x),
            _ => return Err("(a) the aggregate's argument is not a column of the atom".into()),
        }
    }
    let arity = if matches!(agg.as_str(), "corr" | "cov" | "ols_slope" | "ols_r") { 2 } else { 1 };
    if columns.len() != arity {
        return Err(format!("(a) `{}` reads {} column(s) of the atom", agg, columns.len()));
    }
    if agg == "quantile" && level.is_none() {
        return Err("(a) a quantile's level is not a number".into());
    }
    let cal = prog.infos[rule].cal.clone();
    let base_at_head = prog
        .relations
        .get(&r.head.name)
        .and_then(|s| s.key_pos())
        .and_then(|kp| r.head.terms.get(kp))
        .is_some_and(|t| matches!(t, Term::Var(v, _) if v == bvar));
    // A product's window reads its value, the first factor's column times
    // the second's.
    let (relation, columns, factor) = match product {
        Some(((p, pv), (q, qv))) => (
            p,
            vec![pv],
            Some(Factor {
                product: atom.name.clone(),
                relation: q,
                column: qv,
            }),
        ),
        None => (atom.name.clone(), columns, None),
    };
    Ok(Shape {
        relation,
        columns,
        function: agg.clone(),
        level,
        window: kind.clone(),
        dur: dur.clone(),
        min: min.clone(),
        entity_var,
        base_var: bvar.clone(),
        window_var: wvar.clone(),
        cal,
        base_at_head,
        factor,
    })
}

/// Whether an eligible literal's answer can be delivered in the stream and
/// read with no local window (ERRATA E-157), else why not. The pass must
/// also prove no input row late, which only the data says (`push_down`).
pub fn deliverable(shape: &Shape, prog: &Program) -> Result<(), String> {
    // The pass answers a bucket at every label whose bucket holds a row,
    // and the window, whose minimum is 1, has a value exactly there; it
    // answers a rows or span window only at the group's own rows, and the
    // window is read at any label.
    if !matches!(shape.window, WindowKind::Bucket(_)) {
        return Err("the pass answers its window only at the group's own rows, and the rule reads it at any label".into());
    }
    // A function that can fail (a correlation of a constant series) is
    // evaluated here to halt where the rule does.
    if !matches!(shape.function.as_str(), "first" | "last" | "min" | "max" | "sum" | "mean" | "count" | "median" | "quantile") {
        return Err(format!("`{}` can meet partial arithmetic", shape.function));
    }
    // Rules are evaluated at the decision barriers alone, so a label of the
    // decision calendar is read once its bucket is closed; a label of
    // another calendar can be read while its bucket is open, from the rows
    // received so far.
    if shape.cal != prog.decision_cal {
        return Err("its rule is not on the decision calendar, whose labels alone are read closed".into());
    }
    // The bucket read closed is the decision calendar's at the label the
    // rule is derived at. Another bucket, or one at a label the body binds
    // (`bucket(session, S)` with `session_of(A, T, S)` decided at every
    // bar), is read before its label's barrier, from the rows received so
    // far, where the pass has no answer yet.
    if !matches!(&shape.window, WindowKind::Bucket(c) if *c == prog.decision_cal) || !shape.base_at_head {
        return Err("its bucket is not the decision calendar's at the rule's own label, so it can be read while it is open".into());
    }
    Ok(())
}

/// The aggregate literals of a program the stream can deliver, by (rule,
/// literal), with the calendar of their labels: each has a stored relation
/// of the kernel's (`delivered_name`), whether or not a run delivers it.
pub fn deliverable_literals(prog: &Program) -> Vec<((usize, usize), Cal)> {
    eligibilities(prog)
        .into_iter()
        .filter_map(|(at, e)| e.ok().filter(|s| deliverable(s, prog).is_ok()).map(|s| (at, s.cal)))
        .collect()
}

/// The name of the kernel's stored relation holding a delivered literal's
/// answers: no relation of a program can be named so.
pub fn delivered_name(prog: &Program, rule: usize, lit: usize) -> String {
    format!("pushed.{}.{}", prog.rule_label(rule), lit)
}

/// The eligibility of every aggregate literal of a program, by (rule,
/// literal).
pub fn eligibilities(prog: &Program) -> Vec<((usize, usize), Result<Shape, String>)> {
    let mut out = Vec::new();
    for (ri, r) in prog.rules.iter().enumerate() {
        for (li, l) in r.body.iter().enumerate() {
            if matches!(l, Literal::Agg { .. }) {
                out.push(((ri, li), eligibility(prog, ri, li)));
            }
        }
    }
    out
}

/// How `show plan` states a literal's pushdown.
pub fn describe(e: &Result<Shape, String>) -> String {
    match e {
        Ok(s) => {
            let w = match &s.window {
                WindowKind::Rows => format!("rows[{}]", s.dur),
                WindowKind::PriorRows => format!("prior_rows[{}]", s.dur),
                WindowKind::Window => format!("window[{}]", s.dur),
                WindowKind::Prior => format!("prior_window[{}]", s.dur),
                WindowKind::Bucket(c) => format!("bucket({})", c),
                WindowKind::Session => "session".into(),
            };
            match &s.factor {
                Some(f) => format!("pushdown: {} over {} of `{}` times `{}` per instrument", s.function, w, s.relation, f.relation),
                None => format!("pushdown: {} over {} of `{}` per instrument", s.function, w, s.relation),
            }
        }
        Err(why) => format!("local: {}", why),
    }
}
