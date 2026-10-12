//! Verification (compiler design, section 6): every property is decidable
//! with its complexity, decidable but costly with its budget, or
//! undecidable with the runtime diagnostic that replaces it. The tiers run
//! in order; tier 0 is the checker, tier 1 the abstract interpretation,
//! tier 2 the exact linear feasibility of this module's `lp`, tier 3 an SMT
//! solver driven as a subprocess with replay files, tier 4 constrained Horn
//! clauses, tier 5 translation validation, tier 6 the runtime checks.
//!
//! Tier 2, and the rule-feasibility part of tier 3 when a solver is named,
//! run inside `compile` because their verdicts license R2; the rest run in
//! `verify`, after compilation, and never change the plan (section 6.5:
//! a budgeted semi-decision procedure is not a precondition of a
//! deterministic compilation).

pub mod chc;
pub mod lp;
pub mod obligations;
pub mod smt;
pub mod tv;

use crate::check::{Code, Diagnostic, Program, Severity};
use crate::compiler::{CompileOptions, Compiled, Stage};
use crate::kernel::{Dataset, ExecConfig};
use crate::rewrite::DeadVerdict;

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
pub enum Verdict {
    /// The property holds (an invariant proved, a guard pair disjoint, an
    /// identity confirmed, a judgment that passed).
    Proved,
    /// A sufficient condition did not hold: nothing follows (the relational
    /// part may still separate two guards; a candidate invariant may not be
    /// inductive).
    NotProved,
    /// The property fails with a witness (two plans that differ, a run that
    /// halted).
    Refuted,
    /// The literal, guard or rule is infeasible: the rule never fires.
    Infeasible,
    /// Feasible: nothing follows.
    Feasible,
    /// A finding reported for the reader: a checker or analysis warning, a
    /// coverage gap.
    Warning,
    /// Not decided: solver absent, budget spent, beyond the procedure's size.
    Undecided,
    /// Not attempted at this tier.
    Skipped,
}

impl Verdict {
    pub fn text(self) -> &'static str {
        match self {
            Verdict::Proved => "proved",
            Verdict::NotProved => "not proved",
            Verdict::Refuted => "refuted",
            Verdict::Infeasible => "infeasible",
            Verdict::Feasible => "feasible",
            Verdict::Warning => "warning",
            Verdict::Undecided => "undecided",
            Verdict::Skipped => "skipped",
        }
    }
}

/// One obligation: what was asked, of which rule, with what answer, and
/// how to reproduce it.
#[derive(Clone, Debug, serde::Serialize)]
pub struct Obligation {
    pub id: String,
    pub tier: u8,
    pub kind: String,
    pub rule: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rule_index: Option<usize>,
    pub statement: String,
    pub verdict: Verdict,
    pub evidence: String,
    /// The rewrite the verdict licenses (`R2`), when it does.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub licenses: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub logic: Option<String>,
    pub budget_ms: u64,
    pub elapsed_ms: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub replay_file: Option<String>,
}

impl Obligation {
    pub fn new(tier: u8, id: impl Into<String>, kind: &str, statement: impl Into<String>) -> Obligation {
        Obligation {
            id: id.into(),
            tier,
            kind: kind.into(),
            rule: None,
            rule_index: None,
            statement: statement.into(),
            verdict: Verdict::Skipped,
            evidence: String::new(),
            licenses: None,
            logic: None,
            budget_ms: 0,
            elapsed_ms: 0,
            replay_file: None,
        }
    }
    pub fn on_rule(mut self, prog: &Program, ri: usize) -> Obligation {
        self.rule = Some(prog.rule_label(ri));
        self.rule_index = Some(ri);
        self
    }
    pub fn with(mut self, verdict: Verdict, evidence: impl Into<String>) -> Obligation {
        self.verdict = verdict;
        self.evidence = evidence.into();
        self
    }
    pub fn licensing(mut self, rewrite: &str) -> Obligation {
        self.licenses = Some(rewrite.into());
        self
    }
}

