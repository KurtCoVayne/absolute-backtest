//! Provenance (compiler design, section 7.3): one witness per derived
//! tuple, the first derivation in canonical evaluation order, found by
//! re-evaluating the tuple's rules at its bar with the head bound to the
//! tuple (deterministic by I2, so a witness needs no recording); the
//! derivation tree down to supplied tuples; and why-not: for an absent
//! tuple, the first excluding literal of each rule and the bindings at
//! that point, with the verdict *absence-driven* when that literal is a
//! supplied relation's atom with no tuple.

use std::collections::HashMap;

use crate::check::Program;
use crate::compiler::print::render_lit;
use crate::ir::{Kind, Literal, Mode, Term};
use crate::kernel::time::{format_timestamp, parse_timestamp};
use crate::kernel::{Kernel, RunError, Value};

/// A tuple's id: its text with the bar it holds at, `R(v1, ..., vn)@bar`.
pub fn tuple_id(k: &Kernel, rel: &str, tuple: &[Value], bar: i64) -> String {
    format!("{}({})@{}", rel, tuple.iter().map(|v| k.show(v)).collect::<Vec<_>>().join(", "), format_timestamp(bar))
}

/// A tuple id parsed: the relation, the bar, the argument texts.
pub fn parse_id(id: &str) -> Result<(String, i64, Vec<String>), String> {
    let (tuple, bar) = id.rsplit_once('@').ok_or_else(|| format!("`{}` is not a tuple id (`R(v1, ..., vn)@BAR`)", id))?;
    let bar = parse_timestamp(bar.trim()).ok_or_else(|| format!("`{}`: `{}` is not a timestamp", id, bar))?;
    let tuple = tuple.trim();
    let open = tuple.find('(').ok_or_else(|| format!("`{}`: no argument list", id))?;
    if !tuple.ends_with(')') {
        return Err(format!("`{}`: the argument list does not close", id));
    }
    let name = tuple[..open].trim().to_string();
    let inner = &tuple[open + 1..tuple.len() - 1];
    let mut args = Vec::new();
    let mut depth = 0usize;
    let mut cur = String::new();
    for c in inner.chars() {
        match c {
            '(' => {
                depth += 1;
                cur.push(c);
            }
            ')' => {
                depth = depth.saturating_sub(1);
                cur.push(c);
            }
            ',' if depth == 0 => {
                args.push(cur.trim().to_string());
                cur.clear();
            }
            _ => cur.push(c),
        }
    }
    if !cur.trim().is_empty() {
        args.push(cur.trim().to_string());
    }
    Ok((name, bar, args))
}

/// Whether a relation is supplied (a primitive, the executor's or the
/// kernel's) rather than derived by rules.
fn supplied(prog: &Program, rel: &str) -> Option<&'static str> {
    match prog.relations.get(rel).map(|s| &s.kind) {
        Some(Kind::Primitive { .. }) => Some("primitive"),
        Some(Kind::Executor) => Some("executor"),
        Some(Kind::KernelState) => Some("kernel"),
        _ => None,
    }
}

/// The head bindings that pin a rule to one tuple: every head variable
/// outside the key and the inputs (which `explain` binds itself), and the
/// entity inside a decision constructor.
fn head_bindings(prog: &Program, ri: usize, tuple: &[Value]) -> Vec<(String, Value)> {
    let rule = &prog.rules[ri];
    let sig = prog.relations.get(&rule.head.name);
    let key = sig.and_then(|s| s.key_pos());
    let inputs: Vec<usize> = sig
        .map(|s| s.args.iter().enumerate().filter(|(_, a)| false && a.mode == Mode::Key).map(|(i, _)| i).collect())
        .unwrap_or_default();
    let mut out = Vec::new();
    for (i, term) in rule.head.terms.iter().enumerate() {
        if Some(i) == key || inputs.contains(&i) {
            continue;
        }
        match (term, tuple.get(i)) {
            (Term::Var(v, _), Some(val)) => out.push((v.clone(), val.clone())),
            (Term::Ctor(_, args, _), Some(Value::Decision(d))) => {
                if let Some(Term::Var(a, _)) = args.first() {
                    out.push((a.clone(), Value::Equity(d.equity)));
                }
            }
            _ => {}
        }
    }
    out
}

