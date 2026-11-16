//! Browser-only approval projection over server-owned metadata. No provider or secret lane.
//!
//! The browser cannot depend on the native/tokio runtime. These metadata records mirror
//! its Approval model; decisions use the same ordered SET + confirming GET protocol.

use std::collections::HashMap;

pub use phux_client_core::approvals::{Approval, CredentialApproval, Requester};
use phux_protocol::ids::ApprovalId;
use phux_protocol::wire::frame::{APPROVAL_KEY_PREFIX, AgentEvent, FrameKind, Scope};

use super::{Connection, Outcome, encode};

struct InFlight {
    id: ApprovalId,
    decide_id: u32,
    refusal: Option<String>,
}

#[derive(Default)]
pub(super) struct ApprovalWatch {
    supported: bool,
    counter: u32,
    listing: Option<u32>,
    relist: bool,
    reads: HashMap<u32, ApprovalId>,
    decisions: HashMap<u32, InFlight>,
    pending: HashMap<ApprovalId, Approval>,
    status: String,
}

impl ApprovalWatch {
    fn next_id(&mut self) -> u32 {
        // Separate from attach/discovery, pane and path-query request ids.
        self.counter = self.counter.wrapping_add(1) & 0x0fff_ffff;
        0x5000_0000 | self.counter
    }

    pub(super) fn negotiate(&mut self, supported: bool) -> Vec<Vec<u8>> {
        self.supported = supported;
        self.list()
    }

    fn list(&mut self) -> Vec<Vec<u8>> {
        if !self.supported {
            return Vec::new();
        }
        if self.listing.is_some() {
            self.relist = true;
            return Vec::new();
        }
        let request_id = self.next_id();
        self.listing = Some(request_id);
        vec![encode(&FrameKind::ListMetadata {
            request_id,
            scope: Scope::Global,
        })]
    }

    fn read(&mut self, id: ApprovalId) -> Option<Vec<u8>> {
        if self.reads.values().any(|reading| *reading == id) {
            return None;
        }
        let request_id = self.next_id();
        self.reads.insert(request_id, id);
        Some(encode(&FrameKind::GetMetadata {
            request_id,
            scope: Scope::Global,
            key: id.record_key(),
        }))
    }

    pub(super) fn disconnect(&mut self) {
        self.supported = false;
        if !self.decisions.is_empty() {
            let ids = self
                .decisions
                .values()
                .map(|flight| flight.id.to_string())
                .collect::<Vec<_>>()
                .join(", ");
            self.status = format!(
                "Decision unconfirmed for {ids}: connection ended. Re-read pending approvals before deciding again. Nothing will be retried automatically."
            );
        }
        self.decisions.clear();
        self.reads.clear();
        self.listing = None;
        self.pending.clear();
    }

    fn listed(&mut self, keys: &[String]) -> Outcome {
        self.listing = None;
        let ids: Vec<_> = keys
            .iter()
            .filter_map(|key| key.strip_prefix(APPROVAL_KEY_PREFIX))
            .filter_map(ApprovalId::parse)
            .collect();
        self.pending.retain(|id, _| ids.contains(id));
        let mut send = Vec::new();
        for id in ids {
            if !self.pending.contains_key(&id) {
                send.extend(self.read(id));
            }
        }
        if std::mem::take(&mut self.relist) {
            send.extend(self.list());
        }
        Outcome {
            send,
            approvals: true,
            ..Outcome::default()
        }
    }

    fn accept_record(&mut self, request_id: u32, value: Option<&[u8]>) -> Outcome {
        let id = self
            .reads
            .remove(&request_id)
            .expect("correlation checked by caller");
        if let Some(bytes) = value {
            match Approval::parse(bytes) {
                Ok(record) if record.id == id.to_string() => {
                    self.pending.insert(id, record);
                }
                _ => {
                    self.pending.remove(&id);
                    self.status =
                        "Could not read an approval record; it cannot be approved here.".into()
                }
            }
        } else {
            self.pending.remove(&id);
        }
        Outcome {
            approvals: true,
            ..Outcome::default()
        }
    }

    fn finish(&mut self, confirm_id: u32, pending: Option<bool>) -> Outcome {
        let flight = self
            .decisions
            .remove(&confirm_id)
            .expect("correlation checked by caller");
        self.status = match (flight.refusal, pending) {
            (None, Some(false) | None) => { self.pending.remove(&flight.id); format!("Decision accepted for {}.", flight.id) }
            (Some(_), Some(false)) => { self.pending.remove(&flight.id); "Approval is stale: already decided, expired, or withdrawn.".into() }
            (Some(message), _) => format!("Decision refused: {message}"),
            (None, Some(true)) => "Decision unconfirmed. Re-read pending approvals before deciding again; do not retry automatically.".into(),
        };
        Outcome {
            approvals: true,
            ..Outcome::default()
        }
    }

