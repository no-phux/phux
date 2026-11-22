//! [`Outbound`]: the mailbox payload toward a client. A leaf, so engines
//! that frame their own output need not depend on the server.

/// Default per-client outbound mailbox depth (frames are coalesced chunks,
/// so eight is ample).
pub const DEFAULT_CLIENT_MAILBOX: usize = 8;

/// A message on a client's outbound mailbox.
///
/// [`Outbound::Frame`] is encoded and written; [`Outbound::FatalError`]
/// is a final `ERROR`, then `DETACHED(PROTOCOL_ERROR)` and a close, after
/// which the writer discards anything queued.
#[derive(Debug)]
pub enum Outbound {
    /// A structured frame; the writer encodes it before writing.
    Frame(phux_protocol::wire::frame::FrameKind),
    /// Final protocol error and `DETACHED(PROTOCOL_ERROR)`, then transport close.
    FatalError {
        /// Optional request correlation.
        request_id: Option<u32>,
        /// Protocol error category.
        code: phux_protocol::wire::frame::ErrorCode,
        /// Human-readable failure detail.
        message: String,
    },
}
