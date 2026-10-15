//! Tiers 5 and 6 (compiler design, sections 6.6 and 6.7). Translation
//! validation runs the plan with every rewrite disabled and the written
//! literal order, and the plan the full pipeline produced, on one binding,
//! and compares `decide` at every bar, the ledgers of the derived
//! relations, and the book, to the bit (Proposition 6.1). The runtime
//! tier reads what the optimised run recorded: coverage, instances that
//! met a partial operation, the halt if any.

use std::collections::BTreeMap;

use super::{Obligation, TierReport, Verdict};
use crate::check::Program;
use crate::compiler::Compiled;
use crate::ir::Kind;
use crate::kernel::{Dataset, ExecConfig, Kernel, RunError, RunResult, Value};
use crate::lower::Lowered;

/// What a run produced, as comparable text with bits: `decide` at every
/// bar, the fills, the book at every bar, the final state.
pub fn digest(r: &RunResult) -> Vec<String> {
    let mut out = Vec::new();
    out.push(format!("bars {:?}", r.bars));
    for d in &r.decisions {
        out.push(format!("decision {} {} {}", d.t, r.describe_decision(&d.decision), d.rule));
    }
    for f in &r.fills {
        out.push(format!(
            "fill {} {} {:x} {:x} {} {:x} {:x} {:x} {:x} {:x} {} {}",
            f.t,
            r.symbols[f.equity as usize],
            f.quantity.to_bits(),
            f.price.to_bits(),
            f.at_last_price,
            f.commission.to_bits(),
            f.fee.to_bits(),
            f.slippage.to_bits(),
            f.impact.to_bits(),
            f.participation.to_bits(),
            f.partial,
            f.forced
        ));
    }
    for (t, d, why) in &r.dropped {
        out.push(format!("dropped {} {} {}", t, r.describe_decision(d), why));
    }
    for e in &r.exposure {
        out.push(format!(
            "exposure {} {:x} {:x} {:x} {:x} {:x}",
            e.t,
            e.cash.to_bits(),
            e.gross.to_bits(),
            e.net.to_bits(),
            e.equity.to_bits(),
            e.leverage.to_bits()
        ));
    }
    for a in &r.actions {
        out.push(format!("action {} {} {:?} {:x}", a.t, r.symbols[a.equity as usize], a.action, a.cash.to_bits()));
    }
    out.push(format!("costs {:?}", r.costs));
    out.push(format!("funding {:?}", r.funding));
    out.push(format!("final_cash {:x}", r.final_cash.to_bits()));
    out.push(format!("final_positions {:?}", r.final_positions));
    out
}

pub type Ledgers = BTreeMap<String, Vec<String>>;

/// The ledgers of every derived relation whose inputs are at most its
/// entity: every tuple at every bar, as text with bits.
pub fn ledgers(k: &mut Kernel, prog: &Program, bars: &[i64]) -> Ledgers {
    let mut out = BTreeMap::new();
    let (Some(&first), Some(&last)) = (bars.first(), bars.last()) else { return out };
    let names: Vec<String> = prog
        .relations
        .iter()
        .filter(|(_, s)| matches!(s.kind, Kind::Derived { .. } | Kind::Output))
        .map(|(n, _)| n.clone())
        .collect();
    for rel in names {
        let sig = &prog.relations[&rel];
        let per_symbol = false;
        let own: Vec<i64> = k.bars_at(&sig.cal).into_iter().filter(|&b| b >= first && b <= last).collect();
        let symbols: Vec<Value> = if per_symbol {
            (0..k.symbol_names().len()).map(|i| Value::Equity(i as u32)).collect()
        } else {
            vec![]
        };
        let mut rows = Vec::new();
        for t in own {
            let found: Vec<Vec<Value>> = if per_symbol {
                let mut all = Vec::new();
                for s in &symbols {
                    match k.query(&rel, t, std::slice::from_ref(s)) {
                        Ok(tus) => all.extend(tus),
                        Err(RunError::Arithmetic { .. }) => all.push(vec![Value::Count(-1)]),
                        Err(e) => all.push(vec![Value::Count(-2), Value::Count(e.to_string().len() as i64)]),
                    }
                }
                all
            } else {
                match k.query(&rel, t, &[]) {
                    Ok(tus) => tus,
                    Err(RunError::Arithmetic { .. }) => vec![vec![Value::Count(-1)]],
                    Err(e) => vec![vec![Value::Count(-2), Value::Count(e.to_string().len() as i64)]],
                }
            };
            for tu in found {
                let cells: Vec<String> = tu
                    .iter()
                    .map(|v| match v {
                        Value::Num(x) => format!("{:x}", x.to_bits()),
                        v => k.show(v),
                    })
                    .collect();
                rows.push(format!("{} {}", t, cells.join(" ")));
            }
        }
        out.insert(rel, rows);
    }
    out
}

