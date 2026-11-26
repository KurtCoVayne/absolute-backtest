//! A derived relation's rules compiled to operators over the columns of a
//! bar (the block evaluator, `--evaluator block`; ERRATA E-150). Where every
//! literal of every rule, in the order `derive_rules` solves them, is one
//! the operators take, a miss of `Kernel::call` at a bar derives the
//! relation's whole cross-section there at once (`Kernel::run_plan`): the
//! first atom's tuples at the bar for every entity (`Scan`), then per later
//! literal a merge with another relation at the bar by entity (`Join`), a
//! column computed (`Let`) or a filter (`Test`). Each operator does to the
//! frame of bindings what the row path does to its block of rows, in the
//! same order, with the same arithmetic (`cexpr.rs`). An aggregate the
//! data layer answers, delivered in the stream or as a table, is read
//! first (`Pushed`) or joined by entity (`PushedJoin`) through the reads
//! the row path makes of it (ERRATA E-175). What an operator
//! cannot do exactly as the row path would -- an input that is not one
//! tuple per entity, a value of a kind a column does not hold, a partial
//! operation on any row -- declines the whole (relation, bar), and
//! `derive_rules` derives it as it does without a plan, reading what the
//! plan read from the memo.

use std::sync::Arc;

use super::cexpr::{CExpr, FCol};
use super::col::{BlockEntry, Col, ColBlock, RuleTag};
use super::colops::{ConjAtom, RowsAggOp, WindowAggOp};
use super::eval;
use super::hash::FxHashMap;
use super::{CompiledRule, Kernel, MemoKey, Tuple, Value};
use crate::ir::{Atom, Builtin, Cal, CmpOp, Literal, Term, WindowKind};

/// One operator of a rule's plan.
#[derive(Debug)]
pub(crate) enum ColOp {
    /// The first atom: its relation's tuples at the bar, one per entity,
    /// the entity bound to `eslot` and each other argument but the key to
    /// its slot in `binds` (`None` for a wildcard).
    Scan { rel: usize, eslot: usize, binds: Vec<Option<usize>> },
    /// A later atom at the bar with the entity bound: the rows whose entity
    /// has the relation's one tuple there, with its arguments bound.
    Join { rel: usize, binds: Vec<Option<usize>> },
    /// `X = expr` with `X` unbound.
    Let { slot: usize, expr: CExpr },
    /// `lhs op rhs`.
    Test { op: CmpOp, lhs: CExpr, rhs: CExpr },
    /// An aggregate the data layer answers (K-4.5) as the first literal:
    /// its answer at the bar for every entity, the entity bound to `eslot`
    /// and the value to `vslot`, when every answer was received by the
    /// barrier (`Kernel::pushed_answers`, the same test); a delivered
    /// literal's answers from its store (`Kernel::delivered_block`, ERRATA
    /// E-157, E-175).
    Pushed { eslot: usize, vslot: usize },
    /// An aggregate the data layer answers, after an atom bound the
    /// entity to `eslot`: each row's answer for its entity bound to
    /// `vslot`, a row without one dropped where the literal is delivered;
    /// read as a table, every row's answer received by the barrier, else
    /// the row path evaluates a window here and the operator declines, as
    /// it does where the row path solves the window over the whole frame
    /// before reading the answers (`Kernel::bucket_prefetch`; ERRATA E-175).
    PushedJoin { eslot: usize, vslot: usize },
    /// A `rows[n]` aggregate over the entity's own rows (`colops.rs`).
    RowsAgg(Box<RowsAggOp>),
    /// A later atom with the entity bound, keyed at a time each row binds
    /// (`R(A, L, ..)` after `L` is bound): the rows whose entity has the
    /// relation's one tuple at the row's time, with its arguments bound.
    GatherAt { rel: usize, tslot: usize, binds: Vec<Option<usize>> },
    /// An as-of read of a stored relation with the entity bound
    /// (`R(A, L, ..) asof T`): per row, what the row path's walk finds
    /// (`Kernel::asof_stored_into`) for the pattern of the entity and the
    /// constants `pat`, at most one tuple, its key and other arguments
    /// bound by `binds` (tuple position, slot).
    AsofStored {
        rel: usize,
        at: usize,
        within: Option<Cal>,
        epos: usize,
        pat: Vec<Option<Value>>,
        binds: Vec<(usize, usize)>,
    },
    /// An as-of read of a derived relation with the entity bound
    /// (`R(A, T1) asof T0`, an entity's previous row): per row, the bar the
    /// row path's walk finds (`Kernel::asof_walk`), the relation read at
    /// each bar from its columns where they are held and by the row path's
    /// call where they are not; at most one tuple, its key and other
    /// arguments bound by `binds` (tuple position, slot).
    AsofDerived { rel: usize, at: usize, within: Option<Cal>, binds: Vec<(usize, usize)> },
    /// `not R(A, T, ..)`: the rows whose entity has no tuple of the
    /// relation at the bar matching the constants (column, value).
    AntiJoin { rel: usize, consts: Vec<(usize, NegConst)> },
    /// `prev(T, T0)` of a time every row shares: the label before it on its
    /// calendar, bound to `t1` (`None` for a wildcard); with none, no row.
    Prev { t: usize, cal: Cal, t1: Option<usize> },
    /// `X = case { guard -> value ; ... ; else -> value }` with `X` unbound,
    /// each guard one read with the frame's entity bound: per row the first
    /// branch whose guard finds the entity's one tuple, its value over the
    /// guard's bindings and the row's; the `else` value for the rows no
    /// guard holds of, or no row without one.
    Case { out: usize, branches: Vec<CaseBranch>, otherwise: Option<CaseValue> },
    /// A calendar window's aggregate over a stored relation read as of each
    /// of the window's bars (`colops.rs`).
    WindowAgg(Box<WindowAggOp>),
}

/// A branch of a case: its guard, and its value with the frame's slots
/// the value reads.
#[derive(Debug)]
pub(crate) struct CaseBranch {
    pub guard: Guard,
    pub value: CaseValue,
}

/// A case's value: the expression, and the frame's slots it reads.
#[derive(Debug)]
pub(crate) struct CaseValue {
    pub expr: CExpr,
    pub reads: Vec<usize>,
}

/// A case's guard: an atom at the bar with the frame's entity (a `Join`'s
/// shape), or an as-of read of a stored relation (an `AsofStored`'s), each
/// other variable it binds local to the branch.
#[derive(Debug)]
pub(crate) enum Guard {
    Atom {
        rel: usize,
        binds: Vec<Option<usize>>,
    },
    AsofStored {
        rel: usize,
        at: usize,
        within: Option<Cal>,
        epos: usize,
        pat: Vec<Option<Value>>,
        binds: Vec<(usize, usize)>,
    },
}

/// A constant of a negated atom: a parameter's value, or a literal, whose
/// value is the row path's own (`lit_value`) at the bar it is first read.
#[derive(Debug)]
pub(crate) enum NegConst {
    Value(Value),
    Lit(crate::ir::Lit),
}

/// An as-of read's time for the rows that share it, its floor, the latest
/// label at or before it and the relation's columns there.
#[derive(Clone)]
struct AsofAt {
    t0: i64,
    floor: i64,
    first: Option<i64>,
    block: Option<Arc<ColBlock>>,
}

/// An operator's refusal of the bar: the row path derives it.
pub(crate) struct Decline;

impl From<super::RunError> for Decline {
    fn from(_: super::RunError) -> Decline {
        Decline
    }
}

/// What a head argument other than the key and the entity takes.
#[derive(Debug)]
pub(crate) enum HeadCol {
    Slot(usize),
    Const(Value),
}

/// One rule's operators, each with the position of its literal in the
/// rule's body, and its head.
#[derive(Debug)]
pub(crate) struct RulePlan {
    pub ri: usize,
    pub nslots: usize,
    pub key_slot: usize,
    pub ops: Vec<(usize, ColOp)>,
    pub head: Vec<HeadCol>,
}

/// A derived relation's plan: its rules', in the order `derive_rules`
/// takes them (the rules the plan skips left out, as there).
#[derive(Debug)]
pub(crate) struct ColPlan {
    pub key_pos: usize,
    pub entity_pos: usize,
    pub rules: Vec<RulePlan>,
}

/// The bindings of a rule at a bar while its plan runs: the entity of
/// each row, and each slot's column (or one value for every row).
struct Frame {
    ids: Vec<u32>,
    cols: Vec<Option<FCol>>,
}

