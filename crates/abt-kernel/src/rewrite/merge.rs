//! R3, aggregation merging (compiler design, section 4.3): two aggregations
//! in one rule with identical group, window, minimum and arguments, whose
//! aggregates factor through one summary (Gray et al. 1997), are replaced
//! by one summary computation and two finalisations. The kernel lowers it
//! as one window state shared by the two literals; the finalisation of each
//! aggregate is the canonical formula of the numerics setting whether or
//! not it shares a summary, so no operation sequence changes (I3).

use super::RewriteLog;
use crate::check::Program;
use crate::compiler::normalise::{key_of, Nir, NirBodyLit, NirExpr, NirLit};

/// The summary class two aggregates share, if any.
fn summary_of(a: &str, b: &str) -> Option<&'static str> {
    let moment = |x: &str| matches!(x, "sum" | "mean" | "std" | "cov" | "corr" | "ols_beta");
    let order = |x: &str| matches!(x, "max" | "min" | "first" | "last" | "count");
    if moment(a) && moment(b) {
        Some("moments (n, Σx, Σy, Σx², Σy², Σxy)")
    } else if order(a) && order(b) {
        Some("the window's rows")
    } else {
        None
    }
}

/// The group identity of an aggregation: its group, window, minimum and
/// arguments, rendered name-free.
fn group_key(l: &NirLit) -> Option<String> {
    let NirLit::Agg { args, group, min, by, .. } = l else { return None };
    let probe = NirLit::Agg {
        var: "_".into(),
        agg: "probe".into(),
        args: args.clone(),
        group: group.clone(),
        by: by.clone(),
        min: min.clone(),
        anchor: None,
    };
    Some(key_of(&probe).2)
}

pub fn merge(prog: &Program, nir: &mut Nir, shared: &mut Vec<(usize, usize, usize)>, log: &mut RewriteLog) -> bool {
    let mut any = false;
    for r in nir.rules.iter_mut() {
        if r.elided {
            continue;
        }
        // Aggregation literals by body position, with their group keys.
        let aggs: Vec<(usize, String, String)> = r
            .body
            .iter()
            .enumerate()
            .filter_map(|(i, b)| match &b.lit {
                NirLit::Agg { agg, .. } if !agg.starts_with("summary") => group_key(&b.lit).map(|k| (i, agg.clone(), k)),
                _ => None,
            })
            .collect();
        let literals_before = aggs.len();
        let mut merged: Option<(usize, usize, &'static str)> = None;
        'search: for i in 0..aggs.len() {
            for j in (i + 1)..aggs.len() {
                if aggs[i].2 != aggs[j].2 {
                    continue;
                }
                // Two aggregates of one resample share one pass already.
                if r.body[aggs[i].0].unit == r.body[aggs[j].0].unit {
                    continue;
                }
                if let Some(class) = summary_of(&aggs[i].1, &aggs[j].1) {
                    merged = Some((aggs[i].0, aggs[j].0, class));
                    break 'search;
                }
            }
        }
        let Some((pi, pj, class)) = merged else { continue };
        let (
            NirLit::Agg {
                var: va,
                agg: aa,
                args,
                group,
                by,
                min,
                anchor,
            },
            NirLit::Agg { var: vb, agg: ab, .. },
        ) = (r.body[pi].lit.clone(), r.body[pj].lit.clone())
        else {
            continue;
        };
        let summary_var = format!("S{}", pi);
        let summary = NirLit::Agg {
            var: summary_var.clone(),
            agg: format!("summary[{}, {}]", aa, ab),
            args,
            group,
            by,
            min,
            anchor,
        };
        let fin_a = NirLit::Assign {
            var: Some(va.clone()),
            expr: NirExpr::Call(format!("finalise_{}", aa), vec![NirExpr::Var(summary_var.clone())]),
        };
        let fin_b = NirLit::Assign {
            var: Some(vb.clone()),
            expr: NirExpr::Call(format!("finalise_{}", ab), vec![NirExpr::Var(summary_var.clone())]),
        };
        let unit_a = r.body[pi].unit;
        let unit_b = r.body[pj].unit;
        r.body[pi] = NirBodyLit { lit: summary, unit: unit_a };
        r.body.insert(pi + 1, NirBodyLit { lit: fin_a, unit: unit_a });
        let pj = pj + 1;
        r.body[pj] = NirBodyLit { lit: fin_b, unit: unit_b };
        // The written literals that share the state.
        let primary = r.surface_origin[unit_a];
        let other = r.surface_origin[unit_b];
        if !shared.contains(&(r.origin, other, primary)) {
            shared.push((r.origin, other, primary));
        }
        let _ = prog;
        log.push(
            "R3",
            &r.id,
            &r.span,
            format!("{} = {}(...), {} = {}(...)", va, aa, vb, ab),
            format!(
                "{} = summary over the group; {} = finalise_{}({}), {} = finalise_{}({})",
                summary_var, va, aa, summary_var, vb, ab, summary_var
            ),
            &format!("algebraic aggregates factor through a bounded summary: {} (Gray et al. 1997); one window state in the kernel", class),
            [literals_before, literals_before - 1],
        );
        any = true;
    }
    any
}
