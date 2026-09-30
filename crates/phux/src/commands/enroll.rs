//! The shared ssh middle of `phux host add HOST` (ADR-0055, ADR-0066,
//! ADR-0122): bring another machine's server up and pair with it over ssh.
//! Enrollment grants no authority ssh did not already grant; it removes the
//! hand-copying of a token and a fingerprint.
//!
//! Over one ssh channel: confirm `phux` is installed; make sure a server runs
//! and stays running (install the service, adopt a live server, or fall back to
//! `phux server --ensure`); run `phux pair --json` (migrating a legacy token
//! store once); probe the direct routes; and return the first that answers, or
//! `ssh://HOST` with the first candidate kept as `direct`. A host with nothing
//! dialable is not an error.
//!
//! A remote also gets a workload client certificate (ADR-0116): the key is
//! generated here, only its CSR crosses ssh (on stdin, never argv) to
//! `phux workload add-key --cert-stdout`, and the returned chain is checked
//! against the key before both are stored owner-only. The probes present it.
//! Replacing a certificate is two-phase ([`settle_identity`]): the credential
//! it supersedes is revoked on the far host only after the registry entry
//! names the new pair, and a new credential nothing recorded is revoked
//! instead, so no failure strands the entry on a revoked certificate.
//!
//! Nothing here prints or writes a registry: callers render the events, and
//! the role-specific tails in `host` own the token path and the entry.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use phux_client::attach::connection::Connection;
use phux_dial::TlsClientIdentity;
use phux_server::workload::ClientRequest;

// Default QUIC port for an enrolled server (ADR-0081), the one
// `docs/remote-access.md` uses throughout.
use phux_client_runtime::target::DEFAULT_QUIC_PORT;

/// How long one direct-route probe may take; only a filtered path waits
/// this long.
const PROBE_DEADLINE: Duration = Duration::from_secs(5);

/// Test seam for [`PROBE_DEADLINE`], in milliseconds. Undocumented for
/// operators: shortening it turns a slow host into an `ssh://` entry.
const PROBE_DEADLINE_ENV: &str = "PHUX_DIRECT_PROBE_TIMEOUT_MS";

/// ssh's exit status when ssh itself failed (resolution, connection,
/// authentication), as opposed to the remote command.
const SSH_FAILED: i32 = 255;

/// A POSIX shell's "command not found".
const COMMAND_NOT_FOUND: i32 = 127;

/// `phux service install`'s refusal when a live server holds the socket;
/// enrollment answers it with `--adopt`.
const SERVICE_INCUMBENT_LIVE: &str = "a server is already running on";

/// `phux pair`'s refusal for a pre-versioning token store; enrollment runs
/// the documented migration once.
const LEGACY_TOKEN_STORE: &str = "legacy token store requires explicit migration";

/// What `phux pair --json` reported on the remote host.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PairReport {
    pub(crate) token: String,
    pub(crate) cert_fingerprint: Option<String>,
    pub(crate) overlay_addresses: Vec<String>,
}

impl PairReport {
    /// Parse `phux pair --json`: tolerant of unknown fields, strict about the two
    /// it needs.
    pub(crate) fn parse(stdout: &str) -> Result<Self, String> {
        let value = pair_document_in(stdout)
            .ok_or_else(|| "remote `phux pair --json` emitted no JSON document".to_owned())?;

        let token = value
            .get("token")
            .and_then(serde_json::Value::as_str)
            .filter(|token| !token.is_empty())
            .ok_or_else(|| "remote `phux pair --json` reported no token".to_owned())?
            .to_owned();

        let cert_fingerprint = value
            .get("cert_fingerprint")
            .and_then(serde_json::Value::as_str)
            .filter(|fp| !fp.is_empty())
            .map(str::to_owned);

        let overlay_addresses = value
            .get("overlay_addresses")
            .and_then(serde_json::Value::as_array)
            .map(|addrs| {
                addrs
                    .iter()
                    .filter_map(serde_json::Value::as_str)
                    .map(str::to_owned)
                    .collect()
            })
            .unwrap_or_default();

        Ok(Self {
            token,
            cert_fingerprint,
            overlay_addresses,
        })
    }
}

/// The `phux pair --json` object wherever it sits in stdout (pretty-printed,
/// possibly after shell startup noise). Only an object with a `token` key
/// counts.
fn pair_document_in(stdout: &str) -> Option<serde_json::Value> {
    json_document_in(stdout, |value| value.get("token").is_some())
}

/// The JSON object `is_document` accepts wherever it sits in a remote
/// command's stdout: the whole output, a one-line document after banner
/// lines, or a pretty-printed one between the outermost braces.
fn json_document_in(
    stdout: &str,
    is_document: impl Fn(&serde_json::Value) -> bool,
) -> Option<serde_json::Value> {
    let is_document = |value: &serde_json::Value| is_document(value);
    if let Some(value) = serde_json::from_str::<serde_json::Value>(stdout.trim())
        .ok()
        .filter(is_document)
    {
        return Some(value);
    }
    // One-line document after banner lines.
    if let Some(value) = stdout
        .lines()
        .rev()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .find_map(|line| {
            serde_json::from_str::<serde_json::Value>(line)
                .ok()
                .filter(is_document)
        })
    {
        return Some(value);
    }
    // Pretty-printed document after banner lines: the outermost braces.
    let start = stdout.find('{')?;
    let end = stdout.rfind('}')?;
    serde_json::from_str::<serde_json::Value>(stdout.get(start..=end)?)
        .ok()
        .filter(is_document)
}

/// The direct routes worth dialing, most likely first: an operator
/// `--endpoint` alone, otherwise each overlay address then the host `ssh -G`
/// names, on the QUIC port, and only when a fingerprint exists (ADR-0031
/// refuses an unpinned routable dial).
pub(crate) fn candidate_endpoints(
    ssh_target_host: &str,
    report: &PairReport,
    host_override: Option<&str>,
    quic_port: u16,
) -> Vec<String> {
    if let Some(host) = host_override {
        return vec![if host.contains("://") {
            host.to_owned()
        } else {
            format!("quic://{host}")
        }];
    }
    if report.cert_fingerprint.is_none() {
        return Vec::new();
    }
    let mut candidates: Vec<String> = report
        .overlay_addresses
        .iter()
        .map(|addr| format!("quic://{}", authority(addr, quic_port)))
        .collect();
    if !ssh_target_host.is_empty() {
        let via_ssh_host = format!("quic://{}", authority(ssh_target_host, quic_port));
        if !candidates.contains(&via_ssh_host) {
            candidates.push(via_ssh_host);
        }
    }
    candidates
}

/// The local path a remote's token is written to.
pub(crate) fn token_path(state_dir: &Path, name: &str) -> PathBuf {
    state_dir.join("remotes").join(format!("{name}.token"))
}

/// The local path a satellite's token is written to; `remotes/` and
/// `satellites/` mirror the split registries.
pub(crate) fn satellite_token_path(state_dir: &Path, name: &str) -> PathBuf {
    state_dir.join("satellites").join(format!("{name}.token"))
}

/// Whether the remote server is kept alive by something, after
/// [`ensure_remote_server`] ran.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Supervision {
    /// The service unit was written and loaded: the server is running under
    /// launchd or systemd and comes back after a reboot.
    Service,
    /// A server was already live, so the unit was written and armed; that
    /// server keeps its panes and supervision takes over its next start.
    Adopted,
    /// No service manager would take the unit; `phux server --ensure` is
    /// running an unsupervised server instead. `reason` is the install
    /// failure.
    Unsupervised { reason: String },
    /// `--no-service`: only `phux server --ensure` ran.
    Skipped,
}

impl Supervision {
    /// One clause for a progress line.
    pub(crate) fn describe(&self) -> String {
        match self {
            Self::Service => "server running, supervised by its service unit".to_owned(),
            Self::Adopted => {
                "server already running; its service unit is armed for the next start".to_owned()
            }
            Self::Unsupervised { reason } => format!(
                "server running unsupervised (service install failed: {reason}); it will not come back by itself after a reboot"
            ),
            Self::Skipped => {
                "server running unsupervised (--no-service); it will not come back by itself after a reboot".to_owned()
            }
        }
    }
}

/// Whether [`ensure_remote_server`] installs the host's service unit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ServicePolicy {
    /// Install (or adopt into) the per-user unit so the server survives
    /// reboot. The default.
    Install,
    /// `--no-service`: start a server now and nothing more.
    Skip,
}

