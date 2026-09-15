//! Canonical protocol-codec fixtures for the owning-thread operation tests.
//! Regenerate with clients/cockpit/scripts/generate-operation-fixtures.sh.
use bytes::{Bytes, BytesMut};
use phux_protocol::wire::frame::{
    AgentEvent, CloseReason, Command, CommandResult, CommandValue, ErrorCode, FrameKind,
    SpawnError, SpawnResult,
};
use phux_protocol::wire::info::{ResourceInfo, SessionInfo, SessionSnapshot, WindowInfo};
use phux_protocol::{
    BootstrapId, BootstrapStreamProfile, GroupId, SatelliteHost, StreamId, ResourceId,
    SessionId, WindowId,
};
use std::{error::Error, path::Path};

fn write(dir: &Path, name: &str, frames: Vec<FrameKind>) -> Result<(), Box<dyn Error>> {
    let mut encoded = BytesMut::new();
    for frame in frames {
        frame.encode(&mut encoded);
    }
    std::fs::write(dir.join(name), encoded)?;
    Ok(())
}

fn bootstrap(id: ResourceId) -> Vec<FrameKind> {
    let stream_id = StreamId::new(17).unwrap();
    let bootstrap_id = BootstrapId::new(1).unwrap();
    vec![
        FrameKind::BootstrapBegin {
            terminal_id: id.clone(),
            stream_id,
            bootstrap_id,
            profile: BootstrapStreamProfile::SynthesizedVtRaw,
            cols: 80,
            rows: 24,
            base_seq: 0,
        },
        FrameKind::BootstrapChunk {
            terminal_id: id.clone(),
            stream_id,
            bootstrap_id,
            chunk_seq: 0,
            payload: Bytes::from_static(b"OPERATION READY"),
        },
        FrameKind::BootstrapReady {
            terminal_id: id,
            stream_id,
            bootstrap_id,
            history_cursor: None,
        },
    ]
}

