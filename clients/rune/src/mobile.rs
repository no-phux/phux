//! Safe application interface for a native phone host. UIKit owns its
//! window, input services and lifecycle; Rune owns the same Desk and view
//! as the desktop. The small iOS bridge crate owns the C ABI's unsafe code.

use phux_client_runtime::Target;
use serde::Deserialize;
use serde_json::{Value, json};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

pub use crate::desk::Desk as State;
pub use crate::desk::native_input::{Action, Bridge as Input, Operation};
pub use phux_client_runtime::TlsClientIdentity;
pub use phux_client_runtime::enroll::{DeviceSigner, enroll_target_blocking, held_identity};
pub use rune;

/// Native-only state, absent from the desktop-hosted `--phone` form.
pub struct Platform {
    pub(crate) target: Target,
    pub connection_requested: bool,
    /// A command asked for the system pasteboard, which only the host reads.
    pub paste_requested: bool,
    pub active: bool,
    /// When the app left the foreground. Wall time: a monotonic clock may
    /// stand still while the device sleeps.
    pub away_since: Option<SystemTime>,
}

impl Platform {
    /// Note a lifecycle edge at `now`. Coming back, how long the app was
    /// away, when it had left.
    fn set_active(&mut self, active: bool, now: SystemTime) -> Option<Duration> {
        self.active = active;
        if !active {
            // Resigning and then backgrounding is one absence, from the first.
            self.away_since.get_or_insert(now);
            return None;
        }
        // A clock set back while away measures nothing: only probe then.
        let since = self.away_since.take()?;
        Some(now.duration_since(since).unwrap_or_default())
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Configuration {
    pub endpoint: String,
    /// What the person calls this computer, from pairing. Only words: the
    /// endpoint is what gets dialed.
    #[serde(default)]
    pub name: Option<String>,
    /// Relay SNI; also scopes the end-to-end authority handshake to its route.
    #[serde(default)]
    pub tls_server_name: Option<String>,
    #[serde(default)]
    pub certificate_pin: Option<String>,
    #[serde(default)]
    pub token: Option<String>,
    #[serde(default)]
    pub session: Option<String>,
    #[serde(default)]
    pub authority_ca: Option<String>,
    #[serde(default)]
    pub chain_pem: Option<String>,
    /// Public Keychain tag identifying the key this chain was issued to.
    #[serde(default)]
    pub key_id: Option<String>,
}

impl Configuration {
    /// Never learn trust from a connection. Off loopback, a host needs a
    /// pin and token supplied by pairing, just like the shared dialer.
    pub fn target(&self) -> Result<Target, String> {
        let url = parse_endpoint(&self.endpoint)?;
        let loopback = is_loopback(&url)?;
        let mut target = endpoint_target(&url, loopback)?;
        self.require_credentials(loopback)?;
        target.cert_fingerprint = self.certificate_pin.as_deref().map(compact_pin);
        target.token = self.token.clone();
        target.authority.ca = self.authority_ca.as_deref().map(|pin| {
            format!(
                "sha256:{}",
                compact_pin(pin.strip_prefix("sha256:").unwrap_or(pin))
            )
        });
        self.validate_credential_shapes()?;
        self.apply_relay_route(&mut target)?;
        if let Some(name) = self.display_name() {
            target.name = name.to_owned();
        }
        Ok(target)
    }

    /// The computer's name when pairing gave one fit to show.
    fn display_name(&self) -> Option<&str> {
        let name = self.name.as_deref()?.trim();
        (!name.is_empty() && !name.chars().any(char::is_control)).then_some(name)
    }

    fn apply_relay_route(&self, target: &mut Target) -> Result<(), String> {
        let Some(name) = &self.tls_server_name else {
            return Ok(());
        };
        if !matches!(target.transport, phux_client_runtime::Transport::Quic(_)) {
            return Err("A relay route needs a quic:// endpoint.".into());
        }
        let route = phux_client_runtime::target::validate_tls_server_name(name)
            .map_err(|_| "The relay route must be a DNS server name.")?;
        target.tls_server_name = Some(route.clone());
        target.authority.route = Some(route);
        Ok(())
    }

    pub fn enrollment_target(&self) -> Result<Target, String> {
        let target = self.target()?;
        if !matches!(target.transport, phux_client_runtime::Transport::Quic(_)) {
            return Err("Enrollment needs a quic:// endpoint.".into());
        }
        if self.authority_ca.as_deref().is_none_or(str::is_empty) {
            return Err("Enrollment needs the certificate authority pin from pairing.".into());
        }
        Ok(target)
    }

    /// Validate before saving. Parsing the stored PEM and attaching the held
    /// signer happens in `state_with_identity`, before any connection starts.
    pub fn validate_connection(&self) -> Result<(), String> {
        self.target()?;
        let url = parse_endpoint(&self.endpoint)?;
        if !is_loopback(&url)? && self.chain_pem.as_deref().is_none_or(str::is_empty) {
            return Err("Enroll this phone before connecting to a remote host.".into());
        }
        if self.chain_pem.is_some() && self.key_id.as_deref().is_none_or(str::is_empty) {
            return Err("The enrolled device key is missing. Enroll this phone again.".into());
        }
        Ok(())
    }

    pub fn target_with_identity(
        &self,
        signer: Option<Arc<dyn DeviceSigner>>,
    ) -> Result<Target, String> {
        self.validate_connection()?;
        let mut target = self.target()?;
        if let Some(chain) = &self.chain_pem {
            let signer = signer.ok_or("The platform-held device key is unavailable.")?;
            target.client_identity = TlsClientIdentity::Held(held_identity(chain, signer)?);
        }
        Ok(target)
    }

    fn validate_credential_shapes(&self) -> Result<(), String> {
        if let Some(pin) = &self.certificate_pin {
            validate_pin(pin)?;
        }
        if let Some(pin) = &self.authority_ca {
            validate_pin(pin.strip_prefix("sha256:").unwrap_or(pin))?;
        }
        if let Some(token) = self.token.as_deref()
            && (token.len() != 64 || !token.bytes().all(|b| b.is_ascii_hexdigit()))
        {
            return Err("The pairing token must contain 64 hexadecimal digits.".into());
        }
        Ok(())
    }

    fn require_credentials(&self, loopback: bool) -> Result<(), String> {
        if !loopback
            && (self.certificate_pin.as_deref().is_none_or(str::is_empty)
                || self.token.as_deref().is_none_or(str::is_empty))
        {
            return Err("A remote host needs its pairing certificate pin and token.".into());
        }
        Ok(())
    }
}

/// Import one complete enrollment link without merging stored credentials.
/// Returns configuration plus ephemeral ticket and public confirmation labels.
/// No connection or enrollment starts until the native host obtains consent.
pub fn parse_pairing_invite(raw: &str) -> Result<Value, String> {
    validate_invite_input(raw)?;
    let normalized;
    let link = if let Some(query) = raw.strip_prefix("phux-rune://connect?") {
        normalized = format!("https://phux.sh/connect?{query}");
        normalized.as_str()
    } else {
        raw
    };
    let invite = phux_client_runtime::pairing::parse_connect_link(link)?;
    let configuration = invite_configuration(&invite)?;
    let ticket = invite
        .enrollment_ticket
        .as_deref()
        .ok_or("Pairing needs the single-use enrollment ticket. Generate a new phone QR.")?;
    if !is_hex_secret(ticket) {
        return Err("The enrollment ticket must contain 64 hexadecimal digits.".into());
    }
    let endpoint = configuration["endpoint"]
        .as_str()
        .ok_or("Invalid pairing endpoint.")?;
    let peer = invite.tls_server_name.as_ref().map_or_else(
        || endpoint.to_owned(),
        |route| format!("{route} via {endpoint}"),
    );
    Ok(json!({"configuration": configuration, "ticket": ticket,
        "name": invite.name, "peer": peer}))
}

fn validate_invite_input(raw: &str) -> Result<(), String> {
    if raw.len() > phux_client_runtime::pairing::MAX_CONNECT_LINK_BYTES {
        return Err("Pairing links must not exceed 16 KiB.".into());
    }
    if raw.chars().any(char::is_control) {
        return Err("Pairing links must not contain control characters.".into());
    }
    Ok(())
}

fn invite_configuration(
    invite: &phux_client_runtime::pairing::ConnectLink,
) -> Result<Value, String> {
    let endpoint = invite
        .quic
        .as_deref()
        .ok_or("Phone pairing needs a quic:// endpoint. Generate a new phone QR.")?;
    let pin = invite
        .cert_fingerprint
        .as_deref()
        .filter(|pin| !pin.is_empty())
        .ok_or("Pairing needs the server certificate pin.")?;
    let authority = invite
        .ca_fingerprint
        .as_deref()
        .ok_or("Pairing needs the certificate authority pin. Generate a new phone QR.")?;
    if !is_hex_secret(&invite.token) {
        return Err("The pairing token must contain 64 hexadecimal digits.".into());
    }
    let mut value = json!({"endpoint": endpoint, "certificate_pin": pin,
        "authority_ca": authority, "token": invite.token});
    if let Some(route) = &invite.tls_server_name {
        value["tls_server_name"] = json!(route);
    }
    let configuration: Configuration =
        serde_json::from_value(value.clone()).map_err(|_| "Invalid pairing configuration.")?;
    configuration.enrollment_target()?;
    Ok(value)
}

fn is_hex_secret(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn validate_pin(pin: &str) -> Result<(), String> {
    let hex = compact_pin(pin);
    if hex.len() != 64 || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err("A certificate pin must be a SHA-256 fingerprint (64 hex digits).".into());
    }
    Ok(())
}

fn compact_pin(pin: &str) -> String {
    pin.replace(':', "")
        .replace(char::is_whitespace, "")
        .to_ascii_lowercase()
}

fn is_loopback(url: &url::Url) -> Result<bool, String> {
    Ok(match url.host().ok_or("The endpoint needs a host.")? {
        url::Host::Domain(host) => host == "localhost",
        url::Host::Ipv4(ip) => ip.is_loopback(),
        url::Host::Ipv6(ip) => ip.is_loopback(),
    })
}

fn parse_endpoint(raw: &str) -> Result<url::Url, String> {
    let url = url::Url::parse(raw).map_err(|_| "Enter a quic:// or wss:// endpoint.")?;
    if !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
        || url.query().is_some()
    {
        return Err(
            "Credentials, query strings and fragments must not be embedded in the endpoint.".into(),
        );
    }
    Ok(url)
}

fn endpoint_target(url: &url::Url, loopback: bool) -> Result<Target, String> {
    match url.scheme() {
        "quic" if url.path().is_empty() || url.path() == "/" => {
            let host = url.host().ok_or("The endpoint needs a host.")?;
            Ok(Target::quic(format!(
                "{host}:{}",
                url.port().unwrap_or(8788)
            )))
        }
        "wss" => Ok(Target::ws(url.as_str())),
        "ws" if loopback => Ok(Target::ws(url.as_str())),
        _ => Err("Use quic:// or wss://; plaintext ws:// is allowed only on loopback.".into()),
    }
}

pub fn spec() -> rune::AppSpec<State> {
    crate::app_spec((393.0, 852.0), (1.0, 1.0))
}

pub fn state(configuration: Configuration) -> Result<State, String> {
    state_with_identity(configuration, None)
}

pub fn state_with_identity(
    configuration: Configuration,
    signer: Option<Arc<dyn DeviceSigner>>,
) -> Result<State, String> {
    let target = configuration.target_with_identity(signer)?;
    Ok(with_target(
        target,
        configuration.session.filter(|s| !s.trim().is_empty()),
    ))
}

fn with_target(target: Target, session: Option<String>) -> State {
    let mut state = State::new(&crate::launch::Resolved::embedded(session)).with_phone(true);
    state.machines.current = Some(target.name.clone());
    state.native = Some(Platform {
        target,
        connection_requested: false,
        paste_requested: false,
        active: true,
        away_since: None,
    });
    state
}

pub fn set_active(rt: &mut rune::Runtime<State>, active: bool) {
    rt.cancel_composition();
    rt.update(|state, tx| {
        state.preedit = None;
        let now = SystemTime::now();
        let native = state.native.as_mut();
        let away = native.and_then(|native| native.set_active(active, now));
        state.focus_changed(active);
        if active {
            state.resume_link(away);
        } else {
            // A finger still down will not be heard lifting.
            state.phone_interrupted();
        }
        tx.instant();
    });
}

/// Local qualification evidence; no credentials, endpoint or clipboard
/// contents. A real grid, not a placeholder title, must contain the marker.
pub fn report(rt: &rune::Runtime<State>, frames: u64, marker: &str) -> Value {
    let state = &rt.state;
    let grid = state
        .client
        .as_ref()
        .zip(state.shown.as_ref())
        .and_then(|(c, s)| c.acquire_view(s.view?));
    let text = grid
        .as_ref()
        .map(|frame| {
            let cols = usize::from(frame.cols).max(1);
            frame
                .buffer
                .cells
                .chunks(cols)
                .enumerate()
                .map(|(row, cells)| {
                    (0..cells.len())
                        .map(|col| {
                            String::from_utf8_lossy(frame.buffer.cell_text(row * cols + col))
                        })
                        .collect::<String>()
                })
                .collect::<Vec<_>>()
                .join("\n")
        })
        .unwrap_or_default();
    json!({"schema": 1, "frames": frames, "connected": grid.is_some(),
        "session": state.selected_session_name(), "terminal_id": state.shown.as_ref().map(|s| s.terminal.to_string()),
        "marker_seen": !marker.is_empty() && text.contains(marker), "error": state.error})
}

mod host;
pub use host::{edge_srgb, paste, path_changed, take_requests, zoom};

#[cfg(test)]
mod pairing_tests;

#[cfg(test)]
mod tests;
