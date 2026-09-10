//! Which server a headless session verb talks to: the local socket, or a
//! `--remote` host (phux-c2td.2).
//!
//! `phux attach --remote` owns the resolution ladder (`remote_target`): a
//! registered host, a pasted code, a one-time ssh pairing, an honest refusal.
//! This module lets the session-lifecycle verbs (`ls`, `new`, `kill`,
//! `rename`, `detach`) walk that same ladder and dial the same QUIC/WSS
//! endpoint, so a target means one thing on every verb. A verb resolves one
//! [`ServerTarget`] and connects through it; it branches on local versus
//! remote only where the difference is real (auto-spawning a server, and
//! stopping one, are local acts).
//!
//! The headless verbs do not take `--code` or `--no-enroll`. Those modify the
//! pairing rungs, and pairing is an operator-present act that `phux attach
//! --remote` already covers. Without `--json` an unregistered host still
//! pairs over ssh here, exactly as it would under attach. With `--json` the
//! ssh rung is skipped and the host is refused with the ladder's remedies:
//! pairing narrates on stderr and ssh may prompt, and a machine-readable call
//! promises one JSON line on stderr and must never block on a prompt.
//!
//! Every refusal on the way to a dial is reported here, once, through
//! `json_err::emit`: the dial planners return a typed
//! [`DialRefusal`](attach::DialRefusal) instead of printing, and this module
//! words it for the registry entry the endpoint came from rather than for
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
use super::remote_target::{self, Bootstrap, RemoteTarget};

/// The refusal for `--socket` next to `--remote`. One names a local UDS, the
/// other a network dial, and silently preferring either would run the verb
/// against a server the operator did not mean.
pub(crate) const SOCKET_REMOTE_CONFLICT: &str =
    "phux: --socket dials a local UDS and cannot combine with --remote; drop one";

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
    /// A registered remote, dialed over QUIC or WSS.
    Remote(RemoteServer),
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
            eprintln!("{SOCKET_REMOTE_CONFLICT}");
            return Err(ExitCode::from(2));
        }
        resolve_remote(rt, &raw, verb, json).map(ServerTarget::Remote)
    }
}

/// Walk attach's resolution ladder for `raw`, then turn the entry into a dial.
fn resolve_remote(
    rt: &tokio::runtime::Runtime,
    raw: &str,
    verb: &str,
    json: bool,
) -> Result<RemoteServer, ExitCode> {
    let target = RemoteTarget::parse(raw).map_err(|err| {
        json_err::emit(json, &CliError::new(codes::REMOTE_UNRESOLVED, err, ""), 2)
    })?;
    // The function `phux attach --remote` resolves through, with attach's
    // defaults (no pasted code, ssh pairing allowed) except under `--json`,
    // which never pairs (see the module doc).
    let entry = remote_target::resolve(&target, None, bootstrap_for(json))
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

/// Turn a registered entry into a dial, reporting any refusal on the
/// `--json` contract with exit code 1.
///
/// `ssh://` entries are refused. They carry an interactive attach (`ssh -t
/// HOST phux attach`) and nothing else, so there is no connection here for a
/// headless verb to speak the protocol over.
fn dial_entry(
    rt: &tokio::runtime::Runtime,
    entry: &RemoteEntry,
    verb: &str,
    json: bool,
) -> Result<RemoteServer, ExitCode> {
    let fail = |err: &CliError| json_err::emit(json, err, 1);
    let endpoint = Endpoint::parse(&entry.endpoint)
        .map_err(|err| fail(&entry_refusal(entry, &DialRefusal::Malformed(err))))?;
    let token = remote::read_token(entry)
        .map_err(|err| fail(&unusable_entry(entry, &err, &re_pair_remedy(&entry.name))))?;
    let fingerprint = entry.cert_fingerprint.clone();
    let plan = match endpoint {
        Endpoint::Quic(addr) => attach::plan_quic_dial(rt, &addr, token, fingerprint, None),
        Endpoint::Ws(url) => attach::plan_ws_dial(url, token, fingerprint, None),
        Endpoint::Ssh(destination) => {
            return Err(fail(&ssh_only_refusal(&entry.name, &destination, verb)));
        }
    };
    let DialPlan { dial, loopback } =
        plan.map_err(|refusal| fail(&entry_refusal(entry, &refusal)))?;
    Ok(RemoteServer {
        name: entry.name.clone(),
        endpoint: entry.endpoint.clone(),
        dial,
        loopback,
    })
}

/// The contract error for a registry entry the dial planner refused, worded
/// for the entry rather than for `phux attach`'s flags. Pure for tests.
///
/// A name that did not resolve is a reachability failure (`transport`): the
/// entry may be right and the network down. Every other refusal means the
/// entry cannot be dialed as registered (`remote_unresolved`).
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
    format!("re-pair the host so its registry entry is rewritten: `phux host enroll {name}`")
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
            "`phux host enroll {name}` registers a direct QUIC endpoint the session verbs can dial;\n\
             until then, run the command on the host: `ssh {destination} phux {verb} ...`"
        ),
    )
}

