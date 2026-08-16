//! Connection-side helpers shared by the entry points, the headless
//! composite, and the main loop: the ATTACH handshake, frame acks, and
//! terminal-reply plumbing.

use std::io::{self};

#[cfg(not(all(feature = "native-engine", not(target_arch = "wasm32"))))]
use phux_protocol::caps::BootstrapCapabilities;
use phux_protocol::caps::{
    BootstrapLimits, ClientCapabilities, Layer, LayerSet, ServerFeature, detect_color_support,
};
use phux_protocol::ids::TerminalId;
use phux_protocol::wire::frame::{AttachTarget, FrameKind};

use crate::attach::connection::Connection;
use crate::attach::outcome::AttachError;
use crate::attach::server_frame::FrameOutcome;
use crate::render::chrome::status_bar::Notice;

use super::viewport::current_viewport;

/// Whether to emit a `FRAME_ACK` for an applied `TERMINAL_OUTPUT`.
///
/// Acks are load-bearing only for a `StateSync` consumer: the server
/// folds each into that consumer's per-seq RTT/backpressure accounting
/// (`on_frame_ack`). A raw broadcast consumer's acks carry no seq the
/// server tracks, so it drops them — emitting one is a wasted client
/// write plus a server decode and state lock on the same UDS that carries
/// keystrokes during a repaint storm. In raw mode the ack is skipped; in
/// state-sync mode the `(terminal_id, seq)` flows through unchanged.
///
/// Not `const`: the `(TerminalId, u64)` it threads carries a non-trivial
/// destructor (the federation `TerminalId::Satellite` variant owns a
/// `String`), which a `const fn` may not drop at compile time.
pub(super) fn should_emit_frame_ack(
    wants_state_sync: bool,
    ack: Option<(
        TerminalId,
        phux_protocol::StreamId,
        phux_protocol::BootstrapId,
        u64,
    )>,
) -> Option<(
    TerminalId,
    phux_protocol::StreamId,
    phux_protocol::BootstrapId,
    u64,
)> {
    wants_state_sync.then_some(ack).flatten()
}

pub(super) fn take_terminal_replies(
    outcome: &mut FrameOutcome,
    terminal_reply_supported: bool,
) -> Vec<(TerminalId, Vec<u8>)> {
    // phux-501l (hardening, not the fix — see `peer_gone` for that): an
    // outcome that ends the attach loop should not put another byte on the
    // wire. A terminal reply is addressed to a pane's PTY, and once an outcome
    // carries `exit` there is no pane left to route it to.
    //
    // This matters because the call site sends replies BEFORE it inspects
    // `outcome.exit`, so an outcome carrying both would write into a session
    // it is in the middle of abandoning. No current handler produces that
    // combination — the last-pane-closed branch returns a `FrameOutcome` with
    // `pty_writes` defaulted empty — so this closes a latent hole rather than
    // an observed one. It is cheap and it makes the ordering at the call site
    // stop mattering.
    if outcome.exit {
        outcome.pty_writes.clear();
        return Vec::new();
    }
    if terminal_reply_supported {
        return std::mem::take(&mut outcome.pty_writes);
    }
    if outcome.pty_writes.is_empty() {
        return Vec::new();
    }
    outcome.pty_writes.clear();
    let message = "terminal query reply not sent: server lacks terminal-reply support";
    tracing::warn!(feature = ?ServerFeature::TerminalReply, "{message}");
    outcome.notices.push(Notice::warn(message));
    Vec::new()
}

pub(super) async fn send_terminal_replies(
    conn: &mut Connection,
    replies: Vec<(TerminalId, Vec<u8>)>,
) -> Result<(), AttachError> {
    for (terminal_id, bytes) in replies {
        send_unless_peer_gone(
            conn,
            &FrameKind::InputTerminalReply {
                terminal_id,
                bytes: bytes::Bytes::from(bytes),
            },
        )
        .await?;
    }
    Ok(())
}

/// Whether a write failed because the peer had already closed the connection.
///
/// `BrokenPipe` is the local half discovering the socket is gone;
/// `ConnectionReset`/`ConnectionAborted` are the same discovery on the
/// transports where the kernel reports it that way. None of them describe a
/// fault in *this* process — they describe a peer that is no longer there.
pub(super) fn peer_gone(err: &AttachError) -> bool {
    matches!(
        err,
        AttachError::Io(inner)
            if matches!(
                inner.kind(),
                io::ErrorKind::BrokenPipe
                    | io::ErrorKind::ConnectionReset
                    | io::ErrorKind::ConnectionAborted
            )
    )
}

