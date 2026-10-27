//! The marked block `phux omarchy setup` owns inside a file it shares with
//! the user, such as `~/.config/hypr/bindings.lua`. Everything outside the
//! markers is left byte for byte, so setup can run again and remove can
//! take back exactly what setup added.

const BEGIN: &str = ">>> phux: managed by `phux omarchy setup`; `phux omarchy remove` deletes it";
const END: &str = "<<< phux";

/// `text` with the block holding `body` in place of any earlier one, or
/// appended when there is none. `comment` starts a line comment in the
/// file's language (`--` for Lua, `#` for hyprlang).
pub(super) fn upsert(text: &str, comment: &str, body: &str) -> String {
    let block = format!("{comment} {BEGIN}\n{}\n{comment} {END}\n", body.trim_end());
    if let Some((start, end)) = span(text, comment) {
        return format!("{}{block}{}", &text[..start], &text[end..]);
    }
    let mut out = text.to_owned();
    if !out.is_empty() && !out.ends_with('\n') {
        out.push('\n');
    }
    if !out.is_empty() {
        out.push('\n');
    }
    out.push_str(&block);
    out
}

/// `text` without the block, or `None` when it has none. The blank line
/// [`upsert`] put before the block goes with it.
pub(super) fn strip(text: &str, comment: &str) -> Option<String> {
    let (mut start, end) = span(text, comment)?;
    if text[..start].ends_with("\n\n") {
        start -= 1;
    }
    Some(format!("{}{}", &text[..start], &text[end..]))
}

/// Whether `text` holds the block.
pub(super) fn contains(text: &str, comment: &str) -> bool {
    span(text, comment).is_some()
}

/// Byte range of the block's lines, end marker and its newline included.
fn span(text: &str, comment: &str) -> Option<(usize, usize)> {
    let begin = format!("{comment} {BEGIN}");
    let end = format!("{comment} {END}");
    let mut offset = 0;
    let mut start = None;
    for line in text.split_inclusive('\n') {
        let trimmed = line.trim_end_matches(['\n', '\r']);
        match start {
            None if trimmed == begin => start = Some(offset),
            Some(start) if trimmed == end => return Some((start, offset + line.len())),
            _ => {}
        }
        offset += line.len();
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    const USER: &str = "-- my binds\no.bind(\"SUPER + X\", \"X\", \"x\")\n";

    #[test]
    fn upsert_appends_once_and_replaces_in_place() {
        let once = upsert(USER, "--", "o.bind(1)");
        assert!(once.starts_with(USER));
        assert!(contains(&once, "--"));
        let twice = upsert(&once, "--", "o.bind(1)");
        assert_eq!(once, twice, "running setup again changes nothing");
        let changed = upsert(&once, "--", "o.bind(2)");
        assert!(changed.contains("o.bind(2)") && !changed.contains("o.bind(1)"));
        assert_eq!(changed.matches(BEGIN).count(), 1);
    }

    #[test]
    fn strip_restores_the_user_text_exactly() {
        for original in [USER, "", "no trailing newline"] {
            let with = upsert(original, "#", "exec-once = phux");
            let without = strip(&with, "#").expect("block present");
            let expected = if original.is_empty() || original.ends_with('\n') {
                original.to_owned()
            } else {
                format!("{original}\n")
            };
            assert_eq!(without, expected);
        }
        assert_eq!(strip(USER, "--"), None);
    }

    #[test]
    fn text_after_the_block_survives() {
        let with = upsert(USER, "--", "o.bind(1)");
        let edited = format!("{with}-- added later\n");
        let without = strip(&edited, "--").expect("block present");
        assert_eq!(without, format!("{USER}-- added later\n"));
    }

    #[test]
    fn an_unterminated_block_is_not_touched() {
        let text = format!("-- {BEGIN}\no.bind(1)\n");
        assert!(!contains(&text, "--"));
        assert_eq!(strip(&text, "--"), None);
    }
}
