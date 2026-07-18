//! In-memory tracing capture + auto-dump-on-panic (item 4 of the e2e
//! flywheel).
//!
//! A failing repro is only useful if you can see what the server was doing
//! when it broke. This module installs a process-local `tracing`
//! subscriber whose `fmt` layer writes into a shared in-memory buffer, and
//! returns a [`CaptureGuard`]. If the guard is dropped during a panic
//! (the normal way a `#[test]` fails an assertion), it dumps the captured
//! log — plus an optional last-screen snapshot — to stderr AND to a
//! `/tmp/phux-repro-*.log` file, so the failing run is immediately
//! inspectable without re-running under `RUST_LOG`.
//!
//! It is deliberately self-contained: it does NOT touch the server's
//! production `telemetry::init` (which the sibling agent owns). It uses a
//! local `fmt` layer + a `Mutex<Vec<u8>>` `MakeWriter`, set as the default
//! subscriber for the duration of the guard via
//! `tracing::subscriber::set_default` (scoped, not global `init`, so
//! parallel test binaries don't fight over the global default).
//!
//! Usage:
//! ```ignore
//! let cap = TracingCapture::install("resize_storm");
//! // ... run the scenario; on panic the guard dumps automatically ...
//! cap.attach_screen(client.screenshot().await.snapshot_text());
//! // success path: drop quietly (no dump unless you call `dump()`).
//! ```

#![allow(
    clippy::print_stderr,
    reason = "the dump deliberately surfaces the captured log on stderr"
)]
#![allow(
    clippy::format_push_string,
    reason = "dump assembly: clarity over the marginal write! alloc saving"
)]
#![allow(
    clippy::significant_drop_tightening,
    reason = "MakeWriter holds the lock for the whole write; that is the contract"
)]

use std::io::Write as _;
use std::sync::{Arc, Mutex};

use tracing::subscriber::DefaultGuard;
use tracing_subscriber::fmt::MakeWriter;

/// Shared in-memory log sink. `Clone` is a cheap `Arc` bump so the
/// `MakeWriter` and the guard can both hold it.
#[derive(Clone, Default)]
struct SharedBuf(Arc<Mutex<Vec<u8>>>);

impl SharedBuf {
    fn contents(&self) -> String {
        let guard = self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        String::from_utf8_lossy(&guard).into_owned()
    }
}

/// A `std::io::Write` handle onto the shared buffer. One is produced per
/// log line by the `fmt` layer via [`MakeWriter`].
struct BufWriter(Arc<Mutex<Vec<u8>>>);

impl std::io::Write for BufWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let mut guard = self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        guard.extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> MakeWriter<'a> for SharedBuf {
    type Writer = BufWriter;
    fn make_writer(&'a self) -> Self::Writer {
        BufWriter(Arc::clone(&self.0))
    }
}

/// Handle to an active capture. Hold it for the duration of the scenario.
/// Drop-on-panic dumps; drop-on-success is quiet.
pub struct TracingCapture {
    buf: SharedBuf,
    /// Scoped subscriber guard — restores the prior default on drop.
    _sub_guard: DefaultGuard,
    label: String,
    last_screen: Mutex<Option<String>>,
}

impl TracingCapture {
    /// Install a scoped capturing subscriber for the current thread and
    /// return the guard. `label` is woven into the dump filename so a
    /// multi-test run produces distinguishable artifacts.
    ///
    /// The filter defaults to `RUST_LOG` if set, else `debug` for the
    /// `phux` crates (so a repro captures the server's own spans without
    /// the caller exporting an env var).
    #[must_use]
    pub fn install(label: &str) -> Self {
        use tracing_subscriber::layer::SubscriberExt as _;
        use tracing_subscriber::{EnvFilter, Registry, fmt};

        let buf = SharedBuf::default();
        let make_subscriber = || {
            let filter = EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| EnvFilter::new("phux_server=debug,phux_core=debug,phux=debug"));
            let fmt_layer = fmt::layer()
                .with_ansi(false)
                .with_writer(buf.clone())
                .with_target(true);
            Registry::default().with(filter).with(fmt_layer)
        };
        let guard = tracing::subscriber::set_default(make_subscriber());
        // ALSO claim the process-global default (best-effort; first caller
        // wins, later calls are a no-op `Err`). `set_default` above is
        // thread-local, which misses the PTY reader/writer bridge threads
        // (`phux-pty-reader` / `phux-pty-writer`) — exactly the threads
        // whose death modes the route_input forensics need to see. Safe
        // here because nextest runs one test per process; under plain
        // `cargo test` a parallel sibling's threads could interleave into
        // this buffer, which is acceptable noise for a debug artifact.
        let _ = tracing::subscriber::set_global_default(make_subscriber());

        Self {
            buf,
            _sub_guard: guard,
            label: label.to_owned(),
            last_screen: Mutex::new(None),
        }
    }

    /// Record the latest screen snapshot text so the panic dump includes
    /// what the client saw. Call after each `converge`/`screenshot` so the
    /// dump reflects the freshest grid.
    pub fn attach_screen(&self, screen_text: String) {
        if let Ok(mut slot) = self.last_screen.lock() {
            *slot = Some(screen_text);
        }
    }

    /// Force a dump now (independent of panic). Returns the path written.
    /// Used by the repro example to always leave an artifact, and by tests
    /// that want the log on a soft failure.
    pub fn dump(&self) -> std::path::PathBuf {
        self.dump_inner(false)
    }

    fn dump_inner(&self, panicking: bool) -> std::path::PathBuf {
        let log = self.buf.contents();
        let screen = self
            .last_screen
            .lock()
            .ok()
            .and_then(|s| s.clone())
            .unwrap_or_default();

        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or_default();
        let path = std::env::temp_dir().join(format!("phux-repro-{}-{ts}.log", self.label));

        let mut body = String::new();
        body.push_str(&format!("=== phux e2e repro dump: {} ===\n", self.label));
        if panicking {
            body.push_str("(captured on test panic)\n");
        }
        body.push_str("\n--- last screen ---\n");
        body.push_str(&screen);
        body.push_str("\n\n--- tracing log ---\n");
        body.push_str(&log);
        body.push('\n');

        if let Ok(mut f) = std::fs::File::create(&path) {
            let _ = f.write_all(body.as_bytes());
        }
        // Also surface on stderr so a CI run shows it inline.
        eprintln!("{body}");
        eprintln!("[phux e2e] dump written to {}", path.display());
        path
    }
}

impl Drop for TracingCapture {
    fn drop(&mut self) {
        // The canonical "test failed" signal: a panic is unwinding through
        // this guard's scope. Dump then. On the success path stay quiet.
        if std::thread::panicking() {
            self.dump_inner(true);
        }
    }
}
