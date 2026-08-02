//! Protocol-0.7 web session over the shared synchronous client kernel.
//!
//! Transport framing stays here; replica lifecycle, generation validation,
//! ordering, READY fences, and input eligibility stay in `phux-client-core`.

use std::rc::Rc;

use bytes::{Bytes, BytesMut};
use phux_client_core::engine::{
    BootstrapProgress, CanonicalGeometry, EngineAdapter, EngineDamage, EngineEffect,
    EngineEffectBuffer,
};
use phux_client_core::session::{
    EffectBuffer, InputEligibility, KernelAction, KernelEffect, KernelInput, KernelSend,
    SessionKernel,
};
use phux_protocol::PROTOCOL_VERSION;
use phux_protocol::caps::{
    BootstrapCapabilities, BootstrapLimits, BootstrapProfile, BootstrapProfileKind,
    BootstrapProfileSet, BootstrapStreamProfile, ClientCapabilities, ImageProtocolSet,
    ServerFeature,
};
use phux_protocol::ids::{BootstrapId, StreamId, TerminalId};
use phux_protocol::input::InputEvent;
use phux_protocol::input::key::KeyEvent;
use phux_protocol::wire::frame::{AttachTarget, FrameKind, ViewportInfo};
use phux_vt_web::{Grid, Terminal, Vt};

const ATTACH_ID: u32 = 1;
const HISTORY_LINES: u32 = 5_000;

/// The capability set phux-web advertises in `HELLO`.
///
/// The browser engine consumes only synthesized VT profiles. It never
/// advertises native libghostty checkpoint support and advertises no image
/// protocols until the canvas renderer can project them.
#[must_use]
pub fn client_caps() -> ClientCapabilities {
    let profiles = BootstrapProfileSet::with(&[
        BootstrapProfileKind::SynthesizedVtRaw,
        BootstrapProfileKind::SynthesizedVtStateSync,
    ]);
    ClientCapabilities::new()
        .with_image_protocols(ImageProtocolSet::new())
        .with_bootstrap(BootstrapCapabilities::new().with_profiles(profiles))
}

fn validate_hello_ok(
    protocol_major: u16,
    protocol_minor: u16,
    protocol_patch: u16,
    selected_profile: BootstrapProfile,
    bootstrap_limits: BootstrapLimits,
) -> Result<(), &'static str> {
    if (protocol_major, protocol_minor, protocol_patch)
        != (
            PROTOCOL_VERSION.major,
            PROTOCOL_VERSION.minor,
            PROTOCOL_VERSION.patch,
        )
    {
        return Err("HELLO_OK selected a different protocol version");
    }
    let profile_kind = match selected_profile {
        BootstrapProfile::SynthesizedVtRaw => BootstrapProfileKind::SynthesizedVtRaw,
        BootstrapProfile::SynthesizedVtStateSync => BootstrapProfileKind::SynthesizedVtStateSync,
        BootstrapProfile::NativeState { .. } => {
            return Err("HELLO_OK selected an unadvertised native profile");
        }
        _ => return Err("HELLO_OK selected an unknown profile"),
    };
    let offered = client_caps().bootstrap;
    if !offered.profiles.contains(profile_kind) {
        return Err("HELLO_OK selected an unadvertised bootstrap profile");
    }
    if bootstrap_limits.intersect(offered.limits) != bootstrap_limits {
        return Err("HELLO_OK selected bootstrap limits above the client offer");
    }
    Ok(())
}

/// The result of handling one incoming frame.
#[derive(Default)]
pub struct Outcome {
    /// Encoded frames for the browser transport to send.
    pub send: Vec<Vec<u8>>,
    /// Whether a published replica changed and should be repainted.
    pub render: bool,
    /// Fatal protocol/kernel failure; the transport must close.
    pub fatal: Option<String>,
}

struct WebEngine {
    vt: Rc<Vt>,
}

struct WebReplica {
    terminal: Terminal,
}

#[derive(Debug)]
enum WebEngineError {
    UnsupportedProfile(BootstrapStreamProfile),
}

impl std::fmt::Display for WebEngineError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnsupportedProfile(profile) => {
                write!(formatter, "unsupported web bootstrap profile: {profile:?}")
            }
        }
    }
}

impl std::error::Error for WebEngineError {}

impl EngineAdapter for WebEngine {
    type Replica = WebReplica;
    type Error = WebEngineError;

    fn start_replica(
        &mut self,
        profile: BootstrapStreamProfile,
        geometry: CanonicalGeometry,
    ) -> Result<Self::Replica, Self::Error> {
        if !matches!(
            profile,
            BootstrapStreamProfile::SynthesizedVtRaw
                | BootstrapStreamProfile::SynthesizedVtStateSync
        ) {
            return Err(WebEngineError::UnsupportedProfile(profile));
        }
        Ok(WebReplica {
            terminal: self.vt.terminal(geometry.cols, geometry.rows),
        })
    }

