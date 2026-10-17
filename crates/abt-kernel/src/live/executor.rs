//! `BrokerExecutor` (K-6.4): the executor of `live-broker`, a `Broker`
//! behind the `Executor` trait. The bar's own events (corporate actions,
//! expiries, settlements), the marks, the margin and the book's records are
//! the simulation's (`SimExecutor`, whose book this is); what differs is the
//! orders. `on_decisions` applies the limits (K-3.5) as the simulation
//! does, sizes each decision at the decision bar's marks and sends it,
//! cancelling or replacing the order working on its instrument; `fill`
//! takes the broker's execution reports, so the book moves by the broker's
//! fills at the broker's prices and commissions and no simulated cost is
//! applied. Reconciliation runs at every session open and on a reconnect,
//! after the bar's own events and before its marks. A run's event log is
//! the same log, so its book, its outputs and its queries are any run's
//! (K-7.1).

use std::collections::BTreeMap;

use super::broker::{Broker, BrokerEvent, OrderRequest, Report};
use super::deployment::Deployment;
use super::lifecycle::{Lifecycle, LiveOrder, OrderState};
use super::reconcile::{self, Policy};
use crate::kernel::events::{EventKind, Fate, OrderId, RunEvent};
use crate::kernel::time;
use crate::kernel::{Ctor, Decision, ExecConfig, FillRecord, Kernel, Lot, OnLeverage, OnRuin, RunError, RunResult, RunWarning, Sym, Value};
use crate::sim::{Executor, Expected, SimExecutor};

/// The executor of a `live-broker` run.
pub struct BrokerExecutor {
    /// The book, the bar's own events and the records; never its fills.
    sim: SimExecutor,
    broker: Box<dyn Broker>,
    deployment: Deployment,
    orders: Lifecycle,
    next_order: OrderId,
    /// The calendar day of the last bar opened: a bar of a new day is a
    /// session's open.
    last_day: Option<i64>,
    last_bar: Option<i64>,
    /// The broker reported a reconnect: reconcile at the next bar.
    reconnected: bool,
    connected: bool,
    disconnected_at: Option<i64>,
    disconnects: usize,
    /// The reconciliations that found a difference and adopted the
    /// broker's account, or warned of it.
    adopted: Vec<String>,
    warned: Vec<String>,
    /// Reports the lifecycle did not admit (an order it never sent, a
    /// transition it has not), ignored.
    ignored: Vec<String>,
}

/// A decision sized as an order at its decision bar.
struct Sized {
    decision: Decision,
    rule: usize,
    id: OrderId,
    sym: Sym,
    quantity: f64,
    reference: Option<f64>,
    reduces: bool,
}

impl BrokerExecutor {
    pub fn new(cfg: ExecConfig, broker: Box<dyn Broker>, deployment: Deployment) -> BrokerExecutor {
        BrokerExecutor {
            sim: SimExecutor::new(cfg),
            broker,
            deployment,
            orders: Lifecycle::default(),
            next_order: 0,
            last_day: None,
            last_bar: None,
            reconnected: false,
            connected: true,
            disconnected_at: None,
            disconnects: 0,
            adopted: Vec::new(),
            warned: Vec::new(),
            ignored: Vec::new(),
        }
    }

    /// The run's orders through their lifecycle.
    pub fn orders(&self) -> &Lifecycle {
        &self.orders
    }

    pub fn deployment(&self) -> &Deployment {
        &self.deployment
    }

