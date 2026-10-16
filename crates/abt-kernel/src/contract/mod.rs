//! Contracts derived from structure (K-3): a total function of the checked
//! program and its analysis (Proposition K-3.2) to the preconditions the
//! binding must meet before the first barrier, the invariants the kernel
//! checks on itself at every decision barrier, the postconditions computed
//! when the run finishes, and the pre-trade limits of `execution.limits`.
//! Nothing in a contract is written by the author (A-D14). Each clause has
//! a source, a discharge (a static proof with its tier, the binding, the
//! run, or the report) and a policy; a run gives each its status
//! (`proven`, `checked`, `violated`, `warned`, `not_applicable`). The
//! checks are in `check`; `abt contract` prints the table.

pub mod check;

use std::fmt;

use serde::{Deserialize, Serialize};

use crate::check::{LimitKind, LimitPolicy, Program};
use crate::ir::{Expr, Lit, Literal, Term};

/// Where a clause is checked.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    Pre,
    Inv,
    Post,
    Limit,
}

impl Kind {
    pub fn name(self) -> &'static str {
        match self {
            Kind::Pre => "pre",
            Kind::Inv => "inv",
            Kind::Post => "post",
            Kind::Limit => "limit",
        }
    }
}

/// How a clause is discharged (Definition K-3.1).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Discharge {
    /// Proved before the run by a verification tier (0: the checker).
    Static(u8),
    /// Settled by the resolver's answer, before the first barrier.
    Bind,
    /// Checked at every decision barrier.
    Runtime,
    /// Computed after the run.
    Report,
}

impl fmt::Display for Discharge {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Discharge::Static(t) => write!(f, "static(tier {})", t),
            Discharge::Bind => write!(f, "bind"),
            Discharge::Runtime => write!(f, "runtime"),
            Discharge::Report => write!(f, "report"),
        }
    }
}

/// What a violation does.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Policy {
    /// The run is refused before its first barrier.
    Refuse,
    Warn,
    /// The run halts at the barrier, naming the clause and the values.
    Halt,
    Report,
    /// Reported; the acceptance suite halts on it (G5, G7).
    ReportHaltInCi,
    /// The next listed model is used, warned.
    FallBack,
    /// A limit's policy (K-3.5).
    Limit(LimitPolicy),
}

impl fmt::Display for Policy {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Policy::Refuse => write!(f, "refuse"),
            Policy::Warn => write!(f, "warn"),
            Policy::Halt => write!(f, "halt"),
            Policy::Report => write!(f, "report"),
            Policy::ReportHaltInCi => write!(f, "report; halt in CI"),
            Policy::FallBack => write!(f, "fall back"),
            Policy::Limit(p) => write!(f, "{}", p.name()),
        }
    }
}

/// A clause's status (K-3.6).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "status")]
pub enum Status {
    /// Not evaluated: no run.
    Open,
    /// Proved statically by the tier.
    Proven { tier: u8 },
    /// Held at every check: `times` checks.
    Checked { times: u64 },
    /// Failed, at a barrier when it has one.
    Violated { at: Option<i64>, detail: String },
    /// A warn policy fired `count` times.
    Warned { count: u64, detail: String },
    /// The clause does not apply to the program or its binding.
    NotApplicable { why: String },
}

impl fmt::Display for Status {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Status::Open => write!(f, "open"),
            Status::Proven { tier } => write!(f, "proven(tier {})", tier),
            Status::Checked { times } => write!(f, "checked({})", times),
            Status::Violated { at: Some(t), detail } => write!(f, "violated({}): {}", crate::kernel::time::format_timestamp(*t), detail),
            Status::Violated { at: None, detail } => write!(f, "violated: {}", detail),
            Status::Warned { count, detail } => write!(f, "warned({}): {}", count, detail),
            Status::NotApplicable { why } => write!(f, "not_applicable: {}", why),
        }
    }
}