/// The input values of a tuple, in signature order.
fn inputs_of(prog: &Program, rel: &str, tuple: &[Value]) -> Vec<Value> {
    prog.relations
        .get(rel)
        .map(|s| {
            s.args
                .iter()
                .enumerate()
                .filter(|(_, a)| false && a.mode == Mode::Key)
                .filter_map(|(i, _)| tuple.get(i).cloned())
                .collect()
        })
        .unwrap_or_default()
}

/// A derived tuple's witness: the rule and the body tuples (positive atoms
/// instantiated by the first solution).
#[derive(Clone, Debug)]
pub struct Witness {
    pub rule: String,
    pub rule_index: usize,
    pub body: Vec<String>,
    pub solution: Vec<(String, String)>,
}

/// The first rule, in written order, that derives the tuple at the bar,
/// with its witness; `None` when no rule does (a supplied tuple, or one
/// no rule re-derives).
pub fn witness(k: &mut Kernel, prog: &Program, rel: &str, bar: i64, tuple: &[Value]) -> Result<Option<Witness>, RunError> {
    let inputs = inputs_of(prog, rel, tuple);
    for ri in prog.rules_for(rel) {
        let bindings = head_bindings(prog, ri, tuple);
        match k.explain_with(ri, bar, &inputs, &bindings) {
            Ok(ex) if ex.solutions > 0 => {
                let body = body_ids(prog, ri, &ex.solution);
                return Ok(Some(Witness {
                    rule: ex.rule.clone(),
                    rule_index: ri,
                    body,
                    solution: ex.solution,
                }));
            }
            Ok(_) => {}
            Err(RunError::Arithmetic { .. }) | Err(RunError::Request(_)) => {}
            Err(e) => return Err(e),
        }
    }
    Ok(None)
}

/// The tuple ids of a rule's positive atoms under a solution.
fn body_ids(prog: &Program, ri: usize, solution: &[(String, String)]) -> Vec<String> {
    let rule = &prog.rules[ri];
    let value = |t: &Term| -> String {
        match t {
            Term::Var(v, _) => solution.iter().find(|(n, _)| n == v).map(|(_, x)| x.clone()).unwrap_or_else(|| "_".into()),
            Term::Wild(_) => "_".into(),
            Term::Lit(l, _) => render_lit(l),
            Term::Param(p, _) => prog.param(&rule.unit, p).map(|p| render_lit(&p.value)).unwrap_or_else(|| p.clone()),
            Term::Ctor(..) => t.to_string(),
        }
    };
    let atom_id = |a: &crate::ir::Atom| -> String {
        let bar = prog
            .relations
            .get(&a.name)
            .and_then(|s| s.key_pos())
            .and_then(|kp| a.terms.get(kp))
            .map(value)
            .unwrap_or_else(|| "_".into());
        format!("{}({})@{}", a.name, a.terms.iter().map(value).collect::<Vec<_>>().join(", "), bar)
    };
    let mut out = Vec::new();
    for l in &rule.body {
        match l {
            Literal::Atom(a) | Literal::Top { atom: a, .. } => out.push(atom_id(a)),
            Literal::AsOf { atom, .. } => out.push(format!("{} (asof)", atom_id(atom))),
            Literal::Neg(a) => out.push(format!("not {}", atom_id(a))),
            Literal::Agg { .. } | Literal::Builtin(..) | Literal::Window { .. } | Literal::Cmp { .. } | Literal::Assign { .. } | Literal::Case { .. } => {}
        }
    }
    out
}

/// A derivation tree: the tuple, the rule that derived it, its body
/// tuples' trees, down to supplied tuples or the depth limit.
#[derive(Clone, Debug)]
pub struct Derivation {
    pub id: String,
    pub relation: String,
    pub bar: i64,
    pub rule: Option<String>,
    pub body: Vec<Derivation>,
    pub note: Option<String>,
}

impl Derivation {
    pub fn text(&self) -> String {
        let mut out = String::new();
        self.render(&mut out, 0);
        out
    }
    fn render(&self, out: &mut String, depth: usize) {
        let indent = "  ".repeat(depth);
        out.push_str(&format!(
            "{}{}{}{}\n",
            indent,
            self.id,
            self.rule.as_ref().map(|r| format!("  [{}]", r)).unwrap_or_default(),
            self.note.as_ref().map(|n| format!("  ({})", n)).unwrap_or_default()
        ));
        for b in &self.body {
            b.render(out, depth + 1);
        }
    }
    pub fn json(&self) -> serde_json::Value {
        serde_json::json!({
            "tuple": self.id,
            "relation": self.relation,
            "bar": format_timestamp(self.bar),
            "rule": self.rule,
            "note": self.note,
            "body": self.body.iter().map(|b| b.json()).collect::<Vec<_>>(),
        })
    }
    /// The number of nodes.
    pub fn size(&self) -> usize {
        1 + self.body.iter().map(|b| b.size()).sum::<usize>()
    }
}

