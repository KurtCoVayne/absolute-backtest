//! An execution's rows (Q-2.5, Q-2.6, P-4): every run a trial in
//! `query.trials`; the plan's outcomes in `query.<plan>.outcomes`, one row
//! per run with every metric (replaced at every execution: they are the
//! latest's); the execution in `query.<plan>.executions`; the plan in
//! `query.plans` with the bindings it pins.

use std::collections::BTreeMap;

use abt_wire::generated::abt::wire::Role;

use super::exec::{Execution, Kind, Outcome};
use super::lineage::TRIALS;
use super::{Plan, Point};
use crate::data::{Data, Error};
use crate::relation::{self, Col, Rows};

pub const PLANS: &str = "query.plans";

/// The relation of a plan's `name` under its hash.
pub fn relation_of(hash: &str, name: &str) -> String {
    format!("query.{}.{}", hash, name)
}

fn point_json(p: &Point) -> String {
    let m: BTreeMap<&str, &str> = p.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
    serde_json::to_string(&m).unwrap_or_default()
}

fn num(v: f64) -> Option<f64> {
    v.is_finite().then_some(v)
}

/// The metric columns of an outcome, by name (Q-3.1's return and trading
/// groups).
fn metric_columns(os: &[&Outcome]) -> Vec<(&'static str, Col)> {
    let n = |f: &dyn Fn(&Outcome) -> f64| Col::Num(os.iter().map(|o| num(f(o))).collect());
    let c = |f: &dyn Fn(&Outcome) -> usize| Col::Count(os.iter().map(|o| Some(f(o) as i64)).collect());
    vec![
        ("n", c(&|o| o.returns.n)),
        ("total_return", n(&|o| o.returns.total_return)),
        ("cagr", n(&|o| o.returns.cagr)),
        ("volatility", n(&|o| o.returns.volatility)),
        ("sharpe", n(&|o| o.returns.sharpe)),
        ("sharpe_lo", n(&|o| o.returns.sharpe_lo)),
        ("sharpe_se", n(&|o| o.returns.sharpe_se)),
        ("sharpe_period", n(&|o| o.returns.sharpe_period)),
        ("sortino", n(&|o| o.returns.sortino)),
        ("calmar", n(&|o| crate::metrics::objective_value("calmar", &o.returns, &o.trading).unwrap_or(f64::NAN))),
        ("max_drawdown", n(&|o| o.returns.max_drawdown)),
        ("max_drawdown_duration", c(&|o| o.returns.max_drawdown_duration)),
        ("cvar_5", n(&|o| o.returns.cvar_5)),
        ("skew", n(&|o| o.returns.skew)),
        ("kurtosis", n(&|o| o.returns.kurtosis)),
        ("worst_month", n(&|o| o.returns.worst_month)),
        ("t_stat", n(&|o| o.returns.t_stat)),
        ("autocorrelation_1", n(&|o| o.returns.autocorrelation_1)),
        ("turnover", n(&|o| o.trading.turnover)),
        ("fill_ratio", n(&|o| o.trading.fill_ratio)),
        ("avg_participation", n(&|o| o.trading.avg_participation)),
        ("max_participation", n(&|o| o.trading.max_participation)),
        ("avg_gross", n(&|o| o.trading.avg_gross)),
        ("avg_net", n(&|o| o.trading.avg_net)),
        ("max_leverage", n(&|o| o.trading.max_leverage)),
        ("cost_drag", n(&|o| o.trading.cost_drag)),
        ("impact_drag", n(&|o| o.trading.impact_drag)),
        ("fills", c(&|o| o.trading.fills)),
        ("decisions", c(&|o| o.trading.decisions)),
        ("dropped", c(&|o| o.trading.dropped)),
    ]
}

