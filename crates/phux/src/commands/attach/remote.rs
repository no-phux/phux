//! Remote attach execution and repair classification.
use std::process::ExitCode;

use phux_client::attach::connection::Connection;
use phux_client::attach::{AttachEnd, AttachError};

use crate::commands::rec::RecordSpec;
use crate::commands::remote::{self as host_registry, Endpoint, RemoteEntry};
use super::{
    DialPlan, attach_with_reconnect, fallback_lookup_name, finalize_recording,
    plan_quic_dial, plan_ws_dial, predictive_config_for, reachability_hint,
    report_attach_end, requested_attach_target, resolved_default_session_name,
};

/// Turn a finished WebSocket attach into its exit code and repair hint,
/// reporting the ending.
fn report_ws_attach_outcome(
    result: Result<AttachEnd, AttachError>,
    loopback: bool,
) -> RemoteAttachOutcome {
    let repair = remote_repair(&result);
    let code = match result {
        Ok(end) => {
            report_attach_end(end);
            ExitCode::SUCCESS
        }
        // Already reported by the reconnect window (distinct SocketGone /
        // TimedOut lines naming `phux doctor`) — see `attach_with_reconnect`.
        Err(AttachError::Disconnected) => ExitCode::FAILURE,
        Err(err) => {
            error!("WebSocket attach failed: {err}");
            if let Some(hint) = reachability_hint(&err, loopback) {
                hint!("{hint}");
            }
            ExitCode::FAILURE
        }
    };
    RemoteAttachOutcome { code, repair }
}

/// Classify a failed remote attach for the ssh repair rung: an unanswered
/// dial wants the server started, a refused one wants a re-pair, and
/// anything past the transport wants nothing.
pub(super) const fn remote_repair(result: &Result<AttachEnd, AttachError>) -> Repair {
    match result {
        Err(AttachError::Unreachable(_)) => Repair::Start,
        Err(AttachError::Connect(_)) => Repair::RePair,
        _ => Repair::None,
    }
}
/// Attach over WebSocket to `phux server --listen`.
pub(crate) fn run_attach_ws(
    session: Option<String>,
    url: String,
    token: Option<String>,
    cert_fingerprint: Option<String>,
    tls_server_name: Option<String>,
    rec: Option<&RecordSpec>,
) -> ExitCode {
    let credentials = RemoteCredentials {
        name: None,
        token,
        cert_fingerprint,
        authority: phux_client_runtime::target::AuthorityPin::default(),
        identity: None,
    };
    run_attach_ws_outcome(session, url, credentials, tls_server_name, rec).code
}

fn run_attach_ws_outcome(
    session: Option<String>,
    url: String,
    credentials: RemoteCredentials,
    tls_server_name: Option<String>,
    rec: Option<&RecordSpec>,
) -> RemoteAttachOutcome {
    let RemoteCredentials {
        name,
        token,
        cert_fingerprint,
        authority,
        identity,
    } = credentials;
    let label = name.unwrap_or_else(|| url.clone());
    let DialPlan { dial, loopback } =
        match plan_ws_dial(url, token, cert_fingerprint, tls_server_name).and_then(|plan| {
            plan.with_identity(identity)
                .with_authority(&label, &authority)
        }) {
            Ok(plan) => plan,
            Err(refusal) => {
                return RemoteAttachOutcome::repairable(refusal.report_for_attach());
            }
        };

    let rt = match crate::commands::cli_runtime() {
        Ok(rt) => rt,
        Err(code) => return RemoteAttachOutcome::terminal(code),
    };

    let predict_cfg = predictive_config_for(&dial);

    let default_name = resolved_default_session_name();
    let use_default = session.is_none();
    let target = requested_attach_target(session);
    let default = use_default
        .then(|| fallback_lookup_name(&default_name))
        .flatten();

    let result = rt.block_on(attach_with_reconnect(
        &dial,
        None,
        target,
        predict_cfg,
        default,
        rec,
    ));
    finalize_recording(rec);
    report_ws_attach_outcome(result, loopback)
}
/// A one-off dial's relay route and the CA of the server behind it
/// (`--tls-server-name`, `--ca-fingerprint`): the dial runs an end-to-end
/// session to that server inside the relayed stream (ADR-0180).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RelayHop {
    /// The route, offered as the outer TLS server name.
    pub(crate) route: String,
    /// The server's CA, `sha256:` form. `None` is refused at planning.
    pub(crate) ca: Option<String>,
}

