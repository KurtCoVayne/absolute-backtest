//! The kernel: evaluation of a checked program over an environment instance
//! in the closed executor loop of section 7. Evaluation is top-down with
//! memoisation; every derived tuple is requested at a bound temporal key, and
//! WF-4 and WF-8 guarantee that every such request terminates.

pub mod eval;
pub mod time;
pub mod value;

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::rc::Rc;

use crate::check::Program;
use crate::ir::*;
pub use value::{Ctor, Decision, Sym, Symbols, Value};

pub type Tuple = Vec<Value>;
/// Memo key: relation id, then the key time and the input values.
pub(crate) type MemoKey = (usize, Vec<Value>);
pub(crate) type Derived = Rc<Vec<(Tuple, usize)>>;

/// Primitive facts: an environment instance E (section 7).
#[derive(Clone, Debug, Default)]
pub struct Dataset {
    pub symbols: Symbols,
    pub facts: BTreeMap<String, Vec<Tuple>>,
}

impl Dataset {
    pub fn new() -> Dataset {
        Dataset::default()
    }
    pub fn intern(&mut self, name: &str) -> Sym {
        self.symbols.intern(name)
    }
    pub fn add(&mut self, relation: &str, tuple: Tuple) {
        self.facts.entry(relation.to_string()).or_default().push(tuple);
    }
    /// The dataset restricted to tuples whose temporal key falls in a
    /// decision-resolution bucket at or before `t` (E|ₜ of section 7).
    pub fn truncated(&self, prog: &Program, t: i64) -> Dataset {
        let mut out = Dataset {
            symbols: self.symbols.clone(),
            facts: BTreeMap::new(),
        };
        for (name, tuples) in &self.facts {
            let Some(sig) = prog.relations.get(name) else { continue };
            let (Some(k), Some(res)) = (sig.key_pos(), sig.res) else { continue };
            let kept: Vec<Tuple> = tuples
                .iter()
                .filter(|tu| {
                    let key = tu[k].as_time().unwrap_or(i64::MAX);
                    let label = if res == prog.resolution { key } else { time::bucket(prog.resolution, key) };
                    label <= t
                })
                .cloned()
                .collect();
            out.facts.insert(name.clone(), kept);
        }
        out
    }
}

/// Executor configuration: part of the kernel, not of the program (section 6).
#[derive(Clone, Debug)]
pub struct ExecConfig {
    pub initial_cash: f64,
    /// Slippage applied to the fill price, in basis points, against the order.
    pub slippage_bps: f64,
    pub commission_per_share: f64,
    /// Primitive relation that supplies fill and valuation prices; `None`
    /// picks a Price-valued primitive, preferring one named `close`.
    pub price_relation: Option<String>,
    /// Parameter values replacing the program's defaults (section 3: the
    /// only values the kernel may vary between runs of one program). A name
    /// is `param` in the strategy or `unit::param`; each value must be of
    /// the parameter's type and within its declared range.
    pub param_overrides: Vec<(String, Lit)>,
}

impl Default for ExecConfig {
    fn default() -> ExecConfig {
        ExecConfig {
            initial_cash: 1_000_000.0,
            slippage_bps: 0.0,
            commission_per_share: 0.0,
            price_relation: None,
            param_overrides: Vec::new(),
        }
    }
}

#[derive(Clone, Debug)]
pub enum RunError {
    /// Partial arithmetic (section 7): the rule, the tuple and the expression.
    Arithmetic {
        rule: String,
        bindings: String,
        expr: String,
        message: String,
    },
    /// Two distinct decisions for one instrument at one T (section 6).
    Conflict {
        t: i64,
        equity: String,
        first: (String, String),
        second: (String, String),
    },
    /// A delta decision with a non-positive quantity (section 4).
    BadDecision {
        t: i64,
        rule: String,
        decision: String,
        message: String,
    },
    NoBars,
    /// An explain request at a timestamp outside the rule's time domain
    /// (section 6): the nearest bars before and after it, if any.
    NotABar {
        t: i64,
        res: Resolution,
        before: Option<i64>,
        after: Option<i64>,
    },
    /// A request the kernel cannot serve as asked: missing or ill-typed
    /// explain inputs, a binding of a variable the rule does not have, a
    /// parameter override outside its type or range.
    Request(String),
    /// An executor configuration the program cannot honour.
    Config(String),
    Internal(String),
}

impl std::fmt::Display for RunError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RunError::Arithmetic { rule, bindings, expr, message } => write!(f, "partial arithmetic in rule {} at tuple [{}]: `{}`: {}", rule, bindings, expr, message),
            RunError::Conflict { t, equity, first, second } => write!(
                f,
                "conflicting decisions for {} at {}: {} (rule {}) and {} (rule {})",
                equity,
                time::format_timestamp(*t),
                first.0,
                first.1,
                second.0,
                second.1
            ),
            RunError::BadDecision { t, rule, decision, message } => write!(f, "invalid decision {} from rule {} at {}: {}", decision, rule, time::format_timestamp(*t), message),
            RunError::NoBars => write!(f, "the environment instance has no bars at the decision resolution"),
            RunError::NotABar { t, res, before, after } => {
                write!(f, "{} is not a bar at {}; ", time::format_timestamp(*t), res)?;
                match (before, after) {
                    (Some(b), Some(a)) => write!(f, "nearest bars are {} and {}", time::format_timestamp(*b), time::format_timestamp(*a)),
                    (Some(b), None) | (None, Some(b)) => write!(f, "the nearest bar is {}", time::format_timestamp(*b)),
                    (None, None) => write!(f, "the time domain is empty"),
                }
            }
            RunError::Request(m) => write!(f, "{}", m),
            RunError::Config(m) => write!(f, "configuration error: {}", m),
            RunError::Internal(m) => write!(f, "internal kernel error: {}", m),
        }
    }
}

