//! Desktop NAPI encoder (ADR-0135).
//!
//! `DesktopClient` owns one registry identity. `connect` starts one runtime
//! session; reconnect stays inside that session. `close`, GC, and environment
//! cleanup invalidate the identity and close the same native Client. String
//! handles and 64-bit correlations never pass through a JS number.
//! Explicit `close` returns the final event batch, including all undrained
//! receipts and the outcomes settled by runtime shutdown. Consume that batch
//! before discarding the JS owner; native handle lookups are already stale.
//! Cleanup registration is per environment, not per disposed client.
//!
//! One runtime listener queues one coalesced JS wake. The JS owner calls
//! `takeEvents` once per wake (including empty wakes) to rearm it, then reads
//! status/topology and schedules native paints. A queued wake can arrive after
//! close: its handle is stale and must be ignored. No callbacks run on the
//! runtime thread; the TSFN is weak and cannot keep the environment alive.
//!
//! The native painter uses `initialize().client(handle)` in the SAME loaded
//! host. It must not replace the listener or drain events. No cells cross JS.
//!
//! `attachTerminal`/`detachTerminal` are resource subscription commands, not
//! view constructors. `createView` allocates a runtime ViewId over the same
//! terminal; scrolling, selection and search belong to that view. Normalized
//! input validates current engine ownership and readiness during admission.
//! Focused geometry, native painting and duplicate-view UX require additional
//! desktop integration; a ResourceId alone never identifies a presentation.

mod connection;
mod dto;
mod input;
mod lifecycle;
mod notification;
mod registry;
mod views;

pub use connection::{DesktopBootstrapProfile, DesktopConnectionIdentity, DesktopServerInfo};
pub use dto::*;
pub use input::{DesktopKeyEvent, DesktopMouseEvent};
pub use lifecycle::{DesktopEnvironmentEntry, DesktopInitialSize, DesktopSpawnOptions};
pub use registry::{Registry, RegistryError, initialize};
pub use views::{
    DesktopDocumentPoint, DesktopGestureResult, DesktopResizeOutcome, DesktopSearchMatch,
    DesktopSelectionGesture, DesktopViewInfo,
};

use ::napi::bindgen_prelude::Function;
use ::napi::{Env, Error, Result};
use napi_derive::napi;
use phux_client_runtime::control::ControlOptions;
use phux_client_runtime::{Client, ClientOptions, Target};
use phux_protocol::ResourceId;
use phux_protocol::wire::frame::{AttachTarget, FrameKind, RESOURCE_AGENT_KEY, Scope};

use crate::projection::{id, status, topology};

fn registry_error(error: RegistryError) -> Error {
    Error::from_reason(error.to_string())
}

fn terminal_id(value: &str) -> Result<ResourceId> {
    id::parse(value).ok_or_else(|| Error::from_reason("InvalidResourceId"))
}

/// Initial local-server connection configuration. Dimensions are the runtime
/// connection's initial viewport, not an independently resizable view.
#[napi(object)]
#[derive(Debug)]
pub struct DesktopConnectOptions {
    pub socket_path: String,
    pub cols: f64,
    pub rows: f64,
    /// Existing home session to attach during negotiation; absent browses.
    pub session_name: Option<String>,
    /// Observe-only attach intent. This never requests lease takeover.
    pub observer: Option<bool>,
}

impl DesktopConnectOptions {
    fn runtime_options(self) -> Result<(Target, ClientOptions)> {
        if self.socket_path.is_empty() {
            return Err(Error::from_reason("InvalidConnectOptions"));
        }
        let cols = dimension(self.cols)?;
        let rows = dimension(self.rows)?;
        let attach = self.session_name.map(existing_session).transpose()?;
        Ok((
            Target::uds(self.socket_path),
            ClientOptions {
                control: ControlOptions {
                    client_name: "phux-desktop".to_owned(),
                    viewport: (cols, rows),
                    attach,
                    attach_role: Some(if self.observer.unwrap_or(false) {
                        phux_protocol::wire::frame::RolePolicy::VIEWER
                    } else {
                        phux_protocol::wire::frame::RolePolicy::PRIMARY
                    }),
                    auto_attach_foreign_spawns: false,
                    ..ControlOptions::default()
                },
                ..ClientOptions::default()
            },
        ))
    }
}

