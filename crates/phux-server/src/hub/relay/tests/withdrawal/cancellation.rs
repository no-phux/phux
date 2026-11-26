//! Registration cancellation and withdrawal isolation across link epochs.

use super::*;

#[tokio::test]
async fn dropping_a_registration_waiter_withdraws_before_or_after_mailbox_admission() {
    for admitted in [false, true] {
        let (handle, mut mailbox) = RelayHandle::new(host());
        let mut session = RelaySession::new(host(), BootstrapLimits::default());
        session.set_resource_kinds(Arc::clone(&mailbox.resource_kinds));
        let (out_tx, mut out_rx) = mpsc::channel(8);
        let mut registration = Box::pin(handle.command_subscribing(
            Command::SubscribeResourceEvents {
                terminal_id: ResourceId::local(9),
                event_types: Vec::new(),
            },
            proxy(9, ClientId(1), out_tx),
        ));
        assert!(futures_util::poll!(&mut registration).is_pending());
        let request = mailbox.requests.try_recv().unwrap();
        if admitted {
            assert_eq!(session.handle_queued_request(request).len(), 1);
        } else {
            // Withdrawal can overtake the bounded registration mailbox.
            drop(registration);
            session.handle_queued_unsubscribe(mailbox.unsubscribes.try_recv().unwrap());
            assert!(session.handle_queued_request(request).is_empty());
            assert!(!session.subscribers.contains_key(&9));
            continue;
        }
        drop(registration);
        assert_eq!(
            session
                .handle_queued_unsubscribe(mailbox.unsubscribes.try_recv().unwrap())
                .len(),
            1,
        );
        session.feed(&agent_event(9));
        assert!(
            out_rx.try_recv().is_err(),
            "cancelled registrations must not receive events"
        );
        assert!(!session.subscribers.contains_key(&9));
    }
}

#[tokio::test]
async fn cancelled_registration_cannot_withdraw_a_replacement() {
    let (handle, mut mailbox) = RelayHandle::new(host());
    let mut session = RelaySession::new(host(), BootstrapLimits::default());
    session.set_resource_kinds(Arc::clone(&mailbox.resource_kinds));
    let (out_tx, mut out_rx) = mpsc::channel(8);
    let command = Command::SubscribeResourceEvents {
        terminal_id: ResourceId::local(9),
        event_types: Vec::new(),
    };
    let mut first = Box::pin(
        handle.command_subscribing(command.clone(), proxy(9, ClientId(1), out_tx.clone())),
    );
    assert!(futures_util::poll!(&mut first).is_pending());
    session.handle_queued_request(mailbox.requests.try_recv().unwrap());
    drop(first);
    let mut replacement =
        Box::pin(handle.command_subscribing(command, proxy(9, ClientId(1), out_tx)));
    assert!(futures_util::poll!(&mut replacement).is_pending());
    session.handle_queued_request(mailbox.requests.try_recv().unwrap());
    assert!(
        session
            .handle_queued_unsubscribe(mailbox.unsubscribes.try_recv().unwrap())
            .is_empty()
    );
    session.feed(&agent_event(9));
    assert!(matches!(
        next_frame(&mut out_rx, "replacement remains subscribed"),
        FrameKind::Event { .. }
    ));
}

#[tokio::test]
async fn stale_link_withdrawal_cannot_detach_a_reused_terminal() {
    let (handle, mut mailbox) = RelayHandle::new(host());
    let mut old = RelaySession::new(host(), BootstrapLimits::default());
    old.set_resource_kinds(Arc::clone(&mailbox.resource_kinds));
    let old_handle = handle.pinned();
    let mut withdrawal = Box::pin(old_handle.unsubscribe_resource(ClientId(1), 9));
    assert!(futures_util::poll!(&mut withdrawal).is_pending());
    old.teardown("restart");
    let mut replacement = RelaySession::new(host(), BootstrapLimits::default());
    replacement.set_resource_kinds(Arc::clone(&mailbox.resource_kinds));
    let (out_tx, mut out_rx) = mpsc::channel(8);
    subscribe(&mut replacement, 9, ClientId(1), out_tx);
    assert!(
        replacement
            .handle_queued_unsubscribe(mailbox.unsubscribes.try_recv().unwrap())
            .is_empty()
    );
    assert_eq!(
        withdrawal.await,
        CommandResult::Ok,
        "old link has no live registration"
    );
    replacement.feed(&agent_event(9));
    assert!(matches!(
        next_frame(&mut out_rx, "new link stays subscribed"),
        FrameKind::Event { .. }
    ));
    assert!(replacement.pending_detaches.is_empty());
}

/// What a redial resumed is still the old link's to withdraw: a withdrawal
/// stamped on that link reaches the attach the new link carried over.
#[tokio::test]
async fn stale_link_withdrawal_reaches_the_attach_the_new_link_resumed() {
    let (handle, mut mailbox) = RelayHandle::new(host());
    let mut old = RelaySession::new(host(), BootstrapLimits::default());
    old.set_resource_kinds(Arc::clone(&mailbox.resource_kinds));
    old.set_incarnation(Some([1; 16]));
    let (out_tx, _out_rx) = mpsc::channel(8);
    let subscription = ProxySubscription {
        awaits_snapshot: true,
        bootstrap_profile: Some(old.bootstrap_profile),
        bootstrap_limits: Some(old.bootstrap_limits),
        ..proxy(9, ClientId(1), out_tx)
    };
    let attach = Command::attach_terminal(ResourceId::local(9), None);
    let mut attached = Box::pin(handle.command_subscribing(attach, subscription));
    assert!(futures_util::poll!(&mut attached).is_pending());
    old.handle_queued_request(mailbox.requests.try_recv().unwrap());
    crate::hub::relay::tests::resume::answer(&mut old, &CommandResult::Ok);
    assert_eq!(attached.await, CommandResult::Ok);

    let old_handle = handle.pinned();
    let mut withdrawal = Box::pin(old_handle.unsubscribe_resource(ClientId(1), 9));
    assert!(futures_util::poll!(&mut withdrawal).is_pending());
    let resume = old.teardown("lost");
    let mut replacement = RelaySession::new(host(), BootstrapLimits::default());
    replacement.set_resource_kinds(Arc::clone(&mailbox.resource_kinds));
    replacement.set_incarnation(Some([1; 16]));
    replacement.resume(resume);
    assert!(
        replacement.subscribers.contains_key(&9),
        "the attach crossed"
    );
    let detach = replacement.handle_queued_unsubscribe(mailbox.unsubscribes.try_recv().unwrap());
    assert_eq!(withdrawal.await, CommandResult::Ok);
    assert!(!replacement.subscribers.contains_key(&9), "withdrawn");
    assert_eq!(detach.len(), 1, "its last proxy left: detach upstream");
}
