//! Rule evaluation: a body is solved literal by literal in the plan's order
//! (the written order without a plan), under Kleene's three-valued
//! conjunction (language-v2, section 5), each literal mapping a set of
//! partial bindings to a larger or smaller set. Derived relations are
//! requested through `call`, which memoises by temporal key.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use abt_agg::{Function, ShiftedSums};

use super::cexpr;
use super::col;
use super::colplan;
use super::eval_set::{Block, Sink};
use super::time;
use super::value::{Ctor, Decision, Order, OrderKind, Tif, Value};
use super::{CompiledRule, Env, Kernel, RunError, Sym, Tuple};
use crate::ir::*;

/// The rows of one bar of a windowed group, each with the aggregate's
/// arguments evaluated on it.
pub type WindowRows = Vec<(Env, Vec<f64>)>;

/// A group's cached bars in time order. A call usually adds the newest bar
/// and forgets the oldest, and reads the rest in order: a deque, searched
/// by bisection, rather than a tree.
#[derive(Clone, Debug, Default)]
pub struct WindowCache {
    bars: std::collections::VecDeque<(i64, WindowRows)>,
}

impl WindowCache {
    pub fn new() -> WindowCache {
        WindowCache::default()
    }

    pub fn len(&self) -> usize {
        self.bars.len()
    }

    pub fn is_empty(&self) -> bool {
        self.bars.is_empty()
    }

    pub fn contains_key(&self, t: &i64) -> bool {
        self.bars.binary_search_by_key(t, |(k, _)| *k).is_ok()
    }

    pub fn get(&self, t: &i64) -> Option<&WindowRows> {
        self.bars.binary_search_by_key(t, |(k, _)| *k).ok().map(|i| &self.bars[i].1)
    }

    /// The bar's rows, replacing any held.
    pub fn insert(&mut self, t: i64, rows: WindowRows) {
        if self.bars.back().is_none_or(|(k, _)| *k < t) {
            self.bars.push_back((t, rows));
        } else if self.bars.front().is_some_and(|(k, _)| *k > t) {
            self.bars.push_front((t, rows));
        } else {
            match self.bars.binary_search_by_key(&t, |(k, _)| *k) {
                Ok(i) => self.bars[i].1 = rows,
                Err(i) => self.bars.insert(i, (t, rows)),
            }
        }
    }

    /// The bars from `lo` to `hi`, both included, in time order.
    pub fn range(&self, lo: i64, hi: i64) -> std::collections::vec_deque::Iter<'_, (i64, WindowRows)> {
        let a = self.bars.partition_point(|(k, _)| *k < lo);
        let b = self.bars.partition_point(|(k, _)| *k <= hi).max(a);
        self.bars.range(a..b)
    }

    /// Forget the bars before `lo`.
    pub fn forget_before(&mut self, lo: i64) {
        let a = self.bars.partition_point(|(k, _)| *k < lo);
        self.bars.drain(..a);
    }

    pub fn clear(&mut self) {
        self.bars.clear();
    }

    pub fn iter(&self) -> std::collections::vec_deque::Iter<'_, (i64, WindowRows)> {
        self.bars.iter()
    }
}

impl std::ops::Index<&i64> for WindowCache {
    type Output = WindowRows;
    fn index(&self, t: &i64) -> &WindowRows {
        self.get(t).expect("a cached bar")
    }
}

impl FromIterator<(i64, WindowRows)> for WindowCache {
    fn from_iter<I: IntoIterator<Item = (i64, WindowRows)>>(it: I) -> WindowCache {
        let mut c = WindowCache::new();
        for (t, rows) in it {
            c.insert(t, rows);
        }
        c
    }
}

impl IntoIterator for WindowCache {
    type Item = (i64, WindowRows);
    type IntoIter = std::collections::vec_deque::IntoIter<(i64, WindowRows)>;
    fn into_iter(self) -> Self::IntoIter {
        self.bars.into_iter()
    }
}

/// The static shape of a rows window literal.
/// A rows window's conjunction solved at one bar for every row of a block
/// at once (06-performance 3.2, "window states advanced for the slots
/// present"): per row, whether the anchor held and the solutions, or `None`
/// where an instance stuck (the row-at-a-time path places the halt).
#[derive(Clone, Debug)]
pub(crate) struct Prefetch {
    rule: usize,
    literal: usize,
    t: i64,
    rows: Vec<Option<(bool, Vec<Env>)>>,
}

/// A bucket or span window's conjunction solved at the bars every row of a
/// block misses, for all rows at once (3.2): per row and bar, the
/// solutions, or `None` for a row on which an instance stuck.
#[derive(Clone, Debug)]
pub(crate) struct PrefetchBucket {
    rule: usize,
    literal: usize,
    bars: Vec<i64>,
    rows: Vec<Option<Vec<Vec<Env>>>>,
}

#[derive(Debug)]
pub(crate) struct RowsPlan {
    pub(crate) literal: usize,
    wslot: usize,
    base: Term,
    pub(crate) dur: Expr,
    pub(crate) outer: Vec<usize>,
    pub(crate) shareable: bool,
    /// `prior_rows`: the bar itself is excluded.
    pub(crate) prior: bool,
    order: Vec<Literal>,
    anchor: Vec<Literal>,
    /// The anchor is the first literal of `order`, so a block solved past
    /// it is the conjunction's block after its first literal; and it has no
    /// constructor pattern, since `call` counts each time a stored relation
    /// is asked through it (the coverage) and the anchor would be asked
    /// once where it is asked twice.
    pub(crate) anchor_first: bool,
    /// The derived relations the conjunction reads, when a block solve of
    /// it is a pure reading wherever each of them is memoised without a
    /// stuck instance at the window's bar: every literal a positive atom
    /// without a constructor pattern (nothing counted in the coverage,
    /// nothing that sticks), a derived one keyed at the window's time, the
    /// last literal a comparison or an assignment at most (an instance that
    /// sticks there meets no later literal to be excluded by, which is
    /// counted). `None` for any other conjunction. `prefetch_rows_window`
    /// leaves such a solve out when no row would read it.
    replayable: Option<Vec<usize>>,
    /// The group's states dense by entity (`Kernel::ent_rows`), when the
    /// outer bindings hold the rule's entity (at `ent_pos` of `outer`) and
    /// the window is shared across calls.
    pub(crate) ent: Option<usize>,
    pub(crate) ent_pos: usize,
}

/// What an aggregation literal's calls share, computed once per (rule,
/// literal): the slots of its group and, when a call can be served from the
/// per-bar cache (`window_columns`), the cached window's shape.
#[derive(Debug)]
pub(crate) struct AggPlan {
    /// The group's variables as slots, ascending: those of them a call
    /// leaves unbound are the keys the aggregation binds.
    group: Vec<usize>,
    window: Option<WindowPlan>,
    /// The aggregate's function, parsed once: any but `count`, which is
    /// the rows' number, and `quantile`, whose level a row gives.
    f: Option<Function>,
}

/// The static shape of a calendar window served from the per-bar cache.
#[derive(Debug)]
pub(crate) struct WindowPlan {
    /// The window literal's position in the conjunction.
    window: usize,
    wslot: usize,
    /// The literal as the cache's key names it (shared windows share it).
    literal: usize,
    /// The tests of the base time or of what only the call binds, applied
    /// to the cached rows at each call (`Ex = T`, `Ex > T1`), each with the
    /// slots the call must bind for it: what neither the base nor the
    /// cached rows give it, and an assignment's variable unless an atom
    /// binds it (the assignment would bind it).
    late: Vec<(Literal, Vec<usize>)>,
    /// The outer bindings the cached conjunction reads, ascending: with the
    /// literal, a group of the cache.
    outer: Vec<usize>,
    /// The cached conjunction, in its solve order.
    order: Vec<Literal>,
}

/// A rows window served: its rows' count, and the aggregate over them
/// (computed only when the count reaches the window's minimum; an error is
/// partial arithmetic).
pub(crate) type Served = (usize, Result<Option<Value>, String>);

/// A calendar window's rows as their count and the aggregate's argument
/// columns.
type WindowColumns = (usize, Vec<Vec<f64>>);

/// The window of a rows group at bar `b`, as positions `lo..hi` in its row
/// buffer: the rows among the last `n` anchored bars at or before `b` (all
/// of them from the group's first bar when the buffer reaches it). `None`
/// when the buffer does not hold the anchors the window needs.
fn window_at(st: &RowsState, b: i64, n: usize) -> Option<(usize, usize)> {
    let hi_a = st.anchors.partition_point(|a| *a <= b);
    if hi_a == 0 {
        return Some((st.rows.len(), st.rows.len()));
    }
    let lo_t = if hi_a >= n && n > 0 {
        st.anchors[hi_a - n]
    } else if st.from_start {
        st.anchors[0]
    } else {
        return None;
    };
    let lo = st.rows.partition_point(|t1| *t1 < lo_t);
    let hi = st.rows.partition_point(|t1| *t1 <= b);
    Some((lo, hi))
}

/// The moment aggregate over a group's window at bar `t` from running
/// sums that are a function of the group's rows and the bar alone, never
/// of which bars were asked for (the compiler design's I2: the reduction
/// order of a demanded value is fixed). The sums walk the group's time
/// domain bar by bar: at a checkpoint (every `K = max(n, 8)`-th bar of the
/// domain) they are rebuilt from the window's rows, shifted by its first
/// row; at an anchored bar they take the rows that entered and drop those
/// that left; at any other bar the window is unchanged. A request at `t`
/// brings the sums from where they are to `t` along that walk, or from the
/// checkpoint at or before `t` when they are behind it, past it, or
/// absent. The one departure, counted in the kernel's statistics, is a
/// checkpoint window the buffer no longer holds; the sums are then built
/// at `t` itself.
fn running_value(st: &mut RowsState, agg: &str, t: i64, n: usize, bars: &[i64], noncanonical: &mut usize) -> Option<f64> {
    let k = abt_agg::checkpoint_period(n);
    let it = bars.binary_search(&t).ok()?;
    let ic = it - it % k;
    let rebuild = |st: &mut RowsState, b: i64, lo: usize, hi: usize| {
        let first = (lo < hi && lo < st.rows.len()).then(|| st.rows.pair(lo));
        let shift = match first {
            Some((v, k)) => (if k > 0 { v[0] } else { 0.0 }, if k > 1 { v[1] } else { 0.0 }),
            None => (0.0, 0.0),
        };
        let shift = (if shift.0.is_finite() { shift.0 } else { 0.0 }, if shift.1.is_finite() { shift.1 } else { 0.0 });
        let mut r = RunningSums::new(st.base_id + lo as u64, shift);
        for i in lo..hi.min(st.rows.len()) {
            let (v, k) = st.rows.pair(i);
            r.apply(&v[..k], 1.0);
        }
        r.hi = st.base_id + hi as u64;
        r.at = b;
        st.sums = Some(r);
    };
    let advance = |st: &mut RowsState, b: i64, lo: usize, hi: usize| {
        let (lo_id, hi_id) = (st.base_id + lo as u64, st.base_id + hi as u64);
        let base = st.base_id;
        let rows = &st.rows;
        let r = st.sums.as_mut().unwrap();
        for id in r.hi.max(lo_id)..hi_id {
            let (v, k) = rows.pair((id - base) as usize);
            r.apply(&v[..k], 1.0);
        }
        for id in r.lo..lo_id.min(r.hi) {
            let (v, k) = rows.pair((id - base) as usize);
            r.apply(&v[..k], -1.0);
        }
        r.lo = lo_id.max(r.lo);
        r.hi = hi_id.max(r.hi);
        r.at = b;
    };
    // Where to start the walk: just after the sums' bar when they are
    // behind `t` on the walk, else at the checkpoint.
    let from = match &st.sums {
        Some(r) if r.at == t => None,
        Some(r) if r.at < t && r.at >= bars[ic] => bars.binary_search(&r.at).ok().map(|i| i + 1),
        _ => {
            match window_at(st, bars[ic], n) {
                Some((lo, hi)) if lo <= hi => rebuild(st, bars[ic], lo, hi),
                _ => {
                    *noncanonical += 1;
                    let (lo, hi) = window_at(st, t, n)?;
                    rebuild(st, t, lo, hi);
                    return st.sums.as_ref().unwrap().aggregate(agg);
                }
            }
            Some(ic + 1)
        }
    };
    if let Some(from) = from {
        for &b in &bars[from..=it] {
            let Some((lo, hi)) = window_at(st, b, n) else {
                *noncanonical += 1;
                let (lo, hi) = window_at(st, t, n)?;
                rebuild(st, t, lo, hi);
                return st.sums.as_ref().unwrap().aggregate(agg);
            };
            let ib = bars.binary_search(&b).ok()?;
            if ib % k == 0 {
                rebuild(st, b, lo, hi);
            } else if st.anchors.binary_search(&b).is_ok() {
                advance(st, b, lo, hi);
            } else {
                st.sums.as_mut().unwrap().at = b;
            }
        }
    }
    let (lo, hi) = window_at(st, t, n)?;
    let (lo_id, hi_id) = (st.base_id + lo as u64, st.base_id + hi as u64);
    let r = st.sums.as_ref()?;
    if r.lo != lo_id || r.hi != hi_id {
        // The walk did not land on the window asked for (a row the walk's
        // anchors did not account for): built at `t` itself.
        *noncanonical += 1;
        rebuild(st, t, lo, hi);
    }
    st.sums.as_ref().unwrap().aggregate(agg)
}

/// Anchored bars a rows window group keeps beyond its N, so that a call a
/// few bars back (a recursion reaching its previous row) is served too;
/// abt-agg's rows windows keep the same.
pub(crate) const ROWS_SLACK: usize = abt_agg::window::ROWS_SLACK;

/// The rows from which a block's rows window has its conjunction solved at
/// the bar over the whole block (`Kernel::prefetch_rows_window`), and the
/// block evaluator's window operator reads the bar's relations before any
/// entity's older bars (`colops.rs`), as that solve derives them.
pub(crate) const PREFETCH_MIN_ROWS: usize = 32;

/// A rows window group's running state: its last N anchored bars and the
/// rows among them, through `upto`. A bar holds at most one row here (a
/// group whose bar has two is the general path's), so the rows' order is
/// their bars': the order the general path sorts a window's environments
/// to (E-46).
#[derive(Clone, Debug)]
pub(crate) struct RowsState {
    pub(crate) upto: i64,
    /// The buffer reaches the group's first bar.
    from_start: bool,
    anchors: std::collections::VecDeque<i64>,
    rows: FlatRows,
    /// The id of the buffer's first row; a row's id is its position since
    /// the group began.
    base_id: u64,
    /// Sums over the window last served, for the moment aggregates.
    sums: Option<RunningSums>,
}

impl Default for RowsState {
    fn default() -> RowsState {
        RowsState {
            upto: i64::MIN,
            from_start: false,
            anchors: Default::default(),
            rows: FlatRows::default(),
            base_id: 0,
            sums: None,
        }
    }
}

impl RowsState {
    /// A group no call has fed: the first call walks back for it.
    pub(crate) fn is_new(&self) -> bool {
        self.anchors.is_empty() && self.rows.is_empty() && self.upto == i64::MIN
    }

    /// Bar `t1` taken in: an anchored bar, and the row there with the
    /// aggregate's arguments, if the conjunction holds there.
    pub(crate) fn feed(&mut self, t1: i64, anchored: bool, vals: Option<&[f64]>) {
        if anchored {
            self.anchors.push_back(t1);
        }
        if let Some(vals) = vals {
            self.rows.push(t1, vals);
        }
    }

    /// The window served at `t`, as positions `lo..hi` of the buffer: the
    /// rows among its last `n` anchored bars at or before `t` (all of them
    /// when the buffer reaches the group's first bar). `None` when the
    /// buffer holds too few anchored bars and does not reach the first.
    pub(crate) fn window(&self, t: i64, n: usize) -> Option<(usize, usize)> {
        let hi = self.anchors.partition_point(|a| *a <= t);
        if hi < n && !self.from_start {
            return None;
        }
        let lo_t = if hi >= n && n > 0 {
            self.anchors[hi - n]
        } else if hi > 0 {
            self.anchors[0]
        } else {
            i64::MAX
        };
        let lo = self.rows.partition_point(|t1| *t1 < lo_t);
        let hi_r = self.rows.partition_point(|t1| *t1 <= t);
        Some((lo, hi_r.max(lo)))
    }

    /// The argument `j` over the rows `lo..hi`, in their order.
    pub(crate) fn col(&self, j: usize, lo: usize, hi: usize) -> &[f64] {
        self.rows.col(j, lo, hi)
    }

    /// Keep N plus a checkpoint's span plus slack anchored bars (a call a
    /// few bars back, and the rebuild at the checkpoint before any bar,
    /// are served from the buffer), and the rows from the oldest on.
    pub(crate) fn trim(&mut self, n: usize) {
        while self.anchors.len() > n + abt_agg::checkpoint_period(n) + ROWS_SLACK {
            self.anchors.pop_front();
            self.from_start = false;
        }
        let oldest = self.anchors.front().copied().unwrap_or(i64::MAX);
        while self.rows.front().is_some_and(|t1| t1 < oldest) {
            self.rows.pop_front();
            self.base_id += 1;
        }
    }
}

/// A rows window group's rows, flat: each row's bar, and each argument's
/// values as a column, from `start` on. A row that leaves the window
/// advances `start` (the buffers are compacted once half of them is behind
/// it), so a window is a slice of each column, handed to the reduction as
/// it is.
#[derive(Clone, Debug, Default)]
pub(crate) struct FlatRows {
    times: Vec<i64>,
    cols: Vec<Vec<f64>>,
    start: usize,
}

impl FlatRows {
    pub(crate) fn len(&self) -> usize {
        self.times.len() - self.start
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.len() == 0
    }

    fn front(&self) -> Option<i64> {
        self.times.get(self.start).copied()
    }

    fn partition_point(&self, pred: impl FnMut(&i64) -> bool) -> usize {
        self.times[self.start..].partition_point(pred)
    }

    /// A row at `t` with the aggregate's arguments `vals` (none for a
    /// count): a group's rows all have as many.
    fn push(&mut self, t: i64, vals: &[f64]) {
        if self.cols.len() != vals.len() {
            debug_assert!(self.is_empty(), "a group's rows have one width");
            self.times.clear();
            self.start = 0;
            self.cols = vec![Vec::new(); vals.len()];
        }
        self.times.push(t);
        for (c, v) in self.cols.iter_mut().zip(vals) {
            c.push(*v);
        }
    }

    fn pop_front(&mut self) {
        self.start += 1;
        if self.start == self.times.len() {
            self.times.clear();
            self.cols.iter_mut().for_each(Vec::clear);
            self.start = 0;
        } else if self.start >= 32 && 2 * self.start >= self.times.len() {
            self.times.drain(..self.start);
            for c in &mut self.cols {
                c.drain(..self.start);
            }
            self.start = 0;
        }
    }

    /// Argument `j` over rows `lo..hi`; a group that never held a row has
    /// no columns, and its every window is empty.
    fn col(&self, j: usize, lo: usize, hi: usize) -> &[f64] {
        match self.cols.get(j) {
            Some(c) => &c[self.start + lo..self.start + hi],
            None => &[],
        }
    }

    /// Row `i`'s first two arguments (zero where it has fewer), and how
    /// many it has of them: what running sums read of a row.
    fn pair(&self, i: usize) -> ([f64; 2], usize) {
        let mut out = [0.0; 2];
        let k = self.cols.len().min(2);
        for (j, slot) in out.iter_mut().enumerate().take(k) {
            *slot = self.cols[j][self.start + i];
        }
        (out, k)
    }
}

/// The states of one rows window group (an aggregation literal of a rule,
/// or the literal whose state it shares), one per entity and indexed by
/// its symbol (`RowsPlan::ent`), for the calls whose outer values other
/// than the entity are those of the group's first call (`rest`); a call
/// with others keeps its state in `Kernel::rows_state` by its whole key.
#[derive(Clone, Debug, Default)]
pub(crate) struct EntRows {
    rest: Option<Vec<Option<Value>>>,
    states: Vec<Option<Box<RowsState>>>,
    live: usize,
}

impl EntRows {
    /// The states held.
    pub(crate) fn live(&self) -> usize {
        self.live
    }

    pub(crate) fn clear(&mut self) {
        self.rest = None;
        self.states.clear();
        self.live = 0;
    }

    pub(crate) fn get(&self, s: Sym) -> Option<&RowsState> {
        self.states.get(s as usize).and_then(|st| st.as_deref())
    }

    pub(crate) fn take(&mut self, s: Sym) -> Option<Box<RowsState>> {
        let st = self.states.get_mut(s as usize).and_then(Option::take);
        if st.is_some() {
            self.live -= 1;
        }
        st
    }

    pub(crate) fn put(&mut self, s: Sym, st: Box<RowsState>) {
        let i = s as usize;
        if self.states.len() <= i {
            self.states.resize_with(i + 1, || None);
        }
        if self.states[i].replace(st).is_none() {
            self.live += 1;
        }
    }

    /// Whether a call whose outer values other than the entity are `rest`
    /// keeps its state here, the first call naming them.
    pub(crate) fn admits(&mut self, rest: impl Iterator<Item = Option<Value>>) -> bool {
        match &self.rest {
            Some(r) => r.iter().cloned().eq(rest),
            None => {
                self.rest = Some(rest.collect());
                true
            }
        }
    }

    fn admits_now(&self, rest: impl Iterator<Item = Option<Value>>) -> bool {
        self.rest.as_ref().is_none_or(|r| r.iter().cloned().eq(rest))
    }
}

/// Symbols at or above this keep their rows window states in the map: the
/// dense states are a vector by symbol.
pub(crate) const ENT_ROWS_MAX: Sym = 1 << 20;

/// Where a rows window group's state is kept: dense by its entity, or in
/// the map by its whole key.
pub(crate) enum GroupAt {
    Ent(usize, Sym),
    Map(WindowKey),
}

/// Running sums over the rows with ids in `lo..hi` of a group: abt-agg's
/// shifted compensated sums (P-1.2), with the window they hold and the bar
/// of the group's time domain they were last brought to. At every
/// checkpoint of the domain (every `abt_agg::checkpoint_period(n)`-th bar)
/// the sums are rebuilt from the rows, so rounding cannot accumulate over a
/// long history, and the walk from checkpoint to checkpoint is the same
/// whatever bars were asked for (`running_value`).
#[derive(Clone, Debug)]
struct RunningSums {
    lo: u64,
    hi: u64,
    sums: ShiftedSums,
    /// The bar of the group's time domain the sums were last brought to.
    at: i64,
}

impl RunningSums {
    fn new(lo: u64, shift: (f64, f64)) -> RunningSums {
        RunningSums {
            lo,
            hi: lo,
            sums: ShiftedSums::new(shift),
            at: i64::MIN,
        }
    }
    fn apply(&mut self, vals: &[f64], sign: f64) {
        self.sums.apply(vals, sign);
    }
    /// The aggregate over the window, or `None` where the exact path must
    /// answer.
    fn aggregate(&self, agg: &str) -> Option<f64> {
        let f = Function::parse(agg, None).ok()?;
        self.sums.value(f, (self.hi - self.lo) as usize)
    }
}

/// The aggregates running sums answer.
fn moment_aggregate(agg: &str) -> bool {
    Function::parse(agg, None).map(|f| f.running()).unwrap_or(false)
}

