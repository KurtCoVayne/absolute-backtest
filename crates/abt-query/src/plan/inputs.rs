//! The tables a test reads (Q-4.1, Q-5.1's INPUT), from the data layer:
//! a run's bar returns; its decisions as rule activations, or its fills as
//! realised against modelled slippage; an observed relation's ledger, with
//! forward returns when the test asks for them; a relation of the binding;
//! a ledger step's table; a grid's runs side by side. Every table holds the
//! rows its window and the hold-out keep (Q-2.3).

use std::collections::{BTreeMap, BTreeSet};

use super::Point;
use crate::data::{Data, Error};
use crate::relation;
use crate::run::Run;
use crate::tests::Table;

/// Which bars a table keeps.
pub type Keep<'a> = &'a dyn Fn(i64) -> bool;

fn table(time: Vec<i64>, entity: Option<Vec<u64>>, columns: BTreeMap<String, Vec<f64>>) -> Table {
    Table { time: Some(time), entity, columns }
}

/// A run's bar returns (the fixed-base or compounded returns its report
/// block states), as the series `returns`.
pub fn returns(data: &Data, run: &str, keep: Keep) -> Result<Table, Error> {
    let (t, x): (Vec<i64>, Vec<f64>) = Run::read(data, run)?.bar_returns().into_iter().filter(|(t, _)| keep(*t)).unzip();
    Ok(table(t, None, BTreeMap::from([("returns".to_string(), x)])))
}

/// The name of the i-th of n trials' column, ordered as the grid is.
pub fn trial_column(i: usize, n: usize) -> String {
    let width = n.saturating_sub(1).to_string().len();
    format!("trial{:0w$}", i, w = width)
}

/// A grid's runs side by side: one column of bar returns per run
/// (`trial0`, ...), on the bars every run has.
pub fn grid(data: &Data, runs: &[String], keep: Keep) -> Result<Table, Error> {
    let series: Vec<BTreeMap<i64, f64>> = runs
        .iter()
        .map(|r| Run::read(data, r).map(|x| x.bar_returns().into_iter().filter(|(t, _)| keep(*t)).collect()))
        .collect::<Result<_, _>>()?;
    let Some(first) = series.first() else { return Ok(Table::default()) };
    let common: Vec<i64> = first.keys().filter(|t| series.iter().all(|s| s.contains_key(t))).copied().collect();
    let columns = series.iter().enumerate().map(|(i, s)| (trial_column(i, series.len()), common.iter().map(|t| s[t]).collect())).collect();
    Ok(table(common, None, columns))
}

/// The name of a rule's activation column: `head_k` for the strategy's own
/// rules (`decide_1`), `unit__head_k` for a library's; a rule label is
/// `unit::head#k`.
pub fn rule_column(label: &str, strategy: &str) -> String {
    let (unit, rest) = label.split_once("::").unwrap_or(("", label));
    let rest = rest.replace('#', "_");
    if unit == strategy || unit.is_empty() {
        rest
    } else {
        format!("{}__{}", unit, rest)
    }
}

/// A run's decisions as rule activations (Q-7): one row per instance (a
/// bar and an instrument any rule decided on), one column per rule, 1
/// where the rule fired on the instance, else 0.
pub fn activations(data: &Data, run: &str, strategy: &str, keep: Keep) -> Result<Table, Error> {
    let Some(b) = relation::read(data, &format!("kernel.{}.decisions", run))? else {
        return Ok(Table::default());
    };
    let (t, ins, rule) = (relation::ints(&b, "t")?, relation::ints(&b, "instrument")?, relation::texts(&b, "rule")?);
    let mut fired: BTreeSet<(i64, u64, String)> = BTreeSet::new();
    let mut rules: BTreeSet<String> = BTreeSet::new();
    for i in 0..b.num_rows() {
        let (Some(t), Some(r)) = (t[i], &rule[i]) else { continue };
        if !keep(t) {
            continue;
        }
        let name = rule_column(r, strategy);
        rules.insert(name.clone());
        fired.insert((t, ins[i].unwrap_or(0) as u64, name));
    }
    let instances: BTreeSet<(i64, u64)> = fired.iter().map(|(t, e, _)| (*t, *e)).collect();
    let columns = rules
        .iter()
        .map(|r| (r.clone(), instances.iter().map(|(t, e)| if fired.contains(&(*t, *e, r.clone())) { 1.0 } else { 0.0 }).collect()))
        .collect();
    let (time, entity): (Vec<i64>, Vec<u64>) = instances.into_iter().unzip();
    Ok(table(time, Some(entity), columns))
}

