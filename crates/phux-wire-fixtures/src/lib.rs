//! Frame builders for client-side tests: the frames a server would send,
//! built from one vocabulary instead of a struct literal per test file.
//!
//! Dev-only. Depends on `phux-protocol` alone so it compiles for wasm and
//! links into every client crate's tests, including the separate `rune`
//! and `phux-web` workspaces.
//!
//! Values are never defaulted where tests disagree: a [`HelloOk`] names its
//! server id and capabilities, because the existing suites assert different
//! ones.

#![allow(
    clippy::expect_used,
    clippy::panic,
    reason = "test fixtures: a malformed fixture is a test bug and should fail loudly"
)]

use bytes::{Bytes, BytesMut};
use phux_protocol::PROTOCOL_VERSION;
use phux_protocol::caps::{
    BootstrapCapabilities, BootstrapLimits, BootstrapProfile, BootstrapStreamProfile,
    ClientCapabilities, Layer, LayerSet, ServerCapabilities, ServerFeatureExtSet, ServerFeatureSet,
    select_bootstrap_profile,
};
use phux_protocol::ids::{BootstrapId, ClientId, ResourceId, StreamId};
use phux_protocol::wire::frame::{CloseReason, CommandResult, CommandValue, ErrorCode, FrameKind};
use phux_protocol::wire::info::SessionSnapshot;

/// A server advertising layer 3 and no features.
#[must_use]
pub const fn l3_caps() -> ServerCapabilities {
    ServerCapabilities::new().with_layers(LayerSet::with(&[Layer::L3]))
}

/// Builds a `HELLO_OK`.
///
/// The server id and capabilities are required: suites pin different ones
/// (`[0xAB; 16]`, `b"incarnation-1"`, a per-server byte, empty). Everything
/// else defaults to this build's protocol version, raw synthesized VT, and
/// default limits.
#[derive(Debug, Clone)]
pub struct HelloOk {
    server_id: Vec<u8>,
    caps: ServerCapabilities,
    patch: u16,
    profile: BootstrapProfile,
    limits: BootstrapLimits,
}

impl HelloOk {
    /// A `HELLO_OK` from `server_id` advertising `caps`.
    #[must_use]
    pub fn new(server_id: impl Into<Vec<u8>>, caps: ServerCapabilities) -> Self {
        Self {
            server_id: server_id.into(),
            caps,
            patch: PROTOCOL_VERSION.patch,
            profile: BootstrapProfile::SynthesizedVtRaw,
            limits: BootstrapLimits::default(),
        }
    }

    /// Report protocol patch `patch` instead of this build's.
    #[must_use]
    pub const fn patch(mut self, patch: u16) -> Self {
        self.patch = patch;
        self
    }

    /// Replace the advertised feature word.
    #[must_use]
    pub const fn features(mut self, features: ServerFeatureSet) -> Self {
        self.caps = self.caps.with_features(features);
        self
    }

    /// Replace the advertised extended feature word.
    #[must_use]
    pub const fn features_ext(mut self, features_ext: ServerFeatureExtSet) -> Self {
        self.caps = self.caps.with_features_ext(features_ext);
        self
    }

    /// Select `profile` with `limits`.
    #[must_use]
    pub const fn profile_limits(
        mut self,
        profile: BootstrapProfile,
        limits: BootstrapLimits,
    ) -> Self {
        self.profile = profile;
        self.limits = limits;
        self
    }

    /// Select what a default server would pick for `offered`.
    ///
    /// # Panics
    ///
    /// When `offered` shares no bootstrap profile with a default server.
    #[must_use]
    pub fn negotiated_from(self, offered: &ClientCapabilities) -> Self {
        let (profile, limits) = select_bootstrap_profile(offered, &BootstrapCapabilities::new())
            .expect("the client and a default server share a bootstrap profile");
        self.profile_limits(profile, limits)
    }

    /// The frame.
    #[must_use]
    pub fn build(self) -> FrameKind {
        FrameKind::HelloOk {
            protocol_major: PROTOCOL_VERSION.major,
            protocol_minor: PROTOCOL_VERSION.minor,
            protocol_patch: self.patch,
            server_caps: self.caps,
            server_id: self.server_id,
            selected_profile: Some(self.profile),
            bootstrap_limits: self.limits,
        }
    }
}

impl From<HelloOk> for FrameKind {
    fn from(hello: HelloOk) -> Self {
        hello.build()
    }
}

/// How a [`bootstrap_frames_with`] transcript differs from the plain one.
#[derive(Debug, Clone)]
pub struct BootstrapOptions {
    /// The stream profile `BOOTSTRAP_BEGIN` names.
    pub profile: BootstrapStreamProfile,
    /// The output sequence the bootstrap stands at.
    pub base_seq: u64,
    /// The history cursor `BOOTSTRAP_READY` carries.
    pub history_cursor: Option<Vec<u8>>,
}

impl Default for BootstrapOptions {
    fn default() -> Self {
        Self {
            profile: BootstrapStreamProfile::SynthesizedVtRaw,
            base_seq: 0,
            history_cursor: None,
        }
    }
}

const fn stream_ids([stream, bootstrap]: [u64; 2]) -> (StreamId, BootstrapId) {
    let stream_id = StreamId::new(stream).expect("a nonzero stream id");
    let bootstrap_id = BootstrapId::new(bootstrap).expect("a nonzero bootstrap id");
    (stream_id, bootstrap_id)
}

