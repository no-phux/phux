//! Canonical wire input for the Zig C-grid style projection regression.
//! Run `cargo run --locked -p phux-client-ffi --example cockpit_style_fixture`.

use std::{error::Error, path::Path};

use bytes::{Bytes, BytesMut};
use phux_protocol::caps::ServerCapabilities;
use phux_protocol::wire::frame::FrameKind;
use phux_protocol::wire::info::{ResourceInfo, SessionInfo, SessionSnapshot, WindowInfo};
use phux_protocol::{
    BootstrapId, BootstrapLimits, BootstrapProfile, BootstrapStreamProfile, ClientId,
    PROTOCOL_VERSION, ResourceId, SessionId, StreamId, WindowId,
};

// Each row resets attributes. Truecolor values are fixture inputs, not a
// presentation palette. The combining cluster, CJK tail and wrap-head exercise
// real libghostty cell occupancy rather than fabricated C records.
const VT: &[u8] = concat!(
    "\x1b[2J\x1b[H",
    "\x1b]10;#c0b0a0\x1b\\\x1b]11;#102840\x1b\\\x1b]12;#80a0c0\x1b\\",
    "\x1b]4;1;#102030;9;#8090a0\x1b\\",
    "\x1b[38;2;180;120;60;48;2;20;40;80;58;2;60;90;150m",
    "\x1b[1;3;9;53;4:3mA\x1b[0m\r\n",
    "\x1b[4:0m0\x1b[4:1m1\x1b[4:2m2\x1b[4:3m3\x1b[4:4m4\x1b[4:5m5\x1b[0m\r\n",
    "\x1b[38;2;180;120;60;48;2;20;40;80m",
    "\x1b[7mI\x1b[27;2mF\x1b[7mJ\x1b[0;8;9;4:2mH\x1b[0m",
    "─e\u{301}界",
    "\x1b]8;;https://example.test/owned\x1b\\L\x1b]8;;\x1b\\",
    "\x1b[4;16H界",
    "\x1b[6;1H\x1b[0;1;31mB\x1b[0;7;4mU\x1b[0m",
    "\x1b[1;38;2;16;32;48mE\x1b[0;1;91mR\x1b[0;1;38;5;200mC",
    "\x1b[0;2;4mD\x1b[48;2;240;80;20mQ\x1b[0;1;31;7;4mX",
    "\x1b[0;31;7;4;58;2;16;32;48mY\x1b[0m",
    "\x1b[5;4H\x1b[44m\x1b[3X\x1b[0m\x1b[5;8H\x1b[48;2;20;60;100m\x1b[2X\x1b[0m",
    "\x1b[3;7H\x1b[1 q",
)
.as_bytes();

fn main() -> Result<(), Box<dyn Error>> {
    let output = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../clients/cockpit/src/providers/phux/style_fixture");
    std::fs::create_dir_all(&output)?;
    write(
        &output,
        "hello",
        &FrameKind::HelloOk {
            protocol_major: PROTOCOL_VERSION.major,
            protocol_minor: PROTOCOL_VERSION.minor,
            protocol_patch: PROTOCOL_VERSION.patch,
            server_caps: ServerCapabilities::new(),
            server_id: b"cockpit-styles".to_vec(),
            selected_profile: BootstrapProfile::SynthesizedVtRaw,
            bootstrap_limits: BootstrapLimits::new(1024, 1024).ok_or("invalid limits")?,
        },
    )?;
    let terminal_id = ResourceId::local(7);
    let session_id = SessionId::new(1);
    let window_id = WindowId::new(1);
    let stream_id = StreamId::new(7).ok_or("invalid stream")?;
    let bootstrap_id = BootstrapId::new(1).ok_or("invalid bootstrap")?;
    let snapshot = SessionSnapshot::new(session_id, window_id, terminal_id.clone())
        .with_sessions(vec![SessionInfo::new(session_id, "styles")])
        .with_windows(vec![WindowInfo::new(window_id, session_id, "styles")])
        .with_resources(vec![ResourceInfo::new(
            terminal_id.clone(),
            window_id,
            16,
            6,
        )]);
    write(
        &output,
        "attached",
        &FrameKind::Attached {
            attach_id: 1,
            snapshot,
            initial_client_id: ClientId::new(1),
        },
    )?;
    write(
        &output,
        "begin",
        &FrameKind::BootstrapBegin {
            terminal_id: terminal_id.clone(),
            stream_id,
            bootstrap_id,
            profile: BootstrapStreamProfile::SynthesizedVtRaw,
            cols: 16,
            rows: 6,
            base_seq: 0,
        },
    )?;
    write(
        &output,
        "chunk",
        &FrameKind::BootstrapChunk {
            terminal_id: terminal_id.clone(),
            stream_id,
            bootstrap_id,
            chunk_seq: 0,
            payload: Bytes::from_static(VT),
        },
    )?;
    write(
        &output,
        "ready",
        &FrameKind::BootstrapReady {
            terminal_id,
            stream_id,
            bootstrap_id,
            history_cursor: None,
        },
    )?;
    write(
        &output,
        "attach-ready",
        &FrameKind::AttachReady { attach_id: 1 },
    )?;
    update(&output, "tail", 1, b"\x1b[3;8H\x1b[2 q")?;
    update(
        &output,
        "reset-colors",
        2,
        b"\x1b]110\x1b\\\x1b]111\x1b\\\x1b]112\x1b\\\x1b]104\x1b\\",
    )?;
    update(&output, "reverse", 3, b"\x1b[?5h")
}

fn update(output: &Path, name: &str, seq: u64, bytes: &'static [u8]) -> Result<(), Box<dyn Error>> {
    write(
        output,
        name,
        &FrameKind::ResourceOutput {
            terminal_id: ResourceId::local(7),
            stream_id: StreamId::new(7).ok_or("invalid stream")?,
            bootstrap_id: BootstrapId::new(1).ok_or("invalid bootstrap")?,
            seq,
            bytes: Bytes::from_static(bytes),
        },
    )
}

fn write(output: &Path, name: &str, frame: &FrameKind) -> Result<(), Box<dyn Error>> {
    let mut bytes = BytesMut::new();
    frame.encode(&mut bytes);
    std::fs::write(output.join(format!("{name}.bin")), bytes)?;
    Ok(())
}
