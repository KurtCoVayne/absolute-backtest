//! The metrics library (data-bundle doc, section 7): return metrics with
//! their conventions fixed by hand-computed values, the Bailey–López de
//! Prado statistics against worked numbers, PBO on constructed grids, the
//! trading metrics of a run, and the DSL metrics over `nav` agreeing with
//! the kernel's own.

mod corpus;

use absolute_backtest::check::{check_program, Program};
use absolute_backtest::data::synthetic_daily;
use absolute_backtest::kernel::time::parse_timestamp;
use absolute_backtest::kernel::{run, ExecConfig, Kernel, Value};
use absolute_backtest::study::{block_bootstrap_sharpe, capacity, deflated_sharpe, min_track_record_length, newey_west_t, pbo_cscv, probabilistic_sharpe, return_metrics, trading_metrics, Returns};

fn program(src: &str, name: &str) -> Program {
    let mut ws = corpus::base_workspace();
    ws.add_source(src).unwrap();
    let (p, diags) = check_program(&ws, name);
    let text: Vec<String> = diags.iter().map(|d| d.to_string()).collect();
    p.unwrap_or_else(|| panic!("{} should check:\n{}", name, text.join("\n")))
}

fn close(a: f64, b: f64, tol: f64) -> bool {
    (a - b).abs() <= tol * b.abs().max(1.0)
}

fn curve() -> Vec<(i64, f64)> {
    let dates = ["2024-01-29", "2024-01-30", "2024-01-31", "2024-02-01", "2024-02-02", "2024-02-05", "2024-02-06", "2024-02-07"];
    let equity = [100.0, 101.0, 99.0, 102.0, 102.0, 100.0, 103.0, 104.0];
    dates.iter().zip(equity).map(|(d, e)| (parse_timestamp(d).unwrap(), e)).collect()
}

#[test]
fn return_metrics_follow_the_stated_conventions() {
    let m = return_metrics(&curve(), 252.0);
    assert_eq!(m.n, 7);
    assert!(close(m.total_return, 0.04, 1e-12));
    assert!(close(m.cagr, 3.10393255398042, 1e-9), "{}", m.cagr);
    assert!(close(m.volatility, 0.32749335495759396, 1e-9), "{}", m.volatility);
    assert!(close(m.sharpe_period, 0.28115549428782777, 1e-9));
    assert!(close(m.sharpe, 4.4632051057502, 1e-9), "{}", m.sharpe);
    assert!(close(m.sortino, 8.741849544535615, 1e-9), "{}", m.sortino);
    assert!(close(m.max_drawdown, 0.01980198019801982, 1e-12));
    assert_eq!(m.max_drawdown_duration, 1);
    // Seven observations: the worst 5 % is the single worst return.
    assert!(close(m.cvar_5, 0.01980198019801982, 1e-12));
    assert!(close(m.skew, -0.08944848680934106, 1e-9), "{}", m.skew);
    assert!(close(m.kurtosis, -1.334514079101957, 1e-9), "{}", m.kurtosis);
    // January holds one return (+1 %), February compounds the other six.
    assert!(close(m.worst_month, -0.01, 1e-12), "{}", m.worst_month);
    assert!(close(m.autocorrelation_1, -0.4894558526824186, 1e-9));
    // Two Bartlett lags for seven observations.
    assert!(close(m.t_stat, 1.8534432370342788, 1e-9), "{}", m.t_stat);
    // The alternation is so strong that Lo's truncated sum is not positive
    // definite over 252 periods; the ratio then falls back to the iid one.
    assert!(close(m.sharpe_lo, m.sharpe, 1e-12));
    assert!(close(m.sharpe_se, 6.117423592937487, 1e-9), "{}", m.sharpe_se);
}