impl Frame {
    /// The rows `keep` names (ascending), every column with them.
    fn keep(&mut self, keep: &[u32]) {
        if keep.len() == self.ids.len() {
            return;
        }
        self.ids = keep.iter().map(|&i| self.ids[i as usize]).collect();
        for c in self.cols.iter_mut().flatten() {
            if let FCol::Col(_) = c {
                *c = c.gather(keep);
            }
        }
    }
}

/// A rule's rows at the bar: their entities and the head's other
/// arguments.
struct RuleOut {
    ri: usize,
    ids: Vec<u32>,
    head: Vec<FCol>,
}

/// What a plan derived at a bar: its columns, or its tuples where they are
/// not columns (a few tuples of several rules out of entity order).
pub(crate) enum Planned {
    Block(Arc<ColBlock>),
    Rows(Vec<(Tuple, usize)>),
}

impl<'p> Kernel<'p> {
    /// Every derived relation's plan, compiled for the order the rules are
    /// solved in now (the kernel's build, then a plan applied).
    pub(crate) fn compile_col_plans(&mut self) {
        let closed = crate::plan::planner::closed_loop(self.prog);
        self.closed_rels = self.rels.iter().map(|r| closed.contains(&r.name)).collect();
        let plans: Vec<Option<Arc<ColPlan>>> = (0..self.rels.len()).map(|rel| self.compile_col_plan(rel).map(Arc::new)).collect();
        // A rule whose window the columns feed is not cut across the
        // workers under the block evaluator: a bar its plan declines is
        // derived on this kernel, so each entity's window state is one.
        let windowed: Vec<usize> = plans
            .iter()
            .flatten()
            .flat_map(|p| p.rules.iter())
            .filter(|rp| rp.ops.iter().any(|(_, op)| matches!(op, ColOp::RowsAgg(_) | ColOp::WindowAgg(_))))
            .map(|rp| rp.ri)
            .collect();
        for ri in 0..self.prog.rules.len() {
            self.parallel_ok[ri] = !crate::plan::planner::cross_sectional(self.prog, ri) && !(self.cfg.block_eval && windowed.contains(&ri));
        }
        self.col_plans = plans;
    }

    /// The plan of `rel`, if every literal of its rules maps to an operator
    /// and its head to the frame: a derived relation with one entity
    /// argument and no input, open-loop. A temporally recursive relation
    /// reads itself at an earlier bar a row binds (`GatherAt`): its columns
    /// there, which the chain's extension at each barrier memoised
    /// (`Kernel::trim_stores`), or derived there first through `cols_of`
    /// as the row path's call derives them; behind the horizon the bar
    /// declines, and the row path reads the entity's latest tuples there
    /// (E-126).
    fn compile_col_plan(&self, rel: usize) -> Option<ColPlan> {
        let info = &self.rels[rel];
        if info.stored || !info.inputs.is_empty() || info.entity_positions.len() != 1 || info.rules.is_empty() || self.closed_rels[rel] {
            return None;
        }
        let (key_pos, entity_pos) = (info.key_pos, info.entity_positions[0]);
        let mut rules = Vec::new();
        for &ri in &info.rules {
            if self.skip_rule.get(ri).copied().unwrap_or(false) {
                continue;
            }
            rules.push(self.compile_rule_plan(ri, key_pos, entity_pos)?);
        }
        Some(ColPlan { key_pos, entity_pos, rules })
    }

    fn compile_rule_plan(&self, ri: usize, key_pos: usize, entity_pos: usize) -> Option<RulePlan> {
        let cr = &self.compiled[ri];
        let rule = &self.prog.rules[ri];
        let slot_of = |t: &Term| match t {
            Term::Var(v, _) => cr.slots.get(v).copied(),
            _ => None,
        };
        let key_slot = slot_of(rule.head.terms.get(key_pos)?)?;
        let head_entity = slot_of(rule.head.terms.get(entity_pos)?)?;
        let mut bound = vec![false; cr.nslots];
        bound[key_slot] = true;
        let mut eslot: Option<usize> = None;
        let mut ops = Vec::new();
        for (pos, &li) in self.plan_order(ri).iter().enumerate() {
            let op = match &rule.body[li] {
                Literal::Atom(atom) => {
                    let r = *self.rel_ids.get(&atom.name)?;
                    let ri2 = &self.rels[r];
                    if !ri2.inputs.is_empty() || ri2.entity_positions.len() != 1 || atom.terms.len() < 2 {
                        return None;
                    }
                    let (k2, e2) = (ri2.key_pos, ri2.entity_positions[0]);
                    let ks = slot_of(atom.terms.get(k2)?)?;
                    let es = slot_of(&atom.terms[e2])?;
                    // Keyed at another time than the rule's: one a row has
                    // bound, read with the frame's entity.
                    let gather = ks != key_slot;
                    if gather && (!bound[ks] || ks == es || eslot != Some(es)) {
                        return None;
                    }
                    // The atom's other arguments bind fresh variables, once
                    // each, or are wildcards.
                    let mut binds = Vec::new();
                    for (p, term) in atom.terms.iter().enumerate() {
                        if p == k2 || p == e2 {
                            continue;
                        }
                        match term {
                            Term::Wild(_) => binds.push(None),
                            Term::Var(..) => {
                                let s = slot_of(term)?;
                                if bound[s] || s == es {
                                    return None;
                                }
                                bound[s] = true;
                                binds.push(Some(s));
                            }
                            _ => return None,
                        }
                    }
                    match eslot {
                        _ if gather => ColOp::GatherAt { rel: r, tslot: ks, binds },
                        None => {
                            if bound[es] {
                                return None;
                            }
                            bound[es] = true;
                            eslot = Some(es);
                            ColOp::Scan { rel: r, eslot: es, binds }
                        }
                        Some(e) => {
                            if es != e {
                                return None;
                            }
                            ColOp::Join { rel: r, binds }
                        }
                    }
                }
                Literal::AsOf { atom, at, within_bucket, .. } if eslot.is_some() => self.compile_asof(cr, atom, at, within_bucket.as_ref(), &mut bound, eslot)?,
                // `not R(A, T, ..)` at the bar with the frame's entity, its
                // other terms wildcards or constants.
                Literal::Neg(atom) if eslot.is_some() => {
                    let r = *self.rel_ids.get(&atom.name)?;
                    let ri2 = &self.rels[r];
                    if !ri2.inputs.is_empty() || ri2.entity_positions.len() != 1 || atom.terms.len() < 2 {
                        return None;
                    }
                    let (k2, e2) = (ri2.key_pos, ri2.entity_positions[0]);
                    if slot_of(atom.terms.get(k2)?)? != key_slot || slot_of(atom.terms.get(e2)?) != eslot {
                        return None;
                    }
                    let mut consts = Vec::new();
                    for (c, term) in atom.terms.iter().enumerate().filter(|(p, _)| *p != k2 && *p != e2).map(|(_, t)| t).enumerate() {
                        match term {
                            Term::Wild(_) => {}
                            Term::Param(name, _) => consts.push((c, NegConst::Value(cr.params.get(name)?.clone()))),
                            Term::Lit(l, _) => consts.push((c, NegConst::Lit(l.clone()))),
                            _ => return None,
                        }
                    }
                    ColOp::AntiJoin { rel: r, consts }
                }
                // `prev(T, T0)` of a time the frame binds once: a constant.
                Literal::Builtin(Builtin::Prev { t, t1 }, _) => {
                    let ts = slot_of(t)?;
                    let cal = self.term_cal(cr, t);
                    if !bound[ts] || cal.per_entity() {
                        return None;
                    }
                    let t1 = match t1 {
                        Term::Wild(_) => None,
                        Term::Var(..) => {
                            let s = slot_of(t1)?;
                            if bound[s] {
                                return None;
                            }
                            bound[s] = true;
                            Some(s)
                        }
                        _ => return None,
                    };
                    ColOp::Prev { t: ts, cal, t1 }
                }
                // Before the first atom an assignment or a test reads the
                // bar and constants only, on the one starting binding.
                Literal::Assign { var, expr, .. } => {
                    let slot = *cr.slots.get(var)?;
                    if bound[slot] {
                        return None;
                    }
                    let expr = self.compile_expr(cr, expr, &bound, eslot.unwrap_or(usize::MAX))?;
                    bound[slot] = true;
                    ColOp::Let { slot, expr }
                }
                Literal::Cmp { op, lhs, rhs, .. } => ColOp::Test {
                    op: *op,
                    lhs: self.compile_expr(cr, lhs, &bound, eslot.unwrap_or(usize::MAX))?,
                    rhs: self.compile_expr(cr, rhs, &bound, eslot.unwrap_or(usize::MAX))?,
                },
                // An aggregate the data layer can answer, grouped by the
                // frame's entity at the rule's own bar; whether it does is
                // known only once the run asked (`answered`).
                Literal::Agg { var, .. } if self.cfg.pushdown && eslot.is_some_and(|e| self.pushable_at(ri, li, e, key_slot)) => {
                    let vslot = *cr.slots.get(var)?;
                    if bound[vslot] {
                        return None;
                    }
                    bound[vslot] = true;
                    ColOp::PushedJoin { eslot: eslot?, vslot }
                }
                Literal::Agg { .. } if eslot.is_some() => match self.compile_rows_agg(ri, li, &bound, eslot?, key_slot) {
                    Some(op) => {
                        bound[op.out] = true;
                        ColOp::RowsAgg(Box::new(op))
                    }
                    None => {
                        let op = self.compile_window_agg(ri, li, &bound, eslot?, key_slot)?;
                        bound[op.out] = true;
                        ColOp::WindowAgg(Box::new(op))
                    }
                },
                // `X = case { .. }` with `X` unbound, each guard one read
                // with the frame's entity; a guard's variables are the
                // branch's, its value over them and the frame's slots.
                Literal::Case { var, branches, otherwise, .. } if eslot.is_some() => {
                    let es = eslot?;
                    let out = *cr.slots.get(var)?;
                    if bound[out] {
                        return None;
                    }
                    let mut arms = Vec::with_capacity(branches.len());
                    for b in branches {
                        let [g] = b.guard.as_slice() else { return None };
                        let mut local = bound.clone();
                        let guard = self.compile_guard(cr, g, key_slot, es, &mut local)?;
                        arms.push(CaseBranch {
                            guard,
                            value: self.compile_case_value(cr, &b.value, &local, es)?,
                        });
                    }
                    let otherwise = match otherwise {
                        Some(e) => Some(self.compile_case_value(cr, e, &bound, es)?),
                        None => None,
                    };
                    bound[out] = true;
                    ColOp::Case { out, branches: arms, otherwise }
                }
                // A pushed aggregate binds the entity the head names (its
                // group) and its value; whether the data layer answers it
                // at a bar is known only there.
                Literal::Agg { var, .. } if pos == 0 && self.cfg.pushdown => {
                    let vslot = *cr.slots.get(var)?;
                    if vslot == head_entity || vslot == key_slot {
                        return None;
                    }
                    bound[head_entity] = true;
                    bound[vslot] = true;
                    eslot = Some(head_entity);
                    ColOp::Pushed { eslot: head_entity, vslot }
                }
                _ => return None,
            };
            ops.push((li, op));
        }
        if eslot != Some(head_entity) {
            return None;
        }
        let mut head = Vec::new();
        for (p, term) in rule.head.terms.iter().enumerate() {
            if p == key_pos || p == entity_pos {
                continue;
            }
            head.push(match term {
                Term::Var(..) => {
                    let s = slot_of(term)?;
                    if !bound[s] || s == head_entity {
                        return None;
                    }
                    HeadCol::Slot(s)
                }
                Term::Param(p, _) => HeadCol::Const(cr.params.get(p)?.clone()),
                Term::Lit(l, _) => HeadCol::Const(literal(l)?),
                _ => return None,
            });
        }
        Some(RulePlan {
            ri,
            nslots: cr.nslots,
            key_slot,
            ops,
            head,
        })
    }

