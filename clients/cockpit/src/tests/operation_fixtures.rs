//! Canonical protocol-codec fixtures for the owning-thread operation tests.
//! Regenerate with clients/cockpit/scripts/generate-operation-fixtures.sh.
use bytes::{Bytes, BytesMut};
use phux_protocol::wire::frame::{
    Command, CommandResult, ErrorCode, FrameKind, SpawnError, SpawnResult,
};
use phux_protocol::{
    BootstrapId, BootstrapStreamProfile, GroupId, SatelliteHost, StreamId, TerminalId,
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

fn bootstrap(id: TerminalId) -> Vec<FrameKind> {
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
    write(
        dir,
        "detach-request.bin",
        vec![FrameKind::Command {
            request_id: 1,
            command: Command::DetachTerminal {
                terminal_id: TerminalId::local(7),
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
            command: Command::AttachTerminal {
                terminal_id: TerminalId::local(7),
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
    write(dir, "reattach-ready.bin", bootstrap(TerminalId::local(7)))?;
    let mut churn = Vec::new();
    for n in 0..20 {
        let id = TerminalId::local(100 + n);
        churn.push(FrameKind::TerminalSpawned {
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
    let local = TerminalId::local(8);
    write(
        dir,
        "spawned-terminal-closed.bin",
        vec![FrameKind::TerminalClosed {
            terminal_id: local.clone(),
            exit_status: Some(0),
        }],
    )?;
    write(
        dir,
        "remote-title.bin",
        vec![FrameKind::TerminalOutput {
            terminal_id: TerminalId::local(7),
            stream_id: StreamId::new(7).unwrap(),
            bootstrap_id: BootstrapId::new(1).unwrap(),
            seq: 1,
            bytes: Bytes::from_static(b"\x1b]2;remote-title-review\x07"),
        }],
    )?;
    let satellite = TerminalId::satellite(SatelliteHost::new("build-host"), 9);
    write(
        dir,
        "initial-terminal-closed.bin",
        vec![FrameKind::TerminalClosed {
            terminal_id: TerminalId::local(7),
            exit_status: Some(0),
        }],
    )?;
    for (name, host, owner) in [
        ("spawn-owner-request.bin", None, Some(TerminalId::local(7))),
        (
            "spawn-satellite-request.bin",
            Some(SatelliteHost::new("build-host")),
            Some(TerminalId::satellite(SatelliteHost::new("build-host"), 6)),
        ),
    ] {
        write(
            dir,
            name,
            vec![FrameKind::SpawnTerminal {
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
            }],
        )?;
    }
    write(
        dir,
        "attach-satellite-request.bin",
        vec![FrameKind::Command {
            request_id: 2,
            command: Command::AttachTerminal {
                terminal_id: satellite.clone(),
            },
        }],
    )?;
    write(
        dir,
        "spawn-local.bin",
        vec![FrameKind::TerminalSpawned {
            request_id: 1,
            result: SpawnResult::Ok(local.clone()),
        }],
    )?;
    write(
        dir,
        "spawn-satellite.bin",
        vec![FrameKind::TerminalSpawned {
            request_id: 1,
            result: SpawnResult::Ok(satellite.clone()),
        }],
    )?;
    write(
        dir,
        "spawn-refused.bin",
        vec![FrameKind::TerminalSpawned {
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
            command: Command::AttachTerminal {
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

fn search_resize() -> Vec<FrameKind> {
    let terminal_id = TerminalId::local(7);
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
