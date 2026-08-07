//! Hub-mode satellite table and outbound dialer (phux-v45.1/v45.3, ADR-0007).
//!
//! A phux server acting as a federation *hub* consumes the satellite
//! registry declared in `config.toml` (`[[satellites]]`, see
//! [`phux_config::SatelliteConfigEntry`]). At startup the hub validates
//! every **enabled** entry's endpoint URI into a typed
//! [`SatelliteTarget`] and holds the result — alongside the entry's
//! ADR-0038 auth material — as a [`HubTable`] keyed by [`SatelliteHost`],
//! the same host token that tags `TerminalId::Satellite` on the wire
//! (ADR-0007, ADR-0015).
//!
//! The [`link`] submodule is the outbound dialer (phux-v45.3): one link
//! supervisor per table entry dials, authenticates, and maintains the
//! hub-to-satellite connection, exposing a per-satellite
//! [`link::LinkStatus`]. `quic://`, `ws://`, and `wss://` endpoints dial
//! through the shared `phux-dial` stack; `ssh://` endpoints dial through
//! the SSH-stdio transport (phux-v45.9): the system `ssh` binary running
//! the remote `phux stdio-bridge` verb. The [`relay`] submodule
//! (phux-v45.4) routes frames over the established links regardless of
//! transport: satellite-tagged terminal commands, input, and acks go out
//! with their ids rewritten to the satellite's `Local` space, and
//! responses/streams come back re-tagged `Satellite { host, id }`
//! (ADR-0007 §4, opaque relay). A server not started in hub mode never
//! reads the registry at all (see [`resolve_hub_table`]).

pub mod link;
pub mod relay;

use std::collections::BTreeMap;
use std::path::PathBuf;

use phux_config::SatelliteConfigEntry;
use phux_protocol::ids::SatelliteHost;

/// A satellite endpoint parsed into its transport scheme.
///
/// The variants mirror the transports the server itself listens on
/// (QUIC and WebSocket, plain or TLS) plus the SSH-stdio dial path
/// (`ssh://`, phux-v45.9), which reaches a satellite by spawning the
/// system `ssh` binary and bridging the wire over the remote
/// `phux stdio-bridge` verb's stdin/stdout (ADR-0007).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SatelliteTarget {
    /// `quic://host:port` — QUIC dial target (ADR-0007).
    Quic {
        /// Hostname, IPv4, or bracketed IPv6 literal.
        host: String,
        /// UDP port (1-65535).
        port: u16,
    },
    /// `ws://...` — plaintext WebSocket dial URL (loopback dev only).
    Ws {
        /// The full endpoint URL as configured.
        url: String,
    },
    /// `wss://...` — TLS WebSocket dial URL.
    Wss {
        /// The full endpoint URL as configured.
        url: String,
    },
    /// `ssh://[user@]host[:port]` — SSH-stdio dial target (phux-v45.9).
    ///
    /// The fields become arguments to the system `ssh` binary (never a
    /// shell), so they are charset-validated at parse time: a host or
    /// user that could be read as an `ssh` option (leading `-`) or
    /// smuggle extra argv/config is rejected outright — the hub table
    /// fails closed at startup rather than at dial time.
    Ssh {
        /// Login user (`-l`), if the endpoint named one.
        user: Option<String>,
        /// Hostname, IPv4, IPv6 literal (stored without brackets), or an
        /// `ssh_config` alias.
        host: String,
        /// SSH port (`-p`), if the endpoint named one; `None` defers to
        /// ssh's own default/config resolution.
        port: Option<u16>,
    },
}

impl core::fmt::Display for SatelliteTarget {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Quic { host, port } => write!(f, "quic://{host}:{port}"),
            Self::Ws { url } | Self::Wss { url } => f.write_str(url),
            Self::Ssh { user, host, port } => {
                f.write_str("ssh://")?;
                if let Some(user) = user {
                    write!(f, "{user}@")?;
                }
                // Re-bracket IPv6 literals so the display round-trips as
                // a valid endpoint URI.
                if host.contains(':') {
                    write!(f, "[{host}]")?;
                } else {
                    f.write_str(host)?;
                }
                if let Some(port) = port {
                    write!(f, ":{port}")?;
                }
                Ok(())
            }
        }
    }
}

