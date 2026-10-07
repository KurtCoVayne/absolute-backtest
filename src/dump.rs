//! `abt run --dump DIR` (data-bundle doc, section 8): everything a run saw
//! and did, as Parquet and JSON, so that an independent implementation can
//! replay the decisions through its own executor and be compared with the
//! kernel's fills, book and NAV. Timestamps are naive second timestamps and
//! equities are written by name.

use std::path::Path;

use crate::check::Program;
use crate::data::write_parquet_dir;
use crate::kernel::time::format_timestamp;
use crate::kernel::{Dataset, ExecConfig, RunResult};
use crate::table::{write_table, Col};

fn strs<I: IntoIterator<Item = String>>(it: I) -> Col {
    Col::Str(it.into_iter().map(Some).collect())
}

fn nums<I: IntoIterator<Item = f64>>(it: I) -> Col {
    Col::Float(it.into_iter().map(Some).collect())
}

fn times<I: IntoIterator<Item = i64>>(it: I) -> Col {
    Col::Time(it.into_iter().map(Some).collect())
}

fn flags<I: IntoIterator<Item = bool>>(it: I) -> Col {
    Col::Bool(it.into_iter().map(Some).collect())
}

/// Write the dump: `data/` (the dataset as Parquet), `config.json`, and
/// `decisions`, `fills`, `dropped`, `actions`, `nav` and `final` (cash and
/// positions at the end) as `.parquet`.
pub fn write_dump(prog: &Program, ds: &Dataset, cfg: &ExecConfig, result: &RunResult, dir: &Path) -> Result<(), String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("{}: {}", dir.display(), e))?;
    write_parquet_dir(prog, ds, &dir.join("data"))?;
    let config = serde_json::json!({
        "strategy": prog.strategy,
        "environment": prog.environment,
        "resolution": prog.resolution.to_string(),
        "mode": prog.mode.to_string(),
        "symbols": result.symbols,
        "price_relation": result.price_relation,
        "volume_relation": result.volume_relation,
        "bars": result.bars.iter().map(|t| format_timestamp(*t)).collect::<Vec<_>>(),
        "exec": cfg,
    });
    let text = serde_json::to_string_pretty(&config).map_err(|e| e.to_string())?;
    std::fs::write(dir.join("config.json"), text).map_err(|e| e.to_string())?;
    let sym = |i: u32| result.symbols[i as usize].clone();
    let ds_ = &result.decisions;
    write_table(
        &dir.join("decisions.parquet"),
        vec![
            ("t", times(ds_.iter().map(|d| d.t))),
            ("equity", strs(ds_.iter().map(|d| sym(d.decision.equity)))),
            ("ctor", strs(ds_.iter().map(|d| d.decision.ctor.name().to_string()))),
            ("amount", nums(ds_.iter().map(|d| d.decision.amount))),
            ("order", strs(ds_.iter().map(|d| d.decision.order.to_string()))),
            ("rule", strs(ds_.iter().map(|d| prog.rule_label(d.rule)))),
        ],
    )?;
    let fs = &result.fills;
    write_table(
        &dir.join("fills.parquet"),
        vec![
            ("t", times(fs.iter().map(|f| f.t))),
            ("equity", strs(fs.iter().map(|f| sym(f.equity)))),
            ("quantity", nums(fs.iter().map(|f| f.quantity))),
            ("price", nums(fs.iter().map(|f| f.price))),
            ("commission", nums(fs.iter().map(|f| f.commission))),
            ("fee", nums(fs.iter().map(|f| f.fee))),
            ("slippage", nums(fs.iter().map(|f| f.slippage))),
            ("impact", nums(fs.iter().map(|f| f.impact))),
            ("participation", nums(fs.iter().map(|f| f.participation))),
            ("at_last_price", flags(fs.iter().map(|f| f.at_last_price))),
            ("partial", flags(fs.iter().map(|f| f.partial))),
            ("forced", flags(fs.iter().map(|f| f.forced))),
        ],
    )?;
    let dr = &result.dropped;
    write_table(
        &dir.join("dropped.parquet"),
        vec![
            ("t", times(dr.iter().map(|(t, _, _)| *t))),
            ("equity", strs(dr.iter().map(|(_, d, _)| sym(d.equity)))),
            ("ctor", strs(dr.iter().map(|(_, d, _)| d.ctor.name().to_string()))),
            ("amount", nums(dr.iter().map(|(_, d, _)| d.amount))),
            ("reason", strs(dr.iter().map(|(_, _, r)| r.clone()))),
        ],
    )?;
    let ac = &result.actions;
    let kind_detail = |a: &crate::kernel::Action| -> (String, String) {
        match a {
            crate::kernel::Action::Split { factor } => ("split".into(), format!("{}", factor)),
            crate::kernel::Action::Dividend { amount, shares } => ("dividend".into(), format!("{} x {}", amount, shares)),
            crate::kernel::Action::Delisting { reason, haircut } => ("delisting".into(), format!("{} {}", reason, haircut)),
            crate::kernel::Action::Reinvest { amount, shares, added } => ("reinvest".into(), format!("{} x {} -> {}", amount, shares, added)),
        }
    };
    write_table(
        &dir.join("actions.parquet"),
        vec![
            ("t", times(ac.iter().map(|a| a.t))),
            ("equity", strs(ac.iter().map(|a| sym(a.equity)))),
            ("action", strs(ac.iter().map(|a| kind_detail(&a.action).0))),
            ("detail", strs(ac.iter().map(|a| kind_detail(&a.action).1))),
            ("cash", nums(ac.iter().map(|a| a.cash))),
        ],
    )?;
    write_nav(result, &dir.join("nav.parquet"))?;
    let mut keys = vec!["cash".to_string()];
    let mut values = vec![result.final_cash];
    for (s, q) in &result.final_positions {
        keys.push(sym(*s));
        values.push(*q);
    }
    write_table(&dir.join("final.parquet"), vec![("key", strs(keys)), ("value", nums(values))])?;
    Ok(())
}

/// The book at every bar (`t, equity, cash, gross, net, leverage, ret`),
/// `ret` being the bar's return under the run's accounting.
pub fn write_nav(result: &RunResult, path: &Path) -> Result<(), String> {
    let ex = &result.exposure;
    write_table(
        path,
        vec![
            ("t", times(ex.iter().map(|e| e.t))),
            ("equity", nums(ex.iter().map(|e| e.equity))),
            ("cash", nums(ex.iter().map(|e| e.cash))),
            ("gross", nums(ex.iter().map(|e| e.gross))),
            ("net", nums(ex.iter().map(|e| e.net))),
            ("leverage", nums(ex.iter().map(|e| e.leverage))),
        ],
    )
}