/// An instance that met a partial operation while a body was solved: its
/// bindings, the slots left undefined by the literal that failed, and the
/// halt it would cause if no later literal excludes it.
pub(crate) struct Stuck {
    pub env: Env,
    pub undefined: Vec<usize>,
    pub error: RunError,
    /// The starting binding the instance came from (`Block::origin_of`), and
    /// the position in the plan's order of the literal it stuck at: together
    /// the order the single thread lists stuck instances in.
    pub origin: u32,
    pub at: usize,
}

/// Whether two aggregation literals of one rule may share one window state
/// (compiler design, R3 lowered): the same conjunction, written with the
/// same variables, the same window and minimum, and the same arguments, so
/// that the rows one solves are exactly the rows the other would.
pub fn shareable_windows(rule: &Rule, primary: usize, other: usize) -> bool {
    let (Some(a), Some(b)) = (rule.body.get(primary), rule.body.get(other)) else { return false };
    // Equality of the written text, not of the IR: the IR carries spans,
    // and two aggregations of one rule are never at one position.
    let identity = |v: &str| v.to_string();
    let text = |args: &[crate::ir::Expr], conj: &[Literal]| -> (Vec<String>, Vec<String>) {
        (
            args.iter().map(|e| crate::compiler::print::render_expr(e, &identity)).collect(),
            conj.iter().map(|l| crate::compiler::print::render_literal(l, &identity)).collect(),
        )
    };
    match (a, b) {
        (Literal::Agg { args: aa, conj: ca, .. }, Literal::Agg { args: ab, conj: cb, .. }) => text(aa, ca) == text(ab, cb),
        _ => false,
    }
}

/// A windowed aggregation group: the rule, the aggregation literal, and the
/// outer bindings its conjunction reads apart from the window's base time.
#[derive(Clone, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct WindowKey {
    pub rule: usize,
    pub literal: usize,
    pub outer: Vec<Option<Value>>,
}

/// Result sets larger than this are kept sorted by their first entity.
pub(super) const SORTED_MIN: usize = 16;

/// `env` extended by the tuple `tu` at the atom's variable slots (`slots`,
/// one per term), pushed to `out` unless a term conflicts. A slot bound in
/// `env`, or by an earlier term of the atom (a variable written twice),
/// must hold the tuple's value, as when the terms bind one after another.
fn bind_row<S: Sink>(env: &[Option<Value>], slots: &[Option<usize>], tu: &[Value], out: &mut S) {
    for (k, (slot, val)) in slots.iter().zip(tu.iter()).enumerate() {
        let Some(slot) = *slot else { continue };
        let bound = match &env[slot] {
            Some(x) => Some(x),
            None => slots[..k].iter().zip(tu.iter()).find(|(s, _)| **s == Some(slot)).map(|(_, v)| v),
        };
        if bound.is_some_and(|x| x != val) {
            return;
        }
    }
    out.push_env(env);
    let row = out.last_row().expect("a row was pushed");
    for (slot, val) in slots.iter().zip(tu.iter()) {
        if let Some(slot) = *slot {
            if row[slot].is_none() {
                row[slot] = Some(val.clone());
            }
        }
    }
}

/// The merge of `Kernel::merge_join_atom`: the block's rows, ascending by
/// the entity in `eslot`, beside a relation's `n` tuples at the bar, `tuple(j)`
/// the j-th, read in place (a store's block or a memoised result: no tuple
/// is copied). Each row is bound to the tuples of its entity's run that its
/// own bindings (`slots`, one per term) and the atom's constants (`shared`)
/// match, in the tuples' order. `false`, with nothing pushed, when the
/// tuples are not ascending by `entity_pos`.
#[allow(clippy::too_many_arguments)]
fn merge_join_rows<'t>(live: &Block, next: &mut Block, eslot: usize, entity_pos: usize, slots: &[Option<usize>], shared: &[Option<Value>], n: usize, tuple: impl Fn(usize) -> &'t [Value]) -> bool {
    if !(1..n).all(|j| tuple(j - 1)[entity_pos] <= tuple(j)[entity_pos]) {
        return false;
    }
    let mut j = 0usize;
    for i in 0..live.len() {
        let env = live.row(i);
        next.set_origin(live.origin_of(i));
        let e = env[eslot].as_ref().expect("checked");
        while j < n && tuple(j)[entity_pos] < *e {
            j += 1;
        }
        let mut k = j;
        while k < n {
            let tu = tuple(k);
            if tu[entity_pos] != *e {
                break;
            }
            k += 1;
            // The row's own bindings on the atom's other terms, and the constants.
            let ok = slots.iter().zip(shared).zip(tu).all(|((slot, constant), v)| match slot {
                Some(s) => env[*s].as_ref().is_none_or(|b| b == v),
                None => constant.as_ref().is_none_or(|c| c == v),
            });
            if ok {
                bind_row(env, slots, tu, next);
            }
        }
    }
    true
}

/// The bar an as-of read of a derived relation probes first: the latest
/// label of the relation's domain at or before `t`, no earlier than `floor`.
fn asof_latest(dom: &BTreeSet<i64>, floor: i64, t: i64) -> Option<i64> {
    dom.range(floor..=t).next_back().copied()
}

/// The bar an as-of walk probes after `k`: the latest label before it, no
/// earlier than `floor`. With `asof_latest`, the labels of `floor..=t` from
/// the latest back, each looked up when the walk reaches it.
fn asof_before(dom: &BTreeSet<i64>, floor: i64, k: i64) -> Option<i64> {
    dom.range(floor..k).next_back().copied()
}

impl<'p> Kernel<'p> {
    /// All tuples of `rel` matching `pattern` (`Some` at the key, at every
    /// input and at any output used as a filter), with the index of the rule
    /// that derived each one (`usize::MAX` for stored relations).
    pub(crate) fn call(&mut self, rel: usize, pattern: &[Option<Value>]) -> Result<super::Derived, RunError> {
        let info = self.rels[rel].clone();
        let key = match &pattern[info.key_pos] {
            Some(Value::Time(t)) => *t,
            _ => return Err(RunError::Internal(format!("`{}` requested with an unbound temporal key", info.name))),
        };
        let matches = |tu: &[Value]| pattern.iter().zip(tu.iter()).all(|(p, v)| p.as_ref().map(|p| p == v).unwrap_or(true));
        if info.stored {
            let entity = info.entity_positions.first().and_then(|&e| pattern[e].as_ref());
            let tuples: Vec<(Tuple, usize)> = self
                .store_lookup(rel, key, entity)
                .iter()
                .filter(|tu| matches(tu))
                .map(|tu| (smallvec::SmallVec::from(tu), usize::MAX))
                .collect();
            let cov = &mut self.coverage[rel];
            cov.0 += 1;
            cov.1 += usize::from(!tuples.is_empty());
            cov.2 += tuples.len();
            return Ok(Arc::new(tuples));
        }
        let mut mkey: Vec<Value> = vec![Value::Time(key)];
        for &i in &info.inputs {
            match &pattern[i] {
                Some(v) => mkey.push(v.clone()),
                None => return Err(RunError::Internal(format!("`{}` requested with its input #{} unbound", info.name, i + 1))),
            }
        }
        let memo_key = (rel, mkey);
        // A memoised result set of more than a few tuples is sorted by the
        // relation's first entity: a call that binds it reads its run.
        let entity = info.entity_positions.first().copied();
        let narrow = |rc: &super::Derived| -> super::Derived {
            if pattern.iter().all(|p| p.is_none()) {
                return rc.clone();
            }
            let slice: &[(Tuple, usize)] = match (entity, entity.and_then(|e| pattern[e].as_ref())) {
                (Some(e), Some(v)) if rc.len() > SORTED_MIN => {
                    let lo = rc.partition_point(|(t, _)| t[e] < *v);
                    let hi = lo + rc[lo..].partition_point(|(t, _)| t[e] <= *v);
                    &rc[lo..hi]
                }
                _ => &rc[..],
            };
            if slice.len() == rc.len() && slice.iter().all(|(tu, _)| matches(tu)) {
                rc.clone()
            } else {
                Arc::new(slice.iter().filter(|(tu, _)| matches(tu)).cloned().collect())
            }
        };
        // A stuck instance the pattern meets halts the call (Kleene: a ⊥
        // instance a decision demands); one it does not meet is not read.
        let compatible = |partial: &[Option<Value>]| {
            pattern.iter().zip(partial.iter()).all(|(p, v)| match (p, v) {
                (Some(p), Some(v)) => p == v,
                _ => true,
            })
        };
        if let Some(hit) = self.memo_get(&memo_key) {
            if let Some((_, e)) = self.memo_stuck.get(&memo_key).and_then(|st| st.iter().find(|(p, _)| compatible(p))) {
                return Err(e.clone());
            }
            return Ok(narrow(hit));
        }
        // A call that halted on a stuck instance (Kleene conjunction: a
        // partial operation no later literal excluded) halts again when
        // repeated: the halt is memoised too, so that a ⊥ instance read by
        // many callers at one bar is computed once.
        if let Some(e) = self.memo_errors.get(&memo_key) {
            return Err(e.clone());
        }
        // Behind the memo's oldest bucket with the stores trimmed (E-126),
        // a derivation would read rows the horizon dropped: an entity's
        // read of its previous row is answered from the latest tuples the
        // eviction kept for it (at that bar, or none), and nothing is
        // derived.
        if self.cfg.trim_stores && !info.stored {
            if let (Some(oldest), Some(e)) = (self.memo.oldest(), entity) {
                if key < oldest {
                    if let Some(v) = pattern[e].as_ref() {
                        return Ok(match self.memo.latest_row(rel, v) {
                            Some((t, d)) if t == key => narrow(d),
                            _ => Arc::new(Vec::new()),
                        });
                    }
                }
            }
        }
        // WF-4 rules out a relation depending on itself at the same time;
        // debug builds (the tests) still check that the checker did its job.
        if cfg!(debug_assertions) && !self.in_progress.insert(memo_key.clone()) {
            return Err(RunError::Internal(format!(
                "`{}` depends on itself at the same time; the checker should have rejected this (WF-4)",
                info.name
            )));
        }
        self.derive_stack.push((rel, std::time::Instant::now(), 0.0));
        // Under the block evaluator a relation with a plan is derived over
        // the bar's columns, or the plan declines and the rules derive it.
        let planned = if self.cfg.block_eval && self.cfg.set_eval { self.run_plan(rel, key) } else { None };
        let derived = match planned {
            Some(_) => Ok((Vec::new(), Vec::new())),
            None => {
                let d = self.derive_rules(&info, pattern);
                self.forget_replay(rel);
                d
            }
        };
        if let Some((r, started, inner)) = self.derive_stack.pop() {
            let total = started.elapsed().as_secs_f64();
            self.derive_secs[r] += total - inner;
            if let Some(parent) = self.derive_stack.last_mut() {
                parent.2 += total;
            }
        }
        if cfg!(debug_assertions) {
            self.in_progress.remove(&memo_key);
        }
        let (mut results, stucks) = match derived {
            Ok(r) => r,
            Err(e) => {
                if matches!(e, RunError::Arithmetic { .. }) {
                    self.memo_errors.insert(memo_key, e.clone());
                    self.stuck_bars[rel] += 1;
                }
                return Err(e);
            }
        };
        if !stucks.is_empty() {
            self.memo_stuck.insert(memo_key.clone(), Arc::new(stucks));
            self.stuck_bars[rel] += 1;
        }
        // A plan's result is in the order `call` leaves the rules' in (the
        // plan sorts it as below), and its columns are memoised beside it.
        let (rc, block) = match planned {
            Some(colplan::Planned::Block(b)) => {
                let (rows, built) = b.rows();
                if built {
                    self.row_views[rel] += 1;
                }
                (rows, Some(col::BlockEntry::Cols(b)))
            }
            Some(colplan::Planned::Rows(rows)) => (Arc::new(rows), Some(col::BlockEntry::NotCols)),
            None => {
                if let (Some(e), true) = (entity, results.len() > SORTED_MIN) {
                    // The block solver yields its rows in the first generator's order,
                    // the universe's entity order: a linear check spares the sort.
                    if !results.windows(2).all(|w| w[0].0[e] <= w[1].0[e]) {
                        results.sort_by(|a, b| a.0[e].cmp(&b.0[e]));
                    }
                }
                let rc = Arc::new(results);
                let block = if self.cfg.block_eval && self.cfg.block_identity && !self.memo_stuck.contains_key(&memo_key) {
                    self.identity_block(rel, key, &rc)
                } else {
                    None
                };
                match block {
                    Some((rows, b)) => (rows, Some(col::BlockEntry::Cols(b))),
                    None => (rc, None),
                }
            }
        };
        if rc.len() <= SORTED_MIN {
            self.small_bars[rel] += 1;
        }
        let cov = &mut self.coverage[rel];
        cov.0 += 1;
        cov.1 += usize::from(!rc.is_empty());
        cov.2 += rc.len();
        if let (Some(b), None) = (block, self.worker) {
            self.memo.blocks.insert(rel, key, b);
        }
        self.memo_insert(memo_key.clone(), rc.clone());
        if let Some((_, e)) = self.memo_stuck.get(&memo_key).and_then(|st| st.iter().find(|(p, _)| compatible(p))) {
            return Err(e.clone());
        }
        Ok(narrow(&rc))
    }

    /// The first tuple of a derived relation at its key for one entity: a
    /// memoised block read by binary search, one tuple cloned, no result set
    /// built (the executor's marks, one per held name per bar). The general
    /// path when the result is not memoised or not sorted.
    pub(crate) fn call_first(&mut self, rel: usize, pattern: &[Option<Value>]) -> Result<Option<Tuple>, RunError> {
        let info = self.rels[rel].clone();
        if !info.stored && info.inputs.is_empty() {
            if let (Some(Value::Time(t)), Some(e)) = (pattern.get(info.key_pos).cloned().flatten(), info.entity_positions.first().copied()) {
                if let Some(v) = pattern[e].as_ref() {
                    let memo_key: super::MemoKey = (rel, vec![Value::Time(t)]);
                    if !self.memo_stuck.contains_key(&memo_key) {
                        if let Some(hit) = self.memo_get(&memo_key) {
                            if hit.len() > SORTED_MIN {
                                let lo = hit.partition_point(|(tu, _)| tu[e] < *v);
                                let matches = |tu: &[Value]| pattern.iter().zip(tu.iter()).all(|(p, x)| p.as_ref().map(|p| p == x).unwrap_or(true));
                                return Ok(hit[lo..].iter().take_while(|(tu, _)| tu[e] == *v).find(|(tu, _)| matches(tu)).map(|(tu, _)| tu.clone()));
                            }
                        }
                    }
                }
            }
        }
        Ok(self.call(rel, pattern)?.first().map(|(tu, _)| tu.clone()))
    }

    /// The tuples of `rel` matching `pattern` and the instances there left
    /// stuck on a partial operation, as partial tuples with their halts: an
    /// observed relation's (Definition V-8.3), whose stuck instance no
    /// decision demanded is a row of the ledger rather than a halt. A call
    /// that halted as a whole (an error no instance isolates) is one stuck
    /// row carrying the pattern.
    #[allow(clippy::type_complexity)]
    pub(crate) fn observe_at(&mut self, rel: usize, pattern: &[Option<Value>]) -> Result<(Vec<Tuple>, Vec<(Vec<Option<Value>>, RunError)>), RunError> {
        let e = match self.call(rel, pattern) {
            Ok(found) => return Ok((found.iter().map(|(tu, _)| tu.clone()).collect(), Vec::new())),
            Err(e) => e,
        };
        let info = self.rels[rel].clone();
        let mut mkey: Vec<Value> = Vec::with_capacity(1 + info.inputs.len());
        for &i in std::iter::once(&info.key_pos).chain(&info.inputs) {
            match &pattern[i] {
                Some(v) => mkey.push(v.clone()),
                None => return Err(e),
            }
        }
        let memo_key = (rel, mkey);
        let matches = |tu: &Tuple| pattern.iter().zip(tu.iter()).all(|(p, v)| p.as_ref().is_none_or(|p| p == v));
        let compatible = |partial: &[Option<Value>]| {
            pattern.iter().zip(partial.iter()).all(|(p, v)| match (p, v) {
                (Some(p), Some(v)) => p == v,
                _ => true,
            })
        };
        match (self.memo_get(&memo_key), self.memo_stuck.get(&memo_key)) {
            (Some(live), Some(stuck)) => Ok((
                live.iter().filter(|(tu, _)| matches(tu)).map(|(tu, _)| tu.clone()).collect(),
                stuck.iter().filter(|(p, _)| compatible(p)).cloned().collect(),
            )),
            _ if matches!(e, RunError::Arithmetic { .. }) => Ok((Vec::new(), vec![(pattern.to_vec(), e)])),
            _ => Err(e),
        }
    }

    /// Every tuple the rules of a derived relation yield for a pattern, with
    /// the rule that derived each (the first, when several do), and the
    /// instances left stuck on a partial operation as partial head tuples
    /// with their errors (`call` halts a reader whose pattern meets one).
    #[allow(clippy::type_complexity)]
    pub(crate) fn derive_rules(&mut self, info: &super::RelInfo, pattern: &[Option<Value>]) -> Result<(Vec<(Tuple, usize)>, Vec<(Vec<Option<Value>>, RunError)>), RunError> {
        let mut results: Vec<(Tuple, usize)> = Vec::new();
        let mut stucks: Vec<(Vec<Option<Value>>, RunError)> = Vec::new();
        // Duplicates are found by a scan while the set is small, by a hash
        // set once it grows (most calls derive at most a tuple or two).
        let mut seen: Option<super::hash::FxHashSet<Tuple>> = None;
        for &ri in &info.rules {
            // A rule the rewriter proved infeasible (compiler design, R2)
            // contributes no tuple: the plan skips it.
            if self.skip_rule.get(ri).copied().unwrap_or(false) {
                continue;
            }
            let cr = self.compiled[ri].clone();
            let rule = &self.prog.rules[ri];
            let mut env: Env = vec![None; cr.nslots];
            let mut applicable = true;
            for (i, term) in rule.head.terms.iter().enumerate() {
                if i != info.key_pos && !info.inputs.contains(&i) {
                    continue;
                }
                match term {
                    Term::Var(v, _) => env[cr.slots[v]] = pattern[i].clone(),
                    Term::Wild(_) => {}
                    t => {
                        let v = self.term_value(&cr, t, &env)?;
                        if pattern[i].as_ref().map(|p| *p != v).unwrap_or(false) {
                            applicable = false;
                        }
                    }
                }
            }
            if !applicable {
                continue;
            }
            // The body in the plan's order (the written order without a
            // plan); an instance left stuck on a partial operation halts the
            // run here, where a decision demanded it (section 7).
            let order = self.plan_order(ri);
            // The set evaluator solves the body over a block of bindings,
            // the tuple evaluator one environment at a time; both yield the
            // same rows in the same order (M-8, `tests/eval_set.rs`).
            let (sols, stuck) = if self.cfg.set_eval {
                self.solve_block_maybe_parallel(&cr, ri, &rule.body, &order, env)?
            } else {
                let (sols, stuck) = self.solve_kleene(&cr, &rule.body, &order, vec![env], Some(ri))?;
                (Block::of(cr.nslots, sols), stuck)
            };
            for s in stuck {
                let mut partial = Vec::with_capacity(rule.head.terms.len());
                for term in &rule.head.terms {
                    partial.push(match term {
                        Term::Var(v, _) => s.env[cr.slots[v]].clone(),
                        Term::Wild(_) | Term::Ctor(..) => None,
                        t => self.term_value(&cr, t, &s.env).ok(),
                    });
                }
                stucks.push((partial, s.error));
            }
            for i in 0..sols.len() {
                let sol = sols.row(i);
                let mut tu: Tuple = smallvec::SmallVec::with_capacity(rule.head.terms.len());
                for term in &rule.head.terms {
                    tu.push(self.term_value(&cr, term, sol)?);
                }
                let fresh = match &mut seen {
                    Some(set) => set.insert(tu.clone()),
                    None if results.len() < 8 => !results.iter().any(|(t, _)| *t == tu),
                    None => {
                        let mut set: super::hash::FxHashSet<Tuple> = results.iter().map(|(t, _)| t.clone()).collect();
                        let fresh = set.insert(tu.clone());
                        seen = Some(set);
                        fresh
                    }
                };
                if fresh {
                    results.push((tu, ri));
                }
            }
        }
        Ok((results, stucks))
    }

    /// The index of a body literal in its rule, mapped to the literal whose
    /// window state it shares under the plan (R3); the window caches are
    /// keyed by it.
    pub(crate) fn literal_index(&self, cr: &CompiledRule, lit: &Literal) -> usize {
        let literal = self.prog.rules[cr.idx].body.iter().position(|l| std::ptr::eq(l, lit)).unwrap_or(usize::MAX);
        self.shared_windows.get(&(cr.idx, literal)).copied().unwrap_or(literal)
    }

    /// The order the plan evaluates a rule's body in (the written order
    /// without one).
    pub(crate) fn plan_order(&self, ri: usize) -> Arc<Vec<usize>> {
        if self.written_order {
            return Arc::new((0..self.prog.rules[ri].body.len()).collect());
        }
        match self.plan_order.get(ri).and_then(|o| o.clone()) {
            Some(o) => o,
            None => Arc::new((0..self.prog.rules[ri].body.len()).collect()),
        }
    }

    fn term_value(&mut self, cr: &CompiledRule, term: &Term, env: &[Option<Value>]) -> Result<Value, RunError> {
        match term {
            Term::Var(v, _) => env[cr.slots[v]].clone().ok_or_else(|| RunError::Internal(format!("variable `{}` unbound in rule {}", v, cr.label))),
            Term::Wild(_) => Err(RunError::Internal(format!("wildcard where a value is needed in rule {}", cr.label))),
            Term::Param(p, _) => self.param_value(cr, p),
            Term::Lit(l, _) => Ok(super::lit_value(l, &mut self.symbols, &mut self.labels, &self.literal_equities)),
            Term::Ctor(c, subs, _) if c == "combo" => {
                // `combo([leg(A1, Q1), ...][, ORDER])` (K-2.3): the legs in
                // the order written; the decision's instrument is the first.
                let ill = || RunError::Internal(format!("ill-typed combo in rule {}", cr.label));
                let Some(Term::Ctor(_, legs, _)) = subs.first() else { return Err(ill()) };
                let mut out = Vec::with_capacity(legs.len());
                for leg in legs {
                    let Term::Ctor(_, args, _) = leg else { return Err(ill()) };
                    let (Value::Equity(s), Some(q)) = (self.term_value(cr, &args[0], env)?, self.term_value(cr, &args[1], env)?.as_f64()) else {
                        return Err(ill());
                    };
                    out.push((s, q));
                }
                let order = match subs.get(1) {
                    Some(o) => self.order_value(cr, o, env)?,
                    None => Order::default(),
                };
                let first = out.first().map(|(s, _)| *s).ok_or_else(ill)?;
                Ok(Value::Decision(Box::new(Decision {
                    ctor: Ctor::Combo,
                    equity: first,
                    amount: out.len() as f64,
                    order,
                    legs: out,
                })))
            }
            Term::Ctor(c, subs, _) => {
                let ctor = Ctor::parse(c).ok_or_else(|| RunError::Internal(format!("unknown constructor `{}`", c)))?;
                let e = self.term_value(cr, &subs[0], env)?;
                let a = self.term_value(cr, &subs[1], env)?;
                let order = match subs.get(2) {
                    Some(o) => self.order_value(cr, o, env)?,
                    None => Order::default(),
                };
                match (e, a.as_f64()) {
                    (Value::Equity(s), Some(x)) => Ok(Value::Decision(Box::new(Decision::single(ctor, s, x, order)))),
                    _ => Err(RunError::Internal(format!("ill-typed decision in rule {}", cr.label))),
                }
            }
        }
    }

