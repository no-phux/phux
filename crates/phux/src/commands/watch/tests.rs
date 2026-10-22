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
