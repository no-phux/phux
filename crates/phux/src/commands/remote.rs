//! The remote half of the machine registries (ADR-0055, ADR-0066): a
//! `[[remote]]` entry stores the endpoint, the certificate pin, and a token-file
//! path once, so `phux attach NAME` resolves through it. The verbs live in
//! [`super::host`].
//!
//! An entry may also remember `ssh` (the destination it was enrolled through,
//! used to restart a stopped server), `direct` (a paired `quic://` endpoint
//! kept beside an `ssh://` one, tried first and promoted once it answers), and
//! `client-cert` / `client-key` (the workload client certificate `phux host
//! add` enrolled, ADR-0116, presented on every TLS dial to this remote).
//!
//! Endpoint schemes: `ssh://HOST` (no pairing; attach re-execs `ssh -t HOST phux
//! attach`), `quic://HOST:PORT` (token and pin, ADR-0031), and `wss://HOST:PORT`
//! (the same, where UDP is blocked). A remote is a server this consumer dials
//! for itself; a satellite (`[[satellites]]`) is one a hub dials for its users.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use phux_config::loader as config_loader;
use phux_dial::TlsClientIdentity;
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
    /// The ssh destination the entry was enrolled through, when it was.
    pub(crate) ssh: Option<String>,
    /// A paired direct endpoint kept beside an `ssh://` route, when the
    /// direct route did not answer at enrollment.
    pub(crate) direct: Option<String>,
    /// The enrolled workload client certificate chain. Public; a path.
    pub(crate) client_cert: Option<PathBuf>,
    /// Its private key: the path is displayable, the bytes behind it are the
    /// secret and are read only by the TLS stack at dial time.
    pub(crate) client_key: Option<PathBuf>,
}

impl RemoteEntry {
    /// The TLS client identity to present when dialing this remote: the
    /// enrolled certificate, required-paired when `PHUX_WORKLOAD_REQUIRE_PAIRED`
    /// is set, or `None` when the entry enrolled none (the dialer then reads
    /// `PHUX_WORKLOAD_CERT` / `PHUX_WORKLOAD_KEY` as before).
    ///
    /// # Errors
    ///
    /// Half an identity or a relative path, which would otherwise dial
    /// without the certificate the operator enrolled.
    pub(crate) fn client_identity(&self) -> Result<Option<TlsClientIdentity>, String> {
        let paths = phux_config::remote::client_identity_paths(
            &self.name,
            self.client_cert.as_deref(),
            self.client_key.as_deref(),
        )?;
        let require_paired = std::env::var_os(phux_dial::tls::REQUIRE_PAIRED_ENV).is_some();
        Ok(paths.map(|(certificate, private_key)| {
            if require_paired {
                TlsClientIdentity::RequirePaired {
                    certificate,
                    private_key,
                }
            } else {
                TlsClientIdentity::PemFiles {
                    certificate,
                    private_key,
                }
            }
        }))
    }

    /// Warn on stderr, once per process and remote, when the enrolled
    /// workload client certificate is due for renewal (expired, unreadable,
    /// or within [`super::enroll::RENEW_WITHIN_SECONDS`] of expiry). The dial
    /// goes ahead regardless: the far host decides admission.
    pub(crate) fn warn_if_renewal_due(&self) {
        use std::sync::{Mutex, PoisonError};
        static WARNED: Mutex<BTreeSet<String>> = Mutex::new(BTreeSet::new());

        let Some(cert) = self.client_cert.as_deref() else {
            return;
        };
        let Some(detail) = super::enroll::renewal_due(cert, chrono::Utc::now().timestamp()) else {
            return;
        };
        let first = WARNED
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(self.name.clone());
        if first {
            eprintln!(
                "phux: warning: {}: the workload client certificate {detail}; renew it with `phux host renew {}`",
                self.name, self.name
            );
        }
    }

    /// The enrolled identity's paths, for carrying it into a rewritten entry.
    pub(crate) fn client_identity_paths(&self) -> Option<(&Path, &Path)> {
        self.client_cert.as_deref().zip(self.client_key.as_deref())
    }

    /// The destination to hand `ssh` when this entry's server needs starting:
    /// the enrolled one, else an `ssh://` endpoint's host, else the entry name.
    pub(crate) fn ssh_destination(&self) -> String {
        if let Some(ssh) = &self.ssh {
            return ssh.clone();
        }
        if let Ok(Endpoint::Ssh(host)) = Endpoint::parse(&self.endpoint) {
            return host;
        }
        self.name.clone()
    }
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
    /// Classify a registry endpoint URI, so `phux host add` rejects a typo up
    /// front.
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
    toml_registry::reject_duplicate_names(cfg.remote.iter().map(|r| r.name.as_str()), "remote")?;
    Ok(cfg
        .remote
        .into_iter()
        .enumerate()
        .map(|(index, remote)| RemoteEntry {
            index,
            name: remote.name,
            endpoint: remote.endpoint,
            token_file: remote.token_file,
            cert_fingerprint: remote.cert_fingerprint,
            session: remote.session,
            ssh: remote.ssh,
            direct: remote.direct,
            client_cert: remote.client_cert,
            client_key: remote.client_key,
        })
        .collect())
}

