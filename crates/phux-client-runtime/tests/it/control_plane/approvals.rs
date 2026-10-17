//! The server-held approval projection (ADR-0128) driven sans-IO: listing,
//! live discovery and endings, gap recovery, and each decision outcome.

use super::*;
use phux_client_runtime::approvals::{Decision, DecisionOutcome};
use phux_protocol::ids::ApprovalId;
use phux_protocol::wire::frame::{AgentEvent, ApprovalOutcome, Scope};

fn outbound(plane: &mut ControlPlane) -> Vec<FrameKind> {
    plane
        .take_outbound()
        .iter()
        .map(|bytes| decode(bytes))
        .collect()
}

const fn id(byte: u8) -> ApprovalId {
    ApprovalId::new([byte; 16]).unwrap()
}

fn record(id: ApprovalId, requested_at_ms: u64) -> Vec<u8> {
    format!(
        r#"{{"schema_version":1,"id":"{id}","requester":{{"client":3,"client_name":"agent"}},"method":"SIGNAL_TERMINAL","signal":"kill","subjects":["terminal:4"],"requested_at_ms":{requested_at_ms},"expires_at_ms":120000}}"#
    )
    .into_bytes()
}

fn negotiated(features: &[ServerFeature]) -> ControlPlane {
    let mut plane = ControlPlane::new(ControlOptions::default());
    plane.connection_opened();
    plane.feed(hello_ok_with(features)).unwrap();
    let _ = plane.take_outbound();
    let _ = plane.take_events();
    plane
}

fn list_id(frames: &[FrameKind]) -> u32 {
    frames
        .iter()
        .find_map(|frame| match frame {
            FrameKind::ListMetadata {
                request_id,
                scope: Scope::Global,
            } => Some(*request_id),
            _ => None,
        })
        .expect("LIST_METADATA { Global }")
}

fn reads(frames: &[FrameKind]) -> Vec<(u32, String)> {
    frames
        .iter()
        .filter_map(|frame| match frame {
            FrameKind::GetMetadata {
                request_id,
                scope: Scope::Global,
                key,
            } => Some((*request_id, key.clone())),
            _ => None,
        })
        .collect()
}

fn changed(plane: &mut ControlPlane) -> bool {
    plane
        .take_events()
        .iter()
        .any(|event| matches!(event, Event::ApprovalsChanged))
}

fn event(plane: &mut ControlPlane, event: AgentEvent) {
    plane
        .feed(FrameKind::Event {
            terminal: None,
            event,
            stamp: None,
        })
        .unwrap();
}

/// A plane watching, with `pending` listed and read.
fn watching(pending: &[(ApprovalId, u64)]) -> ControlPlane {
    let mut plane = negotiated(&[ServerFeature::Approvals]);
    plane.watch_approvals();
    let listing = list_id(&outbound(&mut plane));
    let mut keys: Vec<String> = pending.iter().map(|(id, _)| id.record_key()).collect();
    keys.push("phux.session.project/v1".to_owned());
    plane
        .feed(FrameKind::MetadataKeys {
            request_id: listing,
            keys,
        })
        .unwrap();
    let sent = reads(&outbound(&mut plane));
    assert_eq!(sent.len(), pending.len(), "one read per listed record only");
    for ((request_id, key), (id, at)) in sent.into_iter().zip(pending) {
        assert_eq!(key, id.record_key());
        plane
            .feed(FrameKind::MetadataValue {
                request_id,
                value: Some(record(*id, *at)),
            })
            .unwrap();
    }
    let _ = plane.take_events();
    plane
}

#[test]
fn nothing_is_sent_to_a_server_without_approvals() {
    let mut plane = negotiated(&[]);
    plane.watch_approvals();
    assert!(outbound(&mut plane).is_empty());
    assert!(!plane.approvals_supported());
    assert!(!plane.decide_approval(id(1), Decision::Approve));
    assert!(outbound(&mut plane).is_empty());
}

