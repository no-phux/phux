//! Keeps the host terminal's answer to an OSC 52 clipboard read: the only
//! OSC string the input parser does not drop. Any other string is skipped
//! after at most three bytes, and an answer is bounded by the 1 MiB
//! clipboard cap.

use phux_protocol::wire::frame::MAX_CLIPBOARD_TEXT;

/// Base64 of the largest clipboard a reply may carry, plus `52;c;`.
const MAX_BODY: usize = MAX_CLIPBOARD_TEXT.div_ceil(3) * 4 + 16;

#[derive(Debug, Default)]
pub(super) struct Osc52Capture {
    active: bool,
    body: Vec<u8>,
    reply: Option<Vec<u8>>,
}

impl Osc52Capture {
    pub(super) const fn new() -> Self {
        Self {
            active: false,
            body: Vec::new(),
            reply: None,
        }
    }

    /// A control string starts; only an OSC may be an answer.
    pub(super) fn begin(&mut self, osc: bool) {
        self.active = osc;
        self.body.clear();
    }

    pub(super) fn push(&mut self, b: u8) {
        if !self.active {
            return;
        }
        let prefix = b"52;";
        if self.body.len() < prefix.len() && prefix[self.body.len()] != b
            || self.body.len() >= MAX_BODY
        {
            self.active = false;
            self.body = Vec::new();
            return;
        }
        self.body.push(b);
    }

    /// The string ended: keep it if it was `52;<selection>;<base64>`.
    pub(super) fn finish(&mut self) {
        if std::mem::take(&mut self.active)
            && let Some(rest) = self.body.strip_prefix(b"52;")
            && let Some(at) = rest.iter().position(|&b| b == b';')
        {
            self.reply = Some(rest[at + 1..].to_vec());
        }
        self.body = Vec::new();
    }

    pub(super) const fn take(&mut self) -> Option<Vec<u8>> {
        self.reply.take()
    }
}

#[cfg(test)]
mod tests {
    use super::super::StdinParser;

    #[test]
    fn the_answer_is_kept_and_keys_around_it_still_arrive() {
        let mut parser = StdinParser::new();
        let events = parser.feed(b"a\x1b]52;c;aGk=\x1b\\b");
        assert_eq!(events.len(), 2);
        assert_eq!(parser.take_clipboard_reply().as_deref(), Some(&b"aGk="[..]));
        assert_eq!(parser.take_clipboard_reply(), None);
    }

    #[test]
    fn a_split_answer_with_bel_is_kept() {
        let mut parser = StdinParser::new();
        parser.feed(b"\x1b]5");
        parser.feed(b"2;p;eA");
        parser.feed(b"==\x07");
        assert_eq!(parser.take_clipboard_reply().as_deref(), Some(&b"eA=="[..]));
    }

    #[test]
    fn other_strings_are_dropped() {
        let mut parser = StdinParser::new();
        parser.feed(b"\x1b]11;rgb:0/0/0\x07\x1bP52;c;x\x1b\\\x1b]5;c;x\x07");
        assert_eq!(parser.take_clipboard_reply(), None);
    }
}