/// A progress event from the shared ssh middle, rendered by the caller with
/// [`EnrollEvent::describe`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum EnrollEvent {
    /// `phux --version` answered on the host.
    PhuxFound { version: String },
    /// The service question is settled one way or another.
    ServerReady(Supervision),
    /// Neither the service install nor `phux server --ensure` succeeded.
    /// Pairing is still tried, since a server may be live regardless, but
    /// `phux pair` mints nothing without a bound listener (ADR-0141).
    ServerStartFailed { error: String },
    /// The host's token store predated versioning; `phux pair` was rerun
    /// with `--migrate-legacy`.
    LegacyStoreMigrated,
    /// After a migration, `phux upgrade` was asked to restart the server so
    /// its listeners re-read the store.
    ListenersRestarted,
    /// That restart did not happen; the probe below decides what is
    /// reachable regardless.
    ListenerRestartFailed { error: String },
    /// The pairing document came back.
    Paired,
    /// A previously enrolled credential still works after the server was
    /// brought up; no new credential was minted.
    CredentialReused,
    /// A previously enrolled credential was revoked while minting its
    /// replacement.
    CredentialReplaced,
    /// A direct route is being dialed.
    Probing { endpoint: String },
    /// A direct route answered.
    DirectReachable { endpoint: String },
    /// A direct route did not answer within the deadline.
    DirectUnreachable { endpoint: String, reason: String },
    /// A workload client certificate was enrolled. Any credential it
    /// supersedes is still live until the entry names the new one.
    CertificateEnrolled { credential_id: String },
    /// No workload client certificate was enrolled. Not fatal: the host is
    /// paired, and dials fall back to the previous identity or none.
    CertificateNotEnrolled { reason: String },
    /// The held certificate is inside the renewal window (or unreadable), so
    /// it is replaced rather than kept.
    CertificateRenewalDue { detail: String },
    /// A credential was revoked on the far host: the one the entry named
    /// before, or a new one nothing here recorded.
    CertificateRevoked {
        credential_id: String,
        retired: Retired,
    },
    /// That revocation did not happen; `remedy` is the command that does it.
    CertificateNotRevoked {
        credential_id: String,
        retired: Retired,
        reason: String,
        remedy: String,
    },
    /// The far host holds no credential with the superseded id: the entry
    /// may have pointed at another host, where it stays admitted.
    CertificateNotHeld { credential_id: String },
    /// The entry named a client key without its certificate (or a
    /// certificate that does not read), so the far-host credential it
    /// supersedes cannot be named and is not revoked.
    PreviousCertificateUnidentified { remedy: String },
}

/// Why a credential is revoked on the far host.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Retired {
    /// The entry named it before; the new pair superseded it.
    Superseded,
    /// It was enrolled, but no entry here records it, so no one holds its
    /// key.
    Unrecorded,
}

impl Retired {
    const fn describe(self) -> &'static str {
        match self {
            Self::Superseded => "the superseded client certificate",
            Self::Unrecorded => "the client certificate nothing here recorded",
        }
    }
}

impl EnrollEvent {
    /// Whether this event leaves something for the operator to do about the
    /// client certificate: reported in `enrollment.warnings` under `--json`.
    pub(crate) const fn is_warning(&self) -> bool {
        matches!(
            self,
            Self::CertificateNotRevoked { .. }
                | Self::CertificateNotHeld { .. }
                | Self::PreviousCertificateUnidentified { .. }
        )
    }

    /// The progress line for this event, without a prefix: callers add the
    /// host name or `phux:` as their contract wants.
    pub(crate) fn describe(&self) -> String {
        match self {
            Self::PhuxFound { version } => format!("{version} found over ssh"),
            Self::ServerReady(supervision) => supervision.describe(),
            Self::ServerStartFailed { error } => format!(
                "could not start a server there ({error}); pairing needs one with a remote listener, trying anyway"
            ),
            Self::LegacyStoreMigrated => {
                "its token store predates versioning; migrated it (secrets preserved)".to_owned()
            }
            Self::ListenersRestarted => {
                "restarted the server so its listeners re-read the token store".to_owned()
            }
            Self::ListenerRestartFailed { error } => format!(
                "could not restart the server after the migration ({error}); its listeners come up on the next restart"
            ),
            Self::Paired => "paired".to_owned(),
            Self::CredentialReused => {
                "previous credential still works; kept it (no new mint)".to_owned()
            }
            Self::CredentialReplaced => {
                "minted a new credential and revoked the previous one".to_owned()
            }
            Self::Probing { endpoint } => format!("trying {endpoint}"),
            Self::DirectReachable { endpoint } => format!("direct route reachable at {endpoint}"),
            Self::DirectUnreachable { endpoint, reason } => {
                format!("{endpoint} did not answer ({reason})")
            }
            Self::CertificateEnrolled { credential_id } => {
                format!("enrolled workload client certificate {credential_id}")
            }
            Self::CertificateNotEnrolled { reason } => format!(
                "no workload client certificate enrolled ({reason}); dials keep the previous one, or present none, until `phux host add` runs again"
            ),
            Self::CertificateRenewalDue { detail } => {
                format!("the workload client certificate {detail}; enrolling a new one")
            }
            Self::CertificateRevoked {
                credential_id,
                retired,
            } => format!("revoked {} {credential_id}", retired.describe()),
            Self::CertificateNotRevoked {
                credential_id,
                retired,
                reason,
                remedy,
            } => format!(
                "could not revoke {} {credential_id} ({reason}); it stays admitted until it expires or you run `{remedy}`",
                retired.describe()
            ),
            Self::CertificateNotHeld { credential_id } => format!(
                "this host holds no credential {credential_id}, the certificate the entry named before; if the entry used to reach another host, revoke it there with `phux workload revoke {credential_id}`"
            ),
            Self::PreviousCertificateUnidentified { remedy } => format!(
                "the entry named a client key without a readable certificate, so the credential it replaces cannot be named or revoked; find it with `{remedy}` and revoke it there"
            ),
        }
    }
}

/// Why the ssh middle stopped: one variant per remedy the callers phrase.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum EnrollFailure {
    /// ssh itself failed (exit 255): resolution, connection, authentication.
    SshUnreachable(String),
    /// `phux` is not on the remote `PATH` (exit 127, or `phux --version`
    /// failed some other way).
    MissingPhux(String),
    /// Pairing failed: `phux pair --json` did not run, or its output did
    /// not parse.
    Pair(String),
}

impl EnrollFailure {
    /// The failure's own words, for callers that print them inline.
    pub(crate) fn detail(&self) -> &str {
        match self {
            Self::SshUnreachable(detail) | Self::MissingPhux(detail) | Self::Pair(detail) => detail,
        }
    }
}

/// Everything the shared middle needs to enroll one host.
pub(crate) struct EnrollRequest<'a> {
    /// The ssh destination, exactly as typed after `ssh`.
    pub(crate) ssh_host: &'a str,
    /// The `phux` to run on the host (`--remote-phux`).
    pub(crate) remote_phux: &'a str,
    /// `--endpoint`: the one address to register, probed but never
    /// second-guessed.
    pub(crate) endpoint_override: Option<&'a str>,
    /// The QUIC port to configure on the host and dial.
    pub(crate) quic_port: u16,
    /// Whether to install the host's service unit.
    pub(crate) service: ServicePolicy,
    /// Bearer token already held for this name when re-enrolling: probed first
    /// and reused if a direct route answers, else revoked via `--replace-token`.
    pub(crate) previous_token: Option<&'a str>,
    /// Certificate fingerprint saved with [`Self::previous_token`], required
    /// to probe a previous credential (ADR-0031 refuses an unpinned dial).
    pub(crate) previous_fingerprint: Option<&'a str>,
    /// The workload client certificate already held for this name, presented
    /// on the reuse probe and kept when the previous credential still works.
    pub(crate) previous_identity: Option<&'a ClientIdentityFiles>,
    /// Enroll a workload client certificate over the same ssh trust
    /// (ADR-0116); `None` for a role that presents none (a satellite).
    pub(crate) workload: Option<WorkloadEnrollment<'a>>,
}

/// Where and as what a workload client certificate is enrolled.
pub(crate) struct WorkloadEnrollment<'a> {
    /// The directory the key and certificate land in (`<state>/remotes`).
    pub(crate) dir: &'a Path,
    /// The registry name, which prefixes the two file names.
    pub(crate) name: &'a str,
}

/// A stored workload client identity: its two owner-only files and the
/// credential id the far host recorded for it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ClientIdentityFiles {
    /// The certificate chain (PEM, leaf then CA).
    pub(crate) certificate: PathBuf,
    /// The private key (PEM). Named here, read only by the TLS stack.
    pub(crate) private_key: PathBuf,
    /// `sha256:` of the key: the far host's registry id for it.
    pub(crate) credential_id: Option<String>,
    /// Whether this enrollment minted it (and so owns its cleanup).
    pub(crate) fresh: bool,
}

impl ClientIdentityFiles {
    /// An identity already on disk, as a registry entry names it.
    pub(crate) fn existing(certificate: &Path, private_key: &Path) -> Self {
        Self {
            credential_id: phux_server::workload::stored_credential_id(certificate),
            certificate: certificate.to_path_buf(),
            private_key: private_key.to_path_buf(),
            fresh: false,
        }
    }

    /// The identity a dial presents.
    pub(crate) fn tls(&self) -> TlsClientIdentity {
        TlsClientIdentity::PemFiles {
            certificate: self.certificate.clone(),
            private_key: self.private_key.clone(),
        }
    }

    /// Whether these are a pair `phux host add` enrolled for `name` in
    /// `dir` (see [`is_enrolled_file`]).
    #[cfg(test)]
    pub(crate) fn enrolled_under(&self, dir: &Path, name: &str) -> bool {
        is_enrolled_file(&self.certificate, dir, name, CERT_EXTENSION)
            && is_enrolled_file(&self.private_key, dir, name, KEY_EXTENSION)
    }

    /// Remove the two files, when this enrollment minted them and nothing
    /// will record them.
    pub(crate) fn discard_if_fresh(&self) {
        if self.fresh {
            phux_server::workload::remove_identity_files(&self.private_key, &self.certificate);
        }
    }

    /// Why this identity should be replaced now, or `None` while it is good
    /// for more than [`RENEW_WITHIN_SECONDS`].
    pub(crate) fn renewal_due(&self, now: i64) -> Option<String> {
        renewal_due(&self.certificate, now)
    }
}

