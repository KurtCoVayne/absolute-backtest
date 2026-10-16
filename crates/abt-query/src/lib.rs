//! The query layer (docs/spec/03-query.md): plans, metrics, the statistical
//! test framework, claims, feed and activation statistics, reports. A client
//! of the kernel and the data layer (A-D13, A-D17): it reads a run's tables
//! through the data service and never evaluates a program.

pub mod cli;
pub mod data;
pub mod kernel;
pub mod metrics;
pub mod plan;
pub mod relation;
pub mod run;
pub mod tests;
pub mod time;
