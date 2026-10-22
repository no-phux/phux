//! `phux attach` and the naked `phux`: plan the recording, then dial
//! whichever transport the flags named.

use std::path::PathBuf;
use std::process::ExitCode;

use crate::{commands, exit_codes};

/// usage-rs `requires(a, b)` is all-of, so `--token` / `--cert-fingerprint` /
/// `--tls-server-name` cannot declare "needs `--quic` or `--ws`" at parse
/// time. The any-of rule lives here (same reason `phux logs -f` moved off
/// the parser).
pub(crate) const fn dial_auth_without_transport(
    quic: Option<&str>,
    ws: Option<&str>,
    token: Option<&str>,
    cert_fingerprint: Option<&str>,
    tls_server_name: Option<&str>,
) -> Option<&'static str> {
    if (token.is_some() || cert_fingerprint.is_some() || tls_server_name.is_some())
        && quic.is_none()
        && ws.is_none()
    {
        Some("--token, --cert-fingerprint, and --tls-server-name need --quic or --ws")
    } else {
        None
    }
}

/// The relay hop a one-off `--quic` dial names (ADR-0180): `--tls-server-name`
/// is the route and `--ca-fingerprint` the server behind it. `None` for a
/// direct dial or a WebSocket one, whose server name is only SNI.
///
/// # Errors
///
/// `--ca-fingerprint` without a relay route, or not a `sha256:` CA pin.
pub(crate) fn relay_hop(
    quic: Option<&str>,
    tls_server_name: Option<&str>,
    ca_fingerprint: Option<String>,
) -> Result<Option<commands::attach::RelayHop>, String> {
    let ca = ca_fingerprint
        .map(|pin| {
            phux_config::known_authorities::canonical_authority(&pin).ok_or_else(|| {
                format!(
                    "--ca-fingerprint {pin:?} is not a CA pin: expected sha256: and 64 hex digits"
                )
            })
        })
        .transpose()?;
    match (quic, tls_server_name) {
        (Some(_), Some(route)) => Ok(Some(commands::attach::RelayHop {
            route: route.to_owned(),
            ca,
        })),
        _ if ca.is_some() => {
            Err("--ca-fingerprint names the server behind a relay route: it needs --quic and --tls-server-name".to_owned())
        }
        _ => Ok(None),
    }
}

/// Resolve a `--remote` target and attach to it.
///
/// One helper for both the root and the verb-scoped spelling, so the two
/// cannot drift in what a target means.
fn attach_remote_target(
    target: &str,
    session: Option<String>,
    code: Option<&str>,
    no_enroll: bool,
    rec: Option<&commands::rec::RecordSpec>,
) -> ExitCode {
    use commands::remote_target::{Bootstrap, RemoteAttach, parse_target};

    let target = match parse_target(target) {
        Ok(target) => target,
        Err(err) => {
            error!("{err}");
            return ExitCode::from(2);
        }
    };
    commands::remote_target::run(RemoteAttach {
        target,
        session,
        code,
        bootstrap: if no_enroll {
            Bootstrap::Never
        } else {
            Bootstrap::Auto
        },
        rec,
    })
}

/// Resolve `--rec` into a recording plan on the cooked terminal, before the
/// alt screen is up.
fn plan_rec(opts: &commands::RecOpts) -> Result<Option<commands::rec::RecordSpec>, ExitCode> {
    opts.rec
        .as_deref()
        .map(|path| commands::rec::spec::plan(path, opts.rec_format))
        .transpose()
}

/// The fully-parsed inputs of `phux attach`, carried as one value so the
/// verb's body can live in [`run_attach`] rather than inline in the dispatch
/// table. The field names mirror the clap variant's, so the arm builds this
/// by field shorthand.
struct AttachInvocation {
    session: Option<String>,
    quic: Option<String>,
    ws: Option<String>,
    token: Option<String>,
    cert_fingerprint: Option<String>,
    tls_server_name: Option<String>,
    ca_fingerprint: Option<String>,
    remote: Option<String>,
    code: Option<String>,
    no_enroll: bool,
    ssh: Option<String>,
    remote_phux: String,
    udp_ports: Option<String>,
    viewer: bool,
    take: bool,
    rec: commands::RecOpts,
    socket: Option<std::path::PathBuf>,
}

/// The attach role `--viewer` / `--take` declare (ADR-0127); the parser
/// already refuses both at once.
pub(crate) const fn declared_attach_role(
    viewer: bool,
    take: bool,
) -> phux_protocol::wire::frame::RolePolicy {
    use phux_protocol::wire::frame::RolePolicy;
    if viewer {
        RolePolicy::VIEWER
    } else if take {
        RolePolicy::TAKEOVER
    } else {
        RolePolicy::PRIMARY
    }
}

