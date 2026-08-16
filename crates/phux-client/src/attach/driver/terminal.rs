//! Outer-terminal state ownership: raw mode + alt screen (`RawModeGuard`),
//! mouse/hover DECSET reconciliation, termios snapshots, and the
//! signal/panic/detach teardown paths.

use std::cell::RefCell;
use std::io::{self, IsTerminal, Write};
use std::os::fd::AsFd;
use std::rc::Rc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

#[cfg(not(all(feature = "native-engine", not(target_arch = "wasm32"))))]
use phux_protocol::caps::BootstrapCapabilities;
use phux_protocol::ids::TerminalId;
use rustix::termios::{LocalModes, OptionalActions, Termios};

use crate::attach::outcome::{AttachEnd, AttachError};
use crate::attach::record::{SessionRecorder, TeeSink};
use crate::attach::render::write_reset;

/// RAII handle that flips stdin into raw mode and stdout into the alt
/// screen on construction, and restores both on drop.
///
/// Restoration runs in `Drop`, so a panic anywhere in the attach loop —
/// including the renderer or the connection — leaves the user's outer
/// terminal in a usable state.
pub struct RawModeGuard {
    original_termios: Termios,
}

impl std::fmt::Debug for RawModeGuard {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RawModeGuard").finish_non_exhaustive()
    }
}

impl RawModeGuard {
    /// Install the guard, writing the alt-screen-enter + cursor-hide
    /// sequence to real stdout. Convenience wrapper around
    /// [`Self::install_with_stdout`] for the common path; tests use
    /// the writer-injecting variant. Enables mouse capture by default
    /// (ADR-0048).
    pub fn install() -> Result<Self, AttachError> {
        Self::install_with_stdout(&mut io::stdout(), true)
    }

    /// Install the guard. Errors if stdin is not a TTY or the termios
    /// dance fails. The alt-screen + cursor-hide bytes are written to
    /// `out` so tests can capture them and assert on the regression
    /// guard for `phux-roz`.
    ///
    /// `mouse` gates the client's own outer-terminal mouse tracking
    /// (ADR-0048): when `true` the entry sequence also emits DECSET
    /// `?1002h?1006h` so divider drags work without an inner program
    /// turning mouse mode on; when `false` the client emits no mouse DECSET
    /// and only sees mouse when an inner program enables tracking (the host's
    /// native selection is untouched).
    pub fn install_with_stdout<W: Write>(out: &mut W, mouse: bool) -> Result<Self, AttachError> {
        let stdin = io::stdin();
        if !stdin.is_terminal() {
            return Err(AttachError::NotATty);
        }
        let fd = stdin.as_fd();
        let original = rustix::termios::tcgetattr(fd)
            .map_err(|err| AttachError::Terminal(format!("tcgetattr: {err}")))?;
        let mut raw = original.clone();
        raw.input_modes.remove(
            rustix::termios::InputModes::IGNBRK
                | rustix::termios::InputModes::BRKINT
                | rustix::termios::InputModes::PARMRK
                | rustix::termios::InputModes::ISTRIP
                | rustix::termios::InputModes::INLCR
                | rustix::termios::InputModes::IGNCR
                | rustix::termios::InputModes::ICRNL
                | rustix::termios::InputModes::IXON,
        );
        raw.output_modes.remove(rustix::termios::OutputModes::OPOST);
        raw.local_modes.remove(
            LocalModes::ECHO
                | LocalModes::ECHONL
                | LocalModes::ICANON
                | LocalModes::ISIG
                | LocalModes::IEXTEN,
        );
        raw.control_modes
            .remove(rustix::termios::ControlModes::CSIZE | rustix::termios::ControlModes::PARENB);
        raw.control_modes.insert(rustix::termios::ControlModes::CS8);

        // Make `read` block until at least one byte is available, with
        // no timeout. Tokio's stdin uses a blocking helper thread, so
        // this matches its expectations.
        raw.special_codes[rustix::termios::SpecialCodeIndex::VMIN] = 1;
        raw.special_codes[rustix::termios::SpecialCodeIndex::VTIME] = 0;

        rustix::termios::tcsetattr(fd, OptionalActions::Now, &raw)
            .map_err(|err| AttachError::Terminal(format!("tcsetattr: {err}")))?;

        // Enter the alt screen + hide the cursor up front so the first
        // frame paint doesn't briefly show on the normal screen. With
        // `mouse` on, also enable our own outer-terminal mouse tracking so
        // divider drags work by default (ADR-0048).
        write_enter_alt_screen(out, mouse).map_err(AttachError::Io)?;

        // Remember that we entered the alt screen so signal handlers
        // know to emit the leave sequence. We deliberately set this
        // AFTER the writes succeed so a half-completed entry doesn't
        // confuse cleanup.
        ALT_SCREEN_ACTIVE.store(true, Ordering::SeqCst);

        // Upgrade the fatal-signal handler to also emit the DECSET resets.
        // Paired with the matching downgrade in `Drop`, so the escape codes
        // are only ever written while there is genuinely an alt screen and
        // mouse tracking to undo. No-op if the handler was never installed
        // (the writer-injecting test path never reaches here — it returns
        // `NotATty` above — but a future caller might).
        phux_crash::enable_terminal_escape_restore();

        // Park a clone of the original Termios in process-global storage
        // so the signal-handler arms and the panic hook (which can't
        // reach the instance field) can perform a true restore rather
        // than a best-effort re-cook. The instance field remains the
        // Drop-path source of truth; the global is a snapshot.
        save_termios_snapshot(original.clone());

        Ok(Self {
            original_termios: original,
        })
    }
}