fn main() -> Result<(), Box<dyn Error>> {
    let out = std::env::args().nth(1).expect("output directory");
    let dir = Path::new(&out);
    write(dir, "search-resize.bin", search_resize())?;
    for seq in 1..=4 {
        write(dir, &format!("remote-bell-{seq}.bin"), vec![FrameKind::ResourceOutput {
            terminal_id: ResourceId::local(7),
            stream_id: StreamId::new(7).unwrap(),
            bootstrap_id: BootstrapId::new(1).unwrap(),
            seq,
            bytes: Bytes::from_static(b"\x07"),
        }])?;
    }
    write(
        dir,
        "detach-request.bin",
        vec![FrameKind::Command {
            request_id: 1,
            command: Command::DetachResource {
                terminal_id: ResourceId::local(7),
            },
        }],
    )?;
    write(
        dir,
        "detach-ok.bin",
        vec![FrameKind::CommandResult {
            request_id: 1,
            result: CommandResult::Ok,
        }],
    )?;
    write(
        dir,
        "reattach-request.bin",
        vec![FrameKind::Command {
            request_id: 2,
            command: Command::AttachResource {
                terminal_id: ResourceId::local(7),
            },
        }],
    )?;
    write(
        dir,
        "reattach-ok.bin",
        vec![FrameKind::CommandResult {
            request_id: 2,
            result: CommandResult::Ok,
        }],
    )?;
    write(dir, "reattach-ready.bin", bootstrap(ResourceId::local(7)))?;
    let mut churn = Vec::new();
    for n in 0..20 {
        let id = ResourceId::local(100 + n);
        churn.push(FrameKind::ResourceSpawned {
            request_id: 2 * n + 1,
            result: SpawnResult::Ok(id.clone()),
        });
        churn.extend(bootstrap(id));
        churn.push(FrameKind::CommandResult {
            request_id: 2 * n + 2,
            result: CommandResult::Ok,
        });
    }
    write(dir, "detach-churn.bin", churn)?;
    for (name, seq, bytes) in [
        ("clear-parser-prefix.bin", 1, b"\x1b[3".as_slice()),
        ("clear-parser-suffix.bin", 2, b"1mX".as_slice()),
    ] {
        write(dir, name, vec![FrameKind::ResourceOutput {
            terminal_id: ResourceId::local(7),
            stream_id: StreamId::new(7).unwrap(),
            bootstrap_id: BootstrapId::new(1).unwrap(),
            seq,
            bytes: Bytes::copy_from_slice(bytes),
        }])?;
    }
    let local = ResourceId::local(8);
    write(
        dir,
        "spawned-terminal-closed.bin",
        vec![FrameKind::ResourceClosed {
            terminal_id: local.clone(),
            exit_status: Some(0),
            reason: phux_protocol::wire::frame::CloseReason::Unknown,
            signal: None,
        }],
    )?;
    write(
        dir,
        "remote-title.bin",
        vec![FrameKind::ResourceOutput {
            terminal_id: ResourceId::local(7),
            stream_id: StreamId::new(7).unwrap(),
            bootstrap_id: BootstrapId::new(1).unwrap(),
            seq: 1,
            bytes: Bytes::from_static(b"\x1b]2;remote-title-review\x07"),
        }],
    )?;
    let satellite = ResourceId::satellite(SatelliteHost::new("build-host"), 9);
    write(
        dir,
        "initial-terminal-closed.bin",
        vec![FrameKind::ResourceClosed {
            terminal_id: ResourceId::local(7),
            exit_status: Some(0),
            reason: phux_protocol::wire::frame::CloseReason::Unknown,
            signal: None,
        }],
    )?;
    write_status_fixtures(dir)?;
    for (name, host, owner) in [
        ("spawn-owner-request.bin", None, Some(ResourceId::local(7))),
        (
            "spawn-satellite-request.bin",
            Some(SatelliteHost::new("build-host")),
            Some(ResourceId::satellite(SatelliteHost::new("build-host"), 6)),
        ),
    ] {
        write(
            dir,
            name,
            vec![FrameKind::SpawnResource {
                request_id: 1,
                group: GroupId::new(1),
                command: None,
                cwd: None,
                env: None,
                term: None,
                satellite: host,
                owner_terminal: owner,
                agent_session: None,
                initial_size: Some((80, 24)),
                resource: None,
            }],
        )?;
    }
    write(
        dir,
        "attach-satellite-request.bin",
        vec![FrameKind::Command {
            request_id: 2,
            command: Command::AttachResource {
                terminal_id: satellite.clone(),
            },
        }],
    )?;
    write(
        dir,
        "spawn-local.bin",
        vec![FrameKind::ResourceSpawned {
            request_id: 1,
            result: SpawnResult::Ok(local.clone()),
        }],
    )?;
    write(
        dir,
        "spawn-satellite.bin",
        vec![FrameKind::ResourceSpawned {
            request_id: 1,
            result: SpawnResult::Ok(satellite.clone()),
        }],
    )?;
    write(
        dir,
        "spawn-refused.bin",
        vec![FrameKind::ResourceSpawned {
            request_id: 1,
            result: SpawnResult::Err(SpawnError::SpawnFailed("fixture refusal".into())),
        }],
    )?;
    write(
        dir,
        "attach-refused.bin",
        vec![FrameKind::CommandResult {
            request_id: 1,
            result: CommandResult::Error {
                code: ErrorCode::InvalidCommand,
                message: "attach refusal".into(),
            },
        }],
    )?;
    write(
        dir,
        "attach-local-request.bin",
        vec![FrameKind::Command {
            request_id: 1,
            command: Command::AttachResource {
                terminal_id: local.clone(),
            },
        }],
    )?;
    write(
        dir,
        "restore-accepted.bin",
        vec![FrameKind::CommandResult {
            request_id: 1,
            result: CommandResult::Ok,
        }],
    )?;
    write(dir, "local-ready.bin", bootstrap(local))?;
    write(dir, "satellite-ready.bin", bootstrap(satellite))?;
    write(
        dir,
        "attach-accepted.bin",
        vec![FrameKind::CommandResult {
            request_id: 2,
            result: CommandResult::Ok,
        }],
    )?;
    Ok(())
}

