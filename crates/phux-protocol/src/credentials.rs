//! Ephemeral, explicitly approved 1Password exchanges. Values never belong in metadata.

use serde::{Deserialize, Serialize};
use zeroize::Zeroize;

/// Maximum references in one approved request.
pub const MAX_REFERENCES: usize = 16;
/// Maximum size of one UTF-8 secret field.
pub const MAX_FIELD_BYTES: usize = 64 * 1024;
/// Maximum combined UTF-8 bytes in a reply.
pub const MAX_SECRET_BYTES: usize = 128 * 1024;
/// Maximum encoded credential message, including JSON escaping.
pub const MAX_CREDENTIAL_PAYLOAD: usize = 1024 * 1024;

/// Exact references and destination shown before approval.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CredentialRequest {
    /// Name of an online provider connection; bound before approval.
    pub provider: String,
    /// Exact fields, in reply order. No templates or query parameters.
    pub references: Vec<String>,
    /// Untrusted human-readable description of the intended consumer.
    pub purpose: String,
}

impl CredentialRequest {
    /// Reject unbounded, ambiguous or terminal-control-bearing requests.
    ///
    /// # Errors
    /// A payload-free diagnostic when validation fails.
    pub fn validate(&self) -> Result<(), String> {
        validate_provider(&self.provider)?;
        if self.purpose.is_empty()
            || self.purpose.len() > 512
            || self.purpose.chars().any(char::is_control)
        {
            return Err("credential purpose must be 1..512 printable bytes".into());
        }
        if self.references.is_empty() || self.references.len() > MAX_REFERENCES {
            return Err("credential request needs 1..16 references".into());
        }
        for reference in &self.references {
            validate_reference(reference)?;
        }
        Ok(())
    }
}

/// Validate a provider label without interpreting it as a host identity.
/// # Errors
/// An invalid label.
pub fn validate_provider(provider: &str) -> Result<(), String> {
    if provider.is_empty()
        || provider.len() > 64
        || !provider
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b))
    {
        return Err(
            "provider name must be 1..64 ASCII letters, digits, dash, underscore or dot".into(),
        );
    }
    Ok(())
}

/// Validate an exact 1Password field URI. Intentionally excludes template variables,
/// attachments with arbitrary characters, OTP queries and private-key export modifiers.
/// # Errors
/// An invalid reference (the reference is never echoed).
pub fn validate_reference(reference: &str) -> Result<(), String> {
    let path = reference
        .strip_prefix("op://")
        .ok_or("expected an exact op:// field reference")?;
    let parts: Vec<_> = path.split('/').collect();
    if reference.len() > 1024 || !(3..=4).contains(&parts.len()) {
        return Err("expected op://vault/item/[section/]field".into());
    }
    for part in parts {
        if part.trim().is_empty()
            || !part
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"-_. ".contains(&b))
        {
            return Err("unsupported 1Password reference component; use item and field IDs".into());
        }
    }
    Ok(())
}

/// Secret values in request order. Debug output is always redacted.
/// This is an in-memory transport value, not a storage format.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SecretValues(pub Vec<String>);

impl std::fmt::Debug for SecretValues {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SecretValues([REDACTED])")
    }
}

impl Drop for SecretValues {
    fn drop(&mut self) {
        self.0.zeroize();
    }
}

impl SecretValues {
    /// Check size and suitability for environment injection.
    /// # Errors
    /// A payload-free diagnostic for an invalid value set.
    pub fn validate(&self) -> Result<(), String> {
        if self.0.is_empty()
            || self.0.len() > MAX_REFERENCES
            || self.0.iter().map(String::len).sum::<usize>() > MAX_SECRET_BYTES
        {
            return Err("credential reply exceeds limits".into());
        }
        if self
            .0
            .iter()
            .any(|value| value.contains('\0') || value.len() > MAX_FIELD_BYTES)
        {
            return Err("credential values must be bounded UTF-8 without NUL".into());
        }
        Ok(())
    }
}

/// A credential broker operation. No operation has replay semantics.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
pub enum CredentialOp {
    /// Wait for explicit approval, then receive values once on this connection.
    Request(CredentialRequest),
    /// Register this connection as a named provider and claim one approved request.
    Next {
        /// Provider label. A concurrent connection cannot take it over.
        provider: String,
    },
    /// Settle a claimed request; `None` means provider/unlock failure.
    Complete {
        /// Random approval/exchange identifier.
        id: String,
        /// Values, or failure. Never an error containing provider output.
        values: Option<SecretValues>,
    },
}

