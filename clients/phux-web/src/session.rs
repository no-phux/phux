//! Wire-protocol session over the ghostty-vt engine.
//!
//! Decodes server frames, feeds the engine, and produces frames to send back —
//! all pure logic (no DOM, no WebSocket), so it's deterministically testable.
//! The DOM/WebSocket glue in [`crate::client`] drives it.

use std::rc::Rc;

use bytes::BytesMut;
use phux_protocol::PROTOCOL_VERSION;
use phux_protocol::caps::{
    BootstrapCapabilities, BootstrapLimits, BootstrapProfile, BootstrapProfileKind,
    BootstrapProfileSet, ClientCapabilities, EngineCodecSet, EngineFeatureSet, ImageProtocolSet,
};
use phux_protocol::ids::TerminalId;
use phux_protocol::input::key::KeyEvent;
use phux_protocol::wire::frame::{AttachTarget, FrameKind, ViewportInfo};
use phux_vt_web::{Grid, Terminal, Vt};


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
    let bootstrap = BootstrapCapabilities::new()
        .with_profiles(BootstrapProfileSet::with(&[
            BootstrapProfileKind::SynthesizedVtRaw,
        ]))
        .with_native_codecs(EngineCodecSet::new())
        .with_native_features(EngineFeatureSet::new());
    ClientCapabilities::new()
        .with_image_protocols(ImageProtocolSet::new())
        .with_bootstrap(bootstrap)
}

/// The result of handling one incoming frame.
#[derive(Default)]
pub struct Outcome {
    /// Encoded frames to write back to the transport (e.g. `FRAME_ACK`).
    pub send: Vec<Vec<u8>>,
    /// Whether the grid changed and should be repainted.
    pub render: bool,
    /// Fatal protocol violation. The transport must close without sending any
    /// stateful follow-up.
    pub fatal: Option<String>,
}

/// A single-terminal wire session backed by a ghostty-vt engine terminal.
pub struct Session {
    term: Terminal,
    cols: u16,
    rows: u16,
    terminal_id: Option<TerminalId>,
    negotiated: Option<(BootstrapProfile, BootstrapLimits)>,
    pending_attach_id: Option<u32>,
    next_attach_id: u32,
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
            negotiated: None,
            pending_attach_id: None,
            next_attach_id: 1,
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
            FrameKind::HelloOk {
                protocol_major,
                protocol_minor,
                protocol_patch,
                selected_profile,
                bootstrap_limits,
                ..
            } => {
                if self.negotiated.is_some() {
                    return fatal("duplicate HELLO_OK on an established web connection");
                }
                if let Err(message) = validate_hello_ok(
                    &client_caps(),
                    protocol_major,
                    protocol_minor,
                    protocol_patch,
                    selected_profile,
                    bootstrap_limits,
                ) {
                    return fatal(message);
                }
                self.negotiated = Some((selected_profile, bootstrap_limits));
                let attach_id = self.next_attach_id;
                self.next_attach_id = self.next_attach_id.wrapping_add(1).max(1);
                self.pending_attach_id = Some(attach_id);
                Outcome {
                    send: vec![encode(&FrameKind::Attach {
                        attach_id,
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
                    fatal: None,
                }
            }
            FrameKind::Attached { attach_id, .. }
                if self.pending_attach_id == Some(attach_id) =>
            {
                Outcome::default()
            }
            FrameKind::Attached { attach_id, .. } => fatal(format!(
                "ATTACHED attach_id mismatch: expected {:?}, received {attach_id}",
                self.pending_attach_id,
            )),
            FrameKind::AttachReady { attach_id }
                if self.pending_attach_id == Some(attach_id) =>
            {
                self.pending_attach_id = None;
                Outcome::default()
            }
            FrameKind::AttachReady { attach_id } => fatal(format!(
                "ATTACH_READY attach_id mismatch: expected {:?}, received {attach_id}",
                self.pending_attach_id,
            )),
            FrameKind::TerminalOutput {
                terminal_id,
                stream_id: _,
                bootstrap_id: _,
                seq: _,
                bytes,
            } => {
                if self.negotiated.is_none() {
                    return fatal("TERMINAL_OUTPUT received before HELLO_OK");
                }
                self.terminal_id.get_or_insert_with(|| terminal_id.clone());
                self.term.write(&bytes);
                Outcome {
                    send: Vec::new(),
                    render: true,
                    fatal: None,
                }
            }
            // PONG, ERROR, metadata, and bootstrap frames are not rendered by
            // this handshake-only slice.
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

    /// Negotiated payload bounds used by the browser receive path.
    #[must_use]
    pub fn bootstrap_limits(&self) -> Option<BootstrapLimits> {
        self.negotiated.map(|(_, limits)| limits)
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

fn fatal(message: impl Into<String>) -> Outcome {
    Outcome {
        send: Vec::new(),
        render: false,
        fatal: Some(message.into()),
    }
}

fn validate_hello_ok(
    offered: &ClientCapabilities,
    protocol_major: u16,
    protocol_minor: u16,
    protocol_patch: u16,
    selected_profile: BootstrapProfile,
    selected_limits: BootstrapLimits,
) -> Result<(), String> {
    if (protocol_major, protocol_minor, protocol_patch)
        != (
            PROTOCOL_VERSION.major,
            PROTOCOL_VERSION.minor,
            PROTOCOL_VERSION.patch,
        )
    {
        return Err(format!(
            "HELLO_OK selected unsupported protocol {protocol_major}.{protocol_minor}.{protocol_patch}",
        ));
    }
    let profile_is_offered = match selected_profile {
        BootstrapProfile::NativeState { codec, features } => {
            offered
                .bootstrap
                .profiles
                .contains(BootstrapProfileKind::NativeState)
                && offered.bootstrap.native_codecs.contains(codec)
                && features.supports_native()
                && offered.bootstrap.native_features.intersect(features) == features
        }
        BootstrapProfile::SynthesizedVtRaw => offered
            .bootstrap
            .profiles
            .contains(BootstrapProfileKind::SynthesizedVtRaw),
        BootstrapProfile::SynthesizedVtStateSync => offered
            .bootstrap
            .profiles
            .contains(BootstrapProfileKind::SynthesizedVtStateSync),
        _ => false,
    };
    if !profile_is_offered {
        return Err(format!(
            "HELLO_OK selected bootstrap profile outside the web client's offer: {selected_profile:?}",
        ));
    }
    if offered.bootstrap.limits.intersect(selected_limits) != selected_limits {
        return Err(format!(
            "HELLO_OK selected bootstrap limits outside the web client's offer: chunk={} history_page={}",
            selected_limits.max_chunk_bytes(),
            selected_limits.max_history_page_bytes(),
        ));
    }
    Ok(())
}
