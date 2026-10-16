//! The elaborator and checker of the second formulation (`docs/language-v2.md`,
//! section 10; `corpus-v2/README.md`, "Codes"). A strategy's blocks are
//! resolved against the class catalog and the libraries it uses, the library
//! templates are instantiated at every call (bracketed inputs and calendars
//! substituted at link time, F-11.1), the bar views are generated, and the
//! resulting first-order program is judged rule by rule and as a whole.
//! Every diagnostic carries exactly one code, and every code is one judgment.

pub mod config;
pub mod dof;
pub mod link;
pub mod rule;
pub mod types;

pub use config::{Clock, Commission, Delisting, ExecBlock, Impact, Leverage, Limit, LimitKind, LimitPolicy, ReportConfig, Slippage};
pub use dof::DegreesOfFreedom;

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use crate::ir::*;
use crate::syntax::ast::{self, Need, NeedKind, Prefer, Require, Unit};

/// One diagnostic code per judgment (language-v2, section 10; the retained
/// judgments of the semantic model, section 8).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Code {
    /// Name resolution: undeclared or reserved.
    U,
    /// A primitive the demand did not ask for.
    E,
    /// WF-1 range restriction: an unbound head variable.
    B,
    /// WF-2 well-modedness: an unbound input, or no well-moded order.
    M,
    /// WF-3 types.
    T,
    /// WF-4 temporal recursion without a step.
    R,
    /// WF-5 negation over an open relation.
    N,
    /// WF-6 causality.
    F,
    /// WF-7 deterministic reduction.
    D,
    /// WF-8 temporal stratification.
    S,
    /// WF-9: at least one decide rule.
    Z,
    /// WF-16 calendars.
    X,
    /// WF-11 keyedness.
    K,
    /// WF-13 explicit grouping.
    G,
    /// WF-14 closed configuration.
    P,
    /// WF-15 vocabulary.
    V,
    /// WF-17 demand.
    Q,
    /// Observation names a relation of the program.
    O,
    /// Warning: a derived relation decide never reaches.
    W1,
    /// Warning: an unused parameter.
    W2,
    /// Warning: a declared relation with no defining rule.
    W3,
    /// Warning: a reduction whose group holds every identity of its atom,
    /// so it ranks one tuple.
    W4,
    /// Warning: a numeric literal inside a strategy rule is a degree of
    /// freedom the study counts.
    W5,
    /// Warning: a ticker literal is a snapshot of the bundle date.
    W6,
    /// Warning: a calendar window's minimum exceeds what its duration holds.
    W7,
    /// Warning (on a run): a relation on a path to decide never fired.
    W8,
    /// Warning: an instruments table is a hindsight constant, counted.
    W9,
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
            "X" => Code::X,
            "K" => Code::K,
            "G" => Code::G,
            "P" => Code::P,
            "V" => Code::V,
            "Q" => Code::Q,
            "O" => Code::O,
            "W1" => Code::W1,
            "W2" => Code::W2,
            "W3" => Code::W3,
            "W4" => Code::W4,
            "W5" => Code::W5,
            "W6" => Code::W6,
            "W7" => Code::W7,
            "W8" => Code::W8,
            "W9" => Code::W9,
            _ => return None,
        })
    }
    pub fn judgment(self) -> &'static str {
        match self {
            Code::U => "name resolution",
            Code::E => "demand",
            Code::B => "WF-1 range restriction",
            Code::M => "WF-2 well-modedness",
            Code::T => "WF-3 types",
            Code::R => "WF-4 temporal recursion",
            Code::N => "WF-5 completeness",
            Code::F => "WF-6 causality",
            Code::D => "WF-7 determinism",
            Code::S => "WF-8 stratification",
            Code::Z => "WF-9 decisions",
            Code::X => "WF-16 calendars",
            Code::K => "WF-11 keyedness",
            Code::G => "WF-13 grouping",
            Code::P => "WF-14 closed configuration",
            Code::V => "WF-15 vocabulary",
            Code::Q => "WF-17 demand",
            Code::O => "observation",
            Code::W1 => "dead rule",
            Code::W2 => "unused parameter",
            Code::W3 => "undefined relation",
            Code::W4 => "degenerate reduction",
            Code::W5 => "degrees of freedom",
            Code::W6 => "ticker snapshot",
            Code::W7 => "window sanity",
            Code::W8 => "never fired",
            Code::W9 => "hindsight list",
        }
    }
    /// Warnings never refuse a program.
    pub fn is_warning(self) -> bool {
        matches!(self, Code::W1 | Code::W2 | Code::W3 | Code::W4 | Code::W5 | Code::W6 | Code::W7 | Code::W8 | Code::W9)
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

/// Every parsed unit the checker may resolve names against: strategies,
/// libraries, class catalogs and execution presets.
#[derive(Clone, Debug, Default)]
pub struct Workspace {
    pub units: Vec<Unit>,
}

impl Workspace {
    pub fn new() -> Workspace {
        Workspace { units: vec![] }
    }
    pub fn add_source(&mut self, src: &str) -> Result<(), crate::syntax::ParseError> {
        let units = crate::syntax::parse_units(src)?;
        self.units.extend(units);
        Ok(())
    }
    pub fn strategy(&self, name: &str) -> Option<&ast::Strategy> {
        self.units.iter().find_map(|u| match u {
            Unit::Strategy(s) if s.name == name => Some(s),
            _ => None,
        })
    }
    pub fn library(&self, name: &str) -> Option<&ast::Library> {
        self.units.iter().find_map(|u| match u {
            Unit::Library(l) if l.name == name => Some(l),
            _ => None,
        })
    }
    pub fn class(&self, name: &str) -> Option<&ast::Class> {
        self.units.iter().find_map(|u| match u {
            Unit::Class(c) if c.name == name => Some(c),
            _ => None,
        })
    }
    pub fn preset(&self, name: &str) -> Option<&ast::ExecutionPreset> {
        self.units.iter().find_map(|u| match u {
            Unit::Execution(e) if e.name == name => Some(e),
            _ => None,
        })
    }
    pub fn strategies(&self) -> impl Iterator<Item = &ast::Strategy> {
        self.units.iter().filter_map(|u| match u {
            Unit::Strategy(s) => Some(s),
            _ => None,
        })
    }
    pub fn libraries(&self) -> impl Iterator<Item = &ast::Library> {
        self.units.iter().filter_map(|u| match u {
            Unit::Library(l) => Some(l),
            _ => None,
        })
    }
    pub fn presets(&self) -> Vec<(String, Vec<ast::ExecField>)> {
        self.units
            .iter()
            .filter_map(|u| match u {
                Unit::Execution(e) => Some((e.name.clone(), e.fields.clone())),
                _ => None,
            })
            .collect()
    }
}

/// Polarity of a body reference for the dependency graph.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Polarity {
    Positive,
    Negative,
    /// Inside an aggregation, a reduction or a case guard.
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
    /// The label of T's own session, which closes after T (3.5): a value,
    /// not a causal key.
    Future,
    /// Bound by an atom the checker could not resolve (U or E already
    /// reported): not judged.
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
    /// The literal is a `top` or a `rank` (the reduction closes the relation, WF-5).
    pub is_top: bool,
    /// A positive atom of a case guard that later branches negate
    /// (Definition 5.9): the relation must be closed.
    pub in_case_guard: bool,
}

/// What the per-rule pass learned about one rule.
#[derive(Clone, Debug)]
pub struct RuleInfo {
    pub head: String,
    pub refs: Vec<AtomRef>,
    pub params_used: Vec<String>,
    /// The calendar the rule is on.
    pub cal: Cal,
    /// A well-moded order of the body (Proposition 4.2): the written order
    /// is semantically void, and this is the one the kernel evaluates in
    /// when no plan is chosen.
    pub order: Vec<usize>,
    /// The type each string literal of the rule resolved to (entity or
    /// label), by its span.
    pub str_types: Vec<(Span, Ty)>,
}

/// The demand of a program (Definition 3.11): what the data block asks for.
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Demand {
    pub class: String,
    pub bars: Option<Resolution>,
    pub bars_from: Option<i64>,
    pub bars_complete: bool,
    pub actions: bool,
    pub listing: bool,
    pub classification: bool,
    pub lists: Vec<String>,
    /// Series by name, with the resolution the need wrote (or the class's).
    pub series: Vec<(String, Option<Resolution>)>,
    pub series_complete: bool,
    pub sessions: bool,
    pub point_value: bool,
    pub commission: bool,
    pub usable: bool,
    pub instruments_where: Vec<(String, String)>,
    /// `require reviewed`: every version bound carries a reviewer's
    /// `reviewed` attribute; `allow unreviewed` admits the others, warned.
    pub require_reviewed: bool,
    pub prefer: String,
    pub needs: usize,
    /// The data block's `budget`, when it writes one (A-D22).
    pub budget: Option<BudgetSpec>,
    /// The dynamic needs (D-7.2): the option chains read per barrier.
    pub chains: Vec<ChainSpec>,
}

/// A duration's nominal length in days (a month as 365.25 / 12), to order
/// the ends of a range written in mixed units.
fn duration_days(d: &crate::ir::Duration) -> f64 {
    d.months as f64 * 365.25 / 12.0 + d.days as f64 + d.nanos as f64 / crate::kernel::time::DAY as f64
}

/// A demand's budget: the caps the resolver keeps under (D-7.1).
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct BudgetSpec {
    pub money: Option<f64>,
    pub bytes: Option<u64>,
    pub requests: Option<u64>,
    pub seconds: Option<f64>,
}

/// A dynamic need of a chain (D-7.2).
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ChainSpec {
    pub underlying: String,
    pub expiry: (crate::ir::Duration, crate::ir::Duration),
    pub moneyness: (f64, f64),
    pub right: Option<String>,
    pub spot: String,
}

impl Demand {
    /// The `need` that would admit a primitive, for the E diagnostic.
    pub fn need_for(name: &str) -> &'static str {
        match name {
            "open" | "high" | "low" | "close" | "volume" => "bars",
            "split" | "dividend" | "delisted" => "actions",
            "listed" | "ticker" => "listing",
            "member" => "lists",
            "value" => "series",
            "session" => "sessions",
            "point_value" => "point_value",
            "commission" => "commission",
            "classification" => "classification",
            "usable" => "usable",
            _ => "",
        }
    }
    pub fn admits(&self, name: &str) -> bool {
        match Demand::need_for(name) {
            "bars" => self.bars.is_some(),
            "actions" => self.actions,
            "listing" => self.listing,
            "lists" => !self.lists.is_empty(),
            "series" => !self.series.is_empty(),
            "sessions" => self.sessions,
            "point_value" => self.point_value,
            "commission" => self.commission,
            "classification" => self.classification,
            "usable" => self.usable,
            _ => false,
        }
    }
}