/// The derivation tree of a tuple id, to `depth` levels; a subtree beyond
/// the depth is a node with a note, and a tuple is derived once.
pub fn derive(k: &mut Kernel, prog: &Program, id: &str, depth: usize) -> Result<Derivation, RunError> {
    let mut memo: HashMap<String, Derivation> = HashMap::new();
    derive_in(k, prog, id, depth, &mut memo)
}

fn derive_in(k: &mut Kernel, prog: &Program, id: &str, depth: usize, memo: &mut HashMap<String, Derivation>) -> Result<Derivation, RunError> {
    if let Some(d) = memo.get(id) {
        return Ok(d.clone());
    }
    let (rel, bar, args) = parse_id(id).map_err(RunError::Request)?;
    let id = id.trim().to_string();
    let Some(sig) = prog.relations.get(&rel) else {
        return Err(RunError::Request(format!("`{}` is not a relation of {}", rel, prog.strategy)));
    };
    let leaf = |id: &str, note: String| Derivation {
        id: id.to_string(),
        relation: rel.clone(),
        bar,
        rule: None,
        body: Vec::new(),
        note: Some(note),
    };
    if args.len() != sig.args.len() {
        return Ok(leaf(&id, format!("{} argument(s) given, the signature has {}", args.len(), sig.args.len())));
    }
    // Signatures carry no inputs (language-v2, 3.4): the tuple is found
    // among the relation's tuples at the bar, argument by argument; a `_`
    // (a body literal's wildcard) matches any value, and the node is then
    // named by the tuple found.
    let inputs: Vec<Value> = Vec::new();
    let tuples = match k.query(&rel, bar, &inputs) {
        Ok(t) => t,
        Err(RunError::Arithmetic { message, .. }) => return Ok(leaf(&id, format!("partial arithmetic: {}", message))),
        Err(RunError::NotABar { .. }) => return Ok(leaf(&id, format!("{} is not a bar of {}", format_timestamp(bar), rel))),
        Err(e) => return Err(e),
    };
    let found = tuples.into_iter().find(|tu| tu.len() == args.len() && tu.iter().zip(&args).all(|(v, a)| a == "_" || k.show(v) == *a));
    let Some(tuple) = found else {
        return Ok(leaf(&id, format!("no such tuple of {} at {}", rel, format_timestamp(bar))));
    };
    let id = if args.iter().any(|a| a == "_") {
        format!("{}({})@{}", rel, tuple.iter().map(|v| k.show(v)).collect::<Vec<_>>().join(", "), format_timestamp(bar))
    } else {
        id
    };
    if let Some(kind) = supplied(prog, &rel) {
        return Ok(leaf(&id, format!("supplied by the {}", kind)));
    }
    let Some(w) = witness(k, prog, &rel, bar, &tuple)? else {
        return Ok(leaf(&id, "no rule re-derives it".into()));
    };
    let mut body = Vec::new();
    for b in &w.body {
        if b.starts_with("not ") {
            body.push(Derivation {
                id: b.clone(),
                relation: String::new(),
                bar,
                rule: None,
                body: Vec::new(),
                note: Some("absent, as the negation requires".into()),
            });
            continue;
        }
        let bid = b.trim_end_matches(" (asof)");
        if depth == 0 {
            body.push(Derivation {
                id: bid.to_string(),
                relation: String::new(),
                bar,
                rule: None,
                body: Vec::new(),
                note: Some("depth limit: explain this tuple to continue".into()),
            });
            continue;
        }
        let child = derive_in(k, prog, bid, depth - 1, memo)?;
        body.push(child);
    }
    let d = Derivation {
        id: id.clone(),
        relation: rel,
        bar,
        rule: Some(w.rule),
        body,
        note: None,
    };
    memo.insert(id, d.clone());
    Ok(d)
}

