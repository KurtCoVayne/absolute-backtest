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
//!
//! The calendar window operator (ERRATA E-171): a window over the bars of
//! the rule's calendar whose conjunction is one as-of read of a stored
//! relation at the window's bar, with tests applied at each call
//! (`dividend_at`). Each entity's per-bar cache is the row path's own
//! (`Kernel::windows`, `eval::WindowCache`), taken, brought to the bar and
//! put back as `window_columns` does; a bar the cache lacks is read from
//! the as-of read of every entity at once (`Kernel::asof_bucket`), and the
//! rows are assembled, tested and reduced as there.

use std::sync::Arc;

use abt_agg::Function;

use super::cexpr::{CExpr, FCol};
use super::col::ColBlock;
use super::colplan::Decline;
use super::eval;
use super::hash::FxHashMap;
use super::{Env, Kernel, MemoKey, Tuple, Value};
use crate::ir::{Cal, CmpOp, Literal};

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

/// A calendar window's aggregate of a rule whose group the frame binds,
/// its conjunction one as-of read of a stored relation at the window's
/// bar with the frame's entity bound (`colplan::ColOp::WindowAgg`).
#[derive(Debug)]
pub(crate) struct WindowAggOp {
    /// The literal's position in the rule's body.
    pub li: usize,
    /// The slot the aggregate binds, and the frame's entity.
    pub out: usize,
    pub eslot: usize,
    /// The stored relation read, the position of its entity, the calendar
    /// whose bucket bounds the read, the pattern's constants (the entity
    /// and the key unbound) and the slot each other variable binds (tuple
    /// position, slot).
    pub rel: usize,
    pub epos: usize,
    pub within: Option<Cal>,
    pub pat: Vec<Option<Value>>,
    pub binds: Vec<(usize, usize)>,
    /// Whether the read takes every key of the bar's bucket (ERRATA E-162)
    /// rather than the latest that holds a tuple.
    pub every: bool,
    /// The aggregate's arguments, and the tests each call applies to the
    /// cached rows, over a row's slots.
    pub args: Vec<CExpr>,
    pub late: Vec<(CmpOp, CExpr, CExpr)>,
}

/// What an as-of read of a stored relation at one bar finds: for every
/// entity at once (`Kernel::asof_bucket`), or to be read entity by entity
/// from the floor.
struct LabelRows {
    t: i64,
    floor: i64,
    each: Option<Vec<(u32, Tuple)>>,
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

