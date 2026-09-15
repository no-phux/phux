//! `phux take` / `phux give` / `phux signal` — the supervisory verbs
//! (ADR-0033, "take the wheel + kill").
//!
//! Each resolves a selector client-side to one pane (the same front door
//! `send-keys` / `run` use) and issues a single control command built by
//! [`phux_client::signal`]: `ACQUIRE_INPUT` (seize the input lease),
//! `RELEASE_INPUT`, or `SIGNAL_TERMINAL`.

use std::path::PathBuf;
use std::process::ExitCode;

use phux_client::signal::LeaseOutcome;
use phux_protocol::wire::frame::TerminalSignal;
use phux_server::runtime::default_socket_path;

use crate::commands::{
    SignalArg, parse_selector, report_no_server, request_command, resolve_target_for_input,
};

/// `phux take TARGET [--ttl SECS]` — seize the input lease over the
/// resolved pane so only this client's input reaches the PTY (ADR-0033).
/// Uses `Seize` mode, so it preempts any current holder. `ttl` arms the
/// server-side expiry timer this lane adds: the lease auto-releases after
/// that many seconds even if this process has long since exited.
pub(crate) fn run_take(target: &str, ttl: Option<u32>, socket: Option<PathBuf>) -> ExitCode {
    run_lease(target, socket, Some(ttl_ms(ttl)))
}

/// `phux give TARGET` — release the input lease over the resolved pane,
/// returning it to open input (ADR-0033). Idempotent.
pub(crate) fn run_give(target: &str, socket: Option<PathBuf>) -> ExitCode {
    run_lease(target, socket, None)
}

/// Seconds to the wire's `ttl_ms`: `None` (no `--ttl`) is `0`, "never" —
/// today's default. `--ttl`'s clap-level `validate` already refuses a
/// value that would overflow `u32` milliseconds (exit 2, before this ever
/// runs); `saturating_mul` is defense in depth, not the primary guard —
/// silently clamping an out-of-range value here, rather than rejecting it
/// up front, was review round 2's low finding.
fn ttl_ms(ttl: Option<u32>) -> u32 {
    ttl.unwrap_or(0).saturating_mul(1000)
}

/// `take` is `Some(ttl_ms)`; `give` is `None`, distinguishing the two verbs
/// for the shared resolve-then-request body below.
fn run_lease(target: &str, socket: Option<PathBuf>, take: Option<u32>) -> ExitCode {
    let verb = if take.is_some() { "take" } else { "give" };
    let selector = match parse_selector(Some(target)) {
        Ok(sel) => sel,
        Err(code) => return code,
    };
    let socket_path = socket.unwrap_or_else(default_socket_path);
    let rt = match crate::commands::cli_runtime() {
        Ok(rt) => rt,
        Err(code) => return code,
    };
    rt.block_on(async move {
        let terminal_id = match resolve_target_for_input(&socket_path, &selector, verb, false).await
        {
            Ok(id) => id,
            Err(code) => return code,
        };
        let command = if let Some(ttl_ms) = take {
            phux_client::signal::take_command(terminal_id, ttl_ms)
        } else {
            phux_client::signal::give_command(terminal_id)
        };
        match request_command(&socket_path, command)
            .await
            .map(LeaseOutcome::from_result)
        {
            Ok(LeaseOutcome::Ok) => {
                match take {
                    Some(0) => outln!("phux: took the wheel of {target}"),
                    Some(ttl_ms) => outln!(
                        "phux: took the wheel of {target} (expires in {}s)",
                        ttl_ms / 1000
                    ),
                    None => outln!("phux: released the wheel of {target}"),
                }
                ExitCode::SUCCESS
            }
            Ok(LeaseOutcome::Refused(message)) => {
                eprintln!("phux: {verb} refused for {target}: {message}");
                ExitCode::from(2)
            }
            Ok(LeaseOutcome::Unexpected(other)) => {
                eprintln!(
                    "phux: {target}: {}",
                    phux_client::explain::explain_unexpected(verb, &other)
                );
                ExitCode::from(2)
            }
            Err(err) => report_no_server(&err, &socket_path, verb),
        }
    })
}

/// `phux signal TARGET SIGNAL` — deliver a POSIX signal to the resolved pane's
/// process group (ADR-0033). `freeze`/`resume` is the reversible brake.
pub(crate) fn run_signal(target: &str, signal: SignalArg, socket: Option<PathBuf>) -> ExitCode {
    let selector = match parse_selector(Some(target)) {
        Ok(sel) => sel,
        Err(code) => return code,
    };
    let socket_path = socket.unwrap_or_else(default_socket_path);
    let rt = match crate::commands::cli_runtime() {
        Ok(rt) => rt,
        Err(code) => return code,
    };
    let wire_signal = TerminalSignal::from(signal);
    rt.block_on(async move {
        let terminal_id =
            match resolve_target_for_input(&socket_path, &selector, "signal", false).await {
                Ok(id) => id,
                Err(code) => return code,
            };
        match request_command(
            &socket_path,
            phux_client::signal::signal_command(terminal_id, wire_signal),
        )
        .await
        .map(LeaseOutcome::from_result)
        {
            Ok(LeaseOutcome::Ok) => {
                outln!("phux: signalled {target} ({signal:?})");
                ExitCode::SUCCESS
            }
            Ok(LeaseOutcome::Refused(message)) => {
                eprintln!("phux: signal refused for {target}: {message}");
                ExitCode::from(2)
            }
            Ok(LeaseOutcome::Unexpected(other)) => {
                eprintln!(
                    "phux: {target}: {}",
                    phux_client::explain::explain_unexpected("signal", &other)
                );
                ExitCode::from(2)
            }
            Err(err) => report_no_server(&err, &socket_path, "signal"),
        }
    })
}
