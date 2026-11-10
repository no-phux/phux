//! Safe application interface for a native phone host. UIKit owns its
//! window, input services and lifecycle; Rune owns the same Desk and view
//! as the desktop. The small iOS bridge crate owns the C ABI's unsafe code.

use phux_client_runtime::Target;
use serde::Deserialize;
use serde_json::{Value, json};
use std::sync::Arc;

pub use crate::desk::Desk as State;
pub use crate::desk::native_input::{Action, Bridge as Input, Operation};
pub use phux_client_runtime::TlsClientIdentity;
pub use phux_client_runtime::enroll::{DeviceSigner, enroll_target_blocking, held_identity};
pub use rune;

/// Native-only state, absent from the desktop-hosted `--phone` form.
pub struct Platform {
    pub(crate) target: Target,
    pub connection_requested: bool,
    pub active: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Configuration {
    pub endpoint: String,
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
        Ok(target)
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
        active: true,
    });
    state
}

pub fn set_active(rt: &mut rune::Runtime<State>, active: bool) {
    rt.cancel_composition();
    rt.update(|state, tx| {
        state.preedit = None;
        if let Some(native) = &mut state.native {
            native.active = active;
        }
        state.focus_changed(active);
        tx.instant();
    });
}

/// The surface's edge colour as sRGB, so UIKit can paint the status-bar and
/// home-indicator strips it keeps outside the surface as one ground.
pub fn edge_srgb(rt: &rune::Runtime<State>, dark: bool) -> [f32; 3] {
    let color = crate::ui::phone_edge_color(&rt.state, dark);
    [color.r, color.g, color.b].map(|linear| {
        let linear = linear.clamp(0.0, 1.0);
        if linear <= 0.003_130_8 {
            linear * 12.92
        } else {
            1.055 * linear.powf(1.0 / 2.4) - 0.055
        }
    })
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

#[cfg(test)]
mod pairing_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use phux_client_runtime::Transport;

    #[test]
    fn relay_configuration_rejects_invalid_sni_without_dropping_it() {
        let mut configuration = config("quic://localhost:8788");
        for route in ["", "127.0.0.1", "secret/bad", "-bad", "secret\0"] {
            configuration.tls_server_name = Some(route.into());
            let error = configuration.target().unwrap_err();
            assert!(!error.contains("secret"));
        }
        configuration.tls_server_name = Some("mini-route".into());
        assert!(configuration.target().is_ok());
        configuration.endpoint = "wss://localhost:8787".into();
        assert!(configuration.target().is_err());
    }

    fn config(endpoint: &str) -> Configuration {
        Configuration {
            endpoint: endpoint.into(),
            tls_server_name: None,
            certificate_pin: None,
            token: None,
            session: None,
            authority_ca: None,
            chain_pem: None,
            key_id: None,
        }
    }

    #[test]
    fn edge_colour_is_the_settings_page_or_the_terminal_ground_as_srgb() {
        let mut rt = rune::Runtime::new(
            with_target(config("quic://localhost:8788").target().unwrap(), None),
            crate::ui::view,
            (393.0, 852.0),
            rune::Env::new(true, 2.0),
        );
        // No terminal yet: the theme's own ground, encoded for UIKit.
        let ground = edge_srgb(&rt, true);
        let linear = crate::ui::phone_edge_color(&rt.state, true);
        assert!(ground.iter().all(|c| (0.0..=1.0).contains(c)), "{ground:?}");
        assert!(
            ground[2] > linear.b,
            "sRGB encoding lifts a dark linear value"
        );
        rt.update(|state, _| state.settings_open = true);
        assert_ne!(edge_srgb(&rt, true), ground, "Settings paints its own page");
    }

    #[test]
    fn remote_enrollment_and_connection_have_distinct_requirements() {
        let mut configuration = config("quic://example.com:8788");
        configuration.certificate_pin = Some("ab".repeat(32));
        configuration.token = Some("cd".repeat(32));
        assert!(configuration.target().is_ok());
        assert!(configuration.enrollment_target().is_err());
        assert!(configuration.validate_connection().is_err());
        configuration.authority_ca = Some(format!("sha256:{}", "ef".repeat(32)));
        let target = configuration.enrollment_target().unwrap();
        assert_eq!(target.authority.ca, configuration.authority_ca);
        configuration.chain_pem = Some("public certificate".into());
        assert!(configuration.validate_connection().is_err());
        configuration.key_id = Some("device-key".into());
        assert!(configuration.validate_connection().is_ok());
        assert!(state_with_identity(configuration, None).is_err());
    }

    #[test]
    fn mobile_rejects_malformed_credentials_and_wss_enrollment() {
        let mut configuration = config("quic://localhost:8788");
        for pin in ["no", "sha256:", &"x".repeat(64)] {
            configuration.authority_ca = Some(pin.into());
            assert!(configuration.target().is_err());
        }
        configuration.authority_ca = Some(format!("sha256:{}", "ab:".repeat(31) + "ab"));
        assert!(configuration.enrollment_target().is_ok());
        for token in ["secret", "a", "00\n"] {
            configuration.token = Some(token.into());
            assert!(configuration.target().is_err());
        }
        configuration.token = None;
        configuration.endpoint = "wss://localhost:8788".into();
        assert!(configuration.target().is_ok());
        assert!(configuration.enrollment_target().is_err());
    }

    #[test]
    fn configuration_round_trips_public_identity_fields_and_rejects_tickets() {
        let configuration: Configuration = serde_json::from_value(json!({
            "endpoint": "quic://localhost:8788",
            "authority_ca": format!("sha256:{}", "ab".repeat(32)),
            "chain_pem": "public chain",
            "key_id": "persistent-key-tag"
        }))
        .unwrap();
        assert_eq!(configuration.chain_pem.as_deref(), Some("public chain"));
        assert_eq!(configuration.key_id.as_deref(), Some("persistent-key-tag"));
        assert!(configuration.enrollment_target().is_ok());
        assert!(
            serde_json::from_value::<Configuration>(json!({
                "endpoint": "quic://localhost:8788", "ticket": "not persisted"
            }))
            .is_err()
        );
        let unpaired = config("quic://localhost:8788");
        assert!(unpaired.validate_connection().is_ok());
        assert!(matches!(
            unpaired.target_with_identity(None).unwrap().client_identity,
            TlsClientIdentity::None
        ));
    }

    #[test]
    fn mobile_never_infers_remote_trust_or_a_local_cli() {
        for endpoint in [
            "ws://example.com/",
            "wss://example.com/",
            "quic://example.com:8788",
            "file:///tmp/s",
            "quic://localhost/path",
            "wss://secret@example.com/",
        ] {
            assert!(config(endpoint).target().is_err(), "{endpoint}");
        }
        for endpoint in [
            "ws://127.0.0.1:9999/",
            "quic://localhost:8788",
            "quic://[::1]:8788",
        ] {
            assert!(config(endpoint).target().is_ok());
        }
        assert_eq!(
            config("quic://[::1]:8788").target().unwrap().transport,
            Transport::Quic("[::1]:8788".into())
        );
        let resolved = crate::launch::Resolved::embedded(None);
        assert!(!resolved.can_ensure());
        assert!(!resolved.missing_cli());
        assert!(resolved.ensured().phux.is_none());
    }
}
