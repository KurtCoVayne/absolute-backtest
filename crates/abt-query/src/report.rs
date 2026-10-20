//! Reports (Q-8): a plan's execution as tables under
//! `query.<plan>.report.*` and a text rendering. The tables are the runs'
//! metrics (`summary`), the lineage's trials and the deflated Sharpe ratio's
//! inputs (`trials`), every claim with its verdicts and raw and adjusted
//! p-values (`claims`), the test steps (`tests`), the kernel's contract
//! statuses over the runs (`contract`), every warning with its bias
//! (`warnings`), the forty biases with the layer that owns each and what
//! its status rests on (`biases`), the profit of every run by entity
//! (`contribution`, E-95), and the grid's `surface`, the walk-forward's
//! `folds`, the `subperiods` (the best run's Sharpe ratio, net profit and
//! trades per calendar year), and the feed and activation steps' tables.

use std::collections::BTreeMap;

use crate::data::{Data, Error};
use crate::plan::claims::Judged;
use crate::plan::exec::Execution;
use crate::plan::ReportSpec;
use crate::relation;
use crate::sheet::{Cell, Sheet};

/// One of the forty biases (B-4 to B-6 of the data-bundle document; Q-8):
/// the layer that owns it, its status and what the status rests on.
#[derive(Clone, Copy, Debug, PartialEq, serde::Serialize)]
pub struct Bias {
    pub name: &'static str,
    pub owner: &'static str,
    pub status: &'static str,
    pub source: &'static str,
}

const fn b(name: &'static str, owner: &'static str, status: &'static str, source: &'static str) -> Bias {
    Bias { name, owner, status, source }
}

