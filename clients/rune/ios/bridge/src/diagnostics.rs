//! Opt-in simulator-fixture trace. No ordinary app installs a subscriber.
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock, PoisonError};
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
    "input_injected",
    "input_refused",
    "enroll_runtime",
    "enroll_validate",
    "enroll_plan",
    "enroll_public_key",
    "enroll_csr",
    "enroll_prepare",
    "enroll_connect",
    "enroll_relay_connect",
    "enroll_open_stream",
    "enroll_write",
    "enroll_read",
    "enroll_drain",
    "enroll_decode",
    "enroll_chain",
    "enroll_succeeded",
    "enroll_failed",
    "enroll_deadline",
    "enroll_refused",
    "enroll_authority_changed",
    "enroll_unreachable",
    "enroll_stalled",
    "enroll_exchange_failed",
];

struct Capture {
    state: Arc<Mutex<CaptureState>>,
    started: Instant,
}

struct CaptureState {
    file: File,
    status: PathBuf,
    failed: bool,
    finished: bool,
    events: u64,
}

static ACTIVE: OnceLock<Arc<Mutex<CaptureState>>> = OnceLock::new();

impl Capture {
    fn new(file: File, path: &Path) -> Self {
        Self {
            state: Arc::new(Mutex::new(CaptureState {
                file,
                status: path.with_extension("status.json"),
                failed: false,
                finished: false,
                events: 0,
            })),
            started: Instant::now(),
        }
    }
}

fn private_file(path: &Path) -> Result<File, String> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    options
        .open(path)
        .map_err(|_| "Could not create private connection evidence.".to_owned())
}

pub(super) fn start(path: &Path) -> Result<(), String> {
    let capture = Capture::new(private_file(path)?, path);
    let state = capture.state.clone();
    tracing::subscriber::set_global_default(capture)
        .map_err(|_| "A tracing subscriber is already installed.".to_owned())?;
    ACTIVE
        .set(state)
        .map_err(|_| "Connection capture already started.".to_owned())
}

/// The fixture's final background callback seals the writer before termination.
/// Missing or failed completion evidence can never certify a valid JSONL prefix.
pub(super) fn finish() -> Result<(), String> {
    finish_capture(ACTIVE.get().ok_or("Connection capture was not started.")?)
}

