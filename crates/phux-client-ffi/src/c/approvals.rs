//! Approval-only operations: no provider registration, invocation, or secret values.

use phux_client_runtime::approvals::Decision;
use phux_protocol::ids::ApprovalId;

use super::{
    BridgeError, PhuxBytes, PhuxClient, PhuxClientResult, bytes_in, bytes_out, with_client_mut,
};

/// Watch pending approvals on this and later connections. Pump/poll normally.
/// # Safety
/// `client` must be live, exclusively accessed on its owning thread.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn phux_client_watch_approvals(client: *mut PhuxClient) -> PhuxClientResult {
    with_client_mut(client, |client| {
        client.runtime.watch_approvals();
        client.drain_outbound();
        Ok(())
    })
}

/// Re-read pending approvals without resubmitting decisions.
/// # Safety
/// `client` must be live, exclusively accessed on its owning thread.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn phux_client_refresh_approvals(
    client: *mut PhuxClient,
) -> PhuxClientResult {
    with_client_mut(client, |client| {
        client.runtime.refresh_approvals();
        client.drain_outbound();
        Ok(())
    })
}

/// Borrow a versioned UTF-8 JSON snapshot (`schema_version`, `supported`, `approvals`).
///
/// Purpose and names are untrusted labels. Show exact references and the bound provider's
/// client and credential id. No resolved values are returned. A provider desktop unlocks locally.
/// # Safety
/// Client is live, exclusive on its owning thread; out is writable and disjoint.
/// The borrowed span lasts until the next mutable call on this client.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn phux_client_approvals_json(
    client: *mut PhuxClient,
    out: *mut PhuxBytes,
) -> PhuxClientResult {
    with_client_mut(client, |client| {
        let out = unsafe { out.as_mut() }.ok_or_else(|| BridgeError::invalid("output is null"))?;
        let control = client.control();
        let approvals = control.approvals();
        let supported = control.approvals_supported();
        drop(control);
        client.approval_json = crate::projection::approvals::pending_json(supported, &approvals)
            .map_err(|_| BridgeError::state("could not encode approvals"))?;
        *out = bytes_out(&client.approval_json);
        Ok(())
    })
}

/// Submit one decision, once. Invalid, unsupported, unlisted, or already in-flight ids
/// are refused locally. Poll `phux_client_approval_decisions_take_json` for confirmation.
/// # Safety
/// Client and input span are live and disjoint, client exclusively accessed on its owning thread.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn phux_client_decide_approval(
    client: *mut PhuxClient,
    id: PhuxBytes,
    approve: bool,
) -> PhuxClientResult {
    with_client_mut(client, |client| {
        let bytes = unsafe { bytes_in(id.data, id.len) }?;
        let id = std::str::from_utf8(bytes)
            .ok()
            .and_then(ApprovalId::parse)
            .ok_or_else(|| BridgeError::invalid("invalid approval id"))?;
        let mut control = client.control();
        if !control
            .approvals()
            .iter()
            .any(|approval| approval.id == id.to_string())
        {
            return Err(BridgeError::state(
                "approval is not in the current pending snapshot",
            ));
        }
        if !control.decide_approval(
            id,
            if approve {
                Decision::Approve
            } else {
                Decision::Deny
            },
        ) {
            return Err(BridgeError::state(
                "approval decision not sent (unsupported or already pending)",
            ));
        }
        drop(control);
        client.drain_outbound();
        Ok(())
    })
}

