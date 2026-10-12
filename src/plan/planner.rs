//! The planner's driver (compiler design, section 5): from a checked
//! program, the inputs the earlier stages supply (infeasible rules, elided
//! and demanded relations, selectivities, window lengths, shared window
//! states) and the statistics of the bound data, a `Plan`.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use super::cost::{aggregate_class, implementation, literal_costs, Statistics};
use super::search::{literal_deps, order_cost, search};
use super::{AggPlan, LitPlan, Plan, PlanTotals, RelPlan, RulePlan, SearchKind, StratumPlan};
use crate::check::Program;
use crate::ir::*;
use crate::lower::Numerics;

/// What the analysis and rewrite stages tell the planner.
#[derive(Clone, Debug, Default)]
pub struct PlanInputs {
    /// Rules proved infeasible (R2), with the evidence.
    pub infeasible: BTreeMap<usize, String>,
    /// Relations nothing demanded, observed or queried reaches (R5).
    pub elided: BTreeSet<String>,
    /// The demanded relation set, when known.
    pub demanded: Option<BTreeSet<String>>,
    /// Selectivity of a comparison, by (rule, literal), from the abstract
    /// intervals.
    pub cmp_selectivity: BTreeMap<(usize, usize), f64>,
    /// The most bars a window can hold, by (rule, aggregation literal).
    pub w_max: BTreeMap<(usize, usize), f64>,
    /// (rule, literal, primary literal) of aggregations merged by R3.
    pub shared: Vec<(usize, usize, usize)>,
}

