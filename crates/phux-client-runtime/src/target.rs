//! Registry resolution for a remote host: rung 1 of the CLI's `phux --remote`
//! ladder (ADR-0093), for native embedders.
//!
//! Reads the `[[remote]]` registry (ADR-0055) through the CLI's own
//! `phux-config` schema. The target grammar, registry match order, and
//! token-file rule here are the ones the CLI uses too
//! (`crates/phux/src/commands/remote_target.rs`); only the error wording
//! differs, through [`RemoteTarget::parse_labeled`]. Pairing (rungs 2-4) is
//! operator-present and stays in the CLI.

use std::path::{Path, PathBuf};

use phux_config::RemoteConfigEntry;
use phux_dial::TlsClientIdentity;

/// The QUIC port a server auto-binds on its overlay address (ADR-0081), and
/// therefore the port a target with no `:PORT` means.
pub const DEFAULT_QUIC_PORT: u16 = 8788;

/// A parsed `[USER@]HOST[:PORT]` target. `user@` is a registry label, never a
/// wire identity (see the CLI module's header).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteTarget {
    /// The `user@` label, if typed.
    pub user: Option<String>,
    /// The host as typed, unbracketed.
    pub host: String,
    /// An explicit `:PORT`, applied per dial and never written back.
    pub port: Option<u16>,
}

/// Why a target is not `[USER@]HOST[:PORT]`, worded by the caller: an
/// embedder has no flag to name, the CLI names the spelling in use.
#[derive(Debug, Clone, PartialEq, Eq)]
enum TargetError {
    /// Nothing but whitespace.
    Empty,
    /// A URI (the trimmed input).
    Uri(String),
    /// `@host` (the trimmed input).
    EmptyUser(String),
    /// `user@` or `:port` (the trimmed input).
    EmptyHost(String),
    /// A `/` or a leading selector sigil (the host).
    InvalidHost(String),
    /// `[v6` (the host-and-port text).
    UnclosedBracket(String),
    /// `[v6]x` (the host-and-port text).
    TrailingAfterBracket(String),
    /// Not `1..=65535` (the port text).
    InvalidPort(String),
}

impl TargetError {
    /// The embedder wording, with no CLI spelling to name.
    fn unlabeled(&self) -> String {
        match self {
            Self::Empty => "enter a registered host, e.g. mini or me@mini".to_owned(),
            Self::Uri(raw) => format!(
                "{raw:?} is a URI; register it with `phux host add NAME {raw}` and connect by NAME"
            ),
            Self::EmptyUser(raw) => format!("{raw:?} has an empty user"),
            Self::EmptyHost(raw) => format!("{raw:?} has an empty host"),
            Self::InvalidHost(host) => format!(
                "host {host:?} must not contain '/' or start with a selector sigil (@ # . =)"
            ),
            Self::UnclosedBracket(rest) => format!("{rest:?} has an unclosed '['"),
            Self::TrailingAfterBracket(rest) => format!("{rest:?} has trailing text after ']'"),
            Self::InvalidPort(raw) => format!("port {raw:?} must be 1..=65535"),
        }
    }

    /// The wording for a CLI spelling of the grammar (`--remote`, `host add`).
    fn labeled(&self, label: &str) -> String {
        match self {
            Self::Empty => format!("{label} needs a target, e.g. {label} me@mini"),
            Self::Uri(raw) => format!(
                "{label} takes [USER@]HOST[:PORT], not a URI (got {raw:?}); \
                 register a full endpoint with `phux host add NAME {raw}`"
            ),
            Self::EmptyUser(_)
            | Self::EmptyHost(_)
            | Self::UnclosedBracket(_)
            | Self::TrailingAfterBracket(_) => format!("{label} target {}", self.unlabeled()),
            Self::InvalidHost(_) | Self::InvalidPort(_) => format!("{label} {}", self.unlabeled()),
        }
    }
}