    /// A `rows[n]` aggregate the columns can feed (`colops.rs`): one rows
    /// window over the rule's key, its count and minimum constants, its
    /// conjunction in the row path's solve order the atoms of derived
    /// relations at the window's bar with the frame's entity, the anchor
    /// first, each binding fresh slots; its arguments expressions over
    /// those slots and the frame's others; its group bound by the frame
    /// (the call binds nothing but the aggregate); not under running sums
    /// and not `quantile`.
    fn compile_rows_agg(&self, ri: usize, li: usize, bound: &[bool], es: usize, key_slot: usize) -> Option<RowsAggOp> {
        if !self.cfg.window_cache || self.cfg.running_sums {
            return None;
        }
        let cr = &self.compiled[ri];
        let rule = &self.prog.rules[ri];
        let lit = &rule.body[li];
        let Literal::Agg { var, agg, args, conj, within, .. } = lit else { return None };
        let slot_of = |t: &Term| match t {
            Term::Var(v, _) => cr.slots.get(v).copied(),
            _ => None,
        };
        let out = *cr.slots.get(var)?;
        if bound[out] || agg == "quantile" {
            return None;
        }
        let f = if agg == "count" { None } else { Some(abt_agg::Function::parse(agg, None).ok()?) };
        let windows: Vec<&Literal> = conj.iter().filter(|l| matches!(l, Literal::Window { .. })).collect();
        let [Literal::Window {
            var: wv @ Term::Var(..),
            base: bv @ Term::Var(..),
            kind: WindowKind::Rows,
            dur,
            min,
            lo: None,
            ..
        }] = windows.as_slice()
        else {
            return None;
        };
        let (wslot, bslot) = (slot_of(wv)?, slot_of(bv)?);
        let mut consts = Vec::new();
        dur.vars(&mut consts);
        min.vars(&mut consts);
        if bslot != key_slot || bound[wslot] || !consts.is_empty() {
            return None;
        }
        // The row path's solve order of the conjunction, the anchor (its
        // first atom as written) first.
        let order: Vec<Literal> = eval::conj_order(conj).into_iter().filter(|l| !matches!(l, Literal::Window { .. })).collect();
        let anchor = conj.iter().find(|l| matches!(l, Literal::Atom(_)))?;
        if order.first() != Some(anchor) {
            return None;
        }
        let mut local = vec![false; cr.nslots];
        let mut atoms = Vec::with_capacity(order.len());
        for l in &order {
            let Literal::Atom(a) = l else { return None };
            let r = *self.rel_ids.get(&a.name)?;
            let info = &self.rels[r];
            if info.stored || !info.inputs.is_empty() || info.entity_positions.len() != 1 || a.terms.len() < 2 || self.closed_rels[r] {
                return None;
            }
            let (k2, e2) = (info.key_pos, info.entity_positions[0]);
            if slot_of(a.terms.get(k2)?)? != wslot || slot_of(a.terms.get(e2)?)? != es {
                return None;
            }
            let mut binds = Vec::new();
            for (p, term) in a.terms.iter().enumerate() {
                if p == k2 || p == e2 {
                    continue;
                }
                match term {
                    Term::Wild(_) => binds.push(None),
                    Term::Var(..) => {
                        let s = slot_of(term)?;
                        if bound[s] || local[s] || s == wslot || s == bslot {
                            return None;
                        }
                        local[s] = true;
                        binds.push(Some(s));
                    }
                    _ => return None,
                }
            }
            atoms.push(ConjAtom { rel: r, binds });
        }
        // The arguments read the atoms' slots and the frame's, never the
        // base time (the window would not be shared) or the entity.
        let mut frame_slots = Vec::new();
        let slot = |v: &str| cr.slots.get(v).copied().filter(|&s| s != es && s != bslot && s != wslot && (local[s] || bound[s]));
        let param = |p: &str| cr.params.get(p).cloned();
        // A count reads no argument of a row.
        let mut cargs = Vec::with_capacity(args.len());
        for a in args.iter().filter(|_| agg != "count") {
            cargs.push(CExpr::compile(a, &slot, &param)?);
            let mut vs = Vec::new();
            a.vars(&mut vs);
            frame_slots.extend(vs.iter().filter_map(|(v, _)| cr.slots.get(v).copied()).filter(|&s| bound[s] && !local[s]));
        }
        frame_slots.sort_unstable();
        frame_slots.dedup();
        // The group, as the row path takes it (`agg_plan_at`): bound, so
        // the call binds only the aggregate.
        let group = eval::group_vars(rule, lit, conj, args, within.as_deref());
        if group.iter().filter_map(|v| cr.slots.get(v)).any(|&s| !bound[s]) {
            return None;
        }
        Some(RowsAggOp {
            li,
            out,
            eslot: es,
            f,
            atoms,
            args: cargs,
            frame_slots,
        })
    }