    /// The order term of a decision (section 6, order types): `market`,
    /// `moo`, `moc`, or `limit(P[, TIF])` / `stop(P[, TIF])` with TIF one of
    /// `day`, `gtc`, `bars(N)`; the checker has judged its shape.
    fn order_value(&mut self, cr: &CompiledRule, term: &Term, env: &[Option<Value>]) -> Result<Order, RunError> {
        let bad = || RunError::Internal(format!("ill-formed order term in rule {}", cr.label));
        match term {
            Term::Param(p, _) => Ok(Order {
                kind: match p.as_str() {
                    "market" => OrderKind::Market,
                    "moo" => OrderKind::Moo,
                    "moc" => OrderKind::Moc,
                    "moo_moc" => OrderKind::MooMoc,
                    _ => return Err(bad()),
                },
                tif: Tif::Day,
            }),
            Term::Ctor(c, subs, _) => {
                let price = self.term_value(cr, subs.first().ok_or_else(bad)?, env)?.as_f64().ok_or_else(bad)?;
                let kind = match c.as_str() {
                    "limit" => OrderKind::Limit(price),
                    "stop" => OrderKind::Stop(price),
                    _ => return Err(bad()),
                };
                let tif = match subs.get(1) {
                    None => Tif::Day,
                    Some(Term::Param(t, _)) if t == "day" => Tif::Day,
                    Some(Term::Param(t, _)) if t == "gtc" => Tif::Gtc,
                    Some(Term::Ctor(b, n, _)) if b == "bars" && n.len() == 1 => {
                        let v = self.term_value(cr, &n[0], env)?;
                        Tif::Bars(v.as_f64().ok_or_else(bad)?.max(1.0) as u32)
                    }
                    _ => return Err(bad()),
                };
                Ok(Order { kind, tif })
            }
            _ => Err(bad()),
        }
    }

    /// A parameter of the rule's unit: from the rule's own table, filled
    /// when the kernel was built; the kernel's table answers, or names the
    /// unknown parameter, for one the rule's lacks.
    fn param_value(&self, cr: &CompiledRule, name: &str) -> Result<Value, RunError> {
        if let Some(v) = cr.params.get(name) {
            return Ok(v.clone());
        }
        let unit = &self.prog.rules[cr.idx].unit;
        self.params
            .get(&(unit.clone(), name.to_string()))
            .cloned()
            .ok_or_else(|| RunError::Internal(format!("unknown parameter `{}` in `{}`", name, unit)))
    }

    /// Solve a conjunction in the order written: the live solutions, or the
    /// halt of an instance left stuck on a partial operation.
    pub(crate) fn solve(&mut self, cr: &Arc<CompiledRule>, lits: &[Literal], envs: Vec<Env>) -> Result<Vec<Env>, RunError> {
        let order: Vec<usize> = (0..lits.len()).collect();
        let (live, stuck) = self.solve_kleene(cr, lits, &order, envs, None)?;
        match stuck.into_iter().next() {
            Some(s) => Err(s.error),
            None => Ok(live),
        }
    }

    /// The slots a literal mentions, by the rule's slot table.
    fn literal_slots(cr: &CompiledRule, lit: &Literal) -> Vec<usize> {
        let mut names = Vec::new();
        literal_vars(lit, &mut names);
        let mut slots: Vec<usize> = names.iter().filter_map(|v| cr.slots.get(v).copied()).collect();
        slots.sort_unstable();
        slots.dedup();
        slots
    }

    /// Solve a body under Kleene conjunction (semantic model, section 4; compiler
    /// design, section 4.1, "interaction with ⊥"): the literals are taken in
    /// `order`; an instance on which a literal meets a partial operation (a
    /// division by zero, a log of a non-positive, an aggregate of too few
    /// rows) is neither excluded nor fired but *stuck*, with the slots that
    /// literal would have bound undefined. A later literal that mentions no
    /// undefined slot still runs on it: when it is false the instance is
    /// excluded, which is what the written order would have done had the
    /// test come first; a literal that mentions an undefined slot is skipped
    /// (its value is ⊥ too). What survives every literal stuck is returned
    /// apart, and the caller halts on it, since no literal excluded it. The
    /// set of fired, excluded and stuck instances is therefore independent
    /// of the order, which is what lets the planner choose one.
    ///
    /// With `record`, the rule's per-literal counters are updated: the
    /// bindings entering and leaving each literal (the observed cardinalities
    /// the plan reports) and the literal at which a call lost its last
    /// binding (the failure table).
    pub(crate) fn solve_kleene(&mut self, cr: &Arc<CompiledRule>, lits: &[Literal], order: &[usize], envs: Vec<Env>, record: Option<usize>) -> Result<(Vec<Env>, Vec<Stuck>), RunError> {
        self.solve_kleene_from(cr, lits, order, envs, Vec::new(), record)
    }

    /// `solve_kleene` continuing from live and stuck instances (what `explain`
    /// steps literal by literal).
    pub(crate) fn solve_kleene_from(
        &mut self,
        cr: &Arc<CompiledRule>,
        lits: &[Literal],
        order: &[usize],
        envs: Vec<Env>,
        stuck: Vec<Stuck>,
        record: Option<usize>,
    ) -> Result<(Vec<Env>, Vec<Stuck>), RunError> {
        let mut live = envs;
        let mut stuck: Vec<Stuck> = stuck;
        for &li in order {
            let lit = &lits[li];
            let entering = live.len();
            let mut next = Vec::new();
            for env in &live {
                let mut out: Vec<Env> = Vec::new();
                match self.step(cr, lit, env, &mut out) {
                    Ok(()) => next.extend(out),
                    Err(error @ RunError::Arithmetic { .. }) => {
                        let undefined: Vec<usize> = Self::literal_slots(cr, lit).into_iter().filter(|&s| env[s].is_none()).collect();
                        stuck.push(Stuck {
                            env: env.clone(),
                            undefined,
                            error,
                            origin: 0,
                            at: 0,
                        });
                    }
                    Err(e) => return Err(e),
                }
            }
            stuck = self.advance_stuck(cr, lit, stuck)?;
            self.count_literal(record, li, entering, next.len(), stuck.is_empty());
            live = next;
            if live.is_empty() && stuck.is_empty() {
                break;
            }
        }
        Ok((live, stuck))
    }

    /// `solve_kleene` over a block of bindings (M-8; the set evaluator,
    /// `eval_set`): each literal maps the block entering it to the block
    /// leaving it, the rows in the order the tuple evaluator takes its
    /// environments and each row's solutions in the order it yields them.
    /// An instance that meets a partial operation leaves no row (the rows it
    /// pushed are withdrawn) and is stuck, as there.
    pub(crate) fn solve_kleene_block(&mut self, cr: &Arc<CompiledRule>, lits: &[Literal], order: &[usize], start: Block, record: Option<usize>) -> Result<(Block, Vec<Stuck>), RunError> {
        self.solve_kleene_block_from(cr, lits, order, start, Vec::new(), record)
    }

    /// `solve_kleene_block` continuing from a block and the instances the
    /// literals before `order` left stuck: a conjunction past its first
    /// literal, when that literal's block is wanted too
    /// (`prefetch_rows_window`). A stuck instance's `at` counts from the
    /// first literal of `order`.
    fn solve_kleene_block_from(&mut self, cr: &Arc<CompiledRule>, lits: &[Literal], order: &[usize], start: Block, stuck: Vec<Stuck>, record: Option<usize>) -> Result<(Block, Vec<Stuck>), RunError> {
        let mut live = start;
        let mut stuck: Vec<Stuck> = stuck;
        for (pos, &li) in order.iter().enumerate() {
            let lit = &lits[li];
            let entering = live.len();
            let mut next = Block::with_capacity(live.stride(), entering);
            // A positive atom over a block in entity order is joined by one
            // merge pass (06-performance 3.2, "joins between blocks"): one
            // demand for the whole relation, the rows and their matches in
            // the order the row-at-a-time path yields them.
            if let Literal::Atom(atom) = lit {
                if self.merge_join_atom(cr, atom, &live, &mut next)? {
                    self.count_literal(record, li, entering, next.len(), stuck.is_empty());
                    live = next;
                    if live.is_empty() && stuck.is_empty() {
                        break;
                    }
                    continue;
                }
            }
            // A rows window over the block: its conjunction solved once for
            // every row at the bar, each group then fed its own row.
            let saved = self.prefetch.take();
            let saved_bucket = self.prefetch_bucket.take();
            if let Literal::Agg { args, conj, within, .. } = lit {
                self.prefetch = self.prefetch_rows_window(cr, lit, conj, args, within.as_deref(), &live)?;
                if self.prefetch.is_none() {
                    self.prefetch_bucket = self.prefetch_bucket_window(cr, lit, conj, args, within.as_deref(), &live)?;
                }
            }
            for i in 0..live.len() {
                let env = live.row(i);
                let before = next.len();
                next.set_origin(live.origin_of(i));
                self.prefetch_row = i;
                match self.step(cr, lit, env, &mut next) {
                    Ok(()) => {}
                    Err(error @ RunError::Arithmetic { .. }) => {
                        next.truncate(before);
                        let undefined: Vec<usize> = Self::literal_slots(cr, lit).into_iter().filter(|&s| env[s].is_none()).collect();
                        stuck.push(Stuck {
                            env: env.to_vec(),
                            undefined,
                            error,
                            origin: live.origin_of(i),
                            at: pos,
                        });
                    }
                    Err(e) => return Err(e),
                }
            }
            self.prefetch = saved;
            self.prefetch_bucket = saved_bucket;
            stuck = self.advance_stuck(cr, lit, stuck)?;
            self.count_literal(record, li, entering, next.len(), stuck.is_empty());
            live = next;
            if live.is_empty() && stuck.is_empty() {
                break;
            }
        }
        Ok((live, stuck))
    }

    /// The stuck instances past one more literal (Kleene conjunction): one
    /// that mentions an undefined slot is skipped, its value ⊥ too; any
    /// other runs, and is excluded when it is false.
    fn advance_stuck(&mut self, cr: &Arc<CompiledRule>, lit: &Literal, stuck: Vec<Stuck>) -> Result<Vec<Stuck>, RunError> {
        if stuck.is_empty() {
            return Ok(stuck);
        }
        let mentioned = Self::literal_slots(cr, lit);
        let mut still = Vec::with_capacity(stuck.len());
        for s in stuck {
            if mentioned.iter().any(|m| s.undefined.contains(m)) {
                still.push(s);
                continue;
            }
            let mut out: Vec<Env> = Vec::new();
            match self.step(cr, lit, &s.env, &mut out) {
                Ok(()) => {
                    if out.is_empty() {
                        self.stats.excluded_after_undefined += 1;
                    }
                    for e in out {
                        still.push(Stuck {
                            env: e,
                            undefined: s.undefined.clone(),
                            error: s.error.clone(),
                            origin: s.origin,
                            at: s.at,
                        });
                    }
                }
                Err(RunError::Arithmetic { .. }) => {
                    let mut undefined = s.undefined;
                    undefined.extend(mentioned.iter().copied().filter(|&m| s.env[m].is_none()));
                    still.push(Stuck {
                        env: s.env,
                        undefined,
                        error: s.error,
                        origin: s.origin,
                        at: s.at,
                    });
                }
                Err(e) => return Err(e),
            }
        }
        Ok(still)
    }

    /// A rule's per-literal counters: the bindings entering and leaving
    /// literal `li`, and whether the call lost its last binding there.
    pub(crate) fn count_literal(&mut self, record: Option<usize>, li: usize, entering: usize, leaving: usize, no_stuck: bool) {
        if let Some(ri) = record {
            if let Some(counts) = self.literal_counts.get_mut(ri) {
                if let Some(c) = counts.get_mut(li) {
                    c.0 += entering as u64;
                    c.1 += leaving as u64;
                    if entering > 0 && leaving == 0 && no_stuck {
                        c.2 += 1;
                    }
                }
            }
        }
    }

    /// The slot each term of an atom binds. Computed at each use: a literal
    /// has no address that is stable for the run (a window's conjunction is
    /// reordered into a temporary on every evaluation, `conj_order`), so a
    /// cache keyed by the atom's address served one atom's map to another
    /// (E-130).
    fn atom_slots(cr: &CompiledRule, atom: &Atom) -> Vec<Option<usize>> {
        atom.terms.iter().map(|t| if let Term::Var(v, _) = t { Some(cr.slots[v]) } else { None }).collect()
    }

    /// The pattern an atom presents to `call`, and whether each term needs
    /// post-matching (constructor patterns and fresh variables).
    fn atom_pattern(&mut self, cr: &CompiledRule, atom: &Atom, env: &[Option<Value>]) -> Result<Vec<Option<Value>>, RunError> {
        let mut pat = Vec::with_capacity(atom.terms.len());
        for term in &atom.terms {
            pat.push(match term {
                Term::Var(v, _) => env[cr.slots[v]].clone(),
                Term::Wild(_) | Term::Ctor(..) => None,
                Term::Param(p, _) => Some(self.param_value(cr, p)?),
                Term::Lit(l, _) => Some(super::lit_value(l, &mut self.symbols, &mut self.labels, &self.literal_equities)),
            });
        }
        Ok(pat)
    }

    /// Solutions of a positive atom from one binding.
    fn atom_matches(&mut self, cr: &CompiledRule, atom: &Atom, env: &[Option<Value>]) -> Result<Vec<Env>, RunError> {
        let mut out = Vec::new();
        self.atom_matches_into(cr, atom, env, &mut out)?;
        Ok(out)
    }

    /// The merge join of a block with a relation (3.2): when the block's rows
    /// bind the atom's entity in ascending order and its time to one bar, the
    /// relation is demanded once, whole, and walked beside the block. The
    /// matches of a row come in the relation's order, as `atom_matches_into`
    /// yields them, so the two paths produce the same block. `false` when the
    /// form is not this one, or a stuck instance would halt a row (the
    /// row-at-a-time path halts the right rows): nothing was pushed then.
    fn merge_join_atom(&mut self, cr: &CompiledRule, atom: &Atom, live: &Block, next: &mut Block) -> Result<bool, RunError> {
        const MIN_ROWS: usize = 32;
        if live.len() < MIN_ROWS || atom.terms.iter().any(|t| matches!(t, Term::Ctor(..))) {
            return Ok(false);
        }
        let Some(&rel) = self.rel_ids.get(&atom.name) else { return Ok(false) };
        let (stored, key_pos, entity_pos, inputs_empty) = {
            let info = &self.rels[rel];
            (info.stored, info.key_pos, info.entity_positions.first().copied(), info.inputs.is_empty())
        };
        let Some(entity_pos) = entity_pos else { return Ok(false) };
        if !inputs_empty {
            return Ok(false);
        }
        let Term::Var(ev, _) = &atom.terms[entity_pos] else { return Ok(false) };
        let Term::Var(kv, _) = &atom.terms[key_pos] else { return Ok(false) };
        let (eslot, kslot) = (cr.slots[ev], cr.slots[kv]);
        // Every row binds the entity, ascending, and the time to one bar.
        let first = live.row(0);
        let Some(Value::Time(key)) = first[kslot] else { return Ok(false) };
        let mut prev: Option<&Value> = None;
        for i in 0..live.len() {
            let row = live.row(i);
            match (&row[eslot], &row[kslot]) {
                (Some(e @ Value::Equity(_)), Some(Value::Time(k))) if *k == key => {
                    if prev.is_some_and(|p| p > e) {
                        return Ok(false);
                    }
                    prev = Some(e);
                }
                _ => return Ok(false),
            }
        }
        // The pattern of the terms every row shares (the key and the
        // constants); a variable bound differently per row is matched per row.
        let mut shared: Vec<Option<Value>> = vec![None; atom.terms.len()];
        for (i, term) in atom.terms.iter().enumerate() {
            match term {
                Term::Var(_, _) | Term::Wild(_) => {}
                Term::Param(p, _) => shared[i] = Some(self.param_value(cr, p)?),
                Term::Lit(l, _) => shared[i] = Some(super::lit_value(l, &mut self.symbols, &mut self.labels, &self.literal_equities)),
                Term::Ctor(..) => return Ok(false),
            }
        }
        shared[key_pos] = Some(Value::Time(key));
        let slots = Self::atom_slots(cr, atom);
        // The relation, whole at the bar, read where it lives: a stored
        // relation's block, a derived one's memoised result.
        if stored {
            let rows = self.store_lookup(rel, key, None);
            return Ok(merge_join_rows(live, next, eslot, entity_pos, &slots, &shared, rows.len(), |j| rows.row(j)));
        }
        let mkey: super::MemoKey = (rel, vec![Value::Time(key)]);
        // A halt the whole demand meets is the row-at-a-time path's to
        // place on the rows it belongs to.
        let derived = match self.call(rel, &shared) {
            Ok(d) => d,
            Err(RunError::Arithmetic { .. }) => return Ok(false),
            Err(e) => return Err(e),
        };
        if self.memo_stuck.contains_key(&mkey) {
            return Ok(false);
        }
        Ok(merge_join_rows(live, next, eslot, entity_pos, &slots, &shared, derived.len(), |j| derived[j].0.as_slice()))
    }

    /// `atom_matches` into a sink, in the same order.
    fn atom_matches_into<S: Sink>(&mut self, cr: &CompiledRule, atom: &Atom, env: &[Option<Value>], out: &mut S) -> Result<(), RunError> {
        let rel = self.rel_ids.get(&atom.name).copied().ok_or_else(|| RunError::Internal(format!("unknown relation `{}`", atom.name)))?;
        let pat = self.atom_pattern(cr, atom, env)?;
        // A stored relation binds straight from its store's block: no result
        // set, no tuple copies (decision patterns take the general path).
        let (stored, key_pos, entity_pos) = {
            let info = &self.rels[rel];
            (info.stored, info.key_pos, info.entity_positions.first().copied())
        };
        if stored && !atom.terms.iter().any(|t| matches!(t, Term::Ctor(..))) {
            let Some(Value::Time(key)) = pat[key_pos] else {
                return Err(RunError::Internal(format!("`{}` requested with an unbound temporal key", atom.name)));
            };
            let slots = Self::atom_slots(cr, atom);
            let entity = entity_pos.and_then(|e| pat[e].clone());
            for tu in self.store_lookup(rel, key, entity.as_ref()) {
                if !pat.iter().zip(tu.iter()).all(|(p, v)| p.as_ref().map(|p| p == v).unwrap_or(true)) {
                    continue;
                }
                bind_row(env, &slots, tu, out);
            }
            return Ok(());
        }
        let tuples = self.call(rel, &pat)?;
        self.bind_tuples_into(cr, atom, env, &tuples, out)
    }

    /// `R(...) asof T`: R's tuples at the latest key at or before T that
    /// has one matching the bound terms (data-bundle doc, section 2: the
    /// as-of join). A stored relation is answered from its index; a derived
    /// one by calling it at its own resolution's bars backwards from T,
    /// with the answer memoised for every bar walked.
    fn asof_matches<S: Sink>(&mut self, cr: &CompiledRule, atom: &Atom, at: &Term, within: Option<&Cal>, env: &[Option<Value>], out: &mut S) -> Result<(), RunError> {
        let rel = self.rel_ids.get(&atom.name).copied().ok_or_else(|| RunError::Internal(format!("unknown relation `{}`", atom.name)))?;
        let t = self.time_of(cr, at, env)?;
        let info = self.rels[rel].clone();
        // A bar view of a finer relation (3.4): only a key within the bucket
        // of the as-of bar on that calendar is read.
        let floor: i64 = match within {
            Some(c) => {
                let entity = cr.entity_slot.and_then(|sl| env.get(sl)).and_then(|v| v.as_ref()).and_then(|v| v.as_equity());
                let dom = self.domain_for(c, entity)?;
                dom.range(..t).next_back().map(|p| p + 1).unwrap_or(i64::MIN)
            }
            None => i64::MIN,
        };
        let mut pat = self.atom_pattern(cr, atom, env)?;
        pat[info.key_pos] = None;
        let tuples = if info.stored {
            let matches = |tu: &[Value]| pat.iter().zip(tu.iter()).all(|(p, v)| p.as_ref().map(|p| p == v).unwrap_or(true));
            let mut found: Vec<(Tuple, usize)> = Vec::new();
            let entity = info.entity_positions.first().and_then(|&e| pat[e].clone());
            let mut cur = t;
            while let Some(k) = self.stores[rel].by_time.range(floor..=cur).next_back().map(|(k, _)| *k) {
                found.extend(
                    self.store_lookup(rel, k, entity.as_ref())
                        .iter()
                        .filter(|tu| matches(tu))
                        .map(|tu| (smallvec::SmallVec::from(tu), usize::MAX)),
                );
                if !found.is_empty() || k == i64::MIN {
                    break;
                }
                cur = k - 1;
            }
            // Behind the retention horizon: the latest row of each identity
            // the trimmed blocks held (`Store::trim_before`).
            if found.is_empty() && floor == i64::MIN {
                let e = info.entity_positions.first().copied();
                found.extend(
                    self.stores[rel]
                        .kept
                        .iter()
                        .filter(|tu| entity.as_ref().map(|v| e.map(|e| tu[e] == *v).unwrap_or(true)).unwrap_or(true) && matches(tu))
                        .map(|tu| (tu.clone(), usize::MAX)),
                );
            }
            Arc::new(found)
        } else {
            for &i in &info.inputs {
                if pat[i].is_none() {
                    return Err(RunError::Internal(format!("`{}` requested as of {} with its input #{} unbound", info.name, t, i + 1)));
                }
            }
            let entity = info.entity_positions.first().and_then(|&e| pat[e].as_ref()).and_then(|v| v.as_equity());
            // The relation's bars from T back to the floor, one label taken
            // at a time (`asof_latest`): a call cannot hold the domain, and
            // nothing opens a bucket while a body is solved, so the labels
            // are those a copy of the range would have listed.
            let Some(first) = asof_latest(self.domain_for(&info.cal, entity)?, floor, t) else {
                return Ok(());
            };
            // The common case: the latest bar itself has a tuple (a relation
            // read at its previous row). Answered directly, nothing memoised.
            pat[info.key_pos] = Some(Value::Time(first));
            let latest = self.call(rel, &pat)?;
            if !latest.is_empty() {
                return self.bind_tuples_into(cr, atom, env, &latest, out);
            }
            pat[info.key_pos] = None;
            // The walk's answer is the latest bar with a tuple matching this
            // pattern, so its memo is keyed by the whole pattern (the key
            // unbound), the entity with it: keyed by the inputs alone, one
            // entity's bar was served to the next (E-132).
            let akey: Vec<Option<Value>> = pat.clone();
            let mut probe: super::AsofKey = (rel, akey, first);
            let mut walked: Vec<i64> = Vec::new();
            let mut found: Option<i64> = None;
            let mut next = Some(first);
            while let Some(k) = next {
                probe.2 = k;
                if let Some(hit) = self.asof_memo.get(&probe) {
                    found = *hit;
                    break;
                }
                walked.push(k);
                // The first bar was just called and has no tuple.
                if k != first {
                    pat[info.key_pos] = Some(Value::Time(k));
                    if !self.call(rel, &pat)?.is_empty() {
                        found = Some(k);
                        break;
                    }
                }
                next = asof_before(self.domain_for(&info.cal, entity)?, floor, k);
            }
            for k in walked {
                probe.2 = k;
                self.asof_memo.insert(probe.clone(), found);
            }
            match found {
                Some(k) => {
                    pat[info.key_pos] = Some(Value::Time(k));
                    self.call(rel, &pat)?
                }
                None => Arc::new(Vec::new()),
            }
        };
        self.bind_tuples_into(cr, atom, env, &tuples, out)
    }