#[derive(Clone, Debug, serde::Serialize)]
pub struct TierReport {
    pub tier: u8,
    pub name: String,
    pub label: String,
    pub obligations: Vec<Obligation>,
    pub notes: Vec<String>,
}

impl TierReport {
    /// A tier with its name and label from the table of section 6.1.
    pub fn new(tier: u8) -> TierReport {
        let (name, label) = match tier {
            0 => ("checker", "decidable, polynomial"),
            1 => ("abstract interpretation", "decidable, polynomial; sound, incomplete"),
            2 => ("linear feasibility", "decidable; exponential worst case by simplex, microseconds at this size"),
            3 => ("SMT", "QF_LRA decidable, NP-complete with Boolean structure; QF_NRA decidable, doubly exponential; budgeted"),
            4 => ("constrained Horn clauses", "semi-decidable; budgeted"),
            5 => ("translation validation", "decidable per run, exact under I2 and I3"),
            _ => ("runtime", "data-dependent"),
        };
        TierReport {
            tier,
            name: name.into(),
            label: label.into(),
            obligations: Vec::new(),
            notes: Vec::new(),
        }
    }
    /// The dead-rule verdicts this tier established (what R2 reads).
    pub fn dead_verdicts(&self) -> Vec<DeadVerdict> {
        self.obligations
            .iter()
            .filter(|o| o.verdict == Verdict::Infeasible && o.licenses.as_deref() == Some("R2"))
            .filter_map(|o| {
                o.rule_index.map(|r| DeadVerdict {
                    rule: r,
                    tier: format!("tier {} ({})", self.tier, self.name),
                    evidence: format!("{}: {}", o.statement, o.evidence),
                })
            })
            .collect()
    }
}

/// The verification report: the tiers in order with their obligations.
#[derive(Clone, Debug, Default, serde::Serialize)]
pub struct Report {
    pub strategy: String,
    pub hash: String,
    pub numerics: String,
    pub solver: Option<String>,
    pub margin: String,
    pub tiers: Vec<TierReport>,
}

impl Report {
    pub fn tier(&self, n: u8) -> Option<&TierReport> {
        self.tiers.iter().find(|t| t.tier == n)
    }
    pub fn counts(&self) -> std::collections::BTreeMap<String, usize> {
        let mut m = std::collections::BTreeMap::new();
        for t in &self.tiers {
            for o in &t.obligations {
                *m.entry(o.verdict.text().to_string()).or_default() += 1;
            }
        }
        m
    }
    pub fn obligations(&self) -> impl Iterator<Item = &Obligation> {
        self.tiers.iter().flat_map(|t| t.obligations.iter())
    }
    /// The dead-rule verdicts of every tier (what R2 reads).
    pub fn dead_verdicts(&self) -> Vec<DeadVerdict> {
        self.tiers.iter().flat_map(|t| t.dead_verdicts()).collect()
    }
    /// Whether any obligation was refuted: a translation-validation
    /// difference or a halted run, defects by section 8.
    pub fn refuted(&self) -> bool {
        self.obligations().any(|o| o.verdict == Verdict::Refuted)
    }
}