    /// Whether rule `ri`'s aggregate literal `li` is one the data layer can
    /// answer (`pushdown::eligibility`) grouped by the frame's entity at the
    /// rule's bar: its instrument variable in `eslot`, its window's base the
    /// rule's key. A rows window stays the window operator's.
    fn pushable_at(&self, ri: usize, li: usize, eslot: usize, key_slot: usize) -> bool {
        let cr = &self.compiled[ri];
        match super::pushdown::eligibility(self.prog, ri, li) {
            Ok(s) => !matches!(s.window, WindowKind::Rows | WindowKind::PriorRows) && cr.slots.get(&s.entity_var) == Some(&eslot) && cr.slots.get(&s.base_var) == Some(&key_slot),
            Err(_) => false,
        }
    }

    /// An as-of read whose entity the frame binds (`eslot`), at a bound
    /// time; its key and other variables fresh (bound in `bound`), its
    /// other terms wildcards or, of a stored relation, constants, which the
    /// walk matches as the row path's pattern does. A derived relation is
    /// walked over its own calendar's labels.
    fn compile_asof(&self, cr: &CompiledRule, atom: &Atom, at: &Term, within_bucket: Option<&Cal>, bound: &mut [bool], eslot: Option<usize>) -> Option<ColOp> {
        let r = *self.rel_ids.get(&atom.name)?;
        let ri2 = &self.rels[r];
        if !ri2.inputs.is_empty() || ri2.entity_positions.len() != 1 || within_bucket.is_some_and(|c| c.per_entity()) {
            return None;
        }
        let stored = ri2.stored;
        if !stored && (ri2.cal.per_entity() || self.closed_rels[r]) {
            return None;
        }
        let (k2, e2) = (ri2.key_pos, ri2.entity_positions[0]);
        let at = var_slot(cr, at)?;
        if !bound[at] || var_slot(cr, atom.terms.get(e2)?) != eslot {
            return None;
        }
        let mut pat = vec![None; atom.terms.len()];
        let mut binds = Vec::new();
        for (p, term) in atom.terms.iter().enumerate() {
            if p == e2 {
                continue;
            }
            match term {
                Term::Wild(_) => {}
                Term::Var(..) => {
                    let s = var_slot(cr, term)?;
                    if bound[s] {
                        return None;
                    }
                    bound[s] = true;
                    binds.push((p, s));
                }
                Term::Param(name, _) if p != k2 && stored => pat[p] = Some(cr.params.get(name)?.clone()),
                Term::Lit(l, _) if p != k2 && stored => pat[p] = Some(literal(l)?),
                _ => return None,
            }
        }
        let within = within_bucket.cloned();
        Some(if stored {
            ColOp::AsofStored {
                rel: r,
                at,
                within,
                epos: e2,
                pat,
                binds,
            }
        } else {
            ColOp::AsofDerived { rel: r, at, within, binds }
        })
    }

    /// A case's guard: an atom at the bar (`key_slot`) with the frame's
    /// entity (`es`) whose other terms are wildcards or fresh variables, or
    /// an as-of read of a stored relation; the variables it binds marked in
    /// the branch's `bound`.
    fn compile_guard(&self, cr: &CompiledRule, lit: &Literal, key_slot: usize, es: usize, bound: &mut [bool]) -> Option<Guard> {
        match lit {
            Literal::Atom(atom) => {
                let r = *self.rel_ids.get(&atom.name)?;
                let info = &self.rels[r];
                if !info.inputs.is_empty() || info.entity_positions.len() != 1 || atom.terms.len() < 2 || self.closed_rels[r] {
                    return None;
                }
                let (k2, e2) = (info.key_pos, info.entity_positions[0]);
                if var_slot(cr, atom.terms.get(k2)?)? != key_slot || var_slot(cr, atom.terms.get(e2)?)? != es {
                    return None;
                }
                let mut binds = Vec::new();
                for (p, term) in atom.terms.iter().enumerate() {
                    if p == k2 || p == e2 {
                        continue;
                    }
                    match term {
                        Term::Wild(_) => binds.push(None),
                        Term::Var(..) => {
                            let s = var_slot(cr, term)?;
                            if bound[s] {
                                return None;
                            }
                            bound[s] = true;
                            binds.push(Some(s));
                        }
                        _ => return None,
                    }
                }
                Some(Guard::Atom { rel: r, binds })
            }
            Literal::AsOf { atom, at, within_bucket, .. } => match self.compile_asof(cr, atom, at, within_bucket.as_ref(), bound, Some(es))? {
                ColOp::AsofStored { rel, at, within, epos, pat, binds } => Some(Guard::AsofStored { rel, at, within, epos, pat, binds }),
                _ => None,
            },
            _ => None,
        }
    }

    /// A case's value over the slots `bound` (the entity's aside), with the
    /// slots it reads.
    fn compile_case_value(&self, cr: &CompiledRule, e: &crate::ir::Expr, bound: &[bool], es: usize) -> Option<CaseValue> {
        let expr = self.compile_expr(cr, e, bound, es)?;
        let mut reads = Vec::new();
        expr.slots(&mut reads);
        reads.sort_unstable();
        reads.dedup();
        Some(CaseValue { expr, reads })
    }

    /// A calendar window's aggregate the columns can serve (`colops.rs`):
    /// one window over the rule's calendar (not a rows window, not one of
    /// the entity's sessions) whose base is the rule's key, its length and
    /// minimum constants, that the row path serves from its per-bar cache
    /// (`Kernel::window_plan`); the cached conjunction one as-of read of a
    /// stored relation at the window's bar with the frame's entity, its
    /// other variables fresh, its other terms wildcards or constants; the
    /// tests applied at each call comparisons over the frame's slots, the
    /// window's bar and the read's, as the arguments are; its group bound
    /// by the frame; not under running sums and not `quantile`.
    fn compile_window_agg(&self, ri: usize, li: usize, bound: &[bool], es: usize, key_slot: usize) -> Option<WindowAggOp> {
        if !self.cfg.window_cache || self.cfg.running_sums {
            return None;
        }
        let cr = &self.compiled[ri];
        let rule = &self.prog.rules[ri];
        let lit = &rule.body[li];
        let Literal::Agg { var, agg, args, conj, within, .. } = lit else { return None };
        let out = *cr.slots.get(var)?;
        if bound[out] || agg == "quantile" || (agg != "count" && abt_agg::Function::parse(agg, None).is_err()) {
            return None;
        }
        let wp = self.window_plan(cr, lit, conj, args)?;
        let Literal::Window {
            var: wv @ Term::Var(..),
            kind,
            base: bv @ Term::Var(..),
            dur,
            min,
            lo,
            ..
        } = &conj[wp.window]
        else {
            return None;
        };
        if matches!(kind, WindowKind::Rows | WindowKind::PriorRows) || self.per_entity_window(kind) {
            return None;
        }
        let (wslot, bslot) = (var_slot(cr, wv)?, var_slot(cr, bv)?);
        let mut consts = Vec::new();
        dur.vars(&mut consts);
        min.vars(&mut consts);
        if let Some(l) = lo {
            l.vars(&mut consts);
        }
        if bslot != key_slot || wslot != wp.wslot || bound[wslot] || !consts.is_empty() {
            return None;
        }
        let [Literal::AsOf { atom, at, within_bucket, .. }] = wp.order.as_slice() else { return None };
        if var_slot(cr, at)? != wslot {
            return None;
        }
        let r = *self.rel_ids.get(&atom.name)?;
        let info = &self.rels[r];
        if !info.stored || !info.inputs.is_empty() || info.entity_positions.len() != 1 || within_bucket.as_ref().is_some_and(|c| c.per_entity()) {
            return None;
        }
        let (k2, e2) = (info.key_pos, info.entity_positions[0]);
        if var_slot(cr, atom.terms.get(e2)?)? != es {
            return None;
        }
        // The read's variables, each fresh and bound once.
        let mut local = vec![false; cr.nslots];
        let mut pat = vec![None; atom.terms.len()];
        let mut binds = Vec::new();
        for (p, term) in atom.terms.iter().enumerate() {
            if p == e2 {
                continue;
            }
            match term {
                Term::Wild(_) => {}
                Term::Var(..) => {
                    let s = var_slot(cr, term)?;
                    if bound[s] || local[s] || s == wslot {
                        return None;
                    }
                    local[s] = true;
                    binds.push((p, s));
                }
                Term::Param(name, _) if p != k2 => pat[p] = Some(cr.params.get(name)?.clone()),
                Term::Lit(l, _) if p != k2 => pat[p] = Some(literal(l)?),
                _ => return None,
            }
        }
        // A row's slots: the frame's but its entity, the window's bar, the
        // read's.
        let row_slot = |v: &str| cr.slots.get(v).copied().filter(|&s| s != es && (bound[s] || local[s] || s == wslot));
        let param = |p: &str| cr.params.get(p).cloned();
        let mut late = Vec::with_capacity(wp.late.len());
        for (l, need) in &wp.late {
            let Literal::Cmp { op, lhs, rhs, .. } = l else { return None };
            if need.iter().any(|&s| !bound[s]) {
                return None;
            }
            late.push((*op, CExpr::compile(lhs, &row_slot, &param)?, CExpr::compile(rhs, &row_slot, &param)?));
        }
        let mut cargs = Vec::with_capacity(args.len());
        for a in args.iter().filter(|_| agg != "count") {
            cargs.push(CExpr::compile(a, &row_slot, &param)?);
        }
        if eval::group_vars(rule, lit, conj, args, within.as_deref()).iter().filter_map(|v| cr.slots.get(v)).any(|&s| !bound[s]) {
            return None;
        }
        Some(WindowAggOp {
            li,
            out,
            eslot: es,
            rel: r,
            epos: e2,
            within: within_bucket.clone(),
            pat,
            binds,
            args: cargs,
            late,
        })
    }

