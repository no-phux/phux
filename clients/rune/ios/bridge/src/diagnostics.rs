//! Opt-in simulator-fixture trace. No ordinary app installs a subscriber.
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::Path;
use std::sync::{Mutex, PoisonError};
use std::time::Instant;

use serde_json::{Map, Value, json};
use tracing::field::{Field, Visit};
use tracing::span::{Attributes, Id, Record};
use tracing::{Event, Metadata, Subscriber};

const TARGET: &str = "phux_client_runtime::connection_diagnostics";
// Deny unknown text even on this target. In particular, a future raw-error or
// host field cannot silently turn fixture diagnostics into a credential log.
const PHASES: &[&str] = &[
    "attempt_started",
    "dial_started",
    "dial_connected",
    "dial_deadline",
    "hello_sent",
    "hello_received",
    "hello_accepted",
    "topology_received",
    "topology_applied",
    "attach_ready",
    "initial_deadline",
    "end_closed",
    "end_dropped",
    "end_refused",
    "end_resync",
    "session_ended",
    "resync_requested",
    "nudge_requested",
    "path_changed",
    "probe_deadline",
];

struct Capture {
    file: Mutex<File>,
    started: Instant,
}

pub(super) fn start(path: &Path) -> Result<(), String> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    let file = options
        .open(path)
        .map_err(|_| "Could not create the private connection trace.")?;
    tracing::subscriber::set_global_default(Capture {
        file: Mutex::new(file),
        started: Instant::now(),
    })
    .map_err(|_| "A tracing subscriber is already installed.".to_owned())
}

#[derive(Default)]
struct Fields(Map<String, Value>);

impl Visit for Fields {
    fn record_u64(&mut self, field: &Field, value: u64) {
        if matches!(field.name(), "connection" | "attempt") && value == 0 {
            return;
        }
        if matches!(field.name(), "connection" | "attempt" | "elapsed_ms") {
            self.0.insert(field.name().to_owned(), value.into());
        }
    }

    fn record_str(&mut self, field: &Field, value: &str) {
        if field.name() == "phase" && PHASES.contains(&value) {
            self.0.insert("phase".to_owned(), value.into());
        }
    }

    fn record_debug(&mut self, _: &Field, _: &dyn std::fmt::Debug) {}
}

impl Subscriber for Capture {
    fn enabled(&self, metadata: &Metadata<'_>) -> bool {
        metadata.is_event() && metadata.target() == TARGET
    }

    fn event(&self, event: &Event<'_>) {
        if !self.enabled(event.metadata()) {
            return;
        }
        let mut fields = Fields::default();
        event.record(&mut fields);
        if fields.0.len() != 4 {
            return;
        }
        let mut file = self.file.lock().unwrap_or_else(PoisonError::into_inner);
        fields.0.insert(
            "process_elapsed_ms".to_owned(),
            json!(u64::try_from(self.started.elapsed().as_millis()).unwrap_or(u64::MAX)),
        );
        // One complete JSON line per event, no buffered tail to lose when the
        // driver expires or the qualification process is terminated.
        let mut line = Value::Object(fields.0).to_string();
        line.push('\n');
        if file.write_all(line.as_bytes()).is_err() {
            eprintln!("private connection trace write failed");
        }
    }

