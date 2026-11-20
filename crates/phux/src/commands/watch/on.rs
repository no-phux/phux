//! Foreground, live-only reactions to the pane watch stream.

use std::ffi::OsString;
use std::io;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

use phux_client::attach::AttachError;
use phux_client::watch::{
    CONTROLLER_EVENT, WatchEvent, WatchOutcome, event_name, watch_live_async,
};
use phux_config::socket::default_socket_path;
use phux_protocol::{
    ids::ResourceId,
    wire::frame::{AgentEvent, ResourceLifecycle},
};
use tokio::signal::unix::{SignalKind, signal};

use super::render::watch_event_json;
use super::{AGENT_STATE_EVENT, WATCH_EVENT_NAMES};
use crate::commands::{OnArgs, cli_runtime, json_err, parse_selector, resolve_target};

mod handler;

const HANDLER_TIMEOUT: Duration = Duration::from_secs(30);
const GAPS: [&str; 2] = ["journal_gap", "source_gap"];

fn dispatchable(name: &str) -> bool {
    !GAPS.contains(&name) && ![AGENT_STATE_EVENT, CONTROLLER_EVENT].contains(&name)
}

pub(crate) fn run(args: &OnArgs, socket: Option<PathBuf>) -> ExitCode {
    if !WATCH_EVENT_NAMES.contains(&args.event.as_str()) || !dispatchable(&args.event) {
        let names: Vec<_> = WATCH_EVENT_NAMES
            .iter()
            .copied()
            .filter(|name| dispatchable(name))
            .collect();
        return json_err::emit(
            false,
            &json_err::CliError::new(
                json_err::codes::UNKNOWN_EVENT_NAME,
                format!("'{}' is not a dispatchable event", args.event),
                format!("use one of: {}", names.join(", ")),
            ),
            crate::exit_codes::EXIT_USAGE,
        );
    }
    let rt = match cli_runtime() {
        Ok(rt) => rt,
        Err(code) => return code,
    };
    let socket = socket.unwrap_or_else(default_socket_path);
    rt.block_on(supervise(&socket, args))
}

async fn supervise(socket: &Path, args: &OnArgs) -> ExitCode {
    let signals = || {
        Ok::<_, io::Error>((
            signal(SignalKind::interrupt())?,
            signal(SignalKind::terminate())?,
            signal(SignalKind::hangup())?,
        ))
    };
    let (mut interrupt, mut terminate, mut hangup) = match signals() {
        Ok(signals) => signals,
        Err(err) => {
            error!("on: could not watch for cancellation: {err}");
            return ExitCode::FAILURE;
        }
    };
    tokio::select! {
        biased;
        _ = interrupt.recv() => ExitCode::SUCCESS,
        _ = terminate.recv() => ExitCode::SUCCESS,
        _ = hangup.recv() => ExitCode::SUCCESS,
        code = listen(socket, args) => code,
    }
}

async fn listen(socket: &Path, args: &OnArgs) -> ExitCode {
    let selector = match parse_selector(args.target.as_deref()) {
        Ok(selector) => selector,
        Err(code) => return code,
    };
    let target = match resolve_target(socket, &selector, "on").await {
        Ok(target) => target,
        Err(refusal) => return refusal.emit(false),
    };
    if !target.is_local() {
        error!("on: run this command on the server that owns the pane");
        return ExitCode::FAILURE;
    }
    // Watch anchors to the current journal head, covering the race between
    // reading that head and subscribing. Never take a caller's replay cursor.
    dispatch_events(socket, target, args).await
}

async fn dispatch_events(socket: &Path, target: ResourceId, args: &OnArgs) -> ExitCode {
    let mut failure = None;
    let result = watch_live_async(socket, target.clone(), async |item| {
        match react(&item, &args.event, &args.command).await {
            Ok(()) => !closes_target(&item, &target),
            Err(err) => {
                failure = Some(err);
                false
            }
        }
    })
    .await;
    if let Some(err) = failure {
        error!("on: {err}");
        return ExitCode::FAILURE;
    }
    match result {
        Ok(WatchOutcome::Stopped) => ExitCode::SUCCESS,
        Ok(WatchOutcome::Ended | WatchOutcome::TimedOut) => {
            error!("on: event stream ended; no reconnect or replay was attempted");
            ExitCode::FAILURE
        }
        Err(err @ AttachError::Io(_)) => json_err::report_no_server(false, &err, socket, "on"),
        Err(err) => {
            error!("on: event stream failed: {err}");
            ExitCode::FAILURE
        }
    }
}

fn closes_target(item: &WatchEvent, target: &ResourceId) -> bool {
    // The watch also sees lifecycle events for children of this pane.
    item.terminal.as_ref() == Some(target)
        && matches!(
            item.event,
            AgentEvent::ResourceClosed { .. }
                | AgentEvent::TerminalControl {
                    lifecycle: ResourceLifecycle::Exited,
                    ..
                }
        )
}

async fn react(item: &WatchEvent, event: &str, argv: &[OsString]) -> Result<(), String> {
    let name = event_name(&item.event);
    if GAPS.contains(&name) {
        let gap = watch_event_json(item).map_err(|err| err.to_string())?;
        return Err(format!("event delivery gap: {gap}"));
    }
    if event == name {
        let line = watch_event_json(item).map_err(|err| err.to_string())?;
        handle(argv, line, HANDLER_TIMEOUT).await?;
    }
    Ok(())
}

async fn handle(argv: &[OsString], mut line: String, timeout: Duration) -> Result<(), String> {
    let Some((program, arguments)) = argv.split_first() else {
        return Err("missing handler executable".to_owned());
    };
    line.push('\n');
    let result = tokio::time::timeout(timeout, handler::invoke(program, arguments, &line)).await;
    match result {
        Ok(Ok(status)) if status.success() => Ok(()),
        Ok(Ok(status)) => Err(format!("handler failed: {status}")),
        Ok(Err(err)) => Err(format!("could not run handler: {err}")),
        Err(_) => Err(format!("handler timed out after {}s", timeout.as_secs())),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "test setup")]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    use super::*;

    #[tokio::test]
    async fn an_executable_text_file_without_a_shebang_is_not_run_in_a_shell() {
        let dir = tempfile::tempdir().unwrap();
        let handler = dir.path().join("text-file");
        std::fs::write(&handler, "exit 0\n").unwrap();
        std::fs::set_permissions(&handler, std::fs::Permissions::from_mode(0o700)).unwrap();
        let result = handle(
            &[handler.into_os_string()],
            "{}".into(),
            Duration::from_secs(2),
        )
        .await;
        assert!(result.is_err(), "{result:?}");
    }

    #[tokio::test]
    async fn an_expired_handler_is_a_visible_failure() {
        let result = handle(
            &["/bin/sleep".into(), "30".into()],
            "{}".into(),
            Duration::from_millis(100),
        )
        .await;
        assert!(result.unwrap_err().contains("handler timed out"));
    }
}
