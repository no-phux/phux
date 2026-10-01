//! A phux server inside one Cloudflare Durable Object, backed by curated shells.
//! Each inbound WebSocket message is one real `phux-protocol` frame; each returned
//! frame is sent as one message. Terminal resources share this one session.

mod shell;
mod terminal;
#[cfg(test)]
mod tests;

use bytes::BytesMut;
use phux_protocol::PROTOCOL_VERSION;
use phux_protocol::caps::{
    BootstrapLimits, BootstrapProfile, ServerCapabilities, ServerFeature, ServerFeatureSet,
};
use phux_protocol::ids::{ClientId, ResourceId, SessionId, WindowId};
use phux_protocol::input::{
    key::KeyEvent,
    paste::{PasteEvent, PasteTrust},
};
use phux_protocol::wire::frame::{
    CloseReason, Command, CommandResult, ErrorCode, FrameKind, SpawnError, SpawnResult,
    TYPE_FRAME_COMPRESSED,
};
use phux_protocol::wire::info::{ResourceInfo, SessionSnapshot};
use serde::{Deserialize, Serialize};
use wasm_bindgen::prelude::*;

use shell::{Shell, ShellCheckpoint};
use terminal::{MAX_VIEWPORT, Terminal, TerminalCheckpoint, valid_size};

