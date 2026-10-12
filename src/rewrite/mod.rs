//! The Rewrite stage (compiler design, section 4): six rewrites on the
//! normal form, each licensed by a stated law, with a termination measure
//! and one log entry per application, applied in the fixed order R6, R1,
//! R2, R5, R3, R4 to a fixpoint of the whole sequence.
//!
//! The result is `NIR'`: the normal form with its literals hoisted (R1),
//! its infeasible rules flagged (R2), its parametric relations specialised
//! to their demand and its unreached relations elided (R5), its merged
//! aggregations sharing one summary (R3), its closed expressions folded
//! (R6), and R4 recorded as disabled under this kernel's numerics (section
//! 4.4). What the kernel needs of it is handed to the planner and the
//! lowering: the infeasible rules, the elided relations, the shared window
//! states and the program copy with folded constants.

pub mod dead;
pub mod demand;
pub mod fold;
pub mod fuse;
pub mod hoist;
pub mod merge;

use std::collections::{BTreeMap, BTreeSet};

use crate::analysis::Analysis;
use crate::check::Program;
use crate::compiler::normalise::Nir;
use crate::compiler::Stage;
use crate::lower::Numerics;

/// One application of a rewrite (section 4.8).
#[derive(Clone, Debug, serde::Serialize)]
pub struct LogEntry {
    pub rewrite: String,
    pub rule: String,
    pub span: String,
    pub before: String,
    pub after: String,
    pub law: String,
    pub measure: [usize; 2],
}

#[derive(Clone, Debug, Default, serde::Serialize)]
pub struct RewriteLog {
    pub entries: Vec<LogEntry>,
}

impl RewriteLog {
    #[allow(clippy::too_many_arguments)]
    pub fn push(&mut self, rewrite: &str, rule: &str, span: &str, before: String, after: String, law: &str, measure: [usize; 2]) {
        self.entries.push(LogEntry {
            rewrite: rewrite.into(),
            rule: rule.into(),
            span: span.into(),
            before,
            after,
            law: law.into(),
            measure,
        });
    }
    /// The JSON lines of section 4.8.
    pub fn json_lines(&self) -> String {
        self.entries.iter().map(|e| serde_json::to_string(e).unwrap_or_default()).collect::<Vec<_>>().join("\n") + if self.entries.is_empty() { "" } else { "\n" }
    }
    pub fn table(&self) -> String {
        if self.entries.is_empty() {
            return "no rewrite applied\n".into();
        }
        let mut out = String::new();
        for e in &self.entries {
            out.push_str(&format!(
                "{} {}: {} → {}  [{}; measure {} → {}]\n",
                e.rewrite, e.rule, e.before, e.after, e.law, e.measure[0], e.measure[1]
            ));
        }
        out
    }
}

/// Dead-rule verdicts from the verification tiers (R2): a rule's origin
/// index, the tier, the evidence.
#[derive(Clone, Debug)]
pub struct DeadVerdict {
    pub rule: usize,
    pub tier: String,
    pub evidence: String,
}

/// The rewritten program and what the later stages read from it.
#[derive(Clone, Debug)]
pub struct Rewritten {
    pub nir: Nir,
    pub log: RewriteLog,
    /// Infeasible rules by origin index, with the evidence.
    pub infeasible: BTreeMap<usize, String>,
    pub elided: BTreeSet<String>,
    /// (rule origin, literal written index, primary literal written index).
    pub shared: Vec<(usize, usize, usize)>,
    /// The checked program with its closed expressions folded (R6).
    pub folded: Option<Program>,
    /// Why R4 is disabled, when it had candidates.
    pub fusion_disabled: Vec<String>,
    pub passes: usize,
}

/// Run the rewrites (section 4) over a normal form.
pub fn rewrite(prog: &Program, nir: &Nir, analysis: &Analysis, dead: &[DeadVerdict], numerics: Numerics, margin_log2: i32) -> Rewritten {
    let mut nir = nir.clone();
    nir.stage = "rewritten".into();
    let mut log = RewriteLog::default();
    let mut infeasible: BTreeMap<usize, String> = BTreeMap::new();
    let mut elided: BTreeSet<String> = BTreeSet::new();
    let mut shared: Vec<(usize, usize, usize)> = Vec::new();
    let mut fusion_disabled: Vec<String> = Vec::new();
    let mut folded: Option<Program> = None;
    let mut passes = 0;
    // Each pass applies every rewrite once in the fixed order; the measures
    // of section 4.7 make the loop finite, and a pass that changes nothing
    // ends it.
    loop {
        passes += 1;
        let mut changed = false;
        changed |= fold::fold(prog, &mut nir, &mut folded, &mut log);
        changed |= hoist::hoist(prog, &mut nir, &mut log);
        changed |= dead::dead(prog, &mut nir, analysis, dead, margin_log2, &mut infeasible, &mut log);
        changed |= demand::specialise(prog, &mut nir, analysis, &mut elided, &mut log);
        changed |= merge::merge(prog, &mut nir, &mut shared, &mut log);
        changed |= fuse::fuse(prog, &mut nir, numerics, &mut fusion_disabled, &mut log);
        if !changed || passes >= 8 {
            break;
        }
    }
    Rewritten {
        nir,
        log,
        infeasible,
        elided,
        shared,
        folded,
        fusion_disabled,
        passes,
    }
}

/// The rewritten form with the program it came from, for the printable stage.
pub struct RewrittenStage<'a> {
    pub rewritten: &'a Rewritten,
    pub prog: &'a Program,
}

impl<'a> Stage for RewrittenStage<'a> {
    fn text(&self) -> String {
        let r = self.rewritten;
        let mut out = String::new();
        out.push_str(&format!(
            "rewritten in {} pass(es): {} rewrites applied; {} infeasible rule(s), {} elided relation(s), {} shared aggregation state(s){}\n",
            r.passes,
            r.log.entries.len(),
            r.infeasible.len(),
            r.elided.len(),
            r.shared.len(),
            if r.fusion_disabled.is_empty() {
                String::new()
            } else {
                format!("; R4 disabled: {}", r.fusion_disabled.join("; "))
            }
        ));
        out.push_str("\ncore forms (NIR'):\n");
        out.push_str(&r.nir.core_text());
        out.push_str("\nrewrite log:\n");
        out.push_str(&r.log.table());
        out
    }
    fn json(&self) -> serde_json::Value {
        let r = self.rewritten;
        serde_json::json!({
            "stage": "rewritten",
            "passes": r.passes,
            "rules": r.nir.rules,
            "infeasible": r.infeasible,
            "elided": r.elided,
            "shared": r.shared,
            "fusion_disabled": r.fusion_disabled,
            "log": r.log.entries,
        })
    }
}
