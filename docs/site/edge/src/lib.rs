//! phux-edge — a phux *server* that runs as WASM inside a Cloudflare Durable
//! Object. It speaks the **real** phux wire (`phux-protocol`) to the browser
//! client, backed by a curated [`Shell`] that emits VT bytes. No OS, no
//! processes, no container — so it runs free on the edge (Workers/DO), and the
//! browser's libghostty-vt engine renders exactly what crosses the wire.
//!
//! The Durable Object is a dumb pipe: every inbound WebSocket message is one
//! encoded `FrameKind`; hand it to [`EdgeSession::on_message`] and send back
//! each returned frame as its own WebSocket message.

mod shell;

use bytes::{Bytes, BytesMut};
use phux_protocol::PROTOCOL_VERSION;
use phux_protocol::caps::{
    BootstrapLimits, BootstrapProfile, BootstrapStreamProfile, ServerCapabilities,
};
use phux_protocol::ids::{BootstrapId, ClientId, ResourceId, SessionId, StreamId, WindowId};
use phux_protocol::wire::frame::{FrameKind, TYPE_FRAME_COMPRESSED};
#[cfg(test)]
use phux_protocol::wire::frame::{AttachTarget, ViewportInfo};
use phux_protocol::wire::info::{ResourceInfo, SessionSnapshot};
use serde::{Deserialize, Serialize};
use wasm_bindgen::prelude::*;

use shell::{Shell, ShellCheckpoint};

const CHECKPOINT_VERSION: u8 = 1;
const MAX_VIEWPORT: u16 = 1000;

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Checkpoint {
    version: u8,
    kind: CheckpointKind,
    cols: u16,
    rows: u16,
    seq: u64,
    shell: ShellCheckpoint,
}

#[derive(Deserialize, Serialize)]
enum CheckpointKind {
    #[serde(rename = "edge-session")]
    EdgeSession,
}

/// One live wire session: the curated shell + the bits of server state the wire
/// needs (a terminal id, an output sequence counter, the viewport size).
#[wasm_bindgen]
pub struct EdgeSession {
    terminal_id: ResourceId,
    cols: u16,
    rows: u16,
    seq: u64,
    shell: Shell,
}

#[wasm_bindgen]
impl EdgeSession {
    /// Create a session. `cols`/`rows` are defaults until the client's `ATTACH`
    /// reports its viewport.
    #[wasm_bindgen(constructor)]
    #[must_use]
    pub fn new(cols: u16, rows: u16, mode: &str, snapshot_json: &str) -> Self {
        Self {
            terminal_id: ResourceId::new(1),
            cols,
            rows,
            seq: 0,
            shell: Shell::new(mode, snapshot_json),
        }
    }

    /// Serialize logical session state only. The caller persists mode and the
    /// portfolio snapshot separately and supplies them again to `restore`.
    #[must_use]
    pub fn checkpoint(&self) -> String {
        serde_json::to_string(&Checkpoint {
            version: CHECKPOINT_VERSION,
            kind: CheckpointKind::EdgeSession,
            cols: self.cols,
            rows: self.rows,
            seq: self.seq,
            shell: self.shell.checkpoint(),
        })
        .expect("checkpoint is serializable")
    }

    #[wasm_bindgen(js_name = restore)]
    pub fn restore(
        checkpoint_json: &str,
        mode: &str,
        snapshot_json: &str,
    ) -> Result<Self, JsValue> {
        Self::restore_inner(checkpoint_json, mode, snapshot_json)
            .map_err(|message| js_sys::Error::new(&message).into())
    }

    /// Handle one inbound WebSocket message (one encoded `FrameKind`). Returns a
    /// JS array of `Uint8Array`s — each is one frame to send back, one WS
    /// message per element.
    #[must_use]
    pub fn on_message(&mut self, data: &[u8]) -> js_sys::Array {
        let out = js_sys::Array::new();
        let Some(frame) = decode_inbound_frame(data) else {
            return out;
        };
        for frame in self.handle(frame) {
            out.push(&js_sys::Uint8Array::from(frame.as_slice()));
        }
        out
    }
}

