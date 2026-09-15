//! The shared ssh middle of `phux host add HOST` — bring another
//! machine's server up and pair with it over ssh (ADR-0055, ADR-0066,
//! ADR-0122).
//!
//! The remote path already worked and was still unusable: `phux pair` prints
//! a 64-hex token and a 64-hex fingerprint, and the operator retypes both
//! into a command line they retype again on every attach. The unexploited
//! asset is ssh. Anyone self-hosting a phux server already holds ssh trust to
//! that host — they used it to install phux — and that channel is
//! authenticated, confidential, and already inside their threat model.
//!
//! So enrollment grants no authority ssh did not already grant: whoever can
//! `ssh HOST` can run `phux pair` there and read the token themselves. What
//! it removes is a transcription error class, and the reason nobody used the
//! remote path.
//!
//! The flow, all over one ssh channel, in the order an operator would do it
//! by hand:
//!
//! 1. confirm `phux` is on the remote `PATH` ([`remote_phux_version`]);
//! 2. make sure a server is running there and will keep running
//!    ([`ensure_remote_server`]): install the remote's service unit, adopt
//!    a server that is already live, or fall back to an unsupervised
//!    `phux server --ensure`;
//! 3. run `phux pair --json` there and read back token + fingerprint,
//!    migrating a pre-versioned token store once if that is what stops it;
//! 4. list the direct routes worth trying — an operator-supplied address,
//!    the host's detected overlay addresses, the host ssh itself connects
//!    to — and dial each one briefly with the minted credentials;
//! 5. hand back the first route that answered, or `ssh://HOST` and the
//!    first candidate as a `direct` route to promote later.
//!
//! Step 5 is the one that ends benignly: a host with nothing dialable is
//! not an error. An `ssh://` entry still gives the operator `phux attach
//! HOST` against a server whose sessions outlive the connection.
//!
//! Nothing here prints: `phux host add` and the attach repair rung each own
//! an output contract, so events carry the facts and the caller renders
//! them. Nothing here writes a registry either — the role-specific tails
//! in `host` own the token path and the entry, so this one flow cannot
//! drift into deciding trust direction.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use phux_client::attach::connection::Connection;

/// Default QUIC port for an enrolled server. Matches the port
/// `docs/remote-access.md` uses throughout.
const DEFAULT_QUIC_PORT: u16 = 8788;

/// How long one direct-route probe may take before the route is judged
/// unreachable. A reachable host answers a QUIC handshake in one round trip;
/// this only fires on a filtered path, where the alternative is waiting out
/// quinn's idle timeout.
const PROBE_DEADLINE: Duration = Duration::from_secs(5);

/// Test seam for [`PROBE_DEADLINE`], in milliseconds. The fleet tests dial
/// TEST-NET addresses that can never answer; without a shorter deadline
/// every candidate costs the full five seconds. Never documented for
/// operators: shortening it turns a slow but reachable host into an
/// `ssh://` entry.
const PROBE_DEADLINE_ENV: &str = "PHUX_DIRECT_PROBE_TIMEOUT_MS";

/// ssh's exit status when ssh itself failed (resolution, connection,
/// authentication), as opposed to the remote command.
const SSH_FAILED: i32 = 255;

/// A POSIX shell's "command not found".
const COMMAND_NOT_FOUND: i32 = 127;

/// What `phux service install` says when a live server already holds the
/// socket. The operator's remedy is `--adopt`; here it is taken for them,
/// because "a server is already running" is exactly the state enrollment
/// wants.
const SERVICE_INCUMBENT_LIVE: &str = "a server is already running on";

/// What `phux pair` says when the token store predates versioning. The
/// migration is a documented, secret-preserving conversion (`phux doctor`
/// names it as the remedy), so enrollment performs it once rather than
/// stopping to relay the instruction.
const LEGACY_TOKEN_STORE: &str = "legacy token store requires explicit migration";

/// What `phux pair --json` reported on the remote host.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PairReport {
    pub(crate) token: String,
    pub(crate) cert_fingerprint: Option<String>,
    pub(crate) overlay_addresses: Vec<String>,
}

