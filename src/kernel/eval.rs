//! Rule evaluation: a body is solved literal by literal in the order written
//! (section 4), each literal mapping a set of partial bindings to a larger
//! or smaller set. Derived relations are requested through `call`, which
//! memoises by temporal key and inputs.

use std::collections::{BTreeMap, HashSet};
use std::rc::Rc;

use super::time;
use super::value::{Ctor, Decision, Value};
use super::{CompiledRule, Env, Kernel, RunError, Tuple};
use crate::ir::*;

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
            let tuples: Vec<(Tuple, usize)> = self.stores[rel]
                .by_time
                .get(&key)
                .map(|tus| tus.iter().filter(|tu| matches(tu)).map(|tu| (tu.clone(), usize::MAX)).collect())
                .unwrap_or_default();
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
        let mut seen: HashSet<Tuple> = HashSet::new();
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
            Term::Lit(l, _) => Ok(super::lit_value(l, &mut self.symbols)),
            Term::Ctor(c, subs, _) => {
                let ctor = Ctor::parse(c).ok_or_else(|| RunError::Internal(format!("unknown constructor `{}`", c)))?;
                let e = self.term_value(cr, &subs[0], env)?;
                let a = self.term_value(cr, &subs[1], env)?;
                match (e, a.as_f64()) {
                    (Value::Equity(s), Some(x)) => Ok(Value::Decision(Decision { ctor, equity: s, amount: x })),
                    _ => Err(RunError::Internal(format!("ill-typed decision in rule {}", cr.label))),
                }
            }
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
                Term::Lit(l, _) => Some(super::lit_value(l, &mut self.symbols)),
            });
        }
        Ok(pat)
    }

    /// Solutions of a positive atom from one binding.
    fn atom_matches(&mut self, cr: &CompiledRule, atom: &Atom, env: &Env) -> Result<Vec<Env>, RunError> {
        let rel = self.rel_ids.get(&atom.name).copied().ok_or_else(|| RunError::Internal(format!("unknown relation `{}`", atom.name)))?;
        let pat = self.atom_pattern(cr, atom, env)?;
        let tuples = self.call(rel, &pat)?;
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

    fn bind_time(&self, cr: &CompiledRule, term: &Term, env: &Env, t: i64, out: &mut Vec<Env>) {
        if let Term::Var(v, _) = term {
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
    }

    fn domain(&self, cr: &CompiledRule) -> Result<&std::collections::BTreeSet<i64>, RunError> {
        let res = self.prog.infos[cr.idx].resolution;
        self.domains.get(&res).ok_or_else(|| RunError::Internal(format!("no time domain at {}", res)))
    }

    pub(crate) fn step(&mut self, cr: &Rc<CompiledRule>, lit: &Literal, env: &Env, out: &mut Vec<Env>) -> Result<(), RunError> {
        match lit {
            Literal::Atom(a) => out.extend(self.atom_matches(cr, a, env)?),
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
                    let bound = time::sub_duration(t, d);
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
            Literal::Window { var, kind, base, dur, .. } => {
                let t = self.time_of(cr, base, env)?;
                let Value::Dur(d) = self.eval_expr(cr, dur, env)? else {
                    return Err(RunError::Internal("window length is not a Duration".into()));
                };
                let lo = time::sub_duration(t, d);
                let times: Vec<i64> = match kind {
                    WindowKind::Window => self.domain(cr)?.range(lo..=t).copied().collect(),
                    WindowKind::Prior => self.domain(cr)?.range(lo..t).copied().collect(),
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
                let mut rows = self.solve(cr, conj, vec![env.clone()])?;
                if (rows.len() as i64) < min_count {
                    return Ok(());
                }
                rows.sort();
                if agg == "count" {
                    let mut e = env.clone();
                    e[cr.slots[var]] = Some(Value::Count(rows.len() as i64));
                    out.push(e);
                    return Ok(());
                }
                let mut cols: Vec<Vec<f64>> = vec![Vec::with_capacity(rows.len()); args.len()];
                for row in &rows {
                    for (j, a) in args.iter().enumerate() {
                        let v = self.eval_expr(cr, a, row)?;
                        cols[j].push(v.as_f64().ok_or_else(|| self.arith(cr, row, a, "aggregate argument is not numeric"))?);
                    }
                }
                let value = match aggregate(agg, &cols) {
                    Ok(Some(v)) => v,
                    Ok(None) => return Ok(()),
                    Err(m) => return Err(self.arith(cr, env, &args[0], &m)),
                };
                let mut e = env.clone();
                e[cr.slots[var]] = Some(value);
                out.push(e);
            }
            Literal::Top { n, atom, by, .. } => {
                let n = self.eval_expr(cr, n, env)?.as_f64().unwrap_or(0.0).max(0.0) as usize;
                let mut rows = self.atom_matches(cr, atom, env)?;
                let keys: Vec<(usize, Dir)> = by.as_ref().map(|b| b.iter().map(|(k, d, _)| (cr.slots[k], *d)).collect()).unwrap_or_default();
                rows.sort_by(|a, b| {
                    for (slot, dir) in &keys {
                        let o = a[*slot].cmp(&b[*slot]);
                        let o = if *dir == Dir::Desc { o.reverse() } else { o };
                        if o != std::cmp::Ordering::Equal {
                            return o;
                        }
                    }
                    a.cmp(b)
                });
                rows.truncate(n);
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
                            Err(m) => return Err(self.arith(cr, &e, expr, &m)),
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

    fn arith(&self, cr: &CompiledRule, env: &Env, expr: &Expr, message: &str) -> RunError {
        let mut names: Vec<(&String, &usize)> = cr.slots.iter().collect();
        names.sort_by_key(|(_, s)| **s);
        let bindings: Vec<String> = names.iter().filter_map(|(n, s)| env[**s].as_ref().map(|v| format!("{}={}", n, self.show(v)))).collect();
        RunError::Arithmetic {
            rule: cr.label.clone(),
            bindings: bindings.join(", "),
            expr: expr.to_string(),
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
            Value::Decision(d) => format!("{}({}, {})", d.ctor.name(), self.symbols.name(d.equity), d.amount),
        }
    }

    pub(crate) fn eval_expr(&mut self, cr: &CompiledRule, e: &Expr, env: &Env) -> Result<Value, RunError> {
        match e {
            Expr::Var(v, _) => env[cr.slots[v]].clone().ok_or_else(|| RunError::Internal(format!("variable `{}` unbound in rule {}", v, cr.label))),
            Expr::Param(p, _) => self.param_value(cr, p),
            Expr::Lit(l, _) => Ok(super::lit_value(l, &mut self.symbols)),
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
