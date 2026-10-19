//! The obligations of tiers 2 and 3 (compiler design, sections 6.3 and
//! 6.4).
//!
//! Tier 2: for a rule, the conjunction of its comparisons and assignments
//! whose expressions are linear in the rule's variables, with the declared
//! parameter ranges as bounds and Analyse's column intervals as bounds on
//! the variables atoms bind, is tested for feasibility over the reals by
//! the exact procedure of `lp`. Infeasibility licenses R2 only beyond the
//! margin of section 6.8: the system with every comparison, assignment and
//! bound relaxed by `2^-40 · scale` must stay infeasible, where the scale
//! is the largest magnitude the system mentions.
//!
//! Tier 3: SMT-LIB2 scripts for (1) guard disjointness of the rules of a
//! relation keyed at runtime, (2) guard disjointness of `decide` rules for
//! conflict freedom, (3) the equivalence of R6's folds and of R3's
//! finalisation identities over the reals, (4) the feasibility of guards
//! with nonlinear terms or beyond tier 2's size, through QF_NRA.
//!
//! Soundness of every encoding is in one direction: each asserted
//! constraint is implied by the rule's body holding for a firing instance,
//! so an unsatisfiable script means no firing instance (for feasibility)
//! or no common instance (for disjointness). A satisfiable script means
//! nothing: the relational part can restrict instances further.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::time::Instant;

use super::lp::{self, Constraint, Feasibility, Rel, Q};
use super::smt::{numeral, symbol, Driver, Script, SolverAnswer};
use super::{Obligation, TierReport, Verdict};
use crate::analysis::{Analysis, Keyedness};
use crate::check::Program;
use crate::compiler::Compiled;
use crate::ir::{Atom, BinOp, CmpOp, Expr, Kind, Lit, Literal, Term};

pub(super) fn magnitude(l: &Lit) -> Option<f64> {
    match l {
        Lit::Int(i) => Some(*i as f64),
        Lit::Float(x) | Lit::Quantity(x, _) | Lit::Money(x, _) | Lit::Price(x, _) => Some(*x),
        _ => None,
    }
}

pub(super) fn q_of_lit(l: &Lit) -> Option<Q> {
    magnitude(l).and_then(Q::of_f64)
}

/// A literal as the rational its decimal form denotes (tier 2's constants;
/// the double rounding is below the margin).
fn decimal_of_lit(l: &Lit) -> Option<Q> {
    magnitude(l).and_then(Q::of_decimal)
}

/// A parameter as the encodings see it: a variable over its declared
/// range, a constant, or not numeric.
enum ParamVal {
    Ranged(Q, Q),
    Fixed(Q),
    Other,
}

fn param_val_with(prog: &Program, unit: &str, name: &str, of: fn(&Lit) -> Option<Q>) -> ParamVal {
    let Some(p) = prog.param(unit, name) else { return ParamVal::Other };
    match (&p.range, of(&p.value)) {
        (Some((lo, hi)), _) => match (of(lo), of(hi)) {
            (Some(l), Some(h)) => ParamVal::Ranged(l, h),
            _ => ParamVal::Other,
        },
        (None, Some(q)) => ParamVal::Fixed(q),
        _ => ParamVal::Other,
    }
}

/// A parameter with its exact double values (the SMT encodings).
fn param_val(prog: &Program, unit: &str, name: &str) -> ParamVal {
    param_val_with(prog, unit, name, q_of_lit)
}

/// A linear form `Σ coef·x + k` over named variables.
#[derive(Clone, Debug, PartialEq)]
pub struct Linear {
    pub coef: BTreeMap<String, Q>,
    pub k: Q,
}

impl Linear {
    fn constant(k: Q) -> Linear {
        Linear { coef: BTreeMap::new(), k }
    }
    fn var(name: &str) -> Linear {
        let mut coef = BTreeMap::new();
        coef.insert(name.to_string(), Q::ONE);
        Linear { coef, k: Q::ZERO }
    }
    fn is_constant(&self) -> bool {
        self.coef.is_empty()
    }
    fn add(&self, o: &Linear) -> Option<Linear> {
        let mut coef = self.coef.clone();
        for (v, c) in &o.coef {
            let e = coef.entry(v.clone()).or_insert(Q::ZERO);
            *e = e.add(*c)?;
        }
        coef.retain(|_, c| !c.is_zero());
        Some(Linear { coef, k: self.k.add(o.k)? })
    }
    fn neg(&self) -> Linear {
        Linear {
            coef: self.coef.iter().map(|(v, c)| (v.clone(), c.neg())).collect(),
            k: self.k.neg(),
        }
    }
    fn scale(&self, q: Q) -> Option<Linear> {
        let mut coef = BTreeMap::new();
        for (v, c) in &self.coef {
            let s = c.mul(q)?;
            if !s.is_zero() {
                coef.insert(v.clone(), s);
            }
        }
        Some(Linear { coef, k: self.k.mul(q)? })
    }
}

/// The linearisation of expressions in one rule.
struct Lin<'a> {
    prog: &'a Program,
    unit: &'a str,
    ranged: BTreeMap<String, (Q, Q)>,
    consts: Vec<f64>,
}

impl Lin<'_> {
    fn lin(&mut self, e: &Expr) -> Option<Linear> {
        match e {
            Expr::Var(v, _) => Some(Linear::var(v)),
            Expr::Param(p, _) => match param_val_with(self.prog, self.unit, p, decimal_of_lit) {
                ParamVal::Ranged(lo, hi) => {
                    let name = format!("param:{}", p);
                    self.consts.push(lo.to_f64());
                    self.consts.push(hi.to_f64());
                    self.ranged.insert(name.clone(), (lo, hi));
                    Some(Linear::var(&name))
                }
                ParamVal::Fixed(q) => {
                    self.consts.push(q.to_f64());
                    Some(Linear::constant(q))
                }
                ParamVal::Other => None,
            },
            Expr::Lit(l, _) => {
                let q = decimal_of_lit(l)?;
                self.consts.push(q.to_f64());
                Some(Linear::constant(q))
            }
            Expr::Neg(x, _) => self.lin(x).map(|l| l.neg()),
            Expr::Bin(op, a, b, _) => {
                let (la, lb) = (self.lin(a)?, self.lin(b)?);
                match op {
                    BinOp::Add => la.add(&lb),
                    BinOp::Sub => la.add(&lb.neg()),
                    BinOp::Mul => {
                        if la.is_constant() {
                            lb.scale(la.k)
                        } else if lb.is_constant() {
                            la.scale(lb.k)
                        } else {
                            None
                        }
                    }
                    BinOp::Div => {
                        if lb.is_constant() && !lb.k.is_zero() {
                            la.scale(Q::ONE.div(lb.k)?)
                        } else {
                            None
                        }
                    }
                }
            }
            Expr::Call(..) => None,
        }
    }
}

/// One constraint `lin (rel) 0` of a rule's system.
#[derive(Clone, Debug)]
pub struct Item {
    pub lin: Linear,
    pub rel: Rel,
    /// Loosened by the margin in the robustness check (comparisons,
    /// assignments and Analyse's bounds; never a declared parameter range).
    pub relaxable: bool,
    pub text: String,
}