/// An `observe` line: the relation, the identity bindings of its `where`,
/// and whether instances are traced.
#[derive(Clone, Debug, PartialEq)]
pub struct Observe {
    pub rel: String,
    pub wheres: Vec<(String, Lit)>,
    pub trace_instances: bool,
    pub span: Span,
}

/// A resolved, checked program: a strategy with its libraries instantiated
/// and its configuration expanded (language-v2, 8.4, form 2).
#[derive(Clone, Debug)]
pub struct Program {
    pub strategy: String,
    /// The instrument class and its unit word (`shares`, `contracts`).
    pub class: String,
    pub unit: String,
    /// The base resolution of the class (the finest bars the demand reads).
    pub base: Resolution,
    pub demand: Demand,
    /// The declared calendars by name (`base` included) and the display
    /// name of each calendar.
    pub calendars: BTreeMap<String, Cal>,
    pub cal_names: BTreeMap<Cal, String>,
    pub decision_cal: Cal,
    pub clock: Clock,
    /// The series the demand reads, with their native resolutions.
    pub series: BTreeMap<String, Resolution>,
    /// The instruments table, when the universe block writes one.
    pub instruments: Vec<(String, Option<i64>)>,
    /// All relations in scope, by name.
    pub relations: BTreeMap<String, Signature>,
    /// Parameters by unit name, then parameter name (the strategy's, under
    /// every unit that has rules).
    pub params: BTreeMap<String, BTreeMap<String, Param>>,
    /// All rules: the strategy's in written order, then the instantiated
    /// library and view rules.
    pub rules: Vec<Rule>,
    /// Per-rule analysis results, parallel to `rules`.
    pub infos: Vec<RuleInfo>,
    /// Per rule, the calendar of every time variable the elaborator
    /// inferred (3.4, 3.5): what `prev`, `lag` and the calendar windows
    /// step in (a session label's previous label is the instrument's
    /// previous session, whether or not it coincides with a base bar).
    pub var_cals: Vec<HashMap<String, Cal>>,
    /// Strata in evaluation order: each is a set of relation names.
    pub strata: Vec<Vec<String>>,
    /// Relations judged complete (WF-5).
    pub complete: BTreeSet<String>,
    pub degrees_of_freedom: DegreesOfFreedom,
    /// `silence flat` (true) or `silence hold`.
    pub silence_flat: bool,
    pub default_order: String,
    pub exec: ExecBlock,
    pub report: ReportConfig,
    pub observes: Vec<Observe>,
    /// The strategy as written: what the renderings of the program print.
    pub source: ast::Strategy,
}

impl Program {
    pub fn rules_for(&self, rel: &str) -> Vec<usize> {
        self.rules.iter().enumerate().filter(|(_, r)| r.head.name == rel).map(|(i, _)| i).collect()
    }
    pub fn param(&self, unit: &str, name: &str) -> Option<&Param> {
        self.params.get(unit).and_then(|m| m.get(name)).or_else(|| self.params.get(&self.strategy).and_then(|m| m.get(name)))
    }
    pub fn rule_label(&self, i: usize) -> String {
        rule_label(&self.rules, i)
    }
    /// The calendar of a relation.
    pub fn cal_of(&self, rel: &str) -> Cal {
        self.relations.get(rel).map(|s| s.cal.clone()).unwrap_or(self.decision_cal.clone())
    }
    /// The display name of a calendar: its declared name, or its constructor.
    pub fn cal_name(&self, cal: &Cal) -> String {
        self.cal_names.get(cal).cloned().unwrap_or_else(|| cal.to_string())
    }
    /// The reporting calendar, resolved.
    pub fn report_cal(&self) -> Cal {
        self.calendars.get(&self.report.calendar).cloned().unwrap_or(self.decision_cal.clone())
    }
}

pub fn rule_label(rules: &[Rule], i: usize) -> String {
    let r = &rules[i];
    let k = rules[..i].iter().filter(|o| o.unit == r.unit && o.head.name == r.head.name).count() + 1;
    format!("{}::{}#{}", r.unit, r.head.name, k)
}

/// Order two literals of one type, for a parameter's range: numbers by
/// value, money of one currency by amount, durations by calendar length.
/// `None` when they are not comparable.
pub(crate) fn lit_order(a: &Lit, b: &Lit) -> Option<std::cmp::Ordering> {
    use std::cmp::Ordering;
    match (a, b) {
        (Lit::Money(x, cx), Lit::Money(y, cy)) if cx == cy => x.partial_cmp(y),
        (Lit::Price(x, cx), Lit::Price(y, cy)) if cx == cy => x.partial_cmp(y),
        (Lit::Quantity(x, _), Lit::Quantity(y, _)) => x.partial_cmp(y),
        (Lit::Duration(x), Lit::Duration(y)) if x == y => Some(Ordering::Equal),
        (Lit::Duration(x), Lit::Duration(y)) if x.min_nanos() > y.max_nanos() => Some(Ordering::Greater),
        (Lit::Duration(x), Lit::Duration(y)) if x.max_nanos() < y.min_nanos() => Some(Ordering::Less),
        (Lit::Time(x), Lit::Time(y)) => Some(x.cmp(y)),
        (Lit::Int(_) | Lit::Float(_), Lit::Int(_) | Lit::Float(_)) => a.magnitude()?.partial_cmp(&b.magnitude()?),
        _ => None,
    }
}

/// Check every strategy in the workspace.
pub fn check_workspace(ws: &Workspace) -> Vec<Diagnostic> {
    let mut out: Vec<Diagnostic> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    let mut names: BTreeSet<&str> = BTreeSet::new();
    // The positions of the libraries' diagnostics: a probe adds a
    // diagnostic only where no strategy (and no earlier probe) reported
    // one, while two strategies' different messages at one position are
    // both kept (a window on two calendars).
    let lib_names: BTreeSet<&str> = ws.libraries().map(|l| l.name.as_str()).collect();
    let mut lib_seen: HashSet<(Code, String, u32, u32)> = HashSet::new();
    for s in ws.strategies() {
        if names.insert(&s.name) {
            for d in check_program(ws, &s.name).1 {
                if lib_names.contains(d.unit.as_str()) {
                    lib_seen.insert((d.code, d.unit.clone(), d.span.line, d.span.col));
                }
                if seen.insert(d.to_string()) {
                    out.push(d);
                }
            }
        }
    }
    // Every library is judged, whether or not a strategy uses it: in a
    // probe that calls each of its relations once (`library_probe`), keeping
    // what is the library's own.
    for lib in ws.libraries() {
        for (_, _, diags) in check_library_probe(ws, &lib.name) {
            for d in diags.into_iter().filter(|d| d.unit == lib.name) {
                if lib_seen.insert((d.code, d.unit.clone(), d.span.line, d.span.col)) && seen.insert(d.to_string()) {
                    out.push(d);
                }
            }
        }
    }
    out
}

/// Check one strategy; returns the resolved program when it has no errors.
/// A program whose demand covers a class whole: every relation the class
/// declares, every list, and the series catalog, for what writes or tests
/// a bundle of the class rather than one strategy's slice of it (`abt
/// synth`, `abt bundle build`, `abt bundle test`). The program decides
/// nothing of consequence (`hold(A, 0)` over the universe).
pub fn class_program(ws: &Workspace, class: &str) -> Result<Program, Vec<Diagnostic>> {
    class_program_at(ws, class, None)
}

/// `class_program` reading the class's bars at `base` (`@1m` for a minute
/// bundle of a class whose catalog is written at `@1d`); `None` reads them
/// at the class's own base.
pub fn class_program_at(ws: &Workspace, class: &str, base: Option<Resolution>) -> Result<Program, Vec<Diagnostic>> {
    let Some(cls) = ws.class(class) else {
        let known: Vec<String> = ws.units.iter().filter_map(|u| if let Unit::Class(c) = u { Some(c.name.clone()) } else { None }).collect();
        return Err(vec![Diagnostic {
            code: Code::U,
            severity: Severity::Error,
            unit: class.to_string(),
            rule: None,
            span: Span::default(),
            message: format!(
                "class `{}` is not in the workspace (the classes are {})",
                class,
                if known.is_empty() { "none".to_string() } else { known.join(", ") }
            ),
        }]);
    };
    let src = class_strategy_source(
        ws,
        cls,
        base,
        &ClassStrategy {
            name: format!("__class_{}", class),
            uses: "calendar".into(),
            params: String::new(),
            rules: String::new(),
            decides: "decide(T, hold(A, 0)) :- universe(A, T).".into(),
        },
    );
    let mut ws2 = ws.clone();
    if ws2.library("calendar").is_none() {
        ws2.add_source(CALENDAR_LIBRARY).map_err(|e| {
            vec![Diagnostic {
                code: Code::U,
                severity: Severity::Error,
                unit: "calendar".into(),
                rule: None,
                span: e.span,
                message: e.message,
            }]
        })?;
    }
    ws2.add_source(&src).map_err(|e| {
        vec![Diagnostic {
            code: Code::U,
            severity: Severity::Error,
            unit: class.to_string(),
            rule: None,
            span: e.span,
            message: format!("the class program does not parse: {}", e.message),
        }]
    })?;
    let (prog, diags) = check_program(&ws2, &format!("__class_{}", class));
    match prog {
        Some(p) => Ok(p),
        None => Err(diags),
    }
}

/// The parts of a strategy over a class's whole demand that vary: its name,
/// the libraries it uses, and its params, rules and decide rules.
struct ClassStrategy {
    name: String,
    uses: String,
    params: String,
    rules: String,
    decides: String,
}