fn agent_watches() -> &'static std::sync::Mutex<std::collections::HashMap<u32, ResourceId>> {
    static WATCHES: std::sync::OnceLock<
        std::sync::Mutex<std::collections::HashMap<u32, ResourceId>>,
    > = std::sync::OnceLock::new();
    WATCHES.get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()))
}

pub(super) fn remember_agent_watch(request_id: u32, terminal: ResourceId) {
    if let Ok(mut watches) = agent_watches().lock() {
        watches.insert(request_id, terminal);
    }
}

pub(super) fn take_agent_watch(request_id: u32) -> Option<ResourceId> {
    agent_watches().lock().ok()?.remove(&request_id)
}

fn existing_session(name: String) -> Result<AttachTarget> {
    if name.is_empty() || name.len() > 4096 || name.contains('\0') {
        return Err(Error::from_reason("InvalidSessionName"));
    }
    Ok(AttachTarget::ByName(name))
}

fn protocol_integer(value: f64, max: u32, error: &str) -> Result<u32> {
    if !value.is_finite() || value.fract() != 0.0 || !(0.0..=f64::from(max)).contains(&value) {
        return Err(Error::from_reason(error));
    }
    // The original JS number is finite, integral, nonnegative, and bounded
    // above by a u32. Validate before narrowing, never through NAPI's ToUint32.
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "range and integer checks above"
    )]
    Ok(value as u32)
}

fn dimension(value: f64) -> Result<u16> {
    let value = protocol_integer(value, u32::from(u16::MAX), "InvalidConnectOptions")?;
    if value == 0 {
        return Err(Error::from_reason("InvalidConnectOptions"));
    }
    u16::try_from(value).map_err(|_| Error::from_reason("InvalidConnectOptions"))
}

#[napi]
#[derive(Debug)]
pub struct DesktopClient {
    handle: String,
}

#[napi]
#[allow(
    clippy::needless_pass_by_value,
    reason = "NAPI requires owned JS primitives and function values"
)]
impl DesktopClient {
    #[napi(constructor)]
    pub fn new(env: Env) -> Result<Self> {
        let key = env.raw() as usize;
        initialize().register_environment(key, || {
            env.add_env_cleanup_hook(key, |key| {
                let _ = initialize().close_environment(key);
            })
            .map(|_| ())
        })?;
        let handle = initialize().create(Some(key)).map_err(registry_error)?;
        Ok(Self { handle })
    }

    #[napi(getter)]
    #[must_use]
    pub fn handle(&self) -> String {
        self.handle.clone()
    }

    /// Register the sole notification owner and begin dialing. Success means
    /// the runtime started; observe status/events for negotiation or failure.
    #[napi]
    pub fn connect(
        &self,
        options: DesktopConnectOptions,
        on_activity: Function<'_, String, ()>,
    ) -> Result<()> {
        let (target, options) = options.runtime_options()?;
        let session = initialize().session(&self.handle).map_err(registry_error)?;
        if session.client().is_ok() {
            return Err(registry_error(RegistryError::AlreadyConnected));
        }
        session.wake.install(self.handle.clone(), &on_activity)?;
        let result = session.connect(target, options);
        if result.is_err() {
            session.wake.close();
        }
        result
    }

    #[napi]
    /// Invalidate the native handle immediately and return the final event
    /// batch, including queued receipts and unresolved-input outcomes. Process
    /// this batch exactly like `takeEvents`; no later drain is permitted.
    pub fn close(&self) -> Result<Vec<DesktopEvent>> {
        Ok(initialize()
            .close(&self.handle)
            .map_err(registry_error)?
            .into_iter()
            .filter_map(dto::encode_event)
            .collect())
    }

    #[napi]
    pub fn status(&self) -> Result<DesktopStatus> {
        Ok(status::connection(Some(self.client()?.status())).into())
    }

    #[napi]
    pub fn last_error(&self) -> Result<Option<String>> {
        Ok(self.client()?.last_error())
    }