#[test]
fn lo_adjustment_lowers_a_positively_autocorrelated_sharpe() {
    let mut e = 100.0;
    let mut c = vec![(0i64, e)];
    for i in 0..400 {
        let r = 0.004 + 0.01 * ((i as f64) / 6.0).sin();
        e *= 1.0 + r;
        c.push((i as i64 + 1, e));
    }
    let m = return_metrics(&c, 252.0);
    assert!(m.autocorrelation_1 > 0.5, "{}", m.autocorrelation_1);
    assert!(m.sharpe_lo < m.sharpe, "lo {} iid {}", m.sharpe_lo, m.sharpe);
    let iid_se = ((1.0 + m.sharpe_period * m.sharpe_period / 2.0) / m.n as f64).sqrt() * 252f64.sqrt();
    assert!(m.sharpe_se < iid_se);
    assert!(m.max_drawdown_duration > 5);
}

#[test]
fn newey_west_degenerates_to_the_plain_t_statistic_at_zero_lags() {
    let x = [0.01, -0.02, 0.03, 0.0, 0.01];
    let n = x.len() as f64;
    let mu = x.iter().sum::<f64>() / n;
    let var = x.iter().map(|v| (v - mu).powi(2)).sum::<f64>() / n;
    assert!(close(newey_west_t(&x, Some(0)), mu / (var / n).sqrt(), 1e-12));
}

#[test]
fn probabilistic_sharpe_and_min_track_record_length_agree_with_the_paper() {
    // SR 0.1 per period over 100 observations, skew -0.5, excess kurtosis 2.
    let psr = probabilistic_sharpe(0.1, 0.0, 100, -0.5, 2.0);
    assert!(close(psr, 0.8330822773316748, 1e-9), "{}", psr);
    let n = min_track_record_length(0.1, 0.0, -0.5, 2.0, 0.95);
    assert!(close(n, 287.7876061341135, 1e-6), "{}", n);
    // PSR reaches the confidence exactly at that length.
    assert!(probabilistic_sharpe(0.1, 0.0, 288, -0.5, 2.0) >= 0.95);
    assert!(probabilistic_sharpe(0.1, 0.0, 287, -0.5, 2.0) < 0.95);
    assert!(min_track_record_length(0.0, 0.0, 0.0, 0.0, 0.95).is_infinite());
}

#[test]
fn the_deflated_sharpe_ratio_uses_the_expected_maximum_of_the_trials() {
    let one = deflated_sharpe(0.1, 100, -0.5, 2.0, 1, 0.5);
    assert_eq!(one.expected_max_sr, 0.0);
    assert!(close(one.dsr, probabilistic_sharpe(0.1, 0.0, 100, -0.5, 2.0), 1e-12));
    for (trials, var, want) in [(2usize, 1.0, 0.5197553442805939), (10, 0.04, 0.31491966026915), (100, 0.0025, 0.12653014466008425)] {
        let d = deflated_sharpe(0.1, 100, -0.5, 2.0, trials, var);
        assert!(close(d.expected_max_sr, want, 1e-8), "{} trials: {}", trials, d.expected_max_sr);
        assert!(d.dsr < one.dsr);
    }
}

#[test]
fn pbo_is_zero_when_one_trial_dominates_and_high_when_the_winner_rotates() {
    // Trial 0 earns steadily; the others are flat noise.
    let rows: Vec<Vec<f64>> = (0..64)
        .map(|i| {
            let noise = if i % 2 == 0 { 0.001 } else { -0.001 };
            vec![0.01 + noise, noise, -noise, if i % 3 == 0 { 0.002 } else { -0.001 }]
        })
        .collect();
    let p = pbo_cscv(&rows, 8).unwrap();
    assert_eq!(p.combinations, 70);
    assert_eq!(p.pbo, 0.0, "{:?}", p);
    // Each block has its own winner, which is a loser in every other block.
    let trials = 8;
    let rows: Vec<Vec<f64>> = (0..64)
        .map(|i| {
            let block = i / 8;
            (0..trials)
                .map(|j| if j == block { 0.02 + 0.001 * (i % 2) as f64 } else { -0.003 + 0.001 * ((i + j) % 2) as f64 })
                .collect()
        })
        .collect();
    let p = pbo_cscv(&rows, 8).unwrap();
    assert!(p.pbo >= 0.5, "{:?}", p);
    assert!(pbo_cscv(&rows[..10], 8).is_none());
}

