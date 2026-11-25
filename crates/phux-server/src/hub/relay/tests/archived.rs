//! The executing link, not outer hub capabilities, controls archived execution.
use super::*;
use phux_protocol::caps::{ServerFeatureExt2Set, ServerFeatureExtSet};
use phux_protocol::ids::{OperationId, ResourceKey, Revision};
use phux_protocol::native_agent::{NativeRequest, NativeRequestPayload, SpawnNativeAgent};

fn relaunch(n: u8) -> Command {
    Command::ResourceRelaunch { operation: OperationId::new([n;16]).unwrap(),
        resource: ResourceKey::new([7;16]).unwrap(), expected: Revision::new(1) }
}
fn native(request: NativeRequest) -> Command {
    Command::NativeAgent { request: NativeRequestPayload::new(request).unwrap() }
}

#[test]
fn archived_forwarding_requires_current_executing_peer_proof_and_keeps_prior_unknown() {
    let fence = crate::hub::operation_fence::OperationFence::default();
    let mut current = negotiated(ServerFeature::ALL);
    current.set_incarnation(Some([1;16]));
    current.set_operation_fence(fence.clone());
    current.set_satellite_features_ext(ServerFeatureExtSet::all());
    assert!(matches!(current.command_rejection(&relaunch(1)), Some(CommandResult::Error {
        code: ErrorCode::UnsupportedSatelliteRoute, .. })), "all older words do not imply executing peer proof");
    current.set_satellite_features_ext2(ServerFeatureExt2Set::all());
    let (reply, _receiver) = oneshot::channel();
    assert!(current.handle_request_checked(RelayRequest::Keyed {
        command: relaunch(1), actor: ClientId(7), reply,
    }).is_some(), "the proof-bearing executing connection can forward once");
    let mut replacement = negotiated(ServerFeature::ALL);
    replacement.set_operation_fence(fence);
    replacement.set_incarnation(Some([2;16]));
    replacement.set_satellite_features_ext(ServerFeatureExtSet::all());
    assert!(matches!(replacement.command_rejection(&relaunch(1)), Some(CommandResult::Error {
        code: ErrorCode::InputDeliveryUnknown, .. })), "a replacement cannot erase an earlier possible send");
    assert!(matches!(replacement.command_rejection(&relaunch(2)), Some(CommandResult::Error {
        code: ErrorCode::UnsupportedSatelliteRoute, .. })), "a new operation was never sent");
    let (reply, mut receiver) = oneshot::channel();
    assert!(replacement.handle_request_checked(RelayRequest::Keyed {
        command: relaunch(1), actor: ClientId(7), reply,
    }).is_none());
    assert!(matches!(receiver.try_recv().unwrap(), CommandResult::Error { code: ErrorCode::InputDeliveryUnknown, .. }));
}

#[test]
fn guardian_spawns_and_raw_review_cannot_bypass_executing_peer_or_local_owner_checks() {
    let mut peer = negotiated(ServerFeature::ALL);
    peer.set_satellite_features_ext(ServerFeatureExtSet::all());
    let guardian = native(NativeRequest::Spawn { spec: SpawnNativeAgent {
        session: "archive".into(), backend: "acp".into(), provider: "saved".into(),
        command: vec!["old-provider".into()], cwd: Some("/".into()), env: Default::default(),
        native_id: Some("resume-123".into()), guardian: Some(ResourceId::local(1)),
        guardian_key: None, guardian_expected: Some(Revision::new(1)),
    }});
    assert!(peer.command_rejection(&guardian).is_some());
    peer.set_satellite_features_ext2(ServerFeatureExt2Set::all());
    assert!(peer.command_rejection(&guardian).is_none());
    let prepare = native(NativeRequest::PrepareRelaunch { operation: OperationId::new([3;16]).unwrap(),
        resource: ResourceKey::new([7;16]).unwrap(), expected: Revision::new(1) });
    assert!(matches!(peer.command_rejection(&prepare), Some(CommandResult::Error {
        code: ErrorCode::UnsupportedSatelliteRoute, .. })), "a hub cannot manufacture local owner review authority");
    let (reply, mut receiver) = oneshot::channel();
    assert!(peer.handle_request_checked(RelayRequest::Command { command: relaunch(4), reply, subscribe: None }).is_none());
    assert!(matches!(receiver.try_recv().unwrap(), CommandResult::Error { code: ErrorCode::PreconditionFailed, .. }));
    assert!(peer.handle_request_checked(RelayRequest::Forward { frame: FrameKind::Command {
        request_id: 1, command: relaunch(5),
    }}).is_none(), "uncorrelated forwarding cannot discard operation ownership");
}
