//! Preparing a run for its `?` queries, and answering every form over the
//! kernel at the end of the run (or at the last barrier).

use super::parse::{Ask, Query, Show};
use crate::check::{Program, Severity, Workspace};
use crate::ir::{Cal, Term};
use crate::kernel::{DataClient, Kernel, RunError, RunResult, Value};
use crate::plan::Plan;
use crate::state::tables::{Cell, ColType, Table};
use crate::syntax::ast;

/// What an answer reads besides the kernel: the program the run held (the
/// prepared one), the run, the plan it ran under, and the data service and
/// run id `ledger ... into` writes to.
pub struct Context<'a> {
    pub prog: &'a Program,
    pub result: &'a RunResult,
    pub prepared: &'a Prepared,
    pub plan: Option<&'a Plan>,
    pub out: Option<(&'a DataClient, &'a str)>,
}

/// The prefix of the relation a `?` query is compiled into.
pub const PREFIX: &str = "__query_";

/// A query answered: the table of a form that answers one, and text.
#[derive(Clone, Debug)]
pub struct Answer {
    pub table: Option<Table>,
    pub text: String,
}

impl Answer {
    fn table(t: Table) -> Answer {
        Answer { text: t.text(), table: Some(t) }
    }
    fn text(text: String) -> Answer {
        Answer { table: None, text }
    }
}

/// The program a run holds for its queries: P with, for each `?` query, a
/// relation and a rule `__query_<i>(T) :- BODY.` in a stratum above P's,
/// observed so that the plan keeps it (Definition V-8.1: the body is
/// checked with P as any rule is; by Proposition V-8.4 the run decides as
/// P's). Per query, its rule and the body's variables in written order.
pub struct Prepared {
    pub program: Program,
    pub rules: Vec<Option<usize>>,
    pub vars: Vec<Vec<String>>,
    /// A query whose body is one atom: its relation and the variable at
    /// each argument (what its rows are the relation's tuples of, with the
    /// rule that derived each).
    pub atoms: Vec<Option<(String, Vec<Option<String>>)>>,
    /// A one-atom query answered from the kernel's own extension of the
    /// relation, without a rule of its own (`Prepared::direct`).
    pub direct: Vec<Option<ast::Atom>>,
}

impl Prepared {
    /// The queries answered over a kernel whose program does not hold them
    /// as rules (a run already running or ended): a `?` must be one atom
    /// of a relation of the program, which is answered from the relation's
    /// extension, its constants and its `for` binding applied.
    pub fn direct(prog: &Program, queries: &[Query]) -> Result<Prepared, String> {
        let mut out = Prepared {
            program: prog.clone(),
            rules: vec![None; queries.len()],
            vars: vec![Vec::new(); queries.len()],
            atoms: vec![None; queries.len()],
            direct: vec![None; queries.len()],
        };
        for (i, q) in queries.iter().enumerate() {
            let Query::Ask(a) = q else { continue };
            let body = crate::syntax::parse_body(&a.body).map_err(|e| format!("`? {}`: {}", a.body, e.message))?;
            let [ast::Literal::Atom(atom)] = body.as_slice() else {
                return Err(format!("`? {}` is more than one atom: the run must hold it as a rule (Definition V-8.1)", a.body));
            };
            let Some(sig) = prog.relations.get(&atom.name).filter(|_| atom.brackets.is_empty() && atom.cal.is_none()) else {
                return Err(format!("`{}` is not a relation of {}", atom.name, prog.strategy));
            };
            if atom.terms.len() != sig.args.len() {
                return Err(format!("`{}` takes {} arguments, {} given", atom.name, sig.args.len(), atom.terms.len()));
            }
            out.vars[i] = body_vars(&body);
            out.atoms[i] = Some((
                atom.name.clone(),
                atom.terms
                    .iter()
                    .map(|t| match t {
                        Term::Var(v, _) if v != "_" => Some(v.clone()),
                        _ => None,
                    })
                    .collect(),
            ));
            out.direct[i] = Some(atom.clone());
        }
        Ok(out)
    }
}

