//! Safe application interface for a native phone host. UIKit owns its
//! window, input services and lifecycle; Rune owns the same Desk and view
//! as the desktop. The small iOS bridge crate owns the C ABI's unsafe code.

use phux_client_runtime::Target;
use serde::Deserialize;
use serde_json::{Value, json};
use std::cell::RefCell;
use std::collections::VecDeque;
use std::net::Ipv4Addr;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

pub use crate::desk::Desk as State;
pub use crate::desk::native_input::{Action, Bridge as Input, Operation};
pub use crate::desk::phone::navigation::native::{menu_dismiss, menu_pick};
pub use phux_client_runtime::TlsClientIdentity;
pub use phux_client_runtime::enroll::{DeviceSigner, enroll_target_blocking, held_identity};
pub use rune;

/// Native-only state, absent from the desktop-hosted `--phone` form.
pub struct Platform {
    pub(crate) target: Target,
    /// The distributed app and its version ("phux 0.4.98"), when it says.
    pub(crate) app: Option<String>,
    pub connection_requested: bool,
    /// A command asked for the system pasteboard, which only the host reads.
    pub paste_requested: bool,
    /// The paperclip asked for the host's attachment menu.
    pub attach_requested: bool,
    /// Where the host's attachments go, newest first, and how many were
    /// asked for.
    pub(crate) attaching: VecDeque<crate::desk::phone::attach::Attaching>,
    pub(crate) attachments: u64,
    /// At most one source-bound native menu answer, including visual dismissal.
    pub(crate) held_menu: Option<crate::desk::phone::navigation::native::Request>,
    /// What [`crate::desk::presence`] last told the host it came from.
    pub(crate) presence: Option<crate::desk::presence::Inputs>,
    /// Where a computer posts this phone's agent counts while it is closed.
    pub(crate) live: Option<crate::desk::presence::Live>,
    pub active: bool,
    /// When the app left the foreground. Wall time: a monotonic clock may
    /// stand still while the device sleeps.
    pub away_since: Option<SystemTime>,
    /// The floating controls the last view run left for UIKit to draw as
    /// Liquid Glass ([`Control`]). The view only has `&Desk`, so it notes
    /// them here; the host reads where they landed after each frame.
    pub(crate) controls: RefCell<Vec<Control>>,
}

/// A floating phone control that Rune lays out, names and acts for, and the
/// native host draws: Liquid Glass must own its content, or it would blur
/// Rune's own label beneath it. Rune keeps an empty, labelled box of the
/// same size in its place, so layout, hit testing and its accessibility
/// node are unchanged; a tap on the native control runs that node's press.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Control {
    /// The Rune element's key, which the host's tap names back.
    pub key: String,
    /// The accessible name, which VoiceOver reads.
    pub label: String,
    /// Visible text beside the symbol; none for a round symbol button.
    pub title: Option<String>,
    /// A second, smaller line under the title, in its own colour.
    pub subtitle: Option<(String, rune::Color)>,
    /// Text starts at the leading edge with the symbol trailing, as a
    /// navigation title does; otherwise symbol and title are centred.
    pub leading: bool,
    /// An SF Symbol name.
    pub symbol: &'static str,
    pub enabled: bool,
    /// Whether the ground it sits on is dark, for the glass's appearance.
    pub dark: bool,
    /// Its symbol and title colour, linear.
    pub ink: rune::Color,
    /// A dot on its upper trailing shoulder (the bell's unread news).
    pub badge: Option<rune::Color>,
    /// The surface paints this control; UIKit hosts its touch and AX target.
    pub painted: bool,
}

impl Control {
    pub(crate) fn painted(key: &str, label: &str, enabled: bool) -> Self {
        Self {
            key: key.to_owned(),
            label: label.to_owned(),
            title: None,
            subtitle: None,
            leading: false,
            symbol: "",
            enabled,
            dark: false,
            ink: rune::Color::TRANSPARENT,
            badge: None,
            painted: true,
        }
    }
}

impl Platform {
    /// A foreground phone app that has asked for nothing.
    #[cfg(test)]
    pub fn test() -> Platform {
        Platform {
            target: phux_client_runtime::Target::uds("/tmp/not-used"),
            app: None,
            active: true,
            connection_requested: false,
            paste_requested: false,
            attach_requested: false,
            attaching: VecDeque::new(),
            attachments: 0,
            held_menu: None,
            presence: None,
            live: None,
            away_since: None,
            controls: RefCell::default(),
        }
    }

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
    /// The distributed app and its version, for About. Words only.
    #[serde(default)]
    pub app: Option<String>,
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
        let mut target = endpoint_target(&url)?;
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
        Self::require_native_trust(&mut target, loopback)?;
        self.require_phone_pairing(loopback)?;
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

    /// Phone pairing additionally requires the original leaf and token even
    /// for QUIC/CA-only trust. Enrollment and the held device key remain
    /// separate platform requirements, not native transport policy.
    fn require_phone_pairing(&self, loopback: bool) -> Result<(), String> {
        if !loopback
            && (self.certificate_pin.as_deref().is_none_or(str::is_empty)
                || self.token.as_deref().is_none_or(str::is_empty))
        {
            return Err("A remote host needs its pairing certificate pin and token.".into());
        }
        Ok(())
    }

