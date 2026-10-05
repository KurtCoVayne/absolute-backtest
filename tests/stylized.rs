//! Stylized-fact tests (data-bundle doc, section 8): the canonical
//! strategies (12-1 momentum, low volatility, short-term reversal, size by
//! its catalog proxy) run on the point-in-time universe with the default
//! cost model and land inside wide published ranges. The ranges catch gross
//! execution or data errors, not strategy merit. The bundle is named by
//! `ABT_BUNDLE_DIR`; without it the ranges are skipped with a note, and the
//! strategies are only shown to trade on the synthetic catalog market.

mod corpus;

use std::fs;
use std::path::Path;

use absolute_backtest::bundle::load_bundle;
use absolute_backtest::check::{check_program, Program};
use absolute_backtest::data::synthetic_daily_v2;
use absolute_backtest::kernel::{run, ExecConfig, RunResult};
use absolute_backtest::study::{return_metrics, subperiod_sharpes, trading_metrics, ReturnMetrics};
use absolute_backtest::Lit;

const CANONICAL: [&str; 4] = ["momentum_12_1", "low_volatility", "short_term_reversal", "size_proxy"];

fn program(name: &str) -> Program {
    let src = fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join(format!("corpus/strategies/{}.dsl", name))).unwrap();
    let mut ws = corpus::base_workspace();
    ws.add_source(&src).unwrap();
    let (p, diags) = check_program(&ws, name);
    let text: Vec<String> = diags.iter().map(|d| d.to_string()).collect();
    p.unwrap_or_else(|| panic!("{} should check:\n{}", name, text.join("\n")))
}

#[test]
fn the_canonical_strategies_trade_both_legs_on_the_synthetic_catalog_market() {
    let ds = synthetic_daily_v2(&["AAA", "BBB", "CCC", "DDD", "SPY"], (2022, 1, 3), 400, 11);
    for name in CANONICAL {
        let prog = program(name);
        let r = run(&prog, &ds, ExecConfig::default()).unwrap_or_else(|e| panic!("{}: {}", name, e));
        assert!(!r.fills.is_empty(), "{} never filled", name);
        assert!(r.fills.iter().any(|f| f.quantity < 0.0) && r.fills.iter().any(|f| f.quantity > 0.0), "{} did not trade both legs", name);
        // Four tenths of equity a leg at each rebalance: the book is never
        // levered at a fill, and the mark between rebalances stays near one.
        let tm = trading_metrics(&r, 252.0);
        assert!(tm.max_leverage < 2.0, "{}: gross exposure {} times equity", name, tm.max_leverage);
    }
}

/// A published range for a canonical premium: the long-short annualised
/// return, its volatility, the deepest drawdown, and a year the strategy is
/// known to have failed (the sign of that year's Sharpe ratio, when the
/// sample covers it).
struct Range {
    name: &'static str,
    cagr: (f64, f64),
    volatility: (f64, f64),
    max_drawdown: f64,
    known_failure: Option<(i64, &'static str)>,
}

const RANGES: [Range; 4] = [
    Range {
        name: "momentum_12_1",
        cagr: (-0.10, 0.30),
        volatility: (0.08, 0.45),
        max_drawdown: 0.85,
        known_failure: Some((2009, "the momentum crash")),
    },
    Range {
        name: "low_volatility",
        cagr: (-0.10, 0.25),
        volatility: (0.05, 0.40),
        max_drawdown: 0.80,
        known_failure: Some((2020, "the low-volatility drawdown")),
    },
    Range {
        name: "short_term_reversal",
        cagr: (-0.15, 0.30),
        volatility: (0.05, 0.45),
        max_drawdown: 0.85,
        known_failure: None,
    },
    Range {
        name: "size_proxy",
        cagr: (-0.15, 0.25),
        volatility: (0.05, 0.45),
        max_drawdown: 0.85,
        known_failure: None,
    },
];

fn check_range(range: &Range, r: &RunResult) -> Vec<String> {
    let m: ReturnMetrics = return_metrics(&r.equity_curve, 252.0);
    let mut problems = Vec::new();
    if m.cagr < range.cagr.0 || m.cagr > range.cagr.1 {
        problems.push(format!("{}: CAGR {:.4} outside {:?}", range.name, m.cagr, range.cagr));
    }
    if m.volatility < range.volatility.0 || m.volatility > range.volatility.1 {
        problems.push(format!("{}: volatility {:.4} outside {:?}", range.name, m.volatility, range.volatility));
    }
    if m.max_drawdown > range.max_drawdown {
        problems.push(format!("{}: max drawdown {:.4} above {}", range.name, m.max_drawdown, range.max_drawdown));
    }
    if let Some((year, what)) = range.known_failure {
        let years = subperiod_sharpes(&r.equity_curve, 252.0, 100);
        if let Some((_, s)) = years.iter().find(|(y, _)| *y == year) {
            if *s > 0.0 {
                problems.push(format!("{}: {} ({}) does not show: that year's Sharpe ratio is {:.2}", range.name, what, year, s));
            }
        }
    }
    problems
}

#[test]
fn the_canonical_premiums_land_in_their_published_ranges_on_the_bundle() {
    let Ok(dir) = std::env::var("ABT_BUNDLE_DIR") else {
        eprintln!("skipped: ABT_BUNDLE_DIR names no bundle of the point-in-time universe");
        return;
    };
    let n: i64 = std::env::var("ABT_STYLIZED_N").ok().and_then(|s| s.parse().ok()).unwrap_or(50);
    let mut problems = Vec::new();
    for range in &RANGES {
        let prog = program(range.name);
        let (ds, manifest) = load_bundle(&prog, Path::new(&dir), false).unwrap_or_else(|e| panic!("{}", e));
        let cfg = ExecConfig {
            param_overrides: vec![("n".to_string(), Lit::Int(n))],
            ..ExecConfig::default()
        };
        let r = run(&prog, &ds, cfg).unwrap_or_else(|e| panic!("{} on {}@{}: {}", range.name, manifest.name, manifest.version, e));
        let m = return_metrics(&r.equity_curve, 252.0);
        let tm = trading_metrics(&r, 252.0);
        eprintln!(
            "{} on {}@{}: {} bars, CAGR {:.4}, volatility {:.4}, Sharpe {:.3} (se {:.3}), max drawdown {:.4}, turnover {:.2}, fill ratio {:.3}",
            range.name,
            manifest.name,
            manifest.version,
            r.bars.len(),
            m.cagr,
            m.volatility,
            m.sharpe,
            m.sharpe_se,
            m.max_drawdown,
            tm.turnover,
            tm.fill_ratio
        );
        problems.extend(check_range(range, &r));
    }
    assert!(problems.is_empty(), "{}", problems.join("\n"));
}