/// One clause of the contract.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Clause {
    /// A stable identifier: `pre.prices`, `inv.nav`, `limit.gross`.
    pub id: String,
    pub kind: Kind,
    /// The clause as K-3 states it.
    pub text: String,
    /// The block, rule or analysis fact it follows from.
    pub source: String,
    pub discharge: Discharge,
    pub policy: Policy,
    pub status: Status,
    /// For a limit: the bars it fired at.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fired: Vec<i64>,
}

/// The contract of a program: its clauses in K-3's order.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Contract {
    pub clauses: Vec<Clause>,
}

impl Contract {
    pub fn get(&self, id: &str) -> Option<&Clause> {
        self.clauses.iter().find(|c| c.id == id)
    }

    /// Sets a clause's status; a clause the contract does not hold is
    /// ignored (a limit the program does not declare).
    pub fn set(&mut self, id: &str, status: Status) {
        if let Some(c) = self.clauses.iter_mut().find(|c| c.id == id) {
            c.status = status;
        }
    }

    /// Counts one more check of a runtime clause that held.
    pub fn checked(&mut self, id: &str) {
        if let Some(c) = self.clauses.iter_mut().find(|c| c.id == id) {
            c.status = match &c.status {
                Status::Checked { times } => Status::Checked { times: times + 1 },
                Status::Open => Status::Checked { times: 1 },
                s => s.clone(),
            };
        }
    }

    /// The clauses as the table `abt contract` prints.
    pub fn table(&self) -> String {
        let mut out = String::new();
        let w = self.clauses.iter().map(|c| c.id.len()).max().unwrap_or(4).max(6);
        out.push_str(&format!("{:<w$}  {:<5}  {:<18}  {:<16}  {:<17}  status\n", "clause", "kind", "source", "discharge", "policy", w = w));
        for c in &self.clauses {
            out.push_str(&format!(
                "{:<w$}  {:<5}  {:<18}  {:<16}  {:<17}  {}\n",
                c.id,
                c.kind.name(),
                c.source,
                c.discharge.to_string(),
                c.policy.to_string(),
                c.status,
                w = w
            ));
            out.push_str(&format!("{:<w$}  {}\n", "", c.text, w = w));
        }
        out
    }

    /// Whether every clause has a status a run reports (G7): proven,
    /// checked, warned or not applicable, and none violated or open.
    pub fn settled(&self) -> Result<(), Vec<String>> {
        let bad: Vec<String> = self
            .clauses
            .iter()
            .filter(|c| matches!(c.status, Status::Open | Status::Violated { .. }))
            .map(|c| format!("{}: {}", c.id, c.status))
            .collect();
        if bad.is_empty() {
            Ok(())
        } else {
            Err(bad)
        }
    }
}

fn clause(id: &str, kind: Kind, source: &str, discharge: Discharge, policy: Policy, text: &str) -> Clause {
    let status = match discharge {
        Discharge::Static(tier) => Status::Proven { tier },
        _ => Status::Open,
    };
    Clause {
        id: id.into(),
        kind,
        text: text.into(),
        source: source.into(),
        discharge,
        policy,
        status,
        fired: Vec::new(),
    }
}