const MAX_TERMINALS: usize = 4;
const MAX_INPUT_BYTES: usize = 4096;
const MAX_INBOUND_BYTES: usize = 65536;
// Below the Durable Object's 128KiB per-value storage limit, including JSON escaping.
const MAX_CHECKPOINT_BYTES: usize = 120 * 1024;

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Checkpoint {
    version: u8,
    kind: CheckpointKind,
    next_id: u32,
    terminals: Vec<TerminalCheckpoint>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LegacyCheckpoint {
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

/// One live hosted wire session with up to four independently addressed shells.
#[wasm_bindgen]
pub struct EdgeSession {
    terminals: Vec<Terminal>,
    next_id: u32,
    mode: String,
    snapshot_json: String,
}

#[wasm_bindgen]
impl EdgeSession {
    #[wasm_bindgen(constructor)]
    #[must_use]
    pub fn new(cols: u16, rows: u16, mode: &str, snapshot_json: &str) -> Self {
        Self {
            terminals: vec![Terminal::new(
                1,
                cols.clamp(1, MAX_VIEWPORT),
                rows.clamp(1, MAX_VIEWPORT),
                Shell::new(mode, snapshot_json),
            )],
            next_id: 2,
            mode: mode.to_owned(),
            snapshot_json: snapshot_json.to_owned(),
        }
    }

    /// Persist all resources, subscriptions, replica generations and the ID allocator.
    /// Mode and the portfolio snapshot remain in the caller's boot record.
    #[must_use]
    pub fn checkpoint(&self) -> String {
        serde_json::to_string(&Checkpoint {
            version: 2,
            kind: CheckpointKind::EdgeSession,
            next_id: self.next_id,
            terminals: self.terminals.iter().map(Terminal::checkpoint).collect(),
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

    /// One encoded frame in, one JS Uint8Array per response frame out.
    #[must_use]
    pub fn on_message(&mut self, data: &[u8]) -> js_sys::Array {
        let frames = match decode_inbound_frame(data) {
            Some(frame) => self.handle(frame),
            None => vec![encode(&error(
                ErrorCode::MalformedMessage,
                "expected one uncompressed frame of at most 64KiB",
            ))],
        };
        let out = js_sys::Array::new();
        for frame in frames {
            out.push(&js_sys::Uint8Array::from(frame.as_slice()));
        }
        out
    }
}

impl EdgeSession {
    fn restore_inner(json: &str, mode: &str, snapshot: &str) -> Result<Self, String> {
        if !matches!(mode, "demo" | "portfolio" | "native-fallback") {
            return Err("invalid session mode".to_owned());
        }
        if json.len() > MAX_CHECKPOINT_BYTES {
            return Err("checkpoint exceeds storage limit".to_owned());
        }
        // Try the strict v2 schema first; legacy decoding remains equally strict.
        let checkpoint = match serde_json::from_str::<Checkpoint>(json) {
            Ok(checkpoint) => checkpoint,
            Err(_) => return Self::restore_legacy(json, mode, snapshot),
        };
        if checkpoint.version != 2 {
            return Err("unsupported checkpoint version".to_owned());
        }
        if checkpoint.terminals.is_empty() || checkpoint.terminals.len() > MAX_TERMINALS {
            return Err("checkpoint must contain one to four terminals".to_owned());
        }
        let mut terminals: Vec<Terminal> = Vec::with_capacity(checkpoint.terminals.len());
        for state in checkpoint.terminals {
            if state.id >= checkpoint.next_id || terminals.iter().any(|t| t.id == state.id) {
                return Err("checkpoint resource IDs or allocator are invalid".to_owned());
            }
            terminals.push(Terminal::restore(state, mode, snapshot)?);
        }
        Ok(Self {
            terminals,
            next_id: checkpoint.next_id,
            mode: mode.to_owned(),
            snapshot_json: snapshot.to_owned(),
        })
    }

    fn restore_legacy(json: &str, mode: &str, snapshot: &str) -> Result<Self, String> {
        let old: LegacyCheckpoint =
            serde_json::from_str(json).map_err(|_| "invalid checkpoint schema".to_owned())?;
        if old.version != 1 {
            return Err("unsupported checkpoint version".to_owned());
        }
        let CheckpointKind::EdgeSession = old.kind;
        if !valid_size(old.cols, old.rows) || old.seq == u64::MAX {
            return Err("legacy checkpoint viewport or sequence is invalid".to_owned());
        }
        let mut shell = Shell::new(mode, snapshot);
        shell.restore(old.shell)?;
        // v1 retained no output. Preserve its current input/portfolio selection and
        // sequence, and resume generation 1 for an already-hibernating WebSocket.
        let mut terminal = Terminal::new(1, old.cols, old.rows, shell);
        terminal.seq = old.seq;
        terminal.generation = 1;
        terminal.subscribed = true;
        Ok(Self {
            terminals: vec![terminal],
            next_id: 2,
            mode: mode.to_owned(),
            snapshot_json: snapshot.to_owned(),
        })
    }

    fn handle(&mut self, frame: FrameKind) -> Vec<Vec<u8>> {
        let frames = match frame {
            FrameKind::Hello { .. } => vec![hello_ok()],
            FrameKind::Attach {
                attach_id,
                viewport,
                ..
            } => self.attach(attach_id, viewport.cols, viewport.rows),
            spawn @ FrameKind::SpawnResource { .. } => self.spawn(spawn),
            FrameKind::Command {
                request_id,
                command,
            } => self.command(request_id, command),
            FrameKind::ResizeTerminal {
                terminal_id,
                cols,
                rows,
            } => self.resize(&terminal_id, cols, rows),
            FrameKind::InputKey { terminal_id, event } => self.input_key(&terminal_id, &event),
            FrameKind::InputPaste { terminal_id, event } => self.input_paste(&terminal_id, &event),
            // The curated shell has no mouse, focus, terminal-reply or ACK work.
            FrameKind::InputMouse { .. }
            | FrameKind::InputFocus { .. }
            | FrameKind::InputTerminalReply { .. }
            | FrameKind::FrameAck { .. }
            | FrameKind::ViewportResize { .. } => Vec::new(),
            _ => vec![error(
                ErrorCode::UnknownMessageType,
                "operation is not supported by the hosted edge shell",
            )],
        };
        frames.iter().map(encode).collect()
    }

    fn attach(&mut self, attach_id: u32, cols: u16, rows: u16) -> Vec<FrameKind> {
        if !valid_size(cols, rows) {
            return vec![error(
                ErrorCode::MalformedMessage,
                "viewport axes must be between 1 and 1000",
            )];
        }
        if self.terminals.iter().any(|t| t.generation >= u64::MAX - 1) {
            return vec![error(
                ErrorCode::ResourceExhausted,
                "terminal generation exhausted",
            )];
        }
        // A whole-session viewport is not a per-pane resize after reconnect.
        if self.terminals.len() == 1 {
            self.terminals[0].cols = cols;
            self.terminals[0].rows = rows;
        }
        let focused = self.terminals[0].resource_id();
        let resources = self
            .terminals
            .iter()
            .map(|t| ResourceInfo::new(t.resource_id(), WindowId::new(1), t.cols, t.rows))
            .collect();
        let mut frames = vec![FrameKind::Attached {
            attach_id,
            snapshot: SessionSnapshot::new(SessionId::new(1), WindowId::new(1), focused)
                .with_resources(resources),
            initial_client_id: ClientId::new(1),
        }];
        for terminal in &mut self.terminals {
            frames.extend(
                terminal
                    .bootstrap()
                    .expect("generation checked before attach"),
            );
        }
        frames.push(FrameKind::AttachReady { attach_id });
        frames
    }

    fn spawn(&mut self, frame: FrameKind) -> Vec<FrameKind> {
        let FrameKind::SpawnResource {
            request_id,
            group,
            command,
            cwd,
            env,
            term,
            satellite,
            owner_terminal,
            agent_session,
            initial_size,
            resource,
        } = frame
        else {
            unreachable!()
        };
        let result = if satellite.is_some() {
            Err(SpawnError::UnsupportedSatelliteRoute)
        } else if group != phux_protocol::ids::GroupId::new(1) {
            Err(SpawnError::GroupNotFound)
        } else if resource.as_ref().is_some_and(|r| !r.kind.is_terminal()) {
            Err(SpawnError::UnsupportedKind)
        } else if owner_terminal
            .as_ref()
            .is_some_and(|id| self.terminal_index(id).is_err())
        {
            Err(SpawnError::SpawnFailed(
                "owner terminal does not exist".to_owned(),
            ))
        } else if command.is_some()
            || cwd.is_some()
            || env.is_some()
            || term.is_some()
            || agent_session.is_some()
            || resource.is_some_and(|r| *r != Default::default())
        {
            Err(SpawnError::SpawnFailed("edge supports only the default curated shell; commands, environment, agent bindings and retention options require native mode".to_owned()))
        } else {
            self.create_terminal(initial_size)
        };
        vec![FrameKind::ResourceSpawned {
            request_id,
            result: match result {
                Ok(id) => SpawnResult::Ok(id),
                Err(reason) => SpawnResult::Err(reason),
            },
        }]
    }

    fn create_terminal(
        &mut self,
        initial_size: Option<(u16, u16)>,
    ) -> Result<ResourceId, SpawnError> {
        if self.terminals.len() >= MAX_TERMINALS {
            return Err(SpawnError::SpawnFailed(
                "hosted sessions support at most four terminals; close a pane first".to_owned(),
            ));
        }
        let next_id = self
            .next_id
            .checked_add(1)
            .ok_or_else(|| SpawnError::SpawnFailed("resource ID space exhausted".to_owned()))?;
        let (cols, rows) = initial_size
            .filter(|(c, r)| *c != 0 && *r != 0)
            .unwrap_or((self.terminals[0].cols, self.terminals[0].rows));
        if !valid_size(cols, rows) {
            return Err(SpawnError::SpawnFailed(
                "terminal axes must be between 1 and 1000".to_owned(),
            ));
        }
        let id = self.next_id;
        self.terminals.push(Terminal::new(
            id,
            cols,
            rows,
            Shell::new(&self.mode, &self.snapshot_json),
        ));
        self.next_id = next_id;
        Ok(ResourceId::new(id))
    }

    fn terminal_index(&self, id: &ResourceId) -> Result<usize, (ErrorCode, String)> {
        if !id.is_local() {
            return Err((
                ErrorCode::UnsupportedSatelliteRoute,
                "edge sessions cannot route satellite resources".to_owned(),
            ));
        }
        self.terminals
            .iter()
            .position(|t| Some(t.id) == id.local_id())
            .ok_or_else(|| {
                (
                    ErrorCode::TerminalNotFound,
                    format!("terminal {id} does not exist"),
                )
            })
    }

    fn command(&mut self, request_id: u32, command: Command) -> Vec<FrameKind> {
        let mut frames = Vec::new();
        let result = self.resource_command(command, &mut frames);
        let reply = FrameKind::CommandResult {
            request_id,
            result: match result {
                Ok(()) => CommandResult::Ok,
                Err((code, message)) => CommandResult::Error { code, message },
            },
        };
        // Bootstrap must precede attach's ack; close is a notification after its ack.
        if matches!(frames.first(), Some(FrameKind::ResourceClosed { .. })) {
            frames.insert(0, reply);
        } else {
            frames.push(reply);
        }
        frames
    }

    fn resource_command(
        &mut self,
        command: Command,
        frames: &mut Vec<FrameKind>,
    ) -> Result<(), (ErrorCode, String)> {
        match command {
            Command::AttachResource {
                terminal_id,
                role_policy: None,
            } => {
                let index = self.terminal_index(&terminal_id)?;
                frames.extend(
                    self.terminals[index]
                        .bootstrap()
                        .map_err(|message| (ErrorCode::ResourceExhausted, message))?,
                );
            }
            Command::DetachResource { terminal_id } => {
                let index = self.terminal_index(&terminal_id)?;
                self.terminals[index].subscribed = false;
            }
            Command::KillResource {
                terminal_id,
                operation_id: None,
            } => {
                let index = self.terminal_index(&terminal_id)?;
                if self.terminals.len() == 1 {
                    return Err((
                        ErrorCode::PreconditionFailed,
                        "cannot close the last hosted terminal; end the session instead".to_owned(),
                    ));
                }
                self.terminals.remove(index);
                frames.push(FrameKind::ResourceClosed {
                    terminal_id,
                    exit_status: None,
                    reason: CloseReason::Killed,
                    signal: None,
                });
            }
            _ => {
                return Err((
                    ErrorCode::InvalidCommand,
                    "command or optional command policy is not supported by the hosted edge shell"
                        .to_owned(),
                ));
            }
        }
        Ok(())
    }

    fn resize(&mut self, id: &ResourceId, cols: u16, rows: u16) -> Vec<FrameKind> {
        let index = match self.terminal_index(id) {
            Ok(index) => index,
            Err((code, message)) => return vec![error(code, &message)],
        };
        if cols == 0 || rows == 0 {
            return Vec::new();
        }
        if !valid_size(cols, rows) {
            return vec![error(
                ErrorCode::MalformedMessage,
                "terminal axes must be between 1 and 1000",
            )];
        }
        let terminal = &mut self.terminals[index];
        if (terminal.cols, terminal.rows) == (cols, rows) {
            return Vec::new();
        }
        if terminal.generation >= u64::MAX - 1 {
            return vec![error(
                ErrorCode::ResourceExhausted,
                "terminal generation exhausted",
            )];
        }
        terminal.cols = cols;
        terminal.rows = rows;
        if terminal.subscribed {
            terminal
                .bootstrap()
                .unwrap_or_else(|message| vec![error(ErrorCode::ResourceExhausted, &message)])
        } else {
            Vec::new()
        }
    }

    fn input_key(&mut self, id: &ResourceId, event: &KeyEvent) -> Vec<FrameKind> {
        let terminal = match self.input_terminal(id) {
            Ok(terminal) => terminal,
            Err((code, message)) => return vec![error(code, &message)],
        };
        if let Err(message) = terminal.shell.validate_key(event) {
            return vec![error(ErrorCode::CanonicalLimitExceeded, &message)];
        }
        let output = terminal.shell.input(event);
        terminal.output(output)
    }

    fn input_paste(&mut self, id: &ResourceId, event: &PasteEvent) -> Vec<FrameKind> {
        let terminal = match self.input_terminal(id) {
            Ok(terminal) => terminal,
            Err((code, message)) => return vec![error(code, &message)],
        };
        if event.data.len() > MAX_INPUT_BYTES {
            return vec![error(
                ErrorCode::CanonicalLimitExceeded,
                "paste exceeds 4096 bytes",
            )];
        }
        let Ok(text) = std::str::from_utf8(&event.data) else {
            return vec![error(ErrorCode::UnsafePaste, "paste must be UTF-8 text")];
        };
        if event.trust == PasteTrust::Untrusted && text.chars().any(|c| c.is_control() && c != '\t')
        {
            return vec![error(
                ErrorCode::UnsafePaste,
                "untrusted paste contains control characters; paste a single line and press Enter separately",
            )];
        }
        match terminal.shell.paste(text) {
            Ok(output) => terminal.output(output),
            Err(message) => vec![error(ErrorCode::UnsafePaste, &message)],
        }
    }

    fn input_terminal(&mut self, id: &ResourceId) -> Result<&mut Terminal, (ErrorCode, String)> {
        let index = self.terminal_index(id)?;
        self.terminals[index].ensure_input_capacity()?;
        Ok(&mut self.terminals[index])
    }
}

fn hello_ok() -> FrameKind {
    FrameKind::HelloOk {
        protocol_major: PROTOCOL_VERSION.major,
        protocol_minor: PROTOCOL_VERSION.minor,
        protocol_patch: PROTOCOL_VERSION.patch,
        server_caps: ServerCapabilities::new()
            .with_features(ServerFeatureSet::with(&[ServerFeature::SpawnInitialSize])),
        server_id: b"phux-edge".to_vec(),
        selected_profile: BootstrapProfile::SynthesizedVtRaw,
        bootstrap_limits: BootstrapLimits::default(),
    }
}

fn error(code: ErrorCode, message: &str) -> FrameKind {
    FrameKind::Error {
        request_id: None,
        code,
        message: message.to_owned(),
    }
}

/// Client compression is forbidden, and exactly one bounded frame is allowed.
fn decode_inbound_frame(data: &[u8]) -> Option<FrameKind> {
    if data.len() > MAX_INBOUND_BYTES || data.get(4) == Some(&TYPE_FRAME_COMPRESSED) {
        return None;
    }
    let (frame, rest) = FrameKind::decode(data).ok()?;
    rest.is_empty().then_some(frame)
}

fn encode(frame: &FrameKind) -> Vec<u8> {
    let mut buffer = BytesMut::new();
    frame.encode(&mut buffer);
    buffer.to_vec()
}