impl RemoteTarget {
    /// Parse `host`, `user@host`, `host:port`, `user@host:port`, and their
    /// bracketed-IPv6 spellings. A URI is refused rather than guessed at.
    pub fn parse(raw: &str) -> Result<Self, String> {
        Self::parse_target(raw).map_err(|err| err.unlabeled())
    }

    /// [`Self::parse`] with the errors worded for the CLI spelling `label`
    /// (`--remote`, `host add`) that shares the grammar.
    pub fn parse_labeled(raw: &str, label: &str) -> Result<Self, String> {
        Self::parse_target(raw).map_err(|err| err.labeled(label))
    }

    fn parse_target(raw: &str) -> Result<Self, TargetError> {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            return Err(TargetError::Empty);
        }
        if trimmed.contains("://") {
            return Err(TargetError::Uri(trimmed.to_owned()));
        }
        // Split on the LAST `@`: a username cannot contain `@`, but this way
        // an address that does is still parsed the way the operator meant.
        let (user, rest) = match trimmed.rsplit_once('@') {
            Some(("", _)) => return Err(TargetError::EmptyUser(trimmed.to_owned())),
            Some((user, rest)) => (Some(user.to_owned()), rest),
            None => (None, trimmed),
        };
        let (host, port) = split_host_port(rest)?;
        if host.is_empty() {
            return Err(TargetError::EmptyHost(trimmed.to_owned()));
        }
        // A registry name may not contain `/` (it would escape the token
        // directory on join) or a selector sigil.
        if host.contains('/') || host.starts_with(['@', '#', '.', '=']) {
            return Err(TargetError::InvalidHost(host));
        }
        Ok(Self { user, host, port })
    }

    /// The registry key: the typed spelling minus any port, so `mini` and
    /// `mini:8788` are one host.
    #[must_use]
    pub fn registry_name(&self) -> String {
        self.user
            .as_ref()
            .map_or_else(|| self.host.clone(), |user| format!("{user}@{}", self.host))
    }

    /// `HOST:PORT` to dial, bracketing an IPv6 literal.
    #[must_use]
    pub fn authority(&self) -> String {
        let port = self.port.unwrap_or(DEFAULT_QUIC_PORT);
        if self.host.contains(':') {
            format!("[{}]:{port}", self.host)
        } else {
            format!("{}:{port}", self.host)
        }
    }
}

fn split_host_port(rest: &str) -> Result<(String, Option<u16>), TargetError> {
    if let Some(inner) = rest.strip_prefix('[') {
        return split_bracketed(rest, inner);
    }
    // A bare IPv6 literal has no port: `fd7a::1:8788` is ambiguous, and
    // guessing would silently dial the wrong address.
    if rest.matches(':').count() > 1 {
        return Ok((rest.to_owned(), None));
    }
    match rest.split_once(':') {
        Some((host, port)) => Ok((host.to_owned(), Some(parse_port(port)?))),
        None => Ok((rest.to_owned(), None)),
    }
}

/// `[v6]` or `[v6]:port`: the brackets make the port unambiguous.
fn split_bracketed(rest: &str, inner: &str) -> Result<(String, Option<u16>), TargetError> {
    let (host, tail) = inner
        .split_once(']')
        .ok_or_else(|| TargetError::UnclosedBracket(rest.to_owned()))?;
    if tail.is_empty() {
        return Ok((host.to_owned(), None));
    }
    let port = tail
        .strip_prefix(':')
        .ok_or_else(|| TargetError::TrailingAfterBracket(rest.to_owned()))?;
    Ok((host.to_owned(), Some(parse_port(port)?)))
}

fn parse_port(raw: &str) -> Result<u16, TargetError> {
    raw.parse::<u16>()
        .ok()
        .filter(|port| *port != 0)
        .ok_or_else(|| TargetError::InvalidPort(raw.to_owned()))
}

/// The registry entry that describes `target`.
///
/// Matched as exact `user@host`, then the bare host (what `phux host add`
/// registers), then any entry whose endpoint addresses that host.
#[must_use]
pub fn find_entry<'a>(
    entries: &'a [RemoteConfigEntry],
    target: &RemoteTarget,
) -> Option<&'a RemoteConfigEntry> {
    find_entry_by(entries, target, |entry| (&entry.name, &entry.endpoint))
}

