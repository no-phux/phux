use super::*;

fn state_event(terminal: &str, from: Option<&str>, to: &str) -> Event {
    Event::State {
        terminal: terminal.to_owned(),
        kind: "claude".to_owned(),
        name: "fox".to_owned(),
        from: from.map(str::to_owned),
        to: to.to_owned(),
    }
}

/// Drive `terminal` through `states` one second apart, starting at 1 s.
fn walk(state: &mut State, terminal: &str, states: &[&str]) -> i64 {
    let mut from = None;
    let mut now = 0;
    for (i, to) in states.iter().enumerate() {
        now = (i64::try_from(i).unwrap() + 1) * 1000;
        state.apply(state_event(terminal, from, to), now);
        from = Some(*to);
    }
    now
}

fn published(state: &mut State, now_ms: i64) -> serde_json::Value {
    serde_json::from_slice(&state.bounded_document(now_ms)).unwrap()
}

#[test]
fn done_and_working_to_idle_each_finish_one_task() {
    let mut state = State::default();
    walk(
        &mut state,
        "1",
        &["idle", "working", "done", "idle", "working", "idle"],
    );
    assert_eq!(state.agents["1"].turns, 2, "working→done→idle is one task");

    let mut state = State::default();
    walk(&mut state, "2", &["working", "blocked", "working", "idle"]);
    assert_eq!(
        state.agents["2"].turns, 1,
        "a permission stop is not a task"
    );

    let mut state = State::default();
    state.apply(state_event("3", None, "done"), 1);
    assert_eq!(
        state.agents["3"].turns, 1,
        "a first sighting in done counts"
    );
}

#[test]
fn working_time_adds_up_closed_and_open_intervals() {
    let mut state = State::default();
    // working 2..3 (1 s), blocked, working 4..6 and still open.
    walk(&mut state, "1", &["idle", "working", "blocked", "working"]);
    assert_eq!(state.agents["1"].working_ms, 1000);
    assert_eq!(state.agents["1"].working_s(4000), 1, "just re-entered");
    assert_eq!(state.agents["1"].working_s(6500), 3, "open interval counts");
    state.apply(state_event("1", Some("working"), "done"), 7000);
    assert_eq!(state.agents["1"].working_s(60_000), 4, "closed at done");
}

#[test]
fn a_late_run_counts_its_task_but_not_its_state() {
    let mut state = State::default();
    state.apply(state_event("1", Some("idle"), "working"), 5000);
    // An older working→done run took the lock after the newer one.
    state.apply(state_event("1", Some("working"), "done"), 4000);
    let agent = &state.agents["1"];
    assert_eq!((agent.turns, agent.state.as_str()), (1, "working"));
    assert_eq!((agent.since_ms, agent.updated_ms), (5000, 5000));
}

#[test]
fn exit_prunes_and_a_withdrawn_stranger_is_ignored() {
    let mut state = State::default();
    walk(&mut state, "1", &["working", "done"]);
    walk(&mut state, "2", &["working"]);
    state.apply(
        Event::Exit {
            terminal: "1".to_owned(),
        },
        9000,
    );
    assert!(!state.agents.contains_key("1"));
    let withdrawn = Event::State {
        terminal: "9".to_owned(),
        kind: String::new(),
        name: String::new(),
        from: None,
        to: "unknown".to_owned(),
    };
    state.apply(withdrawn, 9000);
    assert!(!state.agents.contains_key("9"));
    // A withdrawal keeps a known agent's identity and closes its timer.
    let withdrawn = Event::State {
        terminal: "2".to_owned(),
        kind: String::new(),
        name: String::new(),
        from: Some("working".to_owned()),
        to: "unknown".to_owned(),
    };
    state.apply(withdrawn, 9000);
    let agent = &state.agents["2"];
    assert_eq!((agent.kind.as_str(), agent.working_ms), ("claude", 8000));
}

