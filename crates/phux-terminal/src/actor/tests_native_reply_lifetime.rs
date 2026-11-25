//! Unread native capture/activation replies own actual engine retains.

#![allow(clippy::future_not_send, reason = "native replies tests drive the LocalSet-only actor")]

use crate::actor::{NativeBootstrapRequest, NativePublicationRequest, TerminalActor};
use futures_util::FutureExt;
use phux_protocol::ids::{BootstrapId, StreamId};
use tokio::sync::oneshot;

fn request(actor: &mut TerminalActor, generation: u64) -> oneshot::Receiver<Result<crate::actor::NativeBootstrapReply, crate::native_state::NativeStateError>> {
    let (reply, response) = oneshot::channel();
    actor.handle_native_bootstrap(NativeBootstrapRequest::full_budget(
        1,
        phux_protocol::ids::ResourceId::local(1),
        StreamId::new(7).unwrap(),
        BootstrapId::new(generation).unwrap(),
        phux_protocol::caps::BootstrapLimits::default(),
        reply,
    ));
    for _ in 0..64 {
        if !actor.native_bootstrap_pending() {
            return response;
        }
        actor.cooperative_native_step();
    }
    panic!("native capture exceeded its bounded cooperative fixture");
}

#[allow(clippy::future_not_send, reason = "drives the LocalSet-only terminal owner")]
async fn retire_cancelled(actor: &mut TerminalActor) {
    let mut state = actor.arm_run_loop().await;
    let deadline = tokio::time::sleep(std::time::Duration::from_secs(3600));
    tokio::pin!(deadline);
    assert!(actor.drive_run_loop(&mut state, deadline.as_mut()).now_or_never().is_none());
}

