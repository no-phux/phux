//! Frame assembly for byte-stream transports, and the Unix domain socket transport.
#![allow(
    clippy::redundant_pub_crate,
    reason = "private transport module; the accept loop uses these through the transport re-export"
)]

use std::io;

use bytes::BytesMut;
use phux_protocol::policy::{PeerIdentity, TransportType};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixListener;
use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};

use phux_protocol::wire::framing::{self, LENGTH_PREFIX_LEN as LENGTH_PREFIX};

use super::{FrameReader, FrameWriter, Incoming};

// ── Unix domain socket ───────────────────────────────────────────────────────

/// Bytes a stream reader asks for per read. The frame buffer grows only by
/// what actually arrived, so a bare length header never commits a
/// maximum-size frame's memory.
const READ_CHUNK: usize = 64 * 1024;

/// Reassembles length-prefixed frames off a byte stream, cancel-safely.
///
/// The client loop races `read_frame` against other events in a `select!`,
/// which drops the read future whenever another arm wins. Bytes already read
/// live here, not in that future, so a dropped read loses nothing and the
/// stream never desynchronises onto payload bytes.
#[derive(Debug, Default)]
pub(crate) struct FrameAssembler {
    pub(super) buf: BytesMut,
}

impl FrameAssembler {
    /// The next complete frame (prefix included), or `None` at a clean end of
    /// stream on a frame boundary. Cancel-safe.
    ///
    /// A frame already buffered by an earlier call is returned only after
    /// yielding to the scheduler. One read can carry hundreds of frames (a
    /// typed line is one `INPUT_KEY` per character), and handing them out
    /// with no await point would dispatch the whole burst in one poll while
    /// pane actors and other connections on this thread wait. Input credits
    /// (ADR-0144) keep such a burst lossless; this keeps it fair, as the
    /// per-frame socket reads it replaced were under tokio's cooperative
    /// budget.
    pub(crate) async fn read_frame<R>(&mut self, reader: &mut R) -> io::Result<Option<BytesMut>>
    where
        R: tokio::io::AsyncRead + Unpin,
    {
        let mut read_this_call = false;
        loop {
            if frame_complete(&self.buf)? {
                if !read_this_call {
                    // Before the split, so a cancelled yield loses nothing.
                    tokio::task::yield_now().await;
                }
                return Ok(framing::split_frame(&mut self.buf)?);
            }
            read_this_call = true;
            self.buf.reserve(READ_CHUNK);
            if reader.read_buf(&mut self.buf).await? == 0 {
                if self.buf.is_empty() {
                    return Ok(None);
                }
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "stream ended mid-frame",
                ));
            }
        }
    }
}

/// Whether `buf` starts with one whole frame; a bad length header is the
/// same framing error [`framing::split_frame`] reports.
fn frame_complete(buf: &[u8]) -> Result<bool, framing::FramingError> {
    let Some(header) = buf.first_chunk::<LENGTH_PREFIX>() else {
        return Ok(false);
    };
    Ok(buf.len() >= LENGTH_PREFIX + framing::decode_length(*header)?)
}

/// UDS read half: reassembles length-prefixed frames off the byte stream.
pub(crate) struct UdsReader {
    pub(super) reader: OwnedReadHalf,
    pub(super) frames: FrameAssembler,
}

impl FrameReader for UdsReader {
    async fn read_frame(&mut self) -> io::Result<Option<BytesMut>> {
        self.frames.read_frame(&mut self.reader).await
    }
}

/// UDS write half.
pub(crate) struct UdsWriter {
    writer: OwnedWriteHalf,
}

impl FrameWriter for UdsWriter {
    async fn write_frame(&mut self, frame: &[u8]) -> io::Result<()> {
        self.writer.write_all(frame).await
    }

    /// One `write_all` for the whole batch: identical bytes, one syscall.
    async fn write_frames(&mut self, batch: &mut BytesMut, _ends: &[usize]) -> io::Result<()> {
        self.writer.write_all(batch).await
    }

    async fn close(&mut self) -> io::Result<()> {
        self.writer.shutdown().await
    }
}

