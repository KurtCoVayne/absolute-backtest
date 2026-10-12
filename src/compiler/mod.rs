//! The compiler (docs/compiler-design.md): six total functions on the typed
//! IR, each with a printable result, ending in a plan the kernel executes.
//! `compile` runs Normalise, Analyse, Rewrite, Plan and Lower in order and
//! keeps every stage's result; the verification tiers are run by `verify`.

pub mod normalise;
pub mod print;
pub mod sha256;

pub use normalise::{normalise, Nir};
pub use print::Stage;

use std::path::PathBuf;

use crate::check::Program;
use crate::lower::Numerics;
use crate::plan::cost::Statistics;

/// What the compiler is told (the `report` block of the second formulation;
/// options of the command line here).
#[derive(Clone, Debug)]
pub struct CompileOptions {
    pub numerics: Numerics,
    /// Run the rewrites (R1 to R6) and the planner's search; off, the plan
    /// is the written order with no rewrite (the unoptimised path of
    /// section 6.6).
    pub optimise: bool,
    /// Statistics of the bound data for the planner and the bundle-relative
    /// verdicts of Analyse.
    pub stats: Option<Statistics>,
    /// The SMT solver to drive (`z3`, `cvc5`), looked up on the PATH; none
    /// leaves tiers 3 and 4 undecided.
    pub solver: Option<String>,
    pub smt_budget_ms: u64,
    pub compile_budget_ms: u64,
    pub chc_budget_ms: u64,
    /// Where the replay files of the SMT and CHC obligations are written.
    pub replay_dir: Option<PathBuf>,
    /// The dead-literal margin of section 6.8, as a power of two (−40).
    pub margin_log2: i32,
}

impl Default for CompileOptions {
    fn default() -> CompileOptions {
        CompileOptions {
            numerics: Numerics::Running,
            optimise: true,
            stats: None,
            solver: None,
            smt_budget_ms: 2000,
            compile_budget_ms: 30_000,
            chc_budget_ms: 10_000,
            replay_dir: None,
            margin_log2: -40,
        }
    }
}

/// The normalised form of a program with its hash: the first stage.
pub fn normalised(prog: &Program) -> Nir {
    normalise(prog)
}

/// Every stage's result for a program.
pub struct Compiled {
    pub nir: Nir,
    pub analysis: crate::analysis::Analysis,
    pub rewritten: crate::rewrite::Rewritten,
    pub plan: crate::plan::Plan,
    pub lowered: crate::lower::Lowered,
}

/// Run the pipeline: Normalise, Analyse, Rewrite, Plan, Lower. With
/// `optimise` off the rewrites are not applied and the plan is the written
/// order (the unoptimised path of section 6.6). `dead` carries the
/// verdicts of the verification tiers that license R2 beyond Analyse.
pub fn compile_with(prog: &Program, opts: &CompileOptions, dead: &[crate::rewrite::DeadVerdict]) -> Compiled {
    let nir = normalise(prog);
    let analysis = crate::analysis::analyse(prog, &nir, opts.stats.as_ref(), opts.margin_log2);
    let stats = opts.stats.clone().unwrap_or_default();
    let rewritten = if opts.optimise {
        crate::rewrite::rewrite(prog, &nir, &analysis, dead, opts.numerics, opts.margin_log2)
    } else {
        crate::rewrite::Rewritten {
            nir: nir.clone(),
            log: Default::default(),
            infeasible: Default::default(),
            elided: Default::default(),
            shared: Vec::new(),
            folded: None,
            fusion_disabled: Vec::new(),
            passes: 0,
        }
    };
    let mut inputs = analysis.plan_inputs();
    inputs.infeasible = rewritten.infeasible.clone();
    inputs.elided = rewritten.elided.clone();
    inputs.shared = rewritten.shared.clone();
    let plan = crate::plan::plan_program(prog, &inputs, &stats, opts.numerics, opts.optimise);
    let lowered = crate::lower::order::lower(prog, &plan, rewritten.folded.clone(), opts.numerics);
    Compiled {
        nir,
        analysis,
        rewritten,
        plan,
        lowered,
    }
}

/// `compile_with` and no verdicts beyond Analyse.
pub fn compile(prog: &Program, opts: &CompileOptions) -> Compiled {
    compile_with(prog, opts, &[])
}
