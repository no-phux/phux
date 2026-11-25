#![allow(
    clippy::future_not_send,
    reason = "client-side libghostty Terminal is !Send; ADR-0003 binds us to current-thread"
)]

use std::cell::RefCell;
use std::io::{self, IsTerminal};
use std::path::Path;
use std::process::ExitCode;
use std::rc::Rc;

use phux_client::attach::connection::Connection;
use phux_client::attach::{AttachEnd, AttachError, Dial, InputReplayJournal};
use phux_client::predict::PredictiveConfig;
use phux_config::loader as config_loader;
use phux_protocol::wire::frame::AttachTarget;
use phux_tui::attach::{self, record::SessionRecorder, status_bar::Notice};

use crate::commands::rec::RecordSpec;

/// A live `--rec` recorder, shared so a graceful-upgrade reconnect continues
/// the same recording.
type RecorderHandle = Rc<RefCell<SessionRecorder>>;

/// The ADR-0053 acknowledged-input replay journal, shared across reconnect
/// attempts so a journaled operation is replayed under its original id. Remote
/// dials only.
type ReplayHandle = Rc<RefCell<InputReplayJournal>>;

/// Refuse interactive entry points before they can start a server, connect,
/// or let the driver write terminal-control sequences.
pub(crate) fn interactive_tty_preflight() -> Result<(), ExitCode> {
    if io::stdin().is_terminal() && io::stdout().is_terminal() {
        return Ok(());
    }
    error!("interactive use requires both stdin and stdout to be terminals");
    Err(ExitCode::FAILURE)
}

/// The override for [`nested_attach_guard`]: set non-empty to attach a TUI to
/// the server whose pane it is running in anyway.
pub(crate) const ALLOW_NESTED_ENV: &str = "PHUX_ALLOW_NESTED";

/// Refuse to attach an interactive client to the very server whose pane this
/// process is running in. That client would render itself inside itself:
/// every frame it draws is pane output it then has to draw again. A pane
/// carries `PHUX_TERMINAL_ID` and `PHUX_SOCKET` (set at spawn); attaching to
/// another server, a registered host, or a QUIC endpoint never trips this.
pub(crate) fn nested_attach_guard(socket_path: &Path) -> Result<(), ExitCode> {
    let env = |name: &str| std::env::var_os(name).filter(|value| !value.is_empty());
    if !is_nested_attach(
        socket_path,
        env("PHUX_TERMINAL_ID").is_some(),
        env("PHUX_SOCKET").as_deref().map(Path::new),
        env(ALLOW_NESTED_ENV).is_some(),
    ) {
        return Ok(());
    }
    error!(
        "refusing to attach from inside one of this server's own panes \
         (the client would draw itself inside itself)."
    );
    hint!(
        "Switch sessions with the session picker (`C-a s`), or set \
         {ALLOW_NESTED_ENV}=1 if nesting is intentional."
    );
    Err(ExitCode::from(2))
}

/// [`nested_attach_guard`]'s decision with the environment injected.
fn is_nested_attach(
    socket_path: &Path,
    in_pane: bool,
    pane_socket: Option<&Path>,
    allow_nested: bool,
) -> bool {
    if !in_pane || allow_nested {
        return false;
    }
    let Some(pane_socket) = pane_socket else {
        return false;
    };
    let canonical = |path: &Path| path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    canonical(pane_socket) == canonical(socket_path)
}

/// Explain how a successful attach ended, once the terminal is cooked again:
/// a detach says nothing, a last-pane death prints one line. Covers the paths
/// that return an [`AttachEnd`] rather than exiting inside the driver.
pub(crate) fn report_attach_end(end: AttachEnd) {
    if let Some(line) = end.explanation() {
        // AttachEnd::explanation owns a legacy phux prefix.
        info!("{}", line.strip_prefix("phux: ").unwrap_or(&line));
    }
}

/// Export the `--rec` capture and report it, once the TUI is down. Runs even
/// when the attach ended badly: the bytes up to the failure are playable.
fn finalize_recording(rec: Option<&RecordSpec>) {
    if let Some(spec) = rec {
        crate::commands::rec::finalize(spec);
    }
}


/// Drive one attach attempt against `socket_path` with `target`, picking
/// the predict-enabled entry point iff the user opted in.
pub(crate) async fn run_attach_once(
    dial: &Dial,
    target: AttachTarget,
    predict_cfg: PredictiveConfig,
) -> Result<AttachEnd, AttachError> {
    run_attach_once_rec(dial, target, predict_cfg, None, None, None).await
}

/// [`run_attach_once`] with an optional live recorder and an optional
/// status-bar notice (the reconnect loop's "re-attached" message).
pub(crate) async fn run_attach_once_rec(
    dial: &Dial,
    target: AttachTarget,
    predict_cfg: PredictiveConfig,
    rec: Option<RecorderHandle>,
    initial_notice: Option<Notice>,
    input_replay: Option<ReplayHandle>,
) -> Result<AttachEnd, AttachError> {
    // `predict.enabled = false` is the non-predictive path, and `rec` only
    // wraps the driver's render sink with the tee, so one call covers every
    // transport and mode.
    attach::run_dial(dial, target, predict_cfg, rec, initial_notice, input_replay).await
}

async fn run_attach_connection_rec(
    connection: Connection,
    dial: &Dial,
    target: AttachTarget,
    predict_cfg: PredictiveConfig,
    rec: Option<RecorderHandle>,
    initial_notice: Option<Notice>,
    input_replay: Option<ReplayHandle>,
) -> Result<AttachEnd, AttachError> {
    Box::pin(attach::run_connection(
        connection,
        dial,
        target,
        predict_cfg,
        rec,
        initial_notice,
        input_replay,
    ))
    .await
}

