//! The data layer (docs/spec/01-data.md): the registry, the store, the query
//! engine, the stream with explicit clock barriers, the aggregate pass and
//! the resolver. It holds typed relations with an event time and a receipt
//! time and interprets nothing (A-D6): point-in-time, as-of, causality and
//! hold-out are the kernel's and the query layer's, expressed as filters on
//! its queries.

pub mod catalog;
pub mod error;
pub mod registry;
pub mod rows;
pub mod store;

pub use error::DataError;
pub use registry::Registry;
pub use store::{Manifest, RowFilter, Store};
