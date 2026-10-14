//! Streaming recognizer for an open ECMA-48 escape sequence.
//!
//! Encoder modes change only inside one of these sequences. A chunk of plain
//! text that starts and ends in [`InputModeSpan::Ground`] cannot change them,
//! so the actor keeps the input snapshot the lane already has.
//!
//! Like libghostty's stream, ground state decodes UTF-8 and only `ESC` opens
//! a sequence: a byte in `0x80..=0x9F` there is part of a character (`┐` is
//! `E2 94 90`), never an 8-bit C1 introducer.

/// Where the next PTY byte sits in an ECMA-48 control sequence.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(super) enum InputModeSpan {
    /// No sequence is open.
    #[default]
    Ground,
    /// Saw `ESC`.
    Escape,
    /// Inside CSI, waiting for a final byte.
    Csi,
    /// Inside OSC, waiting for BEL or ST.
    Osc,
    /// Saw `ESC` while inside OSC; `\` would be ST.
    OscEsc,
    /// Inside DCS, SOS, PM, or APC.
    String,
    /// Saw `ESC` while inside a string; `\` would be ST.
    StringEsc,
}

impl InputModeSpan {
    /// Advance across `bytes`.
    ///
    /// `true` when a sequence was already open or this chunk entered one,
    /// including a sequence that completed here. A later plain-text chunk
    /// returns `false` again.
    pub(super) fn observe(&mut self, mut bytes: &[u8]) -> bool {
        let mut affects_modes = *self != Self::Ground;
        loop {
            if *self == Self::Ground {
                // Text: skip straight to the next sequence, if any.
                let Some(escape) = memchr::memchr(0x1b, bytes) else {
                    return affects_modes;
                };
                affects_modes = true;
                bytes = &bytes[escape..];
            }
            let Some((&byte, rest)) = bytes.split_first() else {
                return affects_modes;
            };
            self.step(byte);
            bytes = rest;
        }
    }

    const fn step(&mut self, byte: u8) {
        *self = match *self {
            Self::Ground => match byte {
                0x1b => Self::Escape,
                _ => Self::Ground,
            },
            Self::Escape => match byte {
                b'[' => Self::Csi,
                b']' => Self::Osc,
                b'P' | b'X' | b'^' | b'_' => Self::String,
                0x1b => Self::Escape,
                _ => Self::Ground,
            },
            Self::Csi => match byte {
                0x1b => Self::Escape,
                0x18 | 0x1a | 0x40..=0x7e => Self::Ground,
                _ => Self::Csi,
            },
            Self::Osc => match byte {
                0x1b => Self::OscEsc,
                0x07 | 0x9c | 0x18 | 0x1a => Self::Ground,
                _ => Self::Osc,
            },
            Self::OscEsc => match byte {
                b'\\' => Self::Ground,
                0x1b => Self::Escape,
                _ => Self::Osc,
            },
            Self::String => match byte {
                0x1b => Self::StringEsc,
                0x9c | 0x18 | 0x1a => Self::Ground,
                _ => Self::String,
            },
            Self::StringEsc => match byte {
                b'\\' => Self::Ground,
                0x1b => Self::Escape,
                _ => Self::String,
            },
        };
    }
}

#[cfg(test)]
mod tests {
    use super::InputModeSpan;

    fn affects(chunks: &[&[u8]]) -> (Vec<bool>, InputModeSpan) {
        let mut span = InputModeSpan::default();
        let flags = chunks.iter().map(|chunk| span.observe(chunk)).collect();
        (flags, span)
    }

    #[test]
    fn plain_text_stays_in_ground() {
        let (flags, span) = affects(&[b"hello", b"a", b"\r\n\t"]);
        assert_eq!(flags, vec![false, false, false]);
        assert_eq!(span, InputModeSpan::Ground);
    }

    #[test]
    fn a_complete_csi_publishes_and_the_next_letter_does_not() {
        let (flags, span) = affects(&[b"\x1b[?2004h", b"a"]);
        assert_eq!(flags, vec![true, false]);
        assert_eq!(span, InputModeSpan::Ground);
    }

    #[test]
    fn a_csi_split_after_esc_publishes_on_the_completing_chunk() {
        let (flags, span) = affects(&[b"\x1b", b"[?2004h", b"a"]);
        assert_eq!(flags, vec![true, true, false]);
        assert_eq!(span, InputModeSpan::Ground);
    }

    #[test]
    fn a_csi_fed_one_byte_at_a_time_stays_open_until_the_final() {
        let bytes = b"\x1b[?2004h";
        let mut span = InputModeSpan::default();
        for (index, byte) in bytes.iter().enumerate() {
            assert!(
                span.observe(&[*byte]),
                "byte {index} of an open CSI must publish"
            );
        }
        assert_eq!(span, InputModeSpan::Ground);
        assert!(!span.observe(b"a"));
    }

    #[test]
    fn utf8_continuation_bytes_in_the_c1_range_are_text() {
        // `┐` E2 94 90, `┘` E2 94 98, `❯` E2 9D AF, and an emoji F0 9F 98 80
        // carry the bytes of DCS, SOS, OSC, and APC introducers.
        let (flags, span) = affects(&["┐┘", "❯ ", "\u{1f600}"].map(str::as_bytes));
        assert_eq!(flags, vec![false, false, false]);
        assert_eq!(span, InputModeSpan::Ground);
    }

    #[test]
    fn a_sequence_after_wide_text_is_still_seen() {
        let (flags, span) = affects(&["┐\x1b[?2004h┘".as_bytes(), b"a"]);
        assert_eq!(flags, vec![true, false]);
        assert_eq!(span, InputModeSpan::Ground);
    }

    #[test]
    fn an_osc_split_mid_payload_publishes_when_it_completes() {
        let (flags, span) = affects(&[b"\x1b]0;ti", b"tle\x1b\\", b"z"]);
        assert_eq!(flags, vec![true, true, false]);
        assert_eq!(span, InputModeSpan::Ground);
    }

    #[test]
    fn can_aborts_a_csi_back_to_ground() {
        let (flags, span) = affects(&[b"\x1b[123\x18hello", b"a"]);
        assert_eq!(flags, vec![true, false]);
        assert_eq!(span, InputModeSpan::Ground);
    }

    #[test]
    fn text_after_a_finished_sequence_in_the_same_chunk_still_publishes() {
        let (flags, span) = affects(&[b"\x1b[?2004hhello"]);
        assert_eq!(flags, vec![true]);
        assert_eq!(span, InputModeSpan::Ground);
    }
}