/// Drain bounded decision notifications as JSON (`schema_version`, `decisions`, `dropped`).
///
/// Each has id, decision, status, message. Status is decided/stale/refused/unconfirmed.
/// If dropped, reconcile the current snapshot. Never retry decisions automatically.
/// # Safety
/// Client is live, exclusive on its owning thread; out is writable and disjoint.
/// The returned bytes last until the next mutable call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn phux_client_approval_decisions_take_json(
    client: *mut PhuxClient,
    out: *mut PhuxBytes,
) -> PhuxClientResult {
    with_client_mut(client, |client| {
        let out = unsafe { out.as_mut() }.ok_or_else(|| BridgeError::invalid("output is null"))?;
        client.approval_json = serde_json::to_vec(&serde_json::json!({ "schema_version": 1, "decisions": client.approval_decisions, "dropped": client.approval_decisions_dropped })).map_err(|_| BridgeError::state("could not encode approval decisions"))?;
        client.approval_decisions.clear();
        client.approval_decisions_dropped = false;
        *out = bytes_out(&client.approval_json);
        Ok(())
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::c::{phux_client_free, test_support};
    use phux_protocol::caps::ServerFeature;
    use phux_protocol::wire::frame::{ErrorCode, FrameKind};

    const ID: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const RECORD: &[u8] = br#"{"id":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","requester":{"client":3},"method":"CREDENTIAL_REQUEST","requested_at_ms":1,"expires_at_ms":120001,"credential":{"request":{"provider":"desktop","references":["op://vault/item/field"],"purpose":"Run tests"},"provider":{"client":8,"credential_id":"sha256:bound"}}}"#;

    fn sent(client: *mut PhuxClient) -> Vec<FrameKind> {
        // SAFETY: each test owns this live client and copies frames before mutation.
        unsafe {
            (*client)
                .inner
                .outgoing
                .iter()
                .map(|bytes| FrameKind::decode(bytes).unwrap().0)
                .collect()
        }
    }

    fn snapshot(client: *mut PhuxClient, decisions: bool) -> serde_json::Value {
        let mut bytes = PhuxBytes::default();
        // SAFETY: test owns the client and writable output; copy the borrow before mutation.
        unsafe {
            let result = if decisions {
                phux_client_approval_decisions_take_json(client, &raw mut bytes)
            } else {
                phux_client_approvals_json(client, &raw mut bytes)
            };
            assert_eq!(result, PhuxClientResult::Ok);
            serde_json::from_slice(bytes_in(bytes.data, bytes.len).unwrap()).unwrap()
        }
    }

    fn ready() -> *mut PhuxClient {
        let client = test_support::negotiated_client(&[ServerFeature::Approvals]);
        // SAFETY: test owns this live client on its thread.
        assert_eq!(
            unsafe { phux_client_watch_approvals(client) },
            PhuxClientResult::Ok
        );
        let request_id = sent(client)
            .iter()
            .find_map(|frame| match frame {
                FrameKind::ListMetadata { request_id, .. } => Some(*request_id),
                _ => None,
            })
            .unwrap();
        assert_eq!(
            test_support::feed(
                client,
                &FrameKind::MetadataKeys {
                    request_id,
                    keys: vec![format!("phux.approval/v1/{ID}")]
                }
            ),
            PhuxClientResult::Ok
        );
        let request_id = sent(client)
            .iter()
            .find_map(|frame| match frame {
                FrameKind::GetMetadata { request_id, .. } => Some(*request_id),
                _ => None,
            })
            .unwrap();
        assert_eq!(
            test_support::feed(
                client,
                &FrameKind::MetadataValue {
                    request_id,
                    value: Some(RECORD.to_vec())
                }
            ),
            PhuxClientResult::Ok
        );
        client
    }

    #[test]
    fn c_snapshot_and_confirmed_decision_preserve_the_nonsecret_contract() {
        let client = ready();
        let json = snapshot(client, false);
        assert_eq!(json["supported"], true);
        assert_eq!(
            json["approvals"][0]["credential"]["request"]["references"][0],
            "op://vault/item/field"
        );
        assert_eq!(
            json["approvals"][0]["credential"]["provider"]["credential_id"],
            "sha256:bound"
        );
        // SAFETY: test owns the client and static id bytes outlive the call.
        assert_eq!(
            unsafe { phux_client_decide_approval(client, bytes_out(ID.as_bytes()), true) },
            PhuxClientResult::Ok
        );
        let frames = sent(client);
        let confirm_id = frames
            .iter()
            .rev()
            .find_map(|frame| match frame {
                FrameKind::GetMetadata { request_id, .. } => Some(*request_id),
                _ => None,
            })
            .unwrap();
        assert_eq!(
            test_support::feed(
                client,
                &FrameKind::MetadataValue {
                    request_id: confirm_id,
                    value: None
                }
            ),
            PhuxClientResult::Ok
        );
        assert!(
            snapshot(client, false)["approvals"]
                .as_array()
                .unwrap()
                .is_empty()
        );
        assert_eq!(snapshot(client, true)["decisions"][0]["status"], "decided");
        assert!(
            snapshot(client, true)["decisions"]
                .as_array()
                .unwrap()
                .is_empty()
        );
        // SAFETY: this test owns the client and frees it exactly once.
        unsafe { phux_client_free(client) };
    }

    #[test]
    fn c_refusal_is_reported_not_mistaken_for_a_success() {
        let client = ready();
        // SAFETY: test owns the client and static id bytes outlive the call.
        assert_eq!(
            unsafe { phux_client_decide_approval(client, bytes_out(ID.as_bytes()), true) },
            PhuxClientResult::Ok
        );
        let frames = sent(client);
        let decide_id = frames
            .iter()
            .rev()
            .find_map(|frame| match frame {
                FrameKind::SetMetadata { request_id, .. } => Some(*request_id),
                _ => None,
            })
            .unwrap();
        let confirm_id = frames
            .iter()
            .rev()
            .find_map(|frame| match frame {
                FrameKind::GetMetadata { request_id, .. } => Some(*request_id),
                _ => None,
            })
            .unwrap();
        assert_eq!(
            test_support::feed(
                client,
                &FrameKind::Error {
                    request_id: Some(decide_id),
                    code: ErrorCode::PermissionDenied,
                    message: "not a decider".into()
                }
            ),
            PhuxClientResult::Ok
        );
        assert_eq!(
            test_support::feed(
                client,
                &FrameKind::MetadataValue {
                    request_id: confirm_id,
                    value: Some(RECORD.to_vec())
                }
            ),
            PhuxClientResult::Ok
        );
        let result = snapshot(client, true);
        assert_eq!(result["decisions"][0]["status"], "refused");
        assert_eq!(result["decisions"][0]["message"], "not a decider");
        // SAFETY: this test owns the client and frees it exactly once.
        unsafe { phux_client_free(client) };
    }

    #[test]
    fn c_never_sends_decision_keys_to_an_old_server() {
        let client = test_support::negotiated_client(&[]);
        // SAFETY: test owns the client and input.
        unsafe {
            assert_eq!(phux_client_watch_approvals(client), PhuxClientResult::Ok);
            assert_ne!(
                phux_client_decide_approval(client, bytes_out(ID.as_bytes()), true),
                PhuxClientResult::Ok
            );
        }
        assert!(sent(client).is_empty());
        assert_eq!(snapshot(client, false)["supported"], false);
        // SAFETY: this test owns the client and frees it exactly once.
        unsafe { phux_client_free(client) };
    }
}
