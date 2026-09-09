//! Generate protocol-0.8 workspace replies for Cockpit's existing terminal-7 attach fixture.
//! Usage: `cargo run -p phux-client-ffi --example workspace_fixtures -- OUTPUT_DIRECTORY`.
//! Requests follow initial read, refresh, rename, split, resize. Pass output bytes
//! through the ordinary FFI frame feeder; no emulator allocation is needed for listings.

use phux_client_core::layout::{self, Workspace};
use phux_protocol::wire::frame::{CommandResult, CommandValue, FrameKind};
use phux_protocol::wire::info::{ResourceInfo, SessionInfo, SessionSnapshot, WindowInfo};
use phux_protocol::{ResourceId, SessionId, WindowId};
use std::{error::Error, path::Path};

fn snapshot(extra: bool) -> SessionSnapshot {
    let mut snapshot =
        SessionSnapshot::new(SessionId::new(1), WindowId::new(1), ResourceId::local(7))
            .with_sessions(vec![SessionInfo::new(SessionId::new(1), "fixture")])
            .with_windows(vec![WindowInfo::new(
                WindowId::new(1),
                SessionId::new(1),
                "registry",
            )])
            .with_resources(vec![ResourceInfo::new(
                ResourceId::local(7),
                WindowId::new(1),
                80,
                24,
            )]);
    if extra {
        snapshot
            .sessions
            .push(SessionInfo::new(SessionId::new(2), "external"));
        snapshot.windows.push(WindowInfo::new(
            WindowId::new(2),
            SessionId::new(2),
            "external",
        ));
        snapshot.resources.extend([
            ResourceInfo::new(ResourceId::local(8), WindowId::new(1), 80, 24)
                .with_title(Some("unplaced terminal".into())),
            ResourceInfo::new(ResourceId::local(9), WindowId::new(2), 80, 24),
        ]);
        snapshot.focused_session = SessionId::new(2);
    }
    snapshot
}

fn write(directory: &Path, name: &str, frame: &FrameKind) -> Result<(), Box<dyn Error>> {
    let mut bytes = bytes::BytesMut::new();
    frame.encode(&mut bytes);
    std::fs::write(directory.join(name), bytes)?;
    Ok(())
}

fn pair(
    directory: &Path,
    name: &str,
    state: u32,
    metadata: u32,
    extra: bool,
    topology: Option<&Workspace>,
) -> Result<(), Box<dyn Error>> {
    write_pair(
        directory,
        name,
        state,
        metadata,
        snapshot(extra),
        topology.map(Workspace::encode_topology_cbor).transpose()?,
    )
}

fn write_pair(
    directory: &Path,
    name: &str,
    state: u32,
    metadata: u32,
    snapshot: SessionSnapshot,
    value: Option<Vec<u8>>,
) -> Result<(), Box<dyn Error>> {
    write(
        directory,
        &format!("{name}_state.bin"),
        &FrameKind::CommandResult {
            request_id: 0x8000_0000 + state,
            result: CommandResult::OkWith(CommandValue::State(snapshot)),
        },
    )?;
    write(
        directory,
        &format!("{name}_metadata.bin"),
        &FrameKind::MetadataValue {
            request_id: 0x8000_0000 + metadata,
            value,
        },
    )
}

fn cutover_fixtures(directory: &Path) -> Result<(), Box<dyn Error>> {
    // Complete previous schema with an empty window array. It is present data,
    // not permission to generate fallback or to replace the server's bytes.
    write_pair(
        directory,
        "workspace_old_schema",
        2,
        3,
        snapshot(false),
        Some(b"\xa3\x67version\x02\x67windows\x80\x74focused_window_index\x00".to_vec()),
    )?;
    let mut topology = Workspace::single(ResourceId::local(7));
    topology.add_window(String::new(), ResourceId::local(8));
    pair(directory, "workspace_add", 13, 14, true, Some(&topology))
}

fn main() -> Result<(), Box<dyn Error>> {
    let directory = std::env::args_os()
        .nth(1)
        .ok_or("output directory required")?;
    let directory = Path::new(&directory);
    std::fs::create_dir_all(directory)?;
    let mut topology = Workspace::single(ResourceId::local(7));
    pair(directory, "workspace_initial", 0, 1, false, None)?;
    pair(directory, "workspace_refresh", 2, 3, true, Some(&topology))?;
    topology.windows[0].name = "renamed".into();
    pair(directory, "workspace_rename", 4, 5, true, Some(&topology))?;
    topology.windows[0].state.tree = Some(layout::split_at(
        topology.windows[0]
            .state
            .tree
            .as_ref()
            .ok_or("missing seed tree")?,
        &ResourceId::local(7),
        &ResourceId::local(8),
        layout::SplitDir::Horizontal,
        0.5,
    )?);
    pair(directory, "workspace_split", 7, 8, true, Some(&topology))?;
    if let Some(layout::LayoutNode::Split { ratio, .. }) = &mut topology.windows[0].state.tree {
        *ratio = 0.7;
    }
    pair(directory, "workspace_resize", 10, 11, true, Some(&topology))?;
    cutover_fixtures(directory)?;
    Ok(())
}
