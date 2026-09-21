#![allow(
    clippy::wildcard_imports,
    clippy::unwrap_used,
    clippy::significant_drop_in_scrutinee,
    clippy::significant_drop_tightening,
    reason = "projection state uses one private mutex graph; poison means the owning runtime already panicked"
)]

use super::*;
use phux_client_runtime::control::{
    DirectoryFailure, FileUploadOutcome, SpawnRequest, TranscribeOutcome,
};

const SCROLLBACK_LINES: u32 = 1000;

#[uniffi::export(with_foreign)]
pub trait WireListener: Send + Sync {
    fn on_wire_activity(&self);
}

struct ListenerProjection(Arc<dyn WireListener>);

impl Listener for ListenerProjection {
    fn on_activity(&self) {
        self.0.on_wire_activity();
    }
}

#[derive(uniffi::Object)]
pub struct RemoteClient {
    url: String,
    cols: Mutex<u16>,
    rows: Mutex<u16>,
    fingerprint: Option<String>,
    token: Option<String>,
    client: Mutex<Option<Client>>,
    listener: Mutex<Option<Arc<dyn WireListener>>>,
    input_deliveries: Mutex<Vec<WireInputDelivery>>,
    authoritative_damage: Mutex<HashSet<ResourceId>>,
    #[cfg(feature = "engine")]
    generations: Mutex<HashMap<ResourceId, u64>>,
    pending_agent_metadata: Mutex<HashMap<u32, ResourceId>>,
}

#[uniffi::export]
pub fn scrollback_lines() -> u32 {
    SCROLLBACK_LINES
}

#[uniffi::export]
pub fn dial_timeout_millis() -> u64 {
    u64::try_from(phux_client_runtime::dial::DIAL_TIMEOUT.as_millis()).unwrap_or(u64::MAX)
}

#[uniffi::export]
pub fn initial_connect_budget_millis() -> u64 {
    u64::try_from(ConnectOptions::default().initial_budget.as_millis()).unwrap_or(u64::MAX)
}

#[uniffi::export]
impl RemoteClient {
    #[uniffi::constructor]
    pub fn new(
        url: String,
        cols: u16,
        rows: u16,
        fingerprint: Option<String>,
        token: Option<String>,
    ) -> Arc<Self> {
        Arc::new(Self {
            url,
            cols: Mutex::new(cols.max(1)),
            rows: Mutex::new(rows.max(1)),
            fingerprint,
            token,
            client: Mutex::new(None),
            listener: Mutex::new(None),
            input_deliveries: Mutex::new(Vec::new()),
            authoritative_damage: Mutex::new(HashSet::new()),
            #[cfg(feature = "engine")]
            generations: Mutex::new(HashMap::new()),
            pending_agent_metadata: Mutex::new(HashMap::new()),
        })
    }

    pub fn resize_viewport(&self, cols: u16, rows: u16) {
        let viewport = (cols.max(1), rows.max(1));
        *self.cols.lock().unwrap() = viewport.0;
        *self.rows.lock().unwrap() = viewport.1;
        if let Some(client) = self.runtime_client() {
            client.resize_viewport(viewport.0, viewport.1);
        }
    }

    pub fn connect(&self) -> Result<(), WireError> {
        let mut slot = self.client.lock().unwrap();
        if slot.is_some() {
            return Err(WireError::AlreadyConnected);
        }
        let target = Target {
            transport: Transport::Ws(self.url.clone()),
            name: self.url.clone(),
            cert_fingerprint: self.fingerprint.clone(),
            token_file: None,
            token: self.token.clone(),
        };
        let options = ClientOptions {
            control: phux_client_runtime::control::ControlOptions {
                client_name: "phux-mobile".to_owned(),
                viewport: (*self.cols.lock().unwrap(), *self.rows.lock().unwrap()),
                scrollback_lines: SCROLLBACK_LINES,
                attach: None,
                ..phux_client_runtime::control::ControlOptions::default()
            },
            connect: ConnectOptions::default(),
        };
        let client = Runtime::connect(target, options).map_err(|error| WireError::Runtime {
            message: error.to_string(),
        })?;
        if let Some(listener) = self.listener.lock().unwrap().clone() {
            client.set_listener(Arc::new(ListenerProjection(listener)));
        }
        *slot = Some(client);
        Ok(())
    }

