//! The remote half of the machine registries (ADR-0055, ADR-0066).
//!
//! `phux attach mini` should not require the operator to retype a 64-hex
//! token and a 64-hex fingerprint. A `[[remote]]` entry stores the endpoint,
//! the certificate pin, and a path to the token file once; attach resolves
//! the name through it.
//!
//! The user-facing verbs live in [`super::host`]: `phux host add|ls|rm`
//! (role `remote`, the default) operates on this registry. The former
//! `phux remote` verb tree was absorbed into `phux host` (ADR-0066) and
//! removed in v0.12.1 once its deprecation window closed (phux-dpjf).
//!
//! Three endpoint schemes, in increasing order of setup cost:
//!
//! * `ssh://HOST` — no pairing at all. Attach re-execs `ssh -t HOST phux
//!   attach`, so the session still lives on the remote server and survives
//!   the ssh connection dropping. This is the zero-ceremony path, and it is
//!   what `phux host enroll` falls back to when a host has no reachable
//!   listener.
//! * `quic://HOST:PORT` — the real remote transport (ADR-0031). Needs a
//!   token and a pin.
//! * `wss://HOST:PORT` — the same, for networks that block UDP.
//!
//! The registry is a sibling of `[[satellites]]`, not a reuse of it: a
//! satellite is a peer a *hub* dials for its users, a remote is a server
//! *this consumer* dials for itself. See [`phux_config::remote`].

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use phux_config::loader as config_loader;
use toml_edit::{Table, value};

use super::toml_registry;

/// The `config.toml` key this registry owns.
const KEY: &str = "remote";

/// A registered remote, with its position in the array of tables so an
/// update can rewrite the right entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RemoteEntry {
    pub(crate) index: usize,
    pub(crate) name: String,
    pub(crate) endpoint: String,
    /// Path to the pairing-token file. The path is displayable; the token
    /// bytes behind it are the secret and are read only at dial time.
    pub(crate) token_file: Option<PathBuf>,
    /// TLS certificate SHA-256 pin, as printed by `phux pair`. Not a secret.
    pub(crate) cert_fingerprint: Option<String>,
    /// Session to request on arrival, when the operator pinned one.
    pub(crate) session: Option<String>,
}

/// The transport a validated endpoint names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Endpoint {
    /// `quic://HOST:PORT`, carrying the `HOST:PORT` the QUIC dialer wants.
    Quic(String),
    /// `ws://` or `wss://`, carrying the URL unchanged.
    Ws(String),
    /// `ssh://HOST`, carrying the ssh destination.
    Ssh(String),
}

impl Endpoint {
    /// Classify a registry endpoint URI.
    ///
    /// Validation lives here rather than at dial time so `phux host add`
    /// rejects a typo while the operator is still looking at what they
    /// typed.
    pub(crate) fn parse(endpoint: &str) -> Result<Self, String> {
        let trimmed = endpoint.trim();
        if let Some(rest) = trimmed.strip_prefix("quic://") {
            if rest.is_empty() || !rest.contains(':') {
                return Err(
                    "quic:// endpoint needs HOST:PORT (e.g. quic://mini.tailnet.ts.net:8788)"
                        .to_owned(),
                );
            }
            return Ok(Self::Quic(rest.to_owned()));
        }
        if trimmed.starts_with("wss://") || trimmed.starts_with("ws://") {
            return Ok(Self::Ws(trimmed.to_owned()));
        }
        if let Some(rest) = trimmed.strip_prefix("ssh://") {
            if rest.is_empty() {
                return Err("ssh:// endpoint needs a host (e.g. ssh://mini)".to_owned());
            }
            return Ok(Self::Ssh(rest.to_owned()));
        }
        Err(format!(
            "remote endpoint {trimmed:?} must start with quic://, wss://, ws://, or ssh://"
        ))
    }

