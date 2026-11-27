//! The block evaluator's window operator (ERRATA E-151): a `rows[n]`
//! aggregate whose group is the rule's entity, over every entity of the
//! frame at once. Each entity's window state is the one the row path keeps
//! (`eval::EntRows`), brought to the bar by the same functions
//! (`RowsState::feed`, `window`, `trim`, `Kernel::rows_serve`); what
//! changes is how a bar the state lacks is read: the conjunction's
//! relations at that bar as columns (`Kernel::cols_of`), each entity found
//! in them, its arguments computed with the row path's arithmetic
//! (`CExpr::value_in`), instead of the conjunction solved row by row.
//!
//! Where the columns cannot say what the row path would -- a bar behind
//! the horizon, a relation not one tuple per entity there, a partial
//! operation on the entity's row, an entity with no state yet (its walk
//! back is the row path's), a request behind the state's last bar, a
//! window the state does not hold -- the operator runs the row path's own
//! step for that entity (`Kernel::step`), which feeds the same state from
//! where the columns left it. A step that halts (an instance the row path
//! leaves stuck) declines the bar; the outcomes of the steps already run
//! are kept for `derive_rules` (`Kernel::rows_replay`), which then meets
//! each as it was instead of running it twice, so that the bar's counts
//! are the row path's.

use std::sync::Arc;

use abt_agg::Function;

use super::cexpr::{CExpr, FCol};
use super::col::ColBlock;
use super::eval;
use super::{Env, Kernel, MemoKey, Value};
use crate::ir::Literal;

/// A `rows[n]` aggregate of a rule whose group is the entity the frame
/// binds (`colplan::ColOp::RowsAgg`).
#[derive(Debug)]
pub(crate) struct RowsAggOp {
    /// The literal's position in the rule's body.
    pub li: usize,
    /// The slot the aggregate binds, and the frame's entity.
    pub out: usize,
    pub eslot: usize,
    /// The aggregate's function, parsed once (`None` for `count`).
    pub f: Option<Function>,
    /// The conjunction's atoms in its solve order, the anchor first.
    pub atoms: Vec<ConjAtom>,
    /// The aggregate's arguments over the atoms' slots and the frame's.
    pub args: Vec<CExpr>,
    /// The frame's slots the arguments read.
    pub frame_slots: Vec<usize>,
}

/// A window step the operator ran on the row path at a bar it then
/// declined: the rule, the literal, the binding, and the step's outcome
/// (`Kernel::rows_replay`).
pub(crate) type Replayed = (usize, usize, Env, Result<Vec<Env>, super::RunError>);

/// An atom of the window's conjunction: a derived relation at the
/// window's bar with the entity bound, each of its other arguments but
/// the key binding a fresh slot or none.
#[derive(Debug)]
pub(crate) struct ConjAtom {
    pub rel: usize,
    pub binds: Vec<Option<usize>>,
}

/// The conjunction's relations at one bar, each fetched when an entity
/// first reaches it (as the row path demands it; the operator's own bar
/// before any entity over a large frame), with a cursor: the entities
/// come in ascending order.
struct BarCols {
    t: i64,
    blocks: Vec<Option<Option<Arc<ColBlock>>>>,
    cursors: Vec<usize>,
}

/// What the columns say of an entity at a bar.
enum Feed {
    /// Whether the anchor holds, and the row's arguments where the whole
    /// conjunction does.
    Row(bool, Option<smallvec::SmallVec<[f64; 2]>>),
    /// The row path must read the bar.
    RowPath,
    /// The bar's own relations are not columns: no entity is read so.
    Decline,
}