    /// The bindings of `env` extended by each of `tuples` the atom's terms
    /// match, into a sink, in the tuples' order.
    fn bind_tuples_into<S: Sink>(&mut self, cr: &CompiledRule, atom: &Atom, env: &[Option<Value>], tuples: &super::Derived, out: &mut S) -> Result<(), RunError> {
        if !atom.terms.iter().any(|t| matches!(t, Term::Ctor(..))) {
            let slots = Self::atom_slots(cr, atom);
            for (tu, _) in tuples.iter() {
                bind_row(env, &slots, tu, out);
            }
            return Ok(());
        }
        // A constructor pattern binds its sub-terms as it matches.
        'tuples: for (tu, _) in tuples.iter() {
            let mut e = env.to_vec();
            for (term, val) in atom.terms.iter().zip(tu.iter()) {
                match term {
                    Term::Var(v, _) => {
                        let slot = cr.slots[v];
                        match &e[slot] {
                            Some(x) => {
                                if x != val {
                                    continue 'tuples;
                                }
                            }
                            None => e[slot] = Some(val.clone()),
                        }
                    }
                    Term::Ctor(c, subs, _) if !self.match_ctor(cr, c, subs, val, &mut e)? => continue 'tuples,
                    _ => {}
                }
            }
            out.push_owned(e);
        }
        Ok(())
    }

    fn match_ctor(&mut self, cr: &CompiledRule, name: &str, subs: &[Term], val: &Value, env: &mut Env) -> Result<bool, RunError> {
        let Value::Decision(d) = val else { return Ok(false) };
        if !d.ctor.matches(name) {
            return Ok(false);
        }
        let fields = [Value::Equity(d.equity), Value::Num(d.amount)];
        for (sub, fv) in subs.iter().zip(fields.iter()) {
            match sub {
                Term::Var(v, _) => {
                    let slot = cr.slots[v];
                    match &env[slot] {
                        Some(x) => {
                            if x != fv {
                                return Ok(false);
                            }
                        }
                        None => env[slot] = Some(fv.clone()),
                    }
                }
                Term::Wild(_) => {}
                t => {
                    let v = self.term_value(cr, t, env)?;
                    if !values_equal(&v, fv) {
                        return Ok(false);
                    }
                }
            }
        }
        Ok(true)
    }

    fn time_of(&self, cr: &CompiledRule, term: &Term, env: &[Option<Value>]) -> Result<i64, RunError> {
        match term {
            Term::Var(v, _) => env[cr.slots[v]]
                .as_ref()
                .and_then(|x| x.as_time())
                .ok_or_else(|| RunError::Internal(format!("`{}` is not a bound timestamp in rule {}", v, cr.label))),
            _ => Err(RunError::Internal("timestamp term is not a variable".into())),
        }
    }

    /// Bind a builtin's output position to `t`: a variable takes the value
    /// (or must already equal it); `_` accepts it without binding anything.
    fn bind_time<S: Sink>(&self, cr: &CompiledRule, term: &Term, env: &[Option<Value>], t: i64, out: &mut S) {
        match term {
            Term::Var(v, _) => {
                let slot = cr.slots[v];
                match &env[slot] {
                    Some(Value::Time(x)) if *x == t => out.push_env(env),
                    Some(_) => {}
                    None => out.push_with(env, slot, Some(Value::Time(t))),
                }
            }
            Term::Wild(_) => out.push_env(env),
            _ => {}
        }
    }

    /// The calendar of a time variable: what the elaborator inferred for
    /// it (3.4, 3.5), else the rule's.
    fn term_cal(&self, cr: &CompiledRule, term: &Term) -> Cal {
        if let Term::Var(v, _) = term {
            if let Some(c) = self.prog.var_cals.get(cr.idx).and_then(|m| m.get(v)) {
                return c.clone();
            }
        }
        self.prog.infos[cr.idx].cal.clone()
    }

    /// The domain a builtin or a calendar window steps in: the domain of
    /// its argument's calendar, per instrument on the sessions calendar
    /// (`prev(S, S1)` on a session label is the instrument's previous
    /// session, whether or not the label coincides with a base bar).
    fn domain_of_term(&self, cr: &CompiledRule, term: &Term, env: &[Option<Value>]) -> Result<&std::collections::BTreeSet<i64>, RunError> {
        let cal = self.term_cal(cr, term);
        let entity = cr.entity_slot.and_then(|s| env.get(s)).and_then(|v| v.as_ref()).and_then(|v| v.as_equity());
        self.domain_for(&cal, entity)
    }

    /// The time domain of a rule: its calendar's, and for the per-instrument
    /// sessions calendar the sessions of the head's instrument.
    fn domain(&self, cr: &CompiledRule, env: &[Option<Value>]) -> Result<&std::collections::BTreeSet<i64>, RunError> {
        let cal = &self.prog.infos[cr.idx].cal;
        let entity = cr.entity_slot.and_then(|s| env.get(s)).and_then(|v| v.as_ref()).and_then(|v| v.as_equity());
        self.domain_for(cal, entity)
    }

    /// The domain of a calendar, per instrument when it is the sessions calendar.
    pub(crate) fn domain_for(&self, cal: &Cal, entity: Option<Sym>) -> Result<&std::collections::BTreeSet<i64>, RunError> {
        if cal.per_entity() {
            if let Some(d) = entity.and_then(|s| self.session_domains.get(&s)) {
                return Ok(d);
            }
        }
        self.domains.get(cal).ok_or_else(|| RunError::Internal(format!("no time domain at {}", cal)))
    }

    /// The base bars a temporal constraint of a bucket or a session selects
    /// (3.3, 3.5): the bars of `t`'s bucket on the calendar; the bars of the
    /// instrument's session holding `t` (or labelled `t`) with offset in
    /// [lo, hi), at or before `t`.
    fn window_times(&mut self, cr: &CompiledRule, kind: &WindowKind, base: &Term, dur: &Expr, lo: Option<&Expr>, env: &[Option<Value>]) -> Result<Vec<i64>, RunError> {
        let t = self.time_of(cr, base, env)?;
        match kind {
            WindowKind::Window | WindowKind::Prior => {
                let Value::Dur(d) = self.eval_expr(cr, dur, env)? else {
                    return Err(RunError::Internal("window length is not a Duration".into()));
                };
                let lo_t = time::sub_duration(t, *d);
                Ok(match kind {
                    WindowKind::Window => self.domain_of_term(cr, base, env)?.range(lo_t..=t).copied().collect(),
                    _ => self.domain_of_term(cr, base, env)?.range(lo_t..t).copied().collect(),
                })
            }
            WindowKind::Bucket(c) => {
                let entity = cr.entity_slot.and_then(|s| env.get(s)).and_then(|v| v.as_ref()).and_then(|v| v.as_equity());
                if c.per_entity() {
                    // The instrument's session labelled t.
                    let Some(sym) = entity else { return Ok(vec![]) };
                    let Some((open, label)) = self.sessions.get(&sym).and_then(|v| {
                        let i = v.partition_point(|(_, l)| *l < t);
                        v.get(i).copied().filter(|(_, l)| *l == t)
                    }) else {
                        return Ok(vec![]);
                    };
                    return Ok(self.domains.get(&Cal::Base).map(|d| d.range(open + 1..=label).copied().collect()).unwrap_or_default());
                }
                let start = self.domain_for(c, entity)?.range(..t).next_back().map(|p| p + 1).unwrap_or(i64::MIN);
                Ok(self.domains.get(&Cal::Base).map(|d| d.range(start..=t).copied().collect()).unwrap_or_default())
            }
            WindowKind::Session => {
                let entity = cr.entity_slot.and_then(|s| env.get(s)).and_then(|v| v.as_ref()).and_then(|v| v.as_equity());
                let Some(sym) = entity else { return Ok(vec![]) };
                let Value::Dur(hi) = self.eval_expr(cr, dur, env)? else {
                    return Err(RunError::Internal("session offset is not a Duration".into()));
                };
                let lo_d = match lo {
                    Some(e) => match self.eval_expr(cr, e, env)? {
                        Value::Dur(d) => *d,
                        _ => return Err(RunError::Internal("session offset is not a Duration".into())),
                    },
                    None => Duration::default(),
                };
                let Some(v) = self.sessions.get(&sym) else { return Ok(vec![]) };
                let i = v.partition_point(|(_, l)| *l < t);
                let Some(&(open, label)) = v.get(i) else { return Ok(vec![]) };
                if !(open < t && t <= label) {
                    return Ok(vec![]);
                }
                // A bar's offset in its session is its start less the open
                // (minute 0 is the first bar of a session; bars are labelled
                // at their close), so labels in [open + lo + res, open + hi + res).
                let res = self.prog.base.nanos().unwrap_or(0);
                let from = time::add_duration(open, lo_d) + res;
                let to = time::add_duration(open, *hi) + res;
                let end = label.min(t);
                // A sub-interval that starts after `t` (minute 5 asked at
                // minute 3) has no bar yet (E-106): the empty window, where
                // the range's start would pass its end.
                let upper = to.min(end + 1);
                if from >= upper {
                    return Ok(vec![]);
                }
                Ok(self
                    .domains
                    .get(&Cal::Base)
                    .map(|d| d.range(from..upper).filter(|b| **b > open && **b <= end).copied().collect())
                    .unwrap_or_default())
            }
            WindowKind::Rows | WindowKind::PriorRows => Err(RunError::Internal("a `rows` window is solved by its aggregation".into())),
        }
    }

    /// A constraint whose bars depend on the instrument: a session, or a
    /// bucket of the per-instrument sessions calendar.
    fn per_entity_window(&self, kind: &WindowKind) -> bool {
        match kind {
            WindowKind::Session => true,
            WindowKind::Bucket(c) => c.per_entity(),
            _ => false,
        }
    }

    /// `session_of(A, T, S)`: the label of A's session holding base bar T.
    fn session_label(&self, sym: Sym, t: i64) -> Option<i64> {
        let v = self.sessions.get(&sym)?;
        let i = v.partition_point(|(_, l)| *l < t);
        let (open, label) = *v.get(i)?;
        (open < t && t <= label).then_some(label)
    }

    /// The value of a case form on one binding: the first branch whose
    /// guard holds binds the variable to its value (Definition 5.9); with
    /// no branch and no `else` the instance is excluded.
    fn case_value(&mut self, cr: &Arc<CompiledRule>, branches: &[Branch], otherwise: Option<&Expr>, env: &[Option<Value>]) -> Result<Option<Value>, RunError> {
        for b in branches {
            let order: Vec<usize> = (0..b.guard.len()).collect();
            let (sols, stuck) = self.solve_kleene(cr, &b.guard, &order, vec![env.to_vec()], None)?;
            if let Some(sol) = sols.into_iter().next() {
                return Ok(Some(self.eval_expr(cr, &b.value, &sol)?));
            }
            if let Some(s) = stuck.into_iter().next() {
                return Err(s.error);
            }
        }
        match otherwise {
            Some(e) => Ok(Some(self.eval_expr(cr, e, env)?)),
            None => Ok(None),
        }
    }

    /// The rule's time domain as a sorted vector (cached per calendar, and
    /// per instrument on the sessions calendar): the bars the running sums
    /// walk, with their indices.
    fn domain_bars(&mut self, cr: &CompiledRule, env: &[Option<Value>]) -> Result<Arc<Vec<i64>>, RunError> {
        let cal = self.prog.infos[cr.idx].cal.clone();
        let entity = if cal.per_entity() {
            cr.entity_slot.and_then(|s| env.get(s)).and_then(|v| v.as_ref()).and_then(|v| v.as_equity())
        } else {
            None
        };
        let key = (cal, entity);
        if let Some(v) = self.domain_vecs.get(&key) {
            return Ok(v.clone());
        }
        let v: Arc<Vec<i64>> = Arc::new(self.domain(cr, env)?.iter().copied().collect());
        self.domain_vecs.insert(key, v.clone());
        Ok(v)
    }

    pub(crate) fn step<S: Sink>(&mut self, cr: &Arc<CompiledRule>, lit: &Literal, env: &[Option<Value>], out: &mut S) -> Result<(), RunError> {
        match lit {
            Literal::Atom(a) => self.atom_matches_into(cr, a, env, out)?,
            Literal::AsOf { atom, at, within_bucket, .. } => self.asof_matches(cr, atom, at, within_bucket.as_ref(), env, out)?,
            Literal::Neg(a) => {
                if self.atom_matches(cr, a, env)?.is_empty() {
                    out.push_env(env);
                }
            }
            Literal::Builtin(b, _) => match b {
                Builtin::Prev { t, t1 } => {
                    let at = self.time_of(cr, t, env)?;
                    // The argument's calendar: a session label's previous
                    // label is the instrument's previous session.
                    let dom = self.domain_of_term(cr, t, env)?;
                    if let Some(&p) = dom.range(..at).next_back() {
                        self.bind_time(cr, t1, env, p, out);
                    }
                }
                Builtin::Lag { t, n, t1 } => {
                    let at = self.time_of(cr, t, env)?;
                    let Value::Dur(d) = self.eval_expr(cr, n, env)? else {
                        return Err(RunError::Internal("lag length is not a Duration".into()));
                    };
                    let bound = time::sub_duration(at, *d);
                    if let Some(&p) = self.domain_of_term(cr, t, env)?.range(..=bound).next_back() {
                        self.bind_time(cr, t1, env, p, out);
                    }
                }
                Builtin::MonthStart { t } => {
                    let at = self.time_of(cr, t, env)?;
                    let prev = self.domain_of_term(cr, t, env)?.range(..at).next_back().copied();
                    if prev.map(|p| time::month_key(p) != time::month_key(at)).unwrap_or(true) {
                        out.push_env(env);
                    }
                }
                Builtin::DayStart { t } => {
                    let at = self.time_of(cr, t, env)?;
                    let prev = self.domain_of_term(cr, t, env)?.range(..at).next_back().copied();
                    if prev.map(|p| time::day_key(p) != time::day_key(at)).unwrap_or(true) {
                        out.push_env(env);
                    }
                }
                Builtin::SessionOf { a, t, s } => {
                    let sym = match self.term_value(cr, a, env)? {
                        Value::Equity(x) => x,
                        _ => return Err(RunError::Internal("session_of needs an instrument".into())),
                    };
                    let t = self.time_of(cr, t, env)?;
                    if let Some(label) = self.session_label(sym, t) {
                        self.bind_time(cr, s, env, label, out);
                    }
                }
            },
            Literal::Window {
                kind: WindowKind::Rows | WindowKind::PriorRows,
                ..
            } => {
                return Err(RunError::Internal("a `rows` window is solved by its aggregation".into()));
            }
            Literal::Window { var, kind, base, dur, lo, .. } => {
                // A session constraint, or a bucket of the per-instrument
                // sessions calendar, with the instrument unbound: the bars
                // of every instrument's session there, each with the
                // instrument bound (the group keys of a view rule).
                if self.per_entity_window(kind) {
                    if let Some(slot) = cr.entity_slot.filter(|&s| env[s].is_none()) {
                        let t = self.time_of(cr, base, env)?;
                        let mut syms: Vec<Sym> = self
                            .sessions
                            .iter()
                            .filter(|(_, v)| {
                                let i = v.partition_point(|(_, l)| *l < t);
                                v.get(i).map(|(o, l)| *o < t && t <= *l).unwrap_or(false)
                            })
                            .map(|(s, _)| *s)
                            .collect();
                        syms.sort_unstable();
                        for sym in syms {
                            let mut e = env.to_vec();
                            e[slot] = Some(Value::Equity(sym));
                            let times = self.window_times(cr, kind, base, dur, lo.as_ref(), &e)?;
                            for t1 in times {
                                self.bind_time(cr, var, &e, t1, out);
                            }
                        }
                        return Ok(());
                    }
                }
                let times = self.window_times(cr, kind, base, dur, lo.as_ref(), env)?;
                for t1 in times {
                    self.bind_time(cr, var, env, t1, out);
                }
            }
            Literal::Case { var, branches, otherwise, .. } => {
                if let Some(v) = self.case_value(cr, branches, otherwise.as_ref(), env)? {
                    let slot = cr.slots[var];
                    match &env[slot] {
                        Some(x) => {
                            if values_equal(x, &v) {
                                out.push_env(env);
                            }
                        }
                        None => out.push_with(env, slot, Some(v)),
                    }
                }
            }
            Literal::Cmp { op, lhs, rhs, .. } => {
                let a = self.eval_expr(cr, lhs, env)?;
                let b = self.eval_expr(cr, rhs, env)?;
                if compare(*op, &a, &b) {
                    out.push_env(env);
                }
            }
            Literal::Assign { var, expr, .. } => {
                let v = self.eval_expr(cr, expr, env)?;
                let slot = cr.slots[var];
                match &env[slot] {
                    Some(x) => {
                        if values_equal(x, &v) {
                            out.push_env(env);
                        }
                    }
                    None => out.push_with(env, slot, Some(v)),
                }
            }
            Literal::Agg { var, agg, args, conj, within, .. } => {
                // The literal's position in the body, found once for the
                // pushed answers and the plan, which are both kept by it.
                let pos = self.body_position(cr, lit);
                // This binding's window step, run by the block evaluator's
                // operator before it declined the bar: its outcome as it was.
                if !self.rows_replay.is_empty() {
                    if let Some(i) = self.rows_replay.iter().position(|(ri, li, e, _)| *ri == cr.idx && Some(*li) == pos && e.as_slice() == env) {
                        let (_, _, _, outcome) = self.rows_replay.swap_remove(i);
                        return outcome.map(|rows| rows.iter().for_each(|r| out.push_env(r)));
                    }
                }
                if self.pushed_answers(cr, pos, var, env, out) {
                    return Ok(());
                }
                let mut min_count: i64 = 0;
                for l in conj {
                    if let Literal::Window { min, .. } = l {
                        let k = self.eval_expr(cr, min, env)?.as_f64().unwrap_or(0.0) as i64;
                        min_count = min_count.max(k);
                    }
                }
                self.stats.window_calls += 1;
                let plan = self.agg_plan_at(cr, pos, lit, conj, args, within.as_deref());
                let group_slots: Vec<usize> = plan.group.iter().copied().filter(|&s| env[s].is_none()).collect();
                let grouped = !group_slots.is_empty();
                // The rows of the group with the aggregate's arguments evaluated
                // on each: a `rows` window walks back over the group's own
                // bars; otherwise from the per-bar cache when the group has
                // exactly one window (the common case), else solved whole.
                let has_rows = conj.iter().any(|l| {
                    matches!(
                        l,
                        Literal::Window {
                            kind: WindowKind::Rows | WindowKind::PriorRows,
                            ..
                        }
                    )
                });
                // A rows window whose time variable is the first slot the
                // conjunction binds yields rows already in the order the
                // environments sort to (time order, one row a bar): no
                // environments are materialised and nothing is sorted.
                let mut in_order = false;
                if has_rows && self.cfg.window_cache && !grouped {
                    if let Some((count, value)) = self.rows_columns(cr, lit, conj, agg, args, env, plan.f, min_count)? {
                        if (count as i64) < min_count {
                            return Ok(());
                        }
                        let value = match value {
                            Ok(Some(v)) => v,
                            Ok(None) => return Ok(()),
                            Err(m) => return Err(self.agg_error(cr, env, agg, args, &m)),
                        };
                        out.push_with(env, cr.slots[var], Some(value));
                        return Ok(());
                    }
                }
                // A calendar window over the per-bar cache: its count and its
                // arguments' columns, in the order the rows sort to.
                if let Some(wp) = plan.window.as_ref().filter(|_| !grouped && !has_rows && self.cfg.window_cache) {
                    if let Some((count, cols)) = self.window_columns(cr, wp, conj, agg, args, env)? {
                        if (count as i64) < min_count {
                            return Ok(());
                        }
                        let value = if agg == "count" {
                            Value::Count(count as i64)
                        } else {
                            match plan.f.map_or_else(|| aggregate(agg, &cols), |f| reduce_columns(f, &cols)) {
                                Ok(Some(v)) => v,
                                Ok(None) => return Ok(()),
                                Err(m) => {
                                    let a: Vec<String> = args.iter().map(|e| e.to_string()).collect();
                                    return Err(self.arith_text(cr, env, format!("{}({}) over (...)", agg, a.join(", ")), &m));
                                }
                            }
                        };
                        out.push_with(env, cr.slots[var], Some(value));
                        return Ok(());
                    }
                }
                let cached = if grouped {
                    None
                } else if has_rows {
                    let ordered = self.time_ordered(cr, lit, conj, env);
                    let (rows, sorted) = self.rows_window(cr, lit, conj, agg, args, env, !ordered)?;
                    in_order = sorted;
                    Some(rows)
                } else {
                    None
                };
                let mut rows: Vec<(Env, Vec<f64>)> = match cached {
                    Some(rows) => rows,
                    None => {
                        // Count the bars enumerated, as the cache does, for the stats.
                        for l in conj {
                            if let Literal::Window {
                                kind,
                                base: base @ Term::Var(..),
                                dur,
                                lo,
                                ..
                            } = l
                            {
                                if !matches!(kind, WindowKind::Rows | WindowKind::PriorRows) {
                                    self.stats.window_bars_solved += self.window_times(cr, kind, base, dur, lo.as_ref(), env)?.len();
                                }
                            }
                        }
                        let order = conj_order(conj);
                        let sols = self.solve(cr, &order, vec![env.to_vec()])?;
                        let mut rows = Vec::with_capacity(sols.len());
                        for row in sols {
                            let vals = if agg == "count" { Vec::new() } else { self.agg_args(cr, args, &row)? };
                            rows.push((row, vals));
                        }
                        rows
                    }
                };
                if (rows.len() as i64) < min_count {
                    return Ok(());
                }
                if !in_order {
                    // A window's rows are read in its time order (P-1.2;
                    // Definition V-3.5: `first` and `last` are the window's
                    // end rows), the rows of one bar in the order of their
                    // other bindings (ERRATA E-46).
                    let wslot = conj.iter().find_map(|l| match l {
                        Literal::Window { var: Term::Var(v, _), .. } => cr.slots.get(v).copied(),
                        _ => None,
                    });
                    match wslot {
                        Some(w) => rows.sort_by(|a, b| a.0[w].cmp(&b.0[w]).then_with(|| a.0.cmp(&b.0))),
                        None => rows.sort_by(|a, b| a.0.cmp(&b.0)),
                    }
                }
                if grouped {
                    let mut groups: BTreeMap<Vec<Option<Value>>, Vec<usize>> = BTreeMap::new();
                    for (i, (row, _)) in rows.iter().enumerate() {
                        let key: Vec<Option<Value>> = group_slots.iter().map(|&s| row[s].clone()).collect();
                        groups.entry(key).or_default().push(i);
                    }
                    for (key, idx) in groups {
                        if (idx.len() as i64) < min_count {
                            continue;
                        }
                        let value = if agg == "count" {
                            Value::Count(idx.len() as i64)
                        } else {
                            let mut cols: Vec<Vec<f64>> = vec![Vec::with_capacity(idx.len()); args.len()];
                            for &i in &idx {
                                for (j, v) in rows[i].1.iter().enumerate() {
                                    cols[j].push(*v);
                                }
                            }
                            match aggregate(agg, &cols) {
                                Ok(Some(v)) => v,
                                Ok(None) => continue,
                                Err(m) => {
                                    let a: Vec<String> = args.iter().map(|e| e.to_string()).collect();
                                    return Err(self.arith_text(cr, env, format!("{}({}) over (...)", agg, a.join(", ")), &m));
                                }
                            }
                        };
                        out.push_env(env);
                        let e = out.last_row().expect("a row was pushed");
                        for (s, v) in group_slots.iter().zip(key) {
                            e[*s] = v;
                        }
                        e[cr.slots[var]] = Some(value);
                    }
                    return Ok(());
                }
                if agg == "count" {
                    out.push_with(env, cr.slots[var], Some(Value::Count(rows.len() as i64)));
                    return Ok(());
                }
                let mut cols: Vec<Vec<f64>> = vec![Vec::with_capacity(rows.len()); args.len()];
                for (_, vals) in &rows {
                    for (j, v) in vals.iter().enumerate() {
                        cols[j].push(*v);
                    }
                }
                let value = match aggregate(agg, &cols) {
                    Ok(Some(v)) => v,
                    Ok(None) => return Ok(()),
                    Err(m) => {
                        let a: Vec<String> = args.iter().map(|e| e.to_string()).collect();
                        return Err(self.arith_text(cr, env, format!("{}({}) over (...)", agg, a.join(", ")), &m));
                    }
                };
                out.push_with(env, cr.slots[var], Some(value));
            }
            Literal::Top { n, atom, by, rank, .. } => {
                let mut rows = self.atom_matches(cr, atom, env)?;
                let keys: Vec<(usize, Dir)> = by.as_ref().map(|b| b.iter().map(|(k, d, _)| (cr.slots[k], *d)).collect()).unwrap_or_default();
                let by_keys = |a: &Env, b: &Env| -> std::cmp::Ordering {
                    for (slot, dir) in &keys {
                        let o = a[*slot].cmp(&b[*slot]);
                        let o = if *dir == Dir::Desc { o.reverse() } else { o };
                        if o != std::cmp::Ordering::Equal {
                            return o;
                        }
                    }
                    std::cmp::Ordering::Equal
                };
                rows.sort_by(|a, b| by_keys(a, b).then_with(|| a.cmp(b)));
                if let Some(n) = n {
                    let n = self.eval_expr(cr, n, env)?.as_f64().unwrap_or(0.0).max(0.0) as usize;
                    rows.truncate(n);
                }
                if let Some((k, ties, _)) = rank {
                    // 1-based positions; a run of equal keys shares the mean
                    // of its positions under `ties average`.
                    let slot = cr.slots[k];
                    let mut i = 0;
                    while i < rows.len() {
                        let mut j = i + 1;
                        if *ties == Ties::Average {
                            while j < rows.len() && by_keys(&rows[i], &rows[j]) == std::cmp::Ordering::Equal {
                                j += 1;
                            }
                        }
                        let r = (i + 1 + j) as f64 / 2.0;
                        for row in &mut rows[i..j] {
                            row[slot] = Some(Value::Num(if *ties == Ties::Average { r } else { (i + 1) as f64 }));
                        }
                        i = j;
                    }
                }
                for row in rows {
                    out.push_owned(row);
                }
            }
        }
        Ok(())
    }

    /// The pushed answers of an aggregate literal at this binding (K-4.5):
    /// the data layer's value for the bound instrument at the bound base
    /// time or, when the aggregation binds the instrument, every
    /// instrument's at that time, in the instrument order the local grouping
    /// gives; only when every row they read was received by the barrier
    /// being evaluated (ERRATA E-47). False when the window is evaluated here.
    fn pushed_answers<S: Sink>(&mut self, cr: &CompiledRule, pos: Option<usize>, var: &str, env: &[Option<Value>], out: &mut S) -> bool {
        if self.pushed.is_empty() {
            return false;
        }
        let Some(pos) = pos else { return false };
        let Some(p) = self.pushed.get(&(cr.idx, pos)).cloned() else { return false };
        let Some(Some(Value::Time(t))) = env.get(p.base_slot) else { return false };
        let Some(section) = p.by_time.get(t) else { return false };
        let vslot = cr.slots[var];
        match env.get(p.entity_slot) {
            Some(Some(Value::Equity(a))) => {
                let Ok(i) = section.binary_search_by_key(a, |(s, _)| *s) else { return false };
                let ans = &section[i].1;
                let (Some(x), false, true) = (ans.value, ans.error, ans.r <= self.now) else { return false };
                self.pushed_read_r = self.pushed_read_r.max(ans.r);
                out.push_with(env, vslot, Some(Value::Num(x)));
            }
            Some(None) => {
                if section.iter().any(|(_, a)| a.value.is_none() || a.error || a.r > self.now) {
                    return false;
                }
                for (s, a) in section {
                    self.pushed_read_r = self.pushed_read_r.max(a.r);
                    out.push_env(env);
                    let e = out.last_row().expect("a row was pushed");
                    e[p.entity_slot] = Some(Value::Equity(*s));
                    e[vslot] = a.value.map(Value::Num);
                }
            }
            _ => return false,
        }
        self.stats.pushed_answers += 1;
        true
    }

    /// A window's length or minimum, which only parameters and literals
    /// make (K-4.5 (b)).
    pub(crate) fn static_value(&mut self, rule: usize, e: &Expr) -> Result<Value, RunError> {
        let cr = self.compiled[rule].clone();
        let env: Env = vec![None; cr.nslots];
        self.eval_expr(&cr, e, &env)
    }

    /// The aggregate's arguments on one row of its group, as numbers.
    fn agg_args(&mut self, cr: &CompiledRule, args: &[Expr], row: &[Option<Value>]) -> Result<Vec<f64>, RunError> {
        let mut vals = Vec::with_capacity(args.len());
        for a in args {
            let v = self.eval_expr(cr, a, row)?;
            vals.push(v.as_f64().ok_or_else(|| self.arith(cr, row, a, "aggregate argument is not numeric"))?);
        }
        Ok(vals)
    }

    /// The aggregation literal's plan, computed on its first call: by its
    /// position in the rule's body (one nested in a case is planned at
    /// each call).
    fn agg_plan(&mut self, cr: &CompiledRule, lit: &Literal, conj: &[Literal], args: &[Expr], within: Option<&[String]>) -> Arc<AggPlan> {
        let pos = self.body_position(cr, lit);
        self.agg_plan_at(cr, pos, lit, conj, args, within)
    }

    /// The position of a literal in its rule's body, `None` for one nested
    /// in another (a case's guard): found by its address among the body's
    /// literals at each use, since a literal has no other name (E-130).
    fn body_position(&self, cr: &CompiledRule, lit: &Literal) -> Option<usize> {
        self.prog.rules[cr.idx].body.iter().position(|l| std::ptr::eq(l, lit))
    }

    /// `agg_plan` for a literal whose position in the body is in hand.
    fn agg_plan_at(&mut self, cr: &CompiledRule, pos: Option<usize>, lit: &Literal, conj: &[Literal], args: &[Expr], within: Option<&[String]>) -> Arc<AggPlan> {
        if let Some(p) = pos.and_then(|p| self.agg_plans.get(&(cr.idx, p))) {
            return p.clone();
        }
        // The group (language-v2, 4.2; semantic model, 5.1): with `within`,
        // exactly its variables; otherwise the variables the conjunction
        // shares with the rest of the rule (the head or another literal). A
        // variable of the conjunction alone is local to it, neither a key
        // nor bound by the aggregation. A group variable bound outside is an
        // input of the group; an unbound one is a key the aggregation binds,
        // with one result per distinct binding (`close@C(A, T, P) :- P =
        // last(P1) over (...)` groups by A).
        let group_vars: Vec<String> = match within {
            Some(w) => w.to_vec(),
            None => {
                let mut aggregated: Vec<String> = Vec::new();
                for l in conj {
                    if let Literal::Window { var: Term::Var(v, _), .. } = l {
                        aggregated.push(v.clone());
                    }
                }
                for a in args {
                    let mut vs = Vec::new();
                    a.vars(&mut vs);
                    aggregated.extend(vs.into_iter().map(|(v, _)| v));
                }
                let outer = outer_vars(&self.prog.rules[cr.idx], lit);
                let mut mentioned: Vec<String> = Vec::new();
                for l in conj {
                    literal_vars(l, &mut mentioned);
                }
                mentioned.into_iter().filter(|v| !aggregated.contains(v) && outer.contains(v)).collect()
            }
        };
        let mut group: Vec<usize> = group_vars.iter().filter_map(|v| cr.slots.get(v).copied()).collect();
        group.sort_unstable();
        group.dedup();
        let plan = Arc::new(AggPlan {
            group,
            window: self.window_plan(cr, lit, conj, args),
            f: match lit {
                Literal::Agg { agg, .. } if agg != "count" && agg != "quantile" => Function::parse(agg, None).ok(),
                _ => None,
            },
        });
        if let Some(p) = pos {
            self.agg_plans.insert((cr.idx, p), plan.clone());
        }
        plan
    }

    /// The shape of a calendar window served from the per-bar cache, or
    /// `None` when no call can be: more than one window, a `rows` window, or
    /// the base time read otherwise than by a test the cache can apply.
    fn window_plan(&self, cr: &CompiledRule, lit: &Literal, conj: &[Literal], args: &[Expr]) -> Option<WindowPlan> {
        let windows: Vec<usize> = conj.iter().enumerate().filter(|(_, l)| matches!(l, Literal::Window { .. })).map(|(i, _)| i).collect();
        if windows.len() != 1 {
            return None;
        }
        let Literal::Window {
            var: Term::Var(wvar, _),
            kind,
            base: Term::Var(bvar, _),
            dur,
            min,
            ..
        } = &conj[windows[0]]
        else {
            return None;
        };
        if matches!(kind, WindowKind::Rows | WindowKind::PriorRows) {
            return None;
        }
        // A test of the base time (`Ex = T`: the dividend going ex at the
        // bar), or a comparison with what only the call binds (`Ex > T1`,
        // after the calendar's previous bar, E-145), is applied to the rows
        // each call assembles; the rest of the conjunction is solved once a
        // bar and cached, and is not keyed by what the test reads. The
        // tests bind nothing, fail on no value, and the rows are sorted
        // after, so the aggregate reads what a whole solve gives it.
        let reads_base = |l: &Literal| {
            let mut vs = Vec::new();
            literal_vars(l, &mut vs);
            vs.iter().any(|v| v == bvar)
        };
        // A comparison's variable that no other literal of the conjunction
        // mentions is bound by the call.
        let mut inner: Vec<String> = Vec::new();
        for l in conj.iter().filter(|l| !matches!(l, Literal::Cmp { .. })) {
            literal_vars(l, &mut inner);
        }
        let reads_call = |l: &Literal| match l {
            Literal::Cmp { lhs, rhs, .. } if total_expr(lhs) && total_expr(rhs) => {
                let mut vs = Vec::new();
                literal_vars(l, &mut vs);
                vs.iter().any(|v| !inner.contains(v))
            }
            _ => false,
        };
        // A variable an atom of the cached conjunction binds (a match binds
        // all of an atom's variables).
        let atom_binds = |v: &String| {
            conj.iter().any(|l| match l {
                Literal::Atom(a) | Literal::AsOf { atom: a, .. } => !reads_base(l) && a.terms.iter().any(|t| matches!(t, Term::Var(x, _) if x == v)),
                _ => false,
            })
        };
        let mut late: Vec<&Literal> = Vec::new();
        // The outer bindings the cached conjunction reads (not the window
        // variable): the rows of a bar are a function of them and the bar.
        let mut refs: Vec<String> = Vec::new();
        for (i, l) in conj.iter().enumerate() {
            if i == windows[0] {
                continue;
            }
            if reads_base(l) {
                match l {
                    Literal::Cmp { lhs, rhs, .. } if total_expr(lhs) && total_expr(rhs) => late.push(l),
                    Literal::Assign { expr, .. } if total_expr(expr) => late.push(l),
                    // Any other reading of the base time makes rows that
                    // change with every call: nothing to share, so solve it
                    // whole.
                    _ => return None,
                }
                continue;
            }
            if reads_call(l) {
                late.push(l);
                continue;
            }
            literal_vars(l, &mut refs);
        }
        for a in args {
            let mut vs = Vec::new();
            a.vars(&mut vs);
            refs.extend(vs.into_iter().map(|(v, _)| v));
        }
        if refs.iter().any(|v| v == bvar) {
            return None;
        }
        // A test reads only what the call or the cached rows bind; an
        // assignment is a test of a variable an atom or the call binds.
        let mut tests = Vec::with_capacity(late.len());
        for l in late {
            let mut vs = Vec::new();
            literal_vars(l, &mut vs);
            let mut need = Vec::new();
            for v in vs.iter().filter(|v| *v != bvar && !refs.contains(v)) {
                need.push(*cr.slots.get(v)?);
            }
            if let Literal::Assign { var, .. } = l {
                if !atom_binds(var) {
                    need.push(*cr.slots.get(var)?);
                }
            }
            need.sort_unstable();
            need.dedup();
            tests.push((l.clone(), need));
        }
        // The window's own length and minimum tell groups of different
        // windows over one conjunction apart.
        for e in [dur, min] {
            let mut vs = Vec::new();
            e.vars(&mut vs);
            refs.extend(vs.into_iter().map(|(v, _)| v));
        }
        let wslot = *cr.slots.get(wvar)?;
        let mut outer = Vec::with_capacity(refs.len());
        for v in &refs {
            let s = *cr.slots.get(v)?;
            if s != wslot {
                outer.push(s);
            }
        }
        outer.sort_unstable();
        outer.dedup();
        Some(WindowPlan {
            window: windows[0],
            wslot,
            // The literal by its position in the rule's body (aggregations
            // do not nest), so that the key survives a checkpoint.
            literal: self.literal_index(cr, lit),
            late: tests,
            outer,
            order: conj_order(conj)
                .into_iter()
                .filter(|l| !matches!(l, Literal::Window { .. }) && !reads_base(l) && !reads_call(l))
                .collect(),
        })
    }

    /// The rows of a windowed group from the per-bar cache (data-bundle doc,
    /// section 2, "State"): the group is the rule, the aggregation literal
    /// and the outer bindings its conjunction reads apart from the window's
    /// base time; each bar of the window is solved once, with the window
    /// variable bound to it and the rest of the conjunction in evaluation
    /// order, and kept until it leaves the window. `None` when this call
    /// cannot be served so (solved whole instead).
    /// A bucket or span window's conjunction solved for every row of the
    /// block at once (`PrefetchBucket`): the bars the first row's group
    /// misses, each solved over the whole block, each row then holding its
    /// own solutions in its own solve's order.
    fn prefetch_bucket_window(&mut self, cr: &Arc<CompiledRule>, lit: &Literal, conj: &[Literal], args: &[Expr], within: Option<&[String]>, live: &Block) -> Result<Option<PrefetchBucket>, RunError> {
        const MIN_ROWS: usize = 32;
        if live.len() < MIN_ROWS || !self.cfg.window_cache {
            return Ok(None);
        }
        let aplan = self.agg_plan(cr, lit, conj, args, within);
        let Some(wp) = aplan.window.as_ref() else { return Ok(None) };
        if !wp.late.is_empty() {
            return Ok(None);
        }
        let Literal::Window { kind, base, dur, lo: lo_off, .. } = &conj[wp.window] else { return Ok(None) };
        if matches!(kind, WindowKind::Rows | WindowKind::PriorRows) || self.per_entity_window(kind) {
            return Ok(None);
        }
        let first = live.row(0);
        if first[wp.wslot].is_some() || aplan.group.iter().any(|&s| first[s].is_none()) {
            return Ok(None);
        }
        let t = self.time_of(cr, base, first)?;
        for i in 1..live.len() {
            let r = live.row(i);
            if r[wp.wslot].is_some() || self.time_of(cr, base, r)? != t {
                return Ok(None);
            }
        }
        let times: Vec<i64> = self.window_times(cr, kind, base, dur, lo_off.as_ref(), first)?;
        let lo = times.first().copied().unwrap_or(t);
        // The bars the first row's group misses: what every group of this
        // block is about to ask for.
        let key = WindowKey {
            rule: cr.idx,
            literal: wp.literal,
            outer: wp.outer.iter().map(|&s| first[s].clone()).collect(),
        };
        let bars: Vec<i64> = match self.windows.get(&key) {
            Some(cache) => {
                let have: Vec<i64> = cache.range(lo, t).map(|(k, _)| *k).collect();
                times.iter().copied().filter(|t1| have.binary_search(t1).is_err()).collect()
            }
            None => times.clone(),
        };
        if bars.is_empty() {
            return Ok(None);
        }
        let n = live.len();
        let mut rows: Vec<Option<Vec<Vec<Env>>>> = vec![Some(vec![Vec::new(); bars.len()]); n];
        let order: Vec<usize> = (0..wp.order.len()).collect();
        for (k, &t1) in bars.iter().enumerate() {
            let mut start = Block::with_capacity(cr.nslots, n);
            for i in 0..n {
                let mut e = live.row(i).to_vec();
                e[wp.wslot] = Some(Value::Time(t1));
                start.push_row(&e, i as u32);
            }
            let (sb, sstuck) = self.solve_kleene_block(cr, &wp.order, &order, start, None)?;
            for j in 0..sb.len() {
                if let Some(r) = &mut rows[sb.origin_of(j) as usize] {
                    r[k].push(sb.row(j).to_vec());
                }
            }
            for s in sstuck {
                rows[s.origin as usize] = None;
            }
        }
        Ok(Some(PrefetchBucket {
            rule: cr.idx,
            literal: wp.literal,
            bars,
            rows,
        }))
    }

    fn window_columns(&mut self, cr: &Arc<CompiledRule>, plan: &WindowPlan, conj: &[Literal], agg: &str, args: &[Expr], env: &[Option<Value>]) -> Result<Option<WindowColumns>, RunError> {
        let Literal::Window { kind, base, dur, lo: lo_off, .. } = &conj[plan.window] else {
            return Ok(None);
        };
        let wslot = plan.wslot;
        if env[wslot].is_some() {
            return Ok(None);
        }
        // A per-instrument constraint with the instrument unbound enumerates
        // the instruments: solved whole.
        if self.per_entity_window(kind) && cr.entity_slot.map(|s| env[s].is_none()).unwrap_or(true) {
            return Ok(None);
        }
        let t = self.time_of(cr, base, env)?;
        let times: Vec<i64> = self.window_times(cr, kind, base, dur, lo_off.as_ref(), env)?;
        let lo = times.first().copied().unwrap_or(t);
        if plan.late.iter().any(|(_, need)| need.iter().any(|&s| env[s].is_none())) {
            return Ok(None);
        }
        let late = &plan.late;
        let key = WindowKey {
            rule: cr.idx,
            literal: plan.literal,
            outer: plan.outer.iter().map(|&s| env[s].clone()).collect(),
        };
        let order = &plan.order;
        let mut cache = self.windows.remove(&key).unwrap_or_default();
        // The window's bars the cache does not hold yet (usually the newest):
        // the bars and the cache's keys are both in order, so one merge.
        let mut missing = Vec::new();
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
        let pre: Option<(Vec<i64>, Vec<Vec<Env>>)> = self
            .prefetch_bucket
            .as_ref()
            .filter(|p| p.rule == cr.idx && p.literal == plan.literal)
            .and_then(|p| p.rows.get(self.prefetch_row).cloned().flatten().map(|r| (p.bars.clone(), r)));
        for t1 in missing {
            // A row stuck or failing here may be one a deferred test
            // excludes (Kleene conjunction, V-5): the whole solve decides.
            let fetched = pre.as_ref().and_then(|(bars, rows)| bars.iter().position(|&b| b == t1).map(|k| rows[k].clone()));
            let sols = match fetched {
                Some(s) => s,
                None => {
                    let mut e = env.to_vec();
                    e[wslot] = Some(Value::Time(t1));
                    match self.solve(cr, order, vec![e]) {
                        Ok(s) => s,
                        Err(err) => {
                            self.windows.insert(key, cache);
                            return if late.is_empty() { Err(err) } else { Ok(None) };
                        }
                    }
                }
            };
            self.stats.window_bars_solved += 1;
            let mut rows = Vec::with_capacity(sols.len());
            for row in sols {
                let vals = if agg == "count" {
                    Vec::new()
                } else {
                    match self.agg_args(cr, args, &row) {
                        Ok(v) => v,
                        Err(err) => {
                            self.windows.insert(key, cache);
                            return if late.is_empty() { Err(err) } else { Ok(None) };
                        }
                    }
                };
                rows.push((row, vals));
            }
            cache.insert(t1, rows);
        }
        // A cached row carries the outer bindings of the call that solved it;
        // what the aggregate sees is this call's outer bindings plus the
        // conjunction's own (the slots unbound here), as a whole solve would
        // produce. Its rows are read in the window's time order, those of one
        // bar in the order of their own bindings (ERRATA E-46), which is the
        // order a sort of the whole rows gives: the bound slots are equal.
        let unbound: Vec<usize> = (0..env.len()).filter(|&i| env[i].is_none()).collect();
        let assembled: Result<WindowColumns, RunError> = 'assemble: {
            let mut count = 0usize;
            let mut cols: Vec<Vec<f64>> = vec![Vec::new(); if agg == "count" { 0 } else { args.len() }];
            let mut held = cache.range(lo, t).peekable();
            for &t1 in &times {
                while held.peek().is_some_and(|(k, _)| *k < t1) {
                    held.next();
                }
                let Some((_, bar)) = held.next_if(|(k, _)| *k == t1) else { continue };
                let mut order: Vec<usize> = (0..bar.len()).collect();
                if bar.len() > 1 {
                    order.sort_by(|&a, &b| unbound.iter().map(|&s| bar[a].0[s].cmp(&bar[b].0[s])).find(|o| o.is_ne()).unwrap_or(std::cmp::Ordering::Equal));
                }
                'rows: for i in order {
                    let (row, vals) = &bar[i];
                    if !late.is_empty() {
                        let mut e = env.to_vec();
                        for &s in &unbound {
                            e[s] = row[s].clone();
                        }
                        for (l, _) in late {
                            let holds = match l {
                                Literal::Cmp { op, lhs, rhs, .. } => match (self.eval_expr(cr, lhs, &e), self.eval_expr(cr, rhs, &e)) {
                                    (Ok(a), Ok(b)) => compare(*op, &a, &b),
                                    (Err(err), _) | (_, Err(err)) => break 'assemble Err(err),
                                },
                                // As `step` takes an assignment to a bound variable.
                                Literal::Assign { var, expr, .. } => match self.eval_expr(cr, expr, &e) {
                                    Ok(v) => e[cr.slots[var]].as_ref().is_some_and(|x| values_equal(x, &v)),
                                    Err(err) => break 'assemble Err(err),
                                },
                                _ => true,
                            };
                            if !holds {
                                continue 'rows;
                            }
                        }
                    }
                    count += 1;
                    for (c, v) in cols.iter_mut().zip(vals) {
                        c.push(*v);
                    }
                }
            }
            Ok((count, cols))
        };
        // A deferred test that cannot be evaluated is the whole solve's to
        // decide, as are its rows.
        let Ok((count, cols)) = assembled else {
            self.windows.insert(key, cache);
            return Ok(None);
        };
        if self.recording {
            self.summary_rows.push(super::SummaryRow {
                rule: key.rule,
                literal: key.literal,
                outer: key.outer.clone(),
                bar: t,
                agg: agg.to_string(),
                rows: count,
                path: "cached rows",
                shift: None,
                sums: None,
                value: None,
            });
        }
        // Bars before the window's start will not be asked for again by a
        // later bar of the same group; a request for an earlier base time
        // simply solves them again.
        cache.forget_before(lo);
        self.windows.insert(key, cache);
        Ok(Some((count, cols)))
    }

    /// The rows of a `rows` window group: walking back from the base bar,
    /// the N latest bars at which the conjunction's first atom holds for the
    /// group (the group's rows, the index of a rolling window), and of those
    /// the rows where the whole conjunction holds (what the window
    /// aggregates; `min K` counts them, like a rolling window's minimum of
    /// periods). Data-bundle doc, section 2, "State": each bar is solved
    /// once and kept while it can be among the group's last N; a walk stops
    /// at the group's first bar once it is known.
    /// The static shape of a rows window (rule, literal): computed once.
    pub(crate) fn rows_plan(&mut self, cr: &Arc<CompiledRule>, lit: &Literal, conj: &[Literal], args: &[Expr]) -> Result<Arc<RowsPlan>, RunError> {
        let literal = self.literal_index(cr, lit);
        if let Some(p) = self.rows_plans.get(&(cr.idx, literal)) {
            return Ok(p.clone());
        }
        let windows: Vec<usize> = conj.iter().enumerate().filter(|(_, l)| matches!(l, Literal::Window { .. })).map(|(i, _)| i).collect();
        let (
            Some(Literal::Window {
                var: Term::Var(wvar, _),
                base: Term::Var(bvar, _),
                dur,
                kind,
                ..
            }),
            1,
        ) = (windows.first().map(|&i| &conj[i]), windows.len())
        else {
            return Err(RunError::Internal("a `rows` window needs one window, a variable and a bound base time".into()));
        };
        let prior = *kind == WindowKind::PriorRows;
        let mut refs: Vec<String> = Vec::new();
        for (i, l) in conj.iter().enumerate() {
            if i != windows[0] {
                literal_vars(l, &mut refs);
            }
        }
        for a in args {
            let mut vs = Vec::new();
            a.vars(&mut vs);
            refs.extend(vs.into_iter().map(|(v, _)| v));
        }
        let shareable = !refs.iter().any(|v| v == bvar);
        let mut vs = Vec::new();
        dur.vars(&mut vs);
        refs.extend(vs.into_iter().map(|(v, _)| v));
        let wslot = cr.slots[wvar];
        let mut outer: Vec<usize> = refs.iter().map(|v| cr.slots[v]).filter(|&s| s != wslot).collect();
        outer.sort_unstable();
        outer.dedup();
        let order: Vec<Literal> = conj_order(conj).into_iter().filter(|l| !matches!(l, Literal::Window { .. })).collect();
        let anchor: Vec<Literal> = conj.iter().find(|l| matches!(l, Literal::Atom(_))).cloned().into_iter().collect();
        let plain = |l: &Literal| matches!(l, Literal::Atom(a) if !a.terms.iter().any(|t| matches!(t, Term::Ctor(..))));
        let replayable = order.split_last().and_then(|(last, before)| {
            let atoms = if matches!(last, Literal::Cmp { .. } | Literal::Assign { .. }) { before } else { &order[..] };
            let mut derived = Vec::new();
            for l in atoms {
                let Literal::Atom(a) = l else { return None };
                let rel = *self.rel_ids.get(&a.name)?;
                let info = &self.rels[rel];
                if !plain(l) || !(info.stored || info.inputs.is_empty()) {
                    return None;
                }
                // A derived relation is read at the window's bar, the one
                // whose memoised result stands for the read.
                if !info.stored {
                    if !matches!(a.terms.get(info.key_pos), Some(Term::Var(v, _)) if v == wvar) {
                        return None;
                    }
                    derived.push(rel);
                }
            }
            Some(derived)
        });
        // A group per entity of the rule: its states dense by the entity.
        let ent_pos = cr.entity_slot.and_then(|e| outer.iter().position(|&s| s == e));
        let ent = match ent_pos {
            Some(_) if shareable && !prior => {
                self.ent_rows.push(EntRows::default());
                Some(self.ent_rows.len() - 1)
            }
            _ => None,
        };
        let plan = Arc::new(RowsPlan {
            literal,
            wslot,
            base: Term::Var(bvar.clone(), crate::ir::Span::default()),
            dur: dur.clone(),
            outer,
            shareable,
            prior,
            anchor_first: anchor.first().is_some_and(|a| order.first() == Some(a) && plain(a)),
            replayable,
            order,
            anchor,
            ent,
            ent_pos: ent_pos.unwrap_or(0),
        });
        self.rows_plans.insert((cr.idx, literal), plan.clone());
        Ok(plan)
    }

    /// The rows window's conjunction solved at the bar for every row of the
    /// block at once (`Prefetch`), when every row asks the same bar with the
    /// window's time free and reads the window per instrument. Each row's
    /// anchor and solutions are what its own solve yields, in the same order
    /// (the block solver's, V-5); a row on which an instance stuck is left to
    /// the row-at-a-time path.
    fn prefetch_rows_window(&mut self, cr: &Arc<CompiledRule>, lit: &Literal, conj: &[Literal], args: &[Expr], within: Option<&[String]>, live: &Block) -> Result<Option<Prefetch>, RunError> {
        if live.len() < PREFETCH_MIN_ROWS || !self.cfg.window_cache {
            return Ok(None);
        }
        let has_rows = conj.iter().any(|l| {
            matches!(
                l,
                Literal::Window {
                    kind: WindowKind::Rows | WindowKind::PriorRows,
                    ..
                }
            )
        });
        if !has_rows {
            return Ok(None);
        }
        let plan = self.rows_plan(cr, lit, conj, args)?;
        if !plan.shareable || plan.prior {
            return Ok(None);
        }
        let first = live.row(0);
        if first[plan.wslot].is_some() {
            return Ok(None);
        }
        let aplan = self.agg_plan(cr, lit, conj, args, within);
        if aplan.group.iter().any(|&s| first[s].is_none()) {
            return Ok(None);
        }
        let t = self.time_of(cr, &plan.base, first)?;
        for i in 1..live.len() {
            let r = live.row(i);
            if r[plan.wslot].is_some() || self.time_of(cr, &plan.base, r)? != t {
                return Ok(None);
            }
        }
        // A group already brought to this bar has no fresh bar, and
        // `rows_columns` reads a prefetch for a fresh bar only. When that is
        // every row's group (the second aggregate over a shared window,
        // whose state the first one fed on this pass) no row would read the
        // prefetch, and it is not solved, provided the solve would be a
        // pure reading: one that derives a relation, or meets a stuck
        // instance, is counted in the run's statistics and coverage.
        if let Some(derived) = &plan.replayable {
            let mut unread = true;
            for i in 0..live.len() {
                if self.group_state(cr, &plan, live.row(i)).is_none_or(|st| st.upto != t) {
                    unread = false;
                    break;
                }
            }
            let memoised = |k: &Self, rel: usize| {
                let mkey: super::MemoKey = (rel, vec![Value::Time(t)]);
                k.memo_get(&mkey).is_some() && !k.memo_stuck.contains_key(&mkey)
            };
            if unread && derived.iter().all(|&rel| memoised(self, rel)) {
                self.stats.prefetch_skipped += 1;
                return Ok(None);
            }
        }
        let n = live.len();
        let mut start = Block::with_capacity(cr.nslots, n);
        for i in 0..n {
            let mut e = live.row(i).to_vec();
            e[plan.wslot] = Some(Value::Time(t));
            start.push_row(&e, i as u32);
        }
        let mut rows: Vec<Option<(bool, Vec<Env>)>> = vec![Some((plan.anchor.is_empty(), Vec::new())); n];
        // The anchor holds on the rows its block kept; a row it stuck on is
        // the row-at-a-time path's.
        fn anchored(rows: &mut [Option<(bool, Vec<Env>)>], ab: &Block, astuck: &[Stuck]) {
            for j in 0..ab.len() {
                if let Some((a, _)) = &mut rows[ab.origin_of(j) as usize] {
                    *a = true;
                }
            }
            for s in astuck {
                rows[s.origin as usize] = None;
            }
        }
        let order: Vec<usize> = (0..plan.order.len()).collect();
        let (sb, sstuck) = if plan.anchor_first {
            // The anchor is the conjunction's first literal: it is solved
            // once, and the rest of the conjunction goes on from its block
            // and the instances it left stuck, as the whole solve does past
            // its first literal.
            let (ab, astuck) = self.solve_kleene_block(cr, &plan.order, &order[..1], start, None)?;
            anchored(&mut rows, &ab, &astuck);
            if ab.is_empty() && astuck.is_empty() {
                (ab, astuck)
            } else {
                self.solve_kleene_block_from(cr, &plan.order, &order[1..], ab, astuck, None)?
            }
        } else {
            if !plan.anchor.is_empty() {
                let (ab, astuck) = self.solve_kleene_block(cr, &plan.anchor, &[0], start.clone(), None)?;
                anchored(&mut rows, &ab, &astuck);
            }
            self.solve_kleene_block(cr, &plan.order, &order, start, None)?
        };
        for j in 0..sb.len() {
            if let Some((_, v)) = &mut rows[sb.origin_of(j) as usize] {
                v.push(sb.row(j).to_vec());
            }
        }
        for s in sstuck {
            rows[s.origin as usize] = None;
        }
        Ok(Some(Prefetch {
            rule: cr.idx,
            literal: plan.literal,
            t,
            rows,
        }))
    }

    /// A time-ordered rows window kept incrementally per group: each call
    /// solves only the bars since the group's previous call, and the group
    /// holds its last N anchored bars and their rows (`rows_*` below, which
    /// the block evaluator's window operator runs too). The rows' count
    /// and, when there are at least `min_count`, the aggregate over them
    /// (`f`, parsed once, or the aggregate by name). `None` when the window
    /// cannot be served so (a bar with more than one row, a base time read
    /// by the rows, or a call earlier than the group's last): the general
    /// path answers then.
    #[allow(clippy::too_many_arguments)]
    fn rows_columns(
        &mut self,
        cr: &Arc<CompiledRule>,
        lit: &Literal,
        conj: &[Literal],
        agg: &str,
        args: &[Expr],
        env: &[Option<Value>],
        f: Option<Function>,
        min_count: i64,
    ) -> Result<Option<Served>, RunError> {
        let plan = self.rows_plan(cr, lit, conj, args)?;
        if !plan.shareable || plan.prior || env[plan.wslot].is_some() {
            return Ok(None);
        }
        let t = self.time_of(cr, &plan.base, env)?;
        let n = self.eval_expr(cr, &plan.dur, env)?.as_f64().unwrap_or(0.0).max(0.0) as usize;
        // A request behind the group's last bar (a query after the run, a
        // recursion reaching back) is served from a buffer rebuilt by walking
        // back from that bar, exactly as a group first asked there would be,
        // so that the running sums answer it along the same canonical walk;
        // the group's own buffer is left as it is.
        let at = self.group_claim(cr, &plan, env);
        let mut original = self.group_take(&at);
        let backward = original.as_ref().map(|s| s.upto != i64::MIN && t < s.upto).unwrap_or(false);
        let mut st = if backward { Box::default() } else { original.take().unwrap_or_default() };
        if backward {
            self.stats.window_backward += 1;
        }
        // Bring the group up to t: a new group walks back to N (plus slack)
        // anchored bars; a known one solves only the bars since its last call.
        let (fresh, walked) = if st.is_new() {
            let (bars, anchored, from_start) = self.rows_walk(cr, &plan, env, t, n)?;
            st.from_start = from_start;
            (bars, Some(anchored))
        } else if t > st.upto {
            let mut v = std::mem::take(&mut self.fresh_scratch);
            v.clear();
            v.extend(self.domain(cr, env)?.range((std::ops::Bound::Excluded(st.upto), std::ops::Bound::Included(t))).copied());
            (v, None)
        } else {
            (Vec::new(), None)
        };
        // The block's own solve of this bar (`Prefetch`), when the bar is the
        // one the group lacks. The row's entry is taken, not copied: it is
        // this row's alone and this call is its one reader (a call that
        // finds it gone solves the bar itself, to the same rows).
        let mut pre: Option<(bool, Vec<Env>)> = if fresh.len() == 1 && fresh[0] == t {
            let row = self.prefetch_row;
            self.prefetch
                .as_mut()
                .filter(|p| p.rule == cr.idx && p.literal == plan.literal && p.t == t)
                .and_then(|p| p.rows.get_mut(row).and_then(Option::take))
        } else {
            None
        };
        for (i, &t1) in fresh.iter().enumerate() {
            let solved = match pre.take() {
                Some(solved) => solved,
                None => self.rows_solve_bar(cr, &plan, env, t1, walked.as_ref().map(|a| a[i]))?,
            };
            if !self.rows_feed_solved(cr, args, agg, &mut st, t1, solved)? {
                return Ok(None);
            }
        }
        if fresh.capacity() > self.fresh_scratch.capacity() {
            self.fresh_scratch = fresh;
        }
        st.upto = st.upto.max(t);
        let served = self.rows_serve(cr, &plan, &mut st, env, t, n, agg, f, args.len(), min_count)?;
        st.trim(n);
        match (backward, original) {
            (true, Some(o)) => self.group_put(at, o),
            _ => self.group_put(at, st),
        }
        Ok(served)
    }

    /// Where the group of a rows window call keeps its state: dense by the
    /// entity when the plan has a group per entity, the entity is in range
    /// and the call's other outer values are the group's, in the map by
    /// its whole key otherwise.
    pub(crate) fn group_at(&self, cr: &CompiledRule, plan: &RowsPlan, env: &[Option<Value>]) -> GroupAt {
        if let (Some(ent), Some(Value::Equity(s))) = (plan.ent, env[plan.outer[plan.ent_pos]].as_ref()) {
            if *s < ENT_ROWS_MAX && self.ent_rows[ent].admits_now(Self::rest_of(plan, env)) {
                return GroupAt::Ent(ent, *s);
            }
        }
        GroupAt::Map(WindowKey {
            rule: cr.idx,
            literal: plan.literal,
            outer: plan.outer.iter().map(|&s| env[s].clone()).collect(),
        })
    }

    /// A call's outer values other than the entity, in the plan's order.
    fn rest_of<'e>(plan: &'e RowsPlan, env: &'e [Option<Value>]) -> impl Iterator<Item = Option<Value>> + Clone + 'e {
        plan.outer.iter().enumerate().filter(move |(i, _)| *i != plan.ent_pos).map(move |(_, &s)| env[s].clone())
    }

    /// The group's state, if it has one.
    pub(crate) fn group_state(&self, cr: &CompiledRule, plan: &RowsPlan, env: &[Option<Value>]) -> Option<&RowsState> {
        match self.group_at(cr, plan, env) {
            GroupAt::Ent(ent, s) => self.ent_rows[ent].get(s),
            GroupAt::Map(key) => self.rows_state.get(&key).map(|b| &**b),
        }
    }

    /// The group's state taken out while a call works on it.
    pub(crate) fn group_take(&mut self, at: &GroupAt) -> Option<Box<RowsState>> {
        match at {
            GroupAt::Ent(ent, s) => self.ent_rows[*ent].take(*s),
            GroupAt::Map(key) => self.rows_state.remove(key),
        }
    }

    /// The group's state put back; the first state of a dense group names
    /// the outer values its states are for.
    pub(crate) fn group_put(&mut self, at: GroupAt, st: Box<RowsState>) {
        match at {
            GroupAt::Ent(ent, s) => self.ent_rows[ent].put(s, st),
            GroupAt::Map(key) => {
                self.rows_state.insert(key, st);
            }
        }
    }

    /// `group_at` for a call that will keep its state: the first call of a
    /// dense group names its outer values other than the entity.
    pub(crate) fn group_claim(&mut self, cr: &CompiledRule, plan: &RowsPlan, env: &[Option<Value>]) -> GroupAt {
        if let (Some(ent), Some(Value::Equity(s))) = (plan.ent, env[plan.outer[plan.ent_pos]].as_ref()) {
            if *s < ENT_ROWS_MAX && self.ent_rows[ent].admits(Self::rest_of(plan, env)) {
                return GroupAt::Ent(ent, *s);
            }
        }
        self.group_at(cr, plan, env)
    }

    /// A new group's walk back from `t`: the domain's bars from the latest
    /// back until N plus a checkpoint's span plus slack of them are
    /// anchored, or the domain's first bar, in time order with whether each
    /// is anchored, and whether the walk reached the first bar. Each bar's
    /// anchor is what feeding the bar (`rows_solve_bar`) would find, so a
    /// fed bar takes the walk's answer: read as `call` reads it where that
    /// is a lookup (`anchor_holds`), solved otherwise.
    pub(crate) fn rows_walk(&mut self, cr: &Arc<CompiledRule>, plan: &RowsPlan, env: &[Option<Value>], t: i64, n: usize) -> Result<(Vec<i64>, Vec<bool>, bool), RunError> {
        let mut bars: Vec<i64> = Vec::new();
        let mut flags: Vec<bool> = Vec::new();
        let mut anchors = 0usize;
        let mut upper = t;
        let mut from_start = true;
        let mut e = env.to_vec();
        let probe = self.anchor_probe(cr, plan, env);
        let mut key: super::MemoKey = (probe.as_ref().map(|p| p.0).unwrap_or(0), vec![Value::Time(t)]);
        'walk: loop {
            let chunk: Vec<i64> = self.domain(cr, env)?.range(..=upper).rev().take(64).copied().collect();
            if chunk.is_empty() {
                break;
            }
            for &t1 in &chunk {
                let read = probe.as_ref().and_then(|(rel, ent)| self.anchor_holds(*rel, ent, t1, &mut key));
                let anchored = match read {
                    // Debug builds (the tests) solve it too: a lookup is
                    // what the solve would read, with nothing derived.
                    Some(a) if cfg!(debug_assertions) => {
                        e[plan.wslot] = Some(Value::Time(t1));
                        let solved = plan.anchor.is_empty() || !self.solve(cr, &plan.anchor, vec![e.clone()])?.is_empty();
                        assert_eq!(solved, a, "the anchor read at {} differs from the anchor solved", t1);
                        a
                    }
                    Some(a) => a,
                    None => {
                        e[plan.wslot] = Some(Value::Time(t1));
                        plan.anchor.is_empty() || !self.solve(cr, &plan.anchor, vec![e.clone()])?.is_empty()
                    }
                };
                if anchored {
                    anchors += 1;
                }
                bars.push(t1);
                flags.push(anchored);
                if anchors >= n + abt_agg::checkpoint_period(n) + ROWS_SLACK {
                    from_start = false;
                    break 'walk;
                }
            }
            match chunk.last() {
                Some(&last) if last > i64::MIN => upper = last - 1,
                _ => break,
            }
        }
        bars.reverse();
        flags.reverse();
        self.stats.rows_walks += 1;
        self.stats.rows_walked_bars += bars.len();
        Ok((bars, flags, from_start))
    }

    /// The anchor of a walk back read without solving it (`anchor_holds`):
    /// its relation and the call's entity, when the anchor is an atom of a
    /// derived relation with no input whose terms are the rule's entity,
    /// bound, the window's bar, and wildcards (the universe the checker
    /// anchors a window on).
    fn anchor_probe(&self, cr: &CompiledRule, plan: &RowsPlan, env: &[Option<Value>]) -> Option<(usize, Value)> {
        let [Literal::Atom(a)] = plan.anchor.as_slice() else { return None };
        let rel = *self.rel_ids.get(&a.name)?;
        let info = &self.rels[rel];
        let (es, epos) = (cr.entity_slot?, *info.entity_positions.first()?);
        if info.stored || !info.inputs.is_empty() || info.entity_positions.len() != 1 || a.terms.len() <= epos.max(info.key_pos) {
            return None;
        }
        let ok = a.terms.iter().enumerate().all(|(p, term)| match term {
            Term::Var(v, _) if p == epos => cr.slots.get(v) == Some(&es),
            Term::Var(v, _) if p == info.key_pos => cr.slots.get(v) == Some(&plan.wslot),
            Term::Wild(_) => p != epos && p != info.key_pos,
            _ => false,
        });
        let e = env[es].clone().filter(|v| matches!(v, Value::Equity(_)))?;
        ok.then_some((rel, e))
    }

    /// Whether the anchor `rel(e, t1, _...)` holds, read as `Kernel::call`
    /// reads it where that reading is a lookup, branch for branch: a
    /// memoised result (the entity's tuples in it, found as `call` narrows
    /// it), or, behind the memo's oldest bucket with the stores trimmed,
    /// the latest tuples the eviction kept for the entity (E-126). `None`
    /// where `call` would halt, meet a stuck instance or derive: the walk
    /// then solves the anchor. `key` is the memo key, rewritten in place.
    fn anchor_holds(&self, rel: usize, e: &Value, t1: i64, key: &mut super::MemoKey) -> Option<bool> {
        key.1[0] = Value::Time(t1);
        let info = &self.rels[rel];
        let (epos, kpos) = (info.entity_positions[0], info.key_pos);
        let held = |d: &super::Derived| {
            let at = |tu: &Tuple| tu[epos] == *e && tu[kpos] == Value::Time(t1);
            if d.len() > SORTED_MIN {
                let lo = d.partition_point(|(tu, _)| tu[epos] < *e);
                let hi = lo + d[lo..].partition_point(|(tu, _)| tu[epos] <= *e);
                d[lo..hi].iter().any(|(tu, _)| at(tu))
            } else {
                d.iter().any(|(tu, _)| at(tu))
            }
        };
        if let Some(hit) = self.memo_get(key) {
            if self.memo_stuck.contains_key(key) {
                return None;
            }
            return Some(held(hit));
        }
        if self.memo_errors.contains_key(key) {
            return None;
        }
        match self.memo.oldest() {
            Some(oldest) if self.cfg.trim_stores && t1 < oldest => Some(match self.memo.latest_row(rel, e) {
                Some((t, d)) if t == t1 => held(d),
                _ => false,
            }),
            _ => None,
        }
    }

    /// The window's conjunction at bar `t1` for the call's bindings: whether
    /// the anchor holds there and the solutions. `anchored` is the walk's
    /// answer for the bar, when a walk solved it. With the anchor first in
    /// the solve order it is solved once and the rest goes on from it, and
    /// a bar it does not hold at has no solution; otherwise the anchor and
    /// the conjunction are solved apart. Either way the bar yields what
    /// solving the anchor and then the whole conjunction yields.
    pub(crate) fn rows_solve_bar(&mut self, cr: &Arc<CompiledRule>, plan: &RowsPlan, env: &[Option<Value>], t1: i64, anchored: Option<bool>) -> Result<(bool, Vec<Env>), RunError> {
        let mut e = env.to_vec();
        e[plan.wslot] = Some(Value::Time(t1));
        if plan.anchor_first {
            if anchored == Some(false) {
                return Ok((false, Vec::new()));
            }
            let order: Vec<usize> = (0..plan.order.len()).collect();
            let (live, stuck) = self.solve_kleene(cr, &plan.order, &order[..1], vec![e], None)?;
            if let Some(s) = stuck.into_iter().next() {
                return Err(s.error);
            }
            if live.is_empty() {
                return Ok((false, live));
            }
            let (live, stuck) = self.solve_kleene_from(cr, &plan.order, &order[1..], live, Vec::new(), None)?;
            return match stuck.into_iter().next() {
                Some(s) => Err(s.error),
                None => Ok((true, live)),
            };
        }
        let anchored = match anchored {
            Some(a) => a,
            None => plan.anchor.is_empty() || !self.solve(cr, &plan.anchor, vec![e.clone()])?.is_empty(),
        };
        Ok((anchored, self.solve(cr, &plan.order, vec![e])?))
    }

    /// Bar `t1` solved on the row path, fed to the group: counted as a
    /// solved bar, then its row's arguments evaluated. `false` when it has
    /// more than one solution (the general path's window).
    fn rows_feed_solved(&mut self, cr: &CompiledRule, args: &[Expr], agg: &str, st: &mut RowsState, t1: i64, (anchored, sols): (bool, Vec<Env>)) -> Result<bool, RunError> {
        self.stats.window_bars_solved += 1;
        if sols.len() > 1 {
            return Ok(false);
        }
        if anchored {
            st.anchors.push_back(t1);
        }
        if let Some(row) = sols.first() {
            let vals = if agg == "count" { Vec::new() } else { self.agg_args(cr, args, row)? };
            st.rows.push(t1, &vals);
        }
        Ok(true)
    }

    /// The window at `t` of a group brought up to it: the rows' count and,
    /// when there are at least `min_count`, the aggregate over them (from
    /// running sums under `numerics running`, else the two-pass reduction
    /// of the columns' slices). The summary row is recorded when asked.
    /// `None` when the buffer does not hold the window.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn rows_serve(
        &mut self,
        cr: &CompiledRule,
        plan: &RowsPlan,
        st: &mut RowsState,
        env: &[Option<Value>],
        t: i64,
        n: usize,
        agg: &str,
        f: Option<Function>,
        nargs: usize,
        min_count: i64,
    ) -> Result<Option<Served>, RunError> {
        let Some((lo, hi)) = st.window(t, n) else { return Ok(None) };
        let count = hi - lo;
        let running = if self.cfg.running_sums && moment_aggregate(agg) && count >= 2 {
            let bars = self.domain_bars(cr, env)?;
            let mut noncanonical = 0;
            let v = running_value(st, agg, t, n, &bars, &mut noncanonical);
            self.stats.window_noncanonical += noncanonical;
            v
        } else {
            None
        };
        if self.recording {
            let sums = if running.is_some() { st.sums.as_ref() } else { None };
            self.summary_rows.push(super::SummaryRow {
                rule: cr.idx,
                literal: plan.literal,
                outer: plan.outer.iter().map(|&s| env[s].clone()).collect(),
                bar: t,
                agg: agg.to_string(),
                rows: count,
                path: if running.is_some() { "running" } else { "two_pass" },
                shift: sums.map(|r| r.sums.shift),
                sums: sums.map(|r| r.sums.s.map(|x| x.value())),
                value: running,
            });
        }
        let value = if (count as i64) < min_count {
            Ok(None)
        } else if agg == "count" {
            Ok(Some(Value::Count(count as i64)))
        } else if let Some(v) = running {
            Ok(Some(Value::Num(v)))
        } else {
            let cols: smallvec::SmallVec<[&[f64]; 2]> = (0..nargs).map(|j| st.col(j, lo, hi)).collect();
            rows_reduce(agg, f, &cols)
        };
        Ok(Some((count, value)))
    }

    /// Whether a rows window's time variable is the lowest slot among the
    /// variables its conjunction binds (then rows in time order are rows in
    /// environment order). Judged once per rule and literal.
    fn time_ordered(&mut self, cr: &Arc<CompiledRule>, lit: &Literal, conj: &[Literal], env: &[Option<Value>]) -> bool {
        let literal = self.literal_index(cr, lit);
        if let Some(&known) = self.time_ordered_memo.get(&(cr.idx, literal)) {
            return known;
        }
        let Some(Literal::Window { var: Term::Var(wvar, _), .. }) = conj.iter().find(|l| matches!(l, Literal::Window { .. })) else {
            return false;
        };
        let wslot = cr.slots[wvar];
        let mut vars = Vec::new();
        for l in conj {
            literal_vars(l, &mut vars);
        }
        let ordered = vars.iter().all(|v| {
            let s = cr.slots[v];
            s == wslot || env[s].is_some() || s > wslot
        });
        self.time_ordered_memo.insert((cr.idx, literal), ordered);
        ordered
    }

    #[allow(clippy::too_many_arguments)]
    fn rows_window(&mut self, cr: &Arc<CompiledRule>, lit: &Literal, conj: &[Literal], agg: &str, args: &[Expr], env: &[Option<Value>], want_env: bool) -> Result<(WindowRows, bool), RunError> {
        let windows: Vec<usize> = conj.iter().enumerate().filter(|(_, l)| matches!(l, Literal::Window { .. })).map(|(i, _)| i).collect();
        let Some(Literal::Window {
            var: Term::Var(wvar, _),
            base: base @ Term::Var(bvar, _),
            dur,
            kind,
            ..
        }) = windows.first().map(|&i| &conj[i])
        else {
            return Err(RunError::Internal("a `rows` window needs a variable and a bound base time".into()));
        };
        let prior = *kind == WindowKind::PriorRows;
        if windows.len() != 1 {
            return Err(RunError::Internal("a `rows` window cannot share its aggregation with another window".into()));
        }
        let wslot = cr.slots[wvar];
        let t = self.time_of(cr, base, env)?;
        let n = self.eval_expr(cr, dur, env)?.as_f64().unwrap_or(0.0).max(0.0) as usize;
        let mut refs: Vec<String> = Vec::new();
        for (i, l) in conj.iter().enumerate() {
            if i != windows[0] {
                literal_vars(l, &mut refs);
            }
        }
        for a in args {
            let mut vs = Vec::new();
            a.vars(&mut vs);
            refs.extend(vs.into_iter().map(|(v, _)| v));
        }
        // Rows that read the base time change with every call: no sharing.
        let shareable = self.cfg.window_cache && !refs.iter().any(|v| v == bvar);
        let mut vs = Vec::new();
        dur.vars(&mut vs);
        refs.extend(vs.into_iter().map(|(v, _)| v));
        let mut outer: Vec<usize> = refs.iter().map(|v| cr.slots[v]).filter(|&s| s != wslot).collect();
        outer.sort_unstable();
        outer.dedup();
        let literal = self.literal_index(cr, lit);
        let key = WindowKey {
            rule: cr.idx,
            literal,
            outer: outer.into_iter().map(|s| env[s].clone()).collect(),
        };
        let order: Vec<Literal> = conj_order(conj).into_iter().filter(|l| !matches!(l, Literal::Window { .. })).collect();
        // The anchor: the first positive atom, which says where the group has a row.
        let anchor: Vec<Literal> = conj.iter().find(|l| matches!(l, Literal::Atom(_))).cloned().into_iter().collect();
        let mut cache = if shareable { self.windows.remove(&key).unwrap_or_default() } else { WindowCache::new() };
        let mut anchored = if shareable { self.rows_anchor.remove(&key).unwrap_or_default() } else { BTreeSet::new() };
        let floor = if shareable { self.rows_floor.get(&key).copied() } else { None };
        let mut found: Vec<i64> = Vec::new();
        let mut reached_start = true;
        let mut upper = if prior { t - 1 } else { t };
        'walk: while found.len() < n {
            // The domain in chunks, latest first, so that solving can borrow the kernel.
            let chunk: Vec<i64> = self.domain(cr, env)?.range(..=upper).rev().take(64).copied().collect();
            if chunk.is_empty() {
                break;
            }
            for &t1 in &chunk {
                if floor.map(|f| t1 < f).unwrap_or(false) {
                    break 'walk;
                }
                if !cache.contains_key(&t1) {
                    let mut e = env.to_vec();
                    e[wslot] = Some(Value::Time(t1));
                    let is_row = anchor.is_empty() || !self.solve(cr, &anchor, vec![e.clone()]).map(|s| s.is_empty()).unwrap_or(true);
                    if is_row {
                        anchored.insert(t1);
                    }
                    let solved = self.solve(cr, &order, vec![e]).and_then(|sols| {
                        let mut rows = Vec::with_capacity(sols.len());
                        for row in sols {
                            let vals = if agg == "count" { Vec::new() } else { self.agg_args(cr, args, &row)? };
                            rows.push((row, vals));
                        }
                        Ok(rows)
                    });
                    let rows = match solved {
                        Ok(r) => r,
                        Err(err) => {
                            if shareable {
                                self.windows.insert(key.clone(), cache);
                                self.rows_anchor.insert(key, anchored);
                            }
                            return Err(err);
                        }
                    };
                    self.stats.window_bars_solved += 1;
                    cache.insert(t1, rows);
                }
                if anchored.contains(&t1) || !cache[&t1].is_empty() {
                    found.push(t1);
                    if found.len() == n {
                        reached_start = false;
                        break 'walk;
                    }
                }
            }
            match chunk.last() {
                Some(&last) if last > i64::MIN => upper = last - 1,
                _ => break,
            }
        }
        let mut rows = Vec::new();
        if self.recording {
            self.summary_rows.push(super::SummaryRow {
                rule: key.rule,
                literal: key.literal,
                outer: key.outer.clone(),
                bar: t,
                agg: agg.to_string(),
                rows: found.iter().map(|t1| cache[t1].len()).sum(),
                path: "rows walk",
                shift: None,
                sums: None,
                value: None,
            });
        }
        // Without environments only when every bar has at most one row (the
        // order within a bar is the environments').
        let bare = !want_env && found.iter().all(|t1| cache[t1].len() <= 1);
        for t1 in found.iter().rev() {
            for (row, vals) in &cache[t1] {
                if bare {
                    rows.push((Vec::new(), vals.clone()));
                    continue;
                }
                let mut e = env.to_vec();
                for (i, v) in row.iter().enumerate() {
                    if env[i].is_none() {
                        e[i] = v.clone();
                    }
                }
                rows.push((e, vals.clone()));
            }
        }
        if shareable {
            // Every bar before the earliest one found is either known empty
            // (the walk reached the start) or not needed by a later bar.
            if reached_start && floor.is_none() {
                self.rows_floor.insert(key.clone(), found.last().copied().unwrap_or(t + 1));
            }
            match found.last() {
                Some(&oldest) => {
                    cache.forget_before(oldest);
                    anchored = anchored.split_off(&oldest);
                }
                None => {
                    cache.clear();
                    anchored.clear();
                }
            }
            self.windows.insert(key.clone(), cache);
            self.rows_anchor.insert(key, anchored);
        }
        Ok((rows, bare))
    }

    fn arith(&self, cr: &CompiledRule, env: &[Option<Value>], expr: &Expr, message: &str) -> RunError {
        self.arith_text(cr, env, expr.to_string(), message)
    }

    /// The halt of an aggregate whose reduction is partial, naming it.
    fn agg_error(&self, cr: &CompiledRule, env: &[Option<Value>], agg: &str, args: &[Expr], message: &str) -> RunError {
        let a: Vec<String> = args.iter().map(|e| e.to_string()).collect();
        self.arith_text(cr, env, format!("{}({}) over (...)", agg, a.join(", ")), message)
    }

    /// The partial-arithmetic halt of section 7, with the offending
    /// expression given as text (an aggregate names the whole aggregate).
    fn arith_text(&self, cr: &CompiledRule, env: &[Option<Value>], expr: String, message: &str) -> RunError {
        let mut names: Vec<(&String, &usize)> = cr.slots.iter().collect();
        names.sort_by_key(|(_, s)| **s);
        let bindings: Vec<String> = names.iter().filter_map(|(n, s)| env[**s].as_ref().map(|v| format!("{}={}", n, self.show(v)))).collect();
        RunError::Arithmetic {
            rule: cr.label.clone(),
            bindings: bindings.join(", "),
            expr,
            message: message.to_string(),
        }
    }

    pub fn show(&self, v: &Value) -> String {
        match v {
            Value::Equity(s) => self.symbols.name(*s).to_string(),
            Value::Time(t) => time::format_timestamp(*t),
            Value::Num(x) => format!("{}", x),
            Value::Count(c) => format!("{}", c),
            Value::Dur(d) => format!("{}", d),
            Value::Decision(d) if d.order.is_market() => format!("{}({}, {})", d.ctor.name(), self.symbols.name(d.equity), d.amount),
            Value::Decision(d) => format!("{}({}, {}, {})", d.ctor.name(), self.symbols.name(d.equity), d.amount, d.order),
            Value::Label(s) => self.labels.name(*s).to_string(),
        }
    }

    pub(crate) fn eval_expr(&mut self, cr: &CompiledRule, e: &Expr, env: &[Option<Value>]) -> Result<Value, RunError> {
        match e {
            Expr::Var(v, _) => env[cr.slots[v]].clone().ok_or_else(|| RunError::Internal(format!("variable `{}` unbound in rule {}", v, cr.label))),
            Expr::Param(p, _) => self.param_value(cr, p),
            Expr::Lit(l, _) => Ok(super::lit_value(l, &mut self.symbols, &mut self.labels, &self.literal_equities)),
            // The arithmetic is `cexpr`'s, which the block evaluator's
            // compiled expressions apply to whole columns.
            Expr::Neg(x, _) => {
                let v = self.eval_expr(cr, x, env)?;
                cexpr::neg_value(&v).map_err(|m| self.arith(cr, env, e, m))
            }
            Expr::Bin(op, a, b, _) => {
                let x = self.eval_expr(cr, a, env)?;
                let y = self.eval_expr(cr, b, env)?;
                cexpr::bin_value(*op, &x, &y).map_err(|m| self.arith(cr, env, e, m))
            }
            Expr::Call(f, args, _) => {
                let mut vals = Vec::with_capacity(args.len());
                for a in args {
                    vals.push(self.eval_expr(cr, a, env)?);
                }
                let num = |v: &Value| v.as_f64();
                if let Some(f1) = cexpr::Fn1::parse(f) {
                    return cexpr::call1_value(f1, &vals[0]).map_err(|m| self.arith(cr, env, e, &m));
                }
                match f.as_str() {
                    "clamp" => {
                        let x = num(&vals[0]).ok_or_else(|| self.arith(cr, env, e, "clamp of a non-numeric value"))?;
                        let lo = num(&vals[1]).ok_or_else(|| self.arith(cr, env, e, "clamp of a non-numeric value"))?;
                        let hi = num(&vals[2]).ok_or_else(|| self.arith(cr, env, e, "clamp of a non-numeric value"))?;
                        if lo > hi {
                            return Err(self.arith(cr, env, e, &format!("clamp with lo {} above hi {}", lo, hi)));
                        }
                        Ok(Value::Num(x.max(lo).min(hi)))
                    }
                    "decile" => {
                        // ceil(rank / n * 10): 1 for the smallest tenth, 10 the largest.
                        let r = num(&vals[0]).ok_or_else(|| self.arith(cr, env, e, "decile of a non-numeric rank"))?;
                        let n = num(&vals[1]).ok_or_else(|| self.arith(cr, env, e, "decile of a non-numeric count"))?;
                        if n <= 0.0 {
                            return Err(self.arith(cr, env, e, "decile over an empty group"));
                        }
                        Ok(Value::Count(((r / n) * 10.0).ceil().clamp(1.0, 10.0) as i64))
                    }
                    "least" | "greatest" => Ok(cexpr::least_greatest(f == "least", &vals)),
                    n if PRICING_FUNCTIONS.contains(&n) => self.pricing(cr, env, e, n, &vals),
                    _ => Err(RunError::Internal(format!("unknown function `{}`", f))),
                }
            }
        }
    }

    /// The option pricing functions of K-2.5, total on their domain: a
    /// value outside it (a non-positive price, a negative volatility, a
    /// price outside the no-arbitrage bounds for `implied_vol`) leaves the
    /// instance stuck (⊥, V-5.1).
    fn pricing(&self, cr: &CompiledRule, env: &[Option<Value>], e: &Expr, f: &str, vals: &[Value]) -> Result<Value, RunError> {
        use crate::functions::{american, b76, bsm, iv, Right, Style};
        let num = |i: usize| {
            vals.get(i)
                .and_then(|v| v.as_f64())
                .ok_or_else(|| self.arith(cr, env, e, &format!("{}'s argument {} is not a number", f, i + 1)))
        };
        // τ: a Duration in years (ACT/365, a month as a twelfth of 365
        // days), or a Scalar already in years.
        let years = |i: usize| match vals.get(i) {
            Some(Value::Dur(d)) => Ok((d.months as f64 * 365.0 / 12.0 + d.days as f64) / 365.0 + crate::functions::years(d.nanos)),
            _ => num(i),
        };
        let word = |i: usize| -> Option<String> {
            match vals.get(i)? {
                Value::Label(l) => Some(self.labels.name(*l).to_string()),
                // A string literal the checker left as written.
                Value::Equity(s) => Some(self.symbols.name(*s).to_string()),
                _ => None,
            }
        };
        let right = |i: usize| {
            word(i)
                .and_then(|w| Right::parse(&w))
                .ok_or_else(|| self.arith(cr, env, e, &format!("{}'s right is `call` or `put`", f)))
        };
        let v = if f == "implied_vol" {
            let style = word(7)
                .and_then(|w| Style::parse(&w))
                .ok_or_else(|| self.arith(cr, env, e, "implied_vol's style is `american` or `european`"))?;
            let v = iv::implied_vol(num(0)?, num(1)?, num(2)?, years(3)?, num(4)?, num(5)?, right(6)?, style);
            v.ok_or_else(|| self.arith(cr, env, e, "implied_vol: the price is outside the no-arbitrage bounds"))?
        } else if let Some(g) = f.strip_prefix("b76_") {
            let (fwd, k, tau, r, sigma, w) = (num(0)?, num(1)?, years(2)?, num(3)?, num(4)?, right(5)?);
            match g {
                "price" => b76::price(fwd, k, tau, r, sigma, w),
                "delta" => b76::delta(fwd, k, tau, r, sigma, w),
                "gamma" => b76::gamma(fwd, k, tau, r, sigma, w),
                "vega" => b76::vega(fwd, k, tau, r, sigma, w),
                "theta" => b76::theta(fwd, k, tau, r, sigma, w),
                _ => b76::rho(fwd, k, tau, r, sigma, w),
            }
        } else {
            let (s, k, tau, r, q, sigma, w) = (num(0)?, num(1)?, years(2)?, num(3)?, num(4)?, num(5)?, right(6)?);
            match f {
                "am_price" => american::price(s, k, tau, r, q, sigma, w),
                "bs_price" => bsm::price(s, k, tau, r, q, sigma, w),
                "bs_delta" => bsm::delta(s, k, tau, r, q, sigma, w),
                "bs_gamma" => bsm::gamma(s, k, tau, r, q, sigma, w),
                "bs_vega" => bsm::vega(s, k, tau, r, q, sigma, w),
                "bs_theta" => bsm::theta(s, k, tau, r, q, sigma, w),
                _ => bsm::rho(s, k, tau, r, q, sigma, w),
            }
        };
        if !v.is_finite() {
            return Err(self.arith(cr, env, e, &format!("{} is undefined at these arguments", f)));
        }
        Ok(Value::Num(v))
    }
}