/// Errors produced while building a [`HubTable`] from the registry.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum HubTableError {
    /// An entry's endpoint URI could not be parsed into a
    /// [`SatelliteTarget`].
    #[error("satellite {name:?}: malformed endpoint {endpoint:?}: {reason}")]
    MalformedEndpoint {
        /// Hub-local satellite name of the offending entry.
        name: String,
        /// The endpoint string as configured.
        endpoint: String,
        /// Why it did not parse.
        reason: String,
    },

    /// Two registry entries share a name. Names key the table (and tag
    /// `TerminalId::Satellite` on the wire), so duplicates are rejected
    /// outright — including duplicates involving disabled entries, to
    /// match the `phux host add --role satellite` CRUD invariant.
    #[error("duplicate satellite name {name:?} in registry")]
    DuplicateName {
        /// The name that appears more than once.
        name: String,
    },
}

/// One validated hub-table entry: the parsed dial target plus the
/// ADR-0038 auth material the dialer needs to authenticate as a remote
/// consumer (pairing token by file path, certificate-fingerprint pin).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HubEntry {
    /// The endpoint parsed into its transport scheme.
    pub target: SatelliteTarget,
    /// Path to the file holding the pairing bearer token (ADR-0038). The
    /// dialer re-reads it on every attempt so token rotation needs no hub
    /// restart. `None` is only dialable for loopback endpoints — the link
    /// planner fails closed on routable ones (see [`link::plan_link`]).
    pub token_file: Option<PathBuf>,
    /// SHA-256 fingerprint pin of the satellite's TLS leaf certificate.
    /// Same fail-closed rule as the token: required for routable
    /// endpoints, optional for loopback dev.
    pub cert_fingerprint: Option<String>,
}

/// The validated, runtime-held satellite table for a hub server.
///
/// Keyed by [`SatelliteHost`]; ordered (`BTreeMap`) so startup logging
/// and iteration are deterministic. Built once at startup by
/// [`resolve_hub_table`]; the [`link`] supervisors dial from it, and
/// phux-v45.4 will route over the established links.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HubTable {
    entries: BTreeMap<SatelliteHost, HubEntry>,
}

impl HubTable {
    /// Build the table from the raw registry entries.
    ///
    /// Disabled entries are skipped (their endpoints are not validated —
    /// a disabled satellite must never block hub startup), but their
    /// names still count for duplicate detection, matching the CRUD
    /// invariant enforced by `phux host add --role satellite`.
    ///
    /// # Errors
    ///
    /// [`HubTableError::DuplicateName`] if two entries share a name;
    /// [`HubTableError::MalformedEndpoint`] if an enabled entry's
    /// endpoint does not parse.
    pub fn from_registry(satellites: &[SatelliteConfigEntry]) -> Result<Self, HubTableError> {
        let mut entries = BTreeMap::new();
        let mut seen = std::collections::HashSet::new();
        for satellite in satellites {
            if !seen.insert(satellite.name.as_str()) {
                return Err(HubTableError::DuplicateName {
                    name: satellite.name.clone(),
                });
            }
            if !satellite.enabled {
                continue;
            }
            let target = parse_endpoint(&satellite.endpoint).map_err(|reason| {
                HubTableError::MalformedEndpoint {
                    name: satellite.name.clone(),
                    endpoint: satellite.endpoint.clone(),
                    reason,
                }
            })?;
            entries.insert(
                SatelliteHost::new(satellite.name.clone()),
                HubEntry {
                    target,
                    token_file: satellite.token_file.clone(),
                    cert_fingerprint: satellite.cert_fingerprint.clone(),
                },
            );
        }
        Ok(Self { entries })
    }

    /// Number of enabled, validated satellites in the table.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// `true` when no enabled satellites are configured.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Look up a satellite's validated entry by host token.
    #[must_use]
    pub fn get(&self, host: &SatelliteHost) -> Option<&HubEntry> {
        self.entries.get(host)
    }

    /// Iterate the table in deterministic (name) order.
    pub fn iter(&self) -> impl Iterator<Item = (&SatelliteHost, &HubEntry)> {
        self.entries.iter()
    }
}