impl EdgeSession {
    fn restore_inner(
        checkpoint_json: &str,
        mode: &str,
        snapshot_json: &str,
    ) -> Result<Self, String> {
        if !matches!(mode, "demo" | "portfolio" | "native-fallback") {
            return Err("invalid session mode".to_owned());
        }
        let checkpoint: Checkpoint =
            serde_json::from_str(checkpoint_json).map_err(|_| "invalid checkpoint".to_owned())?;
        if checkpoint.version != CHECKPOINT_VERSION {
            return Err("unsupported checkpoint version".to_owned());
        }
        if checkpoint.cols == 0
            || checkpoint.rows == 0
            || checkpoint.cols > MAX_VIEWPORT
            || checkpoint.rows > MAX_VIEWPORT
        {
            return Err("checkpoint viewport is out of range".to_owned());
        }
        let mut shell = Shell::new(mode, snapshot_json);
        shell.restore(checkpoint.shell)?;
        Ok(Self {
            terminal_id: ResourceId::new(1),
            cols: checkpoint.cols,
            rows: checkpoint.rows,
            seq: checkpoint.seq,
            shell,
        })
    }

    fn handle(&mut self, frame: FrameKind) -> Vec<Vec<u8>> {
        match frame {
            FrameKind::Hello { .. } => vec![encode(&hello_ok())],
            FrameKind::Attach {
                attach_id,
                viewport,
                ..
            } => {
                self.cols = viewport.cols.clamp(1, MAX_VIEWPORT);
                self.rows = viewport.rows.clamp(1, MAX_VIEWPORT);
                self.seq = 0;
                let greeting = Bytes::from(self.shell.greeting());
                vec![
                    encode(&attached(
                        attach_id,
                        self.terminal_id.clone(),
                        self.cols,
                        self.rows,
                    )),
                    encode(&bootstrap_begin(
                        self.terminal_id.clone(),
                        self.cols,
                        self.rows,
                    )),
                    encode(&FrameKind::BootstrapChunk {
                        terminal_id: self.terminal_id.clone(),
                        stream_id: stream_id(),
                        bootstrap_id: bootstrap_id(),
                        chunk_seq: 0,
                        payload: greeting,
                    }),
                    encode(&FrameKind::BootstrapReady {
                        terminal_id: self.terminal_id.clone(),
                        stream_id: stream_id(),
                        bootstrap_id: bootstrap_id(),
                        history_cursor: None,
                    }),
                    encode(&FrameKind::AttachReady { attach_id }),
                ]
            }
            FrameKind::InputKey { event, .. } => {
                let bytes = self.shell.input(&event);
                if bytes.is_empty() {
                    Vec::new()
                } else {
                    self.seq += 1;
                    vec![encode(&FrameKind::ResourceOutput {
                        terminal_id: self.terminal_id.clone(),
                        stream_id: stream_id(),
                        bootstrap_id: bootstrap_id(),
                        seq: self.seq,
                        bytes: bytes.into(),
                    })]
                }
            }
            _ => Vec::new(),
        }
    }
}

fn stream_id() -> StreamId {
    StreamId::new(1).expect("edge stream id is non-zero")
}

fn bootstrap_id() -> BootstrapId {
    BootstrapId::new(1).expect("edge bootstrap id is non-zero")
}

fn hello_ok() -> FrameKind {
    FrameKind::HelloOk {
        protocol_major: PROTOCOL_VERSION.major,
        protocol_minor: PROTOCOL_VERSION.minor,
        protocol_patch: PROTOCOL_VERSION.patch,
        server_caps: ServerCapabilities::new(),
        server_id: b"phux-edge".to_vec(),
        selected_profile: BootstrapProfile::SynthesizedVtRaw,
        bootstrap_limits: BootstrapLimits::default(),
    }
}

fn attached(attach_id: u32, terminal_id: ResourceId, cols: u16, rows: u16) -> FrameKind {
    FrameKind::Attached {
        attach_id,
        snapshot: SessionSnapshot::new(SessionId::new(1), WindowId::new(1), terminal_id.clone())
            .with_resources(vec![ResourceInfo::new(
                terminal_id,
                WindowId::new(1),
                cols,
                rows,
            )]),
        initial_client_id: ClientId::new(1),
    }
}

fn bootstrap_begin(terminal_id: ResourceId, cols: u16, rows: u16) -> FrameKind {
    FrameKind::BootstrapBegin {
        terminal_id,
        stream_id: stream_id(),
        bootstrap_id: bootstrap_id(),
        profile: BootstrapStreamProfile::SynthesizedVtRaw,
        cols,
        rows,
        base_seq: 0,
    }
}

#[cfg(test)]
fn smoke_hello() -> FrameKind {
    FrameKind::Hello {
        client_name: "phux-site-smoke".to_owned(),
        protocol_major: PROTOCOL_VERSION.major,
        protocol_minor: PROTOCOL_VERSION.minor,
        protocol_patch: PROTOCOL_VERSION.patch,
        client_caps: phux_protocol::caps::ClientCapabilities::new(),
    }
}

