use super::*;
use phux_protocol::ids::{AuthorityId, Revision, SessionKey};
use phux_protocol::wire::frame::{CommandValue, OrganizationPage, SessionRecord};

/// The organization authority the fixture server keeps across restarts.
const DURABLE_AUTHORITY: [u8; 16] = [0x0A; 16];
/// The durable key of the fixture's `working` session.
const WORKING_KEY: [u8; 16] = [0x01; 16];

/// Open a connection to incarnation `server_id` of a server that keeps
/// `working`'s durable authority and key across restarts, and return the
/// attach id its `ATTACH` carried. After a restart the client attaches only
/// by that exact key, once the new connection's organization seed names it;
/// it never follows the reused name or numeric id.
pub(super) fn handshake_capabilities(client: &Client, server_id: u8, supports_resume: bool) -> u32 {
    client.with_control(ControlPlane::connection_opened);
    let _ = client.take_outbound();
    let features_ext = if supports_resume {
        ServerFeatureExtSet::with(&[
            ServerFeatureExt::Organization,
            ServerFeatureExt::AgentStreamResume,
        ])
    } else {
        ServerFeatureExtSet::with(&[ServerFeatureExt::Organization])
    };
    client
        .feed(FrameKind::HelloOk {
            protocol_major: PROTOCOL_VERSION.major,
            protocol_minor: PROTOCOL_VERSION.minor,
            protocol_patch: PROTOCOL_VERSION.patch,
            server_caps: ServerCapabilities::new()
                .with_layers(LayerSet::with(&[Layer::L3]))
                .with_features(ServerFeatureSet::new())
                .with_features_ext(features_ext),
            server_id: vec![server_id; 16],
            selected_profile: Some(BootstrapProfile::SynthesizedVtRaw),
            bootstrap_limits: BootstrapLimits::default(),
        })
        .expect("HELLO_OK");
    let queued = outgoing(client);
    seed_organization(client, &queued);
    attach_id(&queued)
        .or_else(|| attach_id(&outgoing(client)))
        .expect("ATTACH queued")
}

/// Answer the connection's organization seed: `working` is live as session 1.
fn seed_organization(client: &Client, queued: &[FrameKind]) {
    let request_id = queued
        .iter()
        .find_map(|frame| match frame {
            FrameKind::Command {
                request_id,
                command: Command::GetOrganization { .. },
            } => Some(*request_id),
            _ => None,
        })
        .expect("organization seed requested");
    let working = SessionRecord::new(
        SessionKey::new(WORKING_KEY).expect("nonzero"),
        "working",
        Revision::new(1),
    )
    .with_live(Some(SessionId::new(1)));
    let page = OrganizationPage::new(
        AuthorityId::new(DURABLE_AUTHORITY).expect("nonzero"),
        Revision::new(1),
        0,
    )
    .with_sessions(vec![working]);
    client
        .feed(FrameKind::CommandResult {
            request_id,
            result: CommandResult::OkWith(CommandValue::OrganizationPage(Box::new(page))),
        })
        .expect("organization page");
}

fn attach_id(frames: &[FrameKind]) -> Option<u32> {
    frames.iter().find_map(|frame| match frame {
        FrameKind::Attach { attach_id, .. } => Some(*attach_id),
        _ => None,
    })
}
