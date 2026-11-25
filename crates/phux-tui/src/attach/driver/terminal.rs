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

use crate::attach::outcome::AttachEnd;
use crate::attach::record::{SessionRecorder, TeeSink};
use crate::attach::render::write_reset;

mod raw_mode;
pub(super) use raw_mode::RawModeGuard;

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

/// Kitty keyboard flags pushed on the host while attached (ADR-0146):
/// disambiguate, so modified keys (Cmd+Return, Ctrl+I vs Tab) arrive as
/// `CSI u` while plain text stays plain bytes, plus report-alternates, so a
/// chord on a non-Latin layout carries its base-layout key (Ctrl+С is ^C).
const HOST_KITTY_KEYBOARD_PUSH: &[u8] = b"\x1b[>5u";
/// Pops [`HOST_KITTY_KEYBOARD_PUSH`]; emitted only while on the alt screen,
/// where the push landed (the kitty stack is per screen).
const HOST_KITTY_KEYBOARD_POP: &[u8] = b"\x1b[<u";

/// Enter the alt screen, hide the cursor, push kitty keyboard flags, and
/// enable bracketed paste and focus reports; with `mouse`, `?1002h` button
/// tracking (not `?1003h`) and SGR coordinates. A `switch-host` hand-off is
/// already on the alt screen and skips `?1049h`, which would clear it.
fn write_enter_alt_screen<W: Write>(out: &mut W, mouse: bool) -> io::Result<()> {
    if !INHERITED_ALT_SCREEN.take() {
        out.write_all(b"\x1b[?1049h")?;
    }
    out.write_all(HOST_KITTY_KEYBOARD_PUSH)?;
    out.write_all(b"\x1b[?25l")?;
    out.write_all(b"\x1b[?2004h")?;
    out.write_all(b"\x1b[?1004h")?;
    // DEC 2031: the host reports a light/dark theme switch.
    out.write_all(b"\x1b[?2031h")?;
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
/// a context menu or visible any-motion application consumes it.
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

/// The window title this client set on the host (`[chrome] title`), or
/// `None` when it set none and owes no pop.
static OUTER_TITLE: Mutex<Option<String>> = Mutex::new(None);

/// XTWINOPS: save the host's window title on its title stack, and restore it.
const PUSH_TITLE: &[u8] = b"\x1b[22;2t";
const POP_TITLE: &[u8] = b"\x1b[23;2t";

/// `[chrome] title` with `{session}` and `{window}` filled in once (a name
/// containing `{window}` stays literal). Characters that must never reach a
/// VT emitter are dropped, since names and pane titles are untrusted.
#[allow(
    clippy::literal_string_with_formatting_args,
    reason = "`{session}` and `{window}` are the option's own placeholders"
)]
pub(super) fn outer_title(format: &str, session: &str, window: &str) -> String {
    let mut title = String::new();
    let mut rest = format;
    while let Some(at) = rest.find('{') {
        title.push_str(&rest[..at]);
        let tail = &rest[at..];
        if let Some(after) = tail.strip_prefix("{session}") {
            title.push_str(session);
            rest = after;
        } else if let Some(after) = tail.strip_prefix("{window}") {
            title.push_str(window);
            rest = after;
        } else {
            title.push('{');
            rest = &tail[1..];
        }
    }
    title.push_str(rest);
    title.retain(|ch| crate::render::chrome::status_bar::widgets::cell_width(ch).is_some());
    title
}

/// Set the host's window title to `title` (OSC 2), saving the old one the
/// first time; an empty `title` hands the old one back. A no-op when
/// unchanged.
pub(super) fn sync_outer_title<W: Write>(out: &mut W, title: &str) -> io::Result<()> {
    let Ok(mut current) = OUTER_TITLE.lock() else {
        return Ok(());
    };
    if current.as_deref().unwrap_or_default() == title {
        return Ok(());
    }
    if title.is_empty() {
        out.write_all(POP_TITLE)?;
    } else {
        if current.is_none() {
            out.write_all(PUSH_TITLE)?;
        }
        write!(out, "\x1b]2;{title}\x07")?;
    }
    *current = (!title.is_empty()).then(|| title.to_owned());
    out.flush()
}

