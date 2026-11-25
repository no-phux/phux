use super::*;

#[tokio::test(flavor = "current_thread")]
async fn two_native_pumps_from_one_client_do_not_tombstone_each_other() {
    let bundle = TerminalActor::new(20, 5).expect("new actor");
    let mut actor = bundle.actor;
    let mut output = bundle.handle.output.subscribe();
    let owner = 7_u64;
    let stream_a = phux_protocol::ids::StreamId::new(1).expect("stream");
    let stream_b = phux_protocol::ids::StreamId::new(2).expect("stream");
    let bootstrap_a = phux_protocol::ids::BootstrapId::new(1).expect("bootstrap");
    let bootstrap_b = phux_protocol::ids::BootstrapId::new(2).expect("bootstrap");

    let first = capture_native_for_pump(&mut actor, owner, stream_a, bootstrap_a);
    let second = capture_native_for_pump(&mut actor, owner, stream_b, bootstrap_b);

    let mut tombstones = Vec::new();
    while let Ok(message) = output.try_recv() {
        if let ResourceOutput::Control {
            frame: FrameKind::BootstrapTombstone { stream_id, .. },
            ..
        } = message
        {
            tombstones.push(stream_id);
        }
    }
    assert!(
        tombstones.is_empty(),
        "a sibling pump must not tombstone the first: {tombstones:?}"
    );
    assert!(
        actor.publication
            .native_cursor_owners
            .contains_key(&NativeCursorKey::new(owner, stream_a))
            && actor.publication
                .native_cursor_owners
                .contains_key(&NativeCursorKey::new(owner, stream_b)),
        "both pumps keep a live binding"
    );
    activate_native_for_pump(
        &mut actor,
        owner,
        stream_a,
        bootstrap_a,
        first.capture.cursor,
    )
    .expect("first pump still activates publication");
    activate_native_for_pump(
        &mut actor,
        owner,
        stream_b,
        bootstrap_b,
        second.capture.cursor,
    )
    .expect("second pump activates publication");

    actor.release_native_owner(owner);
    assert!(
        actor.publication.native_cursor_owners.is_empty(),
        "client detach still releases every pump for that owner"
    );
}

/// Recapturing one `(owner, stream_id)` tombstones only that binding.
#[tokio::test(flavor = "current_thread")]
async fn recapturing_the_same_owner_stream_tombstones_only_that_binding() {
    let bundle = TerminalActor::new(20, 5).expect("new actor");
    let mut actor = bundle.actor;
    let mut output = bundle.handle.output.subscribe();
    let owner = 7_u64;
    let stream_a = phux_protocol::ids::StreamId::new(1).expect("stream");
    let stream_b = phux_protocol::ids::StreamId::new(2).expect("stream");
    let bootstrap_a = phux_protocol::ids::BootstrapId::new(1).expect("bootstrap");
    let bootstrap_b = phux_protocol::ids::BootstrapId::new(2).expect("bootstrap");
    let bootstrap_a2 = phux_protocol::ids::BootstrapId::new(3).expect("bootstrap");

    let first = capture_native_for_pump(&mut actor, owner, stream_a, bootstrap_a);
    let sibling = capture_native_for_pump(&mut actor, owner, stream_b, bootstrap_b);
    while output.try_recv().is_ok() {}

    let replacement = capture_native_for_pump(&mut actor, owner, stream_a, bootstrap_a2);
    let mut tombstoned = Vec::new();
    while let Ok(message) = output.try_recv() {
        if let ResourceOutput::Control {
            frame:
                FrameKind::BootstrapTombstone {
                    stream_id,
                    bootstrap_id,
                    reason,
                    ..
                },
            ..
        } = message
        {
            tombstoned.push((stream_id, bootstrap_id, reason));
        }
    }
    assert_eq!(
        tombstoned,
        [(
            stream_a,
            bootstrap_a,
            phux_protocol::wire::frame::TombstoneReason::ExplicitReattach
        )],
        "recapture tombstones only the same (owner, stream) generation"
    );
    assert_eq!(
        actor.publication
            .native_cursor_owners
            .get(&NativeCursorKey::new(owner, stream_a))
            .map(|binding| binding.bootstrap_id),
        Some(bootstrap_a2)
    );
    assert_eq!(
        actor.publication
            .native_cursor_owners
            .get(&NativeCursorKey::new(owner, stream_b))
            .map(|binding| binding.bootstrap_id),
        Some(bootstrap_b)
    );
    assert!(
        activate_native_for_pump(
            &mut actor,
            owner,
            stream_a,
            bootstrap_a,
            first.capture.cursor,
        )
        .is_err(),
        "the recaptured generation cannot activate publication"
    );
    activate_native_for_pump(
        &mut actor,
        owner,
        stream_b,
        bootstrap_b,
        sibling.capture.cursor,
    )
    .expect("sibling pump still activates");
    activate_native_for_pump(
        &mut actor,
        owner,
        stream_a,
        bootstrap_a2,
        replacement.capture.cursor,
    )
    .expect("replacement pump activates");
}

/// Two pumps of one client joining one capture both stay waiters and
/// activate publication.
#[tokio::test(flavor = "current_thread")]
async fn two_same_client_pumps_can_share_one_pending_capture() {
    let bundle = TerminalActor::new(20, 5).expect("new actor");
    let mut actor = bundle.actor;
    let owner = 7_u64;
    let stream_a = phux_protocol::ids::StreamId::new(1).expect("stream");
    let stream_b = phux_protocol::ids::StreamId::new(2).expect("stream");
    let bootstrap_a = phux_protocol::ids::BootstrapId::new(1).expect("bootstrap");
    let bootstrap_b = phux_protocol::ids::BootstrapId::new(2).expect("bootstrap");
    let request = |stream_id, bootstrap_id, reply| {
        native_request(
            owner,
            phux_protocol::ids::ResourceId::local(1),
            (stream_id, bootstrap_id),
            reply,
        )
    };
    let (reply_a, mut response_a) = oneshot::channel();
    actor.handle_native_bootstrap(request(stream_a, bootstrap_a, reply_a));
    let (reply_b, mut response_b) = oneshot::channel();
    actor.handle_native_bootstrap(request(stream_b, bootstrap_b, reply_b));
    assert_eq!(
        actor.publication
            .pending_native_bootstrap
            .as_ref()
            .map(|pending| pending.waiters.len()),
        Some(2),
        "both pumps wait on the shared capture"
    );

    let mut first = None;
    let mut second = None;
    for _ in 0..64 {
        if first.is_none()
            && let Ok(result) = response_a.try_recv()
        {
            first = Some(result.expect("first capture"));
        }
        if second.is_none()
            && let Ok(result) = response_b.try_recv()
        {
            second = Some(result.expect("second capture"));
        }
        if first.is_some() && second.is_some() {
            break;
        }
        actor.cooperative_native_step();
    }
    let first = first.expect("first capture finished");
    let second = second.expect("second capture finished");
    assert_eq!(first.capture.cursor, second.capture.cursor);
    activate_native_for_pump(
        &mut actor,
        owner,
        stream_a,
        bootstrap_a,
        first.capture.cursor,
    )
    .expect("first shared waiter activates");
    activate_native_for_pump(
        &mut actor,
        owner,
        stream_b,
        bootstrap_b,
        second.capture.cursor,
    )
    .expect("second shared waiter activates");
}