#[derive(Clone, Debug)]
pub struct DecisionRecord {
    pub t: i64,
    pub decision: Decision,
    pub rule: usize,
}

#[derive(Clone, Debug)]
pub struct FillRecord {
    pub t: i64,
    pub equity: Sym,
    pub quantity: f64,
    pub price: f64,
}

#[derive(Clone, Debug, Default)]
pub struct RunResult {
    pub symbols: Vec<String>,
    pub bars: Vec<i64>,
    pub decisions: Vec<DecisionRecord>,
    pub fills: Vec<FillRecord>,
    /// Decisions the executor could not carry out, with the reason.
    pub dropped: Vec<(i64, Decision, String)>,
    /// Cash plus marked positions at each bar, before that bar's decisions.
    pub equity_curve: Vec<(i64, f64)>,
    pub final_cash: f64,
    pub final_positions: BTreeMap<Sym, f64>,
}

impl RunResult {
    pub fn decisions_at(&self, t: i64) -> Vec<&Decision> {
        self.decisions.iter().filter(|d| d.t == t).map(|d| &d.decision).collect()
    }
    pub fn describe_decision(&self, d: &Decision) -> String {
        format!("{}({}, {})", d.ctor.name(), self.symbols[d.equity as usize], d.amount)
    }
}

/// A stored relation (primitive, executor or kernel state), indexed by key.
#[derive(Clone, Debug, Default)]
pub struct Store {
    pub by_time: BTreeMap<i64, Vec<Tuple>>,
}

impl Store {
    pub fn insert(&mut self, key: i64, tuple: Tuple) {
        self.by_time.entry(key).or_default().push(tuple);
    }
}

#[derive(Clone, Debug)]
pub(crate) struct RelInfo {
    pub name: String,
    pub key_pos: usize,
    pub inputs: Vec<usize>,
    pub stored: bool,
    pub res: Resolution,
    pub rules: Vec<usize>,
    pub entity_positions: Vec<usize>,
}

#[derive(Debug)]
pub(crate) struct CompiledRule {
    pub idx: usize,
    pub slots: HashMap<String, usize>,
    pub nslots: usize,
    pub label: String,
}

pub type Env = Vec<Option<Value>>;

/// The explanation of why a rule did or did not fire at a bar (section 7, diagnostics).
#[derive(Clone, Debug)]
pub struct Explanation {
    pub rule: String,
    pub t: i64,
    /// Variables pre-bound for this explanation, with the shown values.
    pub bindings: Vec<(String, String)>,
    pub solutions: usize,
    /// Index and text of the first body literal with no solution, if any.
    pub failed_at: Option<(usize, String)>,
}

impl std::fmt::Display for Explanation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let with = if self.bindings.is_empty() {
            String::new()
        } else {
            format!(" with {}", self.bindings.iter().map(|(v, x)| format!("{} = {}", v, x)).collect::<Vec<_>>().join(", "))
        };
        match &self.failed_at {
            Some((i, text)) => write!(
                f,
                "rule {} did not fire at {}{}: literal {} `{}` has no solution",
                self.rule,
                time::format_timestamp(self.t),
                with,
                i + 1,
                text
            ),
            None => write!(f, "rule {} fired at {}{} with {} solution(s)", self.rule, time::format_timestamp(self.t), with, self.solutions),
        }
    }
}

pub struct Kernel<'p> {
    pub(crate) prog: &'p Program,
    pub symbols: Symbols,
    pub(crate) stores: Vec<Store>,
    pub(crate) rels: Vec<RelInfo>,
    pub(crate) rel_ids: HashMap<String, usize>,
    pub(crate) compiled: Vec<Rc<CompiledRule>>,
    pub(crate) memo: HashMap<MemoKey, Derived>,
    pub(crate) in_progress: HashSet<MemoKey>,
    pub(crate) domains: HashMap<Resolution, BTreeSet<i64>>,
    pub(crate) cfg: ExecConfig,
    pub(crate) price_rel: Option<usize>,
    pub(crate) price_col: usize,
    pub(crate) last_price: HashMap<Sym, f64>,
    pub(crate) params: HashMap<(String, String), Value>,
}