    fn error(&mut self, request_id: u32, message: &str) -> Option<Outcome> {
        if self.listing == Some(request_id) {
            self.listing = None;
            self.status = format!("Approvals unavailable: {message}");
            let send = if std::mem::take(&mut self.relist) {
                self.list()
            } else {
                Vec::new()
            };
            return Some(Outcome {
                send,
                approvals: true,
                ..Outcome::default()
            });
        }
        if self.reads.remove(&request_id).is_some() {
            self.status = format!("Approval unavailable: {message}");
            return Some(Outcome {
                approvals: true,
                ..Outcome::default()
            });
        }
        if let Some(flight) = self
            .decisions
            .values_mut()
            .find(|flight| flight.decide_id == request_id)
        {
            flight.refusal = Some(message.to_owned());
            return Some(Outcome::default());
        }
        if self.decisions.contains_key(&request_id) {
            return Some(self.finish(request_id, None));
        }
        None
    }

    pub(super) fn on_frame(&mut self, frame: &FrameKind) -> Option<Outcome> {
        if !self.supported {
            return None;
        }
        if let FrameKind::Event { event, .. } = frame {
            return self.event(event);
        }
        match frame {
            FrameKind::MetadataKeys { request_id, keys } if self.listing == Some(*request_id) => {
                Some(self.listed(keys))
            }
            FrameKind::MetadataValue { request_id, value }
                if self.reads.contains_key(request_id) =>
            {
                Some(self.accept_record(*request_id, value.as_deref()))
            }
            FrameKind::MetadataValue { request_id, value }
                if self.decisions.contains_key(request_id) =>
            {
                Some(self.finish(*request_id, Some(value.is_some())))
            }
            FrameKind::Error {
                request_id: Some(request_id),
                message,
                ..
            } => self.error(*request_id, message),
            _ => None,
        }
    }

    fn event(&mut self, event: &AgentEvent) -> Option<Outcome> {
        match event {
            AgentEvent::ApprovalRequested { id } => Some(Outcome {
                send: self.read(*id).into_iter().collect(),
                approvals: true,
                ..Outcome::default()
            }),
            AgentEvent::ApprovalDecided { id, .. } => {
                self.pending.remove(id);
                Some(Outcome {
                    approvals: true,
                    ..Outcome::default()
                })
            }
            AgentEvent::JournalGap { .. } | AgentEvent::SourceGap { .. } => Some(Outcome {
                send: self.list(),
                ..Outcome::default()
            }),
            _ => None,
        }
    }

    fn decide(&mut self, id: ApprovalId, approve: bool) -> Vec<Vec<u8>> {
        if !self.supported
            || !self.pending.contains_key(&id)
            || self.decisions.values().any(|flight| flight.id == id)
        {
            return Vec::new();
        }
        let decide_id = self.next_id();
        let confirm_id = self.next_id();
        self.decisions.insert(
            confirm_id,
            InFlight {
                id,
                decide_id,
                refusal: None,
            },
        );
        self.status = "Decision sent; waiting for server confirmation.".into();
        vec![
            encode(&FrameKind::SetMetadata {
                request_id: decide_id,
                scope: Scope::Global,
                key: id.decide_key(),
                value: if approve {
                    b"approve".to_vec()
                } else {
                    b"deny".to_vec()
                },
            }),
            encode(&FrameKind::GetMetadata {
                request_id: confirm_id,
                scope: Scope::Global,
                key: id.record_key(),
            }),
        ]
    }
}

impl Connection {
    /// Pending records, oldest first. Exact references only; never resolved values.
    pub fn approvals(&self) -> Vec<&Approval> {
        let mut approvals: Vec<_> = self.approvals.pending.values().collect();
        approvals.sort_by(|a, b| (a.requested_at_ms, &a.id).cmp(&(b.requested_at_ms, &b.id)));
        approvals
    }

    /// Whether the live peer supports approval records and decisions.
    pub fn approvals_supported(&self) -> bool {
        self.approvals.supported && !self.failed
    }

    /// The most recent decision or refresh diagnostic; never a secret value.
    pub fn approval_status(&self) -> &str {
        &self.approvals.status
    }

