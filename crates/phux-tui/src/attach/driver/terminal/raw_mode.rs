//! Own the outer terminal termios and restore it on every unwind path.
use super::{
    ALT_SCREEN_ACTIVE, HOST_KITTY_KEYBOARD_PUSH, Termios, save_termios_snapshot,
    take_termios_snapshot, write_enter_alt_screen, write_terminal_reset,
};
use crate::attach::outcome::AttachError;
use rustix::termios::{LocalModes, OptionalActions};
use std::{
    io::{self, IsTerminal, Write},
    os::fd::AsFd,
    sync::atomic::Ordering,
};

/// RAII handle: raw-mode stdin plus alt-screen stdout, restored on drop (so
/// a panic anywhere in the attach loop still leaves a usable terminal).
pub(in crate::attach::driver) struct RawModeGuard {
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
    pub(in crate::attach::driver) fn install_with_stdout<W: Write>(
        out: &mut W,
        mouse: bool,
    ) -> Result<Self, AttachError> {
        Self::install(out, mouse, true)
    }

    /// Native clients leave rendering and scrollback to the outer emulator.
    /// Cleanup still leaves any alternate screen enabled by the inner app.
    pub(in crate::attach::driver) fn install_native(
        out: &mut impl Write,
    ) -> Result<Self, AttachError> {
        Self::install(out, false, false)
    }

    fn install(out: &mut impl Write, mouse: bool, alternate: bool) -> Result<Self, AttachError> {
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

        // Own the termios change before writing so a broken output pipe
        // during entry still restores the terminal.
        let guard = Self {
            original_termios: original.clone(),
        };

        // Arm cleanup before entry: a partial escape write must also leave
        // the alternate screen and restore the saved termios.
        ALT_SCREEN_ACTIVE.store(true, Ordering::SeqCst);

        // The fatal-signal handler also emits DECSET resets while the alt
        // screen is up (downgraded again in Drop).
        phux_crash::enable_terminal_escape_restore();

        // A global snapshot for the signal arms and panic hook, which cannot
        // reach this instance.
        save_termios_snapshot(original);
        if alternate {
            write_enter_alt_screen(out, mouse).map_err(AttachError::Io)?;
        } else {
            out.write_all(HOST_KITTY_KEYBOARD_PUSH)?;
            out.write_all(b"\x1b[?2004h\x1b[?1004h\x1b[?25h")?;
            out.flush()?;
        }

        Ok(guard)
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