/// A rule's linear system (section 6.3).
#[derive(Clone, Debug)]
pub struct LinearSystem {
    pub items: Vec<Item>,
    /// Parameter variables with their declared ranges.
    pub ranged: BTreeMap<String, (Q, Q)>,
    /// Comparisons and assignments that are not linear (tier 3's).
    pub nonlinear: Vec<String>,
    /// Comparisons and assignments the system holds.
    pub comparisons: usize,
    /// Variables Analyse bounded.
    pub bounded: Vec<String>,
    /// The largest magnitude the system mentions, at least 1.
    pub scale: f64,
}

impl LinearSystem {
    pub fn is_empty(&self) -> bool {
        self.comparisons == 0
    }
    /// The statement: the comparisons, the ranges, the bounded variables.
    pub fn statement(&self) -> String {
        let mut parts: Vec<String> = self.items.iter().filter(|i| i.relaxable && !i.text.contains('∈')).map(|i| format!("`{}`", i.text)).collect();
        for (p, (lo, hi)) in &self.ranged {
            parts.push(format!("{} ≤ {} ≤ {}", lo.decimal_text(), p.trim_start_matches("param:"), hi.decimal_text()));
        }
        if !self.bounded.is_empty() {
            parts.push(format!("Analyse's bounds on {}", self.bounded.join(", ")));
        }
        let mut s = parts.join(" ∧ ");
        if s.chars().count() > 240 {
            s = s.chars().take(237).collect::<String>() + "…";
        }
        s
    }
}

/// A bound as a short rational on its loose side (down for a lower bound,
/// up for an upper one), so that the exact arithmetic stays in range; a
/// looser bound keeps every verdict sound.
fn loose(x: f64, up: bool) -> Option<Q> {
    let q = Q::of_f64(x)?;
    let digits = 9 - x.abs().log10().floor().clamp(-9.0, 9.0) as i32;
    let scale = 10i128.checked_pow(digits.max(0) as u32)?;
    q.round_to(scale, up)
}

fn push_bounds(items: &mut Vec<Item>, v: &str, interval: crate::analysis::domains::Interval, scale: &mut f64, bounded: &mut Vec<String>) {
    if interval.is_bottom() || interval.is_top() {
        return;
    }
    let mut any = false;
    if interval.lo.is_finite() {
        if let Some(lo) = loose(interval.lo, false) {
            if let Some(lin) = Linear::constant(lo).add(&Linear::var(v).neg()) {
                items.push(Item {
                    lin,
                    rel: Rel::Le,
                    relaxable: true,
                    text: format!("{} ∈ [{}, ·)", v, interval.lo),
                });
                *scale = scale.max(interval.lo.abs());
                any = true;
            }
        }
    }
    if interval.hi.is_finite() {
        if let Some(hi) = loose(interval.hi, true) {
            if let Some(lin) = Linear::var(v).add(&Linear::constant(hi).neg()) {
                items.push(Item {
                    lin,
                    rel: Rel::Le,
                    relaxable: true,
                    text: format!("{} ∈ (·, {}]", v, interval.hi),
                });
                *scale = scale.max(interval.hi.abs());
                any = true;
            }
        }
    }
    if any && !bounded.iter().any(|b| b == v) {
        bounded.push(v.to_string());
    }
}

/// The linear system of a rule: its linear comparisons and assignments,
/// the declared ranges of the parameters they mention, and Analyse's
/// finite bounds on the variables its atoms and aggregations bind.
pub fn linear_system(prog: &Program, analysis: &Analysis, ri: usize) -> LinearSystem {
    let rule = &prog.rules[ri];
    let mut b = Lin {
        prog,
        unit: &rule.unit,
        ranged: BTreeMap::new(),
        consts: Vec::new(),
    };
    let mut items: Vec<Item> = Vec::new();
    let mut nonlinear = Vec::new();
    let mut comparisons = 0;
    let mut scale = 1.0f64;
    let mut bounded = Vec::new();
    let facts = analysis.rules.iter().find(|r| r.rule == ri);
    for (li, l) in rule.body.iter().enumerate() {
        match l {
            Literal::Cmp { op, lhs, rhs, .. } => match (b.lin(lhs), b.lin(rhs)) {
                (Some(a), Some(c)) => match a.add(&c.neg()) {
                    Some(d) => {
                        comparisons += 1;
                        let (lin, rel) = match op {
                            CmpOp::Lt => (d, Rel::Lt),
                            CmpOp::Le => (d, Rel::Le),
                            CmpOp::Gt => (d.neg(), Rel::Lt),
                            CmpOp::Ge => (d.neg(), Rel::Le),
                            CmpOp::Eq => (d, Rel::Eq),
                        };
                        items.push(Item {
                            lin,
                            rel,
                            relaxable: true,
                            text: l.describe(),
                        });
                    }
                    None => nonlinear.push(l.describe()),
                },
                _ => nonlinear.push(l.describe()),
            },
            Literal::Assign { var, expr, .. } => match b.lin(expr) {
                Some(e) => {
                    if let Some(d) = Linear::var(var).add(&e.neg()) {
                        comparisons += 1;
                        items.push(Item {
                            lin: d,
                            rel: Rel::Eq,
                            relaxable: true,
                            text: l.describe(),
                        });
                    }
                }
                None => nonlinear.push(l.describe()),
            },
            Literal::Atom(a) => {
                if let Some(rf) = analysis.relation(&a.name) {
                    for (i, t) in a.terms.iter().enumerate() {
                        if let (Term::Var(v, _), Some(col)) = (t, rf.columns.get(i)) {
                            if col.value.numeric {
                                push_bounds(&mut items, v, col.value.interval, &mut scale, &mut bounded);
                            }
                        }
                    }
                }
            }
            Literal::Agg { var, .. } => {
                if let Some(st) = facts.and_then(|f| f.literals.iter().find(|x| x.index == li)) {
                    if let Some((_, val)) = st.state.iter().find(|(n, _)| n == var) {
                        if val.numeric {
                            push_bounds(&mut items, var, val.interval, &mut scale, &mut bounded);
                        }
                    }
                }
            }
            _ => {}
        }
    }
    for c in &b.consts {
        if c.is_finite() {
            scale = scale.max(c.abs());
        }
    }
    LinearSystem {
        items,
        ranged: b.ranged,
        nonlinear,
        comparisons,
        bounded,
        scale,
    }
}