impl Drop for RawModeGuard {
    fn drop(&mut self) {
        // Best-effort restore. We deliberately swallow errors — the
        // process is on its way out and a panic in Drop is worse than
        // a slightly-wedged terminal.
        //
        // Clear the global snapshot before restoring from the instance
        // field. Either source restores the same Termios (the global is
        // a clone of `original_termios`); the clear prevents a later
        // install from inheriting a stale snapshot if the next
        // `install_with_stdout` errors out before reaching the save.
        let _ = take_termios_snapshot();
        let stdin = io::stdin();
        let _ =
            rustix::termios::tcsetattr(stdin.as_fd(), OptionalActions::Now, &self.original_termios);
        let mut out = io::stdout().lock();
        let _ = write_terminal_reset(&mut out);
        ALT_SCREEN_ACTIVE.store(false, Ordering::SeqCst);

        // Downgrade the fatal-signal handler back to termios-only. The alt
        // screen is gone; a crash from here on must not spray DECSET resets
        // across the user's restored normal screen. Deliberately last, so a
        // fault *during* the reset above is still covered by the full
        // sequence.
        phux_crash::disable_terminal_escape_restore();
    }
}

/// Whether the alt-screen / cursor-hide sequence is currently active.
///
/// Set inside [`RawModeGuard::install_with_stdout`] after the entry
/// sequence has been emitted, cleared by [`RawModeGuard::drop`] and the
/// signal-handler cleanup. The signal path consults this so SIGINT
/// during the pre-handshake stage (no alt-screen, no raw mode) does NOT
/// emit a spurious leave sequence that the cooked terminal would print
/// as garbage.
///
/// Kept deliberately separate from [`SAVED_TERMIOS`]: alt-screen ENTER
/// and the termios flip happen at different points in
/// [`RawModeGuard::install_with_stdout`] (termios first, then alt
/// screen). Tying the two together via a single state variable would
/// couple two independent concerns and risks leaving the alt screen
/// when we should restore termios (or vice versa) on a half-failed
/// install. Two cheap flags is the right factoring.
static ALT_SCREEN_ACTIVE: AtomicBool = AtomicBool::new(false);

/// Whether the client enabled its OWN outer-terminal mouse tracking
/// (DECSET `?1002h` button-motion + `?1006h` SGR) on attach (ADR-0048).
///
/// Set by [`write_enter_alt_screen`] when the `mouse` config is on, so the
/// client receives pointer reports over a divider even when the inner
/// program has no mouse mode (the common shell case) — that is what makes
/// drag-to-resize work by default. Cleared by [`write_terminal_reset`],
/// which emits the matching `?1006l?1002l` BEFORE the `?1049l` alt-screen
/// leave so the host terminal's native click-drag selection comes back on
/// detach. Kept separate from [`ALT_SCREEN_ACTIVE`] for the same reason
/// that flag is separate from the termios snapshot: independent concerns,
/// each restored exactly once.
static MOUSE_CAPTURE_ACTIVE: AtomicBool = AtomicBool::new(false);

