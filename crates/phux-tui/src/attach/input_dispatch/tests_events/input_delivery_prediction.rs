// ---- acknowledged input --------------------------------------------------------------

fn journal() -> std::cell::RefCell<InputReplayJournal> {
    let journal = std::cell::RefCell::new(InputReplayJournal::new());
    assert!(
        journal
            .borrow_mut()
            .begin_connection(Some(&[0xAB; 16]), true)
            .is_empty()
    );
    journal
}

fn apply_input_events(frame: &FrameKind) -> (u32, &[InputEvent]) {
    match frame {
        FrameKind::Command {
            request_id,
            command: Command::ApplyInput { events, .. },
        } => (*request_id, events),
        other => panic!("expected APPLY_INPUT, got {other:?}"),
    }
}

/// Pastes and the Enter after them go out one `APPLY_INPUT` at a time in
/// terminal order; an oversized paste is refused visibly.
#[tokio::test]
async fn dispatch_queues_paste_then_paste_then_enter_in_terminal_order() {
    use phux_protocol::input::paste::{PasteEvent, PasteTrust};
    let journal = journal();
    let paste = |data: &[u8]| {
        InputEvent::Paste(PasteEvent {
            trust: PasteTrust::Trusted,
            data: data.to_vec(),
        })
    };
    let mut env = two_pane_env(&[], (8, 16));
    env.journal = Some(&journal);
    let sent = env
        .dispatch(vec![
            paste(b"A"),
            paste(b"B"),
            paste(&vec![b'x'; 70 * 1024]),
            press(PhysicalKey::Enter, None),
        ])
        .await;
    assert_eq!(sent.frames.len(), 1, "only A is sent before its reply");
    let overflow = journal.borrow_mut().take_reports();
    assert_eq!(overflow.len(), 1);
    assert_eq!(overflow[0].disposition, ReplayDisposition::Refused);
    let (mut request, events) = apply_input_events(&sent.frames[0]);
    assert!(matches!(events[0], InputEvent::Paste(_)));
    for (next_id, is_paste) in [(50, true), (60, false)] {
        journal
            .borrow_mut()
            .resolve(request, &phux_protocol::wire::frame::CommandResult::Ok)
            .expect("resolve");
        let (_, frames) = journal.borrow_mut().next_frames(&mut { next_id });
        assert_eq!(frames.len(), 1);
        let (id, events) = apply_input_events(&frames[0]);
        assert_eq!(matches!(events[0], InputEvent::Paste(_)), is_paste);
        request = id;
    }
}

/// A failed send leaves the frame it failed on uncertain and rolls the
/// never-handed-over rest back to refused.
#[tokio::test]
async fn replay_send_failure_rolls_back_frames_not_handed_to_the_connection() {
    use crate::attach::input_replay::mint_input_operation_id;
    use phux_protocol::input::paste::{PasteEvent, PasteTrust};
    let journal = journal();
    for terminal in 1..=3 {
        let data = terminal.to_string().into_bytes();
        journal
            .borrow_mut()
            .submit(
                mint_input_operation_id(),
                tid(terminal),
                vec![InputEvent::Paste(PasteEvent {
                    trust: PasteTrust::Trusted,
                    data,
                })],
            )
            .expect("queue independent terminal");
    }
    let (_, frames) = journal.borrow_mut().next_frames(&mut 1);
    assert_eq!(frames.len(), 3);
    let (stream, peer) = tokio::net::UnixStream::pair().expect("uds pair");
    drop(peer);
    let mut conn = crate::attach::connection::Connection::from_stream(stream);
    send_replay_frames(&mut conn, &journal, &frames)
        .await
        .expect_err("A send fails");
    let dispositions: Vec<_> = journal
        .borrow_mut()
        .drain_unresolved("the connection send failed")
        .iter()
        .map(|report| report.disposition)
        .collect();
    assert_eq!(
        dispositions,
        [
            ReplayDisposition::Unknown,
            ReplayDisposition::Refused,
            ReplayDisposition::Refused
        ]
    );
}

// ---- set-pane ---------------------------------------------------------------------

/// `set-pane mouse` accepts on/off/toggle and booleans; a missing or unknown
/// value, or no focused pane, bells.
#[test]
fn set_pane_mouse_updates_the_opt_out() {
    let mut f = fx(crate::layout::ArrangementDoc::single(tid(1)));
    let set = |value: toml::Value| act("set-pane", &[("mouse", value)]);
    for (value, opted_out) in [
        ("off".into(), true),
        ("on".into(), false),
        ("toggle".into(), true),
        ("toggle".into(), false),
        (false.into(), true),
        (true.into(), false),
    ] {
        let effects = f.run(&set(value));
        assert!(!effects.bell);
        assert_eq!(f.mouse_optout.contains(&tid(1)), opted_out);
    }
    assert!(f.run(&bare_action("set-pane")).bell);
    assert!(f.run(&set("sideways".into())).bell);
    assert!(f.mouse_optout.is_empty());

    let mut f = fx(crate::layout::ArrangementDoc::default());
    assert!(f.run(&set("off".into())).bell);
    assert!(f.mouse_optout.is_empty());
}

// ---- predictive echo alt-screen gate --------------------------------------------------

/// The gate reads DEC private modes: main screen predicts; `?1049h` and the
/// legacy `?1047h` are app mode; `?1049l` returns to the main screen.
#[test]
fn alt_screen_gate_tracks_dec_private_modes() {
    let terminal = |vt: &[u8]| {
        let mut terminal = libghostty_vt::Terminal::new(80, 24).expect("terminal");
        terminal
            .set_scrollback_max_lines(Some(100))
            .expect("terminal");
        terminal.vt_write(vt);
        terminal
    };
    for (vt, alt) in [
        (&b""[..], false),
        (b"\x1b[?1049h", true),
        (b"\x1b[?1049h\x1b[?1049l", false),
        (b"\x1b[?1047h", true),
    ] {
        assert_eq!(terminal_in_alt_screen(&terminal(vt)), alt, "{vt:?}");
    }
}

/// Through the real dispatch path a printable key queues one prediction in
/// both screens; on the primary screen it displays at once, in an alt-screen
/// app it stays hidden until an echo confirms (ADR-0090).
#[tokio::test]
async fn dispatch_predicts_but_gates_display_in_alt_screen_apps() {
    for alt_screen in [false, true] {
        let vt: &[u8] = if alt_screen { b"\x1b[?1049h" } else { b"" };
        let mut env = Env::new(CtxFixture::default()).published(&[(&tid(1), 80, 24, vt)]);
        env.predict = PredictionState::new(PredictiveConfig::enabled(), 80, 24);
        env.dispatch(vec![press(PhysicalKey::A, Some("a"))]).await;
        assert_eq!(env.predict.pending_len(), 1);
        assert_eq!(env.predict.should_display(predict_now_ms()), !alt_screen);
        if alt_screen {
            assert!(!env.predict.echo_confirmed());
        }
    }
}
