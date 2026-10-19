//! The elaborator: links a strategy against the class catalog and its
//! libraries. Library relations are templates (language-v2, section 7): a
//! call `mean_window[lookback, 12, close]` instantiates the template with its
//! bracketed inputs substituted (F-11.1) at the calendar of the key it is
//! called with (3.5), so the linked program is first order and every
//! judgment applies to it unchanged. Bar views (3.3), the per-calendar
//! universes (7.1), the instruments table (3.7), the series and catalog
//! reads (3.4) and the clock (3.5) are generated here.

use std::collections::{HashMap, HashSet, VecDeque};

use super::{Checker, Code, Template};
use crate::ir::*;
use crate::syntax::ast;

/// A resolved bracketed argument: a value (a literal or a parameter) or a
/// relation reference, fully substituted, independent of the caller's
/// variables.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum RArg {
    Val(Expr),
    Rel(RRel),
}

/// A relation reference: a concrete relation, or a template with its
/// arguments, with an explicit calendar when written (`R@C`).
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct RRel {
    pub name: String,
    pub args: Vec<RArg>,
    pub cal: Option<Cal>,
    /// The name is an instance already linked (a derived relation
    /// substituted for a template's relation input), not a template's.
    pub instance: bool,
}

impl RRel {
    /// The textual form that names an instance: `name[args]`.
    pub fn render(&self) -> String {
        if self.args.is_empty() {
            return self.name.clone();
        }
        let args: Vec<String> = self
            .args
            .iter()
            .map(|a| match a {
                RArg::Val(e) => e.to_string(),
                RArg::Rel(r) => r.render(),
            })
            .collect();
        format!("{}[{}]", self.name, args.join(", "))
    }
}

/// The substitution of a template instance: value inputs to expressions,
/// relation inputs to references.
#[derive(Clone, Debug, Default)]
pub(crate) struct Subst {
    pub vals: HashMap<String, Expr>,
    pub rels: HashMap<String, RRel>,
}

/// A rule awaiting elaboration.
#[derive(Clone, Debug)]
pub(crate) struct Job {
    pub unit: String,
    pub rule: ast::Rule,
    pub cal: Cal,
    pub subst: Subst,
    /// The instance the head defines.
    pub head: String,
    /// The rule is a decide rule / a universe rule.
    pub kind: JobKind,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum JobKind {
    Rule,
    Decide,
    Universe,
}

/// How an atom of a primitive is read at a calendar other than its own.
#[derive(Clone, Debug, PartialEq)]
enum Read {
    Plain,
    /// The bar view of a coarser calendar: the latest key in the bucket.
    Bucket(Cal),
    /// The as-of read of a coarser relation at a finer bar.
    AsOf,
}

#[derive(Clone, Debug)]
struct Resolved {
    name: String,
    cal: Cal,
    read: Read,
}

const KERNEL_RELS: &[&str] = &[
    "position",
    "cash",
    "nav",
    "capital",
    "mark",
    "fill",
    "decided",
    "decide",
    "clock",
    "spec",
    "chain",
    "greeks",
    "margin_req",
    "margin_by",
    "contract",
];
const BAR_PRIMS: &[&str] = &["open", "high", "low", "close", "volume"];

fn is_var(name: &str) -> bool {
    name.chars().next().map(|c| c.is_uppercase()).unwrap_or(false)
}

fn builtin_hint(name: &str) -> Option<String> {
    Some(match name {
        "month_start" => "the idiom is a derived relation over the builtin, `mstart(T) :- bar(T), month_start(T).`, which is closed and can be negated".to_string(),
        "day_start" => "the idiom is a derived relation over the builtin, `dstart(T) :- bar(T), day_start(T).`, which is closed and can be negated".to_string(),
        "prev" | "lag" | "session_of" => format!("`{}` binds a time; write it positively and test what it binds", name),
        _ => return None,
    })
}

impl<'a> Checker<'a> {
    /// Build the scope and elaborate every rule: the kernel relations, the
    /// strategy's own relations, the universe block, the rules and the
    /// decide rules, then every template instance and view they demand.
    pub(crate) fn link(&mut self) {
        let root = self.root;
        let root_name = root.name.clone();
        for sig in kernel_relations(&self.decision_cal) {
            self.relations.insert(sig.name.clone(), sig);
        }
        if self.class_name == "option" {
            for sig in crate::ir::option_relations(&self.decision_cal) {
                self.relations.insert(sig.name.clone(), sig);
            }
        }
        if self.class_name == "future" {
            for sig in crate::ir::future_relations(&self.decision_cal) {
                self.relations.insert(sig.name.clone(), sig);
            }
        }
        let by_instrument = self.exec.as_ref().is_some_and(|x| !x.margin.is_empty());
        for sig in crate::ir::margin_relations(&self.decision_cal, by_instrument) {
            self.relations.insert(sig.name.clone(), sig);
        }
        if self.clock_offset.is_some() {
            self.relations.insert(
                "clock".into(),
                Signature {
                    name: "clock".into(),
                    args: vec![
                        Arg {
                            name: "A".into(),
                            mode: Mode::Out,
                            ty: Ty::Entity,
                        },
                        Arg {
                            name: "T".into(),
                            mode: Mode::Key,
                            ty: Ty::Timestamp,
                        },
                    ],
                    cal: self.decision_cal.clone(),
                    complete: true,
                    kind: Kind::KernelState,
                    span: Span::default(),
                    native: None,
                },
            );
        }
        // The universe: the entity domain, complete by definition.
        self.relations.insert(
            "universe".into(),
            Signature {
                name: "universe".into(),
                args: vec![
                    Arg {
                        name: "A".into(),
                        mode: Mode::Out,
                        ty: Ty::Entity,
                    },
                    Arg {
                        name: "T".into(),
                        mode: Mode::Key,
                        ty: Ty::Timestamp,
                    },
                ],
                cal: self.decision_cal.clone(),
                complete: true,
                kind: Kind::Derived { unit: root_name.clone() },
                span: root.universe.as_ref().map(|u| u.span).unwrap_or_default(),
                native: None,
            },
        );
        self.cals_used.insert(self.decision_cal.clone());
        // The instruments table: a derived relation keyed at the decision
        // calendar with one fact per row.
        if !self.instruments.is_empty() {
            self.relations.insert(
                "instruments".into(),
                Signature {
                    name: "instruments".into(),
                    args: vec![
                        Arg {
                            name: "A".into(),
                            mode: Mode::Out,
                            ty: Ty::Entity,
                        },
                        Arg {
                            name: "T".into(),
                            mode: Mode::Key,
                            ty: Ty::Timestamp,
                        },
                        Arg {
                            name: "From".into(),
                            mode: Mode::Out,
                            ty: Ty::Timestamp,
                        },
                    ],
                    cal: self.decision_cal.clone(),
                    complete: true,
                    kind: Kind::Derived { unit: root_name.clone() },
                    span: root.universe.as_ref().and_then(|u| u.instruments.as_ref().map(|i| i.1)).unwrap_or_default(),
                    native: None,
                },
            );
            let rows = self.instruments.clone();
            let sp = root.universe.as_ref().and_then(|u| u.instruments.as_ref().map(|i| i.1)).unwrap_or_default();
            for (name, from) in rows {
                self.rules.push(Rule {
                    head: Atom {
                        name: "instruments".into(),
                        terms: vec![Term::Lit(Lit::Entity(name.clone()), sp), Term::Var("T".into(), sp), Term::Lit(Lit::Time(from.unwrap_or(i64::MIN)), sp)],
                        span: sp,
                    },
                    body: vec![],
                    span: sp,
                    unit: root_name.clone(),
                });
                self.var_cals.push(HashMap::from([("T".to_string(), self.decision_cal.clone())]));
            }
        }
        // The strategy's declarations: templates with their calendars.
        let decls: Vec<ast::RelDecl> = root.rules.as_ref().map(|r| r.rels.clone()).unwrap_or_default();
        let rules: Vec<ast::Rule> = root.rules.as_ref().map(|r| r.rules.clone()).unwrap_or_default();
        for decl in &decls {
            if is_reserved_relation(&decl.name, !decl.inputs.is_empty()) {
                self.diag(
                    Code::U,
                    &root_name,
                    None,
                    decl.span,
                    format!("`{}` is a builtin or keyword and cannot be declared as a relation; choose another name", decl.name),
                );
                self.reserved_declared.insert(decl.name.clone());
                continue;
            }
            if self.strategy_rels.contains_key(&decl.name) {
                self.diag(Code::U, &root_name, None, decl.span, format!("relation `{}` is declared twice in `{}`", decl.name, root_name));
                continue;
            }
            if self.relations.contains_key(&decl.name) {
                let what = match self.relations[&decl.name].kind {
                    Kind::Primitive { .. } => format!("a primitive of class `{}`", self.class_name),
                    _ => "kernel-supplied".to_string(),
                };
                self.diag(Code::U, &root_name, None, decl.span, format!("relation `{}` is {} and cannot be declared", decl.name, what));
                continue;
            }
            // The calendars: the key's annotation, or the decision calendar.
            let mut cals: Vec<Cal> = Vec::new();
            if let Some(key) = decl.args.iter().find(|a| a.key) {
                for (c, csp) in &key.cals {
                    match self.calendars.get(c) {
                        Some(cal) => cals.push(cal.clone()),
                        None => {
                            let known: Vec<&str> = self.calendars.keys().map(|k| k.as_str()).collect();
                            self.diag(
                                Code::X,
                                &root_name,
                                None,
                                *csp,
                                format!(
                                    "`@{}: {}` names a calendar the `calendars` block does not declare and the class does not provide (declared: {})",
                                    key.name,
                                    c,
                                    known.join(", ")
                                ),
                            );
                        }
                    }
                }
            }
            if cals.is_empty() {
                cals.push(self.decision_cal.clone());
            }
            let own_rules: Vec<ast::Rule> = rules.iter().filter(|r| r.head.name == decl.name).cloned().collect();
            self.strategy_rels.insert(decl.name.clone(), super::StrategyRel { cals });
            self.strategy_templates.insert(
                decl.name.clone(),
                Template {
                    lib: root_name.clone(),
                    decl: decl.clone(),
                    rules: own_rules,
                },
            );
        }
        for r in &rules {
            if !decls.iter().any(|d| d.name == r.head.name) && !self.reserved_declared.contains(&r.head.name) {
                let hint = if self.relations.contains_key(&r.head.name) {
                    "it is supplied by the class, the kernel or the universe block and cannot be defined by a rule".to_string()
                } else if self.templates.contains_key(&r.head.name) {
                    format!("rules for it belong in library `{}`", self.templates[&r.head.name].lib)
                } else {
                    format!("declare it with `rel {}(...)` in the rules block", r.head.name)
                };
                self.diag(
                    Code::U,
                    &root_name,
                    None,
                    r.span,
                    format!("rule for `{}`, which `{}` does not declare; {}", r.head.name, root_name, hint),
                );
            }
        }
        // Instantiate every strategy relation without inputs on each of its
        // calendars, in written order: these are the program's own rules.
        for decl in &decls {
            if self.reserved_declared.contains(&decl.name) || !decl.inputs.is_empty() {
                continue;
            }
            let Some(sr) = self.strategy_rels.get(&decl.name).cloned() else { continue };
            for cal in &sr.cals {
                let rrel = RRel {
                    name: decl.name.clone(),
                    args: vec![],
                    cal: None,
                    instance: false,
                };
                self.resolve_rrel(&rrel, cal, decl.span, &root_name, None);
            }
        }
        // Universe rules.
        if let Some(u) = &root.universe {
            if u.rules.is_empty() {
                self.diag(Code::P, &root_name, None, u.span, "the universe block has no rule for `universe(A, T)`; the entity domain is explicit");
            }
            for r in &u.rules {
                if r.head.name != "universe" {
                    self.diag(Code::U, &root_name, None, r.span, format!("the universe block defines `universe(A, T)` only, not `{}`", r.head.name));
                    continue;
                }
                self.pending.push_back(Job {
                    unit: root_name.clone(),
                    rule: r.clone(),
                    cal: self.decision_cal.clone(),
                    subst: Subst::default(),
                    head: "universe".into(),
                    kind: JobKind::Universe,
                });
            }
        }
        // Decide rules.
        if let Some(d) = &root.decisions {
            for r in &d.rules {
                if r.head.name != "decide" {
                    self.diag(
                        Code::U,
                        &root_name,
                        None,
                        r.span,
                        format!("the decisions block holds decide rules only, not rules for `{}`", r.head.name),
                    );
                    continue;
                }
                self.pending.push_back(Job {
                    unit: root_name.clone(),
                    rule: r.clone(),
                    cal: self.decision_cal.clone(),
                    subst: Subst::default(),
                    head: "decide".into(),
                    kind: JobKind::Decide,
                });
            }
        }
        // The execution block's price relation.
        if let Some(exec) = self.exec.clone() {
            let cal = match &exec.price_cal {
                Some(c) => self.calendars.get(c).cloned().unwrap_or(self.decision_cal.clone()),
                None => self.decision_cal.clone(),
            };
            // A bar primitive at a coarser decision calendar is its bar view there.
            let rrel = RRel {
                name: exec.price.clone(),
                args: vec![],
                cal: if cal != Cal::Base && BAR_PRIMS.contains(&exec.price.as_str()) { Some(cal.clone()) } else { None },
                instance: false,
            };
            let sp = root.span;
            if let Some(res) = self.resolve_rrel(&rrel, &cal, sp, &root_name, None) {
                let sig = self.relations.get(&res.name).cloned();
                let price_ok = sig
                    .map(|s| s.args.iter().any(|a| matches!(&a.ty, Ty::Quantity(d) if d.is_price())) && s.args.iter().any(|a| a.ty.is_entity()))
                    .unwrap_or(false);
                if !price_ok {
                    self.diag(
                        Code::P,
                        &root_name,
                        None,
                        sp,
                        format!("the execution block's price relation `{}` has no Price<...> value or no entity argument", exec.price),
                    );
                }
                if let Some(x) = self.exec.as_mut() {
                    x.price_instance = res.name;
                }
            }
        }
        // What the option model reads whether or not a rule does (K-5.5):
        // the quotes it marks and fills at, and each chain's spot.
        if self.class_name == "option" {
            let mut read = vec!["quote".to_string()];
            read.extend(self.demand.chains.iter().map(|c| c.spot.clone()));
            for name in read {
                let rrel = RRel {
                    name,
                    args: vec![],
                    cal: None,
                    instance: false,
                };
                let dc = self.decision_cal.clone();
                let _ = self.resolve_rrel(&rrel, &dc, root.span, &root_name, None);
            }
        }
        self.run_jobs();
    }

