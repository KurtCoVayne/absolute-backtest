//! The types and messages the three layers share, and nothing else
//! (`docs/spec/04-protocol.md`, P-1.1; decision A-D2): the scalar types of
//! D-2.1, hashes (BLAKE3, A-6), canonical forms for hashing (RFC 8785, P-6),
//! the frame codec (P-2.2) and the FlatBuffers messages compiled from
//! `schema/abt.fbs` by planus. No function here interprets a relation.

pub mod canonical;
pub mod convert;
pub mod frame;
pub mod hash;
pub mod scalar;

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
pub const PROTOCOL: &str = "1.0";
