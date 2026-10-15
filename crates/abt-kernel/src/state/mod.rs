//! The state tables and the provenance of a run (compiler design, sections
//! 7.2 to 7.5): after a run, every table has a stable schema, renders as
//! text or JSON, and exports as Arrow IPC (or Parquet); a derived tuple
//! has one witness (the first derivation in canonical order, deterministic
//! by I2), found by re-evaluating its rule at its bar; an absent tuple is
//! explained by the first excluding literal of each rule and the bindings
//! at that point. Nothing here changes a decision: the tables read the
//! model the run built, and a query evaluates rules the run may have
//! skipped (an elided relation) without adding a rule.

pub mod provenance;
pub mod tables;

pub use tables::{Cell, ColType, Table};
