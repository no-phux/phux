//! `phux take` / `phux give` / `phux signal` — the supervisory verbs
//! (ADR-0033, "take the wheel + kill").
//!
//! `take` / `give` resolve a selector client-side to one pane (the same
//! front door `send-keys` / `run` use) and issue a single control command
//! built by [`phux_client::signal`]. `signal` calls
//! [`phux_client::signal::deliver`], the same path MCP `phux_signal` uses.

use std::path::PathBuf;
use std::process::ExitCode;

use phux_client::read::Verb;
use phux_client::signal::{LeaseOutcome, SignalError};
use phux_protocol::ids::IdempotencyKey;
use phux_protocol::wire::frame::TerminalSignal;

use crate::commands::ctx::{self, Ctx};
use crate::commands::json_err::{CliError, Refusal};
use crate::commands::{
    SignalArg, parse_target, partial, read_kind, request_command, resolve_target_for_input,
};
use crate::exit_codes::EXIT_USAGE;

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
    let verb = if take.is_some() {
        Verb::Take
    } else {
        Verb::Give
    };
    ctx::run_local(socket, json, |ctx| {
        let selector = parse_target(Some(target))?;
        ctx.rt.block_on(async {
            let terminal_id = resolve_target_for_input(ctx.socket(), &selector, verb).await?;
            let terminal = terminal_id.local_id();
            let command = if let Some(ttl_ms) = take {
                phux_client::signal::take_command(terminal_id, ttl_ms)
            } else {
                phux_client::signal::give_command(terminal_id)
            };
            let outcome = request_command(ctx.socket(), command)
                .await
                .map(LeaseOutcome::from_result)
                .map_err(|err| ctx.unreachable(&err, verb.name()))?;
            check_applied(verb.name(), target, outcome)?;
            if json {
                return Ok(crate::output::json(&serde_json::json!({
                    "schema_version": SUPERVISE_SCHEMA_VERSION,
                    "terminal_id": terminal,
                    "held": take.is_some(),
                    "ttl_secs": take.filter(|ms| *ms > 0).map(|ms| ms / 1000),
                })));
            }
            match take {
                Some(0) => outln!("phux: took the wheel of {target}"),
                Some(ttl_ms) => outln!(
                    "phux: took the wheel of {target} (expires in {}s)",
                    ttl_ms / 1000
                ),
                None => outln!("phux: released the wheel of {target}"),
            }
            Ok(ExitCode::SUCCESS)
        })
    })
}

/// Refuse a lease or signal the server did not apply: `server_error` for a
/// refusal, `unexpected_reply` for an answer this client cannot read. Exit 2.
fn check_applied(verb: &str, target: &str, outcome: LeaseOutcome) -> Result<(), Refusal> {
    let err = match outcome {
        LeaseOutcome::Ok => return Ok(()),
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
    Err(Refusal::from_error(err, EXIT_USAGE))
}

/// `phux signal TARGET SIGNAL` — deliver a POSIX signal to the resolved pane's
/// process group (ADR-0033). `freeze`/`resume` is the reversible brake; the
/// ending signals are dangerous (ADR-0128) and need `yes` or a typed "y",
/// asked before anything is dialed.
pub(crate) fn run_signal(
    target: &str,
    signal: SignalArg,
    key: Option<IdempotencyKey>,
    yes: bool,
    json: bool,
    socket: Option<PathBuf>,
) -> ExitCode {
    ctx::run_local(socket, json, |ctx| {
        let selector = parse_target(Some(target))?;
        let wire_signal = TerminalSignal::from(signal);
        if phux_client::signal::is_dangerous(wire_signal) && !yes {
            ctx.rt
                .block_on(refuse_kind_before_asking(ctx.socket(), &selector))?;
            let action = format!("signal {target} ({signal:?})");
            if let Err(code) = crate::commands::confirm::confirmed_for(json, yes, &action) {
                // Declining has already been reported.
                return Ok(code);
            }
        }
        ctx.rt.block_on(async {
            let mut notices = Vec::new();
            let result = phux_client::signal::deliver(
                ctx.socket(),
                &selector,
                wire_signal,
                key,
                &mut notices,
            )
            .await;
            for notice in &notices {
                warn!("{} saw only part of the fleet — {notice}", "signal");
            }
            let delivered = result.map_err(|err| signal_refusal(ctx, target, err))?;
            for message in delivered.interleaved {
                warn!("partial results — {message}");
            }
            check_applied("signal", target, delivered.outcome)?;
            if json {
                return Ok(crate::output::json(&serde_json::json!({
                    "schema_version": SUPERVISE_SCHEMA_VERSION,
                    "target": target,
                    "signal": format!("{signal:?}").to_lowercase(),
                    "delivered": true,
                })));
            }
            outln!("phux: signalled {target} ({signal:?})");
            Ok(ExitCode::SUCCESS)
        })
    })
}

/// A signal that never reached the server's decision. A miss keeps status
/// 1 like every single-pane verb, and still never claims absence on a
/// partial view.
fn signal_refusal(ctx: &Ctx<PathBuf>, target: &str, err: SignalError) -> Refusal {
    match err {
        SignalError::Miss { degradation } => {
            partial::target_miss_keeping_status(Some(target), &degradation)
        }
        SignalError::Agent(err) => crate::commands::agent_refusal(&err, true),
        SignalError::WrongKind(err) => read_kind::wrong_kind_refusal(&err),
        SignalError::Attach(err) => ctx.unreachable(&err, "signal"),
    }
}

/// A dangerous signal asks first, so a target that is not a Terminal is
/// refused before the question rather than after it. Only an exact ID can
/// name another kind; anything else, or an unreachable server, is left to
/// [`phux_client::signal::deliver`] to resolve and report.
async fn refuse_kind_before_asking(
    socket: &std::path::Path,
    selector: &phux_client::selector::Selector,
) -> Result<(), Refusal> {
    let Some(id) = selector.explicit_id() else {
        return Ok(());
    };
    let Ok(view) = phux_client::state::get_state(socket).await else {
        return Ok(());
    };
    let resolved = phux_client::read::Resolved::in_snapshot(view.snapshot(), id);
    phux_client::read::require_terminal(Verb::Signal, &resolved)
        .map_err(|err| read_kind::wrong_kind_refusal(&err))
}

/// Run `phux take` as parsed.
pub(crate) fn take(args: crate::commands::TakeArgs, socket: Option<PathBuf>) -> ExitCode {
    let crate::commands::TakeArgs { target, ttl, json } = args;
    run_take(&target, ttl, json.json, socket)
}

/// Run `phux give` as parsed.
pub(crate) fn give(args: crate::commands::GiveArgs, socket: Option<PathBuf>) -> ExitCode {
    let crate::commands::GiveArgs { target, json } = args;
    run_give(&target, json.json, socket)
}

/// Run `phux signal` as parsed: resolve `--idempotency-key`, then signal.
pub(crate) fn signal(args: crate::commands::SignalArgs, socket: Option<PathBuf>) -> ExitCode {
    let crate::commands::SignalArgs {
        target,
        signal,
        idempotency_key,
        yes,
        json,
    } = args;
    match crate::commands::spawn::parse_key_arg(idempotency_key.as_deref(), json.json) {
        Ok(key) => run_signal(&target, signal, key, yes, json.json, socket),
        Err(code) => code,
    }
}