impl<'p> Kernel<'p> {
    pub fn new(prog: &'p Program, dataset: &Dataset, cfg: ExecConfig) -> Result<Kernel<'p>, RunError> {
        // Intern every equity literal of the program, then order ids by identifier.
        let mut symbols = dataset.symbols.clone();
        for unit in prog.params.values() {
            for p in unit.values() {
                if let Lit::Equity(s) = &p.value {
                    symbols.intern(s);
                }
            }
        }
        for (_, l) in &cfg.param_overrides {
            if let Lit::Equity(s) = l {
                symbols.intern(s);
            }
        }
        for rule in &prog.rules {
            for_each_lit(rule, &mut |l| {
                if let Lit::Equity(s) = l {
                    symbols.intern(s);
                }
            });
        }
        let (symbols, remap) = symbols.sorted();
        let remap_value = |v: &Value| -> Value {
            match v {
                Value::Equity(s) => Value::Equity(remap[*s as usize]),
                Value::Decision(d) => Value::Decision(Decision {
                    ctor: d.ctor,
                    equity: remap[d.equity as usize],
                    amount: d.amount,
                }),
                v => v.clone(),
            }
        };
        // Relation table.
        let mut rels = Vec::new();
        let mut rel_ids = HashMap::new();
        for (name, sig) in &prog.relations {
            let key_pos = sig.key_pos().ok_or_else(|| RunError::Internal(format!("relation `{}` has no temporal key", name)))?;
            let stored = matches!(sig.kind, Kind::Primitive { .. } | Kind::Executor | Kind::KernelState);
            let res = sig.res.ok_or_else(|| RunError::Internal(format!("relation `{}` has no resolution", name)))?;
            rel_ids.insert(name.clone(), rels.len());
            rels.push(RelInfo {
                name: name.clone(),
                key_pos,
                inputs: sig.args.iter().enumerate().filter(|(_, a)| a.mode == Mode::In).map(|(i, _)| i).collect(),
                stored,
                res,
                rules: prog.rules_for(name),
                entity_positions: sig.args.iter().enumerate().filter(|(_, a)| a.ty.is_entity()).map(|(i, _)| i).collect(),
            });
        }
        // Stores and time domains.
        let mut stores: Vec<Store> = rels.iter().map(|_| Store::default()).collect();
        let mut native: HashMap<Resolution, BTreeSet<i64>> = HashMap::new();
        for (name, tuples) in &dataset.facts {
            let Some(&id) = rel_ids.get(name) else { continue };
            let info = &rels[id];
            for tu in tuples {
                let tu: Tuple = tu.iter().map(remap_value).collect();
                let key = tu[info.key_pos].as_time().ok_or_else(|| RunError::Internal(format!("non-timestamp key in `{}`", name)))?;
                native.entry(info.res).or_default().insert(key);
                stores[id].insert(key, tu);
            }
        }
        let all_res = [Resolution::M1, Resolution::M5, Resolution::M15, Resolution::M30, Resolution::H1, Resolution::D1];
        let mut domains: HashMap<Resolution, BTreeSet<i64>> = HashMap::new();
        for r in all_res {
            let mut dom: BTreeSet<i64> = native.get(&r).cloned().unwrap_or_default();
            for (fr, ts) in &native {
                if fr.finer_than(r) {
                    for &t in ts {
                        dom.insert(time::bucket(r, t));
                    }
                }
            }
            domains.insert(r, dom);
        }
        // Compiled rules: variable slots.
        let mut compiled = Vec::new();
        for (i, rule) in prog.rules.iter().enumerate() {
            let mut slots: HashMap<String, usize> = HashMap::new();
            collect_vars(rule, &mut |v| {
                let n = slots.len();
                slots.entry(v.to_string()).or_insert(n);
            });
            let nslots = slots.len();
            compiled.push(Rc::new(CompiledRule {
                idx: i,
                slots,
                nslots,
                label: prog.rule_label(i),
            }));
        }
        // Parameters.
        let mut params = HashMap::new();
        let mut sym_tmp = symbols.clone();
        for (unit, ps) in &prog.params {
            for (name, p) in ps {
                params.insert((unit.clone(), name.clone()), lit_value(&p.value, &mut sym_tmp));
            }
        }
        for (name, value) in &cfg.param_overrides {
            let (unit, pname) = name.split_once("::").unwrap_or((prog.strategy.as_str(), name.as_str()));
            let Some(p) = prog.param(unit, pname) else {
                let known: Vec<String> = prog
                    .params
                    .iter()
                    .flat_map(|(u, ps)| ps.keys().map(move |n| if *u == prog.strategy { n.clone() } else { format!("{}::{}", u, n) }))
                    .collect();
                return Err(RunError::Request(format!(
                    "no parameter `{}` in `{}`; the parameters are {}",
                    name,
                    unit,
                    if known.is_empty() { "none".to_string() } else { known.join(", ") }
                )));
            };
            if !crate::check::types::compat(&p.ty, &value.ty()) {
                return Err(RunError::Request(format!("parameter `{}` of `{}` is {}, and `{}` is {}", pname, unit, p.ty, value, value.ty())));
            }
            if let Some((lo, hi)) = &p.range {
                let within = match (lit_magnitude(value), lit_magnitude(lo), lit_magnitude(hi)) {
                    (Some(v), Some(l), Some(h)) => l <= v && v <= h,
                    _ => true,
                };
                if !within {
                    return Err(RunError::Request(format!("parameter `{}` of `{}` = {} is outside its range {}..{}", pname, unit, value, lo, hi)));
                }
            }
            // WF-4 was judged on the default: `lag` by a zero duration is
            // causal, not strict (section 5), so an override may not change
            // whether a lag duration is zero.
            if let (Lit::Duration(d), Lit::Duration(v)) = (&p.value, value) {
                if d.is_zero() != v.is_zero() && lag_uses_param(prog, unit, pname) {
                    return Err(RunError::Request(format!(
                        "parameter `{}` of `{}` is a lag duration and the checker judged WF-4 with its default {}; an override may not change whether it is zero (got {})",
                        pname, unit, d, v
                    )));
                }
            }
            params.insert((unit.to_string(), pname.to_string()), lit_value(value, &mut sym_tmp));
        }
        // Price relation for the executor.
        let price_rel = match &cfg.price_relation {
            Some(name) => {
                let id = *rel_ids.get(name).ok_or_else(|| RunError::Config(format!("price relation `{}` is not in the program", name)))?;
                let sig = prog.relations.get(name).unwrap();
                if !matches!(sig.kind, Kind::Primitive { .. }) || price_column(sig).is_none() || !sig.args.iter().any(|a| a.ty.is_entity()) {
                    return Err(RunError::Config(format!(
                        "price relation `{}` must be a primitive with an equity argument and a Price<...> output; `{}` has none",
                        name, name
                    )));
                }
                if !sig.res.map(|r| r <= prog.resolution).unwrap_or(false) {
                    return Err(RunError::Config(format!("price relation `{}` is coarser than the decision resolution {}", name, prog.resolution)));
                }
                Some(id)
            }
            None => {
                let mut cands: Vec<(&String, &Signature)> = prog
                    .relations
                    .iter()
                    .filter(|(_, s)| matches!(s.kind, Kind::Primitive { .. }) && s.res.map(|r| r <= prog.resolution).unwrap_or(false))
                    .filter(|(_, s)| s.args.iter().any(|a| a.ty.is_entity()) && price_column(s).is_some())
                    .collect();
                cands.sort_by_key(|(n, s)| (s.res != Some(prog.resolution), !n.starts_with("close"), (*n).clone()));
                cands.first().map(|(n, _)| rel_ids[*n])
            }
        };
        let price_col = price_rel.and_then(|id| price_column(prog.relations.get(&rels[id].name).unwrap())).unwrap_or(0);
        Ok(Kernel {
            prog,
            symbols,
            stores,
            rels,
            rel_ids,
            compiled,
            memo: HashMap::new(),
            in_progress: HashSet::new(),
            domains,
            cfg,
            price_rel,
            price_col,
            last_price: HashMap::new(),
            params,
        })
    }

    pub fn program(&self) -> &'p Program {
        self.prog
    }