/// A run's fills as realised against modelled slippage per unit of
/// notional (`cost_realism`): a simulated fill realises the model's own
/// slippage; a live one's comes with the fills of M-6 and M-10.
pub fn fills(data: &Data, run: &str, keep: Keep) -> Result<Table, Error> {
    let r = Run::read(data, run)?;
    let rows: Vec<_> = r.fills.iter().filter(|f| keep(f.t) && f.quantity != 0.0 && f.price > 0.0).collect();
    let per = |f: &crate::run::Fill| f.slippage / (f.quantity.abs() * f.price * f.multiplier);
    let slippage: Vec<f64> = rows.iter().map(|f| per(f)).collect();
    Ok(table(
        rows.iter().map(|f| f.t).collect(),
        Some(rows.iter().map(|f| f.instrument.unwrap_or(0)).collect()),
        BTreeMap::from([("realised".to_string(), slippage.clone()), ("modelled".to_string(), slippage)]),
    ))
}

/// The numeric columns of a relation's rows, with its time and entity.
fn numeric(b: &arrow::record_batch::RecordBatch, time: &str, skip: &[&str]) -> Result<Table, Error> {
    use arrow::datatypes::DataType;
    let t = relation::ints(b, time)?;
    let mut entity = None;
    let mut columns = BTreeMap::new();
    for f in b.schema().fields() {
        let name = f.name().as_str();
        if name == time || skip.contains(&name) {
            continue;
        }
        match f.data_type() {
            DataType::UInt64 if entity.is_none() => {
                entity = Some(relation::ints(b, name)?.into_iter().map(|v| v.unwrap_or(0) as u64).collect::<Vec<u64>>());
            }
            DataType::Float64 | DataType::Int64 => {
                columns.insert(name.to_string(), relation::nums(b, name)?.into_iter().map(|v| v.unwrap_or(f64::NAN)).collect());
            }
            _ => {}
        }
    }
    Ok(table(t.into_iter().map(|v| v.unwrap_or(i64::MIN)).collect(), entity, columns))
}

/// The rows of `t` at the indices `idx`.
fn rows_at(t: Table, idx: &[usize]) -> Table {
    let time = t.time.unwrap_or_default();
    table(
        idx.iter().map(|&i| time[i]).collect(),
        t.entity.map(|e| idx.iter().map(|&i| e[i]).collect()),
        t.columns.into_iter().map(|(k, v)| (k, idx.iter().map(|&i| v[i]).collect())).collect(),
    )
}

fn keep_rows(t: Table, keep: Keep) -> Table {
    let idx: Vec<usize> = t.time.as_ref().map(|time| (0..time.len()).filter(|&i| keep(time[i])).collect()).unwrap_or_default();
    rows_at(t, &idx)
}

/// The ledger of `relation` the run observed (`kernel.<run>.ledger.R`), its
/// numeric arguments as columns; the instances stuck on a literal are left
/// out.
pub fn feature(data: &Data, run: &str, rel: &str, keep: Keep) -> Result<Table, Error> {
    let name = format!("kernel.{}.ledger.{}", run, rel);
    let b = relation::read(data, &name)?.ok_or_else(|| Error::Missing(format!("run {} has no ledger of `{}`: the run observes it only when a step or a claim reads it", run, rel)))?;
    let stuck = relation::bools(&b, "stuck")?;
    let t = numeric(&b, "bar", &["row", "stuck", "stuck_literal"])?;
    let time = t.time.clone().unwrap_or_default();
    let idx: Vec<usize> = (0..time.len()).filter(|&i| keep(time[i]) && !stuck[i].unwrap_or(false)).collect();
    Ok(rows_at(t, &idx))
}

/// A ledger step's table (`kernel.<run>.<NAME>`).
pub fn ledger(data: &Data, relation_name: &str, keep: Keep) -> Result<Table, Error> {
    let b = relation::read(data, relation_name)?.ok_or_else(|| Error::Missing(format!("the store has no relation {}", relation_name)))?;
    Ok(keep_rows(numeric(&b, "bar", &["row", "stuck", "stuck_literal"])?, keep))
}

