//! Outer-terminal state ownership: raw mode + alt screen (`RawModeGuard`),
//! mouse/hover DECSET reconciliation, termios snapshots, and the
//! signal/panic/detach teardown paths.

use std::cell::RefCell;
use std::io::{self, IsTerminal, Write};
use std::os::fd::AsFd;
use std::rc::Rc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

use phux_protocol::ids::ResourceId;
use rustix::termios::{LocalModes, OptionalActions, Termios};

use crate::attach::outcome::{AttachEnd, AttachError};
use crate::attach::record::{SessionRecorder, TeeSink};
use crate::attach::render::write_reset;

/// RAII handle: raw-mode stdin plus alt-screen stdout, restored on drop (so
/// a panic anywhere in the attach loop still leaves a usable terminal).
pub(super) struct RawModeGuard {
    original_termios: Termios,
}

impl std::fmt::Debug for RawModeGuard {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RawModeGuard").finish_non_exhaustive()
    }
}

impl RawModeGuard {
    /// Install the guard; errors if stdin is not a TTY. The entry bytes go to
    /// `out`. `mouse` (ADR-0048) also emits `?1002h?1006h` so divider drags
    /// work without an inner mouse mode.
    pub(super) fn install_with_stdout<W: Write>(
        out: &mut W,
        mouse: bool,
    ) -> Result<Self, AttachError> {
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

        // Reads complete per byte, with no timeout.
        raw.special_codes[rustix::termios::SpecialCodeIndex::VMIN] = 1;
        raw.special_codes[rustix::termios::SpecialCodeIndex::VTIME] = 0;

        rustix::termios::tcsetattr(fd, OptionalActions::Now, &raw)
            .map_err(|err| AttachError::Terminal(format!("tcsetattr: {err}")))?;

        // Enter the alt screen first so the first paint never lands on the
        // normal screen.
        write_enter_alt_screen(out, mouse).map_err(AttachError::Io)?;

        // Set only after the writes succeed, so signal cleanup knows to leave.
        ALT_SCREEN_ACTIVE.store(true, Ordering::SeqCst);

        // The fatal-signal handler also emits DECSET resets while the alt
        // screen is up (downgraded again in Drop).
        phux_crash::enable_terminal_escape_restore();

        // A global snapshot for the signal arms and panic hook, which cannot
        // reach this instance.
        save_termios_snapshot(original.clone());

        Ok(Self {
            original_termios: original,
        })
    }
}

impl Drop for RawModeGuard {
    fn drop(&mut self) {
        // Best effort: a panic in Drop is worse than a wedged terminal. Clear
        // the global snapshot so a later failed install cannot inherit it.
        let _ = take_termios_snapshot();
        let stdin = io::stdin();
        crate::attach::terminal_probe::discard_pending(stdin.as_fd());
        let _ =
            rustix::termios::tcsetattr(stdin.as_fd(), OptionalActions::Now, &self.original_termios);
        let mut out = io::stdout().lock();
        let _ = write_terminal_reset(&mut out);
        ALT_SCREEN_ACTIVE.store(false, Ordering::SeqCst);

        // Back to termios-only restore, last, so a fault during the reset
        // above is still covered.
        phux_crash::disable_terminal_escape_restore();
    }
}

/// Whether the alt-screen entry sequence is active, so a signal during the
/// pre-handshake stage emits no stray leave sequence. Separate from the
/// termios snapshot: the two flip at different points of install.
static ALT_SCREEN_ACTIVE: AtomicBool = AtomicBool::new(false);

/// Whether the client enabled its own mouse tracking (ADR-0048); the reset
/// emits `?1006l?1002l` before leaving the alt screen so the host's native
/// selection returns.
static MOUSE_CAPTURE_ACTIVE: AtomicBool = AtomicBool::new(false);

/// The pre-raw termios, for a true `tcsetattr` restore from paths that
/// skip Drop (`process::exit`). The signal arms are tokio futures and the
/// panic hook runs in normal context, so the mutex is safe in both.
static SAVED_TERMIOS: Mutex<Option<Termios>> = Mutex::new(None);

/// Park a termios snapshot (a poisoned lock is ignored).
fn save_termios_snapshot(t: Termios) {
    if let Ok(mut slot) = SAVED_TERMIOS.lock() {
        *slot = Some(t);
    }
}