    /// What the row path's as-of read of the derived relation `rel` (of
    /// `arity` arguments, on calendar `cal`) finds for entity `e`, given the
    /// latest label at or before the read's time, `first`, and the
    /// relation's columns there when held (`at_first`): the entity's one
    /// tuple at the bar the walk stops at (`Kernel::asof_walk`, from
    /// `first` back to `floor`), or none. The relation is read at each bar
    /// as the row path reads it: from its columns where `cols_of` holds
    /// them, else by the row path's call for the entity, whose answer
    /// behind the horizon is the one the row path gets (E-126). `Decline`
    /// for two tuples of the entity at the bar and for a call that halts.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn asof_derived_tuple(&mut self, rel: usize, cal: &Cal, arity: usize, e: u32, first: i64, floor: i64, at_first: Option<&ColBlock>) -> Result<Option<Tuple>, Decline> {
        let (key_pos, epos) = {
            let info = &self.rels[rel];
            (info.key_pos, info.entity_positions[0])
        };
        let mut pat: Vec<Option<Value>> = vec![None; arity];
        pat[epos] = Some(Value::Equity(e));
        let akey = pat.clone();
        if let Some(tu) = self.entity_tuple_at(rel, &mut pat, key_pos, e, first, at_first)? {
            return Ok(Some(tu));
        }
        let found = self.asof_walk(rel, cal, None, akey, first, floor, |k: &mut Self, at: i64| -> Result<bool, Decline> {
            match k.cols_of(rel, at) {
                Some(b) => Ok(b.find(e).is_some()),
                None => {
                    pat[key_pos] = Some(Value::Time(at));
                    Ok(!k.call(rel, &pat)?.is_empty())
                }
            }
        })?;
        match found {
            Some(k) => {
                let b = self.cols_of(rel, k);
                self.entity_tuple_at(rel, &mut pat, key_pos, e, k, b.as_deref())
            }
            None => Ok(None),
        }
    }

    /// Entity `e`'s one tuple of the derived relation `rel` at bar `at`:
    /// from the relation's columns there when given, else by the row path's
    /// call with the pattern `pat` (the entity bound) keyed at `at`.
    fn entity_tuple_at(&mut self, rel: usize, pat: &mut [Option<Value>], key_pos: usize, e: u32, at: i64, b: Option<&ColBlock>) -> Result<Option<Tuple>, Decline> {
        if let Some(b) = b {
            let Some(j) = b.find(e) else { return Ok(None) };
            if b.ids.get(j + 1) == Some(&e) {
                return Err(Decline);
            }
            return Ok(Some(b.tuple(j)));
        }
        pat[key_pos] = Some(Value::Time(at));
        let d = self.call(rel, pat)?;
        match d.as_slice() {
            [] => Ok(None),
            [(tu, _)] => Ok(Some(tu.clone())),
            _ => Err(Decline),
        }
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

    /// What an as-of read of the stored relation `rel` at `t`, no earlier
    /// than `floor`, finds for every entity, read from the blocks of
    /// `floor..=t` at once: per entity the tuples the pattern `pat` admits
    /// (its entity and key aside) at the latest key that has one, in the
    /// block's order, the entities ascending -- for each entity what
    /// `Kernel::asof_stored_into` finds walking its runs back. `None` where
    /// the walk is the only reading: a read with no floor (which reaches
    /// the rows trimmed away), a block not yet sorted by the entity (the
    /// walk's lookup sorts it), and blocks holding more than `limit`
    /// tuples, where the walk, stopping at the first key an entity has,
    /// reads less.
    pub(crate) fn asof_bucket(&self, rel: usize, epos: usize, pat: &[Option<Value>], floor: i64, t: i64, limit: usize) -> Option<Vec<(u32, Tuple)>> {
        let st = self.store(rel);
        if floor == i64::MIN || self.worker.is_some() || st.entity_pos != Some(epos) || st.dirty.range(floor..=t).next().is_some() {
            return None;
        }
        if st.by_time.range(floor..=t).map(|(_, b)| b.len()).sum::<usize>() > limit {
            return None;
        }
        let admits = |tu: &[Value]| pat.iter().zip(tu.iter()).enumerate().all(|(p, (q, v))| p == epos || q.as_ref().is_none_or(|q| q == v));
        // The key that answers each entity: the latest that holds a tuple
        // of it the pattern admits.
        let mut answered: FxHashMap<u32, i64> = FxHashMap::default();
        let mut out: Vec<(u32, Tuple)> = Vec::new();
        for (&k, block) in st.by_time.range(floor..=t).rev() {
            for tu in block.iter().filter(|tu| admits(tu)) {
                let Value::Equity(e) = tu[epos] else { return None };
                if *answered.entry(e).or_insert(k) == k {
                    out.push((e, smallvec::SmallVec::from(tu)));
                }
            }
        }
        // Stable: an entity's tuples, all of one key, keep their order.
        out.sort_by_key(|(e, _)| *e);
        Some(out)
    }

    /// The as-of read of the stored relation `rel` (`ColOp::AsofStored`)
    /// for the rows `rows` of a frame whose entities are `ids` (ascending)
    /// and whose read times are `at`: per row the one tuple the row path's
    /// read finds (`Kernel::asof_stored_into`), with the row; a row it
    /// finds none for is left out. Rows that share one time read the
    /// relation for every entity at once where it holds few tuples there
    /// (`asof_bucket`). `None` for a row that finds two tuples, which would
    /// be two rows of one entity, and for a time that is not one.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn asof_stored_frame(&mut self, rel: usize, at: &FCol, within: Option<&Cal>, epos: usize, pat: &[Option<Value>], ids: &[u32], rows: &[u32]) -> Option<Vec<(u32, Tuple)>> {
        let mut out: Vec<(u32, Tuple)> = Vec::with_capacity(rows.len());
        if let FCol::Const(Value::Time(t0)) = at {
            let floor = self.asof_floor(within, None, *t0).ok()?;
            if let Some(each) = self.asof_bucket(rel, epos, pat, floor, *t0, rows.len()) {
                let mut j = 0usize;
                for &r in rows {
                    let e = ids[r as usize];
                    while j < each.len() && each[j].0 < e {
                        j += 1;
                    }
                    match each[j..].iter().take_while(|(x, _)| *x == e).count() {
                        0 => {}
                        1 => out.push((r, each[j].1.clone())),
                        _ => return None,
                    }
                }
                return Some(out);
            }
        }
        let mut pat = pat.to_vec();
        let mut found: Vec<(Tuple, usize)> = Vec::with_capacity(1);
        let mut floor: Option<(i64, i64)> = None;
        for &r in rows {
            let Value::Time(t0) = at.value(r as usize) else { return None };
            let lo = match floor {
                Some((t, lo)) if t == t0 => lo,
                _ => {
                    let lo = self.asof_floor(within, None, t0).ok()?;
                    floor = Some((t0, lo));
                    lo
                }
            };
            pat[epos] = Some(Value::Equity(ids[r as usize]));
            found.clear();
            self.asof_stored_into(rel, lo, t0, &pat, &mut found);
            match found.as_mut_slice() {
                [] => {}
                [(tu, _)] => out.push((r, std::mem::take(tu))),
                _ => return None,
            }
        }
        Some(out)
    }

    /// The calendar window's aggregate at bar `t` for each row of the frame
    /// (`ids`, the entities, ascending; `cols`, the frame's slots): its
    /// value, or `None` where the row has no tuple. Per entity, as
    /// `Kernel::window_columns` serves a call: the group's cache taken from
    /// `Kernel::windows`, the window's bars it lacks read and kept (each
    /// counted as solved), the rows of the window's bars in time order, those
    /// of one bar in the order of the bindings the call leaves unbound, each
    /// tested; the cache's bars before the window forgotten and the cache put
    /// back; the rows reduced by the row path's function. `None` (the bar
    /// declined) where the row path would solve the window whole or halt:
    /// what the operator has done to the caches by then is what the row
    /// path's own calls would have done, so that they go on from there.
    /// `calls` counts the window calls answered.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn run_window_agg(&mut self, ri: usize, op: &WindowAggOp, ids: &[u32], cols: &[Option<FCol>], t: i64, calls: &mut usize) -> Option<Vec<Option<Value>>> {
        let prog = self.prog;
        let cr = self.compiled[ri].clone();
        let lit = &prog.rules[ri].body[op.li];
        let Literal::Agg { agg, args, conj, within, .. } = lit else { return None };
        if !self.cfg.window_cache || self.pushed.contains_key(&(ri, op.li)) {
            return None;
        }
        let plan = self.agg_plan_at(&cr, Some(op.li), lit, conj, args, within.as_deref());
        let wp = plan.window.as_ref()?;
        // A call's binding: the frame's constants, the entity and the
        // slots each row binds; the rest unbound.
        let mut env: Env = vec![None; cr.nslots];
        let mut per_row: Vec<usize> = Vec::new();
        for (s, c) in cols.iter().enumerate() {
            match c {
                Some(FCol::Const(v)) => env[s] = Some(v.clone()),
                Some(FCol::Col(_)) => per_row.push(s),
                None => {}
            }
        }
        let unbound: Vec<usize> = (0..cr.nslots).filter(|&s| s != op.eslot && cols[s].is_none()).collect();
        let is_unbound = |s: &usize| unbound.binary_search(s).is_ok();
        // A group the call does not bind, a window variable it binds, or a
        // test reading what it leaves unbound: the row path solves the
        // window whole.
        if plan.group.iter().any(is_unbound) || !is_unbound(&wp.wslot) || wp.late.iter().any(|(_, need)| need.iter().any(is_unbound)) {
            return None;
        }
        let mut min_count: i64 = 0;
        for l in conj {
            if let Literal::Window { min, .. } = l {
                let k = self.eval_expr(&cr, min, &env).ok()?.as_f64().unwrap_or(0.0) as i64;
                min_count = min_count.max(k);
            }
        }
        let Literal::Window { kind, base, dur, lo, .. } = &conj[wp.window] else { return None };
        if self.per_entity_window(kind) {
            return None;
        }
        let times: Vec<i64> = self.window_times(&cr, kind, base, dur, lo.as_ref(), &env).ok()?;
        let lo = times.first().copied().unwrap_or(t);
        let counted = agg == "count";
        let mut labels: Vec<LabelRows> = Vec::new();
        let mut pat = op.pat.clone();
        let mut found: Vec<(Tuple, usize)> = Vec::new();
        let mut tested = env.clone();
        let mut missing: Vec<i64> = Vec::new();
        let mut acc: Vec<Vec<f64>> = vec![Vec::new(); if counted { 0 } else { args.len() }];
        let mut out: Vec<Option<Value>> = Vec::with_capacity(ids.len());
        for (i, &e) in ids.iter().enumerate() {
            for &s in &per_row {
                env[s] = cols[s].as_ref().map(|c| c.value(i));
                tested[s] = env[s].clone();
            }
            env[op.eslot] = Some(Value::Equity(e));
            tested[op.eslot] = env[op.eslot].clone();
            let key = eval::WindowKey {
                rule: cr.idx,
                literal: wp.literal,
                outer: wp.outer.iter().map(|&s| env[s].clone()).collect(),
            };
            let mut cache = self.windows.remove(&key).unwrap_or_default();
            // The window's bars the cache does not hold, in order.
            missing.clear();
            {
                let mut have = cache.range(lo, t).map(|(k, _)| *k).peekable();
                for &t1 in &times {
                    while have.peek().is_some_and(|&k| k < t1) {
                        have.next();
                    }
                    if have.peek() == Some(&t1) {
                        have.next();
                    } else {
                        missing.push(t1);
                    }
                }
            }
            for &t1 in &missing {
                match self.window_bar_rows(op, &mut labels, &mut pat, &mut found, &env, wp.wslot, counted, ids.len(), e, t1) {
                    Some(rows) => {
                        self.stats.window_bars_solved += 1;
                        cache.insert(t1, rows);
                    }
                    // An argument the row path cannot evaluate: its own call
                    // solves this bar again and meets it.
                    None => {
                        self.windows.insert(key, cache);
                        return None;
                    }
                }
            }
            let mut count = 0usize;
            acc.iter_mut().for_each(Vec::clear);
            let mut halted = false;
            {
                let mut held = cache.range(lo, t).peekable();
                'bars: for &t1 in &times {
                    while held.peek().is_some_and(|(k, _)| *k < t1) {
                        held.next();
                    }
                    let Some((_, bar)) = held.next_if(|(k, _)| *k == t1) else { continue };
                    let mut order: Vec<usize> = (0..bar.len()).collect();
                    if bar.len() > 1 {
                        order.sort_by(|&a, &b| unbound.iter().map(|&s| bar[a].0[s].cmp(&bar[b].0[s])).find(|o| o.is_ne()).unwrap_or(std::cmp::Ordering::Equal));
                    }
                    'rows: for k in order {
                        let (row, vals) = &bar[k];
                        if !op.late.is_empty() {
                            for &s in &unbound {
                                tested[s] = row[s].clone();
                            }
                            for (cmp, lhs, rhs) in &op.late {
                                let (Some(a), Some(b)) = (lhs.value_in(&tested), rhs.value_in(&tested)) else {
                                    halted = true;
                                    break 'bars;
                                };
                                if !eval::compare(*cmp, &a, &b) {
                                    continue 'rows;
                                }
                            }
                        }
                        count += 1;
                        for (c, v) in acc.iter_mut().zip(vals) {
                            c.push(*v);
                        }
                    }
                }
            }
            // A test the row path cannot evaluate: its call solves the
            // window whole.
            if halted {
                self.windows.insert(key, cache);
                return None;
            }
            cache.forget_before(lo);
            self.windows.insert(key, cache);
            *calls += 1;
            if (count as i64) < min_count {
                out.push(None);
                continue;
            }
            if counted {
                out.push(Some(Value::Count(count as i64)));
                continue;
            }
            let slices: Vec<&[f64]> = acc.iter().map(|c| c.as_slice()).collect();
            match eval::rows_reduce(agg, plan.f, &slices) {
                Ok(v) => out.push(v),
                // The reduction halts: the row path raises it.
                Err(_) => return None,
            }
        }
        Some(out)
    }

    /// `ColOp::WindowAgg` for the whole frame in one walk of the stored
    /// relation (ERRATA E-165): the window's bars in order, each bar's
    /// bucket read once for every entity -- the tuples of an entity's
    /// latest key there, or with `every` each distinct tuple of the bucket
    /// whatever its key, as `Kernel::asof_matches` reads them -- and each
    /// tuple found given to its entity's row of the frame, where it is
    /// bound, its arguments computed, the bar's rows put in the order of
    /// their own bindings and tested as `window_columns` does. No entity's
    /// cache is read or written: what a bar costs is the tuples the
    /// relation holds in the window, not the frame's entities times the
    /// window's bars. `None` where the walk cannot say what the rules
    /// would: a frame with two rows of one entity, a worker, a read with
    /// no floor or reaching behind the trimmed horizon, a block not sorted
    /// by the read's entity, an argument or a test without a value.
    pub(crate) fn run_window_frame(&mut self, ri: usize, op: &WindowAggOp, ids: &[u32], cols: &[Option<FCol>], calls: &mut usize) -> Option<Vec<Option<Value>>> {
        let prog = self.prog;
        let cr = self.compiled[ri].clone();
        let lit = &prog.rules[ri].body[op.li];
        let Literal::Agg { agg, args, conj, within, .. } = lit else { return None };
        if !self.cfg.window_cache || self.worker.is_some() || self.pushed.contains_key(&(ri, op.li)) {
            return None;
        }
        if !ids.windows(2).all(|w| w[0] < w[1]) || self.store(op.rel).entity_pos != Some(op.epos) {
            return None;
        }
        let plan = self.agg_plan_at(&cr, Some(op.li), lit, conj, args, within.as_deref());
        let wp = plan.window.as_ref()?;
        let mut env: Env = vec![None; cr.nslots];
        let mut per_row: Vec<usize> = Vec::new();
        for (s, c) in cols.iter().enumerate() {
            match c {
                Some(FCol::Const(v)) => env[s] = Some(v.clone()),
                Some(FCol::Col(_)) => per_row.push(s),
                None => {}
            }
        }
        let unbound: Vec<usize> = (0..cr.nslots).filter(|&s| s != op.eslot && cols[s].is_none()).collect();
        let is_unbound = |s: &usize| unbound.binary_search(s).is_ok();
        if plan.group.iter().any(is_unbound) || !is_unbound(&wp.wslot) || wp.late.iter().any(|(_, need)| need.iter().any(is_unbound)) {
            return None;
        }
        let mut min_count: i64 = 0;
        for l in conj {
            if let Literal::Window { min, .. } = l {
                let k = self.eval_expr(&cr, min, &env).ok()?.as_f64().unwrap_or(0.0) as i64;
                min_count = min_count.max(k);
            }
        }
        let Literal::Window { kind, base, dur, lo, .. } = &conj[wp.window] else { return None };
        if self.per_entity_window(kind) {
            return None;
        }
        let times: Vec<i64> = self.window_times(&cr, kind, base, dur, lo.as_ref(), &env).ok()?;
        let counted = agg == "count";
        let nargs = if counted { 0 } else { op.args.len() };
        let every = op.every && op.within.is_some();
        let kp = self.rels[op.rel].key_pos;
        let admits = |tu: &[Value]| op.pat.iter().zip(tu.iter()).enumerate().all(|(p, (q, v))| p == op.epos || q.as_ref().is_none_or(|q| q == v));
        let mut counts: Vec<usize> = vec![0; ids.len()];
        let mut acc: Vec<Vec<f64>> = vec![Vec::new(); ids.len() * nargs];
        // A bar's tuples, each with its row of the frame; the rows of one
        // entity at that bar, bound.
        let mut bar: Vec<(u32, Tuple)> = Vec::new();
        let mut keys: Vec<i64> = Vec::new();
        let mut answered: FxHashMap<u32, i64> = FxHashMap::default();
        let mut rows: Vec<(Env, smallvec::SmallVec<[f64; 2]>)> = Vec::new();
        for &t1 in &times {
            let floor = self.asof_floor(op.within.as_ref(), None, t1).ok()?;
            if floor == i64::MIN || floor < self.horizon_floor {
                return None;
            }
            keys.clear();
            keys.extend(self.store(op.rel).by_time.range(floor..=t1).map(|(k, _)| *k));
            bar.clear();
            if every {
                // Every key in time order.
                for &k in &keys {
                    for tu in self.store_mut(op.rel).lookup(k, None).iter().filter(|tu| admits(tu)) {
                        let Value::Equity(e) = tu[op.epos] else { return None };
                        if let Ok(i) = ids.binary_search(&e) {
                            bar.push((i as u32, smallvec::SmallVec::from(tu)));
                        }
                    }
                }
            } else {
                // The latest key that holds a tuple of the entity.
                answered.clear();
                for &k in keys.iter().rev() {
                    for tu in self.store_mut(op.rel).lookup(k, None).iter().filter(|tu| admits(tu)) {
                        let Value::Equity(e) = tu[op.epos] else { return None };
                        if *answered.entry(e).or_insert(k) == k {
                            if let Ok(i) = ids.binary_search(&e) {
                                bar.push((i as u32, smallvec::SmallVec::from(tu)));
                            }
                        }
                    }
                }
            }
            if bar.is_empty() {
                continue;
            }
            // Stable: an entity's tuples keep the order they were read in.
            bar.sort_by_key(|(i, _)| *i);
            let mut a = 0;
            while a < bar.len() {
                let i = bar[a].0 as usize;
                let b = a + bar[a..].iter().take_while(|(j, _)| *j as usize == i).count();
                for &s in &per_row {
                    env[s] = cols[s].as_ref().map(|c| c.value(i));
                }
                env[op.eslot] = Some(Value::Equity(ids[i]));
                rows.clear();
                for x in a..b {
                    let tu = &bar[x].1;
                    // The key is no term of a read of every key: two tuples
                    // equal but for it are one row.
                    if every
                        && bar[a..x]
                            .iter()
                            .any(|(_, f)| f.len() == tu.len() && f.iter().zip(tu.iter()).enumerate().all(|(p, (u, v))| p == kp || u == v))
                    {
                        continue;
                    }
                    let mut row = env.clone();
                    row[wp.wslot] = Some(Value::Time(t1));
                    for &(p, s) in &op.binds {
                        row[s] = Some(tu[p].clone());
                    }
                    let vals: smallvec::SmallVec<[f64; 2]> = if counted {
                        smallvec::SmallVec::new()
                    } else {
                        op.args.iter().map(|x| x.value_in(&row).and_then(|v| v.as_f64())).collect::<Option<_>>()?
                    };
                    rows.push((row, vals));
                }
                a = b;
                if rows.len() > 1 {
                    rows.sort_by(|x, y| unbound.iter().map(|&s| x.0[s].cmp(&y.0[s])).find(|o| o.is_ne()).unwrap_or(std::cmp::Ordering::Equal));
                }
                'rows: for (row, vals) in rows.iter() {
                    for (cmp, lhs, rhs) in &op.late {
                        let (Some(l), Some(r)) = (lhs.value_in(row), rhs.value_in(row)) else { return None };
                        if !eval::compare(*cmp, &l, &r) {
                            continue 'rows;
                        }
                    }
                    counts[i] += 1;
                    for (c, v) in vals.iter().enumerate() {
                        acc[i * nargs + c].push(*v);
                    }
                }
            }
        }
        let mut out: Vec<Option<Value>> = Vec::with_capacity(ids.len());
        for (i, &count) in counts.iter().enumerate() {
            if (count as i64) < min_count {
                out.push(None);
            } else if counted {
                out.push(Some(Value::Count(count as i64)));
            } else {
                let slices: Vec<&[f64]> = acc[i * nargs..(i + 1) * nargs].iter().map(|c| c.as_slice()).collect();
                // The reduction halts: the row path raises it.
                out.push(eval::rows_reduce(agg, plan.f, &slices).ok()?);
            }
        }
        *calls += ids.len();
        Some(out)
    }

    /// The rows of the window's bar `t1` for entity `e`, as the row path's
    /// solve of the cached conjunction gives them: the call's binding `env`
    /// with the window's variable at `t1` and the read's tuples bound, each
    /// with the aggregate's arguments evaluated (none for a count). The
    /// read at `t1` is made once for every entity of the frame (`labels`),
    /// or entity by entity. `None` where an argument is partial or not a
    /// number.
    #[allow(clippy::too_many_arguments)]
    fn window_bar_rows(
        &mut self,
        op: &WindowAggOp,
        labels: &mut Vec<LabelRows>,
        pat: &mut [Option<Value>],
        found: &mut Vec<(Tuple, usize)>,
        env: &Env,
        wslot: usize,
        counted: bool,
        limit: usize,
        e: u32,
        t1: i64,
    ) -> Option<eval::WindowRows> {
        let at = match labels.iter().position(|l| l.t == t1) {
            Some(at) => at,
            None => {
                let floor = self.asof_floor(op.within.as_ref(), None, t1).ok()?;
                let each = self.asof_bucket(op.rel, op.epos, pat, floor, t1, limit);
                labels.push(LabelRows { t: t1, floor, each });
                labels.len() - 1
            }
        };
        found.clear();
        match &labels[at].each {
            Some(each) => {
                let lo = each.partition_point(|(x, _)| *x < e);
                found.extend(each[lo..].iter().take_while(|(x, _)| *x == e).map(|(_, tu)| (tu.clone(), usize::MAX)));
            }
            None => {
                pat[op.epos] = Some(Value::Equity(e));
                let floor = labels[at].floor;
                self.asof_stored_into(op.rel, floor, t1, pat, found);
            }
        }
        let mut rows: eval::WindowRows = Vec::with_capacity(found.len());
        for (tu, _) in found.iter() {
            let mut row = env.clone();
            row[wslot] = Some(Value::Time(t1));
            for &(p, s) in &op.binds {
                row[s] = Some(tu[p].clone());
            }
            let vals: Vec<f64> = if counted {
                Vec::new()
            } else {
                op.args.iter().map(|a| a.value_in(&row).and_then(|v| v.as_f64())).collect::<Option<Vec<f64>>>()?
            };
            rows.push((row, vals));
        }
        Some(rows)
    }
}