    fn run_jobs(&mut self) {
        while let Some(job) = self.pending.pop_front() {
            if let Some((rule, cals)) = self.elaborate_rule(&job) {
                self.rules.push(rule);
                self.var_cals.push(cals);
            }
        }
    }

    /// The label the rule will have once pushed.
    fn next_label(&self, unit: &str, head: &str) -> String {
        let k = self.rules.iter().filter(|o| o.unit == unit && o.head.name == head).count() + 1;
        format!("{}::{}#{}", unit, head, k)
    }

    /// Resolve a relation reference at a calendar: the instance name it
    /// denotes, instantiating templates and generating views as needed.
    fn resolve_rrel(&mut self, rrel: &RRel, cal: &Cal, span: Span, unit: &str, label: Option<&str>) -> Option<Resolved> {
        let cal = rrel.cal.clone().unwrap_or(cal.clone());
        let name = rrel.name.as_str();
        let decision = self.decision_cal.clone();
        let err = |cx: &mut Checker, code: Code, msg: String| {
            cx.diag(code, unit, label.map(|l| l.to_string()), span, msg);
        };
        // An instance already linked: a derived relation substituted for a
        // template's relation input.
        if rrel.instance {
            let Some(sig) = self.relations.get(name).cloned() else {
                err(self, Code::U, format!("`{}` is not a relation of this program", name));
                return None;
            };
            if sig.cal != cal && !(sig.cal.is_sessions() && cal.is_sessions()) {
                err(
                    self,
                    Code::X,
                    format!("`{}` is on calendar `{}` and is read here with a key on `{}`", name, self.cal_name(&sig.cal), self.cal_name(&cal)),
                );
                return None;
            }
            return Some(Resolved {
                name: name.to_string(),
                cal: sig.cal.clone(),
                read: Read::Plain,
            });
        }
        // Kernel relations.
        if KERNEL_RELS.contains(&name) {
            if !rrel.args.is_empty() {
                err(self, Code::T, format!("`{}` takes no bracketed inputs", name));
                return None;
            }
            if !self.relations.contains_key(name) {
                let why = match name {
                    "clock" => "the clock has no session offset",
                    "margin_by" => "the execution block lists no margin model (K-5.6)",
                    "contract" => "it is the future class's (K-2.4)",
                    _ => "it is the option class's (K-2.4)",
                };
                err(self, Code::U, format!("`{}` is not a relation of this program ({})", name, why));
                return None;
            }
            if cal != decision {
                err(
                    self,
                    Code::X,
                    format!(
                        "`{}` is supplied at the decision calendar `{}` and is read here with a key on `{}`; the book is read at the decision bar, where the kernel supplies it (relate the bar to its session with `session_of(A, T, S)`)",
                        name,
                        self.cal_name(&decision),
                        self.cal_name(&cal)
                    ),
                );
                return None;
            }
            return Some(Resolved {
                name: name.to_string(),
                cal,
                read: Read::Plain,
            });
        }
        if name == "universe" {
            if !rrel.args.is_empty() {
                err(self, Code::T, "`universe` takes no bracketed inputs".into());
                return None;
            }
            let inst = self.ensure_universe_at(&cal);
            return Some(Resolved { name: inst, cal, read: Read::Plain });
        }
        if name == "instruments" {
            if self.instruments.is_empty() {
                err(self, Code::U, "`instruments` is the table of the universe block, which this strategy does not write".into());
                return None;
            }
            if cal != decision {
                err(self, Code::X, "`instruments` is read at the decision calendar".into());
                return None;
            }
            return Some(Resolved {
                name: "instruments".into(),
                cal,
                read: Read::Plain,
            });
        }
        // `traded@C(A, T)`: the bar view that says A has a bar in T's bucket (3.3).
        if name == "traded" {
            if !rrel.args.is_empty() {
                err(self, Code::T, "`traded` takes no bracketed inputs".into());
                return None;
            }
            return match self.ensure_traded(&cal) {
                Some(inst) => Some(Resolved { name: inst, cal, read: Read::Plain }),
                None => {
                    if !self.demand_empty && !self.scope_incomplete {
                        err(
                            self,
                            Code::E,
                            format!("`traded@{}` reads the bars, which the data block does not ask for; add `need bars`", self.cal_name(&cal)),
                        );
                    }
                    None
                }
            };
        }
        // A primitive of the class (or the series class's `value`).
        {
            if let Some(decl) = self.class_decls.iter().find(|r| r.name == name).cloned() {
                if !rrel.args.is_empty() {
                    err(self, Code::T, format!("`{}` is a primitive and takes no bracketed inputs", name));
                    return None;
                }
                let class_name = if name == "value" { "series".to_string() } else { self.class_name.clone() };
                let Some(sig) = self.relations.get(name).cloned() else {
                    if !self.demand_empty {
                        let need = super::Demand::need_for(name);
                        err(
                            self,
                            Code::E,
                            format!(
                                "`{}` is a primitive of class `{}` that the demand did not ask for; add `need {}` to the data block",
                                name, class_name, need
                            ),
                        );
                    }
                    return None;
                };
                let _ = decl;
                // Bars: at base, or the bar view at a coarser calendar.
                if BAR_PRIMS.contains(&name) {
                    if cal == Cal::Base {
                        return Some(Resolved {
                            name: name.to_string(),
                            cal,
                            read: Read::Plain,
                        });
                    }
                    // In a library rule instantiated on a calendar, the bars
                    // are read at that calendar: the same rule on the
                    // session calendar reads the session's bars (7, F-13).
                    if rrel.cal.is_none() && unit == self.root.name {
                        err(
                            self,
                            Code::X,
                            format!(
                                "`{}` is at the base calendar and is read here with a key on `{}`; a base relation is read at a `{}` label only through its bar view, `{}@{}`",
                                name,
                                self.cal_name(&cal),
                                self.cal_name(&cal),
                                name,
                                self.cal_name(&cal)
                            ),
                        );
                        return None;
                    }
                    let inst = self.ensure_view(name, &cal);
                    return Some(Resolved { name: inst, cal, read: Read::Plain });
                }
                if name == "session" {
                    // `session(A, S, Open)` is keyed by the session label: the sessions calendar.
                    if !cal.is_sessions() {
                        err(
                            self,
                            Code::X,
                            format!(
                                "`session` is keyed by the session label, a `sessions` calendar; it is read here with a key on `{}`",
                                self.cal_name(&cal)
                            ),
                        );
                        return None;
                    }
                    return Some(Resolved {
                        name: name.to_string(),
                        cal,
                        read: Read::Plain,
                    });
                }
                // Catalog relations and series: at their native calendar,
                // as a bar view on a coarser one, as of the bar on a finer.
                let native: Cal = match sig.native {
                    Some(r) => r.as_calendar(),
                    None => Cal::Base,
                };
                if native == Cal::Base && (cal == Cal::Base || cal == self.base.as_calendar()) {
                    return Some(Resolved {
                        name: name.to_string(),
                        cal,
                        read: Read::Plain,
                    });
                }
                if native == cal {
                    // At its own calendar on a finer base: the tuple at the
                    // label, read as the latest in the label's bucket.
                    return Some(Resolved {
                        name: name.to_string(),
                        cal: cal.clone(),
                        read: Read::Bucket(cal),
                    });
                }
                if name == "value" {
                    // The direction depends on the series; decided per atom
                    // by the caller from the name literal. Default: as of.
                    return Some(Resolved {
                        name: name.to_string(),
                        cal,
                        read: Read::AsOf,
                    });
                }
                if native.refines(&cal) {
                    return Some(Resolved {
                        name: name.to_string(),
                        cal: cal.clone(),
                        read: Read::Bucket(cal),
                    });
                }
                return Some(Resolved {
                    name: name.to_string(),
                    cal,
                    read: Read::AsOf,
                });
            }
        }
        // A declared relation: the strategy's own (which shadow a library's
        // in its rules) or a library template.
        if let Some(tpl) = self.template_in_scope(name, unit).cloned() {
            return self.instantiate(&tpl, rrel, &cal, span, unit, label);
        }
        if let Some(hint) = builtin_hint(name) {
            err(self, Code::U, format!("`{}` is a temporal builtin, not a relation, so it does not resolve here; {}", name, hint));
            return None;
        }
        if self.scope_incomplete {
            return None;
        }
        // A template in a library the strategy does not use.
        for lib in self.ws.libraries() {
            if lib.rules.rels.iter().any(|d| d.name == name) && !self.libs.iter().any(|l| l.name == lib.name) {
                err(
                    self,
                    Code::U,
                    format!("`{}` is declared in library `{}`, which `{}` does not use; add `uses {}`", name, lib.name, self.root.name, lib.name),
                );
                return None;
            }
        }
        let libs: Vec<String> = self.libs.iter().map(|l| l.name.clone()).collect();
        let where_ = if libs.is_empty() { String::new() } else { format!(", its libraries ({})", libs.join(", ")) };
        err(
            self,
            Code::U,
            format!("relation `{}` is declared nowhere: not in `{}`{} or class `{}`", name, self.root.name, where_, self.class_name),
        );
        None
    }

