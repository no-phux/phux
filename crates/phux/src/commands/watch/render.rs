//! Rendering one watched item: agent-state lines and session events, human
//! and JSON.

use phux_client::resource::control_action_name;
use phux_client::watch::{AgentStateUpdate, WatchEvent, WatchItem};
use phux_protocol::wire::frame::AgentEvent;

use super::AGENT_STATE_EVENT;

/// The `event` name this item renders under: what `--until` matches and the
/// `--json` line carries.
pub(crate) const fn watch_item_event(item: &WatchItem) -> &'static str {
    match item {
        WatchItem::Event(ev) => watch_event_kind(&ev.event),
        WatchItem::AgentState(_) => AGENT_STATE_EVENT,
    }
}

/// Render one streamed [`WatchItem`] to stdout — one line either way.
pub(crate) fn print_watch_item(item: &WatchItem, json: bool) {
    match item {
        WatchItem::Event(ev) => print_watch_event(ev, json),
        WatchItem::AgentState(update) => print_agent_state(update, json),
    }
}

/// Render one [`AgentStateUpdate`] as the `agent_state` line.
pub(crate) fn print_agent_state(update: &AgentStateUpdate, json: bool) {
    let terminal = update
        .terminal
        .as_ref()
        .map(phux_client::selector::format_terminal_id);

    if json {
        match agent_state_json(update, terminal.as_deref()) {
            Ok(s) => outln!("{s}"),
            Err(err) => error!("failed to serialize event: {err}"),
        }
    } else {
        let scope = terminal.as_deref().unwrap_or("server");
        outln!("{scope}\t{AGENT_STATE_EVENT}{}", agent_state_detail(update));
    }
}

/// The human suffix for an `agent_state` line: name, kind, transition, and
/// effective attention. A cleared record still names the agent it was.
pub(super) fn agent_state_detail(update: &AgentStateUpdate) -> String {
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

/// Build the `--json` line for an agent-state change. `state` is `null` on a
/// cleared record, `attention` is the effective level (derived from `state`),
/// and `from` appears only when this watch already saw a record for the pane.
pub(crate) fn agent_state_json(
    update: &AgentStateUpdate,
    terminal: Option<&str>,
) -> Result<String, serde_json::Error> {
    let mut obj = serde_json::Map::new();
    obj.insert(
        "event".to_owned(),
        serde_json::Value::from(AGENT_STATE_EVENT),
    );
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

/// Render one watch event as a line (JSON or compact human form). A
/// serialization failure is reported on stderr and the line skipped.
pub(crate) fn print_watch_event(ev: &WatchEvent, json: bool) {
    if json {
        match watch_event_json(ev) {
            Ok(s) => outln!("{s}"),
            Err(err) => error!("failed to serialize event: {err}"),
        }
        return;
    }
    let terminal = ev
        .terminal
        .as_ref()
        .map(phux_client::selector::format_terminal_id);
    let scope = terminal.as_deref().unwrap_or("server");
    outln!(
        "{scope}\t{}{}",
        watch_event_kind(&ev.event),
        human_detail(&ev.event)
    );
}

/// The compact human suffix for one event line.
pub(super) fn human_detail(event: &AgentEvent) -> String {
    match event {
        AgentEvent::TitleChanged { title } => format!(" {title:?}"),
        AgentEvent::CommandFinished { exit_code } => exit_suffix(*exit_code),
        AgentEvent::ResourceClosed { exit_status } => exit_suffix(*exit_status),
        AgentEvent::Asked { question, .. } => format!(" {question:?}"),
        AgentEvent::CwdChanged { cwd } => format!(" {cwd}"),
        AgentEvent::TerminalControl {
            action,
            exit_status,
            ..
        } => format!(
            " {}{}",
            control_action_name(*action),
            exit_suffix(*exit_status)
        ),
        AgentEvent::JournalGap {
            first_missing,
            last_missing,
        } => format!(" missed {first_missing}..={last_missing}"),
        AgentEvent::SourceGap { dropped } => format!(" dropped={dropped}"),
        AgentEvent::ApprovalRequested { id } => format!(" {id}"),
        AgentEvent::ApprovalDecided { id, outcome } => format!(" {id} {}", outcome.as_str()),
        AgentEvent::Unknown { tag, .. } => format!(" tag={tag}"),
        _ => String::new(),
    }
}

pub(super) fn exit_suffix(code: Option<i32>) -> String {
    code.map_or_else(String::new, |code| format!(" exit={code}"))
}

/// The stable `event` name of an event, shared with MCP `phux_watch`; an
/// unknown tag renders as `unknown`.
pub(super) const fn watch_event_kind(event: &AgentEvent) -> &'static str {
    phux_client::watch::event_name(event)
}

/// Build the `--json` line for a watch event via
/// [`phux_client::watch::event_json`], shared with MCP `phux_watch`.
pub(crate) fn watch_event_json(ev: &WatchEvent) -> Result<String, serde_json::Error> {
    serde_json::to_string(&phux_client::watch::event_json(ev))
}