/// `show schedule`: the program graph's partition into the open loop,
/// evaluated ahead of the executor, and the closed loop, bar by bar after
/// the previous bar's fills (F-11.5).
pub fn schedule(prog: &Program) -> String {
    let g = crate::observe::program_graph(prog);
    let (closed, open): (Vec<_>, Vec<_>) = g.nodes.iter().filter(|n| n.kind == "derived").partition(|n| n.closed_loop);
    format!(
        "open loop (evaluated ahead of the executor, per entity): {}\nclosed loop (bar by bar, after the previous bar's fills): {}\n",
        open.iter().map(|n| n.name.as_str()).collect::<Vec<_>>().join(", "),
        closed.iter().map(|n| n.name.as_str()).collect::<Vec<_>>().join(", ")
    )
}

fn term_vars(t: &Term, out: &mut Vec<String>) {
    match t {
        Term::Var(v, _) if v != "_" && !out.contains(v) => out.push(v.clone()),
        Term::Ctor(_, args, _) => args.iter().for_each(|a| term_vars(a, out)),
        _ => {}
    }
}

/// The variables of a body in the order they first appear in its atoms,
/// its temporal builtins and its assignments.
fn body_vars(body: &[ast::Literal]) -> Vec<String> {
    let mut out = Vec::new();
    for l in body {
        match l {
            ast::Literal::Atom(a) | ast::Literal::Neg(a) => a.terms.iter().for_each(|t| term_vars(t, &mut out)),
            ast::Literal::Prev { t, t1, .. } | ast::Literal::Lag { t, t1, .. } => [t, t1].into_iter().for_each(|x| term_vars(x, &mut out)),
            ast::Literal::SessionOf { a, t, s, .. } => [a, t, s].into_iter().for_each(|x| term_vars(x, &mut out)),
            ast::Literal::MonthStart { t, .. } | ast::Literal::DayStart { t, .. } => term_vars(t, &mut out),
            ast::Literal::Window { var, .. } => term_vars(var, &mut out),
            ast::Literal::Assign { var, .. } if !out.contains(var) => out.push(var.clone()),
            _ => {}
        }
    }
    out
}

/// The query's bar variable and its calendar: the variable `at` names, else
/// the key of the first atom of a relation of P; the calendar is that
/// atom's (the decision calendar when no atom of P keys on it).
fn bar_of(prog: &Program, body: &[ast::Literal], named: Option<&str>) -> Result<(String, Cal), String> {
    for l in body {
        let ast::Literal::Atom(a) = l else { continue };
        let Some(sig) = prog.relations.get(&a.name).filter(|_| a.brackets.is_empty()) else { continue };
        let Some(key) = sig.key_pos() else { continue };
        let Some(Term::Var(v, _)) = a.terms.get(key) else { continue };
        if named.is_some_and(|n| n != v) {
            continue;
        }
        let cal = match &a.cal {
            Some((name, _)) => prog.calendars.get(name).cloned().unwrap_or_else(|| sig.cal.clone()),
            None => sig.cal.clone(),
        };
        return Ok((v.clone(), cal));
    }
    match named {
        Some(n) => Ok((n.to_string(), prog.decision_cal.clone())),
        None => Err("the query's bar: name it (`at T = ...`), or key an atom of the program on a variable".into()),
    }
}