/// The forty biases re-owned by layer (Q-8, E-23).
pub const BIASES: [Bias; 40] = [
    b("look-ahead", "kernel", "guaranteed", "the checker's causality rules and the fold's barrier order (K-4.2)"),
    b(
        "point-in-time",
        "data",
        "data-guaranteed",
        "every fact carries its receipt time; a decision sees what was received by its barrier (D-5.3)",
    ),
    b("historical revision", "data", "data-guaranteed", "revisions are kept and a read returns the latest admitted (D-3.3)"),
    b("corporate-action", "kernel", "modeled", "splits and dividends applied to the book (`actions`)"),
    b("survivorship", "query", "measured", "the `survivorship` feed statistic (Q-6)"),
    b("universe leakage", "program", "warned", "W9: a universe read from an instruments table"),
    b(
        "feature leakage",
        "query",
        "guaranteed",
        "a trailing hold-out truncates the stream (Proposition Q-2.1); claims read the in-sample bars",
    ),
    b(
        "label leakage",
        "query",
        "guaranteed",
        "the embargo by truncation, and forward returns read only for the bars kept (Q-2.3)",
    ),
    b("delisting", "kernel", "modeled", "the delisting haircut or the last price (`delisting`)"),
    b("selection", "query", "measured", "the `survivorship` and `membership` feed statistics (Q-6)"),
    b("regime", "query", "measured", "the Sharpe ratio per calendar year and the rules' drift (Q-3.1, Q-7)"),
    b("transaction-cost neglect", "kernel", "modeled", "commissions and fees (`commission`)"),
    b("slippage", "kernel", "modeled", "fixed and volatility-scaled slippage (`slippage`)"),
    b("market-impact", "kernel", "modeled", "square-root impact over average daily volume (`impact`)"),
    b("liquidity", "kernel", "modeled", "the participation cap (`participation`)"),
    b("bid-ask spread", "kernel", "modeled", "the spread from quotes when bound, else the slippage model"),
    b("execution timing", "kernel", "guaranteed", "fills at the price the `orders` field names, after the decision (K-5.8)"),
    b("bar-resolution", "kernel", "warned", "the checker's warnings on what a bar cannot tell"),
    b("partial-fill", "kernel", "modeled", "fills capped by participation, remainders expire or re-issue"),
    b("latency", "data", "measured", "the `latency` feed statistic and the kernel's late rule (Q-6, K-4.4)"),
    b("rebalancing", "kernel", "modeled", "targets re-issued until reached (`orders`)"),
    b("cash-management", "kernel", "modeled", "cash and debit financing (`financing`)"),
    b("leverage", "kernel", "modeled", "the gross limit and the margin call (`leverage`)"),
    b("funding-cost neglect", "kernel", "modeled", "debit, short and borrow rates (`financing`)"),
    b("shorting", "kernel", "modeled", "borrow buckets by average daily volume"),
    b("borrow-availability", "kernel", "modeled", "the shortable flag of a borrow bucket"),
    b("currency", "kernel", "warned", "one currency per book; a foreign price is warned (E-23)"),
    b("portfolio-construction", "kernel", "modeled", "target weights sized against the book's base"),
    b("data-snooping", "query", "measured", "every run is a counted trial of its lineage (Proposition Q-2.2)"),
    b(
        "multiple-testing",
        "query",
        "measured",
        "Benjamini–Hochberg over the claims, the deflated Sharpe over the lineage (Q-4.3)",
    ),
    b("p-hacking", "query", "measured", "every test and claim is a counted trial (Q-2.5)"),
    b("false statistical significance", "query", "measured", "the tests' declared dependence treatments (Q-4.2)"),
    b("overfitting", "query", "measured", "PBO and the deflated Sharpe ratio (Q-3.1)"),
    b("parameter over-optimization", "query", "measured", "PBO and the parameter surface (Q-3.1)"),
    b(
        "out-of-sample",
        "query",
        "guaranteed",
        "the hold-out, enforced by truncation and lifted only by a counted reveal (Q-2.3)",
    ),
    b("walk-forward", "query", "measured", "the walk-forward efficiency (Q-3.1)"),
    b("autocorrelation", "query", "measured", "Lo's Sharpe correction and Newey–West (Q-3.1, Q-4.2)"),
    b("non-stationarity", "query", "measured", "sub-period stability and the stationarity tests (Q-3.1, Q-4.4)"),
    b("short-sample", "query", "measured", "the minimum track-record length (Q-3.1)"),
    b("tail-risk neglect", "query", "measured", "CVaR, kurtosis and the `returns` feed statistic (Q-3.1, Q-6)"),
];

/// The metrics a summary shows when the report block names none.
pub const DEFAULT_METRICS: [&str; 7] = ["sharpe", "cagr", "max_drawdown", "volatility", "sortino", "calmar", "turnover"];

/// A report's tables, in order.
#[derive(Clone, Debug, serde::Serialize)]
pub struct Report {
    pub tables: Vec<Sheet>,
}

impl Report {
    pub fn table(&self, name: &str) -> Option<&Sheet> {
        self.tables.iter().find(|t| t.name == name)
    }

    /// The tables as text, those the report block names when it names any.
    pub fn text(&self, spec: &ReportSpec) -> String {
        self.tables
            .iter()
            .filter(|t| spec.tables.is_empty() || spec.tables.iter().any(|n| t.name == *n || t.name.starts_with(&format!("{}.", n))) || t.name == "summary")
            .map(|t| t.text())
            .collect::<Vec<_>>()
            .join("\n")
    }
}

fn point_text(p: &crate::plan::Point) -> String {
    p.iter().map(|(k, v)| format!("{}={}", k, v)).collect::<Vec<_>>().join(" ")
}