    /// A decision as an order at bar `t`: its signed quantity against the
    /// book, at the bar's mark; none when it cannot be routed (dropped,
    /// with the reason).
    #[allow(clippy::too_many_arguments)]
    fn size(&mut self, k: &mut Kernel, t: i64, d: Decision, rule: usize, id: OrderId, base: f64, result: &mut RunResult) -> Option<Sized> {
        let sym = d.equity;
        let drop = |d: Decision, reason: String| RunEvent::new(t, Some(sym), EventKind::Dropped { decided: t, decision: d, reason }).rule(rule).order(id);
        if matches!(d.ctor, Ctor::Combo | Ctor::Exercise) {
            let what = if d.ctor == Ctor::Combo { "a combo" } else { "an exercise" };
            self.sim
                .record(result, drop(d, format!("{} is not routed to the broker: the interface sends single orders (ERRATA E-82)", what)));
            return None;
        }
        let pos = self.sim.book_ref().position(sym);
        let m = k.multiplier(sym);
        let price = k.bar_price(sym, t);
        let lot = self.sim.cfg().lot;
        let round = |x: f64| match lot {
            Lot::Whole => x.trunc(),
            Lot::Fractional => x,
        };
        let quantity = match (d.ctor, price) {
            (Ctor::Trade, _) => round(d.amount),
            (Ctor::HoldQuantity, _) => round(d.amount) - pos,
            (Ctor::HoldWeight, Some(p)) => round(d.amount * base / (p * m)) - pos,
            (Ctor::HoldWeight, None) if d.amount == 0.0 => -pos,
            _ => {
                let why = format!("no price for {} at {}", k.symbols.name(sym), time::format_timestamp(t));
                self.sim.record(result, drop(d, why));
                return None;
            }
        };
        let reference = price.or_else(|| k.last_price.get(&sym).copied());
        let reduces = pos != 0.0 && (pos + quantity) * pos >= 0.0 && (pos + quantity).abs() < pos.abs();
        Some(Sized {
            decision: d,
            rule,
            id,
            sym,
            quantity,
            reference,
            reduces,
        })
    }

    /// Past the leverage field, the book the working orders and this one
    /// would leave, valued at the bar's marks and this order at the price it
    /// was sized at: an estimate, the broker's fills being unknown until
    /// they come (ERRATA E-82). What it would be, if past.
    fn leverage(&self, k: &mut Kernel, t: i64, sym: Sym, quantity: f64, p: f64) -> Option<String> {
        let book = self.sim.book_ref();
        let mut projected: BTreeMap<Sym, f64> = book.positions.clone();
        let mut cash = book.cash;
        // The order working on the instrument is the one this replaces.
        for o in self.orders.orders().filter(|o| o.state.working() && o.instrument != sym) {
            let left = o.quantity - o.filled;
            let price = k.price_at(o.instrument, t).unwrap_or(0.0);
            *projected.entry(o.instrument).or_default() += left;
            cash -= self.sim.traded_cash(k, o.instrument, left, price);
        }
        let after = self.sim.fill_reference(k, sym, p);
        *projected.entry(sym).or_default() += quantity;
        cash -= self.sim.traded_cash(k, sym, quantity, p);
        let (mut gross, mut net) = (0.0, cash);
        for (&s, &q) in &projected {
            let m = k.multiplier(s);
            if s == sym {
                gross += q.abs() * p * m;
                net += self.sim.value_at(k, s, q, p, after);
            } else {
                let price = k.price_at(s, t).unwrap_or(0.0);
                gross += q.abs() * price * m;
                net += self.sim.position_value(k, s, q, price);
            }
        }
        let mut liquid = cash;
        for r in &book.receivables {
            net += r.cash;
            liquid += r.cash;
        }
        let cfg = self.sim.cfg();
        let max_gross = cfg.max_gross.max(1.0);
        let tol = 1e-9 * (1.0 + net.abs());
        let breach = if cfg.compounding {
            liquid < -(max_gross - 1.0) * net.max(0.0) - tol || gross > max_gross * net + tol
        } else {
            gross > max_gross * cfg.initial_cash + tol
        };
        breach.then(|| {
            format!(
                "would exceed {}x equity at the bar's marks: cash {:.2}, gross {:.2} against equity {:.2}, the order valued at the price it was sized at",
                max_gross, cash, gross, net
            )
        })
    }