    #[napi]
    pub fn connection_epoch(&self) -> Result<String> {
        Ok(self.client()?.connection_epoch().to_string())
    }

    #[napi]
    pub fn topology(&self) -> Result<Option<DesktopTopology>> {
        Ok(self
            .client()?
            .topology()
            .map(topology::session_graph)
            .map(Into::into))
    }

    /// The sole event drain. Every callback, even an empty one, needs a drain
    /// to rearm notifications. Returned order is runtime event order.
    #[napi]
    pub fn take_events(&self) -> Result<Vec<DesktopEvent>> {
        Ok(self
            .client()?
            .take_events()
            .into_iter()
            .filter_map(dto::encode_event)
            .collect())
    }

    #[napi]
    pub fn refresh_topology(&self) -> Result<Option<u32>> {
        Ok(self.client()?.refresh_topology())
    }

    /// Subscribe to this terminal's agent badge. Empty name means no declared agent.
    #[napi]
    pub fn watch_agent(&self, terminal: String) -> Result<()> {
        let id = terminal_id(&terminal)?;
        let client = self.client()?;
        let request_id = client.next_request_id();
        remember_agent_watch(request_id, id.clone());
        let key = RESOURCE_AGENT_KEY.to_owned();
        client.queue_frame(&FrameKind::GetMetadata {
            request_id,
            scope: Scope::Resource(id.clone()),
            key: key.clone(),
        });
        client.queue_frame(&FrameKind::SubscribeMetadata {
            scope: Scope::Resource(id),
            key,
        });
        Ok(())
    }

    /// Attach an existing named server session; does not create it implicitly.
    #[napi]
    pub fn attach_session(&self, name: String) -> Result<()> {
        self.client()?.attach_session(AttachTarget::ByName(name));
        Ok(())
    }

    /// Convenience for immediate commands. Delayed UI actions must retain the
    /// reviewed identity and use `spawnTerminalWithOptions` instead.
    #[napi]
    pub fn spawn_terminal(&self, session_id: f64) -> Result<u32> {
        protocol_integer(session_id, u32::MAX, "InvalidSessionId")?;
        let info = self
            .server_info()?
            .ok_or_else(|| Error::from_reason("NotNegotiated"))?;
        self.spawn_terminal_with_options(DesktopSpawnOptions {
            identity: DesktopConnectionIdentity {
                server_id: info.server_id,
                connection_epoch: info.connection_epoch,
            },
            session_id,
            command: None,
            cwd: None,
            env: None,
            initial_size: None,
        })
    }

    #[napi]
    pub fn attach_terminal(&self, resource_id: String) -> Result<u32> {
        Ok(self.client()?.attach_terminal(&terminal_id(&resource_id)?))
    }

    #[napi]
    pub fn detach_terminal(&self, resource_id: String) -> Result<u32> {
        Ok(self.client()?.detach_terminal(&terminal_id(&resource_id)?))
    }

    #[napi]
    pub fn input_readiness(&self, resource_id: String) -> Result<DesktopInputReadiness> {
        let client = self.client()?;
        let id = terminal_id(&resource_id)?;
        Ok(DesktopInputReadiness {
            ready: client.input_ready(&id),
            delivery_fenced: client.delivery_fenced(&id),
        })
    }

    /// Queues one acknowledged untrusted paste. The returned string correlates
    /// `InputDelivery`; it is NOT confirmation of delivery. Never auto-retry an
    /// Unknown result. The native painter acknowledges fresh projections.
    #[napi]
    pub fn apply_paste(&self, resource_id: String, text: String) -> Result<String> {
        let client = self.client()?;
        let delivery = id::parse(&resource_id).map_or_else(
            || client.refuse_acknowledged_input("invalid terminal id"),
            |id| client.apply_paste(&id, &text),
        );
        Ok(delivery.to_string())
    }
}

impl DesktopClient {
    fn client(&self) -> Result<Client> {
        initialize().client(&self.handle).map_err(registry_error)
    }
}

impl Drop for DesktopClient {
    fn drop(&mut self) {
        let _ = initialize().close(&self.handle);
    }
}
