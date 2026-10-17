//! A broker in memory (M-10): an account, and a policy that says what
//! becomes of each working order every time the broker is asked for its
//! events. By default it fills an order in full at the price it was sized
//! at, without commission. A handle: its clones share the one broker, so a
//! test keeps one to script it and to read what it was sent.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, MutexGuard};

use super::broker::{Broker, BrokerError, BrokerEvent, BrokerOrderId, OrderRequest, Report, Snapshot};
use crate::kernel::value::OrderKind;

/// What becomes of a working order when the broker is asked for its
/// events: the order, and how many times it was asked before. Reports that
/// end the order end it; otherwise it works on. A policy must not use the
/// broker's handle (the broker is locked while it runs).
pub type Policy = Box<dyn FnMut(&OrderRequest, u32) -> Vec<Report> + Send>;

/// Something the broker does at a given call of `events`, before its
/// orders: its session dropping or coming back, its account changed
/// outside the run (a trade elsewhere, a transfer).
#[derive(Clone, Debug, PartialEq)]
pub enum Scripted {
    Disconnect,
    Reconnect,
    Position(String, f64),
    Cash(f64),
}

struct Mock {
    account: Snapshot,
    policy: Policy,
    /// The orders working, in the order they were sent: each with how many
    /// times the policy saw it.
    working: BTreeMap<u64, (BrokerOrderId, OrderRequest, u32)>,
    next: u64,
    connected: bool,
    calls: u32,
    script: Vec<(u32, Scripted)>,
    queue: Vec<BrokerEvent>,
    sent: Vec<(BrokerOrderId, OrderRequest)>,
    cancelled: Vec<BrokerOrderId>,
    replaced: Vec<(BrokerOrderId, BrokerOrderId)>,
}

#[derive(Clone)]
pub struct MockBroker {
    inner: Arc<Mutex<Mock>>,
}

impl std::fmt::Debug for MockBroker {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let m = self.lock();
        f.debug_struct("MockBroker")
            .field("account", &m.account)
            .field("working", &m.working.len())
            .field("connected", &m.connected)
            .finish()
    }
}

impl MockBroker {
    /// A broker whose account holds `cash` and no position, filling every
    /// order at once.
    pub fn new(cash: f64) -> MockBroker {
        MockBroker::with_policy(cash, MockBroker::fill_at_reference)
    }

    pub fn with_policy(cash: f64, policy: impl FnMut(&OrderRequest, u32) -> Vec<Report> + Send + 'static) -> MockBroker {
        MockBroker {
            inner: Arc::new(Mutex::new(Mock {
                account: Snapshot { positions: BTreeMap::new(), cash },
                policy: Box::new(policy),
                working: BTreeMap::new(),
                next: 1,
                connected: true,
                calls: 0,
                script: Vec::new(),
                queue: Vec::new(),
                sent: Vec::new(),
                cancelled: Vec::new(),
                replaced: Vec::new(),
            })),
        }
    }

    /// The default policy: acknowledged and filled in full at the price
    /// the order was sized at, without commission; a limit or a stop the
    /// reference does not reach works until it is cancelled.
    pub fn fill_at_reference(o: &OrderRequest, _asked: u32) -> Vec<Report> {
        let Some(p) = o.reference else {
            return vec![Report::Rejected {
                reason: "no reference price to fill at".into(),
            }];
        };
        let buy = o.quantity > 0.0;
        let price = match o.order.kind {
            OrderKind::Limit(l) if (buy && p > l) || (!buy && p < l) => return vec![Report::New],
            OrderKind::Limit(l) => l,
            OrderKind::Stop(s) if (buy && p < s) || (!buy && p > s) => return vec![Report::New],
            _ => p,
        };
        vec![
            Report::New,
            Report::Filled {
                quantity: o.quantity,
                price,
                commission: 0.0,
            },
        ]
    }

    fn lock(&self) -> MutexGuard<'_, Mock> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Does `action` at the `call`-th call of `events` (from zero).
    pub fn at_call(&self, call: u32, action: Scripted) {
        self.lock().script.push((call, action));
    }

    pub fn set_position(&self, instrument: &str, quantity: f64) {
        self.lock().account.positions.insert(instrument.to_string(), quantity);
    }

    pub fn set_cash(&self, cash: f64) {
        self.lock().account.cash = cash;
    }

    /// The session drops now: requests fail until it is back.
    pub fn disconnect(&self) {
        let mut m = self.lock();
        m.connected = false;
        m.queue.push(BrokerEvent::Disconnect);
    }

    pub fn reconnect(&self) {
        let mut m = self.lock();
        m.connected = true;
        m.queue.push(BrokerEvent::Reconnect);
    }