impl PairReport {
    /// Parse the document `phux pair --json` writes.
    ///
    /// Tolerant of fields it does not know so a newer remote phux can add
    /// keys without breaking an older `phux host add`; strict about the
    /// two it cannot proceed without.
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

/// The JSON object `phux pair --json` printed, wherever it sits in stdout.
///
/// The document is pretty-printed over many lines, and a shell startup file
/// that prints on a non-interactive login can put text in front of it. Only
/// an object with a `token` key counts: a bare array element such as
/// `"100.64.0.2"` is valid JSON on its own line and must not be mistaken
/// for the document.
fn pair_document_in(stdout: &str) -> Option<serde_json::Value> {
    let is_document = |value: &serde_json::Value| value.get("token").is_some();
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

/// The direct routes worth dialing for an enrolled host, most likely first.
///
/// * an operator-supplied `--endpoint` is the only candidate — they know
///   their network, and a full `wss://` URI is registered without a probe
///   (the probe speaks QUIC);
/// * otherwise every detected overlay address, then the host ssh itself
///   connects to (`ssh -G`), each on the QUIC port — but only when the
///   remote produced a certificate fingerprint, since ADR-0031 refuses an
///   unpinned routable dial and registering one would just move the
///   failure later.
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
///
/// Under the state dir beside the rest of phux's credential material, named
/// for the remote so two enrollments never collide.
pub(crate) fn token_path(state_dir: &Path, name: &str) -> PathBuf {
    state_dir.join("remotes").join(format!("{name}.token"))
}

/// The local path a satellite's token is written to.
///
/// A deliberate sibling of [`token_path`], not a merge: the `remotes/` and
/// `satellites/` directories mirror the split registries (ADR-0066 keeps the
/// trust directions apart), so an entry's role is readable from where its
/// credential lives.
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

/// A progress event from the shared ssh middle.
///
/// The middle does not print: `phux host add` and the attach repair rung
/// each own an output contract — different `--json` suppression rules,
/// different prefixes — so events carry the facts and the caller renders
/// them with [`EnrollEvent::describe`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum EnrollEvent {
    /// `phux --version` answered on the host.
    PhuxFound { version: String },
    /// The service question is settled one way or another.
    ServerReady(Supervision),
    /// Neither the service install nor `phux server --ensure` succeeded.
    /// Not fatal: pairing is still worth doing, and an `ssh://` attach
    /// starts a server on its own.
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
    /// A direct route is being dialed.
    Probing { endpoint: String },
    /// A direct route answered.
    DirectReachable { endpoint: String },
    /// A direct route did not answer within the deadline.
    DirectUnreachable { endpoint: String, reason: String },
}

impl EnrollEvent {
    /// The progress line for this event, without a prefix: callers add the
    /// host name or `phux:` as their contract wants.
    pub(crate) fn describe(&self) -> String {
        match self {
            Self::PhuxFound { version } => format!("{version} found over ssh"),
            Self::ServerReady(supervision) => supervision.describe(),
            Self::ServerStartFailed { error } => format!(
                "could not start a server there ({error}); pairing anyway, an ssh attach starts one on demand"
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
            Self::Probing { endpoint } => format!("trying {endpoint}"),
            Self::DirectReachable { endpoint } => format!("direct route reachable at {endpoint}"),
            Self::DirectUnreachable { endpoint, reason } => {
                format!("{endpoint} did not answer ({reason})")
            }
        }
    }
}

/// Why the shared ssh middle stopped.
///
/// Three variants, because the callers phrase exactly three remedies: an
/// unreachable host has a check-your-ssh fix, a missing `phux` has an
/// install-it fix, and everything else on the pairing path shares one
/// look-at-the-host-or-`--ssh-only` fix.
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
}

/// The shared middle of every ssh enrollment: confirm phux is installed on
/// the host, make sure a server is running and supervised, mint pairing
/// material there, and find a direct route that answers.
///
/// Role-agnostic on purpose — nothing here touches a registry or writes a
/// token.
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

    // `phux pair` writes the token store the server reads at startup, so it
    // must run after the service install — which is also what makes
    // PHUX_QUIC_ADDR visible to the pair invocation's overlay-derived link.
    let report = pair_over_ssh(req.ssh_host, req.remote_phux, on_event)?;
    on_event(EnrollEvent::Paired);

    let ssh_target_host = ssh_hostname(req.ssh_host);
    let candidates = candidate_endpoints(
        &ssh_target_host,
        &report,
        req.endpoint_override,
        req.quic_port,
    );
    let mut tried = Vec::new();
    for candidate in &candidates {
        // Only QUIC can be probed here; a `wss://` override is registered
        // as the operator wrote it, as it always was.
        let Some(target) = candidate.strip_prefix("quic://") else {
            return Ok(EnrollOutcome {
                endpoint: candidate.clone(),
                direct: None,
                tried,
                report,
                supervision,
            });
        };
        on_event(EnrollEvent::Probing {
            endpoint: candidate.clone(),
        });
        tried.push(candidate.clone());
        match probe(target, &report.token, report.cert_fingerprint.as_deref()) {
            Ok(()) => {
                on_event(EnrollEvent::DirectReachable {
                    endpoint: candidate.clone(),
                });
                return Ok(EnrollOutcome {
                    endpoint: candidate.clone(),
                    direct: None,
                    tried,
                    report,
                    supervision,
                });
            }
            Err(reason) => on_event(EnrollEvent::DirectUnreachable {
                endpoint: candidate.clone(),
                reason,
            }),
        }
    }
    Ok(EnrollOutcome {
        endpoint: format!("ssh://{}", req.ssh_host),
        direct: candidates.first().cloned(),
        tried,
        report,
        supervision,
    })
}

