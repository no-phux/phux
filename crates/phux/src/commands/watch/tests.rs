//! Unit tests for `phux watch`.

use phux_client::agent_meta::{AgentMetaState, AgentRecord};
use phux_client::watch::{AgentStateUpdate, WatchEvent, WatchItem};
use phux_protocol::wire::frame::AgentEvent;

use super::render::{
    agent_state_detail, agent_state_json, watch_event_json, watch_event_kind, watch_item_event,
};
use super::{AGENT_STATE_EVENT, WATCH_EVENT_NAMES, describe_gate};

fn pane(selector: &str) -> phux_protocol::ids::ResourceId {
    phux_protocol::ids::ResourceId::local(selector.trim_start_matches('@').parse().unwrap())
}

/// An event on no terminal, outside the journal.
fn unstamped(event: AgentEvent) -> WatchEvent {
    WatchEvent {
        terminal: None,
        event,
        stamp: None,
    }
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

/// Each event's line carries exactly its documented keys: a missing
/// terminal is absent, a missing exit status is present and null.
#[test]
fn watch_json_lines_carry_each_events_fields() {
    use serde_json::json;
    for (event, terminal, expected) in [
        (
            AgentEvent::TitleChanged {
                title: "build".to_owned(),
            },
            Some("@3"),
            json!({"event": "title_changed", "title": "build", "terminal": "@3"}),
        ),
        (AgentEvent::Bell, None, json!({"event": "bell"})),
        (
            AgentEvent::ResourceClosed {
                exit_status: Some(0),
            },
            Some("@1"),
            json!({"event": "pane_closed", "exit_status": 0, "terminal": "@1"}),
        ),
        // A signal-killed pane reports null exit_status (present, not absent).
        (
            AgentEvent::ResourceClosed { exit_status: None },
            Some("@1"),
            json!({"event": "pane_closed", "exit_status": null, "terminal": "@1"}),
        ),
        // The documented exit-code gap: the reference server emits None.
        (
            AgentEvent::CommandFinished { exit_code: None },
            None,
            json!({"event": "command_finished", "exit_code": null}),
        ),
        (
            AgentEvent::Asked {
                id: "q1".to_owned(),
                question: "Deploy to prod?".to_owned(),
                suggestions: vec!["Yes".to_owned(), "No".to_owned(), "Hold".to_owned()],
                elapsed_seconds: None,
            },
            Some("@9"),
            json!({
                "event": "asked", "terminal": "@9", "id": "q1", "question": "Deploy to prod?",
                "suggestions": ["Yes", "No", "Hold"], "elapsed_seconds": null,
            }),
        ),
    ] {
        assert_eq!(json_of(event, terminal), expected);
    }
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

/// The detector never writes `attention`; the line carries the derived
/// value. `from` appears only once a prior record is known, and a tombstone
/// keeps the identity with a present-and-null `state`, so a consumer keyed on
/// `state` sees the record go away.
#[test]
fn agent_state_json_carries_identity_state_attention_and_transition() {
    use serde_json::json;
    let blocked = || Some(record("reviewer", Some("claude"), AgentMetaState::Blocked));
    let update = |record, previous| AgentStateUpdate {
        terminal: None,
        record,
        previous,
    };
    for (update, expected) in [
        (
            update(blocked(), None),
            json!({"event": "agent_state", "terminal": "@7", "name": "reviewer",
                "kind": "claude", "state": "blocked", "attention": "high"}),
        ),
        (
            update(
                Some(record("reviewer", None, AgentMetaState::Blocked)),
                Some(record("reviewer", None, AgentMetaState::Working)),
            ),
            json!({"event": "agent_state", "terminal": "@7", "name": "reviewer",
                "state": "blocked", "attention": "high", "from": "working"}),
        ),
        (
            update(None, blocked()),
            json!({"event": "agent_state", "terminal": "@7", "name": "reviewer",
                "kind": "claude", "state": null, "from": "blocked"}),
        ),
        (
            update(None, None),
            json!({"event": "agent_state", "terminal": "@7", "state": null}),
        ),
    ] {
        assert_eq!(agent_json(&update, Some("@7")), expected);
    }
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
        let ev = unstamped(event);
        let name = watch_item_event(&WatchItem::Event(ev.clone()));
        let line = watch_event_json(&ev).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert_eq!(parsed["event"], name, "gate name vs printed name: {line}");
    }
}

/// `unknown` stays a waitable name; the journal-era events (ADR-0123) have
/// their own.
#[test]
fn items_and_journal_era_events_are_named_and_unknown_tags_stay_unknown() {
    let item = WatchItem::AgentState(AgentStateUpdate {
        terminal: None,
        record: Some(record("reviewer", None, AgentMetaState::Blocked)),
        previous: None,
    });
    assert_eq!(watch_item_event(&item), "agent_state");
    let names: Vec<_> = every_event().iter().map(watch_event_kind).collect();
    assert_eq!(
        names,
        [
            "command_started",
            "command_finished",
            "title_changed",
            "bell",
            "pane_spawned",
            "pane_closed",
            "dirty",
            "idle",
            "asked",
            "cwd_changed",
            "terminal_control",
            "journal_gap",
            "source_gap",
            "unknown",
        ]
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

    let item = |event| WatchItem::Event(unstamped(event));
    let named_later = every_event().into_iter().filter(|event| {
        matches!(
            event,
            AgentEvent::CwdChanged { .. }
                | AgentEvent::TerminalControl { .. }
                | AgentEvent::JournalGap { .. }
                | AgentEvent::SourceGap { .. }
        )
    });
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