/// The rows of an execution, written; the binding pinned (Q-2.6).
pub fn write(data: &Data, plan: &Plan, ex: &Execution) -> Result<(), Error> {
    let os: Vec<&Outcome> = ex.outcomes.iter().collect();
    let best: Vec<bool> = (0..os.len()).map(|i| ex.grids.iter().any(|g| g.runs.get(g.best) == Some(&i))).collect();
    if !os.is_empty() {
        // The trials, added to every earlier one.
        let texts = |f: &dyn Fn(&Outcome) -> String| Col::Label(os.iter().map(|o| Some(f(o))).collect());
        let (schema, batch) = Rows::new(TRIALS)
            .col("trial_id", Role::Label, texts(&|o| o.run_id.clone()))
            .col("t", Role::EventTime, Col::Time(vec![Some(ex.at); os.len()]))
            .col("plan_hash", Role::Value, Col::Label(vec![Some(ex.hash.clone()); os.len()]))
            .col("step", Role::Value, texts(&|o| o.step.clone()))
            .col("lineage", Role::Value, Col::Label(vec![Some(ex.lineage.clone()); os.len()]))
            .col("program_hash", Role::Value, texts(&|o| o.program_hash.clone()))
            .col("binding_id", Role::Value, texts(&|o| o.binding.clone()))
            .col("run_id", Role::Value, texts(&|o| o.run_id.clone()))
            .col("point", Role::Value, Col::Bytes(os.iter().map(|o| Some(point_json(&o.point))).collect()))
            .col("window_from", Role::Value, Col::Time(os.iter().map(|o| o.window.map(|w| w.0)).collect()))
            .col("window_to", Role::Value, Col::Time(os.iter().map(|o| o.window.map(|w| w.1)).collect()))
            .col("holdout", Role::Value, Col::Label(vec![Some(ex.holdout.clone()); os.len()]))
            .col("kind", Role::Value, texts(&|o| o.kind.name().to_string()))
            .col("objective", Role::Value, Col::Label(vec![Some(plan.objective.clone()); os.len()]))
            .col("value", Role::Value, Col::Num(os.iter().map(|o| num(o.objective)).collect()))
            .col("sharpe", Role::Value, Col::Num(os.iter().map(|o| num(o.returns.sharpe_period)).collect()))
            .col("fold", Role::Value, Col::Count(os.iter().map(|o| o.fold.as_ref().map(|f| f.k as i64)).collect()))
            .col("phase", Role::Value, Col::Label(os.iter().map(|o| o.fold.as_ref().map(|f| f.phase.to_string())).collect()))
            .col("status", Role::Value, texts(&|o| o.status.clone()))
            .col("test", Role::Value, Col::Label(vec![None; os.len()]))
            .col("test_version", Role::Value, Col::Label(vec![None; os.len()]))
            .finish()?;
        data.put(schema, &batch, false)?;
        // The outcomes, the latest execution's.
        let mut rows = Rows::new(relation_of(&ex.hash, "outcomes"))
            .col("step", Role::Label, Col::Label(os.iter().map(|o| Some(o.step.clone())).collect()))
            .col("run_id", Role::Label, Col::Label(os.iter().map(|o| Some(o.run_id.clone())).collect()))
            .col("t", Role::EventTime, Col::Time(vec![Some(ex.at); os.len()]))
            .col("canonical", Role::Value, Col::Label(os.iter().map(|o| Some(o.canonical.clone())).collect()))
            .col("kind", Role::Value, Col::Label(os.iter().map(|o| Some(o.kind.name().to_string())).collect()))
            .col("status", Role::Value, Col::Bytes(os.iter().map(|o| Some(o.status.clone())).collect()))
            .col("point", Role::Value, Col::Bytes(os.iter().map(|o| Some(point_json(&o.point))).collect()))
            .col("fold", Role::Value, Col::Count(os.iter().map(|o| o.fold.as_ref().map(|f| f.k as i64)).collect()))
            .col("phase", Role::Value, Col::Label(os.iter().map(|o| o.fold.as_ref().map(|f| f.phase.to_string())).collect()))
            .col("best", Role::Value, Col::Bool(best.iter().map(|b| Some(*b)).collect()))
            .col("window_from", Role::Value, Col::Time(os.iter().map(|o| o.window.map(|w| w.0)).collect()))
            .col("window_to", Role::Value, Col::Time(os.iter().map(|o| o.window.map(|w| w.1)).collect()))
            .col("objective", Role::Value, Col::Num(os.iter().map(|o| num(o.objective)).collect()));
        for (name, c) in metric_columns(&os) {
            rows = rows.col(name, Role::Value, c);
        }
        // A reveal adds its run to the outcomes it revealed a point of.
        let reveal = os.iter().any(|o| o.kind == Kind::Reveal);
        let (schema, batch) = rows.finish()?;
        data.put(schema, &batch, !reveal)?;
    }
    // The execution.
    let (schema, batch) = Rows::new(relation_of(&ex.hash, "executions"))
        .col("binding", Role::Label, Col::Label(vec![Some(ex.binding.clone())]))
        .col("t", Role::EventTime, Col::Time(vec![Some(ex.at)]))
        .col(
            "kind",
            Role::Value,
            Col::Label(vec![Some(if ex.outcomes.iter().any(|o| o.kind == Kind::Reveal) { "reveal" } else { "run" }.to_string())]),
        )
        .col("runs", Role::Value, Col::Count(vec![Some(ex.outcomes.len() as i64)]))
        .col("trials", Role::Value, Col::Count(vec![Some(ex.trials as i64)]))
        .finish()?;
    data.put(schema, &batch, false)?;
    // The plan, with every binding it has run over, pinned.
    let mut plans = read_plans(data)?;
    let entry = plans.entry(ex.hash.clone()).or_insert_with(|| PlanRow {
        hash: ex.hash.clone(),
        name: plan.name.clone(),
        normal_form: ex.normal_form.clone(),
        program_hash: ex.program_hash.clone(),
        lineage: ex.lineage.clone(),
        created: ex.at,
        bindings: vec![],
    });
    let mut bindings: Vec<String> = ex.outcomes.iter().map(|o| o.binding.clone()).filter(|b| !b.is_empty()).collect();
    bindings.push(ex.binding.clone());
    for b in bindings {
        if !entry.bindings.contains(&b) {
            data.pin(&b, true)?;
            entry.bindings.push(b);
        }
    }
    write_plans(data, &plans)
}