/// One rule's attempt at an identity and bar: fired, or excluded at a
/// literal with the bindings at that point, or halted.
#[derive(Clone, Debug)]
pub struct Failure {
    pub rule: String,
    pub rule_index: usize,
    pub identity: String,
    pub bar: i64,
    pub fired: bool,
    pub solutions: usize,
    pub literal: Option<(usize, String)>,
    pub bindings: Vec<(String, String)>,
    /// The excluding literal is a supplied relation's atom with no tuple.
    pub absence_driven: bool,
    pub error: Option<String>,
    /// When the excluding literal reads a derived relation at an identity
    /// and bar the bindings fix: why that relation has no tuple there.
    pub deeper: Option<Box<WhyNot>>,
}

/// The head variable that names the instrument: the first entity-typed
/// head position holding a variable, or the first argument of a decision
/// constructor.
fn entity_var(prog: &Program, ri: usize) -> Option<String> {
    let rule = &prog.rules[ri];
    let sig = prog.relations.get(&rule.head.name)?;
    for (i, term) in rule.head.terms.iter().enumerate() {
        match term {
            Term::Var(v, _) if sig.args.get(i).map(|a| a.ty.is_entity()).unwrap_or(false) => return Some(v.clone()),
            Term::Ctor(_, args, _) => {
                if let Some(Term::Var(a, _)) = args.first() {
                    return Some(a.clone());
                }
            }
            _ => {}
        }
    }
    None
}

/// One attempt: the inputs, the head bindings, the identity shown.
type Attempt = (Vec<Value>, Vec<(String, Value)>, String);

/// Every rule of `rel` at `bar`, for the one identity or for every
/// symbol: whether it fired, else the first excluding literal.
pub fn failures(k: &mut Kernel, prog: &Program, rel: &str, bar: i64, inputs: Option<&[Value]>, symbol: Option<Value>) -> Result<Vec<Failure>, RunError> {
    let mut out = Vec::new();
    let Some(sig) = prog.relations.get(rel) else {
        return Err(RunError::Request(format!("`{}` is not a relation of {}", rel, prog.strategy)));
    };
    let need: Vec<&crate::ir::Arg> = sig.args.iter().filter(|a| false && a.mode == Mode::Key).collect();
    let input_sets: Vec<Vec<Value>> = match inputs {
        Some(v) => vec![v.to_vec()],
        None if need.is_empty() => vec![vec![]],
        None if matches!(need.as_slice(), [a] if a.ty.is_entity()) => match &symbol {
            Some(s) => vec![vec![s.clone()]],
            None => (0..k.symbol_names().len()).map(|i| vec![Value::Equity(i as u32)]).collect(),
        },
        None => {
            return Err(RunError::Request(format!(
                "`{}` takes the inputs {}; name them with --inputs",
                rel,
                need.iter().map(|a| format!("{}: {}", a.name, a.ty)).collect::<Vec<_>>().join(", ")
            )))
        }
    };
    let entity_input = matches!(need.as_slice(), [a] if a.ty.is_entity());
    for ri in prog.rules_for(rel) {
        let var = entity_var(prog, ri);
        // The identities to attempt: the inputs name the entity, or the
        // head's entity variable ranges over the symbols (or the one given).
        let attempts: Vec<Attempt> = if entity_input || var.is_none() {
            input_sets.iter().map(|set| (set.clone(), Vec::new(), set.first().map(|v| k.show(v)).unwrap_or_default())).collect()
        } else {
            let v = var.clone().unwrap();
            let symbols: Vec<Value> = match &symbol {
                Some(s) => vec![s.clone()],
                None => (0..k.symbol_names().len()).map(|i| Value::Equity(i as u32)).collect(),
            };
            let mut all = Vec::new();
            for set in &input_sets {
                for s in &symbols {
                    all.push((set.clone(), vec![(v.clone(), s.clone())], k.show(s)));
                }
            }
            all
        };
        for (set, bindings, identity) in attempts {
            let base = Failure {
                rule: prog.rule_label(ri),
                rule_index: ri,
                identity,
                bar,
                fired: false,
                solutions: 0,
                literal: None,
                bindings: Vec::new(),
                absence_driven: false,
                error: None,
                deeper: None,
            };
            match k.explain_with(ri, bar, &set, &bindings) {
                Ok(ex) => {
                    let absence = ex.failed_at.as_ref().map(|(i, _)| absence_driven(prog, ri, *i)).unwrap_or(false);
                    out.push(Failure {
                        fired: ex.solutions > 0,
                        solutions: ex.solutions,
                        literal: ex.failed_at.clone(),
                        bindings: if ex.solutions > 0 { ex.solution.clone() } else { ex.at_failure.clone() },
                        absence_driven: absence,
                        ..base
                    });
                }
                Err(RunError::Request(m)) => {
                    // A binding the rule has no variable for (the head holds a
                    // literal instrument): the rule is attempted unbound once.
                    out.push(Failure { error: Some(m), ..base });
                }
                Err(RunError::Arithmetic { message, expr, .. }) => out.push(Failure {
                    error: Some(format!("`{}`: {}", expr, message)),
                    ..base
                }),
                Err(e) => return Err(e),
            }
        }
    }
    Ok(out)
}