    /// Instantiate a template at a calendar with resolved arguments: the
    /// instance's signature and its rules (enqueued once).
    fn instantiate(&mut self, tpl: &Template, rrel: &RRel, cal: &Cal, span: Span, unit: &str, label: Option<&str>) -> Option<Resolved> {
        let decision = self.decision_cal.clone();
        let strategy_rel = if tpl.lib == self.root.name { self.strategy_rels.get(&tpl.decl.name).cloned() } else { None };
        // Calendar admissibility.
        if let Some(sr) = &strategy_rel {
            if !sr.cals.contains(cal) {
                let declared: Vec<String> = sr.cals.iter().map(|c| self.cal_name(c)).collect();
                self.diag(
                    Code::X,
                    unit,
                    label.map(|l| l.to_string()),
                    span,
                    format!(
                        "`{}` is on calendar `{}` and is read here with a key on `{}`; a relation is read at its own calendar (declare `@T: {}` on it, or read it through a bar view or as of the bar)",
                        tpl.decl.name,
                        declared.join(" | "),
                        self.cal_name(cal),
                        self.cal_name(cal)
                    ),
                );
                return None;
            }
        }
        // Arguments against the inputs.
        if rrel.args.len() != tpl.decl.inputs.len() {
            self.diag(
                Code::T,
                unit,
                label.map(|l| l.to_string()),
                span,
                format!("`{}` takes {} bracketed input(s), {} given", tpl.decl.name, tpl.decl.inputs.len(), rrel.args.len()),
            );
            return None;
        }
        let mut subst = Subst::default();
        let mut dims: HashMap<String, Ty> = HashMap::new();
        let mut arg_sigs: Vec<Option<Signature>> = Vec::new();
        for (input, arg) in tpl.decl.inputs.iter().zip(&rrel.args) {
            match (&input.ty, arg) {
                (ast::InputTy::Value(tn), RArg::Val(e)) => {
                    let expected = Ty::parse(&tn.name, tn.arg.as_deref()).unwrap_or(Ty::DimVar(tn.name.clone()));
                    let actual = match e {
                        Expr::Lit(l, _) => Some(l.ty()),
                        Expr::Param(p, _) => self.params.get(p).map(|p| p.ty.clone()),
                        Expr::Neg(inner, _) => match &**inner {
                            Expr::Lit(l, _) => Some(l.ty()),
                            _ => None,
                        },
                        _ => None,
                    };
                    if let Some(actual) = actual {
                        if let Ty::DimVar(d) = &expected {
                            match dims.get(d) {
                                Some(bound) if !super::types::compat(bound, &actual) => {
                                    self.diag(
                                        Code::T,
                                        unit,
                                        label.map(|l| l.to_string()),
                                        span,
                                        format!("input `{}` of `{}` is {} ({}), but `{}` is {}", input.name, tpl.decl.name, d, bound, e, actual),
                                    );
                                    return None;
                                }
                                Some(_) => {}
                                None => {
                                    dims.insert(d.clone(), if actual == Ty::IntLit { Ty::scalar() } else { actual.clone() });
                                }
                            }
                        } else if !super::types::compat(&expected, &actual) {
                            self.diag(
                                Code::T,
                                unit,
                                label.map(|l| l.to_string()),
                                span,
                                format!("`{}` takes a {} in its bracket `{}`, and `{}` is {}", tpl.decl.name, expected, input.name, e, actual),
                            );
                            return None;
                        }
                    }
                    subst.vals.insert(input.name.clone(), e.clone());
                    arg_sigs.push(None);
                }
                (ast::InputTy::Rel(shape), RArg::Rel(r)) => {
                    // The relation argument is instantiated at the same calendar.
                    let res = self.resolve_rrel(r, cal, span, unit, label)?;
                    let sig = self.relations.get(&res.name).cloned()?;
                    // Unify the shape with the signature.
                    let mut shape_ok = sig.args.len() == shape.len();
                    if shape_ok {
                        for (s, a) in shape.iter().zip(&sig.args) {
                            if s.key {
                                shape_ok &= a.mode == Mode::Key;
                                continue;
                            }
                            let Some(tn) = &s.ty else { continue };
                            match Ty::parse(&tn.name, tn.arg.as_deref()) {
                                Some(t) => shape_ok &= super::types::compat(&t, &a.ty) || (t.is_entity() && a.ty.is_entity()),
                                None => {
                                    // A dimension variable.
                                    match dims.get(&tn.name) {
                                        Some(bound) => shape_ok &= bound == &a.ty,
                                        None => {
                                            dims.insert(tn.name.clone(), a.ty.clone());
                                        }
                                    }
                                }
                            }
                        }
                    }
                    if !shape_ok {
                        let want: Vec<String> = shape
                            .iter()
                            .map(|s| {
                                if s.key {
                                    format!("@{}", s.name)
                                } else {
                                    s.ty.as_ref().map(|t| t.name.clone()).unwrap_or_default()
                                }
                            })
                            .collect();
                        let got: Vec<String> = sig.args.iter().map(|a| if a.mode == Mode::Key { format!("@{}", a.name) } else { a.ty.to_string() }).collect();
                        self.diag(
                            Code::T,
                            unit,
                            label.map(|l| l.to_string()),
                            span,
                            format!(
                                "input `{}` of `{}` is a relation of the shape rel({}), and `{}` has the shape ({})",
                                input.name,
                                tpl.decl.name,
                                want.join(", "),
                                res.name,
                                got.join(", ")
                            ),
                        );
                        return None;
                    }
                    let mut rr = r.clone();
                    rr.name = res.name.clone();
                    rr.args = vec![];
                    // A derived instance is linked once and read as is; a
                    // primitive is read at the calendar of each use (its
                    // bar view at a coarser one).
                    rr.instance = !sig.is_primitive();
                    rr.cal = if rr.instance { Some(res.cal.clone()) } else { None };
                    subst.rels.insert(input.name.clone(), rr);
                    arg_sigs.push(Some(sig));
                }
                (ast::InputTy::Value(_), RArg::Rel(r)) => {
                    self.diag(
                        Code::T,
                        unit,
                        label.map(|l| l.to_string()),
                        span,
                        format!("input `{}` of `{}` is a value, and `{}` names a relation", input.name, tpl.decl.name, r.render()),
                    );
                    return None;
                }
                (ast::InputTy::Rel(_), RArg::Val(e)) => {
                    self.diag(
                        Code::T,
                        unit,
                        label.map(|l| l.to_string()),
                        span,
                        format!("input `{}` of `{}` is a relation, and `{}` is a value", input.name, tpl.decl.name, e),
                    );
                    return None;
                }
            }
        }
        // The instance name.
        let rendered = RRel {
            name: tpl.decl.name.clone(),
            args: rrel
                .args
                .iter()
                .zip(&subst_order(&tpl.decl, &subst))
                .map(|(a, s)| match (a, s) {
                    (RArg::Rel(_), Some(r)) => RArg::Rel(RRel {
                        name: r.clone(),
                        args: vec![],
                        cal: None,
                        instance: false,
                    }),
                    _ => a.clone(),
                })
                .collect(),
            cal: None,
            instance: false,
        }
        .render();
        // A library relation the strategy also declares is named by its library.
        let rendered = if strategy_rel.is_none() && self.strategy_rels.contains_key(&tpl.decl.name) {
            format!("{}.{}", tpl.lib, rendered)
        } else {
            rendered
        };
        let instance = match &strategy_rel {
            Some(sr) if sr.cals.len() == 1 => rendered,
            Some(_) => format!("{}@{}", rendered, self.cal_name(cal)),
            None if *cal == decision => rendered,
            None => format!("{}@{}", rendered, self.cal_name(cal)),
        };
        if self.relations.contains_key(&instance) {
            return Some(Resolved {
                name: instance,
                cal: cal.clone(),
                read: Read::Plain,
            });
        }
        // The signature, with dimension variables resolved.
        let mut sig = self.signature_of(&tpl.decl, &tpl.lib, cal.clone())?;
        sig.name = instance.clone();
        sig.complete = false;
        for a in sig.args.iter_mut() {
            if let Ty::DimVar(d) = &a.ty {
                match dims.get(d) {
                    Some(t) => a.ty = t.clone(),
                    None => {
                        self.diag(
                            Code::T,
                            &tpl.lib,
                            None,
                            tpl.decl.span,
                            format!("the dimension variable `{}` of `{}` is bound by no input", d, tpl.decl.name),
                        );
                        return None;
                    }
                }
            }
        }
        self.relations.insert(instance.clone(), sig);
        self.cals_used.insert(cal.clone());
        // Enqueue the template's rules.
        for r in &tpl.rules {
            // The head's bracket names are the inputs, in order.
            let names: Vec<String> = r
                .head
                .brackets
                .iter()
                .filter_map(|b| if let ast::BracketArg::Name(n, _) = b { Some(n.clone()) } else { None })
                .collect();
            let expected: Vec<String> = tpl.decl.inputs.iter().map(|i| i.name.clone()).collect();
            if names != expected {
                self.diag(
                    Code::U,
                    &tpl.lib,
                    None,
                    r.span,
                    format!("the head of a rule for `{}` writes its inputs as `[{}]`, as the declaration does", tpl.decl.name, expected.join(", ")),
                );
                continue;
            }
            self.pending.push_back(Job {
                unit: tpl.lib.clone(),
                rule: r.clone(),
                cal: cal.clone(),
                subst: subst.clone(),
                head: instance.clone(),
                kind: JobKind::Rule,
            });
        }
        Some(Resolved {
            name: instance,
            cal: cal.clone(),
            read: Read::Plain,
        })
    }

