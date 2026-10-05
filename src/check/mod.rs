//! The well-formedness checker (spec section 5 and the rule map of section 8).
//! Every diagnostic carries exactly one code, and every code is one judgment.

pub mod rule;
pub mod types;

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use crate::ir::*;

/// One diagnostic code per judgment (section 8).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Code {
    /// Name resolution.
    U,
    /// Primitive not provided by the declared environment.
    E,
    /// WF-1 range restriction.
    B,
    /// WF-2 modes.
    M,
    /// WF-3 types.
    T,
    /// WF-4 temporal recursion.
    R,
    /// WF-5 completeness.
    N,
    /// WF-6 causality.
    F,
    /// WF-7 deterministic reduction.
    D,
    /// WF-8 stratification.
    S,
    /// WF-9: at least one decide rule.
    Z,
    /// WF-9: decision mode and constructors.
    C,
    /// WF-10 resolution compatibility.
    X,
    /// Warning: dead derived relation.
    W1,
    /// Warning: unused parameter.
    W2,
    /// Warning: declared relation with no defining rule.
    W3,
}

impl Code {
    pub fn parse(s: &str) -> Option<Code> {
        Some(match s {
            "U" => Code::U,
            "E" => Code::E,
            "B" => Code::B,
            "M" => Code::M,
            "T" => Code::T,
            "R" => Code::R,
            "N" => Code::N,
            "F" => Code::F,
            "D" => Code::D,
            "S" => Code::S,
            "Z" => Code::Z,
            "C" => Code::C,
            "X" => Code::X,
            "W1" => Code::W1,
            "W2" => Code::W2,
            "W3" => Code::W3,
            _ => return None,
        })
    }
    pub fn judgment(self) -> &'static str {
        match self {
            Code::U => "name resolution",
            Code::E => "environment",
            Code::B => "WF-1 range restriction",
            Code::M => "WF-2 modes",
            Code::T => "WF-3 types",
            Code::R => "WF-4 temporal recursion",
            Code::N => "WF-5 completeness",
            Code::F => "WF-6 causality",
            Code::D => "WF-7 determinism",
            Code::S => "WF-8 stratification",
            Code::Z => "WF-9 decisions",
            Code::C => "WF-9 decisions",
            Code::X => "WF-10 resolution",
            Code::W1 => "dead rule",
            Code::W2 => "unused parameter",
            Code::W3 => "undefined relation",
        }
    }
}

impl std::fmt::Display for Code {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?}", self)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Severity {
    Error,
    Warning,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Diagnostic {
    pub code: Code,
    pub severity: Severity,
    pub unit: String,
    pub rule: Option<String>,
    pub span: Span,
    pub message: String,
}

impl std::fmt::Display for Diagnostic {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let sev = match self.severity {
            Severity::Error => "error",
            Severity::Warning => "warning",
        };
        write!(f, "{} [{}] {} at {}", sev, self.code, self.unit, self.span)?;
        if let Some(r) = &self.rule {
            write!(f, " in rule {}", r)?;
        }
        write!(f, ": {} ({})", self.message, self.code.judgment())
    }
}

/// Every parsed unit the checker may resolve names against.
#[derive(Clone, Debug, Default)]
pub struct Workspace {
    pub units: Vec<Unit>,
}

impl Workspace {
    pub fn new() -> Workspace {
        Workspace { units: vec![] }
    }
    pub fn add_source(&mut self, src: &str) -> Result<(), crate::parser::ParseError> {
        let units = crate::parser::parse_units(src)?;
        self.units.extend(units);
        Ok(())
    }
    pub fn find(&self, kind: UnitKind, name: &str) -> Option<&Unit> {
        self.units.iter().find(|u| u.kind == kind && u.name == name)
    }
    pub fn strategies(&self) -> impl Iterator<Item = &Unit> {
        self.units.iter().filter(|u| u.kind == UnitKind::Strategy)
    }
    pub fn libraries(&self) -> impl Iterator<Item = &Unit> {
        self.units.iter().filter(|u| u.kind == UnitKind::Library)
    }
}

/// Polarity of a body reference for the dependency graph.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Polarity {
    Positive,
    Negative,
    /// Inside an aggregation, a reduction or a resample.
    Aggregate,
}

/// Where a time term provably sits relative to the rule's head time T (WF-6).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TimeProv {
    /// T itself.
    Head,
    /// Provably <= T.
    Causal,
    /// Provably < T.
    Strict,
    /// Not derived from T.
    Other,
    /// Bound by an atom the checker could not resolve (U or E already
    /// reported): not judged, so one unresolved name does not cascade into
    /// causality errors on every atom after it.
    Unknown,
}

