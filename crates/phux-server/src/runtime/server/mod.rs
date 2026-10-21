//! The public runtime builder; execution lives in its private serve module.

use crate::runtime::config::{RuntimeFlags, ServerConfig, ServerError};
use crate::runtime::upgrade;
use phux_protocol::wire::RemoteListenersReport;
use std::future::Future;
use std::net::SocketAddr;
use std::os::fd::RawFd;
use tokio::runtime::Builder;

#[cfg(not(feature = "webtransport"))]
use tracing::warn;

mod serve;

/// Server runtime owning the listener loop and per-client task scaffolding.
#[derive(Debug)]
pub struct ServerRuntime {
    cfg: ServerConfig,
    /// WebSocket listen address; `None` falls back to [`crate::runtime::ServerEnv::ws_addr`].
    ws_addr: Option<SocketAddr>,
    /// QUIC listen address; `None` falls back to [`crate::runtime::ServerEnv::quic_addr`].
    quic_addr: Option<SocketAddr>,
    /// WebTransport listen address; `None` falls back to
    /// [`crate::runtime::ServerEnv::wt_addr`].
    #[cfg(feature = "webtransport")]
    wt_addr: Option<SocketAddr>,
    /// Inherited handoff-blob descriptor for `--resume` (ADR-0032).
    resume_fd: Option<RawFd>,
    /// Upgrade handoff taken from the environment by [`Self::resume`].
    inherited_upgrade: upgrade::InheritedUpgradeEnv,
    /// Federation-hub mode; only a hub reads [`Self::satellites`].
    hub: bool,
    /// `[[satellites]]` registry, validated at startup in hub mode.
    satellites: Vec<phux_config::SatelliteConfigEntry>,
    /// Where a hub re-reads `[[satellites]]` on a config-reload doorbell.
    satellite_source: Option<crate::hub::SatelliteSource>,
    /// Outbound relay connector entries.
    connectors: Vec<phux_config::ConnectorConfigEntry>,
    /// Raw `--connect` value, kept for the upgrade argv.
    connect_override: Option<String>,
    /// Hubs this server dials out to as a satellite (ADR-0182).
    uplinks: Vec<phux_config::UplinkConfigEntry>,
    /// Where a config-reload doorbell re-reads `[[uplink]]`.
    uplink_source: Option<crate::uplink::UplinkSource>,
    /// Overlay-address source for the auto-bound listener (ADR-0081). Never
    /// called on the startup path; see [`crate::runtime::listeners::serve_auto_overlay_listeners`].
    overlay_detect: fn() -> Vec<std::net::IpAddr>,
    /// `--autosave`: the crash-safe workspace archive (ADR-0150).
    autosave: Option<crate::autosave::Autosave>,
    /// Told once the startup listeners are bound; see [`Self::on_listening`].
    on_listening: ListeningHook,
}

/// The callback behind [`ServerRuntime::on_listening`].
pub(in crate::runtime) type ListeningCallback = Box<dyn FnOnce(&RemoteListenersReport) + Send>;

/// An optional [`ListeningCallback`]; a newtype so [`ServerRuntime`] keeps
/// its `Debug`.
pub(in crate::runtime) struct ListeningHook(Option<ListeningCallback>);

impl std::fmt::Debug for ListeningHook {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(if self.0.is_some() {
            "ListeningHook(Some)"
        } else {
            "ListeningHook(None)"
        })
    }
}

impl ServerRuntime {
    /// Create a runtime ready to be `run`. Does not perform I/O.
    #[must_use]
    pub const fn new(cfg: ServerConfig) -> Self {
        Self {
            cfg,
            ws_addr: None,
            quic_addr: None,
            #[cfg(feature = "webtransport")]
            wt_addr: None,
            resume_fd: None,
            inherited_upgrade: upgrade::InheritedUpgradeEnv::none(),
            hub: false,
            satellites: Vec::new(),
            satellite_source: None,
            connectors: Vec::new(),
            connect_override: None,
            uplinks: Vec::new(),
            uplink_source: None,
            overlay_detect: phux_config::overlay::detect,
            autosave: None,
            on_listening: ListeningHook(None),
        }
    }

    /// Call `hook` once the UDS socket and every configured remote listener
    /// have bound (each remote one may have failed; the report says which),
    /// before the session tree is seeded. A startup that fails, a socket bind
    /// above all, never calls it, so a "listening" line printed from here is
    /// never a lie. The auto-bound overlay listener (ADR-0081) binds later
    /// and is not in the report.
    #[must_use]
    pub fn on_listening(
        mut self,
        hook: impl FnOnce(&RemoteListenersReport) + Send + 'static,
    ) -> Self {
        self.on_listening = ListeningHook(Some(Box::new(hook)));
        self
    }

    /// Keep a crash-safe workspace archive (ADR-0150): restore it once on a
    /// cold start, then rewrite it atomically as the workspace changes.
    #[must_use]
    pub fn autosave(mut self, autosave: crate::autosave::Autosave) -> Self {
        self.autosave = Some(autosave);
        self
    }

    /// Override the overlay-address source for the auto-bound listener
    /// (ADR-0081), so tests control detection timing
    /// (`tests/lifecycle/overlay_startup.rs`).
    #[must_use]
    pub const fn overlay_detect(mut self, detect: fn() -> Vec<std::net::IpAddr>) -> Self {
        self.overlay_detect = detect;
        self
    }

