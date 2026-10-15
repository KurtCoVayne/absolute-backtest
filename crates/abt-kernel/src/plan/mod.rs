//! The planner (compiler design, section 5): for every rule the evaluation
//! order of its literals, for every aggregation its implementation, for
//! every relation its store, indexes and memo policy, for every stratum its
//! schedule. The plan is printable with its estimated costs and, after a
//! run, with the observed cardinalities beside them.

pub mod cost;
pub mod planner;
pub mod print;
pub mod search;

pub use planner::{plan_program, PlanInputs};

use crate::lower::Numerics;

/// How a rule's order was found.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum SearchKind {
    /// Dynamic programming over subsets: exact.
    Exact,
    /// Cheapest next literal: the fallback for long rules (section 5.3).
    Greedy,
    /// The written order (no search: the unoptimised plan).
    Written,
}

impl SearchKind {
    pub fn name(self) -> &'static str {
        match self {
            SearchKind::Exact => "exact",
            SearchKind::Greedy => "greedy",
            SearchKind::Written => "written",
        }
    }
}

/// One literal in a rule's planned order, with its estimates and, after a
/// run, its observed counts.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct LitPlan {
    /// Index in the written body.
    pub index: usize,
    pub text: String,
    pub kind: String,
    /// Estimated bindings entering and leaving the literal, per bar.
    pub card_in: f64,
    pub card_out: f64,
    /// Estimated cost of one evaluation of the literal.
    pub cost: f64,
    pub selectivity: f64,
    /// Observed (entering, leaving, calls it emptied), when a run supplied them.
    pub observed: Option<(u64, u64, u64)>,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct RulePlan {
    pub rule: usize,
    pub label: String,
    /// The evaluation order, as indices into the written body.
    pub order: Vec<usize>,
    /// The literals in that order.
    pub literals: Vec<LitPlan>,
    /// Estimated cost per bar of the chosen order and of the written order.
    pub est_cost: f64,
    pub written_cost: f64,
    pub search: SearchKind,
    /// The rewriter proved the rule infeasible (R2): no work is emitted.
    pub infeasible: bool,
    /// The rule's head is elided (R5): nothing demands it.
    pub elided: bool,
    /// The number of orders the search considered.
    pub orders_considered: usize,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct AggPlan {
    pub rule: usize,
    pub literal: usize,
    pub agg: String,
    /// Gray's class: distributive, algebraic or holistic.
    pub class: String,
    pub window: String,
    /// The kernel's implementation under the numerics setting.
    pub implementation: String,
    pub numerics: Numerics,
    /// The literal (of the same rule) whose window state this one reads (R3).
    pub shares_state_of: Option<usize>,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct RelPlan {
    pub name: String,
    pub kind: String,
    pub store: String,
    /// Kept because some rule reads the relation through `asof`.
    pub asof_index: bool,
    /// Kept because some rule reads it in a `rows` window.
    pub rows_window: bool,
    pub memo_horizon: Option<String>,
    /// `ahead-of-time` for open-loop relations, `per-bar` for closed-loop ones.
    pub schedule: String,
    pub demanded: bool,
    pub elided: bool,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct StratumPlan {
    pub index: usize,
    pub relations: Vec<String>,
    pub schedule: String,
    /// The stratum holds a cross-sectional literal (a free entity in a rule
    /// with a bound bar): a barrier between per-entity partitions.
    pub barrier: bool,
}

#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
pub struct PlanTotals {
    pub est_cost: f64,
    pub written_cost: f64,
    pub rules: usize,
    pub reordered: usize,
    pub infeasible: usize,
    pub elided: usize,
    pub greedy: usize,
}

/// The plan of a program.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct Plan {
    pub label: String,
    pub numerics: Numerics,
    pub rules: Vec<RulePlan>,
    pub aggregates: Vec<AggPlan>,
    pub relations: Vec<RelPlan>,
    pub strata: Vec<StratumPlan>,
    pub totals: PlanTotals,
    /// Where the statistics came from (`defaults` or a dataset's name).
    pub statistics: String,
    pub notes: Vec<String>,
}

impl Plan {
    /// Attach a run's observed counts to the literals.
    pub fn with_observed(mut self, counts: &[Vec<(u64, u64, u64)>]) -> Plan {
        for rp in &mut self.rules {
            for lp in &mut rp.literals {
                lp.observed = counts.get(rp.rule).and_then(|c| c.get(lp.index)).copied();
            }
        }
        self
    }
}