/// A conjunction with each window literal moved ahead of the first atom that
/// binds its variable in a temporal-key position. The conjunction is
/// commutative, so this changes nothing but the cost: the window enumerates
/// the bars and the atom becomes a lookup instead of a scan.
/// Every variable a literal mentions.
/// The variables a rule mentions outside one of its body literals: in the
/// head or in another literal. Another aggregation's conjunction is its own
/// scope, so of it only the result and the group count.
pub(crate) fn outer_vars(rule: &Rule, lit: &Literal) -> Vec<String> {
    fn term(t: &Term, out: &mut Vec<String>) {
        match t {
            Term::Var(v, _) => out.push(v.clone()),
            Term::Ctor(_, subs, _) => subs.iter().for_each(|s| term(s, out)),
            _ => {}
        }
    }
    let mut out = Vec::new();
    rule.head.terms.iter().for_each(|t| term(t, &mut out));
    for l in &rule.body {
        if std::ptr::eq(l, lit) || l == lit {
            continue;
        }
        match l {
            Literal::Agg { var, within, .. } => {
                out.push(var.clone());
                if let Some(w) = within {
                    out.extend(w.iter().cloned());
                }
            }
            _ => literal_vars(l, &mut out),
        }
    }
    out
}

pub(crate) fn literal_vars(l: &Literal, out: &mut Vec<String>) {
    fn term(t: &Term, out: &mut Vec<String>) {
        match t {
            Term::Var(v, _) => out.push(v.clone()),
            Term::Ctor(_, subs, _) => subs.iter().for_each(|s| term(s, out)),
            _ => {}
        }
    }
    fn expr(e: &Expr, out: &mut Vec<String>) {
        let mut vs = Vec::new();
        e.vars(&mut vs);
        out.extend(vs.into_iter().map(|(v, _)| v));
    }
    match l {
        Literal::Atom(a) | Literal::Neg(a) => a.terms.iter().for_each(|t| term(t, out)),
        Literal::Builtin(b, _) => match b {
            Builtin::Prev { t, t1 } => {
                term(t, out);
                term(t1, out);
            }
            Builtin::Lag { t, n, t1 } => {
                term(t, out);
                expr(n, out);
                term(t1, out);
            }
            Builtin::MonthStart { t } | Builtin::DayStart { t } => term(t, out),
            Builtin::SessionOf { a, t, s } => {
                term(a, out);
                term(t, out);
                term(s, out);
            }
        },
        Literal::Window { var, base, dur, min, lo, .. } => {
            term(var, out);
            term(base, out);
            expr(dur, out);
            expr(min, out);
            if let Some(lo) = lo {
                expr(lo, out);
            }
        }
        Literal::Cmp { lhs, rhs, .. } => {
            expr(lhs, out);
            expr(rhs, out);
        }
        Literal::Assign { var, expr: e, .. } => {
            out.push(var.clone());
            expr(e, out);
        }
        Literal::Agg { var, args, conj, .. } => {
            out.push(var.clone());
            args.iter().for_each(|a| expr(a, out));
            conj.iter().for_each(|c| literal_vars(c, out));
        }
        Literal::Top { n, atom, by, rank, .. } => {
            if let Some(n) = n {
                expr(n, out);
            }
            atom.terms.iter().for_each(|t| term(t, out));
            if let Some(keys) = by {
                out.extend(keys.iter().map(|(k, _, _)| k.clone()));
            }
            if let Some((k, _, _)) = rank {
                out.push(k.clone());
            }
        }
        Literal::AsOf { atom, at, .. } => {
            atom.terms.iter().for_each(|t| term(t, out));
            term(at, out);
        }
        Literal::Case { var, branches, otherwise, .. } => {
            out.push(var.clone());
            for b in branches {
                b.guard.iter().for_each(|c| literal_vars(c, out));
                expr(&b.value, out);
            }
            if let Some(e) = otherwise {
                expr(e, out);
            }
        }
    }
}