/// Prepares the program for the queries (the `?` ones are compiled into it).
pub fn prepare(ws: &Workspace, strategy: &str, base: &Program, queries: &[Query]) -> Result<Prepared, String> {
    let mut vars = vec![Vec::new(); queries.len()];
    let mut atoms = vec![None; queries.len()];
    let asks: Vec<(usize, &Ask)> = queries
        .iter()
        .enumerate()
        .filter_map(|(i, q)| match q {
            Query::Ask(a) => Some((i, a)),
            _ => None,
        })
        .collect();
    if asks.is_empty() {
        return Ok(Prepared {
            program: base.clone(),
            rules: vec![None; queries.len()],
            vars,
            atoms,
            direct: vec![None; queries.len()],
        });
    }
    let mut src = String::from("strategy __queries {\n  rules {\n");
    for (i, a) in &asks {
        let body = crate::syntax::parse_body(&a.body).map_err(|e| format!("`? {}`: {}", a.body, e.message))?;
        vars[*i] = body_vars(&body);
        if let [ast::Literal::Atom(a)] = body.as_slice() {
            let at = a
                .terms
                .iter()
                .map(|t| match t {
                    Term::Var(v, _) if v != "_" => Some(v.clone()),
                    _ => None,
                })
                .collect();
            atoms[*i] = Some((a.name.clone(), at));
        }
        let (t, cal) = bar_of(base, &body, a.time_var.as_deref()).map_err(|m| format!("`? {}`: {}", a.body, m))?;
        let ann = if cal == base.decision_cal { String::new() } else { format!(": {}", base.cal_name(&cal)) };
        src.push_str(&format!(
            "    rel {p}{i}(@{t}{ann})\n    {p}{i}({t}) :- {body}.\n    observe {p}{i}\n",
            p = PREFIX,
            i = i,
            t = t,
            ann = ann,
            body = a.body
        ));
    }
    src.push_str("  }\n}\n");
    let extra = match crate::syntax::parse_units(&src).map_err(|e| e.message)?.into_iter().next() {
        Some(ast::Unit::Strategy(s)) => s.rules.unwrap_or_else(|| unreachable!("the queries' rules block")),
        _ => unreachable!("the queries' unit"),
    };
    let mut ws = ws.clone();
    let target = ws
        .units
        .iter_mut()
        .find_map(|u| match u {
            ast::Unit::Strategy(s) if s.name == strategy => Some(s),
            _ => None,
        })
        .ok_or_else(|| format!("strategy `{}` is not in the workspace", strategy))?;
    let block = target.rules.get_or_insert_with(|| ast::RulesBlock {
        span: Default::default(),
        rels: vec![],
        rules: vec![],
        observes: vec![],
    });
    block.rels.extend(extra.rels);
    block.rules.extend(extra.rules);
    block.observes.extend(extra.observes);
    let (prog, diags) = crate::check::check_program(&ws, strategy);
    let errors: Vec<String> = diags.iter().filter(|d| d.severity == Severity::Error).map(|d| d.to_string()).collect();
    let Some(program) = prog.filter(|_| errors.is_empty()) else {
        return Err(format!("the query does not check as a rule of `{}`:\n{}", strategy, errors.join("\n")));
    };
    let rules = (0..queries.len()).map(|i| program.rules.iter().position(|r| r.head.name == format!("{}{}", PREFIX, i))).collect();
    let direct = vec![None; queries.len()];
    Ok(Prepared { program, rules, vars, atoms, direct })
}

/// The type of a column from its values (text when they disagree).
fn column_type(values: &[Option<&Value>]) -> ColType {
    let mut ty: Option<ColType> = None;
    for v in values.iter().flatten() {
        let t = match v {
            Value::Time(_) => ColType::Time,
            Value::Num(_) => ColType::Float,
            Value::Count(_) => ColType::Int,
            _ => ColType::Str,
        };
        match ty {
            None => ty = Some(t),
            Some(x) if x == t => {}
            _ => return ColType::Str,
        }
    }
    ty.unwrap_or(ColType::Str)
}

/// The bars a `?` asks at, of a calendar's domain within the run.
fn bars_of(k: &mut Kernel, cal: &Cal, a: &Ask, first: i64, last: i64) -> Result<Vec<i64>, RunError> {
    let domain = k.bars_at(cal);
    Ok(match (a.at, a.over, a.every_bar) {
        (Some(t), _, _) => {
            k.check_bar(cal, t)?;
            vec![t]
        }
        (None, Some((t0, t1)), _) => domain.into_iter().filter(|&b| b >= t0 && b <= t1 && b >= first && b <= last).collect(),
        (None, None, true) => domain.into_iter().filter(|&b| b >= first && b <= last).collect(),
        (None, None, false) => domain.into_iter().rfind(|&b| b <= last).into_iter().collect(),
    })
}