impl Stage for Report {
    fn text(&self) -> String {
        let mut out = String::new();
        let counts = self.counts();
        out.push_str(&format!(
            "verification of {} ({}): numerics {}; solver {}; margin {}\n{} tier(s); {}\n",
            self.strategy,
            &self.hash[..self.hash.len().min(16)],
            self.numerics,
            self.solver.as_deref().unwrap_or("none (tiers 3 and 4 undecided)"),
            self.margin,
            self.tiers.len(),
            if counts.is_empty() {
                "no obligation".to_string()
            } else {
                counts.iter().map(|(k, v)| format!("{} {}", v, k)).collect::<Vec<_>>().join(", ")
            }
        ));
        for t in &self.tiers {
            out.push_str(&format!("\ntier {} {} [{}]: {} obligation(s)\n", t.tier, t.name, t.label, t.obligations.len()));
            for n in &t.notes {
                out.push_str(&format!("  note: {}\n", n));
            }
            for o in &t.obligations {
                out.push_str(&format!(
                    "  {} {}{}: {} → {}{}{}{}{}{}\n",
                    o.id,
                    o.kind,
                    o.rule.as_ref().map(|r| format!(" ({})", r)).unwrap_or_default(),
                    o.statement,
                    o.verdict.text(),
                    if o.evidence.is_empty() { String::new() } else { format!("; {}", o.evidence) },
                    o.licenses.as_ref().map(|l| format!("; licenses {}", l)).unwrap_or_default(),
                    o.logic.as_ref().map(|l| format!("; {}", l)).unwrap_or_default(),
                    if o.budget_ms > 0 { format!("; {} of {} ms", o.elapsed_ms, o.budget_ms) } else { String::new() },
                    o.replay_file.as_ref().map(|f| format!("; replay {}", f)).unwrap_or_default()
                ));
            }
        }
        out
    }
    fn json(&self) -> serde_json::Value {
        let mut v = serde_json::to_value(self).unwrap_or(serde_json::Value::Null);
        if let serde_json::Value::Object(m) = &mut v {
            m.insert("stage".into(), serde_json::Value::String("verified".into()));
        }
        v
    }
}

/// What `verify` is given beyond the compiled program.
pub struct VerifyInputs<'a> {
    pub prog: &'a Program,
    /// The checker's diagnostics for the strategy (tier 0's warnings).
    pub diags: &'a [Diagnostic],
    pub opts: &'a CompileOptions,
    /// Run the tiers up to this one (0 to 6).
    pub max_tier: u8,
    /// A binding for tiers 5 and 6; without one they are skipped with a note.
    pub data: Option<(&'a Dataset, ExecConfig)>,
}

/// Run the tiers up to `max_tier` over a compiled program. Tiers 2 and 3's
/// feasibility obligations are taken from the compilation (they ran there,
/// licensing R2); tier 3's structural obligations, tier 4, 5 and 6 run
/// here. A proved CHC invariant refines the analysis in `compiled`
/// (section 6.5) and a proved guard disjointness upgrades a keyedness
/// label (section 3.3); neither changes the plan.
pub fn verify(inputs: &VerifyInputs, compiled: &mut Compiled) -> Report {
    let prog = inputs.prog;
    let opts = inputs.opts;
    let mut report = Report {
        strategy: prog.strategy.clone(),
        hash: compiled.nir.hash.clone(),
        numerics: opts.numerics.name().to_string(),
        solver: opts.solver.clone(),
        margin: format!("2^{}", opts.margin_log2),
        tiers: Vec::new(),
    };
    report.tiers.push(tier0(prog, inputs.diags));
    if inputs.max_tier < 1 {
        return report;
    }
    report.tiers.push(tier1(prog, &compiled.analysis));
    if inputs.max_tier < 2 {
        return report;
    }
    let driver = smt::Driver::from_options(opts, &prog.strategy);
    let mut t2 = compiled.tiers.iter().find(|t| t.tier == 2).cloned().unwrap_or_else(|| TierReport::new(2));
    if t2.obligations.is_empty() && t2.notes.is_empty() {
        t2.notes.push("no rule carries a linear comparison".into());
    }
    report.tiers.push(t2);
    if inputs.max_tier < 3 {
        return report;
    }
    let mut t3 = compiled.tiers.iter().find(|t| t.tier == 3).cloned().unwrap_or_else(|| TierReport::new(3));
    if opts.solver.is_none() {
        // The feasibility obligations were not attempted at compile time
        // without a solver; build them now so the report lists them and
        // the replay files can be written.
        let t2 = report.tier(2).cloned().unwrap_or_else(|| TierReport::new(2));
        t3 = obligations::tier3_feasibility(prog, &compiled.analysis, &driver, opts.margin_log2, &t2);
    }
    obligations::tier3_structural(prog, compiled, &driver, &mut t3);
    if let Some(n) = driver.absent_note() {
        t3.notes.insert(0, n);
    }
    report.tiers.push(t3);
    if inputs.max_tier < 4 {
        return report;
    }
    let chc_driver = driver.with_budget(opts.chc_budget_ms);
    report.tiers.push(chc::tier4(prog, &mut compiled.analysis, &chc_driver));
    if inputs.max_tier < 5 {
        return report;
    }
    match &inputs.data {
        Some((ds, cfg)) => {
            let (t5, run) = tv::tier5(prog, compiled, ds, cfg.clone());
            report.tiers.push(t5);
            if inputs.max_tier >= 6 {
                report.tiers.push(tv::tier6(prog, compiled, ds, cfg.clone(), run));
            }
        }
        None => {
            let mut t5 = TierReport::new(5);
            t5.notes
                .push("no binding: name --data DIR, --bundle DIR or --synthetic to run the written and the optimised plan".into());
            report.tiers.push(t5);
            if inputs.max_tier >= 6 {
                let mut t6 = TierReport::new(6);
                t6.notes.push("no binding: the runtime checks need a run".into());
                report.tiers.push(t6);
            }
        }
    }
    report
}

