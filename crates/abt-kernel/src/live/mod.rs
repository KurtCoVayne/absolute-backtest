//! The live interface (K-6.4, K-6.5; M-10): the `Broker` trait an adapter
//! implements, `BrokerExecutor` that runs a program's orders through one,
//! the deployment document a live run takes, the reconciliation of the book
//! against the broker's account, the order lifecycle and the kill switch.
//! No adapter is part of this build (A-D27): `broker mock` runs against
//! `MockBroker`, a broker in memory, and any other broker is refused naming
//! the adapter it lacks (ERRATA E-82).

pub mod broker;
pub mod control;
pub mod deployment;
pub mod executor;
pub mod lifecycle;
pub mod mock;
pub mod reconcile;

pub use broker::{Broker, BrokerError, BrokerEvent, BrokerOrderId, OrderRequest, Report, Snapshot};
pub use control::LiveControl;
pub use deployment::{Deployment, HaltSource};
pub use executor::BrokerExecutor;
pub use lifecycle::{Lifecycle, LiveOrder, OrderState};
pub use mock::{MockBroker, Scripted};
pub use reconcile::Policy;

use crate::kernel::RunError;

/// The broker a deployment names, connected, its account opening with
/// `cash`: the mock, or a refusal naming the adapter this build lacks.
pub fn connect(d: &Deployment, cash: f64) -> Result<Box<dyn Broker>, RunError> {
    match d.broker.to_ascii_lowercase().as_str() {
        "mock" => Ok(Box::new(MockBroker::new(cash))),
        other => Err(RunError::Config(format!(
            "broker `{}`: this build has no adapter for it. The broker interface is frozen (K-6.4) and its adapters are the live deployment's (A-D27); `broker mock` rehearses against a broker in memory",
            other
        ))),
    }
}

/// The live session a live-broker run reads: the one named, else the
/// deployment's recording stream; none for a rehearsal over the data's
/// replay. Refused when the two name different streams.
pub fn session_of(named: Option<String>, d: &Deployment) -> Result<Option<String>, String> {
    match (named, &d.recording) {
        (Some(s), Some(r)) if &s != r => Err(format!("the session {} is not the deployment's recording stream {}", s, r)),
        (s, r) => Ok(s.or_else(|| r.clone())),
    }
}
