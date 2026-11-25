//! The fed-frame fixture the `it` and `perf_counters` binaries share: a plane
//! past `HELLO_OK`, attached to one 20x4 terminal named `main`, and (with the
//! engine) an embedded client holding that terminal's replica.

#![allow(
    unreachable_pub,
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    dead_code,
    reason = "shared test support; each binary uses a different part"
)]

use phux_client_runtime::control::{ControlOptions, ControlPlane, Status};
#[cfg(feature = "engine")]
use phux_protocol::PROTOCOL_VERSION;
use phux_protocol::ids::{ResourceId, SessionId, WindowId};
#[cfg(feature = "engine")]
use phux_protocol::wire::frame::AttachTarget;
use phux_protocol::wire::frame::{Command, CommandResult, FrameKind};
use phux_protocol::wire::info::{ResourceInfo, SessionInfo, SessionSnapshot, WindowInfo};
use phux_wire_fixtures::{BootstrapOptions, HelloOk, bootstrap_frames_with, l3_caps};

pub use phux_wire_fixtures::decode_one as decode;

pub const fn terminal() -> ResourceId {
    ResourceId::local(7)
}

/// The fixture server's `HELLO_OK`, to adjust: server `[0xAB; 16]`, layer 3.
pub fn server_hello() -> HelloOk {
    HelloOk::new([0xAB; 16], l3_caps())
}

pub fn hello_ok(patch: u16) -> FrameKind {
    server_hello().patch(patch).build()
}

pub fn snapshot() -> SessionSnapshot {
    SessionSnapshot::new(SessionId::new(1), WindowId::new(1), terminal())
        .with_sessions(vec![SessionInfo::new(SessionId::new(1), "main")])
        .with_windows(vec![WindowInfo::new(
            WindowId::new(1),
            SessionId::new(1),
            "shell",
        )])
        .with_resources(vec![ResourceInfo::new(terminal(), WindowId::new(1), 20, 4)])
}

pub fn attach_with_history(
    plane: &mut ControlPlane,
    attach_id: u32,
    bytes: &[u8],
    history_cursor: Option<Vec<u8>>,
) {
    plane.attached(attach_id, snapshot());
    let options = BootstrapOptions {
        history_cursor,
        ..BootstrapOptions::default()
    };
    for frame in bootstrap_frames_with(&terminal(), [1, 1], [20, 4], bytes, options) {
        plane.feed(frame).expect("bootstrap frame");
    }
    plane
        .feed(FrameKind::AttachReady { attach_id })
        .expect("ATTACH_READY");
}

#[cfg(feature = "engine")]
pub fn embedded_with_history(history_cursor: Option<Vec<u8>>) -> phux_client_runtime::Client {
    let client = phux_client_runtime::Runtime::embedded(ControlOptions {
        attach: Some(AttachTarget::ByName("main".into())),
        viewport: (20, 4),
        ..ControlOptions::default()
    });
    client.with_control(ControlPlane::connection_opened);
    let _ = client.take_outbound();
    client.feed(hello_ok(PROTOCOL_VERSION.patch)).unwrap();
    let attach_id = client
        .take_outbound()
        .iter()
        .find_map(|bytes| match decode(bytes) {
            FrameKind::Attach { attach_id, .. } => Some(attach_id),
            _ => None,
        })
        .unwrap();
    client.with_control(|plane| {
        attach_with_history(
            plane,
            attach_id,
            b"zero\r\none\r\ntwo\r\nthree\r\nfour\r\nfive",
            history_cursor,
        );
    });
    let _ = client.take_outbound();
    client
}

/// Shorthands for feeding a plane the frames a server would answer with.
pub trait PlaneExt {
    /// Answer `request_id` with `result`.
    fn reply(&mut self, request_id: u32, result: CommandResult);
    /// Drain the queued frames and return the `ATTACH` id among them.
    fn queued_attach_id(&mut self) -> u32;
    /// Answer `ATTACH` `attach_id` with `snapshot`.
    fn attached(&mut self, attach_id: u32, snapshot: SessionSnapshot);
}

impl PlaneExt for ControlPlane {
    fn reply(&mut self, request_id: u32, result: CommandResult) {
        answer(self, request_id, result);
    }

    fn queued_attach_id(&mut self) -> u32 {
        self.take_outbound()
            .iter()
            .find_map(|bytes| match decode(bytes) {
                FrameKind::Attach { attach_id, .. } => Some(attach_id),
                _ => None,
            })
            .expect("ATTACH")
    }

    fn attached(&mut self, attach_id: u32, snapshot: SessionSnapshot) {
        self.feed(phux_wire_fixtures::attached(attach_id, snapshot, 1))
            .expect("ATTACHED");
    }
}

impl PlaneExt for phux_client_runtime::Client {
    fn reply(&mut self, request_id: u32, result: CommandResult) {
        self.feed(FrameKind::CommandResult { request_id, result })
            .expect("COMMAND_RESULT");
    }

    fn queued_attach_id(&mut self) -> u32 {
        self.with_control(PlaneExt::queued_attach_id)
    }

    fn attached(&mut self, attach_id: u32, snapshot: SessionSnapshot) {
        self.feed(phux_wire_fixtures::attached(attach_id, snapshot, 1))
            .expect("ATTACHED");
    }
}

/// Open a fresh connection and answer its `HELLO` with `hello`, dropping
/// the opening frames.
pub fn reopen(plane: &mut ControlPlane, hello: FrameKind) {
    plane.connection_opened();
    let _ = plane.take_outbound();
    plane.feed(hello).expect("HELLO_OK");
}

/// A plane with `options`, connected and answered with `hello`.
pub fn opened(options: ControlOptions, hello: FrameKind) -> ControlPlane {
    let mut plane = ControlPlane::new(std::time::Instant::now(), options);
    reopen(&mut plane, hello);
    plane
}

/// Answer `request_id` with `result`.
pub fn answer(plane: &mut ControlPlane, request_id: u32, result: CommandResult) {
    plane
        .feed(FrameKind::CommandResult { request_id, result })
        .expect("COMMAND_RESULT");
}

/// Every outbound `COMMAND`, with its request id.
pub fn commands(plane: &mut ControlPlane) -> Vec<(u32, Command)> {
    plane
        .take_outbound()
        .iter()
        .filter_map(|bytes| match decode(bytes) {
            FrameKind::Command {
                request_id,
                command,
            } => Some((request_id, command)),
            _ => None,
        })
        .collect()
}

/// A plane opened with `options` and fed `hello`, with the follow-up
/// frames that queued, decoded. The opening must be exactly one `HELLO`, and
/// `expected_followups`, when given, the number of follow-ups.
pub fn negotiate(
    options: ControlOptions,
    hello: FrameKind,
    expected_followups: Option<usize>,
) -> (ControlPlane, Vec<FrameKind>) {
    let mut plane = ControlPlane::new(std::time::Instant::now(), options);
    assert_eq!(plane.status(), Status::Idle);
    plane.connection_opened();
    assert_eq!(plane.status(), Status::Connecting);
    let opening = plane.take_outbound();
    assert_eq!(opening.len(), 1, "the opening is one HELLO");
    assert!(matches!(decode(&opening[0]), FrameKind::Hello { .. }));
    plane.feed(hello).expect("HELLO_OK");
    let followups: Vec<FrameKind> = plane.take_outbound().iter().map(|b| decode(b)).collect();
    if let Some(expected) = expected_followups {
        assert_eq!(followups.len(), expected, "{followups:?}");
    }
    (plane, followups)
}