/// Attach over QUIC (ADR-0007) to `HOST:PORT`. TLS trust keys on the
/// resolved address: `--cert-fingerprint` pins the leaf; loopback without a pin
/// skips verification; routable without a pin is refused. Through a relay
/// (`relay`) that pin is the relay's, and the server is verified end to end by
/// its CA. With no session name each reconnect starts with `Last`.
#[allow(
    clippy::needless_pass_by_value,
    reason = "clap hands over the owned HOST:PORT value; a &str signature would only push the borrow into lib.rs's dispatch"
)]
pub(crate) fn run_attach_quic(
    session: Option<String>,
    target: String,
    token: Option<String>,
    cert_fingerprint: Option<String>,
    relay: Option<RelayHop>,
    identity: Option<phux_dial::TlsClientIdentity>,
    rec: Option<&RecordSpec>,
) -> ExitCode {
    let server_name = relay.as_ref().map(|hop| hop.route.clone());
    let authority = relay.map_or_else(phux_client_runtime::target::AuthorityPin::default, |hop| {
        phux_client_runtime::target::AuthorityPin {
            ca: hop.ca,
            route: Some(hop.route),
        }
    });
    let credentials = RemoteCredentials {
        name: None,
        token,
        cert_fingerprint,
        authority,
        identity,
    };
    run_attach_quic_outcome(session, target, credentials, server_name, rec).code
}

/// What a remote dial authenticates with: the bearer token, the server pin,
/// and the workload client identity (`None` reads the environment).
struct RemoteCredentials {
    /// The registry entry's name, which labels refusals; `None` for a typed
    /// target, which then labels them itself.
    name: Option<String>,
    token: Option<String>,
    cert_fingerprint: Option<String>,
    /// The CA pin beside the leaf pin (ADR-0153); default for a typed
    /// `--cert-fingerprint`, which pins the leaf alone.
    authority: phux_client_runtime::target::AuthorityPin,
    identity: Option<phux_dial::TlsClientIdentity>,
}

#[allow(
    clippy::needless_pass_by_value,
    reason = "the owned connection inputs move into the Dial on the successful path"
)]
fn run_attach_quic_outcome(
    session: Option<String>,
    target: String,
    credentials: RemoteCredentials,
    server_name: Option<String>,
    rec: Option<&RecordSpec>,
) -> RemoteAttachOutcome {
    let RemoteCredentials {
        name,
        token,
        cert_fingerprint,
        authority,
        identity,
    } = credentials;
    let rt = match crate::commands::cli_runtime() {
        Ok(rt) => rt,
        Err(code) => return RemoteAttachOutcome::terminal(code),
    };

    let label = name.as_deref().unwrap_or(&target);
    let plan =
        match plan_quic_dial(&rt, &target, token, cert_fingerprint, server_name).and_then(|plan| {
            plan.with_identity(identity)
                .with_authority(label, &authority)
        }) {
            Ok(plan) => plan,
            Err(refusal) => {
                return RemoteAttachOutcome::repairable(refusal.report_for_attach());
            }
        };
    attach_quic_planned(&rt, plan, &target, session, rec, None)
}

/// Attach over a planned QUIC dial on `rt`, starting from `first` when a
/// probe on that runtime already connected it.
pub(crate) fn attach_quic_planned(
    rt: &tokio::runtime::Runtime,
    DialPlan { dial, loopback }: DialPlan,
    target: &str,
    session: Option<String>,
    rec: Option<&RecordSpec>,
    first: Option<Box<Connection>>,
) -> RemoteAttachOutcome {
    let predict_cfg = predictive_config_for(&dial);

    let default_name = resolved_default_session_name();
    let use_default = session.is_none();
    let attach_target = requested_attach_target(session);
    let default = use_default
        .then(|| fallback_lookup_name(&default_name))
        .flatten();

    let result = rt.block_on(attach_with_reconnect(
        &dial,
        first,
        attach_target,
        predict_cfg,
        default,
        rec,
    ));
    finalize_recording(rec);
    let repair = remote_repair(&result);
    let code = match result {
        Ok(end) => {
            report_attach_end(end);
            ExitCode::SUCCESS
        }
        // Already reported by the reconnect window (distinct SocketGone /
        // TimedOut lines naming `phux doctor`) — see `attach_with_reconnect`.
        Err(AttachError::Disconnected) => ExitCode::FAILURE,
        Err(err) => {
            error!("QUIC attach to {target} failed: {err}");
            if let Some(hint) = reachability_hint(&err, loopback) {
                hint!("{hint}");
            }
            ExitCode::FAILURE
        }
    };
    RemoteAttachOutcome { code, repair }
}
/// Result of a registered remote attach, including whether its saved route
/// failed early enough that ssh can repair it, and how.
pub(crate) struct RemoteAttachOutcome {
    pub(crate) code: ExitCode,
    pub(crate) repair: Repair,
}