/// Look up one remote by name. A config failure is also `None`: on the
/// `phux attach NAME` hot path an unreadable config falls through to the local
/// session.
pub(crate) fn find(name: &str) -> Option<RemoteEntry> {
    load_registry()
        .ok()?
        .into_iter()
        .find(|entry| entry.name == name)
}

/// Validated fields for a new or updated entry.
#[derive(Debug, Clone)]
pub(crate) struct NewRemote {
    pub(crate) name: String,
    pub(crate) endpoint: String,
    pub(crate) token_file: Option<PathBuf>,
    pub(crate) cert_fingerprint: Option<String>,
    pub(crate) session: Option<String>,
    pub(crate) ssh: Option<String>,
    pub(crate) direct: Option<String>,
    pub(crate) client_cert: Option<PathBuf>,
    pub(crate) client_key: Option<PathBuf>,
}

impl NewRemote {
    /// Every field of a registered entry, revalidated, so one field can be
    /// replaced without dropping the others (`add_or_update` writes the
    /// whole entry). The client identity is not carried: the caller sets it.
    pub(crate) fn from_entry(entry: &RemoteEntry) -> Result<Self, String> {
        Self::new(
            &entry.name,
            &entry.endpoint,
            entry.token_file.as_deref(),
            entry.cert_fingerprint.as_deref(),
            entry.session.as_deref(),
        )?
        .with_ssh(entry.ssh.as_deref())
        .with_direct(entry.direct.as_deref())
    }

    /// Record the enrolled workload client identity (both absolute paths),
    /// or none. Half an identity is refused like any other invalid field.
    pub(crate) fn with_client_identity(
        mut self,
        identity: Option<(&Path, &Path)>,
    ) -> Result<Self, String> {
        let paths = phux_config::remote::client_identity_paths(
            &self.name,
            identity.map(|(cert, _)| cert),
            identity.map(|(_, key)| key),
        )?;
        (self.client_cert, self.client_key) = paths.unzip();
        Ok(self)
    }

    /// Remember the ssh destination the entry was enrolled through.
    pub(crate) fn with_ssh(mut self, ssh: Option<&str>) -> Self {
        self.ssh = ssh
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_owned);
        self
    }

    /// Keep a paired direct endpoint beside an `ssh://` route. Validated
    /// like an endpoint, and dropped when the entry's own endpoint is
    /// already direct: it would only duplicate it.
    pub(crate) fn with_direct(mut self, direct: Option<&str>) -> Result<Self, String> {
        self.direct = match direct.map(str::trim).filter(|s| !s.is_empty()) {
            Some(direct) if self.endpoint.starts_with("ssh://") => match Endpoint::parse(direct)? {
                Endpoint::Quic(_) | Endpoint::Ws(_) => Some(direct.to_owned()),
                Endpoint::Ssh(_) => {
                    return Err(format!(
                        "direct endpoint {direct} must be quic:// or wss://, not ssh://"
                    ));
                }
            },
            _ => None,
        };
        Ok(self)
    }

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
                 or let `phux host add HOST` fetch it over ssh)"
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
            ssh: None,
            direct: None,
            client_cert: None,
            client_key: None,
        })
    }
}

/// A remote name is what the operator types after `phux attach`, so it must
/// not collide with the selector grammar's sigils.
pub(crate) fn validate_name(name: &str) -> Result<String, String> {
    toml_registry::plain_machine_name(name)
        .filter(|trimmed| !trimmed.starts_with(['@', '#', '.', '=']))
        .map(str::to_owned)
        .ok_or_else(|| {
            "remote name must be non-empty, must not contain '/' or ':', and must not start \
             with a selector sigil (@ # . =)"
                .to_owned()
        })
}

/// The token file is read relative to wherever `phux attach` runs, so it must
/// be absolute (existence is not required: enrollment may write it later).
fn validate_token_file(path: &Path) -> Result<PathBuf, String> {
    toml_registry::validate_token_file(path, "remote")
}

/// Insert a new entry, or replace an existing one with the same name.
pub(crate) fn add_or_update(new: &NewRemote) -> Result<(), String> {
    toml_registry::upsert_machine(
        &config_loader::config_path(),
        KEY,
        || {
            Ok(load_registry()?
                .into_iter()
                .find(|entry| entry.name == new.name)
                .map(|entry| entry.index))
        },
        |table| fill_table(table, new),
    )
}

/// Remove the captured entry under the shared read/modify/publish lock.
pub(crate) fn remove_entry(entry: &RemoteEntry) -> Result<(), String> {
    toml_registry::remove_machine_entry(
        &config_loader::config_path(),
        KEY,
        &entry.name,
        &entry.endpoint,
    )
}

