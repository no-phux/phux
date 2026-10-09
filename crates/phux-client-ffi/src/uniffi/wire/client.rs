#![allow(
    clippy::wildcard_imports,
    clippy::unwrap_used,
    clippy::significant_drop_in_scrutinee,
    clippy::significant_drop_tightening,
    reason = "projection state uses one private mutex graph; poison means the owning runtime already panicked"
)]

use super::*;
use phux_client_runtime::control::{Observation, SpawnRequest};

use crate::uniffi::engine;

#[cfg(test)]
mod quic_live;
mod search;

const SCROLLBACK_LINES: u32 = 1000;

/// Classify the stable string constructor's endpoint before the runtime owns
/// the session. QUIC drops its URI scheme because [`Transport::Quic`] carries
/// only the authority; WebSocket keeps the complete URL.
fn classify_remote_endpoint(endpoint: &str) -> Result<Transport, String> {
    let endpoint = endpoint.trim();
    if let Some(authority) = endpoint.strip_prefix("quic://") {
        validate_quic_authority(authority)?;
        return Ok(Transport::Quic(authority.to_owned()));
    }
    if endpoint.starts_with("ws://") || endpoint.starts_with("wss://") {
        return Ok(Transport::Ws(endpoint.to_owned()));
    }
    Err(format!(
        "{endpoint:?} must start with quic://, wss://, or ws://"
    ))
}

/// Reject malformed QUIC targets without starting the runtime's reconnect
/// loop. DNS resolution and reachability remain the runtime dialer's job.
fn validate_quic_authority(authority: &str) -> Result<(), String> {
    let (host, port) = authority
        .rsplit_once(':')
        .ok_or_else(|| format!("quic://{authority} needs HOST:PORT (bracket an IPv6 address)"))?;
    let bracketed = host.starts_with('[') && host.ends_with(']');
    let bare_host = host.trim_matches(['[', ']']);
    let valid_host = !bare_host.is_empty()
        && !authority.contains(['/', '?', '#', '@'])
        && (bracketed || !host.contains(':'));
    let valid_port = port.parse::<u16>().is_ok_and(|port| port != 0);
    if valid_host && valid_port {
        return Ok(());
    }
    Err(format!(
        "quic://{authority} needs HOST:PORT with a non-zero numeric port; bracket an IPv6 address"
    ))
}

/// A P-256 key the platform keystore holds (Secure Enclave, StrongBox):
/// the phone's workload identity (ADR-0154). The private key never crosses
/// this boundary.
#[uniffi::export(with_foreign)]
pub trait DeviceKey: Send + Sync {
    /// The uncompressed public point, X9.62 (`0x04 || X || Y`, 65 bytes):
    /// `SecKeyCopyExternalRepresentation` of the public key.
    fn public_point(&self) -> Vec<u8>;
    /// ECDSA P-256 over SHA-256 of `message`, DER-encoded
    /// (`ecdsaSignatureMessageX962SHA256`).
    fn sign(&self, message: Vec<u8>) -> Result<Vec<u8>, DeviceKeyError>;
}

/// A keystore failure, reported to the user as given.
#[derive(uniffi::Error, Debug, thiserror::Error)]
pub enum DeviceKeyError {
    #[error("{reason}")]
    Failed { reason: String },
}

/// The runtime's view of a [`DeviceKey`].
struct DeviceKeySigner(Arc<dyn DeviceKey>);

impl phux_client_runtime::enroll::DeviceSigner for DeviceKeySigner {
    fn public_point(&self) -> Vec<u8> {
        self.0.public_point()
    }

    fn sign(&self, message: &[u8]) -> Result<Vec<u8>, String> {
        self.0.sign(message.to_vec()).map_err(|err| err.to_string())
    }
}

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

impl RemoteClient {
    /// The runtime target `connect` and `enroll_device` dial.
    fn target(&self) -> Result<Target, WireError> {
        let transport =
            classify_remote_endpoint(&self.url).map_err(|reason| WireError::Runtime {
                reason: format!("invalid remote endpoint: {reason}"),
            })?;
        Ok(Target {
            transport,
            name: self.url.clone(),
            cert_fingerprint: self.fingerprint.clone(),
            authority: self.authority_pin(),
            token_file: None,
            token: self.token.clone(),
            tls_server_name: self.route.lock().unwrap().clone(),
            client_identity: self.client_identity.lock().unwrap().clone(),
        })
    }

