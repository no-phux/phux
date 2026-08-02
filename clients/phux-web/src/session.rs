//! Wire-protocol session over the ghostty-vt engine.
//!
//! Decodes server frames, feeds the engine, and produces frames to send back —
//! all pure logic (no DOM, no WebSocket), so it's deterministically testable.
//! The DOM/WebSocket glue in [`crate::client`] drives it.

use std::rc::Rc;

use bytes::BytesMut;
use phux_protocol::PROTOCOL_VERSION;
use phux_protocol::caps::{ClientCapabilities, ImageProtocolSet};
use phux_protocol::ids::TerminalId;
use phux_protocol::input::key::KeyEvent;
use phux_protocol::wire::frame::{AttachTarget, FrameKind, ViewportInfo};
use phux_vt_web::{Grid, Terminal, Vt};

/// Advisory cell pixel size handed to the engine on resize.
const CELL_W: u32 = 8;
const CELL_H: u32 = 16;

/// The capability set phux-web advertises in `HELLO`.
///
/// The canvas renderer paints text, color, and the cursor only
/// (`docs/consumers/web.md` "Scope and limits"): image escapes the engine
/// parses are never projected to the canvas. Advertising an image protocol
/// we cannot render would make the server forward image payloads
/// (kitty graphics APC, sixel DCS, iTerm2 OSC 1337 — SPEC 6.2 /
/// `phux-server::downsample`) that die on arrival, wasting wire bytes on
/// exactly the largest escape class. Advertise NO image protocols until an
/// image-aware renderer pass exists (ADR-0034 sketches it); the server
/// then strips image escapes before forwarding. Everything else keeps the
/// defaults: the engine we carry handles truecolor, kitty keyboard
/// replies, and OSC 8 hyperlink framing without harm.
#[must_use]
pub fn client_caps() -> ClientCapabilities {
    ClientCapabilities::new().with_image_protocols(ImageProtocolSet::new())
}

/// The result of handling one incoming frame.
#[derive(Default)]
pub struct Outcome {
    /// Encoded frames to write back to the transport (e.g. `FRAME_ACK`).
    pub send: Vec<Vec<u8>>,
    /// Whether the grid changed and should be repainted.
    pub render: bool,
}

/// A single-terminal wire session backed by a ghostty-vt engine terminal.
pub struct Session {
    term: Terminal,
    cols: u16,
    rows: u16,
    terminal_id: Option<TerminalId>,
}

impl Session {
    /// Open a session with a fresh engine terminal of `cols`×`rows`.
    #[must_use]
    pub fn new(vt: &Rc<Vt>, cols: u16, rows: u16) -> Self {
        Self {
            term: vt.terminal(cols, rows),
            cols,
            rows,
            terminal_id: None,
        }
    }

    /// Frame to send immediately once the transport opens: `HELLO`.
    ///
    /// `ATTACH` is returned from [`Self::on_frame`] only after `HELLO_OK`,
    /// so a refusal can never race a stateful frame onto the connection.
    #[must_use]
    pub fn handshake(&self) -> Vec<Vec<u8>> {
        vec![encode(&FrameKind::Hello {
            client_name: "phux-web".to_owned(),
            protocol_major: PROTOCOL_VERSION.major,
            protocol_minor: PROTOCOL_VERSION.minor,
            protocol_patch: PROTOCOL_VERSION.patch,
            client_caps: client_caps(),
        })]
    }

    /// Handle one decoded server frame: feed the engine and return any frames
    /// to send back plus whether a repaint is needed.
    pub fn on_frame(&mut self, frame: FrameKind) -> Outcome {
        match frame {
            FrameKind::HelloOk { .. } => Outcome {
                send: vec![encode(&FrameKind::Attach {
                    // The web client owns one session named "default": attach
                    // to it, or create it if the server has none yet.
                    target: AttachTarget::CreateIfMissing {
                        name: "default".to_owned(),
                        command: None,
                        cwd: None,
                    },
                    viewport: ViewportInfo::new(self.cols, self.rows),
                    request_scrollback: false,
                    scrollback_limit_lines: 0,
                })],
                render: false,
            },
            FrameKind::TerminalSnapshot {
                terminal_id,
                cols,
                rows,
                vt_replay_bytes,
                scrollback_bytes,
            } => {
                self.terminal_id = Some(terminal_id);
                if cols != self.cols || rows != self.rows {
                    self.term.resize(cols, rows, CELL_W, CELL_H);
                    self.cols = cols;
                    self.rows = rows;
                }
                if let Some(sb) = scrollback_bytes {
                    self.term.write(&sb);
                }
                self.term.write(&vt_replay_bytes);
                Outcome {
                    send: Vec::new(),
                    render: true,
                }
            }
            FrameKind::TerminalOutput {
                terminal_id,
                seq,
                bytes,
            } => {
                self.terminal_id.get_or_insert_with(|| terminal_id.clone());
                self.term.write(&bytes);
                Outcome {
                    send: vec![encode(&FrameKind::FrameAck { terminal_id, seq })],
                    render: true,
                }
            }
            // PONG, ERROR, metadata, etc. — nothing to render.
            _ => Outcome::default(),
        }
    }

    /// The current styled grid (for the renderer).
    #[must_use]
    pub fn grid(&self) -> Grid {
        self.term.grid()
    }

    /// Grid dimensions in cells.
    #[must_use]
    pub const fn dims(&self) -> (u16, u16) {
        (self.cols, self.rows)
    }

    /// Encode an `INPUT_KEY` for the attached terminal, or `None` if not yet
    /// attached.
    #[must_use]
    pub fn key_frame(&self, event: KeyEvent) -> Option<Vec<u8>> {
        self.terminal_id
            .clone()
            .map(|terminal_id| encode(&FrameKind::InputKey { terminal_id, event }))
    }
}

/// Encode a frame to a length-prefixed byte vector (one WebSocket message).
fn encode(frame: &FrameKind) -> Vec<u8> {
    let mut buf = BytesMut::new();
    frame.encode(&mut buf);
    buf.to_vec()
}
