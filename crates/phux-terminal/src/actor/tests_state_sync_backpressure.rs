use super::*;

/// Drain every `ResourceOutput` currently queued on `rx`, returning the
/// concatenated payload bytes and the ordered list of `seq`s.
fn drain_terminal_output(rx: &mut mpsc::Receiver<Outbound>) -> (Vec<u8>, Vec<u64>) {
    let mut bytes = Vec::new();
    let mut seqs = Vec::new();
    while let Ok(frame) = rx.try_recv() {
        if let Outbound::Frame(FrameKind::ResourceOutput {
            seq, bytes: body, ..
        }) = frame
        {
            seqs.push(seq);
            bytes.extend_from_slice(&body);
        }
    }
    (bytes, seqs)
}

/// A full mailbox never loses content or burns a `seq`: the permit is
/// reserved before the diff advances the reference, so every marker
/// survives once drained, in a contiguous seq stream.
#[test]
fn backpressured_consumer_loses_no_content_after_draining() {
    // More rounds than the mailbox holds so the tick's send hits `Full`.
    const ROUNDS: usize = 12;
    let bundle = TerminalActor::new(80, 24).expect("new");
    let mut actor = bundle.actor;
    actor.enable_tick_emit_for_test();

    let client = ClientId(1);
    // Tiny mailbox so a few ticks saturate it — same shape as the
    // production `DEFAULT_CLIENT_MAILBOX` pressure, smaller and faster.
    let (tx, mut rx) = mpsc::channel::<Outbound>(2);
    actor
        .register_consumer(client, tx, 11, false)
        .expect("register");

    // Write a distinct marker on its own line and tick to emit it,
    // WITHOUT draining the receiver. Every marker must survive.
    let markers: Vec<String> = (0..ROUNDS).map(|i| format!("MARK{i:03}=")).collect();
    for marker in &markers {
        actor.vt_write_for_test(format!("{marker}\r\n").as_bytes());
        actor.tick_emit();
    }

    // Drain, then keep ticking + draining so any content held back under
    // backpressure flows once there is room.
    let mut delivered = Vec::new();
    let mut all_seqs = Vec::new();
    for round in 0..=ROUNDS * 2 {
        if round > 0 {
            actor.tick_emit();
        }
        let (chunk, seqs) = drain_terminal_output(&mut rx);
        delivered.extend_from_slice(&chunk);
        all_seqs.extend(seqs);
    }
    let contiguous: Vec<u64> = (1..=all_seqs.len() as u64).collect();
    assert_eq!(
        all_seqs, contiguous,
        "a frame not shipped must not burn a seq (no gaps)"
    );

    for marker in &markers {
        assert!(
            contains_subslice(&delivered, marker.as_bytes()),
            "marker {marker:?} never reached the consumer: content was lost \
                 under mailbox backpressure (reference advanced past a dropped \
                 frame). delivered={:?}",
            String::from_utf8_lossy(&delivered),
        );
    }
}
