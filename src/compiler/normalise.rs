//! The Normalise stage (compiler design, sections 2.5 to 2.10): a checked
//! program's canonical form. Every derived form is reduced to a core form
//! (2.6), the literals of each rule are ordered by a key that does not
//! depend on variable names (2.7), the variables are renamed by first
//! occurrence (2.8), and the program hash is a SHA-256 of the JSON of the
//! result (2.9). The text rendering is the program in the surface syntax
//! with its canonical names and order, so that it re-parses to the same
//! normal form (the round-trip test).
//!
//! Two readings of each body coexist: the *core* literals, over which the
//! analysis, the rewrites and the hash operate, and the *surface* literals
//! they came from, which the kernel executes and the text rendering prints.
//! A derived form reduces to several core literals (an `asof` join to an
//! assignment and an atom; a `top` to an atom, a count and a comparison);
//! they stay one unit, ordered and moved together.

use std::cmp::Ordering;
use std::collections::{BTreeMap, HashMap, HashSet};

use serde::Serialize;

use super::print::{render_lit, render_literal, render_param, render_rule, render_signature, SourceRendering, Stage};
use super::sha256::sha256_hex;
use crate::check::Program;
use crate::ir::*;

#[derive(Clone, Debug, Serialize, PartialEq)]
pub enum NirTerm {
    Var(String),
    Wild,
    Param(String),
    Lit(String),
    Ctor(String, Vec<NirTerm>),
}

#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct NirAtom {
    pub name: String,
    pub terms: Vec<NirTerm>,
}

#[derive(Clone, Debug, Serialize, PartialEq)]
pub enum NirExpr {
    Var(String),
    Param(String),
    Lit(String),
    Neg(Box<NirExpr>),
    Bin(String, Box<NirExpr>, Box<NirExpr>),
    Call(String, Vec<NirExpr>),
    /// A partial function of time (2.6): `prev(T)`, `lag(T, d)`,
    /// `asof(R, k̄, T)`, `bucket(res, T)`, `month_start(T)`, `day_start(T)`.
    Time(String, Vec<NirExpr>),
    /// Order keys with their directions, for the lexicographic comparison a
    /// reduction's rank count is over.
    Keys(Vec<(NirExpr, String)>),
}

/// The core literal forms (2.5): atom, negation, aggregation, assignment,
/// comparison; a window is a temporal constraint inside a group.
#[derive(Clone, Debug, Serialize, PartialEq)]
pub enum NirLit {
    Atom(NirAtom),
    Neg(NirAtom),
    Agg {
        var: String,
        agg: String,
        args: Vec<NirExpr>,
        group: Vec<NirLit>,
        /// Grouping variables bound by the aggregation (a resample's entities).
        by: Vec<String>,
        min: Option<NirExpr>,
        /// The group literal that anchors a `rows` window (the kernel's
        /// semantics: the group's own rows are the bars at which it holds).
        anchor: Option<usize>,
    },
    /// `X = e`; with no variable, the definedness test of a partial
    /// function (`prev(T, _)`, `month_start(T)`).
    Assign {
        var: Option<String>,
        expr: NirExpr,
    },
    Cmp {
        op: String,
        lhs: NirExpr,
        rhs: NirExpr,
    },
    Window {
        var: String,
        kind: String,
        base: String,
        dur: NirExpr,
        min: NirExpr,
    },
    /// A case form (language-v2, 5.3): the guards in order with their values.
    Case {
        var: String,
        branches: Vec<(Vec<NirLit>, NirExpr)>,
        otherwise: Option<NirExpr>,
    },
}

/// A core literal with the surface literal it came from (its index in the
/// rule's canonical surface body).
#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct NirBodyLit {
    pub lit: NirLit,
    pub unit: usize,
}