/// The variables and constraints of a system, with every relaxable
/// constraint loosened by `relax` when given.
pub fn constraints(sys: &LinearSystem, relax: Option<Q>) -> (Vec<String>, Vec<Constraint>) {
    let mut names: BTreeSet<String> = BTreeSet::new();
    for it in &sys.items {
        names.extend(it.lin.coef.keys().cloned());
    }
    names.extend(sys.ranged.keys().cloned());
    let vars: Vec<String> = names.into_iter().collect();
    let index: HashMap<&str, usize> = vars.iter().enumerate().map(|(i, v)| (v.as_str(), i)).collect();
    let row = |lin: &Linear| -> Vec<Q> {
        let mut r = vec![Q::ZERO; vars.len()];
        for (v, c) in &lin.coef {
            r[index[v.as_str()]] = *c;
        }
        r
    };
    let mut out = Vec::new();
    for it in &sys.items {
        let mu = if it.relaxable { relax } else { None };
        let rhs = it.lin.k.neg();
        match it.rel {
            Rel::Eq => match mu {
                Some(m) => {
                    out.push(Constraint {
                        coef: row(&it.lin),
                        rel: Rel::Le,
                        rhs: rhs.add(m).unwrap_or(rhs),
                    });
                    out.push(Constraint {
                        coef: row(&it.lin.neg()),
                        rel: Rel::Le,
                        rhs: rhs.neg().add(m).unwrap_or(rhs.neg()),
                    });
                }
                None => out.push(Constraint {
                    coef: row(&it.lin),
                    rel: Rel::Eq,
                    rhs,
                }),
            },
            rel => out.push(Constraint {
                coef: row(&it.lin),
                rel,
                rhs: match mu {
                    Some(m) => rhs.add(m).unwrap_or(rhs),
                    None => rhs,
                },
            }),
        }
    }
    for (p, (lo, hi)) in &sys.ranged {
        let i = index[p.as_str()];
        let mut lo_row = vec![Q::ZERO; vars.len()];
        lo_row[i] = Q::int(-1);
        out.push(Constraint {
            coef: lo_row,
            rel: Rel::Le,
            rhs: lo.neg(),
        });
        let mut hi_row = vec![Q::ZERO; vars.len()];
        hi_row[i] = Q::ONE;
        out.push(Constraint { coef: hi_row, rel: Rel::Le, rhs: *hi });
    }
    (vars, out)
}

/// The margin of section 6.8 as a rational: `2^margin_log2 · scale`.
pub fn margin(scale: f64, margin_log2: i32) -> Option<Q> {
    let m = scale.max(1.0) * 2f64.powi(margin_log2);
    Q::of_f64(m).filter(|q| q.is_positive())
}

fn short(x: f64) -> String {
    if x == 0.0 {
        "0".into()
    } else if (1e-4..1e9).contains(&x.abs()) {
        format!("{}", x)
    } else {
        format!("{:.3e}", x)
    }
}

/// Tier 2 over every rule of the program.
pub fn tier2(prog: &Program, analysis: &Analysis, margin_log2: i32) -> TierReport {
    let mut t = TierReport::new(2);
    for ri in 0..prog.rules.len() {
        let sys = linear_system(prog, analysis, ri);
        if sys.is_empty() {
            continue;
        }
        let (vars, plain) = constraints(&sys, None);
        let mut o = Obligation::new(2, format!("t2-r{}", ri + 1), "rule feasibility", sys.statement()).on_rule(prog, ri);
        o.logic = Some(format!(
            "{} variable(s), {} constraint(s){}",
            vars.len(),
            plain.len(),
            if sys.nonlinear.is_empty() {
                String::new()
            } else {
                format!("; {} nonlinear left to tier 3", sys.nonlinear.len())
            }
        ));
        let start = Instant::now();
        o = match lp::feasible(vars.len(), &plain) {
            Feasibility::Feasible => o.with(Verdict::Feasible, "the linear part admits a real solution"),
            Feasibility::Undecided => o.with(
                Verdict::Undecided,
                format!(
                    "beyond {} constraints or {} variables, or the arithmetic's range; tier 3 attempts it",
                    lp::MAX_CONSTRAINTS,
                    lp::MAX_VARIABLES
                ),
            ),
            Feasibility::Infeasible => match margin(sys.scale, margin_log2) {
                None => o.with(Verdict::Infeasible, "infeasible over the reals; the margin is out of range: kept"),
                Some(mu) => {
                    let (_, relaxed) = constraints(&sys, Some(mu));
                    match lp::feasible(vars.len(), &relaxed) {
                        Feasibility::Infeasible => o
                            .with(
                                Verdict::Infeasible,
                                format!(
                                    "infeasible over the reals with every comparison relaxed by {} (2^{} · scale {}): the rule never fires",
                                    short(mu.to_f64()),
                                    margin_log2,
                                    short(sys.scale)
                                ),
                            )
                            .licensing("R2"),
                        Feasibility::Feasible => o.with(
                            Verdict::Infeasible,
                            format!("infeasible over the reals but feasible within the rounding margin {}: kept", short(mu.to_f64())),
                        ),
                        Feasibility::Undecided => o.with(Verdict::Infeasible, "infeasible over the reals; the margin check is undecided: kept"),
                    }
                }
            },
        };
        o.elapsed_ms = start.elapsed().as_millis() as u64;
        t.obligations.push(o);
    }
    if t.obligations.is_empty() {
        t.notes.push("no rule carries a linear comparison".into());
    }
    t
}

// ---------------------------------------------------------------------
// SMT encodings (tier 3)
// ---------------------------------------------------------------------

/// The encoding of one rule's terms into a script: variables under a
/// prefix, parameters as ranged variables or numerals, partial functions
/// with their domain conditions.
pub(super) struct Smt<'a> {
    prog: &'a Program,
    unit: String,
    prefix: String,
    fresh: usize,
    pub(super) nonlinear: bool,
    consts: Vec<f64>,
    /// Every parameter at its default (for R6's folds, which evaluated it).
    params_fixed: bool,
    /// Clause mode (tier 4): variables are bound by the clause's quantifier
    /// rather than declared, with their sorts here, and side conditions
    /// are collected in `pending` for the clause's body.
    pub(super) quantified: Option<BTreeMap<String, String>>,
    pub(super) pending: Vec<String>,
}

