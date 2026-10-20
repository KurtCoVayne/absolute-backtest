//! Claims and test steps (Q-4.3, Q-5). A test of the registry is applied
//! to an input over a window, with its parameters and a seed fixed by the
//! plan, so that its result is the same to the bit at every execution
//! (Q-4.1). A claim's verdict compares the result with its expectation,
//! its p-value adjusted by Benjamini and Hochberg over the plan's claims
//! of its phase (Q-4.3: a `before` claim decides before the `after` claims
//! exist). A verdict is `holds`, `fails` or `undecidable` with the reason
//! (Definition Q-5.1); judging changes no run (Proposition Q-5.2).

use std::collections::BTreeMap;

use super::inputs::{self, Keep};
use super::{Claim, Expect, Input, Op, Phase, Point, Policy, Ref};
use crate::data::{Data, Error};
use crate::tests::{self, Param, ParamKind, Params, Table, Test, TestResult};

/// A claim's verdict (Definition Q-5.1).
#[derive(Clone, Debug, PartialEq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Verdict {
    Holds,
    Fails,
    Undecidable(String),
}

impl Verdict {
    pub fn text(&self) -> String {
        match self {
            Verdict::Holds => "holds".into(),
            Verdict::Fails => "fails".into(),
            Verdict::Undecidable(why) => format!("undecidable({})", why),
        }
    }
}

/// A claim judged, or a test step run, with what it read.
#[derive(Clone, Debug, serde::Serialize)]
pub struct Judged {
    /// The claim or step as written, and its canonical name.
    pub name: String,
    pub canonical: String,
    /// `claim` or `test`.
    pub kind: &'static str,
    pub test: String,
    pub test_version: String,
    pub input: String,
    /// The first and last time of the rows the test read.
    pub window: Option<(i64, i64)>,
    pub params: Point,
    pub result: Option<TestResult>,
    /// Why the test gave no result.
    pub error: Option<String>,
    pub p_adjusted: Option<f64>,
    pub level: Option<f64>,
    pub dependence: String,
    pub phase: Option<Phase>,
    pub policy: Option<Policy>,
    pub expect: Option<String>,
    pub verdict: Option<Verdict>,
    /// Judged in sample, or over a reveal's embargoed bars.
    pub in_sample: bool,
    /// The runs the input read.
    pub runs: Vec<String>,
    pub trial_id: String,
}

/// The run ids a reference reads.
pub type RunsOf<'a> = Box<dyn Fn(&Ref) -> Result<Vec<String>, Error> + 'a>;

/// The relation a ledger step wrote.
pub type LedgerOf<'a> = Box<dyn Fn(&str) -> Result<String, Error> + 'a>;

/// What resolving an input needs from the plan's execution.
pub struct Context<'a> {
    pub data: &'a Data,
    pub strategy: &'a str,
    /// The relations of the binding, with their versions.
    pub binding: &'a [(String, String)],
    /// The run ids a reference reads: a run's, a grid's best point's, or
    /// every point's of a grid read whole.
    pub runs: RunsOf<'a>,
    /// The relation a ledger step wrote.
    pub ledger: LedgerOf<'a>,
    /// A walk-forward step's folds: the in-sample and out-of-sample CAGR of
    /// each (what `wfe` reads, through `grid NAME`).
    pub folds: FoldsOf<'a>,
    /// The values of the parameters a plan supplies when a claim does not
    /// write them: the lineage's trial count and the variance of its trials'
    /// Sharpe ratios, for `dsr` (Q-3.2).
    pub defaults: Params,
    pub plan_hash: &'a str,
}

/// A walk-forward step's folds.
pub type FoldsOf<'a> = Box<dyn Fn(&str) -> Option<Vec<(f64, f64)>> + 'a>;

/// The seed of a claim or test (Q-4.1): the first eight bytes of the
/// BLAKE3 of the plan's hash and the canonical name.
pub fn seed(plan_hash: &str, canonical: &str) -> u64 {
    let h = abt_wire::Hash::of(format!("{}:{}", plan_hash, canonical).as_bytes());
    u64::from_le_bytes(h.0[..8].try_into().expect("eight bytes"))
}