/// A plan as `query.plans` holds it.
#[derive(Clone, Debug, PartialEq, serde::Serialize)]
pub struct PlanRow {
    pub hash: String,
    pub name: String,
    pub normal_form: String,
    pub program_hash: String,
    pub lineage: String,
    pub created: i64,
    pub bindings: Vec<String>,
}

pub fn read_plans(data: &Data) -> Result<BTreeMap<String, PlanRow>, Error> {
    let Some(b) = relation::read(data, PLANS)? else { return Ok(BTreeMap::new()) };
    let (h, n, nf, p, l, t, bs) = (
        relation::texts(&b, "hash")?,
        relation::texts(&b, "name")?,
        relation::texts(&b, "normal_form")?,
        relation::texts(&b, "program_hash")?,
        relation::texts(&b, "lineage")?,
        relation::ints(&b, "t")?,
        relation::texts(&b, "bindings")?,
    );
    Ok((0..b.num_rows())
        .map(|i| {
            let row = PlanRow {
                hash: h[i].clone().unwrap_or_default(),
                name: n[i].clone().unwrap_or_default(),
                normal_form: nf[i].clone().unwrap_or_default(),
                program_hash: p[i].clone().unwrap_or_default(),
                lineage: l[i].clone().unwrap_or_default(),
                created: t[i].unwrap_or(0),
                bindings: bs[i].as_deref().and_then(|j| serde_json::from_str(j).ok()).unwrap_or_default(),
            };
            (row.hash.clone(), row)
        })
        .collect())
}

/// `query.plans` written whole (it holds a row per plan, changed as the
/// plan's bindings grow; E-56's replace).
pub fn write_plans(data: &Data, plans: &BTreeMap<String, PlanRow>) -> Result<(), Error> {
    let rows: Vec<&PlanRow> = plans.values().collect();
    let (schema, batch) = Rows::new(PLANS)
        .col("hash", Role::Label, Col::Label(rows.iter().map(|r| Some(r.hash.clone())).collect()))
        .col("t", Role::EventTime, Col::Time(rows.iter().map(|r| Some(r.created)).collect()))
        .col("name", Role::Value, Col::Bytes(rows.iter().map(|r| Some(r.name.clone())).collect()))
        .col("normal_form", Role::Value, Col::Bytes(rows.iter().map(|r| Some(r.normal_form.clone())).collect()))
        .col("program_hash", Role::Value, Col::Label(rows.iter().map(|r| Some(r.program_hash.clone())).collect()))
        .col("lineage", Role::Value, Col::Label(rows.iter().map(|r| Some(r.lineage.clone())).collect()))
        .col(
            "bindings",
            Role::Value,
            Col::Bytes(rows.iter().map(|r| Some(serde_json::to_string(&r.bindings).unwrap_or_default())).collect()),
        )
        .finish()?;
    data.put(schema, &batch, true).map(|_| ())
}

/// `abt plan drop`: the plan leaves `query.plans`, and the bindings no other
/// plan holds are unpinned (Q-2.6). Its trials stay: they are its
/// lineage's.
pub fn drop_plan(data: &Data, hash: &str) -> Result<PlanRow, Error> {
    let mut plans = read_plans(data)?;
    let row = plans.remove(hash).ok_or_else(|| Error::Missing(format!("no plan {} in the data layer", hash)))?;
    for b in &row.bindings {
        if !plans.values().any(|p| p.bindings.contains(b)) {
            data.pin(b, false)?;
        }
    }
    write_plans(data, &plans)?;
    Ok(row)
}

/// An outcome row of a plan as the store holds it.
#[derive(Clone, Debug, PartialEq)]
pub struct Stored {
    pub step: String,
    pub run_id: String,
    pub point: Point,
    /// The best point of its grid.
    pub best: bool,
    pub kind: String,
}

/// The latest execution's outcome rows of a plan (and its reveals').
pub fn outcomes(data: &Data, hash: &str) -> Result<Vec<Stored>, Error> {
    let Some(b) = relation::read(data, &relation_of(hash, "outcomes"))? else { return Ok(vec![]) };
    let (s, r, p, best, k) = (
        relation::texts(&b, "step")?,
        relation::texts(&b, "run_id")?,
        relation::texts(&b, "point")?,
        relation::bools(&b, "best")?,
        relation::texts(&b, "kind")?,
    );
    Ok((0..b.num_rows())
        .map(|i| {
            let point: BTreeMap<String, String> = p[i].as_deref().and_then(|j| serde_json::from_str(j).ok()).unwrap_or_default();
            Stored {
                step: s[i].clone().unwrap_or_default(),
                run_id: r[i].clone().unwrap_or_default(),
                point: point.into_iter().collect(),
                best: best[i].unwrap_or(false),
                kind: k[i].clone().unwrap_or_default(),
            }
        })
        .collect())
}
