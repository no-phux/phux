//! Paste protection, as Ghostty's default `clipboard-paste-bracketed-safe`:
//! a paste into a pane with bracketed paste on is delivered; one that would
//! arrive as typed input asks first when it carries a line break (each is an
//! Enter; a bare CR counts). A paste carrying `ESC [ 201 ~` always asks,
//! since it could close the bracket early. Unknown bracketed mode counts as
//! off.

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

    #[test]
    fn bracketed_panes_take_multiline_pastes_without_asking() {
        assert!(!needs_confirmation(b"one\ntwo\n", true));
        assert!(!needs_confirmation(b"single line", false));
    }

    #[test]
    fn unbracketed_line_breaks_ask_including_a_bare_cr() {
        assert!(needs_confirmation(b"rm -rf build\n", false));
        assert!(needs_confirmation(b"echo hi\r", false));
    }

    #[test]
    fn the_bracket_terminator_always_asks() {
        assert!(needs_confirmation(b"x\x1b[201~rm -rf ~\n", true));
        assert!(needs_confirmation(b"\x1b[201~", false));
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
