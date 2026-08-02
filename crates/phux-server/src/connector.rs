//! Supervised outbound relay connectors (ADR-0051/ADR-0052).
//!
//! Each configured connector dials a relay with the dedicated
//! `phux-relay/1` ALPN, writes only its enrollment-token preamble on stream 0,
//! and accepts one ordinary phux consumer on every relay-initiated bidi
//! stream. Consumer bearer preambles are verified by the server's own
//! [`TokenStore`](crate::auth::TokenStore) before the existing client dispatch
//! sees any frame. A dropped relay leg is redialed with the same capped
//! exponential backoff as federation hub links.

use std::io;
use std::net::{IpAddr, SocketAddr};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use phux_dial::{CertTrust, QuicDial};
use phux_protocol::policy::{PeerIdentity, QUIC_RELAY_ALPN, TransportType};
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

use crate::hub::link::Backoff;
use crate::runtime::accept_loop;
use crate::runtime::input_lane::InputLaneHandle;
use crate::state::SharedState;
use crate::transport::Incoming;
use crate::transport::quic::{QuicReader, QuicWriter, authorize_preamble};

const BACKOFF_BASE: Duration = Duration::from_millis(500);
const BACKOFF_CAP: Duration = Duration::from_secs(30);
const AUTH_FAILED_CODE: u32 = 0x01;

/// A validated outbound relay dial plan. Credential contents remain on disk
/// and are re-read for every attempt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConnectorSpec {
    host: String,
    port: u16,
    trust: CertTrust,
    token_file: Option<PathBuf>,
}

impl core::fmt::Display for ConnectorSpec {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "{}:{}", self.host, self.port)
    }
}

/// Fail-closed connector configuration errors detected before server bind.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ConnectorError {
    /// The relay was not a valid `HOST:PORT` endpoint.
    #[error("connector relay {relay:?} is malformed: {reason}")]
    Malformed {
        /// Original endpoint.
        relay: String,
        /// Parse failure.
        reason: String,
    },
    /// A routable relay had no certificate pin.
    #[error("connector relay {relay} is routable and requires cert-fingerprint")]
    MissingFingerprint {
        /// Unsafe routable endpoint.
        relay: String,
    },
    /// A routable relay had no token path.
    #[error("connector relay {relay} is routable and requires token-file")]
    MissingToken {
        /// Unsafe routable endpoint.
        relay: String,
    },
}

/// Validate all connector entries into immutable dial plans.
///
/// No DNS or file I/O occurs here. That keeps startup deterministic while the
/// supervisor still re-reads rotating token files on every attempt.
pub fn plan_connectors(
    entries: &[phux_config::ConnectorConfigEntry],
) -> Result<Vec<ConnectorSpec>, ConnectorError> {
    entries.iter().map(plan_connector).collect()
}

/// Validate one connector entry.
pub fn plan_connector(
    entry: &phux_config::ConnectorConfigEntry,
) -> Result<ConnectorSpec, ConnectorError> {
    let (host, port) = parse_host_port(&entry.relay)?;
    let loopback = host_is_loopback(&host);
    let trust = if loopback {
        entry
            .cert_fingerprint
            .clone()
            .map_or(CertTrust::SkipVerify, CertTrust::Pinned)
    } else {
        let Some(pin) = entry.cert_fingerprint.clone() else {
            return Err(ConnectorError::MissingFingerprint {
                relay: entry.relay.clone(),
            });
        };
        if entry.token_file.is_none() {
            return Err(ConnectorError::MissingToken {
                relay: entry.relay.clone(),
            });
        }
        CertTrust::Pinned(pin)
    };

    Ok(ConnectorSpec {
        host,
        port,
        trust,
        token_file: entry.token_file.clone(),
    })
}

fn parse_host_port(relay: &str) -> Result<(String, u16), ConnectorError> {
    let malformed = |reason: &str| ConnectorError::Malformed {
        relay: relay.to_owned(),
        reason: reason.to_owned(),
    };
    let (host, port) = if let Some(rest) = relay.strip_prefix('[') {
        let Some((host, port)) = rest.split_once("]:") else {
            return Err(malformed("bracketed IPv6 must end with ]:PORT"));
        };
        (host, port)
    } else {
        relay
            .rsplit_once(':')
            .ok_or_else(|| malformed("expected HOST:PORT"))?
    };
    if host.is_empty() {
        return Err(malformed("host is empty"));
    }
    let port = port
        .parse::<u16>()
        .map_err(|_| malformed("port must be an integer from 1 to 65535"))?;
    if port == 0 {
        return Err(malformed("port must be an integer from 1 to 65535"));
    }
    Ok((host.to_owned(), port))
}