fn assert_released(actor: &TerminalActor, cursor: &crate::native_state::OpaqueHistoryCursor) {
    assert!(!actor.terminal.borrow_mut().native_manager().unwrap().has_generation(cursor));
    assert!(actor.publication.native_cursor_owners.is_empty());
    assert!(actor.publication.native_publications.is_empty());
}

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn dropping_an_unread_successful_capture_releases_the_engine_without_a_detach() {
    let mut actor = TerminalActor::new(20, 5).unwrap().actor;
    let response = request(&mut actor, 1);
    let cursor = actor.publication.native_cursor_owners.values().next().unwrap().cursor;
    assert!(actor.terminal.borrow_mut().native_manager().unwrap().has_generation(&cursor));
    drop(response);
    retire_cancelled(&mut actor).await;
    assert_released(&actor, &cursor);
    assert!(request(&mut actor, 2).try_recv().unwrap().is_ok(), "the terminal can capture again");
}

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn dropping_an_unread_activation_success_releases_history_and_publication() {
    let mut actor = TerminalActor::new(20, 5).unwrap().actor;
    let capture = request(&mut actor, 1).try_recv().unwrap().unwrap();
    let cursor = capture.capture.cursor;
    let (reply, response) = oneshot::channel();
    actor.handle_native_publication(NativePublicationRequest {
        owner: 1,
        terminal_id: phux_protocol::ids::ResourceId::local(1),
        stream_id: StreamId::new(7).unwrap(),
        bootstrap_id: BootstrapId::new(1).unwrap(),
        cursor,
        cancellation: capture.capture.cancellation(),
        reply,
    });
    // Both replies succeeded. Their receivers disappearing is still withdrawal,
    // not a published subscription and not authority to retire any replacement.
    drop(capture);
    drop(response);
    retire_cancelled(&mut actor).await;
    assert_released(&actor, &cursor);
}

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn an_old_reply_drop_cannot_withdraw_a_same_cursor_replacement() {
    let mut actor = TerminalActor::new(20, 5).unwrap().actor;
    let old = request(&mut actor, 1).try_recv().unwrap().unwrap();
    let replacement = request(&mut actor, 2).try_recv().unwrap().unwrap();
    // An unchanged engine can reuse its opaque cursor, but not its lifetime.
    drop(old);
    retire_cancelled(&mut actor).await;
    let binding = actor.publication.native_cursor_owners.values().next().unwrap();
    assert_eq!(binding.bootstrap_id, BootstrapId::new(2).unwrap());
    assert!(!binding.cancellation.is_cancelled());
    let (reply, mut response) = oneshot::channel();
    actor.handle_native_publication(NativePublicationRequest {
        owner: 1,
        terminal_id: phux_protocol::ids::ResourceId::local(1),
        stream_id: StreamId::new(7).unwrap(),
        bootstrap_id: BootstrapId::new(2).unwrap(),
        cursor: replacement.capture.cursor,
        cancellation: replacement.capture.cancellation(),
        reply,
    });
    let published = response.try_recv().unwrap().unwrap();
    let cursor = replacement.capture.cursor;
    drop(replacement);
    drop(published);
    retire_cancelled(&mut actor).await;
    assert_released(&actor, &cursor);
}

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn capture_capacity_refuses_before_queueing_and_recovers_after_reply_cancellation() {
    let mut actor = TerminalActor::new(20, 5).unwrap().actor;
    let mut responses = Vec::new();
    for owner in 1..=8 {
        let (reply, response) = oneshot::channel();
        actor.handle_native_bootstrap(NativeBootstrapRequest::full_budget(
            owner, phux_protocol::ids::ResourceId::local(1), StreamId::new(owner).unwrap(),
            BootstrapId::new(1).unwrap(), phux_protocol::caps::BootstrapLimits::default(), reply,
        ));
        responses.push(response);
    }
    let (reply, mut refused) = oneshot::channel();
    actor.handle_native_bootstrap(NativeBootstrapRequest::full_budget(
        9, phux_protocol::ids::ResourceId::local(1), StreamId::new(9).unwrap(),
        BootstrapId::new(1).unwrap(), phux_protocol::caps::BootstrapLimits::default(), reply,
    ));
    assert!(matches!(refused.try_recv(), Ok(Err(crate::native_state::NativeStateError::LimitExceeded))));
    for _ in 0..64 {
        if !actor.native_bootstrap_pending() { break; }
        actor.cooperative_native_step();
    }
    let captures: Vec<_> = responses.iter_mut().map(|response| response.try_recv().unwrap().unwrap()).collect();
    let cursor = captures[0].capture.cursor;
    assert!(actor.terminal.borrow_mut().native_manager().unwrap().has_generation(&cursor));
    drop(captures);
    retire_cancelled(&mut actor).await;
    assert_released(&actor, &cursor);
    assert!(request(&mut actor, 2).try_recv().unwrap().is_ok());
}

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn cancelled_queued_activation_cannot_withdraw_its_published_successor() {
    let mut actor = TerminalActor::new(20, 5).unwrap().actor;
    let capture = request(&mut actor, 1).try_recv().unwrap().unwrap();
    let cursor = capture.capture.cursor;
    let (old_reply, old_response) = oneshot::channel();
    // Both requests captured the same lifetime before either entered the actor.
    let queued = NativePublicationRequest {
        owner: 1, terminal_id: phux_protocol::ids::ResourceId::local(1),
        stream_id: StreamId::new(7).unwrap(), bootstrap_id: BootstrapId::new(1).unwrap(),
        cursor, cancellation: capture.capture.cancellation(), reply: old_reply,
    };
    let (reply, mut response) = oneshot::channel();
    actor.handle_native_publication(NativePublicationRequest {
        owner: 1, terminal_id: phux_protocol::ids::ResourceId::local(1),
        stream_id: StreamId::new(7).unwrap(), bootstrap_id: BootstrapId::new(1).unwrap(),
        cursor, cancellation: capture.capture.cancellation(), reply,
    });
    let publication = response.try_recv().unwrap().unwrap();
    drop(old_response);
    actor.handle_native_publication(queued);
    retire_cancelled(&mut actor).await;
    assert!(actor.terminal.borrow_mut().native_manager().unwrap().has_generation(&cursor));
    assert!(!actor.publication.native_cursor_owners.values().next().unwrap().cancellation.is_cancelled());
    // The unread stale request had no authority over this newer reply owner.
    drop(publication);
    retire_cancelled(&mut actor).await;
    assert_released(&actor, &cursor);
}