/// The written parameters as the test's typed ones; a name the test does
/// not take is refused.
pub fn typed(test: &dyn Test, written: &Point) -> Result<Params, String> {
    let mut out = Params::new();
    for (k, v) in written {
        let spec = test.params().iter().find(|p| p.name == k).ok_or_else(|| {
            format!(
                "`{}` is not a parameter of {} ({})",
                k,
                test.name(),
                test.params().iter().map(|p| p.name).collect::<Vec<_>>().join(", ")
            )
        })?;
        let v = v.trim();
        let p = match spec.kind {
            ParamKind::Int => Param::Int(v.parse().map_err(|_| format!("{}: `{}` is not an integer", k, v))?),
            ParamKind::Float => Param::Float(v.parse().map_err(|_| format!("{}: `{}` is not a number", k, v))?),
            ParamKind::Bool => Param::Bool(v.parse().map_err(|_| format!("{}: `{}` is not true or false", k, v))?),
            ParamKind::Text => Param::Text(v.trim_matches('"').to_string()),
        };
        if let (Some((lo, hi)), Param::Int(_) | Param::Float(_)) = (spec.range, &p) {
            let x = match p {
                Param::Int(i) => i as f64,
                Param::Float(f) => f,
                _ => unreachable!(),
            };
            if x < lo || x > hi {
                return Err(format!("{}: {} is outside {}..{}", k, v, lo, hi));
            }
        }
        out.insert(k.clone(), p);
    }
    Ok(out)
}

/// The tables `input` resolves to for `test`, and the runs read.
pub fn tables(ctx: &Context, input: &Input, test: &dyn Test, params: &Params, keep: Keep) -> Result<(Vec<Table>, Vec<String>), Error> {
    let wants = |c: &str| test.inputs().first().is_some_and(|s| s.columns.contains(&c));
    let one = |r: &Ref| -> Result<String, Error> {
        let runs = (ctx.runs)(r)?;
        match runs.as_slice() {
            [run] => Ok(run.clone()),
            _ => Err(Error::Malformed(format!("`{}` reads one run, and `{}` made {}", test.name(), r.step, runs.len()))),
        }
    };
    Ok(match input {
        Input::Returns(r) => {
            let runs = (ctx.runs)(r)?;
            let t = if runs.len() == 1 {
                inputs::returns(ctx.data, &runs[0], keep)?
            } else {
                inputs::grid(ctx.data, &runs, keep)?
            };
            (vec![t], runs)
        }
        Input::Grid(g) => match (ctx.folds)(g) {
            // A walk-forward's folds, a row each.
            Some(folds) => {
                let (is, oos): (Vec<f64>, Vec<f64>) = folds.into_iter().unzip();
                let t = Table {
                    time: None,
                    entity: None,
                    columns: BTreeMap::from([("in_sample".to_string(), is), ("out_of_sample".to_string(), oos)]),
                };
                (vec![t], (ctx.runs)(&Ref { step: g.clone(), best: false }).unwrap_or_default())
            }
            None => {
                let runs = (ctx.runs)(&Ref { step: g.clone(), best: false })?;
                (vec![inputs::grid(ctx.data, &runs, keep)?], runs)
            }
        },
        Input::Events(r) => {
            let run = one(r)?;
            let t = if wants("realised") {
                inputs::fills(ctx.data, &run, keep)?
            } else {
                inputs::activations(ctx.data, &run, ctx.strategy, keep)?
            };
            (vec![t], vec![run])
        }
        Input::Feature { relation, of } => {
            let run = one(of)?;
            let mut t = inputs::feature(ctx.data, &run, relation, keep)?;
            if wants("forward") {
                // The feature's value column, and the forward returns of its
                // instruments over `horizon` bars of the binding's close.
                let value = t.columns.keys().last().cloned().ok_or_else(|| Error::Malformed(format!("`{}` has no numeric column", relation)))?;
                let values = t.columns.remove(&value).unwrap_or_default();
                t.columns.clear();
                t.columns.insert("feature".into(), values);
                let horizon = match params.get("horizon") {
                    Some(Param::Int(h)) if *h > 0 => *h as usize,
                    _ => 1,
                };
                let prices = inputs::feed(ctx.data, ctx.binding, "close", None, &|_| true)?;
                let at: Vec<(i64, u64)> = t.time.clone().unwrap_or_default().into_iter().zip(t.entity.clone().unwrap_or_default()).collect();
                t.columns.insert("forward".into(), inputs::forward_returns(&prices, horizon, &at));
            }
            (vec![t], vec![run])
        }
        Input::Feed { relation, filter } => (vec![inputs::feed(ctx.data, ctx.binding, relation, filter.as_deref(), keep)?], vec![]),
        Input::Ledger(step) => {
            let rel = (ctx.ledger)(step)?;
            let run = rel.strip_prefix("kernel.").and_then(|r| r.split('.').next()).unwrap_or_default().to_string();
            (vec![inputs::ledger(ctx.data, &rel, keep)?], vec![run])
        }
    })
}