/// A one-atom `?` over the kernel's extension of its relation: its
/// constants and its `for` binding narrow the call, a variable twice in it
/// must agree, and a derived relation's rows name their rule.
fn ask_direct(k: &mut Kernel, cx: &Context, i: usize, a: &Ask, atom: &ast::Atom) -> Result<Answer, RunError> {
    let prog = cx.prog;
    let (Some(&first), Some(&last)) = (cx.result.bars.first(), cx.result.bars.last()) else {
        return Err(RunError::NoBars);
    };
    let rel = k.relation_id(&atom.name).ok_or_else(|| RunError::Request(format!("`{}` is not a relation the run holds", atom.name)))?;
    let sig = &prog.relations[&atom.name];
    let key = k.rels[rel].key_pos;
    let bars = bars_of(k, &sig.cal, a, first, last)?;
    let bound = match &a.bind {
        Some((v, key)) => Some((v.clone(), k.parse_binding(key).map_err(|m| RunError::Request(format!("for {} = {}: {}", v, key, m)))?)),
        None => None,
    };
    let mut pattern: Vec<Option<Value>> = vec![None; sig.args.len()];
    for (j, t) in atom.terms.iter().enumerate() {
        match t {
            Term::Lit(l, _) => pattern[j] = Some(crate::kernel::lit_value(l, &mut k.symbols, &mut k.labels, &k.literal_equities)),
            Term::Var(v, _) => {
                if let Some((bv, x)) = &bound {
                    if bv == v {
                        pattern[j] = Some(x.clone());
                    }
                }
            }
            _ => {}
        }
    }
    if bound.as_ref().is_some_and(|(v, _)| !atom.terms.iter().any(|t| matches!(t, Term::Var(x, _) if x == v))) {
        return Err(RunError::Request(format!(
            "`for {} = ...`: `{}` is not a variable of the query",
            bound.as_ref().map(|b| b.0.as_str()).unwrap_or(""),
            bound.as_ref().map(|b| b.0.as_str()).unwrap_or("")
        )));
    }
    let vars = &cx.prepared.vars[i];
    let derived = !prog.rules_for(&atom.name).is_empty();
    let mut rows: Vec<(Vec<Option<Value>>, Option<String>)> = Vec::new();
    for t in bars {
        pattern[key] = Some(Value::Time(t));
        let found = if k.rels[rel].inputs.is_empty() { k.call(rel, &pattern)?.as_ref().clone() } else { Vec::new() };
        let mut at: Vec<(Vec<Option<Value>>, Option<String>)> = Vec::new();
        'tuple: for (tu, ri) in found {
            let mut row: Vec<Option<Value>> = vec![None; vars.len()];
            for (j, term) in atom.terms.iter().enumerate() {
                if let Term::Var(v, _) = term {
                    if let Some(c) = vars.iter().position(|n| n == v) {
                        match &row[c] {
                            Some(x) if *x != tu[j] => continue 'tuple,
                            _ => row[c] = Some(tu[j].clone()),
                        }
                    }
                }
            }
            at.push((row, derived.then(|| prog.rule_label(ri))));
        }
        at.sort_by(|x, y| x.0.cmp(&y.0));
        at.dedup_by(|x, y| x.0 == y.0);
        rows.extend(at);
    }
    let types: Vec<ColType> = (0..vars.len()).map(|j| column_type(&rows.iter().map(|r| r.0[j].as_ref()).collect::<Vec<_>>())).collect();
    let mut cols: Vec<(&str, ColType)> = vars.iter().map(|v| v.as_str()).zip(types.iter().copied()).collect();
    if derived {
        cols.push(("rule", ColType::Str));
    }
    let mut table = Table::new("?", &cols);
    for (r, rule) in &rows {
        let mut cells: Vec<Cell> = r
            .iter()
            .zip(&types)
            .map(|(v, ty)| v.as_ref().map(|v| crate::state::tables::cell_of(k, v, *ty)).unwrap_or(Cell::Null))
            .collect();
        if derived {
            cells.push(rule.clone().map(Cell::Str).unwrap_or(Cell::Null));
        }
        table.push(cells);
    }
    Ok(Answer::table(table))
}

