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

use bytes::BytesMut;
use phux_protocol::ids::TerminalId;
use phux_protocol::wire::frame::FrameKind;
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
    terminal_id: TerminalId,
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
            terminal_id: TerminalId::new(1),
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
        let Ok((frame, _rest)) = FrameKind::decode(data) else {
            return out; // ignore undecodable input
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
            terminal_id: TerminalId::new(1),
            cols: checkpoint.cols,
            rows: checkpoint.rows,
            seq: checkpoint.seq,
            shell,
        })
    }

    fn handle(&mut self, frame: FrameKind) -> Vec<Vec<u8>> {
        match frame {
            // Attach → reply with a snapshot whose replay bytes are the shell's
            // greeting. Adopt the client's viewport so sizes match (no resize).
            FrameKind::Attach { viewport, .. } => {
                self.cols = viewport.cols.clamp(1, MAX_VIEWPORT);
                self.rows = viewport.rows.clamp(1, MAX_VIEWPORT);
                vec![encode(&FrameKind::TerminalSnapshot {
                    terminal_id: self.terminal_id.clone(),
                    cols: self.cols,
                    rows: self.rows,
                    vt_replay_bytes: self.shell.greeting(),
                    scrollback_bytes: None,
                })]
            }
            // A keystroke → run it through the shell → stream the VT output.
            FrameKind::InputKey { event, .. } => {
                let bytes = self.shell.input(&event);
                if bytes.is_empty() {
                    Vec::new()
                } else {
                    self.seq += 1;
                    vec![encode(&FrameKind::TerminalOutput {
                        terminal_id: self.terminal_id.clone(),
                        seq: self.seq,
                        bytes: bytes.into(),
                    })]
                }
            }
            // Hello, FrameAck, mouse/focus/paste, etc. — nothing to send.
            _ => Vec::new(),
        }
    }
}

/// Encode a frame to one length-prefixed WebSocket message.
fn encode(frame: &FrameKind) -> Vec<u8> {
    let mut buf = BytesMut::new();
    frame.encode(&mut buf);
    buf.to_vec()
}

#[cfg(test)]
mod tests {
    use super::{EdgeSession, FrameKind};

    // The exact CreateIfMissing ATTACH emitted by the hosted phux-web client.
    const WEB_ATTACH: &[u8] = &[
        0, 0, 0, 34, 2, 1, 4, 10, 3, 0, 0, 0, 3, 100, 101, 118, 0, 0, 2, 4, 6, 0, 80, 0, 24, 0, 0,
        3, 4, 1, 0, 4, 4, 4, 0, 0, 0, 0,
    ];

    #[test]
    fn hosted_web_attach_decodes_and_returns_a_snapshot() {
        let (frame, rest) = FrameKind::decode(WEB_ATTACH).expect("web ATTACH must decode");
        assert!(rest.is_empty());
        let mut session = EdgeSession::new(100, 24, "native-fallback", "{}");
        let output = session.handle(frame);
        assert_eq!(output.len(), 1);
        let (response, rest) = FrameKind::decode(&output[0]).expect("snapshot must decode");
        assert!(rest.is_empty());
        assert!(matches!(response, FrameKind::TerminalSnapshot { .. }));
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