/// Run `phux attach`: resolve the recording plan, then dial whichever
/// transport the flags named.
fn run_attach(invocation: AttachInvocation) -> ExitCode {
    let AttachInvocation {
        session,
        quic,
        ws,
        token,
        cert_fingerprint,
        tls_server_name,
        ca_fingerprint,
        remote,
        code,
        no_enroll,
        ssh,
        remote_phux,
        udp_ports,
        viewer,
        take,
        rec,
        socket,
    } = invocation;
    // Every ATTACH this process sends carries it, whichever transport the
    // flags below pick, so a reconnect keeps the role too.
    phux_tui::attach::set_attach_role(declared_attach_role(viewer, take));

    // `phux attach` owns its own `--rec`; the root copy is reserved
    // for the naked invocation below.
    let rec_spec = match plan_rec(&rec) {
        Ok(spec) => spec,
        Err(code) => return code,
    };
    let rec_spec = rec_spec.as_ref();
    // `--socket` is a local UDS path and cannot combine with the remote
    // transports; checked here because it is a root global.
    if socket.is_some() && (quic.is_some() || ws.is_some() || ssh.is_some()) {
        error!("--socket dials a local UDS and cannot combine with --quic/--ws/--ssh; drop one");
        return ExitCode::from(2);
    }
    // The parser cannot say "one of --quic/--ws" (`requires` is every listed
    // flag), so a lone `--token` would otherwise fall through to a local
    // attach and silently drop the credentials.
    if let Some(message) = dial_auth_without_transport(
        quic.as_deref(),
        ws.as_deref(),
        token.as_deref(),
        cert_fingerprint.as_deref(),
        tls_server_name.as_deref(),
    ) {
        error!("{message}");
        return ExitCode::from(exit_codes::EXIT_USAGE);
    }
    let relay = match relay_hop(quic.as_deref(), tls_server_name.as_deref(), ca_fingerprint) {
        Ok(relay) => relay,
        Err(message) => {
            error!("{message}");
            return ExitCode::from(exit_codes::EXIT_USAGE);
        }
    };
    if let Some(destination) = ssh {
        return commands::ssh_bootstrap::run(commands::ssh_bootstrap::SshAttach {
            destination,
            session,
            remote_phux,
            udp_ports,
            identity: None,
            rec: rec_spec,
        });
    }
    if let Some(target) = remote {
        return attach_remote_target(&target, session, code.as_deref(), no_enroll, rec_spec);
    }
    match (quic, ws) {
        (Some(addr), None) => commands::attach::run_attach_quic(
            session,
            addr,
            token,
            cert_fingerprint,
            relay,
            None,
            rec_spec,
        ),
        (None, Some(url)) => commands::attach::run_attach_ws(
            session,
            url,
            token,
            cert_fingerprint,
            tls_server_name,
            rec_spec,
        ),
        (None, None) => commands::attach::run_attach_rec(session, socket, rec_spec),
        (Some(_), Some(_)) => {
            error!("choose only one remote attach transport (--quic or --ws)");
            ExitCode::FAILURE
        }
    }
}

/// Run the naked `phux` — no verb, so attach to the user's session, locally
/// or at the root `--remote` target.
pub(crate) fn run_naked_invocation(
    root_rec: &commands::RecOpts,
    root_remote: Option<String>,
    socket: Option<std::path::PathBuf>,
) -> ExitCode {
    let rec_spec = match plan_rec(root_rec) {
        Ok(spec) => spec,
        Err(code) => return code,
    };
    if let Some(target) = root_remote {
        // The `--socket` collision was already refused post-parse.
        return attach_remote_target(&target, None, None, false, rec_spec.as_ref());
    }
    commands::attach::run_naked(socket, rec_spec.as_ref())
}

pub(super) fn attach(args: commands::AttachArgs, socket: Option<PathBuf>) -> ExitCode {
    let commands::AttachArgs {
        session,
        quic,
        ws,
        token,
        cert_fingerprint,
        tls_server_name,
        ca_fingerprint,
        remote,
        code,
        no_enroll,
        ssh,
        remote_phux,
        udp_ports,
        viewer,
        take,
        rec,
    } = args;
    run_attach(AttachInvocation {
        session,
        quic,
        ws,
        token,
        cert_fingerprint,
        tls_server_name,
        ca_fingerprint,
        remote,
        code,
        no_enroll,
        ssh,
        remote_phux,
        udp_ports,
        viewer,
        take,
        rec,
        socket,
    })
}
