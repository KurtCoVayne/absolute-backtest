//! Rule evaluation: a body is solved literal by literal in the plan's order
//! (the written order without a plan), under Kleene's three-valued
//! conjunction (language-v2, section 5), each literal mapping a set of
//! partial bindings to a larger or smaller set. Derived relations are
//! requested through `call`, which memoises by temporal key.

use std::collections::{BTreeMap, BTreeSet};
use std::rc::Rc;

use abt_agg::{Function, ShiftedSums};

use super::time;
use super::value::{Ctor, Decision, Order, OrderKind, Tif, Value};
use super::{CompiledRule, Env, Kernel, RunError, Sym, Tuple};
use crate::ir::*;

/// The rows of one bar of a windowed group, each with the aggregate's
/// arguments evaluated on it.
pub type WindowRows = Vec<(Env, Vec<f64>)>;
/// A group's cached bars.
pub type WindowCache = BTreeMap<i64, WindowRows>;

/// The static shape of a rows window literal.
#[derive(Debug)]
pub(crate) struct RowsPlan {
    literal: usize,
    wslot: usize,
    base: Term,
    dur: Expr,
    outer: Vec<usize>,
    shareable: bool,
    /// `prior_rows`: the bar itself is excluded.
    prior: bool,
    order: Vec<Literal>,
    anchor: Vec<Literal>,
    /// Every slot the conjunction mentions, ascending.
    conj_slots: Vec<usize>,
}

