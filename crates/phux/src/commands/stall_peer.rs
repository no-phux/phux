//! Stalled peers for the `run` / `wait` deadline tests (phux-69pq.10).
//!
//! Both verbs own a `--timeout`, and before phux-69pq.10 they only checked
//! it between screen reads, so a server that accepted the connection and
//! then never answered held the CLI forever. These helpers stand up exactly
//! that server on a private socket and assert the verb gives up with its
//! documented timeout code, on time.
//!
//! The CLI entry points build their own runtime and block on it, so the peer
//! runs on its own thread with its own runtime rather than as a task.

#![allow(clippy::expect_used, reason = "test harness")]

use std::path::Path;
use std::process::ExitCode;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use phux_client::testkit::{self, ScriptSpec};
use phux_core::screen::{SCHEMA_VERSION, ScreenState};
use phux_protocol::wire::info::{ResourceInfo, SessionInfo, SessionSnapshot, WindowInfo};
use phux_protocol::{ResourceId, SessionId, WindowId};

/// The one pane every scripted peer's snapshot carries.
pub(crate) const PANE_SELECTOR: &str = "@100";

/// The `--timeout` every stall test passes, in the CLI's own unit.
pub(crate) const BUDGET_SECS: u64 = 1;

/// [`BUDGET_SECS`] as a duration.
pub(crate) const BUDGET: Duration = Duration::from_secs(BUDGET_SECS);

/// Slack over the expected give-up time for a loaded machine; a regression
/// overruns by far more (before the fix it never returned at all).
const TOLERANCE: Duration = Duration::from_secs(4);

/// How long to wait for the verb before declaring it wedged, so a regression
/// fails the test instead of hanging the run.
const WEDGE: Duration = Duration::from_secs(30);

/// How the peer behaves.
pub(crate) enum Peer {
    /// Accepts every connection and never answers `HELLO`.
    Silent,
    /// Answers `HELLO`, `GET_STATE` (one pane, [`PANE_SELECTOR`]) and input,
    /// but never `GET_SCREEN`.
    WedgedScreen,
    /// Acknowledges the pasted command line but never the Enter after it.
    WedgedEnter,
    /// Answers everything; every `GET_SCREEN` shows one line of this text.
    Showing(&'static str),
}

/// Bind `socket` now and serve `peer` on it from a background thread.
///
/// The bind happens before this returns, so the verb under test can never
/// race the listener into existence.
pub(crate) fn serve(peer: Peer, socket: &Path) {
    let listener = std::os::unix::net::UnixListener::bind(socket).expect("bind stalled peer");
    listener
        .set_nonblocking(true)
        .expect("stalled peer listener must be non-blocking for tokio");
    std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("stalled peer runtime");
        rt.block_on(async move {
            let listener =
                tokio::net::UnixListener::from_std(listener).expect("stalled peer listener");
            if matches!(peer, Peer::Silent) {
                testkit::hold_silent(listener).await;
            } else {
                testkit::serve_every(listener, move || spec_for(&peer)).await;
            }
        });
    });
}

/// The scripted answers for every peer that does answer `HELLO`.
fn spec_for(peer: &Peer) -> ScriptSpec {
    let spec = ScriptSpec::new().state(one_pane());
    match peer {
        Peer::Silent => spec,
        Peer::WedgedScreen => spec.wedge_screen_reads(),
        Peer::WedgedEnter => spec.wedge_input_after(1),
        Peer::Showing(text) => spec.screen(&showing(text)),
    }
}

/// Run `verb` on its own thread; its exit code and how long it took.
///
/// # Panics
///
/// If the verb has not returned within [`WEDGE`].
pub(crate) fn run_verb(verb: impl FnOnce() -> ExitCode + Send + 'static) -> (ExitCode, Duration) {
    let start = Instant::now();
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(verb());
    });
    let code = rx
        .recv_timeout(WEDGE)
        .expect("the verb must return; blocking here is the wedge itself");
    (code, start.elapsed())
}

/// Run `verb` and assert it exits `expected` no sooner than `min` and within
/// tolerance of it.
pub(crate) fn assert_times_out(
    verb: impl FnOnce() -> ExitCode + Send + 'static,
    expected: u8,
    min: Duration,
) {
    let (code, elapsed) = run_verb(verb);
    assert_eq!(
        code,
        ExitCode::from(expected),
        "the documented timeout code"
    );
    assert!(elapsed >= min, "gave up early after {elapsed:?}");
    assert!(elapsed < min + TOLERANCE, "overran: {elapsed:?}");
}

/// A server with one session, one window, and the pane [`PANE_SELECTOR`]
/// names.
fn one_pane() -> SessionSnapshot {
    let session = SessionId::new(1);
    let window = WindowId::new(10);
    let pane = ResourceId::local(100);
    SessionSnapshot::new(session, window, pane.clone())
        .with_sessions(vec![SessionInfo::new(session, "work")])
        .with_windows(vec![WindowInfo::new(window, session, "shell")])
        .with_resources(vec![ResourceInfo::new(pane, window, 80, 24)])
}

/// A one-line screen reading `text`.
fn showing(text: &str) -> ScreenState {
    ScreenState {
        schema_version: SCHEMA_VERSION,
        pane: 100,
        cols: 80,
        rows: 1,
        lines: vec![text.to_owned()],
        ..ScreenState::default()
    }
}
