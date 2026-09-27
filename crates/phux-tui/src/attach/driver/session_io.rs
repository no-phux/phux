//! Connection-side helpers shared by the entry points, the headless
//! composite, and the main loop: the ATTACH handshake, frame acks, and
//! terminal-reply plumbing.

use std::io::{self};

use phux_protocol::caps::{
    BootstrapLimits, ClientCapabilities, Layer, LayerSet, ServerFeature, detect_color_support,
};
use phux_protocol::ids::ResourceId;
use phux_protocol::wire::frame::{AttachTarget, FrameKind};

use crate::attach::connection::Connection;
use crate::attach::outcome::AttachError;
use crate::attach::server_frame::FrameOutcome;
use crate::render::chrome::status_bar::Notice;

use super::viewport::current_viewport;

/// Whether to emit a `FRAME_ACK`: only a `StateSync` consumer's acks feed the
/// server's RTT/backpressure accounting; a raw consumer's are dropped, so they
/// are skipped.
pub(super) fn should_emit_frame_ack(
    wants_state_sync: bool,
    ack: Option<(
        ResourceId,
        phux_protocol::StreamId,
        phux_protocol::BootstrapId,
        u64,
    )>,
) -> Option<(
    ResourceId,
    phux_protocol::StreamId,
    phux_protocol::BootstrapId,
    u64,
)> {
    wants_state_sync.then_some(ack).flatten()
}

pub(super) fn take_terminal_replies(
    outcome: &mut FrameOutcome,
    terminal_reply_supported: bool,
) -> Vec<(ResourceId, Vec<u8>)> {
    // An ending outcome has no PTY left to answer.
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
    replies: Vec<(ResourceId, Vec<u8>)>,
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

/// Whether a write failed because the peer already closed the connection.
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

/// Send a frame, treating "the peer already hung up" as success. The read
/// side owns the ending: the frames explaining why the session ended (say a
/// last-pane `RESOURCE_CLOSED`) are already buffered, and failing on an ack
/// written into the closed socket used to replace "the last pane exited 7"
/// with "Broken pipe". Every write here is advisory or a request whose answer
/// can no longer arrive, so nothing is lost.
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
/// The reference TUI's per-connection HELLO profile, passed to the dial
/// itself so no path can double-HELLO.
pub(super) fn attach_client_caps(
    default_colors: Option<phux_protocol::caps::TerminalDefaultColors>,
    dial: &crate::attach::Dial,
) -> ClientCapabilities {
    // Color tier from the environment (SPEC §6.2); L3 for layout metadata.
    let bootstrap = phux_client_core::engine::ghostty::native_bootstrap_capabilities(
        BootstrapLimits::default(),
    );
    let mut client_caps = ClientCapabilities::new()
        .with_bootstrap(bootstrap)
        .with_color_support(detect_color_support())
        .with_layers(LayerSet::with(&[Layer::L3]));
    if let Some(colors) = default_colors {
        client_caps = client_caps.with_default_colors(colors);
    }
    // Offer frame compression on remote lanes only (proto.md §6.4): a remote
    // native bootstrap deflates ~14x, while UDS would just burn CPU.
    if !matches!(dial, crate::attach::Dial::Uds(_)) {
        client_caps = client_caps.with_compression(phux_protocol::caps::CompressionSet::all());
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
    // ADR-0127: `--viewer` / `--take`, or nothing for the default.
    let role_policy = crate::attach::attach_role::attach_role_for(conn)?;
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
        role_policy,
    })
    .await?;
    Ok(attach_id)
}

/// Read frames until `ATTACHED`, mapping `ERROR` to `AttachError::Refused`
/// and anything else to `Protocol`. Runs on the cooked terminal.
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
            // ATTACH must be answered with ATTACHED or ERROR.
            Err(AttachError::Protocol(
                phux_client::explain::unexpected_reply("ATTACH"),
            ))
        }
    }
}
