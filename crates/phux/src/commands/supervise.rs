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

use crate::commands::json_err::{self, CliError};
use crate::commands::{SignalArg, parse_selector, request_command, resolve_target_for_input};

/// Version of the `take`, `give`, and `signal` `--json` documents.
pub(crate) const SUPERVISE_SCHEMA_VERSION: u8 = 1;

/// `phux take TARGET [--ttl SECS]` — seize the input lease over the
/// resolved pane so only this client's input reaches the PTY (ADR-0033).
/// Uses `Seize` mode, so it preempts any current holder. `ttl` arms the
/// server-side expiry timer this lane adds: the lease auto-releases after
/// that many seconds even if this process has long since exited.
pub(crate) fn run_take(
    target: &str,
    ttl: Option<u32>,
    json: bool,
    socket: Option<PathBuf>,
) -> ExitCode {
    run_lease(target, json, socket, Some(ttl_ms(ttl)))
}

/// `phux give TARGET` — release the input lease over the resolved pane,
/// returning it to open input (ADR-0033). Idempotent.
pub(crate) fn run_give(target: &str, json: bool, socket: Option<PathBuf>) -> ExitCode {
    run_lease(target, json, socket, None)
}

/// Seconds to the wire's `ttl_ms` (`None` is `0`, never). `--ttl` is
/// range-checked at parse time; the saturation is defense in depth.
fn ttl_ms(ttl: Option<u32>) -> u32 {
    ttl.unwrap_or(0).saturating_mul(1000)
}

/// `take` is `Some(ttl_ms)`; `give` is `None`, distinguishing the two verbs
/// for the shared resolve-then-request body below.
fn run_lease(target: &str, json: bool, socket: Option<PathBuf>, take: Option<u32>) -> ExitCode {
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
        let terminal_id = match resolve_target_for_input(&socket_path, &selector, verb, json).await
        {
            Ok(id) => id,
            Err(code) => return code,
        };
        let terminal = terminal_id.local_id();
        let command = if let Some(ttl_ms) = take {
            phux_client::signal::take_command(terminal_id, ttl_ms)
        } else {
            phux_client::signal::give_command(terminal_id)
        };
        match request_command(&socket_path, command)
            .await
            .map(LeaseOutcome::from_result)
        {
            Ok(LeaseOutcome::Ok) if json => crate::output::json(&serde_json::json!({
                "schema_version": SUPERVISE_SCHEMA_VERSION,
                "terminal_id": terminal,
                "held": take.is_some(),
                "ttl_secs": take.filter(|ms| *ms > 0).map(|ms| ms / 1000),
            })),
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
            Ok(outcome) => report_refusal(json, verb, target, outcome),
            Err(err) => json_err::report_no_server(json, &err, &socket_path, verb),
        }
    })
}

/// A lease or signal the server did not apply: `server_error` for a
/// refusal, `unexpected_reply` for an answer this client cannot read. Exit 2.
fn report_refusal(json: bool, verb: &str, target: &str, outcome: LeaseOutcome) -> ExitCode {
    let err = match outcome {
        LeaseOutcome::Ok => return ExitCode::SUCCESS,
        LeaseOutcome::Refused(message) => CliError::new(
            "server_error",
            format!("{verb} refused for {target}: {message}"),
            "run `phux resource show TARGET` to see who holds the pane",
        ),
        LeaseOutcome::Unexpected(other) => CliError::new(
            "unexpected_reply",
            format!(
                "{target}: {}",
                phux_client::explain::explain_unexpected(verb, &other)
            ),
            "upgrade the client or the server so they agree on the reply",
        ),
    };
    json_err::emit(json, &err, 2)
}

/// `phux signal TARGET SIGNAL` — deliver a POSIX signal to the resolved pane's
/// process group (ADR-0033). `freeze`/`resume` is the reversible brake; the
/// ending signals are dangerous (ADR-0128) and need `yes` or a typed "y",
/// asked before anything is dialed.
///
/// `key` is `--idempotency-key` (`docs/protocol.md`): a retry under the
/// same key answers the first result and delivers nothing. A server that
/// does not advertise `KEYED_SIGNAL` would signal again, so a keyed signal to
/// one is refused before the signal is sent.
pub(crate) fn run_signal(
    target: &str,
    signal: SignalArg,
    key: Option<IdempotencyKey>,
    yes: bool,
    json: bool,
    socket: Option<PathBuf>,
) -> ExitCode {
    let selector = match parse_selector(Some(target)) {
        Ok(sel) => sel,
        Err(code) => return code,
    };
    let wire_signal = TerminalSignal::from(signal);
    if phux_client::signal::is_dangerous(wire_signal)
        && let Err(code) = crate::commands::confirm::confirmed_for(
            json,
            yes,
            &format!("signal {target} ({signal:?})"),
        )
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
            warn!("{} saw only part of the fleet — {notice}", "signal");
        }
        match result {
            Ok(delivered) => {
                for message in delivered.interleaved {
                    warn!("partial results — {message}");
                }
                match delivered.outcome {
                    LeaseOutcome::Ok if json => crate::output::json(&serde_json::json!({
                        "schema_version": SUPERVISE_SCHEMA_VERSION,
                        "target": target,
                        "signal": format!("{signal:?}").to_lowercase(),
                        "delivered": true,
                    })),
                    LeaseOutcome::Ok => {
                        outln!("phux: signalled {target} ({signal:?})");
                        ExitCode::SUCCESS
                    }
                    outcome => report_refusal(json, "signal", target, outcome),
                }
            }
            Err(phux_client::signal::SignalError::Miss { degradation }) => {
                crate::commands::partial::report_target_miss_keeping_status_for(
                    json,
                    Some(target),
                    &degradation,
                )
            }
            Err(phux_client::signal::SignalError::Agent(err)) => {
                crate::commands::report_agent_resolve_error(json, &err, true)
            }
            Err(phux_client::signal::SignalError::Keyed(KeyedError::Unsupported)) => {
                crate::commands::spawn::unsupported_server(json, ServerFeature::KeyedSignal)
            }
            Err(phux_client::signal::SignalError::Keyed(KeyedError::Attach(err))) => {
                json_err::report_no_server(json, &err, &socket_path, "signal")
            }
        }
    })
}