/// [`find_entry`] over any registry row type, given its `(name, endpoint)`.
#[must_use]
pub fn find_entry_by<'a, E>(
    entries: &'a [E],
    target: &RemoteTarget,
    name_and_endpoint: impl Fn(&E) -> (&str, &str),
) -> Option<&'a E> {
    let name = target.registry_name();
    let named = |wanted: &str| {
        entries
            .iter()
            .find(|entry| name_and_endpoint(entry).0 == wanted)
    };
    named(&name).or_else(|| named(&target.host)).or_else(|| {
        entries.iter().find(|entry| {
            endpoint_host(name_and_endpoint(entry).1).as_deref() == Some(&target.host)
        })
    })
}

/// The host an endpoint URI addresses, unbracketed; `None` when it has none.
#[must_use]
pub fn endpoint_host(endpoint: &str) -> Option<String> {
    let rest = endpoint.split_once("://").map(|(_, rest)| rest)?;
    let authority = rest.split(['/', '?']).next().unwrap_or(rest);
    let authority = authority.rsplit_once('@').map_or(authority, |(_, a)| a);
    let host = if let Some(inner) = authority.strip_prefix('[') {
        inner.split_once(']').map(|(host, _)| host)?
    } else if authority.matches(':').count() > 1 {
        authority
    } else {
        authority
            .split_once(':')
            .map_or(authority, |(host, _)| host)
    };
    (!host.is_empty()).then(|| host.to_owned())
}

/// The transport a registry endpoint names, restricted to what an embedder
/// can dial without a terminal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Transport {
    /// `quic://HOST:PORT`, carrying `HOST:PORT`.
    Quic(String),
    /// `ws://` or `wss://`, carrying the URL.
    Ws(String),
}

impl Transport {
    /// The transport's name as a log field.
    #[must_use]
    pub const fn label(&self) -> &'static str {
        match self {
            Self::Quic(_) => "quic",
            Self::Ws(_) => "ws",
        }
    }
}

/// Classify an endpoint, applying an explicit `:PORT` from the target as a
/// per-dial override (the registry is never rewritten). Returns the effective
/// endpoint URI beside its transport.
pub fn classify(endpoint: &str, target: &RemoteTarget) -> Result<(String, Transport), String> {
    let trimmed = endpoint.trim();
    if let Some(rest) = trimmed.strip_prefix("quic://") {
        if rest.is_empty() || !rest.contains(':') {
            return Err(format!(
                "registry endpoint {trimmed:?} needs quic://HOST:PORT"
            ));
        }
        if target.port.is_some() {
            let authority = target.authority();
            return Ok((format!("quic://{authority}"), Transport::Quic(authority)));
        }
        return Ok((trimmed.to_owned(), Transport::Quic(rest.to_owned())));
    }
    for scheme in ["wss", "ws"] {
        if trimmed.starts_with(&format!("{scheme}://")) {
            let url = if target.port.is_some() {
                format!("{scheme}://{}", target.authority())
            } else {
                trimmed.to_owned()
            };
            return Ok((url.clone(), Transport::Ws(url)));
        }
    }
    if trimmed.starts_with("ssh://") {
        return Err(format!(
            "{trimmed} rides ssh, which needs a terminal; run `phux --remote NAME` in one, \
             or give the host a direct listener with `phux host add`"
        ));
    }
    Err(format!(
        "registry endpoint {trimmed:?} must start with quic://, wss://, or ws://"
    ))
}

/// Read the bearer token behind an entry's `token-file`.
///
/// That is the first line that is neither blank nor a `#` comment. Failures
/// name the path and never echo token bytes.
pub fn read_token(path: Option<&Path>) -> Result<Option<String>, String> {
    let Some(path) = path else {
        return Ok(None);
    };
    let raw = std::fs::read_to_string(path)
        .map_err(|err| format!("could not read token file {}: {err}", path.display()))?;
    let token = raw
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty() && !line.starts_with('#'))
        .ok_or_else(|| format!("token file {} has no token line", path.display()))?;
    Ok(Some(token.to_owned()))
}