impl<'a> Smt<'a> {
    pub(super) fn new(prog: &'a Program, unit: &str, prefix: &str) -> Smt<'a> {
        Smt {
            prog,
            unit: unit.to_string(),
            prefix: prefix.to_string(),
            fresh: 0,
            nonlinear: false,
            consts: Vec::new(),
            params_fixed: false,
            quantified: None,
            pending: Vec::new(),
        }
    }
    /// The name of a rule variable in this encoding.
    pub(super) fn name_of(&self, v: &str) -> String {
        format!("{}{}", self.prefix, symbol(v))
    }
    fn bind(&mut self, n: &str, sort: &str, s: &mut Script) {
        match &mut self.quantified {
            Some(q) => {
                q.entry(n.to_string()).or_insert_with(|| sort.to_string());
            }
            None => s.declare(n, sort),
        }
    }
    fn side(&mut self, s: &mut Script, cond: String) {
        if self.quantified.is_some() {
            self.pending.push(cond);
        } else {
            s.assert(cond);
        }
    }
    /// Bind a rule variable with a sort other than Real (a bar index).
    pub(super) fn bind_sort(&mut self, v: &str, sort: &str, s: &mut Script) -> String {
        let n = self.name_of(v);
        if let Some(q) = &mut self.quantified {
            q.insert(n.clone(), sort.to_string());
        } else {
            s.declare(&n, sort);
        }
        n
    }
    pub(super) fn var(&mut self, v: &str, s: &mut Script) -> String {
        let n = self.name_of(v);
        self.bind(&n, "Real", s);
        n
    }
    pub(super) fn fresh(&mut self, what: &str, s: &mut Script) -> String {
        self.fresh += 1;
        let n = format!("{}{}_{}", self.prefix, symbol(what), self.fresh);
        self.bind(&n, "Real", s);
        n
    }
    pub(super) fn param(&mut self, p: &str, s: &mut Script) -> Option<String> {
        let unit = self.unit.clone();
        if self.params_fixed {
            let q = q_of_lit(&self.prog.param(&unit, p)?.value)?;
            self.consts.push(q.to_f64());
            return Some(numeral(q));
        }
        match param_val(self.prog, &unit, p) {
            ParamVal::Ranged(lo, hi) => {
                let n = format!("{}p_{}", if self.quantified.is_some() { self.prefix.as_str() } else { "" }, symbol(&format!("{}__{}", unit, p)));
                let fresh_here = match &self.quantified {
                    Some(q) => !q.contains_key(&n),
                    None => !s.declared(&n),
                };
                if fresh_here {
                    self.bind(&n, "Real", s);
                    self.side(s, format!("(>= {} {})", n, numeral(lo)));
                    self.side(s, format!("(<= {} {})", n, numeral(hi)));
                }
                self.consts.push(lo.to_f64());
                self.consts.push(hi.to_f64());
                Some(n)
            }
            ParamVal::Fixed(q) => {
                self.consts.push(q.to_f64());
                Some(numeral(q))
            }
            ParamVal::Other => None,
        }
    }
    /// An entity, label or string literal as an uninterpreted real
    /// constant (distinct constants are asserted distinct by the caller).
    fn entity(&mut self, name: &str, s: &mut Script, entities: &mut BTreeSet<String>) -> String {
        let n = format!("ent_{}", symbol(name));
        self.bind(&n, "Real", s);
        entities.insert(n.clone());
        n
    }
    /// A term of a head or an atom: a variable, a numeral, an entity
    /// constant; `None` for a wildcard, a duration or a constructor.
    pub(super) fn head_term(&mut self, t: &Term, s: &mut Script, entities: &mut BTreeSet<String>) -> Option<String> {
        match t {
            Term::Var(v, _) => Some(self.var(v, s)),
            Term::Wild(_) | Term::Ctor(..) => None,
            Term::Lit(l, _) => match l {
                Lit::Str(x) | Lit::Entity(x) | Lit::Label(x) => Some(self.entity(x, s, entities)),
                _ => q_of_lit(l).map(numeral),
            },
            Term::Param(p, _) => {
                let unit = self.unit.clone();
                match self.prog.param(&unit, p).map(|p| p.value.clone()) {
                    Some(Lit::Str(x) | Lit::Entity(x) | Lit::Label(x)) => Some(self.entity(&x, s, entities)),
                    _ => self.param(p, s),
                }
            }
        }
    }
    pub(super) fn term(&mut self, e: &Expr, s: &mut Script) -> Option<String> {
        match e {
            Expr::Var(v, _) => Some(self.var(v, s)),
            Expr::Param(p, _) => self.param(p, s),
            Expr::Lit(l, _) => {
                let q = q_of_lit(l)?;
                self.consts.push(q.to_f64());
                Some(numeral(q))
            }
            Expr::Neg(x, _) => Some(format!("(- {})", self.term(x, s)?)),
            Expr::Bin(op, a, b, _) => {
                let (ta, tb) = (self.term(a, s)?, self.term(b, s)?);
                match op {
                    BinOp::Add => Some(format!("(+ {} {})", ta, tb)),
                    BinOp::Sub => Some(format!("(- {} {})", ta, tb)),
                    BinOp::Mul => {
                        if self.unknown(a) && self.unknown(b) {
                            self.nonlinear = true;
                        }
                        Some(format!("(* {} {})", ta, tb))
                    }
                    BinOp::Div => {
                        if self.unknown(b) {
                            self.nonlinear = true;
                            self.side(s, format!("(not (= {} 0.0))", tb));
                        }
                        Some(format!("(/ {} {})", ta, tb))
                    }
                }
            }
            Expr::Call(f, args, _) => {
                let ts: Vec<String> = args.iter().map(|a| self.term(a, s)).collect::<Option<_>>()?;
                match (f.as_str(), ts.as_slice()) {
                    ("abs", [x]) => Some(format!("(ite (>= {x} 0.0) {x} (- {x}))")),
                    ("least", xs) if !xs.is_empty() => {
                        let mut acc = xs[0].clone();
                        for x in &xs[1..] {
                            acc = format!("(ite (<= {acc} {x}) {acc} {x})");
                        }
                        Some(acc)
                    }
                    ("greatest", xs) if !xs.is_empty() => {
                        let mut acc = xs[0].clone();
                        for x in &xs[1..] {
                            acc = format!("(ite (>= {acc} {x}) {acc} {x})");
                        }
                        Some(acc)
                    }
                    ("sqrt", [x]) => {
                        let r = self.fresh("sqrt", s);
                        self.side(s, format!("(>= {} 0.0)", x));
                        self.side(s, format!("(>= {} 0.0)", r));
                        if self.quantified.is_some() {
                            // A clause stays linear: the root is a non-negative
                            // unknown (sound: a looser body).
                        } else {
                            self.side(s, format!("(= (* {r} {r}) {x})"));
                            self.nonlinear = true;
                        }
                        Some(r)
                    }
                    ("log", [x]) => {
                        let r = self.fresh("log", s);
                        self.side(s, format!("(> {} 0.0)", x));
                        Some(r)
                    }
                    ("exp", [_]) => {
                        let r = self.fresh("exp", s);
                        self.side(s, format!("(> {} 0.0)", r));
                        Some(r)
                    }
                    _ => Some(self.fresh(f, s)),
                }
            }
        }
    }

    /// Assert a rule's arithmetic part: comparisons, assignments, the
    /// parameter ranges its expressions mention, Analyse's bounds on the
    /// variables its atoms and aggregations bind. With `relax`, every
    /// comparison, assignment and bound is loosened by it. Returns how
    /// many literals were asserted.
    pub(super) fn guard(&mut self, s: &mut Script, analysis: &Analysis, ri: usize, relax: Option<Q>) -> usize {
        let rule = &self.prog.rules[ri];
        let mu = relax.map(numeral);
        let mut n = 0;
        let facts = analysis.rules.iter().find(|r| r.rule == ri);
        for (li, l) in rule.body.iter().enumerate() {
            match l {
                Literal::Cmp { op, lhs, rhs, .. } => {
                    if let (Some(a), Some(b)) = (self.term(lhs, s), self.term(rhs, s)) {
                        n += 1;
                        let c = cmp(*op, &a, &b, mu.as_deref());
                        self.side(s, c);
                    }
                }
                Literal::Assign { var, expr, .. } => {
                    if let Some(e) = self.term(expr, s) {
                        let v = self.var(var, s);
                        n += 1;
                        let c = cmp(CmpOp::Eq, &v, &e, mu.as_deref());
                        self.side(s, c);
                    }
                }
                Literal::Atom(a) => {
                    if let Some(rf) = analysis.relation(&a.name) {
                        for (i, t) in a.terms.iter().enumerate() {
                            if let (Term::Var(v, _), Some(col)) = (t, rf.columns.get(i)) {
                                if col.value.numeric {
                                    let name = self.var(v, s);
                                    for c in bounds(&name, col.value.interval, mu.as_deref(), &mut self.consts) {
                                        self.side(s, c);
                                    }
                                }
                            }
                        }
                    }
                }
                Literal::Agg { var, .. } => {
                    if let Some(st) = facts.and_then(|f| f.literals.iter().find(|x| x.index == li)) {
                        if let Some((_, val)) = st.state.iter().find(|(n, _)| n == var) {
                            if val.numeric {
                                let name = self.var(var, s);
                                for c in bounds(&name, val.interval, mu.as_deref(), &mut self.consts) {
                                    self.side(s, c);
                                }
                            }
                        }
                    }
                }
                _ => {}
            }
        }
        n
    }
    /// Whether an expression's value is not fixed by the encoding: it
    /// mentions a variable, a ranged parameter (a variable too), or a
    /// function the encoding leaves uninterpreted.
    fn unknown(&self, e: &Expr) -> bool {
        match e {
            Expr::Var(..) => true,
            Expr::Param(p, _) => !self.params_fixed && matches!(param_val(self.prog, &self.unit, p), ParamVal::Ranged(..)),
            Expr::Lit(..) => false,
            Expr::Neg(x, _) => self.unknown(x),
            Expr::Bin(_, a, b, _) => self.unknown(a) || self.unknown(b),
            Expr::Call(..) => true,
        }
    }
    fn scale(&self) -> f64 {
        self.consts.iter().filter(|c| c.is_finite()).fold(1.0f64, |m, c| m.max(c.abs()))
    }
}

/// `a op b`, or with a relaxation `μ`: `a − b` within `μ` of the relation.
pub(super) fn cmp(op: CmpOp, a: &str, b: &str, mu: Option<&str>) -> String {
    match mu {
        None => match op {
            CmpOp::Lt => format!("(< {} {})", a, b),
            CmpOp::Le => format!("(<= {} {})", a, b),
            CmpOp::Gt => format!("(> {} {})", a, b),
            CmpOp::Ge => format!("(>= {} {})", a, b),
            CmpOp::Eq => format!("(= {} {})", a, b),
        },
        Some(m) => {
            let d = format!("(- {} {})", a, b);
            match op {
                CmpOp::Lt => format!("(< {} {})", d, m),
                CmpOp::Le => format!("(<= {} {})", d, m),
                CmpOp::Gt => format!("(> {} (- {}))", d, m),
                CmpOp::Ge => format!("(>= {} (- {}))", d, m),
                CmpOp::Eq => format!("(and (<= {d} {m}) (>= {d} (- {m})))"),
            }
        }
    }
}

/// Analyse's finite bounds on a variable as conjuncts, loosened by `mu`
/// when given.
pub(super) fn bounds(name: &str, interval: crate::analysis::domains::Interval, mu: Option<&str>, consts: &mut Vec<f64>) -> Vec<String> {
    let mut out = Vec::new();
    if interval.is_bottom() || interval.is_top() {
        return out;
    }
    if interval.lo.is_finite() {
        if let Some(lo) = Q::of_f64(interval.lo) {
            consts.push(interval.lo);
            out.push(match mu {
                Some(m) => format!("(>= {} (- {} {}))", name, numeral(lo), m),
                None => format!("(>= {} {})", name, numeral(lo)),
            });
        }
    }
    if interval.hi.is_finite() {
        if let Some(hi) = Q::of_f64(interval.hi) {
            consts.push(interval.hi);
            out.push(match mu {
                Some(m) => format!("(<= {} (+ {} {}))", name, numeral(hi), m),
                None => format!("(<= {} {})", name, numeral(hi)),
            });
        }
    }
    out
}

fn verdict_of(answer: &SolverAnswer, unsat: (Verdict, &str), sat: (Verdict, &str)) -> (Verdict, String) {
    match answer {
        SolverAnswer::Unsat => (unsat.0, unsat.1.to_string()),
        SolverAnswer::Sat => (sat.0, sat.1.to_string()),
        other => (Verdict::Undecided, other.text()),
    }
}

/// Fill an obligation from a solver run.
fn settle(mut o: Obligation, driver: &Driver, script: &Script, unsat: (Verdict, &str), sat: (Verdict, &str)) -> Obligation {
    let out = driver.run(&o.id, script);
    let (v, e) = verdict_of(&out.answer, unsat, sat);
    o.verdict = v;
    o.evidence = e;
    o.logic = Some(script.logic.clone());
    o.budget_ms = driver.budget_ms;
    o.elapsed_ms = out.elapsed_ms;
    o.replay_file = out.replay_file;
    o
}

/// Obligation 4: the feasibility of every rule tier 2 did not settle and
/// whose guards carry a nonlinear term (or exceed tier 2's size), through
/// QF_NRA with the margin relaxation; `unsat` licenses R2.
pub fn tier3_feasibility(prog: &Program, analysis: &Analysis, driver: &Driver, margin_log2: i32, tier2: &TierReport) -> TierReport {
    let mut t = TierReport::new(3);
    for ri in 0..prog.rules.len() {
        let sys = linear_system(prog, analysis, ri);
        let settled = tier2.obligations.iter().find(|o| o.rule_index == Some(ri));
        let decided = settled.map(|o| o.verdict == Verdict::Infeasible && o.licenses.is_some()).unwrap_or(false);
        let undecided = settled.map(|o| o.verdict == Verdict::Undecided).unwrap_or(false);
        let rule = &prog.rules[ri];
        let has_comparison = rule.body.iter().any(|l| matches!(l, Literal::Cmp { .. }));
        if decided || !has_comparison || (sys.nonlinear.is_empty() && !undecided) {
            continue;
        }
        let mut script = Script::new("QF_LRA");
        let mut ctx = Smt::new(prog, &rule.unit, "r_");
        // A first pass finds the scale; the second asserts with the margin.
        let mut probe = Script::new("QF_LRA");
        let n = ctx.guard(&mut probe, analysis, ri, None);
        if n == 0 {
            continue;
        }
        let scale = ctx.scale().max(sys.scale);
        let mu = margin(scale, margin_log2);
        let mut ctx = Smt::new(prog, &rule.unit, "r_");
        ctx.guard(&mut script, analysis, ri, mu);
        if ctx.nonlinear {
            script.logic = "QF_NRA".into();
        }
        let id = format!("t3-o4-r{}", ri + 1);
        let statement = format!(
            "the guards of {} admit no instance ({} with the margin 2^{} · {}){}",
            prog.rule_label(ri),
            sys.nonlinear.iter().take(3).map(|x| format!("`{}`", x)).collect::<Vec<_>>().join(", "),
            margin_log2,
            short(scale),
            if undecided { "; beyond tier 2's size" } else { "" }
        );
        script.comment(format!("obligation {}: {}", id, statement));
        script.comment("unsat: the rule never fires (licenses R2); sat: feasible, or within the rounding margin");
        let o = Obligation::new(3, id, "rule feasibility", statement).on_rule(prog, ri);
        let mut o = settle(
            o,
            driver,
            &script,
            (Verdict::Infeasible, "unsat with every comparison relaxed by the margin: the rule never fires"),
            (Verdict::Feasible, "sat: the guards admit an instance, or one within the rounding margin"),
        );
        if o.verdict == Verdict::Infeasible {
            o = o.licensing("R2");
        }
        t.obligations.push(o);
    }
    if t.obligations.is_empty() {
        t.notes.push("no rule left to tier 3 by tier 2 (no nonlinear guard)".into());
    }
    t
}

/// Whether a relation holds at most one tuple per identity and bar: a
/// stored relation by the loader; a derived one by its keyedness label,
/// or with one rule whose positive atoms are all functional.
fn functional(prog: &Program, analysis: &Analysis, rel: &str, memo: &mut HashMap<String, bool>, visiting: &mut HashSet<String>) -> bool {
    if let Some(&b) = memo.get(rel) {
        return b;
    }
    let Some(sig) = prog.relations.get(rel) else { return false };
    let out = match &sig.kind {
        Kind::Output => false,
        Kind::Derived { .. } => match analysis.relation(rel).map(|f| f.keyedness) {
            Some(Keyedness::ByConstruction) | Some(Keyedness::ByGuard) => true,
            Some(Keyedness::AtRuntime) => false,
            _ => {
                let rules = prog.rules_for(rel);
                let ids = sig.identity_positions();
                let has_values = (0..sig.args.len()).any(|i| !ids.contains(&i));
                if !has_values {
                    true
                } else if rules.len() != 1 {
                    false
                } else if !visiting.insert(rel.to_string()) {
                    true
                } else {
                    let mut ok = true;
                    for l in &prog.rules[rules[0]].body {
                        if let Literal::Atom(a) = l {
                            if !functional(prog, analysis, &a.name, memo, visiting) {
                                ok = false;
                                break;
                            }
                        }
                    }
                    visiting.remove(rel);
                    ok
                }
            }
        },
        _ => true,
    };
    memo.insert(rel.to_string(), out);
    out
}

/// A union-find over term encodings, so that equalities derived from the
/// head alignment and from functional dependencies compose.
#[derive(Default)]
struct Classes {
    parent: HashMap<String, String>,
}

impl Classes {
    fn find(&mut self, x: &str) -> String {
        let p = self.parent.get(x).cloned().unwrap_or_else(|| x.to_string());
        if p == x {
            return p;
        }
        let r = self.find(&p);
        self.parent.insert(x.to_string(), r.clone());
        r
    }
    /// Union; `true` when the classes were distinct.
    fn union(&mut self, x: &str, y: &str) -> bool {
        let (rx, ry) = (self.find(x), self.find(y));
        if rx == ry {
            return false;
        }
        self.parent.insert(rx, ry);
        true
    }
}

/// The positive top-level atoms of a rule.
fn atoms(rule: &crate::ir::Rule) -> Vec<&Atom> {
    rule.body
        .iter()
        .filter_map(|l| match l {
            Literal::Atom(a) => Some(a),
            _ => None,
        })
        .collect()
}

/// The comparison literals of a rule, for a statement.
fn guard_text(rule: &crate::ir::Rule) -> String {
    let cs: Vec<String> = rule.body.iter().filter(|l| matches!(l, Literal::Cmp { .. } | Literal::Neg(_))).map(|l| l.describe()).collect();
    if cs.is_empty() {
        "no comparison".into()
    } else {
        let mut s = cs.iter().take(4).map(|c| format!("`{}`", c)).collect::<Vec<_>>().join(", ");
        if cs.len() > 4 {
            s.push_str(", …");
        }
        s
    }
}

/// The script of obligations 1 and 2: two rules with their heads aligned
/// on the identity positions, the functional dependencies of the keyed
/// relations both read, their complete relations' membership as Booleans,
/// and both arithmetic parts. `unsat` means no identity and bar at which
/// both fire.
fn pair_script(prog: &Program, analysis: &Analysis, r1: usize, r2: usize, identity: &[(Term, Term)]) -> Script {
    let mut s = Script::new("QF_LRA");
    let mut a = Smt::new(prog, &prog.rules[r1].unit, "a_");
    let mut b = Smt::new(prog, &prog.rules[r2].unit, "b_");
    let mut entities: BTreeSet<String> = BTreeSet::new();
    let mut classes = Classes::default();
    let equate = |s: &mut Script, classes: &mut Classes, x: &str, y: &str| {
        if classes.union(x, y) {
            s.assert(format!("(= {} {})", x, y));
        }
    };
    for (t1, t2) in identity {
        if let (Some(x), Some(y)) = (a.head_term(t1, &mut s, &mut entities), b.head_term(t2, &mut s, &mut entities)) {
            equate(&mut s, &mut classes, &x, &y);
        }
    }
    // Functional dependencies, to a fixpoint.
    let mut memo = HashMap::new();
    let mut visiting = HashSet::new();
    let atoms1 = atoms(&prog.rules[r1]);
    let atoms2 = atoms(&prog.rules[r2]);
    let encode = |ctx: &mut Smt, atom: &Atom, s: &mut Script, entities: &mut BTreeSet<String>| -> Vec<Option<String>> { atom.terms.iter().map(|t| ctx.head_term(t, s, entities)).collect() };
    let enc1: Vec<(usize, Vec<Option<String>>)> = atoms1.iter().enumerate().map(|(i, at)| (i, encode(&mut a, at, &mut s, &mut entities))).collect();
    let enc2: Vec<(usize, Vec<Option<String>>)> = atoms2.iter().enumerate().map(|(i, at)| (i, encode(&mut b, at, &mut s, &mut entities))).collect();
    loop {
        let mut changed = false;
        for (i, t1) in &enc1 {
            for (j, t2) in &enc2 {
                let (a1, a2) = (atoms1[*i], atoms2[*j]);
                if a1.name != a2.name || a1.terms.len() != a2.terms.len() {
                    continue;
                }
                if !functional(prog, analysis, &a1.name, &mut memo, &mut visiting) {
                    continue;
                }
                let Some(sig) = prog.relations.get(&a1.name) else { continue };
                let ids = sig.identity_positions();
                let same = ids.iter().all(|&k| match (&t1[k], &t2[k]) {
                    (Some(x), Some(y)) => classes.find(x) == classes.find(y),
                    _ => false,
                });
                if !same {
                    continue;
                }
                for k in 0..a1.terms.len() {
                    if ids.contains(&k) {
                        continue;
                    }
                    if let (Some(x), Some(y)) = (&t1[k], &t2[k]) {
                        if classes.union(x, y) {
                            s.assert(format!("(= {} {})", x, y));
                            changed = true;
                        }
                    }
                }
            }
        }
        if !changed {
            break;
        }
    }
    // Membership of complete relations as Booleans over the identity class.
    let booleans = |ctx: &mut Smt, rule: &crate::ir::Rule, s: &mut Script, classes: &mut Classes, entities: &mut BTreeSet<String>| {
        for l in &rule.body {
            let (atom, positive) = match l {
                Literal::Atom(at) => (at, true),
                Literal::Neg(at) => (at, false),
                _ => continue,
            };
            if !prog.complete.contains(&atom.name) {
                continue;
            }
            let Some(sig) = prog.relations.get(&atom.name) else { continue };
            let ids = sig.identity_positions();
            if !positive && atom.terms.iter().enumerate().any(|(k, t)| !ids.contains(&k) && !matches!(t, Term::Wild(_))) {
                continue;
            }
            let mut key = Vec::new();
            let mut ok = true;
            for &k in &ids {
                match ctx.head_term(&atom.terms[k], s, entities) {
                    Some(x) => key.push(classes.find(&x)),
                    None => {
                        ok = false;
                        break;
                    }
                }
            }
            if !ok {
                continue;
            }
            let name = format!("in_{}", symbol(&format!("{}__{}", atom.name, key.join("_"))));
            s.declare(&name, "Bool");
            s.assert(if positive { name } else { format!("(not {})", name) });
        }
    };
    booleans(&mut a, &prog.rules[r1], &mut s, &mut classes, &mut entities);
    booleans(&mut b, &prog.rules[r2], &mut s, &mut classes, &mut entities);
    a.guard(&mut s, analysis, r1, None);
    b.guard(&mut s, analysis, r2, None);
    if entities.len() > 1 {
        s.assert(format!("(distinct {})", entities.iter().cloned().collect::<Vec<_>>().join(" ")));
    }
    if a.nonlinear || b.nonlinear {
        s.logic = "QF_NRA".into();
    }
    s
}

/// Obligations 1 to 3 (section 6.4), appended to the tier's report; a
/// relation whose every rule pair is proved disjoint is relabelled keyed
/// by guard in the analysis.
pub fn tier3_structural(prog: &Program, compiled: &mut Compiled, driver: &Driver, t: &mut TierReport) {
    // 1. Guard implication for the relations keyed at runtime.
    let at_runtime: Vec<String> = compiled
        .analysis
        .relations
        .iter()
        .filter(|r| r.keyedness == Keyedness::AtRuntime && r.kind == "derived")
        .map(|r| r.name.clone())
        .collect();
    for name in at_runtime {
        let rules = prog.rules_for(&name);
        let Some(sig) = prog.relations.get(&name) else { continue };
        let ids = sig.identity_positions();
        let mut all_proved = !rules.is_empty() && rules.len() > 1;
        for i in 0..rules.len() {
            for j in (i + 1)..rules.len() {
                let (r1, r2) = (rules[i], rules[j]);
                let identity: Vec<(Term, Term)> = ids
                    .iter()
                    .filter_map(|&k| Some((prog.rules[r1].head.terms.get(k)?.clone(), prog.rules[r2].head.terms.get(k)?.clone())))
                    .collect();
                let mut script = pair_script(prog, &compiled.analysis, r1, r2, &identity);
                let id = format!("t3-o1-{}-r{}-r{}", symbol(&name), r1 + 1, r2 + 1);
                let statement = format!(
                    "{} and {} never both derive a tuple of `{}` for one identity and bar ({} | {})",
                    prog.rule_label(r1),
                    prog.rule_label(r2),
                    name,
                    guard_text(&prog.rules[r1]),
                    guard_text(&prog.rules[r2])
                );
                script.comment(format!("obligation {}: {}", id, statement));
                script.comment("unsat: the guards are disjoint over the arithmetic part (keyed by guard); sat: not proved");
                let mut o = Obligation::new(3, id, "guard disjointness", statement);
                o.rule = Some(format!("{}, {}", prog.rule_label(r1), prog.rule_label(r2)));
                let o = settle(
                    o,
                    driver,
                    &script,
                    (Verdict::Proved, "unsat: the arithmetic parts admit no common instance"),
                    (Verdict::NotProved, "sat: the arithmetic parts admit a common model; the relational part may still separate the rules"),
                );
                if o.verdict != Verdict::Proved {
                    all_proved = false;
                }
                t.obligations.push(o);
            }
        }
        if all_proved {
            if let Some(r) = compiled.analysis.relations.iter_mut().find(|r| r.name == name) {
                r.keyedness = Keyedness::ByGuard;
                r.keyedness_note = format!("tier 3: every pair of rules disjoint by guard ({})", driver.solver.as_deref().unwrap_or("solver"));
            }
            t.notes.push(format!("`{}`: keyed at runtime upgraded to keyed by guard", name));
        }
    }
    // 2. Guard disjointness of the decide rules (conflict freedom).
    let decide = prog.rules_for("decide");
    let mut all_proved = decide.len() > 1;
    for i in 0..decide.len() {
        for j in (i + 1)..decide.len() {
            let (r1, r2) = (decide[i], decide[j]);
            let head = |r: usize| -> Option<(Term, Term, String)> {
                let h = &prog.rules[r].head;
                let t = h.terms.first()?.clone();
                match h.terms.get(1)? {
                    Term::Ctor(c, args, _) => Some((t, args.first()?.clone(), c.clone())),
                    _ => None,
                }
            };
            let (Some((t1, e1, c1)), Some((t2, e2, c2))) = (head(r1), head(r2)) else { continue };
            let mut script = pair_script(prog, &compiled.analysis, r1, r2, &[(t1, t2), (e1, e2)]);
            let id = format!("t3-o2-r{}-r{}", r1 + 1, r2 + 1);
            let statement = format!(
                "{} ({}) and {} ({}) never both fire for one instrument and bar ({} | {})",
                prog.rule_label(r1),
                c1,
                prog.rule_label(r2),
                c2,
                guard_text(&prog.rules[r1]),
                guard_text(&prog.rules[r2])
            );
            script.comment(format!("obligation {}: {}", id, statement));
            script.comment("unsat: conflict-free by guard; sat: not proved (the kernel's conflict check stays)");
            let mut o = Obligation::new(3, id, "conflict freedom", statement);
            o.rule = Some(format!("{}, {}", prog.rule_label(r1), prog.rule_label(r2)));
            let o = settle(
                o,
                driver,
                &script,
                (Verdict::Proved, "unsat: conflict-free by guard"),
                (Verdict::NotProved, "sat: the arithmetic parts admit a common model; the runtime conflict check stays"),
            );
            if o.verdict != Verdict::Proved {
                all_proved = false;
            }
            t.obligations.push(o);
        }
    }
    if all_proved {
        t.notes.push("`decide`: conflict-free by guard".into());
    }
    // 3a. R6: each fold equals its closed expression over the reals, within
    // the margin (the fold was computed in doubles).
    if let Some(folded) = &compiled.rewritten.folded {
        let mut k = 0;
        for (ri, (orig, fold)) in prog.rules.iter().zip(folded.rules.iter()).enumerate() {
            let mut diffs: Vec<(Expr, Lit)> = Vec::new();
            for (a, b) in orig.body.iter().zip(fold.body.iter()) {
                literal_diffs(a, b, &mut diffs);
            }
            for (before, after) in diffs {
                k += 1;
                let Some(v) = q_of_lit(&after) else { continue };
                let mut script = Script::new("QF_LRA");
                let mut ctx = Smt::new(prog, &orig.unit, "f_");
                ctx.params_fixed = true;
                let Some(e) = ctx.term(&before, &mut script) else { continue };
                if ctx.nonlinear {
                    script.logic = "QF_NRA".into();
                }
                let mu = margin(v.to_f64().abs(), -40).unwrap_or(Q::ZERO);
                let m = numeral(mu);
                script.assert(format!("(or (> (- {e} {v}) {m}) (< (- {e} {v}) (- {m})))", v = numeral(v)));
                let id = format!("t3-o3-fold{}", k);
                let statement = format!(
                    "`{}` = {} within 2^-40 · max(1, |{}|)",
                    crate::compiler::print::render_expr(&before, &|v: &str| v.to_string()),
                    crate::compiler::print::render_lit(&after),
                    crate::compiler::print::render_lit(&after)
                );
                script.comment(format!("obligation {}: {}", id, statement));
                script.comment("unsat: the fold equals the closed expression over the reals (within the margin); sat: refuted");
                let o = Obligation::new(3, id, "rewrite equivalence (R6)", statement).on_rule(prog, ri);
                t.obligations.push(settle(
                    o,
                    driver,
                    &script,
                    (Verdict::Proved, "unsat: equal over the reals within the margin"),
                    (Verdict::Refuted, "sat: the fold differs from the closed expression beyond the margin"),
                ));
            }
        }
    }
    // 3b. R3: the finalisation of a merged aggregate from the shared moments
    // equals its two-pass form over the reals.
    let mut seen: BTreeSet<String> = BTreeSet::new();
    for (origin, other, primary) in &compiled.rewritten.shared {
        for li in [*primary, *other] {
            let Some(Literal::Agg { agg, .. }) = prog.rules.get(*origin).and_then(|r| r.body.get(li)) else {
                continue;
            };
            if !seen.insert(agg.clone()) {
                continue;
            }
            let id = format!("t3-o3-finalise-{}", symbol(agg));
            match agg.as_str() {
                "sum" | "mean" => {
                    let statement = format!("finalise_{}: the moment form is the definition (Σx, Σx/n)", agg);
                    t.obligations.push(
                        Obligation::new(3, id, "rewrite equivalence (R3)", statement)
                            .on_rule(prog, *origin)
                            .with(Verdict::Proved, "an identity by definition"),
                    );
                }
                "std" | "cov" | "corr" | "ols_beta" => {
                    let (mut script, statement) = moments_identity(agg);
                    script.comment(format!("obligation {}: {}", id, statement));
                    script.comment("unsat: the centred sums equal the moment form over the reals; sat: refuted");
                    let o = Obligation::new(3, id, "rewrite equivalence (R3)", statement).on_rule(prog, *origin);
                    t.obligations.push(settle(
                        o,
                        driver,
                        &script,
                        (Verdict::Proved, "unsat: the finalisation identity holds over the reals"),
                        (Verdict::Refuted, "sat: the finalisation identity fails"),
                    ));
                }
                _ => {
                    let statement = format!("finalise_{}: the shared summary is the window's rows; no arithmetic identity", agg);
                    t.obligations.push(
                        Obligation::new(3, id, "rewrite equivalence (R3)", statement)
                            .on_rule(prog, *origin)
                            .with(Verdict::Skipped, "nothing to prove"),
                    );
                }
            }
        }
    }
}

/// The identity behind a moment aggregate's finalisation, for three
/// symbolic rows: `Σ(x−x̄)(y−ȳ) = Σxy − ΣxΣy/n` (the variance form with
/// `y = x`), as a QF_NRA script whose negation must be unsatisfiable.
fn moments_identity(agg: &str) -> (Script, String) {
    let mut s = Script::new("QF_NRA");
    let n = 3;
    let xs: Vec<String> = (1..=n).map(|i| format!("x{}", i)).collect();
    let ys: Vec<String> = if agg == "std" { xs.clone() } else { (1..=n).map(|i| format!("y{}", i)).collect() };
    for v in xs.iter().chain(ys.iter()) {
        s.declare(v, "Real");
    }
    let sum = |vs: &[String]| format!("(+ {})", vs.join(" "));
    let mx = format!("(/ {} {}.0)", sum(&xs), n);
    let my = format!("(/ {} {}.0)", sum(&ys), n);
    let centred: Vec<String> = xs.iter().zip(ys.iter()).map(|(x, y)| format!("(* (- {} {}) (- {} {}))", x, mx, y, my)).collect();
    let products: Vec<String> = xs.iter().zip(ys.iter()).map(|(x, y)| format!("(* {} {})", x, y)).collect();
    let moment = format!("(- {} (/ (* {} {}) {}.0))", sum(&products), sum(&xs), sum(&ys), n);
    s.assert(format!("(not (= {} {}))", sum(&centred), moment));
    let statement = match agg {
        "std" => "finalise_std: Σ(x−x̄)² = Σx² − (Σx)²/n, so std = sqrt(·/(n−1)) agrees with the two-pass form (n = 3)".to_string(),
        "cov" => "finalise_cov: Σ(x−x̄)(y−ȳ) = Σxy − ΣxΣy/n, so cov = ·/(n−1) agrees with the two-pass form (n = 3)".to_string(),
        "corr" => "finalise_corr: the centred cross sum and both centred squares equal their moment forms, so corr = Sxy/sqrt(Sxx·Syy) agrees (n = 3)".to_string(),
        _ => "finalise_ols_beta: the centred cross sum and the regressor's centred square equal their moment forms, so beta = Sxy/Sxx agrees (n = 3)".to_string(),
    };
    (s, statement)
}

/// The closed subexpressions a fold replaced: pairs (expression before,
/// literal after), by walking the two literals in parallel.
fn literal_diffs(a: &Literal, b: &Literal, out: &mut Vec<(Expr, Lit)>) {
    match (a, b) {
        (Literal::Cmp { lhs: l1, rhs: r1, .. }, Literal::Cmp { lhs: l2, rhs: r2, .. }) => {
            expr_diffs(l1, l2, out);
            expr_diffs(r1, r2, out);
        }
        (Literal::Assign { expr: e1, .. }, Literal::Assign { expr: e2, .. }) => expr_diffs(e1, e2, out),
        (Literal::Agg { args: a1, conj: c1, .. }, Literal::Agg { args: a2, conj: c2, .. }) => {
            for (x, y) in a1.iter().zip(a2.iter()) {
                expr_diffs(x, y, out);
            }
            for (x, y) in c1.iter().zip(c2.iter()) {
                literal_diffs(x, y, out);
            }
        }
        (Literal::Window { dur: d1, min: m1, .. }, Literal::Window { dur: d2, min: m2, .. }) => {
            expr_diffs(d1, d2, out);
            expr_diffs(m1, m2, out);
        }
        (Literal::Top { n: Some(n1), .. }, Literal::Top { n: Some(n2), .. }) => expr_diffs(n1, n2, out),
        (Literal::Case { branches: b1, otherwise: o1, .. }, Literal::Case { branches: b2, otherwise: o2, .. }) => {
            for (x, y) in b1.iter().zip(b2.iter()) {
                for (g, h) in x.guard.iter().zip(y.guard.iter()) {
                    literal_diffs(g, h, out);
                }
                expr_diffs(&x.value, &y.value, out);
            }
            if let (Some(x), Some(y)) = (o1, o2) {
                expr_diffs(x, y, out);
            }
        }
        (Literal::Builtin(crate::ir::Builtin::Lag { n: n1, .. }, _), Literal::Builtin(crate::ir::Builtin::Lag { n: n2, .. }, _)) => expr_diffs(n1, n2, out),
        _ => {}
    }
}

fn expr_diffs(a: &Expr, b: &Expr, out: &mut Vec<(Expr, Lit)>) {
    match (a, b) {
        (Expr::Lit(..), Expr::Lit(..)) | (Expr::Var(..), _) | (Expr::Param(..), _) => {}
        (_, Expr::Lit(l, _)) => out.push((a.clone(), l.clone())),
        (Expr::Neg(x, _), Expr::Neg(y, _)) => expr_diffs(x, y, out),
        (Expr::Bin(_, a1, b1, _), Expr::Bin(_, a2, b2, _)) => {
            expr_diffs(a1, a2, out);
            expr_diffs(b1, b2, out);
        }
        (Expr::Call(_, x, _), Expr::Call(_, y, _)) => {
            for (p, q) in x.iter().zip(y.iter()) {
                expr_diffs(p, q, out);
            }
        }
        _ => {}
    }
}
