//! Native CLI route planning: address/credential acquisition and diagnostics.
//! Trust decisions remain in phux-dial's pure native planners.

use std::process::ExitCode;

use phux_client::attach::{AttachError, Dial, QuicDial, WsDial};

/// Stderr hint for a non-loopback dial that got no answer: either an overlay
/// network is down, or a packet filter on the server host (on macOS the
/// application firewall silently drops an unrecognized, adhoc-signed binary)
/// swallows it. `phux doctor` on the server probes for the latter.
pub(crate) const OVERLAY_REACHABILITY_HINT: &str = "      The server did not answer or its name could not be resolved; credentials were never checked.\n      If the host lives on an overlay network (Tailscale/WireGuard), confirm the overlay is up on both ends.\n      If the overlay is up and ssh to the host works, suspect a firewall on the SERVER host instead:\n      run `phux doctor` there, which probes its own remote listener and names the blocker.";

/// The reachability hint applies only to [`AttachError::Unreachable`] on a
/// non-loopback target; a pin or auth failure means a host answered.
pub(crate) fn reachability_hint(err: &AttachError, loopback: bool) -> Option<&'static str> {
    (!loopback && matches!(err, AttachError::Unreachable(_))).then_some(OVERLAY_REACHABILITY_HINT)
}

/// Split a `--quic` `HOST:PORT` dial target into host and port. HOST may be
/// a DNS name, an IPv4 literal, or a bracketed IPv6 literal (`[::1]:8788`);
/// brackets stay on the host half for the caller to trim.
fn split_host_port(target: &str) -> Result<(&str, u16), String> {
    let (host, port) = target.rsplit_once(':').ok_or_else(|| {
        format!("--quic target '{target}' is missing a port (expected HOST:PORT)")
    })?;
    if host.is_empty() {
        return Err(format!(
            "--quic target '{target}' is missing a host (expected HOST:PORT)"
        ));
    }
    let port = port
        .parse::<u16>()
        .map_err(|err| format!("--quic target '{target}' has an invalid port: {err}"))?;
    Ok((host, port))
}

/// Resolve a `--quic` `HOST:PORT` to its first address plus the default TLS
/// server name. Resolution precedes the trust decision, which keys on the
/// resolved address.
fn resolve_quic_target(
    rt: &tokio::runtime::Runtime,
    target: &str,
) -> Result<(std::net::SocketAddr, String), DialRefusal> {
    let (host, port) = split_host_port(target).map_err(DialRefusal::Malformed)?;
    let bare_host = host.trim_matches(['[', ']']);
    let host_is_ip_literal = bare_host.parse::<std::net::IpAddr>().is_ok();

    let resolved = rt
        .block_on(tokio::net::lookup_host((bare_host, port)))
        .map(|mut addrs| addrs.next());
    let detail = match resolved {
        Ok(Some(addr)) => {
            // The TLS server name defaults to the dialed hostname when one
            // was given (conventional SNI); an IP-literal target keeps the
            // historical `localhost` default, matching the server's
            // self-signed SANs.
            let server_name = if host_is_ip_literal {
                "localhost".to_owned()
            } else {
                bare_host.to_owned()
            };
            return Ok((addr, server_name));
        }
        Ok(None) => "name resolution returned no addresses".to_owned(),
        Err(err) => format!("name resolution failed: {err}"),
    };
    // An unresolvable name is the overlay-down shape (MagicDNS unreachable),
    // so it earns the same hint as an unanswered dial.
    Err(DialRefusal::Unresolved {
        target: target.to_owned(),
        detail,
        dns_name: !host_is_ip_literal,
    })
}

/// A one-off dial's relay route and the CA of the server behind it
/// (`--tls-server-name`, `--ca-fingerprint`): the dial runs an end-to-end
/// session to that server inside the relayed stream (ADR-0180).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RelayHop {
    /// The route, offered as the outer TLS server name.
    pub(crate) route: String,
    /// The server's CA, `sha256:` form. `None` is refused at planning.
    pub(crate) ca: Option<String>,
}
/// What a remote dial authenticates with: the bearer token, the server pin,
/// and the workload client identity (`None` reads the environment).
pub(crate) struct RemoteCredentials {
    /// The registry label, or `None` to label a refusal with the endpoint.
    pub(crate) name: Option<String>,
    pub(crate) token: Option<String>,
    pub(crate) cert_fingerprint: Option<String>,
    pub(crate) authority: phux_client_runtime::target::AuthorityPin,
    /// `None` permits the CLI adapter to read the workload environment.
    pub(crate) identity: Option<phux_dial::TlsClientIdentity>,
}

impl RemoteCredentials {
    /// Project pure trust refusals into the existing CLI diagnostic contract.
    fn refusal(
        &self,
        endpoint: &str,
        websocket: bool,
        error: &phux_dial::PlanRefusal,
    ) -> DialRefusal {
        use phux_dial::PlanRefusal;
        match error {
            PlanRefusal::Unpinned if websocket => DialRefusal::UnpinnedWs {
                url: endpoint.into(),
            },
            PlanRefusal::Unpinned => DialRefusal::UnpinnedQuic {
                target: endpoint.into(),
            },
            PlanRefusal::Plaintext => DialRefusal::Plaintext {
                url: endpoint.into(),
            },
            PlanRefusal::NoToken => DialRefusal::NoToken {
                url: endpoint.into(),
            },
            PlanRefusal::BadToken => DialRefusal::BadToken("pairing token is not valid hex".into()),
            PlanRefusal::RelayRequiresQuic => {
                DialRefusal::Malformed("a relay route needs a quic:// endpoint".into())
            }
            PlanRefusal::UnpinnedRelay => {
                let route = self.authority.route.as_deref().unwrap_or(endpoint);
                DialRefusal::Malformed(format!(
                    "{} (`phux attach --remote NAME --code LINK` with a link from `phux pair \
                     --relay-route {route}` on the server, or `phux host add ... --ca-fingerprint`)",
                    phux_client_runtime::dial::unpinned_relay_route(
                        self.name.as_deref().unwrap_or(endpoint),
                        route,
                    ),
                ))
            }
        }
    }
}
/// A remote dial ready to connect, plus whether it stays on this machine
/// (the failure hints key on it). Shared by `attach --quic/--ws` and the
/// headless verbs' `--remote`.
pub(crate) struct DialPlan {
    pub(crate) dial: Dial,
    pub(crate) loopback: bool,
}

