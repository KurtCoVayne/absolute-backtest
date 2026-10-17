//! Activation statistics (Q-7), from a run's decisions in the data layer:
//! per rule, its firing rate per bar and per instance, the autocorrelation
//! of its firing and the drift of its rate across calendar years; per pair
//! of rules, the 2×2 table over the instances, phi with its block-bootstrap
//! interval (`coactivation`), Jaccard's index, the mutual information and
//! P(b | a), and the cross-lag co-activation P(b at t | a at t − k) for k ≤
//! 20; and the effective number of rules (`effective_rules`). Pairs whose
//! phi is above the threshold are candidates for the kernel's structural
//! check (`abt contract --implications`). No bare p-value: effects carry
//! intervals (Q-4.2).

use std::collections::{BTreeMap, BTreeSet};

use crate::data::{Data, Error};
use crate::plan::inputs;
use crate::run::Run;
use crate::sheet::{Cell, Sheet};
use crate::tests::{self, Param, Params};
use crate::time::{civil_from_days, day_key};

/// The phi above which a pair is a candidate for the structural check.
pub const PHI_THRESHOLD: f64 = 0.9;

/// The longest lag of the cross-lag co-activation, in decision bars.
pub const MAX_LAG: usize = 20;

/// A run's activation tables.
#[derive(Clone, Debug, serde::Serialize)]
pub struct Activation {
    pub rules: Sheet,
    pub pairs: Sheet,
    pub lags: Sheet,
    pub effective: Sheet,
}

impl Activation {
    pub fn sheets(&self) -> [&Sheet; 4] {
        [&self.rules, &self.pairs, &self.lags, &self.effective]
    }
}

fn autocorrelation(x: &[f64]) -> f64 {
    crate::metrics::autocorrelation(x, 1)
}

