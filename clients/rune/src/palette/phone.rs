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
    Shortcut::ScrollBottom,
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
fn action(command: &Command, native: bool) -> Item {
    let title = if native && command.action == Shortcut::Machines {
        "Change Computer"
    } else {
        command.title
    };
    Item {
        title: title.to_owned(),
        detail: String::new(),
        kind: "action",
        target: Target::Action(command.action),
    }
}

/// A phone's empty-query order: its first commands, terminals, sessions,
/// `extra` (the desk's own rows), the remaining commands, then themes.
pub fn items(
    topology: Option<&Topology>,
    order: &[SessionKey],
    native: bool,
    extra: Vec<Item>,
) -> Vec<Item> {
    let mut out: Vec<Item> = FIRST
        .iter()
        .filter_map(|shortcut| command(*shortcut))
        .map(|command| action(command, native))
        .collect();
    out.extend(terminal_items(topology));
    out.extend(session_items(topology, order));
    out.extend(extra);
    let rest = COMMANDS.iter().filter(|command| {
        command.listed && !FIRST.contains(&command.action) && !DESKTOP.contains(&command.action)
    });
    out.extend(rest.map(|command| action(command, native)));
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
        assert_eq!(&titles[..3], ["New Tab", "Paste", "Find in Terminal"]);
        assert!(titles.contains(&"Change Computer"));
        assert!(!titles.contains(&"Switch or Add Machine"));
        for desktop in ["Toggle Sidebar", "Keyboard Shortcuts", "Check for Updates"] {
            assert!(!titles.contains(&desktop), "{desktop}");
        }
        let actions = list.iter().filter(|item| item.kind == "action");
        assert!(actions.clone().all(|item| item.detail.is_empty()));
        let mut seen = std::collections::HashSet::new();
        assert!(actions.into_iter().all(|item| seen.insert(&item.title)));
        // The desktop-hosted preview keeps the shared name.
        let preview = items(None, &[], false, Vec::new());
        assert!(
            preview
                .iter()
                .any(|item| item.title == "Switch or Add Machine")
        );
    }
}
