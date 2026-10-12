//! The cost model (compiler design, section 5.2) and the statistics it reads.

use std::collections::BTreeMap;

use crate::check::Program;
use crate::ir::*;
use crate::kernel::Dataset;

/// Per-column minima and maxima observed in a dataset (also what Analyse
/// labels bundle-relative).
#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
pub struct ColumnStats {
    pub min: f64,
    pub max: f64,
}

/// What a primitive relation looks like in the bound data.
#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
pub struct RelationStats {
    pub rows_per_bar: f64,
    pub rows_per_entity_bar: f64,
    /// The probability that a tuple exists at a given identity and bar.
    pub fill_ratio: f64,
    pub columns: BTreeMap<String, ColumnStats>,
}

/// The statistics the planner and Analyse read (section 5.2): defaults when
/// no data is bound, observed values from a dataset otherwise.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct Statistics {
    pub source: String,
    pub bars: f64,
    pub entities_mean: f64,
    pub entities_max: f64,
    pub relations: BTreeMap<String, RelationStats>,
    /// Decision bars before the run's first (`--start`): what windows can
    /// warm up on; unknown without a start.
    #[serde(default)]
    pub warmup_bars: Option<f64>,
}

impl Default for Statistics {
    fn default() -> Statistics {
        Statistics {
            source: "defaults".into(),
            bars: 1000.0,
            entities_mean: 100.0,
            entities_max: 100.0,
            relations: BTreeMap::new(),
            warmup_bars: None,
        }
    }
}

impl Statistics {
    /// Statistics of a dataset for a program: bars at the decision
    /// resolution, entities per bar from the universe-like primitive (the
    /// complete enumerable one, else every entity seen), rows per bar and
    /// fill ratio per primitive, and per-column minima and maxima.
    pub fn from_dataset(prog: &Program, ds: &Dataset, name: &str) -> Statistics {
        use crate::kernel::time;
        let key_of = |sig: &Signature, tu: &[crate::kernel::Value]| sig.key_pos().and_then(|k| tu.get(k)).and_then(|v| v.as_time());
        let mut decision_bars: std::collections::BTreeSet<i64> = std::collections::BTreeSet::new();
        for (name, tuples) in &ds.facts {
            let Some(sig) = prog.relations.get(name) else { continue };
            let Some(res) = sig.res else { continue };
            for tu in tuples {
                if let Some(t) = key_of(sig, tu) {
                    decision_bars.insert(if res == prog.resolution { t } else { time::bucket(prog.resolution, t) });
                }
            }
        }
        let bars = decision_bars.len().max(1) as f64;
        // Entities per bar: from `universe` when the environment has it.
        let universe = prog
            .relations
            .iter()
            .find(|(_, s)| matches!(s.kind, Kind::Primitive { .. }) && s.complete && s.args.iter().any(|a| a.mode == Mode::Out && a.ty.is_entity()))
            .map(|(n, _)| n.clone());
        let (entities_mean, entities_max) = match universe.as_ref().and_then(|u| ds.facts.get(u)) {
            Some(tuples) => {
                let sig = &prog.relations[universe.as_ref().unwrap()];
                let mut per_bar: BTreeMap<i64, usize> = BTreeMap::new();
                for tu in tuples {
                    if let Some(t) = key_of(sig, tu) {
                        *per_bar.entry(t).or_default() += 1;
                    }
                }
                let n = per_bar.len().max(1) as f64;
                let mean = per_bar.values().sum::<usize>() as f64 / n;
                let max = per_bar.values().copied().max().unwrap_or(0) as f64;
                (mean.max(1.0), max.max(1.0))
            }
            None => {
                let n = ds.symbols.len().max(1) as f64;
                (n, n)
            }
        };
        let mut relations = BTreeMap::new();
        for (name, tuples) in &ds.facts {
            let Some(sig) = prog.relations.get(name) else { continue };
            let Some(k) = sig.key_pos() else { continue };
            let mut per_bar: BTreeMap<i64, usize> = BTreeMap::new();
            let mut columns: BTreeMap<String, ColumnStats> = BTreeMap::new();
            for tu in tuples {
                if let Some(t) = tu[k].as_time() {
                    *per_bar.entry(t).or_default() += 1;
                }
                for (arg, v) in sig.args.iter().zip(tu.iter()) {
                    if let Some(x) = v.as_f64() {
                        let c = columns.entry(arg.name.clone()).or_insert(ColumnStats {
                            min: f64::INFINITY,
                            max: f64::NEG_INFINITY,
                        });
                        c.min = c.min.min(x);
                        c.max = c.max.max(x);
                    }
                }
            }
            let own_bars = per_bar.len().max(1) as f64;
            let rows_per_bar = tuples.len() as f64 / own_bars;
            let has_entity = sig.args.iter().any(|a| a.ty.is_entity());
            let rows_per_entity_bar = if has_entity { rows_per_bar / entities_mean.max(1.0) } else { rows_per_bar };
            let fill_ratio = if has_entity {
                (rows_per_bar / entities_mean.max(1.0)).min(1.0)
            } else {
                (own_bars / bars).min(1.0)
            };
            relations.insert(
                name.clone(),
                RelationStats {
                    rows_per_bar,
                    rows_per_entity_bar,
                    fill_ratio,
                    columns,
                },
            );
        }
        Statistics {
            source: name.to_string(),
            bars,
            entities_mean,
            entities_max,
            relations,
            warmup_bars: None,
        }
    }

