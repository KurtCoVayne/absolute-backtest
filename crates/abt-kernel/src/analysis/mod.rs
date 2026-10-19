//! The Analyse stage (compiler design, section 3): keyedness, the open-loop
//! partition, demand, window lengths, abstract values and warnings. The
//! first four are syntactic (`structure`); the abstract values are the
//! abstract interpretation of `ai` over the domains of `domains`. The
//! analysis walks the checked program's rules (the typed IR) by their
//! written index, so every fact and warning carries the author's names and
//! spans; the normal form maps each rule to its canonical id.

pub mod ai;
pub mod domains;
pub mod structure;

use std::collections::{BTreeMap, BTreeSet};

pub use domains::{AbsVal, Interval, Sign};

use crate::check::Program;
use crate::compiler::normalise::Nir;
use crate::compiler::Stage;
use crate::plan::cost::Statistics;

/// How a derived relation in a temporally recursive component is known to
/// be keyed (section 3.3).
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
pub enum Keyedness {
    /// Not in a temporally recursive component: keyedness is not required.
    NotRequired,
    ByConstruction,
    ByGuard,
    AtRuntime,
}

impl Keyedness {
    pub fn text(self) -> &'static str {
        match self {
            Keyedness::NotRequired => "not required",
            Keyedness::ByConstruction => "keyed by construction",
            Keyedness::ByGuard => "keyed by guard",
            Keyedness::AtRuntime => "keyed at runtime",
        }
    }
}

/// The demand on a parametric relation (section 3.4).
#[derive(Clone, Debug, PartialEq, serde::Serialize)]
pub enum DemandSet {
    /// No parametric argument: demand on identity arguments is total.
    Total,
    /// The finite set of parameter tuples the program names.
    Static(Vec<Vec<String>>),
    /// A call binds a parametric argument to a value from the data.
    Dynamic(String),
}

#[derive(Clone, Debug, serde::Serialize)]
pub struct ColumnFacts {
    pub name: String,
    pub ty: String,
    pub value: AbsVal,
}

#[derive(Clone, Debug, serde::Serialize)]
pub struct RelationFacts {
    pub name: String,
    pub kind: String,
    pub calendar: String,
    pub stratum: Option<usize>,
    pub component: usize,
    pub recursive: bool,
    pub keyedness: Keyedness,
    pub keyedness_note: String,
    pub closed_loop: bool,
    pub demanded: bool,
    pub demand: DemandSet,
    pub columns: Vec<ColumnFacts>,
    /// Columns whose bound widening took to ±∞ (note A010).
    pub widened: Vec<String>,
}

#[derive(Clone, Debug, serde::Serialize)]
pub struct WindowFacts {
    pub rule: usize,
    pub literal: usize,
    pub kind: String,
    pub w_max: Option<f64>,
    pub k: Option<f64>,
    pub warmup: Option<f64>,
}

#[derive(Clone, Debug, serde::Serialize)]
pub struct LiteralFacts {
    pub index: usize,
    pub text: String,
    /// The abstract state after the literal: every bound variable.
    pub state: Vec<(String, AbsVal)>,
    /// `never true` or `always true`, when decided.
    pub verdict: Option<String>,
    pub selectivity: Option<f64>,
}

#[derive(Clone, Debug, serde::Serialize)]
pub struct RuleFacts {
    pub rule: usize,
    pub label: String,
    pub literals: Vec<LiteralFacts>,
    pub may_undef: bool,
}

#[derive(Clone, Debug, serde::Serialize)]
pub struct Warning {
    pub code: String,
    pub rule: Option<String>,
    pub span: String,
    pub message: String,
    /// Sound only for the bound data (section 3.8, item 4).
    pub bundle_relative: bool,
    /// A note rather than a warning (A005, A008, A010).
    pub note: bool,
}

impl std::fmt::Display for Warning {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} [{}]", if self.note { "note" } else { "warning" }, self.code)?;
        if let Some(r) = &self.rule {
            write!(f, " in rule {}", r)?;
        }
        if self.span != "0:0" {
            write!(f, " at {}", self.span)?;
        }
        write!(f, ": {}", self.message)?;
        if self.bundle_relative {
            write!(f, " (bundle-relative)")?;
        }
        Ok(())
    }
}

/// A comparison decided never true, with its evidence and the relative gap
/// between the sides (section 6.8: below the margin it stays, as a note).
#[derive(Clone, Debug, serde::Serialize)]
pub struct NeverTrue {
    pub rule: usize,
    pub literal: usize,
    pub evidence: String,
    pub gap: f64,
    pub within_margin: bool,
    pub bundle_relative: bool,
}

#[derive(Clone, Debug, serde::Serialize)]
pub struct Analysis {
    pub relations: Vec<RelationFacts>,
    pub rules: Vec<RuleFacts>,
    pub windows: Vec<WindowFacts>,
    pub demanded: BTreeSet<String>,
    pub never_true: Vec<NeverTrue>,
    pub warnings: Vec<Warning>,
    pub thresholds: Vec<f64>,
    pub statistics: String,
    pub bundle_relative: bool,
}

/// Analyse a checked program (section 3), with its normal form for the
/// keyedness comparisons and, optionally, the bound data's statistics.
pub fn analyse(prog: &Program, nir: &Nir, stats: Option<&Statistics>, margin_log2: i32) -> Analysis {
    let st = structure::structure(prog, nir, stats);
    ai::interpret(prog, st, stats, margin_log2)
}

