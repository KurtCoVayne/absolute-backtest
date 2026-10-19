//! The data layer (docs/spec/01-data.md): the registry, the store, the query
//! engine, the stream with explicit clock barriers, the aggregate pass and
//! the resolver. It holds typed relations with an event time and a receipt
//! time and interprets nothing (A-D6): point-in-time, as-of, causality and
//! hold-out are the kernel's and the query layer's, expressed as filters on
//! its queries.

pub mod adapter;
pub mod adapters;
pub mod agg;
pub mod catalog;
pub mod cli;
pub mod error;
pub mod fault;
pub mod hot;
pub mod live;
pub mod options;
pub mod query;
pub mod registry;
pub mod resident;
pub mod resolver;
pub mod rows;
pub mod service;
pub mod staging;
pub mod store;
pub mod stream;

pub use error::DataError;
pub use registry::Registry;
pub use store::{Manifest, RowFilter, Store};
