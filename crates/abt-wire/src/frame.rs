//! The frame codec (P-2.2): `length: u32 LE | kind: u8 | correlation: u64 LE
//! | payload`, where `length` counts the bytes that follow it. A control
//! payload is a size-prefixed FlatBuffer `Envelope`; a `schema` or `batch`
//! payload is an Arrow IPC message. A frame is at most 64 MiB and a control
//! payload at most 1 MiB; a larger batch spans several frames.

use std::fmt;
use std::io::{self, Read, Write};

/// The largest frame, header included.
pub const MAX_FRAME: usize = 64 << 20;
/// The largest control payload (the size rule of P-2.2).
pub const MAX_CONTROL: usize = 1 << 20;
/// `kind` and `correlation`, the bytes `length` counts besides the payload.
pub const HEADER_AFTER_LENGTH: usize = 1 + 8;

#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum FrameKind {
    Hello = 0,
    Request = 1,
    Response = 2,
    Schema = 3,
    Batch = 4,
    End = 5,
    Error = 6,
    Cancel = 7,
    Ack = 8,
    Ping = 9,
    Pong = 10,
}

impl FrameKind {
    pub fn from_u8(b: u8) -> Option<FrameKind> {
        use FrameKind::*;
        Some(match b {
            0 => Hello,
            1 => Request,
            2 => Response,
            3 => Schema,
            4 => Batch,
            5 => End,
            6 => Error,
            7 => Cancel,
            8 => Ack,
            9 => Ping,
            10 => Pong,
            _ => return None,
        })
    }

    /// Whether the payload is a control message (a FlatBuffer), bounded by
    /// [`MAX_CONTROL`].
    pub fn is_control(self) -> bool {
        matches!(self, FrameKind::Hello | FrameKind::Request | FrameKind::Response | FrameKind::Error | FrameKind::Ack)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Frame {
    pub kind: FrameKind,
    pub correlation: u64,
    pub payload: Vec<u8>,
}

#[derive(Debug)]
pub enum FrameError {
    /// The frame exceeds [`MAX_FRAME`], or a control payload [`MAX_CONTROL`].
    TooLarge {
        kind: Option<FrameKind>,
        bytes: usize,
        limit: usize,
    },
    UnknownKind(u8),
    /// The stream ended inside a frame.
    Truncated,
    /// A length too small to hold the header.
    Malformed(String),
    Io(io::Error),
}

impl fmt::Display for FrameError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            FrameError::TooLarge { kind, bytes, limit } => match kind {
                Some(k) => write!(f, "a {:?} frame of {} bytes exceeds the limit of {} bytes", k, bytes, limit),
                None => write!(f, "a frame of {} bytes exceeds the limit of {} bytes", bytes, limit),
            },
            FrameError::UnknownKind(k) => write!(f, "unknown frame kind {}", k),
            FrameError::Truncated => write!(f, "the stream ended inside a frame"),
            FrameError::Malformed(m) => write!(f, "malformed frame: {}", m),
            FrameError::Io(e) => write!(f, "{}", e),
        }
    }
}

impl std::error::Error for FrameError {}

impl From<io::Error> for FrameError {
    fn from(e: io::Error) -> Self {
        FrameError::Io(e)
    }
}

impl Frame {
    pub fn new(kind: FrameKind, correlation: u64, payload: Vec<u8>) -> Frame {
        Frame { kind, correlation, payload }
    }

    fn check(kind: FrameKind, payload_len: usize) -> Result<(), FrameError> {
        let total = 4 + HEADER_AFTER_LENGTH + payload_len;
        if total > MAX_FRAME {
            return Err(FrameError::TooLarge {
                kind: Some(kind),
                bytes: total,
                limit: MAX_FRAME,
            });
        }
        if kind.is_control() && payload_len > MAX_CONTROL {
            return Err(FrameError::TooLarge {
                kind: Some(kind),
                bytes: payload_len,
                limit: MAX_CONTROL,
            });
        }
        Ok(())
    }

    /// The frame's bytes.
    pub fn encode(&self) -> Result<Vec<u8>, FrameError> {
        Frame::check(self.kind, self.payload.len())?;
        let mut out = Vec::with_capacity(4 + HEADER_AFTER_LENGTH + self.payload.len());
        out.extend_from_slice(&((HEADER_AFTER_LENGTH + self.payload.len()) as u32).to_le_bytes());
        out.push(self.kind as u8);
        out.extend_from_slice(&self.correlation.to_le_bytes());
        out.extend_from_slice(&self.payload);
        Ok(out)
    }

    pub fn write_to(&self, w: &mut impl Write) -> Result<(), FrameError> {
        w.write_all(&self.encode()?)?;
        Ok(())
    }

    /// The next frame of a stream; `None` at a clean end (no byte of a
    /// further frame).
    pub fn read_from(r: &mut impl Read) -> Result<Option<Frame>, FrameError> {
        let mut len = [0u8; 4];
        match read_full(r, &mut len)? {
            0 => return Ok(None),
            4 => {}
            _ => return Err(FrameError::Truncated),
        }
        let len = u32::from_le_bytes(len) as usize;
        if len < HEADER_AFTER_LENGTH {
            return Err(FrameError::Malformed(format!("length {} is shorter than the header", len)));
        }
        if 4 + len > MAX_FRAME {
            return Err(FrameError::TooLarge {
                kind: None,
                bytes: 4 + len,
                limit: MAX_FRAME,
            });
        }
        let mut head = [0u8; HEADER_AFTER_LENGTH];
        if read_full(r, &mut head)? != HEADER_AFTER_LENGTH {
            return Err(FrameError::Truncated);
        }
        let kind = FrameKind::from_u8(head[0]).ok_or(FrameError::UnknownKind(head[0]))?;
        let correlation = u64::from_le_bytes(head[1..9].try_into().expect("eight bytes"));
        let payload_len = len - HEADER_AFTER_LENGTH;
        Frame::check(kind, payload_len)?;
        let mut payload = vec![0u8; payload_len];
        if read_full(r, &mut payload)? != payload_len {
            return Err(FrameError::Truncated);
        }
        Ok(Some(Frame { kind, correlation, payload }))
    }

    /// Decodes one frame from the front of `bytes`: the frame and the bytes
    /// it used, or `None` when `bytes` does not yet hold a whole frame.
    pub fn decode_prefix(bytes: &[u8]) -> Result<Option<(Frame, usize)>, FrameError> {
        if bytes.len() < 4 {
            return Ok(None);
        }
        let len = u32::from_le_bytes(bytes[..4].try_into().expect("four bytes")) as usize;
        if len < HEADER_AFTER_LENGTH {
            return Err(FrameError::Malformed(format!("length {} is shorter than the header", len)));
        }
        if 4 + len > MAX_FRAME {
            return Err(FrameError::TooLarge {
                kind: None,
                bytes: 4 + len,
                limit: MAX_FRAME,
            });
        }
        if bytes.len() < 4 + len {
            return Ok(None);
        }
        let mut r = &bytes[..4 + len];
        let f = Frame::read_from(&mut r)?.ok_or(FrameError::Truncated)?;
        Ok(Some((f, 4 + len)))
    }
}

/// Reads until `buf` is full or the stream ends; the bytes read.
fn read_full(r: &mut impl Read, buf: &mut [u8]) -> io::Result<usize> {
    let mut n = 0;
    while n < buf.len() {
        match r.read(&mut buf[n..]) {
            Ok(0) => break,
            Ok(k) => n += k,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(n)
}
