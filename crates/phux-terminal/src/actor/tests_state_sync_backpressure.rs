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

/// A full mailbox never loses content: the permit is reserved before the
/// diff advances the reference, so every marker survives once drained.
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
    let (chunk, _seqs) = drain_terminal_output(&mut rx);
    delivered.extend_from_slice(&chunk);
    for _ in 0..ROUNDS * 2 {
        actor.tick_emit();
        let (chunk, _seqs) = drain_terminal_output(&mut rx);
        delivered.extend_from_slice(&chunk);
    }

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

/// A frame not shipped does not consume a `seq` (no gaps, SPEC §12.2).
#[test]
fn backpressured_consumer_sees_contiguous_seq_stream() {
    const ROUNDS: usize = 10;
    let bundle = TerminalActor::new(80, 24).expect("new");
    let mut actor = bundle.actor;
    actor.enable_tick_emit_for_test();

    let client = ClientId(1);
    let (tx, mut rx) = mpsc::channel::<Outbound>(2);
    actor
        .register_consumer(client, tx, 11, false)
        .expect("register");

    for i in 0..ROUNDS {
        actor.vt_write_for_test(format!("seqmark{i:03}\r\n").as_bytes());
        actor.tick_emit();
    }

    let mut all_seqs = Vec::new();
    let (_b, seqs) = drain_terminal_output(&mut rx);
    all_seqs.extend(seqs);
    for _ in 0..ROUNDS * 2 {
        actor.tick_emit();
        let (_b, seqs) = drain_terminal_output(&mut rx);
        all_seqs.extend(seqs);
    }

    assert!(
        !all_seqs.is_empty(),
        "expected at least one delivered frame"
    );
    for (idx, seq) in all_seqs.iter().enumerate() {
        let expected = u64::try_from(idx).expect("fits") + 1;
        assert_eq!(
            *seq, expected,
            "delivered seq stream must be contiguous from 1 with no gaps; \
                 a dropped-but-seq-burned frame leaves a hole. got={all_seqs:?}",
        );
    }
}