    /// The bar view of a bar primitive at a calendar (Definition 3.5).
    fn ensure_view(&mut self, prim: &str, cal: &Cal) -> String {
        let name = format!("{}@{}", prim, self.cal_name(cal));
        if self.relations.contains_key(&name) {
            return name;
        }
        let base = self.relations[prim].clone();
        let sig = Signature {
            name: name.clone(),
            cal: cal.clone(),
            kind: Kind::Derived { unit: "view".into() },
            native: None,
            ..base.clone()
        };
        self.relations.insert(name.clone(), sig);
        self.cals_used.insert(cal.clone());
        let sp = Span::default();
        let agg = match prim {
            "open" => "first",
            "high" => "max",
            "low" => "min",
            "close" => "last",
            _ => "sum",
        };
        // view(A, T, V) :- V = agg(X) over (T1 in bucket(C, T), prim(A, T1, X)).
        let rule = Rule {
            head: Atom {
                name: name.clone(),
                terms: vec![Term::Var("A".into(), sp), Term::Var("T".into(), sp), Term::Var("V".into(), sp)],
                span: sp,
            },
            body: vec![Literal::Agg {
                var: "V".into(),
                agg: agg.into(),
                args: vec![Expr::Var("X".into(), sp)],
                conj: vec![
                    Literal::Window {
                        var: Term::Var("T1".into(), sp),
                        kind: WindowKind::Bucket(cal.clone()),
                        base: Term::Var("T".into(), sp),
                        dur: Expr::Lit(Lit::Int(0), sp),
                        min: Expr::Lit(Lit::Int(1), sp),
                        lo: None,
                        span: sp,
                    },
                    Literal::Atom(Atom {
                        name: prim.into(),
                        terms: vec![Term::Var("A".into(), sp), Term::Var("T1".into(), sp), Term::Var("X".into(), sp)],
                        span: sp,
                    }),
                ],
                within: None,
                span: sp,
            }],
            span: sp,
            unit: "view".into(),
        };
        self.rules.push(rule);
        self.var_cals.push(HashMap::from([("T".to_string(), cal.clone()), ("T1".to_string(), Cal::Base)]));
        name
    }

    /// The view of a catalog relation or a series at another calendar: the
    /// latest tuple in the label's bucket on a coarser calendar, the tuple
    /// as of the bar on a finer one (3.4), as a relation of its own so
    /// that it may be negated when the relation is complete.
    fn ensure_catalog_view(&mut self, prim: &str, cal: &Cal, read: &Read) -> String {
        let name = format!("{}@{}", prim, self.cal_name(cal));
        if self.relations.contains_key(&name) {
            return name;
        }
        let base = self.relations[prim].clone();
        let sig = Signature {
            name: name.clone(),
            cal: cal.clone(),
            kind: Kind::Derived { unit: "view".into() },
            native: None,
            ..base.clone()
        };
        self.relations.insert(name.clone(), sig);
        self.cals_used.insert(cal.clone());
        let sp = Span::default();
        let key = base.key_pos().unwrap_or(0);
        let head_terms: Vec<Term> = base.args.iter().map(|a| Term::Var(a.name.clone(), sp)).collect();
        let mut body_terms = head_terms.clone();
        body_terms[key] = Term::Wild(sp);
        let at = head_terms[key].clone();
        self.rules.push(Rule {
            head: Atom {
                name: name.clone(),
                terms: head_terms,
                span: sp,
            },
            body: vec![Literal::AsOf {
                atom: Atom {
                    name: prim.into(),
                    terms: body_terms,
                    span: sp,
                },
                at,
                within_bucket: match read {
                    Read::Bucket(c) => Some(c.clone()),
                    _ => None,
                },
                span: sp,
            }],
            span: sp,
            unit: "view".into(),
        });
        self.var_cals.push(HashMap::from([(base.args[key].name.clone(), cal.clone())]));
        name
    }

    /// `traded@C(A, T)`: A has a bar in T's bucket (at base, a bar at T).
    fn ensure_traded(&mut self, cal: &Cal) -> Option<String> {
        let name = format!("traded@{}", self.cal_name(cal));
        if self.relations.contains_key(&name) {
            return Some(name);
        }
        if !self.relations.contains_key("close") {
            return None;
        }
        let sig = Signature {
            name: name.clone(),
            args: vec![
                Arg {
                    name: "A".into(),
                    mode: Mode::Out,
                    ty: Ty::Entity,
                },
                Arg {
                    name: "T".into(),
                    mode: Mode::Key,
                    ty: Ty::Timestamp,
                },
            ],
            cal: cal.clone(),
            complete: self.relations["close"].complete,
            kind: Kind::Derived { unit: "view".into() },
            span: Span::default(),
            native: None,
        };
        self.relations.insert(name.clone(), sig);
        self.cals_used.insert(cal.clone());
        let sp = Span::default();
        let body = if *cal == Cal::Base {
            vec![Literal::Atom(Atom {
                name: "close".into(),
                terms: vec![Term::Var("A".into(), sp), Term::Var("T".into(), sp), Term::Wild(sp)],
                span: sp,
            })]
        } else {
            vec![
                Literal::Agg {
                    var: "N".into(),
                    agg: "count".into(),
                    args: vec![Expr::Var("T1".into(), sp)],
                    conj: vec![
                        Literal::Window {
                            var: Term::Var("T1".into(), sp),
                            kind: WindowKind::Bucket(cal.clone()),
                            base: Term::Var("T".into(), sp),
                            dur: Expr::Lit(Lit::Int(0), sp),
                            min: Expr::Lit(Lit::Int(1), sp),
                            lo: None,
                            span: sp,
                        },
                        Literal::Atom(Atom {
                            name: "close".into(),
                            terms: vec![Term::Var("A".into(), sp), Term::Var("T1".into(), sp), Term::Wild(sp)],
                            span: sp,
                        }),
                    ],
                    within: None,
                    span: sp,
                },
                Literal::Cmp {
                    op: CmpOp::Gt,
                    lhs: Expr::Var("N".into(), sp),
                    rhs: Expr::Lit(Lit::Int(0), sp),
                    span: sp,
                },
            ]
        };
        self.rules.push(Rule {
            head: Atom {
                name: name.clone(),
                terms: vec![Term::Var("A".into(), sp), Term::Var("T".into(), sp)],
                span: sp,
            },
            body,
            span: sp,
            unit: "view".into(),
        });
        self.var_cals.push(HashMap::from([("T".to_string(), cal.clone()), ("T1".to_string(), Cal::Base)]));
        Some(name)
    }

