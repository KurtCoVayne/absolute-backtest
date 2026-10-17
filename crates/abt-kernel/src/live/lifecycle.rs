//! The order lifecycle (K-6.4): an order sent, acknowledged (`new`), part
//! filled, and ended filled, cancelled, rejected or expired. One order
//! works per instrument; a new decision on it cancels or replaces the one
//! working, which ends `replaced` from the run's side whatever its last
//! report (a fill that crossed the cancel is still a fill).

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::broker::{BrokerOrderId, Report};
use crate::kernel::events::OrderId;
use crate::kernel::{Decision, Sym};

/// Where an order is in its life.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum OrderState {
    /// Sent, not acknowledged yet.
    Sent,
    /// Acknowledged, nothing filled.
    New,
    /// Part filled, the rest working.
    Partial,
    Filled,
    /// Cancelled by the broker.
    Cancelled,
    Rejected,
    Expired,
    /// Superseded by a later decision on its instrument (cancelled or
    /// replaced at the run's request), or cancelled at the run's end.
    Replaced,
}

impl OrderState {
    /// Whether the order works at the broker.
    pub fn working(self) -> bool {
        matches!(self, OrderState::Sent | OrderState::New | OrderState::Partial)
    }

    /// The state a report moves the order to, or why the lifecycle does
    /// not admit it.
    pub fn after(self, r: &Report) -> Result<OrderState, String> {
        use OrderState::*;
        match (self, r) {
            (Sent, Report::New) => Ok(New),
            (Sent | New | Partial, Report::Partial { .. }) => Ok(Partial),
            (Sent | New | Partial, Report::Filled { .. }) => Ok(Filled),
            (Sent | New | Partial, Report::Cancelled { .. }) => Ok(Cancelled),
            (Sent | New, Report::Rejected { .. }) => Ok(Rejected),
            (Sent | New | Partial, Report::Expired { .. }) => Ok(Expired),
            // What a superseded order still reports: the end of its cancel,
            // or a fill that crossed it.
            (Replaced, Report::New | Report::Partial { .. } | Report::Filled { .. } | Report::Cancelled { .. } | Report::Expired { .. }) => Ok(Replaced),
            (s, r) => Err(format!("a {} order cannot report `{}`", s.name(), r.name())),
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            OrderState::Sent => "sent",
            OrderState::New => "new",
            OrderState::Partial => "partial",
            OrderState::Filled => "filled",
            OrderState::Cancelled => "cancelled",
            OrderState::Rejected => "rejected",
            OrderState::Expired => "expired",
            OrderState::Replaced => "replaced",
        }
    }
}

/// One order through its life.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LiveOrder {
    pub broker_id: BrokerOrderId,
    /// The run's id of the order (K-5.2).
    pub order_id: OrderId,
    pub instrument: Sym,
    pub decision: Decision,
    pub rule: usize,
    /// The decision bar.
    pub decided: i64,
    /// The quantity sent (signed), and what of it filled.
    pub quantity: f64,
    pub filled: f64,
    pub state: OrderState,
    /// Every state the order entered, with the bar it entered it at.
    pub history: Vec<(i64, OrderState)>,
}

impl LiveOrder {
    /// What is left to fill, unsigned.
    pub fn remaining(&self) -> f64 {
        (self.quantity - self.filled).abs()
    }
}

/// A run's orders by the broker's id, and the one working on each
/// instrument.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Lifecycle {
    orders: BTreeMap<BrokerOrderId, LiveOrder>,
    working: BTreeMap<Sym, BrokerOrderId>,
}

impl Lifecycle {
    /// An order sent at its decision bar: it works on its instrument, where
    /// no other may.
    pub fn sent(&mut self, mut o: LiveOrder) -> Result<(), String> {
        if let Some(w) = self.working.get(&o.instrument) {
            return Err(format!("order {} already works on the instrument", w));
        }
        o.state = OrderState::Sent;
        o.history = vec![(o.decided, OrderState::Sent)];
        self.working.insert(o.instrument, o.broker_id.clone());
        self.orders.insert(o.broker_id.clone(), o);
        Ok(())
    }

    pub fn get(&self, id: &str) -> Option<&LiveOrder> {
        self.orders.get(id)
    }

    /// The order working on `sym`.
    pub fn working_on(&self, sym: Sym) -> Option<&LiveOrder> {
        self.working.get(&sym).and_then(|id| self.orders.get(id))
    }

    /// The instruments with an order working, one entry an order.
    pub fn working(&self) -> Vec<Sym> {
        self.working.keys().copied().collect()
    }

    pub fn orders(&self) -> impl Iterator<Item = &LiveOrder> {
        self.orders.values()
    }

    /// A report on order `id`, received at bar `t`: the order after it.
    pub fn report(&mut self, id: &str, t: i64, r: &Report) -> Result<&LiveOrder, String> {
        let o = self
            .orders
            .get_mut(id)
            .ok_or_else(|| format!("the broker reported `{}` of order {}, which the run did not send", r.name(), id))?;
        let next = o.state.after(r).map_err(|e| format!("order {}: {}", id, e))?;
        if let Report::Partial { quantity, .. } | Report::Filled { quantity, .. } = r {
            o.filled += quantity;
        }
        if next != o.state {
            o.history.push((t, next));
        }
        o.state = next;
        if !next.working() && self.working.get(&o.instrument).is_some_and(|w| w == id) {
            self.working.remove(&o.instrument);
        }
        Ok(o)
    }

    /// Order `id` superseded at bar `t`: by a later decision on its
    /// instrument, or by the run's end.
    pub fn superseded(&mut self, id: &str, t: i64) {
        if let Some(o) = self.orders.get_mut(id) {
            if o.state != OrderState::Replaced {
                o.state = OrderState::Replaced;
                o.history.push((t, OrderState::Replaced));
            }
            if self.working.get(&o.instrument).is_some_and(|w| w == id) {
                self.working.remove(&o.instrument);
            }
        }
    }
}
