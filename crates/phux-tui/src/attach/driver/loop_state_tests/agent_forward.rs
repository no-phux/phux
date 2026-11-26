/// A bootstrapped loop on a server that does or does not broker ssh-agents,
/// serving `link`.
async fn loop_serving(
    supported: bool,
    link: std::sync::Arc<crate::attach::agent_forward::AgentLink>,
) -> (SessionLoop, Connection, Connection, Vec<u8>) {
    let ext: &[_] = if supported {
        &[phux_protocol::ServerFeatureExt::ForwardAgent]
    } else {
        &[]
    };
    let (mut state, mut client, server, out) = Box::pin(loop_before_drain_ext(
        ServerFeatureSet::new(),
        phux_protocol::ServerFeatureExtSet::with(ext),
    ))
    .await;
    state.set_agent_link(Some(link));
    state
        .emit_deferred_bootstrap_outbound(&mut client)
        .await
        .unwrap();
    (state, client, server, out)
}

/// The ssh-agent commands among what the client sent.
async fn agent_ops(
    client: &mut Connection,
    server: &mut Connection,
) -> Vec<phux_protocol::forward::ForwardOp> {
    let sent = frames_sent(client, server).await;
    sent.into_iter()
        .filter_map(|frame| match frame {
            FrameKind::Command {
                command: Command::Forward { op },
                ..
            } => Some(op),
            _ => None,
        })
        .collect()
}

/// An attach with `forward-agent` offers once its connection is up; a switch
/// on that connection does not offer again, and the next connection (a
/// reconnect) does. Without the feature, or a local agent, nothing is sent
/// and the attach goes on.
#[tokio::test(flavor = "current_thread")]
async fn an_attach_offers_its_agent_once_per_connection() {
    use crate::attach::agent_forward::{AgentLink, tests::local_agent};
    use phux_protocol::forward::ForwardOp;
    let dir = tempfile::tempdir().unwrap();
    let socket = local_agent(&dir);
    let link = || AgentLink::new("mini".to_owned(), Ok(socket.clone()), None);

    let first = link();
    for (link, offers) in [
        (first.clone(), vec![ForwardOp::AgentOffer]),
        (first, vec![]),
        (link(), vec![ForwardOp::AgentOffer]),
    ] {
        let (_state, mut client, mut server, _out) = Box::pin(loop_serving(true, link)).await;
        assert_eq!(agent_ops(&mut client, &mut server).await, offers);
    }
    for (supported, link) in [
        (false, link()),
        (
            true,
            AgentLink::new("mini".to_owned(), Err("no agent here".to_owned()), None),
        ),
    ] {
        let (_state, mut client, mut server, _out) = Box::pin(loop_serving(supported, link)).await;
        assert!(agent_ops(&mut client, &mut server).await.is_empty());
    }
}

/// A pane's sign request raises the question over the view, and the answer
/// goes back as `AGENT_REPLY`.
#[tokio::test(flavor = "current_thread")]
async fn a_sign_request_is_asked_in_the_attach_and_answered() {
    use crate::attach::agent_forward::{AgentLink, tests};
    use phux_protocol::forward::ForwardOp;
    let dir = tempfile::tempdir().unwrap();
    let link = AgentLink::new("mini".to_owned(), Ok(tests::local_agent(&dir)), None);
    let (mut state, mut client, mut server, mut out) =
        Box::pin(loop_serving(true, link.clone())).await;
    agent_ops(&mut client, &mut server).await;

    let request = tests::sign_request(9);
    state
        .handle_frame_burst(&mut client, &mut out, None, request)
        .await
        .unwrap();
    out.clear();
    let due = tests::due(&link).await;
    state
        .on_agent_due(&mut client, &mut out, None, due)
        .await
        .unwrap();
    let painted = crate::test_util::strip_csi(&String::from_utf8_lossy(&out));
    assert!(painted.contains("Refuse"), "{painted}");
    assert!(state.overlays.is_active());

    let refuse = crate::test_util::press(PhysicalKey::Escape);
    state.overlays.handle_key(&refuse);
    assert!(!state.overlays.is_active());
    let due = tests::due(&link).await;
    state
        .on_agent_due(&mut client, &mut out, None, due)
        .await
        .unwrap();
    let reply = ForwardOp::AgentReply {
        id: 9,
        message: vec![5],
    };
    assert_eq!(agent_ops(&mut client, &mut server).await, [reply]);
}