    /// The rows of `open` (ascending rows of the frame) a case's guard holds
    /// of at bar `t`, ascending, each with the guard's one tuple bound into
    /// `sub` (a column per slot, over those rows). `None` where the guard
    /// would find two tuples for a row or cannot be read as columns.
    fn case_guard(&mut self, g: &Guard, f: &Frame, open: &[u32], t: i64, sub: &mut [Option<FCol>]) -> Option<Vec<u32>> {
        match g {
            Guard::Atom { rel, binds } => {
                let b = self.cols_of(*rel, t).filter(|b| b.unique)?;
                let (mut hit, mut at) = (Vec::with_capacity(open.len().min(b.len())), Vec::with_capacity(open.len().min(b.len())));
                let mut j = 0usize;
                for &r in open {
                    let id = f.ids[r as usize];
                    while j < b.ids.len() && b.ids[j] < id {
                        j += 1;
                    }
                    if j < b.ids.len() && b.ids[j] == id {
                        hit.push(r);
                        at.push(j as u32);
                    }
                }
                for (c, s) in b.cols.iter().zip(binds) {
                    if let Some(s) = s {
                        sub[*s] = Some(FCol::Col(c.gather(&at)));
                    }
                }
                Some(hit)
            }
            Guard::AsofStored { rel, at, within, epos, pat, binds } => {
                let found = self.asof_stored_frame(*rel, f.cols[*at].as_ref()?, within.as_ref(), *epos, pat, &f.ids, open)?;
                for (s, c) in bind_cols(&found, binds) {
                    sub[s] = Some(FCol::Col(c));
                }
                Some(found.into_iter().map(|(r, _)| r).collect())
            }
        }
    }

    /// The identity plan (`ExecConfig::block_identity`, a test switch): the
    /// rules' result `d` of `rel` at `t` turned into columns and back into
    /// tuples, which the call then serves, with the block memoised beside
    /// them. `None` where the relation is not read as columns there.
    pub(crate) fn identity_block(&mut self, rel: usize, t: i64, d: &super::Derived) -> Option<(super::Derived, Arc<ColBlock>)> {
        let info = &self.rels[rel];
        if !self.columns_at(t) || self.closed_rels[rel] || info.entity_positions.len() != 1 || !info.inputs.is_empty() {
            return None;
        }
        let arity = self.prog.relations.get(&info.name).map(|s| s.args.len())?;
        let b = ColBlock::from_rows(t, info.key_pos, info.entity_positions[0], arity, d)?;
        let fresh = Arc::new(ColBlock::new(b.t, b.key_pos, b.entity_pos, b.ids.clone(), b.cols.clone(), b.tags.clone()));
        let (rows, built) = fresh.rows();
        if built {
            self.row_views[rel] += 1;
        }
        self.plan_bars[rel] += 1;
        Some((rows, fresh))
    }

    /// The memo was cleared (a late tuple, a resumed run): with the stores
    /// trimmed, the memo's oldest bucket no longer marks where rows were
    /// dropped (E-135), so no plan runs until the next trim sets the floor.
    pub(crate) fn memo_cleared(&mut self) {
        if self.cfg.trim_stores {
            self.horizon_floor = i64::MAX;
        }
    }

    /// Whether `rel` has a plan over columns that can run.
    pub fn has_col_plan(&self, rel: usize) -> bool {
        self.col_plans.get(rel).is_some_and(|p| p.as_ref().is_some_and(|p| self.answered(p)))
    }

    /// Whether every aggregate a plan reads from the data layer was
    /// answered there (`bind::push_down`); one the data layer was not
    /// asked for, or could not answer, is evaluated here at every bar, and
    /// the relation has no plan.
    fn answered(&self, plan: &ColPlan) -> bool {
        plan.rules.iter().all(|rp| {
            rp.ops
                .iter()
                .all(|(li, op)| !matches!(op, ColOp::Pushed { .. } | ColOp::PushedJoin { .. }) || self.pushed.contains_key(&(rp.ri, *li)))
        })
    }

    /// Per relation with a plan or a count: its name, whether it has a
    /// plan, the bars its plan derived and declined, and its results built
    /// into tuples from columns (`--timing`'s per-relation table).
    pub fn plan_counts(&self) -> Vec<(String, bool, u64, u64, u64)> {
        (0..self.rels.len())
            .filter(|&r| self.has_col_plan(r) || self.plan_bars[r] + self.plan_declined_bars[r] + self.row_views[r] > 0)
            .map(|r| (self.rels[r].name.clone(), self.has_col_plan(r), self.plan_bars[r], self.plan_declined_bars[r], self.row_views[r]))
            .collect()
    }

    /// The window steps the window operator of `name`'s rules handed to
    /// the row path (`--timing`'s per-relation table).
    pub fn row_steps(&self, name: &str) -> u64 {
        self.rel_ids.get(name).map(|&r| self.rels[r].rules.iter().map(|&ri| self.row_steps[ri]).sum()).unwrap_or(0)
    }

    /// An expression of a rule over its bound slots other than the entity.
    fn compile_expr(&self, cr: &super::CompiledRule, e: &crate::ir::Expr, bound: &[bool], eslot: usize) -> Option<CExpr> {
        let slot = |v: &str| cr.slots.get(v).copied().filter(|&s| bound[s] && s != eslot);
        let param = |p: &str| cr.params.get(p).cloned();
        CExpr::compile(e, &slot, &param)
    }

    /// Whether a plan or a column read may run at bar `t`: on the main
    /// kernel, in the order the plans were compiled for, recording no
    /// window summaries, and not behind the horizon -- the last cutoff of
    /// the memo's eviction or the stores' trim, or, with the stores
    /// trimmed, before the memo's oldest bucket, where `call` answers an
    /// entity's read from the tuples the eviction kept (E-126, E-134,
    /// E-135): there the row path reads what it reads today.
    pub(crate) fn columns_at(&self, t: i64) -> bool {
        self.worker.is_none() && !self.written_order && !self.recording && t >= self.horizon_floor && !(self.cfg.trim_stores && self.memo.oldest().is_some_and(|o| t < o))
    }