/// Print a ladder refusal: the same prose lines `phux attach --remote`
/// prints, or one JSON line under `json`.
fn emit_ladder_refusal(json: bool, report: &str) -> ExitCode {
    if !json {
        for line in report.lines() {
            eprintln!("{line}");
        }
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
mod tests {
    use std::path::PathBuf;

    use phux_client::attach::{AttachError, CertTrust, Dial};

    use super::{
        ServerSpec, ServerTarget, bootstrap_for, dial_entry, entry_refusal, ladder_refusal_error,
        ssh_only_refusal,
    };
    use crate::commands::attach::DialRefusal;
    use crate::commands::json_err::codes;
    use crate::commands::remote::RemoteEntry;
    use crate::commands::remote_target::Bootstrap;

    fn runtime() -> tokio::runtime::Runtime {
        crate::commands::cli_runtime().expect("runtime")
    }

    fn entry(endpoint: &str, fingerprint: Option<&str>) -> RemoteEntry {
        RemoteEntry {
            index: 0,
            name: "mini".to_owned(),
            endpoint: endpoint.to_owned(),
            token_file: None,
            cert_fingerprint: fingerprint.map(str::to_owned),
            session: None,
        }
    }

    /// The prose path walks attach's full ladder; `--json` never pairs.
    #[test]
    fn only_json_skips_the_ssh_pairing_rung() {
        assert_eq!(bootstrap_for(false), Bootstrap::Auto);
        assert_eq!(bootstrap_for(true), Bootstrap::Never);
    }

    /// No `--remote` means the local socket, exactly as before this module:
    /// `--socket` when given, the default path otherwise.
    #[test]
    fn a_spec_without_remote_is_the_local_socket() {
        let rt = runtime();
        let explicit = ServerSpec::local(Some(PathBuf::from("/tmp/x.sock")))
            .resolve(&rt, "ls", false)
            .expect("local resolves");
        assert_eq!(
            explicit.socket_path(),
            Some(PathBuf::from("/tmp/x.sock").as_path())
        );
        assert!(!explicit.is_remote());

        let default = ServerSpec::default()
            .resolve(&rt, "ls", false)
            .expect("default resolves");
        assert_eq!(
            default.socket_path(),
            Some(phux_server::runtime::default_socket_path().as_path())
        );
    }

    /// `--socket` and `--remote` name two different servers; the helper
    /// refuses the pair as a usage error rather than preferring one.
    #[test]
    fn socket_and_remote_are_mutually_exclusive() {
        let spec = ServerSpec {
            socket: Some(PathBuf::from("/tmp/x.sock")),
            remote: Some("mini".to_owned()),
        };
        let err = spec
            .resolve(&runtime(), "ls", false)
            .expect_err("must refuse");
        assert_eq!(err, std::process::ExitCode::from(2));
    }

    /// A malformed target is the same usage error attach reports, raised
    /// before the registry or ssh are consulted.
    #[test]
    fn a_malformed_target_is_a_usage_error() {
        let spec = ServerSpec {
            socket: None,
            remote: Some("quic://mini:8788".to_owned()),
        };
        let err = spec
            .resolve(&runtime(), "ls", true)
            .expect_err("must refuse");
        assert_eq!(err, std::process::ExitCode::from(2));
    }

    /// A registered loopback QUIC entry dials without a pin, like
    /// `phux attach --quic 127.0.0.1:PORT`.
    #[test]
    fn a_loopback_quic_entry_dials_without_a_pin() {
        let remote = dial_entry(
            &runtime(),
            &entry("quic://127.0.0.1:8788", None),
            "ls",
            false,
        )
        .expect("loopback quic dials");
        assert!(remote.loopback);
        let Dial::Quic(quic) = &remote.dial else {
            panic!("expected a QUIC dial, got {:?}", remote.dial);
        };
        assert!(matches!(quic.trust, CertTrust::SkipVerify));
        assert_eq!(quic.addr.port(), 8788);
    }

    /// A pinned routable entry carries its registered pin into the dial, on
    /// both transports.
    #[test]
    fn a_pinned_routable_entry_carries_its_pin() {
        let quic = dial_entry(
            &runtime(),
            &entry("quic://203.0.113.7:8788", Some("ab")),
            "ls",
            false,
        )
        .expect("a pinned routable quic entry dials");
        assert!(!quic.loopback);
        let Dial::Quic(dial) = &quic.dial else {
            panic!("expected a QUIC dial, got {:?}", quic.dial);
        };
        assert!(
            matches!(&dial.trust, CertTrust::Pinned(fp) if fp == "ab"),
            "{:?}",
            dial.trust
        );

        // A routable wss:// dial also needs a bearer token, as under attach.
        let dir = tempfile::tempdir().expect("tempdir");
        let token_file = dir.path().join("mini.token");
        std::fs::write(&token_file, format!("{}\n", "a".repeat(64))).expect("write token");
        let mut wss_entry = entry("wss://203.0.113.7:8787", Some("cd"));
        wss_entry.token_file = Some(token_file);
        let wss = dial_entry(&runtime(), &wss_entry, "ls", false)
            .expect("a pinned, tokened routable wss entry dials");
        let Dial::Ws(dial) = &wss.dial else {
            panic!("expected a WebSocket dial, got {:?}", wss.dial);
        };
        assert!(
            matches!(&dial.trust, CertTrust::Pinned(fp) if fp == "cd"),
            "{:?}",
            dial.trust
        );
    }

    /// Credential gaps are refused, as attach refuses them.
    #[test]
    fn unauthenticated_routable_entries_are_refused() {
        let rt = runtime();
        assert!(dial_entry(&rt, &entry("quic://203.0.113.7:8788", None), "ls", false).is_err());
        assert!(
            dial_entry(
                &rt,
                &entry("wss://203.0.113.7:8787", Some("ab")),
                "ls",
                false
            )
            .is_err(),
            "a routable wss dial with no token must refuse"
        );
        assert!(dial_entry(&rt, &entry("ssh://me@mini", None), "ls", true).is_err());
    }

    /// Every planner refusal is worded for the registry entry, never for
    /// `phux attach`'s flags; only a failed name lookup is `transport`.
    #[test]
    fn planner_refusals_name_the_registry_entry() {
        let bad = entry("quic://203.0.113.7:8788", None);
        let refusals = [
            DialRefusal::Malformed("missing a port".to_owned()),
            DialRefusal::Unresolved {
                target: "ghost.invalid:8788".to_owned(),
                detail: "name resolution failed".to_owned(),
                dns_name: true,
            },
            DialRefusal::UnpinnedQuic {
                target: "203.0.113.7:8788".to_owned(),
            },
            DialRefusal::UnpinnedWs {
                url: "wss://203.0.113.7:8787".to_owned(),
            },
            DialRefusal::Plaintext {
                url: "ws://203.0.113.7:8787".to_owned(),
            },
            DialRefusal::NoToken {
                url: "wss://203.0.113.7:8787".to_owned(),
            },
            DialRefusal::BadToken("secret-shaped detail".to_owned()),
        ];
        for refusal in &refusals {
            let err = entry_refusal(&bad, refusal);
            let expected = if matches!(refusal, DialRefusal::Unresolved { .. }) {
                codes::TRANSPORT
            } else {
                codes::REMOTE_UNRESOLVED
            };
            assert_eq!(err.code, expected, "{refusal:?}");
            assert!(
                err.message.contains("\"mini\""),
                "{refusal:?}: {}",
                err.message
            );
            assert!(
                err.remedy.contains("phux host enroll mini"),
                "{refusal:?}: {}",
                err.remedy
            );
            let all = format!("{} {}", err.message, err.remedy);
            assert!(
                !all.contains("phux attach")
                    && !all.contains("--quic")
                    && !all.contains("--token "),
                "{refusal:?} leaks attach wording: {all}"
            );
            assert!(
                !all.contains("secret-shaped"),
                "a token detail must not be echoed"
            );
        }
    }

    /// The overlay hint belongs to a DNS name, never to an IP literal.
    #[test]
    fn only_a_dns_name_earns_the_overlay_hint() {
        let unresolved = |dns_name| DialRefusal::Unresolved {
            target: "x:1".to_owned(),
            detail: "no".to_owned(),
            dns_name,
        };
        let e = entry("quic://ghost.invalid:8788", None);
        assert!(
            entry_refusal(&e, &unresolved(true))
                .remedy
                .contains("overlay")
        );
        assert!(
            !entry_refusal(&e, &unresolved(false))
                .remedy
                .contains("overlay")
        );
    }

    /// The ssh:// refusal names the command actually being run.
    #[test]
    fn the_ssh_refusal_names_the_verb() {
        let err = ssh_only_refusal("mini", "me@mini", "kill");
        assert_eq!(err.code, codes::REMOTE_UNRESOLVED);
        assert!(
            err.remedy.contains("ssh me@mini phux kill"),
            "{}",
            err.remedy
        );
        assert!(!err.remedy.contains("phux ls"), "{}", err.remedy);
    }

    /// The ladder's multi-line refusal becomes one contract error: headline
    /// as the message, remedies as the remedy, `phux:` prefixes gone.
    #[test]
    fn a_ladder_refusal_splits_into_message_and_remedy() {
        let err = ladder_refusal_error(
            "phux: mini is not a registered host\nphux: to pair without ssh, run `phux pair`\nphux:   phux --remote mini --code '<link>'",
        );
        assert_eq!(err.code, codes::REMOTE_UNRESOLVED);
        assert_eq!(err.message, "mini is not a registered host");
        assert_eq!(
            err.remedy,
            "to pair without ssh, run `phux pair`\nphux --remote mini --code '<link>'"
        );
    }

    /// A remote failure names the host and endpoint and maps onto the
    /// shared transport vocabulary; the resolved target reports itself as
    /// remote, with no local socket and the planned dial.
    #[test]
    fn a_remote_failure_names_the_host_and_uses_the_transport_codes() {
        let remote = dial_entry(
            &runtime(),
            &entry("quic://127.0.0.1:8788", None),
            "ls",
            false,
        )
        .expect("dials");
        let gone = remote.failure(&AttachError::Disconnected, "ls");
        assert_eq!(gone.code, codes::SERVER_DISCONNECTED);
        assert!(gone.message.contains("mini") && gone.message.contains("quic://127.0.0.1:8788"));

        let refused = remote.failure(&AttachError::Unreachable("timeout".to_owned()), "ls");
        assert_eq!(refused.code, codes::TRANSPORT);
        // Loopback never earns the overlay hint.
        assert!(
            refused.remedy.contains("phux host ls"),
            "{}",
            refused.remedy
        );

        let target = ServerTarget::Remote(remote);
        assert!(target.is_remote());
        assert_eq!(target.socket_path(), None);
        assert!(matches!(target.dial(), Dial::Quic(_)));
    }
}