/// Snapshot of the outer terminal's pre-raw Termios, parked here so
/// the signal-handler arms in `main_loop` and the panic hook installed
/// by [`install_panic_hook_once`] can perform a true `tcsetattr`
/// restore — rather than a best-effort "force ICANON|ECHO|ISIG re-cook"
/// — when [`RawModeGuard::drop`] is unreachable (process exits via
/// `std::process::exit`, which skips Drop).
///
/// Signal-safety: the signal arms in `main_loop` are tokio
/// `signal::unix::Signal::recv()` futures, which deliver on the tokio
/// runtime thread — NOT inside a POSIX async-signal-handler context.
/// The panic hook runs on the panicking thread after unwind has begun,
/// also normal Rust context. So acquiring this `Mutex` is safe in both
/// callers; we are explicitly NOT in a context that would deadlock on
/// re-entrant lock acquisition.
///
/// Written by [`RawModeGuard::install_with_stdout`] (clone of the
/// instance's `original_termios`) and cleared by [`RawModeGuard::drop`]
/// and the signal-restore path. The instance field on `RawModeGuard`
/// remains the Drop-path source of truth; this global is a snapshot
/// for the paths that can't reach the instance.
static SAVED_TERMIOS: Mutex<Option<Termios>> = Mutex::new(None);

/// Park a Termios snapshot in [`SAVED_TERMIOS`]. Errors on lock
/// poisoning are swallowed: a poisoned lock means another thread
/// panicked while holding it, in which case we still want subsequent
/// installs to succeed and the most we lose is the signal-arm's true
/// restore (fall-back path covers it).
fn save_termios_snapshot(t: Termios) {
    if let Ok(mut slot) = SAVED_TERMIOS.lock() {
        *slot = Some(t);
    }
}

/// Take the Termios snapshot out of [`SAVED_TERMIOS`], leaving `None`.
/// Returns `None` if the lock is poisoned (signal-arm falls back to
/// the re-cook path; Drop falls back to the instance field).
fn take_termios_snapshot() -> Option<Termios> {
    SAVED_TERMIOS.lock().ok().and_then(|mut slot| slot.take())
}

/// Whether [`install_panic_hook_once`] has already run. The panic hook
/// is global to the process; we don't want a re-entrant install to
/// chain hooks indefinitely.
static PANIC_HOOK_INSTALLED: AtomicBool = AtomicBool::new(false);

/// Write the alt-screen-enter + cursor-hide sequence, plus — when
/// `mouse` is on — the client's own mouse-tracking DECSET (ADR-0048).
/// Factored out so the install path and any future re-entry path share
/// one byte definition.
///
/// `?1002h` is button-event tracking (motion only while a button is held,
/// not `?1003h` any-motion which would flood the wire with hover traffic
/// we discard); `?1006h` is SGR extended coordinates, mandatory to address
/// columns past 223. Records [`MOUSE_CAPTURE_ACTIVE`] so the matching
/// reset emits the leave sequence.
fn write_enter_alt_screen<W: Write>(out: &mut W, mouse: bool) -> io::Result<()> {
    out.write_all(b"\x1b[?1049h")?;
    out.write_all(b"\x1b[?25l")?;
    if mouse {
        out.write_all(b"\x1b[?1002h\x1b[?1006h")?;
        MOUSE_CAPTURE_ACTIVE.store(true, Ordering::SeqCst);
    }
    out.flush()
}

/// Reconcile the client's outer-terminal mouse-tracking DECSET with
/// `want` (phux-npb3: capture follows focus).
///
/// The current state lives in [`MOUSE_CAPTURE_ACTIVE`] — the same flag
/// [`write_enter_alt_screen`] sets and [`write_terminal_reset`] consumes —
/// so a detach or signal reset while an opted-out pane holds focus never
/// emits a redundant leave sequence. No-op when the state already
/// matches; otherwise emits the ADR-0048 enter pair (`?1002h?1006h`) or
/// its reverse-order leave (`?1006l?1002l`).
/// Whether the client's outer-terminal mouse capture should currently be
/// on (phux-npb3): the global `mouse` config gate must be on AND the
/// focused pane must not have opted out via `set-pane mouse off`. With no
/// focused pane yet (pre-ATTACHED) the global gate alone decides.
pub(super) fn desired_mouse_capture(
    cfg_on: bool,
    focused: Option<&TerminalId>,
    optout: &std::collections::HashSet<TerminalId>,
) -> bool {
    cfg_on && !focused.is_some_and(|id| optout.contains(id))
}

pub(super) fn sync_mouse_capture<W: Write>(out: &mut W, want: bool) -> io::Result<()> {
    if MOUSE_CAPTURE_ACTIVE.swap(want, Ordering::SeqCst) == want {
        return Ok(());
    }
    if want {
        out.write_all(b"\x1b[?1002h\x1b[?1006h")?;
    } else {
        // Any-motion is a strict superset of button-event tracking; drop it
        // first so a capture-off transition while a menu is open cannot
        // leave `?1003h` armed on the host terminal.
        if HOVER_TRACKING_ACTIVE.swap(false, Ordering::SeqCst) {
            out.write_all(b"\x1b[?1003l")?;
        }
        out.write_all(b"\x1b[?1006l\x1b[?1002l")?;
    }
    out.flush()
}

