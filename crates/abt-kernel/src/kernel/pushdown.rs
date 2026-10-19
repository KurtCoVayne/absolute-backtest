//! Pushdown (K-4.5): an aggregate literal the data layer's pass computes
//! (D-6) instead of the evaluator's window. A literal is eligible when
//!
//! - (a) its window's conjunction is the window and one atom of a stored
//!   relation, keyed by the group's instrument and the window's time, its
//!   other arguments distinct variables only the aggregate reads (`count` is
//!   evaluated here: it counts the window's rows, the pass the rows with a
//!   value);
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

use super::hash::FxHashMap;

use super::Sym;
use crate::check::Program;
use crate::ir::{Cal, Expr, Kind, Lit, Literal, Term, WindowKind};

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

/// A pushed literal: its shape, the slots its lookup reads, and the answer
/// rows by label, each label's in instrument order.
#[derive(Clone, Debug)]
pub struct Pushed {
    pub shape: Shape,
    pub entity_slot: usize,
    pub base_slot: usize,
    pub by_time: FxHashMap<i64, Vec<(Sym, Answer)>>,
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
}

/// The shape of rule `rule`'s literal `lit` when it is eligible, else why
/// not, in the words `show plan` prints.
pub fn eligibility(prog: &Program, rule: usize, lit: usize) -> Result<Shape, String> {
    let r = &prog.rules[rule];
    let Some(Literal::Agg { agg, args, conj, within, .. }) = r.body.get(lit) else {
        return Err("not an aggregate".into());
    };
    if agg == "count" {
        return Err("(a) `count` counts the window's rows, and the pass counts the rows with a value".into());
    }
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
    // (a) one atom of a stored relation, keyed by the entity and the window's time.
    let Some(sig) = prog.relations.get(&atom.name) else {
        return Err(format!("(a) `{}` is not a relation of the program", atom.name));
    };
    if !matches!(sig.kind, Kind::Primitive { .. }) {
        return Err(format!("(a) `{}` is not a stored relation", atom.name));
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
    let mut columns = Vec::new();
    let mut level = None;
    for (k, a) in args.iter().enumerate() {
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
    Ok(Shape {
        relation: atom.name.clone(),
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
    })
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
            format!("pushdown: {} over {} of `{}` per instrument", s.function, w, s.relation)
        }
        Err(why) => format!("local: {}", why),
    }
}