/// The text of a strategy over a class's whole demand (every relation the
/// class declares, every list and the series catalog), deciding nothing of
/// consequence: what `class_program` checks, and the probe a library is
/// judged in (`library_probe`).
fn class_strategy_source(ws: &Workspace, cls: &ast::Class, base: Option<Resolution>, parts: &ClassStrategy) -> String {
    let class = &cls.name;
    let mut needs: Vec<String> = vec![format!("need bars{}", base.map(|r| format!(" {}", r)).unwrap_or_default())];
    for word in ["actions", "listing", "sessions", "point_value", "commission"] {
        if cls.rels.iter().any(|r| Demand::need_for(&r.name) == word) {
            needs.push(format!("need {}", word));
        }
    }
    for (kind, items, _) in &cls.vocab {
        if kind == "lists" && !items.is_empty() {
            let names: Vec<String> = items.iter().map(|(s, _)| format!("\"{}\"", s)).collect();
            needs.push(format!("need lists {}", names.join(", ")));
        }
    }
    if let Some(sc) = ws.class("series") {
        for (kind, items, _) in &sc.vocab {
            if kind == "names" && !items.is_empty() {
                let names: Vec<String> = items.iter().map(|(s, r)| format!("\"{}\"{}", s, r.map(|r| format!(" {}", r)).unwrap_or_default())).collect();
                needs.push(format!("need series {}", names.join(", ")));
            }
        }
    }
    let universe = if cls.rels.iter().any(|r| r.name == "listed") {
        "listed(A, T), traded@base(A, T)"
    } else {
        "traded@base(A, T)"
    };
    format!(
        "strategy {name} {{\n  data {{ class {class}\n    {needs}\n    require reviewed\n    prefer latest }}\n  uses {uses}\n  clock {{ decide on base }}\n  universe {{ universe(A, T) :- {universe}. }}\n  params {{\n{params}  }}\n  rules {{\n{rules}  }}\n  decisions {{ silence hold   orders market\n    {decides}\n  }}\n  execution {{\n    price close\n    orders market\n    lot whole\n    capital 1_000_000 USD fixed\n    commission per_share 0.005 USD min 1 USD\n    slippage none\n    impact none\n    participation none\n    financing none\n    leverage unconstrained\n    actions apply\n    delisting last_price\n    oversize halt\n    ruin halt\n    numerics running\n  }}\n  report {{ calendar base   periods 252 per year }}\n}}\n",
        name = parts.name,
        class = class,
        needs = needs.join("\n    "),
        uses = parts.uses,
        universe = universe,
        params = parts.params,
        rules = parts.rules,
        decides = parts.decides,
    )
}

/// The type names a signature may write; any other uppercase name is a
/// dimension variable (F-9).
const CONCRETE_TYPES: &[&str] = &["Count", "Scalar", "Duration", "Notional", "Price", "Quantity", "Timestamp", "Label", "Entity", "Decision"];

/// A placeholder of a written type for a library probe: the type the probe
/// declares (a dimension variable is a Scalar) and a literal of it; `None`
/// for a type the probe cannot supply.
fn probe_value(ty: &ast::TyName, unit_word: &str) -> Option<(String, String)> {
    let written = match &ty.arg {
        Some(a) => format!("{}<{}>", ty.name, a),
        None => ty.name.clone(),
    };
    Some(match (ty.name.as_str(), ty.arg.as_deref()) {
        ("Count", _) => (written, "2".into()),
        ("Scalar", _) => (written, "0.5".into()),
        ("Duration", _) => (written, "20d".into()),
        ("Notional", Some(c)) => (written, format!("1_000 {}", c)),
        ("Price", Some(c)) => (written, format!("1 {}/unit", c)),
        ("Quantity", _) => (written, format!("1 {}", unit_word)),
        (n, None) if !CONCRETE_TYPES.contains(&n) => ("Scalar".into(), "0.5".into()),
        _ => return None,
    })
}

/// The classes a library is probed against: the one its data block names,
/// or, for `class any` (written for every class), every class of the
/// workspace that has bars.
pub fn probe_classes<'w>(ws: &'w Workspace, lib: &ast::Library) -> Vec<&'w ast::Class> {
    match lib.data.as_ref().and_then(|d| d.class.as_ref()).map(|(c, _)| c.as_str()) {
        Some(c) if c != "any" => ws.class(c).into_iter().collect(),
        _ => ws
            .units
            .iter()
            .filter_map(|u| match u {
                Unit::Class(c) if c.base.is_some() && c.rels.iter().any(|r| r.name == "close") => Some(c),
                _ => None,
            })
            .collect(),
    }
}