/// Answers query `i` of the prepared run, over the kernel at the end of it.
pub fn answer(k: &mut Kernel, cx: &Context, i: usize, q: &Query) -> Result<Answer, RunError> {
    let (prog, result, prepared) = (cx.prog, cx.result, cx.prepared);
    let (Some(&first), Some(&last)) = (result.bars.first(), result.bars.last()) else {
        return Err(RunError::NoBars);
    };
    match q {
        Query::Ask(a) => {
            if let Some(atom) = &prepared.direct[i] {
                return ask_direct(k, cx, i, a, atom);
            }
            let ri = prepared.rules[i].ok_or_else(|| RunError::Internal("an unprepared query".into()))?;
            let cal = prog.infos[ri].cal.clone();
            let domain = k.bars_at(&cal);
            let bars: Vec<i64> = match (a.at, a.over, a.every_bar) {
                (Some(t), _, _) => {
                    k.check_bar(&cal, t)?;
                    vec![t]
                }
                (None, Some((t0, t1)), _) => domain.into_iter().filter(|&b| b >= t0 && b <= t1 && b >= first && b <= last).collect(),
                (None, None, true) => domain.into_iter().filter(|&b| b >= first && b <= last).collect(),
                (None, None, false) => domain.into_iter().rfind(|&b| b <= last).into_iter().collect(),
            };
            let bindings = match &a.bind {
                Some((v, key)) => vec![(v.clone(), k.parse_binding(key).map_err(|m| RunError::Request(format!("for {} = {}: {}", v, key, m)))?)],
                None => Vec::new(),
            };
            let vars = &prepared.vars[i];
            // One atom of a derived relation: each row's rule is the one that
            // derived the tuple (V-8: `? decide(T, D) at T`, the rule of each).
            let derived = prepared.atoms[i].as_ref().filter(|(r, _)| !prog.rules_for(r).is_empty());
            let mut rows: Vec<(Vec<Option<Value>>, Option<String>)> = Vec::new();
            let mut stuck: Vec<String> = Vec::new();
            for t in bars {
                let (sols, halts) = k.solutions(ri, t, &bindings)?;
                let mut at: Vec<Vec<Option<Value>>> = sols
                    .into_iter()
                    .map(|sol| vars.iter().map(|v| sol.iter().find(|(n, _)| n == v).map(|(_, x)| x.clone())).collect())
                    .collect();
                at.sort();
                at.dedup();
                stuck.extend(halts.into_iter().map(|e| e.to_string()));
                let tuples = match derived.and_then(|(r, _)| k.relation_id(r)) {
                    Some(rel) => {
                        let width = prog.relations[&derived.expect("an atom").0].args.len();
                        let mut pattern = vec![None; width];
                        pattern[k.rels[rel].key_pos] = Some(Value::Time(t));
                        k.call(rel, &pattern).map(|d| d.as_ref().clone()).unwrap_or_default()
                    }
                    None => Vec::new(),
                };
                for row in at {
                    let rule = derived.and_then(|(_, terms)| {
                        tuples
                            .iter()
                            .find(|(tu, _)| {
                                terms
                                    .iter()
                                    .zip(tu.iter())
                                    .all(|(v, x)| v.as_ref().is_none_or(|v| vars.iter().position(|n| n == v).and_then(|j| row[j].as_ref()) == Some(x)))
                            })
                            .map(|(_, r)| prog.rule_label(*r))
                    });
                    rows.push((row, rule));
                }
            }
            let types: Vec<ColType> = (0..vars.len()).map(|j| column_type(&rows.iter().map(|r| r.0[j].as_ref()).collect::<Vec<_>>())).collect();
            let mut cols: Vec<(&str, ColType)> = vars.iter().map(|v| v.as_str()).zip(types.iter().copied()).collect();
            if derived.is_some() {
                cols.push(("rule", ColType::Str));
            }
            let mut table = Table::new("?", &cols);
            for (r, rule) in &rows {
                let mut cells: Vec<Cell> = r
                    .iter()
                    .zip(&types)
                    .map(|(v, ty)| v.as_ref().map(|v| crate::state::tables::cell_of(k, v, *ty)).unwrap_or(Cell::Null))
                    .collect();
                if derived.is_some() {
                    cells.push(rule.clone().map(Cell::Str).unwrap_or(Cell::Null));
                }
                table.push(cells);
            }
            if !stuck.is_empty() {
                table.notes.push(format!("{} instance(s) stuck on a partial operation, the first: {}", stuck.len(), stuck[0]));
            }
            Ok(Answer::table(table))
        }
        Query::Why { relation, args } => {
            let sig = prog
                .relations
                .get(relation)
                .ok_or_else(|| RunError::Request(format!("`{}` is not a relation of {}", relation, prog.strategy)))?;
            let key = sig.key_pos().ok_or_else(|| RunError::Request(format!("`{}` has no time key", relation)))?;
            let bar = args
                .get(key)
                .ok_or_else(|| RunError::Request(format!("`why {}(...)` takes its {} arguments", relation, sig.args.len())))?;
            let id = format!("{}({})@{}", relation, args.join(", "), bar);
            let d = crate::state::provenance::derive(k, prog, &id, 8)?;
            Ok(Answer::text(d.text()))
        }
        Query::WhyNot { relation, args, written } => {
            let sig = prog
                .relations
                .get(relation)
                .ok_or_else(|| RunError::Request(format!("`{}` is not a relation of {}", relation, prog.strategy)))?;
            let key = sig.key_pos().ok_or_else(|| RunError::Request(format!("`{}` has no time key", relation)))?;
            let bar = args
                .get(key)
                .and_then(|a| crate::kernel::time::parse_timestamp(a))
                .ok_or_else(|| RunError::Request(format!("`why not {}(...)`: its argument {} is the bar, a time", relation, key + 1)))?;
            let symbol = match sig.args.iter().position(|a| a.ty.is_entity()).and_then(|p| args.get(p)) {
                Some(s) => Some(k.parse_binding(s).map_err(RunError::Request)?),
                None => None,
            };
            k.set_written_order(*written);
            let w = crate::state::provenance::why_not(k, prog, relation, bar, None, symbol);
            k.set_written_order(false);
            let w = w?;
            Ok(Answer::text(format!("{}{}", w.text(), if w.absence_driven() { "verdict: absence-driven\n" } else { "" })))
        }
        Query::Trace { relation, at } => {
            // Every instance of the relation's rules attempted at the bar:
            // fired, with its solutions and first solution's bindings, or
            // excluded, with the literal and the bindings entering it.
            let mut t = Table::new(
                "trace",
                &[
                    ("rule", ColType::Str),
                    ("identity", ColType::Str),
                    ("bar", ColType::Time),
                    ("fired", ColType::Bool),
                    ("solutions", ColType::Int),
                    ("literal", ColType::Int),
                    ("literal_text", ColType::Str),
                    ("bindings", ColType::Str),
                    ("absence_driven", ColType::Bool),
                ],
            );
            for f in crate::state::provenance::failures(k, prog, relation, *at, None, None)? {
                let (li, text) = match (&f.literal, &f.error) {
                    (Some((i, text)), _) => (Cell::Int(*i as i64), Cell::Str(text.clone())),
                    (None, Some(e)) => (Cell::Null, Cell::Str(format!("halted: {}", e))),
                    (None, None) => (Cell::Null, Cell::Null),
                };
                t.push(vec![
                    Cell::Str(f.rule.clone()),
                    Cell::Str(f.identity.clone()),
                    Cell::Time(*at),
                    Cell::Bool(f.fired),
                    Cell::Int(f.solutions as i64),
                    li,
                    text,
                    Cell::Str(f.bindings.iter().map(|(v, x)| format!("{} = {}", v, x)).collect::<Vec<_>>().join(", ")),
                    Cell::Bool(f.absence_driven),
                ]);
            }
            Ok(Answer::table(t))
        }
        Query::Coverage { over } => {
            let (t0, t1) = over.unwrap_or((first, last));
            let graph = crate::observe::program_graph(prog);
            let mut cov = Vec::new();
            for node in &graph.nodes {
                let Some(sig) = prog.relations.get(&node.name) else { continue };
                let derived = !prog.rules_for(&node.name).is_empty();
                let bars: Vec<i64> = k.bars_at(&sig.cal).into_iter().filter(|&b| b >= t0 && b <= t1 && b >= first && b <= last).collect();
                let (mut nonempty, mut tuples) = (0, 0);
                for &t in &bars {
                    let n = match k.query(&node.name, t, &[]) {
                        Ok(v) => v.len(),
                        Err(_) => 0,
                    };
                    nonempty += usize::from(n > 0);
                    tuples += n;
                }
                cov.push(crate::kernel::RelationCoverage {
                    name: node.name.clone(),
                    derived,
                    calls: bars.len(),
                    nonempty,
                    tuples,
                });
            }
            let (text, causes) = crate::observe::coverage_report(prog, &cov, result.bars.iter().filter(|&&b| b >= t0 && b <= t1).count());
            let mut text = text;
            for c in &causes {
                let cal = prog.cal_of(&c.relation);
                if let Some(t) = k.bars_at(&cal).into_iter().find(|&b| b >= t0.max(first) && b <= t1.min(last)) {
                    if let Ok(w) = crate::state::provenance::why_not(k, prog, &c.relation, t, None, None) {
                        text.push_str(&w.text());
                    }
                }
            }
            let mut table = Table::new(
                "coverage",
                &[
                    ("relation", ColType::Str),
                    ("derived", ColType::Bool),
                    ("bars", ColType::Int),
                    ("bars_with_tuples", ColType::Int),
                    ("tuples", ColType::Int),
                    ("empty", ColType::Bool),
                ],
            );
            for c in &cov {
                table.push(vec![
                    Cell::Str(c.name.clone()),
                    Cell::Bool(c.derived),
                    Cell::Int(c.calls as i64),
                    Cell::Int(c.nonempty as i64),
                    Cell::Int(c.tuples as i64),
                    Cell::Bool(c.calls > 0 && c.tuples == 0),
                ]);
            }
            Ok(Answer { table: Some(table), text })
        }
        Query::Show(what) => Ok(match what {
            Show::Program => Answer::text(crate::observe::show_program(prog)),
            Show::Plan(rel) => {
                let compiled;
                let plan = match cx.plan {
                    Some(p) => p,
                    None => {
                        compiled = crate::compiler::compile(prog, &crate::compiler::CompileOptions::default());
                        &compiled.plan
                    }
                };
                let plan = plan.clone().with_observed(&result.literal_counts);
                Answer::text(match rel {
                    Some(r) if !prog.relations.contains_key(r) => return Err(RunError::Request(format!("`{}` is not a relation of {}", r, prog.strategy))),
                    Some(r) => plan.only(prog, r).text(),
                    None => plan.text(),
                })
            }
            Show::Schedule => Answer::text(schedule(prog)),
            Show::Calendars => {
                let mut t = Table::new("calendars", &[("calendar", ColType::Str), ("bars", ColType::Int), ("first", ColType::Time), ("last", ColType::Time)]);
                for cal in k.calendars() {
                    let bars: Vec<i64> = k.bars_at(&cal).into_iter().filter(|&b| b >= first && b <= last).collect();
                    t.push(vec![
                        Cell::Str(prog.cal_name(&cal)),
                        Cell::Int(bars.len() as i64),
                        bars.first().map(|&b| Cell::Time(b)).unwrap_or(Cell::Null),
                        bars.last().map(|&b| Cell::Time(b)).unwrap_or(Cell::Null),
                    ]);
                }
                Answer::table(t)
            }
            Show::Contract => Answer::text(result.contract.table()),
        }),
        Query::Ledger { relation, into } => {
            let (client, run) = cx
                .out
                .ok_or_else(|| RunError::Request("`ledger R into NAME` writes through a data service: run over one (--data SOCKET)".into()))?;
            let n = crate::kernel::outputs::ledger_into(run, prog, k, client, result, relation, into)?;
            Ok(Answer::text(format!("{} rows of {} to kernel.{}.{}\n", n, relation, run, into)))
        }
    }
}
