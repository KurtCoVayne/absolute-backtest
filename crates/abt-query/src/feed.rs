//! Feed statistics (Q-6): what the binding's data are, reported in full
//! rather than counted: coverage, revisions, latency, returns, liquidity,
//! survivorship, actions, membership and chains. Each is a query over the
//! binding's relations through the data layer, producing a table.

use std::collections::{BTreeMap, BTreeSet};

use arrow::datatypes::DataType;

use crate::data::{Data, Error};
use crate::metrics;
use crate::relation;
use crate::sheet::{Cell, Sheet};
use crate::time::{civil_from_days, day_key, DAY};

/// The statistics, by name.
pub const STATISTICS: [&str; 9] = ["coverage", "revisions", "latency", "returns", "liquidity", "survivorship", "actions", "membership", "chains"];

/// A relation of a market class as the store holds it: its instrument, its
/// time, its receipt, and its other columns by kind.
#[derive(Clone, Debug, Default)]
pub struct ClassRows {
    pub entity: Vec<u64>,
    pub t: Vec<i64>,
    pub r: Option<Vec<i64>>,
    pub nums: BTreeMap<String, Vec<f64>>,
    pub labels: BTreeMap<String, Vec<Option<String>>>,
    pub times: BTreeMap<String, Vec<Option<i64>>>,
}

impl ClassRows {
    pub fn len(&self) -> usize {
        self.t.len()
    }

    pub fn is_empty(&self) -> bool {
        self.t.is_empty()
    }

    /// The rows within `[from, to]`.
    fn within(self, w: Window) -> ClassRows {
        let idx: Vec<usize> = (0..self.t.len()).filter(|&i| w.0.is_none_or(|f| self.t[i] >= f) && w.1.is_none_or(|e| self.t[i] <= e)).collect();
        let take_u = |v: &Vec<u64>| idx.iter().map(|&i| v[i]).collect::<Vec<u64>>();
        ClassRows {
            entity: if self.entity.is_empty() { vec![] } else { take_u(&self.entity) },
            t: idx.iter().map(|&i| self.t[i]).collect(),
            r: self.r.as_ref().map(|r| idx.iter().map(|&i| r[i]).collect()),
            nums: self.nums.iter().map(|(k, v)| (k.clone(), idx.iter().map(|&i| v[i]).collect())).collect(),
            labels: self.labels.iter().map(|(k, v)| (k.clone(), idx.iter().map(|&i| v[i].clone()).collect())).collect(),
            times: self.times.iter().map(|(k, v)| (k.clone(), idx.iter().map(|&i| v[i]).collect())).collect(),
        }
    }
}

/// A statistic's window, `[from, to]`.
pub type Window = (Option<i64>, Option<i64>);

/// The rows of a stored relation, none when the store has none.
pub fn class_rows(data: &Data, stored: &str) -> Result<Option<ClassRows>, Error> {
    let Some(b) = relation::read(data, stored)? else { return Ok(None) };
    let mut out = ClassRows::default();
    let mut seen_time = false;
    for f in b.schema().fields() {
        let name = f.name().as_str();
        match f.data_type() {
            DataType::UInt64 if out.entity.is_empty() && name != "seq" => out.entity = relation::ints(&b, name)?.into_iter().map(|v| v.unwrap_or(0) as u64).collect(),
            DataType::Timestamp(_, _) if name == "r" => out.r = Some(relation::ints(&b, name)?.into_iter().map(|v| v.unwrap_or(0)).collect()),
            DataType::Timestamp(_, _) if !seen_time => {
                seen_time = true;
                out.t = relation::ints(&b, name)?.into_iter().map(|v| v.unwrap_or(0)).collect();
            }
            DataType::Timestamp(_, _) => {
                out.times.insert(name.to_string(), relation::ints(&b, name)?);
            }
            DataType::Float64 | DataType::Int64 => {
                out.nums.insert(name.to_string(), relation::nums(&b, name)?.into_iter().map(|v| v.unwrap_or(f64::NAN)).collect());
            }
            DataType::UInt64 => {}
            _ => {
                out.labels.insert(name.to_string(), relation::texts(&b, name)?);
            }
        }
    }
    if out.entity.is_empty() {
        out.entity = vec![0; b.num_rows()];
    }
    Ok(Some(out))
}

