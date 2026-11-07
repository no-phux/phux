//! Adversarial receipt, publication-order and duplicate document-close cases.
use super::documents::{
    acknowledge_subscriptions, connection, connection_with_terminal, png, publish, queue_document,
};
use super::*;
use crate::connection::documents::Content;
use phux_protocol::ResourceKind;
use phux_protocol::ids::{
    ActivationId, AuthorityId, ResourceKey, Revision, ServerInstance, SessionId, SessionKey,
};
use phux_protocol::wire::frame::{
    ActivationInfo, CommandResult, CommandValue, ExecState, OrganizationPage, ResourceRecord,
    SessionRecord,
};

fn replace_record(connection: &Connection, record: ResourceRecord) {
    let request_id = connection
        .client
        .with_control(|control| control.refresh_organization())
        .unwrap();
    let session = SessionKey::new([1; 16]).unwrap();
    let page = OrganizationPage::new(AuthorityId::new([2; 16]).unwrap(), Revision::new(2), 0)
        .with_sessions(vec![
            SessionRecord::new(session, "docs", Revision::new(2))
                .with_live(Some(SessionId::new(1))),
        ])
        .with_resources(vec![record]);
    connection
        .client
        .feed(FrameKind::CommandResult {
            request_id,
            result: CommandResult::OkWith(CommandValue::OrganizationPage(Box::new(page))),
        })
        .unwrap();
}

#[test]
fn bootstrap_before_attach_receipt_is_retained_but_hidden_until_success() {
    let _dial = DeterministicDial::arm();
    for (id, profile, bytes) in [
        (
            8,
            BootstrapStreamProfile::AccessibilityTreeJsonV1,
            br#"{"text":"static first frame"}"#.to_vec(),
        ),
        (9, BootstrapStreamProfile::PixelSurfacePngV1, png()),
    ] {
        for separate_drains in [false, true] {
            for refused in [false, true] {
                let mut connection = connection();
                let resource = ResourceId::local(id);
                assert_ne!(connection.subscribe_resource(&resource), 0);
                queue_document(&connection, id, profile, &bytes);
                if separate_drains {
                    connection.drain();
                }
                assert!(connection.document_content(&resource).is_none());
                assert!(!connection.subscription_ready(&resource));
                acknowledge_subscriptions(&mut connection, refused);
                assert_eq!(connection.subscription_ready(&resource), !refused);
                assert_eq!(
                    connection.document_content(&resource).is_some(),
                    !refused,
                    "the sole first frame must survive either drain ordering without exposing a refused subscription"
                );
            }
        }
    }
}

