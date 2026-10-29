//! Additive recovery facts shared by CLI and MCP projections.
//!
//! These describe an operation's effects, never task completion. Identities
//! from distinct scopes are not interchangeable. Missing evidence stays absent;
//! a socket path, resource handle or operation id cannot establish a scope id.
use serde::{Deserialize, Serialize};

/// What the operation's receipt establishes about its effects.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EffectCertainty {
    /// No effects were applied.
    NotApplied,
    /// The operation's effects were applied, not necessarily consumed or completed.
    Applied,
    /// Effects may or may not have happened.
    Unknown,
}

/// Where an operation identity has meaning.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScopeKind {
    /// The durable work-store ledger, not the terminal server.
    WorkStore,
    /// The durable organization store of one authority.
    OrganizationAuthority,
    /// A bounded input receipt in one terminal-server incarnation.
    InputServerIncarnation,
    /// One MCP adapter's in-memory observation ledger.
    McpAdapter,
}

/// The identity domain and, only when known, its authoritative identity.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Scope {
    /// The domain; equal operation strings across domains imply nothing.
    pub kind: ScopeKind,
    /// Actual authority/incarnation identity, never inferred from a path or handle.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
}

/// The safe next class of action, not permission to bypass authority checks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NextAction {
    /// An identical request under its original identity and retention rules.
    RetrySameOperation,
    /// Proven non-delivery permits a new operation, subject to normal admission.
    RetryNewOperation,
    /// Inspect authoritative state; do not blindly resubmit an uncertain effect.
    Reconcile,
}

/// Facts attached to an existing error document without replacing its code/type.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Recovery {
    /// The original operation identity, when the source error retained it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub operation_id: Option<String>,
    /// Its domain and any authoritative identity actually observed.
    pub scope: Scope,
    /// Evidence about the operation's effects, not the program or agent's success.
    pub effect_certainty: EffectCertainty,
    /// The safe recovery class.
    pub next_action: NextAction,
}

impl Recovery {
    /// Build recovery facts from typed evidence, without guessing missing identities.
    #[must_use]
    pub const fn new(
        kind: ScopeKind,
        operation_id: Option<String>,
        effect_certainty: EffectCertainty,
        next_action: NextAction,
    ) -> Self {
        Self {
            operation_id,
            scope: Scope { kind, id: None },
            effect_certainty,
            next_action,
        }
    }

    /// Attach an actual observed authority/incarnation. Empty observations stay absent.
    #[must_use]
    pub fn with_scope_id(mut self, id: Option<&str>) -> Self {
        self.scope.id = id.filter(|id| !id.is_empty()).map(str::to_owned);
        self
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "assertions in recovery metadata tests")]
mod tests {
    use super::*;

    #[test]
    fn organization_unknown_failure_retains_its_typed_identity() {
        let operation = phux_protocol::ids::OperationId::new([7; 16]).unwrap();
        let recovery = crate::org::OrgError::OutcomeUnknown {
            operation: Some(operation),
        }
        .recovery();
        assert_eq!(recovery.operation_id, Some(operation.to_hex()));
        assert_eq!(recovery.scope.kind, ScopeKind::OrganizationAuthority);
        assert_eq!(recovery.effect_certainty, EffectCertainty::Unknown);
        assert!(recovery.scope.id.is_none());
    }

    #[test]
    fn prompt_unknown_and_proven_non_delivery_remain_distinct() {
        use crate::agent_prompt::PromptError;
        let unknown = PromptError::DeliveryUnknown {
            operation_id: "original-input".into(),
            message: "not a parseable identity".into(),
        }
        .recovery();
        assert_eq!(unknown.operation_id.as_deref(), Some("original-input"));
        assert_eq!(unknown.effect_certainty, EffectCertainty::Unknown);
        assert_eq!(unknown.next_action, NextAction::Reconcile);
        assert_eq!(unknown.scope.kind, ScopeKind::InputServerIncarnation);
        let not_written = PromptError::NotWritten {
            operation_id: "other-input".into(),
            message: "no bytes".into(),
        }
        .recovery();
        assert_eq!(not_written.effect_certainty, EffectCertainty::NotApplied);
        assert_eq!(not_written.next_action, NextAction::RetryNewOperation);
    }

    #[test]
    fn unknown_identity_is_not_invented() {
        let recovery = Recovery::new(
            ScopeKind::OrganizationAuthority,
            None,
            EffectCertainty::Unknown,
            NextAction::Reconcile,
        )
        .with_scope_id(Some(""));
        let value = serde_json::to_value(&recovery).unwrap();
        assert_eq!(value["scope"]["kind"], "organization_authority");
        assert!(value.get("operation_id").is_none());
        assert!(value["scope"].get("id").is_none());
        assert_eq!(serde_json::from_value::<Recovery>(value).unwrap(), recovery);
    }

    #[test]
    fn input_recovery_preserves_scope_and_uncertainty() {
        let recovery = Recovery::new(
            ScopeKind::InputServerIncarnation,
            Some("input-id".into()),
            EffectCertainty::Unknown,
            NextAction::Reconcile,
        )
        .with_scope_id(Some("observed-incarnation"));
        let value = serde_json::to_value(&recovery).unwrap();
        assert_eq!(value["scope"]["id"], "observed-incarnation");
        assert_eq!(value["operation_id"], "input-id");
        assert_eq!(value["effect_certainty"], "unknown");
        assert_eq!(value["next_action"], "reconcile");
    }
}
