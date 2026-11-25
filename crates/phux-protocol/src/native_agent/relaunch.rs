//! An explicit owner decision, not persistent archive trust.
use serde::{Deserialize, Serialize};

/// Complete executing-host proposal displayed before local owner approval.
/// Changing any field requires a new explicit decision. Import never creates it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RelaunchApproval {
    /// Current executing host incarnation; never an archive incarnation.
    pub instance: [u8; 16],
    /// Exact relaunch operation, as nonzero 32-digit lowercase hex.
    #[serde(with = "operation_hex")]
    pub operation: crate::ids::OperationId,
    /// Exact dormant target.
    pub resource: crate::ids::ResourceKey,
    /// Exact target revision, including its activation and saved recipe.
    pub expected: crate::ids::Revision,
    /// Exact observed activation and execution identity, or null if never started.
    pub activation: serde_json::Value,
    /// Complete stored executable intent, including env, cwd and resume identity.
    pub recipe: serde_json::Value,
    /// Concrete execution after resolving current shell, TERM and cwd.
    /// Displayed alongside the original recipe and checked again.
    pub execution: serde_json::Value,
}

/// The native JSON projection deliberately does not define global ID serde.
pub(super) mod operation_hex {
    use crate::ids::{Hex16, OperationId, parse_hex16};
    use serde::{
        Deserializer, Serializer,
        de::{Error, Visitor},
    };
    use std::fmt;

    pub(in crate::native_agent) fn serialize<S: Serializer>(
        operation: &OperationId,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        serializer.collect_str(&Hex16(operation.as_bytes()))
    }

    pub(in crate::native_agent) fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<OperationId, D::Error> {
        struct OperationVisitor;
        impl Visitor<'_> for OperationVisitor {
            type Value = OperationId;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("a nonzero 32-digit hexadecimal operation identity")
            }

            fn visit_str<E: Error>(self, value: &str) -> Result<Self::Value, E> {
                parse_hex16(value)
                    .and_then(OperationId::new)
                    .ok_or_else(|| E::custom("operation must be nonzero 32-hex"))
            }
        }
        deserializer.deserialize_str(OperationVisitor)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "native JSON contract assertions")]
mod tests {
    use super::*;
    use crate::native_agent::{NativeRequest, NativeRequestPayload};
    use serde_json::{Value, json};

    const OPERATION: &str = "0123456789abcdef0123456789abcdef";
    const PREPARE: &str = r#"{"type":"prepare_relaunch","operation":"0123456789abcdef0123456789abcdef","resource":"aebagbafaydqqcikbmga2dqpca","expected":7}"#;

    fn approval_json() -> Value {
        json!({
            "instance": [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16],
            "operation": "0123456789abcdef0123456789abcdef",
            "resource": "aebagbafaydqqcikbmga2dqpca",
            "expected": 7,
            "activation": null,
            "recipe": {"argv": ["sh", "-l"], "env": {"LANG": "C"}},
            "execution": {"argv": ["/bin/sh", "-l"], "cwd": "/tmp"}
        })
    }

    #[test]
    fn native_operation_json_is_explicit_canonical_hex_and_debug_is_redacted() {
        let payload = NativeRequestPayload::decode(PREPARE.as_bytes()).unwrap();
        assert_eq!(payload.encoded(), PREPARE.as_bytes());
        let NativeRequest::PrepareRelaunch { operation, .. } = payload.request() else {
            panic!("prepare relaunch");
        };
        assert_eq!(operation.to_hex(), OPERATION);
        assert_eq!(
            operation.as_bytes(),
            &[
                1, 35, 69, 103, 137, 171, 205, 239, 1, 35, 69, 103, 137, 171, 205, 239
            ]
        );
        assert_eq!(format!("{operation:?}"), "IdempotencyKey(<redacted>)");
        let proposal = approval_json();
        let approval: RelaunchApproval = serde_json::from_value(proposal.clone()).unwrap();
        assert_eq!(serde_json::to_value(&approval).unwrap(), proposal);
        assert_eq!(approval.operation, *operation);
        assert!(!format!("{approval:?}").contains(OPERATION));
        let request = json!({"type": "approve_relaunch", "approval": proposal});
        let decoded = NativeRequestPayload::decode(&serde_json::to_vec(&request).unwrap()).unwrap();
        assert_eq!(serde_json::to_value(decoded.request()).unwrap(), request);
    }

    #[test]
    fn native_operation_json_rejects_zero_malformed_and_nonstring_ids() {
        for invalid in [
            json!("00000000000000000000000000000000"),
            json!("0123456789abcdef0123456789abcde"),
            json!("0123456789abcdef0123456789abcdef0"),
            json!("0123456789abcdef0123456789abcdeg"),
            json!(" 123456789abcdef0123456789abcdef"),
            json!("é123456789abcdef0123456789abcde"),
            json!(123),
            json!([1, 2, 3]),
            Value::Null,
        ] {
            let mut prepare: Value = serde_json::from_str(PREPARE).unwrap();
            prepare["operation"] = invalid.clone();
            assert!(NativeRequestPayload::decode(&serde_json::to_vec(&prepare).unwrap()).is_err());
            let mut approval = approval_json();
            approval["operation"] = invalid;
            assert!(serde_json::from_value::<RelaunchApproval>(approval.clone()).is_err());
            let request = json!({"type": "approve_relaunch", "approval": approval});
            assert!(NativeRequestPayload::decode(&serde_json::to_vec(&request).unwrap()).is_err());
        }
    }
}