/// PHA-284: the subscribed terminal-process facts phux-client-ffi folds into
/// PHUX_CLIENT_STATUS_CWD/_COMMAND_STARTED/_COMMAND_FINISHED/_EXITED, each
/// scoped to the attached fixture terminal (local 7).
fn write_status_fixtures(dir: &Path) -> Result<(), Box<dyn Error>> {
    let event = |event| {
        vec![FrameKind::Event {
            terminal: Some(ResourceId::local(7)),
            event,
            // Unjournaled: the fixture server does not advertise EVENT_JOURNAL.
            stamp: None,
        }]
    };
    let closed = |exit_status, reason| {
        vec![FrameKind::ResourceClosed {
            terminal_id: ResourceId::local(7),
            exit_status,
            reason,
            signal: None,
        }]
    };
    let cwd = AgentEvent::CwdChanged {
        cwd: "/srv/work/cockpit-fixture".to_owned(),
    };
    write(dir, "remote-cwd.bin", event(cwd))?;
    write(dir, "remote-command-started.bin", event(AgentEvent::CommandStarted))?;
    let finished = AgentEvent::CommandFinished { exit_code: Some(2) };
    write(dir, "remote-command-finished.bin", event(finished))?;
    write(dir, "remote-exited.bin", closed(Some(0), CloseReason::Exited))?;
    write(dir, "remote-killed.bin", closed(None, CloseReason::Killed))?;
    let root = AgentEvent::CwdChanged { cwd: "/".to_owned() };
    write(dir, "remote-cwd-root.bin", event(root))?;
    let overlong = AgentEvent::CwdChanged {
        cwd: format!("/{}", "x".repeat(5000)),
    };
    write(dir, "remote-cwd-overlong.bin", event(overlong))?;
    let titles = (1..=100)
        .map(|seq| FrameKind::ResourceOutput {
            terminal_id: ResourceId::local(7),
            stream_id: StreamId::new(7).unwrap(),
            bootstrap_id: BootstrapId::new(1).unwrap(),
            seq,
            bytes: Bytes::from(format!("\x1b]2;burst-{seq}\x07")),
        })
        .collect();
    write(dir, "remote-title-burst.bin", titles)?;
    write_status_workspace_fixtures(dir)
}

/// Workspace reads around terminal 7's end, correlated as a client's first
/// refresh after attach (internal requests 2 and 3); tests re-correlate them.
/// No layout metadata, so the registry is the fallback topology.
fn write_status_workspace_fixtures(dir: &Path) -> Result<(), Box<dyn Error>> {
    let sessions = |keep_empty: bool| {
        vec![SessionInfo::new(SessionId::new(1), "fixture").with_keep_empty(keep_empty)]
    };
    let live = |keep_empty: bool| {
        SessionSnapshot::new(SessionId::new(1), WindowId::new(1), ResourceId::local(7))
            .with_sessions(sessions(keep_empty))
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
            )])
    };
    // The session after its last terminal ended: no windows, no resources.
    let ended = |keep_empty: bool| {
        SessionSnapshot::new(SessionId::new(1), WindowId::new(0), ResourceId::local(0))
            .with_sessions(sessions(keep_empty))
    };
    let state = |snapshot: SessionSnapshot| {
        vec![FrameKind::CommandResult {
            request_id: 0x8000_0002,
            result: CommandResult::OkWith(CommandValue::State(snapshot)),
        }]
    };
    write(dir, "status-keep-empty-state.bin", state(live(true)))?;
    write(dir, "status-keep-empty-ended-state.bin", state(ended(true)))?;
    write(dir, "status-ended-state.bin", state(ended(false)))?;
    let metadata = FrameKind::MetadataValue {
        request_id: 0x8000_0003,
        value: None,
    };
    write(dir, "status-no-layout-metadata.bin", vec![metadata])
}

fn search_resize() -> Vec<FrameKind> {
    let terminal_id = ResourceId::local(7);
    let stream_id = StreamId::new(7).unwrap();
    let bootstrap_id = BootstrapId::new(2).unwrap();
    vec![
        FrameKind::BootstrapBegin {
            terminal_id: terminal_id.clone(),
            stream_id,
            bootstrap_id,
            profile: BootstrapStreamProfile::SynthesizedVtRaw,
            cols: 80,
            rows: 22,
            base_seq: 0,
        },
        FrameKind::BootstrapChunk {
            terminal_id: terminal_id.clone(),
            stream_id,
            bootstrap_id,
            chunk_seq: 0,
            payload: Bytes::from_static(b"COCKPIT COCKPIT COCKPIT"),
        },
        FrameKind::BootstrapReady {
            terminal_id,
            stream_id,
            bootstrap_id,
            history_cursor: None,
        },
    ]
}
