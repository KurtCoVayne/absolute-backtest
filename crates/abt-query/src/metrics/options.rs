//! The options metrics of Q-3.1 (ERRATA E-73), from the run's holdings,
//! fills and events:
//! - the premium paid and collected, and the exercises, assignments and
//!   expiries;
//! - the days to expiry at entry and at exit;
//! - the book's net Greeks at every barrier an option is held;
//! - the realised against the implied volatility of each underlying traded;
//! - the P&L of the options carried over each bar, attributed to Δ, Γ, Θ
//!   and V by the discrete Taylor expansion, with the residual.
//!
//! An option is an instrument whose holdings carry option columns: a
//! contract opened and closed between two barriers is never held at one and
//! is not counted.

use std::collections::{BTreeMap, BTreeSet};

use crate::run::{HoldingRow, Run};
use crate::time::DAY;

/// The year of K-2.5's τ (ACT/365), in nanoseconds.
const YEAR: f64 = 365.0 * DAY as f64;

#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct OptionsMetrics {
    /// Σ q · price · multiplier over the option fills that buy, and over
    /// those that sell.
    pub premium_paid: f64,
    pub premium_collected: f64,
    pub exercises: usize,
    pub assignments: usize,
    /// Positions that expired without exercise or assignment.
    pub expiries: usize,
    /// The mean calendar days from a fill's day to the contract's expiry
    /// date, over the fills that open a position, and over the fills and
    /// events that close one.
    pub days_to_expiry_entry: Option<f64>,
    pub days_to_expiry_exit: Option<f64>,
    /// The book's net Greeks at each barrier an option is held.
    pub greeks: Vec<NetGreeks>,
    /// Per underlying of the options held.
    pub volatility: Vec<UnderlyingVolatility>,
    pub attribution: Attribution,
}

/// The book's options at a barrier: Σ q · m · Δ · S (the delta in money),
/// Σ q · m · Γ, Σ q · m · V (per unit of volatility) and Σ q · m · Θ (per
/// year), over the holdings with Greeks.
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct NetGreeks {
    pub t: i64,
    pub delta_money: f64,
    pub gamma: f64,
    pub vega: f64,
    pub theta: f64,
}

/// An underlying's realised volatility, the annualised standard deviation
/// of its log returns between consecutive barriers at which an option on it
/// was held, and the mean implied volatility of those options at those
/// barriers.
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct UnderlyingVolatility {
    pub underlying: u64,
    pub returns: usize,
    pub realised: Option<f64>,
    pub implied: Option<f64>,
}

/// The P&L of the options carried over a bar (held at both its barriers,
/// with the multiplier unchanged), q · m · (V₁ − V₀), and its discrete
/// Taylor expansion at the first barrier's Greeks: Δ · dS, ½ · Γ · dS²,
/// Θ · dt and V · dσ, and the residual, P&L less the four. A bar without
/// the Greeks, the underlying's prices or the marks is unattributed.
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Attribution {
    pub pnl: f64,
    pub delta: f64,
    pub gamma: f64,
    pub theta: f64,
    pub vega: f64,
    pub residual: f64,
    /// The instrument-bars attributed, and those not with their P&L.
    pub bars: usize,
    pub unattributed: usize,
    pub unattributed_pnl: f64,
}

fn day(t: i64) -> i64 {
    t.div_euclid(DAY)
}

fn mean(x: &[f64]) -> Option<f64> {
    (!x.is_empty()).then(|| x.iter().sum::<f64>() / x.len() as f64)
}

/// The options metrics of a run that held an option, `None` otherwise.
pub fn options_metrics(r: &Run, periods_per_year: f64) -> Option<OptionsMetrics> {
    let options: BTreeMap<u64, (i64, Option<u64>)> = r.holdings.iter().filter_map(|h| h.option.as_ref().map(|o| (h.instrument, (o.expiry, o.underlying)))).collect();
    if options.is_empty() {
        return None;
    }
    let mut m = OptionsMetrics::default();
    for f in r.fills.iter().filter(|f| f.instrument.is_some_and(|i| options.contains_key(&i))) {
        let premium = f.quantity.abs() * f.price * f.multiplier;
        if f.quantity > 0.0 {
            m.premium_paid += premium;
        } else {
            m.premium_collected += premium;
        }
    }
    let count = |kind: &str| r.option_events.iter().filter(|e| e.kind == kind).count();
    (m.exercises, m.assignments, m.expiries) = (count("exercise"), count("assignment"), count("option_expiry"));
    (m.days_to_expiry_entry, m.days_to_expiry_exit) = days_to_expiry(r, &options);
    m.greeks = net_greeks(r);
    m.volatility = volatility(r, periods_per_year);
    m.attribution = attribution(r);
    Some(m)
}

/// The mean days to expiry at the fills that open a position and at the
/// fills and events that close one, walked in the log's order per contract.
fn days_to_expiry(r: &Run, options: &BTreeMap<u64, (i64, Option<u64>)>) -> (Option<f64>, Option<f64>) {
    // (seq, t, instrument, quantity): a closing event's quantity is the
    // position it ends.
    let mut steps: Vec<(u64, i64, u64, Option<f64>)> = r
        .fills
        .iter()
        .filter_map(|f| f.instrument.filter(|i| options.contains_key(i)).map(|i| (f.seq, f.t, i, Some(f.quantity))))
        .collect();
    steps.extend(
        r.option_events
            .iter()
            .filter(|e| e.kind != "adjustment")
            .filter_map(|e| e.instrument.filter(|i| options.contains_key(i)).map(|i| (e.seq, e.t, i, None))),
    );
    steps.sort_by_key(|s| s.0);
    let (mut entries, mut exits) = (Vec::new(), Vec::new());
    let mut position: BTreeMap<u64, f64> = BTreeMap::new();
    for (_, t, i, q) in steps {
        let dte = (day(options[&i].0) - day(t)).max(0) as f64;
        let before = position.get(&i).copied().unwrap_or(0.0);
        let after = q.map(|q| before + q).unwrap_or(0.0);
        let after = if after.abs() < 1e-9 { 0.0 } else { after };
        if before != 0.0 && (after == 0.0 || after.signum() != before.signum()) {
            exits.push(dte);
        }
        if after != 0.0 && (before == 0.0 || after.signum() != before.signum()) {
            entries.push(dte);
        }
        position.insert(i, after);
    }
    (mean(&entries), mean(&exits))
}