    /// Whether this immutable request already has a submitted decision.
    pub fn approval_deciding(&self, id: &str) -> bool {
        self.approvals
            .decisions
            .values()
            .any(|flight| flight.id.to_string() == id)
    }

    /// Reconcile pending records after an unavailable or unconfirmed decision.
    /// This only reads metadata; it never retries a decision.
    pub fn refresh_approvals(&mut self) -> Vec<Vec<u8>> {
        if self.failed {
            return Vec::new();
        }
        self.approvals.status.clear();
        self.approvals.list()
    }

    /// Send exactly once, then wait for the decision's correlated confirmation.
    pub fn decide_approval(&mut self, id: &str, approve: bool) -> Vec<Vec<u8>> {
        if self.failed {
            return Vec::new();
        }
        ApprovalId::parse(id).map_or_else(Vec::new, |id| self.approvals.decide(id, approve))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use phux_protocol::wire::frame::ErrorCode;

    const ID: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const RECORD: &[u8] = br#"{"id":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","requester":{"client":3},"method":"CREDENTIAL_REQUEST","requested_at_ms":1,"expires_at_ms":120001,"credential":{"request":{"provider":"desktop","references":["op://vault/item/field"],"purpose":"Run tests"},"provider":{"client":8,"credential_id":"sha256:bound","client_name":"desktop label"}}}"#;

    fn decode(bytes: &[u8]) -> FrameKind {
        FrameKind::decode(bytes).unwrap().0
    }

    fn ready_watch() -> ApprovalWatch {
        let mut watch = ApprovalWatch::default();
        let first = watch.negotiate(true);
        let FrameKind::ListMetadata { request_id, .. } = decode(&first[0]) else {
            panic!("expected listing")
        };
        let listed = watch
            .on_frame(&FrameKind::MetadataKeys {
                request_id,
                keys: vec![format!("{APPROVAL_KEY_PREFIX}{ID}")],
            })
            .unwrap();
        let FrameKind::GetMetadata { request_id, .. } = decode(&listed.send[0]) else {
            panic!("expected read")
        };
        watch
            .on_frame(&FrameKind::MetadataValue {
                request_id,
                value: Some(RECORD.to_vec()),
            })
            .unwrap();
        watch
    }

    fn decide(watch: &mut ApprovalWatch) -> (u32, u32) {
        let frames = watch.decide(ApprovalId::parse(ID).unwrap(), true);
        let FrameKind::SetMetadata {
            request_id: decide_id,
            scope: Scope::Global,
            key,
            value,
        } = decode(&frames[0])
        else {
            panic!("expected decision")
        };
        assert_eq!(key, format!("phux.approval.decide/v1/{ID}"));
        assert_eq!(value, b"approve");
        let FrameKind::GetMetadata {
            request_id: confirm_id,
            ..
        } = decode(&frames[1])
        else {
            panic!("expected confirming read")
        };
        (decide_id, confirm_id)
    }

    #[cfg_attr(target_arch = "wasm32", wasm_bindgen_test::wasm_bindgen_test)]
    #[cfg_attr(not(target_arch = "wasm32"), test)]
    fn an_old_server_never_receives_a_decision_key() {
        let mut watch = ApprovalWatch::default();
        assert!(watch.negotiate(false).is_empty());
        assert!(
            watch
                .decide(ApprovalId::parse(ID).unwrap(), true)
                .is_empty()
        );
    }

    #[cfg_attr(target_arch = "wasm32", wasm_bindgen_test::wasm_bindgen_test)]
    #[cfg_attr(not(target_arch = "wasm32"), test)]
    fn exact_references_and_bound_identity_survive_listing() {
        let watch = ready_watch();
        let credential = watch.pending[&ApprovalId::parse(ID).unwrap()]
            .credential
            .as_ref()
            .unwrap();
        assert_eq!(credential.request.references, ["op://vault/item/field"]);
        assert_eq!(credential.provider.client, 8);
        assert_eq!(
            credential.provider.credential_id.as_deref(),
            Some("sha256:bound")
        );
    }

    #[cfg_attr(target_arch = "wasm32", wasm_bindgen_test::wasm_bindgen_test)]
    #[cfg_attr(not(target_arch = "wasm32"), test)]
    fn decisions_are_not_retried_and_success_requires_the_confirming_read() {
        let mut watch = ready_watch();
        let (_, confirm_id) = decide(&mut watch);
        assert!(
            watch
                .decide(ApprovalId::parse(ID).unwrap(), true)
                .is_empty()
        );
        assert_eq!(watch.pending.len(), 1);
        watch
            .on_frame(&FrameKind::MetadataValue {
                request_id: confirm_id,
                value: None,
            })
            .unwrap();
        assert!(watch.pending.is_empty());
        assert!(watch.status.contains("accepted"));
        assert!(
            watch
                .decide(ApprovalId::parse(ID).unwrap(), true)
                .is_empty()
        );
    }

    #[cfg_attr(target_arch = "wasm32", wasm_bindgen_test::wasm_bindgen_test)]
    #[cfg_attr(not(target_arch = "wasm32"), test)]
    fn refused_and_unconfirmed_decisions_are_not_successes() {
        let mut watch = ready_watch();
        let (decide_id, confirm_id) = decide(&mut watch);
        watch
            .on_frame(&FrameKind::Error {
                request_id: Some(decide_id),
                code: ErrorCode::PermissionDenied,
                message: "not allowed".into(),
            })
            .unwrap();
        watch
            .on_frame(&FrameKind::MetadataValue {
                request_id: confirm_id,
                value: Some(RECORD.to_vec()),
            })
            .unwrap();
        assert!(watch.status.contains("refused: not allowed"));
        assert_eq!(watch.pending.len(), 1);
        let (_, confirm_id) = decide(&mut watch);
        watch
            .on_frame(&FrameKind::MetadataValue {
                request_id: confirm_id,
                value: Some(RECORD.to_vec()),
            })
            .unwrap();
        assert!(watch.status.contains("unconfirmed"));
        assert_eq!(watch.pending.len(), 1);
        let refresh = watch.list();
        assert_eq!(refresh.len(), 1);
        assert!(matches!(
            decode(&refresh[0]),
            FrameKind::ListMetadata {
                scope: Scope::Global,
                ..
            }
        ));
        assert!(
            watch.decisions.is_empty(),
            "refresh must not retry a decision"
        );
    }

    #[cfg_attr(target_arch = "wasm32", wasm_bindgen_test::wasm_bindgen_test)]
    #[cfg_attr(not(target_arch = "wasm32"), test)]
    fn disconnect_voids_pending_ids_and_reports_an_unknown_decision() {
        let mut watch = ready_watch();
        decide(&mut watch);
        watch.disconnect();
        assert!(watch.status.contains("unconfirmed"));
        assert!(watch.pending.is_empty());
        assert!(watch.decisions.is_empty());
        assert!(!watch.supported);
    }

    #[cfg_attr(target_arch = "wasm32", wasm_bindgen_test::wasm_bindgen_test)]
    #[cfg_attr(not(target_arch = "wasm32"), test)]
    fn gaps_coalesce_to_one_fresh_listing_after_the_in_flight_listing() {
        let mut watch = ApprovalWatch::default();
        watch.negotiate(true);
        assert!(watch.list().is_empty());
        assert!(watch.list().is_empty());
        let first = watch.listing.unwrap();
        let outcome = watch
            .on_frame(&FrameKind::MetadataKeys {
                request_id: first,
                keys: Vec::new(),
            })
            .unwrap();
        assert_eq!(outcome.send.len(), 1);
        assert_ne!(watch.listing, Some(first));
    }

    #[cfg_attr(target_arch = "wasm32", wasm_bindgen_test::wasm_bindgen_test)]
    #[cfg_attr(not(target_arch = "wasm32"), test)]
    fn malformed_or_mismatched_records_are_not_decidable() {
        let mut watch = ready_watch();
        watch.pending.clear();
        for record in [
            b"not json".to_vec(),
            {
                let mut record: serde_json::Value = serde_json::from_slice(RECORD).unwrap();
                record["credential"] = serde_json::Value::Null;
                serde_json::to_vec(&record).unwrap()
            },
            {
                let mut record: serde_json::Value = serde_json::from_slice(RECORD).unwrap();
                record["credential"]["request"]["references"] = serde_json::json!([]);
                serde_json::to_vec(&record).unwrap()
            },
            RECORD
                .iter()
                .copied()
                .map(|b| if b == b'a' { b'b' } else { b })
                .collect(),
        ] {
            let id = ApprovalId::parse(ID).unwrap();
            let frame = watch.read(id).unwrap();
            let FrameKind::GetMetadata { request_id, .. } = decode(&frame) else {
                panic!("expected read")
            };
            watch
                .on_frame(&FrameKind::MetadataValue {
                    request_id,
                    value: Some(record),
                })
                .unwrap();
            assert!(watch.pending.is_empty());
        }
    }
}