fn absence_driven(prog: &Program, ri: usize, literal: usize) -> bool {
    match prog.rules[ri].body.get(literal) {
        Some(Literal::Atom(a)) | Some(Literal::AsOf { atom: a, .. }) => supplied(prog, &a.name).is_some(),
        _ => false,
    }
}

/// Why-not (section 7.4): per rule, the first excluding literal and its
/// bindings for one identity at one bar.
#[derive(Clone, Debug)]
pub struct WhyNot {
    pub relation: String,
    pub identity: String,
    pub bar: i64,
    pub rules: Vec<Failure>,
}

impl WhyNot {
    pub fn text(&self) -> String {
        let mut out = String::new();
        self.render(&mut out, 0);
        out
    }
    fn render(&self, out: &mut String, depth: usize) {
        let pad = "  ".repeat(depth);
        out.push_str(&format!("{}why not {}({}) at {}:\n", pad, self.relation, self.identity, format_timestamp(self.bar)));
        if self.rules.is_empty() {
            out.push_str(&format!("{}  no rule: the relation is supplied, not derived\n", pad));
        }
        for f in &self.rules {
            if f.fired {
                out.push_str(&format!("{}  {} fired ({} solution(s)): the tuple is present\n", pad, f.rule, f.solutions));
                continue;
            }
            match (&f.literal, &f.error) {
                (Some((i, text)), _) => out.push_str(&format!(
                    "{}  {} excluded at literal {} `{}`{}{}\n",
                    pad,
                    f.rule,
                    i + 1,
                    text,
                    if f.bindings.is_empty() {
                        String::new()
                    } else {
                        format!(" with {}", f.bindings.iter().map(|(v, x)| format!("{} = {}", v, x)).collect::<Vec<_>>().join(", "))
                    },
                    if f.absence_driven { ": absence-driven (a supplied relation has no tuple there)" } else { "" }
                )),
                (None, Some(e)) => out.push_str(&format!("{}  {} halted: {}\n", pad, f.rule, e)),
                (None, None) => out.push_str(&format!("{}  {} did not fire\n", pad, f.rule)),
            }
            if let Some(d) = &f.deeper {
                d.render(out, depth + 2);
            }
        }
    }
    pub fn json(&self) -> serde_json::Value {
        serde_json::json!({
            "relation": self.relation,
            "identity": self.identity,
            "bar": format_timestamp(self.bar),
            "rules": self.rules.iter().map(|f| serde_json::json!({
                "rule": f.rule,
                "fired": f.fired,
                "solutions": f.solutions,
                "literal": f.literal.as_ref().map(|(i, _)| i),
                "literal_text": f.literal.as_ref().map(|(_, t)| t),
                "bindings": f.bindings.iter().map(|(v, x)| serde_json::json!({"var": v, "value": x})).collect::<Vec<_>>(),
                "absence_driven": f.absence_driven,
                "error": f.error,
                "deeper": f.deeper.as_ref().map(|d| d.json()),
            })).collect::<Vec<_>>(),
        })
    }
    /// The verdict: `absence-driven` when every rule that did not fire was
    /// excluded by a supplied relation's absent tuple, directly or through
    /// the derived relations its excluding literal reads.
    pub fn absence_driven(&self) -> bool {
        let failed: Vec<&Failure> = self.rules.iter().filter(|f| !f.fired).collect();
        !failed.is_empty() && failed.iter().all(|f| f.absence_driven || f.deeper.as_ref().map(|d| d.absence_driven()).unwrap_or(false))
    }
    /// The deepest excluding literals: the root causes the chain reaches.
    pub fn roots(&self) -> Vec<&Failure> {
        let mut out = Vec::new();
        for f in self.rules.iter().filter(|f| !f.fired) {
            match &f.deeper {
                Some(d) => out.extend(d.roots()),
                None => out.push(f),
            }
        }
        out
    }
    /// The number of levels the chain reaches.
    pub fn depth(&self) -> usize {
        1 + self.rules.iter().filter_map(|f| f.deeper.as_ref().map(|d| d.depth())).max().unwrap_or(0)
    }
}

