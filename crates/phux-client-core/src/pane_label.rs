//! What a list calls a pane: the one rule Rune, the TUI and the mobile
//! clients share (#2377).
//!
//! A user-set name wins. Then the work a program says it is doing: an agent
//! titles its terminal after its task (Claude Code's `✳ Fix the login
//! bug`, opencode's `OC | Fix the login bug`), and that task is what a
//! person knows it by. A shell's title, or a title that only repeats the
//! program or its folder, says nothing new, so otherwise the pane is named
//! after what runs in it and where, `claude · phux` or `zsh · ~`, from the
//! server's `phux.pane-occupant/v1` foreground and the pane's cwd. Without
//! a known foreground (an older server, a satellite pane) the program's
//! title stands, then the folder alone.

use crate::ask::parse_ask_title;

/// Programs whose title is the prompt's, not a task's: a shell titles its
/// terminal `user@host: ~` or after the command it last ran.
const SHELLS: [&str; 9] = [
    "zsh", "bash", "fish", "sh", "dash", "ksh", "nu", "pwsh", "login",
];

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
    if let Some(task) = nonempty(title).and_then(|title| task_title(title, foreground, cwd)) {
        return Some(task);
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

/// `title` as the work a program names, or `None` when it names none.
///
/// None is a shell's prompt title, an ask, a path, `user@host`, or the
/// program's or folder's own name (`Claude Code` while idle, `opencode`).
/// A spinner or badge in front (`✳ `, `⠂ `) and a short tag (`OC | `) are
/// dropped.
#[must_use]
pub fn task_title(title: &str, foreground: Option<&str>, cwd: Option<&str>) -> Option<String> {
    let program = foreground
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .map(program_name);
    let path = title.trim_start().starts_with(['~', '/', '.']);
    if program.is_some_and(|p| SHELLS.contains(&p)) || parse_ask_title(title).is_some() || path {
        return None;
    }
    let task = untagged(
        title
            .trim_start_matches(|c: char| !c.is_alphanumeric())
            .trim(),
    );
    let user_at_host = task.contains('@') && task.contains(':');
    if task.chars().count() < 2 || user_at_host {
        return None;
    }
    let squashed = squash(task);
    let repeats = |name: &str| {
        let name = squash(name);
        !name.is_empty() && squashed.starts_with(&name) && squashed.len() <= name.len() + 5
    };
    let place = cwd.map(str::trim).filter(|c| !c.is_empty()).map(folder);
    if program.is_some_and(repeats) || place.is_some_and(repeats) {
        return None;
    }
    Some(task.to_owned())
}

/// A foreground's program: the command's first word, without its path or a
/// login shell's `-`.
fn program_name(foreground: &str) -> &str {
    let word = foreground.split_whitespace().next().unwrap_or(foreground);
    word.rsplit('/')
        .next()
        .unwrap_or(word)
        .trim_start_matches('-')
}

/// `title` without a short leading tag, as opencode's `OC | `.
fn untagged(title: &str) -> &str {
    match title.split_once(" | ") {
        Some((tag, rest)) if tag.chars().count() <= 4 && !rest.trim().is_empty() => rest.trim(),
        _ => title,
    }
}

/// Letters and digits only, lowercased: `Claude Code` and `claude-code`
/// compare equal.
fn squash(words: &str) -> String {
    words
        .chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

/// The cwd as a person names it: its last component, `~` for a user's home
/// on the pane's host (`/Users/<u>`, `/home/<u>`, `/root`).
#[must_use]
pub fn folder(cwd: &str) -> &str {
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
    use super::{pane_label, task_title};

    #[test]
    fn an_agent_is_named_after_its_task() {
        let cwd = Some("/Users/me/src/phux");
        let label = |title, fg| pane_label(None, Some(title), fg, cwd);
        assert_eq!(
            label("✳ Phux UI polish vs Rex", Some("claude")).as_deref(),
            Some("Phux UI polish vs Rex")
        );
        assert_eq!(
            label("⠂ Fix the login bug", Some("claude")).as_deref(),
            Some("Fix the login bug")
        );
        assert_eq!(
            label("OC | Reduce unnecessary code and docs", Some("opencode")).as_deref(),
            Some("Reduce unnecessary code and docs")
        );
        // A user's name still wins over the task.
        assert_eq!(
            pane_label(Some("api"), Some("✳ Fix it"), Some("claude"), cwd).as_deref(),
            Some("api")
        );
    }

    #[test]
    fn a_title_that_names_nothing_new_keeps_program_and_folder() {
        let cwd = Some("/Users/me/src/phux");
        let label = |title, fg| pane_label(None, Some(title), fg, cwd);
        // Idle agents title themselves after the program.
        assert_eq!(
            label("✳ Claude Code", Some("claude")).as_deref(),
            Some("claude · phux")
        );
        assert_eq!(
            label("opencode", Some("opencode")).as_deref(),
            Some("opencode · phux")
        );
        // A shell's title is its prompt's, whatever it says.
        assert_eq!(
            label("admin@mini: ~", Some("zsh")).as_deref(),
            Some("zsh · phux")
        );
        assert_eq!(
            label("cargo test", Some("-zsh")).as_deref(),
            Some("-zsh · phux")
        );
        assert_eq!(
            label("~/src/phux", Some("vim")).as_deref(),
            Some("vim · phux")
        );
        assert_eq!(label("phux", Some("node")).as_deref(), Some("node · phux"));
        assert_eq!(task_title("phux-ask[a]:Deploy?", Some("claude"), cwd), None);
        assert_eq!(task_title("✳", Some("claude"), cwd), None);
    }

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
