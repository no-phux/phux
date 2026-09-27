use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

use phux_client::attach::AttachError;
use phux_client::resource::control_action_name;
use phux_client::resource::cursor::{Cursor, ResumeState};
use phux_client::watch::{AgentStateUpdate, WatchEvent, WatchItem, WatchOutcome};
use phux_protocol::wire::frame::AgentEvent;
use phux_server::runtime::default_socket_path;

use crate::commands::{cli_runtime, json_err, parse_selector, resolve_target};

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
        let direct = after
            .is_some()
            .then(|| crate::commands::resource::direct_id(&selector))
            .flatten();
        let terminal_id = match direct {
            Some(id) => id,
            None => match resolve_target(&socket_path, &selector, "watch", json).await {
                Ok(id) => id,
                Err(code) => return code,
            },
        };

        // Race the stream against Ctrl-C (exit 0); the deadline lives in
        // `watch_resumable` so it also covers the connect. `resume` outlives the
        // stream so every ending reports where to resume.
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
            eprintln!(
                "phux: watch timed out after {}s{}",
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
            eprintln!("phux: watch failed: {err}");
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
        eprintln!(
            "phux: watch: the --after cursor belongs to another server run; \
             streamed live events only"
        );
    }
    if let Some(cursor) = cursor {
        eprintln!("phux: watch cursor {cursor} (resume with --after {cursor})");
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
        .map(crate::selector::format_terminal_id);

    if json {
        match agent_state_json(update, terminal.as_deref()) {
            Ok(s) => outln!("{s}"),
            Err(err) => eprintln!("phux: failed to serialize event: {err}"),
        }
    } else {
        let scope = terminal.as_deref().unwrap_or("server");
        outln!("{scope}\t{AGENT_STATE_EVENT}{}", agent_state_detail(update));
    }
}

/// The human suffix for an `agent_state` line: name, kind, transition, and
/// effective attention. A cleared record still names the agent it was.
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
            Err(err) => eprintln!("phux: failed to serialize event: {err}"),
        }
        return;
    }
    let terminal = ev
        .terminal
        .as_ref()
        .map(crate::selector::format_terminal_id);
    let scope = terminal.as_deref().unwrap_or("server");
    outln!(
        "{scope}\t{}{}",
        watch_event_kind(&ev.event),
        human_detail(&ev.event)
    );
}

/// The compact human suffix for one event line.
fn human_detail(event: &AgentEvent) -> String {
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

fn exit_suffix(code: Option<i32>) -> String {
    code.map_or_else(String::new, |code| format!(" exit={code}"))
}

/// The stable `event` name of an event, shared with MCP `phux_watch`; an
/// unknown tag renders as `unknown`.
const fn watch_event_kind(event: &AgentEvent) -> &'static str {
    phux_client::watch::event_name(event)
}

/// Build the `--json` line for a watch event via
/// [`phux_client::watch::event_json`], shared with MCP `phux_watch`.
pub(crate) fn watch_event_json(ev: &WatchEvent) -> Result<String, serde_json::Error> {
    serde_json::to_string(&phux_client::watch::event_json(ev))
}

#[cfg(test)]
mod tests {
    use phux_client::agent_meta::{AgentMetaState, AgentRecord};
    use phux_client::watch::{AgentStateUpdate, WatchEvent, WatchItem};
    use phux_protocol::wire::frame::AgentEvent;

    use super::{
        AGENT_STATE_EVENT, WATCH_EVENT_NAMES, agent_state_detail, agent_state_json, describe_gate,
        watch_event_json, watch_event_kind, watch_item_event,
    };

    fn pane(selector: &str) -> phux_protocol::ids::ResourceId {
        phux_protocol::ids::ResourceId::local(selector.trim_start_matches('@').parse().unwrap())
    }

    /// Build an event's JSON line and parse it back.
    fn json_of(event: AgentEvent, terminal: Option<&str>) -> serde_json::Value {
        let ev = WatchEvent {
            terminal: terminal.map(pane),
            event,
            stamp: None,
        };
        let line = watch_event_json(&ev).unwrap();
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
            AgentEvent::ResourceClosed {
                exit_status: Some(0),
            },
            Some("@1"),
        );
        assert_eq!(v["event"], "pane_closed");
        assert_eq!(v["exit_status"], 0);

        // A signal-killed pane reports null exit_status (present, not absent).
        let v = json_of(AgentEvent::ResourceClosed { exit_status: None }, Some("@1"));
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

    // -- the --until vocabulary -------------------------------------------