#[cfg(test)]
fn smoke_attach() -> FrameKind {
    FrameKind::Attach {
        attach_id: 1,
        target: AttachTarget::CreateIfMissing {
            name: "default".to_owned(),
            command: None,
            cwd: None,
        },
        viewport: ViewportInfo::new(80, 24),
        request_scrollback: true,
        scrollback_limit_lines: 5_000,
    }
}

/// Decode one client WebSocket message. `FRAME_COMPRESSED` is server-to-client
/// only (proto.md §6.4). Refuse it before `FrameKind::decode` so a 64KiB
/// envelope cannot inflate to the 8MiB bootstrap ceiling.
fn decode_inbound_frame(data: &[u8]) -> Option<FrameKind> {
    if data.get(4) == Some(&TYPE_FRAME_COMPRESSED) {
        return None;
    }
    FrameKind::decode(data).ok().map(|(frame, _rest)| frame)
}

/// Encode a frame to one length-prefixed WebSocket message.
fn encode(frame: &FrameKind) -> Vec<u8> {
    let mut buf = BytesMut::new();
    frame.encode(&mut buf);
    buf.to_vec()
}

#[cfg(test)]
mod tests {
    use super::{
        EdgeSession, FrameKind, PROTOCOL_VERSION, decode_inbound_frame, encode, smoke_attach,
        smoke_hello,
    };
    use phux_protocol::caps::ClientCapabilities;
    use phux_protocol::wire::frame::TYPE_FRAME_COMPRESSED;

    fn decode_all(frames: &[Vec<u8>]) -> Vec<FrameKind> {
        frames
            .iter()
            .map(|frame| {
                let (decoded, rest) = FrameKind::decode(frame).expect("edge frame must decode");
                assert!(rest.is_empty());
                decoded
            })
            .collect()
    }

    #[test]
    fn hello_returns_protocol_09_hello_ok() {
        let mut session = EdgeSession::new(80, 24, "demo", "");
        let output = session.handle(smoke_hello());
        assert_eq!(output.len(), 1);
        assert!(matches!(
            decode_all(&output)[0],
            FrameKind::HelloOk {
                protocol_major: 0,
                protocol_minor: 9,
                protocol_patch: 0,
                ..
            }
        ));
    }

    #[test]
    fn attach_returns_ready_fenced_greeting() {
        let mut session = EdgeSession::new(100, 24, "native-fallback", "{}");
        let output = session.handle(smoke_attach());
        assert_eq!(output.len(), 5);
        let decoded = decode_all(&output);
        assert!(matches!(
            decoded[0],
            FrameKind::Attached { attach_id: 1, .. }
        ));
        assert!(matches!(decoded[1], FrameKind::BootstrapBegin { .. }));
        let FrameKind::BootstrapChunk { payload, .. } = &decoded[2] else {
            panic!("expected bootstrap chunk");
        };
        let greeting = String::from_utf8_lossy(payload);
        assert!(greeting.contains("instant edge tour"), "{greeting}");
        assert!(matches!(decoded[3], FrameKind::BootstrapReady { .. }));
        assert!(matches!(
            decoded[4],
            FrameKind::AttachReady { attach_id: 1 }
        ));
    }

    #[test]
    fn smoke_hello_advertises_workspace_protocol() {
        let FrameKind::Hello {
            protocol_major,
            protocol_minor,
            protocol_patch,
            client_caps,
            ..
        } = smoke_hello()
        else {
            panic!("expected HELLO");
        };
        assert_eq!(
            (protocol_major, protocol_minor, protocol_patch),
            (
                PROTOCOL_VERSION.major,
                PROTOCOL_VERSION.minor,
                PROTOCOL_VERSION.patch
            )
        );
        assert_eq!(client_caps, ClientCapabilities::new());
        assert!(!encode(&smoke_hello()).is_empty());
        assert!(!encode(&smoke_attach()).is_empty());
    }

    #[test]
    fn inbound_decode_refuses_client_frame_compressed_before_inflate() {
        let hello = encode(&smoke_hello());
        assert_eq!(hello[4], 1);
        assert!(decode_inbound_frame(&hello).is_some());
        let mut compressed = hello.clone();
        compressed[4] = TYPE_FRAME_COMPRESSED;
        assert!(decode_inbound_frame(&compressed).is_none());
    }