    /// Whether this transport authenticates with a pairing token and a pin.
    /// `ssh://` does not: it rides the operator's existing ssh trust.
    const fn needs_pairing(&self) -> bool {
        matches!(self, Self::Quic(_) | Self::Ws(_))
    }
}

/// Read every `[[remote]]` entry, rejecting a duplicate name — two entries
/// with one name make `phux attach NAME` ambiguous, and silently picking the
/// first would be a trap.
pub(crate) fn load_registry() -> Result<Vec<RemoteEntry>, String> {
    let cfg = config_loader::load().map_err(|err| err.to_string())?;
    let mut seen = BTreeSet::new();
    let mut entries = Vec::with_capacity(cfg.remote.len());
    for (index, remote) in cfg.remote.into_iter().enumerate() {
        if !seen.insert(remote.name.clone()) {
            return Err(format!("duplicate remote name {:?}", remote.name));
        }
        entries.push(RemoteEntry {
            index,
            name: remote.name,
            endpoint: remote.endpoint,
            token_file: remote.token_file,
            cert_fingerprint: remote.cert_fingerprint,
            session: remote.session,
        });
    }
    Ok(entries)
}

/// Look up one remote by name, or `None` when the registry has no such
/// entry.
///
/// Returns `None` — not an error — on a *config* failure too, because this
/// is called on the `phux attach NAME` hot path where an unreadable config
/// must fall through to the local-session interpretation rather than block
/// an attach.
pub(crate) fn find(name: &str) -> Option<RemoteEntry> {
    load_registry()
        .ok()?
        .into_iter()
        .find(|entry| entry.name == name)
}

/// Validated fields for a new or updated entry.
///
/// Fields are `pub(crate)` so `phux host add --role remote` (the ADR-0066
/// umbrella) can report what it just registered without re-deriving the
/// trimmed name from raw input.
#[derive(Debug, Clone)]
pub(crate) struct NewRemote {
    pub(crate) name: String,
    pub(crate) endpoint: String,
    pub(crate) token_file: Option<PathBuf>,
    pub(crate) cert_fingerprint: Option<String>,
    pub(crate) session: Option<String>,
}

impl NewRemote {
    /// Validate every field up front, so a rejected entry never reaches the
    /// config file half-written.
    pub(crate) fn new(
        name: &str,
        endpoint: &str,
        token_file: Option<&Path>,
        cert_fingerprint: Option<&str>,
        session: Option<&str>,
    ) -> Result<Self, String> {
        let parsed = Endpoint::parse(endpoint)?;
        let cert_fingerprint = cert_fingerprint
            .map(|fp| toml_registry::validate_fingerprint(fp, "remote"))
            .transpose()?;

        // Fail closed, matching ADR-0038's posture: a paired transport with
        // no pin would be refused at dial time anyway, and refusing at
        // registration puts the error where the operator can act on it.
        if parsed.needs_pairing() && cert_fingerprint.is_none() {
            return Err(format!(
                "{endpoint} needs --cert-fingerprint (run `phux pair` on the remote host, \
                 or let `phux host enroll` fetch it over ssh)"
            ));
        }

        Ok(Self {
            name: validate_name(name)?,
            endpoint: endpoint.trim().to_owned(),
            token_file: token_file.map(validate_token_file).transpose()?,
            cert_fingerprint,
            session: session
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_owned),
        })
    }
}

/// A remote name is what the operator types after `phux attach`, so it must
/// not collide with the selector grammar's sigils.
fn validate_name(name: &str) -> Result<String, String> {
    let trimmed = name.trim();
    if trimmed.is_empty()
        || trimmed.contains('/')
        || trimmed.contains(':')
        || trimmed.starts_with(['@', '#', '.', '='])
    {
        return Err(
            "remote name must be non-empty, must not contain '/' or ':', and must not start \
             with a selector sigil (@ # . =)"
                .to_owned(),
        );
    }
    Ok(trimmed.to_owned())
}