    /// Every order sent (a replacement included), in order, with its id.
    pub fn sent(&self) -> Vec<(BrokerOrderId, OrderRequest)> {
        self.lock().sent.clone()
    }

    pub fn cancelled(&self) -> Vec<BrokerOrderId> {
        self.lock().cancelled.clone()
    }

    /// Each replace: the order replaced and its replacement.
    pub fn replaced(&self) -> Vec<(BrokerOrderId, BrokerOrderId)> {
        self.lock().replaced.clone()
    }

    pub fn account(&self) -> Snapshot {
        self.lock().account.clone()
    }

    /// The orders working now.
    pub fn working(&self) -> Vec<BrokerOrderId> {
        self.lock().working.values().map(|(id, ..)| id.clone()).collect()
    }

    /// How many times `events` was called.
    pub fn calls(&self) -> u32 {
        self.lock().calls
    }
}

impl Mock {
    fn up(&self) -> Result<(), BrokerError> {
        if self.connected {
            Ok(())
        } else {
            Err(BrokerError("the broker's session is down".into()))
        }
    }

    fn take_working(&mut self, id: &str) -> Result<(), BrokerError> {
        let key = self
            .working
            .iter()
            .find(|(_, (w, ..))| w == id)
            .map(|(k, _)| *k)
            .ok_or_else(|| BrokerError(format!("no order {} works", id)))?;
        self.working.remove(&key);
        Ok(())
    }

    fn add(&mut self, order: OrderRequest) -> BrokerOrderId {
        let id = format!("M{}", self.next);
        self.working.insert(self.next, (id.clone(), order.clone(), 0));
        self.next += 1;
        self.sent.push((id.clone(), order));
        id
    }
}

impl Broker for MockBroker {
    fn send(&mut self, order: OrderRequest) -> Result<BrokerOrderId, BrokerError> {
        let mut m = self.lock();
        m.up()?;
        Ok(m.add(order))
    }

    fn cancel(&mut self, id: &str) -> Result<(), BrokerError> {
        let mut m = self.lock();
        m.up()?;
        m.take_working(id)?;
        m.cancelled.push(id.to_string());
        m.queue.push(BrokerEvent::Execution {
            order: id.to_string(),
            report: Report::Cancelled {
                reason: "cancelled at the client's request".into(),
            },
        });
        Ok(())
    }

    fn replace(&mut self, id: &str, order: OrderRequest) -> Result<BrokerOrderId, BrokerError> {
        let mut m = self.lock();
        m.up()?;
        m.take_working(id)?;
        m.queue.push(BrokerEvent::Execution {
            order: id.to_string(),
            report: Report::Cancelled { reason: "replaced".into() },
        });
        let new = m.add(order);
        m.replaced.push((id.to_string(), new.clone()));
        Ok(new)
    }

    fn events(&mut self) -> Vec<BrokerEvent> {
        let mut guard = self.lock();
        let m = &mut *guard;
        let call = m.calls;
        m.calls += 1;
        for (_, action) in m.script.iter().filter(|(c, _)| *c == call) {
            match action {
                Scripted::Disconnect => {
                    m.connected = false;
                    m.queue.push(BrokerEvent::Disconnect);
                }
                Scripted::Reconnect => {
                    m.connected = true;
                    m.queue.push(BrokerEvent::Reconnect);
                }
                Scripted::Position(name, q) => {
                    m.account.positions.insert(name.clone(), *q);
                }
                Scripted::Cash(c) => m.account.cash = *c,
            }
        }
        let mut out = std::mem::take(&mut m.queue);
        if !m.connected {
            return out;
        }
        let keys: Vec<u64> = m.working.keys().copied().collect();
        for key in keys {
            let Some((id, order, asked)) = m.working.get(&key).cloned() else { continue };
            let reports = (m.policy)(&order, asked);
            if let Some(w) = m.working.get_mut(&key) {
                w.2 += 1;
            }
            for r in reports {
                if let Report::Partial { quantity, price, commission } | Report::Filled { quantity, price, commission } = &r {
                    *m.account.positions.entry(order.instrument.clone()).or_default() += quantity;
                    m.account.cash -= quantity * price * order.multiplier + commission;
                }
                if !matches!(r, Report::New | Report::Partial { .. }) {
                    m.working.remove(&key);
                }
                out.push(BrokerEvent::Execution { order: id.clone(), report: r });
            }
        }
        m.account.positions.retain(|_, q| q.abs() > 1e-9);
        out
    }

    fn snapshot(&mut self) -> Result<Snapshot, BrokerError> {
        let m = self.lock();
        m.up()?;
        Ok(m.account.clone())
    }
}