#[derive(Clone, Debug, Serialize)]
pub struct NirRule {
    /// `unit::head#k`, numbered in canonical order.
    pub id: String,
    /// Index into the checked program's rules.
    #[serde(skip_serializing)]
    pub origin: usize,
    pub unit: String,
    pub head: NirAtom,
    pub body: Vec<NirBodyLit>,
    /// The surface literals, renamed and in canonical order.
    #[serde(skip_serializing)]
    pub surface: Vec<Literal>,
    /// For each canonical surface literal, its index in the written body.
    #[serde(skip_serializing)]
    pub surface_origin: Vec<usize>,
    /// Canonical name to written name.
    pub names: Vec<(String, String)>,
    /// The rewriter's verdict (R2) with its evidence.
    pub infeasible: Option<String>,
    /// Nothing demanded, observed or queried reaches the head (R5).
    pub elided: bool,
    /// A copy specialised to a parameter tuple (R5): the substitutions.
    pub specialised: Option<Vec<(String, String)>>,
    pub span: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct NirSig {
    pub name: String,
    pub unit: Option<String>,
    pub kind: String,
    pub signature: String,
    pub calendar: String,
    pub complete: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct NirParam {
    pub unit: String,
    pub name: String,
    pub text: String,
}

/// The normal form of a program.
#[derive(Clone, Debug, Serialize)]
pub struct Nir {
    pub strategy: String,
    pub class: String,
    pub base: String,
    pub decision_calendar: String,
    pub clock_offset: Option<String>,
    /// The demand (3.11), the decisions block and the execution block: part
    /// of the program's identity (Propositions 3.13 and 4.3); the report
    /// block is not (4.5).
    pub demand: serde_json::Value,
    pub silence: String,
    pub orders: String,
    pub execution: serde_json::Value,
    pub data: Vec<NirSig>,
    pub relations: Vec<NirSig>,
    pub params: Vec<NirParam>,
    pub rules: Vec<NirRule>,
    pub hash: String,
    /// Where the rewriter's elision log and the hash leave the program's
    /// name: a renamed strategy keeps its hash.
    #[serde(skip_serializing)]
    pub stage: String,
}

// ---------------------------------------------------------------------------
// Reduction of derived forms (2.6)
// ---------------------------------------------------------------------------

fn term(t: &Term) -> NirTerm {
    match t {
        Term::Var(v, _) => NirTerm::Var(v.clone()),
        Term::Wild(_) => NirTerm::Wild,
        Term::Param(p, _) => NirTerm::Param(p.clone()),
        Term::Lit(l, _) => NirTerm::Lit(render_lit(l)),
        Term::Ctor(c, subs, _) => NirTerm::Ctor(c.clone(), subs.iter().map(term).collect()),
    }
}

fn term_expr(t: &Term) -> NirExpr {
    match t {
        Term::Var(v, _) => NirExpr::Var(v.clone()),
        Term::Param(p, _) => NirExpr::Param(p.clone()),
        Term::Lit(l, _) => NirExpr::Lit(render_lit(l)),
        Term::Wild(_) => NirExpr::Lit("_".into()),
        Term::Ctor(c, subs, _) => NirExpr::Call(c.clone(), subs.iter().map(term_expr).collect()),
    }
}

pub fn expr(e: &Expr) -> NirExpr {
    match e {
        Expr::Var(v, _) => NirExpr::Var(v.clone()),
        Expr::Param(p, _) => NirExpr::Param(p.clone()),
        Expr::Lit(l, _) => NirExpr::Lit(render_lit(l)),
        Expr::Neg(x, _) => NirExpr::Neg(Box::new(expr(x))),
        Expr::Bin(op, a, b, _) => NirExpr::Bin(op.to_string(), Box::new(expr(a)), Box::new(expr(b))),
        Expr::Call(f, args, _) => NirExpr::Call(f.clone(), args.iter().map(expr).collect()),
    }
}

fn atom(a: &Atom) -> NirAtom {
    NirAtom {
        name: a.name.clone(),
        terms: a.terms.iter().map(term).collect(),
    }
}

fn var_of(t: &Term) -> Option<String> {
    if let Term::Var(v, _) = t {
        Some(v.clone())
    } else {
        None
    }
}

/// The surface order's binding model (the checker's): a head input is bound
/// on entry; the key and the outputs are bound by the body. What a literal
/// can bind if unbound: the variables in the output positions of its atoms,
/// a builtin's last argument, an assignment's or aggregation's result, a
/// window's variable, a reduction's fresh variables.
fn bindable(prog: &Program, head_key: Option<&str>, lit: &Literal) -> Vec<String> {
    let mut out = Vec::new();
    let atom_binds = |a: &Atom, key_fresh: bool, out: &mut Vec<String>| {
        let sig = prog.relations.get(&a.name);
        for (i, t) in a.terms.iter().enumerate() {
            let mode = sig.and_then(|s| s.args.get(i)).map(|x| x.mode);
            match mode {
                Some(Mode::Key) => {
                    if key_fresh || matches!((head_key, t), (Some(h), Term::Var(v, _)) if v == h) {
                        ctor_vars(t, out);
                    }
                }
                _ => ctor_vars(t, out),
            }
        }
    };
    match lit {
        Literal::Atom(a) => atom_binds(a, false, &mut out),
        Literal::AsOf { atom, .. } => atom_binds(atom, true, &mut out),
        Literal::Neg(_) | Literal::Cmp { .. } => {}
        Literal::Builtin(b, _) => match b {
            Builtin::Prev { t1, .. } | Builtin::Lag { t1, .. } => out.extend(var_of(t1)),
            _ => {}
        },
        Literal::Window { var, .. } => out.extend(var_of(var)),
        Literal::Assign { var, .. } | Literal::Agg { var, .. } | Literal::Case { var, .. } => out.push(var.clone()),
        Literal::Top { atom, rank, .. } => {
            for t in &atom.terms {
                ctor_vars(t, &mut out);
            }
            if let Some((k, _, _)) = rank {
                out.push(k.clone());
            }
        }
    }
    out
}

fn ctor_vars(t: &Term, out: &mut Vec<String>) {
    match t {
        Term::Var(v, _) => out.push(v.clone()),
        Term::Ctor(_, subs, _) => subs.iter().for_each(|s| ctor_vars(s, out)),
        _ => {}
    }
}

fn mentioned(lit: &Literal) -> Vec<String> {
    let mut v = Vec::new();
    crate::kernel::eval::literal_vars(lit, &mut v);
    v
}

fn expr_vars(e: &Expr) -> Vec<String> {
    let mut vs = Vec::new();
    e.vars(&mut vs);
    vs.into_iter().map(|(v, _)| v).collect()
}

/// The reads and binds of a surface literal (what the rewriter's hoisting
/// needs), under the binding model of `deps`.
pub fn literal_reads_binds(prog: &Program, rule: &Rule, lit: &Literal, bound_before: &HashSet<String>) -> (Vec<String>, Vec<String>) {
    let head_key = prog.relations.get(&rule.head.name).and_then(|s| s.key_pos()).and_then(|k| rule.head.terms.get(k)).and_then(var_of);
    deps(prog, head_key.as_deref(), lit, bound_before)
}

/// The variables bound on entry to a rule's body (the head's inputs).
pub fn initially_bound(prog: &Program, rule: &Rule) -> HashSet<String> {
    bound_before_written(prog, rule).1
}

/// The core forms of one surface literal, with fresh internal names drawn
/// from `counter` (what the rewriter re-reduces a changed literal with).
pub fn core_of(prog: &Program, lit: &Literal, bound_before: &HashSet<String>, counter: &mut usize) -> (Literal, Vec<NirLit>) {
    let mut fresh = Fresh(*counter);
    let out = reduce(prog, lit, bound_before, &mut fresh);
    *counter = fresh.0;
    out
}

/// The reads and binds of a surface literal for the canonical order: a
/// plain atom reads its inputs and its key (unless the key is the head's,
/// which the first atom to mention it binds) and may bind its outputs; a
/// grouping form (an aggregation, a reduction, a resample) reads what was
/// bound before it in the written order, since moving its binders past it
/// would change its group; tests read everything they mention.
fn deps(prog: &Program, head_key: Option<&str>, lit: &Literal, bound_before: &HashSet<String>) -> (Vec<String>, Vec<String>) {
    let mut reads: Vec<String> = Vec::new();
    let mut binds: Vec<String> = Vec::new();
    let push = |v: String, into: &mut Vec<String>| {
        if !into.contains(&v) {
            into.push(v);
        }
    };
    match lit {
        Literal::Atom(a) => {
            let sig = prog.relations.get(&a.name);
            for (i, t) in a.terms.iter().enumerate() {
                let mode = sig.and_then(|s| s.args.get(i)).map(|x| x.mode);
                let mut vs = Vec::new();
                ctor_vars(t, &mut vs);
                for v in vs {
                    match mode {
                        Some(Mode::Key) if head_key != Some(v.as_str()) => push(v, &mut reads),
                        _ => push(v, &mut binds),
                    }
                }
            }
        }
        Literal::AsOf { atom: a, at, .. } => {
            let sig = prog.relations.get(&a.name);
            for (i, t) in a.terms.iter().enumerate() {
                let mode = sig.and_then(|s| s.args.get(i)).map(|x| x.mode);
                let mut vs = Vec::new();
                ctor_vars(t, &mut vs);
                for v in vs {
                    let _ = mode;
                    push(v, &mut binds);
                }
            }
            if let Some(v) = var_of(at) {
                push(v, &mut reads);
            }
        }
        Literal::Neg(_) | Literal::Cmp { .. } => {
            for v in mentioned(lit) {
                push(v, &mut reads);
            }
        }
        Literal::Builtin(b, _) => match b {
            Builtin::Prev { t, t1 } => {
                reads.extend(var_of(t));
                binds.extend(var_of(t1));
            }
            Builtin::Lag { t, n, t1 } => {
                reads.extend(var_of(t));
                for v in expr_vars(n) {
                    push(v, &mut reads);
                }
                binds.extend(var_of(t1));
            }
            Builtin::MonthStart { t } | Builtin::DayStart { t } => reads.extend(var_of(t)),
            Builtin::SessionOf { a, t, s } => {
                reads.extend(var_of(a));
                reads.extend(var_of(t));
                binds.extend(var_of(s));
            }
        },
        Literal::Window { var, base, dur, min, lo, .. } => {
            reads.extend(var_of(base));
            for v in expr_vars(dur).into_iter().chain(expr_vars(min)).chain(lo.as_ref().map(expr_vars).unwrap_or_default()) {
                push(v, &mut reads);
            }
            binds.extend(var_of(var));
        }
        Literal::Case { var, branches, otherwise, .. } => {
            // The guards bind their own locals; everything else they mention
            // (and the values' variables) is read.
            let mut local: Vec<String> = Vec::new();
            for b in branches {
                for g in &b.guard {
                    local.extend(bindable(prog, head_key, g));
                }
            }
            for b in branches {
                for g in &b.guard {
                    for v in mentioned(g) {
                        if bound_before.contains(&v) || !local.contains(&v) {
                            push(v, &mut reads);
                        }
                    }
                }
                for v in expr_vars(&b.value) {
                    if bound_before.contains(&v) || !local.contains(&v) {
                        push(v, &mut reads);
                    }
                }
            }
            if let Some(e) = otherwise {
                for v in expr_vars(e) {
                    push(v, &mut reads);
                }
            }
            binds.push(var.clone());
        }
        Literal::Assign { var, expr: e, .. } => {
            for v in expr_vars(e) {
                push(v, &mut reads);
            }
            if bound_before.contains(var) {
                push(var.clone(), &mut reads);
            } else {
                binds.push(var.clone());
            }
        }
        Literal::Agg { .. } | Literal::Top { .. } => {
            let can = bindable(prog, head_key, lit);
            for v in mentioned(lit) {
                if bound_before.contains(&v) {
                    push(v, &mut reads);
                } else if can.contains(&v) {
                    push(v, &mut binds);
                } else if !matches!(lit, Literal::Agg { .. }) {
                    push(v, &mut reads);
                }
            }
        }
    }
    (reads, binds)
}

/// The binding sets before each written literal (the checker's model).
fn bound_before_written(prog: &Program, rule: &Rule) -> (Vec<HashSet<String>>, HashSet<String>, Option<String>) {
    let mut bound: HashSet<String> = HashSet::new();
    let mut head_key = None;
    if let Some(sig) = prog.relations.get(&rule.head.name) {
        for (t, arg) in rule.head.terms.iter().zip(sig.args.iter()) {
            if let Term::Var(v, _) = t {
                match arg.mode {
                    Mode::Key => head_key = Some(v.clone()),
                    Mode::Out => {}
                }
            }
        }
    }
    let initial = bound.clone();
    let mut out = Vec::with_capacity(rule.body.len());
    for lit in &rule.body {
        out.push(bound.clone());
        let can = bindable(prog, head_key.as_deref(), lit);
        for v in mentioned(lit) {
            if !bound.contains(&v) && can.contains(&v) {
                bound.insert(v);
            }
        }
    }
    (out, initial, head_key)
}

/// Fresh internal variables of the core forms; never a written name.
struct Fresh(usize);
impl Fresh {
    fn next(&mut self, what: &str) -> String {
        self.0 += 1;
        format!("${}{}", what, self.0)
    }
}

/// Reduce one surface literal to its core literals (2.6). The surface
/// literal returned has its aggregation group in canonical order.
fn reduce(prog: &Program, lit: &Literal, bound_before: &HashSet<String>, fresh: &mut Fresh) -> (Literal, Vec<NirLit>) {
    match lit {
        Literal::Atom(a) => (lit.clone(), vec![NirLit::Atom(atom(a))]),
        Literal::Neg(a) => (lit.clone(), vec![NirLit::Neg(atom(a))]),
        Literal::Cmp { op, lhs, rhs, .. } => (
            lit.clone(),
            vec![NirLit::Cmp {
                op: op.to_string(),
                lhs: expr(lhs),
                rhs: expr(rhs),
            }],
        ),
        Literal::Assign { var, expr: e, .. } => (
            lit.clone(),
            vec![NirLit::Assign {
                var: Some(var.clone()),
                expr: expr(e),
            }],
        ),
        Literal::Builtin(b, _) => {
            let core = match b {
                Builtin::Prev { t, t1 } => NirLit::Assign {
                    var: var_of(t1),
                    expr: NirExpr::Time("prev".into(), vec![term_expr(t)]),
                },
                Builtin::Lag { t, n, t1 } => NirLit::Assign {
                    var: var_of(t1),
                    expr: NirExpr::Time("lag".into(), vec![term_expr(t), expr(n)]),
                },
                Builtin::MonthStart { t } => NirLit::Assign {
                    var: None,
                    expr: NirExpr::Time("month_start".into(), vec![term_expr(t)]),
                },
                Builtin::DayStart { t } => NirLit::Assign {
                    var: None,
                    expr: NirExpr::Time("day_start".into(), vec![term_expr(t)]),
                },
                Builtin::SessionOf { a, t, s } => NirLit::Assign {
                    var: var_of(s),
                    expr: NirExpr::Time("session_of".into(), vec![term_expr(a), term_expr(t)]),
                },
            };
            (lit.clone(), vec![core])
        }
        Literal::Window { var, kind, base, dur, min, lo, .. } => (
            lit.clone(),
            vec![NirLit::Window {
                var: var_of(var).unwrap_or_else(|| "_".into()),
                kind: window_kind_name(kind),
                base: var_of(base).unwrap_or_else(|| "_".into()),
                dur: match (kind, lo) {
                    (WindowKind::Session, Some(l)) => NirExpr::Keys(vec![(expr(l), "lo".into()), (expr(dur), "hi".into())]),
                    _ => expr(dur),
                },
                min: expr(min),
            }],
        ),
        Literal::AsOf { atom: a, at, .. } => {
            let sig = prog.relations.get(&a.name);
            let key_pos = sig.and_then(|s| s.key_pos());
            let mut keys: Vec<NirExpr> = vec![NirExpr::Lit(a.name.clone())];
            for (i, t) in a.terms.iter().enumerate() {
                let identity = sig.and_then(|s| s.args.get(i)).map(|x| x.ty.is_entity()).unwrap_or(false);
                if identity && Some(i) != key_pos {
                    keys.push(term_expr(t));
                }
            }
            keys.push(term_expr(at));
            let key_var = key_pos.and_then(|k| a.terms.get(k)).and_then(var_of);
            (
                lit.clone(),
                vec![
                    NirLit::Assign {
                        var: key_var,
                        expr: NirExpr::Time("asof".into(), keys),
                    },
                    NirLit::Atom(atom(a)),
                ],
            )
        }
        Literal::Agg { var, agg, args, conj, within, span } => {
            let (ordered, group, anchor, min) = canonical_group(prog, conj, bound_before, fresh);
            let surface = Literal::Agg {
                var: var.clone(),
                agg: agg.clone(),
                args: args.clone(),
                conj: ordered,
                within: within.clone(),
                span: *span,
            };
            (
                surface,
                vec![NirLit::Agg {
                    var: var.clone(),
                    agg: agg.clone(),
                    args: args.iter().map(expr).collect(),
                    group,
                    by: within.clone().unwrap_or_default(),
                    min,
                    anchor,
                }],
            )
        }
        Literal::Top { n, atom: a, by, rank, .. } => {
            // `top(N, R(ū), by (ō))` → `R(ū), K = count over (R(ū'), ō(ū') <lex ō(ū)), K < N`;
            // `rank(..., as K)` → `K0 = count(...), K = K0 + 1`; averaged ties
            // take the mean of the positions a run of equal keys spans.
            let mut core = vec![NirLit::Atom(atom(a))];
            let keys: Vec<(String, Dir)> = by.as_ref().map(|b| b.iter().map(|(k, d, _)| (k.clone(), *d)).collect()).unwrap_or_default();
            let mut primes: HashMap<String, String> = HashMap::new();
            let primed_atom = NirAtom {
                name: a.name.clone(),
                terms: a
                    .terms
                    .iter()
                    .map(|t| match t {
                        Term::Var(v, _) if bound_before.contains(v) => NirTerm::Var(v.clone()),
                        Term::Var(v, _) => NirTerm::Var(primes.entry(v.clone()).or_insert_with(|| fresh.next("p")).clone()),
                        t => term(t),
                    })
                    .collect(),
            };
            let dir = |d: Dir| if d == Dir::Desc { "desc" } else { "asc" }.to_string();
            let lhs = NirExpr::Keys(keys.iter().map(|(k, d)| (NirExpr::Var(primes.get(k).cloned().unwrap_or_else(|| k.clone())), dir(*d))).collect());
            let rhs = NirExpr::Keys(keys.iter().map(|(k, d)| (NirExpr::Var(k.clone()), dir(*d))).collect());
            let before_count = |fresh: &mut Fresh, op: &str| -> (String, NirLit) {
                let k = fresh.next("k");
                (
                    k.clone(),
                    NirLit::Agg {
                        var: k,
                        agg: "count".into(),
                        args: vec![],
                        group: vec![
                            NirLit::Atom(primed_atom.clone()),
                            NirLit::Cmp {
                                op: op.into(),
                                lhs: lhs.clone(),
                                rhs: rhs.clone(),
                            },
                        ],
                        by: vec![],
                        min: None,
                        anchor: None,
                    },
                )
            };
            match (n, rank) {
                (Some(n), _) => {
                    let (k, count) = before_count(fresh, "<lex");
                    core.push(count);
                    core.push(NirLit::Cmp {
                        op: "<".into(),
                        lhs: NirExpr::Var(k),
                        rhs: expr(n),
                    });
                }
                (None, Some((k, Ties::Ordinal, _))) => {
                    let (k0, count) = before_count(fresh, "<lex");
                    core.push(count);
                    core.push(NirLit::Assign {
                        var: Some(k.clone()),
                        expr: NirExpr::Bin("+".into(), Box::new(NirExpr::Var(k0)), Box::new(NirExpr::Lit("1".into()))),
                    });
                }
                (None, Some((k, Ties::Average, _))) => {
                    let (l, below) = before_count(fresh, "<lex");
                    let (e, equal) = before_count(fresh, "=lex");
                    core.push(below);
                    core.push(equal);
                    // K = L + (E + 1) / 2
                    core.push(NirLit::Assign {
                        var: Some(k.clone()),
                        expr: NirExpr::Bin(
                            "+".into(),
                            Box::new(NirExpr::Var(l)),
                            Box::new(NirExpr::Bin(
                                "/".into(),
                                Box::new(NirExpr::Bin("+".into(), Box::new(NirExpr::Var(e)), Box::new(NirExpr::Lit("1".into())))),
                                Box::new(NirExpr::Lit("2".into())),
                            )),
                        ),
                    });
                }
                (None, None) => {}
            }
            (lit.clone(), core)
        }
        Literal::Case { var, branches, otherwise, .. } => {
            let mut bs = Vec::new();
            for b in branches {
                let mut guard = Vec::new();
                for g in &b.guard {
                    let (_, core) = reduce(prog, g, bound_before, fresh);
                    guard.extend(core);
                }
                bs.push((guard, expr(&b.value)));
            }
            (
                lit.clone(),
                vec![NirLit::Case {
                    var: var.clone(),
                    branches: bs,
                    otherwise: otherwise.as_ref().map(expr),
                }],
            )
        }
    }
}

/// A group literal being ordered: its surface and core forms, reads,
/// binds and name-free keys.
type GroupItem = (Literal, Vec<NirLit>, Vec<String>, Vec<String>, Vec<(u8, String, String)>);

/// An aggregation group in canonical order: windows first, the `rows`
/// anchor (the written group's first atom) next, then the rest by the
/// name-free key under the binding constraint.
fn canonical_group(prog: &Program, conj: &[Literal], outer: &HashSet<String>, fresh: &mut Fresh) -> (Vec<Literal>, Vec<NirLit>, Option<usize>, Option<NirExpr>) {
    let has_rows = conj.iter().any(|l| matches!(l, Literal::Window { kind: WindowKind::Rows, .. }));
    let anchor_written = if has_rows { conj.iter().position(|l| matches!(l, Literal::Atom(_))) } else { None };
    let mut items: Vec<GroupItem> = Vec::new();
    let mut bound_w = outer.clone();
    for l in conj {
        let (surface, core) = reduce(prog, l, &bound_w, fresh);
        let (reads, binds) = deps(prog, None, l, &bound_w);
        // Inside a group an atom's key is read (bound by the window) unless
        // nothing binds it, in which case the atom binds it.
        let keys = core.iter().map(key_of).collect();
        for b in &binds {
            bound_w.insert(b.clone());
        }
        items.push((surface, core, reads, binds, keys));
    }
    let n = items.len();
    let mut placed = vec![false; n];
    let mut bound = outer.clone();
    let mut order: Vec<usize> = Vec::with_capacity(n);
    let available = |i: usize, bound: &HashSet<String>, items: &[GroupItem]| -> bool { items[i].2.iter().all(|v| bound.contains(v)) };
    while order.len() < n {
        let mut best: Option<usize> = None;
        for i in 0..n {
            if placed[i] {
                continue;
            }
            // A window is available once its base is bound; an atom whose
            // key the window will bind waits for it.
            let is_window = matches!(items[i].0, Literal::Window { .. });
            let ok = available(i, &bound, &items) || (is_window && items[i].2.iter().all(|v| bound.contains(v)));
            if !ok {
                continue;
            }
            let rank = |j: usize| -> (u8, Vec<(u8, String, String)>, usize) {
                let pin = if matches!(items[j].0, Literal::Window { .. }) {
                    0
                } else if anchor_written == Some(j) {
                    1
                } else {
                    2
                };
                (pin, items[j].4.clone(), j)
            };
            if best.map(|b| rank(i) < rank(b)).unwrap_or(true) {
                best = Some(i);
            }
        }
        let Some(i) = best else {
            // Nothing available (a variable read before any binder): keep
            // the written order for what remains.
            for (i, p) in placed.iter_mut().enumerate() {
                if !*p {
                    order.push(i);
                    *p = true;
                }
            }
            break;
        };
        placed[i] = true;
        for b in &items[i].3 {
            bound.insert(b.clone());
        }
        order.push(i);
    }
    let anchor = anchor_written.and_then(|a| order.iter().position(|&i| i == a));
    let min = conj.iter().find_map(|l| if let Literal::Window { min, .. } = l { Some(expr(min)) } else { None });
    let mut surface = Vec::with_capacity(n);
    let mut core = Vec::new();
    for &i in &order {
        let (s, c, _, _, _) = items[i].clone();
        surface.push(s);
        core.extend(c);
    }
    (surface, core, anchor, min)
}

// ---------------------------------------------------------------------------
// Keys and rendering of core literals
// ---------------------------------------------------------------------------

/// A rendering of a core literal with variables through `names`; the
/// name-free key renders them by first occurrence within the literal.
/// The text of a window kind in the core forms.
pub fn window_kind_name(kind: &WindowKind) -> String {
    match kind {
        WindowKind::Window => "window".into(),
        WindowKind::Prior => "prior_window".into(),
        WindowKind::Rows => "rows".into(),
        WindowKind::PriorRows => "prior_rows".into(),
        WindowKind::Bucket(c) => format!("bucket({})", c),
        WindowKind::Session => "session".into(),
    }
}

pub fn render_core(l: &NirLit, names: &mut dyn FnMut(&str) -> String) -> String {
    fn t(x: &NirTerm, names: &mut dyn FnMut(&str) -> String) -> String {
        match x {
            NirTerm::Var(v) => names(v),
            NirTerm::Wild => "_".into(),
            NirTerm::Param(p) => format!("param {}", p),
            NirTerm::Lit(l) => l.clone(),
            NirTerm::Ctor(c, subs) => format!("{}({})", c, subs.iter().map(|s| t(s, names)).collect::<Vec<_>>().join(", ")),
        }
    }
    fn e(x: &NirExpr, names: &mut dyn FnMut(&str) -> String) -> String {
        match x {
            NirExpr::Var(v) => names(v),
            NirExpr::Param(p) => format!("param {}", p),
            NirExpr::Lit(l) => l.clone(),
            NirExpr::Neg(a) => format!("-{}", e(a, names)),
            NirExpr::Bin(op, a, b) => format!("({} {} {})", e(a, names), op, e(b, names)),
            NirExpr::Call(f, args) | NirExpr::Time(f, args) => format!("{}({})", f, args.iter().map(|a| e(a, names)).collect::<Vec<_>>().join(", ")),
            NirExpr::Keys(ks) => format!("[{}]", ks.iter().map(|(k, d)| format!("{} {}", e(k, names), d)).collect::<Vec<_>>().join(", ")),
        }
    }
    fn a(x: &NirAtom, names: &mut dyn FnMut(&str) -> String) -> String {
        format!("{}({})", x.name, x.terms.iter().map(|s| t(s, names)).collect::<Vec<_>>().join(", "))
    }
    match l {
        NirLit::Atom(x) => a(x, names),
        NirLit::Neg(x) => format!("not {}", a(x, names)),
        NirLit::Agg {
            var,
            agg,
            args,
            group,
            by,
            min,
            anchor,
        } => {
            let v = names(var);
            let g: Vec<String> = group.iter().map(|x| render_core(x, names)).collect();
            let ar: Vec<String> = args.iter().map(|x| e(x, names)).collect();
            let mut s = format!("{} = {}({}) over ({})", v, agg, ar.join(", "), g.join(", "));
            if !by.is_empty() {
                s.push_str(&format!(" by ({})", by.iter().map(|b| names(b)).collect::<Vec<_>>().join(", ")));
            }
            if let Some(m) = min {
                s.push_str(&format!(" min {}", e(m, names)));
            }
            if let Some(k) = anchor {
                s.push_str(&format!(" anchor {}", k));
            }
            s
        }
        NirLit::Assign { var, expr: x } => match var {
            Some(v) => format!("{} = {}", names(v), e(x, names)),
            None => format!("_ = {}", e(x, names)),
        },
        NirLit::Cmp { op, lhs, rhs } => format!("{} {} {}", e(lhs, names), op, e(rhs, names)),
        NirLit::Window { var, kind, base, dur, min } => format!("{} in {}({}, {}, min {})", names(var), kind, names(base), e(dur, names), e(min, names)),
        NirLit::Case { var, branches, otherwise } => {
            let mut parts: Vec<String> = branches
                .iter()
                .map(|(g, v)| format!("{} -> {}", g.iter().map(|x| render_core(x, names)).collect::<Vec<_>>().join(", "), e(v, names)))
                .collect();
            if let Some(o) = otherwise {
                parts.push(format!("else -> {}", e(o, names)));
            }
            format!("{} = case {{ {} }}", names(var), parts.join(" ; "))
        }
    }
}

fn kind_rank(l: &NirLit) -> u8 {
    match l {
        NirLit::Window { .. } => 0,
        NirLit::Atom(_) => 1,
        NirLit::Agg { .. } => 2,
        NirLit::Assign { .. } => 3,
        NirLit::Cmp { .. } => 4,
        NirLit::Neg(_) => 5,
        NirLit::Case { .. } => 3,
    }
}

fn name_of(l: &NirLit) -> String {
    fn head(e: &NirExpr) -> String {
        match e {
            NirExpr::Var(_) => "var".into(),
            NirExpr::Param(_) => "param".into(),
            NirExpr::Lit(_) => "lit".into(),
            NirExpr::Neg(_) => "neg".into(),
            NirExpr::Bin(op, _, _) => op.clone(),
            NirExpr::Call(f, _) | NirExpr::Time(f, _) => f.clone(),
            NirExpr::Keys(_) => "keys".into(),
        }
    }
    match l {
        NirLit::Atom(a) | NirLit::Neg(a) => a.name.clone(),
        NirLit::Agg { agg, group, .. } => {
            let mut rels: Vec<String> = group
                .iter()
                .filter_map(|g| match g {
                    NirLit::Atom(a) => Some(a.name.clone()),
                    _ => None,
                })
                .collect();
            rels.sort();
            format!("{}:{}", agg, rels.join(","))
        }
        NirLit::Assign { expr, .. } => head(expr),
        NirLit::Cmp { op, .. } => op.clone(),
        NirLit::Window { kind, .. } => kind.clone(),
        NirLit::Case { .. } => "case".into(),
    }
}

/// The name-free key of a core literal (2.7): kind, name, shape.
pub fn key_of(l: &NirLit) -> (u8, String, String) {
    let mut seen: Vec<String> = Vec::new();
    let mut idx = |v: &str| -> String {
        match seen.iter().position(|x| x == v) {
            Some(i) => format!("#{}", i),
            None => {
                seen.push(v.to_string());
                format!("#{}", seen.len() - 1)
            }
        }
    };
    (kind_rank(l), name_of(l), render_core(l, &mut idx))
}

// ---------------------------------------------------------------------------
// Renaming
// ---------------------------------------------------------------------------

fn rename_term(t: &Term, m: &HashMap<String, String>) -> Term {
    match t {
        Term::Var(v, s) => Term::Var(m.get(v).cloned().unwrap_or_else(|| v.clone()), *s),
        Term::Ctor(c, subs, s) => Term::Ctor(c.clone(), subs.iter().map(|x| rename_term(x, m)).collect(), *s),
        t => t.clone(),
    }
}

fn rename_expr(e: &Expr, m: &HashMap<String, String>) -> Expr {
    match e {
        Expr::Var(v, s) => Expr::Var(m.get(v).cloned().unwrap_or_else(|| v.clone()), *s),
        Expr::Neg(x, s) => Expr::Neg(Box::new(rename_expr(x, m)), *s),
        Expr::Bin(op, a, b, s) => Expr::Bin(*op, Box::new(rename_expr(a, m)), Box::new(rename_expr(b, m)), *s),
        Expr::Call(f, args, s) => Expr::Call(f.clone(), args.iter().map(|a| rename_expr(a, m)).collect(), *s),
        e => e.clone(),
    }
}

fn rename_atom(a: &Atom, m: &HashMap<String, String>) -> Atom {
    Atom {
        name: a.name.clone(),
        terms: a.terms.iter().map(|t| rename_term(t, m)).collect(),
        span: a.span,
    }
}

fn rn(v: &str, m: &HashMap<String, String>) -> String {
    m.get(v).cloned().unwrap_or_else(|| v.to_string())
}

pub fn rename_literal(l: &Literal, m: &HashMap<String, String>) -> Literal {
    match l {
        Literal::Atom(a) => Literal::Atom(rename_atom(a, m)),
        Literal::Neg(a) => Literal::Neg(rename_atom(a, m)),
        Literal::Builtin(b, s) => Literal::Builtin(
            match b {
                Builtin::Prev { t, t1 } => Builtin::Prev {
                    t: rename_term(t, m),
                    t1: rename_term(t1, m),
                },
                Builtin::Lag { t, n, t1 } => Builtin::Lag {
                    t: rename_term(t, m),
                    n: rename_expr(n, m),
                    t1: rename_term(t1, m),
                },
                Builtin::MonthStart { t } => Builtin::MonthStart { t: rename_term(t, m) },
                Builtin::DayStart { t } => Builtin::DayStart { t: rename_term(t, m) },
                Builtin::SessionOf { a, t, s } => Builtin::SessionOf {
                    a: rename_term(a, m),
                    t: rename_term(t, m),
                    s: rename_term(s, m),
                },
            },
            *s,
        ),
        Literal::Window { var, kind, base, dur, min, lo, span } => Literal::Window {
            var: rename_term(var, m),
            kind: kind.clone(),
            base: rename_term(base, m),
            dur: rename_expr(dur, m),
            min: rename_expr(min, m),
            lo: lo.as_ref().map(|x| rename_expr(x, m)),
            span: *span,
        },
        Literal::Cmp { op, lhs, rhs, span } => Literal::Cmp {
            op: *op,
            lhs: rename_expr(lhs, m),
            rhs: rename_expr(rhs, m),
            span: *span,
        },
        Literal::Assign { var, expr, span } => Literal::Assign {
            var: rn(var, m),
            expr: rename_expr(expr, m),
            span: *span,
        },
        Literal::Agg { var, agg, args, conj, within, span } => Literal::Agg {
            var: rn(var, m),
            agg: agg.clone(),
            args: args.iter().map(|a| rename_expr(a, m)).collect(),
            conj: conj.iter().map(|c| rename_literal(c, m)).collect(),
            within: within.as_ref().map(|w| w.iter().map(|v| rn(v, m)).collect()),
            span: *span,
        },
        Literal::Top { n, atom, by, rank, within, span } => Literal::Top {
            n: n.as_ref().map(|x| rename_expr(x, m)),
            atom: rename_atom(atom, m),
            by: by.as_ref().map(|b| b.iter().map(|(k, d, s)| (rn(k, m), *d, *s)).collect()),
            rank: rank.as_ref().map(|(k, t, s)| (rn(k, m), *t, *s)),
            within: within.as_ref().map(|w| w.iter().map(|v| rn(v, m)).collect()),
            span: *span,
        },
        Literal::AsOf { atom, at, within_bucket, span } => Literal::AsOf {
            atom: rename_atom(atom, m),
            at: rename_term(at, m),
            within_bucket: within_bucket.clone(),
            span: *span,
        },
        Literal::Case { var, branches, otherwise, span } => Literal::Case {
            var: rn(var, m),
            branches: branches
                .iter()
                .map(|b| Branch {
                    guard: b.guard.iter().map(|g| rename_literal(g, m)).collect(),
                    value: rename_expr(&b.value, m),
                    span: b.span,
                })
                .collect(),
            otherwise: otherwise.as_ref().map(|x| rename_expr(x, m)),
            span: *span,
        },
    }
}

fn rename_core(l: &NirLit, m: &HashMap<String, String>) -> NirLit {
    fn t(x: &NirTerm, m: &HashMap<String, String>) -> NirTerm {
        match x {
            NirTerm::Var(v) => NirTerm::Var(rn(v, m)),
            NirTerm::Ctor(c, subs) => NirTerm::Ctor(c.clone(), subs.iter().map(|s| t(s, m)).collect()),
            x => x.clone(),
        }
    }
    fn e(x: &NirExpr, m: &HashMap<String, String>) -> NirExpr {
        match x {
            NirExpr::Var(v) => NirExpr::Var(rn(v, m)),
            NirExpr::Neg(a) => NirExpr::Neg(Box::new(e(a, m))),
            NirExpr::Bin(op, a, b) => NirExpr::Bin(op.clone(), Box::new(e(a, m)), Box::new(e(b, m))),
            NirExpr::Call(f, args) => NirExpr::Call(f.clone(), args.iter().map(|a| e(a, m)).collect()),
            NirExpr::Time(f, args) => NirExpr::Time(f.clone(), args.iter().map(|a| e(a, m)).collect()),
            NirExpr::Keys(ks) => NirExpr::Keys(ks.iter().map(|(k, d)| (e(k, m), d.clone())).collect()),
            x => x.clone(),
        }
    }
    fn a(x: &NirAtom, m: &HashMap<String, String>) -> NirAtom {
        NirAtom {
            name: x.name.clone(),
            terms: x.terms.iter().map(|s| t(s, m)).collect(),
        }
    }
    match l {
        NirLit::Atom(x) => NirLit::Atom(a(x, m)),
        NirLit::Neg(x) => NirLit::Neg(a(x, m)),
        NirLit::Agg {
            var,
            agg,
            args,
            group,
            by,
            min,
            anchor,
        } => NirLit::Agg {
            var: rn(var, m),
            agg: agg.clone(),
            args: args.iter().map(|x| e(x, m)).collect(),
            group: group.iter().map(|g| rename_core(g, m)).collect(),
            by: by.iter().map(|b| rn(b, m)).collect(),
            min: min.as_ref().map(|x| e(x, m)),
            anchor: *anchor,
        },
        NirLit::Assign { var, expr } => NirLit::Assign {
            var: var.as_ref().map(|v| rn(v, m)),
            expr: e(expr, m),
        },
        NirLit::Cmp { op, lhs, rhs } => NirLit::Cmp {
            op: op.clone(),
            lhs: e(lhs, m),
            rhs: e(rhs, m),
        },
        NirLit::Window { var, kind, base, dur, min } => NirLit::Window {
            var: rn(var, m),
            kind: kind.clone(),
            base: rn(base, m),
            dur: e(dur, m),
            min: e(min, m),
        },
        NirLit::Case { var, branches, otherwise } => NirLit::Case {
            var: rn(var, m),
            branches: branches.iter().map(|(g, v)| (g.iter().map(|x| rename_core(x, m)).collect(), e(v, m))).collect(),
            otherwise: otherwise.as_ref().map(|o| e(o, m)),
        },
    }
}

/// Every variable a core literal mentions, in traversal order (2.8): the
/// result variable, the group's literals, then the arguments of an
/// aggregation; left to right otherwise.
fn core_vars(l: &NirLit, out: &mut Vec<String>) {
    fn t(x: &NirTerm, out: &mut Vec<String>) {
        match x {
            NirTerm::Var(v) => out.push(v.clone()),
            NirTerm::Ctor(_, subs) => subs.iter().for_each(|s| t(s, out)),
            _ => {}
        }
    }
    fn e(x: &NirExpr, out: &mut Vec<String>) {
        match x {
            NirExpr::Var(v) => out.push(v.clone()),
            NirExpr::Neg(a) => e(a, out),
            NirExpr::Bin(_, a, b) => {
                e(a, out);
                e(b, out);
            }
            NirExpr::Call(_, args) | NirExpr::Time(_, args) => args.iter().for_each(|a| e(a, out)),
            NirExpr::Keys(ks) => ks.iter().for_each(|(k, _)| e(k, out)),
            _ => {}
        }
    }
    match l {
        NirLit::Atom(a) | NirLit::Neg(a) => a.terms.iter().for_each(|x| t(x, out)),
        NirLit::Agg { var, args, group, by, min, .. } => {
            out.push(var.clone());
            group.iter().for_each(|g| core_vars(g, out));
            args.iter().for_each(|a| e(a, out));
            out.extend(by.iter().cloned());
            if let Some(m) = min {
                e(m, out);
            }
        }
        NirLit::Assign { var, expr } => {
            out.extend(var.iter().cloned());
            e(expr, out);
        }
        NirLit::Cmp { lhs, rhs, .. } => {
            e(lhs, out);
            e(rhs, out);
        }
        NirLit::Window { var, base, dur, min, .. } => {
            out.push(var.clone());
            out.push(base.clone());
            e(dur, out);
            e(min, out);
        }
        NirLit::Case { var, branches, otherwise } => {
            out.push(var.clone());
            for (g, v) in branches {
                g.iter().for_each(|x| core_vars(x, out));
                e(v, out);
            }
            if let Some(o) = otherwise {
                e(o, out);
            }
        }
    }
}

// ---------------------------------------------------------------------------
// The canonical order of a rule (2.7, 2.8)
// ---------------------------------------------------------------------------

struct Unit {
    written: usize,
    surface: Literal,
    core: Vec<NirLit>,
    reads: Vec<String>,
    binds: Vec<String>,
    key: Vec<(u8, String, String)>,
}

/// A canonical rule: head and body renamed and ordered.
pub struct CanonRule {
    pub head: Atom,
    pub surface: Vec<Literal>,
    pub surface_origin: Vec<usize>,
    pub core: Vec<NirBodyLit>,
    /// Canonical name to written name.
    pub names: Vec<(String, String)>,
}

fn greedy_order(units: &[Unit], initially_bound: &HashSet<String>, tie: &dyn Fn(usize, usize) -> Ordering) -> Vec<usize> {
    let n = units.len();
    let mut placed = vec![false; n];
    let mut bound = initially_bound.clone();
    let mut order = Vec::with_capacity(n);
    while order.len() < n {
        let mut best: Option<usize> = None;
        for i in 0..n {
            if placed[i] || !units[i].reads.iter().all(|v| bound.contains(v)) {
                continue;
            }
            let better = match best {
                None => true,
                Some(b) => match units[i].key.cmp(&units[b].key) {
                    Ordering::Less => true,
                    Ordering::Greater => false,
                    Ordering::Equal => tie(i, b) == Ordering::Less,
                },
            };
            if better {
                best = Some(i);
            }
        }
        let Some(i) = best else {
            for (i, p) in placed.iter_mut().enumerate() {
                if !*p {
                    order.push(i);
                    *p = true;
                }
            }
            break;
        };
        placed[i] = true;
        bound.extend(units[i].binds.iter().cloned());
        order.push(i);
    }
    order
}

/// Canonical names by first occurrence (2.8): head arguments, then the
/// units in order, each core literal's variables in traversal order.
fn naming(head: &Atom, units: &[Unit], order: &[usize]) -> HashMap<String, String> {
    let mut names: HashMap<String, String> = HashMap::new();
    let mut seen: Vec<String> = Vec::new();
    let mut name = |v: &str, names: &mut HashMap<String, String>| {
        if v == "_" || names.contains_key(v) {
            return;
        }
        let n = seen.len();
        seen.push(v.to_string());
        names.insert(v.to_string(), format!("V{}", n));
    };
    let mut hv = Vec::new();
    for t in &head.terms {
        ctor_vars(t, &mut hv);
    }
    for v in hv {
        name(&v, &mut names);
    }
    for &i in order {
        let mut vs = Vec::new();
        for c in &units[i].core {
            core_vars(c, &mut vs);
        }
        for v in vs {
            name(&v, &mut names);
        }
    }
    names
}

/// The canonical form of one rule.
pub fn canon_rule(prog: &Program, rule_idx: usize) -> CanonRule {
    let rule = &prog.rules[rule_idx];
    let (before, initial, head_key) = bound_before_written(prog, rule);
    let mut fresh = Fresh(0);
    let mut units: Vec<Unit> = Vec::with_capacity(rule.body.len());
    for (i, lit) in rule.body.iter().enumerate() {
        let (surface, core) = reduce(prog, lit, &before[i], &mut fresh);
        let (reads, binds) = deps(prog, head_key.as_deref(), lit, &before[i]);
        let key = core.iter().map(key_of).collect();
        units.push(Unit {
            written: i,
            surface,
            core,
            reads,
            binds,
            key,
        });
    }
    // The provisional order breaks ties by the written position; the
    // final order by the rendering under the canonical names, iterated to
    // a fixed point (the names depend on the order and the order on them).
    let mut order = greedy_order(&units, &initial, &|a, b| units[a].written.cmp(&units[b].written));
    let mut names = naming(&rule.head, &units, &order);
    for _ in 0..8 {
        let full = |i: usize| -> Vec<String> { units[i].core.iter().map(|c| render_core(c, &mut |v| rn(v, &names))).collect() };
        let next = greedy_order(&units, &initial, &|a, b| full(a).cmp(&full(b)).then(units[a].written.cmp(&units[b].written)));
        let next_names = naming(&rule.head, &units, &next);
        let stable = next == order;
        order = next;
        names = next_names;
        if stable {
            break;
        }
    }
    let head = rename_atom(&rule.head, &names);
    let mut surface = Vec::new();
    let mut surface_origin = Vec::new();
    let mut core = Vec::new();
    for (pos, &i) in order.iter().enumerate() {
        surface.push(rename_literal(&units[i].surface, &names));
        surface_origin.push(units[i].written);
        for c in &units[i].core {
            core.push(NirBodyLit {
                lit: rename_core(c, &names),
                unit: pos,
            });
        }
    }
    let mut name_pairs: Vec<(String, String)> = names.iter().filter(|(w, _)| !w.starts_with('$')).map(|(w, c)| (c.clone(), w.clone())).collect();
    name_pairs.sort_by_key(|(c, _)| c.trim_start_matches('V').parse::<usize>().unwrap_or(usize::MAX));
    CanonRule {
        head,
        surface,
        surface_origin,
        core,
        names: name_pairs,
    }
}

// ---------------------------------------------------------------------------
// The program
// ---------------------------------------------------------------------------

/// The label a unit carries in the hash: the strategy is `self`, so that
/// renaming it keeps its hash; libraries keep their names.
pub fn unit_label(prog: &Program, unit: &str) -> String {
    if unit == prog.strategy {
        "self".into()
    } else {
        unit.to_string()
    }
}

fn identity(v: &str) -> String {
    v.to_string()
}

/// Normalise a checked program (2.5 to 2.10).
pub fn normalise(prog: &Program) -> Nir {
    let canon: Vec<CanonRule> = (0..prog.rules.len()).map(|i| canon_rule(prog, i)).collect();
    let render = |c: &CanonRule| -> String {
        let r = Rule {
            head: c.head.clone(),
            body: c.surface.clone(),
            span: Span::default(),
            unit: String::new(),
        };
        render_rule(&r, &identity, None)
    };
    let mut idx: Vec<usize> = (0..prog.rules.len()).collect();
    idx.sort_by(|&a, &b| {
        let ra = &prog.rules[a];
        let rb = &prog.rules[b];
        unit_label(prog, &ra.unit)
            .cmp(&unit_label(prog, &rb.unit))
            .then(ra.head.name.cmp(&rb.head.name))
            .then_with(|| render(&canon[a]).cmp(&render(&canon[b])))
            .then(a.cmp(&b))
    });
    let mut counts: BTreeMap<(String, String), usize> = BTreeMap::new();
    let mut rules = Vec::with_capacity(idx.len());
    for &i in &idx {
        let r = &prog.rules[i];
        let c = &canon[i];
        let k = counts.entry((r.unit.clone(), r.head.name.clone())).or_default();
        *k += 1;
        rules.push(NirRule {
            id: format!("{}::{}#{}", r.unit, r.head.name, k),
            origin: i,
            unit: r.unit.clone(),
            head: atom(&c.head),
            body: c.core.clone(),
            surface: c.surface.clone(),
            surface_origin: c.surface_origin.clone(),
            names: c.names.clone(),
            infeasible: None,
            elided: false,
            specialised: None,
            span: r.span.to_string(),
        });
    }
    let sig = |s: &Signature| NirSig {
        name: s.name.clone(),
        unit: match &s.kind {
            Kind::Derived { unit } => Some(unit.clone()),
            Kind::Primitive { class: env } => Some(env.clone()),
            _ => None,
        },
        kind: super::print::kind_name(&s.kind).to_string(),
        signature: render_signature(s, true),
        calendar: s.cal.to_string(),
        complete: prog.complete.contains(&s.name),
    };
    let data: Vec<NirSig> = prog.relations.values().filter(|s| matches!(s.kind, Kind::Primitive { .. })).map(sig).collect();
    let relations: Vec<NirSig> = prog.relations.values().filter(|s| matches!(s.kind, Kind::Derived { .. })).map(sig).collect();
    let params: Vec<NirParam> = prog
        .params
        .iter()
        .flat_map(|(u, ps)| {
            ps.values().map(move |p| NirParam {
                unit: u.clone(),
                name: p.name.clone(),
                text: render_param(p),
            })
        })
        .collect();
    let mut nir = Nir {
        strategy: prog.strategy.clone(),
        class: prog.class.clone(),
        base: prog.base.to_string(),
        decision_calendar: prog.decision_cal.to_string(),
        clock_offset: prog.clock.offset.map(|d| d.to_string()),
        demand: serde_json::to_value(&prog.demand).unwrap_or(serde_json::Value::Null),
        silence: if prog.silence_flat { "flat".into() } else { "hold".into() },
        orders: prog.default_order.clone(),
        execution: serde_json::to_value(&prog.exec).unwrap_or(serde_json::Value::Null),
        data,
        relations,
        params,
        rules,
        hash: String::new(),
        stage: "normalised".into(),
    };
    nir.hash = program_hash(&nir);
    nir
}

/// The JSON the hash is over (2.9): the program's structure with the
/// strategy's name as `self`, no spans, no written names.
pub fn hash_view(nir: &Nir) -> serde_json::Value {
    let label = |u: &str| if u == nir.strategy { "self".to_string() } else { u.to_string() };
    let mut data: Vec<serde_json::Value> = nir.data.iter().map(|s| serde_json::json!({"name": s.name, "signature": s.signature, "complete": s.complete})).collect();
    data.sort_by_key(|v| v["name"].as_str().unwrap_or("").to_string());
    let mut relations: Vec<serde_json::Value> = nir
        .relations
        .iter()
        .map(|s| serde_json::json!({"unit": s.unit.as_deref().map(label), "name": s.name, "signature": s.signature}))
        .collect();
    relations.sort_by_key(|v| format!("{} {}", v["unit"], v["name"]));
    let mut params: Vec<serde_json::Value> = nir.params.iter().map(|p| serde_json::json!({"unit": label(&p.unit), "text": p.text})).collect();
    params.sort_by_key(|v| format!("{} {}", v["unit"], v["text"]));
    let mut rules: Vec<serde_json::Value> = nir
        .rules
        .iter()
        .map(|r| {
            serde_json::json!({
                "unit": label(&r.unit),
                "head": r.head,
                "body": r.body.iter().map(|b| serde_json::to_value(&b.lit).unwrap_or(serde_json::Value::Null)).collect::<Vec<_>>(),
            })
        })
        .collect();
    rules.sort_by_key(|v| v.to_string());
    serde_json::json!({
        "class": nir.class,
        "base": nir.base,
        "decision_calendar": nir.decision_calendar,
        "clock_offset": nir.clock_offset,
        "demand": nir.demand,
        "silence": nir.silence,
        "orders": nir.orders,
        "execution": nir.execution,
        "data": data,
        "relations": relations,
        "params": params,
        "rules": rules,
    })
}

/// The program hash (2.9): SHA-256 of the hash view's JSON.
pub fn program_hash(nir: &Nir) -> String {
    sha256_hex(hash_view(nir).to_string().as_bytes())
}

impl Nir {
    /// A rule's canonical text (surface syntax, canonical names and order).
    pub fn rule_text(&self, i: usize) -> String {
        let r = &self.rules[i];
        let rule = Rule {
            head: Atom {
                name: r.head.name.clone(),
                terms: vec![],
                span: Span::default(),
            },
            body: r.surface.clone(),
            span: Span::default(),
            unit: r.unit.clone(),
        };
        let head = format!("{}({})", r.head.name, r.head.terms.iter().map(render_nir_term).collect::<Vec<_>>().join(", "));
        let body: Vec<String> = rule.body.iter().map(|l| render_literal(l, &identity)).collect();
        format!("{} :- {}.", head, body.join(", "))
    }

    /// The program in the surface syntax with canonical names and order
    /// (re-parseable).
    pub fn source(&self, prog: &Program) -> String {
        let by_origin: HashMap<usize, usize> = self.rules.iter().enumerate().map(|(i, r)| (r.origin, i)).collect();
        let text = |origin: usize| -> String { self.rule_text(by_origin[&origin]) };
        let order: Vec<usize> = self.rules.iter().map(|r| r.origin).collect();
        SourceRendering {
            prog,
            strategy_name: &prog.strategy,
            rule_text: &text,
            rule_order: order,
        }
        .render()
    }

    /// The canonical core text of every rule, one per line.
    pub fn core_text(&self) -> String {
        let mut out = String::new();
        for r in &self.rules {
            let body: Vec<String> = r.body.iter().map(|b| render_core(&b.lit, &mut identity_mut)).collect();
            let head = format!("{}({})", r.head.name, r.head.terms.iter().map(render_nir_term).collect::<Vec<_>>().join(", "));
            let mut flags = Vec::new();
            if let Some(why) = &r.infeasible {
                flags.push(format!("infeasible: {}", why));
            }
            if r.elided {
                flags.push("elided".to_string());
            }
            if let Some(s) = &r.specialised {
                flags.push(format!("specialised at {}", s.iter().map(|(p, v)| format!("{} = {}", p, v)).collect::<Vec<_>>().join(", ")));
            }
            out.push_str(&format!(
                "{}: {} :- {}.{}\n",
                r.id,
                head,
                body.join(", "),
                if flags.is_empty() { String::new() } else { format!("  [{}]", flags.join("; ")) }
            ));
        }
        out
    }

    /// The name map: per rule, canonical name to written name.
    pub fn name_map_text(&self) -> String {
        let mut out = String::new();
        for r in &self.rules {
            let pairs: Vec<String> = r.names.iter().map(|(c, w)| format!("{} = {}", c, w)).collect();
            out.push_str(&format!("# {}: {}\n", r.id, pairs.join(", ")));
        }
        out
    }

    /// The written name of a canonical variable in a rule.
    pub fn written_name(&self, rule: usize, canonical: &str) -> String {
        self.rules[rule]
            .names
            .iter()
            .find(|(c, _)| c == canonical)
            .map(|(_, w)| w.clone())
            .unwrap_or_else(|| canonical.to_string())
    }

    /// The NIR rule for an origin rule index.
    pub fn rule_of_origin(&self, origin: usize) -> Option<usize> {
        self.rules.iter().position(|r| r.origin == origin)
    }
}

fn identity_mut(v: &str) -> String {
    v.to_string()
}

pub fn render_nir_term(t: &NirTerm) -> String {
    match t {
        NirTerm::Var(v) => v.clone(),
        NirTerm::Wild => "_".into(),
        NirTerm::Param(p) => p.clone(),
        NirTerm::Lit(l) => l.clone(),
        NirTerm::Ctor(c, subs) => format!("{}({})", c, subs.iter().map(render_nir_term).collect::<Vec<_>>().join(", ")),
    }
}

/// The NIR with the program it came from, for the printable stage.
pub struct NirStage<'a> {
    pub nir: &'a Nir,
    pub prog: &'a Program,
}

impl<'a> Stage for NirStage<'a> {
    fn text(&self) -> String {
        let mut out = self.nir.source(self.prog);
        out.push_str(&format!("\n# hash: {}\n# names (canonical = written), per rule:\n", self.nir.hash));
        out.push_str(&self.nir.name_map_text());
        out.push_str("\n# core forms:\n");
        for line in self.nir.core_text().lines() {
            out.push_str(&format!("# {}\n", line));
        }
        out
    }
    fn json(&self) -> serde_json::Value {
        let mut v = serde_json::to_value(self.nir).unwrap_or(serde_json::Value::Null);
        if let serde_json::Value::Object(m) = &mut v {
            m.insert("stage".into(), serde_json::Value::String(self.nir.stage.clone()));
            m.insert("source".into(), serde_json::Value::String(self.nir.source(self.prog)));
        }
        v
    }
}
