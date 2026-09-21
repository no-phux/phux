#![allow(
    clippy::wildcard_imports,
    clippy::unwrap_used,
    clippy::significant_drop_in_scrutinee,
    clippy::significant_drop_tightening,
    reason = "projection state uses one private mutex graph; poison means the owning runtime already panicked"
)]

use super::*;
use phux_client_runtime::control::SpawnRequest;

use crate::uniffi::engine;

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
            reason: error.to_string(),
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
                    .map(|receipt| outcome::upload_receipt(receipt).into())
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
                    .map(|listing| outcome::directory(listing).into())
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

    pub fn stop_connection(&self) {
        if let Some(client) = self.runtime_client() {
            client.close();
        }
    }

    pub fn status(&self) -> WireStatus {
        status::connection(self.runtime_client().map(|client| client.status())).into()
    }

    pub fn last_error(&self) -> Option<String> {
        self.runtime_client().and_then(|client| client.last_error())
    }

    pub fn server_protocol_version(&self) -> Option<String> {
        self.runtime_client()
            .and_then(|client| client.server())
            .map(|server| status::protocol_version(&server))
    }

    pub fn negotiated_capabilities(&self) -> Vec<String> {
        self.runtime_client()
            .and_then(|client| client.server())
            .as_ref()
            .map(status::negotiated_features)
            .unwrap_or_default()
    }

    pub fn topology(&self) -> Option<SessionTopology> {
        self.runtime_client()
            .and_then(|client| client.topology())
            .map(|graph| topology::session_graph(graph).into())
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
        let Some(id) = id::parse(&terminal_id) else {
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
                    .map(|receipt| outcome::transcribe_receipt(receipt).into())
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
        let Some(id) = id::parse(&terminal_id) else {
            return;
        };
        self.generations.lock().unwrap().remove(&id);
        if let Some(client) = self.runtime_client() {
            client.release(&id);
        }
    }

    pub fn projection_changed(&self, terminal_id: String) -> bool {
        let Some(id) = id::parse(&terminal_id) else {
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
    ) -> Result<Option<engine::GridProjection>, engine::EngineError> {
        let Some(id) = id::parse(&terminal_id) else {
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
        Ok(Some(engine::grid_projection(
            &grid::view(&frame),
            grid::dirty_rows(&frame),
        )))
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
    ) -> Result<Option<engine::ScrollbarState>, engine::EngineError> {
        let Some(id) = id::parse(&terminal_id) else {
            return Ok(None);
        };
        Ok(self
            .runtime_client()
            .and_then(|client| client.acquire(&id))
            .map(|frame| engine::scrollbar_state(grid::view(&frame).scrollbar)))
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
        let id = id::parse(terminal_id)?;
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
        let Some(id) = id::parse(terminal_id) else {
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
                projected.push(agent::badge(&id, value.as_deref()).into());
            }
            FrameKind::MetadataValue { request_id, value } => {
                if let Some(id) = self
                    .pending_agent_metadata
                    .lock()
                    .unwrap()
                    .remove(&request_id)
                {
                    projected.push(agent::badge(&id, value.as_deref()).into());
                }
            }
            _ => {}
        }
    }

    /// Lower one runtime event.
    ///
    /// The two classifiers run first: whatever they recognize is a product
    /// fact the projection layer already decided, and this method only
    /// lowers it. What is left is either bridge-local state (the damage
    /// bookkeeping, the metadata correlation) or a fact with no Swift
    /// vocabulary.
    fn project_event(&self, event: Event, projected: &mut Vec<WireEvent>) {
        let event = match event::terminal_signal(event) {
            Ok(signal) => return projected.extend(Option::<WireEvent>::from(signal)),
            Err(event) => event,
        };
        let event = match event::lifecycle(event) {
            Ok(lifecycle) => return projected.extend(Option::<WireEvent>::from(lifecycle)),
            Err(event) => event,
        };
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
            Event::AgentAsked {
                terminal_id,
                question_id,
                text,
                suggestions,
                waiting_seconds,
            } => projected.push(WireEvent::AgentAsked {
                terminal_id: id::encode(&terminal_id),
                question_id,
                text,
                suggestions,
                waiting_seconds,
            }),
            Event::InputDelivery {
                delivery_id,
                outcome: result,
                code,
                message,
            } => self.input_deliveries.lock().unwrap().push(
                outcome::InputDelivery {
                    delivery_id,
                    outcome: outcome::delivery(result),
                    code,
                    message,
                }
                .into(),
            ),
            Event::Frame(frame) => self.project_metadata_frame(*frame, projected),
            _ => {}
        }
    }
}

impl Drop for RemoteClient {
    fn drop(&mut self) {
        if let Some(client) = self.client.get_mut().unwrap().take() {
            client.close();
        }
    }
}