    /// The universe at a calendar (7.1): the decision calendar's is the
    /// program's; a coarser calendar's labels are those whose bucket holds
    /// one of the entity's universe bars; a finer calendar's rows are the
    /// bars the entity traded.
    fn ensure_universe_at(&mut self, cal: &Cal) -> String {
        if *cal == self.decision_cal {
            return "universe".into();
        }
        let name = format!("universe@{}", self.cal_name(cal));
        if self.relations.contains_key(&name) {
            return name;
        }
        let sig = Signature {
            name: name.clone(),
            args: vec![
                Arg {
                    name: "A".into(),
                    mode: Mode::Out,
                    ty: Ty::Entity,
                },
                Arg {
                    name: "T".into(),
                    mode: Mode::Key,
                    ty: Ty::Timestamp,
                },
            ],
            cal: cal.clone(),
            complete: true,
            kind: Kind::Derived { unit: "view".into() },
            span: Span::default(),
            native: None,
        };
        self.relations.insert(name.clone(), sig);
        self.cals_used.insert(cal.clone());
        let sp = Span::default();
        let decision = self.decision_cal.clone();
        let body = if decision.refines(cal) && *cal != decision {
            // Coarser than the decision calendar: a bucket holds a universe bar.
            vec![
                Literal::Agg {
                    var: "N".into(),
                    agg: "count".into(),
                    args: vec![Expr::Var("T1".into(), sp)],
                    conj: vec![
                        Literal::Window {
                            var: Term::Var("T1".into(), sp),
                            kind: WindowKind::Bucket(cal.clone()),
                            base: Term::Var("T".into(), sp),
                            dur: Expr::Lit(Lit::Int(0), sp),
                            min: Expr::Lit(Lit::Int(1), sp),
                            lo: None,
                            span: sp,
                        },
                        Literal::Atom(Atom {
                            name: "universe".into(),
                            terms: vec![Term::Var("A".into(), sp), Term::Var("T1".into(), sp)],
                            span: sp,
                        }),
                    ],
                    within: None,
                    span: sp,
                },
                Literal::Cmp {
                    op: CmpOp::Gt,
                    lhs: Expr::Var("N".into(), sp),
                    rhs: Expr::Lit(Lit::Int(0), sp),
                    span: sp,
                },
            ]
        } else {
            // Finer (or incomparable): the bars the entity traded, within
            // the universe as of the bar.
            let traded = self.ensure_traded(cal);
            let mut body = Vec::new();
            if let Some(t) = traded {
                body.push(Literal::Atom(Atom {
                    name: t,
                    terms: vec![Term::Var("A".into(), sp), Term::Var("T".into(), sp)],
                    span: sp,
                }));
            }
            body.push(Literal::AsOf {
                atom: Atom {
                    name: "universe".into(),
                    terms: vec![Term::Var("A".into(), sp), Term::Wild(sp)],
                    span: sp,
                },
                at: Term::Var("T".into(), sp),
                within_bucket: None,
                span: sp,
            });
            body
        };
        let mut cals = HashMap::from([("T".to_string(), cal.clone())]);
        cals.insert("T1".to_string(), if decision == Cal::Base { Cal::Base } else { decision.clone() });
        self.rules.push(Rule {
            head: Atom {
                name: name.clone(),
                terms: vec![Term::Var("A".into(), sp), Term::Var("T".into(), sp)],
                span: sp,
            },
            body,
            span: sp,
            unit: "view".into(),
        });
        self.var_cals.push(cals);
        name
    }

    /// Resolve the bracketed arguments of a surface atom in the current
    /// substitution: values and relation references.
    fn bracket_args(&mut self, atom: &ast::Atom, subst: &Subst, unit: &str, label: &str) -> Option<Vec<RArg>> {
        let mut out = Vec::new();
        for b in &atom.brackets {
            match b {
                ast::BracketArg::Lit(l, sp) => {
                    // A literal the strategy writes in a bracket is its own
                    // degree of freedom (W5), wherever it is substituted.
                    if unit == self.root.name {
                        self.bracket_literals.push((label.to_string(), l.clone(), *sp));
                    }
                    out.push(RArg::Val(Expr::Lit(l.clone(), *sp)))
                }
                ast::BracketArg::Name(n, sp) => {
                    if let Some(e) = subst.vals.get(n) {
                        out.push(RArg::Val(e.clone()));
                    } else if let Some(r) = subst.rels.get(n) {
                        out.push(RArg::Rel(r.clone()));
                    } else if self.params.contains_key(n) {
                        out.push(RArg::Val(Expr::Param(n.clone(), *sp)));
                    } else if self.template_in_scope(n, unit).is_some() || self.relations.contains_key(n) || self.class_decls.iter().any(|r| r.name == *n) || n == "universe" || n == "traded" {
                        out.push(RArg::Rel(RRel {
                            name: n.clone(),
                            args: vec![],
                            cal: None,
                            instance: false,
                        }));
                    } else if is_var(n) {
                        self.diag(
                            Code::M,
                            unit,
                            Some(label.to_string()),
                            *sp,
                            format!(
                                "`{}` in the brackets of `{}` is bound by no literal of the rule: a bracketed input is bound by the caller, as a literal, a parameter or a relation, so that the family `{}[...]` is finite",
                                n, atom.name, atom.name
                            ),
                        );
                        return None;
                    } else {
                        if !self.scope_incomplete {
                            self.diag(
                                Code::U,
                                unit,
                                Some(label.to_string()),
                                *sp,
                                format!("`{}` in the brackets of `{}` is neither a parameter nor a relation", n, atom.name),
                            );
                        }
                        return None;
                    }
                }
                ast::BracketArg::Nested(inner) => {
                    let args = self.bracket_args(inner, subst, unit, label)?;
                    let cal = match &inner.cal {
                        Some((c, csp)) => match self.calendars.get(c) {
                            Some(cal) => Some(cal.clone()),
                            None => {
                                self.diag(Code::X, unit, Some(label.to_string()), *csp, format!("`@{}` names no declared calendar", c));
                                return None;
                            }
                        },
                        None => None,
                    };
                    out.push(RArg::Rel(RRel {
                        name: inner.name.clone(),
                        args,
                        cal,
                        instance: false,
                    }));
                }
            }
        }
        Some(out)
    }

    /// Elaborate one rule: substitute the template's inputs, infer the
    /// calendar of every time variable, resolve every atom and lower the
    /// literals to the core forms.
    pub(crate) fn elaborate_rule(&mut self, job: &Job) -> Option<(Rule, HashMap<String, Cal>)> {
        let unit = job.unit.clone();
        let label = self.next_label(&unit, &job.head);
        // A rule mentioning a relation declared with a reserved name is not
        // judged: U was reported at the declaration.
        if !self.reserved_declared.is_empty() && job.rule.body.iter().any(|l| ast_mentions(l, &self.reserved_declared)) {
            return None;
        }
        let head_sig = self.relations.get(&job.head).cloned()?;
        let key_pos = head_sig.key_pos()?;
        // The head: terms lowered, the instance name.
        if job.rule.head.terms.len() != head_sig.args.len() {
            self.diag(
                Code::T,
                &unit,
                Some(label.clone()),
                job.rule.head.span,
                format!(
                    "the head of `{}` has {} arguments, and the relation takes {}",
                    job.rule.head.name,
                    job.rule.head.terms.len(),
                    head_sig.args.len()
                ),
            );
            return None;
        }
        let head_key = match &job.rule.head.terms[key_pos] {
            Term::Var(v, _) => v.clone(),
            t => {
                self.diag(Code::F, &unit, Some(label.clone()), t.span(), format!("the head's temporal key must be a variable, found `{}`", t));
                return None;
            }
        };
        let mut cals: HashMap<String, Cal> = HashMap::new();
        cals.insert(head_key.clone(), job.cal.clone());
        self.infer_cals(&job.rule.body, &job.subst, &mut cals, &job.cal, &unit, &label);
        let mut ctx = Ctx {
            unit: unit.clone(),
            label: label.clone(),
            cal: job.cal.clone(),
            head_key: head_key.clone(),
            subst: job.subst.clone(),
            head_entity: job.rule.head.terms.iter().find_map(|t| match t {
                Term::Var(v, _) if head_sig.args.iter().zip(&job.rule.head.terms).any(|(a, tt)| a.ty.is_entity() && tt == t) => Some(v.clone()),
                _ => None,
            }),
            cals: cals.clone(),
            errors: false,
        };
        let mut body = Vec::new();
        for l in &job.rule.body {
            if let Some(ll) = self.lower_literal(l, &mut ctx, false) {
                body.push(ll);
            }
        }
        if ctx.errors {
            return None;
        }
        let mut head_terms = Vec::new();
        for t in &job.rule.head.terms {
            head_terms.push(self.lower_term(t, &mut ctx, true)?);
        }
        // Decide rules: the default order and the clock.
        if job.kind == JobKind::Decide {
            if let Some(Term::Ctor(c, subs, csp)) = head_terms.get_mut(1) {
                if (c == "hold" || c == "trade") && subs.len() == 2 {
                    subs.push(Term::Param(self.default_order.clone(), *csp));
                }
                if self.clock_offset.is_some() && self.relations.contains_key("clock") {
                    if let Some(a) = subs.first().cloned() {
                        body.push(Literal::Atom(Atom {
                            name: "clock".into(),
                            terms: vec![a, Term::Var(head_key.clone(), *csp)],
                            span: *csp,
                        }));
                    }
                }
            }
        }
        let rule = Rule {
            head: Atom {
                name: job.head.clone(),
                terms: head_terms,
                span: job.rule.head.span,
            },
            body,
            span: job.rule.span,
            unit,
        };
        Some((rule, ctx.cals))
    }

