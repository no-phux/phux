//! What a list calls a pane: the one rule Rune, the TUI and the mobile
//! clients share (#2377).
//!
//! A user-set name wins. Otherwise the pane is named after what runs in it
//! and where, `claude · phux` or `zsh · ~`, from the server's
//! `phux.pane-occupant/v1` foreground and the pane's cwd. Without a known
//! foreground (an older server, a satellite pane) the program's title
//! stands, then the folder alone. Bash's `user@host:path` prompt is not a
//! title: on Arch, and so on Omarchy, it is written into every pane and
//! clips to the user and host, hiding the path.

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
    let title = nonempty(title).filter(|title| parse_ask_title(title).is_none());
    if let Some(title) = title {
        // A shell prompt names a place, not a program. Prefer the server's
        // cwd; the path in the prompt is what an older server still has.
        if let Some(place) = shell_prompt_place(title) {
            return Some(folder.map_or_else(|| place.to_owned(), str::to_owned));
        }
        return Some(title.to_owned());
    }
    folder.map(str::to_owned)
}

/// What a one-line chrome label shows.
///
/// A program's own title stands, including an ask. A shell prompt does
/// not: the pane is named the way a list names it (`bash · phux`, or the
/// folder when the server has not said what is running).
#[must_use]
pub fn rail_label(
    title: Option<&str>,
    foreground: Option<&str>,
    cwd: Option<&str>,
) -> Option<String> {
    let title = title.map(str::trim).filter(|title| !title.is_empty());
    if let Some(title) = title
        && shell_prompt_place(title).is_none()
    {
        return Some(title.to_owned());
    }
    pane_label(None, title, foreground, cwd)
}

/// The folder a shell prompt title names, as a subslice of `title`.
///
/// Bash writes `user@host:path` (`path` is `~`, `~/…`, or absolute; a
/// space after the colon is tolerated). `None` when `title` is not that
/// shape, so a program's own title is left alone.
#[must_use]
pub fn shell_prompt_place(title: &str) -> Option<&str> {
    let title = title.trim();
    let (user_host, path) = title.split_once(':')?;
    let (user, host) = user_host.split_once('@')?;
    if user.is_empty()
        || host.is_empty()
        || user.chars().any(char::is_whitespace)
        || host.chars().any(|c| c.is_whitespace() || c == '@')
    {
        return None;
    }
    path_place(path.trim())
}

/// The last component of a prompt path, or `None` when `path` is not a
/// location (`~`, `~/…`, or absolute).
fn path_place(path: &str) -> Option<&str> {
    if !(path == "~" || path.starts_with("~/") || path.starts_with('/')) {
        return None;
    }
    let trimmed = path.trim_end_matches('/');
    if trimmed.is_empty() {
        return Some(&path[..1]);
    }
    if trimmed == "~" {
        return Some(trimmed);
    }
    Some(
        trimmed
            .rsplit('/')
            .next()
            .filter(|part| !part.is_empty())
            .unwrap_or(trimmed),
    )
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
    use super::{pane_label, rail_label};

    #[test]
    fn panes_are_named_after_what_runs_and_where() {
        let label = |fg| pane_label(None, Some("admin@mini: ~"), fg, Some("/Users/me/src/phux"));
        assert_eq!(label(Some("zsh")).as_deref(), Some("zsh · phux"));
        assert_eq!(label(Some("claude")).as_deref(), Some("claude · phux"));
        assert_eq!(
            label(Some("npm run dev")).as_deref(),
            Some("npm run dev · phux")
        );
        // No foreground: a prompt is not a name, so the folder stands.
        assert_eq!(label(None).as_deref(), Some("phux"));
    }

    #[test]
    fn a_shell_prompt_is_a_place_not_a_name() {
        // Arch, and so Omarchy, writes this into every pane.
        assert_eq!(
            pane_label(
                None,
                Some("drspeedracer@powerhouse:~/Workspace/no-phux"),
                None,
                Some("/home/drspeedracer/Workspace/no-phux"),
            )
            .as_deref(),
            Some("no-phux")
        );
        assert_eq!(
            pane_label(None, Some("drspeedracer@powerhouse:~"), None, None).as_deref(),
            Some("~")
        );
        assert_eq!(
            pane_label(None, Some("me@host:/tmp/build"), None, None).as_deref(),
            Some("build")
        );
        assert_eq!(
            pane_label(None, Some("me@host:/"), None, None).as_deref(),
            Some("/")
        );
        // A program's title stands, even beside a cwd.
        assert_eq!(
            pane_label(
                None,
                Some("Polish omarchy TUI"),
                None,
                Some("/home/me/Workspace/no-phux"),
            )
            .as_deref(),
            Some("Polish omarchy TUI")
        );
        // An address with a sentence after the colon is not a prompt.
        assert_eq!(
            pane_label(None, Some("review@alice: please look"), None, Some("/tmp")).as_deref(),
            Some("review@alice: please look")
        );
    }

    #[test]
    fn the_rail_keeps_a_program_title_and_drops_a_prompt() {
        assert_eq!(
            rail_label(
                Some("Polish omarchy TUI"),
                Some("opencode"),
                Some("/home/me/no-phux"),
            )
            .as_deref(),
            Some("Polish omarchy TUI")
        );
        assert_eq!(
            rail_label(
                Some("me@host:~/Workspace/no-phux"),
                Some("bash"),
                Some("/home/me/Workspace/no-phux"),
            )
            .as_deref(),
            Some("bash · no-phux")
        );
        assert_eq!(
            rail_label(Some("me@host:/tmp/build"), None, None).as_deref(),
            Some("build")
        );
        // An ask borrows the title; the rail still shows it.
        assert_eq!(
            rail_label(Some("phux-ask:what next?"), None, Some("/tmp")).as_deref(),
            Some("phux-ask:what next?")
        );
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