    /// Resume from a graceful upgrade (ADR-0032): read the handoff state blob
    /// from inherited descriptor `fd`, adopt the inherited listener, and
    /// rebuild the session tree rather than starting fresh.
    ///
    /// Also consumes the `PHUX_UPGRADE_*` handoff variables and removes them
    /// from the process environment, so call it before starting threads.
    #[must_use]
    pub fn resume(mut self, fd: RawFd) -> Self {
        self.resume_fd = Some(fd);
        self.inherited_upgrade = upgrade::InheritedUpgradeEnv::take_from_env();
        self
    }

    /// A cold start: honor no inherited upgrade handoff, and remove any
    /// `PHUX_UPGRADE_*` variables from the process environment so hooks and
    /// every other child of this server cannot inherit them either. The
    /// counterpart of [`Self::resume`]; call it at the same point, before
    /// starting threads.
    #[must_use]
    pub fn discard_inherited_upgrade(self) -> Self {
        let _ignored = upgrade::InheritedUpgradeEnv::take_from_env();
        self
    }

    /// Also accept WebSocket connections on `addr`, overriding
    /// `PHUX_WS_ADDR`. Loopback is plaintext; a routable address gets TLS and
    /// bearer auth (ADR-0031).
    #[must_use]
    pub const fn listen_ws(mut self, addr: SocketAddr) -> Self {
        self.ws_addr = Some(addr);
        self
    }

    /// Also accept QUIC connections on `addr`, overriding `PHUX_QUIC_ADDR`.
    /// A routable address requires a paired bearer token (ADR-0031).
    #[must_use]
    pub const fn listen_quic(mut self, addr: SocketAddr) -> Self {
        self.quic_addr = Some(addr);
        self
    }

    /// Also accept WebTransport connections on `addr`, overriding
    /// `PHUX_WT_ADDR`. A routable address requires a paired bearer token in
    /// the `CONNECT` request (ADR-0031).
    #[cfg(feature = "webtransport")]
    #[must_use]
    pub const fn listen_webtransport(mut self, addr: SocketAddr) -> Self {
        self.wt_addr = Some(addr);
        self
    }

    /// Built without the `webtransport` feature: the listen address is
    /// ignored (with a warning) so callers keep one call site.
    #[cfg(not(feature = "webtransport"))]
    #[must_use]
    pub fn listen_webtransport(self, addr: SocketAddr) -> Self {
        warn!(
            %addr,
            "phux-server was built without the `webtransport` feature; ignoring the WebTransport listen address"
        );
        self
    }

    /// Run as a federation hub (ADR-0007), validating `satellites` into the
    /// [`crate::hub::HubTable`] at startup; a bad entry fails with
    /// [`ServerError::Hub`].
    #[must_use]
    pub fn hub(mut self, satellites: Vec<phux_config::SatelliteConfigEntry>) -> Self {
        self.hub = true;
        self.satellites = satellites;
        self
    }

    /// On each `phux.config.reload/v1` doorbell, re-read `[[satellites]]`
    /// from `source`. A hub dials added entries, stops removed ones, and
    /// redials changed ones. A server that did not boot as a hub starts
    /// those links the first time the registry is non-empty. Local panes
    /// stay up either way.
    #[must_use]
    pub fn hub_reload(mut self, source: crate::hub::SatelliteSource) -> Self {
        self.satellite_source = Some(source);
        self
    }

    /// Supervise outbound relay connector entries. `connect_override` is the
    /// raw `--connect` value, kept for the upgrade argv.
    #[must_use]
    pub fn connectors(
        mut self,
        entries: Vec<phux_config::ConnectorConfigEntry>,
        connect_override: Option<String>,
    ) -> Self {
        self.connectors = entries;
        self.connect_override = connect_override;
        self
    }

    /// Keep an uplink to each enabled hub in `entries`, serving it as a
    /// satellite (ADR-0182). A malformed entry fails startup with
    /// [`ServerError::Uplink`].
    #[must_use]
    pub fn uplinks(mut self, entries: Vec<phux_config::UplinkConfigEntry>) -> Self {
        self.uplinks = entries;
        self
    }

    /// On each `phux.config.reload/v1` doorbell, re-read `[[uplink]]` from
    /// `source` and start, stop, or redial exactly the uplinks that changed,
    /// so `phux host join` needs no restart.
    #[must_use]
    pub fn uplink_reload(mut self, source: crate::uplink::UplinkSource) -> Self {
        self.uplink_source = Some(source);
        self
    }

    /// Build a current-thread runtime and block on [`Self::run_async`].
    pub fn run<F>(self, shutdown: F) -> Result<(), ServerError>
    where
        F: Future<Output = ()> + Send + 'static,
    {
        let rt = Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(ServerError::Runtime)?;
        rt.block_on(self.run_async(shutdown))
    }

    fn runtime_flags(&self) -> RuntimeFlags {
        RuntimeFlags {
            ws_addr: self.ws_addr,
            quic_addr: self.quic_addr,
            #[cfg(feature = "webtransport")]
            wt_addr: self.wt_addr,
            #[cfg(not(feature = "webtransport"))]
            wt_addr: None,
            hub: self.hub,
            connect: self.connect_override.clone(),
            exit_after_idle: self.cfg.exit_after_idle,
            upgrade_source_exe: self.inherited_upgrade.source_exe.clone(),
            autosave: self
                .autosave
                .as_ref()
                .map(|autosave| autosave.path().to_path_buf()),
        }
    }
}
