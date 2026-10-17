//! The broker interface (K-6.4): what the kernel asks of a broker and what
//! a broker tells it. An adapter (IBKR's FIX 4.4 session or the TWS API,
//! A-D27) implements `Broker`; nothing here depends on its dialect.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::kernel::value::Order;

/// A broker's id of an order.
pub type BrokerOrderId = String;

/// An order as it is sent: a NewOrderSingle, or a NewOrderMultileg when it
/// has legs.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct OrderRequest {
    /// The kernel's id of the order (K-5.2), the client's id at the broker.
    pub client_id: u64,
    /// The instrument, by the name the binding gives it.
    pub instrument: String,
    /// Signed: a buy is positive, a sale negative.
    pub quantity: f64,
    /// How it executes (market, at the open or the close, limit, stop) and
    /// its time in force, as the program decided it.
    pub order: Order,
    /// The broker's time in force for the order's kind, from the
    /// deployment's `tif_map`; none for the broker's default.
    pub tif: Option<String>,
    /// A multileg order's legs, each instrument with its signed ratio;
    /// empty for a single order.
    pub legs: Vec<(String, f64)>,
    /// The units one contract of the instrument carries.
    pub multiplier: f64,
    /// The price the order was sized at: its instrument's mark at the
    /// decision bar.
    pub reference: Option<f64>,
    /// Where it is routed, from the deployment's `routing`.
    pub destination: Option<String>,
}

/// An execution report (K-6.4): what became of an order.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum Report {
    /// Accepted: the order works.
    New,
    /// Part of it filled (this fill's signed quantity, its price and the
    /// broker's commission); the rest works on.
    Partial { quantity: f64, price: f64, commission: f64 },
    /// Filled: the last fill's signed quantity, price and commission. The
    /// order is done.
    Filled { quantity: f64, price: f64, commission: f64 },
    /// Cancelled, at the client's request or the broker's.
    Cancelled { reason: String },
    /// Refused, with the broker's reason.
    Rejected { reason: String },
    /// Ended by its time in force.
    Expired { reason: String },
}

impl Report {
    /// The report's name as an ExecutionReport calls it.
    pub fn name(&self) -> &'static str {
        match self {
            Report::New => "new",
            Report::Partial { .. } => "partial",
            Report::Filled { .. } => "filled",
            Report::Cancelled { .. } => "cancelled",
            Report::Rejected { .. } => "rejected",
            Report::Expired { .. } => "expired",
        }
    }
}

/// What a broker tells the kernel between its requests (K-6.4):
/// ExecutionReports, PositionReports and CashReports, and the session
/// going down and coming back.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum BrokerEvent {
    Execution { order: BrokerOrderId, report: Report },
    Position { instrument: String, quantity: f64 },
    Cash { amount: f64 },
    Disconnect,
    Reconnect,
}

/// The account as the broker holds it, for reconciliation: positions by
/// instrument, and cash.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Snapshot {
    pub positions: BTreeMap<String, f64>,
    pub cash: f64,
}

/// A request the broker refused or could not take (a session that is
/// down), with its reason.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BrokerError(pub String);

impl std::fmt::Display for BrokerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for BrokerError {}

/// A broker (K-6.4). `events` drains what the broker reported since it was
/// last called: the executor calls it at every fill step, before the bar's
/// decisions, rather than taking each event through `on_event` (ERRATA
/// E-82). It returns a vector, which keeps the trait an object.
pub trait Broker: Send {
    /// A new order; the broker's id of it.
    fn send(&mut self, order: OrderRequest) -> Result<BrokerOrderId, BrokerError>;
    fn cancel(&mut self, id: &str) -> Result<(), BrokerError>;
    /// A working order replaced by another (cancel/replace); the broker's id
    /// of the new one.
    fn replace(&mut self, id: &str, order: OrderRequest) -> Result<BrokerOrderId, BrokerError>;
    fn events(&mut self) -> Vec<BrokerEvent>;
    fn snapshot(&mut self) -> Result<Snapshot, BrokerError>;
}
