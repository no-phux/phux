//! `phux take` / `phux give` / `phux signal` — the supervisory verbs
//! (ADR-0033, "take the wheel + kill").
//!
//! `take` / `give` resolve a selector client-side to one pane (the same
//! front door `send-keys` / `run` use) and issue a single control command
//! built by [`phux_client::signal`]. `signal` calls
//! [`phux_client::signal::deliver`], the same path MCP `phux_signal` uses.

use std::path::PathBuf;
use std::process::ExitCode;

use phux_client::kill::KeyedError;
use phux_client::signal::LeaseOutcome;
use phux_protocol::caps::ServerFeature;
use phux_protocol::ids::IdempotencyKey;
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
/// process group (ADR-0033). `freeze`/`resume` is the reversible brake; the
/// ending signals are dangerous (ADR-0128) and need `yes` or a typed "y",
/// asked before anything is dialed.
///
/// `key` is `--idempotency-key` (`docs/spec/L1.md` §5.1.1): a retry under the
/// same key answers the first result and delivers nothing. A server that
/// does not advertise `KEYED_SIGNAL` would signal again, so a keyed signal to
/// one is refused before the signal is sent.
pub(crate) fn run_signal(
    target: &str,
    signal: SignalArg,
    key: Option<IdempotencyKey>,
    yes: bool,
    socket: Option<PathBuf>,
) -> ExitCode {
    let selector = match parse_selector(Some(target)) {
        Ok(sel) => sel,
        Err(code) => return code,
    };
    let wire_signal = TerminalSignal::from(signal);
    if crate::commands::confirm::signal_is_dangerous(wire_signal)
        && let Err(code) =
            crate::commands::confirm::confirmed(yes, &format!("signal {target} ({signal:?})"))
    {
        return code;
    }
    let socket_path = socket.unwrap_or_else(default_socket_path);
    let rt = match crate::commands::cli_runtime() {
        Ok(rt) => rt,
        Err(code) => return code,
    };
    rt.block_on(async move {
        let mut notices = Vec::new();
        let result =
            phux_client::signal::deliver(&socket_path, &selector, wire_signal, key, &mut notices)
                .await;
        for notice in &notices {
            eprintln!(
                "{}",
                phux_client::state::partial_view_warning("signal", notice)
            );
        }
        match result {
            Ok(delivered) => {
                for message in delivered.interleaved {
                    eprintln!("phux: warning: partial results — {message}");
                }
                match delivered.outcome {
                    LeaseOutcome::Ok => {
                        outln!("phux: signalled {target} ({signal:?})");
                        ExitCode::SUCCESS
                    }
                    LeaseOutcome::Refused(message) => {
                        eprintln!("phux: signal refused for {target}: {message}");
                        ExitCode::from(2)
                    }
                    LeaseOutcome::Unexpected(other) => {
                        eprintln!(
                            "phux: {target}: {}",
                            phux_client::explain::explain_unexpected("signal", &other)
                        );
                        ExitCode::from(2)
                    }
                }
            }
            Err(phux_client::signal::SignalError::Miss { degradation }) => {
                crate::commands::partial::report_target_miss_keeping_status_for(
                    false,
                    None,
                    &degradation,
                )
            }
            Err(phux_client::signal::SignalError::Agent(err)) => {
                crate::commands::report_agent_resolve_error(false, &err, true)
            }
            Err(phux_client::signal::SignalError::Keyed(KeyedError::Unsupported)) => {
                crate::commands::spawn::unsupported_server(false, ServerFeature::KeyedSignal)
            }
            Err(phux_client::signal::SignalError::Keyed(KeyedError::Attach(err))) => {
                report_no_server(&err, &socket_path, "signal")
            }
        }
    })
}
