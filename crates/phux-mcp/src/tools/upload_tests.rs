#[cfg(test)]
mod upload_tests {
    #![allow(clippy::expect_used, clippy::unwrap_used, reason = "tests")]
    use super::*;

    #[tokio::test]
    async fn upload_rejects_invalid_options_before_wire_or_files() {
        for bad in [
            json!({"timeout_secs": 0}),
            json!({"timeout_secs": 3601}),
            json!({"timeout_secs": 1.5}),
            json!({"timeout_secs": "1"}),
            json!({"extension": "../x"}),
            json!({"extension": ""}),
            json!({"extension": "abcdefghijklmnopq"}),
            json!({"path": ""}),
            json!({"path": 12}),
            json!({"path": "bad\u{0}path"}),
            json!({"socket": 12}),
            json!({"bogus": true}),
        ] {
            let mut args = json!({"target": "@1", "path": "/nonexistent/file", "socket": "/nonexistent/socket"});
            args.as_object_mut()
                .unwrap()
                .extend(bad.as_object().unwrap().clone());
            let error = dispatch("phux_upload", &args).await.unwrap_err();
            assert!(
                !error.0.contains("could not read upload file"),
                "{}",
                error.0
            );
            assert!(!error.0.contains("No such file"), "{}", error.0);
        }
    }

    #[test]
    fn upload_documents_preserve_exact_receipts_for_all_outcomes() {
        use phux_client::upload::{UploadOutcome, UploadReceipt};
        for outcome in [
            UploadOutcome::Completed,
            UploadOutcome::Refused,
            UploadOutcome::Unknown,
        ] {
            let receipt = UploadReceipt {
                terminal_id: phux_protocol::ResourceId::satellite("build", 7),
                transfer_id: 42,
                outcome,
                path: (outcome == UploadOutcome::Completed).then(|| "/owner/file.bin".into()),
                code: Some(3),
                message: "detail".into(),
            };
            let expected = serde_json::to_value(&receipt).unwrap();
            let result = upload_document(&receipt);
            let doc = if outcome == UploadOutcome::Completed {
                result.unwrap()
            } else {
                serde_json::from_str::<Value>(&result.unwrap_err().0).unwrap()
            };
            assert_eq!(doc["receipt"], expected);
            if outcome != UploadOutcome::Completed {
                assert!(
                    doc["warning"]
                        .as_str()
                        .unwrap()
                        .contains("do not blindly retry")
                );
            }
        }
    }
}