    fn new_span(&self, _: &Attributes<'_>) -> Id {
        Id::from_u64(1)
    }
    fn record(&self, _: &Id, _: &Record<'_>) {}
    fn record_follows_from(&self, _: &Id, _: &Id) {}
    fn enter(&self, _: &Id) {}
    fn exit(&self, _: &Id) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_allowlisted_connection_fields_reach_disk() {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path =
            std::env::temp_dir().join(format!("rune-trace-{}-{nonce}.jsonl", std::process::id()));
        let file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&path)
            .unwrap();
        let capture = Capture {
            file: Mutex::new(file),
            started: Instant::now(),
        };
        tracing::subscriber::with_default(capture, || {
            tracing::info!(target: TARGET, connection = 1_u64, attempt = 2_u64,
                elapsed_ms = 18_000_u64, phase = "initial_deadline",
                host = "private-host-sentinel", token = "private-token-sentinel",
                error = "private-error-sentinel");
            tracing::info!(target: TARGET, connection = 1_u64, attempt = 2_u64,
                elapsed_ms = 18_001_u64, phase = "private-unknown-phase-sentinel");
            tracing::info!(target: TARGET, connection = 0_u64, attempt = 2_u64,
                elapsed_ms = 18_001_u64, phase = "initial_deadline");
            tracing::info!(target: TARGET, connection = "private-not-a-number", attempt = 2_u64,
                elapsed_ms = 18_001_u64, phase = "initial_deadline");
            tracing::info!(target: "other", message = "private-other-target-sentinel");
        });
        let text = std::fs::read_to_string(&path).unwrap();
        std::fs::remove_file(path).unwrap();
        assert_eq!(text.lines().count(), 1);
        assert!(!text.contains("private-"));
        let row: Value = serde_json::from_str(text.trim()).unwrap();
        assert_eq!(row["phase"], "initial_deadline");
        assert_eq!(row["attempt"], 2);
        assert_eq!(row.as_object().unwrap().len(), 5);
    }

    #[test]
    fn capture_refuses_existing_files_without_overwriting_them() {
        // The fixture's create_new failure occurs before any global subscriber
        // is installed, so this unit test never changes process-wide tracing.
        let path = std::env::current_exe().unwrap();
        assert!(start(&path).is_err());
    }

    #[test]
    fn global_capture_is_process_wide_exclusive_and_private() {
        const CHILD: &str = "RUNE_TRACE_TEST_CHILD";
        if std::env::var_os(CHILD).is_none() {
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "diagnostics::tests::global_capture_is_process_wide_exclusive_and_private",
                    "--nocapture",
                ])
                .env(CHILD, "1")
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            print!("{}", String::from_utf8_lossy(&output.stdout));
            return;
        }
        let path =
            std::env::temp_dir().join(format!("rune-global-trace-{}.jsonl", std::process::id()));
        start(&path).unwrap();
        assert!(start(&path).is_err(), "a duplicate start must not clobber");
        let other = path.with_extension("other.jsonl");
        assert!(
            start(&other).is_err(),
            "a second subscriber must not replace the first"
        );
        std::thread::spawn(|| {
            for phase in [
                "attempt_started",
                "dial_started",
                "dial_connected",
                "hello_sent",
                "hello_received",
                "hello_accepted",
                "initial_deadline",
                "session_ended",
            ] {
                tracing::info!(target: TARGET, connection = 1_u64, attempt = 1_u64,
                    elapsed_ms = 18_000_u64, phase);
            }
            for phase in [
                "attempt_started",
                "dial_started",
                "dial_connected",
                "hello_sent",
                "hello_received",
                "hello_accepted",
                "topology_received",
                "topology_applied",
            ] {
                tracing::info!(target: TARGET, connection = 2_u64, attempt = 1_u64,
                    elapsed_ms = 18_001_u64, phase);
            }
        })
        .join()
        .unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert_eq!(text.lines().count(), 16);
        assert!(text.contains("initial_deadline") && text.contains("topology_applied"));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            assert_eq!(
                std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        assert_eq!(std::fs::metadata(&other).unwrap().len(), 0);
        // An explicit test-only output directory retains actual subscriber bytes
        // for the collector's validation; ordinary unit runs clean up.
        if let Some(directory) = std::env::var_os("RUNE_TRACE_TEST_EVIDENCE") {
            std::fs::copy(
                &path,
                std::path::PathBuf::from(directory).join("subscriber.jsonl"),
            )
            .unwrap();
        }
        std::fs::remove_file(path).unwrap();
        std::fs::remove_file(other).unwrap();
    }
}