/// Take the termios snapshot (`None` if absent or poisoned).
fn take_termios_snapshot() -> Option<Termios> {
    SAVED_TERMIOS.lock().ok().and_then(|mut slot| slot.take())
}

/// Whether [`install_panic_hook_once`] already ran.
static PANIC_HOOK_INSTALLED: AtomicBool = AtomicBool::new(false);

/// Enter the alt screen, hide the cursor, and enable bracketed paste
/// (`?2004h`, so a paste arrives as one `InputEvent::Paste`) and focus
/// reports (`?1004h`). With `mouse`, also `?1002h` button-event tracking (not
/// `?1003h`, which floods hover traffic) and `?1006h` SGR coordinates.
fn write_enter_alt_screen<W: Write>(out: &mut W, mouse: bool) -> io::Result<()> {
    out.write_all(b"\x1b[?1049h")?;
    out.write_all(b"\x1b[?25l")?;
    out.write_all(b"\x1b[?2004h")?;
    out.write_all(b"\x1b[?1004h")?;
    if mouse {
        out.write_all(b"\x1b[?1002h\x1b[?1006h")?;
        MOUSE_CAPTURE_ACTIVE.store(true, Ordering::SeqCst);
    }
    out.flush()
}

/// Whether mouse capture should be on: the global gate, unless the focused
/// pane opted out (`set-pane mouse off`).
pub(super) fn desired_mouse_capture(
    cfg_on: bool,
    focused: Option<&ResourceId>,
    optout: &std::collections::HashSet<ResourceId>,
) -> bool {
    cfg_on && !focused.is_some_and(|id| optout.contains(id))
}

/// Reconcile the client's mouse-tracking DECSET with `want` (a no-op when
/// unchanged).
pub(super) fn sync_mouse_capture<W: Write>(out: &mut W, want: bool) -> io::Result<()> {
    if MOUSE_CAPTURE_ACTIVE.swap(want, Ordering::SeqCst) == want {
        return Ok(());
    }
    if want {
        out.write_all(b"\x1b[?1002h\x1b[?1006h")?;
    } else {
        // Drop any-motion first so capture-off never leaves `?1003h` armed.
        if HOVER_TRACKING_ACTIVE.swap(false, Ordering::SeqCst) {
            out.write_all(b"\x1b[?1003l")?;
        }
        out.write_all(b"\x1b[?1006l\x1b[?1002l")?;
    }
    out.flush()
}

/// Whether any-motion reporting (`?1003h`) is raised on top of capture;
/// only a hover-tracking context menu consumes it.
static HOVER_TRACKING_ACTIVE: AtomicBool = AtomicBool::new(false);

/// Reconcile any-motion reporting with `want`; never raised without capture.
/// Leaving re-asserts `?1002h`, since some terminals treat both DECSETs as
/// one mode.
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