fn judged_rows(name: &str, js: &[&Judged]) -> Sheet {
    let mut s = Sheet::new(
        name,
        &[
            "name",
            "phase",
            "policy",
            "test",
            "input",
            "params",
            "statistic",
            "p_value",
            "p_adjusted",
            "level",
            "effect",
            "interval_lo",
            "interval_hi",
            "n",
            "dependence",
            "expect",
            "verdict",
            "in_sample",
            "notes",
        ],
    );
    for j in js {
        let r = j.result.as_ref();
        s.push(vec![
            Cell::text(j.name.clone()),
            j.phase.map(|p| Cell::text(format!("{:?}", p).to_lowercase())).unwrap_or(Cell::Null),
            j.policy.map(|p| Cell::text(format!("{:?}", p).to_lowercase())).unwrap_or(Cell::Null),
            Cell::text(format!("{}@{}", j.test, j.test_version)),
            Cell::text(j.input.clone()),
            Cell::text(point_text(&j.params)),
            r.map(|r| Cell::num(r.statistic)).unwrap_or(Cell::Null),
            r.and_then(|r| r.p_value).map(Cell::num).unwrap_or(Cell::Null),
            j.p_adjusted.map(Cell::num).unwrap_or(Cell::Null),
            j.level.map(Cell::num).unwrap_or(Cell::Null),
            r.and_then(|r| r.effect).map(Cell::num).unwrap_or(Cell::Null),
            r.and_then(|r| r.interval).map(|i| Cell::num(i.0)).unwrap_or(Cell::Null),
            r.and_then(|r| r.interval).map(|i| Cell::num(i.1)).unwrap_or(Cell::Null),
            r.map(|r| Cell::Int(r.n as i64)).unwrap_or(Cell::Null),
            Cell::text(j.dependence.clone()),
            j.expect.clone().map(Cell::text).unwrap_or(Cell::Null),
            j.verdict.as_ref().map(|v| Cell::text(v.text())).unwrap_or(Cell::Null),
            Cell::Bool(j.in_sample),
            Cell::text(r.map(|r| r.notes.join("; ")).unwrap_or_else(|| j.error.clone().unwrap_or_default())),
        ]);
    }
    s
}