fn host_is_loopback(host: &str) -> bool {
    host.eq_ignore_ascii_case("localhost")
        || host
            .parse::<IpAddr>()
            .is_ok_and(|address| address.is_loopback())
}

/// Read one enrollment token and enforce the owner-only secret-file contract.
fn read_connector_token(path: Option<&Path>) -> Result<Option<Vec<u8>>, String> {
    let Some(path) = path else {
        return Ok(None);
    };
    let metadata = std::fs::metadata(path)
        .map_err(|error| format!("read token file metadata {}: {error}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        if metadata.permissions().mode() & 0o077 != 0 {
            return Err(format!(
                "token file {} must be owner-only (mode 0600)",
                path.display()
            ));
        }
    }
    let raw = std::fs::read_to_string(path)
        .map_err(|error| format!("read token file {}: {error}", path.display()))?;
    let token = raw
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .ok_or_else(|| format!("token file {} is empty", path.display()))?;
    let token = phux_dial::quic::parse_token_hex(token)
        .map_err(|error| format!("token file {}: {error}", path.display()))?;
    if token.len() != crate::auth::TOKEN_LEN {
        return Err(format!(
            "token file {} must contain exactly {} token bytes",
            path.display(),
            crate::auth::TOKEN_LEN
        ));
    }
    Ok(Some(token))
}

async fn resolve(spec: &ConnectorSpec) -> Result<SocketAddr, String> {
    tokio::net::lookup_host((spec.host.as_str(), spec.port))
        .await
        .map_err(|error| format!("resolve {spec}: {error}"))?
        .next()
        .ok_or_else(|| format!("resolve {spec}: no addresses"))
}

/// One established relay connection exposed as an `Incoming` source.
struct ConnectorIncoming {
    connection: quinn::Connection,
    consumer_tokens: Arc<crate::auth::TokenStore>,
}

impl Incoming for ConnectorIncoming {
    type Reader = QuicReader;
    type Writer = QuicWriter;

    async fn accept(&self) -> io::Result<(Self::Reader, Self::Writer, PeerIdentity)> {
        loop {
            let (mut send, mut recv) = self
                .connection
                .accept_bi()
                .await
                .map_err(io::Error::other)?;
            let relay = self.connection.remote_address();
            let Some(device_id) = authorize_preamble(&mut recv, &self.consumer_tokens).await else {
                warn!(%relay, "bridged consumer refused: missing or invalid server token");
                let _ = send.reset(AUTH_FAILED_CODE.into());
                let _ = recv.stop(AUTH_FAILED_CODE.into());
                continue;
            };
            return Ok((
                QuicReader::from_stream(recv),
                QuicWriter::from_stream(send),
                PeerIdentity {
                    uid: 0,
                    pid: None,
                    exe_path: None,
                    mcp_host_key: Some(device_id),
                    transport: TransportType::Quic,
                    source_addr: Some(relay.ip()),
                },
            ));
        }
    }

    fn accept_errors_are_fatal(&self) -> bool {
        true
    }

    fn kind(&self) -> &'static str {
        "relay-quic"
    }
}

/// Spawn one independently supervised task per connector plan.
pub(crate) fn spawn_connectors(
    specs: Vec<ConnectorSpec>,
    consumer_tokens: &Arc<crate::auth::TokenStore>,
    state: &SharedState,
    input_lane: &InputLaneHandle,
    root_token: &CancellationToken,
) {
    for spec in specs {
        let tokens = Arc::clone(consumer_tokens);
        let state = state.clone();
        let input_lane = input_lane.clone();
        let cancel = root_token.child_token();
        tokio::task::spawn_local(supervise(spec, tokens, state, input_lane, cancel));
    }
}