/// A body reference to a relation, recorded during rule analysis for the
/// program-level judgments.
#[derive(Clone, Debug)]
pub struct AtomRef {
    pub name: String,
    pub polarity: Polarity,
    pub key_prov: TimeProv,
    pub span: Span,
    /// The literal is a `top` (the reduction closes the relation, WF-5).
    pub is_top: bool,
}

/// What the per-rule pass learned about one rule.
#[derive(Clone, Debug)]
pub struct RuleInfo {
    pub head: String,
    pub refs: Vec<AtomRef>,
    pub params_used: Vec<String>,
    pub resolution: Resolution,
}

/// A resolved, checked program: a strategy with its libraries and environment.
#[derive(Clone, Debug)]
pub struct Program {
    pub strategy: String,
    pub environment: String,
    pub resolution: Resolution,
    pub mode: DecisionMode,
    /// All relations in scope, by name.
    pub relations: BTreeMap<String, Signature>,
    /// Parameters by unit name, then parameter name.
    pub params: BTreeMap<String, BTreeMap<String, Param>>,
    /// All rules (library rules first, in `uses` order, then the strategy's).
    pub rules: Vec<Rule>,
    /// Per-rule analysis results, parallel to `rules`.
    pub infos: Vec<RuleInfo>,
    /// Strata in evaluation order: each is a set of relation names.
    pub strata: Vec<Vec<String>>,
    /// Relations judged complete (WF-5).
    pub complete: BTreeSet<String>,
}

impl Program {
    pub fn rules_for(&self, rel: &str) -> Vec<usize> {
        self.rules.iter().enumerate().filter(|(_, r)| r.head.name == rel).map(|(i, _)| i).collect()
    }
    pub fn param(&self, unit: &str, name: &str) -> Option<&Param> {
        self.params.get(unit).and_then(|m| m.get(name))
    }
    pub fn rule_label(&self, i: usize) -> String {
        rule_label(&self.rules, i)
    }
}

/// Order two literals of one type, for a parameter's range (section 3):
/// numbers by value, money of one currency by amount, durations by
/// calendar length. A calendar duration has no single length, so two
/// durations are ordered only when every length of one lies on the same
/// side of every length of the other (`1mo` is neither above nor below
/// `30d`). `None` when they are not comparable.
fn lit_order(a: &Lit, b: &Lit) -> Option<std::cmp::Ordering> {
    use std::cmp::Ordering;
    let num = |l: &Lit| match l {
        Lit::Int(i) => Some(*i as f64),
        Lit::Float(x) | Lit::Shares(x) => Some(*x),
        _ => None,
    };
    match (a, b) {
        (Lit::Money(x, cx), Lit::Money(y, cy)) if cx == cy => x.partial_cmp(y),
        (Lit::Duration(x), Lit::Duration(y)) if x == y => Some(Ordering::Equal),
        (Lit::Duration(x), Lit::Duration(y)) if x.min_days() > y.max_days() => Some(Ordering::Greater),
        (Lit::Duration(x), Lit::Duration(y)) if x.max_days() < y.min_days() => Some(Ordering::Less),
        (Lit::Money(..), _) | (_, Lit::Money(..)) | (Lit::Duration(_), _) | (_, Lit::Duration(_)) | (Lit::Equity(_), _) | (_, Lit::Equity(_)) => None,
        _ => num(a)?.partial_cmp(&num(b)?),
    }
}

/// The decision constructors written in `decided` patterns of a body, each
/// with the pattern's time term and span, including those inside an
/// aggregation's conjunction.
fn decided_patterns(body: &[Literal]) -> Vec<(&str, &Term, Span)> {
    fn atom<'a>(a: &'a Atom, out: &mut Vec<(&'a str, &'a Term, Span)>) {
        if a.name == "decided" && a.terms.len() == 2 {
            if let Term::Ctor(c, _, sp) = &a.terms[1] {
                out.push((c, &a.terms[0], *sp));
            }
        }
    }
    fn lit<'a>(l: &'a Literal, out: &mut Vec<(&'a str, &'a Term, Span)>) {
        match l {
            Literal::Atom(a) | Literal::Neg(a) | Literal::Top { atom: a, .. } | Literal::Resample { inner: a, .. } => atom(a, out),
            Literal::Agg { conj, .. } => conj.iter().for_each(|l| lit(l, out)),
            Literal::Builtin(..) | Literal::Window { .. } | Literal::Cmp { .. } | Literal::Assign { .. } => {}
        }
    }
    let mut out = Vec::new();
    body.iter().for_each(|l| lit(l, &mut out));
    out
}

pub fn rule_label(rules: &[Rule], i: usize) -> String {
    let r = &rules[i];
    let k = rules[..i].iter().filter(|o| o.unit == r.unit && o.head.name == r.head.name).count() + 1;
    format!("{}::{}#{}", r.unit, r.head.name, k)
}

