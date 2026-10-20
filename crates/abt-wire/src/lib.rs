//! The types and messages the three layers share, and nothing else
//! (`docs/spec/04-protocol.md`, P-1.1; decision A-D2): the scalar types of
//! D-2.1, hashes (BLAKE3, A-6), canonical forms for hashing (RFC 8785, P-6),
//! the frame codec (P-2.2), the FlatBuffers messages compiled from
//! `schema/abt.fbs` by planus, tables as Arrow IPC frames (P-4), and the
//! moving of frames: an in-process channel pair and a Unix socket carrying
//! the same bytes, with the request and response exchange every service and
//! client speaks (P-2.1 to P-2.4; docs/spec/ERRATA.md, E-29). No function
//! here interprets a relation or answers a request; the calendar arithmetic
//! of durations and the chain filters of D-4.4 are here because two layers
//! must compute them identically.

pub mod canonical;
pub mod chain;
pub mod convert;
pub mod frame;
pub mod hash;
pub mod memory;
pub mod rpc;
pub mod scalar;
pub mod table;
pub mod transport;

/// The code planus generates from `schema/abt.fbs`: owned types that derive
/// serde, zero-copy readers (`…Ref`) and their conversions.
#[allow(clippy::all, clippy::pedantic, unused_imports, dead_code, non_camel_case_types, missing_docs)]
pub mod generated {
    include!(concat!(env!("OUT_DIR"), "/abt_generated.rs"));
}

pub use canonical::{to_canonical_json, Canonical, CanonicalError};
pub use frame::{Frame, FrameError, FrameKind};
pub use generated::abt::wire::{Envelope, EnvelopeRef, Error as ErrorMessage, ErrorCode, Hello, Layer, Message};
pub use hash::{BindingId, Hash, PlanHash, RunId, TrialId, VersionId};
pub use scalar::{Entity, Label, Seq, Time};

/// The protocol version a peer announces in `Hello` (P-2.3, P-7).
pub const PROTOCOL: &str = "1.2";