    pub fn decision_bars(&self) -> Vec<i64> {
        self.domains[&self.prog.resolution].iter().copied().collect()
    }

    fn rel(&self, name: &str) -> Result<usize, RunError> {
        self.rel_ids.get(name).copied().ok_or_else(|| RunError::Internal(format!("unknown relation `{}`", name)))
    }

    /// Run the closed loop of section 7 over every decision bar.
    pub fn run(&mut self) -> Result<RunResult, RunError> {
        let bars = self.decision_bars();
        if bars.is_empty() {
            return Err(RunError::NoBars);
        }
        let decide = self.rel("decide")?;
        let decided = self.rel("decided")?;
        let position = self.rel("position")?;
        let cash_rel = self.rel("cash")?;
        let fill_rel = self.rel("fill")?;
        let mut cash = self.cfg.initial_cash;
        let mut positions: BTreeMap<Sym, f64> = BTreeMap::new();
        self.stores[cash_rel].insert(bars[0], vec![Value::Time(bars[0]), Value::Num(cash)]);
        let mut result = RunResult {
            symbols: self.symbols.names().to_vec(),
            bars: bars.clone(),
            ..Default::default()
        };
        let delta = self.prog.mode == DecisionMode::Delta;
        for (k, &t) in bars.iter().enumerate() {
            // Mark to market before the bar's decisions.
            let mut equity = cash;
            for (&sym, &q) in &positions {
                if let Some(p) = self.price_at(sym, t) {
                    equity += q * p;
                }
            }
            result.equity_curve.push((t, equity));
            // Decisions at t.
            let tuples = self.call(decide, &[Some(Value::Time(t)), None])?;
            let mut by_equity: BTreeMap<Sym, Vec<(Decision, usize)>> = BTreeMap::new();
            for (tu, rule) in tuples.iter() {
                let Value::Decision(d) = &tu[1] else {
                    return Err(RunError::Internal("decide tuple without a decision".into()));
                };
                if delta && d.amount.partial_cmp(&0.0) != Some(std::cmp::Ordering::Greater) {
                    return Err(RunError::BadDecision {
                        t,
                        rule: self.prog.rule_label(*rule),
                        decision: result.describe_decision(d),
                        message: "delta decisions require a positive quantity".into(),
                    });
                }
                by_equity.entry(d.equity).or_default().push((d.clone(), *rule));
            }
            for (sym, ds) in &by_equity {
                if ds.len() > 1 {
                    let (a, ra) = &ds[0];
                    let (b, rb) = &ds[1];
                    return Err(RunError::Conflict {
                        t,
                        equity: self.symbols.name(*sym).to_string(),
                        first: (result.describe_decision(a), self.prog.rule_label(*ra)),
                        second: (result.describe_decision(b), self.prog.rule_label(*rb)),
                    });
                }
            }
            for ds in by_equity.values() {
                for (d, rule) in ds {
                    result.decisions.push(DecisionRecord { t, decision: d.clone(), rule: *rule });
                    self.stores[decided].insert(t, vec![Value::Time(t), Value::Decision(d.clone())]);
                }
            }
            // Executor: fills at the next bar (section 6, execution contract).
            if let Some(&tn) = bars.get(k + 1) {
                let mut equity_next = cash;
                for (&sym, &q) in &positions {
                    if let Some(p) = self.price_at(sym, tn) {
                        equity_next += q * p;
                    }
                }
                for ds in by_equity.values() {
                    for (d, _) in ds {
                        let sym = d.equity;
                        let Some(p) = self.bar_price(sym, tn) else {
                            result
                                .dropped
                                .push((t, d.clone(), format!("no price for {} at {}", self.symbols.name(sym), time::format_timestamp(tn))));
                            continue;
                        };
                        let pos = positions.get(&sym).copied().unwrap_or(0.0);
                        let qty = match d.ctor {
                            Ctor::Buy | Ctor::Cover => d.amount,
                            Ctor::Sell | Ctor::Short => -d.amount,
                            Ctor::TargetQuantity => d.amount - pos,
                            Ctor::TargetWeight => (d.amount * equity_next / p).trunc() - pos,
                        };
                        if qty == 0.0 {
                            continue;
                        }
                        let slip = self.cfg.slippage_bps / 10_000.0;
                        let fill_price = if qty > 0.0 { p * (1.0 + slip) } else { p * (1.0 - slip) };
                        cash -= qty * fill_price + self.cfg.commission_per_share * qty.abs();
                        let new_pos = pos + qty;
                        if new_pos.abs() < 1e-9 {
                            positions.remove(&sym);
                        } else {
                            positions.insert(sym, new_pos);
                        }
                        self.last_price.insert(sym, fill_price);
                        self.stores[fill_rel].insert(tn, vec![Value::Equity(sym), Value::Time(tn), Value::Num(qty), Value::Num(fill_price)]);
                        result.fills.push(FillRecord {
                            t: tn,
                            equity: sym,
                            quantity: qty,
                            price: fill_price,
                        });
                    }
                }
                for (&sym, &q) in &positions {
                    self.stores[position].insert(tn, vec![Value::Equity(sym), Value::Time(tn), Value::Num(q)]);
                }
                self.stores[cash_rel].insert(tn, vec![Value::Time(tn), Value::Num(cash)]);
            } else {
                // A decision on the last bar has no bar to fill at: it is
                // recorded in `decided` like any other, and dropped here so
                // that decisions = fills + dropped.
                for ds in by_equity.values() {
                    for (d, _) in ds {
                        result.dropped.push((t, d.clone(), "no next bar".to_string()));
                    }
                }
            }
        }
        result.final_cash = cash;
        result.final_positions = positions;
        Ok(result)
    }