/// The report of an execution.
pub fn build(data: &Data, ex: &Execution, spec: &ReportSpec) -> Result<Report, Error> {
    let mut tables = Vec::new();
    // summary
    let metrics: Vec<String> = if spec.metrics.is_empty() {
        DEFAULT_METRICS.iter().map(|m| m.to_string()).collect()
    } else {
        spec.metrics.clone()
    };
    let mut cols: Vec<&str> = vec!["step", "run", "point", "fold", "kind", "status"];
    cols.extend(metrics.iter().map(String::as_str));
    let mut summary = Sheet::new("summary", &cols);
    for o in &ex.outcomes {
        let mut row = vec![
            Cell::text(o.step.clone()),
            Cell::text(o.run_id.clone()),
            Cell::text(point_text(&o.point)),
            o.fold.as_ref().map(|f| Cell::text(format!("{}/{} {}", f.k, f.of, f.phase))).unwrap_or(Cell::Null),
            Cell::text(o.kind.name()),
            Cell::text(o.status.clone()),
        ];
        for m in &metrics {
            row.push(crate::metrics::objective_value(m, &o.returns, &o.trading, &o.trades).map(Cell::num).unwrap_or(Cell::Null));
        }
        summary.push(row);
    }
    tables.push(summary);
    // contribution: per run and entity, where the profit came from (E-95),
    // entities named by their ticker where the store has one.
    let tickers: BTreeMap<u64, String> = match relation::read(data, "ticker")? {
        Some(b) => {
            let (ids, symbols) = (relation::ints(&b, "id")?, relation::texts(&b, "symbol")?);
            ids.into_iter().zip(symbols).filter_map(|(i, s)| Some((i? as u64, s?))).collect()
        }
        None => BTreeMap::new(),
    };
    let mut contribution = Sheet::new("contribution", &["step", "run", "entity", "net_profit", "share", "trades", "hit_rate"]);
    for o in &ex.outcomes {
        for c in &o.trades.by_entity {
            let entity = match c.entity {
                Some(id) => tickers.get(&id).cloned().unwrap_or_else(|| id.to_string()),
                None => "-".to_string(),
            };
            contribution.push(vec![
                Cell::text(o.step.clone()),
                Cell::text(o.run_id.clone()),
                Cell::text(entity),
                Cell::num(c.net_profit),
                Cell::num(c.share),
                Cell::Int(c.trades as i64),
                Cell::num(c.hit_rate),
            ]);
        }
    }
    tables.push(contribution);
    // trials
    let mut trials = Sheet::new("trials", &["what", "value"]);
    trials.push(vec![Cell::text("lineage"), Cell::text(ex.lineage.clone())]);
    trials.push(vec![Cell::text("trials"), Cell::Int(ex.trials as i64)]);
    for (k, n) in &ex.trial_kinds {
        trials.push(vec![Cell::text(format!("of kind {}", k)), Cell::Int(*n as i64)]);
    }
    trials.push(vec![Cell::text("variance of the trials' Sharpe ratios"), Cell::num(ex.var_trials)]);
    if let (Some(d), Some(m)) = (&ex.dsr, ex.main()) {
        trials.push(vec![Cell::text("main result"), Cell::text(format!("{} {}", m.step, point_text(&m.point)))]);
        trials.push(vec![Cell::text("per-period Sharpe ratio"), Cell::num(m.returns.sharpe_period)]);
        trials.push(vec![Cell::text("observations"), Cell::Int(m.returns.n as i64)]);
        trials.push(vec![Cell::text("expected maximum per-period Sharpe"), Cell::num(d.expected_max_sr)]);
        trials.push(vec![Cell::text("deflated Sharpe ratio"), Cell::num(d.dsr)]);
    }
    tables.push(trials);
    // claims and tests
    tables.push(judged_rows("claims", &ex.claims.iter().collect::<Vec<_>>()));
    tables.push(judged_rows("tests", &ex.tests.iter().collect::<Vec<_>>()));
    // contract: the kernel's statuses over the runs
    let mut statuses: BTreeMap<(String, String, String), usize> = BTreeMap::new();
    for o in ex.outcomes.iter().filter(|o| !o.run_id.is_empty()) {
        let Some(b) = relation::read(data, &format!("kernel.{}.contract", o.run_id))? else { continue };
        let (clause, kind, status) = (relation::texts(&b, "clause")?, relation::texts(&b, "kind")?, relation::texts(&b, "status")?);
        for i in 0..b.num_rows() {
            *statuses
                .entry((clause[i].clone().unwrap_or_default(), kind[i].clone().unwrap_or_default(), status[i].clone().unwrap_or_default()))
                .or_default() += 1;
        }
    }
    let mut contract = Sheet::new("contract", &["clause", "kind", "status", "runs"]);
    for ((c, k, s), n) in statuses {
        contract.push(vec![Cell::text(c), Cell::text(k), Cell::text(s), Cell::Int(n as i64)]);
    }
    tables.push(contract);
    // warnings
    let mut counted: BTreeMap<(String, String), usize> = BTreeMap::new();
    for w in &ex.warnings {
        *counted.entry((w.bias.to_lowercase(), w.message.clone())).or_default() += 1;
    }
    let mut warnings = Sheet::new("warnings", &["bias", "message", "count"]);
    for ((bias, m), n) in &counted {
        warnings.push(vec![Cell::text(bias.clone()), Cell::text(m.clone()), Cell::Int(*n as i64)]);
    }
    tables.push(warnings);
    // biases
    let mut biases = Sheet::new("biases", &["bias", "owner", "status", "source", "touched"]);
    for bias in BIASES {
        let touched = counted.keys().any(|(b, _)| b == bias.name);
        let status = if bias.name == "out-of-sample" && ex.holdout == "none" { "warned" } else { bias.status };
        biases.push(vec![Cell::text(bias.name), Cell::text(bias.owner), Cell::text(status), Cell::text(bias.source), Cell::Bool(touched)]);
    }
    tables.push(biases);
    // surface, folds, sub-periods
    let mut surface = Sheet::new("surface", &["grid", "best", "best_value", "neighbours", "stability", "tolerance", "pbo"]);
    // The sub-periods keep their columns and gain the best run's net profit
    // and trades per calendar year (E-95).
    let mut subperiods = Sheet::new("subperiods", &["grid", "year", "sharpe", "net_profit", "trades"]);
    for g in &ex.grids {
        let best = &ex.outcomes[g.runs[g.best]];
        surface.push(vec![
            Cell::text(g.step.clone()),
            Cell::text(point_text(&best.point)),
            Cell::num(best.objective),
            g.surface.as_ref().map(|s| Cell::Int(s.neighbours as i64)).unwrap_or(Cell::Null),
            g.surface.as_ref().map(|s| Cell::num(s.stability)).unwrap_or(Cell::Null),
            g.surface.as_ref().map(|s| Cell::num(s.tolerance)).unwrap_or(Cell::Null),
            g.pbo.as_ref().map(|p| Cell::num(p.pbo)).unwrap_or(Cell::Null),
        ]);
        for (y, s) in &g.subperiods {
            let year = best.trades.by_year.iter().find(|r| r.year == *y);
            subperiods.push(vec![
                Cell::text(g.step.clone()),
                Cell::Int(*y),
                Cell::num(*s),
                year.map(|r| Cell::num(r.net_profit)).unwrap_or(Cell::Null),
                year.map(|r| Cell::Int(r.trades as i64)).unwrap_or(Cell::Null),
            ]);
        }
    }
    tables.push(surface);
    let mut folds = Sheet::new(
        "folds",
        &[
            "walk_forward",
            "fold",
            "train_from",
            "train_to",
            "test_from",
            "test_to",
            "best_point",
            "in_sample",
            "out_of_sample",
            "in_sample_cagr",
            "out_of_sample_cagr",
        ],
    );
    for w in &ex.walk_forwards {
        for (k, f) in w.folds.iter().enumerate() {
            folds.push(vec![
                Cell::text(w.step.clone()),
                Cell::Int(k as i64 + 1),
                Cell::Time(f.train.0),
                Cell::Time(f.train.1),
                Cell::Time(f.test.0),
                Cell::Time(f.test.1),
                Cell::Int(f.best as i64),
                Cell::num(f.in_sample),
                Cell::num(f.out_of_sample),
                Cell::num(f.in_sample_cagr),
                Cell::num(f.out_of_sample_cagr),
            ]);
        }
        folds.push(vec![
            Cell::text(w.step.clone()),
            Cell::Null,
            Cell::Null,
            Cell::Null,
            Cell::Null,
            Cell::Null,
            Cell::text("efficiency"),
            Cell::Null,
            Cell::Null,
            w.efficiency.map(Cell::num).unwrap_or(Cell::Null),
            Cell::num(w.out_of_sample.cagr),
        ]);
    }
    tables.push(folds);
    tables.push(subperiods);
    for s in &ex.sheets {
        tables.push(s.clone());
    }
    Ok(Report { tables })
}

/// The report's tables written whole under `query.<plan>.report.*`, and
/// its text as `report.text` when the report block asks for it.
pub fn write(data: &Data, ex: &Execution, report: &Report, spec: &ReportSpec) -> Result<Vec<String>, Error> {
    let mut written = Vec::new();
    for t in &report.tables {
        let name = crate::plan::trials::relation_of(&ex.hash, &format!("report.{}", t.name));
        let (schema, batch) = t.relation(&name, ex.at)?;
        data.put(schema, &batch, true)?;
        written.push(name);
    }
    if spec.text {
        let mut text = Sheet::new("text", &["text"]);
        text.push(vec![Cell::text(report.text(spec))]);
        let name = crate::plan::trials::relation_of(&ex.hash, "report.text");
        let (schema, batch) = text.relation(&name, ex.at)?;
        data.put(schema, &batch, true)?;
        written.push(name);
    }
    Ok(written)
}