/// A run with what the comparison reads.
pub struct Run {
    pub digest: Vec<String>,
    pub ledgers: Ledgers,
    pub result: RunResult,
}

/// A run under a plan: the digest and the ledgers (through the kernel kept
/// after the run), or the halt.
pub fn run_with_ledgers(p: &Program, lowered: &Lowered, ds: &Dataset, cfg: ExecConfig) -> Result<Run, String> {
    let client = crate::kernel::connect_dataset(ds, p).map_err(|e| e.to_string())?;
    let (r, l) = crate::kernel::run_keeping(p, Some(lowered), &client, cfg, &crate::kernel::Replay::default(), |r, k| Ok(ledgers(k, p, &r.bars))).map_err(|e| e.to_string())?;
    Ok(Run {
        digest: digest(&r),
        ledgers: l,
        result: r,
    })
}

/// The first line at which two texts differ.
pub fn first_difference(a: &[String], b: &[String]) -> Option<(usize, String, String)> {
    let n = a.len().max(b.len());
    (0..n)
        .find(|&i| a.get(i) != b.get(i))
        .map(|i| (i, a.get(i).cloned().unwrap_or_default(), b.get(i).cloned().unwrap_or_default()))
}

fn clip(s: &str) -> String {
    if s.chars().count() > 100 {
        s.chars().take(97).collect::<String>() + "…"
    } else {
        s.to_string()
    }
}

/// Tier 5 on one binding: the written plan against the compiled one. The
/// optimised run is returned for tier 6.
pub fn tier5(prog: &Program, compiled: &Compiled, ds: &Dataset, cfg: ExecConfig) -> (TierReport, Option<Run>) {
    let mut t = TierReport::new(5);
    let written = Lowered::written(prog, compiled.plan.numerics);
    let reordered = compiled
        .lowered
        .order
        .iter()
        .enumerate()
        .filter(|(i, o)| o.iter().enumerate().any(|(k, &l)| k != l) && !compiled.lowered.skip.get(*i).copied().unwrap_or(false))
        .count();
    t.notes.push(format!(
        "written plan against `{}`: {} rule(s) reordered, {} skipped (R2), {} shared window state(s) (R3), constants folded: {}",
        compiled.lowered.label,
        reordered,
        compiled.lowered.skip.iter().filter(|s| **s).count(),
        compiled.lowered.shared_windows.len(),
        if compiled.lowered.program.is_some() { "yes" } else { "no" }
    ));
    let a = run_with_ledgers(prog, &written, ds, cfg.clone());
    let b = run_with_ledgers(prog, &compiled.lowered, ds, cfg);
    let (a, b) = match (a, b) {
        (Ok(a), Ok(b)) => (a, b),
        (Err(ea), Err(eb)) => {
            let same = ea == eb;
            t.obligations
                .push(Obligation::new(5, "t5-run", "both plans halt alike", "the written and the optimised plan halt with one message").with(
                    if same { Verdict::Proved } else { Verdict::Refuted },
                    if same {
                        format!("both halted: {}", clip(&ea))
                    } else {
                        format!("written: {}; optimised: {}", clip(&ea), clip(&eb))
                    },
                ));
            return (t, None);
        }
        (Ok(_), Err(eb)) => {
            t.obligations
                .push(Obligation::new(5, "t5-run", "both plans run", "the written and the optimised plan both complete").with(Verdict::Refuted, format!("the optimised plan halted: {}", clip(&eb))));
            return (t, None);
        }
        (Err(ea), Ok(b)) => {
            t.obligations
                .push(Obligation::new(5, "t5-run", "both plans run", "the written and the optimised plan both complete").with(Verdict::Refuted, format!("the written plan halted: {}", clip(&ea))));
            return (t, Some(b));
        }
    };
    t.obligations
        .push(Obligation::new(5, "t5-run", "both plans run", "the written and the optimised plan both complete").with(
            Verdict::Proved,
            format!("{} bar(s), {} decision(s), {} fill(s)", b.result.bars.len(), b.result.decisions.len(), b.result.fills.len()),
        ));
    let o = Obligation::new(
        5,
        "t5-decide-book",
        "decide and the book",
        "`decide` at every bar, the fills, the book at every bar and the final state are bit-identical",
    );
    t.obligations.push(match first_difference(&a.digest, &b.digest) {
        None => o.with(Verdict::Proved, format!("{} line(s) equal", a.digest.len())),
        Some((i, x, y)) => o.with(Verdict::Refuted, format!("line {} differs: written `{}`, optimised `{}`", i, clip(&x), clip(&y))),
    });
    let names: Vec<&String> = a.ledgers.keys().chain(b.ledgers.keys()).collect::<std::collections::BTreeSet<_>>().into_iter().collect();
    for rel in names {
        let o = Obligation::new(5, format!("t5-ledger-{}", rel), "ledger", format!("the ledger of `{}` at every bar is bit-identical", rel));
        let (x, y) = (a.ledgers.get(rel), b.ledgers.get(rel));
        t.obligations.push(match (x, y) {
            (Some(x), Some(y)) => match first_difference(x, y) {
                None => o.with(Verdict::Proved, format!("{} tuple(s) equal", x.len())),
                Some((i, p, q)) => o.with(Verdict::Refuted, format!("tuple {} differs: written `{}`, optimised `{}`", i, clip(&p), clip(&q))),
            },
            _ => o.with(Verdict::Refuted, "the relation is in one run's ledgers only"),
        });
    }
    (t, Some(b))
}