/// Check every strategy and library in the workspace.
///
/// A library is checked in the scope of every strategy that uses it and once
/// on its own, so a diagnostic on a library rule that does not depend on the
/// using strategy would be reported once per check; identical diagnostics are
/// kept once. Diagnostics that differ (an X error naming the using strategy's
/// decision resolution, say) are all kept.
pub fn check_workspace(ws: &Workspace) -> Vec<Diagnostic> {
    let mut out: Vec<Diagnostic> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    let mut push = |d: Diagnostic, out: &mut Vec<Diagnostic>| {
        if seen.insert(d.to_string()) {
            out.push(d);
        }
    };
    // A name declared twice is checked once; the clash itself is reported by
    // that check (adv-02).
    let mut seen_names: BTreeSet<&str> = BTreeSet::new();
    for s in ws.strategies() {
        if seen_names.insert(&s.name) {
            for d in check_program(ws, &s.name).1 {
                push(d, &mut out);
            }
        }
    }
    seen_names.clear();
    for l in ws.libraries() {
        if seen_names.insert(&l.name) {
            for d in check_library(ws, &l.name) {
                push(d, &mut out);
            }
        }
    }
    out
}

/// Check one strategy; returns the resolved program when it has no errors.
pub fn check_program(ws: &Workspace, strategy: &str) -> (Option<Program>, Vec<Diagnostic>) {
    let Some(unit) = ws.find(UnitKind::Strategy, strategy) else {
        return (
            None,
            vec![Diagnostic {
                code: Code::U,
                severity: Severity::Error,
                unit: strategy.to_string(),
                rule: None,
                span: Span::default(),
                message: format!("strategy `{}` is not in the workspace", strategy),
            }],
        );
    };
    let mut cx = Checker::new(ws, unit);
    cx.resolve();
    cx.check_rules();
    cx.program_checks();
    cx.finish()
}

/// Check a library on its own (as if used by a strategy at its resolution).
pub fn check_library(ws: &Workspace, library: &str) -> Vec<Diagnostic> {
    let Some(unit) = ws.find(UnitKind::Library, library) else {
        return vec![];
    };
    let mut cx = Checker::new(ws, unit);
    cx.resolve();
    cx.check_rules();
    cx.program_checks();
    cx.finish().1
}

pub(crate) struct Checker<'a> {
    pub ws: &'a Workspace,
    pub root: &'a Unit,
    pub diags: Vec<Diagnostic>,
    pub relations: BTreeMap<String, Signature>,
    pub params: BTreeMap<String, BTreeMap<String, Param>>,
    pub units: Vec<&'a Unit>,
    pub rules: Vec<Rule>,
    pub infos: Vec<Option<RuleInfo>>,
    pub resolution: Resolution,
    pub mode: Option<DecisionMode>,
    pub env_name: String,
    /// Unit resolutions by unit name.
    pub unit_res: HashMap<String, Resolution>,
    /// The environment or a used library is missing from the workspace (U
    /// reported once at unit level): names that fail to resolve are then not
    /// reported per atom, since they may well live in the missing unit.
    pub scope_incomplete: bool,
    /// Environments of used libraries that differ from the root unit's (E
    /// reported once at unit level): their primitives resolve to nothing,
    /// silently.
    pub foreign_envs: HashSet<String>,
    /// Relations declared with a reserved name (reported once at the
    /// declaration; rules mentioning them are not judged).
    pub reserved_declared: HashSet<String>,
    scc_order: Option<Sccs>,
    complete_set: BTreeSet<String>,
}