#[allow(
    clippy::future_not_send,
    reason = "ADR-0014: connector dispatch runs on the server LocalSet"
)]
async fn supervise(
    spec: ConnectorSpec,
    consumer_tokens: Arc<crate::auth::TokenStore>,
    state: SharedState,
    input_lane: InputLaneHandle,
    cancel: CancellationToken,
) {
    let mut backoff = Backoff::new(BACKOFF_BASE, BACKOFF_CAP);
    loop {
        let attempt = backoff.failures().saturating_add(1);
        let connected = async {
            let token = read_connector_token(spec.token_file.as_deref())?;
            let addr = resolve(&spec).await?;
            let dial = QuicDial {
                addr,
                server_name: spec.host.clone(),
                token,
                trust: spec.trust.clone(),
            };
            phux_dial::quic::dial_with_alpn(&dial, QUIC_RELAY_ALPN)
                .await
                .map_err(|error| error.to_string())
        };
        let outcome = tokio::select! {
            () = cancel.cancelled() => return,
            outcome = connected => outcome,
        };

        let (failed_attempt, error) = match outcome {
            Ok((endpoint, connection, control_send, control_recv)) => {
                info!(relay = %spec, attempt, "outbound connector established");
                let established_at = tokio::time::Instant::now();
                let incoming = ConnectorIncoming {
                    connection,
                    consumer_tokens: Arc::clone(&consumer_tokens),
                };
                // Keep the endpoint driver and connector-initiated stream 0
                // alive and silent while bridged consumer streams are served.
                let _control = (endpoint, control_send, control_recv);
                match accept_loop(
                    &incoming,
                    state.clone(),
                    cancel.clone(),
                    Some(input_lane.clone()),
                )
                .await
                {
                    Ok(()) => return,
                    Err(error) => (
                        backoff.settle_after_loss(established_at.elapsed()),
                        error.to_string(),
                    ),
                }
            }
            Err(error) => (attempt, error),
        };
        warn!(
            relay = %spec,
            attempt = failed_attempt,
            %error,
            "outbound connector lost; scheduling redial"
        );
        let delay = backoff.next_delay();
        tokio::select! {
            () = cancel.cancelled() => return,
            () = tokio::time::sleep(delay) => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write as _;

    fn entry(
        relay: &str,
        token_file: Option<&str>,
        fingerprint: Option<&str>,
    ) -> phux_config::ConnectorConfigEntry {
        phux_config::ConnectorConfigEntry {
            relay: relay.to_owned(),
            token_file: token_file.map(PathBuf::from),
            cert_fingerprint: fingerprint.map(str::to_owned),
            route: None,
        }
    }

    #[test]
    fn routable_relays_require_pin_and_token() {
        assert!(matches!(
            plan_connector(&entry("relay.example:4433", Some("/token"), None)),
            Err(ConnectorError::MissingFingerprint { .. })
        ));
        assert!(matches!(
            plan_connector(&entry("relay.example:4433", None, Some("AB"))),
            Err(ConnectorError::MissingToken { .. })
        ));
    }

    #[test]
    fn loopback_keeps_the_dev_trust_carveout() {
        let plan = plan_connector(&entry("[::1]:4433", None, None)).expect("loopback plan");
        assert!(matches!(plan.trust, CertTrust::SkipVerify));
        assert_eq!(plan.host, "::1");
        assert_eq!(plan.port, 4433);
    }

    #[test]
    fn malformed_relay_is_rejected_before_dial() {
        for relay in ["localhost", ":4433", "localhost:0", "[::1:4433"] {
            assert!(matches!(
                plan_connector(&entry(relay, None, None)),
                Err(ConnectorError::Malformed { .. })
            ));
        }
    }

    #[test]
    fn token_file_must_be_owner_only_and_valid_hex() {
        let mut file = tempfile::NamedTempFile::new().expect("token file");
        writeln!(file, "{}", "11".repeat(32)).expect("write token");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(file.path(), std::fs::Permissions::from_mode(0o644))
                .expect("set broad mode");
            let error = read_connector_token(Some(file.path())).expect_err("broad mode refused");
            assert!(error.contains("owner-only"), "{error}");
            std::fs::set_permissions(file.path(), std::fs::Permissions::from_mode(0o600))
                .expect("set owner-only mode");
        }
        assert_eq!(
            read_connector_token(Some(file.path())).expect("valid token"),
            Some(vec![0x11; 32])
        );
    }

    #[test]
    fn token_file_is_reread_and_length_checked() {
        let file = tempfile::NamedTempFile::new().expect("token file");
        std::fs::write(
            file.path(),
            format!("{}\n", "11".repeat(crate::auth::TOKEN_LEN)),
        )
        .expect("write first token");
        assert_eq!(
            read_connector_token(Some(file.path())).expect("first token"),
            Some(vec![0x11; crate::auth::TOKEN_LEN])
        );

        std::fs::write(
            file.path(),
            format!("{}\n", "22".repeat(crate::auth::TOKEN_LEN)),
        )
        .expect("rotate token");
        assert_eq!(
            read_connector_token(Some(file.path())).expect("rotated token"),
            Some(vec![0x22; crate::auth::TOKEN_LEN])
        );

        std::fs::write(file.path(), "22\n").expect("write short token");
        let error = read_connector_token(Some(file.path())).expect_err("short token refused");
        assert!(error.contains("exactly 32 token bytes"), "{error}");
    }
}