#[test]
fn a_new_kind_in_the_same_pane_starts_over() {
    let mut state = State::default();
    walk(&mut state, "1", &["working", "done"]);
    let codex = Event::State {
        terminal: "1".to_owned(),
        kind: "codex".to_owned(),
        name: String::new(),
        from: None,
        to: "idle".to_owned(),
    };
    state.apply(codex, 9000);
    let agent = &state.agents["1"];
    assert_eq!((agent.kind.as_str(), agent.turns), ("codex", 0));
    assert_eq!(agent.first_seen_ms, 9000);
}

#[test]
fn the_map_keeps_the_newest_agents_and_fits_the_limit() {
    let mut state = State::default();
    for i in 0..300_i64 {
        let event = Event::State {
            terminal: i.to_string(),
            kind: "claude".to_owned(),
            name: "n".repeat(200),
            from: None,
            to: "idle".to_owned(),
        };
        state.apply(event, i + 1);
    }
    let bytes = state.bounded_document(1000);
    assert!(bytes.len() <= MAX_DOCUMENT_BYTES, "{} bytes", bytes.len());
    assert!(state.agents.len() <= MAX_AGENTS);
    assert!(state.agents.contains_key("299"), "the newest stays");
    assert!(!state.agents.contains_key("0"), "the oldest goes");
    assert_eq!(state.agents["299"].name.chars().count(), MAX_NAME_CHARS);
}

#[test]
fn the_published_document_has_the_contract_shape() {
    let mut state = State::default();
    walk(&mut state, "7", &["idle", "working", "done"]);
    assert_eq!(
        published(&mut state, 3000),
        serde_json::json!({"v": 1, "agents": {"7": {
            "kind": "claude", "name": "fox", "turns": 1, "working_s": 1,
            "first_seen_ms": 1000, "updated_ms": 3000
        }}})
    );
    let compact = String::from_utf8(state.bounded_document(3000)).unwrap();
    assert!(!compact.contains(' '), "compact JSON: {compact}");
}

#[test]
fn the_hook_environment_becomes_an_event() {
    let env = |pairs: &'static [(&'static str, &'static str)]| {
        move |name: &str| {
            pairs
                .iter()
                .find(|(key, _)| *key == name)
                .map(|(_, value)| (*value).to_owned())
        }
    };
    assert_eq!(
        Event::from_env(env(&[
            ("PHUX_EVENT", "agent-state-changed"),
            ("PHUX_TERMINAL_ID", "4"),
            ("PHUX_AGENT_KIND", "claude"),
            ("PHUX_AGENT_NAME", "fox"),
            ("PHUX_TO", "working"),
        ])),
        Event::State {
            terminal: "4".to_owned(),
            kind: "claude".to_owned(),
            name: "fox".to_owned(),
            from: None,
            to: "working".to_owned(),
        }
    );
    assert_eq!(
        Event::from_env(env(&[
            ("PHUX_EVENT", "pane-exit"),
            ("PHUX_TERMINAL_ID", "4")
        ])),
        Event::Exit {
            terminal: "4".to_owned()
        }
    );
    assert_eq!(
        Event::from_env(env(&[("PHUX_EVENT", "client-attached")])),
        Event::None
    );
}

#[test]
fn concurrent_runs_never_lose_a_task() {
    let dir = tempfile::tempdir().unwrap();
    let socket = Path::new("/tmp/phux test.sock");
    let threads: Vec<_> = (0..16_i64)
        .map(|i| {
            let dir = dir.path().to_owned();
            std::thread::spawn(move || {
                let (store, mut state) = Store::open(&dir, socket).unwrap();
                state.apply(state_event("1", Some("working"), "done"), i);
                std::thread::sleep(std::time::Duration::from_millis(2));
                store.save(&state).unwrap();
            })
        })
        .collect();
    for thread in threads {
        thread.join().unwrap();
    }
    let (_store, state) = Store::open(dir.path(), socket).unwrap();
    assert_eq!(state.agents["1"].turns, 16, "every run saw the last save");
    assert!(dir.path().join("_tmp_phux_test_sock.json").is_file());
}
