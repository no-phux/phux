//! Tests for the refusal contract.

use std::path::Path;

use super::{Refusal, codes, error_document, transport_refusal};
use crate::attach::AttachError;

#[test]
fn recovery_is_additive_and_never_extracted_from_message_text() {
    use crate::recovery::{EffectCertainty, NextAction, Recovery, ScopeKind};
    let error = Refusal::new(
        "session_create_failed",
        "opaque diagnostic",
        "inspect state",
    )
    .with_cause_code("outcome_unknown")
    .with_recovery(Recovery::new(
        ScopeKind::OrganizationAuthority,
        Some("actual-operation".into()),
        EffectCertainty::Unknown,
        NextAction::Reconcile,
    ));
    let doc = error_document(&error, 1);
    assert_eq!(doc["schema_version"], 1);
    assert_eq!(doc["error"]["code"], "session_create_failed");
    assert_eq!(doc["error"]["cause_code"], "outcome_unknown");
    assert_eq!(doc["error"]["recovery"]["operation_id"], "actual-operation");
    assert_eq!(
        doc["error"]["recovery"]["scope"]["kind"],
        "organization_authority"
    );
    assert!(doc["error"]["recovery"]["scope"].get("id").is_none());
}

/// The printed line keeps the documented key order and carries exactly
/// the document's content.
#[test]
fn the_error_line_is_the_document_in_documented_order() {
    use crate::recovery::{EffectCertainty, NextAction, Recovery, ScopeKind};
    let plain = Refusal::new(codes::NO_SUCH_TARGET, "no such target: @9", "run `phux ls`");
    assert_eq!(
        super::error_line(&plain, 1),
        r#"{"schema_version":1,"error":{"code":"no_such_target","message":"no such target: @9"},"remedy":"run `phux ls`","exit_code":1}"#
    );
    let mut full = plain
        .with_cause_code("outcome_unknown")
        .with_recovery(Recovery::new(
            ScopeKind::OrganizationAuthority,
            Some("op".into()),
            EffectCertainty::Unknown,
            NextAction::Reconcile,
        ));
    full.auto_start_error = Some("bind failed".to_owned());
    let line = super::error_line(&full, 3);
    let parsed: serde_json::Value = serde_json::from_str(&line).unwrap_or_default();
    assert_eq!(parsed, error_document(&full, 3));
    let order = [
        "\"code\"",
        "\"message\"",
        "\"auto_start_error\"",
        "\"recovery\"",
        "\"cause_code\"",
    ];
    let positions: Vec<usize> = order.iter().filter_map(|key| line.find(key)).collect();
    assert_eq!(positions.len(), order.len(), "{line}");
    assert!(positions.is_sorted(), "{line}");
}

/// Every attach-error arm maps onto the closed vocabulary, each with a
/// remedy naming the server log and doctor.
#[test]
fn transport_refusals_use_the_closed_vocabulary() {
    let socket = Path::new("/tmp/phux-test.sock");
    let refused = AttachError::Io(std::io::Error::from(std::io::ErrorKind::ConnectionRefused));
    let err = transport_refusal(&refused, socket, "ls", None);
    assert_eq!(err.code, codes::NO_SERVER);
    assert!(err.message.contains("/tmp/phux-test.sock"));
    assert!(
        err.remedy.contains("`phux server --ensure`"),
        "{}",
        err.remedy
    );
    assert!(err.remedy.contains("server log: "), "{}", err.remedy);
    assert!(err.remedy.contains("phux doctor"));

    let err = transport_refusal(&AttachError::Disconnected, socket, "kill", None);
    assert_eq!(err.code, codes::SERVER_DISCONNECTED);
    assert!(err.message.contains("kill"));
    assert!(err.remedy.contains("server log: "), "{}", err.remedy);

    let denied = AttachError::Io(std::io::Error::from(std::io::ErrorKind::PermissionDenied));
    let err = transport_refusal(&denied, socket, "tag", None);
    assert_eq!(err.code, codes::TRANSPORT);
    assert!(
        err.message
            .starts_with("tag: cannot reach the server socket /tmp/phux-test.sock: "),
        "{}",
        err.message
    );

    let err = transport_refusal(
        &AttachError::Refused("policy said no".to_owned()),
        socket,
        "tag",
        None,
    );
    assert_eq!(err.code, codes::TRANSPORT);
    assert!(err.message.contains("policy said no"));
    assert!(err.remedy.contains("phux doctor"));
}

