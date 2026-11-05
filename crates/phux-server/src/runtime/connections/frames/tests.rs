use super::*;
use crate::state::SharedState;

#[test]
fn a_stream_frame_names_its_resource_and_a_viewport_vote_names_none() {
    let id = WireResourceId::local(7);
    let ack = FrameKind::FrameAck {
        terminal_id: id.clone(),
        stream_id: phux_protocol::ids::StreamId::new(1).expect("nonzero"),
        bootstrap_id: phux_protocol::ids::BootstrapId::new(1).expect("nonzero"),
        seq: 1,
    };
    assert_eq!(frame_resource(&ack), Some(&id));
    let vote = FrameKind::ViewportResize {
        viewport: phux_protocol::wire::frame::ViewportInfo::new(80, 24),
    };
    assert!(frame_resource(&vote).is_none());
}

#[test]
fn remote_and_unknown_resources_have_no_default_app() {
    let state = SharedState::new();
    assert!(crate::runtime::apps::app_serving(&state, &WireResourceId::local(999)).is_none());
    let remote = WireResourceId::satellite(phux_protocol::ids::SatelliteHost::from("remote"), 1);
    assert!(crate::runtime::apps::app_serving(&state, &remote).is_none());
}