impl<'a> Checker<'a> {
    fn new(ws: &'a Workspace, root: &'a Unit) -> Checker<'a> {
        Checker {
            ws,
            root,
            diags: vec![],
            relations: BTreeMap::new(),
            params: BTreeMap::new(),
            units: vec![],
            rules: vec![],
            infos: vec![],
            resolution: Resolution::D1,
            mode: None,
            env_name: String::new(),
            unit_res: HashMap::new(),
            scope_incomplete: false,
            foreign_envs: HashSet::new(),
            reserved_declared: HashSet::new(),
            scc_order: None,
            complete_set: BTreeSet::new(),
        }
    }

    pub fn diag(&mut self, code: Code, unit: &str, rule: Option<String>, span: Span, message: impl Into<String>) {
        let severity = if matches!(code, Code::W1 | Code::W2 | Code::W3) { Severity::Warning } else { Severity::Error };
        self.diags.push(Diagnostic {
            code,
            severity,
            unit: unit.to_string(),
            rule,
            span,
            message: message.into(),
        });
    }

    /// Build the scope: environment primitives, kernel relations, library and
    /// strategy declarations, parameters and rules.
    fn resolve(&mut self) {
        let root = self.root;
        let root_name = root.name.clone();
        self.unique_in_workspace(root.kind, &root.name, &root_name, None);
        self.redeclarations(root);
        match root.resolution {
            Some((r, _)) => self.resolution = r,
            None => self.diag(
                Code::X,
                &root_name,
                None,
                root.span,
                format!("{} `{}` declares no resolution; add `resolution @1d` (or another of @1m, @5m, @15m, @30m, @1h)", root.kind, root.name),
            ),
        }
        if root.kind == UnitKind::Strategy {
            match root.mode {
                Some((m, _)) => self.mode = Some(m),
                None => self.diag(Code::C, &root_name, None, root.span, "strategy declares no decision mode; add `mode delta` or `mode target`"),
            }
        }
        // Environment.
        match &root.env {
            Some((e, sp)) => match self.ws.find(UnitKind::Environment, e) {
                Some(env) => {
                    self.unique_in_workspace(UnitKind::Environment, e, &root_name, Some(*sp));
                    self.env_name = env.name.clone();
                    for sig in &env.rels {
                        self.declare_named(sig.clone(), &root_name);
                    }
                }
                None => {
                    self.scope_incomplete = true;
                    self.diag(Code::U, &root_name, None, *sp, format!("environment `{}` is not in the workspace", e));
                }
            },
            None => {
                self.scope_incomplete = true;
                self.diag(Code::U, &root_name, None, root.span, format!("{} `{}` names no environment; add `env <name>`", root.kind, root.name));
            }
        }
        // Kernel relations at the decision resolution.
        for sig in kernel_relations(self.resolution) {
            self.declare(sig, &root_name);
        }
        // Libraries in `uses` order, then the root unit.
        let mut units: Vec<&'a Unit> = Vec::new();
        for (l, sp) in &root.uses {
            match self.ws.find(UnitKind::Library, l) {
                Some(lib) => {
                    if units.iter().any(|u| u.name == lib.name) {
                        self.diag(Code::U, &root_name, None, *sp, format!("library `{}` is used twice", l));
                        continue;
                    }
                    self.unique_in_workspace(UnitKind::Library, l, &root_name, Some(*sp));
                    units.push(lib);
                }
                None => {
                    self.scope_incomplete = true;
                    self.diag(Code::U, &root_name, None, *sp, format!("library `{}` is not in the workspace", l));
                }
            }
        }
        units.push(root);
        for u in &units {
            let ures = match u.resolution {
                Some((r, _)) => r,
                None => {
                    if u.name != root.name {
                        self.diag(Code::X, &u.name, None, u.span, format!("library `{}` declares no resolution", u.name));
                    }
                    self.resolution
                }
            };
            self.unit_res.insert(u.name.clone(), ures);
            if u.name != root.name {
                self.redeclarations(u);
            }
            // A library is written against one environment (section 4) and
            // is usable only by a unit on that environment: a mismatch is one
            // E at unit level, and the library's primitives are then not
            // reported again rule by rule.
            if u.kind == UnitKind::Library {
                match &u.env {
                    Some((e, sp)) if *e != self.env_name => {
                        if self.ws.find(UnitKind::Environment, e).is_none() {
                            self.scope_incomplete = true;
                            self.diag(Code::U, &u.name, None, *sp, format!("environment `{}` is not in the workspace", e));
                        } else if !self.scope_incomplete {
                            self.foreign_envs.insert(e.clone());
                            let uses_span = root.uses.iter().find(|(l, _)| *l == u.name).map(|(_, sp)| *sp).unwrap_or(root.span);
                            self.diag(
                                Code::E,
                                &root_name,
                                None,
                                uses_span,
                                format!(
                                    "library `{}` is written against environment `{}`, which is not the declared environment `{}` of {} `{}`; a library can only be used on its own environment",
                                    u.name, e, self.env_name, root.kind, root.name
                                ),
                            );
                        }
                    }
                    None => {
                        self.scope_incomplete = true;
                        self.diag(Code::U, &u.name, None, u.span, format!("library `{}` names no environment; add `env <name>`", u.name));
                    }
                    _ => {}
                }
            }
            for sig in &u.rels {
                let mut sig = sig.clone();
                if sig.res.is_none() {
                    sig.res = Some(ures);
                }
                self.declare_named(sig, &u.name);
            }
            let mut pm = BTreeMap::new();
            for p in &u.params {
                if pm.contains_key(&p.name) {
                    self.diag(Code::U, &u.name, None, p.span, format!("parameter `{}` is declared twice", p.name));
                }
                if !types::compat(&p.ty, &p.value.ty()) {
                    self.diag(
                        Code::T,
                        &u.name,
                        None,
                        p.span,
                        format!("parameter `{}` is declared {} but its default is {}", p.name, p.ty, p.value.ty()),
                    );
                }
                if let Some((lo, hi)) = &p.range {
                    if !types::compat(&p.ty, &lo.ty()) || !types::compat(&p.ty, &hi.ty()) {
                        self.diag(Code::T, &u.name, None, p.span, format!("range of parameter `{}` must be {}", p.name, p.ty));
                    } else if lit_order(lo, hi) == Some(std::cmp::Ordering::Greater) {
                        self.diag(
                            Code::T,
                            &u.name,
                            None,
                            p.span,
                            format!("range of parameter `{}` is inverted: `{}..{}` has its lower bound above its upper bound", p.name, lo, hi),
                        );
                    } else if lit_order(lo, &p.value) == Some(std::cmp::Ordering::Greater) || lit_order(&p.value, hi) == Some(std::cmp::Ordering::Greater) {
                        self.diag(
                            Code::T,
                            &u.name,
                            None,
                            p.span,
                            format!("default of parameter `{}` is `{}`, outside its declared range `{}..{}`", p.name, p.value, lo, hi),
                        );
                    }
                }
                pm.insert(p.name.clone(), p.clone());
            }
            self.params.insert(u.name.clone(), pm);
            for r in &u.rules {
                self.rules.push(r.clone());
            }
        }
        self.units = units;
    }

    /// A header line (`env`, `resolution`, `mode`) written twice in one unit
    /// (adv-01): the first stays in effect, every later one is an error of
    /// the judgment that owns the declaration (U, X, C).
    fn redeclarations(&mut self, u: &Unit) {
        for (what, sp) in &u.redeclared {
            let (code, first) = match what.as_str() {
                "mode" => (Code::C, u.mode.map(|(m, s)| format!("`mode {}` at {}", m, s))),
                "resolution" => (Code::X, u.resolution.map(|(r, s)| format!("`resolution {}` at {}", r, s))),
                _ => (Code::U, u.env.as_ref().map(|(e, s)| format!("`env {}` at {}", e, s))),
            };
            self.diag(
                code,
                &u.name,
                None,
                *sp,
                format!(
                    "`{}` is declared twice in {} `{}`; {} stays in effect, remove this line",
                    what,
                    u.kind,
                    u.name,
                    first.unwrap_or_default()
                ),
            );
        }
    }

    /// Two units with one (kind, name) in the workspace (adv-02): a U error
    /// naming every declaration, reported at `span` or, for the root unit
    /// itself, at its last declaration.
    fn unique_in_workspace(&mut self, kind: UnitKind, name: &str, reporting_unit: &str, span: Option<Span>) {
        let spans: Vec<Span> = self.ws.units.iter().filter(|u| u.kind == kind && u.name == name).map(|u| u.span).collect();
        if spans.len() < 2 {
            return;
        }
        let at: Vec<String> = spans.iter().map(|s| s.to_string()).collect();
        let span = span.unwrap_or(*spans.last().unwrap());
        self.diag(
            Code::U,
            reporting_unit,
            None,
            span,
            format!("{} `{}` is declared twice in the workspace (at {}); rename or remove one", kind, name, at.join(" and ")),
        );
    }

    /// Declare a user-named relation (a primitive or a `rel`): a reserved
    /// name is a U error (adv-16) and the relation is not declared.
    fn declare_named(&mut self, sig: Signature, reporting_unit: &str) {
        if is_reserved(&sig.name) {
            let unit = match &sig.kind {
                Kind::Derived { unit } => unit.clone(),
                _ => reporting_unit.to_string(),
            };
            self.diag(
                Code::U,
                &unit,
                None,
                sig.span,
                format!("`{}` is a builtin or keyword and cannot be declared as a relation; choose another name", sig.name),
            );
            self.reserved_declared.insert(sig.name);
            return;
        }
        self.declare(sig, reporting_unit);
    }

    fn declare(&mut self, sig: Signature, reporting_unit: &str) {
        if let Some(prev) = self.relations.get(&sig.name) {
            let where_ = match &prev.kind {
                Kind::Primitive { env } => format!("a primitive of environment `{}`", env),
                Kind::Executor | Kind::KernelState | Kind::Output => "kernel-supplied".to_string(),
                Kind::Derived { unit } => format!("declared in `{}`", unit),
            };
            let unit = match &sig.kind {
                Kind::Derived { unit } => unit.clone(),
                _ => reporting_unit.to_string(),
            };
            self.diag(Code::U, &unit, None, sig.span, format!("relation `{}` is already {}", sig.name, where_));
            return;
        }
        self.relations.insert(sig.name.clone(), sig);
    }

    fn check_rules(&mut self) {
        let rules = self.rules.clone();
        for (i, r) in rules.iter().enumerate() {
            let info = rule::analyze(self, i, r);
            self.infos.push(info);
        }
    }

    /// Program-level judgments: R, N, S, Z, W1, W2, plus diagnostic ordering.
    fn program_checks(&mut self) {
        let root_name = self.root.name.clone();
        // WF-9 Z: at least one decide rule.
        if self.root.kind == UnitKind::Strategy && !self.rules.iter().any(|r| r.unit == root_name && r.head.name == "decide") {
            self.diag(Code::Z, &root_name, None, self.root.span, "strategy has no decide rule");
        }
        if self.root.kind == UnitKind::Library {
            let rules = self.rules.clone();
            for (i, r) in rules.iter().enumerate() {
                if r.head.name == "decide" {
                    let label = rule_label(&rules, i);
                    self.diag(Code::C, &r.unit, Some(label), r.span, "a library may not contain decide rules");
                }
            }
        }
        // Dependency graph.
        let infos: Vec<(usize, RuleInfo)> = self.infos.iter().enumerate().filter_map(|(i, o)| o.clone().map(|x| (i, x))).collect();
        let mut edges: Vec<(String, String, Polarity, Span, usize)> = Vec::new();
        for (i, info) in &infos {
            for r in &info.refs {
                edges.push((info.head.clone(), r.name.clone(), r.polarity, r.span, *i));
            }
        }
        let nodes: Vec<String> = self.relations.keys().cloned().collect();
        let index: HashMap<&str, usize> = nodes.iter().enumerate().map(|(i, n)| (n.as_str(), i)).collect();
        let n = nodes.len();
        // All-edge SCCs for stratification (WF-8).
        let mut adj_all = vec![vec![]; n];
        let mut adj_pos = vec![vec![]; n];
        for (h, b, pol, _, _) in &edges {
            if let (Some(&hi), Some(&bi)) = (index.get(h.as_str()), index.get(b.as_str())) {
                adj_all[hi].push(bi);
                if *pol == Polarity::Positive {
                    adj_pos[hi].push(bi);
                }
            }
        }
        let scc_all = tarjan(n, &adj_all);
        let scc_pos = tarjan(n, &adj_pos);
        // WF-8 S.
        for (h, b, pol, span, ri) in &edges {
            if *pol == Polarity::Positive {
                continue;
            }
            if let (Some(&hi), Some(&bi)) = (index.get(h.as_str()), index.get(b.as_str())) {
                if scc_all.id[hi] == scc_all.id[bi] {
                    let via = if *pol == Polarity::Negative { "negation" } else { "an aggregate" };
                    let label = rule_label(&self.rules, *ri);
                    let unit = self.rules[*ri].unit.clone();
                    self.diag(
                        Code::S,
                        &unit,
                        Some(label),
                        *span,
                        format!("`{}` depends on itself through {} (via `{}`); recursion through not or an aggregate is not stratifiable", h, via, b),
                    );
                }
            }
        }
        // WF-4 R: positive cycles must step strictly back in time.
        for (h, b, pol, span, ri) in &edges {
            if *pol != Polarity::Positive {
                continue;
            }
            if let (Some(&hi), Some(&bi)) = (index.get(h.as_str()), index.get(b.as_str())) {
                if scc_pos.id[hi] == scc_pos.id[bi] {
                    let info = &infos.iter().find(|(i, _)| i == ri).unwrap().1;
                    let r = info.refs.iter().find(|r| r.span == *span && r.name == *b).unwrap();
                    if !matches!(r.key_prov, TimeProv::Strict | TimeProv::Unknown) {
                        let label = rule_label(&self.rules, *ri);
                        let unit = self.rules[*ri].unit.clone();
                        self.diag(
                            Code::R,
                            &unit,
                            Some(label),
                            *span,
                            format!(
                                "`{}` is recursive through `{}` but this reference is not at a strictly earlier time; bind its temporal key through prev or lag",
                                h, b
                            ),
                        );
                    }
                }
            }
        }
        // WF-5 N: completeness fixpoint.
        let complete = self.completeness(&infos);
        for (i, info) in &infos {
            for r in &info.refs {
                if r.polarity == Polarity::Negative && !complete.contains(&r.name) {
                    let why = self.incomplete_reason(&r.name, &infos, &complete);
                    let label = rule_label(&self.rules, *i);
                    let unit = self.rules[*i].unit.clone();
                    self.diag(Code::N, &unit, Some(label), r.span, format!("`not {}` is not permitted: {}", r.name, why));
                }
            }
        }
        // W1: strategy-defined relations not reachable from decide.
        if self.root.kind == UnitKind::Strategy {
            let mut reach: HashSet<String> = HashSet::new();
            let mut stack = vec!["decide".to_string()];
            while let Some(x) = stack.pop() {
                if !reach.insert(x.clone()) {
                    continue;
                }
                for (h, b, _, _, _) in &edges {
                    if *h == x && !reach.contains(b) {
                        stack.push(b.clone());
                    }
                }
            }
            let decls: Vec<Signature> = self.root.rels.clone();
            for sig in decls {
                if !reach.contains(&sig.name) && !self.reserved_declared.contains(&sig.name) {
                    self.diag(Code::W1, &root_name, None, sig.span, format!("derived relation `{}` is not reached by any decide rule", sig.name));
                }
            }
            // WF-9 (C): a library has no mode, so a `decided` pattern written
            // in one is judged by each strategy that reaches the rule, against
            // that strategy's mode; the strategy's own patterns are judged in
            // `rule::analyze` (adv-18).
            if let Some(mode) = self.mode {
                let rules = self.rules.clone();
                for (i, r) in rules.iter().enumerate() {
                    if r.unit == root_name || !reach.contains(&r.head.name) {
                        continue;
                    }
                    for (c, t, sp) in decided_patterns(&r.body) {
                        if let Some(m) = ctor_mode(c).filter(|m| *m != mode) {
                            let label = rule_label(&rules, i);
                            self.diag(
                                Code::C,
                                &r.unit,
                                Some(label),
                                sp,
                                format!(
                                    "`{}` is a {} constructor but strategy `{}`, which reaches this rule, declares `mode {}`; `decided` holds only that strategy's own decisions, so `decided({}, {}(...))` can never match there",
                                    c, m, root_name, mode, t, c
                                ),
                            );
                        }
                    }
                }
            }
        }
        // W3: a declared relation that no rule defines is empty for ever, so
        // every rule reading it positively can never fire (adv-12).
        let decls: Vec<Signature> = self.root.rels.clone();
        for sig in decls {
            if self.reserved_declared.contains(&sig.name) || self.rules.iter().any(|r| r.head.name == sig.name) {
                continue;
            }
            self.diag(
                Code::W3,
                &root_name,
                None,
                sig.span,
                format!(
                    "derived relation `{}` is declared but no rule defines it; it is always empty, so every rule reading it positively can never fire",
                    sig.name
                ),
            );
        }
        // W2: unused parameters.
        let mut used: HashSet<(String, String)> = HashSet::new();
        for (i, info) in &infos {
            for p in &info.params_used {
                used.insert((self.rules[*i].unit.clone(), p.clone()));
            }
        }
        let units: Vec<&Unit> = self.units.clone();
        for u in units {
            for p in &u.params {
                if !used.contains(&(u.name.clone(), p.name.clone())) {
                    self.diag(Code::W2, &u.name, None, p.span, format!("parameter `{}` is never referenced", p.name));
                }
            }
        }
        // Order diagnostics: errors before warnings; unit-level diagnostics
        // (a missing or mismatched environment or library, no decide) first,
        // since everything else may follow from them; then by dependency rank
        // of the rule's head (section 5, WF-6: "the first rule in dependency
        // order that fails"), then by position.
        let rank: HashMap<String, usize> = nodes.iter().enumerate().map(|(i, nm)| (nm.clone(), scc_all.rank[i])).collect();
        let rules = self.rules.clone();
        self.diags.sort_by_key(|d| {
            let r = d.rule.as_ref().map(|lbl| {
                rules
                    .iter()
                    .enumerate()
                    .find(|(i, _)| rule_label(&rules, *i) == *lbl)
                    .map(|(_, r)| rank.get(&r.head.name).copied().unwrap_or(usize::MAX))
                    .unwrap_or(usize::MAX)
            });
            (d.severity == Severity::Warning, r.is_some(), r.unwrap_or(0), d.unit.clone(), d.span.line, d.span.col)
        });
        self.scc_order = Some(scc_all);
        self.complete_set = complete;
    }

    fn completeness(&self, infos: &[(usize, RuleInfo)]) -> BTreeSet<String> {
        let mut complete: BTreeSet<String> = BTreeSet::new();
        for (name, sig) in &self.relations {
            match sig.kind {
                Kind::Primitive { .. } if sig.complete => {
                    complete.insert(name.clone());
                }
                Kind::Executor | Kind::KernelState | Kind::Output => {
                    complete.insert(name.clone());
                }
                _ => {}
            }
        }
        loop {
            let mut changed = false;
            for (name, sig) in &self.relations {
                if complete.contains(name) || !matches!(sig.kind, Kind::Derived { .. }) {
                    continue;
                }
                let rules: Vec<&RuleInfo> = infos.iter().filter(|(_, i)| i.head == *name).map(|(_, i)| i).collect();
                if rules.is_empty() {
                    continue;
                }
                let ok = rules.iter().all(|info| self.rule_complete(info, &complete));
                if ok {
                    complete.insert(name.clone());
                    changed = true;
                }
            }
            if !changed {
                break;
            }
        }
        complete
    }

    /// A rule is complete when every positive atom outside a `top` is complete;
    /// a `top` closes whatever it reduces (section 5, WF-5).
    fn rule_complete(&self, info: &RuleInfo, complete: &BTreeSet<String>) -> bool {
        info.refs.iter().filter(|r| r.polarity != Polarity::Negative && !r.is_top).all(|r| complete.contains(&r.name))
    }

    fn incomplete_reason(&self, name: &str, infos: &[(usize, RuleInfo)], complete: &BTreeSet<String>) -> String {
        let mut chain = vec![name.to_string()];
        let mut cur = name.to_string();
        let mut seen = HashSet::new();
        loop {
            if !seen.insert(cur.clone()) {
                break;
            }
            match self.relations.get(&cur).map(|s| &s.kind) {
                Some(Kind::Primitive { env }) => {
                    return format!(
                        "`{}` is incomplete: `{}` is a primitive of environment `{}` not declared complete, so a missing tuple is unknown, not false",
                        chain.join("` depends on `"),
                        cur,
                        env
                    );
                }
                Some(Kind::Derived { .. }) => {
                    let infos_for: Vec<&RuleInfo> = infos.iter().filter(|(_, i)| i.head == cur).map(|(_, i)| i).collect();
                    if infos_for.is_empty() {
                        return format!("`{}` has no rules and is not a complete primitive", cur);
                    }
                    let next = infos_for
                        .iter()
                        .flat_map(|i| i.refs.iter())
                        .find(|r| r.polarity != Polarity::Negative && !r.is_top && !complete.contains(&r.name));
                    match next {
                        Some(r) => {
                            cur = r.name.clone();
                            chain.push(cur.clone());
                        }
                        None => return format!("`{}` is not complete", cur),
                    }
                }
                _ => return format!("`{}` is not declared", cur),
            }
        }
        format!("`{}` is not complete", chain.join("` depends on `"))
    }

    fn finish(self) -> (Option<Program>, Vec<Diagnostic>) {
        let has_error = self.diags.iter().any(|d| d.severity == Severity::Error);
        if has_error || self.root.kind != UnitKind::Strategy {
            return (None, self.diags);
        }
        let scc = self.scc_order.expect("program checks ran");
        let nodes: Vec<String> = self.relations.keys().cloned().collect();
        // Strata: SCCs in dependency order (dependencies first).
        let mut by_rank: BTreeMap<usize, Vec<String>> = BTreeMap::new();
        for (i, nm) in nodes.iter().enumerate() {
            by_rank.entry(scc.rank[i]).or_default().push(nm.clone());
        }
        let strata: Vec<Vec<String>> = by_rank.into_values().collect();
        let infos: Vec<RuleInfo> = self.infos.into_iter().map(|i| i.expect("no errors means every rule analysed")).collect();
        let program = Program {
            strategy: self.root.name.clone(),
            environment: self.env_name,
            resolution: self.resolution,
            mode: self.mode.expect("strategy mode"),
            relations: self.relations,
            params: self.params,
            rules: self.rules,
            infos,
            strata,
            complete: self.complete_set,
        };
        (Some(program), self.diags)
    }
}