    /// `rel`'s tuples at bar `t` as columns: a stored relation's block
    /// (not kept), or a derived relation's result through `call` (a plan's,
    /// or the rules') with its columns memoised. `None` where `columns_at`
    /// refuses, for a closed-loop relation, where the call halts or left an
    /// instance stuck, and where the result has no columns
    /// (`ColBlock::from_rows`): the operator that asked declines, and the
    /// row path meets the halt where it would.
    pub(crate) fn cols_of(&mut self, rel: usize, t: i64) -> Option<Arc<ColBlock>> {
        if !self.columns_at(t) || self.closed_rels.get(rel).copied().unwrap_or(true) {
            return None;
        }
        let info = self.rels[rel].clone();
        if info.entity_positions.len() != 1 || !info.inputs.is_empty() {
            return None;
        }
        let arity = self.prog.relations.get(&info.name).map(|s| s.args.len())?;
        let (key_pos, e) = (info.key_pos, info.entity_positions[0]);
        if info.stored {
            let rows = self.store_lookup(rel, t, None);
            return ColBlock::from_slice(t, key_pos, e, arity, rows).map(Arc::new);
        }
        let entry = |k: &Kernel<'p>| match k.memo.blocks.get(rel, t) {
            Some(BlockEntry::Cols(b)) => Some(Some(b.clone())),
            Some(BlockEntry::NotCols) => Some(None),
            None => None,
        };
        if let Some(b) = entry(self) {
            return b;
        }
        let mkey: MemoKey = (rel, vec![Value::Time(t)]);
        if self.memo_stuck.contains_key(&mkey) || self.memo_errors.contains_key(&mkey) {
            return None;
        }
        let mut pattern = vec![None; arity];
        pattern[key_pos] = Some(Value::Time(t));
        let d = self.call(rel, &pattern).ok()?;
        if self.memo_stuck.contains_key(&mkey) {
            return None;
        }
        // A plan's result is memoised as columns by the call.
        if let Some(b) = entry(self) {
            return b;
        }
        let b = ColBlock::from_rows(t, key_pos, e, arity, &d).map(Arc::new);
        let kept = match &b {
            Some(b) => BlockEntry::Cols(b.clone()),
            None => BlockEntry::NotCols,
        };
        self.memo.blocks.insert(rel, t, kept);
        b
    }

    /// `rel` derived at bar `t` by its plan, every rule's operators run over
    /// the bar's columns and the rules' rows united as `derive_rules` unites
    /// them (a tuple derived twice kept once, the first rule's), in entity
    /// order above `SORTED_MIN` tuples as `call` leaves a result. `None` when
    /// it has no plan or declines; the literal counts and the pushed answers
    /// read are the kernel's only when it does not.
    pub(crate) fn run_plan(&mut self, rel: usize, t: i64) -> Option<Planned> {
        let plan = self.col_plans.get(rel).cloned().flatten()?;
        if !self.answered(&plan) {
            return None;
        }
        let mut counts: Vec<(usize, usize, usize, usize)> = Vec::new();
        let mut pushed = 0usize;
        let mut calls = 0usize;
        let mut outs: Vec<RuleOut> = Vec::with_capacity(plan.rules.len());
        let ran = self.columns_at(t)
            && plan.rules.iter().all(|rp| match self.run_rule(rp, t, &mut counts, &mut pushed, &mut calls) {
                Some(out) => {
                    outs.push(out);
                    true
                }
                None => false,
            });
        let planned = if ran { assemble(&plan, t, outs) } else { None };
        let Some(planned) = planned else {
            // The window steps the operators ran are met again by the
            // rules (`rows_replay`), which `call` then forgets.
            self.plan_declined_bars[rel] += 1;
            return None;
        };
        self.forget_replay(rel);
        for (ri, li, entering, leaving) in counts {
            self.count_literal(Some(ri), li, entering, leaving, true);
        }
        self.stats.pushed_answers += pushed;
        self.stats.window_calls += calls;
        self.plan_bars[rel] += 1;
        Some(planned)
    }

    /// The window steps kept for `rel`'s rules dropped: its bar was
    /// derived, by its plan or by the rules after the plan declined.
    pub(crate) fn forget_replay(&mut self, rel: usize) {
        if !self.rows_replay.is_empty() {
            let rules = &self.rels[rel].rules;
            self.rows_replay.retain(|(ri, ..)| !rules.contains(ri));
        }
    }

    /// One rule's operators over the bar, stopping at an empty frame as
    /// `solve_kleene_block` stops; each literal's bindings entering and
    /// leaving go to `counts`. `None` when an operator declines.
    fn run_rule(&mut self, rp: &RulePlan, t: i64, counts: &mut Vec<(usize, usize, usize, usize)>, pushed: &mut usize, calls: &mut usize) -> Option<RuleOut> {
        let mut f = Frame {
            ids: Vec::new(),
            cols: vec![None; rp.nslots],
        };
        f.cols[rp.key_slot] = Some(FCol::Const(Value::Time(t)));
        // Until the first atom binds the entities, the frame is the one
        // starting binding, alive or emptied by a test.
        let mut scanned = false;
        let mut alive = true;
        for (li, op) in &rp.ops {
            let entering = if scanned { f.ids.len() } else { 1 };
            let rows = if scanned { f.ids.len() } else { 1 };
            match op {
                ColOp::Scan { rel, eslot, binds } => {
                    let b = self.cols_of(*rel, t)?;
                    if !b.unique {
                        return None;
                    }
                    f.ids = b.ids.clone();
                    // The entity's slot is read from `ids`.
                    f.cols[*eslot] = None;
                    for (c, s) in b.cols.iter().zip(binds) {
                        if let Some(s) = s {
                            f.cols[*s] = Some(FCol::Col(c.clone()));
                        }
                    }
                }
                ColOp::Join { rel, binds } => {
                    let b = self.cols_of(*rel, t)?;
                    if !b.unique {
                        return None;
                    }
                    // Both sides ascend strictly: one merge pass.
                    let (mut keep, mut at) = (Vec::with_capacity(f.ids.len()), Vec::with_capacity(f.ids.len()));
                    let mut j = 0usize;
                    for (i, &id) in f.ids.iter().enumerate() {
                        while j < b.ids.len() && b.ids[j] < id {
                            j += 1;
                        }
                        if j < b.ids.len() && b.ids[j] == id {
                            keep.push(i as u32);
                            at.push(j as u32);
                        }
                    }
                    f.keep(&keep);
                    for (c, s) in b.cols.iter().zip(binds) {
                        if let Some(s) = s {
                            f.cols[*s] = Some(FCol::Col(c.gather(&at)));
                        }
                    }
                }
                ColOp::Let { slot, expr } => {
                    let v = expr.eval(&f.cols, rows)?;
                    if !scanned && !matches!(v, FCol::Const(_)) {
                        return None;
                    }
                    f.cols[*slot] = Some(v);
                }
                ColOp::Test { op, lhs, rhs } => {
                    let mask = CExpr::test(*op, lhs, rhs, &f.cols, rows)?;
                    if scanned {
                        let keep: Vec<u32> = mask.iter().enumerate().filter(|(_, k)| **k).map(|(i, _)| i as u32).collect();
                        f.keep(&keep);
                    } else {
                        alive = mask.first().copied().unwrap_or(false);
                    }
                }
                ColOp::RowsAgg(op) => {
                    let values = self.run_rows_agg(rp.ri, op, &f.ids, &f.cols, op.eslot, t, calls)?;
                    let keep: Vec<u32> = values.iter().enumerate().filter(|(_, v)| v.is_some()).map(|(i, _)| i as u32).collect();
                    f.keep(&keep);
                    f.cols[op.out] = Some(FCol::Col(Col::of_values(values.into_iter().flatten().collect())));
                }
                ColOp::WindowAgg(op) => {
                    let values = self.run_window_agg(rp.ri, op, &f.ids, &f.cols, t, calls)?;
                    let keep: Vec<u32> = values.iter().enumerate().filter(|(_, v)| v.is_some()).map(|(i, _)| i as u32).collect();
                    f.keep(&keep);
                    f.cols[op.out] = Some(FCol::Col(Col::of_values(values.into_iter().flatten().collect())));
                }
                ColOp::Case { out, branches, otherwise } => {
                    let n = f.ids.len();
                    let mut vals: Vec<Option<Value>> = vec![None; n];
                    // The rows no branch has taken, ascending: each branch's
                    // guard is read for them alone, as the row path tries a
                    // row's branches in turn.
                    let mut open: Vec<u32> = (0..n as u32).collect();
                    for br in branches {
                        if open.is_empty() {
                            break;
                        }
                        let mut sub: Vec<Option<FCol>> = vec![None; rp.nslots];
                        let hit = self.case_guard(&br.guard, &f, &open, t, &mut sub)?;
                        if hit.is_empty() {
                            continue;
                        }
                        for &s in &br.value.reads {
                            if sub[s].is_none() {
                                sub[s] = f.cols[s].as_ref().map(|c| c.gather(&hit));
                            }
                        }
                        let v = br.value.expr.eval(&sub, hit.len())?;
                        for (k, &r) in hit.iter().enumerate() {
                            vals[r as usize] = Some(v.value(k));
                        }
                        let mut taken = hit.iter().peekable();
                        open.retain(|r| {
                            let took = taken.peek() == Some(&r);
                            if took {
                                taken.next();
                            }
                            !took
                        });
                    }
                    if let (Some(e), false) = (otherwise, open.is_empty()) {
                        let mut sub: Vec<Option<FCol>> = vec![None; rp.nslots];
                        for &s in &e.reads {
                            sub[s] = f.cols[s].as_ref().map(|c| c.gather(&open));
                        }
                        let v = e.expr.eval(&sub, open.len())?;
                        for (k, &r) in open.iter().enumerate() {
                            vals[r as usize] = Some(v.value(k));
                        }
                    }
                    let keep: Vec<u32> = vals.iter().enumerate().filter(|(_, v)| v.is_some()).map(|(i, _)| i as u32).collect();
                    f.keep(&keep);
                    f.cols[*out] = Some(FCol::Col(Col::of_values(vals.into_iter().flatten().collect())));
                }
                ColOp::GatherAt { rel, tslot, binds } => {
                    // The relation at each time the rows bind, in the order
                    // the rows first reach it, as the row path demands it.
                    let mut at: Vec<(u32, u32)> = Vec::with_capacity(f.ids.len());
                    let mut keep: Vec<u32> = Vec::with_capacity(f.ids.len());
                    let mut blocks: Vec<(i64, Arc<ColBlock>)> = Vec::new();
                    let mut index: FxHashMap<i64, usize> = FxHashMap::default();
                    let mut last = usize::MAX;
                    for (i, &id) in f.ids.iter().enumerate() {
                        let Value::Time(t1) = f.cols[*tslot].as_ref()?.value(i) else { return None };
                        let k = match blocks.get(last) {
                            Some((t, _)) if *t == t1 => last,
                            _ => match index.get(&t1) {
                                Some(&k) => k,
                                None => {
                                    let b = self.cols_of(*rel, t1).filter(|b| b.unique)?;
                                    blocks.push((t1, b));
                                    index.insert(t1, blocks.len() - 1);
                                    blocks.len() - 1
                                }
                            },
                        };
                        last = k;
                        if let Some(j) = blocks[k].1.find(id) {
                            keep.push(i as u32);
                            at.push((k as u32, j as u32));
                        }
                    }
                    f.keep(&keep);
                    for (c, s) in binds.iter().enumerate() {
                        if let Some(s) = s {
                            f.cols[*s] = Some(FCol::Col(match blocks.as_slice() {
                                [(_, b)] => b.cols[c].gather(&at.iter().map(|&(_, j)| j).collect::<Vec<u32>>()),
                                _ => {
                                    let mut col: Option<Col> = None;
                                    for &(k, j) in &at {
                                        let v = blocks[k as usize].1.cols[c].value(j as usize);
                                        col.get_or_insert_with(|| Col::like(&v, at.len())).push(&v);
                                    }
                                    col.unwrap_or(Col::Num(Vec::new()))
                                }
                            }));
                        }
                    }
                }
                ColOp::AsofStored { rel, at, within, epos, pat, binds } => {
                    let rows: Vec<u32> = (0..f.ids.len() as u32).collect();
                    let found = self.asof_stored_frame(*rel, f.cols[*at].as_ref()?, within.as_ref(), *epos, pat, &f.ids, &rows)?;
                    let keep: Vec<u32> = found.iter().map(|(r, _)| *r).collect();
                    f.keep(&keep);
                    for (s, c) in bind_cols(&found, binds) {
                        f.cols[s] = Some(FCol::Col(c));
                    }
                }
                ColOp::AsofDerived { rel, at, within, binds } => {
                    let (cal, arity) = {
                        let info = &self.rels[*rel];
                        (info.cal.clone(), self.prog.relations.get(&info.name).map(|s| s.args.len())?)
                    };
                    let mut keep: Vec<u32> = Vec::with_capacity(f.ids.len());
                    let mut cols: Vec<Option<Col>> = vec![None; binds.len()];
                    // For the rows' time (every row's one, mostly).
                    let mut here: Option<AsofAt> = None;
                    for (i, &id) in f.ids.iter().enumerate() {
                        let Value::Time(t0) = f.cols[*at].as_ref()?.value(i) else { return None };
                        if here.as_ref().is_none_or(|h| h.t0 != t0) {
                            let floor = self.asof_floor(within.as_ref(), None, t0).ok()?;
                            let first = eval::asof_latest(self.domain_for(&cal, None).ok()?, floor, t0);
                            let block = first.and_then(|k| self.cols_of(*rel, k));
                            here = Some(AsofAt { t0, floor, first, block });
                        }
                        let Some(AsofAt { floor, first: Some(first), block, .. }) = here.clone() else { continue };
                        let Some(tu) = self.asof_derived_tuple(*rel, &cal, arity, id, first, floor, block.as_deref()).ok()? else {
                            continue;
                        };
                        keep.push(i as u32);
                        for ((p, _), c) in binds.iter().zip(cols.iter_mut()) {
                            c.get_or_insert_with(|| Col::like(&tu[*p], f.ids.len())).push(&tu[*p]);
                        }
                    }
                    f.keep(&keep);
                    for ((_, s), c) in binds.iter().zip(cols) {
                        f.cols[*s] = Some(FCol::Col(c.unwrap_or(Col::Num(Vec::new()))));
                    }
                }
                ColOp::AntiJoin { rel, consts } => {
                    let mut want: Vec<(usize, Value)> = Vec::with_capacity(consts.len());
                    for (c, k) in consts {
                        want.push((
                            *c,
                            match k {
                                NegConst::Value(v) => v.clone(),
                                NegConst::Lit(l) => super::lit_value(l, &mut self.symbols, &mut self.labels, &self.literal_equities),
                            },
                        ));
                    }
                    let b = self.cols_of(*rel, t)?;
                    // Both sides ascend: one merge pass, each entity's run of
                    // the relation searched for a tuple the constants match.
                    let mut keep: Vec<u32> = Vec::with_capacity(f.ids.len());
                    let mut j = 0usize;
                    for (i, &id) in f.ids.iter().enumerate() {
                        while j < b.ids.len() && b.ids[j] < id {
                            j += 1;
                        }
                        let run = j..j + b.ids[j..].iter().take_while(|&&x| x == id).count();
                        if !run.into_iter().any(|r| want.iter().all(|(c, v)| b.cols[*c].value(r) == *v)) {
                            keep.push(i as u32);
                        }
                    }
                    f.keep(&keep);
                }
                ColOp::Prev { t: ts, cal, t1 } => {
                    let Some(FCol::Const(Value::Time(at))) = &f.cols[*ts] else { return None };
                    let at = *at;
                    match self.domain_for(cal, None).ok()?.range(..at).next_back().copied() {
                        Some(p) => {
                            if let Some(s) = t1 {
                                f.cols[*s] = Some(FCol::Const(Value::Time(p)));
                            }
                        }
                        None if scanned => f.keep(&[]),
                        None => alive = false,
                    }
                }
                ColOp::Pushed { eslot, vslot } => {
                    let p = self.pushed.get(&(rp.ri, *li)).cloned()?;
                    if p.base_slot != rp.key_slot || p.entity_slot != *eslot {
                        return None;
                    }
                    let (ids, col) = match &p.delivered {
                        // Every row of the label the store holds, as
                        // `delivered_answers` reads them unbound.
                        Some(d) => {
                            let (ids, values) = self.delivered_block(d, t)?;
                            (ids, Col::of_values(values))
                        }
                        // First in its rule, the literal is entered by the
                        // one starting binding, under `PREFETCH_MIN_ROWS`:
                        // the row path solves no window over a block here
                        // (`bucket_prefetch`, E-175).
                        None => {
                            let section = p.by_time.get(&t)?;
                            if section.iter().any(|(_, a)| a.value.is_none() || a.error || a.r > self.now) || section.windows(2).any(|w| w[0].0 >= w[1].0) {
                                return None;
                            }
                            for (_, a) in section {
                                self.pushed_read_r = self.pushed_read_r.max(a.r);
                            }
                            (section.iter().map(|(s, _)| *s).collect(), table_answers(&p, section.iter().map(|(_, a)| a)))
                        }
                    };
                    f.ids = ids;
                    f.cols[*vslot] = Some(FCol::Col(col));
                    *pushed += 1;
                }
                ColOp::PushedJoin { eslot, vslot } => {
                    let p = self.pushed.get(&(rp.ri, *li)).cloned()?;
                    if p.base_slot != rp.key_slot || p.entity_slot != *eslot {
                        return None;
                    }
                    // The row path reads one answer per row, each a pushed
                    // answer it counts.
                    *pushed += f.ids.len();
                    match &p.delivered {
                        // A row's answer is its entity's in the store, as
                        // `delivered_answers` reads it bound; a row without
                        // one leaves.
                        Some(d) => {
                            let (ids, values) = self.delivered_block(d, t)?;
                            let (mut keep, mut found) = (Vec::with_capacity(f.ids.len()), Vec::with_capacity(f.ids.len()));
                            let mut j = 0usize;
                            for (i, &id) in f.ids.iter().enumerate() {
                                while j < ids.len() && ids[j] < id {
                                    j += 1;
                                }
                                if j < ids.len() && ids[j] == id {
                                    keep.push(i as u32);
                                    found.push(values[j].clone());
                                }
                            }
                            f.keep(&keep);
                            f.cols[*vslot] = Some(FCol::Col(Col::of_values(found)));
                        }
                        // Read as a table, a row without an answer received
                        // by the barrier is evaluated here: the bar is the
                        // rules'. So is a bar where the row path solves the
                        // window over the rows entering the literal before
                        // it reads their answers (`bucket_prefetch`, the one
                        // test of both evaluators): what that solve derives
                        // is counted (E-175).
                        None => {
                            let cr = self.compiled[rp.ri].clone();
                            let prog = self.prog;
                            if self.bucket_prefetch(&cr, &prog.rules[rp.ri].body[*li], f.ids.len()).is_some() {
                                return None;
                            }
                            let section = p.by_time.get(&t)?;
                            if section.windows(2).any(|w| w[0].0 >= w[1].0) {
                                return None;
                            }
                            let mut answers = Vec::with_capacity(f.ids.len());
                            let mut j = 0usize;
                            for &id in &f.ids {
                                while j < section.len() && section[j].0 < id {
                                    j += 1;
                                }
                                let a = section.get(j).filter(|(s, _)| *s == id).map(|(_, a)| a)?;
                                if a.value.is_none() || a.error || a.r > self.now {
                                    return None;
                                }
                                answers.push(a);
                            }
                            for a in &answers {
                                self.pushed_read_r = self.pushed_read_r.max(a.r);
                            }
                            f.cols[*vslot] = Some(FCol::Col(table_answers(&p, answers.into_iter())));
                        }
                    }
                }
            }
            scanned |= matches!(op, ColOp::Scan { .. } | ColOp::Pushed { .. });
            let leaving = if scanned { f.ids.len() } else { usize::from(alive) };
            counts.push((rp.ri, *li, entering, leaving));
            if leaving == 0 {
                break;
            }
        }
        let n = f.ids.len();
        let mut head = Vec::with_capacity(rp.head.len());
        for h in &rp.head {
            head.push(match h {
                // A rule that stopped at an empty frame has no value to give.
                HeadCol::Slot(s) => match &f.cols[*s] {
                    Some(c) => c.clone(),
                    None if n == 0 => FCol::Col(Col::Num(Vec::new())),
                    None => return None,
                },
                HeadCol::Const(v) => FCol::Const(v.clone()),
            });
        }
        Some(RuleOut { ri: rp.ri, ids: f.ids, head })
    }
}

