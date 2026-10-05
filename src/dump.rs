//! `abt run --dump DIR` (data-bundle doc, section 8): everything a run saw
//! and did, as CSV and JSON, so that an independent implementation can
//! replay the decisions through its own executor and be compared with the
//! kernel's fills, book and NAV. Timestamps are written as the loader reads
//! them (`YYYY-MM-DD`, with a time when not midnight) and equities by name.

use std::path::Path;

use crate::check::Program;
use crate::data::write_csv_dir;
use crate::kernel::time::format_timestamp;
use crate::kernel::{Dataset, ExecConfig, RunResult};

fn csv_field(s: &str) -> String {
    if s.contains(',') || s.contains('"') || s.contains('\n') {
        format!("\"{}\"", s.replace('"', "\"\""))
    } else {
        s.to_string()
    }
}

/// Write the dump: `data/` (the dataset as CSV), `config.json`,
/// `decisions.csv`, `fills.csv`, `dropped.csv`, `actions.csv`, `nav.csv`
/// and `final.csv` (cash and positions at the end).
pub fn write_dump(prog: &Program, ds: &Dataset, cfg: &ExecConfig, result: &RunResult, dir: &Path) -> Result<(), String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("{}: {}", dir.display(), e))?;
    write_csv_dir(prog, ds, &dir.join("data"))?;
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
    let mut out = String::from("t,equity,ctor,amount,rule\n");
    for d in &result.decisions {
        out.push_str(&format!(
            "{},{},{},{},{}\n",
            format_timestamp(d.t),
            sym(d.decision.equity),
            d.decision.ctor.name(),
            d.decision.amount,
            csv_field(&prog.rule_label(d.rule))
        ));
    }
    std::fs::write(dir.join("decisions.csv"), out).map_err(|e| e.to_string())?;
    let mut out = String::from("t,equity,quantity,price,commission,fee,slippage,impact,participation,at_last_price,partial,forced\n");
    for f in &result.fills {
        out.push_str(&format!(
            "{},{},{},{},{},{},{},{},{},{},{},{}\n",
            format_timestamp(f.t),
            sym(f.equity),
            f.quantity,
            f.price,
            f.commission,
            f.fee,
            f.slippage,
            f.impact,
            f.participation,
            f.at_last_price,
            f.partial,
            f.forced
        ));
    }
    std::fs::write(dir.join("fills.csv"), out).map_err(|e| e.to_string())?;
    let mut out = String::from("t,equity,ctor,amount,reason\n");
    for (t, d, reason) in &result.dropped {
        out.push_str(&format!("{},{},{},{},{}\n", format_timestamp(*t), sym(d.equity), d.ctor.name(), d.amount, csv_field(reason)));
    }
    std::fs::write(dir.join("dropped.csv"), out).map_err(|e| e.to_string())?;
    let mut out = String::from("t,equity,action,detail,cash\n");
    for a in &result.actions {
        let (kind, detail) = match &a.action {
            crate::kernel::Action::Split { factor } => ("split", format!("{}", factor)),
            crate::kernel::Action::Dividend { amount, shares } => ("dividend", format!("{} x {}", amount, shares)),
            crate::kernel::Action::Delisting { reason, haircut } => ("delisting", format!("{} {}", reason, haircut)),
        };
        out.push_str(&format!("{},{},{},{},{}\n", format_timestamp(a.t), sym(a.equity), kind, csv_field(&detail), a.cash));
    }
    std::fs::write(dir.join("actions.csv"), out).map_err(|e| e.to_string())?;
    let mut out = String::from("t,equity,cash,gross,net,leverage\n");
    for e in &result.exposure {
        out.push_str(&format!("{},{},{},{},{},{}\n", format_timestamp(e.t), e.equity, e.cash, e.gross, e.net, e.leverage));
    }
    std::fs::write(dir.join("nav.csv"), out).map_err(|e| e.to_string())?;
    let mut out = String::from("key,value\n");
    out.push_str(&format!("cash,{}\n", result.final_cash));
    for (s, q) in &result.final_positions {
        out.push_str(&format!("{},{}\n", sym(*s), q));
    }
    std::fs::write(dir.join("final.csv"), out).map_err(|e| e.to_string())?;
    Ok(())
}
