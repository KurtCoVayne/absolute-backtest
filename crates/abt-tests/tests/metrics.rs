//! The metrics library (data-bundle doc, section 7; the query layer's,
//! Q-3.1): return metrics with their conventions fixed by hand-computed
//! values, the Bailey–López de Prado statistics against worked numbers, PBO
//! on constructed grids, the trading metrics of a run read back from the
//! store, and the `book` library's metrics over `nav` agreeing with the
//! query layer's.

use abt_tests as common;

use abt_kernel::data::synthetic_daily;
use abt_kernel::kernel::time::parse_timestamp;
use abt_kernel::kernel::{ExecConfig, Value};
use abt_query::metrics::{block_bootstrap_sharpe, capacity, deflated_sharpe, min_track_record_length, newey_west_t, pbo_cscv, probabilistic_sharpe, return_metrics, trading_metrics, Returns};
use abt_tests::stored_run;

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
    let prog = common::corpus_program("momentum_top_n");
    let ds = synthetic_daily(&["AAA", "BBB", "CCC", "DDD", "SPY"], (2022, 1, 3), 320, 11);
    let (r, stored) = stored_run(&prog, &ds, ExecConfig::default()).unwrap();
    let tm = trading_metrics(&stored, 252.0);
    let mean_equity = r.equity_curve.iter().map(|(_, e)| e).sum::<f64>() / r.equity_curve.len() as f64;
    let years = (r.equity_curve.len() - 1) as f64 / 252.0;
    assert!(close(tm.turnover, r.costs.turnover / mean_equity / years, 1e-12));
    assert!(tm.fills > 0 && tm.fills == r.fills.len());
    assert!(tm.avg_gross > 0.0 && tm.avg_gross <= 1.0 + 1e-9, "{}", tm.avg_gross);
    assert!(tm.cost_drag > tm.impact_drag && tm.impact_drag > 0.0);
    let cap = capacity(&stored, 252.0, 0.01).unwrap();
    assert!(close(cap, r.equity_curve[0].1 * (0.01 / tm.impact_drag).powi(2), 1e-9));
    // Twice the tolerated drag admits four times the book.
    assert!(close(capacity(&stored, 252.0, 0.02).unwrap(), 4.0 * cap, 1e-9));
    let (_, frictionless) = stored_run(&prog, &ds, ExecConfig::frictionless()).unwrap();
    assert!(capacity(&frictionless, 252.0, 0.01).is_none());
}

/// A fifth of the capital in every name while the book's drawdown is under
/// the limit, flat once it crosses it; the twenty-row NAV volatility is an
/// instance of the `book` library the strategy names.
fn guard() -> String {
    common::strategy(
        "drawdown_guard",
        "    w : Scalar = 0.2\n    limit : Scalar = 0.03",
        "    rel vol20(@T, V: Scalar)\n    vol20(T, V) :- nav_vol[20](T, V).",
        "    decide(T, hold(A, w)) :- universe(A, T), drawdown(T, D), D < limit.\n    decide(T, hold(A, 0)) :- held(A, T, _), drawdown(T, D), D >= limit.",
    )
}

#[test]
fn the_dsl_metrics_over_nav_agree_with_the_kernel_metrics() {
    let prog = common::program(&guard(), "drawdown_guard");
    let ds = synthetic_daily(&["AAA", "BBB", "CCC", "DDD", "SPY"], (2022, 1, 3), 200, 3);
    let (r, mut k) = abt_tests::run_kernel(&prog, &ds, ExecConfig::default()).unwrap();
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
            let nr = k.query("nav_return", t, &[]).unwrap();
            assert!(close(nr[0][1].as_f64().unwrap(), rets.r[i - 1], 1e-12));
            compared += 1;
        }
    }
    assert_eq!(compared, rets.len());
    // The guard acted: some bar saw the drawdown cross the limit.
    assert!(r.decisions.iter().any(|d| d.decision.amount == 0.0), "the guard never fired");
    let v = k.query("nav_vol[20]", *r.bars.last().unwrap(), &[]).unwrap();
    assert_eq!(v.len(), 1);
}