pub(crate) fn conj_order(conj: &[Literal]) -> Vec<Literal> {
    let mut out: Vec<Literal> = Vec::with_capacity(conj.len());
    let mut placed = vec![false; conj.len()];
    for (i, lit) in conj.iter().enumerate() {
        if placed[i] {
            continue;
        }
        let key_vars: Vec<&String> = match lit {
            Literal::Atom(a) | Literal::Neg(a) | Literal::AsOf { atom: a, .. } => a.terms.iter().filter_map(|t| if let Term::Var(v, _) = t { Some(v) } else { None }).collect(),
            _ => vec![],
        };
        for (j, later) in conj.iter().enumerate().skip(i + 1) {
            if placed[j] {
                continue;
            }
            if let Literal::Window { var: Term::Var(v, _), .. } = later {
                if key_vars.contains(&v) {
                    out.push(later.clone());
                    placed[j] = true;
                }
            }
        }
        out.push(lit.clone());
        placed[i] = true;
    }
    out
}

/// An expression that evaluates on every binding of well-typed values: no
/// division, no partial function (a test of it fails or holds, never sticks).
fn total_expr(e: &Expr) -> bool {
    match e {
        Expr::Var(..) | Expr::Param(..) | Expr::Lit(..) => true,
        Expr::Neg(x, _) => total_expr(x),
        Expr::Bin(op, a, b, _) => !matches!(op, BinOp::Div) && total_expr(a) && total_expr(b),
        Expr::Call(..) => false,
    }
}