/// Gate + build: the one call sites use.
///
/// Returns `Ok(None)` when `hub` is `false` — a non-hub server ignores
/// the registry entirely, malformed entries and all (they are the CRUD
/// surface's problem until hub mode is requested). When `hub` is `true`
/// the registry is validated into a [`HubTable`]; any error here should
/// fail server startup, because a hub with a half-parsed table would
/// silently drop satellites.
///
/// # Errors
///
/// Propagates [`HubTableError`] from [`HubTable::from_registry`] in hub
/// mode.
pub fn resolve_hub_table(
    hub: bool,
    satellites: &[SatelliteConfigEntry],
) -> Result<Option<HubTable>, HubTableError> {
    if !hub {
        return Ok(None);
    }
    HubTable::from_registry(satellites).map(Some)
}

/// Parse one endpoint URI into a [`SatelliteTarget`], scheme-first.
///
/// Deliberately not a full URL parser (no new dependency for four
/// schemes): split on `://`, then apply per-scheme shape rules. Errors
/// are human-readable reasons, wrapped with the satellite's name by the
/// caller.
fn parse_endpoint(endpoint: &str) -> Result<SatelliteTarget, String> {
    let Some((scheme, rest)) = endpoint.split_once("://") else {
        return Err(
            "missing '<scheme>://' prefix (expected quic://, ws://, wss://, or ssh://)".to_owned(),
        );
    };
    if scheme.is_empty() {
        return Err("empty scheme before '://'".to_owned());
    }
    if rest.is_empty() {
        return Err(format!("empty host after '{scheme}://'"));
    }
    match scheme {
        "quic" => {
            // QUIC dial targets are `host:port` — the dialer (phux-v45.3)
            // resolves the host and needs an explicit UDP port; there is
            // no default port to assume. `rsplit_once` keeps bracketed
            // IPv6 literals (`quic://[::1]:8788`) intact.
            if rest.contains('/') {
                return Err("quic endpoint must be host:port with no path".to_owned());
            }
            let Some((host, port)) = rest.rsplit_once(':') else {
                return Err("quic endpoint requires an explicit port (quic://host:port)".to_owned());
            };
            if host.is_empty() {
                return Err("quic endpoint has an empty host".to_owned());
            }
            let port: u16 = port
                .parse()
                .map_err(|_| format!("quic endpoint port {port:?} is not a valid port number"))?;
            if port == 0 {
                return Err("quic endpoint port must be non-zero".to_owned());
            }
            Ok(SatelliteTarget::Quic {
                host: host.to_owned(),
                port,
            })
        }
        "ws" | "wss" => {
            // WebSocket targets keep the whole URL (path and all) — the
            // dialer hands it to the WS client verbatim. Only require a
            // non-empty authority.
            let authority = rest.split('/').next().unwrap_or("");
            if authority.is_empty() {
                return Err(format!("{scheme} endpoint has an empty host"));
            }
            if scheme == "ws" {
                Ok(SatelliteTarget::Ws {
                    url: endpoint.to_owned(),
                })
            } else {
                Ok(SatelliteTarget::Wss {
                    url: endpoint.to_owned(),
                })
            }
        }
        "ssh" => parse_ssh_authority(rest),
        other => Err(format!(
            "unsupported scheme {other:?} (expected quic://, ws://, wss://, or ssh://)"
        )),
    }
}