/// Benjamini and Hochberg's adjusted p-values (1995): the i-th smallest of
/// m is min over j ≥ i of m·p₍ⱼ₎/j, at most 1; a missing p-value stays
/// missing and is not counted.
pub fn benjamini_hochberg(p: &[Option<f64>]) -> Vec<Option<f64>> {
    let mut idx: Vec<usize> = (0..p.len()).filter(|&i| p[i].is_some_and(f64::is_finite)).collect();
    idx.sort_by(|&a, &b| p[a].partial_cmp(&p[b]).expect("finite").then(a.cmp(&b)));
    let m = idx.len() as f64;
    let mut out = vec![None; p.len()];
    let mut running = 1.0f64;
    for (rank, &i) in idx.iter().enumerate().rev() {
        let adj = (p[i].expect("finite") * m / (rank + 1) as f64).min(running).min(1.0);
        running = adj;
        out[i] = Some(adj);
    }
    out
}

/// A claim's verdict on its result, its p-value adjusted.
pub fn verdict(c: &Claim, r: &TestResult, p_adjusted: Option<f64>) -> Verdict {
    let of = |b: bool| if b { Verdict::Holds } else { Verdict::Fails };
    let significant = |p: Option<f64>| -> Option<bool> { c.level.map(|l| p.is_some_and(|p| p < l)) };
    match &c.expect {
        Expect::Reject | Expect::Accept => match p_adjusted {
            None => Verdict::Undecidable("the test gives no p-value".into()),
            Some(p) => {
                let rejects = p < c.level.unwrap_or(0.05);
                of(if c.expect == Expect::Reject { rejects } else { !rejects })
            }
        },
        Expect::Statistic(op, v) => {
            if !r.statistic.is_finite() {
                return Verdict::Undecidable("the statistic is not finite".into());
            }
            of(op.holds(r.statistic, *v) && significant(p_adjusted).unwrap_or(true))
        }
        Expect::Effect(op, v) => match r.effect.filter(|e| e.is_finite()) {
            None => Verdict::Undecidable("the test reports no effect".into()),
            Some(e) => of(op.holds(e, *v) && significant(p_adjusted).unwrap_or(true)),
        },
        Expect::IntervalExcludes(v) => match r.interval {
            None => Verdict::Undecidable("the test reports no interval".into()),
            Some((lo, hi, _)) => of(*v < lo || *v > hi),
        },
        // Every point of the interval: its lower end for > and >=, its
        // upper end for < and <= (E-112).
        Expect::Interval(op, v) => match r.interval {
            None => Verdict::Undecidable("the test reports no interval".into()),
            Some((lo, hi, _)) => of(match op {
                Op::Gt | Op::Ge => op.holds(lo, *v),
                Op::Lt | Op::Le => op.holds(hi, *v),
            }),
        },
    }
}