    /// Sends a sized order, replacing or cancelling the order working on
    /// its instrument (one works per instrument).
    fn route(&mut self, k: &mut Kernel, t: i64, s: Sized, result: &mut RunResult) -> Result<(), RunError> {
        let Sized {
            decision: d,
            rule,
            id,
            sym,
            quantity,
            reference,
            reduces,
        } = s;
        let working = self.orders.working_on(sym).map(|o| (o.broker_id.clone(), o.order_id, o.rule, o.decided));
        let superseded = |me: &mut BrokerExecutor, result: &mut RunResult, (bid, oid, orule, odecided): (String, OrderId, usize, i64)| {
            me.orders.superseded(&bid, t);
            me.sim
                .record(result, RunEvent::new(t, Some(sym), EventKind::Replaced { decided: odecided, by: id }).rule(orule).order(oid));
        };
        if quantity == 0.0 {
            // The position meets the decision: what works on the instrument
            // is cancelled.
            if let Some(w) = working {
                if self.broker.cancel(&w.0).is_ok() {
                    superseded(self, result, w);
                }
            }
            self.sim.record(result, RunEvent::new(t, Some(sym), EventKind::Satisfied { decided: t }).rule(rule).order(id));
            return Ok(());
        }
        if !reduces {
            if let Some(breach) = self.leverage(k, t, sym, quantity, reference.unwrap_or(0.0)) {
                match self.sim.cfg().on_leverage {
                    OnLeverage::Halt => {
                        return Err(RunError::Risk {
                            t,
                            rule: k.prog.rule_label(rule),
                            decision: result.describe_decision(&d),
                            message: format!("leverage: the order {}", breach),
                        });
                    }
                    OnLeverage::Reject => {
                        let reason = format!("rejected, the order {}", breach);
                        self.sim
                            .record(result, RunEvent::new(t, Some(sym), EventKind::Dropped { decided: t, decision: d, reason }).rule(rule).order(id));
                        return Ok(());
                    }
                    OnLeverage::Allow => {}
                }
            }
        }
        let request = OrderRequest {
            client_id: id,
            instrument: k.symbols.name(sym).to_string(),
            quantity,
            order: d.order,
            tif: self.deployment.tif_for(&d.order),
            legs: Vec::new(),
            multiplier: k.multiplier(sym),
            reference,
            destination: self.deployment.destination.clone(),
        };
        let sent = match &working {
            Some((bid, ..)) => self.broker.replace(bid, request),
            None => self.broker.send(request),
        };
        match sent {
            Ok(bid) => {
                if let Some(w) = working {
                    superseded(self, result, w);
                }
                self.orders
                    .sent(LiveOrder {
                        broker_id: bid,
                        order_id: id,
                        instrument: sym,
                        decision: d,
                        rule,
                        decided: t,
                        quantity,
                        filled: 0.0,
                        state: OrderState::Sent,
                        history: Vec::new(),
                    })
                    .map_err(RunError::Internal)?;
                self.sim.note_requested(quantity.abs());
            }
            Err(e) => {
                let reason = format!("the broker refused the order: {}", e);
                self.sim
                    .record(result, RunEvent::new(t, Some(sym), EventKind::Rejected { decided: t, decision: d, reason }).rule(rule).order(id));
            }
        }
        Ok(())
    }

    /// What the broker reported since it was last asked, at bar `tn`: its
    /// fills move the book, its ends end the orders, its session's state is
    /// kept.
    fn drain(&mut self, k: &mut Kernel, tn: i64, result: &mut RunResult) {
        for e in self.broker.events() {
            match e {
                BrokerEvent::Execution { order, report } => self.report(k, tn, &order, report, result),
                // Reconciliation reads the broker's snapshot.
                BrokerEvent::Position { .. } | BrokerEvent::Cash { .. } => {}
                BrokerEvent::Disconnect => {
                    if self.connected {
                        self.disconnects += 1;
                        self.disconnected_at = Some(tn);
                    }
                    self.connected = false;
                }
                BrokerEvent::Reconnect => {
                    if !self.connected {
                        self.reconnected = true;
                    }
                    self.connected = true;
                    self.disconnected_at = None;
                }
            }
        }
    }