/// Whether the client upgraded the outer terminal to any-motion reporting
/// (`?1003h`) on top of its button-event capture (phux-wrnm).
///
/// ADR-0048 deliberately enables only `?1002h`: hover traffic the client
/// discards is wasted bytes on every pointer move. A context menu is the
/// one thing that *does* consume it — it hover-tracks the row under the
/// pointer with no button held — so the mode is raised while such an
/// overlay is on the stack and dropped the moment it closes. Kept as its
/// own flag (not folded into [`MOUSE_CAPTURE_ACTIVE`]) so the capture
/// reconcile and the reset path each restore exactly what they set.
static HOVER_TRACKING_ACTIVE: AtomicBool = AtomicBool::new(false);

/// Reconcile any-motion reporting with `want` (phux-wrnm).
///
/// A no-op unless the state changed, like [`sync_mouse_capture`]. Never
/// raises `?1003h` while capture is off: with no capture the client has no
/// business reporting motion at all, and the host terminal owns the mouse.
/// Leaving hover mode re-asserts `?1002h` — some terminals treat the two
/// DECSETs as one tracking mode and `?1003l` alone would drop button
/// reporting with it, killing divider drags.
pub(super) fn sync_hover_tracking<W: Write>(out: &mut W, want: bool) -> io::Result<()> {
    let want = want && MOUSE_CAPTURE_ACTIVE.load(Ordering::SeqCst);
    if HOVER_TRACKING_ACTIVE.swap(want, Ordering::SeqCst) == want {
        return Ok(());
    }
    if want {
        out.write_all(b"\x1b[?1003h")?;
    } else {
        out.write_all(b"\x1b[?1003l\x1b[?1002h")?;
    }
    out.flush()
}

/// Restore the outer terminal to a sane post-attach state: drop SGR,
/// show the cursor, and (if we ever entered the alt screen) leave it.
///
/// Used by both [`RawModeGuard::drop`] and the signal-handler arms in
/// the private `main_loop` function. Safe to call multiple times — the
/// second call sees
/// `ALT_SCREEN_ACTIVE == false` and skips the leave sequence.
pub fn write_terminal_reset<W: Write>(out: &mut W) -> io::Result<()> {
    write_reset(out)?;
    // phux-wrnm: a context menu open at detach (or at SIGINT) left the
    // terminal in any-motion mode; drop that before the capture pair so the
    // host is handed back exactly what it had.
    if HOVER_TRACKING_ACTIVE.swap(false, Ordering::SeqCst) {
        out.write_all(b"\x1b[?1003l")?;
        out.flush()?;
    }
    // ADR-0048: drop our own mouse tracking BEFORE leaving the alt screen,
    // so the host terminal's native click-drag selection is restored on
    // detach. `?1006l` then `?1002l` undoes the entry pair in reverse.
    if MOUSE_CAPTURE_ACTIVE.swap(false, Ordering::SeqCst) {
        out.write_all(b"\x1b[?1006l\x1b[?1002l")?;
        out.flush()?;
    }
    if ALT_SCREEN_ACTIVE.swap(false, Ordering::SeqCst) {
        out.write_all(b"\x1b[?1049l")?;
        out.flush()?;
    }
    Ok(())
}

/// Best-effort termios restore shared by signal and clean-detach exits.
/// Termios goes back to the saved state (recovered from [`SAVED_TERMIOS`]
/// when populated; otherwise a re-cook fall-back). Errors are swallowed: the
/// process is on its way out.
///
/// Behaviour change for phux-2r7 (was best-effort re-cook only,
/// committed in 63dc6ff): when [`RawModeGuard`] has parked a snapshot,
/// we now do a true `tcsetattr` restore to the user's pre-attach
/// flags, preserving customisations like IUTF8 / VEOF that the re-cook
/// would clobber. The manual SIGINT-during-attach repro that motivated
/// the original fix still passes; verifying the precise-restore
/// behaviour requires a live PTY and is not unit-testable from here.
fn restore_terminal_termios() {
    let stdin = io::stdin();
    let fd = stdin.as_fd();
    if let Some(saved) = take_termios_snapshot() {
        // True restore: the snapshot is exactly what `tcgetattr`
        // returned before we flipped into raw mode.
        let _ = rustix::termios::tcsetattr(fd, OptionalActions::Now, &saved);
    } else if let Ok(mut termios) = rustix::termios::tcgetattr(fd) {
        // Fall-back re-cook for the (rare) case where the snapshot is
        // missing — e.g. signal fired before `install_with_stdout`
        // reached the save, or the lock was poisoned. We force the
        // canonical-mode flags back on so the cooked shell at least
        // shows what the user types; non-default flags are NOT
        // preserved on this path and the user may want to run `reset`
        // after.
        termios.local_modes.insert(
            LocalModes::ECHO
                | LocalModes::ECHONL
                | LocalModes::ICANON
                | LocalModes::ISIG
                | LocalModes::IEXTEN,
        );
        termios.input_modes.insert(
            rustix::termios::InputModes::BRKINT
                | rustix::termios::InputModes::ICRNL
                | rustix::termios::InputModes::IXON,
        );
        termios
            .output_modes
            .insert(rustix::termios::OutputModes::OPOST);
        let _ = rustix::termios::tcsetattr(fd, OptionalActions::Now, &termios);
    }
}