    /// Price of `sym` for marking at decision bar `t`: the bar's price, or
    /// the last price seen when the bar has none.
    pub fn price_at(&mut self, sym: Sym, t: i64) -> Option<f64> {
        match self.bar_price(sym, t) {
            Some(p) => Some(p),
            None => self.last_price.get(&sym).copied(),
        }
    }

    /// Price of `sym` at decision bar `t` from the configured price relation:
    /// the tuple at `t`, or the last fine tuple in its bucket; `None` when the
    /// bar has no price, so that the executor drops rather than fills.
    pub fn bar_price(&mut self, sym: Sym, t: i64) -> Option<f64> {
        let id = self.price_rel?;
        let info = &self.rels[id];
        let entity_pos = *info.entity_positions.first()?;
        let col = self.price_col;
        let found = if info.res == self.prog.resolution {
            self.stores[id]
                .by_time
                .get(&t)
                .and_then(|tus| tus.iter().find(|tu| tu[entity_pos] == Value::Equity(sym)))
                .and_then(|tu| tu[col].as_f64())
        } else {
            let (lo, hi) = time::bucket_range(self.prog.resolution, t);
            self.stores[id]
                .by_time
                .range(lo..=hi)
                .rev()
                .find_map(|(_, tus)| tus.iter().find(|tu| tu[entity_pos] == Value::Equity(sym)).and_then(|tu| tu[col].as_f64()))
        };
        if let Some(p) = found {
            self.last_price.insert(sym, p);
        }
        found
    }

    /// The `+` head arguments of rule `rule_idx`, in signature order: what
    /// `explain` needs as `inputs`.
    pub fn rule_inputs(&self, rule_idx: usize) -> Vec<(String, Ty)> {
        let sig = &self.prog.relations[&self.prog.rules[rule_idx].head.name];
        sig.args.iter().filter(|a| a.mode == Mode::In).map(|a| (a.name.clone(), a.ty.clone())).collect()
    }

