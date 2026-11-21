//! Raw scans for OSC sequences the engine parses but does not report: the
//! OSC 7501 `resume=` key (`super::resume`) and the OSC 1337
//! `SetUserVar=phux_icon` pane icon (`docs/program-sequences.md`). Each reads
//! the same PTY bytes the engine is fed, before it is fed them.

const ESC: u8 = 0x1b;

#[derive(Debug, Clone, Copy, Default)]
enum State {
    #[default]
    Ground,
    /// After `ESC`.
    Esc,
    /// After `ESC ]` and this many bytes of the intro.
    Intro(usize),
    Body,
    /// After `ESC` inside the body: `\` ends the sequence.
    BodyEsc,
}

/// Finds `ESC ] <intro> <body> ST` and hands over each body, across writes.
#[derive(Debug)]
pub(super) struct OscScan {
    intro: &'static [u8],
    max_body: usize,
    state: State,
    body: Vec<u8>,
}

impl OscScan {
    /// Scan for `intro`; a body longer than `max_body` is not ours.
    pub(super) const fn new(intro: &'static [u8], max_body: usize) -> Self {
        Self {
            intro,
            max_body,
            state: State::Ground,
            body: Vec::new(),
        }
    }

    /// Hand `done` each body `bytes` finishes, continuing a sequence an
    /// earlier write left open. Costs one `memchr` when no `ESC` is present.
    pub(super) fn scan(&mut self, bytes: &[u8], mut done: impl FnMut(&[u8])) {
        let mut rest = bytes;
        loop {
            if matches!(self.state, State::Ground) {
                let Some(at) = memchr::memchr(ESC, rest) else {
                    break;
                };
                rest = &rest[at..];
            }
            let Some((&byte, tail)) = rest.split_first() else {
                break;
            };
            rest = tail;
            self.state = match (self.state, byte) {
                (State::Body, 0x07) | (State::BodyEsc, b'\\') => {
                    done(&self.body);
                    State::Ground
                }
                (State::Body, ESC) => State::BodyEsc,
                // CAN and SUB cancel the sequence; a longer one is not ours.
                (State::Body, 0x18 | 0x1a) => State::Ground,
                (State::Body, _) if self.body.len() == self.max_body => State::Ground,
                (State::Body, _) => {
                    self.body.push(byte);
                    State::Body
                }
                (State::Esc | State::BodyEsc, b']') => State::Intro(0),
                (State::Intro(seen), _) if self.intro.get(seen) == Some(&byte) => {
                    if seen + 1 == self.intro.len() {
                        self.body.clear();
                        State::Body
                    } else {
                        State::Intro(seen + 1)
                    }
                }
                (_, ESC) => State::Esc,
                _ => State::Ground,
            };
        }
    }
}

/// An icon change: `Some(icon)` sets it, `None` removes it.
pub(super) type IconChange = Option<String>;

/// The OSC 1337 `phux_icon` user variable: the icon a program gives its pane.
#[derive(Debug)]
pub(super) struct IconScan(OscScan);

impl Default for IconScan {
    fn default() -> Self {
        // Base64 of the longest icon, with room to spare.
        Self(OscScan::new(b"1337;SetUserVar=phux_icon=", 64))
    }
}

impl IconScan {
    /// The last icon change `bytes` asks for: `Some(None)` removes the icon.
    /// A value that breaks the rule changes nothing.
    pub(super) fn scan(&mut self, bytes: &[u8]) -> Option<IconChange> {
        let mut change = None;
        self.0.scan(bytes, |value| {
            if let Some(next) = icon(value) {
                change = Some(next);
            }
        });
        change
    }
}

fn icon(value: &[u8]) -> Option<IconChange> {
    use base64::Engine as _;
    if value.is_empty() {
        return Some(None);
    }
    let value = value
        .strip_suffix(b"==")
        .or_else(|| value.strip_suffix(b"="))
        .unwrap_or(value);
    let bytes = base64::engine::general_purpose::STANDARD_NO_PAD
        .decode(value)
        .ok()?;
    let icon = String::from_utf8(bytes).ok()?;
    phux_config_core::pane_icon::is_icon(&icon).then_some(Some(icon))
}

#[cfg(test)]
mod tests {
    use super::IconScan;

    /// `phux_icon` sets (base64, padding optional, split across writes) and
    /// removes the icon; another variable, a bad value, and an overlong
    /// sequence change nothing.
    #[test]
    fn a_phux_icon_user_variable_sets_and_removes_the_icon() {
        let crab = || Some(Some("\u{1f980}".to_owned()));
        let mut scan = IconScan::default();
        assert_eq!(
            scan.scan(b"\x1b]1337;SetUserVar=phux_icon=8J+mgA==\x07"),
            crab()
        );
        assert_eq!(scan.scan(b"x\x1b]1337;SetUser"), None);
        assert_eq!(scan.scan(b"Var=phux_icon=8J+mgA\x1b\\y"), crab());
        assert_eq!(
            scan.scan(b"\x1b]1337;SetUserVar=phux_icon=\x07"),
            Some(None)
        );
        let long = format!("\x1b]1337;SetUserVar=phux_icon={}\x07", "QUJD".repeat(20));
        for ignored in [
            b"\x1b]1337;SetUserVar=other=8J+mgA==\x07".as_slice(),
            b"\x1b]1337;SetUserVar=phux_icon=YSBi\x07", // "a b"
            b"\x1b]1337;SetUserVar=phux_icon=!!\x07",
            b"\x1b]1337;SetUserVar=phux_icon=8J+mgA\x18\x07",
            long.as_bytes(),
        ] {
            assert_eq!(scan.scan(ignored), None, "{ignored:?}");
        }
    }
}