/// Restore termios and leave the alt screen from a signal handler arm.
pub(super) fn terminal_reset_on_signal() {
    restore_terminal_termios();
    let mut out = io::stdout().lock();
    let _ = write_terminal_reset(&mut out);
}

fn write_terminal_reset_and_finalize<W: Write>(
    out: &mut W,
    recorder: Option<&Rc<RefCell<SessionRecorder>>>,
) {
    if let Some(recorder) = recorder {
        {
            let mut tee = TeeSink {
                inner: out,
                rec: Rc::clone(recorder),
            };
            let _ = write_terminal_reset(&mut tee);
        }
        if let Err(err) = recorder.borrow_mut().finish_in_place() {
            tracing::warn!(error = %err, "closing the session recording failed");
        }
    } else {
        let _ = write_terminal_reset(out);
    }
}

/// Clean client exit after a server-acknowledged DETACH (or a
/// detach-intended disconnect). Restores the terminal and exits the
/// process immediately rather than returning up the stack.
///
/// Why not just `return Ok(())` and let `RawModeGuard::drop` + the
/// runtime teardown clean up? Because `tokio::io::stdin()` parks an
/// **uncancellable** blocking `read()` on a helper thread. The terminal
/// restore (guard Drop) does run, but the subsequent runtime drop then
/// blocks forever waiting for that stuck read to return. The result is
/// a zombie client that never exits, keeps a reader on the shared PTY,
/// and steals the first line the user types next — most painfully their
/// reattach command, which is why reattach "did nothing." Exiting here
/// closes that window: the restore mirrors the signal path, and
/// `process::exit` skips the teardown that would otherwise hang.
///
/// phux-i0e8.2.2: because this never returns, the CLI's own `Ok(end)`
/// handling can't run on this path — so the one-line explanation for a
/// last-pane death (`AttachEnd::explanation`) is printed HERE, after the
/// terminal reset (the screen is cooked again) and before the exit. A
/// plain detach explains nothing. Process exit stays `0` either way:
/// the attach succeeded; the ending just deserves words.
#[allow(
    clippy::exit,
    reason = "detach must exit now; runtime drop hangs on the stdin read thread"
)]
#[allow(
    clippy::print_stderr,
    reason = "phux-i0e8.2.2: the terminal is cooked again and the process exits before the CLI could print; this is the only window for the last-pane explanation"
)]
pub(super) fn exit_after_detach(
    end: AttachEnd,
    locally_requested: bool,
    onboarding_path: &std::path::Path,
    recorder: Option<&Rc<RefCell<SessionRecorder>>>,
) -> ! {
    restore_terminal_termios();
    let mut stdout = io::stdout().lock();
    write_terminal_reset_and_finalize(&mut stdout, recorder);
    drop(stdout);
    if let Some(line) = end.explanation() {
        eprintln!("{line}");
    } else if locally_requested
        && let Some(line) = crate::attach::onboarding::after_detach(onboarding_path)
    {
        eprintln!("{line}");
    }
    std::process::exit(0);
}