/// Data-bundle doc, section 7: a fixed-base book is reported both ways.
/// Four weekly NAV changes on a base of 100: +10, -20, +5, +5.
#[test]
fn a_fixed_base_book_reports_compounded_and_additive_conventions() {
    use abt_query::metrics::{convention_metrics, period_returns};
    use abt_query::run::{BookRow, Fill, Run};
    let day = abt_kernel::kernel::time::DAY;
    let curve = [(0, 100.0), (7 * day, 110.0), (14 * day, 90.0), (21 * day, 95.0), (28 * day, 100.0)];
    // A future worth 2 a point.
    let fill = |seq: u64, t: i64, q: f64, p: f64| Fill {
        seq,
        t,
        instrument: Some(0),
        quantity: q,
        price: p,
        commission: 0.5,
        fee: 0.0,
        slippage: 0.0,
        impact: 0.0,
        participation: 0.0,
        multiplier: 2.0,
    };
    let r = Run {
        id: "by hand".into(),
        book: curve
            .iter()
            .map(|&(t, nav)| BookRow {
                t,
                cash: nav,
                nav,
                gross: 0.0,
                net: 0.0,
                leverage: 0.0,
                receivables: 0.0,
                capital: 100.0,
                margin_initial: 0.0,
                margin_maintenance: 0.0,
                variation: 0.0,
            })
            .collect(),
        // Two round trips: +1 x 2 - 1 and -3 x 2 - 1.
        fills: vec![fill(1, 0, 1.0, 10.0), fill(2, day, -1.0, 11.0), fill(3, 2 * day, 1.0, 10.0), fill(4, 3 * day, -1.0, 7.0)],
        decisions: 4,
        dropped: 0,
        report: [("base_capital".to_string(), serde_json::json!(100.0))].into_iter().collect(),
        ..Run::default()
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

/// The rows the red flags read (Q-3.1, ERRATA E-95), each against a hand
/// computation over six bars on a fixed base of 100 across the turn of
/// 1990 and four round trips (a win, a loss, a win, a flat one) among
/// three securities, with one position left open.
#[test]
fn the_red_flag_rows_are_computed_as_stated_from_hand_built_fills() {
    use abt_query::metrics::{objective_value, skewness, trade_metrics, ReturnMetrics, TradingMetrics, METRIC_NAMES};
    use abt_query::run::{BookRow, Fill, Run};
    let t: Vec<i64> = ["1989-12-27", "1989-12-28", "1989-12-29", "1990-01-02", "1990-01-03", "1990-01-04"]
        .iter()
        .map(|d| parse_timestamp(d).unwrap())
        .collect();
    let nav = [100.0, 104.0, 101.0, 103.0, 108.0, 106.0];
    let fill = |seq: u64, t: i64, instrument: u64, q: f64, p: f64, commission: f64, multiplier: f64| Fill {
        seq,
        t,
        instrument: Some(instrument),
        quantity: q,
        price: p,
        commission,
        fee: 0.0,
        slippage: 0.0,
        impact: 0.0,
        participation: 0.0,
        multiplier,
    };
    let r = Run {
        id: "by hand".into(),
        book: t
            .iter()
            .zip(nav)
            .map(|(&t, nav)| BookRow {
                t,
                cash: nav,
                nav,
                gross: 0.0,
                net: 0.0,
                leverage: 0.0,
                receivables: 0.0,
                capital: 100.0,
                margin_initial: 0.0,
                margin_maintenance: 0.0,
                variation: 0.0,
            })
            .collect(),
        fills: vec![
            // A: a future worth 2 a point, bought at 10 and sold at 12, half
            // a unit of commission a side: 2 x 2 - 1 = +3, closed in 1989.
            fill(1, t[0], 0, 1.0, 10.0, 0.5, 2.0),
            fill(2, t[1], 0, -1.0, 12.0, 0.5, 2.0),
            // B: two shares bought at 5 and sold at 4: -2 - 0.5 = -2.5.
            fill(3, t[1], 1, 2.0, 5.0, 0.25, 1.0),
            fill(4, t[3], 1, -2.0, 4.0, 0.25, 1.0),
            // C: the future sold at 12 and covered at 11: 2 - 1 = +1.
            fill(5, t[3], 0, -1.0, 12.0, 0.5, 2.0),
            fill(6, t[4], 0, 1.0, 11.0, 0.5, 2.0),
            // D: bought and sold at 7 with no commission: exactly 0.
            fill(7, t[4], 2, 1.0, 7.0, 0.0, 1.0),
            fill(8, t[5], 2, -1.0, 7.0, 0.0, 1.0),
            // Still open at the end: no trade.
            fill(9, t[5], 1, 1.0, 4.0, 0.25, 1.0),
        ],
        decisions: 9,
        dropped: 0,
        report: [("base_capital".to_string(), serde_json::json!(100.0))].into_iter().collect(),
        ..Run::default()
    };
    let m = trade_metrics(&r, 252.0, &|_| true);
    // The bars' P&L: +4, -3, +2, +5, -2 = 6 over five bars on 100.
    assert_eq!(m.base, 100.0);
    assert!(close(m.net_profit, 6.0, 1e-12));
    assert!(close(m.annual_return, 6.0 / 5.0 / 100.0 * 252.0, 1e-12), "{}", m.annual_return);
    // The cumulative P&L 4, 1, 3, 8, 6 falls 3 from its peak of 4.
    assert!(close(m.max_drawdown_base, 0.03, 1e-12));
    assert!(close(m.mar, 3.024 / 0.03, 1e-9), "{}", m.mar);
    // 1989's two bars made 4 - 3 = 1 of the 6.
    assert!(close(m.profit_before_1990_share, 1.0 / 6.0, 1e-12));
    assert_eq!(m.by_year.iter().map(|y| (y.year, y.trades)).collect::<Vec<_>>(), vec![(1989, 1), (1990, 3)]);
    assert!(close(m.by_year[0].net_profit, 1.0, 1e-12) && close(m.by_year[1].net_profit, 5.0, 1e-12));
    // The trades: +3, -2.5, +1, 0.
    assert_eq!(m.trades, 4);
    assert!(close(m.profit_factor, 4.0 / 2.5, 1e-12));
    assert_eq!(m.hit_rate, 0.5, "a flat trade is no win");
    assert!(close(m.avg_win_loss, 2.0 / 2.5, 1e-12));
    assert!(close(m.avg_trade, 1.5 / 4.0, 1e-12));
    // Commissions 2.75 over 11 units filled, twice.
    assert!(close(m.round_turn_cost, 0.5, 1e-12), "{}", m.round_turn_cost);
    assert!(close(m.avg_trade_to_cost, 0.75, 1e-12));
    assert_eq!(m.largest_loss, 2.5);
    let pnls = [3.0, -2.5, 1.0, 0.0];
    let m3 = -1.37109375;
    let m2: f64 = 3.921875;
    assert!(close(m.trade_skew, m3 / m2.powf(1.5), 1e-12), "{}", m.trade_skew);
    assert!(close(m.trade_skew, skewness(&pnls), 1e-15));
    // By entity: the future +4 over two trades, the share -2.5, the flat
    // one 0; the net profit over every trade is 1.5.
    let by: Vec<(Option<u64>, f64, usize, f64)> = m.by_entity.iter().map(|c| (c.entity, c.net_profit, c.trades, c.hit_rate)).collect();
    assert_eq!(by, vec![(Some(0), 4.0, 2, 1.0), (Some(1), -2.5, 1, 0.0), (Some(2), 0.0, 1, 0.0)]);
    assert!(close(m.by_entity[0].share, 4.0 / 1.5, 1e-12));
    assert!(close(m.top_market_share, 4.0 / 1.5, 1e-12));
    assert!(close(m.profitable_market_share, 1.0 / 3.0, 1e-12));
    // Every row has its name, and `drawdown_duration` is the drawdown's.
    let (rm, tm) = (
        ReturnMetrics {
            max_drawdown_duration: 7,
            ..Default::default()
        },
        TradingMetrics::default(),
    );
    for name in [
        "return",
        "mar",
        "drawdown_duration",
        "trades",
        "profit_factor",
        "hit_rate",
        "avg_win_loss",
        "avg_trade",
        "avg_trade_to_cost",
        "largest_loss",
        "trade_skew",
        "top_market_share",
        "profitable_market_share",
        "profit_before_1990_share",
    ] {
        assert!(METRIC_NAMES.contains(&name), "{}", name);
        assert!(objective_value(name, &rm, &tm, &m).is_some(), "{}", name);
    }
    assert_eq!(objective_value("return", &rm, &tm, &m), Some(m.annual_return));
    assert_eq!(objective_value("drawdown_duration", &rm, &tm, &m), Some(7.0));
    assert_eq!(objective_value("trades", &rm, &tm, &m), Some(4.0));
    assert!(!abt_query::metrics::higher_is_better("largest_loss") && !abt_query::metrics::higher_is_better("top_market_share"));
    assert!(abt_query::metrics::higher_is_better("mar") && abt_query::metrics::higher_is_better("profit_factor"));
    // Kept bars only: 1990's three bars; the trades closing in 1990, B's
    // among them although it opened in 1989 (E-114: a window's round trips
    // are the run's that close in it; before, B's open was left out and B
    // never closed).
    let k = trade_metrics(&r, 252.0, &|t| t >= parse_timestamp("1990-01-01").unwrap());
    assert!(close(k.net_profit, 5.0, 1e-12));
    assert!(close(k.annual_return, 5.0 / 3.0 / 100.0 * 252.0, 1e-12));
    assert_eq!(k.trades, 3);
    assert_eq!(k.profit_before_1990_share, 0.0);
    assert_eq!(k.by_year.iter().map(|y| (y.year, y.trades)).collect::<Vec<_>>(), vec![(1990, 3)]);
    // No trade: the ratios are undefined, the counts zero.
    let e = trade_metrics(&r, 252.0, &|_| false);
    assert_eq!((e.trades, e.hit_rate, e.largest_loss), (0, 0.0, 0.0));
    assert!(e.profit_factor.is_nan() && e.avg_trade.is_nan() && e.top_market_share.is_nan() && e.mar.is_nan());
}
