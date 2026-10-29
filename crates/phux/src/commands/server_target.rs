//! Which server a headless session verb (`ls`, `new`, `kill`, `rename`,
//! `detach`) talks to: the local socket, or a `--remote` host resolved through
//! the same ladder and dial as `phux attach --remote`.
//!
//! The headless verbs take no `--code`/`--no-enroll`. Without `--json` an
//! unregistered host still pairs over ssh; with `--json` the ssh rung is
//! skipped, because a machine-readable call must not narrate or prompt. Every
//! refusal is reported once, worded for the registry entry rather than for
//! `phux attach`'s flags.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use phux_client::attach::connection::Connection;
use phux_client::attach::{AttachError, Dial};
use phux_client::state::StateView;
use phux_server::runtime::default_socket_path;

use super::attach::{self, DialPlan, DialRefusal};
use super::json_err::{self, CliError, codes};
use super::remote::{self, Endpoint, RemoteEntry};
use super::remote_target::{self, Bootstrap};

/// The refusal for `--socket` next to `--remote`. One names a local UDS, the
/// other a network dial, and silently preferring either would run the verb
/// against a server the operator did not mean.
pub(crate) const SOCKET_REMOTE_CONFLICT: &str =
    "--socket dials a local UDS and cannot combine with --remote; drop one";

/// How the operator named the server, before anything is resolved.
#[derive(Debug, Default)]
pub(crate) struct ServerSpec {
    /// The global `--socket` override.
    pub(crate) socket: Option<PathBuf>,
    /// The verb's `--remote [USER@]HOST[:PORT]`.
    pub(crate) remote: Option<String>,
}

/// A resolved server: where the verb's connection goes.
#[derive(Debug)]
pub(crate) enum ServerTarget {
    /// The local server's Unix socket.
    Local(PathBuf),
    /// A registered remote, dialed over QUIC or WSS. Boxed: its dial plan
    /// dwarfs a socket path.
    Remote(Box<RemoteServer>),
}

/// A remote server resolved to a dial, plus what its diagnostics name.
#[derive(Debug)]
pub(crate) struct RemoteServer {
    name: String,
    endpoint: String,
    dial: Dial,
    loopback: bool,
}

impl ServerSpec {
    /// A spec that can only mean the local server.
    pub(crate) const fn local(socket: Option<PathBuf>) -> Self {
        Self {
            socket,
            remote: None,
        }
    }

    /// Build the verb's runtime and resolve the server it talks to.
    ///
    /// `verb` names the command in diagnostics. Failures are reported here,
    /// as prose or as one JSON line under `json`, so a verb only has to
    /// return the exit code.
    pub(crate) fn prepare(
        self,
        verb: &str,
        json: bool,
    ) -> Result<(tokio::runtime::Runtime, ServerTarget), ExitCode> {
        let rt = super::cli_runtime()?;
        let target = self.resolve(&rt, verb, json)?;
        Ok((rt, target))
    }

    /// Resolve the spec: the local socket unless `--remote` names a host.
    ///
    /// The `--socket`/`--remote` pair is refused post-parse before any verb
    /// runs; the guard here keeps this helper honest for a caller that did
    /// not come through the CLI.
    pub(crate) fn resolve(
        self,
        rt: &tokio::runtime::Runtime,
        verb: &str,
        json: bool,
    ) -> Result<ServerTarget, ExitCode> {
        let Some(raw) = self.remote else {
            return Ok(ServerTarget::Local(
                self.socket.unwrap_or_else(default_socket_path),
            ));
        };
        if self.socket.is_some() {
            error!("{SOCKET_REMOTE_CONFLICT}");
            return Err(ExitCode::from(2));
        }
        resolve_remote(rt, &raw, verb, json).map(|remote| ServerTarget::Remote(Box::new(remote)))
    }
}

/// Walk attach's resolution ladder for `raw`, then turn the entry into a dial.
fn resolve_remote(
    rt: &tokio::runtime::Runtime,
    raw: &str,
    verb: &str,
    json: bool,
) -> Result<RemoteServer, ExitCode> {
    let target = remote_target::parse_target(raw).map_err(|err| {
        json_err::emit(json, &CliError::new(codes::REMOTE_UNRESOLVED, err, ""), 2)
    })?;
    // Attach's own resolver; JSON automation and deadline-bounded transfers never pair.
    let entry = remote_target::resolve(
        &target,
        None,
        bootstrap_for(json || matches!(verb, "upload" | "download")),
    )
    .map_err(|report| emit_ladder_refusal(json, &report))?;
    dial_entry(rt, &entry, verb, json)
}

/// Whether a cold target may pair over ssh: as under attach, except for a
/// `--json` call, which must neither narrate on stderr nor block on a prompt.
const fn bootstrap_for(json: bool) -> Bootstrap {
    if json {
        Bootstrap::Never
    } else {
        Bootstrap::Auto
    }
}

