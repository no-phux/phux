use std::path::PathBuf;
use std::process::ExitCode;

use phux_client::attach::AttachError;
use phux_client::watch::WatchEvent;
use phux_protocol::wire::frame::AgentEvent;
use phux_server::runtime::default_socket_path;

use crate::commands::{cli_runtime, json_err, parse_selector, resolve_target};

/// `phux watch [TARGET]` — stream a pane's live events (SPEC §7.5,
/// ADR-0022 'events', `phux-y2t`).
///
/// Resolves `TARGET` (a selector; default: the focused session) to a pane
/// client-side, subscribes to the server's `EVENT` stream scoped to that
/// pane, and prints one event per line until EOF (server gone) or Ctrl-C.
/// The subscription neither attaches nor resizes the pane.
///
/// `--json` emits one JSON object per line and keeps stdout pure (the
/// resolved-target diagnostics and connect errors go to stderr); the
/// human form is a compact one-liner.
pub(crate) fn run_watch(session: Option<&str>, json: bool, socket: Option<PathBuf>) -> ExitCode {
    let selector = match parse_selector(session) {
        Ok(sel) => sel,
        Err(code) => return code,
    };
    let socket_path = socket.unwrap_or_else(default_socket_path);
    let rt = match cli_runtime() {
        Ok(rt) => rt,
        Err(code) => return code,
    };

    rt.block_on(async move {
        let terminal_id = match resolve_target(&socket_path, &selector, "watch", json).await {
            Ok(id) => id,
            Err(code) => return code,
        };

        // Stream until EOF or Ctrl-C. `tokio::select!` races the event
        // stream against the interrupt so Ctrl-C exits cleanly (exit 0 —
        // the user asked to stop, not a failure).
        let stream = phux_client::watch::watch_events(&socket_path, Some(terminal_id), |ev| {
            print_watch_event(&ev, json);
            true
        });
        tokio::pin!(stream);
        tokio::select! {
            result = &mut stream => match result {
                Ok(()) => ExitCode::SUCCESS,
                Err(err @ AttachError::Io(_)) => {
                    json_err::report_no_server(json, &err, &socket_path, "watch")
                }
                Err(err) => {
                    eprintln!("phux: watch failed: {err}");
                    ExitCode::FAILURE
                }
            },
            _ = tokio::signal::ctrl_c() => ExitCode::SUCCESS,
        }
    })
}

/// Render one [`phux_client::watch::WatchEvent`] to stdout — one line, as
/// JSON (`--json`) or a compact human form. Keeps stdout pure JSON under
/// `--json` (no human framing). A serialization failure is reported to
/// stderr and the line skipped rather than aborting the stream.
pub(crate) fn print_watch_event(ev: &WatchEvent, json: bool) {
    // A stable, scriptable name for each event kind (matches the spec
    // taxonomy in §7.5.1).
    let kind = watch_event_kind(&ev.event);
    let terminal = ev
        .terminal
        .as_ref()
        .map(crate::selector::format_terminal_id);

    if json {
        match watch_event_json(ev, kind, terminal.as_deref()) {
            Ok(s) => outln!("{s}"),
            Err(err) => eprintln!("phux: failed to serialize event: {err}"),
        }
    } else {
        let scope = terminal.as_deref().unwrap_or("server");
        let detail = match &ev.event {
            AgentEvent::TitleChanged { title } => format!(" {title:?}"),
            AgentEvent::CommandFinished { exit_code } => {
                exit_code.map_or_else(String::new, |c| format!(" exit={c}"))
            }
            AgentEvent::PaneClosed { exit_status } => {
                exit_status.map_or_else(String::new, |c| format!(" exit={c}"))
            }
            AgentEvent::Asked { question, .. } => format!(" {question:?}"),
            AgentEvent::Unknown { tag, .. } => format!(" tag={tag}"),
            _ => String::new(),
        };
        outln!("{scope}\t{kind}{detail}");
    }
}

const fn watch_event_kind(event: &AgentEvent) -> &'static str {
    match event {
        AgentEvent::CommandStarted => "command_started",
        AgentEvent::CommandFinished { .. } => "command_finished",
        AgentEvent::TitleChanged { .. } => "title_changed",
        AgentEvent::Bell => "bell",
        AgentEvent::PaneSpawned => "pane_spawned",
        AgentEvent::PaneClosed { .. } => "pane_closed",
        AgentEvent::Dirty => "dirty",
        AgentEvent::Idle => "idle",
        AgentEvent::Asked { .. } => "asked",
        // `AgentEvent::Unknown` (a tag this client predates, preserved by
        // the decoder) and any future `#[non_exhaustive]` variant both
        // render generically rather than failing the stream.
        _ => "unknown",
    }
}