    /// One execution report of order `id`.
    fn report(&mut self, k: &mut Kernel, tn: i64, id: &str, report: Report, result: &mut RunResult) {
        let Some(before) = self.orders.get(id).map(|o| o.state) else {
            self.ignored.push(format!("the broker reported `{}` of order {}, which the run did not send", report.name(), id));
            return;
        };
        let o = match self.orders.report(id, tn, &report) {
            Ok(o) => o.clone(),
            Err(e) => {
                self.ignored.push(e);
                return;
            }
        };
        let sym = o.instrument;
        match report {
            Report::New => {}
            Report::Partial { quantity, price, commission } | Report::Filled { quantity, price, commission } => {
                let m = k.multiplier(sym);
                let reference = self.sim.fill_reference(k, sym, price);
                let flow = self.sim.traded_cash(k, sym, quantity, price) + commission;
                let participation = k.bar_volume(sym, tn).filter(|v| *v > 0.0).map(|v| quantity.abs() / v);
                let working = o.state == OrderState::Partial;
                let fill = FillRecord {
                    t: tn,
                    equity: sym,
                    quantity,
                    price,
                    at_last_price: false,
                    commission,
                    fee: 0.0,
                    slippage: 0.0,
                    impact: 0.0,
                    participation: participation.unwrap_or(0.0),
                    partial: working,
                    forced: false,
                    spread: 0.0,
                    reference,
                    broker: Some(id.to_string()),
                };
                let remainder = working.then(|| (o.remaining(), Fate::Working));
                self.sim
                    .record(result, RunEvent::new(tn, Some(sym), EventKind::Fill { fill, remainder }).rule(o.rule).order(o.order_id).cash(-flow));
                self.sim.note_traded(tn, quantity.abs() * price * m);
                self.sim.note_filled(quantity.abs(), participation);
                k.last_price.insert(sym, price);
                let fill_rel = k.rel_id("fill");
                k.stores[fill_rel].insert(tn, vec![Value::Equity(sym), Value::Time(tn), Value::Num(quantity), Value::Num(price)]);
                result.costs.commissions += commission;
                result.costs.turnover += quantity.abs() * price * m;
            }
            // The end of a cancel the run asked for: its outcome is logged.
            Report::Cancelled { .. } | Report::Expired { .. } if before == OrderState::Replaced => {}
            Report::Cancelled { reason } => {
                let reason = format!("cancelled by the broker: {}", reason);
                self.sim.record(
                    result,
                    RunEvent::new(
                        tn,
                        Some(sym),
                        EventKind::Dropped {
                            decided: o.decided,
                            decision: o.decision,
                            reason,
                        },
                    )
                    .rule(o.rule)
                    .order(o.order_id),
                );
            }
            Report::Rejected { reason } => {
                let reason = format!("the broker rejected the order: {}", reason);
                self.sim.record(
                    result,
                    RunEvent::new(
                        tn,
                        Some(sym),
                        EventKind::Rejected {
                            decided: o.decided,
                            decision: o.decision,
                            reason,
                        },
                    )
                    .rule(o.rule)
                    .order(o.order_id),
                );
            }
            Report::Expired { reason } => {
                let reason = format!("expired at the broker: {}", reason);
                self.sim.record(
                    result,
                    RunEvent::new(
                        tn,
                        Some(sym),
                        EventKind::ExpiredOrder {
                            decided: o.decided,
                            decision: o.decision,
                            reason,
                        },
                    )
                    .rule(o.rule)
                    .order(o.order_id),
                );
            }
        }
    }

    /// The book against the broker's account at `event` (`session_open`,
    /// `reconnect`), under the deployment's policy; whether the book
    /// changed.
    fn reconcile(&mut self, k: &mut Kernel, t: i64, event: &str, result: &mut RunResult) -> Result<bool, RunError> {
        let stop = |message: String| RunError::Risk {
            t,
            rule: "(reconciliation)".into(),
            decision: format!("({})", event),
            message,
        };
        let snapshot = self.broker.snapshot().map_err(|e| stop(format!("the broker's account could not be read: {}", e)))?;
        let diff = reconcile::compare(self.sim.book_ref(), &k.symbols, &snapshot, self.deployment.cash_tolerance);
        if diff.is_empty() {
            return Ok(false);
        }
        let policy = self.deployment.reconcile;
        let what = format!("at the {} of {}: {}", event.replace('_', " "), time::format_timestamp(t), diff.describe(&k.symbols));
        let adopted = policy == Policy::Adopt && diff.unknown.is_empty();
        let mut e = RunEvent::new(
            t,
            None,
            EventKind::Reconciliation {
                policy: policy.name().into(),
                at: event.into(),
                positions: diff.positions.clone(),
                cash: diff.cash,
                adopted,
            },
        );
        if adopted && diff.cash_differs {
            e = e.cash(snapshot.cash - self.sim.book_ref().cash);
        }
        self.sim.record(result, e);
        match policy {
            Policy::Halt => Err(stop(format!("the book and the broker's account differ {} (reconcile halt)", what))),
            Policy::Adopt if !adopted => Err(stop(format!("the broker holds what the book cannot adopt {}", what))),
            Policy::Adopt => {
                self.adopted.push(what);
                Ok(true)
            }
            Policy::Warn => {
                self.warned.push(what);
                Ok(false)
            }
        }
    }
}

