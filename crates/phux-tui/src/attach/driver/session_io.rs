//! Connection-side helpers shared by the entry points, the headless
//! composite, and the main loop: the ATTACH handshake and frame acks.

use std::io::{self};

use phux_protocol::caps::{
    BootstrapLimits, ClientCapabilities, Layer, LayerSet, detect_color_support,
};
use phux_protocol::ids::ResourceId;
use phux_protocol::wire::frame::{AttachTarget, ErrorCode, FrameKind, ViewportInfo};

use crate::attach::connection::Connection;
use crate::attach::outcome::AttachError;

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
/// side owns the ending: failing here would replace "the last pane exited 7"
/// with "Broken pipe", and no answer could arrive anyway.
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
        .with_layers(LayerSet::with(&[Layer::L3]))
        // The TUI draws terminals and agent sessions, no tree, surface or
        // document, so the server streams it none.
        .with_resource_kinds(phux_protocol::caps::ResourceKindSet::with(&[
            phux_protocol::ids::ResourceKind::Terminal,
            phux_protocol::ids::ResourceKind::AgentSession,
        ]));
    if let Some(colors) = default_colors {
        client_caps = client_caps.with_default_colors(colors);
    }
    // Offer frame compression on remote lanes only (docs/protocol.md#framing): a remote
    // native bootstrap deflates ~14x, while UDS would just burn CPU.
    if !matches!(dial, crate::attach::Dial::Uds(_)) {
        client_caps = client_caps.with_compression(phux_protocol::caps::CompressionSet::all());
    }
    client_caps
}

pub(super) fn attach_client_name() -> String {
    format!("phux-client/{}", env!("CARGO_PKG_VERSION"))
}

/// Send the TUI's `ATTACH` with no window-size vote (ADR-0145): the server
/// applies per-pane cell size, and the outer window is not any pane's size.
/// A zero viewport resizes nothing on attach and re-resolves nothing on
/// detach (L1 §9), so a read-only view leaves live panes alone.
pub(super) async fn send_attach(
    conn: &mut Connection,
    target: AttachTarget,
) -> Result<u32, AttachError> {
    // ADR-0127: `--viewer` / `--take`, or nothing for the default.
    let role_policy = crate::attach::attach_role::attach_role_for(conn)?;
    let attach_id = conn.next_attach_id();
    conn.send(&FrameKind::Attach {
        attach_id,
        target,
        viewport: ViewportInfo::new(0, 0),
        // SPEC §13: clients SHOULD opt in to scrollback. The cap below
        // matches the default in docs/config.md; a configurable knob lives
        // with the rest of `phux-config`.
        request_scrollback: true,
        scrollback_limit_lines: 10_000,
        role_policy,
    })
    .await?;
    Ok(attach_id)
}

/// Read frames until `ATTACHED` on the cooked terminal. An `ERROR` is
/// `Refused`, except `VERSION_INCOMPATIBLE` ([`AttachError::IncompatibleProtocol`]).
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
        FrameKind::Error { code, message, .. } => Err(if code == ErrorCode::VersionIncompatible {
            AttachError::IncompatibleProtocol(message)
        } else {
            AttachError::Refused(message)
        }),
        _ => {
            // ATTACH must be answered with ATTACHED or ERROR.
            Err(AttachError::Protocol(
                phux_client::explain::unexpected_reply("ATTACH"),
            ))
        }
    }
}

/// Make the session `new-session` asked for persistent by creating it
/// headless with that lifetime, as `phux new` does; the `CreateIfMissing`
/// attach then opens it. Skipped without the organization plane.
pub(super) async fn create_persistent_session(conn: &mut Connection, target: &AttachTarget) {
    let AttachTarget::CreateIfMissing { name, cwd, .. } = target else {
        return;
    };
    if !phux_client::org::supported(conn) {
        return;
    }
    let request = phux_client::session::CreateSessionRequest {
        name,
        command: None,
        cwd: cwd.as_deref(),
        env: &std::collections::BTreeMap::new(),
        agent_session: None,
        idempotency_key: None,
        lifetime: phux_protocol::wire::frame::SessionLifetime::Persistent,
        attrs: &[],
    };
    let created = phux_client::session::create_session(conn, &request, &mut Vec::new()).await;
    tracing::debug!(session = %name, ?created, "new-session: persistent create");
}
