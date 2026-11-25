//! Lifetime operations for the foreign client.

use super::*;

#[uniffi::export]
impl RemoteClient {
    /// A zero `cols` or `rows` casts no window-size vote (ADR-0145): attaches
    /// and spawns then carry no geometry, and the client sizes exactly the
    /// panes it shows with [`Self::resize_terminal`].
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
        *self.viewport.lock().unwrap() = viewport;
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

    /// Attach watch-only (ADR-0127): the server grants `VIEWER`, so input
    /// and resizes from this connection are refused rather than racing the
    /// primary. Takes effect at the next `connect`; the default declares no
    /// role. Taking the wheel later is `acquire_input(seize: true)` on an
    /// ordinary attach, not a role change.
    pub fn set_attach_viewer(&self, viewer: bool) {
        *self.attach_viewer.lock().unwrap() = viewer;
    }

    /// Pin the server's certificate authority (`sha256:` fingerprint,
    /// ADR-0153) beside the leaf fingerprint, or clear the pin. Takes effect
    /// at the next `connect`. A pinned server presenting another authority is
    /// refused with a "certificate authority changed" error that names both
    /// fingerprints, and the connection does not retry.
    pub fn set_authority_pin(&self, authority: Option<String>) {
        *self.authority.lock().unwrap() = authority.filter(|pin| !pin.trim().is_empty());
    }

    /// Enroll `key` as this device's workload identity with the single-use
    /// `ticket` a pairing link carried (`enroll=`), over this client's
    /// `quic://` endpoint and pins. Blocks for at most the dial timeout; call
    /// it off the main thread, before `connect`. Returns the issued chain
    /// (public PEM): store it beside the key and hand both to
    /// `set_device_identity` on later launches. The server's CA must be
    /// pinned first ([`Self::set_authority_pin`], the link's `ca`); an
    /// unpinned client is refused rather than learning it (ADR-0179).
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
            control: self.control_options(),
            connect: ConnectOptions::default(),
        };
        let client = Runtime::connect(target, options).map_err(|error| WireError::Runtime {
            reason: error.to_string(),
        })?;
        if let Some(listener) = self.listener.lock().unwrap().clone() {
            client.set_listener(Arc::new(ListenerProjection(listener)));
        }
        if let Some(theme) = *self.terminal_theme.lock().unwrap() {
            client.set_terminal_theme(Some(theme));
        }
        *slot = Some(client);
        Ok(())
    }

    /// Install (or clear, with `None`) the client-side terminal theme
    /// (ADR-0157): palette 0..=15 and the default foreground, background,
    /// and cursor every grid is resolved with. An application's OSC colour
    /// changes still win. Applies immediately when connected and to every
    /// later `connect`. Fails, changing nothing, unless `ansi16` holds
    /// exactly sixteen colours.
    pub fn set_terminal_theme(&self, theme: Option<WireTerminalTheme>) -> Result<(), WireError> {
        let theme = theme.map(TryInto::try_into).transpose()?;
        *self.terminal_theme.lock().unwrap() = theme;
        if let Some(client) = self.runtime_client() {
            client.set_terminal_theme(theme);
        }
        Ok(())
    }

    pub fn set_listener(&self, listener: Arc<dyn WireListener>) {
        *self.listener.lock().unwrap() = Some(listener.clone());
        if let Some(client) = self.runtime_client() {
            client.set_listener(Arc::new(ListenerProjection(listener)));
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
}
impl RemoteClient {
    /// The control options the next `connect` uses. The viewport is the
    /// constructor's or the last `resize_viewport`'s, unclamped: zero is the
    /// no-vote contract, not a size.
    pub(super) fn control_options(&self) -> phux_client_runtime::control::ControlOptions {
        phux_client_runtime::control::ControlOptions {
            client_name: "phux-mobile".to_owned(),
            viewport: *self.viewport.lock().unwrap(),
            scrollback_lines: SCROLLBACK_LINES,
            attach: None,
            attach_role: self.attach_role(),
            subscribe_agent_sessions: true,
            ..phux_client_runtime::control::ControlOptions::default()
        }
    }

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
            viewport: Mutex::new((cols, rows)),
            fingerprint,
            token,
            authority: Mutex::new(None),
            route: Mutex::new(tls_server_name),
            client_identity: Mutex::new(phux_client_runtime::TlsClientIdentity::None),
            attach_viewer: Mutex::new(false),
            terminal_theme: Mutex::new(None),
            client: Mutex::new(None),
            listener: Mutex::new(None),
            input_deliveries: Mutex::new(Vec::new()),
            generations: Mutex::new(HashMap::new()),
            receipts: Mutex::new(std::collections::VecDeque::new()),
            kills: Mutex::new(HashMap::new()),
            last_kill: std::sync::atomic::AtomicU64::new(0),
            document_input: Mutex::default(),
        })
    }
}
impl Drop for RemoteClient {
    fn drop(&mut self) {
        if let Some(client) = self.client.get_mut().unwrap().take() {
            client.close();
        }
    }
}
