//! What a list calls a pane: the one rule Rune, the TUI and the mobile
//! clients share (#2377).
//!
//! A user-set name wins. Otherwise the pane is named after what runs in it
//! and where, `claude · phux` or `zsh · ~`, from the server's
//! `phux.pane-occupant/v1` foreground and the pane's cwd. Without a known
//! foreground (an older server, a satellite pane) the program's title
//! stands, then the folder alone.

use crate::ask::parse_ask_title;

/// The pane's label, or `None` when nothing names it.
#[must_use]
pub fn pane_label<'a>(
    name: Option<&'a str>,
    title: Option<&'a str>,
    foreground: Option<&'a str>,
    cwd: Option<&'a str>,
) -> Option<String> {
    let nonempty = |s: Option<&'a str>| s.map(str::trim).filter(|s| !s.is_empty());
    if let Some(name) = nonempty(name) {
        return Some(name.to_owned());
    }
    let folder = nonempty(cwd).map(folder);
    if let Some(program) = nonempty(foreground) {
        return Some(folder.map_or_else(
            || program.to_owned(),
            |folder| format!("{program} · {folder}"),
        ));
    }
    nonempty(title)
        // An ask borrows the title to reach the server; it isn't a name.
        .filter(|title| parse_ask_title(title).is_none())
        .map(str::to_owned)
        .or_else(|| folder.map(str::to_owned))
}

/// The cwd as a person names it: its last component, `~` for a user's home
/// on the pane's host (`/Users/<u>`, `/home/<u>`, `/root`).
fn folder(cwd: &str) -> &str {
    let trimmed = cwd.trim_end_matches('/');
    let parts: Vec<&str> = trimmed.split('/').filter(|p| !p.is_empty()).collect();
    match parts.as_slice() {
        [] => "/",
        ["root"] | ["Users" | "home", _] => "~",
        [.., last] => last,
    }
}

#[cfg(test)]
mod tests {
    use super::pane_label;

    #[test]
    fn panes_are_named_after_what_runs_and_where() {
        let label = |fg| pane_label(None, Some("admin@mini: ~"), fg, Some("/Users/me/src/phux"));
        assert_eq!(label(Some("zsh")).as_deref(), Some("zsh · phux"));
        assert_eq!(label(Some("claude")).as_deref(), Some("claude · phux"));
        assert_eq!(
            label(Some("npm run dev")).as_deref(),
            Some("npm run dev · phux")
        );
        // No foreground (older server, satellite): the title stands.
        assert_eq!(label(None).as_deref(), Some("admin@mini: ~"));
    }

    #[test]
    fn a_user_name_wins_and_the_rest_falls_back() {
        assert_eq!(
            pane_label(Some("api"), None, Some("zsh"), Some("/tmp")).as_deref(),
            Some("api")
        );
        assert_eq!(
            pane_label(Some(" "), None, Some("zsh"), Some("/Users/me")).as_deref(),
            Some("zsh · ~")
        );
        assert_eq!(
            pane_label(None, None, Some("vim"), None).as_deref(),
            Some("vim")
        );
        assert_eq!(
            pane_label(None, None, None, Some("/root/")).as_deref(),
            Some("~")
        );
        assert_eq!(
            pane_label(None, None, None, Some("/")).as_deref(),
            Some("/")
        );
        assert_eq!(pane_label(None, Some(""), None, None), None);
    }
}