/// Write every field, clearing omitted ones, so re-adding never leaves stale
/// auth material pointing at a new endpoint.
fn fill_table(table: &mut Table, new: &NewRemote) {
    let display = |path: &Option<PathBuf>| path.as_ref().map(|path| path.display().to_string());
    table.insert("name", value(&new.name));
    table.insert("endpoint", value(&new.endpoint));
    toml_registry::set_or_remove(table, "token-file", display(&new.token_file));
    toml_registry::set_or_remove(table, "cert-fingerprint", new.cert_fingerprint.as_ref());
    toml_registry::set_or_remove(table, "session", new.session.as_ref());
    toml_registry::set_or_remove(table, "ssh", new.ssh.as_ref());
    toml_registry::set_or_remove(table, "direct", new.direct.as_ref());
    toml_registry::set_or_remove(table, "client-cert", display(&new.client_cert));
    toml_registry::set_or_remove(table, "client-key", display(&new.client_key));
}

/// Read the bearer token for an entry, if it declares a token file.
///
/// The token is a secret, so failures name the path and never echo bytes;
/// the file rule is the runtime's, shared with every embedder.
pub(crate) fn read_token(entry: &RemoteEntry) -> Result<Option<String>, String> {
    phux_client_runtime::target::read_token(entry.token_file.as_deref())
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
            ssh: None,
            direct: None,
            client_cert: None,
            client_key: None,
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

    /// An enrolled identity is two absolute paths or nothing; half of one
    /// is refused at registration and at dial time alike.
    #[test]
    fn a_client_identity_is_whole_and_absolute() {
        let fp = "ab".repeat(32);
        let new = || NewRemote::new("mini", "quic://mini:8788", None, Some(&fp), None);
        let cert = Path::new("/state/remotes/mini.client.pem");
        let key = Path::new("/state/remotes/mini.client.key");
        let whole = new()
            .expect("entry")
            .with_client_identity(Some((cert, key)))
            .expect("whole identity");
        assert_eq!(whole.client_cert.as_deref(), Some(cert));
        assert_eq!(whole.client_key.as_deref(), Some(key));
        let none = new()
            .expect("entry")
            .with_client_identity(None)
            .expect("no identity");
        assert!(none.client_cert.is_none() && none.client_key.is_none());
        assert!(
            new()
                .expect("entry")
                .with_client_identity(Some((Path::new("rel.pem"), key)))
                .is_err()
        );

        let mut entry = entry_with_token(None);
        assert_eq!(entry.client_identity(), Ok(None));
        entry.client_cert = Some(cert.to_path_buf());
        let half = entry.client_identity().expect_err("half an identity");
        assert!(half.contains("together"), "{half}");
        entry.client_key = Some(key.to_path_buf());
        assert!(matches!(
            entry.client_identity(),
            Ok(Some(
                phux_dial::TlsClientIdentity::PemFiles { .. }
                    | phux_dial::TlsClientIdentity::RequirePaired { .. }
            ))
        ));
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

    /// `direct` only means something beside an `ssh://` route: it is the
    /// paired endpoint a later attach promotes. Beside a direct endpoint it
    /// would only duplicate it, so it is dropped rather than stored.
    #[test]
    fn direct_candidate_is_kept_only_beside_an_ssh_route() {
        let fp = "ab".repeat(32);
        let ssh = NewRemote::new("mini", "ssh://me@mini", None, Some(&fp), None)
            .expect("ssh entry")
            .with_direct(Some("quic://100.64.0.7:8788"))
            .expect("a quic candidate is valid");
        assert_eq!(ssh.direct.as_deref(), Some("quic://100.64.0.7:8788"));

        let quic = NewRemote::new("mini", "quic://mini:8788", None, Some(&fp), None)
            .expect("quic entry")
            .with_direct(Some("quic://100.64.0.7:8788"))
            .expect("dropped, not refused");
        assert_eq!(quic.direct, None, "already direct: nothing to promote");

        let err = NewRemote::new("mini", "ssh://mini", None, None, None)
            .expect("ssh entry")
            .with_direct(Some("ssh://mini"))
            .expect_err("an ssh candidate is not a direct route");
        assert!(err.contains("quic://"), "got {err}");
        assert!(
            NewRemote::new("mini", "ssh://mini", None, None, None)
                .expect("ssh entry")
                .with_direct(Some("nonsense"))
                .is_err(),
            "a candidate is validated like an endpoint"
        );
    }

    /// The destination a repair sshes to: the enrolled one first, then the
    /// host of an `ssh://` endpoint, then the name itself.
    #[test]
    fn ssh_destination_prefers_what_the_entry_remembers() {
        let mut entry = entry_with_token(None);
        assert_eq!(
            entry.ssh_destination(),
            "mini",
            "a bare name is an ssh alias"
        );
        entry.endpoint = "ssh://me@box".to_owned();
        assert_eq!(entry.ssh_destination(), "me@box");
        entry.ssh = Some("me@mini.lan".to_owned());
        assert_eq!(entry.ssh_destination(), "me@mini.lan");
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