#[test]
fn the_block_bootstrap_is_seeded_and_brackets_the_estimate() {
    let c = curve();
    let r = Returns::from_curve(&c, 252.0);
    let m = return_metrics(&c, 252.0);
    let a = block_bootstrap_sharpe(&r.r, 252.0, Some(2), 500, 0.9, 7).unwrap();
    let b = block_bootstrap_sharpe(&r.r, 252.0, Some(2), 500, 0.9, 7).unwrap();
    assert_eq!(a, b);
    assert!(a.0 < m.sharpe && m.sharpe < a.1, "{:?} around {}", a, m.sharpe);
    assert!(block_bootstrap_sharpe(&r.r[..3], 252.0, None, 10, 0.9, 1).is_none());
}

#[test]
fn trading_metrics_and_capacity_read_the_run() {
    let src = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/corpus/strategies/momentum_top_n.dsl")).unwrap();
    let prog = program(&src, "momentum_top_n");
    let ds = synthetic_daily(&["AAA", "BBB", "CCC", "DDD", "SPY"], (2022, 1, 3), 320, 11);
    let r = run(&prog, &ds, ExecConfig::default()).unwrap();
    let tm = trading_metrics(&r, 252.0);
    let mean_equity = r.equity_curve.iter().map(|(_, e)| e).sum::<f64>() / r.equity_curve.len() as f64;
    let years = (r.equity_curve.len() - 1) as f64 / 252.0;
    assert!(close(tm.turnover, r.costs.turnover / mean_equity / years, 1e-12));
    assert!(tm.fills > 0 && tm.fills == r.fills.len());
    assert!(tm.avg_gross > 0.0 && tm.avg_gross <= 1.0 + 1e-9, "{}", tm.avg_gross);
    assert!(tm.cost_drag > tm.impact_drag && tm.impact_drag > 0.0);
    let cap = capacity(&r, 252.0, 0.01).unwrap();
    assert!(close(cap, r.equity_curve[0].1 * (0.01 / tm.impact_drag).powi(2), 1e-9));
    // Twice the tolerated drag admits four times the book.
    assert!(close(capacity(&r, 252.0, 0.02).unwrap(), 4.0 * cap, 1e-9));
    let r0 = run(&prog, &ds, ExecConfig::frictionless()).unwrap();
    assert!(capacity(&r0, 252.0, 0.01).is_none());
}

const GUARD: &str = r#"
strategy drawdown_guard {
  env equities_1d
  uses features
  uses metrics
  resolution @1d
  mode target
  param w : Scalar = 0.2
  param limit : Scalar = 0.03
  decide(T, target_weight(A, w)) :- universe(A, T), drawdown(T, D), D < limit.
  decide(T, target_weight(A, 0)) :- held(A, T, _), drawdown(T, D), D >= limit.
}
"#;

