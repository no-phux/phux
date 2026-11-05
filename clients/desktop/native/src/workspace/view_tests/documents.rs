//! Document-only layouts use organization identity, not terminal inventory or pixels.
use super::*;
use crate::connection::documents::Content;
use crate::settings::DisplayPrefs;
use phux_client_runtime::control::ControlPlane;
use phux_protocol::ResourceKind;
use phux_protocol::caps::{
    BootstrapLimits, BootstrapProfile, ServerCapabilities, ServerFeatureExt, ServerFeatureExtSet,
};
use phux_protocol::ids::{AuthorityId, ResourceKey, Revision, SessionId, SessionKey, WindowId};
use phux_protocol::wire::frame::{
    CommandResult, CommandValue, OrganizationPage, ResourceRecord, SessionRecord,
};
use phux_protocol::wire::info::{ResourceInfo, SessionInfo, SessionSnapshot, WindowInfo};

pub(crate) fn connection() -> Connection {
    connection_with_terminal(false)
}

fn connection_with_terminal(mixed: bool) -> Connection {
    let mut connection =
        Connection::browse(std::path::Path::new("/isolated-documents.sock"), None).unwrap();
    let client = connection.client.clone();
    client.with_control(ControlPlane::connection_opened);
    let _ = client.take_outbound();
    client
        .feed(FrameKind::HelloOk {
            protocol_major: phux_protocol::PROTOCOL_VERSION.major,
            protocol_minor: phux_protocol::PROTOCOL_VERSION.minor,
            protocol_patch: phux_protocol::PROTOCOL_VERSION.patch,
            server_caps: ServerCapabilities::new().with_features_ext(ServerFeatureExtSet::with(&[
                ServerFeatureExt::Organization,
                ServerFeatureExt::AccessibilityTree,
                ServerFeatureExt::PixelSurface,
            ])),
            server_id: vec![1; 16],
            selected_profile: Some(BootstrapProfile::SynthesizedVtRaw),
            bootstrap_limits: BootstrapLimits::default(),
        })
        .unwrap();
    let session = SessionKey::new([1; 16]).unwrap();
    let authority = AuthorityId::new([2; 16]).unwrap();
    for bytes in client.take_outbound() {
        let FrameKind::Command {
            request_id,
            command,
        } = FrameKind::decode(&bytes).unwrap().0
        else {
            continue;
        };
        let value = match command {
            Command::GetState { .. } => CommandValue::State(
                SessionSnapshot::new(SessionId::new(1), WindowId::new(1), ResourceId::local(0))
                    .with_sessions(vec![SessionInfo::new(SessionId::new(1), "docs")])
                    .with_windows(if mixed {
                        vec![WindowInfo::new(
                            WindowId::new(1),
                            SessionId::new(1),
                            "shell",
                        )]
                    } else {
                        vec![]
                    })
                    .with_resources(if mixed {
                        vec![ResourceInfo::new(
                            ResourceId::local(7),
                            WindowId::new(1),
                            20,
                            4,
                        )]
                    } else {
                        vec![]
                    }),
            ),
            Command::GetOrganization { .. } => CommandValue::OrganizationPage(Box::new(
                OrganizationPage::new(authority, Revision::new(1), 0)
                    .with_sessions(vec![
                        SessionRecord::new(session, "docs", Revision::new(1))
                            .with_live(Some(SessionId::new(1))),
                    ])
                    .with_resources(
                        [
                            ResourceRecord::new(
                                ResourceKey::new([3; 16]).unwrap(),
                                session,
                                Revision::new(1),
                            )
                            .with_kind(ResourceKind::AccessibilityTree)
                            .with_name(Some("Native tree".into()))
                            .with_live(Some(ResourceId::local(8))),
                            ResourceRecord::new(
                                ResourceKey::new([4; 16]).unwrap(),
                                session,
                                Revision::new(1),
                            )
                            .with_kind(ResourceKind::PixelSurface)
                            .with_name(Some("Native pixels".into()))
                            .with_live(Some(ResourceId::local(9))),
                        ]
                        .into_iter()
                        .chain(mixed.then(|| {
                            ResourceRecord::new(
                                ResourceKey::new([5; 16]).unwrap(),
                                session,
                                Revision::new(1),
                            )
                            .with_live(Some(ResourceId::local(7)))
                        }))
                        .collect(),
                    ),
            )),
            _ => continue,
        };
        client
            .feed(FrameKind::CommandResult {
                request_id,
                result: CommandResult::OkWith(value),
            })
            .unwrap();
    }
    connection.drain();
    assert!(connection.activate_home("docs".into(), Some(session), Some(authority)));
    connection.drain();
    assert_eq!(connection.status, Status::Attached);
    if mixed {
        admit_terminal(&mut connection);
    }
    connection
}

