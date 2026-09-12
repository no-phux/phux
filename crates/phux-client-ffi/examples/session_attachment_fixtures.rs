//! Derive an independent session's canonical replies for the Cockpit B18 test.
//! Run `cargo run --locked -p phux-client-ffi --example session_attachment_fixtures --profile ffi-dev`.
//! The same terminal, window and stream numbers intentionally occur in both
//! clients; only the session and terminal output differ.
use bytes::{Bytes, BytesMut};
use phux_protocol::SessionId;
use phux_protocol::wire::frame::{CommandResult, CommandValue, FrameKind};
use phux_protocol::wire::info::{SessionInfo, SessionSnapshot};
use std::{error::Error, path::Path};

fn second_session(snapshot: &mut SessionSnapshot) {
    snapshot.focused_session = SessionId::new(2);
    for session in &mut snapshot.sessions {
        session.id = SessionId::new(2);
        session.name = "independent".into();
    }
    for window in &mut snapshot.windows {
        window.session_id = SessionId::new(2);
    }
}

fn rewrite(
    directory: &Path,
    input: &str,
    output: &str,
    second: bool,
) -> Result<(), Box<dyn Error>> {
    let source = std::fs::read(directory.join(input))?;
    let mut remaining = source.as_slice();
    let mut encoded = BytesMut::new();
    while !remaining.is_empty() {
        let (mut frame, rest) = FrameKind::decode(remaining)?;
        match &mut frame {
            FrameKind::Attached { snapshot, .. }
            | FrameKind::CommandResult {
                result: CommandResult::OkWith(CommandValue::State(snapshot)),
                ..
            } => {
                if second {
                    second_session(snapshot);
                } else {
                    snapshot
                        .sessions
                        .push(SessionInfo::new(SessionId::new(2), "deploy"));
                }
            }
            FrameKind::BootstrapChunk { payload, .. } => {
                *payload = Bytes::from_static(b"\x1b[2J\x1b[HINDEPENDENT B\x1b[?1004h");
            }
            _ => {}
        }
        frame.encode(&mut encoded);
        remaining = rest;
    }
    std::fs::write(directory.join(output), encoded)?;
    Ok(())
}

fn main() -> Result<(), Box<dyn Error>> {
    let directory =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../clients/cockpit/src/tests/fixtures");
    rewrite(&directory, "attached.bin", "attached_session_b.bin", true)?;
    rewrite(
        &directory,
        "../../providers/phux/fixtures/workspace_initial_state.bin",
        "workspace_session_b_state.bin",
        true,
    )?;
    rewrite(
        &directory,
        "../../providers/phux/fixtures/workspace_initial_state.bin",
        "workspace_sessions_a_state.bin",
        false,
    )
}