/// The token file is read at dial time from whatever directory the operator
/// happened to run `phux attach` in, so a relative path would resolve
/// somewhere else. Require absolute. Existence is not required: enrollment
/// may write the config entry before the token lands.
fn validate_token_file(path: &Path) -> Result<PathBuf, String> {
    if path.as_os_str().is_empty() || !path.is_absolute() {
        return Err(format!(
            "remote token-file must be an absolute path (got {})",
            path.display()
        ));
    }
    Ok(path.to_path_buf())
}

/// Insert a new entry, or replace an existing one with the same name.
pub(crate) fn add_or_update(new: &NewRemote) -> Result<(), String> {
    let config_path = config_loader::config_path();
    toml_registry::reject_symlink(&config_path)?;
    let mut doc = toml_registry::read_document(&config_path)?;

    let existing = load_registry()?
        .into_iter()
        .find(|entry| entry.name == new.name);
    if let Some(entry) = existing {
        let table = toml_registry::table_mut(&mut doc, KEY, entry.index)?;
        fill_table(table, new);
    } else {
        let mut table = Table::new();
        fill_table(&mut table, new);
        toml_registry::tables_mut(&mut doc, KEY)?.push(table);
    }
    toml_registry::write_document(&config_path, &doc)
}

/// Remove the entry at `index`.
pub(crate) fn remove_at(index: usize) -> Result<(), String> {
    let config_path = config_loader::config_path();
    toml_registry::reject_symlink(&config_path)?;
    let mut doc = toml_registry::read_document(&config_path)?;
    let tables = toml_registry::tables_mut(&mut doc, KEY)?;
    if index >= tables.len() {
        return Err(format!("remote registry index {index} disappeared"));
    }
    tables.remove(index);
    toml_registry::write_document(&config_path, &doc)
}

/// Write every field, clearing the ones this entry omits.
///
/// `add` replaces the whole entry, so an absent flag removes the key rather
/// than leaving stale auth material pointed at a new endpoint.
fn fill_table(table: &mut Table, new: &NewRemote) {
    table.insert("name", value(&new.name));
    table.insert("endpoint", value(&new.endpoint));
    match &new.token_file {
        Some(path) => {
            table.insert("token-file", value(path.display().to_string()));
        }
        None => {
            table.remove("token-file");
        }
    }
    match &new.cert_fingerprint {
        Some(fingerprint) => {
            table.insert("cert-fingerprint", value(fingerprint));
        }
        None => {
            table.remove("cert-fingerprint");
        }
    }
    match &new.session {
        Some(session) => {
            table.insert("session", value(session));
        }
        None => {
            table.remove("session");
        }
    }
}