impl Analysis {
    pub fn relation(&self, name: &str) -> Option<&RelationFacts> {
        self.relations.iter().find(|r| r.name == name)
    }

    /// What the planner reads from the analysis.
    pub fn plan_inputs(&self) -> crate::plan::PlanInputs {
        let mut inputs = crate::plan::PlanInputs {
            demanded: Some(self.demanded.clone()),
            ..Default::default()
        };
        for r in &self.rules {
            for l in &r.literals {
                if let Some(s) = l.selectivity {
                    inputs.cmp_selectivity.insert((r.rule, l.index), s);
                }
            }
        }
        for w in &self.windows {
            if let Some(m) = w.w_max {
                inputs.w_max.insert((w.rule, w.literal), m);
            }
        }
        inputs
    }

    pub fn window(&self, rule: usize, literal: usize) -> Option<&WindowFacts> {
        self.windows.iter().find(|w| w.rule == rule && w.literal == literal)
    }
}

impl Stage for Analysis {
    fn text(&self) -> String {
        let mut out = String::new();
        out.push_str(&format!(
            "analysis (statistics: {}{}): {} relations, {} rules, {} windows, {} warnings\n",
            self.statistics,
            if self.bundle_relative { "; interval verdicts are bundle-relative" } else { "" },
            self.relations.len(),
            self.rules.len(),
            self.windows.len(),
            self.warnings.iter().filter(|w| !w.note).count()
        ));
        out.push_str("\nrelations:\n");
        for r in &self.relations {
            let mut tags = vec![r.kind.clone(), r.calendar.clone()];
            if let Some(s) = r.stratum {
                tags.push(format!("stratum {}", s));
            }
            tags.push(format!("component {}", r.component));
            if r.recursive {
                tags.push("temporally recursive".into());
                tags.push(r.keyedness.text().into());
            }
            tags.push(if r.closed_loop { "closed loop".into() } else { "open loop".into() });
            tags.push(if r.demanded { "demanded".into() } else { "elidable".into() });
            match &r.demand {
                DemandSet::Total => {}
                DemandSet::Static(ts) => tags.push(format!("demand {{{}}}", ts.iter().map(|t| format!("({})", t.join(", "))).collect::<Vec<_>>().join(", "))),
                DemandSet::Dynamic(why) => tags.push(format!("demand dynamic: {}", why)),
            }
            out.push_str(&format!("  {}  [{}]\n", r.name, tags.join(", ")));
            if !r.keyedness_note.is_empty() {
                out.push_str(&format!("    keyedness: {}\n", r.keyedness_note));
            }
            for c in &r.columns {
                if c.value.numeric {
                    out.push_str(&format!(
                        "    {}: {}  {}{}\n",
                        c.name,
                        c.ty,
                        c.value.text(),
                        if r.widened.contains(&c.name) { "  (widened)" } else { "" }
                    ));
                }
            }
        }
        out.push_str("\nwindows:\n");
        for w in &self.windows {
            out.push_str(&format!(
                "  rule {} literal {}: {}; at most {} bars, min {}{}\n",
                w.rule,
                w.literal + 1,
                w.kind,
                w.w_max.map(|x| format!("{}", x)).unwrap_or_else(|| "?".into()),
                w.k.map(|x| format!("{}", x)).unwrap_or_else(|| "?".into()),
                w.warmup.map(|x| format!(", {} warm-up bars", x)).unwrap_or_default()
            ));
        }
        out.push_str("\nrules (the abstract state after each literal):\n");
        for r in &self.rules {
            out.push_str(&format!("  {}{}\n", r.label, if r.may_undef { "  [may meet a partial operation]" } else { "" }));
            for l in &r.literals {
                let state: Vec<String> = l.state.iter().filter(|(_, v)| v.numeric).map(|(n, v)| format!("{} ∈ {}", n, v.text())).collect();
                out.push_str(&format!(
                    "    {}. {}{}{}\n      {}\n",
                    l.index + 1,
                    l.text,
                    l.verdict.as_ref().map(|v| format!("  [{}]", v)).unwrap_or_default(),
                    l.selectivity.map(|s| format!("  sel {:.2}", s)).unwrap_or_default(),
                    if state.is_empty() { "(no numeric variable bound)".to_string() } else { state.join(", ") }
                ));
            }
        }
        out.push_str("\nwarnings:\n");
        for w in &self.warnings {
            out.push_str(&format!("  {}\n", w));
        }
        if self.warnings.is_empty() {
            out.push_str("  none\n");
        }
        out
    }

    fn json(&self) -> serde_json::Value {
        let mut v = serde_json::to_value(self).unwrap_or(serde_json::Value::Null);
        if let serde_json::Value::Object(m) = &mut v {
            m.insert("stage".into(), serde_json::Value::String("analysed".into()));
        }
        v
    }
}

/// A structural summary (what `structure` computes before the interpreter).
pub struct Structure {
    pub components: BTreeMap<String, usize>,
    pub recursive: BTreeSet<String>,
    pub closed_loop: BTreeSet<String>,
    pub keyedness: BTreeMap<String, (Keyedness, String)>,
    pub demand: BTreeMap<String, DemandSet>,
    pub demanded: BTreeSet<String>,
    pub windows: Vec<WindowFacts>,
}