    pub fn take_events(&self) -> Vec<WireEvent> {
        let Some(client) = self.runtime_client() else {
            return Vec::new();
        };
        let mut projected = Vec::new();
        for event in client.take_events() {
            self.project_event(event, &mut projected);
        }
        projected
    }

    pub fn take_input_deliveries(&self) -> Vec<WireInputDelivery> {
        std::mem::take(&mut *self.input_deliveries.lock().unwrap())
    }

    pub fn take_file_upload_receipts(&self) -> Vec<WireFileUploadReceipt> {
        self.runtime_client()
            .map(|client| {
                client
                    .take_file_upload_receipts()
                    .into_iter()
                    .map(|receipt| WireFileUploadReceipt {
                        transfer_id: receipt.transfer_id,
                        outcome: match receipt.outcome {
                            FileUploadOutcome::Completed => WireFileUploadOutcome::Completed,
                            FileUploadOutcome::Refused => WireFileUploadOutcome::Refused,
                            FileUploadOutcome::Unknown => WireFileUploadOutcome::Unknown,
                        },
                        path: receipt.path,
                        code: receipt.code,
                        message: receipt.message,
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    pub fn attach_session(&self, name: String) {
        if let Some(client) = self.runtime_client() {
            client.attach_session(AttachTarget::ByName(name));
        }
    }

    pub fn attach_terminal(&self, terminal_id: String) -> u32 {
        self.with_terminal(&terminal_id, Client::attach_terminal)
            .unwrap_or(0)
    }

    pub fn detach_terminal(&self, terminal_id: String) -> u32 {
        self.with_terminal(&terminal_id, Client::detach_terminal)
            .unwrap_or(0)
    }

    pub fn spawn_terminal(&self) {
        if let Some(client) = self.runtime_client() {
            let _ = client.spawn_terminal(SpawnRequest::default());
        }
    }

    pub fn spawn_terminal_in(&self, session: String) -> u32 {
        self.spawn_terminal_in_directory(session, None)
    }

    pub fn spawn_terminal_in_directory(&self, session: String, cwd: Option<String>) -> u32 {
        let Some(client) = self.runtime_client() else {
            return 0;
        };
        let session_id = client
            .topology()
            .and_then(|topology| topology.session_named(&session).map(|item| item.id));
        let Some(session_id) = session_id else {
            client.attach_session(AttachTarget::CreateIfMissing {
                name: session,
                command: None,
                cwd,
            });
            return 0;
        };
        client.attach_session(AttachTarget::ById(phux_protocol::SessionId::new(
            session_id,
        )));
        client.spawn_terminal(SpawnRequest {
            cwd,
            session_id: Some(session_id),
            ..SpawnRequest::default()
        })
    }

    pub fn spawn_terminal_with_command(&self, command: Vec<String>) -> u32 {
        self.runtime_client().map_or(0, |client| {
            client.spawn_terminal(SpawnRequest {
                command: Some(command),
                ..SpawnRequest::default()
            })
        })
    }

    pub fn list_directory(&self, path: String) -> u32 {
        self.runtime_client()
            .map_or(0, |client| client.list_directory(path))
    }

    pub fn take_directory_listings(&self) -> Vec<WireDirectoryListing> {
        self.runtime_client()
            .map(|client| {
                client
                    .take_directory_listings()
                    .into_iter()
                    .map(|listing| WireDirectoryListing {
                        request_id: listing.request_id,
                        path: listing.path,
                        parent: listing.parent,
                        entries: listing
                            .entries
                            .into_iter()
                            .map(|entry| WireDirectoryEntry {
                                name: entry.name,
                                is_symlink: entry.is_symlink,
                            })
                            .collect(),
                        truncated: listing.truncated,
                        error: listing.error.map(|error| match error {
                            DirectoryFailure::NotFound => WireDirectoryErrorCode::NotFound,
                            DirectoryFailure::PermissionDenied => {
                                WireDirectoryErrorCode::PermissionDenied
                            }
                            DirectoryFailure::NotADirectory => {
                                WireDirectoryErrorCode::NotADirectory
                            }
                            DirectoryFailure::Other => WireDirectoryErrorCode::Other,
                            DirectoryFailure::Unanswered => WireDirectoryErrorCode::Unanswered,
                        }),
                        message: listing.message,
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    pub fn kill_terminal(&self, terminal_id: String) {
        let _ = self.with_terminal(&terminal_id, Client::kill_terminal);
    }

    pub fn set_listener(&self, listener: Arc<dyn WireListener>) {
        *self.listener.lock().unwrap() = Some(listener.clone());
        if let Some(client) = self.runtime_client() {
            client.set_listener(Arc::new(ListenerProjection(listener)));
        }
    }

    pub fn input_ready(&self, terminal_id: String) -> bool {
        self.with_terminal(&terminal_id, Client::input_ready)
            .unwrap_or(false)
    }

    pub fn refresh_topology(&self) {
        if let Some(client) = self.runtime_client() {
            let _ = client.refresh_topology();
        }
    }

    pub fn resync(&self) {
        if let Some(client) = self.runtime_client() {
            client.resync();
        }
    }

    pub fn nudge(&self) {
        if let Some(client) = self.runtime_client() {
            client.nudge();
        }
    }

    pub fn close(&self) {
        if let Some(client) = self.runtime_client() {
            client.close();
        }
    }

    pub fn status(&self) -> WireStatus {
        match self.runtime_client().map(|client| client.status()) {
            None | Some(Status::Idle | Status::Connecting | Status::Negotiated) => {
                WireStatus::Connecting
            }
            Some(Status::Attached) => WireStatus::Attached,
            Some(Status::Closed) => WireStatus::Closed,
            Some(Status::Failed) => WireStatus::Failed,
        }
    }

    pub fn last_error(&self) -> Option<String> {
        self.runtime_client().and_then(|client| client.last_error())
    }

    pub fn server_protocol_version(&self) -> Option<String> {
        self.runtime_client().and_then(|client| {
            client.server().map(|server| {
                format!(
                    "{}.{}.{}",
                    server.protocol.0, server.protocol.1, server.protocol.2
                )
            })
        })
    }

    pub fn negotiated_capabilities(&self) -> Vec<String> {
        let Some(server) = self.runtime_client().and_then(|client| client.server()) else {
            return Vec::new();
        };
        [
            (ServerFeature::AcknowledgedInput, "acknowledged-input"),
            (ServerFeature::FileUpload, "file-upload"),
            (ServerFeature::Transcribe, "transcribe"),
            (ServerFeature::ListDirectory, "list-directory"),
        ]
        .into_iter()
        .filter(|(feature, _)| server.features.contains(*feature))
        .map(|(_, name)| name.to_owned())
        .collect()
    }

    pub fn topology(&self) -> Option<SessionTopology> {
        self.runtime_client()
            .and_then(|client| client.topology())
            .map(project_topology)
    }

    pub fn send_text(&self, terminal_id: String, text: String) {
        let _ = self.with_terminal(&terminal_id, |client, id| client.send_text(id, &text));
    }

    pub fn send_key(&self, terminal_id: String, key: KeyPress, mods: KeyMods) {
        let _ = self.with_terminal(&terminal_id, |client, id| {
            client.send_key(id, keymap::key_event(&key, mods))
        });
    }

    pub fn send_paste(&self, terminal_id: String, text: String) {
        self.send_paste_with(terminal_id, text, PasteTrust::Untrusted);
    }

    pub fn send_paste_trusted(&self, terminal_id: String, text: String) {
        self.send_paste_with(terminal_id, text, PasteTrust::Trusted);
    }

    pub fn apply_line(&self, terminal_id: String, text: String) -> u64 {
        self.apply_acknowledged(&terminal_id, |client, id| client.apply_line(id, &text))
    }

    pub fn apply_paste(&self, terminal_id: String, text: String) -> u64 {
        self.apply_acknowledged(&terminal_id, |client, id| client.apply_paste(id, &text))
    }

    pub fn apply_tab_completion(&self, terminal_id: String, text: String) -> u64 {
        self.apply_acknowledged(&terminal_id, |client, id| {
            client.apply_tab_completion(id, &text)
        })
    }

    pub fn put_file(&self, terminal_id: String, extension: String, data: Vec<u8>) -> u64 {
        let Some(client) = self.runtime_client() else {
            return 0;
        };
        let Some(id) = parse_terminal_id(&terminal_id) else {
            return client.refuse_file_upload("invalid terminal id");
        };
        client.put_file(id, extension, data)
    }

    pub fn transcribe(&self, transfer_id: u64) -> u32 {
        self.runtime_client()
            .map_or(0, |client| client.transcribe(transfer_id))
    }

    pub fn take_transcribe_receipts(&self) -> Vec<WireTranscribeReceipt> {
        self.runtime_client()
            .map(|client| {
                client
                    .take_transcribe_receipts()
                    .into_iter()
                    .map(|receipt| WireTranscribeReceipt {
                        request_id: receipt.request_id,
                        transfer_id: receipt.transfer_id,
                        outcome: match receipt.outcome {
                            TranscribeOutcome::Completed => WireTranscribeOutcome::Completed,
                            TranscribeOutcome::Refused => WireTranscribeOutcome::Refused,
                            TranscribeOutcome::Unknown => WireTranscribeOutcome::Unknown,
                        },
                        text: receipt.text,
                        pasted: receipt.pasted,
                        code: receipt.code,
                        message: receipt.message,
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    pub fn send_mouse(
        &self,
        terminal_id: String,
        action: MouseAction,
        button: MouseButton,
        mods: KeyMods,
        col: u32,
        row: u32,
    ) {
        let _ = self.with_terminal(&terminal_id, |client, id| {
            client.send_mouse(
                id,
                MouseEvent {
                    action: action.wire(),
                    button: button.wire(),
                    mods: mods.to_mod_set(),
                    x: f64::from(col),
                    y: f64::from(row),
                },
            )
        });
    }

    pub fn send_focus(&self, terminal_id: String, gained: bool) {
        let _ = self.with_terminal(&terminal_id, |client, id| {
            client.send_focus(
                id,
                if gained {
                    FocusEvent::Gained
                } else {
                    FocusEvent::Lost
                },
            )
        });
    }
}

#[cfg(not(feature = "engine"))]
#[uniffi::export]
impl RemoteClient {
    pub fn take_output(&self, terminal_id: String) -> Vec<u8> {
        self.with_terminal(&terminal_id, |client, id| {
            let bytes = client.take_output(id);
            if !bytes.is_empty() {
                client.acknowledge_projection(id);
            }
            bytes
        })
        .unwrap_or_default()
    }

    pub fn has_stream(&self, terminal_id: String) -> bool {
        self.with_terminal(&terminal_id, |client, id| {
            client.has_projection(id) && !client.is_closed(id)
        })
        .unwrap_or(false)
    }

    pub fn ensure_stream(&self, terminal_id: String) {
        let _ = self.with_terminal(&terminal_id, Client::ensure_stream);
    }
}

#[cfg(feature = "engine")]
#[uniffi::export]
impl RemoteClient {
    pub fn has_projection(&self, terminal_id: String) -> bool {
        self.with_terminal(&terminal_id, Client::has_projection)
            .unwrap_or(false)
    }

    pub fn retain_projection_on_close(&self, terminal_id: String) {
        let _ = self.with_terminal(&terminal_id, |client, id| {
            client.set_retain_on_close(id, true);
        });
    }

    pub fn release_projection(&self, terminal_id: String) {
        let Some(id) = parse_terminal_id(&terminal_id) else {
            return;
        };
        self.generations.lock().unwrap().remove(&id);
        if let Some(client) = self.runtime_client() {
            client.release(&id);
        }
    }

    pub fn projection_changed(&self, terminal_id: String) -> bool {
        let Some(id) = parse_terminal_id(&terminal_id) else {
            return false;
        };
        let Some(client) = self.runtime_client() else {
            return false;
        };
        let Some(generation) = client.generation(&id) else {
            return false;
        };
        self.generations
            .lock()
            .unwrap()
            .get(&id)
            .copied()
            .unwrap_or(0)
            != generation
    }

    #[allow(
        clippy::unnecessary_wraps,
        reason = "preserves the established UniFFI throws signature for generated clients"
    )]
    pub fn render_projection(
        &self,
        terminal_id: String,
    ) -> Result<Option<crate::engine::GridProjection>, crate::engine::EngineError> {
        let Some(id) = parse_terminal_id(&terminal_id) else {
            return Ok(None);
        };
        let Some(client) = self.runtime_client() else {
            return Ok(None);
        };
        let Some(frame) = client.acquire(&id) else {
            return Ok(None);
        };
        self.generations
            .lock()
            .unwrap()
            .insert(id.clone(), frame.generation);
        if self.authoritative_damage.lock().unwrap().remove(&id) {
            client.acknowledge_projection(&id);
        }
        Ok(Some(crate::engine::GridProjection {
            cols: frame.cols,
            rows: frame.rows,
            cells: crate::engine::encode_cells(&frame.buffer.cells),
            utf8: frame.buffer.utf8.clone(),
            cursor_col: frame.cursor.visible.then_some(frame.cursor.col),
            cursor_row: frame.cursor.visible.then_some(frame.cursor.row),
            cursor_visible: frame.cursor.visible,
            cursor_shape: match frame.cursor.style {
                phux_client_core::grid::CursorStyle::Block => crate::engine::CursorShape::Block,
                phux_client_core::grid::CursorStyle::Bar => crate::engine::CursorShape::Bar,
                phux_client_core::grid::CursorStyle::Underline => {
                    crate::engine::CursorShape::Underline
                }
                phux_client_core::grid::CursorStyle::BlockHollow => {
                    crate::engine::CursorShape::BlockHollow
                }
            },
            cursor_blinking: frame.cursor.blinking,
            default_fg: crate::engine::Color {
                r: frame.colors.foreground.r,
                g: frame.colors.foreground.g,
                b: frame.colors.foreground.b,
            },
            default_bg: crate::engine::Color {
                r: frame.colors.background.r,
                g: frame.colors.background.g,
                b: frame.colors.background.b,
            },
            generation: frame.generation,
            dirty_rows: frame.dirty_rows().collect(),
            scrollbar: crate::engine::ScrollbarState {
                total: frame.scrollbar.total,
                offset: frame.scrollbar.offset,
                len: frame.scrollbar.len,
            },
        }))
    }

    pub fn predict_projection_text(&self, terminal_id: String, text: String) {
        let _ = self.with_terminal(&terminal_id, |client, id| client.predict_text(id, text));
    }

    pub fn clear_projection_predictions(&self, terminal_id: String) {
        let _ = self.with_terminal(&terminal_id, Client::clear_predictions);
    }

    pub fn scroll_projection(&self, terminal_id: String, delta: i64) {
        let _ = self.with_terminal(&terminal_id, |client, id| {
            client.scroll(id, phux_client_runtime::engine::Scroll::Delta(delta))
        });
    }

    pub fn scroll_projection_to_bottom(&self, terminal_id: String) {
        let _ = self.with_terminal(&terminal_id, |client, id| {
            client.scroll(id, phux_client_runtime::engine::Scroll::Bottom)
        });
    }

    #[allow(
        clippy::unnecessary_wraps,
        reason = "preserves the established UniFFI throws signature for generated clients"
    )]
    pub fn projection_scrollbar(
        &self,
        terminal_id: String,
    ) -> Result<Option<crate::engine::ScrollbarState>, crate::engine::EngineError> {
        let Some(id) = parse_terminal_id(&terminal_id) else {
            return Ok(None);
        };
        Ok(self
            .runtime_client()
            .and_then(|client| client.acquire(&id))
            .map(|frame| crate::engine::ScrollbarState {
                total: frame.scrollbar.total,
                offset: frame.scrollbar.offset,
                len: frame.scrollbar.len,
            }))
    }

    pub fn projection_is_alt_screen(&self, terminal_id: String) -> bool {
        self.with_terminal(&terminal_id, Client::is_alt_screen)
            .unwrap_or(false)
    }
}

impl RemoteClient {
    fn runtime_client(&self) -> Option<Client> {
        self.client.lock().unwrap().clone()
    }

    fn with_terminal<T>(
        &self,
        terminal_id: &str,
        call: impl FnOnce(&Client, &ResourceId) -> T,
    ) -> Option<T> {
        let id = parse_terminal_id(terminal_id)?;
        let client = self.runtime_client()?;
        Some(call(&client, &id))
    }

    fn apply_acknowledged(
        &self,
        terminal_id: &str,
        apply: impl FnOnce(&Client, &ResourceId) -> u64,
    ) -> u64 {
        let Some(client) = self.runtime_client() else {
            return 0;
        };
        let Some(id) = parse_terminal_id(terminal_id) else {
            return client.refuse_acknowledged_input("invalid terminal id");
        };
        apply(&client, &id)
    }

    fn send_paste_with(&self, terminal_id: String, text: String, trust: PasteTrust) {
        let _ = self.with_terminal(&terminal_id, |client, id| {
            client.send_paste(id, text.into_bytes(), trust)
        });
    }

    fn sync_agent_metadata(&self) {
        let Some(client) = self.runtime_client() else {
            return;
        };
        let supports_metadata = client
            .server()
            .is_some_and(|server| server.layers.contains(Layer::L3));
        if !supports_metadata {
            return;
        }
        let Some(topology) = client.topology() else {
            return;
        };
        // A topology snapshot supersedes unanswered reads from the previous
        // connection or graph revision; their late replies are ignored.
        self.pending_agent_metadata.lock().unwrap().clear();
        for pane in topology.panes {
            let request_id = client.next_request_id();
            self.pending_agent_metadata
                .lock()
                .unwrap()
                .insert(request_id, pane.terminal_id.clone());
            client.queue_frame(&FrameKind::GetMetadata {
                request_id,
                scope: Scope::Resource(pane.terminal_id.clone()),
                key: RESOURCE_AGENT_KEY.to_owned(),
            });
            client.queue_frame(&FrameKind::SubscribeMetadata {
                scope: Scope::Resource(pane.terminal_id),
                key: RESOURCE_AGENT_KEY.to_owned(),
            });
        }
    }

    fn project_metadata_frame(&self, frame: FrameKind, projected: &mut Vec<WireEvent>) {
        match frame {
            FrameKind::MetadataChanged {
                scope: Scope::Resource(id),
                key,
                value,
                ..
            } if key == RESOURCE_AGENT_KEY => {
                projected.push(agent_state_event(&id, value.as_deref()));
            }
            FrameKind::MetadataValue { request_id, value } => {
                if let Some(id) = self
                    .pending_agent_metadata
                    .lock()
                    .unwrap()
                    .remove(&request_id)
                {
                    projected.push(agent_state_event(&id, value.as_deref()));
                }
            }
            _ => {}
        }
    }

    fn project_event(&self, event: Event, projected: &mut Vec<WireEvent>) {
        match event {
            Event::TopologyChanged => {
                projected.push(WireEvent::TopologyChanged);
                self.sync_agent_metadata();
            }
            Event::TerminalChanged { terminal_id } => {
                self.authoritative_damage
                    .lock()
                    .unwrap()
                    .insert(terminal_id);
            }
            lifecycle @ (Event::PaneSpawned { .. }
            | Event::TerminalSpawned { .. }
            | Event::TerminalAttached { .. }
            | Event::TerminalDetached { .. }
            | Event::TerminalClosed { .. }) => project_lifecycle(lifecycle, projected),
            signal @ (Event::Bell { .. }
            | Event::TitleChanged { .. }
            | Event::OutputStarted { .. }
            | Event::OutputSettled { .. }
            | Event::CommandStarted { .. }
            | Event::CommandFinished { .. }
            | Event::CwdChanged { .. }) => project_terminal_signal(signal, projected),
            Event::AgentAsked {
                terminal_id,
                question_id,
                text,
                suggestions,
                waiting_seconds,
            } => projected.push(WireEvent::AgentAsked {
                terminal_id: terminal_id_string(&terminal_id),
                question_id,
                text,
                suggestions,
                waiting_seconds,
            }),
            Event::InputDelivery {
                delivery_id,
                outcome,
                code,
                message,
            } => self
                .input_deliveries
                .lock()
                .unwrap()
                .push(WireInputDelivery {
                    delivery_id,
                    outcome: project_delivery_outcome(outcome),
                    code,
                    message,
                }),
            Event::ServerError { message, .. } => {
                projected.push(WireEvent::ServerError { message });
            }
            Event::Frame(frame) => self.project_metadata_frame(*frame, projected),
            _ => {}
        }
    }
}

fn project_lifecycle(event: Event, projected: &mut Vec<WireEvent>) {
    let event = match event {
        Event::PaneSpawned { terminal_id } => WireEvent::PaneSpawned {
            terminal_id: terminal_id_string(&terminal_id),
        },
        Event::TerminalSpawned {
            request_id,
            terminal_id,
            error,
        } => WireEvent::TerminalSpawned {
            request_id,
            terminal_id: terminal_id.as_ref().map(terminal_id_string),
            error,
        },
        Event::TerminalAttached {
            request_id,
            terminal_id,
            error,
        } => WireEvent::TerminalAttached {
            request_id,
            terminal_id: terminal_id_string(&terminal_id),
            error,
        },
        Event::TerminalDetached {
            request_id,
            terminal_id,
            error,
        } => WireEvent::TerminalDetached {
            request_id,
            terminal_id: terminal_id_string(&terminal_id),
            error,
        },
        Event::TerminalClosed {
            terminal_id,
            exit_status,
            ..
        } => WireEvent::PaneClosed {
            terminal_id: terminal_id_string(&terminal_id),
            exit_status,
        },
        _ => return,
    };
    projected.push(event);
}

fn project_terminal_signal(event: Event, projected: &mut Vec<WireEvent>) {
    let event = match event {
        Event::Bell { terminal_id } => WireEvent::Bell {
            terminal_id: terminal_id_string(&terminal_id),
        },
        Event::TitleChanged { terminal_id, title } => WireEvent::TitleChanged {
            terminal_id: terminal_id_string(&terminal_id),
            title,
        },
        Event::OutputStarted { terminal_id } => WireEvent::OutputStarted {
            terminal_id: terminal_id_string(&terminal_id),
        },
        Event::OutputSettled { terminal_id } => WireEvent::OutputSettled {
            terminal_id: terminal_id_string(&terminal_id),
        },
        Event::CommandStarted { terminal_id } => WireEvent::CommandStarted {
            terminal_id: terminal_id_string(&terminal_id),
        },
        Event::CommandFinished {
            terminal_id,
            exit_code,
        } => WireEvent::CommandFinished {
            terminal_id: terminal_id_string(&terminal_id),
            exit_code,
        },
        Event::CwdChanged { terminal_id, cwd } => WireEvent::CwdChanged {
            terminal_id: terminal_id_string(&terminal_id),
            cwd,
        },
        _ => return,
    };
    projected.push(event);
}

fn project_delivery_outcome(outcome: DeliveryOutcome) -> WireInputDeliveryOutcome {
    match outcome {
        DeliveryOutcome::Delivered => WireInputDeliveryOutcome::Delivered,
        DeliveryOutcome::Refused => WireInputDeliveryOutcome::Refused,
        DeliveryOutcome::Unknown => WireInputDeliveryOutcome::Unknown,
    }
}

#[derive(Default)]
struct ParsedAgentRecord {
    name: String,
    kind: Option<String>,
    session: Option<String>,
    state: AgentState,
    attention: Option<AgentAttention>,
}

fn agent_state_event(id: &ResourceId, bytes: Option<&[u8]>) -> WireEvent {
    let record = bytes.and_then(parse_agent_record).unwrap_or_default();
    let attention = record
        .attention
        .unwrap_or_else(|| derived_attention(record.state));
    WireEvent::AgentStateChanged {
        terminal_id: terminal_id_string(id),
        name: record.name,
        kind: record.kind,
        session: record.session,
        state: record.state,
        attention,
    }
}

fn parse_agent_record(bytes: &[u8]) -> Option<ParsedAgentRecord> {
    let value: serde_json::Value = serde_json::from_slice(bytes).ok()?;
    let object = value.as_object()?;
    let name = object.get("name")?.as_str()?;
    if name.is_empty() {
        return None;
    }
    Some(ParsedAgentRecord {
        name: name.to_owned(),
        kind: object
            .get("kind")
            .and_then(|value| value.as_str())
            .map(str::to_owned),
        session: object
            .get("session")
            .and_then(|value| value.as_str())
            .map(str::to_owned),
        state: object
            .get("state")
            .and_then(|value| value.as_str())
            .map_or(AgentState::Unknown, agent_state_from),
        attention: object
            .get("attention")
            .and_then(|value| value.as_str())
            .map(agent_attention_from),
    })
}

fn agent_state_from(word: &str) -> AgentState {
    match word {
        "idle" => AgentState::Idle,
        "working" => AgentState::Working,
        "blocked" => AgentState::Blocked,
        "done" => AgentState::Done,
        _ => AgentState::Unknown,
    }
}

fn agent_attention_from(word: &str) -> AgentAttention {
    match word {
        "none" => AgentAttention::None,
        "low" => AgentAttention::Low,
        "high" => AgentAttention::High,
        _ => AgentAttention::Normal,
    }
}

fn derived_attention(state: AgentState) -> AgentAttention {
    match state {
        AgentState::Blocked => AgentAttention::High,
        AgentState::Working => AgentAttention::Normal,
        AgentState::Done | AgentState::Unknown => AgentAttention::Low,
        AgentState::Idle => AgentAttention::None,
    }
}

impl Drop for RemoteClient {
    fn drop(&mut self) {
        if let Some(client) = self.client.get_mut().unwrap().take() {
            client.close();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agent_metadata_keeps_open_vocabulary_defaults() {
        let event = agent_state_event(
            &ResourceId::local(9),
            Some(br#"{"name":"Claude","kind":"claude","state":"newer"}"#),
        );
        assert_eq!(
            event,
            WireEvent::AgentStateChanged {
                terminal_id: "local:9".to_owned(),
                name: "Claude".to_owned(),
                kind: Some("claude".to_owned()),
                session: None,
                state: AgentState::Unknown,
                attention: AgentAttention::Low,
            }
        );
    }

    #[test]
    fn malformed_agent_metadata_clears_the_badge() {
        let event = agent_state_event(&ResourceId::local(2), Some(b"not-json"));
        assert_eq!(
            event,
            WireEvent::AgentStateChanged {
                terminal_id: "local:2".to_owned(),
                name: String::new(),
                kind: None,
                session: None,
                state: AgentState::Unknown,
                attention: AgentAttention::Low,
            }
        );
    }
}