/// The contract of a checked program (Definition K-3.1). The statuses of
/// static clauses are set; the others wait for a binding and a run.
pub fn contract(prog: &Program) -> Contract {
    use Discharge::*;
    use Kind::*;
    let mut c = vec![
        clause("pre.binding", Pre, "data", Bind, Policy::Refuse, "the demand has a binding"),
        clause(
            "pre.attributes",
            Pre,
            "data",
            Bind,
            Policy::Refuse,
            "every version the binding names has the attributes the program requires (`reviewed = complete` for a `complete` need)",
        ),
        clause(
            "pre.vocabulary",
            Pre,
            "rules",
            Bind,
            Policy::Refuse,
            "every label and series name a rule uses is in the binding's vocabulary (WF-15)",
        ),
        clause(
            "pre.instruments",
            Pre,
            "universe",
            Bind,
            Policy::Refuse,
            "every instrument of the `instruments` table is an instrument of the binding",
        ),
        clause(
            "pre.span",
            Pre,
            "analysis",
            Bind,
            Policy::Warn,
            "the binding's span covers the report window plus the program's longest lookback",
        ),
        clause("pre.params", Pre, "params", Static(0), Policy::Refuse, "every parameter value is in its declared range and of its type"),
        clause("pre.execution", Pre, "execution", Static(0), Policy::Refuse, "the execution block is complete and admissible (WF-14)"),
        clause("pre.state_bound", Pre, "analysis", Bind, Policy::Refuse, "the derived state bound is within the memory budget (4.6)"),
        clause(
            "pre.revisions",
            Pre,
            "binding",
            Bind,
            Policy::Refuse,
            "versions declaring `revisions = none` hold at most one row per identity and t",
        ),
        clause(
            "pre.prices",
            Pre,
            "binding",
            Bind,
            Policy::Refuse,
            "prices of the price relation are positive on non-future instruments over the span",
        ),
        clause(
            "pre.calendars",
            Pre,
            "binding",
            Bind,
            Policy::Refuse,
            "every bar label of the program's calendars is a label of the binding's `calendar_label`",
        ),
        clause(
            "pre.actions",
            Pre,
            "binding",
            Bind,
            Policy::Warn,
            "a close ratio outside [0.6, 1.67] between consecutive bars is explained by a split or a dividend that bar",
        ),
        clause(
            "pre.delistings",
            Pre,
            "binding",
            Bind,
            Policy::Warn,
            "an instrument whose last bar precedes the span's end has a `delisted` row",
        ),
        clause("pre.membership", Pre, "binding", Bind, Policy::Warn, "a `member` row's instrument has a bar that day"),
        clause(
            "pre.option_inputs",
            Pre,
            "execution",
            Bind,
            Policy::Refuse,
            "an option program's `option_fill` needs `quote`; `greeks data` needs `greeks`",
        ),
        clause(
            "pre.margin_rows",
            Pre,
            "execution",
            Bind,
            Policy::FallBack,
            "`margin published` needs `margin` rows covering the instruments traded",
        ),
        clause("inv.nav", Inv, "book", Runtime, Policy::Halt, "nav(T) = cash(T) + sum of position * mark * multiplier + receivables(T)"),
        clause("inv.one_decision", Inv, "decide", Runtime, Policy::Halt, "one decision per instrument per bar"),
        clause(
            "inv.no_read_past",
            Inv,
            "barrier",
            Runtime,
            Policy::Halt,
            "no read past the barrier: every data-layer answer read at T was received by at(T)",
        ),
        clause(
            "inv.keyed_store",
            Inv,
            "stored relations",
            Runtime,
            Policy::Halt,
            "every stored relation of Σ_t holds at most one tuple per identity at T",
        ),
        clause(
            "inv.keyed_recursive",
            Inv,
            "analysis",
            Static(0),
            Policy::Halt,
            "every temporally recursive derived relation is keyed (WF-11)",
        ),
        clause(
            "inv.gross",
            Inv,
            "execution",
            Runtime,
            Policy::Halt,
            "gross exposure is within the `leverage` field's multiple of equity after every fill",
        ),
        clause(
            "inv.position_limits",
            Inv,
            "execution.limits",
            Runtime,
            Policy::Halt,
            "positions never exceed the `names` and `weight` limits after a fill when the limit rejects or clamps",
        ),
        clause(
            "inv.log_fold",
            Inv,
            "events",
            Runtime,
            Policy::Halt,
            "the event log's fold equals the book: positions, cash and receivables",
        ),
        clause(
            "inv.pushdown_receipt",
            Inv,
            "pushdown",
            Runtime,
            Policy::Halt,
            "the receipt horizon of every pushed-down answer read at T is at(T)",
        ),
        clause("inv.working_orders", Inv, "executor", Runtime, Policy::Halt, "working orders are at most one per instrument"),
        clause(
            "post.decisions",
            Post,
            "executor",
            Report,
            Policy::ReportHaltInCi,
            "every order is filled, dropped, met, replaced or expired, or open at the end",
        ),
        clause(
            "post.coverage",
            Post,
            "observe",
            Report,
            Policy::Warn,
            "every relation on a path to `decide` derived at least one tuple (W8)",
        ),
        clause(
            "post.ledgers",
            Post,
            "observation",
            Report,
            Policy::Report,
            "every observed relation's ledger was written over the bars the mode permits",
        ),
        clause(
            "post.truncation",
            Post,
            "A-4.2, G5",
            Report,
            Policy::ReportHaltInCi,
            "a run stopped at a barrier equals the truncated run; a resumed run equals the unbroken run",
        ),
        clause(
            "post.final_cash",
            Post,
            "events",
            Report,
            Policy::ReportHaltInCi,
            "final cash = initial + fills (signed notional) - costs + dividends and settlements + funding",
        ),
        clause("post.bind_warnings", Post, "binding", Report, Policy::Report, "every warned bind clause is listed with its count"),
        clause(
            "post.limits",
            Post,
            "execution.limits",
            Report,
            Policy::Report,
            "every limit reports its trigger count and the bars it fired",
        ),
    ];
    for l in &prog.exec.limits {
        let mut cl = clause(
            &format!("limit.{}", l.kind.name()),
            Limit,
            "execution.limits",
            Runtime,
            Policy::Limit(l.policy),
            &format!("`{}` before accepting a bar's orders", l),
        );
        if let Some(tier) = limit_proof(prog, l) {
            cl.discharge = Static(tier);
            cl.status = Status::Proven { tier };
        }
        c.push(cl);
    }
    Contract { clauses: c }
}