#[test]
fn the_listing_is_read_oldest_first_and_live_events_keep_it_current() {
    let mut plane = watching(&[(id(2), 20), (id(1), 10)]);
    let listed: Vec<String> = plane.approvals().into_iter().map(|a| a.id).collect();
    assert_eq!(listed, vec![id(1).to_string(), id(2).to_string()]);

    event(&mut plane, AgentEvent::ApprovalRequested { id: id(3) });
    let sent = reads(&outbound(&mut plane));
    assert_eq!(sent.len(), 1);
    plane
        .feed(FrameKind::MetadataValue {
            request_id: sent[0].0,
            value: Some(record(id(3), 30)),
        })
        .unwrap();
    assert!(changed(&mut plane));
    assert_eq!(plane.approvals().len(), 3);

    // Already known: no second read.
    event(&mut plane, AgentEvent::ApprovalRequested { id: id(3) });
    assert!(reads(&outbound(&mut plane)).is_empty());

    event(
        &mut plane,
        AgentEvent::ApprovalDecided {
            id: id(1),
            outcome: ApprovalOutcome::Expired,
        },
    );
    assert!(changed(&mut plane));
    assert_eq!(plane.approvals().len(), 2);
}

#[test]
fn a_record_gone_before_its_read_is_skipped() {
    let mut plane = negotiated(&[ServerFeature::Approvals]);
    plane.watch_approvals();
    event(&mut plane, AgentEvent::ApprovalRequested { id: id(4) });
    let sent = outbound(&mut plane);
    let read = reads(&sent)
        .into_iter()
        .find(|(_, key)| *key == id(4).record_key());
    plane
        .feed(FrameKind::MetadataValue {
            request_id: read.unwrap().0,
            value: None,
        })
        .unwrap();
    assert!(plane.approvals().is_empty());
}

#[test]
fn a_gap_relists_and_drops_what_is_no_longer_listed() {
    let mut plane = watching(&[(id(1), 10), (id(2), 20)]);
    event(
        &mut plane,
        AgentEvent::JournalGap {
            first_missing: 4,
            last_missing: 9,
        },
    );
    let listing = list_id(&outbound(&mut plane));
    plane
        .feed(FrameKind::MetadataKeys {
            request_id: listing,
            keys: vec![id(2).record_key()],
        })
        .unwrap();
    assert!(changed(&mut plane));
    assert!(
        reads(&outbound(&mut plane)).is_empty(),
        "records are immutable"
    );
    let left: Vec<String> = plane.approvals().into_iter().map(|a| a.id).collect();
    assert_eq!(left, vec![id(2).to_string()]);
}

#[test]
fn a_gap_during_a_listing_lists_once_more_when_it_lands() {
    let mut plane = watching(&[]);
    let gap = AgentEvent::SourceGap { dropped: 3 };
    event(&mut plane, gap.clone());
    let first = list_id(&outbound(&mut plane));
    event(&mut plane, gap.clone());
    event(&mut plane, gap);
    assert!(
        outbound(&mut plane).is_empty(),
        "one listing in flight at a time"
    );
    plane
        .feed(FrameKind::MetadataKeys {
            request_id: first,
            keys: Vec::new(),
        })
        .unwrap();
    let sent = outbound(&mut plane);
    let second = list_id(&sent);
    assert_ne!(second, first);
    assert_eq!(sent.len(), 1, "the gaps fold into one more listing");
    plane
        .feed(FrameKind::MetadataKeys {
            request_id: second,
            keys: vec![id(3).record_key()],
        })
        .unwrap();
    let sent = reads(&outbound(&mut plane));
    assert_eq!(sent.len(), 1, "no listing after the follow-up lands");
    assert_eq!(sent[0].1, id(3).record_key());
}

