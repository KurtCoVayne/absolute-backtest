//! The report tables the red flags read (Q-8, ERRATA E-95): the `summary`
//! sheet shows the rows of E-95 by name, `contribution` holds every run's
//! profit by entity (named by ticker), and `subperiods` keeps its columns
//! and gains the best run's net profit and trades per calendar year.

#![allow(clippy::result_large_err)]

use abt_kernel::data::synthetic_daily;
use abt_query::plan::exec::{execute, Endpoints, Options};
use abt_query::plan::{self, trials::relation_of};
use abt_query::relation;
use abt_tests as common;

use abt_tests::{momentum, plan_program, plan_services};

#[test]
fn the_report_shows_the_red_flag_rows_the_contribution_and_the_years() {
    let src = momentum("reported", "equity_default");
    let prog = common::program(&src, "reported");
    let ds = synthetic_daily(&["AAA", "BBB", "CCC", "DDD", "SPY"], (2022, 1, 3), 700, 5);
    let (data, kernels) = plan_services(&prog, &ds, 2);
    let text = "study reported { program reported\n grid { n = 1, 2 }\n holdout trailing:1y\n report { metrics [return, mar, drawdown_duration, trades, profit_factor, hit_rate, avg_trade_to_cost, top_market_share]; tables [summary, contribution, subperiods] } }";
    let p = plan::plan(&plan::parse(text).unwrap(), "reported").unwrap();
    let ex = execute(
        &p,
        &plan_program(&src, "reported"),
        &Endpoints { data: &data, kernels: &kernels },
        &Options { threads: 2, ..Options::default() },
    )
    .unwrap();
    assert_eq!(ex.outcomes.len(), 2);
    assert!(ex.outcomes.iter().all(|o| o.trades.trades > 0), "{:?}", ex.outcomes.iter().map(|o| o.trades.trades).collect::<Vec<_>>());
    // summary: the rows by name, one column each.
    let summary = relation::read(&data, &relation_of(&ex.hash, "report.summary")).unwrap().unwrap();
    type Of = fn(&plan::exec::Outcome) -> f64;
    let rows: Vec<(&str, Of)> = vec![
        ("return", |o| o.trades.annual_return),
        ("mar", |o| o.trades.mar),
        ("drawdown_duration", |o| o.returns.max_drawdown_duration as f64),
        ("trades", |o| o.trades.trades as f64),
        ("profit_factor", |o| o.trades.profit_factor),
        ("hit_rate", |o| o.trades.hit_rate),
        ("top_market_share", |o| o.trades.top_market_share),
    ];
    for (name, of) in rows {
        let col = nums_or_nulls(&summary, name);
        for (i, o) in ex.outcomes.iter().enumerate() {
            let want = of(o);
            match col[i] {
                Some(v) => assert!((v - want).abs() <= 1e-12 * want.abs().max(1.0), "{} of {}: {} against {}", name, o.run_id, v, want),
                None => assert!(!want.is_finite(), "{} of {} is null", name, o.run_id),
            }
        }
    }
    // avg_trade_to_cost: the default execution block charges commissions,
    // and every point traded.
    assert!(relation::nums(&summary, "avg_trade_to_cost").unwrap().iter().all(|v| v.is_some_and(|v| v.is_finite())));
    assert!(ex.outcomes.iter().all(|o| o.trades.round_turn_cost > 0.0));
    // contribution: per run and entity, by ticker; the shares of a run
    // whose trades made money sum to one.
    let c = relation::read(&data, &relation_of(&ex.hash, "report.contribution")).unwrap().unwrap();
    let (runs, entities, profit, shares, trades, hits) = (
        relation::texts(&c, "run").unwrap(),
        relation::texts(&c, "entity").unwrap(),
        relation::nums(&c, "net_profit").unwrap(),
        nums_or_nulls(&c, "share"),
        relation::ints(&c, "trades").unwrap(),
        relation::nums(&c, "hit_rate").unwrap(),
    );
    // By ticker (the synthetic market renames its second name).
    assert!(entities.iter().flatten().all(|e| ["AAA", "BBB", "BBBX", "CCC", "DDD", "SPY"].contains(&e.as_str())), "{:?}", entities);
    for o in &ex.outcomes {
        let rows: Vec<usize> = (0..c.num_rows()).filter(|&i| runs[i].as_deref() == Some(&o.run_id)).collect();
        assert_eq!(rows.len(), o.trades.by_entity.len(), "{}", o.run_id);
        let total: f64 = rows.iter().map(|&i| profit[i].unwrap()).sum();
        let by: f64 = o.trades.by_entity.iter().map(|c| c.net_profit).sum();
        assert!((total - by).abs() < 1e-9);
        assert_eq!(rows.iter().map(|&i| trades[i].unwrap() as usize).sum::<usize>(), o.trades.trades);
        assert!(rows.iter().all(|&i| hits[i].is_some_and(|h| (0.0..=1.0).contains(&h))));
        // The shares of a run whose trades made money sum to one; of one
        // whose trades lost, they are undefined, as `by_entity` says.
        if total > 0.0 {
            let sum: f64 = rows.iter().map(|&i| shares[i].unwrap()).sum();
            assert!((sum - 1.0).abs() < 1e-9, "{}", sum);
        } else {
            assert!(rows.iter().all(|&i| shares[i].is_none()));
            assert!(o.trades.by_entity.iter().all(|c| c.share.is_nan()));
        }
    }
    // subperiods: the old columns, then the best run's year rows.
    let s = relation::read(&data, &relation_of(&ex.hash, "report.subperiods")).unwrap().unwrap();
    let names: Vec<String> = s.schema().fields().iter().map(|f| f.name().clone()).collect();
    for col in ["grid", "year", "sharpe", "net_profit", "trades"] {
        assert!(names.contains(&col.to_string()), "{:?}", names);
    }
    let g = &ex.grids[0];
    let best = &ex.outcomes[g.runs[g.best]];
    let (years, profits, counts) = (relation::ints(&s, "year").unwrap(), relation::nums(&s, "net_profit").unwrap(), relation::ints(&s, "trades").unwrap());
    assert_eq!(years.len(), g.subperiods.len());
    for i in 0..years.len() {
        let y = years[i].unwrap();
        let row = best.trades.by_year.iter().find(|r| r.year == y).unwrap_or_else(|| panic!("{} has no year row", y));
        assert!((profits[i].unwrap() - row.net_profit).abs() < 1e-9);
        assert_eq!(counts[i].unwrap() as usize, row.trades);
    }
    let kept: f64 = best.trades.by_year.iter().map(|r| r.net_profit).sum();
    assert!((kept - best.trades.net_profit).abs() < 1e-9);
}

/// A numeric column, or, when the sheet held a null in every row (a ratio
/// undefined for every run), as many nulls as rows.
fn nums_or_nulls(b: &arrow::record_batch::RecordBatch, name: &str) -> Vec<Option<f64>> {
    match relation::nums(b, name) {
        Ok(c) => c,
        Err(_) => {
            let texts = relation::texts(b, name).unwrap_or_else(|e| panic!("{}: {}", name, e));
            assert!(texts.iter().all(Option::is_none), "{}: {:?}", name, texts);
            vec![None; texts.len()]
        }
    }
}
