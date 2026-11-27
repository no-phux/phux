//! The Action Button's dictation: its words reach the named session's
//! terminal as one submission, and never any other terminal.
use super::*;
use phux_client_runtime::control::Topology;
use phux_protocol::ids::{ResourceId, SessionId, WindowId};
use phux_protocol::input::InputEvent;
use phux_protocol::input::key::PhysicalKey;
use phux_protocol::wire::frame::{Command, CommandResult, CommandValue};
use phux_protocol::wire::info::{ResourceInfo, SessionInfo, SessionSnapshot};

/// A native phone showing a terminal of session `shown_in`, beside an
/// `other` session with a pane of its own.
fn phone(shown_in: &str, other: &str) -> (std::sync::Arc<Client>, Runtime<Desk>) {
    let (client, mut rt) = crate::test_support::input_fixture_showing("❯ ");
    rt.update(|d, tx| {
        d.phone = Some(Phone::default());
        d.native = Some(crate::mobile::Platform::test());
        tx.instant();
    });
    let terminal = rt.state.connection.shown.as_ref().unwrap().resource.clone();
    let window = WindowId::new(1);
    let snapshot = SessionSnapshot::new(SessionId::new(1), window, terminal.clone())
        .with_sessions(vec![
            SessionInfo::new(SessionId::new(1), shown_in),
            SessionInfo::new(SessionId::new(2), other),
        ])
        .with_resources(vec![
            ResourceInfo::new(terminal, window, 80, 24).with_session(Some(SessionId::new(1))),
            ResourceInfo::new(ResourceId::local(90), window, 80, 24)
                .with_session(Some(SessionId::new(2))),
        ]);
    let request_id = client.refresh_topology().unwrap();
    client
        .feed(FrameKind::CommandResult {
            request_id,
            result: CommandResult::OkWith(CommandValue::State(snapshot.clone())),
        })
        .unwrap();
    rt.update(|d, _| d.connection.topology = Some(Topology::from_snapshot(&snapshot)));
    settle(&mut rt);
    let _ = client.take_outbound();
    (client, rt)
}

/// Every input submission sent, by terminal.
fn submissions(client: &Client) -> Vec<(ResourceId, Vec<InputEvent>)> {
    client
        .take_outbound()
        .into_iter()
        .filter_map(|bytes| match FrameKind::decode(&bytes).unwrap().0 {
            FrameKind::Command {
                command:
                    Command::ApplyInput {
                        terminal_id,
                        events,
                        ..
                    },
                ..
            } => Some((terminal_id, events)),
            FrameKind::InputKey { terminal_id, .. } | FrameKind::InputPaste { terminal_id, .. } => {
                Some((terminal_id, Vec::new()))
            }
            _ => None,
        })
        .collect()
}

#[test]
fn dictation_to_the_shown_session_is_sent_as_one_line() {
    let (client, mut rt) = phone("commander", "notes");
    crate::mobile::dictate(&mut rt, " commander ", " run the tests \n");
    settle(&mut rt);
    let sent = submissions(&client);
    let [(terminal, events)] = sent.as_slice() else {
        panic!("expected one submission, got {sent:?}")
    };
    assert_eq!(
        terminal,
        &rt.state.connection.shown.as_ref().unwrap().resource
    );
    let [InputEvent::Paste(paste), InputEvent::Key(enter)] = events.as_slice() else {
        panic!("expected one paste and Enter")
    };
    assert_eq!(paste.data, b"run the tests");
    assert_eq!(enter.key, PhysicalKey::Enter);
    assert!(rt.state.phone.as_ref().unwrap().dictation.is_none());
}

#[test]
fn dictation_never_types_into_another_sessions_terminal() {
    let (client, mut rt) = phone("notes", "commander");
    let shown = rt.state.connection.shown.as_ref().unwrap().resource.clone();
    crate::mobile::dictate(&mut rt, "commander", "deploy it");
    settle(&mut rt);
    assert!(
        submissions(&client).iter().all(|(t, _)| t != &shown),
        "the words reached the wrong session's terminal"
    );
    // It waits for commander to show instead.
    assert!(rt.state.phone.as_ref().unwrap().dictation.is_some());
}

#[test]
fn dictation_to_a_missing_session_is_refused_aloud() {
    let (client, mut rt) = phone("notes", "work");
    crate::mobile::dictate(&mut rt, "commander", "hello");
    settle(&mut rt);
    assert!(submissions(&client).is_empty());
    assert!(crate::test_support::notice(&rt.state).contains("no session \"commander\""));
    assert!(rt.state.phone.as_ref().unwrap().dictation.is_none());
}

#[test]
fn dictation_reaches_its_session_from_under_the_sessions_page() {
    let (client, mut rt) = phone("commander", "notes");
    rt.update(|d, tx| {
        d.open_phone_sessions();
        tx.instant();
    });
    settle(&mut rt);
    assert!(
        rt.state.composer_owner().is_none(),
        "the page covers the terminal"
    );
    crate::mobile::dictate(&mut rt, "commander", "status");
    settle(&mut rt);
    let sent = submissions(&client);
    assert_eq!(sent.len(), 1, "expected one submission, got {sent:?}");
    assert!(rt.state.phone.as_ref().unwrap().page.is_none());
}

#[test]
fn dictation_of_several_lines_asks_before_it_runs_them() {
    let (client, mut rt) = phone("commander", "notes");
    crate::mobile::dictate(&mut rt, "commander", "rm -rf build\nmake");
    settle(&mut rt);
    assert!(submissions(&client).is_empty());
    assert!(matches!(
        rt.state.pending,
        Some(crate::desk::Pending::SendDraft(_))
    ));
}

#[test]
fn dictation_joins_a_draft_already_there_and_sends_nothing() {
    let (client, mut rt) = phone("commander", "notes");
    let owner = rt.state.composer_owner().unwrap();
    rt.update(|d, _| {
        let draft = d.phone.as_mut().unwrap().composer.draft_entry(&owner);
        draft.edit = rune::TextEdit::new("git status");
    });
    crate::mobile::dictate(&mut rt, "commander", "and then push");
    settle(&mut rt);
    assert!(submissions(&client).is_empty());
    let draft = rt.state.composer_draft(&owner).unwrap();
    assert_eq!(draft.edit.text, "git status and then push");
}