    /// Parse a command-line value as an input of type `ty`: an equity by
    /// identifier, a timestamp, or a literal in the DSL's own grammar.
    pub fn parse_input(&self, ty: &Ty, raw: &str) -> Result<Value, String> {
        let raw = raw.trim();
        match ty {
            Ty::Equity => self.equity(raw.trim_matches('"')),
            Ty::Timestamp => time::parse_timestamp(raw)
                .map(Value::Time)
                .ok_or_else(|| format!("`{}` is not a Timestamp (YYYY-MM-DD[THH:MM[:SS]])", raw)),
            Ty::Count => raw.parse().map(Value::Count).map_err(|_| format!("`{}` is not a Count", raw)),
            Ty::Duration => match crate::parser::parse_lit(raw) {
                Ok(Lit::Duration(d)) => Ok(Value::Dur(d)),
                _ => Err(format!("`{}` is not a Duration (such as 20d, 3mo or 1y)", raw)),
            },
            Ty::Quantity(_) => match crate::parser::parse_lit(raw) {
                Ok(l @ (Lit::Int(_) | Lit::Float(_))) => Ok(Value::Num(l_num(&l))),
                Ok(l @ (Lit::Shares(_) | Lit::Money(..))) if crate::check::types::compat(ty, &l.ty()) => Ok(Value::Num(l_num(&l))),
                Ok(l) => Err(format!("`{}` is {}, not {}", raw, l.ty(), ty)),
                Err(_) => Err(format!("`{}` is not a {}", raw, ty)),
            },
            Ty::Decision | Ty::IntLit => Err(format!("a {} cannot be given on the command line", ty)),
        }
    }

    /// Parse a command-line value for a body variable, whose type is not
    /// declared: an equity of the dataset, a timestamp, or a literal (a bare
    /// integer is a Count; write a quantity with a decimal point or a unit).
    pub fn parse_binding(&self, raw: &str) -> Result<Value, String> {
        let raw = raw.trim();
        if let Some(s) = self.symbols.get(raw) {
            return Ok(Value::Equity(s));
        }
        if let Some(t) = time::parse_timestamp(raw) {
            return Ok(Value::Time(t));
        }
        match crate::parser::parse_lit(raw) {
            Ok(Lit::Equity(name)) => self.equity(&name),
            Ok(Lit::Int(i)) => Ok(Value::Count(i)),
            Ok(Lit::Duration(d)) => Ok(Value::Dur(d)),
            Ok(l) => Ok(Value::Num(l_num(&l))),
            Err(_) => Err(format!("`{}` is not an equity of the dataset, a timestamp or a literal", raw)),
        }
    }

    fn equity(&self, name: &str) -> Result<Value, String> {
        self.symbols.get(name).map(Value::Equity).ok_or_else(|| format!("`{}` is not an equity of the dataset", name))
    }

    /// Why rule `rule_idx` did or did not fire at `t` (section 7): the first
    /// body literal with no solution. `inputs` supplies the rule's `+` head
    /// arguments, in signature order (empty for a decide rule).
    pub fn explain(&mut self, rule_idx: usize, t: i64, inputs: &[Value]) -> Result<Explanation, RunError> {
        self.explain_with(rule_idx, t, inputs, &[])
    }

    /// `explain` with body variables pre-bound: the explanation is then about
    /// the bindings that agree with them (one instrument, say) rather than
    /// about the union of every binding.
    pub fn explain_with(&mut self, rule_idx: usize, t: i64, inputs: &[Value], bindings: &[(String, Value)]) -> Result<Explanation, RunError> {
        let rule = &self.prog.rules[rule_idx];
        let cr = self.compiled[rule_idx].clone();
        let rel = self.rel(&rule.head.name)?;
        let info = self.rels[rel].clone();
        // Only bars of the rule's time domain are ever evaluated (section 6);
        // a label between two bars would read a phantom bucket.
        let res = self.prog.infos[rule_idx].resolution;
        let domain = self.domains.get(&res).ok_or_else(|| RunError::Internal(format!("no time domain at {}", res)))?;
        if !domain.contains(&t) {
            return Err(RunError::NotABar {
                t,
                res,
                before: domain.range(..t).next_back().copied(),
                after: domain.range(t..).next().copied(),
            });
        }
        let mut env: Env = vec![None; cr.nslots];
        if let Term::Var(v, _) = &rule.head.terms[info.key_pos] {
            env[cr.slots[v]] = Some(Value::Time(t));
        }
        if inputs.len() != info.inputs.len() {
            let need: Vec<String> = self.rule_inputs(rule_idx).iter().map(|(n, ty)| format!("{}: {}", n, ty)).collect();
            return Err(RunError::Request(format!(
                "rule {} takes {} inputs ({}), {} given",
                cr.label,
                info.inputs.len(),
                need.join(", "),
                inputs.len()
            )));
        }
        for (pos, val) in info.inputs.iter().zip(inputs) {
            if let Term::Var(v, _) = &rule.head.terms[*pos] {
                env[cr.slots[v]] = Some(val.clone());
            }
        }
        let mut shown = Vec::with_capacity(bindings.len());
        for (var, val) in bindings {
            let Some(&slot) = cr.slots.get(var) else {
                let mut vars: Vec<&String> = cr.slots.keys().collect();
                vars.sort();
                return Err(RunError::Request(format!(
                    "rule {} has no variable `{}`; its variables are {}",
                    cr.label,
                    var,
                    vars.iter().map(|v| v.as_str()).collect::<Vec<_>>().join(", ")
                )));
            };
            if let Some(x) = &env[slot] {
                if x != val {
                    return Err(RunError::Request(format!(
                        "`{}` is already {} in rule {} and cannot also be {}",
                        var,
                        self.show(x),
                        cr.label,
                        self.show(val)
                    )));
                }
            }
            env[slot] = Some(val.clone());
            shown.push((var.clone(), self.show(val)));
        }
        let mut envs = vec![env];
        for (i, lit) in rule.body.iter().enumerate() {
            let mut next = Vec::new();
            for e in &envs {
                self.step(&cr, lit, e, &mut next)?;
            }
            if next.is_empty() {
                return Ok(Explanation {
                    rule: cr.label.clone(),
                    t,
                    bindings: shown,
                    solutions: 0,
                    failed_at: Some((i, lit.describe())),
                });
            }
            envs = next;
        }
        Ok(Explanation {
            rule: cr.label.clone(),
            t,
            bindings: shown,
            solutions: envs.len(),
            failed_at: None,
        })
    }
}