pub(crate) fn values_equal(a: &Value, b: &Value) -> bool {
    match (a.as_f64(), b.as_f64()) {
        (Some(x), Some(y)) => x == y,
        _ => a == b,
    }
}

pub(crate) fn compare(op: CmpOp, a: &Value, b: &Value) -> bool {
    let ord = match (a, b) {
        (Value::Dur(x), Value::Dur(y)) => x.approx_days().partial_cmp(&y.approx_days()).unwrap_or(std::cmp::Ordering::Equal),
        _ => match (a.as_f64(), b.as_f64()) {
            // Two numbers as the block evaluator's tests compare them.
            (Some(x), Some(y)) => return cexpr::compare_num(op, x, y),
            _ => a.cmp(b),
        },
    };
    cexpr::holds(op, ord)
}

/// The closed aggregate set (section 1), reduced by abt-agg's two-pass
/// formulas in the group's order (P-1.2). `Ok(None)` is "no tuple": an empty
/// group for anything but `count`. Errors are partial arithmetic.
pub fn aggregate(name: &str, cols: &[Vec<f64>]) -> Result<Option<Value>, String> {
    let xs = &cols[0];
    if name == "count" {
        return Ok(Some(Value::Count(xs.len() as i64)));
    }
    if xs.is_empty() {
        return Ok(None);
    }
    let level = if name == "quantile" {
        Some(cols.get(1).and_then(|c| c.first()).copied().ok_or("quantile needs a level")?)
    } else {
        None
    };
    reduce_columns(Function::parse(name, level).map_err(|e| e.to_string())?, cols)
}