fn admit_terminal(connection: &mut Connection) {
    let client = connection.client.clone();
    for bytes in client.take_outbound() {
        if let FrameKind::Command {
            request_id,
            command: Command::AttachResource { .. },
        } = FrameKind::decode(&bytes).unwrap().0
        {
            client
                .feed(FrameKind::CommandResult {
                    request_id,
                    result: CommandResult::Ok,
                })
                .unwrap();
        }
    }
    let stream_id = StreamId::new(7).unwrap();
    let bootstrap_id = BootstrapId::new(1).unwrap();
    for frame in [
        FrameKind::BootstrapBegin {
            terminal_id: ResourceId::local(7),
            stream_id,
            bootstrap_id,
            profile: BootstrapStreamProfile::SynthesizedVtRaw,
            cols: 20,
            rows: 4,
            base_seq: 0,
        },
        FrameKind::BootstrapReady {
            terminal_id: ResourceId::local(7),
            stream_id,
            bootstrap_id,
            history_cursor: None,
        },
    ] {
        client.feed(frame).unwrap();
    }
    connection.drain();
    assert!(
        connection.pane(&ResourceId::local(7)).is_some(),
        "mixed fixture inventories its terminal"
    );
    assert!(
        connection.ready(&ResourceId::local(7)),
        "mixed fixture bootstraps and admits its terminal"
    );
}

pub(crate) fn publish(
    connection: &mut Connection,
    id: u32,
    profile: BootstrapStreamProfile,
    bytes: &[u8],
) {
    let resource = ResourceId::local(id);
    let stream_id = StreamId::new(u64::from(id)).unwrap();
    let bootstrap_id = BootstrapId::new(1).unwrap();
    for frame in [
        FrameKind::BootstrapBegin {
            terminal_id: resource.clone(),
            stream_id,
            bootstrap_id,
            profile,
            cols: 0,
            rows: 0,
            base_seq: 0,
        },
        FrameKind::BootstrapChunk {
            terminal_id: resource.clone(),
            stream_id,
            bootstrap_id,
            chunk_seq: 0,
            payload: bytes.to_vec().into(),
        },
        FrameKind::BootstrapReady {
            terminal_id: resource,
            stream_id,
            bootstrap_id,
            history_cursor: None,
        },
    ] {
        connection.client.feed(frame).unwrap();
    }
    connection.drain();
}

pub(crate) fn png() -> Vec<u8> {
    let image = image::RgbaImage::from_pixel(3, 2, image::Rgba([17, 80, 200, 255]));
    let mut bytes = std::io::Cursor::new(Vec::new());
    image.write_to(&mut bytes, image::ImageFormat::Png).unwrap();
    bytes.into_inner()
}

#[test]
fn empty_documents_are_discovered_opened_duplicated_restored_and_closed_without_terminal_views() {
    let _dial = DeterministicDial::arm();
    let connection = connection();
    assert!(
        connection.panes().is_empty(),
        "topology intentionally remains terminal-only"
    );
    assert_eq!(connection.documents_in("docs").len(), 2);
    assert!(connection.ready(&ResourceId::local(8)));
    assert_eq!(
        connection.document_label(&ResourceId::local(8)).as_deref(),
        Some("Native tree")
    );
    let mut direct = Workspace::new(
        "docs".into(),
        WorkspaceOptions {
            fresh: true,
            initial_terminal: Some(ResourceId::local(9)),
        },
    );
    direct.settle(&connection);
    assert_eq!(
        direct.tabs.len(),
        1,
        "direct open is not substituted with a shell"
    );
    assert_eq!(
        direct.focused().unwrap().terminal_id,
        Some(ResourceId::local(9))
    );
    assert!(direct.focused().unwrap().view_id.is_none());
    let mut workspace = Workspace::new("docs".into(), WorkspaceOptions::default());
    workspace.settle(&connection);
    assert_eq!(workspace.tabs.len(), 2);
    assert!(leaves(&workspace).iter().all(|item| item.view_id.is_none()));
    assert!(workspace.unresolved.is_empty());
    workspace.open_resource(&connection, &ResourceId::local(9));
    workspace.duplicate_view(&connection);
    assert_eq!(workspace.views_of(&ResourceId::local(9)), 2);
    let saved = snapshot_layout(&workspace, &connection, &DisplayPrefs::default()).unwrap();
    let _ = connection.client.take_outbound();
    let mut restored = Workspace::new("docs".into(), WorkspaceOptions::default());
    restored.restore(Some(saved));
    restored.settle(&connection);
    assert!(restored.unresolved.is_empty());
    assert!(leaves(&restored).iter().all(|item| item.view_id.is_none()));
    let id = restored.focused().unwrap().id.clone();
    restored.terminate_pane(&connection, &id);
    assert_eq!(restored.views_of(&ResourceId::local(9)), 1);
    assert!(connection.ready(&ResourceId::local(9)));
    for tab in restored.tabs.clone() {
        restored.terminate_tab(&connection, &tab.id);
    }
    assert!(restored.tabs.is_empty());
    assert!(
        connection
            .client
            .take_outbound()
            .iter()
            .all(|bytes| !releases_terminal(&FrameKind::decode(bytes).unwrap().0))
    );
}