/// Read the bearer token for an entry, if it declares a token file.
///
/// The token is a secret, so failures name the path and never echo bytes.
pub(crate) fn read_token(entry: &RemoteEntry) -> Result<Option<String>, String> {
    let Some(path) = &entry.token_file else {
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

#[cfg(test)]
mod tests {
    use super::{Endpoint, NewRemote, RemoteEntry, read_token, validate_name, validate_token_file};
    use std::path::{Path, PathBuf};

    fn entry_with_token(path: Option<PathBuf>) -> RemoteEntry {
        RemoteEntry {
            index: 0,
            name: "mini".to_owned(),
            endpoint: "quic://mini:8788".to_owned(),
            token_file: path,
            cert_fingerprint: None,
            session: None,
        }
    }

    #[test]
    fn endpoint_parse_covers_the_three_schemes() {
        assert_eq!(
            Endpoint::parse("quic://mini.ts.net:8788"),
            Ok(Endpoint::Quic("mini.ts.net:8788".to_owned()))
        );
        assert_eq!(
            Endpoint::parse("wss://mini.ts.net:8787"),
            Ok(Endpoint::Ws("wss://mini.ts.net:8787".to_owned()))
        );
        assert_eq!(
            Endpoint::parse("ssh://mini"),
            Ok(Endpoint::Ssh("mini".to_owned()))
        );
        // Whitespace from a paste is trimmed, not rejected.
        assert_eq!(
            Endpoint::parse("  ssh://mini  "),
            Ok(Endpoint::Ssh("mini".to_owned()))
        );
    }

    #[test]
    fn endpoint_parse_rejects_typos_at_registration_time() {
        assert!(Endpoint::parse("mini:8788").is_err(), "bare host:port");
        assert!(Endpoint::parse("https://mini").is_err(), "wrong scheme");
        assert!(Endpoint::parse("quic://mini").is_err(), "quic needs a port");
        assert!(Endpoint::parse("ssh://").is_err(), "ssh needs a host");
    }

    #[test]
    fn ssh_needs_no_pairing_but_quic_and_wss_do() {
        assert!(!Endpoint::parse("ssh://mini").expect("ssh").needs_pairing());
        assert!(
            Endpoint::parse("quic://mini:1")
                .expect("quic")
                .needs_pairing()
        );
        assert!(
            Endpoint::parse("wss://mini:1")
                .expect("wss")
                .needs_pairing()
        );
    }

    #[test]
    fn paired_transports_fail_closed_without_a_pin() {
        // ADR-0038's posture: refuse at registration, where the operator can
        // still see what they pasted.
        let err = NewRemote::new("mini", "quic://mini:8788", None, None, None)
            .expect_err("unpinned quic must be refused");
        assert!(err.contains("--cert-fingerprint"), "got {err}");

        // ssh:// rides existing ssh trust, so it needs no pin.
        assert!(NewRemote::new("mini", "ssh://mini", None, None, None).is_ok());

        // With a pin, quic is accepted.
        let fp = "ab".repeat(32);
        assert!(NewRemote::new("mini", "quic://mini:8788", None, Some(&fp), None).is_ok());
    }

    #[test]
    fn names_must_not_collide_with_the_selector_grammar() {
        // `phux attach @3` already means a terminal id; a remote named `@3`
        // would make that ambiguous.
        for bad in ["@id", "#tag", ".", "=", "a/b", "a:b", "", "  "] {
            assert!(validate_name(bad).is_err(), "{bad:?} must be rejected");
        }
        assert_eq!(validate_name("  mini  ").as_deref(), Ok("mini"));
        assert_eq!(
            validate_name("studio-mini_2").as_deref(),
            Ok("studio-mini_2")
        );
    }

    #[test]
    fn token_file_must_be_absolute() {
        // A relative path resolves against whatever cwd `phux attach` ran
        // in, which is not where enrollment wrote the token.
        assert!(validate_token_file(Path::new("tokens")).is_err());
        assert!(validate_token_file(Path::new("")).is_err());
        assert!(validate_token_file(Path::new("/abs/tokens")).is_ok());
    }

    #[test]
    fn read_token_takes_the_first_meaningful_line() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("token");
        std::fs::write(&path, "# a comment\n\n  deadbeef  \nsecond\n").expect("write");
        assert_eq!(
            read_token(&entry_with_token(Some(path))).expect("read"),
            Some("deadbeef".to_owned())
        );
    }

    #[test]
    fn read_token_is_none_without_a_token_file() {
        assert_eq!(read_token(&entry_with_token(None)).expect("read"), None);
    }

    #[test]
    fn read_token_names_the_path_it_could_not_read() {
        let err = read_token(&entry_with_token(Some(PathBuf::from("/nope/missing"))))
            .expect_err("missing file must error");
        assert!(err.contains("/nope/missing"), "got {err}");

        let dir = tempfile::tempdir().expect("tempdir");
        let empty = dir.path().join("empty");
        std::fs::write(&empty, "# only a comment\n").expect("write");
        let err = read_token(&entry_with_token(Some(empty))).expect_err("no token line");
        assert!(err.contains("no token line"), "got {err}");
    }
}