/// How deep `why_not` follows derived relations by default.
pub const WHY_NOT_DEPTH: usize = 8;

/// Why `rel` has no tuple for `identity` at `bar`: per rule the first
/// excluding literal, and when that literal reads a derived relation at an
/// identity and bar the bindings fix, why that relation has no tuple there,
/// down to a comparison, a supplied relation, or `WHY_NOT_DEPTH` levels.
pub fn why_not(k: &mut Kernel, prog: &Program, rel: &str, bar: i64, inputs: Option<&[Value]>, symbol: Option<Value>) -> Result<WhyNot, RunError> {
    why_not_to_depth(k, prog, rel, bar, inputs, symbol, WHY_NOT_DEPTH)
}

pub fn why_not_to_depth(k: &mut Kernel, prog: &Program, rel: &str, bar: i64, inputs: Option<&[Value]>, symbol: Option<Value>, depth: usize) -> Result<WhyNot, RunError> {
    let mut rules = failures(k, prog, rel, bar, inputs, symbol.clone())?;
    if depth > 0 {
        for f in rules.iter_mut() {
            if f.fired {
                continue;
            }
            let Some((li, _)) = &f.literal else { continue };
            let Some((name, sub_bar, sub_inputs, sub_symbol)) = derived_target(k, prog, f.rule_index, *li, &f.bindings) else {
                continue;
            };
            match why_not_to_depth(k, prog, &name, sub_bar, Some(&sub_inputs), sub_symbol, depth - 1) {
                Ok(w) => f.deeper = Some(Box::new(w)),
                Err(RunError::NotABar { .. }) | Err(RunError::Request(_)) => {}
                Err(e) => return Err(e),
            }
        }
    }
    Ok(WhyNot {
        relation: rel.to_string(),
        identity: symbol
            .map(|s| k.show(&s))
            .or_else(|| inputs.map(|v| v.iter().map(|x| k.show(x)).collect::<Vec<_>>().join(", ")))
            .unwrap_or_default(),
        bar,
        rules,
    })
}

/// The derived relation an excluding literal reads, with the bar, inputs
/// and instrument the bindings fix for it; `None` when the literal is not
/// an atom of a derived relation or a needed term is unbound.
fn derived_target(k: &Kernel, prog: &Program, ri: usize, li: usize, bindings: &[(String, String)]) -> Option<(String, i64, Vec<Value>, Option<Value>)> {
    let rule = &prog.rules[ri];
    let atom = match rule.body.get(li)? {
        Literal::Atom(a) | Literal::AsOf { atom: a, .. } => a,
        _ => return None,
    };
    if prog.rules_for(&atom.name).is_empty() {
        return None;
    }
    let sig = prog.relations.get(&atom.name)?;
    let bound = |v: &str| bindings.iter().find(|(n, _)| n == v).map(|(_, x)| x.as_str());
    let text_of = |t: &Term| -> Option<String> {
        match t {
            Term::Var(v, _) => bound(v).map(|s| s.to_string()),
            Term::Lit(l, _) => Some(render_lit(l)),
            Term::Param(p, _) => prog.param(&rule.unit, p).map(|p| render_lit(&p.value)),
            Term::Wild(_) | Term::Ctor(..) => None,
        }
    };
    let key = sig.key_pos()?;
    let bar = parse_timestamp(&text_of(atom.terms.get(key)?)?)?;
    // Signatures carry no inputs (language-v2, 4.2): the step binds the
    // relation's instrument, when the atom names one.
    let inputs = Vec::new();
    let mut symbol = None;
    for (i, a) in sig.args.iter().enumerate() {
        let term = atom.terms.get(i)?;
        if i != key && a.ty.is_entity() && symbol.is_none() {
            if let Some(t) = text_of(term) {
                symbol = k.parse_binding(&t).ok();
            }
        }
    }
    Some((atom.name.clone(), bar, inputs, symbol))
}
