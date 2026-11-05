//! Real native cards and interaction gates over the embedded document runtime.
use super::*;
use crate::app::pane_identity::PaneKind;
use crate::workspace::view_tests::documents;
use gpui::{TestAppContext, VisualTestContext};
use phux_protocol::caps::BootstrapStreamProfile;

fn fixture(cx: &mut TestAppContext) -> (gpui::AnyWindowHandle, gpui::Entity<Desk>) {
    let (handle, desk) = crate::app::overlays::palette_tests::fixture(cx);
    let connection = documents::connection();
    let mut workspace = Workspace::new("docs".into(), WorkspaceOptions::default());
    workspace.settle(&connection);
    desk.update(cx, |desk, cx| {
        desk.session = "docs".into();
        desk.link = Some(Link {
            connection,
            workspace,
        });
        cx.notify();
    });
    (handle, desk)
}

#[gpui::test]
fn native_document_card_is_read_only_before_first_frame_and_after_json_or_png(
    cx: &mut TestAppContext,
) {
    let _dial = crate::connection::DeterministicDial::arm();
    let (handle, desk) = fixture(cx);
    let cx = &mut VisualTestContext::from_window(handle, cx);
    cx.run_until_parked();
    cx.update(|window, cx| window.draw(cx).clear(cx));
    assert!(
        cx.debug_bounds("read-only-document").is_some(),
        "pending document is already a document card"
    );
    desk.update(cx, |desk, cx| {
        desk.reveal_terminal(&ResourceId::local(8));
        assert_eq!(
            desk.pane_identity(desk.focused_placement().unwrap()).kind,
            PaneKind::AccessibilityTree
        );
        assert_eq!(desk.pane_title(Some(&ResourceId::local(8))), "Native tree");
        let link = desk.link.as_mut().unwrap();
        documents::publish(
            &mut link.connection,
            8,
            BootstrapStreamProfile::AccessibilityTreeJsonV1,
            br#"{"role":"text","name":"<b>literal</b>"}"#,
        );
        cx.notify();
    });
    cx.update(|window, cx| window.draw(cx).clear(cx));
    desk.update(cx, |desk, cx| {
        let tree = ResourceId::local(8);
        let _ = desk.client().unwrap().take_outbound();
        desk.send_text("must not type");
        assert!(!desk.paste_text_to(&tree, "must not paste"));
        assert_eq!(desk.queue_paste(&tree, "must not queue"), Some(false));
        assert!(desk.destination_of(&tree).is_none());
        let placement = desk.focused_id().unwrap();
        desk.drop_paths(&placement, &[PathBuf::from("/must-not-read")], cx);
        desk.start_upload(
            &tree,
            crate::attach::UploadBatch::Paths(vec![PathBuf::from("/must-not-read")]),
            cx,
        );
        desk.scroll(Scroll::Top, cx);
        desk.host_action(HostAction::Paste, cx);
        assert!(
            desk.client().unwrap().take_outbound().is_empty(),
            "no document input/upload/history/acks"
        );
        assert!(
            desk.panes.get(&placement).is_none(),
            "no terminal entity was constructed"
        );
        let link = desk.link.as_mut().unwrap();
        documents::publish(
            &mut link.connection,
            9,
            BootstrapStreamProfile::PixelSurfacePngV1,
            &documents::png(),
        );
        desk.reveal_terminal(&ResourceId::local(9));
        assert_eq!(
            desk.pane_identity(desk.focused_placement().unwrap()).kind,
            PaneKind::Surface
        );
        cx.notify();
    });
    cx.update(|window, cx| window.draw(cx).clear(cx));
    assert!(cx.debug_bounds("read-only-document").is_some());
    cx.update(|window, cx| desk.update(cx, |desk, cx| desk.execute("goto", window, cx)));
    cx.update(|window, cx| window.draw(cx).clear(cx));
    desk.read_with(cx, |desk, _| assert_eq!(desk.modal, Modal::Goto));
}

#[gpui::test]
fn transfer_readiness_accepts_documents_but_fences_other_incarnations(cx: &mut TestAppContext) {
    let _dial = crate::connection::DeterministicDial::arm();
    let (_, desk) = fixture(cx);
    desk.update(cx, |desk, _| {
        let id = ResourceId::local(8);
        let connection = &desk.link.as_ref().unwrap().connection;
        let server = connection.server_id().unwrap();
        let record = connection.resource_record(&id).unwrap();
        assert!(desk.transfer_target_ready(&id, &server, Some(&record)));
        assert!(!desk.transfer_target_ready(&id, "other-incarnation", Some(&record)));
        let mut replaced = record.clone();
        replaced.key = phux_protocol::ids::ResourceKey::new([99; 16]).unwrap();
        assert!(!desk.transfer_target_ready(&id, &server, Some(&replaced)));
    });
}

#[gpui::test]
fn transfer_releases_only_the_source_document_view_after_destination_admission(
    cx: &mut TestAppContext,
) {
    let _dial = crate::connection::DeterministicDial::arm();
    let (_, source) = fixture(cx);
    let (_, target) = fixture(cx);
    let resource = ResourceId::local(8);
    source.update(cx, |desk, cx| {
        desk.reveal_terminal(&resource);
        let placement = desk.focused_id().unwrap();
        let _ = desk.client().unwrap().take_outbound();
        desk.finish_window_transfer(target.clone(), resource.clone(), placement, cx);
    });
    cx.run_until_parked();
    source.read_with(cx, |desk, _| {
        assert!(desk.placement_of(&resource).is_none());
        assert!(desk.link.as_ref().unwrap().connection.ready(&resource));
        assert!(
            desk.client().unwrap().take_outbound().is_empty(),
            "no producer or resource termination"
        );
    });
    target.read_with(cx, |desk, _| {
        assert!(desk.placement_of(&resource).is_some());
    });
}

#[gpui::test]
fn new_window_transfer_keeps_the_source_until_the_exact_document_is_ready(cx: &mut TestAppContext) {
    let _dial = crate::connection::DeterministicDial::arm();
    let (source_handle, source) = fixture(cx);
    let resource = ResourceId::local(8);
    source.update(cx, |desk, cx| {
        desk.reveal_terminal(&resource);
        desk.move_to_window(cx);
    });
    cx.run_until_parked();
    source.read_with(cx, |desk, _| {
        assert!(
            desk.placement_of(&resource).is_some(),
            "an unready destination never removes the source"
        )
    });
    let target_handle = cx.update(|cx| {
        cx.windows()
            .into_iter()
            .find(|handle| *handle != source_handle)
            .unwrap()
    });
    let target = target_handle
        .update(cx, |root, _, cx| {
            root.downcast::<gpui_kit::component::Root>()
                .unwrap()
                .read(cx)
                .view()
                .clone()
                .downcast::<Desk>()
                .unwrap()
        })
        .unwrap();
    target.update(cx, |desk, cx| {
        let connection = documents::connection();
        let mut workspace = Workspace::new(
            "docs".into(),
            WorkspaceOptions {
                fresh: false,
                initial_terminal: Some(resource.clone()),
            },
        );
        workspace.settle(&connection);
        desk.link = Some(Link {
            connection,
            workspace,
        });
        cx.notify();
    });
    cx.executor().advance_clock(Duration::from_millis(50));
    cx.run_until_parked();
    source.read_with(cx, |desk, _| {
        assert!(desk.placement_of(&resource).is_none())
    });
    target.read_with(cx, |desk, _| {
        assert_eq!(desk.focused_terminal(), Some(resource));
        assert!(desk.focused_view().is_none());
    });
}