    fn require_native_trust(target: &mut Target, loopback: bool) -> Result<(), String> {
        use phux_client_runtime::dial::{CertTrust, InnerTls, PlanRefusal, WsTarget};
        let route = target.authority.route.as_deref();
        let trust = target.authority.trust(target.cert_fingerprint.as_deref());
        let verdict = match &target.transport {
            phux_client_runtime::Transport::Quic(_) => CertTrust::for_endpoint(trust, loopback)
                .and_then(|_| {
                    InnerTls::for_route(
                        route,
                        target.authority.ca.as_deref(),
                        TlsClientIdentity::None,
                    )
                    .map(|_| ())
                }),
            phux_client_runtime::Transport::Ws(url) => {
                let parsed = WsTarget::parse(url).map_err(|err| err.to_string())?;
                parsed
                    .credentials(trust, target.token.take(), route)
                    .map(|(_, token)| {
                        target.token = token;
                    })
            }
            _ => unreachable!("endpoint_target admits only native QUIC and WebSocket"),
        };
        verdict.map_err(|error| match error {
            PlanRefusal::Unpinned | PlanRefusal::NoToken => {
                "A remote host needs its pairing certificate pin and token.".into()
            }
            PlanRefusal::Plaintext => {
                "Use quic:// or wss://; plaintext ws:// is allowed only on loopback.".into()
            }
            PlanRefusal::BadToken => "The pairing token must contain 64 hexadecimal digits.".into(),
            PlanRefusal::UnpinnedRelay => {
                "A relay route needs the server's certificate authority pin from pairing.".into()
            }
            PlanRefusal::RelayRequiresQuic => "A relay route needs a quic:// endpoint.".into(),
        })
    }
}

pub(super) fn is_hex_secret(value: &str) -> bool {
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
        url::Host::Domain("localhost") => true,
        // `quic://` is not a special URL scheme, so its IPv4 host parses
        // as an opaque domain rather than an address.
        url::Host::Domain(host) => host.parse().is_ok_and(|ip: Ipv4Addr| ip.is_loopback()),
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

fn endpoint_target(url: &url::Url) -> Result<Target, String> {
    match url.scheme() {
        "quic" if url.path().is_empty() || url.path() == "/" => {
            let host = url.host().ok_or("The endpoint needs a host.")?;
            Ok(Target::quic(format!(
                "{host}:{}",
                url.port().unwrap_or(8788)
            )))
        }
        "wss" | "ws" => Ok(Target::ws(url.as_str())),
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
    let session = configuration.session.filter(|s| !s.trim().is_empty());
    let mut state = with_target(target, session);
    if let Some(native) = state.native.as_mut() {
        native.app = configuration.app;
    }
    Ok(state)
}

fn with_target(target: Target, session: Option<String>) -> State {
    let mut state = State::new(&crate::launch::Resolved::embedded(session)).with_phone(true);
    state.machines.current = Some(target.name.clone());
    state.native = Some(Platform {
        target,
        app: None,
        connection_requested: false,
        paste_requested: false,
        attach_requested: false,
        attaching: VecDeque::new(),
        attachments: 0,
        held_menu: None,
        presence: None,
        live: None,
        active: true,
        away_since: None,
        controls: RefCell::default(),
    });
    state
}

pub fn set_active(rt: &mut rune::Runtime<State>, active: bool) {
    rt.finish_composition();
    rt.update(|state, tx| {
        state.connection.preedit = None;
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

/// Qualification log: the probe observed `input_ready` and is about to type.
pub fn trace_input_ready(rt: &rune::Runtime<State>) {
    if let Some(client) = rt.state.connection.client.as_ref() {
        client.trace_input_ready();
    }
}

/// Local qualification evidence; no credentials, endpoint or clipboard
/// contents. A real grid, not a placeholder title, must contain the marker.
pub fn report(rt: &rune::Runtime<State>, frames: u64, marker: &str) -> Value {
    let state = &rt.state;
    // A drawn frame can precede the shown terminal's attach (PHUX-834); only
    // the client's own gate says typed input would reach it.
    let input_ready = (state.connection.client.as_ref())
        .zip(state.connection.shown.as_ref())
        .is_some_and(|(c, s)| c.input_ready(&s.resource));
    let grid = state
        .connection
        .client
        .as_ref()
        .zip(state.connection.shown.as_ref())
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
        "input_ready": input_ready, "input_refusals": state.input_refusals,
        "session": state.selected_session_name(), "terminal_id": state.connection.shown.as_ref().map(|s| s.resource.to_string()),
        // Frames carry no soft-wrap flag, so a marker that wraps a row also counts.
        "marker_seen": !marker.is_empty() && (text.contains(marker) || text.replace('\n', "").contains(marker)),
        "error": state.connection.error})
}

mod host;
mod invite;
pub use host::{
    attach, controls, dictate, edge_srgb, open_link, paste, path_changed, press, set_accessibility,
    set_live, take_requests, viewport_changed, zoom,
};
pub use invite::parse_pairing_invite;

#[cfg(test)]
mod pairing_tests;

#[cfg(test)]
mod tests;
