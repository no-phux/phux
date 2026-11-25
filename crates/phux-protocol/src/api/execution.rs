//! Public exact execution queries, separate from a resource's latest activation.
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::{ExecutionObservation, schemas};
use crate::ids::{AuthorityId, ResourceKey, parse_hex16};

/// Maximum time budget for an exact execution wait.
pub const MAX_EXECUTION_WAIT_SECS: u32 = 3600;

/// The immutable identity of one activation on its owning authority.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ExecutionRef {
    /// Durable owning authority, not a host alias.
    #[schemars(schema_with = "schemas::durable_key")]
    pub authority: String,
    /// Full durable resource key, not a live `@N` handle.
    #[schemars(schema_with = "schemas::durable_key")]
    pub resource: String,
    /// Monotonic activation number within the resource.
    #[schemars(range(min = 1))]
    pub activation: u64,
    /// Incarnation recorded on the activation, not the current connection.
    #[schemars(schema_with = "schemas::retry_key")]
    pub instance: String,
}

impl ExecutionRef {
    /// Pin a resource's observed activation for a later exact read.
    #[must_use]
    pub fn from_observation(
        authority: &str,
        resource: &str,
        observation: &ExecutionObservation,
    ) -> Self {
        Self {
            authority: authority.to_owned(),
            resource: resource.to_owned(),
            activation: observation.activation,
            instance: observation.instance.clone(),
        }
    }

    /// Validate and normalize the exact identity before I/O.
    ///
    /// # Errors
    ///
    /// A malformed or zero identity.
    pub fn validated(&self) -> Result<(AuthorityId, ResourceKey, [u8; 16]), &'static str> {
        let authority = AuthorityId::parse(&self.authority).ok_or("invalid authority key")?;
        let resource = ResourceKey::parse(&self.resource).ok_or("invalid resource key")?;
        let instance = parse_hex16(&self.instance)
            .filter(|bytes| *bytes != [0; 16])
            .ok_or("instance must be nonzero 32-hex")?;
        if self.activation == 0 {
            return Err("activation must be nonzero");
        }
        Ok((authority, resource, instance))
    }
}

/// Read once if `timeout_secs` is absent, or wait while this activation is active.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ExecutionResultRequest {
    /// Exact execution identity, as reported by its launch or resource inspection.
    pub execution: ExecutionRef,
    /// Whole-operation wait budget in seconds, including reconnects.
    #[serde(default)]
    #[schemars(range(min = 1, max = 3600))]
    pub timeout_secs: Option<u32>,
}

/// Outcome of the read or wait, independent of the process exit status.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionResultStatus {
    /// Exact activation observed. Consult its state and outcome separately.
    Observed,
    /// The resource now has a newer activation; the old result is not retained.
    Superseded,
    /// No matching execution evidence is visible. This does not prove it never ran.
    Unavailable,
    /// Wait expired before obtaining a settled observation.
    TimedOut,
}

/// Read-only exact execution result. Never carries another activation's outcome.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ExecutionResult {
    /// Document revision.
    #[schemars(range(min = 1, max = 1))]
    pub schema_version: u32,
    /// Requested exact execution identity.
    pub execution: ExecutionRef,
    /// Result of observing, not whether the process succeeded.
    pub status: ExecutionResultStatus,
    /// Last exact observation, when available. `lost` is not a known exit.
    pub observation: Option<ExecutionObservation>,
}

/// Input schema derived from the public typed request.
#[must_use]
pub fn execution_result_request_schema() -> serde_json::Value {
    schemars::schema_for!(ExecutionResultRequest).to_value()
}

/// Output schema derived from the public typed result.
#[must_use]
pub fn execution_result_schema() -> serde_json::Value {
    schemars::schema_for!(ExecutionResult).to_value()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_identity_matches_the_nonzero_hex_schema() {
        let key = "aebagbafaydqqcikbmga2dqpca";
        let mut reference = ExecutionRef {
            authority: key.to_owned(),
            resource: key.to_owned(),
            activation: 1,
            instance: "ABCDEF1234567890ABCDEF1234567890".to_owned(),
        };
        assert!(reference.validated().is_ok());
        for instance in [
            "00000000000000000000000000000000",
            "+1000000000000000000000000000000",
        ] {
            instance.clone_into(&mut reference.instance);
            assert!(reference.validated().is_err());
        }
    }
}
