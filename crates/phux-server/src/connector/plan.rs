//! Validating `[[connector]]` entries into dial plans before the server
//! binds (see [`super`]).

use std::net::IpAddr;
use std::path::PathBuf;

use phux_dial::CertTrust;

/// A validated outbound relay dial plan. Credential contents remain on disk
/// and are re-read for every attempt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConnectorSpec {
    pub(super) host: String,
    pub(super) port: u16,
    pub(super) trust: CertTrust,
    pub(super) token_file: Option<PathBuf>,
}

impl core::fmt::Display for ConnectorSpec {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        if self.host.contains(':') {
            write!(f, "[{}]:{}", self.host, self.port)
        } else {
            write!(f, "{}:{}", self.host, self.port)
        }
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