    /// The calendar of every time variable of a body (3.4, 3.5): the head's
    /// key is on the rule's calendar; prev, lag, rows and windows stay on
    /// their base's; `bucket` and `session` range over the base bars;
    /// `session_of` binds a session label; an atom's fresh key is on the
    /// relation's calendar when it has one fixed, else the rule's.
    fn infer_cals(&mut self, body: &[ast::Literal], subst: &Subst, cals: &mut HashMap<String, Cal>, rule_cal: &Cal, unit: &str, label: &str) {
        let mut lits: Vec<&ast::Literal> = Vec::new();
        fn walk<'b>(l: &'b ast::Literal, out: &mut Vec<&'b ast::Literal>) {
            out.push(l);
            match l {
                ast::Literal::Agg { conj, .. } => conj.iter().for_each(|c| walk(c, out)),
                ast::Literal::Case { branches, .. } => branches.iter().for_each(|b| b.guard.iter().for_each(|c| walk(c, out))),
                _ => {}
            }
        }
        body.iter().for_each(|l| walk(l, &mut lits));
        let var_of = |t: &Term| -> Option<String> {
            match t {
                Term::Var(v, _) => Some(v.clone()),
                _ => None,
            }
        };
        let mut changed = true;
        let mut rounds = 0;
        while changed && rounds < 8 {
            changed = false;
            rounds += 1;
            for l in &lits {
                match l {
                    ast::Literal::Prev { t, t1, .. } | ast::Literal::Lag { t, t1, .. } => {
                        if let (Some(a), Some(b)) = (var_of(t), var_of(t1)) {
                            if let Some(c) = cals.get(&a).cloned() {
                                if cals.get(&b).is_none() {
                                    cals.insert(b, c);
                                    changed = true;
                                }
                            }
                        }
                    }
                    ast::Literal::SessionOf { s, .. } => {
                        if let Some(v) = var_of(s) {
                            if cals.get(&v).is_none() {
                                cals.insert(v, Cal::Sessions);
                                changed = true;
                            }
                        }
                    }
                    ast::Literal::Window { var, spec, .. } => {
                        let Some(v) = var_of(var) else { continue };
                        if cals.contains_key(&v) {
                            continue;
                        }
                        let c = match spec {
                            ast::WindowSpec::Rows { .. } | ast::WindowSpec::PriorRows { .. } | ast::WindowSpec::Window { .. } | ast::WindowSpec::PriorWindow { .. } => Some(rule_cal.clone()),
                            ast::WindowSpec::Bucket { .. } | ast::WindowSpec::Session { .. } => Some(Cal::Base),
                        };
                        if let Some(c) = c {
                            cals.insert(v, c);
                            changed = true;
                        }
                    }
                    ast::Literal::Atom(a) | ast::Literal::Neg(a) | ast::Literal::Top { atom: a, .. } | ast::Literal::AsOf { atom: a, .. } => {
                        // The relation's fixed calendar, when it has one.
                        let name = subst.rels.get(&a.name).map(|r| r.name.clone()).unwrap_or(a.name.clone());
                        let explicit = a.cal.as_ref().and_then(|(c, _)| self.calendars.get(c).cloned());
                        let fixed: Option<Cal> = if let Some(c) = explicit {
                            Some(c)
                        } else if let Some(r) = subst.rels.get(&a.name) {
                            r.cal.clone()
                        } else if KERNEL_RELS.contains(&name.as_str()) {
                            Some(self.decision_cal.clone())
                        } else if BAR_PRIMS.contains(&name.as_str()) {
                            Some(Cal::Base)
                        } else if name == "session" {
                            Some(Cal::Sessions)
                        } else if let Some(sr) = self.strategy_rels.get(&name).filter(|_| unit == self.root.name) {
                            if sr.cals.len() == 1 {
                                Some(sr.cals[0].clone())
                            } else {
                                None
                            }
                        } else {
                            None
                        };
                        let key_term = self.key_term_of(a, &name, unit);
                        let Some(kv) = key_term.and_then(|t| var_of(&t)) else { continue };
                        if cals.contains_key(&kv) {
                            continue;
                        }
                        if let Some(c) = fixed {
                            cals.insert(kv, c);
                            changed = true;
                        }
                    }
                    _ => {}
                }
            }
        }
        let _ = (unit, label);
    }

    /// The term in the key position of a surface atom, by the relation's
    /// declaration (a primitive, a template or a strategy relation).
    fn key_term_of(&self, a: &ast::Atom, name: &str, unit: &str) -> Option<Term> {
        let pos = if KERNEL_RELS.contains(&name) {
            match name {
                "position" | "mark" | "fill" | "clock" | "spec" | "chain" | "greeks" | "margin_by" | "contract" => 1,
                _ => 0,
            }
        } else if name == "universe" {
            1
        } else if name == "instruments" {
            return None;
        } else if let Some(decl) = self.class_decls.iter().find(|r| r.name == name) {
            decl.args.iter().position(|x| x.key)?
        } else {
            self.template_in_scope(name, unit)?.decl.args.iter().position(|x| x.key)?
        };
        a.terms.get(pos).cloned()
    }

    /// The template a name denotes in a unit's rules: the strategy's own
    /// first in the strategy, else a library's.
    fn template_in_scope(&self, name: &str, unit: &str) -> Option<&Template> {
        if unit == self.root.name {
            if let Some(t) = self.strategy_templates.get(name) {
                return Some(t);
            }
        }
        self.templates.get(name)
    }

    fn lower_term(&mut self, t: &Term, ctx: &mut Ctx, _in_head: bool) -> Option<Term> {
        Some(match t {
            Term::Var(v, sp) => match ctx.subst.vals.get(v) {
                Some(e) => match e {
                    Expr::Lit(l, _) => Term::Lit(l.clone(), *sp),
                    Expr::Param(p, _) => Term::Param(p.clone(), *sp),
                    Expr::Neg(inner, _) => match &**inner {
                        Expr::Lit(l, _) => Term::Lit(negate_lit(l), *sp),
                        _ => Term::Var(v.clone(), *sp),
                    },
                    _ => Term::Var(v.clone(), *sp),
                },
                None => Term::Var(v.clone(), *sp),
            },
            Term::Wild(sp) => Term::Wild(*sp),
            Term::Param(p, sp) => {
                if self.params.contains_key(p) || ORDER_KINDS.contains(&p.as_str()) || p == "gtc" || p == "day" {
                    Term::Param(p.clone(), *sp)
                } else if let Some(e) = ctx.subst.vals.get(p) {
                    match e {
                        Expr::Lit(l, _) => Term::Lit(l.clone(), *sp),
                        Expr::Param(q, _) => Term::Param(q.clone(), *sp),
                        _ => Term::Param(p.clone(), *sp),
                    }
                } else {
                    self.diag(
                        Code::U,
                        &ctx.unit.clone(),
                        Some(ctx.label.clone()),
                        *sp,
                        format!("`{}` is neither a parameter of `{}` nor a variable (variables start with an uppercase letter)", p, self.root.name),
                    );
                    ctx.errors = true;
                    return None;
                }
            }
            Term::Lit(l, sp) => Term::Lit(l.clone(), *sp),
            Term::Ctor(c, subs, sp) => {
                let mut out = Vec::new();
                for s in subs {
                    out.push(self.lower_term(s, ctx, _in_head)?);
                }
                Term::Ctor(c.clone(), out, *sp)
            }
        })
    }

    fn lower_expr(&mut self, e: &Expr, ctx: &mut Ctx) -> Option<Expr> {
        Some(match e {
            Expr::Var(v, sp) => match ctx.subst.vals.get(v) {
                Some(x) => x.clone(),
                None => Expr::Var(v.clone(), *sp),
            },
            Expr::Param(p, sp) => {
                if let Some(x) = ctx.subst.vals.get(p) {
                    x.clone()
                } else if self.params.contains_key(p) {
                    Expr::Param(p.clone(), *sp)
                } else {
                    self.diag(
                        Code::U,
                        &ctx.unit.clone(),
                        Some(ctx.label.clone()),
                        *sp,
                        format!("`{}` is neither a parameter of `{}` nor a variable (variables start with an uppercase letter)", p, self.root.name),
                    );
                    ctx.errors = true;
                    return None;
                }
            }
            Expr::Lit(l, sp) => Expr::Lit(l.clone(), *sp),
            Expr::Neg(a, sp) => Expr::Neg(Box::new(self.lower_expr(a, ctx)?), *sp),
            Expr::Bin(op, a, b, sp) => Expr::Bin(*op, Box::new(self.lower_expr(a, ctx)?), Box::new(self.lower_expr(b, ctx)?), *sp),
            Expr::Call(f, args, sp) => {
                let mut out = Vec::new();
                for a in args {
                    out.push(self.lower_expr(a, ctx)?);
                }
                Expr::Call(f.clone(), out, *sp)
            }
        })
    }