/// Parse the authority of an `ssh://[user@]host[:port]` endpoint.
///
/// Stricter than the other schemes because the parts become **argv for
/// the system `ssh` binary** (see [`link`]): a malformed or hostile
/// authority must fail the hub table (fail closed at startup), never
/// reach a spawn. No path, no query, no empty parts; user and host are
/// charset-allowlisted and must not start with `-` (option injection);
/// IPv6 literals must be bracketed and are stored bare.
fn parse_ssh_authority(rest: &str) -> Result<SatelliteTarget, String> {
    if rest.contains('/') {
        return Err("ssh endpoint must be [user@]host[:port] with no path".to_owned());
    }
    let (user, host_port) = match rest.split_once('@') {
        Some((user, host_port)) => {
            validate_ssh_word(user, "user")?;
            (Some(user.to_owned()), host_port)
        }
        None => (None, rest),
    };
    let (host, port) = if let Some(inner) = host_port.strip_prefix('[') {
        // Bracketed IPv6 literal: `[::1]` or `[::1]:2222`.
        let Some((host, after)) = inner.split_once(']') else {
            return Err("ssh endpoint has an unclosed '[' in its host".to_owned());
        };
        if host.is_empty() {
            return Err("ssh endpoint has an empty host".to_owned());
        }
        if !host
            .chars()
            .all(|c| c.is_ascii_hexdigit() || matches!(c, ':' | '.' | '%'))
        {
            return Err(format!(
                "ssh endpoint IPv6 host {host:?} has invalid characters"
            ));
        }
        match after.strip_prefix(':') {
            None if after.is_empty() => (host.to_owned(), None),
            None => {
                return Err(format!(
                    "ssh endpoint has trailing garbage after ']': {after:?}"
                ));
            }
            Some(port) => (host.to_owned(), Some(parse_ssh_port(port)?)),
        }
    } else if let Some((host, port)) = host_port.rsplit_once(':') {
        validate_ssh_word(host, "host")?;
        (host.to_owned(), Some(parse_ssh_port(port)?))
    } else {
        validate_ssh_word(host_port, "host")?;
        (host_port.to_owned(), None)
    };
    Ok(SatelliteTarget::Ssh { user, host, port })
}

