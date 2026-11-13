//! Nonsecret approval review and decision projection.

use super::*;

#[uniffi::export]
impl RemoteClient {
    /// Opt in to pending approvals. No secret values or provider invocation cross this seam.
    pub fn watch_approvals(&self) {
        if let Some(client) = self.runtime_client() {
            client.watch_approvals();
        }
    }

    /// Re-read pending approvals after a refused, stale, or unconfirmed outcome.
    /// This never resubmits a decision.
    pub fn refresh_approvals(&self) {
        if let Some(client) = self.runtime_client() {
            client.refresh_approvals();
        }
    }

    /// Versioned JSON shared with C: supported and pending records with exact references
    /// and bound provider identity. Purpose/name are untrusted labels, not authenticated identity.
    pub fn approvals_json(&self) -> Result<Vec<u8>, WireError> {
        let client = self.runtime_client();
        let supported = client.as_ref().is_some_and(Client::approvals_supported);
        let approvals = client.as_ref().map_or_else(Vec::new, Client::approvals);
        crate::projection::approvals::pending_json(supported, &approvals).map_err(|_| {
            WireError::Runtime {
                reason: "could not encode approvals".to_owned(),
            }
        })
    }

    /// Submit once; the `ApprovalDecision` event confirms its outcome. Never retry automatically.
    /// The provider desktop still has to unlock 1Password locally.
    pub fn decide_approval(&self, approval_id: String, approve: bool) -> bool {
        let Some(id) = phux_protocol::ids::ApprovalId::parse(&approval_id) else {
            return false;
        };
        let Some(client) = self.runtime_client() else {
            return false;
        };
        if !client
            .approvals()
            .iter()
            .any(|approval| approval.id == approval_id)
        {
            return false;
        }
        client.decide_approval(
            id,
            if approve {
                phux_client_runtime::approvals::Decision::Approve
            } else {
                phux_client_runtime::approvals::Decision::Deny
            },
        )
    }
}

impl RemoteClient {
    pub(super) fn project_approval(event: &Event, projected: &mut Vec<WireEvent>) -> bool {
        if matches!(event, Event::ApprovalsChanged) {
            projected.push(WireEvent::ApprovalsChanged);
            return true;
        }
        let Some(decision) = crate::projection::approvals::decision(event) else {
            return false;
        };
        projected.push(WireEvent::ApprovalDecision {
            id: decision.id,
            decision: decision.decision,
            status: decision.status,
            message: decision.message,
        });
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use phux_protocol::wire::frame::FrameKind;

    #[test]
    fn approval_bridge_refuses_offline_and_projects_the_shared_outcomes() {
        use phux_client_runtime::approvals::{Decision, DecisionOutcome};
        let remote = RemoteClient::new("unused".into(), 80, 24, None, None);
        remote.watch_approvals();
        assert!(!remote.decide_approval("a".repeat(32), true));
        let json: serde_json::Value =
            serde_json::from_slice(&remote.approvals_json().unwrap()).unwrap();
        assert_eq!(json["supported"], false);
        assert!(json["approvals"].as_array().unwrap().is_empty());
        let mut projected = Vec::new();
        remote.project_event(None, Event::ApprovalsChanged, &mut projected);
        remote.project_event(
            None,
            Event::ApprovalDecision {
                id: phux_protocol::ids::ApprovalId::parse(&"a".repeat(32)).unwrap(),
                decision: Decision::Approve,
                outcome: DecisionOutcome::Unconfirmed,
            },
            &mut projected,
        );
        assert_eq!(projected[0], WireEvent::ApprovalsChanged);
        assert!(
            matches!(&projected[1], WireEvent::ApprovalDecision { id, decision, status, .. }
            if id == &"a".repeat(32) && decision == "approve" && status == "unconfirmed")
        );
    }

    #[test]
    fn mobile_approval_listing_and_decision_use_the_runtime_metadata_lane() {
        use phux_protocol::caps::{
            BootstrapLimits, BootstrapProfile, ServerCapabilities, ServerFeature, ServerFeatureSet,
        };
        let mut remote = RemoteClient::new("unused".into(), 0, 0, None, None);
        let runtime = Runtime::embedded(phux_client_runtime::control::ControlOptions {
            automatic_lifecycle: false,
            ..phux_client_runtime::control::ControlOptions::default()
        });
        *Arc::get_mut(&mut remote).unwrap().client.get_mut().unwrap() = Some(runtime.clone());
        runtime.with_control(phux_client_runtime::control::ControlPlane::connection_opened);
        drop(runtime.take_outbound());
        runtime
            .feed(FrameKind::HelloOk {
                protocol_major: phux_protocol::PROTOCOL_VERSION.major,
                protocol_minor: phux_protocol::PROTOCOL_VERSION.minor,
                protocol_patch: phux_protocol::PROTOCOL_VERSION.patch,
                server_caps: ServerCapabilities::new()
                    .with_features(ServerFeatureSet::with(&[ServerFeature::Approvals])),
                server_id: vec![0xAB; 16],
                selected_profile: Some(BootstrapProfile::SynthesizedVtRaw),
                bootstrap_limits: BootstrapLimits::default(),
            })
            .unwrap();
        drop(runtime.take_outbound());
        remote.watch_approvals();
        let frames = runtime.take_outbound();
        let list_id = frames
            .iter()
            .find_map(|bytes| match FrameKind::decode(bytes).unwrap().0 {
                FrameKind::ListMetadata { request_id, .. } => Some(request_id),
                _ => None,
            })
            .unwrap();
        let id = "a".repeat(32);
        runtime
            .feed(FrameKind::MetadataKeys {
                request_id: list_id,
                keys: vec![format!("phux.approval/v1/{id}")],
            })
            .unwrap();
        let frames = runtime.take_outbound();
        let read_id = frames
            .iter()
            .find_map(|bytes| match FrameKind::decode(bytes).unwrap().0 {
                FrameKind::GetMetadata { request_id, .. } => Some(request_id),
                _ => None,
            })
            .unwrap();
        let record = br#"{"id":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","requester":{"client":3},"method":"CREDENTIAL_REQUEST","requested_at_ms":1,"expires_at_ms":120001,"credential":{"request":{"provider":"desktop","references":["op://vault/item/field"],"purpose":"Run tests"},"provider":{"client":8,"credential_id":"sha256:bound"}}}"#;
        runtime
            .feed(FrameKind::MetadataValue {
                request_id: read_id,
                value: Some(record.to_vec()),
            })
            .unwrap();
        let json: serde_json::Value =
            serde_json::from_slice(&remote.approvals_json().unwrap()).unwrap();
        assert_eq!(
            json["approvals"][0]["credential"]["provider"]["credential_id"],
            "sha256:bound"
        );
        assert!(remote.take_events().contains(&WireEvent::ApprovalsChanged));
        assert!(remote.decide_approval(id.clone(), true));
        assert!(
            !remote.decide_approval(id.clone(), true),
            "no duplicate decision in flight"
        );
        let frames = runtime.take_outbound();
        let confirm_id = frames
            .iter()
            .find_map(|bytes| match FrameKind::decode(bytes).unwrap().0 {
                FrameKind::GetMetadata { request_id, .. } => Some(request_id),
                _ => None,
            })
            .unwrap();
        runtime
            .feed(FrameKind::MetadataValue {
                request_id: confirm_id,
                value: None,
            })
            .unwrap();
        assert!(remote.take_events().iter().any(|event| matches!(event, WireEvent::ApprovalDecision { id: observed, decision, status, .. } if observed == &id && decision == "approve" && status == "decided")));
    }
}