/// Send a frame, treating "the peer already hung up" as success.
///
/// phux-501l. THE READ SIDE OWNS THE ENDING. A failed write tells us only that
/// the connection is gone; the reason a session ended is carried by frames the
/// server sent *before* it went, and those are already sitting in our decode
/// buffer. Failing the attach loop on the write throws that reason away and
/// replaces it with the mechanical symptom.
///
/// Concretely, the bug this fixes: the last pane's shell exits, so the server
/// emits `TERMINAL_OUTPUT` (the shell's final bytes) immediately followed by
/// `TERMINAL_CLOSED`, then reaps the now-empty session and exits, closing the
/// UDS. The client's next read pulls BOTH frames into one coalesced batch. It
/// processes the `TERMINAL_OUTPUT` first and answers it with a `FRAME_ACK` — a
/// write, into a socket the server has already closed. That returns EPIPE, `?`
/// turns it into `AttachError::Io`, and the loop dies **without ever
/// processing the `TERMINAL_CLOSED` sitting in the same batch**. The user typed
/// `exit 7` and got
///     phux: attach failed: attach loop io error: Broken pipe (os error 32)
/// instead of "session ended: the last pane exited 7".
///
/// Whether the write lost that race was pure scheduling, which is why it read
/// as a flake — nextest's retries had been hiding it on `main`, and it failed
/// 6/6 on the runners where the server won.
///
/// Swallowing the error loses nothing. Every write in the inbound arm is
/// either advisory (`FRAME_ACK` is flow-control accounting the server drops
/// outright for a raw consumer) or a request whose answer can no longer
/// arrive. In both cases the loop continues, drains the frames it already
/// holds, and ends for the reason those frames give: `LastPaneClosed` here, or
/// `AttachError::Disconnected` from the EOF on the following `recv` if the
/// batch carried no ending. Both are strictly better than `Io`, and neither
/// can mask a live-server fault — if the server were still there, the write
/// would not have failed.
pub(super) async fn send_unless_peer_gone(
    conn: &mut Connection,
    frame: &FrameKind,
) -> Result<(), AttachError> {
    match conn.send(frame).await {
        Ok(()) => Ok(()),
        Err(err) if peer_gone(&err) => {
            tracing::debug!(
                ?err,
                "write dropped: peer already closed; letting the read side name the ending",
            );
            Ok(())
        }
        Err(err) => Err(err),
    }
}
/// Build the reference TUI's per-connection HELLO profile.
///
/// The same value is passed to [`Connection::connect_dial_with_hello`] before
/// any ATTACH can be sent. Keeping it as data rather than a second handshake
/// routine prevents reconnect and custom-capability paths from double-HELLO.
pub(super) fn attach_client_caps(
    default_colors: Option<phux_protocol::caps::TerminalDefaultColors>,
) -> ClientCapabilities {
    // Sniff `$COLORTERM` / `$TERM` / `$TERM_PROGRAM` per
    // `detect_color_support`. The advertised tier feeds the server's
    // per-client `downsample::rewrite_bytes` (SPEC §6.2).
    //
    // phux-4li.5: declare L3 (`Layer::L3`) so the server forwards
    // `MetadataChanged` events for the `phux.tui.layout/v1` key.
    #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
    let bootstrap = phux_client_core::engine::ghostty::native_bootstrap_capabilities(
        BootstrapLimits::default(),
    );
    #[cfg(not(all(feature = "native-engine", not(target_arch = "wasm32"))))]
    let bootstrap = BootstrapCapabilities::new().with_limits(BootstrapLimits::default());
    let mut client_caps = ClientCapabilities::new()
        .with_bootstrap(bootstrap)
        .with_color_support(detect_color_support())
        .with_layers(LayerSet::with(&[Layer::L3]));
    if let Some(colors) = default_colors {
        client_caps = client_caps.with_default_colors(colors);
    }
    client_caps
}

pub(super) fn attach_client_name() -> String {
    format!("phux-client/{}", env!("CARGO_PKG_VERSION"))
}

/// Send the `ATTACH` frame using the current terminal viewport.
pub(super) async fn send_attach(
    conn: &mut Connection,
    target: AttachTarget,
) -> Result<u32, AttachError> {
    let viewport = current_viewport()?;
    let attach_id = conn.next_attach_id();
    conn.send(&FrameKind::Attach {
        attach_id,
        target,
        viewport,
        // SPEC §13: clients SHOULD opt in to scrollback. The cap below
        // matches the default in docs/consumers/tui.md §X; a configurable knob lives
        // with the rest of `phux-config`.
        request_scrollback: true,
        scrollback_limit_lines: 10_000,
    })
    .await?;
    Ok(attach_id)
}

/// Read frames off `conn` until we get the expected `ATTACHED` reply,
/// surfacing a structured `Error` frame as `AttachError::Refused` and
/// any other unexpected frame as `AttachError::Protocol`.
///
/// Runs entirely on the cooked terminal (pre-`RawModeGuard`) per
/// `phux-roz`. A server-side reject prints an actionable error on the
/// normal screen and exits without flicker.
pub(super) async fn wait_for_attached(
    conn: &mut Connection,
    expected_attach_id: u32,
) -> Result<FrameKind, AttachError> {
    let frame = conn.recv().await?;
    match frame {
        FrameKind::Attached { attach_id, .. } if attach_id == expected_attach_id => Ok(frame),
        FrameKind::Attached { attach_id, .. } => Err(AttachError::Protocol(format!(
            "ATTACHED attach_id mismatch: sent {expected_attach_id}, received {attach_id}",
        ))),
        FrameKind::Error {
            code: _, message, ..
        } => Err(AttachError::Refused(message)),
        _ => {
            // Anything else this early is a protocol violation. The
            // server is required to answer `ATTACH` with either
            // `ATTACHED` or `ERROR`; reject otherwise rather than
            // silently soldiering on into a half-attached state.
            Err(AttachError::Protocol(crate::explain::unexpected_reply(
                "ATTACH",
            )))
        }
    }
}