    /// Every `AgentEvent` this binary can be handed, so the vocabulary guard
    /// below covers the whole rendered surface rather than the three variants
    /// someone remembered.
    fn every_event() -> Vec<AgentEvent> {
        vec![
            AgentEvent::CommandStarted,
            AgentEvent::CommandFinished { exit_code: Some(0) },
            AgentEvent::TitleChanged {
                title: "build".to_owned(),
            },
            AgentEvent::Bell,
            AgentEvent::ResourceSpawned {
                kind: phux_protocol::ids::ResourceKind::Terminal,
                parent: None,
            },
            AgentEvent::ResourceClosed {
                exit_status: Some(0),
            },
            AgentEvent::Dirty,
            AgentEvent::Idle,
            AgentEvent::Asked {
                id: "q1".to_owned(),
                question: "?".to_owned(),
                suggestions: Vec::new(),
                elapsed_seconds: None,
            },
            AgentEvent::CwdChanged {
                cwd: "/tmp".to_owned(),
            },
            AgentEvent::TerminalControl {
                lifecycle: phux_protocol::wire::frame::ResourceLifecycle::Exited,
                exit_status: Some(3),
                input_holder: None,
                action: phux_protocol::wire::frame::ControlAction::Exited,
                actor: None,
            },
            AgentEvent::JournalGap {
                first_missing: 4,
                last_missing: 9,
            },
            AgentEvent::SourceGap { dropped: 2 },
            AgentEvent::Unknown {
                tag: 0xFE,
                body: Vec::new(),
            },
        ]
    }

    /// The gate can only be validated at argv-parse time if the vocabulary
    /// really is every name the stream prints. A name the stream emits but
    /// the list omits would be a line a consumer can see and cannot wait for.
    #[test]
    fn event_vocabulary_covers_every_rendered_name() {
        for event in every_event() {
            let kind = watch_event_kind(&event);
            assert!(
                WATCH_EVENT_NAMES.contains(&kind),
                "`{kind}` is printed by watch but is not in the --until vocabulary"
            );
        }
        assert!(
            WATCH_EVENT_NAMES.contains(&AGENT_STATE_EVENT),
            "the agent_state line must be waitable too"
        );
    }

