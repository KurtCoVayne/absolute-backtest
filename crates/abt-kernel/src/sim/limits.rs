//! The pre-trade limits (K-3.5): each limit of `execution.limits`, in the
//! order written, acting on the bar's orders at their expected fills before
//! any is sent: a breach halts the run, rejects the increasing orders,
//! scales them to the limit, or warns, and is a `limit_triggered` event.

use super::*;

/// What the pre-trade limits read (K-5.1 "limits state"): the NAV the day
/// opened at, the peak NAV, the notional traded by bar for a trailing
/// turnover.
#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
pub struct LimitState {
    day: Option<i64>,
    pub day_open_nav: f64,
    last_nav: f64,
    pub peak_nav: f64,
    pub(super) traded: std::collections::VecDeque<(i64, f64)>,
}

impl LimitState {
    /// The NAV of a decision bar: a new calendar day opens at the last
    /// bar's NAV (the first at the initial capital).
    pub(super) fn observe(&mut self, t: i64, nav: f64, initial: f64) {
        let day = time::day_key(t);
        if self.day != Some(day) {
            self.day_open_nav = if self.day.is_some() { self.last_nav } else { initial };
            self.day = Some(day);
        }
        self.last_nav = nav;
        self.peak_nav = self.peak_nav.max(nav);
    }

    /// The notional traded in `(since, t]`, forgetting what is older.
    fn traded_since(&mut self, since: i64) -> f64 {
        while self.traded.front().is_some_and(|(at, _)| *at <= since) {
            self.traded.pop_front();
        }
        self.traded.iter().map(|(_, n)| n).sum()
    }
}

/// One order as the limits see it: at the bar's mark times the multiplier
/// (`pm`), from the position `q0` to `q1`; a combo is one of these a leg,
/// each under the decision's instrument (`key`).
#[derive(Clone, Debug)]
struct Sized {
    sym: Sym,
    key: Sym,
    combo: bool,
    index: usize,
    id: OrderId,
    rule: usize,
    q0: f64,
    q1: f64,
    pm: f64,
}

impl Sized {
    fn increasing(&self) -> bool {
        self.q1.abs() > self.q0.abs()
    }
}