/// A signature waits for a question already up, and never covers it.
#[tokio::test(flavor = "current_thread")]
async fn a_sign_request_waits_for_the_question_already_up() {
    use crate::attach::agent_forward::{AgentLink, tests};
    let dir = tempfile::tempdir().unwrap();
    let link = AgentLink::new("mini".to_owned(), Ok(tests::local_agent(&dir)), None);
    let (mut state, mut client, _server, mut out) =
        Box::pin(loop_serving(true, link.clone())).await;
    let text = phux_client_core::engine::ClipboardText("held".to_owned());
    let confirm = crate::render::overlay::ConfirmOverlay::clipboard("pane 1", text, &state.settings.theme);
    state.overlays.push(Box::new(confirm));

    assert!(link.take(&tests::sign_request(9)).is_some());
    let due = tests::due(&link).await;
    state
        .on_agent_due(&mut client, &mut out, None, due)
        .await
        .unwrap();
    assert_eq!(state.overlays.depth(), 1, "the confirm stands alone");
    state.overlays.dismiss();
    state.raise_agent_question(&mut out, None);
    assert_eq!(state.overlays.depth(), 1);
    assert!(state.overlays.asking());
}

/// A link prompt and a signature question never share the screen: the
/// signature waits for the link's answer, and a link asked for while a
/// signature is up is declined.
#[tokio::test(flavor = "current_thread")]
async fn a_link_prompt_and_a_signature_take_turns() {
    use crate::attach::agent_forward::{AgentLink, tests};
    use phux_protocol::open_url::OpenUrlOp;
    use phux_protocol::wire::frame::AgentEvent;
    let link = |id| FrameKind::Event {
        terminal: Some(ResourceId::local(1)),
        event: AgentEvent::OpenUrl {
            id,
            url: "https://id.example/".to_owned(),
        },
        stamp: None,
    };
    let dir = tempfile::tempdir().unwrap();
    let agent = AgentLink::new("mini".to_owned(), Ok(tests::local_agent(&dir)), None);
    let (mut state, mut client, mut server, mut out) =
        Box::pin(loop_serving(true, agent.clone())).await;

    state.handle_frame(&mut out, link(1), None, true).unwrap();
    assert!(agent.take(&tests::sign_request(9)).is_some());
    let due = tests::due(&agent).await;
    state
        .on_agent_due(&mut client, &mut out, None, due)
        .await
        .unwrap();
    assert_eq!(state.overlays.depth(), 1, "the link stands alone");
    state.overlays.dismiss();
    out.clear();
    state.raise_agent_question(&mut out, None);
    let painted = crate::test_util::strip_csi(&String::from_utf8_lossy(&out));
    assert!(painted.contains("Use your ssh key?"), "{painted}");

    state.handle_frame(&mut out, link(2), None, true).unwrap();
    assert_eq!(state.overlays.depth(), 1, "the signature stands alone");
    state.send_open_url_answers(&mut client).await.unwrap();
    let declined: Vec<u64> = frames_sent(&mut client, &mut server)
        .await
        .into_iter()
        .filter_map(|frame| match frame {
            FrameKind::Command {
                command:
                    Command::OpenUrl {
                        op: OpenUrlOp::Reply { id, opened: false },
                        ..
                    },
                ..
            } => Some(id),
            _ => None,
        })
        .collect();
    assert_eq!(declined, [1, 2], "one dismissed, one never asked");
}
