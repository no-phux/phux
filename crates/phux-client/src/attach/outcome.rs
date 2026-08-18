//! The attach loop's exit vocabulary: the error every attach path funnels
//! into, and the "how did it end" explanation the CLI prints after teardown.
//!
//! Lifted out of [`super::driver`] under phux-4fbs.4. Eleven attach siblings
//! (plus `crate::layout_ops`) need nothing from the driver but these two
//! types, and importing them from the driver made every one of those modules
//! a back-edge into the file that owns the `tokio::select!` lifecycle. This
//! module depends on nothing inside `attach` except [`super::render`], so the
//! dependency now runs strictly one way: the driver and its siblings both
//! read this vocabulary, and it reads nothing back.
//!
//! `phux_client::attach::{AttachEnd, AttachError}` — the only path any other
//! crate uses — is unchanged; `super`'s re-export still publishes both.

use std::io;

use phux_protocol::wire::frame::DetachReason;
use phux_protocol::wire::framing::FramingError;

/// Errors the attach loop can surface to its caller.
///
/// Most variants wrap a richer underlying cause; the driver is careful to
/// fail fast rather than silently dropping protocol violations.
///
/// This is the client-facing attach-loop error vocabulary. It is distinct
/// from `phux-server/src/state/client.rs::AttachError`, which describes
/// failures in the server's internal registry attach operation.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum AttachError {
    /// Local I/O error — UDS connect, socket read/write, stdin/stdout, or
    /// terminal ioctl.
    #[error("attach loop io error: {0}")]
    Io(#[source] io::Error),

    /// A remote transport could not be established: QUIC handshake, TLS
    /// certificate verification (a fingerprint that did not match the pin), or
    /// a refused/oversized auth preamble. Distinguished from local [`Self::Io`]
    /// so the CLI can point at the address, the pin, and the token rather than a
    /// missing socket file.
    #[error("transport connect error: {0}")]
    Connect(String),

    /// The remote host did not answer the dial: connection refused, no
    /// route, or handshake timeout. Distinguished from [`Self::Connect`]
    /// (which covers pin and auth failures on a host that answered) so the
    /// CLI can hint at overlay reachability instead of credentials.
    #[error("transport connect error: {0}")]
    Unreachable(String),

    /// The server closed the connection without sending `DETACHED`.
    /// Distinguished from a clean detach so the CLI can surface "server
    /// went away" vs "you detached".
    #[error("connection closed by server before DETACHED")]
    Disconnected,

    /// The server sent something we cannot interpret — undecodable frame,
    /// or a valid frame we don't expect at this point in the lifecycle.
    #[error("protocol error: {0}")]
    Protocol(String),

    /// The server broke `docs/spec/proto.md` §5 framing: a length outside
    /// `1..=MAX_FRAME_LEN`, or a message whose size disagrees with the length
    /// it declares.
    ///
    /// Split out of [`Self::Protocol`] as a *typed* variant on purpose. §5
    /// obliges the receiving peer — either peer, per the spec text — to answer
    /// with `ERROR { code: FRAME_TOO_LARGE }` before closing. The client does
    /// not yet: its readers do not hold the paired write half at the decode
    /// seam, so emission stays deliberately deferred. Keeping the
    /// [`FramingError`] instead of flattening it to a string at the detection
    /// point means the eventual emitter needs the write half and nothing else
    /// — no re-plumbing of two call sites and this enum. The rendered message
    /// is unchanged from the string form it replaces.
    #[error("protocol error: server sent a malformed frame: {0}")]
    Framing(#[from] FramingError),

    /// Could not put the outer terminal into the expected state.
    #[error("terminal control error: {0}")]
    Terminal(String),

    /// Stdin is not a terminal. The attach loop needs a TTY because raw
    /// mode and alt-screen toggling require one. We bail early instead of
    /// silently no-op'ing.
    #[error("stdin is not a terminal; attach requires an interactive TTY")]
    NotATty,

    /// A libghostty operation failed on the client's local Terminal.
    #[error("libghostty: {0}")]
    Ghostty(#[from] libghostty_vt::Error),

    /// The server replied with a structured `ERROR` frame instead of
    /// `ATTACHED`. The session may not exist, the protocol version may
    /// have been rejected, or some other ATTACH-time server policy
    /// refused the request. The CLI surfaces this as actionable text.
    #[error("server refused attach: {0}")]
    Refused(String),
}

impl From<io::Error> for AttachError {
    fn from(value: io::Error) -> Self {
        Self::Io(value)
    }
}

impl From<phux_dial::DialError> for AttachError {
    fn from(value: phux_dial::DialError) -> Self {
        match value {
            phux_dial::DialError::Io(err) => Self::Io(err),
            phux_dial::DialError::Connect(msg) => Self::Connect(msg),
            phux_dial::DialError::Unreachable(msg) => Self::Unreachable(msg),
            // A stalled lane IS a disconnection — the peer is gone, we just
            // had to ask to find out. Mapping it here is what routes a
            // half-open `wss://` socket into the same bounded reconnect the
            // UDS graceful-upgrade blink uses, instead of hanging forever.
            phux_dial::DialError::Stalled(msg) => {
                tracing::info!(reason = %msg, "WebSocket lane stalled; treating it as a disconnect");
                Self::Disconnected
            }
        }
    }
}

impl From<super::render::RenderError> for AttachError {
    fn from(value: super::render::RenderError) -> Self {
        match value {
            super::render::RenderError::Io(e) => Self::Io(e),
            super::render::RenderError::Ghostty(e) => Self::Ghostty(e),
            super::render::RenderError::KittyReplay(e) => Self::Protocol(e.to_string()),
        }
    }
}

/// phux-i0e8.2.2: how a successful attach loop ended.
///
/// Threaded out of every `run_*` entry point so the CLI can tell "you
/// detached" from "your last pane died" — before this, an OOM-killed
/// shell tore the whole TUI down with zero explanation and looked
/// exactly like a phux crash. Either way the attach was *successful*
/// (the process exits `0`); this is an explanation, not an error.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AttachEnd {
    /// The server ended the attach with `DETACHED`, or the client tore its
    /// own attach down after asking for one.
    Detached {
        /// The `DETACHED` frame's reason, or `None` when the server stated
        /// none — a server predating `0.7.0-draft.7`, a reason this build
        /// does not recognise, or an ending this client drove locally
        /// without a server frame.
        ///
        /// `None` and `Some(Requested)` are both the quiet, expected ending
        /// and explain themselves; every other reason is something the user
        /// did not ask for and gets words on the cooked terminal.
        reason: Option<DetachReason>,
    },
    /// The last pane's process exited, so there was nothing left to
    /// render or route input to and the consumer-owned detach policy
    /// (phux-4r1) left the session.
    LastPaneClosed {
        /// The dead pane's `_exit(n)` code, or `None` for signal kills /
        /// unknown causes — the same shape `TERMINAL_CLOSED` carries on
        /// the wire.
        exit_status: Option<i32>,
    },
}

impl AttachEnd {
    /// One-line explanation for the cooked terminal after teardown, or
    /// `None` when the ending needs no words (a plain detach).
    ///
    /// Printed by `exit_after_detach` on the production path (which
    /// exits the process before the CLI regains control — see its doc
    /// comment) and available to CLI callers holding a returned
    /// `AttachEnd` on any path that does return.
    #[must_use]
    pub fn explanation(self) -> Option<String> {
        match self {
            // A detach the user asked for needs no words. Anything else —
            // the server shut down, the session was killed, another client
            // took over, the connection broke the protocol — is an ending
            // the user did not choose, and before phux-l83x the wire could
            // not tell them apart.
            Self::Detached { reason } => match reason {
                None | Some(DetachReason::Requested) => None,
                Some(reason) => Some(format!("phux: detached: {}", reason.describe())),
            },
            Self::LastPaneClosed { exit_status } => Some(format!(
                "phux: session ended: the last pane {}",
                describe_exit(exit_status),
            )),
        }
    }
}

/// phux-i0e8.2.2: human phrase for a `TERMINAL_CLOSED` exit status.
///
/// The wire carries `Some(n)` for a plain `_exit(n)` and `None` for
/// signal kills / unknown causes (frame.rs `TerminalClosed`). One
/// spelling shared by the survivor notice and the last-pane exit
/// explanation, so both surfaces read as one vocabulary.
pub(super) fn describe_exit(exit_status: Option<i32>) -> String {
    exit_status.map_or_else(
        || "killed (signal or unknown)".to_owned(),
        |code| format!("exited {code}"),
    )
}

#[cfg(test)]
mod tests {
    /// phux-i0e8.2.2: one wording for every exit shape, shared by the
    /// survivor notice and the last-pane explanation.
    #[test]
    fn describe_exit_covers_all_shapes() {
        assert_eq!(super::describe_exit(Some(0)), "exited 0");
        assert_eq!(super::describe_exit(Some(137)), "exited 137");
        assert_eq!(super::describe_exit(Some(-1)), "exited -1");
        assert_eq!(super::describe_exit(None), "killed (signal or unknown)");
    }

    /// The link between "the WebSocket keepalive noticed a stalled peer" and
    /// "the client enters its reconnect window": `attach_with_reconnect`
    /// reconnects on `Disconnected` and on nothing else, so a `Stalled` that
    /// mapped to `Io` or `Connect` would detect the network switch and then
    /// exit on it anyway. The other variants must NOT collapse into
    /// `Disconnected` — a pin mismatch or an unreachable host is a fault to
    /// report, not a blip to wait out.
    #[test]
    fn a_stalled_lane_maps_to_disconnected_and_nothing_else_does() {
        assert!(matches!(
            super::AttachError::from(phux_dial::DialError::Stalled("no pong".to_owned())),
            super::AttachError::Disconnected
        ));

        assert!(matches!(
            super::AttachError::from(phux_dial::DialError::Unreachable("no route".to_owned())),
            super::AttachError::Unreachable(_)
        ));
        assert!(matches!(
            super::AttachError::from(phux_dial::DialError::Connect("pin mismatch".to_owned())),
            super::AttachError::Connect(_)
        ));
        assert!(matches!(
            super::AttachError::from(phux_dial::DialError::Io(std::io::Error::from(
                std::io::ErrorKind::BrokenPipe
            ))),
            super::AttachError::Io(_)
        ));
    }
}