    /// The pin `connect` dials with: the stored authority, or a learner that
    /// records the one a leaf-pinned server presents.
    fn authority_pin(&self) -> phux_client_runtime::target::AuthorityPin {
        let ca = self.authority.lock().unwrap().clone();
        let route = self.route.lock().unwrap().clone();
        // A relayed server's CA is never learned: the relay's leaf does not
        // authenticate it (ADR-0154).
        let learner = (ca.is_none() && route.is_none()).then(|| {
            let slot = Arc::clone(&self.learned_authority);
            phux_client_runtime::AuthorityLearner::new(move |authority| {
                *slot.lock().unwrap() = Some(authority.to_owned());
            })
        });
        phux_client_runtime::target::AuthorityPin { ca, learner, route }
    }
}

#[derive(uniffi::Object)]
pub struct RemoteClient {
    url: String,
    cols: Mutex<u16>,
    rows: Mutex<u16>,
    fingerprint: Option<String>,
    token: Option<String>,
    /// The certificate-authority pin beside the leaf pin (ADR-0153), set
    /// before `connect` by [`RemoteClient::set_authority_pin`].
    authority: Mutex<Option<String>>,
    /// The relay route the server is reached through (ADR-0149), set by
    /// [`RemoteClient::set_relay_route`].
    route: Mutex<Option<String>>,
    /// The CA a leaf-pinned connection learned, for the embedder to store.
    learned_authority: Arc<Mutex<Option<String>>>,
    /// The workload identity every dial presents (ADR-0154), once enrolled.
    client_identity: Mutex<phux_client_runtime::TlsClientIdentity>,
    client: Mutex<Option<Client>>,
    listener: Mutex<Option<Arc<dyn WireListener>>>,
    input_deliveries: Mutex<Vec<WireInputDelivery>>,
    authoritative_damage: Mutex<HashSet<ResourceId>>,
    generations: Mutex<HashMap<ResourceId, u64>>,
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
        Self::build(url, cols, rows, fingerprint, token, None)
    }

    /// Construct a relay-routed client without changing the legacy constructor.
    /// The DNS route overrides TLS SNI, not the dial endpoint or certificate pin.
    /// Initializes the same route as [`Self::set_relay_route`]; the server's
    /// authority pin and enrolled device identity are still set separately.
    ///
    /// # Errors
    /// Returns an error for an invalid TLS DNS route before starting a runtime.
    #[uniffi::constructor]
    pub fn new_routed(
        url: String,
        cols: u16,
        rows: u16,
        fingerprint: Option<String>,
        token: Option<String>,
        tls_server_name: String,
    ) -> Result<Arc<Self>, WireError> {
        let tls_server_name =
            phux_client_runtime::target::validate_tls_server_name(&tls_server_name)
                .map_err(|reason| WireError::Runtime { reason })?;
        Ok(Self::build(
            url,
            cols,
            rows,
            fingerprint,
            token,
            Some(tls_server_name),
        ))
    }

    pub fn resize_viewport(&self, cols: u16, rows: u16) {
        let viewport = (cols.max(1), rows.max(1));
        *self.cols.lock().unwrap() = viewport.0;
        *self.rows.lock().unwrap() = viewport.1;
        if let Some(client) = self.runtime_client() {
            client.resize_viewport(viewport.0, viewport.1);
        }
    }

    /// Reach the server through a relay route (a connect link's `sni`,
    /// ADR-0149): the URL is then the relay, the fingerprint the relay's,
    /// and the authority pin the server's, verified end to end inside the
    /// relayed stream, where the device identity is presented too
    /// (ADR-0154). Takes effect at the next `connect` or `enroll_device`.
    pub fn set_relay_route(&self, route: Option<String>) {
        *self.route.lock().unwrap() = route.filter(|route| !route.trim().is_empty());
    }

    /// Pin the server's certificate authority (`sha256:` fingerprint,
    /// ADR-0153) beside the leaf fingerprint, or clear the pin. Takes effect
    /// at the next `connect`. A pinned server presenting another authority is
    /// refused with a "certificate authority changed" error that names both
    /// fingerprints, and the connection does not retry.
    pub fn set_authority_pin(&self, authority: Option<String>) {
        *self.authority.lock().unwrap() = authority.filter(|pin| !pin.trim().is_empty());
    }

    /// The certificate authority (`sha256:` fingerprint) the server presented
    /// on a connection authenticated by the leaf pin alone, once one has.
    /// Store it beside the leaf pin and pass it to `set_authority_pin` from
    /// then on (trust on first connect, ADR-0153). `None` until then, and
    /// while an authority is pinned.
    pub fn learned_authority(&self) -> Option<String> {
        self.learned_authority.lock().unwrap().clone()
    }

    /// Enroll `key` as this device's workload identity with the single-use
    /// `ticket` a pairing link carried (`enroll=`), over this client's
    /// `quic://` endpoint and pins. Blocks for at most the dial timeout; call
    /// it off the main thread, before `connect`. Returns the issued chain
    /// (public PEM): store it beside the key and hand both to
    /// `set_device_identity` on later launches. The server's CA is pinned
    /// for this client too ([`Self::learned_authority`]).
    pub fn enroll_device(
        &self,
        ticket: String,
        key: Arc<dyn DeviceKey>,
    ) -> Result<String, WireError> {
        let target = self.target()?;
        let signer: Arc<dyn phux_client_runtime::enroll::DeviceSigner> =
            Arc::new(DeviceKeySigner(key));
        let enrolled =
            phux_client_runtime::enroll::enroll_target_blocking(&target, &ticket, signer)
                .map_err(|reason| WireError::Runtime { reason })?;
        *self.client_identity.lock().unwrap() =
            phux_client_runtime::TlsClientIdentity::Held(enrolled.identity);
        if self.authority.lock().unwrap().is_none() {
            *self.learned_authority.lock().unwrap() = Some(enrolled.authority);
        }
        Ok(enrolled.chain_pem)
    }

    /// Present the enrolled chain, signed by `key`, on every dial from the
    /// next `connect` (ADR-0154).
    pub fn set_device_identity(
        &self,
        chain_pem: String,
        key: Arc<dyn DeviceKey>,
    ) -> Result<(), WireError> {
        let identity =
            phux_client_runtime::enroll::held_identity(&chain_pem, Arc::new(DeviceKeySigner(key)))
                .map_err(|reason| WireError::Runtime { reason })?;
        *self.client_identity.lock().unwrap() =
            phux_client_runtime::TlsClientIdentity::Held(identity);
        Ok(())
    }

    pub fn connect(&self) -> Result<(), WireError> {
        let mut slot = self.client.lock().unwrap();
        if slot.is_some() {
            return Err(WireError::AlreadyConnected);
        }
        let target = self.target()?;
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

    /// Drain events and sample status, error, topology and the connection
    /// epoch in one step. Prefer this to `take_events` plus separate reads:
    /// those can straddle a reconnect. Both drain the same queue, so a
    /// consumer uses one or the other.
    pub fn take_publication(&self) -> WirePublication {
        let Some(client) = self.runtime_client() else {
            return WirePublication {
                connection_epoch: 0,
                events: Vec::new(),
                events_dropped: false,
                status: status::connection(None).into(),
                last_error: None,
                topology: None,
            };
        };
        self.publication(client.take_observation())
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

    /// Browse immediate children when `recursive` is false; search beneath
    /// `root` when true. `host` names a satellite; `None` uses the serving
    /// host. Zero means unavailable or capacity exhausted, with no request
    /// sent. Results are paths, not text to paste into a focused pane.
    pub fn path_query(
        &self,
        root: String,
        query: String,
        recursive: bool,
        host: Option<String>,
    ) -> u32 {
        self.runtime_client().map_or(0, |client| {
            client.path_query(
                root,
                query,
                recursive,
                host.filter(|name| !name.is_empty())
                    .map(phux_protocol::ids::SatelliteHost::new),
            )
        })
    }

    /// Drain correlated answers and disconnect cancellations.
    pub fn take_path_answers(&self) -> Vec<WirePathAnswer> {
        self.runtime_client()
            .map(|client| {
                client
                    .take_path_answers()
                    .into_iter()
                    .map(|answer| outcome::path_result(answer).into())
                    .collect()
            })
            .unwrap_or_default()
    }

    pub fn kill_terminal(&self, terminal_id: String) {
        let _ = self.with_terminal(&terminal_id, Client::kill_terminal);
    }

    /// Take the pane's input lease (ADR-0033): cooperative when `seize` is
    /// false (refused while another client drives), preempting when true.
    /// Returns the request id, or 0 for an unparsable terminal id. The
    /// outcome arrives as `WireEvent::InputHolderChanged`.
    pub fn acquire_input(&self, terminal_id: String, seize: bool) -> u32 {
        self.with_terminal(&terminal_id, |client, id| client.acquire_input(id, seize))
            .unwrap_or(0)
    }

    /// Give the wheel back; a no-op when this connection does not hold it.
    pub fn release_input(&self, terminal_id: String) -> u32 {
        self.with_terminal(&terminal_id, Client::release_input)
            .unwrap_or(0)
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

    /// The device's network path changed: a QUIC connection migrates onto
    /// a fresh socket and keeps its incarnation, then every lane probes as
    /// `nudge` does. Call from the path monitor instead of `nudge`.
    pub fn network_path_changed(&self) {
        if let Some(client) = self.runtime_client() {
            client.network_path_changed();
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

impl RemoteClient {
    fn build(
        url: String,
        cols: u16,
        rows: u16,
        fingerprint: Option<String>,
        token: Option<String>,
        tls_server_name: Option<String>,
    ) -> Arc<Self> {
        Arc::new(Self {
            url,
            cols: Mutex::new(cols.max(1)),
            rows: Mutex::new(rows.max(1)),
            fingerprint,
            token,
            authority: Mutex::new(None),
            route: Mutex::new(tls_server_name),
            learned_authority: Arc::new(Mutex::new(None)),
            client_identity: Mutex::new(phux_client_runtime::TlsClientIdentity::None),
            client: Mutex::new(None),
            listener: Mutex::new(None),
            input_deliveries: Mutex::new(Vec::new()),
            authoritative_damage: Mutex::new(HashSet::new()),
            generations: Mutex::new(HashMap::new()),
        })
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

    fn publication(&self, observation: Observation) -> WirePublication {
        let mut events = Vec::new();
        for event in observation.events {
            match event {
                Event::ConnectionLost { message } => {
                    events.push(WireEvent::ConnectionLost { message });
                }
                Event::ConnectionOpened { connection_epoch } => {
                    events.push(WireEvent::ConnectionOpened { connection_epoch });
                }
                event => self.project_event(event, &mut events),
            }
        }
        WirePublication {
            connection_epoch: observation.connection_epoch,
            events,
            events_dropped: observation.events_dropped,
            status: status::connection(Some(observation.status)).into(),
            last_error: observation.last_error,
            topology: observation
                .topology
                .map(|graph| topology::session_graph(graph).into()),
        }
    }

    /// Lower one runtime event.
    ///
    /// The two classifiers run first: whatever they recognize is a product
    /// fact the projection layer already decided, and this method only
    /// lowers it. What is left is either bridge-local state (the damage
    /// bookkeeping) or a fact with no Swift
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
            Event::AgentAskedState { terminal_id, asked } => {
                projected.push(WireEvent::AgentAskedState {
                    terminal_id: id::encode(&terminal_id),
                    asked,
                });
            }
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
            Event::AgentMetadata { terminal_id, value } => {
                projected.push(agent::badge(&terminal_id, value.as_deref()).into());
            }
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

#[cfg(test)]
mod tests {
    use super::*;
    use phux_protocol::wire::frame::{FrameKind, RESOURCE_AGENT_KEY, Scope};

    #[test]
    fn routed_constructor_preserves_route_and_credentials_in_runtime_target() {
        for (url, transport) in [
            (
                "quic://relay.example:443",
                Transport::Quic("relay.example:443".into()),
            ),
            (
                "wss://relay.example:443/phux",
                Transport::Ws("wss://relay.example:443/phux".into()),
            ),
        ] {
            let remote = RemoteClient::new_routed(
                url.into(),
                80,
                24,
                Some("pin".into()),
                Some("token".into()),
                " mini-route.example ".into(),
            )
            .unwrap();
            let target = remote.target().unwrap();
            assert_eq!(target.transport, transport);
            assert_eq!(
                target.tls_server_name.as_deref(),
                Some("mini-route.example")
            );
            assert_eq!(target.cert_fingerprint.as_deref(), Some("pin"));
            assert_eq!(target.token.as_deref(), Some("token"));
            assert_eq!(target.token_file, None);
            assert_eq!(
                target.authority.route.as_deref(),
                Some("mini-route.example")
            );
            assert!(
                target.authority.learner.is_none(),
                "relay leaf cannot authenticate the server CA"
            );
            assert_eq!(
                target.client_identity,
                phux_client_runtime::TlsClientIdentity::None
            );
            assert!(remote.runtime_client().is_none());

            let direct = RemoteClient::new(url.into(), 80, 24, None, None);
            assert_eq!(direct.target().unwrap().tls_server_name, None);

            remote.set_authority_pin(Some("server-ca".into()));
            remote.set_relay_route(Some("replacement-route".into()));
            let target = remote.target().unwrap();
            assert_eq!(target.authority.ca.as_deref(), Some("server-ca"));
            assert_eq!(target.authority.route.as_deref(), Some("replacement-route"));
            assert_eq!(target.tls_server_name.as_deref(), Some("replacement-route"));
        }
    }

    #[test]
    fn routed_constructor_rejects_malformed_dns_before_starting_runtime() {
        for route in [
            "",
            " ",
            "127.0.0.1",
            "::1",
            "bad_route",
            "-route",
            "route-",
            "route..example",
            "route.example.",
            "route:443",
            "route/path",
            "route?x=y",
            "route#fragment",
            "user@route",
            "routé.example",
        ] {
            let result = RemoteClient::new_routed(
                "quic://relay.example:443".into(),
                80,
                24,
                None,
                None,
                route.into(),
            );
            assert!(
                matches!(result, Err(WireError::Runtime { reason }) if reason.contains("TLS server name")),
                "{route:?}"
            );
        }
    }

    #[test]
    fn remote_endpoint_classification_selects_the_runtime_transport() {
        assert_eq!(
            classify_remote_endpoint("quic://mini.example:8788"),
            Ok(Transport::Quic("mini.example:8788".to_owned()))
        );
        assert_eq!(
            classify_remote_endpoint("quic://[fd7a:115c:a1e0::1]:8788"),
            Ok(Transport::Quic("[fd7a:115c:a1e0::1]:8788".to_owned()))
        );
        for url in ["ws://127.0.0.1:8787", "wss://mini.example:8787"] {
            assert_eq!(
                classify_remote_endpoint(url),
                Ok(Transport::Ws(url.to_owned()))
            );
        }
    }

    #[test]
    fn remote_endpoint_classification_rejects_unsupported_or_malformed_targets() {
        for endpoint in [
            "https://mini.example:8788",
            "quic://mini.example",
            "quic://mini.example:not-a-port",
            "quic://fd7a:115c:a1e0::1:8788",
            "quic://[fd7a:115c:a1e0::1]",
            "quic://user@mini.example:8788",
            "quic://mini.example:8788/path",
            "quic://mini.example:8788?mode=fast",
            "quic://mini.example:0",
        ] {
            let error = classify_remote_endpoint(endpoint).expect_err(endpoint);
            assert!(
                error.contains("HOST:PORT") || error.contains("must start"),
                "{endpoint}: {error}"
            );
        }

        let remote = RemoteClient::new("https://mini.example:8788".into(), 80, 24, None, None);
        let error = remote
            .connect()
            .expect_err("invalid scheme must not start a runtime");
        assert!(
            matches!(error, WireError::Runtime { reason } if reason.contains("invalid remote endpoint"))
        );
    }

    #[test]
    fn agent_declarations_lower_once_and_absence_clears_the_existing_badge() {
        let remote = RemoteClient::new("unused".into(), 80, 24, None, None);
        let terminal_id = ResourceId::local(7);
        let value = Some(br#"{"name":"agent","state":"blocked"}"#.to_vec());
        let mut projected = Vec::new();
        remote.project_event(
            Event::AgentMetadata {
                terminal_id: terminal_id.clone(),
                value: value.clone(),
            },
            &mut projected,
        );
        // Raw extension delivery remains available to the C ABI, but cannot
        // reintroduce duplicate or stale badge handling in this encoder.
        remote.project_event(
            Event::Frame(Box::new(FrameKind::MetadataChanged {
                scope: Scope::Resource(terminal_id.clone()),
                key: RESOURCE_AGENT_KEY.into(),
                value,
                actor: None,
            })),
            &mut projected,
        );
        remote.project_event(
            Event::AgentMetadata {
                terminal_id,
                value: None,
            },
            &mut projected,
        );
        assert_eq!(projected.len(), 2);
        assert!(
            matches!(&projected[0], WireEvent::AgentStateChanged { name, .. } if name == "agent")
        );
        assert!(
            matches!(&projected[1], WireEvent::AgentStateChanged { name, .. } if name.is_empty())
        );
    }

    #[test]
    fn a_publication_keeps_lifecycle_boundaries_in_order_and_take_events_omits_them() {
        let remote = RemoteClient::new("unused".into(), 80, 24, None, None);
        let terminal_id = ResourceId::local(7);
        let events = vec![
            Event::Bell {
                terminal_id: terminal_id.clone(),
            },
            Event::ConnectionLost {
                message: Some("reset".into()),
            },
            Event::ConnectionOpened {
                connection_epoch: 2,
            },
            Event::TopologyChanged,
            Event::AgentAskedState {
                terminal_id,
                asked: false,
            },
        ];
        let published = remote.publication(Observation {
            connection_epoch: 2,
            events: events.clone(),
            events_dropped: true,
            status: phux_client_runtime::control::Status::Attached,
            last_error: Some("reset".into()),
            topology: None,
        });
        assert_eq!(
            published,
            WirePublication {
                connection_epoch: 2,
                events: vec![
                    WireEvent::Bell {
                        terminal_id: "local:7".into(),
                    },
                    WireEvent::ConnectionLost {
                        message: Some("reset".into()),
                    },
                    WireEvent::ConnectionOpened {
                        connection_epoch: 2,
                    },
                    WireEvent::TopologyChanged,
                    WireEvent::AgentAskedState {
                        terminal_id: "local:7".into(),
                        asked: false,
                    },
                ],
                events_dropped: true,
                status: WireStatus::Attached,
                last_error: Some("reset".into()),
                topology: None,
            }
        );
        // The established drain keeps its vocabulary.
        let mut legacy = Vec::new();
        for event in events {
            remote.project_event(event, &mut legacy);
        }
        assert!(!legacy.iter().any(|event| matches!(
            event,
            WireEvent::ConnectionLost { .. } | WireEvent::ConnectionOpened { .. }
        )));
        assert_eq!(legacy.len(), 3);
    }

    #[test]
    fn an_unconnected_client_publishes_an_empty_first_incarnation() {
        let remote = RemoteClient::new("unused".into(), 80, 24, None, None);
        let published = remote.take_publication();
        assert_eq!(published.connection_epoch, 0);
        assert!(published.events.is_empty());
        assert!(!published.events_dropped);
        assert_eq!(published.status, WireStatus::Connecting);
        assert_eq!(published.topology, None);
    }

    #[test]
    fn asked_levels_lower_after_their_announcement_in_order() {
        let remote = RemoteClient::new("unused".into(), 80, 24, None, None);
        let terminal_id = ResourceId::local(7);
        let mut projected = Vec::new();
        for event in [
            Event::AgentAskedState {
                terminal_id: terminal_id.clone(),
                asked: true,
            },
            Event::AgentAsked {
                terminal_id: terminal_id.clone(),
                question_id: "q1".into(),
                text: "Proceed?".into(),
                suggestions: Vec::new(),
                waiting_seconds: None,
            },
            Event::AgentAskedState {
                terminal_id,
                asked: false,
            },
        ] {
            remote.project_event(event, &mut projected);
        }
        assert_eq!(
            projected,
            vec![
                WireEvent::AgentAskedState {
                    terminal_id: "local:7".into(),
                    asked: true,
                },
                WireEvent::AgentAsked {
                    terminal_id: "local:7".into(),
                    question_id: "q1".into(),
                    text: "Proceed?".into(),
                    suggestions: Vec::new(),
                    waiting_seconds: None,
                },
                WireEvent::AgentAskedState {
                    terminal_id: "local:7".into(),
                    asked: false,
                },
            ]
        );
    }
}