/// Tarjan's SCC algorithm. `id[v]` is the component of `v`; `rank[v]` orders
/// components so that every edge points to an equal or lower rank
/// (dependencies evaluate first).
pub struct Sccs {
    pub id: Vec<usize>,
    pub rank: Vec<usize>,
}

pub fn tarjan(n: usize, adj: &[Vec<usize>]) -> Sccs {
    struct St<'a> {
        adj: &'a [Vec<usize>],
        index: Vec<Option<usize>>,
        low: Vec<usize>,
        on: Vec<bool>,
        stack: Vec<usize>,
        next: usize,
        id: Vec<usize>,
        count: usize,
    }
    fn strong(s: &mut St, v: usize) {
        s.index[v] = Some(s.next);
        s.low[v] = s.next;
        s.next += 1;
        s.stack.push(v);
        s.on[v] = true;
        for &w in &s.adj[v].to_vec() {
            if s.index[w].is_none() {
                strong(s, w);
                s.low[v] = s.low[v].min(s.low[w]);
            } else if s.on[w] {
                s.low[v] = s.low[v].min(s.index[w].unwrap());
            }
        }
        if s.low[v] == s.index[v].unwrap() {
            loop {
                let w = s.stack.pop().unwrap();
                s.on[w] = false;
                s.id[w] = s.count;
                if w == v {
                    break;
                }
            }
            s.count += 1;
        }
    }
    let mut s = St {
        adj,
        index: vec![None; n],
        low: vec![0; n],
        on: vec![false; n],
        stack: vec![],
        next: 0,
        id: vec![0; n],
        count: 0,
    };
    for v in 0..n {
        if s.index[v].is_none() {
            strong(&mut s, v);
        }
    }
    // Tarjan emits components in reverse topological order of the condensation
    // (a component is emitted after everything it reaches), so the component
    // id already orders dependencies first.
    let rank = s.id.clone();
    Sccs { id: s.id, rank }
}