/// Whether any `lag` in `unit`'s rules takes parameter `name` as its length.
fn lag_uses_param(prog: &Program, unit: &str, name: &str) -> bool {
    fn lit(l: &Literal, name: &str) -> bool {
        match l {
            Literal::Builtin(Builtin::Lag { n: Expr::Param(p, _), .. }, _) => p == name,
            Literal::Agg { conj, .. } => conj.iter().any(|c| lit(c, name)),
            _ => false,
        }
    }
    prog.rules.iter().filter(|r| r.unit == unit).any(|r| r.body.iter().any(|l| lit(l, name)))
}

fn l_num(l: &Lit) -> f64 {
    match l {
        Lit::Int(i) => *i as f64,
        Lit::Float(x) | Lit::Shares(x) | Lit::Money(x, _) | Lit::Price(x, _) => *x,
        Lit::Duration(_) | Lit::Equity(_) => f64::NAN,
    }
}

/// The magnitude a literal is ranged by: its number, or a duration's
/// approximate length in days; an equity has none.
fn lit_magnitude(l: &Lit) -> Option<f64> {
    match l {
        Lit::Duration(d) => Some(d.approx_days()),
        Lit::Equity(_) => None,
        _ => Some(l_num(l)),
    }
}

fn price_column(sig: &Signature) -> Option<usize> {
    sig.args
        .iter()
        .position(|a| a.mode == Mode::Out && matches!(&a.ty, Ty::Quantity(d) if d.c2 == 2 && d.s2 == -2 && d.t2 == 0))
}

pub(crate) fn lit_value(l: &Lit, symbols: &mut Symbols) -> Value {
    match l {
        Lit::Int(i) => Value::Count(*i),
        Lit::Float(x) | Lit::Shares(x) | Lit::Money(x, _) | Lit::Price(x, _) => Value::Num(*x),
        Lit::Duration(d) => Value::Dur(*d),
        Lit::Equity(s) => Value::Equity(symbols.intern(s)),
    }
}

fn for_each_lit(rule: &Rule, f: &mut dyn FnMut(&Lit)) {
    fn term(t: &Term, f: &mut dyn FnMut(&Lit)) {
        match t {
            Term::Lit(l, _) => f(l),
            Term::Ctor(_, subs, _) => subs.iter().for_each(|s| term(s, f)),
            _ => {}
        }
    }
    fn expr(e: &Expr, f: &mut dyn FnMut(&Lit)) {
        match e {
            Expr::Lit(l, _) => f(l),
            Expr::Neg(x, _) => expr(x, f),
            Expr::Bin(_, a, b, _) => {
                expr(a, f);
                expr(b, f);
            }
            Expr::Call(_, args, _) => args.iter().for_each(|a| expr(a, f)),
            _ => {}
        }
    }
    fn lit(l: &Literal, f: &mut dyn FnMut(&Lit)) {
        match l {
            Literal::Atom(a) | Literal::Neg(a) => a.terms.iter().for_each(|t| term(t, f)),
            Literal::Builtin(b, _) => match b {
                Builtin::Prev { t, t1 } => {
                    term(t, f);
                    term(t1, f);
                }
                Builtin::Lag { t, n, t1 } => {
                    term(t, f);
                    expr(n, f);
                    term(t1, f);
                }
                Builtin::MonthStart { t } | Builtin::DayStart { t } => term(t, f),
            },
            Literal::Window { dur, min, .. } => {
                expr(dur, f);
                expr(min, f);
            }
            Literal::Cmp { lhs, rhs, .. } => {
                expr(lhs, f);
                expr(rhs, f);
            }
            Literal::Assign { expr: e, .. } => expr(e, f),
            Literal::Agg { args, conj, .. } => {
                args.iter().for_each(|a| expr(a, f));
                conj.iter().for_each(|c| lit(c, f));
            }
            Literal::Top { n, atom, .. } => {
                expr(n, f);
                atom.terms.iter().for_each(|t| term(t, f));
            }
            Literal::Resample { inner, min, aggs, .. } => {
                inner.terms.iter().for_each(|t| term(t, f));
                expr(min, f);
                aggs.iter().for_each(|(_, _, e)| expr(e, f));
            }
        }
    }
    rule.head.terms.iter().for_each(|t| term(t, f));
    rule.body.iter().for_each(|l| lit(l, f));
}

