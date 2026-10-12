//! R2, dead rule removal (compiler design, section 4.2): a rule with a
//! literal that is never true, by Analyse (A004 beyond the margin of 6.8)
//! or by a verification tier, is flagged infeasible and contributes no work.

use std::collections::BTreeMap;

use super::{DeadVerdict, RewriteLog};
use crate::analysis::Analysis;
use crate::check::Program;
use crate::compiler::normalise::Nir;

pub fn dead(prog: &Program, nir: &mut Nir, analysis: &Analysis, verdicts: &[DeadVerdict], _margin_log2: i32, infeasible: &mut BTreeMap<usize, String>, log: &mut RewriteLog) -> bool {
    let mut any = false;
    let unflagged_before = nir.rules.iter().filter(|r| r.infeasible.is_none()).count();
    let mut found: Vec<(usize, String)> = Vec::new();
    for n in &analysis.never_true {
        if n.within_margin {
            continue;
        }
        let lit = prog.rules[n.rule].body.get(n.literal).map(|l| l.describe()).unwrap_or_default();
        found.push((
            n.rule,
            format!(
                "tier 1 (abstract interpretation): `{}` never holds: {}{}",
                lit,
                n.evidence,
                if n.bundle_relative { "; bundle-relative" } else { "" }
            ),
        ));
    }
    for v in verdicts {
        found.push((v.rule, format!("{}: {}", v.tier, v.evidence)));
    }
    for (origin, evidence) in found {
        let Some(r) = nir.rules.iter_mut().find(|r| r.origin == origin && r.specialised.is_none()) else {
            continue;
        };
        if r.infeasible.is_some() {
            continue;
        }
        r.infeasible = Some(evidence.clone());
        infeasible.insert(origin, evidence.clone());
        let after = unflagged_before - infeasible.len().min(unflagged_before);
        log.push(
            "R2",
            &r.id,
            &r.span,
            "feasible".into(),
            format!("infeasible: {}", evidence),
            "Proposition 4.1: a false literal contributes no tuple",
            [unflagged_before, after],
        );
        any = true;
    }
    any
}