    /// The list is the frozen CLI surface (ADR-0071), printed verbatim in the
    /// `--until` help and in the usage error. Sorted and duplicate-free so a
    /// later addition lands in one obvious place.
    #[test]
    fn event_vocabulary_is_sorted_and_free_of_duplicates() {
        let mut sorted = WATCH_EVENT_NAMES.to_vec();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.as_slice(), WATCH_EVENT_NAMES);
    }

    /// The name `--until` matches is byte-for-byte the `event` value on the
    /// `--json` line.
    #[test]
    fn until_matches_exactly_the_name_the_json_line_carries() {
        for event in every_event() {
            let ev = WatchEvent {
                terminal: None,
                event,
                stamp: None,
            };
            let name = watch_item_event(&WatchItem::Event(ev.clone()));
            let line = watch_event_json(&ev).unwrap();
            let parsed: serde_json::Value = serde_json::from_str(&line).unwrap();
            assert_eq!(parsed["event"], name, "gate name vs printed name: {line}");
        }
    }

    #[test]
    fn an_agent_state_item_is_named_agent_state() {
        let item = WatchItem::AgentState(AgentStateUpdate {
            terminal: None,
            record: Some(record("reviewer", None, AgentMetaState::Blocked)),
            previous: None,
        });
        assert_eq!(watch_item_event(&item), "agent_state");
    }

    /// `unknown` stays a waitable name; the journal-era events (ADR-0123) have
    /// their own.
    #[test]
    fn journal_era_events_are_named_and_unknown_tags_stay_unknown() {
        assert_eq!(
            watch_event_kind(&AgentEvent::Unknown {
                tag: 0xFE,
                body: Vec::new()
            }),
            "unknown"
        );
        assert_eq!(
            watch_event_kind(&AgentEvent::CwdChanged {
                cwd: "/tmp".to_owned()
            }),
            "cwd_changed"
        );
        assert_eq!(
            watch_event_kind(&AgentEvent::SourceGap { dropped: 1 }),
            "source_gap"
        );
    }

    /// A journaled event's line carries `seq`, `ts_ms`, and the `actor`; an
    /// unjournaled one carries none of them (keys absent, not null).
    #[test]
    fn a_stamped_line_carries_seq_ts_and_actor() {
        use phux_protocol::ids::ClientId;
        use phux_protocol::wire::frame::{ActorRef, EventStamp};

        let actor = ActorRef::new(ClientId::new(5)).with_client_name(Some("phux-cli/1".to_owned()));
        let ev = WatchEvent {
            terminal: Some(pane("@3")),
            event: AgentEvent::CwdChanged {
                cwd: "/repo".to_owned(),
            },
            stamp: Some(EventStamp::new(42, 1_700).with_actor(Some(actor))),
        };
        let v: serde_json::Value = serde_json::from_str(&watch_event_json(&ev).unwrap()).unwrap();
        assert_eq!(v["event"], "cwd_changed");
        assert_eq!(v["cwd"], "/repo");
        assert_eq!(v["seq"], 42);
        assert_eq!(v["ts_ms"], 1_700);
        assert_eq!(v["actor"]["client"], 5);
        assert_eq!(v["actor"]["client_name"], "phux-cli/1");

        let plain = json_of(AgentEvent::Bell, Some("@3"));
        assert!(plain.get("seq").is_none() && plain.get("actor").is_none());

        let control = json_of(
            AgentEvent::TerminalControl {
                lifecycle: phux_protocol::wire::frame::ResourceLifecycle::Exited,
                exit_status: Some(7),
                input_holder: None,
                action: phux_protocol::wire::frame::ControlAction::Exited,
                actor: Some(ClientId::new(2)),
            },
            Some("@3"),
        );
        assert_eq!(control["event"], "terminal_control");
        assert_eq!(control["action"], "exited");
        assert_eq!(control["lifecycle"], "exited");
        assert_eq!(control["exit_status"], 7);
        assert_eq!(control["actor_client"], 2);

        let gap = json_of(
            AgentEvent::JournalGap {
                first_missing: 4,
                last_missing: 9,
            },
            None,
        );
        assert_eq!(gap["first_missing"], 4);
        assert_eq!(gap["last_missing"], 9);
    }

    #[test]
    fn a_malformed_after_cursor_is_a_usage_error_before_any_connection() {
        let code = super::run_watch(super::WatchArgs {
            session: Some("@1"),
            until: &[],
            timeout: Some(1),
            after: Some("nope"),
            json: true,
            socket: Some(std::path::PathBuf::from("/nonexistent/phux.sock")),
        });
        assert_eq!(
            code,
            std::process::ExitCode::from(crate::exit_codes::EXIT_USAGE)
        );
    }

    /// The timeout and EOF diagnostics name what the caller was waiting for;
    /// with no gate there is nothing to name and the clause disappears rather
    /// than reading "waiting for ".
    #[test]
    fn the_gate_description_lists_the_alternatives_and_is_empty_without_one() {
        assert_eq!(describe_gate(&[]), "");
        assert_eq!(
            describe_gate(&["asked".to_owned(), "idle".to_owned()]),
            " waiting for asked or idle"
        );
    }

    /// ADR-0071 point 6 froze these gate names; the list must not drift, and
    /// every one of them is still in the accepted vocabulary.
    #[test]
    fn the_frozen_gate_names_are_adr_0071_point_6() {
        assert_eq!(
            super::FROZEN_GATE_NAMES,
            [
                "agent_state",
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
            ]
        );
        for name in super::FROZEN_GATE_NAMES {
            assert!(WATCH_EVENT_NAMES.contains(name), "{name}");
        }
    }

    /// `--until unknown` still matches the events named after 1.0 (they
    /// printed as `unknown` before), while their own names gate on exactly
    /// one kind and a frozen name never matches through `unknown`.
    #[test]
    fn until_unknown_keeps_matching_events_named_after_1_0() {
        use super::gate_matches;

        let item = |event: AgentEvent| {
            WatchItem::Event(WatchEvent {
                terminal: None,
                event,
                stamp: None,
            })
        };
        let named_later = [
            AgentEvent::CwdChanged {
                cwd: "/tmp".to_owned(),
            },
            AgentEvent::TerminalControl {
                lifecycle: phux_protocol::wire::frame::ResourceLifecycle::Exited,
                exit_status: Some(0),
                input_holder: None,
                action: phux_protocol::wire::frame::ControlAction::Exited,
                actor: None,
            },
            AgentEvent::JournalGap {
                first_missing: 1,
                last_missing: 2,
            },
            AgentEvent::SourceGap { dropped: 1 },
        ];
        for event in named_later {
            let item = item(event);
            let name = watch_item_event(&item);
            assert!(gate_matches("unknown", &item), "`unknown` matches {name}");
            assert!(gate_matches(name, &item), "{name} matches itself");
            assert!(!gate_matches("bell", &item));
        }
        let bell = item(AgentEvent::Bell);
        assert!(gate_matches("bell", &bell));
        assert!(
            !gate_matches("unknown", &bell),
            "a frozen name never matches through `unknown`"
        );
        let tag = item(AgentEvent::Unknown {
            tag: 0xFE,
            body: Vec::new(),
        });
        assert!(gate_matches("unknown", &tag));
    }
}