/// The extension of an enrolled certificate chain file.
const CERT_EXTENSION: &str = ".pem";
/// The extension of an enrolled private key file.
const KEY_EXTENSION: &str = ".key";

/// Whether `path` is a file `phux host add` enrolled for `name` in `dir`:
/// directly in `dir` and named `<name>.client.<hex digest><extension>`, as
/// [`identity_stem`] names them. Only such files are removed when a
/// re-enrollment supersedes them; a `client-cert` or `client-key` the
/// operator pointed at their own files is left alone.
fn is_enrolled_file(path: &Path, dir: &Path, name: &str, extension: &str) -> bool {
    let prefix = format!("{name}.client.");
    path.parent() == Some(dir)
        && path
            .file_name()
            .and_then(|file| file.to_str())
            .and_then(|file| file.strip_prefix(&prefix))
            .and_then(|rest| rest.strip_suffix(extension))
            .is_some_and(|digest| {
                !digest.is_empty() && digest.bytes().all(|b| b.is_ascii_hexdigit())
            })
}

/// How long before a client certificate stops admitting `phux host add`
/// replaces it, dials warn, and `phux doctor` reports it: 14 days.
pub(crate) const RENEW_WITHIN_SECONDS: i64 = 14 * SECONDS_PER_DAY;

const SECONDS_PER_DAY: i64 = 24 * 60 * 60;

/// When the far host stops admitting the client certificate at `cert`, as
/// Unix seconds. The registry expiry is authoritative (workload-auth §7)
/// and issuance rounds the certificate's `notAfter` up to the next midnight
/// after it, so admission ends within the day before `notAfter`; this
/// answers the start of that day. `None` when the file does not read as a
/// certificate.
pub(crate) fn admission_ends(cert: &Path) -> Option<i64> {
    phux_server::workload::stored_certificate_expiry(cert)
        .map(|not_after| not_after.saturating_sub(SECONDS_PER_DAY))
}

/// Why the client certificate at `cert` should be renewed at `now`
/// (unreadable, expired, or within [`RENEW_WITHIN_SECONDS`]), in words that
/// follow "the workload client certificate"; `None` while it is good.
pub(crate) fn renewal_due(cert: &Path, now: i64) -> Option<String> {
    let Some(ends) = admission_ends(cert) else {
        return Some(format!("at {} cannot be read", cert.display()));
    };
    let remaining = ends.saturating_sub(now);
    if remaining > RENEW_WITHIN_SECONDS {
        return None;
    }
    let date = calendar_date(ends);
    Some(if remaining <= 0 {
        format!("expired on {date}")
    } else {
        match remaining / SECONDS_PER_DAY {
            0 => format!("expires on {date} (in less than a day)"),
            days => format!("expires on {date} (in {days} day(s))"),
        }
    })
}

/// `YYYY-MM-DD` in UTC, or the raw seconds when out of range.
pub(crate) fn calendar_date(seconds: i64) -> String {
    chrono::DateTime::from_timestamp(seconds, 0).map_or_else(
        || seconds.to_string(),
        |at| at.format("%Y-%m-%d").to_string(),
    )
}

/// The client identity files an entry names, whole or half, before a
/// (re-)enrollment replaces them: what [`settle_identity`] retires once the
/// entry names the new pair.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct HeldIdentity {
    /// `client-cert`, when the entry names one.
    pub(crate) certificate: Option<PathBuf>,
    /// `client-key`, when the entry names one.
    pub(crate) private_key: Option<PathBuf>,
}

impl HeldIdentity {
    /// Both halves, as the pair a dial presents; `None` for half or none.
    pub(crate) fn pair(&self) -> Option<ClientIdentityFiles> {
        let certificate = self.certificate.as_deref()?;
        let private_key = self.private_key.as_deref()?;
        Some(ClientIdentityFiles::existing(certificate, private_key))
    }

    /// Whether the entry names neither half.
    pub(crate) const fn is_empty(&self) -> bool {
        self.certificate.is_none() && self.private_key.is_none()
    }

    /// The far host's id for the held credential, read from a certificate
    /// (public material): the one the entry names, else the enrolled
    /// certificate beside an enrolled key the entry names alone. `None` when
    /// neither reads: a key alone cannot name it without reading private
    /// bytes.
    pub(crate) fn credential_id(&self, dir: &Path, name: &str) -> Option<String> {
        self.read_certificate(dir, name, phux_server::workload::stored_credential_id)
    }

    /// The `sha256:` fingerprint of the CA that issued the held
    /// certificate, from the chain stored with it (found as for
    /// [`Self::credential_id`]). `None` for a leaf-only or unreadable file.
    pub(crate) fn authority(&self, dir: &Path, name: &str) -> Option<String> {
        self.read_certificate(dir, name, phux_server::workload::stored_chain_authority)
    }

    /// `read` over the certificate the entry names, else the enrolled
    /// certificate beside an enrolled key the entry names alone.
    fn read_certificate(
        &self,
        dir: &Path,
        name: &str,
        read: fn(&Path) -> Option<String>,
    ) -> Option<String> {
        self.certificate.as_deref().and_then(read).or_else(|| {
            self.enrolled_files(dir, name)
                .iter()
                .filter(|path| is_enrolled_file(path, dir, name, CERT_EXTENSION))
                .find_map(|path| read(path))
        })
    }

    /// The files an enrollment wrote for `name` under `dir` that this
    /// identity names: each enrolled half, with the other half of its pair
    /// beside it, since an entry that lost one half still left both files
    /// on disk. Files the operator named themselves are never included.
    fn enrolled_files(&self, dir: &Path, name: &str) -> Vec<PathBuf> {
        let mut files = Vec::new();
        for (path, extension) in [
            (self.certificate.as_deref(), CERT_EXTENSION),
            (self.private_key.as_deref(), KEY_EXTENSION),
        ] {
            let Some(path) = path.filter(|path| is_enrolled_file(path, dir, name, extension))
            else {
                continue;
            };
            for sibling in [CERT_EXTENSION, KEY_EXTENSION] {
                let sibling = path.with_extension(sibling.trim_start_matches('.'));
                if !files.contains(&sibling) {
                    files.push(sibling);
                }
            }
        }
        files
    }
}

/// What the shared ssh middle produced.
pub(crate) struct EnrollOutcome {
    /// The endpoint to register: the first direct route that answered, else
    /// `ssh://HOST`.
    pub(crate) endpoint: String,
    /// When `endpoint` is `ssh://`, the first direct candidate, kept so a
    /// later attach can try it and promote it once it answers.
    pub(crate) direct: Option<String>,
    /// Every direct route that was dialed, in order, for the report.
    pub(crate) tried: Vec<String>,
    /// The parsed `phux pair --json` document (token, fingerprint, overlay
    /// addresses).
    pub(crate) report: PairReport,
    /// How the remote server is kept alive.
    pub(crate) supervision: Option<Supervision>,
    /// The workload client identity to register: freshly enrolled, the
    /// previous one kept, or none.
    pub(crate) identity: Option<ClientIdentityFiles>,
    /// What happened to the workload client certificate, for the report.
    pub(crate) certificate: CertificateStatus,
}

/// What an enrollment did about the workload client certificate: the
/// `enrollment.status` of `phux host add --json` and `phux host renew
/// --json`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum CertificateStatus {
    /// A new certificate was enrolled.
    Enrolled,
    /// The previous certificate still works and is not due for renewal.
    Kept,
    /// Enrollment was tried and failed; the entry keeps the previous
    /// certificate, or none.
    Failed(String),
    /// Nothing was tried: a satellite, `--ssh-only`, the manual form, or a
    /// host with no direct route to present a certificate to.
    Skipped,
}

impl CertificateStatus {
    /// The stable `enrollment.status` string.
    pub(crate) const fn as_str(&self) -> &'static str {
        match self {
            Self::Enrolled => "enrolled",
            Self::Kept => "kept",
            Self::Failed(_) => "failed",
            Self::Skipped => "skipped",
        }
    }

    /// Why enrollment failed, when it did.
    pub(crate) fn error(&self) -> Option<&str> {
        match self {
            Self::Failed(reason) => Some(reason),
            Self::Enrolled | Self::Kept | Self::Skipped => None,
        }
    }
}