/// A window's rows as their count and the aggregate's argument columns.
/// A window's rows as their count and the aggregate's argument columns, or
/// the aggregate itself when running sums answered it (no columns then).
type Columns = (usize, Vec<Vec<f64>>, Option<f64>);

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
    let lo = st.rows.partition_point(|(t1, _, _)| *t1 < lo_t);
    let hi = st.rows.partition_point(|(t1, _, _)| *t1 <= b);
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
        let first = st.rows.get(lo).filter(|_| lo < hi).map(|r| &r.2);
        let shift = (first.and_then(|f| f.first().copied()).unwrap_or(0.0), first.and_then(|f| f.get(1).copied()).unwrap_or(0.0));
        let shift = (if shift.0.is_finite() { shift.0 } else { 0.0 }, if shift.1.is_finite() { shift.1 } else { 0.0 });
        let mut r = RunningSums::new(st.base_id + lo as u64, shift);
        for i in lo..hi {
            r.apply(&st.rows[i].2, 1.0);
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
            r.apply(&rows[(id - base) as usize].2, 1.0);
        }
        for id in r.lo..lo_id.min(r.hi) {
            r.apply(&rows[(id - base) as usize].2, -1.0);
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
const ROWS_SLACK: usize = abt_agg::window::ROWS_SLACK;

/// A rows window group's running state: its last N anchored bars and the
/// rows among them (time, the row's locally bound slots in slot order, which
/// is the order the general path sorts environments by, and the aggregate's
/// argument values), through `upto`.
#[derive(Clone, Debug)]
pub(crate) struct RowsState {
    upto: i64,
    /// The buffer reaches the group's first bar.
    from_start: bool,
    anchors: std::collections::VecDeque<i64>,
    rows: std::collections::VecDeque<(i64, Vec<Value>, Vec<f64>)>,
    /// The id of `rows[0]`; a row's id is its position since the group began.
    base_id: u64,
    /// Row ids in the general path's order (their bound values, then time),
    /// kept as rows arrive and leave, when that is not time order.
    sorted: Vec<u64>,
    /// Sums over the window last served, for the moment aggregates.
    sums: Option<RunningSums>,
}

impl Default for RowsState {
    fn default() -> RowsState {
        RowsState {
            upto: i64::MIN,
            from_start: false,
            anchors: Default::default(),
            rows: Default::default(),
            base_id: 0,
            sorted: Vec::new(),
            sums: None,
        }
    }
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
const SORTED_MIN: usize = 16;

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
        let matches = |tu: &Tuple| pattern.iter().zip(tu.iter()).all(|(p, v)| p.as_ref().map(|p| p == v).unwrap_or(true));
        if info.stored {
            let entity = info.entity_positions.first().and_then(|&e| pattern[e].as_ref());
            let tuples: Vec<(Tuple, usize)> = self.stores[rel].lookup(key, entity).iter().filter(|tu| matches(tu)).map(|tu| (tu.clone(), usize::MAX)).collect();
            let cov = &mut self.coverage[rel];
            cov.0 += 1;
            cov.1 += usize::from(!tuples.is_empty());
            cov.2 += tuples.len();
            return Ok(Rc::new(tuples));
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
                Rc::new(slice.iter().filter(|(tu, _)| matches(tu)).cloned().collect())
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
        if let Some(hit) = self.memo.get(&memo_key) {
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
        // WF-4 rules out a relation depending on itself at the same time;
        // debug builds (the tests) still check that the checker did its job.
        if cfg!(debug_assertions) && !self.in_progress.insert(memo_key.clone()) {
            return Err(RunError::Internal(format!(
                "`{}` depends on itself at the same time; the checker should have rejected this (WF-4)",
                info.name
            )));
        }
        let derived = self.derive_rules(&info, pattern);
        if cfg!(debug_assertions) {
            self.in_progress.remove(&memo_key);
        }
        let (mut results, stucks) = match derived {
            Ok(r) => r,
            Err(e) => {
                if matches!(e, RunError::Arithmetic { .. }) {
                    self.memo_errors.insert(memo_key, e.clone());
                }
                return Err(e);
            }
        };
        if !stucks.is_empty() {
            self.memo_stuck.insert(memo_key.clone(), Rc::new(stucks));
        }
        if let (Some(e), true) = (entity, results.len() > SORTED_MIN) {
            results.sort_by(|a, b| a.0[e].cmp(&b.0[e]));
        }
        let cov = &mut self.coverage[rel];
        cov.0 += 1;
        cov.1 += usize::from(!results.is_empty());
        cov.2 += results.len();
        let rc = Rc::new(results);
        self.memo.insert(memo_key.clone(), rc.clone());
        if let Some((_, e)) = self.memo_stuck.get(&memo_key).and_then(|st| st.iter().find(|(p, _)| compatible(p))) {
            return Err(e.clone());
        }
        Ok(narrow(&rc))
    }

    /// Every tuple the rules of a derived relation yield for a pattern, with
    /// the rule that derived each (the first, when several do), and the
    /// instances left stuck on a partial operation as partial head tuples
    /// with their errors (`call` halts a reader whose pattern meets one).
    #[allow(clippy::type_complexity)]
    fn derive_rules(&mut self, info: &super::RelInfo, pattern: &[Option<Value>]) -> Result<(Vec<(Tuple, usize)>, Vec<(Vec<Option<Value>>, RunError)>), RunError> {
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
            let (sols, stuck) = self.solve_kleene(&cr, &rule.body, &order, vec![env], Some(ri))?;
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
            for sol in sols {
                let mut tu = Vec::with_capacity(rule.head.terms.len());
                for term in &rule.head.terms {
                    tu.push(self.term_value(&cr, term, &sol)?);
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
    pub(crate) fn plan_order(&self, ri: usize) -> Rc<Vec<usize>> {
        match self.plan_order.get(ri).and_then(|o| o.clone()) {
            Some(o) => o,
            None => Rc::new((0..self.prog.rules[ri].body.len()).collect()),
        }
    }

    fn term_value(&mut self, cr: &CompiledRule, term: &Term, env: &Env) -> Result<Value, RunError> {
        match term {
            Term::Var(v, _) => env[cr.slots[v]].clone().ok_or_else(|| RunError::Internal(format!("variable `{}` unbound in rule {}", v, cr.label))),
            Term::Wild(_) => Err(RunError::Internal(format!("wildcard where a value is needed in rule {}", cr.label))),
            Term::Param(p, _) => self.param_value(cr, p),
            Term::Lit(l, _) => Ok(super::lit_value(l, &mut self.symbols, &mut self.labels, &self.literal_equities)),
            Term::Ctor(c, subs, _) => {
                let ctor = Ctor::parse(c).ok_or_else(|| RunError::Internal(format!("unknown constructor `{}`", c)))?;
                let e = self.term_value(cr, &subs[0], env)?;
                let a = self.term_value(cr, &subs[1], env)?;
                let order = match subs.get(2) {
                    Some(o) => self.order_value(cr, o, env)?,
                    None => Order::default(),
                };
                match (e, a.as_f64()) {
                    (Value::Equity(s), Some(x)) => Ok(Value::Decision(Box::new(Decision { ctor, equity: s, amount: x, order }))),
                    _ => Err(RunError::Internal(format!("ill-typed decision in rule {}", cr.label))),
                }
            }
        }
    }

    /// The order term of a decision (section 6, order types): `market`,
    /// `moo`, `moc`, or `limit(P[, TIF])` / `stop(P[, TIF])` with TIF one of
    /// `day`, `gtc`, `bars(N)`; the checker has judged its shape.
    fn order_value(&mut self, cr: &CompiledRule, term: &Term, env: &Env) -> Result<Order, RunError> {
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

    fn param_value(&self, cr: &CompiledRule, name: &str) -> Result<Value, RunError> {
        let unit = &self.prog.rules[cr.idx].unit;
        self.params
            .get(&(unit.clone(), name.to_string()))
            .cloned()
            .ok_or_else(|| RunError::Internal(format!("unknown parameter `{}` in `{}`", name, unit)))
    }

    /// Solve a conjunction in the order written: the live solutions, or the
    /// halt of an instance left stuck on a partial operation.
    pub(crate) fn solve(&mut self, cr: &Rc<CompiledRule>, lits: &[Literal], envs: Vec<Env>) -> Result<Vec<Env>, RunError> {
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
    pub(crate) fn solve_kleene(&mut self, cr: &Rc<CompiledRule>, lits: &[Literal], order: &[usize], envs: Vec<Env>, record: Option<usize>) -> Result<(Vec<Env>, Vec<Stuck>), RunError> {
        self.solve_kleene_from(cr, lits, order, envs, Vec::new(), record)
    }

    /// `solve_kleene` continuing from live and stuck instances (what `explain`
    /// steps literal by literal).
    pub(crate) fn solve_kleene_from(
        &mut self,
        cr: &Rc<CompiledRule>,
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
                let mut out = Vec::new();
                match self.step(cr, lit, env, &mut out) {
                    Ok(()) => next.extend(out),
                    Err(error @ RunError::Arithmetic { .. }) => {
                        let undefined: Vec<usize> = Self::literal_slots(cr, lit).into_iter().filter(|&s| env[s].is_none()).collect();
                        stuck.push(Stuck { env: env.clone(), undefined, error });
                    }
                    Err(e) => return Err(e),
                }
            }
            if !stuck.is_empty() {
                let mentioned = Self::literal_slots(cr, lit);
                let mut still = Vec::with_capacity(stuck.len());
                for s in stuck.drain(..) {
                    if mentioned.iter().any(|m| s.undefined.contains(m)) {
                        still.push(s);
                        continue;
                    }
                    let mut out = Vec::new();
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
                            });
                        }
                        Err(e) => return Err(e),
                    }
                }
                stuck = still;
            }
            if let Some(ri) = record {
                if let Some(counts) = self.literal_counts.get_mut(ri) {
                    if let Some(c) = counts.get_mut(li) {
                        c.0 += entering as u64;
                        c.1 += next.len() as u64;
                        if entering > 0 && next.is_empty() && stuck.is_empty() {
                            c.2 += 1;
                        }
                    }
                }
            }
            live = next;
            if live.is_empty() && stuck.is_empty() {
                break;
            }
        }
        Ok((live, stuck))
    }

    /// The pattern an atom presents to `call`, and whether each term needs
    /// post-matching (constructor patterns and fresh variables).
    fn atom_pattern(&mut self, cr: &CompiledRule, atom: &Atom, env: &Env) -> Result<Vec<Option<Value>>, RunError> {
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
    fn atom_matches(&mut self, cr: &CompiledRule, atom: &Atom, env: &Env) -> Result<Vec<Env>, RunError> {
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
            let slots: Vec<Option<usize>> = atom.terms.iter().map(|t| if let Term::Var(v, _) = t { Some(cr.slots[v]) } else { None }).collect();
            let entity = entity_pos.and_then(|e| pat[e].clone());
            let mut out = Vec::new();
            'tuples: for tu in self.stores[rel].lookup(key, entity.as_ref()) {
                if !pat.iter().zip(tu.iter()).all(|(p, v)| p.as_ref().map(|p| p == v).unwrap_or(true)) {
                    continue;
                }
                let mut e = env.clone();
                for (slot, val) in slots.iter().zip(tu.iter()) {
                    if let Some(slot) = *slot {
                        match &e[slot] {
                            Some(x) => {
                                if x != val {
                                    continue 'tuples;
                                }
                            }
                            None => e[slot] = Some(val.clone()),
                        }
                    }
                }
                out.push(e);
            }
            return Ok(out);
        }
        let tuples = self.call(rel, &pat)?;
        self.bind_tuples(cr, atom, env, &tuples)
    }

    /// `R(...) asof T`: R's tuples at the latest key at or before T that
    /// has one matching the bound terms (data-bundle doc, section 2: the
    /// as-of join). A stored relation is answered from its index; a derived
    /// one by calling it at its own resolution's bars backwards from T,
    /// with the answer memoised for every bar walked.
    fn asof_matches(&mut self, cr: &CompiledRule, atom: &Atom, at: &Term, within: Option<&Cal>, env: &Env) -> Result<Vec<Env>, RunError> {
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
            let matches = |tu: &Tuple| pat.iter().zip(tu.iter()).all(|(p, v)| p.as_ref().map(|p| p == v).unwrap_or(true));
            let mut found: Vec<(Tuple, usize)> = Vec::new();
            let entity = info.entity_positions.first().and_then(|&e| pat[e].clone());
            let mut cur = t;
            while let Some(k) = self.stores[rel].by_time.range(floor..=cur).next_back().map(|(k, _)| *k) {
                found.extend(self.stores[rel].lookup(k, entity.as_ref()).iter().filter(|tu| matches(tu)).map(|tu| (tu.clone(), usize::MAX)));
                if !found.is_empty() || k == i64::MIN {
                    break;
                }
                cur = k - 1;
            }
            Rc::new(found)
        } else {
            let mut mkey: Vec<Value> = Vec::with_capacity(info.inputs.len());
            for &i in &info.inputs {
                match &pat[i] {
                    Some(v) => mkey.push(v.clone()),
                    None => return Err(RunError::Internal(format!("`{}` requested as of {} with its input #{} unbound", info.name, t, i + 1))),
                }
            }
            let entity = info.entity_positions.first().and_then(|&e| pat[e].as_ref()).and_then(|v| v.as_equity());
            let rel_dom: Vec<i64> = self.domain_for(&info.cal, entity)?.range(floor..=t).rev().copied().collect();
            // The common case: the latest bar itself has a tuple (a relation
            // read at its previous row). Answered directly, nothing memoised.
            if let Some(k) = rel_dom.first().copied() {
                pat[info.key_pos] = Some(Value::Time(k));
                let first = self.call(rel, &pat)?;
                if !first.is_empty() {
                    return self.bind_tuples(cr, atom, env, &first);
                }
            }
            let mut walked: Vec<i64> = Vec::new();
            let mut found: Option<i64> = None;
            for k in rel_dom {
                if let Some(hit) = self.asof_memo.get(&(rel, mkey.clone(), k)) {
                    found = *hit;
                    break;
                }
                walked.push(k);
                pat[info.key_pos] = Some(Value::Time(k));
                if !self.call(rel, &pat)?.is_empty() {
                    found = Some(k);
                    break;
                }
            }
            for k in walked {
                self.asof_memo.insert((rel, mkey.clone(), k), found);
            }
            match found {
                Some(k) => {
                    pat[info.key_pos] = Some(Value::Time(k));
                    self.call(rel, &pat)?
                }
                None => Rc::new(Vec::new()),
            }
        };
        self.bind_tuples(cr, atom, env, &tuples)
    }

    fn bind_tuples(&mut self, cr: &CompiledRule, atom: &Atom, env: &Env, tuples: &super::Derived) -> Result<Vec<Env>, RunError> {
        let mut out = Vec::new();
        'tuples: for (tu, _) in tuples.iter() {
            let mut e = env.clone();
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
            out.push(e);
        }
        Ok(out)
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

    fn time_of(&self, cr: &CompiledRule, term: &Term, env: &Env) -> Result<i64, RunError> {
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
    fn bind_time(&self, cr: &CompiledRule, term: &Term, env: &Env, t: i64, out: &mut Vec<Env>) {
        match term {
            Term::Var(v, _) => {
                let slot = cr.slots[v];
                match &env[slot] {
                    Some(Value::Time(x)) if *x == t => out.push(env.clone()),
                    Some(_) => {}
                    None => {
                        let mut e = env.clone();
                        e[slot] = Some(Value::Time(t));
                        out.push(e);
                    }
                }
            }
            Term::Wild(_) => out.push(env.clone()),
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
    fn domain_of_term(&self, cr: &CompiledRule, term: &Term, env: &Env) -> Result<&std::collections::BTreeSet<i64>, RunError> {
        let cal = self.term_cal(cr, term);
        let entity = cr.entity_slot.and_then(|s| env.get(s)).and_then(|v| v.as_ref()).and_then(|v| v.as_equity());
        self.domain_for(&cal, entity)
    }

    /// The time domain of a rule: its calendar's, and for the per-instrument
    /// sessions calendar the sessions of the head's instrument.
    fn domain(&self, cr: &CompiledRule, env: &Env) -> Result<&std::collections::BTreeSet<i64>, RunError> {
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
    fn window_times(&mut self, cr: &CompiledRule, kind: &WindowKind, base: &Term, dur: &Expr, lo: Option<&Expr>, env: &Env) -> Result<Vec<i64>, RunError> {
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
                Ok(self
                    .domains
                    .get(&Cal::Base)
                    .map(|d| d.range(from..to.min(end + 1)).filter(|b| **b > open && **b <= end).copied().collect())
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
    fn case_value(&mut self, cr: &Rc<CompiledRule>, branches: &[Branch], otherwise: Option<&Expr>, env: &Env) -> Result<Option<Value>, RunError> {
        for b in branches {
            let order: Vec<usize> = (0..b.guard.len()).collect();
            let (sols, stuck) = self.solve_kleene(cr, &b.guard, &order, vec![env.clone()], None)?;
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
    fn domain_bars(&mut self, cr: &CompiledRule, env: &Env) -> Result<Rc<Vec<i64>>, RunError> {
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
        let v: Rc<Vec<i64>> = Rc::new(self.domain(cr, env)?.iter().copied().collect());
        self.domain_vecs.insert(key, v.clone());
        Ok(v)
    }

    pub(crate) fn step(&mut self, cr: &Rc<CompiledRule>, lit: &Literal, env: &Env, out: &mut Vec<Env>) -> Result<(), RunError> {
        match lit {
            Literal::Atom(a) => out.extend(self.atom_matches(cr, a, env)?),
            Literal::AsOf { atom, at, within_bucket, .. } => out.extend(self.asof_matches(cr, atom, at, within_bucket.as_ref(), env)?),
            Literal::Neg(a) => {
                if self.atom_matches(cr, a, env)?.is_empty() {
                    out.push(env.clone());
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
                        out.push(env.clone());
                    }
                }
                Builtin::DayStart { t } => {
                    let at = self.time_of(cr, t, env)?;
                    let prev = self.domain_of_term(cr, t, env)?.range(..at).next_back().copied();
                    if prev.map(|p| time::day_key(p) != time::day_key(at)).unwrap_or(true) {
                        out.push(env.clone());
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
                            let mut e = env.clone();
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
                                out.push(env.clone());
                            }
                        }
                        None => {
                            let mut e = env.clone();
                            e[slot] = Some(v);
                            out.push(e);
                        }
                    }
                }
            }
            Literal::Cmp { op, lhs, rhs, .. } => {
                let a = self.eval_expr(cr, lhs, env)?;
                let b = self.eval_expr(cr, rhs, env)?;
                if compare(*op, &a, &b) {
                    out.push(env.clone());
                }
            }
            Literal::Assign { var, expr, .. } => {
                let v = self.eval_expr(cr, expr, env)?;
                let slot = cr.slots[var];
                match &env[slot] {
                    Some(x) => {
                        if values_equal(x, &v) {
                            out.push(env.clone());
                        }
                    }
                    None => {
                        let mut e = env.clone();
                        e[slot] = Some(v);
                        out.push(e);
                    }
                }
            }
            Literal::Agg { var, agg, args, conj, within, .. } => {
                if self.pushed_answers(cr, lit, var, env, out) {
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
                // The group (language-v2, 4.2; semantic model, 5.1): with
                // `within`, exactly its variables; otherwise the variables
                // the conjunction shares with the rest of the rule (the head
                // or another literal). A variable of the conjunction alone
                // is local to it, neither a key nor bound by the aggregation.
                // A group variable bound outside is an input of the group; an
                // unbound one is a key the aggregation binds, with one result
                // per distinct binding (`close@C(A, T, P) :- P = last(P1)
                // over (...)` groups by A).
                let group_vars: Vec<String> = match within {
                    Some(w) => w.clone(),
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
                let mut group_slots: Vec<usize> = group_vars.iter().filter_map(|v| cr.slots.get(v).copied()).filter(|&s| env[s].is_none()).collect();
                group_slots.sort_unstable();
                group_slots.dedup();
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
                    if let Some((count, cols, running)) = self.rows_columns(cr, lit, conj, agg, args, env)? {
                        if (count as i64) < min_count {
                            return Ok(());
                        }
                        let value = if agg == "count" {
                            Value::Count(count as i64)
                        } else if let Some(v) = running {
                            Value::Num(v)
                        } else {
                            match aggregate(agg, &cols) {
                                Ok(Some(v)) => v,
                                Ok(None) => return Ok(()),
                                Err(m) => {
                                    let a: Vec<String> = args.iter().map(|e| e.to_string()).collect();
                                    return Err(self.arith_text(cr, env, format!("{}({}) over (...)", agg, a.join(", ")), &m));
                                }
                            }
                        };
                        let mut e = env.clone();
                        e[cr.slots[var]] = Some(value);
                        out.push(e);
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
                } else if self.cfg.window_cache {
                    self.window_rows(cr, lit, conj, agg, args, env)?
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
                        let sols = self.solve(cr, &order, vec![env.clone()])?;
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
                        let mut e = env.clone();
                        for (s, v) in group_slots.iter().zip(key) {
                            e[*s] = v;
                        }
                        e[cr.slots[var]] = Some(value);
                        out.push(e);
                    }
                    return Ok(());
                }
                if agg == "count" {
                    let mut e = env.clone();
                    e[cr.slots[var]] = Some(Value::Count(rows.len() as i64));
                    out.push(e);
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
                let mut e = env.clone();
                e[cr.slots[var]] = Some(value);
                out.push(e);
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
                out.extend(rows);
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
    fn pushed_answers(&mut self, cr: &CompiledRule, lit: &Literal, var: &str, env: &Env, out: &mut Vec<Env>) -> bool {
        if self.pushed.is_empty() {
            return false;
        }
        let Some(pos) = self.prog.rules[cr.idx].body.iter().position(|l| std::ptr::eq(l, lit)) else {
            return false;
        };
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
                let mut e = env.clone();
                e[vslot] = Some(Value::Num(x));
                out.push(e);
            }
            Some(None) => {
                if section.iter().any(|(_, a)| a.value.is_none() || a.error || a.r > self.now) {
                    return false;
                }
                for (s, a) in section {
                    self.pushed_read_r = self.pushed_read_r.max(a.r);
                    let mut e = env.clone();
                    e[p.entity_slot] = Some(Value::Equity(*s));
                    e[vslot] = a.value.map(Value::Num);
                    out.push(e);
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
    fn agg_args(&mut self, cr: &CompiledRule, args: &[Expr], row: &Env) -> Result<Vec<f64>, RunError> {
        let mut vals = Vec::with_capacity(args.len());
        for a in args {
            let v = self.eval_expr(cr, a, row)?;
            vals.push(v.as_f64().ok_or_else(|| self.arith(cr, row, a, "aggregate argument is not numeric"))?);
        }
        Ok(vals)
    }

    /// The rows of a windowed group from the per-bar cache (data-bundle doc,
    /// section 2, "State"): the group is the rule, the aggregation literal
    /// and the outer bindings its conjunction reads apart from the window's
    /// base time; each bar of the window is solved once, with the window
    /// variable bound to it and the rest of the conjunction in evaluation
    /// order, and kept until it leaves the window. `None` when the group is
    /// not a single-window one (solved whole instead).
    fn window_rows(&mut self, cr: &Rc<CompiledRule>, lit: &Literal, conj: &[Literal], agg: &str, args: &[Expr], env: &Env) -> Result<Option<WindowRows>, RunError> {
        let windows: Vec<usize> = conj.iter().enumerate().filter(|(_, l)| matches!(l, Literal::Window { .. })).map(|(i, _)| i).collect();
        if windows.len() != 1 {
            return Ok(None);
        }
        let Literal::Window {
            var: Term::Var(wvar, _),
            kind,
            base: base @ Term::Var(bvar, _),
            dur,
            min,
            lo: lo_off,
            ..
        } = &conj[windows[0]]
        else {
            return Ok(None);
        };
        let wslot = cr.slots[wvar];
        if env[wslot].is_some() {
            return Ok(None);
        }
        if matches!(kind, WindowKind::Rows | WindowKind::PriorRows) {
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
        // The outer bindings the conjunction reads (not the window variable):
        // the rows of a bar are a function of them and the bar.
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
        // A conjunction that reads the base time itself has rows that change
        // with every call: nothing to share, so solve it whole.
        if refs.iter().any(|v| v == bvar) {
            return Ok(None);
        }
        // The window's own length and minimum tell groups of different
        // windows over one conjunction apart.
        for e in [dur, min] {
            let mut vs = Vec::new();
            e.vars(&mut vs);
            refs.extend(vs.into_iter().map(|(v, _)| v));
        }
        let mut outer: Vec<usize> = refs.iter().map(|v| cr.slots[v]).filter(|&s| s != wslot).collect();
        outer.sort_unstable();
        outer.dedup();
        // The literal by its position in the rule's body (aggregations do not
        // nest), so that the key survives a checkpoint.
        let literal = self.literal_index(cr, lit);
        let key = WindowKey {
            rule: cr.idx,
            literal,
            outer: outer.into_iter().map(|s| env[s].clone()).collect(),
        };
        let order: Vec<Literal> = conj_order(conj).into_iter().filter(|l| !matches!(l, Literal::Window { .. })).collect();
        let mut cache = self.windows.remove(&key).unwrap_or_default();
        for &t1 in &times {
            if cache.contains_key(&t1) {
                continue;
            }
            let mut e = env.clone();
            e[wslot] = Some(Value::Time(t1));
            let sols = match self.solve(cr, &order, vec![e]) {
                Ok(s) => s,
                Err(err) => {
                    self.windows.insert(key, cache);
                    return Err(err);
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
                            return Err(err);
                        }
                    }
                };
                rows.push((row, vals));
            }
            cache.insert(t1, rows);
        }
        // A cached row carries the outer bindings of the call that solved it;
        // what the aggregate sees is this call's outer bindings plus the
        // conjunction's own (the slots unbound here), exactly as a whole
        // solve would produce, so that the rows sort the same way.
        let mut rows = Vec::new();
        for t1 in &times {
            if let Some(r) = cache.get(t1) {
                for (row, vals) in r {
                    let mut e = env.clone();
                    for (i, v) in row.iter().enumerate() {
                        if env[i].is_none() {
                            e[i] = v.clone();
                        }
                    }
                    rows.push((e, vals.clone()));
                }
            }
        }
        if self.recording {
            self.summary_rows.push(super::SummaryRow {
                rule: key.rule,
                literal: key.literal,
                outer: key.outer.clone(),
                bar: t,
                agg: agg.to_string(),
                rows: rows.len(),
                path: "cached rows",
                shift: None,
                sums: None,
                value: None,
            });
        }
        // Bars before the window's start will not be asked for again by a
        // later bar of the same group; a request for an earlier base time
        // simply solves them again.
        cache = cache.split_off(&lo);
        self.windows.insert(key, cache);
        Ok(Some(rows))
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
    fn rows_plan(&mut self, cr: &Rc<CompiledRule>, lit: &Literal, conj: &[Literal], args: &[Expr]) -> Result<Rc<RowsPlan>, RunError> {
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
        let plan = Rc::new(RowsPlan {
            literal,
            wslot,
            base: Term::Var(bvar.clone(), crate::ir::Span::default()),
            dur: dur.clone(),
            outer,
            shareable,
            prior,
            order: conj_order(conj).into_iter().filter(|l| !matches!(l, Literal::Window { .. })).collect(),
            anchor: conj.iter().find(|l| matches!(l, Literal::Atom(_))).cloned().into_iter().collect(),
            conj_slots: {
                let mut vars = Vec::new();
                for l in conj {
                    literal_vars(l, &mut vars);
                }
                let mut slots: Vec<usize> = vars.iter().map(|v| cr.slots[v]).collect();
                slots.sort_unstable();
                slots.dedup();
                slots
            },
        });
        self.rows_plans.insert((cr.idx, literal), plan.clone());
        Ok(plan)
    }

    /// A time-ordered rows window as columns of its aggregate's arguments, in
    /// time order, kept incrementally per group: each call solves only the
    /// bars since the group's previous call, and the group holds its last N
    /// anchored bars and their rows. `None` when the window cannot be served
    /// so (a bar with more than one row, a base time read by the rows, or a
    /// call earlier than the group's last): the general path answers then.
    fn rows_columns(&mut self, cr: &Rc<CompiledRule>, lit: &Literal, conj: &[Literal], agg: &str, args: &[Expr], env: &Env) -> Result<Option<Columns>, RunError> {
        let plan = self.rows_plan(cr, lit, conj, args)?;
        if !plan.shareable || plan.prior || env[plan.wslot].is_some() {
            return Ok(None);
        }
        let t = self.time_of(cr, &plan.base, env)?;
        let n = self.eval_expr(cr, &plan.dur, env)?.as_f64().unwrap_or(0.0).max(0.0) as usize;
        // The slots the rows bind. A window is read in its time order, the
        // general path's sort (E-46), and a bar holds one row here, so the
        // rows' order is the bars'.
        let locals: Vec<usize> = plan.conj_slots.iter().copied().filter(|&s| env[s].is_none()).collect();
        let time_first = true;
        let key = WindowKey {
            rule: cr.idx,
            literal: plan.literal,
            outer: plan.outer.iter().map(|&s| env[s].clone()).collect(),
        };
        // A request behind the group's last bar (a query after the run, a
        // recursion reaching back) is served from a buffer rebuilt by walking
        // back from that bar, exactly as a group first asked there would be,
        // so that the running sums answer it along the same canonical walk;
        // the group's own buffer is left as it is.
        let mut original = self.rows_state.remove(&key);
        let backward = original.as_ref().map(|s| s.upto != i64::MIN && t < s.upto).unwrap_or(false);
        let mut st = if backward { RowsState::default() } else { original.take().unwrap_or_default() };
        if backward {
            self.stats.window_backward += 1;
        }
        // Bring the group up to t: a new group walks back to N (plus slack)
        // anchored bars; a known one solves only the bars since its last call.
        let fresh: Vec<i64> = if st.anchors.is_empty() && st.rows.is_empty() && st.upto == i64::MIN {
            let mut bars: Vec<i64> = Vec::new();
            let mut anchors = 0usize;
            let mut upper = t;
            st.from_start = true;
            'walk: loop {
                let chunk: Vec<i64> = self.domain(cr, env)?.range(..=upper).rev().take(64).copied().collect();
                if chunk.is_empty() {
                    break;
                }
                for &t1 in &chunk {
                    let mut e = env.clone();
                    e[plan.wslot] = Some(Value::Time(t1));
                    if plan.anchor.is_empty() || !self.solve(cr, &plan.anchor, vec![e])?.is_empty() {
                        anchors += 1;
                    }
                    bars.push(t1);
                    if anchors >= n + abt_agg::checkpoint_period(n) + ROWS_SLACK {
                        st.from_start = false;
                        break 'walk;
                    }
                }
                match chunk.last() {
                    Some(&last) if last > i64::MIN => upper = last - 1,
                    _ => break,
                }
            }
            bars.reverse();
            bars
        } else if t > st.upto {
            self.domain(cr, env)?.range((std::ops::Bound::Excluded(st.upto), std::ops::Bound::Included(t))).copied().collect()
        } else {
            Vec::new()
        };
        for t1 in fresh {
            let mut e = env.clone();
            e[plan.wslot] = Some(Value::Time(t1));
            let anchored = plan.anchor.is_empty() || !self.solve(cr, &plan.anchor, vec![e.clone()])?.is_empty();
            let sols = self.solve(cr, &plan.order, vec![e])?;
            self.stats.window_bars_solved += 1;
            if sols.len() > 1 {
                return Ok(None);
            }
            if anchored {
                st.anchors.push_back(t1);
            }
            if let Some(row) = sols.first() {
                let vals = if agg == "count" { Vec::new() } else { self.agg_args(cr, args, row)? };
                let key: Vec<Value> = if time_first {
                    Vec::new()
                } else {
                    locals.iter().map(|&s| row[s].clone().unwrap_or(Value::Count(0))).collect()
                };
                let id = st.base_id + st.rows.len() as u64;
                if !time_first {
                    let base = st.base_id;
                    let pos = st.sorted.partition_point(|&i| st.rows[(i - base) as usize].1 <= key);
                    st.sorted.insert(pos, id);
                }
                st.rows.push_back((t1, key, vals));
            }
        }
        st.upto = st.upto.max(t);
        // Serve t from the buffer: its last N anchored bars at or before t
        // (all of them when the buffer reaches the group's first bar).
        let hi = st.anchors.partition_point(|a| *a <= t);
        let served = if hi >= n || st.from_start {
            let lo_t = if hi >= n && n > 0 {
                st.anchors[hi - n]
            } else if hi > 0 {
                st.anchors[0]
            } else {
                i64::MAX
            };
            let lo = st.rows.partition_point(|(t1, _, _)| *t1 < lo_t);
            let hi_r = st.rows.partition_point(|(t1, _, _)| *t1 <= t);
            let count = hi_r.saturating_sub(lo);
            let running = if self.cfg.running_sums && moment_aggregate(agg) && count >= 2 {
                let bars = self.domain_bars(cr, env)?;
                let mut noncanonical = 0;
                let v = running_value(&mut st, agg, t, n, &bars, &mut noncanonical);
                self.stats.window_noncanonical += noncanonical;
                v
            } else {
                None
            };
            if self.recording {
                let sums = if running.is_some() { st.sums.as_ref() } else { None };
                self.summary_rows.push(super::SummaryRow {
                    rule: key.rule,
                    literal: key.literal,
                    outer: key.outer.clone(),
                    bar: t,
                    agg: agg.to_string(),
                    rows: count,
                    path: if running.is_some() { "running" } else { "two_pass" },
                    shift: sums.map(|r| r.sums.shift),
                    sums: sums.map(|r| r.sums.s.map(|x| x.value())),
                    value: running,
                });
            }
            let mut cols: Vec<Vec<f64>> = vec![Vec::with_capacity(count); args.len()];
            if agg != "count" && count > 0 && running.is_none() {
                let (lo_id, hi_id) = (st.base_id + lo as u64, st.base_id + hi_r as u64);
                let mut push = |vals: &[f64]| {
                    for (j, v) in vals.iter().enumerate() {
                        cols[j].push(*v);
                    }
                };
                if time_first {
                    for i in lo..hi_r {
                        push(&st.rows[i].2);
                    }
                } else {
                    for &id in st.sorted.iter().filter(|&&id| lo_id <= id && id < hi_id) {
                        push(&st.rows[(id - st.base_id) as usize].2);
                    }
                }
            }
            Some((count, cols, running))
        } else {
            None
        };
        // Keep N plus a checkpoint's span plus slack anchored bars (a call a
        // few bars back, and the rebuild at the checkpoint before any bar,
        // are served from the buffer).
        while st.anchors.len() > n + abt_agg::checkpoint_period(n) + ROWS_SLACK {
            st.anchors.pop_front();
            st.from_start = false;
        }
        let oldest = st.anchors.front().copied().unwrap_or(i64::MAX);
        while st.rows.front().map(|(t1, _, _)| *t1 < oldest).unwrap_or(false) {
            if !st.sorted.is_empty() {
                let (base, front) = (st.base_id, &st.rows[0].1);
                let pos = st.sorted.partition_point(|&i| {
                    let k = &st.rows[(i - base) as usize].1;
                    k < front || (k == front && i < base)
                });
                st.sorted.remove(pos);
            }
            st.rows.pop_front();
            st.base_id += 1;
        }
        match (backward, original) {
            (true, Some(o)) => {
                self.rows_state.insert(key, o);
            }
            _ => {
                self.rows_state.insert(key, st);
            }
        }
        Ok(served)
    }

    /// Whether a rows window's time variable is the lowest slot among the
    /// variables its conjunction binds (then rows in time order are rows in
    /// environment order). Judged once per rule and literal.
    fn time_ordered(&mut self, cr: &Rc<CompiledRule>, lit: &Literal, conj: &[Literal], env: &Env) -> bool {
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
    fn rows_window(&mut self, cr: &Rc<CompiledRule>, lit: &Literal, conj: &[Literal], agg: &str, args: &[Expr], env: &Env, want_env: bool) -> Result<(WindowRows, bool), RunError> {
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
                    let mut e = env.clone();
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
                let mut e = env.clone();
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
                    cache = cache.split_off(&oldest);
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

    fn arith(&self, cr: &CompiledRule, env: &Env, expr: &Expr, message: &str) -> RunError {
        self.arith_text(cr, env, expr.to_string(), message)
    }

    /// The partial-arithmetic halt of section 7, with the offending
    /// expression given as text (an aggregate names the whole aggregate).
    fn arith_text(&self, cr: &CompiledRule, env: &Env, expr: String, message: &str) -> RunError {
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

    pub(crate) fn eval_expr(&mut self, cr: &CompiledRule, e: &Expr, env: &Env) -> Result<Value, RunError> {
        match e {
            Expr::Var(v, _) => env[cr.slots[v]].clone().ok_or_else(|| RunError::Internal(format!("variable `{}` unbound in rule {}", v, cr.label))),
            Expr::Param(p, _) => self.param_value(cr, p),
            Expr::Lit(l, _) => Ok(super::lit_value(l, &mut self.symbols, &mut self.labels, &self.literal_equities)),
            Expr::Neg(x, _) => match self.eval_expr(cr, x, env)? {
                Value::Num(v) => Ok(Value::Num(-v)),
                Value::Count(c) => Ok(Value::Count(-c)),
                _ => Err(self.arith(cr, env, e, "cannot negate a non-numeric value")),
            },
            Expr::Bin(op, a, b, _) => {
                let x = self.eval_expr(cr, a, env)?;
                let y = self.eval_expr(cr, b, env)?;
                match (op, &x, &y) {
                    (BinOp::Add, Value::Count(p), Value::Count(q)) => Ok(Value::Count(p + q)),
                    (BinOp::Sub, Value::Count(p), Value::Count(q)) => Ok(Value::Count(p - q)),
                    (BinOp::Mul, Value::Count(p), Value::Count(q)) => Ok(Value::Count(p * q)),
                    _ => {
                        let (p, q) = match (x.as_f64(), y.as_f64()) {
                            (Some(p), Some(q)) => (p, q),
                            _ => return Err(self.arith(cr, env, e, "arithmetic on non-numeric values")),
                        };
                        Ok(Value::Num(match op {
                            BinOp::Add => p + q,
                            BinOp::Sub => p - q,
                            BinOp::Mul => p * q,
                            BinOp::Div => {
                                if q == 0.0 {
                                    return Err(self.arith(cr, env, e, "division by zero"));
                                }
                                p / q
                            }
                        }))
                    }
                }
            }
            Expr::Call(f, args, _) => {
                let mut vals = Vec::with_capacity(args.len());
                for a in args {
                    vals.push(self.eval_expr(cr, a, env)?);
                }
                let num = |v: &Value| v.as_f64();
                match f.as_str() {
                    "log" => {
                        let x = num(&vals[0]).ok_or_else(|| self.arith(cr, env, e, "log of a non-numeric value"))?;
                        if x <= 0.0 {
                            return Err(self.arith(cr, env, e, &format!("log of a non-positive value ({})", x)));
                        }
                        Ok(Value::Num(x.ln()))
                    }
                    "exp" => Ok(Value::Num(num(&vals[0]).ok_or_else(|| self.arith(cr, env, e, "exp of a non-numeric value"))?.exp())),
                    "sqrt" => {
                        let x = num(&vals[0]).ok_or_else(|| self.arith(cr, env, e, "sqrt of a non-numeric value"))?;
                        if x < 0.0 {
                            return Err(self.arith(cr, env, e, &format!("sqrt of a negative value ({})", x)));
                        }
                        Ok(Value::Num(x.sqrt()))
                    }
                    "abs" => match &vals[0] {
                        Value::Count(c) => Ok(Value::Count(c.abs())),
                        v => Ok(Value::Num(num(v).ok_or_else(|| self.arith(cr, env, e, "abs of a non-numeric value"))?.abs())),
                    },
                    "sign" => {
                        let x = num(&vals[0]).ok_or_else(|| self.arith(cr, env, e, "sign of a non-numeric value"))?;
                        Ok(Value::Num(if x > 0.0 {
                            1.0
                        } else if x < 0.0 {
                            -1.0
                        } else {
                            0.0
                        }))
                    }
                    "floor" => Ok(Value::Num(num(&vals[0]).ok_or_else(|| self.arith(cr, env, e, "floor of a non-numeric value"))?.floor())),
                    "ceil" => Ok(Value::Num(num(&vals[0]).ok_or_else(|| self.arith(cr, env, e, "ceil of a non-numeric value"))?.ceil())),
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
                    "least" | "greatest" => {
                        // An integer literal is a Count and a Scalar is a Num; the
                        // checker admits the mix (`least(1, W)`), so the comparison
                        // is numeric across the two, never the enum's variant order.
                        let num = |v: &Value| match v {
                            Value::Num(x) => Some(*x),
                            Value::Count(c) => Some(*c as f64),
                            _ => None,
                        };
                        let mut best = vals[0].clone();
                        for v in &vals[1..] {
                            let better = match (num(v), num(&best)) {
                                (Some(a), Some(b)) => {
                                    if f == "least" {
                                        a < b
                                    } else {
                                        a > b
                                    }
                                }
                                _ => {
                                    if f == "least" {
                                        v < &best
                                    } else {
                                        v > &best
                                    }
                                }
                            };
                            if better {
                                best = v.clone();
                            }
                        }
                        // The result has the checker's type: a Scalar when any argument is one.
                        if let (Value::Count(c), true) = (&best, vals.iter().any(|v| matches!(v, Value::Num(_)))) {
                            best = Value::Num(*c as f64);
                        }
                        Ok(best)
                    }
                    _ => Err(RunError::Internal(format!("unknown function `{}`", f))),
                }
            }
        }
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
            (Some(x), Some(y)) => match x.partial_cmp(&y) {
                Some(o) => o,
                None => return false,
            },
            _ => a.cmp(b),
        },
    };
    match op {
        CmpOp::Lt => ord == std::cmp::Ordering::Less,
        CmpOp::Le => ord != std::cmp::Ordering::Greater,
        CmpOp::Eq => ord == std::cmp::Ordering::Equal,
        CmpOp::Gt => ord == std::cmp::Ordering::Greater,
        CmpOp::Ge => ord != std::cmp::Ordering::Less,
    }
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
    let f = Function::parse(name, level).map_err(|e| e.to_string())?;
    let columns: Vec<&[f64]> = cols.iter().take(f.arity()).map(|c| c.as_slice()).collect();
    abt_agg::reduce(f, abt_agg::Mode::TwoPass, &columns).map(|v| v.map(Value::Num)).map_err(|e| e.to_string())
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
            st.rows.push_back((i as i64, Vec::new(), vec![*a, *b]));
            st.anchors.push_back(i as i64);
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
