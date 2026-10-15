//! Control payloads (P-2.2): a size-prefixed FlatBuffer whose root is the
//! `Envelope` of `schema/abt.fbs`. The size prefix is the buffer's length as
//! `u32` little-endian, as FlatBuffers' size-prefixed form writes it; on
//! receipt the buffer is checked once, converted to the owned type (which
//! visits every field the verifier would), and read from there.

use planus::{Builder, ReadAsRoot};

use crate::frame::MAX_CONTROL;
use crate::generated::abt::wire::{Envelope, EnvelopeRef, Message};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecodeError(pub String);

impl std::fmt::Display for DecodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for DecodeError {}

/// The control payload of a message.
pub fn encode(message: Message) -> Vec<u8> {
    let mut builder = Builder::new();
    let envelope = Envelope { message: Some(message) };
    let buf = builder.finish(&envelope, None);
    let mut out = Vec::with_capacity(4 + buf.len());
    out.extend_from_slice(&(buf.len() as u32).to_le_bytes());
    out.extend_from_slice(buf);
    out
}

/// The message of a control payload, verified and owned.
pub fn decode(payload: &[u8]) -> Result<Message, DecodeError> {
    if payload.len() > MAX_CONTROL + 4 {
        return Err(DecodeError(format!("a control payload of {} bytes exceeds {}", payload.len(), MAX_CONTROL)));
    }
    if payload.len() < 4 {
        return Err(DecodeError("a control payload shorter than its size prefix".into()));
    }
    let size = u32::from_le_bytes(payload[..4].try_into().expect("four bytes")) as usize;
    if size != payload.len() - 4 {
        return Err(DecodeError(format!("the size prefix says {} bytes, the payload holds {}", size, payload.len() - 4)));
    }
    let root = EnvelopeRef::read_as_root(&payload[4..]).map_err(|e| DecodeError(format!("malformed envelope: {}", e)))?;
    let owned = Envelope::try_from(root).map_err(|e| DecodeError(format!("malformed envelope: {}", e)))?;
    owned.message.ok_or_else(|| DecodeError("an envelope without a message".into()))
}