    /// Resolve a surface atom in a rule: the instance it denotes and how it
    /// is read, with its terms lowered.
    fn lower_atom(&mut self, a: &ast::Atom, ctx: &mut Ctx, negated: bool) -> Option<(Atom, Read, Cal)> {
        let unit = ctx.unit.clone();
        let label = ctx.label.clone();
        // A relation input of the template: substitute.
        let (name, args, explicit, instance) = match ctx.subst.rels.get(&a.name).cloned() {
            Some(r) => {
                if !a.brackets.is_empty() {
                    self.diag(
                        Code::T,
                        &unit,
                        Some(label.clone()),
                        a.span,
                        format!("`{}` stands for `{}` here and takes no brackets", a.name, r.render()),
                    );
                    ctx.errors = true;
                    return None;
                }
                (r.name, r.args, r.cal.or_else(|| a.cal.as_ref().and_then(|(c, _)| self.calendars.get(c).cloned())), r.instance)
            }
            None => {
                let args = match self.bracket_args(a, &ctx.subst.clone(), &unit, &label) {
                    Some(x) => x,
                    None => {
                        ctx.errors = true;
                        return None;
                    }
                };
                let explicit = match &a.cal {
                    Some((c, csp)) => match self.calendars.get(c) {
                        Some(cal) => Some(cal.clone()),
                        None => {
                            let known: Vec<&str> = self.calendars.keys().map(|k| k.as_str()).collect();
                            self.diag(
                                Code::X,
                                &unit,
                                Some(label.clone()),
                                *csp,
                                format!("`{}@{}` names a calendar the `calendars` block does not declare (declared: {})", a.name, c, known.join(", ")),
                            );
                            ctx.errors = true;
                            return None;
                        }
                    },
                    None => None,
                };
                (a.name.clone(), args, explicit, false)
            }
        };
        // `instruments(A, From)`: the table, keyed at the head's bar.
        let mut terms: Vec<Term> = a.terms.clone();
        if name == "instruments" && terms.len() == 2 {
            terms.insert(1, Term::Var(ctx.head_key.clone(), a.span));
        }
        // The calendar: explicit, or the key variable's, or the rule's.
        let key_term = self.key_term_of(a, &name, &unit).or_else(|| if name == "instruments" { terms.get(1).cloned() } else { None });
        let key_cal = key_term.as_ref().and_then(|t| match t {
            Term::Var(v, _) => ctx.cals.get(v).cloned(),
            _ => None,
        });
        let cal = explicit.clone().or(key_cal.clone()).unwrap_or(ctx.cal.clone());
        if let (Some(e), Some(k)) = (&explicit, &key_cal) {
            if e != k {
                self.diag(
                    Code::X,
                    &unit,
                    Some(label.clone()),
                    a.span,
                    format!(
                        "`{}@{}` is read with a key on `{}`; the key of a `{}` instance is on `{}`",
                        name,
                        self.cal_name(e),
                        self.cal_name(k),
                        self.cal_name(e),
                        self.cal_name(e)
                    ),
                );
                ctx.errors = true;
                return None;
            }
        }
        let rrel = RRel {
            name: name.clone(),
            args,
            cal: explicit.clone(),
            instance,
        };
        let Some(mut res) = self.resolve_rrel(&rrel, &cal, a.span, &unit, Some(&label)) else {
            ctx.errors = true;
            return None;
        };
        // A series read: the direction from the series' calendar.
        let mut read = res.read.clone();
        if res.name == "value" {
            let series_cal = match terms.first() {
                Some(Term::Lit(Lit::Str(n), _)) => self.series.get(n).map(|r| r.as_calendar()),
                _ => None,
            };
            if let Some(Term::Lit(Lit::Str(n), nsp)) = terms.first() {
                if !self.demand.series.iter().any(|(s, _)| s == n) {
                    let known: Vec<String> = self.demand.series.iter().map(|(s, _)| format!("\"{}\"", s)).collect();
                    self.diag(
                        Code::V,
                        &unit,
                        Some(label.clone()),
                        *nsp,
                        format!(
                            "series `{}` occurs in a rule but in no `need series` of the data block (needed: {})",
                            n,
                            if known.is_empty() { "none".to_string() } else { known.join(", ") }
                        ),
                    );
                    ctx.errors = true;
                    return None;
                }
            }
            let sc = series_cal.unwrap_or(Cal::Base);
            let target = explicit.clone().unwrap_or(cal.clone());
            read = if sc == target || (sc == Cal::Base && target == self.base.as_calendar()) {
                if explicit.is_some() {
                    Read::Bucket(target.clone())
                } else {
                    Read::AsOf
                }
            } else if sc.refines(&target) {
                Read::Bucket(target.clone())
            } else if explicit.is_some() {
                self.diag(
                    Code::X,
                    &unit,
                    Some(label.clone()),
                    a.span,
                    format!(
                        "`value@{}` asks for the bar view of a series held at `{}`, a finer calendar than the series'; a bar view aggregates to a coarser calendar, and on a finer one the series is read as of the bar, `value(...)` with no calendar",
                        self.cal_name(&target),
                        sc
                    ),
                );
                ctx.errors = true;
                return None;
            } else {
                Read::AsOf
            };
        }
        // Vocabulary (WF-15): a label literal in a vocabulary column.
        if let Some(sig) = self.relations.get(&res.name).cloned() {
            if sig.args.len() != terms.len() {
                self.diag(
                    Code::T,
                    &unit,
                    Some(label.clone()),
                    a.span,
                    format!("`{}` takes {} arguments, {} given", a.name, sig.args.len(), terms.len()),
                );
                ctx.errors = true;
                return None;
            }
            for (arg, t) in sig.args.iter().zip(&terms) {
                if let (Ty::Label, Term::Lit(Lit::Str(s), lsp)) = (&arg.ty, t) {
                    let (vocab, what): (Option<Vec<String>>, &str) = match res.name.as_str() {
                        "member" => (Some(self.demand.lists.clone()), "`need lists`"),
                        "delisted" => (self.vocab.get("reasons").cloned(), "the class's reasons"),
                        "classification" => (
                            self.vocab.get("schemes").cloned().map(|mut v| {
                                v.extend(self.vocab.get("codes").cloned().unwrap_or_default());
                                v
                            }),
                            "the class's schemes",
                        ),
                        _ => (None, ""),
                    };
                    if let Some(v) = vocab {
                        if !v.contains(s) && !(res.name == "classification" && arg.name != "Scheme") {
                            let known: Vec<String> = v.iter().map(|x| format!("\"{}\"", x)).collect();
                            self.diag(
                                Code::V,
                                &unit,
                                Some(label.clone()),
                                *lsp,
                                format!(
                                    "`\"{}\"` occurs in a rule but in no {} of the data block ({}: {})",
                                    s,
                                    what,
                                    what,
                                    if known.is_empty() { "none".to_string() } else { known.join(", ") }
                                ),
                            );
                            ctx.errors = true;
                            return None;
                        }
                    }
                }
            }
        }
        if negated && read != Read::Plain {
            // The negation of a complete relation read across calendars is
            // the negation of its closed view at the rule's calendar; an
            // incomplete one has no closed view (WF-5).
            let complete = self.relations.get(&res.name).map(|s| s.complete).unwrap_or(false);
            if !complete {
                self.diag(
                    Code::N,
                    &unit,
                    Some(label.clone()),
                    a.span,
                    format!("`not {}` negates a relation read across calendars that is not complete, so a missing tuple is unknown, not false; negate a derived relation that reads it, or require it complete", a.name),
                );
                ctx.errors = true;
                return None;
            }
            let target = res.cal.clone();
            res.name = self.ensure_catalog_view(&res.name, &target, &read);
            read = Read::Plain;
        }
        let mut out = Vec::new();
        for t in &terms {
            out.push(self.lower_term(t, ctx, false)?);
        }
        Some((
            Atom {
                name: res.name,
                terms: out,
                span: a.span,
            },
            read,
            res.cal,
        ))
    }