/// Everything a dial needs, resolved from the registry.
///
/// Holds no secret: the token file is only NAMED here, and the tunnel thread
/// reads it just before dialing, so resolving a host for display never
/// touches the token.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resolved {
    /// The registry entry's name.
    pub name: String,
    /// Effective endpoint URI, after any `:PORT` override.
    pub endpoint: String,
    /// The entry's pinned session, if any.
    pub session: Option<String>,
    /// The lane the endpoint selects.
    pub transport: Transport,
    /// Where the bearer token lives; read only at dial time.
    pub token_file: Option<PathBuf>,
    /// The SHA-256 leaf fingerprint to pin, or `None` for loopback.
    pub cert_fingerprint: Option<String>,
    /// The workload client certificate the entry enrolled (ADR-0116), or
    /// [`TlsClientIdentity::None`]. Only paths: the key is read by the TLS
    /// stack at dial time, and the environment is never consulted.
    pub client_identity: TlsClientIdentity,
}

/// The TLS identity a registry entry names: its enrolled certificate and
/// key, or none.
///
/// # Errors
///
/// Half an identity or a relative path (see
/// [`RemoteConfigEntry::client_identity`]).
pub fn entry_identity(entry: &RemoteConfigEntry) -> Result<TlsClientIdentity, String> {
    Ok(entry
        .client_identity()?
        .map_or(TlsClientIdentity::None, |(certificate, private_key)| {
            TlsClientIdentity::PemFiles {
                certificate,
                private_key,
            }
        }))
}

/// Resolve `raw` against the registry at `config_path`, or at the CLI's own
/// canonical path when none is given.
pub fn resolve(raw: &str, config_path: Option<&Path>) -> Result<Resolved, String> {
    let target = RemoteTarget::parse(raw)?;
    let path: PathBuf =
        config_path.map_or_else(phux_config::loader::config_path, Path::to_path_buf);
    let config = phux_config::loader::load_from(&path)
        .map_err(|err| format!("could not read the phux config {}: {err}", path.display()))?;
    reject_duplicates(&config.remote)?;
    let entry = find_entry(&config.remote, &target).ok_or_else(|| unregistered(&target))?;
    resolve_entry(entry, &target)
}

/// Resolve one registry entry for `target`, applying its explicit `:PORT`.
///
/// # Errors
///
/// An endpoint no embedder can dial ([`classify`]) or a malformed client
/// identity ([`entry_identity`]).
pub fn resolve_entry(entry: &RemoteConfigEntry, target: &RemoteTarget) -> Result<Resolved, String> {
    let (endpoint, transport) = classify(&entry.endpoint, target)?;
    Ok(Resolved {
        name: entry.name.clone(),
        endpoint,
        session: entry
            .session
            .clone()
            .filter(|session| !session.trim().is_empty()),
        transport,
        token_file: entry.token_file.clone(),
        cert_fingerprint: entry.cert_fingerprint.clone(),
        client_identity: entry_identity(entry)?,
    })
}

fn reject_duplicates(entries: &[RemoteConfigEntry]) -> Result<(), String> {
    let mut seen = std::collections::BTreeSet::new();
    for entry in entries {
        if !seen.insert(entry.name.as_str()) {
            return Err(format!(
                "the phux config registers {:?} twice; remove one with `phux host rm`",
                entry.name
            ));
        }
    }
    Ok(())
}

