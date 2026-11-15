//! Finds OSC 52 clipboard reads (`ESC ] 52 ; c ; ? ST`) in live output.
//!
//! See `docs/program-sequences.md#clipboard-osc-52`. libghostty's read
//! hook cannot tell `c` from an unknown selection, nor OSC 52 from OSC 5522,
//! so the frontend gets the selection from the bytes themselves. A query is
//! at most a few bytes long; anything longer is not one.

/// The selection a program asked to read. Every other selection (empty,
/// `s`, `0`-`7`, several at once) is denied by never being reported.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClipboardSelection {
    /// `c`: the clipboard.
    Clipboard,
    /// `p`: the primary selection, where the host has one.
    Primary,
}

impl ClipboardSelection {
    /// The OSC 52 selection byte.
    #[must_use]
    pub const fn byte(self) -> u8 {
        match self {
            Self::Clipboard => b'c',
            Self::Primary => b'p',
        }
    }

    /// The selection a byte names, if it is one a read may answer.
    #[must_use]
    pub const fn from_byte(byte: u8) -> Option<Self> {
        match byte {
            b'c' => Some(Self::Clipboard),
            b'p' => Some(Self::Primary),
            _ => None,
        }
    }
}

/// The longest OSC body a read can be: `52;c;?`, with room for a few extra
/// selection bytes so a multi-selection query is seen (and refused) whole.
const MAX_BODY: usize = 16;

/// Scanner state carried across output chunks.
#[derive(Debug, Default)]
pub struct Osc52ReadScanner {
    state: State,
    body: Vec<u8>,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
enum State {
    #[default]
    Ground,
    /// Saw ESC.
    Escape,
    /// Inside an OSC body.
    Osc,
    /// Inside an OSC body, saw ESC (maybe ST).
    OscEscape,
}

impl Osc52ReadScanner {
    /// Scan one chunk of live output; the last read it completes, if any.
    pub fn feed(&mut self, bytes: &[u8]) -> Option<ClipboardSelection> {
        let mut found = None;
        let mut rest = bytes;
        while !rest.is_empty() {
            if self.state == State::Ground {
                // Fast path: nothing happens until the next ESC.
                let Some(at) = rest.iter().position(|&b| b == 0x1B) else {
                    break;
                };
                rest = &rest[at..];
            }
            let Some((&b, tail)) = rest.split_first() else {
                break;
            };
            rest = tail;
            if let Some(selection) = self.step(b) {
                found = Some(selection);
            }
        }
        found
    }

    fn step(&mut self, b: u8) -> Option<ClipboardSelection> {
        match (self.state, b) {
            (State::Ground | State::Escape | State::OscEscape, 0x1B) => {
                self.state = State::Escape;
            }
            (State::Escape, b']') => {
                self.body.clear();
                self.state = State::Osc;
            }
            (State::Osc, 0x07) | (State::OscEscape, b'\\') => return self.finish(),
            (State::Osc, 0x1B) => self.state = State::OscEscape,
            (State::Osc, _) if self.body.len() < MAX_BODY => self.body.push(b),
            // Too long to be a read, or a string cut short: not one.
            _ => self.state = State::Ground,
        }
        None
    }

    const fn finish(&mut self) -> Option<ClipboardSelection> {
        self.state = State::Ground;
        match self.body.as_slice() {
            [b'5', b'2', b';', selection, b';', b'?'] => ClipboardSelection::from_byte(*selection),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{ClipboardSelection, Osc52ReadScanner};

    fn scan(chunks: &[&[u8]]) -> Option<ClipboardSelection> {
        let mut scanner = Osc52ReadScanner::default();
        chunks.iter().fold(None, |_, chunk| scanner.feed(chunk))
    }

    #[test]
    fn reads_of_c_and_p_are_found_with_either_terminator() {
        assert_eq!(
            scan(&[b"hi\x1b[1m\x1b]52;c;?\x07"]),
            Some(ClipboardSelection::Clipboard)
        );
        assert_eq!(
            scan(&[b"\x1b]52;p;?\x1b\\"]),
            Some(ClipboardSelection::Primary)
        );
    }

    #[test]
    fn a_read_split_across_chunks_is_found() {
        assert_eq!(
            scan(&[b"x\x1b", b"]5", b"2;c", b";?\x1b", b"\\"]),
            Some(ClipboardSelection::Clipboard)
        );
    }

    #[test]
    fn unknown_selections_writes_and_other_strings_are_not_reads() {
        for bytes in [
            &b"\x1b]52;s;?\x07"[..],
            b"\x1b]52;;?\x07",
            b"\x1b]52;cp;?\x07",
            b"\x1b]52;x;?\x07",
            b"\x1b]52;c;aGk=\x07",
            b"\x1b]5522;type=read;?\x07",
            b"\x1b]0;52;c;?\x07",
            b"\x1b]52;c;?",
        ] {
            assert_eq!(scan(&[bytes]), None, "{bytes:?}");
        }
    }

    #[test]
    fn a_long_string_before_a_read_does_not_hide_it() {
        let mut long = b"\x1b]2;".to_vec();
        long.extend(std::iter::repeat_n(b'a', 100));
        long.extend_from_slice(b"\x07\x1b]52;c;?\x07");
        assert_eq!(scan(&[&long]), Some(ClipboardSelection::Clipboard));
    }
}