/// The rules' rows at the bar as the relation's result: a block of columns
/// when they are entity-ascending and each argument is of one kind, its
/// tuples otherwise.
fn assemble(plan: &ColPlan, t: i64, mut outs: Vec<RuleOut>) -> Option<Planned> {
    let width = plan.rules.first().map(|r| r.head.len()).unwrap_or(0);
    // One rule: its rows ascend strictly by entity (the scan's order, which
    // every later operator keeps), and no two are one tuple.
    if outs.len() == 1 {
        let out = outs.pop().expect("one");
        let n = out.ids.len();
        let cols: Vec<Col> = out.head.into_iter().map(|c| c.into_col(n)).collect();
        return Some(Planned::Block(Arc::new(ColBlock::new(t, plan.key_pos, plan.entity_pos, out.ids, cols, RuleTag::One(out.ri as u32)))));
    }
    // Several: the rows rule by rule, a tuple a rule before derived dropped
    // (two tuples are one when their entity and every argument are equal),
    // then in entity order, stably, above `SORTED_MIN`.
    let total: usize = outs.iter().map(|o| o.ids.len()).sum();
    let mut kept: Vec<(usize, usize)> = Vec::with_capacity(total);
    let mut by_id: FxHashMap<u32, Vec<usize>> = FxHashMap::default();
    for (k, out) in outs.iter().enumerate() {
        for (i, &id) in out.ids.iter().enumerate() {
            let same = |&(k2, i2): &(usize, usize)| (0..width).all(|c| outs[k2].head[c].value(i2) == out.head[c].value(i));
            let seen = by_id.entry(id).or_default();
            if outs.len() > 1 && seen.iter().any(|&j| same(&kept[j])) {
                continue;
            }
            seen.push(kept.len());
            kept.push((k, i));
        }
    }
    if kept.len() > super::eval::SORTED_MIN {
        kept.sort_by_key(|&(k, i)| outs[k].ids[i]);
    }
    let ids: Vec<u32> = kept.iter().map(|&(k, i)| outs[k].ids[i]).collect();
    let tags: Vec<u32> = kept.iter().map(|&(k, _)| outs[k].ri as u32).collect();
    if ids.windows(2).all(|w| w[0] <= w[1]) {
        let cols: Vec<Col> = (0..width).map(|c| Col::of_values(kept.iter().map(|&(k, i)| outs[k].head[c].value(i)).collect())).collect();
        let tags = match tags.first() {
            Some(&r) if tags.iter().all(|&x| x == r) => RuleTag::One(r),
            Some(_) => RuleTag::Per(tags),
            None => RuleTag::One(plan.rules.first().map(|r| r.ri as u32).unwrap_or(u32::MAX)),
        };
        return Some(Planned::Block(Arc::new(ColBlock::new(t, plan.key_pos, plan.entity_pos, ids, cols, tags))));
    }
    let arity = width + 2;
    let mut rows = Vec::with_capacity(kept.len());
    for &(k, i) in &kept {
        let mut tu: Tuple = smallvec::SmallVec::with_capacity(arity);
        let mut c = 0;
        for p in 0..arity {
            if p == plan.key_pos {
                tu.push(Value::Time(t));
            } else if p == plan.entity_pos {
                tu.push(Value::Equity(outs[k].ids[i]));
            } else {
                tu.push(outs[k].head[c].value(i));
                c += 1;
            }
        }
        rows.push((tu, outs[k].ri));
    }
    Some(Planned::Rows(rows))
}