/// Restore the outer terminal: drop SGR, bracketed paste, hover, and mouse
/// capture, show the cursor, and leave the alt screen if entered.
/// Idempotent.
pub fn write_terminal_reset<W: Write>(out: &mut W) -> io::Result<()> {
    write_reset(out)?;
    out.write_all(b"\x1b[?2004l")?;
    out.write_all(b"\x1b[?1004l")?;
    out.flush()?;
    if HOVER_TRACKING_ACTIVE.swap(false, Ordering::SeqCst) {
        out.write_all(b"\x1b[?1003l")?;
        out.flush()?;
    }
    // Before leaving the alt screen, so native selection returns on detach.
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

/// Best-effort termios restore for signal and clean-detach exits: the saved
/// snapshot when present (keeping flags like IUTF8), else a re-cook.
fn restore_terminal_termios() {
    let stdin = io::stdin();
    let fd = stdin.as_fd();
    // A color-probe reply still sitting in the input queue is what the shell
    // prints as `^[]10;rgb:...` once echo comes back.
    crate::attach::terminal_probe::discard_pending(fd);
    if let Some(saved) = take_termios_snapshot() {
        // True restore: the snapshot is exactly what `tcgetattr`
        // returned before we flipped into raw mode.
        let _ = rustix::termios::tcsetattr(fd, OptionalActions::Now, &saved);
    } else if let Ok(mut termios) = rustix::termios::tcgetattr(fd) {
        // Re-cook fallback: canonical flags back on; custom flags are lost.
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

/// Put the outer terminal back to cooked mode and the primary screen for a
/// process hand-off (`switch-host` execs `phux attach`), the same restore a
/// detach performs, without exiting.
pub(super) fn restore_terminal_for_handoff() {
    restore_terminal_termios();
    let mut stdout = io::stdout().lock();
    write_terminal_reset_and_finalize(&mut stdout, None);
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
/// Because this never returns, the CLI's own `Ok(end)`
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

/// Install (once) a panic hook that logs the panic and a backtrace to the
/// file sink, then restores the terminal, then chains the default hook.
pub(super) fn install_panic_hook_once() {
    if PANIC_HOOK_INSTALLED.swap(true, Ordering::SeqCst) {
        return;
    }
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        // `Backtrace::capture` honors `RUST_BACKTRACE`.
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

    /// A real `Termios` from `/dev/tty`, or `None` without a controlling TTY.
    fn try_borrow_real_termios() -> Option<Termios> {
        let tty = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open("/dev/tty")
            .ok()?;
        rustix::termios::tcgetattr(tty.as_fd()).ok()
    }

    /// The termios snapshot round-trips exactly once.
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

    /// Entry enables bracketed paste, focus reports, and (when configured)
    /// mouse capture; the reset undoes them before leaving the alt screen.
    #[test]
    fn outer_terminal_modes_enable_and_disable_bytes() {
        let _guard = TERMINAL_RESET_TEST_LOCK
            .lock()
            .expect("terminal reset test lock");
        MOUSE_CAPTURE_ACTIVE.store(false, Ordering::SeqCst);
        ALT_SCREEN_ACTIVE.store(false, Ordering::SeqCst);

        let mut entry = Vec::new();
        write_enter_alt_screen(&mut entry, true).unwrap();
        assert!(
            entry.windows(8).any(|w| w == b"\x1b[?2004h"),
            "entry must enable bracketed paste framing: {entry:?}"
        );
        assert!(
            entry.windows(8).any(|w| w == b"\x1b[?1004h"),
            "entry must enable focus reports: {entry:?}"
        );
        assert!(
            entry.windows(8).any(|w| w == b"\x1b[?1002h"),
            "entry must enable button-motion tracking: {entry:?}"
        );
        assert!(
            entry.windows(8).any(|w| w == b"\x1b[?1006h"),
            "entry must enable SGR coordinates: {entry:?}"
        );
        // Set by install on a real attach; needed for the full leave path.
        ALT_SCREEN_ACTIVE.store(true, Ordering::SeqCst);
        // Reset emits the leave pair before the ?1049l alt-screen leave.
        let mut reset = Vec::new();
        write_terminal_reset(&mut reset).unwrap();
        let pos_2004l = reset
            .windows(8)
            .position(|w| w == b"\x1b[?2004l")
            .expect("reset must disable bracketed paste framing");
        assert!(
            reset.windows(8).any(|w| w == b"\x1b[?1004l"),
            "reset must disable focus reports: {reset:?}"
        );
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
            pos_2004l < pos_1049l && pos_1006l < pos_1049l && pos_1002l < pos_1049l,
            "outer-terminal mode resets must precede the alt-screen leave: {reset:?}"
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

    /// `mouse = false` skips mouse DECSET but keeps bracketed paste.
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
            entry.windows(8).any(|w| w == b"\x1b[?2004h"),
            "mouse=false must still enable bracketed paste: {entry:?}"
        );
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
            reset.windows(8).any(|w| w == b"\x1b[?2004l"),
            "reset must always disable bracketed paste: {reset:?}"
        );
        assert!(
            !reset.windows(8).any(|w| w == b"\x1b[?1002l"),
            "no capture ⇒ no mouse-disable on reset: {reset:?}"
        );
        assert!(
            !reset.windows(8).any(|w| w == b"\x1b[?1006l"),
            "no capture ⇒ no SGR mouse-disable on reset: {reset:?}"
        );
    }

    /// Leave pair when dropping, enter pair when restoring, nothing when
    /// unchanged.
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

    /// Hover is raised only on top of live capture and always unwound.
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

    /// Capture follows focus — wanted iff the global gate is on
    /// AND the focused pane has not opted out.
    #[test]
    fn desired_mouse_capture_follows_focused_pane_optout() {
        let t1 = ResourceId::local(1);
        let t2 = ResourceId::local(2);
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
}