/// Tier 6: what the optimised run recorded.
pub fn tier6(prog: &Program, compiled: &Compiled, ds: &Dataset, cfg: ExecConfig, run: Option<Run>) -> TierReport {
    let mut t = TierReport::new(6);
    let run = match run {
        Some(r) => r,
        None => match run_with_ledgers(prog, &compiled.lowered, ds, cfg) {
            Ok(r) => r,
            Err(e) => {
                t.obligations
                    .push(Obligation::new(6, "t6-run", "run", "the optimised plan completes on the binding").with(Verdict::Refuted, format!("halted: {}", clip(&e))));
                return t;
            }
        },
    };
    let r = &run.result;
    t.obligations.push(Obligation::new(6, "t6-run", "run", "the optimised plan completes on the binding").with(
        Verdict::Proved,
        format!("{} bar(s), {} decision(s), {} fill(s), {} dropped", r.bars.len(), r.decisions.len(), r.fills.len(), r.dropped.len()),
    ));
    let never: Vec<&crate::kernel::RelationCoverage> = r.coverage.iter().filter(|c| c.derived && c.calls > 0 && c.nonempty == 0).collect();
    if never.is_empty() {
        t.obligations
            .push(Obligation::new(6, "t6-coverage", "coverage", "every demanded derived relation held at least once").with(
                Verdict::Proved,
                format!("{} derived relation(s) called", r.coverage.iter().filter(|c| c.derived && c.calls > 0).count()),
            ));
    } else {
        for (i, c) in never.iter().enumerate() {
            t.obligations
                .push(Obligation::new(6, format!("t6-coverage-{}", i + 1), "coverage", format!("`{}` holds at some bar", c.name)).with(
                    Verdict::Warning,
                    format!(
                        "demanded {} time(s) and never held (the first empty link of a path to decide, or a relation written for another binding)",
                        c.calls
                    ),
                ));
        }
    }
    let stuck = r.stats.excluded_after_undefined;
    t.obligations.push(Obligation::new(6, "t6-stuck", "partial operations", "no instance met a partial operation").with(
        if stuck == 0 { Verdict::Proved } else { Verdict::Warning },
        if stuck == 0 {
            "no instance was excluded after an undefined value".to_string()
        } else {
            format!(
                "{} instance(s) met a partial operation and were excluded by a later literal (Kleene conjunction; a halt only when no literal excludes)",
                stuck
            )
        },
    ));
    t.notes.push(format!(
        "kernel: {} window call(s), {} memo entries, {} late tuple(s)",
        r.stats.window_calls, r.stats.memo_entries, r.stats.late_tuples
    ));
    t
}