/// Answers read as a table as the row path binds them
/// (`Kernel::pushed_answers`): a count's the window's row count, any
/// other's the value.
fn table_answers<'a>(p: &super::pushdown::Pushed, answers: impl Iterator<Item = &'a super::pushdown::Answer>) -> Col {
    if p.count {
        Col::Count(answers.map(|a| a.count).collect())
    } else {
        Col::Num(answers.map(|a| a.value.unwrap_or(f64::NAN)).collect())
    }
}

/// The slot of a variable term of a rule.
fn var_slot(cr: &CompiledRule, t: &Term) -> Option<usize> {
    match t {
        Term::Var(v, _) => cr.slots.get(v).copied(),
        _ => None,
    }
}

/// The columns of the tuples `found` (one per row) at the positions
/// `binds` names, each with its slot.
fn bind_cols(found: &[(u32, Tuple)], binds: &[(usize, usize)]) -> Vec<(usize, Col)> {
    binds
        .iter()
        .map(|&(p, s)| {
            let mut col: Option<Col> = None;
            for (_, tu) in found {
                col.get_or_insert_with(|| Col::like(&tu[p], found.len())).push(&tu[p]);
            }
            (s, col.unwrap_or(Col::Num(Vec::new())))
        })
        .collect()
}

/// A literal as a value without interning anything: a number, a time.
fn literal(l: &crate::ir::Lit) -> Option<Value> {
    use crate::ir::Lit;
    Some(match l {
        Lit::Int(i) => Value::Count(*i),
        Lit::Float(x) | Lit::Quantity(x, _) | Lit::Money(x, _) | Lit::Price(x, _) => Value::Num(*x),
        Lit::Time(t) => Value::Time(*t),
        Lit::Duration(_) | Lit::Str(_) | Lit::Entity(_) | Lit::Label(_) => return None,
    })
}
