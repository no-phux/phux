//! Fresh authority and key validation must precede any terminal acquisition.
use super::*;
use phux_protocol::caps::{
    BootstrapLimits, BootstrapProfile, Layer, LayerSet, ServerCapabilities, ServerFeatureExtSet,
};
use phux_protocol::ids::{AuthorityId, Revision, SessionId, WindowId};
use phux_protocol::wire::frame::{
    CommandResult, CommandValue, FrameKind, OrganizationPage, SessionRecord,
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
fn authority_refusal_precedes_all_terminal_attach_frames_even_with_a_matching_name_or_key() {
    let _dial = DeterministicDial::arm();
    let key = SessionKey::new([1; 16]).unwrap();
    let owner = AuthorityId::new([1; 16]).unwrap();
    for (actual_owner, actual_key) in [
        (AuthorityId::new([2; 16]).unwrap(), key),
        (owner, SessionKey::new([2; 16]).unwrap()),
    ] {
        let mut connection = Connection::connect_with_key(
            "/isolated-authority.sock".into(),
            "work".into(),
            Some(key),
        )
        .unwrap();
        connection.expect_authority(Some(owner));
        let client = Arc::clone(&connection.client);
        client.with_control(phux_client_runtime::control::ControlPlane::connection_opened);
        let _ = outbound(&client);
        client
            .feed(FrameKind::HelloOk {
                protocol_major: phux_protocol::PROTOCOL_VERSION.major,
                protocol_minor: phux_protocol::PROTOCOL_VERSION.minor,
                protocol_patch: phux_protocol::PROTOCOL_VERSION.patch,
                server_caps: ServerCapabilities::new()
                    .with_layers(LayerSet::with(&[Layer::L3]))
                    .with_features_ext(ServerFeatureExtSet::with(&[
                        ServerFeatureExt::Organization,
                    ])),
                server_id: vec![0xAB; 16],
                selected_profile: BootstrapProfile::SynthesizedVtRaw,
                bootstrap_limits: BootstrapLimits::default(),
            })
            .unwrap();
        let mut sent = outbound(&client);
        for frame in sent.clone() {
            let FrameKind::Command {
                request_id,
                command,
            } = frame
            else {
                continue;
            };
            let value = match command {
                Command::GetState { .. } => CommandValue::State(
                    SessionSnapshot::new(SessionId::new(1), WindowId::new(1), ResourceId::local(0))
                        .with_sessions(vec![SessionInfo::new(SessionId::new(1), "work")]),
                ),
                Command::GetOrganization { .. } => CommandValue::OrganizationPage(Box::new(
                    OrganizationPage::new(actual_owner, Revision::new(1), 0).with_sessions(vec![
                        SessionRecord::new(actual_key, "work", Revision::new(1))
                            .with_live(Some(SessionId::new(1))),
                    ]),
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
        sent.extend(outbound(&client));
        assert_eq!(
            connection.status,
            Status::Failed,
            "a changed authority or reused name is refused"
        );
        assert!(
            sent.iter().all(|frame| !matches!(
                frame,
                FrameKind::Attach { .. }
                    | FrameKind::Command {
                        command: Command::AttachResource { .. },
                        ..
                    }
            )),
            "acquired work before refusal: {sent:?}"
        );
    }
}
