//! The lowered form (compiler design, section 2.1, "Lower"): what the
//! kernel executes for a plan. The kernel keeps the checked program's rules,
//! variable slots and written literal order as its invariant representation
//! (I2: slot order fixes the order reductions and aggregations sort by), and
//! a plan is applied on top of it: per rule, the order its body literals are
//! evaluated in; the rules the rewriter proved infeasible, which contribute
//! no work; the aggregation literals that share one window state after R3;
//! and the program copy with the constants R6 folded, evaluated in place of
//! the original.

pub mod order;

use std::collections::BTreeMap;

use crate::check::Program;

/// The numerics setting of the program (compiler design, sections 4.3, 5.4
/// and 6.8): which formulas the kernel's aggregates use. This kernel has two:
/// `running` (sliding compensated sums over `rows` windows, rebuilt every N
/// rows; the default) and `two_pass` (the exact two-pass formulas, what the
/// parity runs use). `ExecConfig::running_sums` is the same switch.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Numerics {
    #[default]
    Running,
    TwoPass,
}

impl Numerics {
    pub fn from_running_sums(running: bool) -> Numerics {
        if running {
            Numerics::Running
        } else {
            Numerics::TwoPass
        }
    }
    pub fn name(self) -> &'static str {
        match self {
            Numerics::Running => "running",
            Numerics::TwoPass => "two_pass",
        }
    }
}

impl std::str::FromStr for Numerics {
    type Err = String;
    fn from_str(s: &str) -> Result<Numerics, String> {
        match s {
            "running" => Ok(Numerics::Running),
            "two_pass" | "two-pass" | "exact" => Ok(Numerics::TwoPass),
            _ => Err(format!("`{}` is not one of running, two_pass", s)),
        }
    }
}

/// The plan as the kernel applies it. Rule and literal indices are those of
/// the checked program's `rules` and of each rule's written body.
#[derive(Clone, Debug, Default)]
pub struct Lowered {
    /// A name for the plan in reports: `written` or `optimised`.
    pub label: String,
    /// Per rule, the evaluation order of its body literals (a permutation of
    /// `0..body.len()`); the written order when absent.
    pub order: Vec<Vec<usize>>,
    /// Per rule, whether it is infeasible (R2) and contributes no work.
    pub skip: Vec<bool>,
    /// (rule, literal) of an aggregation that reads the window state of
    /// another aggregation literal of the same rule (R3), by the primary's
    /// literal index.
    pub shared_windows: BTreeMap<(usize, usize), usize>,
    /// The program with R6's constants folded, evaluated in place of the
    /// checked program when present (the rules, slots and literal indices
    /// are the same; only closed subexpressions differ).
    pub program: Option<Program>,
    /// Which aggregate formulas the kernel uses.
    pub numerics: Numerics,
    /// What the lowering did and could not do, one line each.
    pub notes: Vec<String>,
}

impl Lowered {
    /// The written order of every rule, nothing skipped: the plan the
    /// unoptimised path runs (compiler design, section 6.6).
    pub fn written(prog: &Program, numerics: Numerics) -> Lowered {
        Lowered {
            label: "written".into(),
            order: prog.rules.iter().map(|r| (0..r.body.len()).collect()).collect(),
            skip: vec![false; prog.rules.len()],
            shared_windows: BTreeMap::new(),
            program: None,
            numerics,
            notes: Vec::new(),
        }
    }

    /// The program the kernel evaluates: the folded copy, or the checked one.
    pub fn program_for<'p>(&'p self, prog: &'p Program) -> &'p Program {
        self.program.as_ref().unwrap_or(prog)
    }
}
