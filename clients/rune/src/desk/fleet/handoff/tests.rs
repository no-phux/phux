//! Actual destination reconciliation keeps the watched execution, not a reused
//! session name or a default pane chosen by a new runtime.

use super::*;
use crate::activity::Activities;
use crate::creature::Stats;
use crate::test_support::terminal::Replica;
use phux_wire_fixtures::bootstrap_frames;

fn terminal(id: u32) -> ResourceId { ResourceId::local(id) }

fn install(desk: &mut Desk, watched: &Replica, target: u32) {
    let epoch = watched.client.connection_epoch();
    let origin = WatchScope::capture(&watched.client, epoch).unwrap();
    let watch = Watch {
        client: watched.client.clone(), topology: watched.client.topology(), scope: None,
        activities: Activities::default(), status: watched.client.status(), stats: Stats::default(), asked: false,
    };
    desk.connection.fleet_handoff = Some(Handoff {
        watch, origin, terminal: terminal(target), choice: desk.connection.view_choice, installed: false,
    });
}

#[test]
fn exact_agent_handoff_selects_its_destination_source_not_the_default_pane() {
    let watched = Replica::new(80, 24, 2);
    let destination = Replica::new(80, 24, 2);
    let mut desk = destination.window();
    install(&mut desk, &watched, 2);
    assert!(desk.settle_home(&destination.client));
    assert!(desk.connection.fleet_handoff.is_none());
    assert_eq!(desk.connection.wanted.as_ref().map(|(id, _)| id), Some(&terminal(2)));
    assert!(desk.open_wanted(&destination.client));
    let shown = desk.connection.shown.as_ref().unwrap();
    assert_eq!(shown.resource, terminal(2));
    assert_eq!(destination.client.acquire_view(shown.view.unwrap()).unwrap().terminal_id, terminal(2));
    destination.client.destroy_view(shown.view.unwrap()).unwrap();
    assert!(destination.client.acquire_view(destination.views[0]).is_some());
    assert!(destination.client.acquire_view(destination.views[1]).is_some());
    assert!(destination.client.take_outbound().is_empty());
}

#[test]
fn pending_first_source_retains_origin_watch_until_destination_proof_is_captured() {
    let watched = Replica::new(80, 24, 2);
    let origin = std::sync::Arc::downgrade(&watched.client);
    let destination = Replica::pending(80, 24, 2);
    let mut desk = destination.window();
    install(&mut desk, &watched, 2);
    drop(watched);
    assert!(desk.settle_home(&destination.client));
    assert!(desk.connection.fleet_handoff.is_some());
    assert!(origin.upgrade().is_some(), "the original watch is not disposed during transfer");
    assert!(!desk.open_wanted(&destination.client));
    for frame in bootstrap_frames(&terminal(2), [2, 2], [80, 24], b"original agent") {
        destination.client.feed(frame).unwrap();
    }
    let _ = destination.client.take_outbound();
    assert!(desk.open_wanted(&destination.client));
    assert!(desk.settle_home(&destination.client));
    assert!(desk.connection.fleet_handoff.is_none());
    assert!(origin.upgrade().is_none(), "the monitor's lease ends after destination source qualification");
    assert_eq!(desk.connection.shown.as_ref().unwrap().resource, terminal(2));
    assert!(destination.client.take_outbound().is_empty());
}

#[test]
fn a_watch_reconnect_cannot_requalify_an_old_numeric_agent_target() {
    let watched = Replica::new(80, 24, 2);
    let destination = Replica::new(80, 24, 2);
    let mut desk = destination.window();
    install(&mut desk, &watched, 2);
    watched.replace_server(1); // Same HELLO, different originating connection.
    let before = destination.client.acquire(&terminal(2)).unwrap();
    assert!(desk.settle_home(&destination.client));
    assert!(desk.connection.fleet_handoff.is_none());
    assert!(desk.connection.wanted.is_none());
    assert!(std::sync::Arc::ptr_eq(&before, &destination.client.acquire(&terminal(2)).unwrap()));
    assert!(destination.client.take_outbound().is_empty());
}

#[test]
fn a_different_destination_server_cannot_reuse_the_watched_terminal_id() {
    let watched = Replica::new(80, 24, 2);
    let destination = Replica::new(80, 24, 2);
    let mut desk = destination.window();
    install(&mut desk, &watched, 2);
    destination.replace_server(2);
    let _ = destination.client.take_outbound();
    assert!(desk.settle_home(&destination.client));
    assert!(desk.connection.fleet_handoff.is_none());
    assert!(desk.connection.wanted.is_none());
    assert!(destination.client.has_projection(&terminal(2)));
    assert!(destination.client.take_outbound().is_empty());
}

#[test]
fn a_newer_destination_gesture_retires_the_old_handoff_without_replacing_its_choice() {
    let watched = Replica::new(80, 24, 2);
    let destination = Replica::new(80, 24, 2);
    let mut desk = destination.window();
    install(&mut desk, &watched, 2);
    desk.want(terminal(1));
    assert!(!desk.settle_home(&destination.client));
    assert!(desk.connection.fleet_handoff.is_none());
    assert_eq!(desk.connection.wanted.as_ref().map(|(id, _)| id), Some(&terminal(1)));
    assert!(desk.open_wanted(&destination.client));
    assert_eq!(desk.connection.shown.as_ref().unwrap().resource, terminal(1));
}

#[test]
fn closing_pending_interest_releases_the_watch_and_late_bootstrap_cannot_restore_focus() {
    let watched = Replica::new(80, 24, 2);
    let destination = Replica::pending(80, 24, 2);
    let mut desk = destination.window();
    install(&mut desk, &watched, 2);
    assert!(desk.settle_home(&destination.client));
    assert!(!desk.settle_home(&destination.client), "waiting does not busy-repaint the window");
    desk.close_shown(&destination.client, &[terminal(2)]);
    assert!(!desk.settle_home(&destination.client));
    assert!(desk.connection.fleet_handoff.is_none());
    for frame in bootstrap_frames(&terminal(2), [2, 2], [80, 24], b"late original agent") {
        destination.client.feed(frame).unwrap();
    }
    desk.open_wanted(&destination.client);
    assert_ne!(desk.connection.shown.as_ref().map(|shown| &shown.resource), Some(&terminal(2)));
}