/// Install a global panic hook that first records the panic to the
/// `tracing` file sink, then runs [`write_terminal_reset`], then chains
/// the previous (default) hook. Idempotent — repeated calls after the
/// first are no-ops.
///
/// Ordering matters and is deliberate:
///
/// 1. **Log first.** The client's `tracing` subscriber writes to a file
///    (never stderr — the alt screen is up), so we emit the panic message
///    plus a captured [`std::backtrace::Backtrace`] there BEFORE touching
///    the terminal. This is the durable record: even though the next step
///    restores the cooked terminal and the default hook's stderr backtrace
///    lands on a screen the user may not be watching, the crash is fully
///    recoverable from the log file.
/// 2. **Restore the terminal.** Without this, a panic deep inside the
///    renderer or libghostty would unwind through `main_loop` and the
///    default hook would print into the alt screen we're about to leave —
///    so the user would see nothing.
/// 3. **Chain the previous hook** (the default backtrace printer).
pub(super) fn install_panic_hook_once() {
    if PANIC_HOOK_INSTALLED.swap(true, Ordering::SeqCst) {
        return;
    }
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        // (1) Durable capture to the file sink before the terminal is
        // touched. `Backtrace::capture` honors `RUST_BACKTRACE`: it is
        // `Disabled` (rendered as a hint) unless the env var is set, so
        // there's no symbolication cost in the common case while a full
        // trace is available when the operator asks for one.
        let backtrace = std::backtrace::Backtrace::capture();
        let location = info
            .location()
            .map_or_else(|| "<unknown>".to_owned(), ToString::to_string);
        tracing::error!(
            panic.location = %location,
            panic.message = %info,
            panic.backtrace = %backtrace,
            "client panic",
        );
        // (2) Restore the outer terminal so the chained hook's output
        // doesn't vanish into the dead alt screen.
        terminal_reset_on_signal();
        // (3) Default hook: prints the panic + backtrace to stderr.
        previous(info);
    }));
}

#[cfg(test)]
#[allow(clippy::expect_used, reason = "tests")]
mod tests {
    use super::*;

    static TERMINAL_RESET_TEST_LOCK: Mutex<()> = Mutex::new(());

    /// Borrow a real `Termios` from `/dev/tty` so tests that need to
    /// exercise [`save_termios_snapshot`] / [`take_termios_snapshot`]
    /// can run with a plausible value. Returns `None` when the test
    /// process has no controlling TTY (e.g. some CI sandboxes); the
    /// caller skips in that case.
    fn try_borrow_real_termios() -> Option<Termios> {
        let tty = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open("/dev/tty")
            .ok()?;
        rustix::termios::tcgetattr(tty.as_fd()).ok()
    }

    /// The save/take helpers behind [`SAVED_TERMIOS`] round-trip a
    /// snapshot exactly once: after a save, the next take returns
    /// `Some(_)`; subsequent takes return `None`. This is the unit
    /// surface that backs the signal-arm true-restore path
    /// (phux-2r7). The signal arm itself is exercised by a manual
    /// SIGINT during an attach session — see the comment on
    /// [`terminal_reset_on_signal`].
    ///
    /// `SAVED_TERMIOS` is a process-global; we clear at both ends to
    /// be hygienic across the in-test serial execution model.
    #[test]
    fn saved_termios_round_trip() {
        let Some(t) = try_borrow_real_termios() else {
            // No controlling TTY in this test process; nothing to
            // assert. The save/take helpers are still type-checked.
            return;
        };
        // Pre-clean: another test (or a panic) may have left state.
        let _ = take_termios_snapshot();
        assert!(take_termios_snapshot().is_none());

        save_termios_snapshot(t);
        assert!(
            take_termios_snapshot().is_some(),
            "save then take must return the snapshot"
        );
        assert!(
            take_termios_snapshot().is_none(),
            "second take must be empty"
        );
    }

    /// Documents the manual SIGINT repro that backs phux-2r7. The
    /// signal-arm path can't be unit-tested without forking and
    /// driving a real PTY; this `#[ignore]`-stub keeps the procedure
    /// next to the code and surfaces in `cargo test -- --ignored` if
    /// someone wires up an integration harness later.
    /// ADR-0048: with mouse capture on, the alt-screen entry sequence also
    /// enables the client's own outer-terminal mouse tracking
    /// (`?1002h` button-motion + `?1006h` SGR), and the reset undoes it
    /// (`?1006l?1002l`) BEFORE leaving the alt screen so the host's native
    /// selection is restored.
    #[test]
    fn mouse_capture_enable_and_disable_bytes() {
        let _guard = TERMINAL_RESET_TEST_LOCK
            .lock()
            .expect("terminal reset test lock");
        MOUSE_CAPTURE_ACTIVE.store(false, Ordering::SeqCst);
        ALT_SCREEN_ACTIVE.store(false, Ordering::SeqCst);

        let mut entry = Vec::new();
        write_enter_alt_screen(&mut entry, true).unwrap();
        assert!(
            entry.windows(8).any(|w| w == b"\x1b[?1002h"),
            "entry must enable button-motion tracking: {entry:?}"
        );
        assert!(
            entry.windows(8).any(|w| w == b"\x1b[?1006h"),
            "entry must enable SGR coordinates: {entry:?}"
        );
        // `write_enter_alt_screen` records MOUSE_CAPTURE_ACTIVE; the
        // alt-screen flag is set separately by `install_with_stdout` on a
        // real attach. Set it here so reset exercises the full leave path
        // (mouse-disable AND alt-screen-leave) the way a live detach does.
        ALT_SCREEN_ACTIVE.store(true, Ordering::SeqCst);
        // Reset emits the leave pair before the ?1049l alt-screen leave.
        let mut reset = Vec::new();
        write_terminal_reset(&mut reset).unwrap();
        let pos_1006l = reset
            .windows(8)
            .position(|w| w == b"\x1b[?1006l")
            .expect("reset must disable SGR coordinates");
        let pos_1002l = reset
            .windows(8)
            .position(|w| w == b"\x1b[?1002l")
            .expect("reset must disable button-motion");
        let pos_1049l = reset
            .windows(8)
            .position(|w| w == b"\x1b[?1049l")
            .expect("reset must leave the alt screen");
        assert!(
            pos_1006l < pos_1049l && pos_1002l < pos_1049l,
            "mouse-disable must precede the alt-screen leave: {reset:?}"
        );
    }