/// A whole raw synthesized bootstrap of `terminal` on `[stream, bootstrap]`
/// at `[cols, rows]`: begin, one chunk of `payload`, ready.
///
/// # Panics
///
/// When either id is zero.
#[must_use]
pub fn bootstrap_frames(
    terminal: &ResourceId,
    ids: [u64; 2],
    size: [u16; 2],
    payload: &[u8],
) -> [FrameKind; 3] {
    bootstrap_frames_with(terminal, ids, size, payload, BootstrapOptions::default())
}

/// [`bootstrap_frames`] with a non-default profile, base sequence, or
/// history cursor.
///
/// # Panics
///
/// When either id is zero.
#[must_use]
pub fn bootstrap_frames_with(
    terminal: &ResourceId,
    ids: [u64; 2],
    [cols, rows]: [u16; 2],
    payload: &[u8],
    options: BootstrapOptions,
) -> [FrameKind; 3] {
    let (stream_id, bootstrap_id) = stream_ids(ids);
    [
        FrameKind::BootstrapBegin {
            terminal_id: terminal.clone(),
            stream_id,
            bootstrap_id,
            profile: options.profile,
            cols,
            rows,
            base_seq: options.base_seq,
        },
        FrameKind::BootstrapChunk {
            terminal_id: terminal.clone(),
            stream_id,
            bootstrap_id,
            chunk_seq: 0,
            payload: Bytes::copy_from_slice(payload),
        },
        FrameKind::BootstrapReady {
            terminal_id: terminal.clone(),
            stream_id,
            bootstrap_id,
            history_cursor: options.history_cursor.map(Bytes::from),
        },
    ]
}

/// `bytes` of `terminal` at `seq`, on stream 1 of bootstrap 1.
#[must_use]
pub fn output(terminal: &ResourceId, seq: u64, bytes: &[u8]) -> FrameKind {
    output_on(terminal, [1, 1], seq, bytes)
}

/// `bytes` of `terminal` at `seq`, on `[stream, bootstrap]`.
///
/// # Panics
///
/// When either id is zero.
#[must_use]
pub fn output_on(terminal: &ResourceId, ids: [u64; 2], seq: u64, bytes: &[u8]) -> FrameKind {
    let (stream_id, bootstrap_id) = stream_ids(ids);
    FrameKind::ResourceOutput {
        terminal_id: terminal.clone(),
        stream_id,
        bootstrap_id,
        seq,
        bytes: Bytes::copy_from_slice(bytes),
    }
}

/// A bare `OK` answer to `request_id`.
#[must_use]
pub const fn command_ok(request_id: u32) -> FrameKind {
    FrameKind::CommandResult {
        request_id,
        result: CommandResult::Ok,
    }
}

/// An `OK_WITH(value)` answer to `request_id`.
#[must_use]
pub const fn command_value(request_id: u32, value: CommandValue) -> FrameKind {
    FrameKind::CommandResult {
        request_id,
        result: CommandResult::OkWith(value),
    }
}

/// A refusal of `request_id` with `code` and `message`.
#[must_use]
pub fn command_err(request_id: u32, code: ErrorCode, message: &str) -> FrameKind {
    FrameKind::CommandResult {
        request_id,
        result: CommandResult::Error {
            code,
            message: message.to_owned(),
        },
    }
}

/// The answer to `ATTACH` `attach_id`: `snapshot`, as client `client_id`.
#[must_use]
pub const fn attached(attach_id: u32, snapshot: SessionSnapshot, client_id: u32) -> FrameKind {
    FrameKind::Attached {
        attach_id,
        snapshot,
        initial_client_id: ClientId::new(client_id),
    }
}

/// `terminal` ended with `exit_status`, for no recorded reason or signal.
#[must_use]
pub fn closed(terminal: &ResourceId, exit_status: Option<i32>) -> FrameKind {
    FrameKind::ResourceClosed {
        terminal_id: terminal.clone(),
        exit_status,
        reason: CloseReason::Unknown,
        signal: None,
    }
}

/// `frame` on the wire.
#[must_use]
pub fn encode(frame: &FrameKind) -> Vec<u8> {
    let mut bytes = BytesMut::new();
    frame.encode(&mut bytes);
    bytes.to_vec()
}

/// The one frame `bytes` holds.
///
/// # Panics
///
/// When `bytes` does not decode, or holds more than one frame.
#[must_use]
pub fn decode_one(bytes: &[u8]) -> FrameKind {
    let (frame, rest) = FrameKind::decode(bytes).expect("the frame decodes");
    assert!(rest.is_empty(), "one frame, no trailing bytes");
    frame
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_built_hello_round_trips() {
        let hello = HelloOk::new([0xAB; 16], l3_caps()).patch(3).build();
        assert_eq!(decode_one(&encode(&hello)), hello);
    }

    #[test]
    fn a_bootstrap_transcript_carries_its_options() {
        let options = BootstrapOptions {
            history_cursor: Some(b"cursor".to_vec()),
            base_seq: 4,
            ..BootstrapOptions::default()
        };
        let [begin, _, ready] =
            bootstrap_frames_with(&ResourceId::local(7), [2, 3], [20, 4], b"x", options);
        assert!(matches!(
            begin,
            FrameKind::BootstrapBegin {
                base_seq: 4,
                cols: 20,
                ..
            }
        ));
        assert!(matches!(
            ready,
            FrameKind::BootstrapReady { history_cursor: Some(ref c), .. } if &c[..] == b"cursor"
        ));
    }
}