/// The shared middle of every ssh enrollment. Role-agnostic: nothing here
/// touches a registry or writes a token; a workload client identity, when
/// requested, is written to its own new files and handed back.
pub(crate) fn enroll_over_ssh(
    req: &EnrollRequest<'_>,
    on_event: &mut dyn FnMut(EnrollEvent),
) -> Result<EnrollOutcome, EnrollFailure> {
    let version = remote_phux_version(req.ssh_host, req.remote_phux)?;
    on_event(EnrollEvent::PhuxFound {
        version: version.trim().to_owned(),
    });

    let supervision = match ensure_remote_server_after_version(
        req.ssh_host,
        req.remote_phux,
        req.quic_port,
        req.service,
    ) {
        Ok(supervision) => {
            on_event(EnrollEvent::ServerReady(supervision.clone()));
            Some(supervision)
        }
        Err(error) => {
            on_event(EnrollEvent::ServerStartFailed { error });
            None
        }
    };

    // Reuse the credential already held before minting another live bearer
    // (ADR-0122).
    let (report, reused) = if let Some(report) = try_reuse_previous_credential(req, on_event) {
        on_event(EnrollEvent::CredentialReused);
        (report, true)
    } else {
        let report = pair_over_ssh(req.ssh_host, req.remote_phux, req.previous_token, on_event)?;
        if req.previous_token.is_some() {
            on_event(EnrollEvent::CredentialReplaced);
        } else {
            on_event(EnrollEvent::Paired);
        }
        (report, false)
    };

    let ssh_target_host = ssh_hostname(req.ssh_host);
    let candidates = candidate_endpoints(
        &ssh_target_host,
        &report,
        req.endpoint_override,
        req.quic_port,
    );
    // Only a host with a dialable route has anything to present a
    // certificate to.
    let (identity, certificate) = if candidates.is_empty() {
        let kept = req.previous_identity.cloned();
        let status = if kept.is_some() {
            CertificateStatus::Kept
        } else {
            CertificateStatus::Skipped
        };
        (kept, status)
    } else {
        client_identity_for(req, reused, on_event)
    };
    let (endpoint, direct, tried) = first_answering_route(
        req.ssh_host,
        &candidates,
        &report,
        identity.as_ref(),
        on_event,
    );
    Ok(EnrollOutcome {
        endpoint,
        direct,
        tried,
        report,
        supervision,
        identity,
        certificate,
    })
}

/// Dial each candidate in turn; the first that answers is the endpoint.
/// Nothing answering registers `ssh://HOST` with the first candidate kept as
/// the direct route to promote later. Returns `(endpoint, direct, tried)`.
fn first_answering_route(
    ssh_host: &str,
    candidates: &[String],
    report: &PairReport,
    identity: Option<&ClientIdentityFiles>,
    on_event: &mut dyn FnMut(EnrollEvent),
) -> (String, Option<String>, Vec<String>) {
    let presented = identity.map(ClientIdentityFiles::tls);
    let mut tried = Vec::new();
    for candidate in candidates {
        // Only QUIC can be probed here; a `wss://` override is registered
        // as the operator wrote it, as it always was.
        let Some(target) = candidate.strip_prefix("quic://") else {
            return (candidate.clone(), None, tried);
        };
        on_event(EnrollEvent::Probing {
            endpoint: candidate.clone(),
        });
        tried.push(candidate.clone());
        match probe(
            target,
            &report.token,
            report.cert_fingerprint.as_deref(),
            presented.clone(),
        ) {
            Ok(()) => {
                on_event(EnrollEvent::DirectReachable {
                    endpoint: candidate.clone(),
                });
                return (candidate.clone(), None, tried);
            }
            Err(reason) => on_event(EnrollEvent::DirectUnreachable {
                endpoint: candidate.clone(),
                reason,
            }),
        }
    }
    (
        format!("ssh://{ssh_host}"),
        candidates.first().cloned(),
        tried,
    )
}

/// The scope `phux host add` enrolls with: every verb on the whole host.
/// Enrollment grants no authority ssh did not already grant, and an operator
/// who can run `phux` over ssh there already holds all of it. Spelled out
/// rather than `*`, which the far host's shell would glob.
pub(crate) const HOST_ADD_WORKLOAD_SCOPE: &str =
    "inventory,observe,create,bind,input,signal@global";

/// The workload identity to register: the previous one when its bearer
/// still worked (nothing was re-minted) and it is not due for renewal,
/// otherwise a fresh enrollment. The previous credential stays live on the
/// far host until [`settle_identity`] runs after the registry write. A
/// failed enrollment is a warning, never a failed `host add`: the host is
/// paired either way, and the previous identity (if any) is kept.
fn client_identity_for(
    req: &EnrollRequest<'_>,
    reused: bool,
    on_event: &mut dyn FnMut(EnrollEvent),
) -> (Option<ClientIdentityFiles>, CertificateStatus) {
    if reused && let Some(previous) = req.previous_identity {
        match previous.renewal_due(chrono::Utc::now().timestamp()) {
            None => return (Some(previous.clone()), CertificateStatus::Kept),
            Some(detail) => on_event(EnrollEvent::CertificateRenewalDue { detail }),
        }
    }
    let Some(workload) = req.workload.as_ref() else {
        return (None, CertificateStatus::Skipped);
    };
    let far = FarHost {
        ssh_host: req.ssh_host,
        remote_phux: req.remote_phux,
    };
    match enroll_client_certificate(&far, workload, on_event) {
        Ok(identity) => {
            on_event(EnrollEvent::CertificateEnrolled {
                credential_id: identity.credential_id.clone().unwrap_or_default(),
            });
            (Some(identity), CertificateStatus::Enrolled)
        }
        Err(reason) => {
            on_event(EnrollEvent::CertificateNotEnrolled {
                reason: reason.clone(),
            });
            (
                req.previous_identity.cloned(),
                CertificateStatus::Failed(reason),
            )
        }
    }
}

/// The far end of an enrollment: where ssh goes and which `phux` runs there.
#[derive(Debug, Clone, Copy)]
pub(crate) struct FarHost<'a> {
    pub(crate) ssh_host: &'a str,
    pub(crate) remote_phux: &'a str,
}

impl FarHost<'_> {
    /// The command an operator runs to revoke `credential_id` by hand.
    fn revoke_command(&self, credential_id: &str) -> String {
        format!(
            "ssh {} {} workload revoke {credential_id}",
            self.ssh_host, self.remote_phux
        )
    }

    /// Revoke `credential_id` on the far host: `Ok(true)` once it is
    /// revoked (now or before), `Ok(false)` when the host holds no such
    /// credential (it admits nothing there, but it may still be live on
    /// another host). The id is public (a hash of a public key); nothing
    /// secret enters argv.
    fn revoke(&self, credential_id: &str) -> Result<bool, String> {
        let out = ssh_run(
            self.ssh_host,
            &[
                self.remote_phux,
                "workload",
                "revoke",
                credential_id,
                "--json",
            ],
        )?;
        if out.status.success() {
            return Ok(true);
        }
        if out.stderr.contains(NO_SUCH_CREDENTIAL) {
            return Ok(false);
        }
        Err(out.failure_detail())
    }

    /// Revoke `credential_id`, narrating the result: `Some(true)` when it
    /// is revoked, `Some(false)` when the revocation failed, `None` when
    /// this host never held it. A superseded credential this host does not
    /// hold is reported (the entry may have pointed at another host); an
    /// unrecorded one it does not hold was never enrolled, so says nothing.
    fn retire(
        &self,
        credential_id: &str,
        retired: Retired,
        on_event: &mut dyn FnMut(EnrollEvent),
    ) -> Option<bool> {
        let credential_id = credential_id.to_owned();
        match self.revoke(&credential_id) {
            Ok(true) => {
                on_event(EnrollEvent::CertificateRevoked {
                    credential_id,
                    retired,
                });
                Some(true)
            }
            Ok(false) => {
                if retired == Retired::Superseded {
                    on_event(EnrollEvent::CertificateNotHeld { credential_id });
                }
                None
            }
            Err(reason) => {
                on_event(EnrollEvent::CertificateNotRevoked {
                    remedy: self.revoke_command(&credential_id),
                    credential_id,
                    retired,
                    reason,
                });
                Some(false)
            }
        }
    }
}

/// `phux workload revoke`'s refusal for an id its registry does not hold.
const NO_SUCH_CREDENTIAL: &str = "no workload credential";

/// What [`settle_identity`] did with the credential the new pair replaced.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Settlement {
    /// The far-host id of the credential the entry named before, when it
    /// could be read and differs from the new one.
    pub(crate) previous_credential_id: Option<String>,
    /// Whether that credential is revoked now; `None` when there was none
    /// to revoke, it could not be named, or this host does not hold it.
    pub(crate) previous_revoked: Option<bool>,
}

/// The second phase of a (re-)enrollment, run after the registry write.
///
/// When the entry now names `fresh` (`recorded`), the credential `held`
/// named is revoked on the far host and the files an enrollment wrote for
/// it are removed. When it does not, `fresh` is the orphan: its files are
/// removed and its credential revoked, and the entry keeps what it held.
/// Revoking only after the entry moved means no failure leaves the entry
/// on a credential the far host already refused; the cost is a window,
/// between the two ssh calls, in which both credentials of this one client
/// are admitted.
pub(crate) fn settle_identity(
    far: &FarHost<'_>,
    fresh: Option<&ClientIdentityFiles>,
    recorded: bool,
    held: &HeldIdentity,
    workload: &WorkloadEnrollment<'_>,
    on_event: &mut dyn FnMut(EnrollEvent),
) -> Settlement {
    let Some(fresh) = fresh.filter(|identity| identity.fresh) else {
        return Settlement::default();
    };
    if !recorded {
        fresh.discard_if_fresh();
        if let Some(id) = &fresh.credential_id {
            far.retire(id, Retired::Unrecorded, on_event);
        }
        return Settlement::default();
    }
    let mut settlement = Settlement::default();
    match held.credential_id(workload.dir, workload.name) {
        Some(previous) if fresh.credential_id.as_deref() != Some(previous.as_str()) => {
            settlement.previous_revoked = far.retire(&previous, Retired::Superseded, on_event);
            settlement.previous_credential_id = Some(previous);
        }
        Some(_) => {}
        None if held.is_empty() => {}
        None => on_event(EnrollEvent::PreviousCertificateUnidentified {
            remedy: format!("ssh {} {} workload list", far.ssh_host, far.remote_phux),
        }),
    }
    for path in held.enrolled_files(workload.dir, workload.name) {
        if path != fresh.certificate && path != fresh.private_key {
            phux_server::workload::remove_owned_file(&path);
        }
    }
    settlement
}

