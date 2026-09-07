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
    let local = TerminalId::local(8);
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
