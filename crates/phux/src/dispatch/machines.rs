//! The dispatch bodies of the `Machines` verbs and the profiling ones.

use std::path::PathBuf;
use std::process::ExitCode;

use crate::commands;

/// Run `phux service`: the installed-supervisor lifecycle verbs.
pub(crate) fn run_service(
    action: commands::ServiceAction,
    socket: Option<std::path::PathBuf>,
) -> ExitCode {
    match action {
        commands::ServiceAction::Install {
            quic,
            listen,
            restore,
            hub,
            adopt,
            print,
        } => commands::service::run_install(
            quic,
            listen,
            restore,
            socket,
            hub,
            if adopt {
                commands::service::Takeover::Adopt
            } else {
                commands::service::Takeover::Refuse
            },
            print,
        ),
        commands::ServiceAction::Reconcile { print } => commands::service::run_reconcile(print),
        commands::ServiceAction::Uninstall => commands::service::run_uninstall(),
        commands::ServiceAction::Status => commands::service::run_status(),
        commands::ServiceAction::Logs { follow, lines } => {
            commands::service::run_logs(follow, lines)
        }
        commands::ServiceAction::PruneLogs { dry_run } => {
            commands::service::run_prune_logs(dry_run)
        }
    }
}

pub(super) fn server(args: commands::ServerArgs, socket: Option<PathBuf>) -> ExitCode {
    let commands::ServerArgs {
        // --ensure returns from run before tracing and this dispatch.
        ensure: _,
        json: _,
        session,
        listen,
        quic,
        webtransport,
        connect,
        hub,
        exit_after_idle,
        autosave,
        daemonize,
        seed_command,
        resume,
        no_seed,
    } = args;
    commands::server::run_server(
        (!no_seed).then_some(session.as_str()),
        socket,
        listen,
        quic,
        webtransport,
        connect,
        hub,
        exit_after_idle,
        autosave.as_deref(),
        daemonize,
        seed_command.as_deref(),
        resume,
    )
}

pub(super) fn perf(args: commands::PerfArgs, socket: Option<PathBuf>) -> ExitCode {
    let commands::PerfArgs {
        action,
        json,
        desktop,
        watch,
        reset,
    } = args;
    commands::perf::run_perf(
        commands::perf::PerfOptions {
            json: json.json,
            desktop,
            watch,
            reset,
        },
        action,
        socket,
    )
}

pub(super) fn download(args: commands::DownloadArgs, socket: Option<PathBuf>) -> ExitCode {
    let commands::DownloadArgs {
        source,
        local,
        force,
        timeout,
        json,
        remote,
    } = args;
    commands::download::run(
        commands::download::DownloadArgs {
            source,
            local,
            force,
            timeout,
            json,
        },
        commands::server_target::ServerSpec {
            socket,
            remote: remote.remote,
        },
    )
}

pub(super) fn bootstrap(args: commands::BootstrapArgs, socket: Option<PathBuf>) -> ExitCode {
    let commands::BootstrapArgs {
        client_version,
        port_range,
        linger,
    } = args;
    commands::bootstrap::run(&commands::bootstrap::BootstrapArgs {
        socket,
        client_version,
        port_range,
        linger,
    })
}

pub(super) fn pair(args: commands::PairArgs, socket: Option<PathBuf>) -> ExitCode {
    let commands::PairArgs {
        action,
        tokens,
        cert,
        qr,
        secret_file,
        expires,
        no_qr,
        host,
        name,
        json,
        replace_token,
        relay_route,
        relay,
        enroll,
        enable,
    } = args;
    commands::pair::run_pair(
        action,
        socket,
        tokens,
        cert,
        qr,
        secret_file,
        expires,
        no_qr,
        host,
        name,
        json,
        replace_token,
        relay_route.map_or(commands::pair::Door::Listener { enroll }, |route| {
            commands::pair::Door::Relay {
                route: commands::pair::RelayRoute { route, relay },
                enroll,
            }
        }),
        enable,
    )
}
