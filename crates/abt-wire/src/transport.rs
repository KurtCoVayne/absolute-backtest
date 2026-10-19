//! Moving frames between two peers (P-2.1): an in-process channel pair and a
//! Unix domain socket carry the same encoded frames, so a test over the
//! socket is a transport test and never a logic test (decision A-D3). There
//! is no TCP.

use std::io::{self, BufReader, BufWriter, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::Path;
use std::sync::mpsc::{sync_channel, Receiver, SyncSender};

use crate::frame::{Frame, FrameError};

/// Frames out of one end of a connection.
pub trait FrameTx: Send {
    fn send(&mut self, f: &Frame) -> Result<(), FrameError>;
}

/// Frames into one end of a connection; `None` once the peer has closed.
pub trait FrameRx: Send {
    fn recv(&mut self) -> Result<Option<Frame>, FrameError>;
}

/// One end of a connection.
pub struct Connection {
    pub tx: Box<dyn FrameTx>,
    pub rx: Box<dyn FrameRx>,
}

struct ChannelTx(SyncSender<Vec<u8>>);
struct ChannelRx(Receiver<Vec<u8>>);

impl FrameTx for ChannelTx {
    fn send(&mut self, f: &Frame) -> Result<(), FrameError> {
        let bytes = f.encode()?;
        self.0.send(bytes).map_err(|_| FrameError::Io(io::Error::new(io::ErrorKind::BrokenPipe, "the peer closed the channel")))
    }
}

impl FrameRx for ChannelRx {
    fn recv(&mut self) -> Result<Option<Frame>, FrameError> {
        match self.0.recv() {
            Ok(bytes) => {
                let (f, used) = Frame::decode_prefix(&bytes)?.ok_or(FrameError::Truncated)?;
                if used != bytes.len() {
                    return Err(FrameError::Malformed(format!("{} bytes after a frame", bytes.len() - used)));
                }
                Ok(Some(f))
            }
            Err(_) => Ok(None),
        }
    }
}

/// Two connected ends in one process. Each direction holds at most `bound`
/// frames in flight; a sender blocks on a full direction (backpressure,
/// P-2.4: the service blocks rather than drops).
pub fn channel_pair(bound: usize) -> (Connection, Connection) {
    let (a_tx, b_rx) = sync_channel(bound.max(1));
    let (b_tx, a_rx) = sync_channel(bound.max(1));
    (
        Connection {
            tx: Box::new(ChannelTx(a_tx)),
            rx: Box::new(ChannelRx(a_rx)),
        },
        Connection {
            tx: Box::new(ChannelTx(b_tx)),
            rx: Box::new(ChannelRx(b_rx)),
        },
    )
}

struct SocketTx(BufWriter<UnixStream>);
struct SocketRx(BufReader<UnixStream>);

impl FrameTx for SocketTx {
    fn send(&mut self, f: &Frame) -> Result<(), FrameError> {
        f.write_to(&mut self.0)?;
        self.0.flush()?;
        Ok(())
    }
}

impl FrameRx for SocketRx {
    fn recv(&mut self) -> Result<Option<Frame>, FrameError> {
        match Frame::read_from(&mut self.0) {
            Err(FrameError::Io(e)) if matches!(e.kind(), io::ErrorKind::ConnectionReset | io::ErrorKind::BrokenPipe) => Ok(None),
            other => other,
        }
    }
}

impl Connection {
    /// The two halves of a connected socket.
    pub fn socket(stream: UnixStream) -> io::Result<Connection> {
        let write = stream.try_clone()?;
        Ok(Connection {
            tx: Box::new(SocketTx(BufWriter::new(write))),
            rx: Box::new(SocketRx(BufReader::new(stream))),
        })
    }

    /// Connects to a service's socket.
    pub fn connect(path: &Path) -> io::Result<Connection> {
        Connection::socket(UnixStream::connect(path)?)
    }
}

/// Binds a service's socket, readable and writable by its owner only
/// (P-2.1: mode 0600; there is no other authentication). A stale socket
/// file is replaced.
pub fn bind(path: &Path) -> io::Result<UnixListener> {
    if path.exists() {
        std::fs::remove_file(path)?;
    }
    if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
        std::fs::create_dir_all(dir)?;
    }
    let listener = UnixListener::bind(path)?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    Ok(listener)
}

/// The default socket of a layer: `$XDG_RUNTIME_DIR/abt/<layer>.sock`, or
/// the temporary directory when the variable is unset.
pub fn default_socket(layer: &str) -> std::path::PathBuf {
    let base = std::env::var_os("XDG_RUNTIME_DIR").map(std::path::PathBuf::from).unwrap_or_else(std::env::temp_dir);
    base.join("abt").join(format!("{}.sock", layer))
}