impl<'p> Kernel<'p> {
    /// The aggregate at bar `t` for each row of the frame (`ids`, the
    /// entities, ascending; `cols`, the frame's slots): its value, or
    /// `None` where the row has no tuple (fewer rows than the minimum, an
    /// empty reduction). `None` when the operator declines the bar; `calls`
    /// counts the window calls the columns answered.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn run_rows_agg(&mut self, ri: usize, op: &RowsAggOp, ids: &[u32], cols: &[Option<FCol>], eslot: usize, t: i64, calls: &mut usize) -> Option<Vec<Option<Value>>> {
        let prog = self.prog;
        let cr = self.compiled[ri].clone();
        let lit = &prog.rules[ri].body[op.li];
        let Literal::Agg { agg, args, conj, .. } = lit else { return None };
        if self.pushed.contains_key(&(ri, op.li)) || prog.infos[ri].cal.per_entity() {
            return None;
        }
        let plan = self.rows_plan(&cr, lit, conj, args).ok()?;
        let ent = plan.ent.filter(|_| plan.shareable && !plan.prior && plan.anchor_first)?;
        // The group's outer values other than the entity: the frame's
        // constants, or unbound; a state is kept dense only for those of
        // the group's first call, as the row path keeps it.
        let mut rest: Vec<Option<Value>> = Vec::with_capacity(plan.outer.len());
        for (i, &s) in plan.outer.iter().enumerate() {
            if i == plan.ent_pos {
                continue;
            }
            match &cols[s] {
                None => rest.push(None),
                Some(FCol::Const(v)) => rest.push(Some(v.clone())),
                Some(FCol::Col(_)) => return None,
            }
        }
        if !self.ent_rows[ent].admits(rest.into_iter()) {
            return None;
        }
        // The constants of the frame: what the window's count and minimum,
        // and the arguments, read besides the rows.
        let mut base: Env = vec![None; cr.nslots];
        for (s, c) in cols.iter().enumerate() {
            if let Some(FCol::Const(v)) = c {
                base[s] = Some(v.clone());
            }
        }
        let n = self.eval_expr(&cr, &plan.dur, &base).ok()?.as_f64().unwrap_or(0.0).max(0.0) as usize;
        let mut min_count: i64 = 0;
        for l in conj {
            if let Literal::Window { min, .. } = l {
                let k = self.eval_expr(&cr, min, &base).ok()?.as_f64().unwrap_or(0.0) as i64;
                min_count = min_count.max(k);
            }
        }
        // A relation of the conjunction that left an instance stuck or a
        // halt at this bar: the row path solves it there, over its block.
        for a in &op.atoms {
            let key: MemoKey = (a.rel, vec![Value::Time(t)]);
            if self.memo_stuck.contains_key(&key) || self.memo_errors.contains_key(&key) {
                return None;
            }
        }
        let cal = prog.infos[ri].cal.clone();
        let mut bars: Vec<BarCols> = Vec::new();
        // Over a frame this large the row path solves the conjunction at
        // the bar over the whole block before it brings any entity over
        // older bars (`eval::prefetch_rows_window`), deriving the anchor's
        // relation there, then each later atom's while an entity of the
        // frame is in the ones before. They are read here in that order,
        // so that a relation of the conjunction with a window of its own
        // is asked at the older bars after this one, behind its groups'
        // last bar, as there. A bar whose own relations are not columns
        // declines before any entity is read.
        if ids.len() >= eval::PREFETCH_MIN_ROWS {
            let mut bar = BarCols {
                t,
                blocks: vec![None; op.atoms.len()],
                cursors: vec![0; op.atoms.len()],
            };
            let mut alive: Vec<u32> = ids.to_vec();
            for (k, atom) in op.atoms.iter().enumerate() {
                if alive.is_empty() {
                    break;
                }
                let b = self.cols_of(atom.rel, t).filter(|b| b.unique)?;
                let mut j = 0usize;
                alive.retain(|&e| {
                    while j < b.ids.len() && b.ids[j] < e {
                        j += 1;
                    }
                    j < b.ids.len() && b.ids[j] == e
                });
                bar.blocks[k] = Some(Some(b));
            }
            bars.push(bar);
        }
        let mut fresh: Vec<i64> = Vec::new();
        let mut out: Vec<Option<Value>> = Vec::with_capacity(ids.len());
        let mut row_env = base.clone();
        for (i, &e) in ids.iter().enumerate() {
            let taken = if e < eval::ENT_ROWS_MAX { self.ent_rows[ent].take(e) } else { None };
            // The columns answer a group the row path has fed, brought to
            // the bar forward.
            let mut st = match taken {
                Some(st) if !st.is_new() && st.upto <= t => st,
                other => {
                    if let Some(st) = other {
                        self.ent_rows[ent].put(e, st);
                    }
                    out.push(self.rows_agg_step(ri, op, ids, cols, eslot, i)?);
                    continue;
                }
            };
            for &s in &op.frame_slots {
                row_env[s] = cols[s].as_ref().map(|c| c.value(i));
            }
            row_env[eslot] = Some(Value::Equity(e));
            fresh.clear();
            match self.domain_for(&cal, None) {
                Ok(dom) => fresh.extend(dom.range((std::ops::Bound::Excluded(st.upto), std::ops::Bound::Included(t))).copied()),
                Err(_) => {
                    self.ent_rows[ent].put(e, st);
                    return None;
                }
            }
            let mut read = true;
            for &t1 in &fresh {
                match self.feed_from_cols(op, &mut bars, &mut row_env, e, t1, t) {
                    Feed::Row(anchored, vals) => {
                        st.feed(t1, anchored, vals.as_deref());
                        st.upto = t1;
                        self.stats.window_bars_solved += 1;
                    }
                    Feed::RowPath => {
                        read = false;
                        break;
                    }
                    Feed::Decline => {
                        self.ent_rows[ent].put(e, st);
                        return None;
                    }
                }
            }
            if !read {
                // The row path's step goes on from the bar the columns
                // stopped at.
                self.ent_rows[ent].put(e, st);
                out.push(self.rows_agg_step(ri, op, ids, cols, eslot, i)?);
                continue;
            }
            st.upto = st.upto.max(t);
            match self.rows_serve(&cr, &plan, &mut st, &base, t, n, agg, op.f, args.len(), min_count) {
                Ok(Some((_, Ok(value)))) => {
                    st.trim(n);
                    self.ent_rows[ent].put(e, st);
                    *calls += 1;
                    out.push(value);
                }
                // A window the buffer does not hold (the general path's),
                // or a reduction that halts: the row path's step serves it
                // from the state as it is.
                _ => {
                    self.ent_rows[ent].put(e, st);
                    out.push(self.rows_agg_step(ri, op, ids, cols, eslot, i)?);
                }
            }
        }
        Some(out)
    }

    /// Bar `t1` of the window for entity `e` read from the columns of the
    /// conjunction's relations there: the anchor's block, then each later
    /// atom's while the entity is in the ones before (the relations the
    /// row path would ask), the arguments on the entity's row. `t` is the
    /// bar the operator runs at.
    fn feed_from_cols(&mut self, op: &RowsAggOp, bars: &mut Vec<BarCols>, row_env: &mut Env, e: u32, t1: i64, t: i64) -> Feed {
        if !self.columns_at(t1) {
            return Feed::RowPath;
        }
        let at = match bars.iter().position(|b| b.t == t1) {
            Some(at) => at,
            None => {
                bars.push(BarCols {
                    t: t1,
                    blocks: vec![None; op.atoms.len()],
                    cursors: vec![0; op.atoms.len()],
                });
                bars.len() - 1
            }
        };
        let unread = if t1 == t { Feed::Decline } else { Feed::RowPath };
        for (k, atom) in op.atoms.iter().enumerate() {
            if bars[at].blocks[k].is_none() {
                let b = self.cols_of(atom.rel, t1).filter(|b| b.unique);
                bars[at].blocks[k] = Some(b);
            }
            let bar = &mut bars[at];
            let Some(Some(b)) = &bar.blocks[k] else { return unread };
            let cur = &mut bar.cursors[k];
            while *cur < b.ids.len() && b.ids[*cur] < e {
                *cur += 1;
            }
            if *cur >= b.ids.len() || b.ids[*cur] != e {
                // The anchor does not hold, or the conjunction does not.
                return Feed::Row(k > 0, None);
            }
            for (c, bind) in b.cols.iter().zip(&atom.binds) {
                if let Some(s) = bind {
                    row_env[*s] = Some(c.value(*cur));
                }
            }
        }
        let mut vals = smallvec::SmallVec::new();
        for a in &op.args {
            match a.value_in(row_env).and_then(|v| v.as_f64()) {
                Some(x) => vals.push(x),
                // A partial operation, or an argument that is not a number:
                // the row path raises it.
                None => return Feed::RowPath,
            }
        }
        Feed::Row(true, Some(vals))
    }

    /// The row path's step of the aggregate for row `i` of the frame, its
    /// outcome kept for `derive_rules` should the bar be declined: the
    /// aggregate's value, `None` for no tuple. `None` outright (the bar
    /// declined) when the step halts, which the row path then meets.
    fn rows_agg_step(&mut self, ri: usize, op: &RowsAggOp, ids: &[u32], cols: &[Option<FCol>], eslot: usize, i: usize) -> Option<Option<Value>> {
        let prog = self.prog;
        let cr = self.compiled[ri].clone();
        let lit = &prog.rules[ri].body[op.li];
        let mut env: Env = cols.iter().map(|c| c.as_ref().map(|c| c.value(i))).collect();
        env[eslot] = Some(Value::Equity(ids[i]));
        let mut rows: Vec<Env> = Vec::new();
        self.row_steps[ri] += 1;
        let outcome = self.step(&cr, lit, &env, &mut rows).map(|()| rows);
        let value = match &outcome {
            Ok(rows) if rows.len() <= 1 => Some(rows.first().and_then(|r| r[op.out].clone())),
            _ => None,
        };
        self.rows_replay.push((ri, op.li, env, outcome));
        value
    }
}
