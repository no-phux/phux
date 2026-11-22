//! Words a palette row answers to besides its title: what people type when
//! they reach for it ("quit" for Detach, "close" for Kill pane, "fullscreen"
//! for Zoom). They rank below a title match and never light the title.

/// The extra words for the action `name`, space-separated ("" for none).
pub(super) fn aliases(name: &str) -> &'static str {
    match name {
        "detach" => "quit exit leave disconnect",
        "kill-pane" | "kill-window" => "close remove delete end",
        "close-view" => "hide minimize",
        "reopen-views" => "unhide restore show",
        "split-pane" => "divide vertical horizontal new pane",
        "new-window" => "create add open",
        "new-session" => "create add project workspace",
        "new-worktree" => "branch git checkout",
        "go-to-directory" => "folder cd open directory",
        "toggle-zoom" => "maximize fullscreen focus expand",
        "toggle-sidebar" => "show hide panel",
        "focus-sidebar" => "jump agents list",
        "session-picker" => "switch jump open sessions",
        "window-picker" => "switch jump open tabs",
        "next-attention" => "waiting blocked question agent needs you",
        "return-from-attention" => "back return",
        "agent-fleet" => "dashboard overview agents fleet",
        "settings" => "preferences options config configure",
        "reload-config" => "refresh reread",
        "rename-window" | "rename-session" => "title name",
        "display-panes" => "numbers identify which",
        "url-picker" => "url open browser link",
        "find-path" => "file search insert",
        "popup" => "float floating scratch",
        "sync-panes" => "broadcast synchronize all",
        "signal-terminal" => "pause stop suspend",
        "set-pane" => "mouse capture",
        "take-input" => "control drive input",
        "give-input" => "release control",
        "report-bug" => "feedback issue",
        "getting-started" => "help tutorial guide keys",
        "restore-sessions" => "resume reopen",
        _ => "",
    }
}

/// `chord` in key-cap form: `C-a %` reads `⌃A %`, `M-x` reads `⌥X`, `S-Tab`
/// reads `⇧Tab`. Single letters after a modifier are capitalized, as on a
/// keyboard; bare keys stay as typed (`%`, `x`, `Enter`).
pub(super) fn keycaps(chord: &str) -> String {
    chord.split(' ').map(keycap).collect::<Vec<_>>().join(" ")
}

fn keycap(stroke: &str) -> String {
    let mut out = String::new();
    let mut rest = stroke;
    loop {
        let glyph = match rest.get(..2) {
            Some("C-") => "⌃",
            Some("M-") => "⌥",
            Some("S-") => "⇧",
            Some("D-") => "⌘",
            _ => break,
        };
        if rest.len() == 2 {
            break;
        }
        out.push_str(glyph);
        rest = &rest[2..];
    }
    if out.is_empty() {
        return stroke.to_owned();
    }
    let mut chars = rest.chars();
    match (chars.next(), chars.next()) {
        (Some(letter), None) => out.extend(letter.to_uppercase()),
        _ => out.push_str(rest),
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chords_read_as_keycaps() {
        assert_eq!(keycaps("C-a %"), "⌃A %");
        assert_eq!(keycaps("C-a \""), "⌃A \"");
        assert_eq!(keycaps("M-x"), "⌥X");
        assert_eq!(keycaps("C-M-Left"), "⌃⌥Left");
        assert_eq!(keycaps("S-Tab"), "⇧Tab");
        assert_eq!(keycaps("C-a x"), "⌃A x");
        assert_eq!(keycaps("C-"), "C-", "a bare modifier stays as written");
        assert_eq!(keycaps("Enter"), "Enter");
    }

    #[test]
    fn every_alias_names_a_real_action() {
        for spec in super::super::ACTIONS {
            let _ = aliases(spec.name);
        }
        let known: std::collections::BTreeSet<&str> =
            super::super::ACTIONS.iter().map(|spec| spec.name).collect();
        for name in [
            "detach",
            "kill-pane",
            "kill-window",
            "close-view",
            "reopen-views",
            "split-pane",
            "new-window",
            "new-session",
            "new-worktree",
            "go-to-directory",
            "toggle-zoom",
            "toggle-sidebar",
            "focus-sidebar",
            "session-picker",
            "window-picker",
            "next-attention",
            "return-from-attention",
            "agent-fleet",
            "settings",
            "reload-config",
            "rename-window",
            "rename-session",
            "display-panes",
            "url-picker",
            "find-path",
            "popup",
            "sync-panes",
            "signal-terminal",
            "set-pane",
            "take-input",
            "give-input",
            "report-bug",
            "getting-started",
            "restore-sessions",
        ] {
            assert!(known.contains(name), "{name} is not an action");
            assert!(!aliases(name).is_empty(), "{name} has aliases");
        }
    }
}