/// Decide `id` and return the decision and confirming-read request ids.
fn decide(plane: &mut ControlPlane, id: ApprovalId) -> (u32, u32) {
    assert!(plane.decide_approval(id, Decision::Approve));
    assert!(
        !plane.decide_approval(id, Decision::Deny),
        "one decision per approval in flight"
    );
    let sent = outbound(plane);
    let FrameKind::SetMetadata {
        request_id: decide_id,
        scope: Scope::Global,
        key,
        value,
    } = &sent[0]
    else {
        panic!("SET_METADATA first: {sent:?}");
    };
    assert_eq!(*key, id.decide_key());
    assert_eq!(value.as_slice(), b"approve");
    let FrameKind::GetMetadata {
        request_id: confirm_id,
        key,
        ..
    } = &sent[1]
    else {
        panic!("confirming read second: {sent:?}");
    };
    assert_eq!(*key, id.record_key());
    (*decide_id, *confirm_id)
}

fn outcome(plane: &mut ControlPlane) -> DecisionOutcome {
    plane
        .take_events()
        .into_iter()
        .find_map(|event| match event {
            Event::ApprovalDecision { outcome, .. } => Some(outcome),
            _ => None,
        })
        .expect("one decision outcome")
}

fn refuse(plane: &mut ControlPlane, request_id: u32) {
    plane
        .feed(FrameKind::Error {
            request_id: Some(request_id),
            code: ErrorCode::PermissionDenied,
            message: "permission denied".to_owned(),
        })
        .unwrap();
}

#[test]
fn an_accepted_decision_clears_the_approval() {
    let mut plane = watching(&[(id(1), 10)]);
    let (_, confirm) = decide(&mut plane, id(1));
    plane
        .feed(FrameKind::MetadataValue {
            request_id: confirm,
            value: None,
        })
        .unwrap();
    let events = plane.take_events();
    assert!(events.iter().any(|e| matches!(e, Event::ApprovalsChanged)));
    assert!(events.iter().any(|e| matches!(
        e,
        Event::ApprovalDecision {
            outcome: DecisionOutcome::Decided,
            decision: Decision::Approve,
            ..
        }
    )));
    assert!(plane.approvals().is_empty());
}

#[test]
fn a_refused_decision_on_a_gone_record_is_stale_and_not_retried() {
    let mut plane = watching(&[(id(1), 10)]);
    let (decide_id, confirm) = decide(&mut plane, id(1));
    refuse(&mut plane, decide_id);
    plane
        .feed(FrameKind::MetadataValue {
            request_id: confirm,
            value: None,
        })
        .unwrap();
    assert_eq!(outcome(&mut plane), DecisionOutcome::Stale);
    assert!(plane.approvals().is_empty());
    assert!(outbound(&mut plane).is_empty(), "never retried");
    assert!(
        plane.last_error().is_none(),
        "a stale decision is no failure"
    );
}

#[test]
fn a_refused_decision_on_a_pending_record_keeps_it() {
    let mut plane = watching(&[(id(1), 10)]);
    let (decide_id, confirm) = decide(&mut plane, id(1));
    refuse(&mut plane, decide_id);
    plane
        .feed(FrameKind::MetadataValue {
            request_id: confirm,
            value: Some(record(id(1), 10)),
        })
        .unwrap();
    assert!(matches!(
        outcome(&mut plane),
        DecisionOutcome::Refused {
            code: ErrorCode::PermissionDenied,
            ..
        }
    ));
    assert_eq!(plane.approvals().len(), 1);
}

#[test]
fn a_decision_cut_off_by_the_connection_is_unconfirmed_and_relisted() {
    let mut plane = watching(&[(id(1), 10)]);
    let _ = decide(&mut plane, id(1));
    plane.connection_lost(None);
    assert_eq!(outcome(&mut plane), DecisionOutcome::Unconfirmed);
    plane.connection_opened();
    plane
        .feed(hello_ok_with(&[ServerFeature::Approvals]))
        .unwrap();
    let listing = list_id(&outbound(&mut plane));
    plane
        .feed(FrameKind::MetadataKeys {
            request_id: listing,
            keys: Vec::new(),
        })
        .unwrap();
    assert!(plane.approvals().is_empty());
}
