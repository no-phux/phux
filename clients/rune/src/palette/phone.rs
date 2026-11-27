//! The palette's list on a phone. There are no chords to find a command by
//! and no room to scan sixty rows, so what a phone reaches for comes first
//! and what needs a desktop is left out.

use phux_client_runtime::control::Topology;
use phux_protocol::ids::SessionKey;

use super::{Item, Target, session_items, terminal_items, theme_items};
use crate::keys::{COMMANDS, Command, Shortcut, command};

/// The first rows, in the order a phone reaches for them.
const FIRST: [Shortcut; 12] = [
    Shortcut::NewTerminal,
    Shortcut::Paste,
    Shortcut::Find,
    Shortcut::SwitchSession,
    Shortcut::SplitRight,
    Shortcut::SplitDown,
    Shortcut::ZoomIn,
    Shortcut::ZoomOut,
    Shortcut::Settings,
    Shortcut::Machines,
    Shortcut::CloseTab,
    Shortcut::ScrollToBottom,
];

/// What needs a desktop's window, sidebar, updater or keyboard.
const DESKTOP: [Shortcut; 11] = [
    Shortcut::Popup,
    Shortcut::ToggleSidebar,
    Shortcut::ToggleFullScreen,
    Shortcut::KeyboardShortcuts,
    Shortcut::EssentialShortcuts,
    Shortcut::GetStarted,
    Shortcut::CheckForUpdates,
    Shortcut::InstallUpdate,
    Shortcut::InsertPath,
    Shortcut::Screenshot,
    Shortcut::PairPhone,
];

/// A command's row with no chord beside it. A native phone pairs with one
/// computer at a time, so its machines command says what it does there.
fn action(command: &Command, native: bool, kind: &'static str) -> Item {
    let title = if native && command.action == Shortcut::Machines {
        "Change computer".to_owned()
    } else {
        command.label()
    };
    Item {
        title,
        detail: String::new(),
        kind,
        target: Target::Action(command.action),
    }
}

/// How many of [`FIRST`] lead the list as suggested: few enough that the
/// terminals and sessions still show above the fold on a small phone.
const SUGGESTED: usize = 4;

/// A phone's empty-query order: its first few commands (suggested),
/// terminals, sessions, the rest of its first commands and then the
/// remaining ones, `extra` (the desk's own rows), then themes.
pub fn items(
    topology: Option<&Topology>,
    order: &[SessionKey],
    native: bool,
    extra: Vec<Item>,
) -> Vec<Item> {
    let first = || FIRST.iter().filter_map(|shortcut| command(*shortcut));
    let mut out: Vec<Item> = first()
        .take(SUGGESTED)
        .map(|command| action(command, native, super::sections::SUGGESTED_KIND))
        .collect();
    out.extend(terminal_items(topology));
    out.extend(session_items(topology, order));
    out.extend(
        first()
            .skip(SUGGESTED)
            .map(|command| action(command, native, "action")),
    );
    let rest = COMMANDS.iter().filter(|command| {
        command.listed && !FIRST.contains(&command.action) && !DESKTOP.contains(&command.action)
    });
    out.extend(rest.map(|command| action(command, native, "action")));
    out.extend(extra);
    out.extend(theme_items());
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_phone_lists_its_commands_first_without_chords_or_desktop_rows() {
        let list = items(None, &[], true, Vec::new());
        let titles: Vec<&str> = list.iter().map(|item| item.title.as_str()).collect();
        assert_eq!(&titles[..3], ["New tab", "Paste", "Find in terminal"]);
        // A short Suggested, so terminals and sessions follow it on screen.
        let suggested =
            (list.iter()).filter(|item| item.kind == super::super::sections::SUGGESTED_KIND);
        assert_eq!(suggested.count(), SUGGESTED);
        assert!(
            list[..SUGGESTED]
                .iter()
                .all(|item| item.kind == "suggested")
        );
        assert!(titles.contains(&"Change computer"));
        assert!(!titles.contains(&"Switch or Add Machine"));
        for desktop in ["Toggle Sidebar", "Keyboard Shortcuts", "Check for Updates"] {
            assert!(!titles.contains(&desktop), "{desktop}");
        }
        let actions = (list.iter()).filter(|item| matches!(item.kind, "action" | "suggested"));
        assert!(actions.clone().all(|item| item.detail.is_empty()));
        let mut seen = std::collections::HashSet::new();
        assert!(actions.into_iter().all(|item| seen.insert(&item.title)));
        // The desktop-hosted preview keeps the shared name.
        let preview = items(None, &[], false, Vec::new());
        assert!(
            preview
                .iter()
                .any(|item| item.title == "Switch or add machine")
        );
    }
}