#[test]
fn the_dsl_metrics_over_nav_agree_with_the_kernel_metrics() {
    let prog = program(GUARD, "drawdown_guard");
    let ds = synthetic_daily(&["AAA", "BBB", "CCC", "DDD", "SPY"], (2022, 1, 3), 200, 3);
    let mut k = Kernel::new(&prog, &ds, ExecConfig::default()).unwrap();
    let r = k.run().unwrap();
    assert!(!r.fills.is_empty());
    let rets = Returns::from_curve(&r.equity_curve, 252.0);
    let mut peak = f64::NEG_INFINITY;
    let mut compared = 0;
    for (i, &(t, e)) in r.equity_curve.iter().enumerate() {
        peak = peak.max(e);
        let dd = k.query("drawdown", t, &[]).unwrap();
        assert_eq!(dd.len(), 1, "drawdown at bar {}", i);
        assert!(close(dd[0][1].as_f64().unwrap(), 1.0 - e / peak, 1e-12));
        let nav = k.query("nav", t, &[]).unwrap();
        assert_eq!(nav[0][1], Value::Num(e));
        if i > 0 {
            let nr = k.query("nav_ret", t, &[]).unwrap();
            assert!(close(nr[0][1].as_f64().unwrap(), rets.r[i - 1], 1e-12));
            compared += 1;
        }
    }
    assert_eq!(compared, rets.len());
    // The guard acted: some bar saw the drawdown cross the limit.
    assert!(r.decisions.iter().any(|d| d.decision.amount == 0.0), "the guard never fired");
    let v = k
        .query("nav_vol", *r.bars.last().unwrap(), &[Value::Dur(absolute_backtest::Duration { months: 0, days: 20 }), Value::Num(10.0)])
        .unwrap();
    assert_eq!(v.len(), 1);
}

/// Data-bundle doc, section 7: a fixed-base book is reported both ways.
/// Four weekly NAV changes on a base of 100: +10, -20, +5, +5.
#[test]
fn a_fixed_base_book_reports_compounded_and_additive_conventions() {
    use absolute_backtest::kernel::{FillRecord, RunResult};
    use absolute_backtest::study::metrics::{convention_metrics, period_returns};
    let day = 86_400;
    let curve = vec![(0, 100.0), (7 * day, 110.0), (14 * day, 90.0), (21 * day, 95.0), (28 * day, 100.0)];
    let fill = |t: i64, q: f64, p: f64| FillRecord {
        t,
        equity: 0,
        quantity: q,
        price: p,
        at_last_price: false,
        commission: 0.5,
        fee: 0.0,
        slippage: 0.0,
        impact: 0.0,
        participation: 0.0,
        partial: false,
        forced: false,
    };
    let r = RunResult {
        equity_curve: curve,
        base_capital: Some(100.0),
        // Two round trips of a future worth 2 a point: +1 x 2 - 1 and -3 x 2 - 1.
        fills: vec![fill(0, 1.0, 10.0), fill(day, -1.0, 11.0), fill(2 * day, 1.0, 10.0), fill(3 * day, -1.0, 7.0)],
        multipliers: [(0u32, 2.0)].into_iter().collect(),
        ..Default::default()
    };
    let x: Vec<f64> = period_returns(&r, false).iter().map(|(_, x, _)| *x).collect();
    assert_eq!(x, vec![0.1, -0.2, 0.05, 0.05]);
    let m = convention_metrics(&r, 52.0, false);
    let e: f64 = 1.1 * 0.8 * 1.05 * 1.05;
    assert!((m.cagr - (e.powf(52.0 / 4.0) - 1.0)).abs() < 1e-12, "{}", m.cagr);
    // Compounded drawdown 1 - 0.88/1.1; additive 0.2 from the 0.1 peak.
    assert!((m.max_drawdown - 0.2).abs() < 1e-12, "{}", m.max_drawdown);
    assert!((m.max_drawdown_additive - 0.2).abs() < 1e-12);
    let mu = 0.0;
    let pop = ((0.01 + 0.04 + 0.0025 + 0.0025) / 4.0f64).sqrt();
    assert!((m.sharpe_population - mu / pop * 52f64.sqrt()).abs() < 1e-12);
    assert_eq!(m.annual_return, 0.0);
    assert!((m.total_pnl - 0.0).abs() < 1e-12);
    assert!((m.profit_factor - 20.0 / 20.0).abs() < 1e-12);
    assert_eq!(m.trades, 2);
    assert!((m.profit_factor_trades - 1.0 / 7.0).abs() < 1e-12, "{}", m.profit_factor_trades);
    assert_eq!(m.win_rate, 0.5);
    // By day, each week is its own day.
    assert_eq!(period_returns(&r, true).len(), 4);
}