    #[test]
    fn smoke_hello_and_attach_bytes_match_the_site_smoke_client() {
        assert_eq!(
            encode(&smoke_hello()),
            vec![
                0, 0, 0, 65, 1, 1, 4, 15, 112, 104, 117, 120, 45, 115, 105, 116, 101, 45, 115, 109,
                111, 107, 101, 2, 4, 2, 0, 0, 3, 4, 2, 0, 9, 4, 4, 2, 0, 0, 5, 4, 28, 0, 1, 7, 3,
                1, 0, 0, 6, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 4, 0, 0, 0, 16, 0, 0,
            ]
        );
        assert_eq!(
            encode(&smoke_attach()),
            vec![
                0, 0, 0, 45, 2, 1, 4, 14, 3, 0, 0, 0, 7, 100, 101, 102, 97, 117, 108, 116, 0, 0, 2,
                4, 6, 0, 80, 0, 24, 0, 0, 3, 4, 1, 1, 4, 4, 4, 0, 0, 19, 136, 5, 4, 4, 0, 0, 0, 1,
            ]
        );
    }

    #[test]
    fn demo_checkpoint_round_trips_logical_state() {
        let mut session = EdgeSession::new(100, 24, "demo", "");
        session.cols = 132;
        session.rows = 43;
        session.seq = 19;
        session
            .shell
            .restore(super::ShellCheckpoint::Demo {
                line: "echo partial".to_owned(),
            })
            .unwrap();

        let restored = EdgeSession::restore_inner(&session.checkpoint(), "demo", "").unwrap();
        assert_eq!(restored.cols, 132);
        assert_eq!(restored.rows, 43);
        assert_eq!(restored.seq, 19);
        assert_eq!(restored.shell.checkpoint(), session.shell.checkpoint());
    }

    #[test]
    fn portfolio_checkpoint_round_trips_selection_and_detail() {
        let snapshot = r#"{"repos":[{"name":"one","description":null,"url":"https://example.com","homepage":null,"language":null,"stars":0,"forks":0,"open_issues":0,"pushed_at":"","latest_run":null},{"name":"two","description":null,"url":"https://example.com","homepage":null,"language":null,"stars":0,"forks":0,"open_issues":0,"pushed_at":"","latest_run":null}]}"#;
        let mut session = EdgeSession::new(80, 24, "portfolio", snapshot);
        session
            .shell
            .restore(super::ShellCheckpoint::Portfolio {
                selected: 1,
                detail: true,
            })
            .unwrap();

        let restored =
            EdgeSession::restore_inner(&session.checkpoint(), "portfolio", snapshot).unwrap();
        assert_eq!(restored.shell.checkpoint(), session.shell.checkpoint());
    }

    #[test]
    fn checkpoint_rejects_invalid_schema_ranges_and_kind() {
        for invalid in [
            r#"{"version":2,"kind":"edge-session","cols":80,"rows":24,"seq":0,"shell":{"kind":"demo","line":""}}"#,
            r#"{"version":1,"kind":"other","cols":80,"rows":24,"seq":0,"shell":{"kind":"demo","line":""}}"#,
            r#"{"version":1,"kind":"edge-session","cols":0,"rows":24,"seq":0,"shell":{"kind":"demo","line":""}}"#,
            r#"{"version":1,"kind":"edge-session","cols":80,"rows":1001,"seq":0,"shell":{"kind":"demo","line":""}}"#,
            r#"{"version":1,"kind":"edge-session","cols":80,"rows":24,"seq":0,"extra":true,"shell":{"kind":"demo","line":""}}"#,
        ] {
            assert!(EdgeSession::restore_inner(invalid, "demo", "").is_err());
        }
    }

    #[test]
    fn checkpoint_rejects_mode_mismatch_and_bad_shell_state() {
        let portfolio_snapshot = r#"{"repos":[]}"#;
        let portfolio = r#"{"version":1,"kind":"edge-session","cols":80,"rows":24,"seq":0,"shell":{"kind":"portfolio","selected":1,"detail":false}}"#;
        assert!(EdgeSession::restore_inner(portfolio, "portfolio", portfolio_snapshot).is_err());
        assert!(EdgeSession::restore_inner(portfolio, "demo", "").is_err());

        let oversized = format!(
            r#"{{"version":1,"kind":"edge-session","cols":80,"rows":24,"seq":0,"shell":{{"kind":"demo","line":"{}"}}}}"#,
            "x".repeat(4097)
        );
        assert!(EdgeSession::restore_inner(&oversized, "demo", "").is_err());
        assert!(EdgeSession::restore_inner(
            r#"{"version":1,"kind":"edge-session","cols":80,"rows":24,"seq":0,"shell":{"kind":"demo","line":""}}"#,
            "invalid",
            ""
        ).is_err());
    }
}