/// A limit the program's decisions cannot pass (K-3.6): a `weight` limit
/// over decisions that are all target weights (`hold` with a Scalar)
/// whose weight is a constant (a literal, a parameter's value, a variable
/// the body binds to one) within it: its interval is the point. The
/// runtime check stays (C-6.8); the report says `proven`.
fn limit_proof(prog: &Program, l: &crate::check::Limit) -> Option<u8> {
    if l.kind != LimitKind::Weight {
        return None;
    }
    let mut any = false;
    for r in prog.rules.iter().filter(|r| r.head.name == "decide") {
        let Some(Term::Ctor(ctor, args, _)) = r.head.terms.get(1) else { return None };
        if ctor != "hold_weight" {
            return None;
        }
        let w = match args.get(1)? {
            Term::Lit(lit, _) => number(lit)?,
            Term::Param(p, _) => param_value(prog, &r.unit, p)?,
            Term::Var(v, _) => constant_of(prog, r, v)?,
            _ => return None,
        };
        if w.abs() > l.bound {
            return None;
        }
        any = true;
    }
    any.then_some(1)
}

fn number(l: &Lit) -> Option<f64> {
    match l {
        Lit::Int(x) => Some(*x as f64),
        Lit::Float(x) => Some(*x),
        _ => None,
    }
}

fn param_value(prog: &Program, unit: &str, name: &str) -> Option<f64> {
    number(&prog.params.get(unit)?.get(name)?.value)
}

/// The constant a body binds a variable to (`W = 0.1`, `W = w`), when the
/// rule fixes it.
fn constant_of(prog: &Program, r: &crate::ir::Rule, v: &str) -> Option<f64> {
    r.body.iter().find_map(|l| match l {
        Literal::Assign { var, expr, .. } if var == v => match expr {
            Expr::Lit(lit, _) => number(lit),
            Expr::Param(p, _) => param_value(prog, &r.unit, p),
            _ => None,
        },
        _ => None,
    })
}
