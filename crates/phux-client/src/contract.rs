//! The stable `--json` error contract every surface reports a failure on,
//! the CLI's `--json` verbs and the MCP tools alike (ADR-0065 §4):
//!
//! ```json
//! {"schema_version":1,"error":{"code":"no_server","message":"..."},"remedy":"...","exit_code":1}
//! ```
//!
//! [`Refusal`] is one failure (a stable `code` from [`codes`], the human
//! `message`, the `remedy`) and [`error_document`] renders it. Each surface
//! decorates and prints the refusal on its own channel; the CLI's
//! process-global auto-start context stays in the CLI, because a long-lived
//! MCP server would read it stale. Consumers read
//! `docs/scripting.md#json-output-and-errors`.

use crate::recovery::Recovery;

/// Version of the JSON error document. Additive fields do not bump it.
pub const ERROR_SCHEMA_VERSION: u8 = 1;

/// The closed vocabulary of stable error codes. Consumers branch on these,
/// so renaming one is a breaking change; add new codes here.
#[expect(
    clippy::too_long_first_doc_paragraph,
    reason = "each doc comment is one meaning, catalogued verbatim by `phux --capabilities`"
)]
pub mod codes;

/// One failure, carrying everything every output channel needs: a stable
/// machine `code`, the human `message`, and the `remedy` naming the way out.
#[derive(Debug, Clone)]
pub struct Refusal {
    /// Stable code from [`codes`].
    pub code: &'static str,
    /// What went wrong, for a person.
    pub message: String,
    /// The way out; lines separate independent steps.
    pub remedy: String,
    /// Why this process's auto-start of the server failed, when it tried;
    /// emitted as `error.auto_start_error`.
    pub auto_start_error: Option<String>,
    /// Cold metadata stays off the common error's stack footprint.
    context: Option<Box<RefusalContext>>,
}

#[derive(Debug, Clone, Default)]
struct RefusalContext {
    recovery: Option<Recovery>,
    cause_code: Option<&'static str>,
}

impl Refusal {
    /// A refusal with no recovery, cause code, or auto-start context.
    pub fn new(code: &'static str, message: impl Into<String>, remedy: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            remedy: remedy.into(),
            auto_start_error: None,
            context: None,
        }
    }

    /// Attach the typed recovery, emitted as `error.recovery`.
    #[must_use]
    pub fn with_recovery(mut self, recovery: Recovery) -> Self {
        self.context.get_or_insert_with(Box::default).recovery = Some(recovery);
        self
    }

    /// Attach the underlying cause's code, emitted as `error.cause_code`.
    #[must_use]
    pub fn with_cause_code(mut self, code: &'static str) -> Self {
        self.context.get_or_insert_with(Box::default).cause_code = Some(code);
        self
    }
}

/// The JSON error document for `err`. `exit_code` is embedded because a
/// consumer reading stderr may not see the process status.
#[must_use]
pub fn error_document(err: &Refusal, exit_code: u8) -> serde_json::Value {
    let mut error = serde_json::json!({ "code": err.code, "message": err.message });
    if let Some(auto_start_error) = &err.auto_start_error {
        error["auto_start_error"] = serde_json::Value::from(auto_start_error.as_str());
    }
    if let Some(context) = &err.context {
        if let Some(recovery) = &context.recovery {
            error["recovery"] = serde_json::json!(recovery);
        }
        if let Some(code) = context.cause_code {
            error["cause_code"] = serde_json::json!(code);
        }
    }
    serde_json::json!({
        "schema_version": ERROR_SCHEMA_VERSION,
        "error": error,
        "remedy": err.remedy,
        "exit_code": exit_code,
    })
}

#[cfg(test)]
mod tests {
    use super::{ERROR_SCHEMA_VERSION, Refusal, codes, error_document};

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

    /// The emit shape, pinned: `schema_version` 1, nested error object with
    /// code and message, top-level `remedy` and `exit_code`.
    #[test]
    fn error_document_pins_the_contract_shape() {
        let err = Refusal::new(codes::NO_SERVER, "no server running at /tmp/x", "start one");
        let doc = error_document(&err, 1);
        assert_eq!(doc["schema_version"], u64::from(ERROR_SCHEMA_VERSION));
        assert_eq!(doc["error"]["code"], "no_server");
        assert_eq!(doc["error"]["message"], "no server running at /tmp/x");
        assert_eq!(doc["remedy"], "start one");
        assert_eq!(doc["exit_code"], 1);
        // Exactly the four top-level keys; a consumer may deny-list nothing.
        assert_eq!(doc.as_object().map(serde_json::Map::len), Some(4));
        // The emitted form is one line.
        let line = serde_json::to_string(&doc).unwrap_or_default();
        assert!(!line.contains('\n'), "error line must be single-line");
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
}
