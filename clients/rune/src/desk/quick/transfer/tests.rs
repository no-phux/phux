use super::*;
use crate::desk::{Shown, quick::Panel};
use phux_client_runtime::control::Event;
use phux_protocol::ids::Revision;
use phux_protocol::wire::frame::{CommandResult, CommandValue, Committed, ErrorCode, Refusal};
use rune::{Env, Runtime};
use std::sync::{
    Arc,
    mpsc::{self, Receiver},
};

mod fixture;
mod placement;
use fixture::*;

fn refused(desk: &mut Desk) -> String {
    let (send, receive) = mpsc::channel();
    desk.adopt_quick(
        terminal(),
        Box::new(move |result| send.send(result).unwrap()),
    );
    let error = receive
        .try_recv()
        .expect("failure answered immediately")
        .unwrap_err();
    assert!(desk.quick_move.is_none());
    assert!(!desk.splits_waiting());
    error
}

#[test]
fn remote_disconnected_and_unselected_main_windows_answer_the_panel() {
    let (_, mut desk) = connected();
    desk.machines.current = Some("remote".to_owned());
    assert!(refused(&mut desk).contains("another machine"));
    desk.machines.current = None;
    desk.client = None;
    assert!(refused(&mut desk).contains("Connect the main window"));
    let (_, mut desk) = connected();
    desk.topology.as_mut().unwrap().sessions.clear();
    assert!(refused(&mut desk).contains("Open a session"));
}

#[test]
fn a_stale_connection_and_the_quick_session_itself_are_not_destinations() {
    let (client, mut desk) = connected();
    client.attach_session(AttachTarget::ById(SessionId::new(2)));
    assert!(refused(&mut desk).contains("Open a session"));
    client.with_control(|control| control.connection_lost(Some("offline".into())));
    assert_eq!(
        desk.status,
        Status::Attached,
        "the window has not polled yet"
    );
    assert!(refused(&mut desk).contains("Connect the main window"));
}

#[test]
fn a_submission_refusal_is_not_a_move() {
    let (client, mut desk) = connected();
    let resource = client
        .organization()
        .resource_by_live(&terminal())
        .unwrap()
        .key;
    for _ in 0..phux_client_core::org_ops::ORG_JOURNAL_MAX_OPS {
        client.move_resource(resource, session(1)).unwrap();
    }
    assert!(refused(&mut desk).contains("too many organization operations"));
}

#[test]
fn queuing_and_unrelated_receipts_do_not_place_or_complete_the_move() {
    let (client, mut desk) = connected();
    let (_, operation, result) = begin(&mut desk, &client);
    assert!(result.try_recv().is_err(), "Ok from submit is not success");
    assert!(!desk.splits_waiting());
    assert!(desk.quick_move.is_some());
    let other = OperationId::new([99; 16]).unwrap();
    assert!(!desk.quick_move_outcome(&client, other, &OrgOutcome::Accepted));
    assert!(result.try_recv().is_err());
    assert!(desk.owns_quick_move(operation));
    let error = {
        let (send, result) = mpsc::channel();
        desk.adopt_quick(
            terminal(),
            Box::new(move |outcome| send.send(outcome).unwrap()),
        );
        result.try_recv().unwrap().unwrap_err()
    };
    assert!(error.contains("already waiting"));
    assert!(
        desk.owns_quick_move(operation),
        "the first request is retained"
    );
}

#[test]
fn a_server_refusal_reports_the_error_without_placing_or_refilling() {
    let (client, mut desk) = connected();
    let (request, _, result) = begin(&mut desk, &client);
    reply(
        &mut desk,
        &client,
        request,
        CommandResult::Error {
            code: ErrorCode::PermissionDenied,
            message: "move refused".to_owned(),
        },
    );
    assert!(
        result
            .try_recv()
            .unwrap()
            .unwrap_err()
            .contains("move refused")
    );
    assert!(!desk.splits_waiting());
    assert!(desk.pending_spawns.is_empty());
    assert!(desk.quick_move.is_none());
}

#[test]
fn an_organization_conflict_keeps_the_terminal_out_of_the_layout() {
    let (client, mut desk) = connected();
    let (request, _, result) = begin(&mut desk, &client);
    reply(
        &mut desk,
        &client,
        request,
        CommandResult::Refused(Refusal::Conflict {
            current: Revision::new(2),
        }),
    );
    assert!(result.try_recv().unwrap().unwrap_err().contains("Conflict"));
    assert!(!desk.splits_waiting());
    assert!(desk.quick_move.is_none());
}

#[test]
fn only_the_successful_server_receipt_queues_the_tab_and_completes_once() {
    let (client, mut desk) = connected();
    let (request, operation, result) = begin(&mut desk, &client);
    // A different selection while the reply travels cannot redirect placement.
    client.attach_session(AttachTarget::ById(SessionId::new(2)));
    reply(
        &mut desk,
        &client,
        request,
        CommandResult::OkWith(CommandValue::Committed(Box::new(Committed::new(
            operation,
            Revision::new(2),
        )))),
    );
    assert_eq!(result.try_recv().unwrap(), Ok(()));
    assert_eq!(client.selected_session(), Some(1));
    assert_eq!(desk.unplaced_splits.len(), 1);
    assert_eq!(desk.unplaced_splits[0].terminal, terminal());
    assert!(!desk.quick_move_outcome(&client, operation, &OrgOutcome::Accepted));
    assert!(result.try_recv().is_err(), "no duplicate completion");
}