/// Build the `--json` line for a watch event: a single JSON object with a
/// stable `event` name, an optional `terminal` selector, and the event's
/// payload field (`title` / `exit_code` / `exit_status` / `tag`). Pure
/// function over the event so the wire-to-JSON projection is unit-testable
/// without touching stdout.
pub(crate) fn watch_event_json(
    ev: &WatchEvent,
    kind: &str,
    terminal: Option<&str>,
) -> Result<String, serde_json::Error> {
    let mut obj = serde_json::Map::new();
    obj.insert("event".to_owned(), serde_json::Value::from(kind));
    if let Some(t) = terminal {
        obj.insert("terminal".to_owned(), serde_json::Value::from(t));
    }
    match &ev.event {
        AgentEvent::TitleChanged { title } => {
            obj.insert("title".to_owned(), serde_json::Value::from(title.clone()));
        }
        AgentEvent::CommandFinished { exit_code } => {
            obj.insert(
                "exit_code".to_owned(),
                exit_code.map_or(serde_json::Value::Null, serde_json::Value::from),
            );
        }
        AgentEvent::PaneClosed { exit_status } => {
            obj.insert(
                "exit_status".to_owned(),
                exit_status.map_or(serde_json::Value::Null, serde_json::Value::from),
            );
        }
        AgentEvent::Asked {
            id,
            question,
            suggestions,
            elapsed_seconds,
        } => {
            obj.insert("id".to_owned(), serde_json::Value::from(id.clone()));
            obj.insert(
                "question".to_owned(),
                serde_json::Value::from(question.clone()),
            );
            obj.insert(
                "suggestions".to_owned(),
                serde_json::Value::Array(
                    suggestions
                        .iter()
                        .cloned()
                        .map(serde_json::Value::from)
                        .collect(),
                ),
            );
            obj.insert(
                "elapsed_seconds".to_owned(),
                elapsed_seconds.map_or(serde_json::Value::Null, serde_json::Value::from),
            );
        }
        AgentEvent::Unknown { tag, .. } => {
            obj.insert("tag".to_owned(), serde_json::Value::from(*tag));
        }
        _ => {}
    }
    serde_json::to_string(&serde_json::Value::Object(obj))
}

#[cfg(test)]
mod tests {
    use phux_client::watch::WatchEvent;
    use phux_protocol::wire::frame::AgentEvent;

    use super::{watch_event_json, watch_event_kind};

    /// Build the JSON line for an event and parse it back, asserting the
    /// shape the `phux watch --json` contract promises: one object with a
    /// stable `event` name, an optional `terminal` selector, and the
    /// event's payload field.
    fn json_of(event: AgentEvent, terminal: Option<&str>) -> serde_json::Value {
        let ev = WatchEvent {
            terminal: None,
            event,
        };
        let kind = watch_event_kind(&ev.event);
        let line = watch_event_json(&ev, kind, terminal).unwrap();
        // One line, no embedded newline — `phux watch --json` is
        // one-object-per-line.
        assert!(
            !line.contains('\n'),
            "watch --json line must be single-line"
        );
        serde_json::from_str(&line).unwrap()
    }

    #[test]
    fn watch_json_title_changed_carries_title_and_terminal() {
        let v = json_of(
            AgentEvent::TitleChanged {
                title: "build".to_owned(),
            },
            Some("@3"),
        );
        assert_eq!(v["event"], "title_changed");
        assert_eq!(v["title"], "build");
        assert_eq!(v["terminal"], "@3");
    }

    #[test]
    fn watch_json_bell_is_minimal() {
        let v = json_of(AgentEvent::Bell, None);
        assert_eq!(v["event"], "bell");
        // No terminal selector supplied → key absent (not null).
        assert!(v.get("terminal").is_none());
        // Bell carries no payload field.
        assert!(v.get("title").is_none());
    }

    #[test]
    fn watch_json_pane_closed_carries_exit_status() {
        let v = json_of(
            AgentEvent::PaneClosed {
                exit_status: Some(0),
            },
            Some("@1"),
        );
        assert_eq!(v["event"], "pane_closed");
        assert_eq!(v["exit_status"], 0);

        // A signal-killed pane reports null exit_status (present, not absent).
        let v = json_of(AgentEvent::PaneClosed { exit_status: None }, Some("@1"));
        assert!(v["exit_status"].is_null());
    }

    #[test]
    fn watch_json_command_finished_exit_code_nullable() {
        // The documented exit-code gap: the reference server emits None.
        let v = json_of(AgentEvent::CommandFinished { exit_code: None }, None);
        assert_eq!(v["event"], "command_finished");
        assert!(v["exit_code"].is_null());
    }

    #[test]
    fn watch_json_asked_carries_question() {
        let v = json_of(
            AgentEvent::Asked {
                id: "q1".to_owned(),
                question: "Deploy to prod?".to_owned(),
                suggestions: vec!["Yes".to_owned(), "No".to_owned(), "Hold".to_owned()],
                elapsed_seconds: None,
            },
            Some("@9"),
        );
        assert_eq!(v["event"], "asked");
        assert_eq!(v["terminal"], "@9");
        assert_eq!(v["id"], "q1");
        assert_eq!(v["question"], "Deploy to prod?");
        assert_eq!(v["suggestions"], serde_json::json!(["Yes", "No", "Hold"]));
        assert!(v["elapsed_seconds"].is_null());
    }
}
