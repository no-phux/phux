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
    /// The TLS server name a dial offers instead of the endpoint's host: the
    /// relay route when the endpoint is a relay (ADR-0149).
    pub(crate) tls_server_name: Option<String>,
    /// Session to request on arrival, when the operator pinned one.
    pub(crate) session: Option<String>,
    /// The ssh destination the entry was enrolled through, when it was.
    pub(crate) ssh: Option<String>,
    /// A paired direct endpoint kept beside an `ssh://` route, when the
    /// direct route did not answer at enrollment.
    pub(crate) direct: Option<String>,
    /// The `phux` to run over ssh there, when it is not plain `phux`.
    pub(crate) remote_phux: Option<String>,
    /// The enrolled workload client certificate chain. Public; a path.
    pub(crate) client_cert: Option<PathBuf>,
    /// Its private key: the path is displayable, the bytes behind it are the
    /// secret and are read only by the TLS stack at dial time.
    pub(crate) client_key: Option<PathBuf>,
}

impl RemoteEntry {
    /// The certificate-authority pin beside this entry's leaf pin
    /// (ADR-0153): the one `known-authorities` holds from pairing, if any.
    pub(crate) fn authority_pin(&self) -> phux_client_runtime::target::AuthorityPin {
        phux_client_runtime::target::AuthorityPin::from_store(
            &config_loader::config_path(),
            self.cert_fingerprint.as_deref(),
            self.tls_server_name.as_deref(),
        )
    }

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
            warn!(
                "{}: the workload client certificate {detail}; renew it with `phux host renew {}`",
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

    /// The `phux` to run over ssh there: the recorded one, else `phux`.
    pub(crate) fn remote_phux(&self) -> &str {
        self.remote_phux.as_deref().unwrap_or("phux")
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
            tls_server_name: remote.tls_server_name,
            session: remote.session,
            ssh: remote.ssh,
            direct: remote.direct,
            remote_phux: remote.remote_phux,
            client_cert: remote.client_cert,
            client_key: remote.client_key,
        })
        .collect())
}

/// The `known-authorities` store beside the registry this CLI reads and
/// writes (ADR-0153).
pub(crate) fn known_authorities() -> PathBuf {
    phux_config::known_authorities::path_beside(&config_loader::config_path())
}

/// Pin `authority` beside the leaf pin a pairing just registered for `name`.
/// A failure is a warning: the entry still pins the leaf, and nothing learns
/// the authority later (ADR-0179); re-pair to pin it.
/// For a relayed server, `route` keys the pin beside the relay's leaf.
pub(crate) fn pin_authority(
    name: &str,
    leaf: Option<&str>,
    route: Option<&str>,
    authority: Option<&str>,
) {
    let (Some(leaf), Some(authority)) = (leaf, authority) else {
        return;
    };
    if let Err(err) =
        phux_config::known_authorities::record(&known_authorities(), leaf, route, authority)
    {
        warn!(
            "{name}: could not pin its certificate authority ({err}); \
             the first connection pins it instead"
        );
    }
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
    pub(crate) tls_server_name: Option<String>,
    pub(crate) session: Option<String>,
    pub(crate) ssh: Option<String>,
    pub(crate) direct: Option<String>,
    pub(crate) remote_phux: Option<String>,
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
        .with_remote_phux(entry.remote_phux.as_deref())
        .with_direct(entry.direct.as_deref())?
        .with_tls_server_name(entry.tls_server_name.as_deref())
    }

    /// Offer `name` as the TLS server name on every dial of this entry: the
    /// relay route when the endpoint is a relay (ADR-0149). Validated as a
    /// DNS name; blank clears it. An `ssh://` endpoint has no TLS of its own
    /// to name, so it is refused there.
    pub(crate) fn with_tls_server_name(mut self, name: Option<&str>) -> Result<Self, String> {
        self.tls_server_name = match name.map(str::trim).filter(|name| !name.is_empty()) {
            None => None,
            Some(_) if self.endpoint.starts_with("ssh://") => {
                return Err(format!(
                    "{} rides ssh, which offers no TLS server name; register the relay's \
                     quic:// endpoint instead",
                    self.endpoint
                ));
            }
            Some(name) => Some(phux_client_runtime::target::validate_tls_server_name(name)?),
        };
        Ok(self)
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

    /// Remember the `phux` to run over ssh there; plain `phux` (the default)
    /// is not recorded.
    pub(crate) fn with_remote_phux(mut self, remote_phux: Option<&str>) -> Self {
        self.remote_phux = remote_phux
            .map(str::trim)
            .filter(|phux| !phux.is_empty() && *phux != "phux")
            .map(str::to_owned);
        self
    }

    /// Keep a paired direct endpoint beside an `ssh://` route, or a paired
    /// `quic://` one beside a `wss://` route; an attach tries it first and
    /// promotes it once it answers. Validated like an endpoint, and dropped
    /// where it could only duplicate or downgrade the entry's own endpoint.
    pub(crate) fn with_direct(mut self, direct: Option<&str>) -> Result<Self, String> {
        let over_wss = self.endpoint.starts_with("wss://");
        self.direct = match direct.map(str::trim).filter(|s| !s.is_empty()) {
            Some(direct) if self.endpoint.starts_with("ssh://") => match Endpoint::parse(direct)? {
                Endpoint::Quic(_) | Endpoint::Ws(_) => Some(direct.to_owned()),
                Endpoint::Ssh(_) => {
                    return Err(format!(
                        "direct endpoint {direct} must be quic:// or wss://, not ssh://"
                    ));
                }
            },
            Some(direct) if over_wss => match Endpoint::parse(direct)? {
                Endpoint::Quic(_) => Some(direct.to_owned()),
                Endpoint::Ws(_) | Endpoint::Ssh(_) => None,
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
            tls_server_name: None,
            ssh: None,
            direct: None,
            remote_phux: None,
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
    toml_registry::set_or_remove(table, "tls-server-name", new.tls_server_name.as_ref());
    toml_registry::set_or_remove(table, "session", new.session.as_ref());
    toml_registry::set_or_remove(table, "ssh", new.ssh.as_ref());
    toml_registry::set_or_remove(table, "direct", new.direct.as_ref());
    toml_registry::set_or_remove(table, "remote-phux", new.remote_phux.as_ref());
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
mod tests;