    fn apply_bootstrap_chunk(
        &mut self,
        replica: &mut Self::Replica,
        payload: &[u8],
        effects: &mut EngineEffectBuffer,
    ) -> Result<BootstrapProgress, Self::Error> {
        replica.terminal.write(payload);
        effects.push(EngineEffect::Damage(EngineDamage::Full));
        Ok(BootstrapProgress::Pending)
    }

    fn finish_bootstrap(
        &mut self,
        _replica: &mut Self::Replica,
        effects: &mut EngineEffectBuffer,
    ) -> Result<BootstrapProgress, Self::Error> {
        effects.push(EngineEffect::Damage(EngineDamage::Full));
        Ok(BootstrapProgress::Ready)
    }

    fn apply_history_page(
        &mut self,
        _replica: &mut Self::Replica,
        _payload: &[u8],
        _effects: &mut EngineEffectBuffer,
    ) -> Result<BootstrapProgress, Self::Error> {
        // Synthesized history pages are opaque and independently bounded.
        // The current wasm ABI has no history-import surface; consume them to
        // advance the protocol cursor without replaying them into the live grid.
        Ok(BootstrapProgress::Finished)
    }

    fn apply_output(
        &mut self,
        replica: &mut Self::Replica,
        payload: &[u8],
        effects: &mut EngineEffectBuffer,
    ) -> Result<(), Self::Error> {
        replica.terminal.write(payload);
        effects.push(EngineEffect::Damage(EngineDamage::Full));
        Ok(())
    }
}

/// A wire session whose terminal replicas are owned by [`SessionKernel`].
pub struct Session {
    vt: Rc<Vt>,
    blank: Terminal,
    kernel: Option<SessionKernel<WebEngine>>,
    effects: EffectBuffer,
    cols: u16,
    rows: u16,
    focused_terminal: Option<TerminalId>,
    terminal_order: Vec<TerminalId>,
    bootstrap_limits: Option<BootstrapLimits>,
    terminal_reply_supported: bool,
    failed: bool,
    render_visible: bool,
}

impl Session {
    /// Open a session with a blank fallback grid of `cols`×`rows`.
    #[must_use]
    pub fn new(vt: &Rc<Vt>, cols: u16, rows: u16) -> Self {
        Self {
            vt: Rc::clone(vt),
            blank: vt.terminal(cols, rows),
            kernel: None,
            effects: EffectBuffer::new(),
            cols,
            rows,
            focused_terminal: None,
            terminal_order: Vec::new(),
            bootstrap_limits: None,
            terminal_reply_supported: false,
            failed: false,
            render_visible: false,
        }
    }
    /// Negotiated decode limits after `HELLO_OK`.
    #[must_use]
    pub const fn bootstrap_limits(&self) -> Option<BootstrapLimits> {
        self.bootstrap_limits
    }

    /// Whether this session has entered its terminal protocol-failure state.
    #[must_use]
    pub const fn is_failed(&self) -> bool {
        self.failed
    }

    /// Permanently fail this session after a transport or framing violation.
    pub fn fail_protocol(&mut self, _message: &str) {
        self.failed = true;
    }

    /// Frame sent when the transport opens. Stateful frames wait for `HELLO_OK`.
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

