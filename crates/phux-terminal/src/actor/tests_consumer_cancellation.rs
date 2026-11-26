//! Registration ownership is exercised at the real actor request/loop seams.

use super::*;
use crate::actor::{
    ConsumerAttachError, ConsumerAttachOutcome, ConsumerAttachRequest, ConsumerRegistrationGuard,
    TerminalResource as _,
};
use futures_util::FutureExt;
use phux_protocol::ids::{BootstrapId, StreamId};
use tokio::sync::{mpsc, oneshot, watch};
use tokio_util::sync::CancellationToken;

struct Pending {
    request: ConsumerAttachRequest,
    guard: ConsumerRegistrationGuard,
    reply: oneshot::Receiver<Result<ConsumerAttachOutcome, ConsumerAttachError>>,
    output: mpsc::Receiver<Outbound>,
    _host_output: mpsc::Sender<Outbound>,
}

fn pending(generation: u64, state_sync: bool, lifetime: &CancellationToken) -> Pending {
    let stream_id = StreamId::new(7).unwrap();
    let bootstrap_id = BootstrapId::new(generation).unwrap();
    let guard = ConsumerRegistrationGuard::new(lifetime);
    let (outbound, output) = mpsc::channel(8);
    let (reply, response) = oneshot::channel();
    Pending {
        request: ConsumerAttachRequest {
            client_id: ClientId(1),
            outbound: outbound.clone(),
            wire_terminal_id: 1,
            stream_id,
            bootstrap_id,
            cancellation: guard.cancellation(),
            wants_state_sync: state_sync,
            state_sync_scrollback: None,
            image_protocols: phux_protocol::caps::ImageProtocolSet::all(),
            bootstrap_max_bytes: usize::MAX,
            bootstrap_max_frames: usize::MAX,
            bootstrap_chunk_bytes: 1,
            loss_tolerant: false,
            live_gate: watch::channel(false).1,
            reply,
        },
        guard,
        reply: response,
        output,
        _host_output: outbound,
    }
}

#[test]
fn withdrawal_before_actor_install_refuses_a_late_queued_registration() {
    let bundle = TerminalActor::new(20, 5).unwrap();
    let mut actor = bundle.actor;
    let pending = pending(1, true, &CancellationToken::new());
    drop(pending.guard);
    actor.handle_consumer_attach(pending.request);
    assert!(matches!(
        pending.reply.now_or_never(),
        Some(Ok(Err(ConsumerAttachError::Cancelled)))
    ));
    assert_eq!(actor.consumer_count(), 0);
    assert!(actor.publication.cancellation_waits_empty());
}

