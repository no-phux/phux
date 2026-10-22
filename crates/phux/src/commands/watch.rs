use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

use phux_client::attach::AttachError;
use phux_client::resource::cursor::{Cursor, ResumeState};
use phux_client::watch::{WatchItem, WatchOutcome};
use phux_server::runtime::default_socket_path;

use crate::commands::{cli_runtime, json_err, parse_selector, resolve_target};

mod render;
#[cfg(test)]
mod tests;

pub(crate) use render::{print_watch_item, watch_item_event};

/// The `event` name emitted for a `phux.agent/v1` record change.
const AGENT_STATE_EVENT: &str = "agent_state";

/// The `event` names this stream emits, and so exactly the words `--until`
/// accepts. The stream carries no `schema_version`, so this vocabulary is its
/// compatibility unit (frozen by ADR-0071). Keep it sorted and in step with
/// [`watch_event_kind`] and [`AGENT_STATE_EVENT`]; `unknown` is included because
/// the stream really prints it for an event tag this binary predates.
pub(crate) const WATCH_EVENT_NAMES: &[&str] = &[
    AGENT_STATE_EVENT,
    "approval_decided",
    "approval_requested",
    "asked",
    "bell",
    "command_finished",
    "command_started",
    "cwd_changed",
    "dirty",
    "idle",
    "journal_gap",
    "pane_closed",
    "pane_spawned",
    "source_gap",
    "terminal_control",
    "title_changed",
    "unknown",
];

/// The `--until` gate names frozen at 1.0 (ADR-0071 point 6). `--until
/// unknown` keeps matching events named later, which printed as `unknown`
/// before they had names.
pub(crate) const FROZEN_GATE_NAMES: &[&str] = &[
    AGENT_STATE_EVENT,
    "asked",
    "bell",
    "command_finished",
    "command_started",
    "dirty",
    "idle",
    "pane_closed",
    "pane_spawned",
    "title_changed",
    "unknown",
];

/// Whether `--until NAME` is satisfied by `item` (see
/// [`FROZEN_GATE_NAMES`]).
pub(crate) fn gate_matches(name: &str, item: &WatchItem) -> bool {
    let event = watch_item_event(item);
    name == event || (name == "unknown" && !FROZEN_GATE_NAMES.contains(&event))
}

/// Arguments for [`run_watch`], mirroring the clap `Watch` variant.
#[derive(Debug)]
pub(crate) struct WatchArgs<'a> {
    /// Target selector; `None` means the most-recently-focused session.
    pub(crate) session: Option<&'a str>,
    /// Event names that satisfy the watch. Empty streams until EOF/Ctrl-C.
    pub(crate) until: &'a [String],
    /// Seconds after which an unsatisfied watch gives up (exit 124).
    pub(crate) timeout: Option<u64>,
    /// `--after CURSOR`: resume from a previous run's journal position.
    pub(crate) after: Option<&'a str>,
    /// Emit NDJSON rather than the compact human form.
    pub(crate) json: bool,
    /// Server socket override.
    pub(crate) socket: Option<PathBuf>,
}

