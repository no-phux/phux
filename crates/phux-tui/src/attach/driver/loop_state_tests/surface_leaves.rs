/// The visible text of `bytes`, CSI sequences stripped.
fn screen_text(bytes: &[u8]) -> String {
    let text = String::from_utf8_lossy(bytes);
    let mut out = String::new();
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\x1b' && chars.peek() == Some(&'[') {
            chars.next();
            for n in chars.by_ref() {
                if ('@'..='~').contains(&n) {
                    break;
                }
            }
        } else if c != '\x1b' {
            out.push(c);
        }
    }
    out
}

/// Rune opens a document live and places it beside pane 1. The live loop
/// learns its kind from the spawn event (no document bootstrap reaches
/// this client), so it neither attaches nor resizes the document, and the
/// placement repaints it as a placeholder.
#[tokio::test(flavor = "current_thread")]
async fn a_document_placed_live_is_never_attached_or_resized() {
    use crate::layout::{Group, LayoutNode, LayoutState, SplitDir, split_at};
    let (mut state, mut client, mut server, mut out) =
        Box::pin(bootstrapped_loop_with(ServerFeatureSet::new())).await;
    drop(frames_sent(&mut client, &mut server).await);
    let (pane, doc) = (ResourceId::local(1), ResourceId::local(5));
    let spawn = FrameKind::Event {
        terminal: Some(doc.clone()),
        event: phux_protocol::wire::frame::AgentEvent::ResourceSpawned {
            kind: phux_protocol::ResourceKind::Document,
            parent: Some(pane.clone()),
        },
        stamp: None,
    };
    state
        .handle_frame_burst(&mut client, &mut out, None, spawn)
        .await
        .expect("spawn event");
    let tree = split_at(
        &LayoutNode::Leaf(pane.clone()),
        &pane,
        &doc,
        SplitDir::Horizontal,
        0.5,
    )
    .expect("split");
    let layout = ArrangementDoc {
        groups: vec![Group::new(
            "1".to_owned(),
            LayoutState {
                tree: Some(tree),
                focus: Some(pane.clone()),
            },
        )],
        active: 0,
    };
    let placed = FrameKind::MetadataChanged {
        scope: Scope::Group(DEFAULT_GROUP_ID),
        key: layout_key(SessionId::new(1)),
        value: Some(layout.encode_cbor().expect("encode")),
        actor: None,
    };
    out.clear();
    state
        .handle_frame_burst(&mut client, &mut out, None, placed)
        .await
        .expect("placement");
    let sent = frames_sent(&mut client, &mut server).await;
    let names_doc = |frame: &FrameKind| match frame {
        FrameKind::ResizeTerminal { terminal_id, .. } => terminal_id == &doc,
        FrameKind::Command { command, .. } => format!("{command:?}").contains("Local { id: 5 }"),
        _ => false,
    };
    assert!(!sent.iter().any(names_doc), "{sent:?}");
    assert!(
        sent.iter().any(|frame| matches!(
            frame,
            FrameKind::ResizeTerminal { terminal_id, .. } if terminal_id == &pane
        )),
        "the terminal still reflows into its half: {sent:?}"
    );
    let screen = screen_text(&out);
    assert!(screen.contains("Document"), "{screen:?}");
    drop(client);
    drop(server);
}
