use super::*;
use phux_client_core::session::{AgentStreamInfo, agent_stream::AgentRecordMode};
use phux_client_runtime::control::{AgentPublication, AgentSessionInfo};
use phux_protocol::native_agent::{NativeCapabilities, NativeView};

#[gpui::test]
fn conversation_arriving_after_tab_switch_releases_setup_without_taking_focus(
    cx: &mut TestAppContext,
) {
    let _dial = crate::connection::DeterministicDial::arm();
    let (handle, desk) = fixture(cx, true);
    let cx = &mut gpui::VisualTestContext::from_window(handle, cx);
    cx.update(|window, cx| window.draw(cx).clear(cx));
    let (target, reader, pending) = desk.update(cx, |desk, cx| {
        let link = desk.link.as_mut().unwrap();
        let target = link.workspace.focused().unwrap().id.clone();
        link.workspace.duplicate_view(&link.connection);
        let reader = link.workspace.focused().unwrap().clone();
        let tab = &mut link.workspace.tabs[0];
        tab.root = crate::workspace::layout::remove_leaf(tab.root.clone(), &reader.id).unwrap();
        tab.focused_id = target.clone();
        link.workspace.tabs.push(crate::workspace::layout::DeskTab {
            id: "reader-tab".into(),
            title: None,
            root: crate::workspace::layout::LayoutNode::Leaf {
                placement: reader.clone(),
            },
            focused_id: reader.id.clone(),
            zoomed_id: None,
        });
        let epoch = link.connection.connection_epoch();
        let publication = AgentPublication {
            resource_id: ResourceId::local(10),
            stream: AgentStreamInfo {
                stream_id: StreamId::new(2).unwrap(),
                bootstrap_id: BootstrapId::new(2).unwrap(),
                last_seq: 0,
            },
            mode: AgentRecordMode::Replace,
            records: Vec::new(),
        };
        let info = AgentSessionInfo {
            parent: Some(ResourceId::local(7)),
            provider: Some("opencode".into()),
            ..Default::default()
        };
        link.connection
            .publish_test_conversation(&publication, &info);
        let conversation = link
            .connection
            .conversations
            .get_mut(&ResourceId::local(10))
            .unwrap();
        conversation.native = Some(NativeView {
            activation: "late-activation".into(),
            backend: "acp".into(),
            capabilities: NativeCapabilities::default(),
            connected: true,
            turn_id: None,
            provider_session_id: Some("provider-session".into()),
            questions: Vec::new(),
        });
        desk.native_launch.pending = Some(Pending {
            owner: Arc::downgrade(&link.connection.client),
            epoch,
            session: "remote-work".into(),
            session_key: Some(key()),
            terminal: ResourceId::local(7),
            agent: ResourceId::local(10),
            activation: "late-activation".into(),
            revealed: true,
        });
        desk.link
            .as_mut()
            .unwrap()
            .workspace
            .select_tab("reader-tab");
        cx.notify();
        (
            target,
            reader.id,
            desk.native_launch.pending.take().unwrap(),
        )
    });
    for _ in 0..3 {
        cx.run_until_parked();
        cx.update(|window, cx| window.draw(cx).clear(cx));
    }
    let reader_focus = cx.update(|window, cx| {
        desk.update(cx, |desk, cx| {
            // The reader owns the window's normal keyboard focus; a late
            // conversation composer must not replace this rendered handle.
            let focus = desk.focus.clone();
            focus.focus(window, cx);
            let target_tab = desk.link.as_ref().unwrap().workspace.tabs[0].id.clone();
            desk.link
                .as_mut()
                .unwrap()
                .workspace
                .select_tab(&target_tab);
            desk.native_launch.pending = Some(pending);
            desk.settle_pending_native(cx);
            // Switch after scheduling, before GPUI drains the deferred handoff.
            desk.link
                .as_mut()
                .unwrap()
                .workspace
                .select_tab("reader-tab");
            focus
        })
    });
    cx.run_until_parked();
    desk.read_with(cx, |desk, _| {
        assert!(
            !desk.native_launch.busy(),
            "an offscreen ready conversation cannot hold the setup lock"
        );
        assert!(desk.conversation_visible(&target));
        assert_eq!(
            desk.focused_placement().unwrap().id,
            reader,
            "late setup must not take focus back from the reader"
        );
    });
    cx.update(|window, _| {
        assert!(
            reader_focus.is_focused(window),
            "late setup must not steal keyboard focus"
        )
    });
}