/// The largest scale in [0, 1] of the increasing orders' deltas under which
/// `f` (convex in the scale) stays at most `bound`.
fn largest_scale(f: impl Fn(f64) -> f64, bound: f64) -> f64 {
    if f(1.0) <= bound {
        return 1.0;
    }
    if f(0.0) > bound {
        return 0.0;
    }
    let (mut lo, mut hi) = (0.0, 1.0);
    for _ in 0..60 {
        let mid = (lo + hi) / 2.0;
        if f(mid) <= bound {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    lo
}

impl SimExecutor {
    /// The pre-trade limits (K-3.5) on the bar's orders, at their expected
    /// fills (the bar's marks): each limit in the order written; a breach
    /// halts the run, rejects the increasing orders, scales them to the
    /// limit, or warns, and is a `limit_triggered` event. A limit never
    /// stops an order that reduces a position, except a participation
    /// limit, which is the order's size against the bar's volume.
    pub(super) fn apply_limits(&mut self, k: &mut Kernel, t: i64, pending: &mut BTreeMap<Sym, Vec<(Decision, usize, OrderId)>>, result: &mut RunResult) -> Result<(), RunError> {
        use crate::check::{LimitKind, LimitPolicy};
        let nav = self.nav(k, t);
        let capital = if self.cfg.compounding { nav.max(0.0) } else { self.cfg.initial_cash };
        let mut orders: Vec<Sized> = Vec::new();
        for (sym, ds) in pending.iter() {
            for (index, (d, rule, id)) in ds.iter().enumerate() {
                // A combo is sized leg by leg, and acted on whole (all or none).
                let legs: Vec<(Sym, Option<f64>)> = if d.ctor == Ctor::Combo {
                    d.legs.iter().map(|(s, q)| (*s, Some(*q))).collect()
                } else {
                    vec![(*sym, None)]
                };
                for (leg, delta) in legs {
                    // An order without a mark cannot be sized before its fill:
                    // the limits leave it to the executor's own checks.
                    let Some(p) = k.price_at(leg, t) else { continue };
                    let pm = p * k.multiplier(leg);
                    let q0 = self.book.position(leg);
                    let q1 = match (d.ctor, delta) {
                        (_, Some(q)) => q0 + q,
                        (Ctor::Trade, None) => q0 + d.amount,
                        (Ctor::HoldQuantity, None) => d.amount,
                        (Ctor::HoldWeight, None) if pm != 0.0 => d.amount * capital / pm,
                        (Ctor::HoldWeight | Ctor::Combo | Ctor::Exercise, None) => q0,
                    };
                    orders.push(Sized {
                        sym: leg,
                        key: *sym,
                        combo: d.ctor == Ctor::Combo,
                        index,
                        id: *id,
                        rule: *rule,
                        q0,
                        q1,
                        pm,
                    });
                }
            }
        }
        // The held positions the bar does not order, marked.
        let ordered: HashSet<Sym> = orders.iter().map(|o| o.sym).collect();
        let mut others: Vec<(f64, f64)> = Vec::new();
        for (&sym, &q) in &self.book.positions {
            if !ordered.contains(&sym) {
                if let Some(p) = k.price_at(sym, t) {
                    others.push((q, p * k.multiplier(sym)));
                }
            }
        }
        let tol = 1e-9 * capital.abs().max(1.0);
        for limit in self.cfg.limits.clone() {
            let bound = limit.bound * capital;
            // An increasing order at scale `s` of its delta; any other in full.
            let at = |o: &Sized, s: f64| if o.increasing() { o.q0 + s * (o.q1 - o.q0) } else { o.q1 };
            let gross = |os: &[Sized], s: f64| others.iter().map(|(q, pm)| q.abs() * pm).sum::<f64>() + os.iter().map(|o| at(o, s).abs() * o.pm).sum::<f64>();
            let net = |os: &[Sized], s: f64| others.iter().map(|(q, pm)| q * pm).sum::<f64>() + os.iter().map(|o| at(o, s) * o.pm).sum::<f64>();
            // The orders a breach acts on, the scale of each that meets the
            // limit (for `clamp`; none where the limit cannot be scaled to),
            // and what the run reports.
            let (affected, scale, detail): (Vec<usize>, Option<Vec<f64>>, String) = match limit.kind {
                LimitKind::Gross => {
                    let g = gross(&orders, 1.0);
                    if g <= bound + tol {
                        continue;
                    }
                    let s = largest_scale(|s| gross(&orders, s), bound);
                    let idx: Vec<usize> = (0..orders.len()).filter(|&i| orders[i].increasing()).collect();
                    (idx, Some(vec![s; orders.len()]), format!("gross {:.2} after the bar's orders against {:.2}", g, bound))
                }
                LimitKind::Net => {
                    let n = net(&orders, 1.0);
                    if n.abs() <= bound + tol {
                        continue;
                    }
                    let s = largest_scale(|s| net(&orders, s).abs(), bound);
                    let idx: Vec<usize> = (0..orders.len()).filter(|&i| orders[i].increasing() && (orders[i].q1 - orders[i].q0) * n > 0.0).collect();
                    (idx, Some(vec![s; orders.len()]), format!("net {:.2} after the bar's orders against {:.2}", n, bound))
                }
                LimitKind::Weight => {
                    let idx: Vec<usize> = (0..orders.len()).filter(|&i| orders[i].increasing() && (orders[i].q1 * orders[i].pm).abs() > bound + tol).collect();
                    if idx.is_empty() {
                        continue;
                    }
                    let scales: Vec<f64> = orders
                        .iter()
                        .map(|o| {
                            let cap = if o.pm != 0.0 { bound / o.pm.abs() } else { o.q1.abs() };
                            let target = o.q1.signum() * cap.min(o.q1.abs());
                            if o.q1 != o.q0 {
                                ((target - o.q0) / (o.q1 - o.q0)).clamp(0.0, 1.0)
                            } else {
                                1.0
                            }
                        })
                        .collect();
                    (idx, Some(scales), format!("a position above {:.2} per name", bound))
                }
                LimitKind::Names => {
                    let held = others.iter().filter(|(q, _)| *q != 0.0).count() + orders.iter().filter(|o| o.q1 != 0.0).count();
                    if held as f64 <= limit.bound {
                        continue;
                    }
                    let idx: Vec<usize> = (0..orders.len()).filter(|&i| orders[i].q0 == 0.0 && orders[i].q1 != 0.0).collect();
                    (idx, None, format!("{} names after the bar's orders against {}", held, limit.bound))
                }
                LimitKind::Loss => {
                    let loss = self.limit_state.day_open_nav - nav;
                    if loss <= bound + tol {
                        continue;
                    }
                    let idx: Vec<usize> = (0..orders.len()).filter(|&i| orders[i].increasing()).collect();
                    (idx, None, format!("the day's loss {:.2} against {:.2}", loss, bound))
                }
                LimitKind::Drawdown => {
                    let floor = (1.0 - limit.bound) * self.limit_state.peak_nav;
                    if nav >= floor - tol {
                        continue;
                    }
                    let idx: Vec<usize> = (0..orders.len()).filter(|&i| orders[i].increasing()).collect();
                    (idx, None, format!("NAV {:.2} below {:.2}, {} under the peak {:.2}", nav, floor, limit.bound, self.limit_state.peak_nav))
                }
                LimitKind::Turnover => {
                    let period = match limit.period.as_deref() {
                        Some("day") => crate::ir::Duration { months: 0, days: 1, nanos: 0 },
                        Some("week") => crate::ir::Duration { months: 0, days: 7, nanos: 0 },
                        Some("quarter") => crate::ir::Duration { months: 3, days: 0, nanos: 0 },
                        Some("year") => crate::ir::Duration { months: 12, days: 0, nanos: 0 },
                        _ => crate::ir::Duration { months: 1, days: 0, nanos: 0 },
                    };
                    let traded = self.limit_state.traded_since(time::sub_duration(t, period));
                    let turnover = |s: f64| traded + orders.iter().map(|o| (at(o, s) - o.q0).abs() * o.pm.abs()).sum::<f64>();
                    let total = turnover(1.0);
                    if total <= bound + tol {
                        continue;
                    }
                    let s = largest_scale(turnover, bound);
                    let idx: Vec<usize> = (0..orders.len()).filter(|&i| orders[i].increasing()).collect();
                    (
                        idx,
                        Some(vec![s; orders.len()]),
                        format!("turnover {:.2} over the trailing {} against {:.2}", total, limit.period.as_deref().unwrap_or("month"), bound),
                    )
                }
                LimitKind::Participation => {
                    let mut idx = Vec::new();
                    let mut scales = vec![1.0; orders.len()];
                    for (i, o) in orders.iter().enumerate() {
                        let Some(v) = k.bar_volume(o.sym, t) else { continue };
                        let cap = limit.bound * v;
                        let size = (o.q1 - o.q0).abs();
                        if size > cap * (1.0 + 1e-12) {
                            idx.push(i);
                            scales[i] = if size > 0.0 { cap / size } else { 1.0 };
                        }
                    }
                    if idx.is_empty() {
                        continue;
                    }
                    (idx, Some(scales), format!("an order above {} of its bar's volume", limit.bound))
                }
            };
            let ids: Vec<OrderId> = affected.iter().map(|&i| orders[i].id).collect();
            // A limit that cannot be scaled to rejects (the checker admits
            // `clamp` only where it can, K-3.5).
            let policy = match (limit.policy, &scale) {
                (LimitPolicy::Clamp, None) => LimitPolicy::Reject,
                (p, _) => p,
            };
            let reason = format!("limit `{}`: {}", limit, detail);
            match policy {
                LimitPolicy::Halt => {
                    let o = affected.first().map(|&i| &orders[i]);
                    return Err(RunError::Risk {
                        t,
                        rule: o.map(|o| k.prog.rule_label(o.rule)).unwrap_or_else(|| "(limits)".into()),
                        decision: o
                            .and_then(|o| pending.get(&o.key).and_then(|ds| ds.get(o.index)))
                            .map(|(d, ..)| result.describe_decision(d))
                            .unwrap_or_default(),
                        message: reason,
                    });
                }
                LimitPolicy::Warn => {}
                LimitPolicy::Reject => {
                    let gone: HashSet<OrderId> = ids.iter().copied().collect();
                    self.reject_orders(t, &gone, &reason, pending, &mut orders, result);
                }
                LimitPolicy::Clamp => {
                    let scales = scale.unwrap_or_default();
                    // A combo is all or none: a limit that would scale it
                    // rejects it.
                    let combos: HashSet<OrderId> = affected.iter().filter(|&&i| orders[i].combo).map(|&i| orders[i].id).collect();
                    for &i in &affected {
                        if orders[i].combo {
                            continue;
                        }
                        let s = scales.get(i).copied().unwrap_or(1.0);
                        let o = &mut orders[i];
                        let q = o.q0 + s * (o.q1 - o.q0);
                        o.q1 = q;
                        if let Some((d, ..)) = pending.get_mut(&o.key).and_then(|ds| ds.get_mut(o.index)) {
                            d.amount = match d.ctor {
                                Ctor::Trade => q - o.q0,
                                Ctor::HoldQuantity => q,
                                Ctor::HoldWeight if capital != 0.0 => q * o.pm / capital,
                                Ctor::HoldWeight => 0.0,
                                Ctor::Combo | Ctor::Exercise => d.amount,
                            };
                        }
                    }
                    if !combos.is_empty() {
                        self.reject_orders(t, &combos, &reason, pending, &mut orders, result);
                    }
                }
            }
            let fired = EventKind::LimitTriggered {
                limit: limit.to_string(),
                policy: policy.name().to_string(),
                orders: ids,
                detail,
            };
            self.record(result, RunEvent::new(t, None, fired));
        }
        let mut positions: Vec<(Sym, f64, f64)> = orders.iter().map(|o| (o.sym, o.q1, o.pm)).collect();
        for (&sym, &q) in &self.book.positions {
            if !orders.iter().any(|o| o.sym == sym) {
                if let Some(p) = k.price_at(sym, t) {
                    positions.push((sym, q, p * k.multiplier(sym)));
                }
            }
        }
        self.expected = Expected { t, capital, positions };
        Ok(())
    }

    /// Rejects the bar's orders `gone` (K-3.5 `reject`): each is a
    /// `rejected` event, and leaves the bar's orders and the limits' view.
    fn reject_orders(&mut self, t: i64, gone: &HashSet<OrderId>, reason: &str, pending: &mut BTreeMap<Sym, Vec<(Decision, usize, OrderId)>>, orders: &mut Vec<Sized>, result: &mut RunResult) {
        for o in orders.iter().filter(|o| gone.contains(&o.id)) {
            if let Some(ds) = pending.get_mut(&o.key) {
                if let Some(pos) = ds.iter().position(|(_, _, id)| *id == o.id) {
                    let (d, rule, id) = ds.remove(pos);
                    let rejected = EventKind::Rejected {
                        decided: t,
                        decision: d,
                        reason: reason.to_string(),
                    };
                    self.record(result, RunEvent::new(t, Some(o.key), rejected).rule(rule).order(id));
                }
            }
        }
        pending.retain(|_, ds| !ds.is_empty());
        orders.retain(|o| !gone.contains(&o.id));
        for o in orders.iter_mut() {
            if let Some(i) = pending.get(&o.key).and_then(|ds| ds.iter().position(|(_, _, id)| *id == o.id)) {
                o.index = i;
            }
        }
    }
}