    /// With the warm-up known: the decision bars of the dataset before `start`.
    pub fn with_start(mut self, prog: &Program, ds: &Dataset, start: Option<i64>) -> Statistics {
        if let Some(start) = start {
            use crate::kernel::time;
            let mut bars: std::collections::BTreeSet<i64> = std::collections::BTreeSet::new();
            for (name, tuples) in &ds.facts {
                let Some(sig) = prog.relations.get(name) else { continue };
                let (Some(k), Some(res)) = (sig.key_pos(), sig.res) else { continue };
                for tu in tuples {
                    if let Some(t) = tu[k].as_time() {
                        bars.insert(if res == prog.resolution { t } else { time::bucket(prog.resolution, t) });
                    }
                }
            }
            self.warmup_bars = Some(bars.range(..start).count() as f64);
        }
        self
    }

    pub fn relation(&self, name: &str) -> RelationStats {
        self.relations.get(name).cloned().unwrap_or(RelationStats {
            rows_per_bar: self.entities_mean,
            rows_per_entity_bar: 1.0,
            fill_ratio: 1.0,
            columns: BTreeMap::new(),
        })
    }
}

/// The System R default selectivity of a range predicate (section 5.2).
pub const RANGE_SELECTIVITY: f64 = 1.0 / 3.0;

/// The cost of one literal: what one evaluation costs and by what factor
/// it multiplies the bindings (above one for a generator, below one for a
/// test).
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct LiteralCost {
    pub cost: f64,
    pub factor: f64,
    pub kind: String,
}

/// Gray's class of an aggregate (section 5.4).
pub fn aggregate_class(agg: &str) -> &'static str {
    match agg {
        "sum" | "count" | "max" | "min" | "first" | "last" => "distributive",
        "mean" | "std" | "cov" | "corr" | "ols_beta" => "algebraic",
        "median" | "quantile" => "holistic",
        _ => "unknown",
    }
}

/// The kernel's implementation of an aggregate over a window under the
/// numerics setting: what the plan reports (section 5.4, as this kernel
/// carries it).
pub fn implementation(agg: &str, window: Option<WindowKind>, running: bool) -> String {
    let moment = matches!(agg, "mean" | "sum" | "std" | "cov" | "corr" | "ols_beta");
    match (window, moment, running) {
        (Some(WindowKind::Rows), true, true) => "sliding compensated sums (Neumaier), rebuilt every N rows; exact two-pass where the sums cannot answer".into(),
        (Some(WindowKind::Rows), true, false) => "two-pass over the window's cached rows".into(),
        (Some(WindowKind::Rows), false, _) => match agg {
            "count" => "the cached row count".into(),
            "median" | "quantile" => "sort the window's cached rows".into(),
            _ => "recompute over the window's cached rows".into(),
        },
        (Some(_), _, _) => match agg {
            "count" => "the cached row count".into(),
            "median" | "quantile" => "sort the window's cached rows".into(),
            _ => "recompute over the window's cached rows (one solve per bar)".into(),
        },
        (None, _, _) => "solve the group and aggregate once per call".into(),
    }
}

