//! R4, window and resample fusion at the summary level (compiler design,
//! section 4.4): an aggregation over a relation that is itself a resample
//! or window aggregation of the same class, demanded only as a summary,
//! could be one aggregation over the inner input with the merged summary.
//! The fused summary must perform the same floating-point operations in the
//! same order as the two-level computation (I3), which holds only under a
//! numerics setting whose summary merge is the canonical operation. This
//! kernel's `running` setting is a rebuilt compensated sum and its
//! `two_pass` setting is not a summary at all, so R4 is detected and
//! disabled with a log entry stating why.

use super::RewriteLog;
use crate::check::Program;
use crate::compiler::normalise::{Nir, NirLit};
use crate::ir::Literal;
use crate::lower::Numerics;

/// Whether a relation is defined by one rule whose body holds a resample
/// or a windowed aggregation.
fn inner_class(prog: &Program, name: &str) -> Option<String> {
    let rules = prog.rules_for(name);
    if rules.len() != 1 {
        return None;
    }
    for l in &prog.rules[rules[0]].body {
        match l {
            Literal::Agg { agg, conj, .. } if conj.iter().any(|c| matches!(c, Literal::Window { .. })) => return Some(agg.clone()),
            _ => {}
        }
    }
    None
}

pub fn fuse(prog: &Program, nir: &mut Nir, numerics: Numerics, disabled: &mut Vec<String>, log: &mut RewriteLog) -> bool {
    let algebraic = |a: &str| matches!(a, "sum" | "count" | "mean" | "std" | "cov" | "corr" | "ols_beta" | "max" | "min");
    for r in nir.rules.iter() {
        for b in &r.body {
            let NirLit::Agg { agg, group, .. } = &b.lit else { continue };
            if !algebraic(agg) {
                continue;
            }
            let atoms: Vec<&str> = group.iter().filter_map(|g| if let NirLit::Atom(a) = g { Some(a.name.as_str()) } else { None }).collect();
            if atoms.len() != 1 {
                continue;
            }
            let Some(inner) = inner_class(prog, atoms[0]) else { continue };
            if inner != *agg {
                continue;
            }
            let why = format!(
                "{} over {} in {}: fusable in principle ({} over a {} of the same class), disabled: numerics `{}` is {}, not a monoid merge of summaries",
                agg,
                atoms[0],
                r.id,
                agg,
                inner,
                numerics.name(),
                match numerics {
                    Numerics::Running => "a sliding compensated sum rebuilt every N rows",
                    Numerics::TwoPass => "the two-pass formula over the rows, which is not a summary",
                }
            );
            if !disabled.contains(&why) {
                log.push("R4", &r.id, &r.span, format!("{} over {}", agg, atoms[0]), "not fused".into(), &why, [1, 1]);
                disabled.push(why);
            }
        }
    }
    false
}
