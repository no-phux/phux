//! In-memory tracing capture that dumps the log (plus the last screen) to
//! stderr and a temp file when the test panics; quiet on success.

#![allow(
    clippy::print_stderr,
    reason = "the dump deliberately surfaces the captured log on stderr"
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
        // Also claim the global default (first caller wins): the thread-local
        // default misses the PTY bridge threads. nextest runs one test per process.
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

    fn dump_on_panic(&self) {
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

        let body = format!(
            "=== phux e2e repro dump: {} (test panicked) ===\n\n--- last screen ---\n{screen}\n\n--- tracing log ---\n{log}\n",
            self.label
        );

        if let Ok(mut f) = std::fs::File::create(&path) {
            let _ = f.write_all(body.as_bytes());
        }
        // Also surface on stderr so a CI run shows it inline.
        eprintln!("{body}");
        eprintln!("[phux e2e] dump written to {}", path.display());
    }
}

impl Drop for TracingCapture {
    fn drop(&mut self) {
        if std::thread::panicking() {
            self.dump_on_panic();
        }
    }
}