#[test]
fn dropped_registration_reply_cannot_install_interest() {
    let bundle = TerminalActor::new(20, 5).unwrap();
    let mut actor = bundle.actor;
    let pending = pending(1, true, &CancellationToken::new());
    drop(pending.reply);
    actor.handle_consumer_attach(pending.request);
    assert_eq!(actor.consumer_count(), 0);
    assert!(actor.publication.cancellation_waits_empty());
}

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn installed_raw_and_gated_consumers_retire_with_full_detach_mailbox() {
    for state_sync in [false, true] {
        let bundle = TerminalActor::new(20, 5).unwrap();
        let mut actor = bundle.actor;
        let handle = bundle.handle.terminal().unwrap();
        let pending = pending(1, state_sync, &CancellationToken::new());
        actor.handle_consumer_attach(pending.request);
        assert_eq!(actor.consumer_count(), 1);
        // Leave the successful reply unread: cancellation wins after actor install.
        let mut state = actor.arm_run_loop().await;
        let deadline = tokio::time::sleep(std::time::Duration::from_secs(3600));
        tokio::pin!(deadline);
        {
            let run = actor.drive_run_loop(&mut state, deadline.as_mut());
            tokio::pin!(run);
            assert!(futures_util::poll!(run.as_mut()).is_pending());
            while handle
                .consumer_detach
                .try_send(ConsumerDetachRequest {
                    client_id: ClientId(99),
                    generation: Some((StreamId::new(7).unwrap(), BootstrapId::new(1).unwrap())),
                    reply: oneshot::channel().0,
                })
                .is_ok()
            {}
            assert_eq!(handle.consumer_detach.capacity(), 0);
            // No state tick, native lease or closed outbound is needed to wake cleanup.
            drop(pending.guard);
            assert!(futures_util::poll!(run.as_mut()).is_pending());
        }
        assert_eq!(actor.consumer_count(), 0);
        assert!(actor.publication.cancellation_waits_empty());
        assert!(
            !pending.output.is_closed(),
            "the connection is still usable"
        );
        // Draining the full mailbox spent this task's coop budget, so a
        // budgeted poll of the reply would report Pending; read it directly.
        let mut reply = pending.reply;
        assert!(
            reply.try_recv().unwrap().is_ok(),
            "late success is not renewed interest"
        );
    }
}

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn old_withdrawal_and_late_request_cannot_remove_the_replacement() {
    let bundle = TerminalActor::new(20, 5).unwrap();
    let mut actor = bundle.actor;
    let old = pending(1, true, &CancellationToken::new());
    let new = pending(2, true, &CancellationToken::new());
    actor.handle_consumer_attach(new.request);
    assert!(new.reply.now_or_never().unwrap().unwrap().is_ok());
    drop(old.guard);
    actor.handle_consumer_attach(old.request);
    assert!(matches!(
        old.reply.now_or_never(),
        Some(Ok(Err(ConsumerAttachError::Cancelled)))
    ));
    let mut state = actor.arm_run_loop().await;
    let deadline = tokio::time::sleep(std::time::Duration::from_secs(3600));
    tokio::pin!(deadline);
    assert!(
        actor
            .drive_run_loop(&mut state, deadline.as_mut())
            .now_or_never()
            .is_none()
    );
    let current = actor.consumer_state(ClientId(1)).unwrap();
    assert_eq!(current.sync.bootstrap_id, BootstrapId::new(2).unwrap());
    assert!(!current.cancellation.is_cancelled());
    assert_eq!(actor.consumer_count(), 1);
}

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn published_interest_observes_parent_cancellation_without_a_detach_message() {
    let bundle = TerminalActor::new(20, 5).unwrap();
    let mut actor = bundle.actor;
    let lifetime = CancellationToken::new();
    let pending = pending(1, false, &lifetime);
    actor.handle_consumer_attach(pending.request);
    assert!(pending.reply.now_or_never().unwrap().unwrap().is_ok());
    pending.guard.published();
    let mut state = actor.arm_run_loop().await;
    let deadline = tokio::time::sleep(std::time::Duration::from_secs(3600));
    tokio::pin!(deadline);
    {
        let run = actor.drive_run_loop(&mut state, deadline.as_mut());
        tokio::pin!(run);
        assert!(futures_util::poll!(run.as_mut()).is_pending());
        lifetime.cancel();
        assert!(futures_util::poll!(run.as_mut()).is_pending());
    }
    assert_eq!(actor.consumer_count(), 0);
    assert!(actor.publication.cancellation_waits_empty());
    assert!(!pending.output.is_closed());
}

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn cancelled_installed_registration_cannot_retire_its_replacement() {
    let bundle = TerminalActor::new(20, 5).unwrap();
    let mut actor = bundle.actor;
    let old = pending(1, false, &CancellationToken::new());
    actor.handle_consumer_attach(old.request);
    assert!(old.reply.now_or_never().unwrap().unwrap().is_ok());
    let new = pending(2, false, &CancellationToken::new());
    actor.handle_consumer_attach(new.request);
    assert!(new.reply.now_or_never().unwrap().unwrap().is_ok());
    drop(old.guard);
    let mut state = actor.arm_run_loop().await;
    let deadline = tokio::time::sleep(std::time::Duration::from_secs(3600));
    tokio::pin!(deadline);
    assert!(
        actor
            .drive_run_loop(&mut state, deadline.as_mut())
            .now_or_never()
            .is_none()
    );
    let current = actor.consumer_state(ClientId(1)).unwrap();
    assert_eq!(current.sync.bootstrap_id, BootstrapId::new(2).unwrap());
    assert!(!current.cancellation.is_cancelled());
}
