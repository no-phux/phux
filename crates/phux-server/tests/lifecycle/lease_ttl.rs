//! ADR-0033 `ttl_ms` through the production frame loop: a held lease
//! expires after its TTL (visible in `GET_STATE`), `SEIZE` rearms the TTL at
//! the new holder's value, and no path that ends or replaces a lease lets
//! its dead timer expire a later one. Watchers are journal-aware so
//! `EXPIRED` crosses as itself rather than rendered `RELEASED` (L1 §7.1).

use std::time::Duration;

use phux_protocol::ids::ResourceId;
use phux_protocol::wire::frame::{AgentEvent, Command, CommandResult, ControlAction, InputMode};
use phux_server_testkit::command;
use tokio::net::UnixStream;

use crate::common::{
    assert_no_control, find, journaled_demo as start, next_control as control, state,
};

async fn acquire(
    stream: &mut UnixStream,
    request_id: u32,
    pane: &ResourceId,
    mode: InputMode,
    ttl_ms: u32,
) {
    let cmd = Command::AcquireInput {
        terminal_id: pane.clone(),
        mode,
        ttl_ms,
    };
    assert_eq!(command(stream, request_id, cmd).await, CommandResult::Ok);
}

async fn release(stream: &mut UnixStream, request_id: u32, pane: &ResourceId) {
    let cmd = Command::ReleaseInput {
        terminal_id: pane.clone(),
    };
    assert_eq!(command(stream, request_id, cmd).await, CommandResult::Ok);
}

#[test]
fn a_lease_with_a_ttl_expires_and_get_state_follows_the_holder() {
    phux_server_testkit::run_local(async {
        let (server, owner, pane, mut events) = start().await;
        let mut driver = server.connect().await;
        assert_eq!(
            find(&state(&mut driver, 1).await, &pane)
                .unwrap()
                .input_holder,
            None
        );

        acquire(&mut driver, 2, &pane, InputMode::Cooperative, 150).await;
        control(&mut events, ControlAction::Acquired).await;
        assert!(
            find(&state(&mut driver, 3).await, &pane)
                .unwrap()
                .input_holder
                .is_some()
        );
        let AgentEvent::TerminalControl {
            input_holder,
            actor,
            ..
        } = control(&mut events, ControlAction::Expired).await
        else {
            unreachable!()
        };
        assert_eq!(input_holder, None, "EXPIRED returns the pane to Open");
        assert_eq!(actor, None, "the server's own timer acted");
        assert_eq!(
            find(&state(&mut driver, 4).await, &pane)
                .unwrap()
                .input_holder,
            None
        );

        drop((owner, events, driver));
        server.stop().await;
    });
}

#[test]
fn seize_rearms_the_ttl_at_the_new_holders_value() {
    phux_server_testkit::run_local(async {
        let (server, owner, pane, mut events) = start().await;
        let mut first = server.connect().await;
        let mut second = server.connect().await;
        acquire(&mut first, 1, &pane, InputMode::Cooperative, 60_000).await;
        control(&mut events, ControlAction::Acquired).await;
        acquire(&mut second, 1, &pane, InputMode::Seize, 150).await;
        control(&mut events, ControlAction::Seized).await;
        let AgentEvent::TerminalControl { input_holder, .. } =
            control(&mut events, ControlAction::Expired).await
        else {
            unreachable!()
        };
        assert_eq!(input_holder, None);

        drop((owner, events, first, second));
        server.stop().await;
    });
}

/// A release, or a same-holder `ttl_ms = 0` re-acquire, cancels the armed
/// timer, and the dead timer must never share a generation with a later
/// grant and expire it at the old deadline.
#[test]
fn a_released_or_replaced_lease_never_expires_a_later_one() {
    phux_server_testkit::run_local(async {
        let (server, owner, pane, mut events) = start().await;
        let mut driver = server.connect().await;

        acquire(&mut driver, 1, &pane, InputMode::Cooperative, 300).await;
        control(&mut events, ControlAction::Acquired).await;
        release(&mut driver, 2, &pane).await;
        control(&mut events, ControlAction::Released).await;
        acquire(&mut driver, 3, &pane, InputMode::Cooperative, 60_000).await;
        control(&mut events, ControlAction::Acquired).await;
        assert_no_control(&mut events, Duration::from_millis(800), |_| true).await;

        acquire(&mut driver, 4, &pane, InputMode::Cooperative, 300).await;
        control(&mut events, ControlAction::Acquired).await;
        acquire(&mut driver, 5, &pane, InputMode::Cooperative, 0).await;
        control(&mut events, ControlAction::Acquired).await;
        acquire(&mut driver, 6, &pane, InputMode::Cooperative, 60_000).await;
        control(&mut events, ControlAction::Acquired).await;
        assert_no_control(&mut events, Duration::from_millis(800), |_| true).await;

        drop((owner, events, driver));
        server.stop().await;
    });
}
