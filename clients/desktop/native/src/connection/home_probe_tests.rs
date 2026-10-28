//! Bootstrap ordering: Intake must win even when the live graph arrives first.
use super::*;
use phux_protocol::ResourceId;
use phux_protocol::caps::{
    BootstrapLimits, BootstrapProfile, Layer, LayerSet, ServerCapabilities, ServerFeatureExtSet,
};
use phux_protocol::ids::{AuthorityId, Revision, SessionId, WindowId};
use phux_protocol::wire::frame::{
    Command, CommandResult, CommandValue, FrameKind, OrganizationPage, SessionRecord,
};
use phux_protocol::wire::info::{SessionInfo, SessionSnapshot};

fn browsing(organization: bool) -> (ControlPlane, Option<u32>) {
    let mut control = ControlPlane::new(ControlOptions::default());
    let seed = negotiate(&mut control, organization);
    (control, seed)
}

fn negotiate(control: &mut ControlPlane, organization: bool) -> Option<u32> {
    control.connection_opened();
    let _ = control.take_outbound();
    control
        .feed(FrameKind::HelloOk {
            protocol_major: phux_protocol::PROTOCOL_VERSION.major,
            protocol_minor: phux_protocol::PROTOCOL_VERSION.minor,
            protocol_patch: phux_protocol::PROTOCOL_VERSION.patch,
            server_caps: ServerCapabilities::new()
                .with_layers(LayerSet::with(&[Layer::L3]))
                .with_features_ext(if organization {
                    ServerFeatureExtSet::with(&[ServerFeatureExt::Organization])
                } else {
                    ServerFeatureExtSet::new()
                }),
            server_id: vec![0xab; 16],
            selected_profile: BootstrapProfile::SynthesizedVtRaw,
            bootstrap_limits: BootstrapLimits::default(),
        })
        .unwrap();
    let mut seed = None;
    for bytes in control.take_outbound() {
        if let FrameKind::Command {
            request_id,
            command,
        } = FrameKind::decode(&bytes).unwrap().0
        {
            match command {
                Command::GetState { .. } => control
                    .feed(FrameKind::CommandResult {
                        request_id,
                        result: CommandResult::OkWith(CommandValue::State(
                            SessionSnapshot::new(
                                SessionId::new(2),
                                WindowId::new(2),
                                ResourceId::local(2),
                            )
                            .with_sessions(vec![SessionInfo::new(
                                SessionId::new(2),
                                "windows-desktop-smoke",
                            )]),
                        )),
                    })
                    .unwrap(),
                Command::GetOrganization { .. } => seed = Some(request_id),
                _ => {}
            }
        }
    }
    seed
}

fn seed_intake(control: &mut ControlPlane, request_id: u32) {
    control.feed(intake_seed(request_id)).unwrap();
}

fn intake_seed(request_id: u32) -> FrameKind {
    let key = SessionKey::new([1; 16]).unwrap();
    FrameKind::CommandResult {
        request_id,
        result: CommandResult::OkWith(CommandValue::OrganizationPage(Box::new(
            OrganizationPage::new(AuthorityId::new([2; 16]).unwrap(), Revision::new(1), 0)
                .with_intake(Some(key))
                // Intake may be cold and absent from the live topology.
                .with_sessions(vec![SessionRecord::new(key, "intake", Revision::new(1))]),
        ))),
    }
}

#[test]
fn topology_before_seed_cannot_select_the_recent_session_as_home() {
    let (mut control, seed) = browsing(true);
    let before_seed = control.topology().unwrap().clone();
    assert_eq!(before_seed.inbox, None);
    assert!(ready_home(&control, None).unwrap().is_none());
    seed_intake(&mut control, seed.unwrap());
    // The old split reads could combine this stale graph with live=true and
    // take the wrong fallback. The coherent sample must use the new graph.
    assert!(control.organization().is_live());
    assert_eq!(
        choose(None, &before_seed, None),
        Home::Existing("windows-desktop-smoke".into())
    );
    let (topology, intake) = ready_home(&control, None).unwrap().unwrap();
    assert_eq!(intake.as_deref(), Some("intake"));
    assert_eq!(
        choose(None, &topology, intake.as_deref()),
        Home::Existing("intake".into())
    );
    assert_eq!(
        choose(Some("windows-desktop-smoke"), &topology, intake.as_deref()),
        Home::Existing("windows-desktop-smoke".into())
    );
}

#[test]
fn a_seeded_intake_is_selected_on_the_first_sample_too() {
    let (mut control, seed) = browsing(true);
    seed_intake(&mut control, seed.unwrap());
    let (topology, intake) = ready_home(&control, None).unwrap().unwrap();
    assert_eq!(
        choose(None, &topology, intake.as_deref()),
        Home::Existing("intake".into())
    );
}

#[test]
fn legacy_and_refused_organization_keep_the_recent_session_fallback() {
    let (legacy, _) = browsing(false);
    let (topology, intake) = ready_home(&legacy, None).unwrap().unwrap();
    assert_eq!(
        choose(None, &topology, intake.as_deref()),
        Home::Existing("windows-desktop-smoke".into())
    );
    let (refused, _) = browsing(true);
    let (topology, intake) = ready_home(&refused, Some(refused.connection_epoch()))
        .unwrap()
        .unwrap();
    assert_eq!(
        choose(None, &topology, intake.as_deref()),
        Home::Existing("windows-desktop-smoke".into())
    );
}

#[test]
fn a_closed_probe_reports_failure_instead_of_selecting_a_session() {
    let (mut control, _) = browsing(true);
    control.connection_lost(Some("test peer closed".into()));
    control.close();
    assert!(ready_home(&control, None).is_err());
}

#[test]
fn an_old_refusal_cannot_release_a_reconnected_probe_before_its_seed() {
    let (mut control, _) = browsing(true);
    let refused_epoch = control.connection_epoch();
    assert!(ready_home(&control, Some(refused_epoch)).unwrap().is_some());
    control.connection_lost(None);
    let seed = negotiate(&mut control, true).unwrap();
    assert_ne!(control.connection_epoch(), refused_epoch);
    assert!(ready_home(&control, Some(refused_epoch)).unwrap().is_none());
    seed_intake(&mut control, seed);
    let (_, intake) = ready_home(&control, Some(refused_epoch)).unwrap().unwrap();
    assert_eq!(intake.as_deref(), Some("intake"));
}

#[test]
fn a_not_ready_probe_rearms_the_wake_for_its_seed() {
    let (control, seed) = browsing(true);
    let client = Runtime::embedded(ControlOptions::default());
    client.with_control(|plane| *plane = control);
    let (tx, wake) = sync_channel(1);
    client.set_listener(Arc::new(Wake(tx)));
    client.wake();
    let probe = Probe {
        client,
        wake,
        deadline: Instant::now() + WAIT,
    };
    probe.wake.recv_timeout(Duration::from_secs(1)).unwrap();
    let mut unavailable = None;
    assert!(probe.sample_ready(&mut unavailable).unwrap().is_none());
    assert!(
        probe.wake.try_recv().is_err(),
        "no extra wake may hide a missing rearm"
    );
    probe.client.feed(intake_seed(seed.unwrap())).unwrap();
    // The embedded transport owns notification; mirror the connected driver's
    // post-publication wake. This edge is suppressed if sampling did not rearm.
    probe.client.wake();
    probe
        .wake
        .recv_timeout(Duration::from_secs(1))
        .expect("the newly seeded replica must wake the probe");
    let (_, intake) = probe.sample_ready(&mut unavailable).unwrap().unwrap();
    assert_eq!(intake.as_deref(), Some("intake"));
}