    #[test]
    fn clean_detach_records_the_complete_reset_before_finalizing() {
        let _guard = TERMINAL_RESET_TEST_LOCK
            .lock()
            .expect("terminal reset test lock");
        HOVER_TRACKING_ACTIVE.store(true, Ordering::SeqCst);
        MOUSE_CAPTURE_ACTIVE.store(true, Ordering::SeqCst);
        ALT_SCREEN_ACTIVE.store(true, Ordering::SeqCst);

        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("detach.cast");
        let recorder = Rc::new(RefCell::new(
            SessionRecorder::create(&path, None, phux_record::cast::CastVersion::V2)
                .expect("recorder"),
        ));
        let mut reset = Vec::new();
        write_terminal_reset_and_finalize(&mut reset, Some(&recorder));

        let cast = std::fs::read(&path).expect("read cast");
        let text = String::from_utf8(cast.clone()).expect("cast utf-8");
        assert!(
            text.lines()
                .next()
                .is_some_and(|line| line.contains("\"duration\":")),
            "clean detach must backfill duration"
        );
        let (_, events) = phux_record::cast::read_cast(cast.as_slice()).expect("parse cast");
        let recorded_reset: String = events
            .iter()
            .filter(|event| event.code == phux_record::cast::EventCode::Output)
            .map(|event| event.data.as_str())
            .collect();
        assert_eq!(
            recorded_reset.as_bytes(),
            reset,
            "the recorder must capture every reset byte before it is finalized"
        );
    }

    /// ADR-0048: `mouse = false` skips the DECSET entirely — the entry
    /// sequence emits no mouse tracking, host native selection untouched.
    #[test]
    fn mouse_capture_disabled_emits_no_decset() {
        let _guard = TERMINAL_RESET_TEST_LOCK
            .lock()
            .expect("terminal reset test lock");
        MOUSE_CAPTURE_ACTIVE.store(false, Ordering::SeqCst);
        ALT_SCREEN_ACTIVE.store(false, Ordering::SeqCst);

        let mut entry = Vec::new();
        write_enter_alt_screen(&mut entry, false).unwrap();
        assert!(
            !entry.windows(8).any(|w| w == b"\x1b[?1002h"),
            "mouse=false must not enable tracking: {entry:?}"
        );
        assert!(
            entry.windows(8).any(|w| w == b"\x1b[?1049h"),
            "alt-screen enter still emitted: {entry:?}"
        );
        // With capture never set, reset emits no mouse-disable bytes.
        let mut reset = Vec::new();
        write_terminal_reset(&mut reset).unwrap();
        assert!(
            !reset.windows(8).any(|w| w == b"\x1b[?1002l"),
            "no capture ⇒ no mouse-disable on reset: {reset:?}"
        );
        assert!(
            !reset.windows(8).any(|w| w == b"\x1b[?1006l"),
            "no capture ⇒ no SGR mouse-disable on reset: {reset:?}"
        );
    }

    /// phux-npb3: `sync_mouse_capture` reconciles the outer DECSET with the
    /// desired state — leave pair when dropping, enter pair when restoring,
    /// and nothing at all when the state already matches.
    #[test]
    fn sync_mouse_capture_emits_transitions_only() {
        let _guard = TERMINAL_RESET_TEST_LOCK
            .lock()
            .expect("terminal reset test lock");
        MOUSE_CAPTURE_ACTIVE.store(true, Ordering::SeqCst);

        // Already on ⇒ no bytes.
        let mut out = Vec::new();
        sync_mouse_capture(&mut out, true).unwrap();
        assert!(out.is_empty(), "no transition ⇒ no bytes: {out:?}");

        // On → off emits the reverse-order leave pair.
        sync_mouse_capture(&mut out, false).unwrap();
        assert_eq!(out, b"\x1b[?1006l\x1b[?1002l");

        // Off is now recorded ⇒ a second off is a no-op.
        out.clear();
        sync_mouse_capture(&mut out, false).unwrap();
        assert!(out.is_empty(), "idempotent off ⇒ no bytes: {out:?}");

        // Off → on emits the entry pair, and the reset path sees capture as
        // active again (the shared MOUSE_CAPTURE_ACTIVE flag).
        sync_mouse_capture(&mut out, true).unwrap();
        assert_eq!(out, b"\x1b[?1002h\x1b[?1006h");
        assert!(MOUSE_CAPTURE_ACTIVE.load(Ordering::SeqCst));

        MOUSE_CAPTURE_ACTIVE.store(false, Ordering::SeqCst);
    }

