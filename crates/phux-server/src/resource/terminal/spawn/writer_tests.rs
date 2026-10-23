use super::*;
use std::io::ErrorKind;

/// A `Write` whose every call is scripted.
struct ScriptedWriter {
    /// Popped front-to-back, one per `write` call.
    script: Vec<Result<usize, std::io::Error>>,
    /// Bytes the "child" actually received.
    received: Vec<u8>,
    flush_script: Vec<Result<(), std::io::Error>>,
}

impl ScriptedWriter {
    fn new(script: Vec<Result<usize, std::io::Error>>) -> Self {
        Self {
            script,
            received: Vec::new(),
            flush_script: Vec::new(),
        }
    }
}

impl Write for ScriptedWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        match self.script.remove(0) {
            Ok(n) => {
                let n = n.min(buf.len());
                self.received.extend_from_slice(&buf[..n]);
                Ok(n)
            }
            Err(e) => Err(e),
        }
    }

    fn flush(&mut self) -> std::io::Result<()> {
        if self.flush_script.is_empty() {
            return Ok(());
        }
        self.flush_script.remove(0)
    }
}

/// `EIO`/`EPIPE` are routine child exit, not a fatal input fault.
#[test]
fn child_exit_errnos_classify_as_pane_gone() {
    assert_eq!(
        classify_write_error(&std::io::Error::from_raw_os_error(EIO)),
        WriteFailure::PaneGone,
    );
    assert_eq!(
        classify_write_error(&std::io::Error::from(ErrorKind::BrokenPipe)),
        WriteFailure::PaneGone,
    );
    assert_eq!(
        classify_write_error(&std::io::Error::from(ErrorKind::PermissionDenied)),
        WriteFailure::Fatal,
    );
}

/// Short writes are resumed.
#[test]
fn partial_writes_are_resumed_until_the_payload_lands() {
    let mut w = ScriptedWriter::new(vec![Ok(3), Ok(3), Ok(3)]);
    write_all_resilient(&mut w, b"abcdefghi").expect("should complete");
    assert_eq!(w.received, b"abcdefghi");
}

/// `EINTR` is a signal artifact, never a delivery failure.
#[test]
fn interrupted_is_retried() {
    let mut w = ScriptedWriter::new(vec![
        Err(std::io::Error::from(ErrorKind::Interrupted)),
        Ok(5),
    ]);
    write_all_resilient(&mut w, b"hello").expect("should complete");
    assert_eq!(w.received, b"hello");
}

/// A failure reports how much the child already ingested.
#[test]
fn failure_reports_the_partial_write_count() {
    let mut w = ScriptedWriter::new(vec![Ok(4), Err(std::io::Error::from_raw_os_error(EIO))]);
    let err = write_all_resilient(&mut w, b"abcdefgh").expect_err("should fail");
    assert_eq!(err.failure, WriteFailure::PaneGone);
    assert_eq!(err.written, 4, "must report the truncated prefix length");
}

/// A zero-length write ends the loop instead of spinning.
#[test]
fn zero_length_write_terminates_instead_of_spinning() {
    let mut w = ScriptedWriter::new(vec![Ok(0)]);
    let err = write_all_resilient(&mut w, b"abc").expect_err("should fail");
    assert_eq!(err.failure, WriteFailure::PaneGone);
    assert_eq!(err.written, 0);
}

/// A child exiting between write and flush is teardown, not a fault.
#[test]
fn flush_classifies_child_exit_as_pane_gone() {
    let mut w = ScriptedWriter::new(vec![]);
    w.flush_script = vec![Err(std::io::Error::from_raw_os_error(EIO))];
    let err = flush_resilient(&mut w).expect_err("should fail");
    assert_eq!(err.failure, WriteFailure::PaneGone);
}