    /// Reduce one decoded server frame through the shared kernel.
    pub fn on_frame(&mut self, frame: FrameKind) -> Outcome {
        if self.failed {
            return Outcome::default();
        }
        match frame {
            FrameKind::HelloOk {
                protocol_major,
                protocol_minor,
                protocol_patch,
                server_caps,
                selected_profile,
                bootstrap_limits,
                ..
            } => {
                if self.kernel.is_some() {
                    return self.protocol_failure("server sent duplicate HELLO_OK");
                }
                if let Err(message) = validate_hello_ok(
                    protocol_major,
                    protocol_minor,
                    protocol_patch,
                    selected_profile,
                    bootstrap_limits,
                ) {
                    return self.protocol_failure(message);
                }
                self.bootstrap_limits = Some(bootstrap_limits);
                self.terminal_reply_supported = server_caps
                    .features
                    .contains(ServerFeature::TerminalReply);
                self.kernel = Some(SessionKernel::new(
                    WebEngine {
                        vt: Rc::clone(&self.vt),
                    },
                    selected_profile,
                ));
                Outcome {
                    send: vec![encode(&FrameKind::Attach {
                        attach_id: ATTACH_ID,
                        target: AttachTarget::CreateIfMissing {
                            name: "default".to_owned(),
                            command: None,
                            cwd: None,
                        },
                        viewport: ViewportInfo::new(self.cols, self.rows),
                        request_scrollback: true,
                        scrollback_limit_lines: HISTORY_LINES,
                    })],
                    render: false,
                    fatal: None,
                }
            }
            FrameKind::Attached {
                attach_id,
                snapshot,
                ..
            } => {
                if attach_id != ATTACH_ID {
                    return self.protocol_failure("ATTACHED used the wrong attach identifier");
                }
                let terminal_ids: Vec<_> =
                    snapshot.panes.iter().map(|pane| pane.id.clone()).collect();
                let focused_terminal = snapshot.focused_pane;
                let (outcome, applied) = self.apply_kernel(KernelInput::AttachStarted {
                    attach_id,
                    terminals: &terminal_ids,
                });
                if applied {
                    self.focused_terminal = Some(focused_terminal);
                    self.terminal_order = terminal_ids;
                    self.render_visible = false;
                }
                outcome
            }
            FrameKind::BootstrapBegin {
                terminal_id,
                stream_id,
                bootstrap_id,
                profile,
                cols,
                rows,
                base_seq,
            } => {
                let (outcome, applied) = self.apply_kernel(KernelInput::BootstrapBegin {
                    terminal_id: &terminal_id,
                    stream_id,
                    bootstrap_id,
                    profile,
                    geometry: CanonicalGeometry { cols, rows },
                    base_seq,
                });
                if applied {
                    self.focused_terminal
                        .get_or_insert_with(|| terminal_id.clone());
                }
                outcome
            }
            FrameKind::BootstrapChunk {
                terminal_id,
                stream_id,
                bootstrap_id,
                chunk_seq,
                payload,
            } => {
                self.apply_kernel(KernelInput::BootstrapChunk {
                    terminal_id: &terminal_id,
                    stream_id,
                    bootstrap_id,
                    chunk_seq,
                    payload: &payload,
                })
                .0
            }
            FrameKind::BootstrapReady {
                terminal_id,
                stream_id,
                bootstrap_id,
                history_cursor,
            } => {
                self.apply_kernel(KernelInput::BootstrapReady {
                    terminal_id: &terminal_id,
                    stream_id,
                    bootstrap_id,
                    history_cursor: history_cursor.as_deref(),
                })
                .0
            }
            FrameKind::HistoryPage {
                terminal_id,
                stream_id,
                bootstrap_id,
                cursor,
                next_cursor,
                payload,
            } => {
                self.apply_kernel(KernelInput::HistoryPage {
                    terminal_id: &terminal_id,
                    stream_id,
                    bootstrap_id,
                    payload: &payload,
                    cursor: &cursor,
                    next_cursor: next_cursor.as_deref(),
                })
                .0
            }
            FrameKind::TerminalOutput {
                terminal_id,
                stream_id,
                bootstrap_id,
                seq,
                bytes,
            } => {
                self.apply_kernel(KernelInput::TerminalOutput {
                    terminal_id: &terminal_id,
                    stream_id,
                    bootstrap_id,
                    seq,
                    payload: &bytes,
                })
                .0
            }
            FrameKind::BootstrapTombstone {
                terminal_id,
                stream_id,
                bootstrap_id,
                reason,
                last_valid_seq,
            } => {
                self.apply_kernel(KernelInput::Tombstone {
                    terminal_id: &terminal_id,
                    stream_id,
                    bootstrap_id,
                    reason,
                    last_valid_seq,
                })
                .0
            }
            FrameKind::TerminalClosed { terminal_id, .. } => {
                let was_focused = self.focused_terminal.as_ref() == Some(&terminal_id);
                let (outcome, applied) = self.apply_kernel(KernelInput::TerminalClosed {
                    terminal_id: &terminal_id,
                });
                if applied && was_focused {
                    self.focused_terminal = self.first_published_terminal();
                }
                outcome
            }
            FrameKind::AttachReady { attach_id } => {
                self.apply_kernel(KernelInput::AttachReady { attach_id }).0
            }
            _ => Outcome::default(),
        }
    }

    /// Current styled grid from a published replica, or the initial blank grid.
    #[must_use]
    pub fn grid(&self) -> Grid {
        self.published_terminal()
            .map_or_else(|| self.blank.grid(), |terminal| terminal.grid())
    }

    /// Whether canvas paint is allowed past the aggregate first-damage barrier.
    #[must_use]
    pub const fn render_visible(&self) -> bool {
        self.render_visible
    }

    /// Current published grid dimensions in cells.
    #[must_use]
    pub fn dims(&self) -> (u16, u16) {
        self.published_geometry()
            .map_or((self.cols, self.rows), |geometry| {
                (geometry.cols, geometry.rows)
            })
    }