/// Enroll a workload client certificate on the far host (ADR-0116):
/// generate the key here, send only its CSR over ssh stdin to `phux
/// workload add-key --cert-stdout`, validate the returned chain against the
/// key, and store both owner-only under new names. Nothing is revoked here:
/// the credential this one supersedes goes in [`settle_identity`], once the
/// entry names the new pair. If the far host enrolled the key but a check
/// or the store fails here, that new credential (whose key is then gone) is
/// revoked before the error returns.
pub(crate) fn enroll_client_certificate(
    far: &FarHost<'_>,
    workload: &WorkloadEnrollment<'_>,
    on_event: &mut dyn FnMut(EnrollEvent),
) -> Result<ClientIdentityFiles, String> {
    let (ssh_host, remote_phux) = (far.ssh_host, far.remote_phux);
    // The name becomes a file name: refuse one the registry would refuse
    // before anything is written here or changed on the far host.
    if super::remote::validate_name(workload.name).as_deref() != Ok(workload.name) {
        return Err(format!("{:?} is not a valid remote name", workload.name));
    }
    let authority = ssh_run(
        ssh_host,
        &[remote_phux, "workload", "authority", "--init", "--json"],
    )?;
    if !authority.status.success() {
        return Err(format!(
            "the host could not initialize its workload authority: {}",
            authority.failure_detail()
        ));
    }
    let request = ClientRequest::generate()
        .map_err(|err| format!("could not generate a client key: {err}"))?;
    let argv = [
        remote_phux,
        "workload",
        "add-key",
        "--json",
        "--cert-stdout",
        "--scope",
        HOST_ADD_WORKLOAD_SCOPE,
    ];
    let added = ssh_run_with_stdin(ssh_host, &argv, request.csr_pem().as_bytes())?;
    if !added.status.success() {
        // ssh itself failed: the far host may have committed before the
        // channel dropped, so retire this request's own id (a host that
        // never enrolled it answers "not held", which says nothing).
        if added.status.code() == Some(SSH_FAILED) {
            far.retire(&request.credential_id(), Retired::Unrecorded, on_event);
        }
        return Err(format!(
            "`phux workload add-key` failed there: {}",
            added.failure_detail()
        ));
    }
    accept_and_store(&request, &added.stdout, workload).inspect_err(|_| {
        // The far host may hold a credential for a key that is gone now.
        // Only this request's own id is revoked, never one the untrusted
        // reply named.
        far.retire(&request.credential_id(), Retired::Unrecorded, on_event);
    })
}

/// The local half of [`enroll_client_certificate`]: check the reply, bind it
/// to the key, and store both owner-only.
fn accept_and_store(
    request: &ClientRequest,
    stdout: &str,
    workload: &WorkloadEnrollment<'_>,
) -> Result<ClientIdentityFiles, String> {
    let reply = AddKeyReply::parse(stdout)?;
    if reply.credential_id != request.credential_id() {
        return Err("the host recorded a different credential than the key sent".to_owned());
    }
    let issued = request
        .accept(&reply.certificate_chain)
        .map_err(|err| err.to_string())?;
    let stem = identity_stem(workload.name, issued.credential_id());
    let files = ClientIdentityFiles {
        certificate: workload.dir.join(format!("{stem}{CERT_EXTENSION}")),
        private_key: workload.dir.join(format!("{stem}{KEY_EXTENSION}")),
        credential_id: Some(issued.credential_id().to_owned()),
        fresh: true,
    };
    issued
        .store(&files.private_key, &files.certificate)
        .map_err(|err| format!("could not store the client identity: {err}"))?;
    Ok(files)
}

/// `<name>.client.<first 16 hex digits of the credential id>`: one pair of
/// files per enrollment, so a re-enrollment never overwrites the pair the
/// registry still names.
fn identity_stem(name: &str, credential_id: &str) -> String {
    let digest = credential_id
        .strip_prefix("sha256:")
        .unwrap_or(credential_id);
    let short = digest.get(..16).unwrap_or(digest);
    format!("{name}.client.{short}")
}

/// What `phux workload add-key --json --cert-stdout` reported on the far
/// host. Untrusted: every field is checked here or by
/// [`ClientRequest::accept`].
#[derive(Debug)]
struct AddKeyReply {
    credential_id: String,
    certificate_chain: String,
}

impl AddKeyReply {
    fn parse(stdout: &str) -> Result<Self, String> {
        let is_document = |value: &serde_json::Value| {
            value.get("operation").and_then(serde_json::Value::as_str) == Some("add-key")
        };
        let document = json_document_in(stdout, is_document)
            .ok_or_else(|| "`phux workload add-key` reported no document".to_owned())?;
        let field = |name: &str| {
            document
                .get(name)
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned)
                .ok_or_else(|| {
                    format!(
                        "`phux workload add-key` reported no {name}; is phux on the host older than this one?"
                    )
                })
        };
        let credential_id = field("credential_id")?;
        if !phux_server::workload::is_canonical_credential_id(&credential_id) {
            return Err("the host reported a malformed credential id".to_owned());
        }
        Ok(Self {
            credential_id,
            certificate_chain: field("certificate_chain")?,
        })
    }
}

/// Make sure a server is running on `ssh_host` and, under
/// [`ServicePolicy::Install`], that it keeps running. Confirms `phux` first so a
/// lost install is reported as that.
pub(crate) fn ensure_remote_server(
    ssh_host: &str,
    remote_phux: &str,
    quic_port: u16,
    policy: ServicePolicy,
) -> Result<Supervision, EnrollFailure> {
    remote_phux_version(ssh_host, remote_phux)?;
    ensure_remote_server_after_version(ssh_host, remote_phux, quic_port, policy)
        .map_err(EnrollFailure::Pair)
}

/// [`ensure_remote_server`] once `phux` is known to be there. Under
/// [`ServicePolicy::Install`], `phux service install --quic` covers every state:
/// no unit (written and started), a stopped unit (reloaded), or a live server
/// holding the socket (refused, then answered with `--adopt`). Anything else
/// falls back to an unsupervised `phux server --ensure`.
fn ensure_remote_server_after_version(
    ssh_host: &str,
    remote_phux: &str,
    quic_port: u16,
    policy: ServicePolicy,
) -> Result<Supervision, String> {
    let quic_bind = format!("0.0.0.0:{quic_port}");
    let install_error = match policy {
        ServicePolicy::Skip => None,
        ServicePolicy::Install => {
            let install = ssh_run(
                ssh_host,
                &[remote_phux, "service", "install", "--quic", &quic_bind],
            )?;
            if install.status.success() {
                return Ok(Supervision::Service);
            }
            if install.stderr.contains(SERVICE_INCUMBENT_LIVE) {
                let adopt = ssh_run(
                    ssh_host,
                    &[
                        remote_phux,
                        "service",
                        "install",
                        "--quic",
                        &quic_bind,
                        "--adopt",
                    ],
                )?;
                if adopt.status.success() {
                    return Ok(Supervision::Adopted);
                }
                Some(adopt.failure_detail())
            } else {
                Some(install.failure_detail())
            }
        }
    };

    let ensure = ssh_run(ssh_host, &[remote_phux, "server", "--ensure"])?;
    if !ensure.status.success() {
        let detail = ensure.failure_detail();
        return Err(install_error.map_or_else(
            || format!("`phux server --ensure` failed: {detail}"),
            |install| {
                format!(
                    "service install failed ({install}) and `phux server --ensure` failed ({detail})"
                )
            },
        ));
    }
    Ok(
        install_error.map_or(Supervision::Skipped, |reason| Supervision::Unsupervised {
            reason,
        }),
    )
}

/// After the server is up, dial with the previous credential; `Some` when a
/// direct route accepts it.
fn try_reuse_previous_credential(
    req: &EnrollRequest<'_>,
    on_event: &mut dyn FnMut(EnrollEvent),
) -> Option<PairReport> {
    let token = req.previous_token?;
    let fingerprint = req.previous_fingerprint.filter(|fp| !fp.is_empty())?;
    let report = PairReport {
        token: token.to_owned(),
        cert_fingerprint: Some(fingerprint.to_owned()),
        overlay_addresses: Vec::new(),
    };
    let presented = req.previous_identity.map(ClientIdentityFiles::tls);
    let ssh_target_host = ssh_hostname(req.ssh_host);
    let candidates = candidate_endpoints(
        &ssh_target_host,
        &report,
        req.endpoint_override,
        req.quic_port,
    );
    for candidate in &candidates {
        let Some(target) = candidate.strip_prefix("quic://") else {
            // A `wss://` override cannot be probed here; keep the previous
            // credential and let the caller register it as written.
            return Some(report);
        };
        on_event(EnrollEvent::Probing {
            endpoint: candidate.clone(),
        });
        match probe(
            target,
            &report.token,
            report.cert_fingerprint.as_deref(),
            presented.clone(),
        ) {
            Ok(()) => {
                on_event(EnrollEvent::DirectReachable {
                    endpoint: candidate.clone(),
                });
                return Some(report);
            }
            Err(reason) => on_event(EnrollEvent::DirectUnreachable {
                endpoint: candidate.clone(),
                reason,
            }),
        }
    }
    None
}

