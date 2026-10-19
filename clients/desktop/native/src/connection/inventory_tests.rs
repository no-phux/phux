//! Browsing resolves labels using metadata reads, never acquiring terminal views.
use super::*;
use phux_protocol::caps::{
    BootstrapLimits, BootstrapProfile, Layer, LayerSet, ServerCapabilities, ServerFeatureExtSet,
};
use phux_protocol::ids::{ArrangementKey, AuthorityId, ResourceKey, Revision, SessionId, WindowId};
use phux_protocol::wire::frame::{
    ArrangementDoc, ArrangementGroup, ArrangementNode, ArrangementOwner, ArrangementValue,
    CommandResult, CommandValue, EntryRef, EntryTarget, FrameKind, OrganizationPage,
    ResourceRecord, SessionRecord,
};
use phux_protocol::wire::info::{SessionInfo, SessionSnapshot};

fn outbound(client: &Client) -> Vec<FrameKind> {
    client
        .take_outbound()
        .iter()
        .map(|bytes| FrameKind::decode(bytes).unwrap().0)
        .collect()
}

#[test]
fn inventory_fetches_group_labels_without_attaching_or_spawning() {
    let _dial = DeterministicDial::arm();
    let mut connection =
        Connection::browse(std::path::Path::new("/isolated-inventory.sock"), None).unwrap();
    let client = Arc::clone(&connection.client);
    let session = SessionKey::new([1; 16]).unwrap();
    let resource = ResourceKey::new([2; 16]).unwrap();
    let arrangement = ArrangementKey::new([3; 16]).unwrap();
    let authority = AuthorityId::new([4; 16]).unwrap();
    let terminal = ResourceId::local(7);
    client.with_control(phux_client_runtime::control::ControlPlane::connection_opened);
    let mut sent = outbound(&client);
    client
        .feed(FrameKind::HelloOk {
            protocol_major: phux_protocol::PROTOCOL_VERSION.major,
            protocol_minor: phux_protocol::PROTOCOL_VERSION.minor,
            protocol_patch: phux_protocol::PROTOCOL_VERSION.patch,
            server_caps: ServerCapabilities::new()
                .with_layers(LayerSet::with(&[Layer::L3]))
                .with_features_ext(ServerFeatureExtSet::with(&[ServerFeatureExt::Organization])),
            server_id: vec![0xAB; 16],
            selected_profile: BootstrapProfile::SynthesizedVtRaw,
            bootstrap_limits: BootstrapLimits::default(),
        })
        .unwrap();
    let initial = outbound(&client);
    for frame in &initial {
        let FrameKind::Command {
            request_id,
            command,
        } = frame
        else {
            continue;
        };
        let value = match command {
            Command::GetState { .. } => CommandValue::State(
                SessionSnapshot::new(SessionId::new(1), WindowId::new(1), terminal.clone())
                    .with_sessions(vec![SessionInfo::new(SessionId::new(1), "work")]),
            ),
            Command::GetOrganization { .. } => CommandValue::OrganizationPage(Box::new(
                OrganizationPage::new(authority, Revision::new(1), 0)
                    .with_sessions(vec![
                        SessionRecord::new(session, "work", Revision::new(1))
                            .with_live(Some(SessionId::new(1)))
                            .with_arrangement(Some(arrangement)),
                    ])
                    .with_resources(vec![
                        ResourceRecord::new(resource, session, Revision::new(1))
                            .with_live(Some(terminal.clone())),
                    ]),
            )),
            _ => continue,
        };
        client
            .feed(FrameKind::CommandResult {
                request_id: *request_id,
                result: CommandResult::OkWith(value),
            })
            .unwrap();
    }
    sent.extend(initial);
    connection.drain();
    let reads = outbound(&client);
    let request_id = reads
        .iter()
        .find_map(|frame| match frame {
            FrameKind::Command {
                request_id,
                command: Command::ArrangementGet { arrangement: key },
            } if *key == arrangement => Some(*request_id),
            _ => None,
        })
        .expect("inventory fetches the session's group labels");
    sent.extend(reads);
    client
        .feed(FrameKind::CommandResult {
            request_id,
            result: CommandResult::OkWith(CommandValue::Arrangement(Box::new(
                ArrangementValue::new(
                    arrangement,
                    ArrangementOwner::Session(session),
                    Revision::new(1),
                    ArrangementDoc {
                        groups: vec![ArrangementGroup {
                            id: [5; 16],
                            name: Some("logs".into()),
                            tree: ArrangementNode::Leaf(EntryRef {
                                entry: [6; 16],
                                target: EntryTarget::Member(resource),
                            }),
                        }],
                    },
                ),
            ))),
        })
        .unwrap();
    connection.drain();
    sent.extend(outbound(&client));
    assert_eq!(connection.group_name(&terminal), Some("logs"));
    assert!(client.attached_session().is_none());
    assert!(client.selected_session().is_none());
    assert!(
        sent.iter().all(|frame| !matches!(
            frame,
            FrameKind::Attach { .. }
                | FrameKind::SpawnResource { .. }
                | FrameKind::Command {
                    command: Command::AttachResource { .. } | Command::ResourceCreate { .. },
                    ..
                }
        )),
        "browsing must stay read-only: {sent:?}"
    );
}
