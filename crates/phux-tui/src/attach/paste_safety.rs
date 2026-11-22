//! Paste protection, as Ghostty's default `clipboard-paste-bracketed-safe`.
//!
//! A bracketed paste is delivered; one arriving as typed input asks first
//! when it carries a line break (each is an Enter). A paste carrying
//! `ESC [ 201 ~` always asks. Unknown bracketed mode counts as off.

const BRACKET_END: &[u8] = b"\x1b[201~";

/// Whether delivering `text` to a pane with bracketed paste `bracketed`
/// should ask first.
pub(super) fn needs_confirmation(text: &[u8], bracketed: bool) -> bool {
    if text.windows(BRACKET_END.len()).any(|w| w == BRACKET_END) {
        return true;
    }
    !bracketed && text.iter().any(|&b| b == b'\n' || b == b'\r')
}

/// The lines `text` would enter: a trailing break ends the last line rather
/// than opening an empty one, and `\r\n` is one break.
pub(super) fn line_count(text: &[u8]) -> usize {
    let mut count = 0;
    let mut open = false;
    let mut bytes = text.iter().peekable();
    while let Some(&b) = bytes.next() {
        if b == b'\n' || b == b'\r' {
            count += 1;
            open = false;
            if b == b'\r' && bytes.peek() == Some(&&b'\n') {
                bytes.next();
            }
        } else {
            open = true;
        }
    }
    count + usize::from(open)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Bracketed panes take multi-line pastes; unbracketed line breaks
    /// (including a bare CR) ask; the bracket terminator always asks.
    #[test]
    fn confirmation_follows_line_breaks_and_the_bracket_terminator() {
        #[rustfmt::skip] // a case table, one row per case
        let cases: [(&str, &[u8], bool, bool); 6] = [
            ("bracketed multi-line", b"one\ntwo\n", true, false),
            ("unbracketed single line", b"single line", false, false),
            ("unbracketed newline", b"rm -rf build\n", false, true),
            ("unbracketed bare cr", b"echo hi\r", false, true),
            ("embedded terminator, bracketed", b"x\x1b[201~rm -rf ~\n", true, true),
            ("bare terminator, unbracketed", b"\x1b[201~", false, true),
        ];
        for (case, bytes, bracketed, asks) in cases {
            assert_eq!(needs_confirmation(bytes, bracketed), asks, "{case}");
        }
    }

    #[test]
    fn line_count_reads_like_a_person_would() {
        assert_eq!(line_count(b""), 0);
        assert_eq!(line_count(b"one"), 1);
        assert_eq!(line_count(b"one\n"), 1);
        assert_eq!(line_count(b"one\r\ntwo"), 2);
        assert_eq!(line_count(b"one\n\nthree\n"), 3);
    }
}
