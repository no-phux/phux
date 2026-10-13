//! Streaming recognizer for an open ECMA-48 escape sequence.
//!
//! Encoder modes change only inside one of these sequences. A chunk of plain
//! text that starts and ends in [`InputModeSpan::Ground`] cannot change them,
//! so the actor keeps the input snapshot the lane already has.

/// Where the next PTY byte sits in an ECMA-48 control sequence.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(super) enum InputModeSpan {
    /// No sequence is open.
    #[default]
    Ground,
    /// Saw `ESC` (or a C1 introducer that still needs its next byte).
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
    pub(super) fn observe(&mut self, bytes: &[u8]) -> bool {
        let mut affects_modes = !matches!(self, Self::Ground);
        for &byte in bytes {
            let before = *self;
            self.step(byte);
            if !matches!(before, Self::Ground) || !matches!(self, Self::Ground) {
                affects_modes = true;
            }
        }
        affects_modes
    }

    const fn step(&mut self, byte: u8) {
        *self = match *self {
            Self::Ground => match byte {
                0x1b => Self::Escape,
                0x9b => Self::Csi,
                0x9d => Self::Osc,
                0x90 | 0x98 | 0x9e | 0x9f => Self::String,
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
    fn c1_csi_and_osc_are_sequences() {
        let (flags, span) = affects(&[b"\x9b?2004h", b"a", b"\x9d0;title\x07", b"b"]);
        assert_eq!(flags, vec![true, false, true, false]);
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