/// Attach to the user's default session via `Last` (over `connection` when
/// one is already open); if an older server refuses it, make exactly one
/// lookup-only retry by name (never `CreateIfMissing`).
async fn attach_default_with_fallback(
    connection: Option<Connection>,
    dial: &Dial,
    default_name: &str,
    predict_cfg: PredictiveConfig,
    rec: Option<&RecorderHandle>,
    initial_notice: Option<Notice>,
    input_replay: Option<&ReplayHandle>,
) -> Result<AttachEnd, AttachError> {
    let first = match connection {
        Some(connection) => {
            Box::pin(run_attach_connection_rec(
                connection,
                dial,
                AttachTarget::Last,
                predict_cfg,
                rec.map(Rc::clone),
                initial_notice.clone(),
                input_replay.map(Rc::clone),
            ))
            .await
        }
        None => {
            run_attach_once_rec(
                dial,
                AttachTarget::Last,
                predict_cfg,
                rec.map(Rc::clone),
                initial_notice.clone(),
                input_replay.map(Rc::clone),
            )
            .await
        }
    };
    match first {
        // A live old server will not grow a session if asked again.
        Err(err) if phux_client::attach::is_incompatible_protocol(&err) => Err(err),
        Err(AttachError::Refused(message)) => {
            warn!(
                "server could not resolve the last session ({message}); trying existing `{default_name}`"
            );
            run_attach_once_rec(
                dial,
                default_lookup_target(default_name),
                predict_cfg,
                rec.map(Rc::clone),
                initial_notice,
                input_replay.map(Rc::clone),
            )
            .await
        }
        other => other,
    }
}

struct AttachAttempt<'a> {
    dial: &'a Dial,
    target: &'a AttachTarget,
    predict_cfg: PredictiveConfig,
    default_name: Option<&'a str>,
    recorder: Option<&'a RecorderHandle>,
    input_replay: Option<&'a ReplayHandle>,
}

impl AttachAttempt<'_> {
    async fn run(
        &self,
        connection: Option<Box<Connection>>,
        initial_notice: Option<Notice>,
    ) -> Result<AttachEnd, AttachError> {
        if let Some(name) = self.default_name {
            return Box::pin(attach_default_with_fallback(
                connection.map(|connection| *connection),
                self.dial,
                name,
                self.predict_cfg,
                self.recorder,
                initial_notice,
                self.input_replay,
            ))
            .await;
        }
        let (recorder, replay) = (
            self.recorder.map(Rc::clone),
            self.input_replay.map(Rc::clone),
        );
        match connection {
            Some(connection) => {
                Box::pin(run_attach_connection_rec(
                    *connection,
                    self.dial,
                    self.target.clone(),
                    self.predict_cfg,
                    recorder,
                    initial_notice,
                    replay,
                ))
                .await
            }
            None => {
                Box::pin(run_attach_once_rec(
                    self.dial,
                    self.target.clone(),
                    self.predict_cfg,
                    recorder,
                    initial_notice,
                    replay,
                ))
                .await
            }
        }
    }
}

/// The client's cwd as a wire `cwd`, so a seed pane starts in the user's
/// project directory rather than the daemon's. `None` only when unreadable; the
/// server validates it and falls back to its default.
pub(crate) fn client_cwd() -> Option<String> {
    std::env::current_dir()
        .ok()
        .map(|path| path.to_string_lossy().into_owned())
}

/// Flush the residual UTF-8 tail, backfill `duration`, and close the cast.
/// Idempotent: the clean-detach path finalizes before `process::exit`.
fn close_recorder(recorder: Option<RecorderHandle>) {
    let Some(handle) = recorder else {
        return;
    };
    if let Err(err) = handle.borrow_mut().finish_in_place() {
        tracing::warn!(error = %err, "closing the session recording failed");
    }
}


/// The predictive-echo setting for one dial: the config's explicit value,
/// else on for a dial that leaves the machine. A config that fails to load
/// disables prediction: an unreadable file may well have said `false`.
pub(crate) fn predictive_config_for(dial: &Dial) -> PredictiveConfig {
    match config_loader::load() {
        Ok(cfg) => PredictiveConfig {
            enabled: cfg
                .experimental
                .predictive_echo_for(dial_leaves_the_machine(dial)),
        },
        Err(err) => {
            warn!(
                "config load failed ({err}); predictive echo stays off \
                 (an unreadable config is not consent to turn it on)"
            );
            PredictiveConfig::disabled()
        }
    }
}

/// Whether this dial crosses a network. Keys on the resolved address for
/// QUIC and on the URL host for WebSocket (loopback is local whatever the
/// transport); an unparseable URL answers local, failing closed.
fn dial_leaves_the_machine(dial: &Dial) -> bool {
    match dial {
        Dial::Uds(_) => false,
        Dial::Ssh { .. } => true,
        Dial::Quic(quic) => !quic.addr.ip().is_loopback(),
        Dial::Ws(ws) => {
            phux_dial::ws::WsTarget::parse(&ws.url).is_ok_and(|target| !target.is_loopback())
        }
    }
}

mod home;
mod local;
pub(crate) mod native;
mod reconnect;
pub(crate) mod remote;
mod stdio;
pub(crate) use local::{run_naked, run_attach, run_attach_rec};
pub(crate) use remote::{
    RemoteAttachOutcome, Repair, run_attach_remote, run_attach_remote_outcome,
};
#[cfg(test)]
use local::{session_is_missing, missing_session_lines};
pub(crate) use home::{
    configured_session_name_template, configured_spawn_on_attach, render_default_session_name,
    resolved_default_session_name,
};
use home::default_lookup_target;
pub(crate) use reconnect::attach_with_reconnect;
pub(crate) use stdio::run_attach_stdio;

#[cfg(test)]
mod tests;
