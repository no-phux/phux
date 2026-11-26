//! The text of a kept file: reduced to what may be shown, and composed
//! into what a new engine starts with.

/// `-- before the restart, 2026-11-25 14:02 --`, in local time.
pub(super) fn divider() -> String {
    format!(
        "{DIVIDER}{} --",
        jiff::Zoned::now().strftime("%Y-%m-%d %H:%M")
    )
}

const DIVIDER: &str = "-- before the restart, ";

/// `text`'s last `lines` lines and `divider`, scrolled into history, with
/// the cursor home on a blank screen. Empty for no text.
pub(super) fn compose(text: &str, lines: u32, rows: u16, divider: &str) -> Vec<u8> {
    let text = clean(text, lines);
    // A run that printed nothing adds no second divider under the first.
    let text = match text.rsplit_once('\n') {
        Some((above, last)) if last.contains(DIVIDER) => above,
        _ => text.as_str(),
    };
    if text.is_empty() {
        return Vec::new();
    }
    let mut seed = text.replace('\n', "\r\n");
    seed.push_str("\x1b[0m\r\n\x1b[2m");
    seed.push_str(divider);
    seed.push_str("\x1b[0m");
    seed.push_str(&"\r\n".repeat(usize::from(rows)));
    seed.push_str("\x1b[H");
    seed.into_bytes()
}

/// The last `lines` lines of `text`, keeping printable characters, newlines
/// and SGR (`CSI` digits `m`). Every other control character and escape
/// sequence is dropped whole; an unfinished one ends with its line.
pub(super) fn clean(text: &str, lines: u32) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text.chars().peekable();
    while let Some(c) = rest.next() {
        match c {
            '\x1b' => match rest.next_if(|c| *c != '\n' && *c != '\x1b') {
                Some('[') => {
                    let mut params = String::new();
                    while let Some(c) = rest.next_if(|c| matches!(c, ' '..='?')) {
                        params.push(c);
                    }
                    let last = rest.next_if(|c| matches!(c, '@'..='~'));
                    let plain = |c: char| c.is_ascii_digit() || matches!(c, ';' | ':');
                    if last == Some('m') && params.len() <= 64 && params.chars().all(plain) {
                        out.push_str("\x1b[");
                        out.push_str(&params);
                        out.push('m');
                    }
                }
                // A string (OSC, DCS, SOS, PM, APC) runs to BEL or ST; the
                // `\` of ST falls to the two-character escapes below.
                Some(']' | 'P' | 'X' | '^' | '_') => {
                    while rest
                        .next_if(|c| !matches!(c, '\x07' | '\x1b' | '\n' | '\u{9c}'))
                        .is_some()
                    {}
                }
                Some(' '..='/') => {
                    while rest.next_if(|c| matches!(c, ' '..='/')).is_some() {}
                    let _ = rest.next_if(|c| *c != '\n' && *c != '\x1b');
                }
                _ => {}
            },
            '\n' => out.push('\n'),
            c if c.is_control() => {}
            c => out.push(c),
        }
    }
    let out = out.trim_end_matches('\n');
    let from = out
        .rmatch_indices('\n')
        .nth((lines as usize).saturating_sub(1))
        .map_or(0, |(at, _)| at + 1);
    out[from..].to_owned()
}

#[cfg(test)]
mod tests {
    use tokio::sync::oneshot;

    use super::*;

    #[test]
    fn only_sgr_and_text_survive() {
        for (hostile, want) in [
            (
                "\x1b[1;38:2::1:2:3mred\x1b[m",
                "\x1b[1;38:2::1:2:3mred\x1b[m",
            ),
            ("a\x1b]0;title\x07b\x1b]52;c;aGk=\x1b\\c", "abc"),
            ("a\x1b]7501;state=working:app=x\x1b\\b", "ab"),
            ("a\x1bP$q\"p\x1b\\b\x1b_Gq=1\x1b\\c\x1b^x\x1b\\d", "abcd"),
            ("a\x1b[2J\x1b[Hb\x1b[?1049hc\x1b[>4;1md\x1b[6n", "abcd"),
            ("a\x1b[1 mb\x1b[=mc", "abc"),
            ("a\x07\x08\r\tb\x7f\u{9b}c\0", "abc"),
            ("a\x1b(0b\x1bcc\x1b#8d\x1b%Ge", "abcde"),
            ("a\x1b]0;never closed\nb\x1b[3", "a\nb"),
            ("a\x1b\x1b[31mb\x1b", "a\x1b[31mb"),
        ] {
            assert_eq!(clean(hostile, 100), want, "{hostile:?}");
        }
        let long = format!("\x1b[{}m", "1;".repeat(100));
        assert_eq!(clean(&long, 100), "");
        assert_eq!(clean("1\n2\n3\n\n\n", 2), "2\n3");
        assert_eq!(clean("1\n2", 5), "1\n2");
    }

    /// History holds the lines and the divider, the screen stays blank, and
    /// a taller client does not pull them back onto it.
    #[tokio::test]
    async fn restored_lines_are_history_above_a_divider() {
        let seed = compose("one\n\x1b[31mtwo", 10, 24, &format!("{DIVIDER}now --"));
        let again = compose(
            &format!("one\n\x1b[2m{DIVIDER}then --\x1b[0m"),
            10,
            24,
            "-- d --",
        );
        assert!(!String::from_utf8(again).expect("UTF-8").contains("then"));
        assert!(compose("\x1b]0;x\x07", 10, 24, "-- d --").is_empty());

        let bundle = phux_terminal::TerminalActor::new_with_seed(80, 24, &seed).expect("actor");
        let terminal = phux_terminal_api::actor::TerminalResource::terminal(&bundle.handle)
            .expect("terminal")
            .clone();
        tokio::task::LocalSet::new()
            .run_until(async move {
                tokio::task::spawn_local(bundle.actor.run());
                terminal
                    .resize
                    .send(phux_terminal_api::actor::ResizeRequest {
                        cols: 120,
                        rows: 60,
                        cell_px: None,
                        resync_clients: false,
                        resync_only: false,
                        resync_for: None,
                    })
                    .await
                    .expect("resize");
                let (reply, screen) = oneshot::channel();
                terminal
                    .screen
                    .send(phux_terminal_api::actor::ScreenRequest {
                        pane: 1,
                        scrollback: Some(0),
                        cells: false,
                        format: 0,
                        reply,
                    })
                    .await
                    .expect("request");
                let Ok(phux_terminal_api::actor::ScreenReply::Projection(screen)) = screen.await
                else {
                    panic!("no screen");
                };
                assert_eq!(
                    screen.scrollback,
                    ["one", "two", "-- before the restart, now --"]
                );
                assert_eq!(screen.rows, 60);
                assert!(screen.lines.iter().all(String::is_empty), "{screen:?}");
                bundle.token.cancel();
            })
            .await;
    }
}