/// `phux watch [TARGET]` — stream a pane's live events and its
/// `phux.agent/v1` record, one line per item, until EOF or Ctrl-C, without
/// attaching or resizing. `--json` keeps stdout pure NDJSON.
///
/// No `schema_version` on this stream (ADR-0071): a per-line field is overhead
/// and a header line is missed by reconnecting consumers. The binary version
/// and the `event` vocabulary are the contract; consumers ignore unknown events
/// and fields (see `docs/consumers/agents.md`).
///
/// `--until EVENT` gates the stream: the first matching item is printed and the
/// watch exits 0. `--timeout SECS` always exits 124 on expiry, like `wait`. A
/// server EOF with an unsatisfied `--until` exits 1 (the event did not happen);
/// without `--until`, EOF is exit 0. Ctrl-C is always exit 0. Diagnostics go to
/// stderr; no summary is appended to stdout.
pub(crate) fn run_watch(args: WatchArgs<'_>) -> ExitCode {
    let WatchArgs {
        session,
        until,
        timeout,
        after,
        json,
        socket,
    } = args;

    // Validate the gate before touching the network. An event name outside
    // the vocabulary can never arrive, so accepting it would buy the caller a
    // silent wait to the deadline instead of an answer.
    for name in until {
        if !WATCH_EVENT_NAMES.contains(&name.as_str()) {
            return json_err::emit(
                json,
                &json_err::CliError::new(
                    json_err::codes::UNKNOWN_EVENT_NAME,
                    format!("'{name}' is not an event this stream emits"),
                    format!("use one of: {}", WATCH_EVENT_NAMES.join(", ")),
                ),
                crate::exit_codes::EXIT_USAGE,
            );
        }
    }
    let after = match after.map(str::parse::<Cursor>).transpose() {
        Ok(after) => after,
        Err(err) => return crate::commands::resource::invalid_cursor(json, &err),
    };

    let selector = match parse_selector(session) {
        Ok(sel) => sel,
        Err(code) => return code,
    };
    let deadline = timeout.map(Duration::from_secs);
    let socket_path = socket.unwrap_or_else(default_socket_path);
    let rt = match cli_runtime() {
        Ok(rt) => rt,
        Err(code) => return code,
    };

    rt.block_on(async move {
        // A resumed `@N` needs no live inventory: the replay can reach the
        // events of a pane that has since closed and was not retained.
        let direct = after.is_some().then(|| selector.explicit_id()).flatten();
        let terminal_id = match direct {
            Some(id) => id,
            None => match resolve_target(&socket_path, &selector, "watch", json).await {
                Ok(id) => id,
                Err(code) => return code,
            },
        };

        // Race the stream against Ctrl-C (exit 0); the deadline lives in
        // `watch_resumable` so it also covers the connect, which always gets
        // the first-read floor (`--timeout 0` subscribes once, as `wait` reads
        // once). `resume` outlives the stream so every ending reports where to
        // resume.
        let mut resume = ResumeState::new(after);
        let code = {
            let stream = phux_client::watch::watch_resumable(
                &socket_path,
                terminal_id,
                &mut resume,
                deadline,
                |item| {
                    print_watch_item(&item, json);
                    // An empty `--until` set is the unbounded stream: nothing
                    // satisfies it, so it never stops early.
                    !until.iter().any(|name| gate_matches(name, &item))
                },
            );
            tokio::pin!(stream);
            tokio::select! {
                result = &mut stream => watch_exit(result, until, deadline, json, &socket_path),
                _ = tokio::signal::ctrl_c() => ExitCode::SUCCESS,
            }
        };
        report_cursor(&resume, json);
        code
    })
}

/// The exit code for a finished watch (see [`run_watch`] for the mapping).
fn watch_exit(
    result: Result<WatchOutcome, AttachError>,
    until: &[String],
    deadline: Option<Duration>,
    json: bool,
    socket_path: &Path,
) -> ExitCode {
    match result {
        Ok(WatchOutcome::Stopped) => ExitCode::SUCCESS,
        Ok(WatchOutcome::TimedOut) => {
            error!(
                "watch timed out after {}s{}",
                deadline.map_or(0, |d| d.as_secs()),
                describe_gate(until),
            );
            ExitCode::from(crate::exit_codes::EXIT_WAIT_TIMEOUT)
        }
        Ok(WatchOutcome::Ended) if until.is_empty() => ExitCode::SUCCESS,
        Ok(WatchOutcome::Ended) => json_err::emit(
            json,
            &json_err::CliError::new(
                json_err::codes::STREAM_ENDED,
                format!(
                    "the server closed the event stream before {} arrived",
                    gate_alternatives(until),
                ),
                "the pane's session ended or the server exited; \
                 `phux ls` shows what is left",
            ),
            crate::exit_codes::EXIT_FAILURE,
        ),
        Err(err @ AttachError::Io(_)) => {
            json_err::report_no_server(json, &err, socket_path, "watch")
        }
        Err(err) => {
            error!("watch failed: {err}");
            ExitCode::FAILURE
        }
    }
}

/// The last stderr line: where to resume with `--after` (ADR-0123), as JSON
/// under `--json`. Nothing without an event journal.
fn report_cursor(resume: &ResumeState, json: bool) {
    let cursor = resume.cursor();
    if json {
        if cursor.is_some() || resume.cursor_void() {
            // Resume-cursor JSON is a machine stderr contract, not a diagnostic.
            eprintln!(
                "{}",
                serde_json::json!({
                    "cursor": cursor.as_ref().map(ToString::to_string),
                    "cursor_void": resume.cursor_void(),
                })
            );
        }
        return;
    }
    if resume.cursor_void() {
        warn!(
            "watch: the --after cursor belongs to another server run; \
             streamed live events only"
        );
    }
    if let Some(cursor) = cursor {
        info!("watch cursor {cursor} (resume with --after {cursor})");
    }
}

/// The `--until` names as prose ("asked or idle") for diagnostics.
fn gate_alternatives(until: &[String]) -> String {
    until.join(" or ")
}

/// The trailing clause naming what the watch was waiting for, or the empty
/// string when it was an unbounded stream with no gate.
fn describe_gate(until: &[String]) -> String {
    if until.is_empty() {
        String::new()
    } else {
        format!(" waiting for {}", gate_alternatives(until))
    }
}