    fn lower_literal(&mut self, l: &ast::Literal, ctx: &mut Ctx, _in_agg: bool) -> Option<Literal> {
        let unit = ctx.unit.clone();
        let label = ctx.label.clone();
        Some(match l {
            ast::Literal::Atom(a) => {
                let (atom, read, _) = self.lower_atom(a, ctx, false)?;
                match read {
                    Read::Plain => Literal::Atom(atom),
                    other => {
                        // The key term becomes a fresh wildcard; the read is as of the written key.
                        let sig = self.relations.get(&atom.name).cloned()?;
                        let k = sig.key_pos()?;
                        let at = atom.terms[k].clone();
                        let mut atom = atom;
                        atom.terms[k] = Term::Wild(at.span());
                        Literal::AsOf {
                            atom,
                            at,
                            within_bucket: match other {
                                Read::Bucket(c) => Some(c),
                                _ => None,
                            },
                            span: a.span,
                        }
                    }
                }
            }
            ast::Literal::Neg(a) => {
                if let Some(hint) = builtin_hint(&a.name) {
                    self.diag(
                        Code::U,
                        &unit,
                        Some(label),
                        a.span,
                        format!("`{}` is a temporal builtin, not a relation, so `not {}(...)` does not resolve; {}", a.name, a.name, hint),
                    );
                    ctx.errors = true;
                    return None;
                }
                let (atom, _, _) = self.lower_atom(a, ctx, true)?;
                Literal::Neg(atom)
            }
            ast::Literal::Prev { t, t1, span } => Literal::Builtin(
                Builtin::Prev {
                    t: self.lower_term(t, ctx, false)?,
                    t1: self.lower_term(t1, ctx, false)?,
                },
                *span,
            ),
            ast::Literal::Lag { t, n, t1, span } => Literal::Builtin(
                Builtin::Lag {
                    t: self.lower_term(t, ctx, false)?,
                    n: self.lower_expr(n, ctx)?,
                    t1: self.lower_term(t1, ctx, false)?,
                },
                *span,
            ),
            ast::Literal::MonthStart { t, span } => Literal::Builtin(Builtin::MonthStart { t: self.lower_term(t, ctx, false)? }, *span),
            ast::Literal::DayStart { t, span } => Literal::Builtin(Builtin::DayStart { t: self.lower_term(t, ctx, false)? }, *span),
            ast::Literal::SessionOf { a, t, s, span } => {
                if !self.demand.sessions && !self.demand_empty {
                    self.diag(
                        Code::E,
                        &unit,
                        Some(label),
                        *span,
                        "`session_of` reads the sessions, which the data block does not ask for; add `need sessions`",
                    );
                    ctx.errors = true;
                    return None;
                }
                Literal::Builtin(
                    Builtin::SessionOf {
                        a: self.lower_term(a, ctx, false)?,
                        t: self.lower_term(t, ctx, false)?,
                        s: self.lower_term(s, ctx, false)?,
                    },
                    *span,
                )
            }
            ast::Literal::Window { var, spec, span } => {
                let var = self.lower_term(var, ctx, false)?;
                let base_var = Term::Var(ctx.head_key.clone(), *span);
                match spec {
                    ast::WindowSpec::Rows { n, min } | ast::WindowSpec::PriorRows { n, min } => {
                        let n = self.lower_expr(n, ctx)?;
                        let min = match min {
                            Some(m) => self.lower_expr(m, ctx)?,
                            None => n.clone(),
                        };
                        Literal::Window {
                            var,
                            kind: if matches!(spec, ast::WindowSpec::Rows { .. }) { WindowKind::Rows } else { WindowKind::PriorRows },
                            base: base_var,
                            dur: n,
                            min,
                            lo: None,
                            span: *span,
                        }
                    }
                    ast::WindowSpec::Window { dur, min } | ast::WindowSpec::PriorWindow { dur, min } => {
                        let dur = self.lower_expr(dur, ctx)?;
                        let Some(min) = min else {
                            self.diag(
                                Code::T,
                                &unit,
                                Some(label),
                                *span,
                                "a calendar window requires `min K`, because a calendar duration has no natural count of bars; `rows[N]` is the entity's last N rows and needs none",
                            );
                            ctx.errors = true;
                            return None;
                        };
                        let min = self.lower_expr(min, ctx)?;
                        Literal::Window {
                            var,
                            kind: if matches!(spec, ast::WindowSpec::Window { .. }) { WindowKind::Window } else { WindowKind::Prior },
                            base: base_var,
                            dur,
                            min,
                            lo: None,
                            span: *span,
                        }
                    }
                    ast::WindowSpec::Bucket { cal: (c, csp), base } => {
                        let Some(cal) = self.calendars.get(c).cloned() else {
                            let known: Vec<&str> = self.calendars.keys().map(|k| k.as_str()).collect();
                            self.diag(
                                Code::X,
                                &unit,
                                Some(label),
                                *csp,
                                format!("`bucket({}, ...)` names a calendar the `calendars` block does not declare (declared: {})", c, known.join(", ")),
                            );
                            ctx.errors = true;
                            return None;
                        };
                        let base = self.lower_term(base, ctx, false)?;
                        if let Term::Var(b, _) = &base {
                            if let Some(bc) = ctx.cals.get(b) {
                                if *bc != cal {
                                    self.diag(
                                        Code::X,
                                        &unit,
                                        Some(label),
                                        *span,
                                        format!(
                                            "`bucket({}, {})` buckets by `{}`, but `{}` is on `{}`; a bucket aggregate goes from the base bars to the label's calendar",
                                            c,
                                            b,
                                            c,
                                            b,
                                            self.cal_name(bc)
                                        ),
                                    );
                                    ctx.errors = true;
                                    return None;
                                }
                            }
                        }
                        if cal == Cal::Base {
                            self.diag(
                                Code::X,
                                &unit,
                                Some(label),
                                *span,
                                "`bucket(base, T)` is the bar itself; a bucket aggregates the base bars of a coarser calendar's label",
                            );
                            ctx.errors = true;
                            return None;
                        }
                        self.cals_used.insert(cal.clone());
                        Literal::Window {
                            var,
                            kind: WindowKind::Bucket(cal),
                            base,
                            dur: Expr::Lit(Lit::Int(0), *span),
                            min: Expr::Lit(Lit::Int(1), *span),
                            lo: None,
                            span: *span,
                        }
                    }
                    ast::WindowSpec::Session { base, lo, hi } => {
                        if !self.demand.sessions && !self.demand_empty {
                            self.diag(
                                Code::E,
                                &unit,
                                Some(label),
                                *span,
                                "`session(X, [a, b))` reads the sessions, which the data block does not ask for; add `need sessions`",
                            );
                            ctx.errors = true;
                            return None;
                        }
                        Literal::Window {
                            var,
                            kind: WindowKind::Session,
                            base: self.lower_term(base, ctx, false)?,
                            dur: self.lower_expr(hi, ctx)?,
                            min: Expr::Lit(Lit::Int(0), *span),
                            lo: Some(self.lower_expr(lo, ctx)?),
                            span: *span,
                        }
                    }
                }
            }
            ast::Literal::Cmp { op, lhs, rhs, span } => Literal::Cmp {
                op: *op,
                lhs: self.lower_expr(lhs, ctx)?,
                rhs: self.lower_expr(rhs, ctx)?,
                span: *span,
            },
            ast::Literal::Assign { var, expr, span } => {
                // A template value input assigned in the body (`P0 = 1 USD/share` is not one).
                if let Some(e) = ctx.subst.vals.get(var).cloned() {
                    Literal::Cmp {
                        op: CmpOp::Eq,
                        lhs: e,
                        rhs: self.lower_expr(expr, ctx)?,
                        span: *span,
                    }
                } else {
                    Literal::Assign {
                        var: var.clone(),
                        expr: self.lower_expr(expr, ctx)?,
                        span: *span,
                    }
                }
            }
            ast::Literal::Agg { var, agg, args, conj, within, span } => {
                let mut cj = Vec::new();
                let mut rows_anchor: Option<Term> = None;
                for c in conj {
                    if let ast::Literal::Window {
                        var: wv,
                        spec: ast::WindowSpec::Rows { .. } | ast::WindowSpec::PriorRows { .. },
                        ..
                    } = c
                    {
                        rows_anchor = Some(wv.clone());
                    }
                    if let Some(lc) = self.lower_literal(c, ctx, true) {
                        cj.push(lc);
                    }
                }
                // The entity's own rows: anchored on the universe at the
                // rule's calendar (7.1); an entity-less rule's rows are the
                // calendar's bars.
                if let (Some(Term::Var(wv, wsp)), Some(entity)) = (&rows_anchor, ctx.head_entity.clone()) {
                    let cal = ctx.cal.clone();
                    let anchor = self.ensure_universe_at(&cal);
                    cj.insert(
                        0,
                        Literal::Atom(Atom {
                            name: anchor,
                            terms: vec![Term::Var(entity, *wsp), Term::Var(wv.clone(), *wsp)],
                            span: *wsp,
                        }),
                    );
                }
                let mut a = Vec::new();
                for e in args {
                    a.push(self.lower_expr(e, ctx)?);
                }
                Literal::Agg {
                    var: var.clone(),
                    agg: agg.clone(),
                    args: a,
                    conj: cj,
                    within: within.as_ref().map(|w| w.iter().map(|(v, _)| v.clone()).collect()),
                    span: *span,
                }
            }
            ast::Literal::Top { n, atom, by, rank, within, span } => {
                let (atom, read, _) = self.lower_atom(atom, ctx, false)?;
                if read != Read::Plain {
                    self.diag(
                        Code::X,
                        &unit,
                        Some(label),
                        *span,
                        "a reduction ranges over a relation read at its own calendar; derive the view in a relation of its own first",
                    );
                    ctx.errors = true;
                    return None;
                }
                Literal::Top {
                    n: match n {
                        Some(e) => Some(self.lower_expr(e, ctx)?),
                        None => None,
                    },
                    atom,
                    by: by.clone(),
                    rank: rank.clone(),
                    within: within.as_ref().map(|w| w.iter().map(|(v, _)| v.clone()).collect()),
                    span: *span,
                }
            }
            ast::Literal::AsOf { atom, at, span } => {
                let (atom, _, _) = self.lower_atom(atom, ctx, false)?;
                Literal::AsOf {
                    atom,
                    at: self.lower_term(at, ctx, false)?,
                    within_bucket: None,
                    span: *span,
                }
            }
            ast::Literal::Case { var, branches, otherwise, span } => {
                let mut bs = Vec::new();
                for b in branches {
                    let mut guard = Vec::new();
                    for g in &b.guard {
                        if let Some(lg) = self.lower_literal(g, ctx, _in_agg) {
                            guard.push(lg);
                        }
                    }
                    bs.push(Branch {
                        guard,
                        value: self.lower_expr(&b.value, ctx)?,
                        span: b.span,
                    });
                }
                Literal::Case {
                    var: var.clone(),
                    branches: bs,
                    otherwise: match otherwise {
                        Some(e) => Some(self.lower_expr(e, ctx)?),
                        None => None,
                    },
                    span: *span,
                }
            }
        })
    }

    /// Display name of a calendar.
    pub(crate) fn cal_name(&self, cal: &Cal) -> String {
        self.cal_names.get(cal).cloned().unwrap_or_else(|| cal.to_string())
    }
}

/// The substituted relation arguments of an instance, in input order (for
/// its name).
/// Whether a surface literal names one of the given relations or builtins.
fn ast_mentions(l: &ast::Literal, names: &HashSet<String>) -> bool {
    let has = |n: &str| names.contains(n);
    match l {
        ast::Literal::Atom(a) | ast::Literal::Neg(a) | ast::Literal::Top { atom: a, .. } | ast::Literal::AsOf { atom: a, .. } => has(&a.name),
        ast::Literal::Prev { .. } => has("prev"),
        ast::Literal::Lag { .. } => has("lag"),
        ast::Literal::MonthStart { .. } => has("month_start"),
        ast::Literal::DayStart { .. } => has("day_start"),
        ast::Literal::SessionOf { .. } => has("session_of"),
        ast::Literal::Window { .. } | ast::Literal::Cmp { .. } | ast::Literal::Assign { .. } => false,
        ast::Literal::Agg { conj, .. } => conj.iter().any(|c| ast_mentions(c, names)),
        ast::Literal::Case { branches, .. } => branches.iter().any(|b| b.guard.iter().any(|g| ast_mentions(g, names))),
    }
}

fn subst_order(decl: &ast::RelDecl, subst: &Subst) -> Vec<Option<String>> {
    decl.inputs.iter().map(|i| subst.rels.get(&i.name).map(|r| r.name.clone())).collect()
}

fn negate_lit(l: &Lit) -> Lit {
    match l {
        Lit::Int(i) => Lit::Int(-i),
        Lit::Float(x) => Lit::Float(-x),
        Lit::Quantity(x, u) => Lit::Quantity(-x, u.clone()),
        Lit::Money(x, c) => Lit::Money(-x, c.clone()),
        Lit::Price(x, c) => Lit::Price(-x, c.clone()),
        other => other.clone(),
    }
}

/// The context of one rule's elaboration.
pub(crate) struct Ctx {
    pub unit: String,
    pub label: String,
    pub cal: Cal,
    pub head_key: String,
    pub subst: Subst,
    pub head_entity: Option<String>,
    pub cals: HashMap<String, Cal>,
    pub errors: bool,
}

#[allow(dead_code)]
fn _unused(_: &HashSet<String>, _: &VecDeque<Job>) {}
