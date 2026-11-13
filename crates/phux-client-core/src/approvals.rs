//! The shared nonsecret approval record for native and browser clients.

use serde::{Deserialize, Serialize};

/// One action awaiting a decision: its `phux.approval/v1/<id>` record.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Approval {
    /// The approval id, 32 lowercase hex digits.
    pub id: String,
    /// The connection whose command is held.
    pub requester: Requester,
    /// The held command's catalog name, e.g. `KILL_RESOURCE`.
    pub method: String,
    /// The signal, for a held `SIGNAL_TERMINAL`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signal: Option<String>,
    /// The subjects it names, in the registry selector grammar.
    #[serde(default)]
    pub subjects: Vec<String>,
    /// Nonsecret credential request details and the provider bound by the server.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub credential: Option<CredentialApproval>,
    /// When it was held, Unix milliseconds.
    pub requested_at_ms: u64,
    /// When it expires unless decided, Unix milliseconds.
    pub expires_at_ms: u64,
}

/// A credential approval contains references, never resolved secret values.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CredentialApproval {
    /// Exact request to review. Its purpose is untrusted requester text.
    pub request: phux_protocol::credentials::CredentialRequest,
    /// Authenticated provider connection chosen before the approval was published.
    pub provider: Requester,
}

impl Approval {
    /// Parse a `phux.approval/v1/<id>` value.
    ///
    /// # Errors
    /// The value is not a valid record, or a credential request lacks reviewable details.
    pub fn parse(bytes: &[u8]) -> Result<Self, serde_json::Error> {
        let record: Self = serde_json::from_slice(bytes)?;
        record
            .validate_credential()
            .map_err(serde::de::Error::custom)?;
        Ok(record)
    }

    fn validate_credential(&self) -> Result<(), String> {
        let needs_details = matches!(self.method.as_str(), "CREDENTIALS" | "CREDENTIAL_REQUEST");
        let Some(credential) = &self.credential else {
            return if needs_details {
                Err("credential approval requires exact request and provider details".into())
            } else {
                Ok(())
            };
        };
        credential.request.validate()
    }
}

/// The requester's or provider's connection as the journal names an actor.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Requester {
    /// The connection's wire client id.
    pub client: u32,
    /// The workload credential it authenticated with, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub credential_id: Option<String>,
    /// The untrusted name it announced in `HELLO`, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_name: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn credential_methods_require_valid_review_details() {
        for method in ["CREDENTIALS", "CREDENTIAL_REQUEST"] {
            let mut record = json!({
                "id": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                "requester": {"client": 1},
                "method": method,
                "requested_at_ms": 1,
                "expires_at_ms": 120_001
            });
            assert!(Approval::parse(&serde_json::to_vec(&record).unwrap()).is_err());
            record["credential"] = serde_json::Value::Null;
            assert!(Approval::parse(&serde_json::to_vec(&record).unwrap()).is_err());
            record["credential"] = json!({
                "request": {
                    "provider": "desktop",
                    "references": ["op://vault/item/field"],
                    "purpose": "Run tests"
                },
                "provider": {"client": 2}
            });
            assert!(Approval::parse(&serde_json::to_vec(&record).unwrap()).is_ok());
            for references in [json!([]), json!(["op://vault/item/field?attribute=otp"])] {
                record["credential"]["request"]["references"] = references;
                assert!(Approval::parse(&serde_json::to_vec(&record).unwrap()).is_err());
            }
        }
    }
}
