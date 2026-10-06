//! Terminal restore sequences for signal handler context.
//!
//! MODIFIED FROM UPSTREAM (`xai-crash-handler`): the mode table below was
//! retargeted from grok's crossterm call sites to phux's hand-written DECSETs,
//! and the unused mouse-only constants and Windows writer were removed.
//! [`RESTORE_SEQ`] itself is unchanged.
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
//   ?2004   Bracketed paste mode                           write_enter_alt_screen
//   ?1004   Focus reporting (focus in/out events)          write_enter_alt_screen
//   CSI<u   Kitty keyboard protocol pop (flags 1 pushed    write_enter_alt_screen
//           with CSI>1u)                                   (ADR-0146)
//
// The "never enabled" rows stay in the sequence on purpose: they cost a
// handful of bytes on a path that only runs once, as the process dies, and
// they cover both a future phux that does enable them and a terminal left
// armed by something else before phux started.
// -----------------------------------------------------------------------

/// Full escape sequence to restore the terminal to a sane state.
///
/// The kitty CSI-u pop precedes `?1049l` per spec (the protocol stack
/// is per-screen).
pub const RESTORE_SEQ: &[u8] =
    b"\x1b[?2026l\x1b[?25h\x1b[?1000l\x1b[?1002l\x1b[?1003l\x1b[?1015l\x1b[?1006l\x1b[?2004l\x1b[?1004l\x1b[<u\x1b[?1049l";

/// Write terminal restore sequences to stderr using raw `libc::write`.
///
/// This is async-signal-safe: it only calls `write(2)` on fd 2 (stderr).
#[cfg(unix)]
pub(crate) fn restore_in_signal_handler() {
    // SAFETY: `write(2)` is async-signal-safe and reads exactly
    // `RESTORE_SEQ.len()` bytes from a `'static` slice. A short or failed
    // write is ignored: there is nothing useful to do about it while dying.
    unsafe {
        libc::write(
            2, // stderr
            RESTORE_SEQ.as_ptr().cast::<libc::c_void>(),
            RESTORE_SEQ.len(),
        );
    }
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