/// UDS listener (a newtype so `Incoming::accept` does not shadow
/// `UnixListener::accept`).
pub(crate) struct UdsListener(UnixListener);

impl UdsListener {
    pub(crate) const fn new(listener: UnixListener) -> Self {
        Self(listener)
    }

    /// The listening descriptor, inherited across a graceful upgrade
    /// (ADR-0032).
    pub(crate) fn as_raw_fd(&self) -> std::os::fd::RawFd {
        use std::os::fd::AsRawFd;
        self.0.as_raw_fd()
    }
}

impl Incoming for UdsListener {
    type Reader = UdsReader;
    type Writer = UdsWriter;

    fn transport_type(&self) -> TransportType {
        TransportType::UnixSocket
    }

    async fn accept(&self) -> io::Result<(UdsReader, UdsWriter, crate::auth::ConnectionIdentity)> {
        let (stream, _addr) = self.0.accept().await?;
        let peer_identity = peer_identity_from_uds(&stream)?;
        let (reader, writer) = stream.into_split();
        Ok((
            UdsReader {
                reader,
                frames: FrameAssembler::default(),
            },
            UdsWriter { writer },
            peer_identity.into(),
        ))
    }

    fn kind(&self) -> &'static str {
        "uds"
    }
}

pub(super) fn peer_identity_from_credentials(
    credentials: io::Result<(u32, Option<u32>)>,
) -> io::Result<PeerIdentity> {
    let (uid, pid) = credentials?;
    Ok(PeerIdentity {
        uid,
        pid,
        exe_path: None,
        mcp_host_key: None,
        transport: TransportType::UnixSocket,
        source_addr: None,
    })
}

/// Extract peer identity from a Unix domain socket.
#[cfg(target_os = "linux")]
pub(super) fn peer_identity_from_uds(stream: &tokio::net::UnixStream) -> io::Result<PeerIdentity> {
    peer_identity_from_credentials(stream.peer_cred().map(|cred| {
        (
            cred.uid(),
            cred.pid().and_then(|pid| u32::try_from(pid).ok()),
        )
    }))
}

/// Extract peer identity from a Unix domain socket on Darwin.
#[cfg(target_os = "macos")]
pub(super) fn peer_identity_from_uds(stream: &tokio::net::UnixStream) -> io::Result<PeerIdentity> {
    use std::os::fd::AsRawFd as _;

    let mut uid: libc::uid_t = 0;
    let mut gid: libc::gid_t = 0;
    // SAFETY: both output pointers are valid for writes for the duration of
    // the call, and `stream` owns a live Unix-domain socket descriptor.
    let status = unsafe { libc::getpeereid(stream.as_raw_fd(), &raw mut uid, &raw mut gid) };
    if status != 0 {
        return Err(io::Error::last_os_error());
    }
    let mut pid: libc::pid_t = 0;
    let mut length = u32::try_from(std::mem::size_of::<libc::pid_t>())
        .map_err(|_| io::Error::other("peer PID credential size is not representable"))?;
    // SAFETY: the socket is live; the output pointer and length describe one
    // writable pid_t. LOCAL_PEERPID is kernel-attested, not supplied in HELLO.
    let status = unsafe {
        libc::getsockopt(
            stream.as_raw_fd(),
            libc::SOL_LOCAL,
            libc::LOCAL_PEERPID,
            (&raw mut pid).cast(),
            &raw mut length,
        )
    };
    if status != 0 {
        return Err(io::Error::last_os_error());
    }
    if usize::try_from(length).ok() != Some(std::mem::size_of::<libc::pid_t>()) {
        return Err(io::Error::other(
            "kernel returned an invalid peer PID credential size",
        ));
    }
    let pid = u32::try_from(pid)
        .ok()
        .filter(|pid| *pid != 0)
        .ok_or_else(|| io::Error::other("kernel returned an invalid peer PID"))?;
    peer_identity_from_credentials(Ok((uid, Some(pid))))
}

/// Reject UDS transports on targets without an authenticated peer credential API.
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub(super) fn peer_identity_from_uds(_stream: &tokio::net::UnixStream) -> io::Result<PeerIdentity> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "authenticated Unix peer credentials are unavailable",
    ))
}
