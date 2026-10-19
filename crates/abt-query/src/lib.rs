//! The query layer (docs/spec/03-query.md): plans, metrics, the statistical
//! test framework, claims, feed and activation statistics, reports, the
//! briefs harness. A client
//! of the kernel and the data layer (A-D13, A-D17): it reads a run's tables
//! through the data service and never evaluates a program.

pub mod activation;
pub mod briefs;
pub mod cli;
pub mod data;
pub mod dispatch;
pub mod feed;
pub mod kernel;
pub mod metrics;
pub mod plan;
pub mod relation;
pub mod report;
pub mod run;
pub mod service;
pub mod sheet;
pub mod tests;
pub mod time;