/// The store's name of the binding's relation `rel` (`close` is
/// `equity.close`).
pub fn stored(binding: &[(String, String)], rel: &str) -> Option<String> {
    binding.iter().map(|(r, _)| r.clone()).find(|r| r == rel || r.rsplit_once('.').is_some_and(|(_, short)| short == rel))
}

fn short(rel: &str) -> &str {
    rel.rsplit_once('.').map(|(_, s)| s).unwrap_or(rel)
}

/// The binding's market relations (not the registry's identities).
fn market(binding: &[(String, String)]) -> Vec<String> {
    binding
        .iter()
        .map(|(r, _)| r.clone())
        .filter(|r| r.contains('.') && !r.starts_with("kernel.") && !r.starts_with("query."))
        .collect()
}

fn year(t: i64) -> i64 {
    civil_from_days(day_key(t)).0
}

fn quantile(sorted: &[f64], q: f64) -> f64 {
    if sorted.is_empty() {
        return f64::NAN;
    }
    let pos = q * (sorted.len() - 1) as f64;
    let (lo, hi) = (pos.floor() as usize, pos.ceil() as usize);
    sorted[lo] + (sorted[hi] - sorted[lo]) * (pos - lo as f64)
}

/// The statistic `name` of the binding (of `relation`, when the statistic
/// is per relation and one is named), over `window`.
pub fn statistic(data: &Data, binding: &[(String, String)], name: &str, relation: Option<&str>, window: Window) -> Result<Sheet, Error> {
    let rel_of = |r: &str| stored(binding, r).ok_or_else(|| Error::Missing(format!("the binding holds no relation `{}`", r)));
    let read = |r: &str| -> Result<Option<ClassRows>, Error> { Ok(class_rows(data, r)?.map(|x| x.within(window))) };
    let named: Vec<String> = match relation {
        Some(r) => vec![rel_of(r)?],
        None => market(binding),
    };
    match name {
        "coverage" => {
            let mut s = Sheet::new("coverage", &["relation", "rows", "entities", "from", "to", "gaps", "covered"]);
            // The calendar: the bars of the binding's close, else each
            // relation's own times.
            let calendar: Option<BTreeSet<i64>> = match stored(binding, "close") {
                Some(c) => read(&c)?.map(|x| x.t.into_iter().collect()),
                None => None,
            };
            for r in &named {
                let Some(x) = read(r)? else { continue };
                let own: BTreeSet<i64> = x.t.iter().copied().collect();
                let cal = calendar.as_ref().unwrap_or(&own);
                let mut by: BTreeMap<u64, BTreeSet<i64>> = BTreeMap::new();
                for i in 0..x.len() {
                    by.entry(x.entity[i]).or_default().insert(x.t[i]);
                }
                let (mut gaps, mut expected) = (0usize, 0usize);
                for ts in by.values() {
                    let (Some(a), Some(b)) = (ts.first(), ts.last()) else { continue };
                    let span = cal.range(a..=b).count();
                    expected += span;
                    gaps += cal.range(a..=b).filter(|t| !ts.contains(t)).count();
                }
                let (from, to) = (x.t.iter().min().copied(), x.t.iter().max().copied());
                s.push(vec![
                    Cell::text(short(r)),
                    Cell::Int(x.len() as i64),
                    Cell::Int(by.len() as i64),
                    from.map(Cell::Time).unwrap_or(Cell::Null),
                    to.map(Cell::Time).unwrap_or(Cell::Null),
                    Cell::Int(gaps as i64),
                    if expected > 0 { Cell::num((expected - gaps) as f64 / expected as f64) } else { Cell::Null },
                ]);
            }
            Ok(s)
        }
        "revisions" | "latency" => {
            let cols: &[&str] = if name == "latency" {
                &["relation", "rows", "min_s", "median_s", "p90_s", "p99_s", "max_s"]
            } else {
                &["relation", "rows", "received_after_their_day", "latest_lag_s"]
            };
            let mut s = Sheet::new(name, cols);
            for r in &named {
                let Some(x) = read(r)? else { continue };
                let Some(rec) = &x.r else { continue };
                let mut lag: Vec<f64> = rec.iter().zip(&x.t).map(|(r, t)| (r - t) as f64 / 1e9).collect();
                lag.sort_by(|a, b| a.partial_cmp(b).expect("finite lags"));
                if name == "latency" {
                    s.push(vec![
                        Cell::text(short(r)),
                        Cell::Int(lag.len() as i64),
                        Cell::num(quantile(&lag, 0.0)),
                        Cell::num(quantile(&lag, 0.5)),
                        Cell::num(quantile(&lag, 0.9)),
                        Cell::num(quantile(&lag, 0.99)),
                        Cell::num(quantile(&lag, 1.0)),
                    ]);
                } else {
                    let late = rec.iter().zip(&x.t).filter(|(r, t)| **r >= (day_key(**t) + 1) * DAY).count();
                    s.push(vec![
                        Cell::text(short(r)),
                        Cell::Int(x.len() as i64),
                        Cell::Int(late as i64),
                        Cell::num(lag.last().copied().unwrap_or(f64::NAN)),
                    ]);
                }
            }
            Ok(s)
        }
        "returns" => {
            let close = read(&rel_of("close")?)?.unwrap_or_default();
            let p = close.nums.values().next().cloned().unwrap_or_default();
            let mut by: BTreeMap<u64, Vec<(i64, f64)>> = BTreeMap::new();
            for ((e, t), x) in close.entity.iter().zip(&close.t).zip(&p) {
                by.entry(*e).or_default().push((*t, *x));
            }
            let mut s = Sheet::new("returns", &["entity", "n", "mean", "sd", "skew", "kurtosis", "min", "max", "rho_1"]);
            let mut pooled = Vec::new();
            let row = |label: Cell, r: &[f64]| -> Vec<Cell> {
                let n = r.len() as f64;
                let mean = r.iter().sum::<f64>() / n.max(1.0);
                let sd = if r.len() > 1 {
                    (r.iter().map(|x| (x - mean).powi(2)).sum::<f64>() / (n - 1.0)).sqrt()
                } else {
                    f64::NAN
                };
                vec![
                    label,
                    Cell::Int(r.len() as i64),
                    Cell::num(mean),
                    Cell::num(sd),
                    Cell::num(metrics::skewness(r)),
                    Cell::num(metrics::excess_kurtosis(r)),
                    Cell::num(r.iter().cloned().fold(f64::INFINITY, f64::min)),
                    Cell::num(r.iter().cloned().fold(f64::NEG_INFINITY, f64::max)),
                    Cell::num(metrics::autocorrelation(r, 1)),
                ]
            };
            for (e, mut v) in by {
                v.sort_by_key(|x| x.0);
                let r: Vec<f64> = v.windows(2).filter(|w| w[0].1 > 0.0).map(|w| w[1].1 / w[0].1 - 1.0).collect();
                pooled.extend(r.iter().copied());
                s.push(row(Cell::Int(e as i64), &r));
            }
            s.push(row(Cell::text("pooled"), &pooled));
            Ok(s)
        }
        "liquidity" => {
            let close = read(&rel_of("close")?)?.unwrap_or_default();
            let volume = read(&rel_of("volume")?)?.unwrap_or_default();
            let p: BTreeMap<(u64, i64), f64> = (0..close.len())
                .map(|i| ((close.entity[i], close.t[i]), close.nums.values().next().map(|c| c[i]).unwrap_or(f64::NAN)))
                .collect();
            let v = volume.nums.values().next().cloned().unwrap_or_default();
            let mut by: BTreeMap<u64, Vec<(f64, f64)>> = BTreeMap::new();
            for ((e, t), x) in volume.entity.iter().zip(&volume.t).zip(&v) {
                let price = p.get(&(*e, *t)).copied().unwrap_or(f64::NAN);
                by.entry(*e).or_default().push((*x, price));
            }
            let mut s = Sheet::new("liquidity", &["entity", "bars", "adv", "dollar_volume", "zero_volume_share"]);
            for (e, rows) in by {
                let n = rows.len() as f64;
                let adv = rows.iter().map(|r| r.0).sum::<f64>() / n;
                let dv: Vec<f64> = rows.iter().filter(|r| r.1.is_finite()).map(|r| r.0 * r.1).collect();
                s.push(vec![
                    Cell::Int(e as i64),
                    Cell::Int(rows.len() as i64),
                    Cell::num(adv),
                    Cell::num(dv.iter().sum::<f64>() / dv.len().max(1) as f64),
                    Cell::num(rows.iter().filter(|r| r.0 == 0.0).count() as f64 / n),
                ]);
            }
            Ok(s)
        }
        "survivorship" => {
            let listed = read(&rel_of("listed")?)?.unwrap_or_default();
            let delisted = match stored(binding, "delisted") {
                Some(d) => read(&d)?.unwrap_or_default(),
                None => ClassRows::default(),
            };
            let mut first: BTreeMap<u64, i64> = BTreeMap::new();
            let mut last: BTreeMap<u64, i64> = BTreeMap::new();
            for i in 0..listed.len() {
                let e = listed.entity[i];
                first.entry(e).and_modify(|t| *t = (*t).min(listed.t[i])).or_insert(listed.t[i]);
                last.entry(e).and_modify(|t| *t = (*t).max(listed.t[i])).or_insert(listed.t[i]);
            }
            let mut gone: BTreeMap<u64, (i64, String)> = BTreeMap::new();
            let reason = delisted.labels.values().next().cloned().unwrap_or_default();
            for i in 0..delisted.len() {
                let e = delisted.entity[i];
                let r = reason.get(i).cloned().flatten().unwrap_or_else(|| "unknown".into());
                gone.entry(e)
                    .and_modify(|x| {
                        if delisted.t[i] < x.0 {
                            *x = (delisted.t[i], r.clone());
                        }
                    })
                    .or_insert((delisted.t[i], r));
            }
            let (Some(start), Some(end)) = (listed.t.iter().min().copied(), listed.t.iter().max().copied()) else {
                return Ok(Sheet::new("survivorship", &["year", "listings", "delistings"]));
            };
            let mut s = Sheet::new("survivorship", &["year", "listings", "delistings", "reasons"]);
            for y in year(start)..=year(end) {
                let listings = first.values().filter(|t| year(**t) == y && **t > start).count();
                let out: Vec<&(i64, String)> = gone.values().filter(|(t, _)| year(*t) == y).collect();
                let mut reasons: BTreeMap<&str, usize> = BTreeMap::new();
                for (_, r) in &out {
                    *reasons.entry(r.as_str()).or_default() += 1;
                }
                s.push(vec![
                    Cell::Int(y),
                    Cell::Int(listings as i64),
                    Cell::Int(out.len() as i64),
                    Cell::text(reasons.iter().map(|(r, n)| format!("{} {}", r, n)).collect::<Vec<_>>().join(", ")),
                ]);
            }
            let at_start: Vec<u64> = first.iter().filter(|(_, t)| **t == start).map(|(e, _)| *e).collect();
            let survived = at_start.iter().filter(|e| last.get(e) == Some(&end)).count();
            s.push(vec![
                Cell::text("still listed at the end"),
                Cell::Int(survived as i64),
                Cell::Int(at_start.len() as i64),
                Cell::text(format!(
                    "{:.3} of the universe at the start",
                    if at_start.is_empty() { f64::NAN } else { survived as f64 / at_start.len() as f64 }
                )),
            ]);
            Ok(s)
        }
        "actions" => {
            let splits = stored(binding, "split").map(|r| read(&r)).transpose()?.flatten().unwrap_or_default();
            let dividends = stored(binding, "dividend").map(|r| read(&r)).transpose()?.flatten().unwrap_or_default();
            let close = read(&rel_of("close")?)?.unwrap_or_default();
            let p: BTreeMap<u64, Vec<(i64, f64)>> = {
                let c = close.nums.values().next().cloned().unwrap_or_default();
                let mut m: BTreeMap<u64, Vec<(i64, f64)>> = BTreeMap::new();
                for ((e, t), x) in close.entity.iter().zip(&close.t).zip(&c) {
                    m.entry(*e).or_default().push((*t, *x));
                }
                m.values_mut().for_each(|v| v.sort_by_key(|x| x.0));
                m
            };
            // A split reconciles when the close across its ex-date moves by
            // its factor's inverse, within a quarter.
            let factor = splits.nums.values().next().cloned().unwrap_or_default();
            let (mut reconciled, mut unreconciled) = (0usize, 0usize);
            for ((e, t), f) in splits.entity.iter().zip(&splits.t).zip(&factor) {
                let Some(v) = p.get(e) else { continue };
                let k = v.partition_point(|x| x.0 < *t);
                match (k.checked_sub(1).and_then(|j| v.get(j)), v.get(k)) {
                    (Some(a), Some(b)) if a.1 > 0.0 && *f > 0.0 && (b.1 / a.1 * f - 1.0).abs() < 0.25 => reconciled += 1,
                    _ => unreconciled += 1,
                }
            }
            let years: BTreeSet<i64> = splits.t.iter().chain(&dividends.t).map(|t| year(*t)).collect();
            let mut s = Sheet::new("actions", &["year", "splits", "dividends"]);
            for y in years {
                s.push(vec![
                    Cell::Int(y),
                    Cell::Int(splits.t.iter().filter(|t| year(**t) == y).count() as i64),
                    Cell::Int(dividends.t.iter().filter(|t| year(**t) == y).count() as i64),
                ]);
            }
            s.push(vec![Cell::text("splits reconciled with the closes"), Cell::Int(reconciled as i64), Cell::Int(unreconciled as i64)]);
            Ok(s)
        }
        "membership" => {
            let member = match stored(binding, "member") {
                Some(m) => read(&m)?.unwrap_or_default(),
                None => ClassRows::default(),
            };
            let close: BTreeSet<(u64, i64)> = match stored(binding, "close") {
                Some(c) => read(&c)?.map(|x| (0..x.len()).map(|i| (x.entity[i], x.t[i])).collect()).unwrap_or_default(),
                None => BTreeSet::new(),
            };
            let list = member.labels.values().next().cloned().unwrap_or_default();
            let mut by: BTreeMap<String, BTreeMap<i64, BTreeSet<u64>>> = BTreeMap::new();
            let mut without_bars: BTreeMap<String, usize> = BTreeMap::new();
            for i in 0..member.len() {
                let l = list.get(i).cloned().flatten().unwrap_or_default();
                by.entry(l.clone()).or_default().entry(member.t[i]).or_default().insert(member.entity[i]);
                if !close.contains(&(member.entity[i], member.t[i])) {
                    *without_bars.entry(l).or_default() += 1;
                }
            }
            let mut s = Sheet::new("membership", &["list", "bars", "min_size", "mean_size", "max_size", "entries", "exits", "members_without_bars"]);
            for (l, bars) in &by {
                let sizes: Vec<usize> = bars.values().map(|m| m.len()).collect();
                let (mut entries, mut exits) = (0usize, 0usize);
                let sets: Vec<&BTreeSet<u64>> = bars.values().collect();
                for w in sets.windows(2) {
                    entries += w[1].difference(w[0]).count();
                    exits += w[0].difference(w[1]).count();
                }
                s.push(vec![
                    Cell::text(l.clone()),
                    Cell::Int(sizes.len() as i64),
                    Cell::Int(sizes.iter().min().copied().unwrap_or(0) as i64),
                    Cell::num(sizes.iter().sum::<usize>() as f64 / sizes.len().max(1) as f64),
                    Cell::Int(sizes.iter().max().copied().unwrap_or(0) as i64),
                    Cell::Int(entries as i64),
                    Cell::Int(exits as i64),
                    Cell::Int(without_bars.get(l).copied().unwrap_or(0) as i64),
                ]);
            }
            Ok(s)
        }
        "chains" => {
            let mut s = Sheet::new("chains", &["underlying", "note"]);
            s.push(vec![Cell::Null, Cell::text("the binding holds no option chain: options come with M-7")]);
            Ok(s)
        }
        other => Err(Error::Malformed(format!("`{}` is not a feed statistic ({})", other, STATISTICS.join(", ")))),
    }
}