/// Allowlist one `ssh` user or host token: non-empty, `[A-Za-z0-9._-]`
/// only (hostnames, IPv4 literals, and `ssh_config` aliases all fit),
/// and never starting with `-` so it cannot be read as an `ssh` option.
fn validate_ssh_word(word: &str, what: &str) -> Result<(), String> {
    if word.is_empty() {
        return Err(format!("ssh endpoint has an empty {what}"));
    }
    if word.starts_with('-') {
        return Err(format!(
            "ssh endpoint {what} {word:?} must not start with '-'"
        ));
    }
    if let Some(bad) = word
        .chars()
        .find(|c| !(c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-')))
    {
        return Err(format!(
            "ssh endpoint {what} {word:?} has invalid character {bad:?}"
        ));
    }
    Ok(())
}

/// Parse an explicit ssh port: 1-65535.
fn parse_ssh_port(port: &str) -> Result<u16, String> {
    let parsed: u16 = port
        .parse()
        .map_err(|_| format!("ssh endpoint port {port:?} is not a valid port number"))?;
    if parsed == 0 {
        return Err("ssh endpoint port must be non-zero".to_owned());
    }
    Ok(parsed)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(name: &str, endpoint: &str, enabled: bool) -> SatelliteConfigEntry {
        SatelliteConfigEntry {
            name: name.to_owned(),
            endpoint: endpoint.to_owned(),
            enabled,
            token_file: None,
            cert_fingerprint: None,
        }
    }

    // --- endpoint parsing matrix -------------------------------------

    #[test]
    fn parses_quic_host_port() {
        assert_eq!(
            parse_endpoint("quic://devbox:8788"),
            Ok(SatelliteTarget::Quic {
                host: "devbox".to_owned(),
                port: 8788,
            })
        );
    }

    #[test]
    fn parses_quic_ipv6_literal() {
        assert_eq!(
            parse_endpoint("quic://[::1]:8788"),
            Ok(SatelliteTarget::Quic {
                host: "[::1]".to_owned(),
                port: 8788,
            })
        );
    }

    #[test]
    fn parses_ws_and_wss_urls() {
        assert_eq!(
            parse_endpoint("ws://127.0.0.1:8787"),
            Ok(SatelliteTarget::Ws {
                url: "ws://127.0.0.1:8787".to_owned(),
            })
        );
        assert_eq!(
            parse_endpoint("wss://host:8787/phux"),
            Ok(SatelliteTarget::Wss {
                url: "wss://host:8787/phux".to_owned(),
            })
        );
    }

    // --- ssh endpoint matrix (phux-v45.9) ------------------------------

    #[test]
    fn parses_ssh_host_user_port_matrix() {
        let cases: &[(&str, Option<&str>, &str, Option<u16>)] = &[
            ("ssh://devbox", None, "devbox", None),
            ("ssh://devbox.example.com", None, "devbox.example.com", None),
            ("ssh://devbox:2222", None, "devbox", Some(2222)),
            ("ssh://me@devbox", Some("me"), "devbox", None),
            ("ssh://me@devbox:2222", Some("me"), "devbox", Some(2222)),
            (
                "ssh://build-agent@10.0.0.7:22",
                Some("build-agent"),
                "10.0.0.7",
                Some(22),
            ),
            ("ssh://[::1]", None, "::1", None),
            ("ssh://[::1]:2222", None, "::1", Some(2222)),
            (
                "ssh://me@[2001:db8::1]:2222",
                Some("me"),
                "2001:db8::1",
                Some(2222),
            ),
        ];
        for (endpoint, user, host, port) in cases {
            assert_eq!(
                parse_endpoint(endpoint),
                Ok(SatelliteTarget::Ssh {
                    user: user.map(str::to_owned),
                    host: (*host).to_owned(),
                    port: *port,
                }),
                "{endpoint}"
            );
        }
    }

    #[test]
    fn rejects_malformed_ssh_endpoints() {
        // (endpoint, expected reason fragment)
        let cases: &[(&str, &str)] = &[
            ("ssh://devbox/path", "no path"),
            ("ssh://@devbox", "empty user"),
            ("ssh://me@", "empty host"),
            ("ssh://devbox:", "not a valid port"),
            ("ssh://devbox:0", "non-zero"),
            ("ssh://devbox:70000", "not a valid port"),
            ("ssh://devbox:ssh", "not a valid port"),
            ("ssh://[::1", "unclosed"),
            ("ssh://[]", "empty host"),
            ("ssh://[::1]junk", "trailing garbage"),
            // Unbracketed IPv6 is ambiguous with `host:port`.
            ("ssh://::1", "invalid character"),
            // Option injection: a host or user readable as an ssh flag.
            ("ssh://-oProxyCommand=evil", "must not start with '-'"),
            ("ssh://-fool@devbox", "must not start with '-'"),
            // Shell metacharacters and whitespace never reach argv.
            ("ssh://dev box", "invalid character"),
            ("ssh://devbox;rm", "invalid character"),
            ("ssh://me$@devbox", "invalid character"),
            ("ssh://dev`box`", "invalid character"),
        ];
        for (endpoint, fragment) in cases {
            let err = parse_endpoint(endpoint).unwrap_err();
            assert!(err.contains(fragment), "{endpoint}: {err}");
        }
    }

    #[test]
    fn rejects_missing_scheme() {
        let err = parse_endpoint("devbox:8788").unwrap_err();
        assert!(err.contains("missing '<scheme>://'"), "{err}");
    }

    #[test]
    fn rejects_empty_scheme() {
        let err = parse_endpoint("://devbox").unwrap_err();
        assert!(err.contains("empty scheme"), "{err}");
    }

    #[test]
    fn rejects_unknown_scheme() {
        let err = parse_endpoint("http://devbox").unwrap_err();
        assert!(err.contains("unsupported scheme \"http\""), "{err}");
    }

    #[test]
    fn rejects_empty_host() {
        assert!(parse_endpoint("quic://").is_err());
        assert!(parse_endpoint("quic://:8788").is_err());
        assert!(parse_endpoint("ws:///path").is_err());
        assert!(parse_endpoint("wss://").is_err());
    }

    #[test]
    fn rejects_quic_without_port() {
        let err = parse_endpoint("quic://devbox").unwrap_err();
        assert!(err.contains("explicit port"), "{err}");
    }

    #[test]
    fn rejects_quic_bad_port() {
        assert!(parse_endpoint("quic://devbox:phux").is_err());
        assert!(parse_endpoint("quic://devbox:0").is_err());
        assert!(parse_endpoint("quic://devbox:70000").is_err());
    }

    #[test]
    fn rejects_quic_with_path() {
        let err = parse_endpoint("quic://devbox:8788/route").unwrap_err();
        assert!(err.contains("no path"), "{err}");
    }

    // --- table construction ------------------------------------------

    #[test]
    fn builds_table_keyed_by_satellite_host() {
        let table = HubTable::from_registry(&[
            entry("devbox", "quic://devbox:8788", true),
            entry("sandbox", "wss://sandbox:8787", true),
        ])
        .unwrap();
        assert_eq!(table.len(), 2);
        assert_eq!(
            table.get(&SatelliteHost::new("devbox")).map(|e| &e.target),
            Some(&SatelliteTarget::Quic {
                host: "devbox".to_owned(),
                port: 8788,
            })
        );
        assert_eq!(
            table.get(&SatelliteHost::new("sandbox")).map(|e| &e.target),
            Some(&SatelliteTarget::Wss {
                url: "wss://sandbox:8787".to_owned(),
            })
        );
    }

    #[test]
    fn table_entries_carry_auth_material() {
        let mut satellite = entry("devbox", "quic://devbox:8788", true);
        satellite.token_file = Some(PathBuf::from("/secrets/devbox.token"));
        satellite.cert_fingerprint = Some("AB:CD".to_owned());
        let table = HubTable::from_registry(&[satellite]).unwrap();
        let held = table.get(&SatelliteHost::new("devbox")).unwrap();
        assert_eq!(
            held.token_file.as_deref(),
            Some(std::path::Path::new("/secrets/devbox.token"))
        );
        assert_eq!(held.cert_fingerprint.as_deref(), Some("AB:CD"));
    }

    #[test]
    fn disabled_entries_are_skipped_even_when_malformed() {
        let table = HubTable::from_registry(&[
            entry("devbox", "quic://devbox:8788", true),
            entry("parked", "not a uri at all", false),
        ])
        .unwrap();
        assert_eq!(table.len(), 1);
        assert!(table.get(&SatelliteHost::new("parked")).is_none());
    }

    #[test]
    fn duplicate_names_rejected() {
        let err = HubTable::from_registry(&[
            entry("devbox", "quic://a:1", true),
            entry("devbox", "quic://b:2", true),
        ])
        .unwrap_err();
        assert_eq!(
            err,
            HubTableError::DuplicateName {
                name: "devbox".to_owned(),
            }
        );
    }

    #[test]
    fn duplicate_names_rejected_even_when_one_is_disabled() {
        let err = HubTable::from_registry(&[
            entry("devbox", "quic://a:1", false),
            entry("devbox", "quic://b:2", true),
        ])
        .unwrap_err();
        assert!(matches!(err, HubTableError::DuplicateName { .. }));
    }

    #[test]
    fn malformed_enabled_entry_fails_with_name_and_endpoint() {
        let err = HubTable::from_registry(&[entry("devbox", "gopher://devbox", true)]).unwrap_err();
        match err {
            HubTableError::MalformedEndpoint {
                name,
                endpoint,
                reason,
            } => {
                assert_eq!(name, "devbox");
                assert_eq!(endpoint, "gopher://devbox");
                assert!(reason.contains("unsupported scheme"), "{reason}");
            }
            other @ HubTableError::DuplicateName { .. } => {
                panic!("expected MalformedEndpoint, got {other:?}")
            }
        }
    }

    // --- hub gate ------------------------------------------------------

    #[test]
    fn non_hub_mode_ignores_the_registry() {
        // Registry full of garbage: duplicates AND malformed endpoints.
        // Without hub mode none of it is read, so resolution succeeds
        // with no table.
        let garbage = [
            entry("devbox", "not a uri", true),
            entry("devbox", "also broken", true),
        ];
        assert_eq!(resolve_hub_table(false, &garbage), Ok(None));
    }

    #[test]
    fn hub_mode_validates_the_registry() {
        let table = resolve_hub_table(true, &[entry("devbox", "ssh://devbox", true)])
            .unwrap()
            .unwrap();
        assert_eq!(table.len(), 1);
        assert!(resolve_hub_table(true, &[entry("devbox", "nope", true)]).is_err());
    }

    #[test]
    fn hub_mode_with_empty_registry_is_an_empty_table() {
        let table = resolve_hub_table(true, &[]).unwrap().unwrap();
        assert!(table.is_empty());
    }

    #[test]
    fn display_is_log_friendly_and_round_trips() {
        for endpoint in [
            "quic://devbox:8788",
            "ssh://devbox",
            "ssh://me@devbox:2222",
            "ssh://[::1]:2222",
        ] {
            assert_eq!(
                parse_endpoint(endpoint).unwrap().to_string(),
                endpoint,
                "{endpoint}"
            );
        }
    }
}
