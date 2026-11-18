use super::*;

fn operation_id(byte: u8) -> InputOperationId {
    InputOperationId::new([byte; 16]).expect("non-zero operation id")
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