#[test]
fn mixed_terminal_and_document_splits_restore_without_converting_documents_to_terminals() {
    let _dial = DeterministicDial::arm();
    let connection = connection_with_terminal(true);
    let mut workspace = Workspace::new("docs".into(), WorkspaceOptions::default());
    workspace.settle(&connection);
    assert_eq!(workspace.tabs.len(), 3);
    let target = workspace.focused().unwrap().id.clone();
    let tab = workspace.active_id.clone();
    workspace.land(
        &connection,
        &terminal_label(&ResourceId::local(9)),
        Destination::Split {
            tab_id: tab,
            target_id: target,
            axis: Axis::Row,
            before: false,
        },
    );
    assert_eq!(placements(&workspace.active_tab().unwrap().root).len(), 2);
    let saved = snapshot_layout(&workspace, &connection, &DisplayPrefs::default()).unwrap();
    let mut restored = Workspace::new("docs".into(), WorkspaceOptions::default());
    restored.restore(Some(saved));
    restored.settle(&connection);
    for placement in leaves(&restored) {
        assert_eq!(
            placement.view_id.is_some(),
            placement.terminal_id == Some(ResourceId::local(7))
        );
    }
}

#[test]
fn whole_json_and_real_png_are_native_documents_and_invalid_new_frames_do_not_show_stale_pixels() {
    let _dial = DeterministicDial::arm();
    let mut connection = connection();
    let mut workspace = Workspace::new("docs".into(), WorkspaceOptions::default());
    workspace.settle(&connection);
    publish(
        &mut connection,
        8,
        BootstrapStreamProfile::AccessibilityTreeJsonV1,
        br#"{"text":"<script>inert</script>"}"#,
    );
    assert!(
        matches!(connection.document_content(&ResourceId::local(8)), Some(Content::Text(text)) if text.contains("<script>inert</script>"))
    );
    publish(
        &mut connection,
        9,
        BootstrapStreamProfile::PixelSurfacePngV1,
        &png(),
    );
    assert!(matches!(
        connection.document_content(&ResourceId::local(9)),
        Some(Content::Image(_))
    ));
    let _ = connection.client.take_outbound();
    connection
        .client
        .feed(FrameKind::ResourceOutput {
            terminal_id: ResourceId::local(9),
            stream_id: StreamId::new(9).unwrap(),
            bootstrap_id: BootstrapId::new(1).unwrap(),
            seq: 1,
            bytes: b"invalid PNG".to_vec().into(),
        })
        .unwrap();
    connection.drain();
    assert!(matches!(
        connection.document_content(&ResourceId::local(9)),
        Some(Content::Invalid(_))
    ));
    assert!(
        connection.client.take_outbound().is_empty(),
        "no grid resize/history/frame acknowledgements for documents"
    );
    connection.client.with_control(|control| {
        control.connection_lost(None);
        control.connection_opened();
    });
    connection.drain();
    assert!(
        connection.documents.is_empty(),
        "epoch boundary clears content"
    );
    let _ = connection.client.feed(FrameKind::ResourceOutput {
        terminal_id: ResourceId::local(9),
        stream_id: StreamId::new(9).unwrap(),
        bootstrap_id: BootstrapId::new(1).unwrap(),
        seq: 2,
        bytes: png().into(),
    });
    connection.drain();
    assert!(
        connection.documents.is_empty(),
        "old-epoch output cannot repopulate the document"
    );
}

#[test]
fn replaced_organization_identity_cannot_reuse_cached_document_content() {
    let _dial = DeterministicDial::arm();
    let mut connection = connection();
    publish(
        &mut connection,
        9,
        BootstrapStreamProfile::PixelSurfacePngV1,
        &png(),
    );
    assert!(connection.document_content(&ResourceId::local(9)).is_some());
    let request_id = connection
        .client
        .with_control(|control| control.refresh_organization())
        .unwrap();
    let session = SessionKey::new([1; 16]).unwrap();
    connection
        .client
        .feed(FrameKind::CommandResult {
            request_id,
            result: CommandResult::OkWith(CommandValue::OrganizationPage(Box::new(
                OrganizationPage::new(AuthorityId::new([2; 16]).unwrap(), Revision::new(2), 0)
                    .with_sessions(vec![
                        SessionRecord::new(session, "docs", Revision::new(2))
                            .with_live(Some(SessionId::new(1))),
                    ])
                    .with_resources(vec![
                        ResourceRecord::new(
                            ResourceKey::new([99; 16]).unwrap(),
                            session,
                            Revision::new(2),
                        )
                        .with_kind(ResourceKind::PixelSurface)
                        .with_live(Some(ResourceId::local(9))),
                    ]),
            ))),
        })
        .unwrap();
    connection.drain();
    assert!(connection.document_content(&ResourceId::local(9)).is_none());
}