/// `phux pair --json` on the host, migrating a pre-versioned token store
/// once if that is what refuses the mint. When `replace_token` is set, the
/// remote mint revokes that bearer in the same rewrite.
fn pair_over_ssh(
    ssh_host: &str,
    remote_phux: &str,
    replace_token: Option<&str>,
    on_event: &mut dyn FnMut(EnrollEvent),
) -> Result<PairReport, EnrollFailure> {
    let mut pair_argv = vec![remote_phux, "pair", "--json"];
    let replace_flag;
    let replace_value;
    if let Some(token) = replace_token {
        replace_flag = "--replace-token".to_owned();
        replace_value = token.to_owned();
        pair_argv.push(&replace_flag);
        pair_argv.push(&replace_value);
    }
    let first = ssh_run(ssh_host, &pair_argv).map_err(EnrollFailure::Pair)?;
    let stdout = if first.status.success() {
        first.stdout
    } else if first.stderr.contains(LEGACY_TOKEN_STORE) {
        migrate_legacy_over_ssh(ssh_host, remote_phux, &pair_argv, on_event)?
    } else {
        return Err(EnrollFailure::Pair(
            first.command_failure(ssh_host, &pair_argv),
        ));
    };
    PairReport::parse(&stdout).map_err(EnrollFailure::Pair)
}

/// Rerun `phux pair --json` with `--migrate-legacy`, then restart the server
/// so its listeners re-read the migrated store. A server from before remote
/// listeners bound ahead of the store disabled them when the legacy store
/// failed to load at boot, and `phux pair` mints nothing with no listener
/// bound (ADR-0141); the migration has landed by then, so that refusal is
/// answered with the restart first and one more `phux pair --json`. Returns
/// the pairing document's stdout.
fn migrate_legacy_over_ssh(
    ssh_host: &str,
    remote_phux: &str,
    pair_argv: &[&str],
    on_event: &mut dyn FnMut(EnrollEvent),
) -> Result<String, EnrollFailure> {
    let mut migrated_argv = pair_argv.to_vec();
    migrated_argv.push("--migrate-legacy");
    let migrated = ssh_run(ssh_host, &migrated_argv).map_err(EnrollFailure::Pair)?;
    if migrated.status.success() {
        on_event(EnrollEvent::LegacyStoreMigrated);
        restart_listeners(ssh_host, remote_phux, on_event);
        return Ok(migrated.stdout);
    }
    if !migrated.stderr.contains(super::pair::NO_BOUND_LISTENER) {
        return Err(EnrollFailure::Pair(
            migrated.command_failure(ssh_host, &migrated_argv),
        ));
    }
    restart_listeners(ssh_host, remote_phux, on_event);
    let retried = ssh_run(ssh_host, pair_argv).map_err(EnrollFailure::Pair)?;
    if !retried.status.success() {
        return Err(EnrollFailure::Pair(
            retried.command_failure(ssh_host, pair_argv),
        ));
    }
    on_event(EnrollEvent::LegacyStoreMigrated);
    Ok(retried.stdout)
}

/// Restart the server with `phux upgrade`, the restart that keeps every pane
/// alive, so its listeners re-read a migrated store.
fn restart_listeners(ssh_host: &str, remote_phux: &str, on_event: &mut dyn FnMut(EnrollEvent)) {
    match ssh_run(ssh_host, &[remote_phux, "upgrade"]) {
        Ok(upgrade) if upgrade.status.success() => on_event(EnrollEvent::ListenersRestarted),
        Ok(upgrade) => on_event(EnrollEvent::ListenerRestartFailed {
            error: upgrade.failure_detail(),
        }),
        Err(error) => on_event(EnrollEvent::ListenerRestartFailed { error }),
    }
}

/// Write a pairing token owner-only, creating the directory it lives in.
pub(crate) fn write_token(path: &Path, token: &str) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|err| format!("could not create {}: {err}", parent.display()))?;
    }
    // Create with the restrictive mode rather than chmod-after-write: a
    // world-readable window, however brief, is a window on a bearer token.
    #[cfg(unix)]
    {
        use std::io::Write as _;
        use std::os::unix::fs::OpenOptionsExt as _;
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(path)
            .map_err(|err| format!("could not write {}: {err}", path.display()))?;
        writeln!(file, "{token}")
            .map_err(|err| format!("could not write {}: {err}", path.display()))?;
    }
    #[cfg(not(unix))]
    {
        std::fs::write(path, format!("{token}\n"))
            .map_err(|err| format!("could not write {}: {err}", path.display()))?;
    }
    Ok(())
}

/// Confirm `phux` is on the remote `PATH` before doing anything that assumes
/// it, so the failure names the real problem instead of surfacing as a
/// baffling empty pair document. Returns the version line.
pub(crate) fn remote_phux_version(
    ssh_host: &str,
    remote_phux: &str,
) -> Result<String, EnrollFailure> {
    let output =
        ssh_run(ssh_host, &[remote_phux, "--version"]).map_err(EnrollFailure::SshUnreachable)?;
    match output.status.code() {
        Some(0) => Ok(output.stdout),
        Some(SSH_FAILED) => Err(EnrollFailure::SshUnreachable(output.failure_detail())),
        Some(COMMAND_NOT_FOUND) => Err(EnrollFailure::MissingPhux(format!(
            "`{remote_phux}` was not found on {ssh_host}"
        ))),
        _ => Err(EnrollFailure::MissingPhux(
            output.command_failure(ssh_host, &[remote_phux, "--version"]),
        )),
    }
}

/// What one ssh-run command produced.
pub(crate) struct SshOutput {
    pub(crate) status: std::process::ExitStatus,
    pub(crate) stdout: String,
    pub(crate) stderr: String,
}

impl SshOutput {
    /// The far end's stderr, trimmed, or the exit status when it said
    /// nothing.
    fn failure_detail(&self) -> String {
        let detail = self.stderr.trim();
        if detail.is_empty() {
            format!("exit {}", self.status)
        } else {
            detail.to_owned()
        }
    }

    /// The failure worded with the command that failed, for the error path.
    fn command_failure(&self, ssh_host: &str, argv: &[&str]) -> String {
        format!(
            "`ssh {ssh_host} {}` failed: {}",
            argv.join(" "),
            self.failure_detail()
        )
    }
}

/// Run a command on the remote host over ssh (honoring `$PHUX_SSH`), with
/// `BatchMode=yes` so a missing key errors instead of prompting. `Err` means ssh
/// could not run at all.
pub(crate) fn ssh_run(ssh_host: &str, argv: &[&str]) -> Result<SshOutput, String> {
    let program = ssh_program();
    let output = ssh_command(&program, ssh_host, argv)
        .stdin(Stdio::null())
        .output()
        .map_err(|err| format!("could not run {}: {err}", program.to_string_lossy()))?;
    Ok(SshOutput::from(output))
}

/// [`ssh_run`] with `stdin` piped to the remote command: how public
/// enrollment material (a CSR) reaches the far host without entering argv.
pub(crate) fn ssh_run_with_stdin(
    ssh_host: &str,
    argv: &[&str],
    stdin: &[u8],
) -> Result<SshOutput, String> {
    use std::io::Write as _;

    let program = ssh_program();
    let mut child = ssh_command(&program, ssh_host, argv)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|err| format!("could not run {}: {err}", program.to_string_lossy()))?;
    // The input is a few hundred bytes, well under a pipe buffer, so the
    // write cannot block on output the child has not drained. A remote that
    // exits without reading is reported by its status, not the broken pipe.
    if let Some(mut pipe) = child.stdin.take() {
        let _ = pipe.write_all(stdin);
    }
    let output = child
        .wait_with_output()
        .map_err(|err| format!("could not run {}: {err}", program.to_string_lossy()))?;
    Ok(SshOutput::from(output))
}

/// `ssh -o BatchMode=yes HOST ARGV...`, so a missing key errors instead of
/// prompting.
fn ssh_command(program: &OsString, ssh_host: &str, argv: &[&str]) -> Command {
    let mut command = Command::new(program);
    command
        .arg("-o")
        .arg("BatchMode=yes")
        .arg(ssh_host)
        .args(argv);
    command
}

impl From<std::process::Output> for SshOutput {
    fn from(output: std::process::Output) -> Self {
        Self {
            status: output.status,
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        }
    }
}

/// The ssh program: `$PHUX_SSH` when set, otherwise `ssh`.
pub(crate) fn ssh_program() -> OsString {
    std::env::var_os("PHUX_SSH").unwrap_or_else(|| "ssh".into())
}

/// The host ssh would connect to for `destination`, from `ssh -G`, so an
/// alias with a `HostName` resolves to the machine it names. Falls back to
/// the host spelled in the destination when `ssh -G` has no answer.
pub(crate) fn ssh_hostname(destination: &str) -> String {
    Command::new(ssh_program())
        .arg("-G")
        .arg("--")
        .arg(destination)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()
        .filter(|output| output.status.success())
        .and_then(|output| parse_ssh_g_hostname(&String::from_utf8_lossy(&output.stdout)))
        .unwrap_or_else(|| destination_host(destination))
}

/// The `hostname` line of `ssh -G` output.
fn parse_ssh_g_hostname(output: &str) -> Option<String> {
    output
        .lines()
        .find_map(|line| line.strip_prefix("hostname "))
        .map(str::trim)
        .filter(|host| !host.is_empty())
        .map(str::to_owned)
}