/// A message carrying `hint: ` remedy lines keeps only its failure as
/// the message; the remedies lead the remedy, one per line.
#[test]
fn hint_lines_in_a_message_move_to_the_remedy() {
    let err = Refusal::new("usage_error", "refused\nhint: do a\nhint: do b", "run help");
    assert_eq!(err.message, "refused");
    assert_eq!(err.remedy, "do a\ndo b\nrun help");
    let plain = Refusal::new("usage_error", "plain\ntwo lines", "x");
    assert_eq!(plain.message, "plain\ntwo lines");
    assert_eq!(plain.remedy, "x");
}

/// An old-protocol server stays on the existing `transport` code. The
/// remedy is the stop-and-start recovery, with the pid when it is known.
#[test]
fn an_incompatible_server_keeps_transport_and_names_the_stop_recovery() {
    const FORBIDDEN: [&str; 6] = [
        "stale",
        "phux upgrade",
        "survive",
        "remove",
        "pkill",
        "killall",
    ];
    let socket = Path::new("/tmp/old.sock");
    let err = AttachError::IncompatibleProtocol(
        "incompatible protocol: client offered 1.4.0, server requires 0.9.x; \
         update the phux server so protocol major.minor match"
            .to_owned(),
    );
    let with_pid = transport_refusal(&err, socket, "ls", Some(4242));
    assert_eq!(with_pid.code, codes::TRANSPORT);
    assert!(
        with_pid.message.contains("server refused the request"),
        "{}",
        with_pid.message
    );
    let remedy = &with_pid.remedy;
    assert!(remedy.contains("kill 4242"), "{remedy}");
    assert!(
        remedy.contains("`ps -p 4242 -o pid=,ppid=,user=,args=`"),
        "{remedy}"
    );
    assert!(
        remedy.contains("phux server --ensure --socket '/tmp/old.sock'`"),
        "{remedy}"
    );
    for forbidden in FORBIDDEN {
        assert!(!remedy.contains(forbidden), "{forbidden}: {remedy}");
        assert!(
            !with_pid.message.contains(forbidden),
            "{forbidden}: {}",
            with_pid.message
        );
    }
    let line = serde_json::to_string(&error_document(&with_pid, 1)).unwrap_or_default();
    assert!(!line.contains('\n'), "error line must be single-line");

    let unknown = transport_refusal(&err, socket, "ls", None);
    assert_eq!(unknown.code, codes::TRANSPORT);
    let remedy = &unknown.remedy;
    assert!(remedy.contains("`lsof -nP -- '/tmp/old.sock'`"), "{remedy}");
    assert!(!remedy.contains("`kill "), "{remedy}");
    for forbidden in FORBIDDEN {
        assert!(!remedy.contains(forbidden), "{forbidden}: {remedy}");
    }
}

/// The agent verbs' codes are part of the closed vocabulary frozen by
/// ADR-0071 point 6.
#[test]
fn the_agent_verb_codes_live_in_the_single_closed_vocabulary() {
    assert_eq!(codes::NO_AGENT_RECORD, "no_agent_record");
    assert_eq!(codes::AGENT_DEPARTED, "agent_departed");
    assert_eq!(codes::AGENT_MISMATCH, "agent_mismatch");
    assert_eq!(codes::INVALID_KEY_SPEC, "invalid_key_spec");
    // Added after ADR-0071 point 6 was written.
    assert_eq!(codes::INPUT_NOT_WRITTEN, "input_not_written");
    assert_eq!(codes::DELIVERY_UNKNOWN, "delivery_unknown");
}

/// The agent session family (`agent session open|close`, `agent emit`,
/// `agent log`) spells its codes here, `snake_case`, so the Claude shim and
/// the MCP adapter branch on one vocabulary.
#[test]
fn the_agent_session_codes_live_in_the_single_closed_vocabulary() {
    assert_eq!(codes::UNSUPPORTED_SERVER, "unsupported_server");
    assert_eq!(codes::NO_AGENT_SESSION, "no_agent_session");
    assert_eq!(codes::AGENT_SESSION_AMBIGUOUS, "agent_session_ambiguous");
    assert_eq!(codes::WRONG_RESOURCE_KIND, "wrong_resource_kind");
    assert_eq!(codes::NOT_PRODUCER, "not_producer");
    assert_eq!(codes::RECORD_INVALID, "record_invalid");
    assert_eq!(codes::OVERFLOW, "overflow");
    assert_eq!(codes::PARENT_NOT_FOUND, "parent_not_found");
    assert_eq!(codes::PARENT_KIND_MISMATCH, "parent_kind_mismatch");
    assert_eq!(codes::AGENT_SESSION_REFUSED, "agent_session_refused");
    assert_eq!(codes::AGENT_WITHDRAWN, "agent_withdrawn");
}
