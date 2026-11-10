//! `phux_signal`: one `SIGNAL_TERMINAL` through the path `phux signal` takes.

use phux_client::signal::LeaseOutcome;
use phux_protocol::wire::frame::TerminalSignal;
use serde_json::{Value, json};

use super::{idempotency_key, unsupported_keyed_signal};
use crate::cli_adapter::{bounded_string, enum_string};
use crate::cli_tools::optional_bool;
use crate::tools::{ToolError, incomplete_view_miss, parse_selector, socket_arg, strict_object};

/// `phux_signal` — one `SIGNAL_TERMINAL` to the resolved pane's process
/// group (ADR-0033), through [`phux_client::signal::deliver`].
pub(crate) async fn signal(args: &Value) -> Result<Value, ToolError> {
    strict_object(
        args,
        &["target", "signal", "confirm", "idempotency_key", "socket"],
        &["target", "signal"],
    )?;
    let target = bounded_string(args, "target", true)?.unwrap_or_default();
    let signal = enum_string(
        args,
        "signal",
        &["interrupt", "freeze", "resume", "terminate", "kill"],
        None,
    )?;
    // `confirm` was checked by the shared table (ADR-0128) before dispatch.
    let _ = optional_bool(args, "confirm")?;
    let key = bounded_string(args, "idempotency_key", false)?
        .as_deref()
        .map(idempotency_key)
        .transpose()?;
    let socket = socket_arg(args)?;
    let selector = parse_selector(&target)?;
    let mut notices = Vec::new();
    let delivered = match phux_client::signal::deliver(
        &socket,
        &selector,
        wire_signal(&signal),
        key,
        &mut notices,
    )
    .await
    {
        Ok(delivered) => delivered,
        Err(err) => return Err(signal_error(&target, err)),
    };
    crate::tools::warn_partial_view("signal", &notices);
    match delivered.outcome {
        LeaseOutcome::Ok => Ok(json!({
            "schema_version": 1,
            "signaled": true,
            "target": target,
            "signal": signal,
        })),
        LeaseOutcome::Refused(message) => Err(ToolError::new(format!(
            "signal refused for {target}: {message}"
        ))),
        LeaseOutcome::Unexpected(other) => Err(ToolError::new(format!(
            "{target}: {}",
            phux_client::explain::explain_unexpected("signal", &other)
        ))),
    }
}

fn signal_error(target: &str, err: phux_client::signal::SignalError) -> ToolError {
    use phux_client::kill::KeyedError;
    use phux_client::signal::SignalError;
    match err {
        SignalError::Miss { degradation } if degradation.is_complete() => {
            ToolError::new("no such target")
        }
        SignalError::Miss { degradation } => incomplete_view_miss(target, degradation.notices()),
        SignalError::Agent(err) => crate::tools::agent_resolve_error(&err),
        SignalError::WrongKind(err) => crate::tools::wrong_kind_error(&err),
        SignalError::Keyed(KeyedError::Unsupported) => unsupported_keyed_signal(),
        SignalError::Keyed(KeyedError::Attach(err)) => err.into(),
    }
}

fn wire_signal(name: &str) -> TerminalSignal {
    match name {
        "interrupt" => TerminalSignal::Interrupt,
        "freeze" => TerminalSignal::Freeze,
        "resume" => TerminalSignal::Resume,
        "terminate" => TerminalSignal::Terminate,
        _ => TerminalSignal::Kill,
    }
}