/// The instruments `filter` names (`A in (T1, T2, ...)`), by their tickers,
/// in its order.
fn filtered(data: &Data, filter: &str) -> Result<Vec<(String, u64)>, Error> {
    let list = filter
        .split_once(" in ")
        .map(|(_, l)| l.trim().trim_start_matches('(').trim_end_matches(')').to_string())
        .ok_or_else(|| Error::Malformed(format!("`{}` is not an entity filter: A in (T1, T2, ...)", filter)))?;
    let b = relation::read(data, "ticker")?.ok_or_else(|| Error::Missing("the store has no ticker relation".into()))?;
    let (ids, symbols) = (relation::ints(&b, "id")?, relation::texts(&b, "symbol")?);
    list.split(',')
        .map(|s| s.trim().trim_matches('"').to_string())
        .filter(|s| !s.is_empty())
        .map(|s| {
            (0..b.num_rows())
                .find(|&i| symbols[i].as_deref() == Some(s.as_str()))
                .and_then(|i| ids[i])
                .map(|id| (s.clone(), id as u64))
                .ok_or_else(|| Error::Missing(format!("no instrument carries the ticker {}", s)))
        })
        .collect()
}

/// A relation of the binding (`feed R`, Q-5.1): its rows as a panel of its
/// numeric columns, or, filtered to named instruments, one column per
/// instrument (by ticker) on the times they share, the instruments in the
/// filter's order (what a cointegration test reads).
pub fn feed(data: &Data, binding: &[(String, String)], rel: &str, filter: Option<&str>, keep: Keep) -> Result<Table, Error> {
    let stored = crate::feed::stored(binding, rel).ok_or_else(|| Error::Missing(format!("the binding holds no relation `{}`", rel)))?;
    let x = crate::feed::class_rows(data, &stored)?.ok_or_else(|| Error::Missing(format!("the store has no relation {}", stored)))?;
    let all = keep_rows(table(x.t, Some(x.entity), x.nums), keep);
    let Some(filter) = filter else { return Ok(all) };
    let names = filtered(data, filter)?;
    let value = all.columns.keys().next().cloned().ok_or_else(|| Error::Malformed(format!("{} has no numeric column", stored)))?;
    let entity = all.entity.clone().ok_or_else(|| Error::Malformed(format!("{} has no instrument to filter by", stored)))?;
    let time = all.time.clone().unwrap_or_default();
    let mut by: Vec<BTreeMap<i64, f64>> = vec![BTreeMap::new(); names.len()];
    for i in 0..time.len() {
        if let Some(k) = names.iter().position(|(_, id)| *id == entity[i]) {
            by[k].insert(time[i], all.columns[&value][i]);
        }
    }
    let common: Vec<i64> = by.first().map(|m| m.keys().filter(|t| by.iter().all(|s| s.contains_key(t))).copied().collect()).unwrap_or_default();
    let columns = names.iter().zip(&by).map(|((n, _), s)| (n.clone(), common.iter().map(|t| s[t]).collect())).collect();
    Ok(table(common, None, columns))
}

/// The forward return of each row's instrument over `horizon` of its
/// bars, from a price panel (`close`): `p(t + h) / p(t) − 1`, NaN past the
/// end.
pub fn forward_returns(prices: &Table, horizon: usize, at: &[(i64, u64)]) -> Vec<f64> {
    let (Some(time), Some(entity)) = (&prices.time, &prices.entity) else {
        return vec![f64::NAN; at.len()];
    };
    let Some(p) = prices.columns.values().next() else { return vec![f64::NAN; at.len()] };
    let mut by: BTreeMap<u64, Vec<(i64, f64)>> = BTreeMap::new();
    for i in 0..time.len() {
        by.entry(entity[i]).or_default().push((time[i], p[i]));
    }
    for v in by.values_mut() {
        v.sort_by_key(|x| x.0);
    }
    at.iter()
        .map(|(t, e)| {
            let Some(s) = by.get(e) else { return f64::NAN };
            let Ok(i) = s.binary_search_by_key(t, |x| x.0) else { return f64::NAN };
            match s.get(i + horizon) {
                Some(&(_, later)) if s[i].1 > 0.0 => later / s[i].1 - 1.0,
                _ => f64::NAN,
            }
        })
        .collect()
}

/// The parameters of a test or a claim, as written, by name.
pub fn written(params: &Point) -> BTreeMap<String, String> {
    params.iter().cloned().collect()
}