/// The cost and cardinality factor of each literal of a rule (section 5.2),
/// from the statistics, the binding structure and the selectivities Analyse
/// supplies for comparisons (`cmp_selectivity[i]`, 1/3 when unknown).
pub fn literal_costs(
    prog: &Program,
    rule: usize,
    deps: &super::search::RuleDeps,
    stats: &Statistics,
    cmp_selectivity: &dyn Fn(usize) -> Option<f64>,
    w_max: &dyn Fn(usize) -> Option<f64>,
) -> Vec<LiteralCost> {
    let r = &prog.rules[rule];
    let mut out = Vec::with_capacity(r.body.len());
    for (i, lit) in r.body.iter().enumerate() {
        let d = &deps.literals[i];
        let (cost, factor, kind) = match lit {
            Literal::Atom(a) | Literal::AsOf { atom: a, .. } => {
                let sig = prog.relations.get(&a.name);
                let rs = stats.relation(&a.name);
                let free_entity = sig
                    .map(|s| {
                        s.args
                            .iter()
                            .zip(a.terms.iter())
                            .any(|(arg, t)| arg.ty.is_entity() && matches!(t, Term::Var(v, _) if d.binds.contains(v)) || (arg.ty.is_entity() && matches!(t, Term::Wild(_))))
                    })
                    .unwrap_or(false);
                let stored = sig.map(|s| !matches!(s.kind, Kind::Derived { .. })).unwrap_or(true);
                let derived_cost = if stored { 1.0 } else { 4.0 };
                if free_entity {
                    (derived_cost, rs.rows_per_bar.max(1e-3), "generator".to_string())
                } else {
                    (derived_cost, rs.fill_ratio.clamp(1e-6, 1.0) * rs.rows_per_entity_bar.max(1.0), "lookup".to_string())
                }
            }
            Literal::Neg(a) => {
                // Without data the fill ratio is a guess: a negation then
                // excludes half; with data it excludes what the data says.
                let known = stats.source != "defaults" && stats.relations.contains_key(&a.name);
                let sel = if known { (1.0 - stats.relation(&a.name).fill_ratio).clamp(0.01, 0.99) } else { 0.5 };
                (1.0, sel, "negation".to_string())
            }
            Literal::Builtin(b, _) => match b {
                Builtin::Prev { .. } => (1.0, 1.0 - 1.0 / stats.bars.max(2.0), "time".to_string()),
                Builtin::Lag { .. } => (1.0, 0.9, "time".to_string()),
                Builtin::MonthStart { .. } => (1.0, 1.0 / 21.0, "time".to_string()),
                Builtin::DayStart { .. } => (1.0, 1.0 / 390.0, "time".to_string()),
            },
            Literal::Window { .. } => (1.0, 1.0, "window".to_string()),
            Literal::Cmp { .. } => (1.0, cmp_selectivity(i).unwrap_or(RANGE_SELECTIVITY).clamp(0.0, 1.0), "comparison".to_string()),
            Literal::Assign { var, .. } => {
                if d.binds.contains(var) {
                    (1.0, 1.0, "assignment".to_string())
                } else {
                    (1.0, 0.1, "equality".to_string())
                }
            }
            Literal::Agg { agg, conj, .. } => {
                let window = conj.iter().find_map(|l| if let Literal::Window { kind, .. } = l { Some(*kind) } else { None });
                let w = w_max(i).unwrap_or(20.0);
                let class = aggregate_class(agg);
                // A windowed aggregation costs the cache's lookup and its
                // arguments' evaluation, plus the recomputation over the
                // window where the kernel has no running state; a group
                // without a window is solved whole at every call.
                let cost = match (window, class) {
                    (Some(_), "holistic") => 4.0 + w.log2().max(1.0),
                    (Some(_), _) => 4.0 + w / 4.0,
                    (None, _) => 4.0 + stats.entities_mean.max(1.0),
                };
                (cost, 0.95, "aggregation".to_string())
            }
            Literal::Top { n, atom, .. } => {
                let rs = stats.relation(&atom.name);
                let group = rs.rows_per_bar.max(1.0);
                let kept = match n {
                    Some(Expr::Lit(Lit::Int(k), _)) => (*k as f64).min(group),
                    _ => group,
                };
                (group * group.log2().max(1.0), kept.max(1e-3), "reduction".to_string())
            }
            Literal::Resample { inner, .. } => {
                let rs = stats.relation(&inner.name);
                (rs.rows_per_bar.max(1.0) * 20.0, 1.0, "resample".to_string())
            }
        };
        out.push(LiteralCost { cost, factor, kind });
    }
    out
}