fn collect_vars(rule: &Rule, f: &mut dyn FnMut(&str)) {
    fn term(t: &Term, f: &mut dyn FnMut(&str)) {
        match t {
            Term::Var(v, _) => f(v),
            Term::Ctor(_, subs, _) => subs.iter().for_each(|s| term(s, f)),
            _ => {}
        }
    }
    fn expr(e: &Expr, f: &mut dyn FnMut(&str)) {
        let mut vs = Vec::new();
        e.vars(&mut vs);
        for (v, _) in vs {
            f(&v);
        }
    }
    fn lit(l: &Literal, f: &mut dyn FnMut(&str)) {
        match l {
            Literal::Atom(a) | Literal::Neg(a) => a.terms.iter().for_each(|t| term(t, f)),
            Literal::Builtin(b, _) => match b {
                Builtin::Prev { t, t1 } => {
                    term(t, f);
                    term(t1, f);
                }
                Builtin::Lag { t, n, t1 } => {
                    term(t, f);
                    expr(n, f);
                    term(t1, f);
                }
                Builtin::MonthStart { t } | Builtin::DayStart { t } => term(t, f),
            },
            Literal::Window { var, base, dur, min, .. } => {
                term(var, f);
                term(base, f);
                expr(dur, f);
                expr(min, f);
            }
            Literal::Cmp { lhs, rhs, .. } => {
                expr(lhs, f);
                expr(rhs, f);
            }
            Literal::Assign { var, expr: e, .. } => {
                f(var);
                expr(e, f);
            }
            Literal::Agg { var, args, conj, .. } => {
                f(var);
                args.iter().for_each(|a| expr(a, f));
                conj.iter().for_each(|c| lit(c, f));
            }
            Literal::Top { n, atom, by, .. } => {
                expr(n, f);
                atom.terms.iter().for_each(|t| term(t, f));
                if let Some(by) = by {
                    by.iter().for_each(|(k, _, _)| f(k));
                }
            }
            Literal::Resample { inner, as_var, min, aggs, .. } => {
                inner.terms.iter().for_each(|t| term(t, f));
                f(as_var);
                expr(min, f);
                aggs.iter().for_each(|(x, _, e)| {
                    f(x);
                    expr(e, f);
                });
            }
        }
    }
    rule.head.terms.iter().for_each(|t| term(t, f));
    rule.body.iter().for_each(|l| lit(l, f));
}

/// Run a checked program over a dataset; the whole closed loop of section 7.
/// Evaluation recurses one level per bar of strictly-earlier recursion, so it
/// runs on a thread with a generous stack.
pub fn run(prog: &Program, dataset: &Dataset, cfg: ExecConfig) -> Result<RunResult, RunError> {
    std::thread::scope(|s| {
        std::thread::Builder::new()
            .stack_size(512 << 20)
            .spawn_scoped(s, || {
                let mut k = Kernel::new(prog, dataset, cfg)?;
                k.run()
            })
            .map_err(|e| RunError::Internal(format!("cannot spawn kernel thread: {}", e)))?
            .join()
            .map_err(|_| RunError::Internal("kernel thread panicked".into()))?
    })
}

/// A sampled timestamp at which the full run and the truncated run disagree.
#[derive(Clone, Debug)]
pub struct CausalityMismatch {
    pub t: i64,
    pub full: Vec<String>,
    pub truncated: Vec<String>,
}

/// The empirical check of the causality theorem (section 7): for each sampled
/// t, decide(t) over E must equal decide(t) over E|ₜ.
pub fn verify_causality(prog: &Program, dataset: &Dataset, cfg: ExecConfig, samples: &[i64]) -> Result<Vec<CausalityMismatch>, RunError> {
    let full = run(prog, dataset, cfg.clone())?;
    let describe = |r: &RunResult, t: i64| -> Vec<String> {
        let mut v: Vec<String> = r.decisions_at(t).into_iter().map(|d| r.describe_decision(d)).collect();
        v.sort();
        v
    };
    let mut out = Vec::new();
    for &t in samples {
        let trunc = dataset.truncated(prog, t);
        let partial = run(prog, &trunc, cfg.clone())?;
        let a = describe(&full, t);
        let b = describe(&partial, t);
        if a != b {
            out.push(CausalityMismatch { t, full: a, truncated: b });
        }
    }
    Ok(out)
}
