//! The query layer's commands (Q-9). `abt metrics --run ID`: a run's
//! metrics, computed from the tables the kernel wrote to the store (Q-3.1).

use std::collections::BTreeMap;

use abt_wire::transport::Connection;

use crate::data::Data;
use crate::metrics;
use crate::run::Run;

/// A run's metrics as one record (`--json`), with the versions of the
/// tables they were computed from (Q-3.1: every metric is recorded with its
/// tables' versions).
#[derive(Clone, Debug, serde::Serialize)]
pub struct RunMetrics {
    pub run: String,
    pub periods_per_year: f64,
    pub returns: metrics::ReturnMetrics,
    pub trading: metrics::TradingMetrics,
    pub convention: metrics::ConventionMetrics,
    /// The NAV at which the annual impact drag reaches 5 %, when the run
    /// paid impact.
    pub capacity: Option<f64>,
    pub subperiods: Vec<(i64, f64)>,
    pub versions: BTreeMap<String, String>,
}

/// The metrics of `run`.
pub fn run_metrics(data: &Data, run: &str) -> Result<RunMetrics, crate::data::Error> {
    let r = Run::read(data, run)?;
    let ppy = r.periods_per_year();
    let (by_day, days) = r.periods();
    Ok(RunMetrics {
        run: run.to_string(),
        periods_per_year: ppy,
        returns: metrics::return_metrics(&r.metric_curve(), ppy),
        trading: metrics::trading_metrics(&r, ppy),
        convention: metrics::convention_metrics_on(&r, ppy, by_day, days.as_ref()),
        capacity: metrics::capacity(&r, ppy, 0.05),
        subperiods: metrics::subperiod_sharpes(&r.metric_curve(), ppy, 20),
        versions: data.versions(&format!("kernel.{}.", run))?,
    })
}

fn usage() -> i32 {
    eprintln!("usage:\n  abt metrics --run RUN (--data SOCKET | --store DIR) [--json]   (a run's metrics, from the tables it wrote to the store: returns, trading, the report block's periods, capacity, sub-period Sharpe ratios)");
    2
}

/// `abt metrics` over a connection to the data service; the exit code.
pub fn metrics_main(args: &[String], conn: Connection) -> i32 {
    let mut run = None;
    let mut json = false;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--run" => {
                run = args.get(i + 1).cloned();
                i += 1;
            }
            "--json" => json = true,
            "--data" | "--store" => i += 1,
            _ => return usage(),
        }
        i += 1;
    }
    let Some(run) = run else { return usage() };
    let data = match Data::connect(conn) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("{}", e);
            return 2;
        }
    };
    let m = match run_metrics(&data, &run) {
        Ok(m) => m,
        Err(e) => {
            eprintln!("{}", e);
            return 1;
        }
    };
    if json {
        println!("{}", serde_json::to_string(&m).unwrap_or_default());
        return 0;
    }
    let r = &m.returns;
    println!("run {}", m.run);
    println!(
        "returns ({} a year, {} returns): total {:.6}   cagr {:.6}   volatility {:.6}   sharpe {:.4} (Lo {:.4}, se {:.4})   sortino {:.4}   max_drawdown {:.6} over {} bars   cvar_5 {:.6}   skew {:.4}   kurtosis {:.4}   worst_month {:.6}   t {:.4}   rho_1 {:.4}",
        m.periods_per_year, r.n, r.total_return, r.cagr, r.volatility, r.sharpe, r.sharpe_lo, r.sharpe_se, r.sortino, r.max_drawdown, r.max_drawdown_duration, r.cvar_5, r.skew, r.kurtosis, r.worst_month, r.t_stat, r.autocorrelation_1
    );
    let t = &m.trading;
    println!(
        "trading: decisions {}   fills {}   dropped {}   turnover {:.4}   fill_ratio {:.4}   participation {:.4} (max {:.4})   gross {:.4}   net {:.4}   max_leverage {:.4}   cost_drag {:.6}   impact_drag {:.6}",
        t.decisions, t.fills, t.dropped, t.turnover, t.fill_ratio, t.avg_participation, t.max_participation, t.avg_gross, t.avg_net, t.max_leverage, t.cost_drag, t.impact_drag
    );
    let c = &m.convention;
    println!(
        "periods ({} a year, {} periods): cagr {:.6}   max_drawdown {:.6}   sharpe {:.4} (population {:.4})   annual_return {:.6}   max_drawdown_additive {:.6}   total_pnl {:.2}   profit_factor {:.3} per period, {:.3} per trade   trades {}   win_rate {:.3}",
        c.periods_per_year, c.periods, c.cagr, c.max_drawdown, c.sharpe, c.sharpe_population, c.annual_return, c.max_drawdown_additive, c.total_pnl, c.profit_factor, c.profit_factor_trades, c.trades, c.win_rate
    );
    match m.capacity {
        Some(x) => println!("capacity at 5% impact drag: {:.0}", x),
        None => println!("capacity: unmeasured (the run paid no impact)"),
    }
    for (y, s) in &m.subperiods {
        println!("sharpe {}: {:.4}", y, s);
    }
    for (rel, v) in &m.versions {
        println!("read {} at {}", rel, v);
    }
    0
}
