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

mod agent_control;
mod approvals;
use agent_control::project_agent_control;
#[cfg(test)]
mod agent_records;
mod document_input;
mod endpoint;
use endpoint::classify_remote_endpoint;
mod organization;
mod publication;
#[cfg(test)]
mod quic_live;
mod runtime_access;
mod search;
mod selection;
mod service_http;
mod input;
mod presentation;
mod workflows;
mod lifetime;

const SCROLLBACK_LINES: u32 = 1000;

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
            // Mobile has no askpass; its ssh never prompts.
            ssh_askpass: None,
        })
    }

    /// The role every `ATTACH` on the next connection declares.
    fn attach_role(&self) -> Option<phux_protocol::wire::frame::RolePolicy> {
        self.attach_viewer
            .lock()
            .unwrap()
            .then_some(phux_protocol::wire::frame::RolePolicy::VIEWER)
    }

    /// The pin `connect` dials with: the stored authority. Nothing is
    /// learned from a connection (ADR-0179).
    fn authority_pin(&self) -> phux_client_runtime::target::AuthorityPin {
        phux_client_runtime::target::AuthorityPin {
            ca: self.authority.lock().unwrap().clone(),
            route: self.route.lock().unwrap().clone(),
        }
    }
}

#[derive(uniffi::Object)]
pub struct RemoteClient {
    url: String,
    /// `(cols, rows)`: one lock, so a resize never tears.
    viewport: Mutex<(u16, u16)>,
    fingerprint: Option<String>,
    token: Option<String>,
    /// The certificate-authority pin beside the leaf pin (ADR-0153), set
    /// before `connect` by [`RemoteClient::set_authority_pin`].
    authority: Mutex<Option<String>>,
    /// The relay route the server is reached through (ADR-0149), set by
    /// [`RemoteClient::set_relay_route`].
    route: Mutex<Option<String>>,
    /// The workload identity every dial presents (ADR-0154), once enrolled.
    client_identity: Mutex<phux_client_runtime::TlsClientIdentity>,
    /// Whether every `ATTACH` declares the watch-only role (ADR-0127), set
    /// by [`RemoteClient::set_attach_viewer`].
    attach_viewer: Mutex<bool>,
    /// The client-side terminal theme (ADR-0157), set by
    /// [`RemoteClient::set_terminal_theme`] and carried into every connect.
    terminal_theme: Mutex<Option<phux_client_runtime::engine::TerminalTheme>>,
    client: Mutex<Option<Client>>,
    listener: Mutex<Option<Arc<dyn WireListener>>>,
    input_deliveries: Mutex<Vec<WireInputDelivery>>,
    generations: Mutex<HashMap<ResourceId, u64>>,
    /// Committed organization receipts, newest last, kept for `undo`.
    receipts: Mutex<std::collections::VecDeque<phux_protocol::wire::frame::Committed>>,
    /// `kill_session` operations awaiting their outcome, by the id the call
    /// returned.
    kills: Mutex<HashMap<phux_protocol::ids::OperationId, u64>>,
    /// The last id `kill_session` returned.
    last_kill: std::sync::atomic::AtomicU64,
    document_input: Mutex<document_input::DocumentInputState>,
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
    pub fn take_events(&self) -> Vec<WireEvent> {
        let Some(client) = self.runtime_client() else {
            return Vec::new();
        };
        let mut projected = Vec::new();
        for event in client.take_events() {
            self.project_event(Some(&client), event, &mut projected);
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
                status: WireStatus::from(None),
                last_error: None,
                topology: None,
            };
        };
        self.publication(Some(&client), client.take_observation())
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
                    .map(Into::into)
                    .collect()
            })
            .unwrap_or_default()
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
                    .map(Into::into)
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
                    .map(Into::into)
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Run one checkout method on the checkout `terminal_id` is in, resolved
    /// by the server that owns the Terminal from its live working directory.
    /// Zero means nothing was sent: disconnected, the server lacks
    /// `checkout`, an unparseable id, a malformed or oversized request, or a
    /// full answer queue. Every non-zero request is answered exactly once in
    /// `take_checkout_answers`.
    pub fn checkout_query(&self, terminal_id: String, request: WireCheckoutRequest) -> u32 {
        self.with_terminal(&terminal_id, |client, id| {
            client.checkout(id.clone(), request.into())
        })
        .unwrap_or(0)
    }

    /// Drain correlated checkout answers, including disconnect cancellations.
    pub fn take_checkout_answers(&self) -> Vec<WireCheckoutAnswer> {
        self.runtime_client()
            .map(|client| {
                client
                    .take_checkout_answers()
                    .into_iter()
                    .map(Into::into)
                    .collect()
            })
            .unwrap_or_default()
    }


    /// Write one client-owned `Global` metadata key: the push grant
    /// `phux.push/v1/<device>` (ADR-0155), or any key the server stores as
    /// an ordinary value. Returns the request id, or 0 when offline (nothing
    /// is sent then). `SET_METADATA` has no success reply on the wire; a
    /// writer that needs confirmation reads the key back.
    pub fn set_global_metadata(&self, key: String, value: Vec<u8>) -> u32 {
        let Some(client) = self.runtime_client() else {
            return 0;
        };
        let request_id = client.next_request_id();
        client.queue_frame(&phux_protocol::wire::frame::FrameKind::SetMetadata {
            request_id,
            scope: phux_protocol::wire::frame::Scope::Global,
            key,
            value,
        });
        request_id
    }

    /// Delete one client-owned `Global` metadata key. Returns the request
    /// id, or 0 when offline.
    pub fn delete_global_metadata(&self, key: String) -> u32 {
        let Some(client) = self.runtime_client() else {
            return 0;
        };
        let request_id = client.next_request_id();
        client.queue_frame(&phux_protocol::wire::frame::FrameKind::DeleteMetadata {
            request_id,
            scope: phux_protocol::wire::frame::Scope::Global,
            key,
        });
        request_id
    }

    /// Subscribe to one `Global` metadata key (`SUBSCRIBE_METADATA`) and
    /// read its current value (`GET_METADATA`). Idempotent; the runtime also
    /// subscribes and reads it again on every later attach. Values arrive
    /// through `take_global_metadata`. Returns the read's request id, or 0
    /// when nothing was sent (offline, an empty key, or more than 32 keys).
    pub fn watch_global_metadata(&self, key: String) -> u32 {
        self.runtime_client()
            .map_or(0, |client| client.watch_global_metadata(key))
    }

    /// Drain the watched `Global` keys' values observed since the last
    /// call, read replies and live changes alike, oldest first and newest
    /// value per key. `value` is `None` when the key is absent or deleted.
    pub fn take_global_metadata(&self) -> Vec<WireGlobalMetadata> {
        self.runtime_client()
            .map(|client| {
                client
                    .take_global_metadata()
                    .into_iter()
                    .map(Into::into)
                    .collect()
            })
            .unwrap_or_default()
    }


    /// Resize one pane exactly, without moving the client's viewport vote
    /// (`resize_viewport`). The outcome is local: `Queued` means one
    /// `RESIZE_TERMINAL` left; the server's own geometry arrives in frames.
    pub fn resize_terminal(&self, terminal_id: String, cols: u16, rows: u16) -> WireResizeOutcome {
        self.with_terminal(&terminal_id, |client, id| {
            client
                .resize_terminal(id, u32::from(cols), u32::from(rows))
                .into()
        })
        .unwrap_or(WireResizeOutcome::Unavailable)
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


    pub fn input_ready(&self, terminal_id: String) -> bool {
        self.with_terminal(&terminal_id, Client::input_ready)
            .unwrap_or(false)
    }

    pub fn refresh_topology(&self) {
        if let Some(client) = self.runtime_client() {
            let _ = client.refresh_topology();
        }
    }


    pub fn status(&self) -> WireStatus {
        WireStatus::from(self.runtime_client().map(|client| client.status()))
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

    /// The session graph with the organization replica's records and held
    /// arrangements.
    pub fn topology(&self) -> Option<SessionTopology> {
        let client = self.runtime_client()?;
        let replica = client.organization();
        client
            .topology()
            .map(|graph| session_topology(graph, Some(&replica)))
    }

}



impl RemoteClient {

    /// Lower one runtime event. What [`lower_event`] does not name is a
    /// runtime-owned fact with no Swift vocabulary.
    fn project_event(&self, client: Option<&Client>, event: Event, projected: &mut Vec<WireEvent>) {
        if Self::project_approval(&event, projected) {
            return;
        }
        let event = match lower_event(event) {
            Ok(lowered) => return projected.extend(lowered),
            Err(event) => event,
        };
        let Some(event) = project_agent_control(event, projected) else {
            return;
        };
        let Some(event) = self.project_document(client, event, projected) else {
            return;
        };
        match event {
            Event::TerminalCreation { request_id, outcome } => {
                projected.push(WireEvent::TerminalCreation { request_id, outcome: outcome.into() });
            }
            Event::TopologyChanged => {
                projected.push(WireEvent::TopologyChanged);
            }
            Event::InputDelivery {
                delivery_id,
                outcome: result,
                code,
                message,
            } => self
                .input_deliveries
                .lock()
                .unwrap()
                .push(WireInputDelivery {
                    delivery_id,
                    outcome: result.into(),
                    code,
                    message,
                }),
            Event::AgentMetadata { terminal_id, value } => {
                projected.push(agent::badge(&terminal_id, value.as_deref()).into());
            }
            Event::TerminalsClosed { request_id, error } => {
                projected.push(WireEvent::TerminalsClosed { request_id, error });
            }
            event @ (Event::OrganizationChanged
            | Event::OrganizationUnavailable { .. }
            | Event::OrganizationOperation { .. }) => {
                projected.extend(self.organization_event(event));
            }
            _ => {}
        }
    }
}


#[cfg(test)]
mod tests;