/// Restore the outer terminal; idempotent. Every teardown (Drop, signal,
/// panic hook, detach exit, switch-host hand-off) routes through here.
pub fn write_terminal_reset<W: Write>(out: &mut W) -> io::Result<()> {
    write_reset(out)?;
    out.write_all(b"\x1b[?2004l")?;
    out.write_all(b"\x1b[?1004l")?;
    out.write_all(b"\x1b[?2031l")?;
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
    // `try_lock`: a panic inside `sync_outer_title` must not deadlock here.
    if OUTER_TITLE
        .try_lock()
        .ok()
        .and_then(|mut t| t.take())
        .is_some()
    {
        out.write_all(POP_TITLE)?;
        out.flush()?;
    }
    if ALT_SCREEN_ACTIVE.swap(false, Ordering::SeqCst) {
        out.write_all(HOST_KITTY_KEYBOARD_POP)?;
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

/// Hand the outer terminal to the process a `switch-host` execs: cooked
/// termios and input modes as a detach restores them, but the alt screen
/// and frame stay up ([`crate::attach::hosts::ALT_SCREEN_HANDOFF_ENV`]), so
/// the re-dial does not flash the primary screen.
fn restore_terminal_for_handoff() {
    restore_terminal_termios();
    let mut stdout = io::stdout().lock();
    // The successor owns the alt screen from here; nothing in this process
    // may leave it.
    ALT_SCREEN_ACTIVE.store(false, Ordering::SeqCst);
    let _ = write_terminal_reset(&mut stdout);
    let _ = stdout.write_all(HOST_KITTY_KEYBOARD_POP);
    // The cursor stays hidden over the frame until the successor paints; a
    // successor that never attaches shows it again as it leaves.
    let _ = stdout.write_all(b"\x1b[?25l");
    let _ = stdout.flush();
}

/// An alt screen this process inherited from a hand-off and has neither
/// adopted (the raw-mode guard skips re-entering it) nor left.
static INHERITED_ALT_SCREEN: InheritedAltScreen = InheritedAltScreen::new();

/// The once-read hand-off flag. Read lazily so a process that never touches
/// the terminal pays nothing; only a real terminal stdout counts, so a
/// widget child that inherited the variable into a pipe writes nothing.
struct InheritedAltScreen {
    read: std::sync::Once,
    pending: AtomicBool,
}

impl InheritedAltScreen {
    const fn new() -> Self {
        Self {
            read: std::sync::Once::new(),
            pending: AtomicBool::new(false),
        }
    }

    /// Read the hand-off flag once; while it is owed, a terminating signal
    /// still leaves the alt screen.
    fn read_once(&self) {
        self.read.call_once(|| {
            let handed_off = std::env::var_os(crate::attach::hosts::ALT_SCREEN_HANDOFF_ENV)
                .is_some_and(|v| v == "1");
            let pending = handed_off && io::stdout().is_terminal();
            self.pending.store(pending, Ordering::SeqCst);
            if pending {
                phux_crash::arm_interrupt_leave(LEAVE_HANDED_OFF_ALT_SCREEN);
                // A panic (an abort in release) before the attach adopts the
                // screen still leaves it, ahead of the message.
                let previous = std::panic::take_hook();
                std::panic::set_hook(Box::new(move |info| {
                    leave_inherited_alt_screen();
                    previous(info);
                }));
            }
        });
    }

    /// Claim the inherited alt screen; true at most once per process.
    fn take(&self) -> bool {
        self.read_once();
        let claimed = self.pending.swap(false, Ordering::SeqCst);
        if claimed {
            phux_crash::disarm_interrupt_leave();
        }
        claimed
    }
}

/// Note a `switch-host` hand-off at process start, so even a Ctrl-C or panic
/// during the successor's dial hands the shell its screen back.
pub fn adopt_alt_screen_handoff() {
    INHERITED_ALT_SCREEN.read_once();
}

/// Leave an alt screen inherited from a `switch-host` hand-off, before the
/// CLI prints a cooked diagnostic. A no-op once adopted or without one.
pub fn leave_inherited_alt_screen() {
    if INHERITED_ALT_SCREEN.take() {
        let mut stdout = io::stdout().lock();
        let _ = stdout.write_all(LEAVE_HANDED_OFF_ALT_SCREEN);
        let _ = stdout.flush();
    }
}

/// Show the cursor the hand-off hid and leave the alt screen it kept.
const LEAVE_HANDED_OFF_ALT_SCREEN: &[u8] = b"\x1b[?25h\x1b[?1049l";

/// Hand the terminal to `phux attach` against `host`'s `session`. Returns
/// only if the exec failed: the alt screen is left again and the reason
/// printed on the shell's screen before the process exits.
#[allow(
    clippy::print_stderr,
    reason = "the TUI has already handed the terminal back; a failed exec \
              reports on the cooked terminal, like a detach explanation"
)]
#[allow(
    clippy::exit,
    reason = "the attach state was handed off; there is nothing to return to"
)]
pub(super) fn hand_off_to_host(host: &str, session: &str) -> ! {
    restore_terminal_for_handoff();
    let err = crate::attach::hosts::exec_switch_host(host, session);
    let mut stdout = io::stdout().lock();
    let _ = stdout.write_all(LEAVE_HANDED_OFF_ALT_SCREEN);
    let _ = stdout.flush();
    drop(stdout);
    eprintln!("phux: could not switch to {session} on {host}: {err}");
    std::process::exit(1);
}

/// Clean client exit after a server-acknowledged DETACH: restore the
/// terminal and exit the process instead of returning.
///
/// `tokio::io::stdin()` parks an uncancellable blocking `read()`, so runtime
/// teardown would hang forever, leaving a zombie client that steals the next
/// line the user types. Since this never returns, the last-pane explanation
/// (`AttachEnd::explanation`) is printed here, after the reset. Exit is `0`.
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
mod tests;