/// Tier 0: the checker's judgments, which held since the program checked,
/// and its warnings.
fn tier0(prog: &Program, diags: &[Diagnostic]) -> TierReport {
    let mut t = TierReport::new(0);
    let judgments = [Code::U, Code::E, Code::B, Code::M, Code::T, Code::R, Code::N, Code::F, Code::D, Code::S, Code::Z, Code::X];
    for (i, c) in judgments.iter().enumerate() {
        t.obligations
            .push(Obligation::new(0, format!("t0-j{}", i + 1), "judgment", c.judgment()).with(Verdict::Proved, format!("[{}] holds over {} rule(s)", c, prog.rules.len())));
    }
    let mut k = 0;
    for d in diags.iter().filter(|d| d.severity == Severity::Warning) {
        k += 1;
        let mut o = Obligation::new(0, format!("t0-w{}", k), "warning", d.code.judgment()).with(Verdict::Warning, format!("[{}] {} at {}", d.code, d.message, d.span));
        o.rule = d.rule.clone();
        t.obligations.push(o);
    }
    if k == 0 {
        t.notes.push("no checker warning".into());
    }
    t
}

/// Tier 1: the abstract interpretation's dead literals (with the margin of
/// section 6.8 deciding the licence) and its warnings.
fn tier1(prog: &Program, analysis: &crate::analysis::Analysis) -> TierReport {
    let mut t = TierReport::new(1);
    for (i, n) in analysis.never_true.iter().enumerate() {
        let lit = prog.rules[n.rule].body.get(n.literal).map(|l| l.describe()).unwrap_or_else(|| "?".into());
        let mut o = Obligation::new(1, format!("t1-d{}", i + 1), "dead literal", format!("`{}` never holds", lit)).on_rule(prog, n.rule);
        if n.within_margin {
            o = o.with(
                Verdict::Infeasible,
                format!("{}; within the rounding margin: kept{}", n.evidence, if n.bundle_relative { "; bundle-relative" } else { "" }),
            );
        } else {
            o = o
                .with(
                    Verdict::Infeasible,
                    format!("{}; gap {} beyond the margin{}", n.evidence, n.gap, if n.bundle_relative { "; bundle-relative" } else { "" }),
                )
                .licensing("R2");
        }
        t.obligations.push(o);
    }
    let mut k = 0;
    for w in analysis.warnings.iter().filter(|w| w.code != "A004") {
        k += 1;
        let verdict = match w.code.as_str() {
            "A005" => Verdict::Proved,
            "A006" | "A007" | "A009" => Verdict::Infeasible,
            _ => Verdict::Warning,
        };
        let mut o = Obligation::new(1, format!("t1-w{}", k), if w.note { "note" } else { "warning" }, format!("[{}] {}", w.code, w.message))
            .with(verdict, format!("at {}{}", w.span, if w.bundle_relative { "; bundle-relative" } else { "" }));
        o.rule = w.rule.clone();
        t.obligations.push(o);
    }
    if t.obligations.is_empty() {
        t.notes.push("no dead literal and no warning".into());
    }
    t
}