/// The relations that read, directly or transitively, what the executor
/// produces (section 3.2): evaluated per bar, after the previous bar's fills.
pub fn closed_loop(prog: &Program) -> BTreeSet<String> {
    let mut closed: BTreeSet<String> = prog
        .relations
        .iter()
        .filter(|(_, s)| matches!(s.kind, Kind::Executor | Kind::KernelState))
        .map(|(n, _)| n.clone())
        .collect();
    loop {
        let mut changed = false;
        for (i, r) in prog.rules.iter().enumerate() {
            if closed.contains(&r.head.name) {
                continue;
            }
            if prog.infos[i].refs.iter().any(|rf| closed.contains(&rf.name)) {
                closed.insert(r.head.name.clone());
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }
    closed
}

/// Whether a rule holds a cross-sectional literal: a free entity argument
/// inside a reduction, a resample or an aggregation's group (a barrier
/// between per-entity partitions, section 5.5).
fn cross_sectional(prog: &Program, rule: usize) -> bool {
    let deps = literal_deps(prog, rule);
    let r = &prog.rules[rule];
    r.body.iter().enumerate().any(|(i, lit)| match lit {
        Literal::Top { atom, .. } | Literal::Resample { inner: atom, .. } => {
            let sig = prog.relations.get(&atom.name);
            atom.terms.iter().enumerate().any(|(k, t)| {
                let entity = sig.and_then(|s| s.args.get(k)).map(|a| a.ty.is_entity()).unwrap_or(false);
                entity && matches!(t, Term::Var(v, _) if deps.literals[i].binds.contains(v))
            })
        }
        Literal::Agg { conj, .. } => conj.iter().any(|c| match c {
            Literal::Atom(a) => {
                let sig = prog.relations.get(&a.name);
                a.terms.iter().enumerate().any(|(k, t)| {
                    let entity = sig.and_then(|s| s.args.get(k)).map(|x| x.ty.is_entity()).unwrap_or(false);
                    entity && matches!(t, Term::Var(v, _) if !deps.literals[i].reads.contains(v))
                })
            }
            _ => false,
        }),
        _ => false,
    })
}

/// The longest lookback a program's windows and lags name, as text.
fn memo_horizon_text(prog: &Program) -> Option<String> {
    let mut longest: Option<Duration> = None;
    let mut rows: usize = 0;
    let mut unbounded = false;
    let dur_of = |e: &Expr, unit: &str| -> Option<Duration> {
        match e {
            Expr::Lit(Lit::Duration(d), _) => Some(*d),
            Expr::Param(p, _) => match prog.param(unit, p).map(|p| &p.value) {
                Some(Lit::Duration(d)) => Some(*d),
                _ => None,
            },
            _ => None,
        }
    };
    let count_of = |e: &Expr, unit: &str| -> Option<usize> {
        match e {
            Expr::Lit(Lit::Int(n), _) => Some(*n as usize),
            Expr::Param(p, _) => match prog.param(unit, p).map(|p| &p.value) {
                Some(Lit::Int(n)) => Some(*n as usize),
                _ => None,
            },
            _ => None,
        }
    };
    for r in &prog.rules {
        let mut stack: Vec<&Literal> = r.body.iter().collect();
        while let Some(l) = stack.pop() {
            match l {
                Literal::Agg { conj, .. } => stack.extend(conj.iter()),
                Literal::Window { kind: WindowKind::Rows, dur, .. } => match count_of(dur, &r.unit) {
                    Some(n) => rows = rows.max(n),
                    None => unbounded = true,
                },
                Literal::Window { dur, .. } | Literal::Builtin(Builtin::Lag { n: dur, .. }, _) => match dur_of(dur, &r.unit) {
                    Some(d) => {
                        longest = Some(match longest {
                            Some(l) if l.approx_days() >= d.approx_days() => l,
                            _ => d,
                        })
                    }
                    None => unbounded = true,
                },
                _ => {}
            }
        }
    }
    if unbounded {
        return Some("unbounded (a lookback the program does not fix)".into());
    }
    let mut parts = Vec::new();
    if let Some(d) = longest {
        parts.push(format!("{} + 7d", d));
    }
    if rows > 0 {
        parts.push(format!("{} rows", rows));
    }
    if parts.is_empty() {
        Some("7d".into())
    } else {
        Some(parts.join(", "))
    }
}

/// Plan a program (section 5.1). With `optimise` off every rule keeps its
/// written order (the unoptimised plan of section 6.6).
pub fn plan_program(prog: &Program, inputs: &PlanInputs, stats: &Statistics, numerics: Numerics, optimise: bool) -> Plan {
    let closed = closed_loop(prog);
    let shared: HashMap<(usize, usize), usize> = inputs.shared.iter().map(|&(r, l, p)| ((r, l), p)).collect();
    let mut rules = Vec::with_capacity(prog.rules.len());
    let mut aggregates = Vec::new();
    let mut totals = PlanTotals {
        rules: prog.rules.len(),
        ..Default::default()
    };
    for (ri, r) in prog.rules.iter().enumerate() {
        let deps = literal_deps(prog, ri);
        let costs = literal_costs(prog, ri, &deps, stats, &|li| inputs.cmp_selectivity.get(&(ri, li)).copied(), &|li| inputs.w_max.get(&(ri, li)).copied());
        let written: Vec<usize> = (0..r.body.len()).collect();
        let written_cost = order_cost(&costs, &written);
        let infeasible = inputs.infeasible.contains_key(&ri);
        let elided = inputs.elided.contains(&r.head.name);
        let searched = if optimise && !infeasible {
            search(&deps, &costs, stats)
        } else {
            super::search::Searched {
                order: written.clone(),
                cost: written_cost,
                kind: SearchKind::Written,
                considered: 1,
            }
        };
        if searched.order != written {
            totals.reordered += 1;
        }
        if searched.kind == SearchKind::Greedy {
            totals.greedy += 1;
        }
        if infeasible {
            totals.infeasible += 1;
        }
        if elided {
            totals.elided += 1;
        }
        let mut card = 1.0;
        let mut literals = Vec::with_capacity(r.body.len());
        for &li in &searched.order {
            let c = &costs[li];
            let out = card * c.factor;
            literals.push(LitPlan {
                index: li,
                text: r.body[li].describe(),
                kind: c.kind.clone(),
                card_in: card,
                card_out: out,
                cost: c.cost,
                selectivity: c.factor,
                observed: None,
            });
            card = out;
        }
        let est = if infeasible { 0.0 } else { searched.cost };
        totals.est_cost += est;
        totals.written_cost += written_cost;
        for (li, lit) in r.body.iter().enumerate() {
            if let Literal::Agg { agg, conj, .. } = lit {
                let window = conj.iter().find_map(|l| if let Literal::Window { kind, .. } = l { Some(*kind) } else { None });
                aggregates.push(AggPlan {
                    rule: ri,
                    literal: li,
                    agg: agg.clone(),
                    class: aggregate_class(agg).into(),
                    window: match window {
                        Some(WindowKind::Window) => "calendar window".into(),
                        Some(WindowKind::Prior) => "prior calendar window".into(),
                        Some(WindowKind::Rows) => "rows window".into(),
                        None => "no window (the group at the bar)".into(),
                    },
                    implementation: implementation(agg, window, numerics == Numerics::Running),
                    numerics,
                    shares_state_of: shared.get(&(ri, li)).copied(),
                });
            }
        }
        rules.push(RulePlan {
            rule: ri,
            label: prog.rule_label(ri),
            order: searched.order,
            literals,
            est_cost: est,
            written_cost,
            search: searched.kind,
            infeasible,
            elided,
            orders_considered: searched.considered,
        });
    }
    // Relations: store, indexes, memo policy, schedule.
    let mut asof_readers: BTreeSet<String> = BTreeSet::new();
    let mut rows_readers: BTreeSet<String> = BTreeSet::new();
    for r in &prog.rules {
        for l in &r.body {
            match l {
                Literal::AsOf { atom, .. } => {
                    asof_readers.insert(atom.name.clone());
                }
                Literal::Agg { conj, .. } if conj.iter().any(|c| matches!(c, Literal::Window { kind: WindowKind::Rows, .. })) => {
                    for c in conj {
                        if let Literal::Atom(a) = c {
                            rows_readers.insert(a.name.clone());
                        }
                    }
                }
                _ => {}
            }
        }
    }
    let horizon = memo_horizon_text(prog);
    let relations: Vec<RelPlan> = prog
        .relations
        .values()
        .map(|s| {
            let derived = matches!(s.kind, Kind::Derived { .. } | Kind::Output);
            RelPlan {
                name: s.name.clone(),
                kind: crate::compiler::print::kind_name(&s.kind).into(),
                store: if derived {
                    "memo by bar and inputs".into()
                } else {
                    "columnar, blocked by bar, sorted by entity".into()
                },
                asof_index: asof_readers.contains(&s.name),
                rows_window: rows_readers.contains(&s.name),
                memo_horizon: if derived { horizon.clone() } else { None },
                schedule: if closed.contains(&s.name) {
                    "per-bar (closed loop)".into()
                } else {
                    "ahead-of-time (open loop)".into()
                },
                demanded: inputs.demanded.as_ref().map(|d| d.contains(&s.name)).unwrap_or(true),
                elided: inputs.elided.contains(&s.name),
            }
        })
        .collect();
    let strata: Vec<StratumPlan> = prog
        .strata
        .iter()
        .enumerate()
        .map(|(i, names)| {
            let per_bar = names.iter().any(|n| closed.contains(n));
            let barrier = names.iter().any(|n| prog.rules_for(n).into_iter().any(|ri| cross_sectional(prog, ri)));
            StratumPlan {
                index: i,
                relations: names.clone(),
                schedule: if per_bar {
                    "per-bar".into()
                } else {
                    "ahead-of-time, bar-vectorised, partitioned by entity".into()
                },
                barrier,
            }
        })
        .collect();
    Plan {
        label: if optimise { "optimised".into() } else { "written".into() },
        numerics,
        rules,
        aggregates,
        relations,
        strata,
        totals,
        statistics: stats.source.clone(),
        notes: Vec::new(),
    }
}