/// Turn a registered entry into a dial, reporting any refusal on the `--json`
/// contract with exit code 1. `ssh://` entries carry only an interactive
/// attach, so they are refused.
fn dial_entry(
    rt: &tokio::runtime::Runtime,
    entry: &RemoteEntry,
    verb: &str,
    json: bool,
) -> Result<RemoteServer, ExitCode> {
    plan_entry(rt, entry, verb).map_err(|err| json_err::emit(json, &err, 1))
}

/// Plan a registered entry's dial without printing anything: the refusal is
/// returned for the caller to report, which is what lets `phux ls --all`
/// fold one bad entry into its row instead of failing the whole listing.
pub(crate) fn plan_entry(
    rt: &tokio::runtime::Runtime,
    entry: &RemoteEntry,
    verb: &str,
) -> Result<RemoteServer, CliError> {
    let endpoint = Endpoint::parse(&entry.endpoint)
        .map_err(|err| entry_refusal(entry, &DialRefusal::Malformed(err)))?;
    let token = remote::read_token(entry)
        .map_err(|err| unusable_entry(entry, &err, &re_pair_remedy(&entry.name)))?;
    let fingerprint = entry.cert_fingerprint.clone();
    let server_name = entry.tls_server_name.clone();
    let identity = entry
        .client_identity()
        .map_err(|err| unusable_entry(entry, &err, &re_pair_remedy(&entry.name)))?;
    entry.warn_if_renewal_due();
    let plan = match endpoint {
        Endpoint::Quic(addr) => attach::plan_quic_dial(rt, &addr, token, fingerprint, server_name),
        Endpoint::Ws(url) => attach::plan_ws_dial(url, token, fingerprint, server_name),
        Endpoint::Ssh(destination) => {
            return Err(ssh_only_refusal(&entry.name, &destination, verb));
        }
    };
    let DialPlan { dial, loopback } = plan
        .and_then(|plan| {
            plan.with_identity(identity)
                .with_authority(&entry.name, &entry.authority_pin())
        })
        .map_err(|refusal| entry_refusal(entry, &refusal))?;
    Ok(RemoteServer {
        name: entry.name.clone(),
        endpoint: entry.endpoint.clone(),
        dial,
        loopback,
    })
}

/// The contract error for a registry entry the dial planner refused. A failed
/// name lookup is `transport` (the network may be down); anything else means
/// the entry cannot be dialed as registered.
fn entry_refusal(entry: &RemoteEntry, refusal: &DialRefusal) -> CliError {
    let name = &entry.name;
    let endpoint = &entry.endpoint;
    let re_pair = re_pair_remedy(name);
    let with_credentials = format!(
        "{re_pair}, or record them by hand with the values `phux pair` prints on that host: \
         `phux host add {name} {endpoint} --token-file PATH --cert-fingerprint FP`"
    );
    match refusal {
        DialRefusal::Unresolved {
            detail, dns_name, ..
        } => CliError::new(
            codes::TRANSPORT,
            format!("remote {name:?} ({endpoint}) did not resolve: {detail}"),
            unresolved_remedy(*dns_name, &re_pair),
        ),
        DialRefusal::Malformed(err) => unusable_entry(
            entry,
            &format!("its endpoint does not parse: {err}"),
            &re_pair,
        ),
        DialRefusal::UnpinnedQuic { .. } | DialRefusal::UnpinnedWs { .. } => unusable_entry(
            entry,
            "it is routable but records no certificate pin",
            &with_credentials,
        ),
        DialRefusal::NoToken { .. } => unusable_entry(
            entry,
            "it is a routable wss:// endpoint with no pairing token",
            &with_credentials,
        ),
        DialRefusal::Plaintext { .. } => unusable_entry(
            entry,
            "it is plaintext ws:// to a routable address",
            &format!("{re_pair}, or register the host's wss:// endpoint with `phux host add`"),
        ),
        // The detail is deliberately not echoed: it describes a secret.
        DialRefusal::BadToken(_) => unusable_entry(
            entry,
            "its token file does not hold a valid hex pairing token",
            &re_pair,
        ),
    }
}

/// A `remote_unresolved` error for an entry that cannot be dialed as
/// registered.
fn unusable_entry(entry: &RemoteEntry, why: &str, remedy: &str) -> CliError {
    CliError::new(
        codes::REMOTE_UNRESOLVED,
        format!(
            "remote {:?} ({}) cannot be dialed as registered: {why}",
            entry.name, entry.endpoint
        ),
        remedy,
    )
}

/// The first remedy for any bad entry: re-pairing rewrites it whole.
fn re_pair_remedy(name: &str) -> String {
    format!("re-pair the host so its registry entry is rewritten: `phux host add {name}`")
}

/// The way out of a name that did not resolve: the overlay hint for a DNS
/// name, then the entry, in case the host moved.
fn unresolved_remedy(dns_name: bool, re_pair: &str) -> String {
    let mut lines: Vec<String> = Vec::new();
    if dns_name {
        lines.extend(
            attach::OVERLAY_REACHABILITY_HINT
                .lines()
                .map(|line| line.trim().to_owned()),
        );
    }
    lines.push(format!("if the host's address changed, {re_pair}"));
    lines.join("\n")
}