    /// phux-wrnm: hover reporting is raised only while something consumes
    /// it, only on top of live capture, and always unwound — including when
    /// capture itself drops while a menu is still open.
    #[test]
    fn sync_hover_tracking_rides_on_top_of_capture() {
        let _guard = TERMINAL_RESET_TEST_LOCK
            .lock()
            .expect("terminal reset test lock");
        MOUSE_CAPTURE_ACTIVE.store(false, Ordering::SeqCst);
        HOVER_TRACKING_ACTIVE.store(false, Ordering::SeqCst);

        // No capture ⇒ the client has no business reporting motion.
        let mut out = Vec::new();
        sync_hover_tracking(&mut out, true).unwrap();
        assert!(out.is_empty(), "capture off ⇒ no hover bytes: {out:?}");
        assert!(!HOVER_TRACKING_ACTIVE.load(Ordering::SeqCst));

        // With capture live, opening a menu raises any-motion once.
        MOUSE_CAPTURE_ACTIVE.store(true, Ordering::SeqCst);
        sync_hover_tracking(&mut out, true).unwrap();
        assert_eq!(out, b"\x1b[?1003h");
        out.clear();
        sync_hover_tracking(&mut out, true).unwrap();
        assert!(out.is_empty(), "no transition ⇒ no bytes: {out:?}");

        // Closing it drops any-motion and re-asserts button-event tracking,
        // so divider drags survive the round trip.
        sync_hover_tracking(&mut out, false).unwrap();
        assert_eq!(out, b"\x1b[?1003l\x1b[?1002h");

        // Capture dropping (focus moved to an opted-out pane) while a menu
        // is open must not strand `?1003h` on the host terminal.
        out.clear();
        sync_hover_tracking(&mut out, true).unwrap();
        assert_eq!(out, b"\x1b[?1003h");
        out.clear();
        sync_mouse_capture(&mut out, false).unwrap();
        assert_eq!(out, b"\x1b[?1003l\x1b[?1006l\x1b[?1002l");
        assert!(!HOVER_TRACKING_ACTIVE.load(Ordering::SeqCst));

        MOUSE_CAPTURE_ACTIVE.store(false, Ordering::SeqCst);
    }

    /// phux-npb3: capture follows focus — wanted iff the global gate is on
    /// AND the focused pane has not opted out.
    #[test]
    fn desired_mouse_capture_follows_focused_pane_optout() {
        let t1 = TerminalId::local(1);
        let t2 = TerminalId::local(2);
        let mut optout = std::collections::HashSet::new();
        optout.insert(t2.clone());

        // Global gate off wins unconditionally.
        assert!(!desired_mouse_capture(false, Some(&t1), &optout));
        assert!(!desired_mouse_capture(false, None, &optout));
        // Gate on: an opted-in focused pane (or none yet) keeps capture.
        assert!(desired_mouse_capture(true, Some(&t1), &optout));
        assert!(desired_mouse_capture(true, None, &optout));
        // Gate on but the focused pane opted out ⇒ capture drops.
        assert!(!desired_mouse_capture(true, Some(&t2), &optout));
    }

    #[test]
    #[ignore = "manual: requires a live PTY and a SIGINT during attach"]
    fn signal_arm_true_restore_manual_repro() {
        // 1. `stty -a` in an outer shell; note `iutf8` / VEOF / etc.
        // 2. `phux attach <session>` — driver enters raw mode + alt
        //    screen; `RawModeGuard::install_with_stdout` parks the
        //    pre-attach Termios in `SAVED_TERMIOS`.
        // 3. In a sibling shell: `kill -INT <phux-pid>` (or hit Ctrl-C
        //    if your outer shell forwards it without phux eating it).
        // 4. `stty -a` again; ALL flags should match step (1). Before
        //    phux-2r7, only ICANON|ECHO|ISIG round-tripped and custom
        //    flags like `iutf8` were lost.
    }
}