fn unregistered(target: &RemoteTarget) -> String {
    let name = target.registry_name();
    format!(
        "{name} is not a registered host; pair it once in a terminal with \
         `phux --remote {name}` or `phux host add {name}`"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(name: &str, endpoint: &str) -> RemoteConfigEntry {
        RemoteConfigEntry {
            name: name.to_owned(),
            endpoint: endpoint.to_owned(),
            token_file: None,
            cert_fingerprint: None,
            session: None,
            ssh: None,
            direct: None,
            client_cert: None,
            client_key: None,
        }
    }

    fn target(raw: &str) -> RemoteTarget {
        RemoteTarget::parse(raw).unwrap_or_else(|err| unreachable!("{raw}: {err}"))
    }

    #[test]
    fn parses_the_cli_target_spellings() {
        let bare = target("mini");
        assert_eq!(
            (bare.user, bare.host.as_str(), bare.port),
            (None, "mini", None)
        );
        let full = target("phall@mini:9999");
        assert_eq!(full.user.as_deref(), Some("phall"));
        assert_eq!(full.port, Some(9999));
        assert_eq!(full.registry_name(), "phall@mini");
        let bracketed = target("me@[fd7a::1]:8788");
        assert_eq!(bracketed.host, "fd7a::1");
        assert_eq!(bracketed.authority(), "[fd7a::1]:8788");
        let bare_v6 = target("fd7a::1");
        assert_eq!(bare_v6.port, None);
        assert_eq!(bare_v6.authority(), "[fd7a::1]:8788");
        // `mini` and `mini:8788` are one registry host, and a portless
        // target means the ADR-0081 auto-listen port.
        assert_eq!(target("mini:8788").registry_name(), "mini");
        assert_eq!(target("mini").authority(), "mini:8788");
    }

    #[test]
    fn refuses_what_the_cli_refuses() {
        for raw in [
            "",
            "  ",
            "@mini",
            "me@",
            "mini:0",
            "mini:70000",
            "mini:ssh",
            "../evil",
            "#tag",
        ] {
            assert!(RemoteTarget::parse(raw).is_err(), "{raw:?} must be refused");
        }
        let uri = RemoteTarget::parse("quic://mini:8788").expect_err("uri");
        assert!(uri.contains("phux host add"), "{uri}");
    }

    #[test]
    fn matches_name_then_bare_host_then_endpoint_host() {
        let entries = [
            entry("mini", "quic://mini.ts.net:8788"),
            entry("phall@studio", "wss://studio:8787"),
            entry("lab", "quic://lab.example:8788"),
        ];
        assert_eq!(
            find_entry(&entries, &target("phall@studio")).map(|e| e.name.as_str()),
            Some("phall@studio")
        );
        // `host add` registers the bare host; `me@mini` still finds it.
        assert_eq!(
            find_entry(&entries, &target("me@mini")).map(|e| e.name.as_str()),
            Some("mini")
        );
        // Endpoint host is the widest match.
        assert_eq!(
            find_entry(&entries, &target("lab.example")).map(|e| e.name.as_str()),
            Some("lab")
        );
        assert!(find_entry(&entries, &target("elsewhere")).is_none());
    }

    #[test]
    fn explicit_port_overrides_the_dial_but_ssh_is_refused() {
        let (endpoint, transport) =
            classify("quic://mini:8788", &target("mini:9999")).expect("quic");
        assert_eq!(endpoint, "quic://mini:9999");
        assert_eq!(transport, Transport::Quic("mini:9999".to_owned()));
        let (endpoint, _) = classify("wss://mini:8787", &target("mini:9999")).expect("wss");
        assert_eq!(endpoint, "wss://mini:9999");
        let (untouched, _) = classify("quic://mini:9999", &target("mini")).expect("portless");
        assert_eq!(untouched, "quic://mini:9999");
        let ssh = classify("ssh://mini", &target("mini")).expect_err("ssh");
        assert!(ssh.contains("phux --remote"), "{ssh}");
        assert!(classify("http://mini", &target("mini")).is_err());
    }

    #[test]
    fn resolves_through_the_cli_registry_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let token = dir.path().join("mini.token");
        std::fs::write(&token, "# minted by phux pair\n\n  abcd01  \n").expect("token");
        let config = dir.path().join("config.toml");
        std::fs::write(
            &config,
            format!(
                "[[remote]]\nname = \"mini\"\nendpoint = \"quic://127.0.0.1:8788\"\n\
                 token-file = \"{}\"\ncert-fingerprint = \"{}\"\nsession = \"work\"\n",
                token.display(),
                "ab".repeat(32)
            ),
        )
        .expect("config");
        let resolved = resolve("me@mini", Some(&config)).expect("resolve");
        assert_eq!(resolved.name, "mini");
        assert_eq!(resolved.endpoint, "quic://127.0.0.1:8788");
        assert_eq!(resolved.session.as_deref(), Some("work"));
        // Only the path: resolving for display never reads the secret.
        assert_eq!(resolved.token_file.as_deref(), Some(token.as_path()));
        assert!(!format!("{resolved:?}").contains("abcd01"));
        std::fs::remove_file(&token).expect("remove token");
        assert!(
            resolve("me@mini", Some(&config)).is_ok(),
            "a missing token file is the dial's problem, not resolution's"
        );

        let missing = resolve("studio", Some(&config)).expect_err("unregistered");
        assert!(missing.contains("phux --remote studio"), "{missing}");
        // An absent config file is an empty registry, not an I/O failure.
        let empty = resolve("mini", Some(&dir.path().join("absent.toml"))).expect_err("empty");
        assert!(empty.contains("not a registered host"), "{empty}");
    }

    /// The registry's enrolled client certificate is what the dial presents;
    /// an entry without one presents none, and half of one is refused.
    #[test]
    fn resolution_carries_the_enrolled_client_identity() {
        let dir = tempfile::tempdir().expect("tempdir");
        let config = dir.path().join("config.toml");
        let pin = "ab".repeat(32);
        std::fs::write(
            &config,
            format!(
                "[[remote]]\nname = \"mini\"\nendpoint = \"quic://127.0.0.1:8788\"\n\
                 cert-fingerprint = \"{pin}\"\nclient-cert = \"/s/mini.pem\"\n\
                 client-key = \"/s/mini.key\"\n\
                 [[remote]]\nname = \"bare\"\nendpoint = \"quic://127.0.0.1:8789\"\n\
                 [[remote]]\nname = \"half\"\nendpoint = \"quic://127.0.0.1:8790\"\n\
                 client-cert = \"/s/half.pem\"\n"
            ),
        )
        .expect("config");
        let resolved = resolve("mini", Some(&config)).expect("mini");
        assert_eq!(
            resolved.client_identity,
            TlsClientIdentity::PemFiles {
                certificate: PathBuf::from("/s/mini.pem"),
                private_key: PathBuf::from("/s/mini.key"),
            }
        );
        let target = crate::connection::Target::from(resolved);
        assert!(matches!(
            target.client_identity,
            TlsClientIdentity::PemFiles { .. }
        ));
        assert_eq!(
            resolve("bare", Some(&config))
                .expect("bare")
                .client_identity,
            TlsClientIdentity::None
        );
        let half = resolve("half", Some(&config)).expect_err("half");
        assert!(half.contains("together"), "{half}");
    }

    #[test]
    fn duplicate_names_are_ambiguous_not_first_wins() {
        let dir = tempfile::tempdir().expect("tempdir");
        let config = dir.path().join("config.toml");
        std::fs::write(
            &config,
            "[[remote]]\nname = \"mini\"\nendpoint = \"ws://127.0.0.1:1\"\n\
             [[remote]]\nname = \"mini\"\nendpoint = \"ws://127.0.0.1:2\"\n",
        )
        .expect("config");
        let err = resolve("mini", Some(&config)).expect_err("duplicate");
        assert!(err.contains("twice"), "{err}");
    }

    #[test]
    fn unreadable_token_names_the_path_and_never_the_bytes() {
        let err = read_token(Some(Path::new("/nonexistent/phux/token"))).expect_err("missing");
        assert!(err.contains("/nonexistent/phux/token"), "{err}");
        assert_eq!(read_token(None), Ok(None));
    }
}