/// Why a remote dial could not be planned; each caller words its own
/// remedy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum DialRefusal {
    /// The endpoint text did not parse: a bad `HOST:PORT` or URL.
    Malformed(String),
    /// The host did not resolve. `dns_name` is false for an IP literal,
    /// which never touches DNS and so never earns the overlay hint.
    Unresolved {
        target: String,
        detail: String,
        dns_name: bool,
    },
    /// A routable QUIC endpoint with no certificate pin.
    UnpinnedQuic { target: String },
    /// A routable `wss://` endpoint with no certificate pin.
    UnpinnedWs { url: String },
    /// Plaintext `ws://` to a routable address.
    Plaintext { url: String },
    /// A routable `wss://` endpoint with no bearer token.
    NoToken { url: String },
    /// The bearer token is not well-formed hex.
    BadToken(String),
}

impl DialRefusal {
    /// The lines `phux attach --quic/--ws` prints for this refusal, the same
    /// wording the planners printed before they returned a typed error.
    fn attach_lines(&self) -> Vec<String> {
        match self {
            Self::Malformed(err) | Self::BadToken(err) => vec![err.to_owned()],
            Self::Unresolved {
                target,
                detail,
                dns_name,
            } => {
                let mut lines = vec![format!("QUIC attach to {target} failed: {detail}")];
                if *dns_name {
                    lines.push(OVERLAY_REACHABILITY_HINT.to_owned());
                }
                lines
            }
            Self::UnpinnedQuic { target } => vec![
                format!(
                    "refusing to dial non-loopback QUIC server {target} without --cert-fingerprint."
                ),
                "Run `phux pair` on the server host to print its certificate fingerprint,"
                    .to_owned(),
                format!("then pass it: phux attach --quic {target} --cert-fingerprint <FP>"),
            ],
            Self::UnpinnedWs { url } => vec![
                format!(
                    "refusing to dial non-loopback WebSocket server {url} without --cert-fingerprint."
                ),
                "Run `phux pair` on the server host, then pass the printed fingerprint.".to_owned(),
            ],
            Self::Plaintext { url } => vec![
                format!("refusing plaintext WebSocket attach to non-loopback URL {url}."),
                "Use wss:// plus `phux pair` credentials for remote devices.".to_owned(),
            ],
            Self::NoToken { url } => vec![
                format!("refusing remote WebSocket attach to {url} without --token."),
                "Run `phux pair` on the server host and pass the printed token once.".to_owned(),
            ],
        }
    }

    /// Print the attach wording and return the failure exit code.
    pub(crate) fn report_for_attach(&self) -> ExitCode {
        crate::output::error_block(self.attach_lines().iter().map(String::as_str));
        ExitCode::FAILURE
    }
}

/// Resolve `target` and build its QUIC dial; trust keys on the resolved
/// address.
pub(crate) fn plan_quic_dial(
    rt: &tokio::runtime::Runtime,
    target: &str,
    mut credentials: RemoteCredentials,
    server_name: Option<String>,
) -> Result<DialPlan, DialRefusal> {
    let (addr, default_server_name) = resolve_quic_target(rt, target)?;
    if credentials.authority.route.is_none() {
        credentials.authority.route = server_name;
    }
    let route = credentials.authority.route.as_deref();
    // Credential discovery belongs to this CLI adapter, never the planner.
    let identity = match credentials.identity.take() {
        None if route.is_some() => Some(
            phux_dial::tls::client_identity_from_env()
                .map_err(|err| DialRefusal::Malformed(err.to_string()))?,
        ),
        identity => identity,
    };
    let dial = QuicDial::plan(
        addr,
        default_server_name,
        credentials.token.as_deref(),
        credentials.cert_fingerprint.as_deref(),
        credentials.authority.ca.as_deref(),
        route,
        identity,
    )
    .map_err(|error| credentials.refusal(target, false, &error))?;
    Ok(DialPlan {
        dial: Dial::Quic(dial),
        loopback: addr.ip().is_loopback(),
    })
}

/// Validate `url` and its credentials and build the WebSocket dial, or say
/// why it cannot be.
pub(crate) fn plan_ws_dial(
    url: String,
    mut credentials: RemoteCredentials,
    tls_server_name: Option<String>,
) -> Result<DialPlan, DialRefusal> {
    let target = phux_dial::ws::WsTarget::parse(&url)
        .map_err(|err| DialRefusal::Malformed(err.to_string()))?;
    let (trust, token) = target
        .credentials(
            credentials
                .authority
                .trust(credentials.cert_fingerprint.as_deref()),
            credentials.token.take(),
            credentials.authority.route.as_deref(),
        )
        .map_err(|error| credentials.refusal(&url, true, &error))?;
    Ok(DialPlan {
        dial: Dial::Ws(WsDial {
            url,
            token,
            trust,
            tls_server_name,
            identity: credentials.identity,
        }),
        loopback: target.is_loopback(),
    })
}

#[cfg(test)]
mod tests;
