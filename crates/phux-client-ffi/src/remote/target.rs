//! Registry resolution for a remote host: rung 1 of the CLI's `phux --remote`
//! ladder (ADR-0093), for native embedders.
//!
//! The `[[remote]]` registry in the phux `config.toml` (ADR-0055) is the only
//! registry. `phux host add|enroll` and `phux --remote` write it; this module
//! only reads it, through the same `phux-config` schema and loader the CLI
//! uses, so an entry means one thing to both.
//!
//! The target grammar and the three-way match mirror
//! `crates/phux/src/commands/remote_target.rs`. They are restated here rather
//! than imported because that module is private to the CLI binary; the tests
//! below pin the same spellings the CLI's tests pin, so a drift fails here.
//!
//! Deliberately absent: rungs 2-4. Pairing (a pasted connect code, or `phux
//! pair` over ssh) is an operator-present terminal act that writes a bearer
//! token to disk; an embedder that cannot find a registered host says so and
//! names the CLI command that pairs it.

use std::path::{Path, PathBuf};

use phux_config::RemoteConfigEntry;

/// The QUIC port a server auto-binds on its overlay address (ADR-0081), and
/// therefore the port a target with no `:PORT` means. Same constant as the
/// CLI's `remote_target::DEFAULT_QUIC_PORT`.
pub(crate) const DEFAULT_QUIC_PORT: u16 = 8788;

/// A parsed `[USER@]HOST[:PORT]` target. `user@` is a registry label, never a
/// wire identity (see the CLI module's header).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RemoteTarget {
    pub(crate) user: Option<String>,
    pub(crate) host: String,
    pub(crate) port: Option<u16>,
}

impl RemoteTarget {
    /// Parse `host`, `user@host`, `host:port`, `user@host:port`, and their
    /// bracketed-IPv6 spellings. A URI is refused rather than guessed at.
    pub(crate) fn parse(raw: &str) -> Result<Self, String> {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            return Err("enter a registered host, e.g. mini or me@mini".to_owned());
        }
        if trimmed.contains("://") {
            return Err(format!(
                "{trimmed:?} is a URI; register it with `phux host add NAME {trimmed}` and \
                 connect by NAME"
            ));
        }
        let (user, rest) = match trimmed.rsplit_once('@') {
            Some((user, rest)) => {
                if user.is_empty() {
                    return Err(format!("{trimmed:?} has an empty user"));
                }
                (Some(user.to_owned()), rest)
            }
            None => (None, trimmed),
        };
        let (host, port) = split_host_port(rest)?;
        if host.is_empty() {
            return Err(format!("{trimmed:?} has an empty host"));
        }
        if host.contains('/') || host.starts_with(['@', '#', '.', '=']) {
            return Err(format!(
                "host {host:?} must not contain '/' or start with a selector sigil (@ # . =)"
            ));
        }
        Ok(Self { user, host, port })
    }

    /// The registry key: the typed spelling minus any port.
    pub(crate) fn registry_name(&self) -> String {
        self.user
            .as_ref()
            .map_or_else(|| self.host.clone(), |user| format!("{user}@{}", self.host))
    }

    /// `HOST:PORT` to dial, bracketing an IPv6 literal.
    pub(crate) fn authority(&self) -> String {
        let port = self.port.unwrap_or(DEFAULT_QUIC_PORT);
        if self.host.contains(':') {
            format!("[{}]:{port}", self.host)
        } else {
            format!("{}:{port}", self.host)
        }
    }
}

fn split_host_port(rest: &str) -> Result<(String, Option<u16>), String> {
    if let Some(inner) = rest.strip_prefix('[') {
        return split_bracketed(rest, inner);
    }
    // A bare IPv6 literal has no port: `fd7a::1:8788` is ambiguous.
    if rest.matches(':').count() > 1 {
        return Ok((rest.to_owned(), None));
    }
    match rest.split_once(':') {
        Some((host, port)) => Ok((host.to_owned(), Some(parse_port(port)?))),
        None => Ok((rest.to_owned(), None)),
    }
}