impl CredentialOp {
    /// Validate before dispatch or encoding.
    /// # Errors
    /// A payload-free diagnostic.
    pub fn validate(&self) -> Result<(), String> {
        match self {
            Self::Request(request) => request.validate(),
            Self::Next { provider } => validate_provider(provider),
            Self::Complete { id, values } => {
                if crate::ids::ApprovalId::parse(id).is_none() {
                    return Err("invalid credential exchange id".into());
                }
                if let Some(values) = values {
                    values.validate()?;
                }
                Ok(())
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_checklist_capability_does_not_advertise_credentials() {
        use crate::caps::{ServerFeatureExt, ServerFeatureExtSet};

        let checklist = ServerFeatureExtSet::from_wire(0x0080_0000);
        assert!(!checklist.contains(ServerFeatureExt::Credentials));
        let credentials = ServerFeatureExtSet::with(&[ServerFeatureExt::Credentials]);
        assert_eq!(credentials.as_wire(), 0x0100_0000);
    }

    #[test]
    fn references_are_exact_and_printable() {
        for good in [
            "op://dev/GitHub/token",
            "op://dev/Item name/Section/field-id",
        ] {
            assert!(validate_reference(good).is_ok());
        }
        for bad in [
            "op://$ENV/item/token",
            "op://a/b/c?attribute=otp",
            "op://a/b/\u{1b}",
            "op://a//c",
            "op://a/b/",
            "op://a/b/c/d/e",
            "--help",
        ] {
            assert!(validate_reference(bad).is_err());
        }
    }

    #[test]
    fn nested_debug_never_contains_values() {
        let op = CredentialOp::Complete {
            id: "01010101010101010101010101010101".into(),
            values: Some(SecretValues(vec!["canary-secret".into()])),
        };
        assert!(!format!("{op:?}").contains("canary-secret"));
        assert!(SecretValues(vec!["bad\0value".into()]).validate().is_err());
    }

    #[test]
    fn every_operation_and_secret_reply_round_trips() {
        use crate::wire::frame::{Command, CommandResult, CommandValue, FrameKind};
        let request = CredentialRequest {
            provider: "mac".into(),
            references: vec!["op://dev/item/token".into()],
            purpose: "coding agent".into(),
        };
        let values = SecretValues(vec!["not-a-real-secret\nwith-newline".into()]);
        let ops = [
            CredentialOp::Request(request),
            CredentialOp::Next {
                provider: "mac".into(),
            },
            CredentialOp::Complete {
                id: "01010101010101010101010101010101".into(),
                values: Some(values.clone()),
            },
            CredentialOp::Complete {
                id: "01010101010101010101010101010101".into(),
                values: None,
            },
        ];
        let mut frames: Vec<_> = ops
            .into_iter()
            .map(|op| FrameKind::Command {
                request_id: 7,
                command: Command::Credentials { op },
            })
            .collect();
        frames.push(FrameKind::CommandResult {
            request_id: 7,
            result: CommandResult::OkWith(CommandValue::CredentialSecrets(values)),
        });
        for frame in frames {
            let mut bytes = bytes::BytesMut::new();
            frame.encode(&mut bytes);
            let (decoded, tail) = FrameKind::decode(&bytes).unwrap();
            assert_eq!(decoded, frame);
            assert!(tail.is_empty());
            assert!(!format!("{decoded:?}").contains("not-a-real-secret"));
        }
    }

    #[test]
    fn invalid_operations_and_secret_replies_are_rejected_by_the_wire() {
        use crate::wire::frame::{Command, CommandResult, CommandValue, FrameKind};
        let frames = [
            FrameKind::Command {
                request_id: 1,
                command: Command::Credentials {
                    op: CredentialOp::Next {
                        provider: "bad\nlabel".into(),
                    },
                },
            },
            FrameKind::CommandResult {
                request_id: 1,
                result: CommandResult::OkWith(CommandValue::CredentialSecrets(SecretValues(vec![
                    "x".repeat(MAX_SECRET_BYTES + 1),
                ]))),
            },
        ];
        for frame in frames {
            let mut bytes = bytes::BytesMut::new();
            frame.encode(&mut bytes);
            assert!(FrameKind::decode(&bytes).is_err());
        }
    }
}