    /// Encode an eligible structured key event for the focused published pane.
    #[must_use]
    pub fn key_frame(&mut self, event: KeyEvent) -> Option<Vec<u8>> {
        if self.failed {
            return None;
        }
        let terminal_id = self.first_published_terminal()?;
        let kernel = self.kernel.as_ref()?;
        if !matches!(
            kernel.input_eligibility(&terminal_id),
            InputEligibility::Eligible { .. }
        ) {
            return None;
        }
        let (outcome, applied) = self.apply_kernel(KernelInput::Action(KernelAction::Input {
            terminal_id: &terminal_id,
            event: &InputEvent::Key(event),
        }));
        if applied {
            outcome.send.into_iter().next()
        } else {
            None
        }
    }


    fn apply_kernel(&mut self, input: KernelInput<'_>) -> (Outcome, bool) {
        let Some(kernel) = self.kernel.as_mut() else {
            self.effects.clear();
            return (
                self.protocol_failure("stateful frame arrived before HELLO_OK"),
                false,
            );
        };
        let result = kernel.update(input, &mut self.effects);
        let focused = self.focused_terminal.as_ref();
        let mut outcome = Outcome::default();
        for effect in self.effects.as_slice() {
            match effect {
                KernelEffect::Send(KernelSend::Input { terminal_id, event }) => {
                    outcome
                        .send
                        .push(encode(&(*event).clone().into_frame(terminal_id.clone())));
                }
                KernelEffect::Send(KernelSend::FrameAck {
                    terminal_id,
                    stream_id,
                    bootstrap_id,
                    seq,
                }) => outcome.send.push(encode(&FrameKind::FrameAck {
                    terminal_id: terminal_id.clone(),
                    stream_id: *stream_id,
                    bootstrap_id: *bootstrap_id,
                    seq: *seq,
                })),
                KernelEffect::Send(KernelSend::HistoryRequest { key, cursor, max_bytes }) => {
                    outcome.send.push(encode(&FrameKind::HistoryRequest {
                        terminal_id: key.terminal_id.clone(),
                        stream_id: key.stream_id,
                        bootstrap_id: key.bootstrap_id,
                        cursor: Bytes::copy_from_slice(cursor),
                        max_bytes: *max_bytes,
                    }));
                }
                KernelEffect::Send(KernelSend::PtyWrite { terminal_id, bytes }) => {
                    if self.terminal_reply_supported {
                        outcome.send.push(encode(&FrameKind::InputTerminalReply {
                            terminal_id: terminal_id.clone(),
                            bytes: Bytes::copy_from_slice(bytes),
                        }));
                    } else {
                        outcome.fatal = Some(
                            "terminal query reply not sent: server lacks terminal-reply support"
                                .to_owned(),
                        );
                    }
                }
                KernelEffect::Damage(damage) => {
                    if focused == Some(&damage.terminal_id) || focused.is_none() {
                        outcome.render = true;
                    }
                }
                KernelEffect::Status(_) | KernelEffect::Job(_) => {}
            }
        }
        if outcome.render {
            self.render_visible = true;
        }
        if outcome.fatal.is_some() {
            self.failed = true;
            return (outcome, false);
        }
        match result {
            Ok(()) => (outcome, true),
            Err(error) => {
                self.failed = true;
                outcome.fatal = Some(error.to_string());
                (outcome, false)
            }
        }
    }

    fn protocol_failure(&mut self, message: &str) -> Outcome {
        self.fail_protocol(message);
        Outcome {
            fatal: Some(message.to_owned()),
            ..Outcome::default()
        }
    }

    fn first_published_terminal(&self) -> Option<TerminalId> {
        let kernel = self.kernel.as_ref()?;
        if let Some(focused) = self.focused_terminal.as_ref() {
            if kernel.published(focused).is_some() {
                return Some(focused.clone());
            }
        }
        self.terminal_order
            .iter()
            .find(|terminal_id| kernel.published(terminal_id).is_some())
            .cloned()
    }

    fn published_terminal(&self) -> Option<&Terminal> {
        let terminal_id = self.first_published_terminal()?;
        let kernel = self.kernel.as_ref()?;
        Some(&kernel.published_engine(&terminal_id)?.terminal)
    }

    fn published_geometry(&self) -> Option<CanonicalGeometry> {
        let terminal_id = self.first_published_terminal()?;
        self.kernel
            .as_ref()?
            .published(&terminal_id)
            .map(|replica| replica.geometry())
    }
}

fn encode(frame: &FrameKind) -> Vec<u8> {
    let mut buf = BytesMut::new();
    frame.encode(&mut buf);
    buf.to_vec()
}