#[test]
fn an_uncertain_move_is_not_retried_or_presented_as_success() {
    let (client, mut desk) = connected();
    let (_, operation, result) = begin(&mut desk, &client);
    assert!(desk.quick_move_outcome(
        &client,
        operation,
        &OrgOutcome::Unknown {
            reason: "receipt expired".to_owned(),
        }
    ));
    let error = result.try_recv().unwrap().unwrap_err();
    assert!(error.contains("receipt expired") && error.contains("Check"));
    assert!(!desk.splits_waiting());
    assert!(desk.quick_move.is_none());
}

#[test]
fn replacing_the_connection_answers_the_move_and_keeps_panel_registration_state() {
    let (client, mut desk) = connected();
    desk.quick_fixture(true);
    let (_, _, result) = begin(&mut desk, &client);
    let mut next = Desk::settings_test_fixture();
    desk.carry_into(&mut next);
    assert!(
        result
            .try_recv()
            .unwrap()
            .unwrap_err()
            .contains("connection was replaced")
    );
    assert!(next.quick_chord.available());
    assert!(next.quick_chord.on);
    assert!(desk.quick_move.is_none());
}

#[test]
fn the_panel_keeps_its_terminal_and_renders_a_visible_error_without_native_windows() {
    // Handles belong to apps that never run: no desktop or global chord.
    let panel = rune::App::new(crate::app_spec((320.0, 160.0), (320.0, 160.0)));
    let main = rune::App::new(crate::app_spec((320.0, 160.0), (320.0, 160.0)));
    let mut desk = Desk::settings_test_fixture();
    desk.quick = Some(Panel {
        me: panel.handle(),
        main: main.handle(),
        dialed: true,
        moving: true,
    });
    desk.shown = Some(Shown {
        resource: terminal(),
        view: None,
    });
    desk.quick_move_finished(Err("move refused".to_owned()));
    assert_eq!(desk.shown.as_ref().unwrap().resource, terminal());
    assert!(desk.pending_spawns.is_empty(), "failure never refills");
    assert!(
        !desk.quick.as_ref().unwrap().moving,
        "the panel can try again"
    );
    assert_eq!(desk.notice.as_ref().unwrap().kind, NoticeKind::Error);
    let mut runtime = Runtime::new(desk, crate::ui::view, (960.0, 440.0), Env::new(true, 1.0));
    runtime.frame(0.0);
    assert!(runtime.find("toast").is_some());
    assert!(
        runtime
            .elements()
            .iter()
            .any(|el| el.text.as_deref() == Some("move refused"))
    );
}

#[test]
fn a_success_before_a_new_connection_waits_for_identity_and_never_hides_on_failure() {
    let (client, mut desk) = connected();
    let mut tx = rune::Tx::default();
    desk.observe(&client, client.take_activity(), &mut tx);
    let (_, operation, result) = begin(&mut desk, &client);
    let mut observation = client.take_activity();
    observation.connection_epoch += 1;
    observation.status = Status::Connecting;
    observation.events = vec![
        Event::OrganizationOperation {
            operation,
            outcome: OrgOutcome::Accepted,
        },
        Event::ConnectionOpened {
            connection_epoch: observation.connection_epoch,
        },
    ];
    desk.observe(&client, observation.clone(), &mut tx);
    assert!(
        result.try_recv().is_err(),
        "an unverified old success cannot hide the panel"
    );
    assert!(!desk.splits_waiting());
    observation.status = Status::Failed;
    observation.events.clear();
    desk.observe(&client, observation, &mut tx);
    assert!(
        result
            .try_recv()
            .unwrap()
            .unwrap_err()
            .contains("could not be verified")
    );
    assert!(!desk.splits_waiting());
    assert!(desk.quick_move.is_none());
}

#[test]
fn an_old_failure_settles_even_while_the_new_connection_is_unverified() {
    let (client, mut desk) = connected();
    let mut tx = rune::Tx::default();
    desk.observe(&client, client.take_activity(), &mut tx);
    let (_, operation, result) = begin(&mut desk, &client);
    let mut observation = client.take_activity();
    observation.connection_epoch += 1;
    observation.status = Status::Connecting;
    observation.events = vec![
        Event::OrganizationOperation {
            operation,
            outcome: OrgOutcome::NotSent {
                reason: "not sent".into(),
            },
        },
        Event::ConnectionOpened {
            connection_epoch: observation.connection_epoch,
        },
    ];
    desk.observe(&client, observation, &mut tx);
    assert!(result.try_recv().unwrap().unwrap_err().contains("not sent"));
    assert!(desk.quick_move.is_none());
    assert!(!desk.splits_waiting());
}

#[test]
fn a_successful_panel_completion_refills_only_after_the_move_receipt() {
    let (client, mut desk) = connected();
    client.attach_session(AttachTarget::ById(SessionId::new(2)));
    let panel = rune::App::new(crate::app_spec((320.0, 160.0), (320.0, 160.0)));
    let main = rune::App::new(crate::app_spec((320.0, 160.0), (320.0, 160.0)));
    desk.quick = Some(Panel {
        me: panel.handle(),
        main: main.handle(),
        dialed: true,
        moving: true,
    });
    assert!(desk.pending_spawns.is_empty());
    // Apps never run: hide/open only queue requests, never touch the desktop.
    desk.quick_move_finished(Ok(()));
    assert_eq!(desk.pending_spawns.len(), 1);
    assert!(!desk.quick.as_ref().unwrap().moving);
}