/// Why an `ssh://` entry cannot carry a headless verb, and the two ways out.
fn ssh_only_refusal(name: &str, destination: &str, verb: &str) -> CliError {
    CliError::new(
        codes::REMOTE_UNRESOLVED,
        format!("remote {name:?} is an ssh:// entry, which carries an interactive attach only"),
        format!(
            "`phux host add {name}` registers a direct QUIC endpoint the session verbs can dial;\n\
             until then, run the command on the host: `ssh {destination} phux {verb} ...`"
        ),
    )
}

/// Print a ladder refusal: the same prose lines `phux attach --remote`
/// prints, or one JSON line under `json`.
fn emit_ladder_refusal(json: bool, report: &str) -> ExitCode {
    if !json {
        crate::output::error_block(report.lines());
        return ExitCode::FAILURE;
    }
    json_err::emit(true, &ladder_refusal_error(report), 1)
}

/// The JSON form of a ladder refusal: its first line is the message and the
/// rest, with their `phux:` prefixes stripped, is the remedy. Pure for tests.
fn ladder_refusal_error(report: &str) -> CliError {
    let mut lines = report
        .lines()
        .map(|line| line.trim_start_matches("phux:").trim());
    let message = lines.next().unwrap_or_default().to_owned();
    let remedy = lines.collect::<Vec<_>>().join("\n");
    CliError::new(codes::REMOTE_UNRESOLVED, message, remedy)
}

impl ServerTarget {
    /// The local server at `path`, for callers that only ever mean it.
    pub(crate) fn local(path: &Path) -> Self {
        Self::Local(path.to_path_buf())
    }

    /// The local socket path, when the target is local.
    pub(crate) fn socket_path(&self) -> Option<&Path> {
        match self {
            Self::Local(path) => Some(path),
            Self::Remote(_) => None,
        }
    }

    /// Whether the server lives on another machine.
    pub(crate) const fn is_remote(&self) -> bool {
        matches!(self, Self::Remote(_))
    }

    /// The dial for an attach against this server.
    pub(crate) fn dial(&self) -> Dial {
        match self {
            Self::Local(path) => Dial::uds(path),
            Self::Remote(remote) => remote.dial.clone(),
        }
    }

    /// Open a negotiated control connection (not an attach).
    pub(crate) async fn connect(&self) -> Result<Connection, AttachError> {
        match self {
            Self::Local(path) => Connection::connect(path).await,
            Self::Remote(remote) => Connection::connect_dial(&remote.dial).await,
        }
    }

    /// Connect and fetch the server-wide session snapshot.
    pub(crate) async fn get_state(&self) -> Result<StateView, AttachError> {
        let mut conn = self.connect().await?;
        phux_client::state::get_state_on(&mut conn).await
    }

    /// Report a failure talking to this server and return exit code 1.
    ///
    /// The local arm is the existing no-server diagnostic, unchanged. The
    /// remote arm names the host and endpoint, and adds the overlay hint when
    /// the dial never got an answer.
    pub(crate) fn report_unreachable(&self, json: bool, err: &AttachError, verb: &str) -> ExitCode {
        match self {
            Self::Local(path) => json_err::report_no_server(json, err, path, verb),
            Self::Remote(remote) => json_err::emit(json, &remote.failure(err, verb), 1),
        }
    }

    /// Report a failed attach, for `phux new`'s create-and-attach path.
    pub(crate) fn report_attach_failure(&self, err: &AttachError, session: &str) {
        match self {
            Self::Local(path) => super::print_attach_error(err, path, session),
            Self::Remote(remote) => {
                let verb = format!("attach to session {session:?}");
                let _ = json_err::emit(false, &remote.failure(err, &verb), 1);
            }
        }
    }
}

impl RemoteServer {
    /// The contract error for a failed exchange with this remote.
    fn failure(&self, err: &AttachError, verb: &str) -> CliError {
        let code = if matches!(err, AttachError::Disconnected) {
            codes::SERVER_DISCONNECTED
        } else {
            codes::TRANSPORT
        };
        CliError::new(
            code,
            format!(
                "{verb} on remote {} ({}) failed: {err}",
                self.name, self.endpoint
            ),
            self.remedy(err),
        )
    }

    /// The way out: the overlay hint for an unanswered dial, else where to
    /// look on each end.
    fn remedy(&self, err: &AttachError) -> String {
        attach::reachability_hint(err, self.loopback).map_or_else(
            || {
                format!(
                    "confirm the server on {} is running (`phux status` there) and that \
                     `phux host ls` lists the endpoint you expect",
                    self.name
                )
            },
            |hint| hint.lines().map(str::trim).collect::<Vec<_>>().join("\n"),
        )
    }
}

#[cfg(test)]
mod tests;