#[test]
fn queued_old_publication_cannot_bind_a_coalesced_key_or_activation_replacement() {
    let _dial = DeterministicDial::arm();
    for activation_change in [false, true] {
        let mut connection = connection();
        let resource = ResourceId::local(8);
        publish(
            &mut connection,
            8,
            BootstrapStreamProfile::AccessibilityTreeJsonV1,
            br#"{"text":"initial"}"#,
        );
        assert!(matches!(
            connection.document_content(&resource),
            Some(Content::Text(_))
        ));
        // Deliberately do not drain: the event carries old bytes while the
        // organization query now exposes the replacement under the same wire id.
        connection
            .client
            .feed(FrameKind::ResourceOutput {
                terminal_id: resource.clone(),
                stream_id: StreamId::new(8).unwrap(),
                bootstrap_id: BootstrapId::new(1).unwrap(),
                seq: 1,
                bytes: br#"{"text":"stale queued bytes"}"#.to_vec().into(),
            })
            .unwrap();
        let mut replacement = connection.resource_record(&resource).unwrap();
        if activation_change {
            replacement.activation = Some(ActivationInfo::new(
                ActivationId::new(2),
                ServerInstance::new([5; 16]),
                ExecState::Running,
            ));
        } else {
            replacement.key = ResourceKey::new([99; 16]).unwrap();
        }
        replace_record(&connection, replacement);
        connection.drain();
        assert!(
            connection.document_content(&resource).is_none(),
            "stale bytes must not be stamped with the replacement"
        );
        assert!(!connection.subscription_ready(&resource));
        acknowledge_subscriptions(&mut connection, false);
        assert!(
            connection.subscription_ready(&resource),
            "replacement requires a new successful subscription"
        );
        let _ = connection.client.feed(FrameKind::ResourceOutput {
            terminal_id: resource.clone(),
            stream_id: StreamId::new(8).unwrap(),
            bootstrap_id: BootstrapId::new(1).unwrap(),
            seq: 2,
            bytes: br#"{"text":"late old stream"}"#.to_vec().into(),
        });
        connection.drain();
        assert!(
            connection.document_content(&resource).is_none(),
            "a receipt does not revive the old stream"
        );
        for frame in [
            FrameKind::BootstrapBegin {
                terminal_id: resource.clone(),
                stream_id: StreamId::new(80).unwrap(),
                bootstrap_id: BootstrapId::new(2).unwrap(),
                profile: BootstrapStreamProfile::AccessibilityTreeJsonV1,
                cols: 0,
                rows: 0,
                base_seq: 0,
            },
            FrameKind::BootstrapChunk {
                terminal_id: resource.clone(),
                stream_id: StreamId::new(80).unwrap(),
                bootstrap_id: BootstrapId::new(2).unwrap(),
                chunk_seq: 0,
                payload: br#"{"text":"fresh replacement"}"#.to_vec().into(),
            },
            FrameKind::BootstrapReady {
                terminal_id: resource.clone(),
                stream_id: StreamId::new(80).unwrap(),
                bootstrap_id: BootstrapId::new(2).unwrap(),
                history_cursor: None,
            },
        ] {
            connection.client.feed(frame).unwrap();
        }
        connection.drain();
        assert!(
            matches!(connection.document_content(&resource), Some(Content::Text(text)) if text.contains("fresh replacement"))
        );
    }
}

#[test]
fn closing_a_tab_removes_every_duplicate_document_placement_without_releasing_others() {
    let _dial = DeterministicDial::arm();
    let connection = connection();
    let resource = ResourceId::local(8);
    let options = WorkspaceOptions {
        fresh: false,
        initial_terminal: Some(resource.clone()),
    };
    let mut workspace = Workspace::new("docs".into(), options.clone());
    workspace.settle(&connection);
    workspace.duplicate_view(&connection);
    let mut other = Workspace::new("docs".into(), options);
    other.settle(&connection);
    assert_eq!(workspace.views_of(&resource), 2);
    let _ = connection.client.take_outbound();
    workspace.terminate_tab(&connection, &workspace.active_id.clone());
    assert!(workspace.tabs.is_empty());
    assert_eq!(other.views_of(&resource), 1);
    assert!(connection.ready(&resource));
    assert!(
        connection.client.take_outbound().is_empty(),
        "view removal must not stop/detach the producer or another viewer"
    );
}

#[test]
fn mixed_tab_close_removes_duplicate_documents_even_while_terminal_termination_is_pending() {
    let _dial = DeterministicDial::arm();
    let connection = connection_with_terminal(true);
    let mut workspace = Workspace::new("docs".into(), Default::default());
    workspace.settle(&connection);
    let tab = workspace.active_id.clone();
    let target = workspace.focused().unwrap().id.clone();
    workspace.land(
        &connection,
        &terminal_label(&ResourceId::local(8)),
        Destination::Split {
            tab_id: tab.clone(),
            target_id: target,
            axis: Axis::Row,
            before: false,
        },
    );
    workspace.duplicate_view(&connection);
    assert_eq!(
        workspace.views_of(&ResourceId::local(8)),
        3,
        "one background tab and two split placements"
    );
    workspace.terminate_tab(&connection, &tab);
    assert_eq!(
        workspace.views_of(&ResourceId::local(8)),
        1,
        "the background viewer is not in the closed tab"
    );
    assert_eq!(workspace.views_of(&ResourceId::local(9)), 1);
    assert_eq!(
        workspace.views_of(&ResourceId::local(7)),
        1,
        "terminal view waits for its termination receipt"
    );
    assert_eq!(
        connection.document_kind(&ResourceId::local(8)),
        Some(ResourceKind::AccessibilityTree)
    );
}