/// Make sure a server is running on `ssh_host` and, under
/// [`ServicePolicy::Install`], that it will keep running.
///
/// The attach repair rung calls this on its own when a saved route stops
/// answering: the host was enrolled before, so what is missing is the
/// server, not the credentials. It confirms `phux` first so a host that
/// lost its install is reported as that.
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

/// [`ensure_remote_server`] once `phux` is known to be there.
///
/// Under [`ServicePolicy::Install`], `phux service install --quic` is the
/// one idempotent call that covers every state the host can be in:
///
/// * no unit yet: the unit is written, loaded, and started;
/// * a unit that is loaded but stopped — a server that exited cleanly
///   (`phux kill --server`, or the operator's own `kill`) stays stopped
///   under the deliberate restart policy — is reloaded: `install` unloads
///   and re-bootstraps the unit, which starts it;
/// * a live server already holding the socket: `install` refuses, and the
///   refusal is answered with `--adopt`, which writes and arms the unit
///   without touching that server's panes.
///
/// A host with no service manager, or an install that fails for any other
/// reason, still gets a server: `phux server --ensure` is the same start the
/// naked `phux` does, minus supervision.
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

/// `phux pair --json` on the host, migrating a pre-versioned token store
/// once if that is what refuses the mint.
fn pair_over_ssh(
    ssh_host: &str,
    remote_phux: &str,
    on_event: &mut dyn FnMut(EnrollEvent),
) -> Result<PairReport, EnrollFailure> {
    let first = ssh_run(ssh_host, &[remote_phux, "pair", "--json"]).map_err(EnrollFailure::Pair)?;
    let stdout = if first.status.success() {
        first.stdout
    } else if first.stderr.contains(LEGACY_TOKEN_STORE) {
        let migrated = ssh_run(
            ssh_host,
            &[remote_phux, "pair", "--json", "--migrate-legacy"],
        )
        .map_err(EnrollFailure::Pair)?;
        if !migrated.status.success() {
            return Err(EnrollFailure::Pair(migrated.command_failure(
                ssh_host,
                &[remote_phux, "pair", "--json", "--migrate-legacy"],
            )));
        }
        on_event(EnrollEvent::LegacyStoreMigrated);
        // The server disabled its remote listeners when the store failed to
        // load at boot; only a restart brings them back. `phux upgrade` is
        // the restart that keeps every pane alive.
        match ssh_run(ssh_host, &[remote_phux, "upgrade"]) {
            Ok(upgrade) if upgrade.status.success() => on_event(EnrollEvent::ListenersRestarted),
            Ok(upgrade) => on_event(EnrollEvent::ListenerRestartFailed {
                error: upgrade.failure_detail(),
            }),
            Err(error) => on_event(EnrollEvent::ListenerRestartFailed { error }),
        }
        migrated.stdout
    } else {
        return Err(EnrollFailure::Pair(
            first.command_failure(ssh_host, &[remote_phux, "pair", "--json"]),
        ));
    };
    PairReport::parse(&stdout).map_err(EnrollFailure::Pair)
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

/// Run a command on the remote host over ssh, capturing both streams.
///
/// Honors `$PHUX_SSH`, the same seam the federation hub's satellite dialer
/// uses, so a custom ssh wrapper works for both. `BatchMode=yes` turns a
/// missing key into a prompt-free error rather than a hung enrollment
/// waiting on a password nobody is watching for. `Err` is "ssh could not be
/// run at all"; a command that ran and failed is an `Ok` with its status.
pub(crate) fn ssh_run(ssh_host: &str, argv: &[&str]) -> Result<SshOutput, String> {
    let program = ssh_program();
    let output = Command::new(&program)
        .arg("-o")
        .arg("BatchMode=yes")
        .arg(ssh_host)
        .args(argv)
        .stdin(Stdio::null())
        .output()
        .map_err(|err| format!("could not run {}: {err}", program.to_string_lossy()))?;
    Ok(SshOutput {
        status: output.status,
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
    })
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
    .map_err(|refusal| format!("{refusal:?}"))?;
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
        PairReport, authority, candidate_endpoints, destination_host, parse_ssh_g_hostname,
        token_path, write_token,
    };
    use std::path::Path;

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
        // What the real `phux pair --json` prints: `serde_json` pretty
        // output, where an overlay address sits alone on its own line as a
        // bare JSON string. Scanning lines from the end for "anything that
        // parses" picked that string up and reported no token.
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