/// A strategy that calls every relation of a library once, with a
/// parameter of its type for each value input and a stub relation of its
/// shape for each relation input, over the whole demand of a class the
/// library is written for: the library's rules are instantiated and judged
/// in it as in any strategy that uses them, so that a library is checked
/// whether or not a strategy of the workspace uses it. `None` when there is
/// nothing to probe.
pub fn library_probe(ws: &Workspace, lib: &ast::Library, cls: &ast::Class) -> Option<(String, String)> {
    // The class's entity sort: the type of the first argument of its
    // relations that is not a value type.
    let entity = cls
        .rels
        .iter()
        .flat_map(|r| r.args.iter())
        .filter(|a| !a.key)
        .filter_map(|a| a.ty.as_ref().map(|t| t.name.clone()))
        .find(|n| !CONCRETE_TYPES.contains(&n.as_str()))?;
    let unit_word = cls.unit.clone().unwrap_or_else(|| "units".into());
    let is_entity = |t: &ast::TyName| t.name == "Entity" || t.name == entity;
    let (mut params, mut rules, mut decides) = (String::new(), String::new(), Vec::new());
    'rel: for (k, d) in lib.rules.rels.iter().enumerate() {
        if d.args.iter().any(|a| !a.cals.is_empty()) {
            continue;
        }
        let mut brackets: Vec<String> = Vec::new();
        let mut stubs = String::new();
        let mut own_params = String::new();
        for (j, input) in d.inputs.iter().enumerate() {
            match &input.ty {
                ast::InputTy::Value(ty) => {
                    let Some((ty, lit)) = probe_value(ty, &unit_word) else { continue 'rel };
                    let name = format!("probe_p{}_{}", k, j);
                    own_params.push_str(&format!("    {} : {} = {}\n", name, ty, lit));
                    brackets.push(name);
                }
                ast::InputTy::Rel(shape) => {
                    let name = format!("probe_r{}_{}", k, j);
                    let (mut args, mut terms, mut body) = (Vec::new(), Vec::new(), vec!["universe(A, T)".to_string()]);
                    for (m, a) in shape.iter().enumerate() {
                        match &a.ty {
                            None => {
                                args.push("@T".to_string());
                                terms.push("T".to_string());
                            }
                            Some(t) if is_entity(t) => {
                                args.push(format!("A: {}", entity));
                                terms.push("A".to_string());
                            }
                            Some(t) => {
                                let Some((ty, lit)) = probe_value(t, &unit_word) else { continue 'rel };
                                args.push(format!("V{}: {}", m, ty));
                                terms.push(format!("V{}", m));
                                body.push(format!("V{} = {}", m, lit));
                            }
                        }
                    }
                    if !terms.contains(&"A".to_string()) || !terms.contains(&"T".to_string()) {
                        continue 'rel;
                    }
                    stubs.push_str(&format!("    rel {}({})\n    {}({}) :- {}.\n", name, args.join(", "), name, terms.join(", "), body.join(", ")));
                    brackets.push(name);
                }
            }
        }
        let terms: Vec<&str> = d
            .args
            .iter()
            .map(|a| {
                if a.key {
                    "T"
                } else if a.ty.as_ref().map(is_entity).unwrap_or(false) {
                    "A"
                } else {
                    "_"
                }
            })
            .collect();
        let call = if brackets.is_empty() {
            format!("{}({})", d.name, terms.join(", "))
        } else {
            format!("{}[{}]({})", d.name, brackets.join(", "), terms.join(", "))
        };
        params.push_str(&own_params);
        rules.push_str(&stubs);
        rules.push_str(&format!("    rel probe_{}(A: {}, @T)\n    probe_{}(A, T) :- universe(A, T), {}.\n", k, entity, k, call));
        decides.push(format!("decide(T, hold(A, 0)) :- probe_{}(A, T).", k));
    }
    if decides.is_empty() {
        return None;
    }
    let name = format!("__library_{}_{}", lib.name, cls.name);
    let uses = if lib.name == "calendar" { "calendar".to_string() } else { format!("calendar, {}", lib.name) };
    let src = class_strategy_source(
        ws,
        cls,
        None,
        &ClassStrategy {
            name: name.clone(),
            uses,
            params,
            rules,
            decides: decides.join("\n    "),
        },
    );
    Some((name, src))
}

/// The check of a library's probes (`library_probe`), one per class it is
/// probed against: the class, the program and every diagnostic, the probe's
/// own included. What `check_workspace` keeps of them is the library's.
pub type ProbeCheck = (String, Option<Program>, Vec<Diagnostic>);

pub fn check_library_probe(ws: &Workspace, lib: &str) -> Vec<ProbeCheck> {
    let Some(lib) = ws.library(lib) else { return Vec::new() };
    let mut out = Vec::new();
    for cls in probe_classes(ws, lib) {
        let Some((name, src)) = library_probe(ws, lib, cls) else { continue };
        let mut ws2 = ws.clone();
        if ws2.add_source(&src).is_err() {
            continue;
        }
        let (prog, mut diags) = check_program(&ws2, &name);
        // The probe is named by the library in what it reports.
        let probe = format!("`{}`", name);
        for d in &mut diags {
            d.message = d.message.replace(&probe, &format!("`{}`", lib.name));
        }
        out.push((cls.name.clone(), prog, diags));
    }
    out
}

/// The `calendar` module of the standard library (corpus-v2/lib/calendar.dsl),
/// for a workspace that holds the classes alone.
const CALENDAR_LIBRARY: &str = "library calendar {\n  data { class any }\n  rules {\n    rel bar(@T)\n    bar(T) :- universe(_, T).\n    rel prev_row(A: Entity, @T, T1: Timestamp)\n    prev_row(A, T, T1) :- universe(A, T), prev(T, T0), universe(A, T1) asof T0.\n    rel first_row(A: Entity, @T)\n    first_row(A, T) :- universe(A, T), not prev_row(A, T, _).\n    rel row_number(A: Entity, @T, K: Count)\n    row_number(A, T, 0) :- first_row(A, T).\n    row_number(A, T, K) :- prev_row(A, T, T1), row_number(A, T1, K0), K = K0 + 1.\n  }\n}\n";

pub fn check_program(ws: &Workspace, strategy: &str) -> (Option<Program>, Vec<Diagnostic>) {
    let Some(unit) = ws.strategy(strategy) else {
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
    cx.configure();
    cx.link();
    cx.check_rules();
    cx.program_checks();
    cx.finish()
}

/// A library template: its declaration and rules, with the library it is
/// written in.
#[derive(Clone, Debug)]
pub(crate) struct Template {
    pub lib: String,
    pub decl: ast::RelDecl,
    pub rules: Vec<ast::Rule>,
}

/// A strategy relation: its declaration and the calendars it is
/// instantiated on (one, or the finite set of `@X: base | session`).
#[derive(Clone, Debug)]
pub(crate) struct StrategyRel {
    pub cals: Vec<Cal>,
}

pub(crate) struct Checker<'a> {
    pub ws: &'a Workspace,
    pub root: &'a ast::Strategy,
    pub diags: Vec<Diagnostic>,
    // Configuration.
    pub class: Option<&'a ast::Class>,
    pub class_name: String,
    pub base: Resolution,
    pub unit_word: String,
    pub demand: Demand,
    pub calendars: BTreeMap<String, Cal>,
    pub cal_names: BTreeMap<Cal, String>,
    pub decision_cal: Cal,
    pub clock_offset: Option<Duration>,
    pub series: BTreeMap<String, Resolution>,
    /// The class's vocabularies: lists, reasons, schemes, names.
    pub vocab: BTreeMap<String, Vec<String>>,
    pub exec: Option<ExecBlock>,
    pub report: Option<ReportConfig>,
    pub silence_flat: bool,
    pub default_order: String,
    pub observes: Vec<Observe>,
    pub instruments: Vec<(String, Option<i64>)>,
    // Scope.
    pub relations: BTreeMap<String, Signature>,
    pub params: BTreeMap<String, Param>,
    pub libs: Vec<&'a ast::Library>,
    /// The libraries' templates by name (a name is declared in one library).
    pub templates: BTreeMap<String, Template>,
    /// The strategy's own templates, which shadow a library's in its rules.
    pub strategy_templates: BTreeMap<String, Template>,
    pub strategy_rels: BTreeMap<String, StrategyRel>,
    /// The class relations in scope: the class's own and, when series are
    /// needed, the series class's `value`.
    pub class_decls: Vec<ast::RelDecl>,
    /// Per relation, the provenance of each Timestamp-typed output relative
    /// to the relation's key (Causal: at or before it; Strict: before it),
    /// from its rules; a primitive's outputs carry none.
    pub out_prov: HashMap<String, Vec<Option<TimeProv>>>,
    /// Rules awaiting elaboration: (unit, rule, calendar, substitution).
    pub pending: std::collections::VecDeque<link::Job>,
    pub rules: Vec<Rule>,
    pub infos: Vec<Option<RuleInfo>>,
    /// Per rule: the calendar of every time variable the elaborator inferred.
    pub var_cals: Vec<HashMap<String, Cal>>,
    /// Relations declared with a reserved name (reported once; rules
    /// mentioning them are not judged).
    pub reserved_declared: HashSet<String>,
    /// A missing library or class was reported once at unit level: names
    /// that fail to resolve are then not reported per atom.
    pub scope_incomplete: bool,
    /// The demand has no `need`: primitives are not reported per atom (Q).
    pub demand_empty: bool,
    scc_order: Option<Sccs>,
    complete_set: BTreeSet<String>,
    dof: DegreesOfFreedom,
    /// Calendars used by any rule: views and universes are generated for them.
    pub cals_used: BTreeSet<Cal>,
    /// Literals the strategy writes in bracket arguments, by rule label:
    /// its own degrees of freedom (W5), substituted into library instances.
    pub bracket_literals: Vec<(String, Lit, Span)>,
}

impl<'a> Checker<'a> {
    fn new(ws: &'a Workspace, root: &'a ast::Strategy) -> Checker<'a> {
        Checker {
            ws,
            root,
            diags: vec![],
            class: None,
            class_name: String::new(),
            base: Resolution::D1,
            unit_word: "units".into(),
            demand: Demand::default(),
            calendars: BTreeMap::new(),
            cal_names: BTreeMap::new(),
            decision_cal: Cal::Base,
            clock_offset: None,
            series: BTreeMap::new(),
            vocab: BTreeMap::new(),
            exec: None,
            report: None,
            silence_flat: false,
            default_order: "market".into(),
            observes: vec![],
            instruments: vec![],
            relations: BTreeMap::new(),
            params: BTreeMap::new(),
            libs: vec![],
            templates: BTreeMap::new(),
            strategy_templates: BTreeMap::new(),
            strategy_rels: BTreeMap::new(),
            class_decls: vec![],
            out_prov: HashMap::new(),
            pending: Default::default(),
            rules: vec![],
            infos: vec![],
            var_cals: vec![],
            reserved_declared: HashSet::new(),
            scope_incomplete: false,
            demand_empty: false,
            scc_order: None,
            complete_set: BTreeSet::new(),
            dof: DegreesOfFreedom::default(),
            cals_used: BTreeSet::new(),
            bracket_literals: Vec::new(),
        }
    }

    pub fn diag(&mut self, code: Code, unit: &str, rule: Option<String>, span: Span, message: impl Into<String>) {
        let severity = if code.is_warning() { Severity::Warning } else { Severity::Error };
        self.diags.push(Diagnostic {
            code,
            severity,
            unit: unit.to_string(),
            rule,
            span,
            message: message.into(),
        });
    }

    fn unit_diag(&mut self, code: Code, span: Span, message: impl Into<String>) {
        let unit = self.root.name.clone();
        self.diag(code, &unit, None, span, message);
    }

    /// The blocks and their fields (WF-14 P, WF-17 Q, WF-16 X on the
    /// declarations), the class catalog, the libraries in scope, the
    /// parameters (T) and the configuration.
    fn configure(&mut self) {
        let root = self.root;
        let root_name = root.name.clone();
        // Duplicate strategies in the workspace.
        let same: Vec<Span> = self.ws.strategies().filter(|s| s.name == root.name).map(|s| s.span).collect();
        if same.len() > 1 {
            let at: Vec<String> = same.iter().map(|s| s.to_string()).collect();
            self.unit_diag(
                Code::U,
                root.span,
                format!("strategy `{}` is declared twice in the workspace (at {}); rename or remove one", root.name, at.join(" and ")),
            );
        }
        // Blocks written twice.
        let mut seen: HashMap<&str, Span> = HashMap::new();
        for (b, sp) in &root.blocks {
            if b == "uses" {
                continue;
            }
            if let Some(first) = seen.insert(b.as_str(), *sp) {
                self.unit_diag(Code::P, *sp, format!("the `{}` block is written twice (first at {}); a strategy has one of each block", b, first));
            }
        }
        // Missing blocks.
        let required: [(&str, bool); 8] = [
            ("data", root.data.is_some()),
            ("clock", root.clock.is_some()),
            ("universe", root.universe.is_some()),
            ("params", root.params.is_some()),
            ("rules", root.rules.is_some()),
            ("decisions", root.decisions.is_some()),
            ("execution", root.execution.is_some()),
            ("report", root.report.is_some()),
        ];
        for (name, present) in required {
            if !present {
                let what = match name {
                    "data" => "the demand: the class and what the program needs",
                    "clock" => "the decision calendar: `clock { decide on base }`",
                    "universe" => "the entity domain: rules for `universe(A, T)`",
                    "params" => "the parameters (an empty block when there are none)",
                    "rules" => "the relations and rules (an empty block when there are none)",
                    "decisions" => "`silence`, `orders` and the decide rules",
                    "execution" => "the executor's configuration: a preset or the fifteen fields",
                    _ => "the reporting calendar and the periods a year",
                };
                self.unit_diag(
                    Code::P,
                    root.span,
                    format!("strategy `{}` has no `{}` block; every block is present in a closed program ({})", root.name, name, what),
                );
            }
        }
        // The data block and the class.
        if let Some(data) = &root.data {
            self.demand(data);
        }
        // Calendars: `base` always exists; the clock names the decision calendar.
        self.calendars.insert("base".into(), Cal::Base);
        self.cal_names.insert(Cal::Base, "base".into());
        if let Some(cb) = &root.calendars {
            for (name, cal, sp) in &cb.decls {
                if self.calendars.contains_key(name) {
                    self.unit_diag(Code::X, *sp, format!("calendar `{}` is declared twice", name));
                    continue;
                }
                if cal.is_sessions() && self.class_name != "future" && self.class_name != "any" && self.class.map(|c| !c.rels.iter().any(|r| r.name == "session")).unwrap_or(false) {
                    self.unit_diag(Code::X, *sp, format!("calendar `{}` is `{}`, but class `{}` has no `session` relation", name, cal, self.class_name));
                }
                if cal.is_sessions() && !self.demand.sessions && self.demand.needs > 0 {
                    self.unit_diag(Code::X, *sp, format!("calendar `{}` is `{}`, but the data block does not `need sessions`", name, cal));
                }
                if matches!(cal, Cal::Weeks(0) | Cal::Months(0) | Cal::Years(0)) {
                    self.unit_diag(Code::X, *sp, format!("calendar `{}` counts zero periods", name));
                }
                self.calendars.insert(name.clone(), cal.clone());
                self.cal_names.entry(cal.clone()).or_insert_with(|| name.clone());
            }
        }
        if let Some(clock) = &root.clock {
            match self.calendars.get(&clock.on.0) {
                Some(c) => self.decision_cal = c.clone(),
                None => {
                    let known: Vec<&str> = self.calendars.keys().map(|k| k.as_str()).collect();
                    self.unit_diag(
                        Code::X,
                        clock.on.1,
                        format!("the clock decides on `{}`, which is not a declared calendar (declared: {})", clock.on.0, known.join(", ")),
                    );
                }
            }
            if let Some((Lit::Duration(d), sp)) = &clock.offset {
                if self.decision_cal != Cal::Base {
                    self.unit_diag(Code::X, *sp, "a session offset applies to a decision clock on `base`; a coarser calendar decides at its bar");
                } else if !self.base.is_sub_daily() {
                    self.unit_diag(Code::X, *sp, format!("a session offset needs a sub-daily base; the class's base is {}", self.base));
                } else if !self.demand.sessions && self.demand.needs > 0 {
                    self.unit_diag(Code::X, *sp, "a session offset needs the sessions: add `need sessions` to the data block");
                }
                self.clock_offset = Some(*d);
            }
        }
        // Libraries, transitively, in `uses` order (a library's own `uses` after it).
        let mut order: Vec<&'a ast::Library> = Vec::new();
        let mut stack: Vec<(String, Span, String)> = root.uses.iter().map(|(l, sp)| (l.clone(), *sp, root.name.clone())).collect();
        stack.reverse();
        let mut visited: HashSet<String> = HashSet::new();
        while let Some((name, sp, by)) = stack.pop() {
            if !visited.insert(name.clone()) {
                continue;
            }
            match self.ws.library(&name) {
                Some(lib) => {
                    let copies: Vec<Span> = self.ws.libraries().filter(|l| l.name == name).map(|l| l.span).collect();
                    if copies.len() > 1 {
                        let at: Vec<String> = copies.iter().map(|s| s.to_string()).collect();
                        self.diag(
                            Code::U,
                            &name,
                            None,
                            copies[1],
                            format!("library `{}` is declared twice in the workspace (at {}); rename or remove one", name, at.join(" and ")),
                        );
                    }
                    order.push(lib);
                    for (l, lsp) in lib.uses.iter().rev() {
                        stack.push((l.clone(), *lsp, lib.name.clone()));
                    }
                }
                None => {
                    self.scope_incomplete = true;
                    self.diag(Code::U, &by, None, sp, format!("library `{}` is not in the workspace", name));
                }
            }
        }
        for lib in &order {
            // A library's own demand must be covered by the strategy's.
            if let Some(d) = &lib.data {
                if let Some((c, csp)) = &d.class {
                    if c != "any" && *c != self.class_name && !self.class_name.is_empty() {
                        self.diag(
                            Code::E,
                            &root_name,
                            None,
                            *csp,
                            format!("library `{}` is written for class `{}`, and strategy `{}` reads class `{}`", lib.name, c, root.name, self.class_name),
                        );
                    }
                }
                if self.demand.needs > 0 {
                    for n in &d.needs {
                        let missing = match &n.kind {
                            NeedKind::Bars { .. } => self.demand.bars.is_none(),
                            NeedKind::Actions => !self.demand.actions,
                            NeedKind::Listing => !self.demand.listing,
                            NeedKind::Lists(_) => self.demand.lists.is_empty(),
                            NeedKind::Series(_) => self.demand.series.is_empty(),
                            NeedKind::Sessions => !self.demand.sessions,
                            NeedKind::PointValue => !self.demand.point_value,
                            NeedKind::Commission => !self.demand.commission,
                            NeedKind::Instruments { .. } => false,
                            NeedKind::Chain(_) => self.demand.chains.is_empty(),
                        };
                        if missing {
                            let word = match &n.kind {
                                NeedKind::Bars { .. } => "bars",
                                NeedKind::Actions => "actions",
                                NeedKind::Listing => "listing",
                                NeedKind::Lists(_) => "lists",
                                NeedKind::Series(_) => "series",
                                NeedKind::Sessions => "sessions",
                                NeedKind::PointValue => "point_value",
                                NeedKind::Commission => "commission",
                                NeedKind::Instruments { .. } => "instruments",
                                NeedKind::Chain(_) => "chain",
                            };
                            self.diag(
                                Code::E,
                                &root_name,
                                None,
                                root.data.as_ref().map(|d| d.span).unwrap_or(root.span),
                                format!("library `{}` needs `{}`, which the data block of `{}` does not ask for; add `need {}`", lib.name, word, root.name, word),
                            );
                        }
                    }
                }
            }
            for decl in &lib.rules.rels {
                if is_reserved_relation(&decl.name, !decl.inputs.is_empty()) {
                    self.diag(
                        Code::U,
                        &lib.name,
                        None,
                        decl.span,
                        format!("`{}` is a builtin or keyword and cannot be declared as a relation; choose another name", decl.name),
                    );
                    self.reserved_declared.insert(decl.name.clone());
                    continue;
                }
                if let Some(prev) = self.templates.get(&decl.name) {
                    if prev.lib != lib.name {
                        self.diag(
                            Code::U,
                            &lib.name,
                            None,
                            decl.span,
                            format!("relation `{}` is declared in library `{}` too; a name is declared once in scope", decl.name, prev.lib),
                        );
                        continue;
                    }
                }
                let rules: Vec<ast::Rule> = lib.rules.rules.iter().filter(|r| r.head.name == decl.name).cloned().collect();
                self.templates.insert(
                    decl.name.clone(),
                    Template {
                        lib: lib.name.clone(),
                        decl: decl.clone(),
                        rules,
                    },
                );
            }
            for r in &lib.rules.rules {
                if !lib.rules.rels.iter().any(|d| d.name == r.head.name) && !self.reserved_declared.contains(&r.head.name) {
                    self.diag(
                        Code::U,
                        &lib.name,
                        None,
                        r.span,
                        format!("rule for `{}` in library `{}`, which declares no such relation", r.head.name, lib.name),
                    );
                }
            }
        }
        self.libs = order;
        // Parameters (WF-3 on their defaults and ranges).
        if let Some(pb) = &root.params {
            for p in &pb.params {
                if self.params.contains_key(&p.name) {
                    self.unit_diag(Code::U, p.span, format!("parameter `{}` is declared twice", p.name));
                    continue;
                }
                if is_reserved_param(&p.name) {
                    self.unit_diag(Code::U, p.span, format!("`{}` is a builtin or keyword and cannot be a parameter name", p.name));
                    continue;
                }
                let Some(ty) = Ty::parse(&p.ty.name, p.ty.arg.as_deref()) else {
                    self.unit_diag(
                        Code::T,
                        p.ty.span,
                        format!(
                            "`{}` is not a type (Scalar, Count, Duration, Price<USD>, Notional<USD>, Quantity<Shares>, Timestamp, Label, the class's entity)",
                            p.ty.name
                        ),
                    );
                    continue;
                };
                let mut value = p.value.clone();
                let actual = value.ty();
                // A mistyped default is reported once; the parameter is
                // still declared with its type, so its uses resolve.
                let mistyped = !types::compat(&ty, &actual);
                if mistyped {
                    let what = if actual == Ty::StrLit {
                        "a string literal (an entity or a label)".to_string()
                    } else {
                        actual.to_string()
                    };
                    let hint = match (&ty, &value) {
                        (Ty::Quantity(d), Lit::Money(x, c)) if d.is_price() => format!("; a price is written per unit, `{} {}/{}`", x, c, self.unit_word.trim_end_matches('s')),
                        (Ty::Quantity(d), Lit::Int(_) | Lit::Float(_)) if d.is_quantity() => format!("; a quantity carries the class's unit, `{} {}`", value, self.unit_word),
                        _ => String::new(),
                    };
                    self.unit_diag(Code::T, p.span, format!("parameter `{}` is declared {} but its default `{}` is {}{}", p.name, ty, value, what, hint));
                }
                if let Lit::Str(s) = &value {
                    value = if ty == Ty::Label { Lit::Label(s.clone()) } else { Lit::Entity(s.clone()) };
                }
                if let (Some((lo, hi)), false) = (&p.range, mistyped) {
                    if !types::compat(&ty, &lo.ty()) || !types::compat(&ty, &hi.ty()) {
                        self.unit_diag(Code::T, p.span, format!("range of parameter `{}` must be {}", p.name, ty));
                    } else if lit_order(lo, hi) == Some(std::cmp::Ordering::Greater) {
                        self.unit_diag(
                            Code::T,
                            p.span,
                            format!("range of parameter `{}` is inverted: `{}..{}` has its lower bound above its upper bound", p.name, lo, hi),
                        );
                    } else if lit_order(lo, &value) == Some(std::cmp::Ordering::Greater) || lit_order(&value, hi) == Some(std::cmp::Ordering::Greater) {
                        self.unit_diag(
                            Code::T,
                            p.span,
                            format!("default of parameter `{}` is `{}`, outside its declared range `{}..{}`", p.name, value, lo, hi),
                        );
                    }
                }
                self.params.insert(
                    p.name.clone(),
                    Param {
                        name: p.name.clone(),
                        ty,
                        value,
                        range: p.range.clone(),
                        span: p.span,
                    },
                );
            }
        }
        // Decisions: silence and orders.
        if let Some(d) = &root.decisions {
            match &d.silence {
                Some((w, sp)) => match w.as_str() {
                    "hold" => self.silence_flat = false,
                    "flat" => self.silence_flat = true,
                    other => self.unit_diag(Code::P, *sp, format!("`silence {}`: the admissible values are `hold` and `flat`", other)),
                },
                None => self.unit_diag(Code::P, d.span, "the decisions block names no `silence`; write `silence hold` or `silence flat`"),
            }
            match &d.orders {
                Some((w, sp)) => {
                    if ORDER_KINDS.contains(&w.as_str()) {
                        self.default_order = w.clone();
                    } else {
                        self.unit_diag(Code::P, *sp, format!("`orders {}`: the admissible values are market, moo, moc and moo_moc", w));
                    }
                }
                None => self.unit_diag(Code::P, d.span, "the decisions block names no `orders`; write `orders market` (or moo, moc, moo_moc)"),
            }
        }
        // Execution and report.
        if let Some(eb) = &root.execution {
            match config::expand_execution(eb, &self.ws.presets()) {
                Ok(x) => self.exec = Some(x),
                Err(errs) => {
                    for (sp, m) in errs {
                        self.unit_diag(Code::P, sp, m);
                    }
                }
            }
        }
        if let Some(exec) = self.exec.clone() {
            if exec.orders == "moo_moc" && !self.base.is_sub_daily() && self.demand.needs > 0 {
                self.unit_diag(Code::P, root.span, "`orders moo_moc` is an intraday order and needs a sub-daily base");
            }
            if let Some(c) = &exec.price_cal {
                if !self.calendars.contains_key(c) {
                    self.unit_diag(Code::X, root.span, format!("the execution block's price relation is on calendar `{}`, which is not declared", c));
                }
            }
            if exec.commission == Commission::PerContract && !self.demand.commission && self.demand.needs > 0 {
                self.unit_diag(Code::P, root.span, "`commission per_contract from catalog` needs the catalog's commission: add `need commission`");
            }
        }
        if let Some(rb) = &root.report {
            match config::expand_report(rb) {
                Ok(r) => {
                    if !self.calendars.contains_key(&r.calendar) {
                        let known: Vec<&str> = self.calendars.keys().map(|k| k.as_str()).collect();
                        self.unit_diag(
                            Code::X,
                            rb.calendar.as_ref().map(|c| c.1).unwrap_or(rb.span),
                            format!("the report calendar `{}` is not declared (declared: {})", r.calendar, known.join(", ")),
                        );
                    }
                    self.report = Some(r);
                }
                Err(errs) => {
                    for (sp, m) in errs {
                        self.unit_diag(Code::P, sp, m);
                    }
                }
            }
        }
        // The instruments table (W9, counted).
        if let Some(u) = &root.universe {
            if let Some((rows, sp)) = &u.instruments {
                let mut seen: HashSet<&str> = HashSet::new();
                for (name, from, isp) in rows {
                    if !seen.insert(name) {
                        self.unit_diag(Code::U, *isp, format!("instrument `{}` is listed twice in the instruments table", name));
                    }
                    self.instruments.push((name.clone(), *from));
                }
                let dates = rows.iter().filter(|(_, f, _)| f.is_some()).count();
                self.unit_diag(
                    Code::W9,
                    *sp,
                    format!(
                        "the instruments table is a hindsight constant: {} names and {} dates chosen after the fact, counted as degrees of freedom",
                        rows.len(),
                        dates
                    ),
                );
                self.dof.instruments = rows.len() + dates;
            }
        }
    }

    /// The demand (WF-17 Q): the class catalog, the needs, and the
    /// primitives they bring into scope.
    fn demand(&mut self, data: &ast::DataBlock) {
        let root_name = self.root.name.clone();
        let Some((class, csp)) = &data.class else {
            self.unit_diag(Code::P, data.span, "the data block names no `class`; write `class equity`, `class future` or `class series`");
            return;
        };
        self.class_name = class.clone();
        let Some(cls) = self.ws.class(class) else {
            self.scope_incomplete = true;
            let known: Vec<String> = self.ws.units.iter().filter_map(|u| if let Unit::Class(c) = u { Some(c.name.clone()) } else { None }).collect();
            self.unit_diag(
                Code::U,
                *csp,
                format!(
                    "class `{}` is not in the catalog (the classes are {})",
                    class,
                    if known.is_empty() { "none".to_string() } else { known.join(", ") }
                ),
            );
            return;
        };
        self.class = Some(cls);
        self.base = cls.base.unwrap_or(Resolution::D1);
        self.unit_word = cls.unit.clone().unwrap_or_else(|| "units".into());
        // The series catalog: each name's native calendar (3.4).
        if let Some(sc) = self.ws.class("series") {
            for (kind, items, _) in &sc.vocab {
                if kind == "names" {
                    for (name, res) in items {
                        if let Some(r) = res {
                            self.series.insert(name.clone(), *r);
                        }
                    }
                }
            }
        }
        for (kind, items, _) in &cls.vocab {
            self.vocab.insert(kind.clone(), items.iter().map(|(s, _)| s.clone()).collect());
            if kind == "names" {
                for (name, res) in items {
                    if let Some(r) = res {
                        self.series.insert(name.clone(), *r);
                    }
                }
            }
        }
        let mut d = Demand {
            class: class.clone(),
            prefer: "latest".into(),
            ..Default::default()
        };
        d.needs = data.needs.len();
        for Need { kind, complete, span } in &data.needs {
            match kind {
                NeedKind::Bars { res, from } => {
                    if d.bars.is_some() {
                        self.unit_diag(Code::Q, *span, "`need bars` is written twice");
                    }
                    let r = res.unwrap_or(self.base);
                    if r.finer_than(self.base) {
                        // The program's base is the finest bars it needs;
                        // whether a bundle holds them is the run's
                        // resolution of the demand (WF-17 defers it).
                        self.base = r;
                    }
                    if r != self.base && !r.as_calendar().refines(&Cal::Years(1)) {
                        self.unit_diag(Code::Q, *span, format!("`need bars {}` is not a calendar over the class's base", r));
                    }
                    d.bars = Some(r);
                    d.bars_from = *from;
                    d.bars_complete = *complete;
                }
                NeedKind::Actions => d.actions = true,
                NeedKind::Listing => d.listing = true,
                NeedKind::Lists(names) => {
                    for n in names {
                        if let Some(lists) = self.vocab.get("lists") {
                            if !lists.contains(n) {
                                self.unit_diag(Code::V, *span, format!("list `{}` is not a list of class `{}` (the lists are {})", n, class, lists.join(", ")));
                            }
                        }
                        d.lists.push(n.clone());
                    }
                }
                NeedKind::Series(items) => {
                    for (name, res) in items {
                        match (res, self.series.get(name)) {
                            (Some(r), Some(native)) if r != native => {
                                self.unit_diag(Code::Q, *span, format!("series `{}` is held at {}, and the need asks for {}", name, native, r));
                            }
                            (Some(r), None) => {
                                self.series.insert(name.clone(), *r);
                            }
                            (None, None) => {
                                self.unit_diag(Code::V, *span, format!("series `{}` is not a series of the catalog and the need names no resolution", name));
                            }
                            _ => {}
                        }
                        d.series.push((name.clone(), *res));
                    }
                    d.series_complete = d.series_complete || *complete;
                }
                NeedKind::Sessions => d.sessions = true,
                NeedKind::PointValue => d.point_value = true,
                NeedKind::Commission => d.commission = true,
                NeedKind::Instruments { attr, value } => d.instruments_where.push((attr.clone(), value.clone())),
                NeedKind::Chain(c) => {
                    // A chain is the option class's (A-D23): its contracts
                    // are the class's instruments, its spot the
                    // underlying's price.
                    if class != "option" {
                        self.unit_diag(
                            Code::Q,
                            *span,
                            format!("`need chain` reads the option class's chains, and this program's class is `{}` (write `class option`)", class),
                        );
                    }
                    if duration_days(&c.expiry.0) > duration_days(&c.expiry.1) {
                        self.unit_diag(Code::Q, *span, format!("the expiry range {}..{} of a chain is empty", c.expiry.0, c.expiry.1));
                    }
                    if !(c.moneyness.0 > 0.0 && c.moneyness.0 <= c.moneyness.1) {
                        self.unit_diag(
                            Code::Q,
                            *span,
                            format!("the moneyness range {}..{} of a chain is not a positive interval (strike over spot)", c.moneyness.0, c.moneyness.1),
                        );
                    }
                    if *complete {
                        self.unit_diag(Code::Q, *span, "a chain is not `complete`: which contracts list is the exchange's, not a review's");
                    }
                    d.chains.push(ChainSpec {
                        underlying: c.underlying.clone(),
                        expiry: c.expiry,
                        moneyness: c.moneyness,
                        right: c.right.clone(),
                        spot: c.spot.clone().unwrap_or_else(|| "close".into()),
                    });
                }
            }
        }
        // A classification is a point-in-time membership, read with the
        // lists; a research-defined availability (`usable`) with the listing.
        d.classification = !d.lists.is_empty();
        d.usable = d.listing;
        if let Some((r, _)) = &data.require {
            d.require_reviewed = *r == Require::Reviewed;
        } else {
            self.unit_diag(Code::Q, data.span, "the data block says neither `require reviewed` nor `allow unreviewed`");
        }
        if let Some((b, _)) = &data.budget {
            d.budget = Some(BudgetSpec {
                money: b.money,
                bytes: b.bytes,
                requests: b.requests,
                seconds: b.seconds,
            });
        }
        if let Some((p, _)) = &data.prefer {
            d.prefer = match p {
                Prefer::Latest => "latest".into(),
                Prefer::Version(v) => format!("version {}", v),
                Prefer::BaseFinest => "base finest".into(),
            };
        } else {
            self.unit_diag(Code::Q, data.span, "the data block names no `prefer` (latest, version V, or base finest)");
        }
        if data.needs.is_empty() {
            self.demand_empty = true;
            self.unit_diag(
                Code::Q,
                data.span,
                format!(
                    "the data block of `{}` has no `need`: a demand is a conjunctive query over the catalog with at least one need (bars, actions, listing, lists, series, sessions, point_value, commission)",
                    root_name
                ),
            );
        }
        // The primitives in scope: the class's relations the needs admit,
        // and the series class's `value` when series are needed.
        let mut class_rels: Vec<ast::RelDecl> = cls.rels.clone();
        if !d.series.is_empty() && !class_rels.iter().any(|r| r.name == "value") {
            if let Some(sc) = self.ws.class("series") {
                class_rels.extend(sc.rels.iter().filter(|r| r.name == "value").cloned());
            }
        }
        self.class_decls = class_rels.clone();
        let series_native = d.series.iter().filter_map(|(n, r)| r.or_else(|| self.series.get(n).copied())).min();
        let catalog_native = if self.base.is_sub_daily() { Some(Resolution::D1) } else { None };
        for decl in &class_rels {
            if !d.admits(&decl.name) {
                continue;
            }
            let Some(mut sig) = self.signature_of(decl, &self.class_name.clone(), Cal::Base) else { continue };
            sig.kind = Kind::Primitive { class: class.clone() };
            let bars = matches!(decl.name.as_str(), "open" | "high" | "low" | "close" | "volume");
            sig.complete = if bars {
                d.bars_complete
            } else if decl.name == "value" {
                d.series_complete
            } else {
                decl.complete
            };
            sig.native = if bars || decl.name == "session" {
                None
            } else if decl.name == "value" {
                series_native.or(Some(Resolution::D1))
            } else {
                catalog_native
            };
            if decl.name == "session" {
                // `session(A, S, Open)` is keyed by the session label (3.5).
                sig.cal = Cal::Sessions;
            }
            self.relations.insert(sig.name.clone(), sig);
        }
        self.demand = d;
    }

    /// The core signature of a declaration: entity sorts, the key with its
    /// calendar, the value types.
    pub(crate) fn signature_of(&mut self, decl: &ast::RelDecl, unit: &str, cal: Cal) -> Option<Signature> {
        let mut args = Vec::new();
        let mut ok = true;
        for a in &decl.args {
            if a.key {
                args.push(Arg {
                    name: a.name.clone(),
                    mode: Mode::Key,
                    ty: Ty::Timestamp,
                });
                continue;
            }
            let Some(tn) = &a.ty else { continue };
            let ty = match Ty::parse(&tn.name, tn.arg.as_deref()) {
                Some(t) => t,
                None if tn.name.len() <= 2 && tn.name.chars().all(|c| c.is_ascii_uppercase() || c.is_ascii_digit()) => Ty::DimVar(tn.name.clone()),
                None => {
                    self.diag(Code::T, unit, None, tn.span, format!("`{}` is not a type in the signature of `{}`", tn.name, decl.name));
                    ok = false;
                    continue;
                }
            };
            args.push(Arg {
                name: a.name.clone(),
                mode: Mode::Out,
                ty,
            });
        }
        if !ok {
            return None;
        }
        if !args.iter().any(|a| a.mode == Mode::Key) {
            self.diag(Code::T, unit, None, decl.span, format!("relation `{}` declares no temporal key (`@T`)", decl.name));
            return None;
        }
        Some(Signature {
            name: decl.name.clone(),
            args,
            cal,
            complete: decl.complete,
            kind: Kind::Derived { unit: unit.to_string() },
            span: decl.span,
            native: None,
        })
    }

    fn check_rules(&mut self) {
        let n = self.rules.len();
        self.infos = vec![None; n];
        // Rules are judged dependencies first (the condensation order of
        // the relation graph), so that a relation's Timestamp outputs carry
        // the provenance its rules give them when a later rule keys on them
        // (`prev_row(A, T, T1)` binds a strictly earlier bar).
        let nodes: Vec<String> = self.relations.keys().cloned().collect();
        let index: HashMap<&str, usize> = nodes.iter().enumerate().map(|(i, nm)| (nm.as_str(), i)).collect();
        let mut adj = vec![vec![]; nodes.len()];
        for r in &self.rules {
            let Some(&h) = index.get(r.head.name.as_str()) else { continue };
            let mut lits = Vec::new();
            r.body.iter().for_each(|l| l.walk(&mut lits));
            for l in lits {
                let name = match l {
                    Literal::Atom(a) | Literal::Neg(a) | Literal::AsOf { atom: a, .. } | Literal::Top { atom: a, .. } => &a.name,
                    _ => continue,
                };
                if let Some(&b) = index.get(name.as_str()) {
                    adj[h].push(b);
                }
            }
        }
        let scc = tarjan(nodes.len(), &adj);
        let mut order: Vec<usize> = (0..n).collect();
        order.sort_by_key(|&i| {
            (
                index.get(self.rules[i].head.name.as_str()).map(|&k| scc.id[k]).unwrap_or(usize::MAX),
                self.rules[i].head.name.clone(),
                i,
            )
        });
        let mut k = 0;
        while k < order.len() {
            let head = self.rules[order[k]].head.name.clone();
            let mut provs: Vec<Vec<TimeProv>> = Vec::new();
            while k < order.len() && self.rules[order[k]].head.name == head {
                let i = order[k];
                let rule = self.rules[i].clone();
                match rule::analyze_full(self, i, &rule) {
                    Some(a) => {
                        if let Some(ctor) = a.head_ctor {
                            if let Some(Term::Ctor(c, _, _)) = self.rules[i].head.terms.get_mut(1) {
                                *c = ctor;
                            }
                        }
                        provs.push(a.head_prov);
                        self.infos[i] = Some(a.info);
                    }
                    None => provs.push(vec![]),
                }
                k += 1;
            }
            // The meet over the rules of each Timestamp output's provenance.
            let Some(sig) = self.relations.get(&head).cloned() else { continue };
            let key = sig.key_pos();
            let mut out: Vec<Option<TimeProv>> = vec![None; sig.args.len()];
            for (j, arg) in sig.args.iter().enumerate() {
                if Some(j) == key || arg.ty != Ty::Timestamp {
                    continue;
                }
                let mut acc: Option<TimeProv> = None;
                for p in &provs {
                    let q = match p.get(j) {
                        Some(TimeProv::Strict) => TimeProv::Strict,
                        Some(TimeProv::Head) | Some(TimeProv::Causal) => TimeProv::Causal,
                        _ => TimeProv::Other,
                    };
                    acc = Some(match (acc, q) {
                        (None, q) => q,
                        (Some(TimeProv::Strict), TimeProv::Strict) => TimeProv::Strict,
                        (Some(TimeProv::Strict | TimeProv::Causal), TimeProv::Causal) | (Some(TimeProv::Causal), TimeProv::Strict) => TimeProv::Causal,
                        _ => TimeProv::Other,
                    });
                }
                out[j] = acc.filter(|p| *p != TimeProv::Other);
            }
            self.out_prov.insert(head, out);
        }
        // Resolve string literals by the type the rule pass found.
        for i in 0..self.rules.len() {
            let Some(info) = self.infos[i].as_ref() else { continue };
            if info.str_types.is_empty() {
                continue;
            }
            let types: HashMap<(u32, u32), Ty> = info.str_types.iter().map(|(sp, ty)| ((sp.line, sp.col), ty.clone())).collect();
            dof::literals_in_rule_mut(&mut self.rules[i], &mut |lit, span| {
                if let Lit::Str(s) = lit {
                    match types.get(&(span.line, span.col)) {
                        Some(Ty::Label) => *lit = Lit::Label(s.clone()),
                        Some(_) => *lit = Lit::Entity(s.clone()),
                        None => {}
                    }
                }
            });
        }
    }

    /// Program-level judgments: Z, R, N, S, K, O, W1, W2, W3, W5, W6; and
    /// the diagnostic order.
    fn program_checks(&mut self) {
        let root_name = self.root.name.clone();
        // Z: at least one decide rule.
        if self.root.decisions.as_ref().map(|d| !d.rules.iter().any(|r| r.head.name == "decide")).unwrap_or(false) {
            let sp = self.root.decisions.as_ref().map(|d| d.span).unwrap_or(self.root.span);
            self.unit_diag(Code::Z, sp, "the decisions block has no decide rule; at least one is required");
        }
        // O: observations name relations of the program. A relation declared
        // on several calendars is instantiated on each (section 3), and
        // observing it observes every instance.
        let observes: Vec<ast::Observe> = self.root.rules.as_ref().map(|r| r.observes.clone()).unwrap_or_default();
        for o in &observes {
            let instances: Vec<String> = if self.relations.contains_key(&o.rel) {
                vec![o.rel.clone()]
            } else {
                self.relations.keys().filter(|k| k.split_once('@').is_some_and(|(r, _)| r == o.rel)).cloned().collect()
            };
            let Some(sig) = instances.first().and_then(|r| self.relations.get(r)).cloned() else {
                self.unit_diag(Code::O, o.span, format!("`observe {}` names no relation of the program", o.rel));
                continue;
            };
            let mut wheres = Vec::new();
            let mut bad = false;
            for (v, lit, vsp) in &o.wheres {
                let Some(arg) = sig.args.iter().find(|a| a.name == *v) else {
                    let names: Vec<&str> = sig.args.iter().map(|a| a.name.as_str()).collect();
                    self.unit_diag(Code::O, *vsp, format!("`{}` is not an argument of `{}` (its arguments are {})", v, o.rel, names.join(", ")));
                    bad = true;
                    continue;
                };
                if !(arg.ty.is_entity() || arg.ty == Ty::Label) {
                    self.unit_diag(Code::O, *vsp, format!("`where {} = ...` binds an identity argument; `{}` of `{}` is a {}", v, v, o.rel, arg.ty));
                    bad = true;
                    continue;
                }
                let lit = match lit {
                    Lit::Str(s) => {
                        if arg.ty == Ty::Label {
                            Lit::Label(s.clone())
                        } else {
                            Lit::Entity(s.clone())
                        }
                    }
                    other => other.clone(),
                };
                wheres.push((v.clone(), lit));
            }
            match o.trace.as_deref() {
                None | Some("relations") | Some("instances") => {}
                Some(other) => {
                    self.unit_diag(Code::O, o.span, format!("`trace {}`: the trace levels are `relations` and `instances`", other));
                    bad = true;
                }
            }
            if !bad {
                for rel in instances {
                    self.observes.push(Observe {
                        rel,
                        wheres: wheres.clone(),
                        trace_instances: o.trace.as_deref() == Some("instances"),
                        span: o.span,
                    });
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
        // S: a negative or aggregate edge inside a cycle must step strictly
        // back in time (temporal stratification, F-12 item 1).
        for (h, b, pol, span, ri) in &edges {
            if *pol == Polarity::Positive {
                continue;
            }
            if let (Some(&hi), Some(&bi)) = (index.get(h.as_str()), index.get(b.as_str())) {
                if scc_all.id[hi] == scc_all.id[bi] {
                    let info = &infos.iter().find(|(i, _)| i == ri).unwrap().1;
                    let r = info.refs.iter().find(|r| r.span == *span && r.name == *b).unwrap();
                    if matches!(r.key_prov, TimeProv::Strict | TimeProv::Unknown) {
                        continue;
                    }
                    let via = if *pol == Polarity::Negative { "negation" } else { "an aggregate" };
                    let label = rule_label(&self.rules, *ri);
                    let unit = self.rules[*ri].unit.clone();
                    self.diag(
                        Code::S,
                        &unit,
                        Some(label),
                        *span,
                        format!(
                            "`{}` depends on itself through {} (via `{}`) at the same bar; a cycle through negation or an aggregate must step strictly back in time",
                            h, via, b
                        ),
                    );
                }
            }
        }
        // R: positive cycles must step strictly back in time (WF-4).
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
                                "`{}` is recursive through `{}` but this reference is not at a strictly earlier bar; bind its temporal key through prev, prev_row or lag",
                                h, b
                            ),
                        );
                    }
                }
            }
        }
        // N: completeness fixpoint (WF-5).
        let complete = self.completeness(&infos);
        for (i, info) in &infos {
            for r in &info.refs {
                if r.polarity == Polarity::Negative && !complete.contains(&r.name) {
                    let why = self.incomplete_reason(&r.name, &infos, &complete);
                    let label = rule_label(&self.rules, *i);
                    let unit = self.rules[*i].unit.clone();
                    self.diag(Code::N, &unit, Some(label), r.span, format!("`not {}` is not permitted: {}", r.name, why));
                } else if r.in_case_guard && !complete.contains(&r.name) {
                    let why = self.incomplete_reason(&r.name, &infos, &complete);
                    let label = rule_label(&self.rules, *i);
                    let unit = self.rules[*i].unit.clone();
                    self.diag(
                        Code::N,
                        &unit,
                        Some(label),
                        r.span,
                        format!("`{}` in a case guard is negated by the later branches, and it is not closed: {}", r.name, why),
                    );
                }
            }
        }
        // K: every temporally recursive relation is keyed (WF-11).
        self.keyedness(&infos);
        // Reachability from decide and the observations (demand).
        let mut reach: HashSet<String> = HashSet::new();
        let mut stack = vec!["decide".to_string()];
        stack.extend(self.observes.iter().map(|o| o.rel.clone()));
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
        // W1: strategy-declared relations decide never reaches.
        let decls: Vec<ast::RelDecl> = self.root.rules.as_ref().map(|r| r.rels.clone()).unwrap_or_default();
        for decl in &decls {
            if self.reserved_declared.contains(&decl.name) {
                continue;
            }
            let is_instance = |k: &str| k == decl.name || k.starts_with(&format!("{}@", decl.name)) || k.starts_with(&format!("{}[", decl.name));
            let instances: Vec<String> = self.relations.keys().filter(|k| is_instance(k)).cloned().collect();
            if !instances.is_empty() && !instances.iter().any(|k| reach.contains(k)) {
                self.unit_diag(Code::W1, decl.span, format!("derived relation `{}` is not reached by any decide rule or observation", decl.name));
            }
            // W3: declared, never defined.
            if !self.rules.iter().any(|r| is_instance(&r.head.name)) {
                self.unit_diag(
                    Code::W3,
                    decl.span,
                    format!(
                        "derived relation `{}` is declared but no rule defines it; it is always empty, so every rule reading it positively can never fire",
                        decl.name
                    ),
                );
            }
        }
        // W3 for a library's declaration no rule of the library defines.
        let lib_decls: Vec<(String, String, Span)> = self
            .libs
            .iter()
            .flat_map(|lib| {
                lib.rules
                    .rels
                    .iter()
                    .filter(|d| !lib.rules.rules.iter().any(|r| r.head.name == d.name) && !is_reserved(&d.name))
                    .map(|d| (lib.name.clone(), d.name.clone(), d.span))
                    .collect::<Vec<_>>()
            })
            .collect();
        for (lib, name, span) in lib_decls {
            self.diag(
                Code::W3,
                &lib,
                None,
                span,
                format!(
                    "derived relation `{}` of library `{}` is declared but no rule defines it; it is always empty, so every rule reading it positively can never fire",
                    name, lib
                ),
            );
        }
        // W2: unused parameters.
        let mut used: HashSet<String> = HashSet::new();
        for (_, info) in &infos {
            used.extend(info.params_used.iter().cloned());
        }
        let params: Vec<Param> = self.params.values().cloned().collect();
        for p in &params {
            if !used.contains(&p.name) {
                self.unit_diag(Code::W2, p.span, format!("parameter `{}` is never referenced", p.name));
            }
        }
        // W5, W6 and the degrees of freedom.
        let rules = self.rules.clone();
        let mut dof = DegreesOfFreedom {
            params: self.params.len(),
            instruments: self.dof.instruments,
            ..Default::default()
        };
        // A literal written in a strategy's bracket is the strategy's (W5),
        // not the library instance's it was substituted into.
        let substituted: HashSet<(u32, u32)> = self.bracket_literals.iter().map(|(_, _, sp)| (sp.line, sp.col)).collect();
        let bracket_literals = self.bracket_literals.clone();
        for (i, r) in rules.iter().enumerate() {
            let own = r.unit == root_name;
            if own {
                dof.rules += 1;
            } else if !reach.contains(&r.head.name) {
                continue;
            }
            let label = rule_label(&rules, i);
            let mut seen: HashSet<(u32, u32)> = HashSet::new();
            let mut lits: Vec<(Lit, Span)> = dof::literals_in_rule(r).into_iter().filter(|(_, sp)| own || !substituted.contains(&(sp.line, sp.col))).collect();
            if own {
                lits.extend(bracket_literals.iter().filter(|(l, _, _)| *l == label).map(|(_, lit, sp)| (lit.clone(), *sp)));
            }
            for (lit, span) in lits {
                // `rows[N]` desugars its count into the minimum too: one literal.
                if !seen.insert((span.line, span.col)) {
                    continue;
                }
                if let Lit::Entity(_) = lit {
                    // The instruments table is warned once, as W9.
                    if own && r.head.name != "instruments" {
                        self.diag(
                            Code::W6,
                            &r.unit,
                            Some(label.clone()),
                            span,
                            format!(
                                "`{}` names the instrument carrying that ticker at the bundle date; it is a snapshot, not an identity (a ticker is a time-keyed relation)",
                                lit
                            ),
                        );
                    }
                    continue;
                }
                if dof::is_structural(&lit) || matches!(lit, Lit::Duration(_)) {
                    continue;
                }
                if own && r.head.name != "instruments" {
                    self.diag(
                        Code::W5,
                        &r.unit,
                        Some(label.clone()),
                        span,
                        format!(
                            "literal `{}` inside a rule is a degree of freedom the study counts; lift it into `params` so it is declared, typed and sweepable",
                            lit
                        ),
                    );
                    dof.literals.push((label.clone(), lit));
                } else if !own {
                    dof.library_literals.push((label.clone(), lit));
                }
            }
        }
        for p in &params {
            if let Lit::Entity(_) = &p.value {
                self.unit_diag(
                    Code::W6,
                    p.span,
                    format!(
                        "`{}` names the instrument carrying that ticker at the bundle date; it is a snapshot, not an identity (a ticker is a time-keyed relation)",
                        p.value
                    ),
                );
            }
        }
        self.dof = dof;
        // Order diagnostics: errors before warnings; unit-level first; then
        // by dependency rank of the rule's head; then by position.
        let rank: HashMap<String, usize> = nodes.iter().enumerate().map(|(i, nm)| (nm.clone(), scc_all.rank[i])).collect();
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
        self.diags.dedup();
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
                Kind::Derived { .. } if sig.complete => {
                    complete.insert(name.clone());
                }
                _ => {}
            }
        }
        // The universe is complete by definition (3.7), whatever it reads.
        let fixed: BTreeSet<String> = complete.clone();
        for (name, sig) in &self.relations {
            if matches!(sig.kind, Kind::Derived { .. }) && infos.iter().any(|(_, i)| i.head == *name) {
                complete.insert(name.clone());
            }
        }
        loop {
            let mut changed = false;
            for (name, sig) in &self.relations {
                if !complete.contains(name) || !matches!(sig.kind, Kind::Derived { .. }) || fixed.contains(name) {
                    continue;
                }
                let ok = infos
                    .iter()
                    .filter(|(_, i)| i.head == *name)
                    .all(|(_, info)| info.refs.iter().filter(|r| r.polarity != Polarity::Negative && !r.is_top).all(|r| complete.contains(&r.name)));
                if !ok {
                    complete.remove(name);
                    changed = true;
                }
            }
            if !changed {
                break;
            }
        }
        complete
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
                Some(Kind::Primitive { class }) => {
                    let how = match Demand::need_for(&cur) {
                        "bars" => "add `complete` to `need bars` when the store is reviewed, so that a missing bar means the instrument did not trade".to_string(),
                        "series" => "add `complete` to `need series` when the series is reviewed".to_string(),
                        _ => format!("`{}` is not declared complete in class `{}`", cur, class),
                    };
                    return format!(
                        "`{}` is not closed: `{}` is a primitive of class `{}` the demand does not require complete, so a missing tuple is unknown, not false ({})",
                        chain.join("` reads `"),
                        cur,
                        class,
                        how
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
                        None => return format!("`{}` is not closed", cur),
                    }
                }
                _ => return format!("`{}` is not declared", cur),
            }
        }
        format!("`{}` is not closed", chain.join("` reads `"))
    }

    /// WF-11 (K): a relation that recurses through its own earlier bars with
    /// more than one rule must be keyed by syntactic complementarity: every
    /// pair of its rules holds a literal and its negation (an atom against
    /// `not` of it on the same identity, a comparison against its negation,
    /// a relation defined as the negation of another, or the recursion's
    /// own guard against its negation). A rule with a case form is keyed by
    /// construction (Proposition 5.10) when it is the relation's only rule.
    fn keyedness(&mut self, infos: &[(usize, RuleInfo)]) {
        let rules = self.rules.clone();
        let mut heads: Vec<String> = rules.iter().map(|r| r.head.name.clone()).collect();
        heads.sort();
        heads.dedup();
        for head in heads {
            let idx: Vec<usize> = rules.iter().enumerate().filter(|(_, r)| r.head.name == head).map(|(i, _)| i).collect();
            if idx.len() < 2 {
                continue;
            }
            // Temporally recursive: some rule reads the head at a strictly earlier bar.
            let recursive = idx
                .iter()
                .any(|&i| infos.iter().any(|(j, info)| *j == i && info.refs.iter().any(|r| r.name == head && r.key_prov == TimeProv::Strict)));
            if !recursive {
                continue;
            }
            for a in 0..idx.len() {
                for b in a + 1..idx.len() {
                    let (ra, rb) = (&rules[idx[a]], &rules[idx[b]]);
                    if !rule::complementary(self, ra, rb) {
                        let label = rule_label(&rules, idx[b]);
                        let unit = rb.unit.clone();
                        self.diag(
                            Code::K,
                            &unit,
                            Some(label),
                            rb.span,
                            format!(
                                "`{}` recurses through its own earlier bars, and rules {} and {} are not complementary: both can fire at one key, two values at one bar; guard them by a literal and its negation, or write one rule with a case form `X = case {{ G -> e ; else -> e0 }}`",
                                head,
                                rule_label(&rules, idx[a]),
                                rule_label(&rules, idx[b])
                            ),
                        );
                    }
                }
            }
        }
    }

    fn finish(self) -> (Option<Program>, Vec<Diagnostic>) {
        let has_error = self.diags.iter().any(|d| d.severity == Severity::Error);
        if has_error {
            return (None, self.diags);
        }
        let (Some(exec), Some(report)) = (self.exec.clone(), self.report.clone()) else {
            return (None, self.diags);
        };
        let scc = self.scc_order.expect("program checks ran");
        let nodes: Vec<String> = self.relations.keys().cloned().collect();
        let mut by_rank: BTreeMap<usize, Vec<String>> = BTreeMap::new();
        for (i, nm) in nodes.iter().enumerate() {
            by_rank.entry(scc.rank[i]).or_default().push(nm.clone());
        }
        let strata: Vec<Vec<String>> = by_rank.into_values().collect();
        let infos: Vec<RuleInfo> = self.infos.into_iter().map(|i| i.expect("no errors means every rule analysed")).collect();
        let mut params: BTreeMap<String, BTreeMap<String, Param>> = BTreeMap::new();
        let mut units: BTreeSet<String> = self.rules.iter().map(|r| r.unit.clone()).collect();
        units.insert(self.root.name.clone());
        for u in units {
            params.insert(u, self.params.clone());
        }
        let clock = Clock {
            calendar: self.root.clock.as_ref().map(|c| c.on.0.clone()).unwrap_or_else(|| "base".into()),
            offset: self.clock_offset,
        };
        let program = Program {
            strategy: self.root.name.clone(),
            class: self.class_name,
            unit: self.unit_word,
            base: self.base,
            demand: self.demand,
            calendars: self.calendars,
            cal_names: self.cal_names,
            decision_cal: self.decision_cal,
            clock,
            series: self.series,
            instruments: self.instruments,
            relations: self.relations,
            params,
            rules: self.rules,
            infos,
            var_cals: self.var_cals,
            strata,
            complete: self.complete_set,
            degrees_of_freedom: self.dof,
            silence_flat: self.silence_flat,
            default_order: self.default_order,
            exec,
            report,
            observes: self.observes,
            source: self.root.clone(),
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
    let rank = s.id.clone();
    Sccs { id: s.id, rank }
}
