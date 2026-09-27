//! Helpers shared by the attach suites.

#![allow(
    clippy::redundant_pub_crate,
    reason = "`unreachable_pub` demands pub(crate) in a test-binary module"
)]

use std::path::{Path, PathBuf};

use phux_protocol::PROTOCOL_VERSION;
use phux_protocol::caps::ClientCapabilities;
use phux_protocol::ids::ResourceId;
use phux_protocol::input::key::{KeyAction, KeyEvent, ModSet, PhysicalKey};
use phux_protocol::wire::frame::{AttachTarget, FrameKind, ViewportInfo};
use phux_protocol::wire::info::SessionSnapshot;
use phux_server_testkit::screen::Screen;
use phux_server_testkit::{
    SOCKET_CONNECT_DEADLINE, WIRE_RECV_TIMEOUT, ascii_key, recv_typed, recv_until,
    recv_until_deadline, send_frame, wait_for_raw_socket,
};
use portable_pty::CommandBuilder;
use tokio::net::UnixStream;

/// `/bin/sh -c script`.
pub(crate) fn sh(script: &str) -> CommandBuilder {
    let mut cmd = CommandBuilder::new("/bin/sh");
    cmd.args(["-c", script]);
    cmd
}

/// `ATTACH` with attach id 1 and no scrollback.
pub(crate) const fn attach(target: AttachTarget, cols: u16, rows: u16) -> FrameKind {
    FrameKind::Attach {
        attach_id: 1,
        target,
        viewport: ViewportInfo::new(cols, rows),
        request_scrollback: false,
        scrollback_limit_lines: 0,
        role_policy: None,
    }
}

/// `frame` (an `ATTACH`) with a different, connection-unique `attach_id`.
pub(crate) const fn with_attach_id(mut frame: FrameKind, id: u32) -> FrameKind {
    if let FrameKind::Attach { attach_id, .. } = &mut frame {
        *attach_id = id;
    }
    frame
}

/// `ATTACH { CreateIfMissing }` at 80x24.
pub(crate) fn create_if_missing(
    name: &str,
    command: Option<Vec<String>>,
    cwd: Option<String>,
) -> FrameKind {
    let target = AttachTarget::CreateIfMissing {
        name: name.to_owned(),
        command,
        cwd,
    };
    attach(target, 80, 24)
}

/// Read up to `ATTACHED` and return its snapshot.
pub(crate) async fn attached(stream: &mut UnixStream) -> SessionSnapshot {
    recv_until(stream, |_, frame| match frame {
        FrameKind::Attached { snapshot, .. } => Some(snapshot),
        _ => None,
    })
    .await
}

/// The focused session's name.
pub(crate) fn focused_name(snapshot: &SessionSnapshot) -> &str {
    &snapshot
        .sessions
        .iter()
        .find(|session| session.id == snapshot.focused_session)
        .expect("focused session listed")
        .name
}

/// Connect and complete HELLO with `caps`, asserting `HELLO_OK`.
pub(crate) async fn connect_with(path: &Path, caps: ClientCapabilities) -> UnixStream {
    let mut stream = wait_for_raw_socket(path, SOCKET_CONNECT_DEADLINE).await;
    send_frame(
        &mut stream,
        &FrameKind::Hello {
            client_name: "phux-attach-test".to_owned(),
            protocol_major: PROTOCOL_VERSION.major,
            protocol_minor: PROTOCOL_VERSION.minor,
            protocol_patch: PROTOCOL_VERSION.patch,
            client_caps: caps,
        },
    )
    .await;
    let (_, reply) = recv_typed(&mut stream).await;
    assert!(matches!(reply, FrameKind::HelloOk { .. }), "{reply:?}");
    stream
}

/// Enter with no text; the encoder synthesizes the CR.
pub(crate) const fn enter_key() -> KeyEvent {
    KeyEvent {
        action: KeyAction::Press,
        key: PhysicalKey::Enter,
        mods: ModSet::empty(),
        consumed_mods: ModSet::empty(),
        composing: false,
        text: None,
        unshifted_codepoint: None,
    }
}

/// Type `c` then Enter into `pane` (cooked-mode `cat` echoes on Enter).
pub(crate) async fn type_line(
    stream: &mut UnixStream,
    pane: &ResourceId,
    c: char,
    key: PhysicalKey,
) {
    for event in [ascii_key(c, key), enter_key()] {
        let frame = FrameKind::InputKey {
            terminal_id: pane.clone(),
            event,
        };
        send_frame(stream, &frame).await;
    }
}

/// Feed `RESOURCE_OUTPUT` into `screen` until it shows `needle`.
pub(crate) async fn render_until(stream: &mut UnixStream, screen: &mut Screen, needle: &str) {
    let deadline = tokio::time::Instant::now() + WIRE_RECV_TIMEOUT;
    let found = recv_until_deadline(stream, deadline, |_, frame| {
        if let FrameKind::ResourceOutput { bytes, .. } = frame {
            screen.write(&bytes);
        }
        screen.contains(needle).then_some(())
    })
    .await;
    assert!(
        found.is_some(),
        "never rendered {needle:?}:\n{}",
        screen.snapshot_text()
    );
}

/// Accumulate `RESOURCE_OUTPUT` bytes until they contain `needle`.
pub(crate) async fn output_until(stream: &mut UnixStream, needle: &[u8]) -> Vec<u8> {
    let deadline = tokio::time::Instant::now() + WIRE_RECV_TIMEOUT;
    let mut acc = Vec::new();
    let found = recv_until_deadline(stream, deadline, |_, frame| {
        if let FrameKind::ResourceOutput { bytes, .. } = frame {
            acc.extend_from_slice(&bytes);
        }
        contains(&acc, needle).then_some(())
    })
    .await;
    assert!(
        found.is_some(),
        "output never contained {:?}; got {:?}",
        String::from_utf8_lossy(needle),
        String::from_utf8_lossy(&acc)
    );
    acc
}

pub(crate) fn contains(hay: &[u8], needle: &[u8]) -> bool {
    hay.windows(needle.len()).any(|window| window == needle)
}

/// The pane's reported cwd, canonicalized (macOS /var -> /private/var).
pub(crate) fn canonical_cwd(snapshot: &SessionSnapshot, pane: usize) -> PathBuf {
    let cwd = PathBuf::from(
        snapshot.resources[pane]
            .cwd
            .as_deref()
            .expect("a PTY-backed pane carries a cwd"),
    );
    cwd.canonicalize().unwrap_or(cwd)
}