impl Executor for BrokerExecutor {
    fn open_bar(&mut self, k: &mut Kernel, t: i64, result: &mut RunResult) -> Result<(), RunError> {
        let mut changed = self.sim.open_actions(k, t, result);
        self.last_bar = Some(t);
        if let (false, Some(since), Some(limit)) = (self.connected, self.disconnected_at, self.deployment.deadline_session) {
            if t - since > limit {
                return Err(RunError::Risk {
                    t,
                    rule: "(broker)".into(),
                    decision: "(session)".into(),
                    message: format!(
                        "the broker's session has been down since {}, longer than the deployment's session deadline of {} s",
                        time::format_timestamp(since),
                        time::secs_f64(limit)
                    ),
                });
            }
        }
        // Reconciliation at a session's open and on a reconnect, after the
        // bar's own events (the broker's account carries them too) and
        // before its marks.
        let day = time::day_key(t);
        let event = if self.reconnected {
            Some("reconnect")
        } else if self.last_day != Some(day) {
            Some("session_open")
        } else {
            None
        };
        self.last_day = Some(day);
        if let Some(event) = event.filter(|_| self.connected) {
            self.reconnected = false;
            if self.deployment.reconciles_at(event) {
                changed |= self.reconcile(k, t, event, result)?;
            }
        }
        self.sim.open_marks(k, t, result, changed)
    }

    fn on_decisions(&mut self, k: &mut Kernel, t: i64, by_equity: &BTreeMap<Sym, Vec<(Decision, usize)>>, result: &mut RunResult) -> Result<(), RunError> {
        // Every decision becomes an order with its id, then the pre-trade
        // limits act on the bar's orders (K-3.5), as in the simulation.
        let mut pending: BTreeMap<Sym, Vec<(Decision, usize, OrderId)>> = BTreeMap::new();
        for (sym, ds) in by_equity {
            for (d, rule) in ds {
                let id = self.next_order;
                self.next_order += 1;
                self.sim
                    .record(result, RunEvent::new(t, Some(*sym), EventKind::OrderSent { decision: d.clone() }).rule(*rule).order(id));
                pending.entry(*sym).or_default().push((d.clone(), *rule, id));
            }
        }
        if !self.sim.cfg().limits.is_empty() {
            self.sim.apply_limits(k, t, &mut pending, result)?;
        }
        let orders: Vec<(Decision, usize, OrderId)> = pending.into_values().flatten().collect();
        if orders.is_empty() {
            return Ok(());
        }
        let nav = self.sim.nav(k, t);
        let (compounding, initial_cash, on_ruin) = {
            let c = self.sim.cfg();
            (c.compounding, c.initial_cash, c.on_ruin)
        };
        if nav <= 0.0 && on_ruin == OnRuin::Halt {
            let (d, rule, _) = &orders[0];
            return Err(RunError::Risk {
                t,
                rule: k.prog.rule_label(*rule),
                decision: result.describe_decision(d),
                message: format!("ruin: equity at {} is {:.2}, not positive, with orders to send", time::format_timestamp(t), nav),
            });
        }
        let base = if compounding { nav.max(0.0) } else { initial_cash };
        // Sized at the bar's marks; the orders that reduce a position are
        // sent first, as the simulation fills them first, so the leverage
        // estimate counts them before the orders that add.
        let mut sized = Vec::new();
        for (d, rule, id) in orders {
            if let Some(s) = self.size(k, t, d, rule, id, base, result) {
                sized.push(s);
            }
        }
        sized.sort_by_key(|s| !s.reduces);
        for s in sized {
            self.route(k, t, s, result)?;
        }
        Ok(())
    }

    fn fill(&mut self, k: &mut Kernel, tn: i64, result: &mut RunResult) -> Result<(), RunError> {
        self.drain(k, tn, result);
        // The executor relations at the bar: the book the broker's fills
        // left.
        let position = k.rel_id("position");
        let cash_rel = k.rel_id("cash");
        let book = self.sim.book_ref();
        for (&sym, &q) in &book.positions {
            k.stores[position].insert(tn, vec![Value::Equity(sym), Value::Time(tn), Value::Num(q)]);
        }
        k.stores[cash_rel].insert(tn, vec![Value::Time(tn), Value::Num(book.cash)]);
        Ok(())
    }