/// One test applied: the registry's test by name, the input resolved
/// over the bars `keep` and the window keeps, the parameters typed.
#[allow(clippy::too_many_arguments)]
pub fn apply(ctx: &Context, name: &str, canonical: &str, kind: &'static str, test_name: &str, input: &Input, over: &Option<super::Window>, params: &Point, keep: Keep, in_sample: bool) -> Judged {
    let mut j = Judged {
        name: name.to_string(),
        canonical: canonical.to_string(),
        kind,
        test: test_name.to_string(),
        test_version: String::new(),
        input: input.render(&|s: &str| s.to_string()),
        window: None,
        params: params.clone(),
        result: None,
        error: None,
        p_adjusted: None,
        level: None,
        dependence: String::new(),
        phase: None,
        policy: None,
        expect: None,
        verdict: None,
        in_sample,
        runs: vec![],
        trial_id: String::new(),
    };
    let Some(test) = tests::get(test_name) else {
        j.error = Some(format!("no test `{}` is registered ({})", test_name, tests::names().join(", ")));
        return j;
    };
    j.test_version = test.version().to_string();
    j.dependence = serde_json::to_string(&test.dependence()).unwrap_or_default();
    let mut typed = match typed(test.as_ref(), params) {
        Ok(p) => p,
        Err(e) => {
            j.error = Some(e);
            return j;
        }
    };
    for (k, v) in &ctx.defaults {
        if test.params().iter().any(|p| p.name == k) && !typed.contains_key(k) {
            typed.insert(k.clone(), v.clone());
        }
    }
    let (from, to) = match over {
        Some(w) => (crate::time::parse_timestamp(&w.from), crate::time::parse_timestamp(&w.to)),
        None => (None, None),
    };
    let kept = |t: i64| keep(t) && from.is_none_or(|f| t >= f) && to.is_none_or(|e| t <= e);
    let (tables, runs) = match tables(ctx, input, test.as_ref(), &typed, &kept) {
        Ok(x) => x,
        Err(e) => {
            j.error = Some(e.to_string());
            return j;
        }
    };
    j.runs = runs;
    j.window = tables.first().and_then(|t| t.time.as_ref()).and_then(|t| Some((*t.iter().min()?, *t.iter().max()?)));
    if tables.iter().all(|t| t.is_empty()) {
        j.error = Some("the input is empty over the window the hold-out leaves".into());
        return j;
    }
    match test.run(&tables, &typed, seed(ctx.plan_hash, canonical)) {
        Ok(r) => j.result = Some(r),
        Err(e) => j.error = Some(e.to_string()),
    }
    j.trial_id = abt_wire::Hash::of(
        serde_json::json!({"plan": ctx.plan_hash, "name": canonical, "in_sample": in_sample, "runs": j.runs, "window": j.window})
            .to_string()
            .as_bytes(),
    )
    .hex();
    j
}

/// The plan's claims of one phase judged, their p-values adjusted over
/// them together.
pub fn judge(ctx: &Context, claims: &[(&Claim, String)], keep: Keep, in_sample: bool) -> Vec<Judged> {
    let mut out: Vec<Judged> = claims
        .iter()
        .map(|(c, canonical)| {
            let mut j = apply(ctx, &c.name, canonical, "claim", &c.test, &c.input, &c.over, &c.params, keep, in_sample);
            j.phase = Some(c.phase);
            j.policy = Some(c.policy);
            j.level = c.level;
            j.expect = Some(c.expect.render());
            j
        })
        .collect();
    let adjusted = benjamini_hochberg(&out.iter().map(|j| j.result.as_ref().and_then(|r| r.p_value)).collect::<Vec<_>>());
    for ((j, (c, _)), p) in out.iter_mut().zip(claims).zip(adjusted) {
        j.p_adjusted = p;
        j.verdict = Some(match (&j.result, &j.error) {
            (Some(r), _) => verdict(c, r, p),
            (None, Some(e)) => Verdict::Undecidable(e.clone()),
            (None, None) => Verdict::Undecidable("no result".into()),
        });
    }
    out
}

/// The parameters as written, by name (for the rows).
pub fn params_json(p: &Point) -> String {
    serde_json::to_string(&p.iter().cloned().collect::<BTreeMap<String, String>>()).unwrap_or_default()
}