/// The host part of an ssh destination: `[user@]host`, `[user@]host:port`,
/// or `ssh://[user@]host[:port]`, with IPv6 brackets removed.
pub(crate) fn destination_host(destination: &str) -> String {
    let rest = destination.strip_prefix("ssh://").unwrap_or(destination);
    let authority = rest.split('/').next().unwrap_or(rest);
    let host = authority
        .rsplit_once('@')
        .map_or(authority, |(_, host)| host);
    if let Some(inner) = host.strip_prefix('[') {
        return inner
            .split_once(']')
            .map_or(inner, |(host, _)| host)
            .to_owned();
    }
    if host.matches(':').count() == 1 {
        return host
            .split_once(':')
            .map_or(host, |(host, _)| host)
            .to_owned();
    }
    host.to_owned()
}

/// `HOST:PORT`, bracketing an IPv6 literal.
pub(crate) fn authority(host: &str, port: u16) -> String {
    if host.contains(':') {
        format!("[{host}]:{port}")
    } else {
        format!("{host}:{port}")
    }
}

/// How long one probe dial may take: [`PROBE_DEADLINE`], or the test seam.
fn probe_deadline() -> Duration {
    std::env::var(PROBE_DEADLINE_ENV)
        .ok()
        .and_then(|raw| raw.trim().parse::<u64>().ok())
        .filter(|ms| *ms > 0)
        .map_or(PROBE_DEADLINE, Duration::from_millis)
}

/// Dial `target` (`HOST:PORT`) once, briefly, with the given credentials, to
/// learn whether a pinned QUIC route reaches a listener at all.
pub(crate) fn probe(
    target: &str,
    token: &str,
    cert_fingerprint: Option<&str>,
    identity: Option<TlsClientIdentity>,
) -> Result<(), String> {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|err| format!("could not build a runtime: {err}"))?;
    let plan = super::attach::plan_quic_dial(
        &rt,
        target,
        Some(token.to_owned()),
        cert_fingerprint.map(str::to_owned),
        None,
    )
    .map_err(|refusal| format!("{refusal:?}"))?
    .with_identity(identity);
    let deadline = probe_deadline();
    rt.block_on(async {
        match tokio::time::timeout(deadline, Connection::connect_dial(&plan.dial)).await {
            Ok(Ok(_connection)) => Ok(()),
            Ok(Err(err)) => Err(err.to_string()),
            Err(_) => Err(format!(
                "no answer within {}s; is UDP filtered?",
                deadline.as_secs_f64()
            )),
        }
    })
}

/// The default QUIC port, exposed for the CLI's `default_value_t`.
pub(crate) const fn default_quic_port() -> u16 {
    DEFAULT_QUIC_PORT
}

#[cfg(test)]
mod tests {
    use super::{
        AddKeyReply, CertificateStatus, ClientIdentityFiles, HeldIdentity, PairReport,
        RENEW_WITHIN_SECONDS, admission_ends, authority, candidate_endpoints, destination_host,
        identity_stem, parse_ssh_g_hostname, renewal_due, token_path, write_token,
    };
    use std::path::{Path, PathBuf};

    /// A self-signed certificate valid until `days` from now (day
    /// granularity), written as PEM to `path`.
    fn certificate_until(path: &Path, days: i64) {
        use chrono::Datelike as _;
        let key = rcgen::KeyPair::generate().expect("key");
        let mut params = rcgen::CertificateParams::new(vec!["client".to_owned()]).expect("params");
        let at = chrono::Utc::now() + chrono::Duration::days(days);
        let narrow = |value: u32| u8::try_from(value).expect("fits");
        params.not_after = rcgen::date_time_ymd(at.year(), narrow(at.month()), narrow(at.day()));
        let pem = params.self_signed(&key).expect("self-signed").pem();
        std::fs::write(path, pem).expect("write certificate");
    }

    /// A certificate is due for renewal when admission ends within the
    /// window, has ended, or cannot be read; admission is taken to end the
    /// day before `notAfter`, where issuance rounds the registry expiry.
    #[test]
    fn renewal_is_due_inside_the_window_expired_or_unreadable() {
        let dir = tempfile::tempdir().expect("tempdir");
        let now = chrono::Utc::now().timestamp();
        let path = |name: &str| dir.path().join(name);
        certificate_until(&path("long.pem"), 60);
        certificate_until(&path("edge.pem"), 16);
        certificate_until(&path("soon.pem"), 10);
        certificate_until(&path("gone.pem"), -2);
        std::fs::write(path("junk.pem"), "not a certificate").expect("junk");

        assert_eq!(renewal_due(&path("long.pem"), now), None);
        assert_eq!(
            renewal_due(&path("edge.pem"), now),
            None,
            "16 days is outside"
        );
        let ends = admission_ends(&path("long.pem")).expect("reads");
        assert!(
            ends - now > 58 * 86_400 && ends - now < 60 * 86_400,
            "{}",
            ends - now
        );
        let soon = renewal_due(&path("soon.pem"), now).expect("inside the window");
        assert!(soon.starts_with("expires on "), "{soon}");
        assert!(soon.contains("day(s)"), "{soon}");
        let gone = renewal_due(&path("gone.pem"), now).expect("expired");
        assert!(gone.starts_with("expired on "), "{gone}");
        for unreadable in ["junk.pem", "missing.pem"] {
            let why = renewal_due(&path(unreadable), now).expect("unreadable");
            assert!(why.contains("cannot be read"), "{why}");
        }
        assert!(
            renewal_due(&path("long.pem"), now + 60 * 86_400 - RENEW_WITHIN_SECONDS).is_some(),
            "the same certificate falls due as time passes"
        );
    }

    /// The `enrollment.status` vocabulary is stable, and only a failure
    /// carries an error.
    #[test]
    fn certificate_status_strings_are_stable() {
        let failed = CertificateStatus::Failed("no phux".to_owned());
        assert_eq!(failed.as_str(), "failed");
        assert_eq!(failed.error(), Some("no phux"));
        for (status, text) in [
            (CertificateStatus::Enrolled, "enrolled"),
            (CertificateStatus::Kept, "kept"),
            (CertificateStatus::Skipped, "skipped"),
        ] {
            assert_eq!(status.as_str(), text);
            assert_eq!(status.error(), None);
        }
    }

    /// A half-named identity still names the credential it replaces when an
    /// enrollment wrote it: from the certificate the entry names, or from
    /// the enrolled certificate beside a lone enrolled key. Both files of
    /// an enrolled pair are cleaned up; files the operator named are not.
    #[test]
    fn a_half_named_identity_is_retired_through_its_enrolled_pair() {
        let dir = tempfile::tempdir().expect("tempdir");
        let stem = identity_stem("mini", ID);
        let (cert, key) = (
            dir.path().join(format!("{stem}.pem")),
            dir.path().join(format!("{stem}.key")),
        );
        certificate_until(&cert, 30);
        let id = phux_server::workload::stored_credential_id(&cert).expect("id");
        let held = |certificate: Option<&PathBuf>, private_key: Option<&PathBuf>| HeldIdentity {
            certificate: certificate.cloned(),
            private_key: private_key.cloned(),
        };
        let both = vec![cert.clone(), key.clone()];

        let whole = held(Some(&cert), Some(&key));
        assert!(whole.pair().is_some());
        assert_eq!(
            whole.credential_id(dir.path(), "mini").as_deref(),
            Some(id.as_str())
        );
        assert_eq!(whole.enrolled_files(dir.path(), "mini"), both);

        let cert_only = held(Some(&cert), None);
        assert!(cert_only.pair().is_none() && !cert_only.is_empty());
        assert_eq!(
            cert_only.credential_id(dir.path(), "mini").as_deref(),
            Some(id.as_str())
        );
        assert_eq!(cert_only.enrolled_files(dir.path(), "mini"), both);

        let key_only = held(None, Some(&key));
        assert_eq!(
            key_only.credential_id(dir.path(), "mini").as_deref(),
            Some(id.as_str()),
            "the enrolled certificate beside the key names it"
        );
        assert_eq!(key_only.enrolled_files(dir.path(), "mini"), both);
        assert_eq!(
            key_only.credential_id(dir.path(), "other"),
            None,
            "not ours"
        );

        std::fs::remove_file(&cert).expect("lose the certificate");
        assert_eq!(key_only.credential_id(dir.path(), "mini"), None);

        let theirs = dir.path().join("mine.key");
        let operator = held(None, Some(&theirs));
        assert_eq!(operator.credential_id(dir.path(), "mini"), None);
        assert!(operator.enrolled_files(dir.path(), "mini").is_empty());
        assert!(held(None, None).is_empty());
    }

    const ID: &str = "sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

    /// The far host's `add-key` reply is read past shell noise, and every
    /// field it must carry is checked before the chain is even parsed.
    #[test]
    fn the_add_key_reply_is_untrusted_and_checked() {
        let doc = serde_json::json!({
            "schema_version": 1,
            "operation": "add-key",
            "credential_id": ID,
            "certificate_chain": "-----BEGIN CERTIFICATE-----",
            "replaced": true,
        });
        let pretty = serde_json::to_string_pretty(&doc).expect("json");
        let reply = AddKeyReply::parse(&format!("Welcome to box\n{pretty}\n")).expect("parse");
        assert_eq!(reply.credential_id, ID);

        let mut no_chain = doc.clone();
        no_chain
            .as_object_mut()
            .expect("object")
            .remove("certificate_chain");
        let older = AddKeyReply::parse(&no_chain.to_string()).expect_err("an older phux");
        assert!(older.contains("certificate_chain"), "{older}");

        let mut bad_id = doc.clone();
        bad_id["credential_id"] = serde_json::json!("sha256:ABC");
        assert!(AddKeyReply::parse(&bad_id.to_string()).is_err());

        let mut other = doc;
        other["operation"] = serde_json::json!("revoke");
        assert!(AddKeyReply::parse(&other.to_string()).is_err());
        assert!(AddKeyReply::parse("not json").is_err());
    }