fn finish_capture(shared: &Mutex<CaptureState>) -> Result<(), String> {
    let mut state = shared.lock().unwrap_or_else(PoisonError::into_inner);
    if state.finished {
        return Err("Connection capture already finished.".to_owned());
    }
    state.finished = true;
    let pending = state.status.with_extension("pending");
    let mut receipt = private_file(&pending)?;
    writeln!(
        receipt,
        "{}",
        json!({
            "complete": true, "failed": state.failed, "events": state.events,
        })
    )
    .map_err(|_| "Could not write connection capture completion.".to_owned())?;
    drop(receipt);
    // Publish complete bytes atomically, without replacing an existing receipt.
    // A failed/partial pending file remains available for investigation.
    std::fs::hard_link(&pending, &state.status)
        .map_err(|_| "Could not publish connection capture completion.".to_owned())?;
    let _ = std::fs::remove_file(pending);
    if state.failed {
        return Err("Connection capture lost events.".to_owned());
    }
    Ok(())
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
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        if state.finished {
            return;
        }
        fields.0.insert(
            "process_elapsed_ms".to_owned(),
            json!(u64::try_from(self.started.elapsed().as_millis()).unwrap_or(u64::MAX)),
        );
        // One complete JSON line per event, no buffered tail to lose when the
        // driver expires or the qualification process is terminated.
        let mut line = Value::Object(fields.0).to_string();
        line.push('\n');
        if state.file.write_all(line.as_bytes()).is_err() {
            state.failed = true;
        } else {
            state.events = state.events.saturating_add(1);
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
mod enrollment_tests;

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
        let capture = Capture::new(file, &path);
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
        finish().unwrap();
        assert!(
            finish().is_err(),
            "completion must never overwrite evidence"
        );
        tracing::debug!(target: TARGET, connection = 3_u64, attempt = 1_u64,
            elapsed_ms = 1_u64, phase = "attempt_started");
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
            let directory = std::path::PathBuf::from(directory);
            std::fs::copy(&path, directory.join("subscriber.jsonl")).unwrap();
            std::fs::copy(
                path.with_extension("status.json"),
                directory.join("subscriber.status.json"),
            )
            .unwrap();
        }
        std::fs::remove_file(path.with_extension("status.json")).unwrap();
        std::fs::remove_file(path).unwrap();
        std::fs::remove_file(other).unwrap();
    }

    const READY: &[&str] = &[
        "attempt_started",
        "dial_started",
        "dial_connected",
        "hello_sent",
        "hello_received",
        "hello_accepted",
        "topology_received",
        "topology_applied",
    ];

    fn emit(connection: u64, phases: &[&'static str]) {
        for &phase in phases {
            tracing::debug!(target: TARGET, connection, attempt = 1_u64, elapsed_ms = 1_u64, phase);
        }
    }

    fn lost_write(name: &str, after_ready: bool) {
        let directory = std::env::temp_dir().join(format!("{name}-{}", std::process::id()));
        std::fs::create_dir(&directory).unwrap();
        let path = directory.join("capture.jsonl");
        let capture = Capture::new(private_file(&path).unwrap(), &path);
        let state = capture.state.clone();
        tracing::subscriber::with_default(capture, || {
            if after_ready {
                emit(1, READY);
            } else {
                emit(1, &READY[..6]);
            }
            // A real whole-write EBADF on an owned read-only file, not a
            // synthetic missing phase or a partial-tail-only error.
            state.lock().unwrap().file = File::open(&path).unwrap();
            emit(
                1,
                &[if after_ready {
                    "session_ended"
                } else {
                    "initial_deadline"
                }],
            );
            if !after_ready {
                state.lock().unwrap().file = OpenOptions::new().append(true).open(&path).unwrap();
                emit(2, READY);
            }
        });
        assert!(
            finish_capture(&state).is_err(),
            "sticky failure cannot recover"
        );
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(!text.contains(if after_ready {
            "session_ended"
        } else {
            "initial_deadline"
        }));
        let status_path = path.with_extension("status.json");
        let status: Value = serde_json::from_slice(&std::fs::read(&status_path).unwrap()).unwrap();
        assert_eq!(status["complete"], true);
        assert_eq!(status["failed"], true);
        assert_eq!(status["events"], text.lines().count());
        if let Some(destination) = std::env::var_os("RUNE_TRACE_TEST_EVIDENCE") {
            let destination = PathBuf::from(destination);
            std::fs::copy(&path, destination.join(format!("{name}.jsonl"))).unwrap();
            std::fs::copy(
                &status_path,
                destination.join(format!("{name}.status.json")),
            )
            .unwrap();
        }
        std::fs::remove_file(path).unwrap();
        std::fs::remove_file(status_path).unwrap();
        std::fs::remove_dir(directory).unwrap();
    }

    #[test]
    fn whole_deadline_write_failure_stays_observable_after_later_recovery() {
        lost_write("lost-deadline", false);
    }

    #[test]
    fn whole_tail_write_failure_is_observable_without_a_later_row() {
        lost_write("lost-tail", true);
    }

    #[test]
    fn completion_refuses_an_existing_receipt_without_overwriting_it() {
        let path = std::env::temp_dir().join(format!("rune-seal-{}.jsonl", std::process::id()));
        let capture = Capture::new(private_file(&path).unwrap(), &path);
        let status = path.with_extension("status.json");
        std::fs::write(&status, b"existing-receipt-sentinel").unwrap();
        assert!(finish_capture(&capture.state).is_err());
        assert_eq!(
            std::fs::read(&status).unwrap(),
            b"existing-receipt-sentinel"
        );
        assert!(capture.state.lock().unwrap().finished);
        std::fs::remove_file(path.with_extension("status.pending")).unwrap();
        std::fs::remove_file(status).unwrap();
        std::fs::remove_file(path).unwrap();
    }
}
