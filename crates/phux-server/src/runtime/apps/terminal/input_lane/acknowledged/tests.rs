use super::*;

fn operation_id(byte: u8) -> InputOperationId {
    InputOperationId::new([byte; 16]).expect("non-zero operation id")
}

fn operation_id_from_u64(value: u64) -> InputOperationId {
    let mut bytes = [0; 16];
    bytes[8..].copy_from_slice(&value.to_be_bytes());
    InputOperationId::new(bytes).expect("non-zero operation id")
}

#[test]
fn operation_digest_is_canonical_without_cloning_event_payloads() {
    use phux_protocol::input::paste::{PasteEvent, PasteTrust};

    let operation_id = operation_id(19);
    let terminal_id = phux_protocol::ResourceId::local(7);
    let events = vec![InputEvent::Paste(PasteEvent {
        trust: PasteTrust::Trusted,
        data: vec![b'x'; 4096],
    })];
    let InputEvent::Paste(paste) = &events[0] else {
        unreachable!()
    };
    let payload_ptr = paste.data.as_ptr();
    let reference = FrameKind::Command {
        request_id: 0,
        command: Command::ApplyInput {
            operation_id,
            terminal_id: terminal_id.clone(),
            events: events.clone(),
        },
    };
    let mut encoded = BytesMut::new();
    reference.encode(&mut encoded);
    let expected: [u8; 32] = Sha256::digest(&encoded).into();

    let (actual, recovered) = operation_digest(operation_id, &terminal_id, events);
    assert_eq!(actual, expected, "digest remains the canonical frame hash");
    let InputEvent::Paste(recovered_paste) = &recovered[0] else {
        unreachable!()
    };
    assert_eq!(
        recovered_paste.data.as_ptr(),
        payload_ptr,
        "digesting transfers and recovers the event allocation instead of cloning it"
    );
}

/// phux-w7z2.60: a partial write (`Failed`) is unknown and must not be
/// retried under a fresh id; a request that never reached a writer
/// (`NotWritten`) has its own code.
#[test]
fn not_written_and_failed_map_to_distinct_error_codes() {
    assert_eq!(
        completion_result(WriteCompletion::NotWritten),
        not_written()
    );
    assert_eq!(
        completion_result(WriteCompletion::Failed),
        delivery_unknown()
    );
    assert!(matches!(
        not_written(),
        CommandResult::Error {
            code: ErrorCode::InputNotWritten,
            ..
        }
    ));
}

/// The facade keeps the lane's reading of the shared record: a pending
/// retry receives the owner's final result, a refusal before handoff
/// reaches the waiters without being cached, and a repeat after it owns
/// the operation again. The record's own bounds are pinned in
/// `runtime::operation_dedupe`.
#[test]
fn facade_maps_the_shared_record_onto_command_results() {
    let cache = SharedOperationCache::default();
    let id = operation_id(21);
    let now = Instant::now();
    assert!(matches!(
        cache.claim_at(id, [0x21; 32], now),
        CacheClaim::Owner
    ));
    let CacheClaim::Pending(mut waiter) = cache.claim_at(id, [0x21; 32], now) else {
        panic!("a same-id retry joins the unresolved operation");
    };
    let refusal = not_written();
    cache.set_retryable(id, &refusal);
    assert_eq!(waiter.try_recv().expect("refusal delivered"), refusal);
    assert!(matches!(
        cache.claim_at(id, [0x21; 32], now),
        CacheClaim::Owner
    ));
    cache.set_final(id, &CommandResult::Ok);
    assert!(matches!(
        cache.claim_at(id, [0x21; 32], now),
        CacheClaim::Final(CommandResult::Ok)
    ));
    assert!(matches!(
        cache.claim_at(id, [0x22; 32], now),
        CacheClaim::Conflict
    ));
    assert!(matches!(
        cache.claim_at(operation_id_from_u64(22), [0x22; 32], now),
        CacheClaim::Owner
    ));
}

/// Deadlines are assigned in registration order, so the waiter's ordered
/// index must hand back the earliest first regardless of insertion order.
#[test]
fn pending_set_orders_by_deadline_and_forgets_taken_tickets() {
    let admission = Arc::new(AcknowledgedAdmission::default());
    let mut tickets = TicketSource::default();
    let now = Instant::now();
    let mut pending = PendingSet::default();

    let mut register = |pending: &mut PendingSet, terminal: u32, offset_ms: u64| {
        let terminal_id = phux_protocol::ResourceId::local(terminal);
        let reservation = AcknowledgedReservation::try_acquire(&admission, &terminal_id)
            .expect("distinct terminals admit");
        // The receiver is dropped immediately: this test exercises the
        // ordered index, not delivery.
        let (reply, _rx) = oneshot::channel();
        let ticket = tickets.next_ticket();
        pending.insert(PendingCompletion::new(
            ticket,
            operation_id(terminal.try_into().expect("small")),
            now + Duration::from_millis(offset_ms),
            reservation,
            reply,
        ));
        ticket
    };

    let early = register(&mut pending, 1, 10);
    let late = register(&mut pending, 2, 30);
    let middle = register(&mut pending, 3, 20);

    assert_eq!(
        pending.next_deadline(),
        Some(now + Duration::from_millis(10))
    );
    assert!(pending.take(middle).is_some());
    assert!(
        pending.take(middle).is_none(),
        "a taken ticket must not resolve twice"
    );
    assert_eq!(
        pending.take_expired(now + Duration::from_millis(15)).len(),
        1,
        "only the earliest deadline has passed"
    );
    assert_eq!(
        pending.next_deadline(),
        Some(now + Duration::from_millis(30))
    );
    assert!(pending.take(early).is_none());
    assert!(pending.take(late).is_some());
    assert!(pending.next_deadline().is_none());
}
