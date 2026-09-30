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
    /// A workload client certificate was enrolled; `replaced` when the far
    /// host revoked the previous one in the same write.
    CertificateEnrolled {
        credential_id: String,
        replaced: bool,
    },
    /// No workload client certificate was enrolled. Not fatal: the host is
    /// paired, and dials fall back to the previous identity or none.
    CertificateNotEnrolled { reason: String },
}

impl EnrollEvent {
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
            Self::CertificateEnrolled {
                credential_id,
                replaced,
            } => {
                let replaced = if *replaced {
                    "; the previous one is revoked"
                } else {
                    ""
                };
                format!("enrolled workload client certificate {credential_id}{replaced}")
            }
            Self::CertificateNotEnrolled { reason } => format!(
                "no workload client certificate enrolled ({reason}); direct dials present none until `phux host add` runs again"
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
    /// `dir`: both directly in `dir` and named `<name>.client.<digest>` with
    /// the extensions [`identity_stem`] gives them. Only such a pair is
    /// removed when a re-enrollment supersedes it; a `client-cert` or
    /// `client-key` the operator pointed at their own files is left alone.
    pub(crate) fn enrolled_under(&self, dir: &Path, name: &str) -> bool {
        let prefix = format!("{name}.client.");
        let is_ours = |path: &Path, extension: &str| {
            path.parent() == Some(dir)
                && path
                    .file_name()
                    .and_then(|file| file.to_str())
                    .and_then(|file| file.strip_prefix(&prefix))
                    .and_then(|rest| rest.strip_suffix(extension))
                    .is_some_and(|digest| {
                        !digest.is_empty() && digest.bytes().all(|b| b.is_ascii_hexdigit())
                    })
        };
        is_ours(&self.certificate, ".pem") && is_ours(&self.private_key, ".key")
    }

    /// Remove the two files, when this enrollment minted them and nothing
    /// will record them.
    pub(crate) fn discard_if_fresh(&self) {
        if self.fresh {
            phux_server::workload::remove_identity_files(&self.private_key, &self.certificate);
        }
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
    let identity = if candidates.is_empty() {
        req.previous_identity.cloned()
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
/// still worked (nothing was re-minted), otherwise a fresh enrollment that
/// replaces it on the far host. A failed enrollment is a warning, never a
/// failed `host add`: the host is paired either way, and the previous
/// identity (if any) is kept.
fn client_identity_for(
    req: &EnrollRequest<'_>,
    reused: bool,
    on_event: &mut dyn FnMut(EnrollEvent),
) -> Option<ClientIdentityFiles> {
    if reused && let Some(previous) = req.previous_identity {
        return Some(previous.clone());
    }
    let workload = req.workload.as_ref()?;
    let replaces = req
        .previous_identity
        .and_then(|previous| previous.credential_id.clone());
    match enroll_client_certificate(req.ssh_host, req.remote_phux, workload, replaces.as_deref()) {
        Ok((identity, revoked_previous)) => {
            on_event(EnrollEvent::CertificateEnrolled {
                credential_id: identity.credential_id.clone().unwrap_or_default(),
                replaced: revoked_previous,
            });
            Some(identity)
        }
        Err(reason) => {
            on_event(EnrollEvent::CertificateNotEnrolled { reason });
            req.previous_identity.cloned()
        }
    }
}

/// Enroll a workload client certificate on `ssh_host` (ADR-0116): generate
/// the key here, send only its CSR over ssh stdin to `phux workload add-key
/// --cert-stdout`, validate the returned chain against the key, and store
/// both owner-only under new names. `replaces` is revoked on the far host in
/// the same registry write. Returns the identity and whether a predecessor
/// was revoked.
fn enroll_client_certificate(
    ssh_host: &str,
    remote_phux: &str,
    workload: &WorkloadEnrollment<'_>,
    replaces: Option<&str>,
) -> Result<(ClientIdentityFiles, bool), String> {
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
    let mut argv = vec![
        remote_phux,
        "workload",
        "add-key",
        "--json",
        "--cert-stdout",
        "--scope",
        HOST_ADD_WORKLOAD_SCOPE,
    ];
    if let Some(replaces) = replaces {
        argv.extend(["--replace", replaces]);
    }
    let added = ssh_run_with_stdin(ssh_host, &argv, request.csr_pem().as_bytes())?;
    if !added.status.success() {
        return Err(format!(
            "`phux workload add-key` failed there: {}",
            added.failure_detail()
        ));
    }
    let reply = AddKeyReply::parse(&added.stdout)?;
    if reply.credential_id != request.credential_id() {
        return Err("the host recorded a different credential than the key sent".to_owned());
    }
    let issued = request
        .accept(&reply.certificate_chain)
        .map_err(|err| err.to_string())?;
    let stem = identity_stem(workload.name, issued.credential_id());
    let files = ClientIdentityFiles {
        certificate: workload.dir.join(format!("{stem}.pem")),
        private_key: workload.dir.join(format!("{stem}.key")),
        credential_id: Some(issued.credential_id().to_owned()),
        fresh: true,
    };
    issued
        .store(&files.private_key, &files.certificate)
        .map_err(|err| format!("could not store the client identity: {err}"))?;
    Ok((files, reply.replaced))
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
    replaced: bool,
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
            replaced: document
                .get("replaced")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false),
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
        AddKeyReply, ClientIdentityFiles, PairReport, authority, candidate_endpoints,
        destination_host, identity_stem, parse_ssh_g_hostname, token_path, write_token,
    };
    use std::path::Path;

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
        assert!(reply.replaced);

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