    /// One pair of files per enrollment, named by the registry name and the
    /// credential, so a re-enrollment never overwrites the pair in use.
    #[test]
    fn identity_files_are_named_per_credential() {
        assert_eq!(identity_stem("mini", ID), "mini.client.0123456789abcdef");
        assert_ne!(
            identity_stem("mini", ID),
            identity_stem("mini", &ID.replace("0123", "fedc"))
        );
    }

    /// Only a pair enrolled under this name in this directory is ever
    /// cleaned up; files the operator named themselves are not.
    #[test]
    fn only_an_enrolled_pair_is_ours_to_remove() {
        let dir = Path::new("/state/remotes");
        let pair = |cert: &str, key: &str| ClientIdentityFiles {
            certificate: Path::new(cert).to_path_buf(),
            private_key: Path::new(key).to_path_buf(),
            credential_id: None,
            fresh: false,
        };
        let stem = identity_stem("mini", ID);
        let ours = pair(
            &format!("/state/remotes/{stem}.pem"),
            &format!("/state/remotes/{stem}.key"),
        );
        assert!(ours.enrolled_under(dir, "mini"));
        assert!(!ours.enrolled_under(dir, "mini2"), "another name's pair");
        assert!(!ours.enrolled_under(Path::new("/elsewhere"), "mini"));
        for (cert, key) in [
            ("/home/me/client.pem", "/home/me/client.key"),
            ("/state/remotes/mini.pem", "/state/remotes/mini.key"),
            (
                &*format!("/state/remotes/sub/{stem}.pem"),
                &*format!("/state/remotes/sub/{stem}.key"),
            ),
            (
                &*format!("/state/remotes/{stem}.pem"),
                "/home/me/client.key",
            ),
            (
                "/state/remotes/mini.client.not-hex.pem",
                "/state/remotes/mini.client.not-hex.key",
            ),
        ] {
            assert!(!pair(cert, key).enrolled_under(dir, "mini"), "{cert}");
        }
    }

    fn report(fp: Option<&str>, overlay: &[&str]) -> PairReport {
        PairReport {
            token: "deadbeef".to_owned(),
            cert_fingerprint: fp.map(str::to_owned),
            overlay_addresses: overlay.iter().map(|a| (*a).to_owned()).collect(),
        }
    }

    #[test]
    fn parses_the_pair_document() {
        let parsed = PairReport::parse(
            r#"{
              "token": "abc123",
              "cert_fingerprint": "AB:CD",
              "overlay_addresses": ["100.64.0.2"],
              "ws_addr": null,
              "quic_addr": "0.0.0.0:8788"
            }"#,
        )
        .expect("parse");
        assert_eq!(parsed.token, "abc123");
        assert_eq!(parsed.cert_fingerprint.as_deref(), Some("AB:CD"));
        assert_eq!(parsed.overlay_addresses, vec!["100.64.0.2".to_owned()]);
    }

    #[test]
    fn a_pretty_printed_document_with_a_multi_line_array_is_read_whole() {
        // Pretty-printed output puts an overlay address alone on a line as a bare
        // JSON string; it must not be mistaken for the document.
        let stdout = "Last login: never\n{\n  \"cert_fingerprint\": \"AB:CD\",\n  \"overlay_addresses\": [\n    \"100.79.155.27\"\n  ],\n  \"quic_addr\": null,\n  \"schema_version\": 1,\n  \"token\": \"863e\",\n  \"ws_addr\": \":8787\"\n}\n";
        let parsed = PairReport::parse(stdout).expect("parse");
        assert_eq!(parsed.token, "863e");
        assert_eq!(parsed.cert_fingerprint.as_deref(), Some("AB:CD"));
        assert_eq!(parsed.overlay_addresses, vec!["100.79.155.27".to_owned()]);
    }

    #[test]
    fn tolerates_unknown_fields_from_a_newer_remote() {
        // A newer remote phux must be able to add keys without breaking an
        // older `phux host add`.
        let parsed = PairReport::parse(r#"{"token":"t","brand_new_field":42}"#).expect("parse");
        assert_eq!(parsed.token, "t");
        assert_eq!(parsed.cert_fingerprint, None);
        assert!(parsed.overlay_addresses.is_empty());
    }

    #[test]
    fn the_document_is_found_past_startup_noise() {
        // A shell startup file that prints on a non-interactive login must
        // not hide the one-line document.
        let parsed = PairReport::parse("Welcome to box\n{\"token\":\"t\"}\n\n").expect("parse");
        assert_eq!(parsed.token, "t");
    }

    #[test]
    fn refuses_a_document_with_no_token() {
        assert!(PairReport::parse(r#"{"cert_fingerprint":"AB"}"#).is_err());
        assert!(PairReport::parse(r#"{"token":""}"#).is_err());
        // Not JSON at all — the shape of an ssh banner leaking into stdout.
        assert!(PairReport::parse("Welcome to Ubuntu\n").is_err());
    }

    #[test]
    fn candidates_are_overlay_addresses_then_the_ssh_host_when_pinned() {
        assert_eq!(
            candidate_endpoints("mini.lan", &report(Some("AB"), &["100.64.0.2"]), None, 8788),
            vec![
                "quic://100.64.0.2:8788".to_owned(),
                "quic://mini.lan:8788".to_owned()
            ]
        );
        // The ssh host is not listed twice when it is the overlay address.
        assert_eq!(
            candidate_endpoints(
                "100.64.0.2",
                &report(Some("AB"), &["100.64.0.2"]),
                None,
                8788
            ),
            vec!["quic://100.64.0.2:8788".to_owned()]
        );
        // No overlay address: the ssh host alone is still worth a dial.
        assert_eq!(
            candidate_endpoints("mini", &report(Some("AB"), &[]), None, 8788),
            vec!["quic://mini:8788".to_owned()]
        );
    }

    #[test]
    fn nothing_is_dialable_without_a_pin() {
        // ADR-0031 would refuse the dial, so registering it would only move
        // the failure later.
        assert!(candidate_endpoints("mini", &report(None, &["100.64.0.2"]), None, 8788).is_empty());
    }

    #[test]
    fn operator_endpoint_override_is_the_only_candidate() {
        let report = report(Some("AB"), &["100.64.0.2"]);
        assert_eq!(
            candidate_endpoints("mini", &report, Some("mini.ts.net:9999"), 8788),
            vec!["quic://mini.ts.net:9999".to_owned()]
        );
        // A full URI passes through, so wss:// is reachable this way.
        assert_eq!(
            candidate_endpoints("mini", &report, Some("wss://mini.ts.net:8787"), 8788),
            vec!["wss://mini.ts.net:8787".to_owned()]
        );
    }

    #[test]
    fn ipv6_overlay_address_is_bracketed() {
        // Without brackets the result would not parse as HOST:PORT.
        assert_eq!(
            candidate_endpoints("", &report(Some("AB"), &["fd7a::1"]), None, 8788),
            vec!["quic://[fd7a::1]:8788".to_owned()]
        );
    }

    #[test]
    fn ssh_g_names_the_real_host() {
        let output = "user me\nhostname 10.0.0.5\nport 22\n";
        assert_eq!(parse_ssh_g_hostname(output).as_deref(), Some("10.0.0.5"));
        assert_eq!(parse_ssh_g_hostname("user me\n"), None);
    }

    #[test]
    fn destination_host_reads_every_ssh_spelling() {
        assert_eq!(destination_host("box"), "box");
        assert_eq!(destination_host("me@box"), "box");
        assert_eq!(destination_host("me@box:2222"), "box");
        assert_eq!(destination_host("ssh://me@box:2222"), "box");
        assert_eq!(destination_host("ssh://me@[fd7a::1]:2222"), "fd7a::1");
        assert_eq!(destination_host("fd7a::1"), "fd7a::1");
    }

    #[test]
    fn authority_brackets_ipv6() {
        assert_eq!(authority("box", 60123), "box:60123");
        assert_eq!(authority("fd7a::1", 60123), "[fd7a::1]:60123");
    }

    #[test]
    fn token_is_written_owner_only() {
        use std::os::unix::fs::PermissionsExt as _;

        let dir = tempfile::tempdir().expect("tempdir");
        let path = token_path(dir.path(), "mini");
        write_token(&path, "deadbeef").expect("write");

        assert_eq!(std::fs::read_to_string(&path).expect("read"), "deadbeef\n");
        let mode = std::fs::metadata(&path).expect("stat").permissions().mode();
        assert_eq!(mode & 0o777, 0o600, "a bearer token must not be readable");
    }

    #[test]
    fn token_path_is_namespaced_per_remote() {
        let state = Path::new("/state");
        assert_eq!(
            token_path(state, "mini"),
            Path::new("/state/remotes/mini.token")
        );
        assert_ne!(token_path(state, "mini"), token_path(state, "studio"));
    }
}
