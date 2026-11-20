use super::*;
use phux_rune::mobile;

struct UnusedSigner;
impl mobile::DeviceSigner for UnusedSigner {
    fn public_point(&self) -> Vec<u8> {
        panic!("invalid ticket must fail first")
    }
    fn sign(&self, _: &[u8]) -> Result<Vec<u8>, String> {
        panic!("no signing expected")
    }
}

#[test]
fn real_blocking_enrollment_is_captured_and_sealed_before_any_consumer() {
    const CHILD: &str = "RUNE_ENROLL_TRACE_TEST_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact",
                "diagnostics::enrollment_tests::real_blocking_enrollment_is_captured_and_sealed_before_any_consumer",
                "--nocapture"])
            .env(CHILD, "1").output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }
    let path = std::env::temp_dir().join(format!("rune-enroll-trace-{}.jsonl", std::process::id()));
    start(&path).unwrap();
    std::thread::spawn(|| {
        let configuration: mobile::Configuration = serde_json::from_value(json!({
            "endpoint": "quic://127.0.0.1:1", "authority_ca": "ab".repeat(32),
            "certificate_pin": "cd".repeat(32), "token": "ef".repeat(32),
        }))
        .unwrap();
        let target = configuration.enrollment_target().unwrap();
        assert!(
            mobile::enroll_target_blocking(
                &target,
                "private-ticket-sentinel",
                Arc::new(UnusedSigner),
            )
            .is_err()
        );
    })
    .join()
    .unwrap();
    finish().unwrap();
    let text = std::fs::read_to_string(&path).unwrap();
    assert!(!text.contains("private-") && !text.contains("127.0.0.1"));
    let rows: Vec<Value> = text
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(
        rows.iter()
            .map(|row| row["phase"].as_str().unwrap())
            .collect::<Vec<_>>(),
        [
            "attempt_started",
            "enroll_runtime",
            "enroll_validate",
            "enroll_failed"
        ]
    );
    assert!(rows.iter().all(|row| row.as_object().unwrap().len() == 5));
    let status: Value =
        serde_json::from_slice(&std::fs::read(path.with_extension("status.json")).unwrap())
            .unwrap();
    assert_eq!(
        status,
        json!({"complete": true, "failed": false, "events": 4})
    );
    assert!(!path.with_extension("status.pending").exists());
    std::fs::remove_file(path.with_extension("status.json")).unwrap();
    std::fs::remove_file(path).unwrap();
}
