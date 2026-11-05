//! Real native cards and interaction gates over the embedded document runtime.
use super::*;
use crate::app::pane_identity::PaneKind;
use crate::workspace::view_tests::documents;
use gpui::{TestAppContext, VisualTestContext};
use phux_protocol::caps::BootstrapStreamProfile;

fn fixture(cx: &mut TestAppContext) -> (gpui::AnyWindowHandle, gpui::Entity<Desk>) {
    fixture_with_subscription(cx, true)
}

fn fixture_with_subscription(
    cx: &mut TestAppContext,
    accepted: bool,
) -> (gpui::AnyWindowHandle, gpui::Entity<Desk>) {
    let (handle, desk) = crate::app::overlays::palette_tests::fixture(cx);
    let mut connection = documents::connection();
    let mut workspace = Workspace::new("docs".into(), WorkspaceOptions::default());
    workspace.settle(&connection);
    if accepted {
        documents::acknowledge_subscriptions(&mut connection, false);
    }
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
fn withheld_destination_subscription_keeps_source_until_success(cx: &mut TestAppContext) {
    let _dial = crate::connection::DeterministicDial::arm();
    let (_, source) = fixture(cx);
    let (_, target) = fixture_with_subscription(cx, false);
    let resource = ResourceId::local(8);
    source.update(cx, |desk, cx| {
        desk.reveal_terminal(&resource);
        desk.finish_window_transfer(
            target.clone(),
            resource.clone(),
            desk.focused_id().unwrap(),
            cx,
        );
    });
    cx.run_until_parked();
    source.read_with(cx, |desk, _| {
        assert!(desk.placement_of(&resource).is_some())
    });
    target.update(cx, |desk, _| {
        documents::acknowledge_subscriptions(&mut desk.link.as_mut().unwrap().connection, false)
    });
    cx.executor().advance_clock(Duration::from_millis(50));
    cx.run_until_parked();
    source.read_with(cx, |desk, _| {
        assert!(desk.placement_of(&resource).is_none())
    });
}

#[gpui::test]
fn refused_destination_subscription_keeps_source(cx: &mut TestAppContext) {
    let _dial = crate::connection::DeterministicDial::arm();
    let (_, source) = fixture(cx);
    let (_, target) = fixture_with_subscription(cx, false);
    let resource = ResourceId::local(8);
    source.update(cx, |desk, cx| {
        desk.reveal_terminal(&resource);
        desk.finish_window_transfer(
            target.clone(),
            resource.clone(),
            desk.focused_id().unwrap(),
            cx,
        );
    });
    target.update(cx, |desk, _| {
        documents::acknowledge_subscriptions(&mut desk.link.as_mut().unwrap().connection, true)
    });
    cx.run_until_parked();
    source.read_with(cx, |desk, _| {
        assert!(desk.placement_of(&resource).is_some())
    });
    target.read_with(cx, |desk, _| {
        assert!(
            !desk
                .link
                .as_ref()
                .unwrap()
                .connection
                .subscription_ready(&resource)
        )
    });
}

#[gpui::test]
fn retained_terminal_view_is_not_transfer_admission_after_connection_loss(cx: &mut TestAppContext) {
    let _dial = crate::connection::DeterministicDial::arm();
    let (_, desk) = fixture(cx);
    let mut connection = crate::workspace::view_tests::attached();
    let workspace = crate::workspace::view_tests::opened(&mut connection);
    desk.update(cx, |desk, _| {
        desk.link = Some(Link {
            connection,
            workspace,
        });
        let resource = ResourceId::local(7);
        let server = desk.link.as_ref().unwrap().connection.server_id().unwrap();
        assert!(desk.transfer_target_ready(&resource, &server, None));
        desk.client().unwrap().with_control(|control| {
            control.connection_lost(None);
            control.connection_opened();
        });
        assert!(
            desk.focused_view().is_some(),
            "retained paint survives reconnect"
        );
        assert!(!desk.transfer_target_ready(&resource, &server, None));
        reconnect_without_bootstrap(&mut desk.link.as_mut().unwrap().connection);
        assert_ne!(desk.client().unwrap().status(), Status::Attached);
        assert!(desk.focused_view().is_some());
        assert!(
            !desk.transfer_target_ready(&resource, &server, None),
            "a new attach awaiting bootstrap does not admit a retained terminal replica"
        );
    });
}

fn reconnect_without_bootstrap(connection: &mut Connection) {
    use phux_protocol::{
        caps::{BootstrapLimits, BootstrapProfile, ServerCapabilities},
        ids::{ClientId, SessionId, WindowId},
        wire::{
            frame::FrameKind,
            info::{ResourceInfo, SessionInfo, SessionSnapshot, WindowInfo},
        },
    };
    let client = &connection.client;
    let _ = client.take_outbound();
    client
        .feed(FrameKind::HelloOk {
            protocol_major: phux_protocol::PROTOCOL_VERSION.major,
            protocol_minor: phux_protocol::PROTOCOL_VERSION.minor,
            protocol_patch: phux_protocol::PROTOCOL_VERSION.patch,
            server_caps: ServerCapabilities::new(),
            server_id: vec![1; 16],
            selected_profile: Some(BootstrapProfile::SynthesizedVtRaw),
            bootstrap_limits: BootstrapLimits::default(),
        })
        .unwrap();
    let attach_id = client
        .take_outbound()
        .iter()
        .find_map(|bytes| match FrameKind::decode(bytes).unwrap().0 {
            FrameKind::Attach { attach_id, .. } => Some(attach_id),
            _ => None,
        })
        .unwrap();
    let snapshot = SessionSnapshot::new(SessionId::new(1), WindowId::new(1), ResourceId::local(7))
        .with_sessions(vec![SessionInfo::new(SessionId::new(1), "views")])
        .with_windows(vec![WindowInfo::new(
            WindowId::new(1),
            SessionId::new(1),
            "shell",
        )])
        .with_resources(vec![ResourceInfo::new(
            ResourceId::local(7),
            WindowId::new(1),
            20,
            4,
        )]);
    client
        .feed(FrameKind::Attached {
            attach_id,
            snapshot,
            initial_client_id: ClientId::new(1),
        })
        .unwrap();
    connection.drain();
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
        let mut connection = documents::connection();
        let mut workspace = Workspace::new(
            "docs".into(),
            WorkspaceOptions {
                fresh: false,
                initial_terminal: Some(resource.clone()),
            },
        );
        workspace.settle(&connection);
        documents::acknowledge_subscriptions(&mut connection, false);
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