    fn finish(&mut self, k: &mut Kernel, result: &mut RunResult) {
        // What works when the run ends is cancelled: a run leaves nothing
        // working at the broker (ERRATA E-82). Then what the broker
        // reported meanwhile: a fill that crossed a cancel is a fill.
        if let Some(end) = self.last_bar {
            let working: Vec<LiveOrder> = self.orders.orders().filter(|o| o.state.working()).cloned().collect();
            for o in working {
                let reason = match self.broker.cancel(&o.broker_id) {
                    Ok(()) => "the run ended: cancelled at the broker".to_string(),
                    Err(e) => format!("the run ended with the order working at the broker, whose cancel failed: {}", e),
                };
                self.orders.superseded(&o.broker_id, end);
                self.sim.record(
                    result,
                    RunEvent::new(
                        end,
                        Some(o.instrument),
                        EventKind::Dropped {
                            decided: o.decided,
                            decision: o.decision,
                            reason,
                        },
                    )
                    .rule(o.rule)
                    .order(o.order_id),
                );
            }
            self.drain(k, end, result);
        }
        let mut warn = |bias: &str, message: String| {
            result.warnings.push(RunWarning { bias: bias.into(), message });
        };
        let counted = |n: usize, what: &str| if n == 1 { format!("1 {}", what) } else { format!("{} {}s", n, what) };
        if self.disconnects > 0 {
            let times = if self.disconnects == 1 { "once".to_string() } else { format!("{} times", self.disconnects) };
            warn("broker", format!("the broker's session went down {}; the orders sent while it was down were refused", times));
        }
        if let Some(first) = self.warned.first() {
            warn(
                "reconciliation",
                format!(
                    "the book was apart from the broker's account at {} (reconcile warn), the first {}",
                    counted(self.warned.len(), "reconciliation"),
                    first
                ),
            );
        }
        if let Some(first) = self.adopted.first() {
            warn(
                "reconciliation",
                format!(
                    "the book adopted the broker's account at {} (reconcile adopt), the first {}",
                    counted(self.adopted.len(), "reconciliation"),
                    first
                ),
            );
        }
        if let Some(first) = self.ignored.first() {
            warn("broker", format!("{} ignored, the first: {}", counted(self.ignored.len(), "broker report"), first));
        }
        self.sim.finish(k, result);
    }

    fn checkpoint(&self) -> Result<serde_json::Value, String> {
        Ok(serde_json::json!({
            "sim": self.sim.checkpoint()?,
            "orders": serde_json::to_value(&self.orders).map_err(|e| e.to_string())?,
            "next_order": self.next_order,
            "last_day": self.last_day,
            "last_bar": self.last_bar,
            "connected": self.connected,
            "disconnected_at": self.disconnected_at,
            "disconnects": self.disconnects,
        }))
    }

    /// The state back from a checkpoint: the broker is reconciled with at
    /// the next bar, as on a reconnect.
    fn restore(&mut self, state: serde_json::Value) -> Result<(), String> {
        let field = |name: &str| state.get(name).cloned().ok_or_else(|| format!("the executor's state has no `{}`", name));
        self.sim.restore(field("sim")?)?;
        self.orders = serde_json::from_value(field("orders")?).map_err(|e| format!("the orders: {}", e))?;
        self.next_order = serde_json::from_value(field("next_order")?).map_err(|e| e.to_string())?;
        self.last_day = serde_json::from_value(field("last_day")?).map_err(|e| e.to_string())?;
        self.last_bar = serde_json::from_value(field("last_bar")?).map_err(|e| e.to_string())?;
        self.connected = serde_json::from_value(field("connected")?).map_err(|e| e.to_string())?;
        self.disconnected_at = serde_json::from_value(field("disconnected_at")?).map_err(|e| e.to_string())?;
        self.disconnects = serde_json::from_value(field("disconnects")?).map_err(|e| e.to_string())?;
        self.reconnected = true;
        Ok(())
    }

    fn book(&self) -> Option<&crate::kernel::events::Book> {
        Some(self.sim.book_ref())
    }

    fn working(&self) -> Vec<Sym> {
        self.orders.working()
    }

    fn expected(&self) -> Option<&Expected> {
        Executor::expected(&self.sim)
    }
}
