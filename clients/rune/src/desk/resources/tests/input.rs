//! A surface whose producer declared a control class takes pane input,
//! and trades clipboards with its host (Cmd+V, Cmd+C).

use super::*;

#[test]
fn a_controlled_surface_takes_a_click_and_keys_until_refused() {
    use phux_protocol::wire::frame::{ControlClass, DocumentInput, DocumentKey, ErrorCode};
    let (client, mut rt) = fixture(
        &[record(1, ResourceKind::PixelSurface)],
        &doc(LayoutNode::Leaf(id(1))),
    );
    publish(&client, 1, ResourceKind::PixelSurface, PNG);
    step(&mut rt);
    let Body::Surface(image) = &rt.state.resources.entries[&id(1)].body else {
        panic!("decoding the surface");
    };
    assert_eq!(image.wait(), Ok([4, 2]));
    step(&mut rt);
    assert_eq!(
        rt.state.document_input_frame(&id(1)),
        None,
        "no control class"
    );
    // The producer declared `page`, as a page mirror does.
    let request = client.refresh_topology().unwrap();
    outbound(&client);
    let surface = ResourceInfo::new(id(1), WindowId::new(1), 0, 0)
        .with_kind(ResourceKind::PixelSurface)
        .with_session(Some(SessionId::new(1)))
        .with_control(Some(ControlClass::Page));
    let snapshot = SessionSnapshot::new(SessionId::new(1), WindowId::new(1), id(1))
        .with_sessions(vec![
            SessionInfo::new(SessionId::new(1), "work").with_key(Some(session())),
        ])
        .with_resources(vec![surface]);
    client
        .feed(FrameKind::CommandResult {
            request_id: request,
            result: CommandResult::OkWith(CommandValue::State(snapshot)),
        })
        .unwrap();
    step(&mut rt);
    assert_eq!(rt.state.document_input_frame(&id(1)), Some([4, 2]));
    outbound(&client);
    let board = rune::clipboard::memory();
    board.set_text("from this machine");
    rt.use_clipboard(&board);
    // 4x2 contain-fit in 400x100 is 200x100 from x = 100.
    rt.update(|d, _| {
        d.document_press(&id(1), [250.0, 75.0, 400.0, 100.0]);
        d.document_press(&id(1), [50.0, 50.0, 400.0, 100.0]);
    });
    for (key, mods) in [
        (Key::Text("h".into()), Mods::default()),
        (Key::Enter, Mods::default()),
        (
            Key::Command('c'),
            Mods {
                cmd: true,
                ..Mods::default()
            },
        ),
        (
            Key::Command('v'),
            Mods {
                cmd: true,
                ..Mods::default()
            },
        ),
    ] {
        rt.key(KeyEvent { key, mods });
    }
    step(&mut rt);
    let sent: Vec<_> = outbound(&client)
        .into_iter()
        .filter_map(|frame| match frame {
            FrameKind::Command {
                request_id,
                command: Command::SendDocumentInput { intent, .. },
            } => Some((request_id, intent)),
            _ => None,
        })
        .collect();
    let intents: Vec<_> = sent.iter().map(|(_, intent)| intent.clone()).collect();
    assert_eq!(
        intents,
        [
            DocumentInput::Click { x: 3, y: 1 },
            DocumentInput::Type { text: "h".into() },
            DocumentInput::Key {
                key: DocumentKey::Enter
            },
            DocumentInput::CopyClipboard { reply: 0 },
            DocumentInput::Paste {
                text: "from this machine".into(),
                chord: true,
            },
        ],
        "the letterbox press sends nothing; Cmd+C and Cmd+V trade clipboards"
    );
    // The host's clipboard lands on this machine's.
    let (copy, _) = sent[3].clone();
    client
        .feed(FrameKind::CommandResult {
            request_id: copy,
            result: CommandResult::OkWith(CommandValue::Bytes(b"from the host".to_vec())),
        })
        .unwrap();
    step(&mut rt);
    assert_eq!(
        board.text().as_deref().map(String::as_str),
        Some("from the host")
    );
    for (request_id, _) in sent.into_iter().filter(|(id, _)| *id != copy) {
        client
            .feed(FrameKind::CommandResult {
                request_id,
                result: CommandResult::Error {
                    code: ErrorCode::NotProducer,
                    message: "the surface's producer is not connected".into(),
                },
            })
            .unwrap();
    }
    step(&mut rt);
    let notice = rt.state.notice.as_ref().expect("the refusal is said");
    assert!(notice.text.contains("producer is not connected"));
    assert_eq!(rt.state.document_input_frame(&id(1)), None, "view only now");
    rt.key(KeyEvent {
        key: Key::Text("x".into()),
        mods: Mods::default(),
    });
    step(&mut rt);
    let sent = outbound(&client);
    assert!(
        !sent.iter().any(|frame| matches!(
            frame,
            FrameKind::Command {
                command: Command::SendDocumentInput { .. },
                ..
            }
        )),
        "a view-only pane sends nothing"
    );
}
