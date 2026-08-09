//! Terminal restore sequences for signal handler context.
//!
//! MODIFIED FROM UPSTREAM (`xai-crash-handler`): the mode table below was
//! retargeted from grok's crossterm call sites to phux's hand-written DECSETs.
//! The byte constants themselves are unchanged.
//!
//! See <https://invisible-island.net/xterm/ctlseqs/ctlseqs.html> (DEC
//! Private Mode Reset / "Mouse Tracking" section) for the full spec.

// -----------------------------------------------------------------------
// Canonical list of DEC private modes reset on a fatal signal.
//
// [`RESTORE_SEQ`] is deliberately a SUPERSET of what phux itself enables.
// Resetting a mode that was never set is a no-op at the terminal, and the
// cost of missing one is a wedged terminal the user has to `reset` by hand —
// so the asymmetry is priced in favour of over-resetting.
//
//   Mode    Purpose                                        Enabled by phux at
//   ----    -------                                        ------------------
//   ?1049   Alternate screen buffer                        write_enter_alt_screen
//   ?25     Cursor visibility (show)                       write_enter_alt_screen
//   ?1002   Button-event mouse tracking (cell-motion held) write_enter_alt_screen,
//                                                          sync_mouse_capture (ADR-0048)
//   ?1006   SGR extended mouse reporting (coords >223)     write_enter_alt_screen,
//                                                          sync_mouse_capture
//   ?1003   All-motion mouse tracking (any movement)       sync_hover_tracking
//                                                          (raised only while a
//                                                          context menu is open)
//   ?2026   Synchronized update                            paint.rs, around every
//                                                          paint transaction
//   ?1000   Normal mouse tracking (X11 press/release)      never enabled by phux
//   ?1015   RXVT extended mouse reporting                  never enabled by phux
//   ?2004   Bracketed paste mode                           never enabled by phux
//                                                          (input.rs parses it, but
//                                                          the DECSET is never sent)
//   ?1004   Focus reporting (focus in/out events)          never enabled by phux
//                                                          (parsed, never requested)
//   CSI<u   Kitty keyboard protocol pop                    never pushed by phux
//                                                          (CSI-u is parsed, never
//                                                          requested)
//
// The "never enabled" rows stay in the sequence on purpose: they cost a
// handful of bytes on a path that only runs once, as the process dies, and
// they cover both a future phux that does enable them and a terminal left
// armed by something else before phux started.
// -----------------------------------------------------------------------

/// Raw CSI sequences to disable every mouse-tracking mode in the table above
/// (`?1000/?1002/?1003/?1015/?1006`) — the mouse subset of [`MOUSE_PASTE_RESET`],
/// without the bracketed-paste (`?2004l`) reset.
///
/// Use this to assert mouse tracking OFF without disturbing paste — e.g. to
/// clear a terminal left reporting by a prior run.
pub const MOUSE_TRACKING_RESET: &[u8] = b"\x1b[?1000l\x1b[?1002l\x1b[?1003l\x1b[?1015l\x1b[?1006l";

/// Raw CSI sequences to disable mouse tracking and bracketed paste.
pub const MOUSE_PASTE_RESET: &[u8] =
    b"\x1b[?1000l\x1b[?1002l\x1b[?1003l\x1b[?1015l\x1b[?1006l\x1b[?2004l";

/// Full escape sequence to restore the terminal to a sane state.
///
/// The kitty CSI-u pop precedes `?1049l` per spec (the protocol stack
/// is per-screen).
pub const RESTORE_SEQ: &[u8] =
    b"\x1b[?2026l\x1b[?25h\x1b[?1000l\x1b[?1002l\x1b[?1003l\x1b[?1015l\x1b[?1006l\x1b[?2004l\x1b[?1004l\x1b[<u\x1b[?1049l";

/// Write terminal restore sequences to stderr using raw `libc::write`.
///
/// This is async-signal-safe: it only calls `write(2)` on fd 2 (stderr).
/// Called from the signal handler after writing the crash blob.
#[cfg(unix)]
pub fn restore_in_signal_handler() {
    unsafe {
        libc::write(
            2, // stderr
            RESTORE_SEQ.as_ptr() as *const libc::c_void,
            RESTORE_SEQ.len(),
        );
    }
}

#[cfg(windows)]
pub fn restore_in_signal_handler() {
    unsafe {
        let stderr = windows_sys::Win32::System::Console::GetStdHandle(
            windows_sys::Win32::System::Console::STD_ERROR_HANDLE,
        );
        if !stderr.is_null() && stderr != -1isize as *mut std::ffi::c_void {
            let mut written: u32 = 0;
            windows_sys::Win32::Storage::FileSystem::WriteFile(
                stderr,
                RESTORE_SEQ.as_ptr(),
                RESTORE_SEQ.len() as u32,
                &mut written,
                std::ptr::null_mut(),
            );
        }
    }
}

#[cfg(not(any(unix, windows)))]
pub fn restore_in_signal_handler() {
    // No-op on unsupported platforms.
}

#[cfg(test)]
mod tests {
    use super::*;

    fn position_of(needle: &[u8]) -> usize {
        RESTORE_SEQ
            .windows(needle.len())
            .position(|w| w == needle)
            .unwrap_or_else(|| {
                panic!(
                    "RESTORE_SEQ must contain {:?}",
                    std::str::from_utf8(needle).unwrap_or("<binary>")
                )
            })
    }

    #[test]
    fn restore_seq_pops_kitty_before_alt_screen_leave() {
        assert!(position_of(b"\x1b[<u") < position_of(b"\x1b[?1049l"));
    }

    #[test]
    fn restore_seq_includes_all_modes() {
        for needle in [
            b"\x1b[?2026l".as_slice(),
            b"\x1b[?25h".as_slice(),
            b"\x1b[?1000l".as_slice(),
            b"\x1b[?1002l".as_slice(),
            b"\x1b[?1003l".as_slice(),
            b"\x1b[?1015l".as_slice(),
            b"\x1b[?1006l".as_slice(),
            b"\x1b[?2004l".as_slice(),
            b"\x1b[?1004l".as_slice(),
            b"\x1b[<u".as_slice(),
            b"\x1b[?1049l".as_slice(),
        ] {
            position_of(needle);
        }
    }

    #[test]
    fn restore_seq_ends_synchronized_update_first() {
        // Multiplexers (zellij/tmux) must stop buffering before subsequent
        // resets arrive, otherwise they get batched onto the wrong screen.
        let end_sync = b"\x1b[?2026l";
        assert_eq!(&RESTORE_SEQ[..end_sync.len()], end_sync);
    }
}