/// The book's net Greeks at each barrier an option is held.
fn net_greeks(r: &Run) -> Vec<NetGreeks> {
    let mut by_t: BTreeMap<i64, NetGreeks> = BTreeMap::new();
    for h in &r.holdings {
        let Some(o) = &h.option else { continue };
        let e = by_t.entry(h.t).or_insert_with(|| NetGreeks { t: h.t, ..Default::default() });
        let (Some(g), Some(s)) = (o.greeks, o.spot) else { continue };
        let qm = h.position * o.multiplier;
        e.delta_money += qm * g[1] * s;
        e.gamma += qm * g[2];
        e.theta += qm * g[3];
        e.vega += qm * g[4];
    }
    by_t.into_values().collect()
}

/// The barriers of the run, in order, and each one's place.
fn barriers(r: &Run) -> BTreeMap<i64, usize> {
    let ts: BTreeSet<i64> = r.book.iter().map(|b| b.t).collect();
    ts.into_iter().enumerate().map(|(i, t)| (t, i)).collect()
}

/// Each underlying's realised volatility over the barriers an option on it
/// was held, against the options' mean implied volatility there.
fn volatility(r: &Run, periods_per_year: f64) -> Vec<UnderlyingVolatility> {
    let place = barriers(r);
    let mut spots: BTreeMap<u64, BTreeMap<i64, f64>> = BTreeMap::new();
    let mut ivs: BTreeMap<u64, Vec<f64>> = BTreeMap::new();
    for h in &r.holdings {
        let Some(o) = &h.option else { continue };
        let Some(u) = o.underlying else { continue };
        if let Some(s) = o.spot.filter(|s| *s > 0.0) {
            spots.entry(u).or_default().insert(h.t, s);
        }
        if let Some(v) = o.greeks.map(|g| g[0]).filter(|v| v.is_finite()) {
            ivs.entry(u).or_default().push(v);
        }
    }
    let unds: BTreeSet<u64> = spots.keys().chain(ivs.keys()).copied().collect();
    unds.into_iter()
        .map(|u| {
            let s: Vec<(i64, f64)> = spots.get(&u).map(|m| m.iter().map(|(t, s)| (*t, *s)).collect()).unwrap_or_default();
            // Log returns between barriers next to each other in the run.
            let rets: Vec<f64> = s
                .windows(2)
                .filter(|w| matches!((place.get(&w[0].0), place.get(&w[1].0)), (Some(a), Some(b)) if b == &(a + 1)))
                .map(|w| (w[1].1 / w[0].1).ln())
                .collect();
            let realised = (rets.len() >= 2).then(|| {
                let mu = rets.iter().sum::<f64>() / rets.len() as f64;
                let var = rets.iter().map(|x| (x - mu).powi(2)).sum::<f64>() / (rets.len() - 1) as f64;
                (var * periods_per_year).sqrt()
            });
            UnderlyingVolatility {
                underlying: u,
                returns: rets.len(),
                realised,
                implied: ivs.get(&u).and_then(|v| mean(v)),
            }
        })
        .collect()
}

/// The P&L of the options carried over each bar and its Taylor expansion.
fn attribution(r: &Run) -> Attribution {
    let place = barriers(r);
    let mut a = Attribution::default();
    let mut by_instrument: BTreeMap<u64, Vec<&HoldingRow>> = BTreeMap::new();
    for h in r.holdings.iter().filter(|h| h.option.is_some()) {
        by_instrument.entry(h.instrument).or_default().push(h);
    }
    for rows in by_instrument.values() {
        for w in rows.windows(2) {
            let (h0, h1) = (w[0], w[1]);
            if !matches!((place.get(&h0.t), place.get(&h1.t)), (Some(x), Some(y)) if *y == x + 1) {
                continue;
            }
            let (Some(o0), Some(o1)) = (&h0.option, &h1.option) else { continue };
            let (Some(v0), Some(v1)) = (h0.mark, h1.mark) else { continue };
            let qm = h0.position * o0.multiplier;
            if o0.multiplier != o1.multiplier {
                // An adjustment changed the contract over the bar.
                a.unattributed += 1;
                continue;
            }
            let pnl = qm * (v1 - v0);
            a.pnl += pnl;
            match (o0.greeks, o0.spot, o1.spot, o1.greeks) {
                (Some(g), Some(s0), Some(s1), Some(g1)) => {
                    let (ds, dsigma, dt) = (s1 - s0, g1[0] - g[0], (h1.t - h0.t) as f64 / YEAR);
                    let (delta, gamma, theta, vega) = (qm * g[1] * ds, qm * 0.5 * g[2] * ds * ds, qm * g[3] * dt, qm * g[4] * dsigma);
                    a.delta += delta;
                    a.gamma += gamma;
                    a.theta += theta;
                    a.vega += vega;
                    a.residual += pnl - (delta + gamma + theta + vega);
                    a.bars += 1;
                }
                _ => {
                    a.unattributed += 1;
                    a.unattributed_pnl += pnl;
                }
            }
        }
    }
    a
}