/// `aggregate` past its function's name, for a function parsed once (an
/// aggregation literal's plan): the two-pass reduction of the columns `f`
/// reads, in their order; no tuple for an empty group.
fn reduce_columns(f: Function, cols: &[Vec<f64>]) -> Result<Option<Value>, String> {
    let slices: smallvec::SmallVec<[&[f64]; 2]> = cols.iter().map(|c| c.as_slice()).collect();
    reduce_slices(f, &slices)
}

/// `reduce_columns` over slices of the columns (a rows window's buffer,
/// read in place).
pub(crate) fn reduce_slices(f: Function, cols: &[&[f64]]) -> Result<Option<Value>, String> {
    if cols.first().is_some_and(|x| x.is_empty()) {
        return Ok(None);
    }
    let k = f.arity().min(cols.len());
    abt_agg::reduce(f, abt_agg::Mode::TwoPass, &cols[..k]).map(|v| v.map(Value::Num)).map_err(|e| e.to_string())
}

/// A rows window's aggregate over its argument columns: by `f` when it was
/// parsed once, else by name (`quantile`, whose level a row gives).
pub(crate) fn rows_reduce(agg: &str, f: Option<Function>, cols: &[&[f64]]) -> Result<Option<Value>, String> {
    match f {
        Some(f) => reduce_slices(f, cols),
        None => aggregate(agg, &cols.iter().map(|c| c.to_vec()).collect::<Vec<Vec<f64>>>()),
    }
}

#[cfg(test)]
mod row_path_tests {
    use super::*;

    /// The labels the lazy as-of walk visits, in its order.
    fn lazy_walk(dom: &BTreeSet<i64>, floor: i64, t: i64) -> Vec<i64> {
        let mut out = Vec::new();
        let mut next = asof_latest(dom, floor, t);
        while let Some(k) = next {
            out.push(k);
            next = asof_before(dom, floor, k);
        }
        out
    }

    #[test]
    fn the_lazy_asof_walk_visits_the_bars_the_collected_walk_did() {
        // Domains with holes, one of them reaching the first and the last instant.
        let doms: Vec<BTreeSet<i64>> = vec![
            BTreeSet::new(),
            [5].into_iter().collect(),
            [1, 2, 3, 7, 8, 20, 21, 40].into_iter().collect(),
            [i64::MIN, -3, 0, 4, 9, i64::MAX].into_iter().collect(),
        ];
        let ts = [i64::MIN, -3, 0, 1, 4, 6, 8, 19, 20, 40, 41, i64::MAX];
        let mut walks = 0;
        for dom in &doms {
            for &t in &ts {
                // No floor (the whole history); a floor on a label, between
                // two and at `t`; and a bucket's floor, one past a label
                // before `t` (`within_bucket`).
                let mut floors = vec![i64::MIN];
                floors.extend(ts.iter().copied().filter(|f| *f <= t));
                floors.extend(dom.range(..t).map(|p| p + 1));
                for floor in floors {
                    let collected: Vec<i64> = dom.range(floor..=t).rev().copied().collect();
                    assert_eq!(lazy_walk(dom, floor, t), collected, "floor {} t {} in {:?}", floor, t, dom);
                    walks += usize::from(collected.len() > 1);
                }
            }
        }
        assert!(walks > 100, "{} walks went past their first bar", walks);
    }

    /// The merge join as it ran when it copied the relation first: every
    /// tuple cloned into a vector, the vector walked beside the block.
    fn copied_join(live: &Block, next: &mut Block, eslot: usize, entity_pos: usize, slots: &[Option<usize>], shared: &[Option<Value>], relation: &[Tuple]) -> bool {
        let tuples: Vec<Tuple> = relation.to_vec();
        if !tuples.windows(2).all(|w| w[0][entity_pos] <= w[1][entity_pos]) {
            return false;
        }
        let mut j = 0usize;
        for i in 0..live.len() {
            let env = live.row(i);
            next.set_origin(live.origin_of(i));
            let e = env[eslot].as_ref().expect("checked");
            while j < tuples.len() && tuples[j][entity_pos] < *e {
                j += 1;
            }
            let mut k = j;
            while k < tuples.len() && tuples[k][entity_pos] == *e {
                let tu = &tuples[k];
                k += 1;
                let ok = slots.iter().enumerate().all(|(p, slot)| match slot {
                    Some(s) => env[*s].as_ref().map(|b| b == &tu[p]).unwrap_or(true),
                    None => shared[p].as_ref().map(|c| c == &tu[p]).unwrap_or(true),
                });
                if ok {
                    bind_row(env, slots, tu, next);
                }
            }
        }
        true
    }

    fn rows_of(b: &Block) -> Vec<(u32, Env)> {
        (0..b.len()).map(|i| (b.origin_of(i), b.row(i).to_vec())).collect()
    }

    #[test]
    fn a_merge_join_from_the_block_binds_what_the_copied_join_did() {
        let (e, t, x) = (|s: u32| Some(Value::Equity(s)), Some(Value::Time(7)), |v: f64| Some(Value::Num(v)));
        // Slots A, T, X, Y. An entity on two rows, one of them with X bound;
        // an entity the relation lacks; a row whose X no tuple has.
        let mut live = Block::with_capacity(4, 6);
        for (i, row) in [
            [e(1), t.clone(), None, None],
            [e(1), t.clone(), x(2.0), None],
            [e(3), t.clone(), None, None],
            [e(4), t.clone(), None, None],
            [e(4), t.clone(), x(9.0), None],
            [e(6), t.clone(), None, None],
        ]
        .iter()
        .enumerate()
        {
            live.push_row(row, 10 + i as u32);
        }
        // The relation (A, T, X, Y), ascending by A: runs of one to three
        // tuples, entities the block lacks before, between and after its own.
        let tu = |s: u32, a: f64, b: f64| -> Tuple { smallvec::smallvec![Value::Equity(s), Value::Time(7), Value::Num(a), Value::Num(b)] };
        let relation: Vec<Tuple> = vec![
            tu(0, 1.0, 10.0),
            tu(1, 1.0, 11.0),
            tu(1, 2.0, 12.0),
            tu(1, 2.0, 2.0),
            tu(2, 5.0, 14.0),
            tu(4, 3.0, 3.0),
            tu(4, 3.0, 16.0),
            tu(5, 1.0, 17.0),
            tu(6, 4.0, 18.0),
            tu(6, 1.0, 1.0),
            tu(8, 1.0, 19.0),
        ];
        let mut stored = super::super::rows::Rows::default();
        for tuple in &relation {
            stored.push(tuple.clone());
        }
        let derived: super::super::Derived = Arc::new(relation.iter().map(|tuple| (tuple.clone(), 0)).collect());
        // r(A, T, X, Y); r(A, T, 1, Y); r(A, T, X, X); r(A, T, _, Y).
        type Atom = (Vec<Option<usize>>, Vec<Option<Value>>);
        let atoms: [Atom; 4] = [
            (vec![Some(0), Some(1), Some(2), Some(3)], vec![None, t.clone(), None, None]),
            (vec![Some(0), Some(1), None, Some(3)], vec![None, t.clone(), x(1.0), None]),
            (vec![Some(0), Some(1), Some(2), Some(2)], vec![None, t.clone(), None, None]),
            (vec![Some(0), Some(1), None, Some(3)], vec![None, t.clone(), None, None]),
        ];
        let mut bound = 0;
        for (slots, shared) in &atoms {
            let mut want = Block::with_capacity(4, 8);
            assert!(copied_join(&live, &mut want, 0, 0, slots, shared, &relation));
            let rows = stored.all();
            let mut from_store = Block::with_capacity(4, 8);
            assert!(merge_join_rows(&live, &mut from_store, 0, 0, slots, shared, rows.len(), |j| rows.row(j)));
            let mut from_memo = Block::with_capacity(4, 8);
            assert!(merge_join_rows(&live, &mut from_memo, 0, 0, slots, shared, derived.len(), |j| derived[j].0.as_slice()));
            assert_eq!(rows_of(&from_store), rows_of(&want), "from the store's block, slots {:?}", slots);
            assert_eq!(rows_of(&from_memo), rows_of(&want), "from the memoised result, slots {:?}", slots);
            bound += want.len();
        }
        assert_eq!(bound, 9 + 3 + 4 + 12, "the rows the four atoms bind");
        // Tuples out of entity order are the row-at-a-time path's: nothing is pushed.
        let mut unsorted = relation.clone();
        unsorted.swap(2, 7);
        let (slots, shared) = &atoms[0];
        let mut next = Block::with_capacity(4, 8);
        assert!(!copied_join(&live, &mut next, 0, 0, slots, shared, &unsorted));
        assert!(!merge_join_rows(&live, &mut next, 0, 0, slots, shared, unsorted.len(), |j| unsorted[j].as_slice()));
        assert!(next.is_empty());
    }
}

#[cfg(test)]
mod running_tests {
    use super::*;

    /// A deterministic pseudo-random stream (xorshift) in [0, 1).
    fn stream(seed: u64) -> impl FnMut() -> f64 {
        let mut x = seed;
        move || {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            (x >> 11) as f64 / (1u64 << 53) as f64
        }
    }

    /// A group whose every bar `0..rows.len()` is anchored and holds one row.
    fn state(rows: &[(f64, f64)]) -> RowsState {
        let mut st = RowsState::default();
        for (i, (a, b)) in rows.iter().enumerate() {
            st.feed(i as i64, true, Some(&[*a, *b]));
        }
        st.from_start = true;
        st
    }

    /// The running value of the window of `n` rows ending at row `hi - 1`
    /// (the bar `hi - 1` of a domain of one bar per row).
    fn at(st: &mut RowsState, agg: &str, lo: usize, hi: usize, n: usize) -> Option<f64> {
        assert_eq!(hi - lo, n.min(hi));
        let bars: Vec<i64> = (0..st.rows.len() as i64).collect();
        let mut nc = 0;
        let v = running_value(st, agg, hi as i64 - 1, n, &bars, &mut nc);
        assert_eq!(nc, 0, "the buffer holds every checkpoint");
        v
    }

    #[test]
    fn running_sums_equal_the_exact_formulas_to_rounding_on_a_sliding_window() {
        let mut r = stream(7);
        // Prices far from zero (the cancellation case) and a trend regressor.
        let rows: Vec<(f64, f64)> = (0..3000).map(|i| (1.0e4 + 50.0 * r() + 0.01 * i as f64, i as f64 + r())).collect();
        let mut st = state(&rows);
        for n in [4usize, 18, 40, 250] {
            st.sums = None;
            for hi in n..rows.len() {
                let lo = hi - n;
                for agg in ["mean", "sum", "std", "cov", "corr", "ols_slope"] {
                    let got = at(&mut st, agg, lo, hi, n).unwrap_or_else(|| panic!("{} n={} hi={}: no running value", agg, n, hi));
                    let cols = vec![rows[lo..hi].iter().map(|r| r.0).collect::<Vec<f64>>(), rows[lo..hi].iter().map(|r| r.1).collect()];
                    let Some(Value::Num(want)) = aggregate(agg, &cols).unwrap() else { panic!() };
                    let scale = want.abs().max(1e-300);
                    assert!((got - want).abs() <= 1e-9 * scale, "{} n={} hi={}: running {} exact {}", agg, n, hi, got, want);
                }
            }
        }
    }

    #[test]
    fn a_flat_or_non_finite_window_is_left_to_the_exact_path() {
        let flat = vec![(5.0, 1.0); 30];
        let mut st = state(&flat);
        assert_eq!(at(&mut st, "mean", 0, 20, 20), Some(5.0));
        for agg in ["std", "corr", "ols_slope"] {
            assert_eq!(at(&mut st, agg, 0, 20, 20), None, "{}", agg);
        }
        let mut rows: Vec<(f64, f64)> = (0..30).map(|i| (i as f64, 2.0 * i as f64)).collect();
        rows[10].0 = f64::NAN;
        let mut st = state(&rows);
        assert_eq!(at(&mut st, "mean", 5, 15, 10), None);
        // Once the NaN has left the window, the sums answer again.
        assert_eq!(at(&mut st, "mean", 11, 21, 10), Some(15.5));
    }

    /// The compiler design's I2 for the running path: the value at a bar is
    /// a function of the group's rows and the bar, whatever bars were asked
    /// for before it. A group served at every bar, one served every third
    /// bar, one first asked late, and one asked backwards agree to the bit.
    #[test]
    fn running_sums_do_not_depend_on_which_bars_were_asked_for() {
        let mut r = stream(11);
        let rows: Vec<(f64, f64)> = (0..600).map(|i| (1.0e3 + 20.0 * r() + 0.3 * i as f64, i as f64 + r())).collect();
        let bars: Vec<i64> = (0..rows.len() as i64).collect();
        for n in [5usize, 18, 40] {
            let mut every = state(&rows);
            let mut third = state(&rows);
            let mut late = state(&rows);
            let mut back = state(&rows);
            let mut nc = 0;
            let mut reference: Vec<(i64, Option<f64>)> = Vec::new();
            for t in (n as i64 - 1)..rows.len() as i64 {
                reference.push((t, running_value(&mut every, "std", t, n, &bars, &mut nc)));
            }
            for &(t, want) in reference.iter().filter(|(t, _)| t % 3 == 0) {
                assert_eq!(running_value(&mut third, "std", t, n, &bars, &mut nc), want, "n={} t={} every third bar", n, t);
            }
            for &(t, want) in reference.iter().filter(|(t, _)| *t >= 300) {
                assert_eq!(running_value(&mut late, "std", t, n, &bars, &mut nc), want, "n={} t={} first asked late", n, t);
            }
            for &(t, want) in reference.iter().rev().step_by(7) {
                assert_eq!(running_value(&mut back, "std", t, n, &bars, &mut nc), want, "n={} t={} asked backwards", n, t);
            }
            assert_eq!(nc, 0);
        }
    }
}

#[cfg(test)]
mod rows_state_tests {
    use super::*;
    use std::collections::VecDeque;

    /// The state a rows window group kept before its rows were flat (a
    /// deque of rows each with its own argument vector), with its serving
    /// rule and its trim: the oracle.
    #[derive(Default)]
    struct Deque {
        upto: i64,
        from_start: bool,
        anchors: VecDeque<i64>,
        rows: VecDeque<(i64, Vec<f64>)>,
    }

    impl Deque {
        fn serve(&self, t: i64, n: usize) -> Option<(usize, Vec<Vec<f64>>)> {
            let hi = self.anchors.partition_point(|a| *a <= t);
            if !(hi >= n || self.from_start) {
                return None;
            }
            let lo_t = if hi >= n && n > 0 {
                self.anchors[hi - n]
            } else if hi > 0 {
                self.anchors[0]
            } else {
                i64::MAX
            };
            let lo = self.rows.partition_point(|(t1, _)| *t1 < lo_t);
            let hi_r = self.rows.partition_point(|(t1, _)| *t1 <= t);
            let count = hi_r.saturating_sub(lo);
            let width = self.rows.front().map(|r| r.1.len()).unwrap_or(0);
            let mut cols: Vec<Vec<f64>> = vec![Vec::new(); width];
            for row in self.rows.iter().take(hi_r).skip(lo) {
                for (j, v) in row.1.iter().enumerate() {
                    cols[j].push(*v);
                }
            }
            Some((count, cols))
        }

        fn trim(&mut self, n: usize) {
            while self.anchors.len() > n + abt_agg::checkpoint_period(n) + ROWS_SLACK {
                self.anchors.pop_front();
                self.from_start = false;
            }
            let oldest = self.anchors.front().copied().unwrap_or(i64::MAX);
            while self.rows.front().map(|(t1, _)| *t1 < oldest).unwrap_or(false) {
                self.rows.pop_front();
            }
        }
    }

    fn next(x: &mut u64) -> u64 {
        *x ^= *x << 13;
        *x ^= *x >> 7;
        *x ^= *x << 17;
        *x
    }

    fn bits(v: &Result<Option<Value>, String>) -> String {
        match v {
            Ok(Some(Value::Num(x))) => format!("{:016x}", x.to_bits()),
            other => format!("{:?}", other),
        }
    }

    /// Random feeds through the flat state and the deque: groups asked at
    /// every bar, every third bar and after long gaps, bars not anchored,
    /// anchored bars without a row, groups that reach their first bar and
    /// groups that do not, and requests behind the group's last bar (served
    /// from a group built fresh). Each serve gives the same window and the
    /// same reduced bits, through one argument and two, and a count.
    #[test]
    fn the_flat_state_serves_what_the_deque_served() {
        let mut x = 0x2545_F491_4F6C_DD1Du64;
        let mut served = 0;
        for case in 0..400 {
            let n = [1usize, 2, 4, 18, 40][case % 5];
            let width = [0usize, 1, 2][case % 3];
            let mut flat = RowsState::default();
            let mut deque = Deque { upto: i64::MIN, ..Default::default() };
            let from_start = case % 4 != 0;
            flat.from_start = from_start;
            deque.from_start = from_start;
            let mut t = 0i64;
            for _ in 0..300 {
                // The bars since the last call: some not anchored, some
                // anchored with no row; a long gap now and then.
                let gap = if next(&mut x).is_multiple_of(50) { 30 + next(&mut x) % 80 } else { 1 + next(&mut x) % 3 };
                for _ in 0..gap {
                    t += 1;
                    let r = next(&mut x) % 10;
                    let anchored = r < 8;
                    let row = r < 7 || r == 9;
                    let vals: Vec<f64> = (0..width).map(|j| ((next(&mut x) % 20_000) as f64 - 10_000.0) / 7.0 + j as f64 * 1e6).collect();
                    flat.feed(t, anchored, row.then_some(vals.as_slice()));
                    if anchored {
                        deque.anchors.push_back(t);
                    }
                    if row {
                        deque.rows.push_back((t, vals));
                    }
                }
                flat.upto = t;
                deque.upto = t;
                let ask = |st: &RowsState, at: i64| st.window(at, n).map(|(lo, hi)| (hi - lo, (0..width).map(|j| st.col(j, lo, hi).to_vec()).collect::<Vec<Vec<f64>>>()));
                let a = ask(&flat, t);
                let b = deque.serve(t, n).map(|(c, cols)| (c, if cols.is_empty() { vec![Vec::new(); width] } else { cols }));
                assert_eq!(a, b, "case {} at {}", case, t);
                if let Some((count, cols)) = a {
                    served += 1;
                    let (lo, hi) = flat.window(t, n).unwrap();
                    let slices: Vec<&[f64]> = (0..width).map(|j| flat.col(j, lo, hi)).collect();
                    assert_eq!(hi - lo, count);
                    for agg in ["mean", "sum", "std", "max", "last", "ols_slope", "corr"] {
                        let f = Function::parse(agg, None).ok();
                        if width == 0 || (width < 2 && matches!(agg, "ols_slope" | "corr")) {
                            continue;
                        }
                        let want = f.map_or_else(|| aggregate(agg, &cols), |f| reduce_columns(f, &cols));
                        assert_eq!(bits(&rows_reduce(agg, f, &slices)), bits(&want), "case {} {} at {}", case, agg, t);
                    }
                }
                // A request a few bars back, as a recursion reaching its
                // previous row makes it, is served from the buffer alike.
                let back = t - (next(&mut x) % 6) as i64;
                assert_eq!(ask(&flat, back).map(|(c, _)| c), deque.serve(back, n).map(|(c, _)| c), "case {} back at {}", case, back);
                flat.trim(n);
                deque.trim(n);
                assert_eq!(flat.anchors, deque.anchors);
                assert_eq!(flat.rows.len(), deque.rows.len());
                assert_eq!(flat.from_start, deque.from_start);
            }
        }
        assert!(served > 50_000, "{} windows served", served);
    }

    /// The dense table of a group's states: put, take and the count of
    /// states held, by symbol, and the outer values its first call names.
    #[test]
    fn the_dense_states_are_held_by_symbol() {
        let mut e = EntRows::default();
        let rest = |v: f64| vec![None, Some(Value::Num(v))].into_iter();
        assert!(e.admits(rest(1.0)), "the first call names the outer values");
        assert!(e.admits(rest(1.0)) && !e.admits(rest(2.0)) && e.admits_now(rest(1.0)) && !e.admits_now(rest(2.0)));
        for s in [7u32, 0, 3000, 7] {
            let mut st = Box::<RowsState>::default();
            st.upto = s as i64;
            e.put(s, st);
        }
        assert_eq!(e.live(), 3);
        assert_eq!(e.get(3000).map(|s| s.upto), Some(3000));
        assert!(e.get(5).is_none() && e.get(1 << 19).is_none());
        assert_eq!(e.take(7).map(|s| s.upto), Some(7));
        assert!(e.take(7).is_none());
        assert_eq!(e.live(), 2);
        e.clear();
        assert_eq!(e.live(), 0);
        assert!(e.get(0).is_none() && e.admits(rest(2.0)), "a cleared table is named again");
    }
}
