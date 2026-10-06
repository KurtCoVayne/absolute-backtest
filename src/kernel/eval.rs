//! Rule evaluation: a body is solved literal by literal in the order written
//! (section 4), each literal mapping a set of partial bindings to a larger
//! or smaller set. Derived relations are requested through `call`, which
//! memoises by temporal key and inputs.

use std::collections::{BTreeMap, BTreeSet};
use std::rc::Rc;

use super::time;
use super::value::{Ctor, Decision, Order, OrderKind, Tif, Value};
use super::{CompiledRule, Env, Kernel, RunError, Tuple};
use crate::ir::*;

/// The rows of one bar of a windowed group, each with the aggregate's
/// arguments evaluated on it.
pub type WindowRows = Vec<(Env, Vec<f64>)>;
/// A group's cached bars.
pub type WindowCache = BTreeMap<i64, WindowRows>;

/// A windowed aggregation group: the rule, the aggregation literal, and the
/// outer bindings its conjunction reads apart from the window's base time.
#[derive(Clone, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct WindowKey {
    pub rule: usize,
    pub literal: usize,
    pub outer: Vec<Option<Value>>,
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
        let matches = |tu: &Tuple| pattern.iter().zip(tu.iter()).all(|(p, v)| p.as_ref().map(|p| p == v).unwrap_or(true));
        if info.stored {
            let entity = info.entity_positions.first().and_then(|&e| pattern[e].as_ref());
            let tuples: Vec<(Tuple, usize)> = self.stores[rel].lookup(key, entity).iter().filter(|tu| matches(tu)).map(|tu| (tu.clone(), usize::MAX)).collect();
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
        if let Some(hit) = self.memo.get(&memo_key) {
            let hit = hit.clone();
            return Ok(if pattern.iter().all(|p| p.is_none()) || hit.iter().all(|(tu, _)| matches(tu)) {
                hit
            } else {
                Rc::new(hit.iter().filter(|(tu, _)| matches(tu)).cloned().collect())
            });
        }
        if !self.in_progress.insert(memo_key.clone()) {
            return Err(RunError::Internal(format!(
                "`{}` depends on itself at the same time; the checker should have rejected this (WF-4)",
                info.name
            )));
        }
        let mut results: Vec<(Tuple, usize)> = Vec::new();
        let mut seen: super::hash::FxHashSet<Tuple> = Default::default();
        for &ri in &info.rules {
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
            let sols = self.solve(&cr, &rule.body, vec![env])?;
            for sol in sols {
                let mut tu = Vec::with_capacity(rule.head.terms.len());
                for term in &rule.head.terms {
                    tu.push(self.term_value(&cr, term, &sol)?);
                }
                if seen.insert(tu.clone()) {
                    results.push((tu, ri));
                }
            }
        }
        self.in_progress.remove(&memo_key);
        let rc = Rc::new(results);
        self.memo.insert(memo_key, rc.clone());
        Ok(if rc.iter().all(|(tu, _)| matches(tu)) {
            rc
        } else {
            Rc::new(rc.iter().filter(|(tu, _)| matches(tu)).cloned().collect())
        })
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

    pub(crate) fn solve(&mut self, cr: &Rc<CompiledRule>, lits: &[Literal], mut envs: Vec<Env>) -> Result<Vec<Env>, RunError> {
        for lit in lits {
            let mut next = Vec::new();
            for env in &envs {
                self.step(cr, lit, env, &mut next)?;
            }
            envs = next;
            if envs.is_empty() {
                break;
            }
        }
        Ok(envs)
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
        let tuples = self.call(rel, &pat)?;
        self.bind_tuples(cr, atom, env, &tuples)
    }

    /// `R(...) asof T`: R's tuples at the latest key at or before T that
    /// has one matching the bound terms (data-bundle doc, section 2: the
    /// as-of join). A stored relation is answered from its index; a derived
    /// one by calling it at its own resolution's bars backwards from T,
    /// with the answer memoised for every bar walked.
    fn asof_matches(&mut self, cr: &CompiledRule, atom: &Atom, at: &Term, env: &Env) -> Result<Vec<Env>, RunError> {
        let rel = self.rel_ids.get(&atom.name).copied().ok_or_else(|| RunError::Internal(format!("unknown relation `{}`", atom.name)))?;
        let at_t = self.time_of(cr, at, env)?;
        let info = self.rels[rel].clone();
        // A tuple is available at its bar's close (section 3): a daily bar is
        // labelled by its date, so read from a finer rule it counts only once
        // its day has ended by T (an intraday label is already the close).
        let rule_res = self.prog.relations.get(&self.prog.rules[cr.idx].head.name).and_then(|s| s.res);
        let t = match (info.res, rule_res) {
            (Resolution::D1, Some(r)) if r.finer_than(Resolution::D1) => at_t - time::DAY + 1,
            _ => at_t,
        };
        let mut pat = self.atom_pattern(cr, atom, env)?;
        pat[info.key_pos] = None;
        let tuples = if info.stored {
            let matches = |tu: &Tuple| pat.iter().zip(tu.iter()).all(|(p, v)| p.as_ref().map(|p| p == v).unwrap_or(true));
            let mut found: Vec<(Tuple, usize)> = Vec::new();
            let entity = info.entity_positions.first().and_then(|&e| pat[e].clone());
            let mut cur = t;
            while let Some(k) = self.stores[rel].by_time.range(..=cur).next_back().map(|(k, _)| *k) {
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
            let mut walked: Vec<i64> = Vec::new();
            let mut found: Option<i64> = None;
            let mut cur = t;
            while let Some(k) = self.domains.get(&info.res).and_then(|d| d.range(..=cur).next_back().copied()) {
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
                cur = k - 1;
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
        if Ctor::parse(name) != Some(d.ctor) {
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

    fn domain(&self, cr: &CompiledRule) -> Result<&std::collections::BTreeSet<i64>, RunError> {
        let res = self.prog.infos[cr.idx].resolution;
        self.domains.get(&res).ok_or_else(|| RunError::Internal(format!("no time domain at {}", res)))
    }

    pub(crate) fn step(&mut self, cr: &Rc<CompiledRule>, lit: &Literal, env: &Env, out: &mut Vec<Env>) -> Result<(), RunError> {
        match lit {
            Literal::Atom(a) => out.extend(self.atom_matches(cr, a, env)?),
            Literal::AsOf { atom, at, .. } => out.extend(self.asof_matches(cr, atom, at, env)?),
            Literal::Neg(a) => {
                if self.atom_matches(cr, a, env)?.is_empty() {
                    out.push(env.clone());
                }
            }
            Literal::Builtin(b, _) => match b {
                Builtin::Prev { t, t1 } => {
                    let t = self.time_of(cr, t, env)?;
                    if let Some(&p) = self.domain(cr)?.range(..t).next_back() {
                        self.bind_time(cr, t1, env, p, out);
                    }
                }
                Builtin::Lag { t, n, t1 } => {
                    let t = self.time_of(cr, t, env)?;
                    let Value::Dur(d) = self.eval_expr(cr, n, env)? else {
                        return Err(RunError::Internal("lag length is not a Duration".into()));
                    };
                    let bound = time::sub_duration(t, *d);
                    if let Some(&p) = self.domain(cr)?.range(..=bound).next_back() {
                        self.bind_time(cr, t1, env, p, out);
                    }
                }
                Builtin::MonthStart { t } => {
                    let t = self.time_of(cr, t, env)?;
                    let prev = self.domain(cr)?.range(..t).next_back().copied();
                    if prev.map(|p| time::month_key(p) != time::month_key(t)).unwrap_or(true) {
                        out.push(env.clone());
                    }
                }
                Builtin::DayStart { t } => {
                    let t = self.time_of(cr, t, env)?;
                    let prev = self.domain(cr)?.range(..t).next_back().copied();
                    if prev.map(|p| time::day_key(p) != time::day_key(t)).unwrap_or(true) {
                        out.push(env.clone());
                    }
                }
            },
            Literal::Window { kind: WindowKind::Rows, .. } => {
                return Err(RunError::Internal("a `rows` window is solved by its aggregation".into()));
            }
            Literal::Window { var, kind, base, dur, .. } => {
                let t = self.time_of(cr, base, env)?;
                let Value::Dur(d) = self.eval_expr(cr, dur, env)? else {
                    return Err(RunError::Internal("window length is not a Duration".into()));
                };
                let lo = time::sub_duration(t, *d);
                let times: Vec<i64> = match kind {
                    WindowKind::Window => self.domain(cr)?.range(lo..=t).copied().collect(),
                    WindowKind::Prior | WindowKind::Rows => self.domain(cr)?.range(lo..t).copied().collect(),
                };
                for t1 in times {
                    self.bind_time(cr, var, env, t1, out);
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
            Literal::Agg { var, agg, args, conj, .. } => {
                let mut min_count: i64 = 0;
                for l in conj {
                    if let Literal::Window { min, .. } = l {
                        let k = self.eval_expr(cr, min, env)?.as_f64().unwrap_or(0.0) as i64;
                        min_count = min_count.max(k);
                    }
                }
                self.stats.window_calls += 1;
                // The rows of the group with the aggregate's arguments evaluated
                // on each: a `rows` window walks back over the group's own
                // bars; otherwise from the per-bar cache when the group has
                // exactly one window (the common case), else solved whole.
                let has_rows = conj.iter().any(|l| matches!(l, Literal::Window { kind: WindowKind::Rows, .. }));
                let cached = if has_rows {
                    Some(self.rows_window(cr, lit, conj, agg, args, env)?)
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
                                ..
                            } = l
                            {
                                let t = self.time_of(cr, base, env)?;
                                if let Value::Dur(d) = self.eval_expr(cr, dur, env)? {
                                    let lo = time::sub_duration(t, *d);
                                    self.stats.window_bars_solved += match kind {
                                        WindowKind::Window => self.domain(cr)?.range(lo..=t).count(),
                                        WindowKind::Prior | WindowKind::Rows => self.domain(cr)?.range(lo..t).count(),
                                    };
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
                rows.sort_by(|a, b| a.0.cmp(&b.0));
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
            Literal::Resample { inner, to, as_var, min, aggs, .. } => {
                let label = env[cr.slots[as_var]]
                    .as_ref()
                    .and_then(|v| v.as_time())
                    .ok_or_else(|| RunError::Internal("resample bucket label unbound".into()))?;
                let k = self.eval_expr(cr, min, env)?.as_f64().unwrap_or(0.0) as i64;
                let rel = self.rel_ids.get(&inner.name).copied().ok_or_else(|| RunError::Internal(format!("unknown relation `{}`", inner.name)))?;
                let info = self.rels[rel].clone();
                let (lo, hi) = time::bucket_range(*to, label);
                let fine: Vec<i64> = self.domains.get(&info.res).map(|d| d.range(lo..=hi).copied().collect()).unwrap_or_default();
                let mut pat = self.atom_pattern(cr, inner, env)?;
                // Grouping columns: entity positions whose term is a fresh variable.
                let group_pos: Vec<(usize, usize)> = info
                    .entity_positions
                    .iter()
                    .filter_map(|&i| match &inner.terms[i] {
                        Term::Var(v, _) if env[cr.slots[v]].is_none() => Some((i, cr.slots[v])),
                        _ => None,
                    })
                    .collect();
                let mut groups: BTreeMap<Vec<Value>, Vec<Tuple>> = BTreeMap::new();
                for t1 in fine {
                    pat[info.key_pos] = Some(Value::Time(t1));
                    let tuples = self.call(rel, &pat)?;
                    for (tu, _) in tuples.iter() {
                        let gk: Vec<Value> = group_pos.iter().map(|(i, _)| tu[*i].clone()).collect();
                        groups.entry(gk).or_default().push(tu.clone());
                    }
                }
                for (gk, tuples) in groups {
                    if (tuples.len() as i64) < k {
                        continue;
                    }
                    let mut e = env.clone();
                    for ((_, slot), v) in group_pos.iter().zip(gk.iter()) {
                        e[*slot] = Some(v.clone());
                    }
                    let mut ok = true;
                    for (x, agg, expr) in aggs {
                        if agg == "count" {
                            e[cr.slots[x]] = Some(Value::Count(tuples.len() as i64));
                            continue;
                        }
                        let mut col = Vec::with_capacity(tuples.len());
                        for tu in &tuples {
                            let mut local = e.clone();
                            for (term, val) in inner.terms.iter().zip(tu.iter()) {
                                if let Term::Var(v, _) = term {
                                    local[cr.slots[v]] = Some(val.clone());
                                }
                            }
                            let v = self.eval_expr(cr, expr, &local)?;
                            col.push(v.as_f64().ok_or_else(|| self.arith(cr, &local, expr, "resample argument is not numeric"))?);
                        }
                        match aggregate(agg, &[col]) {
                            Ok(Some(v)) => e[cr.slots[x]] = Some(v),
                            Ok(None) => ok = false,
                            Err(m) => return Err(self.arith_text(cr, &e, format!("{}({})", agg, expr), &m)),
                        }
                    }
                    if ok {
                        out.push(e);
                    }
                }
            }
        }
        Ok(())
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
            ..
        } = &conj[windows[0]]
        else {
            return Ok(None);
        };
        let wslot = cr.slots[wvar];
        if env[wslot].is_some() {
            return Ok(None);
        }
        let t = self.time_of(cr, base, env)?;
        let Value::Dur(d) = self.eval_expr(cr, dur, env)? else {
            return Err(RunError::Internal("window length is not a Duration".into()));
        };
        let lo = time::sub_duration(t, *d);
        let times: Vec<i64> = match kind {
            WindowKind::Window => self.domain(cr)?.range(lo..=t).copied().collect(),
            WindowKind::Prior => self.domain(cr)?.range(lo..t).copied().collect(),
            WindowKind::Rows => return Ok(None),
        };
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
        let literal = self.prog.rules[cr.idx].body.iter().position(|l| std::ptr::eq(l, lit)).unwrap_or(usize::MAX);
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
    fn rows_window(&mut self, cr: &Rc<CompiledRule>, lit: &Literal, conj: &[Literal], agg: &str, args: &[Expr], env: &Env) -> Result<WindowRows, RunError> {
        let windows: Vec<usize> = conj.iter().enumerate().filter(|(_, l)| matches!(l, Literal::Window { .. })).map(|(i, _)| i).collect();
        let Some(Literal::Window {
            var: Term::Var(wvar, _),
            base: base @ Term::Var(bvar, _),
            dur,
            ..
        }) = windows.first().map(|&i| &conj[i])
        else {
            return Err(RunError::Internal("a `rows` window needs a variable and a bound base time".into()));
        };
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
        let literal = self.prog.rules[cr.idx].body.iter().position(|l| std::ptr::eq(l, lit)).unwrap_or(usize::MAX);
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
        let mut upper = t;
        'walk: while found.len() < n {
            // The domain in chunks, latest first, so that solving can borrow the kernel.
            let chunk: Vec<i64> = self.domain(cr)?.range(..=upper).rev().take(64).copied().collect();
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
        for t1 in found.iter().rev() {
            for (row, vals) in &cache[t1] {
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
        Ok(rows)
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
                    "least" | "greatest" => {
                        let mut best = vals[0].clone();
                        for v in &vals[1..] {
                            let better = if f == "least" { v < &best } else { v > &best };
                            if better {
                                best = v.clone();
                            }
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
        },
        Literal::Window { var, base, dur, min, .. } => {
            term(var, out);
            term(base, out);
            expr(dur, out);
            expr(min, out);
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
        Literal::Resample { inner, as_var, min, aggs, .. } => {
            inner.terms.iter().for_each(|t| term(t, out));
            out.push(as_var.clone());
            expr(min, out);
            for (x, _, e) in aggs {
                out.push(x.clone());
                expr(e, out);
            }
        }
        Literal::AsOf { atom, at, .. } => {
            atom.terms.iter().for_each(|t| term(t, out));
            term(at, out);
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

fn mean(xs: &[f64]) -> f64 {
    xs.iter().sum::<f64>() / xs.len() as f64
}

fn sample_cov(xs: &[f64], ys: &[f64]) -> Result<f64, String> {
    if xs.len() < 2 {
        return Err("covariance of fewer than two observations".into());
    }
    let (mx, my) = (mean(xs), mean(ys));
    Ok(xs.iter().zip(ys).map(|(x, y)| (x - mx) * (y - my)).sum::<f64>() / (xs.len() as f64 - 1.0))
}

/// The closed aggregate set (section 1). `Ok(None)` is "no tuple": an empty
/// group for anything but `count`. Errors are partial arithmetic.
pub fn aggregate(name: &str, cols: &[Vec<f64>]) -> Result<Option<Value>, String> {
    let xs = &cols[0];
    if name == "count" {
        return Ok(Some(Value::Count(xs.len() as i64)));
    }
    if xs.is_empty() {
        return Ok(None);
    }
    let sorted = |v: &[f64]| -> Vec<f64> {
        let mut s = v.to_vec();
        s.sort_by(|a, b| a.total_cmp(b));
        s
    };
    let v = match name {
        "sum" => xs.iter().sum(),
        "mean" => mean(xs),
        "std" => {
            if xs.len() < 2 {
                return Err("std of fewer than two observations".into());
            }
            sample_cov(xs, xs)?.sqrt()
        }
        "median" => {
            let s = sorted(xs);
            let n = s.len();
            if n % 2 == 1 {
                s[n / 2]
            } else {
                (s[n / 2 - 1] + s[n / 2]) / 2.0
            }
        }
        "quantile" => {
            let q = cols.get(1).and_then(|c| c.first()).copied().ok_or("quantile needs a level")?;
            if !(0.0..=1.0).contains(&q) {
                return Err(format!("quantile level {} is outside [0, 1]", q));
            }
            let s = sorted(xs);
            let pos = q * (s.len() as f64 - 1.0);
            let lo = pos.floor() as usize;
            let hi = pos.ceil() as usize;
            s[lo] + (s[hi] - s[lo]) * (pos - lo as f64)
        }
        "max" => xs.iter().cloned().fold(f64::NEG_INFINITY, f64::max),
        "min" => xs.iter().cloned().fold(f64::INFINITY, f64::min),
        "first" => xs[0],
        "last" => xs[xs.len() - 1],
        "corr" => {
            let ys = &cols[1];
            let c = sample_cov(xs, ys)?;
            let (sx, sy) = (sample_cov(xs, xs)?.sqrt(), sample_cov(ys, ys)?.sqrt());
            if sx == 0.0 || sy == 0.0 {
                return Err("correlation of a constant series".into());
            }
            c / (sx * sy)
        }
        "cov" => sample_cov(xs, &cols[1])?,
        "ols_beta" => {
            let ys = xs;
            let x = &cols[1];
            let vx = sample_cov(x, x)?;
            if vx == 0.0 {
                return Err("ols_beta against a constant regressor".into());
            }
            sample_cov(ys, x)? / vx
        }
        _ => return Err(format!("unknown aggregate `{}`", name)),
    };
    Ok(Some(Value::Num(v)))
}
