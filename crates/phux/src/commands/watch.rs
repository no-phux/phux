use std::path::PathBuf;
use std::process::ExitCode;

use phux_client::attach::AttachError;
use phux_client::watch::{AgentStateUpdate, WatchEvent, WatchItem};
use phux_protocol::wire::frame::AgentEvent;
use phux_server::runtime::default_socket_path;

use crate::commands::{cli_runtime, json_err, parse_selector, resolve_target};

/// `phux watch [TARGET]` — stream a pane's live events (SPEC §7.5,
/// ADR-0022 'events', `phux-y2t`).
///
/// Resolves `TARGET` (a selector; default: the focused session) to a pane
/// client-side, subscribes to the server's `EVENT` stream *and* to the
/// pane's `phux.agent/v1` L3 record (ADR-0040 / ADR-0046), and prints one
/// line per item until EOF (server gone) or Ctrl-C. The subscription
/// neither attaches nor resizes the pane.
///
/// `--json` emits one JSON object per line and keeps stdout pure (the
/// resolved-target diagnostics and connect errors go to stderr); the
/// human form is a compact one-liner.
///
/// **Decision: no `schema_version` field on this stream** (ADR-0071,
/// phux-k8u0), unlike every other `--json` document in this crate. Two
/// alternatives were rejected: a per-line field is repeated overhead on a
/// hot, high-volume path for a value that essentially never changes within
/// a run; a versioned header line would be invisible to the common consumer
/// shape here — one that attaches, disconnects, and reconnects, or `tail`s
/// an existing pipe, and so may never observe line one. Instead the stream
/// is versioned by the binary itself (`phux --version`) and the
/// compatibility unit is the closed-but-growing `event` name vocabulary: a
/// consumer that does not recognize an `event` value, or a field on one it
/// does, ignores it exactly as [`watch_event_kind`]'s catch-all arm and
/// [`AgentEvent::Unknown`] already do server-side. A shape-breaking change
/// to an existing event is a breaking change to the CLI's frozen JSON
/// surface like any other and requires the major version bump ADR-0071
/// already mandates. Written up in full at `docs/consumers/agents.md`
/// (the `phux watch` entry, §3).
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
        let stream = phux_client::watch::watch_events(&socket_path, Some(terminal_id), |item| {
            print_watch_item(&item, json);
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

/// Render one streamed [`WatchItem`] to stdout — one line either way.
pub(crate) fn print_watch_item(item: &WatchItem, json: bool) {
    match item {
        WatchItem::Event(ev) => print_watch_event(ev, json),
        WatchItem::AgentState(update) => print_agent_state(update, json),
    }
}

/// Render one [`AgentStateUpdate`] to stdout as the `agent_state` line.
///
/// The event name joins the same closed-but-growing vocabulary the rest of
/// this stream uses (see the `run_watch` docs): a consumer that does not
/// recognize `agent_state` ignores the line, and no `schema_version` rides
/// along.
pub(crate) fn print_agent_state(update: &AgentStateUpdate, json: bool) {
    let terminal = update
        .terminal
        .as_ref()
        .map(crate::selector::format_terminal_id);

    if json {
        match agent_state_json(update, terminal.as_deref()) {
            Ok(s) => outln!("{s}"),
            Err(err) => eprintln!("phux: failed to serialize event: {err}"),
        }
    } else {
        let scope = terminal.as_deref().unwrap_or("server");
        outln!("{scope}\tagent_state{}", agent_state_detail(update));
    }
}

/// The compact human suffix for an `agent_state` line: the agent's name and
/// kind, the transition, and the effective attention. A cleared record (the
/// tombstone) renders the name it had and the state it was last in, because
/// "the agent you were waiting on is gone" is the whole point of emitting
/// the line at all.
fn agent_state_detail(update: &AgentStateUpdate) -> String {
    use std::fmt::Write as _;

    let mut out = String::new();
    // Identity survives a cleared record: fall back to the one last seen.
    if let Some(rec) = update.record.as_ref().or(update.previous.as_ref()) {
        let _ = write!(out, " {}", rec.name);
        if let Some(kind) = &rec.kind {
            let _ = write!(out, " kind={kind}");
        }
    }
    let from = update.previous.as_ref().map(|prev| prev.state.as_str());
    if let Some(rec) = &update.record {
        out.push(' ');
        if let Some(from) = from {
            let _ = write!(out, "{from}->");
        }
        let _ = write!(
            out,
            "{} attention={}",
            rec.state.as_str(),
            rec.effective_attention().as_str()
        );
    } else {
        out.push_str(" cleared");
        if let Some(from) = from {
            let _ = write!(out, " (was {from})");
        }
    }
    out
}

/// Build the `--json` line for an agent-state change.
///
/// `state` is `null` on a cleared record rather than the key being absent,
/// so a consumer keyed on `state` sees the record go away instead of
/// silently reading the previous value forever. `attention` is the
/// *effective* level — the ADR-0046 detector never writes the field and
/// L3 §3.7 derives it from `state`, so the declared value would be absent
/// on every server-derived line. `from` is present only when this watch
/// session already saw a record for the same Terminal.
pub(crate) fn agent_state_json(
    update: &AgentStateUpdate,
    terminal: Option<&str>,
) -> Result<String, serde_json::Error> {
    let mut obj = serde_json::Map::new();
    obj.insert("event".to_owned(), serde_json::Value::from("agent_state"));
    if let Some(t) = terminal {
        obj.insert("terminal".to_owned(), serde_json::Value::from(t));
    }
    // Identity survives the tombstone: fall back to the record we last saw
    // so a consumer filtering by agent name still recognizes the line.
    if let Some(rec) = update.record.as_ref().or(update.previous.as_ref()) {
        obj.insert("name".to_owned(), serde_json::Value::from(rec.name.clone()));
        if let Some(kind) = &rec.kind {
            obj.insert("kind".to_owned(), serde_json::Value::from(kind.clone()));
        }
    }
    match &update.record {
        Some(rec) => {
            obj.insert(
                "state".to_owned(),
                serde_json::Value::from(rec.state.as_str()),
            );
            obj.insert(
                "attention".to_owned(),
                serde_json::Value::from(rec.effective_attention().as_str()),
            );
            if let Some(session) = &rec.session {
                obj.insert(
                    "session".to_owned(),
                    serde_json::Value::from(session.clone()),
                );
            }
        }
        None => {
            obj.insert("state".to_owned(), serde_json::Value::Null);
        }
    }
    if let Some(prev) = &update.previous {
        obj.insert(
            "from".to_owned(),
            serde_json::Value::from(prev.state.as_str()),
        );
    }
    serde_json::to_string(&serde_json::Value::Object(obj))
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
    use phux_client::agent_meta::{AgentMetaState, AgentRecord};
    use phux_client::watch::{AgentStateUpdate, WatchEvent};
    use phux_protocol::wire::frame::AgentEvent;

    use super::{agent_state_detail, agent_state_json, watch_event_json, watch_event_kind};

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

    // -- agent_state ------------------------------------------------------

    fn record(name: &str, kind: Option<&str>, state: AgentMetaState) -> AgentRecord {
        AgentRecord {
            name: name.to_owned(),
            kind: kind.map(str::to_owned),
            state,
            ..AgentRecord::default()
        }
    }

    fn agent_json(update: &AgentStateUpdate, terminal: Option<&str>) -> serde_json::Value {
        let line = agent_state_json(update, terminal).unwrap();
        assert!(
            !line.contains('\n'),
            "watch --json line must be single-line"
        );
        serde_json::from_str(&line).unwrap()
    }

    #[test]
    fn agent_state_json_carries_identity_state_and_derived_attention() {
        let update = AgentStateUpdate {
            terminal: None,
            record: Some(record("reviewer", Some("claude"), AgentMetaState::Blocked)),
            previous: None,
        };
        let v = agent_json(&update, Some("@7"));
        assert_eq!(v["event"], "agent_state");
        assert_eq!(v["terminal"], "@7");
        assert_eq!(v["name"], "reviewer");
        assert_eq!(v["kind"], "claude");
        assert_eq!(v["state"], "blocked");
        // The detector never writes `attention`; L3 §3.7 derives it, and the
        // stream carries the derived value so a consumer need not re-derive.
        assert_eq!(v["attention"], "high");
        // Nothing to transition from on the first record for a Terminal.
        assert!(v.get("from").is_none());
    }

    #[test]
    fn agent_state_json_carries_the_transition_when_one_is_known() {
        let update = AgentStateUpdate {
            terminal: None,
            record: Some(record("reviewer", None, AgentMetaState::Blocked)),
            previous: Some(record("reviewer", None, AgentMetaState::Working)),
        };
        let v = agent_json(&update, Some("@7"));
        assert_eq!(v["from"], "working");
        assert_eq!(v["state"], "blocked");
        assert!(v.get("kind").is_none(), "absent kind stays absent");
    }

    #[test]
    fn agent_state_json_tombstone_nulls_state_and_keeps_identity() {
        let update = AgentStateUpdate {
            terminal: None,
            record: None,
            previous: Some(record("reviewer", Some("claude"), AgentMetaState::Blocked)),
        };
        let v = agent_json(&update, Some("@7"));
        assert_eq!(v["event"], "agent_state");
        // Present-and-null, not absent: a consumer keyed on `state` must see
        // the record go away rather than read the previous value forever.
        assert!(v["state"].is_null());
        assert_eq!(v["name"], "reviewer");
        assert_eq!(v["from"], "blocked");
        assert!(v.get("attention").is_none());
    }

    #[test]
    fn agent_state_json_tombstone_with_no_prior_record_is_still_a_line() {
        let update = AgentStateUpdate {
            terminal: None,
            record: None,
            previous: None,
        };
        let v = agent_json(&update, Some("@7"));
        assert_eq!(v["event"], "agent_state");
        assert!(v["state"].is_null());
        assert!(v.get("name").is_none());
        assert!(v.get("from").is_none());
    }

    #[test]
    fn agent_state_human_form_is_a_compact_transition() {
        let update = AgentStateUpdate {
            terminal: None,
            record: Some(record("reviewer", Some("claude"), AgentMetaState::Blocked)),
            previous: Some(record("reviewer", Some("claude"), AgentMetaState::Working)),
        };
        assert_eq!(
            agent_state_detail(&update),
            " reviewer kind=claude working->blocked attention=high"
        );

        let first = AgentStateUpdate {
            previous: None,
            ..update.clone()
        };
        assert_eq!(
            agent_state_detail(&first),
            " reviewer kind=claude blocked attention=high"
        );

        let cleared = AgentStateUpdate {
            record: None,
            ..update
        };
        assert_eq!(
            agent_state_detail(&cleared),
            " reviewer kind=claude cleared (was working)"
        );
    }
}