/// `[v6]` or `[v6]:port`: the brackets make the port unambiguous.
fn split_bracketed(rest: &str, inner: &str) -> Result<(String, Option<u16>), String> {
    let (host, tail) = inner
        .split_once(']')
        .ok_or_else(|| format!("{rest:?} has an unclosed '['"))?;
    if tail.is_empty() {
        return Ok((host.to_owned(), None));
    }
    let port = tail
        .strip_prefix(':')
        .ok_or_else(|| format!("{rest:?} has trailing text after ']'"))?;
    Ok((host.to_owned(), Some(parse_port(port)?)))
}

fn parse_port(raw: &str) -> Result<u16, String> {
    raw.parse::<u16>()
        .ok()
        .filter(|port| *port != 0)
        .ok_or_else(|| format!("port {raw:?} must be 1..=65535"))
}

/// The registry entry that describes `target`: exact `user@host`, then the
/// bare host (what `phux host enroll` registers), then any entry whose
/// endpoint addresses that host. Same order as the CLI's `find_entry`.
pub(crate) fn find_entry<'a>(
    entries: &'a [RemoteConfigEntry],
    target: &RemoteTarget,
) -> Option<&'a RemoteConfigEntry> {
    let name = target.registry_name();
    entries
        .iter()
        .find(|entry| entry.name == name)
        .or_else(|| entries.iter().find(|entry| entry.name == target.host))
        .or_else(|| {
            entries
                .iter()
                .find(|entry| endpoint_host(&entry.endpoint).as_deref() == Some(&target.host))
        })
}

fn endpoint_host(endpoint: &str) -> Option<String> {
    let rest = endpoint.split_once("://").map(|(_, rest)| rest)?;
    let authority = rest.split(['/', '?']).next().unwrap_or(rest);
    let authority = authority.rsplit_once('@').map_or(authority, |(_, a)| a);
    if let Some(inner) = authority.strip_prefix('[') {
        return inner.split_once(']').map(|(host, _)| host.to_owned());
    }
    if authority.matches(':').count() > 1 {
        return Some(authority.to_owned());
    }
    Some(
        authority
            .split_once(':')
            .map_or(authority, |(host, _)| host)
            .to_owned(),
    )
}

/// The transport a registry endpoint names, restricted to what an embedder
/// can dial without a terminal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Transport {
    /// `quic://HOST:PORT`, carrying `HOST:PORT`.
    Quic(String),
    /// `ws://` or `wss://`, carrying the URL.
    Ws(String),
}

/// Classify an endpoint, applying an explicit `:PORT` from the target as a
/// per-dial override (the registry is never rewritten). Returns the effective
/// endpoint URI beside its transport.
pub(crate) fn classify(
    endpoint: &str,
    target: &RemoteTarget,
) -> Result<(String, Transport), String> {
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
             or give the host a direct listener with `phux host enroll`"
        ));
    }
    Err(format!(
        "registry endpoint {trimmed:?} must start with quic://, wss://, or ws://"
    ))
}

/// Read the bearer token behind an entry's `token-file`: the first line that
/// is neither blank nor a `#` comment. Same rule as the CLI's `read_token`.
/// Failures name the path and never echo token bytes.
pub(crate) fn read_token(path: Option<&Path>) -> Result<Option<String>, String> {
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

/// Everything a dial needs, resolved from the registry. Holds no secret: the
/// token file is only NAMED here, and the tunnel thread reads it just before
/// dialing, so resolving a host for display never touches the token.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Resolved {
    pub(crate) name: String,
    pub(crate) endpoint: String,
    pub(crate) session: Option<String>,
    pub(crate) transport: Transport,
    pub(crate) token_file: Option<PathBuf>,
    pub(crate) cert_fingerprint: Option<String>,
}

/// Resolve `raw` against the registry at `config_path`, or at the CLI's own
/// canonical path when none is given.
pub(crate) fn resolve(raw: &str, config_path: Option<&Path>) -> Result<Resolved, String> {
    let target = RemoteTarget::parse(raw)?;
    let path: PathBuf =
        config_path.map_or_else(phux_config::loader::config_path, Path::to_path_buf);
    let config = phux_config::loader::load_from(&path)
        .map_err(|err| format!("could not read the phux config {}: {err}", path.display()))?;
    reject_duplicates(&config.remote)?;
    let entry = find_entry(&config.remote, &target).ok_or_else(|| unregistered(&target))?;
    let (endpoint, transport) = classify(&entry.endpoint, &target)?;
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
         `phux --remote {name}` or `phux host enroll {name}`"
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
        // `enroll` registers the bare host; `me@mini` still finds it.
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