/// Whether an ssh repair should follow a failed registered attach: only
/// failures establishing the saved transport authorize one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Repair {
    /// The attach got past the transport: nothing ssh can fix.
    None,
    /// Nobody answered at the saved route: the server is most likely
    /// stopped. Start it over ssh and dial again with the saved
    /// credentials; only re-pair if that still fails.
    Start,
    /// A host answered and refused the saved credentials, or the entry
    /// cannot be dialed as written: only a re-pair rewrites what is wrong.
    RePair,
}

impl RemoteAttachOutcome {
    const fn terminal(code: ExitCode) -> Self {
        Self {
            code,
            repair: Repair::None,
        }
    }

    const fn repairable(code: ExitCode) -> Self {
        Self {
            code,
            repair: Repair::RePair,
        }
    }
}

/// Attach through a registered `[[remote]]` entry (ADR-0055). `ssh://`
/// re-execs `ssh -t HOST phux attach`; the session still lives on the remote
/// server.
pub(crate) fn run_attach_remote(
    entry: &RemoteEntry,
    session: Option<String>,
    rec: Option<&RecordSpec>,
) -> ExitCode {
    run_attach_remote_outcome(entry, session, rec).code
}

/// [`run_attach_remote`] plus the early direct-route failure classification
/// the registered-host ladder uses to repair a cold host over ssh.
pub(crate) fn run_attach_remote_outcome(
    entry: &RemoteEntry,
    session: Option<String>,
    rec: Option<&RecordSpec>,
) -> RemoteAttachOutcome {
    let endpoint = match Endpoint::parse(&entry.endpoint) {
        Ok(endpoint) => endpoint,
        Err(err) => {
            error!("remote {:?}: {err}", entry.name);
            return RemoteAttachOutcome::repairable(ExitCode::FAILURE);
        }
    };
    let session = session.or_else(|| entry.session.clone());
    // ADR-0140: the sidebar draws this host's sessions live and every other
    // machine from the hosts provider; it needs to know which one this is.
    phux_tui::attach::hosts::set_attach_origin(phux_tui::attach::hosts::AttachOrigin::Remote(
        entry.name.clone(),
    ));

    let token = match host_registry::read_token(entry) {
        Ok(token) => token,
        Err(err) => {
            error!("remote {:?}: {err}", entry.name);
            return RemoteAttachOutcome::repairable(ExitCode::FAILURE);
        }
    };
    let identity = match entry.client_identity() {
        Ok(identity) => identity,
        Err(err) => {
            error!("{err}");
            return RemoteAttachOutcome::repairable(ExitCode::FAILURE);
        }
    };
    entry.warn_if_renewal_due();
    let credentials = RemoteCredentials {
        name: Some(entry.name.clone()),
        token,
        cert_fingerprint: entry.cert_fingerprint.clone(),
        authority: entry.authority_pin(),
        identity,
    };
    // A relay route (ADR-0149) rides the TLS server name; `None` keeps the
    // endpoint-derived default.
    let server_name = entry.tls_server_name.clone();

    match endpoint {
        Endpoint::Quic(target) => {
            run_attach_quic_outcome(session, target, credentials, server_name, rec)
        }
        Endpoint::Ws(url) => run_attach_ws_outcome(session, url, credentials, server_name, rec),
        // ADR-0120: bootstrap a direct QUIC attach over ssh first, and only
        // fall back to `exec`ing `ssh -t HOST phux attach` when that cannot
        // work. Only the direct path can carry a recording.
        // The bootstrapped listener is the host's own server, so it admits
        // the workload certificate `host add` enrolled there (ADR-0116).
        Endpoint::Ssh(host) => RemoteAttachOutcome::terminal(crate::commands::ssh_bootstrap::run(
            crate::commands::ssh_bootstrap::SshAttach {
                destination: host,
                session,
                remote_phux: entry.remote_phux().to_owned(),
                udp_ports: None,
                identity: credentials.identity,
                rec,
            },
        )),
    }
}