/// The activation statistics of `run` (its program's strategy named, for
/// the rules' columns), the bootstrap seeded by `seed`.
pub fn activation(data: &Data, run: &str, strategy: &str, seed: u64, threshold: f64) -> Result<Activation, Error> {
    let table = inputs::activations(data, run, strategy, &|_| true)?;
    let bars: Vec<i64> = Run::read(data, run)?.book.iter().map(|b| b.t).collect();
    let time = table.time.clone().unwrap_or_default();
    let rules: Vec<String> = table.columns.keys().cloned().collect();
    // Per bar, whether each rule fired on any instance.
    let mut fired_at: BTreeMap<&str, BTreeSet<i64>> = BTreeMap::new();
    for r in &rules {
        let col = &table.columns[r];
        fired_at.insert(r.as_str(), (0..time.len()).filter(|&i| col[i] == 1.0).map(|i| time[i]).collect());
    }
    let indicator = |r: &str| -> Vec<f64> { bars.iter().map(|t| if fired_at[r].contains(t) { 1.0 } else { 0.0 }).collect() };
    let year = |t: i64| civil_from_days(day_key(t)).0;
    let years: BTreeSet<i64> = bars.iter().map(|t| year(*t)).collect();
    let mut per_rule = Sheet::new(
        "activation.rules",
        &["rule", "fired_bars", "rate_per_bar", "firings", "rate_per_instance", "autocorrelation", "rate_by_year"],
    );
    for r in &rules {
        let col = &table.columns[r];
        let firings = col.iter().filter(|v| **v == 1.0).count();
        let ind = indicator(r);
        let drift: Vec<String> = years
            .iter()
            .map(|y| {
                let n = bars.iter().filter(|t| year(**t) == *y).count();
                let f = fired_at[r.as_str()].iter().filter(|t| year(**t) == *y).count();
                format!("{} {:.3}", y, f as f64 / n.max(1) as f64)
            })
            .collect();
        per_rule.push(vec![
            Cell::text(r.clone()),
            Cell::Int(fired_at[r.as_str()].len() as i64),
            Cell::num(fired_at[r.as_str()].len() as f64 / bars.len().max(1) as f64),
            Cell::Int(firings as i64),
            Cell::num(firings as f64 / time.len().max(1) as f64),
            Cell::num(autocorrelation(&ind)),
            Cell::text(drift.join(", ")),
        ]);
    }
    let mut pairs = Sheet::new(
        "activation.pairs",
        &[
            "rule_a",
            "rule_b",
            "n11",
            "n10",
            "n01",
            "n00",
            "phi",
            "phi_lo",
            "phi_hi",
            "jaccard",
            "mutual_information_bits",
            "p_b_given_a",
            "candidate",
        ],
    );
    let mut lags = Sheet::new("activation.lags", &["rule_a", "rule_b", "k", "p_b_given_a_k_bars_before"]);
    let coactivation = tests::get("coactivation");
    for (i, a) in rules.iter().enumerate() {
        for b in rules.iter().skip(i + 1) {
            let (x, y) = (&table.columns[a], &table.columns[b]);
            let mut n = [0usize; 4];
            for k in 0..x.len() {
                n[match (x[k] == 1.0, y[k] == 1.0) {
                    (true, true) => 0,
                    (true, false) => 1,
                    (false, true) => 2,
                    (false, false) => 3,
                }] += 1;
            }
            let (n11, n10, n01, n00) = (n[0] as f64, n[1] as f64, n[2] as f64, n[3] as f64);
            let total = n11 + n10 + n01 + n00;
            let denom = ((n11 + n10) * (n01 + n00) * (n11 + n01) * (n10 + n00)).sqrt();
            let phi = if denom > 0.0 { (n11 * n00 - n10 * n01) / denom } else { f64::NAN };
            let jaccard = if n11 + n10 + n01 > 0.0 { n11 / (n11 + n10 + n01) } else { f64::NAN };
            let mi: f64 = [(n11, n11 + n10, n11 + n01), (n10, n11 + n10, n10 + n00), (n01, n01 + n00, n11 + n01), (n00, n01 + n00, n10 + n00)]
                .iter()
                .filter(|(c, _, _)| *c > 0.0)
                .map(|(c, ra, cb)| (c / total) * ((c * total) / (ra * cb)).log2())
                .sum();
            // phi's interval: the coactivation test's bootstrap over bars.
            let (mut lo, mut hi) = (f64::NAN, f64::NAN);
            if let Some(t) = &coactivation {
                let mut sub = crate::tests::Table {
                    time: table.time.clone(),
                    entity: table.entity.clone(),
                    columns: BTreeMap::new(),
                };
                sub.columns.insert(a.clone(), x.clone());
                sub.columns.insert(b.clone(), y.clone());
                let mut p = Params::new();
                p.insert("rules".into(), Param::Text(format!("{},{}", a, b)));
                if let Ok(r) = t.run(&[sub], &p, seed ^ (i as u64).wrapping_mul(0x9e37_79b9_7f4a_7c15)) {
                    if let Some((l, h, _)) = r.interval {
                        (lo, hi) = (l, h);
                    }
                }
            }
            pairs.push(vec![
                Cell::text(a.clone()),
                Cell::text(b.clone()),
                Cell::Int(n[0] as i64),
                Cell::Int(n[1] as i64),
                Cell::Int(n[2] as i64),
                Cell::Int(n[3] as i64),
                Cell::num(phi),
                Cell::num(lo),
                Cell::num(hi),
                Cell::num(jaccard),
                Cell::num(mi),
                Cell::num(if n11 + n10 > 0.0 { n11 / (n11 + n10) } else { f64::NAN }),
                Cell::Bool(phi.is_finite() && phi > threshold),
            ]);
        }
    }
    // Cross-lag co-activation over the decision bars, both directions.
    for a in &rules {
        let ia = indicator(a);
        for b in rules.iter().filter(|b| *b != a) {
            let ib = indicator(b);
            for k in 1..=MAX_LAG.min(bars.len().saturating_sub(1)) {
                let fired: Vec<usize> = (k..bars.len()).filter(|&t| ia[t - k] == 1.0).collect();
                let p = if fired.is_empty() {
                    f64::NAN
                } else {
                    fired.iter().filter(|&&t| ib[t] == 1.0).count() as f64 / fired.len() as f64
                };
                lags.push(vec![Cell::text(a.clone()), Cell::text(b.clone()), Cell::Int(k as i64), Cell::num(p)]);
            }
        }
    }
    let mut effective = Sheet::new("activation.effective", &["rules", "effective_rules", "notes"]);
    let eff = tests::get("effective_rules").map(|t| t.run(std::slice::from_ref(&table), &Params::new(), seed));
    match eff {
        Some(Ok(r)) => effective.push(vec![Cell::Int(rules.len() as i64), Cell::num(r.statistic), Cell::text(r.notes.join("; "))]),
        Some(Err(e)) => effective.push(vec![Cell::Int(rules.len() as i64), Cell::Null, Cell::text(e.to_string())]),
        None => effective.push(vec![Cell::Int(rules.len() as i64), Cell::Null, Cell::text("no effective_rules test is registered")]),
    }
    Ok(Activation {
        rules: per_rule,
        pairs,
        lags,
        effective,
    })
}
